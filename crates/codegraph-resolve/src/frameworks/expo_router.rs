//! Expo Router [`FrameworkResolver`]: file-based screens and string-keyed
//! navigation in a React Native app. Ports `upstream
//! resolution/frameworks/expo-router.ts` at `b635dd46` (F2, with #2135's
//! `+api` endpoints).
//!
//! 1. **A screen is a file.** Every file under `app/` (or `src/app/`) is a
//!    route: `app/object-detail.tsx` is `/object-detail`,
//!    `app/capture/index.tsx` is `/capture`, `app/item/[id].tsx` is
//!    `/item/[id]`, and a `(group)` directory is invisible in the URL.
//!    [`ExpoRouterResolver::extract`] emits one `route` node per screen,
//!    named by its path, with a `calls` reference to the file's default
//!    export. `app/blog/og-image/[post]+api.ts` is an API route: one
//!    `GET /blog/og-image/:post` endpoint per exported method, referencing
//!    its function.
//!
//! 2. **Navigation is a string.** `router.push('/object-detail?…')`,
//!    `router.navigate({ pathname: '/item/[id]', params })`, a template
//!    literal with the params interpolated, a project's own `safePush('/x')`:
//!    extraction records each as a call that resolves to nothing.
//!    [`ExpoRouterResolver::resolve`] claims those calls, reads the argument
//!    ([`super::route_href`]), matches it against the screens and returns a
//!    `navigates` edge carrying the href.
//!
//! Precision rests on the string naming a real screen, not on the receiver
//! being called `router`: a computed path, an ambiguous dynamic match or a
//! relative href from a file that is no screen stays unresolved. A call
//! matches only the screens of its own Expo app (the longest directory above
//! an `app/` that holds the calling file), and a file under no Expo app is
//! not read, so in a monorepo another app's `navigate('/')` never reaches an
//! Expo screen (KEEP-RUST; upstream keeps one table and reads every file).
//! `<Link href>`, `<Redirect href>` and `Stack.Screen` names are JSX, not
//! calls.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::route_href::{
    ExactTable, HOLE, RouteEntry, decode_segment, first_argument_text, match_route,
    parse_href_expression, read_href_via_local, route_path,
};
use super::{framework_node, js_language_for};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver, NavigationCalls};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// The languages screens and navigation calls are written in.
const ROUTE_LANGUAGES: [Language; 4] = [
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];

/// The methods an API route file exports (`API_METHODS`).
const API_METHODS: &str = "GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS";

/// The app directory, `app/` or `src/app/`; the first match wins
/// (`APP_DIR`).
fn app_dir() -> &'static Regex {
    static APP_DIR: OnceLock<Regex> = OnceLock::new();
    APP_DIR.get_or_init(|| Regex::new(r"(?:^|/)(?:src/)?app/").expect("app dir"))
}

/// The path of `file_path` below its app directory, and the directory the
/// app lives in (`""`, `apps/mobile/`).
fn below_app_dir(file_path: &str) -> Option<(&str, &str)> {
    let dir = app_dir().find(file_path)?;
    // `apps/mobile/app/x.tsx`: the match starts at the `/` that ends the
    // app's own directory.
    let root_end = if file_path[dir.start()..].starts_with('/') {
        dir.start() + 1
    } else {
        dir.start()
    };
    Some((&file_path[dir.end()..], &file_path[..root_end]))
}

/// A screen file's route path, or `None` for a file that is no screen: a
/// layout, a test, an API route (`routePathForFile`).
pub fn route_path_for_file(file_path: &str) -> Option<String> {
    static ROUTE_EXT: OnceLock<Regex> = OnceLock::new();
    static NOT_A_SCREEN: OnceLock<Regex> = OnceLock::new();
    let (rel, _) = below_app_dir(file_path)?;
    let ext = ROUTE_EXT
        .get_or_init(|| {
            Regex::new(r"\.(?:(?:ios|android|native|web)\.)?(tsx|ts|jsx|js|mjs|cjs)$")
                .expect("route ext")
        })
        .find(rel)?;
    let bare = &rel[..ext.start()];
    if bare.ends_with(".d")
        || NOT_A_SCREEN
            .get_or_init(|| Regex::new(r"\.(?:test|spec|stories)$").expect("not a screen"))
            .is_match(bare)
    {
        return None;
    }
    let segs: Vec<&str> = bare.split('/').collect();
    if segs.contains(&"__tests__") || segs.contains(&"__mocks__") {
        return None;
    }
    let base = segs.last().copied().unwrap_or_default();
    // `hello+api.ts` is an endpoint, not a screen. `_layout` (and any other
    // `_` file) is not navigable; `+not-found` is a screen, the other `+`
    // files (`+html`, `+native-intent`) are not.
    if base.ends_with("+api")
        || base.starts_with('_')
        || (base.starts_with('+') && base != "+not-found")
    {
        return None;
    }
    let mut kept: Vec<&str> = segs
        .into_iter()
        .filter(|seg| !(seg.starts_with('(') && seg.ends_with(')')))
        .collect();
    if kept.last() == Some(&"index") {
        kept.pop();
    }
    Some(format!("/{}", kept.join("/")))
}

