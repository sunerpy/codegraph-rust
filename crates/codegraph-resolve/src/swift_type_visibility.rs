//! Where a Swift reference to a type may land (upstream #2107,
//! `swift-type-visibility.ts` in the shape at `b635dd46`: its non-dispatch
//! half; the protocol-extension dispatch is W7-18's and the type-path calls
//! W4-SW-02's).
//!
//! Two shapes bound type references to the wrong node:
//!
//! 1. `extension View { … }` parses as a class_declaration, so the index
//!    holds a `class View` node for every file that extends SwiftUI's `View`,
//!    `Text` or `Color`. With no declaration of the type in the project,
//!    every `struct X: View` and `Text("…")` bound to whichever extension came
//!    first. An extension is not the type: a reference lands on the type's own
//!    declaration, or nowhere when the SDK declares it.
//! 2. A nested type is named bare only inside its parent (or an extension or
//!    subtype of it): `@State` in a view is SwiftUI's property wrapper, never
//!    another file's `ListViewModel.State`. The index keeps a type reference
//!    by its last segment, so a site's qualifier (`Build.Id` vs `Package.Id`)
//!    is read back from the source line.
//!
//! [`gate_swift_type_target`] applies to every strategy's result. A
//! conformance to a type the project only extends now resolves to nothing,
//! so the supertype walk reads such conformances from the declaration
//! ([`extended_conformances`]) and the extensions' members still reach the
//! conformer. Extraction is unchanged; what the readers take from a file's
//! text is kept per file for sync ([`HeadRow`]).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::csharp_types::js_regex;
use crate::source_facts::SourceFacts;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The kinds a Swift type declaration is indexed as (upstream `TYPE_KINDS`).
fn is_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Protocol
            | NodeKind::TypeAlias
    )
}

fn is_swift_type(node: &Node) -> bool {
    node.language == Language::Swift && is_type_kind(node.kind)
}

/// The per-pass memos (upstream's `memos`).
#[derive(Debug, Default)]
pub struct SwiftTables {
    extension: Mutex<HashMap<String, bool>>,
    clauses: Mutex<HashMap<String, Arc<Vec<String>>>>,
    conformances: Mutex<HashMap<String, Arc<Vec<String>>>>,
    inherited: Mutex<HashMap<String, Arc<BTreeSet<String>>>>,
    declaration: Mutex<HashMap<String, Option<Arc<Node>>>>,
    owners: Mutex<HashMap<String, Option<String>>>,
    decls: Mutex<HashMap<String, Arc<crate::swift_scope::SwiftDecl>>>,
    hierarchies: Mutex<HashMap<String, Arc<HashMap<String, usize>>>>,
}

impl SwiftTables {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`crate::swift_scope::swift_decl_of`]'s memo.
    pub(crate) fn decl(&self, type_name: &str) -> Option<Arc<crate::swift_scope::SwiftDecl>> {
        memo_get(&self.decls, type_name)
    }

    pub(crate) fn set_decl(&self, type_name: &str, decl: Arc<crate::swift_scope::SwiftDecl>) {
        memo_set(&self.decls, type_name.to_string(), decl);
    }

    /// The memo of the hierarchies around call sites, by file and line.
    pub(crate) fn hierarchy(&self, key: &str) -> Option<Arc<HashMap<String, usize>>> {
        memo_get(&self.hierarchies, key)
    }

    pub(crate) fn set_hierarchy(&self, key: String, hierarchy: Arc<HashMap<String, usize>>) {
        memo_set(&self.hierarchies, key, hierarchy);
    }
}

fn memo_get<V: Clone>(memo: &Mutex<HashMap<String, V>>, key: &str) -> Option<V> {
    memo.lock().ok().and_then(|memo| memo.get(key).cloned())
}

fn memo_set<V>(memo: &Mutex<HashMap<String, V>>, key: String, value: V) {
    if let Ok(mut memo) = memo.lock() {
        memo.insert(key, value);
    }
}

/// `"…"`: a string literal's contents, which are no keyword or clause.
fn string_literal() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r#""(?:[^"\\\n]|\\.)*""#).expect("Swift string pattern"))
}

/// The first declaration keyword of a class_declaration, past its
/// attributes and modifiers (upstream `DECLARATION_KEYWORD`).
fn declaration_keyword() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?-u:\b)(extension|class|struct|enum|actor|protocol)(?-u:\b)")
            .expect("Swift declaration keyword pattern")
    })
}

/// A declaration's inheritance clause (upstream `INHERITANCE_CLAUSE`):
/// `struct HomeView: View, Sendable {` → `View, Sendable`.
fn inheritance_clause() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        js_regex(
            r"(?-u:\b)(?:class|struct|enum|actor|protocol|extension)\s+[A-Za-z_][A-Za-z0-9_.]*\s*(?:<[^{}]*?>)?\s*:\s*([^{]*?)\s*(?:(?-u:\b)where(?-u:\b)[^{]*)?\{",
        )
    })
}

