//! Upstream `1de7e8f` (#1319) — a multi-hump field-name query must reach the
//! symbol that DEFINES it, even when the stored name segments differently from
//! the query's humps.
//!
//! Drives the real `codegraph` binary against a temp project holding, for each
//! query shape, three files:
//!   * the DEFINER — a callable whose name embeds the query at a hump boundary
//!     (`profileInfo` → `getProfileInfoV2`);
//!   * a PROSE decoy — a constant whose signature/docstring merely MENTIONS the
//!     query words, which is what today's FTS actually returns;
//!   * a NAME decoy — a callable whose lowercase name CONTAINS the query's run
//!     but at no hump boundary (`xxprofileinfoxx`), which a naive `LIKE
//!     %needle%` would bind to.
//!
//! The definer must be returned and must outrank both decoys; the name decoy
//! must not be returned at all.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-multi-hump-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
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

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn run_in(cwd: &Path, args: &[&str]) -> Run {
    let output = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn write_project(root: &Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("profileController.js"),
        "function getProfileInfoV2(userId) {\n  return { userId };\n}\n\n\
         function updateUserProfileIdMapping(a, b) {\n  return [a, b];\n}\n\n\
         function loadOrderStateSnapshot() {\n  return 0;\n}\n\n\
         module.exports = {\n  getProfileInfoV2,\n  updateUserProfileIdMapping,\n  loadOrderStateSnapshot,\n};\n",
    )
    .unwrap();

    std::fs::write(
        src.join("prose_decoy.js"),
        "const NOTES = \"profileInfo userProfileId orderState profile info user order state\";\n\
         function auditLog(msg) {\n  return NOTES + msg;\n}\n\
         module.exports = { auditLog };\n",
    )
    .unwrap();

    std::fs::write(
        src.join("name_decoy.js"),
        "function xxprofileinfoxx() {\n  return 0;\n}\n\n\
         function xxuserprofileidxx() {\n  return 1;\n}\n\n\
         function xxorderstatexx() {\n  return 2;\n}\n\n\
         module.exports = { xxprofileinfoxx, xxuserprofileidxx, xxorderstatexx };\n",
    )
    .unwrap();
}

fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_project(&project);
    let run = run_in(dir.path(), &["init", project.to_str().unwrap()]);
    assert!(run.ok, "init failed: {} {}", run.stdout, run.stderr);
    project
}

/// `(name, filePath)` for every hit, in the order the CLI ranked them.
fn ranked(project: &Path, query: &str) -> Vec<(String, String)> {
    let p = project.to_str().unwrap();
    let run = run_in(project, &["query", query, "-p", p, "--json"]);
    assert!(
        run.ok,
        "query {query:?} must succeed: {} {}",
        run.stdout, run.stderr
    );
    let v: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("query {query:?} must emit JSON ({e}): {}", run.stdout));
    v.as_array()
        .expect("query --json is an array")
        .iter()
        .map(|r| {
            (
                r["node"]["name"].as_str().unwrap_or_default().to_string(),
                r["node"]["filePath"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// `(query, definer, name-decoy)` — camelCase, snake_case and PascalCase forms of
/// the same field names, so the fix cannot pass by handling only the one shape
/// it was written for.
const CASES: &[(&str, &str, &str)] = &[
    ("profileInfo", "getProfileInfoV2", "xxprofileinfoxx"),
    ("profile_info", "getProfileInfoV2", "xxprofileinfoxx"),
    ("ProfileInfo", "getProfileInfoV2", "xxprofileinfoxx"),
    (
        "userProfileId",
        "updateUserProfileIdMapping",
        "xxuserprofileidxx",
    ),
    (
        "user_profile_id",
        "updateUserProfileIdMapping",
        "xxuserprofileidxx",
    ),
    ("orderState", "loadOrderStateSnapshot", "xxorderstatexx"),
];

#[test]
fn multi_hump_field_name_query_reaches_its_definer_not_a_prose_or_name_decoy() {
    let dir = TestDir::new("definer");
    let project = indexed_project(&dir);

    let mut failures: Vec<String> = Vec::new();
    for (query, definer, name_decoy) in CASES {
        let hits = ranked(&project, query);
        let pos = |name: &str| hits.iter().position(|(n, _)| n == name);
        match pos(definer) {
            None => failures.push(format!(
                "query {query:?}: definer {definer} ABSENT; ranking was {hits:?}"
            )),
            Some(d) => {
                if let Some(other) = hits
                    .iter()
                    .position(|(n, _)| n != definer)
                    .filter(|other| *other < d)
                {
                    failures.push(format!(
                        "query {query:?}: {} outranked definer {definer}; ranking was {hits:?}",
                        hits[other].0
                    ));
                }
            }
        }
        if let Some(bad) = pos(name_decoy) {
            failures.push(format!(
                "query {query:?}: non-hump-boundary namesake {name_decoy} must not be returned (at {bad}); ranking was {hits:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "multi-hump field-name queries must reach their definers:\n{}",
        failures.join("\n")
    );
}

/// A query that IS an exact symbol name keeps binding to that symbol — the
/// infix fallback must never displace an exact definition.
#[test]
fn exact_symbol_query_is_unaffected_by_the_infix_fallback() {
    let dir = TestDir::new("exact");
    let project = indexed_project(&dir);

    for name in ["getProfileInfoV2", "updateUserProfileIdMapping", "auditLog"] {
        let hits = ranked(&project, name);
        assert_eq!(
            hits.first().map(|(n, _)| n.as_str()),
            Some(name),
            "exact symbol {name:?} must still rank first, got {hits:?}"
        );
    }
}

/// A bare single-word query must NOT trigger the multi-hump fallback: it has no
/// humps, so widening it would resurrect the natural-language false positives
/// the stop-word guard exists to prevent.
#[test]
fn single_word_query_does_not_pull_in_infix_namesakes() {
    let dir = TestDir::new("single");
    let project = indexed_project(&dir);

    for word in ["profile", "state"] {
        let hits = ranked(&project, word);
        assert!(
            !hits.iter().any(|(n, _)| n.starts_with("xx")),
            "single-word query {word:?} must not reach infix namesakes, got {hits:?}"
        );
    }
}

/// Ranking must be a pure function of the index: repeated identical queries
/// answer identically.
#[test]
fn multi_hump_ranking_is_deterministic_across_repeated_queries() {
    let dir = TestDir::new("determinism");
    let project = indexed_project(&dir);
    let first = ranked(&project, "userProfileId");
    for _ in 0..4 {
        assert_eq!(
            ranked(&project, "userProfileId"),
            first,
            "repeated identical queries must return an identical ranking"
        );
    }
}
