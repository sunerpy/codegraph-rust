use std::path::{Path, PathBuf};

use codegraph_bench::oracle::{
    KnownDiffs, Tier, assert_equivalent, assert_equivalent_with_known_diffs, canonicalize_db,
    diff_canonical, load_golden, write_golden,
};
use serde_json::json;

#[test]
fn committed_known_diffs_doc_is_parsed_and_allowlists_nothing() {
    // Every `assert_equivalent` in this file now adjudicates through the
    // committed KNOWN_DIFFS.md, so it must parse AND stay empty: one active
    // Tier-3 rule would silently widen every golden comparison below.
    let path = KnownDiffs::repo_doc_path();
    assert!(path.is_file(), "{} must exist", path.display());
    let known = KnownDiffs::load(&path).unwrap_or_else(|error| {
        panic!("{} must parse: {error:#}", path.display());
    });
    assert_eq!(known.rule_count(), 0, "{} must stay empty", path.display());
}

#[test]
fn an_unparseable_known_diffs_file_fails_the_equivalence_assertion() {
    let tempdir = TestDir::new("known-diffs-invalid");
    let path = tempdir.path().join("KNOWN_DIFFS.md");
    std::fs::write(
        &path,
        "RULE tier=1 surface=nodes key=* justification=sneaky\n",
    )
    .unwrap();

    let error = assert_equivalent_with_known_diffs(&mini_db(), &mini_golden_dir(), &path)
        .expect_err("an invalid allowlist must fail, not be ignored");
    let message = format!("{error:#}");
    println!("invalid KNOWN_DIFFS.md failure:\n{message}");
    assert!(message.contains("may not be allowlisted"), "got: {message}");
}

/// One pair of byte-drift tests per committed golden corpus. Each corpus's
/// docs/equivalence.md section says what it guards; the regeneration recipe is
/// shared (`scripts/regen-goldens.sh`). The first test regenerates the canonical
/// artifacts from the committed `colby.db` and compares them with the committed
/// JSON; the second runs the full oracle over the same pair.
macro_rules! golden_corpora {
    ($($corpus:literal => $generated:ident, $equivalent:ident;)+) => {
        /// Every corpus with a committed `colby.db`, in declaration order.
        const GOLDEN_CORPORA: &[&str] = &[$($corpus),+];

        $(
            #[test]
            fn $generated() {
                let tempdir = TestDir::new(concat!("generated-golden-", $corpus));
                write_golden(&corpus_db($corpus), tempdir.path()).unwrap();

                let expected = load_golden(&corpus_golden_dir($corpus)).unwrap();
                let actual = load_golden(tempdir.path()).unwrap();

                diff_canonical(&expected, &actual, None).unwrap();
            }

            #[test]
            fn $equivalent() {
                assert_equivalent(&corpus_db($corpus), &corpus_golden_dir($corpus)).unwrap();
            }
        )+
    };
}

