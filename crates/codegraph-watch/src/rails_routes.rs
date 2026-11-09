//! The Rails lane of a sync and its crash marker (W7-19e).
//!
//! A Rails route's `controller#action` reference resolves to the action in
//! `app/controllers/{controller}_controller.rb`, else in the file of the one
//! Ruby class named for the controller path's last segment that holds it
//! (`codegraph_resolve::frameworks::rails`). Those references wait under
//! their whole names, which no new symbol's name matches, and their result
//! reads other files: the Ruby class names of every file, their paths, and
//! those files' method and function names. A sync compares each changed
//! file's [`ControllerSurface`] before and after the change and re-resolves
//! the route references that read one that moved: those naming a class of
//! either side, or naming the file as their
//! `app/controllers/{controller}_controller.rb`; the unresolved ones by
//! their whole names, the resolved ones by site. A file that moves is one
//! removed and one added, so each side counts whole.
//!
//! Those surfaces live in one sync's memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved, and an index without it re-resolves every file
//! holding a Rails route's reference once.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::frameworks::rails::{controller_class_name, controller_file, route_target};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, LaneReference, LaneReferences};

/// Metadata key present when no sync left a Rails controller unretried.
pub const VERSION_KEY: &str = "rails_routes_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every Rails route's references.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no Rails controller is pending: a full index, a migration,
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

/// Every file holding a Rails route's reference, but the ones this sync
/// re-indexed or removed.
pub(crate) fn every_route_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .lane_references(&LaneReferences::RubyRoutes)?
        .into_iter()
        .map(|reference| reference.site.file_path)
        .filter(|file| !reindexed.contains(file) && !removed.contains(file))
        .collect())
}

/// What a route's lookup reads of one file: its Ruby class names, and its
/// Ruby method and function names.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ControllerSurface {
    classes: BTreeSet<String>,
    actions: BTreeSet<String>,
}

impl ControllerSurface {
    /// The surface of a file whose nodes are `nodes`.
    pub(crate) fn of(nodes: &[Node]) -> Self {
        let mut surface = Self::default();
        for node in nodes.iter().filter(|node| node.language == Language::Ruby) {
            match node.kind {
                NodeKind::Class => {
                    surface.classes.insert(node.name.clone());
                }
                NodeKind::Method | NodeKind::Function => {
                    surface.actions.insert(node.name.clone());
                }
                _ => {}
            }
        }
        surface
    }
}

/// What a sync's changes moved: the files whose surfaces differ, and their
/// classes on either side.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MovedControllers {
    files: BTreeSet<String>,
    classes: BTreeSet<String>,
}

