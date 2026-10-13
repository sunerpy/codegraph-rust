//! What Swift type resolution reads of other files (W4-SW-01,
//! [`codegraph_resolve::swift_type_visibility`]), so that a sync re-resolves
//! an unchanged file as a fresh index would.
//!
//! A Swift type reference lands on a type's declaration, never on an
//! extension of it; a nested type is named bare only inside its parent or a
//! type inheriting from it; a qualifier that names no project type is a
//! module's; and a conformance to a type the project only extends is read
//! from the conformer's head (upstream #2107). Whether a class node is an
//! extension and what a head names are read from the declarations' text,
//! which is gone once a file changes on disk, so every full index and
//! migration writes a row per Swift file with an entry per type declaration
//! ([`HeadRow`]). A sync compares a changed file's row with its new text: the
//! names of the entries that came, went or changed seed a closure over every
//! head naming a reached name, and the unchanged Swift files holding a
//! reference to a reached name, resolved or not, or declaring a type of one,
//! re-resolve whole.
//!
//! A call through a type path (W4-SW-02, upstream #2118) reads the path an
//! extension of a nested type writes (`extension API.PackageController`) and
//! whether an enum is a TCA `@Reducer`; a bare or `self.` call reads the
//! heads of the types around it by a second reader (upstream #2170). Both
//! are in the entries, and a file whose entries changed also touches every
//! name it declared or declares (any other changed file, the names whose
//! declarations, by qualified name, came, went or moved), for the name lane
//! and, for the unchanged Swift files holding unresolved references written
//! with a receiver ending in one, for the whole-file lane however many there
//! are.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Swift file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_resolve::swift_type_visibility::{
    HEAD_ROW_KEY_PREFIX, HeadEntry, HeadRow, head_row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a Swift dependent unrefreshed.
pub const VERSION_KEY: &str = "swift_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "2";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// Swift file, declares no type, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Swift {
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

/// Whether this index needs every Swift file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Swift dependent is current: a full index, a migration,
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
    /// The qualified names the file declared before and after, by name.
    old_names: BTreeMap<String, Vec<String>>,
    new_names: BTreeMap<String, Vec<String>>,
}

/// Each name `nodes` declare, with the qualified names declaring it, sorted.
fn node_names(nodes: &[Node]) -> BTreeMap<String, Vec<String>> {
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in nodes {
        names
            .entry(node.name.clone())
            .or_default()
            .push(node.qualified_name.clone());
    }
    for qualified in names.values_mut() {
        qualified.sort();
    }
    names
}

/// What a sync's changed files held for the Swift lane.
#[derive(Debug, Default)]
pub(crate) struct SwiftChanges {
    changes: BTreeMap<String, Change>,
}

impl SwiftChanges {
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
        let (new_row, new_names) = match new {
            Some((language, source, nodes)) => (row_of(language, source, nodes), node_names(nodes)),
            None => (None, BTreeMap::new()),
        };
        if old_row.is_none() && new_row.is_none() && !self.changes.contains_key(path) {
            return Ok(());
        }
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = self
            .changes
            .entry(path.to_string())
            .or_insert_with(|| Change {
                old_row,
                old_names: node_names(old_nodes),
                ..Change::default()
            });
        change.new_row = new_row;
        change.new_names = new_names;
        Ok(())
    }
}

/// What a sync's Swift lane calls for.
#[derive(Debug, Default)]
pub(crate) struct SwiftRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane: every name a file whose entries
    /// changed declared or declares, and in the other changed files the names
    /// whose declarations, by qualified name, differ.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl SwiftRefresh {
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

/// The entries of `row` with how many times each occurs.
fn entry_counts(row: Option<&HeadRow>) -> BTreeMap<&HeadEntry, usize> {
    let mut counts = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        *counts.entry(entry).or_default() += 1;
    }
    counts
}

/// The names of the entries a changed file's rows differ in: a declaration
/// came, went, became or stopped being an extension, moved to another
/// qualified name, or names other supertypes.
fn seeds(changes: &SwiftChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let old = entry_counts(change.old_row.as_ref());
        let new = entry_counts(change.new_row.as_ref());
        for entry in old.keys().chain(new.keys()) {
            if old.get(entry) != new.get(entry) {
                seeds.insert(entry.name().to_string());
            }
        }
    }
    seeds
}

/// `seeds` and the name of every declaration (every unchanged file's row and
/// each changed file's new one) whose head names a reached name, until none
/// is added: the subtypes whose scope reaches a changed type's nested types,
/// and whose supertype walk passes it.
fn closure<'r>(
    rows: impl IntoIterator<Item = &'r HeadRow>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut naming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            for supertype in entry.supertypes.iter().chain(&entry.inherits) {
                naming
                    .entry(supertype.as_str())
                    .or_default()
                    .insert(entry.name());
            }
        }
    }
    let mut reached = seeds.clone();
    let mut queue: VecDeque<String> = seeds.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        for deriving in naming.get(name.as_str()).into_iter().flatten() {
            if reached.insert((*deriving).to_string()) {
                queue.push_back((*deriving).to_string());
            }
        }
    }
    reached
}

