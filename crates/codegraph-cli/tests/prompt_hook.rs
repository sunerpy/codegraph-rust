//! End-to-end tests for the hidden `codegraph prompt-hook` subcommand.
//!
//! `prompt-hook` is a confidence-tiered gate (upstream #1126 multilingual gate
//! + #1136 graph-derived MEDIUM tier, telemetry EXCLUDED):
//! - HIGH — a structural keyword (any of ~29 covered languages) OR a code token
//!   verified in the index → full `codegraph_explore` injection.
//! - MEDIUM — prose words match indexed symbol-name segments → a short
//!   symbol-pointer hint (never runs explore).
//! - silent — nothing verified → zero-cost no-op.
//!
//! Mirrors the spawn-the-real-binary pattern of `sync_incremental.rs` /
//! `installer.rs` via `CARGO_BIN_EXE_codegraph`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn medium_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt_hook_medium")
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-prompt-hook-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cli(args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// Run `prompt-hook` feeding text over stdin (no positional/flag arg).
fn cli_stdin(args: &[&str], stdin_text: &str) -> (String, String, bool) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn codegraph binary");
    child
        .stdin
        .as_mut()
        .expect("child stdin")
        .write_all(stdin_text.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait codegraph");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn index_fixture(fixture: &Path, subdir: &str, label: &str) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.path().join(subdir);
    copy_tree(fixture, &project);
    let (out, err, ok) = cli(&["init", project.to_str().unwrap()]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    (dir, project)
}

fn index_mini(label: &str) -> (TestDir, PathBuf) {
    index_fixture(&mini_fixture(), "mini", label)
}

fn index_medium(label: &str) -> (TestDir, PathBuf) {
    index_fixture(&medium_fixture(), "medium", label)
}

// === HIGH tier: named-symbol code token (call form) =========================

#[test]
fn prompt_hook_high_tier_on_call_form() {
    let (_dir, project) = index_mini("emit");

    // `Counter()` is a code token (call form) — a bare `Counter` is NOT (no
    // inner camelCase / underscore / `(` / `.`), so the HIGH named-symbol path
    // must use the call form. The fixture defines `Counter` in src/math.ts.
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "Counter()",
    ]);
    assert!(ok, "prompt-hook failed: stdout={out} stderr={err}");
    assert!(
        out.contains("Counter"),
        "expected explore output referencing Counter, got:\n{out}"
    );
    assert!(
        out.contains("Exploration:") || out.contains("Source Code"),
        "expected codegraph_explore-style content, got:\n{out}"
    );
    assert!(
        out.contains("<codegraph_context"),
        "HIGH tier must wrap in <codegraph_context>, got:\n{out}"
    );
}

#[test]
fn prompt_hook_call_form_from_stdin() {
    let (_dir, project) = index_mini("stdin");

    let (out, err, ok) = cli_stdin(
        &["prompt-hook", "--path", project.to_str().unwrap()],
        "Counter()",
    );
    assert!(ok, "prompt-hook (stdin) failed: stdout={out} stderr={err}");
    assert!(
        out.contains("Counter"),
        "expected stdin call form to drive explore output, got:\n{out}"
    );
}

// === HIGH tier: structural keyword path =====================================

#[test]
fn gate_fires_on_calls_in_sentence() {
    let (_dir, project) = index_mini("calls-sentence");
    // "what calls Counter()" fires HIGH via the STRUCTURAL-KEYWORD path
    // (`calls`), independent of the tokenizer (#1138 branch-local boundary).
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "what calls Counter()",
    ]);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        out.contains("<codegraph_context"),
        "expected context, got:\n{out}"
    );
    assert!(
        out.contains("Counter"),
        "expected Counter in output, got:\n{out}"
    );
}

#[test]
fn gate_fires_on_inflected_and_phrase() {
    let (_dir, project) = index_mini("inflected");
    for q in ["what are the data flows here", "why does the counter reset"] {
        let (out, err, ok) = cli(&[
            "prompt-hook",
            "--path",
            project.to_str().unwrap(),
            "--query",
            q,
        ]);
        assert!(ok, "failed for {q:?}: stdout={out} stderr={err}");
        assert!(
            out.contains("<codegraph_context"),
            "inflected/phrase prompt {q:?} must emit context, got:\n{out}"
        );
    }
}

#[test]
fn gate_fires_on_devanagari() {
    let (_dir, project) = index_mini("devanagari");
    // Hindi/Devanagari structural question (combining-mark entry, Blocker 1).
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "यह कैसे काम करता है",
    ]);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        out.contains("<codegraph_context"),
        "Devanagari structural prompt must emit context, got:\n{out}"
    );
}

#[test]
fn gate_fires_on_non_english() {
    let (_dir, project) = index_mini("non-english");
    for q in ["comment marche le compteur", "这个计数器如何工作"] {
        let (out, err, ok) = cli(&[
            "prompt-hook",
            "--path",
            project.to_str().unwrap(),
            "--query",
            q,
        ]);
        assert!(ok, "failed for {q:?}: stdout={out} stderr={err}");
        assert!(
            out.contains("<codegraph_context"),
            "non-English structural prompt {q:?} must emit context, got:\n{out}"
        );
    }
}

// === silent tier ============================================================

