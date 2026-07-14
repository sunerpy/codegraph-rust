//! Cross-file resolution parity tests against the mini golden DB.
//!
//! Extracts the committed mini fixture, populates a fresh store with the
//! extracted nodes + unresolved references, runs the resolver, then compares the
//! resolved edges (kind/source/target/line/col/resolvedBy/confidence) to the golden
//! resolved golden edges (`reference/golden/mini/edges.json`). This validates
//! import resolution AND name-match disambiguation against the derived goldens.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use codegraph_core::types::{Edge, FileRecord, Language};
use codegraph_extract::extract_file;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is .../crates/codegraph-resolve.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!(
        "codegraph-resolve-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

/// A comparable resolved-edge key: everything the upstream records on a resolved edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    kind: String,
    source: String,
    target: String,
    line: i64,
    col: i64,
    resolved_by: String,
    confidence_milli: i64,
}

fn edge_key(edge: &Edge) -> EdgeKey {
    let metadata = edge.metadata.as_ref().expect("resolved edge has metadata");
    let confidence = metadata
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
        .expect("confidence");
    let resolved_by = metadata
        .get("resolvedBy")
        .and_then(serde_json::Value::as_str)
        .expect("resolvedBy")
        .to_string();
    EdgeKey {
        kind: edge.kind.as_str().to_string(),
        source: edge.source.clone(),
        target: edge.target.clone(),
        line: edge.line.unwrap_or_default(),
        col: edge.col.unwrap_or_default(),
        resolved_by,
        confidence_milli: (confidence * 1000.0).round() as i64,
    }
}

/// Parse the resolved (non-`contains`) edges from the golden edges.json.
fn golden_resolved_edges() -> BTreeSet<EdgeKey> {
    let text = std::fs::read_to_string(workspace_root().join("reference/golden/mini/edges.json"))
        .expect("read golden edges.json");
    let edges: Vec<serde_json::Value> = serde_json::from_str(&text).expect("parse golden edges");
    edges
        .into_iter()
        .filter(|e| e["kind"].as_str() != Some("contains"))
        .map(|e| {
            let metadata = &e["metadata"];
            EdgeKey {
                kind: e["kind"].as_str().expect("kind").to_string(),
                source: e["source"].as_str().expect("source").to_string(),
                target: e["target"].as_str().expect("target").to_string(),
                line: e["line"].as_i64().expect("line"),
                col: e["col"].as_i64().expect("col"),
                resolved_by: metadata["resolvedBy"]
                    .as_str()
                    .expect("resolvedBy")
                    .to_string(),
                confidence_milli: (metadata["confidence"].as_f64().expect("confidence") * 1000.0)
                    .round() as i64,
            }
        })
        .collect()
}

/// Extract the mini fixture into a fresh store and run the resolver, returning
/// the resolved edges read back from the DB.
fn resolve_mini_fixture(test_name: &str) -> Vec<Edge> {
    resolve_fixture(
        test_name,
        &fixture_root(),
        &["src/math.ts", "src/app.ts", "tools/greeter.py"],
    )
}

/// Extract an arbitrary fixture root + relative file list into a fresh store,
/// run the resolver, and return the resolved (non-`contains`) edges.
fn resolve_fixture(test_name: &str, root: &Path, relative_files: &[&str]) -> Vec<Edge> {
    let mut store = Store::open(&temp_db_path(test_name)).expect("open store");

    for &relative in relative_files {
        let result = extract_file(root, relative).expect("extract file");
        let language = match Path::new(relative).extension().and_then(|e| e.to_str()) {
            Some("ts") => Language::TypeScript,
            Some("py") => Language::Python,
            Some("java") => Language::Java,
            Some("cpp") => Language::Cpp,
            Some("php") => Language::Php,
            other => panic!("unexpected fixture extension {other:?}"),
        };
        store
            .upsert_file(&FileRecord {
                path: relative.to_string(),
                content_hash: "fixture".to_string(),
                language,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
            })
            .expect("upsert file");
        store.upsert_nodes(&result.nodes).expect("upsert nodes");
        store
            .insert_edges(&result.edges)
            .expect("insert contains edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert unresolved refs");
    }

    let mut resolver = ReferenceResolver::new(root.to_string_lossy().to_string());
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve and persist");

    // Read back all non-contains edges (the resolver's output).
    let mut resolved = Vec::new();
    for node in store_all_node_ids(&store) {
        for edge in store
            .edges_by_source_kind(&node, None)
            .expect("edges by source")
        {
            if edge.kind != codegraph_core::types::EdgeKind::Contains {
                resolved.push(edge);
            }
        }
    }
    resolved
}

