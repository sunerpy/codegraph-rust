//! The Play lane of a sync and its crash marker (W7-19c).
//!
//! A Play routes file's `C.m` reference resolves to the first method or
//! function named `m` in the file of a class named `C`, of the classes so
//! named the one nearest the routes file, and never across code families
//! (`codegraph_resolve::frameworks::play`). What that choice reads of a file
//! is its class names and its methods' and functions' names and languages,
//! with the file's path: a sync compares each changed file's
//! [`ActionSurface`] before and after the change and retries the `C.m`
//! names the change moves, the unresolved ones by their whole names, the
//! resolved ones by site. A file that moves is one removed and one added, so
//! each side counts whole.
//!
//! Those surfaces live in one sync's memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved, and an index without it re-resolves every Play
//! routes file once. The routes files are YAML files no other lane's
//! bootstrap picks, and Play reads no other file's references.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::{Node, NodeKind};
use codegraph_resolve::frameworks::play::is_action_segment;
use codegraph_store::Store;

/// Metadata key present when no sync left a Play action unretried.
pub const VERSION_KEY: &str = "play_routes_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every Play routes file.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no Play action is pending: a full index, a migration, or a
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

/// Every indexed Play routes file, but the ones this sync re-indexed or
/// removed.
pub(crate) fn every_routes_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .all_files()?
        .into_iter()
        .filter(|file| {
            codegraph_extract::is_play_routes_file(&file.path)
                && !reindexed.contains(&file.path)
                && !removed.contains(&file.path)
        })
        .map(|file| file.path)
        .collect())
}

/// What Play's choice reads of one file: its class names, and its methods'
/// and functions' names with the languages they are in, the names a `C.m`
/// reference can spell.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ActionSurface {
    classes: BTreeSet<String>,
    actions: BTreeMap<String, BTreeSet<&'static str>>,
}

impl ActionSurface {
    /// The surface of a file whose nodes are `nodes`.
    pub(crate) fn of(nodes: &[Node]) -> Self {
        let mut surface = Self::default();
        for node in nodes {
            if !is_action_segment(&node.name) {
                continue;
            }
            match node.kind {
                NodeKind::Class => {
                    surface.classes.insert(node.name.clone());
                }
                NodeKind::Method | NodeKind::Function => {
                    surface
                        .actions
                        .entry(node.name.clone())
                        .or_default()
                        .insert(node.language.as_str());
                }
                _ => {}
            }
        }
        surface
    }

    /// Add to `out` the `C.m` names whose candidates differ between this
    /// surface and `after`: the (class, action, language) triples on one
    /// side only.
    pub(crate) fn moved_names(&self, after: &Self, out: &mut BTreeSet<String>) {
        if self == after {
            return;
        }
        self.one_sided(after, out);
        after.one_sided(self, out);
    }

    /// The names of the triples of this surface that `other` lacks.
    fn one_sided(&self, other: &Self, out: &mut BTreeSet<String>) {
        if self.classes.is_empty() || self.actions.is_empty() {
            return;
        }
        // The actions this side has in a language the other lacks.
        let own: Vec<&String> = self
            .actions
            .iter()
            .filter(|(action, languages)| {
                other
                    .actions
                    .get(*action)
                    .is_none_or(|theirs| !languages.is_subset(theirs))
            })
            .map(|(action, _)| action)
            .collect();
        for class in &self.classes {
            if other.classes.contains(class) {
                out.extend(own.iter().map(|action| format!("{class}.{action}")));
            } else {
                out.extend(
                    self.actions
                        .keys()
                        .map(|action| format!("{class}.{action}")),
                );
            }
        }
    }
}

/// What a sync's changed files held before it, for the Play lane: tracked
/// only when the index recorded Play detected, as only then can the lane run
/// without the sync re-extracting every file for a framework change.
#[derive(Debug, Default)]
pub(crate) struct PlayChanges {
    /// Whether this sync records the surfaces.
    pub(crate) track: bool,
    /// Each changed file's surface before this sync's first change to it.
    before: BTreeMap<String, ActionSurface>,
}

impl PlayChanges {
    /// Record `file`'s surface before this sync changes it, its stored
    /// nodes being `before`.
    pub(crate) fn note(&mut self, file: &str, before: &[Node]) {
        if self.track && !self.before.contains_key(file) {
            self.before
                .insert(file.to_string(), ActionSurface::of(before));
        }
    }

    /// The `C.m` names this sync's changes moved, each changed file's
    /// surface read back from the store, framework nodes included.
    pub(crate) fn moved_names(&self, store: &Store) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for (file, before) in &self.before {
            let after = ActionSurface::of(&store.nodes_by_file_path(file)?);
            before.moved_names(&after, &mut out);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::Language;

    fn node(kind: NodeKind, name: &str, language: Language) -> Node {
        Node {
            id: format!("{}:{name}:{}", kind.as_str(), language.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "app/C.scala".to_string(),
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

    fn moved(before: &[Node], after: &[Node]) -> Vec<String> {
        let mut out = BTreeSet::new();
        ActionSurface::of(before).moved_names(&ActionSurface::of(after), &mut out);
        out.into_iter().collect()
    }

    #[test]
    fn a_change_moves_the_names_of_its_one_sided_triples() {
        let class = |name| node(NodeKind::Class, name, Language::Scala);
        let method = |name, language| node(NodeKind::Method, name, language);
        let base = [
            class("C"),
            method("m", Language::Scala),
            method("n", Language::Scala),
        ];
        // Nothing but positions changed: nothing moves.
        assert!(moved(&base, &base).is_empty());
        // A class only: its pairs with every action.
        let mut more = base.to_vec();
        more.push(class("D"));
        assert_eq!(moved(&base, &more), ["D.m", "D.n"]);
        // A method only: its pairs with every class.
        let mut fewer = base.to_vec();
        fewer.pop();
        assert_eq!(moved(&base, &fewer), ["C.n"]);
        // The same names in another language (a remapped extension).
        let java = [
            class("C"),
            method("m", Language::Java),
            method("n", Language::Scala),
        ];
        assert_eq!(moved(&base, &java), ["C.m"]);
        // A function counts; a field and a name no `C.m` can spell do not.
        let mut other = base.to_vec();
        other.push(node(NodeKind::Function, "f", Language::Scala));
        other.push(node(NodeKind::Field, "g", Language::Scala));
        other.push(node(NodeKind::Method, "a-b", Language::Scala));
        other.push(node(NodeKind::Class, "Ünï", Language::Scala));
        assert_eq!(moved(&base, &other), ["C.f"]);
        // A file without actions, or without classes, holds no triple.
        assert!(moved(&[class("C")], &[class("C"), class("D")]).is_empty());
        assert!(moved(&[], &[method("m", Language::Scala)]).is_empty());
        // A removed file moves everything it had.
        assert_eq!(moved(&base, &[]), ["C.m", "C.n"]);
    }
}
