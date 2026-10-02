//! Dead code — one derivation of "nothing in this repository reaches here",
//! a port of upstream `src/graph/dead-code.ts` (`buildDeadCodeReport`, `v1.6.1`).
//!
//! `unreferenced` is a fact: no edge in the index other than the `contains`
//! edge from whatever holds it points at the symbol. `dead` is an inference on
//! top of that fact, and every step of it is subtractive — a candidate leaves
//! the list the moment there is any reason to believe something outside the
//! graph reaches it: exported or declared in a header; in a test, generated or
//! vendored file; abstract or declared on an interface; decorated; overriding
//! an ancestor's member (or an ancestor we cannot read); named something the
//! language calls by itself; in an in-file test scope; in a component file
//! whose markup can reference it invisibly; in a file nothing reaches; sharing
//! a name the resolver failed to follow, or a name another referenced symbol
//! carries; in a language the index records no export marker for; or written
//! more than once in a file that can reach it.
//!
//! Every subtraction is counted in [`DeadCodeExclusions`]: that is the sentence
//! under the list, because eight rows drawn from four thousand candidates mean
//! something different from eight drawn from nine. Query-time and read-only.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use codegraph_store::Store;

use crate::collation::locale_compare;
use crate::query::scoring::is_test_file;

/// Kinds asked about by default: callables and types. Values are out on
/// purpose — their `references` coverage is the most language-dependent thing
/// in the resolver.
pub const DEAD_CODE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Class,
    NodeKind::Component,
    NodeKind::Interface,
    NodeKind::Struct,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
    NodeKind::Union,
    NodeKind::TypeAlias,
];

/// Kinds a `kinds=` request may ask for.
pub fn is_allowed_dead_code_kind(kind: NodeKind) -> bool {
    DEAD_CODE_KINDS.contains(&kind)
        || matches!(
            kind,
            NodeKind::Variable
                | NodeKind::Constant
                | NodeKind::Property
                | NodeKind::Field
                | NodeKind::EnumMember
                | NodeKind::Namespace
                | NodeKind::Module
        )
}

/// Candidates pulled out of SQL before any exclusion runs.
pub const MAX_DEAD_CODE_CANDIDATES: i64 = 20_000;
/// Levels walked up looking for an ancestor that declares the same member.
pub const MAX_OVERRIDE_ANCESTOR_DEPTH: usize = 8;
/// Files read for the corroboration pass, and the biggest one read. A file
/// skipped for either reason counts as NOT corroborated, which drops the row.
pub const MAX_CORROBORATION_FILES: usize = 600;
pub const MAX_CORROBORATION_BYTES: u64 = 2_000_000;

fn is_container_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::Union
            | NodeKind::TypeAlias
    )
}

/// Container kinds whose members are declarations, never call targets.
fn is_declaration_container_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Interface | NodeKind::Trait | NodeKind::Protocol
    )
}

/// Member kinds an override can be declared on.
fn is_overridable_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Property | NodeKind::Field
    )
}

/// Names the language or runtime itself calls.
const IMPLICIT_ENTRY_NAMES: &[&str] = &[
    "constructor",
    "main",
    "init",
    "deinit",
    "finalize",
    "destructor",
    "dispose",
    "drop",
    "default",
    "tostring",
    "equals",
    "gethashcode",
    "hashcode",
];

/// Qualified-name segments that mean "inside a test scope the path does not
/// reveal" — a Rust `#[cfg(test)] mod tests`, a nested `Tests` class.
const TEST_SCOPE_SEGMENTS: &[&str] = &["test", "tests", "__tests__", "spec", "specs", "testing"];

/// Languages whose files are markup with a script block inside them: a handler
/// passed by reference in the template is not extracted.
const MARKUP_HOST_LANGUAGES: &[&str] = &[
    "svelte",
    "vue",
    "astro",
    "liquid",
    "html",
    "razor",
    "twig",
    "blade",
    "erb",
    "handlebars",
];

/// Extensions whose contents are declarations for somebody else.
const HEADER_EXTENSIONS: &[&str] = &[
    ".h", ".hh", ".hpp", ".hxx", ".h++", ".inc", ".d.ts", ".d.mts", ".d.cts", ".pyi", ".pxd",
];

