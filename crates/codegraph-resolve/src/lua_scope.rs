//! Lua calls through `local` aliases, Lua's own globals, and chunk-local
//! declarations (upstream #2143, #2188, #2202).
//!
//! Kong localizes everything it calls: `local splitn =
//! require("kong.tools.string").splitn`, `local check_phase =
//! phase_checker.check`, `local fmt = string.format`, `local type = type`.
//! A bare call through such an alias is followed to what the alias names: a
//! module's member (through what the module hands out under that name, or
//! re-exports from a module of its own, a few hops deep), a host table's
//! method, or nothing for the standard library. A `local` belongs to its
//! chunk, and a bare call reaches neither a table's method nor, for a Lua
//! global, a project namesake.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Lua's global functions, and the test runner's: `local type = type` is the
/// standard library's `type` (upstream `LUA_GLOBAL_FUNCTIONS`).
pub(crate) const LUA_GLOBAL_FUNCTIONS: [&str; 44] = [
    "assert",
    "error",
    "ipairs",
    "pairs",
    "next",
    "type",
    "tostring",
    "tonumber",
    "setmetatable",
    "getmetatable",
    "rawget",
    "rawset",
    "rawequal",
    "rawlen",
    "select",
    "pcall",
    "xpcall",
    "unpack",
    "print",
    "load",
    "loadstring",
    "loadfile",
    "dofile",
    "collectgarbage",
    "require",
    "setfenv",
    "getfenv",
    "newproxy",
    "typeof",
    "warn",
    "tick",
    "wait",
    "describe",
    "it",
    "before_each",
    "after_each",
    "setup",
    "teardown",
    "lazy_setup",
    "lazy_teardown",
    "pending",
    "finally",
    "insulate",
    "expose",
];

/// Lua's standard and host libraries (upstream `LUA_LIBRARY_TABLES`).
const LUA_LIBRARY_TABLES: [&str; 20] = [
    "string",
    "table",
    "math",
    "io",
    "os",
    "coroutine",
    "debug",
    "utf8",
    "package",
    "bit",
    "bit32",
    "jit",
    "ffi",
    "vim",
    "ngx",
    "assert",
    "spy",
    "stub",
    "mock",
    "love",
];

/// How deep an alias is followed through modules' exports (upstream's 3).
const ALIAS_DEPTH: usize = 3;

/// `\s` with JavaScript's whitespace, U+FEFF included.
const JS_SPACE: &str = r"[\s\x{FEFF}]";

/// What an alias names: no alias at all (`Plain`: the reference resolves
/// as before), nothing in the project (the standard library, an outside
/// module), or a project node.
#[derive(Debug, Clone)]
enum Alias {
    Plain,
    Nothing,
    Target(Arc<Node>),
}

/// Per-pass memo of module members: `(file, path, depth)` → what that
/// member of that module is. KEEP-RUST: keyed by depth too, so a member a
/// shallow lookup finds is never one a deep lookup gave up on, whichever
/// reference of a parallel pass asks first.
#[derive(Debug, Default)]
pub struct LuaTables {
    members: Mutex<HashMap<(String, String, usize), Alias>>,
    /// Each file's nodes in source order.
    nodes: Mutex<HashMap<String, Arc<Vec<Arc<Node>>>>>,
    /// What each call site's alias names.
    aliases: Mutex<AliasMemo>,
}

/// `(file, line, name)` of a bare call → [`lua_alias_target`]'s answer, the
/// target's node id standing for the resolved reference.
type AliasMemo = HashMap<(String, i64, String), Option<Option<String>>>;

/// `file`'s nodes in source order, read once per pass.
fn file_nodes(file: &str, context: &dyn ResolutionContext) -> Arc<Vec<Arc<Node>>> {
    let tables = context.lua_tables();
    if let Some(hit) = tables
        .nodes
        .lock()
        .ok()
        .and_then(|memo| memo.get(file).cloned())
    {
        return hit;
    }
    let nodes = Arc::new(in_order(context.get_nodes_in_file_shared(file)));
    if let Ok(mut memo) = tables.nodes.lock() {
        memo.insert(file.to_string(), Arc::clone(&nodes));
    }
    nodes
}

impl LuaTables {
    pub fn new() -> Self {
        Self::default()
    }
}

pub(crate) fn is_lua(language: Language) -> bool {
    matches!(language, Language::Lua | Language::Luau)
}

/// A Lua `calls` reference written as a bare name (`[A-Za-z_]\w*`).
pub(crate) fn is_lua_bare_call(reference: &RefView) -> bool {
    is_lua(reference.language)
        && reference.reference_kind == EdgeKind::Calls
        && is_lua_name(&reference.reference_name)
}

