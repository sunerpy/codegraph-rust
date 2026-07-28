//! Regression tests for the React / Vue / NestJS [`FrameworkResolver`]s.
//!
//! Each test drives a resolver through an in-memory [`ResolutionContext`] and
//! asserts the produced [`ResolvedRef`] / extraction shape against the upstream
//! semantics, with `resolvedBy = framework` and the upstream-specified confidence.

use std::collections::HashMap;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::{detect_frameworks, godot, nestjs, react, vue};
use codegraph_resolve::types::{ImportMapping, RefView, ResolutionContext, ResolvedBy};

/// Extraction context for a resolver call that needs no project configuration.
fn no_project_config() -> codegraph_resolve::framework::FrameworkExtractionContext {
    codegraph_resolve::framework::FrameworkExtractionContext::without_config("")
}

/// A self-contained, in-memory [`ResolutionContext`] for resolver tests.
#[derive(Default)]
struct MockContext {
    files: HashMap<String, String>,
    nodes: Vec<Node>,
}

impl MockContext {
    fn with_file(mut self, path: &str, content: &str) -> Self {
        self.files.insert(path.to_string(), content.to_string());
        self
    }

    fn with_node(mut self, node: Node) -> Self {
        self.nodes.push(node);
        self
    }
}

impl ResolutionContext for MockContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.file_path == file_path)
            .cloned()
            .collect()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name == name)
            .cloned()
            .collect()
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.qualified_name == qualified_name)
            .cloned()
            .collect()
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .cloned()
            .collect()
    }

    fn file_exists(&self, file_path: &str) -> bool {
        self.files.contains_key(file_path)
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        self.files.get(file_path).cloned()
    }

    fn get_project_root(&self) -> &str {
        "/project"
    }

    fn get_all_files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name.to_lowercase() == lower_name)
            .cloned()
            .collect()
    }

    fn get_node_by_id(&self, id: &str) -> Option<Node> {
        self.nodes.iter().find(|n| n.id == id).cloned()
    }

    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
}

fn node(id: &str, kind: NodeKind, name: &str, file_path: &str, lang: Language) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: format!("{file_path}::{name}"),
        file_path: file_path.to_string(),
        language: lang,
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

fn ref_view(name: &str, kind: EdgeKind, file_path: &str, lang: Language) -> RefView {
    RefView {
        row_id: None,
        from_node_id: format!("from:{file_path}"),
        reference_name: name.to_string(),
        reference_kind: kind,
        line: 1,
        column: 0,
        file_path: file_path.to_string(),
        language: lang,
        is_function_ref: false,
        reference_subkind: None,
    }
}

// ---------------------------------------------------------------------------
// React
// ---------------------------------------------------------------------------

