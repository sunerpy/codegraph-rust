//! Tauri IPC-bridge integration tests (Tier 4 item 10, upstream issue #1543).
//!
//! Every test here drives the FULL pipeline — base extraction →
//! `extract_and_persist_frameworks` (where `TauriResolver::extract` emits the
//! `tauri:invoke:*` refs) → `resolve_and_persist` (where `TauriResolver::resolve`
//! turns them into edges) — over a fixture written into a temp directory, so the
//! worktree stays clean and nothing under `reference/` is touched.
//!
//! The fixtures are the ones MEASURED in the plan, not invented here:
//!   * `build_repro_fixture`   — the §2 reproduction (2 commands, 2 literal invokes)
//!   * `build_matrix_fixture`  — `/tmp/m-matrix`: 15 shape lines in `.ts` + 4 in `.tsx`
//!   * `build_adversarial_fixture` — `/tmp/m-rust`: 6 raw `tauri::command` hits, 1 real
//!
//! A count is asserted everywhere a presence check could pass on a fabricated
//! edge: the failure this tier's review rounds kept finding is an edge that EXISTS
//! but points at a function the developer never exposed over IPC.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;

fn unique_dir(slug: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "codegraph-tauri-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir fixture");
    dir
}

fn temp_db_path(test_name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    path.push(format!(
        "codegraph-tauri-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(&path, content).expect("write fixture file");
}

/// Index `relative_files` under `root` into a fresh store and run the whole
/// resolver pipeline (detect → framework extract → resolve). Mirrors
/// `tests/godot_post_extract.rs:54`'s `run_pipeline`; `run_post_extract` is
/// omitted because the Tauri resolver has no finalization pass.
struct Indexed {
    store: Store,
    edges: Vec<Edge>,
}

fn run_pipeline(test_name: &str, root: &Path, relative_files: &[&str]) -> Indexed {
    let mut store = index_without_resolve(test_name, root, relative_files);
    let root_str = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(root_str.clone());
    {
        let context = codegraph_resolve::StoreResolutionContext::new(&store, &root_str);
        resolver.initialize(&context);
    }
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve and persist");

    let edges = all_resolved_edges(&store);
    Indexed { store, edges }
}

/// Base extraction plus `extract_and_persist_frameworks`, with NO resolution pass.
///
/// The split exists because resolution DELETES the rows it resolves, so a test that
/// needs to see the emitted `tauri:invoke:*` references beside the extractor's own
/// `invoke | calls` rows has to look before that happens.
fn index_without_resolve(test_name: &str, root: &Path, relative_files: &[&str]) -> Store {
    let mut store = Store::open(&temp_db_path(test_name)).expect("open store");

    for &relative in relative_files {
        let language = detect_language(relative);
        let result = extract_file(root, relative).expect("extract file");
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
                generated: false,
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

    let root_str = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(root_str.clone());
    {
        let context = codegraph_resolve::StoreResolutionContext::new(&store, &root_str);
        resolver.initialize(&context);
    }
    let relative: Vec<String> = relative_files.iter().map(|f| (*f).to_string()).collect();
    resolver
        .extract_and_persist_frameworks(&mut store, &relative)
        .expect("framework extract");
    store
}

fn all_resolved_edges(store: &Store) -> Vec<Edge> {
    let mut resolved = Vec::new();
    for kind in NodeKind::ALL {
        for node in store.nodes_by_kind(kind).expect("nodes by kind") {
            for edge in store
                .edges_by_source_kind(&node.id, None)
                .expect("edges by source")
            {
                if edge.kind != codegraph_core::types::EdgeKind::Contains {
                    resolved.push(edge);
                }
            }
        }
    }
    resolved
}

impl Indexed {
    fn file_path_of(&self, node_id: &str) -> String {
        self.store
            .node_by_id(node_id)
            .expect("node lookup")
            .map(|n| n.file_path)
            .unwrap_or_default()
    }

    fn name_of(&self, node_id: &str) -> String {
        self.store
            .node_by_id(node_id)
            .expect("node lookup")
            .map(|n| n.name)
            .unwrap_or_default()
    }

    /// `calls` edges from a JS-family file into a `.rs` file — the IPC bridge, and
    /// the count every acceptance criterion in the plan asserts.
    fn ipc_edges(&self) -> Vec<&Edge> {
        self.edges
            .iter()
            .filter(|e| e.kind == codegraph_core::types::EdgeKind::Calls)
            .filter(|e| {
                let source = self.file_path_of(&e.source);
                let target = self.file_path_of(&e.target);
                target.ends_with(".rs")
                    && (source.ends_with(".ts")
                        || source.ends_with(".tsx")
                        || source.ends_with(".js")
                        || source.ends_with(".jsx"))
            })
            .collect()
    }

    /// IPC edges landing on a target NAMED `name` — asserted per name so a
    /// failure says which mask state broke rather than reporting a wrong total.
    fn ipc_edges_to(&self, name: &str) -> usize {
        self.ipc_edges()
            .into_iter()
            .filter(|e| self.name_of(&e.target) == name)
            .count()
    }

    fn nodes_named(&self, name: &str) -> Vec<Node> {
        self.store.nodes_by_name(name).expect("nodes by name")
    }
}

/// The §2 reproduction fixture, byte-for-byte: two `#[tauri::command]` fns
/// registered through `tauri::generate_handler!`, and two literal `invoke` call
/// sites in `src/app.ts`.
fn build_repro_fixture(root: &Path) {
    write(
        root,
        "src-tauri/src/main.rs",
        r#"#[tauri::command]
fn get_mcp_port() -> u16 {
    8111
}

#[tauri::command]
fn save_config(body: String) -> bool {
    !body.is_empty()
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_mcp_port, save_config])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
"#,
    );
    write(
        root,
        "src/app.ts",
        r#"import { invoke } from '@tauri-apps/api/core';

export async function loadPort(): Promise<number> {
  return await invoke('get_mcp_port');
}

export async function persist(body: string): Promise<boolean> {
  return await invoke('save_config', { body });
}
"#,
    );
    write(
        root,
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"ac-tauri\", \"identifier\": \"dev.ac.tauri\", \"build\": {}, \"app\": {} }\n",
    );
    write(
        root,
        "package.json",
        "{ \"name\": \"ac-tauri\", \"dependencies\": { \"@tauri-apps/api\": \"^2.0.0\" } }\n",
    );
}

/// `/tmp/m-matrix`'s measured bytes — 15 shape lines in `.ts` (12 that must emit
/// nothing, 3 that must emit) plus 4 in `.tsx`, every one naming the SAME command
/// so any fabricated reference becomes a visible edge.
fn build_matrix_fixture(root: &Path) {
    write(
        root,
        "src/app.ts",
        r#"import { invoke } from '@tauri-apps/api/core';
// invoke('save_config')
/* invoke('save_config') */
const doc = "invoke('save_config')";
const tpl = `invoke('save_config')`;
const reEsc = /invoke\('save_config'\)/;
const reRaw = /invoke('save_config')/;
const a = 6, b = 3;
const q = a / b, r = a /2/ b;
export function viaMember() { return client.invoke('save_config'); }
export function dyn(c: string) { return invoke(c); }
export function interp(id: string) { return invoke(`cmd_${id}`); }
export async function multi() {
  return await invoke(
    'save_config'
  );
}
export async function cmt() { return await invoke /* x */ ('save_config'); }
export async function real() { return await invoke('save_config'); }
"#,
    );
    write(
        root,
        "src/comp.tsx",
        r#"import { invoke } from '@tauri-apps/api/core';
export function A() { return <div title="invoke('save_config')">y</div>; }
export function B() { return <p>invoke('save_config')</p>; }
export async function C() { return await invoke('save_config'); }
"#,
    );
    write(
        root,
        "src-tauri/src/main.rs",
        "#[tauri::command]\nfn save_config(body: String) -> bool { !body.is_empty() }\n",
    );
    write(
        root,
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"neg\", \"identifier\": \"dev.neg\", \"build\": {}, \"app\": {} }\n",
    );
}

/// `/tmp/m-rust`'s measured bytes — SIX raw `tauri::command` hits of which exactly
/// one is a genuine attribute, and a `.ts` invoking all six names. The five fake
/// names each have a real UNATTRIBUTED function, so an unmasked roster binds each
/// to the wrong node: measured, six edges where the correct answer is one.
///
/// `frameworks/tauri.rs` holds the same bytes for the roster key-set test, and both
/// sides assert the raw hit count so the two copies cannot drift apart unnoticed.
fn build_adversarial_fixture(root: &Path) {
    write(
        root,
        "src-tauri/src/main.rs",
        r####"/* outer /* inner */ #[tauri::command]
fn nested_fake() -> u16 { 0 } */
const D1: &str = r##"
#[tauri::command]
fn hash_fake() -> u16 { 1 }
"##;
const D2: &[u8] = br##"
#[tauri::command]
fn byteraw_fake() -> u16 { 2 }
"##;
const D3: &[u8] = b"#[tauri::command] fn bytestr_fake() -> u16 { 3 }";
fn life<'a>(s: &'a str) -> &'a str { s }
const Q: char = '\'';
#[tauri::commandant]
fn attr_boundary_fake() -> u16 { 4 }
fn nested_fake() -> u16 { 40 }
fn hash_fake() -> u16 { 41 }
fn byteraw_fake() -> u16 { 42 }
fn bytestr_fake() -> u16 { 43 }
#[tauri::command]
fn real_cmd() -> u16 { 8111 }
"####,
    );
    write(
        root,
        "src/app.ts",
        r#"import { invoke } from '@tauri-apps/api/core';
export const a = () => invoke('nested_fake');
export const b = () => invoke('hash_fake');
export const c = () => invoke('byteraw_fake');
export const d = () => invoke('bytestr_fake');
export const e = () => invoke('attr_boundary_fake');
export const f = () => invoke('real_cmd');
"#,
    );
    write(
        root,
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"adv\", \"identifier\": \"dev.adv\", \"build\": {}, \"app\": {} }\n",
    );
}

const FAKE_NAMES: &[&str] = &[
    "nested_fake",
    "hash_fake",
    "byteraw_fake",
    "bytestr_fake",
    "attr_boundary_fake",
];

/// T1 — the reproduction, as a test.
///
/// Measured on the real binary at the baseline (plan §2): `codegraph callers
/// get_mcp_port` reports "No callers found", the fixture holds ZERO `calls` edges
/// in either direction, and the raw material sits in `unresolved_refs` as two
/// bare `invoke | calls` rows whose string-literal argument is not stored
/// anywhere. So this asserts `2` and reads `0` until the resolver lands.
#[test]
fn tauri_invoke_bridges_both_commands() {
    let root = unique_dir("repro");
    build_repro_fixture(&root);
    let g = run_pipeline("repro", &root, &["src-tauri/src/main.rs", "src/app.ts"]);

    let edges = g.ipc_edges();
    assert_eq!(
        edges.len(),
        2,
        "expected one .ts -> .rs `calls` edge per literal invoke site, got {:?}",
        edges
            .iter()
            .map(|e| format!("{} -> {}", g.file_path_of(&e.source), g.name_of(&e.target)))
            .collect::<Vec<_>>()
    );
    assert_eq!(g.ipc_edges_to("get_mcp_port"), 1);
    assert_eq!(g.ipc_edges_to("save_config"), 1);
}

/// T9 — the edge's metadata, not merely its existence.
///
/// 0.9 is the switch that says "this resolver already applied its own uniqueness
/// rule": `resolve_one_pure_inner` returns immediately from Strategy 1 at `>= 0.9`,
/// so nothing downstream may add to or override the result. Upstream #878 uses 0.7,
/// which hands the final say to the name matcher — the component whose
/// cross-language behaviour produced the Tier 2 regression.
#[test]
fn tauri_edge_is_framework_resolved_at_confidence_0_9() {
    let root = unique_dir("meta");
    build_repro_fixture(&root);
    let g = run_pipeline("meta", &root, &["src-tauri/src/main.rs", "src/app.ts"]);

    let edges = g.ipc_edges();
    assert_eq!(edges.len(), 2);
    for edge in edges {
        let metadata = edge.metadata.clone().expect("the edge carries metadata");
        assert_eq!(metadata["resolvedBy"].as_str(), Some("framework"));
        assert_eq!(metadata["confidence"].as_f64(), Some(0.9));
        assert_eq!(edge.kind, EdgeKind::Calls);
        assert_eq!(
            g.file_path_of(&edge.source),
            "src/app.ts",
            "the source is the FILE node (D6)"
        );
    }
}

/// T8 / AC-5b — the mask AGREES with the extractor, positionally, in both
/// directions.
///
/// The criterion that does not depend on having enumerated the right shapes in
/// advance: it compares this resolver's lexer against the base extractor's on
/// whatever the fixture happens to contain, so an unenumerated fabricating shape
/// fails without anyone having thought of it first.
///
/// Asserted BEFORE resolution, deliberately: `delete_resolved_rows` removes every
/// ref that resolved (keyed on `unresolved_refs.id`), so after a full pipeline the
/// `tauri:invoke:*` rows for resolvable names are gone and a post-resolution
/// comparison would read an empty set as agreement.
#[test]
fn tauri_extracted_refs_agree_with_the_extractor_positions() {
    let root = unique_dir("agree");
    build_matrix_fixture(&root);
    let files = ["src-tauri/src/main.rs", "src/app.ts", "src/comp.tsx"];
    let store = index_without_resolve("agree", &root, &files);
    let refs = store.all_unresolved_refs().expect("all unresolved refs");

    let extractor_sites: BTreeSet<(String, i64, i64)> = refs
        .iter()
        .filter(|r| r.reference_name == "invoke" && r.reference_kind == EdgeKind::Calls)
        .map(|r| (r.file_path.clone(), r.line, r.col))
        .collect();
    let ours: BTreeSet<(String, i64, i64)> = refs
        .iter()
        .filter(|r| r.reference_name.starts_with("tauri:invoke:"))
        .map(|r| (r.file_path.clone(), r.line, r.col))
        .collect();

    // The extractor's own call-site set, measured on these bytes with the baseline
    // binary. Pinned so a change in ITS behaviour is reported here rather than
    // silently redefining what agreement means.
    let site = |f: &str, l: i64, c: i64| (f.to_string(), l, c);
    assert_eq!(
        extractor_sites,
        BTreeSet::from([
            site("src/app.ts", 11, 40),
            site("src/app.ts", 12, 44),
            site("src/app.ts", 14, 15),
            site("src/app.ts", 18, 43),
            site("src/app.ts", 19, 44),
            site("src/comp.tsx", 4, 41),
        ])
    );

    // S1 (no fabrication) and S2 (no over-masking) at once: our emitted set must be
    // the extractor's set MINUS exactly the two non-literal-argument sites, which
    // gate 4 declines. This fixture carries no object-literal shape, so the one
    // enumerated D6 exception does not apply and equality is exact.
    let non_literal = BTreeSet::from([site("src/app.ts", 11, 40), site("src/app.ts", 12, 44)]);
    assert!(
        non_literal.is_subset(&extractor_sites),
        "the excluded positions must be REAL extractor rows, not typos"
    );
    let expected: BTreeSet<(String, i64, i64)> =
        extractor_sites.difference(&non_literal).cloned().collect();
    assert_eq!(ours, expected);
}

/// AC-5 — the call-side shape matrix as an EDGE count.
///
/// Four call sites, one command, four distinct `(line, col)` — and four rows,
/// because `idx_edges_identity` includes the position, so three sites in one file
/// all sourced from `file:src/app.ts` and all targeting the same command still
/// produce three distinct edges. Measured on these bytes: a raw text scan yields
/// 7, a mask without the regex state 5, mask-plus-receiver-guard-without-regex 4
/// (with a wrong position set), and gates that do not skip masked bytes 2.
#[test]
fn tauri_shape_matrix_yields_exactly_four_edges() {
    let root = unique_dir("matrix");
    build_matrix_fixture(&root);
    let g = run_pipeline(
        "matrix",
        &root,
        &["src-tauri/src/main.rs", "src/app.ts", "src/comp.tsx"],
    );

    let mut positions: Vec<(String, i64, i64)> = g
        .ipc_edges()
        .into_iter()
        .map(|e| {
            (
                g.file_path_of(&e.source),
                e.line.unwrap_or(-1),
                e.col.unwrap_or(-1),
            )
        })
        .collect();
    positions.sort();
    assert_eq!(
        positions,
        vec![
            ("src/app.ts".to_string(), 14, 15),
            ("src/app.ts".to_string(), 18, 43),
            ("src/app.ts".to_string(), 19, 44),
            ("src/comp.tsx".to_string(), 4, 41),
        ]
    );
    assert_eq!(g.ipc_edges_to("save_config"), 4);
}

/// T4b(j) / AC-6b — a masked or boundary-colliding attribute must not promote an
/// unattributed function.
///
/// The three parts are all required: the per-name zeros are the fabrication guard,
/// `real_cmd == 1` stops them being satisfied by a mask that fails closed and
/// resolves nothing, and the total pins both at once. The node-count provenance is
/// what proves the fabrication is REACHABLE rather than theoretical — each fake
/// name has exactly one node, the real unattributed function, so a naive roster's
/// same-file/same-name join has one candidate and it is the wrong one.
#[test]
fn tauri_masked_attributes_promote_no_unattributed_function() {
    let root = unique_dir("adv");
    build_adversarial_fixture(&root);
    let g = run_pipeline("adv", &root, &["src-tauri/src/main.rs", "src/app.ts"]);

    for fake in FAKE_NAMES {
        assert_eq!(
            g.nodes_named(fake).len(),
            1,
            "{fake} must have exactly one node — the real UNATTRIBUTED fn"
        );
        assert_eq!(
            g.ipc_edges_to(fake),
            0,
            "{fake} is not a command; an edge to it is fabricated"
        );
    }
    assert_eq!(g.ipc_edges_to("real_cmd"), 1);
    assert_eq!(
        g.ipc_edges().len(),
        1,
        "six call sites, ONE command, one edge"
    );
}

/// T10 — the accepted false positive, asserted deliberately.
///
/// `generate_handler!` is not parsed (D2), so an attributed-but-unregistered
/// command DOES get an edge. Wrong, but wrong in the direction of the developer's
/// intent and visible. If a later change starts parsing the macro, this is the
/// assertion that notices.
#[test]
fn tauri_unregistered_command_still_bridges() {
    let root = unique_dir("unregistered");
    write(
        &root,
        "src-tauri/src/main.rs",
        r#"#[tauri::command]
fn registered_cmd() -> u16 { 1 }

#[tauri::command]
fn unregistered_cmd() -> u16 { 2 }

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![registered_cmd])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
"#,
    );
    write(
        &root,
        "src/app.ts",
        "import { invoke } from '@tauri-apps/api/core';\n\
         export const a = () => invoke('registered_cmd');\n\
         export const b = () => invoke('unregistered_cmd');\n",
    );
    write(
        &root,
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"u\", \"identifier\": \"dev.u\", \"build\": {}, \"app\": {} }\n",
    );
    let g = run_pipeline(
        "unregistered",
        &root,
        &["src-tauri/src/main.rs", "src/app.ts"],
    );

    assert_eq!(g.ipc_edges_to("registered_cmd"), 1);
    assert_eq!(
        g.ipc_edges_to("unregistered_cmd"),
        1,
        "attributed-but-unregistered is a DELIBERATE false positive (D2)"
    );
}

