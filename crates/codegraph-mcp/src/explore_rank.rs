//! Explore's file ranking (W7-25b).
//!
//! Upstream `handleExplore`'s ranking (`src/mcp/tools.ts`, `b635dd46`
//! 4625-5134): each file's nodes score by how they reached the subgraph
//! (named by the query, an entry point, one edge from one, or peripheral),
//! weighted by kind; generated, ambient-declaration and low-value files are
//! damped; test files are dropped when the query is not about tests; a
//! relative floor and a graph-relevance gate keep the files that matter; a
//! named method's buried signature type is rescued only when its file has
//! next to no graph mass (#1064); and the comparator orders pinned, named,
//! corroborated, then graph-central files first. The graph relevance is a
//! random walk with restart from the entry points
//! ([`codegraph_graph::relevance::graph_relevance`]).
//!
//! `#2557`'s wide test predicate, `#2655`'s type-to-type exception and the
//! other range rows that touch this stage are the next round's (KNOWN_DIFFS).

use std::collections::{HashMap, HashSet};

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_graph::relevance::graph_relevance;
use codegraph_store::Store;

use crate::explore_gather::outgoing_in_order;

/// How strongly a match on a node of this kind says its file is the
/// subject (upstream `RELEVANCE_KIND_WEIGHT`, b635dd46 418-431).
pub(crate) fn kind_weight(kind: NodeKind) -> f64 {
    match kind {
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::Class
        | NodeKind::Struct
        | NodeKind::Union
        | NodeKind::Interface
        | NodeKind::Trait
        | NodeKind::Protocol
        | NodeKind::Component
        | NodeKind::Route
        | NodeKind::Enum
        | NodeKind::TypeAlias => 1.0,
        NodeKind::Namespace | NodeKind::Module => 0.8,
        NodeKind::Property | NodeKind::Field | NodeKind::File => 0.5,
        NodeKind::EnumMember | NodeKind::Constant => 0.35,
        NodeKind::Variable => 0.3,
        NodeKind::Parameter => 0.15,
        _ => DEFAULT_KIND_WEIGHT,
    }
}

/// `DEFAULT_RELEVANCE_KIND_WEIGHT` (b635dd46 432).
const DEFAULT_KIND_WEIGHT: f64 = 0.5;

/// `TYPE_MEMBER_RELEVANCE_WEIGHT` (440): an interface's method signature.
const TYPE_MEMBER_WEIGHT: f64 = 0.5;

/// `ISOLATED_WEAK_KIND_WEIGHT` (453).
const ISOLATED_WEAK_WEIGHT: f64 = 0.08;

/// `PERIPHERAL_SCORE_CAP` (472).
const PERIPHERAL_CAP: f64 = 5.0;

/// `GENERATED_RANK_PENALTY` (486), `LOW_VALUE_RANK_PENALTY` (493),
/// `AMBIENT_DECLARATION_RANK_PENALTY` (516).
const GENERATED_PENALTY: f64 = 0.3;
const LOW_VALUE_PENALTY: f64 = 0.5;
const AMBIENT_PENALTY: f64 = 0.5;

/// The relative score floor (540-558).
const FLOOR_ABSOLUTE: f64 = 1.0;
const FLOOR_FRACTION_OF_TOP: f64 = 0.2;
const FLOOR_MAX: f64 = 10.0;
const FLOOR_KEEP_MIN: usize = 3;

/// The share of the top file's graph mass a file needs to pass the gate
/// and to count as not buried (5005, 5032).
const GATE_FRACTION: f64 = 0.06;

/// The graph-mass difference the comparator treats as a tie (5106).
const GRAPH_EPSILON_FRACTION: f64 = 0.01;

/// The score a rescued signature-type file starts at (5011).
const RESCUED_SCORE: f64 = 45.0;

/// Kinds whose weight depends on whether anything uses them
/// (`WEAK_RELEVANCE_KINDS`, 448-450).
fn is_weak_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Constant
            | NodeKind::Variable
            | NodeKind::Parameter
            | NodeKind::Field
            | NodeKind::Property
            | NodeKind::EnumMember
    )
}

