//! The JSON API under `/api/` — upstream `src/ui-server/api/index.ts`.
//!
//! Reads answer GET and HEAD; the one write is a saved trail. Every handler is
//! synchronous SQLite work, so it runs on a blocking thread with a request-scoped
//! read-only store (see [`crate::session`]), and every outcome — refusals and
//! internal errors included — is JSON.

pub mod boundary;
pub mod deadcode;
pub mod entrypoints;
pub mod file;
pub mod filecode;
pub mod flow;
pub mod map;
pub mod node;
pub mod nodes;
pub mod routes;
pub mod screens;
pub mod search;
pub mod source;
pub mod stats;
pub mod trails;
pub mod wire;

use std::sync::Arc;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use codegraph_store::Store;
use serde_json::{Value, json};

use crate::respond::{ApiError, ApiResult, Query};
use crate::server::{AppState, json as json_response};
use crate::session;

/// What a handler gets: the project, its open index, and the server state.
pub struct Ctx<'a> {
    pub state: &'a AppState,
    pub store: &'a Store,
}

impl Ctx<'_> {
    pub fn project_root(&self) -> &std::path::Path {
        &self.state.project_root
    }
}

/// What `GET /api` answers: the endpoints this server serves. `/api/steps` is
/// not among them until its port lands (the frontend's adapter is built without
/// it, so the Steps view says it cannot draw steps).
fn api_index() -> Value {
    json!({
        "name": "codegraph ui",
        "readOnly": false,
        "writes": ["POST /api/trails", "DELETE /api/trails/<id>"],
        "endpoints": [
            { "path": "/api/stats", "description": "Index state, graph counts, detected frameworks." },
            { "path": "/api/search", "description": "Ranked symbol search.", "params": ["q", "limit"] },
            { "path": "/api/node/<id>", "description": "One symbol: callers, callees, members, type hierarchy, tests, blast radius." },
            { "path": "/api/nodes", "description": "Names and locations for ids you already have.", "params": ["id"] },
            { "path": "/api/source", "description": "Verbatim source for an indexed file, omitted when it has drifted on disk.", "params": ["file", "from", "to"] },
            { "path": "/api/file/<path>", "description": "One file: outline and import rails." },
            { "path": "/api/filecode/<path>", "description": "One file, line by line: call sites, unresolved references and the calls that stay inside it." },
            { "path": "/api/routes", "description": "URL to handler map, when the project is a routed app.", "params": ["limit"] },
            { "path": "/api/map", "description": "The repository at module granularity: modules, cross-module links, cycles.", "params": ["root", "depth"] },
            { "path": "/api/screens", "description": "The app as screens and the transitions between them, each with the conditions it runs under.", "params": [] },
            { "path": "/api/flow", "description": "The call path between symbols: one hop per card, opened at the calling line.", "params": ["from", "to", "symbols", "hop", "limit"] },
            { "path": "/api/events", "description": "Live channel (server-sent events): source files that changed on disk, and the index moving." },
            { "path": "/api/deadcode", "description": "Symbols nothing in the index reaches, grouped by file, with every reason a candidate was excluded.", "params": ["limit", "kinds", "exported", "tests", "generated"] },
            { "path": "/api/entrypoints", "description": "Where to start reading: routes, files that run something, and hubs.", "params": ["limit"] },
            { "path": "/api/trails", "description": "Saved trails, each hop re-resolved against the current index. POST saves one, DELETE /api/trails/<id> removes it. The only endpoint that writes." }
        ]
    })
}

/// Drop a single trailing slash, so `/api/stats/` and `/api/stats` are one route.
fn normalize(pathname: &str) -> &str {
    match pathname.strip_suffix('/') {
        Some(stripped) if pathname.len() > 4 => stripped,
        _ => pathname,
    }
}

fn ok(payload: &Value, method: &str) -> Response {
    json_response(StatusCode::OK, payload, method == "HEAD")
}