/// Directory names that mean "this code is carried, not written here",
/// matched as whole path segments.
const VENDOR_SEGMENTS: &[&str] = &[
    "vendor",
    "vendored",
    "third_party",
    "third-party",
    "thirdparty",
    "external",
    "externals",
    "node_modules",
    "bower_components",
    "site-packages",
    "godeps",
    "pods",
    ".venv",
    "venv",
];

/// One symbol nothing reaches, and what it takes with it.
#[derive(Debug, Clone)]
pub struct DeadCodeEntry {
    pub node: Node,
    /// Unreferenced members folded in: a class nobody instantiates takes its
    /// methods with it.
    pub members: Vec<Node>,
    /// Source lines the entry spans, members included.
    pub lines: i64,
    /// Only ever true when the caller asked for exported symbols.
    pub exported: bool,
}

/// How many candidates each rule removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeadCodeExclusions {
    pub tests: usize,
    pub generated: usize,
    pub exported: usize,
    pub exports_unknown: usize,
    pub declarations: usize,
    pub decorated: usize,
    pub overriding: usize,
    pub implicit: usize,
    pub vendored: usize,
    pub test_scope: usize,
    pub markup: usize,
    pub unreachable_file: usize,
    pub unresolved_name: usize,
    pub ambiguous_name: usize,
    pub mentioned: usize,
    pub unreadable: usize,
    pub nested: usize,
}

