//! Object literals that own their members (upstream #2363, issue #2300).
//!
//! A named object literal, `const App = {…}`, the same inside an IIFE or a
//! function, `App = {…}`, or one hung on a path (`window.App = {…}`,
//! `App.utils = {…}`), makes each function member a node qualified under its
//! owner (`App::init`, `window.App::init`). An owner is a constant or variable
//! whose initializer is an object literal. A member is reached THROUGH its
//! object: `App.init()`, `window.App.init()`, `App.utils.fmt()`, a sibling's
//! `this.init()`, a `const { init } = App` binding, never by its name alone.
//! The holder a call names is the nearest one its own file declares around
//! it (an IIFE's `App` before the file's); otherwise a global one,
//! `window.App = {…}` anywhere or a classic script's top-level `App`, when
//! the calling file neither imports nor binds the name.
//!
//! Each file is scanned once per pass: its `{…}` blocks over the code with
//! comments and string contents blanked, and the scopes and destructuring
//! patterns a lookup asks about (upstream keeps the last 32 files' scans; the
//! per-pass source facts hold them here).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::generated_header::is_js_whitespace;
use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;

use crate::name_matcher::{
    BareCallReceiver, bare_call_receiver, cjs_export_marker, is_import_binding, is_js_family,
    is_js_ts, is_object_literal_language, is_simple_js_identifier, js_export_marker,
    js_function_local_scope, js_import_marker, range_within, resolve_object_literal_member,
    same_language_family,
};
use crate::object_literal::resolve_object_literal_binding;
use crate::source_facts::SourceFacts;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Offset ranges a name is bound in, `(open, close)`; -1 opens the file.
type Scopes = Arc<Vec<(i64, i64)>>;

/// What the object-literal lookups read from one JS/TS file: its `{…}`
/// blocks, and, on first use, the scopes it binds a name in, the names it
/// destructures off a path, and whether it is a classic script.
#[derive(Debug, Default)]
pub struct JsObjectScan {
    /// Offsets of every `{` and `}` of the code, in order.
    events: Vec<i64>,
    /// The innermost block still open just after each event (its `{`
    /// offset), -1 at the top level.
    open: Vec<i64>,
    close: HashMap<i64, i64>,
    length: i64,
    bindings: Mutex<HashMap<String, Scopes>>,
    destructured: OnceLock<HashMap<String, HashSet<String>>>,
    classic: OnceLock<bool>,
}

/// The file's scan, its blocks read on first use.
fn scan_of(facts: &SourceFacts) -> Arc<JsObjectScan> {
    facts.js_object_scan(|facts| read_js_blocks(facts.ts_blanked()))
}

fn read_js_blocks(code: &str) -> JsObjectScan {
    let mut scan = JsObjectScan {
        length: code.len() as i64,
        ..JsObjectScan::default()
    };
    let mut stack: Vec<i64> = Vec::new();
    for (at, byte) in code.bytes().enumerate() {
        let at = at as i64;
        match byte {
            b'{' => stack.push(at),
            b'}' => {
                if let Some(open) = stack.pop() {
                    scan.close.insert(open, at);
                }
            }
            _ => continue,
        }
        scan.events.push(at);
        scan.open.push(stack.last().copied().unwrap_or(-1));
    }
    scan
}

/// The offset of (line, column) in a file.
fn js_offset(facts: &SourceFacts, scan: &JsObjectScan, line: i64, column: i64) -> i64 {
    let start = usize::try_from(line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.lines().raw_line_start(index))
        .map_or(scan.length, |start| start as i64);
    start + column
}

/// The innermost `{…}` holding an offset, as (open, close); `None` at the
/// file's top level.
fn js_block_at(scan: &JsObjectScan, offset: i64) -> Option<(i64, i64)> {
    let before = scan.events.partition_point(|&event| event < offset);
    let open = *scan.open.get(before.checked_sub(1)?)?;
    (open >= 0).then(|| (open, scan.close.get(&open).copied().unwrap_or(scan.length)))
}

