//! What a bare Solidity call reads of other files (W4-SOL-01,
//! [`codegraph_resolve::solidity_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A call written without a receiver reaches a function of the contract
//! around it or of one it inherits: the `is` clauses of every declaration of
//! each name the chain passes, read from their heads (upstream #2178). The
//! text is gone once a file changes on disk, so every full index and
//! migration writes a row per Solidity file with a type ([`HeadRow`]): each
//! type's name and the bases its head names. A sync compares a changed
//! file's row with its new text: the names whose entries changed (a type
//! came, went or inherits otherwise) seed a closure over every type whose
//! head names a reached name. In an unchanged file declaring a type of the
//! closure, only a call to a method of a type that entered or left the chain
//! of a type the file declares can move: those calls, resolved or parked,
//! are retried at their sites. A chain at the resolver's limit of 60 names,
//! which the rows cannot read in the resolver's order, re-resolves its file
//! whole.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Solidity file once and rebuilds
//! the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{EdgeKind, Language, Node};
use codegraph_resolve::solidity_scope::{
    HEAD_ROW_KEY_PREFIX, HIERARCHY_LIMIT, HeadRow, head_row_key, solidity_method_owner,
};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

/// Metadata key present when no sync left a Solidity dependent unrefreshed.
pub const VERSION_KEY: &str = "solidity_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// Solidity file, declares no type, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Solidity {
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

/// Whether this index needs every Solidity file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Solidity dependent is current: a full index, a
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

/// What a sync's changed files held for the Solidity lane.
#[derive(Debug, Default)]
pub(crate) struct SolidityChanges {
    changes: BTreeMap<String, Change>,
}

impl SolidityChanges {
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
                ..Change::default()
            });
        change.new_row = new_row;
        Ok(())
    }
}

/// What a sync's Solidity lane calls for.
#[derive(Debug, Default)]
pub(crate) struct SolidityRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Calls in unchanged files to retry at their sites.
    pub(crate) sites: Vec<FileReferenceSite>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl SolidityRefresh {
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

/// A row's bases by type name, one list per declaration of the name.
fn entries_by_name(row: Option<&HeadRow>) -> BTreeMap<&str, Vec<&Vec<String>>> {
    let mut by_name: BTreeMap<&str, Vec<&Vec<String>>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.types) {
        by_name.entry(&entry.name).or_default().push(&entry.supers);
    }
    by_name
}

/// The names whose entries a sync changed: a type of the name came, went or
/// names other bases in its head.
fn seeds(changes: &SolidityChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let (old, new) = (
            entries_by_name(change.old_row.as_ref()),
            entries_by_name(change.new_row.as_ref()),
        );
        for name in old.keys().chain(new.keys()) {
            if old.get(name) != new.get(name) {
                seeds.insert((*name).to_string());
            }
        }
    }
    seeds
}

/// The files (of `rows`, by path) declaring a reached name, from `seeds`
/// down through every type whose head names a reached name.
fn closure<'r>(
    rows: impl IntoIterator<Item = (&'r str, &'r HeadRow)>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut declaring: HashMap<&'r str, Vec<&'r str>> = HashMap::new();
    let mut inheriting: HashMap<&'r str, Vec<&'r str>> = HashMap::new();
    for (path, row) in rows {
        for entry in &row.types {
            declaring.entry(&entry.name).or_default().push(path);
            for base in &entry.supers {
                inheriting.entry(base).or_default().push(&entry.name);
            }
        }
    }
    let mut reached = seeds.clone();
    let mut queue: Vec<String> = seeds.into_iter().collect();
    let mut files = BTreeSet::new();
    while let Some(name) = queue.pop() {
        for path in declaring.get(name.as_str()).into_iter().flatten() {
            files.insert((*path).to_string());
        }
        for sub in inheriting.get(name.as_str()).into_iter().flatten() {
            if reached.insert((*sub).to_string()) {
                queue.push((*sub).to_string());
            }
        }
    }
    files
}