/// Edges that mean a symbol is used (`RELEVANCE_USAGE_EDGES`, 460-463).
const USAGE_EDGES: [EdgeKind; 10] = [
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::Overrides,
    EdgeKind::Instantiates,
    EdgeKind::Returns,
    EdgeKind::TypeOf,
    EdgeKind::Decorates,
    EdgeKind::Navigates,
];

/// The callables whose signature types the #1064 rescue follows
/// (`CALLABLE_KINDS`, 4682; Rust has no constructor kind).
fn is_change_surface_callable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Function | NodeKind::Component
    )
}

/// The type kinds a signature edge may reach (`TYPE_KINDS`, 4683).
fn is_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Union
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::TypeAlias
    )
}

/// A key lifted out of a YAML or properties file (upstream
/// `isConfigLeafNode`, #383): its line is `key = <value>`, routinely a
/// secret, so it never makes its file a candidate, and `codegraph context`
/// gives its key alone as its code block.
pub(crate) fn is_config_leaf(node: &Node) -> bool {
    node.kind == NodeKind::Constant
        && matches!(node.language, Language::Yaml | Language::Properties)
}

/// A file's rank penalty, on its score and its graph mass (upstream
/// `rankPenalty`, b635dd46 4829-4839): the stronger of the generated and
/// ambient-declaration penalties, never their product, times the low-value
/// one.
pub(crate) fn rank_penalty(generated: bool, damped: bool, low_value: bool) -> f64 {
    let structural = f64::min(
        if generated { GENERATED_PENALTY } else { 1.0 },
        if damped { AMBIENT_PENALTY } else { 1.0 },
    );
    structural * if low_value { LOW_VALUE_PENALTY } else { 1.0 }
}

/// What the ranking reads of the explore subgraph.
pub(crate) struct RankInput<'a> {
    /// The subgraph's nodes in gather order.
    pub(crate) nodes: &'a [Node],
    pub(crate) edges: &'a [Edge],
    /// The entry points, in rank order.
    pub(crate) roots: &'a [String],
    pub(crate) named_seed_ids: &'a [String],
    pub(crate) tier_seed_ids: &'a [String],
    /// Files holding an exact target: Rust ranks them with the named files.
    pub(crate) exact_files: &'a HashSet<String>,
    /// Query-named paths in the order the query names them.
    pub(crate) pinned_files: &'a [String],
    /// Files the index flags generated (the path half is checked here).
    pub(crate) generated_files: &'a HashSet<String>,
    /// Ambient declaration files the query does not exempt.
    pub(crate) ambient_files: &'a HashSet<String>,
    /// Ranking-only `deprioritize` paths: they lose ties to first-party code.
    pub(crate) deprioritized_files: &'a HashSet<String>,
    /// The path-stripped query.
    pub(crate) query: &'a str,
    /// `CODEGRAPH_RANK_NO_MULTITERM=1`: no multi-term corroboration tier.
    pub(crate) multiterm_off: bool,
}

/// The ranking's answer.
pub(crate) struct Ranked {
    /// The files to render, best first.
    pub(crate) file_order: Vec<String>,
    /// Signature types rescued into the subgraph (#1064) that it did not
    /// hold yet, in rescue order.
    pub(crate) rescued: Vec<Node>,
    /// The files those rescues lifted into the named tier.
    pub(crate) rescued_files: Vec<String>,
    /// The one or two graph-central files that match the query's words.
    pub(crate) central_files: HashSet<String>,
    /// Nodes one edge from an entry point or a named seed.
    pub(crate) connected: HashSet<String>,
}

struct Group {
    nodes: Vec<usize>,
    score: f64,
    peripheral: f64,
}

