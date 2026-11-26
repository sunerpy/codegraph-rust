//! CLI integration tests for `codegraph context` (W9-01): colby's command
//! (`src/bin/codegraph.ts` b635dd46 1421-1478, `src/context/`) over the
//! explore engine's search and walk.
//!
//! Drives the real `codegraph` binary against private copies of the committed
//! corpora (`crates/codegraph-bench/fixtures/{mini,typescript}`) and against
//! small projects written the way upstream's own tests write theirs.
//!
//! The four `tests/snapshots/context_*.stdout` files are the binary's stdout,
//! byte for byte (`.stdout`, so that no Markdown or JSON formatter rewrites
//! them). The mini ones are also byte-identical to what upstream's `e8ddc4e2`
//! build prints for the same query; the typescript ones differ from upstream's
//! in their entry points, which come from the Rust search (KNOWN_DIFFS,
//! Tier-2). To update them after an intended change, index a fresh copy of the
//! corpus with the release binary, write its stdout over each file and review
//! the diff before committing:
//!
//! ```text
//! codegraph init <copy-of-mini>
//! codegraph context Counter add runDemo -p <copy-of-mini> > tests/snapshots/context_mini.md.stdout
//! codegraph context --format json Counter add runDemo -p <copy-of-mini> > tests/snapshots/context_mini.json.stdout
//! codegraph init <copy-of-typescript>
//! codegraph context Screen callableSemanticsUniqueHelper generatorTask -p <copy-of-typescript> > tests/snapshots/context_typescript.md.stdout
//! codegraph context --format json Screen callableSemanticsUniqueHelper generatorTask -p <copy-of-typescript> > tests/snapshots/context_typescript.json.stdout
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const MINI_TASK: [&str; 3] = ["Counter", "add", "runDemo"];
const TYPESCRIPT_TASK: [&str; 3] = ["Screen", "callableSemanticsUniqueHelper", "generatorTask"];

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn snapshot(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/snapshots")
            .join(name),
    )
    .expect("read committed snapshot")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-context-{label}-{}-{}",
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

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Run {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    fn json(&self) -> Value {
        assert!(self.ok(), "context failed: {}", self.stderr);
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|err| panic!("stdout is not one JSON value ({err}): {}", self.stdout))
    }
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
        code: output.status.code(),
    }
}

/// `codegraph context <flags...> -p <project> <task...>`.
fn context(project: &Path, flags: &[&str], task: &[&str]) -> Run {
    let mut args = vec!["context"];
    args.extend_from_slice(flags);
    args.push("-p");
    args.push(project.to_str().unwrap());
    args.extend_from_slice(task);
    run_in(project, &args)
}

fn init(project: &Path) {
    let run = run_in(project, &["init", project.to_str().unwrap()]);
    assert!(run.ok(), "init failed: {} {}", run.stdout, run.stderr);
}

/// A private, indexed copy of a committed corpus.
fn indexed_corpus(dir: &TestDir, name: &str) -> PathBuf {
    let project = dir.path().join(name);
    copy_tree(
        &workspace_root()
            .join("crates/codegraph-bench/fixtures")
            .join(name),
        &project,
    );
    init(&project);
    project
}

/// Upstream `__tests__/cli-context-command.test.ts`'s project.
fn auth_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("auth");
    write(
        &project,
        "src/auth.ts",
        "export function parseToken(t: string){ return parseTokenExpiry(t) + t.trim().length; }\n\
         export function parseTokenExpiry(t: string){ return Date.parse(t); }\n",
    );
    init(&project);
    project
}

/// Upstream `__tests__/synthesized-hop.test.ts`'s interface dispatch: the
/// `iface` pass bridges `Exporter.exportReport` to `PdfExporter.exportReport`.
fn synthesized_hop_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("synthhop");
    write(
        &project,
        "src/exporter.ts",
        "export interface Exporter {\n  exportReport(data: string): string;\n}\n",
    );
    write(
        &project,
        "src/pdf-exporter.ts",
        "import type { Exporter } from './exporter';\nimport { renderPdf } from './render';\n\nexport class PdfExporter implements Exporter {\n  exportReport(data: string): string {\n    return renderPdf(data);\n  }\n}\n",
    );
    write(
        &project,
        "src/render.ts",
        "export function renderPdf(data: string): string {\n  return '%PDF ' + data;\n}\n",
    );
    write(
        &project,
        "src/run.ts",
        "import type { Exporter } from './exporter';\n\nexport function runExport(exporter: Exporter): string {\n  return exporter.exportReport('q3');\n}\n",
    );
    init(&project);
    project
}

