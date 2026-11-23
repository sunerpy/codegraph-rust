//! The navigation calls a sync sends back when a route they may name
//! appears, goes or moves (W7-23b: upstream #2425's `reopenNavigationsFor`,
//! with this engine's extensions).
//!
//! A navigation call names its route by path (`navigate('/login')`) and its
//! reference by the router's method (`navigate`, `history.push`), so no name
//! a changed file declares wakes a call parked while its route was missing,
//! and a call bound to the best route then (a catch-all, a parameter route,
//! one arm of a conditional) keeps it while a better one appears. Two lanes
//! read them once the sync's files and framework nodes are written:
//!
//! - N1, the routes. The route nodes before the sync's first change
//!   ([`route_snapshot`], read when the sync first mutates the index) are
//!   compared with those after ([`changed_routes`]). Each router whose route
//!   changed scopes the calls the route may answer to the route's app. A
//!   router whose table reads its route files' text (Vue Router's names,
//!   Angular's redirects) counts each route of a file the sync re-extracted
//!   or removed as changed, and Angular adds the app of a rewritten file
//!   that only lazy-loads routes files. The parked calls with the planned
//!   routers' method tails and the `navigates` edges their resolvers drew,
//!   in those apps and claimed by the router, go back.
//! - N2, the destinations read elsewhere. Angular and React Router read a
//!   call's destination through constants other files hold, so their calls
//!   in the files whose references into a rewritten file the sync refreshes
//!   go back too.
//!
//! Each call goes back at its site; a call in a file the sync re-extracted
//! was resolved anew. Neither lane has a ceiling (OD-B42): upstream skips a
//! name more than 500 calls share.

use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_resolve::ReferenceResolver;
use codegraph_resolve::navigation::{NavigationPlan, changed_routes};
use codegraph_store::Store;
use codegraph_store::queries::FileReferenceSite;

/// The languages whose route nodes the snapshots read: those of the
/// routers that bind navigation calls, which a sync's first mutation reads
/// before it detects any router.
const ROUTE_LANGUAGES: [Language; 6] = [
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
    Language::Vue,
    Language::Svelte,
];

/// The route nodes a navigation router may bind a call to, as the store
/// holds them now.
pub(crate) fn route_snapshot(store: &Store) -> Result<Vec<Node>> {
    Ok(store.nodes_of_kinds_in_languages(&[NodeKind::Route], &ROUTE_LANGUAGES)?)
}

/// What the navigation lanes read of a sync's changes.
pub(crate) struct NavigationChanges<'a> {
    /// The route nodes before the sync's first change; `None` when it
    /// changed nothing.
    pub(crate) routes_before: Option<&'a [Node]>,
    /// The files the sync re-extracted or removed.
    pub(crate) rewritten: &'a BTreeSet<String>,
    /// The files whose references into a rewritten file the sync refreshes
    /// at their sites.
    pub(crate) dependents: &'a BTreeSet<String>,
}

/// The sites of the navigation calls the sync's changes may move (lanes N1
/// and N2). A site in `reindexed` is left out: its file was resolved anew.
pub(crate) fn navigation_sites(
    store: &Store,
    resolver: &ReferenceResolver,
    changes: &NavigationChanges<'_>,
    reindexed: &HashSet<String>,
) -> Result<BTreeSet<FileReferenceSite>> {
    let mut sites = BTreeSet::new();
    if !resolver.has_navigation_routers() {
        return Ok(sites);
    }
    if let Some(before) = changes.routes_before {
        #[cfg(test)]
        crate::sync::tests::note_navigation("routes");
        let after = route_snapshot(store)?;
        let changed = changed_routes(before, &after);
        let rewritten_routes: Vec<Node> = before
            .iter()
            .chain(&after)
            .filter(|route| changes.rewritten.contains(&route.file_path))
            .cloned()
            .collect();
        sites.extend(reopened_by_routes(
            store,
            resolver,
            &changed,
            &rewritten_routes,
            changes.rewritten,
            reindexed,
        )?);
    }
    sites.extend(reopened_by_destinations(
        store,
        resolver,
        changes.dependents,
        reindexed,
    )?);
    Ok(sites)
}

