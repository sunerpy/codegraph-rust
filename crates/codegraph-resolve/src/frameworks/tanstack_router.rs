//! TanStack Router [`FrameworkResolver`]: the path is a literal, and so is
//! the destination. Ports upstream `resolution/frameworks/tanstack-router.ts`
//! at `e8ddc4e2`, which `b635dd46` already had (F5).
//!
//! Routes are declared two ways, and both are read:
//!
//! ```text
//! // file-based (the plugin's default): the full path is the argument
//! export const Route = createFileRoute('/dashboard/invoices/$invoiceId')({
//!   component: InvoiceComponent,
//! })
//!
//! // code-based: a path per route, composed through its parent
//! const postsRoute = createRoute({ getParentRoute: () => rootRoute, path: 'posts' })
//! const postRoute  = createRoute({ getParentRoute: () => postsRoute, path: '$postId' })
//! ```
//!
//! 1. **A parameter is `$id`, not `:id`.** Route names are normalised to the
//!    `:id` the other routers use, so one matcher serves them all.
//! 2. **`to` is the route pattern, not a filled URL.** `<Link
//!    to="/posts/$postId" params={{ postId }}>` names the route and passes the
//!    values beside it, so a destination is normalised the way a route name
//!    is and then matches it exactly.
//! 3. **A destination is an object.** `navigate({ to: '/' })`, `throw
//!    redirect({ to: '/login' })`: the path is under a `to` key, and a
//!    `navigate({ search: … })` with no `to` stays on the page it is on.
//!
//! Not every route file is a page. A segment written `_auth` is a pathless
//! layout, absent from the URL, whose file renders an outlet rather than a
//! screen; a `(group)` segment is invisible too; a `dashboard.route.tsx` is
//! the layout for the `/dashboard` subtree while `dashboard.index.tsx`, whose
//! literal carries a trailing slash, is the page at `/dashboard`.
//!
//! Left unresolved rather than guessed: a computed `to`, a path no route
//! serves, a path two routes of the call's app serve (KEEP-RUST), and a
//! code-based route whose parent is declared in another file (the chain is
//! composed within a file, where a route tree is written). `<Link to>` is
//! JSX, not a call: the link pass reads it through [`tanstack_link_routes`]
//! (`synthesis/links.rs`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::js_scan::{self, JS_SPACE, ObjectField};
use super::route_href::{
    ExactTable, HrefLiteral, LinkRoutes, RouteEntry, app_root_for, destinations_for_href,
    first_argument_text, parse_href_expression, read_href_via_local, read_string_at, to_href,
};
use super::{framework_node, js_language_for};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver, NavigationCalls};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// The languages routes and navigation calls are written in.
const ROUTE_LANGUAGES: [Language; 4] = [
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];

/// The identifier pattern upstream writes `[A-Za-z_$][\w$]*`.
const IDENT: &str = "[A-Za-z_$][A-Za-z0-9_$]*";

// =============================================================================
// Paths
// =============================================================================

/// A TanStack path in the form the other routers' routes take
/// (`tanstackPath`): `$invoiceId` is `:invoiceId` and a bare `$` a splat; a
/// `_auth` segment is a pathless layout and a `(group)` segment a route
/// group, neither of which is in the URL; a trailing `_` un-nests without
/// changing the segment. `None` for a path that names no address.
pub fn tanstack_path(raw: &str) -> Option<String> {
    if !raw.starts_with('/') {
        return None;
    }
    let mut segs: Vec<String> = Vec::new();
    for seg in raw.split('/') {
        if seg.is_empty() || seg.starts_with('_') || (seg.starts_with('(') && seg.ends_with(')')) {
            continue;
        }
        let bare = seg.strip_suffix('_').unwrap_or(seg);
        if bare == "$" {
            segs.push(":splat*".to_string());
            continue;
        }
        segs.push(match bare.strip_prefix('$') {
            Some(name) => format!(":{name}"),
            None => bare.to_string(),
        });
    }
    Some(format!("/{}", segs.join("/")))
}

/// True when the literal names a pathless layout rather than a page
/// (`isPathlessLayout`): `'/_auth'` is the layout itself, at no address of its
/// own; `'/_auth/'` is the index route inside it, at the layout's address.
fn is_pathless_layout(raw: &str) -> bool {
    if raw.len() > 1 && raw.ends_with('/') {
        return false;
    }
    raw.split('/')
        .rfind(|seg| !seg.is_empty())
        .is_some_and(|last| last.starts_with('_'))
}

