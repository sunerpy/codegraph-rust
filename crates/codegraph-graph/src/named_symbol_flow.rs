//! The call path among a bag of named symbols — a port of upstream
//! `src/graph/named-symbol-flow.ts` (`resolveNamedSymbolFlow`, `v1.6.1`).
//!
//! Two modes, expressed as options rather than two implementations:
//!
//! - **`Named`** is explore's rule: every resolved symbol is both a possible
//!   start and a possible end, at most ONE unnamed symbol may bridge two named
//!   ones, and the LONGEST chain wins. The bridge cap is what keeps the search
//!   off a god-function's fan-out — the naming is the evidence a hop is on topic.
//! - **`Directed`** is "how does X reach Y": both ends are pinned, so the search
//!   bridges freely, walks from both ends at once, and the SHORTEST path wins.
//!
//! Overloads differ for the same reason: a bare ambiguous name in `Named` mode
//! is kept only where the query also names its container; in `Directed` mode
//! every candidate for both ends is tried and the pair that connects is the
//! answer. Explore's own Flow section (family F10) is not wired to this yet;
//! the viewer's `/api/flow` is its first caller.

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use codegraph_store::Store;

use crate::graph::{GraphTraverser, NodeEdge};
use crate::query::scoring::is_test_file;
use crate::symbol_lookup::find_all_symbol_nodes;

/// Chain length ceiling, in NODES. Explore's Flow section has always used 7.
pub const DEFAULT_MAX_HOPS: usize = 7;
/// Longer ceiling for a directed question: a real path from an entry point to
/// a storage primitive runs deeper than seven frames.
pub const DIRECTED_MAX_HOPS: usize = 12;
/// At most one consecutive UNNAMED hop may bridge two named symbols.
pub const DEFAULT_MAX_BRIDGE: usize = 1;

const MAX_SEEDS: usize = 8;
const MAX_CANDIDATES_PER_TOKEN: usize = 6;
/// Candidates a DIRECTED endpoint keeps: `main` can have ten definitions, and
/// the one meant may sort seventh.
const MAX_CANDIDATES_DIRECTED: usize = 12;
const MAX_TOKENS: usize = 16;
const MAX_NAMED: usize = 40;
const MAX_DYN_NAMED: usize = 12;
const MAX_DYN_PER_TOKEN: usize = 4;
const NAMED_VISIT_CAP: usize = 1500;
const DIRECTED_VISIT_CAP: usize = 12_000;

/// Node kinds that can sit on a call chain (upstream's `constructor` has no
/// Rust kind: constructors are `method` nodes).
pub fn is_flow_callable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component | NodeKind::Route
    )
}

/// Edge kinds a flow may ride (`navigates` has no Rust edge kind yet).
fn is_flow_edge(kind: EdgeKind) -> bool {
    kind == EdgeKind::Calls
}

/// Non-callable kinds a SYNTHESIZED edge may end at (an RTK thunk is a constant).
fn is_dyn_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Constant | NodeKind::Variable | NodeKind::Field | NodeKind::Property
    )
}

const FILE_EXTENSIONS: &[&str] = &[
    "java", "kt", "kts", "ts", "tsx", "js", "jsx", "mjs", "cjs", "cs", "py", "go", "rb", "php",
    "swift", "rs", "cpp", "cc", "cxx", "c", "h", "hpp", "scala", "lua", "dart", "vue", "svelte",
    "astro", "erl", "hrl",
];

/// Strip ONE real file extension (case-insensitive) — `Class.method` is kept.
fn strip_file_extension(token: &str) -> &str {
    if let Some((stem, ext)) = token.rsplit_once('.')
        && FILE_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
    {
        return stem;
    }
    token
}

/// The token spelling [`flow_tokens`] would have produced for one word.
pub fn normalize_token(token: &str) -> String {
    strip_file_extension(token).trim().to_string()
}

