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
//!   that resolved into it before the change, whole.
//!
//! The unresolved references are retried by their whole names and the
//! resolved ones at their sites, however many there are. The lane compares
//! only when the index recorded a bridge reading the rows; otherwise a
//! bridge this sync detects is a framework change, which re-extracts every
//! file. A sync withdraws [`VERSION_KEY`] before its first change and writes
//! it back once everything resolved; an index without it rebuilds every row
//! and re-resolves every JavaScript-family, Swift and Objective-C file of a
//! project a bridge was detected in.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::frameworks::react_native::{
    BridgeEntry, SwiftMethodFact, is_js_family_path,
};
pub use codegraph_resolve::frameworks::react_native::{BridgeFacts, ROW_KEY_PREFIX, row_key};
use codegraph_resolve::frameworks::swift_objc::swift_base_names_for_objc_selector;
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
        let (old_methods, gains_methods) = if self.track {
            (
                native_methods(old_nodes),
                new.is_some_and(|(_, nodes)| !native_methods(nodes).is_empty()),
            )
        } else {
            (Vec::new(), false)
        };
        if old_row.is_none() && new_row.is_none() && old_methods.is_empty() && !gains_methods {
            return Ok(());
        }
        self.changes.insert(
            path.to_string(),
            Change {
                old_row,
                new_row,
                old_methods,
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
        refresh.rows.insert(path.clone(), change.new_row.clone());
    }
    if !retry || changes.changes.is_empty() {
        return Ok(refresh);
    }
    let mut new_methods: BTreeMap<String, Vec<NativeMethod>> = BTreeMap::new();
    for path in changes.changes.keys() {
        if !removed.contains(path) {
            new_methods.insert(
                path.clone(),
                native_methods(&store.nodes_by_file_path(path)?),
            );
        }
    }
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
    let families: [(Family, Vec<Language>, bool); 3] = [
        (
            Family::Js,
            JS_FAMILY.to_vec(),
            !seeds.js_tails.is_empty() || !seeds.js_receivers.is_empty(),
        ),
        (
            Family::Swift,
            vec![Language::Swift],
            !seeds.swift_tails.is_empty(),
        ),
        (
            Family::ObjC,
            vec![Language::ObjC],
            !seeds.objc_base_names.is_empty(),
        ),
    ];
    for (family, languages, wanted) in families {
        if !wanted {
            continue;
        }
        for reference in store.lane_references(&LaneReferences::Languages(languages))? {
            if !unchanged(&reference.site.file_path) || !moved(&seeds, family, &reference) {
                continue;
            }
            if reference.resolved {
                refresh.sites.insert(reference.site);
            } else {
                refresh.names.insert(reference.reference_name);
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

    #[test]
    fn a_multiset_change_counts_repeats() {
        assert_eq!(
            multiset_changes(&[1, 1, 2], &[1, 2, 3]),
            BTreeSet::from([1, 3])
        );
        assert!(multiset_changes(&[1, 2], &[1, 2]).is_empty());
    }
}