/// A path on the global object, `window.` or `globalThis.` (`self` is the
/// global only in a worker; in page code it is far more often `var self =
/// this`).
fn on_host_global(path: &str) -> bool {
    path.starts_with("window.") || path.starts_with("globalThis.")
}

/// The last `::` segment of a qualified name.
fn last_qualified_segment(qualified_name: &str) -> &str {
    qualified_name
        .rsplit_once("::")
        .map_or(qualified_name, |(_, last)| last)
}

/// The path a constant or variable is reached by: its name, or, for an
/// object hung on a path by assignment, that path, any global-object root
/// dropped (`window.App = {…}` defines the global `App`; `App.utils = {…}` is
/// `App.utils`).
fn holder_path(node: &Node) -> String {
    let last = last_qualified_segment(&node.qualified_name);
    if !last.contains('.') {
        return node.name.clone();
    }
    last.strip_prefix("window.")
        .or_else(|| last.strip_prefix("globalThis."))
        .unwrap_or(last)
        .to_string()
}

/// Whether a constant or variable was hung on a dotted path by assignment
/// rather than declared.
fn is_path_holder(node: &Node) -> bool {
    last_qualified_segment(&node.qualified_name).contains('.')
}

/// Whether `node` holds an object literal: an owner its members can belong
/// to.
fn is_object_literal_owner(node: &Node) -> bool {
    matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
        && is_js_family(node.language)
        && node.signature.as_deref().is_some_and(|signature| {
            signature
                .strip_prefix('=')
                .is_some_and(|rest| rest.trim_start().starts_with('{'))
        })
}

/// The object literal a function node is a member of: its qualified parent,
/// an owner in its file whose extent holds it. `None` for every other
/// function: a declaration, a helper nested in a function, a function inside
/// an initializer that is not an object literal (a module IIFE's own helpers).
pub(crate) fn object_literal_owner_of(
    node: &Node,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    if node.kind != NodeKind::Function || !is_js_family(node.language) {
        return None;
    }
    let (parent, _) = node.qualified_name.rsplit_once("::")?;
    if parent.is_empty() {
        return None;
    }
    context
        .get_nodes_by_qualified_name_shared(parent)
        .into_iter()
        .find(|owner| {
            owner.file_path == node.file_path
                && owner.id != node.id
                && is_object_literal_owner(owner)
                && range_within(node, owner)
        })
}

/// The node a qualified name's parent names, in `node`'s file and holding it.
fn qualified_parent_of(node: &Node, context: &dyn ResolutionContext) -> Option<Arc<Node>> {
    let (parent, _) = node.qualified_name.rsplit_once("::")?;
    if parent.is_empty() {
        return None;
    }
    context
        .get_nodes_by_qualified_name_shared(parent)
        .into_iter()
        .find(|holder| {
            holder.file_path == node.file_path && holder.id != node.id && range_within(node, holder)
        })
}

/// The text of `file`'s `line` (1-based) from `column` (a byte offset, moved
/// back to a character boundary).
fn line_from(
    context: &dyn ResolutionContext,
    file: &str,
    line: i64,
    column: i64,
) -> Option<String> {
    let facts = context.source_facts(file)?;
    let text = facts.raw_line(usize::try_from(line).ok()?.checked_sub(1)?)?;
    let mut column = usize::try_from(column).unwrap_or(0).min(text.len());
    while !text.is_char_boundary(column) {
        column -= 1;
    }
    Some(text[column..].to_string())
}

/// Whether an object literal's member is an arrow function, which has no
/// `this` of its own: its text starts `(a) =>`, `async (a) =>`, `a =>` or
/// `<T>(a: T) =>`, never `name(…) {` or `function …`.
fn is_arrow_member(node: &Node, context: &dyn ResolutionContext) -> bool {
    static ARROW_HEAD: OnceLock<Regex> = OnceLock::new();
    let head = ARROW_HEAD.get_or_init(|| {
        Regex::new(r"^(?:async\s*)?(?:[(<]|[A-Za-z_$][0-9A-Za-z_$]*\s*=>)").expect("arrow head")
    });
    line_from(context, &node.file_path, node.start_line, node.start_column)
        .is_some_and(|text| head.is_match(&text))
}

