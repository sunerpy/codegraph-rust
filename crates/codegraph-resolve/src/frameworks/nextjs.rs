//! Next.js [`FrameworkResolver`]: file-based pages and route handlers, and
//! string-keyed navigation. Ports `upstream resolution/frameworks/nextjs.ts`
//! at `b635dd46`, which upstream split out of `react.ts` (F3).
//!
//! Two things static extraction cannot see on its own, and that together are
//! most of what "how does the site flow" means in a Next app:
//!
//! 1. **A page is a file.** `app/users/page.tsx` is `/users`,
//!    `app/(marketing)/about/page.tsx` is `/about` (a `(group)` is invisible
//!    in the URL), `app/blog/[slug]/page.tsx` is `/blog/:slug`,
//!    `app/docs/[...all]/page.tsx` is `/docs/:all*`; the Pages Router's
//!    `pages/about.tsx` is `/about`. [`NextjsResolver::extract`] emits one
//!    `route` node per page, named by its path, with a `calls` reference to
//!    the file's default export, so the route reaches the component that
//!    renders it. `app/api/users/route.ts` exports `GET` / `POST` / …: one
//!    route per method, `POST /api/users`, with a `references` reference to
//!    that function; `pages/api/users.ts` is `ANY /api/users`, bound to its
//!    default export.
//!
//! 2. **Navigation is a string.** `router.push('/users')` (`next/navigation`,
//!    `next/router`), `redirect('/login')` / `permanentRedirect` in a server
//!    action or a page, `NextResponse.redirect(new URL('/login', req.url))` in
//!    the middleware or a route handler: extraction records each as a call
//!    that resolves to nothing, because the target is a path.
//!    [`NextjsResolver::resolve`] claims those calls, reads the argument off
//!    the source ([`super::route_href`]), matches it against this framework's
//!    own route table and returns a `navigates` edge carrying the href. A
//!    `<Link href>` is a JSX attribute, not a call: the link synthesis reads
//!    it through [`next_link_routes`] (`synthesis/links.rs`).
//!
//! Precision rests on the string resolving to a real page: a computed href, a
//! path no page serves, a relative href, or a conditional none of whose arms
//! names a page stays unresolved. Parallel (`@slot`) and intercepting
//! (`(.)photo`) routes are not modelled; `layout` / `loading` / `error` /
//! `template` files are not routes.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::route_href::{
    ExactTable, HrefLiteral, LinkRoutes, RouteEntry, destinations_for_href, first_argument_text,
    parse_href_expression, read_href_via_local,
};
use super::{framework_node, js_language_for};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver, NavigationCalls};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// The languages Next pages, handlers and navigation calls are written in.
const ROUTE_LANGUAGES: [Language; 4] = [
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];

/// The methods a route handler file exports.
const HTTP_EXPORTS: &str = "GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS";

/// What a file is to the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextRouteKind {
    /// A page component.
    Page,
    /// An App Router `route.ts` handler file.
    Handler,
    /// A Pages Router API file.
    Api,
}

/// A file the router serves (`NextRouteFile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextRouteFile {
    pub kind: NextRouteKind,
    /// `/blog/:slug`: the path, in the form every other framework's routes
    /// use.
    pub path: String,
    /// The directory the Next app lives in (`""`, `apps/web/`): what its
    /// navigation calls are gated on.
    pub root: String,
}

/// `[slug]` → `:slug`, `[...all]` / `[[...all]]` → `:all*`; anything else as
/// written (`nextSegment`).
fn next_segment(seg: &str) -> String {
    let inner = |prefix: &str, suffix: &str| {
        seg.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .filter(|name| !name.is_empty() && !name.contains(']'))
    };
    if let Some(name) = inner("[[...", "]]") {
        return format!(":{name}*");
    }
    if let Some(name) = inner("[...", "]") {
        return format!(":{name}*");
    }
    if let Some(name) = inner("[", "]") {
        return format!(":{name}");
    }
    seg.to_string()
}