#[test]
fn react_detects_via_package_json_react_dep() {
    let ctx =
        MockContext::default().with_file("package.json", r#"{"dependencies":{"react":"18"}}"#);
    assert!(react::ReactResolver.detect(&ctx));
}

#[test]
fn react_detects_via_tsx_file() {
    let ctx = MockContext::default().with_file("src/App.tsx", "export default function App(){}");
    assert!(react::ReactResolver.detect(&ctx));
}

#[test]
fn react_does_not_detect_plain_ts_project() {
    let ctx = MockContext::default()
        .with_file("package.json", r#"{"dependencies":{"lodash":"4"}}"#)
        .with_file("src/index.ts", "export const x = 1;");
    assert!(!react::ReactResolver.detect(&ctx));
}

#[test]
fn react_resolves_component_same_dir_with_framework_confidence() {
    let button = node(
        "component:src/Button.tsx:Button:1",
        NodeKind::Component,
        "Button",
        "src/Button.tsx",
        Language::Tsx,
    );
    let ctx = MockContext::default().with_node(button.clone());
    let reference = ref_view(
        "Button",
        EdgeKind::References,
        "src/Page.tsx",
        Language::Tsx,
    );
    let resolved = react::ReactResolver
        .resolve(&reference, &ctx)
        .expect("resolves");
    assert_eq!(resolved.target_node_id, button.id);
    assert_eq!(resolved.confidence, 0.8);
    assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
}

#[test]
fn react_does_not_resolve_component_from_plain_ts_file() {
    let button = node(
        "class:src/Account.ts:Account:1",
        NodeKind::Class,
        "Account",
        "src/Account.ts",
        Language::TypeScript,
    );
    let ctx = MockContext::default().with_node(button);
    // From a plain .ts file, component resolution must NOT fire (react.ts:43-44).
    let reference = ref_view(
        "Account",
        EdgeKind::References,
        "src/Other.ts",
        Language::TypeScript,
    );
    assert!(react::ReactResolver.resolve(&reference, &ctx).is_none());
}

#[test]
fn react_resolves_hook_preferring_hooks_dir() {
    let hook = node(
        "hook:src/hooks/useAuth.ts:useAuth:1",
        NodeKind::Function,
        "useAuth",
        "src/hooks/useAuth.ts",
        Language::TypeScript,
    );
    let ctx = MockContext::default().with_node(hook.clone());
    let reference = ref_view("useAuth", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
    let resolved = react::ReactResolver
        .resolve(&reference, &ctx)
        .expect("resolves");
    assert_eq!(resolved.target_node_id, hook.id);
    assert_eq!(resolved.confidence, 0.85);
    assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
}

#[test]
fn react_resolves_context() {
    let ctx_node = node(
        "variable:src/context/AuthContext.tsx:AuthContext:1",
        NodeKind::Variable,
        "AuthContext",
        "src/context/AuthContext.tsx",
        Language::Tsx,
    );
    let ctx = MockContext::default().with_node(ctx_node.clone());
    let reference = ref_view(
        "AuthContext",
        EdgeKind::References,
        "src/App.tsx",
        Language::Tsx,
    );
    let resolved = react::ReactResolver
        .resolve(&reference, &ctx)
        .expect("resolves");
    assert_eq!(resolved.target_node_id, ctx_node.id);
    assert_eq!(resolved.confidence, 0.8);
    assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
}

#[test]
fn react_extract_emits_nextjs_page_route() {
    let result = react::ReactResolver
        .extract(
            "pages/about.tsx",
            "export default function About() { return <div/>; }",
            &no_project_config(),
        )
        .expect("extract result");
    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route node");
    assert_eq!(route.name, "/about");
}

#[test]
fn react_extract_component_and_route_reference() {
    let content =
        "export function Home() { return <Layout/>; }\n<Route path=\"/home\" component={Home}/>";
    let result = react::ReactResolver
        .extract("src/Home.tsx", content, &no_project_config())
        .expect("extract result");
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Component && n.name == "Home")
    );
    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route");
    assert_eq!(route.name, "/home");
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "Home" && r.reference_kind == EdgeKind::References)
    );
}

/// Nested v6 shape from upstream #1348: the parent route has a `path` but renders
/// `<Outlet/>`, and the pathless `index` child must stay skipped. The unbounded
/// 400-byte window let the parent borrow the child's `element` and let the `index`
/// route borrow its sibling's `path`, producing `/dashboard -> DashboardHome`,
/// a duplicate `settings` node, and `settings -> DashboardHome`.
#[test]
fn react_route_window_stops_at_tag_end_not_at_sibling_routes() {
    let content = concat!(
        "function DashboardHome() { return null; }\n",
        "function Settings() { return null; }\n",
        "export function App() {\n",
        "  return (\n",
        "    <Routes>\n",
        "      <Route path=\"/dashboard\">\n",
        "        <Route index element={<DashboardHome/>} />\n",
        "        <Route path=\"settings\" element={<Settings/>} />\n",
        "      </Route>\n",
        "    </Routes>\n",
        "  );\n",
        "}\n",
    );
    let result = react::ReactResolver
        .extract("src/App.tsx", content, &no_project_config())
        .expect("extract result");

    let routes: Vec<(String, i64)> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| (n.name.clone(), n.start_line))
        .collect();
    assert_eq!(
        routes,
        vec![("/dashboard".to_string(), 6), ("settings".to_string(), 8)],
        "the pathless index route must not borrow its sibling's path"
    );

    let refs: Vec<(String, i64)> = result
        .references
        .iter()
        .map(|r| (r.reference_name.clone(), r.line))
        .collect();
    assert_eq!(
        refs,
        vec![("Settings".to_string(), 8)],
        "only the settings route owns an element; the parent renders <Outlet/>"
    );
}