/// The caller whose scope `this` belongs to, for code written in `caller`. An
/// object literal's own method, `load() {…}` or `load: function () {…}`, has
/// the object: it is returned as is, so its qualified parent is the owner. An
/// arrow member has no `this` of its own, and a value written in the literal
/// runs where the literal is: both take the `this` of the code around the
/// literal, a class method's in `class App { api() { return { load: () =>
/// this.x() } } }`. Any other caller is its own scope.
pub(crate) fn this_scope_caller(caller: Arc<Node>, context: &dyn ResolutionContext) -> Arc<Node> {
    if !is_js_family(caller.language) {
        return caller;
    }
    let mut current = caller;
    for _ in 0..8 {
        let owner = if is_object_literal_owner(&current) {
            Some(Arc::clone(&current))
        } else {
            object_literal_owner_of(&current, context)
                .filter(|_| is_arrow_member(&current, context))
        };
        let Some(owner) = owner else {
            break;
        };
        let Some(up) = qualified_parent_of(&owner, context) else {
            break;
        };
        current = up;
    }
    current
}

/// The object literal `this` is inside `caller`, when it is one: the owner of
/// an object's own method.
fn this_object_of(caller: Arc<Node>, context: &dyn ResolutionContext) -> Option<Arc<Node>> {
    let scope = this_scope_caller(caller, context);
    let owner = object_literal_owner_of(&scope, context)?;
    (!is_arrow_member(&scope, context)).then_some(owner)
}

/// Whether (line, column) falls inside `node`'s source range.
fn position_within(line: i64, column: i64, node: &Node) -> bool {
    if line < node.start_line || line > node.end_line {
        return false;
    }
    if line == node.start_line && column < node.start_column {
        return false;
    }
    !(line == node.end_line && column >= node.end_column)
}

/// Whether a reference by NAME alone can mean an object literal's member.
/// Only three ways write one: the owner's own `this.load()` (a sibling
/// member), a named function expression calling itself (`load: function
/// load() { load() }`), and a binding destructured off the owner (`const {
/// load } = App`). A bare `load()`, a callback `setTimeout(load)` or
/// `window.Other.load()` means a function of that name, or nothing.
///
/// `owner` is the member's [`object_literal_owner_of`]; a function that is no
/// object's member is reached by name as any other.
pub(crate) fn is_member_reachable_through(
    owner: &Node,
    node: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if reference.file_path == node.file_path {
        // `this` must be that object: written in one of its own methods, not
        // in an arrow member.
        if reference.reference_kind == EdgeKind::Calls
            && position_within(reference.line, reference.column, owner)
            && matches!(
                bare_call_receiver(reference, context),
                Some(BareCallReceiver::OwnObject)
            )
            && context
                .get_node_by_id_shared(&reference.from_node_id)
                .and_then(|caller| this_object_of(caller, context))
                .is_some_and(|this| this.id == owner.id)
        {
            return true;
        }
        if position_within(reference.line, reference.column, node)
            && let Some(text) =
                line_from(context, &node.file_path, node.start_line, node.start_column)
            && let Ok(named) = Regex::new(&format!(
                r"^(?:async\s+)?function\s*\*?\s*{}",
                regex::escape(&node.name)
            ))
            && named
                .find(&text)
                .is_some_and(|found| !text[found.end()..].starts_with(is_js_word_char))
        {
            return true;
        }
    }
    destructures_member(
        &reference.file_path,
        &holder_path(owner),
        &node.name,
        context,
    )
}

/// A JS word character, `[\w$]`.
fn is_js_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Whether an object hung on a dotted path, not the global object, is what a
/// bare name means: never. `App.utils = {…}` and `$.event.special.swipe =
/// {…}` are properties, not bindings, so a `this.swipe()` elsewhere is not
/// that object; one hung on `window` is the global `App`.
pub(crate) fn is_unreachable_path_holder(node: &Node, reference: &RefView) -> bool {
    // The kind first: most candidates are no constant or variable.
    is_object_literal_owner(node)
        && is_path_holder(node)
        && is_simple_js_identifier(&reference.reference_name)
        && !on_host_global(last_qualified_segment(&node.qualified_name))
}

