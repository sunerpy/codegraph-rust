//! React / Next.js [`FrameworkResolver`] — ports
//! `upstream resolution/frameworks/react.ts`.

use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Language, NodeKind};
use regex::Regex;

use super::{framework_node, js_language_for};
use crate::framework::FrameworkResolver;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// React / Next.js resolver (`reactResolver`, react.ts:10-269).
pub struct ReactResolver;

impl FrameworkResolver for ReactResolver {
    fn name(&self) -> &str {
        "react"
    }

    fn languages(&self) -> Option<&[Language]> {
        // `languages: ['javascript', 'typescript']` (react.ts:12).
        const LANGS: [Language; 2] = [Language::JavaScript, Language::TypeScript];
        Some(&LANGS)
    }

    // Ports reactResolver.detect (react.ts:14-32).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                if dep_present(&pkg, &["react", "next", "react-native"]) {
                    return true;
                }
            }
        }
        context
            .get_all_files()
            .iter()
            .any(|f| f.ends_with(".jsx") || f.ends_with(".tsx"))
    }

    // Ports reactResolver.resolve (react.ts:34-86).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = &reference.reference_name;

        // Pattern 1: PascalCase component refs, only from JSX-capable files
        // (react.ts:43-57).
        if matches!(reference.language, Language::Tsx | Language::Jsx)
            && is_pascal_case(name)
            && !is_built_in_type(name)
        {
            if let Some(target) = resolve_component(name, &reference.file_path, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        // Pattern 2: `use*` hooks (react.ts:59-70).
        if name.starts_with("use") && name.len() > 3 {
            if let Some(target) = resolve_hook(name, context) {
                return Some(framework_resolved(reference, target, 0.85));
            }
        }

        // Pattern 3: `*Context` / `*Provider` (react.ts:72-83).
        if name.ends_with("Context") || name.ends_with("Provider") {
            if let Some(target) = resolve_context(name, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        None
    }

    // Ports reactResolver.extract (react.ts:88-268).
    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkResolverExtractionResult> {
        let mut nodes = Vec::new();
        let mut references = Vec::new();
        let jsx_lang = js_language_for(file_path);

        // Component definitions (react.ts:95-133).
        for pattern in component_patterns() {
            for m in pattern.captures_iter(content) {
                let full = m.get(0).expect("group 0");
                let Some(name) = m.get(1) else { continue };
                let full_match = full.as_str();
                let line = line_of(content, full.start());

                let after_start = full.end();
                let after_end = (after_start + 500).min(content.len());
                let after = &content[after_start..after_end];
                let has_jsx = after.contains('<') && (after.contains("/>") || after.contains("</"));
                if !has_jsx {
                    continue;
                }
                let is_exported = full_match.contains("export");
                // Component nodes are tsx/jsx (react.ts:127).
                let lang = if file_path.ends_with(".tsx") {
                    Language::Tsx
                } else {
                    Language::Jsx
                };
                nodes.push(framework_node(
                    format!("component:{file_path}:{}:{line}", name.as_str()),
                    NodeKind::Component,
                    name.as_str().to_string(),
                    format!("{file_path}::{}", name.as_str()),
                    file_path.to_string(),
                    line,
                    line,
                    0,
                    full_match.len() as i64,
                    lang,
                    is_exported,
                ));
            }
        }

        // Custom hooks (react.ts:135-156).
        for m in hook_pattern().captures_iter(content) {
            let full = m.get(0).expect("group 0");
            let Some(name) = m.get(1) else { continue };
            let full_match = full.as_str();
            let line = line_of(content, full.start());
            let is_exported = full_match.contains("export");
            // Hook nodes are typescript/javascript (react.ts:152).
            let lang = if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
                Language::TypeScript
            } else {
                Language::JavaScript
            };
            nodes.push(framework_node(
                format!("hook:{file_path}:{}:{line}", name.as_str()),
                NodeKind::Function,
                name.as_str().to_string(),
                format!("{file_path}::{}", name.as_str()),
                file_path.to_string(),
                line,
                line,
                0,
                full_match.len() as i64,
                lang,
                is_exported,
            ));
        }

        // React Router <Route .../> (v5/v6) (react.ts:158-198).
        for tag in route_tag_regex().find_iter(content) {
            let window_end = (tag.start() + 400).min(content.len());
            let window = &content[tag.start()..window_end];
            let Some(path_match) = route_path_attr().captures(window) else {
                continue;
            };
            let route_path = path_match.get(1).expect("path group").as_str();
            let comp = route_component_attr()
                .captures(window)
                .or_else(|| route_element_attr().captures(window))
                .map(|c| c.get(1).expect("comp group").as_str().to_string());
            let line = line_of(content, tag.start());
            let node = framework_node(
                format!("route:{file_path}:{line}:{route_path}"),
                NodeKind::Route,
                route_path.to_string(),
                format!("{file_path}::route:{route_path}"),
                file_path.to_string(),
                line,
                line,
                0,
                0,
                jsx_lang,
                false,
            );
            let node_id = node.id.clone();
            nodes.push(node);
            if let Some(comp) = comp {
                references.push(route_reference(node_id, comp, line, file_path, jsx_lang));
            }
        }

        // React Router data-router (v6.4+) (react.ts:200-239).
        if data_router_regex().is_match(content) {
            for m in obj_path_regex().captures_iter(content) {
                let whole = m.get(0).expect("group 0");
                let win_end = (whole.start() + 300).min(content.len());
                let win = &content[whole.start()..win_end];
                let comp = obj_element_attr()
                    .captures(win)
                    .or_else(|| obj_component_attr().captures(win))
                    .map(|c| c.get(1).expect("comp group").as_str().to_string());
                let Some(comp) = comp else { continue };
                let route_path = {
                    let p = m.get(1).expect("path group").as_str();
                    if p.is_empty() {
                        "/".to_string()
                    } else {
                        p.to_string()
                    }
                };
                let line = line_of(content, whole.start());
                let node = framework_node(
                    format!("route:{file_path}:{line}:{route_path}"),
                    NodeKind::Route,
                    route_path.clone(),
                    format!("{file_path}::route:{route_path}"),
                    file_path.to_string(),
                    line,
                    line,
                    0,
                    0,
                    jsx_lang,
                    false,
                );
                let node_id = node.id.clone();
                nodes.push(node);
                references.push(route_reference(node_id, comp, line, file_path, jsx_lang));
            }
        }

        // Next.js pages/app routes (react.ts:241-265).
        if file_path.contains("pages/") || file_path.contains("app/") {
            if content.contains("export default") {
                if let Some(route_path) = file_path_to_route(file_path) {
                    let byte = content.find("export default").expect("contains check");
                    let line_num = line_of(content, byte);
                    // Route node language is tsx/typescript/javascript (react.ts:260).
                    let lang = if file_path.ends_with(".tsx") {
                        Language::Tsx
                    } else if file_path.ends_with(".ts") {
                        Language::TypeScript
                    } else {
                        Language::JavaScript
                    };
                    nodes.push(framework_node(
                        format!("route:{file_path}:{route_path}:{line_num}"),
                        NodeKind::Route,
                        route_path.clone(),
                        format!("{file_path}::route:{route_path}"),
                        file_path.to_string(),
                        line_num,
                        line_num,
                        0,
                        0,
                        lang,
                        false,
                    ));
                }
            }
        }

        Some(FrameworkResolverExtractionResult { nodes, references })
    }
}

/// `{ ...dependencies, ...devDependencies }` membership test (react.ts:20-21).
fn dep_present(pkg: &serde_json::Value, keys: &[&str]) -> bool {
    let in_section = |section: &str| -> bool {
        pkg.get(section)
            .and_then(|d| d.as_object())
            .is_some_and(|deps| keys.iter().any(|k| deps.contains_key(*k)))
    };
    in_section("dependencies") || in_section("devDependencies")
}

/// Build a `references` ref linking a route node to its handler component
/// (react.ts:188-196 / 229-237).
fn route_reference(
    from_node_id: String,
    name: String,
    line: i64,
    file_path: &str,
    language: Language,
) -> RefView {
    RefView {
        from_node_id,
        reference_name: name,
        reference_kind: EdgeKind::References,
        line,
        column: 0,
        file_path: file_path.to_string(),
        language,
        is_function_ref: false,
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

/// `isPascalCase` (react.ts:274-276).
fn is_pascal_case(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z][a-zA-Z0-9]*$").expect("pascal"))
        .is_match(s)
}

/// `isBuiltInType` / `BUILT_IN_TYPES` (react.ts:281-289).
fn is_built_in_type(name: &str) -> bool {
    const BUILT_IN_TYPES: [&str; 22] = [
        "Array",
        "Boolean",
        "Date",
        "Error",
        "Function",
        "JSON",
        "Math",
        "Number",
        "Object",
        "Promise",
        "RegExp",
        "String",
        "Symbol",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "React",
        "Component",
        "Fragment",
        "Suspense",
        "StrictMode",
    ];
    BUILT_IN_TYPES.contains(&name)
}

/// `COMPONENT_KINDS` (react.ts:291).
fn is_component_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Component | NodeKind::Function | NodeKind::Class
    )
}

