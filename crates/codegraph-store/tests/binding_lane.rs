//! The two store queries lane B of a sync reads (W7-31): the import bindings
//! other files hold into a module, and the references an importer writes
//! through a binding's local name.

use std::collections::BTreeSet;

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

fn store(label: &str) -> Store {
    let path = std::env::temp_dir().join(format!(
        "codegraph-store-binding-lane-{label}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    Store::open(&path).expect("open store")
}

fn node(id: &str, kind: NodeKind, name: &str, file: &str, language: Language) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: file.to_string(),
        language,
        start_line: 1,
        end_line: 1,
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

fn file_node(file: &str, language: Language) -> Node {
    node(
        &format!("file:{file}"),
        NodeKind::File,
        file,
        file,
        language,
    )
}

fn edge(source: &str, target: &str, kind: EdgeKind, at: Option<(i64, i64)>) -> Edge {
    Edge {
        id: None,
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: None,
        line: at.map(|(line, _)| line),
        col: at.map(|(_, col)| col),
        provenance: None,
    }
}

fn reference(
    from: &str,
    name: &str,
    kind: EdgeKind,
    (line, col): (i64, i64),
    file: &str,
) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line,
        col,
        candidates: None,
        file_path: file.to_string(),
        language: Language::TypeScript,
        is_function_ref: false,
        reference_subkind: None,
    }
}

fn site(from: &str, line: i64, col: i64) -> ReferenceSite {
    ReferenceSite {
        from_node_id: from.to_string(),
        line,
        col,
    }
}

/// The bindings into `src/logging.ts` are the `imports` edges from other
/// files' file nodes to its nodes, with the importer's language; a call
/// into it, the module's own import edge, a synthesized edge and an edge
/// into another file are none. A binding without coordinates gives its
/// importer whole.
#[test]
fn bindings_into_a_module_are_other_files_imports_from_their_file_nodes() {
    let mut store = store("bindings");
    store
        .upsert_nodes(&[
            file_node("src/logging.ts", Language::TypeScript),
            node(
                "constant:logger",
                NodeKind::Constant,
                "logger",
                "src/logging.ts",
                Language::TypeScript,
            ),
            file_node("src/app.ts", Language::TypeScript),
            node(
                "function:run",
                NodeKind::Function,
                "run",
                "src/app.ts",
                Language::TypeScript,
            ),
            file_node("src/View.vue", Language::Vue),
            file_node("src/old.ts", Language::TypeScript),
            file_node("app/main.py", Language::Python),
            file_node("src/other.ts", Language::TypeScript),
            node(
                "constant:other",
                NodeKind::Constant,
                "other",
                "src/other.ts",
                Language::TypeScript,
            ),
        ])
        .unwrap();
    let synthesized = Edge {
        metadata: Some(serde_json::json!({"synthesizedBy": "callback"})),
        ..edge(
            "file:app/main.py",
            "constant:logger",
            EdgeKind::Imports,
            Some((3, 0)),
        )
    };
    store
        .insert_edges(&[
            edge(
                "file:src/app.ts",
                "constant:logger",
                EdgeKind::Imports,
                Some((1, 9)),
            ),
            edge(
                "file:src/app.ts",
                "file:src/logging.ts",
                EdgeKind::Imports,
                Some((1, 0)),
            ),
            edge(
                "function:run",
                "constant:logger",
                EdgeKind::Calls,
                Some((4, 2)),
            ),
            edge(
                "file:src/View.vue",
                "constant:logger",
                EdgeKind::Imports,
                Some((2, 9)),
            ),
            edge(
                "file:src/old.ts",
                "constant:logger",
                EdgeKind::Imports,
                None,
            ),
            edge(
                "file:src/logging.ts",
                "constant:logger",
                EdgeKind::Imports,
                Some((9, 0)),
            ),
            edge(
                "file:src/app.ts",
                "constant:other",
                EdgeKind::Imports,
                Some((2, 9)),
            ),
            synthesized,
        ])
        .unwrap();
    let at = |file: &str, line: i64, col: i64| FileReferenceSite {
        file_path: file.to_string(),
        site: Some(site(&format!("file:{file}"), line, col)),
    };
    assert_eq!(
        store
            .import_binding_sites_into_file("src/logging.ts")
            .unwrap(),
        [
            (at("src/View.vue", 2, 9), Language::Vue),
            (at("src/app.ts", 1, 0), Language::TypeScript),
            (at("src/app.ts", 1, 9), Language::TypeScript),
            (
                FileReferenceSite {
                    file_path: "src/old.ts".to_string(),
                    site: None,
                },
                Language::TypeScript
            ),
        ]
    );
    assert!(
        store
            .import_binding_sites_into_file("src/app.ts")
            .unwrap()
            .is_empty()
    );
}

