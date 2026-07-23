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
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _project_root: &str,
    ) -> Option<FrameworkResolverExtractionResult> {
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
                let after = byte_window(content, after_start, 500);
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
            let window = byte_window(content, tag.start(), 400);
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
                let win = byte_window(content, whole.start(), 300);
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
        reference_subkind: None,
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

fn byte_window(content: &str, start: usize, max_bytes: usize) -> &str {
    let mut end = start.saturating_add(max_bytes).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[start..end]
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

    fn mk_node(id: &str, kind: NodeKind, name: &str, file: &str, lang: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
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

    fn a_ref(name: &str, kind: EdgeKind, file: &str, lang: Language) -> RefView {
        RefView {
            from_node_id: format!("from:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    // -- pure-helper units --------------------------------------------------

    #[test]
    fn is_pascal_case_matches_and_rejects() {
        assert!(is_pascal_case("Button"));
        assert!(is_pascal_case("A"));
        assert!(!is_pascal_case("button"));
        assert!(!is_pascal_case("_X"));
        assert!(!is_pascal_case(""));
    }

    #[test]
    fn is_built_in_type_flags_known_and_passes_others() {
        assert!(is_built_in_type("React"));
        assert!(is_built_in_type("Fragment"));
        assert!(!is_built_in_type("Button"));
    }

    #[test]
    fn is_component_kind_covers_variants() {
        assert!(is_component_kind(NodeKind::Component));
        assert!(is_component_kind(NodeKind::Function));
        assert!(is_component_kind(NodeKind::Class));
        assert!(!is_component_kind(NodeKind::Variable));
    }

    #[test]
    fn dir_of_and_line_of() {
        assert_eq!(dir_of("src/a/b.tsx"), "src/a");
        assert_eq!(dir_of("top.tsx"), "");
        assert_eq!(line_of("a\nb\nc", 4), 3);
        assert_eq!(line_of("abc", 0), 1);
    }

    #[test]
    fn strip_context_suffix_cases() {
        assert_eq!(strip_context_suffix("AuthContext"), "Auth");
        assert_eq!(strip_context_suffix("ThemeProvider"), "Theme");
        assert_eq!(strip_context_suffix("Plain"), "Plain");
    }

    // -- dep_present via detect --------------------------------------------

    #[test]
    fn detect_via_dev_dependency() {
        let ctx = Ctx::default().file("package.json", r#"{"devDependencies":{"next":"14"}}"#);
        assert!(ReactResolver.detect(&ctx));
    }

    #[test]
    fn detect_via_react_native_dependency() {
        let ctx = Ctx::default().file("package.json", r#"{"dependencies":{"react-native":"0.7"}}"#);
        assert!(ReactResolver.detect(&ctx));
    }

    #[test]
    fn detect_malformed_package_json_falls_through_to_file_scan() {
        // Unparseable package.json is ignored; a .jsx file still triggers detection.
        let ctx = Ctx::default()
            .file("package.json", "not json {{{")
            .file("src/App.jsx", "export default function App(){}");
        assert!(ReactResolver.detect(&ctx));
    }

    #[test]
    fn detect_false_when_no_signal() {
        let ctx = Ctx::default().file("src/index.ts", "export const x = 1;");
        assert!(!ReactResolver.detect(&ctx));
    }

    // -- resolve: hook directory preference --------------------------------

    #[test]
    fn resolve_hook_falls_back_to_first_when_no_hooks_dir() {
        // No hooks/ directory, so the first candidate wins (react.ts:342).
        let hook = mk_node(
            "fn:src/util.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/util.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().node(hook.clone());
        let reference = a_ref("useThing", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, hook.id);
        assert_eq!(r.confidence, 0.85);
    }

    #[test]
    fn resolve_hook_none_when_not_a_use_function() {
        // Candidate exists but is not a `use*` function -> no hook resolution.
        let notf = mk_node(
            "class:src/useThing.ts:useThing:1",
            NodeKind::Class,
            "useThing",
            "src/useThing.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().node(notf);
        let reference = a_ref("useThing", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_hook_short_use_name_skipped() {
        // "use" alone (len == 3) is not a hook name.
        let ctx = Ctx::default();
        let reference = a_ref("use", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_context_strips_suffix_when_no_direct_match() {
        // No `AuthProvider` node, but a base `Auth` node exists -> suffix strip
        // fallback resolves to it (react.ts:351-357).
        let base = mk_node(
            "var:src/Auth.tsx:Auth:1",
            NodeKind::Variable,
            "Auth",
            "src/Auth.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(base.clone());
        let reference = a_ref(
            "AuthProvider",
            EdgeKind::References,
            "src/App.tsx",
            Language::Tsx,
        );
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, base.id);
    }

    #[test]
    fn resolve_context_prefers_context_dir() {
        let far = mk_node(
            "var:src/other/AuthContext.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/other/AuthContext.tsx",
            Language::Tsx,
        );
        let near = mk_node(
            "var:src/context/AuthContext.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/context/AuthContext.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(far).node(near.clone());
        let reference = a_ref(
            "AuthContext",
            EdgeKind::References,
            "src/App.tsx",
            Language::Tsx,
        );
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, near.id);
    }

    #[test]
    fn resolve_context_none_when_no_candidate_and_no_base() {
        let ctx = Ctx::default();
        let reference = a_ref(
            "MissingProvider",
            EdgeKind::References,
            "src/App.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_component_prefers_component_dir_over_ambiguous() {
        // Two candidates in unrelated dirs, one under /components/ -> that wins.
        let a = mk_node(
            "component:src/a/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/a/Button.tsx",
            Language::Tsx,
        );
        let b = mk_node(
            "component:src/components/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/components/Button.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(a).node(b.clone());
        let reference = a_ref(
            "Button",
            EdgeKind::References,
            "src/z/Page.tsx",
            Language::Tsx,
        );
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, b.id);
    }

    #[test]
    fn resolve_component_ambiguous_multiple_returns_none() {
        // Two non-preferred candidates in different dirs -> ambiguous, None.
        let a = mk_node(
            "component:src/a/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/a/Button.tsx",
            Language::Tsx,
        );
        let b = mk_node(
            "component:src/b/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/b/Button.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(a).node(b);
        let reference = a_ref(
            "Button",
            EdgeKind::References,
            "src/z/Page.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_component_none_when_no_component_kind() {
        let v = mk_node(
            "var:src/Button.tsx:Button:1",
            NodeKind::Variable,
            "Button",
            "src/Button.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(v);
        let reference = a_ref(
            "Button",
            EdgeKind::References,
            "src/Page.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_built_in_type_component_skipped() {
        // PascalCase but a built-in type -> pattern 1 does not fire.
        let ctx = Ctx::default();
        let reference = a_ref("React", EdgeKind::References, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    // -- extract: component variants + JSX gate -----------------------------

    #[test]
    fn extract_arrow_component_with_jsx() {
        let content = "export const Card = () => { return <div/>; };";
        let result = ReactResolver
            .extract("src/Card.jsx", content, "")
            .expect("extract");
        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Component && n.name == "Card")
        );
    }

    #[test]
    fn extract_component_handles_utf8_at_lookahead_boundary() {
        let mut content = String::from("function Card(");
        content.push_str("<div/>");
        content.push_str(&"a".repeat(493));
        content.push('é');

        let result = ReactResolver
            .extract("src/Card.tsx", &content, "")
            .expect("extract");

        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Component && n.name == "Card")
        );
    }

    #[test]
    fn extract_component_without_jsx_is_skipped() {
        // A PascalCase function that returns no JSX must NOT be a component node.
        let content = "export function Helper() { return 42; }";
        let result = ReactResolver
            .extract("src/Helper.tsx", content, "")
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Component));
    }

    #[test]
    fn extract_forward_ref_and_memo_components() {
        let content =
            "const Fancy = React.forwardRef(() => <div/>);\nconst Wrapped = memo(() => <span/>);";
        let result = ReactResolver
            .extract("src/W.tsx", content, "")
            .expect("extract");
        let names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Component)
            .map(|n| n.name.as_str())
            .collect();
        assert!(names.contains(&"Fancy"), "got {names:?}");
        assert!(names.contains(&"Wrapped"), "got {names:?}");
    }

    #[test]
    fn extract_custom_hook_node() {
        let content = "export function useCounter() { return 0; }";
        let result = ReactResolver
            .extract("src/useCounter.ts", content, "")
            .expect("extract");
        let hook = result
            .nodes
            .iter()
            .find(|n| n.name == "useCounter")
            .expect("hook node");
        assert_eq!(hook.kind, NodeKind::Function);
        assert_eq!(hook.language, Language::TypeScript);
        assert!(hook.is_exported);
    }

    #[test]
    fn extract_hook_js_language_for_plain_js() {
        let content = "const useX = () => 1;";
        let result = ReactResolver
            .extract("src/useX.js", content, "")
            .expect("extract");
        let hook = result
            .nodes
            .iter()
            .find(|n| n.name == "useX")
            .expect("hook");
        assert_eq!(hook.language, Language::JavaScript);
    }

    #[test]
    fn extract_route_element_attribute_variant() {
        // <Route ... element={<Home/>}/> uses the element attr branch.
        let content = "<Route path=\"/x\" element={<Home/>}/>";
        let result = ReactResolver
            .extract("src/App.tsx", content, "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/x");
        assert!(result.references.iter().any(|r| r.reference_name == "Home"));
    }

    #[test]
    fn extract_route_handles_utf8_at_lookahead_boundary() {
        let mut content = String::from("<Route path=\"/utf8\" element={<Home/>} ");
        content.push_str(&"a".repeat(399 - content.len()));
        content.push('é');

        let result = ReactResolver
            .extract("src/App.tsx", &content, "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");

        assert_eq!(route.name, "/utf8");
        assert!(result.references.iter().any(|r| r.reference_name == "Home"));
    }

    #[test]
    fn extract_route_without_path_is_skipped() {
        let content = "<Route element={<Home/>}/>";
        let result = ReactResolver
            .extract("src/App.tsx", content, "")
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn extract_data_router_object_routes() {
        let content =
            "const r = createBrowserRouter([\n  { path: '/dash', element: <Dashboard/> },\n]);";
        let result = ReactResolver
            .extract("src/routes.tsx", content, "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/dash");
        assert!(
            result
                .references
                .iter()
                .any(|r| r.reference_name == "Dashboard")
        );
    }

    #[test]
    fn extract_data_router_handles_utf8_at_lookahead_boundary() {
        let mut content =
            String::from("const routes = createBrowserRouter([{ path: '/utf8', element: <Home/> ");
        let path_start = content.find("path:").expect("path attribute");
        let utf8_start = path_start + 299;
        content.push_str(&"a".repeat(utf8_start - content.len()));
        content.push('é');

        let result = ReactResolver
            .extract("src/routes.tsx", &content, "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");

        assert_eq!(route.name, "/utf8");
        assert!(result.references.iter().any(|r| r.reference_name == "Home"));
    }

    #[test]
    fn extract_data_router_empty_path_becomes_root() {
        let content = "createBrowserRouter([{ path: '', Component: Index }]);";
        let result = ReactResolver
            .extract("src/routes.tsx", content, "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/");
    }

    #[test]
    fn extract_data_router_path_without_component_skipped() {
        let content = "createBrowserRouter([{ path: '/only' }]);";
        let result = ReactResolver
            .extract("src/routes.tsx", content, "")
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn extract_nextjs_app_page_route() {
        let content = "export default function Page() { return <div/>; }";
        let result = ReactResolver
            .extract("app/blog/page.tsx", content, "")
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "/blog");
    }

    #[test]
    fn extract_nextjs_without_export_default_no_route() {
        let content = "export function NotDefault() { return <div/>; }";
        let result = ReactResolver
            .extract("pages/x.tsx", content, "")
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    // -- file_path_to_route pure branches -----------------------------------

    #[test]
    fn file_path_to_route_pages_index_and_param() {
        assert_eq!(file_path_to_route("pages/index.tsx").as_deref(), Some("/"));
        assert_eq!(
            file_path_to_route("pages/users/[id].tsx").as_deref(),
            Some("/users/:id")
        );
    }

    #[test]
    fn file_path_to_route_app_page_and_missing_page() {
        assert_eq!(
            file_path_to_route("app/settings/page.tsx").as_deref(),
            Some("/settings")
        );
        // app/ path but not a page.* file -> None.
        assert_eq!(file_path_to_route("app/settings/layout.tsx"), None);
    }

    #[test]
    fn file_path_to_route_underscore_and_config_and_nonpage() {
        assert_eq!(file_path_to_route("pages/_app.tsx"), None);
        assert_eq!(file_path_to_route("pages/next.config.js"), None);
        // Neither pages/ nor app/ segment.
        assert_eq!(file_path_to_route("src/routes/home.tsx"), None);
    }

    #[test]
    fn file_path_to_route_app_index_becomes_root() {
        assert_eq!(file_path_to_route("app/page.tsx").as_deref(), Some("/"));
    }
}