/// `resolveComponent` (react.ts:296-323).
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }
    let components: Vec<_> = candidates
        .into_iter()
        .filter(|n| is_component_kind(n.kind))
        .collect();
    if components.is_empty() {
        return None;
    }

    let from_dir = dir_of(from_file);
    if let Some(same_dir) = components
        .iter()
        .find(|n| n.file_path.starts_with(&from_dir))
    {
        return Some(same_dir.id.clone());
    }

    const COMPONENT_DIRS: [&str; 7] = [
        "/components/",
        "/src/components/",
        "/app/components/",
        "/pages/",
        "/src/pages/",
        "/views/",
        "/src/views/",
    ];
    if let Some(preferred) = components
        .iter()
        .find(|n| COMPONENT_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(preferred.id.clone());
    }

    // Only an unambiguous single may resolve (react.ts:322).
    if components.len() == 1 {
        Some(components[0].id.clone())
    } else {
        None
    }
}

/// `resolveHook` (react.ts:328-343).
fn resolve_hook(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }
    let hooks: Vec<_> = candidates
        .into_iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("use"))
        .collect();
    if hooks.is_empty() {
        return None;
    }
    const HOOK_DIRS: [&str; 4] = ["/hooks/", "/src/hooks/", "/lib/hooks/", "/utils/hooks/"];
    if let Some(preferred) = hooks
        .iter()
        .find(|n| HOOK_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(preferred.id.clone());
    }
    Some(hooks[0].id.clone())
}

