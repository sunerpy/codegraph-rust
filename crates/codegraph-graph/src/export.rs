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
use serde_json::{Map, Value, json};

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

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::{Edge, EdgeKind, Language};

    fn temp_db_path(test_name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!(
            "codegraph-graph-export-{test_name}-{}-{nanos}.db",
            std::process::id()
        ));
        path
    }

    fn node(id: &str, kind: NodeKind, name: &str, file_path: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file_path.to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 3,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: Some("(): void".to_string()),
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 1,
        }
    }

    fn edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind,
            metadata: None,
            line: Some(2),
            col: Some(0),
            provenance: None,
        }
    }

    fn small_store(test_name: &str) -> Store {
        let mut store = Store::open(&temp_db_path(test_name)).expect("open store");
        store
            .upsert_nodes(&[
                node("file:src/a.ts", NodeKind::File, "a.ts", "src/a.ts"),
                node("function:caller", NodeKind::Function, "caller", "src/a.ts"),
                node("function:callee", NodeKind::Function, "callee", "src/a.ts"),
            ])
            .expect("insert nodes");
        store
            .insert_edges(&[
                edge("file:src/a.ts", "function:caller", EdgeKind::Contains),
                edge("function:caller", "function:callee", EdgeKind::Calls),
            ])
            .expect("insert edges");
        store
    }

    #[test]
    fn node_link_graph_with_centrality_has_full_shape_and_scores() {
        let store = small_store("with-centrality");
        let doc = node_link_graph(&store).expect("export");

        assert_eq!(doc["directed"], json!(true));
        assert_eq!(doc["multigraph"], json!(true));
        assert!(doc["graph"].is_object());

        let nodes = doc["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 3);
        // Every node with centrality carries the four score keys.
        for n in nodes {
            assert!(n["pagerank"].is_number(), "pagerank present");
            assert!(n["god_score"].is_number(), "god_score aliases pagerank");
            assert!(n["in_degree"].is_number());
            assert!(n["out_degree"].is_number());
        }

        // `links` is canonical; `edges` is a duplicate alias — both present, equal.
        let links = doc["links"].as_array().expect("links array");
        let edges = doc["edges"].as_array().expect("edges array");
        assert_eq!(links.len(), 2);
        assert_eq!(links, edges);

        // A resolved edge exposes both `relation` and `kind`, plus its line.
        let calls = links
            .iter()
            .find(|l| l["source"] == json!("function:caller"))
            .expect("calls link");
        assert_eq!(calls["relation"], json!("calls"));
        assert_eq!(calls["kind"], json!("calls"));
        assert_eq!(calls["line"], json!(2));
    }

    #[test]
    fn file_node_gets_file_type_code_node_gets_code_type() {
        let store = small_store("file-type");
        let doc = node_link_graph(&store).expect("export");
        let nodes = doc["nodes"].as_array().expect("nodes");

        let file = nodes
            .iter()
            .find(|n| n["id"] == json!("file:src/a.ts"))
            .expect("file node");
        assert_eq!(file["file_type"], json!("file"));
        assert_eq!(file["kind"], json!("file"));

        let func = nodes
            .iter()
            .find(|n| n["id"] == json!("function:caller"))
            .expect("function node");
        assert_eq!(func["file_type"], json!("code"));
        // Signature is emitted when present.
        assert_eq!(func["signature"], json!("(): void"));
        assert_eq!(func["source_file"], json!("src/a.ts"));
        assert_eq!(func["label"], json!("caller"));
    }

    #[test]
    fn node_link_graph_opts_without_centrality_omits_scores() {
        let store = small_store("no-centrality");
        let doc = node_link_graph_opts(&store, false).expect("export");
        let nodes = doc["nodes"].as_array().expect("nodes");
        assert_eq!(nodes.len(), 3);
        for n in nodes {
            assert!(n["pagerank"].is_null(), "no pagerank without centrality");
            assert!(n["god_score"].is_null());
            assert!(n["in_degree"].is_null());
            assert!(n["out_degree"].is_null());
            // Core fields are still emitted.
            assert!(n["id"].is_string());
            assert!(n["language"].is_string());
        }
    }

    #[test]
    fn empty_store_exports_empty_node_and_link_arrays() {
        let store = Store::open(&temp_db_path("empty")).expect("open store");
        let doc = node_link_graph(&store).expect("export");
        assert_eq!(doc["nodes"].as_array().expect("nodes").len(), 0);
        assert_eq!(doc["links"].as_array().expect("links").len(), 0);
        assert_eq!(doc["edges"].as_array().expect("edges").len(), 0);
    }
}