fn is_lua_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `require "m"` (or a loader named for it, kong's
/// `reload_module("spec.internal.misc")`), then any `.member`s (upstream
/// `LUA_REQUIRE_ALIAS`; the quote back-reference spelled out).
fn require_alias() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"^(?:require|[A-Za-z_][0-9A-Za-z_]*(?:[Rr]equire|_module|[Ii]mport)){s}*\(?{s}*(?:"([^"']+)"|'([^"']+)'){s}*\)?((?:{s}*\.{s}*[A-Za-z_][0-9A-Za-z_]*)*){s}*$"#,
            s = JS_SPACE
        ))
        .expect("lua require alias")
    })
}

/// `name.member…` (upstream `LUA_NAME_ALIAS`).
fn name_alias() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"^([A-Za-z_][0-9A-Za-z_]*)((?:{s}*\.{s}*[A-Za-z_][0-9A-Za-z_]*)*){s}*$",
            s = JS_SPACE
        ))
        .expect("lua name alias")
    })
}

/// The module and member path a `require` alias names.
fn require_parts(rhs: &str) -> Option<(String, Vec<String>)> {
    let caps = require_alias().captures(rhs)?;
    let module = caps.get(1).or_else(|| caps.get(2))?.as_str().to_string();
    Some((module, members(caps.get(3).map_or("", |m| m.as_str()))))
}

/// The `.`-separated members of a chain, trimmed (upstream `luaMembers`).
fn members(chain: &str) -> Vec<String> {
    chain
        .split('.')
        .map(|part| part.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}'))
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// A declaration's initializer without its `=`.
fn initializer(decl: &Node) -> Option<&str> {
    let signature = decl.signature.as_deref()?;
    let trimmed = signature.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    let rest = trimmed.strip_prefix('=').unwrap_or(trimmed);
    Some(rest.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}'))
}

/// `nodes` in source order (start line, column, id).
fn in_order(mut nodes: Vec<Arc<Node>>) -> Vec<Arc<Node>> {
    nodes.sort_by(|a, b| {
        (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
    });
    nodes
}

/// JavaScript's `\s`.
fn is_js_space(c: char) -> bool {
    codegraph_core::generated_header::is_js_whitespace(c)
}

/// The initializer of a `local name = …` line (upstream's
/// `^\s*local\s+name\s*=\s*(.+?)\s*$`, without compiling it per name).
fn local_line_initializer<'t>(text: &'t str, name: &str) -> Option<&'t str> {
    let rest = text.trim_start_matches(is_js_space).strip_prefix("local")?;
    if !rest.starts_with(is_js_space) {
        return None;
    }
    let rest = rest.trim_start_matches(is_js_space).strip_prefix(name)?;
    let rest = rest.trim_start_matches(is_js_space).strip_prefix('=')?;
    let rhs = rest.trim_matches(is_js_space);
    (!rhs.is_empty()).then_some(rhs)
}

/// Whether a declaration line declares `local` (as
/// `name_matcher::is_lua_local` reads it).
pub(crate) fn is_local_line(text: &str) -> bool {
    text.trim_start()
        .strip_prefix("local")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
}

/// [`lua_local_decl`] over a file's nodes in source order, its lines
/// (1-based) and its locality test.
fn local_decl_in(
    name: &str,
    line: i64,
    nodes: &[Arc<Node>],
    line_text: &dyn Fn(i64) -> Option<String>,
    is_local: &dyn Fn(&Node) -> bool,
) -> Option<Arc<Node>> {
    let functions: Vec<&Arc<Node>> = nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
        .collect();
    let mut best: Option<&Arc<Node>> = None;
    for node in nodes {
        if node.name != name
            || node.kind != NodeKind::Variable
            || node.start_line > line
            || node.signature.is_none()
        {
            continue;
        }
        let held_elsewhere = functions.iter().any(|function| {
            function.start_line <= node.start_line
                && node.start_line <= function.end_line
                && !(function.start_line <= line && line <= function.end_line)
        });
        if held_elsewhere || !is_local(node) {
            continue;
        }
        if best.is_none_or(|best| node.start_line > best.start_line) {
            best = Some(node);
        }
    }
    let around = functions
        .iter()
        .filter(|function| {
            function.start_line <= line && line <= function.end_line && function.start_line < line
        })
        .min_by_key(|function| (function.end_line - function.start_line, function.start_line));
    if let Some(function) = around
        && best.is_none_or(|best| best.start_line < function.start_line)
    {
        for at in ((function.start_line + 1)..line).rev() {
            let Some(text) = line_text(at) else {
                continue;
            };
            let Some(rhs) = local_line_initializer(text.trim_end_matches('\r'), name) else {
                continue;
            };
            let file_node = nodes.iter().find(|node| node.kind == NodeKind::File)?;
            let mut alias = (**file_node).clone();
            alias.name = name.to_string();
            alias.kind = NodeKind::Variable;
            alias.signature = Some(format!("= {rhs}"));
            alias.start_line = at;
            alias.end_line = at;
            return Some(Arc::new(alias));
        }
    }
    best.cloned()
}