/// What a file is to the router, or `None` for a file that is not a route
/// (`nextRouteForFile`).
pub fn next_route_for_file(file_path: &str) -> Option<NextRouteFile> {
    static EXCLUDED: OnceLock<Regex> = OnceLock::new();
    static APP: OnceLock<Regex> = OnceLock::new();
    static APP_FILE: OnceLock<Regex> = OnceLock::new();
    static PAGES: OnceLock<Regex> = OnceLock::new();
    static PAGE_EXT: OnceLock<Regex> = OnceLock::new();
    static NOT_A_PAGE: OnceLock<Regex> = OnceLock::new();
    if EXCLUDED
        .get_or_init(|| {
            Regex::new(r"(?:^|/)(?:__tests__|__mocks__|node_modules)/").expect("excluded")
        })
        .is_match(file_path)
    {
        return None;
    }
    if let Some(app) = APP
        .get_or_init(|| Regex::new(r"^((?:[^/]+/)*?)(?:src/)?app/(.+)$").expect("app dir"))
        .captures(file_path)
    {
        let file = APP_FILE
            .get_or_init(|| {
                Regex::new(r"^(.*?)(?:^|/)(page|route)\.(?:tsx|ts|jsx|js|mjs|cjs|mdx?)$")
                    .expect("app file")
            })
            .captures(app.get(2).expect("app rest").as_str())?;
        let segs: Vec<&str> = file[1].split('/').filter(|seg| !seg.is_empty()).collect();
        // Parallel and intercepting routes are a picture of their own; not
        // modelled.
        if segs
            .iter()
            .any(|seg| seg.starts_with('@') || intercepts(seg))
        {
            return None;
        }
        let kept: Vec<String> = segs
            .iter()
            .filter(|seg| !(seg.starts_with('(') && seg.ends_with(')')))
            .map(|seg| next_segment(seg))
            .collect();
        return Some(NextRouteFile {
            kind: if &file[2] == "page" {
                NextRouteKind::Page
            } else {
                NextRouteKind::Handler
            },
            path: format!("/{}", kept.join("/")),
            root: app[1].to_string(),
        });
    }
    let pages = PAGES
        .get_or_init(|| Regex::new(r"^((?:[^/]+/)*?)(?:src/)?pages/(.+)$").expect("pages dir"))
        .captures(file_path)?;
    let rel = pages.get(2).expect("pages rest").as_str();
    let ext = PAGE_EXT
        .get_or_init(|| Regex::new(r"\.(?:tsx|ts|jsx|js|mjs|cjs|mdx?)$").expect("page ext"))
        .find(rel)?;
    let bare = &rel[..ext.start()];
    let mut segs: Vec<&str> = bare.split('/').collect();
    let base = segs.last().copied().unwrap_or_default();
    if base.starts_with('_')
        || NOT_A_PAGE
            .get_or_init(|| Regex::new(r"\.(?:test|spec|stories|config|d)$").expect("not a page"))
            .is_match(bare)
    {
        return None;
    }
    if base == "index" {
        segs.pop();
    }
    let kind = if segs.first() == Some(&"api") {
        NextRouteKind::Api
    } else {
        NextRouteKind::Page
    };
    Some(NextRouteFile {
        kind,
        path: format!(
            "/{}",
            segs.iter()
                .map(|seg| next_segment(seg))
                .collect::<Vec<_>>()
                .join("/")
        ),
        root: pages[1].to_string(),
    })
}

/// An intercepting segment: `(.)photo`, `(..)photo`, `(...)photo`
/// (`/^\(\.{1,3}\)/`).
fn intercepts(seg: &str) -> bool {
    let Some(rest) = seg.strip_prefix('(') else {
        return false;
    };
    let dots = rest.bytes().take_while(|&b| b == b'.').count();
    (1..=3).contains(&dots) && rest.as_bytes().get(dots) == Some(&b')')
}

/// The page a route node is, or `None` for a node the table leaves out (a
/// handler, another framework's route) (`tablePage`).
fn table_page(node: &Node) -> Option<NextRouteFile> {
    next_route_for_file(&node.file_path)
        .filter(|file| file.kind == NextRouteKind::Page && file.path == node.name)
}

/// This framework's pages, matched the Expo Router way (`NextTable`).
#[derive(Debug, Default)]
struct NextTable {
    /// Every page by its path, with its app's root.
    exact: ExactTable,
    /// The pages with a parameter or catch-all segment. Matching is
    /// order-independent: a tie is nothing.
    dynamic: Vec<RouteEntry>,
    /// The directories Next apps live in: a navigation call is only read from
    /// under one.
    roots: Vec<String>,
}

impl NextTable {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut table = Self::default();
        let mut roots = BTreeSet::new();
        for node in context.get_nodes_by_kind_shared(NodeKind::Route) {
            let Some(file) = table_page(&node) else {
                continue;
            };
            if node.name.contains(':') {
                table.dynamic.push(RouteEntry::new(Arc::clone(&node)));
            }
            table
                .exact
                .insert(node.name.clone(), file.root.clone(), node);
            roots.insert(file.root);
        }
        table.roots = roots.into_iter().collect();
        table
    }
}

