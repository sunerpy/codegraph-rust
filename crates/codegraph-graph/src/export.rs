//! NetworkX node-link export of the full code graph.
//!
//! Produces the `graph.json` shape downstream tools (e.g. opsx
//! `agents_md_generator`) consume: `{nodes:[{id,label,file_type,source_file,
//! kind,...}], links:[{source,target,relation,...}], edges:[...], directed,
//! multigraph, graph}`. `links` is the canonical NetworkX `node_link_data` key;
//! `edges` is a duplicate alias for readers that expect it. Deterministic
//! (nodes/edges read in id order).

use std::collections::HashMap;

use codegraph_core::types::{Node, NodeKind};
use codegraph_store::Store;
use serde_json::{json, Map, Value};

use crate::centrality::{self, Centrality};

/// Build the full-graph node-link JSON document with centrality scores.
pub fn node_link_graph(store: &Store) -> anyhow::Result<Value> {
    node_link_graph_opts(store, true)
}

/// Build the full-graph node-link JSON document. When `with_centrality` is
/// false, the PageRank/degree pass is skipped (faster on huge graphs); nodes
/// then carry no `god_score`/`pagerank`/`in_degree`/`out_degree`.
pub fn node_link_graph_opts(store: &Store, with_centrality: bool) -> anyhow::Result<Value> {
    let nodes = store.all_nodes()?;
    let edges = store.all_edges()?;

    let scores = if with_centrality {
        centrality::compute(&nodes, &edges)
    } else {
        HashMap::new()
    };

    let node_values: Vec<Value> = nodes
        .iter()
        .map(|node| node_to_link_node(node, scores.get(&node.id)))
        .collect();
    let link_values: Vec<Value> = edges
        .iter()
        .map(|edge| {
            let mut m = Map::new();
            m.insert("source".into(), json!(edge.source));
            m.insert("target".into(), json!(edge.target));
            m.insert("relation".into(), json!(edge.kind.as_str()));
            m.insert("kind".into(), json!(edge.kind.as_str()));
            if let Some(line) = edge.line {
                m.insert("line".into(), json!(line));
            }
            if let Some(metadata) = &edge.metadata {
                m.insert("metadata".into(), metadata.clone());
            }
            Value::Object(m)
        })
        .collect();

    Ok(json!({
        "directed": true,
        "multigraph": true,
        "graph": {},
        "nodes": node_values,
        "links": link_values,
        "edges": link_values,
    }))
}

fn node_to_link_node(node: &Node, centrality: Option<&Centrality>) -> Value {
    let file_type = if node.kind == NodeKind::File {
        "file"
    } else {
        "code"
    };
    let mut m = Map::new();
    m.insert("id".into(), json!(node.id));
    m.insert("label".into(), json!(node.name));
    m.insert("name".into(), json!(node.name));
    m.insert("kind".into(), json!(node.kind.as_str()));
    m.insert("file_type".into(), json!(file_type));
    m.insert("source_file".into(), json!(node.file_path));
    m.insert("qualified_name".into(), json!(node.qualified_name));
    m.insert("language".into(), json!(node.language.as_str()));
    m.insert("start_line".into(), json!(node.start_line));
    m.insert("end_line".into(), json!(node.end_line));
    if let Some(signature) = &node.signature {
        m.insert("signature".into(), json!(signature));
    }
    if let Some(c) = centrality {
        m.insert("pagerank".into(), json!(c.pagerank));
        m.insert("god_score".into(), json!(c.pagerank));
        m.insert("in_degree".into(), json!(c.in_degree));
        m.insert("out_degree".into(), json!(c.out_degree));
    }
    Value::Object(m)
}
