//! What Scala resolution reads of other files (W4-SC-01,
//! [`codegraph_resolve::scala_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare Scala call reaches the members of the supertypes the heads of the
//! types around it (and of an anonymous class's bases) name, of an imported
//! object's supertypes and of the package objects of its file's packages; a
//! type reference reaches the members of what the types around it extend.
//! Those walks read heads from source by name, in every file, and a package
//! object has no node at all. A candidate's own file says, in its text, which
//! package its package object opens, what package holds its top-level types,
//! which functions an `extension` line marks and which `val`s a block of a
//! class body scopes. None of that is a node, and the text is gone once a
//! file changes on disk, so every full index and migration writes a row per
//! Scala file ([`ScalaSurface`]), and a sync compares a changed file's row
//! with its new text:
//!
//! - heads and package objects that came, went or changed seed a closure over
//!   every head and package object naming one; the unchanged files whose walks
//!   start from a closure name re-resolve whole;
//! - a function or `val` whose `extension` or block reading flipped touches
//!   its name, a changed package-object package every name of its file, and a
//!   changed holder package the names of its file's top-level types, for the
//!   name lane.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Scala file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_resolve::scala_scope::{
    SURFACE_ROW_KEY_PREFIX, ScalaFact, ScalaHead, ScalaSurface, is_scala_type_kind, surface_row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a Scala dependent unrefreshed.