/// The pass's Next route table (`nextRouteTable`).
fn next_route_table(context: &dyn ResolutionContext) -> Arc<NextTable> {
    context
        .route_tables()
        .get_or_build("nextjs", || NextTable::build(context))
}

/// One table for every Next app: a link is read from under any app's root
/// and reaches any page, its own app's first (`nextLinkEdges`).
impl LinkRoutes for NextTable {
    fn is_empty(&self) -> bool {
        self.exact.is_empty()
    }

    fn serves(&self, file: &str) -> bool {
        self.roots
            .iter()
            .any(|root| file.starts_with(root.as_str()))
    }

    fn destinations<'h>(
        &self,
        file: &str,
        href: &'h HrefLiteral,
    ) -> Vec<(Arc<Node>, &'h HrefLiteral)> {
        destinations_for_href(href, &self.exact, &self.dynamic, file)
    }
}

/// The Next pages a `<Link href>` reaches (W7-18a).
pub(crate) fn next_link_routes(context: &dyn ResolutionContext) -> Arc<dyn LinkRoutes> {
    next_route_table(context)
}

/// `router.push` / `.replace` / `.prefetch`, `redirect` /
/// `permanentRedirect`, `NextResponse.redirect` (`NAV_CALL`).
fn nav_call() -> &'static Regex {
    static NAV_CALL: OnceLock<Regex> = OnceLock::new();
    NAV_CALL.get_or_init(|| {
        Regex::new(
            r"(?:^|\.)(push|replace|prefetch)$|^(redirect|permanentRedirect)$|^(?:NextResponse|Response)\.(redirect)$",
        )
        .expect("navigation call")
    })
}

/// The verb a navigation call name stands for, or `None` (`nextNavVerb`).
pub fn next_nav_verb(name: &str) -> Option<&'static str> {
    let captures = nav_call().captures(name)?;
    if captures.get(3).is_some() {
        return Some("response.redirect");
    }
    let verb = captures.get(1).or_else(|| captures.get(2))?.as_str();
    [
        "push",
        "replace",
        "prefetch",
        "redirect",
        "permanentRedirect",
    ]
    .into_iter()
    .find(|known| *known == verb)
}

/// Next.js resolver (`nextjsResolver`).
pub struct NextjsResolver;

/// The navigation calls Next binds to its pages (`navigation`): one table
/// holds every Next app's pages, and a call under any of them matches
/// against all of it.
struct NextNavigation;

impl NavigationCalls for NextNavigation {
    fn tails(&self) -> &[&str] {
        &[
            "push",
            "replace",
            "prefetch",
            "redirect",
            "permanentRedirect",
        ]
    }

    fn scope(&self, route: &Node, context: &dyn ResolutionContext) -> Option<Vec<String>> {
        table_page(route).map(|_| next_route_table(context).roots.clone())
    }
}

