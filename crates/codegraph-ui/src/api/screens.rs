//! `GET /api/screens` — the app as a reader experiences it: screens, and the
//! transitions between them, each labelled with what has to be true for it
//! to happen. Upstream `src/ui-server/api/screens.ts` (`e8ddc4e2`).
//!
//! The graph holds the pieces: a `route` node per screen, and a `navigates`
//! edge from the function that pushes a path to the route it names. What a
//! reader wants is "from the Home screen, tapping an object card opens
//! Object Detail, but only for a collected object" — several hops away from
//! the edge. So for every navigation this walks BACKWARDS from its source
//! through `calls`, `instantiates`, `contains` and function-valued
//! `references` until it reaches the component a route renders (or a layout
//! the route renders inside). That screen is where the transition starts;
//! the nodes passed on the way are the link's `via`, and the conditions at
//! each call site along it, read from the source now, join into its `when`.
//! A navigation no screen reaches is kept as an `origin`; one whose walk hit
//! a cap is `dropped`. With no screen route, or no navigation into one, the
//! payload is `routed: false` and the view says the graph holds no screen
//! navigation.
//!
//! Every list here follows a total order, never the order the store returned
//! its rows in: the navigations by the holder's file, the line, the column
//! and the target; each layer of the walk by the frontier, each target's
//! edges by source, line, column and kind; names and paths by
//! [`locale_compare`]. Upstream takes SQLite's row order, so when two paths
//! of one length reach a screen it may draw the other one.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use codegraph_graph::collation::locale_compare;
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::route_roots::{route_layouts, route_roots};
use super::source::{
    find_indexed_file, has_drifted_on_disk, max_file_size, read_indexed_file_text,
};
use super::when::SiteReader;
use super::wire::{WireNodeRef, to_node_ref, to_posix_path};
use crate::respond::ApiResult;

// =============================================================================
// Wire shapes (upstream `WireScreensPayload`)
// =============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireScreen {
    /// The route node's id — what a link's `from` / `to` name.
    pub id: String,
    pub path: String,
    pub file: String,
    pub line: i64,
    /// The component the route renders, when the graph bound one.
    pub component: Option<WireNodeRef>,
    pub incoming: usize,
    pub outgoing: usize,
}

/// A navigation whose start is not one screen: a function no screen
/// reaches, or a component so many screens render (a top bar) that drawing
/// its navigation from each would repeat the same arrows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireScreenOrigin {
    pub id: String,
    pub node: WireNodeRef,
    pub outgoing: usize,
    /// For shared chrome: how many screens render it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_by: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireScreenSite {
    pub file: String,
    pub line: i64,
    /// The href as written at the call, `${…}` for interpolations.
    pub href: String,
    /// `push`, `replace`, `navigate`, `link`…, or `return` for a helper's
    /// return value.
    pub method: String,
    /// The conditions this site runs under: the chain's, then its own.
    pub when: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireScreenLink {
    pub id: String,
    /// A screen id, or an origin id.
    pub from: String,
    /// Always a screen id.
    pub to: String,
    pub from_origin: bool,
    /// The symbols the transition passes through, from just below the
    /// screen's component down to the one that holds the navigation call.
    pub via: Vec<WireNodeRef>,
    /// The conditions along the whole chain; '' when unconditional.
    pub when: String,
    pub sites: Vec<WireScreenSite>,
    /// The destination came back from a helper's return value.
    pub synthesized: bool,
}

// =============================================================================
// Caps
// =============================================================================

/// Hops walked back from a navigation call before giving up on a screen.
const MAX_DEPTH: usize = 7;
/// Callers expanded per node — a hub (`useToast`) is a dead end, not a path.
const MAX_CALLERS_PER_NODE: usize = 30;
/// Nodes visited per navigation.
const MAX_VISITED: usize = 800;
/// Call sites labelled with conditions per request.
const MAX_WHEN_SITES: usize = 600;
/// Importing files read for mentions of a value nothing calls, and mentions
/// taken, per value.
const MAX_MENTION_FILES: usize = 6;
const MAX_MENTIONS: usize = 8;
const MAX_MENTION_FILE_BYTES: u64 = 256 * 1024;
/// A component rendered by at least this many screens is chrome, not a
/// screen's own behaviour.
const SHARED_CHROME_MIN: usize = 3;

