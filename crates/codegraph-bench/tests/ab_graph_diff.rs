//! Failure injection for `bench --graph-diff` and `bench --ab`.
//!
//! The graph-diff tests run the real `bench` binary on committed goldens. The
//! A/B tests drive `run_ab` with stand-in shell scripts instead of a real
//! `codegraph`: each one copies a committed golden database into the project's
//! index directory, so every failure mode the harness must stop on is cheap to
//! provoke and the tests never touch the large corpora.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use codegraph_bench::graph_diff::{diff_graphs, load_graph};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-bench lives under crates/")
        .to_path_buf()
}

fn golden_dir(name: &str) -> PathBuf {
    workspace().join("reference/golden").join(name)
}

fn golden_db(name: &str) -> PathBuf {
    golden_dir(name).join("colby.db")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-bench-ab-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create test dir");
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

fn graph_diff(left: &Path, right: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bench"))
        .arg("--graph-diff")
        .arg(left)
        .arg(right)
        .output()
        .expect("run bench --graph-diff")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Copy a committed golden database and change it: drop one `calls` edge and
/// rename one function.
fn tampered_copy(golden: &str, destination: &Path) {
    fs::copy(golden_db(golden), destination).expect("copy golden db");
    let conn = rusqlite::Connection::open(destination).expect("open copy");
    let removed = conn
        .execute(
            "DELETE FROM edges WHERE rowid = (SELECT MIN(rowid) FROM edges WHERE kind = 'calls')",
            [],
        )
        .expect("delete an edge");
    assert_eq!(removed, 1, "the {golden} golden must have a calls edge");
    let renamed = conn
        .execute(
            "UPDATE nodes SET name = name || '_tampered' \
             WHERE id = (SELECT MIN(id) FROM nodes WHERE kind = 'function')",
            [],
        )
        .expect("rename a function");
    assert_eq!(renamed, 1, "the {golden} golden must have a function");
}

#[test]
fn graph_diff_of_a_golden_directory_with_itself_is_identical() {
    let output = graph_diff(&golden_dir("go"), &golden_dir("go"));
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("(golden)"), "{text}");
    assert!(text.ends_with("\nresult: identical\n"), "{text}");
}

#[test]
fn graph_diff_of_a_database_and_its_golden_is_identical_and_leaves_no_sidecars() {
    let db = golden_db("typescript");
    let output = graph_diff(&db, &golden_dir("typescript"));
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("(database)"), "{text}");
    assert!(text.ends_with("\nresult: identical\n"), "{text}");
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            !with_suffix(&db, suffix).exists(),
            "graph-diff must not open the committed database in place ({suffix})"
        );
    }
}

#[test]
fn graph_diff_reports_every_changed_row_of_a_tampered_database() {
    let dir = TestDir::new("graph-diff-tampered");
    let tampered = dir.path().join("tampered.db");
    tampered_copy("typescript", &tampered);

    let output = graph_diff(&golden_dir("typescript"), &tampered);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("\nnodes                  1        1\n"),
        "{text}"
    );
    assert!(
        text.contains("\nedges                  1        0\n"),
        "{text}"
    );
    assert!(text.contains("\nschema          identical\n"), "{text}");
    assert!(
        text.ends_with("\nresult: different (2 removed, 1 added rows; schema identical)\n"),
        "{text}"
    );

    // Every changed row the library finds is printed under its group header.
    let (_, golden) = load_graph(&golden_dir("typescript")).unwrap();
    let (_, changed) = load_graph(&tampered).unwrap();
    let diff = diff_graphs(&golden, &changed);
    assert_eq!(diff.groups.len(), 2, "one node group and one edge group");
    for group in &diff.groups {
        let key = &group.key;
        let header = format!(
            "@@ {} {} {} {} (-{} +{})\n",
            key.surface.name(),
            key.language,
            key.kind,
            key.resolved_by,
            group.removed.len(),
            group.added.len()
        );
        assert!(text.contains(&header), "missing {header} in {text}");
        for row in &group.removed {
            assert!(text.contains(&format!("\n- {row}\n")), "{text}");
        }
        for row in &group.added {
            assert!(text.contains(&format!("\n+ {row}\n")), "{text}");
        }
    }
    let edge_group = diff
        .groups
        .iter()
        .find(|group| group.key.surface.name() == "edges")
        .expect("an edge group");
    assert_eq!(edge_group.key.kind, "calls");
    assert_eq!(edge_group.key.language, "typescript");
    assert_ne!(edge_group.key.resolved_by, "-");

    // The report is deterministic.
    let again = graph_diff(&golden_dir("typescript"), &tampered);
    assert_eq!(again.stdout, output.stdout);
}

#[test]
fn graph_diff_refuses_a_corrupted_database_without_printing_a_report() {
    let dir = TestDir::new("graph-diff-corrupt");
    let garbage = dir.path().join("garbage.db");
    fs::write(&garbage, vec![0xA5_u8; 8192]).unwrap();
    let truncated = dir.path().join("truncated.db");
    let bytes = fs::read(golden_db("typescript")).unwrap();
    fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
    let empty = dir.path().join("empty.db");
    fs::write(&empty, b"").unwrap();

    for db in [&garbage, &truncated, &empty] {
        let output = graph_diff(&golden_dir("typescript"), db);
        assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
        assert!(
            output.stdout.is_empty(),
            "no partial report: {}",
            stdout(&output)
        );
        assert!(
            stderr(&output).contains("cannot canonicalize the database"),
            "{}",
            stderr(&output)
        );
    }
    assert!(stderr(&graph_diff(&golden_dir("go"), &garbage)).contains("file is not a database"));
    assert!(stderr(&graph_diff(&golden_dir("go"), &truncated)).contains("malformed"));
}

