//! React / Next.js [`FrameworkResolver`] — ports
//! `upstream resolution/frameworks/react.ts`.

use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::{framework_node, js_language_for, js_scan};
use crate::framework::FrameworkResolver;
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// React / Next.js resolver (`reactResolver`, react.ts:10-269).
pub struct ReactResolver;

impl FrameworkResolver for ReactResolver {
    fn name(&self) -> &str {
        "react"
    }

    /// The languages React components, hooks and contexts are written and
    /// used in. The JSX dialects are here so route extraction runs on the
    /// `.tsx`/`.jsx` files a `<Route element={<X/>}>` lives in (upstream
    /// #841), and `resolve` reads only a script's references, which its
    /// `resolve_languages` default to (#2211): halo's Java `import
    /// org.springframework…SecurityContext` is no React context.
    fn languages(&self) -> Option<&[Language]> {
        const LANGS: [Language; 4] = [
            Language::JavaScript,
            Language::TypeScript,
            Language::Tsx,
            Language::Jsx,
        ];
        Some(&LANGS)
    }

    // Ports reactResolver.detect (react.ts:14-32).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // React in a package.json: the root's, or an app's (`frontend/`,
        // `apps/web/`, `src/Web/ClientApp/`).
        if super::package_deps::depends_on(context, &["react", "next", "react-native"]) {
            return true;
        }
        context
            .get_all_files_shared()
            .iter()
            .any(|f| f.ends_with(".jsx") || f.ends_with(".tsx"))
    }

    // Ports reactResolver.resolve (react.ts:44-131).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = &reference.reference_name;
        // Pattern 1 reads a component USED in markup, which only parses in
        // .tsx/.jsx. Without this gate every PascalCase TYPE in a plain .ts
        // file went through component resolution: in a monorepo with
        // same-named classes per package (#764, amplication), a GraphQL-types
        // file's own `Account` type lost to an arbitrary `Account` class in
        // another package.
        let component = matches!(reference.language, Language::Tsx | Language::Jsx)
            && is_pascal_case(name)
            && !is_built_in_type(name);
        let hook = name.starts_with("use") && name.len() > 3;
        let provided = name.ends_with("Context") || name.ends_with("Provider");
        if !(component || hook || provided) {
            return None;
        }

        // A component, hook or context the file IMPORTS is the import's: the
        // package's (`useQuery` from `@tanstack/react-query`, `<Button>` from
        // a UI kit, upstream #2120 and #2475), or the module the import
        // names, which import resolution finds. Framework resolution runs
        // first, and a name lookup here bound trpc's tests' `useQuery` to a
        // hook nested in one of trpc's own factories.
        if context
            .get_import_mappings(&reference.file_path, reference.language)
            .iter()
            .any(|mapping| mapping.local_name == *name)
        {
            return None;
        }

        // Pattern 1: component references (PascalCase).
        if component {
            // A name the file declares itself as a value is that declaration,
            // whatever refers to it (upstream #2420): the function the value
            // wraps, or else the declaration, which name matching binds —
            // never a same-named component in another app of the repository.
            if let Some(own) = declared_component(name, &reference.file_path, context) {
                return own
                    .map(|component| framework_resolved(reference, component.id.clone(), 0.9));
            }
            if let Some(target) = resolve_component(name, &reference.file_path, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        // Pattern 2: hook references (`use*`).
        if hook {
            if let Some(target) =
                resolve_hook(name, &reference.file_path, context, reference.language)
            {
                return Some(framework_resolved(reference, target, 0.85));
            }
        }

        // Pattern 3: context references (`*Context` / `*Provider`).
        if provided {
            if let Some(target) = resolve_context(name, reference, context) {
                return Some(framework_resolved(reference, target, 0.8));
            }
        }

        None
    }

    // Ports reactResolver.extract (react.ts:133-188).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &crate::framework::FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut nodes = Vec::new();
        let mut references = Vec::new();
        let jsx_lang = js_language_for(file_path);

        // Components and custom hooks are NOT extracted here (upstream #841).
        // The tree-sitter extractor emits them across .ts/.tsx/.js/.jsx:
        // function and arrow components as `function` nodes, components a
        // wrapper declares (`forwardRef`/`memo`/`styled`) as `component`
        // nodes, and `useX` hooks as `function` nodes. The regex this pass
        // carried duplicated those nodes (a `useAuth` became two `function`
        // nodes); it contributes only what tree-sitter can't: routes.

        // React Router <Route .../> (v5/v6) (react.ts:158-198).
        for tag in route_tag_regex().find_iter(content) {
            let window = route_opening_tag_window(content, tag.start());
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
                let win = route_object_own_properties(content, whole.start());
                let comp = obj_element_attr()
                    .captures(&win)
                    .or_else(|| obj_component_attr().captures(&win))
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
        row_id: None,
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
    ResolvedRef::new(
        reference.clone(),
        target_node_id,
        confidence,
        ResolvedBy::Framework,
    )
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
    let candidates = context.get_nodes_by_name_shared(name);
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

/// A value `file_path` declares itself under a component's name, and what it
/// renders (upstream `declaredComponent`, #2420). JavaScript scoping makes
/// the name, used anywhere in that file — `<RegisterPage />`, a route's
/// `element: <RegisterPage />`, a type — this declaration, never another
/// file's same-named component. `Some(component)` is the function the value
/// wraps (`observer(function Login() {…})`), or `Some(None)` when there is
/// none to find and the declaration is the binding, by name. `None` when the
/// file declares no such value (a function or class component is found the
/// usual way). A value's lazy loader is read in a later change.
fn declared_component(
    name: &str,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> Option<Option<Arc<Node>>> {
    let declaration = declared_value(name, file_path, context)?;
    Some(wrapped_component(&declaration, context))
}

/// The module-level `const`, `let` or `var` `file_path` declares as `name`
/// (upstream `declaredValue`): a module-level declarator's qualified name is
/// its bare name.
fn declared_value(
    name: &str,
    file_path: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            node.file_path == file_path
                && matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
                && node.qualified_name == name
        })
        .min_by_key(|node| (node.start_line, node.start_column))
}

/// The component a value wraps, which is what it renders (upstream
/// `wrappedComponent`): the function it holds (`const Application =
/// observer(function Application() {…})`), or the component of its own file
/// a wrapper call is handed (`const TableView = observer(TableViewInner)`,
/// `forwardRef(Inner) as {…}`). An inner function counts when it has the
/// value's name or ends where the value does — never a helper inside an
/// arrow the graph has no node for (`withFallback('X', () => { const load =
/// () => …; … })`).
fn wrapped_component(value: &Node, context: &dyn ResolutionContext) -> Option<Arc<Node>> {
    let nodes = context.get_nodes_in_file_shared(&value.file_path);
    let inner: Vec<&Arc<Node>> = nodes
        .iter()
        .filter(|node| {
            is_component_kind(node.kind)
                && node
                    .qualified_name
                    .strip_prefix(value.qualified_name.as_str())
                    .and_then(|rest| rest.strip_prefix("::"))
                    == Some(node.name.as_str())
                && node.start_line >= value.start_line
                && node.end_line <= value.end_line
        })
        .collect();
    if !inner.is_empty() {
        let held = inner
            .iter()
            .find(|node| node.name == value.name)
            .or_else(|| {
                (inner.len() == 1 && inner[0].end_line == value.end_line).then(|| &inner[0])
            });
        return held.map(|node| Arc::clone(node));
    }
    let code = strip_comments_for_regex(&declaration_text(value, context), CommentLang::TypeScript);
    let init = js_scan::initializer_at(&code, value.name.len())?;
    let handed = js_scan::wrapped_name(&code[init..js_scan::statement_end(&code, init, false)])?;
    if !is_pascal_case(handed) {
        return None;
    }
    nodes
        .iter()
        .find(|node| {
            node.name == handed && node.qualified_name == handed && is_component_kind(node.kind)
        })
        .cloned()
}

/// A declarator's own source text, `B = lazy(…)` in `const A = lazy(…), B =
/// lazy(…)` (upstream `declarationText`): its lines, the last cut at its end
/// column and the first from its start column. Columns are the byte columns
/// the extractor stored; a file that no longer reads that way gives nothing.
fn declaration_text(node: &Node, context: &dyn ResolutionContext) -> String {
    let Some(facts) = context.source_facts(&node.file_path) else {
        return String::new();
    };
    let (Ok(first), Ok(end)) = (
        usize::try_from(node.start_line - 1),
        usize::try_from(node.end_line),
    ) else {
        return String::new();
    };
    let end = end.min(facts.line_count());
    if first >= end {
        return String::new();
    }
    let mut lines: Vec<&str> = (first..end).map(|index| facts.line(index)).collect();
    let last = lines.len() - 1;
    if let Ok(cut @ 1..) = usize::try_from(node.end_column) {
        let line = lines[last];
        let Some(kept) = line.get(..cut.min(line.len())) else {
            return String::new();
        };
        lines[last] = kept;
    }
    let from = usize::try_from(node.start_column).unwrap_or(0);
    let Some(kept) = lines[0].get(from.min(lines[0].len())..) else {
        return String::new();
    };
    lines[0] = kept;
    lines.join("\n")
}

/// `isEsmLanguage` (react.ts:1312-1314): JS/TS and their JSX dialects,
/// modules where a cross-file name needs an import.
fn is_esm_script(language: Language) -> bool {
    matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
    )
}

