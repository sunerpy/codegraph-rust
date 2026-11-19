//! Astro (upstream `resolution/frameworks/astro.ts` at `e8ddc4e2`; #2245,
//! #2267).
//!
//! A `src/pages/` file is a route: an `.astro` page, served by the component
//! its file is, or a `.ts`/`.js`/`.mjs` endpoint, served by the verbs it
//! exports. The `Astro` global and the `astro:*` virtual modules are the
//! framework's own, and a PascalCase tag in Astro markup names a component.

use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::Value;

use super::js_scan::JS_SPACE;
use super::page_component::{page_component_ref, resolve_page_component};
use super::{framework_node, sole};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// Astro's virtual module prefixes (upstream `ASTRO_VIRTUAL_MODULES`):
/// framework-provided, not user code.
const ASTRO_VIRTUAL_MODULES: [&str; 9] = [
    "astro:content",
    "astro:assets",
    "astro:actions",
    "astro:env",
    "astro:i18n",
    "astro:middleware",
    "astro:transitions",
    "astro:components",
    "astro:schema",
];

/// The verbs an endpoint's exported handler may be named.
const VERBS: &str = "GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|ALL";

pub struct AstroResolver;

impl FrameworkResolver for AstroResolver {
    fn name(&self) -> &str {
        "astro"
    }

    /// `astro` in the root `package.json`'s dependencies or dev
    /// dependencies, or any `.astro` file.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        declares_astro(context)
            || context
                .get_all_files_shared()
                .iter()
                .any(|file| file.ends_with(".astro"))
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        // A page route names the component its file is.
        if let Some(page) = resolve_page_component(reference, context) {
            return Some(page);
        }
        let name = reference.reference_name.as_str();
        let to_self = || {
            ResolvedRef::new(
                reference.clone(),
                reference.from_node_id.clone(),
                1.0,
                ResolvedBy::Framework,
            )
        };

        // The `Astro` global (`Astro.props`, `Astro.url`, …) is the runtime's
        // in every component's frontmatter, never a user symbol named Astro.
        if is_astro_global(name) {
            return Some(to_self());
        }

        // `astro:*` virtual module imports.
        if reference.reference_kind == EdgeKind::Imports
            && name.starts_with("astro:")
            && ASTRO_VIRTUAL_MODULES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            return Some(to_self());
        }

        // A PascalCase tag, a `references` from the template or a `calls`
        // from a frontmatter expression, names a component — from Astro
        // markup only (#2245): a `.ts` file's `Page` (Playwright's) or a
        // declaration file's `image?: Image` is no `Page.astro`.
        if reference.file_path.ends_with(".astro")
            && is_pascal_case(name)
            && matches!(
                reference.reference_kind,
                EdgeKind::References | EdgeKind::Calls
            )
            && let Some(target) = resolve_component(name, &reference.file_path, context)
        {
            return Some(ResolvedRef::new(
                reference.clone(),
                target,
                0.8,
                ResolvedBy::Framework,
            ));
        }

        None
    }

    /// The `Astro` global is the runtime's whatever else the project
    /// declares: `Astro.redirect(…)` reaches [`Self::resolve`] even when no
    /// node is named `redirect` or `Astro`. Upstream claims nothing, so its
    /// pre-filter drops such a call unless some file happens to declare a
    /// name it is written with, and a sync could not re-read that: the edge
    /// the call gets targets the component that makes it, not the name.
    fn claims_reference(&self, name: &str) -> bool {
        is_astro_global(name)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(extract_route(file_path, content))
    }
}

/// `Astro`, or a member of it (`Astro.props`, `Astro.cookies.has`).
fn is_astro_global(name: &str) -> bool {
    name == "Astro" || name.starts_with("Astro.")
}

