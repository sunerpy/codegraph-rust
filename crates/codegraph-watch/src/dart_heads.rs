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
use codegraph_core::types::{Language, Node, NodeKind};
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
    /// The members the file declared before the change, by qualified name,
    /// kind and signature (W4-DART-03).
    old_members: BTreeSet<MemberKey>,
}

/// A Dart member as a sync tells a new one from one that moved: its
/// qualified name, kind and signature (a method becoming a getter drops its
/// parameter list).
type MemberKey = (String, &'static str, Option<String>);

/// The members among `nodes` whose names a member read, a typed call or a
/// call through a type waits for: methods and constants a type owns.
fn member_keys(nodes: &[Node]) -> BTreeSet<MemberKey> {
    nodes
        .iter()
        .filter(|node| {
            node.language == Language::Dart
                && matches!(node.kind, NodeKind::Method | NodeKind::Constant)
                && member_owner(&node.qualified_name).is_some()
        })
        .map(|node| {
            (
                node.qualified_name.clone(),
                node.kind.as_str(),
                node.signature.clone(),
            )
        })
        .collect()
}

/// What a sync's changed files held for the Dart lane.
#[derive(Debug, Default)]
pub(crate) struct DartChanges {
    changes: BTreeMap<String, Change>,
    /// The files whose edges reached a type of a changed file whose head
    /// moved or went, read before the change replaced its nodes.
    mentions: BTreeSet<String>,
}

impl DartChanges {
    /// The names of the extensions a changed file declared or declares
    /// (W4-DART-03b).
    pub(crate) fn extension_names(&self) -> BTreeSet<String> {
        self.changes
            .values()
            .flat_map(|change| change.old_row.iter().chain(change.new_row.iter()))
            .flat_map(|row| &row.entries)
            .filter(|entry| entry.extension)
            .map(|entry| entry.name.clone())
            .collect()
    }

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
        // The edges into the old declarations of a type whose head moves or
        // goes, while they are still there (W4-DART-03).
        let (old, new_declared) = (
            declared_by_name(old_row.as_ref()),
            declared_by_name(new_row.as_ref()),
        );
        let moved: Vec<String> = old
            .iter()
            .filter(|(name, entries)| new_declared.get(*name) != Some(*entries))
            .map(|(name, _)| (*name).to_string())
            .collect();
        if !moved.is_empty() {
            self.mentions
                .extend(store.source_files_of_edges_to_named_targets(&moved)?);
        }
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = match self.changes.entry(path.to_string()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(Change {
                old_row,
                old_members: member_keys(&store.nodes_by_file_path(path)?),
                ..Change::default()
            }),
        };
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

/// The types whose members a lookup on a type at or below one of `seeds`
/// can reach that a change of their heads can move (W4-DART-03b): every type
/// of each seed's ancestry, before the change and after it (past the
/// resolver's walk limit, the whole ancestry of every type below the seed),
/// and the extensions `on` one of those types; with a seed that came or
/// went, every extension, as a new type is a project type an extension `on`
/// any type applies to.
pub(crate) fn ancestry_owners(
    store: &Store,
    changes: &DartChanges,
    seeds: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    if seeds.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut stored: Vec<(String, HeadRow)> = Vec::new();
    for (key, value) in store.project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)? {
        let Some(path) = key.strip_prefix(HEAD_ROW_KEY_PREFIX) else {
            continue;
        };
        if changes.changes.contains_key(path) {
            continue;
        }
        // An unreadable row is the bootstrap's to rebuild.
        if let Some(row) = HeadRow::decode(&value) {
            stored.push((path.to_string(), row));
        }
    }
    let rows_with = |old: bool| {
        stored
            .iter()
            .map(|(_, row)| row)
            .chain(changes.changes.values().filter_map(move |change| {
                if old {
                    &change.old_row
                } else {
                    &change.new_row
                }
                .as_ref()
            }))
    };
    let (up_before, up_after) = (links(rows_with(true)), links(rows_with(false)));
    let (down_before, down_after) = (down_links(rows_with(true)), down_links(rows_with(false)));
    let declared = |old: bool| -> BTreeMap<&str, usize> {
        let mut out: BTreeMap<&str, usize> = BTreeMap::new();
        for row in rows_with(old) {
            for entry in &row.entries {
                *out.entry(entry.name.as_str()).or_default() += 1;
            }
        }
        out
    };
    let (declared_before, declared_after) = (declared(true), declared(false));
    let mut owners: BTreeSet<String> = BTreeSet::new();
    let mut every_extension = false;
    for seed in seeds {
        owners.extend(closure(&up_before, seed));
        owners.extend(closure(&up_after, seed));
        every_extension |= declared_before.get(seed.as_str()) != declared_after.get(seed.as_str());
        let mut below = closure(&down_before, seed);
        below.extend(closure(&down_after, seed));
        for name in &below {
            let (before, after) = (closure(&up_before, name), closure(&up_after, name));
            if before.len() >= HIERARCHY_LIMIT || after.len() >= HIERARCHY_LIMIT {
                owners.extend(before);
                owners.extend(after);
            }
        }
    }
    let extensions: Vec<String> = rows_with(true)
        .chain(rows_with(false))
        .flat_map(|row| &row.entries)
        .filter(|entry| {
            entry.extension
                && (every_extension
                    || entry.supers.iter().any(|sup| owners.contains(sup))
                    || entry.on.as_ref().is_some_and(|on| owners.contains(on)))
        })
        .map(|entry| entry.name.clone())
        .collect();
    owners.extend(extensions);
    Ok(owners)
}

/// The names of the members a call can reach through any of `types` (W4-DART-02):
/// the members of every type of their ancestries, as the head rows after
/// this sync name them, and of the extensions `on` one of those types. A
/// subclass that gains or loses one of `types` as its supertype moves the
/// calls on it to these.
pub(crate) fn member_names_through(
    store: &Store,
    changes: &DartChanges,
    types: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    if types.is_empty() {
        return Ok(names);
    }
    let mut rows: Vec<(String, HeadRow)> = Vec::new();
    for (key, value) in store.project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)? {
        let Some(path) = key.strip_prefix(HEAD_ROW_KEY_PREFIX) else {
            continue;
        };
        if changes.changes.contains_key(path) {
            continue;
        }
        // An unreadable row is the bootstrap's to rebuild.
        if let Some(row) = HeadRow::decode(&value) {
            rows.push((path.to_string(), row));
        }
    }
    for (path, change) in &changes.changes {
        if let Some(row) = &change.new_row {
            rows.push((path.clone(), row.clone()));
        }
    }
    let up = links(rows.iter().map(|(_, row)| row));
    let mut owners: BTreeSet<String> = BTreeSet::new();
    for name in types {
        owners.extend(closure(&up, name));
    }
    let extensions: Vec<String> = rows
        .iter()
        .flat_map(|(_, row)| &row.entries)
        .filter(|entry| entry.extension && entry.supers.iter().any(|sup| owners.contains(sup)))
        .map(|entry| entry.name.clone())
        .collect();
    owners.extend(extensions);
    for (path, row) in &rows {
        if !row.entries.iter().any(|entry| owners.contains(&entry.name)) {
            continue;
        }
        for node in store.nodes_by_file_path(path)? {
            if node.language == Language::Dart
                && codegraph_resolve::dart_scope::is_dart_member(&node)
                && member_owner(&node.qualified_name).is_some_and(|owner| owners.contains(owner))
            {
                names.insert(node.name);
            }
        }
    }
    Ok(names)
}

