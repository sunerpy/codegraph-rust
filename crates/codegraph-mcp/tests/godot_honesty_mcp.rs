//! T8 (L6) Godot honesty-signal integration tests for the MCP `codegraph_callers`
//! and `codegraph_impact` tools.
//!
//! Drives the FULL indexing + framework-resolution pipeline against a real Godot
//! fixture, then a JSON-RPC roundtrip through the server, asserting:
//! - a `func _on_hit` referenced ONLY by a `.tscn` `[connection]` (no GDScript
//!   call) is reported as dynamically reachable, NOT zero-callers/dead;
//! - an autoload-only-referenced `func apply` is annotated "may be reached
//!   dynamically";
//! - a `get_node(var)` computed site surfaces as a dynamic/unresolved entry;
//! - a NON-Godot project's callers output is BYTE-UNCHANGED (no annotation).

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::FileRecord;
use codegraph_extract::{detect_language, extract_file};
use codegraph_mcp::McpServer;
use codegraph_resolve::ReferenceResolver;
use codegraph_store::Store;
use serde_json::{json, Value};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TestProject {
    path: PathBuf,
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TestProject {
    fn path(&self) -> &Path {
        &self.path
    }
}

/// Index `files` under a fresh temp project and run the entire pipeline
/// (base extraction → framework extract → resolve → post_extract), so Godot
/// edges and `godot:dynamic:` sentinel refs are actually present.
fn pipeline_project(files: &[(&str, &str)]) -> TestProject {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let base =
        std::env::temp_dir().join(format!("cg-mcp-godot-{}-{nanos}-{seq}", std::process::id()));
    for (rel, src) in files {
        let dst = base.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, src).unwrap();
    }

    let mut store = Store::open(&base.join(".codegraph").join("codegraph.db")).unwrap();
    for (rel, src) in files {
        let result = extract_file(&base, rel).unwrap();
        store
            .upsert_file(&FileRecord {
                path: (*rel).to_string(),
                content_hash: String::new(),
                language: detect_language(rel),
                size: src.len() as i64,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: Vec::new(),
            })
            .unwrap();
        store.upsert_nodes(&result.nodes).unwrap();
        store.insert_edges(&result.edges).unwrap();
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .unwrap();
    }

    let mut resolver = ReferenceResolver::new(base.to_string_lossy().to_string());
    {
        let context =
            codegraph_resolve::StoreResolutionContext::new(&store, base.to_string_lossy());
        resolver.initialize(&context);
    }
    let relative: Vec<String> = files.iter().map(|(rel, _)| (*rel).to_string()).collect();
    resolver
        .extract_and_persist_frameworks(&mut store, &relative)
        .unwrap();
    resolver.resolve_and_persist(&mut store).unwrap();
    resolver.run_post_extract(&mut store).unwrap();
    drop(store);

    TestProject { path: base }
}

fn callers_text(project: &Path, symbol: &str) -> String {
    tool_text(project, "codegraph_callers", json!({ "symbol": symbol }))
}

fn impact_text(project: &Path, symbol: &str) -> String {
    tool_text(project, "codegraph_impact", json!({ "symbol": symbol }))
}

fn tool_text(project: &Path, tool: &str, mut arguments: Value) -> String {
    arguments
        .as_object_mut()
        .unwrap()
        .insert("projectPath".to_string(), json!(project.to_str().unwrap()));
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });
    let input = format!("{}\n", serde_json::to_string(&request).unwrap());
    let mut output = Vec::new();
    let mut server = McpServer::new(Some(project.to_path_buf()));
    server
        .run(Cursor::new(input.into_bytes()), &mut output)
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    let line = text.lines().next().unwrap();
    let resp: Value = serde_json::from_str(line).unwrap();
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

const PROJECT_GODOT: &str =
    "config_version=5\n\n[autoload]\n\nBuffManager=\"*res://buff_manager.gd\"\n";
const BUFF_MANAGER_GD: &str = "extends Node\n\nfunc apply():\n\treturn 1\n";
const PLAYER_GD: &str =
    "extends Node\n\nfunc _on_hit():\n\treturn 2\n\nfunc dyn():\n\tget_node(target_path).foo()\n";