pub const VERSION_KEY: &str = "scala_surface_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s surface row, `None` when it records nothing: a file that is no
/// Scala file, holds nothing a row records, or is not text.
pub fn row_of(
    path: &str,
    language: Language,
    source: &SourceText,
    nodes: &[Node],
) -> Option<ScalaSurface> {
    match source {
        SourceText::Text(text) => row_of_text(path, language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(path: &str, language: Language, text: &str, nodes: &[Node]) -> Option<ScalaSurface> {
    if language != Language::Scala {
        return None;
    }
    let row = ScalaSurface::of(path, text, nodes);
    (!row.is_empty()).then_some(row)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, ScalaSurface)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&surface_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every Scala file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Scala dependent is current: a full index, a migration,
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

fn read_row(store: &Store, path: &str) -> Result<Option<ScalaSurface>> {
    Ok(store
        .get_project_metadata(&surface_row_key(path))?
        .and_then(|value| ScalaSurface::decode(&value)))
}

/// The names of `nodes`, and of their top-level Scala types (a qualified name
/// without `::`, whose holder is the file's package).
fn node_names(nodes: &[Node]) -> (BTreeSet<String>, BTreeSet<String>) {
    let all = nodes.iter().map(|node| node.name.clone()).collect();
    let top_types = nodes
        .iter()
        .filter(|node| is_scala_type_kind(node.kind) && !node.qualified_name.contains("::"))
        .map(|node| node.name.clone())
        .collect();
    (all, top_types)
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<ScalaSurface>,
    new_row: Option<ScalaSurface>,
    old_names: BTreeSet<String>,
    new_names: BTreeSet<String>,
    old_top_types: BTreeSet<String>,
    new_top_types: BTreeSet<String>,
}

/// What a sync's changed files held for the Scala lanes.
#[derive(Debug, Default)]
pub(crate) struct ScalaChanges {
    changes: BTreeMap<String, Change>,
}

impl ScalaChanges {
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
        let new_row = match new {
            Some((language, source, nodes)) => row_of(path, language, source, nodes),
            None => None,
        };
        if old_row.is_none() && new_row.is_none() {
            return Ok(());
        }
        let (old_names, old_top_types) = node_names(old_nodes);
        let (new_names, new_top_types) = new
            .map(|(_, _, nodes)| node_names(nodes))
            .unwrap_or_default();
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = self
            .changes
            .entry(path.to_string())
            .or_insert_with(|| Change {
                old_row,
                old_names,
                old_top_types,
                ..Change::default()
            });
        change.new_row = new_row;
        change.new_names = new_names;
        change.new_top_types = new_top_types;
        Ok(())
    }
}

/// What a sync's Scala lanes call for.
#[derive(Debug, Default)]
pub(crate) struct ScalaRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<ScalaSurface>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl ScalaRefresh {
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

/// The rows of the project as they were before the sync (`before`) or are
/// after it: every unchanged file's stored row, and each changed file's old
/// or new one, by path.
fn rows_view<'r>(
    stored: &'r BTreeMap<String, ScalaSurface>,
    changes: &'r ScalaChanges,
    before: bool,
) -> BTreeMap<&'r str, &'r ScalaSurface> {
    let mut view: BTreeMap<&str, &ScalaSurface> = stored
        .iter()
        .filter(|(path, _)| !changes.changes.contains_key(*path))
        .map(|(path, row)| (path.as_str(), row))
        .collect();
    for (path, change) in &changes.changes {
        let row = if before {
            change.old_row.as_ref()
        } else {
            change.new_row.as_ref()
        };
        if let Some(row) = row {
            view.insert(path.as_str(), row);
        }
    }
    view
}

/// Each name's entries of `entries`, in file order.
fn entries_by_name(entries: &[ScalaHead]) -> BTreeMap<&str, Vec<&ScalaHead>> {
    let mut out: BTreeMap<&str, Vec<&ScalaHead>> = BTreeMap::new();
    for entry in entries {
        out.entry(entry.name.as_str()).or_default().push(entry);
    }
    out
}

/// The names whose head or package-object entries a changed file's rows
/// differ in: an entry came, went or names other supertypes, or same-named
/// entries changed places.
fn seeds(changes: &ScalaChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let pick = |row: Option<&ScalaSurface>, objects: bool| -> Vec<ScalaHead> {
            row.map(|row| {
                if objects {
                    row.package_objects.clone()
                } else {
                    row.heads.clone()
                }
            })
            .unwrap_or_default()
        };
        for objects in [false, true] {
            let old_entries = pick(change.old_row.as_ref(), objects);
            let new_entries = pick(change.new_row.as_ref(), objects);
            let old = entries_by_name(&old_entries);
            let new = entries_by_name(&new_entries);
            for name in old.keys().chain(new.keys()) {
                if old.get(name) != new.get(name) {
                    seeds.insert((*name).to_string());
                }
            }
        }
    }
    seeds
}

/// `seeds` and the name of every head and package object (before and after
/// the change) naming a name already reached, until none is added.
fn closure(
    before: &BTreeMap<&str, &ScalaSurface>,
    after: &BTreeMap<&str, &ScalaSurface>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut naming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in before.values().chain(after.values()) {
        for entry in row.heads.iter().chain(&row.package_objects) {
            for supertype in &entry.supertypes {
                naming
                    .entry(supertype.as_str())
                    .or_default()
                    .insert(entry.name.as_str());
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

/// The names one changed file's text facts touch: its functions whose
/// `extension` reading and its `val`s whose block reading flipped (by node
/// id, so two same-named declarations trading their state both count),
/// every name of the file when the package its package object opens
/// changed, and its top-level types' names when the package holding them
/// changed.
fn fact_names(change: &Change) -> BTreeSet<String> {
    let empty = ScalaSurface::default();
    let old = change.old_row.as_ref().unwrap_or(&empty);
    let new = change.new_row.as_ref().unwrap_or(&empty);
    let flipped = |a: &[ScalaFact], b: &[ScalaFact]| -> BTreeSet<String> {
        let a: BTreeSet<&ScalaFact> = a.iter().collect();
        let b: BTreeSet<&ScalaFact> = b.iter().collect();
        a.symmetric_difference(&b)
            .map(|fact| fact.name.clone())
            .collect()
    };
    let mut names = flipped(&old.extension_functions, &new.extension_functions);
    names.extend(flipped(&old.block_locals, &new.block_locals));
    if old.object_package != new.object_package {
        names.extend(change.old_names.iter().cloned());
        names.extend(change.new_names.iter().cloned());
    }
    if old.holder != new.holder {
        names.extend(change.old_top_types.iter().cloned());
        names.extend(change.new_top_types.iter().cloned());
    }
    names
}

/// Read what the Scala lanes call for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Scala file re-resolves.
pub(crate) fn scala_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &ScalaChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<ScalaRefresh> {
    let mut refresh = ScalaRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Scala {
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
        refresh.names.extend(fact_names(change));
    }
    // A reference written with a receiver (`client.Response`) waits for its
    // tail's name, which the name lane retries only up to a per-tail ceiling:
    // the unchanged files holding such references to a touched name
    // re-resolve whole instead, however many there are.
    if !refresh.names.is_empty() {
        let names: Vec<String> = refresh.names.iter().cloned().collect();
        for reference in store.unresolved_refs_by_name_tails(&names, usize::MAX)? {
            if unchanged(&reference.file_path) {
                refresh.files.insert(reference.file_path);
            }
        }
    }
    let seeds = seeds(changes);
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let stored: BTreeMap<String, ScalaSurface> = store
        .project_metadata_with_prefix(SURFACE_ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            Some((
                key.strip_prefix(SURFACE_ROW_KEY_PREFIX)?.to_string(),
                ScalaSurface::decode(&value)?,
            ))
        })
        .collect();
    let before = rows_view(&stored, changes, true);
    let after = rows_view(&stored, changes, false);
    let closure = closure(&before, &after, seeds);
    for (path, row) in &after {
        if unchanged(path) && row.starts.iter().any(|name| closure.contains(name)) {
            refresh.files.insert((*path).to_string());
        }
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(name: &str, supertypes: &[&str]) -> ScalaHead {
        ScalaHead {
            name: name.to_string(),
            supertypes: supertypes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn change(old: ScalaSurface, new: ScalaSurface) -> ScalaChanges {
        let mut changes = ScalaChanges::default();
        changes.changes.insert(
            "src/A.scala".to_string(),
            Change {
                old_row: Some(old),
                new_row: Some(new),
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn a_changed_head_seeds_its_name_and_closes_over_heads_and_package_objects() {
        let old = ScalaSurface {
            heads: vec![head("Base", &["HelperA"])],
            ..ScalaSurface::default()
        };
        let new = ScalaSurface {
            heads: vec![head("Base", &["HelperB"])],
            ..ScalaSurface::default()
        };
        let changes = change(old, new);
        let seeds = seeds(&changes);
        assert_eq!(seeds, BTreeSet::from(["Base".to_string()]));
        let child = ScalaSurface {
            heads: vec![head("Child", &["Base"])],
            package_objects: vec![head("api", &["Child"])],
            ..ScalaSurface::default()
        };
        let stored = BTreeMap::from([("src/Child.scala".to_string(), child)]);
        let before = rows_view(&stored, &changes, true);
        let after = rows_view(&stored, &changes, false);
        assert_eq!(
            closure(&before, &after, seeds),
            BTreeSet::from(["Base".to_string(), "Child".to_string(), "api".to_string()])
        );
    }

    #[test]
    fn a_changed_package_object_seeds_its_name() {
        let old = ScalaSurface {
            package_objects: vec![head("api", &["Api"])],
            ..ScalaSurface::default()
        };
        let new = ScalaSurface {
            package_objects: vec![head("api", &["Api2"])],
            ..ScalaSurface::default()
        };
        assert_eq!(
            seeds(&change(old, new)),
            BTreeSet::from(["api".to_string()])
        );
    }

    #[test]
    fn unchanged_heads_seed_nothing() {
        let row = ScalaSurface {
            heads: vec![head("Base", &["Root"])],
            starts: vec!["Base".to_string()],
            ..ScalaSurface::default()
        };
        assert!(seeds(&change(row.clone(), row)).is_empty());
    }

    #[test]
    fn text_facts_touch_the_names_they_change() {
        let fact = |id: &str, name: &str| ScalaFact {
            id: id.to_string(),
            name: name.to_string(),
        };
        let old = ScalaSurface {
            extension_functions: vec![fact("function:1", "double")],
            block_locals: vec![fact("field:a", "f"), fact("field:k", "keep")],
            object_package: Some("algebra".to_string()),
            holder: Some("client".to_string()),
            ..ScalaSurface::default()
        };
        // `C::f` is local now and `A::f` a member: the names alone are equal.
        let new = ScalaSurface {
            extension_functions: vec![fact("function:2", "triple")],
            block_locals: vec![fact("field:c", "f"), fact("field:k", "keep")],
            object_package: Some("algebra".to_string()),
            holder: Some("clients".to_string()),
            ..ScalaSurface::default()
        };
        let change = Change {
            old_row: Some(old),
            new_row: Some(new),
            old_names: BTreeSet::from(["Eq".to_string(), "Response".to_string()]),
            new_names: BTreeSet::from(["Eq".to_string(), "Response".to_string()]),
            old_top_types: BTreeSet::from(["Response".to_string()]),
            new_top_types: BTreeSet::from(["Response".to_string()]),
        };
        assert_eq!(
            fact_names(&change),
            BTreeSet::from([
                "Response".to_string(),
                "double".to_string(),
                "f".to_string(),
                "triple".to_string()
            ])
        );
    }

    #[test]
    fn rows_round_trip() {
        let row = ScalaSurface {
            heads: vec![head("Base", &["Root"])],
            package_objects: vec![head("api", &["Api"])],
            starts: vec!["Base".to_string()],
            holder: Some("app".to_string()),
            ..ScalaSurface::default()
        };
        assert_eq!(ScalaSurface::decode(&row.encode()), Some(row));
        assert_eq!(ScalaSurface::decode("not json"), None);
    }
}
