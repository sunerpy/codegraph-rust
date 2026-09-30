//! Awaited JS/TS receiver inference (upstream v1.6.1 #1840/#1885).
//!
//! Ported from upstream `__tests__/awaited-receiver.test.ts`; the incremental
//! invalidation cases are covered by the per-pass `SourceFacts` cache, which a
//! fresh resolution pass always rebuilds.

use std::path::{Path, PathBuf};
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

fn language(path: &str) -> Language {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("ts") => Language::TypeScript,
        Some("tsx") => Language::Tsx,
        other => panic!("unexpected extension: {other:?}"),
    }
}

fn resolve_project(files: &[(&str, &str)]) -> Project {
    let root = std::env::temp_dir().join(format!(
        "codegraph-awaited-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).expect("create fixture root");
    for (relative, source) in files {
        std::fs::write(root.join(relative), source).expect("write fixture");
    }
    let mut store = Store::open(&root.join("graph.db")).expect("open store");
    for (relative, _) in files {
        let result = extract_file(&root, relative).expect("extract fixture");
        store
            .upsert_file(&FileRecord {
                path: (*relative).to_string(),
                content_hash: "fixture".to_string(),
                language: language(relative),
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

/// `file:qualifiedName` of every `calls` target of function `name` in `file`,
/// with the call line.
fn calls(project: &Project, name: &str, file: &str) -> Vec<(String, i64)> {
    let store = project.store.as_ref().expect("store");
    let source = store
        .nodes_by_kind(NodeKind::Function)
        .expect("functions")
        .into_iter()
        .find(|node| node.name == name && node.file_path == file)
        .unwrap_or_else(|| panic!("missing function {name} in {file}"));
    let mut targets: Vec<(String, i64)> = store
        .edges_by_source_kind(&source.id, Some(EdgeKind::Calls))
        .expect("calls")
        .into_iter()
        .filter_map(|edge| {
            let line = edge.line.unwrap_or(0);
            store
                .node_by_id(&edge.target)
                .expect("target query")
                .map(|node| (format!("{}:{}", node.file_path, node.qualified_name), line))
        })
        .collect();
    targets.sort();
    targets
}

fn targets(project: &Project, name: &str) -> Vec<String> {
    calls(project, name, "caller.ts")
        .into_iter()
        .map(|(target, _)| target)
        .collect()
}

const ENGINE: &str = "export class Engine { run() {} }\nexport async function makeEngine(): Promise<Engine> { return new Engine(); }";

#[test]
fn follows_an_imported_factory_alias_not_an_unrelated_namesake() {
    let project = resolve_project(&[
        ("engine.ts", ENGINE),
        (
            "decoy.ts",
            "export async function load(): Promise<string> { return \"\"; }",
        ),
        (
            "caller.ts",
            "import { makeEngine as load } from \"./engine\";\nexport async function drive() { const handle = await load(); handle.run(); }",
        ),
    ]);
    assert_eq!(
        targets(&project, "drive"),
        vec!["engine.ts:Engine::run", "engine.ts:makeEngine"]
    );
}

#[test]
fn resolves_return_type_aliases_in_the_factory_module() {
    let project = resolve_project(&[
        ("engine.ts", ENGINE),
        (
            "factory.ts",
            "import { Engine as Service } from \"./engine\";\nexport async function load(): Promise<Service> { return new Service(); }",
        ),
        (
            "caller.ts",
            "import { load } from \"./factory\";\nclass Service { run() {} }\nexport async function drive() { const handle = await load(); handle.run(); }",
        ),
    ]);
    assert_eq!(
        targets(&project, "drive"),
        vec!["engine.ts:Engine::run", "factory.ts:load"]
    );
}

#[test]
fn a_factory_hidden_by_a_parameter_is_not_inferred() {
    let project = resolve_project(&[
        ("engine.ts", ENGINE),
        (
            "caller.ts",
            "import { makeEngine } from \"./engine\";\nexport async function drive(makeEngine: () => Promise<string>) { const handle = await makeEngine(); handle.run(); }",
        ),
    ]);
    assert!(!targets(&project, "drive").contains(&"engine.ts:Engine::run".to_string()));
}

#[test]
fn same_named_receivers_in_sibling_blocks_stay_distinct() {
    let project = resolve_project(&[(
        "caller.ts",
        "class PaneManager { split() {} }
async function text(): Promise<string> { return \"\"; }
async function pane(): Promise<PaneManager> { return new PaneManager(); }
export async function drive() {
  { const value = await text(); value.split(); }
  { const value = await pane(); value.split(); }
}",
    )]);
    let splits: Vec<(String, i64)> = calls(&project, "drive", "caller.ts")
        .into_iter()
        .filter(|(target, _)| target.ends_with("PaneManager::split"))
        .collect();
    assert_eq!(
        splits,
        vec![("caller.ts:PaneManager::split".to_string(), 6)]
    );
}

#[test]
fn captured_bindings_resolve_but_shadowing_parameters_do_not() {
    let project = resolve_project(&[(
        "caller.ts",
        &format!(
            "{ENGINE}
export async function outer() {{
  const handle = await makeEngine();
  function captured() {{ handle.run(); }}
  function shadow(handle: any) {{ handle.run(); }}
  return {{ captured, shadow }};
}}"
        ),
    )]);
    assert!(targets(&project, "captured").contains(&"caller.ts:Engine::run".to_string()));
    assert!(!targets(&project, "shadow").contains(&"caller.ts:Engine::run".to_string()));
}

#[test]
fn reads_a_multiline_factory_annotation_and_keeps_ordinary_receivers() {
    let project = resolve_project(&[(
        "caller.ts",
        "class Engine { run() {} }
class Decoy { run() {} }
async function load(
  input: string
): Promise<Engine> { return new Engine(); }
export async function drive() { const value = await load(''); value.run(); }
export function ordinary() { const engine = new Engine(); engine.run(); }",
    )]);
    assert_eq!(
        targets(&project, "drive"),
        vec!["caller.ts:Engine::run", "caller.ts:load"]
    );
    assert!(targets(&project, "ordinary").contains(&"caller.ts:Engine::run".to_string()));
}

#[test]
fn newline_terminated_bindings_resolve_but_chained_results_do_not() {
    let project = resolve_project(&[(
        "caller.ts",
        &format!(
            "{ENGINE}
export async function drive() {{
  const value = await makeEngine()
  value.run()
}}
export async function chained() {{
  const value = await makeEngine().toString();
  value.run();
}}"
        ),
    )]);
    assert!(targets(&project, "drive").contains(&"caller.ts:Engine::run".to_string()));
    assert!(!targets(&project, "chained").contains(&"caller.ts:Engine::run".to_string()));
}

#[test]
fn a_nearer_variable_binding_hides_the_local_factory_annotation() {
    let project = resolve_project(&[(
        "caller.ts",
        &format!(
            "{ENGINE}
export async function drive(other: () => Promise<string>) {{
  const makeEngine = other;
  const value = await makeEngine();
  value.run();
}}"
        ),
    )]);
    assert!(!targets(&project, "drive").contains(&"caller.ts:Engine::run".to_string()));
}

#[test]
fn tsx_consumer_follows_an_imported_ts_factory() {
    // The review finding behind #1840's Rust port: a `.tsx` caller importing a
    // `.ts` factory must not lose the typed receiver to a language filter.
    let project = resolve_project(&[
        ("engine.ts", ENGINE),
        (
            "caller.tsx",
            "import { makeEngine } from \"./engine\";\nexport async function drive() { const handle = await makeEngine(); handle.run(); }",
        ),
    ]);
    let found: Vec<String> = calls(&project, "drive", "caller.tsx")
        .into_iter()
        .map(|(target, _)| target)
        .collect();
    assert!(
        found.contains(&"engine.ts:Engine::run".to_string()),
        "{found:?}"
    );
}
