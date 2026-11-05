//! SvelteKit navigation (upstream `frameworks/sveltekit-router.ts` at
//! `e8ddc4e2`, identical at `b635dd46`): pages are directories, and a
//! navigation is a string.
//!
//! [`super::svelte`] reads the route table out of the file tree; this is the
//! navigation half. Two calls carry a user from one page to another, and they
//! do not agree on where the path goes:
//!
//! ```text
//! goto('/login')                     // $app/navigation, in the browser
//! redirect(303, '/article/' + slug)  // @sveltejs/kit, from a load or an action
//! ```
//!
//! `redirect` takes the status first, so its destination is the second
//! argument. A destination is read only when it is whole (KEEP-RUST, as Vue
//! Router reads it): a string, a template whose holes fill whole segments, a
//! conditional of them, or, for `goto`, a local initialized to one; upstream
//! reads the literal heading the expression, so the call above is read as
//! `/article/` and reaches an `/article` page, and a hole sharing a segment
//! (`` `/profile/@${name}` ``) counts as the segment. `<a href>` is markup,
//! which the link pass reads through [`sveltekit_link_routes`]
//! (`synthesis/links.rs`).
//!
//! Only a `+page.svelte` is a screen. A computed destination, a path no page
//! serves, a path two pages of one app serve (`(a)/x` and `(b)/x`), and an
//! absolute URL (KEEP-RUST: upstream matches its path) are left unresolved.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use super::route_href::{
    ExactTable, HrefLiteral, LinkRoutes, Literal, RouteEntry, app_root_for, destinations_for_href,
    nth_argument_text, parse_href, read_href_via_local_with,
};
use crate::framework::{FrameworkResolver, NavigationCalls};
use crate::types::{AlsoTarget, EdgeShape, RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The languages a navigation is written in (`NAV_LANGUAGES`).
const NAV_LANGUAGES: [Language; 3] = [Language::TypeScript, Language::JavaScript, Language::Svelte];

/// True for a page route [`super::svelte`] extracted, and no other
/// (`isSvelteKitPage`).
fn is_sveltekit_page(node: &Node) -> bool {
    node.language == Language::Svelte
        && node.file_path.ends_with("/+page.svelte")
        && node.id == format!("route:{}:{}:1", node.file_path, node.name)
}

/// True for a page the table holds (`inSvelteKitTable`): `[...rest]` is
/// `*rest`, which matches anything and is never an answer.
fn in_sveltekit_table(node: &Node) -> bool {
    is_sveltekit_page(node) && node.name.starts_with('/') && !node.name.contains('*')
}

/// `:id?`, a parameter SvelteKit serves the route with or without
/// (`isOptionalParam`).
fn is_optional_param(seg: &str) -> bool {
    seg.starts_with(':') && seg.ends_with('?')
}

/// Whether `path` names another site: a scheme (`https:`, `mailto:`) or a
/// protocol-relative `//host`.
pub(crate) fn leaves_the_site(path: &str) -> bool {
    path.starts_with("//")
        || path.split_once(':').is_some_and(|(scheme, _)| {
            !scheme.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        })
}

/// `href` without the arms that leave the site, or `None` when every arm
/// does: the arms that stay are matched as if written alone, so an absolute
/// URL never takes the place of an internal arm naming the same page.
fn on_site(href: &HrefLiteral) -> Option<HrefLiteral> {
    let mut arms = href
        .arms()
        .filter(|arm| !leaves_the_site(&arm.path))
        .map(|arm| HrefLiteral {
            path: arm.path.clone(),
            display: arm.display.clone(),
            alternates: Vec::new(),
        });
    let mut first = arms.next()?;
    first.alternates = arms.collect();
    Some(first)
}

/// Which argument of a navigation call is the destination
/// (`svelteKitHrefArgument`): `redirect` puts the status first.
pub(crate) fn href_argument(name: &str) -> Option<usize> {
    match name {
        "goto" => Some(0),
        "redirect" => Some(1),
        _ => None,
    }
}

/// One app's pages, by path.
#[derive(Debug, Default)]
struct AppPages {
    exact: ExactTable,
    /// The pages with a parameter in the path they serve.
    dynamic: Vec<RouteEntry>,
}

impl AppPages {
    /// `path` served by `node` (`addRouteTo`).
    fn add(&mut self, path: &str, root: &str, node: &Arc<Node>) {
        self.exact
            .insert(path.to_string(), root.to_string(), Arc::clone(node));
        if path.contains(':') {
            self.dynamic.push(RouteEntry::at(Arc::clone(node), path));
        }
    }
}

/// Every SvelteKit app's pages, by the directory the app lives in
/// (`svelteKitTable`).
#[derive(Debug, Default)]
struct SvelteKitTable {
    by_root: BTreeMap<String, AppPages>,
}

impl SvelteKitTable {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut table = Self::default();
        // The paths pages serve as named, by app, and the shorter paths a
        // trailing `[[optional]]` segment lets a page serve too.
        let mut served: BTreeSet<(String, String)> = BTreeSet::new();
        let mut shortened: Vec<(String, String, Arc<Node>)> = Vec::new();
        for node in context.get_nodes_by_kind_shared(NodeKind::Route) {
            if !in_sveltekit_table(&node) {
                continue;
            }
            let root = app_root_for(&node.file_path);
            table
                .by_root
                .entry(root.clone())
                .or_default()
                .add(&node.name, &root, &node);
            served.insert((root.clone(), node.name.clone()));
            // `[[optional]]` is `:x?`: the page serves its path with and
            // without it.
            let mut segs: Vec<&str> = node.name.split('/').skip(1).collect();
            while segs.len() > 1 && segs.last().is_some_and(|seg| is_optional_param(seg)) {
                segs.pop();
                shortened.push((
                    root.clone(),
                    format!("/{}", segs.join("/")),
                    Arc::clone(&node),
                ));
            }
        }
        // A shortened path is its pages' unless a page serves it as named.
        // Every page that shortens to it is registered, so two of them leave
        // it ambiguous (KEEP-RUST: upstream keeps the first in node order).
        for (root, path, node) in shortened {
            if served.contains(&(root.clone(), path.clone())) {
                continue;
            }
            if let Some(app) = table.by_root.get_mut(&root) {
                app.add(&path, &root, &node);
            }
        }
        table
    }

    /// The pages of the app `file_path` belongs to: the longest root above
    /// it (`routesForFile`).
    fn app_of(&self, file_path: &str) -> Option<&AppPages> {
        self.by_root
            .iter()
            .filter(|(root, _)| file_path.starts_with(root.as_str()))
            .max_by_key(|(root, _)| root.len())
            .map(|(_, app)| app)
    }
}

