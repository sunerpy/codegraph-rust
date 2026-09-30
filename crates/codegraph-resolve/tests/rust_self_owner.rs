//! `self.method()` owner resolution across same-named Rust types (upstream
//! v1.6.1 #1861/#1882), ported from `__tests__/rust-self-owner.test.ts`.

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
        "codegraph-rust-self-owner-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("src")).expect("create fixture root");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"owners\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )
    .expect("write manifest");
    let mut store = Store::open(&root.join("graph.db")).expect("open store");
    for (file, source) in files {
        let relative = format!("src/{file}");
        std::fs::write(root.join(&relative), source).expect("write fixture");
        let result = extract_file(&root, &relative).expect("extract fixture");
        store
            .upsert_file(&FileRecord {
                path: relative.clone(),
                content_hash: "fixture".to_string(),
                language: Language::Rust,
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

/// `file:qualifiedName` of every call target of `Target::run` in `src/<file>`.
fn targets(project: &Project, file: &str) -> Vec<String> {
    let store = project.store.as_ref().expect("store");
    let caller = store
        .nodes_by_kind(NodeKind::Method)
        .expect("methods")
        .into_iter()
        .find(|node| {
            node.file_path == format!("src/{file}") && node.qualified_name == "Target::run"
        })
        .expect("caller Target::run");
    let mut found: Vec<String> = store
        .edges_by_source_kind(&caller.id, Some(EdgeKind::Calls))
        .expect("calls")
        .into_iter()
        .filter_map(|edge| store.node_by_id(&edge.target).expect("target query"))
        .map(|node| format!("{}:{}", node.file_path, node.qualified_name))
        .collect();
    found.sort();
    found
}

#[test]
fn does_not_borrow_a_missing_method_from_a_same_named_type_in_another_module() {
    let project = resolve_project(&[
        ("lib.rs", "pub mod caller; pub mod decoy;"),
        (
            "caller.rs",
            "pub struct Target;\nimpl Target { pub fn run(&self) { self.reset(); } }",
        ),
        (
            "decoy.rs",
            "pub struct Target;\nimpl Target { pub fn reset(&self) {} }",
        ),
    ]);
    assert_eq!(targets(&project, "caller.rs"), Vec::<String>::new());
}

#[test]
fn keeps_a_proven_local_owner_despite_a_same_named_type_elsewhere() {
    let project = resolve_project(&[
        ("lib.rs", "pub mod caller; pub mod decoy;"),
        (
            "caller.rs",
            "pub struct Target;\nimpl Target { pub fn reset(&self) {} }\nimpl Target { pub fn run(&self) { self.reset(); } }",
        ),
        (
            "decoy.rs",
            "pub struct Target;\nimpl Target { pub fn reset(&self) {} }",
        ),
    ]);
    assert_eq!(
        targets(&project, "caller.rs"),
        vec!["src/caller.rs:Target::reset"]
    );
}

#[test]
fn keeps_a_unique_owner_whose_impl_is_split_across_files() {
    let project = resolve_project(&[
        (
            "lib.rs",
            "pub mod caller;\npub struct Target;\nimpl Target { pub fn reset(&self) {} }",
        ),
        (
            "caller.rs",
            "use crate::Target;\nimpl Target { pub fn run(&self) { self.reset(); } }",
        ),
    ]);
    assert_eq!(
        targets(&project, "caller.rs"),
        vec!["src/lib.rs:Target::reset"]
    );
}

#[test]
fn declines_indistinguishable_inline_module_owners() {
    let project = resolve_project(&[(
        "lib.rs",
        "mod a {
    pub struct Target;
    impl Target { pub fn reset(&self) {} }
  }
  mod b {
    pub struct Target;
    impl Target { pub fn run(&self) { self.reset(); } }
  }",
    )]);
    assert_eq!(targets(&project, "lib.rs"), Vec::<String>::new());
}