impl DeadCodeExclusions {
    /// Every rule with its wire name (upstream's camelCase key), in rule order.
    pub fn entries(&self) -> [(&'static str, usize); 17] {
        [
            ("tests", self.tests),
            ("generated", self.generated),
            ("exported", self.exported),
            ("exportsUnknown", self.exports_unknown),
            ("declarations", self.declarations),
            ("decorated", self.decorated),
            ("overriding", self.overriding),
            ("implicit", self.implicit),
            ("vendored", self.vendored),
            ("testScope", self.test_scope),
            ("markup", self.markup),
            ("unreachableFile", self.unreachable_file),
            ("unresolvedName", self.unresolved_name),
            ("ambiguousName", self.ambiguous_name),
            ("mentioned", self.mentioned),
            ("unreadable", self.unreadable),
            ("nested", self.nested),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct DeadCodeReport {
    /// Ranked, capped.
    pub entries: Vec<DeadCodeEntry>,
    /// Entries before the limit — always the real number.
    pub total: usize,
    /// Symbols with no incoming reference at all, before any exclusion ran.
    pub candidates: usize,
    pub excluded: DeadCodeExclusions,
    /// The kinds actually asked about.
    pub kinds: Vec<NodeKind>,
    pub include_exported: bool,
    /// The candidate scan stopped at [`MAX_DEAD_CODE_CANDIDATES`].
    pub bounded: bool,
    /// Every surviving row was checked against the text of the files that can
    /// reach it. False when no reader was given.
    pub corroborated: bool,
}

/// Reads a project-relative file for the corroboration pass; `None` for
/// anything unreadable.
pub type SourceReader<'a> = &'a dyn Fn(&str) -> Option<String>;

pub struct DeadCodeQuery<'a> {
    pub kinds: Option<Vec<NodeKind>>,
    pub include_exported: bool,
    pub include_tests: bool,
    pub include_generated: bool,
    pub limit: usize,
    /// `None` turns the corroboration pass off, and the report then says so.
    pub read_source: Option<SourceReader<'a>>,
}

impl Default for DeadCodeQuery<'_> {
    fn default() -> Self {
        Self {
            kinds: None,
            include_exported: false,
            include_tests: false,
            include_generated: false,
            limit: 200,
            read_source: None,
        }
    }
}

/// The dead code report: unreferenced symbols, minus every reason to doubt
/// them, plus a count of every doubt.
pub fn build_dead_code_report(
    store: &Store,
    query: &DeadCodeQuery<'_>,
) -> rusqlite::Result<DeadCodeReport> {
    let kinds = normalize_kinds(query.kinds.as_deref());
    let include_exported = query.include_exported;
    let include_tests = query.include_tests;
    let include_generated = query.include_generated;
    let limit = query.limit.max(1);
    let mut excluded = DeadCodeExclusions::default();

    let mut raw = store.unreferenced_nodes(&kinds, MAX_DEAD_CODE_CANDIDATES + 1)?;
    let bounded = raw.len() as i64 > MAX_DEAD_CODE_CANDIDATES;
    raw.truncate(MAX_DEAD_CODE_CANDIDATES as usize);
    let candidates = raw;

    // Whether the exported filter can run at all, per language — skipped when
    // the caller has already accepted outside-reachability.
    let languages_with_exports = if include_exported {
        HashSet::new()
    } else {
        let languages: Vec<String> = candidates
            .iter()
            .map(|(node, _)| node.language.as_str().to_string())
            .collect();
        store.languages_with_exports(&languages)?
    };

    // ---- the cheap, per-row rules ------------------------------------------
    let mut surviving: Vec<&Node> = Vec::new();
    for (node, generated) in &candidates {
        if !include_tests && is_test_file(&node.file_path) {
            excluded.tests += 1;
        } else if !include_generated && *generated {
            excluded.generated += 1;
        } else if !include_exported && (node.is_exported || is_header_file(&node.file_path)) {
            excluded.exported += 1;
        } else if !include_exported && !languages_with_exports.contains(node.language.as_str()) {
            excluded.exports_unknown += 1;
        } else if node.is_abstract {
            excluded.declarations += 1;
        } else if is_implicit_entry_name(&node.name) {
            excluded.implicit += 1;
        } else if is_vendored_path(&node.file_path) {
            excluded.vendored += 1;
        } else if !include_tests && is_test_scope(&node.qualified_name) {
            excluded.test_scope += 1;
        } else if MARKUP_HOST_LANGUAGES.contains(&node.language.as_str()) {
            excluded.markup += 1;
        } else {
            surviving.push(node);
        }
    }

    // ---- the rules that need the graph -------------------------------------
    // A `decorates` edge runs FROM the decorated symbol to the decorator.
    let surviving_ids: Vec<String> = surviving.iter().map(|n| n.id.clone()).collect();
    let decorated: HashSet<String> = store
        .outgoing_edges_from(&surviving_ids, &[EdgeKind::Decorates])?
        .into_iter()
        .map(|e| e.source)
        .collect();
    let containers = containers_of(store, &surviving)?;
    let overriding = override_candidates(store, &surviving, &containers)?;
    // A file nothing reaches is an island: its symbols' zero fan-in describes
    // the file, not the symbol.
    let unreachable_files = files_nothing_reaches(store, &surviving)?;
    surviving.retain(|node| {
        if unreachable_files.contains(&node.file_path) {
            excluded.unreachable_file += 1;
            false
        } else {
            true
        }
    });

    let names: Vec<String> = surviving.iter().map(|n| n.name.clone()).collect();
    let unresolved = store.unresolved_names_among(&names)?;
    let ambiguous = store.ambiguous_referenced_names(&names)?;

    let mut kept: Vec<&Node> = Vec::new();
    for node in surviving {
        if decorated.contains(&node.id) || !node.decorators.is_empty() {
            excluded.decorated += 1;
        } else if containers
            .get(&node.id)
            .is_some_and(|c| is_declaration_container_kind(c.kind))
        {
            excluded.declarations += 1;
        } else if overriding.contains(&node.id) {
            excluded.overriding += 1;
        } else if unresolved.contains(&node.name) {
            excluded.unresolved_name += 1;
        } else if ambiguous.contains(&node.name) {
            excluded.ambiguous_name += 1;
        } else {
            kept.push(node);
        }
    }

    // ---- the file's own text has the last word ------------------------------
    let mut confirmed: Vec<&Node> = Vec::new();
    match query.read_source {
        Some(read_source) => {
            let mut sources: HashMap<String, Option<String>> = HashMap::new();
            let mut read = |file: &str| -> Option<String> {
                if !sources.contains_key(file) {
                    let text = if sources.len() >= MAX_CORROBORATION_FILES {
                        None
                    } else {
                        read_source(file)
                    };
                    sources.insert(file.to_string(), text);
                }
                sources.get(file).cloned().flatten()
            };
            // The declaring file plus everything the index says reaches into
            // it — the same set a call could have come from. Once per file.
            let mut scopes: HashMap<String, Vec<String>> = HashMap::new();
            for node in &kept {
                if !scopes.contains_key(&node.file_path) {
                    let mut scope = vec![node.file_path.clone()];
                    let mut dependents = store.dependent_file_paths(&node.file_path)?;
                    dependents.sort();
                    scope.extend(dependents);
                    scopes.insert(node.file_path.clone(), scope);
                }
            }
            for node in kept {
                let scope = scopes
                    .get(&node.file_path)
                    .cloned()
                    .unwrap_or_else(|| vec![node.file_path.clone()]);
                let mut own_readable = false;
                let mut mentions = 0;
                for file in &scope {
                    let source = read(file);
                    if *file == node.file_path {
                        own_readable = source.is_some();
                    }
                    let Some(source) = source else {
                        continue;
                    };
                    mentions += mention_count(&source, &node.name, 2 - mentions);
                    if mentions >= 2 {
                        break;
                    }
                }
                // The declaration itself is one of the two mentions, so an
                // unreadable declaring file makes the count meaningless.
                if !own_readable {
                    excluded.unreadable += 1;
                } else if mentions >= 2 {
                    excluded.mentioned += 1;
                } else {
                    confirmed.push(node);
                }
            }
        }
        None => confirmed = kept,
    }

    // ---- fold members into a container that is itself dead ------------------
    let kept_ids: HashSet<&str> = confirmed.iter().map(|n| n.id.as_str()).collect();
    let mut order: Vec<String> = Vec::new();
    let mut entries: HashMap<String, DeadCodeEntry> = HashMap::new();
    let mut pending: Vec<(&Node, String)> = Vec::new();
    for node in &confirmed {
        if let Some(container) = containers.get(&node.id)
            && kept_ids.contains(container.id.as_str())
        {
            pending.push((node, container.id.clone()));
            excluded.nested += 1;
            continue;
        }
        order.push(node.id.clone());
        entries.insert(
            node.id.clone(),
            DeadCodeEntry {
                node: (*node).clone(),
                members: Vec::new(),
                lines: (node.end_line - node.start_line + 1).max(1),
                exported: node.is_exported,
            },
        );
    }
    for (member, container_id) in pending {
        // A member whose container was itself folded away has no entry to hang
        // off; it was still counted as nested.
        if let Some(entry) = entries.get_mut(&container_id) {
            entry.members.push(member.clone());
        }
    }
    let mut ranked: Vec<DeadCodeEntry> = order
        .into_iter()
        .filter_map(|id| entries.remove(&id))
        .collect();
    for entry in &mut ranked {
        entry.members.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| locale_compare(&a.name, &b.name))
        });
    }
    // Biggest first; file and line break the tie so the order is stable.
    ranked.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then_with(|| locale_compare(&a.node.file_path, &b.node.file_path))
            .then_with(|| a.node.start_line.cmp(&b.node.start_line))
    });
    let total = ranked.len();
    ranked.truncate(limit);

    Ok(DeadCodeReport {
        entries: ranked,
        total,
        candidates: candidates.len(),
        excluded,
        kinds,
        include_exported,
        bounded,
        corroborated: query.read_source.is_some(),
    })
}