impl FrameworkResolver for NextjsResolver {
    fn name(&self) -> &str {
        "nextjs"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&ROUTE_LANGUAGES)
    }

    fn app_dependencies(&self) -> Option<&'static [&'static str]> {
        Some(&["next"])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        static CONFIG: OnceLock<Regex> = OnceLock::new();
        if super::package_deps::depends_on(context, &["next"]) {
            return true;
        }
        let files = context.get_all_files_shared();
        let config = CONFIG
            .get_or_init(|| Regex::new(r"(?:^|/)next\.config\.[cm]?[jt]s$").expect("next config"));
        files.iter().any(|file| config.is_match(file))
            && files.iter().any(|file| next_route_for_file(file).is_some())
    }

    fn claims_reference(&self, name: &str) -> bool {
        nav_call().is_match(name)
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&NextNavigation)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut result = FrameworkResolverExtractionResult::default();
        let Some(file) = next_route_for_file(file_path) else {
            return Some(result);
        };
        let language = js_language_for(file_path);
        let stripped = strip_comments_for_regex(content, CommentLang::TypeScript);
        let line_of = |index: usize| stripped[..index].matches('\n').count() as i64 + 1;
        let reference = |from: &str, name: &str, kind: EdgeKind, line: i64| RefView {
            row_id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line,
            column: 0,
            file_path: file_path.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        };

        if file.kind == NextRouteKind::Handler {
            // `export async function GET(req) {…}` / `export const POST = …`:
            // one route per method.
            static DECL: OnceLock<Regex> = OnceLock::new();
            let decl = DECL.get_or_init(|| {
                Regex::new(&format!(
                    r"\bexport\s+(?:async\s+)?function\s+({HTTP_EXPORTS})\b|\bexport\s+(?:const|let)\s+({HTTP_EXPORTS})\s*="
                ))
                .expect("handler export")
            });
            let mut seen = HashSet::new();
            for captures in decl.captures_iter(&stripped) {
                let whole = captures.get(0).expect("whole match");
                let method = captures
                    .get(1)
                    .or_else(|| captures.get(2))
                    .expect("method")
                    .as_str();
                if !seen.insert(method) {
                    continue;
                }
                let line = line_of(whole.start());
                let node = framework_node(
                    format!("route:{file_path}:{line}:{method}:{}", file.path),
                    NodeKind::Route,
                    format!("{method} {}", file.path),
                    format!("{file_path}::{method}:{}", file.path),
                    file_path.to_string(),
                    line,
                    line,
                    0,
                    whole.len() as i64,
                    language,
                    true,
                );
                result
                    .references
                    .push(reference(&node.id, method, EdgeKind::References, line));
                result.nodes.push(node);
            }
            return Some(result);
        }

        // A page, or a Pages Router API file: the default export is what
        // runs.
        let api = file.kind == NextRouteKind::Api;
        let node = framework_node(
            if api {
                format!("route:{file_path}:1:ANY:{}", file.path)
            } else {
                format!("route:{file_path}:{}", file.path)
            },
            NodeKind::Route,
            if api {
                format!("ANY {}", file.path)
            } else {
                file.path.clone()
            },
            if api {
                format!("{file_path}::ANY:{}", file.path)
            } else {
                format!("{file_path}::route:{}", file.path)
            },
            file_path.to_string(),
            1,
            1,
            0,
            0,
            language,
            true,
        );
        if let Some((name, index)) = super::route_href::default_export_name(&stripped) {
            result.references.push(reference(
                &node.id,
                &name,
                if api {
                    EdgeKind::References
                } else {
                    EdgeKind::Calls
                },
                line_of(index),
            ));
        }
        result.nodes.push(node);
        Some(result)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let verb = next_nav_verb(&reference.reference_name)?;
        if !ROUTE_LANGUAGES.contains(&reference.language) {
            return None;
        }
        let table = next_route_table(context);
        if table.exact.is_empty()
            || !table
                .roots
                .iter()
                .any(|root| reference.file_path.starts_with(root.as_str()))
        {
            return None;
        }
        let name = &reference.reference_name;
        let callee = name.rsplit('.').next().unwrap_or(name);
        let content = context.read_file_shared(&reference.file_path)?;
        let lines: Vec<&str> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();

        static NEW_URL: OnceLock<Regex> = OnceLock::new();
        let arg = first_argument_text(&lines, reference.line, reference.column, callee)?;
        // `NextResponse.redirect(new URL('/login', req.url))`: the path is the
        // URL's first argument.
        let arg = if NEW_URL
            .get_or_init(|| Regex::new(r"^\s*new\s+URL\s*\(").expect("new URL"))
            .is_match(&arg)
        {
            first_argument_text(&[arg.as_str()], 1, 0, "URL")
        } else {
            Some(arg)
        };
        let mut href = arg.as_deref().and_then(parse_href_expression);
        if href.is_none() {
            let start = context
                .get_node_by_id_shared(&reference.from_node_id)
                .filter(|enclosing| enclosing.file_path == reference.file_path)
                .map_or((reference.line - 40).max(1), |enclosing| {
                    enclosing.start_line
                });
            href = read_href_via_local(&lines, reference.line, reference.column, callee, start);
        }
        let href = href?;
        // Every arm of a conditional destination is somewhere this call goes:
        // the first is this reference's resolution, the rest ride as
        // `also_targets`.
        let mut targets =
            destinations_for_href(&href, &table.exact, &table.dynamic, &reference.file_path)
                .into_iter();
        let (target, arm) = targets.next()?;
        let metadata = |arm: &HrefLiteral| {
            let mut metadata = serde_json::Map::new();
            metadata.insert("href".to_string(), serde_json::json!(arm.display));
            metadata.insert("navMethod".to_string(), serde_json::json!(verb));
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
                metadata: metadata(arm),
                also_targets: targets
                    .map(|(node, arm)| AlsoTarget {
                        target_node_id: node.id.clone(),
                        metadata: metadata(arm),
                    })
                    .collect(),
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::{RouteCtx, call_in};

    fn extract(file: &str, content: &str) -> FrameworkResolverExtractionResult {
        NextjsResolver
            .extract(
                file,
                content,
                &FrameworkExtractionContext::without_config(""),
            )
            .expect("extract")
    }

    /// Upstream `nextjs.test.ts`, `nextRouteForFile`.
    #[test]
    fn a_file_is_the_route_its_path_names() {
        use NextRouteKind::{Api, Handler, Page};
        for (file, kind, path, root) in [
            ("app/page.tsx", Page, "/", ""),
            ("src/app/users/page.tsx", Page, "/users", ""),
            (
                "apps/web/app/(marketing)/about/page.tsx",
                Page,
                "/about",
                "apps/web/",
            ),
            ("app/blog/[slug]/page.tsx", Page, "/blog/:slug", ""),
            ("app/docs/[...all]/page.tsx", Page, "/docs/:all*", ""),
            ("app/docs/[[...all]]/page.jsx", Page, "/docs/:all*", ""),
            ("app/api/users/route.ts", Handler, "/api/users", ""),
            ("app/api/users/[id]/route.ts", Handler, "/api/users/:id", ""),
            ("pages/index.tsx", Page, "/", ""),
            ("pages/about.tsx", Page, "/about", ""),
            ("src/pages/blog/[slug].tsx", Page, "/blog/:slug", ""),
            ("pages/api/users.ts", Api, "/api/users", ""),
            (
                "apps/web/pages/api/users/[id].ts",
                Api,
                "/api/users/:id",
                "apps/web/",
            ),
            (
                "app/(commonLayout)/app/(appDetailLayout)/[appId]/logs/page.tsx",
                Page,
                "/app/:appId/logs",
                "",
            ),
        ] {
            assert_eq!(
                next_route_for_file(file),
                Some(NextRouteFile {
                    kind,
                    path: path.to_string(),
                    root: root.to_string(),
                }),
                "{file}"
            );
        }
        for file in [
            "app/layout.tsx",
            "app/loading.tsx",
            "app/users/error.tsx",
            "app/@modal/photo/page.tsx",
            "app/(.)photo/[id]/page.tsx",
            "app/(...)photo/page.tsx",
            "pages/_app.tsx",
            "pages/_document.tsx",
            "src/pages/vite.config.ts",
            "apps/nextjs-pages/next.config.mjs",
            "app/users/__tests__/page.tsx",
            "src/components/button.tsx",
            "pages/about.test.tsx",
            "pages/types.d.ts",
        ] {
            assert_eq!(next_route_for_file(file), None, "{file}");
        }
    }

    /// Upstream `nextjs.test.ts`, `extract`.
    #[test]
    fn a_page_is_a_route_calling_its_default_export() {
        let result = extract(
            "app/users/page.tsx",
            "export default function UsersPage() {\n  return null\n}\n",
        );
        assert_eq!(result.nodes.len(), 1);
        let route = &result.nodes[0];
        assert_eq!(
            (route.kind, route.name.as_str(), route.language),
            (NodeKind::Route, "/users", Language::Tsx)
        );
        assert_eq!(route.id, "route:app/users/page.tsx:/users");
        assert_eq!(route.qualified_name, "app/users/page.tsx::route:/users");
        assert_eq!(result.references.len(), 1);
        let reference = &result.references[0];
        assert_eq!(
            (
                reference.from_node_id.as_str(),
                reference.reference_name.as_str(),
                reference.reference_kind,
                reference.line
            ),
            (route.id.as_str(), "UsersPage", EdgeKind::Calls, 1)
        );
        // A page whose default export has no name is still a route.
        let anonymous = extract("pages/about.jsx", "export default () => null\n");
        assert_eq!(anonymous.nodes[0].name, "/about");
        assert_eq!(anonymous.nodes[0].language, Language::Jsx);
        assert!(anonymous.references.is_empty());
    }

    #[test]
    fn a_route_handler_is_one_endpoint_per_exported_method() {
        let src = "import { NextResponse } from 'next/server'\nexport async function GET() {\n  return NextResponse.json([])\n}\nexport const POST = async (req) => {\n  return NextResponse.json({}, { status: 201 })\n}\n// export function DELETE() {}\nexport function GET2() {}\n";
        let result = extract("app/api/users/route.ts", src);
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["GET /api/users", "POST /api/users"]);
        let lines: Vec<i64> = result.nodes.iter().map(|n| n.start_line).collect();
        assert_eq!(lines, [2, 5]);
        assert_eq!(
            result.nodes[0].id,
            "route:app/api/users/route.ts:2:GET:/api/users"
        );
        assert_eq!(
            result.nodes[0].qualified_name,
            "app/api/users/route.ts::GET:/api/users"
        );
        let refs: Vec<(&str, EdgeKind)> = result
            .references
            .iter()
            .map(|r| (r.reference_name.as_str(), r.reference_kind))
            .collect();
        assert_eq!(
            refs,
            [
                ("GET", EdgeKind::References),
                ("POST", EdgeKind::References)
            ]
        );
        // One endpoint per method, however often the file names it.
        let twice = extract(
            "app/api/x/route.ts",
            "export async function GET() {}\nexport async function GET() {}\n",
        );
        assert_eq!(twice.nodes.len(), 1);
    }

    #[test]
    fn a_pages_api_file_is_any_on_its_path() {
        let result = extract(
            "pages/api/users.ts",
            "export default async function handler(req, res) {\n  res.status(200).json([])\n}\n",
        );
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["ANY /api/users"]);
        assert_eq!(
            result.nodes[0].id,
            "route:pages/api/users.ts:1:ANY:/api/users"
        );
        assert_eq!(result.references[0].reference_name, "handler");
        assert_eq!(result.references[0].reference_kind, EdgeKind::References);
    }

    #[test]
    fn a_layout_or_a_component_file_is_no_route() {
        assert!(
            extract("app/layout.tsx", "export default function L() {}")
                .nodes
                .is_empty()
        );
        assert!(
            extract("components/nav.tsx", "export default function Nav() {}")
                .nodes
                .is_empty()
        );
    }

    /// Upstream `nextjs.test.ts`, the navigation verbs.
    #[test]
    fn navigation_calls_are_claimed_and_name_their_verb() {
        for (name, verb) in [
            ("router.push", Some("push")),
            ("router.replace", Some("replace")),
            ("this.router.prefetch", Some("prefetch")),
            ("redirect", Some("redirect")),
            ("permanentRedirect", Some("permanentRedirect")),
            ("NextResponse.redirect", Some("response.redirect")),
            ("Response.redirect", Some("response.redirect")),
            ("router.back", None),
            ("fetch", None),
            ("Redirect", None),
        ] {
            assert_eq!(next_nav_verb(name), verb, "{name}");
            assert_eq!(
                NextjsResolver.claims_reference(name),
                verb.is_some(),
                "{name}"
            );
        }
    }

    // -- resolve: a navigation call → the page it names ---------------------

    type Ctx = RouteCtx;

    /// A page file and the route node Next's extraction makes for it.
    trait Pages {
        fn page(self, file: &str) -> Self;
    }

    impl Pages for RouteCtx {
        fn page(self, file: &str) -> Self {
            self.routes(file, extract(file, "").nodes)
        }
    }

    const NAV: &str = "components/nav.ts";

    fn call(name: &str, line: i64, column: i64) -> RefView {
        call_in(NAV, name, line, column)
    }

    fn site() -> Ctx {
        Ctx::default()
            .page("app/page.tsx")
            .page("app/login/page.tsx")
            .page("app/users/page.tsx")
            .page("app/users/[id]/page.tsx")
            .page("app/users/new/page.tsx")
            .page("app/docs/[...all]/page.tsx")
            .file(
                NAV,
                "export function go(user, x, h, list) {\n  router.push(`/users/${user.id}`)\n  router.push('/users/new')\n  redirect('/users?tab=1')\n  NextResponse.redirect(new URL('/login', req.url))\n  router.push(x ? '/login' : '/users')\n  router.push(x ? '/users/' : '/users')\n  router.push(h)\n  router.push('/nowhere')\n  list.push('/login')\n  router.push('./login')\n  router.push('https://example.com/docs/a/b')\n  router.back('/login')\n}\n",
            )
    }

    #[test]
    fn a_navigation_call_navigates_to_the_page_its_href_names() {
        let ctx = site();
        let resolve = |line: i64| NextjsResolver.resolve(&call("router.push", line, 2), &ctx);
        // A template hole fills a parameter; a literal segment beats it.
        let push = resolve(2).expect("template push");
        assert_eq!(
            push.target_node_id,
            ctx.route("/users/:id", "app/users/[id]/page.tsx")
        );
        assert_eq!(push.resolved_by, ResolvedBy::Framework);
        assert_eq!(push.confidence, 0.95);
        let edge = push.edge.as_ref().expect("edge shape");
        assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
        assert_eq!(edge.metadata["href"], "/users/${…}");
        assert_eq!(edge.metadata["navMethod"], "push");
        assert!(edge.also_targets.is_empty());
        assert_eq!(
            resolve(3).unwrap().target_node_id,
            ctx.route("/users/new", "app/users/new/page.tsx")
        );
        // `redirect` drops the query; `new URL(…)` gives its first argument.
        let redirect = NextjsResolver
            .resolve(&call("redirect", 4, 2), &ctx)
            .expect("redirect");
        assert_eq!(
            redirect.target_node_id,
            ctx.route("/users", "app/users/page.tsx")
        );
        assert_eq!(
            redirect.edge.as_ref().unwrap().metadata["href"],
            "/users?tab=1"
        );
        assert_eq!(
            redirect.edge.as_ref().unwrap().metadata["navMethod"],
            "redirect"
        );
        let url = NextjsResolver
            .resolve(&call("NextResponse.redirect", 5, 2), &ctx)
            .expect("NextResponse.redirect");
        assert_eq!(
            url.target_node_id,
            ctx.route("/login", "app/login/page.tsx")
        );
        assert_eq!(
            url.edge.as_ref().unwrap().metadata["navMethod"],
            "response.redirect"
        );
        // An absolute URL keeps its path.
        assert_eq!(
            resolve(12).unwrap().target_node_id,
            ctx.route("/docs/:all*", "app/docs/[...all]/page.tsx")
        );
    }

    #[test]
    fn every_arm_of_a_conditional_is_an_edge_of_its_own() {
        let ctx = site();
        let fork = NextjsResolver
            .resolve(&call("router.push", 6, 2), &ctx)
            .expect("fork");
        assert_eq!(
            fork.target_node_id,
            ctx.route("/login", "app/login/page.tsx")
        );
        let edge = fork.edge.as_ref().unwrap();
        assert_eq!(edge.also_targets.len(), 1);
        assert_eq!(
            edge.also_targets[0].target_node_id,
            ctx.route("/users", "app/users/page.tsx")
        );
        assert_eq!(edge.also_targets[0].metadata["href"], "/users");
        // Arms naming the same page are one edge.
        let same = NextjsResolver
            .resolve(&call("router.push", 7, 2), &ctx)
            .expect("same page");
        assert!(same.edge.as_ref().unwrap().also_targets.is_empty());
    }

    #[test]
    fn what_names_no_page_stays_unresolved() {
        let ctx = site();
        // A computed href, a path no page serves, a relative href.
        for line in [8, 9, 11] {
            assert!(
                NextjsResolver
                    .resolve(&call("router.push", line, 2), &ctx)
                    .is_none(),
                "line {line}"
            );
        }
        // Any receiver's `push` binds when its string names a page (upstream).
        assert_eq!(
            NextjsResolver
                .resolve(&call("list.push", 10, 2), &ctx)
                .unwrap()
                .target_node_id,
            ctx.route("/login", "app/login/page.tsx")
        );
        // Not a navigation verb, not a call, not a script.
        assert!(
            NextjsResolver
                .resolve(&call("router.back", 13, 2), &ctx)
                .is_none()
        );
        let mut reference = call("router.push", 3, 2);
        reference.reference_kind = EdgeKind::References;
        assert!(NextjsResolver.resolve(&reference, &ctx).is_none());
        let mut reference = call("router.push", 3, 2);
        reference.language = Language::Swift;
        assert!(NextjsResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn a_call_outside_every_next_app_is_not_read() {
        let ctx = Ctx::default()
            .page("apps/web/app/login/page.tsx")
            .file("apps/mobile/nav.ts", "router.push('/login')\n");
        let mut reference = call("router.push", 1, 0);
        reference.file_path = "apps/mobile/nav.ts".to_string();
        assert!(NextjsResolver.resolve(&reference, &ctx).is_none());
        reference.file_path = "apps/web/nav.ts".to_string();
        let ctx = ctx.file("apps/web/nav.ts", "router.push('/login')\n");
        assert!(NextjsResolver.resolve(&reference, &ctx).is_some());
    }

    /// W7-07 (KEEP-RUST): of several pages with one path, the call's own
    /// app's; none or several of them is nothing. One page anywhere binds,
    /// as upstream's single table does.
    #[test]
    fn a_path_two_apps_serve_names_the_callers_own_page() {
        let ctx = Ctx::default()
            .page("apps/admin/app/login/page.tsx")
            .page("apps/web/app/login/page.tsx")
            .page("apps/web/app/about/page.tsx")
            .file(
                "apps/admin/nav.ts",
                "router.push('/login')\nrouter.push('/about')\n",
            )
            .file("apps/web/nav.ts", "router.push('/login')\n");
        let at = |file: &str, line: i64| {
            let mut reference = call("router.push", line, 0);
            reference.file_path = file.to_string();
            NextjsResolver
                .resolve(&reference, &ctx)
                .map(|resolved| resolved.target_node_id)
        };
        assert_eq!(
            at("apps/admin/nav.ts", 1),
            Some(ctx.route("/login", "apps/admin/app/login/page.tsx"))
        );
        assert_eq!(
            at("apps/web/nav.ts", 1),
            Some(ctx.route("/login", "apps/web/app/login/page.tsx"))
        );
        assert_eq!(
            at("apps/admin/nav.ts", 2),
            Some(ctx.route("/about", "apps/web/app/about/page.tsx"))
        );
        // A repository-root app and a nested one: the call under neither
        // nested app is the root app's.
        let ctx = Ctx::default()
            .page("app/login/page.tsx")
            .page("apps/web/app/login/page.tsx")
            .file("lib/nav.ts", "router.push('/login')\n");
        let mut reference = call("router.push", 1, 0);
        reference.file_path = "lib/nav.ts".to_string();
        assert_eq!(
            NextjsResolver
                .resolve(&reference, &ctx)
                .map(|resolved| resolved.target_node_id),
            Some(ctx.route("/login", "app/login/page.tsx"))
        );
        // Two pages of one path in one app (a Next build error): nothing,
        // and no dynamic route stands in for it.
        let ctx = Ctx::default()
            .page("pages/login.tsx")
            .page("app/login/page.tsx")
            .page("app/[slug]/page.tsx")
            .file("lib/nav.ts", "router.push('/login')\n");
        assert!(NextjsResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn a_local_variable_names_the_href_it_was_given() {
        let ctx = site().file(
            NAV,
            "export function go(x) {\n  const href = x ? '/login' : '/users'\n  router.push(href)\n}\n",
        );
        let resolved = NextjsResolver
            .resolve(&call("router.push", 3, 2), &ctx)
            .expect("via local");
        assert_eq!(
            resolved.target_node_id,
            ctx.route("/login", "app/login/page.tsx")
        );
        assert_eq!(resolved.edge.as_ref().unwrap().also_targets.len(), 1);
    }

    #[test]
    fn an_href_segment_is_matched_decoded() {
        let ctx = site().file(
            NAV,
            "router.push('/users/n%65w')\nrouter.push('/users/%E0%A4%A')\n",
        );
        assert_eq!(
            NextjsResolver
                .resolve(&call("router.push", 1, 0), &ctx)
                .map(|resolved| resolved.target_node_id),
            Some(ctx.route("/users/new", "app/users/new/page.tsx"))
        );
        // A malformed escape is the segment as written, a value for `[id]`.
        assert_eq!(
            NextjsResolver
                .resolve(&call("router.push", 2, 0), &ctx)
                .map(|resolved| resolved.target_node_id),
            Some(ctx.route("/users/:id", "app/users/[id]/page.tsx"))
        );
    }

    #[test]
    fn next_is_detected_by_its_dependency_or_its_config_beside_a_route() {
        assert!(!NextjsResolver.detect(&Ctx::default().file("app/page.tsx", "")));
        assert!(
            NextjsResolver.detect(
                &Ctx::default()
                    .file("next.config.mjs", "")
                    .file("app/page.tsx", "")
            )
        );
        assert!(!NextjsResolver.detect(&Ctx::default().file("next.config.js", "")));
        assert!(
            NextjsResolver.detect(
                &Ctx::default().file("package.json", "{\"dependencies\":{\"next\":\"15\"}}")
            )
        );
    }

    #[test]
    fn the_navigation_hook_scopes_a_page_to_every_next_app() {
        let ctx = Ctx::default()
            .page("apps/web/app/login/page.tsx")
            .page("docs/pages/index.tsx");
        let navigation = NextjsResolver.navigation().expect("navigation");
        assert_eq!(
            navigation.tails(),
            [
                "push",
                "replace",
                "prefetch",
                "redirect",
                "permanentRedirect"
            ]
        );
        let page = ctx.nodes()[0].clone();
        assert_eq!(
            navigation.scope(&page, &ctx),
            Some(vec!["apps/web/".to_string(), "docs/".to_string()])
        );
        let mut handler = page;
        handler.name = "GET /login".to_string();
        assert_eq!(navigation.scope(&handler, &ctx), None);
    }
}
