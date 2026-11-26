//! The route references a sync follows to the modules they lazily load
//! (W7-23; upstream #2422's `referenceNameTail` and `moduleReferenceKeys`,
//! `sync-route-module-retry.test.ts`), and the lookups its lanes read them by.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::queries::{LaneReference, LaneReferences};
use codegraph_store::{Store, import_path_keys, route_module_path, route_module_stem};

/// The stem a route's module reference waits under, or `None` when it names
/// no module (upstream parks it under `module:` + this).
fn stem(name: &str, kind: EdgeKind) -> Option<&str> {
    route_module_path(name, kind).and_then(route_module_stem)
}

#[test]
fn a_routes_module_reference_waits_under_the_stem_of_its_path() {
    let references = |name| stem(name, EdgeKind::References);
    // React Router: a lazy route, and the lazy layout route around another.
    assert_eq!(references("lazy-import:./pages/Team"), Some("Team"));
    assert_eq!(references("lazy-import:./routes/about.tsx"), Some("about"));
    assert_eq!(references("lazy-import:@/pages/Billing"), Some("Billing"));
    assert_eq!(
        references("lazy-import:./pages/Settings/"),
        Some("Settings")
    );
    assert_eq!(
        references("layout:lazy-import:./layouts/Shell"),
        Some("Shell")
    );
    // Vue Router: the component a route renders is a call, its layout a
    // reference; vben's `#/` alias puts a `#` in the path.
    let calls = |name| stem(name, EdgeKind::Calls);
    assert_eq!(calls("import:@/views/Login#Login"), Some("Login"));
    assert_eq!(calls("import:../views/Home.vue#Home"), Some("Home"));
    assert_eq!(
        calls("import:#/views/dashboard/analytics/index.vue#index"),
        Some("index")
    );
    assert_eq!(
        route_module_path(
            "import:#/views/dashboard/analytics/index.vue#index",
            EdgeKind::Calls
        ),
        Some("#/views/dashboard/analytics/index.vue")
    );
    assert_eq!(
        references("layout:import:@/layout/index.vue#index"),
        Some("index")
    );
    // Angular: a member the import names, or the module's default.
    assert_eq!(
        references("import:./home/home.component#HomeComponent"),
        Some("home")
    );
    assert_eq!(
        references("import:./about/about.component#default"),
        Some("about")
    );
    assert_eq!(
        references("layout:import:./admin/shell.component#ShellComponent"),
        Some("shell")
    );
}

#[test]
fn a_reference_that_names_no_module_has_no_stem() {
    // A layout named by its component, a module path of dots only, an
    // import name with no member, and an import itself.
    assert_eq!(stem("layout:MainLayout", EdgeKind::References), None);
    assert_eq!(
        route_module_path("lazy-import:..", EdgeKind::References),
        Some("..")
    );
    assert_eq!(stem("lazy-import:..", EdgeKind::References), None);
    assert_eq!(stem("lazy-import:./", EdgeKind::References), None);
    assert_eq!(stem("import:./x", EdgeKind::Calls), None);
    assert_eq!(stem("./pages/Team", EdgeKind::Imports), None);
    // The same names as another kind of reference are not a route's.
    assert_eq!(
        stem("import:./home.component#HomeComponent", EdgeKind::Imports),
        None
    );
    assert_eq!(stem("lazy-import:./pages/Team", EdgeKind::TypeOf), None);
    // An empty path or member is no module either.
    assert_eq!(stem("lazy-import:", EdgeKind::References), None);
    assert_eq!(stem("import:#Page", EdgeKind::References), None);
    assert_eq!(stem("import:./x#", EdgeKind::References), None);
}

/// Upstream `moduleReferenceKeys` (`importPathKeys` behind `module:`): a file
/// carries the stem a module reference it could satisfy waits under.
#[test]
fn a_file_carries_the_stem_of_a_module_reference_it_can_satisfy() {
    assert_eq!(
        import_path_keys("src/pages/Team.tsx"),
        ["pages", "Team.tsx", "Team"]
    );
    assert_eq!(
        import_path_keys("src/pages/Settings/index.tsx"),
        ["Settings", "index.tsx", "index"]
    );
    assert_eq!(
        import_path_keys("src/app/home/home.component.ts"),
        ["home", "home.component.ts"]
    );
    for (name, kind, file) in [
        (
            "lazy-import:./pages/Team",
            EdgeKind::References,
            "src/pages/Team.tsx",
        ),
        (
            "lazy-import:./pages/Settings",
            EdgeKind::References,
            "src/pages/Settings/index.tsx",
        ),
        (
            "import:./home/home.component#HomeComponent",
            EdgeKind::References,
            "src/app/home/home.component.ts",
        ),
        (
            "import:../views/About.vue#About",
            EdgeKind::Calls,
            "src/views/About.vue",
        ),
    ] {
        let stem = stem(name, kind).expect("a module reference");
        assert!(
            import_path_keys(file).iter().any(|key| key == stem),
            "{name} waits under {stem:?}, a key of {file}"
        );
    }
}