/// T5 / AC-4 — ambiguity produces no edge, end to end.
///
/// A Tauri app cannot really register two commands under one wire name, but the
/// roster is built by scanning attributes rather than the registration list, so
/// dead code, a `#[cfg]`-excluded duplicate or a vendored copy can reach this. The
/// honest answer is "cannot tell which", and no edge is the answer.
#[test]
fn tauri_two_same_named_commands_produce_no_edge() {
    let root = unique_dir("ambiguous");
    let command = "#[tauri::command]\npub fn save() -> u16 { 0 }\n";
    write(&root, "src-tauri/src/a.rs", command);
    write(&root, "src-tauri/src/b.rs", command);
    write(
        &root,
        "src/app.ts",
        "import { invoke } from '@tauri-apps/api/core';\n\
         export const a = () => invoke('save');\n",
    );
    write(
        &root,
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"amb\", \"identifier\": \"dev.amb\", \"build\": {}, \"app\": {} }\n",
    );
    let g = run_pipeline(
        "ambiguous",
        &root,
        &["src-tauri/src/a.rs", "src-tauri/src/b.rs", "src/app.ts"],
    );

    assert_eq!(g.nodes_named("save").len(), 2, "both commands are indexed");
    assert_eq!(g.ipc_edges().len(), 0, "a poisoned key builds NO edge");
}

/// D4 — the `tauri-specta` camelCase spelling reaches the same command, and the
/// single-word case is not self-poisoned.
#[test]
fn tauri_camel_and_single_word_spellings_bridge() {
    let root = unique_dir("camel");
    write(
        &root,
        "src-tauri/src/main.rs",
        "#[tauri::command]\nfn get_mcp_port() -> u16 { 8111 }\n\
         #[tauri::command]\nfn refresh() -> u16 { 1 }\n",
    );
    write(
        &root,
        "src/app.ts",
        "import { invoke } from '@tauri-apps/api/core';\n\
         export const a = () => invoke('getMcpPort');\n\
         export const b = () => invoke('refresh');\n",
    );
    write(
        &root,
        "src-tauri/tauri.conf.json",
        "{ \"productName\": \"c\", \"identifier\": \"dev.c\", \"build\": {}, \"app\": {} }\n",
    );
    let g = run_pipeline("camel", &root, &["src-tauri/src/main.rs", "src/app.ts"]);

    assert_eq!(
        g.ipc_edges_to("get_mcp_port"),
        1,
        "camel key -> snake command"
    );
    assert_eq!(g.ipc_edges_to("refresh"), 1, "single-word is not poisoned");
}