pub fn fail(err: &ApiError, method: &str) -> Response {
    json_response(
        StatusCode::from_u16(err.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        &err.body(),
        method == "HEAD",
    )
}

/// Run one read handler on a blocking thread against a fresh read-only store.
async fn read<F>(state: Arc<AppState>, method: &str, build: F) -> Response
where
    F: FnOnce(&Ctx<'_>) -> ApiResult<Value> + Send + 'static,
{
    let outcome = tokio::task::spawn_blocking(move || {
        let store = session::open_store(&state.project_root, &state.paths)?;
        let ctx = Ctx {
            state: &state,
            store: &store,
        };
        build(&ctx)
    })
    .await;
    match outcome {
        Ok(Ok(payload)) => ok(&payload, method),
        Ok(Err(err)) => fail(&err, method),
        Err(join) => fail(
            &ApiError::internal(format!("The request failed: {join}")),
            method,
        ),
    }
}

/// Route one `/api/` request. `pathname` is already decoded and checked.
pub async fn dispatch(
    state: Arc<AppState>,
    method: &str,
    pathname: &str,
    query: Query,
    body: Result<Vec<u8>, ApiError>,
) -> Response {
    let route = normalize(pathname).to_string();
    let method = method.to_string();

    if method == "POST" || method == "DELETE" {
        return dispatch_write(state, &method, &route, body).await;
    }

    match route.as_str() {
        "/api" => ok(&api_index(), &method),
        "/api/stats" => read(state, &method, stats::build).await,
        "/api/search" => read(state, &method, move |ctx| search::build(ctx, &query)).await,
        "/api/routes" => read(state, &method, move |ctx| routes::build(ctx, &query)).await,
        "/api/map" => read(state, &method, move |ctx| map::build(ctx, &query)).await,
        "/api/screens" => read(state, &method, screens::build).await,
        "/api/deadcode" => read(state, &method, move |ctx| deadcode::build(ctx, &query)).await,
        "/api/entrypoints" => {
            read(state, &method, move |ctx| entrypoints::build(ctx, &query)).await
        }
        "/api/trails" => read(state, &method, trails::build_list).await,
        "/api/nodes" => read(state, &method, move |ctx| nodes::build(ctx, &query)).await,
        "/api/source" => read(state, &method, move |ctx| source::build(ctx, &query)).await,
        "/api/flow" => read(state, &method, move |ctx| flow::build(ctx, &query)).await,
        "/api/events" => crate::events::subscribe(state, &method).await,
        // Steps needs the effect/program builders and the branch conditions
        // (UI families F1–F6 and F11), which are not ported yet; the frontend
        // is built without it, so its view says it cannot draw steps.
        "/api/steps" => fail(
            &ApiError::not_found(
                "No such endpoint: /api/steps",
                Some(
                    "This server does not build Steps yet: the step, effect and program builders \
                     (UI families F1–F6) and their branch conditions (F11) are not ported. \
                     GET /api lists everything this server answers.",
                ),
            ),
            &method,
        ),
        _ => dispatch_path_routes(state, &method, &route).await,
    }
}

/// Routes that carry a path argument: `/api/node/<id>`, `/api/filecode/<path>`,
/// `/api/file/<path>`. The argument is already decoded and keeps its slashes.
async fn dispatch_path_routes(state: Arc<AppState>, method: &str, route: &str) -> Response {
    if let Some(id) = route.strip_prefix("/api/node/") {
        if id.is_empty() {
            return fail(
                &ApiError::bad_request("No symbol id was given. Use /api/node/<id>."),
                method,
            );
        }
        let id = id.to_string();
        return read(state, method, move |ctx| node::build(ctx, &id)).await;
    }
    // Before `/api/file/`, so a later `/api/file…` sibling cannot silently start
    // matching the shorter prefix.
    if let Some(path) = route.strip_prefix("/api/filecode/") {
        let path = path.to_string();
        return read(state, method, move |ctx| filecode::build(ctx, &path)).await;
    }
    if let Some(path) = route.strip_prefix("/api/file/") {
        if path.is_empty() {
            return fail(
                &ApiError::bad_request("No file path was given. Use /api/file/<path>."),
                method,
            );
        }
        let path = path.to_string();
        return read(state, method, move |ctx| file::build(ctx, &path)).await;
    }
    if route == "/api/filecode" {
        return fail(
            &ApiError::bad_request("/api/filecode needs an argument: /api/filecode/<path>."),
            method,
        );
    }
    if route == "/api/node" || route == "/api/file" {
        let what = if route.ends_with("node") {
            "id"
        } else {
            "path"
        };
        return fail(
            &ApiError::bad_request(format!("{route} needs an argument: {route}/<{what}>.")),
            method,
        );
    }
    fail(
        &ApiError::not_found(
            format!("No such endpoint: {route}"),
            Some("GET /api lists everything this server answers."),
        ),
        method,
    )
}

async fn dispatch_write(
    state: Arc<AppState>,
    method: &str,
    route: &str,
    body: Result<Vec<u8>, ApiError>,
) -> Response {
    enum Write {
        Save(Vec<u8>),
        Delete(String),
    }
    let write = if method == "POST" && route == "/api/trails" {
        match body {
            Ok(body) => Write::Save(body),
            Err(err) => return fail(&err, method),
        }
    } else if method == "DELETE"
        && let Some(id) = route
            .strip_prefix("/api/trails/")
            .filter(|id| !id.is_empty())
    {
        Write::Delete(id.to_string())
    } else if method == "DELETE" && route == "/api/trails" {
        return fail(
            &ApiError::bad_request("Deleting a trail needs its id: DELETE /api/trails/<id>."),
            method,
        );
    } else {
        // Anything else that arrives with a write method names the endpoint
        // that does accept one.
        let mut response = fail(
            &ApiError::bad_request(format!("{method} {route} is not something this server changes.")).with_hint(
                "The only endpoint that writes is /api/trails (POST to save, DELETE /api/trails/<id> to remove).",
            ),
            method,
        );
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    };
    let method_owned = method.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        let store = session::open_store(&state.project_root, &state.paths)?;
        let ctx = Ctx {
            state: &state,
            store: &store,
        };
        match write {
            Write::Save(body) => trails::save(&ctx, &body),
            Write::Delete(id) => trails::delete(&ctx, &id),
        }
    })
    .await;
    match outcome {
        Ok(Ok(payload)) => ok(&payload, &method_owned),
        Ok(Err(err)) => fail(&err, &method_owned),
        Err(join) => fail(
            &ApiError::internal(format!("The request failed: {join}")),
            &method_owned,
        ),
    }
}