/// Edges walked backwards from a navigation call. `contains` because a
/// handler declared inside a screen component is reached from it by
/// containment, not by a call; a `references` edge only when it passes the
/// function as a value (`onPress={handleContinue}`).
const WALK_KINDS: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::Contains,
    EdgeKind::References,
];

fn meta<'e>(edge: &'e Edge, key: &str) -> Option<&'e Value> {
    edge.metadata
        .as_ref()
        .and_then(|metadata| metadata.get(key))
}

/// An edge that arrives from another execution context, never from a
/// caller: a cross-tier hop (a client's fetch onto its route, a queue job
/// onto its consumer) or a React Native event channel. Walked backwards,
/// such an edge leaves the screen, the language and the process, and lands
/// the navigation on whichever screen started the round trip.
fn arrives_from_another_context(edge: &Edge) -> bool {
    if edge.metadata.is_none() {
        return false;
    }
    if meta(edge, "tier").is_some() {
        return true;
    }
    meta(edge, "synthesizedBy").and_then(Value::as_str) == Some("rn-event-channel")
}

/// True when the edge's destination is written at the line the edge points
/// to: an Angular template's (`template: true`), or one registered at the
/// holder's own line.
fn written_here(edge: &Edge, holder: &Node) -> bool {
    if meta(edge, "template").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    match meta(edge, "registeredAt").and_then(Value::as_str) {
        None => edge.provenance.as_deref() != Some("heuristic"),
        Some(at) => edge
            .line
            .is_some_and(|line| at == format!("{}:{line}", holder.file_path)),
    }
}

/// A route a user can be ON, as opposed to one a request goes to: a server
/// route is named with the method that reaches it (`GET /api/orders`), a
/// screen by its path alone; Nuxt's `server/api/` endpoints are named like
/// pages and excluded by path.
fn is_screen_route(route: &Node) -> bool {
    route.name.starts_with('/')
        && !format!("/{}", to_posix_path(&route.file_path)).contains("/server/api/")
}

// =============================================================================
// The endpoint
// =============================================================================

/// One step of a chain: the node, and the edge that led INTO it from the
/// step before.
#[derive(Debug, Clone)]
struct Hop {
    node: Node,
    edge: Option<Edge>,
}

/// Which screen a navigation starts from, and the chain from that screen's
/// component (`path[0]`) down to the holder.
#[derive(Debug, Clone)]
struct Attribution {
    screen: Option<String>,
    path: Vec<Hop>,
}

/// Request-scoped state the walk reads and fills.
struct Walk<'c, 'a> {
    ctx: &'c Ctx<'a>,
    max_bytes: u64,
    /// Component id → every route it serves, in route order.
    screen_of_component: HashMap<String, Vec<String>>,
    /// A file that declares exactly one screen route → that route.
    route_by_file: HashMap<String, String>,
    /// Nodes already read: the components and the navigation holders.
    known: HashMap<String, Node>,
    /// A file's constants and variables, for [`Walk::value_spanning`].
    values_by_file: HashMap<String, Vec<Node>>,
}