/// An API route's path, written the way every server route is
/// (`app/blog/og-image/[post]+api.ts` is `/blog/og-image/:post`), or `None`
/// for any other file (`apiRoutePathForFile`, #2135).
pub fn api_route_path_for_file(file_path: &str) -> Option<String> {
    static API_EXT: OnceLock<Regex> = OnceLock::new();
    let (rel, _) = below_app_dir(file_path)?;
    let ext = API_EXT
        .get_or_init(|| Regex::new(r"\.(tsx|ts|jsx|js|mjs|cjs)$").expect("api ext"))
        .find(rel)?;
    let bare = rel[..ext.start()].strip_suffix("+api")?;
    let mut segs: Vec<&str> = bare
        .split('/')
        .filter(|seg| !seg.is_empty() && !(seg.starts_with('(') && seg.ends_with(')')))
        .collect();
    if segs.last() == Some(&"index") {
        segs.pop();
    }
    let segs: Vec<String> = segs
        .into_iter()
        .map(|seg| {
            if let Some(name) = seg
                .strip_prefix("[...")
                .and_then(|rest| rest.strip_suffix(']'))
                .filter(|name| !name.is_empty())
            {
                format!(":{name}*")
            } else if let Some(name) = seg
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
                .filter(|name| !name.is_empty())
            {
                format!(":{name}")
            } else {
                seg.to_string()
            }
        })
        .collect();
    Some(format!("/{}", segs.join("/")))
}

/// The router methods that take a destination (`back` and `dismiss` take
/// none), and a project's own wrappers around them (`safePush('/x')`,
/// `guardedNavigate('/x')`: a camelCase name ending in the verb)
/// (`NAV_METHOD`).
fn nav_method() -> &'static Regex {
    static NAV_METHOD: OnceLock<Regex> = OnceLock::new();
    NAV_METHOD.get_or_init(|| {
        Regex::new(
            r"(?:^|\.)(push|replace|navigate|dismissTo)$|^[a-z][A-Za-z]*(Push|Replace|Navigate)$",
        )
        .expect("navigation method")
    })
}

/// The verb a navigation call name stands for: a router method as written,
/// a wrapper's suffix lower-cased (`safePush` is `push`) (`navVerb`).
pub fn nav_verb(name: &str) -> Option<String> {
    let captures = nav_method().captures(name)?;
    if let Some(method) = captures.get(1) {
        return Some(method.as_str().to_string());
    }
    captures.get(2).map(|verb| verb.as_str().to_lowercase())
}

/// True for this framework's own screens: the route nodes whose name is the
/// path their file gives (`isScreenRoute`). Another framework's routes in
/// the same project name themselves differently and never match.
fn is_screen_route(node: &Node) -> bool {
    route_path_for_file(&node.file_path).as_deref() == Some(node.name.as_str())
}

/// One Expo app's screens, matched by path (`routeTable`).
#[derive(Debug, Default)]
struct AppScreens {
    /// Every screen by its path (all of this app's root).
    exact: ExactTable,
    /// The screens with a `[param]` or `[...rest]` segment. Matching is
    /// order-independent: a tie is nothing.
    dynamic: Vec<RouteEntry>,
}

/// The screens of every Expo app, by the directory the app lives in. A call
/// matches only its own app's screens (KEEP-RUST): upstream keeps one table
/// for the project and reads every file, so in a monorepo another app's
/// `navigate('/')` reached an Expo screen, and one Expo app's call another's.
#[derive(Debug, Default)]
struct ExpoTable {
    by_root: BTreeMap<String, AppScreens>,
}

