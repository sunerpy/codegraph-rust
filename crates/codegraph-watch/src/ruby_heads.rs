//! What a bare Ruby call reads of other files (W4-RB-01,
//! [`codegraph_resolve::ruby_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare call written inside a class reaches the class's ancestry: the
//! superclasses and mixed-in modules its declarations' source names, each
//! written constant resolved against the lexical nesting to a class or
//! module the project declares (upstream #2147). The walk reads, by
//! qualified name, the first line and the body lines of every declaration it
//! passes. The text is gone once a file changes on disk, so every full index
//! and migration writes a row per Ruby file with what its declarations name
//! ([`HeadRow`]). A sync compares a changed file's row with its new text:
//! the qualified names whose entries changed (a declaration came, went or
//! names other constants) seed a closure over every declaration writing a
//! constant whose last segment is a reached name's, and the unchanged files
//! declaring a class or module of the closure re-resolve whole.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Ruby file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::ruby_scope::{HEAD_ROW_KEY_PREFIX, HeadEntry, HeadRow, head_row_key};
use codegraph_store::Store;

/// Metadata key present when no sync left a Ruby dependent unrefreshed.
pub const VERSION_KEY: &str = "ruby_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// Ruby file, declares no class or module, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Ruby {
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

/// Whether this index needs every Ruby file re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Ruby dependent is current: a full index, a migration,
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

/// What a sync's changed files held for the Ruby lane.
#[derive(Debug, Default)]
pub(crate) struct RubyChanges {
    changes: BTreeMap<String, Change>,
}

impl RubyChanges {
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

/// What a sync's Ruby lane calls for.
#[derive(Debug, Default)]
pub(crate) struct RubyRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl RubyRefresh {
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

/// Each qualified name's entries of `row`, in file order.
fn entries_by_name(row: Option<&HeadRow>) -> BTreeMap<&str, Vec<&HeadEntry>> {
    let mut out: BTreeMap<&str, Vec<&HeadEntry>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry(entry.qualified_name.as_str())
            .or_default()
            .push(entry);
    }
    out
}

/// The qualified names whose entries a changed file's rows differ in: a
/// declaration came or went (so a written constant may now resolve
/// elsewhere), names other constants, or same-named entries changed places.
fn seeds(changes: &RubyChanges) -> BTreeSet<String> {
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

/// A constant's last segment: what a written name and a qualified name are
/// compared by, so the closure never misses a resolution the lexical nesting
/// picks.
fn last_segment(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// `seeds` and the qualified name of every declaration (every unchanged
/// file's row and each changed file's new one) writing a constant whose last
/// segment is a reached name's, until none is added.
fn closure<'r>(
    rows: impl IntoIterator<Item = &'r HeadRow>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut naming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            for written in entry.head.names() {
                naming
                    .entry(last_segment(written))
                    .or_default()
                    .insert(entry.qualified_name.as_str());
            }
        }
    }
    let mut reached = seeds.clone();
    let mut queue: VecDeque<String> = seeds.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        for deriving in naming.get(last_segment(&name)).into_iter().flatten() {
            if reached.insert((*deriving).to_string()) {
                queue.push_back((*deriving).to_string());
            }
        }
    }
    reached
}

/// Read what the Ruby lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Ruby file re-resolves.
pub(crate) fn ruby_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &RubyChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<RubyRefresh> {
    let mut refresh = RubyRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Ruby {
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
        for node in store.nodes_by_qualified_name(name)? {
            if node.language == Language::Ruby
                && matches!(node.kind, NodeKind::Class | NodeKind::Module)
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
    use codegraph_resolve::ruby_scope::RubyHead;

    fn entry(qualified_name: &str, superclass: Option<&str>, mixins: &[&str]) -> HeadEntry {
        HeadEntry {
            qualified_name: qualified_name.to_string(),
            kind: "class".to_string(),
            head: RubyHead {
                superclass: superclass.map(str::to_string),
                mixins: mixins.iter().map(|m| m.to_string()).collect(),
            },
        }
    }

    fn change(old: Vec<HeadEntry>, new: Vec<HeadEntry>) -> RubyChanges {
        let mut changes = RubyChanges::default();
        changes.changes.insert(
            "lib/a.rb".to_string(),
            Change {
                old_row: (!old.is_empty()).then_some(HeadRow { entries: old }),
                new_row: (!new.is_empty()).then_some(HeadRow { entries: new }),
            },
        );
        changes
    }

    #[test]
    fn a_changed_head_seeds_its_qualified_name_and_an_unchanged_one_nothing() {
        let changed = change(
            vec![entry("App::Base", None, &[])],
            vec![entry("App::Base", None, &["Helpers"])],
        );
        assert_eq!(seeds(&changed), BTreeSet::from(["App::Base".to_string()]));
        let same = change(
            vec![entry("App::Base", Some("Root"), &[])],
            vec![entry("App::Base", Some("Root"), &[])],
        );
        assert!(seeds(&same).is_empty());
    }

    #[test]
    fn a_declaration_coming_or_going_seeds_its_name() {
        let added = change(
            Vec::new(),
            vec![entry("App::Cop::Style::RangeHelp", None, &[])],
        );
        assert_eq!(
            seeds(&added),
            BTreeSet::from(["App::Cop::Style::RangeHelp".to_string()])
        );
        let removed = change(vec![entry("App::Gone", None, &[])], Vec::new());
        assert_eq!(seeds(&removed), BTreeSet::from(["App::Gone".to_string()]));
    }

    #[test]
    fn the_closure_follows_written_constants_by_their_last_segment() {
        let rows = [
            HeadRow {
                entries: vec![entry("App::Cop::Style::Foo", Some("Base"), &["RangeHelp"])],
            },
            HeadRow {
                entries: vec![entry("App::Leaf", Some("::App::Cop::Style::Foo"), &[])],
            },
            HeadRow {
                entries: vec![entry("Other", Some("Unrelated"), &[])],
            },
        ];
        let reached = closure(&rows, BTreeSet::from(["App::Cop::Base".to_string()]));
        assert_eq!(
            reached,
            BTreeSet::from([
                "App::Cop::Base".to_string(),
                "App::Cop::Style::Foo".to_string(),
                "App::Leaf".to_string(),
            ])
        );
        let mixin = closure(
            &rows,
            BTreeSet::from(["App::Cop::Style::RangeHelp".to_string()]),
        );
        assert!(mixin.contains("App::Cop::Style::Foo"));
        assert!(!mixin.contains("Other"));
    }
}