/// A reader for callers with no chokepoint of their own: resolves against the
/// project root, refuses anything that escapes it, and skips files over the
/// corroboration size cap.
pub fn default_source_reader(project_root: &Path) -> impl Fn(&str) -> Option<String> + use<> {
    let root: PathBuf = project_root.to_path_buf();
    move |file_path: &str| {
        let absolute = root.join(file_path);
        let canonical = absolute.canonicalize().ok()?;
        let canonical_root = root.canonicalize().ok()?;
        if !canonical.starts_with(&canonical_root) {
            return None;
        }
        let meta = std::fs::metadata(&canonical).ok()?;
        if !meta.is_file() || meta.len() > MAX_CORROBORATION_BYTES {
            return None;
        }
        std::fs::read_to_string(&canonical).ok()
    }
}

/// How many times `name` is written in `source` as a whole identifier, counting
/// no further than `stop_at`. Deliberately dumb: a mention in a comment or a
/// string is exactly what turns out to be a reflective call, and the count only
/// ever makes the report say LESS.
pub fn mention_count(source: &str, name: &str, stop_at: usize) -> usize {
    if name.is_empty() || stop_at == 0 {
        return 0;
    }
    let mut count = 0;
    let mut from = 0;
    while let Some(offset) = source[from..].find(name) {
        let at = from + offset;
        from = at + name.len();
        let before = source[..at].chars().next_back();
        let after = source[from..].chars().next();
        if !before.is_some_and(is_identifier_char) && !after.is_some_and(is_identifier_char) {
            count += 1;
            if count >= stop_at {
                return count;
            }
        }
    }
    count
}

