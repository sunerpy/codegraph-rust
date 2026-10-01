//! C++ local-object initialization must reach a uniquely proven constructor
//! method, never an aggregate/type node or an arbitrary overload (#1839).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::{ReferenceResolver, StoreResolutionContext};
use codegraph_store::Store;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct ResolvedGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl ResolvedGraph {
    fn node(&self, kind: NodeKind, name: &str, file: &str) -> &Node {
        self.nodes
            .iter()
            .find(|node| node.kind == kind && node.name == name && node.file_path == file)
            .unwrap_or_else(|| panic!("missing {kind:?} {name} in {file}"))
    }

    fn call_targets(&self, source: &Node) -> Vec<&Node> {
        let mut targets = self
            .edges
            .iter()
            .filter(|edge| edge.source == source.id && edge.kind == EdgeKind::Calls)
            .map(|edge| {
                self.nodes
                    .iter()
                    .find(|node| node.id == edge.target)
                    .expect("edge target")
            })
            .collect::<Vec<_>>();
        targets.sort_by_key(|node| (node.file_path.as_str(), node.start_line));
        targets
    }
}

fn temp_path(slug: &str) -> PathBuf {
    let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "codegraph-cpp-constructor-{slug}-{}-{nonce}",
        std::process::id()
    ))
}

