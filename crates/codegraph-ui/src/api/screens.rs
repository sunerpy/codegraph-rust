//! `GET /api/screens` — the app as screens and the transitions between them.
//! Upstream `src/ui-server/api/screens.ts`.
//!
//! Screens are built from `navigates` edges into screen routes. The Rust graph
//! has no `navigates` edge kind yet (the navigation resolvers are UI family
//! F1–F6, a later phase), so every index takes upstream's own early return: no
//! navigation, `routed: false`, and the Screens view says the graph holds no
//! screen navigation instead of drawing an empty picture.

use std::time::Instant;

use serde_json::{Value, json};

use super::Ctx;
use crate::respond::ApiResult;

pub fn build(ctx: &Ctx<'_>) -> ApiResult<Value> {
    let started = Instant::now();
    let counts = ctx.store.counts()?;
    Ok(json!({
        "routed": false,
        "entry": Value::Null,
        "screens": [],
        "origins": [],
        "links": [],
        "dropped": 0,
        "index": {
            "lastIndexedAt": ctx.store.last_indexed_at()?,
            "edges": counts.edge_count,
            "files": counts.file_count,
        },
        "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
    }))
}
