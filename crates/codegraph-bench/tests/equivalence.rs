use std::path::{Path, PathBuf};

use codegraph_bench::oracle::{
    Tier, assert_equivalent, canonicalize_db, diff_canonical, load_golden, write_golden,
};
use serde_json::json;

#[test]
fn generated_golden_matches_committed_mini_fixture() {
    let tempdir = TestDir::new("generated-golden");
    write_golden(&mini_db(), tempdir.path()).unwrap();

    let expected = load_golden(&mini_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn upstream_db_is_self_equivalent_to_mini_golden() {
    assert_equivalent(&mini_db(), &mini_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_godot_fixture() {
    // Guards Godot extraction (F1 autoload-call→func + F2 signal-handler edges)
    // against byte-drift: regenerating the canonical golden from the committed
    // godot db must reproduce the committed JSON exactly.
    let tempdir = TestDir::new("generated-golden-godot");
    write_golden(&godot_db(), tempdir.path()).unwrap();

    let expected = load_golden(&godot_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn upstream_db_is_self_equivalent_to_godot_golden() {
    assert_equivalent(&godot_db(), &godot_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_ruby_fixture() {
    // Guards Ruby #1110 receiver.method extraction (instance-call → Calls,
    // class-method call → Calls, `Const.new` → Instantiates, bare include →
    // Implements) against byte-drift: regenerating the canonical golden from the
    // committed ruby db must reproduce the committed JSON exactly.
    let tempdir = TestDir::new("generated-golden-ruby");
    write_golden(&ruby_db(), tempdir.path()).unwrap();

    let expected = load_golden(&ruby_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn upstream_db_is_self_equivalent_to_ruby_golden() {
    assert_equivalent(&ruby_db(), &ruby_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_cpp_fixture() {
    // Guards C++ #1043 base_class_clause inheritance (general Extends extraction
    // + templated-base stripping) against byte-drift: regenerating the canonical
    // golden from the committed cpp db must reproduce the committed JSON exactly.
    let tempdir = TestDir::new("generated-golden-cpp");
    write_golden(&cpp_db(), tempdir.path()).unwrap();

    let expected = load_golden(&cpp_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn cpp_db_is_self_equivalent_to_cpp_golden() {
    assert_equivalent(&cpp_db(), &cpp_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_metal_fixture() {
    // Guards Metal (#1121): `.metal`→cpp mapping + the `[[attribute]]` blank that
    // prevents the spurious `VertexIn extends float4` inheritance edge.
    let tempdir = TestDir::new("generated-golden-metal");
    write_golden(&metal_db(), tempdir.path()).unwrap();

    let expected = load_golden(&metal_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn metal_db_is_self_equivalent_to_metal_golden() {
    assert_equivalent(&metal_db(), &metal_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_cuda_fixture() {
    // Guards CUDA (#1172 CUDA-lang parts): `.cu`→cpp mapping, the `<<<…>>>`
    // launch-config blank that preserves the host→kernel Calls edge (plain +
    // templated), and macro-defined-kernel name recovery (`my_kernel`).
    let tempdir = TestDir::new("generated-golden-cuda");
    write_golden(&cuda_db(), tempdir.path()).unwrap();

    let expected = load_golden(&cuda_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn cuda_db_is_self_equivalent_to_cuda_golden() {
    assert_equivalent(&cuda_db(), &cuda_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_arkts_fixture() {
    // Guards ArkTS extraction (upstream #1186, extraction slice only): the
    // `.ets`->ArkTs mapping, `@Component struct`->NodeKind::Struct via the
    // dedicated tree-sitter-arkts grammar, function/class/import/call extraction.
    let tempdir = TestDir::new("generated-golden-arkts");
    write_golden(&arkts_db(), tempdir.path()).unwrap();

    let expected = load_golden(&arkts_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn arkts_db_is_self_equivalent_to_arkts_golden() {
    assert_equivalent(&arkts_db(), &arkts_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_solidity_fixture() {
    // Guards Solidity extraction (upstream #1170): the `.sol`->Solidity mapping,
    // contract/library->Class + interface->Interface + struct->Struct +
    // enum->Enum, synthetic constructor/fallback/receive method names,
    // state-var/struct-member/event/error->Field, `is`-inheritance->Extends
    // (resolver promotes to Implements), and emit/modifier-guard call edges.
    let tempdir = TestDir::new("generated-golden-solidity");
    write_golden(&solidity_db(), tempdir.path()).unwrap();

    let expected = load_golden(&solidity_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn solidity_db_is_self_equivalent_to_solidity_golden() {
    assert_equivalent(&solidity_db(), &solidity_golden_dir()).unwrap();
}

#[test]
fn generated_golden_matches_committed_nix_fixture() {
    // Guards Nix extraction (upstream #1190, extraction slice only): the
    // `.nix`->Nix mapping, `binding`->Function|Variable, curried lambda->Function
    // with a formatted signature, `inherit`->Variable names, `import`/
    // `callPackage`/`imports`-list literal paths->Import node + Imports ref, and
    // `apply_expression`->Calls ref with curried-chain dedup. The module-system
    // synthesizer / lexical-scope gates / callback synthesizer / import-resolver
    // wiring are DEFERRED, so path refs stay unresolved.
    let tempdir = TestDir::new("generated-golden-nix");
    write_golden(&nix_db(), tempdir.path()).unwrap();

    let expected = load_golden(&nix_golden_dir()).unwrap();
    let actual = load_golden(tempdir.path()).unwrap();

    diff_canonical(&expected, &actual, None).unwrap();
}

#[test]
fn nix_db_is_self_equivalent_to_nix_golden() {
    assert_equivalent(&nix_db(), &nix_golden_dir()).unwrap();
}

#[test]
fn tier1_node_drift_is_reported() {
    let expected = load_golden(&mini_golden_dir()).unwrap();
    let mut actual = expected.clone();
    actual.nodes[0].insert("name".to_string(), json!("DRIFTED_NAME"));

    let error = diff_canonical(&expected, &actual, None).unwrap_err();
    println!("injected Tier-1 drift failure:\n{error}");

    assert!(
        error
            .entries()
            .iter()
            .any(|entry| entry.tier == Tier::Tier1 && entry.surface == "nodes")
    );
}

#[test]
fn tier2_edges_are_order_independent_but_counted() {
    let expected = canonicalize_db(&mini_db()).unwrap();
    let mut reordered = expected.clone();
    reordered.edges.reverse();
    diff_canonical(&expected, &reordered, None).unwrap();

    let mut missing = expected.clone();
    let removed = missing.edges.pop().expect("mini fixture has edges");
    let error = diff_canonical(&expected, &missing, None).unwrap_err();
    println!("removed edge for Tier-2 missing-edge assertion: {removed:?}");
    println!("missing-edge failure:\n{error}");

    assert!(
        error
            .entries()
            .iter()
            .any(|entry| entry.tier == Tier::Tier2 && entry.surface == "edges")
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-bench is under crates/")
        .to_path_buf()
}

fn mini_db() -> PathBuf {
    workspace_root().join("reference/golden/mini/colby.db")
}

fn mini_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/mini")
}

fn godot_db() -> PathBuf {
    workspace_root().join("reference/golden/godot/colby.db")
}

fn godot_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/godot")
}

fn ruby_db() -> PathBuf {
    workspace_root().join("reference/golden/ruby/colby.db")
}

fn ruby_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/ruby")
}

fn cpp_db() -> PathBuf {
    workspace_root().join("reference/golden/cpp/colby.db")
}

fn cpp_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/cpp")
}

fn metal_db() -> PathBuf {
    workspace_root().join("reference/golden/metal/colby.db")
}

fn metal_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/metal")
}

fn cuda_db() -> PathBuf {
    workspace_root().join("reference/golden/cuda/colby.db")
}

fn cuda_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/cuda")
}

fn arkts_db() -> PathBuf {
    workspace_root().join("reference/golden/arkts/colby.db")
}

fn arkts_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/arkts")
}

fn solidity_db() -> PathBuf {
    workspace_root().join("reference/golden/solidity/colby.db")
}

fn solidity_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/solidity")
}

fn nix_db() -> PathBuf {
    workspace_root().join("reference/golden/nix/colby.db")
}

fn nix_golden_dir() -> PathBuf {
    workspace_root().join("reference/golden/nix")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-bench-equivalence-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