/// The pass's SvelteKit table.
fn sveltekit_table(context: &dyn ResolutionContext) -> Arc<SvelteKitTable> {
    context
        .route_tables()
        .get_or_build("sveltekit-router", || SvelteKitTable::build(context))
}

impl LinkRoutes for SvelteKitTable {
    fn is_empty(&self) -> bool {
        self.by_root.is_empty()
    }

    fn serves(&self, file: &str) -> bool {
        self.app_of(file).is_some_and(|app| !app.exact.is_empty())
    }

    fn destinations<'h>(
        &self,
        file: &str,
        href: &'h HrefLiteral,
    ) -> Vec<(Arc<Node>, &'h HrefLiteral)> {
        self.app_of(file).map_or_else(Vec::new, |app| {
            destinations_for_href(href, &app.exact, &app.dynamic, file)
        })
    }
}

/// The SvelteKit pages an internal `<a href>` reaches, in its own app (W7-18a).
pub(crate) fn sveltekit_link_routes(context: &dyn ResolutionContext) -> Arc<dyn LinkRoutes> {
    sveltekit_table(context)
}

/// SvelteKit's navigation resolver (`svelteKitRouterResolver`).
pub struct SvelteKitRouterResolver;

/// The navigation calls SvelteKit binds to its pages: a page answers the
/// calls of its own app.
struct SvelteKitNavigation;

impl NavigationCalls for SvelteKitNavigation {
    fn tails(&self) -> &[&str] {
        &["goto", "redirect"]
    }

    fn scope(&self, route: &Node, _context: &dyn ResolutionContext) -> Option<Vec<String>> {
        in_sveltekit_table(route).then(|| vec![app_root_for(&route.file_path)])
    }
}

impl FrameworkResolver for SvelteKitRouterResolver {
    fn name(&self) -> &str {
        "sveltekit-router"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&NAV_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        super::package_deps::depends_on(context, &["@sveltejs/kit"])
    }

