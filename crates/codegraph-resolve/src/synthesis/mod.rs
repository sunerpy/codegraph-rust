//! Dynamic-dispatch edge synthesis (upstream `resolution/callback-synthesizer.ts`
//! and `db/synthesis-stage.ts`, W7-14).
//!
//! Static extraction leaves out the edge from a dispatcher to the callbacks
//! registered elsewhere through a shared store: `triggerUpdate()` loops over
//! `this.callbacks` calling each, and nothing there names the
//! `triggerRender` that `scene.onUpdate(this.triggerRender)` registered.
//! After base resolution a whole-graph stage reads the base graph, runs each
//! pass, merges their edges in registry order and replaces every synthesized
//! edge with the result in one transaction. A synthesized edge has
//! `provenance: "heuristic"` and `metadata.synthesizedBy` naming its pass,
//! with `registeredAt` where the channel was wired up.
//!
//! The stage reads only the base graph (every edge without `synthesizedBy`,
//! the nodes, `resolved_refs`) and the indexed files' text, so a sync that
//! changes none of them keeps its edges. `codegraph-watch` recomputes them
//! whole after any sync that changes the index, under [`VERSION_KEY`].

mod callback;
mod dispatch;
mod iface;
mod registry;
mod stage;

pub use stage::{StageStats, run_stage, synthesize};

use anyhow::Result;
use codegraph_core::types::Edge;
use codegraph_store::Store;

/// `project_metadata` key of the marker a sync reads: [`VERSION`] while the
/// synthesized edges are the stage's output over the current base graph.
/// A sync withdraws it before its first change and writes it back after its
/// last write, so a sync that stops between the two leaves it absent and the
/// next one recomputes.
pub const VERSION_KEY: &str = "synthesis_version";

/// The marker's value; a change to what the passes produce moves it.
pub const VERSION: &str = "3";

/// The edges one channel, class or declaration links at most (upstream
/// `MAX_CALLBACKS_PER_CHANNEL`).
const MAX_CALLBACKS_PER_CHANNEL: usize = 40;

/// Record that the synthesized edges are the stage's output.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a change to the base graph.
pub fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// Whether the marker holds for this engine's passes.
pub fn marker_is_current(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() == Some(VERSION))
}

/// Whether the synthesis stage owns `edge`: its metadata names a pass, as
/// the store's `synthesizedBy` predicate reads it (a JSON `null` names none).
pub fn is_synthesized(edge: &Edge) -> bool {
    edge.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("synthesizedBy"))
        .is_some_and(|by| !by.is_null())
}