/// Whether `file` binds `member` by destructuring it off the object at
/// `path`: `{ load }` or `{ a, load = fallback }` off `App`, never `{ x: load
/// }`, which binds `load` to `App.x`. The file's patterns are read once.
fn destructures_member(
    file: &str,
    path: &str,
    member: &str,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(facts) = context.source_facts(file) else {
        return false;
    };
    let scan = scan_of(&facts);
    scan.destructured
        .get_or_init(|| read_destructured_paths(facts.ts_blanked()))
        .get(path)
        .is_some_and(|names| names.contains(member))
}

/// Each plain path a pattern is destructured off (the whole right-hand side:
/// `{ … } = App`, `= window.App.utils`) → the names it binds under their own
/// key.
fn read_destructured_paths(code: &str) -> HashMap<String, HashSet<String>> {
    static DESTRUCTURED: OnceLock<Regex> = OnceLock::new();
    let pattern = DESTRUCTURED.get_or_init(|| {
        Regex::new(
            r"\{([^{}]*)\}\s*=\s*(?:(?:window|globalThis)\s*\.\s*)?([A-Za-z_$][0-9A-Za-z_$]*(?:\s*\.\s*[A-Za-z_$][0-9A-Za-z_$]*)*)",
        )
        .expect("destructured path")
    });
    let mut paths: HashMap<String, HashSet<String>> = HashMap::new();
    for found in pattern.captures_iter(code) {
        let whole = found.get(0).expect("match");
        // The path is the whole right-hand side, not a call or a member's
        // receiver.
        if code[whole.end()..]
            .trim_start_matches(is_js_whitespace)
            .starts_with(['.', '(', '['])
        {
            continue;
        }
        let own: Vec<String> = destructured_keys(&found[1])
            .into_iter()
            .filter(|(name, key)| name == key)
            .map(|(name, _)| name)
            .collect();
        if own.is_empty() {
            continue;
        }
        let at: String = found[2].chars().filter(|c| !c.is_whitespace()).collect();
        paths.entry(at).or_default().extend(own);
    }
    paths
}

/// The (local name, key) pairs of a destructuring pattern's parts whose key
/// is an identifier: `a`, `b: c`, `d = 1` give (a, a), (c, b), (d, d).
fn destructured_keys(pattern: &str) -> Vec<(String, String)> {
    static DEFAULT: OnceLock<Regex> = OnceLock::new();
    let default = DEFAULT.get_or_init(|| Regex::new(r"\s*=.*$").expect("default value"));
    pattern
        .split(',')
        .filter_map(|part| {
            let mut sides = part
                .split(':')
                .map(|side| default.replace(side.trim(), "").into_owned());
            let key = sides.next()?;
            let local = sides.next().unwrap_or_else(|| key.clone());
            is_simple_js_identifier(&key).then_some((local, key))
        })
        .collect()
}