/// `resolveContext` (react.ts:348-368).
fn resolve_context(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        // Strip Context/Provider suffix fallback (react.ts:351-357).
        let base_name = strip_context_suffix(name);
        if base_name != name {
            let base_candidates = context.get_nodes_by_name(base_name);
            if !base_candidates.is_empty() {
                return Some(base_candidates[0].id.clone());
            }
        }
        return None;
    }
    const CONTEXT_DIRS: [&str; 6] = [
        "/context/",
        "/contexts/",
        "/src/context/",
        "/src/contexts/",
        "/providers/",
        "/src/providers/",
    ];
    if let Some(preferred) = candidates
        .iter()
        .find(|n| CONTEXT_DIRS.iter().any(|d| n.file_path.contains(d)))
    {
        return Some(preferred.id.clone());
    }
    Some(candidates[0].id.clone())
}

/// `name.replace(/Context$|Provider$/, '')` (react.ts:352).
fn strip_context_suffix(name: &str) -> &str {
    if let Some(stripped) = name.strip_suffix("Context") {
        stripped
    } else if let Some(stripped) = name.strip_suffix("Provider") {
        stripped
    } else {
        name
    }
}

/// `filePathToRoute` (react.ts:373-417).
fn file_path_to_route(file_path: &str) -> Option<String> {
    let base = file_path.rsplit('/').next().unwrap_or(file_path);
    if !page_ext_regex().is_match(base) {
        return None;
    }
    if base.starts_with('_') || config_file_regex().is_match(base) {
        return None;
    }

    if pages_segment_regex().is_match(file_path) {
        // replace(/^.*pages\//, '/')
        let after = pages_prefix_regex().replace(file_path, "/");
        // replace(/\/index\.(tsx?|jsx?)$/, '')
        let after = index_ext_regex().replace(&after, "");
        // replace(/\.(tsx?|jsx?)$/, '')
        let after = trailing_ext_regex().replace(&after, "");
        // replace(/\[([^\]]+)\]/g, ':$1')
        let route = param_regex().replace_all(&after, ":$1").into_owned();
        return Some(if route.is_empty() {
            "/".to_string()
        } else {
            route
        });
    }

    if app_segment_regex().is_match(file_path) {
        if !file_path.contains("page.") {
            return None;
        }
        // replace(/^.*app\//, '/')
        let after = app_prefix_regex().replace(file_path, "/");
        // replace(/\/page\.(tsx?|jsx?)$/, '')
        let after = page_ext_strip_regex().replace(&after, "");
        let route = param_regex().replace_all(&after, ":$1").into_owned();
        return Some(if route.is_empty() {
            "/".to_string()
        } else {
            route
        });
    }

    None
}

