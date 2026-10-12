//! What Kotlin resolution reads of other files (W4-JVM-03,
//! [`codegraph_resolve::kotlin_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A bare Kotlin call reaches the members of the types its hierarchy walk
//! reaches (the heads of Kotlin type declarations, read by name), of the
//! receiver its lambda's function names (that function's last parameter
//! type and the typealiases behind it), and of the project-wide receiver
//! types (function types' receivers, extension receivers no project class
//! names, and what those inherit); a Gradle script's bare call reaches no
//! member of a project class. None of that is a node, and the text is gone
//! once a file changes on disk, so every full index and migration writes a
//! row per Kotlin file ([`KotlinSurface`]), and a sync compares a changed
//! file's row with its new text:
//!
//! - heads that came, went or changed seed a closure over every head naming
//!   one; the unchanged files whose walks can start from a closure name, or
//!   whose lambdas run on a receiver in the closure, re-resolve whole;
//! - a lambda function whose receiver changed sends the unchanged files with
//!   lambdas passed to it down the same lane;
//! - a change of the project-wide receiver types touches the names of the
//!   members of every type that joined or left them, and a project class
//!   name appearing or leaving touches the names of the members a Gradle
//!   script's call could reach through it, for the name lane.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`] re-resolves every Kotlin file once and rebuilds the
//! rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::kotlin_scope::{
    self, KotlinSurface, ReceiverMentions, SURFACE_ROW_KEY_PREFIX, surface_row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a Kotlin dependent unrefreshed.
