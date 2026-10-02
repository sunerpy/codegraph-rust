//! `GET /api/routes?limit=` — the URL → handler map, when the project is a
//! routed app. Upstream `src/ui-server/api/routes.ts` over
//! `QueryBuilder.getRoutingManifest`.
//!
//! The rows come from `route` nodes the framework resolvers mint, joined to the
//! handler their `references` / `calls` edge names. Fewer than three
//! production routes is not a routed app: the answer is then `routed: false`
//! rather than a map of two test fixtures.

use std::collections::{HashMap, HashSet};

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::NodeKind;
use codegraph_store::RoutingRow;
use serde::Serialize;
use serde_json::Value;

use super::Ctx;
use super::wire::to_posix_path;
use crate::respond::{ApiResult, Query};

const HTTP_METHODS: &[&str] = &[
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT", "ANY", "ALL",
    "USE",
];

/// The smallest window a caller may ask for — also the routed-app floor.
const MIN_LIMIT: i64 = 3;

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

/// The route map with at most `limit` rows.
pub fn build_routes(ctx: &Ctx<'_>, limit: i64) -> ApiResult<WireRoutes> {
    // One row over the limit, purely to learn whether there were more.
    let manifest = routing_manifest(ctx, limit + 1)?;
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
    let truncated = entries.len() as i64 > limit;
    let rows: Vec<RoutingRow> = entries.into_iter().take(limit as usize).collect();

    // Every row's handler file in one batched query, so a project that
    // scatters handlers across hundreds of files still costs one query per
    // chunk, and no row past a cap is reported as "not in the index" (#1975).
    let handler_files: Vec<String> = rows.iter().map(|r| r.handler_file.clone()).collect();
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
        .map(|row| {
            let (method, path) = split_route_name(&row.url);
            let handler_id = by_file_line_name
                .get(&(
                    row.handler_file.clone(),
                    row.handler_line,
                    row.handler.clone(),
                ))
                .cloned();
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