/// True for a file that wraps a subtree rather than rendering a page at its
/// own address (`isLayoutFile`): one that renders `<Outlet />`, or one named
/// `route.tsx` by convention. `_auth.invoices.tsx` and
/// `_auth.invoices.index.tsx` both say `/invoices`; the layout is not the
/// page.
fn is_layout_file(file_path: &str, content: &str) -> bool {
    static ROUTE_FILE: OnceLock<Regex> = OnceLock::new();
    static OUTLET: OnceLock<Regex> = OnceLock::new();
    let base = file_path.rsplit('/').next().unwrap_or(file_path);
    ROUTE_FILE
        .get_or_init(|| Regex::new(r"(?:^|\.)route\.(?:tsx|ts|jsx|js)$").expect("route file"))
        .is_match(base)
        || OUTLET
            .get_or_init(|| Regex::new(r"<Outlet(?-u:\b)").expect("outlet"))
            .is_match(content)
}

// =============================================================================
// Reading the routes
// =============================================================================

/// One route a file declares (`TanstackRouteEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TanstackRouteEntry {
    /// `/dashboard/invoices/:invoiceId`, normalised the way the table wants it.
    pub path: String,
    /// The component the route renders, when it names one.
    pub component: Option<String>,
    /// True for the index route at an address, which outranks the layout
    /// that wraps it.
    pub index: bool,
    /// True when the route came from `createFileRoute`: one route per file,
    /// so the file's own shape describes it.
    pub file_based: bool,
    pub line: i64,
}

/// A code-based route as its declaration reads.
struct CodeRoute {
    path: Option<String>,
    parent: Option<String>,
    component: Option<String>,
    root: bool,
    at: usize,
}

/// Every route a file declares, file-based and code-based alike
/// (`parseTanstackRoutes`). A code-based route's own `path` is a fragment
/// (`posts`, `$postId`, `/`), so the chain of `getParentRoute: () => parent`
/// is followed to compose the full address, within the file. A route that
/// is another route's parent is the layout for that subtree, and the address
/// belongs to the index route under it.
pub fn parse_tanstack_routes(content: &str) -> Vec<TanstackRouteEntry> {
    static FACTORY: OnceLock<Regex> = OnceLock::new();
    static FILE_ROUTE: OnceLock<Regex> = OnceLock::new();
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static PARENT: OnceLock<Regex> = OnceLock::new();
    // The calls that declare a route: the cheap gate before parsing anything.
    if !FACTORY
        .get_or_init(|| {
            Regex::new(&format!(
                r"(?-u:\b)create(?:File|Lazy(?:File)?|Root)?Route{JS_SPACE}*\("
            ))
            .expect("route factory")
        })
        .is_match(content)
    {
        return Vec::new();
    }
    let safe = strip_comments_for_regex(content, CommentLang::TypeScript);
    let line_of = |index: usize| safe[..index].matches('\n').count() as i64 + 1;
    let mut out = Vec::new();

    // File-based: the path is the first argument, the options follow.
    let file_route = FILE_ROUTE.get_or_init(|| {
        Regex::new(&format!(r"(?-u:\b)create(?:Lazy)?FileRoute{JS_SPACE}*\(")).expect("file route")
    });
    for found in file_route.find_iter(&safe) {
        let open = found.end() - 1;
        let Some(close) = js_scan::match_bracket(&safe, open) else {
            continue;
        };
        let Some(raw) = read_string_at(
            safe[open + 1..close].trim_start_matches(js_scan::is_js_space),
            0,
        ) else {
            continue;
        };
        let Some(path) = tanstack_path(&raw) else {
            continue;
        };
        if is_pathless_layout(&raw) {
            continue;
        }
        out.push(TanstackRouteEntry {
            path,
            component: component_in(chain_after(&safe, close + 1)),
            // `createFileRoute('/dashboard/')` is the index page at
            // `/dashboard`; `createFileRoute('/dashboard')` the layout
            // around it.
            index: raw.len() > 1 && raw.ends_with('/'),
            file_based: true,
            line: line_of(found.start()),
        });
    }

    // Code-based: a fragment per route, composed through its parent. A name
    // declared again keeps its first place with the later declaration, as a
    // `Map` set again does.
    let named = NAMED.get_or_init(|| {
        Regex::new(&format!(
            r"(?-u:\b)(?:const|let|var){JS_SPACE}+({IDENT}){JS_SPACE}*(?::[^=]*?)?={JS_SPACE}*create(Root)?Route{JS_SPACE}*\({JS_SPACE}*\{{"
        ))
        .expect("code route")
    });
    let parent_of = PARENT
        .get_or_init(|| Regex::new(&format!(r"=>{JS_SPACE}*({IDENT})")).expect("parent route"));
    let mut order: Vec<String> = Vec::new();
    let mut decls: HashMap<String, CodeRoute> = HashMap::new();
    for captures in named.captures_iter(&safe) {
        let (Some(whole), Some(name)) = (captures.get(0), captures.get(1)) else {
            continue;
        };
        let brace = whole.end() - 1;
        let Some(end) = js_scan::match_bracket(&safe, brace) else {
            continue;
        };
        let fields = js_scan::read_fields(&safe, brace, end);
        let field = |key: &str| fields.iter().find(|field: &&ObjectField| field.key == key);
        let root = captures.get(2).is_some();
        let path = if root {
            Some("/".to_string())
        } else {
            field("path").and_then(|path| {
                read_string_at(path.text.trim_start_matches(js_scan::is_js_space), 0)
            })
        };
        let parent = field("getParentRoute").and_then(|parent| {
            parent_of
                .captures(parent.text)
                .and_then(|captures| captures.get(1))
                .map(|name| name.as_str().to_string())
        });
        let component = field("component").and_then(|component| component_in(component.text));
        if !decls.contains_key(name.as_str()) {
            order.push(name.as_str().to_string());
        }
        decls.insert(
            name.as_str().to_string(),
            CodeRoute {
                path,
                parent,
                component,
                root,
                at: whole.start(),
            },
        );
    }
    // A route with an index child is the layout around that address; the
    // child with `path: '/'` is what renders there. A parent with no index
    // child is still the page at its own address.
    let wraps_an_index: HashSet<&str> = decls
        .values()
        .filter(|route| route.path.as_deref() == Some("/"))
        .filter_map(|route| route.parent.as_deref())
        .collect();
    for name in &order {
        let decl = &decls[name];
        // A pathless layout contributes no address; `createRootRoute` is the
        // outermost layout, and the index route beside it renders at `/`.
        if decl.path.is_none() || decl.root || wraps_an_index.contains(name.as_str()) {
            continue;
        }
        let Some(full) = compose_path(name, &decls) else {
            continue;
        };
        let Some(path) = tanstack_path(&full) else {
            continue;
        };
        out.push(TanstackRouteEntry {
            path,
            component: decl.component.clone(),
            index: decl.path.as_deref() == Some("/"),
            file_based: false,
            line: line_of(decl.at),
        });
    }
    out
}