/// `<Success>` / `<Key, [Value]>`: one level of nesting is all a qualifier
/// carries in practice (upstream `GENERIC_ARGS`).
fn generic_args() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"<[^<>]*(?:<[^<>]*>[^<>]*)*>").expect("Swift generic arguments pattern")
    })
}

/// One segment of a written qualifier, its dot included (upstream
/// `QUALIFIER_SEGMENT`).
const QUALIFIER_SEGMENT: &str = r"[A-Za-z_][A-Za-z0-9_]*(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*\.\s*";

/// A qualifier ending where the name begins (upstream `QUALIFIER_BEFORE`).
fn qualifier_before() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(&format!(r"((?:{QUALIFIER_SEGMENT})+)$")))
}

/// One qualifier segment at the start of a text.
fn leading_segment() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| js_regex(&format!(r"^{QUALIFIER_SEGMENT}")))
}

/// The qualifier a chain written from the start of `text` puts before
/// `name` (upstream's `^((?:QUALIFIER_SEGMENT)+)name\b`): the longest run of
/// segments followed by `name` and a word boundary. Read segment by segment,
/// so no pattern is compiled per reference.
fn chain_qualifier<'t>(text: &'t str, name: &str) -> Option<&'t str> {
    let mut end = 0;
    let mut found = None;
    while let Some(segment) = leading_segment().find(&text[end..]) {
        end += segment.end();
        let rest = &text[end..];
        if rest.starts_with(name)
            && !rest[name.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            found = Some(&text[..end]);
        }
    }
    found
}

/// A composition's continuation line: indentation, an `&`, then a blank.
fn composition_continuation() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[ \t]*&[ \t]").expect("Swift composition pattern"))
}

/// `^[A-Za-z_]\w*$`, JavaScript's `\w`.
fn is_identifier(name: &str) -> bool {
    crate::csharp_types::is_simple_identifier(name)
}

/// Line `line` (1-based) of a file, as `split(/\r?\n/)` gives it.
fn line_of(facts: &SourceFacts, line: i64) -> Option<&str> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    facts
        .raw_line(index)
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// The text of a declaration's head: its first line from its column, then
/// the lines after it up to `extra` more, within its span (upstream's
/// `[lines[start - 1].slice(column), ...lines.slice(start, min(end, start +
/// extra))].join(' ')`), with every string literal emptied.
fn head_text(facts: &SourceFacts, node: &Node, extra: i64) -> String {
    let first = line_of(facts, node.start_line).unwrap_or("");
    let column = usize::try_from(node.start_column).unwrap_or(0);
    let mut parts: Vec<&str> = vec![first.get(column..).unwrap_or("")];
    for line in node.start_line + 1..=node.end_line.min(node.start_line + extra) {
        if let Some(text) = line_of(facts, line) {
            parts.push(text);
        }
    }
    string_literal()
        .replace_all(&parts.join(" "), "\"\"")
        .into_owned()
}

/// Does the class_declaration at `node`'s position open with `extension`?
/// (upstream `declaresExtension`.)
pub fn declares_extension(facts: &SourceFacts, node: &Node) -> bool {
    declaration_keyword()
        .captures(&head_text(facts, node, 4))
        .and_then(|captures| captures.get(1))
        .is_some_and(|keyword| keyword.as_str() == "extension")
}

/// The supertypes a declaration's own head names, last segments with their
/// generic arguments dropped: `View`, `Sendable` for `struct HomeView: View,
/// Sendable {` (upstream `declaredSupertypes`).
pub fn head_supertypes(facts: &SourceFacts, node: &Node) -> Vec<String> {
    let head = head_text(facts, node, 6);
    let Some(clause) = inheritance_clause()
        .captures(&head)
        .and_then(|captures| captures.get(1))
    else {
        return Vec::new();
    };
    clause
        .as_str()
        .split(',')
        .filter_map(|part| {
            let part = generic_args().replace_all(part, "");
            let name = part.trim().rsplit('.').next().unwrap_or("").trim();
            is_identifier(name).then(|| name.to_string())
        })
        .collect()
}

/// Is this Swift node an `extension` of a type rather than its declaration?
/// Extraction classifies an extension as a class (upstream
/// `isSwiftExtension`).
pub fn is_swift_extension(node: &Node, context: &dyn ResolutionContext) -> bool {
    if node.language != Language::Swift || node.kind != NodeKind::Class {
        return false;
    }
    let tables = context.swift_tables();
    if let Some(hit) = memo_get(&tables.extension, &node.id) {
        return hit;
    }
    let extension = context
        .source_facts(&node.file_path)
        .is_some_and(|facts| declares_extension(&facts, node));
    memo_set(&tables.extension, node.id.clone(), extension);
    extension
}

