//! Svelte and SvelteKit (upstream `frameworks/svelte.ts` at `e8ddc4e2`,
//! identical at `b635dd46`): a SvelteKit page's route, and what a component's
//! references mean by Svelte's and SvelteKit's conventions.
//!
//! - Only a `+page.svelte` under a `routes/` directory is a route, named by
//!   its directory (upstream #2131): a `(group)` folder is no part of the URL
//!   and a parameter's `=matcher` no part of its name, so
//!   `src/routes/(app)/blog/[slug=slug]/+page.svelte` is `/blog/:slug`.
//!   `[[lang]]` is `:lang?` and `[...rest]` is `*rest`. A `+layout.svelte` or
//!   `+error.svelte` sits at its page's address and is no second screen; a
//!   `+server.ts` is an endpoint and a `+page.ts` a page's loader.
//! - A Svelte 5 rune (`$state.raw`, `$derived.by`, …) is the compiler's: the
//!   reference is its own caller's, at 1.0.
//! - `$count` in a component reads the store `count` (KEEP-RUST: the store the
//!   component declares itself, the one variable or constant of the name in
//!   its file; upstream takes the first variable of the name anywhere, so
//!   `$page` from `$app/stores` reached any project `page`).
//! - An import of `$lib/x` is the file `src/lib/x` of the importing file's app
//!   (KEEP-RUST: upstream reads `src/lib/` at the repository root, which in a
//!   monorepo is no app's) with the extensions SvelteKit tries, and reaches
//!   that file's own node, as every import edge does (upstream the first node
//!   of the file); `$app/…` and `$env/…` are SvelteKit's own modules.
//! - A capitalized call names a component: the one in the calling file's
//!   directory or below, else the one of that name (KEEP-RUST: several there
//!   is none; upstream takes the first of a name prefix, so `src/ui` reached
//!   `src/ui-kit`).

use std::sync::{Arc, OnceLock};

use codegraph_core::node_id::file_node_id;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::route_href::app_root_for;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// Svelte 5 runes, compiler builtins (`SVELTE_RUNES`).
const SVELTE_RUNES: [&str; 13] = [
    "$state",
    "$state.raw",
    "$state.snapshot",
    "$derived",
    "$derived.by",
    "$effect",
    "$effect.pre",
    "$effect.root",
    "$effect.tracking",
    "$props",
    "$bindable",
    "$inspect",
    "$host",
];

/// The modules SvelteKit itself provides (`SVELTEKIT_MODULE_PREFIXES`).
const SVELTEKIT_MODULE_PREFIXES: [&str; 9] = [
    "$app/navigation",
    "$app/stores",
    "$app/environment",
    "$app/forms",
    "$app/paths",
    "$env/static/private",
    "$env/static/public",
    "$env/dynamic/private",
    "$env/dynamic/public",
];

/// What `$lib/x` may add to `x` to name its file, in SvelteKit's order.
const LIB_EXTENSIONS: [&str; 6] = ["", ".ts", ".js", ".svelte", "/index.ts", "/index.js"];

/// The route a SvelteKit page file serves (`filePathToSvelteKitRoute`), or
/// `None` outside a `routes/` directory: its directory below the first
/// `/routes/`, without `(group)` folders, a parameter named without its
/// `=matcher`.
pub(crate) fn sveltekit_route_path(file_path: &str) -> Option<String> {
    static REST: OnceLock<Regex> = OnceLock::new();
    static OPTIONAL: OnceLock<Regex> = OnceLock::new();
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let normalized = file_path.replace('\\', "/");
    let at = normalized.find("/routes/")?;
    let after = &normalized[at + "/routes/".len()..];
    let dir = after.rfind('/').map_or("", |slash| &after[..slash]);
    let joined = dir
        .split('/')
        .filter(|seg| !seg.is_empty() && !(seg.starts_with('(') && seg.ends_with(')')))
        .collect::<Vec<_>>()
        .join("/");
    let rest = REST.get_or_init(|| Regex::new(r"\[\.\.\.([^\]=]+)(?:=[^\]]*)?\]").expect("rest"));
    let optional =
        OPTIONAL.get_or_init(|| Regex::new(r"\[{2}([^\]=]+)(?:=[^\]]*)?\]{2}").expect("optional"));
    let param = PARAM.get_or_init(|| Regex::new(r"\[([^\]=]+)(?:=[^\]]*)?\]").expect("param"));
    let route = format!(
        "/{}",
        param.replace_all(
            &optional.replace_all(&rest.replace_all(&joined, "*${1}"), ":${1}?"),
            ":${1}"
        )
    );
    if route == "/" {
        return Some(route);
    }
    Some(route.strip_suffix('/').unwrap_or(&route).to_string())
}