pub const VERSION_KEY: &str = "kotlin_surface_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s surface row, `None` when it records nothing: a file that is no
/// Kotlin file, holds nothing a row records, or is not text.
pub fn row_of(
    path: &str,
    language: Language,
    source: &SourceText,
    nodes: &[Node],
) -> Option<KotlinSurface> {
    match source {
        SourceText::Text(text) => row_of_text(path, language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(
    path: &str,
    language: Language,
    text: &str,
    nodes: &[Node],
) -> Option<KotlinSurface> {
    if language != Language::Kotlin {
        return None;
    }
    let row = KotlinSurface::of(path, text, nodes);
    (!row.is_empty()).then_some(row)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, KotlinSurface)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&surface_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every Kotlin file re-resolved and its rows
/// rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every Kotlin dependent is current: a full index, a
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

fn read_row(store: &Store, path: &str) -> Result<Option<KotlinSurface>> {
    Ok(store
        .get_project_metadata(&surface_row_key(path))?
        .and_then(|value| KotlinSurface::decode(&value)))
}

/// The names `nodes` declare as classes an extension receiver's outside
/// test reads, and as owners a Gradle script's call reads.
fn class_names(nodes: &[Node]) -> (BTreeSet<String>, BTreeSet<String>) {
    let receivers = nodes
        .iter()
        .filter(|node| kotlin_scope::is_receiver_class(node))
        .map(|node| node.name.clone())
        .collect();
    let owners = nodes
        .iter()
        .filter(|node| kotlin_scope::is_gradle_owner(node))
        .map(|node| node.name.clone())
        .collect();
    (receivers, owners)
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<KotlinSurface>,
    new_row: Option<KotlinSurface>,
    old_receiver_classes: BTreeSet<String>,
    new_receiver_classes: BTreeSet<String>,
    old_owners: BTreeSet<String>,
    new_owners: BTreeSet<String>,
}

/// What a sync's changed files held for the Kotlin lanes.
#[derive(Debug, Default)]
pub(crate) struct KotlinChanges {
    changes: BTreeMap<String, Change>,
}

impl KotlinChanges {
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
        let (old_receiver_classes, old_owners) = class_names(old_nodes);
        let (new_row, (new_receiver_classes, new_owners)) = match new {
            Some((language, source, nodes)) => {
                (row_of(path, language, source, nodes), class_names(nodes))
            }
            None => (None, (BTreeSet::new(), BTreeSet::new())),
        };
        if old_row.is_none()
            && new_row.is_none()
            && old_receiver_classes.is_empty()
            && new_receiver_classes.is_empty()
            && old_owners.is_empty()
            && new_owners.is_empty()
        {
            return Ok(());
        }
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = self
            .changes
            .entry(path.to_string())
            .or_insert_with(|| Change {
                old_row,
                old_receiver_classes,
                old_owners,
                ..Change::default()
            });
        change.new_row = new_row;
        change.new_receiver_classes = new_receiver_classes;
        change.new_owners = new_owners;
        Ok(())
    }
}

/// What a sync's Kotlin lanes call for.
#[derive(Debug, Default)]
pub(crate) struct KotlinRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Member names to touch for the name lane.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<KotlinSurface>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl KotlinRefresh {
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
    stored: &'r BTreeMap<String, KotlinSurface>,
    changes: &'r KotlinChanges,
    before: bool,
) -> BTreeMap<&'r str, &'r KotlinSurface> {
    let mut view: BTreeMap<&str, &KotlinSurface> = stored
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

/// Each name's entries of `rows` per file, in path then file order.
fn heads_by_name<'r>(
    rows: &BTreeMap<&str, &'r KotlinSurface>,
) -> HashMap<&'r str, Vec<&'r kotlin_scope::KotlinHead>> {
    let mut out: HashMap<&str, Vec<&kotlin_scope::KotlinHead>> = HashMap::new();
    for row in rows.values() {
        for head in &row.heads {
            out.entry(head.name.as_str()).or_default().push(head);
        }
    }
    out
}

/// The lambda receiver of every lambda function name `rows` record.
fn lambda_receivers(rows: &BTreeMap<&str, &KotlinSurface>) -> BTreeMap<String, Option<String>> {
    let mut written: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut aliases: HashMap<&str, &str> = HashMap::new();
    for row in rows.values() {
        for lambda in &row.lambdas {
            written
                .entry(lambda.name.as_str())
                .or_default()
                .push(lambda.written.as_str());
        }
        for alias in &row.aliases {
            // The first right side in path order, as resolution reads it.
            aliases
                .entry(alias.name.as_str())
                .or_insert(alias.rhs.as_str());
        }
    }
    let alias_rhs = |name: &str| aliases.get(name).map(|rhs| (*rhs).to_string());
    written
        .into_iter()
        .map(|(name, written)| {
            (
                name.to_string(),
                kotlin_scope::lambda_receiver_from(written, &alias_rhs),
            )
        })
        .collect()
}

/// The project-wide receiver types `rows` make, with `declared` the
/// extension receivers' outside test.
fn receiver_types(
    rows: &BTreeMap<&str, &KotlinSurface>,
    declared: impl Fn(&str) -> bool,
) -> HashSet<String> {
    let mentions: Vec<ReceiverMentions> = rows.values().map(|row| row.mentions()).collect();
    let heads = heads_by_name(rows);
    kotlin_scope::receiver_types_from(&mentions, declared, |name| {
        heads
            .get(name)
            .into_iter()
            .flatten()
            .flat_map(|head| head.supertypes.iter().cloned())
            .collect()
    })
}

/// Each name's head entries of a row, in file order.
fn entries_by_name(row: Option<&KotlinSurface>) -> BTreeMap<&str, Vec<&kotlin_scope::KotlinHead>> {
    let mut out: BTreeMap<&str, Vec<&kotlin_scope::KotlinHead>> = BTreeMap::new();
    for head in row.into_iter().flat_map(|row| &row.heads) {
        out.entry(head.name.as_str()).or_default().push(head);
    }
    out
}

/// The names whose head entries a changed file's rows differ in.
fn head_seeds(changes: &KotlinChanges) -> BTreeSet<String> {
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

/// `seeds` closed over every head (before and after the change) naming a
/// reached name, and over the Android ancestry table read backwards.
fn head_closure(
    before: &BTreeMap<&str, &KotlinSurface>,
    after: &BTreeMap<&str, &KotlinSurface>,
    seeds: BTreeSet<String>,
) -> BTreeSet<String> {
    let mut naming: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for row in before.values().chain(after.values()) {
        for head in &row.heads {
            for supertype in &head.supertypes {
                naming
                    .entry(supertype.as_str())
                    .or_default()
                    .insert(head.name.as_str());
            }
        }
    }
    let mut reached = seeds.clone();
    let mut queue: VecDeque<String> = seeds.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        let deriving = naming
            .get(name.as_str())
            .into_iter()
            .flatten()
            .map(|name| (*name).to_string())
            .chain(
                kotlin_scope::platform_subtypes(&name)
                    .into_iter()
                    .map(str::to_string),
            );
        for name in deriving.collect::<Vec<_>>() {
            if reached.insert(name.clone()) {
                queue.push_back(name);
            }
        }
    }
    reached
}

/// Whether a class-like name of `pick`'s kind exists before or after the
/// change: in an unchanged file, or in the changed files' old or new nodes.
fn flipped_names(
    store: &Store,
    changes: &KotlinChanges,
    pick: impl Fn(&Change) -> (&BTreeSet<String>, &BTreeSet<String>),
    counts: impl Fn(&Node) -> bool,
    unchanged: impl Fn(&str) -> bool,
) -> Result<BTreeSet<String>> {
    let mut sides: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    for change in changes.changes.values() {
        let (old, new) = pick(change);
        for name in old {
            sides.entry(name).or_default().0 = true;
        }
        for name in new {
            sides.entry(name).or_default().1 = true;
        }
    }
    let mut flipped = BTreeSet::new();
    for (name, (before, after)) in sides {
        if before == after {
            continue;
        }
        let elsewhere = store
            .nodes_by_name(name)?
            .iter()
            .any(|node| counts(node) && unchanged(&node.file_path));
        if !elsewhere {
            flipped.insert(name.to_string());
        }
    }
    Ok(flipped)
}

/// The names of the members of `kinds` whose owner, as a bare call reads
/// it, is one of `owners`.
fn member_names(
    store: &Store,
    owners: &BTreeSet<String>,
    kinds: &[NodeKind],
    languages: &[Language],
) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for kind in kinds {
        for node in store.nodes_by_kind(*kind)? {
            if (languages.is_empty() || languages.contains(&node.language))
                && kotlin_scope::member_owner_name(&node.qualified_name)
                    .is_some_and(|owner| owners.contains(&owner))
            {
                names.insert(node.name);
            }
        }
    }
    Ok(names)
}

/// Read what the Kotlin lanes call for, after every changed file is
/// re-extracted (the store holds the new nodes; the rows are still the old
/// ones). With `bootstrap`, every row is rebuilt from disk instead and every
/// Kotlin file re-resolves.
pub(crate) fn kotlin_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &KotlinChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<KotlinRefresh> {
    let mut refresh = KotlinRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if file.language != Language::Kotlin {
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
    }
    let seeds = head_seeds(changes);
    let lambdas_changed = changes.changes.values().any(|change| {
        let lambdas =
            |row: Option<&KotlinSurface>| row.map(|row| (row.lambdas.clone(), row.aliases.clone()));
        lambdas(change.old_row.as_ref()) != lambdas(change.new_row.as_ref())
    });
    let mentions_changed = changes.changes.values().any(|change| {
        change.old_row.as_ref().map(KotlinSurface::mentions)
            != change.new_row.as_ref().map(KotlinSurface::mentions)
    });
    let receiver_flips = flipped_names(
        store,
        changes,
        |change| (&change.old_receiver_classes, &change.new_receiver_classes),
        kotlin_scope::is_receiver_class,
        unchanged,
    )?;
    let owner_flips = flipped_names(
        store,
        changes,
        |change| (&change.old_owners, &change.new_owners),
        kotlin_scope::is_gradle_owner,
        unchanged,
    )?;
    if seeds.is_empty()
        && !lambdas_changed
        && !mentions_changed
        && receiver_flips.is_empty()
        && owner_flips.is_empty()
    {
        return Ok(refresh);
    }
    let stored: BTreeMap<String, KotlinSurface> = store
        .project_metadata_with_prefix(SURFACE_ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            Some((
                key.strip_prefix(SURFACE_ROW_KEY_PREFIX)?.to_string(),
                KotlinSurface::decode(&value)?,
            ))
        })
        .collect();
    let before = rows_view(&stored, changes, true);
    let after = rows_view(&stored, changes, false);
    let receivers_before = lambda_receivers(&before);
    let receivers_after = lambda_receivers(&after);

    // 1. Heads: the files whose walks can pass through a changed head.
    let mut lambda_names: BTreeSet<String> = BTreeSet::new();
    let mut closure = BTreeSet::new();
    if !seeds.is_empty() {
        closure = head_closure(&before, &after, seeds.clone());
        for receivers in [&receivers_before, &receivers_after] {
            for (name, receiver) in receivers {
                if receiver
                    .as_ref()
                    .is_some_and(|receiver| closure.contains(receiver))
                {
                    lambda_names.insert(name.clone());
                }
            }
        }
    }
    // 2. Lambda receivers that moved.
    if lambdas_changed {
        for name in receivers_before.keys().chain(receivers_after.keys()) {
            if receivers_before.get(name).cloned().flatten()
                != receivers_after.get(name).cloned().flatten()
            {
                lambda_names.insert(name.clone());
            }
        }
    }
    if !closure.is_empty() || !lambda_names.is_empty() {
        for (path, row) in &after {
            if unchanged(path)
                && (row.starts.iter().any(|name| closure.contains(name))
                    || row
                        .lambda_calls
                        .iter()
                        .any(|name| lambda_names.contains(name)))
            {
                refresh.files.insert((*path).to_string());
            }
        }
    }

    // 3. The project-wide receiver types.
    if !seeds.is_empty() || mentions_changed || !receiver_flips.is_empty() {
        let exists_after = |name: &str| {
            store
                .nodes_by_name(name)
                .is_ok_and(|nodes| nodes.iter().any(kotlin_scope::is_receiver_class))
        };
        let exists_before = |name: &str| {
            changes
                .changes
                .values()
                .any(|change| change.old_receiver_classes.contains(name))
                || store.nodes_by_name(name).is_ok_and(|nodes| {
                    nodes.iter().any(|node| {
                        kotlin_scope::is_receiver_class(node) && unchanged(&node.file_path)
                    })
                })
        };
        let types_before = receiver_types(&before, exists_before);
        let types_after = receiver_types(&after, exists_after);
        let flipped: BTreeSet<String> = types_before
            .symmetric_difference(&types_after)
            .cloned()
            .collect();
        if !flipped.is_empty() {
            refresh.names.extend(member_names(
                store,
                &flipped,
                &[NodeKind::Method],
                &[Language::Kotlin, Language::Java],
            )?);
        }
    }

    // 4. A Gradle script's owners.
    if !owner_flips.is_empty()
        && store
            .all_files()?
            .iter()
            .any(|file| file.path.ends_with(".kts"))
    {
        refresh.names.extend(member_names(
            store,
            &owner_flips,
            &[NodeKind::Method, NodeKind::Field, NodeKind::Property],
            &[],
        )?);
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_resolve::kotlin_scope::KotlinHead;

    fn head(name: &str, supertypes: &[&str]) -> KotlinHead {
        KotlinHead {
            name: name.to_string(),
            qualified_name: format!("app::{name}"),
            supertypes: supertypes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn change(old: KotlinSurface, new: KotlinSurface) -> KotlinChanges {
        let mut changes = KotlinChanges::default();
        changes.changes.insert(
            "src/A.kt".to_string(),
            Change {
                old_row: Some(old),
                new_row: Some(new),
                ..Change::default()
            },
        );
        changes
    }

    #[test]
    fn a_changed_head_seeds_its_name_and_closes_over_its_subtypes() {
        let old = KotlinSurface {
            heads: vec![head("Base", &["Root"])],
            ..KotlinSurface::default()
        };
        let new = KotlinSurface {
            heads: vec![head("Base", &["Other"])],
            ..KotlinSurface::default()
        };
        let changes = change(old, new);
        let seeds = head_seeds(&changes);
        assert_eq!(seeds, BTreeSet::from(["Base".to_string()]));
        let child = KotlinSurface {
            heads: vec![head("Child", &["Base"])],
            ..KotlinSurface::default()
        };
        let stored = BTreeMap::from([("src/Child.kt".to_string(), child)]);
        let before = rows_view(&stored, &changes, true);
        let after = rows_view(&stored, &changes, false);
        assert_eq!(
            head_closure(&before, &after, seeds),
            BTreeSet::from(["Base".to_string(), "Child".to_string()])
        );
    }

    #[test]
    fn the_android_table_closes_backwards() {
        let empty = BTreeMap::new();
        let reached = head_closure(
            &empty,
            &empty,
            BTreeSet::from(["FragmentActivity".to_string()]),
        );
        assert!(reached.contains("AppCompatActivity"), "{reached:?}");
    }

    #[test]
    fn lambda_receivers_follow_aliases_in_the_rows() {
        let module = KotlinSurface {
            lambdas: vec![kotlin_scope::LambdaParameter {
                name: "single".to_string(),
                written: "Definition<T>".to_string(),
            }],
            ..KotlinSurface::default()
        };
        let definition = KotlinSurface {
            aliases: vec![kotlin_scope::AliasRhs {
                name: "Definition".to_string(),
                rhs: "Scope.(Int) -> T".to_string(),
            }],
            ..KotlinSurface::default()
        };
        let rows = BTreeMap::from([("a.kt", &module), ("b.kt", &definition)]);
        assert_eq!(
            lambda_receivers(&rows)
                .get("single")
                .cloned()
                .flatten()
                .as_deref(),
            Some("Scope")
        );
    }

    #[test]
    fn rows_round_trip() {
        let row = KotlinSurface {
            heads: vec![head("Base", &["Root"])],
            function_types: vec!["Scope".to_string()],
            starts: vec!["Base".to_string()],
            ..KotlinSurface::default()
        };
        assert_eq!(KotlinSurface::decode(&row.encode()), Some(row));
    }
}
