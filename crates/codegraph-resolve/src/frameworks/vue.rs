//! Vue / Nuxt [`FrameworkResolver`] — ports
//! `upstream resolution/frameworks/vue.ts`.

use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Language, NodeKind};
use regex::Regex;

use super::framework_node;
use crate::framework::FrameworkResolver;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// Vue 3 compiler macros (`VUE_COMPILER_MACROS`, vue.ts:14-22).
const VUE_COMPILER_MACROS: [&str; 7] = [
    "defineProps",
    "defineEmits",
    "defineExpose",
    "defineOptions",
    "defineSlots",
    "defineModel",
    "withDefaults",
];

/// Nuxt auto-imported composables (`NUXT_AUTO_IMPORTS`, vue.ts:27-67).
const NUXT_AUTO_IMPORTS: [&str; 30] = [
    "useRoute",
    "useRouter",
    "navigateTo",
    "abortNavigation",
    "useFetch",
    "useAsyncData",
    "useLazyFetch",
    "useLazyAsyncData",
    "refreshNuxtData",
    "useState",
    "clearNuxtState",
    "useHead",
    "useSeoMeta",
    "useServerSeoMeta",
    "useRuntimeConfig",
    "useAppConfig",
    "useNuxtApp",
    "useCookie",
    "useError",
    "createError",
    "showError",
    "clearError",
    "definePageMeta",
    "defineNuxtConfig",
    "defineNuxtPlugin",
    "defineNuxtRouteMiddleware",
    "useRequestHeaders",
    "useRequestEvent",
    "useRequestFetch",
    "useRequestURL",
];

/// Nuxt virtual module prefixes (`NUXT_VIRTUAL_MODULES`, vue.ts:72-78).
const NUXT_VIRTUAL_MODULES: [&str; 5] = ["#imports", "#components", "#app", "#build", "#head"];

/// Vue / Nuxt resolver (`vueResolver`, vue.ts:80-265).
pub struct VueResolver;

impl FrameworkResolver for VueResolver {
    fn name(&self) -> &str {
        "vue"
    }

    // Ports vueResolver.detect (vue.ts:83-101).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                if dep_present(&pkg, &["vue", "nuxt", "@nuxt/kit"]) {
                    return true;
                }
            }
        }
        context.get_all_files().iter().any(|f| f.ends_with(".vue"))
    }

    // Ports vueResolver.resolve (vue.ts:103-188).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = &reference.reference_name;

        // Pattern 1: compiler macros (vue.ts:104-112).
        if VUE_COMPILER_MACROS.contains(&name.as_str()) {
            return Some(self_resolved(reference, 1.0));
        }

        // Pattern 2: Nuxt auto-imported composables (vue.ts:114-122).
        if NUXT_AUTO_IMPORTS.contains(&name.as_str()) {
            return Some(self_resolved(reference, 1.0));
        }

        // Pattern 3: Nuxt virtual module imports (vue.ts:124-134).
        if reference.reference_kind == EdgeKind::Imports && name.starts_with('#') {
            if NUXT_VIRTUAL_MODULES.iter().any(|p| name.starts_with(p)) {
                return Some(self_resolved(reference, 1.0));
            }
        }

        // Pattern 4: `@/` alias imports (vue.ts:136-153).
        if reference.reference_kind == EdgeKind::Imports {
            if let Some(rest) = name.strip_prefix("@/") {
                if let Some(target) = resolve_alias(&format!("src/{rest}"), context) {
                    return Some(framework_resolved(reference, target, 0.9));
                }
            }
        }

        // Pattern 5: `~/` alias imports (vue.ts:155-172).
        if reference.reference_kind == EdgeKind::Imports {
            if let Some(rest) = name.strip_prefix("~/") {
                if let Some(target) = resolve_alias(&format!("src/{rest}"), context) {
                    return Some(framework_resolved(reference, target, 0.9));
                }
            }
        }

        // Pattern 6: PascalCase component refs from `calls` (vue.ts:174-185).
        if is_pascal_case(name) && reference.reference_kind == EdgeKind::Calls {
            if let Some(target) = resolve_component(name, &reference.file_path, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        None
    }

    // Ports vueResolver.extract (vue.ts:190-264).
    fn extract(
        &self,
        file_path: &str,
        _content: &str,
        _project_root: &str,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut nodes = Vec::new();
        let normalized = file_path.replace('\\', "/");

        // Nuxt page routes (pages/ directory) (vue.ts:197-216).
        if let Some(pages_index) = normalized.find("/pages/") {
            if normalized.ends_with(".vue") {
                let after_start = pages_index + "/pages/".len();
                if let Some(route_path) = file_path_to_nuxt_route(&normalized, after_start) {
                    nodes.push(framework_node(
                        format!("route:{file_path}:{route_path}:1"),
                        NodeKind::Route,
                        route_path.clone(),
                        format!("{file_path}::route:{route_path}"),
                        file_path.to_string(),
                        1,
                        1,
                        0,
                        0,
                        Language::Vue,
                        false,
                    ));
                }
            }
        }

        // Nuxt API routes (server/api/ directory) (vue.ts:218-240).
        if let Some(api_index) = normalized.find("/server/api/") {
            let after_api = &normalized[api_index + "/server/api/".len()..];
            let route_name = strip_trailing_index(&strip_extension(after_api));
            let api_route = format!("/api/{route_name}");
            let lang = if normalized.ends_with(".vue") {
                Language::Vue
            } else {
                Language::TypeScript
            };
            nodes.push(framework_node(
                format!("route:{file_path}:{api_route}:1"),
                NodeKind::Route,
                api_route.clone(),
                format!("{file_path}::route:{api_route}"),
                file_path.to_string(),
                1,
                1,
                0,
                0,
                lang,
                false,
            ));
        }

        // Nuxt middleware (middleware/ directory) (vue.ts:242-261).
        if let Some(mw_index) = normalized.find("/middleware/") {
            let after_mw = &normalized[mw_index + "/middleware/".len()..];
            let mw_name = strip_extension(after_mw);
            let lang = if normalized.ends_with(".vue") {
                Language::Vue
            } else {
                Language::TypeScript
            };
            nodes.push(framework_node(
                format!("middleware:{file_path}:{mw_name}:1"),
                NodeKind::Function,
                mw_name.to_string(),
                format!("{file_path}::middleware:{mw_name}"),
                file_path.to_string(),
                1,
                1,
                0,
                0,
                lang,
                false,
            ));
        }

        Some(FrameworkResolverExtractionResult {
            nodes,
            references: Vec::new(),
        })
    }
}

