//! The stage: a read-only view of the base graph, the passes in merge
//! order, the merge, and the replacement (upstream `synthesizeCallbackEdges`
//! and `SynthesisStage`).

use std::cell::{OnceCell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::source_file::{SourceText, oversize_stamp, read_source_file};
use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind, UnresolvedRef};
use codegraph_extract::ExtractOptions;
use codegraph_store::queries::{MethodHeader, NodeSpan, ReferenceSite};
use codegraph_store::{ChangeSet, Store};
use rayon::prelude::*;

use super::context::ViewContext;
use super::footprint::{
    GateInput, PASS_RECORD_VERSION, PassRecord, PassRecordHead, Predicate, Recorder,
    STAGE_RECORD_SCHEME, Slice, StageRecord, TextGate, decode_output_row, keys,
};
use super::inputs::{INPUTS_KEY, Inputs, UnindexedReader};
use super::reuse::{Now, Probe, rerun_reason};
use super::{
    EDGES_KEY_SUFFIX, PASS_KEY_PREFIX, STAGE_KEY, angular, arkui, c_fnptr, callback, dispatch,
    erlang, framework, go, go_dispatch, go_func_field, goframe, iface, is_synthesized, jsx, links,
    nix, react_native, registry, sveltekit, tier, vue,
};
use crate::context::StoreResolutionContext;
use crate::frameworks::route_href::{RouteTableObserver, RouteTables};

/// What one run of the stage did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageStats {
    /// The merged edges the passes produced; for a sync that kept every step
    /// and wrote back what the cascade dropped, the synthesized edges the
    /// index holds after it.
    pub edges: usize,
    /// The stored synthesized edges the replacement removed: each one the
    /// stage did not make again unchanged (W7-18c0).
    pub removed: usize,
    /// The edges it inserted: the merged ones whose identity no base edge
    /// holds and that no stored synthesized edge already equals.
    pub inserted: usize,
    /// The prerequisite steps and passes that ran, one bit each in registry
    /// order ([`step_names`]): the prerequisite steps first, then the
    /// passes. A step whose language gate is closed has no bit.
    pub rerun: u64,
    /// The steps whose recorded output a sync kept instead of running them
    /// (W9-00 row 27).
    pub reused: usize,
}

/// The prerequisite steps' and the passes' names in registry order: bit `i`
/// of [`StageStats::rerun`] is the `i`-th.
pub fn step_names() -> impl Iterator<Item = &'static str> {
    PREREQUISITES.iter().chain(PASSES).map(|(name, _, _)| *name)
}