/// The references in unchanged Dart files that a typed receiver may now
/// resolve differently through the lineage of a type whose head moved
/// (W4-DART-03), at their sites: a member read or a call through a receiver
/// typed `U` reaches a member of `U`'s lineage (upstream #2338's
/// `dartMemberOf`, and `resolve_method_on_type` before it). When `U` is at or
/// below a non-extension type whose supertypes changed, came or went, a type
/// of that lineage can come into it, leave it, or move in the ranking only as
/// its distance from the changed type changed (past the resolver's walk
/// limit, the whole ancestry counts), and only a member of such a type, or
/// of an extension `on` one, can change what the receiver reaches. A
/// receiver is typed by a declaration its file writes (`U x`, `x = U(…)`,
/// `U.name`), which references `U`, or by a field of the class around it or
/// of a class that one extends, whose file references `U`: so, in the files
/// referencing such a type (through an edge, before the change or after it,
/// or a reference left unresolved, by its name or through it as a receiver)
/// and in the files declaring a type below one such a file declares, the
/// references written with a receiver to such a member's name, parked or
/// resolved, are retried. An extension's head reaches its members' calls by
/// their names, in [`dart_refresh`].
pub(crate) fn typed_receiver_sites(
    store: &Store,
    changes: &DartChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<Vec<FileReferenceSite>> {
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    let seeds: BTreeSet<String> = seeds(changes)
        .into_iter()
        .filter(|seed| {
            changes.changes.values().any(|change| {
                [&change.old_row, &change.new_row].into_iter().any(|row| {
                    row.iter()
                        .flat_map(|row| &row.entries)
                        .any(|entry| entry.name == *seed && !entry.extension)
                })
            })
        })
        .collect();
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let mut stored: Vec<(String, HeadRow)> = Vec::new();
    for (key, value) in store.project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)? {
        let Some(path) = key.strip_prefix(HEAD_ROW_KEY_PREFIX) else {
            continue;
        };
        if changes.changes.contains_key(path) {
            continue;
        }
        // An unreadable row is the bootstrap's to rebuild.
        if let Some(row) = HeadRow::decode(&value) {
            stored.push((path.to_string(), row));
        }
    }
    let rows_with = |old: bool| {
        stored.iter().map(|(path, row)| (path.as_str(), row)).chain(
            changes.changes.iter().filter_map(move |(path, change)| {
                let row = if old {
                    &change.old_row
                } else {
                    &change.new_row
                };
                row.as_ref().map(|row| (path.as_str(), row))
            }),
        )
    };
    let (up_before, up_after) = (
        links(rows_with(true).map(|(_, row)| row)),
        links(rows_with(false).map(|(_, row)| row)),
    );
    let (down_before, down_after) = (
        down_links(rows_with(true).map(|(_, row)| row)),
        down_links(rows_with(false).map(|(_, row)| row)),
    );
    // The types whose distance from a seed changed, and a seed that came or
    // went itself: its own members came or went with it.
    let declared = |old: bool| -> BTreeMap<String, usize> {
        let mut out: BTreeMap<String, usize> = BTreeMap::new();
        for (_, row) in rows_with(old) {
            for entry in &row.entries {
                *out.entry(entry.name.clone()).or_default() += 1;
            }
        }
        out
    };
    let (declared_before, declared_after) = (declared(true), declared(false));
    let mut moved: BTreeSet<String> = BTreeSet::new();
    let mut typed: BTreeSet<String> = BTreeSet::new();
    for seed in &seeds {
        if declared_before.get(seed) != declared_after.get(seed) {
            moved.insert(seed.clone());
        }
        let (from_before, from_after) = (depths(&up_before, seed), depths(&up_after, seed));
        for name in from_before.keys().chain(from_after.keys()) {
            if from_before.get(name) != from_after.get(name) {
                moved.insert(name.clone());
            }
        }
        let mut below = closure(&down_before, seed);
        below.extend(closure(&down_after, seed));
        for name in &below {
            let (reach_before, reach_after) = (closure(&up_before, name), closure(&up_after, name));
            if reach_before.len() >= HIERARCHY_LIMIT || reach_after.len() >= HIERARCHY_LIMIT {
                moved.extend(reach_before);
                moved.extend(reach_after);
            }
        }
        typed.extend(below);
    }
    // Their members and the members of the extensions on one of them.
    let mut owners = moved.clone();
    owners.extend(
        rows_with(true)
            .chain(rows_with(false))
            .flat_map(|(_, row)| &row.entries)
            .filter(|entry| entry.extension && entry.supers.iter().any(|sup| moved.contains(sup)))
            .map(|entry| entry.name.clone()),
    );
    let declaring: BTreeSet<&str> = rows_with(false)
        .filter(|(_, row)| row.entries.iter().any(|entry| owners.contains(&entry.name)))
        .map(|(path, _)| path)
        .collect();
    let mut members: BTreeSet<String> = BTreeSet::new();
    for path in declaring {
        for node in store.nodes_by_file_path(path)? {
            if node.language == Language::Dart
                && codegraph_resolve::dart_scope::is_dart_member(&node)
                && member_owner(&node.qualified_name).is_some_and(|owner| owners.contains(owner))
            {
                members.insert(node.name);
            }
        }
    }
    if members.is_empty() {
        return Ok(Vec::new());
    }
    // The files a receiver typed by one of `typed` can be written in.
    let names: Vec<String> = typed.iter().cloned().collect();
    let mut referencing: BTreeSet<String> = changes.mentions.clone();
    referencing.extend(store.source_files_of_edges_to_named_targets(&names)?);
    referencing.extend(
        store
            .unresolved_refs_by_names(&names)?
            .into_iter()
            .filter(|reference| reference.language == Language::Dart)
            .map(|reference| reference.file_path),
    );
    // A call through the type (`U.make()`) names it only as its receiver.
    referencing.extend(
        store
            .unresolved_refs_of_language_through(Language::Dart, &names)?
            .into_iter()
            .map(|reference| reference.file_path),
    );
    // The types the referencing files declare, and every type below one.
    let mut below: BTreeSet<String> = BTreeSet::new();
    for (path, row) in rows_with(false) {
        if referencing.contains(path) {
            for entry in &row.entries {
                below.extend(closure(&down_after, &entry.name));
            }
        }
    }
    for (path, row) in rows_with(false) {
        if row.entries.iter().any(|entry| below.contains(&entry.name)) {
            referencing.insert(path.to_string());
        }
    }
    referencing.retain(|path| unchanged(path));
    if referencing.is_empty() {
        return Ok(Vec::new());
    }
    let members: Vec<String> = members.into_iter().collect();
    let mut sites: BTreeSet<FileReferenceSite> = BTreeSet::new();
    for reference in store.unresolved_refs_of_language_by_members(Language::Dart, &members)? {
        if referencing.contains(&reference.file_path) {
            sites.insert(FileReferenceSite {
                file_path: reference.file_path,
                site: Some(ReferenceSite {
                    from_node_id: reference.from_node_id,
                    line: reference.line,
                    col: reference.col,
                }),
            });
        }
    }
    for site in store
        .reference_sites_of_edges_to_named_targets(&members)?
        .into_iter()
        .chain(store.reference_sites_of_same_file_edges_to_named_targets(&members)?)
    {
        if referencing.contains(&site.file_path) {
            sites.insert(site);
        }
    }
    Ok(sites.into_iter().collect())
}