/// The address a code-based route sits at, following `getParentRoute` up
/// (`composePath`); `None` when a parent is declared in another file.
fn compose_path(name: &str, decls: &HashMap<String, CodeRoute>) -> Option<String> {
    let mut segs: Vec<&str> = Vec::new();
    let mut current = Some(name);
    for _ in 0..24 {
        let Some(at) = current else {
            break;
        };
        let decl = decls.get(at)?;
        if let Some(path) = &decl.path {
            let own: Vec<&str> = path.split('/').filter(|seg| !seg.is_empty()).collect();
            segs.splice(0..0, own);
        }
        current = decl.parent.as_deref();
    }
    Some(format!("/{}", segs.join("/")))
}

/// The text of the call chain starting at `at` (`chainAfter`): `({ … })`, and
/// any `.update({ … })` or `.lazy(…)` after it, where a route's component may
/// be written.
fn chain_after(s: &str, at: usize) -> &str {
    let bytes = s.as_bytes();
    let skip_space = |from: usize| {
        from + (s[from..].len() - s[from..].trim_start_matches(js_scan::is_js_space).len())
    };
    let mut i = at;
    for _ in 0..8 {
        i = skip_space(i);
        if bytes.get(i) == Some(&b'.') {
            i += 1;
            while bytes
                .get(i)
                .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
            {
                i += 1;
            }
            i = skip_space(i);
        }
        if bytes.get(i) != Some(&b'(') {
            break;
        }
        let Some(close) = js_scan::match_bracket(s, i) else {
            break;
        };
        i = close + 1;
    }
    &s[at..i]
}

