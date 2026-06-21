//! Regression tests for the React / Vue / NestJS [`FrameworkResolver`]s.
//!
//! Each test drives a resolver through an in-memory [`ResolutionContext`] and
//! asserts the produced [`ResolvedRef`] / extraction shape against the upstream
//! semantics, with `resolvedBy = framework` and the upstream-specified confidence.

use std::collections::HashMap;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::{detect_frameworks, nestjs, react, vue};
use codegraph_resolve::types::{ImportMapping, RefView, ResolutionContext, ResolvedBy};

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
        from_node_id: format!("from:{file_path}"),
        reference_name: name.to_string(),
        reference_kind: kind,
        line: 1,
        column: 0,
        file_path: file_path.to_string(),
        language: lang,
        is_function_ref: false,
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
        .extract("src/Home.tsx", content)
        .expect("extract result");
    assert!(result
        .nodes
        .iter()
        .any(|n| n.kind == NodeKind::Component && n.name == "Home"));
    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route");
    assert_eq!(route.name, "/home");
    assert!(result
        .references
        .iter()
        .any(|r| r.reference_name == "Home" && r.reference_kind == EdgeKind::References));
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
        .extract("app/pages/users/[id].vue", "")
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
        .extract("src/users/users.controller.ts", content)
        .expect("extract result");
    let route = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Route)
        .expect("route node");
    assert_eq!(route.name, "GET /users/:id");
    assert!(result
        .references
        .iter()
        .any(|r| r.reference_name == "findOne" && r.reference_kind == EdgeKind::References));
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