golden_corpora! {
    "mini" => generated_golden_matches_committed_mini_fixture, upstream_db_is_self_equivalent_to_mini_golden;
    "godot" => generated_golden_matches_committed_godot_fixture, upstream_db_is_self_equivalent_to_godot_golden;
    "ruby" => generated_golden_matches_committed_ruby_fixture, upstream_db_is_self_equivalent_to_ruby_golden;
    "cpp" => generated_golden_matches_committed_cpp_fixture, cpp_db_is_self_equivalent_to_cpp_golden;
    "rust" => generated_golden_matches_committed_rust_fixture, rust_db_is_self_equivalent_to_rust_golden;
    "lua" => generated_golden_matches_committed_lua_fixture, lua_db_is_self_equivalent_to_lua_golden;
    "go" => generated_golden_matches_committed_go_fixture, go_db_is_self_equivalent_to_go_golden;
    "python" => generated_golden_matches_committed_python_fixture, python_db_is_self_equivalent_to_python_golden;
    "kotlin" => generated_golden_matches_committed_kotlin_fixture, kotlin_db_is_self_equivalent_to_kotlin_golden;
    "scala" => generated_golden_matches_committed_scala_fixture, scala_db_is_self_equivalent_to_scala_golden;
    "dart" => generated_golden_matches_committed_dart_fixture, dart_db_is_self_equivalent_to_dart_golden;
    "typescript" => generated_golden_matches_committed_typescript_fixture, typescript_db_is_self_equivalent_to_typescript_golden;
    "metal" => generated_golden_matches_committed_metal_fixture, metal_db_is_self_equivalent_to_metal_golden;
    "cuda" => generated_golden_matches_committed_cuda_fixture, cuda_db_is_self_equivalent_to_cuda_golden;
    "arkts" => generated_golden_matches_committed_arkts_fixture, arkts_db_is_self_equivalent_to_arkts_golden;
    "solidity" => generated_golden_matches_committed_solidity_fixture, solidity_db_is_self_equivalent_to_solidity_golden;
    "nix" => generated_golden_matches_committed_nix_fixture, nix_db_is_self_equivalent_to_nix_golden;
    "terraform" => generated_golden_matches_committed_terraform_fixture, terraform_db_is_self_equivalent_to_terraform_golden;
    "erlang" => generated_golden_matches_committed_erlang_fixture, erlang_db_is_self_equivalent_to_erlang_golden;
    "cfml" => generated_golden_matches_committed_cfml_fixture, cfml_db_is_self_equivalent_to_cfml_golden;
    "csharp" => generated_golden_matches_committed_csharp_fixture, csharp_db_is_self_equivalent_to_csharp_golden;
    "java" => generated_golden_matches_committed_java_fixture, java_db_is_self_equivalent_to_java_golden;
    "swift" => generated_golden_matches_committed_swift_fixture, swift_db_is_self_equivalent_to_swift_golden;
    "objc" => generated_golden_matches_committed_objc_fixture, objc_db_is_self_equivalent_to_objc_golden;
    "php" => generated_golden_matches_committed_php_fixture, php_db_is_self_equivalent_to_php_golden;
    "vue" => generated_golden_matches_committed_vue_fixture, vue_db_is_self_equivalent_to_vue_golden;
    "svelte" => generated_golden_matches_committed_svelte_fixture, svelte_db_is_self_equivalent_to_svelte_golden;
    "python_bases" => generated_golden_matches_committed_python_bases_fixture, python_bases_db_is_self_equivalent_to_python_bases_golden;
    "commonjs" => generated_golden_matches_committed_commonjs_fixture, commonjs_db_is_self_equivalent_to_commonjs_golden;
    "routers" => generated_golden_matches_committed_routers_fixture, routers_db_is_self_equivalent_to_routers_golden;
    "servers" => generated_golden_matches_committed_servers_fixture, servers_db_is_self_equivalent_to_servers_golden;
    "mobile" => generated_golden_matches_committed_mobile_fixture, mobile_db_is_self_equivalent_to_mobile_golden;
    "synthesis" => generated_golden_matches_committed_synthesis_fixture, synthesis_db_is_self_equivalent_to_synthesis_golden;
}

#[test]
fn every_committed_golden_corpus_has_its_test_pair() {
    let mut found: Vec<String> = std::fs::read_dir(workspace_root().join("reference/golden"))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            entry
                .path()
                .join("colby.db")
                .is_file()
                .then(|| entry.file_name().into_string().unwrap())
        })
        .collect();
    found.sort();
    let mut listed: Vec<String> = GOLDEN_CORPORA.iter().map(|c| (*c).to_string()).collect();
    listed.sort();
    assert_eq!(
        found, listed,
        "every reference/golden/<corpus>/colby.db must be listed in golden_corpora!"
    );
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

fn corpus_db(corpus: &str) -> PathBuf {
    workspace_root()
        .join("reference/golden")
        .join(corpus)
        .join("colby.db")
}

fn corpus_golden_dir(corpus: &str) -> PathBuf {
    workspace_root().join("reference/golden").join(corpus)
}

fn mini_db() -> PathBuf {
    corpus_db("mini")
}

fn mini_golden_dir() -> PathBuf {
    corpus_golden_dir("mini")
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