fn is_identifier_char(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic() || c.is_numeric()
}

/// A file whose contents are declarations for somebody else.
pub fn is_header_file(file_path: &str) -> bool {
    let lower = file_path.to_lowercase();
    HEADER_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// A qualified name that runs through a test scope.
pub fn is_test_scope(qualified_name: &str) -> bool {
    qualified_name
        .split(['.', ':', '/', '\\', '#', '>'])
        .any(|segment| TEST_SCOPE_SEGMENTS.contains(&segment.to_lowercase().as_str()))
}

/// Code the repository carries rather than owns.
pub fn is_vendored_path(file_path: &str) -> bool {
    file_path
        .replace('\\', "/")
        .split('/')
        .any(|segment| VENDOR_SEGMENTS.contains(&segment.to_lowercase().as_str()))
}

/// Python and Ruby call `__enter__`, `__iter__`, `__init__` by protocol.
fn is_dunder(name: &str) -> bool {
    name.len() > 4
        && name.starts_with("__")
        && name.ends_with("__")
        && name[2..name.len() - 2]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A name the language calls by itself, so no source file could name it.
pub fn is_implicit_entry_name(name: &str) -> bool {
    is_dunder(name) || IMPLICIT_ENTRY_NAMES.contains(&name.to_lowercase().as_str())
}

fn normalize_kinds(requested: Option<&[NodeKind]>) -> Vec<NodeKind> {
    let Some(requested) = requested.filter(|r| !r.is_empty()) else {
        return DEAD_CODE_KINDS.to_vec();
    };
    let mut kinds: Vec<NodeKind> = Vec::new();
    for kind in requested {
        if is_allowed_dead_code_kind(*kind) && !kinds.contains(kind) {
            kinds.push(*kind);
        }
    }
    if kinds.is_empty() {
        DEAD_CODE_KINDS.to_vec()
    } else {
        kinds
    }
}

/// The type each member candidate is declared in. Only type-ish containers are
/// returned: a function's container is the file, which tells us nothing.
fn containers_of(store: &Store, nodes: &[&Node]) -> rusqlite::Result<HashMap<String, Node>> {
    let member_ids: Vec<String> = nodes
        .iter()
        .filter(|n| is_overridable_kind(n.kind))
        .map(|n| n.id.clone())
        .collect();
    let mut out = HashMap::new();
    if member_ids.is_empty() {
        return Ok(out);
    }
    let mut by_member: HashMap<String, String> = HashMap::new();
    let mut edges = store.incoming_edges_to(&member_ids, &[EdgeKind::Contains])?;
    // First container in a stable order when a member has several.
    edges.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then_with(|| a.source.cmp(&b.source))
    });
    for edge in edges {
        by_member.entry(edge.target).or_insert(edge.source);
    }
    let container_ids: Vec<String> = by_member.values().cloned().collect();
    let container_nodes = store.nodes_by_ids(&container_ids)?;
    for (member_id, container_id) in by_member {
        if let Some(container) = container_nodes.get(&container_id)
            && is_container_kind(container.kind)
        {
            out.insert(member_id, container.clone());
        }
    }
    Ok(out)
}

