use std::path::{Path, PathBuf};

use codegraph_core::types::{EdgeKind, FileRecord, Language, NodeKind};
use codegraph_extract::extract_file;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn temp_root() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "codegraph-scala-companion-{}-{nanos}",
        std::process::id()
    ))
}

fn add_file(store: &mut Store, root: &Path, relative: &str) {
    let result = extract_file(root, relative).expect("extract Scala fixture");
    store
        .upsert_file(&FileRecord {
            path: relative.to_string(),
            content_hash: "fixture".to_string(),
            language: Language::Scala,
            size: 0,
            modified_at: 0,
            indexed_at: 0,
            node_count: result.nodes.len() as i64,
            errors: result.errors,
            generated: false,
        })
        .expect("upsert fixture file");
    store.upsert_nodes(&result.nodes).expect("upsert nodes");
    store
        .insert_edges(&result.edges)
        .expect("insert contains edges");
    store
        .insert_unresolved_refs(&result.unresolved_references)
        .expect("insert unresolved refs");
}

#[test]
fn scala_inheritance_prefers_type_over_earlier_companion_module() {
    let root = temp_root();
    std::fs::create_dir_all(&root).expect("create fixture root");
    // Put the module first so a stable line-order tie would select the wrong
    // target unless inheritance scoring explicitly prefers the real type.
    std::fs::write(
        root.join("ExtAgreement.scala"),
        r#"object ExtAgreement {
  val Kind = "agreement"
}
trait ExtAgreement {
  def extId: String = "x"
}
"#,
    )
    .expect("write companion fixture");
    std::fs::write(
        root.join("MExtAgreement.scala"),
        r#"class MExtAgreement extends ExtAgreement {
  def render(): String = extId
}
"#,
    )
    .expect("write subtype fixture");

    let db = root.join("graph.db");
    let mut store = Store::open(&db).expect("open store");
    add_file(&mut store, &root, "ExtAgreement.scala");
    add_file(&mut store, &root, "MExtAgreement.scala");

    let companion = store
        .nodes_by_kind(NodeKind::Module)
        .expect("query modules")
        .into_iter()
        .find(|node| node.name == "ExtAgreement")
        .expect("companion module");
    let trait_node = store
        .nodes_by_kind(NodeKind::Trait)
        .expect("query traits")
        .into_iter()
        .find(|node| node.name == "ExtAgreement")
        .expect("companion trait");
    let subtype = store
        .nodes_by_kind(NodeKind::Class)
        .expect("query classes")
        .into_iter()
        .find(|node| node.name == "MExtAgreement")
        .expect("subtype class");

    ReferenceResolver::new(root.to_string_lossy().into_owned())
        .resolve_and_persist(&mut store)
        .expect("resolve Scala inheritance");

    let inheritance = store
        .edges_by_source_kind(&subtype.id, Some(EdgeKind::Extends))
        .expect("query inheritance edges");
    assert!(
        inheritance.iter().any(|edge| edge.target == trait_node.id),
        "subtype must extend the trait; edges={inheritance:#?}"
    );
    assert!(
        inheritance.iter().all(|edge| edge.target != companion.id),
        "companion module must not capture inheritance; edges={inheritance:#?}"
    );
}

#[test]
fn scala_singleton_is_never_a_parent_but_owns_callable_methods() {
    let root = temp_root();
    std::fs::create_dir_all(&root).expect("create fixture root");
    // No same-named type exists: the singleton alone must not become a parent.
    std::fs::write(
        root.join("Registry.scala"),
        r#"object Registry {
  def lookup(key: String): String = key
}
"#,
    )
    .expect("write singleton fixture");
    std::fs::write(
        root.join("Client.scala"),
        r#"class Client extends Registry {
  def run(): String = Registry.lookup("k")
}
"#,
    )
    .expect("write client fixture");

    let mut store = Store::open(&root.join("graph.db")).expect("open store");
    add_file(&mut store, &root, "Registry.scala");
    add_file(&mut store, &root, "Client.scala");
    let client = store
        .nodes_by_kind(NodeKind::Class)
        .expect("query classes")
        .into_iter()
        .find(|node| node.name == "Client")
        .expect("client class");
    let run = store
        .nodes_by_kind(NodeKind::Method)
        .expect("query methods")
        .into_iter()
        .find(|node| node.qualified_name == "Client::run")
        .expect("client run");
    let lookup = store
        .nodes_by_kind(NodeKind::Method)
        .expect("query methods")
        .into_iter()
        .find(|node| node.qualified_name == "Registry::lookup")
        .expect("registry lookup");

    ReferenceResolver::new(root.to_string_lossy().into_owned())
        .resolve_and_persist(&mut store)
        .expect("resolve Scala singleton");

    let inheritance = store
        .edges_by_source_kind(&client.id, Some(EdgeKind::Extends))
        .expect("query inheritance edges");
    assert!(
        inheritance.is_empty(),
        "a singleton is not a parent type; edges={inheritance:#?}"
    );
    let calls = store
        .edges_by_source_kind(&run.id, Some(EdgeKind::Calls))
        .expect("query call edges");
    assert!(
        calls.iter().any(|edge| edge.target == lookup.id),
        "Registry.lookup() resolves on the singleton; edges={calls:#?}"
    );
    std::fs::remove_dir_all(&root).ok();
}