/// [`head_supertypes`] of a node, memoized.
fn declared_supertypes(node: &Node, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    let tables = context.swift_tables();
    if let Some(hit) = memo_get(&tables.clauses, &node.id) {
        return hit;
    }
    let names = Arc::new(
        context
            .source_facts(&node.file_path)
            .map(|facts| head_supertypes(&facts, node))
            .unwrap_or_default(),
    );
    memo_set(&tables.clauses, node.id.clone(), Arc::clone(&names));
    names
}

/// The types a Swift type node's own declaration conforms to that the
/// project only extends: `View` for `struct HomeView: View` when `extension
/// View {}` is all the index holds of it (upstream
/// `swiftExtendedConformances`). Those conformances resolve to nothing (an
/// extension is not the type), yet the members the extensions add are the
/// conformer's: the supertype walk reads them from here.
pub fn extended_conformances(node: &Node, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    if node.language != Language::Swift {
        return Arc::new(Vec::new());
    }
    let tables = context.swift_tables();
    if let Some(hit) = memo_get(&tables.conformances, &node.id) {
        return hit;
    }
    let names: Vec<String> = declared_supertypes(node, context)
        .iter()
        .filter(|name| {
            let typed: Vec<Arc<Node>> = context
                .get_nodes_by_name_shared(name)
                .into_iter()
                .filter(|n| is_swift_type(n))
                .collect();
            !typed.is_empty() && typed.iter().all(|n| is_swift_extension(n, context))
        })
        .cloned()
        .collect();
    let names = Arc::new(names);
    memo_set(&tables.conformances, node.id.clone(), Arc::clone(&names));
    names
}

/// Every type a Swift type inherits from or conforms to, as declared in the
/// project, transitively, four levels (upstream `inheritedTypeNames`).
fn inherited_type_names(type_name: &str, context: &dyn ResolutionContext) -> Arc<BTreeSet<String>> {
    let tables = context.swift_tables();
    if let Some(hit) = memo_get(&tables.inherited, type_name) {
        return hit;
    }
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut frontier = vec![type_name.to_string()];
    for _ in 0..4 {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for name in &frontier {
            for node in context.get_nodes_by_name_shared(name) {
                if !is_swift_type(&node) {
                    continue;
                }
                for supertype in declared_supertypes(&node, context).iter() {
                    if supertype == type_name || found.contains(supertype) {
                        continue;
                    }
                    found.insert(supertype.clone());
                    next.push(supertype.clone());
                }
            }
        }
        frontier = next;
    }
    let found = Arc::new(found);
    memo_set(&tables.inherited, type_name.to_string(), Arc::clone(&found));
    found
}

/// The type path a nested type is declared in (`AccountsListViewModel` for
/// `AccountsListViewModel::State`), or `None` at the top level. A framework
/// resolver's node is named after its file, not a scope.
fn parent_path(node: &Node) -> Option<&str> {
    let sep = node.qualified_name.rfind("::").filter(|&sep| sep > 0)?;
    let parent = &node.qualified_name[..sep];
    (!parent.contains('/') && !parent.ends_with(".swift")).then_some(parent)
}

/// How deep the code at `from` sits in `target`'s scope, when it may name
/// `target` bare: 0 for a top-level type, the parent's segment count inside
/// the parent or a subtype of it, one more inside the type itself; `None`
/// when out of scope (upstream `bareScopeDepth`). An `extension
/// API.PackageController` carries only its last segment
/// (`PackageController::get`), so a suffix of a path counts as inside it.
fn bare_scope_depth(
    target: &Node,
    from: Option<&Node>,
    context: &dyn ResolutionContext,
) -> Option<usize> {
    let Some(parent) = parent_path(target) else {
        return Some(0);
    };
    let from = from?;
    let scope = from.qualified_name.as_str();
    let inside = |path: &str| -> bool {
        let segments: Vec<&str> = path.split("::").collect();
        (0..segments.len()).any(|at| {
            let suffix = segments[at..].join("::");
            scope == suffix || scope.starts_with(&format!("{suffix}::"))
        })
    };
    let depth = parent.split("::").count();
    // Inside the type itself (`extension NIOBSDSocket.Option { static let
    // x: Option }`) or inside its parent.
    if inside(&target.qualified_name) {
        return Some(depth + 1);
    }
    if inside(parent) {
        return Some(depth);
    }
    // A subclass (or conformer) names its supertype's nested types bare:
    // `ReadResult` in `BaseStreamSocketChannel: BaseSocketChannel`.
    let parent_name = parent.rsplit("::").next().unwrap_or(parent);
    scope
        .split("::")
        .any(|name| inherited_type_names(name, context).contains(parent_name))
        .then_some(depth)
}

/// `text[at..]` starts with `name`.
fn starts_at(text: &str, name: &str, at: usize) -> bool {
    text.get(at..).is_some_and(|rest| rest.starts_with(name))
}

