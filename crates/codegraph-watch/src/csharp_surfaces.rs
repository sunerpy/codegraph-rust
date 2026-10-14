//! What C# resolution reads of other files (W4-CS-01,
//! [`codegraph_resolve::csharp_types`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare C# type name is visible through the `global using`s of every file
//! of its project, a nested type by bare name only from its owner or a type
//! deriving from it, and a bare member name only in the types around it, the
//! bases they list or a type a `global using static` of any file names
//! (W4-CS-02). The heads of a type's partial parts and of the bases they name
//! are read from source by name in every file, by two readers. The text is
//! gone once a file changes on disk, so every full index and migration
//! writes a row per C# file ([`CsharpSurface`]), and a sync compares a changed
//! file's row with its new text:
//!
//! - a changed file whose `global using`s or `global using static`s changed
//!   re-resolves every unchanged C# file whole (the project boundary is a
//!   `.csproj`, which is no source file);
//! - heads that came, went or changed seed a closure over every head naming
//!   one; the unchanged files declaring a C# type of a closure name
//!   re-resolve whole;
//! - a type whose owner flipped between a type and a namespace of the same
//!   name keeps its node, so its name is touched for the name lane.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every C# file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_resolve::csharp_types::{
    CsharpHead, CsharpSurface, SURFACE_ROW_KEY_PREFIX, csharp_owner_name, is_csharp_owner_type,
    is_csharp_type_kind, surface_row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a C# dependent unrefreshed.
pub const VERSION_KEY: &str = "csharp_surface_version";
/// Value of [`VERSION_KEY`]: 2 since the rows carry `global_statics` and
/// each head's `supers` (W4-CS-02).
pub const VERSION: &str = "2";

/// `source`'s surface row, `None` when it records nothing: a file that is no
/// C# file, holds nothing a row records, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<CsharpSurface> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<CsharpSurface> {
    if language != Language::CSharp {
        return None;
    }
    let row = CsharpSurface::of(text, nodes);
    (!row.is_empty()).then_some(row)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, CsharpSurface)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&surface_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every C# file re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every C# dependent is current: a full index, a migration, or
/// a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

fn read_row(store: &Store, path: &str) -> Result<Option<CsharpSurface>> {
    Ok(store
        .get_project_metadata(&surface_row_key(path))?
        .and_then(|value| CsharpSurface::decode(&value)))
}

/// Whether the C# type `node`'s owner is a type of `nodes` (its own file):
/// what the nested-type rule reads of the candidate's file.
fn owner_is_type(node: &Node, nodes: &[Node]) -> bool {
    csharp_owner_name(&node.qualified_name)
        .is_some_and(|owner| nodes.iter().any(|other| is_csharp_owner_type(other, owner)))
}

/// The names of the C# types of a file that keep their node across the
/// change while their owner flips between a type and something else.
fn owner_flips(old_nodes: &[Node], new_nodes: &[Node]) -> BTreeSet<String> {
    let is_type =
        |node: &&Node| node.language == Language::CSharp && is_csharp_type_kind(node.kind);
    let before: HashMap<&str, bool> = old_nodes
        .iter()
        .filter(is_type)
        .map(|node| (node.id.as_str(), owner_is_type(node, old_nodes)))
        .collect();
    new_nodes
        .iter()
        .filter(is_type)
        .filter(|node| {
            before
                .get(node.id.as_str())
                .is_some_and(|&was| was != owner_is_type(node, new_nodes))
        })
        .map(|node| node.name.clone())
        .collect()
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<CsharpSurface>,
    new_row: Option<CsharpSurface>,
    /// The names of its types whose owner flipped, across every change of
    /// the file in this sync.
    owner_flips: BTreeSet<String>,
}

/// What a sync's changed files held for the C# lanes.
#[derive(Debug, Default)]
pub(crate) struct CsharpChanges {
    changes: BTreeMap<String, Change>,
}

impl CsharpChanges {
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
            Some((language, source, nodes)) => row_of(language, source, nodes),
            None => None,
        };
        let flips = new
            .map(|(_, _, nodes)| owner_flips(old_nodes, nodes))
            .unwrap_or_default();
        if old_row.is_none() && new_row.is_none() && flips.is_empty() {
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
        change.owner_flips.extend(flips);
        Ok(())
    }
}