impl MovedControllers {
    /// Add a file's change from `before` to `after`.
    fn add(&mut self, file: &str, before: &ControllerSurface, after: &ControllerSurface) {
        if before == after {
            return;
        }
        self.files.insert(file.to_string());
        self.classes
            .extend(before.classes.iter().chain(&after.classes).cloned());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether a Rails route's `reference` reads what moved: its controller
    /// class is one of the moved classes, or its conventional file one of
    /// the moved files.
    fn reads(&self, reference: &LaneReference) -> bool {
        route_target(&reference.reference_name).is_some_and(|(controller, _)| {
            self.classes.contains(&controller_class_name(controller))
                || self.files.contains(&controller_file(controller))
        })
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

/// The Rails route references of `store` that what moved reads.
pub(crate) fn retries(store: &Store, moved: &MovedControllers) -> Result<Retries> {
    let mut out = Retries::default();
    if moved.is_empty() {
        return Ok(out);
    }
    for reference in store.lane_references(&LaneReferences::RubyRoutes)? {
        if !moved.reads(&reference) {
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

/// What a sync's changed files held before it, for the Rails lane: tracked
/// only when the index recorded Rails detected, as only then can the lane
/// run without the sync re-extracting every file for a framework change.
#[derive(Debug, Default)]
pub(crate) struct RailsChanges {
    /// Whether this sync records the surfaces.
    pub(crate) track: bool,
    /// Each changed file's surface before this sync's first change to it.
    before: BTreeMap<String, ControllerSurface>,
}

impl RailsChanges {
    /// Record `file`'s surface before this sync changes it, its stored
    /// nodes being `before`.
    pub(crate) fn note(&mut self, file: &str, before: &[Node]) {
        if self.track && !self.before.contains_key(file) {
            self.before
                .insert(file.to_string(), ControllerSurface::of(before));
        }
    }

    /// What this sync's changes moved, each changed file's surface read
    /// back from the store.
    pub(crate) fn moved(&self, store: &Store) -> Result<MovedControllers> {
        let mut out = MovedControllers::default();
        for (file, before) in &self.before {
            let after = ControllerSurface::of(&store.nodes_by_file_path(file)?);
            out.add(file, before, &after);
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
            file_path: "app/controllers/users_controller.rb".to_string(),
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
                file_path: "config/routes.rb".to_string(),
                site: Some(ReferenceSite {
                    from_node_id: "route:config/routes.rb:2:GET:/users:users#index".to_string(),
                    line: 2,
                    col: 0,
                }),
            },
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            resolved: false,
        }
    }

    fn moved(file: &str, before: &[Node], after: &[Node]) -> MovedControllers {
        let mut out = MovedControllers::default();
        out.add(
            file,
            &ControllerSurface::of(before),
            &ControllerSurface::of(after),
        );
        out
    }

    #[test]
    fn a_surface_moves_when_a_ruby_class_method_or_function_name_comes_or_goes() {
        let file = "app/controllers/users_controller.rb";
        let class = |name| node(NodeKind::Class, name, Language::Ruby);
        let method = |name| node(NodeKind::Method, name, Language::Ruby);
        let base = [class("UsersController"), method("index")];
        // Nothing but positions changed: nothing moves.
        assert!(moved(file, &base, &base).is_empty());
        // A method came: the file and its class moved.
        let more = [class("UsersController"), method("index"), method("show")];
        let found = moved(file, &base, &more);
        assert_eq!(
            found.classes.iter().map(String::as_str).collect::<Vec<_>>(),
            ["UsersController"]
        );
        assert_eq!(
            found.files.iter().map(String::as_str).collect::<Vec<_>>(),
            [file]
        );
        // A function counts as an action.
        let mut function = base.to_vec();
        function.push(node(NodeKind::Function, "helper", Language::Ruby));
        assert!(!moved(file, &base, &function).is_empty());
        // A class renamed: both names.
        let renamed = [class("MembersController"), method("index")];
        assert_eq!(
            moved(file, &base, &renamed)
                .classes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["MembersController", "UsersController"]
        );
        // Another language's nodes, a module or a constant name no
        // controller, and no lookup reads them.
        let mut other = base.to_vec();
        other.push(node(NodeKind::Class, "Zones", Language::TypeScript));
        other.push(node(NodeKind::Method, "edit", Language::Php));
        other.push(node(NodeKind::Module, "Admin", Language::Ruby));
        other.push(node(NodeKind::Constant, "LIMIT", Language::Ruby));
        assert!(moved(file, &base, &other).is_empty());
    }

    #[test]
    fn a_route_reference_reads_a_moved_class_or_its_conventional_file() {
        let class = |name| node(NodeKind::Class, name, Language::Ruby);
        let method = |name| node(NodeKind::Method, name, Language::Ruby);
        let found = moved(
            "backend/app/controllers/spree/admin/zones_controller.rb",
            &[class("ZonesController")],
            &[class("ZonesController"), method("index")],
        );
        // By the class its controller path's last segment names.
        assert!(found.reads(&reference("admin/zones#index")));
        assert!(found.reads(&reference("zones#show")));
        assert!(!found.reads(&reference("users#index")));
        // A name that is no route target reads nothing.
        assert!(!found.reads(&reference("ZonesController")));
        assert!(!found.reads(&reference("admin/zones#in-dex")));

        // A file whose class is named otherwise still reads as its
        // controller path's conventional file.
        let misnamed = moved(
            "app/controllers/admin/panel_controller.rb",
            &[class("DashboardController")],
            &[class("DashboardController"), method("index")],
        );
        assert!(misnamed.reads(&reference("admin/panel#index")));
        assert!(!misnamed.reads(&reference("panel#index")));
        assert!(misnamed.reads(&reference("dashboard#index")));
        // A file that loses its last Ruby node moves too.
        assert!(!moved("app/models/user.rb", &[class("User")], &[]).is_empty());
    }
}