/// The reference as its site writes it (upstream `writtenName`). The index
/// keeps a Swift type reference by its last segment (`Build.Id` is a ref
/// named `Id`), so whether the name was qualified is read back from the line:
/// a type annotation's column is the name's (`Build.|Id`), a construction's
/// the chain's (`|API.PackageController.GetRoute.Model(`).
fn written_name(reference: &RefView, context: &dyn ResolutionContext) -> String {
    let name = reference.reference_name.as_str();
    if name.contains('.') {
        return name.to_string();
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return name.to_string();
    };
    let Some(line) = line_of(&facts, reference.line) else {
        return name.to_string();
    };
    // `EventLoopFuture<Success>.Isolated`: the qualifier's generic arguments
    // are not part of its path.
    let clean = |qualifier: &str| -> String {
        let bare = generic_args().replace_all(qualifier, "");
        let joined: String = bare.chars().filter(|c| !c.is_whitespace()).collect();
        format!("{joined}{name}")
    };
    let column = usize::try_from(reference.column).unwrap_or(usize::MAX);
    // A composition's `&` line is parsed one column left of the file, its
    // `&` moved onto the line before (`join_swift_composition_continuations`).
    let at = if !starts_at(line, name, column)
        && composition_continuation().is_match(line)
        && starts_at(line, name, column.saturating_add(1))
    {
        column + 1
    } else {
        column
    };
    if starts_at(line, name, at) {
        let mut from = at.saturating_sub(240);
        while !line.is_char_boundary(from) {
            from += 1;
        }
        return qualifier_before()
            .captures(&line[from..at])
            .and_then(|captures| captures.get(1))
            .map_or_else(|| name.to_string(), |qualifier| clean(qualifier.as_str()));
    }
    let Some(rest) = line.get(column..) else {
        return name.to_string();
    };
    let mut end = rest.len().min(240);
    while !rest.is_char_boundary(end) {
        end -= 1;
    }
    chain_qualifier(&rest[..end], name).map_or_else(|| name.to_string(), clean)
}

/// `API.PackageController.Model` as `API::PackageController::Model`, up to
/// `name`; `None` when `name` is not a segment (upstream `writtenPath`).
fn written_path(written: &str, name: &str) -> Option<String> {
    let segments: Vec<&str> = written.split('.').collect();
    let at = segments.iter().rposition(|segment| *segment == name)?;
    Some(segments[..=at].join("::"))
}

/// Does a type's qualified name fit the path a site wrote? (upstream
/// `fitsPath`.) A type declared in `extension API.PackageController.GetRoute`
/// is `GetRoute::Model`, so its name may be a tail of the path; a
/// module-qualified `Foundation.Date` names a top-level type.
fn fits_path(node: &Node, path: &str, context: &dyn ResolutionContext) -> bool {
    let qualified = node.qualified_name.as_str();
    if qualified == path || qualified.ends_with(&format!("::{path}")) {
        return true;
    }
    if !path.ends_with(&format!("::{qualified}")) {
        return false;
    }
    if qualified.contains("::") {
        return true;
    }
    let head = &path[..path.len() - qualified.len() - 2];
    let parent_name = head.rsplit("::").next().unwrap_or(head);
    // A member declared under `#if` in a type's body can be indexed at the
    // top level (the grammar ends `enum NIOBSDSocket {` at an `#if` right
    // inside it): it still shares a file with a declaration or extension of
    // the parent the site names.
    if context
        .get_nodes_in_file_shared(&node.file_path)
        .iter()
        .any(|p| p.name == parent_name && is_type_kind(p.kind) && p.id != node.id)
    {
        return true;
    }
    // `App.Build`: a top-level type under a qualifier that names no type is
    // a module's.
    !head.contains("::")
        && !context
            .get_nodes_by_name_shared(head)
            .iter()
            .any(|n| is_swift_type(n))
}

/// Directory segments two paths share from the root (upstream `sharedDirs`).
fn shared_dirs(a: &str, b: &str) -> usize {
    let da: Vec<&str> = a.split('/').collect();
    let db: Vec<&str> = b.split('/').collect();
    let da = &da[..da.len().saturating_sub(1)];
    let db = &db[..db.len().saturating_sub(1)];
    da.iter().zip(db).take_while(|(x, y)| x == y).count()
}