/// The references written through `logger` in `src/app.ts`: from both
/// tables, of every kind but `imports`, by the whole first segment
/// (`loggerX.info` and `log.info` are not), in that file only.
#[test]
fn references_through_a_name_come_from_both_tables_and_skip_imports() {
    let mut store = store("through");
    store
        .upsert_nodes(&[
            file_node("src/app.ts", Language::TypeScript),
            node(
                "function:run",
                NodeKind::Function,
                "run",
                "src/app.ts",
                Language::TypeScript,
            ),
            file_node("src/other.ts", Language::TypeScript),
            node(
                "function:else",
                NodeKind::Function,
                "elsewhere",
                "src/other.ts",
                Language::TypeScript,
            ),
        ])
        .unwrap();
    let app = "src/app.ts";
    store
        .insert_unresolved_refs(&[
            reference("file:src/app.ts", "logger", EdgeKind::Imports, (1, 9), app),
            reference("function:run", "logger.info", EdgeKind::Calls, (4, 2), app),
            // A function reference (`setTimeout(logger.warn, 1)`), stored
            // under its own kind.
            UnresolvedRef {
                is_function_ref: true,
                ..reference(
                    "function:run",
                    "logger.warn",
                    EdgeKind::References,
                    (5, 13),
                    app,
                )
            },
            reference("function:run", "loggerX.info", EdgeKind::Calls, (6, 2), app),
            reference("function:run", "log.info", EdgeKind::Calls, (7, 2), app),
            reference(
                "file:src/app.ts",
                "logger.child",
                EdgeKind::Imports,
                (8, 0),
                app,
            ),
            reference(
                "function:else",
                "logger.info",
                EdgeKind::Calls,
                (2, 2),
                "src/other.ts",
            ),
        ])
        .unwrap();
    // Every row but the last call of `run` moves into `resolved_refs`, as a
    // resolution pass moves the ones it resolves.
    let resolved: Vec<i64> = store
        .all_unresolved_refs()
        .unwrap()
        .into_iter()
        .filter_map(|reference| reference.id)
        .collect();
    store.delete_resolved_unresolved_refs(&resolved).unwrap();
    store
        .insert_unresolved_refs(&[reference(
            "function:run",
            "logger.info",
            EdgeKind::Calls,
            (9, 2),
            app,
        )])
        .unwrap();
    let names = |names: &[&str]| -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    };
    assert_eq!(
        store
            .stored_refs_through_names(app, &names(&["logger"]))
            .unwrap(),
        [
            site("function:run", 4, 2),
            site("function:run", 5, 13),
            site("function:run", 9, 2)
        ]
    );
    assert_eq!(
        store
            .stored_refs_through_names(app, &names(&["logger", "log"]))
            .unwrap(),
        [
            site("function:run", 4, 2),
            site("function:run", 5, 13),
            site("function:run", 7, 2),
            site("function:run", 9, 2)
        ]
    );
    assert!(
        store
            .stored_refs_through_names(app, &BTreeSet::new())
            .unwrap()
            .is_empty()
    );
}
