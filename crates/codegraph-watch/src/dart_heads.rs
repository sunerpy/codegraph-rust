//! What a bare Dart call reads of other files (W4-DART-01,
//! [`codegraph_resolve::dart_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare Dart call reaches a member of the class it is written in, of what
//! that class extends, mixes in or implements, or of an extension `on` one of
//! those types, the nearest one winning (upstream #2145); and a core method
//! name on an untyped receiver reaches an extension's member only through a
//! receiver named after the type the extension is `on` (#2207). The hierarchy
//! and the `on` types are read from the declarations' heads: text that is
//! gone once a file changes on disk, so every full index and migration writes
//! a row per Dart file with an entry per type declaration ([`HeadRow`]). A
//! sync compares a changed file's row with its new text; the type names
//! whose entries changed are the seeds. A call reaches a seed's ancestry
//! only from inside a type at or below the seed, and there a type of it can
//! come into reach, leave it or move in the ranking only as its distance
//! from the seed changed; past the resolver's walk limit, a whole ancestry
//! counts. The names of the members of those types, of the extensions on
//! one, and of an extension seed are touched for the name lane, and the
//! unresolved calls written with a receiver to a member of such a name are
//! retried at their sites, however many there are (the shared tail lookup
//! caps them).
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`], or with a row it cannot read, re-resolves every
//! Dart file once and rebuilds the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_resolve::dart_scope::{
    HEAD_ROW_KEY_PREFIX, HIERARCHY_LIMIT, HeadRow, head_row_key, member_owner,
};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

/// Metadata key present when no sync left a Dart dependent unrefreshed.
pub const VERSION_KEY: &str = "dart_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// Dart file, declares no type, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Dart {
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

/// Whether this index needs every Dart file re-resolved and its rows
/// rebuilt: its marker is missing or at another version, or a row cannot be
/// read (failing closed even in a sync with nothing changed).
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    if store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION) {
        return Ok(true);
    }
    Ok(store
        .project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?
        .iter()
        .any(|(_, value)| HeadRow::decode(value).is_none()))
}

/// Record that every Dart dependent is current: a full index, a migration,
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
}

/// What a sync's changed files held for the Dart lane.
#[derive(Debug, Default)]
pub(crate) struct DartChanges {
    changes: BTreeMap<String, Change>,
}

impl DartChanges {
    /// `path` changed (its old row still in the store) to `new` (its
    /// language, text and nodes), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let new_row = new.and_then(|(language, source, nodes)| row_of(language, source, nodes));
        if old_row.is_none() && new_row.is_none() {
            return Ok(());
        }
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = self
            .changes
            .entry(path.to_string())
            .or_insert_with(|| Change {
                old_row,
                ..Change::default()
            });
        change.new_row = new_row;
        Ok(())
    }
}

/// What a sync's Dart lane calls for.
#[derive(Debug, Default)]
pub(crate) struct DartRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane: the members of a seed's ancestry,
    /// before and after, and of the extensions on one of those types.
    pub(crate) names: BTreeSet<String>,
    /// Unresolved calls in unchanged files to retry at their sites.
    pub(crate) sites: Vec<FileReferenceSite>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl DartRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?;
        }
        for (written, (path, row)) in self.rows.iter().enumerate() {
            #[cfg(test)]
            if self.rebuilt
                && written == 1
                && crate::sync::tests::FAIL_AFTER_DART_ROW_PURGE.with(std::cell::Cell::get)
            {
                anyhow::bail!("injected failure in a Dart row rebuild");
            }
            #[cfg(not(test))]
            let _ = written;
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

/// What `row` declares of each type name, in source order: whether it is an
/// extension, the supertypes it names and the type it is `on`.
type Declared<'r> = BTreeMap<&'r str, Vec<(bool, &'r [String], Option<&'r str>)>>;

fn declared_by_name(row: Option<&HeadRow>) -> Declared<'_> {
    let mut out: Declared<'_> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry(entry.name.as_str()).or_default().push((
            entry.extension,
            entry.supers.as_slice(),
            entry.on.as_deref(),
        ));
    }
    out
}

/// The type names whose entries a changed file's rows move: one that came,
/// went, became or stopped being an extension, names other supertypes, or
/// sits `on` another type.
fn seeds(changes: &DartChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let old = declared_by_name(change.old_row.as_ref());
        let new = declared_by_name(change.new_row.as_ref());
        for name in old.keys().chain(new.keys()) {
            if old.get(name) != new.get(name) {
                seeds.insert((*name).to_string());
            }
        }
    }
    seeds
}

/// Each type name's supertypes, as every declaration of the name names them.
type Links<'r> = HashMap<&'r str, BTreeSet<&'r str>>;

fn links<'r>(rows: impl IntoIterator<Item = &'r HeadRow>) -> Links<'r> {
    let mut up: Links<'r> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            up.entry(entry.name.as_str())
                .or_default()
                .extend(entry.supers.iter().map(String::as_str));
        }
    }
    up
}

/// The types naming each type name as a supertype.
fn down_links<'r>(rows: impl IntoIterator<Item = &'r HeadRow>) -> Links<'r> {
    let mut down: Links<'r> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            for sup in &entry.supers {
                down.entry(sup.as_str())
                    .or_default()
                    .insert(entry.name.as_str());
            }
        }
    }
    down
}

/// Every type `links` reaches from `seed`, by supertype distance, with no
/// bound.
fn depths(links: &Links<'_>, seed: &str) -> HashMap<String, usize> {
    let mut found: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::from([(seed.to_string(), 0)]);
    while let Some((name, depth)) = queue.pop_front() {
        if found.contains_key(&name) {
            continue;
        }
        if let Some(next) = links.get(name.as_str()) {
            queue.extend(next.iter().map(|name| ((*name).to_string(), depth + 1)));
        }
        found.insert(name, depth);
    }
    found
}

/// `seed` and every type `links` reaches from it, with no bound.
fn closure(links: &Links<'_>, seed: &str) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::from([seed.to_string()]);
    while let Some(name) = queue.pop_front() {
        if reached.contains(&name) {
            continue;
        }
        if let Some(next) = links.get(name.as_str()) {
            queue.extend(next.iter().map(|name| (*name).to_string()));
        }
        reached.insert(name);
    }
    reached
}

/// Read what the Dart lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Dart file re-resolves.
pub(crate) fn dart_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &DartChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<DartRefresh> {
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        return rebuild(store, project_root, max_file_size, changes, &unchanged);
    }
    let mut refresh = DartRefresh::default();
    for (path, change) in &changes.changes {
        refresh.rows.insert(path.clone(), change.new_row.clone());
    }
    let seeds = seeds(changes);
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let mut stored: Vec<(String, HeadRow)> = Vec::new();
    for (key, value) in store.project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)? {
        let Some(path) = key.strip_prefix(HEAD_ROW_KEY_PREFIX) else {
            continue;
        };
        if changes.changes.contains_key(path) {
            continue;
        }
        let Some(row) = HeadRow::decode(&value) else {
            return rebuild(store, project_root, max_file_size, changes, &unchanged);
        };
        stored.push((path.to_string(), row));
    }
    let before_rows = || {
        stored.iter().map(|(_, row)| row).chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.old_row.as_ref()),
        )
    };
    let after_rows = || {
        stored.iter().map(|(_, row)| row).chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.new_row.as_ref()),
        )
    };
    let (before, down_before) = (links(before_rows()), down_links(before_rows()));
    let (after, down_after) = (links(after_rows()), down_links(after_rows()));
    let declared_unchanged: BTreeSet<&str> = stored
        .iter()
        .flat_map(|(_, row)| &row.entries)
        .map(|entry| entry.name.as_str())
        .collect();
    let extension_names: BTreeSet<&str> = before_rows()
        .chain(after_rows())
        .flat_map(|row| &row.entries)
        .filter(|entry| entry.extension)
        .map(|entry| entry.name.as_str())
        .collect();
    let mut owners: BTreeSet<String> = BTreeSet::new();
    let mut moved: BTreeSet<String> = BTreeSet::new();
    for seed in &seeds {
        // An extension's members come into or leave the reach of the calls
        // in its `on` types' hierarchies.
        if extension_names.contains(seed.as_str()) {
            owners.insert(seed.clone());
        }
        // A call's hierarchy reaches a seed only from inside a type at or
        // below it: one below it in any file, or the seed's own declaration
        // in an unchanged file (a changed file re-resolves whole). There, a
        // type of the seed's ancestry can come into reach, leave it, or move
        // in the ranking only as its distance from the seed changed.
        let mut below = closure(&down_before, seed);
        below.extend(closure(&down_after, seed));
        below.remove(seed);
        let own_unchanged = declared_unchanged.contains(seed.as_str());
        if below.is_empty() && !own_unchanged {
            continue;
        }
        let (from_before, from_after) = (depths(&before, seed), depths(&after, seed));
        for name in from_before.keys().chain(from_after.keys()) {
            if from_before.get(name) != from_after.get(name) {
                moved.insert(name.clone());
            }
        }
        // The resolver's walk stops at its limit in breadth-first order, so
        // a change can push any branch of a long ancestry in or out of it:
        // every type of it, before and after, counts as moved.
        if own_unchanged {
            below.insert(seed.clone());
        }
        for name in &below {
            let (up_before, up_after) = (closure(&before, name), closure(&after, name));
            if up_before.len() >= HIERARCHY_LIMIT || up_after.len() >= HIERARCHY_LIMIT {
                moved.extend(up_before);
                moved.extend(up_after);
            }
        }
    }
    owners.extend(moved.iter().cloned());
    // An extension on one of those types ranks by the type's distance.
    let declaring = || {
        stored.iter().map(|(path, row)| (path.as_str(), row)).chain(
            changes.changes.iter().filter_map(|(path, change)| {
                change.new_row.as_ref().map(|row| (path.as_str(), row))
            }),
        )
    };
    let extensions: Vec<String> = declaring()
        .flat_map(|(_, row)| &row.entries)
        .filter(|entry| entry.extension && entry.supers.iter().any(|sup| moved.contains(sup)))
        .map(|entry| entry.name.clone())
        .collect();
    owners.extend(extensions);
    // The names of those types' members, in the files declaring one (the
    // store holds a changed file's new nodes).
    let files: BTreeSet<&str> = declaring()
        .filter(|(_, row)| row.entries.iter().any(|entry| owners.contains(&entry.name)))
        .map(|(path, _)| path)
        .collect();
    for path in files {
        for node in store.nodes_by_file_path(path)? {
            if node.language == Language::Dart
                && codegraph_resolve::dart_scope::is_dart_member(&node)
                && member_owner(&node.qualified_name).is_some_and(|owner| owners.contains(owner))
            {
                refresh.names.insert(node.name);
            }
        }
    }
    // A call written with a receiver waits for its member's name, which the
    // shared tail lookup caps at 500 per tail: the parked Dart ones to a
    // touched name are retried at their sites, however many there are.
    if !refresh.names.is_empty() {
        let members: Vec<String> = refresh.names.iter().cloned().collect();
        for reference in store.unresolved_refs_of_language_by_members(Language::Dart, &members)? {
            if unchanged(&reference.file_path) {
                refresh.sites.push(FileReferenceSite {
                    file_path: reference.file_path,
                    site: Some(ReferenceSite {
                        from_node_id: reference.from_node_id,
                        line: reference.line,
                        col: reference.col,
                    }),
                });
            }
        }
    }
    Ok(refresh)
}

/// Every Dart file re-resolves and every row is rebuilt from disk (a changed
/// file's from its new text).
fn rebuild(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &DartChanges,
    unchanged: &dyn Fn(&str) -> bool,
) -> Result<DartRefresh> {
    let mut refresh = DartRefresh {
        rebuilt: true,
        ..DartRefresh::default()
    };
    for file in store.all_files()? {
        if file.language != Language::Dart {
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
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_resolve::dart_scope::HeadEntry;

    fn entry(name: &str, extension: bool, supers: &[&str]) -> HeadEntry {
        HeadEntry {
            name: name.to_string(),
            extension,
            supers: supers.iter().map(|s| (*s).to_string()).collect(),
            on: None,
        }
    }

    fn change(old: Option<Vec<HeadEntry>>, new: Option<Vec<HeadEntry>>) -> DartChanges {
        let mut changes = DartChanges::default();
        changes.changes.insert(
            "lib/a.dart".to_string(),
            Change {
                old_row: old.map(|entries| HeadRow { entries }),
                new_row: new.map(|entries| HeadRow { entries }),
            },
        );
        changes
    }

    #[test]
    fn a_type_rebased_turned_into_an_extension_added_or_removed_seeds_itself() {
        let kept = || vec![entry("A", false, &["B"]), entry("C", false, &[])];
        assert!(seeds(&change(Some(kept()), Some(kept()))).is_empty());
        assert_eq!(
            seeds(&change(
                Some(kept()),
                Some(vec![entry("A", false, &["D"]), entry("C", false, &[])])
            )),
            BTreeSet::from(["A".to_string()])
        );
        assert_eq!(
            seeds(&change(
                Some(kept()),
                Some(vec![entry("A", false, &["B"]), entry("C", true, &[])])
            )),
            BTreeSet::from(["C".to_string()])
        );
        assert_eq!(
            seeds(&change(Some(kept()), None)),
            BTreeSet::from(["A".to_string(), "C".to_string()])
        );
    }

    #[test]
    fn a_closure_follows_every_declaration_of_a_name() {
        let rows = [
            HeadRow {
                entries: vec![entry("A", false, &["B"]), entry("B", false, &["C"])],
            },
            HeadRow {
                entries: vec![entry("B", false, &["D"]), entry("D", false, &["A"])],
            },
        ];
        let up = links(rows.iter());
        assert_eq!(
            closure(&up, "A"),
            BTreeSet::from(["A", "B", "C", "D"].map(String::from))
        );
    }
}