/// The component a route names (`componentIn`): the file a lazy import
/// names, else a `component:` identifier, else the text when it is one.
fn component_in(text: &str) -> Option<String> {
    static LAZY: OnceLock<Regex> = OnceLock::new();
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static BARE: OnceLock<Regex> = OnceLock::new();
    if let Some(module) = LAZY
        .get_or_init(|| {
            Regex::new(&format!(
                r#"(?-u:\b)import{JS_SPACE}*\({JS_SPACE}*['"`]([^'"`]+)['"`]"#
            ))
            .expect("lazy component")
        })
        .captures(text)
        .and_then(|captures| captures.get(1))
    {
        let base = module.as_str().rsplit('/').next().unwrap_or_default();
        // `.replace(/\.\w+$/, '')`: the extension, when the tail after the
        // last `.` is all word characters.
        let base = match base.rfind('.') {
            Some(dot)
                if dot + 1 < base.len()
                    && base[dot + 1..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                &base[..dot]
            }
            _ => base,
        };
        return (!base.is_empty()).then(|| base.to_string());
    }
    NAMED
        .get_or_init(|| {
            Regex::new(&format!(
                r"(?:^|[^A-Za-z0-9_$])component{JS_SPACE}*:{JS_SPACE}*([A-Z][A-Za-z0-9_]*)"
            ))
            .expect("named component")
        })
        .captures(text)
        .or_else(|| {
            BARE.get_or_init(|| {
                Regex::new(&format!(r"^{JS_SPACE}*([A-Z][A-Za-z0-9_]*){JS_SPACE}*$"))
                    .expect("bare component")
            })
            .captures(text)
        })
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str().to_string())
}

/// The id a TanStack route carries (`routeId`): a verbatim reconstruction,
/// so the table recognises its own.
fn route_id(file_path: &str, line: i64, path: &str) -> String {
    format!("route:{file_path}:{line}:{path}:tanstack")
}

/// True for a route node this resolver emitted, and no other
/// (`isTanstackRoute`).
fn is_tanstack_route(node: &Node) -> bool {
    node.id == route_id(&node.file_path, node.start_line, &node.name)
}

// =============================================================================
// Route table
// =============================================================================

/// One TanStack app's routes, matched by path.
#[derive(Debug, Default)]
struct AppRoutes {
    exact: ExactTable,
    dynamic: Vec<RouteEntry>,
}

/// The routes of every TanStack app, by the directory the app lives in
/// (`tanstackTable`). A call matches only the routes of its own app; a path
/// two routes of that app serve answers nothing (KEEP-RUST: upstream's
/// `addRouteTo` keeps the first in node order, which a sync changes).
#[derive(Debug, Default)]
struct TanstackTable {
    by_root: BTreeMap<String, AppRoutes>,
}

impl TanstackTable {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut table = Self::default();
        for node in context.get_nodes_by_kind_shared(NodeKind::Route) {
            if !is_tanstack_route(&node) {
                continue;
            }
            let root = app_root_for(&node.file_path);
            let app = table.by_root.entry(root.clone()).or_default();
            if node.name.contains(':') {
                app.dynamic.push(RouteEntry::new(Arc::clone(&node)));
            }
            app.exact.insert(node.name.clone(), root, node);
        }
        table
    }

    /// The routes of the app `file_path` belongs to: the longest root above
    /// it (`routesForFile`).
    fn app_of(&self, file_path: &str) -> Option<&AppRoutes> {
        self.by_root
            .iter()
            .filter(|(root, _)| file_path.starts_with(root.as_str()))
            .max_by_key(|(root, _)| root.len())
            .map(|(_, app)| app)
    }
}

/// The pass's TanStack Router table.
fn tanstack_table(context: &dyn ResolutionContext) -> Arc<TanstackTable> {
    context
        .route_tables()
        .get_or_build("tanstack-router", || TanstackTable::build(context))
}

impl LinkRoutes for TanstackTable {
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

/// The TanStack routes a `<Link to>` pattern reaches, in its own app (W7-18a).
pub(crate) fn tanstack_link_routes(context: &dyn ResolutionContext) -> Arc<dyn LinkRoutes> {
    tanstack_table(context)
}

// =============================================================================
// Navigation calls
// =============================================================================

/// `navigate({ to })` from `useNavigate`, `router.navigate({ to })` and
/// `Route.navigate`, and a thrown `redirect({ to })` (`NAV_CALL`).
fn nav_call() -> &'static Regex {
    static NAV_CALL: OnceLock<Regex> = OnceLock::new();
    NAV_CALL.get_or_init(|| {
        Regex::new(r"^(?:navigate|redirect)$|^(?:router|Route)\.navigate$")
            .expect("navigation call")
    })
}

/// The verb a navigation call name stands for, or `None`
/// (`tanstackNavVerb`).
pub fn tanstack_nav_verb(name: &str) -> Option<&str> {
    nav_call()
        .is_match(name)
        .then(|| name.rsplit('.').next().unwrap_or(name))
}