pub fn build(ctx: &Ctx<'_>) -> ApiResult<Value> {
    let started = Instant::now();
    let counts = ctx.store.counts()?;
    let index = json!({
        "lastIndexedAt": ctx.store.last_indexed_at()?,
        "edges": counts.edge_count,
        "files": counts.file_count,
    });
    let routes: Vec<Node> = ctx
        .store
        .nodes_of_kind_ordered(NodeKind::Route)?
        .into_iter()
        .filter(is_screen_route)
        .collect();
    let route_ids: Vec<String> = routes.iter().map(|route| route.id.clone()).collect();
    let mut nav_edges = if route_ids.is_empty() {
        Vec::new()
    } else {
        ctx.store
            .incoming_edges_to(&route_ids, &[EdgeKind::Navigates])?
    };
    if nav_edges.is_empty() {
        return Ok(json!({
            "routed": false,
            "entry": Value::Null,
            "screens": [],
            "origins": [],
            "links": [],
            "dropped": 0,
            "index": index,
            "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
        }));
    }

    let route_by_id: HashMap<&str, &Node> = routes
        .iter()
        .map(|route| (route.id.as_str(), route))
        .collect();
    // A file that declares exactly ONE route says a component it defines
    // belongs to that screen; a file holding several says nothing.
    let mut routes_per_file: HashMap<&str, usize> = HashMap::new();
    for route in &routes {
        *routes_per_file.entry(route.file_path.as_str()).or_default() += 1;
    }
    let route_by_file: HashMap<String, String> = routes
        .iter()
        .filter(|route| routes_per_file.get(route.file_path.as_str()) == Some(&1))
        .map(|route| (route.file_path.clone(), route.id.clone()))
        .collect();
    let roots = route_roots(ctx, &routes)?;
    let mut component_of: HashMap<&str, Node> = HashMap::new();
    // Component → EVERY route it serves: one component rendered at several
    // addresses navigates from each of them.
    let mut screen_of_component: HashMap<String, Vec<String>> = HashMap::new();
    for route in &routes {
        let Some(root) = roots.get(&route.id) else {
            continue;
        };
        if root.inline {
            continue;
        }
        component_of.insert(route.id.as_str(), root.node.clone());
        screen_of_component
            .entry(root.node.id.clone())
            .or_default()
            .push(route.id.clone());
    }
    // A layout serves every screen nested in it.
    for (route_id, layouts) in route_layouts(ctx, &routes)? {
        for layout in layouts {
            let serves = screen_of_component.entry(layout).or_default();
            if !serves.contains(&route_id) {
                serves.push(route_id.clone());
            }
        }
    }
    let mut wanted: Vec<String> = component_of.values().map(|node| node.id.clone()).collect();
    wanted.extend(nav_edges.iter().map(|edge| edge.source.clone()));
    let known = ctx.store.nodes_by_ids(&wanted)?;
    // The navigations in source order: the holder's file, the line, the
    // column, then the target.
    nav_edges.sort_by(|a, b| {
        let file = |edge: &Edge| {
            known
                .get(&edge.source)
                .map(|node| to_posix_path(&node.file_path))
                .unwrap_or_default()
        };
        file(a)
            .cmp(&file(b))
            .then(a.line.cmp(&b.line))
            .then(a.col.cmp(&b.col))
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.provenance.cmp(&b.provenance))
            .then_with(|| {
                let text = |edge: &Edge| edge.metadata.as_ref().map(Value::to_string);
                text(a).cmp(&text(b))
            })
    });

    let mut walk = Walk {
        ctx,
        max_bytes: max_file_size(ctx),
        screen_of_component,
        route_by_file,
        known,
        values_by_file: HashMap::new(),
    };
    let mut reader = SiteReader::new(ctx, MAX_WHEN_SITES);
    // Lookups only; the output lists are sorted below.
    let mut links: HashMap<String, WireScreenLink> = HashMap::new();
    let mut origins: HashMap<String, WireScreenOrigin> = HashMap::new();
    let mut counts_by_screen: HashMap<String, (usize, usize)> = HashMap::new();
    let mut dropped = 0usize;

    for nav in &nav_edges {
        let Some(target) = route_by_id.get(nav.target.as_str()).copied() else {
            continue;
        };
        let Some(mut holder) = walk.known.get(&nav.source).cloned() else {
            continue;
        };
        // A navigation the file scope holds — inside the arrow a wrapper is
        // handed at a file's top level, which is no node of its own —
        // belongs to the value that spans it.
        if holder.kind == NodeKind::File
            && let Some(line) = nav.line
        {
            let Some(value) = walk.value_spanning(&holder.file_path, line)? else {
                continue;
            };
            walk.known.insert(value.id.clone(), value.clone());
            holder = value;
        }
        let href = meta(nav, "href")
            .and_then(Value::as_str)
            .map_or_else(|| target.name.clone(), str::to_string);
        // How the destination got here: written at the call, it keeps its
        // own verb; returned from a helper elsewhere, it reads `return`.
        let method = match meta(nav, "navMethod").and_then(Value::as_str) {
            Some(verb) if written_here(nav, &holder) => verb.to_string(),
            _ if nav.provenance.as_deref() == Some("heuristic") => "return".to_string(),
            _ => "push".to_string(),
        };
        let own_when = reader.when(&holder.file_path, holder.language, nav.line, nav.col);
        let site_file = to_posix_path(&holder.file_path);
        let site_line = nav.line.unwrap_or(holder.start_line);

        let Some(starts) = walk.attribute(&holder)? else {
            dropped += 1;
            continue;
        };
        let starts = collapse_shared_chrome(starts, &mut origins);
        let attributions = if starts.is_empty() {
            vec![Attribution {
                screen: None,
                path: vec![Hop {
                    node: holder.clone(),
                    edge: None,
                }],
            }]
        } else {
            starts
        };

        for start in attributions {
            let (from_id, from_origin) = match &start.screen {
                Some(screen) => (screen.clone(), false),
                None => {
                    // The origin is the chain's head: the holder itself, or
                    // the shared component the chain was collapsed onto.
                    let head = &start.path[0].node;
                    origins
                        .entry(head.id.clone())
                        .or_insert_with(|| WireScreenOrigin {
                            id: head.id.clone(),
                            node: to_node_ref(head),
                            outgoing: 0,
                            shared_by: None,
                        });
                    (head.id.clone(), true)
                }
            };
            // `path[i].edge` is the call from `path[i - 1]` into `path[i]`,
            // so its site is in `path[i - 1]`'s file. The component itself
            // is not "via" — it IS the screen.
            let via: Vec<WireNodeRef> = start.path[1..]
                .iter()
                .map(|hop| to_node_ref(&hop.node))
                .collect();
            let mut whens: Vec<String> = Vec::new();
            for pair in start.path.windows(2) {
                let Some(edge) = pair[1].edge.as_ref() else {
                    continue;
                };
                let caller = &pair[0].node;
                let when = reader.when(&caller.file_path, caller.language, edge.line, edge.col);
                if !when.is_empty() && !whens.contains(&when) {
                    whens.push(when);
                }
            }
            if !own_when.is_empty() && !whens.contains(&own_when) {
                whens.push(own_when.clone());
            }
            let mine = whens.join(" && ");
            let site = WireScreenSite {
                file: site_file.clone(),
                line: site_line,
                href: href.clone(),
                method: method.clone(),
                when: mine.clone(),
            };
            let via_key = via
                .iter()
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>()
                .join(">");
            let id = format!("{from_id}\u{0}{}\u{0}{via_key}", target.id);
            if let Some(existing) = links.get_mut(&id) {
                existing.sites.push(site);
                if mine != existing.when {
                    // `if (x) push(A) else push(A)`: both arms are "always".
                    if complementary(&mine, &existing.when) {
                        existing.when = String::new();
                    } else if !mine.is_empty() && !existing.when.is_empty() {
                        existing.when = format!("{} || {mine}", existing.when);
                    } else if mine.is_empty() {
                        existing.when = String::new();
                    }
                }
                continue;
            }
            links.insert(
                id.clone(),
                WireScreenLink {
                    id,
                    from: from_id.clone(),
                    to: target.id.clone(),
                    from_origin,
                    via,
                    when: mine,
                    sites: vec![site],
                    synthesized: nav.provenance.as_deref() == Some("heuristic"),
                },
            );
            counts_by_screen.entry(target.id.clone()).or_default().0 += 1;
            if from_origin {
                if let Some(origin) = origins.get_mut(&from_id) {
                    origin.outgoing += 1;
                }
            } else {
                counts_by_screen.entry(from_id).or_default().1 += 1;
            }
        }
    }

    let mut screens: Vec<WireScreen> = routes
        .iter()
        .map(|route| {
            let (incoming, outgoing) = counts_by_screen.get(&route.id).copied().unwrap_or_default();
            WireScreen {
                id: route.id.clone(),
                path: route.name.clone(),
                file: to_posix_path(&route.file_path),
                line: route.start_line,
                component: component_of.get(route.id.as_str()).map(to_node_ref),
                incoming,
                outgoing,
            }
        })
        .collect();
    screens.sort_by(|a, b| locale_compare(&a.path, &b.path).then_with(|| a.id.cmp(&b.id)));
    let entry = screens
        .iter()
        .find(|screen| screen.path == "/")
        .map(|screen| screen.id.clone());
    let mut origins: Vec<WireScreenOrigin> = origins.into_values().collect();
    origins
        .sort_by(|a, b| locale_compare(&a.node.name, &b.node.name).then_with(|| a.id.cmp(&b.id)));
    let mut links: Vec<WireScreenLink> = links.into_values().collect();
    links.sort_by(|a, b| locale_compare(&a.id, &b.id));
    Ok(json!({
        "routed": true,
        "entry": entry,
        "screens": screens,
        "origins": origins,
        "links": links,
        "dropped": dropped,
        "index": index,
        "timing": { "elapsedMs": started.elapsed().as_millis() as u64 },
    }))
}