/// What a sync's C# lanes call for.
#[derive(Debug, Default)]
pub(crate) struct CsharpRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<CsharpSurface>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl CsharpRefresh {
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

/// Whether a change moved what every file of a project reads of the file:
/// its `global using`s or `global using static`s.
fn shared_usings_changed(change: &Change) -> bool {
    let globals = |row: Option<&CsharpSurface>| {
        row.map(|row| (row.globals.clone(), row.global_statics.clone()))
            .unwrap_or_default()
    };
    globals(change.old_row.as_ref()) != globals(change.new_row.as_ref())
}

/// Each name's entries of `heads`, in file order.
fn entries_by_name(heads: &[CsharpHead]) -> BTreeMap<&str, Vec<&CsharpHead>> {
    let mut out: BTreeMap<&str, Vec<&CsharpHead>> = BTreeMap::new();
    for head in heads {
        out.entry(head.name.as_str()).or_default().push(head);
    }
    out
}

/// The names whose head entries a changed file's rows differ in: an entry
/// came, went, moved to another qualified name or names other bases, or
/// same-named entries changed places.
fn seeds(changes: &CsharpChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let heads =
            |row: Option<&CsharpSurface>| row.map(|row| row.heads.clone()).unwrap_or_default();
        let old_heads = heads(change.old_row.as_ref());
        let new_heads = heads(change.new_row.as_ref());
        let old = entries_by_name(&old_heads);
        let new = entries_by_name(&new_heads);
        for name in old.keys().chain(new.keys()) {
            if old.get(name) != new.get(name) {
                seeds.insert((*name).to_string());
            }
        }
    }
    seeds
}

