//! What a bare CFML call reads of other files (W4-CF-01,
//! [`codegraph_resolve::cfml_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare call in a component (`.cfc`) reaches the methods of the component
//! files of its `extends` chain: each component's `extends=` read from its
//! text, the written path matched against the `.cfc` files declaring a class
//! of its last segment's name (upstream #2148). The text is gone once a file
//! changes on disk, so every full index and migration writes a row per `.cfc`
//! file with the classes it declares and what it extends ([`HeadRow`]). A
//! sync compares a changed file's row with its new text: the lowercased class
//! names of a file whose row changed (it came, went, declares other classes
//! or extends something else) seed a closure over every component extending
//! a reached name, and the unchanged files of the closure re-resolve whole.
//!
//! A call on a typed CFML receiver ([`codegraph_resolve::cfml_scope`]'s
//! `typed_receiver_call`) stays unresolved while components of its type's
//! name in several files declare the method and nothing written tells them
//! apart, until a component or method of that name comes or goes. The name
//! lane retries a reference written with a receiver only up to a per-tail
//! ceiling, so the unchanged CFML files holding such references to a method
//! name a changed component gained or lost re-resolve whole instead, however
//! many there are.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every CFML file once (a template's typed
//! call waits on components too) and rebuilds the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::cfml_scope::{
    HEAD_ROW_KEY_PREFIX, HeadRow, extended_name, head_row_key, is_component_file,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a CFML dependent unrefreshed.
pub const VERSION_KEY: &str = "cfml_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s component row, `None` when it records nothing: a file that is
/// no `.cfc` file, declares no class and extends nothing, or is not text.
pub fn row_of(
    path: &str,
    language: Language,
    source: &SourceText,
    nodes: &[Node],
) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(path, language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(path: &str, language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Cfml || !is_component_file(path) {
        return None;
    }
    let row = HeadRow::of(text, nodes);
    (!row.is_empty()).then_some(row)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, HeadRow)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&head_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every `.cfc` file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every CFML dependent is current: a full index, a migration,
/// or a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

fn read_row(store: &Store, path: &str) -> Result<Option<HeadRow>> {
    Ok(store
        .get_project_metadata(&head_row_key(path))?
        .and_then(|value| HeadRow::decode(&value)))
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<HeadRow>,
    new_row: Option<HeadRow>,
    /// The names of the CFML methods the file declared before and after.
    old_methods: BTreeSet<String>,
    new_methods: BTreeSet<String>,
}

/// The names of the CFML methods among `nodes`.
fn method_names(nodes: &[Node]) -> BTreeSet<String> {
    nodes
        .iter()
        .filter(|node| node.language == Language::Cfml && node.kind == NodeKind::Method)
        .map(|node| node.name.clone())
        .collect()
}

/// What a sync's changed files held for the CFML lane.
#[derive(Debug, Default)]
pub(crate) struct CfmlChanges {
    changes: BTreeMap<String, Change>,
}

impl CfmlChanges {
    /// `path` changed (its old row still in the store, its old nodes
    /// `old_nodes`) to `new` (its language, text and nodes), or left the
    /// index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        old_nodes: &[Node],
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let old_methods = method_names(old_nodes);
        let (new_row, new_methods) = match new {
            Some((language, source, nodes)) => {
                (row_of(path, language, source, nodes), method_names(nodes))
            }
            None => (None, BTreeSet::new()),
        };
        if old_row.is_none()
            && new_row.is_none()
            && old_methods.is_empty()
            && new_methods.is_empty()
            && !self.changes.contains_key(path)
        {
            return Ok(());
        }
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = self
            .changes
            .entry(path.to_string())
            .or_insert_with(|| Change {
                old_row,
                old_methods,
                ..Change::default()
            });
        change.new_row = new_row;
        change.new_methods = new_methods;
        Ok(())
    }
}

/// What a sync's CFML lane calls for.
#[derive(Debug, Default)]
pub(crate) struct CfmlRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl CfmlRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = head_row_key(path);
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