/// The `local name = …` in scope at `line` of `file`: the nearest one above
/// it that no other function holds, or, inside a function, the nearest
/// `local name = …` line above the call within it (a function's own locals
/// are no nodes; upstream `luaLocalDecl`).
fn lua_local_decl(
    name: &str,
    file: &str,
    line: i64,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let nodes = file_nodes(file, context);
    let facts = context.source_facts(file);
    let line_text = |at: i64| -> Option<String> {
        let facts = facts.as_ref()?;
        usize::try_from(at - 1)
            .ok()
            .and_then(|index| facts.raw_line(index))
            .map(str::to_string)
    };
    local_decl_in(name, line, &nodes, &line_text, &|node| {
        crate::name_matcher::is_lua_local(node, context)
    })
}

/// Where an alias is written: its file, line and name, and a node there to
/// resolve `require`s from (upstream `LuaSite`).
struct LuaSite<'n> {
    file: &'n str,
    line: i64,
    name: &'n str,
    node: &'n Node,
}

/// What an alias variable names (upstream `luaAliasOf`).
fn lua_alias_of(decl: &Node, context: &dyn ResolutionContext, depth: usize) -> Alias {
    let Some(rhs) = initializer(decl) else {
        return Alias::Plain;
    };
    lua_alias_expr(
        rhs,
        &LuaSite {
            file: &decl.file_path,
            line: decl.start_line,
            name: &decl.name,
            node: decl,
        },
        context,
        depth,
    )
}

/// What an alias's initializer names (upstream `luaAliasExpr`).
fn lua_alias_expr(
    rhs: &str,
    site: &LuaSite<'_>,
    context: &dyn ResolutionContext,
    depth: usize,
) -> Alias {
    if let Some((module, path)) = require_parts(rhs) {
        return lua_module_member(&module, &path, site.node, context, depth);
    }
    let Some(named) = name_alias().captures(rhs) else {
        return Alias::Plain;
    };
    let root = &named[1];
    let path = members(named.get(2).map_or("", |m| m.as_str()));
    let root_decl = (root != site.name)
        .then(|| lua_local_decl(root, site.file, site.line - 1, context))
        .flatten();
    if let Some(root_decl) = root_decl {
        if let Some((module, mut module_path)) = initializer(&root_decl).and_then(require_parts)
            && !path.is_empty()
        {
            module_path.extend(path);
            return lua_module_member(&module, &module_path, &root_decl, context, depth);
        }
        return Alias::Plain;
    }
    if path.is_empty() {
        return if LUA_GLOBAL_FUNCTIONS.contains(&root) {
            Alias::Nothing
        } else {
            Alias::Plain
        };
    }
    if !LUA_LIBRARY_TABLES.contains(&root) {
        // A member of a global table the host provides (kong's `local
        // clear_header = kong.response.clear_header`) is the one method of a
        // table named after its holder.
        let member = path.last().expect("a non-empty path");
        let holder = if path.len() > 1 {
            path[path.len() - 2].as_str()
        } else {
            root
        };
        let owned: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(member)
            .into_iter()
            .filter(|node| {
                is_lua(node.language)
                    && node.kind == NodeKind::Method
                    && crate::kotlin_scope::shares_receiver_word(holder, node)
                    && !(codegraph_core::file_class::is_test_path(&node.file_path)
                        && !codegraph_core::file_class::is_test_path(site.file))
            })
            .collect();
        return match owned.as_slice() {
            [one] => Alias::Target(Arc::clone(one)),
            _ => Alias::Plain,
        };
    }
    if path.len() != 1 {
        return Alias::Plain;
    }
    // A library function the project patches itself (kong's `ngx.sleep`) is
    // the project's; a test's stand-in only for that test.
    let patched: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&path[0])
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node
                    .qualified_name
                    .split(['.', ':'])
                    .next()
                    .is_some_and(|first| first == root)
                && (node.file_path == site.file
                    || !codegraph_core::file_class::is_test_path(&node.file_path))
        })
        .collect();
    match patched.as_slice() {
        [one] => Alias::Target(Arc::clone(one)),
        _ => Alias::Nothing,
    }
}

/// `path` of the module `require(name)` returns, from `decl`'s file
/// (upstream `luaModuleMember`).
fn lua_module_member(
    name: &str,
    path: &[String],
    decl: &Node,
    context: &dyn ResolutionContext,
    depth: usize,
) -> Alias {
    let Some(file) = lua_module_file(name, decl, context) else {
        return Alias::Nothing;
    };
    // `require "kong.conf_loader"` called directly: the module's value.
    if path.is_empty() {
        return Alias::Plain;
    }
    let tables = context.lua_tables();
    let key = (file.clone(), path.join("."), depth);
    if let Some(hit) = tables
        .members
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned())
    {
        return hit;
    }
    let found = lua_member_in(&file, path, context, depth);
    if let Ok(mut memo) = tables.members.lock() {
        memo.insert(key, found.clone());
    }
    found
}