impl ExpoTable {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut table = Self::default();
        for node in context.get_nodes_by_kind_shared(NodeKind::Route) {
            if !is_screen_route(&node) {
                continue;
            }
            let root = app_root_of(&node.file_path);
            let app = table.by_root.entry(root.clone()).or_default();
            if node.name.contains('[') {
                app.dynamic.push(RouteEntry::new(Arc::clone(&node)));
            }
            app.exact.insert(node.name.clone(), root, node);
        }
        table
    }

    /// The screens of the app `file_path` belongs to: the longest root above
    /// it, or `None` under no Expo app (`routesForFile`).
    fn app_of(&self, file_path: &str) -> Option<&AppScreens> {
        self.by_root
            .iter()
            .filter(|(root, _)| file_path.starts_with(root.as_str()))
            .max_by_key(|(root, _)| root.len())
            .map(|(_, app)| app)
    }
}

/// The directory a screen's app lives in: the path above its `app/`.
fn app_root_of(file_path: &str) -> String {
    below_app_dir(file_path)
        .map(|(_, root)| root.to_string())
        .unwrap_or_default()
}

/// The pass's Expo Router table.
fn expo_route_table(context: &dyn ResolutionContext) -> Arc<ExpoTable> {
    context
        .route_tables()
        .get_or_build("expo-router", || ExpoTable::build(context))
}

/// An href path in the form screen names use: no trailing `/`, no `(group)`
/// segment, a hole as a whole-segment `*`, a relative href resolved against
/// the directory of the screen the call sits in (`normalizeHrefPath`).
/// `None` for a relative href from a file that is no screen.
pub fn normalize_href_path(path: &str, from_file: &str) -> Option<Vec<String>> {
    let mut path = path.to_string();
    if !path.starts_with('/') {
        // Expo resolves `./x` and bare `x` against the directory of the
        // screen file the call is in: `/capture` for both
        // `capture/index.tsx` and `capture/review.tsx`.
        route_path_for_file(from_file)?;
        let dir = &from_file[..from_file.rfind('/').map_or(0, |slash| slash + 1)];
        let dir_route = route_path_for_file(&format!("{dir}index.tsx"))?;
        let parent: Vec<&str> = dir_route.split('/').skip(1).collect();
        let joined = |head: &[&str], tail: &str| {
            let mut parts: Vec<&str> = head.to_vec();
            parts.push(tail);
            format!("/{}", parts.join("/"))
        };
        path = if let Some(rest) = path.strip_prefix("./") {
            joined(&parent, rest)
        } else if path.starts_with("../") {
            let mut up = parent.clone();
            let mut rest = path.as_str();
            while let Some(after) = rest.strip_prefix("../") {
                up.pop();
                rest = after;
            }
            joined(&up, rest)
        } else {
            joined(&parent, &path)
        };
    }
    Some(
        path.split('/')
            .skip(1)
            .filter(|seg| !seg.is_empty() && !(seg.starts_with('(') && seg.ends_with(')')))
            .map(|seg| {
                if seg.contains(HOLE) {
                    "*".to_string()
                } else {
                    decode_segment(seg)
                }
            })
            .collect(),
    )
}

/// Expo Router resolver (`expoRouterResolver`).
pub struct ExpoRouterResolver;

/// The navigation calls Expo Router binds to its screens: a screen answers
/// the calls of its own app.
struct ExpoNavigation;

impl NavigationCalls for ExpoNavigation {
    // A project's own wrapper (`safePush`) is left out: its tail is its own
    // name, which a search by suffix cannot find.
    fn tails(&self) -> &[&str] {
        &["push", "replace", "navigate", "dismissTo"]
    }

    fn scope(&self, route: &Node, _context: &dyn ResolutionContext) -> Option<Vec<String>> {
        is_screen_route(route).then(|| vec![app_root_of(&route.file_path)])
    }
}