/// Whether `name` is a rune (`isRuneReference`).
fn is_rune(name: &str) -> bool {
    SVELTE_RUNES.contains(&name)
}

/// `^[A-Z][a-zA-Z0-9]*$` (`isPascalCase`).
fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.all(|c| c.is_ascii_alphanumeric())
}

/// The store `$store` reads in `file`: the one variable or constant of that
/// name the file declares (KEEP-RUST, see the module comment).
fn own_store(store: &str, file: &str, context: &dyn ResolutionContext) -> Option<String> {
    if !super::js_scan::is_identifier(store) {
        return None;
    }
    let nodes = context.get_nodes_in_file_shared(file);
    let declared: Vec<&Arc<Node>> = nodes
        .iter()
        .filter(|node| {
            node.name == store && matches!(node.kind, NodeKind::Variable | NodeKind::Constant)
        })
        .collect();
    match declared.as_slice() {
        [only] => Some(only.id.clone()),
        _ => None,
    }
}

/// The file node of the module `$lib/{rest}` names from `from_file`: under
/// `src/lib/` of the importing file's app, with the first extension that
/// names an indexed file.
fn lib_module(rest: &str, from_file: &str, context: &dyn ResolutionContext) -> Option<String> {
    let root = app_root_for(from_file);
    LIB_EXTENSIONS.iter().find_map(|extension| {
        let path = format!("{root}src/lib/{rest}{extension}");
        (context.file_exists(&path) && !context.get_nodes_in_file_shared(&path).is_empty())
            .then(|| file_node_id(&path))
    })
}

/// The component a capitalized call in `from_file` names
/// (`resolveComponent`): the one in its directory or below, else the one of
/// that name; several in the directory, or several anywhere, is none
/// (KEEP-RUST, see the module comment).
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
    let dir = from_file
        .rfind('/')
        .map_or("", |slash| &from_file[..=slash]);
    let near: Vec<&Arc<Node>> = components
        .iter()
        .filter(|node| node.file_path.starts_with(dir))
        .collect();
    match (near.as_slice(), components.as_slice()) {
        ([only], _) => Some(only.id.clone()),
        ([], [only]) => Some(only.id.clone()),
        _ => None,
    }
}

/// Svelte and SvelteKit's conventions (`svelteResolver`).
pub struct SvelteResolver;

