//! The C/C++ namespace rows of a project's files (W4-CPP-07,
//! [`codegraph_resolve::cpp_namespaces`]).
//!
//! A qualified C/C++ name reads the project's namespace aliases and namespace
//! macros from every file's text, and a declaration's macro prefix from the
//! lines of its file a macro opens. None of that is a node, and the text is
//! gone once a file changes on disk, so every full index and migration
//! writes a row per file with what it contributes ([`NamespaceRow`]): its
//! aliases and macro definitions as written, and the lines that use an
//! opener or closer the project defines.
//!
//! A sync compares each changed file's row with its new text. Seeds: the
//! aliases and macros it added, removed or changed (with the names their
//! bodies write) and the macros whose uses it changed (with the namespaces
//! they open), closed over every alias and macro whose body names a seed;
//! then the declarations whose effective name (macro prefix plus qualified
//! name) moved, in the changed files that use an opener or closer and in the
//! unchanged files that use a seed macro, by their bare names (a call from
//! the global scope, `::pipe()`, names nothing else). The C/C++ name lane
//! ([`crate::cpp_dependents`]) re-resolves every unchanged file mentioning a
//! seed. The rows are written last, with the module-surface rows; an index
//! without the C/C++ marker at version 2 rebuilds them all.
//!
//! When an alias or a macro is defined in several files, the first by path
//! wins, and within a file its first definition.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_resolve::cpp_namespaces::{
    self, CppNamespaceFacts, MACRO_NAMESPACED_KINDS, NamespaceMacros, NamespaceRow, ROW_KEY_PREFIX,
    UsageLine, macro_prefix_in,
};
use codegraph_store::Store;

use crate::cpp_dependents::identifiers;

/// Whether `path`, of `language`, can hold anything a namespace row records:
/// a C/C++ file's macro uses, or the aliases and macros its extension says
/// the tables read.
pub fn is_row_file(path: &str, language: Language) -> bool {
    matches!(language, Language::C | Language::Cpp)
        || cpp_namespaces::is_alias_file(path)
        || cpp_namespaces::is_macro_file(path)
}

/// What a file's text holds for its row, before the project's macros say
/// which of its own-line macro uses open or close a namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamespaceScan {
    pub facts: CppNamespaceFacts,
    /// The lines that may use an opener or closer (C and C++ files only).
    pub candidates: Vec<UsageLine>,
}

impl NamespaceScan {
    fn row(&self, macros: &NamespaceMacros) -> NamespaceRow {
        NamespaceRow {
            facts: self.facts.clone(),
            usage: macros.known_usage(&self.candidates),
        }
    }
}