/// Whether the root `package.json` declares `astro` (upstream's
/// `{ ...pkg.dependencies, ...pkg.devDependencies }.astro`): only an object
/// field lends its keys, a dev dependency overrides a dependency, and the
/// value must be JavaScript-truthy. A manifest that does not parse, or a
/// `null` one (whose property read throws upstream), declares nothing.
fn declares_astro(context: &dyn ResolutionContext) -> bool {
    let Some(content) = context.read_file_shared("package.json") else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    let mut astro = None;
    for field in ["dependencies", "devDependencies"] {
        if let Some(Value::Object(deps)) = manifest.get(field)
            && let Some(value) = deps.get("astro")
        {
            astro = Some(value);
        }
    }
    astro.is_some_and(js_truthy)
}

/// A JSON value's JavaScript truthiness.
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Upstream `isPascalCase`: `^[A-Z][a-zA-Z0-9]*$`.
fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

/// Upstream `resolveComponent`, each tier resolved only through its sole
/// candidate: the components of the name whose path starts with the
/// referencing file's directory (no trailing `/`: an empty one at the root),
/// else every component of the name. Upstream takes the first of the
/// directory's; several are ambiguous here, and several elsewhere are
/// ambiguous upstream too (#764), both left to the name matcher.
fn resolve_component(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let components: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Component)
        .collect();
    let from_dir = from_file.rfind('/').map_or("", |slash| &from_file[..slash]);
    let in_dir = |node: &&Arc<Node>| node.file_path.starts_with(from_dir);
    if components.iter().any(|node| in_dir(&node)) {
        sole(components.iter().filter(in_dir)).map(|node| node.id.clone())
    } else {
        sole(components.iter()).map(|node| node.id.clone())
    }
}

/// A `src/pages/` file's route and what serves it (upstream `extract`).
/// Underscore-prefixed segments and a stray `*.config.*` are no route.
fn extract_route(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    let normalized = file_path.replace('\\', "/");
    static PAGES: OnceLock<Regex> = OnceLock::new();
    let pages = PAGES.get_or_init(|| Regex::new(r"(?:^|/)src/pages/").expect("pages"));
    let Some(found) = pages.find(&normalized) else {
        return result;
    };
    if ![".astro", ".ts", ".js", ".mjs"]
        .iter()
        .any(|extension| normalized.ends_with(extension))
    {
        return result;
    }
    let after_pages = &normalized[found.end()..];
    let base = after_pages.rsplit('/').next().unwrap_or("");
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    let config = CONFIG.get_or_init(|| Regex::new(r"\.config\.[a-z]+$").expect("config"));
    if after_pages
        .split('/')
        .any(|segment| segment.starts_with('_'))
        || config.is_match(base)
    {
        return result;
    }

    let route_path = file_path_to_astro_route(after_pages);
    let is_page = normalized.ends_with(".astro");
    let language = if is_page {
        Language::Astro
    } else {
        Language::TypeScript
    };
    let route = framework_node(
        format!("route:{file_path}:{route_path}:1"),
        NodeKind::Route,
        route_path.clone(),
        format!("{file_path}::route:{route_path}"),
        file_path.to_string(),
        1,
        1,
        0,
        0,
        language,
        false,
    );
    if is_page {
        result
            .references
            .push(page_component_ref(&route, ".astro", Language::Astro));
    } else {
        // An endpoint is served by the verbs it exports: `export const GET:
        // APIRoute = …`, `export async function POST(…)`. Upstream drops
        // `candidates: [verb]` here, which nothing in resolution reads.
        for (verb, line) in endpoint_exports(content) {
            result.references.push(RefView {
                row_id: None,
                from_node_id: route.id.clone(),
                reference_name: verb,
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language,
                is_function_ref: false,
                reference_subkind: None,
            });
        }
    }
    result.nodes.push(route);
    result
}

