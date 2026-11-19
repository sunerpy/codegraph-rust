//! The React Native, Expo and Swift-Objective-C bridge lane of a sync, its
//! rows and its crash marker (W7-21).
//!
//! The bridges' resolution reads what no node records: a JavaScript file's
//! `NativeModules` aliases and `requireNativeModule` bindings, an
//! Objective-C file's `RCT_EXPORT_*`, `RCT_REMAP_*` and `RCT_EXTERN_*`
//! entries, a JVM module's name and `@ReactMethod`s, a TurboModule spec,
//! and whether a Swift method is exposed to Objective-C
//! ([`codegraph_resolve::frameworks::react_native::BridgeFacts`]). Every full
//! index and migration writes a row per file holding any of them
//! (`rn_bridge_surface:{path}`), a sync's resolution reads the rows of the
//! files it did not change instead of every file's text, and a sync writes
//! its changed files' rows last.
//!
//! A sync compares each changed file's row with its new facts, and its
//! Objective-C and JVM method names before and after. The seeds:
//!
//! - an alias or binding whose name came, went or names another module:
//!   the JavaScript references written through that name;
//! - an export, `RCT_EXTERN`, JVM or spec entry that came, went or changed:
//!   the JavaScript references whose last segment is its JavaScript name;
//! - an Objective-C method that came or went: the Swift references whose
//!   last segment is a base name of its selector
//!   ([`swift_base_names_for_objc_selector`]), and the JavaScript references
//!   whose last segment is its first keyword or the JavaScript name of an
//!   export of that native keyword;
//! - a JVM method that came or went: the JavaScript references whose last
//!   segment is its name;
//! - a Swift method or function that came, went, or changed its qualified
//!   name or its exposure: the Objective-C selector references one of whose
//!   base names is its name, and the JavaScript references whose last
//!   segment is the JavaScript name of an `RCT_EXTERN` entry of that native
//!   keyword;
//! - a file whose exported `requireNativeModule` binding changed: the files
//!   that resolved into it before the change, whole;
//! - a node of a changed file, of any language and kind, that came, went,
//!   moved or changed anything else name matching reads of it
//!   ([`rival_keys`]), compared once the framework nodes are back: the
//!   JavaScript-family, Swift and Objective-C references whose last segment
//!   is its name, case aside, that a bridge answered below 0.9 (an
//!   Objective-C one only through a selector, a JavaScript one only through
//!   an entry's JavaScript name), since such an answer gives way to a
//!   candidate in the reference's own language and to any more confident
//!   one (KEEP-RUST, W7-21).
//!
//! The unresolved references are retried by their whole names and the
//! resolved ones at their sites, however many there are. The lane compares
//! only when the index recorded a bridge reading the rows; otherwise a
//! bridge this sync detects is a framework change, which re-extracts every
//! file. A sync withdraws [`VERSION_KEY`] before its first change and writes
//! it back once everything resolved; an index without it rebuilds every row
//! and re-resolves every JavaScript-family, Swift and Objective-C file of a
//! project a bridge was detected in.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Edge, Language, Node, NodeKind};
use codegraph_resolve::frameworks::react_native::{
    BridgeEntry, SwiftMethodFact, is_js_family_path,
};
pub use codegraph_resolve::frameworks::react_native::{BridgeFacts, ROW_KEY_PREFIX, row_key};
use codegraph_resolve::frameworks::swift_objc::swift_base_names_for_objc_selector;
use codegraph_resolve::resolver::FILL_IN_BRIDGES;
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, LaneReference, LaneReferences};

/// Metadata key present when no sync left a bridge reference unretried.
pub const VERSION_KEY: &str = "rn_bridges_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// The bridges whose resolution reads the rows.
pub const READERS: [&str; 3] = [
    "swift-objc-bridge",
    "react-native-bridge",
    "expo-modules-js",
];

/// Every bridge framework (W7-21).
pub const BRIDGES: [&str; 5] = [
    "swift-objc-bridge",
    "react-native-bridge",
    "expo-modules",
    "expo-modules-js",
    "fabric-view",
];

/// The languages whose references the React Native and Expo bridges read.
const JS_FAMILY: [Language; 4] = [
    Language::JavaScript,
    Language::TypeScript,
    Language::Tsx,
    Language::Jsx,
];

/// Whether `path` can hold bridge facts.
pub fn is_row_file(path: &str) -> bool {
    path.ends_with(".m")
        || path.ends_with(".mm")
        || path.ends_with(".java")
        || path.ends_with(".kt")
        || path.ends_with(".swift")
        || is_js_family_path(path)
}