/// Read what the Solidity lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Solidity file re-resolves.
pub(crate) fn solidity_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &SolidityChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<SolidityRefresh> {
    let mut refresh = SolidityRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Solidity {
                continue;
            }
            if unchanged(&file.path) {
                refresh.files.insert(file.path.clone());
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
    let state = |new: bool| {
        stored.iter().map(|(path, row)| (path.as_str(), row)).chain(
            changes.changes.iter().filter_map(move |(path, change)| {
                if new {
                    change.new_row.as_ref()
                } else {
                    change.old_row.as_ref()
                }
                .map(|row| (path.as_str(), row))
            }),
        )
    };
    let (before, after) = (supers_by_name(state(false)), supers_by_name(state(true)));
    // A type whose bases changed is a seed itself, so the types below a
    // seed after the change are every one whose chain can have moved.
    let files = closure(state(true), seeds);
    let declaring = declaring_files(state(true));
    let mut owned: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (path, row) in &stored {
        if !files.contains(path) || !unchanged(path) {
            continue;
        }
        let mut moved: BTreeSet<String> = BTreeSet::new();
        let mut whole = false;
        for entry in &row.types {
            match (
                hierarchy(&entry.name, &before),
                hierarchy(&entry.name, &after),
            ) {
                (Some(old), Some(new)) => {
                    moved.extend(old.symmetric_difference(&new).cloned());
                }
                _ => whole = true,
            }
        }
        if whole {
            refresh.files.insert(path.clone());
            continue;
        }
        if moved.is_empty() {
            continue;
        }
        let mut methods: BTreeSet<String> = BTreeSet::new();
        for owner in &moved {
            if !owned.contains_key(owner) {
                let names = methods_of(store, owner, declaring.get(owner.as_str()))?;
                owned.insert(owner.clone(), names);
            }
            methods.extend(owned[owner].iter().cloned());
        }
        if methods.is_empty() {
            continue;
        }
        let site = |from_node_id: String, line: i64, col: i64| FileReferenceSite {
            file_path: path.clone(),
            site: Some(ReferenceSite {
                from_node_id,
                line,
                col,
            }),
        };
        for (source, line, col, target) in store.call_sites_in_file(path)? {
            if methods.contains(&target) {
                refresh.sites.push(site(source, line, col));
            }
        }
        for reference in store.unresolved_refs_by_file_path(path)? {
            if reference.reference_kind == EdgeKind::Calls
                && methods.contains(&reference.reference_name)
            {
                refresh
                    .sites
                    .push(site(reference.from_node_id, reference.line, reference.col));
            }
        }
    }
    Ok(refresh)
}

/// Every name's bases in `rows`, each declaration of it counted, as the
/// resolver reads them by name.
fn supers_by_name<'r>(
    rows: impl IntoIterator<Item = (&'r str, &'r HeadRow)>,
) -> HashMap<&'r str, Vec<&'r str>> {
    let mut supers: HashMap<&'r str, Vec<&'r str>> = HashMap::new();
    for (_, row) in rows {
        for entry in &row.types {
            supers
                .entry(entry.name.as_str())
                .or_default()
                .extend(entry.supers.iter().map(String::as_str));
        }
    }
    supers
}

/// The files declaring each type name of `rows`.
fn declaring_files<'r>(
    rows: impl IntoIterator<Item = (&'r str, &'r HeadRow)>,
) -> HashMap<&'r str, BTreeSet<&'r str>> {
    let mut declaring: HashMap<&'r str, BTreeSet<&'r str>> = HashMap::new();
    for (path, row) in rows {
        for entry in &row.types {
            declaring
                .entry(entry.name.as_str())
                .or_default()
                .insert(path);
        }
    }
    declaring
}

/// The names a bare call's chain from the type `name` reaches, as the
/// resolver walks it; `None` at its limit of 60 names, past which the
/// resolver's walk order, which the rows do not keep, decides what it holds.
fn hierarchy(name: &str, supers: &HashMap<&str, Vec<&str>>) -> Option<BTreeSet<String>> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([name]);
    while let Some(name) = queue.pop_front() {
        if names.contains(name) {
            continue;
        }
        if names.len() >= HIERARCHY_LIMIT {
            return None;
        }
        names.insert(name.to_string());
        queue.extend(supers.get(name).into_iter().flatten().copied());
    }
    Some(names)
}