#[test]
fn help_lists_context_and_its_flags() {
    let dir = TestDir::new("help");
    let top = run_in(dir.path(), &["--help"]);
    assert!(top.ok(), "--help failed: {}", top.stderr);
    assert!(
        top.stdout
            .lines()
            .any(|line| line.trim_start().starts_with("context ")),
        "top-level --help must list `context`: {}",
        top.stdout
    );
    let help = run_in(dir.path(), &["context", "--help"]);
    assert!(help.ok(), "context --help failed: {}", help.stderr);
    for flag in [
        "-p, --path <PATH>",
        "-f, --format <FORMAT>",
        "[default: markdown]",
        "-n, --max-nodes <NUMBER>",
        "--no-code",
        "<TASK>...",
    ] {
        assert!(
            help.stdout.contains(flag),
            "missing {flag}: {}",
            help.stdout
        );
    }
}

#[test]
fn the_mini_answer_matches_its_snapshots() {
    let dir = TestDir::new("mini");
    let mini = indexed_corpus(&dir, "mini");
    let markdown = context(&mini, &[], &MINI_TASK);
    assert!(markdown.ok(), "{}", markdown.stderr);
    assert_eq!(markdown.stdout, snapshot("context_mini.md.stdout"));
    assert_eq!(markdown.stderr, "");
    let json = context(&mini, &["--format", "json"], &MINI_TASK);
    assert!(json.ok(), "{}", json.stderr);
    assert_eq!(json.stdout, snapshot("context_mini.json.stdout"));
    json.json();
}

#[test]
fn the_typescript_answer_matches_its_snapshots() {
    let dir = TestDir::new("typescript");
    let typescript = indexed_corpus(&dir, "typescript");
    let markdown = context(&typescript, &[], &TYPESCRIPT_TASK);
    assert!(markdown.ok(), "{}", markdown.stderr);
    assert_eq!(markdown.stdout, snapshot("context_typescript.md.stdout"));
    let json = context(&typescript, &["--format", "json"], &TYPESCRIPT_TASK);
    assert!(json.ok(), "{}", json.stderr);
    assert_eq!(json.stdout, snapshot("context_typescript.json.stdout"));
}

/// Upstream `__tests__/cli-context-command.test.ts` (e8ddc4e2 35-113).
#[test]
fn upstream_s_context_command_cases_hold() {
    let dir = TestDir::new("upstream-cases");
    let auth = auth_project(&dir);
    let task = ["parseToken", "expiry", "handling"];

    let json = context(&auth, &["--format", "json"], &task).json();
    assert_eq!(json["query"], "parseToken expiry handling");
    assert!(!json["nodes"].as_array().unwrap().is_empty(), "{json}");
    assert!(!json["codeBlocks"].as_array().unwrap().is_empty(), "{json}");

    let one = context(&auth, &["--format", "json", "--max-nodes", "1"], &task).json();
    assert!(one["nodes"].as_array().unwrap().len() <= 1, "{one}");

    // The Memorix invocation: --format json --max-nodes 8 --no-code.
    let memorix = context(
        &auth,
        &["--format", "json", "--max-nodes", "8", "--no-code"],
        &task,
    )
    .json();
    assert_eq!(memorix["codeBlocks"], serde_json::json!([]));
    assert!(
        !memorix["nodes"].as_array().unwrap().is_empty(),
        "{memorix}"
    );

    let markdown = context(&auth, &[], &task);
    assert!(markdown.ok(), "{}", markdown.stderr);
    assert!(
        markdown.stdout.contains("## Code Context"),
        "{}",
        markdown.stdout
    );
    assert!(
        markdown
            .stdout
            .contains("**Query:** parseToken expiry handling"),
        "{}",
        markdown.stdout
    );

    let yaml = context(&auth, &["--format", "yaml"], &["task"]);
    assert_eq!(yaml.code, Some(1));
    assert!(yaml.stderr.contains("Unknown format"), "{}", yaml.stderr);
}