// =============================================================================
// Attribution: which screen does this navigation start from?
// =============================================================================

/// The walk's parent links, in the order the nodes were first reached:
/// node → the node it leads to (towards the holder) and the edge that does.
struct Parents {
    order: Vec<String>,
    by_id: HashMap<String, (Option<String>, Option<Edge>)>,
}

impl Parents {
    fn new(holder: &str) -> Self {
        let mut by_id = HashMap::new();
        by_id.insert(holder.to_string(), (None, None));
        Self {
            order: vec![holder.to_string()],
            by_id,
        }
    }

    fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn set(&mut self, id: &str, prev: &str, edge: &Edge) {
        if self
            .by_id
            .insert(id.to_string(), (Some(prev.to_string()), Some(edge.clone())))
            .is_none()
        {
            self.order.push(id.to_string());
        }
    }

    /// The chain from `start` down to the holder, following the links.
    fn path_from(&self, start: &str, nodes: &HashMap<String, Node>) -> Vec<Hop> {
        let mut out = Vec::new();
        let mut id = Some(start.to_string());
        let mut edge_into: Option<Edge> = None;
        while let Some(current) = id {
            let Some(node) = nodes.get(&current) else {
                break;
            };
            out.push(Hop {
                node: node.clone(),
                edge: edge_into.take(),
            });
            let Some((prev, edge)) = self.by_id.get(&current) else {
                break;
            };
            edge_into = edge.clone();
            id = prev.clone();
        }
        out
    }
}