/// The project file `require(name)` loads from `decl`'s file, or `None` for
/// a module outside the project (upstream `luaModuleFile`).
pub(crate) fn lua_module_file(
    name: &str,
    decl: &Node,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let reference = RefView {
        row_id: None,
        from_node_id: decl.id.clone(),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::Imports,
        line: decl.start_line,
        column: 0,
        file_path: decl.file_path.clone(),
        language: decl.language,
        is_function_ref: false,
        reference_subkind: None,
    };
    let resolved = crate::import_resolver::resolve_lua_require(&reference, context)?;
    context
        .get_node_by_id_shared(&resolved.target_node_id)
        .map(|node| node.file_path.clone())
}

/// `source` with Lua comments removed, the newlines of a block comment kept
/// (upstream's two replacements).
pub(crate) fn strip_lua_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find("--") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let level = after
            .strip_prefix('[')
            .map(|tail| tail.bytes().take_while(|&b| b == b'=').count())
            .filter(|&equals| after.as_bytes().get(1 + equals) == Some(&b'['));
        match level {
            Some(equals) => {
                let close = format!("]{}]", "=".repeat(equals));
                let body_start = 2 + equals;
                match after[body_start..].find(&close) {
                    Some(end) => {
                        let comment = &rest[at..at + 2 + body_start + end + close.len()];
                        out.extend(comment.chars().filter(|&c| c == '\n'));
                        rest = &rest[at + 2 + body_start + end + close.len()..];
                    }
                    None => {
                        // An unclosed block comment: a line comment, as the
                        // second pattern reads it.
                        let end = rest[at..].find('\n').map_or(rest.len(), |end| at + end);
                        rest = &rest[end..];
                    }
                }
            }
            None => {
                let end = rest[at..].find('\n').map_or(rest.len(), |end| at + end);
                rest = &rest[end..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `path`'s member in a module file (upstream `luaMemberIn`): its function
/// of that name, else, within the alias depth, what the module exports under
/// it (`return { check = check_phase }`, `_M.check = check_phase`), an alias
/// of its own, or a global variable of that name.
fn lua_member_in(
    file: &str,
    path: &[String],
    context: &dyn ResolutionContext,
    depth: usize,
) -> Alias {
    let member = path.last().expect("a non-empty path").as_str();
    let nodes = file_nodes(file, context);
    let in_file: Vec<&Arc<Node>> = nodes.iter().filter(|node| node.name == member).collect();
    let functions: Vec<&Arc<Node>> = in_file
        .iter()
        .copied()
        .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
        .collect();
    if !functions.is_empty() {
        let owner = (path.len() > 1).then(|| path[path.len() - 2].as_str());
        let chosen = owner
            .and_then(|owner| {
                functions
                    .iter()
                    .find(|node| node.qualified_name.ends_with(&format!("{owner}::{member}")))
            })
            .or_else(|| functions.iter().find(|node| node.kind == NodeKind::Method))
            .unwrap_or(&functions[0]);
        return Alias::Target(Arc::clone(chosen));
    }
    if depth >= ALIAS_DEPTH {
        return Alias::Nothing;
    }
    let source = strip_lua_comments(&context.read_file(file).unwrap_or_default());
    if let Some((line, rhs)) = first_export(&source, member)
        && rhs != member
    {
        let rhs = rhs.to_string();
        if is_lua_name(&rhs)
            && let Some(function) = nodes.iter().find(|node| {
                node.name == rhs && matches!(node.kind, NodeKind::Function | NodeKind::Method)
            })
        {
            return Alias::Target(Arc::clone(function));
        }
        if let Some(at_node) = nodes
            .iter()
            .find(|node| node.kind == NodeKind::File)
            .or_else(|| nodes.first())
        {
            let next = lua_alias_expr(
                &rhs,
                &LuaSite {
                    file,
                    line,
                    name: member,
                    node: at_node,
                },
                context,
                depth + 1,
            );
            if !matches!(next, Alias::Plain) {
                return next;
            }
        }
    }
    // The module re-exports an alias of its own.
    if let Some(alias) = in_file.iter().find(|node| {
        node.kind == NodeKind::Variable
            && node.signature.is_some()
            && crate::name_matcher::is_lua_local(node, context)
    }) {
        let next = lua_alias_of(alias, context, depth + 1);
        if !matches!(next, Alias::Plain) {
            return next;
        }
    }
    match in_file.iter().find(|node| {
        node.kind == NodeKind::Variable && !crate::name_matcher::is_lua_local(node, context)
    }) {
        Some(global) => Alias::Target(Arc::clone(global)),
        None => Alias::Nothing,
    }
}

/// A bare Lua call through a `local` alias, followed to what the alias names
/// (upstream `luaAliasTarget`): `None` when the call is not through such an
/// alias, `Some(None)` when the alias names nothing in the project.
pub(crate) fn lua_alias_target(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    let tables = context.lua_tables();
    let key = (
        reference.file_path.clone(),
        reference.line,
        reference.reference_name.clone(),
    );
    let memo = tables
        .aliases
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned());
    let target = match memo {
        Some(hit) => hit,
        None => {
            let target = lua_local_decl(
                &reference.reference_name,
                &reference.file_path,
                reference.line,
                context,
            )
            .and_then(|decl| match lua_alias_of(&decl, context, 0) {
                Alias::Plain => None,
                Alias::Nothing => Some(None),
                Alias::Target(target) => Some(Some(target.id.clone())),
            });
            if let Ok(mut memo) = tables.aliases.lock() {
                memo.insert(key, target.clone());
            }
            target
        }
    };
    target.map(|target| {
        target.map(|target_node_id| ResolvedRef {
            original: reference.clone(),
            target_node_id,
            confidence: 0.9,
            resolved_by: ResolvedBy::Import,
        })
    })
}

/// Where a dotted name (`[A-Za-z_]\w*` segments joined by `.`, whitespace
/// allowed around each `.`) starting at `start` can end, longest first.
fn dotted_ends(source: &str, start: usize) -> Vec<usize> {
    let segment = |at: usize| -> usize {
        let bytes = source.as_bytes();
        if !bytes
            .get(at)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return 0;
        }
        bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count()
    };
    let mut ends = Vec::new();
    let first = segment(start);
    if first == 0 {
        return ends;
    }
    let mut end = start + first;
    ends.push(end);
    loop {
        let after = source[end..].trim_start_matches(is_js_space);
        let Some(dotted) = after.strip_prefix('.') else {
            break;
        };
        let next = source.len() - dotted.trim_start_matches(is_js_space).len();
        let length = segment(next);
        if length == 0 {
            break;
        }
        end = next + length;
        ends.push(end);
    }
    ends.reverse();
    ends
}

/// Whether upstream's tail `\s*(?:[,;}]|$)` (multiline) matches at the
/// start of `after`: a line terminator within the whitespace, the end of
/// the text, or `,`, `;` or `}` after it.
fn export_tail(after: &str) -> bool {
    for c in after.chars() {
        if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
            return true;
        }
        if !is_js_space(c) {
            return matches!(c, ',' | ';' | '}');
        }
    }
    true
}

