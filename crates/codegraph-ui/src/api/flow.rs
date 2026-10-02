//! `GET /api/flow` — the call path between symbols, as cards. Upstream
//! `src/ui-server/api/flow.ts`.
//!
//! One card per hop, each opened at the exact line that makes the next call,
//! synthesized dynamic-dispatch hops marked and carrying the site they were
//! wired at. The path finder is `codegraph_graph::named_symbol_flow`. Three
//! questions arrive here:
//!
//! - `?from=&to=` — directed: both ends pinned, shortest path wins;
//! - `?symbols=a,b,c` — explore's question: longest chain among the named
//!   symbols, at most one unnamed bridge;
//! - `?hop=s<id>&hop=d<id>…` — a trail read as a flow: nothing is searched,
//!   only the edge that already connects each consecutive pair is found.
//!
//! Nothing here is cached at the payload level: a card carries source read from
//! disk, and its drift verdict changes without the index changing. Branch
//! conditions on links (`when`, family F11) are phase 2.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use codegraph_core::types::{Edge, EdgeKind, Node};
use codegraph_graph::flow_boundary::{BoundaryContinuation, continuations_from};
use codegraph_graph::graph::GraphTraverser;
use codegraph_graph::named_symbol_flow::{
    DIRECTED_MAX_HOPS, FlowMode, NamedSymbolFlow, NamedSymbolFlowOptions, normalize_token,
    resolve_named_symbol_flow,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::boundary::{NodeBoundary, find_dynamic_boundaries};
use super::source::{
    find_indexed_file, has_drifted_on_disk, max_file_size, read_indexed_file_text, split_lines,
    to_request_path,
};
use super::trails::MAX_TRAIL_HOPS;
use super::wire::{
    UNCERTAIN_BELOW, WireList, WireNodeRef, edge_confidence, to_node_ref, to_wire_edge, wire_list,
};
use crate::highlight::{HighlightResult, highlight_lines};
use crate::respond::{ApiError, ApiResult, Query};

/// Lines shown either side of the call site on a card.
pub const SOURCE_WINDOW: i64 = 3;
/// How far above a window the highlighter may start reading, so a window that
/// opens inside a block comment still tokenises as one.
const HIGHLIGHT_LEAD_MAX: i64 = 200;
/// Distinct paths returned.
pub const MAX_FLOWS: i64 = 4;
const MAX_CONTINUATIONS: usize = 6;
const MAX_MISSED: usize = 4;
const MAX_SITES_PER_FLOW: usize = 3;
/// The biggest file a card's window is read from.
const MAX_WINDOW_FILE_BYTES: u64 = 8 * 1024 * 1024;

const DRIFT_REASON: &str = "This file changed on disk after the last index sync, so the recorded call line no \
     longer reliably points at this call. The window returns after the next sync.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopDir {
    Start,
    Down,
    Up,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowQuery {
    Directed { from: String, to: String },
    Symbols { text: String },
    Trail { hops: Vec<(String, HopDir)> },
}

/// Read the question out of the query string. A trail hop is its own `hop`
/// parameter (a node id can contain a comma); the one-character direction
/// prefix is the frontend trail codec's.
pub fn parse_flow_query(query: &Query) -> ApiResult<FlowQuery> {
    let raw_hops: Vec<&str> = query
        .get_all("hop")
        .into_iter()
        .filter(|h| h.encode_utf16().count() > 1)
        .collect();
    if !raw_hops.is_empty() {
        if raw_hops.len() > MAX_TRAIL_HOPS {
            return Err(ApiError::bad_request(format!(
                "A trail of {} hops is longer than this endpoint reads ({MAX_TRAIL_HOPS}).",
                raw_hops.len()
            )));
        }
        let hops: Vec<(String, HopDir)> = raw_hops
            .iter()
            .map(|raw| {
                let mut chars = raw.chars();
                let dir = match chars.next() {
                    Some('s') => HopDir::Start,
                    Some('u') => HopDir::Up,
                    _ => HopDir::Down,
                };
                (chars.as_str().to_string(), dir)
            })
            .collect();
        if hops.len() < 2 {
            return Err(ApiError::bad_request(
                "A trail needs at least two hops to be read as a flow.",
            ));
        }
        return Ok(FlowQuery::Trail { hops });
    }
    let from = query.get("from").unwrap_or("").trim().to_string();
    let to = query.get("to").unwrap_or("").trim().to_string();
    if !from.is_empty() && !to.is_empty() {
        if normalize_token(&from) == normalize_token(&to) {
            return Err(ApiError::bad_request(
                "\"from\" and \"to\" name the same symbol, so there is no path to draw.",
            ));
        }
        return Ok(FlowQuery::Directed { from, to });
    }
    let symbols = query.get("symbols").unwrap_or("").trim().to_string();
    if !symbols.is_empty() {
        return Ok(FlowQuery::Symbols { text: symbols });
    }
    Err(ApiError::bad_request("No flow was asked for.").with_hint(
        "Use /api/flow?from=<symbol>&to=<symbol>, ?symbols=a,b,c, or ?hop=s<id>&hop=d<id>.",
    ))
}

/// The sentence under a link. A synthesized hop never reads as a plain
/// `calls`: it is a bridge the resolver inferred, and the wiring site is its
/// evidence — "via callback · registered at file:line".
pub fn flow_edge_label(edge: &Edge, upward: bool) -> String {
    let meta = edge.metadata.as_ref();
    let text = |key: &str| meta.and_then(|m| m.get(key)).and_then(Value::as_str);
    let mut parts: Vec<String> = Vec::new();
    match text("synthesizedBy") {
        Some(mechanism) if edge.provenance.as_deref() == Some("heuristic") => {
            parts.push(format!("via {}", mechanism.replace('-', " ")));
            if let Some(via) = text("via").filter(|v| !v.is_empty()) {
                parts.push(via.to_string());
            }
            if let Some(at) = text("registeredAt").filter(|v| !v.is_empty()) {
                parts.push(format!("registered at {at}"));
            }
        }
        _ => parts.push(if upward {
            "called by".to_string()
        } else {
            edge.kind.as_str().to_string()
        }),
    }
    parts.join(" · ")
}

fn to_flow_edge(edge: &Edge, upward: bool) -> Value {
    let mut wire = serde_json::to_value(to_wire_edge(edge)).unwrap_or_else(|_| json!({}));
    wire["label"] = json!(flow_edge_label(edge, upward));
    wire["upward"] = json!(upward);
    wire["uncertain"] = json!(edge_confidence(edge).is_some_and(|c| c < UNCERTAIN_BELOW));
    wire["synthesized"] = json!(edge.provenance.as_deref() == Some("heuristic"));
    wire
}

/// One read and one drift probe per file, however many cards land in it.
struct FileEntry {
    lines: Option<Vec<String>>,
    language: codegraph_core::types::Language,
    content_hash: String,
    drift: bool,
    reason: Option<&'static str>,
}

fn load_file<'c>(
    ctx: &Ctx<'_>,
    cache: &'c mut HashMap<String, Option<FileEntry>>,
    file_path: &str,
) -> Option<&'c FileEntry> {
    let posix = to_request_path(file_path);
    if !cache.contains_key(&posix) {
        let entry = match find_indexed_file(ctx, &posix) {
            Ok(Some((record, stored))) => Some(
                if has_drifted_on_disk(ctx, &stored, &record, max_file_size(ctx)) {
                    FileEntry {
                        lines: None,
                        language: record.language,
                        content_hash: record.content_hash.clone(),
                        drift: true,
                        reason: Some(DRIFT_REASON),
                    }
                } else {
                    match read_indexed_file_text(ctx, &stored, MAX_WINDOW_FILE_BYTES) {
                        Some(text) => FileEntry {
                            lines: Some(split_lines(&text)),
                            language: record.language,
                            content_hash: record.content_hash.clone(),
                            drift: false,
                            reason: None,
                        },
                        None => FileEntry {
                            lines: None,
                            language: record.language,
                            content_hash: record.content_hash.clone(),
                            drift: false,
                            reason: Some("This file is in the index but could not be read."),
                        },
                    }
                },
            ),
            _ => None,
        };
        cache.insert(posix.clone(), entry);
    }
    cache.get(&posix).and_then(Option::as_ref)
}