/// An unterminated `<Route path=…` followed by a long run of `<` must not let the
/// scan run to end-of-file, and must not reach the well-formed route far below it.
#[test]
fn react_route_window_is_bounded_for_unterminated_tag_and_bare_angle_run() {
    let mut content = String::from("function Comp() { return null; }\n<Route path=\"/a\"\n");
    content.push_str(&"<".repeat(200_000));
    content.push_str("\n<Route path=\"/b\" element={<Comp/>} />\n");

    let result = react::ReactResolver
        .extract("src/Patho.tsx", &content, &no_project_config())
        .expect("extract result");

    let routes: Vec<String> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(routes, vec!["/a".to_string(), "/b".to_string()]);

    let refs: Vec<(String, i64)> = result
        .references
        .iter()
        .map(|r| (r.reference_name.clone(), r.line))
        .collect();
    assert_eq!(
        refs,
        vec![("Comp".to_string(), 4)],
        "the unterminated /a tag must not reach the /b element 200KB later"
    );
}

/// Pins `ROUTE_OPENING_TAG_SCAN_LIMIT` from the outside: a 700-byte prettier-wrapped
/// opening tag — wider than both the old 400-byte window and any tag measured while
/// sizing the bound — still yields its `path` and its `element`, so the bound cannot
/// be tightened to a value that truncates legitimate input.
#[test]
fn react_route_window_keeps_long_multiline_opening_tag_intact() {
    let filler: String = (0..12)
        .map(|i| format!("        data-attribute-number-{i:02}=\"filler-value-{i:02}\"\n"))
        .collect();
    let content = format!(
        concat!(
            "function DeepSettingsPage() {{ return null; }}\n",
            "export function Router() {{\n",
            "  return (\n",
            "    <Routes>\n",
            "      <Route\n",
            "        path=\"/dashboard/settings\"\n",
            "{filler}",
            "        element={{<DeepSettingsPage/>}}\n",
            "      />\n",
            "    </Routes>\n",
            "  );\n",
            "}}\n",
        ),
        filler = filler
    );
    let tag_start = content.find("<Route").expect("route tag");
    let tag_len = content[tag_start..].find("/>").expect("tag end") + 2;
    assert!(
        tag_len > 400,
        "fixture must exceed the old 400-byte window, got {tag_len}"
    );

    let result = react::ReactResolver
        .extract("src/Router.tsx", &content, &no_project_config())
        .expect("extract result");

    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route node");
    assert_eq!(route.name, "/dashboard/settings");
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "DeepSettingsPage"),
        "the element of a {tag_len}-byte opening tag must still be seen"
    );
}

/// The data-router half of upstream #1348: a PARENT route object carries a `path`
/// but no `element` of its own, and the first `element` the fixed 300-byte window
/// met belonged to a CHILD inside `children: [...]`. The parent must produce NO
/// borrowed edge, and each child exactly its own.
#[test]
fn react_data_router_parent_without_element_does_not_borrow_from_children() {
    let content = concat!(
        "const router = createBrowserRouter([\n",
        "  {\n",
        "    path: '/dashboard',\n",
        "    children: [\n",
        "      { index: true, element: <DashboardHome/> },\n",
        "      { path: 'settings', element: <Settings/> },\n",
        "    ],\n",
        "  },\n",
        "]);\n",
    );
    let result = react::ReactResolver
        .extract("src/routes.tsx", content, &no_project_config())
        .expect("extract result");

    let routes: Vec<(String, i64)> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| (n.name.clone(), n.start_line))
        .collect();
    assert_eq!(
        routes,
        vec![("settings".to_string(), 6)],
        "a parent route with no element of its own must emit no route->component pair"
    );

    let refs: Vec<(String, i64)> = result
        .references
        .iter()
        .map(|r| (r.reference_name.clone(), r.line))
        .collect();
    assert_eq!(
        refs,
        vec![("Settings".to_string(), 6)],
        "/dashboard must not borrow DashboardHome from its pathless index child"
    );
}