impl FrameworkResolver for SvelteResolver {
    fn name(&self) -> &str {
        "svelte"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Svelte])
    }

    /// Runes are written in `.svelte.ts` and `.svelte.js` modules too, and a
    /// SvelteKit `+page.ts` or `hooks.server.ts` imports `$lib/…`.
    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&[Language::Svelte, Language::TypeScript, Language::JavaScript])
    }

    /// `svelte` or `@sveltejs/kit` in the root `package.json`, or any
    /// `.svelte` file.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let declared = context
            .read_file("package.json")
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .is_some_and(|manifest| {
                ["dependencies", "devDependencies"].iter().any(|field| {
                    manifest.get(field).is_some_and(|deps| {
                        deps.get("svelte").is_some() || deps.get("@sveltejs/kit").is_some()
                    })
                })
            });
        declared
            || context
                .get_all_files_shared()
                .iter()
                .any(|file| file.ends_with(".svelte"))
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        let to = |target: String, confidence: f64| {
            ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
        };
        // Pattern 1: a rune is the compiler's, never a project symbol.
        if is_rune(name) {
            return Some(to(reference.from_node_id.clone(), 1.0));
        }
        // Pattern 2: a store subscription is a component's syntax only; in
        // any other script `$n` is a name of its own (jQuery's `$el`).
        if reference.language == Language::Svelte
            && let Some(store) = name.strip_prefix('$')
            && !store.starts_with('$')
            && let Some(target) = own_store(store, &reference.file_path, context)
        {
            return Some(to(target, 0.85));
        }
        // Pattern 3: SvelteKit's module aliases.
        if reference.reference_kind == EdgeKind::Imports && name.starts_with('$') {
            if let Some(rest) = name.strip_prefix("$lib/")
                && let Some(target) = lib_module(rest, &reference.file_path, context)
            {
                return Some(to(target, 0.9));
            }
            if SVELTEKIT_MODULE_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                return Some(to(reference.from_node_id.clone(), 1.0));
            }
        }
        // Pattern 4: a capitalized call names a component.
        if reference.reference_kind == EdgeKind::Calls && is_pascal_case(name) {
            return resolve_component(name, &reference.file_path, context)
                .map(|target| to(target, 0.8));
        }
        None
    }

    fn extract(
        &self,
        file_path: &str,
        _content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut result = FrameworkResolverExtractionResult::default();
        let file_name = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
        if file_name != "+page.svelte" {
            return Some(result);
        }
        let Some(route) = sveltekit_route_path(file_path) else {
            return Some(result);
        };
        result.nodes.push(framework_node(
            format!("route:{file_path}:{route}:1"),
            NodeKind::Route,
            route.clone(),
            format!("{file_path}::route:{route}"),
            file_path.to_string(),
            1,
            1,
            0,
            0,
            Language::Svelte,
            false,
        ));
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::{RouteCtx, call_in};

    fn route_names(file: &str) -> Vec<String> {
        SvelteResolver
            .extract(
                file,
                "",
                &FrameworkExtractionContext::without_config("/project"),
            )
            .expect("an extraction")
            .nodes
            .into_iter()
            .filter(|node| node.kind == NodeKind::Route)
            .map(|node| node.name)
            .collect()
    }

    fn node(file: &str, kind: NodeKind, name: &str) -> Node {
        framework_node(
            format!("{}:{file}:{name}", kind.as_str()),
            kind,
            name.to_string(),
            format!("{file}::{name}"),
            file.to_string(),
            1,
            1,
            0,
            0,
            Language::Svelte,
            false,
        )
    }

    fn reference(file: &str, name: &str, kind: EdgeKind, language: Language) -> RefView {
        RefView {
            from_node_id: format!("component:{file}"),
            reference_kind: kind,
            language,
            ..call_in(file, name, 3, 0)
        }
    }

    fn target(context: &RouteCtx, reference: &RefView) -> Option<(String, f64)> {
        SvelteResolver
            .resolve(reference, context)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    /// Upstream `sveltekit-router.test.ts`, "only a +page.svelte is a
    /// route".
    #[test]
    fn only_a_page_is_a_route_named_by_its_directory() {
        assert_eq!(route_names("src/routes/+page.svelte"), ["/"]);
        assert_eq!(route_names("src/routes/login/+page.svelte"), ["/login"]);
        assert_eq!(
            route_names("src/routes/article/[slug]/+page.svelte"),
            ["/article/:slug"]
        );
        for file in [
            "src/routes/+layout.svelte",
            "src/routes/+error.svelte",
            "src/routes/profile/+layout.svelte",
            "src/routes/login/+page.server.js",
            "src/routes/login/+page.ts",
            "src/routes/api/+server.ts",
            "src/lib/+page.svelte",
        ] {
            assert!(route_names(file).is_empty(), "{file}");
        }
    }

    /// Upstream `sveltekit-route-names.test.ts` (#2131), and the optional,
    /// rest and in-segment parameters.
    #[test]
    fn a_route_name_drops_groups_and_matchers() {
        for (file, name) in [
            ("src/routes/(app)/(layout)/blocks/+page.svelte", "/blocks"),
            ("src/routes/(app)/(layout)/+page.svelte", "/"),
            (
                "src/routes/(view)/view/[view=view]/+page.svelte",
                "/view/:view",
            ),
            ("src/routes/docs/[...slug=doc]/+page.svelte", "/docs/*slug"),
            ("src/routes/docs/[...slug]/+page.svelte", "/docs/*slug"),
            ("src/routes/[[lang]]/about/+page.svelte", "/:lang?/about"),
            ("src/routes/[[lang=locale]]/+page.svelte", "/:lang?"),
            ("src/routes/profile/@[user]/+page.svelte", "/profile/@:user"),
            ("src/routes/[a]-[b]/+page.svelte", "/:a-:b"),
            ("apps/web/src/routes/settings/+page.svelte", "/settings"),
            ("src\\routes\\login\\+page.svelte", "/login"),
        ] {
            assert_eq!(route_names(file), [name], "{file}");
        }
    }

    /// A rune is its own caller's; a store is the one its component
    /// declares; `$$props` and a store declared elsewhere are not.
    #[test]
    fn a_rune_is_the_compilers_and_a_store_the_components() {
        let page = "src/routes/+page.svelte";
        let ctx = RouteCtx::default()
            .routes(
                page,
                vec![
                    node(page, NodeKind::Constant, "count"),
                    node(page, NodeKind::Function, "page"),
                ],
            )
            .routes(
                "src/lib/stores.ts",
                vec![node("src/lib/stores.ts", NodeKind::Constant, "user")],
            );
        for rune in ["$state.raw", "$derived.by", "$effect.pre", "$state"] {
            assert_eq!(
                target(
                    &ctx,
                    &reference(page, rune, EdgeKind::Calls, Language::Svelte)
                ),
                Some((format!("component:{page}"), 1.0)),
                "{rune}"
            );
        }
        assert_eq!(
            target(
                &ctx,
                &reference(page, "$count", EdgeKind::References, Language::Svelte)
            ),
            Some((format!("constant:{page}:count"), 0.85))
        );
        for unbound in ["$user", "$page", "$$props", "$count.value"] {
            assert_eq!(
                target(
                    &ctx,
                    &reference(page, unbound, EdgeKind::References, Language::Svelte)
                ),
                None,
                "{unbound}"
            );
        }
        // In a script that is no component, `$user` is a name of its own,
        // though the script declares `user`.
        assert_eq!(
            target(
                &ctx,
                &reference(
                    "src/lib/stores.ts",
                    "$user",
                    EdgeKind::References,
                    Language::TypeScript
                )
            ),
            None
        );
    }

    /// `$lib/x` is the importing file's app's `src/lib/x`, with the first
    /// extension that names a file; `$app/…` and `$env/…` are SvelteKit's.
    #[test]
    fn a_lib_import_is_its_apps_file() {
        let ctx = RouteCtx::default()
            .routes(
                "src/lib/stores.ts",
                vec![node("src/lib/stores.ts", NodeKind::Constant, "count")],
            )
            .routes(
                "src/lib/ui/index.ts",
                vec![node("src/lib/ui/index.ts", NodeKind::Constant, "ui")],
            )
            .routes(
                "apps/web/src/lib/stores.ts",
                vec![node(
                    "apps/web/src/lib/stores.ts",
                    NodeKind::Constant,
                    "count",
                )],
            )
            .routes(
                "src/lib/Button.svelte",
                vec![node("src/lib/Button.svelte", NodeKind::Component, "Button")],
            );
        let import = |file: &str, name: &str| {
            let language = if file.ends_with(".svelte") {
                Language::Svelte
            } else {
                Language::TypeScript
            };
            target(&ctx, &reference(file, name, EdgeKind::Imports, language))
        };
        assert_eq!(
            import("src/hooks.server.ts", "$lib/stores"),
            Some((file_node_id("src/lib/stores.ts"), 0.9))
        );
        assert_eq!(
            import("src/routes/+page.ts", "$lib/ui"),
            Some((file_node_id("src/lib/ui/index.ts"), 0.9))
        );
        assert_eq!(
            import("src/routes/+page.svelte", "$lib/Button.svelte"),
            Some((file_node_id("src/lib/Button.svelte"), 0.9))
        );
        // A monorepo app's `$lib` is its own.
        assert_eq!(
            import("apps/web/src/routes/+page.ts", "$lib/stores"),
            Some((file_node_id("apps/web/src/lib/stores.ts"), 0.9))
        );
        assert_eq!(
            import("apps/admin/src/routes/+page.ts", "$lib/stores"),
            None
        );
        assert_eq!(import("src/hooks.server.ts", "$lib/missing"), None);
        for module in ["$app/navigation", "$env/static/public"] {
            assert_eq!(
                import("src/hooks.server.ts", module),
                Some(("component:src/hooks.server.ts".to_string(), 1.0)),
                "{module}"
            );
        }
        assert_eq!(import("src/hooks.server.ts", "$service-worker"), None);
    }

    /// A capitalized call names the component of its directory or below,
    /// else the one of its name; several of them is none.
    #[test]
    fn a_capitalized_call_names_a_component() {
        let ctx = RouteCtx::default()
            .routes(
                "src/lib/ui/Card.svelte",
                vec![node("src/lib/ui/Card.svelte", NodeKind::Component, "Card")],
            )
            .routes(
                "src/lib/ui-kit/Card.svelte",
                vec![node(
                    "src/lib/ui-kit/Card.svelte",
                    NodeKind::Component,
                    "Card",
                )],
            )
            .routes(
                "src/lib/ui/forms/Field.svelte",
                vec![node(
                    "src/lib/ui/forms/Field.svelte",
                    NodeKind::Component,
                    "Field",
                )],
            )
            .routes(
                "src/lib/other/Field.svelte",
                vec![node(
                    "src/lib/other/Field.svelte",
                    NodeKind::Component,
                    "Field",
                )],
            )
            .routes(
                "src/lib/Alone.svelte",
                vec![node("src/lib/Alone.svelte", NodeKind::Component, "Alone")],
            );
        let call = |file: &str, name: &str| {
            target(
                &ctx,
                &reference(file, name, EdgeKind::Calls, Language::Svelte),
            )
            .map(|(id, _)| id)
        };
        // `src/lib/ui/` holds one `Card`; `src/lib/ui-kit/` is no part of it.
        assert_eq!(
            call("src/lib/ui/List.svelte", "Card").as_deref(),
            Some("component:src/lib/ui/Card.svelte:Card")
        );
        assert_eq!(
            call("src/lib/ui/List.svelte", "Field").as_deref(),
            Some("component:src/lib/ui/forms/Field.svelte:Field")
        );
        assert_eq!(
            call("src/routes/+page.svelte", "Alone").as_deref(),
            Some("component:src/lib/Alone.svelte:Alone")
        );
        // Two of the name under `src/lib/` and none nearer: none.
        assert_eq!(call("src/lib/Grid.svelte", "Card"), None);
        assert_eq!(call("src/routes/+page.svelte", "Card"), None);
        // Not capitalized, or not a call.
        assert_eq!(call("src/lib/ui/List.svelte", "card"), None);
        assert_eq!(
            target(
                &ctx,
                &reference(
                    "src/lib/ui/List.svelte",
                    "Card",
                    EdgeKind::References,
                    Language::Svelte
                )
            ),
            None
        );
    }

    #[test]
    fn svelte_is_detected_by_its_dependency_or_a_component() {
        let manifest = RouteCtx::default().file(
            "package.json",
            r#"{"devDependencies":{"@sveltejs/kit":"2"}}"#,
        );
        assert!(SvelteResolver.detect(&manifest));
        let component = RouteCtx::default().file("src/App.svelte", "<p>hi</p>");
        assert!(SvelteResolver.detect(&component));
        let neither = RouteCtx::default()
            .file("package.json", r#"{"dependencies":{"vue":"3"}}"#)
            .file("src/App.vue", "");
        assert!(!SvelteResolver.detect(&neither));
    }
}