#[test]
fn gate_silent_on_plain_prose() {
    let (_dir, project) = index_mini("silent-prose");
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "please fix this typo",
    ]);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        out.trim().is_empty(),
        "plain prose must be silent, got:\n{out}"
    );
}

#[test]
fn gate_no_false_fire_on_ordinary_words() {
    let (_dir, project) = index_mini("no-false-fire");
    for q in ["about Connecticut weather", "i have a callus"] {
        let (out, err, ok) = cli(&[
            "prompt-hook",
            "--path",
            project.to_str().unwrap(),
            "--query",
            q,
        ]);
        assert!(ok, "failed for {q:?}: stdout={out} stderr={err}");
        assert!(
            out.trim().is_empty(),
            "ordinary-word prompt {q:?} must not fire, got:\n{out}"
        );
    }
}

// === JSON payload parsing ====================================================

#[test]
fn prompt_hook_parses_userpromptsubmit_json() {
    let (_dir, project) = index_mini("json-payload");
    // Real Claude UserPromptSubmit payload — no --path, no arg. The gate must
    // read `.prompt` (fires HIGH via `how`/`does`), resolve the project from
    // `.cwd`, and NOT feed the literal `{"prompt"` blob into the gate.
    let payload = format!(
        r#"{{"prompt":"how does Counter work","cwd":"{}"}}"#,
        project.to_str().unwrap().replace('\\', "\\\\")
    );
    let (out, err, ok) = cli_stdin(&["prompt-hook"], &payload);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        out.contains("<codegraph_context"),
        "JSON payload must emit context, got:\n{out}"
    );
    assert!(
        out.contains("Counter"),
        "must reference Counter (read .prompt), got:\n{out}"
    );
    assert!(
        !out.contains("{\"prompt\""),
        "must not treat the JSON blob as the query, got:\n{out}"
    );
}

// === MEDIUM tier ============================================================

#[test]
fn medium_tier_emits_pointer_not_explore() {
    let (_dir, project) = index_medium("medium-pointer");
    // Non-structural bag-of-words prompt (NO gate word). Tier-A co-occurrence
    // hits CheckoutStateMachine (covers `checkout` + `state`).
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "checkout state machine",
    ]);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        !out.trim().is_empty(),
        "MEDIUM must not be silent, got empty"
    );
    assert!(
        out.contains("<codegraph_context"),
        "MEDIUM must wrap in <codegraph_context>, got:\n{out}"
    );
    assert!(
        out.contains("CheckoutStateMachine"),
        "MEDIUM must name the matched symbol, got:\n{out}"
    );
    assert!(
        out.contains("codegraph_explore ONCE"),
        "MEDIUM must carry the pointer guidance, got:\n{out}"
    );
    // NOT a full explore render.
    assert!(
        !out.contains("Exploration:") && !out.contains("Source Code"),
        "MEDIUM must NOT run full explore, got:\n{out}"
    );
}

#[test]
fn medium_tier_cooccurrence_matches() {
    let (_dir, project) = index_medium("medium-cooccur");
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "checkout state machine",
    ]);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        out.contains("CheckoutStateMachine"),
        "Tier-A co-occurrence must surface CheckoutStateMachine, got:\n{out}"
    );
}

#[test]
fn medium_tier_rare_single_word() {
    let (_dir, project) = index_medium("medium-rare");
    // Single non-structural word: `checkout` clusters across ≥2 names
    // (CheckoutService / CheckoutController / CheckoutStateMachine) → Tier B.
    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "checkout",
    ]);
    assert!(ok, "failed: stdout={out} stderr={err}");
    assert!(
        out.contains("<codegraph_context"),
        "Tier-B must emit context, got:\n{out}"
    );
    assert!(
        out.contains("Checkout"),
        "Tier-B rare word must surface a checkout-segment name, got:\n{out}"
    );
}

// === graceful degradation + hidden ==========================================

#[test]
fn prompt_hook_graceful_in_unindexed_dir() {
    let dir = TestDir::new("unindexed");
    let project = dir.path().join("empty");
    fs::create_dir_all(&project).unwrap();

    let (out, err, ok) = cli(&[
        "prompt-hook",
        "--path",
        project.to_str().unwrap(),
        "--query",
        "how does anything work",
    ]);
    assert!(
        ok,
        "prompt-hook in unindexed dir must exit 0; stdout={out} stderr={err}"
    );
    assert!(
        !err.contains("panicked"),
        "must not panic in an unindexed dir; stderr={err}"
    );
}

#[test]
fn prompt_hook_hidden_from_help() {
    let (out, _err, ok) = cli(&["--help"]);
    assert!(ok, "help failed");
    assert!(
        !out.contains("prompt-hook"),
        "prompt-hook must be hidden from the main help, got:\n{out}"
    );
}

#[test]
fn prompt_hook_kill_switch_silences() {
    let (_dir, project) = index_mini("kill-switch");
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args([
            "prompt-hook",
            "--path",
            project.to_str().unwrap(),
            "--query",
            "how does Counter work",
        ])
        .env("CODEGRAPH_NO_PROMPT_HOOK", "1")
        .output()
        .expect("run codegraph binary");
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "kill-switch run must exit 0");
    assert!(
        out.trim().is_empty(),
        "kill-switch must silence output, got:\n{out}"
    );
}
