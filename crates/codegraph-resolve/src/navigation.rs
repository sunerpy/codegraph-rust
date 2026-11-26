//! The routes a sync compares, and the plan of the navigation calls a
//! changed route may answer (W7-23b: upstream #2425's `changedRoutes` and
//! `reopenNavigationsFor`).
//!
//! A navigation call names its route by path (`navigate('/login')`) and its
//! reference by the router's method (`navigate`, `history.push`). No name a
//! changed routes file declares is the call's, so a call parked while its
//! route was missing stays parked, and a call bound to the best route then
//! (a catch-all, a parameter route, one arm of a conditional) keeps it while
//! a better one appears. A sync compares the route nodes before and after
//! its change ([`changed_routes`]), and each router that binds navigation
//! says which files' calls a changed route may answer
//! ([`crate::framework::NavigationCalls`]): a [`NavigationPlan`].

use codegraph_core::types::Node;
use std::collections::{BTreeSet, HashSet};

use crate::framework::FrameworkResolver;

/// The routes a sync added, removed or renamed (upstream `changedRoutes`):
/// `before` is every route node before the sync changed a file, `after`
/// every one once its framework passes ran, and a route whose file and path
/// are on one side only changed. A body edit that moves a route's line, and
/// with it its id, keeps the route; a route renamed in place (the same node
/// under a new path) is in the result twice, as its old self and its new
/// one. The removed routes come first, each side in its input order.
#[must_use]
pub fn changed_routes(before: &[Node], after: &[Node]) -> Vec<Node> {
    let key = |route: &Node| (route.file_path.clone(), route.name.clone());
    let before_keys: HashSet<(String, String)> = before.iter().map(key).collect();
    let after_keys: HashSet<(String, String)> = after.iter().map(key).collect();
    before
        .iter()
        .filter(|route| !after_keys.contains(&key(route)))
        .chain(
            after
                .iter()
                .filter(|route| !before_keys.contains(&key(route))),
        )
        .cloned()
        .collect()
}

/// The routers whose navigation calls a sync sends back, each with the path
/// prefixes of the files whose calls it may answer (upstream
/// `reopenNavigationsFor`'s plans): see
/// [`crate::ReferenceResolver::navigation_plan`] and
/// [`crate::ReferenceResolver::destination_plan`].
pub struct NavigationPlan<'r> {
    routers: Vec<(&'r dyn FrameworkResolver, Vec<String>)>,
}

impl<'r> NavigationPlan<'r> {
    /// A plan of `routers`, each with a scope that is not empty.
    pub(crate) fn new(routers: Vec<(&'r dyn FrameworkResolver, Vec<String>)>) -> Self {
        Self { routers }
    }

    /// Whether no router's calls go back.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routers.is_empty()
    }

    /// The method tails of the planned routers' calls, sorted: what finds
    /// them among the parked references.
    #[must_use]
    pub fn tails(&self) -> Vec<String> {
        let mut tails: BTreeSet<&str> = BTreeSet::new();
        for (router, _) in &self.routers {
            if let Some(navigation) = router.navigation() {
                tails.extend(navigation.tails().iter().copied());
            }
        }
        tails.into_iter().map(str::to_string).collect()
    }

    /// Whether a planned router claims a call named `name` in `file`, a file
    /// of its scope (upstream `claimed`).
    #[must_use]
    pub fn claims(&self, name: &str, file: &str) -> bool {
        self.routers.iter().any(|(router, scopes)| {
            scopes.iter().any(|scope| file.starts_with(scope.as_str()))
                && router.claims_reference(name)
        })
    }

    /// Whether `file` is in a planned router's scope: what a call whose name
    /// the store no longer holds is read by.
    #[must_use]
    pub fn covers(&self, file: &str) -> bool {
        self.routers
            .iter()
            .any(|(_, scopes)| scopes.iter().any(|scope| file.starts_with(scope.as_str())))
    }
}
