//! Focused regressions for C/C++/Go false call edges found after upstream 1.6.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind, UnresolvedRef};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::{ReferenceResolver, StoreResolutionContext};
use codegraph_store::Store;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct ResolvedGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved: Vec<UnresolvedRef>,
}

impl ResolvedGraph {
    fn node(&self, kind: NodeKind, name: &str, file: &str) -> &Node {
        self.nodes
            .iter()
            .find(|node| node.kind == kind && node.name == name && node.file_path == file)
            .unwrap_or_else(|| panic!("missing {kind:?} {name} in {file}; nodes={:#?}", self.nodes))
    }

    fn outgoing(&self, source: &Node, kind: EdgeKind) -> Vec<&Node> {
        self.edges
            .iter()
            .filter(|edge| edge.source == source.id && edge.kind == kind)
            .map(|edge| {
                self.nodes
                    .iter()
                    .find(|node| node.id == edge.target)
                    .unwrap_or_else(|| {
                        panic!("missing target {}; edges={:#?}", edge.target, self.edges)
                    })
            })
            .collect()
    }
}

fn temp_path(slug: &str) -> PathBuf {
    let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "codegraph-c-go-{slug}-{}-{nonce}",
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
        assert_ne!(
            language,
            Language::Unknown,
            "unknown fixture language: {path}"
        );
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
            .expect("insert fixture file");
        store
            .upsert_nodes(&result.nodes)
            .expect("insert fixture nodes");
        store
            .insert_edges(&result.edges)
            .expect("insert extraction edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert fixture references");
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

    let mut nodes = Vec::new();
    for kind in NodeKind::ALL {
        nodes.extend(store.nodes_by_kind(kind).expect("read nodes"));
    }
    let edges = store.all_edges().expect("read edges");
    let unresolved = store.all_unresolved_refs().expect("read unresolved refs");
    drop(store);
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_dir_all(root);
    ResolvedGraph {
        nodes,
        edges,
        unresolved,
    }
}

#[test]
fn c_static_function_is_visible_only_in_its_translation_unit() {
    let graph = resolve_project(
        "c-static-visibility",
        &[
            ("protocol.h", "static inline void header_helper(void) {}\n"),
            (
                "core.c",
                concat!(
                    "#include \"protocol.h\"\n",
                    "static void same_file(void) {}\n",
                    "void same_file_caller(void) { same_file(); }\n",
                    "void core_run(void) { hidden(); visible(); header_helper(); }\n",
                ),
            ),
            (
                "other.c",
                concat!(
                    "static void\n",
                    "hidden(void) {}\n",
                    "/* mentioning static in a comment changes no visibility */\n",
                    "void visible(void) {}\n",
                ),
            ),
        ],
    );

    let core = graph.node(NodeKind::Function, "core_run", "core.c");
    let core_targets: BTreeSet<_> = graph
        .outgoing(core, EdgeKind::Calls)
        .into_iter()
        .map(|node| (node.file_path.as_str(), node.name.as_str()))
        .collect();
    assert_eq!(
        core_targets,
        BTreeSet::from([("other.c", "visible"), ("protocol.h", "header_helper")])
    );
    assert!(graph.unresolved.iter().any(|reference| {
        reference.from_node_id == core.id && reference.reference_name == "hidden"
    }));

    let local = graph.node(NodeKind::Function, "same_file_caller", "core.c");
    assert_eq!(
        graph
            .outgoing(local, EdgeKind::Calls)
            .into_iter()
            .map(|node| (node.file_path.as_str(), node.name.as_str()))
            .collect::<Vec<_>>(),
        vec![("core.c", "same_file")]
    );
}