/// Every exported endpoint handler and the line its match starts on
/// (upstream `ENDPOINT_EXPORT`, `/^\s*export\s+…/gm`, on the source with
/// its comments). JavaScript's multiline `^` holds after `\n`, `\r`, U+2028
/// and U+2029, which `(?m)` does not all honor, so the pattern is tried
/// anchored at each line start the previous match does not cover. `^\s*`
/// crosses blank lines: the line is where the match starts, as upstream
/// counts it.
fn endpoint_exports(content: &str) -> Vec<(String, i64)> {
    static EXPORT: OnceLock<Regex> = OnceLock::new();
    let export = EXPORT.get_or_init(|| {
        Regex::new(
            &format!(
                r"^{{S}}*export{{S}}+(?:(?:async{{S}}+)?function{{S}}+({VERBS})(?-u:\b)|const{{S}}+({VERBS})(?-u:\b))"
            )
            .replace("{S}", JS_SPACE),
        )
        .expect("endpoint export")
    });
    let mut out = Vec::new();
    let mut from = 0;
    let line_starts = std::iter::once(0).chain(
        content
            .char_indices()
            .filter(|&(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(at, c)| at + c.len_utf8()),
    );
    for start in line_starts {
        if start < from {
            continue;
        }
        let Some(captures) = export.captures(&content[start..]) else {
            continue;
        };
        let verb = captures
            .get(1)
            .or_else(|| captures.get(2))
            .expect("one verb group")
            .as_str();
        let line = content.as_bytes()[..start]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as i64
            + 1;
        out.push((verb.to_string(), line));
        from = start + captures.get(0).expect("whole match").end();
    }
    out
}

/// A `src/pages/` path as an Astro route (upstream `filePathToAstroRoute`):
///
/// ```text
/// blog/[slug].astro     -> /blog/:slug
/// blog/[...path].astro  -> /blog/*path
/// api/posts.ts          -> /api/posts
/// index.astro           -> /
/// ```
fn file_path_to_astro_route(after_pages: &str) -> String {
    static EXTENSION: OnceLock<Regex> = OnceLock::new();
    static INDEX: OnceLock<Regex> = OnceLock::new();
    static REST: OnceLock<Regex> = OnceLock::new();
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let extension =
        EXTENSION.get_or_init(|| Regex::new(r"\.(astro|ts|js|mjs)$").expect("extension"));
    let index = INDEX.get_or_init(|| Regex::new(r"(^|/)index$").expect("index"));
    let rest = REST.get_or_init(|| Regex::new(r"\[\.\.\.([^\]]+)\]").expect("rest"));
    let param = PARAM.get_or_init(|| Regex::new(r"\[([^\]]+)\]").expect("param"));

    let without_extension = extension.replace(after_pages, "");
    let without_index = index.replace(&without_extension, "$1");
    let without_index = without_index.strip_suffix('/').unwrap_or(&without_index);
    let route = format!(
        "/{}",
        param.replace_all(&rest.replace_all(without_index, "*$1"), ":$1")
    );
    if route == "/" {
        return route;
    }
    match route.strip_suffix('/') {
        Some(trimmed) => trimmed.to_string(),
        None => route,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    #[test]
    fn page_paths_become_astro_routes() {
        assert_eq!(file_path_to_astro_route("index.astro"), "/");
        assert_eq!(file_path_to_astro_route("about.astro"), "/about");
        assert_eq!(file_path_to_astro_route("blog/index.astro"), "/blog");
        assert_eq!(file_path_to_astro_route("blog/[slug].astro"), "/blog/:slug");
        assert_eq!(
            file_path_to_astro_route("blog/[...path].astro"),
            "/blog/*path"
        );
        assert_eq!(file_path_to_astro_route("api/posts.ts"), "/api/posts");
        assert_eq!(file_path_to_astro_route("api/index.mjs"), "/api");
        assert_eq!(file_path_to_astro_route("blogindex.astro"), "/blogindex");
        assert_eq!(
            file_path_to_astro_route("[lang]/[...slug].astro"),
            "/:lang/*slug"
        );
    }

    #[test]
    fn javascript_truthiness_of_json_values() {
        for (json, truthy) in [
            ("null", false),
            ("false", false),
            ("true", true),
            ("0", false),
            ("-0", false),
            ("0.0", false),
            ("1", true),
            ("\"\"", false),
            ("\"^4.0.0\"", true),
            ("[]", true),
            ("{}", true),
        ] {
            let value: Value = serde_json::from_str(json).unwrap();
            assert_eq!(js_truthy(&value), truthy, "{json}");
        }
    }

    #[test]
    fn pascal_case_is_ascii_letters_and_digits() {
        assert!(is_pascal_case("Card"));
        assert!(is_pascal_case("BaseLayout2"));
        assert!(!is_pascal_case("card"));
        assert!(!is_pascal_case("Base_Layout"));
        assert!(!is_pascal_case("Über"));
        assert!(!is_pascal_case(""));
    }

    #[test]
    fn endpoint_exports_follow_javascript_line_starts() {
        let content = "import x from 'y';\nexport const GET = () => {};\nexport async function POST() {}\n  export function DELETE() {}\nexport const GETTER = 1;\nconst PUT = 1;\n";
        assert_eq!(
            endpoint_exports(content),
            vec![
                ("GET".to_string(), 2),
                ("POST".to_string(), 3),
                ("DELETE".to_string(), 4),
            ]
        );
        // `^\s*` crosses blank lines: the match starts on the first blank one.
        assert_eq!(
            endpoint_exports("a\n\n\nexport const GET = 1;"),
            vec![("GET".to_string(), 2)]
        );
        // A line separator starts a line; a CRLF line's match starts after
        // its `\r`, on the line before.
        assert_eq!(
            endpoint_exports("a\u{2028}export const PUT = 1;"),
            vec![("PUT".to_string(), 1)]
        );
        assert_eq!(
            endpoint_exports("a\r\nexport const HEAD = 1;"),
            vec![("HEAD".to_string(), 1)]
        );
        // Mid-line exports are no line start.
        assert!(endpoint_exports("x; export const GET = 1;").is_empty());
    }

    // -- upstream `frameworks.test.ts`, `astroResolver.extract` --------------

    fn route_names(file: &str) -> Vec<String> {
        extract_route(file, "")
            .nodes
            .into_iter()
            .filter(|node| node.kind == NodeKind::Route)
            .map(|node| node.name)
            .collect()
    }

    #[test]
    fn src_pages_files_are_routes() {
        assert_eq!(route_names("src/pages/index.astro"), ["/"]);
        assert_eq!(route_names("src/pages/blog/index.astro"), ["/blog"]);
        assert_eq!(route_names("src/pages/about.astro"), ["/about"]);
        assert_eq!(route_names("src/pages/blog/[slug].astro"), ["/blog/:slug"]);
        assert_eq!(route_names("src/pages/[...path].astro"), ["/*path"]);
        assert_eq!(route_names("src/pages/api/posts.ts"), ["/api/posts"]);
        assert_eq!(route_names("src/pages/rss.xml.js"), ["/rss.xml"]);
    }

    #[test]
    fn underscore_segments_config_files_and_other_directories_are_no_routes() {
        for file in [
            "src/pages/_partial.astro",
            "src/pages/blog/_components/Card.astro",
            "src/pages/vite.config.ts",
            "src/components/Button.astro",
            "docs/pages/guide.astro",
            "src/pages/notes.md",
        ] {
            assert!(route_names(file).is_empty(), "{file}");
        }
        assert_eq!(route_names("apps/web/src/pages/index.astro"), ["/"]);
        assert_eq!(route_names("src\\pages\\about.astro"), ["/about"]);
    }

    #[test]
    fn a_page_calls_its_component_and_an_endpoint_references_its_verbs() {
        let page = extract_route("src/pages/blog/[slug].astro", "");
        assert_eq!(page.nodes[0].language, Language::Astro);
        assert_eq!(
            page.nodes[0].id,
            "route:src/pages/blog/[slug].astro:/blog/:slug:1"
        );
        assert_eq!(
            page.nodes[0].qualified_name,
            "src/pages/blog/[slug].astro::route:/blog/:slug"
        );
        let refs: Vec<(&str, EdgeKind)> = page
            .references
            .iter()
            .map(|r| (r.reference_name.as_str(), r.reference_kind))
            .collect();
        assert_eq!(refs, [("[slug]", EdgeKind::Calls)]);

        let endpoint = extract_route(
            "src/pages/api/hello.js",
            "export const GET = () => new Response('hi');\n\nexport async function POST() {}\n",
        );
        assert_eq!(endpoint.nodes[0].language, Language::TypeScript);
        let refs: Vec<(&str, EdgeKind, i64, &str)> = endpoint
            .references
            .iter()
            .map(|r| {
                (
                    r.reference_name.as_str(),
                    r.reference_kind,
                    r.line,
                    r.from_node_id.as_str(),
                )
            })
            .collect();
        let from = "route:src/pages/api/hello.js:/api/hello:1";
        assert_eq!(
            refs,
            [
                ("GET", EdgeKind::References, 1, from),
                ("POST", EdgeKind::References, 2, from),
            ]
        );
    }

    // -- upstream `frameworks.test.ts`, `astroResolver.resolve` --------------

    fn reference(file: &str, name: &str, kind: EdgeKind) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "component:a".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language: Language::Astro,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn component(file: &str, name: &str) -> Node {
        framework_node(
            format!("component:{file}:{name}"),
            NodeKind::Component,
            name.to_string(),
            format!("{file}::{name}"),
            file.to_string(),
            1,
            1,
            0,
            0,
            Language::Astro,
            false,
        )
    }

    fn ctx_of(nodes: Vec<Node>) -> RouteCtx {
        nodes.into_iter().fold(RouteCtx::default(), |ctx, node| {
            let file = node.file_path.clone();
            ctx.routes(&file, vec![node])
        })
    }

    fn target(ctx: &RouteCtx, reference: &RefView) -> Option<(String, f64)> {
        AstroResolver
            .resolve(reference, ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    #[test]
    fn the_astro_global_and_virtual_modules_are_the_frameworks() {
        let ctx = RouteCtx::default();
        let page = "src/pages/index.astro";
        for (name, kind) in [
            ("Astro.props", EdgeKind::References),
            ("Astro", EdgeKind::References),
            ("Astro.redirect", EdgeKind::Calls),
            ("astro:content", EdgeKind::Imports),
            ("astro:assets/x", EdgeKind::Imports),
        ] {
            let resolved = AstroResolver
                .resolve(&reference(page, name, kind), &ctx)
                .unwrap_or_else(|| panic!("{name}"));
            assert_eq!(resolved.target_node_id, "component:a", "{name}");
            assert_eq!(resolved.confidence, 1.0, "{name}");
            assert_eq!(resolved.resolved_by, ResolvedBy::Framework, "{name}");
        }
        // The global from a `.ts` file too, never a user symbol named Astro.
        assert!(
            AstroResolver
                .resolve(
                    &reference("src/lib/nav.ts", "Astro.url", EdgeKind::Calls),
                    &ctx
                )
                .is_some()
        );
        for (name, kind) in [
            ("astrolabe", EdgeKind::Calls),
            ("Astrolabe", EdgeKind::Calls),
            ("astro:unknown", EdgeKind::Imports),
            ("astro:content", EdgeKind::Calls),
        ] {
            assert_eq!(target(&ctx, &reference(page, name, kind)), None, "{name}");
        }
    }

    #[test]
    fn the_astro_global_is_claimed_past_the_name_filter() {
        for name in ["Astro", "Astro.redirect", "Astro.cookies.has"] {
            assert!(AstroResolver.claims_reference(name), "{name}");
        }
        for name in ["Astrolabe", "astro", "astro:content", "Card", "x.Astro"] {
            assert!(!AstroResolver.claims_reference(name), "{name}");
        }
    }

    #[test]
    fn a_page_route_calls_its_own_component() {
        let file = "src/pages/about.astro";
        let route = extract_route(file, "");
        let own = component(file, "about");
        let ctx = ctx_of(vec![
            own.clone(),
            component("src/pages/other.astro", "about"),
        ]);
        assert_eq!(target(&ctx, &route.references[0]), Some((own.id, 0.95)));
    }

    #[test]
    fn a_pascal_case_tag_names_a_component_of_the_directory_or_a_sole_one() {
        let from = "src/pages/index.astro";
        let near = component("src/pages/parts/Card.astro", "Card");
        let far = component("src/components/Card.astro", "Card");
        let tag = reference(from, "Card", EdgeKind::References);
        assert_eq!(
            target(&ctx_of(vec![near.clone(), far.clone()]), &tag),
            Some((near.id.clone(), 0.8))
        );
        assert_eq!(
            target(
                &ctx_of(vec![far.clone()]),
                &reference(from, "Card", EdgeKind::Calls)
            ),
            Some((far.id.clone(), 0.8))
        );
        // Several in the directory, or several elsewhere (#764): ambiguous.
        let twin = component("src/pages/more/Card.astro", "Card");
        assert_eq!(target(&ctx_of(vec![near, twin, far.clone()]), &tag), None);
        let other = component("src/layouts/Card.astro", "Card");
        assert_eq!(target(&ctx_of(vec![far.clone(), other]), &tag), None);
        // The directory is a prefix without its `/`, as upstream reads it.
        let sibling = component("src/pagesmith/Card.astro", "Card");
        assert_eq!(
            target(&ctx_of(vec![sibling.clone(), far.clone()]), &tag),
            Some((sibling.id, 0.8))
        );
    }

    #[test]
    fn only_astro_markup_names_a_component() {
        let page = component("src/components/Page.astro", "Page");
        let ctx = ctx_of(vec![page]);
        // #2245: Playwright's `Page` in a test, a declaration's `Image`.
        for file in ["tests/test-utils.ts", "src/env.d.ts"] {
            assert_eq!(
                target(&ctx, &reference(file, "Page", EdgeKind::References)),
                None,
                "{file}"
            );
        }
        let from = "src/pages/index.astro";
        assert_eq!(
            target(&ctx, &reference(from, "Page", EdgeKind::Imports)),
            None
        );
        assert_eq!(
            target(&ctx, &reference(from, "page", EdgeKind::References)),
            None
        );
        assert!(target(&ctx, &reference(from, "Page", EdgeKind::References)).is_some());
    }

    #[test]
    fn detection_reads_the_root_manifest_as_javascript_spreads_it() {
        let detected = |manifest: &str| {
            AstroResolver.detect(&RouteCtx::default().file("package.json", manifest))
        };
        assert!(detected(r#"{"dependencies": {"astro": "^4.0.0"}}"#));
        assert!(detected(r#"{"devDependencies": {"astro": "^4.0.0"}}"#));
        assert!(detected(
            r#"{"dependencies": {"astro": null}, "devDependencies": {"astro": "^4"}}"#
        ));
        for manifest in [
            r#"{"dependencies": {"astro": ""}}"#,
            r#"{"devDependencies": {"astro": 0}}"#,
            r#"{"dependencies": {"astro": false}}"#,
            r#"{"dependencies": {"astro": "^4"}, "devDependencies": {"astro": null}}"#,
            r#"{"dependencies": ["astro"]}"#,
            r#"{"dependencies": "astro"}"#,
            r#"{"peerDependencies": {"astro": "^4"}}"#,
            "null",
            "{ not json",
        ] {
            assert!(!detected(manifest), "{manifest}");
        }
        // A workspace's manifest is not the root's; any `.astro` file is.
        assert!(!AstroResolver.detect(&RouteCtx::default().file(
            "apps/web/package.json",
            r#"{"dependencies": {"astro": "^4"}}"#
        )));
        assert!(
            AstroResolver.detect(
                &RouteCtx::default()
                    .file("package.json", "null")
                    .file("src/components/Card.astro", "")
            )
        );
    }
}
