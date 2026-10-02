//! `GET /api/nodes?id=…` — names and locations for ids a caller already holds
//! (a trail's hops, a saved flow). Upstream `src/ui-server/api/nodes.ts`.

use serde_json::{Value, json};

use super::Ctx;
use super::wire::to_node_ref;
use crate::respond::{ApiError, ApiResult, Query};

/// Ids answered per request.
pub const MAX_NODE_REFS: usize = 60;

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let ids: Vec<&str> = query
        .get_all("id")
        .into_iter()
        .filter(|id| !id.is_empty())
        .collect();
    if ids.is_empty() {
        return Err(ApiError::bad_request("No ids were given.")
            .with_hint("Use /api/nodes?id=<id>&id=<id> — one `id` parameter per symbol."));
    }
    if ids.len() > MAX_NODE_REFS {
        return Err(ApiError::bad_request(format!(
            "Too many ids: {}. At most {MAX_NODE_REFS} per request.",
            ids.len()
        )));
    }
    let mut unique: Vec<String> = Vec::new();
    for id in ids {
        if !unique.iter().any(|u| u == id) {
            unique.push(id.to_string());
        }
    }
    let by_id = ctx.store.nodes_by_ids(&unique)?;
    let mut items = Vec::new();
    let mut missing = Vec::new();
    // Answer in the order asked, so the caller never has to re-sort.
    for id in &unique {
        match by_id.get(id) {
            Some(node) => items.push(to_node_ref(node)),
            None => missing.push(id.clone()),
        }
    }
    Ok(json!({ "items": items, "missing": missing }))
}