/// The project's own declaration of the type a site names, when exactly one
/// fits (upstream `declarationFor`). Candidates are read in path order
/// (KEEP-RUST: upstream reads them in index order; the choice below does not
/// depend on it but for a span tie within one file).
fn declaration_for(
    name: &str,
    written: &str,
    reference: &RefView,
    from: Option<&Node>,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let tables = context.swift_tables();
    let key = format!(
        "{}\0{}\0{written}\0{name}",
        reference.file_path,
        from.map_or("", |from| from.qualified_name.as_str())
    );
    if let Some(hit) = memo_get(&tables.declaration, &key) {
        return hit;
    }
    let bare = !written.contains('.');
    let path = if bare {
        None
    } else {
        written_path(written, name)
    };
    let mut candidates: Vec<Arc<Node>> = context.get_nodes_by_name_shared(name);
    candidates.sort_by(|a, b| {
        (&a.file_path, a.start_line, a.start_column, &a.id).cmp(&(
            &b.file_path,
            b.start_line,
            b.start_column,
            &b.id,
        ))
    });
    let mut scored: Vec<(Arc<Node>, usize)> = Vec::new();
    for node in candidates {
        if !is_swift_type(&node) || is_swift_extension(&node, context) {
            continue;
        }
        if let Some(path) = &path {
            if fits_path(&node, path, context) {
                scored.push((node, 0));
            }
            continue;
        }
        let depth = if bare {
            bare_scope_depth(&node, from, context)
        } else {
            Some(0)
        };
        if let Some(depth) = depth {
            scored.push((node, depth));
        }
    }
    // Swift looks a bare name up innermost scope first.
    let deepest = scored.iter().map(|(_, depth)| *depth).max();
    // One declaration per file: a framework resolver's one-line twin of a
    // declaration (`@main struct App`) is the same type.
    let mut by_file: Vec<Arc<Node>> = Vec::new();
    for (node, depth) in scored {
        if Some(depth) != deepest {
            continue;
        }
        match by_file
            .iter_mut()
            .find(|had| had.file_path == node.file_path)
        {
            Some(had) => {
                if node.end_line - node.start_line > had.end_line - had.start_line {
                    *had = node;
                }
            }
            None => by_file.push(node),
        }
    }
    let found = match by_file.as_slice() {
        [] => None,
        [only] => Some(Arc::clone(only)),
        declarations => {
            // The same file's, else the nearest by directory (a package's own
            // type over another package's). A tie is left unresolved.
            declarations
                .iter()
                .find(|n| n.file_path == reference.file_path)
                .cloned()
                .or_else(|| {
                    let near = declarations
                        .iter()
                        .map(|n| shared_dirs(&n.file_path, &reference.file_path))
                        .max()
                        .unwrap_or(0);
                    let nearest: Vec<&Arc<Node>> = declarations
                        .iter()
                        .filter(|n| shared_dirs(&n.file_path, &reference.file_path) == near)
                        .collect();
                    match nearest.as_slice() {
                        [only] => Some(Arc::clone(only)),
                        _ => None,
                    }
                })
        }
    };
    memo_set(&tables.declaration, key, found.clone());
    found
}

/// A Swift reference's resolved type target, checked against what the site
/// wrote (upstream `gateSwiftTypeTarget`): an extension moves to the
/// declaration it extends, a qualified name to the type on that path, and a
/// bare name of a nested type out of its scope to one in scope, or the
/// reference is left unresolved.
pub(crate) fn gate_swift_type_target(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    if reference.language != Language::Swift || reference.reference_kind == EdgeKind::Imports {
        return Some(result);
    }
    let Some(target) = context.get_node_by_id_shared(&result.target_node_id) else {
        return Some(result);
    };
    if !is_swift_type(&target) {
        return Some(result);
    }
    let from = context.get_node_by_id_shared(&reference.from_node_id);
    let written = written_name(reference, context);
    if !is_swift_extension(&target, context) {
        if !written.contains('.') {
            if bare_scope_depth(&target, from.as_deref(), context).is_some() {
                return Some(result);
            }
        } else {
            match written_path(&written, &target.name) {
                None => return Some(result),
                Some(path) if fits_path(&target, &path, context) => return Some(result),
                Some(_) => {}
            }
        }
    }
    let declared = declaration_for(&target.name, &written, reference, from.as_deref(), context)?;
    if declared.id == result.target_node_id {
        Some(result)
    } else {
        Some(ResolvedRef {
            target_node_id: declared.id.clone(),
            ..result
        })
    }
}

/// A call through a type path, `API.PackageController.GetRoute.query`: at
/// least two capitalized segments before the member, not led by `Self.`
/// (upstream `SWIFT_TYPE_PATH_CALL`; the extractor keeps the path only for
/// those).
pub(crate) fn is_swift_type_path_call(reference: &RefView) -> bool {
    if reference.language != Language::Swift || reference.reference_kind != EdgeKind::Calls {
        return false;
    }
    let name = reference.reference_name.as_str();
    if name.starts_with("Self.") {
        return false;
    }
    let segments: Vec<&str> = name.split('.').collect();
    let Some((member, path)) = segments.split_last() else {
        return false;
    };
    let capitalized = |segment: &str| {
        let mut chars = segment.chars();
        chars.next().is_some_and(|c| c.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    path.len() >= 2 && path.iter().all(|segment| capitalized(segment)) && is_identifier(member)
}

/// What a type-path call can name: a method, a function, or a case with
/// associated values (`Gitlab.Error.requestFailed(status:)`).
fn is_path_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::EnumMember
    )
}