/// The offset ranges a JS/TS file binds `name` in: a declaration's or a
/// function or class name's enclosing block (the whole file at the top
/// level), a parameter's function body (an expression-bodied arrow's, to the
/// end of its enclosing block). The patterns are the local-binding ones;
/// only the scope is added, so one function's parameter does not shadow a
/// global in another.
fn binding_scopes(name: &str, facts: &SourceFacts, scan: &JsObjectScan) -> Scopes {
    if let Some(known) = scan
        .bindings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(name)
    {
        return Arc::clone(known);
    }
    let code = facts.ts_blanked();
    let mut scopes: Vec<(i64, i64)> = Vec::new();
    if code.contains(name) {
        let length = code.len() as i64;
        let block_of = |at: i64| js_block_at(scan, at).unwrap_or((-1, length));
        // The body a parameter list (ending in `{` or `=>`) opens.
        let body_after = |start: i64, end: i64| -> (i64, i64) {
            let end_at = usize::try_from(end).unwrap_or(0);
            if end_at >= 1 && code.as_bytes().get(end_at - 1) == Some(&b'{') {
                return (
                    end - 1,
                    scan.close.get(&(end - 1)).copied().unwrap_or(length),
                );
            }
            let rest = code.get(end_at..).unwrap_or_default();
            let skipped = rest.len() - rest.trim_start_matches(is_js_whitespace).len();
            let brace = end + skipped as i64;
            if rest[skipped..].starts_with('{') {
                return (brace, scan.close.get(&brace).copied().unwrap_or(length));
            }
            (start, block_of(start).1)
        };
        let n = regex::escape(name);
        let declaration = Regex::new(&format!(
            r"(?-u:\b)(?:const|let|var)\s+(?:{n}(?-u:\b)|[{{\[][^;=]*?(?-u:\b){n}(?-u:\b)[^;=]*?[}}\]])\s*(?:=\s*([^;\n]*))?"
        ));
        let function = Regex::new(&format!(r"(?-u:\b)(?:function|class)\s+{n}(?-u:\b)"));
        let parameter = Regex::new(&format!(
            r"\(\s*(?:(?:\.\.\.)?[0-9A-Za-z_$]+(?:\s*(?:\?\s*)?:[^,()]+|\s*=[^,()]+|\s*),\s*)*{n}(?-u:\b)(?:\s*\??\s*:[^,()]*)?(?:\s*=[^,()]*)?(?:\s*,\s*[^()]*)?\)\s*(?::[^=;{{]*)?(?:=>|\{{)"
        ));
        for found in declaration
            .iter()
            .flat_map(|pattern| pattern.find_iter(code))
        {
            scopes.push(block_of(found.start() as i64));
        }
        for found in function.iter().flat_map(|pattern| pattern.find_iter(code)) {
            scopes.push(block_of(found.start() as i64));
        }
        for found in parameter.iter().flat_map(|pattern| pattern.find_iter(code)) {
            scopes.push(body_after(found.start() as i64, found.end() as i64));
        }
        // An arrow's lone parameter, `name =>`, not preceded by `[\w$.]`.
        for (at, _) in code.match_indices(name) {
            if code[..at]
                .chars()
                .next_back()
                .is_some_and(|before| is_js_word_char(before) || before == '.')
            {
                continue;
            }
            let rest = &code[at + name.len()..];
            let arrow = rest.trim_start_matches(is_js_whitespace);
            if arrow.starts_with("=>") {
                let end = (at + name.len() + (rest.len() - arrow.len()) + 2) as i64;
                scopes.push(body_after(at as i64, end));
            }
        }
    }
    let scopes = Arc::new(scopes);
    scan.bindings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(name.to_string(), Arc::clone(&scopes));
    scopes
}

/// Whether a binding of `name` in the reference's own file encloses the
/// reference.
fn binds_name_at(name: &str, reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let scan = scan_of(&facts);
    let at = js_offset(&facts, &scan, reference.line, reference.column);
    binding_scopes(name, &facts, &scan)
        .iter()
        .any(|&(start, end)| at > start && at < end)
}

/// Where a same-file holder of an object can be seen from: the block it is
/// declared in (an IIFE's, a function's), or the whole file for one at the
/// top level, and for a path hung by assignment, which is a property of its
/// root object wherever that is. The depth to rank by is the block's `{`
/// offset, -1 for the whole file; `None` when the reference cannot see it,
/// outside its block or where the calling function binds the root name
/// itself.
fn holder_scope_at(
    holder: &Node,
    reference: &RefView,
    via_global: bool,
    context: &dyn ResolutionContext,
) -> Option<i64> {
    let mut depth = -1;
    // A single-file component's source holds its template and styles too,
    // whose braces are not blocks: a holder there is read as the script's
    // top level.
    if !is_path_holder(holder)
        && is_js_ts(holder.language)
        && let Some(facts) = context.source_facts(&holder.file_path)
    {
        let scan = scan_of(&facts);
        let declared = js_offset(&facts, &scan, holder.start_line, holder.start_column);
        if let Some((open, close)) = js_block_at(&scan, declared) {
            let at = js_offset(&facts, &scan, reference.line, reference.column);
            if at <= open || at >= close {
                return None;
            }
            depth = open;
        }
    }
    if !via_global {
        let path = holder_path(holder);
        let root = path.split('.').next().unwrap_or_default();
        if let Some((start, end)) = js_function_local_scope(root, reference, context)
            && !(holder.file_path == reference.file_path
                && holder.start_line >= start
                && holder.start_line <= end)
        {
            return None;
        }
    }
    Some(depth)
}