/// Ranks the subgraph's files as upstream's `handleExplore` does.
pub(crate) fn rank_files(store: &Store, input: &RankInput<'_>) -> anyhow::Result<Ranked> {
    let mut nodes: Vec<Node> = input.nodes.to_vec();
    let mut index: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(at, n)| (n.id.clone(), at))
        .collect();
    let named: HashSet<&str> = input.named_seed_ids.iter().map(String::as_str).collect();
    let mut entry: Vec<String> = Vec::new();
    for id in input.roots.iter().chain(input.named_seed_ids) {
        if !entry.contains(id) {
            entry.push(id.clone());
        }
    }
    let entry_set: HashSet<&str> = entry.iter().map(String::as_str).collect();
    let pinned: HashMap<&str, usize> = input
        .pinned_files
        .iter()
        .enumerate()
        .map(|(at, p)| (p.as_str(), at))
        .collect();

    let mut connected: HashSet<String> = HashSet::new();
    for edge in input.edges {
        if entry_set.contains(edge.source.as_str()) {
            connected.insert(edge.target.clone());
        }
        if entry_set.contains(edge.target.as_str()) {
            connected.insert(edge.source.clone());
        }
    }
    let mut usage_degree: HashMap<&str, usize> = HashMap::new();
    for edge in input.edges {
        if !USAGE_EDGES.contains(&edge.kind) || edge.source == edge.target {
            continue;
        }
        *usage_degree.entry(edge.source.as_str()).or_default() += 1;
        *usage_degree.entry(edge.target.as_str()).or_default() += 1;
    }

    let interface_owned = interface_owned_methods(store, input.nodes)?;
    let is_signature =
        |node: &Node| node.kind == NodeKind::Method && interface_owned.contains(&node.id);
    let mut isolated: HashMap<String, bool> = HashMap::new();
    let mut relevance_weight = |node: &Node, probe: bool| -> anyhow::Result<f64> {
        let signature = is_signature(node);
        let weight = if signature {
            TYPE_MEMBER_WEIGHT
        } else {
            kind_weight(node.kind)
        };
        if !probe || !(signature || is_weak_kind(node.kind)) {
            return Ok(weight);
        }
        if usage_degree.get(node.id.as_str()).copied().unwrap_or(0) > 0 {
            return Ok(weight);
        }
        let lonely = match isolated.get(&node.id) {
            Some(&known) => known,
            None => {
                let used = store
                    .edges_by_target_kind(&node.id, None)?
                    .iter()
                    .chain(store.edges_by_source_kind(&node.id, None)?.iter())
                    .any(|edge| USAGE_EDGES.contains(&edge.kind));
                isolated.insert(node.id.clone(), !used);
                !used
            }
        };
        Ok(if lonely { ISOLATED_WEAK_WEIGHT } else { weight })
    };

    // The #1064 change surface: the signature types of the named tier's
    // callables, collected here and rescued below only when buried.
    let mut candidates: Vec<Node> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for seed in input.tier_seed_ids {
        let Some(node) = index.get(seed).map(|at| &nodes[*at]) else {
            continue;
        };
        if !is_change_surface_callable(node.kind) {
            continue;
        }
        for edge in outgoing_in_order(store, seed, &[])? {
            if !matches!(
                edge.kind,
                EdgeKind::References | EdgeKind::TypeOf | EdgeKind::Returns
            ) {
                continue;
            }
            let Some(target) = store.node_by_id(&edge.target)? else {
                continue;
            };
            if !is_type_kind(target.kind) || named.contains(target.id.as_str()) {
                continue;
            }
            if seen.insert(target.id.clone()) {
                candidates.push(target);
            }
        }
    }

    // Group by file and score.
    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut group_at: HashMap<String, usize> = HashMap::new();
    for (at, node) in nodes.iter().enumerate() {
        if matches!(node.kind, NodeKind::Export | NodeKind::Import) || is_config_leaf(node) {
            continue;
        }
        let slot = match group_at.get(&node.file_path) {
            Some(&slot) => slot,
            None => {
                groups.push((
                    node.file_path.clone(),
                    Group {
                        nodes: Vec::new(),
                        score: 0.0,
                        peripheral: 0.0,
                    },
                ));
                group_at.insert(node.file_path.clone(), groups.len() - 1);
                groups.len() - 1
            }
        };
        let group = &mut groups[slot].1;
        group.nodes.push(at);
        if named.contains(node.id.as_str()) {
            group.score += 50.0 * relevance_weight(node, true)?;
        } else if entry_set.contains(node.id.as_str()) {
            group.score += 10.0 * relevance_weight(node, true)?;
        } else if connected.contains(&node.id) {
            group.score += 3.0 * relevance_weight(node, false)?;
        } else {
            group.peripheral += relevance_weight(node, false)?;
        }
    }

    let generated = |path: &str| input.generated_files.contains(path) || is_generated_file(path);
    let damped = |path: &str| input.ambient_files.contains(path);
    let low_value = |path: &str| crate::engine::is_low_value_file(path);
    let rank_penalty = |path: &str| rank_penalty(generated(path), damped(path), low_value(path));
    for (path, group) in &mut groups {
        group.score = (group.score + group.peripheral.min(PERIPHERAL_CAP)) * rank_penalty(path);
    }

    // Test files out unless the query is about tests, when two others remain.
    let mut candidate_files: Vec<usize> = (0..groups.len()).collect();
    if !crate::engine::query_mentions_tests(input.query) {
        let keep: Vec<usize> = candidate_files
            .iter()
            .copied()
            .filter(|&g| {
                let path = groups[g].0.as_str();
                !low_value(path) || pinned.contains_key(path)
            })
            .collect();
        if keep.len() >= 2 {
            candidate_files = keep;
        }
    }

    // The relative floor, backfilled when it leaves fewer than three.
    let top = candidate_files
        .iter()
        .map(|&g| groups[g].1.score)
        .fold(0.0f64, f64::max);
    let floor = FLOOR_ABSOLUTE.max(FLOOR_MAX.min(top * FLOOR_FRACTION_OF_TOP));
    let is_pinned =
        |g: usize, groups: &[(String, Group)]| pinned.contains_key(groups[g].0.as_str());
    let mut relevant: Vec<usize> = candidate_files
        .iter()
        .copied()
        .filter(|&g| groups[g].1.score >= floor || is_pinned(g, &groups))
        .collect();
    if relevant.len() < FLOOR_KEEP_MIN {
        let min_evidence = if relevant.is_empty() {
            f64::EPSILON
        } else {
            FLOOR_ABSOLUTE
        };
        let want = FLOOR_KEEP_MIN.max(relevant.len());
        let mut backfill: Vec<usize> = candidate_files
            .iter()
            .copied()
            .filter(|&g| groups[g].1.score >= min_evidence || is_pinned(g, &groups))
            .collect();
        stable_sort_by(&mut backfill, |&a, &b| {
            (is_pinned(b, &groups) as u8)
                .cmp(&(is_pinned(a, &groups) as u8))
                .then_with(|| cmp_f64(groups[b].1.score, groups[a].1.score))
                .then_with(|| groups[b].1.nodes.len().cmp(&groups[a].1.nodes.len()))
        });
        backfill.truncate(want);
        relevant = backfill;
    }

    // Distinct query words each file matches, in its path or node names.
    let mut terms: Vec<String> = Vec::new();
    for word in input.query.to_lowercase().split_whitespace() {
        if word.chars().count() >= 3 && !terms.iter().any(|t| t == word) {
            terms.push(word.to_string());
        }
    }
    let mut term_hits: HashMap<String, usize> = HashMap::new();
    for &g in &relevant {
        let (path, group) = &groups[g];
        let mut hay = path.to_lowercase();
        for &at in &group.nodes {
            hay.push(' ');
            hay.push_str(&nodes[at].name.to_lowercase());
        }
        let hits = terms.iter().filter(|t| hay.contains(t.as_str())).count();
        term_hits.insert(path.clone(), hits);
    }
    let hits_of = |path: &str| term_hits.get(path).copied().unwrap_or(0);

    // Graph relevance from the entry points, a damped declaration file's
    // nodes left out of the restart vector unless every one is in one.
    let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut seeds: HashSet<String> = entry
        .iter()
        .filter(|id| {
            index
                .get(id.as_str())
                .is_some_and(|at| !damped(&nodes[*at].file_path))
        })
        .cloned()
        .collect();
    if seeds.is_empty() {
        seeds = entry.iter().cloned().collect();
    }
    let mass = graph_relevance(&node_ids, input.edges, &seeds);
    let mut graph: Vec<(String, f64)> = Vec::new();
    let mut graph_at: HashMap<String, usize> = HashMap::new();
    for (at, node) in nodes.iter().enumerate() {
        let slot = *graph_at.entry(node.file_path.clone()).or_insert_with(|| {
            graph.push((node.file_path.clone(), 0.0));
            graph.len() - 1
        });
        graph[slot].1 += mass[at];
    }
    for (path, value) in &mut graph {
        *value *= rank_penalty(path);
    }
    let graph_of = |path: &str| graph_at.get(path).map(|&at| graph[at].1).unwrap_or(0.0);
    let max_graph = graph.iter().map(|(_, g)| *g).fold(0.0f64, f64::max);

    let mut central: Vec<(String, f64)> = graph
        .iter()
        .filter(|(path, g)| *g > 0.0 && hits_of(path) >= 1)
        .cloned()
        .collect();
    stable_sort_by(&mut central, |a, b| {
        cmp_f64(b.1, a.1).then_with(|| hits_of(&b.0).cmp(&hits_of(&a.0)))
    });
    let central_files: HashSet<String> = central.into_iter().take(2).map(|(p, _)| p).collect();

    let entry_files: HashSet<String> = entry
        .iter()
        .filter_map(|id| index.get(id).map(|at| nodes[*at].file_path.clone()))
        .collect();

    // Rescue a signature type only when its file is buried.
    let mut rescued: Vec<Node> = Vec::new();
    let mut rescued_files: Vec<String> = Vec::new();
    for target in candidates {
        let path = target.file_path.clone();
        let buried = graph_of(&path) < max_graph * GATE_FRACTION && hits_of(&path) < 2;
        if !buried {
            continue;
        }
        if !rescued_files.contains(&path) {
            rescued_files.push(path.clone());
        }
        let at = match index.get(&target.id) {
            Some(&at) => at,
            None => {
                nodes.push(target.clone());
                index.insert(target.id.clone(), nodes.len() - 1);
                rescued.push(target.clone());
                nodes.len() - 1
            }
        };
        let slot = match group_at.get(&path) {
            Some(&slot) => slot,
            None => {
                groups.push((
                    path.clone(),
                    Group {
                        nodes: Vec::new(),
                        score: 0.0,
                        peripheral: 0.0,
                    },
                ));
                group_at.insert(path.clone(), groups.len() - 1);
                groups.len() - 1
            }
        };
        let group = &mut groups[slot].1;
        if !group.nodes.contains(&at) {
            group.nodes.push(at);
        }
        group.score = group.score.max(RESCUED_SCORE);
        if !relevant.contains(&slot) {
            relevant.push(slot);
        }
    }

    // The relevance gate, never below two files.
    if max_graph > 0.0 {
        let gated: Vec<usize> = relevant
            .iter()
            .copied()
            .filter(|&g| {
                let path = groups[g].0.as_str();
                pinned.contains_key(path)
                    || graph_of(path) >= max_graph * GATE_FRACTION
                    || central_files.contains(path)
                    || entry_files.contains(path)
                    || rescued_files.iter().any(|f| f == path)
                    || hits_of(path) >= 2
            })
            .collect();
        if gated.len() >= 2 {
            relevant = gated;
        }
    }

    let mut named_files: HashSet<String> = input
        .tier_seed_ids
        .iter()
        .filter_map(|id| index.get(id).map(|at| nodes[*at].file_path.clone()))
        .collect();
    named_files.extend(rescued_files.iter().cloned());
    named_files.extend(input.exact_files.iter().cloned());
    let corroborated = |path: &str| {
        !input.multiterm_off
            && hits_of(path) >= 2
            && (entry_files.contains(path) || central_files.contains(path))
    };

    stable_sort_by(&mut relevant, |&a, &b| {
        let (pa, ga) = (&groups[a].0, &groups[a].1);
        let (pb, gb) = (&groups[b].0, &groups[b].1);
        let pin_a = pinned.get(pa.as_str());
        let pin_b = pinned.get(pb.as_str());
        match (pin_a, pin_b) {
            (Some(x), Some(y)) => return x.cmp(y),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            (None, None) => {}
        }
        let key = |p: &str| named_files.contains(p);
        if key(pa) != key(pb) {
            return key(pb).cmp(&key(pa));
        }
        if corroborated(pa) != corroborated(pb) {
            return corroborated(pb).cmp(&corroborated(pa));
        }
        let (graph_a, graph_b) = (graph_of(pa), graph_of(pb));
        if (graph_a - graph_b).abs() > max_graph * GRAPH_EPSILON_FRACTION {
            return cmp_f64(graph_b, graph_a);
        }
        let (hits_a, hits_b) = (hits_of(pa), hits_of(pb));
        if hits_a != hits_b {
            return hits_b.cmp(&hits_a);
        }
        if low_value(pa) != low_value(pb) {
            return low_value(pa).cmp(&low_value(pb));
        }
        if generated(pa) != generated(pb) {
            return generated(pa).cmp(&generated(pb));
        }
        if ga.score != gb.score {
            return cmp_f64(gb.score, ga.score);
        }
        gb.nodes
            .len()
            .cmp(&ga.nodes.len())
            .then_with(|| {
                input
                    .deprioritized_files
                    .contains(pa.as_str())
                    .cmp(&input.deprioritized_files.contains(pb.as_str()))
            })
            .then_with(|| pa.cmp(pb))
    });

    Ok(Ranked {
        file_order: relevant.into_iter().map(|g| groups[g].0.clone()).collect(),
        rescued,
        rescued_files,
        central_files,
        connected,
    })
}

