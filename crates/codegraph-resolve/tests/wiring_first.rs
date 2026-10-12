//! Every persist entry resolves the wiring references (imports, extends,
//! implements) first and persists them, then the rest (upstream #1789's
//! prerequisite phase, W4-CPP-05): a call on a receiver typed as a subclass
//! reaches the method its base declares even when the `extends` reference
//! sits in the same batch. The serial and the batched entries end with the
//! same graph.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{FileRecord, Language};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::{ReferenceResolver, StoreResolutionContext};
use codegraph_store::Store;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

/// How the references are resolved.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Serial,
    Batched(usize),
}

/// The canonical graph a resolution leaves: every edge and every remaining
/// unresolved reference, by qualified names and positions, sorted.
#[derive(Debug, PartialEq, Eq)]
struct Canonical {
    edges: Vec<String>,
    unresolved: Vec<String>,
}

fn temp_path(slug: &str) -> PathBuf {
    let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "codegraph-wiring-first-{slug}-{}-{nonce}",
        std::process::id()
    ))
}

fn write_fixture(root: &Path, files: &[(&str, &str)]) {
    for (relative, source) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("create parent");
        std::fs::write(path, source).expect("write fixture source");
    }
}

fn resolve(slug: &str, files: &[(&str, &str)], entry: Entry) -> Canonical {
    let root = temp_path(slug);
    std::fs::create_dir_all(&root).expect("create fixture root");
    write_fixture(&root, files);
    let db = temp_path(&format!("{slug}-db")).with_extension("db");
    let mut store = Store::open(&db).expect("open fixture store");
    let mut paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
    paths.sort_unstable();
    for path in paths {
        let result = extract_file(&root, path).expect("extract fixture");
        let language = detect_language(path);
        assert_ne!(language, Language::Unknown, "{path}");
        store
            .upsert_file(&FileRecord {
                path: path.to_string(),
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
    match entry {
        Entry::Serial => {
            resolver.resolve_and_persist(&mut store).expect("serial");
        }
        Entry::Batched(size) => {
            resolver
                .resolve_and_persist_batched(&mut store, size)
                .expect("batched");
        }
    }
    let qualified = |id: &str| {
        store.node_by_id(id).expect("read node").map_or_else(
            || id.to_string(),
            |node| format!("{} {}", node.kind.as_str(), node.qualified_name),
        )
    };
    let mut edges: Vec<String> = store
        .all_edges()
        .expect("read edges")
        .iter()
        .map(|edge| {
            format!(
                "{} -[{}]-> {} at {:?}:{:?} {}",
                qualified(&edge.source),
                edge.kind.as_str(),
                qualified(&edge.target),
                edge.line,
                edge.col,
                edge.metadata
                    .as_ref()
                    .map_or_else(String::new, ToString::to_string)
            )
        })
        .collect();
    edges.sort();
    let mut unresolved: Vec<String> = store
        .all_unresolved_refs()
        .expect("read references")
        .iter()
        .map(|reference| {
            format!(
                "{} -[{}]-> {} at {}:{}",
                qualified(&reference.from_node_id),
                reference.reference_kind.as_str(),
                reference.reference_name,
                reference.line,
                reference.col
            )
        })
        .collect();
    unresolved.sort();
    drop(store);
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_dir_all(root);
    Canonical { edges, unresolved }
}

/// Resolve `files` through the serial entry and through batches of one and of
/// many references, assert the three agree, and return the graph.
fn resolved_everywhere(slug: &str, files: &[(&str, &str)]) -> Canonical {
    let serial = resolve(slug, files, Entry::Serial);
    for size in [1, 1000] {
        assert_eq!(
            resolve(slug, files, Entry::Batched(size)),
            serial,
            "batches of {size} and the serial entry disagree"
        );
    }
    serial
}

fn calls(graph: &Canonical, from: &str, to: &str) -> bool {
    graph
        .edges
        .iter()
        .any(|edge| edge.starts_with(&format!("{from} -[calls]-> {to} at")))
}

#[test]
fn a_cpp_call_on_a_subclass_reaches_the_method_the_alias_named_base_declares() {
    let graph = resolved_everywhere(
        "cpp",
        &[
            (
                "table/internal_iterator.h",
                "namespace rocksdb {\nclass Slice {};\ntemplate <class TValue>\nclass InternalIteratorBase {\n public:\n  virtual void Next() = 0;\n  void SeekToFirst() {}\n};\nusing InternalIterator = InternalIteratorBase<Slice>;\n}  // namespace rocksdb\n",
            ),
            (
                "table/merging_iterator.cc",
                "#include \"table/internal_iterator.h\"\nnamespace rocksdb {\nclass MergingIterator : public InternalIterator {\n public:\n  void Next() override {}\n};\nvoid Rewind(MergingIterator* iter) {\n  iter->SeekToFirst();\n}\n}  // namespace rocksdb\n",
            ),
        ],
    );
    assert!(
        calls(
            &graph,
            "function rocksdb::Rewind",
            "method rocksdb::InternalIteratorBase::SeekToFirst"
        ),
        "{graph:#?}"
    );
}

#[test]
fn a_csharp_call_on_a_subclass_reaches_the_base_method_not_a_namesake() {
    let graph = resolved_everywhere(
        "csharp",
        &[
            (
                "src/JsonWriter.cs",
                "namespace Json\n{\n    public abstract class JsonWriter\n    {\n        public virtual void WriteEndObject() { }\n    }\n\n    public class JsonTextWriter : JsonWriter\n    {\n        public void Flush() { }\n    }\n}\n",
            ),
            (
                "tests/CustomWriter.cs",
                "namespace Json.Tests\n{\n    public class CustomWriter\n    {\n        public void WriteEndObject() { }\n    }\n}\n",
            ),
            (
                "src/Program.cs",
                "namespace Json\n{\n    public class Program\n    {\n        public void Run()\n        {\n            JsonTextWriter writer = new JsonTextWriter();\n            writer.WriteEndObject();\n        }\n    }\n}\n",
            ),
        ],
    );
    assert!(
        calls(
            &graph,
            "method Json::Program::Run",
            "method Json::JsonWriter::WriteEndObject"
        ),
        "{graph:#?}"
    );
    assert!(
        !calls(
            &graph,
            "method Json::Program::Run",
            "method Json.Tests::CustomWriter::WriteEndObject"
        ),
        "{graph:#?}"
    );
}

#[test]
fn a_java_call_on_a_subclass_reaches_the_inherited_method() {
    let graph = resolved_everywhere(
        "java",
        &[
            (
                "src/app/Base.java",
                "package app;\n\npublic class Base {\n    public void run() { }\n}\n",
            ),
            (
                "src/app/Child.java",
                "package app;\n\npublic class Child extends Base {\n}\n",
            ),
            (
                "src/app/Use.java",
                "package app;\n\npublic class Use {\n    public void go() {\n        Child c = new Child();\n        c.run();\n    }\n}\n",
            ),
        ],
    );
    assert!(
        calls(&graph, "method app::Use::go", "method app::Base::run"),
        "{graph:#?}"
    );
}