/// OD-B81: colby's sentences behind the CLI's `Error: `, exit status 1,
/// nothing on stdout; the format and `--max-nodes` are checked before the
/// project, so an uninitialized directory reports them first.
#[test]
fn bad_flags_are_refused_with_colby_s_sentences_before_the_project() {
    let dir = TestDir::new("flags");
    let mini = indexed_corpus(&dir, "mini");
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    for project in [&mini, &empty] {
        for (flags, sentence) in [
            (
                vec!["--format", "yaml"],
                "Error: Unknown format \"yaml\" — use \"markdown\" or \"json\".\n",
            ),
            (
                vec!["--format", "JSON"],
                "Error: Unknown format \"JSON\" — use \"markdown\" or \"json\".\n",
            ),
            (
                vec!["--max-nodes", "0"],
                "Error: --max-nodes expects a positive integer, got \"0\".\n",
            ),
            (
                vec!["--max-nodes", "-5"],
                "Error: --max-nodes expects a positive integer, got \"-5\".\n",
            ),
            (
                vec!["--max-nodes", "abc"],
                "Error: --max-nodes expects a positive integer, got \"abc\".\n",
            ),
        ] {
            let run = context(project, &flags, &["add"]);
            assert_eq!(run.code, Some(1), "{flags:?}: {}", run.stderr);
            assert_eq!(run.stderr, sentence, "{flags:?}");
            assert_eq!(run.stdout, "", "{flags:?}");
        }
    }
}

