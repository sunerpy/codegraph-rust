//! What a PHP call through `$this`, `self`, `static` or `parent`, or through
//! a receiver named after a class, reads of other files (W4-PHP-01,
//! [`codegraph_resolve::php_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! Such a call reaches a method of the enclosing class's ancestry, or a
//! subclass's, or — for a receiver nothing typed — of the ancestry of a
//! class the receiver is named after (upstream #2142, #2152). The ancestry
//! is read from each type's `extends` clause and body trait `use`s, resolved
//! through its file's `namespace` and `use` imports: text that is gone once
//! a file changes on disk, so every full index and migration writes a row
//! per PHP file with an entry per type ([`HeadRow`]). A sync compares a
//! changed file's row with its new text; the types whose entries changed
//! are the seeds. Only the ancestries of the types at or below a seed move,
//! and each moves by what its seeds' ancestries gained or lost, so:
//!
//! - every name a method of a type a seed's ancestry gained or lost has, and
//!   of a seed that changed kind, is touched for the name lane: a call
//!   through `$this` below the seed, or through a receiver named after a
//!   class below it, can only gain or lose a method of such a type;
//! - the unchanged PHP files declaring a type a seed's ancestry gained or
//!   lost re-resolve whole: a `$this` call there may reach a subclass's
//!   method, and the subclasses below the seed are no longer, or now, below
//!   it;
//! - the unchanged PHP files declaring a type below a seed whose ancestry
//!   starts or stops leaving the repository re-resolve whole: past an
//!   outside ancestor any repository trait's method is in scope;
//! - an unresolved call written with a receiver named after a class at or
//!   below a seed, to a method of a touched name, is retried at its site,
//!   however many there are (the shared tail lookup caps them).
//!
//! Where a type below a seed has an ancestry as long as the resolver's walk
//! limit, which a change can cut elsewhere, every file declaring a type above
//! or below a seed re-resolves whole and every method above one is touched.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every PHP file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::php_scope::{
    ANCESTRY_LIMIT, HEAD_ROW_KEY_PREFIX, HeadRow, head_row_key, receiver_class_names,
};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

/// Metadata key present when no sync left a PHP dependent unrefreshed.
pub const VERSION_KEY: &str = "php_heads_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s head row, `None` when it records nothing: a file that is no
/// PHP file, declares no type, or is not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<HeadRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<HeadRow> {
    if language != Language::Php {
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

/// Whether this index needs every PHP file re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every PHP dependent is current: a full index, a migration,
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
    /// Its PHP methods before and after, `(owner, name)`, sorted.
    old_methods: Vec<(String, String)>,
    new_methods: Vec<(String, String)>,
}

/// The PHP methods among `nodes`, `(owner, name)`, sorted.
fn methods_of(nodes: &[Node]) -> Vec<(String, String)> {
    let mut methods: Vec<(String, String)> = nodes
        .iter()
        .filter(|node| node.language == Language::Php && node.kind == NodeKind::Method)
        .filter_map(|node| {
            let (owner, _) = node.qualified_name.rsplit_once("::")?;
            Some((owner.to_string(), node.name.clone()))
        })
        .collect();
    methods.sort();
    methods
}

/// What a sync's changed files held for the PHP lane.
#[derive(Debug, Default)]
pub(crate) struct PhpChanges {
    changes: BTreeMap<String, Change>,
}

impl PhpChanges {
    /// `path` changed (its old row still in the store, its old nodes
    /// `old_nodes`) to `new` (its language, text and nodes), or left the
    /// index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        old_nodes: &[Node],
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let (new_row, new_methods) = match new {
            Some((language, source, nodes)) => (row_of(language, source, nodes), methods_of(nodes)),
            None => (None, Vec::new()),
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
                old_methods: methods_of(old_nodes),
                ..Change::default()
            });
        change.new_row = new_row;
        change.new_methods = new_methods;
        Ok(())
    }
}