/// The destination of a TanStack navigation (`tanstackDestination`): the
/// `to` of `{ to: '/posts/$postId' }`, or a string, normalised exactly as a
/// route name is. A `navigate({ search: … })` with no `to` changes the
/// search on the page the user is on.
pub(crate) fn tanstack_destination(expr: &str) -> Option<HrefLiteral> {
    let args = js_scan::js_trim(expr);
    let literal = if args.starts_with('{') {
        to_key_of(args)
    } else {
        read_string_at(args, 0)
    }?;
    match tanstack_path(&literal) {
        Some(path) => to_href(Some(path)),
        None => parse_href_expression(args),
    }
}

/// The `to:` value of an object destination, or `None` when it has none or
/// it is computed (`toKeyOf`).
fn to_key_of(args: &str) -> Option<String> {
    let end = js_scan::match_bracket(args, 0)?;
    js_scan::read_fields(args, 0, end)
        .into_iter()
        .find(|field| field.key == "to")
        .and_then(|field| read_string_at(field.text.trim_start_matches(js_scan::is_js_space), 0))
}

// =============================================================================
// The resolver
// =============================================================================

/// TanStack Router resolver (`tanstackRouterResolver`).
pub struct TanstackRouterResolver;

/// The navigation calls TanStack Router binds to its routes: a route answers
/// the calls of its own app.
struct TanstackNavigation;

impl NavigationCalls for TanstackNavigation {
    fn tails(&self) -> &[&str] {
        &["navigate", "redirect"]
    }

    fn scope(&self, route: &Node, _context: &dyn ResolutionContext) -> Option<Vec<String>> {
        is_tanstack_route(route).then(|| vec![app_root_for(&route.file_path)])
    }
}

impl FrameworkResolver for TanstackRouterResolver {
    fn name(&self) -> &str {
        "tanstack-router"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&ROUTE_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        super::package_deps::depends_on(
            context,
            &[
                "@tanstack/react-router",
                "@tanstack/solid-router",
                "@tanstack/router",
                "@tanstack/react-start",
                "@tanstack/start",
            ],
        )
    }