/// The names of the methods the Solidity types named `owner` declare, read
/// from the files `files` the rows give for it.
fn methods_of(
    store: &Store,
    owner: &str,
    files: Option<&BTreeSet<&str>>,
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for file in files.into_iter().flatten() {
        for node in store.nodes_by_file_path(file)? {
            if solidity_method_owner(&node) == Some(owner) {
                names.insert(node.name.clone());
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_resolve::solidity_scope::HeadEntry;

    fn entry(name: &str, supers: &[&str]) -> HeadEntry {
        HeadEntry {
            name: name.to_string(),
            supers: supers.iter().map(|base| base.to_string()).collect(),
        }
    }

    fn row(types: Vec<HeadEntry>) -> HeadRow {
        HeadRow { types }
    }

    fn change(old: Option<HeadRow>, new: Option<HeadRow>) -> SolidityChanges {
        let mut changes = SolidityChanges::default();
        changes.changes.insert(
            "contracts/Base.sol".to_string(),
            Change {
                old_row: old,
                new_row: new,
            },
        );
        changes
    }

    #[test]
    fn a_type_that_inherits_otherwise_comes_or_goes_seeds_its_name() {
        let rebased = change(
            Some(row(vec![entry("Base", &[]), entry("Other", &["X"])])),
            Some(row(vec![entry("Base", &["Root"]), entry("Other", &["X"])])),
        );
        assert_eq!(seeds(&rebased), BTreeSet::from(["Base".to_string()]));
        let same = change(
            Some(row(vec![entry("Base", &["Root"])])),
            Some(row(vec![entry("Base", &["Root"])])),
        );
        assert!(seeds(&same).is_empty());
        let added = change(None, Some(row(vec![entry("Base", &[])])));
        assert_eq!(seeds(&added), BTreeSet::from(["Base".to_string()]));
        let removed = change(Some(row(vec![entry("Base", &[])])), None);
        assert_eq!(seeds(&removed), BTreeSet::from(["Base".to_string()]));
    }

    #[test]
    fn the_closure_reaches_every_file_declaring_a_type_below_a_seed() {
        let rows = [
            (
                "contracts/Middle.sol",
                row(vec![entry("Middle", &["Base"])]),
            ),
            (
                "contracts/Leaf.sol",
                row(vec![entry("Leaf", &["Middle", "Other"])]),
            ),
            ("contracts/Twin.sol", row(vec![entry("Base", &[])])),
            ("contracts/Other.sol", row(vec![entry("Other", &[])])),
        ];
        let files = closure(
            rows.iter().map(|(path, row)| (*path, row)),
            BTreeSet::from(["Base".to_string()]),
        );
        assert_eq!(
            files,
            BTreeSet::from([
                "contracts/Leaf.sol".to_string(),
                "contracts/Middle.sol".to_string(),
                "contracts/Twin.sol".to_string(),
            ])
        );
    }

    #[test]
    fn a_chain_is_walked_by_name_and_given_up_at_the_resolvers_limit() {
        let rows = [
            ("contracts/A.sol", row(vec![entry("A", &["B", "C"])])),
            ("contracts/B.sol", row(vec![entry("B", &["C"])])),
            ("contracts/C.sol", row(vec![entry("C", &[])])),
            ("contracts/Twin.sol", row(vec![entry("C", &["D"])])),
        ];
        let supers = supers_by_name(rows.iter().map(|(path, row)| (*path, row)));
        assert_eq!(
            hierarchy("A", &supers),
            Some(BTreeSet::from(["A", "B", "C", "D"].map(String::from)))
        );
        let names: Vec<String> = (0..=HIERARCHY_LIMIT).map(|i| format!("T{i}")).collect();
        let chain: Vec<HeadRow> = names
            .windows(2)
            .map(|pair| row(vec![entry(&pair[0], &[pair[1].as_str()])]))
            .collect();
        let deep = supers_by_name(chain.iter().map(|row| ("contracts/Deep.sol", row)));
        assert_eq!(hierarchy("T0", &deep), None);
        assert_eq!(
            hierarchy("T1", &deep).map(|names| names.len()),
            Some(HIERARCHY_LIMIT)
        );
    }

    #[test]
    fn only_solidity_files_with_a_type_have_rows() {
        let text = SourceText::Text("pragma solidity ^0.8.20;\n".to_string());
        assert!(row_of(Language::Solidity, &text, &[]).is_none());
        assert!(row_of(Language::Ruby, &text, &[]).is_none());
    }
}