/// `API.PackageController.GetRoute.query(on:)` lands on the `query` of the
/// type the path names, and `API.PackageController.Model(name:)` on that
/// nested type, never on the member's name alone (upstream #2118
/// `resolveSwiftTypePathCall`): in a Vapor app every route's type has a
/// `query`. A path the call's own namespace lets it shorten
/// (`PackageController.GetRoute` inside `extension API`) or a module
/// qualifier (`Vapor.HTTPStatus`) still fits. No type on the path, or two,
/// leaves the call unresolved; the caller tries nothing else.
pub(crate) fn resolve_swift_type_path_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let name = reference.reference_name.as_str();
    let dot = name.rfind('.')?;
    let member = &name[dot + 1..];
    let path = name[..dot].replace('.', "::");
    let candidates = context.get_nodes_by_name_shared(member);
    let on_path = |kinds: &dyn Fn(NodeKind) -> bool| -> Vec<Arc<Node>> {
        let mut exact = Vec::new();
        let mut shortened = Vec::new();
        for n in &candidates {
            if n.language != Language::Swift || !kinds(n.kind) {
                continue;
            }
            if is_type_kind(n.kind) && is_swift_extension(n, context) {
                continue;
            }
            let Some(owner) = owner_path(n, context) else {
                continue;
            };
            if owner == path {
                exact.push(Arc::clone(n));
            } else if owner.ends_with(&format!("::{path}"))
                || module_qualified(&owner, &path, context)
            {
                shortened.push(Arc::clone(n));
            }
        }
        if exact.is_empty() { shortened } else { exact }
    };
    // `API.PackageController.Model(name:)` constructs a nested type.
    let mut fit = if member.starts_with(|c: char| c.is_ascii_uppercase()) {
        on_path(&is_type_kind)
    } else {
        Vec::new()
    };
    if fit.is_empty() {
        fit = on_path(&is_path_member_kind);
    }
    // The Composable Architecture's `@Reducer enum Path { case detail(Detail)
    // }` generates `Path.State` and `Path.Action` with the same cases, so
    // `Path.State.detail(…)` constructs the case written in `Path`.
    if fit.is_empty()
        && let Some(reducer) = path
            .strip_suffix("::State")
            .or_else(|| path.strip_suffix("::Action"))
            .filter(|reducer| !reducer.is_empty())
    {
        fit = candidates
            .iter()
            .filter(|n| {
                n.language == Language::Swift
                    && n.kind == NodeKind::EnumMember
                    && owner_path(n, context).is_some_and(|owner| {
                        owner == reducer || owner.ends_with(&format!("::{reducer}"))
                    })
                    && is_reducer_enum(n, context)
            })
            .cloned()
            .collect();
    }
    let owners: BTreeSet<Option<String>> = fit.iter().map(|n| owner_path(n, context)).collect();
    if fit.is_empty() || owners.len() > 1 {
        return None;
    }
    // Of one type's overloads, the first declared.
    let target = fit
        .into_iter()
        .min_by(|a, b| (&a.file_path, a.start_line).cmp(&(&b.file_path, b.start_line)))?;
    Some(ResolvedRef::new(
        reference.clone(),
        target.id.clone(),
        0.9,
        ResolvedBy::QualifiedName,
    ))
}

/// Is this case declared in a `@Reducer enum`? (upstream `isReducerEnum`.)
fn is_reducer_enum(member: &Node, context: &dyn ResolutionContext) -> bool {
    let Some(owner) = context
        .get_nodes_in_file_shared(&member.file_path)
        .into_iter()
        .filter(|t| {
            t.kind == NodeKind::Enum
                && t.start_line <= member.start_line
                && t.end_line >= member.end_line
        })
        .max_by_key(|t| t.start_line)
    else {
        return false;
    };
    context
        .source_facts(&owner.file_path)
        .is_some_and(|facts| has_reducer_attribute(&facts, &owner))
}

/// Whether `@Reducer` is written on an enum's first line or the line above
/// it (the node starts at its attributes).
fn has_reducer_attribute(facts: &SourceFacts, owner: &Node) -> bool {
    let text: Vec<&str> = (owner.start_line - 1..=owner.start_line + 1)
        .filter_map(|line| line_of(facts, line))
        .collect();
    reducer_attribute().is_match(&text.join(" "))
}

/// `@Reducer` as a whole word.
fn reducer_attribute() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"@Reducer(?-u:\b)").expect("Swift reducer pattern"))
}

/// `Vapor::HTTPStatus` for a type `HTTPStatus`: a qualifier that names no
/// project type is a module (upstream `moduleQualified`).
fn module_qualified(owner: &str, path: &str, context: &dyn ResolutionContext) -> bool {
    let Some(head) = path.strip_suffix(&format!("::{owner}")) else {
        return false;
    };
    !head.contains("::")
        && !context
            .get_nodes_by_name_shared(head)
            .iter()
            .any(|n| is_swift_type(n))
}

