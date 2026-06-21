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
