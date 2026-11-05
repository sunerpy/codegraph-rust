//! GoFrame route → controller method (`goframeRouteEdges`, upstream
//! `goframe-synthesizer.ts` at `e8ddc4e2`, #747).
//!
//! GoFrame binds a route reflectively, so the route a request type's
//! `g.Meta` tag declares (the [`crate::frameworks::goframe`] resolver's
//! `route` node) has no static edge to the method that serves it. The
//! method's name is free (`DeptSearchReq` is served by `List`); what joins
//! them is the request type in the method's parameters:
//!
//! ```go
//! func (c *sysDeptController) Add(ctx context.Context, req *system.DeptAddReq) (…)
//! ```
//!
//! A Go method's signature is its parameters and result, so no source is
//! read. The pass writes `calls` from the route to the method
//! (`goframe-route`): the package-qualified type first, the bare type as the
//! fallback for a same-package or aliased parameter, except a method whose
//! qualified parameter is another route's type (OD-B16); several candidates
//! narrow to the controller directories, then to the route's own addon, and
//! link only when one is left.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::json;

use super::dispatch::{place, synthesized};
use super::stage::View;
use crate::frameworks::goframe::GOFRAME_ROUTE_MARKER;
use crate::frameworks::js_scan::JS_SPACE;

/// A backstop over the whole pass; real apps are one route to one method
/// (`FANOUT_CAP`).
const FANOUT_CAP: usize = 2000;

/// A pointer parameter's type, qualified or bare: `*cash.ListReq`,
/// `*ListReq` (`pointerParamTypes`).
static POINTER_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&r"\*\s*(?:([A-Za-z0-9_]+)\.)?([A-Z][A-Za-z0-9_]*)(?-u:\b)".replace(r"\s", JS_SPACE))
        .expect("pointer parameter")
});
/// A path under a controller directory (`/\/controller(s)?\//`).
static CONTROLLER_DIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/controllers?/").expect("controller directory"));
/// The addon module a path lives under (`addonRoot`).
static ADDON_ROOT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|/)addons/([^/]+)/").expect("addon root"));

/// The pointer-parameter types of a Go signature, each match's qualified
/// form (when it has one) then its bare one: `(ctx context.Context, req
/// *cash.ListReq)` gives `cash.ListReq`, `ListReq` (`pointerParamTypes`).
/// The result's pointers come too and match no request type.
fn pointer_param_types(signature: &str) -> Vec<String> {
    let mut out = Vec::new();
    for captures in POINTER_PARAM.captures_iter(signature) {
        if let Some(package) = captures.get(1) {
            out.push(format!("{}.{}", package.as_str(), &captures[2]));
        }
        out.push(captures[2].to_string());
    }
    out
}

/// The addon a path lives under (`addons/hgexample/…` is `hgexample`), or
/// `""` for the core app (`addonRoot`): an addon that clones the module tree
/// has the same packages and request types as the core.
fn addon_root(path: &str) -> &str {
    ADDON_ROOT
        .captures(path)
        .and_then(|captures| captures.get(1))
        .map_or("", |root| root.as_str())
}

/// The one handler of a route among the methods taking its request type
/// (`selectHandler`): the only one; else the only one under a controller
/// directory (all of them when none is); else the only one of the route's
/// own addon; else none.
fn select_handler<'n>(candidates: &[&'n Node], route_file: &str) -> Option<&'n Node> {
    if let [handler] = candidates {
        return Some(handler);
    }
    let mut kept: Vec<&Node> = candidates
        .iter()
        .copied()
        .filter(|handler| CONTROLLER_DIR.is_match(&handler.file_path))
        .collect();
    if kept.is_empty() {
        kept = candidates.to_vec();
    }
    if let [handler] = kept.as_slice() {
        return Some(handler);
    }
    let root = addon_root(route_file);
    match kept
        .into_iter()
        .filter(|handler| addon_root(&handler.file_path) == root)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [handler] => Some(handler),
        _ => None,
    }
}

/// Each GoFrame route to the controller method that serves it
/// (`goframeRouteEdges`).
pub(super) fn route_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    // The routes by their request type, keys in the order they first
    // appear; `wanted` holds each key and its bare type.
    let mut keys: Vec<String> = Vec::new();
    let mut routes: HashMap<String, Vec<Node>> = HashMap::new();
    let mut wanted: HashSet<String> = HashSet::new();
    for route in view.nodes_of_kinds_in(&[NodeKind::Route], &[Language::Go])? {
        let Some(at) = route.qualified_name.find(GOFRAME_ROUTE_MARKER) else {
            continue;
        };
        let key = route.qualified_name[at + GOFRAME_ROUTE_MARKER.len()..].to_string();
        if key.is_empty() {
            continue;
        }
        if let Some(dot) = key.rfind('.') {
            wanted.insert(key[dot + 1..].to_string());
        }
        wanted.insert(key.clone());
        let entry = routes.entry(key.clone()).or_default();
        if entry.is_empty() {
            keys.push(key);
        }
        entry.push(route);
    }
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    // The Go methods taking a wanted type by pointer, under each of its
    // wanted forms, in node order (a method naming one type twice is there
    // twice, as upstream pushes it per match).
    let methods = view.nodes_of_kinds_in(&[NodeKind::Method], &[Language::Go])?;
    let mut handlers: HashMap<String, Vec<&Node>> = HashMap::new();
    for method in &methods {
        let Some(signature) = method.signature.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        for form in pointer_param_types(signature) {
            if wanted.contains(&form) {
                handlers.entry(form).or_default().push(method);
            }
        }
    }
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for key in &keys {
        let bare = key.rfind('.').map_or(key.as_str(), |dot| &key[dot + 1..]);
        // The precise package-qualified match first; the bare type only as
        // the fallback for a same-package or aliased parameter, leaving out
        // a method whose qualified parameter of this type is another route's
        // key: it is that route's (OD-B16, KEEP-RUST; upstream keeps it).
        let candidates: Vec<&Node> = match handlers.get(key) {
            Some(precise) => precise.clone(),
            None => handlers
                .get(bare)
                .into_iter()
                .flatten()
                .copied()
                .filter(|method| !claimed_by_another_route(method, key, bare, &routes))
                .collect(),
        };
        if candidates.is_empty() {
            continue;
        }
        for route in &routes[key] {
            let Some(handler) = select_handler(&candidates, &route.file_path) else {
                continue;
            };
            if route.id == handler.id {
                continue;
            }
            if edges.len() >= FANOUT_CAP || !seen.insert((route.id.clone(), handler.id.clone())) {
                continue;
            }
            edges.push(synthesized(
                route,
                handler,
                EdgeKind::Calls,
                route.start_line,
                json!({
                    "synthesizedBy": "goframe-route",
                    "route": route.name,
                    "requestType": bare,
                    "registeredAt": place(handler),
                }),
            ));
        }
    }
    Ok(edges)
}

/// Whether `method` takes `bare` qualified by another package whose type is
/// the key of another route than `key`'s (OD-B16).
fn claimed_by_another_route(
    method: &Node,
    key: &str,
    bare: &str,
    routes: &HashMap<String, Vec<Node>>,
) -> bool {
    let signature = method.signature.as_deref().unwrap_or_default();
    pointer_param_types(signature).iter().any(|form| {
        form != key
            && form.rsplit_once('.').is_some_and(|(_, tail)| tail == bare)
            && routes.contains_key(form)
    })
}
