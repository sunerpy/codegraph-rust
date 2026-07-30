//! Upstream #1372 — non-ASCII identifiers must be FINDABLE and RANKED
//! correctly through the real user-facing surfaces.
//!
//! Drives the actual `codegraph` binary (`init` then `query --json`) against a
//! temp project whose files are named in SEVEN different scripts, each paired
//! with an ASCII-named DECOY file that defines an identically-named function.
//! The definer inside the script-named file must outrank its decoy, because the
//! query names that file. The scripts are deliberately not just the one the fix
//! was written for: CJK, Cyrillic, Greek, Kana, Hangul, accented Latin, and an
//! ASCII control that must stay byte-identical in behavior.

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
            "codegraph-unicode-search-{label}-{}-{}",
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

/// `(module-file stem, function name)` pairs. The module stem is what the query
/// names; the function is defined TWICE — once in `<stem>.py`, once in the
/// ASCII decoy `zdecoy_<tag>.py`. The decoy is named so it sorts and matches
/// nothing in the query: only path relevance can separate the two.
const CASES: &[(&str, &str, &str)] = &[
    ("示例模块", "handlercjk", "cjk"),
    ("модуль", "handlercyr", "cyr"),
    ("μονάδα", "handlergrk", "grk"),
    ("サンプル", "handlerkana", "kana"),
    ("모듈", "handlerkor", "kor"),
    ("café_módulo", "handleracc", "acc"),
    ("samplemodule", "handlerascii", "ascii"),
];

/// A two-character CJK module stem — below the 3-char floor ASCII tokens use.
/// Unsegmented scripts pack a whole word into two characters, so this shape
/// must rank as well as the four-character one.
const SHORT_CASE: (&str, &str, &str) = ("模块", "handlershort", "short");

fn write_project(root: &Path) {
    let body = |func: &str| format!("def {func}():\n    return 1\n");
    for (stem, func, tag) in CASES.iter().chain(std::iter::once(&SHORT_CASE)) {
        std::fs::write(root.join(format!("{stem}.py")), body(func)).unwrap();
        std::fs::write(root.join(format!("zdecoy_{tag}.py")), body(func)).unwrap();
    }
}

/// `(filePath, name)` for every hit, in the order the CLI ranked them.
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
                r["node"]["filePath"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                r["node"]["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    write_project(&project);
    let run = run_in(dir.path(), &["init", project.to_str().unwrap()]);
    assert!(run.ok, "init failed: {} {}", run.stdout, run.stderr);
    project
}

/// The definer in the file the query NAMES must outrank the same-named decoy in
/// every script, not just the one the fix was written against.
#[test]
fn non_ascii_module_name_outranks_ascii_decoy_in_every_script() {
    let dir = TestDir::new("rank");
    let project = indexed_project(&dir);

    let mut failures: Vec<String> = Vec::new();
    for (stem, func, tag) in CASES.iter().chain(std::iter::once(&SHORT_CASE)) {
        let hits = ranked(&project, &format!("{stem} {func}"));
        let want = format!("{stem}.py");
        let decoy = format!("zdecoy_{tag}.py");
        let pos = |file: &str| hits.iter().position(|(f, n)| f == file && n == func);
        match (pos(&want), pos(&decoy)) {
            (Some(w), Some(d)) if w < d => {}
            (w, d) => failures.push(format!(
                "[{tag}] query {stem:?} {func:?}: definer {want} at {w:?}, decoy {decoy} at {d:?}; ranking was {hits:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "the query names the module file, so its definer must rank first:\n{}",
        failures.join("\n")
    );
}

/// A non-ASCII SYMBOL name (not just a file name) stays findable by exact name.
#[test]
fn non_ascii_symbol_name_is_findable_by_exact_name() {
    let dir = TestDir::new("symbol");
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("mod.py"),
        "def 示例函数():\n    return 1\n\n\ndef 데이터조회():\n    return 2\n",
    )
    .unwrap();
    let run = run_in(dir.path(), &["init", project.to_str().unwrap()]);
    assert!(run.ok, "init failed: {} {}", run.stdout, run.stderr);

    for name in ["示例函数", "데이터조회"] {
        let hits = ranked(&project, name);
        assert_eq!(
            hits.first().map(|(_, n)| n.as_str()),
            Some(name),
            "exact non-ASCII symbol {name:?} must rank first, got {hits:?}"
        );
    }
}

/// The same ranking must hold over the MCP `codegraph_search` contract, not
/// only the CLI: `serve --mcp` against the same index, driven over real stdio.
#[cfg(unix)]
#[test]
fn mcp_search_ranks_the_non_ascii_definer_over_its_decoy() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = TestDir::new("mcp");
    let project = indexed_project(&dir);

    let mut child = Command::new(bin())
        .args(["serve", "--mcp", "--path", project.to_str().unwrap()])
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve --mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));
    let deadline = Instant::now() + Duration::from_secs(30);

    let send = |stdin: &mut std::process::ChildStdin, v: serde_json::Value| {
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = |want_id: i64| -> serde_json::Value {
        loop {
            assert!(Instant::now() < deadline, "timed out awaiting id {want_id}");
            let mut line = String::new();
            match stdout.read_line(&mut line) {
                Ok(0) => panic!("serve --mcp closed stdout before id {want_id}"),
                Ok(_) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim())
                        && v.get("id").and_then(serde_json::Value::as_i64) == Some(want_id)
                    {
                        return v;
                    }
                }
                Err(e) => panic!("reading serve --mcp stdout: {e}"),
            }
        }
    };

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "unicode-search-test", "version": "0" }
            }
        }),
    );
    let _ = recv(1);
    send(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {
                "name": "codegraph_search",
                "arguments": { "query": "示例模块 handlercjk" }
            }
        }),
    );
    let resp = recv(2);
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    assert_ne!(
        resp["result"]["isError"],
        serde_json::json!(true),
        "MCP search must not error on a non-ASCII query: {text}"
    );
    let definer = text
        .find("示例模块.py")
        .unwrap_or_else(|| panic!("MCP search must surface the non-ASCII definer file: {text}"));
    let decoy = text
        .find("zdecoy_cjk.py")
        .unwrap_or_else(|| panic!("MCP search must surface the decoy too: {text}"));
    assert!(
        definer < decoy,
        "MCP search must rank the named module's definer above its decoy: {text}"
    );
}

/// Ranking must be a pure function of the index, not of incidental row order:
/// the same query over the same index answers identically every time.
#[test]
fn non_ascii_ranking_is_deterministic_across_repeated_queries() {
    let dir = TestDir::new("determinism");
    let project = indexed_project(&dir);
    let first = ranked(&project, "示例模块 handlercjk");
    for _ in 0..4 {
        assert_eq!(
            ranked(&project, "示例模块 handlercjk"),
            first,
            "repeated identical queries must return an identical ranking"
        );
    }
}
