//! The Vapor lane of a sync and its crash marker (W7-19g).
//!
//! A Vapor route's handler reference (`TodoController@index`, `@index`)
//! resolves to a Swift method or function named for its method, on the type
//! the route names (`codegraph_resolve::frameworks::swift`). Those
//! references wait under their whole names, which no new symbol's name
//! matches, and their result reads other files: the qualified names, kinds,
//! files and lines of every Swift callable of the method's name, the types
//! around them, and the heads of those files (a `Request` parameter, an
//! `extension A.B` line), none of which a name moves. Whatever such a
//! result reads is in a file declaring a Swift callable of the method's
//! name before or after a change, or in the reference's own file, which a
//! change re-extracts. A sync records each changed file's Swift method and
//! function names before its change and reads them back after, and
//! re-resolves the route handler references whose method is one of them:
//! the unresolved ones by their whole names, the resolved ones by site,
//! however many.
//!
//! Those names live in one sync's memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved, and an index without it re-resolves every file
//! holding a Swift route's reference once.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::frameworks::swift::handler_method;
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, LaneReference, LaneReferences};

/// Metadata key present when no sync left a Vapor handler unretried.
pub const VERSION_KEY: &str = "vapor_routes_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every Swift route's references.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no Vapor handler is pending: a full index, a migration, or a
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

/// Every file holding a Swift route's reference, but the ones this sync
/// re-indexed or removed.
pub(crate) fn every_route_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .lane_references(&LaneReferences::SwiftRoutes)?
        .into_iter()
        .map(|reference| reference.site.file_path)
        .filter(|file| !reindexed.contains(file) && !removed.contains(file))
        .collect())
}

/// The names of the Swift methods and functions among `nodes`: what a
/// handler's lookup reads a file for.
fn callable_names(nodes: &[Node]) -> BTreeSet<String> {
    nodes
        .iter()
        .filter(|node| {
            node.language == Language::Swift
                && matches!(node.kind, NodeKind::Method | NodeKind::Function)
        })
        .map(|node| node.name.clone())
        .collect()
}

/// The callable names a sync's changes touched: a route handler whose
/// method is one of them reads a changed file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TouchedMethods {
    names: BTreeSet<String>,
}

impl TouchedMethods {
    pub(crate) fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Whether `reference` is a route's handler whose method is touched.
    fn reads(&self, reference: &LaneReference) -> bool {
        handler_method(&reference.reference_name).is_some_and(|method| self.names.contains(method))
    }
}

/// The references a change moved, split as a sync retries them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Retries {
    /// The unresolved ones' names, retried whole.
    pub(crate) names: BTreeSet<String>,
    /// The resolved ones' sites.
    pub(crate) sites: BTreeSet<FileReferenceSite>,
}

/// The Swift route handler references of `store` whose method `touched`
/// holds.
pub(crate) fn retries(store: &Store, touched: &TouchedMethods) -> Result<Retries> {
    let mut out = Retries::default();
    if touched.is_empty() {
        return Ok(out);
    }
    for reference in store.lane_references(&LaneReferences::SwiftRoutes)? {
        if !touched.reads(&reference) {
            continue;
        }
        if reference.resolved {
            out.sites.insert(reference.site);
        } else {
            out.names.insert(reference.reference_name);
        }
    }
    Ok(out)
}

/// What a sync's changed files held before it, for the Vapor lane: tracked
/// only when the index recorded Vapor detected, as only then can the lane
/// run without the sync re-extracting every file for a framework change.
#[derive(Debug, Default)]
pub(crate) struct VaporChanges {
    /// Whether this sync records the names.
    pub(crate) track: bool,
    /// Each changed file's callable names before this sync's first change
    /// to it.
    before: BTreeMap<String, BTreeSet<String>>,
}

impl VaporChanges {
    /// Record `file`'s callable names before this sync changes it, its
    /// stored nodes being `before`.
    pub(crate) fn note(&mut self, file: &str, before: &[Node]) {
        if self.track && !self.before.contains_key(file) {
            self.before.insert(file.to_string(), callable_names(before));
        }
    }

    /// The callable names this sync's changed files declare on either
    /// side, each read back from the store after the change. A file's names
    /// count whatever moved: a handler's choice reads its heads, which no
    /// name shows.
    pub(crate) fn touched(&self, store: &Store) -> Result<TouchedMethods> {
        let mut out = TouchedMethods::default();
        for (file, before) in &self.before {
            out.names.extend(before.iter().cloned());
            out.names
                .extend(callable_names(&store.nodes_by_file_path(file)?));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::EdgeKind;
    use codegraph_store::queries::ReferenceSite;

    fn node(kind: NodeKind, name: &str, language: Language) -> Node {
        Node {
            id: format!("{}:{name}:{}", kind.as_str(), language.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "Sources/App/Controllers/TodoController.swift".to_string(),
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

    fn reference(name: &str) -> LaneReference {
        LaneReference {
            site: FileReferenceSite {
                file_path: "Sources/App/routes.swift".to_string(),
                site: Some(ReferenceSite {
                    from_node_id: "route:Sources/App/routes.swift:3:GET:/todos".to_string(),
                    line: 3,
                    col: 0,
                }),
            },
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            resolved: false,
        }
    }

    fn touched(names: &[&str]) -> TouchedMethods {
        TouchedMethods {
            names: names.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    #[test]
    fn a_files_swift_methods_and_functions_are_its_callable_names() {
        let names = callable_names(&[
            node(NodeKind::Method, "index", Language::Swift),
            node(NodeKind::Function, "hello", Language::Swift),
            node(NodeKind::Class, "TodoController", Language::Swift),
            node(NodeKind::Struct, "Todo", Language::Swift),
            node(NodeKind::Method, "show", Language::ObjC),
            node(NodeKind::Function, "create", Language::Ruby),
        ]);
        assert_eq!(
            names.iter().map(String::as_str).collect::<Vec<_>>(),
            ["hello", "index"]
        );
    }

    #[test]
    fn a_handler_reads_the_touched_method_it_names() {
        let found = touched(&["index", "hello"]);
        assert!(found.reads(&reference("TodoController@index")));
        assert!(found.reads(&reference("API.TodoController@index")));
        assert!(found.reads(&reference("@hello")));
        assert!(!found.reads(&reference("@show")));
        // A closure's call or a convention's name is no handler: the name
        // machinery wakes those.
        assert!(!found.reads(&reference("index")));
        assert!(!found.reads(&reference("Greeter.hello")));
        assert!(touched(&[]).is_empty());
    }

    #[test]
    fn an_untracked_sync_notes_nothing_and_a_file_keeps_its_first_side() {
        let mut changes = VaporChanges::default();
        changes.note(
            "Sources/App/A.swift",
            &[node(NodeKind::Method, "index", Language::Swift)],
        );
        assert!(changes.before.is_empty());
        changes.track = true;
        changes.note(
            "Sources/App/A.swift",
            &[node(NodeKind::Method, "index", Language::Swift)],
        );
        changes.note(
            "Sources/App/A.swift",
            &[node(NodeKind::Method, "show", Language::Swift)],
        );
        assert_eq!(
            changes.before["Sources/App/A.swift"]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["index"]
        );
    }
}