fn collect_files(root: &Path, dir: &Path, output: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read fixture directory") {
        let path = entry.expect("fixture entry").path();
        if path.is_dir() {
            collect_files(root, &path, output);
        } else {
            output.push(
                path.strip_prefix(root)
                    .expect("fixture path under root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn resolve_project(slug: &str, files: &[(&str, &str)]) -> ResolvedGraph {
    let root = temp_path(slug);
    std::fs::create_dir_all(&root).expect("create fixture root");
    for (relative, source) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, source).expect("write fixture source");
    }

    let mut relative = Vec::new();
    collect_files(&root, &root, &mut relative);
    relative.sort();
    let db = temp_path(&format!("{slug}-db")).with_extension("db");
    let mut store = Store::open(&db).expect("open fixture store");
    for path in &relative {
        let result = extract_file(&root, path).expect("extract fixture");
        let language = detect_language(path);
        assert_ne!(language, Language::Unknown, "unknown fixture: {path}");
        store
            .upsert_file(&FileRecord {
                path: path.clone(),
                content_hash: "fixture".to_string(),
                language,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: result.errors,
                generated: false,
            })
            .expect("insert file");
        store.upsert_nodes(&result.nodes).expect("insert nodes");
        store.insert_edges(&result.edges).expect("insert edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert references");
    }

    let root_string = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(&root_string);
    {
        let context = StoreResolutionContext::new(&store, &root_string);
        resolver.initialize(&context);
    }
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve fixture");

    assert!(
        store
            .all_unresolved_refs()
            .expect("read remaining refs")
            .iter()
            .all(|reference| !reference
                .reference_name
                .starts_with("codegraph:cpp-constructor:")),
        "internal constructor probes must be consumed, not exposed as dangling calls"
    );

    let nodes = store.all_nodes().expect("read nodes");
    let edges = store.all_edges().expect("read edges");
    drop(store);
    std::fs::remove_file(db).expect("remove fixture db");
    std::fs::remove_dir_all(root).expect("remove fixture root");
    ResolvedGraph { nodes, edges }
}

#[test]
fn local_initializers_resolve_by_owner_and_accepted_arity() {
    let graph = resolve_project(
        "local-overloads",
        &[(
            "main.cpp",
            concat!(
                "struct Aggregate { int value; };\n",
                "class WithConstructor { public: WithConstructor(); explicit WithConstructor(int value); };\n",
                "WithConstructor::WithConstructor() {}\n",
                "WithConstructor::WithConstructor(int value) {}\n",
                "void aggregate_initialization() { Aggregate item{}; }\n",
                "void constructor_default() { WithConstructor item; }\n",
                "void constructor_braced() { WithConstructor item{}; }\n",
                "void constructor_value() { WithConstructor item(1); }\n",
            ),
        )],
    );

    let aggregate = graph.node(NodeKind::Function, "aggregate_initialization", "main.cpp");
    assert!(graph.call_targets(aggregate).is_empty());

    for (caller, expected_line) in [
        ("constructor_default", 3),
        ("constructor_braced", 3),
        ("constructor_value", 4),
    ] {
        let caller = graph.node(NodeKind::Function, caller, "main.cpp");
        let targets = graph.call_targets(caller);
        assert_eq!(targets.len(), 1, "targets={targets:#?}");
        assert_eq!(targets[0].kind, NodeKind::Method);
        assert_eq!(
            targets[0].qualified_name,
            "WithConstructor::WithConstructor"
        );
        assert_eq!(targets[0].start_line, expected_line);
    }
}

#[test]
fn ambiguous_owner_or_overload_stays_unresolved() {
    let graph = resolve_project(
        "ambiguous",
        &[(
            "ambiguous.cpp",
            concat!(
                "namespace left { struct Widget { Widget() {} }; }\n",
                "namespace right { struct Widget { Widget() {} }; }\n",
                "struct Defaults {\n",
                "  Defaults() {}\n",
                "  Defaults(int x = 0) {}\n",
                "};\n",
                "void ambiguous_owner() { Widget value; }\n",
                "void explicit_owner() { left::Widget value; }\n",
                "void ambiguous_overload() { Defaults value; }\n",
            ),
        )],
    );

    for caller_name in ["ambiguous_owner", "ambiguous_overload"] {
        let caller = graph.node(NodeKind::Function, caller_name, "ambiguous.cpp");
        assert!(
            graph.call_targets(caller).is_empty(),
            "{caller_name} must fail closed"
        );
    }
    let caller = graph.node(NodeKind::Function, "explicit_owner", "ambiguous.cpp");
    let targets = graph.call_targets(caller);
    assert_eq!(targets.len(), 1, "targets={targets:#?}");
    assert_eq!(targets[0].qualified_name, "left::Widget::Widget");
}

#[test]
fn lexical_namespace_prefers_nested_or_root_owner_without_path_guessing() {
    let graph = resolve_project(
        "lexical-owner",
        &[(
            "scope.cpp",
            concat!(
                "struct Widget { Widget() {} };\n",
                "namespace nested {\n",
                "struct Widget { Widget() {} };\n",
                "void build_nested() { Widget value; }\n",
                "}\n",
                "void build_root() { Widget value; }\n",
            ),
        )],
    );
    let nested = graph.node(NodeKind::Function, "build_nested", "scope.cpp");
    let nested_targets = graph.call_targets(nested);
    assert_eq!(nested_targets.len(), 1);
    assert_eq!(nested_targets[0].qualified_name, "nested::Widget::Widget");

    let root = graph.node(NodeKind::Function, "build_root", "scope.cpp");
    let root_targets = graph.call_targets(root);
    assert_eq!(root_targets.len(), 1);
    assert_eq!(root_targets[0].qualified_name, "Widget::Widget");
}

impl ResolvedGraph {
    fn function(&self, name: &str) -> &Node {
        self.nodes
            .iter()
            .find(|node| node.kind == NodeKind::Function && node.name == name)
            .unwrap_or_else(|| panic!("missing function {name}"))
    }

    /// `kind qualified_name (file)` of a function's `calls` targets, sorted.
    fn calls(&self, caller: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .call_targets(self.function(caller))
            .into_iter()
            .map(|node| {
                format!(
                    "{} {} ({})",
                    node.kind.as_str(),
                    node.qualified_name,
                    node.file_path
                )
            })
            .collect();
        out.sort();
        out
    }

    /// The signatures (or names) of a function's `calls` targets, sorted.
    fn callee_signatures(&self, caller: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .call_targets(self.function(caller))
            .into_iter()
            .map(|node| node.signature.clone().unwrap_or_else(|| node.name.clone()))
            .collect();
        out.sort();
        out
    }
}

#[test]
fn a_constructor_is_chosen_in_the_sites_namespace_then_an_enclosing_or_global_one() {
    let graph = resolve_project(
        "namespaces",
        &[(
            "ns.cpp",
            concat!(
                "struct Global { Global() {} };\n",
                "union Value { Value() {} int x; };\n",
                "void union_use() { Value v; }\n",
                "namespace first { struct Widget { Widget() {} }; }\n",
                "namespace second {\n",
                "  struct Widget { Widget() {} };\n",
                "  void local_use() { Widget w; }\n",
                "  void global_use() { Global g; }\n",
                "}\n",
                "void explicit_use() { first::Widget w; }\n",
            ),
        )],
    );
    assert_eq!(
        graph.calls("union_use"),
        vec!["method Value::Value (ns.cpp)"]
    );
    assert_eq!(
        graph.calls("local_use"),
        vec!["method second::Widget::Widget (ns.cpp)"]
    );
    assert_eq!(
        graph.calls("global_use"),
        vec!["method Global::Global (ns.cpp)"]
    );
    assert_eq!(
        graph.calls("explicit_use"),
        vec!["method first::Widget::Widget (ns.cpp)"]
    );
}

#[test]
fn an_overload_is_chosen_by_arity_only_when_exactly_one_admits_the_count() {
    let graph = resolve_project(
        "overloads",
        &[(
            "overloads.cpp",
            concat!(
                "struct Widget {\n",
                "  Widget() {}\n",
                "  Widget(int value) {}\n",
                "  Widget(int a, int b = 2) {}\n",
                "};\n",
                "struct Ambiguous {\n",
                "  Ambiguous(int) {}\n",
                "  Ambiguous(double) {}\n",
                "};\n",
                "void default_use() { Widget w; }\n",
                "void two_use() { Widget w(1, 2); }\n",
                "void one_use() { Widget w(1); }\n",
                "void ambiguous_use(int value) { Ambiguous w(value); }\n",
            ),
        )],
    );
    assert_eq!(graph.callee_signatures("default_use"), vec!["()"]);
    assert_eq!(
        graph.callee_signatures("two_use"),
        vec!["(int a, int b = 2)"]
    );
    // `Widget(int)` and `Widget(int, int = 2)` both admit one argument.
    assert_eq!(graph.callee_signatures("one_use"), Vec::<String>::new());
    assert_eq!(
        graph.callee_signatures("ambiguous_use"),
        Vec::<String>::new()
    );
}

#[test]
fn a_default_declared_on_a_prototype_reaches_the_definitions_overload() {
    let graph = resolve_project(
        "prototype-defaults",
        &[
            (
                "widget.hpp",
                "namespace app {\nstruct Widget {\n  Widget(int value = 7);\n  Widget(double value);\n};\nstruct Pair { Pair(int first, int second = 2); };\n}\n",
            ),
            (
                "widget.cpp",
                "#include \"widget.hpp\"\napp::Widget::Widget(int renamed) {}\napp::Widget::Widget(double renamed) {}\napp::Pair::Pair(int a, int b) {}\n",
            ),
            (
                "use.cpp",
                "#include \"widget.hpp\"\nint argument(int value) { return value; }\nvoid defaults() { app::Widget item; }\nvoid nested() { app::Pair item(argument(1)); }\nvoid ambiguous() { app::Widget item(1); }\n",
            ),
        ],
    );
    assert_eq!(
        graph.calls("defaults"),
        vec!["method app::Widget::Widget (widget.cpp)"]
    );
    assert_eq!(graph.callee_signatures("defaults"), vec!["(int renamed)"]);
    assert_eq!(
        graph.calls("nested"),
        vec![
            "function argument (use.cpp)",
            "method app::Pair::Pair (widget.cpp)"
        ]
    );
    assert_eq!(graph.calls("ambiguous"), Vec::<String>::new());
}

#[test]
fn an_initializer_list_overload_or_a_parameter_pack_declines() {
    let graph = resolve_project(
        "declines",
        &[(
            "declines.cpp",
            concat!(
                "namespace std { template <class T> class initializer_list {}; }\n",
                "struct Listed {\n",
                "  Listed(std::initializer_list<int> values) {}\n",
                "  Listed(int one) {}\n",
                "};\n",
                "struct Forwarding {\n",
                "  template <class... A> Forwarding(A&&... args) {}\n",
                "  Forwarding(int one) {}\n",
                "};\n",
                "void braced() { Listed l{1}; }\n",
                "void forwarded() { Forwarding f(1); }\n",
            ),
        )],
    );
    // Brace-init prefers the initializer_list overload, which needs the
    // argument types; a pack admits any count, so `Forwarding(int)` is not
    // the only overload that could take one argument.
    assert_eq!(graph.calls("braced"), Vec::<String>::new());
    assert_eq!(graph.calls("forwarded"), Vec::<String>::new());
}

#[test]
fn array_elements_reach_their_constructors_and_keep_nested_calls() {
    let graph = resolve_project(
        "arrays",
        &[(
            "arrays.cpp",
            concat!(
                "struct Widget {\n",
                " Widget() {}\n",
                " Widget(int value) {}\n",
                "};\n",
                "int argument() { return 1; }\n",
                "void plain() { Widget items[2]; }\n",
                "void empty() { Widget items[2]{}; }\n",
                "void elements() { Widget items[3]{{argument()}, {2}}; }\n",
                "void grid() { Widget items[2][2]{{{1}, {2}}, {{3}}}; }\n",
                "void scalar_elements() { Widget items[2]{1, 2}; }\n",
                "void pointers() { Widget *p{}; Widget *q(nullptr); Widget *arr[2]{}; }\n",
                "void prototype() { Widget most_vexing(); extern Widget external; }\n",
            ),
        )],
    );
    assert_eq!(graph.callee_signatures("plain"), vec!["()"]);
    assert_eq!(graph.callee_signatures("empty"), vec!["()"]);
    assert_eq!(
        graph.callee_signatures("grid"),
        vec!["()", "(int value)", "(int value)", "(int value)"]
    );
    assert_eq!(
        graph.callee_signatures("scalar_elements"),
        vec!["(int value)", "(int value)"]
    );
    assert_eq!(
        graph.callee_signatures("elements"),
        vec!["()", "(int value)", "(int value)", "argument"]
    );
    assert_eq!(graph.calls("pointers"), Vec::<String>::new());
    assert_eq!(graph.calls("prototype"), Vec::<String>::new());
}