fn store_all_node_ids(store: &Store) -> Vec<String> {
    let mut ids = Vec::new();
    for kind in codegraph_core::types::NodeKind::ALL {
        for node in store.nodes_by_kind(kind).expect("nodes by kind") {
            ids.push(node.id);
        }
    }
    ids
}

#[test]
fn cross_file_resolution_matches_upstream_golden_edges() {
    let resolved = resolve_mini_fixture("golden");
    let produced: BTreeSet<EdgeKey> = resolved.iter().map(edge_key).collect();
    let golden = golden_resolved_edges();

    let missing: Vec<&EdgeKey> = golden.difference(&produced).collect();
    let extra: Vec<&EdgeKey> = produced.difference(&golden).collect();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "resolved edges differ from the golden\nMISSING (golden not produced): {missing:#?}\nEXTRA (produced not in golden): {extra:#?}"
    );
}

#[test]
fn imported_function_produces_resolved_edge_to_correct_node() {
    // A fixture importing a fn from another file produces a resolved edge to the
    // right node id: src/app.ts imports `add` from ./math and calls it; the
    // resolved `add` call must point at math.ts's `add` function node.
    let resolved = resolve_mini_fixture("imported-fn");
    let add_node = "function:cce15011e0125d59f6bef014ae79c04f";
    let run_demo = "function:60629aa3876961b8bd3c07c43bbe6a37";

    let edge = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls
            && e.source == run_demo
            && e.target == add_node
    });
    assert!(
        edge.is_some(),
        "expected runDemo→add resolved call edge, got: {resolved:#?}"
    );
    let edge = edge.expect("edge present");
    let metadata = edge.metadata.as_ref().expect("metadata");
    assert_eq!(metadata["resolvedBy"].as_str(), Some("import"));
}

#[test]
fn python_module_member_call_resolves_through_imported_submodule() {
    // `from pkg import mod; mod.helper()` — the `mod.helper` call resolves to the
    // top-level `helper` in pkg/mod.py via resolve_python_module_member, and the
    // `from pkg import mod` ref resolves the submodule to pkg/mod.py (file edge).
    // Ports the upstream per-language import refinements (import-resolver.ts, #578).
    let dir = std::env::temp_dir().join(format!(
        "codegraph-py-modmember-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("pkg")).expect("mkdir fixture");
    std::fs::write(dir.join("pkg/__init__.py"), "").expect("write __init__");
    std::fs::write(dir.join("pkg/mod.py"), "def helper():\n    return 1\n").expect("write mod.py");
    std::fs::write(
        dir.join("app.py"),
        "from pkg import mod\n\ndef run():\n    return mod.helper()\n",
    )
    .expect("write app.py");

    let resolved = resolve_fixture("py-modmember", &dir, &["pkg/mod.py", "app.py"]);
    let _ = std::fs::remove_dir_all(&dir);

    // `run` → `helper` resolved call edge (resolvedBy import).
    let call = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls && e.target.starts_with("function:")
    });
    assert!(
        call.is_some(),
        "expected mod.helper() resolved call edge, got: {resolved:#?}"
    );
    assert_eq!(
        call.unwrap().metadata.as_ref().expect("metadata")["resolvedBy"].as_str(),
        Some("import")
    );
    // `from pkg import mod` → file→file import edge to pkg/mod.py.
    let import_edge = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Imports && e.target.starts_with("file:")
    });
    assert!(
        import_edge.is_some(),
        "expected submodule import file→file edge, got: {resolved:#?}"
    );
}

#[test]
fn name_match_disambiguates_instance_method_call() {
    // `counter.increment(...)` in app.ts must resolve to Counter::increment in
    // math.ts via the instance-method name-match strategy, NOT to any other node.
    let resolved = resolve_mini_fixture("name-match");
    let increment = "method:f501ba98441869bd251c636e30d31e3d";
    let run_demo = "function:60629aa3876961b8bd3c07c43bbe6a37";

    let instance_calls: Vec<&Edge> = resolved
        .iter()
        .filter(|e| {
            e.kind == codegraph_core::types::EdgeKind::Calls
                && e.source == run_demo
                && e.target == increment
        })
        .collect();
    assert_eq!(
        instance_calls.len(),
        2,
        "expected 2 runDemo→Counter::increment instance-method edges (lines 5,6), got: {instance_calls:#?}"
    );
    for edge in instance_calls {
        let metadata = edge.metadata.as_ref().expect("metadata");
        assert_eq!(metadata["resolvedBy"].as_str(), Some("instance-method"));
    }
}

