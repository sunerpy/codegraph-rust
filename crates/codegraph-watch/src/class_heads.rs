//! What a call through a class name reads of the supertypes class heads name
//! (W5-07, upstream #2180, [`codegraph_resolve::class_heads`]), so that a
//! sync re-resolves an unchanged file as a fresh index would.
//!
//! `User.create()` reaches the method `User` inherits, walking at most five
//! levels up the supertypes each declaration's head names, read from source
//! (`declared_supertypes`). That text is gone once a file changes on disk,
//! so every full index, migration and sync writes a row per file declaring a
//! type such a walk reads, with an entry per declaration: its language, its
//! name and the supertypes its own head writes
//! ([`codegraph_resolve::class_heads::declaration_heads`]). A sync compares
//! each changed file's row with its new text; the type names whose entries
//! came, went or changed are the seeds. For each seed:
//!
//! - the receivers whose walk reads the seed's head are the seed and the
//!   types below it, at most four levels down the rows' heads, before or
//!   after the change;
//! - the targets such a walk reaches through the seed are its supertypes at
//!   most five levels up, before or after;
//! - the edges to a method named like a target's re-resolve at their sites,
//!   cross-file and same-file;
//! - the unresolved references written through a receiver (`X.m`, `X::m`)
//!   are retried at their sites, however many there are (the shared tail
//!   lookup caps them).
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every file such a walk reads once and
//! rebuilds the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
pub use codegraph_resolve::class_heads::{HeadEntry, HeadsRow, ROW_KEY_PREFIX, row_key};
use codegraph_resolve::class_heads::{declaration_heads, name_supertypes};
use codegraph_resolve::source_facts::SourceFacts;
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

/// Metadata key present when no sync left a class-name call unrefreshed.
pub const VERSION_KEY: &str = "class_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// How many levels up a walk from a class name goes (`inheritedClassMethod`).
const WALK_LEVELS: usize = 5;

/// The type an entry declares, as a walk reads it.
fn entry_key(entry: &HeadEntry) -> TypeKey {
    (entry.language.as_str(), entry.name.clone())
}

/// Whether a file of `language` can declare a type a walk from a class name
/// reads (the languages `declared_supertypes` reads, and the component files
/// whose scripts declare TS/JS types).
fn may_declare(language: Language) -> bool {
    matches!(
        language,
        Language::Java
            | Language::CSharp
            | Language::Kotlin
            | Language::Dart
            | Language::Swift
            | Language::ObjC
            | Language::Scala
            | Language::Pascal
            | Language::Python
            | Language::Ruby
            | Language::Php
            | Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
            | Language::Vue
            | Language::Svelte
            | Language::Astro
    )
}

/// `source`'s row, `None` when it records nothing: a file no walk reads, one
/// declaring no type, or one that is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadsRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadsRow> {
    if !may_declare(language) {
        return None;
    }
    let facts = SourceFacts::new(Arc::from(text));
    let mut entries: Vec<HeadEntry> = nodes
        .iter()
        .filter_map(|node| {
            declaration_heads(&facts, node).map(|heads| HeadEntry {
                language: node.language,
                name: node.name.clone(),
                heads,
            })
        })
        .collect();
    entries.sort();
    (!entries.is_empty()).then_some(HeadsRow { entries })
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, HeadsRow)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every such file re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every class-name call is current: a full index, a migration,
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

fn read_row(store: &Store, path: &str) -> Result<Option<HeadsRow>> {
    Ok(store
        .get_project_metadata(&row_key(path))?
        .and_then(|value| HeadsRow::decode(&value)))
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<HeadsRow>,
    new_row: Option<HeadsRow>,
}

/// What a sync's changed files held for this lane.
#[derive(Debug, Default)]
pub(crate) struct ClassHeadChanges {
    changes: BTreeMap<String, Change>,
}

impl ClassHeadChanges {
    /// `path` changed (its old row still in the store) to `new` (its
    /// language, text and nodes), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let new_row = new.and_then(|(language, source, nodes)| row_of(language, source, nodes));
        if let Some(change) = self.changes.get_mut(path) {
            // A file changed twice in one sync keeps what it held before the
            // first change.
            change.new_row = new_row;
            return Ok(());
        }
        let old_row = read_row(store, path)?;
        if old_row.is_none() && new_row.is_none() {
            return Ok(());
        }
        self.changes
            .insert(path.to_string(), Change { old_row, new_row });
        Ok(())
    }
}