/// Whether a JS/TS file is a classic script, with no `import`, `export`,
/// CommonJS export or `require(…)`, whose top-level names are globals every
/// other script on the page shares.
fn is_classic_script(file: &str, context: &dyn ResolutionContext) -> bool {
    static REQUIRE: OnceLock<Regex> = OnceLock::new();
    let Some(facts) = context.source_facts(file) else {
        return false;
    };
    let scan = scan_of(&facts);
    *scan.classic.get_or_init(|| {
        let lower = file.to_ascii_lowercase();
        if lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
            || cjs_export_marker().is_match(facts.source())
        {
            return false;
        }
        let code = facts.ts_blanked();
        let require = REQUIRE.get_or_init(|| {
            Regex::new(r"(?:^|[^0-9A-Za-z_$.])require\s*\(").expect("require call")
        });
        !js_import_marker().is_match(code)
            && !js_export_marker().is_match(code)
            && !require.is_match(code)
    })
}

/// Whether a holder in another file is a global: on the global object, or at
/// the top level of a classic script.
fn is_global_holder(node: &Node, context: &dyn ResolutionContext) -> bool {
    if on_host_global(last_qualified_segment(&node.qualified_name)) {
        return true;
    }
    if !is_js_ts(node.language) || !is_classic_script(&node.file_path, context) {
        return false;
    }
    if !is_path_holder(node) {
        let Some(facts) = context.source_facts(&node.file_path) else {
            return false;
        };
        let scan = scan_of(&facts);
        return js_block_at(
            &scan,
            js_offset(&facts, &scan, node.start_line, node.start_column),
        )
        .is_none();
    }
    // `App.utils = {…}` is global when `App` is: declared at a classic
    // script's top level, or on the global object.
    let path = holder_path(node);
    let root = path.split('.').next().unwrap_or_default();
    context
        .get_nodes_by_name_shared(root)
        .into_iter()
        .any(|holder| {
            matches!(holder.kind, NodeKind::Constant | NodeKind::Variable)
                && is_js_family(holder.language)
                && holder_path(&holder) == root
                && holder.id != node.id
                && is_global_holder(&holder, context)
        })
}