fn dir_of(file: &str) -> String {
    match file.rfind('/') {
        Some(idx) => file[..idx].to_string(),
        None => String::new(),
    }
}

/// 1-based line number of `byte_index` (`slice(0, idx).split('\n').length`).
fn line_of(content: &str, byte_index: usize) -> i64 {
    content[..byte_index].matches('\n').count() as i64 + 1
}

fn component_patterns() -> &'static [Regex] {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            // function components (react.ts:97).
            Regex::new(r"(?:export\s+)?function\s+([A-Z][a-zA-Z0-9]*)\s*\(").expect("fn comp"),
            // arrow function components (react.ts:99).
            Regex::new(
                r"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:\([^)]*\)|[a-zA-Z_][a-zA-Z0-9_]*)\s*=>",
            )
            .expect("arrow comp"),
            // forwardRef components (react.ts:101).
            Regex::new(
                r"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:React\.)?forwardRef",
            )
            .expect("forwardRef comp"),
            // memo components (react.ts:103).
            Regex::new(r"(?:export\s+)?(?:const|let)\s+([A-Z][a-zA-Z0-9]*)\s*=\s*(?:React\.)?memo")
                .expect("memo comp"),
        ]
    })
}

fn hook_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:export\s+)?(?:function|const|let)\s+(use[A-Z][a-zA-Z0-9]*)\s*[=(]")
            .expect("hook")
    })
}

fn route_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<Route\b").expect("route tag"))
}

fn route_path_attr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\bpath\s*=\s*["']([^"']+)["']"#).expect("route path"))
}

fn route_component_attr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\bcomponent\s*=\s*\{\s*([A-Z][A-Za-z0-9_]*)").expect("route comp")
    })
}

fn route_element_attr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\belement\s*=\s*\{\s*<\s*([A-Z][A-Za-z0-9_]*)").expect("route element")
    })
}

fn data_router_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b(?:createBrowserRouter|createHashRouter|createMemoryRouter|createRoutesFromElements)\b",
        )
        .expect("data router")
    })
}

fn obj_path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\bpath\s*:\s*['"]([^'"]*)['"]"#).expect("obj path"))
}

fn obj_element_attr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\belement\s*:\s*<\s*([A-Z][A-Za-z0-9_]*)").expect("obj element"))
}

fn obj_component_attr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bComponent\s*:\s*([A-Z][A-Za-z0-9_]*)").expect("obj component"))
}

fn page_ext_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(tsx?|jsx?)$").expect("page ext"))
}

fn config_file_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.config\.[a-z]+$").expect("config file"))
}

fn pages_segment_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)pages/").expect("pages segment"))
}

fn pages_prefix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^.*pages/").expect("pages prefix"))
}

fn index_ext_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/index\.(tsx?|jsx?)$").expect("index ext"))
}

fn trailing_ext_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(tsx?|jsx?)$").expect("trailing ext"))
}

fn param_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]").expect("param"))
}

fn app_segment_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)app/").expect("app segment"))
}

fn app_prefix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^.*app/").expect("app prefix"))
}

fn page_ext_strip_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/page\.(tsx?|jsx?)$").expect("page strip"))
}