impl Walk<'_, '_> {
    /// Every screen whose component reaches `holder`, each with the
    /// shortest chain (breadth-first). `Some([])` when none does within the
    /// caps but the walk completed; `None` when the walk was cut short — a
    /// hub so wide the answer would be a guess.
    fn attribute(&mut self, holder: &Node) -> ApiResult<Option<Vec<Attribution>>> {
        // The holder IS a screen component: the transition starts on that
        // screen — on each of them, when one component has several
        // addresses.
        if let Some(own) = self.screen_of_component.get(&holder.id) {
            return Ok(Some(
                own.iter()
                    .map(|screen| Attribution {
                        screen: Some(screen.clone()),
                        path: vec![Hop {
                            node: holder.clone(),
                            edge: None,
                        }],
                    })
                    .collect(),
            ));
        }
        let mut parents = Parents::new(&holder.id);
        let mut nodes: HashMap<String, Node> = HashMap::new();
        nodes.insert(holder.id.clone(), holder.clone());
        let mut frontier: Vec<String> = vec![holder.id.clone()];
        let mut found: Vec<Attribution> = Vec::new();
        let mut truncated = false;

        for _ in 0..MAX_DEPTH {
            if frontier.is_empty() {
                break;
            }
            let incoming = self.ctx.store.incoming_edges_to(&frontier, WALK_KINDS)?;
            // Lookups only: the layer is read in frontier order below.
            let mut by_target: HashMap<String, Vec<Edge>> = HashMap::new();
            for edge in incoming {
                if edge.kind == EdgeKind::References
                    && meta(&edge, "fnRef").and_then(Value::as_bool) != Some(true)
                {
                    continue;
                }
                if arrives_from_another_context(&edge) {
                    continue;
                }
                by_target.entry(edge.target.clone()).or_default().push(edge);
            }
            // A value nothing calls is used wherever a function in a file
            // that imports it names it: those mentions are its callers.
            for id in &frontier {
                let Some(value) = nodes.get(id) else {
                    continue;
                };
                if !matches!(value.kind, NodeKind::Constant | NodeKind::Variable) {
                    continue;
                }
                let has_callers = by_target
                    .get(id)
                    .is_some_and(|edges| edges.iter().any(|edge| edge.kind != EdgeKind::Contains));
                if has_callers {
                    continue;
                }
                let mentions = self.mentions_of(value)?;
                if !mentions.is_empty() {
                    by_target.insert(id.clone(), mentions);
                }
            }
            // Each target's callers in a total order.
            for edges in by_target.values_mut() {
                edges.sort_by(|a, b| {
                    a.source
                        .cmp(&b.source)
                        .then(a.line.cmp(&b.line))
                        .then(a.col.cmp(&b.col))
                        .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
                });
            }
            let layer: Vec<&Vec<Edge>> =
                frontier.iter().filter_map(|id| by_target.get(id)).collect();
            let mut wanted: Vec<String> = Vec::new();
            let mut wanted_set: HashSet<&str> = HashSet::new();
            for edges in &layer {
                if edges.len() > MAX_CALLERS_PER_NODE {
                    truncated = true;
                    continue;
                }
                for edge in edges.iter() {
                    if !parents.contains(&edge.source) && wanted_set.insert(edge.source.as_str()) {
                        wanted.push(edge.source.clone());
                    }
                }
            }
            if parents.len() + wanted.len() > MAX_VISITED {
                truncated = true;
            }
            let fetched = if wanted.is_empty() {
                HashMap::new()
            } else {
                self.ctx.store.nodes_by_ids(&wanted)?
            };
            let mut next: Vec<String> = Vec::new();
            for edges in &layer {
                if edges.len() > MAX_CALLERS_PER_NODE {
                    continue;
                }
                for edge in edges.iter() {
                    if parents.contains(&edge.source) {
                        continue;
                    }
                    let Some(caller) = fetched
                        .get(&edge.source)
                        .or_else(|| self.known.get(&edge.source))
                    else {
                        continue;
                    };
                    // A file's top level or a route node is not a place a
                    // user is.
                    if matches!(caller.kind, NodeKind::File | NodeKind::Route) {
                        continue;
                    }
                    parents.set(&edge.source, &edge.target, edge);
                    nodes.insert(edge.source.clone(), caller.clone());
                    if let Some(screens) = self.screen_of_component.get(&caller.id) {
                        let path = parents.path_from(&caller.id, &nodes);
                        for screen in screens {
                            found.push(Attribution {
                                screen: Some(screen.clone()),
                                path: path.clone(),
                            });
                        }
                        // A screen is where the walk stops.
                        continue;
                    }
                    next.push(edge.source.clone());
                    if parents.len() >= MAX_VISITED {
                        break;
                    }
                }
            }
            frontier = next;
            if parents.len() >= MAX_VISITED {
                truncated = true;
                break;
            }
        }
        if !found.is_empty() {
            return Ok(Some(found));
        }
        // No screen component reached, but the chain passed through a
        // screen's FILE: a component that file defines for itself (a wrapper
        // the render synthesizer did not see through) belongs to that
        // screen. Nearest first, so the holder's own file wins.
        for id in &parents.order {
            let screen = nodes
                .get(id)
                .and_then(|node| self.route_by_file.get(&node.file_path));
            if let Some(screen) = screen {
                return Ok(Some(vec![Attribution {
                    screen: Some(screen.clone()),
                    path: parents.path_from(id, &nodes),
                }]));
            }
        }
        Ok(if truncated { None } else { Some(Vec::new()) })
    }

