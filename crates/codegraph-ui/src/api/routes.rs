//! `GET /api/routes?limit=` — the URL → handler map, when the project is a
//! routed app. Upstream `src/ui-server/api/routes.ts` over
//! `QueryBuilder.getRoutingManifest`.
//!
//! The rows come from `route` nodes the framework resolvers mint, joined to the
//! handler their `references` / `calls` edge names, one row per route (the
//! viewer keys its lists by route id). Fewer than three production rows is not
//! a routed app: the answer is then `routed: false` rather than a map of two
//! test fixtures.

use std::collections::{HashMap, HashSet};

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::NodeKind;
use codegraph_store::RoutingRow;
use serde::Serialize;
use serde_json::Value;

use super::Ctx;
use super::route_roots::{RouteRoot, route_roots};
use super::wire::{is_test_file, to_posix_path};
use crate::respond::{ApiResult, Query};

const HTTP_METHODS: &[&str] = &[
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT", "ANY", "ALL",
    "USE",
];

/// The smallest window a caller may ask for — also the routed-app floor.
const MIN_LIMIT: i64 = 3;

/// Manifest rows fetched per route shown, so folding repeats still fills the page.
const ROWS_PER_ROUTE: i64 = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRoute {
    pub url: String,
    pub method: Option<String>,
    pub path: String,
    pub handler: String,
    pub handler_kind: String,
    pub file: String,
    pub line: i64,
    pub handler_id: Option<String>,
    pub route_file: String,
    pub route_line: i64,
    pub route_id: String,
    /// The handler is written inline at the registration: the route stands in for it.
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireRoutes {
    pub routed: bool,
    pub route_count: i64,
    pub shown: usize,
    pub truncated: bool,
    pub top_handler_file: Option<String>,
    pub top_handler_file_count: usize,
    pub entries: Vec<WireRoute>,
}

/// `"GET /users/:id"` → `(Some("GET"), "/users/:id")`; a name with no verb
/// (`"/health"`, `"users#index"`) keeps its whole text as the path.
pub fn split_route_name(url: &str) -> (Option<String>, String) {
    let Some(space) = url.find(' ') else {
        return (None, url.to_string());
    };
    if space == 0 {
        return (None, url.to_string());
    }
    let head = url[..space].to_uppercase();
    if !HTTP_METHODS.contains(&head.as_str()) {
        return (None, url.to_string());
    }
    (Some(head), url[space + 1..].trim_start().to_string())
}

/// Upstream `isLowValueFile`: test suites and generated files, which the
/// manifest drops so a fixture app does not pass for the product's routes.
fn is_low_value_file(file_path: &str, generated: &HashSet<String>) -> bool {
    if generated.contains(file_path) {
        return true;
    }
    let lp = file_path.to_lowercase();
    let segments: Vec<&str> = lp.split('/').collect();
    let dir_hit = segments[..segments.len().saturating_sub(1)]
        .iter()
        .any(|s| matches!(*s, "test" | "tests" | "__test__" | "__tests__" | "spec"));
    let name = segments.last().copied().unwrap_or("");
    let python_test =
        name.starts_with("test_") && name.ends_with(".py") && name.len() > "test_.py".len();
    dir_hit
        || lp.ends_with("_test.go")
        || python_test
        || lp.ends_with("_test.py")
        || lp.ends_with("_spec.rb")
        || lp.ends_with("_test.rb")
        || [".test.", ".spec."].iter().any(|m| {
            ["js", "jsx", "ts", "tsx"]
                .iter()
                .any(|ext| lp.ends_with(&format!("{m}{ext}")))
        })
        || ["test", "spec", "tests"].iter().any(|m| {
            ["java", "kt", "scala"]
                .iter()
                .any(|ext| lp.ends_with(&format!("{m}.{ext}")))
        })
        || ["test.cs", "tests.cs", "spec.cs"]
            .iter()
            .any(|s| lp.ends_with(s))
        || lp.ends_with("test.swift")
        || lp.ends_with("tests.swift")
        || lp.ends_with("_test.dart")
        || is_generated_file(file_path)
}

/// Production route rows and the file holding the most handlers.
struct Manifest {
    rows: Vec<RoutingRow>,
    top_handler_file: Option<String>,
    top_handler_file_count: usize,
}

/// The manifest, or `None` when fewer than three production routes remain.
fn routing_manifest(ctx: &Ctx<'_>, limit: i64) -> ApiResult<Option<Manifest>> {
    let rows = ctx.store.routing_manifest_rows(limit)?;
    let handler_files: Vec<String> = rows.iter().map(|r| r.handler_file.clone()).collect();
    let generated = ctx.store.generated_paths_among(&handler_files)?;
    let filtered: Vec<RoutingRow> = rows
        .into_iter()
        .filter(|r| !is_low_value_file(&r.handler_file, &generated))
        .collect();
    if filtered.len() < MIN_LIMIT as usize {
        return Ok(None);
    }
    // The file holding the most handlers; the first to reach the top count wins.
    let mut order: Vec<&str> = Vec::new();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for row in &filtered {
        let count = counts.entry(row.handler_file.as_str()).or_insert_with(|| {
            order.push(row.handler_file.as_str());
            0
        });
        *count += 1;
    }
    let mut top: Option<String> = None;
    let mut top_count = 0;
    for file in order {
        let count = counts[file];
        if count > top_count {
            top = Some(file.to_string());
            top_count = count;
        }
    }
    Ok(Some(Manifest {
        rows: filtered,
        top_handler_file: top,
        top_handler_file_count: top_count,
    }))
}

/// The manifest's rows folded to one per route, in its order: the row naming
/// the route's root (the handler a resolver bound, or the component a screen
/// renders), else the route's first row. A route whose root is its own inline
/// body is that route, labelled as an inline handler rather than as whichever
/// call in the body sorted first, and is left out when it is written in a
/// test, as the manifest leaves out test handlers.
fn fold_rows(rows: Vec<RoutingRow>, roots: &HashMap<String, RouteRoot>) -> Vec<(RoutingRow, bool)> {
    let mut order: Vec<String> = Vec::new();
    let mut by_route: HashMap<String, Vec<RoutingRow>> = HashMap::new();
    for row in rows {
        by_route
            .entry(row.route_id.clone())
            .or_insert_with(|| {
                order.push(row.route_id.clone());
                Vec::new()
            })
            .push(row);
    }
    let mut out = Vec::with_capacity(order.len());
    for route_id in order {
        let mut list = by_route.remove(&route_id).unwrap_or_default();
        if list.is_empty() {
            continue;
        }
        let root = roots.get(&route_id);
        if root.is_some_and(|root| root.inline) {
            let first = list.swap_remove(0);
            if is_test_file(&first.route_file) {
                continue;
            }
            out.push((
                RoutingRow {
                    handler: "inline handler".to_string(),
                    handler_kind: "route".to_string(),
                    handler_file: first.route_file.clone(),
                    handler_line: first.route_line,
                    ..first
                },
                true,
            ));
            continue;
        }
        let named = root.and_then(|root| {
            list.iter().position(|row| {
                row.handler_file == root.node.file_path
                    && row.handler_line == root.node.start_line
                    && row.handler == root.node.name
            })
        });
        out.push((list.swap_remove(named.unwrap_or(0)), false));
    }
    out
}

/// The route map with at most `limit` rows.
pub fn build_routes(ctx: &Ctx<'_>, limit: i64) -> ApiResult<WireRoutes> {
    // The manifest is a row per (route, edge): a route bound to two symbols,
    // or an inline handler whose every call reads as a "handler", repeats, and
    // a repeated route id stops the viewer drawing the list (upstream #2264).
    // A route has ONE answer to "what serves this", route_roots.rs's, so rows
    // are over-fetched and folded onto it; the extra row says whether there
    // were more.
    let fetched = limit * ROWS_PER_ROUTE + 1;
    let manifest = routing_manifest(ctx, fetched)?;
    let route_count = ctx.store.count_nodes_of_kind(NodeKind::Route)?;
    let Some(Manifest {
        rows: entries,
        top_handler_file,
        top_handler_file_count,
    }) = manifest
    else {
        return Ok(WireRoutes {
            routed: false,
            route_count,
            shown: 0,
            truncated: false,
            top_handler_file: None,
            top_handler_file_count: 0,
            entries: Vec::new(),
        });
    };
    let fetched_rows = entries.len() as i64;
    let mut route_ids: Vec<String> = entries.iter().map(|row| row.route_id.clone()).collect();
    route_ids.sort();
    route_ids.dedup();
    let routes: Vec<_> = ctx.store.nodes_by_ids(&route_ids)?.into_values().collect();
    let roots = route_roots(ctx, &routes)?;
    let folded = fold_rows(entries, &roots);
    let truncated = folded.len() as i64 > limit || fetched_rows >= fetched;
    let rows: Vec<(RoutingRow, bool)> = folded.into_iter().take(limit as usize).collect();

    // Every row's handler file in one batched query, so a project that
    // scatters handlers across hundreds of files still costs one query per
    // chunk, and no row past a cap is reported as "not in the index" (#1975).
    let handler_files: Vec<String> = rows.iter().map(|(r, _)| r.handler_file.clone()).collect();
    let mut by_file_line_name: HashMap<(String, i64, String), String> = HashMap::new();
    for node in ctx.store.nodes_in_files(&handler_files)? {
        // Keyed on what the manifest knows: file, line and name. Two symbols can
        // share a line (a decorator and its method); the name breaks the tie,
        // and a miss leaves that entry unlinked.
        by_file_line_name.insert(
            (node.file_path.clone(), node.start_line, node.name.clone()),
            node.id.clone(),
        );
    }

    let entries: Vec<WireRoute> = rows
        .into_iter()
        .map(|(row, inline)| {
            let (method, path) = split_route_name(&row.url);
            // An inline handler has no node of its own; the route stands in.
            let handler_id = if inline {
                Some(row.route_id.clone())
            } else {
                by_file_line_name
                    .get(&(
                        row.handler_file.clone(),
                        row.handler_line,
                        row.handler.clone(),
                    ))
                    .cloned()
            };
            WireRoute {
                method,
                path,
                handler_id,
                file: to_posix_path(&row.handler_file),
                line: row.handler_line,
                route_file: to_posix_path(&row.route_file),
                route_line: row.route_line,
                route_id: row.route_id,
                handler_kind: row.handler_kind,
                handler: row.handler,
                url: row.url,
                inline,
            }
        })
        .collect();

    Ok(WireRoutes {
        routed: true,
        route_count,
        shown: entries.len(),
        truncated,
        top_handler_file: top_handler_file.map(|f| to_posix_path(&f)),
        top_handler_file_count,
        entries,
    })
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let limit = query.int("limit", MIN_LIMIT, 500, Some(200))?;
    Ok(serde_json::to_value(build_routes(ctx, limit)?).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Language, Node};

    fn row(route: &str, line: i64, handler: &str, handler_line: i64) -> RoutingRow {
        RoutingRow {
            url: format!("GET /{route}"),
            route_id: format!("route:{route}"),
            route_file: "server/app.js".to_string(),
            route_line: line,
            handler: handler.to_string(),
            handler_file: "server/app.js".to_string(),
            handler_line,
            handler_kind: "function".to_string(),
        }
    }

    fn root(route: &RoutingRow, name: &str, line: i64, inline: bool) -> (String, RouteRoot) {
        let node = Node {
            id: format!("function:{name}"),
            kind: if inline {
                NodeKind::Route
            } else {
                NodeKind::Function
            },
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "server/app.js".to_string(),
            language: Language::JavaScript,
            start_line: line,
            end_line: line,
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
        };
        (route.route_id.clone(), RouteRoot { node, inline })
    }

    /// Upstream #2264: hono's `GET /stream/text` came back once per call in its
    /// inline handler, and a repeated route id blanked Entry points.
    #[test]
    fn folds_the_manifest_to_one_row_per_route_in_its_order() {
        let card = [
            row("card", 11, "loadUser", 4),
            row("card", 11, "audit", 5),
            row("card", 11, "render", 6),
        ];
        let show = [
            row("show", 18, "loadUser", 4),
            row("show", 18, "showUser", 8),
        ];
        let list = row("list", 17, "listUsers", 7);
        let roots: HashMap<String, RouteRoot> = [
            root(&card[0], "card", 11, true),
            root(&show[0], "showUser", 8, false),
        ]
        .into_iter()
        .collect();
        let rows: Vec<RoutingRow> = card
            .iter()
            .chain([&list])
            .chain(show.iter())
            .cloned()
            .collect();
        let folded = fold_rows(rows, &roots);
        let seen: Vec<(&str, &str, &str, bool)> = folded
            .iter()
            .map(|(r, inline)| {
                (
                    r.route_id.as_str(),
                    r.handler.as_str(),
                    r.handler_kind.as_str(),
                    *inline,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("route:card", "inline handler", "route", true),
                ("route:list", "listUsers", "function", false),
                ("route:show", "showUser", "function", false),
            ]
        );
        // An inline handler is placed where the route is written.
        assert_eq!(folded[0].0.handler_line, 11);
    }

    #[test]
    fn an_inline_handler_written_in_a_test_is_left_out() {
        let mut probe = row("probe", 3, "check", 9);
        probe.route_file = "test/app.test.js".to_string();
        let roots: HashMap<String, RouteRoot> =
            [root(&probe, "probe", 3, true)].into_iter().collect();
        assert!(fold_rows(vec![probe], &roots).is_empty());
    }

    #[test]
    fn takes_the_verb_off_when_there_is_one() {
        assert_eq!(
            split_route_name("POST /v1/users"),
            (Some("POST".into()), "/v1/users".into())
        );
        assert_eq!(
            split_route_name("ANY /healthz"),
            (Some("ANY".into()), "/healthz".into())
        );
    }

    #[test]
    fn leaves_a_file_routed_page_whole() {
        assert_eq!(
            split_route_name("/blog/[slug]"),
            (None, "/blog/[slug]".into())
        );
        assert_eq!(
            split_route_name("user.created handler"),
            (None, "user.created handler".into())
        );
    }

    #[test]
    fn a_route_name_splits_on_a_leading_verb_only() {
        assert_eq!(
            split_route_name("GET /users/:id"),
            (Some("GET".into()), "/users/:id".into())
        );
        assert_eq!(
            split_route_name("post   /login"),
            (Some("POST".into()), "/login".into())
        );
        assert_eq!(split_route_name("/health"), (None, "/health".into()));
        assert_eq!(
            split_route_name("users#index show"),
            (None, "users#index show".into())
        );
        assert_eq!(split_route_name(" GET /x"), (None, " GET /x".into()));
    }

    #[test]
    fn test_suites_and_generated_handlers_are_low_value() {
        let none = HashSet::new();
        for path in [
            "spec/routes.rb",
            "app/tests/urls.py",
            "src/__tests__/app.js",
            "pkg/server_test.go",
            "tests/test_app.py",
            "app/routes_spec.rb",
            "src/app.test.ts",
            "src/AppTest.java",
            "Api/ControllerTests.cs",
            "Sources/RouterTests.swift",
            "lib/router_test.dart",
            "src/api.pb.go",
        ] {
            assert!(is_low_value_file(path, &none), "{path}");
        }
        for path in [
            "app/controllers/users_controller.rb",
            "src/server.ts",
            "test_.py",
            "contest/app.ts",
        ] {
            assert!(!is_low_value_file(path, &none), "{path}");
        }
        let flagged: HashSet<String> = ["src/gen.ts".to_string()].into_iter().collect();
        assert!(is_low_value_file("src/gen.ts", &flagged));
    }
}