/// Resolve `member` on the object a dotted `path` names, at `reference`:
/// `App` for `App.init()` / `window.App.init()`, `App.utils` for
/// `App.utils.fmt()`. A same-file holder the reference can see comes first,
/// the innermost; then, when the file neither imports nor binds the path's
/// root, a global one: `window.App = {…}` anywhere, or a classic script's
/// top-level `App`. Several equally near holders that all have the member are
/// no answer. `host` is the global object the call was written on
/// (`window.App.init()`), `None` for a bare `App.init()`. `None` when no holder
/// of the path exists at all (so a caller can try its other strategies),
/// `Some(None)` when one exists but none has the member.
pub(crate) fn resolve_object_path_member(
    path: &str,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    host: Option<&str>,
) -> Option<Option<ResolvedRef>> {
    let via_global = host.is_some();
    let tail = path.rsplit('.').next().unwrap_or(path);
    // The name's object-literal constants and variables by their paths, read
    // once per pass (W5-08), then this call's own filter.
    let holders = context.name_matcher_tables().object_holders(tail, || {
        let mut by_path = crate::name_matcher::HoldersByPath::new();
        for node in context.get_nodes_by_name_shared(tail) {
            if matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
                && is_object_literal_language(node.language)
            {
                by_path.entry(holder_path(&node)).or_default().push(node);
            }
        }
        by_path
    });
    let named: Vec<Arc<Node>> = holders
        .get(path)
        .into_iter()
        .flatten()
        .filter(|node| same_language_family(node.language, reference.language))
        .cloned()
        .collect();
    if named.is_empty() {
        return None;
    }
    let hit_on = |holder: &Node| {
        resolve_object_literal_member(
            holder,
            member,
            reference,
            context,
            0.85,
            ResolvedBy::InstanceMethod,
        )
        .or_else(|| resolve_object_literal_binding(holder, member, reference, context))
    };

    let mut local: Vec<(i64, &Arc<Node>)> = named
        .iter()
        .filter(|node| node.file_path == reference.file_path)
        .filter_map(|node| {
            holder_scope_at(node, reference, via_global, context).map(|depth| (depth, node))
        })
        .collect();
    if !local.is_empty() {
        // The nearest holders only: an outer one is shadowed where an inner
        // one exists.
        local.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        let nearest = local[0].0;
        return Some(
            local
                .iter()
                .take_while(|(depth, _)| *depth == nearest)
                .find_map(|(_, holder)| hit_on(holder)),
        );
    }

    let hits: Vec<(&Arc<Node>, ResolvedRef)> = named
        .iter()
        .filter(|node| {
            node.file_path != reference.file_path
                && is_object_literal_owner(node)
                && is_global_holder(node, context)
        })
        .filter_map(|node| hit_on(node).map(|hit| (node, hit)))
        .collect();
    if hits.is_empty() {
        return Some(None);
    }
    // A global is what the call names only where nothing nearer binds the
    // name: the file's own import of it, a parameter or local around the
    // call, or, for `window.App`, a `window` of its own.
    let root = path.split('.').next().unwrap_or(path);
    let bound = match host {
        Some(host) => binds_name_at(host, reference, context),
        None => {
            is_import_binding(root, reference, context) || binds_name_at(root, reference, context)
        }
    };
    if bound {
        return Some(None);
    }
    let one_target = |hits: &[&(&Arc<Node>, ResolvedRef)]| {
        hits.iter()
            .map(|(_, hit)| hit.target_node_id.as_str())
            .collect::<HashSet<_>>()
            .len()
            == 1
    };
    let all: Vec<&(&Arc<Node>, ResolvedRef)> = hits.iter().collect();
    if one_target(&all) {
        let mut hit = hits[0].1.clone();
        hit.confidence = 0.8;
        return Some(Some(hit));
    }
    // The same namespace defined twice (a page's copy and a build's): the
    // caller's own directory decides, or nothing does.
    let dir = reference
        .file_path
        .rfind('/')
        .map_or("", |slash| &reference.file_path[..=slash]);
    let near: Vec<&(&Arc<Node>, ResolvedRef)> = hits
        .iter()
        .filter(|(node, _)| {
            node.file_path
                .strip_prefix(dir)
                .is_some_and(|rest| !rest.contains('/'))
        })
        .collect();
    if !near.is_empty() && one_target(&near) {
        let mut hit = near[0].1.clone();
        hit.confidence = 0.75;
        return Some(Some(hit));
    }
    Some(None)
}

/// `a.b.m()` through a namespace an object literal was hung on (`App.utils =
/// {…}` then `App.utils.fmt()`). An identifier-rooted chain carries no type
/// (#1566), so nothing else may resolve it: this answers only when a holder of
/// exactly that path exists.
pub(crate) fn match_object_path_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != EdgeKind::Calls || !is_js_family(reference.language) {
        return None;
    }
    let (path, member) = reference.reference_name.rsplit_once('.')?;
    if path.is_empty() {
        return None;
    }
    let (host, path) = match path.split_once('.') {
        Some((root, rest)) if matches!(root, "window" | "globalThis") => (Some(root), rest),
        _ => (None, path),
    };
    if !path.contains('.') && host.is_none() {
        return None;
    }
    resolve_object_path_member(path, member, reference, context, host).flatten()
}