/// The assignment `member = a.b` the pattern of upstream's `luaMemberIn`
/// matches at `at`, the start of `member` in `source`: its right side as
/// written.
fn export_at(source: &str, at: usize, member: &str) -> Option<usize> {
    let boundary = at == 0
        || source[..at]
            .chars()
            .next_back()
            .is_some_and(|c| is_js_space(c) || matches!(c, '{' | ',' | '.'));
    if !boundary || !source[at..].starts_with(member) {
        return None;
    }
    let after = source[at + member.len()..].trim_start_matches(is_js_space);
    let after = after.strip_prefix('=')?.trim_start_matches(is_js_space);
    let start = source.len() - after.len();
    dotted_ends(source, start)
        .into_iter()
        .find(|&end| export_tail(&source[end..]))
        .map(|end| end - start)
}

/// The line number upstream's `luaMemberIn` reads for a match at `at` (the
/// member's start): the match begins at the boundary character before it,
/// or at the text's start.
fn export_line(source: &str, at: usize) -> i64 {
    let start = source[..at]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index);
    i64::try_from(source[..start].matches('\n').count()).unwrap_or(i64::MAX - 2) + 2
}

/// The first export of `member` in a comment-free module source (upstream
/// `luaMemberIn`'s `member\s*=\s*dotted` pattern, scanned instead of
/// compiled per member): the line upstream reads and the right side.
fn first_export<'s>(source: &'s str, member: &str) -> Option<(i64, &'s str)> {
    source.match_indices(member).find_map(|(at, _)| {
        export_rhs(source, at, member).map(|rhs| (export_line(source, at), rhs))
    })
}

/// The right side of the export of `member` at `at`, if one is there.
fn export_rhs<'s>(source: &'s str, at: usize, member: &str) -> Option<&'s str> {
    let length = export_at(source, at, member)?;
    let start = source.len()
        - source[at + member.len()..]
            .trim_start_matches(is_js_space)
            .strip_prefix('=')?
            .trim_start_matches(is_js_space)
            .len();
    Some(&source[start..start + length])
}

/// Every first export of a comment-free module source, by member.
fn all_exports(source: &str) -> std::collections::BTreeMap<String, (i64, String)> {
    let mut out = std::collections::BTreeMap::new();
    let bytes = source.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let starts_name = (bytes[at].is_ascii_alphabetic() || bytes[at] == b'_')
            && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_'));
        if !starts_name {
            at += 1;
            continue;
        }
        let length = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
            .count();
        let member = &source[at..at + length];
        if !out.contains_key(member)
            && let Some(rhs) = export_rhs(source, at, member)
        {
            out.insert(
                member.to_string(),
                (export_line(source, at), rhs.to_string()),
            );
        }
        at += length;
    }
    out
}