#[test]
fn cross_file_static_method_resolves_to_member_not_class() {
    // `Factory.create()` on a NAMED class import (upstream f7441f21 / #825) must
    // descend to `method:create` as a `calls` edge — NOT mislink to the class
    // and get mis-promoted to `instantiates`. Verified byte-identical to the upstream
    // 1.0.1 (see commit body for the captured edge-set parity check).
    let dir = std::env::temp_dir().join(format!(
        "codegraph-static-member-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("mkdir fixture");
    std::fs::write(
        dir.join("src/factory.ts"),
        "export class Factory {\n  static create(seed: number): number {\n    return seed * 2;\n  }\n\n  build(): number {\n    return Factory.create(1);\n  }\n}\n",
    )
    .expect("write factory.ts");
    std::fs::write(
        dir.join("src/consumer.ts"),
        "import { Factory } from './factory';\n\nexport function run(): number {\n  return Factory.create(7);\n}\n\nrun();\n",
    )
    .expect("write consumer.ts");

    let resolved = resolve_fixture(
        "static-member",
        &dir,
        &["src/factory.ts", "src/consumer.ts"],
    );
    let _ = std::fs::remove_dir_all(&dir);

    let calls_to_create: Vec<&Edge> = resolved
        .iter()
        .filter(|e| {
            e.kind == codegraph_core::types::EdgeKind::Calls
                && e.target.starts_with("method:")
                && e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("import")
        })
        .collect();
    assert!(
        !calls_to_create.is_empty(),
        "expected a cross-file import-resolved calls→method:create edge, got: {resolved:#?}"
    );

    let instantiates: Vec<&Edge> = resolved
        .iter()
        .filter(|e| e.kind == codegraph_core::types::EdgeKind::Instantiates)
        .collect();
    assert!(
        instantiates.is_empty(),
        "static call Factory.create() must NOT produce an instantiates edge, got: {instantiates:#?}"
    );
}

#[test]
fn callback_function_ref_emits_references_edges_with_fn_ref_metadata() {
    // Callback-as-value capture (upstream 8a114ba5 / #756, TS/JS slice): a bare
    // function passed to a registrar (`addEventListener('blur', onBlur)`) and a
    // `this.<method>` member (`bus.on('click', this.handleClick)`) each yield a
    // `references` edge tagged `fnRef:true` / resolvedBy `function-ref`.
    let dir = std::env::temp_dir().join(format!(
        "codegraph-fn-ref-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("mkdir fixture");
    std::fs::write(
        dir.join("src/handlers.ts"),
        "export function onBlur(): void {}\n\nexport function setup(target: EventTarget): void {\n  target.addEventListener('blur', onBlur);\n}\n\nexport class Widget {\n  private handleClick(): void {}\n\n  register(bus: { on(e: string, cb: () => void): void }): void {\n    bus.on('click', this.handleClick);\n  }\n}\n",
    )
    .expect("write handlers.ts");

    let resolved = resolve_fixture("fn-ref", &dir, &["src/handlers.ts"]);
    let _ = std::fs::remove_dir_all(&dir);

    let fn_refs: Vec<&Edge> = resolved
        .iter()
        .filter(|e| {
            e.kind == codegraph_core::types::EdgeKind::References
                && e.metadata
                    .as_ref()
                    .and_then(|m| m.get("fnRef"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        })
        .collect();
    assert_eq!(
        fn_refs.len(),
        2,
        "expected 2 fnRef references edges (setup->onBlur, register->handleClick), got: {resolved:#?}"
    );
    for edge in &fn_refs {
        let metadata = edge.metadata.as_ref().expect("metadata");
        assert_eq!(metadata["resolvedBy"].as_str(), Some("function-ref"));
    }

    let targets_methods = fn_refs.iter().any(|e| e.target.starts_with("method:"));
    let targets_functions = fn_refs.iter().any(|e| e.target.starts_with("function:"));
    assert!(
        targets_methods && targets_functions,
        "fnRef edges must reach both the bare function (onBlur) and the this.member method (handleClick)"
    );
}

#[test]
fn callback_function_ref_python_bare_function_value() {
    // Multi-language function_ref (upstream #756): Python `register(worker)` bare
    // function value yields a `references` edge tagged fnRef:true / resolvedBy
    // function-ref. Edge count + target captured byte-identical from the upstream 1.0.1
    // on this exact source (the upstream emits exactly one fnRef edge here).
    let dir = std::env::temp_dir().join(format!(
        "codegraph-fn-ref-py-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("mkdir fixture");
    std::fs::write(
        dir.join("src/m.py"),
        "def worker():\n    return 1\n\ndef setup():\n    register(worker)\n\nclass Handler:\n    def on_click(self):\n        pass\n\n    def wire(self):\n        bus.subscribe(self.on_click)\n",
    )
    .expect("write m.py");

    let resolved = resolve_fixture("fn-ref-py", &dir, &["src/m.py"]);
    let _ = std::fs::remove_dir_all(&dir);

    let fn_refs: Vec<&Edge> = resolved
        .iter()
        .filter(|e| {
            e.kind == codegraph_core::types::EdgeKind::References
                && e.metadata
                    .as_ref()
                    .and_then(|m| m.get("fnRef"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        })
        .collect();
    assert_eq!(
        fn_refs.len(),
        1,
        "expected 1 Python fnRef edge (setup->worker), got: {resolved:#?}"
    );
    let edge = fn_refs[0];
    assert!(edge.target.starts_with("function:"));
    assert_eq!(
        edge.metadata.as_ref().expect("metadata")["resolvedBy"].as_str(),
        Some("function-ref")
    );
}

/// Build a unique temp fixture directory rooted under the system temp dir.
fn fresh_fixture_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("mkdir fixture src");
    dir
}

#[test]
fn java_field_receiver_resolves_method_on_field_type() {
    // Java field-receiver inference (name-matcher.ts:878-925): `repo.save()`
    // where `repo` is a field of type `Repo` resolves to Repo::save via the
    // field's declared type — the upstream 1.0.1 emits run→method:save 0.9
    // instance-method (captured ground truth), Rust must match (not stop at
    // field:repo).
    let dir = fresh_fixture_dir("java-field-recv");
    std::fs::write(dir.join("src/Repo.java"), "class Repo { void save() {} }\n")
        .expect("write Repo.java");
    std::fs::write(
        dir.join("src/Service.java"),
        "class Service {\n    private Repo repo;\n    void run() { repo.save(); }\n}\n",
    )
    .expect("write Service.java");

    let resolved = resolve_fixture(
        "java-field-recv",
        &dir,
        &["src/Repo.java", "src/Service.java"],
    );
    let _ = std::fs::remove_dir_all(&dir);

    let save_call = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls && e.target.starts_with("method:")
    });
    assert!(
        save_call.is_some(),
        "expected run→method:save instance-method edge, got: {resolved:#?}"
    );
    let metadata = save_call.unwrap().metadata.as_ref().expect("metadata");
    assert_eq!(metadata["resolvedBy"].as_str(), Some("instance-method"));
    assert_eq!(
        (metadata["confidence"].as_f64().expect("confidence") * 1000.0).round() as i64,
        900
    );
}

#[test]
fn cpp_receiver_inference_resolves_method_on_declared_type() {
    // C++ receiver inference (name-matcher.ts:512-567): `Foo f = make();
    // f.v()` infers f's type from the `Foo f` declarator, resolving v on Foo —
    // the upstream 1.0.1 emits run→method:v 0.9 instance-method (captured ground
    // truth), Rust must match.
    let dir = fresh_fixture_dir("cpp-recv");
    std::fs::write(
        dir.join("src/main.cpp"),
        "struct Foo { int v() { return 1; } };\nFoo make();\nint run() { Foo f = make(); return f.v(); }\n",
    )
    .expect("write main.cpp");

    let resolved = resolve_fixture("cpp-recv", &dir, &["src/main.cpp"]);
    let _ = std::fs::remove_dir_all(&dir);

    let v_call = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls && e.target.starts_with("method:")
    });
    assert!(
        v_call.is_some(),
        "expected run→method:v instance-method edge, got: {resolved:#?}"
    );
    let metadata = v_call.unwrap().metadata.as_ref().expect("metadata");
    assert_eq!(metadata["resolvedBy"].as_str(), Some("instance-method"));
    assert_eq!(
        (metadata["confidence"].as_f64().expect("confidence") * 1000.0).round() as i64,
        900
    );
}

#[test]
fn conformance_pass_resolves_chained_call_via_supertype() {
    // Conformance second pass (index.ts:877-904 / #750): `Factory.create().ping()`
    // where create() returns Sub and ping lives on Sub's supertype Base —
    // resolvable only after implements/extends edges exist. the upstream 1.0.1 emits
    // run→method:ping 0.85 instance-method (captured ground truth).
    let dir = fresh_fixture_dir("conformance");
    std::fs::write(
        dir.join("src/Pingable.java"),
        "interface Pingable { void ping(); }\n",
    )
    .expect("write Pingable.java");
    std::fs::write(
        dir.join("src/Base.java"),
        "class Base implements Pingable { public void ping() {} }\n",
    )
    .expect("write Base.java");
    std::fs::write(dir.join("src/Sub.java"), "class Sub extends Base {}\n")
        .expect("write Sub.java");
    std::fs::write(
        dir.join("src/Factory.java"),
        "class Factory {\n    static Sub create() { return new Sub(); }\n    void run() { Factory.create().ping(); }\n}\n",
    )
    .expect("write Factory.java");

    let resolved = resolve_fixture(
        "conformance",
        &dir,
        &[
            "src/Pingable.java",
            "src/Base.java",
            "src/Sub.java",
            "src/Factory.java",
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);

    // The chained `run→method:ping` edge can only come from the conformance
    // second pass (ping is not on Sub directly; it walks Sub→Base→ping).
    let ping_call = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls
            && e.target.starts_with("method:")
            && e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("instance-method")
    });
    assert!(
        ping_call.is_some(),
        "expected run→method:ping conformance edge, got: {resolved:#?}"
    );
    assert_eq!(
        (ping_call.unwrap().metadata.as_ref().expect("metadata")["confidence"]
            .as_f64()
            .expect("confidence")
            * 1000.0)
            .round() as i64,
        850
    );
}

#[test]
fn php_this_prop_typed_property_resolves_in_pass_one() {
    // #1220: `$this->dep->handle()` with a typed property `private Foo $dep;`
    // resolves to Foo::handle in pass 1 (the property's own type, no supertype
    // walk needed) at 0.9 instance-method.
    let dir = fresh_fixture_dir("php-thisprop-typed");
    std::fs::write(
        dir.join("src/Foo.php"),
        "<?php\nclass Foo {\n    function handle() {}\n}\n",
    )
    .expect("write Foo.php");
    std::fs::write(
        dir.join("src/Svc.php"),
        "<?php\nclass Svc {\n    private Foo $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n",
    )
    .expect("write Svc.php");

    let resolved = resolve_fixture("php-thisprop-typed", &dir, &["src/Foo.php", "src/Svc.php"]);
    let _ = std::fs::remove_dir_all(&dir);

    let handle_call = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls
            && e.target.starts_with("method:")
            && e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("instance-method")
    });
    assert!(
        handle_call.is_some(),
        "expected run→Foo::handle instance-method edge, got: {resolved:#?}"
    );
    assert_eq!(
        (handle_call.unwrap().metadata.as_ref().expect("metadata")["confidence"]
            .as_f64()
            .expect("confidence")
            * 1000.0)
            .round() as i64,
        900
    );
}

#[test]
fn php_this_prop_interface_typed_resolves_via_conformance() {
    // #1220: `$this->dep->handle()` where `$dep` is typed to an interface and
    // `handle()` lives on that interface. The method is not on any concrete class
    // reachable in pass 1 — it resolves ONLY after the conformance pass walks the
    // property's declared type to its supertype (the interface).
    let dir = fresh_fixture_dir("php-thisprop-iface");
    std::fs::write(
        dir.join("src/Handler.php"),
        "<?php\ninterface Handler {\n    function handle();\n}\n",
    )
    .expect("write Handler.php");
    std::fs::write(
        dir.join("src/Impl.php"),
        "<?php\nclass Impl implements Handler {\n    function handle() {}\n}\n",
    )
    .expect("write Impl.php");
    std::fs::write(
        dir.join("src/Svc.php"),
        "<?php\nclass Svc {\n    private Handler $dep;\n    function run() {\n        $this->dep->handle();\n    }\n}\n",
    )
    .expect("write Svc.php");

    let resolved = resolve_fixture(
        "php-thisprop-iface",
        &dir,
        &["src/Handler.php", "src/Impl.php", "src/Svc.php"],
    );
    let _ = std::fs::remove_dir_all(&dir);

    let handle_call = resolved.iter().find(|e| {
        e.kind == codegraph_core::types::EdgeKind::Calls
            && e.target.starts_with("method:")
            && e.metadata.as_ref().and_then(|m| m["resolvedBy"].as_str()) == Some("instance-method")
    });
    assert!(
        handle_call.is_some(),
        "expected run→Handler::handle conformance edge, got: {resolved:#?}"
    );
}