/// `{ ...dependencies, ...devDependencies }` membership test (vue.ts:89-90).
fn dep_present(pkg: &serde_json::Value, keys: &[&str]) -> bool {
    let in_section = |section: &str| -> bool {
        pkg.get(section)
            .and_then(|d| d.as_object())
            .is_some_and(|deps| keys.iter().any(|k| deps.contains_key(*k)))
    };
    in_section("dependencies") || in_section("devDependencies")
}

/// A ref that resolves to its own source node (compiler macros / auto-imports).
fn self_resolved(reference: &RefView, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: reference.from_node_id.clone(),
        confidence,
        resolved_by: ResolvedBy::Framework,
    }
}

fn framework_resolved(reference: &RefView, target_node_id: String, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id,
        confidence,
        resolved_by: ResolvedBy::Framework,
    }
}

/// Resolve an `@/` or `~/` alias path through the extension list to a node in
/// the matched file (vue.ts:138-152 / 157-171).
fn resolve_alias(alias_path: &str, context: &dyn ResolutionContext) -> Option<String> {
    const EXTS: [&str; 7] = [
        "",
        ".ts",
        ".js",
        ".vue",
        "/index.ts",
        "/index.js",
        "/index.vue",
    ];
    for ext in EXTS {
        let full_path = format!("{alias_path}{ext}");
        if context.file_exists(&full_path) {
            let nodes = context.get_nodes_in_file(&full_path);
            if !nodes.is_empty() {
                return Some(nodes[0].id.clone());
            }
        }
    }
    None
}

/// `isPascalCase` (vue.ts:270-272).
fn is_pascal_case(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z][a-zA-Z0-9]*$").expect("pascal"))
        .is_match(s)
}

/// `resolveComponent` (vue.ts:277-308).
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut matches: Vec<String> = Vec::new();
    for file in context.get_all_files() {
        if !file.ends_with(".vue") {
            continue;
        }
        let file_name = file.rsplit(['/', '\\']).next().unwrap_or(&file);
        if file_name.strip_suffix(".vue").unwrap_or(file_name) == name {
            matches.push(file);
        }
    }
    if matches.is_empty() {
        return None;
    }

    let component_in = |file: &str| -> Option<String> {
        context
            .get_nodes_in_file(file)
            .into_iter()
            .find(|n| n.kind == NodeKind::Component && n.name == name)
            .map(|n| n.id)
    };

    let from_dir = dir_of(from_file);
    let same_dir: Vec<&String> = matches
        .iter()
        .filter(|f| f.starts_with(&from_dir))
        .collect();
    if let Some(first) = same_dir.first() {
        return component_in(first);
    }

    // Only an unambiguous basename may resolve (vue.ts:307).
    if matches.len() == 1 {
        component_in(&matches[0])
    } else {
        None
    }
}

