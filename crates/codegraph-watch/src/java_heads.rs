//! What Java resolution reads of other files (W4-JVM-01,
//! [`codegraph_resolve::java_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare Java call reaches the methods of the supertypes the heads of the
//! classes around it name, and a bare type name the nested types of their
//! ancestors; both walks read the heads from source, by type name, in every
//! file. The text is gone once a file changes on disk, so every full index
//! and migration writes a row per Java file with what its heads say
//! ([`HeadRow`]), and a sync compares a changed file's row with its new text:
//! the names whose entries changed seed a closure over every head naming
//! one, and the unchanged files declaring a type of a closure name, or an
//! anonymous class instantiating one, re-resolve whole.
//!
//! An import from outside the project owns its name, and "outside" is read
//! from the packages the project's Java, Kotlin and Scala files declare. A
//! sync that makes a package appear in the project, or leave it, re-resolves
//! every unchanged Java file importing from under it.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Java file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::java_scope::{HEAD_ROW_KEY_PREFIX, HeadEntry, HeadRow, head_row_key};
use codegraph_store::Store;

/// Metadata key present when no sync left a Java dependent unrefreshed.
pub const VERSION_KEY: &str = "java_type_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// Java file, holds no type whose head names anything, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Java {
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

/// Whether this index needs every Java file re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Java dependent is current: a full index, a migration,
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

fn is_jvm(language: Language) -> bool {
    matches!(
        language,
        Language::Java | Language::Kotlin | Language::Scala
    )
}

/// The packages `nodes` declare: their Java, Kotlin and Scala namespaces.
fn packages_of(nodes: &[Node]) -> BTreeSet<String> {
    nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Namespace && is_jvm(node.language))
        .map(|node| node.qualified_name.clone())
        .collect()
}

fn read_row(store: &Store, path: &str) -> Result<Option<HeadRow>> {
    Ok(store
        .get_project_metadata(&head_row_key(path))?
        .and_then(|value| HeadRow::decode(&value)))
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    /// Its row before the change.
    old_row: Option<HeadRow>,
    /// Its row after the change; `None` once removed or empty.
    new_row: Option<HeadRow>,
    old_packages: BTreeSet<String>,
    new_packages: BTreeSet<String>,
}

/// What a sync's changed files held for the Java lanes.
#[derive(Debug, Default)]
pub(crate) struct JavaChanges {
    changes: BTreeMap<String, Change>,
}

impl JavaChanges {
    /// `path` changed from `old_nodes` (still in the store with its old row)
    /// to `new` (its language, text and nodes), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        old_nodes: &[Node],
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let old_packages = packages_of(old_nodes);
        let (new_row, new_packages) = match new {
            Some((language, source, nodes)) => {
                (row_of(language, source, nodes), packages_of(nodes))
            }
            None => (None, BTreeSet::new()),
        };
        if old_row.is_none()
            && new_row.is_none()
            && old_packages.is_empty()
            && new_packages.is_empty()
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
                old_packages,
                ..Change::default()
            });
        change.new_row = new_row;
        change.new_packages = new_packages;
        Ok(())
    }
}

/// What a sync's Java lanes call for.
#[derive(Debug, Default)]
pub(crate) struct JavaRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl JavaRefresh {
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

/// Each name's entries, in file order.
fn entries_by_name(row: Option<&HeadRow>) -> BTreeMap<&str, Vec<&HeadEntry>> {
    let mut out: BTreeMap<&str, Vec<&HeadEntry>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry(entry.name.as_str()).or_default().push(entry);
    }
    out
}

/// The names whose entries a changed file's rows differ in: an entry came,
/// went or reads other names, or same-named entries changed places.
fn seeds(changes: &JavaChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let old = entries_by_name(change.old_row.as_ref());
        let new = entries_by_name(change.new_row.as_ref());
        for name in old.keys().chain(new.keys()) {
            if old.get(name) != new.get(name) {
                seeds.insert((*name).to_string());
            }
        }
    }
    seeds
}

/// `seeds` and the name of every entry whose head names one of the names
/// already reached, until none is added. The entries are every unchanged
/// file's row (the store still holds the changed files' old rows) and the
/// changed files' new rows.
fn closure(
    store: &Store,
    changes: &JavaChanges,
    seeds: BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let stored: Vec<HeadRow> = store
        .project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(_, value)| HeadRow::decode(&value))
        .collect();
    let mut naming: HashMap<String, BTreeSet<String>> = HashMap::new();
    for entry in stored
        .iter()
        .chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.new_row.as_ref()),
        )
        .flat_map(|row| &row.entries)
    {
        for written in entry.names() {
            naming
                .entry(written.to_string())
                .or_default()
                .insert(entry.name.clone());
        }
    }
    let mut reached = seeds.clone();
    let mut queue: VecDeque<String> = seeds.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        for deriving in naming.get(&name).into_iter().flatten() {
            if reached.insert(deriving.clone()) {
                queue.push_back(deriving.clone());
            }
        }
    }
    Ok(reached)
}

/// The unchanged files declaring a Java type of a `closure` name, or an
/// anonymous class instantiating one.
fn head_dependents(
    store: &Store,
    closure: &BTreeSet<String>,
    unchanged: impl Fn(&str) -> bool,
) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    for name in closure {
        for node in store.nodes_by_name(name)? {
            if node.language == Language::Java
                && matches!(
                    node.kind,
                    NodeKind::Class | NodeKind::Interface | NodeKind::Enum
                )
                && unchanged(&node.file_path)
            {
                files.insert(node.file_path);
            }
        }
        for node in store.nodes_by_name_prefix(&format!("<{name}$anon@"), i64::MAX)? {
            if node.language == Language::Java
                && node.kind == NodeKind::Class
                && unchanged(&node.file_path)
            {
                files.insert(node.file_path);
            }
        }
    }
    Ok(files)
}