/// The ±[`SOURCE_WINDOW`] lines a card shows, anchored on the line that makes
/// the next call — or, on the last card, the definition.
fn window_for(
    ctx: &Ctx<'_>,
    cache: &mut HashMap<String, Option<FileEntry>>,
    node: &Node,
    anchor: i64,
) -> Value {
    let posix = to_request_path(&node.file_path);
    let Some(file) = load_file(ctx, cache, &node.file_path) else {
        return Value::Null;
    };
    let language = file.language.as_str();
    let Some(lines) = &file.lines else {
        let mut out = json!({ "file": posix, "language": language, "from": anchor, "to": anchor, "drift": file.drift });
        if let Some(reason) = file.reason {
            out["reason"] = json!(reason);
        }
        return out;
    };
    let total = lines.len() as i64;
    let from = (anchor - SOURCE_WINDOW).min(total).max(1);
    let to = (anchor + SOURCE_WINDOW).min(total).max(from);
    // Tokenise with the lead-in, then keep only the window.
    let lead_from = from
        .min(node.start_line.max(from - HIGHLIGHT_LEAD_MAX))
        .max(1);
    let slice = |a: i64, b: i64| -> Vec<String> {
        let start = ((a - 1).max(0) as usize).min(lines.len());
        let end = (b.max(0) as usize).min(lines.len()).max(start);
        lines[start..end].to_vec()
    };
    let key = format!("{}:{posix}:{lead_from}:{to}", file.content_hash);
    let mut highlighted: HighlightResult = highlight_lines(
        &ctx.state.caches.highlight,
        &slice(lead_from, to),
        Some(file.language),
        Some(&key),
    );
    let skip = ((from - lead_from).max(0) as usize).min(highlighted.lines.len());
    highlighted.lines.drain(..skip);
    json!({
        "file": posix,
        "language": language,
        "from": from,
        "to": to,
        "lines": slice(from, to),
        "highlight": highlighted,
        "drift": false,
    })
}

