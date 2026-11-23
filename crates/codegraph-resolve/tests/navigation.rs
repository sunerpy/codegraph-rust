//! The routes a sync compares and the routers' navigation hooks it asks
//! which calls a changed route may answer (W7-23b; upstream #2425's
//! `changedRoutes` and `FrameworkResolver.navigation`,
//! `sync-navigation-retry.test.ts`).

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::{
    angular_router::AngularRouterResolver, expo_router::ExpoRouterResolver, nextjs::NextjsResolver,
    react_router::ReactRouterResolver, sveltekit_router::SvelteKitRouterResolver,
    tanstack_router::TanstackRouterResolver, vue_router::VueRouterResolver,
};
use codegraph_resolve::navigation::changed_routes;
use codegraph_store::reference_name_tail;

fn route(file: &str, name: &str, id: Option<&str>) -> Node {
    Node {
        id: id.map_or_else(|| format!("route:{file}:1:{name}"), str::to_string),
        kind: NodeKind::Route,
        name: name.to_string(),
        qualified_name: format!("{file}::route:{name}"),
        file_path: file.to_string(),
        language: Language::Jsx,
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

/// Upstream: the routes on one side only, compared by file and path; a body
/// edit that moves a route's line, and with it its id, keeps the route.
#[test]
fn the_changed_routes_are_those_on_one_side_only_by_file_and_path() {
    let kept = route("src/App.jsx", "/", None);
    let moved = route("src/App.jsx", "/cart", None);
    let gone = route("src/App.jsx", "/login", None);
    let added = route("src/Admin.jsx", "/admin", None);
    let mut kept_lower = kept.clone();
    kept_lower.id = "route:src/App.jsx:2:/".to_string();
    kept_lower.start_line = 2;
    assert_eq!(
        changed_routes(
            &[kept, moved.clone(), gone.clone()],
            &[kept_lower, moved, added.clone()]
        ),
        [gone, added]
    );
    assert!(changed_routes(&[], &[]).is_empty());
}

/// Upstream: a route renamed in place (the same node under a new path) is
/// its old path and its new one.
#[test]
fn a_route_renamed_in_place_is_its_old_path_and_its_new_one() {
    let old = route(
        "src/router.tsx",
        "/auth/sign-in",
        Some("route:src/router.tsx:5:paths.auth.login.path"),
    );
    let mut renamed = old.clone();
    renamed.name = "/auth/login".to_string();
    assert_eq!(
        changed_routes(std::slice::from_ref(&old), std::slice::from_ref(&renamed)),
        [old, renamed]
    );
}

/// The seven routers that bind navigation calls, by their names.
fn routers() -> Vec<Box<dyn FrameworkResolver>> {
    vec![
        Box::new(ReactRouterResolver),
        Box::new(NextjsResolver),
        Box::new(ExpoRouterResolver),
        Box::new(VueRouterResolver),
        Box::new(AngularRouterResolver),
        Box::new(TanstackRouterResolver),
        Box::new(SvelteKitRouterResolver),
    ]
}

/// Upstream `FrameworkResolver.navigation`: every router that binds
/// navigation calls sets the hook, claims the calls its own tests resolve,
/// and finds each by its tail.
#[test]
fn every_router_that_binds_navigation_finds_its_calls_by_their_tail() {
    let calls: &[(&str, &[&str])] = &[
        (
            "react-router",
            &[
                "history.push",
                "history.replace",
                "navigate",
                "navigate.push",
                "router.navigate",
                "redirect",
            ],
        ),
        (
            "nextjs",
            &[
                "router.push",
                "router.replace",
                "router.prefetch",
                "redirect",
                "permanentRedirect",
                "NextResponse.redirect",
            ],
        ),
        (
            "expo-router",
            &[
                "router.push",
                "router.replace",
                "router.navigate",
                "router.dismissTo",
                "nav.push",
            ],
        ),
        (
            "vue-router",
            &[
                "router.push",
                "this.$router.push",
                "$router.replace",
                "navigateTo",
            ],
        ),
        (
            "angular-router",
            &[
                "this.router.navigate",
                "router.navigateByUrl",
                "this._router.createUrlTree",
                "Router.parseUrl",
            ],
        ),
        (
            "tanstack-router",
            &["navigate", "redirect", "router.navigate", "Route.navigate"],
        ),
        ("sveltekit-router", &["goto", "redirect"]),
    ];
    let routers = routers();
    let mut names: Vec<&str> = routers.iter().map(|router| router.name()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = calls.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    assert_eq!(names, expected);
    for router in &routers {
        let navigation = router
            .navigation()
            .unwrap_or_else(|| panic!("{} binds navigation", router.name()));
        let (_, router_calls) = calls
            .iter()
            .find(|(name, _)| *name == router.name())
            .expect("a router of the table");
        for call in *router_calls {
            assert!(
                router.claims_reference(call),
                "{} claims {call}",
                router.name()
            );
            let tail = reference_name_tail(call, EdgeKind::Calls);
            assert!(
                navigation.tails().contains(&tail.as_str()),
                "{}: {call} by its tail {tail}",
                router.name()
            );
        }
    }
}

/// KEEP-RUST (W7-23b, OD-B43): which routers read more than their route
/// nodes. Vue Router reads a route's name from its file's text, Angular a
/// redirect, so a changed routes file of theirs moves calls though no route
/// node did; Angular and React Router read a call's destination through the
/// constants another file holds.
#[test]
fn the_routers_that_read_route_files_or_other_files_say_so() {
    for router in routers() {
        let navigation = router.navigation().expect("a navigation hook");
        let name = router.name();
        assert_eq!(
            navigation.table_reads_route_files(),
            matches!(name, "vue-router" | "angular-router"),
            "{name}"
        );
        assert_eq!(
            navigation.destinations_read_other_files(),
            matches!(name, "angular-router" | "react-router"),
            "{name}"
        );
    }
}
