//! The interface dispatch pass (upstream `interfaceOverrideEdges` at
//! `e8ddc4e2`): a call through an interface or a base type runs the
//! same-named method of the type that implements or extends it, so each
//! base method links to those (`interface-impl`). A Go interface's
//! methods pair by #2746's key, and a Go struct that satisfies one
//! (`go-implements`) also runs the methods its embeddings promote into it
//! (#2414's `promotedInto`, W7-15b).

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use serde_json::json;

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::dispatch::{methods_of, methods_of_each, place, synthesized};
use super::go::{is_embedding, method_key, type_kind};
use super::stage::{View, node_order};

/// The languages whose methods a call through an interface reaches
/// (upstream `IFACE_OVERRIDE_LANGS`): not tsx or jsx.
const LANGUAGES: [Language; 10] = [
    Language::Java,
    Language::Kotlin,
    Language::CSharp,
    Language::TypeScript,
    Language::JavaScript,
    Language::Swift,
    Language::Scala,
    Language::Go,
    Language::Rust,
    Language::ArkTs,
];

/// The languages whose files open the pass (upstream's `ifaceEdges` gate).
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    [
        Language::Java,
        Language::Kotlin,
        Language::CSharp,
        Language::Swift,
        Language::Scala,
        Language::Go,
        Language::Rust,
        Language::ArkTs,
        Language::TypeScript,
        Language::JavaScript,
        Language::Tsx,
        Language::Jsx,
    ]
    .iter()
    .any(|language| languages.contains(language))
}

/// A concrete type's place in upstream's walk: every class, then every
/// struct, every union, and every Go defined type (`type HandlerFunc
/// func(…)`, a type alias holding its methods); `None` for another node.
fn concrete_rank(node: &Node) -> Option<u8> {
    match node.kind {
        NodeKind::Class => Some(0),
        NodeKind::Struct => Some(1),
        NodeKind::Union => Some(2),
        NodeKind::TypeAlias if node.language == Language::Go => Some(3),
        _ => None,
    }
}

/// The methods of a node, read once each (upstream's `methodsMemo`).
struct Methods<'v, 'a> {
    view: &'v View<'a>,
    memo: HashMap<String, Vec<Node>>,
}

impl Methods<'_, '_> {
    fn of(&mut self, id: &str) -> Result<Vec<Node>> {
        if let Some(methods) = self.memo.get(id) {
            return Ok(methods.clone());
        }
        let methods = methods_of(self.view, id)?;
        self.memo.insert(id.to_string(), methods.clone());
        Ok(methods)
    }

    /// A base type's methods. A Swift protocol's requirements are not
    /// methods; its extensions, the Swift classes of its name, hold the
    /// default implementations a conformer overrides.
    fn of_base(&mut self, base: &Node) -> Result<Vec<Node>> {
        let mut methods = self.of(&base.id)?;
        if base.language == Language::Swift && base.kind == NodeKind::Interface {
            for extension in self.view.nodes_named(&base.name)? {
                if extension.language == Language::Swift && extension.kind == NodeKind::Class {
                    methods.extend(self.of(&extension.id)?);
                }
            }
        }
        Ok(methods)
    }
}

/// The methods a Go struct runs for a method it does not declare, and the
/// line of the embedding of its own they come through (upstream
/// `GoPromotion`).
struct Promotion {
    methods: Vec<Node>,
    line: i64,
}

/// Which embedded type's method a Go struct runs for a method it does not
/// declare (upstream `goPromotions`), as Go's selector picks it:
/// breadth-first through the struct's embeddings, the shallowest depth
/// holding the method wins, and nothing when it occurs more than once at
/// that depth, one type reached along two paths included. A type met at a
/// shallower depth is not walked again. An embedded interface brings its
/// whole method set at its own depth, and a method it provides is a
/// dynamic call once more, so no promotion either. The method is asked for
/// by its [`method_key`].
struct Promotions<'n> {
    /// The nodes the pass read at once; another is read from the view.
    nodes: &'n HashMap<String, Node>,
    kinds: HashMap<String, Option<NodeKind>>,
    embeds: HashMap<String, Vec<Edge>>,
    iface_keys: HashMap<String, HashSet<String>>,
    memo: HashMap<(String, String), Option<Promotion>>,
}