/// The steps of one chain, plus the edge that brought the reader into each.
struct RawHop {
    node: Node,
    edge: Option<Edge>,
    upward: bool,
    /// Open the card here instead (a boundary-only strip's dispatch line).
    anchor: Option<i64>,
}

fn call_ref(line: i64, edge: &Edge, other: &Node, backwards: bool) -> Value {
    json!({
        "line": line,
        "col": edge.col,
        "name": other.name,
        "targetId": other.id,
        "backwards": backwards,
    })
}

fn to_wire_flow(
    ctx: &Ctx<'_>,
    cache: &mut HashMap<String, Option<FileEntry>>,
    raw: &[RawHop],
    boundary: Option<Value>,
    partial: bool,
) -> Value {
    let mut hops: Vec<Value> = Vec::new();
    for (i, step) in raw.iter().enumerate() {
        let previous = i.checked_sub(1).and_then(|p| raw.get(p));
        let next = raw.get(i + 1);
        // Forward: the edge into the NEXT hop was recorded at the line inside
        // THIS body that makes the call. Backwards (a trail read up from a
        // callee): this card IS the caller, and its own incoming edge carries
        // the line where it calls the card before it.
        let mut anchor_line: Option<i64> = None;
        let mut reference = Value::Null;
        if let Some(next) = next
            && !next.upward
            && let Some(edge) = &next.edge
            && let Some(line) = edge.line.filter(|l| *l != 0)
        {
            reference = call_ref(line, edge, &next.node, false);
            anchor_line = Some(line);
        } else if step.upward
            && let Some(previous) = previous
            && let Some(edge) = &step.edge
            && let Some(line) = edge.line.filter(|l| *l != 0)
        {
            reference = call_ref(line, edge, &previous.node, true);
            anchor_line = Some(line);
        }
        let edge = step
            .edge
            .as_ref()
            .map(|e| to_flow_edge(e, step.upward))
            .unwrap_or(Value::Null);
        let anchor = step.anchor.or(anchor_line).unwrap_or(step.node.start_line);
        hops.push(json!({
            "node": to_node_ref(&step.node),
            "edge": edge,
            "callRef": reference,
            "source": window_for(ctx, cache, &step.node, anchor),
        }));
    }
    let first = raw.first().map(|h| h.node.name.as_str()).unwrap_or("?");
    let last = raw.last().map(|h| h.node.name.as_str()).unwrap_or("?");
    json!({
        "id": raw.iter().map(|h| h.node.id.as_str()).collect::<Vec<_>>().join(">"),
        "label": if partial { format!("{first} → stops here") } else { format!("{first} → {last}") },
        "hops": hops,
        "boundary": boundary.unwrap_or(Value::Null),
        "partial": partial,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireContinuation {
    node: WireNodeRef,
    line: Option<i64>,
    confidence: Option<f64>,
}

fn to_continuations(list: &[BoundaryContinuation]) -> WireList<WireContinuation> {
    wire_list(
        list.iter()
            .take(MAX_CONTINUATIONS)
            .map(|c| WireContinuation {
                node: to_node_ref(&c.node),
                line: c.line,
                confidence: c.confidence,
            })
            .collect(),
        list.len(),
    )
}

/// The end cap for a path that stopped short: the dispatch sites the shared
/// scanner found, the calls the stopping symbol makes that this path did not
/// need, and the name-only guesses the search refused to follow.
fn build_boundary(
    ctx: &Ctx<'_>,
    stop: &Node,
    reports: &[NodeBoundary],
    missed: &[Node],
    on_path: &HashSet<String>,
) -> Value {
    let mut sites: Vec<Value> = Vec::new();
    'reports: for report in reports {
        for site in &report.sites {
            if sites.len() >= MAX_SITES_PER_FLOW {
                break 'reports;
            }
            let m = &site.matched;
            sites.push(json!({
                "form": m.form,
                "label": m.label,
                "snippet": m.snippet,
                "line": m.line,
                "key": m.key,
                "keyIsType": m.key_is_type,
                "moreSites": m.more_sites,
                "candidates": site.candidates.iter().map(|c| json!({
                    "node": to_node_ref(&c.node),
                    "display": c.display,
                    "named": c.named,
                })).collect::<Vec<_>>(),
                "candidateNote": site.candidate_note,
            }));
        }
    }
    let continuations = continuations_from(ctx.store, stop, on_path).unwrap_or_default();
    json!({
        "node": to_node_ref(stop),
        "sites": sites,
        "uncertain": to_continuations(&continuations.uncertain),
        "further": to_continuations(&continuations.resolved),
        "missed": missed.iter().take(MAX_MISSED).map(to_node_ref).collect::<Vec<_>>(),
    })
}

/// The edge that already connects two symbols the reader walked between: a
/// call out of `from` (down), or the same edge read backwards (up). A `calls`
/// edge is preferred over any other kind.
fn edge_between(traverser: &GraphTraverser<'_>, from: &Node, to: &Node) -> Option<(Edge, bool)> {
    let pick = |entries: Vec<codegraph_graph::graph::NodeEdge>| -> Option<Edge> {
        let mut best: Option<Edge> = None;
        for entry in entries {
            if entry.node.id != to.id {
                continue;
            }
            let better = match &best {
                None => true,
                Some(current) => {
                    entry.edge.kind == EdgeKind::Calls && current.kind != EdgeKind::Calls
                }
            };
            if better {
                best = Some(entry.edge);
            }
        }
        best
    };
    if let Some(edge) = traverser.get_callees(&from.id, 1).ok().and_then(pick) {
        return Some((edge, false));
    }
    traverser
        .get_callers(&from.id, 1)
        .ok()
        .and_then(pick)
        .map(|edge| (edge, true))
}

fn ambiguities_of(flow: &NamedSymbolFlow, chosen: &HashSet<String>) -> Vec<Value> {
    let mut out = Vec::new();
    for token in &flow.tokens {
        let ids = flow.token_nodes.get(token).cloned().unwrap_or_default();
        if ids.len() < 2 {
            continue;
        }
        let picked = ids.iter().find(|id| chosen.contains(*id)).cloned();
        out.push(json!({
            "token": token,
            "chosen": picked.as_ref().and_then(|id| flow.named.get(id)).map(to_node_ref),
            "others": ids
                .iter()
                .filter(|id| Some(*id) != picked.as_ref())
                .filter_map(|id| flow.named.get(id))
                .map(to_node_ref)
                .collect::<Vec<_>>(),
        }));
    }
    out
}

/// The named symbols a path never reaches, deduped by name, the reader's own
/// (near-)unique names first. Per TOKEN: a token with one overload on the path
/// is answered.
fn uncovered_named(flow: &NamedSymbolFlow, on_path: &HashSet<String>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    let mut seen_name: HashSet<String> = HashSet::new();
    for ids in flow.token_nodes.values() {
        if ids.is_empty() || ids.iter().any(|id| on_path.contains(id)) {
            continue;
        }
        for id in ids {
            let Some(node) = flow.named.get(id) else {
                continue;
            };
            if seen_name.insert(node.name.clone()) {
                out.push(node.clone());
            }
        }
    }
    out.sort_by_key(|n| !flow.unique_named_node_ids.contains(&n.id));
    out
}

/// Bodies to scan when nothing connected: `from`'s, then `to`'s; a symbols
/// question scans what it named.
fn boundary_seeds(flow: &NamedSymbolFlow, ends: Option<(&str, &str)>) -> Vec<Node> {
    let Some((from, to)) = ends else {
        return flow.named.values().cloned().collect();
    };
    let pick = |token: &str| -> Vec<Node> {
        flow.token_nodes
            .get(&normalize_token(token))
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| flow.named.get(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut seeds = pick(from);
    seeds.extend(pick(to));
    seeds
}

fn no_flow_reason(parsed: &FlowQuery, token_count: usize, unresolved: &[String]) -> String {
    if !unresolved.is_empty() {
        let verb = if unresolved.len() > 1 {
            "name"
        } else {
            "names"
        };
        return format!("{} {verb} nothing in this index.", unresolved.join(" and "));
    }
    if let FlowQuery::Directed { from, to } = parsed {
        return format!(
            "No chain of calls reaches {to} from {from} within {DIRECTED_MAX_HOPS} hops. The path may run through \
             a dynamic dispatch — a callback, a registry, a reflective call — that no static edge records."
        );
    }
    if token_count < 2 {
        return "Name at least two symbols: a flow is a path between them.".to_string();
    }
    "Those symbols do not call one another, directly or through one intermediate.".to_string()
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let started = Instant::now();
    let parsed = parse_flow_query(query)?;
    let max_flows = query.int("limit", 1, MAX_FLOWS, Some(MAX_FLOWS))? as usize;
    let counts = ctx.store.counts()?;
    let index = json!({
        "lastIndexedAt": ctx.store.last_indexed_at()?,
        "edges": counts.edge_count,
        "files": counts.file_count,
    });
    let mut cache: HashMap<String, Option<FileEntry>> = HashMap::new();
    let traverser = GraphTraverser::new(ctx.store);

    if let FlowQuery::Trail { hops } = &parsed {
        let ids: Vec<String> = hops.iter().map(|(id, _)| id.clone()).collect();
        let by_id = ctx.store.nodes_by_ids(&ids)?;
        let mut raw: Vec<RawHop> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for (id, dir) in hops {
            let Some(node) = by_id.get(id) else {
                missing.push(id.clone());
                continue;
            };
            let link = raw
                .last()
                .and_then(|previous| edge_between(&traverser, &previous.node, node));
            let (edge, upward) = match link {
                Some((edge, upward)) => (Some(edge), upward),
                None => (None, *dir == HopDir::Up),
            };
            raw.push(RawHop {
                node: node.clone(),
                edge,
                upward,
                anchor: None,
            });
        }
        let flows: Vec<Value> = if raw.len() >= 2 {
            vec![to_wire_flow(ctx, &mut cache, &raw, None, false)]
        } else {
            Vec::new()
        };
        let reason = flows.is_empty().then_some(
            "None of the symbols on this trail are still in the index. Re-index, or start a new trail.",
        );
        return Ok(json!({
            "query": { "kind": "trail", "from": Value::Null, "to": Value::Null, "symbols": [] },
            "flows": flows,
            "ambiguous": [],
            "unresolved": missing,
            "reason": reason,
            "index": index,
            "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
        }));
    }

    let (kind, text, options, ends) = match &parsed {
        FlowQuery::Directed { from, to } => (
            "directed",
            format!("{from} {to}"),
            NamedSymbolFlowOptions {
                mode: FlowMode::Directed,
                from: Some(from.clone()),
                to: Some(to.clone()),
                max_chains: Some(max_flows),
                ..NamedSymbolFlowOptions::default()
            },
            Some((from.as_str(), to.as_str())),
        ),
        FlowQuery::Symbols { text } => (
            "symbols",
            text.clone(),
            NamedSymbolFlowOptions {
                mode: FlowMode::Named,
                max_chains: Some(max_flows),
                ..NamedSymbolFlowOptions::default()
            },
            None,
        ),
        FlowQuery::Trail { .. } => unreachable!("trails returned above"),
    };
    let flow = resolve_named_symbol_flow(ctx.store, &text, &options);
    let unresolved: Vec<String> = flow
        .tokens
        .iter()
        .filter(|t| flow.token_nodes.get(t).is_none_or(Vec::is_empty))
        .cloned()
        .collect();
    let chosen: HashSet<String> = flow
        .chains
        .iter()
        .flat_map(|c| c.steps.iter().map(|s| s.node.id.clone()))
        .collect();

    let mut flows: Vec<Value> = Vec::new();
    for chain in &flow.chains {
        let on_path: HashSet<String> = chain.steps.iter().map(|s| s.node.id.clone()).collect();
        // A path that reaches everything the question named is connected and
        // gets no cap; in directed mode a chain ends at `to`, so never.
        let missed = uncovered_named(&flow, &on_path);
        let mut boundary: Option<Value> = None;
        let mut stop_line: Option<i64> = None;
        if !missed.is_empty()
            && let Some(stop) = chain.steps.last().map(|s| s.node.clone())
        {
            // Explore's scan order: the dead end first, then what it never reached.
            let mut scan = vec![stop.clone()];
            scan.extend(missed.iter().cloned());
            let reports = find_dynamic_boundaries(ctx, &scan, &flow.named, MAX_SITES_PER_FLOW);
            stop_line = reports
                .iter()
                .flat_map(|r| r.sites.iter())
                .next()
                .map(|s| s.matched.line);
            boundary = Some(build_boundary(ctx, &stop, &reports, &missed, &on_path));
        }
        // The last card opens at the dispatch line, so the window shows the site
        // the cap beside it describes.
        let last = chain.steps.len().saturating_sub(1);
        let raw: Vec<RawHop> = chain
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| RawHop {
                node: s.node.clone(),
                edge: s.edge.clone(),
                upward: false,
                anchor: if i == last { stop_line } else { None },
            })
            .collect();
        flows.push(to_wire_flow(ctx, &mut cache, &raw, boundary, false));
    }

    // No path at all. If a dispatch site explains why, the strip is that site:
    // one card at the line where the static path ends, and the cap. With
    // nothing detected, no stopping point is invented.
    if flows.is_empty() && !flow.named.is_empty() {
        let seeds = boundary_seeds(&flow, ends);
        let reports = find_dynamic_boundaries(ctx, &seeds, &flow.named, MAX_SITES_PER_FLOW);
        if let Some(first) = reports.first()
            && let Some(site) = first.sites.first()
        {
            let stop = first.node.clone();
            let on_stop: HashSet<String> = [stop.id.clone()].into_iter().collect();
            let missed = uncovered_named(&flow, &on_stop);
            let boundary = build_boundary(ctx, &stop, &reports, &missed, &on_stop);
            let raw = [RawHop {
                node: stop,
                edge: None,
                upward: false,
                anchor: Some(site.matched.line),
            }];
            flows.push(to_wire_flow(ctx, &mut cache, &raw, Some(boundary), true));
        }
    }

    // A boundary strip is not a path, so the reason still stands.
    let reason = flow
        .chains
        .is_empty()
        .then(|| no_flow_reason(&parsed, flow.tokens.len(), &unresolved));
    let (from, to) = match &parsed {
        FlowQuery::Directed { from, to } => (json!(from), json!(to)),
        _ => (Value::Null, Value::Null),
    };
    Ok(json!({
        "query": { "kind": kind, "from": from, "to": to, "symbols": flow.tokens },
        "flows": flows,
        "ambiguous": ambiguities_of(&flow, &chosen),
        "unresolved": unresolved,
        "reason": reason,
        "index": index,
        "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> ApiResult<FlowQuery> {
        parse_flow_query(&Query::parse(Some(raw)))
    }

    #[test]
    fn the_three_questions_parse() {
        assert_eq!(
            parse("from=main&to=open_store").unwrap(),
            FlowQuery::Directed {
                from: "main".into(),
                to: "open_store".into()
            }
        );
        assert_eq!(
            parse("symbols=a,b").unwrap(),
            FlowQuery::Symbols { text: "a,b".into() }
        );
        assert_eq!(
            parse("hop=sfn:1&hop=dfn:2&hop=ufn:3&hop=x").unwrap(),
            FlowQuery::Trail {
                hops: vec![
                    ("fn:1".into(), HopDir::Start),
                    ("fn:2".into(), HopDir::Down),
                    ("fn:3".into(), HopDir::Up)
                ]
            }
        );
    }

    #[test]
    fn bad_questions_are_400s_with_upstreams_words() {
        let same = parse("from=main.rs&to=main").unwrap_err();
        assert!(same.message.contains("name the same symbol"));
        let short = parse("hop=sfn:1").unwrap_err();
        assert!(short.message.contains("at least two hops"));
        let none = parse("").unwrap_err();
        assert_eq!(none.message, "No flow was asked for.");
        let long: Vec<String> = (0..=MAX_TRAIL_HOPS)
            .map(|i| format!("hop=dfn:{i}"))
            .collect();
        assert!(
            parse(&long.join("&"))
                .unwrap_err()
                .message
                .contains("longer than this endpoint reads")
        );
    }

    fn edge(metadata: Value, provenance: Option<&str>) -> Edge {
        Edge {
            id: None,
            source: "a".into(),
            target: "b".into(),
            kind: EdgeKind::Calls,
            metadata: Some(metadata),
            line: Some(3),
            col: Some(4),
            provenance: provenance.map(str::to_string),
        }
    }

    #[test]
    fn names_the_mechanism_and_the_wiring_site_for_a_synthesized_hop() {
        let synthesized = edge(
            json!({ "synthesizedBy": "callback", "registeredAt": "src/a.ts:12" }),
            Some("heuristic"),
        );
        assert_eq!(
            flow_edge_label(&synthesized, false),
            "via callback · registered at src/a.ts:12"
        );
        let wire = to_flow_edge(&synthesized, false);
        assert_eq!(wire["synthesized"], true);
        assert_eq!(wire["upward"], false);
    }

    #[test]
    fn never_lets_a_synthesized_hop_read_as_a_plain_call() {
        let hop = edge(
            json!({ "synthesizedBy": "react-render", "via": "props" }),
            Some("heuristic"),
        );
        assert_eq!(flow_edge_label(&hop, false), "via react render · props");
    }

    #[test]
    fn says_called_by_when_the_reader_walked_the_edge_backwards() {
        let plain = edge(json!({ "confidence": 0.4 }), Some("resolved"));
        assert_eq!(flow_edge_label(&plain, true), "called by");
        assert_eq!(flow_edge_label(&plain, false), "calls");
        // A name-only guess is drawn dashed.
        assert_eq!(to_flow_edge(&plain, false)["uncertain"], true);
    }

    #[test]
    fn takes_a_trail_over_a_from_to_pair_and_refuses_a_one_hop_trail() {
        assert!(matches!(
            parse("from=a&to=b&hop=sx&hop=dy").unwrap(),
            FlowQuery::Trail { .. }
        ));
        assert!(
            parse("hop=sx")
                .unwrap_err()
                .message
                .contains("at least two hops")
        );
        assert!(
            parse("from=run&to=run")
                .unwrap_err()
                .message
                .contains("same symbol")
        );
    }

    #[test]
    fn reasons_say_what_to_do_next() {
        let symbols = FlowQuery::Symbols { text: "a".into() };
        assert_eq!(
            no_flow_reason(&symbols, 1, &[]),
            "Name at least two symbols: a flow is a path between them."
        );
        assert_eq!(
            no_flow_reason(&symbols, 2, &["x".into(), "y".into()]),
            "x and y name nothing in this index."
        );
        assert_eq!(
            no_flow_reason(&symbols, 2, &["x".into()]),
            "x names nothing in this index."
        );
        let directed = FlowQuery::Directed {
            from: "a".into(),
            to: "b".into(),
        };
        assert!(
            no_flow_reason(&directed, 2, &[])
                .starts_with("No chain of calls reaches b from a within 12 hops.")
        );
    }
}