/// The unresolved Dart references in unchanged files that a member a changed
/// file newly declares may now satisfy, at their sites (W4-DART-03): a member
/// read waits for a getter (upstream #2338), a call on a typed receiver for a
/// member an extension adds, and a call through a type for its static
/// constant (#2367). They wait by the member's name, which the shared tail
/// lookup caps at 500 per tail, so the parked Dart ones to such a member's
/// name are retried here, however many there are. A member is new when no
/// declaration of the file had its qualified name, kind and signature before
/// (one that only moved down makes nothing reachable). A new type's members
/// are left to [`typed_receiver_files`]: only a file naming the type can type
/// a receiver by it.
pub(crate) fn new_member_sites(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &DartChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<Vec<FileReferenceSite>> {
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    let mut existing: Option<BTreeSet<String>> = None;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for (path, change) in &changes.changes {
        let Some(row) = &change.new_row else {
            continue;
        };
        let nodes = store.nodes_by_file_path(path)?;
        let added: Vec<&Node> = nodes
            .iter()
            .filter(|node| {
                member_keys(std::slice::from_ref(node))
                    .iter()
                    .any(|key| !change.old_members.contains(key))
            })
            .collect();
        if added.is_empty() {
            continue;
        }
        let extensions: BTreeSet<&str> = row
            .entries
            .iter()
            .filter(|entry| entry.extension)
            .map(|entry| entry.name.as_str())
            .collect();
        // The types declared before this sync, anywhere.
        let existing = match &mut existing {
            Some(existing) => existing,
            slot => {
                let mut names: BTreeSet<String> = BTreeSet::new();
                for (key, value) in store.project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)? {
                    let changed = key
                        .strip_prefix(HEAD_ROW_KEY_PREFIX)
                        .is_some_and(|path| changes.changes.contains_key(path));
                    if !changed && let Some(row) = HeadRow::decode(&value) {
                        names.extend(row.entries.into_iter().map(|entry| entry.name));
                    }
                }
                for change in changes.changes.values() {
                    names.extend(
                        change
                            .old_row
                            .iter()
                            .flat_map(|row| &row.entries)
                            .map(|entry| entry.name.clone()),
                    );
                }
                slot.insert(names)
            }
        };
        let lines: Vec<String> = read_text(project_root, path, max_file_size)
            .map(|text| text.split('\n').map(str::to_string).collect())
            .unwrap_or_default();
        for node in added {
            let Some(owner) = member_owner(&node.qualified_name) else {
                continue;
            };
            let extension = extensions.contains(owner);
            if !extension && !existing.contains(owner) {
                continue;
            }
            let member = node.kind == NodeKind::Constant || extension || is_getter_at(&lines, node);
            if member {
                names.insert(node.name.clone());
            }
        }
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let members: Vec<String> = names.into_iter().collect();
    Ok(store
        .unresolved_refs_of_language_by_members(Language::Dart, &members)?
        .into_iter()
        .filter(|reference| unchanged(&reference.file_path))
        .map(|reference| FileReferenceSite {
            file_path: reference.file_path,
            site: Some(ReferenceSite {
                from_node_id: reference.from_node_id,
                line: reference.line,
                col: reference.col,
            }),
        })
        .collect())
}

/// Whether `node` is declared a getter in the file whose lines are `lines`,
/// as the resolver reads it ([`codegraph_resolve::dart_members::declares_getter`]).
fn is_getter_at(lines: &[String], node: &Node) -> bool {
    let at = |line: i64| {
        usize::try_from(line - 1)
            .ok()
            .and_then(|index| lines.get(index))
            .map_or("", |text| text.trim_end_matches('\r'))
    };
    let from = usize::try_from(node.start_column.max(0)).unwrap_or(0);
    let head = format!(
        "{} {}",
        at(node.start_line).get(from..).unwrap_or(""),
        at(node.start_line + 1)
    );
    codegraph_resolve::dart_members::declares_getter(&head, &node.name)
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
                ..Change::default()
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