/// Which candidates override something — matched by NAME within a chain the
/// graph already links. An ancestor with no readable members, or one outside
/// the index (a failed `extends` / `implements` row, #1973), counts as a match:
/// "cannot tell" is answered with an exclusion.
fn override_candidates(
    store: &Store,
    nodes: &[&Node],
    containers: &HashMap<String, Node>,
) -> rusqlite::Result<HashSet<String>> {
    let mut dropped = HashSet::new();
    let mut container_ids: Vec<String> = containers.values().map(|n| n.id.clone()).collect();
    container_ids.sort();
    container_ids.dedup();
    if container_ids.is_empty() {
        return Ok(dropped);
    }

    // Level-by-level upward walk over EVERY container at once. `reach` maps an
    // ancestor back to the containers it is an ancestor of.
    let mut reach: HashMap<String, HashSet<String>> = HashMap::new();
    let mut seen: HashMap<String, HashSet<String>> = container_ids
        .iter()
        .map(|id| (id.clone(), HashSet::from([id.clone()])))
        .collect();
    let mut frontier: Vec<(String, HashSet<String>)> = container_ids
        .iter()
        .map(|id| (id.clone(), HashSet::from([id.clone()])))
        .collect();
    let mut depth = 0;
    while depth < MAX_OVERRIDE_ANCESTOR_DEPTH && !frontier.is_empty() {
        let roots_of: HashMap<&str, &HashSet<String>> =
            frontier.iter().map(|(id, r)| (id.as_str(), r)).collect();
        let ids: Vec<String> = frontier.iter().map(|(id, _)| id.clone()).collect();
        let mut edges =
            store.outgoing_edges_from(&ids, &[EdgeKind::Extends, EdgeKind::Implements])?;
        edges.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| a.target.cmp(&b.target))
        });
        let mut next: Vec<(String, HashSet<String>)> = Vec::new();
        let mut next_at: HashMap<String, usize> = HashMap::new();
        for edge in edges {
            if edge.target == edge.source {
                continue;
            }
            let Some(roots) = roots_of.get(edge.source.as_str()) else {
                continue;
            };
            let visited = seen.entry(edge.target.clone()).or_default();
            let known = reach.entry(edge.target.clone()).or_default();
            let mut merged: HashSet<String> = HashSet::new();
            for root in roots.iter() {
                if visited.insert(root.clone()) {
                    merged.insert(root.clone());
                    known.insert(root.clone());
                }
            }
            if known.is_empty() {
                reach.remove(&edge.target);
            }
            if merged.is_empty() {
                continue;
            }
            match next_at.get(&edge.target) {
                Some(&at) => next[at].1.extend(merged),
                None => {
                    next_at.insert(edge.target.clone(), next.len());
                    next.push((edge.target.clone(), merged));
                }
            }
        }
        frontier = next;
        depth += 1;
    }

    let mut opaque: HashSet<String> = HashSet::new();
    let mut probe: Vec<String> = container_ids.clone();
    probe.extend(reach.keys().cloned());
    let external_based = store.unresolved_supertype_sources_among(&probe)?;
    for id in &container_ids {
        if external_based.contains(id) {
            opaque.insert(id.clone());
        }
    }
    for (ancestor, roots) in &reach {
        if external_based.contains(ancestor) {
            opaque.extend(roots.iter().cloned());
        }
    }

    // What each ancestor declares, and whether it declares anything at all.
    let ancestor_ids: Vec<String> = reach.keys().cloned().collect();
    let member_edges = store.outgoing_edges_from(&ancestor_ids, &[EdgeKind::Contains])?;
    let mut member_ids_by_ancestor: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in &member_edges {
        member_ids_by_ancestor
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
    }
    let member_targets: Vec<String> = member_edges.iter().map(|e| e.target.clone()).collect();
    let member_nodes = store.nodes_by_ids(&member_targets)?;
    let mut names_by_container: HashMap<String, HashSet<String>> = HashMap::new();
    for (ancestor, roots) in &reach {
        let names: Vec<&str> = member_ids_by_ancestor
            .get(ancestor.as_str())
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| member_nodes.get(*id))
                    .filter(|m| is_overridable_kind(m.kind))
                    .map(|m| m.name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        for root in roots {
            if names.is_empty() {
                opaque.insert(root.clone());
                continue;
            }
            names_by_container
                .entry(root.clone())
                .or_default()
                .extend(names.iter().map(|n| n.to_string()));
        }
    }

    for node in nodes {
        let Some(container) = containers.get(&node.id) else {
            continue;
        };
        if opaque.contains(&container.id)
            || names_by_container
                .get(&container.id)
                .is_some_and(|names| names.contains(&node.name))
        {
            dropped.insert(node.id.clone());
        }
    }
    Ok(dropped)
}