/// The methods among `nodes` that an interface contains (a signature without a
/// body, #1638). The `contains` edges into every method come in one batched
/// read, and each owner's kind is taken once, from `nodes` when they hold the
/// owner (W7-25 cost); whether any owner is an interface does not depend on
/// the order the edges come in.
pub(crate) fn interface_owned_methods(
    store: &Store,
    nodes: &[Node],
) -> anyhow::Result<HashSet<String>> {
    let methods: Vec<String> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .map(|n| n.id.clone())
        .collect();
    let mut owned = HashSet::new();
    if methods.is_empty() {
        return Ok(owned);
    }
    let held: HashMap<&str, NodeKind> = nodes.iter().map(|n| (n.id.as_str(), n.kind)).collect();
    let mut owner_kind: HashMap<String, Option<NodeKind>> = HashMap::new();
    for edge in store.edges_by_targets_kind(&methods, EdgeKind::Contains)? {
        if owned.contains(&edge.target) {
            continue;
        }
        let kind = match owner_kind.get(&edge.source) {
            Some(kind) => *kind,
            None => {
                let kind = match held.get(edge.source.as_str()) {
                    Some(kind) => Some(*kind),
                    None => store.node_by_id(&edge.source)?.map(|owner| owner.kind),
                };
                owner_kind.insert(edge.source.clone(), kind);
                kind
            }
        };
        if kind == Some(NodeKind::Interface) {
            owned.insert(edge.target);
        }
    }
    Ok(owned)
}

fn cmp_f64(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

/// A stable insertion sort. Upstream's comparator treats graph masses within
/// 1% of the top as equal, which is not transitive; the standard sort may
/// panic on such an order, this one never does and gives the same answer
/// for the same input.
fn stable_sort_by<T, F>(items: &mut [T], mut compare: F)
where
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 && compare(&items[j], &items[j - 1]) == std::cmp::Ordering::Less {
            items.swap(j, j - 1);
            j -= 1;
        }
    }
}
