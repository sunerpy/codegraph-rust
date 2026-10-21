//! Where a route's code starts: the symbol that runs when a request arrives at
//! `POST /users`, or when a navigation lands on `/capture/review`. Upstream
//! `src/ui-server/api/route-roots.ts`; the routes list (upstream #2264) is its
//! reader here, the Steps and Screens pictures that also read it upstream are
//! a later port.
//!
//! Every framework resolver binds a route node to what serves it, but not with
//! the same edge: a file-routed screen draws a `calls` edge to the component
//! its file exports; a resolver that names the handler at the registration
//! (NestJS, React Router here; Spring, FastAPI, Express upstream) draws a
//! `references` edge to it; a route whose handler is an inline arrow has no
//! handler node at all, and its body's calls are the route's own. One answer
//! per route, in order of evidence:
//!
//! 1. the target of a `references` edge that is a function, a method, a class
//!    or another handler kind: the handler the resolver named;
//! 2. the target of a `calls` / `instantiates` edge that is a component: the
//!    page a file-routed screen exports;
//! 3. the route itself, when it carries `calls` edges and nothing above: the
//!    inline handler, which the route stands in for;
//! 4. nothing.

use std::collections::HashMap;

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};

use super::Ctx;
use crate::respond::ApiResult;

/// Where a walk from a route starts.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteRoot {
    /// The symbol that serves the route; the route itself for an inline handler.
    pub node: Node,
    /// The handler is an anonymous function at the registration site, and the
    /// route node stands in for it.
    pub inline: bool,
}

/// What a resolver's `references` edge may name as the handler. A constant or
/// a variable counts: `const authUser = asyncHandler(async (req, res) => …)`
/// is a handler written with a wrapper, and the binding is all that has a node.
const HANDLER_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Class,
    NodeKind::Component,
    NodeKind::Constant,
    NodeKind::Variable,
];

/// A React component, by the convention that names one: a component node, or
/// a PascalCase function in a JavaScript-family file.
pub fn looks_like_component(node: &Node) -> bool {
    match node.kind {
        NodeKind::Component => true,
        NodeKind::Function => {
            matches!(
                node.language,
                Language::JavaScript | Language::TypeScript | Language::Tsx | Language::Jsx
            ) && node.name.starts_with(|c: char| c.is_ascii_uppercase())
        }
        _ => false,
    }
}