/// `source`'s row, `None` when it records nothing: a file no bridge reads,
/// one holding no fact, or one that is not text.
pub fn row_of(path: &str, source: &SourceText, nodes: &[Node]) -> Option<BridgeFacts> {
    match source {
        SourceText::Text(text) => row_of_text(path, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(path: &str, text: &str, nodes: &[Node]) -> Option<BridgeFacts> {
    if !is_row_file(path) {
        return None;
    }
    let facts = BridgeFacts::of_file(path, text, nodes);
    (!facts.is_empty()).then_some(facts)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, BridgeFacts)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs its rows rebuilt and its bridge references
/// re-resolved.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no bridge reference is pending and every row is current: a
/// full index, a migration, or a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

fn read_row(store: &Store, path: &str) -> Result<Option<BridgeFacts>> {
    Ok(store
        .get_project_metadata(&row_key(path))?
        .and_then(|value| BridgeFacts::decode(&value)))
}

/// A method as the bridges' tables key it: its language (by name) and its
/// name.
type NativeMethod = (&'static str, String);

/// The Objective-C and JVM method names among `nodes`, each with its
/// language, sorted with their repeats: what the bridges' tables key
/// methods by.
fn native_methods(nodes: &[Node]) -> Vec<NativeMethod> {
    let mut out: Vec<NativeMethod> = nodes
        .iter()
        .filter(|node| {
            node.kind == NodeKind::Method
                && matches!(
                    node.language,
                    Language::ObjC | Language::Java | Language::Kotlin
                )
        })
        .map(|node| (node.language.as_str(), node.name.clone()))
        .collect();
    out.sort();
    out
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<BridgeFacts>,
    new_row: Option<BridgeFacts>,
    /// Its Objective-C and JVM methods before the sync's first change to it
    /// (when the lane compares).
    old_methods: Vec<NativeMethod>,
    /// Its stored nodes before the sync's first change to it, framework
    /// nodes included (when the lane compares): the lane reads them again
    /// once the framework nodes are back, for its [`Rivals`].
    old_nodes: Vec<Node>,
    /// The files that resolved into it before the change.
    importers: BTreeSet<String>,
}

/// What a sync's changed files held for this lane.
#[derive(Debug, Default)]
pub(crate) struct BridgeChanges {
    /// Whether the lane compares: the index recorded a bridge reading the
    /// rows.
    pub(crate) track: bool,
    changes: BTreeMap<String, Change>,
}

impl BridgeChanges {
    /// `path` changed (its old row still in the store, its stored nodes
    /// `old_nodes`) to `new` (its text and nodes), or left the index
    /// (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        old_nodes: &[Node],
        new: Option<(&SourceText, &[Node])>,
    ) -> Result<()> {
        let new_row = new.and_then(|(source, nodes)| row_of(path, source, nodes));
        if let Some(change) = self.changes.get_mut(path) {
            // A file changed twice in one sync keeps what it held before the
            // first change.
            change.new_row = new_row;
            return Ok(());
        }
        let old_row = read_row(store, path)?;
        // Where the lane compares, every changed file is kept: a node of any
        // of them may rival a bridge's answer.
        if !self.track && old_row.is_none() && new_row.is_none() {
            return Ok(());
        }
        let (old_methods, old_nodes) = if self.track {
            (native_methods(old_nodes), old_nodes.to_vec())
        } else {
            (Vec::new(), Vec::new())
        };
        self.changes.insert(
            path.to_string(),
            Change {
                old_row,
                new_row,
                old_methods,
                old_nodes,
                importers: BTreeSet::new(),
            },
        );
        Ok(())
    }

    /// The files that resolved into `path` before this sync changed it.
    pub(crate) fn note_importers(&mut self, path: &str, importers: &BTreeSet<String>) {
        if let Some(change) = self.changes.get_mut(path) {
            change.importers.extend(importers.iter().cloned());
        }
    }
}

/// What a sync's bridge lane calls for.
#[derive(Debug, Default)]
pub(crate) struct BridgeRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// The unresolved references' names, retried whole.
    pub(crate) names: BTreeSet<String>,
    /// The resolved references' sites.
    pub(crate) sites: BTreeSet<FileReferenceSite>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<BridgeFacts>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl BridgeRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = row_key(path);
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

/// The names a sync's changes move, by what the bridges read them as.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Seeds {
    /// The last segments of the JavaScript references to retry.
    pub(crate) js_tails: BTreeSet<String>,
    /// The first segments (receivers) of the JavaScript references to retry.
    pub(crate) js_receivers: BTreeSet<String>,
    /// The last segments of the Swift references to retry.
    pub(crate) swift_tails: BTreeSet<String>,
    /// The Swift names an Objective-C selector reference to retry has as a
    /// base name.
    pub(crate) objc_base_names: BTreeSet<String>,
    /// The changed files whose exported bindings moved.
    pub(crate) exporters: BTreeSet<String>,
}

/// The entries of `facts` an entry seed compares: exports, `RCT_EXTERN`
/// entries, JVM exports and spec methods, as `(kind, module, class,
/// js_name, native_kw)`.
fn entries(facts: Option<&BridgeFacts>) -> BTreeSet<(u8, String, String, String, String)> {
    let mut out = BTreeSet::new();
    let Some(facts) = facts else {
        return out;
    };
    let entry = |kind: u8, entry: &BridgeEntry| {
        (
            kind,
            entry.module.clone(),
            entry.class.clone(),
            entry.js_name.clone(),
            entry.native_kw.clone(),
        )
    };
    out.extend(facts.exports.iter().map(|export| entry(0, export)));
    out.extend(facts.externs.iter().map(|shim| entry(1, shim)));
    out.extend(facts.jvm_exports.iter().map(|(module, js_name)| {
        (
            2,
            module.clone(),
            String::new(),
            js_name.clone(),
            js_name.clone(),
        )
    }));
    if let Some(spec) = &facts.turbo_spec {
        out.extend(spec.methods.iter().map(|method| {
            (
                3,
                spec.module.clone(),
                String::new(),
                method.clone(),
                method.clone(),
            )
        }));
    }
    out
}

/// What `facts` binds by name: `NativeModules` aliases and own-file
/// `requireNativeModule` bindings.
fn bindings(facts: Option<&BridgeFacts>) -> BTreeSet<(String, String, Option<String>)> {
    let mut out = BTreeSet::new();
    let Some(facts) = facts else {
        return out;
    };
    out.extend(
        facts
            .aliases
            .iter()
            .map(|(alias, module)| (alias.clone(), module.clone(), None)),
    );
    out.extend(facts.expo.locals.iter().map(|(local, binding)| {
        (
            local.clone(),
            binding.module.clone(),
            binding.type_name.clone(),
        )
    }));
    out
}

/// The Swift methods of `facts`, sorted with their repeats.
fn swift_facts(facts: Option<&BridgeFacts>) -> Vec<SwiftMethodFact> {
    facts
        .map(|facts| facts.swift_methods.clone())
        .unwrap_or_default()
}

/// The items one sorted list holds a different number of times than the
/// other.
fn multiset_changes<T: Ord + Clone>(old: &[T], new: &[T]) -> BTreeSet<T> {
    let count = |items: &[T]| {
        let mut out: BTreeMap<T, usize> = BTreeMap::new();
        for item in items {
            *out.entry(item.clone()).or_default() += 1;
        }
        out
    };
    let (old, new) = (count(old), count(new));
    old.keys()
        .chain(new.keys())
        .filter(|item| old.get(*item) != new.get(*item))
        .cloned()
        .collect()
}

/// The first keyword of an Objective-C selector.
fn first_keyword(selector: &str) -> &str {
    selector.split(':').next().unwrap_or(selector)
}

/// The seeds of `changes`: `new_methods` gives a changed file's Objective-C
/// and JVM methods now, and `all_rows` every row before and after the
/// changes (the stored rows of the unchanged files, both rows of the
/// changed ones).
fn seeds<'r>(
    changes: &BridgeChanges,
    new_methods: &BTreeMap<String, Vec<NativeMethod>>,
    all_rows: impl Iterator<Item = &'r BridgeFacts> + Clone,
) -> Seeds {
    let mut out = Seeds::default();
    let mut objc_keywords: BTreeSet<String> = BTreeSet::new();
    let mut swift_names: BTreeSet<String> = BTreeSet::new();
    for (path, change) in &changes.changes {
        let (old, new) = (change.old_row.as_ref(), change.new_row.as_ref());
        for (_, _, _, js_name, _) in entries(old).symmetric_difference(&entries(new)) {
            out.js_tails.insert(js_name.clone());
        }
        for (name, _, _) in bindings(old).symmetric_difference(&bindings(new)) {
            out.js_receivers.insert(name.clone());
        }
        let exported = |facts: Option<&BridgeFacts>| {
            facts.map(|facts| {
                (
                    facts.expo.default_export.clone(),
                    facts.expo.named_exports.clone(),
                )
            })
        };
        if exported(old).unwrap_or_default() != exported(new).unwrap_or_default() {
            out.exporters.insert(path.clone());
        }
        for fact in multiset_changes(&swift_facts(old), &swift_facts(new)) {
            swift_names.insert(fact.0);
        }
        let now = new_methods.get(path).map(Vec::as_slice).unwrap_or_default();
        for (language, name) in multiset_changes(&change.old_methods, now) {
            if language == Language::ObjC.as_str() {
                objc_keywords.insert(first_keyword(&name).to_string());
                out.swift_tails
                    .extend(swift_base_names_for_objc_selector(&name));
            } else {
                out.js_tails.insert(name);
            }
        }
    }
    // A method's first keyword names the exports of that native keyword and
    // a spec's method; a Swift method's name, the `RCT_EXTERN` entries of
    // that native keyword, on any class.
    out.js_tails.extend(objc_keywords.iter().cloned());
    for facts in all_rows {
        for export in &facts.exports {
            if objc_keywords.contains(&export.native_kw) {
                out.js_tails.insert(export.js_name.clone());
            }
        }
        for shim in &facts.externs {
            if swift_names.contains(&shim.native_kw) {
                out.js_tails.insert(shim.js_name.clone());
            }
        }
    }
    out.objc_base_names = swift_names;
    out
}

/// Whether the references `seeds` move include `reference`, a reference of
/// `family`'s languages.
fn moved(seeds: &Seeds, family: Family, reference: &LaneReference) -> bool {
    let name = reference.reference_name.as_str();
    let tail = name.rsplit('.').next().unwrap_or(name);
    match family {
        Family::Js => {
            seeds.js_tails.contains(tail)
                || name
                    .split_once('.')
                    .is_some_and(|(receiver, _)| seeds.js_receivers.contains(receiver))
        }
        Family::Swift => seeds.swift_tails.contains(tail),
        Family::ObjC => {
            tail.contains(':')
                && swift_base_names_for_objc_selector(tail)
                    .iter()
                    .any(|base| seeds.objc_base_names.contains(base))
        }
    }
}

/// A node as a rival of a bridge's answer: its name, lowercased, its id
/// (which holds its kind and line) and what else name matching reads of it
/// ([`crate::sync::candidate_key`]), so a node that came, went, moved or
/// changed differs.
type RivalKey = (String, String, String);

/// The [`RivalKey`]s of `nodes`, sorted with their repeats: every node, of
/// any language and kind.
fn rival_keys(nodes: &[Node]) -> Vec<RivalKey> {
    let mut out: Vec<RivalKey> = nodes
        .iter()
        .map(|node| {
            (
                node.name.to_lowercase(),
                node.id.clone(),
                crate::sync::candidate_key(node),
            )
        })
        .collect();
    out.sort();
    out
}

/// The names, lowercased, of the nodes that came, went, moved or changed
/// between `before` and `after`.
fn changed_names(before: &[Node], after: &[Node]) -> BTreeSet<String> {
    multiset_changes(&rival_keys(before), &rival_keys(after))
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// The JavaScript names `facts` exports: what the React Native bridge
/// answers a call's last segment by.
fn js_names(facts: &BridgeFacts) -> impl Iterator<Item = &str> {
    facts
        .exports
        .iter()
        .chain(&facts.externs)
        .map(|entry| entry.js_name.as_str())
        .chain(
            facts
                .jvm_exports
                .iter()
                .map(|(_, js_name)| js_name.as_str()),
        )
        .chain(
            facts
                .turbo_spec
                .iter()
                .flat_map(|spec| spec.methods.iter().map(String::as_str)),
        )
}

/// The names a sync's changes rival a bridge's answer with (KEEP-RUST,
/// W7-21): a bridge's answer below `0.9` gives way to a candidate in its
/// reference's language and to any more confident one, of any language, so
/// a node named like a reference's last segment that comes, goes or changes
/// can move a reference the bridge answered. Names are lowercased, as a
/// fuzzy match reads them.
#[derive(Debug, Default, PartialEq, Eq)]
struct Rivals {
    /// The names of the changed files' nodes that came, went, moved or
    /// changed since the sync began.
    names: BTreeSet<String>,
    /// Those an entry of the rows names as its JavaScript name: the React
    /// Native bridge answers a JavaScript reference only through one.
    js: BTreeSet<String>,
}

/// The [`Rivals`] of `changes`, `current` holding each changed file's
/// stored nodes now (none for one that left the index); `all_rows` as
/// [`seeds`] reads them.
fn rivals<'r>(
    changes: &BridgeChanges,
    current: &BTreeMap<String, Vec<Node>>,
    all_rows: impl Iterator<Item = &'r BridgeFacts>,
) -> Rivals {
    let mut names = BTreeSet::new();
    for (path, change) in &changes.changes {
        let now = current.get(path).map(Vec::as_slice).unwrap_or_default();
        names.extend(changed_names(&change.old_nodes, now));
    }
    let js = if names.is_empty() {
        BTreeSet::new()
    } else {
        let exported: BTreeSet<String> =
            all_rows.flat_map(js_names).map(str::to_lowercase).collect();
        names.intersection(&exported).cloned().collect()
    };
    Rivals { names, js }
}

/// Whether a node `rivals` holds is named like `reference`'s last segment,
/// a reference of `family`'s languages.
fn rivaled(rivals: &Rivals, family: Family, reference: &LaneReference) -> bool {
    let name = reference.reference_name.as_str();
    let tail = name.rsplit('.').next().unwrap_or(name).to_lowercase();
    match family {
        Family::Js => rivals.js.contains(&tail),
        Family::Swift => rivals.names.contains(&tail),
        // The bridge answers an Objective-C message through its selector
        // alone.
        Family::ObjC => tail.contains(':') && rivals.names.contains(&tail),
    }
}

/// Whether the edge a reference at `site` resolved to is a
/// [`FILL_IN_BRIDGES`] bridge's answer below `0.9`. `edges` keeps each
/// source's edges once read.
fn answered_by_a_bridge(
    store: &Store,
    site: &FileReferenceSite,
    edges: &mut HashMap<String, Vec<Edge>>,
) -> Result<bool> {
    let Some(at) = &site.site else {
        return Ok(false);
    };
    if !edges.contains_key(&at.from_node_id) {
        let found = store.edges_by_source_kind(&at.from_node_id, None)?;
        edges.insert(at.from_node_id.clone(), found);
    }
    Ok(edges[&at.from_node_id].iter().any(|edge| {
        edge.line == Some(at.line) && edge.col == Some(at.col) && is_fill_in_bridge_edge(edge)
    }))
}

/// Whether `edge` is a [`FILL_IN_BRIDGES`] bridge's answer below `0.9`.
fn is_fill_in_bridge_edge(edge: &Edge) -> bool {
    let Some(metadata) = edge.metadata.as_ref() else {
        return false;
    };
    metadata
        .get("framework")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|framework| FILL_IN_BRIDGES.contains(&framework))
        && metadata
            .get("confidence")
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|confidence| confidence < 0.9)
}