/// Key prefix of the Lua module rows (W4-LUA-01): one `project_metadata`
/// row per Lua file, written by every full index and migration and kept
/// current by sync.
pub const MODULE_ROW_KEY_PREFIX: &str = "lua_module:";

/// The `project_metadata` key of `file_path`'s Lua module row.
pub fn module_row_key(file_path: &str) -> String {
    format!("{MODULE_ROW_KEY_PREFIX}{file_path}")
}

/// A declaration a row records as `local`: its name, kind and line.
pub type LocalDecl = (String, String, i64);

/// What another file's Lua resolution reads of one file: its `local`
/// declarations (a `local` is its chunk's alone, upstream #2143), the
/// modules its `local` aliases `require` (whose members they name, #2188),
/// the names its aliases and exports can read through a global table, and a
/// digest of what an alias through this module reads of it (its functions,
/// its variables and the first export of each name).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LuaModuleRow {
    /// Every `local` declaration, in source order, by name, kind and line: of
    /// two namesakes, the one another file's name matches first can change
    /// locality alone.
    pub locals: Vec<LocalDecl>,
    /// The modules the file's aliases `require`: read from each variable's
    /// initializer as the resolver reads it (with a trailing comment, over
    /// lines, as one target of several), and from every `local … = …` line
    /// (a function's own locals are no nodes).
    pub reads: Vec<String>,
    /// The last member of every dotted chain written after an `=` (`local
    /// snooze = ngx.sleep`, `exit = kong.response.exit,`), each shorter
    /// chain's too: an alias of a library or host table's member names the
    /// one project node of that name it finds (upstream's library and
    /// host-table rules), so it reads every node of the name.
    pub globals: Vec<String>,
    pub surface: String,
}

/// [`LuaModuleRow::globals`] of a source and its comment-free text.
fn global_members(sources: [&str; 2]) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for source in sources {
        for (at, _) in source.match_indices('=') {
            let start = source.len() - source[at + 1..].trim_start_matches(is_js_space).len();
            for end in dotted_ends(source, start) {
                if let Some((_, last)) = source[start..end].rsplit_once('.') {
                    out.insert(last.trim_start_matches(is_js_space).to_string());
                }
            }
        }
    }
    out.into_iter().collect()
}

