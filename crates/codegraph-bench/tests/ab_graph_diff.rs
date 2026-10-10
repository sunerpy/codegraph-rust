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

    use codegraph_bench::ab::{AbConfig, AbCorpus, AbReport, render_ab_markdown, run_ab};
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
            candidate,
            corpora: vec![AbCorpus {
                name: "tiny".to_string(),
                commit: None,
                tag: None,
                source: corpus.to_path_buf(),
            }],
            threads,
            runs,
            run_timeout: Duration::from_secs(60),
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
                assert!(
                    series
                        .runs
                        .iter()
                        .all(|run| run.canonical_hash == corpus_report.graph.baseline.hash)
                );
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
        rejected(&|config| config.threads = vec![0], "at least 1");
        rejected(&|config| config.threads = vec![2, 2], "distinct");
        rejected(&|config| config.corpora.clear(), "at least one corpus");
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
}