/// What a sync's PHP lane calls for.
#[derive(Debug, Default)]
pub(crate) struct PhpRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch for the name lane: those of the methods of a seed that
    /// changed kind, and of every type of an ancestry at the walk's limit.
    pub(crate) names: BTreeSet<String>,
    /// The names of the methods of the types a seed's ancestry gained or
    /// lost: the edges to methods of these names re-resolve at their sites.
    pub(crate) moved_names: BTreeSet<String>,
    /// Unresolved calls in unchanged files to retry at their sites.
    pub(crate) sites: Vec<FileReferenceSite>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<HeadRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl PhpRefresh {
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

/// What `row` declares for each qualified name: every entry's kind and
/// supertypes, sorted.
fn declared_by_name(row: Option<&HeadRow>) -> BTreeMap<&str, Vec<(&str, &[String])>> {
    let mut out: BTreeMap<&str, Vec<(&str, &[String])>> = BTreeMap::new();
    for entry in row.into_iter().flat_map(|row| &row.entries) {
        out.entry(entry.qualified_name.as_str())
            .or_default()
            .push((entry.kind.as_str(), entry.supers.as_slice()));
    }
    out
}

/// The types whose entries a changed file's rows move: one that came, went,
/// changed kind, or extends or uses another type.
fn seeds(changes: &PhpChanges) -> BTreeSet<String> {
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

/// The methods the changed files gained or lost, `(owner, name)`: a
/// receiver named after their owner, or a class below it, can reach one now
/// or no longer.
fn method_moves(changes: &PhpChanges) -> BTreeSet<(String, String)> {
    let mut moves = BTreeSet::new();
    for change in changes.changes.values() {
        let mut counts: BTreeMap<&(String, String), i64> = BTreeMap::new();
        for method in &change.old_methods {
            *counts.entry(method).or_default() += 1;
        }
        for method in &change.new_methods {
            *counts.entry(method).or_default() -= 1;
        }
        moves.extend(
            counts
                .into_iter()
                .filter(|(_, count)| *count != 0)
                .map(|(method, _)| method.clone()),
        );
    }
    moves
}

/// Each type's supertypes, or each type's subtypes.
type Links<'r> = HashMap<&'r str, BTreeSet<&'r str>>;

/// The supertypes the entries of `rows` name, by type (`up`), and the
/// types naming each as one (`down`).
fn links<'r>(rows: impl IntoIterator<Item = &'r HeadRow>) -> (Links<'r>, Links<'r>) {
    let mut up: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    let mut down: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            for written in &entry.supers {
                up.entry(entry.qualified_name.as_str())
                    .or_default()
                    .insert(written.as_str());
                down.entry(written.as_str())
                    .or_default()
                    .insert(entry.qualified_name.as_str());
            }
        }
    }
    (up, down)
}

/// The kinds each type has in `rows`, sorted.
fn kinds<'r>(rows: impl IntoIterator<Item = &'r HeadRow>) -> HashMap<&'r str, Vec<&'r str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in rows {
        for entry in &row.entries {
            out.entry(entry.qualified_name.as_str())
                .or_default()
                .push(entry.kind.as_str());
        }
    }
    for kinds in out.values_mut() {
        kinds.sort_unstable();
    }
    out
}

/// How many types the ancestry of `start` reaches by `links`, with no bound,
/// and whether one of them has no row (a declaration outside the
/// repository).
fn reach(links: &Links<'_>, kinds: &HashMap<&str, Vec<&str>>, start: &str) -> (usize, bool) {
    let mut reached: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([start]);
    let mut leaves = false;
    while let Some(name) = queue.pop_front() {
        if !reached.insert(name) {
            continue;
        }
        if !kinds.contains_key(name) {
            leaves = true;
        }
        if let Some(next) = links.get(name) {
            queue.extend(next.iter().copied());
        }
    }
    (reached.len(), leaves)
}

/// What each seed's ancestry gained or lost from `before` to `after`: the
/// ancestry of a type at or below a seed moves by no more, since only the
/// seeds' rows changed.
fn moved(seeds: &BTreeSet<String>, before: &Links<'_>, after: &Links<'_>) -> BTreeSet<String> {
    let mut moved = BTreeSet::new();
    for seed in seeds {
        let one = BTreeSet::from([seed.clone()]);
        moved.extend(
            closure(before, &one)
                .symmetric_difference(&closure(after, &one))
                .cloned(),
        );
    }
    moved
}