impl FrameworkResolver for ExpoRouterResolver {
    fn name(&self) -> &str {
        "expo-router"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&ROUTE_LANGUAGES)
    }

    fn app_dependencies(&self) -> Option<&'static [&'static str]> {
        Some(&["expo-router"])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        static LAYOUT: OnceLock<Regex> = OnceLock::new();
        static EXPO_CONFIG: OnceLock<Regex> = OnceLock::new();
        if super::package_deps::depends_on(context, &["expo-router"]) {
            return true;
        }
        let files = context.get_all_files_shared();
        let layout = LAYOUT.get_or_init(|| {
            Regex::new(r"(?:^|/)(?:src/)?app/_layout\.(?:tsx|jsx|ts|js)$").expect("layout")
        });
        let config = EXPO_CONFIG
            .get_or_init(|| Regex::new(r"^app\.(?:json|config\.(?:js|ts))$").expect("expo config"));
        files.iter().any(|file| layout.is_match(file))
            && files.iter().any(|file| config.is_match(file))
    }

    fn claims_reference(&self, name: &str) -> bool {
        nav_method().is_match(name)
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&ExpoNavigation)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut result = FrameworkResolverExtractionResult::default();
        let language = js_language_for(file_path);
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
        if let Some(api_path) = api_route_path_for_file(file_path) {
            // `export async function GET(request) {…}` / `export const POST =
            // …`: one endpoint per method.
            static DECL: OnceLock<Regex> = OnceLock::new();
            let decl = DECL.get_or_init(|| {
                Regex::new(&format!(
                    r"\bexport\s+(?:async\s+)?function\s+({API_METHODS})\b|\bexport\s+(?:const|let)\s+({API_METHODS})\s*="
                ))
                .expect("api export")
            });
            let stripped = strip_comments_for_regex(content, CommentLang::TypeScript);
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
                let line = stripped[..whole.start()].matches('\n').count() as i64 + 1;
                let node = framework_node(
                    format!("route:{file_path}:{line}:{method}:{api_path}"),
                    NodeKind::Route,
                    format!("{method} {api_path}"),
                    format!("{file_path}::{method}:{api_path}"),
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
        let Some(route_path) = route_path_for_file(file_path) else {
            return Some(result);
        };
        let node = framework_node(
            format!("route:{file_path}:{route_path}"),
            NodeKind::Route,
            route_path.clone(),
            format!("{file_path}::route:{route_path}"),
            file_path.to_string(),
            1,
            1,
            0,
            0,
            language,
            true,
        );
        let stripped = strip_comments_for_regex(content, CommentLang::TypeScript);
        if let Some((name, index)) = super::route_href::default_export_name(&stripped) {
            let line = stripped[..index].matches('\n').count() as i64 + 1;
            result
                .references
                .push(reference(&node.id, &name, EdgeKind::Calls, line));
        }
        result.nodes.push(node);
        Some(result)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let verb = nav_verb(&reference.reference_name)?;
        if !ROUTE_LANGUAGES.contains(&reference.language) {
            return None;
        }
        let table = expo_route_table(context);
        let app = table.app_of(&reference.file_path)?;
        // The name to find on the line: `navigate` in `router.navigate(`, or
        // the wrapper's own name in `safePush(`.
        let name = &reference.reference_name;
        let callee = name.rsplit('.').next().unwrap_or(name);
        let content = context.read_file_shared(&reference.file_path)?;
        let lines: Vec<&str> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let mut href = first_argument_text(&lines, reference.line, reference.column, callee)
            .as_deref()
            .and_then(parse_href_expression);
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
        // `cond ? a : b` names a screen per arm, and the user reaches every
        // one of them: the extra arms ride along as `also_targets`, edges of
        // their own.
        let mut targets: Vec<Arc<Node>> = Vec::new();
        let mut seen = HashSet::new();
        for arm in href.arms() {
            let Some(segs) = normalize_href_path(&arm.path, &reference.file_path) else {
                continue;
            };
            let exact = app.exact.route(&route_path(&segs), &reference.file_path);
            let Some(hit) = match_route(&segs, exact, &app.dynamic) else {
                continue;
            };
            if seen.insert(hit.id.clone()) {
                targets.push(hit);
            }
        }
        let mut targets = targets.into_iter();
        let target = targets.next()?;
        let metadata = |via: bool| {
            let mut metadata = serde_json::Map::new();
            metadata.insert("href".to_string(), serde_json::json!(href.display));
            metadata.insert("navMethod".to_string(), serde_json::json!(verb));
            if via && callee != verb {
                metadata.insert("via".to_string(), serde_json::json!(callee));
            }
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
                metadata: metadata(true),
                also_targets: targets
                    .map(|node| AlsoTarget {
                        target_node_id: node.id.clone(),
                        metadata: metadata(false),
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
        ExpoRouterResolver
            .extract(
                file,
                content,
                &FrameworkExtractionContext::without_config(""),
            )
            .expect("extract")
    }

    /// Upstream `expo-router.test.ts`, `routePathForFile`.
    #[test]
    fn a_screen_file_is_the_route_its_path_names() {
        for (file, route) in [
            ("app/index.tsx", "/"),
            ("src/app/index.tsx", "/"),
            ("src/app/object-detail.tsx", "/object-detail"),
            ("src/app/capture/index.tsx", "/capture"),
            ("src/app/capture/review/index.tsx", "/capture/review"),
            ("src/app/sheets/need-help.tsx", "/sheets/need-help"),
            ("src/app/item/[id].tsx", "/item/[id]"),
            ("src/app/docs/[...slug].tsx", "/docs/[...slug]"),
            ("src/app/(tabs)/home.tsx", "/home"),
            ("src/app/(auth)/(stack)/login.tsx", "/login"),
            ("src/app/+not-found.tsx", "/+not-found"),
            ("src/app/settings.ios.tsx", "/settings"),
            ("src/app/legacy.js", "/legacy"),
            ("apps/mobile/src/app/home.tsx", "/home"),
        ] {
            assert_eq!(route_path_for_file(file).as_deref(), Some(route), "{file}");
        }
        for file in [
            "src/app/_layout.tsx",
            "src/app/(tabs)/_layout.tsx",
            "src/app/+html.tsx",
            "src/app/+native-intent.tsx",
            "src/app/_private-helper.ts",
            "src/app/home.test.tsx",
            "src/app/__tests__/home.tsx",
            "src/app/types.d.ts",
            "src/app/styles.css",
            "src/components/thing.tsx",
            "src/appearance/theme.tsx",
            "app/blog/og-image/[post]+api.ts",
        ] {
            assert_eq!(route_path_for_file(file), None, "{file}");
        }
    }

    /// Upstream #2135: an API route is a server route's path.
    #[test]
    fn an_api_route_file_is_an_endpoint_path() {
        for (file, path) in [
            ("app/blog/og-image/[post]+api.ts", "/blog/og-image/:post"),
            ("src/app/api/[...all]+api.ts", "/api/:all*"),
            ("app/(api)/users/index+api.ts", "/users"),
            ("app/hello+api.js", "/hello"),
        ] {
            assert_eq!(
                api_route_path_for_file(file).as_deref(),
                Some(path),
                "{file}"
            );
        }
        for file in ["app/index.tsx", "src/lib/x+api.ts", "app/x+api.css"] {
            assert_eq!(api_route_path_for_file(file), None, "{file}");
        }
    }

    #[test]
    fn navigation_calls_and_wrappers_name_their_verb() {
        for (name, verb) in [
            ("router.push", Some("push")),
            ("nav.navigate", Some("navigate")),
            ("router.dismissTo", Some("dismissTo")),
            ("replace", Some("replace")),
            ("safePush", Some("push")),
            ("guardedNavigate", Some("navigate")),
            ("router.back", None),
            ("fetch", None),
            ("Push", None),
        ] {
            assert_eq!(nav_verb(name).as_deref(), verb, "{name}");
            assert_eq!(
                ExpoRouterResolver.claims_reference(name),
                verb.is_some(),
                "{name}"
            );
        }
    }

    /// Upstream `expo-router.test.ts`, `normalizeHrefPath`.
    #[test]
    fn an_href_is_normalized_and_a_relative_one_read_from_its_screen() {
        let segs = |path: &str, from: &str| normalize_href_path(path, from);
        let own = |items: &[&str]| Some(items.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(segs("/capture/", "src/services/nav.ts"), own(&["capture"]));
        assert_eq!(segs("/(tabs)/home", "src/services/nav.ts"), own(&["home"]));
        assert_eq!(segs("/a%20b", "src/services/nav.ts"), own(&["a b"]));
        assert_eq!(segs("/", "src/services/nav.ts"), own(&[]));
        assert_eq!(
            segs("./review", "src/app/capture/index.tsx"),
            own(&["capture", "review"])
        );
        assert_eq!(
            segs("review", "src/app/capture/index.tsx"),
            own(&["capture", "review"])
        );
        assert_eq!(
            segs("../home", "src/app/capture/review.tsx"),
            own(&["home"])
        );
        assert_eq!(segs("./review", "src/services/nav.ts"), None);
    }

    /// Upstream `expo-router.test.ts`, `extract`.
    #[test]
    fn a_screen_is_a_route_calling_its_default_export() {
        let result = extract(
            "src/app/object-detail.tsx",
            "import React from 'react'\n\nexport default function ObjectDetail() {\n  return null\n}\n",
        );
        assert_eq!(result.nodes.len(), 1);
        let route = &result.nodes[0];
        assert_eq!(
            (route.kind, route.name.as_str(), route.language),
            (NodeKind::Route, "/object-detail", Language::Tsx)
        );
        assert_eq!(route.id, "route:src/app/object-detail.tsx:/object-detail");
        assert_eq!(result.references.len(), 1);
        let reference = &result.references[0];
        assert_eq!(
            (
                reference.from_node_id.as_str(),
                reference.reference_name.as_str(),
                reference.reference_kind,
                reference.line
            ),
            (route.id.as_str(), "ObjectDetail", EdgeKind::Calls, 3)
        );
        for (file, src) in [
            ("src/app/_layout.tsx", "export default function L() {}"),
            ("src/services/nav.ts", "export default function x() {}"),
        ] {
            let empty = extract(file, src);
            assert!(
                empty.nodes.is_empty() && empty.references.is_empty(),
                "{file}"
            );
        }
    }

    /// Upstream #2135: one endpoint per exported method, referencing it.
    #[test]
    fn an_api_route_is_one_endpoint_per_method() {
        let result = extract(
            "app/blog/og-image/[post]+api.ts",
            "export async function GET(request: Request) { return new Response('png'); }\nexport const POST = async () => new Response('ok');\n",
        );
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            ["GET /blog/og-image/:post", "POST /blog/og-image/:post"]
        );
        assert_eq!(
            result.nodes[1].id,
            "route:app/blog/og-image/[post]+api.ts:2:POST:/blog/og-image/:post"
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
    }

    // -- resolve: a navigation call → the screen it names --------------------

    const NAV: &str = "src/services/nav.ts";

    trait Screens {
        fn screen(self, file: &str) -> Self;
    }

    impl Screens for RouteCtx {
        fn screen(self, file: &str) -> Self {
            self.routes(file, extract(file, "").nodes)
        }
    }

    fn call(name: &str, line: i64, column: i64) -> RefView {
        call_in(NAV, name, line, column)
    }

    /// Upstream `expo-router.test.ts`, `resolve`.
    fn app() -> RouteCtx {
        RouteCtx::default()
            .screen("src/app/index.tsx")
            .screen("src/app/object-detail.tsx")
            .screen("src/app/capture/index.tsx")
            .screen("src/app/item/[id].tsx")
            .screen("src/app/docs/[...slug].tsx")
            .file(
                NAV,
                concat!(
                    "import { router } from 'expo-router'\n",
                    "export function openDetail(item) {\n",
                    "  router.navigate(\n",
                    "    `/object-detail?detectionItem=${encode(item)}` as any\n",
                    "  )\n",
                    "}\n",
                    "export function openItem(id) { router.push(`/item/${id}`) }\n",
                    "export function openDoc() { router.push({ pathname: '/docs/[...slug]', params: { slug: ['a'] } }) }\n",
                    "export function openCapture() { router.replace('/capture/') }\n",
                    "export function missing() { router.push('/nowhere') }\n",
                    "export function computed(h) { router.push(h) }\n",
                    "export function notNav(list) { list.push('/capture') }\n",
                    "export function fork(x) { router.push(x ? '/capture' : '/object-detail') }\n",
                    "export function sameScreen(x) { router.push(x ? '/capture?x=1' : '/capture/') }\n",
                    "export function viaWrapper() { safePush('/capture/') }\n",
                ),
            )
    }

    #[test]
    fn a_navigation_call_navigates_to_the_screen_its_href_names() {
        let ctx = app();
        let capture = ctx.route("/capture", "src/app/capture/index.tsx");
        let resolve = |name: &str, line: i64, column: i64| {
            ExpoRouterResolver.resolve(&call(name, line, column), &ctx)
        };
        // A wrapper named for the verb, remembered as `via`.
        let wrapper = resolve("safePush", 15, 27).expect("wrapper");
        assert_eq!(wrapper.target_node_id, capture);
        let edge = wrapper.edge.as_ref().expect("edge");
        assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
        assert_eq!(
            serde_json::Value::Object(edge.metadata.clone()),
            serde_json::json!({ "href": "/capture/", "navMethod": "push", "via": "safePush" })
        );
        // A multi-line template href.
        let detail = resolve("router.navigate", 3, 2).expect("navigate");
        assert_eq!(
            detail.target_node_id,
            ctx.route("/object-detail", "src/app/object-detail.tsx")
        );
        assert_eq!(detail.resolved_by, ResolvedBy::Framework);
        assert_eq!(
            serde_json::Value::Object(detail.edge.as_ref().unwrap().metadata.clone()),
            serde_json::json!({ "href": "/object-detail?detectionItem=${…}", "navMethod": "navigate" })
        );
        // An interpolated segment fills `[id]`; a pathname object reaches a
        // catch-all; a trailing slash is the index screen.
        assert_eq!(
            resolve("router.push", 7, 29).unwrap().target_node_id,
            ctx.route("/item/[id]", "src/app/item/[id].tsx")
        );
        assert_eq!(
            resolve("router.push", 8, 28).unwrap().target_node_id,
            ctx.route("/docs/[...slug]", "src/app/docs/[...slug].tsx")
        );
        assert_eq!(
            resolve("router.replace", 9, 32).unwrap().target_node_id,
            capture
        );
        // No screen, a computed href.
        assert!(resolve("router.push", 10, 28).is_none());
        assert!(resolve("router.push", 11, 30).is_none());
        // The string naming a real screen decides, not the receiver's name.
        assert_eq!(
            resolve("list.push", 12, 32).unwrap().target_node_id,
            capture
        );
    }

    #[test]
    fn every_arm_of_a_conditional_is_an_edge_with_the_whole_href() {
        let ctx = app();
        let same = ExpoRouterResolver
            .resolve(&call("router.push", 14, 33), &ctx)
            .expect("same screen");
        assert!(same.edge.as_ref().unwrap().also_targets.is_empty());
        let fork = ExpoRouterResolver
            .resolve(&call("router.push", 13, 27), &ctx)
            .expect("fork");
        let edge = fork.edge.as_ref().unwrap();
        assert_eq!(edge.also_targets.len(), 1);
        assert_eq!(
            edge.also_targets[0].target_node_id,
            ctx.route("/object-detail", "src/app/object-detail.tsx")
        );
        // As upstream, every arm's edge carries the whole href's display.
        assert_eq!(edge.also_targets[0].metadata["href"], "/capture");
        assert_eq!(edge.also_targets[0].metadata.get("via"), None);
    }

    #[test]
    fn a_local_variable_names_the_href_it_was_given() {
        let ctx = app().file(
            NAV,
            "export function go(x) {\n  const href = x ? '/capture' : '/object-detail'\n  router.push(href)\n}\n",
        );
        let resolved = ExpoRouterResolver
            .resolve(&call("router.push", 3, 2), &ctx)
            .expect("via local");
        assert_eq!(
            resolved.target_node_id,
            ctx.route("/capture", "src/app/capture/index.tsx")
        );
        assert_eq!(resolved.edge.as_ref().unwrap().also_targets.len(), 1);
    }

    #[test]
    fn only_calls_in_scripts_are_read() {
        let ctx = app();
        let mut reference = call("router.replace", 9, 32);
        reference.reference_kind = EdgeKind::References;
        assert!(ExpoRouterResolver.resolve(&reference, &ctx).is_none());
        let mut reference = call("router.replace", 9, 32);
        reference.language = Language::Swift;
        assert!(ExpoRouterResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn a_relative_href_is_read_from_the_screen_it_is_in() {
        let ctx = app()
            .screen("src/app/capture/review.tsx")
            .file("src/app/capture/index.tsx", "router.push('./review')\n");
        let reference = call_in("src/app/capture/index.tsx", "router.push", 1, 0);
        assert_eq!(
            ExpoRouterResolver
                .resolve(&reference, &ctx)
                .map(|resolved| resolved.target_node_id),
            Some(ctx.route("/capture/review", "src/app/capture/review.tsx"))
        );
    }

    /// W7-08 (KEEP-RUST): of several screens at one path, the calling app's;
    /// several or none there is nothing.
    #[test]
    fn a_path_several_screens_serve_names_the_callers_own_or_none() {
        // `app/index.tsx` beside `app/(tabs)/index.tsx`: both are `/`.
        let ctx = RouteCtx::default()
            .screen("app/index.tsx")
            .screen("app/(tabs)/index.tsx")
            .screen("app/(tabs)/home.tsx")
            .file("lib/nav.ts", "router.push('/')\nrouter.push('/home')\n");
        let at = |ctx: &RouteCtx, file: &str, line: i64| {
            ExpoRouterResolver
                .resolve(&call_in(file, "router.push", line, 0), ctx)
                .map(|resolved| resolved.target_node_id)
        };
        assert_eq!(at(&ctx, "lib/nav.ts", 1), None);
        assert_eq!(
            at(&ctx, "lib/nav.ts", 2),
            Some(ctx.route("/home", "app/(tabs)/home.tsx"))
        );
        // Two Expo apps of a monorepo.
        let ctx = RouteCtx::default()
            .screen("apps/a/app/login.tsx")
            .screen("apps/b/src/app/login.tsx")
            .file("apps/a/lib/nav.ts", "router.push('/login')\n")
            .file("apps/b/lib/nav.ts", "router.push('/login')\n")
            .file("packages/shared/nav.ts", "router.push('/login')\n");
        assert_eq!(
            at(&ctx, "apps/a/lib/nav.ts", 1),
            Some(ctx.route("/login", "apps/a/app/login.tsx"))
        );
        assert_eq!(
            at(&ctx, "apps/b/lib/nav.ts", 1),
            Some(ctx.route("/login", "apps/b/src/app/login.tsx"))
        );
        // A file under neither app is not read.
        assert_eq!(at(&ctx, "packages/shared/nav.ts", 1), None);
    }

    #[test]
    fn expo_router_is_detected_by_its_dependency_or_a_layout_beside_its_config() {
        let ctx = |files: &[&str]| {
            files
                .iter()
                .fold(RouteCtx::default(), |ctx, file| ctx.file(file, "{}"))
        };
        assert!(ExpoRouterResolver.detect(
            &RouteCtx::default().file("package.json", "{\"dependencies\":{\"expo-router\":\"4\"}}")
        ));
        assert!(ExpoRouterResolver.detect(&ctx(&["src/app/_layout.tsx", "app.json"])));
        assert!(ExpoRouterResolver.detect(&ctx(&["app/_layout.js", "app.config.ts"])));
        assert!(!ExpoRouterResolver.detect(&ctx(&["src/app/_layout.tsx"])));
        assert!(!ExpoRouterResolver.detect(&ctx(&["app.json", "app/index.tsx"])));
        assert!(!ExpoRouterResolver.detect(&ctx(&["apps/x/app.json", "app/_layout.tsx"])));
    }

    #[test]
    fn the_navigation_hook_scopes_a_screen_to_its_own_app() {
        let ctx = app();
        let navigation = ExpoRouterResolver.navigation().expect("navigation");
        assert_eq!(
            navigation.tails(),
            ["push", "replace", "navigate", "dismissTo"]
        );
        let screen = ctx.nodes()[0].clone();
        assert_eq!(navigation.scope(&screen, &ctx), Some(vec![String::new()]));
        let mut other = screen;
        other.name = "/elsewhere".to_string();
        assert_eq!(navigation.scope(&other, &ctx), None);
        let ctx = RouteCtx::default()
            .screen("apps/mobile/app/login.tsx")
            .screen("apps/kiosk/src/app/index.tsx");
        let screen = ctx.nodes()[0].clone();
        assert_eq!(
            navigation.scope(&screen, &ctx),
            Some(vec!["apps/mobile/".to_string()])
        );
    }

    /// W7-08 (KEEP-RUST): one Expo app's call never reaches a screen only
    /// another Expo app serves, exact or dynamic.
    #[test]
    fn a_call_matches_only_its_own_apps_screens() {
        let ctx = RouteCtx::default()
            .screen("apps/a/app/index.tsx")
            .screen("apps/b/app/settings.tsx")
            .screen("apps/b/app/item/[id].tsx")
            .file(
                "apps/a/lib/nav.ts",
                "router.push('/settings')\nrouter.push(`/item/${id}`)\nrouter.push('/')\n",
            )
            .file(
                "apps/b/lib/nav.ts",
                "router.push('/settings')\nrouter.push(`/item/${id}`)\n",
            );
        let at = |file: &str, line: i64| {
            ExpoRouterResolver
                .resolve(&call_in(file, "router.push", line, 0), &ctx)
                .map(|resolved| resolved.target_node_id)
        };
        assert_eq!(at("apps/a/lib/nav.ts", 1), None);
        assert_eq!(at("apps/a/lib/nav.ts", 2), None);
        assert_eq!(
            at("apps/a/lib/nav.ts", 3),
            Some(ctx.route("/", "apps/a/app/index.tsx"))
        );
        assert_eq!(
            at("apps/b/lib/nav.ts", 1),
            Some(ctx.route("/settings", "apps/b/app/settings.tsx"))
        );
        assert_eq!(
            at("apps/b/lib/nav.ts", 2),
            Some(ctx.route("/item/[id]", "apps/b/app/item/[id].tsx"))
        );
    }

    /// W7-08 (KEEP-RUST): a call outside every Expo app is not read, though
    /// its href names a screen; upstream reads every file.
    #[test]
    fn a_call_outside_every_expo_app_is_not_read() {
        let ctx = RouteCtx::default()
            .screen("apps/mobile/app/login.tsx")
            .file("apps/mobile/lib/nav.ts", "router.push('/login')\n")
            .file("apps/web/src/main.tsx", "navigate('/login')\n");
        let at = |file: &str, name: &str| {
            ExpoRouterResolver
                .resolve(&call_in(file, name, 1, 0), &ctx)
                .map(|resolved| resolved.target_node_id)
        };
        assert_eq!(
            at("apps/mobile/lib/nav.ts", "router.push"),
            Some(ctx.route("/login", "apps/mobile/app/login.tsx"))
        );
        assert_eq!(at("apps/web/src/main.tsx", "navigate"), None);
    }
}