/// `source`'s scan, `None` when it holds nothing a row could record.
pub fn scan_of(relative: &str, language: Language, source: &SourceText) -> Option<NamespaceScan> {
    match source {
        SourceText::Text(text) => scan_of_text(relative, language, text),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn scan_of_text(relative: &str, language: Language, text: &str) -> Option<NamespaceScan> {
    if !is_row_file(relative, language) {
        return None;
    }
    let scan = NamespaceScan {
        facts: cpp_namespaces::facts_of(relative, text),
        candidates: if matches!(language, Language::C | Language::Cpp) {
            cpp_namespaces::candidate_usage_lines(text.lines())
        } else {
            Vec::new()
        },
    };
    (!scan.facts.is_empty() || !scan.candidates.is_empty()).then_some(scan)
}

/// Write the rows of a full index's `scans` (any order). Call once every
/// file is parsed.
pub fn write_index_rows(store: &Store, scans: &mut [(String, NamespaceScan)]) -> Result<()> {
    scans.sort_by(|a, b| a.0.cmp(&b.0));
    let macros = NamespaceMacros::from_facts(scans.iter().map(|(_, scan)| &scan.facts));
    for (path, scan) in scans.iter() {
        let row = scan.row(&macros);
        if !row.is_empty() {
            store.set_project_metadata(&cpp_namespaces::row_key(path), &row.encode())?;
        }
    }
    Ok(())
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    /// Its C/C++ declarations before the change, when its row used an
    /// opener or closer.
    old_nodes: Vec<Node>,
    /// Its scan after the change; `None` once removed or empty.
    new: Option<NamespaceScan>,
}

/// What a sync's changed files held for their namespace rows.
#[derive(Debug, Default)]
pub(crate) struct NamespaceChanges {
    changes: BTreeMap<String, Change>,
}

impl NamespaceChanges {
    /// Before `path` changes: keep the declarations its row's macro uses
    /// gave a prefix.
    pub(crate) fn before_change(&mut self, store: &Store, path: &str) -> Result<()> {
        let uses_macros = read_row(store, path)?.is_some_and(|row| !row.usage.is_empty());
        let change = self.changes.entry(path.to_string()).or_default();
        if uses_macros {
            change.old_nodes = store
                .nodes_by_file_path(path)?
                .into_iter()
                .filter(is_prefixed_kind)
                .collect();
        }
        Ok(())
    }

    /// After `path` changed (`scan` of its new text) or left the index
    /// (`None`).
    pub(crate) fn after_change(&mut self, path: &str, scan: Option<NamespaceScan>) {
        self.changes.entry(path.to_string()).or_default().new = scan;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

fn is_prefixed_kind(node: &Node) -> bool {
    matches!(node.language, Language::C | Language::Cpp)
        && MACRO_NAMESPACED_KINDS.contains(&node.kind)
}

fn read_row(store: &Store, path: &str) -> Result<Option<NamespaceRow>> {
    Ok(store
        .get_project_metadata(&cpp_namespaces::row_key(path))?
        .and_then(|value| NamespaceRow::decode(&value)))
}

/// What a sync's namespace rows call for.
#[derive(Debug, Default)]
pub(crate) struct NamespaceRefresh {
    /// Seeds for the C/C++ name lane.
    pub(crate) names: HashSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<NamespaceRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl NamespaceRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = cpp_namespaces::row_key(path);
            match row {
                Some(row) if !row.is_empty() => {
                    store.set_project_metadata(&key, &row.encode())?;
                }
                _ if !self.rebuilt => {
                    store.delete_project_metadata(&key)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// C++ keywords a namespace macro's body writes besides the names it opens.
const BODY_KEYWORDS: [&str; 2] = ["namespace", "inline"];

/// The names `text` writes, keywords aside.
fn body_names(text: &str) -> impl Iterator<Item = String> + '_ {
    identifiers(text)
        .filter(|word| !BODY_KEYWORDS.contains(word))
        .map(str::to_string)
}

/// The key the name lane scans for a declaration named `name`: the name, or
/// for one that is no identifier (`operator<<`, `~Foo`), its first
/// identifier (`operator`, `Foo`), which a call spelling it writes.
fn scan_key(name: &str) -> Option<String> {
    identifiers(name).next().map(str::to_string)
}

/// Declarations' macro prefixes, by `(kind, qualified name)`, with the
/// bare name: each declaration's prefix, sorted.
type Prefixes = BTreeMap<(String, String), (String, Vec<Vec<String>>)>;

/// Each declaration's macro prefixes under `frames`.
fn prefixes(nodes: &[Node], frames: &[cpp_namespaces::Frame]) -> Prefixes {
    let mut out = Prefixes::new();
    for node in nodes.iter().filter(|node| is_prefixed_kind(node)) {
        out.entry((node.kind.as_str().to_string(), node.qualified_name.clone()))
            .or_insert_with(|| (node.name.clone(), Vec::new()))
            .1
            .push(macro_prefix_in(frames, node.start_line));
    }
    for (_, list) in out.values_mut() {
        list.sort();
    }
    out
}

/// The scan keys of the declarations whose macro prefixes differ between
/// `old` and `new`. A declaration only one side has came or went, which the
/// sync retries by name anyway.
fn moved_names(old: &Prefixes, new: &Prefixes) -> Vec<String> {
    old.iter()
        .filter_map(|(key, (name, before))| {
            let (_, after) = new.get(key)?;
            (after != before).then(|| scan_key(name)).flatten()
        })
        .collect()
}

/// The multiset of `(token, argument)` uses, for a use that moved lines but
/// not what it opens.
fn uses(usage: &[UsageLine]) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = usage
        .iter()
        .map(|line| (line.token.clone(), line.arg.clone()))
        .collect();
    out.sort();
    out
}

/// The items of sorted `a` and `b` one holds more of.
fn differing<T: Ord + Clone>(a: &[T], b: &[T]) -> Vec<T> {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() || j < b.len() {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x == y => {
                i += 1;
                j += 1;
            }
            (Some(x), Some(y)) if x < y => {
                out.push(x.clone());
                i += 1;
            }
            (Some(_), Some(y)) => {
                out.push(y.clone());
                j += 1;
            }
            (Some(x), None) => {
                out.push(x.clone());
                i += 1;
            }
            (None, Some(y)) => {
                out.push(y.clone());
                j += 1;
            }
            (None, None) => break,
        }
    }
    out
}

fn read_text(project_root: &Path, path: &str, max_file_size: u64) -> Option<String> {
    match read_source_file(&project_root.join(path), path, max_file_size) {
        Ok((_, SourceText::Text(text))) => Some(text),
        _ => None,
    }
}

/// Read what the namespace rows call for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and no
/// name is seeded: every C/C++ file re-resolves anyway.
pub(crate) fn namespace_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &NamespaceChanges,
    reindexed: &HashSet<String>,
    bootstrap: bool,
) -> Result<NamespaceRefresh> {
    let mut refresh = NamespaceRefresh::default();
    if bootstrap {
        refresh.rebuilt = true;
        let mut scans: Vec<(String, NamespaceScan)> = Vec::new();
        for file in store.all_files()? {
            if !is_row_file(&file.path, file.language) {
                continue;
            }
            let scan = match changes.changes.get(&file.path) {
                Some(change) => change.new.clone(),
                None => read_text(project_root, &file.path, max_file_size)
                    .and_then(|text| scan_of_text(&file.path, file.language, &text)),
            };
            if let Some(scan) = scan {
                scans.push((file.path, scan));
            }
        }
        let macros = NamespaceMacros::from_facts(scans.iter().map(|(_, scan)| &scan.facts));
        for (path, scan) in scans {
            refresh.rows.insert(path, Some(scan.row(&macros)));
        }
        return Ok(refresh);
    }
    if changes.is_empty() {
        return Ok(refresh);
    }

    // The rows before the change, and the facts after it.
    let mut old_rows: BTreeMap<String, NamespaceRow> = BTreeMap::new();
    for (key, value) in store.project_metadata_with_prefix(ROW_KEY_PREFIX)? {
        if let Some(row) = NamespaceRow::decode(&value) {
            old_rows.insert(key[ROW_KEY_PREFIX.len()..].to_string(), row);
        }
    }
    let empty = NamespaceRow::default();
    let mut new_facts: BTreeMap<&str, &CppNamespaceFacts> = old_rows
        .iter()
        .filter(|(path, _)| !changes.changes.contains_key(*path))
        .map(|(path, row)| (path.as_str(), &row.facts))
        .collect();
    for (path, change) in &changes.changes {
        if let Some(scan) = &change.new {
            new_facts.insert(path, &scan.facts);
        }
    }
    let old_macros = NamespaceMacros::from_facts(old_rows.values().map(|row| &row.facts));
    let new_macros = NamespaceMacros::from_facts(new_facts.values().copied());

    // Seeds: what the changed files define and use.
    let mut seeds: HashSet<String> = HashSet::new();
    let mut macro_names: HashSet<String> = HashSet::new();
    for row in old_rows.values() {
        macro_names.extend(row.facts.macros.iter().map(|fact| fact.name().to_string()));
    }
    for facts in new_facts.values() {
        macro_names.extend(facts.macros.iter().map(|fact| fact.name().to_string()));
    }
    let mut new_rows: BTreeMap<&str, NamespaceRow> = BTreeMap::new();
    for (path, change) in &changes.changes {
        let old = old_rows.get(path).unwrap_or(&empty);
        let new = change
            .new
            .as_ref()
            .map(|scan| scan.row(&new_macros))
            .unwrap_or_default();
        for (alias, _) in differing(&old.facts.aliases, &new.facts.aliases) {
            seeds.insert(alias);
        }
        for fact in differing(&old.facts.macros, &new.facts.macros) {
            seeds.insert(fact.name().to_string());
            seeds.extend(body_names(fact.body()));
        }
        for (token, arg) in differing(&uses(&old.usage), &uses(&new.usage)) {
            for macros in [&old_macros, &new_macros] {
                if let Some(path) = macros.opened_path(&token, arg.as_deref()) {
                    seeds.extend(path);
                }
            }
            seeds.insert(token);
        }
        new_rows.insert(path, new);
    }

    // Closed over every alias and macro whose body names a seed.
    let mut bodies: Vec<(&str, &str)> = Vec::new();
    for facts in old_rows
        .values()
        .map(|row| &row.facts)
        .chain(new_facts.values().copied())
    {
        bodies.extend(
            facts
                .aliases
                .iter()
                .map(|(alias, target)| (alias.as_str(), target.as_str())),
        );
        bodies.extend(facts.macros.iter().map(|fact| (fact.name(), fact.body())));
    }
    loop {
        let before = seeds.len();
        for (name, body) in &bodies {
            if !seeds.contains(*name) && body_names(body).any(|word| seeds.contains(&word)) {
                seeds.insert((*name).to_string());
                seeds.extend(body_names(body));
            }
        }
        if seeds.len() == before {
            break;
        }
    }

    // The declarations whose effective name moved: in the changed files
    // that use an opener or closer, before and after.
    let mut names: HashSet<String> = seeds.clone();
    for (path, change) in &changes.changes {
        let old = old_rows.get(path).unwrap_or(&empty);
        let new = &new_rows[path.as_str()];
        if old.usage.is_empty() && new.usage.is_empty() {
            continue;
        }
        let new_nodes: Vec<Node> = if change.new.is_some() {
            store.nodes_by_file_path(path)?
        } else {
            Vec::new()
        };
        let before = prefixes(&change.old_nodes, &old_macros.frames_of(&old.usage));
        let after = prefixes(&new_nodes, &new_macros.frames_of(&new.usage));
        names.extend(moved_names(&before, &after));
    }
    for (path, row) in new_rows {
        let changed = old_rows.get(path) != Some(&row);
        if changed {
            refresh
                .rows
                .insert(path.to_string(), (!row.is_empty()).then_some(row));
        }
    }
    // And in the unchanged files that use a seed macro: their uses are read
    // under the new macros, and their rows rewritten when that moved them.
    let seed_macros: HashSet<String> = seeds
        .iter()
        .filter(|name| macro_names.contains(*name))
        .cloned()
        .collect();
    if !seed_macros.is_empty() {
        for file in store.all_files()? {
            if !matches!(file.language, Language::C | Language::Cpp)
                || changes.changes.contains_key(&file.path)
                || reindexed.contains(&file.path)
            {
                continue;
            }
            let old = old_rows.get(&file.path).unwrap_or(&empty);
            let Some(text) = read_text(project_root, &file.path, max_file_size) else {
                continue;
            };
            if !identifiers(&text).any(|word| seed_macros.contains(word)) {
                continue;
            }
            let new = scan_of_text(&file.path, file.language, &text)
                .map(|scan| scan.row(&new_macros))
                .unwrap_or_default();
            let nodes = store.nodes_by_file_path(&file.path)?;
            let before = prefixes(&nodes, &old_macros.frames_of(&old.usage));
            let after = prefixes(&nodes, &new_macros.frames_of(&new.usage));
            names.extend(moved_names(&before, &after));
            if &new != old {
                refresh
                    .rows
                    .insert(file.path.clone(), (!new.is_empty()).then_some(new));
            }
        }
    }
    refresh.names = names;
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_resolve::cpp_namespaces::MacroFact;

    #[test]
    fn a_non_identifier_name_is_scanned_by_its_first_identifier() {
        assert_eq!(scan_key("pipe").as_deref(), Some("pipe"));
        assert_eq!(scan_key("operator<<").as_deref(), Some("operator"));
        assert_eq!(scan_key("operator delete").as_deref(), Some("operator"));
        assert_eq!(scan_key("~Foo").as_deref(), Some("Foo"));
    }

    #[test]
    fn a_body_names_the_namespaces_it_opens_not_its_keywords() {
        assert_eq!(
            body_names("namespace fmt { inline namespace v12 {").collect::<Vec<_>>(),
            ["fmt", "v12"]
        );
    }

    #[test]
    fn moved_lines_are_no_changed_use() {
        let at = |line: i64| UsageLine {
            line,
            token: "BEGIN".to_string(),
            arg: None,
        };
        assert!(differing(&uses(&[at(3)]), &uses(&[at(9)])).is_empty());
        assert_eq!(differing(&uses(&[at(3)]), &uses(&[])).len(), 1);
    }

    #[test]
    fn a_declaration_whose_prefix_moved_is_a_seed_and_one_that_came_is_not() {
        let node = |name: &str, line: i64| Node {
            id: format!("function:{name}{line}"),
            kind: codegraph_core::types::NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "a.h".to_string(),
            language: Language::Cpp,
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 0,
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
        };
        let frames = vec![cpp_namespaces::Frame {
            start: 1,
            end: 5,
            path: vec!["lib".to_string()],
        }];
        let before = prefixes(&[node("pipe", 3), node("stays", 2)], &frames);
        let after = prefixes(
            &[node("pipe", 7), node("stays", 2), node("added", 4)],
            &frames,
        );
        assert_eq!(moved_names(&before, &after), ["pipe"]);
    }

    #[test]
    fn a_fact_change_differs_by_the_fact() {
        let old = [MacroFact::Alias {
            name: "NS".to_string(),
            target: "one".to_string(),
        }];
        let new = [MacroFact::Alias {
            name: "NS".to_string(),
            target: "two".to_string(),
        }];
        assert_eq!(differing(&old, &new).len(), 2);
    }
}