/// `filePathToNuxtRoute` (vue.ts:313-331).
fn file_path_to_nuxt_route(normalized: &str, after_pages_start: usize) -> Option<String> {
    let after_pages = &normalized[after_pages_start..];
    // Remove .vue extension.
    let without_ext = after_pages.strip_suffix(".vue").unwrap_or(after_pages);
    // Remove /index suffix.
    let without_index = without_ext.strip_suffix("/index").unwrap_or(without_ext);

    // [...slug] -> *slug, [[optional]] -> :optional?, [param] -> :param.
    let step1 = catch_all_regex()
        .replace_all(without_index, "*$1")
        .into_owned();
    let step2 = optional_regex().replace_all(&step1, ":$1?").into_owned();
    let step3 = param_regex().replace_all(&step2, ":$1").into_owned();
    let route = format!("/{step3}");

    if route == "/" {
        return Some("/".to_string());
    }
    // Remove trailing slash.
    Some(route.strip_suffix('/').unwrap_or(&route).to_string())
}

/// `replace(/\.[^/.]+$/, '')` — remove the final extension only.
fn strip_extension(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.[^/.]+$").expect("ext"))
        .replace(s, "")
        .into_owned()
}

/// `replace(/\/index$/, '')`.
fn strip_trailing_index(s: &str) -> String {
    s.strip_suffix("/index").unwrap_or(s).to_string()
}

fn dir_of(file: &str) -> String {
    match file.rfind('/') {
        Some(idx) => file[..idx].to_string(),
        None => String::new(),
    }
}

fn catch_all_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\.\.\.([^\]]+)\]").expect("catch all"))
}

fn optional_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([^\]]+)\]\]").expect("optional"))
}