/// The full path of the type a member is declared on (upstream
/// `ownerPath`). Its qualified name starts at the outermost declaration in
/// its file, and an extension is named by its last segment (the members of
/// `extension API.PackageController { enum GetRoute { … } }` are
/// `PackageController::GetRoute::…`), so the outermost extension's written
/// path is read back from its line. `None` for a top-level function.
fn owner_path(member: &Node, context: &dyn ResolutionContext) -> Option<String> {
    let tables = context.swift_tables();
    if let Some(hit) = memo_get(&tables.owners, &member.id) {
        return hit;
    }
    let path = member
        .qualified_name
        .rfind("::")
        .map(|cut| member.qualified_name[..cut].to_string())
        .map(|path| {
            let first = path.split("::").next().unwrap_or(&path).to_string();
            let outer = context
                .get_nodes_in_file_shared(&member.file_path)
                .into_iter()
                .filter(|t| {
                    t.name == first
                        && is_type_kind(t.kind)
                        && t.start_line <= member.start_line
                        && t.end_line >= member.end_line
                })
                .min_by_key(|t| t.start_line);
            match outer.and_then(|outer| extended_path(&outer, context)) {
                Some(extended) => format!("{extended}{}", &path[first.len()..]),
                None => path,
            }
        });
    memo_set(&tables.owners, member.id.clone(), path.clone());
    path
}

/// `extension API.PackageController {` → `API::PackageController`; `None`
/// for anything but an extension of a nested type (upstream `extendedPath`).
fn extended_path(node: &Node, context: &dyn ResolutionContext) -> Option<String> {
    if !is_swift_extension(node, context) {
        return None;
    }
    let facts = context.source_facts(&node.file_path)?;
    extended_path_in(&facts, node)
}

/// [`extended_path`]'s reading of an extension's head.
fn extended_path_in(facts: &SourceFacts, node: &Node) -> Option<String> {
    static WRITTEN: OnceLock<Regex> = OnceLock::new();
    let first = line_of(facts, node.start_line).unwrap_or("");
    let column = usize::try_from(node.start_column).unwrap_or(0);
    let mut parts: Vec<&str> = vec![first.get(column..).unwrap_or("")];
    for line in node.start_line + 1..=node.start_line + 3 {
        if let Some(text) = line_of(facts, line) {
            parts.push(text);
        }
    }
    let head = string_literal()
        .replace_all(&parts.join(" "), "\"\"")
        .into_owned();
    let written = WRITTEN.get_or_init(|| {
        js_regex(r"(?-u:\b)extension\s+((?:[A-Za-z_][A-Za-z0-9_]*\s*\.\s*)+[A-Za-z_][A-Za-z0-9_]*)")
    });
    let path = written.captures(&head)?.get(1)?.as_str();
    let path: String = path.chars().filter(|c| !c.is_whitespace()).collect();
    Some(path.replace('.', "::"))
}

/// Key prefix of the Swift head rows (W4-SW-01): one `project_metadata` row
/// per Swift file, written by every full index and migration and kept current
/// by sync.
pub const HEAD_ROW_KEY_PREFIX: &str = "swift_heads:";

/// The `project_metadata` key of `file_path`'s head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// One Swift type declaration, as the readers of this module and of
/// [`crate::swift_scope`] take it from the text: whether it is an extension,
/// what its head names (by both readers), the nested path an extension
/// writes, and whether an enum is a TCA `@Reducer`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    pub qualified_name: String,
    pub kind: String,
    pub extension: bool,
    pub supertypes: Vec<String>,
    pub inherits: Vec<String>,
    pub written: Option<String>,
    pub reducer: bool,
}

impl HeadEntry {
    /// The declared type's own name.
    pub fn name(&self) -> &str {
        self.qualified_name
            .rsplit("::")
            .next()
            .unwrap_or(&self.qualified_name)
    }
}