/// `seeds` and every type `links` reaches from one, with no bound.
fn closure(links: &Links<'_>, seeds: &BTreeSet<String>) -> BTreeSet<String> {
    let mut reached = BTreeSet::new();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
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

/// Read what the PHP lane calls for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// PHP file re-resolves.
pub(crate) fn php_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &PhpChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<PhpRefresh> {
    let mut refresh = PhpRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Php {
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
    let method_moves = method_moves(changes);
    if seeds.is_empty() && method_moves.is_empty() {
        return Ok(refresh);
    }
    let stored: Vec<(String, HeadRow)> = store
        .project_metadata_with_prefix(HEAD_ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            let path = key.strip_prefix(HEAD_ROW_KEY_PREFIX)?;
            if changes.changes.contains_key(path) {
                return None;
            }
            Some((path.to_string(), HeadRow::decode(&value)?))
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
    let kinds_before = kinds(before_rows());
    let kinds_after = kinds(after_rows());
    let moved = moved(&seeds, &up_before, &up_after);
    // A seed whose kind changed moves whether a call past an outside ancestor
    // can reach its methods, wherever the call is: its methods' names are
    // touched for the name lane.
    let mut owners: BTreeSet<String> = seeds
        .iter()
        .filter(|seed| kinds_before.get(seed.as_str()) != kinds_after.get(seed.as_str()))
        .cloned()
        .collect();
    let mut below = closure(&down_before, &seeds);
    below.extend(closure(&down_after, &seeds));
    let mut whole: BTreeSet<String> = moved.clone();
    let mut cut = false;
    for name in &below {
        let (count_before, leaves_before) = reach(&up_before, &kinds_before, name);
        let (count_after, leaves_after) = reach(&up_after, &kinds_after, name);
        if leaves_before != leaves_after {
            whole.insert(name.clone());
        }
        // The resolver's walk stops at its limit in breadth-first order, so a
        // change can push any branch of this ancestry in or out of it: every
        // type of it, before and after, is treated as moved for this type.
        if count_before >= ANCESTRY_LIMIT || count_after >= ANCESTRY_LIMIT {
            cut = true;
            let one = BTreeSet::from([name.clone()]);
            for ancestry in [closure(&up_before, &one), closure(&up_after, &one)] {
                whole.extend(ancestry.iter().cloned());
                owners.extend(ancestry);
            }
        }
    }
    for (path, row) in &stored {
        if unchanged(path)
            && row
                .entries
                .iter()
                .any(|entry| whole.contains(&entry.qualified_name))
        {
            refresh.files.insert(path.clone());
        }
    }
    // The names of those owners' methods, and of the `moved` types', in the
    // files declaring one (the store holds a changed file's new nodes).
    let declaring: BTreeSet<&str> =
        stored
            .iter()
            .map(|(path, row)| (path.as_str(), row))
            .chain(changes.changes.iter().filter_map(|(path, change)| {
                change.new_row.as_ref().map(|row| (path.as_str(), row))
            }))
            .filter(|(_, row)| {
                row.entries.iter().any(|entry| {
                    owners.contains(&entry.qualified_name) || moved.contains(&entry.qualified_name)
                })
            })
            .map(|(path, _)| path)
            .collect();
    for path in declaring {
        for node in store.nodes_by_file_path(path)? {
            if node.language != Language::Php || node.kind != NodeKind::Method {
                continue;
            }
            let Some((owner, _)) = node.qualified_name.rsplit_once("::") else {
                continue;
            };
            if owners.contains(owner) {
                refresh.names.insert(node.name);
            } else if moved.contains(owner) {
                refresh.moved_names.insert(node.name);
            }
        }
    }
    // A `moved` type's method can only come into or leave reach of a call in
    // a file declaring a type below a seed, or through a receiver named after
    // one: the edges to methods of those names re-resolve at their sites, and
    // so do the parked bare calls of those names in those files, rather than
    // every call of the names anywhere.
    if !refresh.moved_names.is_empty() {
        let below_files: BTreeSet<&str> = stored
            .iter()
            .filter(|(path, row)| {
                unchanged(path)
                    && row
                        .entries
                        .iter()
                        .any(|entry| below.contains(&entry.qualified_name))
            })
            .map(|(path, _)| path.as_str())
            .collect();
        let moved_names: Vec<String> = refresh.moved_names.iter().cloned().collect();
        for reference in store.unresolved_refs_by_names(&moved_names)? {
            if reference.language == Language::Php
                && below_files.contains(reference.file_path.as_str())
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
    }
    // A call written with a receiver waits for its method's name, which the
    // shared tail lookup caps at 500 per tail. The parked ones that can
    // change are retried at their sites, however many there are: a method of
    // a touched name through a receiver named after a class at or below a
    // seed, and a method that came or went (the name lane touches its name
    // already) through a receiver named after its owner or a class below it.
    let mut retry_names: BTreeSet<&str> = refresh
        .names
        .iter()
        .chain(&refresh.moved_names)
        .map(String::as_str)
        .collect();
    let mut receivers: BTreeSet<String> = below.clone();
    if !method_moves.is_empty() {
        let move_owners: BTreeSet<String> = method_moves
            .iter()
            .map(|(owner, _)| owner.clone())
            .collect();
        receivers.extend(closure(&down_before, &move_owners));
        receivers.extend(closure(&down_after, &move_owners));
        retry_names.extend(method_moves.iter().map(|(_, name)| name.as_str()));
    }
    if !retry_names.is_empty() {
        let named: BTreeSet<&str> = receivers
            .iter()
            .map(|name| {
                name.rsplit_once("::")
                    .map_or(name.as_str(), |(_, last)| last)
            })
            .collect();
        let members: Vec<String> = retry_names.iter().map(|name| (*name).to_string()).collect();
        for reference in store.unresolved_refs_of_language_by_members(Language::Php, &members)? {
            let Some((receiver, method)) = reference.reference_name.rsplit_once('.') else {
                continue;
            };
            if retry_names.contains(method)
                && unchanged(&reference.file_path)
                && (cut
                    || receiver_class_names(receiver)
                        .iter()
                        .any(|name| named.contains(name.as_str())))
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
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_resolve::php_scope::HeadEntry;

    fn entry(qualified_name: &str, kind: &str, supers: &[&str]) -> HeadEntry {
        HeadEntry {
            qualified_name: qualified_name.to_string(),
            kind: kind.to_string(),
            supers: supers.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    fn row(entries: Vec<HeadEntry>) -> HeadRow {
        let mut entries = entries;
        entries.sort();
        HeadRow { entries }
    }

    fn change(old: Option<HeadRow>, new: Option<HeadRow>) -> PhpChanges {
        let mut changes = PhpChanges::default();
        changes.changes.insert(
            "src/Middle.php".to_string(),
            Change {
                old_row: old,
                new_row: new,
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn a_method_that_came_or_went_moves_and_one_kept_does_not() {
        let method = |owner: &str, name: &str| (owner.to_string(), name.to_string());
        let mut changes = PhpChanges::default();
        changes.changes.insert(
            "src/Entity.php".to_string(),
            Change {
                old_methods: vec![
                    method("App::Entity", "load"),
                    method("App::Entity", "touch"),
                ],
                new_methods: vec![method("App::Entity", "load"), method("App::Entity", "save")],
                ..Change::default()
            },
        );
        assert_eq!(
            method_moves(&changes),
            BTreeSet::from([
                method("App::Entity", "save"),
                method("App::Entity", "touch")
            ])
        );
    }

    #[test]
    fn a_type_rebased_retyped_added_or_removed_seeds_itself() {
        let rebased = change(
            Some(row(vec![entry("App::Middle", "class", &["App::Base"])])),
            Some(row(vec![entry("App::Middle", "class", &["App::Other"])])),
        );
        assert_eq!(seeds(&rebased), BTreeSet::from(["App::Middle".to_string()]));
        let retyped = change(
            Some(row(vec![entry("App::Concern", "class", &[])])),
            Some(row(vec![entry("App::Concern", "trait", &[])])),
        );
        assert_eq!(
            seeds(&retyped),
            BTreeSet::from(["App::Concern".to_string()])
        );
        let same = change(
            Some(row(vec![entry("App::Middle", "class", &["App::Base"])])),
            Some(row(vec![entry("App::Middle", "class", &["App::Base"])])),
        );
        assert!(seeds(&same).is_empty());
        let added = change(None, Some(row(vec![entry("App::Middle", "class", &[])])));
        assert_eq!(seeds(&added), BTreeSet::from(["App::Middle".to_string()]));
        let gone = change(Some(row(vec![entry("App::Middle", "class", &[])])), None);
        assert_eq!(seeds(&gone), BTreeSet::from(["App::Middle".to_string()]));
    }

    #[test]
    fn the_walk_reaches_every_type_above_and_below_a_seed() {
        let rows = [
            row(vec![entry("App::Child", "class", &["App::Middle"])]),
            row(vec![entry(
                "App::Middle",
                "class",
                &["App::Base", "App::Greets"],
            )]),
            row(vec![entry("App::Base", "class", &["Vendor::Model"])]),
            row(vec![entry("App::Greets", "trait", &[])]),
            row(vec![entry("App::Other", "class", &[])]),
        ];
        let (up, down) = links(rows.iter());
        let seed = BTreeSet::from(["App::Middle".to_string()]);
        assert_eq!(
            closure(&up, &seed),
            BTreeSet::from(
                ["App::Base", "App::Greets", "App::Middle", "Vendor::Model"].map(str::to_string)
            )
        );
        assert_eq!(
            closure(&down, &seed),
            BTreeSet::from(["App::Child", "App::Middle"].map(str::to_string))
        );
    }

    #[test]
    fn a_seed_moves_only_what_its_ancestry_gained_or_lost() {
        let before = [
            row(vec![entry("App::Middle", "class", &["App::Base"])]),
            row(vec![entry("App::Base", "class", &["Vendor::Model"])]),
            row(vec![entry("App::Other", "class", &[])]),
        ];
        let after = [
            row(vec![entry("App::Middle", "class", &["App::Other"])]),
            row(vec![entry("App::Base", "class", &["Vendor::Model"])]),
            row(vec![entry("App::Other", "class", &[])]),
        ];
        let (up_before, _) = links(before.iter());
        let (up_after, _) = links(after.iter());
        let seeds = BTreeSet::from(["App::Middle".to_string()]);
        assert_eq!(
            moved(&seeds, &up_before, &up_after),
            BTreeSet::from(["App::Base", "App::Other", "Vendor::Model"].map(str::to_string))
        );
        // Two seeds trading a supertype still move it.
        let traded_before = [
            row(vec![entry("App::A", "class", &["App::Base"])]),
            row(vec![entry("App::B", "class", &[])]),
        ];
        let traded_after = [
            row(vec![entry("App::A", "class", &[])]),
            row(vec![entry("App::B", "class", &["App::Base"])]),
        ];
        let (up_before, _) = links(traded_before.iter());
        let (up_after, _) = links(traded_after.iter());
        let seeds = BTreeSet::from(["App::A".to_string(), "App::B".to_string()]);
        assert_eq!(
            moved(&seeds, &up_before, &up_after),
            BTreeSet::from(["App::Base".to_string()])
        );
    }

    #[test]
    fn an_ancestry_leaves_the_repository_past_a_type_with_no_row() {
        let rows = [
            row(vec![entry("App::Child", "class", &["App::Middle"])]),
            row(vec![entry("App::Middle", "class", &["Vendor::TestCase"])]),
            row(vec![entry("App::Alone", "class", &[])]),
        ];
        let (up, _) = links(rows.iter());
        let kinds = kinds(rows.iter());
        assert_eq!(reach(&up, &kinds, "App::Child"), (3, true));
        assert_eq!(reach(&up, &kinds, "App::Alone"), (1, false));
    }

    #[test]
    fn only_php_files_declaring_a_type_have_rows() {
        let text = SourceText::Text("<?php\nfunction f() {}\n".to_string());
        assert!(row_of(Language::Php, &text, &[]).is_none());
        assert!(row_of(Language::JavaScript, &text, &[]).is_none());
    }
}