/// The mirror of the parent case, and the shape that makes the object's own closing
/// `}` load-bearing: a pathless-of-its-own CHILD sits inside `children: [...]` while
/// the PARENT declares its `element` AFTER the array. Walking past the child's `}`
/// would hand the parent's component to the child.
#[test]
fn react_data_router_child_without_element_does_not_borrow_from_parent() {
    let content = concat!(
        "const router = createBrowserRouter([\n",
        "  {\n",
        "    path: '/dashboard',\n",
        "    children: [\n",
        "      { path: 'settings' },\n",
        "    ],\n",
        "    element: <DashboardLayout/>,\n",
        "  },\n",
        "]);\n",
    );
    let result = react::ReactResolver
        .extract("src/routes.tsx", content, &no_project_config())
        .expect("extract result");

    let routes: Vec<(String, i64)> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| (n.name.clone(), n.start_line))
        .collect();
    assert_eq!(
        routes,
        vec![("/dashboard".to_string(), 3)],
        "the element-less child must emit nothing; only the parent owns an element"
    );

    let refs: Vec<(String, i64)> = result
        .references
        .iter()
        .map(|r| (r.reference_name.clone(), r.line))
        .collect();
    assert_eq!(
        refs,
        vec![("DashboardLayout".to_string(), 3)],
        "the child must not escape its own closing brace to borrow the parent's element"
    );
}

/// A malformed object carrying two depth-0 `path:` keys — no `{` and no `}` separates
/// them, so only the next-`path:` stop can keep the first from borrowing the second's
/// element.
#[test]
fn react_data_router_object_stops_at_next_path_key_when_unbraced() {
    let content = "createBrowserRouter([{ path: '/a', path: '/b', element: <B/> }]);\n";
    let result = react::ReactResolver
        .extract("src/routes.tsx", content, &no_project_config())
        .expect("extract result");

    let routes: Vec<String> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(
        routes,
        vec!["/b".to_string()],
        "the first path key has no element between it and the second key"
    );

    let refs: Vec<String> = result
        .references
        .iter()
        .map(|r| r.reference_name.clone())
        .collect();
    assert_eq!(refs, vec!["B".to_string()]);
}

/// A route object whose `element` is a nested JSX expression containing `>` inside
/// braces, and whose `path` string itself contains `>`. Neither `>` may end the
/// object, and the following SIBLING object's element must not leak backwards.
#[test]
fn react_data_router_object_survives_angle_brackets_and_stops_at_sibling() {
    let content = concat!(
        "const router = createBrowserRouter([\n",
        "  { path: '/a>b', element: <Guard fallback={<Spinner size={2 > 1 ? 8 : 4}/>}/> },\n",
        "  { path: '/c>d' },\n",
        "  { path: '/plain', element: <Plain/> },\n",
        "]);\n",
    );
    let result = react::ReactResolver
        .extract("src/routes.tsx", content, &no_project_config())
        .expect("extract result");

    let routes: Vec<(String, i64)> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| (n.name.clone(), n.start_line))
        .collect();
    assert_eq!(
        routes,
        vec![("/a>b".to_string(), 2), ("/plain".to_string(), 4)],
        "a `>` inside a quoted path or a braced expression must not end the object, \
         and an element-less object must not borrow from its next sibling"
    );

    let refs: Vec<(String, i64)> = result
        .references
        .iter()
        .map(|r| (r.reference_name.clone(), r.line))
        .collect();
    assert_eq!(
        refs,
        vec![("Guard".to_string(), 2), ("Plain".to_string(), 4)],
        "each object owns exactly its own element; no sibling leakage either way"
    );
}

