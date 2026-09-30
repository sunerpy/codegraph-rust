//! A named Python import proves its receiver exists, not the requested
//! attribute (upstream v1.6.1 #2040): `task.delay()` enqueues work and must not
//! become a call to the task function. Ported from upstream
//! `__tests__/function-ref.test.ts`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{EdgeKind, FileRecord, Language, NodeKind};
use codegraph_extract::extract_file;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

static NONCE: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
    store: Option<Store>,
}

impl Drop for Project {
    fn drop(&mut self) {
        drop(self.store.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn resolve_project(files: &[(&str, &str)]) -> Project {
    let root = std::env::temp_dir().join(format!(
        "codegraph-python-members-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    let mut store = Store::open(&root.join("graph.db")).expect("open store");
    for (relative, source) in files {
        std::fs::write(root.join(relative), source).expect("write fixture");
    }
    for (relative, _) in files {
        let result = extract_file(&root, relative).expect("extract fixture");
        store
            .upsert_file(&FileRecord {
                path: (*relative).to_string(),
                content_hash: "fixture".to_string(),
                language: Language::Python,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
                generated: false,
            })
            .expect("upsert file");
        store.upsert_nodes(&result.nodes).expect("upsert nodes");
        store.insert_edges(&result.edges).expect("insert edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert unresolved refs");
    }
    let mut resolver = ReferenceResolver::new(root.to_string_lossy().to_string());
    resolver.initialize(&codegraph_resolve::StoreResolutionContext::new(
        &store,
        root.to_string_lossy().as_ref(),
    ));
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve fixture");
    Project {
        root,
        store: Some(store),
    }
}

fn node(project: &Project, kind: NodeKind, name: &str) -> String {
    project
        .store
        .as_ref()
        .expect("store")
        .nodes_by_kind(kind)
        .expect("nodes by kind")
        .into_iter()
        .find(|node| node.name == name)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"))
        .id
}

/// Names of the sources of `kind` edges into `target`; `fn_ref` keeps only
/// callable-value references (`References` carrying `fnRef: true`).
fn sources(project: &Project, target: &str, kind: EdgeKind, fn_ref: bool) -> Vec<String> {
    let store = project.store.as_ref().expect("store");
    let mut names: Vec<String> = store
        .edges_by_target_kind(target, Some(kind))
        .expect("incoming edges")
        .into_iter()
        .filter(|edge| {
            !fn_ref
                || edge
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("fnRef"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        })
        .filter_map(|edge| store.node_by_id(&edge.source).expect("source query"))
        .map(|node| node.name)
        .collect();
    names.sort();
    names.dedup();
    names
}

const TASKS: &str = "from celery import shared_task
@shared_task
def send_welcome(item_id):
    return item_id
class Store:
    @staticmethod
    def fetch():
        return 1
settings = None
";

fn main_module(module: &str) -> String {
    format!(
        "from {module} import send_welcome as welcome, Store as Actual, settings
def direct():
    welcome(1)
def callback(pool):
    pool.submit(welcome, 1)
def enqueue():
    welcome.delay(1)
    welcome.apply_async(args=[1])
def member_values(pool):
    pool.submit(welcome.delay, 1)
    cb = welcome.custom_attribute
    return [welcome.apply_async]
def missing_members():
    Actual.missing()
    settings.missing()
def known_member():
    Actual.fetch()
def known_callback(pool):
    pool.submit(Actual.fetch)
def construct():
    return Actual()
"
    )
}

#[test]
fn imported_members_do_not_fall_back_to_their_receiver() {
    for module in ["tasks", ".tasks"] {
        let project = resolve_project(&[("tasks.py", TASKS), ("main.py", &main_module(module))]);
        let task = node(&project, NodeKind::Function, "send_welcome");
        assert_eq!(
            sources(&project, &task, EdgeKind::Calls, false),
            vec!["direct"],
            "{module}"
        );
        assert_eq!(
            sources(&project, &task, EdgeKind::References, true),
            vec!["callback"],
            "{module}"
        );
        let missing = node(&project, NodeKind::Function, "missing_members");
        let store = project.store.as_ref().expect("store");
        let outgoing: Vec<_> = store
            .edges_by_source_kind(&missing, None)
            .expect("outgoing")
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls | EdgeKind::Instantiates))
            .collect();
        assert!(outgoing.is_empty(), "{module}: {outgoing:#?}");
        let fetch = node(&project, NodeKind::Method, "fetch");
        assert_eq!(
            sources(&project, &fetch, EdgeKind::Calls, false),
            vec!["known_member"],
            "{module}"
        );
        assert_eq!(
            sources(&project, &fetch, EdgeKind::References, true),
            vec!["known_callback"],
            "{module}"
        );
        let class = node(&project, NodeKind::Class, "Store");
        assert_eq!(
            sources(&project, &class, EdgeKind::Instantiates, false),
            vec!["construct"],
            "{module}"
        );
    }
}