/// `^[A-Za-z_$][\w$]*(?:(?:::|\.)[\w$]+)*$`, ASCII as in JavaScript.
fn is_symbol_shaped(token: &str) -> bool {
    let b = token.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let Some(&first) = b.first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return false;
    }
    let mut i = 1;
    while i < b.len() && word(b[i]) {
        i += 1;
    }
    while i < b.len() {
        if b[i] == b'.' {
            i += 1;
        } else if b[i] == b':' && b.get(i + 1) == Some(&b':') {
            i += 2;
        } else {
            return false;
        }
        let start = i;
        while i < b.len() && word(b[i]) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    true
}

/// The symbol-shaped tokens of a query, deduped and capped.
pub fn flow_tokens(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in query.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '[' | ']'))
    {
        let token = strip_file_extension(raw).trim();
        if token.len() >= 3 && is_symbol_shaped(token) && !out.iter().any(|t| t == token) {
            out.push(token.to_string());
        }
    }
    out.truncate(MAX_TOKENS);
    out
}

/// A token is shape-precise when it looks like a symbol reference rather than
/// an English word that happened to exact-match a callable.
fn is_precise_token(token: &str) -> bool {
    if token.contains(['.', '_', '$', '/']) || token.contains("::") {
        return true;
    }
    let chars: Vec<char> = token.chars().collect();
    chars
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
        || chars.first().is_some_and(|c| c.is_ascii_uppercase())
}

/// An insertion-ordered map, as JavaScript's `Map` iterates.
#[derive(Debug, Clone)]
pub struct OrderedMap<V> {
    keys: Vec<String>,
    values: HashMap<String, V>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            values: HashMap::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    /// `Map.set`: a new key goes last; an existing key keeps its place.
    pub fn set(&mut self, key: String, value: V) {
        if !self.values.contains_key(&key) {
            self.keys.push(key.clone());
        }
        self.values.insert(key, value);
    }
    pub fn get(&self, key: &str) -> Option<&V> {
        self.values.get(key)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.keys.iter()
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.keys.iter().filter_map(|k| self.values.get(k))
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.keys
            .iter()
            .filter_map(|k| self.values.get(k).map(|v| (k, v)))
    }
}

#[derive(Debug, Clone)]
pub struct FlowStep {
    pub node: Node,
    /// The edge INTO this node from the previous step; `None` on the first.
    pub edge: Option<Edge>,
}