/// A route object that legitimately runs well past the old 300-byte window — `loader`,
/// `action`, `errorElement`, `hydrateFallbackElement`, a nested `handle={{…}}` and a
/// `shouldRevalidate`, with its own `element` declared LAST — must still be seen whole.
/// This pins the new bound from the outside: tightening it below the object length
/// reddens this test.
#[test]
fn react_data_router_keeps_long_route_object_intact() {
    let content = concat!(
        "const router = createBrowserRouter([\n",
        "  {\n",
        "    path: '/organisations/:organisationId/settings/billing',\n",
        "    loader: billingSettingsLoader,\n",
        "    action: billingSettingsAction,\n",
        "    errorElement: <BillingSettingsErrorBoundary/>,\n",
        "    hydrateFallbackElement: <BillingSettingsSkeleton/>,\n",
        "    handle: {\n",
        "      crumb: () => 'Billing settings',\n",
        "      analytics: { screen: 'billing-settings', category: 'billing' },\n",
        "      permissions: ['billing:read', 'billing:write'],\n",
        "    },\n",
        "    shouldRevalidate: ({ currentUrl, nextUrl }) => currentUrl.pathname !== nextUrl.pathname,\n",
        "    element: <BillingSettingsPage/>,\n",
        "  },\n",
        "]);\n",
    );
    let path_key = content.find("path:").expect("path key");
    let element_key = content
        .find("element: <BillingSettings")
        .expect("own element");
    assert!(
        element_key - path_key > 300,
        "fixture must place its own element past the old 300-byte window, got {}",
        element_key - path_key
    );

    let result = react::ReactResolver
        .extract("src/routes.tsx", content, &no_project_config())
        .expect("extract result");

    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route node");
    assert_eq!(
        route.name,
        "/organisations/:organisationId/settings/billing"
    );
    assert_eq!(
        result
            .references
            .iter()
            .map(|r| r.reference_name.clone())
            .collect::<Vec<_>>(),
        vec!["BillingSettingsPage".to_string()],
        "the own element of a {}-byte route object must still be seen, and the \
         errorElement/hydrateFallbackElement must not be mistaken for it",
        element_key - path_key
    );
}

/// An unterminated route object followed by a well-formed one 200KB later: the walk
/// must neither run to end-of-file nor reach that far-away element.
#[test]
fn react_data_router_object_walk_is_bounded_for_unterminated_literal() {
    let mut content = String::from("createBrowserRouter([\n  { path: '/a',\n");
    content.push_str(&"filler,\n".repeat(25_000));
    content.push_str("  { path: '/b', element: <Comp/> },\n]);\n");

    let result = react::ReactResolver
        .extract("src/routes.tsx", &content, &no_project_config())
        .expect("extract result");

    let routes: Vec<String> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(
        routes,
        vec!["/b".to_string()],
        "the unterminated /a object has no element of its own and must emit nothing"
    );

    let refs: Vec<String> = result
        .references
        .iter()
        .map(|r| r.reference_name.clone())
        .collect();
    assert_eq!(
        refs,
        vec!["Comp".to_string()],
        "the unterminated /a object must not reach the /b element 200KB later"
    );
}

// ---------------------------------------------------------------------------
// Vue
// ---------------------------------------------------------------------------