fn read_text(project_root: &Path, path: &str, max_file_size: u64) -> Option<String> {
    match read_source_file(&project_root.join(path), path, max_file_size) {
        Ok((_, SourceText::Text(text))) => Some(text),
        _ => None,
    }
}

/// The method names a parked typed call could now resolve by: every method
/// of a file whose components came, went or were renamed, and the methods
/// that came or went in the others.
fn typed_call_names(changes: &CfmlChanges) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for change in changes.changes.values() {
        let components = |row: &Option<HeadRow>| {
            row.as_ref()
                .map(|row| row.components.clone())
                .unwrap_or_default()
        };
        if components(&change.old_row) != components(&change.new_row) {
            names.extend(change.old_methods.iter().cloned());
            names.extend(change.new_methods.iter().cloned());
        } else {
            names.extend(
                change
                    .old_methods
                    .symmetric_difference(&change.new_methods)
                    .cloned(),
            );
        }
    }
    names
}

/// The lowercased class names of every file whose row a sync changed: what a
/// written `extends` path could resolve to differently now.
fn seeds(changes: &CfmlChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        if change.old_row == change.new_row {
            continue;
        }
        for row in [&change.old_row, &change.new_row].into_iter().flatten() {
            seeds.extend(row.components.iter().cloned());
        }
    }
    seeds
}

/// The files (of `rows`, by path) whose `extends` names a reached class,
/// closing over the classes those files declare, from `seeds`.
fn closure<'r>(
    rows: impl IntoIterator<Item = (&'r str, &'r HeadRow)>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut extending: HashMap<String, Vec<(&'r str, &'r HeadRow)>> = HashMap::new();
    for (path, row) in rows {
        if let Some(name) = row.extends.as_deref().and_then(extended_name) {
            extending.entry(name).or_default().push((path, row));
        }
    }
    let mut reached = seeds.clone();
    let mut queue: Vec<String> = seeds.into_iter().collect();
    let mut files = BTreeSet::new();
    while let Some(name) = queue.pop() {
        for (path, row) in extending.get(&name).into_iter().flatten() {
            files.insert((*path).to_string());
            for component in &row.components {
                if reached.insert(component.clone()) {
                    queue.push(component.clone());
                }
            }
        }
    }
    files
}