/// What a sync's class-head lane calls for.
#[derive(Debug, Default)]
pub(crate) struct ClassHeadRefresh {
    /// Unchanged files to re-resolve whole (a rebuild).
    pub(crate) files: BTreeSet<String>,
    /// The names of the methods a walk through a seed can reach, before or
    /// after: the edges to methods of these names re-resolve at their sites.
    pub(crate) names: BTreeSet<String>,
    /// Unresolved references written through a receiver whose walk reads a
    /// seed's head, to retry at their sites.
    pub(crate) sites: Vec<FileReferenceSite>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadsRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl ClassHeadRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = row_key(path);
            match row {
                Some(row) if !row.entries.is_empty() => {
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

/// A type as a walk reads it: its language (by name) and its name.
type TypeKey = (&'static str, String);

/// What `row` holds per type: the heads of each of its declarations, sorted.
fn heads_by_type(row: Option<&HeadsRow>) -> BTreeMap<TypeKey, Vec<&[String]>> {
    let mut out: BTreeMap<TypeKey, Vec<&[String]>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry(entry_key(entry))
            .or_default()
            .push(entry.heads.as_slice());
    }
    for heads in out.values_mut() {
        heads.sort();
    }
    out
}

/// The types whose declarations a changed file's rows move: one that came,
/// went, or whose head names other supertypes.
fn seeds(changes: &ClassHeadChanges) -> BTreeSet<TypeKey> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let old = heads_by_type(change.old_row.as_ref());
        let new = heads_by_type(change.new_row.as_ref());
        for key in old.keys().chain(new.keys()) {
            if old.get(key) != new.get(key) {
                seeds.insert(key.clone());
            }
        }
    }
    seeds
}

/// Each type's supertypes, or each type's subtypes.
type Links = HashMap<TypeKey, BTreeSet<TypeKey>>;

/// The supertypes a walk reads for each type the `rows` declare, the name's
/// own supertypes included ([`name_supertypes`]) (`up`), and the types
/// naming each as one (`down`).
fn links<'r>(rows: impl IntoIterator<Item = &'r HeadsRow>) -> (Links, Links) {
    let mut written: BTreeMap<TypeKey, (Language, BTreeSet<String>)> = BTreeMap::new();
    for row in rows {
        for entry in &row.entries {
            written
                .entry(entry_key(entry))
                .or_insert_with(|| (entry.language, BTreeSet::new()))
                .1
                .extend(entry.heads.iter().cloned());
        }
    }
    let mut up: Links = HashMap::new();
    let mut down: Links = HashMap::new();
    for ((language, name), (typed, heads)) in &written {
        let extra = name_supertypes(*typed, name, !heads.is_empty());
        for head in heads.iter().chain(&extra) {
            let from = (*language, name.clone());
            let to = (*language, head.clone());
            up.entry(from.clone()).or_default().insert(to.clone());
            down.entry(to).or_default().insert(from);
        }
    }
    (up, down)
}

/// The types `links` reaches from `start` in one to `levels` steps (`start`
/// itself only when a cycle leads back to it).
fn reached(links: &Links, start: &TypeKey, levels: usize) -> BTreeSet<TypeKey> {
    let mut out = BTreeSet::new();
    let mut seen = BTreeSet::from([start.clone()]);
    let mut queue: VecDeque<(TypeKey, usize)> = VecDeque::from([(start.clone(), 0)]);
    while let Some((key, depth)) = queue.pop_front() {
        if depth == levels {
            continue;
        }
        for next in links.get(&key).into_iter().flatten() {
            out.insert(next.clone());
            if seen.insert(next.clone()) {
                queue.push_back((next.clone(), depth + 1));
            }
        }
    }
    out
}

/// The simple name of the type a member belongs to: the last `::` / `.`
/// segment before the member's own (`THorseCore` of `THorseCore::Get`).
fn member_owner_name(qualified_name: &str) -> &str {
    let Some(cut) = qualified_name.rfind("::").filter(|&cut| cut > 0) else {
        return "";
    };
    qualified_name[..cut]
        .rsplit("::")
        .next()
        .and_then(|segment| segment.rsplit('.').next())
        .unwrap_or_default()
}

