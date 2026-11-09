//! The Drupal lane of a sync and its crash marker (W7-19d).
//!
//! A Drupal routing file's handler resolves to the one PHP class its name
//! names, or, of several, to the only one whose file holds the method, and a
//! `hook_X` reference to the one PHP function of a hook file whose name ends
//! with `_X` (`codegraph_resolve::frameworks::drupal`). A `Class::method`
//! handler waits under its method's name, but its result reads how many
//! classes are so named and their files' methods; a bare class and a
//! `hook_X` wait under their whole names, which no new symbol's name
//! matches. A sync compares each changed file's [`HandlerSurface`] before and
//! after the change and re-resolves the references that read one that
//! moved: the routing files' references naming a class of a file whose class
//! or method names moved, and the `hook_X` references a hook function's name
//! that came or went answers; the unresolved ones by their whole names, the
//! resolved ones by site. A `hook_X` definition (`function hook_X`, in an
//! `*.api.php`) outranks the implementations when it comes, and name matching
//! reopens only the edges into a node so named: a resolved reference named
//! like a node that came or went is reopened by site too.
//!
//! Those surfaces live in one sync's memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved, and an index without it re-resolves once every file
//! holding a routing file's reference or a `hook_X` reference.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::frameworks::drupal::{handler_class, hooks_reading, is_hook_function};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, LaneReferences};

/// Metadata key present when no sync left a Drupal reference unretried.
pub const VERSION_KEY: &str = "drupal_refs_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every Drupal reference.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no Drupal reference is pending: a full index, a migration, or
/// a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// Every file holding a routing file's reference or a `hook_X` reference,
/// but the ones this sync re-indexed or removed.
pub(crate) fn every_reference_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for family in [LaneReferences::DrupalRoutingFiles, LaneReferences::PhpHooks] {
        out.extend(
            store
                .lane_references(&family)?
                .into_iter()
                .map(|reference| reference.site.file_path)
                .filter(|file| !reindexed.contains(file) && !removed.contains(file)),
        );
    }
    Ok(out)
}

/// What Drupal's lookups read of one file: its PHP class names and its
/// method names, which a handler reads, and its hook functions by name with
/// how many there are, which a `hook_X` reads.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct HandlerSurface {
    classes: BTreeSet<String>,
    methods: BTreeSet<String>,
    hooks: BTreeMap<String, usize>,
}

impl HandlerSurface {
    /// The surface of a file whose nodes are `nodes`.
    pub(crate) fn of(nodes: &[Node]) -> Self {
        let mut surface = Self::default();
        for node in nodes {
            match node.kind {
                NodeKind::Class if node.language == Language::Php => {
                    surface.classes.insert(node.name.clone());
                }
                NodeKind::Method => {
                    surface.methods.insert(node.name.clone());
                }
                _ if is_hook_function(node) => {
                    *surface.hooks.entry(node.name.clone()).or_default() += 1;
                }
                _ => {}
            }
        }
        surface
    }
}

/// What a sync's changes moved.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MovedHandlers {
    /// The classes, on either side, of the files whose class or method names
    /// moved.
    classes: BTreeSet<String>,
    /// The `hook_X` names a hook function that came or went answers.
    hooks: BTreeSet<String>,
}

impl MovedHandlers {
    /// Add a file's change from `before` to `after`.
    fn add(&mut self, before: &HandlerSurface, after: &HandlerSurface) {
        if before.classes != after.classes || before.methods != after.methods {
            self.classes
                .extend(before.classes.iter().chain(&after.classes).cloned());
        }
        let functions: BTreeSet<&String> = before.hooks.keys().chain(after.hooks.keys()).collect();
        for function in functions {
            if before.hooks.get(function) != after.hooks.get(function) {
                self.hooks.extend(hooks_reading(function));
            }
        }
    }
}

/// The references a change moved, split as a sync retries them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Retries {
    /// The names retried whole: the unresolved handlers', and every `hook_X`
    /// a hook function that came or went answers.
    pub(crate) names: BTreeSet<String>,
    /// The resolved references' sites.
    pub(crate) sites: BTreeSet<FileReferenceSite>,
}