#[test]
fn vue_detects_via_package_json() {
    let ctx = MockContext::default().with_file("package.json", r#"{"dependencies":{"vue":"3"}}"#);
    assert!(vue::VueResolver.detect(&ctx));
}

#[test]
fn vue_detects_via_vue_file() {
    let ctx = MockContext::default().with_file("src/App.vue", "<template></template>");
    assert!(vue::VueResolver.detect(&ctx));
}

#[test]
fn vue_does_not_detect_plain_project() {
    let ctx = MockContext::default()
        .with_file("package.json", r#"{"dependencies":{"express":"4"}}"#)
        .with_file("src/index.ts", "export const x = 1;");
    assert!(!vue::VueResolver.detect(&ctx));
}

#[test]
fn vue_resolves_compiler_macro_to_self_with_full_confidence() {
    let ctx = MockContext::default();
    let reference = ref_view("defineProps", EdgeKind::Calls, "src/App.vue", Language::Vue);
    let resolved = vue::VueResolver
        .resolve(&reference, &ctx)
        .expect("resolves");
    assert_eq!(resolved.target_node_id, reference.from_node_id);
    assert_eq!(resolved.confidence, 1.0);
    assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
}

#[test]
fn vue_resolves_pascalcase_component_call() {
    let comp = node(
        "component:src/Button.vue:Button:1",
        NodeKind::Component,
        "Button",
        "src/Button.vue",
        Language::Vue,
    );
    let ctx = MockContext::default()
        .with_file("src/Button.vue", "<template></template>")
        .with_node(comp.clone());
    let reference = ref_view("Button", EdgeKind::Calls, "src/Page.vue", Language::Vue);
    let resolved = vue::VueResolver
        .resolve(&reference, &ctx)
        .expect("resolves");
    assert_eq!(resolved.target_node_id, comp.id);
    assert_eq!(resolved.confidence, 0.8);
    assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
}

#[test]
fn vue_extract_emits_nuxt_page_route() {
    // the upstream extract keys on `/pages/` (with leading slash), so the route file
    // must sit under a parent dir (vue.ts:198).
    let result = vue::VueResolver
        .extract("app/pages/users/[id].vue", "", &no_project_config())
        .expect("extract result");
    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route node");
    assert_eq!(route.name, "/users/:id");
}

// ---------------------------------------------------------------------------
// NestJS
// ---------------------------------------------------------------------------

#[test]
fn nestjs_detects_via_package_json() {
    let ctx = MockContext::default()
        .with_file("package.json", r#"{"dependencies":{"@nestjs/core":"10"}}"#);
    assert!(nestjs::NestjsResolver.detect(&ctx));
}

#[test]
fn nestjs_does_not_detect_plain_project() {
    let ctx = MockContext::default()
        .with_file("package.json", r#"{"dependencies":{"express":"4"}}"#)
        .with_file("src/index.ts", "export const x = 1;");
    assert!(!nestjs::NestjsResolver.detect(&ctx));
}

#[test]
fn nestjs_resolves_service_provider_preferring_convention() {
    let service = node(
        "class:src/users/users.service.ts:UsersService:1",
        NodeKind::Class,
        "UsersService",
        "src/users/users.service.ts",
        Language::TypeScript,
    );
    let ctx = MockContext::default().with_node(service.clone());
    let reference = ref_view(
        "UsersService",
        EdgeKind::References,
        "src/users/users.controller.ts",
        Language::TypeScript,
    );
    let resolved = nestjs::NestjsResolver
        .resolve(&reference, &ctx)
        .expect("resolves");
    assert_eq!(resolved.target_node_id, service.id);
    assert_eq!(resolved.confidence, 0.85);
    assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
}

#[test]
fn nestjs_extract_http_route_joins_controller_prefix() {
    let content = "@Controller('users')\nclass UsersController {\n  @Get(':id')\n  findOne() {}\n}";
    let result = nestjs::NestjsResolver
        .extract(
            "src/users/users.controller.ts",
            content,
            &no_project_config(),
        )
        .expect("extract result");
    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route node");
    assert_eq!(route.name, "GET /users/:id");
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "findOne" && r.reference_kind == EdgeKind::References)
    );
}

#[test]
fn nestjs_post_extract_applies_router_module_prefix() {
    // The controller route is `GET /` in-file; app.module.ts registers
    // UsersModule (which declares UsersController) under '/admin' via
    // RouterModule.register, so post_extract rewrites the route name.
    let mut controller = node(
        "class:src/users/users.controller.ts:UsersController:1",
        NodeKind::Class,
        "UsersController",
        "src/users/users.controller.ts",
        Language::TypeScript,
    );
    // The route lives inside the controller's line range (post_extract gates on it).
    controller.start_line = 1;
    controller.end_line = 10;
    let mut route = node(
        "route:src/users/users.controller.ts:3:GET:/",
        NodeKind::Route,
        "GET /",
        "src/users/users.controller.ts",
        Language::TypeScript,
    );
    route.qualified_name = "src/users/users.controller.ts::GET:".to_string();
    route.start_line = 3;
    route.end_line = 3;
    let module_content = "@Module({ controllers: [UsersController] })\nexport class UsersModule {}\nRouterModule.register([{ path: 'admin', module: UsersModule }]);";
    let ctx = MockContext::default()
        .with_file("src/app.module.ts", module_content)
        .with_node(controller)
        .with_node(route);

    let updates = nestjs::NestjsResolver
        .post_extract(&ctx)
        .expect("post extract runs");
    let updated = updates
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("updated route node");
    assert_eq!(updated.name, "GET /admin");
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[test]
fn detect_frameworks_returns_only_matching_resolvers() {
    let ctx =
        MockContext::default().with_file("package.json", r#"{"dependencies":{"react":"18"}}"#);
    let detected = detect_frameworks(&ctx);
    let names: Vec<&str> = detected.iter().map(|r| r.name()).collect();
    assert_eq!(names, vec!["react"]);
}

#[test]
fn detect_frameworks_empty_on_plain_project() {
    let ctx = MockContext::default()
        .with_file("package.json", r#"{"dependencies":{"lodash":"4"}}"#)
        .with_file("src/index.ts", "export const x = 1;");
    assert!(detect_frameworks(&ctx).is_empty());
}

// ---------------------------------------------------------------------------
// Godot (T2 skeleton: registration + detect only; resolve/extract/post_extract
// are stubs filled in T3-T7)
// ---------------------------------------------------------------------------

#[test]
fn godot_detects_project_with_project_godot() {
    // A project carrying a `project.godot` at its root IS a Godot project.
    let ctx = MockContext::default()
        .with_file("project.godot", "config_version=5\n\n[application]\n")
        .with_file("scenes/Main.tscn", "[gd_scene]\n")
        .with_file("scripts/player.gd", "extends Node\n");
    assert!(godot::GodotResolver.detect(&ctx));
}

#[test]
fn godot_does_not_detect_project_without_project_godot() {
    // No `project.godot` anywhere -> the resolver must NOT activate (it would
    // otherwise fire on every project, since languages() == None).
    let ctx = MockContext::default()
        .with_file("package.json", r#"{"dependencies":{"lodash":"4"}}"#)
        .with_file("scripts/player.gd", "extends Node\n");
    assert!(!godot::GodotResolver.detect(&ctx));
}

#[test]
fn godot_languages_is_none_applies_to_all() {
    // None == apply to all languages, so extract() can later see .tscn/.tres/
    // project.godot/.gd files regardless of their Language variant.
    assert!(godot::GodotResolver.languages().is_none());
}

#[test]
fn godot_resolver_name_is_godot() {
    assert_eq!(godot::GodotResolver.name(), "godot");
}

#[test]
fn detect_frameworks_includes_godot_for_godot_project() {
    // The registry instantiates GodotResolver and consults its detect().
    let ctx =
        MockContext::default().with_file("project.godot", "config_version=5\n\n[application]\n");
    let detected = detect_frameworks(&ctx);
    let names: Vec<&str> = detected.iter().map(|r| r.name()).collect();
    assert!(names.contains(&"godot"), "expected godot, got {names:?}");
}

#[test]
fn detect_frameworks_excludes_godot_without_project_godot() {
    // No spurious activation: a non-Godot project must not list godot.
    let ctx =
        MockContext::default().with_file("package.json", r#"{"dependencies":{"react":"18"}}"#);
    let detected = detect_frameworks(&ctx);
    let names: Vec<&str> = detected.iter().map(|r| r.name()).collect();
    assert!(
        !names.contains(&"godot"),
        "godot must not fire, got {names:?}"
    );
}