impl LuaModuleRow {
    /// The row of a Lua file whose source is `source` and whose nodes are
    /// `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let lines: Vec<&str> = source.split('\n').collect();
        let line_text = |at: i64| -> Option<String> {
            usize::try_from(at - 1)
                .ok()
                .and_then(|index| lines.get(index))
                .map(|text| (*text).to_string())
        };
        let is_local = |node: &Node| {
            matches!(
                node.kind,
                NodeKind::Variable | NodeKind::Constant | NodeKind::Function
            ) && line_text(node.start_line)
                .is_some_and(|text| is_local_line(text.trim_end_matches('\r')))
        };
        let shared = in_order(nodes.iter().cloned().map(Arc::new).collect());
        let locals: Vec<LocalDecl> = shared
            .iter()
            .filter(|node| is_local(node))
            .map(|node| {
                (
                    node.name.clone(),
                    node.kind.as_str().to_string(),
                    node.start_line,
                )
            })
            .collect();
        let mut reads: Vec<String> = shared
            .iter()
            .filter(|node| node.kind == NodeKind::Variable)
            .filter_map(|node| initializer(node).and_then(require_parts))
            .map(|(module, _)| module)
            .chain(lines.iter().filter_map(|text| {
                let rest = text.trim_start_matches(is_js_space).strip_prefix("local")?;
                let (_, rhs) = rest.split_once('=')?;
                require_parts(rhs.trim_matches(is_js_space)).map(|(module, _)| module)
            }))
            .collect();
        reads.sort();
        reads.dedup();
        // In source order: a module's member is the first of its namesakes.
        let functions: Vec<(String, String, String)> = shared
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
            .map(|node| {
                (
                    node.kind.as_str().to_string(),
                    node.name.clone(),
                    node.qualified_name.clone(),
                )
            })
            .collect();
        let variables: Vec<(String, Option<String>, bool)> = shared
            .iter()
            .filter(|node| node.kind == NodeKind::Variable)
            .map(|node| (node.name.clone(), node.signature.clone(), is_local(node)))
            .collect();
        let stripped = strip_lua_comments(source);
        let exports: Vec<(String, String, Option<String>)> = all_exports(&stripped)
            .into_iter()
            .map(|(member, (line, rhs))| {
                let root = rhs
                    .split(|c: char| c == '.' || is_js_space(c))
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let root_decl = local_decl_in(&root, line - 1, &shared, &line_text, &is_local)
                    .and_then(|decl| decl.signature.clone());
                (member, rhs, root_decl)
            })
            .collect();
        let material = serde_json::json!({
            "functions": functions,
            "variables": variables,
            "exports": exports,
        });
        Self {
            locals,
            reads,
            globals: global_members([source, &stripped]),
            surface: codegraph_core::node_id::hash_content(&material.to_string()),
        }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("a Lua module row serializes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// The module paths a `require` of `module` can load (upstream
/// `resolveLuaRequire`'s suffixes): `a.b` is `a/b.lua`, `a/b.luau`,
/// `a/b/init.lua` or `a/b/init.luau`, under any folder.
pub fn lua_module_may_be(module: &str, file_path: &str) -> bool {
    let base = module.replace('.', "/");
    ["lua", "luau"].iter().any(|extension| {
        [
            format!("{base}.{extension}"),
            format!("{base}/init.{extension}"),
        ]
        .iter()
        .any(|suffix| file_path == suffix || file_path.ends_with(&format!("/{suffix}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values from upstream's two replacements, run under Node.
    #[test]
    fn lua_comments_are_stripped_keeping_block_newlines() {
        let cases: [(&str, &str); 4] = [
            (
                "a = 1 -- note\n--[[ one\ntwo ]] b = 2\n--[==[ x ]] y\n]==] c = 3\n",
                "a = 1 \n\n b = 2\n\n c = 3\n",
            ),
            ("x --[[ open\nno close\ny = 1\n", "x \nno close\ny = 1\n"),
            ("s = '--not' -- c\n", "s = '\n"),
            ("--[=[a]=]b", "b"),
        ];
        for (source, expected) in cases {
            assert_eq!(strip_lua_comments(source), expected, "{source:?}");
        }
    }

    /// The scanner finds what upstream's per-member pattern finds, line
    /// included (expected values from the pattern itself, run under Node).
    #[test]
    fn the_export_scanner_matches_upstreams_pattern() {
        type Case<'a> = (&'a str, &'a str, Option<(i64, &'a str)>);
        let cases: [Case; 10] = [
            (
                "return {\n  check = check_phase,\n}\n",
                "check",
                Some((3, "check_phase")),
            ),
            (
                "local M = {}\nM.check = phases.check\nreturn M\n",
                "check",
                Some((3, "phases.check")),
            ),
            ("x = a.b c\ncheck = a\n.b c\n", "check", Some((2, "a"))),
            ("return { a = b,check = c }\n", "check", Some((2, "c"))),
            (
                "if check == other then end\ncheck = done\n",
                "check",
                Some((2, "done")),
            ),
            (
                "xcheck = a\n_check = b\nM.check=c;",
                "check",
                Some((4, "c")),
            ),
            ("check = a .\n b . c\n", "check", Some((2, "a .\n b . c"))),
            ("check = 1\ncheck = real\n", "check", Some((2, "real"))),
            ("  check   =   tail   ", "check", Some((2, "tail"))),
            ("{check = x}", "check", Some((2, "x"))),
        ];
        for (source, member, expected) in cases {
            assert_eq!(first_export(source, member), expected, "{source:?}");
        }
        let all = all_exports("return {\n  check = check_phase,\n  run = M.run,\n}\n");
        assert_eq!(
            all.into_iter().collect::<Vec<_>>(),
            vec![
                ("check".to_string(), (3, "check_phase".to_string())),
                ("run".to_string(), (4, "M.run".to_string())),
            ]
        );
    }

    /// A Lua node of `kind` named `name` on lines `start..=end`.
    fn lua_node(kind: NodeKind, name: &str, start: i64, end: i64, signature: Option<&str>) -> Node {
        Node {
            id: format!("{}:{name}:{start}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "m.lua".to_string(),
            language: Language::Lua,
            start_line: start,
            end_line: end,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: signature.map(str::to_string),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    #[test]
    fn a_module_row_records_locals_reads_and_a_surface_blind_to_bodies() {
        let source = "local m = require(\"a.b\")\nlocal function check_phase(v)\n  local n = require 'c'.d\n  return v\nend\nfunction helper()\nend\nreturn { check = check_phase }\n";
        let nodes = [
            lua_node(NodeKind::File, "m.lua", 1, 8, None),
            lua_node(NodeKind::Variable, "m", 1, 1, Some("= require(\"a.b\")")),
            lua_node(NodeKind::Function, "check_phase", 2, 5, Some("(v)")),
            lua_node(NodeKind::Function, "helper", 6, 7, Some("()")),
        ];
        let row = LuaModuleRow::of(source, &nodes);
        assert_eq!(
            row.locals,
            [
                ("m".to_string(), "variable".to_string(), 1),
                ("check_phase".to_string(), "function".to_string(), 2),
            ]
        );
        assert_eq!(row.reads, ["a.b", "c"]);
        assert!(row.globals.is_empty(), "{:?}", row.globals);
        let body_edit = source.replace("  return v\n", "  return v + 1\n");
        assert_eq!(LuaModuleRow::of(&body_edit, &nodes).surface, row.surface);
        let export_edit = source.replace("check = check_phase", "check = helper");
        assert_ne!(LuaModuleRow::of(&export_edit, &nodes).surface, row.surface);
        let decoded = LuaModuleRow::decode(&row.encode());
        assert_eq!(decoded.as_ref(), Some(&row));
    }

    /// A row reads the module of every variable's initializer as the
    /// resolver reads it, a trailing comment, lines or other targets beside
    /// it; and its digest follows the source order of namesakes, the first of
    /// which a member lookup takes.
    #[test]
    fn a_module_row_reads_each_initializer_and_keeps_namesakes_in_order() {
        let source = "local p1 = require \"a.one\" -- note\nlocal p2 = require(\n  \"a.two\"\n)\nlocal n, p3 = 1, require \"a.three\"\n";
        let nodes = [
            lua_node(NodeKind::File, "m.lua", 1, 5, None),
            lua_node(NodeKind::Variable, "p1", 1, 1, Some("= require \"a.one\"")),
            lua_node(
                NodeKind::Variable,
                "p2",
                2,
                4,
                Some("= require(\n  \"a.two\"\n)"),
            ),
            lua_node(NodeKind::Variable, "n", 5, 5, Some("= 1")),
            lua_node(
                NodeKind::Variable,
                "p3",
                5,
                5,
                Some("= require \"a.three\""),
            ),
        ];
        let row = LuaModuleRow::of(source, &nodes);
        assert_eq!(row.reads, ["a.one", "a.three", "a.two"]);
        let swapped = "local x = require(\"b\").x\nlocal x = require(\"a\").x\n";
        let ordered = "local x = require(\"a\").x\nlocal x = require(\"b\").x\n";
        let namesakes = |first: &str, second: &str| {
            [
                lua_node(NodeKind::File, "m.lua", 1, 2, None),
                lua_node(NodeKind::Variable, "x", 1, 1, Some(first)),
                lua_node(NodeKind::Variable, "x", 2, 2, Some(second)),
            ]
        };
        assert_ne!(
            LuaModuleRow::of(
                ordered,
                &namesakes("= require(\"a\").x", "= require(\"b\").x")
            )
            .surface,
            LuaModuleRow::of(
                swapped,
                &namesakes("= require(\"b\").x", "= require(\"a\").x")
            )
            .surface
        );
    }

    /// Every member a chain after an `=` ends in, a shorter chain's too, in
    /// the code or (for an export) once the comments are gone.
    #[test]
    fn a_module_row_records_the_members_its_chains_read_through_global_tables() {
        let source = "local snooze = ngx.sleep\nlocal respond = kong . response\n  . exit\nlocal m = require(\"a.b\").c\nlocal t = { exit = kong.--[[ x ]]service.stop, n = 1 }\nif a == b.c then end\nlocal x = y\n";
        let row = LuaModuleRow::of(source, &[]);
        assert_eq!(
            row.globals,
            ["c", "exit", "response", "service", "sleep", "stop"]
        );
    }

    #[test]
    fn a_require_alias_names_its_module_and_members() {
        assert_eq!(
            require_parts(r#"require("kong.tools.string").splitn"#),
            Some(("kong.tools.string".to_string(), vec!["splitn".to_string()]))
        );
        assert_eq!(
            require_parts("require 'a.b'"),
            Some(("a.b".to_string(), Vec::new()))
        );
        assert_eq!(
            require_parts(r#"reload_module("spec.internal.misc") . x . y"#),
            Some((
                "spec.internal.misc".to_string(),
                vec!["x".to_string(), "y".to_string()]
            ))
        );
        assert_eq!(require_parts("string.format"), None);
    }

    /// One pass over a module finds, for every name in it, the export the
    /// per-member scan finds for that name alone.
    #[test]
    fn the_one_pass_export_scan_matches_the_per_member_scan() {
        let sources = [
            "return {\n  check = check_phase,\n}\n",
            "local M = {}\nM.check = phases.check\nreturn M\n",
            "x = a.b c\ncheck = a\n.b c\n",
            "return { a = b,check = c }\n",
            "if check == other then end\ncheck = done\n",
            "xcheck = a\n_check = b\nM.check=c;",
            "check = a .\n b . c\n",
            "check = 1\ncheck = real\n",
            "  check   =   tail   ",
            "{check = x}",
            "\u{e9}check = a\ncheck_x = b\ncheckcheck = c, check = d\n",
            "a=b\nb=c\nc = d.e\n+f = g\n",
        ];
        for source in sources {
            let all = all_exports(source);
            let names: std::collections::BTreeSet<&str> = source
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|name| name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
                .collect();
            for name in names {
                assert_eq!(
                    all.get(name).map(|(line, rhs)| (*line, rhs.as_str())),
                    first_export(source, name),
                    "{source:?} {name:?}"
                );
            }
        }
    }
}