#[test]
fn cpp_macros_and_value_initialization_do_not_fabricate_calls() {
    let graph = resolve_project(
        "cpp-macro-constructor",
        &[
            (
                "marker.hpp",
                concat!(
                    "#include \"nested.hpp\"\n",
                    "static const char *text = R\"tag(\n",
                    "#define FAKE(value) value\n",
                    ")tag\";\n",
                ),
            ),
            ("nested.hpp", "#define TRACE_POINT(value) ((void)(value))\n"),
            ("unrelated.hpp", "#define REAL_FUNCTION() 0\n"),
            ("macro_decoy.cpp", "void TRACE_POINT(int value) {}\n"),
            (
                "exercise.cpp",
                concat!(
                    "#include \"marker.hpp\"\n",
                    "struct Aggregate { int value; };\n",
                    "class WithConstructor { public: WithConstructor(); };\n",
                    "WithConstructor::WithConstructor() {}\n",
                    "void REAL_FUNCTION() {}\n",
                    "void FAKE(int value) {}\n",
                    "void exercise() { TRACE_POINT(1); REAL_FUNCTION(); FAKE(1); }\n",
                    "void aggregate_value() { Aggregate(); Aggregate item{}; }\n",
                    "void constructor_braced() { WithConstructor item{}; }\n",
                    "void constructor_temporary() { WithConstructor(); }\n",
                ),
            ),
        ],
    );

    let exercise = graph.node(NodeKind::Function, "exercise", "exercise.cpp");
    let exercise_calls: BTreeSet<_> = graph
        .outgoing(exercise, EdgeKind::Calls)
        .into_iter()
        .map(|node| node.name.as_str())
        .collect();
    assert_eq!(exercise_calls, BTreeSet::from(["FAKE", "REAL_FUNCTION"]));
    assert!(graph.unresolved.iter().any(|reference| {
        reference.from_node_id == exercise.id && reference.reference_name == "TRACE_POINT"
    }));

    let aggregate = graph.node(NodeKind::Function, "aggregate_value", "exercise.cpp");
    assert!(
        graph.outgoing(aggregate, EdgeKind::Calls).is_empty(),
        "aggregate initialization must not produce a Calls edge to a type node"
    );
    assert_eq!(
        graph
            .outgoing(aggregate, EdgeKind::Instantiates)
            .into_iter()
            .map(|node| (node.kind, node.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (NodeKind::Struct, "Aggregate"),
            (NodeKind::Struct, "Aggregate")
        ],
        "main already models aggregate/value construction as Instantiates, not Calls"
    );
    let ctor_caller = graph.node(NodeKind::Function, "constructor_temporary", "exercise.cpp");
    let ctor_calls = graph.outgoing(ctor_caller, EdgeKind::Calls);
    assert_eq!(ctor_calls.len(), 1);
    assert_eq!(ctor_calls[0].kind, NodeKind::Method);
    assert_eq!(
        ctor_calls[0].qualified_name,
        "WithConstructor::WithConstructor"
    );
    let braced = graph.node(NodeKind::Function, "constructor_braced", "exercise.cpp");
    let braced_calls = graph.outgoing(braced, EdgeKind::Calls);
    assert_eq!(braced_calls.len(), 1);
    assert_eq!(braced_calls[0].kind, NodeKind::Method);
    assert_eq!(
        braced_calls[0].qualified_name,
        "WithConstructor::WithConstructor"
    );
}

#[test]
fn go_parameter_and_local_callable_bindings_beat_unimported_methods() {
    let graph = resolve_project(
        "go-local-callable",
        &[
            (
                "internal/impulse/relogin.go",
                concat!(
                    "package impulse\n",
                    "type socketLoop struct{}\n",
                    "func (s *socketLoop) relogin() error { return nil }\n",
                    "func (s *socketLoop) callback() error { return nil }\n",
                ),
            ),
            (
                "internal/synapse/scheduler.go",
                concat!(
                    "package synapse\n",
                    "type localLoop struct{}\n",
                    "func (s *localLoop) relogin() error { return nil }\n",
                    "func realHelper() error { return nil }\n",
                    "func RunReloginScheduler(relogin func() error) error {\n",
                    "    if err := realHelper(); err != nil { return err }\n",
                    "    return relogin()\n",
                    "}\n",
                    "func RunLocalBinding() error {\n",
                    "    callback := func() error { return nil }\n",
                    "    return callback()\n",
                    "}\n",
                    "func RunGrouped(_ int, relogin func() error) error { return relogin() }\n",
                    "func RunVarBinding() error {\n",
                    "    var callback func() error\n",
                    "    callback = func() error { return nil }\n",
                    "    return callback()\n",
                    "}\n",
                    "func RunVarBlock() error {\n",
                    "    var (\n",
                    "        callback func() error\n",
                    "    )\n",
                    "    callback = func() error { return nil }\n",
                    "    return callback()\n",
                    "}\n",
                    "func RunReceiver() error {\n",
                    "    loop := &localLoop{}\n",
                    "    return loop.relogin()\n",
                    "}\n",
                ),
            ),
        ],
    );

    let scheduler = graph.node(
        NodeKind::Function,
        "RunReloginScheduler",
        "internal/synapse/scheduler.go",
    );
    assert_eq!(
        graph
            .outgoing(scheduler, EdgeKind::Calls)
            .into_iter()
            .map(|node| node.qualified_name.as_str())
            .collect::<Vec<_>>(),
        vec!["realHelper"]
    );

    let local = graph.node(
        NodeKind::Function,
        "RunLocalBinding",
        "internal/synapse/scheduler.go",
    );
    assert!(graph.outgoing(local, EdgeKind::Calls).is_empty());
    for caller in ["RunGrouped", "RunVarBinding", "RunVarBlock"] {
        let node = graph.node(NodeKind::Function, caller, "internal/synapse/scheduler.go");
        assert!(
            graph.outgoing(node, EdgeKind::Calls).is_empty(),
            "local callable binding in {caller} must stay unresolved"
        );
    }

    let receiver = graph.node(
        NodeKind::Function,
        "RunReceiver",
        "internal/synapse/scheduler.go",
    );
    assert_eq!(
        graph
            .outgoing(receiver, EdgeKind::Calls)
            .into_iter()
            .map(|node| node.qualified_name.as_str())
            .collect::<Vec<_>>(),
        vec!["localLoop::relogin"],
        "a real typed receiver call must remain intact"
    );
    assert!(graph.unresolved.iter().any(|reference| {
        reference.from_node_id == scheduler.id && reference.reference_name == "relogin"
    }));
    assert!(graph.unresolved.iter().any(|reference| {
        reference.from_node_id == local.id && reference.reference_name == "callback"
    }));
}

#[test]
fn cpp_static_and_go_package_visibility_are_enforced_after_target_selection() {
    let cpp = resolve_project(
        "cpp-static-visibility",
        &[
            (
                "main.cpp",
                "static void local_only() {}\nvoid run() { local_only(); hidden(); visible(); }\n",
            ),
            ("other.cpp", "static void hidden() {}\nvoid visible() {}\n"),
        ],
    );
    let run = cpp.node(NodeKind::Function, "run", "main.cpp");
    assert_eq!(
        cpp.outgoing(run, EdgeKind::Calls)
            .into_iter()
            .map(|node| (node.file_path.as_str(), node.name.as_str()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([("main.cpp", "local_only"), ("other.cpp", "visible")])
    );

    let go = resolve_project(
        "go-package-visibility",
        &[
            (
                "pkg/a/util.go",
                "package a\nfunc helper() {}\nfunc RunLocal() { helper() }\nfunc RunForeign() { hidden() }\nfunc RunExported() { Report() }\n",
            ),
            (
                "pkg/b/util.go",
                "package b\nfunc helper() {}\nfunc hidden() {}\nfunc Report() {}\n",
            ),
        ],
    );
    let local = go.node(NodeKind::Function, "RunLocal", "pkg/a/util.go");
    assert_eq!(
        go.outgoing(local, EdgeKind::Calls)
            .into_iter()
            .map(|node| node.file_path.as_str())
            .collect::<Vec<_>>(),
        vec!["pkg/a/util.go"]
    );
    let foreign = go.node(NodeKind::Function, "RunForeign", "pkg/a/util.go");
    assert!(go.outgoing(foreign, EdgeKind::Calls).is_empty());
    let exported = go.node(NodeKind::Function, "RunExported", "pkg/a/util.go");
    assert_eq!(
        go.outgoing(exported, EdgeKind::Calls)
            .into_iter()
            .map(|node| node.file_path.as_str())
            .collect::<Vec<_>>(),
        vec!["pkg/b/util.go"]
    );
}

#[test]
fn kotlin_private_and_rust_module_visibility_are_enforced() {
    let kotlin = resolve_project(
        "kotlin-private-visibility",
        &[
            (
                "Secret.kt",
                "class Secret {\n  private fun hidden() {}\n  fun own() { hidden() }\n}\nfun visible() {}\n",
            ),
            ("Use.kt", "fun use() { hidden(); visible() }\n"),
        ],
    );
    let own = kotlin.node(NodeKind::Method, "own", "Secret.kt");
    assert_eq!(
        kotlin
            .outgoing(own, EdgeKind::Calls)
            .into_iter()
            .map(|node| node.qualified_name.as_str())
            .collect::<Vec<_>>(),
        vec!["Secret::hidden"]
    );
    let use_fn = kotlin.node(NodeKind::Function, "use", "Use.kt");
    assert_eq!(
        kotlin
            .outgoing(use_fn, EdgeKind::Calls)
            .into_iter()
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>(),
        vec!["visible"]
    );

    let rust = resolve_project(
        "rust-module-visibility",
        &[
            ("src/net.rs", "fn shared() {}\npub fn exported() {}\n"),
            (
                "src/net/tcp.rs",
                "pub fn open() { shared(); hidden(); exported(); }\n",
            ),
            ("src/util.rs", "fn hidden() {}\n"),
        ],
    );
    let open = rust.node(NodeKind::Function, "open", "src/net/tcp.rs");
    assert_eq!(
        rust.outgoing(open, EdgeKind::Calls)
            .into_iter()
            .map(|node| (node.file_path.as_str(), node.name.as_str()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([("src/net.rs", "exported"), ("src/net.rs", "shared")])
    );
}

#[test]
fn single_argument_function_macros_connect_callers_and_callees() {
    // Upstream #1373: `NATIVE_FN(get_version) { ... }` defines `get_version`
    // once the local `#define` establishes the name.
    for file in ["main.c", "main.cpp"] {
        let graph = resolve_project(
            "single-arg-macro",
            &[(
                file,
                "#define NATIVE_FN(name) int name(void)\n\
                 int helper(void) { return 1; }\n\
                 NATIVE_FN(get_version) { return helper(); }\n\
                 int use_it(void) { return get_version(); }\n\
                 int plain_func(void) { return 42; }\n",
            )],
        );
        let recovered = graph.node(NodeKind::Function, "get_version", file);
        let helper = graph.node(NodeKind::Function, "helper", file);
        let caller = graph.node(NodeKind::Function, "use_it", file);
        graph.node(NodeKind::Function, "plain_func", file);
        assert!(
            graph
                .outgoing(caller, EdgeKind::Calls)
                .iter()
                .any(|target| target.id == recovered.id),
            "{file}"
        );
        assert!(
            graph
                .outgoing(recovered, EdgeKind::Calls)
                .iter()
                .any(|target| target.id == helper.id),
            "{file}"
        );
    }
}