/// `resolveHook` (react.ts:1320-1346).
fn resolve_hook(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
    language: Language,
) -> Option<String> {
    let candidates = context.get_nodes_by_name_shared(name);
    let is_hook = |node: &Node| node.kind == NodeKind::Function && node.name.starts_with("use");
    // A hook nested inside another function is only callable in there
    // (upstream #2120).
    let nested = |node: &Node| {
        context
            .get_nodes_in_file_shared(&node.file_path)
            .iter()
            .any(|outer| {
                outer.id != node.id
                    && matches!(outer.kind, NodeKind::Function | NodeKind::Method)
                    && outer.start_line <= node.start_line
                    && outer.end_line >= node.end_line
                    && (outer.start_line < node.start_line || outer.end_line > node.end_line)
            })
    };
    if let Some(own) = candidates
        .iter()
        .find(|node| node.file_path == from_file && is_hook(node) && !nested(node))
    {
        return Some(own.id.clone());
    }
    // A JS/TS module reaches another file's hook only by importing it — the
    // import resolver's to follow (an imported name never gets here) —
    // never by name alone (upstream #2240).
    if is_esm_script(language) {
        return None;
    }
    let hooks: Vec<_> = candidates
        .iter()
        .filter(|node| is_hook(node) && !nested(node))
        .collect();
    const HOOK_DIRS: [&str; 4] = ["/hooks/", "/src/hooks/", "/lib/hooks/", "/utils/hooks/"];
    hooks
        .iter()
        .find(|node| HOOK_DIRS.iter().any(|dir| node.file_path.contains(dir)))
        .or_else(|| hooks.first())
        .map(|node| node.id.clone())
}