    fn claims_reference(&self, name: &str) -> bool {
        href_argument(name).is_some()
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&SvelteKitNavigation)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        // Only a call carries a destination.
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let name = reference.reference_name.as_str();
        let index = href_argument(name)?;
        if !NAV_LANGUAGES.contains(&reference.language) {
            return None;
        }
        let table = sveltekit_table(context);
        let app = table.app_of(&reference.file_path)?;
        if app.exact.is_empty() {
            return None;
        }
        let content = context.read_file_shared(&reference.file_path)?;
        let lines: Vec<&str> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let arg = nth_argument_text(&lines, reference.line, reference.column, name, index)?;
        let mut href = parse_href(&arg, Literal::Whole);
        if href.is_none() && index == 0 {
            let start = context
                .get_node_by_id_shared(&reference.from_node_id)
                .filter(|enclosing| enclosing.file_path == reference.file_path)
                .map_or((reference.line - 40).max(1), |enclosing| {
                    enclosing.start_line
                });
            href = read_href_via_local_with(
                &lines,
                reference.line,
                reference.column,
                name,
                start,
                Literal::Whole,
            );
        }
        // An absolute URL leaves the site, whatever its path (KEEP-RUST:
        // upstream matches the path, so `redirect(303,
        // 'https://auth.example.com/login')` reached the app's `/login`).
        let href = on_site(&href?)?;
        // Every arm of a conditional destination is somewhere this call goes:
        // the first is this reference's resolution, the rest ride along.
        let mut targets =
            destinations_for_href(&href, &app.exact, &app.dynamic, &reference.file_path)
                .into_iter();
        let (target, arm) = targets.next()?;
        let metadata = |display: &str| {
            let mut metadata = serde_json::Map::new();
            metadata.insert("href".to_string(), serde_json::json!(display));
            metadata.insert("navMethod".to_string(), serde_json::json!(name));
            metadata
        };
        Some(
            ResolvedRef::new(
                reference.clone(),
                target.id.clone(),
                0.95,
                ResolvedBy::Framework,
            )
            .with_edge(EdgeShape {
                edge_kind: Some(EdgeKind::Navigates),
                metadata: metadata(&arm.display),
                also_targets: targets
                    .map(|(node, arm)| AlsoTarget {
                        target_node_id: node.id.clone(),
                        metadata: metadata(&arm.display),
                    })
                    .collect(),
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::FrameworkExtractionContext;
    use crate::frameworks::route_href::test_ctx::{RouteCtx, call_in};
    use crate::frameworks::svelte::SvelteResolver;

    /// The route nodes [`SvelteResolver`] extracts from `files`.
    fn pages(files: &[&str]) -> Vec<Node> {
        files
            .iter()
            .flat_map(|file| {
                SvelteResolver
                    .extract(
                        file,
                        "",
                        &FrameworkExtractionContext::without_config("/project"),
                    )
                    .expect("an extraction")
                    .nodes
            })
            .collect()
    }

    /// An app of pages, with `source` the file the navigation is written in.
    fn app(page_files: &[&str], file: &str, source: &str) -> RouteCtx {
        let mut ctx = RouteCtx::default().file(file, source);
        for node in pages(page_files) {
            let file = node.file_path.clone();
            ctx = ctx.routes(&file, vec![node]);
        }
        ctx
    }

    fn navigation(
        ctx: &RouteCtx,
        file: &str,
        name: &str,
        line: i64,
    ) -> Option<(
        String,
        Vec<String>,
        serde_json::Map<String, serde_json::Value>,
    )> {
        let language = if file.ends_with(".svelte") {
            Language::Svelte
        } else {
            Language::TypeScript
        };
        let reference = RefView {
            language,
            ..call_in(file, name, line, 0)
        };
        SvelteKitRouterResolver
            .resolve(&reference, ctx)
            .map(|resolved| {
                let edge = *resolved.edge.expect("a navigation edge");
                assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
                assert_eq!(resolved.confidence, 0.95);
                (
                    resolved.target_node_id,
                    edge.also_targets
                        .into_iter()
                        .map(|also| also.target_node_id)
                        .collect(),
                    edge.metadata,
                )
            })
    }

    const PAGES: [&str; 6] = [
        "src/routes/+page.svelte",
        "src/routes/login/+page.svelte",
        "src/routes/settings/+page.svelte",
        "src/routes/article/[slug]/+page.svelte",
        "src/routes/(marketing)/about/+page.svelte",
        "src/routes/docs/[...path]/+page.svelte",
    ];

    /// Upstream `sveltekit-router.test.ts`, "which argument carries the
    /// path".
    #[test]
    fn goto_takes_the_path_first_and_redirect_after_the_status() {
        assert_eq!(href_argument("goto"), Some(0));
        assert_eq!(href_argument("redirect"), Some(1));
        assert_eq!(href_argument("push"), None);
        let server = "src/routes/settings/+page.server.js";
        let ctx = app(
            &PAGES,
            server,
            "import { redirect } from '@sveltejs/kit'\nexport function load({ locals }) {\n  if (!locals.user) redirect(302, '/login')\n}\n",
        );
        let (target, also, metadata) =
            navigation(&ctx, server, "redirect", 3).expect("a navigation");
        assert_eq!(target, ctx.route("/login", "src/routes/login/+page.svelte"));
        assert!(also.is_empty());
        assert_eq!(metadata.get("href"), Some(&serde_json::json!("/login")));
        assert_eq!(
            metadata.get("navMethod"),
            Some(&serde_json::json!("redirect"))
        );
        // Only a call navigates: the same site as any other kind of
        // reference is none.
        let value = RefView {
            reference_kind: EdgeKind::References,
            ..call_in(server, "redirect", 3, 0)
        };
        assert!(SvelteKitRouterResolver.resolve(&value, &ctx).is_none());
        assert!(SvelteKitRouterResolver.claims_reference("goto"));
        assert!(!SvelteKitRouterResolver.claims_reference("navigate"));
    }

    /// A template hole fills a parameter's segment; a group folder is no
    /// part of the address; a conditional goes to each arm's page.
    #[test]
    fn a_navigation_reaches_the_page_its_path_names() {
        let editor = "src/routes/editor/+page.svelte";
        let source = "<script>\n  import { goto } from '$app/navigation'\n  async function publish(article) {\n    goto(`/article/${article.slug}`)\n    goto('/about')\n    goto(ok ? '/settings' : '/login')\n    const next = '/settings'\n    goto(next)\n  }\n</script>\n<button on:click={() => goto('/')}>Home</button>\n";
        let ctx = app(&PAGES, editor, source);
        let at = |line: i64| navigation(&ctx, editor, "goto", line);
        let (slug, _, metadata) = at(4).expect("the slug page");
        assert_eq!(
            slug,
            ctx.route("/article/:slug", "src/routes/article/[slug]/+page.svelte")
        );
        assert_eq!(
            metadata.get("href"),
            Some(&serde_json::json!("/article/${…}"))
        );
        assert_eq!(
            at(5).map(|(target, _, _)| target),
            Some(ctx.route("/about", "src/routes/(marketing)/about/+page.svelte"))
        );
        let (first, also, _) = at(6).expect("both arms");
        assert_eq!(
            first,
            ctx.route("/settings", "src/routes/settings/+page.svelte")
        );
        assert_eq!(also, [ctx.route("/login", "src/routes/login/+page.svelte")]);
        assert_eq!(
            at(8).map(|(target, _, _)| target),
            Some(ctx.route("/settings", "src/routes/settings/+page.svelte"))
        );
        assert_eq!(
            at(11).map(|(target, _, _)| target),
            Some(ctx.route("/", "src/routes/+page.svelte"))
        );
    }

    /// What is left unresolved: a computed destination, a concatenation, a
    /// hole sharing a segment, a path no page serves, a rest page, an
    /// absolute or protocol-relative URL whose path a page serves, a
    /// relative path, `redirect`'s first argument, and its local.
    #[test]
    fn a_destination_is_read_only_when_it_is_whole() {
        let server = "src/routes/nowhere/+page.server.js";
        let source = "export function load({ url }) {\n  redirect(307, '/no-such-page')\n  redirect(307, url.searchParams.get('next'))\n  redirect(303, '/article/' + slug)\n  redirect(303, `/articl${e}`)\n  redirect(303, '/docs/a/b')\n  redirect(303, 'https://example.com/login')\n  redirect(303, '//example.com/login')\n  redirect(303, 'login')\n  redirect('/login')\n  const next = '/login'\n  redirect(303, next)\n}\n";
        let ctx = app(
            &[
                PAGES.as_slice(),
                &[
                    "src/routes/article/+page.svelte",
                    "src/routes/articles/+page.svelte",
                ],
            ]
            .concat(),
            server,
            source,
        );
        for line in 2..=12 {
            assert_eq!(
                navigation(&ctx, server, "redirect", line),
                None,
                "line {line}"
            );
        }
    }

    /// `[[lang]]` serves the path with and without it, unless a page serves
    /// the shorter path; a path two pages of one app serve is none, whether
    /// both name it or both shorten to it, in either order.
    #[test]
    fn an_optional_parameter_serves_the_shorter_path_too() {
        let nav = "src/routes/nav.ts";
        let source = "goto('/shop')\ngoto('/blog/en')\ngoto('/blog')\ngoto('/x')\ngoto('/menu')\n";
        let mut pages = vec![
            "src/routes/shop/[[lang]]/+page.svelte",
            "src/routes/blog/[[lang]]/+page.svelte",
            "src/routes/blog/+page.svelte",
            "src/routes/(a)/x/+page.svelte",
            "src/routes/(b)/x/+page.svelte",
            "src/routes/menu/[[lang]]/+page.svelte",
            "src/routes/(m)/menu/[[tab]]/+page.svelte",
        ];
        let ctx = app(&pages, nav, source);
        pages.reverse();
        let reversed = app(&pages, nav, source);
        for line in [4, 5] {
            assert_eq!(
                navigation(&reversed, nav, "goto", line),
                None,
                "line {line}"
            );
        }
        let at = |line: i64| navigation(&ctx, nav, "goto", line).map(|(target, _, _)| target);
        assert_eq!(
            at(1),
            Some(ctx.route("/shop/:lang?", "src/routes/shop/[[lang]]/+page.svelte"))
        );
        assert_eq!(
            at(2),
            Some(ctx.route("/blog/:lang?", "src/routes/blog/[[lang]]/+page.svelte"))
        );
        assert_eq!(
            at(3),
            Some(ctx.route("/blog", "src/routes/blog/+page.svelte"))
        );
        assert_eq!(at(4), None);
        assert_eq!(at(5), None);
    }

    /// An arm that leaves the site drops out before the arms are matched:
    /// the internal arm keeps its page in either order, and a conditional
    /// of external URLs is none.
    #[test]
    fn an_external_arm_leaves_its_page_to_the_internal_arm() {
        let settings = "src/routes/settings/+page.svelte";
        let source = "<script>\n  goto(sso ? 'https://auth.example.com/login' : '/login')\n  goto(sso ? '/login' : 'https://auth.example.com/login')\n  goto(sso ? 'https://auth.example.com/login' : '//auth.example.com/login')\n</script>\n";
        let ctx = app(&PAGES, settings, source);
        let login = ctx.route("/login", "src/routes/login/+page.svelte");
        for line in [2, 3] {
            let (target, also, metadata) =
                navigation(&ctx, settings, "goto", line).expect("the login page");
            assert_eq!(target, login, "line {line}");
            assert!(also.is_empty(), "line {line}");
            assert_eq!(
                metadata.get("href"),
                Some(&serde_json::json!("/login")),
                "line {line}"
            );
        }
        assert_eq!(navigation(&ctx, settings, "goto", 4), None);
    }

    /// A call matches only its own app's pages, and none outside every
    /// SvelteKit app.
    #[test]
    fn a_navigation_matches_only_its_own_apps_pages() {
        let web = "apps/web/src/routes/+page.svelte";
        let ctx = app(
            &[
                "apps/web/src/routes/login/+page.svelte",
                "apps/admin/src/routes/users/+page.svelte",
            ],
            web,
            "<script>\n  goto('/login')\n  goto('/users')\n</script>\n",
        )
        .file("tools/script.ts", "goto('/login')\n");
        assert_eq!(
            navigation(&ctx, web, "goto", 2).map(|(target, _, _)| target),
            Some(ctx.route("/login", "apps/web/src/routes/login/+page.svelte"))
        );
        assert_eq!(navigation(&ctx, web, "goto", 3), None);
        assert_eq!(navigation(&ctx, "tools/script.ts", "goto", 1), None);
    }

    /// The navigation hook scopes a page to its app; a layout, an error page
    /// and a rest page answer no call.
    #[test]
    fn a_page_answers_the_navigation_calls_of_its_app() {
        let navigation = SvelteKitRouterResolver
            .navigation()
            .expect("a navigation hook");
        assert_eq!(navigation.tails(), ["goto", "redirect"]);
        let ctx = RouteCtx::default();
        let [page, rest] = [
            "apps/web/src/routes/login/+page.svelte",
            "apps/web/src/routes/docs/[...path]/+page.svelte",
        ]
        .map(|file| pages(&[file]).pop().expect("a page"));
        assert_eq!(
            navigation.scope(&page, &ctx),
            Some(vec!["apps/web/".to_string()])
        );
        assert_eq!(navigation.scope(&rest, &ctx), None);
        let layout = Node {
            file_path: "apps/web/src/routes/login/+layout.svelte".to_string(),
            id: "route:apps/web/src/routes/login/+layout.svelte:/login:1".to_string(),
            ..page
        };
        assert_eq!(navigation.scope(&layout, &ctx), None);
    }

    #[test]
    fn sveltekit_is_detected_by_its_dependency() {
        let kit = RouteCtx::default().file(
            "package.json",
            r#"{"devDependencies":{"@sveltejs/kit":"2"}}"#,
        );
        assert!(SvelteKitRouterResolver.detect(&kit));
        let plain =
            RouteCtx::default().file("package.json", r#"{"devDependencies":{"svelte":"5"}}"#);
        assert!(!SvelteKitRouterResolver.detect(&plain));
    }
}