fn store(label: &str) -> Store {
    let path = std::env::temp_dir().join(format!(
        "codegraph-store-route-modules-{label}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    Store::open(&path).expect("open store")
}

fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified: &str,
    file: &str,
    language: Language,
) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qualified.to_string(),
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

fn reference(
    from: &str,
    name: &str,
    kind: EdgeKind,
    line: i64,
    file: &str,
    language: Language,
) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line,
        col: 0,
        candidates: None,
        file_path: file.to_string(),
        language,
        is_function_ref: false,
        reference_subkind: None,
    }
}

/// Move every row of `unresolved_refs` whose name is one of `names` into
/// `resolved_refs`, as a resolution pass moves the ones it resolves.
fn resolve(store: &mut Store, names: &[&str]) {
    let ids: Vec<i64> = store
        .all_unresolved_refs()
        .unwrap()
        .into_iter()
        .filter(|reference| names.contains(&reference.reference_name.as_str()))
        .filter_map(|reference| reference.id)
        .collect();
    store.delete_resolved_unresolved_refs(&ids).unwrap();
}

fn names_of(found: &[LaneReference]) -> Vec<(String, bool)> {
    let mut names: Vec<(String, bool)> = found
        .iter()
        .map(|reference| (reference.reference_name.clone(), reference.resolved))
        .collect();
    names.sort();
    names
}

#[test]
fn the_js_routes_family_is_the_references_of_the_javascript_routers_routes() {
    let mut store = store("js-routes");
    store
        .upsert_nodes(&[
            node(
                "route:src/router.tsx:2:/team",
                NodeKind::Route,
                "/team",
                "src/router.tsx::route:/team",
                "src/router.tsx",
                Language::Tsx,
            ),
            node(
                "route:src/router/index.js:3:/:vue",
                NodeKind::Route,
                "/",
                "src/router/index.js::route:/",
                "src/router/index.js",
                Language::JavaScript,
            ),
            node(
                "route:config/routes.rb:1:GET /",
                NodeKind::Route,
                "GET /",
                "config/routes.rb::route:GET /",
                "config/routes.rb",
                Language::Ruby,
            ),
            node(
                "function:app",
                NodeKind::Function,
                "App",
                "App",
                "src/router.tsx",
                Language::Tsx,
            ),
        ])
        .unwrap();
    store
        .insert_unresolved_refs(&[
            reference(
                "route:src/router.tsx:2:/team",
                "lazy-import:./pages/Team",
                EdgeKind::References,
                2,
                "src/router.tsx",
                Language::Tsx,
            ),
            reference(
                "route:src/router.tsx:2:/team",
                "layout:Shell",
                EdgeKind::References,
                2,
                "src/router.tsx",
                Language::Tsx,
            ),
            reference(
                "route:src/router/index.js:3:/:vue",
                "import:../views/Home.vue#Home",
                EdgeKind::Calls,
                3,
                "src/router/index.js",
                Language::JavaScript,
            ),
            reference(
                "route:config/routes.rb:1:GET /",
                "HomeController#index",
                EdgeKind::References,
                1,
                "config/routes.rb",
                Language::Ruby,
            ),
            reference(
                "function:app",
                "lazy-import:./pages/Team",
                EdgeKind::References,
                5,
                "src/router.tsx",
                Language::Tsx,
            ),
        ])
        .unwrap();
    resolve(&mut store, &["layout:Shell"]);
    assert_eq!(
        names_of(&store.lane_references(&LaneReferences::JsRoutes).unwrap()),
        [
            ("import:../views/Home.vue#Home".to_string(), false),
            ("layout:Shell".to_string(), true),
            ("lazy-import:./pages/Team".to_string(), false),
        ]
    );
}

/// No ceiling (W7-23, memory `name-tail-retry-ceiling`): every route
/// reference waiting under one stem is read, however many there are.
#[test]
fn every_route_reference_of_a_stem_is_read_however_many_share_it() {
    let mut store = store("js-routes-many");
    let routes: Vec<Node> = (0..501)
        .map(|line| {
            node(
                &format!("route:src/router.tsx:{line}:/t{line}"),
                NodeKind::Route,
                &format!("/t{line}"),
                &format!("src/router.tsx::route:/t{line}"),
                "src/router.tsx",
                Language::Tsx,
            )
        })
        .collect();
    store.upsert_nodes(&routes).unwrap();
    let refs: Vec<UnresolvedRef> = (0..501)
        .map(|line| {
            reference(
                &format!("route:src/router.tsx:{line}:/t{line}"),
                "lazy-import:./pages/Team",
                EdgeKind::References,
                line,
                "src/router.tsx",
                Language::Tsx,
            )
        })
        .collect();
    store.insert_unresolved_refs(&refs).unwrap();
    let found = store.lane_references(&LaneReferences::JsRoutes).unwrap();
    let waiting = found
        .iter()
        .filter(|reference| {
            stem(&reference.reference_name, reference.reference_kind) == Some("Team")
        })
        .count();
    assert_eq!(waiting, 501);
}