/// The packages whose presence in the project the sync flipped: declared
/// before the change and by no file after it, or the other way round.
fn flipped_packages(
    store: &Store,
    changes: &JavaChanges,
    unchanged: impl Fn(&str) -> bool,
) -> Result<BTreeSet<String>> {
    let mut sides: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    for change in changes.changes.values() {
        for package in &change.old_packages {
            sides.entry(package).or_default().0 = true;
        }
        for package in &change.new_packages {
            sides.entry(package).or_default().1 = true;
        }
    }
    let mut flipped = BTreeSet::new();
    for (package, (before, after)) in sides {
        if before == after {
            continue;
        }
        let elsewhere = store.nodes_by_qualified_name(package)?.iter().any(|node| {
            node.kind == NodeKind::Namespace && is_jvm(node.language) && unchanged(&node.file_path)
        });
        if !elsewhere {
            flipped.insert(package.to_string());
        }
    }
    Ok(flipped)
}

/// The unchanged Java files with an import binding from under a `flipped`
/// package, read from their text as resolution reads them.
fn import_dependents(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    flipped: &BTreeSet<String>,
    unchanged: impl Fn(&str) -> bool,
) -> Result<BTreeSet<String>> {
    let prefixes: Vec<String> = flipped
        .iter()
        .map(|package| format!("{package}."))
        .collect();
    let mut files = BTreeSet::new();
    for file in store.all_files()? {
        if file.language != Language::Java || !unchanged(&file.path) {
            continue;
        }
        let Some(text) = read_text(project_root, &file.path, max_file_size) else {
            continue;
        };
        if codegraph_resolve::import_resolver::extract_import_mappings(&text, Language::Java)
            .iter()
            .any(|mapping| {
                prefixes
                    .iter()
                    .any(|prefix| mapping.source.starts_with(prefix))
            })
        {
            files.insert(file.path);
        }
    }
    Ok(files)
}

/// Read what the Java lanes call for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Java file re-resolves.
pub(crate) fn java_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &JavaChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<JavaRefresh> {
    let mut refresh = JavaRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Java {
                continue;
            }
            let row = match changes.changes.get(&file.path) {
                Some(change) => change.new_row.clone(),
                None => read_text(project_root, &file.path, max_file_size).and_then(|text| {
                    store
                        .nodes_by_file_path(&file.path)
                        .ok()
                        .and_then(|nodes| row_of_text(file.language, &text, &nodes))
                }),
            };
            if unchanged(&file.path) {
                refresh.files.insert(file.path.clone());
            }
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
    if !seeds.is_empty() {
        let closure = closure(store, changes, seeds)?;
        refresh
            .files
            .extend(head_dependents(store, &closure, unchanged)?);
    }
    let flipped = flipped_packages(store, changes, unchanged)?;
    if !flipped.is_empty() {
        refresh.files.extend(import_dependents(
            store,
            project_root,
            max_file_size,
            &flipped,
            unchanged,
        )?);
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, supertypes: &[&str]) -> HeadEntry {
        HeadEntry {
            name: name.to_string(),
            qualified_name: format!("app::{name}"),
            supertypes: supertypes.iter().map(|s| s.to_string()).collect(),
            ancestors: supertypes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn change(old: Vec<HeadEntry>, new: Vec<HeadEntry>) -> JavaChanges {
        let mut changes = JavaChanges::default();
        changes.changes.insert(
            "src/A.java".to_string(),
            Change {
                old_row: (!old.is_empty()).then_some(HeadRow { entries: old }),
                new_row: (!new.is_empty()).then_some(HeadRow { entries: new }),
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn a_changed_head_seeds_its_name() {
        let changes = change(
            vec![entry("Base", &["Root"])],
            vec![entry("Base", &["Other"])],
        );
        assert_eq!(seeds(&changes), BTreeSet::from(["Base".to_string()]));
    }

    #[test]
    fn an_unchanged_head_seeds_nothing() {
        let changes = change(
            vec![entry("Base", &["Root"])],
            vec![entry("Base", &["Root"])],
        );
        assert!(seeds(&changes).is_empty());
    }

    #[test]
    fn same_named_entries_changing_places_seed_their_name() {
        let a = HeadEntry {
            qualified_name: "app::A::Node".to_string(),
            ..entry("Node", &["X"])
        };
        let b = HeadEntry {
            qualified_name: "app::B::Node".to_string(),
            ..entry("Node", &["Y"])
        };
        let changes = change(vec![a.clone(), b.clone()], vec![b, a]);
        assert_eq!(seeds(&changes), BTreeSet::from(["Node".to_string()]));
    }

    #[test]
    fn an_added_or_removed_head_seeds_its_name() {
        let added = change(Vec::new(), vec![entry("Fresh", &["Base"])]);
        assert_eq!(seeds(&added), BTreeSet::from(["Fresh".to_string()]));
        let removed = change(vec![entry("Gone", &["Base"])], Vec::new());
        assert_eq!(seeds(&removed), BTreeSet::from(["Gone".to_string()]));
    }

    #[test]
    fn rows_round_trip() {
        let row = HeadRow {
            entries: vec![entry("Base", &["Root"]), entry("Child", &["Base"])],
        };
        assert_eq!(HeadRow::decode(&row.encode()), Some(row));
        assert_eq!(HeadRow::decode("not json"), None);
    }
}
