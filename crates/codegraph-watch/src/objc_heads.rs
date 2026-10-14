//! What an Objective-C message reached by name reads of other files
//! (W4-SW-04, [`codegraph_resolve::objc_scope`]), so that a sync re-resolves
//! an unchanged file as a fresh index would.
//!
//! A message to `self` or `super` reaches a member of the sender's
//! hierarchy, which the `@interface Name : Super` heads of the classes it
//! passes give (UIKit / AppKit ancestry where none does); an untyped receiver
//! reaches the one method of its name when it names a class, or ends in a
//! property whose `@property … Type *name` declaration gives a type, whose
//! hierarchy has the method's owner (upstream #2150, #2179); and a local
//! typed by its declaration reaches its type's methods, inherited ones too.
//! The heads are read from text that is gone once a file changes on disk, so
//! every full index and migration writes a row per Objective-C file with an
//! entry per class and property ([`HeadRow`]). A sync compares a changed
//! file's row with its new text: the classes whose entries changed, and the
//! types a changed property is and was declared with, seed the hierarchies
//! above them, before the change and after it. Every name a member of a
//! class of those hierarchies has, and every name declared in a file named
//! after one, is touched for the name lane, and the unchanged Objective-C
//! files holding an unresolved message to one written with a receiver
//! re-resolve whole, however many there are: the shared tail lookup reads a
//! keyword selector's tail as empty.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Objective-C file once and rebuilds
//! the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::objc_scope::{
    HEAD_ROW_KEY_PREFIX, HeadRow, file_base, head_row_key, system_superclass,
};
use codegraph_store::Store;

/// Metadata key present when no sync left an Objective-C dependent
/// unrefreshed.
pub const VERSION_KEY: &str = "objc_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// Objective-C file, declares no class or property, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::ObjC {
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

/// Whether this index needs every Objective-C file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Objective-C dependent is current: a full index, a
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

/// What a sync's changed files held for the Objective-C lane.
#[derive(Debug, Default)]
pub(crate) struct ObjcChanges {
    changes: BTreeMap<String, Change>,
}

impl ObjcChanges {
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

/// What a sync's Objective-C lane calls for.
#[derive(Debug, Default)]
pub(crate) struct ObjcRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane: those of the members of the
    /// classes whose hierarchies moved, and those declared in a file named
    /// after one.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl ObjcRefresh {
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

/// What `row` writes for each class and property name, sorted.
fn written_by_name(row: Option<&HeadRow>) -> BTreeMap<(&str, &str), Vec<Option<&str>>> {
    let mut out: BTreeMap<(&str, &str), Vec<Option<&str>>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry((entry.kind.as_str(), entry.name.as_str()))
            .or_default()
            .push(entry.written.as_deref());
    }
    out
}

/// The types whose hierarchies a changed file's rows move: each class whose
/// entries changed (one came, went or names another superclass), and every
/// type a changed property is or was declared with.
fn seeds(changes: &ObjcChanges) -> BTreeSet<String> {
    let mut seeds = BTreeSet::new();
    for change in changes.changes.values() {
        let old = written_by_name(change.old_row.as_ref());
        let new = written_by_name(change.new_row.as_ref());
        for key in old.keys().chain(new.keys()) {
            let (before, after) = (old.get(key), new.get(key));
            if before == after {
                continue;
            }
            let (kind, name) = *key;
            if kind == "class" {
                seeds.insert(name.to_string());
            } else {
                seeds.extend(
                    before
                        .into_iter()
                        .chain(after)
                        .flatten()
                        .flatten()
                        .map(|written| (*written).to_string()),
                );
            }
        }
    }
    seeds
}

/// The superclasses the class entries of `rows` write, by class name.
fn superclasses<'r>(rows: impl IntoIterator<Item = &'r HeadRow>) -> HashMap<&'r str, Vec<&'r str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in rows {
        for entry in row.entries.iter().filter(|entry| entry.kind == "class") {
            let supers = out.entry(entry.name.as_str()).or_default();
            if let Some(written) = entry.written.as_deref()
                && !supers.contains(&written)
            {
                supers.push(written);
            }
        }
    }
    out
}

/// `seeds` and every class above one, by the superclasses `supers` gives
/// (the UIKit / AppKit superclass where none is written), with no bound.
fn hierarchies(supers: &HashMap<&str, Vec<&str>>, seeds: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        if reached.contains(&name) {
            continue;
        }
        match supers
            .get(name.as_str())
            .filter(|written| !written.is_empty())
        {
            Some(written) => queue.extend(written.iter().map(|s| (*s).to_string())),
            None => queue.extend(system_superclass(&name).map(str::to_string)),
        }
        reached.insert(name);
    }
    reached
}

/// Kinds a member of an Objective-C class is.
fn is_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Property | NodeKind::Field
    )
}