/// A layout the route renders inside (an Angular parent route's component) is
/// not the route's own handler.
fn is_layout(edge: &Edge) -> bool {
    edge.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("layout"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

fn handler_rank(node: &Node) -> u8 {
    match node.kind {
        NodeKind::Function | NodeKind::Method => 0,
        NodeKind::Component => 1,
        NodeKind::Class => 2,
        _ => 3,
    }
}

/// Route id → where its code starts, from the routes' outgoing `references`,
/// `calls` and `instantiates` edges and the nodes they point at. Ties go to
/// the earlier line, then the smaller id, so the answer never depends on the
/// order the store returned the edges in.
pub fn roots_from(
    routes: &[Node],
    edges: &[Edge],
    targets: &HashMap<String, Node>,
) -> HashMap<String, RouteRoot> {
    let mut by_route: HashMap<&str, Vec<&Edge>> = HashMap::new();
    for edge in edges {
        by_route.entry(edge.source.as_str()).or_default().push(edge);
    }
    let mut out = HashMap::new();
    for route in routes {
        let Some(list) = by_route.get(route.id.as_str()) else {
            continue;
        };
        // 1. The handler the resolver named.
        let mut named: Vec<&Node> = list
            .iter()
            .filter(|edge| edge.kind == EdgeKind::References && !is_layout(edge))
            .filter_map(|edge| targets.get(&edge.target))
            .filter(|node| HANDLER_KINDS.contains(&node.kind) && node.id != route.id)
            .collect();
        named.sort_by(|a, b| {
            handler_rank(a)
                .cmp(&handler_rank(b))
                .then(a.start_line.cmp(&b.start_line))
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(node) = named.first() {
            out.insert(
                route.id.clone(),
                RouteRoot {
                    node: (*node).clone(),
                    inline: false,
                },
            );
            continue;
        }
        // 2. The component a screen file exports.
        let mut rendered: Vec<&Node> = list
            .iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls | EdgeKind::Instantiates))
            .filter_map(|edge| targets.get(&edge.target))
            .filter(|node| looks_like_component(node) && node.id != route.id)
            .collect();
        rendered.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(node) = rendered.first() {
            out.insert(
                route.id.clone(),
                RouteRoot {
                    node: (*node).clone(),
                    inline: false,
                },
            );
            continue;
        }
        // 3. The inline handler: the route's own calls are the body's.
        let calls_out = list.iter().any(|edge| {
            edge.kind == EdgeKind::Calls
                && targets
                    .get(&edge.target)
                    .is_none_or(|target| target.kind != NodeKind::File)
        });
        if calls_out {
            out.insert(
                route.id.clone(),
                RouteRoot {
                    node: route.clone(),
                    inline: true,
                },
            );
        }
    }
    out
}

/// Route id → where its code starts, for every route that has an answer.
pub fn route_roots(ctx: &Ctx<'_>, routes: &[Node]) -> ApiResult<HashMap<String, RouteRoot>> {
    if routes.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<String> = routes.iter().map(|route| route.id.clone()).collect();
    let edges = ctx.store.outgoing_edges_from(
        &ids,
        &[
            EdgeKind::References,
            EdgeKind::Calls,
            EdgeKind::Instantiates,
        ],
    )?;
    if edges.is_empty() {
        return Ok(HashMap::new());
    }
    let target_ids: Vec<String> = edges.iter().map(|edge| edge.target.clone()).collect();
    let targets = ctx.store.nodes_by_ids(&target_ids)?;
    Ok(roots_from(routes, &edges, &targets))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: NodeKind, name: &str, line: i64, language: Language) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/app.ts".to_string(),
            language,
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind,
            metadata: None,
            line: None,
            col: None,
            provenance: None,
        }
    }

    fn ts(id: &str, kind: NodeKind, name: &str, line: i64) -> Node {
        node(id, kind, name, line, Language::TypeScript)
    }

    fn by_id(nodes: &[Node]) -> HashMap<String, Node> {
        nodes.iter().map(|n| (n.id.clone(), n.clone())).collect()
    }

    #[test]
    fn a_named_handler_wins_over_the_calls_beside_it() {
        let route = ts("route:a", NodeKind::Route, "GET /users", 3);
        let targets = by_id(&[
            ts("function:list", NodeKind::Function, "listUsers", 9),
            ts("function:log", NodeKind::Function, "log", 1),
        ]);
        let edges = [
            edge("route:a", "function:log", EdgeKind::Calls),
            edge("route:a", "function:list", EdgeKind::References),
        ];
        let roots = roots_from(std::slice::from_ref(&route), &edges, &targets);
        assert_eq!(roots["route:a"].node.name, "listUsers");
        assert!(!roots["route:a"].inline);
    }

    #[test]
    fn a_route_with_only_calls_is_its_own_inline_handler() {
        let route = ts("route:b", NodeKind::Route, "GET /users/:id/card", 11);
        let targets = by_id(&[
            ts("function:load", NodeKind::Function, "loadUser", 4),
            ts("function:audit", NodeKind::Function, "audit", 5),
        ]);
        let edges = [
            edge("route:b", "function:load", EdgeKind::Calls),
            edge("route:b", "function:audit", EdgeKind::Calls),
        ];
        let roots = roots_from(std::slice::from_ref(&route), &edges, &targets);
        assert_eq!(roots["route:b"].node.id, "route:b");
        assert!(roots["route:b"].inline);
        // A call into a file is not a body, and a route with no edges has no answer.
        let file = ts("file:src/a.ts", NodeKind::File, "a.ts", 1);
        let only_file = [edge("route:b", "file:src/a.ts", EdgeKind::Calls)];
        assert!(roots_from(&[route], &only_file, &by_id(&[file])).is_empty());
    }

    #[test]
    fn a_file_routed_screen_starts_at_the_component_it_renders() {
        let route = ts("route:c", NodeKind::Route, "/about", 1);
        let page = node(
            "function:page",
            NodeKind::Function,
            "AboutPage",
            7,
            Language::Tsx,
        );
        let helper = node(
            "function:fmt",
            NodeKind::Function,
            "format",
            2,
            Language::Tsx,
        );
        let edges = [
            edge("route:c", "function:fmt", EdgeKind::Calls),
            edge("route:c", "function:page", EdgeKind::Calls),
        ];
        let roots = roots_from(&[route], &edges, &by_id(&[page, helper]));
        assert_eq!(roots["route:c"].node.name, "AboutPage");
        assert!(!roots["route:c"].inline);
    }

    #[test]
    fn a_layout_is_not_the_handler_and_ties_go_by_line_then_id() {
        let route = ts("route:d", NodeKind::Route, "/settings", 1);
        let mut layout = edge("route:d", "class:shell", EdgeKind::References);
        layout.metadata = Some(serde_json::json!({ "layout": true }));
        let targets = by_id(&[
            ts("class:shell", NodeKind::Class, "ShellComponent", 1),
            ts("method:b", NodeKind::Method, "show", 20),
            ts("method:a", NodeKind::Method, "show", 20),
            ts("class:settings", NodeKind::Class, "Settings", 2),
        ]);
        let edges = [
            layout,
            edge("route:d", "class:settings", EdgeKind::References),
            edge("route:d", "method:b", EdgeKind::References),
            edge("route:d", "method:a", EdgeKind::References),
        ];
        let roots = roots_from(&[route], &edges, &targets);
        // A method outranks a class, and of two at one line the smaller id wins.
        assert_eq!(roots["route:d"].node.id, "method:a");
    }

    #[test]
    fn a_pascal_case_function_is_a_component_only_in_a_javascript_family_file() {
        let lang = |language| node("f", NodeKind::Function, "Page", 1, language);
        assert!(looks_like_component(&lang(Language::Tsx)));
        assert!(looks_like_component(&lang(Language::JavaScript)));
        assert!(!looks_like_component(&lang(Language::Python)));
        assert!(!looks_like_component(&node(
            "f",
            NodeKind::Function,
            "page",
            1,
            Language::Tsx
        )));
        assert!(looks_like_component(&ts("c", NodeKind::Component, "x", 1)));
    }
}