    /// Synthetic `references` edges from the functions that mention `value`
    /// by name in the files importing it (an import or `export {…}` line
    /// excepted), read from each file's current source through the viewer's
    /// file chokepoint: a file that drifted since the index is not read.
    fn mentions_of(&self, value: &Node) -> ApiResult<Vec<Edge>> {
        let mut importers: Vec<String> = self
            .ctx
            .store
            .incoming_edges_to(std::slice::from_ref(&value.id), &[EdgeKind::Imports])?
            .into_iter()
            .map(|edge| edge.source)
            .collect();
        importers.sort();
        importers.dedup();
        importers.truncate(MAX_MENTION_FILES);
        let mut out: Vec<Edge> = Vec::new();
        if importers.is_empty() {
            return Ok(out);
        }
        let files = self.ctx.store.nodes_by_ids(&importers)?;
        for importer in &importers {
            let Some(file) = files.get(importer) else {
                continue;
            };
            if file.kind != NodeKind::File {
                continue;
            }
            let Some((record, stored)) = find_indexed_file(self.ctx, &file.file_path)? else {
                continue;
            };
            if has_drifted_on_disk(self.ctx, &stored, &record, self.max_bytes) {
                continue;
            }
            let Some(text) = read_indexed_file_text(self.ctx, &stored, MAX_MENTION_FILE_BYTES)
            else {
                continue;
            };
            let functions: Vec<Node> = self
                .ctx
                .store
                .nodes_in_files(std::slice::from_ref(&file.file_path))?
                .into_iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::Function | NodeKind::Method | NodeKind::Component
                    )
                })
                .collect();
            for (index, line) in text.split('\n').enumerate() {
                if out.len() >= MAX_MENTIONS {
                    break;
                }
                if !mentions_word(line, &value.name) || is_import_line(line) {
                    continue;
                }
                let at = index as i64 + 1;
                let mut best: Option<&Node> = None;
                for function in &functions {
                    if function.start_line <= at
                        && function.end_line >= at
                        && best.is_none_or(|best| function.start_line >= best.start_line)
                    {
                        best = Some(function);
                    }
                }
                let Some(best) = best else {
                    continue;
                };
                if best.id == value.id {
                    continue;
                }
                out.push(Edge {
                    id: None,
                    source: best.id.clone(),
                    target: value.id.clone(),
                    kind: EdgeKind::References,
                    metadata: Some(json!({ "fnRef": true, "mention": true })),
                    line: Some(at),
                    col: None,
                    provenance: Some("heuristic".to_string()),
                });
            }
        }
        Ok(out)
    }

    /// The smallest constant or variable of a file whose lines contain
    /// `line`.
    fn value_spanning(&mut self, file_path: &str, line: i64) -> ApiResult<Option<Node>> {
        if !self.values_by_file.contains_key(file_path) {
            let values: Vec<Node> = self
                .ctx
                .store
                .nodes_in_files(&[file_path.to_string()])?
                .into_iter()
                .filter(|node| matches!(node.kind, NodeKind::Constant | NodeKind::Variable))
                .collect();
            self.values_by_file.insert(file_path.to_string(), values);
        }
        let mut best: Option<&Node> = None;
        for value in &self.values_by_file[file_path] {
            if value.start_line <= line
                && value.end_line >= line
                && best.is_none_or(|best| value.start_line >= best.start_line)
            {
                best = Some(value);
            }
        }
        Ok(best.cloned())
    }
}