#[derive(Debug, Clone)]
pub struct FlowChain {
    pub steps: Vec<FlowStep>,
    /// For each node on the chain, the line where it calls the NEXT one.
    pub call_sites: HashMap<String, i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlowMode {
    #[default]
    Named,
    Directed,
}

#[derive(Debug, Clone, Default)]
pub struct NamedSymbolFlowOptions {
    pub mode: FlowMode,
    /// Required in directed mode: the token the path must start at.
    pub from: Option<String>,
    /// Required in directed mode: the token the path must end at.
    pub to: Option<String>,
    pub max_hops: Option<usize>,
    /// Consecutive unnamed hops allowed; unbounded in directed mode.
    pub max_bridge: Option<usize>,
    /// Distinct chains to return.
    pub max_chains: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct NamedSymbolFlow {
    /// The query's symbol tokens, in the order they were written.
    pub tokens: Vec<String>,
    /// Every CALLABLE the tokens resolved to, by node id.
    pub named: OrderedMap<Node>,
    /// Non-callable endpoints of synthesized edges (RTK thunks and friends).
    pub dyn_named: OrderedMap<Node>,
    /// token → the node ids it resolved to.
    pub token_nodes: OrderedMap<Vec<String>>,
    /// token → its whole same-name callable family, before the container filter.
    pub token_family: OrderedMap<Vec<Node>>,
    /// Ids whose token was a (near-)unique callable name — at most 3 defs.
    pub unique_named_node_ids: HashSet<String>,
    /// Ids resolved from a shape-precise token.
    pub precise_named_ids: HashSet<String>,
    /// Chains found, best first. Empty when nothing connects.
    pub chains: Vec<FlowChain>,
}

/// Production code before test and fixture code, otherwise the index's order.
fn rank_for_directed(nodes: Vec<Node>) -> Vec<Node> {
    let mut ranked = nodes;
    ranked.sort_by_key(|n| is_test_file(&n.file_path));
    ranked
}

fn has_heuristic_edge(store: &Store, id: &str) -> rusqlite::Result<bool> {
    let heuristic = |e: &Edge| e.provenance.as_deref() == Some("heuristic");
    Ok(store.edges_by_target_kind(id, None)?.iter().any(heuristic)
        || store.edges_by_source_kind(id, None)?.iter().any(heuristic))
}

fn container_segment(qualified_name: &str) -> String {
    let lower = qualified_name.to_lowercase();
    let segs: Vec<&str> = lower
        .split("::")
        .flat_map(|s| s.split('.'))
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() >= 2 {
        segs[segs.len() - 2].to_string()
    } else {
        String::new()
    }
}

/// Resolve a query's tokens to nodes, with the overload rules in the module
/// header. No graph traversal happens here.
pub fn resolve_named_tokens(
    store: &Store,
    query: &str,
    opts: &NamedSymbolFlowOptions,
) -> rusqlite::Result<NamedSymbolFlow> {
    let directed = opts.mode == FlowMode::Directed;
    let mut out = NamedSymbolFlow {
        tokens: flow_tokens(query),
        ..NamedSymbolFlow::default()
    };
    if out.tokens.len() < 2 {
        return Ok(out);
    }
    // Name SEGMENTS of every token: an ambiguous simple name is kept only
    // where its container is itself named.
    let mut seg_pool: HashSet<String> = HashSet::new();
    for token in &out.tokens {
        for seg in token.to_lowercase().split("::").flat_map(|s| s.split('.')) {
            if !seg.is_empty() {
                seg_pool.insert(seg.to_string());
            }
        }
    }

    let tokens = out.tokens.clone();
    for token in &tokens {
        let hits = find_all_symbol_nodes(store, token)?;
        let cands: Vec<Node> = hits
            .iter()
            .filter(|n| is_flow_callable(n.kind))
            .cloned()
            .collect();
        out.token_family.set(token.clone(), cands.clone());
        let specific = cands.len() <= 3;
        let pick: Vec<Node> = if specific || directed {
            cands
        } else {
            cands
                .into_iter()
                .filter(|n| {
                    let container = container_segment(&n.qualified_name);
                    !container.is_empty() && seg_pool.contains(&container)
                })
                .collect()
        };
        let kept: Vec<Node> = if directed {
            rank_for_directed(pick)
                .into_iter()
                .take(MAX_CANDIDATES_DIRECTED)
                .collect()
        } else {
            pick.into_iter().take(MAX_CANDIDATES_PER_TOKEN).collect()
        };
        out.token_nodes
            .set(token.clone(), kept.iter().map(|n| n.id.clone()).collect());
        let precise = is_precise_token(token);
        for node in kept {
            if specific {
                out.unique_named_node_ids.insert(node.id.clone());
            }
            if precise {
                out.precise_named_ids.insert(node.id.clone());
            }
            out.named.set(node.id.clone(), node);
        }
        // Same token, non-callable synthesized endpoints — capped per token and
        // gated on an actual heuristic edge so plain constants never qualify.
        if out.dyn_named.len() < MAX_DYN_NAMED {
            let mut token_dyn = 0;
            for node in &hits {
                if is_flow_callable(node.kind)
                    || !is_dyn_kind(node.kind)
                    || out.dyn_named.contains_key(&node.id)
                {
                    continue;
                }
                if has_heuristic_edge(store, &node.id)? {
                    if precise {
                        out.precise_named_ids.insert(node.id.clone());
                    }
                    out.dyn_named.set(node.id.clone(), node.clone());
                    token_dyn += 1;
                }
                if out.dyn_named.len() >= MAX_DYN_NAMED || token_dyn >= MAX_DYN_PER_TOKEN {
                    break;
                }
            }
        }
        if out.named.len() > MAX_NAMED {
            break;
        }
    }
    Ok(out)
}

fn call_sites_of(steps: &[FlowStep]) -> HashMap<String, i64> {
    let mut sites = HashMap::new();
    for pair in steps.windows(2) {
        if let Some(line) = pair[1].edge.as_ref().and_then(|e| e.line)
            && line > 0
        {
            sites.entry(pair[0].node.id.clone()).or_insert(line);
        }
    }
    sites
}

fn callees(traverser: &GraphTraverser<'_>, id: &str) -> rusqlite::Result<Vec<NodeEdge>> {
    traverser.get_callees(id, 1)
}

fn callers(traverser: &GraphTraverser<'_>, id: &str) -> rusqlite::Result<Vec<NodeEdge>> {
    traverser.get_callers(id, 1)
}

/// One BFS parent entry: where the walk came from, over which edge.
type Parent = HashMap<String, (Option<String>, Option<Edge>, Node)>;

/// Breadth-first over `calls` edges — synthesized ones included. Every named
/// symbol is a destination; at most `max_bridge` unnamed symbols between two.
fn walk_calls(
    traverser: &GraphTraverser<'_>,
    seed: &Node,
    named: &HashSet<String>,
    max_hops: usize,
    max_bridge: usize,
) -> rusqlite::Result<(Parent, Vec<String>)> {
    let mut parent: Parent = HashMap::new();
    parent.insert(seed.id.clone(), (None, None, seed.clone()));
    let mut queue: Vec<(String, usize, usize)> = vec![(seed.id.clone(), 0, 0)];
    let mut reached: Vec<String> = Vec::new();
    let mut head = 0;
    while head < queue.len() && parent.len() < NAMED_VISIT_CAP {
        let (id, depth, streak) = queue[head].clone();
        head += 1;
        if id != seed.id && named.contains(&id) {
            reached.push(id.clone());
        }
        if depth + 1 >= max_hops {
            continue;
        }
        for c in callees(traverser, &id)? {
            if !is_flow_edge(c.edge.kind) || parent.contains_key(&c.node.id) {
                continue;
            }
            // A route node is a connector: crossing one costs no bridge budget.
            let new_streak = if named.contains(&c.node.id) {
                0
            } else if c.node.kind == NodeKind::Route {
                streak
            } else {
                streak + 1
            };
            if new_streak > max_bridge {
                continue;
            }
            let next = c.node.id.clone();
            parent.insert(next.clone(), (Some(id.clone()), Some(c.edge), c.node));
            queue.push((next, depth + 1, new_streak));
        }
    }
    Ok((parent, reached))
}

fn chain_to(parent: &Parent, target: &str) -> Vec<FlowStep> {
    let mut steps = Vec::new();
    let mut cur = Some(target.to_string());
    while let Some(id) = cur {
        let Some((prev, edge, node)) = parent.get(&id) else {
            break;
        };
        steps.push(FlowStep {
            node: node.clone(),
            edge: edge.clone(),
        });
        cur = prev.clone();
    }
    steps.reverse();
    steps
}

/// A short call path from `seed` to any of `sinks`, searched from BOTH ends a
/// level at a time, always expanding the smaller frontier, stopping the moment
/// the sides share a node. Nothing claims the result is shortest — only that
/// the graph records it.
fn walk_bidirectional(
    store: &Store,
    traverser: &GraphTraverser<'_>,
    seed: &Node,
    sinks: &[String],
    max_hops: usize,
) -> rusqlite::Result<Option<Vec<FlowStep>>> {
    if sinks.contains(&seed.id) {
        return Ok(None);
    }
    // forward: id → (prev, edge into it, node), in insertion order.
    let mut forward_order: Vec<String> = vec![seed.id.clone()];
    let mut forward: Parent = HashMap::new();
    forward.insert(seed.id.clone(), (None, None, seed.clone()));
    // backward: id → (next towards the sink, the edge out of it); None AT a sink.
    let mut backward: HashMap<String, Option<(String, Edge)>> = HashMap::new();
    let mut back_nodes: HashMap<String, Node> = HashMap::new();
    let mut front_f: Vec<Node> = vec![seed.clone()];
    let mut front_b: Vec<Node> = Vec::new();
    for id in sinks {
        let Some(node) = store.node_by_id(id)? else {
            continue;
        };
        if backward.contains_key(id) {
            continue;
        }
        backward.insert(id.clone(), None);
        back_nodes.insert(id.clone(), node.clone());
        front_b.push(node);
    }
    if front_b.is_empty() {
        return Ok(None);
    }

    let max_edges = max_hops.saturating_sub(1).max(1);
    for _ in 0..max_edges {
        if front_f.len() <= front_b.len() {
            if forward.len() > DIRECTED_VISIT_CAP {
                break;
            }
            let mut next: Vec<Node> = Vec::new();
            for node in &front_f {
                for c in callees(traverser, &node.id)? {
                    if !is_flow_edge(c.edge.kind) || forward.contains_key(&c.node.id) {
                        continue;
                    }
                    forward_order.push(c.node.id.clone());
                    forward.insert(
                        c.node.id.clone(),
                        (Some(node.id.clone()), Some(c.edge), c.node.clone()),
                    );
                    next.push(c.node);
                }
            }
            if next.is_empty() {
                break;
            }
            front_f = next;
        } else {
            if backward.len() > DIRECTED_VISIT_CAP {
                break;
            }
            let mut next: Vec<Node> = Vec::new();
            for node in &front_b {
                for c in callers(traverser, &node.id)? {
                    if !is_flow_edge(c.edge.kind) || backward.contains_key(&c.node.id) {
                        continue;
                    }
                    backward.insert(c.node.id.clone(), Some((node.id.clone(), c.edge)));
                    back_nodes.insert(c.node.id.clone(), c.node.clone());
                    next.push(c.node);
                }
            }
            if next.is_empty() {
                break;
            }
            front_b = next;
        }

        let Some(meet) = forward_order
            .iter()
            .find(|id| backward.contains_key(*id))
            .cloned()
        else {
            continue;
        };
        // Forward half: seed → meet.
        let mut steps = chain_to(&forward, &meet);
        // Backward half: meet → sink. An entry holds the edge OUT of its node,
        // which is the edge INTO the step after it.
        let mut link = backward.get(&meet).cloned().flatten();
        while let Some((next_id, edge)) = link {
            let Some(node) = back_nodes.get(&next_id) else {
                break;
            };
            steps.push(FlowStep {
                node: node.clone(),
                edge: Some(edge),
            });
            link = backward.get(&next_id).cloned().flatten();
        }
        let reaches_sink = steps
            .last()
            .is_some_and(|last| sinks.contains(&last.node.id));
        if steps.len() < 2 || !reaches_sink {
            return Ok(None);
        }
        return Ok(if steps.len() <= max_hops {
            Some(steps)
        } else {
            None
        });
    }
    Ok(None)
}

/// The call path among a query's named symbols. Any lookup failure answers
/// with an empty flow, as upstream's does.
pub fn resolve_named_symbol_flow(
    store: &Store,
    query: &str,
    opts: &NamedSymbolFlowOptions,
) -> NamedSymbolFlow {
    resolve_flow(store, query, opts).unwrap_or_default()
}

fn resolve_flow(
    store: &Store,
    query: &str,
    opts: &NamedSymbolFlowOptions,
) -> rusqlite::Result<NamedSymbolFlow> {
    let directed = opts.mode == FlowMode::Directed;
    let mut flow = resolve_named_tokens(store, query, opts)?;
    if flow.named.len() < 2 {
        return Ok(flow);
    }
    let max_hops = opts.max_hops.unwrap_or(if directed {
        DIRECTED_MAX_HOPS
    } else {
        DEFAULT_MAX_HOPS
    });
    let max_bridge = opts.max_bridge.unwrap_or(if directed {
        usize::MAX
    } else {
        DEFAULT_MAX_BRIDGE
    });
    let max_chains = opts.max_chains.unwrap_or(1).max(1);
    let named_ids: HashSet<String> = flow.named.keys().cloned().collect();
    let traverser = GraphTraverser::new(store);

    let mut found: Vec<Vec<FlowStep>> = Vec::new();
    if directed {
        let from = normalize_token(opts.from.as_deref().unwrap_or(""));
        let to = normalize_token(opts.to.as_deref().unwrap_or(""));
        let from_ids = flow.token_nodes.get(&from).cloned().unwrap_or_default();
        let to_ids = flow.token_nodes.get(&to).cloned().unwrap_or_default();
        if from_ids.is_empty() || to_ids.is_empty() {
            return Ok(flow);
        }
        // Every candidate start is searched: the one that connects IS the
        // answer to which overload was meant.
        for id in &from_ids {
            let Some(seed) = flow.named.get(id) else {
                continue;
            };
            if let Some(steps) = walk_bidirectional(store, &traverser, seed, &to_ids, max_hops)? {
                found.push(steps);
            }
        }
    } else {
        let seeds: Vec<Node> = flow.named.values().take(MAX_SEEDS).cloned().collect();
        for seed in &seeds {
            let (parent, reached) = walk_calls(&traverser, seed, &named_ids, max_hops, max_bridge)?;
            // Explore's rule: the DEEPEST named sink this seed can reach.
            let mut deepest: Option<Vec<FlowStep>> = None;
            for id in &reached {
                let steps = chain_to(&parent, id);
                if deepest.as_ref().is_none_or(|d| steps.len() > d.len()) {
                    deepest = Some(steps);
                }
            }
            if let Some(steps) = deepest {
                found.push(steps);
            }
        }
    }
    if found.is_empty() {
        return Ok(flow);
    }
    if directed {
        found.sort_by_key(|steps| steps.len());
    } else {
        found.sort_by_key(|steps| std::cmp::Reverse(steps.len()));
    }
    // A chain that is a shorter run along one already kept is the same answer
    // twice; alternatives are for genuinely different routes.
    let mut kept: Vec<String> = Vec::new();
    for steps in found {
        let key = steps
            .iter()
            .map(|s| s.node.id.as_str())
            .collect::<Vec<_>>()
            .join(">");
        if kept
            .iter()
            .any(|other| *other == key || other.contains(&key))
        {
            continue;
        }
        kept.push(key);
        let call_sites = call_sites_of(&steps);
        flow.chains.push(FlowChain { steps, call_sites });
        if flow.chains.len() >= max_chains {
            break;
        }
    }
    Ok(flow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_symbol_shaped_deduped_and_extension_free() {
        assert_eq!(
            flow_tokens("how does cmd_serve reach Store::open_for_read, (main.rs) main.rs ab x.y"),
            vec![
                "how",
                "does",
                "cmd_serve",
                "reach",
                "Store::open_for_read",
                "main",
                "x.y"
            ]
        );
        assert_eq!(flow_tokens("a:b foo/bar 9abc abc.9"), vec!["abc.9"]);
        assert_eq!(normalize_token("handler.TS"), "handler");
        // The extension is anchored at the very end, before the trim.
        assert_eq!(normalize_token(" a.ts "), "a.ts");
        assert_eq!(normalize_token("Class.method"), "Class.method");
    }

    #[test]
    fn precise_tokens_look_like_references() {
        assert!(is_precise_token("Store::open"));
        assert!(is_precise_token("openForRead"));
        assert!(is_precise_token("Store"));
        assert!(is_precise_token("open_for_read"));
        assert!(!is_precise_token("open"));
    }

    #[test]
    fn the_container_segment_is_the_one_before_the_name() {
        assert_eq!(container_segment("pkg::Store::open"), "store");
        assert_eq!(container_segment("Store.open"), "store");
        assert_eq!(container_segment("open"), "");
    }
}