/// Read what the Swift lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Swift file re-resolves.
pub(crate) fn swift_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &SwiftChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<SwiftRefresh> {
    let mut refresh = SwiftRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Swift {
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
        if change.old_row != change.new_row {
            refresh.names.extend(change.old_names.keys().cloned());
            refresh.names.extend(change.new_names.keys().cloned());
        } else {
            // A member declared under another owner, or a second one of its
            // name (`extension API.GetRoute` gaining `query()` beside an
            // `Elsewhere.query()`), settles the type-path calls of its name.
            for name in change.old_names.keys().chain(change.new_names.keys()) {
                if change.old_names.get(name) != change.new_names.get(name) {
                    refresh.names.insert(name.clone());
                }
            }
        }
    }
    // A call written with a receiver (`API.PackageController.GetRoute.query`)
    // waits for its tail's name, which the name lane retries only up to a
    // per-tail ceiling: the unchanged Swift files holding such references to
    // a touched name re-resolve whole instead.
    if !refresh.names.is_empty() {
        let names: Vec<String> = refresh.names.iter().cloned().collect();
        for reference in store.unresolved_refs_by_name_tails(&names, usize::MAX)? {
            if reference.language == Language::Swift && unchanged(&reference.file_path) {
                refresh.files.insert(reference.file_path);
            }
        }
    }
    let seeds = seeds(changes);
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let stored: Vec<HeadRow> = store
        .project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?
        .into_iter()
        .filter(|(key, _)| {
            key.strip_prefix(HEAD_ROW_KEY_PREFIX)
                .is_some_and(|path| !changes.changes.contains_key(path))
        })
        .filter_map(|(_, value)| HeadRow::decode(&value))
        .collect();
    let reached: Vec<String> = closure(
        stored.iter().chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.new_row.as_ref()),
        ),
        seeds,
    )
    .into_iter()
    .collect();
    let swift_files: HashSet<String> = store
        .all_files()?
        .into_iter()
        .filter(|file| file.language == Language::Swift)
        .map(|file| file.path)
        .collect();
    let mut add = |path: String| {
        if swift_files.contains(&path) && unchanged(&path) {
            refresh.files.insert(path);
        }
    };
    // The references to a reached name, resolved or parked.
    for site in store.reference_sites_of_edges_to_named_targets(&reached)? {
        add(site.file_path);
    }
    for reference in store.unresolved_refs_by_names(&reached)? {
        add(reference.file_path);
    }
    // The declarations of a reached name, extensions included.
    for name in &reached {
        for node in store.nodes_by_name(name)? {
            add(node.file_path);
        }
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(qualified_name: &str, extension: bool, supertypes: &[&str]) -> HeadEntry {
        HeadEntry {
            qualified_name: qualified_name.to_string(),
            kind: if extension { "class" } else { "struct" }.to_string(),
            extension,
            supertypes: supertypes.iter().map(|s| s.to_string()).collect(),
            inherits: supertypes.iter().map(|s| s.to_string()).collect(),
            written: None,
            reducer: false,
        }
    }

    fn row(entries: Vec<HeadEntry>) -> HeadRow {
        let mut entries = entries;
        entries.sort();
        HeadRow { entries }
    }

    fn change(old: Option<HeadRow>, new: Option<HeadRow>) -> SwiftChanges {
        let mut changes = SwiftChanges::default();
        changes.changes.insert(
            "App/Account+Display.swift".to_string(),
            Change {
                old_row: old,
                new_row: new,
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn an_entry_that_flips_or_names_other_supertypes_seeds_its_name() {
        let flipped = change(
            Some(row(vec![entry("Account", true, &[])])),
            Some(row(vec![HeadEntry {
                kind: "class".to_string(),
                ..entry("Account", false, &[])
            }])),
        );
        assert_eq!(seeds(&flipped), BTreeSet::from(["Account".to_string()]));
        let rebased = change(
            Some(row(vec![entry("Home::Row", false, &["View"])])),
            Some(row(vec![entry("Home::Row", false, &["Other"])])),
        );
        assert_eq!(seeds(&rebased), BTreeSet::from(["Row".to_string()]));
        let same = change(
            Some(row(vec![entry("Account", false, &[])])),
            Some(row(vec![entry("Account", false, &[])])),
        );
        assert!(seeds(&same).is_empty());
        let gone = change(Some(row(vec![entry("Account", true, &[])])), None);
        assert_eq!(seeds(&gone), BTreeSet::from(["Account".to_string()]));
    }

    #[test]
    fn the_closure_follows_heads_naming_a_reached_type() {
        let rows = [
            row(vec![entry("StreamChannel", false, &["BaseChannel"])]),
            row(vec![entry("Leaf", false, &["StreamChannel"])]),
            row(vec![entry("Other", false, &["Unrelated"])]),
        ];
        assert_eq!(
            closure(rows.iter(), BTreeSet::from(["BaseChannel".to_string()])),
            BTreeSet::from([
                "BaseChannel".to_string(),
                "Leaf".to_string(),
                "StreamChannel".to_string()
            ])
        );
    }

    #[test]
    fn only_swift_files_with_a_type_have_rows() {
        let text = SourceText::Text("let x = 1\n".to_string());
        assert!(row_of(Language::Swift, &text, &[]).is_none());
        assert!(row_of(Language::Kotlin, &text, &[]).is_none());
    }
}