/// Whether `line` names `name` as a whole word — not inside a longer name,
/// not as a member (`x.name`): upstream's `(?<![\w$.])name(?![\w$])`,
/// checked by hand.
fn mentions_word(line: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$';
    line.match_indices(name).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + name.len()..].chars().next();
        !before.is_some_and(|ch| word(ch) || ch == '.') && !after.is_some_and(word)
    })
}

/// `/^\s*import\b|^\s*export\s*\{/`.
fn is_import_line(line: &str) -> bool {
    let line = line.trim_start();
    if let Some(rest) = line.strip_prefix("import")
        && !rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return true;
    }
    line.strip_prefix("export")
        .is_some_and(|rest| rest.trim_start().starts_with('{'))
}

/// Shared chrome: when the same first-hop component carries a navigation to
/// [`SHARED_CHROME_MIN`] or more screens, those attributions collapse into
/// ONE from that component, marked with how many screens render it. A top
/// bar's link is one fact about the top bar, not twelve about twelve screens.
fn collapse_shared_chrome(
    starts: Vec<Attribution>,
    origins: &mut HashMap<String, WireScreenOrigin>,
) -> Vec<Attribution> {
    // First hop → the attributions through it, in the order they came.
    let mut order: Vec<String> = Vec::new();
    let mut by_first_hop: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, start) in starts.iter().enumerate() {
        if start.screen.is_none() || start.path.len() < 2 {
            continue;
        }
        let key = start.path[1].node.id.clone();
        by_first_hop
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key);
                Vec::new()
            })
            .push(index);
    }
    let mut out: Vec<Attribution> = Vec::new();
    let mut collapsed: HashSet<usize> = HashSet::new();
    for key in &order {
        let group = &by_first_hop[key];
        // Counted by the screen COMPONENT the chain starts at, not by the
        // address: one component serving four routes is one screen.
        let screens: HashSet<&str> = group
            .iter()
            .map(|index| starts[*index].path[0].node.id.as_str())
            .collect();
        if screens.len() < SHARED_CHROME_MIN {
            continue;
        }
        let first = &starts[group[0]];
        let head = &first.path[1].node;
        let shared = screens.len();
        origins
            .entry(head.id.clone())
            .and_modify(|origin| origin.shared_by = Some(origin.shared_by.unwrap_or(0).max(shared)))
            .or_insert_with(|| WireScreenOrigin {
                id: head.id.clone(),
                node: to_node_ref(head),
                outgoing: 0,
                shared_by: Some(shared),
            });
        // One attribution, headed by the shared component, the chain
        // continuing below it.
        out.push(Attribution {
            screen: None,
            path: first.path[1..].to_vec(),
        });
        collapsed.extend(group.iter().copied());
    }
    out.extend(
        starts
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !collapsed.contains(index))
            .map(|(_, start)| start),
    );
    out
}