fn param_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]").expect("param"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::Node;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Ctx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
    }

    impl Ctx {
        fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self
        }
        fn node(mut self, n: Node) -> Self {
            self.nodes.push(n);
            self
        }
    }

    impl ResolutionContext for Ctx {
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
        fn get_nodes_by_qualified_name(&self, q: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == q)
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
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, _f: &str, _l: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn mk_node(id: &str, kind: NodeKind, name: &str, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language: Language::Vue,
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

    fn a_ref(name: &str, kind: EdgeKind, file: &str) -> RefView {
        RefView {
            from_node_id: format!("from:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language: Language::Vue,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    // -- pure helpers -------------------------------------------------------

    #[test]
    fn is_pascal_case_units() {
        assert!(is_pascal_case("MyComp"));
        assert!(!is_pascal_case("myComp"));
        assert!(!is_pascal_case(""));
    }

    #[test]
    fn strip_extension_and_index_and_dir() {
        assert_eq!(strip_extension("foo/bar.ts"), "foo/bar");
        assert_eq!(strip_extension("no_ext"), "no_ext");
        assert_eq!(strip_trailing_index("users/index"), "users");
        assert_eq!(strip_trailing_index("users"), "users");
        assert_eq!(dir_of("a/b/c.vue"), "a/b");
        assert_eq!(dir_of("top.vue"), "");
    }

    // -- detect -------------------------------------------------------------

    #[test]
    fn detect_via_nuxt_dep_and_dev_dep() {
        let ctx = Ctx::default().file("package.json", r#"{"devDependencies":{"nuxt":"3"}}"#);
        assert!(VueResolver.detect(&ctx));
        let ctx2 = Ctx::default().file("package.json", r#"{"dependencies":{"@nuxt/kit":"3"}}"#);
        assert!(VueResolver.detect(&ctx2));
    }

    #[test]
    fn detect_malformed_json_then_file_scan() {
        let ctx = Ctx::default()
            .file("package.json", "broken {")
            .file("src/A.vue", "<template/>");
        assert!(VueResolver.detect(&ctx));
    }

    #[test]
    fn detect_false_when_no_signal() {
        let ctx = Ctx::default().file("src/index.ts", "export const x=1;");
        assert!(!VueResolver.detect(&ctx));
    }

    // -- resolve: compiler macro / auto-import / virtual module -------------

    #[test]
    fn resolve_nuxt_auto_import_self() {
        let reference = a_ref("useFetch", EdgeKind::Calls, "src/App.vue");
        let r = VueResolver
            .resolve(&reference, &Ctx::default())
            .expect("resolves");
        assert_eq!(r.target_node_id, reference.from_node_id);
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn resolve_nuxt_virtual_module_import() {
        let reference = a_ref("#imports", EdgeKind::Imports, "src/App.vue");
        let r = VueResolver
            .resolve(&reference, &Ctx::default())
            .expect("resolves");
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn resolve_hash_import_unknown_virtual_module_none() {
        // Starts with '#' but is not a known virtual module prefix.
        let reference = a_ref("#unknown", EdgeKind::Imports, "src/App.vue");
        assert!(VueResolver.resolve(&reference, &Ctx::default()).is_none());
    }

    // -- resolve: @/ and ~/ alias imports -----------------------------------

    #[test]
    fn resolve_at_alias_import_resolves_to_node() {
        let target = mk_node(
            "var:src/utils/x.ts:x:1",
            NodeKind::Variable,
            "x",
            "src/utils/x.ts",
        );
        let ctx = Ctx::default()
            .file("src/utils/x.ts", "export const x=1;")
            .node(target.clone());
        let reference = a_ref("@/utils/x", EdgeKind::Imports, "src/App.vue");
        let r = VueResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, target.id);
        assert_eq!(r.confidence, 0.9);
    }

    #[test]
    fn resolve_tilde_alias_import_via_index_ext() {
        // The `~/` alias resolves through the /index.ts extension candidate.
        let target = mk_node(
            "var:src/lib/index.ts:y:1",
            NodeKind::Variable,
            "y",
            "src/lib/index.ts",
        );
        let ctx = Ctx::default()
            .file("src/lib/index.ts", "export const y=1;")
            .node(target.clone());
        let reference = a_ref("~/lib", EdgeKind::Imports, "src/App.vue");
        let r = VueResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, target.id);
    }

    #[test]
    fn resolve_alias_import_missing_file_none() {
        let ctx = Ctx::default();
        let reference = a_ref("@/nope", EdgeKind::Imports, "src/App.vue");
        assert!(VueResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_alias_file_exists_but_no_nodes_none() {
        // File exists across all ext candidates but has no nodes -> None.
        let ctx = Ctx::default().file("src/empty.ts", "// nothing");
        let reference = a_ref("@/empty", EdgeKind::Imports, "src/App.vue");
        assert!(VueResolver.resolve(&reference, &ctx).is_none());
    }

    // -- resolve: component call --------------------------------------------

    #[test]
    fn resolve_component_ambiguous_returns_none() {
        // Two Button.vue files in different dirs, neither in the caller's dir.
        let a = mk_node(
            "component:x/Button.vue:Button:1",
            NodeKind::Component,
            "Button",
            "x/Button.vue",
        );
        let b = mk_node(
            "component:y/Button.vue:Button:1",
            NodeKind::Component,
            "Button",
            "y/Button.vue",
        );
        let ctx = Ctx::default()
            .file("x/Button.vue", "<template/>")
            .file("y/Button.vue", "<template/>")
            .node(a)
            .node(b);
        let reference = a_ref("Button", EdgeKind::Calls, "z/Page.vue");
        assert!(VueResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_component_non_calls_kind_skipped() {
        // PascalCase but reference_kind is References, not Calls -> pattern 6 skips.
        let comp = mk_node(
            "component:src/Button.vue:Button:1",
            NodeKind::Component,
            "Button",
            "src/Button.vue",
        );
        let ctx = Ctx::default()
            .file("src/Button.vue", "<template/>")
            .node(comp);
        let reference = a_ref("Button", EdgeKind::References, "src/Page.vue");
        assert!(VueResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_component_no_matching_vue_file_none() {
        let reference = a_ref("Missing", EdgeKind::Calls, "src/Page.vue");
        assert!(VueResolver.resolve(&reference, &Ctx::default()).is_none());
    }

    // -- extract: nuxt route / api / middleware -----------------------------

    #[test]
    fn extract_nuxt_catch_all_route() {
        let result = VueResolver
            .extract("app/pages/[...slug].vue", "", "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/*slug");
    }

    #[test]
    fn extract_nuxt_optional_param_route() {
        let result = VueResolver
            .extract("app/pages/[[maybe]].vue", "", "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/:maybe?");
    }

    #[test]
    fn extract_nuxt_nested_index_route_strips_index() {
        // `pages/users/index.vue` -> after_pages "users/index" -> strip /index -> "/users".
        let result = VueResolver
            .extract("app/pages/users/index.vue", "", "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/users");
    }

    #[test]
    fn extract_api_route_strips_index_and_ext() {
        // The matcher keys on a leading-slash "/server/api/", so the file needs a
        // parent segment (app/server/api/...).
        let result = VueResolver
            .extract("app/server/api/users/index.ts", "", "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/api/users");
        assert_eq!(route.language, Language::TypeScript);
    }

    #[test]
    fn extract_middleware_node() {
        // Leading-slash "/middleware/" match requires a parent segment.
        let result = VueResolver
            .extract("app/middleware/auth.ts", "", "")
            .expect("extract");
        let mw = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .expect("middleware");
        assert_eq!(mw.name, "auth");
        assert_eq!(mw.language, Language::TypeScript);
    }

    #[test]
    fn extract_non_route_file_yields_no_nodes() {
        let result = VueResolver
            .extract("src/plain.ts", "", "")
            .expect("extract");
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn extract_backslash_paths_normalized() {
        // Windows-style separators normalize so /pages/ still matches.
        let result = VueResolver
            .extract("app\\pages\\about.vue", "", "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/about");
    }
}