/// Read what this lane calls for, after every changed file is re-extracted
/// (the store holds the new nodes; the rows are still the old ones). With
/// `bootstrap`, every row is rebuilt from disk instead and every file a walk
/// reads re-resolves.
pub(crate) fn class_head_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &ClassHeadChanges,
    reindexed: &std::collections::HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<ClassHeadRefresh> {
    let mut refresh = ClassHeadRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if !may_declare(file.language) {
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
        refresh.rows.insert(path.clone(), change.new_row.clone());
    }
    let seeds = seeds(changes);
    if seeds.is_empty() {
        return Ok(refresh);
    }
    let stored: Vec<(String, HeadsRow)> = store
        .project_metadata_with_prefix(ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            let path = key.strip_prefix(ROW_KEY_PREFIX)?;
            if changes.changes.contains_key(path) {
                return None;
            }
            Some((path.to_string(), HeadsRow::decode(&value)?))
        })
        .collect();
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
    let (up_before, down_before) = links(before_rows());
    let (up_after, down_after) = links(after_rows());
    let mut receivers: BTreeSet<TypeKey> = BTreeSet::new();
    let mut targets: BTreeSet<TypeKey> = BTreeSet::new();
    for seed in &seeds {
        receivers.insert(seed.clone());
        for down in [&down_before, &down_after] {
            receivers.extend(reached(down, seed, WALK_LEVELS - 1));
        }
        for up in [&up_before, &up_after] {
            targets.extend(reached(up, seed, WALK_LEVELS));
        }
    }
    // The methods of the targets, in the files declaring one now (the store
    // holds a changed file's new nodes; a removed declaration's methods took
    // their edges with them, and those re-resolve with the file's dependents).
    let declaring: BTreeSet<&str> =
        stored
            .iter()
            .map(|(path, row)| (path.as_str(), row))
            .chain(changes.changes.iter().filter_map(|(path, change)| {
                change.new_row.as_ref().map(|row| (path.as_str(), row))
            }))
            .filter(|(_, row)| {
                row.entries
                    .iter()
                    .any(|entry| targets.contains(&entry_key(entry)))
            })
            .map(|(path, _)| path)
            .collect();
    for path in declaring {
        for node in store.nodes_by_file_path(path)? {
            if node.kind == NodeKind::Method
                && targets.contains(&(
                    node.language.as_str(),
                    member_owner_name(&node.qualified_name).to_string(),
                ))
            {
                refresh.names.insert(node.name);
            }
        }
    }
    // A call written through a receiver waits for its method's name, which
    // the shared tail lookup caps at 500 per tail; the ones whose walk reads
    // a seed's head are retried at their sites, however many there are.
    let by_name: BTreeMap<&str, BTreeSet<&str>> =
        receivers
            .iter()
            .fold(BTreeMap::new(), |mut out, (language, name)| {
                out.entry(name.as_str()).or_default().insert(*language);
                out
            });
    let names: Vec<String> = by_name.keys().map(|name| (*name).to_string()).collect();
    for reference in store.unresolved_refs_by_receivers(&names)? {
        let receiver = reference
            .reference_name
            .split_once("::")
            .map(|(head, _)| head)
            .filter(|head| !head.contains('.'))
            .or_else(|| {
                reference
                    .reference_name
                    .split_once('.')
                    .map(|(head, _)| head)
            })
            .unwrap_or_default();
        if unchanged(&reference.file_path)
            && by_name
                .get(receiver)
                .is_some_and(|languages| languages.contains(reference.language.as_str()))
        {
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
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(language: Language, name: &str, heads: &[&str]) -> HeadEntry {
        HeadEntry {
            language,
            name: name.to_string(),
            heads: heads.iter().map(|head| (*head).to_string()).collect(),
        }
    }

    fn key(name: &str) -> TypeKey {
        (Language::Python.as_str(), name.to_string())
    }

    #[test]
    fn a_walk_reaches_five_levels_up_and_four_levels_down() {
        let row = HeadsRow {
            entries: (0..8)
                .map(|level| {
                    entry(
                        Language::Python,
                        &format!("T{level}"),
                        &[&format!("T{}", level + 1)],
                    )
                })
                .collect(),
        };
        let (up, down) = links([&row]);
        let up_from_t0: Vec<String> = reached(&up, &key("T0"), WALK_LEVELS)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(up_from_t0, ["T1", "T2", "T3", "T4", "T5"]);
        let down_from_t5: Vec<String> = reached(&down, &key("T5"), WALK_LEVELS - 1)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(down_from_t5, ["T1", "T2", "T3", "T4"]);
    }

    #[test]
    fn a_seed_is_a_type_whose_declarations_came_went_or_changed_heads() {
        let mut changes = ClassHeadChanges::default();
        changes.changes.insert(
            "a.py".to_string(),
            Change {
                old_row: Some(HeadsRow {
                    entries: vec![
                        entry(Language::Python, "Kept", &["Base"]),
                        entry(Language::Python, "Moved", &["Base"]),
                        entry(Language::Python, "Gone", &[]),
                    ],
                }),
                new_row: Some(HeadsRow {
                    entries: vec![
                        entry(Language::Python, "Kept", &["Base"]),
                        entry(Language::Python, "Moved", &["Other"]),
                        entry(Language::Python, "New", &[]),
                    ],
                }),
            },
        );
        let names: Vec<String> = seeds(&changes).into_iter().map(|(_, name)| name).collect();
        assert_eq!(names, ["Gone", "Moved", "New"]);
    }

    #[test]
    fn a_receiver_and_its_method_split_at_the_first_separator() {
        assert_eq!(member_owner_name("app::Base::create"), "Base");
        assert_eq!(member_owner_name("pkg.base.Base::create"), "Base");
        assert_eq!(member_owner_name("create"), "");
    }
}
