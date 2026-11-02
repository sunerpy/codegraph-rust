//! The interface dispatch pass (upstream `interfaceOverrideEdges` at
//! `e8ddc4e2`, keyed by method name as at `b635dd46`): a call through an
//! interface or a base type runs the same-named method of the type that
//! implements or extends it, so each base method links to those
//! (`interface-impl`). #2746's Go method key and the methods a Go struct's
//! embeddings promote into it come with the Go prerequisites (W7-15).

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use serde_json::json;

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::dispatch::{methods_of, place, synthesized};
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

/// Interface dispatch (`interfaceOverrideEdges`): for each concrete type,
/// along each of its `extends` and `implements` edges, each method of the
/// base links to every method of the type with its name (all overloads),
/// at most [`MAX_CALLBACKS_PER_CHANNEL`] an edge. A Go type's only
/// dispatching supertype is an interface: its other supertype edges are
/// embeddings.
pub(super) fn interface_override_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    // Upstream walks every concrete type and first skips one without a
    // supertype edge; only those edges' sources can link, so the walk is
    // over them, in upstream's order.
    let mut sources: HashSet<String> = HashSet::new();
    for kind in [EdgeKind::Extends, EdgeKind::Implements] {
        sources.extend(
            view.edges_of_kind(kind)?
                .into_iter()
                .map(|edge| edge.source),
        );
    }
    let mut concrete: Vec<(u8, Node)> = Vec::new();
    for id in &sources {
        if let Some(node) = view.node(id)?
            && let Some(rank) = concrete_rank(&node)
        {
            concrete.push((rank, node));
        }
    }
    concrete.sort_by(|(a_rank, a), (b_rank, b)| a_rank.cmp(b_rank).then_with(|| node_order(a, b)));

    let mut methods = Methods {
        view,
        memo: HashMap::new(),
    };
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (_, class) in &concrete {
        let implementing: Vec<Node> = methods
            .of(&class.id)?
            .into_iter()
            .filter(|method| LANGUAGES.contains(&method.language))
            .collect();
        if implementing.is_empty() {
            continue;
        }
        // Every overload of a name: a call may resolve to any of them.
        let mut by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
        for method in &implementing {
            by_name
                .entry(method.name.as_str())
                .or_default()
                .push(method);
        }
        // Upstream's `getOutgoingEdges(id, ['implements', 'extends'])`:
        // by kind, then target, line and column.
        let mut supertypes = view.edges_from(&class.id, EdgeKind::Extends)?;
        supertypes.extend(view.edges_from(&class.id, EdgeKind::Implements)?);
        for supertype in &supertypes {
            let Some(base) = view.node(&supertype.target)? else {
                continue;
            };
            if !LANGUAGES.contains(&base.language) || base.id == class.id {
                continue;
            }
            if class.language == Language::Go && base.kind != NodeKind::Interface {
                continue;
            }
            let mut added = 0;
            'base: for base_method in methods.of_base(&base)? {
                let Some(matching) = by_name.get(base_method.name.as_str()) else {
                    continue;
                };
                for method in matching {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break 'base;
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
        }
    }
    Ok(edges)
}