/// A Swift file's head row: every type declaration, sorted (positions are not
/// part of it, so an edit above a type changes nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a file whose text is `source` and whose nodes are `nodes`.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let facts = SourceFacts::new(Arc::from(source));
        let mut entries: Vec<HeadEntry> = nodes
            .iter()
            .filter(|node| is_swift_type(node))
            .map(|node| {
                let extension = node.kind == NodeKind::Class && declares_extension(&facts, node);
                HeadEntry {
                    qualified_name: node.qualified_name.clone(),
                    kind: node.kind.as_str().to_string(),
                    extension,
                    supertypes: head_supertypes(&facts, node),
                    inherits: crate::class_heads::swift_head_in(&facts, node).supers,
                    written: extension.then(|| extended_path_in(&facts, node)).flatten(),
                    reducer: node.kind == NodeKind::Enum && has_reducer_attribute(&facts, node),
                }
            })
            .collect();
        entries.sort();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("swift head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, qualified_name: &str, start_line: i64, end_line: i64) -> Node {
        Node {
            id: format!("{}:{qualified_name}", kind.as_str()),
            kind,
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_string(),
            qualified_name: qualified_name.to_string(),
            file_path: "Sources/App/A.swift".to_string(),
            language: Language::Swift,
            start_line,
            end_line,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
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

    fn facts(text: &str) -> SourceFacts {
        SourceFacts::new(Arc::from(text))
    }

    #[test]
    fn an_extension_is_told_from_a_declaration_by_its_first_keyword() {
        let text = "@available(*, deprecated, message: \"use class Foo\")\nextension View {\n}\npublic final class Box {\n}\n";
        let lines = facts(text);
        assert!(declares_extension(
            &lines,
            &node(NodeKind::Class, "View", 1, 3)
        ));
        assert!(!declares_extension(
            &lines,
            &node(NodeKind::Class, "Box", 4, 5)
        ));
    }

    #[test]
    fn a_head_names_its_supertypes_by_their_last_segments() {
        let text = "struct HomeView: SwiftUI.View, Sendable,\n    Equatable {\n}\nfinal class Store<State>: ObservableObject where State: Equatable {\n}\nenum Plain {\n}\n";
        let lines = facts(text);
        assert_eq!(
            head_supertypes(&lines, &node(NodeKind::Struct, "HomeView", 1, 3)),
            ["View", "Sendable", "Equatable"]
        );
        assert_eq!(
            head_supertypes(&lines, &node(NodeKind::Class, "Store", 4, 5)),
            ["ObservableObject"]
        );
        assert!(head_supertypes(&lines, &node(NodeKind::Enum, "Plain", 6, 7)).is_empty());
    }

    #[test]
    fn a_chain_qualifier_is_the_longest_run_of_segments_before_the_name() {
        assert_eq!(
            chain_qualifier("API.PackageController.Model(name: \"x\")", "Model"),
            Some("API.PackageController.")
        );
        assert_eq!(
            chain_qualifier("Future<Int>.Isolated?", "Isolated"),
            Some("Future<Int>.")
        );
        assert_eq!(
            chain_qualifier("A.ModelX.Model", "Model"),
            Some("A.ModelX.")
        );
        assert_eq!(chain_qualifier("Model(", "Model"), None);
        assert_eq!(chain_qualifier("A.Modelled", "Model"), None);
    }

    #[test]
    fn written_paths_and_shared_directories() {
        assert_eq!(
            written_path("API.PackageController.Model", "Model").as_deref(),
            Some("API::PackageController::Model")
        );
        assert_eq!(written_path("Build.Id", "Other"), None);
        assert_eq!(shared_dirs("Sources/App/A.swift", "Sources/App/B.swift"), 2);
        assert_eq!(shared_dirs("Sources/App/A.swift", "Tests/B.swift"), 0);
        assert_eq!(shared_dirs("A.swift", "B.swift"), 0);
    }

    #[test]
    fn a_parent_path_is_a_scope_never_a_file() {
        assert_eq!(
            parent_path(&node(NodeKind::Enum, "ListViewModel::State", 1, 1)),
            Some("ListViewModel")
        );
        assert_eq!(parent_path(&node(NodeKind::Struct, "Account", 1, 1)), None);
        assert_eq!(
            parent_path(&node(
                NodeKind::Struct,
                "Sources/App/Home.swift::Home",
                1,
                1
            )),
            None
        );
    }

    #[test]
    fn head_rows_ignore_positions_and_round_trip() {
        let text = "extension View {\n}\nstruct HomeView: View {\n}\n";
        let nodes = [
            node(NodeKind::Class, "View", 1, 2),
            node(NodeKind::Struct, "HomeView", 3, 4),
        ];
        let row = HeadRow::of(text, &nodes);
        assert_eq!(
            row.entries,
            [
                HeadEntry {
                    qualified_name: "HomeView".to_string(),
                    kind: "struct".to_string(),
                    extension: false,
                    supertypes: vec!["View".to_string()],
                    inherits: vec!["View".to_string()],
                    written: None,
                    reducer: false,
                },
                HeadEntry {
                    qualified_name: "View".to_string(),
                    kind: "class".to_string(),
                    extension: true,
                    supertypes: Vec::new(),
                    inherits: Vec::new(),
                    written: None,
                    reducer: false,
                },
            ]
        );
        let shifted = "\n\nextension View {\n}\nstruct HomeView: View {\n}\n";
        let moved = [
            node(NodeKind::Class, "View", 3, 4),
            node(NodeKind::Struct, "HomeView", 5, 6),
        ];
        assert_eq!(HeadRow::of(shifted, &moved), row);
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
    }
}
