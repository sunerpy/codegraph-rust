//! What a C# / Java / Kotlin call through a declared member reads of other
//! files (W4-CS-03, [`codegraph_resolve::member_types`]), so that a sync
//! re-resolves an unchanged file as a fresh index would.
//!
//! A receiver that is a field or property of the class around a call is the
//! type the class declares it with, or the type a class it inherits declares
//! it with, carrying the type arguments the subclass gives (upstream #2171).
//! The walk reads, by name, the heads and member-level lines of every class
//! it passes. The text is gone once a file changes on disk, so every full
//! index and migration writes a row per such file ([`MemberSurface`]): each
//! class declaration's name, qualified name, the supertypes its head names
//! and a digest of what the walk reads of it. A sync compares a changed
//! file's row with its new text: the names whose entries changed seed a
//! closure over every head naming one, and the unchanged files declaring a
//! class of a closure name re-resolve whole.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every C#, Java and Kotlin file once and
//! rebuilds the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_resolve::member_types::{
    MemberClass, MemberSurface, SURFACE_ROW_KEY_PREFIX, is_member_class_kind,
    is_member_typed_language, surface_row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a member-walk dependent
/// unrefreshed.
pub const VERSION_KEY: &str = "member_surface_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "2";

/// `source`'s member surface row, `None` when it records nothing: a file in
/// no member-typed language, one without a class, or one that is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<MemberSurface> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<MemberSurface> {
    if !is_member_typed_language(language) {
        return None;
    }
    let row = MemberSurface::of(text, nodes);
    (!row.is_empty()).then_some(row)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, MemberSurface)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&surface_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every member-typed file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every member-walk dependent is current: a full index, a
/// migration, or a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

fn read_row(store: &Store, path: &str) -> Result<Option<MemberSurface>> {
    Ok(store
        .get_project_metadata(&surface_row_key(path))?
        .and_then(|value| MemberSurface::decode(&value)))
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<MemberSurface>,
    new_row: Option<MemberSurface>,
}

/// What a sync's changed files held for the member lane.
#[derive(Debug, Default)]
pub(crate) struct MemberChanges {
    changes: BTreeMap<String, Change>,
}

impl MemberChanges {
    /// `path` changed (its old row still in the store) to `new` (its
    /// language, text and nodes), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let new_row = match new {
            Some((language, source, nodes)) => row_of(language, source, nodes),
            None => None,
        };
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

/// What a sync's member lane calls for.
#[derive(Debug, Default)]
pub(crate) struct MemberRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<MemberSurface>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl MemberRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(SURFACE_ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = surface_row_key(path);
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

/// Each name's entries of `classes`, in file order.
fn entries_by_name(classes: &[MemberClass]) -> BTreeMap<&str, Vec<&MemberClass>> {
    let mut out: BTreeMap<&str, Vec<&MemberClass>> = BTreeMap::new();
    for class in classes {
        out.entry(class.name.as_str()).or_default().push(class);
    }
    out
}

/// The names whose entries a changed file's rows differ in: a declaration
/// came, went, moved to another qualified name, names other supertypes or
/// reads otherwise, or same-named entries changed places.
fn seeds(changes: &MemberChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let classes =
            |row: Option<&MemberSurface>| row.map(|row| row.classes.clone()).unwrap_or_default();
        let old_classes = classes(change.old_row.as_ref());
        let new_classes = classes(change.new_row.as_ref());
        let old = entries_by_name(&old_classes);
        let new = entries_by_name(&new_classes);
        for name in old.keys().chain(new.keys()) {
            if old.get(name) != new.get(name) {
                seeds.insert((*name).to_string());
            }
        }
    }
    seeds
}

/// `seeds` and the name of every class (every unchanged file's row and each
/// changed file's new one) whose head names a name already reached, until
/// none is added.
fn closure<'r>(
    rows: impl IntoIterator<Item = &'r MemberSurface>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut naming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in rows {
        for class in &row.classes {
            for sup in &class.supers {
                naming
                    .entry(sup.as_str())
                    .or_default()
                    .insert(class.name.as_str());
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

/// Read what the member lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// member-typed file re-resolves.
pub(crate) fn member_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &MemberChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<MemberRefresh> {
    let mut refresh = MemberRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if !is_member_typed_language(file.language) {
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
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let stored: Vec<MemberSurface> = store
        .project_metadata_with_prefix(SURFACE_ROW_KEY_PREFIX)?
        .into_iter()
        .filter(|(key, _)| {
            key.strip_prefix(SURFACE_ROW_KEY_PREFIX)
                .is_some_and(|path| !changes.changes.contains_key(path))
        })
        .filter_map(|(_, value)| MemberSurface::decode(&value))
        .collect();
    let closure = closure(
        stored.iter().chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.new_row.as_ref()),
        ),
        seeds,
    );
    for name in &closure {
        for node in store.nodes_by_name(name)? {
            if is_member_typed_language(node.language)
                && is_member_class_kind(node.kind)
                && unchanged(&node.file_path)
            {
                refresh.files.insert(node.file_path);
            }
        }
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str, supers: &[&str], digest: &str) -> MemberClass {
        MemberClass {
            name: name.to_string(),
            qualified_name: format!("app::{name}"),
            supers: supers.iter().map(|s| s.to_string()).collect(),
            digest: digest.to_string(),
        }
    }

    fn change(old: Vec<MemberClass>, new: Vec<MemberClass>) -> MemberChanges {
        let mut changes = MemberChanges::default();
        changes.changes.insert(
            "src/A.cs".to_string(),
            Change {
                old_row: Some(MemberSurface { classes: old }),
                new_row: Some(MemberSurface { classes: new }),
            },
        );
        changes
    }

    #[test]
    fn a_changed_member_digest_seeds_its_name_and_closes_over_subclasses() {
        let changes = change(
            vec![class("WriterBase", &[], "aaaa")],
            vec![class("WriterBase", &[], "bbbb")],
        );
        let seeds = seeds(&changes);
        assert_eq!(seeds, BTreeSet::from(["WriterBase".to_string()]));
        let row = MemberSurface {
            classes: vec![
                class("TraceWriter", &["WriterBase"], "cccc"),
                class("Deeper", &["TraceWriter"], "dddd"),
                class("Other", &["Root"], "eeee"),
            ],
        };
        assert_eq!(
            closure([&row], seeds),
            BTreeSet::from([
                "Deeper".to_string(),
                "TraceWriter".to_string(),
                "WriterBase".to_string()
            ])
        );
    }

    #[test]
    fn unchanged_classes_seed_nothing() {
        let row = vec![class("Base", &["Root"], "aaaa")];
        assert!(seeds(&change(row.clone(), row)).is_empty());
    }

    #[test]
    fn rows_round_trip() {
        let row = MemberSurface {
            classes: vec![class("Base", &["Root", "IFoo"], "0123456789abcdef")],
        };
        assert_eq!(MemberSurface::decode(&row.encode()), Some(row));
        assert_eq!(MemberSurface::decode("not json"), None);
    }
}