// =============================================================================
// Conditions
// =============================================================================

/// `x` and `!x`, or `a && x` and `a && !x`: the two arms of one condition.
fn complementary(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let pa: Vec<&str> = a.split(" && ").collect();
    let pb: Vec<&str> = b.split(" && ").collect();
    if pa.len() != pb.len() {
        return false;
    }
    let mut flips = 0;
    for (x, y) in pa.iter().zip(&pb) {
        if x == y {
            continue;
        }
        if x.strip_prefix('!') == Some(*y) || y.strip_prefix('!') == Some(*x) {
            flips += 1;
        } else {
            return false;
        }
    }
    flips == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_arms_of_one_condition_are_complementary_and_nothing_else_is() {
        assert!(complementary("x", "!x"));
        assert!(complementary("a && !x", "a && x"));
        assert!(!complementary("a && x", "b && !x"));
        assert!(!complementary("x", "x"));
        assert!(!complementary("", "!x"));
        assert!(!complementary("a && x", "!a && !x"));
    }

    #[test]
    fn a_mention_is_a_whole_name_outside_an_import() {
        assert!(mentions_word(
            "  const s = useActionState(signIn, {})",
            "signIn"
        ));
        assert!(!mentions_word("  const s = signInNow()", "signIn"));
        assert!(!mentions_word("  form.signIn()", "signIn"));
        assert!(!mentions_word("  $signIn()", "signIn"));
        assert!(mentions_word("signIn", "signIn"));
        assert!(is_import_line("import { signIn } from './actions'"));
        assert!(is_import_line("  export { signIn }"));
        assert!(!is_import_line("const imported = signIn"));
        assert!(!is_import_line("importer(signIn)"));
    }
}