/// Read what the Objective-C lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Objective-C file re-resolves.
pub(crate) fn objc_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &ObjcChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<ObjcRefresh> {
    let mut refresh = ObjcRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::ObjC {
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
    let before = superclasses(
        stored.iter().chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.old_row.as_ref()),
        ),
    );
    let after = superclasses(
        stored.iter().chain(
            changes
                .changes
                .values()
                .filter_map(|change| change.new_row.as_ref()),
        ),
    );
    let mut classes = hierarchies(&before, &seeds);
    classes.extend(hierarchies(&after, &seeds));
    // The names of the members of each class, in the files declaring it
    // (an `@implementation` and a category are its class nodes too).
    for class in &classes {
        let files: BTreeSet<String> = store
            .nodes_by_name(class)?
            .into_iter()
            .filter(|node| node.kind == NodeKind::Class && node.language == Language::ObjC)
            .map(|node| node.file_path)
            .collect();
        for file in files {
            for node in store.nodes_by_file_path(&file)? {
                if node.language == Language::ObjC
                    && is_member_kind(node.kind)
                    && node
                        .qualified_name
                        .rsplit_once("::")
                        .is_some_and(|(owner, _)| owner == class)
                {
                    refresh.names.insert(node.name);
                }
            }
        }
    }
    // And every name declared in a file named after one: what an
    // `@implementation` the parser lost leaves its methods as.
    for file in store.all_files()? {
        if file.language == Language::ObjC && classes.contains(file_base(&file.path)) {
            for node in store.nodes_by_file_path(&file.path)? {
                if !matches!(node.kind, NodeKind::File | NodeKind::Import) {
                    refresh.names.insert(node.name);
                }
            }
        }
    }
    // A message written with a receiver waits for its selector, which the
    // shared tail lookup reads after the last `:` and caps: the unchanged
    // files holding one re-resolve whole.
    if !refresh.names.is_empty() {
        for reference in store.unresolved_refs_of_language(Language::ObjC)? {
            if reference
                .reference_name
                .rsplit_once('.')
                .is_some_and(|(_, selector)| refresh.names.contains(selector))
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
    use codegraph_resolve::objc_scope::HeadEntry;

    fn entry(kind: &str, name: &str, written: Option<&str>) -> HeadEntry {
        HeadEntry {
            kind: kind.to_string(),
            name: name.to_string(),
            written: written.map(str::to_string),
        }
    }

    fn row(entries: Vec<HeadEntry>) -> HeadRow {
        let mut entries = entries;
        entries.sort();
        HeadRow { entries }
    }

    fn change(old: Option<HeadRow>, new: Option<HeadRow>) -> ObjcChanges {
        let mut changes = ObjcChanges::default();
        changes.changes.insert(
            "Core/SDCache.h".to_string(),
            Change {
                old_row: old,
                new_row: new,
            },
        );
        changes
    }

    #[test]
    fn a_class_rebased_and_a_property_retyped_seed_their_types() {
        let rebased = change(
            Some(row(vec![entry("class", "SDCache", Some("SDBase"))])),
            Some(row(vec![entry("class", "SDCache", Some("SDRoot"))])),
        );
        assert_eq!(seeds(&rebased), BTreeSet::from(["SDCache".to_string()]));
        let retyped = change(
            Some(row(vec![entry("property", "url", Some("NSURL"))])),
            Some(row(vec![entry("property", "url", Some("SDURL"))])),
        );
        assert_eq!(
            seeds(&retyped),
            BTreeSet::from(["NSURL".to_string(), "SDURL".to_string()])
        );
        let same = change(
            Some(row(vec![entry("class", "SDCache", Some("SDBase"))])),
            Some(row(vec![entry("class", "SDCache", Some("SDBase"))])),
        );
        assert!(seeds(&same).is_empty());
        let untyped = change(None, Some(row(vec![entry("property", "done", None)])));
        assert!(seeds(&untyped).is_empty());
        let gone = change(Some(row(vec![entry("class", "SDCache", None)])), None);
        assert_eq!(seeds(&gone), BTreeSet::from(["SDCache".to_string()]));
    }

    #[test]
    fn a_hierarchy_climbs_written_superclasses_then_system_ones() {
        let rows = [
            row(vec![entry("class", "SDCache", Some("SDBase"))]),
            row(vec![entry("class", "SDBase", Some("UIView"))]),
            row(vec![entry("class", "UIImageView", None)]),
        ];
        let supers = superclasses(rows.iter());
        assert_eq!(
            hierarchies(&supers, &BTreeSet::from(["SDCache".to_string()])),
            BTreeSet::from(
                ["NSObject", "SDBase", "SDCache", "UIResponder", "UIView"].map(str::to_string)
            )
        );
        assert_eq!(
            hierarchies(&supers, &BTreeSet::from(["UIImageView".to_string()])),
            BTreeSet::from(
                ["NSObject", "UIImageView", "UIResponder", "UIView"].map(str::to_string)
            )
        );
    }

    #[test]
    fn only_objc_files_with_a_class_or_property_have_rows() {
        let text = SourceText::Text("int x = 1;\n".to_string());
        assert!(row_of(Language::ObjC, &text, &[]).is_none());
        assert!(row_of(Language::C, &text, &[]).is_none());
    }
}