#[test]
fn a_file_holds_a_dynamic_import_when_it_parks_a_call_named_import() {
    let mut store = store("dynamic-imports");
    store
        .upsert_nodes(&[
            node(
                "constant:docs",
                NodeKind::Constant,
                "Docs",
                "Docs",
                "src/App.tsx",
                Language::Tsx,
            ),
            node(
                "constant:team",
                NodeKind::Constant,
                "Team",
                "Team",
                "src/Other.tsx",
                Language::Tsx,
            ),
        ])
        .unwrap();
    store
        .insert_unresolved_refs(&[
            reference(
                "constant:docs",
                "import",
                EdgeKind::Calls,
                3,
                "src/App.tsx",
                Language::Tsx,
            ),
            reference(
                "constant:team",
                "import",
                EdgeKind::Calls,
                4,
                "src/Other.tsx",
                Language::Tsx,
            ),
            reference(
                "constant:team",
                "lazy",
                EdgeKind::Calls,
                4,
                "src/Other.tsx",
                Language::Tsx,
            ),
        ])
        .unwrap();
    // The one in `Other.tsx` is resolved, which only a node named `import`
    // allows: without one, `resolved_refs` is not read for it.
    let ids: Vec<i64> = store
        .all_unresolved_refs()
        .unwrap()
        .into_iter()
        .filter(|reference| reference.file_path == "src/Other.tsx")
        .filter_map(|reference| reference.id)
        .collect();
    store.delete_resolved_unresolved_refs(&ids).unwrap();
    assert_eq!(
        store
            .files_with_dynamic_imports()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["src/App.tsx"]
    );
    store
        .upsert_nodes(&[node(
            "method:import",
            NodeKind::Method,
            "import",
            "Loader::import",
            "src/loader.ts",
            Language::TypeScript,
        )])
        .unwrap();
    assert_eq!(
        store
            .files_with_dynamic_imports()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        ["src/App.tsx", "src/Other.tsx"]
    );
}

#[test]
fn a_same_file_value_reference_names_a_module_level_value_of_its_own_file() {
    let mut store = store("same-file-values");
    store
        .upsert_nodes(&[
            node(
                "constant:docs",
                NodeKind::Constant,
                "Docs",
                "Docs",
                "src/App.tsx",
                Language::Tsx,
            ),
            node(
                "variable:page",
                NodeKind::Variable,
                "Page",
                "Page",
                "src/App.tsx",
                Language::Tsx,
            ),
            node(
                "constant:inner",
                NodeKind::Constant,
                "Inner",
                "App::Inner",
                "src/App.tsx",
                Language::Tsx,
            ),
            node(
                "function:app",
                NodeKind::Function,
                "App",
                "App",
                "src/App.tsx",
                Language::Tsx,
            ),
            node(
                "constant:other",
                NodeKind::Constant,
                "Other",
                "Other",
                "src/Other.tsx",
                Language::Tsx,
            ),
            node(
                "route:src/App.tsx:9:/docs",
                NodeKind::Route,
                "/docs",
                "src/App.tsx::route:/docs",
                "src/App.tsx",
                Language::Tsx,
            ),
        ])
        .unwrap();
    store
        .insert_unresolved_refs(&[
            reference(
                "route:src/App.tsx:9:/docs",
                "Docs",
                EdgeKind::References,
                9,
                "src/App.tsx",
                Language::Tsx,
            ),
            reference(
                "function:app",
                "Docs",
                EdgeKind::References,
                9,
                "src/App.tsx",
                Language::Tsx,
            ),
            // A route's layout that is the value.
            reference(
                "route:src/App.tsx:9:/docs",
                "layout:Docs",
                EdgeKind::References,
                9,
                "src/App.tsx",
                Language::Tsx,
            ),
            reference(
                "function:app",
                "Page",
                EdgeKind::References,
                10,
                "src/App.tsx",
                Language::Tsx,
            ),
            // A nested value, another file's value, and a call are not.
            reference(
                "function:app",
                "Inner",
                EdgeKind::References,
                11,
                "src/App.tsx",
                Language::Tsx,
            ),
            reference(
                "function:app",
                "Other",
                EdgeKind::References,
                12,
                "src/App.tsx",
                Language::Tsx,
            ),
            reference(
                "function:app",
                "Docs",
                EdgeKind::Calls,
                13,
                "src/App.tsx",
                Language::Tsx,
            ),
        ])
        .unwrap();
    resolve(&mut store, &["Page"]);
    let found = store
        .same_file_value_references(&["src/App.tsx".to_string(), "src/Other.tsx".to_string()])
        .unwrap();
    let mut seen: Vec<(String, String, i64)> = found
        .into_iter()
        .map(|reference| {
            (
                reference.from_node_id,
                reference.reference_name,
                reference.line,
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("function:app".to_string(), "Docs".to_string(), 9),
            ("function:app".to_string(), "Page".to_string(), 10),
            (
                "route:src/App.tsx:9:/docs".to_string(),
                "Docs".to_string(),
                9
            ),
            (
                "route:src/App.tsx:9:/docs".to_string(),
                "layout:Docs".to_string(),
                9
            ),
        ]
    );
    assert!(store.same_file_value_references(&[]).unwrap().is_empty());
}
