//! The Laravel lane of a sync and its crash marker (W7-19d).
//!
//! A Laravel route's `Controller@action` reference resolves to the method
//! of `app/Http/Controllers/{Controller}.php`, else of the one controller
//! class named the controller path's last segment, and a resource route's
//! controller import to that class (`codegraph_resolve::frameworks::laravel`).
//! Those references wait under their whole names, which no new symbol's
//! name matches, and their result reads other files: the PHP class names of
//! the files whose path holds `Controllers` and those files' method names. A
//! `Model::method` reads `app/Models/{Model}.php` and `app/{Model}.php` the
//! same way. A sync compares each such changed file's [`ControllerSurface`]
//! before and after the change and re-resolves the references that read one
//! that moved: the Laravel route references naming a class of either side,
//! or naming the file as their `app/Http/Controllers/{Controller}.php`, and
//! the `Model::method` references of a moved model file's class; the
//! unresolved ones by their whole names, the resolved ones by site. A file
//! that moves is one removed and one added, so each side counts whole.
//!
//! Those surfaces live in one sync's memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved, and an index without it re-resolves every file
//! holding a Laravel route's reference once.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::{Node, NodeKind};
use codegraph_resolve::frameworks::laravel::{
    controller_class_name, controller_file, controller_paths, model_class, model_file_class,
};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, LaneReference, LaneReferences};

/// Metadata key present when no sync left a Laravel controller unretried.
pub const VERSION_KEY: &str = "laravel_routes_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every Laravel route's references.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no Laravel controller is pending: a full index, a migration,
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

/// Every file holding a Laravel route's reference, but the ones this sync
/// re-indexed or removed.
pub(crate) fn every_route_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .lane_references(&LaneReferences::PhpRoutes)?
        .into_iter()
        .map(|reference| reference.site.file_path)
        .filter(|file| !reindexed.contains(file) && !removed.contains(file))
        .collect())
}

/// Whether a file's surface can move a Laravel choice: a path holding
/// `Controllers` (Patterns 4 and 5) or a model file (Pattern 1).
fn is_tracked(path: &str) -> bool {
    path.contains("Controllers") || model_file_class(path).is_some()
}

/// What Laravel's lookups read of one file: its class names with the
/// languages they are in (the controller lookups take PHP's, the model
/// lookup any), and its method names.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ControllerSurface {
    classes: BTreeMap<String, BTreeSet<&'static str>>,
    methods: BTreeSet<String>,
}

