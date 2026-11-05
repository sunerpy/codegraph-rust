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
//! with `registeredAt` where the channel was wired up; a Go type's
//! `contains` edge to a method of another file (`go-method-contains`) has
//! no provenance, as a parsed one has.
//!
//! The stage reads only the base graph (every edge without `synthesizedBy`,
//! the nodes, `resolved_refs`, and for Go the `extends` rows resolution
//! left in `unresolved_refs`), the indexed files' text, the files the C
//! function-pointer pass `#include`s that the index does not hold, which it
//! records under [`INPUTS_KEY`] (W7-17b), and, for the link passes' import
//! resolution, the JavaScript configuration resolution reads (`tsconfig.json`
//! and `jsconfig.json` aliases, workspace packages, `package.json`
//! manifests), JavaScript's control inputs (W7-18a), so a sync that changes
//! none of them keeps its edges. Its Go prerequisite steps add edges the passes read
//! as base edges first (W7-15b). `codegraph-watch` recomputes them whole
//! after any sync that changes the index or finds a recorded file reading
//! differently, under [`VERSION_KEY`].

mod c_fnptr;
mod callback;
mod context;
mod dispatch;
mod framework;
mod go;
mod go_dispatch;
mod go_func_field;
mod goframe;
mod iface;
mod inputs;
mod jsx;
mod links;
mod registry;
mod stage;

pub use inputs::{INPUTS_KEY, Inputs, inputs_changed, recorded as recorded_inputs};
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
pub const VERSION: &str = "14";

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