/// The reference families the lane retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Js,
    Swift,
    ObjC,
}

/// Read what this lane calls for, after every changed file is re-extracted
/// with its framework nodes (the store holds the new nodes; the rows are
/// still the old ones). With `bootstrap`, every row is rebuilt from disk
/// instead and, when `bridged` (a bridge is detected), every
/// JavaScript-family, Swift and Objective-C file re-resolves. `retry` says
/// whether the lane compares: it tracked the changes and a bridge reading
/// the rows is detected.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bridge_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &BridgeChanges,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
    bridged: bool,
    retry: bool,
) -> Result<BridgeRefresh> {
    let mut refresh = BridgeRefresh::default();
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if bridged
                && unchanged(&file.path)
                && (JS_FAMILY.contains(&file.language)
                    || matches!(file.language, Language::Swift | Language::ObjC))
            {
                refresh.files.insert(file.path.clone());
            }
            if !is_row_file(&file.path) {
                continue;
            }
            let row = match changes.changes.get(&file.path) {
                Some(change) => change.new_row.clone(),
                None => read_text(project_root, &file.path, max_file_size).and_then(|text| {
                    store
                        .nodes_by_file_path(&file.path)
                        .ok()
                        .and_then(|nodes| row_of_text(&file.path, &text, &nodes))
                }),
            };
            refresh.rows.insert(file.path, row);
        }
        return Ok(refresh);
    }
    for (path, change) in &changes.changes {
        if is_row_file(path) {
            refresh.rows.insert(path.clone(), change.new_row.clone());
        }
    }
    if !retry || changes.changes.is_empty() {
        return Ok(refresh);
    }
    // The changed files' stored nodes now, the framework nodes back.
    let mut current: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for path in changes.changes.keys() {
        if !removed.contains(path) {
            current.insert(path.clone(), store.nodes_by_file_path(path)?);
        }
    }
    let new_methods: BTreeMap<String, Vec<NativeMethod>> = current
        .iter()
        .map(|(path, nodes)| (path.clone(), native_methods(nodes)))
        .collect();
    let stored: Vec<BridgeFacts> = store
        .project_metadata_with_prefix(ROW_KEY_PREFIX)?
        .into_iter()
        .filter_map(|(key, value)| {
            let path = key.strip_prefix(ROW_KEY_PREFIX)?;
            if changes.changes.contains_key(path) {
                return None;
            }
            BridgeFacts::decode(&value)
        })
        .collect();
    let all_rows = stored.iter().chain(
        changes
            .changes
            .values()
            .flat_map(|change| change.old_row.iter().chain(change.new_row.iter())),
    );
    let rivals = rivals(changes, &current, all_rows.clone());
    let seeds = seeds(changes, &new_methods, all_rows);
    for path in &seeds.exporters {
        if let Some(change) = changes.changes.get(path) {
            refresh.files.extend(
                change
                    .importers
                    .iter()
                    .filter(|importer| unchanged(importer))
                    .cloned(),
            );
        }
    }
    let families: [(Family, Vec<Language>, bool, bool); 3] = [
        (
            Family::Js,
            JS_FAMILY.to_vec(),
            !seeds.js_tails.is_empty() || !seeds.js_receivers.is_empty(),
            !rivals.js.is_empty(),
        ),
        (
            Family::Swift,
            vec![Language::Swift],
            !seeds.swift_tails.is_empty(),
            !rivals.names.is_empty(),
        ),
        (
            Family::ObjC,
            vec![Language::ObjC],
            !seeds.objc_base_names.is_empty(),
            rivals.names.iter().any(|name| name.contains(':')),
        ),
    ];
    let mut edges: HashMap<String, Vec<Edge>> = HashMap::new();
    for (family, languages, wanted, rivaled_names) in families {
        if !wanted && !rivaled_names {
            continue;
        }
        for reference in store.lane_references(&LaneReferences::Languages(languages))? {
            if !unchanged(&reference.site.file_path) {
                continue;
            }
            if moved(&seeds, family, &reference) {
                if reference.resolved {
                    refresh.sites.insert(reference.site);
                } else {
                    refresh.names.insert(reference.reference_name);
                }
            } else if reference.resolved
                && rivaled(&rivals, family, &reference)
                && answered_by_a_bridge(store, &reference.site, &mut edges)?
            {
                // A bridge's weak answer meets a node of its reference's
                // name: a candidate may take its place.
                refresh.sites.insert(reference.site);
            }
        }
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_resolve::frameworks::expo_modules::{ExpoBinding, ExpoBindings};
    use codegraph_resolve::frameworks::react_native::TurboSpec;

    fn entry(module: &str, class: &str, js_name: &str, native_kw: &str) -> BridgeEntry {
        BridgeEntry {
            module: module.to_string(),
            class: class.to_string(),
            js_name: js_name.to_string(),
            native_kw: native_kw.to_string(),
        }
    }

    fn change(old: BridgeFacts, new: BridgeFacts) -> BridgeChanges {
        let mut changes = BridgeChanges {
            track: true,
            ..BridgeChanges::default()
        };
        changes.changes.insert(
            "f".to_string(),
            Change {
                old_row: Some(old),
                new_row: Some(new),
                ..Change::default()
            },
        );
        changes
    }

    fn names(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    #[test]
    fn an_entry_that_came_went_or_changed_seeds_its_js_name() {
        let old = BridgeFacts {
            exports: vec![
                entry("Cam", "", "kept", "kept"),
                entry("Cam", "", "gone", "gone"),
                entry("Cam", "", "remapped", "native"),
            ],
            externs: vec![entry("Cap", "CaptureView", "sync", "sync")],
            jvm_exports: vec![("Cam".to_string(), "jvmGone".to_string())],
            turbo_spec: Some(TurboSpec {
                module: "Calc".to_string(),
                methods: vec!["add".to_string()],
            }),
            ..BridgeFacts::default()
        };
        let new = BridgeFacts {
            exports: vec![
                entry("Cam", "", "kept", "kept"),
                entry("Cam", "", "remapped", "other"),
            ],
            externs: vec![entry("Cap", "CaptureViewImpl", "sync", "sync")],
            turbo_spec: Some(TurboSpec {
                module: "Calc".to_string(),
                methods: vec!["add".to_string(), "sub".to_string()],
            }),
            ..BridgeFacts::default()
        };
        let found = seeds(&change(old, new), &BTreeMap::new(), std::iter::empty());
        assert_eq!(
            names(&found.js_tails),
            ["gone", "jvmGone", "remapped", "sub", "sync"]
        );
        assert!(found.js_receivers.is_empty());
        assert!(found.exporters.is_empty());
    }

    #[test]
    fn a_binding_seeds_its_name_and_an_exported_one_its_importers() {
        let binding = |module: &str| ExpoBinding {
            module: module.to_string(),
            type_name: None,
        };
        let old = BridgeFacts {
            aliases: vec![
                ("kept".to_string(), "A".to_string()),
                ("moved".to_string(), "A".to_string()),
            ],
            expo: ExpoBindings {
                locals: vec![("Haptics".to_string(), binding("ExpoHaptics"))],
                default_export: Some(binding("ExpoCamera")),
                named_exports: Vec::new(),
            },
            ..BridgeFacts::default()
        };
        let new = BridgeFacts {
            aliases: vec![
                ("kept".to_string(), "A".to_string()),
                ("moved".to_string(), "B".to_string()),
            ],
            expo: ExpoBindings {
                locals: vec![("Haptics".to_string(), binding("ExpoHaptics"))],
                default_export: Some(binding("ExpoCamera2")),
                named_exports: Vec::new(),
            },
            ..BridgeFacts::default()
        };
        let found = seeds(&change(old, new), &BTreeMap::new(), std::iter::empty());
        assert_eq!(names(&found.js_receivers), ["moved"]);
        assert_eq!(names(&found.exporters), ["f"]);
        assert!(found.js_tails.is_empty());
    }

    #[test]
    fn methods_seed_by_platform() {
        let mut changes = change(
            BridgeFacts {
                swift_methods: vec![
                    SwiftMethodFact("animate".to_string(), "Chart::animate".to_string(), false),
                    SwiftMethodFact("kept".to_string(), "Chart::kept".to_string(), true),
                    SwiftMethodFact("moved".to_string(), "A::moved".to_string(), true),
                ],
                ..BridgeFacts::default()
            },
            BridgeFacts {
                swift_methods: vec![
                    SwiftMethodFact("animate".to_string(), "Chart::animate".to_string(), true),
                    SwiftMethodFact("kept".to_string(), "Chart::kept".to_string(), true),
                    SwiftMethodFact("moved".to_string(), "B::moved".to_string(), true),
                ],
                ..BridgeFacts::default()
            },
        );
        changes.changes.get_mut("f").unwrap().old_methods = vec![
            ("java", "jvmKept".to_string()),
            ("objc", "fetchEntryForKey:".to_string()),
        ];
        let now = BTreeMap::from([(
            "f".to_string(),
            vec![
                ("java", "jvmKept".to_string()),
                ("kotlin", "jvmNew".to_string()),
                ("objc", "pauseInferenceNow".to_string()),
            ],
        )]);
        let rows = [BridgeFacts {
            exports: vec![entry("Cam", "", "pause", "pauseInferenceNow")],
            externs: vec![
                entry("Cap", "Other", "doMove", "moved"),
                entry("Cap", "Chart", "doKept", "kept"),
            ],
            ..BridgeFacts::default()
        }];
        let found = seeds(&changes, &now, rows.iter());
        assert_eq!(
            names(&found.js_tails),
            [
                "doMove",
                "fetchEntryForKey",
                "jvmNew",
                "pause",
                "pauseInferenceNow"
            ]
        );
        assert_eq!(
            names(&found.swift_tails),
            ["fetchEntry", "fetchEntryForKey", "pauseInferenceNow"]
        );
        assert_eq!(names(&found.objc_base_names), ["animate", "moved"]);
    }

    fn reference(name: &str) -> LaneReference {
        LaneReference {
            site: FileReferenceSite {
                file_path: "src/a.ts".to_string(),
                site: None,
            },
            reference_name: name.to_string(),
            reference_kind: codegraph_core::types::EdgeKind::Calls,
            resolved: false,
        }
    }

    #[test]
    fn a_reference_moves_by_its_last_segment_or_its_receiver() {
        let seeds = Seeds {
            js_tails: BTreeSet::from(["pause".to_string()]),
            js_receivers: BTreeSet::from(["captureView".to_string()]),
            swift_tails: BTreeSet::from(["fetchEntry".to_string()]),
            objc_base_names: BTreeSet::from(["animate".to_string()]),
            exporters: BTreeSet::new(),
        };
        assert!(moved(
            &seeds,
            Family::Js,
            &reference("NativeModules.Cam.pause")
        ));
        assert!(moved(&seeds, Family::Js, &reference("pause")));
        assert!(moved(&seeds, Family::Js, &reference("captureView.finish")));
        assert!(!moved(&seeds, Family::Js, &reference("captureView")));
        assert!(!moved(&seeds, Family::Js, &reference("other.finish")));
        assert!(moved(&seeds, Family::Swift, &reference("cache.fetchEntry")));
        assert!(!moved(&seeds, Family::Swift, &reference("cache.fetch")));
        assert!(moved(
            &seeds,
            Family::ObjC,
            &reference("chart.animateWithXAxisDuration:yAxisDuration:")
        ));
        // A colonless message names no selector.
        assert!(!moved(&seeds, Family::ObjC, &reference("chart.animate")));
    }

    fn declaration(kind: NodeKind, name: &str, language: Language) -> Node {
        Node {
            id: format!("{}:{name}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "f".to_string(),
            language,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
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

    /// KEEP-RUST (W7-21): a changed file's node of any language and kind
    /// that came, went, moved or changed anything else name matching reads
    /// of it (a qualified name, the case of its name) rivals the bridge's
    /// weak answers, lowercased; one an entry of the rows names as its
    /// JavaScript name rivals the JavaScript references' too. A node stored
    /// again as it was (a framework node re-extracted, a new timestamp)
    /// rivals nothing; a file that left the index rivals with every node.
    #[test]
    fn a_node_of_any_language_that_came_went_or_changed_is_a_rival() {
        let moved = |node: Node, line: i64| Node {
            id: format!("{}@{line}", node.id),
            start_line: line,
            end_line: line,
            ..node
        };
        let qualified = |node: Node, qualified_name: &str| Node {
            qualified_name: qualified_name.to_string(),
            ..node
        };
        let framework = |updated_at: i64| Node {
            id: "rn-export:f:Deck.rewind".to_string(),
            updated_at,
            ..declaration(NodeKind::Method, "rewind", Language::ObjC)
        };
        let old = vec![
            declaration(NodeKind::Method, "objectAtIndex:", Language::ObjC),
            declaration(NodeKind::Method, "first", Language::ObjC),
            declaration(NodeKind::Function, "kept", Language::Swift),
            declaration(NodeKind::Function, "getItem", Language::TypeScript),
            declaration(NodeKind::Method, "fetchEntry", Language::Cpp),
            qualified(
                declaration(NodeKind::Class, "Badge", Language::Kotlin),
                "a::Badge",
            ),
            declaration(NodeKind::Function, "refresh", Language::TypeScript),
            framework(0),
        ];
        let new = vec![
            declaration(NodeKind::Method, "first", Language::ObjC),
            Node {
                updated_at: 7,
                ..declaration(NodeKind::Function, "kept", Language::Swift)
            },
            declaration(NodeKind::Function, "getItem", Language::TypeScript),
            moved(
                declaration(NodeKind::Method, "fetchEntry", Language::Cpp),
                9,
            ),
            qualified(
                declaration(NodeKind::Class, "Badge", Language::Kotlin),
                "b::Badge",
            ),
            declaration(NodeKind::Function, "Refresh", Language::TypeScript),
            framework(7),
            declaration(NodeKind::Method, "reload", Language::ObjC),
            declaration(NodeKind::Function, "Pause", Language::Tsx),
            declaration(NodeKind::Function, "removeItem", Language::JavaScript),
            declaration(NodeKind::Function, "unexported", Language::Jsx),
            declaration(NodeKind::Import, "setItem", Language::TypeScript),
            declaration(NodeKind::File, "multiGet", Language::TypeScript),
            declaration(NodeKind::Method, "clear", Language::Java),
            declaration(NodeKind::Method, "seek", Language::ArkTs),
        ];
        let mut changes = BridgeChanges {
            track: true,
            ..BridgeChanges::default()
        };
        changes.changes.insert(
            "f".to_string(),
            Change {
                old_nodes: old,
                ..Change::default()
            },
        );
        changes.changes.insert(
            "gone.py".to_string(),
            Change {
                old_nodes: vec![declaration(NodeKind::Function, "Gone", Language::Python)],
                ..Change::default()
            },
        );
        let current = BTreeMap::from([("f".to_string(), new)]);
        let rows = [BridgeFacts {
            exports: vec![entry("Player", "", "pause", "pauseInferenceNow")],
            externs: vec![entry("Store", "Store", "setItem", "setItem")],
            jvm_exports: vec![
                ("Store".to_string(), "removeItem".to_string()),
                ("Store".to_string(), "clear".to_string()),
            ],
            turbo_spec: Some(TurboSpec {
                module: "Store".to_string(),
                methods: vec!["getItem".to_string(), "multiGet".to_string()],
            }),
            ..BridgeFacts::default()
        }];
        let found = rivals(&changes, &current, rows.iter());
        assert_eq!(
            names(&found.names),
            [
                "badge",
                "clear",
                "fetchentry",
                "gone",
                "multiget",
                "objectatindex:",
                "pause",
                "refresh",
                "reload",
                "removeitem",
                "seek",
                "setitem",
                "unexported",
            ]
        );
        assert_eq!(
            names(&found.js),
            ["clear", "multiget", "pause", "removeitem", "setitem"]
        );
    }

    /// KEEP-RUST (W7-21): a rival meets a reference by its last segment,
    /// case aside: a Swift reference by any rival's name, whatever its
    /// language; a JavaScript one by a name an entry gives; an Objective-C
    /// one only through a selector, the only name the bridge answers it by.
    #[test]
    fn a_rival_meets_a_references_last_segment_case_aside() {
        let rivals = Rivals {
            names: ["objectatindex:", "fetchentry", "pause", "first"]
                .map(String::from)
                .into_iter()
                .collect(),
            js: BTreeSet::from(["pause".to_string()]),
        };
        assert!(rivaled(&rivals, Family::ObjC, &reference("objectAtIndex:")));
        assert!(rivaled(
            &rivals,
            Family::ObjC,
            &reference("items.objectAtIndex:")
        ));
        assert!(!rivaled(&rivals, Family::ObjC, &reference("objectAtIndex")));
        assert!(!rivaled(&rivals, Family::ObjC, &reference("first")));
        assert!(rivaled(&rivals, Family::Swift, &reference("fetchEntry")));
        assert!(rivaled(
            &rivals,
            Family::Swift,
            &reference("cache.fetchEntry")
        ));
        assert!(rivaled(&rivals, Family::Swift, &reference("pause")));
        assert!(!rivaled(&rivals, Family::Swift, &reference("reload")));
        assert!(rivaled(&rivals, Family::Js, &reference("Pause")));
        assert!(rivaled(&rivals, Family::Js, &reference("player.pause")));
        assert!(!rivaled(&rivals, Family::Js, &reference("fetchEntry")));
        assert!(!rivaled(&rivals, Family::Js, &reference("pauseAll")));
        assert!(!rivaled(&rivals, Family::Js, &reference("pause.all")));
    }

    /// KEEP-RUST (W7-21): a rival retries a reference only where a bridge
    /// that fills in answered it below 0.9.
    #[test]
    fn a_rival_retries_only_a_fill_in_bridges_weak_answer() {
        let edge = |metadata: serde_json::Value| Edge {
            id: None,
            source: "method:a".to_string(),
            target: "method:b".to_string(),
            kind: codegraph_core::types::EdgeKind::Calls,
            metadata: Some(metadata),
            line: Some(3),
            col: Some(4),
            provenance: None,
        };
        for framework in ["swift-objc-bridge", "react-native-bridge"] {
            assert!(is_fill_in_bridge_edge(&edge(serde_json::json!({
                "framework": framework,
                "confidence": 0.6,
            }))));
            assert!(!is_fill_in_bridge_edge(&edge(serde_json::json!({
                "framework": framework,
                "confidence": 0.95,
            }))));
        }
        assert!(!is_fill_in_bridge_edge(&edge(serde_json::json!({
            "framework": "expo-modules-js",
            "confidence": 0.6,
        }))));
        assert!(!is_fill_in_bridge_edge(&edge(serde_json::json!({
            "confidence": 0.4,
            "resolvedBy": "exact-match",
        }))));
    }

    /// KEEP-RUST (W7-21): the lane reads a changed file's nodes back once
    /// its framework nodes are re-extracted. A file stored again as it was,
    /// its `rn-export` node among them though its own extraction makes
    /// none, rivals nothing, so the reference the bridge answered by that
    /// export's JavaScript name is not retried; a node of that name another
    /// changed file comes to hold, in a language the reference is not
    /// written in, retries it at its site.
    #[test]
    fn a_file_stored_again_as_it_was_retries_no_bridged_reference() {
        let dir = std::env::temp_dir().join(format!(
            "cg-rn-bridges-lane-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let mut store = Store::open(&dir.join("index.db")).expect("open");
        let at = |kind: NodeKind, id: &str, name: &str, file: &str, language: Language| Node {
            id: id.to_string(),
            file_path: file.to_string(),
            ..declaration(kind, name, language)
        };
        let deck_text = "#import <React/RCTBridgeModule.h>\n\n@implementation Deck\n\nRCT_EXPORT_MODULE(Deck)\n\nRCT_REMAP_METHOD(rewind, rewindInferenceNow:(BOOL)flag)\n{\n}\n\n@end\n";
        let deck = at(
            NodeKind::Class,
            "class:deck",
            "Deck",
            "ios/Deck.m",
            Language::ObjC,
        );
        let export = at(
            NodeKind::Method,
            "rn-export:ios/Deck.m:Deck.rewind",
            "rewind",
            "ios/Deck.m",
            Language::ObjC,
        );
        store
            .upsert_nodes(&[
                at(
                    NodeKind::Function,
                    "function:halt",
                    "halt",
                    "src/halt.ts",
                    Language::TypeScript,
                ),
                at(
                    NodeKind::Method,
                    "method:rewindInferenceNow:",
                    "rewindInferenceNow:",
                    "ios/Engine.m",
                    Language::ObjC,
                ),
                deck.clone(),
                export.clone(),
            ])
            .expect("nodes");
        store
            .insert_edges(&[Edge {
                id: None,
                source: "function:halt".to_string(),
                target: "method:rewindInferenceNow:".to_string(),
                kind: codegraph_core::types::EdgeKind::Calls,
                metadata: Some(serde_json::json!({
                    "framework": "react-native-bridge",
                    "confidence": 0.6,
                    "resolvedBy": "framework",
                })),
                line: Some(2),
                col: Some(2),
                provenance: None,
            }])
            .expect("edge");
        store
            .insert_unresolved_refs(&[codegraph_core::types::UnresolvedRef {
                id: None,
                from_node_id: "function:halt".to_string(),
                reference_name: "player.rewind".to_string(),
                reference_kind: codegraph_core::types::EdgeKind::Calls,
                line: 2,
                col: 2,
                candidates: None,
                file_path: "src/halt.ts".to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            }])
            .expect("ref");
        let ids: Vec<i64> = store
            .unresolved_refs_by_files(&["src/halt.ts".to_string()])
            .expect("refs")
            .into_iter()
            .filter_map(|reference| reference.id)
            .collect();
        store
            .delete_resolved_unresolved_refs(&ids)
            .expect("resolve");
        let deck_row =
            row_of_text("ios/Deck.m", deck_text, std::slice::from_ref(&deck)).expect("row");
        store
            .set_project_metadata(&row_key("ios/Deck.m"), &deck_row.encode())
            .expect("row");
        let sites = |store: &Store, changes: &BridgeChanges, reindexed: &[&str]| -> Vec<String> {
            let reindexed: HashSet<String> = reindexed.iter().map(|p| p.to_string()).collect();
            bridge_refresh(
                store,
                &dir,
                1 << 20,
                changes,
                &reindexed,
                &BTreeSet::new(),
                false,
                true,
                true,
            )
            .expect("refresh")
            .sites
            .into_iter()
            .map(|site| site.file_path)
            .collect()
        };
        // Deck.m changed and was stored again as it was: its extraction
        // makes the class, the framework pass the export's node again.
        let mut changes = BridgeChanges {
            track: true,
            ..BridgeChanges::default()
        };
        let stored = store.nodes_by_file_path("ios/Deck.m").expect("nodes");
        let source = SourceText::Text(deck_text.to_string());
        changes
            .note(
                &store,
                "ios/Deck.m",
                &stored,
                Some((&source, std::slice::from_ref(&deck))),
            )
            .expect("note");
        assert!(
            sites(&store, &changes, &["ios/Deck.m"]).is_empty(),
            "a file stored as it was rivals nothing"
        );
        // An ArkTS method of the call's last segment comes in another file.
        let player = at(
            NodeKind::Method,
            "method:rewind",
            "rewind",
            "src/app/Player.ets",
            Language::ArkTs,
        );
        store
            .upsert_nodes(std::slice::from_ref(&player))
            .expect("node");
        let player_text = SourceText::Text("export class Player {}\n".to_string());
        changes
            .note(
                &store,
                "src/app/Player.ets",
                &[],
                Some((&player_text, std::slice::from_ref(&player))),
            )
            .expect("note");
        assert_eq!(
            sites(&store, &changes, &["ios/Deck.m", "src/app/Player.ets"]),
            ["src/halt.ts"]
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_multiset_change_counts_repeats() {
        assert_eq!(
            multiset_changes(&[1, 1, 2], &[1, 2, 3]),
            BTreeSet::from([1, 3])
        );
        assert!(multiset_changes(&[1, 2], &[1, 2]).is_empty());
    }
}