impl ControllerSurface {
    /// The surface of a file whose nodes are `nodes`.
    pub(crate) fn of(nodes: &[Node]) -> Self {
        let mut surface = Self::default();
        for node in nodes {
            match node.kind {
                NodeKind::Class => {
                    surface
                        .classes
                        .entry(node.name.clone())
                        .or_default()
                        .insert(node.language.as_str());
                }
                NodeKind::Method => {
                    surface.methods.insert(node.name.clone());
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
    /// The classes of the moved model files.
    models: BTreeSet<String>,
}

impl MovedControllers {
    /// Add a file's change from `before` to `after`.
    fn add(&mut self, file: &str, before: &ControllerSurface, after: &ControllerSurface) {
        if before == after {
            return;
        }
        self.files.insert(file.to_string());
        self.classes
            .extend(before.classes.keys().chain(after.classes.keys()).cloned());
        if let Some(class) = model_file_class(file) {
            self.models.insert(class.to_string());
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether a Laravel route's `reference` reads what moved.
    fn reads(&self, reference: &LaneReference) -> bool {
        let from = reference
            .site
            .site
            .as_ref()
            .map_or("", |site| site.from_node_id.as_str());
        controller_paths(&reference.reference_name, reference.reference_kind, from)
            .iter()
            .any(|path| {
                self.classes.contains(controller_class_name(path))
                    || self.files.contains(&controller_file(path))
            })
    }

    /// The `Model::method` families to read: one per moved model class.
    pub(crate) fn model_families(&self) -> impl Iterator<Item = LaneReferences> + '_ {
        self.models
            .iter()
            .map(|class| LaneReferences::PhpStaticCalls(class.clone()))
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

impl Retries {
    fn add(&mut self, reference: LaneReference) {
        if reference.resolved {
            self.sites.insert(reference.site);
        } else {
            self.names.insert(reference.reference_name);
        }
    }
}

/// The references of `store` that what moved reads: the Laravel route
/// references, and the `Model::method` references of the moved models'
/// classes.
pub(crate) fn retries(store: &Store, moved: &MovedControllers) -> Result<Retries> {
    let mut out = Retries::default();
    if moved.is_empty() {
        return Ok(out);
    }
    for reference in store.lane_references(&LaneReferences::PhpRoutes)? {
        if moved.reads(&reference) {
            out.add(reference);
        }
    }
    for family in moved.model_families() {
        for reference in store.lane_references(&family)? {
            if model_class(&reference.reference_name).is_some() {
                out.add(reference);
            }
        }
    }
    Ok(out)
}

/// What a sync's changed files held before it, for the Laravel lane:
/// tracked only when the index recorded Laravel detected, as only then can
/// the lane run without the sync re-extracting every file for a framework
/// change.
#[derive(Debug, Default)]
pub(crate) struct LaravelChanges {
    /// Whether this sync records the surfaces.
    pub(crate) track: bool,
    /// Each changed file's surface before this sync's first change to it.
    before: BTreeMap<String, ControllerSurface>,
}

impl LaravelChanges {
    /// Record `file`'s surface before this sync changes it, its stored
    /// nodes being `before`.
    pub(crate) fn note(&mut self, file: &str, before: &[Node]) {
        if self.track && is_tracked(file) && !self.before.contains_key(file) {
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
    use codegraph_core::types::{EdgeKind, Language};
    use codegraph_store::queries::ReferenceSite;

    fn node(kind: NodeKind, name: &str, language: Language) -> Node {
        Node {
            id: format!("{}:{name}:{}", kind.as_str(), language.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "app/Http/Controllers/UserController.php".to_string(),
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

    fn reference(name: &str, kind: EdgeKind, from: &str) -> LaneReference {
        LaneReference {
            site: FileReferenceSite {
                file_path: "routes/web.php".to_string(),
                site: Some(ReferenceSite {
                    from_node_id: from.to_string(),
                    line: 1,
                    col: 0,
                }),
            },
            reference_name: name.to_string(),
            reference_kind: kind,
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
    fn only_controller_and_model_files_are_tracked() {
        assert!(is_tracked("app/Http/Controllers/UserController.php"));
        assert!(is_tracked("modules/Billing/Controllers/Invoices.php"));
        assert!(is_tracked("app/Models/User.php"));
        assert!(is_tracked("app/User.php"));
        assert!(!is_tracked("app/Models/Sub/User.php"));
        assert!(!is_tracked("app/Http/Kernel.php"));
        assert!(!is_tracked("routes/web.php"));
    }

    #[test]
    fn a_surface_moves_when_a_class_or_method_name_comes_or_goes() {
        let file = "app/Http/Controllers/UserController.php";
        let class = |name| node(NodeKind::Class, name, Language::Php);
        let method = |name| node(NodeKind::Method, name, Language::Php);
        let base = [class("UserController"), method("index")];
        // Nothing but positions changed: nothing moves.
        assert!(moved(file, &base, &base).is_empty());
        // A method came: the file and its class moved.
        let more = [class("UserController"), method("index"), method("show")];
        let found = moved(file, &base, &more);
        assert_eq!(
            found.classes.iter().map(String::as_str).collect::<Vec<_>>(),
            ["UserController"]
        );
        assert_eq!(
            found.files.iter().map(String::as_str).collect::<Vec<_>>(),
            [file]
        );
        // A class renamed: both names.
        let renamed = [class("UsersController"), method("index")];
        assert_eq!(
            moved(file, &base, &renamed)
                .classes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["UserController", "UsersController"]
        );
        // The same class in another language moves the PHP lookup.
        let other = [
            node(NodeKind::Class, "UserController", Language::Kotlin),
            method("index"),
        ];
        assert!(!moved(file, &base, &other).is_empty());
        // A function or a field names no controller.
        let mut extra = base.to_vec();
        extra.push(node(NodeKind::Function, "helper", Language::Php));
        extra.push(node(NodeKind::Field, "name", Language::Php));
        assert!(moved(file, &base, &extra).is_empty());
    }

    #[test]
    fn a_route_reference_reads_a_moved_class_or_its_controller_file() {
        let file = "app/Http/Controllers/Admin/UserController.php";
        let class = |name| node(NodeKind::Class, name, Language::Php);
        let method = |name| node(NodeKind::Method, name, Language::Php);
        let found = moved(
            file,
            &[class("UserController")],
            &[class("UserController"), method("index")],
        );
        let verb = "route:routes/web.php:3:GET:/users";
        let resource = "route:routes/web.php:4:RESOURCE:photos";
        // `C@m`, by its class or its namespace path's last segment.
        assert!(found.reads(&reference(
            "UserController@index",
            EdgeKind::References,
            verb
        )));
        assert!(found.reads(&reference(
            "Admin\\UserController@index",
            EdgeKind::References,
            verb
        )));
        assert!(!found.reads(&reference(
            "PhotoController@index",
            EdgeKind::References,
            verb
        )));
        // A resource route's controller import, by its last segment.
        assert!(found.reads(&reference("UserController", EdgeKind::Imports, resource)));
        assert!(found.reads(&reference(
            "Admin\\UserController",
            EdgeKind::Imports,
            resource
        )));
        // A bare class handler of a verb route is no controller path: the
        // name lane wakes it by its class's name.
        assert!(!found.reads(&reference("UserController", EdgeKind::References, verb)));
        assert!(!found.reads(&reference("UserController", EdgeKind::Imports, verb)));

        // A file whose class is named otherwise still reads as its path's
        // `app/Http/Controllers/{path}.php`.
        let misnamed = moved(
            "app/Http/Controllers/Admin/Panel.php",
            &[class("Dashboard")],
            &[class("Dashboard"), method("index")],
        );
        assert!(misnamed.reads(&reference("Admin\\Panel@index", EdgeKind::References, verb)));
        assert!(misnamed.reads(&reference("Dashboard@index", EdgeKind::References, verb)));
        assert!(!misnamed.reads(&reference("Panel@index", EdgeKind::References, verb)));
    }

    #[test]
    fn a_moved_model_file_reads_its_class_static_calls() {
        let class = |name| node(NodeKind::Class, name, Language::Php);
        let method = |name| node(NodeKind::Method, name, Language::Php);
        let found = moved(
            "app/Models/User.php",
            &[class("User")],
            &[class("User"), method("find")],
        );
        assert_eq!(
            found.model_families().collect::<Vec<_>>(),
            [LaneReferences::PhpStaticCalls("User".to_string())]
        );
        let controller = moved(
            "app/Http/Controllers/UserController.php",
            &[class("UserController")],
            &[],
        );
        assert_eq!(controller.model_families().count(), 0);
    }
}