/// A call the extractor recorded by its bare name although it was written on
/// an object: `window.App.init()` (the `window` chain keeps its bare name as
/// the project-global escape, #1707) and `this.render()` inside an object
/// literal's member. Both name the owner of the member they call, which is
/// tried first. `None` when the call is neither, or no such owner has the
/// member: the bare-name strategies then run as before.
pub(crate) fn match_collapsed_object_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    static HOST_GLOBAL_RECEIVER: OnceLock<Regex> = OnceLock::new();
    let name = reference.reference_name.as_str();
    if reference.reference_kind != EdgeKind::Calls
        || !is_js_family(reference.language)
        || !is_simple_js_identifier(name)
    {
        return None;
    }
    // A call written bare starts with its own name and `(`: no receiver to
    // read.
    if let Some(text) = line_from(
        context,
        &reference.file_path,
        reference.line,
        reference.column,
    ) && text
        .strip_prefix(name)
        .is_some_and(|after| after.trim_start().starts_with('('))
    {
        return None;
    }
    match bare_call_receiver(reference, context)? {
        BareCallReceiver::OwnObject => {
            // The object literal `this` is, the owner of the method the call
            // is written in (an arrow member's `this` is the code around the
            // literal's), when it has the member.
            let caller = context.get_node_by_id_shared(&reference.from_node_id)?;
            let owner = this_object_of(caller, context)?;
            resolve_object_literal_member(
                &owner,
                name,
                reference,
                context,
                0.85,
                ResolvedBy::InstanceMethod,
            )
        }
        BareCallReceiver::Other { receiver, .. } => {
            let host = HOST_GLOBAL_RECEIVER.get_or_init(|| {
                Regex::new(
                    r"(?:^|[^0-9A-Za-z_$.])(window|globalThis)\s*\??\.\s*([A-Za-z_$][0-9A-Za-z_$]*(?:\s*\??\.\s*[A-Za-z_$][0-9A-Za-z_$]*)*)$",
                )
                .expect("global-object receiver")
            });
            let found = host.captures(&receiver)?;
            let path: String = found[2]
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '?')
                .collect();
            resolve_object_path_member(&path, name, reference, context, Some(&found[1])).flatten()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{destructured_keys, js_block_at, read_destructured_paths, read_js_blocks};

    #[test]
    fn blocks_name_the_innermost_brace_open_at_an_offset() {
        let code = "a { b { c } d } e";
        let scan = read_js_blocks(code);
        assert_eq!(js_block_at(&scan, 0), None);
        assert_eq!(js_block_at(&scan, 4), Some((2, 14)));
        assert_eq!(js_block_at(&scan, 8), Some((6, 10)));
        assert_eq!(js_block_at(&scan, 12), Some((2, 14)));
        assert_eq!(js_block_at(&scan, 16), None);
        // An unclosed block runs to the end of the code.
        let open = read_js_blocks("x { y");
        assert_eq!(js_block_at(&open, 4), Some((2, 5)));
    }

    #[test]
    fn destructuring_off_a_plain_path_binds_its_own_keys() {
        let paths = read_destructured_paths(
            "const { load, a: b, c = 1 } = App;\nconst { d } = window.App . utils;\nconst { e } = App.make();\nconst { f } = App.x[0];\n",
        );
        let names = |path: &str| {
            let mut names: Vec<String> = paths
                .get(path)
                .map(|names| names.iter().cloned().collect())
                .unwrap_or_default();
            names.sort();
            names
        };
        assert_eq!(names("App"), ["c", "load"]);
        assert_eq!(names("App.utils"), ["d"]);
        assert!(names("App.make").is_empty() && names("App.x").is_empty());
        assert_eq!(
            destructured_keys(" a, b: c , 'd': e, f = g"),
            [
                ("a".to_string(), "a".to_string()),
                ("c".to_string(), "b".to_string()),
                ("f".to_string(), "f".to_string()),
            ]
        );
    }
}