impl<'n> Promotions<'n> {
    fn new(nodes: &'n HashMap<String, Node>) -> Self {
        Self {
            nodes,
            kinds: HashMap::new(),
            embeds: HashMap::new(),
            iface_keys: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    fn kind_of(&mut self, view: &View<'_>, id: &str) -> Result<Option<NodeKind>> {
        if let Some(kind) = self.kinds.get(id) {
            return Ok(*kind);
        }
        let kind = node_in(view, self.nodes, id)?.as_ref().and_then(type_kind);
        self.kinds.insert(id.to_string(), kind);
        Ok(kind)
    }

    /// The embeddings out of `id`, one edge per embedded type: the first of
    /// upstream's `getOutgoingEdges(id, ['extends', 'implements'])`, by
    /// kind, then target, line and column.
    fn embeds(&mut self, view: &View<'_>, id: &str) -> Result<Vec<Edge>> {
        if let Some(embeds) = self.embeds.get(id) {
            return Ok(embeds.clone());
        }
        let mut supertypes = view.edges_from(id, EdgeKind::Extends)?;
        supertypes.extend(view.edges_from(id, EdgeKind::Implements)?);
        let source = self.kind_of(view, id)?;
        let mut targets: HashSet<String> = HashSet::new();
        let mut embeds = Vec::new();
        for edge in supertypes {
            let target = self.kind_of(view, &edge.target)?;
            if is_embedding(&edge, source, target) && targets.insert(edge.target.clone()) {
                embeds.push(edge);
            }
        }
        self.embeds.insert(id.to_string(), embeds.clone());
        Ok(embeds)
    }

    /// An interface's method keys, with those of the interfaces it embeds.
    fn iface_keys(
        &mut self,
        view: &View<'_>,
        methods: &mut Methods<'_, '_>,
        id: &str,
    ) -> Result<HashSet<String>> {
        if let Some(keys) = self.iface_keys.get(id) {
            return Ok(keys.clone());
        }
        let mut keys = HashSet::new();
        let mut reached: HashSet<String> = HashSet::from([id.to_string()]);
        let mut pending = vec![id.to_string()];
        while let Some(at) = pending.pop() {
            keys.extend(methods.of(&at)?.iter().map(method_key));
            for edge in self.embeds(view, &at)? {
                if reached.insert(edge.target.clone()) {
                    pending.push(edge.target);
                }
            }
        }
        self.iface_keys.insert(id.to_string(), keys.clone());
        Ok(keys)
    }

    fn provides(
        &mut self,
        view: &View<'_>,
        methods: &mut Methods<'_, '_>,
        id: &str,
        key: &str,
    ) -> Result<bool> {
        if self.kind_of(view, id)? == Some(NodeKind::Interface) {
            return Ok(self.iface_keys(view, methods, id)?.contains(key));
        }
        Ok(methods
            .of(id)?
            .iter()
            .any(|method| method_key(method) == key))
    }

    /// The promotion of the method keyed `key` into the struct `id`.
    fn find(
        &mut self,
        view: &View<'_>,
        methods: &mut Methods<'_, '_>,
        id: &str,
        key: &str,
    ) -> Result<Option<&Promotion>> {
        let memo_key = (id.to_string(), key.to_string());
        if !self.memo.contains_key(&memo_key) {
            let found = self.walk(view, methods, id, key)?;
            self.memo.insert(memo_key.clone(), found);
        }
        Ok(self.memo[&memo_key].as_ref())
    }

    fn walk(
        &mut self,
        view: &View<'_>,
        methods: &mut Methods<'_, '_>,
        id: &str,
        key: &str,
    ) -> Result<Option<Promotion>> {
        // Each type at the current depth, with how many paths reach it and
        // the line of the struct's own embedding the first of them starts
        // with, in the order upstream's map holds them.
        let mut level: Vec<(String, u64, i64)> = vec![(id.to_string(), 1, 0)];
        let mut met: HashSet<String> = HashSet::from([id.to_string()]);
        let mut depth = 0;
        while !level.is_empty() {
            let mut next: Vec<(String, u64, i64)> = Vec::new();
            let mut slot: HashMap<String, usize> = HashMap::new();
            for (at, paths, line) in &level {
                if self.kind_of(view, at)? == Some(NodeKind::Interface) {
                    // Its embeddings are its method set.
                    continue;
                }
                for edge in self.embeds(view, at)? {
                    if met.contains(&edge.target) {
                        continue;
                    }
                    if let Some(&index) = slot.get(&edge.target) {
                        next[index].1 += paths;
                    } else {
                        slot.insert(edge.target.clone(), next.len());
                        let first = if depth == 0 {
                            edge.line.unwrap_or(0)
                        } else {
                            *line
                        };
                        next.push((edge.target, *paths, first));
                    }
                }
            }
            let mut paths = 0;
            let mut provider: Option<usize> = None;
            for (index, (at, reached, _)) in next.iter().enumerate() {
                met.insert(at.clone());
                if !self.provides(view, methods, at, key)? {
                    continue;
                }
                paths += reached;
                provider = Some(index);
            }
            if let Some(index) = provider {
                let (at, _, line) = &next[index];
                if paths == 1 && self.kind_of(view, at)? != Some(NodeKind::Interface) {
                    let provided = methods
                        .of(at)?
                        .into_iter()
                        .filter(|method| method_key(method) == key)
                        .collect();
                    return Ok(Some(Promotion {
                        methods: provided,
                        line: *line,
                    }));
                }
                return Ok(None);
            }
            level = next;
            depth += 1;
        }
        Ok(None)
    }
}

fn is_heuristic(edge: &Edge) -> bool {
    edge.provenance.as_deref() == Some("heuristic")
}

/// The node `id` from those read at once, or else from the view (an id
/// they have no entry for may not have been asked for).
fn node_in(view: &View<'_>, nodes: &HashMap<String, Node>, id: &str) -> Result<Option<Node>> {
    match nodes.get(id) {
        Some(node) => Ok(Some(node.clone())),
        None => view.node(id),
    }
}

/// Interface dispatch (`interfaceOverrideEdges`): for each concrete type,
/// along each of its `extends` and `implements` edges, each method of the
/// base links to every method of the type with its name (all overloads),
/// or its [`method_key`] for a Go interface, at most
/// [`MAX_CALLBACKS_PER_CHANNEL`] an edge. A Go type's only dispatching
/// supertype is an interface: its other supertype edges are embeddings. A
/// Go struct that satisfies an interface (a synthesized edge) may do so
/// with promoted methods, linked after every type, from what its edge's
/// cap has left.
pub(super) fn interface_override_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    // Upstream walks every concrete type and first skips one without a
    // supertype edge; only those edges' sources can link, so the walk is
    // over them, in upstream's order. The nodes at both ends of those
    // edges, and the methods of the types and bases among them, are read
    // at once (W7-15b's cost revision).
    let mut sources: HashSet<String> = HashSet::new();
    let mut targets: HashSet<String> = HashSet::new();
    for kind in [EdgeKind::Extends, EdgeKind::Implements] {
        for edge in view.edges_of_kind(kind)? {
            sources.insert(edge.source);
            targets.insert(edge.target);
        }
    }
    let ends: Vec<String> = sources.union(&targets).cloned().collect();
    let nodes = view.nodes(&ends)?;
    let mut concrete: Vec<(u8, Node)> = Vec::new();
    for id in &sources {
        if let Some(node) = node_in(view, &nodes, id)?
            && let Some(rank) = concrete_rank(&node)
        {
            concrete.push((rank, node));
        }
    }
    concrete.sort_by(|(a_rank, a), (b_rank, b)| a_rank.cmp(b_rank).then_with(|| node_order(a, b)));

    let mut read: Vec<String> = concrete.iter().map(|(_, node)| node.id.clone()).collect();
    read.extend(
        targets
            .iter()
            .filter(|id| {
                nodes
                    .get(*id)
                    .is_some_and(|node| LANGUAGES.contains(&node.language))
            })
            .cloned(),
    );
    let mut methods = Methods {
        view,
        memo: methods_of_each(view, &read)?,
    };
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut promoted: Vec<(&Node, Vec<Node>, usize)> = Vec::new();
    for (_, class) in &concrete {
        // Upstream's `getOutgoingEdges(id, ['implements', 'extends'])`:
        // by kind, then target, line and column.
        let mut supertypes = view.edges_from(&class.id, EdgeKind::Extends)?;
        supertypes.extend(view.edges_from(&class.id, EdgeKind::Implements)?);
        let implementing: Vec<Node> = methods
            .of(&class.id)?
            .into_iter()
            .filter(|method| LANGUAGES.contains(&method.language))
            .collect();
        // A Go struct that satisfies an interface may do so with promoted
        // methods alone.
        let go_struct = class.language == Language::Go && class.kind == NodeKind::Struct;
        if implementing.is_empty() && !(go_struct && supertypes.iter().any(is_heuristic)) {
            continue;
        }
        // Every overload of a name: a call may resolve to any of them.
        let mut by_name: HashMap<String, Vec<&Node>> = HashMap::new();
        let mut by_go_key: HashMap<String, Vec<&Node>> = HashMap::new();
        for method in &implementing {
            by_name.entry(method.name.clone()).or_default().push(method);
            by_go_key
                .entry(method_key(method))
                .or_default()
                .push(method);
        }
        for supertype in &supertypes {
            let Some(base) = node_in(view, &nodes, &supertype.target)? else {
                continue;
            };
            if !LANGUAGES.contains(&base.language) || base.id == class.id {
                continue;
            }
            if class.language == Language::Go && base.kind != NodeKind::Interface {
                continue;
            }
            let promotes = go_struct && is_heuristic(supertype) && base.kind == NodeKind::Interface;
            // A Go interface's unexported method is matched in its own
            // package only: a namesake declared elsewhere is another method.
            let go_keys = base.language == Language::Go && base.kind == NodeKind::Interface;
            let mut added = 0;
            let mut unmatched: Vec<Node> = Vec::new();
            for base_method in methods.of_base(&base)? {
                if added >= MAX_CALLBACKS_PER_CHANNEL {
                    break;
                }
                let matching = if go_keys {
                    by_go_key.get(&method_key(&base_method))
                } else {
                    by_name.get(&base_method.name)
                };
                let Some(matching) = matching else {
                    if promotes {
                        unmatched.push(base_method);
                    }
                    continue;
                };
                for method in matching {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break;
                    }
                    if base_method.id == method.id
                        || !seen.insert((base_method.id.clone(), method.id.clone()))
                    {
                        continue;
                    }
                    edges.push(synthesized(
                        &base_method,
                        method,
                        EdgeKind::Calls,
                        base_method.start_line,
                        json!({
                            "synthesizedBy": "interface-impl",
                            "via": method.name,
                            "registeredAt": place(method),
                        }),
                    ));
                    added += 1;
                }
            }
            if !unmatched.is_empty() && added < MAX_CALLBACKS_PER_CHANNEL {
                promoted.push((class, unmatched, MAX_CALLBACKS_PER_CHANNEL - added));
            }
        }
    }
    // The wiring site of a promoted method is the struct's embedding it
    // comes through: `type blockSeriesSet struct{ blockBaseSeriesSet }`.
    let mut promotions = Promotions::new(&nodes);
    for (class, wanted, left) in promoted {
        let mut budget = left;
        for base_method in wanted {
            if budget == 0 {
                break;
            }
            let key = method_key(&base_method);
            let Some(promotion) = promotions.find(view, &mut methods, &class.id, &key)? else {
                continue;
            };
            let line = if promotion.line == 0 {
                class.start_line
            } else {
                promotion.line
            };
            for method in &promotion.methods {
                if budget == 0 {
                    break;
                }
                if !seen.insert((base_method.id.clone(), method.id.clone())) {
                    continue;
                }
                edges.push(synthesized(
                    &base_method,
                    method,
                    EdgeKind::Calls,
                    base_method.start_line,
                    json!({
                        "synthesizedBy": "interface-impl",
                        "via": method.name,
                        "promotedInto": class.name,
                        "registeredAt": format!("{}:{line}", class.file_path),
                    }),
                ));
                budget -= 1;
            }
        }
    }
    Ok(edges)
}