/// How long each part of one stage run took (W9-00a), for
/// `CODEGRAPH_PHASE_TIMINGS`. Kept beside [`StageStats`], which compares as a
/// result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StageTimings {
    /// Reading the indexed files' text and the project's languages.
    pub read: Duration,
    /// Each prerequisite step and pass that ran, by its upstream name, in run
    /// order; a pass its gate closes does not run and is not listed.
    pub passes: Vec<(&'static str, Duration)>,
    /// Merging and sorting the passes' edges.
    pub merge: Duration,
    /// Replacing the stored synthesized edges with the merged ones.
    pub replace: Duration,
    /// Encoding the records of what the stage read: the files beside the
    /// index and each step's record.
    pub inputs: Duration,
    /// Whether a sync's run kept steps' recorded outputs ([`run_stage_reusing`]),
    /// rather than running every step.
    pub reusing: bool,
    /// A reusing run's reading of the records and the sync's changes.
    pub decide: Duration,
    /// Each step a reusing run kept, by its upstream name, in run order, with
    /// the time its decision took.
    pub kept: Vec<(&'static str, Duration)>,
    /// A reusing run that kept every step: writing back the synthesized
    /// edges the sync's cascade dropped, in place of the merge and the
    /// replacement.
    pub restore: Option<Duration>,
}

/// A pass: the edges it synthesizes over the view, in its own order.
type Pass = fn(&View<'_>) -> Result<Vec<Edge>>;

/// Whether a pass can find anything in a project of these file languages
/// (upstream's `gate`): a pass it closes is provably empty, so it is
/// skipped without changing the output.
type Gate = fn(&HashSet<Language>) -> bool;

fn always(_: &HashSet<Language>) -> bool {
    true
}

/// Upstream's prerequisite steps (CS:5265-5295), in their order: each
/// step's edges join the view before the next step and the passes run, so
/// all of them read those edges as base edges, and they are written beside
/// the merged edges, never merged with them (upstream inserts them first).
const PREREQUISITES: &[(&str, Gate, Pass)] = &[
    ("goMethodContains", go::gate, go::method_contains_edges),
    ("goImplements", go::gate, go::implements_edges),
];

/// The ported passes in upstream `SYNTH_PASSES` order, which is the merge
/// order: the first pass to emit a `source>target` pair keeps it.
const PASSES: &[(&str, Gate, Pass)] = &[
    ("fieldEdges", always, callback::field_edges),
    (
        "closureCollEdges",
        always,
        callback::closure_collection_edges,
    ),
    // Before the in-process emitter pass: a pair both draw keeps the edge
    // that says which tier it crosses (upstream CS:5081-5084).
    ("tierEdges", jsx::gate, tier::cross_tier_edges),
    (
        "testRequestEdges",
        tier::test_request_gate,
        tier::test_request_edges,
    ),
    ("emitterEdges", always, callback::event_emitter_edges),
    ("renderEdges", always, dispatch::react_render_edges),
    ("jsxEdges", jsx::gate, jsx::jsx_render_edges),
    (
        "vueEdges",
        |languages| languages.contains(&Language::Vue),
        vue::template_edges,
    ),
    (
        "svelteKitEdges",
        |languages| languages.contains(&Language::Svelte),
        sveltekit::load_edges,
    ),
    ("pascalEdges", always, dispatch::pascal_form_edges),
    (
        "flutterEdges",
        |languages| languages.contains(&Language::Dart),
        dispatch::flutter_build_edges,
    ),
    ("arkuiStateEdges", arkui::gate, arkui::state_build_edges),
    ("arkuiEmitter", arkui::gate, arkui::emitter_edges),
    ("arkuiRoutes", arkui::gate, arkui::router_edges),
    (
        "cppEdges",
        |languages| languages.contains(&Language::Cpp),
        dispatch::cpp_override_edges,
    ),
    ("ifaceEdges", iface::gate, iface::interface_override_edges),
    (
        "kotlinExpectActual",
        |languages| languages.contains(&Language::Kotlin),
        dispatch::kotlin_expect_actual_edges,
    ),
    ("goGrpcEdges", go::gate, go_dispatch::grpc_stub_impl_edges),
    ("rnEventEdgesList", jsx::gate, react_native::rn_event_edges),
    (
        "fabricNativeEdges",
        always,
        react_native::fabric_native_impl_edges,
    ),
    (
        "expoXPlatEdges",
        |languages| languages.contains(&Language::Swift) && languages.contains(&Language::Kotlin),
        react_native::expo_cross_platform_edges,
    ),
    (
        "rnXPlatEdges",
        jsx::gate,
        react_native::rn_cross_platform_edges,
    ),
    (
        "mybatisEdges",
        |languages| {
            (languages.contains(&Language::Java) || languages.contains(&Language::Kotlin))
                && languages.contains(&Language::Xml)
        },
        dispatch::mybatis_edges,
    ),
    (
        "ginEdges",
        go::gate,
        go_dispatch::gin_middleware_chain_edges,
    ),
    ("thunkEdges", jsx::gate, framework::redux_thunk_edges),
    ("registryEdges", always, registry::object_registry_edges),
    ("rtkEdges", jsx::gate, framework::rtk_query_edges),
    (
        "piniaEdges",
        framework::store_gate,
        framework::pinia_store_edges,
    ),
    (
        "vuexEdges",
        framework::store_gate,
        framework::vuex_dispatch_edges,
    ),
    (
        "celeryEdges",
        |languages| languages.contains(&Language::Python),
        framework::celery_dispatch_edges,
    ),
    (
        "springEdges",
        |languages| languages.contains(&Language::Java),
        framework::spring_event_edges,
    ),
    (
        "mediatrEdges",
        |languages| languages.contains(&Language::CSharp),
        framework::mediatr_dispatch_edges,
    ),
    (
        "sidekiqEdges",
        |languages| languages.contains(&Language::Ruby),
        framework::sidekiq_dispatch_edges,
    ),
    (
        "erlangBehaviourEdges",
        erlang::gate,
        erlang::behaviour_dispatch_edges,
    ),
    (
        "laravelEdges",
        |languages| languages.contains(&Language::Php),
        framework::laravel_event_edges,
    ),
    (
        "cFnPtrEdges",
        |languages| languages.contains(&Language::C) || languages.contains(&Language::Cpp),
        c_fnptr::c_fn_pointer_dispatch_edges,
    ),
    ("goframeEdges", go::gate, goframe::route_edges),
    (
        "goFuncFieldEdges",
        go::gate,
        go_func_field::func_field_edges,
    ),
    (
        "expoRouterReturnEdges",
        jsx::gate,
        links::expo_router_return_edges,
    ),
    ("nextLinkEdges", jsx::gate, links::next_link_edges),
    (
        "reactRouterLinkEdges",
        jsx::gate,
        links::react_router_link_edges,
    ),
    ("tanstackLinkEdges", jsx::gate, links::tanstack_link_edges),
    (
        "vueRouterLinkEdges",
        framework::store_gate,
        links::vue_router_link_edges,
    ),
    (
        "angularTemplateEdges",
        angular::gate,
        angular::template_edges,
    ),
    (
        "svelteKitPageEdges",
        |languages| languages.contains(&Language::Svelte),
        sveltekit::page_component_edges,
    ),
    (
        "svelteKitLinkEdges",
        |languages| languages.contains(&Language::Svelte),
        links::sveltekit_link_edges,
    ),
    ("nixOptionEdges", nix::gate, nix::option_path_edges),
];

/// Compute every synthesized edge over `store`'s base graph and replace the
/// stored ones with them in one transaction, reading the files the index
/// does not hold within `options`' scan scope. The same transaction records
/// what the stage read beside the index (W7-17b) and what each step read
/// and emitted (W9-00 row 27). The marker is the caller's.
pub fn run_stage(
    store: &mut Store,
    project_root: &Path,
    options: &ExtractOptions,
) -> Result<StageStats> {
    Ok(run_stage_timed(store, project_root, options)?.0)
}

/// [`run_stage`], and how long each of its parts took (W9-00a).
pub fn run_stage_timed(
    store: &mut Store,
    project_root: &Path,
    options: &ExtractOptions,
) -> Result<(StageStats, StageTimings)> {
    let mut timings = StageTimings::default();
    let mut view = View::new(store, project_root, options.max_file_size);
    view.unindexed = Some(UnindexedReader::new(project_root, options));
    let output = synthesize_in(view, &mut timings)?;
    let started = Instant::now();
    let rows = output.rows(options)?;
    timings.inputs = started.elapsed();
    let started = Instant::now();
    let (removed, inserted) =
        store.replace_synthesized_edges_and_rows(&output.edges, &[PASS_KEY_PREFIX], &rows)?;
    timings.replace = started.elapsed();
    Ok((
        StageStats {
            edges: output.edges.len(),
            removed,
            inserted,
            rerun: output.rerun,
            reused: 0,
        },
        timings,
    ))
}

/// [`run_stage_timed`] for a sync that knows which base-graph rows it moved
/// (`changes`, the store's change log, W9-00 row 27): each step whose record
/// says nothing it read moved keeps its recorded output, the others run
/// again. The decision reads each step's record head; a step's raw output
/// is in a row of its own, read only when a step runs again and the outputs
/// must be merged, and used only once it is the row its head names by
/// length and hash. Then the stored synthesized edges are replaced with the
/// merged ones as a whole run does. When every step is kept, and the sync
/// removed no base edge, added no node under a new id and wrote no
/// synthesized edge, the stored edges are already the merge but for the
/// ones a re-extraction's cascade dropped, which the change log holds whole,
/// and those are written back instead: no output row is read. Only the rows
/// of the steps that ran and changed are rewritten. `None`, with nothing
/// written, when the records cannot vouch for the stage: none written,
/// written by another build, over another schema, engine version, options,
/// languages or steps, a head missing, unreadable or of another version, an
/// output row missing or of another length than its head names, or, when it
/// is read, another text; the caller runs the stage whole.
pub fn run_stage_reusing(
    store: &mut Store,
    project_root: &Path,
    options: &ExtractOptions,
    changes: &ChangeSet,
) -> Result<Option<(StageStats, StageTimings)>> {
    let mut timings = StageTimings::default();
    let output = {
        let mut view = View::new(store, project_root, options.max_file_size);
        view.unindexed = Some(UnindexedReader::new(project_root, options));
        let Some(output) = synthesize_reusing(
            &mut view,
            store,
            project_root,
            options,
            changes,
            &mut timings,
        )?
        else {
            return Ok(None);
        };
        output
    };
    let started = Instant::now();
    let mut rows = Vec::new();
    for (name, record) in &output.records {
        // A head names its output row by length and hash: an unchanged one
        // is an unchanged output, and an output row is written only when
        // the hash it is named by moves.
        let written = record.to_rows()?;
        let key = head_key(name);
        if store.get_project_metadata(&key)?.as_deref() == Some(written.head_row.as_str()) {
            continue;
        }
        let same_output = output.heads.get(name).is_some_and(|old| {
            old.edges_bytes == written.head.edges_bytes && old.edges_hash == written.head.edges_hash
        });
        rows.push((key, written.head_row));
        if !same_output {
            rows.push((edges_key(name), written.edges_row));
        }
    }
    let inputs = serde_json::to_string(&output.read)?;
    if store.get_project_metadata(INPUTS_KEY)?.as_deref() != Some(inputs.as_str()) {
        rows.push((INPUTS_KEY.to_string(), inputs));
    }
    timings.inputs = started.elapsed();
    let started = Instant::now();
    let (edges, removed, inserted) = match &output.edges {
        Some(edges) => {
            let (removed, inserted) =
                store.replace_synthesized_edges_and_rows(edges, &[], &rows)?;
            timings.replace = started.elapsed();
            (edges.len(), removed, inserted)
        }
        None => {
            let inserted = store.restore_synthesized_edges(&changes.synthesized.removed, &rows)?;
            let held = store.synthesized_edge_count()?;
            timings.restore = Some(started.elapsed());
            (held, 0, inserted)
        }
    };
    Ok(Some((
        StageStats {
            edges,
            removed,
            inserted,
            rerun: output.rerun,
            reused: output.reused,
        },
        timings,
    )))
}

/// What a reusing stage run made: its edges, `None` when every step kept its
/// output and the cascade's edges are written back instead; what its steps
/// read beside the index (the kept steps' records and the others' reads);
/// the records of the steps that ran; the stored heads the decision read;
/// and which ran and how many kept their output.
struct ReusingOutput {
    edges: Option<Vec<Edge>>,
    read: Inputs,
    records: Vec<(&'static str, PassRecord)>,
    heads: HashMap<&'static str, PassRecordHead>,
    rerun: u64,
    reused: usize,
}

/// The `project_metadata` key of `step`'s record head.
fn head_key(step: &str) -> String {
    format!("{PASS_KEY_PREFIX}{step}")
}

/// The `project_metadata` key of `step`'s raw-output row.
fn edges_key(step: &str) -> String {
    format!("{PASS_KEY_PREFIX}{step}{EDGES_KEY_SUFFIX}")
}

/// A step's stored record head: `None` when it is missing, does not read
/// as this layout, or is of another version, the single-row layout of
/// version 1 among them, so a record is never read in part.
fn record_head(store: &Store, step: &str) -> Result<Option<PassRecordHead>> {
    Ok(store
        .get_project_metadata(&head_key(step))?
        .and_then(|row| serde_json::from_str::<PassRecordHead>(&row).ok())
        .filter(|head| head.version == PASS_RECORD_VERSION))
}

/// Whether `step`'s output row is there with the length its head names,
/// read from the row's header (`octet_length`), not its text.
fn edges_row_present(store: &Store, step: &str, head: &PassRecordHead) -> Result<bool> {
    Ok(store.project_metadata_bytes(&edges_key(step))? == Some(head.edges_bytes))
}

/// A step's recorded raw output, read to be merged: `None` when its row is
/// missing, is not the one its head names (length and SHA-256), or does not
/// decode (base64, inflate, JSON).
fn recorded_edges(store: &Store, step: &str, head: &PassRecordHead) -> Result<Option<Vec<Edge>>> {
    Ok(store
        .get_project_metadata(&edges_key(step))?
        .filter(|row| head.holds(row))
        .and_then(|row| output_edges(&row)))
}

/// The edges an `:edges` row holds: `None` when it does not decode.
fn output_edges(row: &str) -> Option<Vec<Edge>> {
    let json = decode_output_row(row)?;
    serde_json::from_str::<Vec<Edge>>(&json).ok()
}

/// The stage over `view` keeping each step its record vouches for (see
/// [`run_stage_reusing`]); `None` when the records cannot vouch for it.
fn synthesize_reusing(
    view: &mut View<'_>,
    store: &Store,
    project_root: &Path,
    options: &ExtractOptions,
    changes: &ChangeSet,
    timings: &mut StageTimings,
) -> Result<Option<ReusingOutput>> {
    let started = Instant::now();
    let languages = view.file_languages()?;
    let mut language_names: Vec<String> = languages
        .iter()
        .map(|language| language.as_str().to_string())
        .collect();
    language_names.sort();
    let expected = StageRecord {
        scheme: STAGE_RECORD_SCHEME,
        engine: super::VERSION.to_string(),
        build: super::BUILD_IDENTITY.to_string(),
        schema: codegraph_store::migrations::CURRENT_SCHEMA_VERSION,
        options: options_digest(options),
        languages: language_names,
        passes: PREREQUISITES
            .iter()
            .chain(PASSES)
            .filter(|(_, gate, _)| gate(&languages))
            .map(|(name, _, _)| (*name).to_string())
            .collect(),
    };
    let recorded = store
        .get_project_metadata(STAGE_KEY)?
        .and_then(|row| serde_json::from_str::<StageRecord>(&row).ok());
    if recorded.as_ref() != Some(&expected) {
        return Ok(None);
    }
    // Every step's head, and its output row there with the length the head
    // names: anything else leaves the stage unvouched for.
    let mut heads: HashMap<&'static str, PassRecordHead> = HashMap::new();
    for (name, gate, _) in PREREQUISITES.iter().chain(PASSES) {
        if !gate(&languages) {
            continue;
        }
        let Some(head) = record_head(store, name)? else {
            return Ok(None);
        };
        if !edges_row_present(store, name, &head)? {
            return Ok(None);
        }
        heads.insert(*name, head);
    }
    let mut probe = Probe::new(changes);
    let now = Now::new(store, project_root, options);
    timings.reusing = true;
    timings.decide = started.elapsed();

    let mut read = Inputs::new();
    let mut records = Vec::new();
    let mut rerun = 0_u64;
    let mut reused = 0_usize;
    // Each prerequisite step that ran, its edges as made; a kept one's are
    // decoded into the overlay only once a later step reads them.
    let mut prerequisites: Vec<(&'static str, Option<Vec<Edge>>)> = Vec::new();
    let mut overlaid = 0;
    for (at, (name, gate, step)) in PREREQUISITES.iter().enumerate() {
        if !gate(&languages) {
            continue;
        }
        let started = Instant::now();
        let head = &heads[name];
        if rerun_reason(head, &probe, &now, view)?.is_none() {
            read.extend(head.unindexed.clone());
            prerequisites.push((*name, None));
            reused += 1;
            timings.kept.push((*name, started.elapsed()));
            continue;
        }
        // A prerequisite step's last output is what the later steps read:
        // a record past its size keeps none to tell what moved, and without
        // it nothing after the step can be vouched for.
        if head.untracked.iter().any(|what| what == "record-size") {
            return Ok(None);
        }
        let Some(before) = recorded_edges(store, name, head)? else {
            return Ok(None);
        };
        if !overlay_kept(view, store, &heads, &mut prerequisites, overlaid)? {
            return Ok(None);
        }
        view.recorder.begin_pass();
        let made = step(view)?;
        let footprint = view.recorder.end_pass();
        probe.moved_overlay(&before, &made);
        read.extend(footprint.unindexed.clone());
        records.push((*name, PassRecord::new(&made, &footprint)));
        view.overlay.extend(made.iter().cloned());
        prerequisites.push((*name, Some(made)));
        overlaid = prerequisites.len();
        rerun |= 1 << at;
        timings.passes.push((*name, started.elapsed()));
    }
    // Every pass decided against the final probe first, on its record's
    // decision fields.
    let mut decided = Vec::with_capacity(PASSES.len());
    for (at, (name, gate, pass)) in PASSES.iter().enumerate() {
        if !gate(&languages) {
            continue;
        }
        let started = Instant::now();
        let head = &heads[name];
        let keep = rerun_reason(head, &probe, &now, view)?.is_none();
        if keep {
            read.extend(head.unindexed.clone());
        }
        decided.push((
            at,
            *name,
            *pass,
            keep,
            !head.gates.is_empty(),
            started.elapsed(),
        ));
    }
    let all_kept = rerun == 0 && decided.iter().all(|(_, _, _, keep, _, _)| *keep);
    if all_kept && can_restore(changes) {
        for (_, name, _, _, _, decide) in &decided {
            reused += 1;
            timings.kept.push((*name, *decide));
        }
        return Ok(Some(ReusingOutput {
            edges: None,
            read,
            records,
            heads,
            rerun,
            reused,
        }));
    }
    // The outputs are merged: the kept steps' are read, a pass that runs
    // again and scans files has the texts read at once, as a whole run does.
    if !overlay_kept(view, store, &heads, &mut prerequisites, overlaid)? {
        return Ok(None);
    }
    if decided
        .iter()
        .any(|(_, _, _, keep, gated, _)| !keep && *gated)
    {
        let started = Instant::now();
        view.read_texts()?;
        timings.read = started.elapsed();
    }
    let mut passes = Vec::with_capacity(decided.len());
    for (at, name, pass, keep, _, decide) in decided {
        if keep {
            let started = Instant::now();
            let Some(edges) = recorded_edges(store, name, &heads[name])? else {
                return Ok(None);
            };
            passes.push(edges);
            reused += 1;
            timings.kept.push((name, decide + started.elapsed()));
            continue;
        }
        let started = Instant::now();
        view.recorder.begin_pass();
        let made = pass(view)?;
        let footprint = view.recorder.end_pass();
        read.extend(footprint.unindexed.clone());
        records.push((name, PassRecord::new(&made, &footprint)));
        passes.push(made);
        rerun |= 1 << (PREREQUISITES.len() + at);
        timings.passes.push((name, decide + started.elapsed()));
    }
    let edges: Vec<Edge> = prerequisites
        .into_iter()
        .flat_map(|(_, made)| made.unwrap_or_default())
        .collect();
    let started = Instant::now();
    let edges = combine(edges, passes);
    timings.merge = started.elapsed();
    Ok(Some(ReusingOutput {
        edges: Some(edges),
        read,
        records,
        heads,
        rerun,
        reused,
    }))
}

/// The kept prerequisite steps from `from` on, their recorded edges read
/// into `prerequisites` and the overlay in step order, before a later step
/// reads them; `false` when an output row is not the one its head names.
fn overlay_kept(
    view: &mut View<'_>,
    store: &Store,
    heads: &HashMap<&'static str, PassRecordHead>,
    prerequisites: &mut [(&'static str, Option<Vec<Edge>>)],
    from: usize,
) -> Result<bool> {
    for &mut (step, ref mut made) in prerequisites.iter_mut().skip(from) {
        if made.is_none() {
            let Some(edges) = recorded_edges(store, step, &heads[step])? else {
                return Ok(false);
            };
            view.overlay.extend(edges.iter().cloned());
            *made = Some(edges);
        }
    }
    Ok(true)
}

/// Whether, for a run that kept every step, writing back the synthesized
/// edges the sync's cascade dropped leaves what replacing the stored ones
/// with the kept outputs merged would. The stored ones were that merge but
/// for the edges the insert skipped, an identity a base edge held or a node
/// that was not there, so the sync must have removed no base edge (it frees
/// an identity such an edge takes), added no node under a new id (such an
/// edge now has its node) and written no synthesized edge.
fn can_restore(changes: &ChangeSet) -> bool {
    if !changes.edges.removed.is_empty() || !changes.synthesized.added.is_empty() {
        return false;
    }
    let removed: HashSet<&str> = changes
        .nodes
        .removed
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    changes
        .nodes
        .added
        .iter()
        .all(|node| removed.contains(node.id.as_str()))
}

/// Every per-file text check a pass scans with ([`View::gated_texts`]), by
/// name: what a sync asks again of a changed file (W9-00 row 27).
static GATES: [&TextGate; 12] = [
    &callback::EMITTER_GATE,
    &registry::REGISTRY_GATE,
    &tier::TIER_GATE,
    &jsx::JSX_GATE,
    &framework::PINIA_FACTORY_GATE,
    &framework::PINIA_USE_GATE,
    &framework::VUEX_GATE,
    &framework::CELERY_GATE,
    &framework::SPRING_GATE,
    &framework::SIDEKIQ_GATE,
    &erlang::CALLBACK_GATE,
    &erlang::DISPATCH_GATE,
];

/// The gate a record names.
pub(super) fn gate_named(name: &str) -> Option<&'static TextGate> {
    GATES.iter().copied().find(|gate| gate.name == name)
}

/// Every synthesized edge over `store`'s base graph: each prerequisite
/// step's, and each pass's merged in registry order, sorted. It reads the
/// indexed files only, never a file the index does not hold.
pub fn synthesize(store: &Store, project_root: &Path, max_file_size: u64) -> Result<Vec<Edge>> {
    Ok(synthesize_in(
        View::new(store, project_root, max_file_size),
        &mut StageTimings::default(),
    )?
    .edges)
}

/// The rows a stage run over `store` would write beside its edges
/// ([`StageOutput::rows`]), computed without writing anything: the control
/// the record tests compare with.
#[doc(hidden)]
pub fn stage_rows(
    store: &Store,
    project_root: &Path,
    options: &ExtractOptions,
) -> Result<Vec<(String, String)>> {
    let mut view = View::new(store, project_root, options.max_file_size);
    view.unindexed = Some(UnindexedReader::new(project_root, options));
    synthesize_in(view, &mut StageTimings::default())?.rows(options)
}

/// Each step whose gate opens over `store`, run alone in a view of its own
/// after the prerequisite steps before it (whose overlay it reads, and whose
/// reads are not its own), with the row its record would be: what a stage
/// run records for the step whatever other steps share its view's caches
/// (W9-00 row 27's control of the footprints).
#[doc(hidden)]
pub fn records_alone(
    store: &Store,
    project_root: &Path,
    options: &ExtractOptions,
) -> Result<Vec<(String, String)>> {
    let languages: HashSet<Language> = store.file_languages()?.into_iter().collect();
    let steps: Vec<&(&str, Gate, Pass)> = PREREQUISITES.iter().chain(PASSES).collect();
    let mut rows = Vec::new();
    for (index, (name, gate, run)) in steps.iter().enumerate() {
        if !gate(&languages) {
            continue;
        }
        let mut view = View::new(store, project_root, options.max_file_size);
        view.unindexed = Some(UnindexedReader::new(project_root, options));
        for (at, (_, gate, step)) in PREREQUISITES.iter().enumerate() {
            if at < index && gate(&languages) {
                let made = step(&view)?;
                view.overlay.extend(made);
            }
        }
        view.recorder.begin_pass();
        let made = run(&view)?;
        let written = PassRecord::new(&made, &view.recorder.end_pass()).to_rows()?;
        rows.push((head_key(name), written.head_row));
        rows.push((edges_key(name), written.edges_row));
    }
    Ok(rows)
}

/// What one stage run made: its edges, what it read beside the index, the
/// indexed files' languages, and each step's record, in run order.
pub(super) struct StageOutput {
    pub(super) edges: Vec<Edge>,
    pub(super) read: Inputs,
    pub(super) languages: Vec<String>,
    pub(super) records: Vec<(&'static str, PassRecord)>,
    /// The steps that ran, one bit each in registry order
    /// ([`StageStats::rerun`]).
    pub(super) rerun: u64,
}

impl StageOutput {
    /// The `project_metadata` rows the run writes: [`STAGE_KEY`], the two
    /// rows of each step that ran (its head under [`PASS_KEY_PREFIX`] and its
    /// output's row), and [`INPUTS_KEY`].
    pub(super) fn rows(&self, options: &ExtractOptions) -> Result<Vec<(String, String)>> {
        let stage = StageRecord {
            scheme: STAGE_RECORD_SCHEME,
            engine: super::VERSION.to_string(),
            build: super::BUILD_IDENTITY.to_string(),
            schema: codegraph_store::migrations::CURRENT_SCHEMA_VERSION,
            options: options_digest(options),
            languages: self.languages.clone(),
            passes: self
                .records
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect(),
        };
        let mut rows = vec![(STAGE_KEY.to_string(), serde_json::to_string(&stage)?)];
        for (name, record) in &self.records {
            let written = record.to_rows()?;
            rows.push((head_key(name), written.head_row));
            rows.push((edges_key(name), written.edges_row));
        }
        rows.push((INPUTS_KEY.to_string(), serde_json::to_string(&self.read)?));
        Ok(rows)
    }
}

/// The digest of the options the stage reads: the size bound of the files
/// it reads and the scan scope of the files the index does not hold, never
/// the project's place on disk.
pub(super) fn options_digest(options: &ExtractOptions) -> String {
    hash_content(&format!(
        "max_file_size={}\ninclude={:?}\nexclude={:?}\nignore_dirs={:?}\nignore_paths={:?}\n",
        options.max_file_size,
        options.include,
        options.exclude,
        options.ignore_dirs,
        options.ignore_paths,
    ))
}

/// The stage over `view`, with what it read beside the index and each
/// step's record, timing its parts into `timings`.
fn synthesize_in(mut view: View<'_>, timings: &mut StageTimings) -> Result<StageOutput> {
    let started = Instant::now();
    view.read_texts()?;
    let languages = view.file_languages()?;
    timings.read = started.elapsed();
    let mut edges = Vec::new();
    let mut records = Vec::new();
    let mut rerun = 0_u64;
    for (at, (name, gate, step)) in PREREQUISITES.iter().enumerate() {
        if gate(&languages) {
            let started = Instant::now();
            view.recorder.begin_pass();
            let made = step(&view)?;
            records.push((*name, PassRecord::new(&made, &view.recorder.end_pass())));
            view.overlay.extend(made.iter().cloned());
            edges.extend(made);
            rerun |= 1 << at;
            timings.passes.push((*name, started.elapsed()));
        }
    }
    let mut passes = Vec::with_capacity(PASSES.len());
    for (at, (name, gate, pass)) in PASSES.iter().enumerate() {
        if gate(&languages) {
            let started = Instant::now();
            view.recorder.begin_pass();
            let made = pass(&view)?;
            records.push((*name, PassRecord::new(&made, &view.recorder.end_pass())));
            passes.push(made);
            rerun |= 1 << (PREREQUISITES.len() + at);
            timings.passes.push((*name, started.elapsed()));
        }
    }
    let read = view
        .unindexed
        .take()
        .map(UnindexedReader::into_inputs)
        .unwrap_or_default();
    let started = Instant::now();
    let edges = combine(edges, passes);
    timings.merge = started.elapsed();
    let mut languages: Vec<String> = languages
        .iter()
        .map(|language| language.as_str().to_string())
        .collect();
    languages.sort();
    Ok(StageOutput {
        edges,
        read,
        languages,
        records,
        rerun,
    })
}

/// The stage's output: the prerequisite steps' edges as they are (upstream
/// inserts them before the passes run) beside the passes' merged ones,
/// sorted.
fn combine(prerequisites: Vec<Edge>, passes: Vec<Vec<Edge>>) -> Vec<Edge> {
    let mut edges = prerequisites;
    edges.extend(merge(passes));
    edges.sort_by(edge_order);
    edges
}

/// Upstream's merge: each edge takes its `source>target` key, and a later
/// edge with a taken key is dropped, so the earlier pass keeps the pair. An
/// edge whose identity a base edge holds stays here and is ignored by the
/// insert (`idx_edges_identity`), still holding its key, as upstream's stage
/// trigger skips it after the merge.
fn merge(passes: Vec<Vec<Edge>>) -> Vec<Edge> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut merged: Vec<Edge> = passes
        .into_iter()
        .flatten()
        .filter(|edge| seen.insert((edge.source.clone(), edge.target.clone())))
        .collect();
    merged.sort_by(edge_order);
    merged
}

/// The order the stage writes edges in.
fn edge_order(a: &Edge, b: &Edge) -> Ordering {
    (&a.source, &a.target, a.kind.as_str(), a.line, a.col).cmp(&(
        &b.source,
        &b.target,
        b.kind.as_str(),
        b.line,
        b.col,
    ))
}

/// The order upstream's node lookups return (`ORDER BY file_path,
/// start_line, id`), which a pass's caps and first matches follow.
pub(super) fn node_order(a: &Node, b: &Node) -> Ordering {
    (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
}

/// Upstream's `methodAndFunctionNodes`: every method, then every function,
/// each in [`node_order`].
fn callable_order(a: &Node, b: &Node) -> Ordering {
    let rank = |node: &Node| node.kind != NodeKind::Method;
    rank(a).cmp(&rank(b)).then_with(|| node_order(a, b))
}

/// The order of upstream's edge lookups past the kind: by the other end,
/// then line and column (`ORDER BY kind, source, line, col` into a node,
/// `kind, target, line, col` out of one; `None` first, as SQLite's NULL).
fn edge_end_order(end: impl Fn(&Edge) -> &str) -> impl Fn(&Edge, &Edge) -> Ordering {
    move |a, b| (end(a), a.line, a.col).cmp(&(end(b), b.line, b.col))
}

/// What the view reads of a file (see [`View::text`]).
#[derive(Clone)]
pub(super) enum Read {
    /// Its text, as the index holds it.
    Text(Arc<str>),
    /// No text, the same every time while the files table holds what it
    /// holds: no row, or the row of a file over the size bound (or not
    /// source) that still reads as its row says.
    Absent,
    /// A file whose row the disk no longer matches, or that could not be
    /// read (H2): read as absent, never sliced by its old nodes' lines.
    Drifted,
}

/// The files table, read once, by path.
struct FileTable {
    records: Vec<FileRecord>,
    by_path: HashMap<String, usize>,
}

/// The node kinds of [`Store::classes_and_members_by_file_paths`].
const CLASS_AND_MEMBER_KINDS: [&str; 3] = ["class", "method", "property"];
/// The node kinds of [`Store::layouts_and_callables_by_file_path`].
const LAYOUT_AND_CALLABLE_KINDS: [&str; 4] = ["struct", "union", "function", "method"];
/// The node kinds of [`Store::enclosing_candidates_by_file_path`].
const ENCLOSING_KINDS: [&str; 3] = ["function", "method", "component"];

/// What a pass reads: the base graph and the indexed files' text, each read
/// recorded into the pass's footprint (W9-00 row 27, `footprint.rs`).
pub(super) struct View<'a> {
    store: &'a Store,
    project_root: PathBuf,
    max_file_size: u64,
    /// The edges of the stage's [`PREREQUISITES`], which the later steps
    /// and the passes read beside the base graph's.
    overlay: Overlay,
    texts: RefCell<HashMap<String, Read>>,
    /// Every indexed file's row, read the first time a pass walks them.
    files: OnceCell<FileTable>,
    /// The reader of the files the index does not hold, when the stage may
    /// read them ([`run_stage`]; never under [`synthesize`]).
    unindexed: Option<UnindexedReader>,
    /// The routers' route tables of this stage run, each built the first
    /// time a pass asks for it through a [`ViewContext`] (W7-18a).
    route_tables: Arc<RouteTables>,
    /// What each step reads.
    recorder: Arc<Recorder>,
}

impl<'a> View<'a> {
    pub(super) fn new(store: &'a Store, project_root: &Path, max_file_size: u64) -> Self {
        let recorder = Arc::new(Recorder::default());
        Self {
            store,
            project_root: project_root.to_path_buf(),
            max_file_size,
            overlay: Overlay::default(),
            texts: RefCell::new(HashMap::new()),
            files: OnceCell::new(),
            unindexed: None,
            route_tables: Arc::new(RouteTables::observed(
                Arc::clone(&recorder) as Arc<dyn RouteTableObserver>
            )),
            recorder,
        }
    }

    /// The recorder of what the step running reads.
    pub(super) fn recorder(&self) -> &Recorder {
        &self.recorder
    }

    /// The view as the [`crate::types::ResolutionContext`] a router's table,
    /// its href readers and import resolution read through (see
    /// [`ViewContext`]).
    pub(super) fn context(&self) -> ViewContext<'_, 'a> {
        let store = StoreResolutionContext::new(
            self.store,
            self.project_root.to_string_lossy().into_owned(),
        )
        .with_max_file_size(self.max_file_size);
        ViewContext::new(self, store)
    }

    /// The route tables of this stage run ([`ViewContext`]'s).
    pub(super) fn route_tables(&self) -> Arc<RouteTables> {
        Arc::clone(&self.route_tables)
    }

    /// The text of `path`, a file the index does not hold, as
    /// [`UnindexedReader::text`] reads and records it; `None` when the
    /// stage reads no such file.
    pub(super) fn unindexed_text(&self, path: &str) -> Option<Arc<str>> {
        let reader = self.unindexed.as_ref()?;
        let text = reader.text(path);
        if let Some(relative) = UnindexedReader::relative(path) {
            let hash = text.as_deref().map(hash_content);
            self.recorder.unindexed(&relative, hash.as_deref());
        }
        text
    }

    /// The project-relative path an unindexed `path` is read under, `None`
    /// for one leaving the root.
    pub(super) fn unindexed_path(path: &str) -> Option<String> {
        UnindexedReader::relative(path)
    }

    /// The Go modules of the `go.mod` files above `files`, read from disk
    /// within the root: Go's control inputs (`control_inputs.rs`), which a
    /// sync that sees one change recomputes the stage whole for.
    pub(super) fn go_modules(&self, files: &[String]) -> Vec<crate::types::GoModule> {
        self.recorder.key(keys::files());
        crate::context::load_go_modules_pub(&self.project_root.to_string_lossy(), files)
    }

    /// Whether the files table flags `file` as generated (upstream
    /// `generatedPredicateFor`'s stored half; its path half is
    /// [`codegraph_core::file_class::is_generated_file`]).
    pub(super) fn is_flagged_generated(&self, file: &str) -> bool {
        self.recorder.key(keys::file_row(file));
        self.file_record(file)
            .is_some_and(|stored| stored.generated)
    }

    /// The base edges of `kind` into `target`, and the overlay's, by source,
    /// line and column.
    pub(super) fn edges_into(&self, target: &str, kind: EdgeKind) -> Result<Vec<Edge>> {
        self.recorder.key(keys::edges_into(target, kind.as_str()));
        let mut edges: Vec<Edge> = self
            .store
            .edges_by_target_kind(target, Some(kind))?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .collect();
        edges.extend(self.overlay.reaching(target, kind).cloned());
        edges.sort_by(edge_end_order(|edge| &edge.source));
        Ok(edges)
    }

    /// The base edges of `kind` out of `source`, and the overlay's, by
    /// target, line and column.
    pub(super) fn edges_from(&self, source: &str, kind: EdgeKind) -> Result<Vec<Edge>> {
        self.recorder.key(keys::edges_from(source, kind.as_str()));
        let mut edges: Vec<Edge> = self
            .store
            .edges_by_source_kind(source, Some(kind))?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .collect();
        edges.extend(self.overlay.out_of(source, kind).cloned());
        edges.sort_by(edge_end_order(|edge| &edge.target));
        Ok(edges)
    }

    /// [`Self::edges_into`] of each of `targets`, by target: the same edges
    /// in the same order, read with one query a chunk of targets rather
    /// than one a target (W7-15b's cost revision). A target without edges
    /// has no entry.
    pub(super) fn edges_into_each(
        &self,
        targets: &[String],
        kind: EdgeKind,
    ) -> Result<HashMap<String, Vec<Edge>>> {
        let keys: Vec<u64> = targets
            .iter()
            .map(|target| keys::edges_into(target, kind.as_str()))
            .collect();
        self.recorder.keys(&keys);
        let mut by_end: HashMap<String, Vec<Edge>> = HashMap::new();
        for edge in self.store.edges_by_targets_kind(targets, kind)? {
            if !is_synthesized(&edge) {
                by_end.entry(edge.target.clone()).or_default().push(edge);
            }
        }
        for target in targets.iter().collect::<HashSet<_>>() {
            let overlay: Vec<Edge> = self.overlay.reaching(target, kind).cloned().collect();
            if !overlay.is_empty() {
                by_end.entry(target.clone()).or_default().extend(overlay);
            }
        }
        for edges in by_end.values_mut() {
            edges.sort_by(edge_end_order(|edge| &edge.source));
        }
        Ok(by_end)
    }

    /// The ones of `targets` a `calls` edge reaches from a node written in
    /// one of `languages`: the base edges and the overlay's, without the
    /// stored synthesized ones, as [`Self::edges_into_each`] reads them
    /// (W7-21's cost revision). The base edges come with one query that
    /// reads their sources' languages, neither every edge into a target nor
    /// a caller's whole row. A caller whose language moves under its id is
    /// no edge change, so that is read too.
    pub(super) fn called_from(
        &self,
        targets: &[String],
        languages: &[Language],
    ) -> Result<HashSet<String>> {
        let mut keys: Vec<u64> = targets
            .iter()
            .map(|target| keys::edges_into(target, EdgeKind::Calls.as_str()))
            .collect();
        keys.push(keys::language_move());
        self.recorder.keys(&keys);
        let mut called: HashSet<String> = self
            .store
            .calls_into_from_languages(targets, languages)?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .map(|edge| edge.target)
            .collect();
        let wanted: HashSet<&str> = targets.iter().map(String::as_str).collect();
        let overlaid: Vec<&Edge> = self
            .overlay
            .of_kind(EdgeKind::Calls)
            .filter(|edge| wanted.contains(edge.target.as_str()) && !called.contains(&edge.target))
            .collect();
        if !overlaid.is_empty() {
            let callers: Vec<String> = overlaid.iter().map(|edge| edge.source.clone()).collect();
            let keys: Vec<u64> = callers.iter().map(|caller| keys::node(caller)).collect();
            self.recorder.keys(&keys);
            let callers = self.store.nodes_by_ids(&callers)?;
            for edge in overlaid {
                if callers
                    .get(&edge.source)
                    .is_some_and(|caller| languages.contains(&caller.language))
                {
                    called.insert(edge.target.clone());
                }
            }
        }
        Ok(called)
    }

    /// [`Self::edges_from`] of each of `sources`, by source, read as
    /// [`Self::edges_into_each`] reads into its targets.
    pub(super) fn edges_from_each(
        &self,
        sources: &[String],
        kind: EdgeKind,
    ) -> Result<HashMap<String, Vec<Edge>>> {
        let keys: Vec<u64> = sources
            .iter()
            .map(|source| keys::edges_from(source, kind.as_str()))
            .collect();
        self.recorder.keys(&keys);
        let mut by_end: HashMap<String, Vec<Edge>> = HashMap::new();
        for edge in self.store.edges_by_sources_kind(sources, kind)? {
            if !is_synthesized(&edge) {
                by_end.entry(edge.source.clone()).or_default().push(edge);
            }
        }
        for source in sources.iter().collect::<HashSet<_>>() {
            let overlay: Vec<Edge> = self.overlay.out_of(source, kind).cloned().collect();
            if !overlay.is_empty() {
                by_end.entry(source.clone()).or_default().extend(overlay);
            }
        }
        for edges in by_end.values_mut() {
            edges.sort_by(edge_end_order(|edge| &edge.target));
        }
        Ok(by_end)
    }

    /// Every base edge of `kind`, and the overlay's.
    pub(super) fn edges_of_kind(&self, kind: EdgeKind) -> Result<Vec<Edge>> {
        self.recorder.key(keys::edges_of_kind(kind.as_str()));
        let mut edges: Vec<Edge> = self
            .store
            .edges_by_kind(kind)?
            .into_iter()
            .filter(|edge| !is_synthesized(edge))
            .collect();
        edges.extend(self.overlay.of_kind(kind).cloned());
        Ok(edges)
    }

    pub(super) fn node(&self, id: &str) -> Result<Option<Node>> {
        self.recorder.key(keys::node(id));
        Ok(self.store.node_by_id(id)?)
    }

    /// The nodes of `ids` that exist, by id, read with one query a chunk.
    pub(super) fn nodes(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        let keys: Vec<u64> = ids.iter().map(|id| keys::node(id)).collect();
        self.recorder.keys(&keys);
        Ok(self.store.nodes_by_ids(ids)?)
    }

    /// The nodes whose qualified name is `qualified`, in [`node_order`].
    pub(super) fn nodes_qualified(&self, qualified: &str) -> Result<Vec<Node>> {
        self.recorder.key(keys::qualified(qualified));
        let mut nodes = self.store.nodes_by_qualified_name(qualified)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The `language` nodes whose decorators may hold `decorator` (see
    /// [`Store::nodes_with_decorator`]), in [`node_order`].
    pub(super) fn nodes_with_decorator(
        &self,
        language: Language,
        decorator: &str,
    ) -> Result<Vec<Node>> {
        self.recorder.predicate(Predicate::Decorator {
            language: language.as_str().to_string(),
            pattern: format!("%\"{decorator}\"%"),
        });
        let mut nodes = self.store.nodes_with_decorator(language, decorator)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The nodes of `file`, by line and id (upstream `getNodesByFile`).
    pub(super) fn nodes_in_file(&self, file: &str) -> Result<Vec<Node>> {
        self.recorder.key(keys::file_nodes(file));
        let mut nodes = self.store.nodes_by_file_path(file)?;
        nodes.sort_by(|a, b| (a.start_line, &a.id).cmp(&(b.start_line, &b.id)));
        Ok(nodes)
    }

    fn record_file_kinds<'f>(&self, files: impl IntoIterator<Item = &'f str>, kinds: &[&str]) {
        let mut keys = Vec::new();
        for file in files {
            keys.extend(kinds.iter().map(|kind| keys::file_kind(file, kind)));
        }
        self.recorder.keys(&keys);
    }

    /// The classes, methods and properties of each of `files`, by file, in
    /// [`Self::nodes_in_file`]'s order, read with one query for them all
    /// (W7-18d's cost revision); a file without any has no entry.
    pub(super) fn classes_and_members_in_files(
        &self,
        files: &[String],
    ) -> Result<HashMap<String, Vec<Node>>> {
        self.record_file_kinds(files.iter().map(String::as_str), &CLASS_AND_MEMBER_KINDS);
        let mut by_file: HashMap<String, Vec<Node>> = HashMap::new();
        for node in self.store.classes_and_members_by_file_paths(files)? {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }
        for nodes in by_file.values_mut() {
            nodes.sort_by(|a, b| (a.start_line, &a.id).cmp(&(b.start_line, &b.id)));
        }
        Ok(by_file)
    }

    /// The structs, unions, functions and methods of [`Self::nodes_in_file`],
    /// in the same order, read without the others (W7-17a).
    pub(super) fn layouts_and_callables_in_file(&self, file: &str) -> Result<Vec<Node>> {
        self.record_file_kinds([file], &LAYOUT_AND_CALLABLE_KINDS);
        let mut nodes = self.store.layouts_and_callables_by_file_path(file)?;
        nodes.sort_by(|a, b| (a.start_line, &a.id).cmp(&(b.start_line, &b.id)));
        Ok(nodes)
    }

    /// The nodes of [`Self::nodes_in_file`] that [`enclosing_fn`] can
    /// return, in the same order, read without the others (W7-15d's cost
    /// revision): `enclosing_fn` over them gives what it gives over every
    /// node of the file.
    pub(super) fn callables_in_file(&self, file: &str) -> Result<Vec<Node>> {
        self.record_file_kinds([file], &ENCLOSING_KINDS);
        let mut nodes = self.store.enclosing_candidates_by_file_path(file)?;
        nodes.sort_by(|a, b| (a.start_line, &a.id).cmp(&(b.start_line, &b.id)));
        Ok(nodes)
    }

    fn record_kinds_in(&self, kinds: &[&str], languages: &[Language]) {
        let mut keys = Vec::new();
        for kind in kinds {
            keys.extend(
                languages
                    .iter()
                    .map(|language| keys::kind_in(kind, language.as_str())),
            );
        }
        self.recorder.keys(&keys);
    }

    /// The nodes of `kinds` written in `languages`, in [`node_order`]
    /// (upstream `iterateNodesByKindIn`).
    pub(super) fn nodes_of_kinds_in(
        &self,
        kinds: &[NodeKind],
        languages: &[Language],
    ) -> Result<Vec<Node>> {
        let names: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        self.record_kinds_in(&names, languages);
        let mut nodes = self.store.nodes_of_kinds_in_languages(kinds, languages)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// [`Self::nodes_of_kinds_in`]'s nodes as [`NodeSpan`]s (id, name,
    /// file, lines), in the same [`node_order`] (W7-24 cost, OD-B73).
    pub(super) fn node_spans_of_kinds_in(
        &self,
        kinds: &[NodeKind],
        languages: &[Language],
    ) -> Result<Vec<NodeSpan>> {
        let names: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        self.record_kinds_in(&names, languages);
        let mut spans = self
            .store
            .node_spans_of_kinds_in_languages(kinds, languages)?;
        spans.sort_by(|a, b| {
            (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
        });
        Ok(spans)
    }

    /// The [`MethodHeader`]s of the methods written in `languages`, in
    /// [`node_order`]: [`Self::nodes_of_kinds_in`]'s methods, five fields of
    /// each (W7-21's cost revision).
    pub(super) fn method_headers_in(&self, languages: &[Language]) -> Result<Vec<MethodHeader>> {
        self.record_kinds_in(&[NodeKind::Method.as_str()], languages);
        let mut headers = self.store.method_headers_in_languages(languages)?;
        headers.sort_by(|a, b| {
            (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
        });
        Ok(headers)
    }

    /// The nodes of `kind` whose signature holds `text` (see
    /// [`Store::nodes_of_kind_whose_signature_holds`]), in [`node_order`].
    pub(super) fn nodes_of_kind_whose_signature_holds(
        &self,
        kind: NodeKind,
        text: &str,
    ) -> Result<Vec<Node>> {
        self.recorder.predicate(Predicate::Signature {
            node_kind: kind.as_str().to_string(),
            text: text.to_string(),
        });
        let mut nodes = self.store.nodes_of_kind_whose_signature_holds(kind, text)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The nodes of `kind`, in [`node_order`] (upstream `getNodesByKind`).
    pub(super) fn nodes_of_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        self.recorder.key(keys::kind(kind.as_str()));
        let mut nodes = self.store.nodes_by_kind(kind)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The nodes named `name`, in [`node_order`].
    pub(super) fn nodes_named(&self, name: &str) -> Result<Vec<Node>> {
        self.recorder.key(keys::name(name));
        let mut nodes = self.store.nodes_by_name(name)?;
        nodes.sort_by(node_order);
        Ok(nodes)
    }

    /// The functions and methods a name pattern can match (see
    /// [`Store::callables_named_like`]): the methods, then the functions.
    pub(super) fn callables_named_like(
        &self,
        globs: &[&str],
        likes: &[&str],
        names: &[&str],
    ) -> Result<Vec<Node>> {
        let owned = |values: &[&str]| values.iter().map(|value| (*value).to_string()).collect();
        self.recorder.predicate(Predicate::Callables {
            globs: owned(globs),
            likes: owned(likes),
            names: owned(names),
        });
        let mut nodes = self.store.callables_named_like(globs, likes, names)?;
        nodes.sort_by(callable_order);
        Ok(nodes)
    }

    /// The functions and methods written in `languages`: the methods, then
    /// the functions.
    pub(super) fn callables_in_languages(&self, languages: &[Language]) -> Result<Vec<Node>> {
        self.record_kinds_in(
            &[NodeKind::Function.as_str(), NodeKind::Method.as_str()],
            languages,
        );
        let mut nodes = self.store.callables_in_languages(languages)?;
        nodes.sort_by(callable_order);
        Ok(nodes)
    }

    /// The languages of the indexed files (upstream
    /// `getDistinctFileLanguages`): the stage's, which open the passes'
    /// gates, so no step's.
    fn file_languages(&self) -> Result<HashSet<Language>> {
        Ok(self.store.file_languages()?.into_iter().collect())
    }

    /// The indexed files of `languages`, by path.
    pub(super) fn files_of_languages(&self, languages: &[Language]) -> Result<Vec<String>> {
        let keys: Vec<u64> = languages
            .iter()
            .map(|language| keys::files_of(language.as_str()))
            .collect();
        self.recorder.keys(&keys);
        let mut files = Vec::new();
        for language in languages {
            files.extend(self.store.file_paths_of_language(*language)?);
        }
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Every indexed file, by path (upstream `getAllFilePaths`).
    pub(super) fn files(&self) -> Result<Vec<String>> {
        self.recorder.key(keys::files());
        let mut files = self.store.file_paths()?;
        files.sort();
        Ok(files)
    }

    /// Every unresolved reference of `kind`, in row order: what resolution
    /// left of it.
    pub(super) fn unresolved_refs_of_kind(&self, kind: EdgeKind) -> Result<Vec<UnresolvedRef>> {
        self.recorder.key(keys::refs_of_kind(kind.as_str()));
        Ok(self.store.unresolved_refs_of_kind(kind)?)
    }

    /// The stored references (`unresolved_refs` and `resolved_refs`) at
    /// `site`.
    pub(super) fn refs_at(&self, site: ReferenceSite) -> Result<Vec<UnresolvedRef>> {
        self.recorder
            .key(keys::site(&site.from_node_id, site.line, site.col));
        Ok(self
            .store
            .stored_refs_at_sites(&[site].into_iter().collect())?)
    }

    /// `file`'s text while it is what the index holds: its size and
    /// modification time are the recorded ones, or its content hashes to the
    /// recorded hash (the MCP engine's freshness probe). A file that drifted
    /// since it was indexed is read as absent, never sliced by its old
    /// nodes' lines, and the step that read it is not kept for a later sync
    /// (H2): the sync that reindexes it recomputes the step.
    pub(super) fn text(&self, file: &str) -> Option<Arc<str>> {
        self.recorder.key(keys::text(file));
        match self.read(file) {
            Read::Text(text) => Some(text),
            Read::Absent => None,
            Read::Drifted => {
                self.recorder.taint();
                None
            }
        }
    }

    /// Lines `start..=end` of `file`'s [`Self::text`], as [`slice_lines`]
    /// reads them. Only the lines are recorded, with their hash, so a change
    /// elsewhere in the file keeps the step.
    pub(super) fn text_lines(&self, file: &str, start: i64, end: i64) -> Option<String> {
        self.lines(file, start, end, false)
    }

    /// Line `line` of `file`'s [`Self::text`], as `split('\n').nth(line - 1)`
    /// reads it, recorded as [`Self::text_lines`] records its lines.
    pub(super) fn text_line(&self, file: &str, line: i64) -> Option<String> {
        self.lines(file, line, line, true)
    }

    fn lines(&self, file: &str, start: i64, end: i64, line: bool) -> Option<String> {
        match self.read(file) {
            Read::Text(text) => {
                let lines = read_lines(&text, start, end, line);
                self.recorder.slice(Slice {
                    path: file.to_string(),
                    start,
                    end,
                    line,
                    hash: lines.as_deref().map(hash_content),
                });
                lines
            }
            Read::Absent => {
                self.recorder.key(keys::text(file));
                None
            }
            Read::Drifted => {
                self.recorder.key(keys::text(file));
                self.recorder.taint();
                None
            }
        }
    }

    /// The indexed files of `gate`'s scope that `keep` keeps, by path, with
    /// their texts, of those whose text `gate` passes under `flags`. Only the
    /// files it passes are recorded: a file it fails is read and dropped,
    /// and a later sync asks the gate again of a changed file.
    pub(super) fn gated_texts(
        &self,
        gate: &'static TextGate,
        flags: u32,
        keep: impl Fn(&str) -> bool,
    ) -> Result<Vec<(String, Arc<str>)>> {
        let table = self.file_table()?;
        let mut passed = Vec::new();
        for stored in &table.records {
            if !(gate.scope)(&stored.path, stored.language) || !keep(&stored.path) {
                continue;
            }
            match self.read(&stored.path) {
                Read::Text(text) => {
                    let input = GateInput { text: &text, flags };
                    if (gate.check)(&input) {
                        passed.push((stored.path.clone(), text));
                    }
                }
                Read::Absent => {}
                Read::Drifted => self.recorder.taint(),
            }
        }
        let contributors: Vec<String> = passed.iter().map(|(path, _)| path.clone()).collect();
        self.recorder.gate(gate.name, flags, &contributors);
        Ok(passed)
    }

    /// What the view reads of `file`, cached for the stage run.
    pub(super) fn read(&self, file: &str) -> Read {
        if let Some(cached) = self.texts.borrow().get(file) {
            return cached.clone();
        }
        let read = match self.file_record(file) {
            Some(stored) => fresh_text(&self.project_root, &stored, self.max_file_size),
            None => Read::Absent,
        };
        self.texts
            .borrow_mut()
            .insert(file.to_string(), read.clone());
        read
    }

    /// `file`'s row of the files table, if it has one.
    pub(super) fn file_record(&self, file: &str) -> Option<FileRecord> {
        if let Some(table) = self.files.get() {
            return table.by_path.get(file).map(|&at| table.records[at].clone());
        }
        self.store.file_by_path(file).ok().flatten()
    }

    fn file_table(&self) -> Result<&FileTable> {
        if self.files.get().is_none() {
            let mut records = self.store.all_files()?;
            records.sort_by(|a, b| a.path.cmp(&b.path));
            let by_path = records
                .iter()
                .enumerate()
                .map(|(at, stored)| (stored.path.clone(), at))
                .collect();
            let _ = self.files.set(FileTable { records, by_path });
        }
        Ok(self.files.get().expect("the files table was read above"))
    }

    /// Every indexed file's [`Self::text`], read at once before the steps
    /// and the passes (W7-15p): the file records in one query, the files in
    /// parallel. The event-emitter pass reads every indexed file in each
    /// stage, so this reads no file the passes would not; a path the files
    /// table lacks is still read when asked for.
    pub(super) fn read_texts(&self) -> Result<()> {
        let root = &self.project_root;
        let max_file_size = self.max_file_size;
        let table = self.file_table()?;
        let texts: Vec<(String, Read)> = table
            .records
            .par_iter()
            .map(|stored| (stored.path.clone(), fresh_text(root, stored, max_file_size)))
            .collect();
        self.texts.borrow_mut().extend(texts);
        Ok(())
    }
}

/// Lines `start..=end` of `text` as [`slice_lines`] reads them, or line
/// `start` as `split('\n').nth(start - 1)` does (`line`).
pub(super) fn read_lines(text: &str, start: i64, end: i64, line: bool) -> Option<String> {
    if line {
        usize::try_from(start - 1)
            .ok()
            .and_then(|at| text.split('\n').nth(at))
            .map(str::to_string)
    } else {
        slice_lines(text, start, end)
    }
}

/// What the view reads of `stored`'s file (see [`View::text`]): its text
/// when it is read within the size bound and its size and modification time
/// are the recorded ones or its content hashes to the recorded hash; absent
/// for a file over the bound or not source that still reads as its row
/// says; drifted otherwise.
fn fresh_text(project_root: &Path, stored: &FileRecord, max_file_size: u64) -> Read {
    let Ok((metadata, source)) = read_source_file(
        &project_root.join(&stored.path),
        &stored.path,
        max_file_size,
    ) else {
        return Read::Drifted;
    };
    let stat_matches = stored.size == metadata.len() as i64
        && metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .is_some_and(|since| since.as_millis() as i64 == stored.modified_at);
    match source {
        SourceText::Text(text) => {
            if !stat_matches && stored.content_hash != hash_content(&text) {
                return Read::Drifted;
            }
            Read::Text(Arc::from(text))
        }
        SourceText::Oversize(size) => {
            if stat_matches || stored.content_hash == hash_content(&oversize_stamp(size)) {
                Read::Absent
            } else {
                Read::Drifted
            }
        }
        SourceText::MpegTransportStream => {
            if stat_matches {
                Read::Absent
            } else {
                Read::Drifted
            }
        }
    }
}

/// The prerequisite steps' edges, indexed by each end.
#[derive(Default)]
struct Overlay {
    edges: Vec<Edge>,
    by_source: HashMap<String, Vec<usize>>,
    by_target: HashMap<String, Vec<usize>>,
}

impl Overlay {
    fn extend(&mut self, edges: impl IntoIterator<Item = Edge>) {
        for edge in edges {
            let at = self.edges.len();
            self.by_source
                .entry(edge.source.clone())
                .or_default()
                .push(at);
            self.by_target
                .entry(edge.target.clone())
                .or_default()
                .push(at);
            self.edges.push(edge);
        }
    }

    fn at<'o>(&'o self, slots: Option<&'o Vec<usize>>) -> impl Iterator<Item = &'o Edge> {
        slots.into_iter().flatten().map(|&at| &self.edges[at])
    }

    fn out_of(&self, source: &str, kind: EdgeKind) -> impl Iterator<Item = &Edge> {
        self.at(self.by_source.get(source))
            .filter(move |edge| edge.kind == kind)
    }

    fn reaching(&self, target: &str, kind: EdgeKind) -> impl Iterator<Item = &Edge> {
        self.at(self.by_target.get(target))
            .filter(move |edge| edge.kind == kind)
    }

    fn of_kind(&self, kind: EdgeKind) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |edge| edge.kind == kind)
    }
}

/// The 1-based line of a byte offset into one source, its newlines indexed
/// once (upstream `makeLineAt`).
pub(super) struct LineAt {
    newlines: Vec<usize>,
    base: i64,
}

impl LineAt {
    /// Lines of `source`, which starts at line `base`.
    pub(super) fn new(source: &str, base: i64) -> Self {
        Self {
            newlines: source.match_indices('\n').map(|(at, _)| at).collect(),
            base,
        }
    }

    pub(super) fn line(&self, offset: usize) -> i64 {
        self.base + self.newlines.partition_point(|&at| at < offset) as i64
    }
}

/// Lines `start..=end` of `content`, joined back (upstream `sliceLines`);
/// `None` without both.
pub(super) fn slice_lines(content: &str, start: i64, end: i64) -> Option<String> {
    if start < 1 || end < 1 {
        return None;
    }
    let lines: Vec<&str> = content
        .split('\n')
        .skip((start - 1) as usize)
        .take((end - start + 1).max(0) as usize)
        .collect();
    Some(lines.join("\n"))
}

/// The innermost function, method or component whose lines hold `line`
/// (upstream `enclosingFn`): of those, the latest-starting, a later one in
/// `nodes`' order winning a tie.
pub(super) fn enclosing_fn(nodes: &[Node], line: i64) -> Option<&Node> {
    let mut best: Option<&Node> = None;
    for node in nodes {
        if !matches!(
            node.kind,
            NodeKind::Method | NodeKind::Function | NodeKind::Component
        ) {
            continue;
        }
        if node.start_line <= line && node.end_line.max(node.start_line) >= line {
            if best.is_none_or(|best| node.start_line >= best.start_line) {
                best = Some(node);
            }
        }
    }
    best
}

/// The latest-starting constant or variable whose lines hold `line`
/// (upstream `enclosingValue`): the value a top-level registration is
/// written inside, a later one in `nodes`' order winning a tie.
pub(super) fn enclosing_value(nodes: &[Node], line: i64) -> Option<&Node> {
    let mut best: Option<&Node> = None;
    for node in nodes {
        if !matches!(node.kind, NodeKind::Constant | NodeKind::Variable) {
            continue;
        }
        if node.start_line <= line && node.end_line.max(node.start_line) >= line {
            if best.is_none_or(|best| node.start_line >= best.start_line) {
                best = Some(node);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "cg-synthesis-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("index.db")).unwrap();
        (store, dir)
    }

    fn function(id: &str, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 2,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn synthesized(source: &str, target: &str, by: &str) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Calls,
            metadata: Some(serde_json::json!({ "synthesizedBy": by })),
            line: None,
            col: None,
            provenance: Some("heuristic".to_string()),
        }
    }

    /// Upstream's merge: the first pass to emit a `source>target` pair keeps
    /// it, whatever a later pass says of the pair; the output is sorted.
    #[test]
    fn merge_keeps_the_first_edge_of_a_pair() {
        let by = |edge: &Edge| edge.metadata.as_ref().unwrap()["synthesizedBy"].clone();
        let merged = merge(vec![
            vec![
                synthesized("c", "b", "callback"),
                synthesized("a", "b", "callback"),
            ],
            vec![
                synthesized("a", "b", "event-emitter"),
                synthesized("a", "c", "event-emitter"),
            ],
        ]);
        let rows: Vec<(String, String, serde_json::Value)> = merged
            .iter()
            .map(|edge| (edge.source.clone(), edge.target.clone(), by(edge)))
            .collect();
        assert_eq!(
            rows,
            [
                ("a".to_string(), "b".to_string(), "callback".into()),
                ("a".to_string(), "c".to_string(), "event-emitter".into()),
                ("c".to_string(), "b".to_string(), "callback".into()),
            ]
        );
    }

    /// A prerequisite step's edge takes no `source>target` key: a pass's
    /// edge between the same two nodes is kept beside it.
    #[test]
    fn combine_keeps_prerequisite_edges_out_of_the_merge() {
        let contains = Edge {
            kind: EdgeKind::Contains,
            provenance: None,
            ..synthesized("a", "b", "go-method-contains")
        };
        let combined = combine(
            vec![contains],
            vec![vec![synthesized("a", "b", "callback")]],
        );
        let rows: Vec<(&str, &str, &str)> = combined
            .iter()
            .map(|edge| {
                (
                    edge.source.as_str(),
                    edge.target.as_str(),
                    edge.kind.as_str(),
                )
            })
            .collect();
        assert_eq!(rows, [("a", "b", "calls"), ("a", "b", "contains")]);
    }

    /// A run that kept every step writes back what the cascade dropped only
    /// while that leaves the merge's rows (W9-00 row 27): a base edge the
    /// sync removed frees an identity an edge the insert ignored takes now,
    /// a node under a new id gives an edge the insert skipped its node, and
    /// a synthesized edge written before the stage is not the merge's.
    #[test]
    fn the_cascade_is_written_back_only_while_no_identity_moved() {
        use codegraph_store::{EdgeImage, NodeImage, SynthesizedEdge};
        let node = |id: &str, end_line: i64| NodeImage {
            id: id.to_string(),
            kind: "function".to_string(),
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: "src/a.ts".to_string(),
            language: "typescript".to_string(),
            start_line: 1,
            end_line,
            signature: None,
            decorators: None,
        };
        let base = EdgeImage {
            source: "a".to_string(),
            target: "b".to_string(),
            kind: "calls".to_string(),
            line: Some(3),
            col: Some(4),
        };
        let dropped = SynthesizedEdge {
            source: "b".to_string(),
            target: "a".to_string(),
            kind: "calls".to_string(),
            metadata: Some(r#"{"synthesizedBy":"callback"}"#.to_string()),
            line: None,
            col: None,
            provenance: Some("heuristic".to_string()),
        };
        let mut changes = ChangeSet::default();
        changes.synthesized.removed.push(dropped.clone());
        assert!(can_restore(&changes), "the cascade alone");
        changes.edges.added.push(base.clone());
        assert!(can_restore(&changes), "an added base edge frees nothing");
        changes.edges.removed.push(base);
        assert!(!can_restore(&changes), "a removed one does");
        let mut changes = ChangeSet::default();
        changes.synthesized.added.push(dropped);
        assert!(!can_restore(&changes), "a synthesized edge written");
        let mut changes = ChangeSet::default();
        changes.nodes.removed.push(node("a", 3));
        changes.nodes.added.push(node("a", 4));
        assert!(can_restore(&changes), "a node rewritten under its id");
        changes.nodes.added.push(node("b", 3));
        assert!(!can_restore(&changes), "a node under a new id");
    }

    /// A pass reads the base graph: the stored synthesized edges of the last
    /// stage are outside the view, as a fresh index has none before it.
    #[test]
    fn the_view_reads_only_base_edges() {
        let (mut store, dir) = temp_store("view");
        store
            .upsert_nodes(&[
                function("a", "src/a.ts"),
                function("b", "src/b.ts"),
                function("c", "src/c.ts"),
            ])
            .unwrap();
        let base = Edge {
            line: Some(3),
            col: Some(1),
            metadata: None,
            provenance: None,
            ..synthesized("a", "b", "base")
        };
        store
            .insert_edges(&[
                base,
                synthesized("a", "b", "callback"),
                synthesized("c", "b", "event-emitter"),
                synthesized("a", "c", "callback"),
            ])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let ends = |edges: Vec<Edge>| -> Vec<(String, String, Option<i64>)> {
            edges
                .into_iter()
                .map(|edge| (edge.source, edge.target, edge.line))
                .collect()
        };
        let base_end = vec![("a".to_string(), "b".to_string(), Some(3))];
        assert_eq!(
            ends(view.edges_into("b", EdgeKind::Calls).unwrap()),
            base_end
        );
        assert_eq!(
            ends(view.edges_from("a", EdgeKind::Calls).unwrap()),
            base_end
        );
        assert!(view.edges_into("c", EdgeKind::Calls).unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file's callables read on their own are its nodes of the kinds
    /// `enclosing_fn` takes, in the order of all its nodes (W7-15d's cost
    /// revision).
    #[test]
    fn a_files_callables_are_its_enclosing_candidates_in_node_order() {
        let (mut store, dir) = temp_store("callables");
        let node = |id: &str, kind: NodeKind, line: i64| Node {
            kind,
            start_line: line,
            end_line: line + 1,
            ..function(id, "src/a.go")
        };
        store
            .upsert_nodes(&[
                node("m", NodeKind::Method, 3),
                node("s", NodeKind::Struct, 1),
                node("f", NodeKind::Function, 3),
                node("c", NodeKind::Component, 2),
                node("v", NodeKind::Variable, 2),
                node("g", NodeKind::Function, 1),
                function("other", "src/b.go"),
            ])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let expected: Vec<String> = view
            .nodes_in_file("src/a.go")
            .unwrap()
            .into_iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Component
                )
            })
            .map(|node| node.id)
            .collect();
        let read: Vec<String> = view
            .callables_in_file("src/a.go")
            .unwrap()
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(read, expected);
        assert_eq!(expected, ["g", "c", "f", "m"]);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The classes, methods and properties of many files read at once
    /// (W7-18d's cost revision) are each file's nodes of those kinds in
    /// [`View::nodes_in_file`]'s order, by line and then id; a file without
    /// any, or not indexed, has no entry.
    #[test]
    fn many_files_classes_and_members_come_in_each_files_node_order() {
        let (mut store, dir) = temp_store("classes-and-members");
        let node = |id: &str, kind: NodeKind, file: &str, line: i64| Node {
            kind,
            start_line: line,
            end_line: line + 1,
            ..function(&format!("{file}:{id}"), file)
        };
        let mut nodes = Vec::new();
        for file in ["src/a.ts", "src/b.ts"] {
            nodes.extend([
                node("m2", NodeKind::Method, file, 3),
                node("p", NodeKind::Property, file, 2),
                node("f", NodeKind::Function, file, 2),
                node("k", NodeKind::Class, file, 1),
                node("m1", NodeKind::Method, file, 3),
                node("i", NodeKind::Import, file, 1),
            ]);
        }
        nodes.push(node("f", NodeKind::Function, "src/c.ts", 1));
        store.upsert_nodes(&nodes).unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let files = ["src/a.ts", "src/b.ts", "src/c.ts", "src/absent.ts"].map(str::to_string);
        let read = view.classes_and_members_in_files(&files).unwrap();
        for file in ["src/a.ts", "src/b.ts"] {
            let expected: Vec<Node> = view
                .nodes_in_file(file)
                .unwrap()
                .into_iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::Class | NodeKind::Method | NodeKind::Property
                    )
                })
                .collect();
            let ids: Vec<String> = expected.iter().map(|node| node.id.clone()).collect();
            assert_eq!(ids, ["k", "p", "m1", "m2"].map(|id| format!("{file}:{id}")));
            assert_eq!(read.get(file), Some(&expected), "{file}");
        }
        assert_eq!(read.len(), 2);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The view returns upstream's orders: a file's nodes by line and id,
    /// not column; callables method first, then by file, line and id; edges
    /// by their other end, then line, whatever order the store keeps.
    #[test]
    fn the_view_walks_upstreams_order() {
        let (mut store, dir) = temp_store("order");
        let at = |id: &str, kind: NodeKind, file: &str, line: i64, column: i64| Node {
            kind,
            name: format!("on{}", id.to_uppercase()),
            start_line: line,
            end_line: line,
            start_column: column,
            ..function(id, file)
        };
        store
            .upsert_nodes(&[
                at("b", NodeKind::Function, "src/a.ts", 3, 0),
                at("a", NodeKind::Function, "src/a.ts", 3, 9),
                at("m", NodeKind::Method, "src/z.ts", 1, 0),
                at("s1", NodeKind::Function, "src/c.ts", 1, 0),
                at("s2", NodeKind::Function, "src/c.ts", 2, 0),
            ])
            .unwrap();
        let call = |source: &str, target: &str, line: i64| Edge {
            line: Some(line),
            col: Some(0),
            metadata: None,
            provenance: None,
            ..synthesized(source, target, "base")
        };
        store
            .insert_edges(&[call("s2", "a", 1), call("s1", "b", 1), call("s1", "a", 9)])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let ids =
            |nodes: Vec<Node>| -> Vec<String> { nodes.into_iter().map(|node| node.id).collect() };
        assert_eq!(ids(view.nodes_in_file("src/a.ts").unwrap()), ["a", "b"]);
        assert_eq!(
            ids(view.callables_named_like(&["on[A-Z]*"], &[], &[]).unwrap()),
            ["m", "a", "b", "s1", "s2"]
        );
        let ends = |edges: Vec<Edge>| -> Vec<(String, String)> {
            edges
                .into_iter()
                .map(|edge| (edge.source, edge.target))
                .collect()
        };
        assert_eq!(
            ends(view.edges_into("a", EdgeKind::Calls).unwrap()),
            [
                ("s1".to_string(), "a".to_string()),
                ("s2".to_string(), "a".to_string())
            ]
        );
        assert_eq!(
            ends(view.edges_from("s1", EdgeKind::Calls).unwrap()),
            [
                ("s1".to_string(), "a".to_string()),
                ("s1".to_string(), "b".to_string())
            ]
        );
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The batched reads give each end what its own read gives: base edges
    /// and the overlay's, without the stored synthesized ones, in the same
    /// order; ids asked twice, ends without edges and more ends than one
    /// chunk holds included. Each parent's methods are `methods_of`'s.
    #[test]
    fn the_batched_reads_are_the_single_ones() {
        let (mut store, dir) = temp_store("batched");
        let at = |id: &str, kind: NodeKind| Node {
            kind,
            ..function(id, "src/a.ts")
        };
        store
            .upsert_nodes(&[
                at("t", NodeKind::Class),
                at("u", NodeKind::Class),
                at("m1", NodeKind::Method),
                at("m2", NodeKind::Method),
                at("f", NodeKind::Function),
            ])
            .unwrap();
        let contains = |source: &str, target: &str, line: Option<i64>| Edge {
            kind: EdgeKind::Contains,
            line,
            metadata: None,
            provenance: None,
            ..synthesized(source, target, "base")
        };
        store
            .insert_edges(&[
                contains("t", "m2", Some(4)),
                contains("t", "f", Some(2)),
                contains("t", "m1", Some(9)),
                contains("u", "m1", None),
                Edge {
                    kind: EdgeKind::Contains,
                    ..synthesized("u", "m2", "go-method-contains")
                },
            ])
            .unwrap();
        let mut view = View::new(&store, &dir, 1 << 20);
        view.overlay
            .extend([contains("u", "m2", Some(1)), contains("t", "m1", Some(1))]);
        let mut ids: Vec<String> = (0..1100).map(|n| format!("absent{n}")).collect();
        ids.extend(["t", "u", "m1", "m2", "f", "t", "m1"].map(String::from));
        let ends = |edges: &[Edge]| -> Vec<(String, String, Option<i64>)> {
            edges
                .iter()
                .map(|edge| (edge.source.clone(), edge.target.clone(), edge.line))
                .collect()
        };
        let into = view.edges_into_each(&ids, EdgeKind::Contains).unwrap();
        let from = view.edges_from_each(&ids, EdgeKind::Contains).unwrap();
        let methods = super::super::dispatch::methods_of_each(&view, &ids).unwrap();
        for id in &ids {
            let single = view.edges_into(id, EdgeKind::Contains).unwrap();
            assert_eq!(
                ends(into.get(id).map_or(&[][..], Vec::as_slice)),
                ends(&single),
                "into {id}"
            );
            let single = view.edges_from(id, EdgeKind::Contains).unwrap();
            assert_eq!(
                ends(from.get(id).map_or(&[][..], Vec::as_slice)),
                ends(&single),
                "from {id}"
            );
            let single: Vec<String> = super::super::dispatch::methods_of(&view, id)
                .unwrap()
                .into_iter()
                .map(|node| node.id)
                .collect();
            let batched: Vec<String> = methods[id].iter().map(|node| node.id.clone()).collect();
            assert_eq!(batched, single, "methods of {id}");
        }
        assert_eq!(
            ends(&from["t"]),
            [
                ("t".to_string(), "f".to_string(), Some(2)),
                ("t".to_string(), "m1".to_string(), Some(1)),
                ("t".to_string(), "m1".to_string(), Some(9)),
                ("t".to_string(), "m2".to_string(), Some(4)),
            ]
        );
        assert_eq!(
            methods["t"]
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            ["m1", "m1", "m2"]
        );
        let found = view.nodes(&ids).unwrap();
        let mut found: Vec<&str> = found.keys().map(String::as_str).collect();
        found.sort_unstable();
        assert_eq!(found, ["f", "m1", "m2", "t", "u"]);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// W7-21's cost revision: the targets a script calls are read with one
    /// query beside the overlay. A base `calls` edge from a script and an
    /// overlay one count; a stored synthesized edge, another kind, a caller
    /// in another language and an overlay edge into another node do not:
    /// what [`View::edges_into_each`] and the callers' rows give.
    #[test]
    fn the_targets_a_language_calls_are_its_base_and_overlay_calls() {
        let (mut store, dir) = temp_store("called-from");
        let at = |id: &str, kind: NodeKind, language: Language| Node {
            kind,
            language,
            ..function(id, "src/a.ts")
        };
        store
            .upsert_nodes(&[
                at("ts", NodeKind::Function, Language::TypeScript),
                at("tsx", NodeKind::Function, Language::Tsx),
                at("java", NodeKind::Function, Language::Java),
                at("m1", NodeKind::Method, Language::Java),
                at("m2", NodeKind::Method, Language::Java),
                at("m3", NodeKind::Method, Language::Java),
                at("m4", NodeKind::Method, Language::Java),
                at("m5", NodeKind::Method, Language::ObjC),
                at("m6", NodeKind::Method, Language::ObjC),
            ])
            .unwrap();
        let calls = |source: &str, target: &str| Edge {
            line: Some(1),
            col: Some(0),
            metadata: None,
            provenance: None,
            ..synthesized(source, target, "base")
        };
        store
            .insert_edges(&[
                calls("ts", "m1"),
                synthesized("ts", "m2", "rn-cross-platform"),
                calls("java", "m3"),
                Edge {
                    kind: EdgeKind::References,
                    ..calls("ts", "m4")
                },
            ])
            .unwrap();
        let mut view = View::new(&store, &dir, 1 << 20);
        view.overlay
            .extend([calls("tsx", "m5"), calls("java", "m4"), calls("ts", "m6")]);
        let targets: Vec<String> = ["m1", "m2", "m3", "m4", "m5"].map(String::from).to_vec();
        let scripts = [
            Language::TypeScript,
            Language::Tsx,
            Language::JavaScript,
            Language::Jsx,
        ];
        let mut called: Vec<String> = view
            .called_from(&targets, &scripts)
            .unwrap()
            .into_iter()
            .collect();
        called.sort();
        assert_eq!(called, ["m1", "m5"]);
        let into = view.edges_into_each(&targets, EdgeKind::Calls).unwrap();
        let callers: Vec<String> = into
            .values()
            .flatten()
            .map(|edge| edge.source.clone())
            .collect();
        let sources = view.nodes(&callers).unwrap();
        let mut read: Vec<String> = targets
            .iter()
            .filter(|target| {
                into.get(*target).is_some_and(|edges| {
                    edges.iter().any(|edge| {
                        sources
                            .get(&edge.source)
                            .is_some_and(|source| scripts.contains(&source.language))
                    })
                })
            })
            .cloned()
            .collect();
        read.sort();
        assert_eq!(read, called);
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// W7-21's cost revision: the methods' light rows come in
    /// [`node_order`], the order of the whole nodes read before.
    #[test]
    fn method_headers_come_in_node_order() {
        let (mut store, dir) = temp_store("method-headers");
        let at = |id: &str, file: &str, line: i64, language: Language| Node {
            kind: NodeKind::Method,
            language,
            start_line: line,
            ..function(id, file)
        };
        store
            .upsert_nodes(&[
                at("b2", "src/b.m", 2, Language::ObjC),
                at("a9", "src/a.java", 9, Language::Java),
                at("b1z", "src/b.m", 1, Language::ObjC),
                at("b1a", "src/b.m", 1, Language::ObjC),
                at("c1", "src/c.swift", 1, Language::Swift),
            ])
            .unwrap();
        let view = View::new(&store, &dir, 1 << 20);
        let languages = [Language::ObjC, Language::Java];
        let headers: Vec<(String, String, Language, String, i64)> = view
            .method_headers_in(&languages)
            .unwrap()
            .into_iter()
            .map(|header| {
                (
                    header.id,
                    header.name,
                    header.language,
                    header.file_path,
                    header.start_line,
                )
            })
            .collect();
        let nodes: Vec<(String, String, Language, String, i64)> = view
            .nodes_of_kinds_in(&[NodeKind::Method], &languages)
            .unwrap()
            .into_iter()
            .map(|node| {
                (
                    node.id,
                    node.name,
                    node.language,
                    node.file_path,
                    node.start_line,
                )
            })
            .collect();
        assert_eq!(headers, nodes);
        assert_eq!(
            headers
                .iter()
                .map(|header| header.0.as_str())
                .collect::<Vec<_>>(),
            ["a9", "b1a", "b1z", "b2"]
        );
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The texts read at once before the steps and the passes are the ones
    /// `text` reads one at a time (W7-15p): a file whose stat is the recorded
    /// one, one whose stat differs but whose content hashes to the recorded
    /// hash, one whose content changed, one over the size bound, an MPEG
    /// transport stream named `.ts`, and files the table lacks.
    #[test]
    fn the_texts_read_at_once_are_the_ones_read_one_at_a_time() {
        let (store, dir) = temp_store("texts");
        let same = "export const same = 1;\n";
        let touched = "export const touched = 2;\n";
        let big = "x".repeat(5000);
        let mut clip = vec![0_u8; 188 * 16];
        for packet in clip.chunks_mut(188) {
            packet[0] = 0x47;
        }
        for (name, bytes) in [
            ("same.ts", same.as_bytes()),
            ("touched.ts", touched.as_bytes()),
            ("edited.ts", b"export const edited = 3;\n".as_slice()),
            ("big.ts", big.as_bytes()),
            ("clip.ts", clip.as_slice()),
            ("unlisted.ts", b"export const unlisted = 4;\n".as_slice()),
        ] {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        let record = |path: &str, text: &str, stat_matches: bool| {
            let metadata = std::fs::metadata(dir.join(path)).unwrap();
            let modified = metadata
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            FileRecord {
                path: path.to_string(),
                content_hash: hash_content(text),
                language: Language::TypeScript,
                size: metadata.len() as i64,
                modified_at: if stat_matches {
                    modified
                } else {
                    modified - 1000
                },
                indexed_at: 0,
                node_count: 0,
                errors: Vec::new(),
                generated: false,
            }
        };
        for file in [
            record("same.ts", "", true),
            record("touched.ts", touched, false),
            record("edited.ts", "export const before = 0;\n", false),
            record("big.ts", &big, true),
            record("clip.ts", "", true),
        ] {
            store.upsert_file(&file).unwrap();
        }
        let at_once = View::new(&store, &dir, 4096);
        at_once.read_texts().unwrap();
        assert_eq!(at_once.texts.borrow().len(), 5);
        let one_at_a_time = View::new(&store, &dir, 4096);
        let read = |view: &View<'_>, name: &str| view.text(name).map(|text| text.to_string());
        let names = [
            "same.ts",
            "touched.ts",
            "edited.ts",
            "big.ts",
            "clip.ts",
            "unlisted.ts",
            "absent.ts",
        ];
        for name in names {
            assert_eq!(read(&at_once, name), read(&one_at_a_time, name), "{name}");
        }
        assert_eq!(read(&at_once, "same.ts").as_deref(), Some(same));
        assert_eq!(read(&at_once, "touched.ts").as_deref(), Some(touched));
        for name in ["edited.ts", "big.ts", "clip.ts", "unlisted.ts", "absent.ts"] {
            assert_eq!(read(&at_once, name), None, "{name}");
        }
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn line_at_and_slices_read_one_based_lines() {
        let source = "a\nbb\nccc\n";
        let lines = LineAt::new(source, 1);
        assert_eq!(
            [0, 1, 2, 4, 5, 8].map(|offset| lines.line(offset)),
            [1, 1, 2, 2, 3, 3]
        );
        assert_eq!(LineAt::new(source, 10).line(5), 12);
        assert_eq!(slice_lines(source, 2, 3).as_deref(), Some("bb\nccc"));
        assert_eq!(slice_lines(source, 0, 3), None);
    }

    /// The latest-starting constant or variable holding the line, a later
    /// one in the nodes' order winning a tie; a function is no value, and a
    /// value whose end precedes its start holds its start line.
    #[test]
    fn the_enclosing_value_is_the_latest_starting_value() {
        let value = |id: &str, kind: NodeKind, lines: (i64, i64)| Node {
            kind,
            start_line: lines.0,
            end_line: lines.1,
            ..function(id, "src/a.ts")
        };
        let nodes = [
            value("outer", NodeKind::Constant, (1, 10)),
            value("inner", NodeKind::Variable, (3, 5)),
            value("callable", NodeKind::Function, (4, 4)),
            value("later", NodeKind::Constant, (3, 6)),
            value("line", NodeKind::Variable, (8, 0)),
        ];
        let at = |line: i64| enclosing_value(&nodes, line).map(|node| node.id.as_str());
        assert_eq!(at(2), Some("outer"));
        assert_eq!(at(4), Some("later"));
        assert_eq!(at(8), Some("line"));
        assert_eq!(at(9), Some("outer"));
        assert_eq!(at(11), None);
    }

    /// A timed stage lists each step and pass its gates open, once each and
    /// in run order, and its parts sum to no more than the run took
    /// (W9-00a).
    #[test]
    fn a_timed_stage_lists_each_pass_that_ran_once_in_order() {
        let (mut store, dir) = temp_store("timed");
        let text = "export function a() {}\n";
        std::fs::write(dir.join("a.ts"), text).unwrap();
        store
            .upsert_file(&FileRecord {
                path: "a.ts".to_string(),
                content_hash: hash_content(text),
                language: Language::TypeScript,
                size: text.len() as i64,
                modified_at: 0,
                indexed_at: 0,
                node_count: 1,
                errors: Vec::new(),
                generated: false,
            })
            .unwrap();
        store.upsert_nodes(&[function("a", "a.ts")]).unwrap();
        let started = Instant::now();
        let (_stats, timings) =
            run_stage_timed(&mut store, &dir, &ExtractOptions::default()).unwrap();
        let wall = started.elapsed();
        let languages: HashSet<Language> = [Language::TypeScript].into_iter().collect();
        let expected: Vec<&str> = PREREQUISITES
            .iter()
            .chain(PASSES)
            .filter(|(_, gate, _)| gate(&languages))
            .map(|(name, _, _)| *name)
            .collect();
        let ran: Vec<&str> = timings.passes.iter().map(|(name, _)| *name).collect();
        assert_eq!(ran, expected);
        assert!(ran.contains(&"fieldEdges"), "{ran:?}");
        assert!(!ran.contains(&"goImplements"), "{ran:?}");
        let passes: Duration = timings.passes.iter().map(|(_, took)| *took).sum();
        let sum = timings.read + passes + timings.merge + timings.replace + timings.inputs;
        assert!(sum <= wall, "parts {sum:?} over the stage's {wall:?}");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `path` written with `text` and indexed as it is.
    fn indexed(store: &Store, dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, text).unwrap();
        let metadata = std::fs::metadata(&full).unwrap();
        store
            .upsert_file(&FileRecord {
                path: path.to_string(),
                content_hash: hash_content(text),
                language: Language::TypeScript,
                size: metadata.len() as i64,
                modified_at: metadata
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64,
                indexed_at: 0,
                node_count: 0,
                errors: Vec::new(),
                generated: false,
            })
            .unwrap();
    }

    fn holds(footprint: &super::super::footprint::Footprint, key: u64) -> bool {
        footprint.keys.contains(&key)
    }

    /// Each read of the base graph records the key of what would change its
    /// answer, the pattern lookups their patterns, and an empty answer as
    /// much as a full one.
    #[test]
    fn each_read_records_what_would_change_it() {
        let (mut store, dir) = temp_store("record-reads");
        store
            .upsert_nodes(&[function("a", "src/a.ts"), function("b", "src/b.ts")])
            .unwrap();
        indexed(&store, &dir, "src/a.ts", "export function a() {}\n");
        let view = View::new(&store, &dir, 1 << 20);
        view.recorder.begin_pass();
        view.node("a").unwrap();
        view.node("missing").unwrap();
        view.nodes_named("b").unwrap();
        view.nodes_in_file("src/b.ts").unwrap();
        view.callables_in_file("src/a.ts").unwrap();
        view.nodes_of_kinds_in(&[NodeKind::Class], &[Language::Kotlin])
            .unwrap();
        view.edges_into("a", EdgeKind::Calls).unwrap();
        view.edges_from("b", EdgeKind::References).unwrap();
        view.edges_of_kind(EdgeKind::Implements).unwrap();
        view.files().unwrap();
        view.files_of_languages(&[Language::Tsx]).unwrap();
        view.text("src/a.ts").unwrap();
        assert!(view.text("src/none.ts").is_none());
        assert!(!view.is_flagged_generated("src/a.ts"));
        view.callables_named_like(&["on[A-Z]*"], &["%emit%"], &["watch"])
            .unwrap();
        let footprint = view.recorder.end_pass();
        for key in [
            keys::node("a"),
            keys::node("missing"),
            keys::name("b"),
            keys::file_nodes("src/b.ts"),
            keys::file_kind("src/a.ts", "function"),
            keys::file_kind("src/a.ts", "component"),
            keys::kind_in("class", "kotlin"),
            keys::edges_into("a", "calls"),
            keys::edges_from("b", "references"),
            keys::edges_of_kind("implements"),
            keys::files(),
            keys::files_of("tsx"),
            keys::text("src/a.ts"),
            keys::text("src/none.ts"),
            keys::file_row("src/a.ts"),
        ] {
            assert!(holds(&footprint, key), "{key:016x}");
        }
        assert!(!holds(&footprint, keys::text("src/b.ts")));
        assert_eq!(
            footprint.predicates.iter().cloned().collect::<Vec<_>>(),
            [Predicate::Callables {
                globs: vec!["on[A-Z]*".to_string()],
                likes: vec!["%emit%".to_string()],
                names: vec!["watch".to_string()],
            }]
        );
        assert!(footprint.reusable());
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A gated scan records the files its check passes and nothing of the
    /// ones it fails, not even the file list; lines record their hash, not
    /// the file.
    #[test]
    fn a_gate_records_what_it_passes_and_lines_their_hash() {
        let (store, dir) = temp_store("record-gates");
        indexed(&store, &dir, "src/bus.ts", "bus.emit('saved');\n");
        indexed(&store, &dir, "src/plain.ts", "export const x = 1;\n");
        indexed(&store, &dir, "src/lines.ts", "one\ntwo\nthree\n");
        let view = View::new(&store, &dir, 1 << 20);
        view.recorder.begin_pass();
        let passed = view
            .gated_texts(&callback::EMITTER_GATE, 0, |_| true)
            .unwrap();
        assert_eq!(
            passed
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["src/bus.ts"]
        );
        assert_eq!(
            view.text_lines("src/lines.ts", 2, 3).as_deref(),
            Some("two\nthree")
        );
        assert_eq!(view.text_line("src/lines.ts", 1).as_deref(), Some("one"));
        assert_eq!(view.text_line("src/lines.ts", 9), None);
        let footprint = view.recorder.end_pass();
        assert_eq!(
            footprint.gates.get(&("emitter".to_string(), 0)),
            Some(&["src/bus.ts".to_string()].into_iter().collect())
        );
        assert!(footprint.keys.is_empty(), "{:?}", footprint.keys);
        let slices: Vec<(i64, i64, bool, Option<String>)> = footprint
            .slices
            .iter()
            .map(|slice| (slice.start, slice.end, slice.line, slice.hash.clone()))
            .collect();
        assert_eq!(
            slices,
            [
                (1, 1, true, Some(hash_content("one"))),
                (2, 3, false, Some(hash_content("two\nthree"))),
                (9, 9, true, None),
            ]
        );
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An indexed file that drifted since it was indexed taints the step
    /// that reads it, through a plain read, a gated scan or its lines (H2);
    /// a file over the size bound that still reads as its row says does
    /// not, nor does a path the index does not hold.
    #[test]
    fn a_drifted_file_taints_its_reader() {
        let (store, dir) = temp_store("record-drift");
        indexed(&store, &dir, "src/drift.ts", "bus.emit('a');\n");
        indexed(&store, &dir, "src/big.ts", &"x".repeat(64));
        std::fs::write(dir.join("src/drift.ts"), "bus.emit('b'); // moved\n").unwrap();
        let read = |reads: &dyn Fn(&View<'_>)| {
            let view = View::new(&store, &dir, 32);
            view.recorder.begin_pass();
            reads(&view);
            view.recorder.end_pass()
        };
        assert!(read(&|view| assert!(view.text("src/drift.ts").is_none())).tainted);
        assert!(
            read(&|view| {
                view.gated_texts(&callback::EMITTER_GATE, 0, |_| true)
                    .unwrap();
            })
            .tainted
        );
        assert!(read(&|view| assert!(view.text_lines("src/drift.ts", 1, 1).is_none())).tainted);
        let calm = read(&|view| {
            assert!(view.text("src/big.ts").is_none());
            assert!(view.text("src/unindexed.ts").is_none());
        });
        assert!(!calm.tainted && calm.reusable());
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A probe on disk or a directory listing through the view's context
    /// keeps the step from a later sync; the tier pass's indexed-only probe
    /// reads the files table and does not.
    #[test]
    fn an_untracked_read_keeps_no_step() {
        use crate::types::ResolutionContext;
        let (store, dir) = temp_store("record-untracked");
        indexed(&store, &dir, "src/a.ts", "export const a = 1;\n");
        let view = View::new(&store, &dir, 1 << 20);
        view.recorder.begin_pass();
        assert!(view.context().indexed_files_only().file_exists("src/a.ts"));
        let indexed_only = view.recorder.end_pass();
        assert!(indexed_only.reusable());
        assert!(holds(&indexed_only, keys::file_row("src/a.ts")));
        view.recorder.begin_pass();
        assert!(view.context().file_exists("src/a.ts"));
        let probed = view.recorder.end_pass();
        assert!(!probed.reusable());
        view.recorder.begin_pass();
        view.context().list_directories("src");
        assert!(!view.recorder.end_pass().reusable());
        drop(view);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The records are a function of the base graph and the files: two
    /// stage runs over the same index write the same rows, and each step run
    /// alone in a view of its own records what it records in the stage, so
    /// no read leaks through a cache the steps share.
    #[test]
    fn records_are_deterministic_and_each_step_alone_reads_the_same() {
        let (mut store, dir) = temp_store("record-alone");
        let mut save = function("save", "src/bus.ts");
        save.end_line = 3;
        let mut handle = function("handle", "src/bus.ts");
        handle.start_line = 4;
        handle.end_line = 4;
        store
            .upsert_nodes(&[save, handle, function("plain", "src/plain.ts")])
            .unwrap();
        indexed(
            &store,
            &dir,
            "src/bus.ts",
            "function save() {\n  bus.emit('saved');\n}\nfunction handle() { bus.on('saved', handle); }\n",
        );
        indexed(&store, &dir, "src/plain.ts", "export function plain() {}\n");
        indexed(
            &store,
            &dir,
            "src/view.tsx",
            "export const V = () => <Child />;\n",
        );
        let options = ExtractOptions::default();
        let first = stage_rows(&store, &dir, &options).unwrap();
        let second = stage_rows(&store, &dir, &options).unwrap();
        assert_eq!(first, second);
        let in_stage: Vec<(String, String)> = first
            .into_iter()
            .filter(|(key, _)| key.starts_with(PASS_KEY_PREFIX))
            .collect();
        assert!(in_stage.len() > 10, "{}", in_stage.len());
        assert_eq!(records_alone(&store, &dir, &options).unwrap(), in_stage);
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