    fn claims_reference(&self, name: &str) -> bool {
        nav_call().is_match(name)
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&TanstackNavigation)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        // A file-based route file describes one route, so the file's own
        // shape says whether that route is a page. A file holding a
        // code-based route tree describes many, and its root component draws
        // the outlet they render into: judging that file by the same rule
        // would drop every route in it.
        let layout = is_layout_file(file_path, content);
        let entries: Vec<TanstackRouteEntry> = parse_tanstack_routes(content)
            .into_iter()
            .filter(|entry| !(entry.file_based && layout))
            .collect();
        let mut result = FrameworkResolverExtractionResult::default();
        if entries.is_empty() {
            return Some(result);
        }
        let language = js_language_for(file_path);
        // An index route is the page at its address; a layout at the same
        // address wraps it. One address, one screen: the index wins it, in
        // the place the address first took.
        let mut by_path: Vec<TanstackRouteEntry> = Vec::new();
        for entry in entries {
            match by_path.iter_mut().find(|held| held.path == entry.path) {
                Some(held) => {
                    if entry.index && !held.index {
                        *held = entry;
                    }
                }
                None => by_path.push(entry),
            }
        }
        for entry in by_path {
            let node = framework_node(
                route_id(file_path, entry.line, &entry.path),
                NodeKind::Route,
                entry.path.clone(),
                format!("{file_path}::route:{}", entry.path),
                file_path.to_string(),
                entry.line,
                entry.line,
                0,
                0,
                language,
                false,
            );
            if let Some(component) = &entry.component {
                // `calls`, as every component-backed screen binds.
                result.references.push(RefView {
                    row_id: None,
                    from_node_id: node.id.clone(),
                    reference_name: component.clone(),
                    reference_kind: EdgeKind::Calls,
                    line: entry.line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language,
                    is_function_ref: false,
                    reference_subkind: None,
                });
            }
            result.nodes.push(node);
        }
        Some(result)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let verb = tanstack_nav_verb(&reference.reference_name)?;
        if !ROUTE_LANGUAGES.contains(&reference.language) {
            return None;
        }
        let table = tanstack_table(context);
        let app = table.app_of(&reference.file_path)?;
        if app.exact.is_empty() {
            return None;
        }
        let content = context.read_file_shared(&reference.file_path)?;
        let lines: Vec<&str> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let arg = first_argument_text(&lines, reference.line, reference.column, verb)?;
        let mut href = tanstack_destination(&arg);
        if href.is_none() {
            let start = context
                .get_node_by_id_shared(&reference.from_node_id)
                .filter(|enclosing| enclosing.file_path == reference.file_path)
                .map_or((reference.line - 40).max(1), |enclosing| {
                    enclosing.start_line
                });
            href = read_href_via_local(&lines, reference.line, reference.column, verb, start);
        }
        let href = href?;
        // Every arm of a conditional destination is somewhere this call goes:
        // the first is this reference's resolution, the rest ride along.
        let mut targets =
            destinations_for_href(&href, &app.exact, &app.dynamic, &reference.file_path)
                .into_iter();
        let (target, arm) = targets.next()?;
        let metadata = |display: &str| {
            let mut metadata = serde_json::Map::new();
            metadata.insert("href".to_string(), serde_json::json!(display));
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
    use crate::frameworks::route_href::test_ctx::{RouteCtx, call_in};

    /// Upstream `tanstack: tanstackPath`.
    #[test]
    fn a_tanstack_path_is_named_as_every_router_names_its_routes() {
        for (raw, normalized) in [
            ("/", "/"),
            ("/login", "/login"),
            ("/posts/$postId", "/posts/:postId"),
            // A pathless layout is not in the URL; nor is a route group.
            ("/_auth/profile", "/profile"),
            ("/_pathlessLayout/route-a", "/route-a"),
            (
                "/(this-folder-is-not-in-the-url)/route-group",
                "/route-group",
            ),
            // An index route's trailing slash is the address of its parent.
            ("/dashboard/", "/dashboard"),
            // A trailing `_` un-nests without changing the segment.
            ("/posts_/$postId/edit", "/posts/:postId/edit"),
            ("/files/$", "/files/:splat*"),
        ] {
            assert_eq!(tanstack_path(raw).as_deref(), Some(normalized), "{raw}");
        }
        assert_eq!(tanstack_path("posts"), None);
    }

    fn entry(
        path: &str,
        component: Option<&str>,
        index: bool,
        file_based: bool,
        line: i64,
    ) -> TanstackRouteEntry {
        TanstackRouteEntry {
            path: path.to_string(),
            component: component.map(str::to_string),
            index,
            file_based,
            line,
        }
    }

    /// Upstream `tanstack: parseTanstackRoutes — file-based`.
    #[test]
    fn a_file_route_takes_its_path_from_the_literal_and_its_component_from_the_options() {
        assert_eq!(
            parse_tanstack_routes(
                "import { createFileRoute } from '@tanstack/react-router'\nexport const Route = createFileRoute('/dashboard/invoices/$invoiceId')({\n  params: { parse: (p) => ({ invoiceId: Number(p.invoiceId) }) },\n  component: InvoiceComponent,\n})\n"
            ),
            [entry(
                "/dashboard/invoices/:invoiceId",
                Some("InvoiceComponent"),
                false,
                true,
                2
            )]
        );
        // A component written on a chained `.update()`.
        assert_eq!(
            parse_tanstack_routes(
                "export const Route = createFileRoute('/login')({\n  validateSearch: z.object({ redirect: z.string().optional() }),\n}).update({\n  component: LoginComponent,\n})\n"
            ),
            [entry("/login", Some("LoginComponent"), false, true, 1)]
        );
        // An index route, a pathless layout that is no address of its own,
        // and the index inside a pathless layout, at that layout's address.
        assert_eq!(
            parse_tanstack_routes("createFileRoute('/dashboard/')({ component: X })"),
            [entry("/dashboard", Some("X"), true, true, 1)]
        );
        assert_eq!(
            parse_tanstack_routes("createFileRoute('/_auth')({ component: X })"),
            []
        );
        assert_eq!(
            parse_tanstack_routes("createFileRoute('/_layout/')({ component: Home })"),
            [entry("/", Some("Home"), true, true, 1)]
        );
        // A lazily loaded component is named by its module's file.
        assert_eq!(
            parse_tanstack_routes(
                "createLazyFileRoute('/about')({ component: lazyRouteComponent(() => import('./pages/About.tsx')) })"
            ),
            [entry("/about", Some("About"), false, true, 1)]
        );
        // A path the file computes names no route.
        assert_eq!(
            parse_tanstack_routes("createFileRoute(base + '/x')({ component: X })"),
            []
        );
    }

    /// Upstream `tanstack: parseTanstackRoutes — code-based`.
    const CODE_ROUTES: &str = "import { createRootRoute, createRoute } from '@tanstack/react-router'\nconst rootRoute = createRootRoute({ component: RootComponent })\nconst indexRoute = createRoute({\n  getParentRoute: () => rootRoute,\n  path: '/',\n  component: IndexComponent,\n})\nconst postsLayoutRoute = createRoute({\n  getParentRoute: () => rootRoute,\n  path: 'posts',\n  component: PostsLayoutComponent,\n})\nconst postsIndexRoute = createRoute({\n  getParentRoute: () => postsLayoutRoute,\n  path: '/',\n  component: PostsIndexComponent,\n})\nconst postRoute = createRoute({\n  getParentRoute: () => postsLayoutRoute,\n  path: '$postId',\n  component: PostComponent,\n})\nconst pathlessRoute = createRoute({\n  getParentRoute: () => rootRoute,\n  id: 'pathless',\n  component: PathlessComponent,\n})\nconst routeARoute = createRoute({\n  getParentRoute: () => pathlessRoute,\n  path: '/route-a',\n  component: RouteAComponent,\n})\n";

    #[test]
    fn a_code_route_composes_its_path_through_its_parents() {
        let routes = parse_tanstack_routes(CODE_ROUTES);
        assert_eq!(
            routes
                .iter()
                .map(|route| (route.path.as_str(), route.component.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("/", Some("IndexComponent")),
                ("/posts", Some("PostsIndexComponent")),
                ("/posts/:postId", Some("PostComponent")),
                ("/route-a", Some("RouteAComponent")),
            ]
        );
        // `postsLayoutRoute` sits at `/posts` and wraps the index there.
        assert_eq!(
            routes.iter().filter(|route| route.path == "/posts").count(),
            1
        );
        assert_eq!(
            routes.iter().map(|route| route.line).collect::<Vec<_>>(),
            [3, 13, 18, 28]
        );
        // A parent declared in another file is not composed.
        assert_eq!(
            parse_tanstack_routes(
                "import { rootRoute } from './root'\nconst aboutRoute = createRoute({ getParentRoute: () => rootRoute, path: 'about', component: About })\n"
            ),
            []
        );
    }

    /// Upstream `tanstack: destinations`.
    #[test]
    fn a_destination_is_the_pattern_its_to_key_names() {
        for (name, verb) in [
            ("navigate", "navigate"),
            ("redirect", "redirect"),
            ("router.navigate", "navigate"),
            ("Route.navigate", "navigate"),
        ] {
            assert_eq!(tanstack_nav_verb(name), Some(verb), "{name}");
        }
        for name in ["push", "replace", "paths.push", "goto", "history.push"] {
            assert_eq!(tanstack_nav_verb(name), None, "{name}");
        }
        let path = |expr: &str| tanstack_destination(expr).map(|href| href.path);
        assert_eq!(
            path("{ to: '/posts/$postId' }").as_deref(),
            Some("/posts/:postId")
        );
        assert_eq!(
            path("{ to: '/login', search: { redirect } }").as_deref(),
            Some("/login")
        );
        assert_eq!(path("'/posts/$postId'").as_deref(), Some("/posts/:postId"));
        // No destination: a change of search on the page the user is on; a
        // computed `to` is none either.
        assert_eq!(path("{ search: (old) => ({ ...old, page: 2 }) }"), None);
        assert_eq!(path("{ to: target }"), None);
    }

    fn route(file: &str, line: i64, path: &str) -> Node {
        framework_node(
            route_id(file, line, path),
            NodeKind::Route,
            path.to_string(),
            format!("{file}::route:{path}"),
            file.to_string(),
            line,
            line,
            0,
            0,
            js_language_for(file),
            false,
        )
    }

    fn resolve(ctx: &RouteCtx, file: &str, name: &str, line: i64) -> Option<ResolvedRef> {
        let mut reference = call_in(file, name, line, 0);
        reference.language = Language::Tsx;
        TanstackRouterResolver.resolve(&reference, ctx)
    }

    /// A navigation names the route its pattern names, in its own app; a
    /// pattern two routes of that app serve, another app's routes, and
    /// another router's routes stay unresolved.
    #[test]
    fn a_navigation_reaches_the_one_route_of_its_app_its_pattern_names() {
        let ctx = RouteCtx::default()
            .routes(
                "web/src/routes/posts.$postId.tsx",
                vec![route("web/src/routes/posts.$postId.tsx", 2, "/posts/:postId")],
            )
            .routes(
                "web/src/routes/posts.tsx",
                vec![route("web/src/routes/posts.tsx", 2, "/posts")],
            )
            .routes(
                "web/src/routes/posts/index.tsx",
                vec![route("web/src/routes/posts/index.tsx", 2, "/posts")],
            )
            .routes(
                "admin/src/routes/login.tsx",
                vec![route("admin/src/routes/login.tsx", 2, "/login")],
            )
            .file(
                "web/src/nav.tsx",
                "export function go(navigate) {\n  navigate({ to: '/posts/$postId', params: { postId: 1 } })\n  navigate({ to: '/posts' })\n  navigate({ to: '/login' })\n}\n",
            );
        let post = resolve(&ctx, "web/src/nav.tsx", "navigate", 2).expect("the post route");
        assert_eq!(
            post.target_node_id,
            ctx.route("/posts/:postId", "web/src/routes/posts.$postId.tsx")
        );
        let edge = post.edge.expect("a navigates edge");
        assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
        assert_eq!(edge.metadata["href"], "/posts/:postId");
        assert_eq!(edge.metadata["navMethod"], "navigate");
        // Two routes of the web app serve `/posts` (KEEP-RUST), and `/login`
        // is only the admin app's.
        assert!(resolve(&ctx, "web/src/nav.tsx", "navigate", 3).is_none());
        assert!(resolve(&ctx, "web/src/nav.tsx", "navigate", 4).is_none());
        // A route another router named, at the same path, is none of
        // TanStack's.
        let mut other = route("web/src/routes/about.tsx", 2, "/about");
        other.id = "route:web/src/routes/about.tsx:2:/about".to_string();
        let ctx = ctx
            .routes("web/src/routes/about.tsx", vec![other])
            .file("web/src/about-nav.tsx", "navigate({ to: '/about' })\n");
        assert!(resolve(&ctx, "web/src/about-nav.tsx", "navigate", 1).is_none());
    }

    /// A sync finds the navigation calls a changed route may answer in its
    /// own app's files, and none for another router's route.
    #[test]
    fn a_route_is_answered_by_the_navigation_calls_of_its_app() {
        let posts = route("web/src/routes/posts.tsx", 2, "/posts");
        let mut other = route("web/src/routes/about.tsx", 2, "/about");
        other.id = "route:web/src/routes/about.tsx:2:/about".to_string();
        let ctx = RouteCtx::default().routes("web/src/routes/posts.tsx", vec![posts.clone()]);
        assert_eq!(
            TanstackNavigation.scope(&posts, &ctx),
            Some(vec!["web/".to_string()])
        );
        assert_eq!(TanstackNavigation.scope(&other, &ctx), None);
        assert_eq!(TanstackNavigation.tails(), ["navigate", "redirect"]);
    }

    /// Extraction: one route per address, the index winning over the layout
    /// in the place the address first took; a layout file's file route is
    /// none; a code route tree's file keeps every route though its root draws
    /// the outlet.
    #[test]
    fn a_file_extracts_one_route_per_address() {
        let extract = |file: &str, content: &str| {
            TanstackRouterResolver
                .extract(
                    file,
                    content,
                    &FrameworkExtractionContext::without_config(""),
                )
                .expect("an extraction")
        };
        let both = extract(
            "src/routes/dashboard.tsx",
            "createFileRoute('/dashboard')({ component: Layout })\ncreateFileRoute('/dashboard/')({ component: Page })\n",
        );
        assert_eq!(
            both.nodes
                .iter()
                .map(|node| (node.id.as_str(), node.name.as_str()))
                .collect::<Vec<_>>(),
            [(
                "route:src/routes/dashboard.tsx:2:/dashboard:tanstack",
                "/dashboard"
            )]
        );
        assert_eq!(
            both.references
                .iter()
                .map(|reference| (reference.reference_name.as_str(), reference.reference_kind))
                .collect::<Vec<_>>(),
            [("Page", EdgeKind::Calls)]
        );
        assert!(
            extract(
                "src/routes/posts.route.tsx",
                "createFileRoute('/posts')({ component: PostsLayout })\n"
            )
            .nodes
            .is_empty()
        );
        assert!(
            extract(
                "src/routes/posts.tsx",
                "createFileRoute('/posts')({ component: Posts })\nfunction Posts() { return <Outlet /> }\n"
            )
            .nodes
            .is_empty()
        );
        let tree = extract(
            "src/router.tsx",
            &format!("{CODE_ROUTES}function RootComponent() {{ return <Outlet /> }}\n"),
        );
        assert_eq!(
            tree.nodes
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["/", "/posts", "/posts/:postId", "/route-a"]
        );
    }
}
