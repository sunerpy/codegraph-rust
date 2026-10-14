//! What a Go call reads of other files' type declarations (W4-GO-01,
//! [`codegraph_resolve::go_packages`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A receiver typed by its declaration reaches the methods its declaring
//! package has, then those of the types its declaration embeds, read from
//! that declaration's text; a field chain `s.db.CreateItem()` reaches the
//! method on the type the field's declaration names (upstream #2361,
//! #1276). The text is gone once a file changes on disk, so every full index
//! and migration writes a row per Go file with an entry per type
//! declaration ([`HeadRow`]: what it embeds, every name it mentions, a digest
//! of its text). A sync compares a changed file's row with its new text: a
//! type whose entries changed, and every name its old or new declaration
//! mentions, seed the embedding closures above them, before the change and
//! after it. Every method of a type of those closures is touched for the
//! name lane, and the unchanged Go files holding an unresolved call written
//! with a receiver ending in one re-resolve whole, however many there are.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Go file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::go_packages::{HEAD_ROW_KEY_PREFIX, HeadEntry, HeadRow, head_row_key};
use codegraph_store::Store;

/// Metadata key present when no sync left a Go dependent unrefreshed.
pub const VERSION_KEY: &str = "go_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no Go
/// file, declares no type, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Go {
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

/// Whether this index needs every Go file re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Go dependent is current: a full index, a migration, or a
/// sync whose resolution is persisted.
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

/// What a sync's changed files held for the Go lane.
#[derive(Debug, Default)]
pub(crate) struct GoChanges {
    changes: BTreeMap<String, Change>,
}

impl GoChanges {
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

/// What a sync's Go lane calls for.
#[derive(Debug, Default)]
pub(crate) struct GoRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane: those of the methods of the types
    /// whose embeddings, or whose fields' and embeddings' types, moved.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl GoRefresh {
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

/// Each type name's entries of `row`, sorted.
fn entries_by_name(row: Option<&HeadRow>) -> BTreeMap<&str, Vec<&HeadEntry>> {
    let mut out: BTreeMap<&str, Vec<&HeadEntry>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry(entry.name.as_str()).or_default().push(entry);
    }
    out
}

/// The type names a changed file's rows move: each type whose entries
/// changed (it came, went, or its declaration's text did), and every name
/// its old or new declaration mentions.
fn seeds(changes: &GoChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let old = entries_by_name(change.old_row.as_ref());
        let new = entries_by_name(change.new_row.as_ref());
        for name in old.keys().chain(new.keys()) {
            let (before, after) = (old.get(name), new.get(name));
            if before == after {
                continue;
            }
            seeds.insert((*name).to_string());
            for entry in before.into_iter().chain(after).flatten() {
                seeds.extend(entry.mentions.iter().cloned());
            }
        }
    }
    seeds
}

/// What the type entries of `rows` embed, by type name.
fn embeddings<'r>(rows: impl IntoIterator<Item = &'r HeadRow>) -> HashMap<&'r str, Vec<&'r str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            let embeds = out.entry(entry.name.as_str()).or_default();
            for embedded in &entry.embeds {
                if !embeds.contains(&embedded.as_str()) {
                    embeds.push(embedded.as_str());
                }
            }
        }
    }
    out
}

/// `seeds` and every type they embed, transitively, by name.
fn closure(embeds: &HashMap<&str, Vec<&str>>, seeds: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        if reached.contains(&name) {
            continue;
        }
        if let Some(embedded) = embeds.get(name.as_str()) {
            queue.extend(embedded.iter().map(|s| (*s).to_string()));
        }
        reached.insert(name);
    }
    reached
}

/// Read what the Go lane calls for, after every changed file is re-extracted
/// (the store holds the new nodes; the rows are still the old ones). With
/// `bootstrap`, every row is rebuilt from disk instead and every Go file
/// re-resolves.
pub(crate) fn go_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &GoChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<GoRefresh> {
    let mut refresh = GoRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Go {
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
    let stored: Vec<HeadRow> = store
        .project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?
        .into_iter()
        .filter(|(key, _)| {
            key.strip_prefix(HEAD_ROW_KEY_PREFIX)
                .is_some_and(|path| !changes.changes.contains_key(path))
        })
        .filter_map(|(_, value)| HeadRow::decode(&value))
        .collect();
    let before = embeddings(
        stored.iter().chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.old_row.as_ref()),
        ),
    );
    let after = embeddings(
        stored.iter().chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.new_row.as_ref()),
        ),
    );
    let mut types = closure(&before, &seeds);
    types.extend(closure(&after, &seeds));
    // The methods of each type, wherever in its package they are declared.
    for name in &types {
        for node in store.nodes_with_qualified_prefix(&format!("{name}::"))? {
            if node.language == Language::Go && node.kind == NodeKind::Method {
                refresh.names.insert(node.name);
            }
        }
    }
    // A call written with a receiver waits for its tail, which the shared
    // tail lookup caps: the unchanged files holding one re-resolve whole.
    if !refresh.names.is_empty() {
        for reference in store.unresolved_refs_of_language(Language::Go)? {
            if reference
                .reference_name
                .rsplit_once('.')
                .is_some_and(|(_, tail)| refresh.names.contains(tail))
                && unchanged(&reference.file_path)
            {
                refresh.files.insert(reference.file_path);
            }
        }
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, embeds: &[&str], mentions: &[&str], digest: &str) -> HeadEntry {
        HeadEntry {
            name: name.to_string(),
            embeds: embeds.iter().map(|s| s.to_string()).collect(),
            mentions: mentions.iter().map(|s| s.to_string()).collect(),
            digest: digest.to_string(),
            imports: Vec::new(),
        }
    }

    fn row(entries: Vec<HeadEntry>) -> HeadRow {
        let mut entries = entries;
        entries.sort();
        HeadRow { entries }
    }

    fn change(old: Option<HeadRow>, new: Option<HeadRow>) -> GoChanges {
        let mut changes = GoChanges::default();
        changes.changes.insert(
            "internal/handlers/server.go".to_string(),
            Change {
                old_row: old,
                new_row: new,
            },
        );
        changes
    }

    #[test]
    fn a_retyped_field_seeds_the_type_and_what_it_mentions() {
        let retyped = change(
            Some(row(vec![entry("server", &[], &["server", "store"], "a")])),
            Some(row(vec![entry("server", &[], &["cache", "server"], "b")])),
        );
        assert_eq!(
            seeds(&retyped),
            BTreeSet::from(["cache", "server", "store"].map(str::to_string))
        );
        let same = change(
            Some(row(vec![entry("server", &[], &["server"], "a")])),
            Some(row(vec![entry("server", &[], &["server"], "a")])),
        );
        assert!(seeds(&same).is_empty());
    }

    #[test]
    fn the_closure_follows_embeddings() {
        let rows = [
            row(vec![entry("server", &["BaseAPI"], &[], "a")]),
            row(vec![entry("BaseAPI", &["Logger"], &[], "b")]),
            row(vec![entry("Other", &["Unrelated"], &[], "c")]),
        ];
        let embeds = embeddings(rows.iter());
        assert_eq!(
            closure(&embeds, &BTreeSet::from(["server".to_string()])),
            BTreeSet::from(["BaseAPI", "Logger", "server"].map(str::to_string))
        );
    }

    #[test]
    fn only_go_files_with_a_type_have_rows() {
        let text = SourceText::Text("package x\n\nfunc f() {}\n".to_string());
        assert!(row_of(Language::Go, &text, &[]).is_none());
        assert!(row_of(Language::Rust, &text, &[]).is_none());
    }
}
