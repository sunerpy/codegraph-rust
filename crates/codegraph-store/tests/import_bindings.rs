//! Which importers an edit of a module re-resolves whole because a binding of
//! their statement does not resolve into it (upstream #2450,
//! `sync-import-binding-retry.test.ts`): the edit can give the module what
//! the binding imports under another name, which no name lookup finds.

use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind, UnresolvedRef};
use codegraph_store::Store;

const WIDGET: &str = "src/widgets/Widget.ts";
const PLAIN: &str = "src/plain.ts";
/// A file node whose id is not `file:<path>`, as an embedded extractor's
/// (Liquid's) is: the lookup goes by path.
const PLAIN_ID: &str = "file:5f2b9c0d";

fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    file: &str,
    language: Language,
    lines: (i64, i64),
) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_path: file.to_string(),
        language,
        start_line: lines.0,
        end_line: lines.1,
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
        (1, 1),
    )
}

/// An import statement of `importer` on `lines` naming `source`.
fn statement(importer: &str, source: &str, lines: (i64, i64), language: Language) -> Node {
    node(
        &format!("import:{importer}:{}", lines.0),
        NodeKind::Import,
        source,
        importer,
        language,
        lines,
    )
}

/// An `imports` edge `resolved_by` one of the resolver's strategies.
fn imports(source: &str, target: &str, line: i64, resolved_by: &str) -> Edge {
    Edge {
        id: None,
        source: source.to_string(),
        target: target.to_string(),
        kind: EdgeKind::Imports,
        metadata: Some(serde_json::json!({ "confidence": 0.9, "resolvedBy": resolved_by })),
        line: Some(line),
        col: Some(0),
        provenance: None,
    }
}

fn parked(file: &str, name: &str, line: i64, language: Language) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: format!("file:{file}"),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::Imports,
        line,
        col: 0,
        candidates: None,
        file_path: file.to_string(),
        language,
        is_function_ref: false,
        reference_subkind: None,
    }
}

fn store() -> Store {
    let path = std::env::temp_dir().join(format!(
        "codegraph-store-import-bindings-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = Store::open(&path).expect("open store");
    let ts = Language::TypeScript;
    let mut nodes = vec![
        file_node(WIDGET, ts),
        node(
            "class:widget",
            NodeKind::Class,
            "Widget",
            WIDGET,
            ts,
            (1, 1),
        ),
        node(PLAIN_ID, NodeKind::File, "plain.ts", PLAIN, ts, (1, 1)),
        file_node("src/other.ts", ts),
        node("class:w", NodeKind::Class, "W", "src/other.ts", ts, (1, 1)),
    ];
    let mut edges = Vec::new();
    let mut refs = Vec::new();
    let widget = format!("file:{WIDGET}");
    // `import { Widget as W } from './widgets/Widget'` while the module
    // declares no `Widget`: the binding is parked.
    nodes.push(file_node("src/parked.ts", ts));
    nodes.push(statement("src/parked.ts", "./widgets/Widget", (1, 1), ts));
    edges.push(imports("file:src/parked.ts", &widget, 1, "import"));
    refs.push(parked("src/parked.ts", "W", 1, ts));
    // The same binding matched by its name to `W` in another file.
    nodes.push(file_node("src/decoy.ts", ts));
    nodes.push(statement("src/decoy.ts", "./widgets/Widget", (1, 1), ts));
    edges.push(imports("file:src/decoy.ts", &widget, 1, "import"));
    edges.push(imports("file:src/decoy.ts", "class:w", 1, "exact-match"));
    // ... or to a `W` the module itself declares.
    nodes.push(node(
        "constant:w",
        NodeKind::Constant,
        "W",
        WIDGET,
        ts,
        (2, 2),
    ));
    nodes.push(file_node("src/inside.ts", ts));
    nodes.push(statement("src/inside.ts", "./widgets/Widget", (1, 1), ts));
    edges.push(imports("file:src/inside.ts", &widget, 1, "import"));
    edges.push(imports(
        "file:src/inside.ts",
        "constant:w",
        1,
        "exact-match",
    ));
    // Every binding resolves through the import.
    nodes.push(file_node("src/bound.ts", ts));
    nodes.push(statement("src/bound.ts", "./widgets/Widget", (1, 1), ts));
    edges.push(imports("file:src/bound.ts", &widget, 1, "import"));
    edges.push(imports("file:src/bound.ts", "class:widget", 1, "import"));
    // The path matched by the file's path, the binding through the import:
    // a component's script.
    nodes.push(file_node("src/component.ts", ts));
    nodes.push(statement(
        "src/component.ts",
        "./widgets/Widget",
        (1, 1),
        ts,
    ));
    edges.push(imports("file:src/component.ts", &widget, 1, "file-path"));
    edges.push(imports(
        "file:src/component.ts",
        "class:widget",
        1,
        "import",
    ));
    // A package's parked binding on another statement of the same file.
    nodes.push(file_node("src/react.ts", ts));
    nodes.push(statement("src/react.ts", "react", (1, 1), ts));
    refs.push(parked("src/react.ts", "react", 1, ts));
    refs.push(parked("src/react.ts", "React", 1, ts));
    nodes.push(statement("src/react.ts", "./widgets/Widget", (2, 2), ts));
    edges.push(imports("file:src/react.ts", &widget, 2, "import"));
    edges.push(imports("file:src/react.ts", "class:widget", 2, "import"));
    // A statement over three lines whose bindings resolve, then a statement
    // of another module whose binding is parked.
    nodes.push(file_node("src/spread.ts", ts));
    nodes.push(statement("src/spread.ts", "./widgets/Widget", (1, 3), ts));
    edges.push(imports("file:src/spread.ts", &widget, 1, "import"));
    edges.push(imports("file:src/spread.ts", "class:widget", 2, "import"));
    nodes.push(statement("src/spread.ts", "./plain", (4, 4), ts));
    edges.push(imports("file:src/spread.ts", PLAIN_ID, 4, "import"));
    refs.push(parked("src/spread.ts", "Thing", 4, ts));
    // No ESM import: a Python file with the same shape.
    let py = Language::Python;
    nodes.push(file_node("src/lib.py", py));
    nodes.push(statement("src/lib.py", "widgets.Widget", (1, 1), py));
    edges.push(imports("file:src/lib.py", &widget, 1, "import"));
    refs.push(parked("src/lib.py", "W", 1, py));
    store.upsert_nodes(&nodes).expect("nodes");
    store.insert_edges(&edges).expect("edges");
    store.insert_unresolved_refs(&refs).expect("refs");
    store
}

#[test]
fn importers_with_unbound_bindings_reads_only_the_statement_importing_the_file() {
    let store = store();
    assert_eq!(
        store.importers_with_unbound_bindings(WIDGET).unwrap(),
        ["src/decoy.ts", "src/inside.ts", "src/parked.ts"]
    );
    assert_eq!(
        store.importers_with_unbound_bindings(PLAIN).unwrap(),
        ["src/spread.ts"]
    );
    // A binding matched into a file is no import of it.
    assert_eq!(
        store
            .importers_with_unbound_bindings("src/other.ts")
            .unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        store
            .importers_with_unbound_bindings("src/absent.ts")
            .unwrap(),
        Vec::<String>::new()
    );
}