/// Lane N1: the sites of the calls `changed` routes may answer, and, for a
/// router whose table reads its route files, those `rewritten_routes` and
/// the files of `rewritten` its table reads may.
pub(crate) fn reopened_by_routes(
    store: &Store,
    resolver: &ReferenceResolver,
    changed: &[Node],
    rewritten_routes: &[Node],
    rewritten: &BTreeSet<String>,
    reindexed: &HashSet<String>,
) -> Result<BTreeSet<FileReferenceSite>> {
    if changed.is_empty() && rewritten_routes.is_empty() && rewritten.is_empty() {
        return Ok(BTreeSet::new());
    }
    let context = resolver.store_context(store);
    let plan = resolver.navigation_plan(changed, rewritten_routes, rewritten, &context);
    if plan.is_empty() {
        return Ok(BTreeSet::new());
    }
    #[cfg(test)]
    crate::sync::tests::note_navigation("calls");
    calls_of(store, &plan, None, reindexed)
}

/// Lane N2: the sites of the calls in `dependents` a router that reads a
/// destination through other files claims.
fn reopened_by_destinations(
    store: &Store,
    resolver: &ReferenceResolver,
    dependents: &BTreeSet<String>,
    reindexed: &HashSet<String>,
) -> Result<BTreeSet<FileReferenceSite>> {
    let files: Vec<String> = dependents
        .iter()
        .filter(|file| !reindexed.contains(*file))
        .cloned()
        .collect();
    if files.is_empty() {
        return Ok(BTreeSet::new());
    }
    let plan = resolver.destination_plan();
    if plan.is_empty() {
        return Ok(BTreeSet::new());
    }
    #[cfg(test)]
    crate::sync::tests::note_navigation("destinations");
    calls_of(store, &plan, Some(&files), reindexed)
}

/// The sites of the calls `plan` claims, in `files` when given: the parked
/// ones found by the planned routers' method tails, and the ones a router's
/// resolver bound (a site whose resolved reference the store no longer
/// names is read by its file alone).
fn calls_of(
    store: &Store,
    plan: &NavigationPlan<'_>,
    files: Option<&[String]>,
    reindexed: &HashSet<String>,
) -> Result<BTreeSet<FileReferenceSite>> {
    let mut sites = BTreeSet::new();
    for call in store.unresolved_calls_by_tails(&plan.tails(), files)? {
        if !reindexed.contains(&call.site.file_path)
            && plan.claims(&call.reference_name, &call.site.file_path)
        {
            sites.insert(call.site);
        }
    }
    for found in store.navigation_sites(files)? {
        let file = &found.site.file_path;
        let claimed = match &found.reference_name {
            Some(name) => plan.claims(name, file),
            None => plan.covers(file),
        };
        if claimed && !reindexed.contains(file) {
            sites.insert(found.site);
        }
    }
    Ok(sites)
}

#[cfg(test)]
mod tests {
    use super::ROUTE_LANGUAGES;
    use codegraph_resolve::FrameworkResolver;
    use codegraph_resolve::frameworks::{
        angular_router::AngularRouterResolver, expo_router::ExpoRouterResolver,
        nextjs::NextjsResolver, react_router::ReactRouterResolver,
        sveltekit_router::SvelteKitRouterResolver, tanstack_router::TanstackRouterResolver,
        vue_router::VueRouterResolver,
    };
    use std::collections::BTreeSet;

    /// The snapshot reads the routes of every router that binds navigation
    /// calls: their languages are the snapshot's.
    #[test]
    fn the_route_snapshot_reads_the_languages_of_every_navigation_router() {
        let routers: [&dyn FrameworkResolver; 7] = [
            &ReactRouterResolver,
            &NextjsResolver,
            &ExpoRouterResolver,
            &VueRouterResolver,
            &AngularRouterResolver,
            &TanstackRouterResolver,
            &SvelteKitRouterResolver,
        ];
        let mut languages = BTreeSet::new();
        for router in routers {
            assert!(router.navigation().is_some(), "{}", router.name());
            languages.extend(
                router
                    .languages()
                    .expect("a router reads its own languages")
                    .iter()
                    .map(|language| language.as_str()),
            );
        }
        let snapshot: BTreeSet<&str> = ROUTE_LANGUAGES
            .iter()
            .map(|language| language.as_str())
            .collect();
        assert_eq!(languages, snapshot);
    }
}