/// `resolveContext` (react.ts:1351-1377).
fn resolve_context(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // In a JS/TS module only the file's own context is in reach by name;
    // another file's comes through an import (trpc's adapters'
    // `createContext?.(…)` is an option, not an example app's
    // `createContext`, upstream #2240).
    if is_esm_script(reference.language) {
        return context
            .get_nodes_by_name_shared(name)
            .iter()
            .find(|node| node.file_path == reference.file_path)
            .map(|node| node.id.clone());
    }
    let candidates = context.get_nodes_by_name_shared(name);
    if candidates.is_empty() {
        // Without the Context/Provider suffix.
        let base_name = strip_context_suffix(name);
        if base_name != name {
            let base_candidates = context.get_nodes_by_name_shared(base_name);
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

/// Hard byte bound on how far the `<Route …>` opening-tag scan may look forward
/// from the `<` (upstream issue #1348).
///
/// The scan normally stops at the tag's own `>` (see [`opening_tag_end`]), but a
/// malformed or unterminated tag has no `>` to find, so the search itself must be
/// capped: without a cap one stray `<Route` would drag the scan to end-of-file,
/// and a file carrying many such tags would turn this pass quadratic.
///
/// 2048 bytes is the cap, sized off the longest opening tags actually measured
/// rather than a guess. Measured reference points, each the true tag length up to
/// its brace-aware terminator: a lazy, error-bounded v6 data route spread over
/// six attribute lines is 278 bytes; the same route with `hydrateFallbackElement`,
/// `shouldRevalidate` and a `handle={{…}}` object is 525 bytes; a prettier-wrapped
/// tag carrying twelve extra `data-*` attributes is 700 bytes. 2048 is ~3x that
/// widest measured tag, so a legitimate tag is never truncated, while the work per
/// tag stays constant. It is also strictly wider than the fixed 400-byte window it
/// replaces (`react.ts:114-123`), so no attribute the old window could reach is
/// lost.
const ROUTE_OPENING_TAG_SCAN_LIMIT: usize = 2048;

/// The `<Route …>` opening tag's own text, starting at `start` (its `<`).
///
/// Replaces the upstream fixed 400-byte window (`react.ts:114-123`), which never
/// cut at the tag's end and therefore let a parent or pathless route match a
/// *sibling's* `path`/`element` (upstream issue #1348). The window is bounded
/// twice over: by the tag terminator when the tag is well formed, and by
/// [`ROUTE_OPENING_TAG_SCAN_LIMIT`] always — so an unterminated tag can never
/// scan unboundedly. A malformed tag additionally stops at the next `<Route`, so
/// even then it cannot borrow a sibling's attributes.
fn route_opening_tag_window(content: &str, start: usize) -> &str {
    let bounded = byte_window(content, start, ROUTE_OPENING_TAG_SCAN_LIMIT);
    if let Some(end) = opening_tag_end(bounded) {
        return &bounded[..end];
    }
    match bounded.get(1..).and_then(|rest| rest.find("<Route")) {
        Some(idx) => &bounded[..idx + 1],
        None => bounded,
    }
}

/// Hard byte bound on how far the data-router route-object scan may walk forward
/// from a `path:` key (upstream issue #1348, data-router half).
///
/// The walk normally stops at the `}` closing that route object (see
/// [`route_object_own_properties`]), but a malformed or unterminated object literal
/// has no matching `}` to find, so the walk itself must be capped: without a cap one
/// stray `path:` would drag it to end-of-file, and a file carrying many such keys
/// would turn this pass quadratic. This is the object-form twin of
/// [`ROUTE_OPENING_TAG_SCAN_LIMIT`], and both invariants are the same: terminate at
/// the construct's own end, and cap the search regardless.
///
/// 4096 bytes is the cap, sized off route objects actually measured rather than a
/// guess. Measured reference points, each the raw walk distance from the object's
/// `path:` key to its own closing `}` (nested children included, because the walk
/// traverses and elides them rather than stopping at them): a minimal inline
/// `{ path, element }` is 38 bytes; a typical v6.4 route with `loader` and
/// `errorElement` is 142; the same plus `action`, `handle={…}` and
/// `shouldRevalidate`, with `element` last, is 407; a wide enterprise route adding
/// `hydrateFallbackElement` and a nested `handle` object is 611; a parent route
/// whose `children` array holds eight loader-bearing child routes, with the parent's
/// own `element` declared last — the worst realistic walk, since every child byte
/// must be traversed before the parent's own `element` is reached — is 1503. 4096 is
/// ~2.7x that widest measured object, so a legitimate object is never truncated,
/// while the work per `path:` key stays constant. It is also strictly wider than the
/// fixed 300-byte window it replaces (`react.ts:206`), so no property the old window
/// could reach is lost.
const ROUTE_OBJECT_SCAN_LIMIT: usize = 4096;

/// The route object's OWN properties, as text, starting at the `path:` key that
/// begins at `path_key_start`.
///
/// Replaces the upstream fixed 300-byte window (`react.ts:206`), which scanned
/// blindly forward and took the FIRST `element`/`Component` it found — so a parent
/// route with no `element` of its own silently borrowed a CHILD route's component,
/// the same borrowing defect [`route_opening_tag_window`] fixes for the `<Route>`
/// JSX form (upstream issue #1348).
///
/// Nested `{…}` / `[…]` groups are ELIDED, not merely stopped at: a `children: […]`
/// array cannot leak its own `element` into the parent's text, and neither can a
/// `handle: {…}` object. Quoted values are copied verbatim, so a `}` or `>` inside
/// `path: 'a>b'` neither ends the object nor unbalances the depth. The walk is
/// bounded twice over, exactly as the JSX form is: by the object's own closing `}`
/// when the literal is well formed, and by [`ROUTE_OBJECT_SCAN_LIMIT`] always. A
/// malformed object additionally stops at the next `path:` key, so even then it
/// cannot borrow a following sibling's properties.
fn route_object_own_properties(content: &str, path_key_start: usize) -> String {
    let bounded = byte_window(content, path_key_start, ROUTE_OBJECT_SCAN_LIMIT);
    let bytes = bounded.as_bytes();
    let mut own = String::with_capacity(bounded.len());
    // Start of the run of depth-0 bytes not yet copied into `own`. Every byte the
    // walk inspects structurally is ASCII, so every boundary is a char boundary.
    let mut segment_start = 0usize;
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;

    for (index, byte) in bytes.iter().enumerate() {
        match quote {
            Some(open) => {
                if *byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' | b'`' => quote = Some(*byte),
                b'{' | b'[' => {
                    if depth == 0 {
                        // Elide the nested group; a space keeps neighbouring tokens
                        // from fusing across the hole.
                        own.push_str(&bounded[segment_start..index]);
                        own.push(' ');
                    }
                    depth += 1;
                }
                b'}' | b']' => {
                    if depth == 0 {
                        // This object's own end: nothing beyond it belongs to it.
                        own.push_str(&bounded[segment_start..index]);
                        return own;
                    }
                    depth -= 1;
                    if depth == 0 {
                        segment_start = index + 1;
                    }
                }
                b'p' if depth == 0 && index > 0 && is_path_key_at(bounded, index) => {
                    // Unterminated object: a second depth-0 `path:` can only be a
                    // following sibling's, so stop rather than borrow from it.
                    own.push_str(&bounded[segment_start..index]);
                    return own;
                }
                _ => {}
            },
        }
    }

    if depth == 0 {
        own.push_str(&bounded[segment_start..]);
    }
    own
}

/// Whether a `path` object key (`path` then optional whitespace then `:`) starts at
/// `index` in `text`.
fn is_path_key_at(text: &str, index: usize) -> bool {
    let Some(rest) = text.get(index..).and_then(|r| r.strip_prefix("path")) else {
        return false;
    };
    rest.trim_start().starts_with(':')
}

/// Byte index just past the `>` closing the opening tag that starts at byte 0 of
/// `text`, or `None` when the tag does not terminate inside `text`.
///
/// `{…}` expression containers and quoted attribute values are skipped, so the
/// `>` inside `element={<Comp/>}` or `path="a>b"` does not end the tag early.
fn opening_tag_end(text: &str) -> Option<usize> {
    let mut brace_depth = 0usize;
    let mut quote: Option<u8> = None;
    for (index, byte) in text.as_bytes().iter().enumerate() {
        match quote {
            Some(open) => {
                if *byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' | b'`' => quote = Some(*byte),
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b'>' if brace_depth == 0 => return Some(index + 1),
                _ => {}
            },
        }
    }
    None
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
    /// Extraction context for a resolver that needs no project config.
    fn test_extraction_context() -> crate::framework::FrameworkExtractionContext {
        crate::framework::FrameworkExtractionContext::without_config("")
    }

    use super::*;
    use crate::types::ImportMapping;
    use codegraph_core::types::Node;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Ctx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
        imports: HashMap<String, Vec<ImportMapping>>,
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
        /// `file` imports `local` from `source`.
        fn import(mut self, file: &str, local: &str, source: &str) -> Self {
            self.imports
                .entry(file.to_string())
                .or_default()
                .push(ImportMapping {
                    local_name: local.to_string(),
                    exported_name: local.to_string(),
                    source: source.to_string(),
                    is_default: false,
                    is_namespace: false,
                });
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
        fn get_import_mappings(&self, file: &str, _l: Language) -> Vec<ImportMapping> {
            self.imports.get(file).cloned().unwrap_or_default()
        }
    }

    /// A node of `file` at `lines` (1-based, inclusive) and byte `columns`,
    /// with its own qualified name.
    fn placed(
        kind: NodeKind,
        qualified_name: &str,
        file: &str,
        lines: (i64, i64),
        columns: (i64, i64),
    ) -> Node {
        let name = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
        Node {
            qualified_name: qualified_name.to_string(),
            start_line: lines.0,
            end_line: lines.1,
            start_column: columns.0,
            end_column: columns.1,
            ..mk_node(
                &format!("{}:{file}:{qualified_name}:{}", kind.as_str(), lines.0),
                kind,
                name,
                file,
                Language::Tsx,
            )
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
            row_id: None,
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
    fn resolve_hook_in_a_script_is_the_files_own() {
        // In a JS/TS module another file's hook comes through an import (the
        // import resolver's), never by name (upstream #2240).
        let hook = mk_node(
            "fn:src/util.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/util.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().node(hook.clone());
        let elsewhere = a_ref("useThing", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&elsewhere, &ctx).is_none());
        let own = a_ref(
            "useThing",
            EdgeKind::Calls,
            "src/util.ts",
            Language::TypeScript,
        );
        let r = ReactResolver.resolve(&own, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, hook.id);
        assert_eq!(r.confidence, 0.85);
    }

    #[test]
    fn resolve_hook_never_targets_a_nested_hook() {
        // A hook declared inside another function is only callable in there
        // (upstream #2120): trpc's `useQuery` inside one of its factories.
        let factory = placed(
            NodeKind::Function,
            "createHooks",
            "src/hooks.ts",
            (1, 10),
            (0, 1),
        );
        let nested = placed(
            NodeKind::Function,
            "createHooks::useQuery",
            "src/hooks.ts",
            (2, 4),
            (2, 3),
        );
        let ctx = Ctx::default().node(factory).node(nested);
        let reference = a_ref(
            "useQuery",
            EdgeKind::Calls,
            "src/hooks.ts",
            Language::TypeScript,
        );
        assert!(ReactResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_hook_outside_a_script_prefers_a_hooks_directory() {
        // Only a non-script language reaches another file's hook by name: a
        // hooks/ directory first, else the first candidate.
        let far = mk_node(
            "fn:src/util.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/util.ts",
            Language::TypeScript,
        );
        let near = mk_node(
            "fn:src/hooks/useThing.ts:useThing:1",
            NodeKind::Function,
            "useThing",
            "src/hooks/useThing.ts",
            Language::TypeScript,
        );
        let ctx = Ctx::default().node(far.clone()).node(near.clone());
        assert_eq!(
            resolve_hook("useThing", "src/a.py", &ctx, Language::Python),
            Some(near.id)
        );
        let ctx = Ctx::default().node(far.clone());
        assert_eq!(
            resolve_hook("useThing", "src/a.py", &ctx, Language::Python),
            Some(far.id)
        );
    }

    #[test]
    fn resolve_hook_none_when_not_a_use_function() {
        // Candidate exists but is not a `use*` function -> no hook resolution.
        let notf = mk_node(
            "class:src/Page.tsx:useThing:1",
            NodeKind::Class,
            "useThing",
            "src/Page.tsx",
            Language::Tsx,
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
    fn resolve_context_in_a_script_is_the_files_own() {
        // trpc's adapters' `createContext?.(…)` is an option, not an example
        // app's `createContext` (upstream #2240).
        let other = mk_node(
            "var:src/context/AuthContext.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/context/AuthContext.tsx",
            Language::Tsx,
        );
        let own = mk_node(
            "var:src/App.tsx:AuthContext:1",
            NodeKind::Variable,
            "AuthContext",
            "src/App.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(other).node(own.clone());
        let reference = a_ref(
            "AuthContext",
            EdgeKind::References,
            "src/App.tsx",
            Language::Tsx,
        );
        let r = ReactResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, own.id);
        let elsewhere = a_ref(
            "AuthContext",
            EdgeKind::References,
            "src/Page.tsx",
            Language::Tsx,
        );
        assert!(ReactResolver.resolve(&elsewhere, &ctx).is_none());
    }

    /// A reference outside JS/TS, which only [`resolve_context`] itself sees.
    fn script_less_ref(name: &str) -> RefView {
        a_ref(name, EdgeKind::References, "src/app.py", Language::Python)
    }

    #[test]
    fn resolve_context_strips_suffix_when_no_direct_match() {
        // No `AuthProvider` node, but a base `Auth` node exists -> suffix strip
        // fallback resolves to it.
        let base = mk_node(
            "var:src/Auth.tsx:Auth:1",
            NodeKind::Variable,
            "Auth",
            "src/Auth.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(base.clone());
        assert_eq!(
            resolve_context("AuthProvider", &script_less_ref("AuthProvider"), &ctx),
            Some(base.id)
        );
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
        assert_eq!(
            resolve_context("AuthContext", &script_less_ref("AuthContext"), &ctx),
            Some(near.id)
        );
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
        assert_eq!(
            resolve_context("MissingProvider", &script_less_ref("MissingProvider"), &ctx),
            None
        );
    }

    // -- resolve: the file's imports and its own values ---------------------

    #[test]
    fn an_imported_name_is_left_to_import_resolution() {
        // antd's `<Button>` renders the package's, not the project's
        // namesake (upstream #2120, #2475); the same for a hook or context.
        let button = mk_node(
            "component:src/components/Button.tsx:Button:1",
            NodeKind::Component,
            "Button",
            "src/components/Button.tsx",
            Language::Tsx,
        );
        let hook = mk_node(
            "fn:src/Page.tsx:useQuery:1",
            NodeKind::Function,
            "useQuery",
            "src/Page.tsx",
            Language::Tsx,
        );
        let ctx = Ctx::default().node(button.clone()).node(hook.clone());
        let tag = a_ref(
            "Button",
            EdgeKind::References,
            "src/Page.tsx",
            Language::Tsx,
        );
        let call = a_ref("useQuery", EdgeKind::Calls, "src/Page.tsx", Language::Tsx);
        assert_eq!(
            ReactResolver.resolve(&tag, &ctx).map(|r| r.target_node_id),
            Some(button.id)
        );
        assert_eq!(
            ReactResolver.resolve(&call, &ctx).map(|r| r.target_node_id),
            Some(hook.id)
        );
        let ctx = ctx.import("src/Page.tsx", "Button", "antd").import(
            "src/Page.tsx",
            "useQuery",
            "@tanstack/react-query",
        );
        assert!(ReactResolver.resolve(&tag, &ctx).is_none());
        assert!(ReactResolver.resolve(&call, &ctx).is_none());
    }

    /// `src/App.tsx` for the declared-value cases, with each value's node.
    fn app_with_values() -> Ctx {
        let file = "src/App.tsx";
        let source = "import { observer } from 'mobx-react';\n\
                      const Panel = observer(function PanelInner() {\n\
                      \x20 return <div />;\n\
                      });\n\
                      function Inner() { return <span />; }\n\
                      const Item = forwardRef(Inner) as any; // a cast\n\
                      const Home = lazy(() => import('./pages/Home'));\n";
        let mut ctx = Ctx::default().file(file, source);
        for (kind, name, lines, columns) in [
            (NodeKind::Constant, "Panel", (2, 4), (6, 2)),
            (NodeKind::Function, "Panel::PanelInner", (2, 4), (23, 1)),
            (NodeKind::Function, "Inner", (5, 5), (0, 37)),
            (NodeKind::Constant, "Item", (6, 6), (6, 37)),
            (NodeKind::Constant, "Home", (7, 7), (6, 47)),
        ] {
            ctx = ctx.node(placed(kind, name, file, lines, columns));
        }
        // Another app's namesakes, in a component directory.
        for name in ["Panel", "Item", "Home"] {
            ctx = ctx.node(mk_node(
                &format!("function:src/components/{name}.tsx:{name}:1"),
                NodeKind::Function,
                name,
                &format!("src/components/{name}.tsx"),
                Language::Tsx,
            ));
        }
        ctx
    }

    #[test]
    fn a_value_the_file_declares_renders_what_it_wraps() {
        // Upstream #2420: the function a value holds, or the component of
        // its own file a wrapper call is handed, past a type assertion.
        let ctx = app_with_values();
        for (name, target) in [
            ("Panel", "function:src/App.tsx:Panel::PanelInner:2"),
            ("Item", "function:src/App.tsx:Inner:5"),
        ] {
            let reference = a_ref(name, EdgeKind::References, "src/App.tsx", Language::Tsx);
            let r = ReactResolver.resolve(&reference, &ctx).expect(name);
            assert_eq!(r.target_node_id, target, "{name}");
            assert_eq!(r.confidence, 0.9, "{name}");
        }
    }

    #[test]
    fn a_value_wrapping_no_component_is_its_own_declaration() {
        // Nothing to render is found, so the declaration binds by name, never
        // another app's same-named component; elsewhere that one still wins.
        let ctx = app_with_values();
        let own = a_ref("Home", EdgeKind::References, "src/App.tsx", Language::Tsx);
        assert!(ReactResolver.resolve(&own, &ctx).is_none());
        let elsewhere = a_ref("Home", EdgeKind::References, "src/Page.tsx", Language::Tsx);
        assert_eq!(
            ReactResolver
                .resolve(&elsewhere, &ctx)
                .map(|r| r.target_node_id),
            Some("function:src/components/Home.tsx:Home:1".to_string())
        );
    }

    #[test]
    fn an_inner_function_counts_by_its_name_or_its_end() {
        let file = "src/Page.tsx";
        let value = placed(NodeKind::Constant, "Page", file, (1, 6), (6, 2));
        // A helper inside an arrow the graph has no node for renders nothing.
        let ctx = Ctx::default().node(value.clone()).node(placed(
            NodeKind::Function,
            "Page::load",
            file,
            (2, 2),
            (8, 20),
        ));
        assert_eq!(wrapped_component(&value, &ctx), None);
        // The value's namesake is what it holds, wherever it ends.
        let ctx = ctx.node(placed(
            NodeKind::Function,
            "Page::Page",
            file,
            (3, 5),
            (2, 3),
        ));
        assert_eq!(
            wrapped_component(&value, &ctx).map(|node| node.id.clone()),
            Some("function:src/Page.tsx:Page::Page:3".to_string())
        );
        // A lone inner function ending where the value does.
        let ctx = Ctx::default().node(value.clone()).node(placed(
            NodeKind::Function,
            "Page::Render",
            file,
            (2, 6),
            (8, 1),
        ));
        assert_eq!(
            wrapped_component(&value, &ctx).map(|node| node.id.clone()),
            Some("function:src/Page.tsx:Page::Render:2".to_string())
        );
    }

    #[test]
    fn a_declaration_reads_its_own_declarator() {
        let ctx = app_with_values();
        let item = placed(NodeKind::Constant, "Item", "src/App.tsx", (6, 6), (6, 37));
        assert_eq!(
            declaration_text(&item, &ctx),
            "Item = forwardRef(Inner) as any"
        );
        let panel = placed(NodeKind::Constant, "Panel", "src/App.tsx", (2, 4), (6, 2));
        assert_eq!(
            declaration_text(&panel, &ctx),
            "Panel = observer(function PanelInner() {\n  return <div />;\n})"
        );
        // A file that no longer reads that way gives nothing.
        let stale = placed(NodeKind::Constant, "Item", "src/App.tsx", (60, 61), (6, 37));
        assert_eq!(declaration_text(&stale, &ctx), "");
        let ctx = Ctx::default().file("src/App.tsx", "const \u{e9}t\u{e9} = 1;\n");
        let split = placed(NodeKind::Constant, "x", "src/App.tsx", (1, 1), (7, 20));
        assert_eq!(declaration_text(&split, &ctx), "");
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

    // -- extract: routes only -------------------------------------------------

    #[test]
    fn extract_emits_no_component_or_hook_nodes() {
        // The tree-sitter extractor owns components and hooks (upstream
        // #841); a regex node here duplicated them.
        let content = "export const Card = () => { return <div/>; };\n\
                       export function Panel() { return <div/>; }\n\
                       const Fancy = React.forwardRef(() => <div/>);\n\
                       export function useCounter() { return 0; }\n";
        for file in [
            "src/Card.tsx",
            "src/Card.jsx",
            "src/useCounter.ts",
            "src/useX.js",
        ] {
            let result = ReactResolver
                .extract(file, content, &test_extraction_context())
                .expect("extract");
            assert!(result.nodes.is_empty(), "{file}: {:?}", result.nodes);
        }
    }

    #[test]
    fn extract_route_element_attribute_variant() {
        // <Route ... element={<Home/>}/> uses the element attr branch.
        let content = "<Route path=\"/x\" element={<Home/>}/>";
        let result = ReactResolver
            .extract("src/App.tsx", content, &test_extraction_context())
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
            .extract("src/App.tsx", &content, &test_extraction_context())
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
            .extract("src/App.tsx", content, &test_extraction_context())
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn extract_data_router_object_routes() {
        let content =
            "const r = createBrowserRouter([\n  { path: '/dash', element: <Dashboard/> },\n]);";
        let result = ReactResolver
            .extract("src/routes.tsx", content, &test_extraction_context())
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
            .extract("src/routes.tsx", &content, &test_extraction_context())
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
            .extract("src/routes.tsx", content, &test_extraction_context())
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
            .extract("src/routes.tsx", content, &test_extraction_context())
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn extract_nextjs_app_page_route() {
        let content = "export default function Page() { return <div/>; }";
        let result = ReactResolver
            .extract("app/blog/page.tsx", content, &test_extraction_context())
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
            .extract("pages/x.tsx", content, &test_extraction_context())
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