/// Overwrite the page-type byte of one index's root page. Table scans never
/// read index pages, so only an integrity check notices.
fn corrupt_an_index_page(db: &Path) {
    let (root_page, page_size): (i64, i64) = {
        let conn = rusqlite::Connection::open(db).expect("open copy");
        let root_page = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE type = 'index' AND rootpage > 1 \
                 ORDER BY name LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("the golden has an index");
        let page_size = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .expect("page size");
        (root_page, page_size)
    };
    let mut bytes = fs::read(db).unwrap();
    let offset = usize::try_from((root_page - 1) * page_size).unwrap();
    bytes[offset] = 0xEE;
    fs::write(db, bytes).unwrap();
}

#[test]
fn graph_diff_refuses_a_database_whose_index_pages_are_corrupted() {
    let dir = TestDir::new("graph-diff-index");
    let db = dir.path().join("index-corrupt.db");
    fs::copy(golden_db("typescript"), &db).unwrap();
    corrupt_an_index_page(&db);

    // Every canonical table still reads, so the corruption is invisible to the
    // oracle on its own.
    let readable = dir.path().join("readable.db");
    fs::copy(&db, &readable).unwrap();
    codegraph_bench::oracle::canonicalize_db(&readable)
        .expect("table scans do not touch index pages");

    let output = graph_diff(&golden_dir("typescript"), &db);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    assert!(
        stderr(&output).contains("PRAGMA quick_check failed"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn graph_diff_refuses_missing_and_non_golden_inputs() {
    let dir = TestDir::new("graph-diff-missing");
    let output = graph_diff(&golden_dir("go"), &dir.path().join("missing.db"));
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(output.stdout.is_empty());

    let source_dir = workspace().join("crates/codegraph-bench/fixtures/go");
    let output = graph_diff(&source_dir, &golden_dir("go"));
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot load the golden directory"),
        "{}",
        stderr(&output)
    );
}

#[cfg(unix)]
mod ab {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use codegraph_bench::ab::{
        AbConfig, AbCorpus, AbOp, AbReport, Threads, render_ab_markdown, run_ab, run_ab_with_probe,
    };
    use codegraph_bench::load_guard::{CpuTimes, LoadGuard, LoadProbe};
    use codegraph_bench::oracle::load_golden;

    use super::*;

    /// Start a freshly written script once, retrying while another test
    /// thread's fork still holds it open for writing (ETXTBSY).
    fn wait_until_executable(script: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match Command::new(script)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.wait();
                    return;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("cannot run {}: {error}", script.display()),
            }
        }
    }

    fn quoted(path: &Path) -> String {
        let text = path.to_str().expect("UTF-8 test path");
        assert!(!text.contains('\''), "test paths must not contain quotes");
        format!("'{text}'")
    }

    /// Write an executable `/bin/sh` script. Every script exits 64 unless its
    /// first argument is `init`, so the warm-up and `--version` probes are
    /// harmless.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\n[ \"$1\" = init ] || exit 64\nproject=$2\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        wait_until_executable(&path);
        path
    }

    /// A stand-in `codegraph` whose `init` checks that it got a fresh copy of
    /// the corpus, logs its environment, and installs `db` as the index.
    fn copying_codegraph(dir: &Path, name: &str, db: &Path, log: &Path) -> PathBuf {
        script(
            dir,
            name,
            &format!(
                "[ -f \"$project/main.go\" ] || exit 65\n\
                 [ -e \"$project/.codegraph\" ] && exit 66\n\
                 [ -e \"$project/.git\" ] && exit 67\n\
                 echo \"${{0##*/}} ${{RAYON_NUM_THREADS:-unset}} ${{CODEGRAPH_NO_DAEMON:-unset}} \
                 ${{CODEGRAPH_NO_WATCH:-unset}} ${{CODEGRAPH_DIR:-unset}}\" >> {log}\n\
                 mkdir \"$project/.codegraph\" && cp {db} \"$project/.codegraph/codegraph.db\"",
                log = quoted(log),
                db = quoted(db)
            ),
        )
    }

    /// A one-file corpus that also carries VCS metadata and a stale index,
    /// neither of which may reach a run's project.
    fn tiny_corpus(dir: &Path) -> PathBuf {
        let corpus = dir.join("corpus");
        fs::create_dir_all(corpus.join(".git")).unwrap();
        fs::create_dir_all(corpus.join(".codegraph")).unwrap();
        fs::write(corpus.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(corpus.join(".codegraph/codegraph.db"), b"stale").unwrap();
        fs::write(corpus.join("main.go"), b"package main\n\nfunc main() {}\n").unwrap();
        corpus
    }

    fn config(
        baseline: PathBuf,
        candidate: PathBuf,
        corpus: &Path,
        threads: Vec<usize>,
        runs: usize,
    ) -> AbConfig {
        AbConfig {
            baseline,
            candidates: vec![candidate],
            corpora: vec![AbCorpus {
                name: "tiny".to_string(),
                commit: None,
                tag: None,
                source: corpus.to_path_buf(),
            }],
            threads: threads.into_iter().map(Threads::Count).collect(),
            runs,
            run_timeout: Duration::from_secs(60),
            op: AbOp::Init,
            load_guard: None,
            workspace_root: None,
            argv: Vec::new(),
        }
    }

    fn first_existing(candidates: &[&str]) -> PathBuf {
        candidates
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .unwrap_or_else(|| panic!("none of {candidates:?} exists"))
    }

    fn failure(config: &AbConfig) -> String {
        let error = run_ab(config).expect_err("the A/B must stop");
        format!("{error:#}")
    }

    #[test]
    fn ab_times_both_binaries_at_each_thread_count_and_reports_identical_graphs() {
        let dir = TestDir::new("identical");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let golden = golden_db("go");
        let baseline = copying_codegraph(dir.path(), "baseline", &golden, &log);
        let candidate = copying_codegraph(dir.path(), "candidate", &golden, &log);

        let report =
            run_ab(&config(baseline, candidate, &corpus, vec![3, 1], 3)).expect("the A/B succeeds");

        assert_eq!(report.kind, "codegraph-ab");
        assert_eq!(report.baseline.sha256.len(), 64);
        assert_eq!(report.settings.thread_env, "RAYON_NUM_THREADS");
        let corpus_report = &report.corpora[0];
        assert!(corpus_report.graph.identical);
        assert!(corpus_report.graph.differing_surfaces.is_empty());
        assert_eq!(corpus_report.copied_files, 1);
        let expected = load_golden(&golden_dir("go")).unwrap();
        let rows = corpus_report.graph.baseline.rows;
        assert_eq!(rows.nodes, expected.nodes.len() as u64);
        assert_eq!(rows.edges, expected.edges.len() as u64);
        assert_eq!(rows.unresolved_refs, expected.unresolved_refs.len() as u64);
        assert_eq!(rows.files, expected.files.len() as u64);

        let threads: Vec<usize> = corpus_report
            .cells
            .iter()
            .map(|cell| cell.threads)
            .collect();
        assert_eq!(threads, vec![3, 1]);
        for cell in &corpus_report.cells {
            for series in [&cell.baseline, &cell.candidate] {
                let warmups: Vec<bool> = series.runs.iter().map(|run| run.warmup).collect();
                assert_eq!(warmups, vec![true, false, false]);
                assert_eq!(series.wall_ms.samples, 2);
                assert!(series.peak_rss_kb.is_some(), "wait4 reports peak RSS");
                assert!(series.runs.iter().all(|run| {
                    run.canonical_hash.as_deref()
                        == Some(corpus_report.graph.baseline.hash.as_str())
                }));
            }
            assert!(cell.ratio.wall_ms.is_some());
            assert_eq!(cell.ratio.canonical_rows, Some(1.0));
            assert!(cell.budget.init_wall_ok.is_some());
        }

        // Every run got its thread count and the daemon/watch opt-outs, with
        // CODEGRAPH_DIR removed, and the binaries alternated which went first.
        let lines: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        let mut expected_lines = Vec::new();
        for threads in [3, 1] {
            for binary in [
                "baseline",
                "candidate",
                "candidate",
                "baseline",
                "baseline",
                "candidate",
            ] {
                expected_lines.push(format!("{binary} {threads} 1 1 unset"));
            }
        }
        assert_eq!(lines, expected_lines);

        let json = serde_json::to_string(&report).unwrap();
        let parsed: AbReport = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.corpora[0].graph.candidate.hash,
            corpus_report.graph.baseline.hash
        );
        let markdown = render_ab_markdown(&report);
        assert!(markdown.contains("| tiny | 3 |"), "{markdown}");
        assert!(markdown.contains("| identical |"), "{markdown}");
    }

    #[test]
    fn ab_reports_a_candidate_that_changes_the_graph() {
        let dir = TestDir::new("changed");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let tampered = dir.path().join("tampered.db");
        tampered_copy("typescript", &tampered);
        let baseline = copying_codegraph(dir.path(), "baseline", &golden_db("typescript"), &log);
        let candidate = copying_codegraph(dir.path(), "candidate", &tampered, &log);

        let report = run_ab(&config(baseline, candidate, &corpus, vec![2], 2))
            .expect("a graph difference is reported, not fatal");

        let graph = &report.corpora[0].graph;
        assert!(!graph.identical);
        assert_eq!(graph.differing_surfaces, vec!["nodes", "edges"]);
        assert_eq!(graph.candidate.rows.edges + 1, graph.baseline.rows.edges);
        assert_ne!(graph.baseline.hash, graph.candidate.hash);
        let rows_ratio = report.corpora[0].cells[0].ratio.canonical_rows.unwrap();
        assert!(rows_ratio < 1.0, "{rows_ratio}");
        assert!(render_ab_markdown(&report).contains("| differs: nodes, edges |"));
    }

    #[test]
    fn ab_stops_when_a_run_exits_non_zero() {
        let dir = TestDir::new("false");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let baseline = copying_codegraph(dir.path(), "baseline", &golden_db("go"), &log);
        let false_binary = first_existing(&["/bin/false", "/usr/bin/false"]);

        let message = failure(&config(baseline, false_binary, &corpus, vec![1], 2));
        assert!(message.contains("candidate run 1/2 on tiny"), "{message}");
        assert!(message.contains("exited with status 1"), "{message}");
    }

    #[test]
    fn ab_stops_when_a_run_exits_zero_without_a_database() {
        let dir = TestDir::new("true");
        let corpus = tiny_corpus(dir.path());
        let true_binary = first_existing(&["/bin/true", "/usr/bin/true"]);

        let message = failure(&config(
            true_binary.clone(),
            true_binary,
            &corpus,
            vec![1],
            2,
        ));
        assert!(message.contains("baseline run 1/2 on tiny"), "{message}");
        assert!(message.contains("produced no database"), "{message}");
    }

    #[test]
    fn ab_stops_on_a_database_that_cannot_be_canonicalized() {
        let dir = TestDir::new("corrupt");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let garbage = dir.path().join("garbage.db");
        fs::write(&garbage, vec![0xA5_u8; 8192]).unwrap();
        let baseline = copying_codegraph(dir.path(), "baseline", &golden_db("go"), &log);
        let candidate = copying_codegraph(dir.path(), "candidate", &garbage, &log);

        let message = failure(&config(baseline, candidate, &corpus, vec![1], 2));
        assert!(message.contains("candidate run 1/2 on tiny"), "{message}");
        assert!(message.contains("cannot be canonicalized"), "{message}");
        assert!(message.contains("file is not a database"), "{message}");
    }

    #[test]
    fn ab_stops_when_runs_of_one_binary_disagree() {
        let dir = TestDir::new("flaky");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let tampered = dir.path().join("tampered.db");
        tampered_copy("typescript", &tampered);
        let counter = dir.path().join("counter");
        let baseline = copying_codegraph(dir.path(), "baseline", &golden_db("typescript"), &log);
        // Odd invocations install the golden graph, even ones the tampered one.
        let candidate = script(
            dir.path(),
            "flaky",
            &format!(
                "n=$(cat {counter} 2>/dev/null || echo 0)\nn=$((n + 1))\necho \"$n\" > {counter}\n\
                 db={golden}\n[ $((n % 2)) -eq 0 ] && db={tampered}\n\
                 mkdir \"$project/.codegraph\" && cp \"$db\" \"$project/.codegraph/codegraph.db\"",
                counter = quoted(&counter),
                golden = quoted(&golden_db("typescript")),
                tampered = quoted(&tampered)
            ),
        );

        let message = failure(&config(baseline, candidate, &corpus, vec![1], 3));
        assert!(message.contains("non-deterministic graph"), "{message}");
        assert!(message.contains("candidate run 2/3 on tiny"), "{message}");
        assert!(
            message.contains("differing surfaces: nodes, edges"),
            "{message}"
        );
    }

    #[test]
    fn ab_stops_when_one_binary_disagrees_with_itself_across_thread_counts() {
        let dir = TestDir::new("threads");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let tampered = dir.path().join("tampered.db");
        tampered_copy("typescript", &tampered);
        let baseline = copying_codegraph(dir.path(), "baseline", &golden_db("typescript"), &log);
        let candidate = script(
            dir.path(),
            "thread-dependent",
            &format!(
                "db={golden}\n[ \"$RAYON_NUM_THREADS\" = 1 ] || db={tampered}\n\
                 mkdir \"$project/.codegraph\" && cp \"$db\" \"$project/.codegraph/codegraph.db\"",
                golden = quoted(&golden_db("typescript")),
                tampered = quoted(&tampered)
            ),
        );

        let message = failure(&config(baseline, candidate, &corpus, vec![1, 2], 2));
        assert!(message.contains("non-deterministic graph"), "{message}");
        assert!(message.contains("RAYON_NUM_THREADS=2"), "{message}");
    }

    #[test]
    fn ab_kills_and_stops_on_a_run_past_its_timeout() {
        let dir = TestDir::new("hang");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let hanging = script(dir.path(), "hanging", "exec sleep 30");
        let candidate = copying_codegraph(dir.path(), "candidate", &golden_db("go"), &log);
        let mut config = config(hanging, candidate, &corpus, vec![1], 2);
        config.run_timeout = Duration::from_secs(1);

        let started = Instant::now();
        let message = failure(&config);
        let elapsed = started.elapsed();
        assert!(message.contains("baseline run 1/2 on tiny"), "{message}");
        assert!(
            message.contains("exceeded the 1 s run timeout"),
            "{message}"
        );
        assert!(
            elapsed >= Duration::from_secs(1),
            "the timeout elapsed: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(20),
            "the hung run was killed: {elapsed:?}"
        );
    }

    #[test]
    fn ab_rejects_settings_that_cannot_produce_a_measurement() {
        let dir = TestDir::new("settings");
        let corpus = tiny_corpus(dir.path());
        let true_binary = first_existing(&["/bin/true", "/usr/bin/true"]);
        let base = config(true_binary.clone(), true_binary, &corpus, vec![1], 2);
        let rejected = |mutate: &dyn Fn(&mut AbConfig), expected: &str| {
            let mut config = base.clone();
            mutate(&mut config);
            let message = failure(&config);
            assert!(message.contains(expected), "{expected}: {message}");
        };
        rejected(&|config| config.runs = 1, "at least 2");
        rejected(&|config| config.threads.clear(), "thread count");
        rejected(
            &|config| config.threads = vec![Threads::Count(0)],
            "at least 1",
        );
        rejected(
            &|config| config.threads = vec![Threads::Count(2), Threads::Count(2)],
            "distinct",
        );
        rejected(
            &|config| config.threads = vec![Threads::Default, Threads::Default],
            "distinct",
        );
        rejected(&|config| config.corpora.clear(), "at least one corpus");
        rejected(
            &|config| config.candidates.clear(),
            "at least one --candidate",
        );
    }

    fn bench(args: &[OsString]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bench"))
            .args(args)
            .current_dir(workspace())
            .output()
            .expect("run bench")
    }

    fn os_args(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn ab_cli_refuses_missing_arguments_and_unpinned_corpora() {
        let dir = TestDir::new("cli");
        let true_binary = first_existing(&["/bin/true", "/usr/bin/true"]);
        let root = dir.path().join("corpora");
        fs::create_dir_all(&root).unwrap();
        let mut base = os_args(&["--ab", "--baseline"]);
        base.push(true_binary.clone().into());
        base.push("--candidate".into());
        base.push(true_binary.into());
        base.extend(os_args(&["--threads", "1", "--corpora-root"]));
        base.push(root.clone().into());

        let output = bench(&os_args(&["--ab"]));
        assert_eq!(output.status.code(), Some(1));
        assert!(
            stderr(&output).contains("--ab needs --baseline"),
            "{}",
            stderr(&output)
        );

        let output = bench(&base);
        assert!(
            stderr(&output).contains("--ab needs --corpora"),
            "{}",
            stderr(&output)
        );

        let mut args = base.clone();
        args.extend(os_args(&["--corpora", "no-such-corpus"]));
        let output = bench(&args);
        assert!(
            stderr(&output).contains("unknown corpus"),
            "{}",
            stderr(&output)
        );

        let mut bad_op = args.clone();
        bad_op.extend(os_args(&["--op", "sync-edit:0"]));
        let output = bench(&bad_op);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            stderr(&output).contains("at least one file"),
            "{}",
            stderr(&output)
        );

        let mut args = base;
        args.extend(os_args(&["--corpora", "fd-small"]));
        let output = bench(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            stderr(&output).contains("is not fetched"),
            "{}",
            stderr(&output)
        );

        // A checkout at another commit than the pin is refused before any run.
        let checkout = root.join("fd-small");
        fs::create_dir_all(checkout.join("src")).unwrap();
        for git_args in [
            &["init", "--quiet"][..],
            &[
                "-c",
                "user.name=CodeGraph Test",
                "-c",
                "user.email=codegraph@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "not the pin",
            ][..],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(git_args)
                .status()
                .expect("run git");
            assert!(status.success(), "git {git_args:?}");
        }
        let output = bench(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            stderr(&output)
                .contains("but the registry pins 25461e5ce13dc12ff2a75993285a87e99b33db2d"),
            "{}",
            stderr(&output)
        );
    }

    /// A stand-in like [`copying_codegraph`] that also prints `phases`, each
    /// `name:microseconds`, as phase-timing lines on stderr.
    fn timing_codegraph(dir: &Path, name: &str, db: &Path, log: &Path, phases: &str) -> PathBuf {
        script(
            dir,
            name,
            &format!(
                "echo \"${{0##*/}} ${{RAYON_NUM_THREADS:-unset}} ${{{phase_env}:-unset}}\" >> {log}\n\
                 for phase in {phases}; do\n\
                 printf 'codegraph-phase\\tinit\\t%s\\t%s\\n' \"${{phase%%:*}}\" \"${{phase#*:}}\" >&2\n\
                 done\n\
                 mkdir \"$project/.codegraph\" && cp {db} \"$project/.codegraph/codegraph.db\"",
                phase_env = "CODEGRAPH_PHASE_TIMINGS",
                log = quoted(log),
                db = quoted(db),
            ),
        )
    }

    #[test]
    fn ab_reads_each_runs_phase_timings_and_cpu_time() {
        let dir = TestDir::new("phases");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let golden = golden_db("go");
        let baseline = timing_codegraph(
            dir.path(),
            "baseline",
            &golden,
            &log,
            "scan:2000 reference_resolution:8000",
        );
        let candidate = timing_codegraph(
            dir.path(),
            "candidate",
            &golden,
            &log,
            "scan:2000 reference_resolution:6000",
        );

        let report =
            run_ab(&config(baseline, candidate, &corpus, vec![2], 3)).expect("the A/B succeeds");

        assert_eq!(report.schema_version, 2);
        let cell = &report.corpora[0].cells[0];
        for run in cell.baseline.runs.iter().chain(&cell.candidate.runs) {
            assert_eq!(run.phase_ms.len(), 2, "{run:?}");
            assert!(run.cpu_ms.is_some(), "wait4 reports CPU time");
            assert!(run.load.is_none() && run.void.is_none(), "the guard is off");
        }
        assert_eq!(cell.baseline.phase_ms["reference_resolution"].median, 8.0);
        assert_eq!(cell.candidate.phase_ms["reference_resolution"].median, 6.0);
        assert_eq!(cell.candidates[0].phase_ratio["reference_resolution"], 0.75);
        assert_eq!(cell.candidates[0].phase_ratio["scan"], 1.0);
        // Every child was asked for its timings.
        let lines = fs::read_to_string(&log).unwrap();
        assert!(lines.lines().all(|line| line.ends_with(" 2 1")), "{lines}");
        let markdown = render_ab_markdown(&report);
        assert!(
            markdown.contains(
                "| tiny | 2 | candidate | reference_resolution | 8.0 (0.0) | 6.0 (0.0) | 0.750 |"
            ),
            "{markdown}"
        );
    }

    #[test]
    fn ab_stops_when_a_binary_reports_different_phases_in_one_cell() {
        let dir = TestDir::new("phase-set");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let counter = dir.path().join("counter");
        let baseline = copying_codegraph(dir.path(), "baseline", &golden_db("go"), &log);
        // Every second run reports an extra phase.
        let candidate = script(
            dir.path(),
            "uneven",
            &format!(
                "n=$(cat {counter} 2>/dev/null || echo 0)\nn=$((n + 1))\necho \"$n\" > {counter}\n\
                 printf 'codegraph-phase\\tinit\\tscan\\t10\\n' >&2\n\
                 [ $((n % 2)) -eq 0 ] && printf 'codegraph-phase\\tinit\\textra\\t10\\n' >&2\n\
                 mkdir \"$project/.codegraph\" && cp {db} \"$project/.codegraph/codegraph.db\"",
                counter = quoted(&counter),
                db = quoted(&golden_db("go")),
            ),
        );

        let message = failure(&config(baseline, candidate, &corpus, vec![1], 3));
        assert!(message.contains("candidate runs on tiny"), "{message}");
        assert!(message.contains("report different phases"), "{message}");
        assert!(message.contains("extra"), "{message}");
    }

    #[test]
    fn ab_rotates_several_candidates_and_compares_each_with_the_baseline() {
        let dir = TestDir::new("rotation");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let golden = golden_db("go");
        let mut config = config(
            copying_codegraph(dir.path(), "baseline", &golden, &log),
            copying_codegraph(dir.path(), "first", &golden, &log),
            &corpus,
            vec![1],
            3,
        );
        let tampered = dir.path().join("tampered.db");
        tampered_copy("typescript", &tampered);
        config
            .candidates
            .push(copying_codegraph(dir.path(), "second", &tampered, &log));

        let report = run_ab(&config).expect("the A/B succeeds");

        let order: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| line.split(' ').next().unwrap().to_string())
            .collect();
        assert_eq!(
            order,
            [
                "baseline", "first", "second", "first", "second", "baseline", "second", "baseline",
                "first"
            ]
        );
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.candidate.sha256, report.candidates[0].sha256);
        let corpus_report = &report.corpora[0];
        assert_eq!(corpus_report.graphs.len(), 2);
        assert!(corpus_report.graphs[0].identical);
        assert!(!corpus_report.graphs[1].identical);
        let cell = &corpus_report.cells[0];
        assert_eq!(cell.candidates.len(), 2);
        for candidate in &cell.candidates {
            assert_eq!(candidate.series.wall_ms.samples, 2);
        }
        assert!(
            report.settings.order.starts_with("rotated"),
            "{}",
            report.settings.order
        );
        let markdown = render_ab_markdown(&report);
        assert!(markdown.contains("| Candidate 2 sha256 |"), "{markdown}");
        assert!(
            markdown.contains("| tiny | 1 | candidate 2 |"),
            "{markdown}"
        );
    }

    #[test]
    fn ab_default_thread_cell_leaves_rayon_unset() {
        let dir = TestDir::new("default-threads");
        let corpus = tiny_corpus(dir.path());
        let log = dir.path().join("runs.log");
        let golden = golden_db("go");
        let mut config = config(
            copying_codegraph(dir.path(), "baseline", &golden, &log),
            copying_codegraph(dir.path(), "candidate", &golden, &log),
            &corpus,
            vec![],
            2,
        );
        config.threads = vec![Threads::Default, Threads::Count(1)];

        let report = run_ab(&config).expect("the A/B succeeds");

        let lines = fs::read_to_string(&log).unwrap();
        let threads: Vec<&str> = lines
            .lines()
            .map(|line| line.split(' ').nth(1).unwrap())
            .collect();
        assert_eq!(
            threads,
            ["unset", "unset", "unset", "unset", "1", "1", "1", "1"]
        );
        let cells = &report.corpora[0].cells;
        assert!(cells[0].default_threads && !cells[1].default_threads);
        assert_eq!(cells[0].threads, codegraph_bench::ab::logical_cpus());
        assert_eq!(report.settings.thread_settings, ["default", "1"]);
        assert!(
            render_ab_markdown(&report)
                .contains(&format!("| tiny | default ({}) |", cells[0].threads))
        );
    }

    /// A stand-in for `/proc`: an idle host whose CPU time jumps during the
    /// run windows listed in `loaded` (1-based, every timed run opens one).
    struct ScriptedProbe {
        busy: u64,
        idle: u64,
        own_calls: usize,
        loaded: std::collections::BTreeSet<usize>,
    }

    impl ScriptedProbe {
        fn loading(windows: impl IntoIterator<Item = usize>) -> Self {
            Self {
                busy: 0,
                idle: 0,
                own_calls: 0,
                loaded: windows.into_iter().collect(),
            }
        }
    }

    impl LoadProbe for ScriptedProbe {
        fn cpu_times(&mut self) -> anyhow::Result<CpuTimes> {
            let times = CpuTimes {
                busy: self.busy,
                idle: self.idle,
            };
            self.idle += 10_000;
            Ok(times)
        }
        fn ticks_per_second(&self) -> f64 {
            100.0
        }
        fn load1(&mut self) -> anyhow::Result<f64> {
            Ok(0.0)
        }
        fn build_processes(&mut self) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }
        /// Called when a run's window opens and when it closes; an opening
        /// of a loaded window adds 1000 busy CPU seconds.
        fn own_cpu(&mut self) -> Duration {
            self.own_calls += 1;
            if self.own_calls % 2 == 1 && self.loaded.contains(&(self.own_calls / 2 + 1)) {
                self.busy += 100_000;
            }
            Duration::ZERO
        }
        fn pause(&mut self, _: Duration) {}
    }

    fn guarded_config(dir: &Path, log: &Path, runs: usize) -> AbConfig {
        let corpus = tiny_corpus(dir);
        let golden = golden_db("go");
        let mut config = config(
            copying_codegraph(dir, "baseline", &golden, log),
            copying_codegraph(dir, "candidate", &golden, log),
            &corpus,
            vec![1],
            runs,
        );
        config.load_guard = Some(LoadGuard {
            start_checks: 0,
            ..LoadGuard::standard(1)
        });
        config
    }

    /// The candidate's first measured run (the third window: runs rotate, so
    /// run 2 starts with the candidate) shares its CPUs: it is kept as void
    /// and run again, and the statistics count the valid runs only.
    #[test]
    fn ab_voids_a_run_that_shared_its_cpus_and_runs_it_again() {
        let dir = TestDir::new("void");
        let log = dir.path().join("runs.log");
        let config = guarded_config(dir.path(), &log, 3);
        let mut probe = ScriptedProbe::loading([3]);

        let report = run_ab_with_probe(&config, Some(&mut probe)).expect("the A/B succeeds");

        let cell = &report.corpora[0].cells[0];
        let voids: Vec<usize> = cell
            .candidate
            .runs
            .iter()
            .filter(|run| run.void.is_some())
            .map(|run| run.run)
            .collect();
        assert_eq!(voids, [2]);
        assert_eq!(cell.candidate.runs.len(), 4);
        assert_eq!(cell.candidate.wall_ms.samples, 2);
        assert_eq!(cell.baseline.runs.len(), 3);
        let void = &cell.candidate.runs[1];
        let load = void.load.expect("the guard records every run");
        assert!(load.foreign_cpus > 0.5, "{load:?}");
        assert!(void.void.as_deref().unwrap().contains("over the 0.5 limit"));
        let fine = cell.baseline.runs[1].load.unwrap();
        assert_eq!(fine.foreign_cpus, 0.0);
        assert_eq!(fine.idle_cpus, 1.0);
        assert!(report.session_start.as_ref().unwrap().checks.is_empty());
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 7);
        // The kept runs' foreign load is summarized, every run's is listed.
        let kept = cell
            .candidate
            .foreign_cpus
            .expect("the guard measured every run");
        assert_eq!((kept.median, kept.samples), (0.0, 2));
        let markdown = render_ab_markdown(&report);
        assert!(markdown.contains("| 0/1 |"), "{markdown}");
        assert!(
            markdown.contains("Foreign CPUs the load guard measured"),
            "{markdown}"
        );
        let void_value = format!("{:.2}", load.foreign_cpus);
        assert!(
            markdown.contains(&format!(
                "| tiny | 1 | candidate | 0.00 | 0.00, 0.00 | {void_value} |"
            )),
            "{markdown}"
        );
        assert!(
            markdown.contains("| tiny | 1 | baseline | 0.00 | 0.00, 0.00 | — |"),
            "{markdown}"
        );
    }

    #[test]
    fn ab_writes_no_report_when_a_cell_cannot_collect_its_runs() {
        let dir = TestDir::new("void-cap");
        let log = dir.path().join("runs.log");
        let config = guarded_config(dir.path(), &log, 2);
        // Every run after the two warm-ups shares its CPUs.
        let mut probe = ScriptedProbe::loading(3..100);

        let error = run_ab_with_probe(&config, Some(&mut probe))
            .expect_err("a cell short of valid runs fails the report");
        let message = format!("{error:#}");
        assert!(message.contains("voided too many runs"), "{message}");
        assert!(message.contains("candidate run 2/2 on tiny"), "{message}");
        // The two warm-ups, then three void candidate attempts: four
        // candidate attempts in all, its cap at two runs.
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 2 + 3);
    }

    /// A script that is the whole stand-in: `init` installs `db`; `sync`
    /// logs the last line of every file in `edited`, prints phase timings
    /// and the summary line saying it re-indexed `reindexed` files, and
    /// installs `sync_db` when one is given.
    fn syncing_codegraph(
        dir: &Path,
        name: &str,
        db: &Path,
        sync_db: Option<&Path>,
        log: &Path,
        edited: &[&str],
        reindexed: usize,
    ) -> PathBuf {
        let path = dir.join(name);
        let install = sync_db.map_or_else(String::new, |sync_db| {
            format!(
                "cp {} \"$project/.codegraph/codegraph.db\"\n",
                quoted(sync_db)
            )
        });
        let tails: String = edited
            .iter()
            .map(|file| {
                format!(
                    "printf '%s|' \"$(tail -n 1 \"$project/{file}\")\" >> {}\n",
                    quoted(log)
                )
            })
            .collect();
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nproject=$2\ncase \"$1\" in\n\
                 init)\n\
                 printf 'codegraph-phase\\tinit\\tscan\\t100\\n' >&2\n\
                 mkdir \"$project/.codegraph\" && cp {db} \"$project/.codegraph/codegraph.db\"\n\
                 ;;\n\
                 sync)\n\
                 printf '%s ' \"${{0##*/}}\" >> {log}\n{tails}echo >> {log}\n\
                 printf 'codegraph-phase\\tsync\\tsync\\t1500\\ncodegraph-phase\\tsync\\tsync.files\\t700\\n' >&2\n\
                 {install}\
                 echo 'Synced: {reindexed} reindexed, 5 skipped (unchanged), 0 removed in 1ms'\n\
                 ;;\n\
                 *) exit 64 ;;\nesac\n",
                db = quoted(db),
                log = quoted(log),
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        wait_until_executable(&path);
        path
    }

    fn go_corpus() -> PathBuf {
        workspace().join("crates/codegraph-bench/fixtures/go")
    }

    fn sync_config(baseline: PathBuf, candidate: PathBuf, op: AbOp) -> AbConfig {
        let mut config = config(baseline, candidate, &go_corpus(), vec![1], 3);
        config.corpora[0].name = "go".to_string();
        config.op = op;
        config
    }

    /// The first two indexed files, in path order, get one comment line
    /// before every sync: the warm-up, the timed runs (alternating), and the
    /// final state each binary's graph is checked in.
    #[test]
    fn ab_times_syncs_after_comment_edits_and_checks_the_final_graph() {
        let dir = TestDir::new("sync-edit");
        let log = dir.path().join("syncs.log");
        let golden = golden_db("go");
        let edited = ["api.pb.go", "config.go"];
        let baseline = syncing_codegraph(dir.path(), "baseline", &golden, None, &log, &edited, 2);
        let candidate = syncing_codegraph(dir.path(), "candidate", &golden, None, &log, &edited, 2);

        let report = run_ab(&sync_config(
            baseline,
            candidate,
            AbOp::SyncEdit { files: 2 },
        ))
        .expect("the A/B succeeds");

        assert_eq!(report.settings.op, "sync-edit:2");
        let corpus_report = &report.corpora[0];
        assert_eq!(corpus_report.edited_files, edited);
        assert!(corpus_report.graph.identical);
        let expected = load_golden(&golden_dir("go")).unwrap();
        assert_eq!(
            corpus_report.graph.baseline.rows.files,
            expected.files.len() as u64
        );
        let cell = &corpus_report.cells[0];
        for series in [&cell.baseline, &cell.candidate] {
            assert_eq!(series.runs.len(), 3);
            assert_eq!(series.wall_ms.samples, 2);
            assert_eq!(series.phase_ms["sync"].median, 1.5);
            assert_eq!(series.phase_ms["sync.files"].median, 0.7);
            assert!(series.runs.iter().all(|run| run.canonical_hash.is_none()));
            assert!(series.rows_per_second.is_none());
        }
        assert_eq!(cell.budget.init_wall_ok, None);
        assert_eq!(
            cell.budget.sync_wall_ok, None,
            "a two-file sync has no wall line"
        );
        let lines: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        let line = |binary: &str, label: &str| {
            format!(
                "{binary} // codegraph-bench sync-edit {label}|// codegraph-bench sync-edit {label}|"
            )
        };
        assert_eq!(
            lines,
            [
                line("baseline", "warm-up"),
                line("candidate", "warm-up"),
                line("baseline", "1"),
                line("candidate", "1"),
                line("candidate", "2"),
                line("baseline", "2"),
                line("baseline", "3"),
                line("candidate", "3"),
                line("baseline", "final"),
                line("candidate", "final"),
            ]
        );
        // The corpus itself is never edited.
        let source = fs::read_to_string(go_corpus().join("config.go")).unwrap();
        assert!(!source.contains("codegraph-bench"));
    }

    #[test]
    fn ab_times_a_single_named_file_against_the_sync_budget() {
        let dir = TestDir::new("sync-file");
        let log = dir.path().join("syncs.log");
        let golden = golden_db("go");
        let edited = ["payroll.go"];
        let baseline = syncing_codegraph(dir.path(), "baseline", &golden, None, &log, &edited, 1);
        let candidate = syncing_codegraph(dir.path(), "candidate", &golden, None, &log, &edited, 1);
        let op = AbOp::SyncEditFile {
            path: "payroll.go".to_string(),
        };

        let report = run_ab(&sync_config(baseline, candidate, op)).expect("the A/B succeeds");

        assert_eq!(report.corpora[0].edited_files, edited);
        assert!(report.corpora[0].cells[0].budget.sync_wall_ok.is_some());
    }

    #[test]
    fn ab_stops_when_a_sync_does_not_converge_with_a_fresh_init() {
        let dir = TestDir::new("sync-diverge");
        let log = dir.path().join("syncs.log");
        let golden = golden_db("go");
        let tampered = dir.path().join("tampered.db");
        tampered_copy("go", &tampered);
        let edited = ["api.pb.go"];
        let baseline = syncing_codegraph(dir.path(), "baseline", &golden, None, &log, &edited, 1);
        let candidate = syncing_codegraph(
            dir.path(),
            "candidate",
            &golden,
            Some(&tampered),
            &log,
            &edited,
            1,
        );

        let message = failure(&sync_config(
            baseline,
            candidate,
            AbOp::SyncEdit { files: 1 },
        ));
        assert!(
            message.contains("finishing the candidate sync runs on go"),
            "{message}"
        );
        assert!(
            message.contains("does not converge with a fresh init"),
            "{message}"
        );
        assert!(message.contains("nodes, edges"), "{message}");
    }

    #[test]
    fn ab_stops_when_a_sync_reindexes_other_than_the_edited_files() {
        let dir = TestDir::new("sync-count");
        let log = dir.path().join("syncs.log");
        let golden = golden_db("go");
        let edited = ["api.pb.go", "config.go"];
        let baseline = syncing_codegraph(dir.path(), "baseline", &golden, None, &log, &edited, 1);
        let candidate = syncing_codegraph(dir.path(), "candidate", &golden, None, &log, &edited, 2);

        let message = failure(&sync_config(
            baseline,
            candidate,
            AbOp::SyncEdit { files: 2 },
        ));
        assert!(
            message.contains("preparing the baseline sync runs on go"),
            "{message}"
        );
        assert!(
            message.contains("re-indexed 1 files after 2 were edited"),
            "{message}"
        );

        let op = AbOp::SyncEditFile {
            path: "missing.go".to_string(),
        };
        let golden_only = syncing_codegraph(dir.path(), "plain", &golden, None, &log, &[], 1);
        let message = failure(&sync_config(golden_only.clone(), golden_only, op));
        assert!(
            message.contains("missing.go is not an indexed file"),
            "{message}"
        );
    }
}
