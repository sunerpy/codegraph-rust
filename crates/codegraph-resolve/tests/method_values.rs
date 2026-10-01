//! Python and Go method values passed as callbacks (upstream v1.6.1 #1820).
//!
//! Ported from upstream `function-ref.test.ts`: a member value keeps its
//! receiver path and resolves through the receiver's scope before a unique
//! name; ambiguity, non-callable members and unknowable receivers stay
//! unlinked.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, NodeKind};
use codegraph_extract::{detect_language, extract_file};
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

impl Project {
    fn store(&self) -> &Store {
        self.store.as_ref().expect("store")
    }

    /// Function-ref `references` edges into callables named `name`.
    fn fn_ref_edges_into(&self, name: &str) -> Vec<Edge> {
        self.store()
            .nodes_by_name(name)
            .expect("nodes by name")
            .into_iter()
            .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
            .flat_map(|node| {
                self.store()
                    .edges_by_target_kind(&node.id, Some(EdgeKind::References))
                    .expect("references")
            })
            .filter(|edge| {
                edge.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("fnRef"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            })
            .collect()
    }

    /// Sorted names of the edges' sources.
    fn source_names(&self, edges: &[Edge]) -> Vec<String> {
        let mut names = edges
            .iter()
            .map(|edge| {
                self.store()
                    .node_by_id(&edge.source)
                    .expect("source query")
                    .expect("source node")
                    .name
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn target(&self, edge: &Edge) -> codegraph_core::types::Node {
        self.store()
            .node_by_id(&edge.target)
            .expect("target query")
            .expect("target node")
    }

    /// Sorted names of every `calls`/`references` caller of the one method
    /// named `name`.
    fn callers_of_method(&self, name: &str) -> Vec<String> {
        let method = self
            .store()
            .nodes_by_name(name)
            .expect("nodes by name")
            .into_iter()
            .find(|node| node.kind == NodeKind::Method)
            .unwrap_or_else(|| panic!("missing method {name}"));
        let mut names = [EdgeKind::Calls, EdgeKind::References]
            .into_iter()
            .flat_map(|kind| {
                self.store()
                    .edges_by_target_kind(&method.id, Some(kind))
                    .expect("incoming")
            })
            .map(|edge| {
                self.store()
                    .node_by_id(&edge.source)
                    .expect("source query")
                    .expect("source node")
                    .name
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }
}

fn resolve_project(files: &[(&str, &str)]) -> Project {
    let root = std::env::temp_dir().join(format!(
        "codegraph-method-values-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    for (relative, source) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, source).expect("write fixture");
    }
    let mut store = Store::open(&root.join("graph.db")).expect("open store");
    for (relative, _) in files {
        let result = extract_file(&root, relative).expect("extract fixture");
        assert!(result.errors.is_empty(), "{relative}: {:?}", result.errors);
        store
            .upsert_file(&FileRecord {
                path: (*relative).to_string(),
                content_hash: "fixture".to_string(),
                language: detect_language(relative),
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
    let root_str = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(root_str.clone());
    resolver.initialize(&codegraph_resolve::StoreResolutionContext::new(
        &store, &root_str,
    ));
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve fixture");
    Project {
        root,
        store: Some(store),
    }
}

#[test]
fn python_member_value_through_an_annotated_base_field_is_a_caller() {
    let project = resolve_project(&[
        (
            "store.py",
            "class Base:\n    pass\n\nclass Store(Base):\n    def fetch(self, ids):\n        return ids\n",
        ),
        (
            "consumer.py",
            "from concurrent.futures import ThreadPoolExecutor\n\
             from store import Base\n\
             \n\
             class Consumer:\n\
             \x20   def __init__(self, store: Base):\n\
             \x20       self.store = store\n\
             \n\
             \x20   def direct(self, ids):\n\
             \x20       return self.store.fetch(ids)\n\
             \n\
             \x20   def via_callback(self, ids, pool: ThreadPoolExecutor):\n\
             \x20       return pool.submit(self.store.fetch, ids)\n",
        ),
    ]);
    let edges = project.fn_ref_edges_into("fetch");
    assert_eq!(project.source_names(&edges), vec!["via_callback"]);
    assert!(
        project
            .callers_of_method("fetch")
            .contains(&"via_callback".to_string())
    );
}

#[test]
fn python_unknown_receivers_stay_ambiguous_with_test_doubles_bases_or_namesakes() {
    let consumer = "class Consumer:\n\
                    \x20   def __init__(self, store):\n\
                    \x20       self.store = store\n\
                    \x20   def via_callback(self, pool, ids):\n\
                    \x20       return pool.submit(self.store.fetch, ids)\n";
    for files in [
        vec![
            (
                "store.py",
                "class Store:\n    def fetch(self, ids):\n        return ids\n",
            ),
            (
                "tests/test_store.py",
                "class FakeStore:\n    def fetch(self, ids):\n        return ids\n",
            ),
            ("consumer.py", consumer),
        ],
        vec![
            (
                "base.py",
                "class Base:\n    def fetch(self, ids):\n        raise NotImplementedError(\"subclass\")\n",
            ),
            (
                "store.py",
                "from base import Base\nclass Store(Base):\n    def fetch(self, ids):\n        return ids\n",
            ),
            ("consumer.py", consumer),
        ],
        vec![
            (
                "a.py",
                "class A:\n    def fetch(self, ids):\n        return ids\n",
            ),
            (
                "b.py",
                "class B:\n    def fetch(self, ids):\n        return ids\n",
            ),
            ("consumer.py", consumer),
        ],
    ] {
        let project = resolve_project(&files);
        assert_eq!(project.fn_ref_edges_into("fetch"), Vec::new(), "{files:?}");
    }
}

#[test]
fn go_method_values_are_callers_and_go_statements_stay_calls() {
    let project = resolve_project(&[
        (
            "store.go",
            "package demo\n\ntype Store struct{}\n\nfunc (s *Store) Fetch(ids []string) []string { return ids }\n",
        ),
        (
            "consumer.go",
            "package demo\n\n\
             func Submit(fn func([]string) []string, ids []string) []string { return fn(ids) }\n\n\
             type Consumer struct{ store *Store }\n\n\
             func (c *Consumer) Direct(ids []string) []string { return c.store.Fetch(ids) }\n\n\
             func (c *Consumer) ViaGo(ids []string) { go c.store.Fetch(ids) }\n\n\
             func (c *Consumer) ViaSubmit(ids []string) []string { return Submit(c.store.Fetch, ids) }\n",
        ),
    ]);
    let callers = project.callers_of_method("Fetch");
    for caller in ["Direct", "ViaGo", "ViaSubmit"] {
        assert!(
            callers.contains(&caller.to_string()),
            "{caller}: {callers:?}"
        );
    }
    assert_eq!(
        project.source_names(&project.fn_ref_edges_into("Fetch")),
        vec!["ViaSubmit"]
    );
}

#[test]
fn python_receiver_identity_beats_same_file_and_imported_decoys() {
    let project = resolve_project(&[
        (
            "store.py",
            "class Store:\n    def fetch(self, ids):\n        return ids\n",
        ),
        (
            "main.py",
            "from store import Store as Actual\n\
             class Store:\n\
             \x20   def fetch(self, ids):\n\
             \x20       return ids\n\
             class Consumer:\n\
             \x20   def __init__(self, store: Actual):\n\
             \x20       self.store = store\n\
             \x20   def callback(self, pool, ids):\n\
             \x20       return pool.submit(self.store.fetch, ids)\n\
             \x20   def assigned(self):\n\
             \x20       cb = self.store.fetch\n\
             \x20   def collected(self):\n\
             \x20       return [self.store.fetch]\n\
             def typed(obj: Actual, pool):\n\
             \x20   pool.submit(obj.fetch)\n\
             def static(pool):\n\
             \x20   pool.submit(Actual.fetch)\n",
        ),
    ]);
    let edges = project.fn_ref_edges_into("fetch");
    assert_eq!(
        project.source_names(&edges),
        vec!["assigned", "callback", "collected", "static", "typed"]
    );
    assert!(
        edges
            .iter()
            .all(|edge| project.target(edge).file_path == "store.py")
    );
}

#[test]
fn python_same_file_ambiguity_and_noncallable_members_stay_unlinked() {
    let project = resolve_project(&[(
        "main.py",
        "class A:\n\
         \x20   def fetch(self):\n\
         \x20       return 1\n\
         class B:\n\
         \x20   def fetch(self):\n\
         \x20       return 2\n\
         class Data:\n\
         \x20   fetch = 42\n\
         class Property:\n\
         \x20   @property\n\
         \x20   def fetch(self):\n\
         \x20       return 42\n\
         def unknown(obj, pool):\n\
         \x20   pool.submit(obj.fetch)\n\
         def data(obj: Data, pool):\n\
         \x20   pool.submit(obj.fetch)\n\
         def prop(obj: Property, pool):\n\
         \x20   pool.submit(obj.fetch)\n\
         def bare(fetch, pool):\n\
         \x20   pool.submit(fetch)\n\
         class Own:\n\
         \x20   def fetch(self):\n\
         \x20       return 3\n\
         \x20   def bound(self, pool):\n\
         \x20       pool.submit(self.fetch)\n\
         \x20   @classmethod\n\
         \x20   def class_bound(cls, pool):\n\
         \x20       pool.submit(cls.fetch)\n",
    )]);
    let edges = project.fn_ref_edges_into("fetch");
    assert_eq!(project.source_names(&edges), vec!["bound", "class_bound"]);
    assert!(
        edges
            .iter()
            .all(|edge| project.target(edge).qualified_name == "Own::fetch")
    );
}

#[test]
fn python_typed_constructed_and_inherited_receivers_exclude_noncallable_values() {
    let project = resolve_project(&[(
        "main.py",
        "class Store:\n\
         \x20   def fetch(self):\n\
         \x20       return 1\n\
         class Child(Store):\n\
         \x20   def inherited(self, pool):\n\
         \x20       pool.submit(self.fetch)\n\
         class Data:\n\
         \x20   def __init__(self):\n\
         \x20       self.fetch = 42\n\
         class Override(Store):\n\
         \x20   def __init__(self):\n\
         \x20       self.fetch = 42\n\
         class Consumer:\n\
         \x20   def __init__(self):\n\
         \x20       self.store = Store()\n\
         \x20   def keyword(self, pool):\n\
         \x20       pool.submit(callback=self.store.fetch)\n\
         def constructor(pool):\n\
         \x20   obj = Store()\n\
         \x20   pool.submit(obj.fetch)\n\
         def partial_ref(obj: Store):\n\
         \x20   return partial(obj.fetch, 1)\n\
         def mapped(obj: Store, xs):\n\
         \x20   return map(obj.fetch, xs)\n\
         def data(obj: Data, pool):\n\
         \x20   pool.submit(obj.fetch)\n\
         def override(obj: Override, pool):\n\
         \x20   pool.submit(obj.fetch)\n\
         def primitive(obj: int, pool):\n\
         \x20   pool.submit(obj.fetch)\n\
         def literal(pool):\n\
         \x20   obj = 42\n\
         \x20   pool.submit(obj.fetch)\n\
         def reassigned(obj: Store, pool):\n\
         \x20   obj = 42\n\
         \x20   pool.submit(obj.fetch)\n\
         def direct(obj: Store):\n\
         \x20   obj.fetch()\n",
    )]);
    assert_eq!(
        project.source_names(&project.fn_ref_edges_into("fetch")),
        vec![
            "constructor",
            "inherited",
            "keyword",
            "mapped",
            "partial_ref"
        ]
    );
    let fetch = project
        .store()
        .nodes_by_name("fetch")
        .expect("nodes")
        .into_iter()
        .find(|node| node.kind == NodeKind::Method)
        .expect("fetch");
    let calls = project
        .store()
        .edges_by_target_kind(&fetch.id, Some(EdgeKind::Calls))
        .expect("calls");
    assert!(project.source_names(&calls).contains(&"direct".to_string()));
}

#[test]
fn go_receiver_types_disambiguate_method_values_and_reject_external_fields() {
    let project = resolve_project(&[(
        "main.go",
        "package demo\n\
         import \"database/sql\"\n\
         type Store struct{}\n\
         func (s *Store) Fetch() {}\n\
         type Decoy struct{}\n\
         func (d *Decoy) Fetch() {}\n\
         type Consumer struct { store *Store; external *sql.DB }\n\
         func (c *Consumer) Callback() { Submit(c.store.Fetch) }\n\
         func Typed(s *Store) { Submit(s.Fetch) }\n\
         func Assigned(s *Store) { cb := s.Fetch }\n\
         func Collected(s *Store) { table := []func(){s.Fetch} }\n\
         func MethodExpression() { Submit(Store.Fetch) }\n\
         func (c *Consumer) External() { Submit(c.external.Fetch) }\n\
         func Unknown(obj interface{}) { Submit(obj.Fetch) }\n",
    )]);
    let edges = project.fn_ref_edges_into("Fetch");
    assert_eq!(
        project.source_names(&edges),
        vec![
            "Assigned",
            "Callback",
            "Collected",
            "MethodExpression",
            "Typed"
        ]
    );
    assert!(
        edges
            .iter()
            .all(|edge| project.target(edge).qualified_name == "Store::Fetch")
    );
}

#[test]
fn unknown_receivers_never_bind_to_a_project_unique_method() {
    // Exactly ONE project method of each name, so the old unique-name
    // fallback would have bound every value below. Nothing proves what these
    // receivers are: `store` is assigned from an unannotated parameter, `obj`
    // is an unannotated parameter, and the Go `obj` has a type from outside
    // the project.
    let python = resolve_project(&[
        (
            "store.py",
            "class Store:\n    def fetch(self, ids):\n        return ids\n",
        ),
        (
            "consumer.py",
            "class Consumer:\n\
             \x20   def __init__(self, store):\n\
             \x20       self.store = store\n\
             \x20   def via_field(self, pool, ids):\n\
             \x20       return pool.submit(self.store.fetch, ids)\n\
             def via_param(obj, pool):\n\
             \x20   return pool.submit(obj.fetch)\n",
        ),
    ]);
    let go = resolve_project(&[
        (
            "store.go",
            "package demo\n\ntype Store struct{}\n\nfunc (s *Store) Fetch(ids []string) []string { return ids }\n",
        ),
        (
            "consumer.go",
            "package demo\n\n\
             import \"example.com/remote\"\n\n\
             func Submit(fn func([]string) []string, ids []string) []string { return fn(ids) }\n\n\
             func ViaParam(obj remote.Client, ids []string) []string { return Submit(obj.Fetch, ids) }\n",
        ),
    ]);
    let bound =
        |project: &Project, name: &str| project.source_names(&project.fn_ref_edges_into(name));
    assert_eq!(
        (bound(&python, "fetch"), bound(&go, "Fetch")),
        (Vec::<String>::new(), Vec::<String>::new())
    );
}