#[test]
fn an_uninitialized_project_is_refused_with_colby_s_sentence() {
    let dir = TestDir::new("uninit");
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let run = context(&empty, &[], &["some", "task"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with(&format!(
            "Error: CodeGraph not initialized in {}",
            empty.display()
        )),
        "{}",
        run.stderr
    );
}

/// A missing task is clap's usage error (exit status 2), where colby's
/// commander exits 1 (KNOWN_DIFFS).
#[test]
fn a_missing_task_is_a_usage_error() {
    let dir = TestDir::new("notask");
    let mini = indexed_corpus(&dir, "mini");
    let run = context(&mini, &[], &[]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("<TASK>..."), "{}", run.stderr);
}

/// OD-B81: `--max-nodes` takes its leading number as `parseInt` does.
#[test]
fn max_nodes_reads_its_leading_number() {
    let dir = TestDir::new("parseint");
    let mini = indexed_corpus(&dir, "mini");
    let json = |flags: &[&str]| context(&mini, flags, &["add"]).json();
    assert_eq!(
        json(&["--format", "json", "--max-nodes", "5abc"]),
        json(&["--format", "json", "--max-nodes", "5"])
    );
    let one = json(&["--format", "json", "--max-nodes", "1.9"]);
    assert_eq!(one, json(&["--format", "json", "--max-nodes", "1"]));
    assert_eq!(one["nodes"].as_array().unwrap().len(), 1, "{one}");
    assert_eq!(
        json(&["--format", "json", "--max-nodes=2"]),
        json(&["-f", "json", "-n", "2"])
    );
}

#[test]
fn the_task_s_words_join_and_an_empty_task_prints_the_header() {
    let dir = TestDir::new("task");
    let mini = indexed_corpus(&dir, "mini");
    let joined = context(&mini, &["--format", "json"], &["Counter", "add runDemo"]).json();
    assert_eq!(joined["query"], "Counter add runDemo");
    // Options may come after the task's first word, as commander allows.
    let interleaved = run_in(
        &mini,
        &[
            "context",
            "Counter",
            "-p",
            mini.to_str().unwrap(),
            "add",
            "runDemo",
        ],
    );
    assert_eq!(interleaved.stdout, snapshot("context_mini.md.stdout"));
    let empty = context(&mini, &[], &[""]);
    assert!(empty.ok(), "{}", empty.stderr);
    assert_eq!(empty.stdout, "## Code Context\n\n**Query:** \n\n");
}

/// `--no-code` drops the code and keeps the call paths; `--max-nodes 1`
/// keeps the first entry point alone.
#[test]
fn no_code_and_one_node_shape_the_answer() {
    let dir = TestDir::new("shape");
    let mini = indexed_corpus(&dir, "mini");
    let markdown = context(&mini, &["--no-code"], &MINI_TASK);
    assert!(markdown.ok(), "{}", markdown.stderr);
    assert!(!markdown.stdout.contains("### Code"), "{}", markdown.stdout);
    assert!(
        markdown
            .stdout
            .contains("- src/math.ts: increment:8\n\n\n## Call paths\n"),
        "{}",
        markdown.stdout
    );
    let one = context(&mini, &["--max-nodes", "1"], &MINI_TASK);
    assert!(one.ok(), "{}", one.stderr);
    assert!(
        one.stdout.starts_with("## Code Context\n\n**Query:** Counter add runDemo\n\n### Entry Points\n\n- **runDemo** (function) - src/app.ts:3\n  `(): number`\n\n### Code\n\n#### runDemo (src/app.ts:3)\n"),
        "{}",
        one.stdout
    );
    assert!(
        !one.stdout.contains("### Related Symbols"),
        "{}",
        one.stdout
    );
    assert!(!one.stdout.contains("## Call paths"), "{}", one.stdout);
}

/// #2428: the interface hop of upstream's synthesized-hop test reads inside
/// the arrow, in `synthesized_hop`'s wording, with the `[…]` footnote.
#[test]
fn a_synthesized_hop_reads_inside_the_arrow() {
    let dir = TestDir::new("synthhop");
    let project = synthesized_hop_project(&dir);
    let run = context(&project, &[], &["runExport", "exportReport", "renderPdf"]);
    assert!(run.ok(), "{}", run.stderr);
    assert!(
        run.stdout.contains("\n\n## Call paths\n\nExecution flow among the key symbols (traced through the call graph):\n\n- runExport → exportReport →[interface → impl @src/pdf-exporter.ts:5] exportReport → renderPdf\n\n_Hops marked `[…]` are dynamic dispatch bridged by codegraph (with the registration site); the rest are direct calls. codegraph_node any symbol for its body._\n"),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("→[event"), "{}", run.stdout);
}

/// A file edited after the last sync gives no code block: its symbol stays
/// listed, nothing is sliced at its stale lines, and stderr names the file.
#[test]
fn a_drifted_file_s_code_is_left_out_and_named_on_stderr() {
    let dir = TestDir::new("drift");
    let project = synthesized_hop_project(&dir);
    let render = project.join("src/render.ts");
    let source = fs::read_to_string(&render).unwrap();
    fs::write(&render, format!("// edited after the sync\n{source}")).unwrap();
    let task = ["runExport", "exportReport", "renderPdf"];

    let markdown = context(&project, &[], &task);
    assert!(markdown.ok(), "{}", markdown.stderr);
    assert!(
        markdown.stdout.contains("- src/render.ts: renderPdf:1\n"),
        "{}",
        markdown.stdout
    );
    assert!(
        !markdown.stdout.contains("#### renderPdf") && !markdown.stdout.contains("edited after"),
        "{}",
        markdown.stdout
    );
    assert_eq!(
        markdown.stderr,
        "warning: left out the code of symbols in files that changed since the last index sync: src/render.ts; run `codegraph sync` to index them again\n"
    );

    let json = context(&project, &["--format", "json"], &task).json();
    assert!(
        json["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["name"] == "renderPdf"),
        "{json}"
    );
    for block in json["codeBlocks"].as_array().unwrap() {
        assert_ne!(block["filePath"], "src/render.ts", "{json}");
        assert!(
            !block["content"].as_str().unwrap().contains("edited after"),
            "{json}"
        );
    }

    let sync = run_in(&project, &["sync", project.to_str().unwrap()]);
    assert!(sync.ok(), "sync failed: {}", sync.stderr);
    let synced = context(&project, &[], &task);
    assert!(synced.ok(), "{}", synced.stderr);
    assert_eq!(synced.stderr, "");
    assert!(
        synced.stdout.contains("#### renderPdf (src/render.ts:2)"),
        "{}",
        synced.stdout
    );
}

/// OD-B80: colby's low-confidence rule over the entry points (upstream
/// `__tests__/context-ranking.test.ts` e8ddc4e2 108-189, its FLAT trap).
#[test]
fn a_common_word_match_carries_the_low_confidence_note() {
    let dir = TestDir::new("confidence");
    let project = dir.path().join("ctxrank");
    write(
        &project,
        "src/app/capture/intro.tsx",
        "export function CaptureIntroScreen() {\n  // Onboarding screen shown before the user selects flat or standing object capture.\n  return null;\n}\n",
    );
    write(
        &project,
        "scripts/dataset/download.ts",
        "export const FLAT = 'freiburg_flat_dataset';\nexport function downloadDataset(name: string): string { return name; }\n",
    );
    init(&project);

    let low = context(&project, &[], &["flat", "object", "thing"]);
    assert!(low.ok(), "{}", low.stderr);
    assert!(
        low.stdout.ends_with("```\n\n\n### ⚠️ Low-confidence match\n\nThis query matched mostly on common words, so the entry points above may be off-target — treat them as a starting point, not a complete answer. For a reliable result:\n- `codegraph_explore` with the **exact symbol names** you are after (class / function / method names), or\n- `codegraph_search <name>` for one specific symbol\n- `codegraph_files` a likely area: `scripts/dataset`\n\nDo not assume the list above is comprehensive.\n"),
        "{}",
        low.stdout
    );
    for task in [
        &["CaptureIntroScreen"][..],
        &["capture", "intro", "onboarding", "screen", "flat", "object"][..],
    ] {
        let high = context(&project, &[], task);
        assert!(high.ok(), "{}", high.stderr);
        assert!(
            !high.stdout.contains("Low-confidence"),
            "{task:?}: {}",
            high.stdout
        );
    }
    let json = context(
        &project,
        &["--format", "json"],
        &["flat", "object", "thing"],
    );
    assert!(!json.stdout.contains("Low-confidence"), "{}", json.stdout);
    json.json();
}

/// OD-B81 (#383): a Spring properties key gives its key alone as its code,
/// never the value on its line.
#[test]
fn a_config_key_s_code_block_is_the_key_alone() {
    let dir = TestDir::new("config-leaf");
    let project = dir.path().join("spring");
    write(
        &project,
        "build.gradle",
        "plugins { id 'org.springframework.boot' }\n",
    );
    write(
        &project,
        "src/main/resources/application.properties",
        "db.password=s3cr3t-value\n",
    );
    init(&project);
    let markdown = context(&project, &[], &["password"]);
    assert!(markdown.ok(), "{}", markdown.stderr);
    assert!(
        markdown.stdout.contains(
            "#### password (src/main/resources/application.properties:1)\n\n```properties\ndb.password\n```\n"
        ),
        "{}",
        markdown.stdout
    );
    let json = context(&project, &["--format", "json"], &["password"]);
    assert_eq!(json.json()["codeBlocks"][0]["content"], "db.password");
    for run in [&markdown, &json] {
        assert!(!run.stdout.contains("s3cr3t"), "{}", run.stdout);
    }
}

/// The context of a synced index is the context of a fresh one.
#[test]
fn a_synced_index_answers_as_a_fresh_one() {
    let dir = TestDir::new("sync");
    let synced = indexed_corpus(&dir, "mini");
    let edit = |root: &Path| {
        let math = root.join("src/math.ts");
        let source = fs::read_to_string(&math).unwrap();
        fs::write(
            &math,
            format!("{source}\nexport function subtract(left: number, right: number): number {{\n  return add(left, -right);\n}}\n"),
        )
        .unwrap();
    };
    edit(&synced);
    let sync = run_in(&synced, &["sync", synced.to_str().unwrap()]);
    assert!(sync.ok(), "sync failed: {}", sync.stderr);

    let fresh = dir.path().join("fresh");
    copy_tree(
        &workspace_root().join("crates/codegraph-bench/fixtures/mini"),
        &fresh,
    );
    edit(&fresh);
    init(&fresh);

    let task = ["Counter", "add", "subtract"];
    for flags in [&[][..], &["--format", "json"][..]] {
        let a = context(&synced, flags, &task);
        let b = context(&fresh, flags, &task);
        assert!(a.ok() && b.ok(), "{} {}", a.stderr, b.stderr);
        assert_eq!(a.stdout, b.stdout, "{flags:?}");
        assert!(a.stdout.contains("subtract"), "{}", a.stdout);
    }
}