/// Files in the candidate set nothing else in the index reaches.
fn files_nothing_reaches(store: &Store, nodes: &[&Node]) -> rusqlite::Result<HashSet<String>> {
    let mut paths: Vec<String> = nodes.iter().map(|n| n.file_path.clone()).collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let dependents: HashMap<String, i64> =
        store.file_dependent_counts(&paths)?.into_iter().collect();
    Ok(paths
        .into_iter()
        .filter(|p| dependents.get(p).copied().unwrap_or(0) == 0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Upstream `__tests__/dead-code.test.ts` "the rules that are pure".

    #[test]
    fn counts_whole_identifier_mentions_only() {
        assert_eq!(
            mention_count("const load = 1; loader(); reload();", "load", usize::MAX),
            1
        );
        assert_eq!(mention_count("a.load(); load();", "load", usize::MAX), 2);
        assert_eq!(mention_count("nothing here", "load", usize::MAX), 0);
        // Stops early: the caller only ever needs "one, or more than one".
        assert_eq!(mention_count("x x x x x", "x", 2), 2);
    }

    #[test]
    fn matches_vendored_directories_as_whole_segments() {
        assert!(is_vendored_path("vendor/lib/a.go"));
        assert!(is_vendored_path("a/node_modules/b/c.js"));
        assert!(!is_vendored_path("src/vendored-parser.ts"));
    }

    #[test]
    fn recognises_headers_as_declaration_surfaces() {
        assert!(is_header_file("src/tree_sitter/parser.h"));
        assert!(is_header_file("types/global.d.ts"));
        assert!(!is_header_file("src/parser.c"));
    }

    #[test]
    fn recognises_a_test_scope_inside_a_file() {
        assert!(is_test_scope("tests::row_sizes_match"));
        assert!(is_test_scope("Fixtures.Tests.Helper"));
        assert!(!is_test_scope("Latest.value"));
    }

    #[test]
    fn recognises_names_the_language_calls_by_itself() {
        assert!(is_implicit_entry_name("constructor"));
        assert!(is_implicit_entry_name("__enter__"));
        assert!(is_implicit_entry_name("ToString"));
        assert!(!is_implicit_entry_name("mainHandler"));
    }

    #[test]
    fn mentions_count_whole_identifiers_only() {
        let source =
            "fn greet() {}\nlet a = greet;\n// greeting\nregreet(); greet_x; $greet; greet2";
        assert_eq!(mention_count(source, "greet", usize::MAX), 2);
        assert_eq!(mention_count(source, "greet", 1), 1);
        assert_eq!(mention_count("été greetété greet", "greet", usize::MAX), 1);
        assert_eq!(mention_count("", "x", 2), 0);
        assert_eq!(mention_count("x", "", 2), 0);
    }

    #[test]
    fn the_cheap_rules_read_paths_and_names() {
        assert!(is_header_file("include/a.H"));
        assert!(is_header_file("types/index.d.ts"));
        assert!(!is_header_file("src/a.ts"));
        assert!(is_test_scope("crate::store::tests::helper"));
        assert!(is_test_scope("Foo.Tests.Bar"));
        assert!(!is_test_scope("crate::contest::run"));
        assert!(is_vendored_path("third_party/zlib/inflate.c"));
        assert!(is_vendored_path("a\\node_modules\\b.js"));
        assert!(!is_vendored_path("src/vendored-parser.ts"));
        assert!(is_implicit_entry_name("__enter__"));
        assert!(is_implicit_entry_name("ToString"));
        assert!(is_implicit_entry_name("main"));
        assert!(!is_implicit_entry_name("____"));
        assert!(!is_implicit_entry_name("__x"));
        assert!(!is_implicit_entry_name("run"));
    }

    #[test]
    fn requested_kinds_are_filtered_and_deduplicated() {
        assert_eq!(normalize_kinds(None), DEAD_CODE_KINDS.to_vec());
        assert_eq!(
            normalize_kinds(Some(&[
                NodeKind::Variable,
                NodeKind::Variable,
                NodeKind::Import
            ])),
            vec![NodeKind::Variable]
        );
        assert_eq!(
            normalize_kinds(Some(&[NodeKind::Import])),
            DEAD_CODE_KINDS.to_vec()
        );
    }
}