/// `seeds` and the name of every head (every unchanged file's row and each
/// changed file's new one) listing a name already reached as a base to
/// either reader, until none is added.
fn closure<'r>(
    rows: impl IntoIterator<Item = &'r CsharpSurface>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut naming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in rows {
        for head in &row.heads {
            for base in head.bases.iter().chain(&head.supers) {
                naming
                    .entry(base.as_str())
                    .or_default()
                    .insert(head.name.as_str());
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

/// Read what the C# lanes call for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// C# file re-resolves.
pub(crate) fn csharp_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &CsharpChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<CsharpRefresh> {
    let mut refresh = CsharpRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::CSharp {
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
    let mut globals_changed = false;
    for (path, change) in &changes.changes {
        if change.old_row.is_some() || change.new_row.is_some() {
            refresh.rows.insert(path.clone(), change.new_row.clone());
        }
        refresh.names.extend(change.owner_flips.iter().cloned());
        globals_changed |= shared_usings_changed(change);
    }
    if globals_changed {
        for file in store.all_files()? {
            if file.language == Language::CSharp && unchanged(&file.path) {
                refresh.files.insert(file.path);
            }
        }
        return Ok(refresh);
    }
    let seeds = seeds(changes);
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let stored: Vec<CsharpSurface> = store
        .project_metadata_with_prefix(SURFACE_ROW_KEY_PREFIX)?
        .into_iter()
        .filter(|(key, _)| {
            key.strip_prefix(SURFACE_ROW_KEY_PREFIX)
                .is_some_and(|path| !changes.changes.contains_key(path))
        })
        .filter_map(|(_, value)| CsharpSurface::decode(&value))
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
            if node.language == Language::CSharp
                && is_csharp_type_kind(node.kind)
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
    use codegraph_core::types::NodeKind;

    fn head(name: &str, bases: &[&str]) -> CsharpHead {
        CsharpHead {
            name: name.to_string(),
            qualified_name: format!("Lib::{name}"),
            bases: bases.iter().map(|s| s.to_string()).collect(),
            supers: bases.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn change(old: CsharpSurface, new: CsharpSurface) -> CsharpChanges {
        let mut changes = CsharpChanges::default();
        changes.changes.insert(
            "src/A.cs".to_string(),
            Change {
                old_row: Some(old),
                new_row: Some(new),
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn a_changed_head_seeds_its_name_and_closes_over_deriving_heads() {
        let old = CsharpSurface {
            heads: vec![head("Middle", &["Reader"])],
            ..CsharpSurface::default()
        };
        let new = CsharpSurface::default();
        let changes = change(old, new);
        let seeds = seeds(&changes);
        assert_eq!(seeds, BTreeSet::from(["Middle".to_string()]));
        let leaf = CsharpSurface {
            heads: vec![head("Leaf", &["Middle"]), head("Other", &["Root"])],
            ..CsharpSurface::default()
        };
        let deeper = CsharpSurface {
            heads: vec![head("Deeper", &["Leaf"])],
            ..CsharpSurface::default()
        };
        assert_eq!(
            closure([&leaf, &deeper], seeds),
            BTreeSet::from([
                "Deeper".to_string(),
                "Leaf".to_string(),
                "Middle".to_string()
            ])
        );
    }

    #[test]
    fn a_head_moving_to_another_qualified_name_seeds_its_name() {
        let old = CsharpSurface {
            heads: vec![head("Reader", &["Base"])],
            ..CsharpSurface::default()
        };
        let mut moved = head("Reader", &["Base"]);
        moved.qualified_name = "Other::Reader".to_string();
        let new = CsharpSurface {
            heads: vec![moved],
            ..CsharpSurface::default()
        };
        assert_eq!(
            seeds(&change(old, new)),
            BTreeSet::from(["Reader".to_string()])
        );
    }

    #[test]
    fn unchanged_heads_seed_nothing() {
        let row = CsharpSurface {
            globals: vec!["App.Support".to_string()],
            global_statics: vec!["App.Execution.ExpressionBuilder".to_string()],
            heads: vec![head("Reader", &["Base"])],
        };
        assert!(seeds(&change(row.clone(), row)).is_empty());
    }

    #[test]
    fn a_base_only_the_second_reader_sees_seeds_its_name_and_closes() {
        let mut middle = head("Middle", &[]);
        middle.supers = vec!["SpecA".to_string()];
        let mut moved = middle.clone();
        moved.supers = vec!["SpecB".to_string()];
        let changes = change(
            CsharpSurface {
                heads: vec![middle],
                ..CsharpSurface::default()
            },
            CsharpSurface {
                heads: vec![moved],
                ..CsharpSurface::default()
            },
        );
        let seeds = seeds(&changes);
        assert_eq!(seeds, BTreeSet::from(["Middle".to_string()]));
        let mut leaf = head("Leaf", &[]);
        leaf.supers = vec!["Middle".to_string()];
        let row = CsharpSurface {
            heads: vec![leaf],
            ..CsharpSurface::default()
        };
        assert_eq!(
            closure([&row], seeds),
            BTreeSet::from(["Leaf".to_string(), "Middle".to_string()])
        );
    }

    #[test]
    fn a_global_using_or_global_using_static_moving_is_a_shared_change() {
        let with = |globals: &[&str], statics: &[&str]| CsharpSurface {
            globals: globals.iter().map(|s| s.to_string()).collect(),
            global_statics: statics.iter().map(|s| s.to_string()).collect(),
            ..CsharpSurface::default()
        };
        let changed = |old: CsharpSurface, new: CsharpSurface| {
            let changes = change(old, new);
            shared_usings_changed(changes.changes.values().next().unwrap())
        };
        assert!(changed(with(&["A"], &[]), with(&["B"], &[])));
        assert!(changed(with(&[], &[]), with(&[], &["App.Builder"])));
        assert!(!changed(with(&["A"], &["S"]), with(&["A"], &["S"])));
    }

    fn node(kind: NodeKind, name: &str, qualified_name: &str, line: i64) -> Node {
        Node {
            id: format!("{kind:?}:{qualified_name}:{line}"),
            kind,
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            file_path: "src/Owner.cs".to_string(),
            language: Language::CSharp,
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: Some("public".to_string()),
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

    #[test]
    fn an_owner_flipping_between_a_type_and_a_namespace_touches_the_nested_name() {
        let nested = node(NodeKind::Class, "Nested", "Owner::Nested", 3);
        let as_class = vec![node(NodeKind::Class, "Owner", "Owner", 1), nested.clone()];
        let as_namespace = vec![node(NodeKind::Namespace, "Owner", "Owner", 1), nested];
        assert_eq!(
            owner_flips(&as_class, &as_namespace),
            BTreeSet::from(["Nested".to_string()])
        );
        assert!(owner_flips(&as_class, &as_class).is_empty());
    }

    #[test]
    fn rows_round_trip() {
        let row = CsharpSurface {
            globals: vec!["Unit.Support".to_string()],
            global_statics: vec!["Unit.Support.Some".to_string()],
            heads: vec![head("Reader", &["Base", "IDisposable"])],
        };
        assert_eq!(CsharpSurface::decode(&row.encode()), Some(row));
        assert_eq!(CsharpSurface::decode("not json"), None);
    }
}