/// The references of `store` that what moved reads: the routing files'
/// references naming a moved class, the `hook_X` references a moved hook
/// function answers, and the resolved references named one of `touched`
/// starting `hook_`, the names whose candidates this sync moved.
pub(crate) fn retries<'a>(
    store: &Store,
    moved: &MovedHandlers,
    touched: impl IntoIterator<Item = &'a String>,
) -> Result<Retries> {
    let mut out = Retries::default();
    if !moved.classes.is_empty() {
        for reference in store.lane_references(&LaneReferences::DrupalRoutingFiles)? {
            if !handler_class(&reference.reference_name)
                .is_some_and(|class| moved.classes.contains(class))
            {
                continue;
            }
            if reference.resolved {
                out.sites.insert(reference.site);
            } else {
                out.names.insert(reference.reference_name);
            }
        }
    }
    out.names.extend(moved.hooks.iter().cloned());
    let mut hooks: BTreeSet<String> = moved.hooks.clone();
    hooks.extend(
        touched
            .into_iter()
            .filter(|name| name.starts_with("hook_"))
            .cloned(),
    );
    if !hooks.is_empty() {
        let hooks: Vec<String> = hooks.into_iter().collect();
        out.sites
            .extend(store.resolved_reference_sites_named(&hooks)?);
    }
    Ok(out)
}

/// What a sync's changed files held before it, for the Drupal lane: tracked
/// only when the index recorded Drupal detected, as only then can the lane
/// run without the sync re-extracting every file for a framework change.
#[derive(Debug, Default)]
pub(crate) struct DrupalChanges {
    /// Whether this sync records the surfaces.
    pub(crate) track: bool,
    /// Each changed file's surface before this sync's first change to it.
    before: BTreeMap<String, HandlerSurface>,
}

impl DrupalChanges {
    /// Record `file`'s surface before this sync changes it, its stored
    /// nodes being `before`.
    pub(crate) fn note(&mut self, file: &str, before: &[Node]) {
        if self.track && !self.before.contains_key(file) {
            self.before
                .insert(file.to_string(), HandlerSurface::of(before));
        }
    }

    /// What this sync's changes moved, each changed file's surface read
    /// back from the store.
    pub(crate) fn moved(&self, store: &Store) -> Result<MovedHandlers> {
        let mut out = MovedHandlers::default();
        for (file, before) in &self.before {
            let after = HandlerSurface::of(&store.nodes_by_file_path(file)?);
            out.add(before, &after);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: NodeKind, name: &str, language: Language, file: &str) -> Node {
        Node {
            id: format!("{}:{name}:{}", kind.as_str(), language.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
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

    fn moved(before: &[Node], after: &[Node]) -> MovedHandlers {
        let mut out = MovedHandlers::default();
        out.add(&HandlerSurface::of(before), &HandlerSurface::of(after));
        out
    }

    fn names(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_class_or_method_name_moves_the_file_s_classes() {
        let file = "src/Controller/GreetingController.php";
        let class = |name| node(NodeKind::Class, name, Language::Php, file);
        let method = |name| node(NodeKind::Method, name, Language::Php, file);
        let base = [class("GreetingController"), method("hello")];
        assert_eq!(moved(&base, &base), MovedHandlers::default());
        let more = [class("GreetingController"), method("hello"), method("bye")];
        assert_eq!(names(&moved(&base, &more).classes), ["GreetingController"]);
        let renamed = [class("HelloController"), method("hello")];
        assert_eq!(
            names(&moved(&base, &renamed).classes),
            ["GreetingController", "HelloController"]
        );
        // A class in another language is no PHP class.
        let kotlin = [
            node(
                NodeKind::Class,
                "GreetingController",
                Language::Kotlin,
                file,
            ),
            method("hello"),
        ];
        assert_eq!(
            names(&moved(&base, &kotlin).classes),
            ["GreetingController"]
        );
        // Methods without a class move nothing a handler reads by name.
        assert!(moved(&[method("hello")], &[]).classes.is_empty());
    }

    #[test]
    fn a_hook_function_that_comes_or_goes_moves_every_hook_it_answers() {
        let hook = |name, file| node(NodeKind::Function, name, Language::Php, file);
        let module = "modules/greeting/greeting.module";
        let base = [hook("greeting_help", module)];
        assert!(moved(&base, &base).hooks.is_empty());
        let more = [
            hook("greeting_help", module),
            hook("greeting_form_alter", module),
        ];
        assert_eq!(
            names(&moved(&base, &more).hooks),
            ["hook_alter", "hook_form_alter"]
        );
        // A second function of a name moves its hooks: one candidate became
        // two.
        let twice = [hook("greeting_help", module), hook("greeting_help", module)];
        assert_eq!(names(&moved(&base, &twice).hooks), ["hook_help"]);
        // A function of a `.php` file is no hook implementation, nor is a
        // hook file's function in another language.
        let php = "src/Helper.php";
        assert!(moved(&[], &[hook("greeting_help", php)]).hooks.is_empty());
        let pascal = node(
            NodeKind::Function,
            "greeting_help",
            Language::Pascal,
            "x.inc",
        );
        assert!(moved(&[], &[pascal]).hooks.is_empty());
        // A removed file moves what it had.
        assert_eq!(names(&moved(&base, &[]).hooks), ["hook_help"]);
    }
}