const MAIN_TSCN: &str = "[gd_scene load_steps=2 format=3]\n\n[ext_resource type=\"Script\" path=\"res://player.gd\" id=\"1\"]\n\n[node name=\"Player\" type=\"Node\"]\nscript = ExtResource(\"1\")\n\n[connection signal=\"hit\" from=\"Player\" to=\".\" method=\"_on_hit\"]\n";

fn godot_project() -> TestProject {
    pipeline_project(&[
        ("project.godot", PROJECT_GODOT),
        ("buff_manager.gd", BUFF_MANAGER_GD),
        ("player.gd", PLAYER_GD),
        ("Main.tscn", MAIN_TSCN),
    ])
}

#[test]
fn connection_handler_callers_reports_dynamically_reachable_not_dead() {
    // Given `func _on_hit` referenced only by a .tscn [connection method="_on_hit"],
    // When codegraph_callers is queried for `_on_hit`,
    // Then it is annotated dynamically reachable, NOT a bare "no callers".
    let project = godot_project();
    let text = callers_text(project.path(), "_on_hit");
    assert!(
        text.contains("may be reached dynamically"),
        "connection handler must be flagged dynamically reachable:\n{text}"
    );
    assert!(
        text.contains("signal/get_node/group"),
        "annotation must name the Godot scene-link source:\n{text}"
    );
}

#[test]
fn autoload_func_callers_reports_dynamically_reachable() {
    // Given `func apply` in the script bound to the BuffManager autoload,
    // When codegraph_callers is queried for `apply`,
    // Then it is annotated "may be reached dynamically (Godot autoload)".
    let project = godot_project();
    let text = callers_text(project.path(), "apply");
    assert!(
        text.contains("may be reached dynamically"),
        "autoload-bound function must be flagged dynamically reachable:\n{text}"
    );
    assert!(
        text.contains("autoload"),
        "annotation must name autoload as the reachability source:\n{text}"
    );
}

#[test]
fn computed_get_node_surfaces_as_dynamic_unresolved() {
    // Given `func dyn` containing `get_node(target_path)` (a godot:dynamic: site),
    // When codegraph_callers is queried for `dyn`,
    // Then the sentinel surfaces in the dynamic/unresolved category.
    let project = godot_project();
    let text = callers_text(project.path(), "dyn");
    assert!(
        text.contains("Dynamic / unresolved references (cannot be statically confirmed)"),
        "computed call-site must surface the dynamic/unresolved category:\n{text}"
    );
    assert!(
        text.contains("godot:dynamic:get_node"),
        "the get_node(var) sentinel must be listed:\n{text}"
    );
}

#[test]
fn impact_surfaces_godot_dynamic_reachability() {
    // Given the same Godot fixture,
    // When codegraph_impact is queried for `_on_hit` (no static dependents),
    // Then the dynamic-reachability annotation appears in impact output too.
    let project = godot_project();
    let text = impact_text(project.path(), "_on_hit");
    assert!(
        text.contains("may be reached dynamically"),
        "impact output must carry the Godot dynamic-reachability annotation:\n{text}"
    );
}

#[test]
fn non_godot_callers_output_is_unchanged() {
    // Given a NON-Godot TypeScript project (no project.godot → resolver inactive),
    // When codegraph_callers is queried for a function with no callers,
    // Then the output is the normal "No callers" text with NO Godot annotation.
    let project = pipeline_project(&[(
        "src/util.ts",
        "export function orphan(): void {\n  return;\n}\n",
    )]);
    let text = callers_text(project.path(), "orphan");
    assert!(
        text.contains("No callers found for \"orphan\""),
        "non-Godot orphan must still report the normal no-callers text:\n{text}"
    );
    assert!(
        !text.contains("may be reached dynamically"),
        "non-Godot output must NOT carry the Godot annotation:\n{text}"
    );
    assert!(
        !text.contains("Dynamic / unresolved references"),
        "non-Godot output must NOT carry the dynamic/unresolved category:\n{text}"
    );
}