/// Read what the CFML lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// CFML file re-resolves.
pub(crate) fn cfml_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &CfmlChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<CfmlRefresh> {
    let mut refresh = CfmlRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    let names: Vec<String> = typed_call_names(changes).into_iter().collect();
    if !names.is_empty() {
        for reference in store.unresolved_refs_by_name_tails(&names, usize::MAX)? {
            if reference.language == Language::Cfml && unchanged(&reference.file_path) {
                refresh.files.insert(reference.file_path);
            }
        }
    }
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Cfml {
                continue;
            }
            if unchanged(&file.path) {
                refresh.files.insert(file.path.clone());
            }
            if !is_component_file(&file.path) {
                continue;
            }
            let row = match changes.changes.get(&file.path) {
                Some(change) => change.new_row.clone(),
                None => read_text(project_root, &file.path, max_file_size).and_then(|text| {
                    store
                        .nodes_by_file_path(&file.path)
                        .ok()
                        .and_then(|nodes| row_of_text(&file.path, file.language, &text, &nodes))
                }),
            };
            refresh.rows.insert(file.path, row);
        }
        return Ok(refresh);
    }
    for (path, change) in &changes.changes {
        if change.old_row.is_some() || change.new_row.is_some() {
            refresh.rows.insert(path.clone(), change.new_row.clone());
        }
    }
    let seeds = seeds(changes);
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let stored: Vec<(String, HeadRow)> = store
        .project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            let path = key.strip_prefix(HEAD_ROW_KEY_PREFIX)?.to_string();
            if changes.changes.contains_key(&path) {
                return None;
            }
            HeadRow::decode(&value).map(|row| (path, row))
        })
        .collect();
    let rows = stored.iter().map(|(path, row)| (path.as_str(), row)).chain(
        changes
            .changes
            .iter()
            .filter_map(|(path, change)| change.new_row.as_ref().map(|row| (path.as_str(), row))),
    );
    for file in closure(rows, seeds) {
        if unchanged(&file) {
            refresh.files.insert(file);
        }
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(components: &[&str], extends: Option<&str>) -> HeadRow {
        HeadRow {
            components: components.iter().map(|c| c.to_string()).collect(),
            extends: extends.map(str::to_string),
        }
    }

    fn change(old: Option<HeadRow>, new: Option<HeadRow>) -> CfmlChanges {
        let mut changes = CfmlChanges::default();
        changes.changes.insert(
            "system/Base.cfc".to_string(),
            Change {
                old_row: old,
                new_row: new,
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn a_changed_row_seeds_its_classes_and_an_unchanged_one_nothing() {
        let extended = change(
            Some(row(&["base"], None)),
            Some(row(&["base"], Some("Root"))),
        );
        assert_eq!(seeds(&extended), BTreeSet::from(["base".to_string()]));
        let same = change(Some(row(&["base"], None)), Some(row(&["base"], None)));
        assert!(seeds(&same).is_empty());
        let added = change(None, Some(row(&["base"], None)));
        assert_eq!(seeds(&added), BTreeSet::from(["base".to_string()]));
    }

    #[test]
    fn the_closure_follows_extends_paths_by_their_last_segment() {
        let rows = [
            (
                "system/Handler.cfc",
                row(&["handler"], Some("app.system.Base")),
            ),
            ("system/Leaf.cfc", row(&["leaf"], Some("Handler"))),
            ("system/Other.cfc", row(&["other"], Some("Unrelated"))),
        ];
        let files = closure(
            rows.iter().map(|(path, row)| (*path, row)),
            BTreeSet::from(["base".to_string()]),
        );
        assert_eq!(
            files,
            BTreeSet::from([
                "system/Handler.cfc".to_string(),
                "system/Leaf.cfc".to_string()
            ])
        );
    }

    #[test]
    fn a_component_that_comes_or_goes_touches_its_methods_and_an_edit_those_it_changed() {
        let methods = |names: &[&str]| -> BTreeSet<String> {
            names.iter().map(|name| name.to_string()).collect()
        };
        let mut changes = CfmlChanges::default();
        changes.changes.insert(
            "b/UserService.cfc".to_string(),
            Change {
                old_row: Some(row(&["userservice"], None)),
                new_row: None,
                old_methods: methods(&["init", "save"]),
                new_methods: BTreeSet::new(),
            },
        );
        changes.changes.insert(
            "a/OrderService.cfc".to_string(),
            Change {
                old_row: Some(row(&["orderservice"], None)),
                new_row: Some(row(&["orderservice"], None)),
                old_methods: methods(&["init", "load"]),
                new_methods: methods(&["init", "list"]),
            },
        );
        assert_eq!(
            typed_call_names(&changes),
            methods(&["init", "list", "load", "save"])
        );
        let mut same = CfmlChanges::default();
        same.changes.insert(
            "a/OrderService.cfc".to_string(),
            Change {
                old_row: Some(row(&["orderservice"], None)),
                new_row: Some(row(&["orderservice"], None)),
                old_methods: methods(&["init"]),
                new_methods: methods(&["init"]),
            },
        );
        assert!(typed_call_names(&same).is_empty());
    }

    #[test]
    fn only_cfml_component_files_have_rows() {
        let text = SourceText::Text("component extends=\"Base\" {}\n".to_string());
        assert!(row_of("views/main.cfm", Language::Cfml, &text, &[]).is_none());
        assert!(row_of("lib/a.rb", Language::Ruby, &text, &[]).is_none());
        assert_eq!(
            row_of("system/Handler.cfc", Language::Cfml, &text, &[]),
            Some(row(&[], Some("Base")))
        );
    }
}
