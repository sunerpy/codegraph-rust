//! `scripts/od9-verdict.py`, the OD-9 adoption rule over `bench --ab`
//! reports: each rule at its boundary (the 5% improvement, the 2×MAD margin,
//! the 2% regression ratio, the noise floor), the evidence it cannot decide
//! without, and a report the harness itself wrote.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-bench lives under crates/")
        .to_path_buf()
}

fn script() -> PathBuf {
    workspace().join("crates/codegraph-bench/scripts/od9-verdict.py")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-od9-verdict-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Five measured values with median `center` and unscaled MAD `mad`.
fn spread(center: f64, mad: f64) -> Vec<f64> {
    vec![
        center - 2.0 * mad,
        center - mad,
        center,
        center + mad,
        center + 2.0 * mad,
    ]
}

/// One binary's measured runs in a cell: the target phase, another phase, a
/// phase shorter than the noise floor, the wall time and the peak RSS.
#[derive(Clone)]
struct Arm {
    target: Vec<f64>,
    other: Vec<f64>,
    short: Vec<f64>,
    wall: Vec<f64>,
    rss: Vec<f64>,
}

impl Arm {
    fn steady() -> Self {
        Self {
            target: spread(100.0, 2.0),
            other: spread(50.0, 0.25),
            short: spread(0.4, 0.01),
            wall: spread(200.0, 2.0),
            rss: vec![1000.0; 5],
        }
    }

    /// The runs as the report holds them: a warm-up far off every median,
    /// which no statistic may count, then the five measured runs.
    fn runs(&self) -> Value {
        let mut runs = vec![json!({
            "run": 1, "warmup": true, "wall_ms": 9999.0, "peak_rss_kb": 999_999,
            "phase_ms": { "target": 9999.0, "other": 9999.0, "short": 9999.0 }
        })];
        for index in 0..5 {
            runs.push(json!({
                "run": index + 2,
                "warmup": false,
                "wall_ms": self.wall[index],
                "peak_rss_kb": self.rss[index],
                "phase_ms": {
                    "target": self.target[index],
                    "other": self.other[index],
                    "short": self.short[index],
                },
            }));
        }
        Value::Array(runs)
    }
}

fn cell(threads: usize, base: &Arm, candidate: &Arm) -> Value {
    json!({
        "threads": threads,
        "default_threads": false,
        "baseline": { "runs": base.runs() },
        "candidates": [ { "series": { "runs": candidate.runs() } } ],
    })
}

fn report(cells: Vec<Value>, identical: bool) -> Value {
    json!({
        "schema_version": 2,
        "kind": "codegraph-ab",
        "candidates": [ { "sha256": "c" } ],
        "corpora": [ {
            "name": "tiny",
            "graphs": [ {
                "identical": identical,
                "differing_surfaces": if identical { json!([]) } else { json!(["edges"]) },
            } ],
            "cells": cells,
        } ],
    })
}

/// A report whose 32- and 4-thread cells hold `base` against `candidate`.
fn both_cells(base: &Arm, candidate: &Arm) -> Value {
    report(
        vec![cell(32, base, candidate), cell(4, base, candidate)],
        true,
    )
}

struct Verdict {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn verdict(dir: &TestDir, reports: &[Value], extra: &[&str]) -> Verdict {
    let mut paths = Vec::new();
    for (index, report) in reports.iter().enumerate() {
        let path = dir.path.join(format!("report-{index}.json"));
        fs::write(&path, serde_json::to_string(report).unwrap()).unwrap();
        paths.push(path);
    }
    let output = Command::new("python3")
        .arg("-I")
        .arg(script())
        .args(["--target", "target", "--noise-floor-ms", "1.0"])
        .args(extra)
        .args(&paths)
        .output()
        .expect("python3 runs the verdict script");
    Verdict {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

const RELEASE: &[&str] = &["--profile", "release", "--goldens-ok"];

#[track_caller]
fn assert_verdict(result: &Verdict, code: i32, verdict: &str) {
    assert_eq!(
        result.code,
        Some(code),
        "{}\n{}",
        result.stdout,
        result.stderr
    );
    assert!(
        result.stdout.contains(&format!("VERDICT: {verdict}")),
        "{}",
        result.stdout
    );
}

fn improved(target: Vec<f64>) -> Arm {
    Arm {
        target,
        ..Arm::steady()
    }
}

#[test]
fn a_five_percent_improvement_past_twice_the_mad_is_adopted() {
    let dir = TestDir::new("adopted");
    let result = verdict(
        &dir,
        &[both_cells(&Arm::steady(), &improved(spread(95.0, 2.0)))],
        RELEASE,
    );
    assert_verdict(&result, 0, "ADOPTED");
    assert!(
        result.stdout.contains("improvement 5.00%"),
        "{}",
        result.stdout
    );
}

#[test]
fn an_improvement_just_under_five_percent_is_rejected() {
    let dir = TestDir::new("under-5");
    let result = verdict(
        &dir,
        &[both_cells(&Arm::steady(), &improved(spread(95.01, 2.0)))],
        RELEASE,
    );
    assert_verdict(&result, 1, "REJECTED_BY_PROFILE");
    assert!(
        result.stdout.contains("failed: (b) tiny/32"),
        "{}",
        result.stdout
    );
}

/// The improvement must exceed twice the larger MAD: equal to it is not
/// enough.
#[test]
fn an_improvement_equal_to_twice_the_mad_is_rejected_and_just_past_it_adopted() {
    let dir = TestDir::new("2mad");
    let base = Arm {
        target: spread(100.0, 2.5),
        ..Arm::steady()
    };
    let at = verdict(
        &dir,
        &[both_cells(&base, &improved(spread(95.0, 2.5)))],
        RELEASE,
    );
    assert_verdict(&at, 1, "REJECTED_BY_PROFILE");
    assert!(
        at.stdout.contains("delta 5.000 vs 2*MAD 5.000"),
        "{}",
        at.stdout
    );
    let past = verdict(
        &dir,
        &[both_cells(&base, &improved(spread(94.9, 2.5)))],
        RELEASE,
    );
    assert_verdict(&past, 0, "ADOPTED");
}

fn with_other(other: Vec<f64>) -> Arm {
    Arm {
        other,
        ..improved(spread(95.0, 2.0))
    }
}

/// Another phase slower by exactly 2% is no regression; by more, past twice
/// the MAD, it is; past 2% but within twice the MAD, it is not.
#[test]
fn another_phase_regresses_only_past_two_percent_and_twice_the_mad() {
    let dir = TestDir::new("2pct");
    let base = Arm::steady();
    let at = verdict(
        &dir,
        &[both_cells(&base, &with_other(spread(51.0, 0.25)))],
        RELEASE,
    );
    assert_verdict(&at, 0, "ADOPTED");
    let past = verdict(
        &dir,
        &[both_cells(&base, &with_other(spread(51.05, 0.25)))],
        RELEASE,
    );
    assert_verdict(&past, 1, "REJECTED_BY_PROFILE");
    assert!(
        past.stdout.contains("(c) tiny/32: other regressed"),
        "{}",
        past.stdout
    );
    let noisy = verdict(
        &dir,
        &[both_cells(&base, &with_other(spread(53.0, 2.0)))],
        RELEASE,
    );
    assert_verdict(&noisy, 0, "ADOPTED");
}

/// A phase shorter than the noise floor is judged by its absolute increase
/// alone: doubled but within the floor is no regression.
#[test]
fn a_phase_under_the_noise_floor_is_judged_by_its_absolute_increase() {
    let dir = TestDir::new("floor");
    let base = Arm::steady();
    let doubled = Arm {
        short: spread(0.8, 0.01),
        ..improved(spread(95.0, 2.0))
    };
    assert_verdict(
        &verdict(&dir, &[both_cells(&base, &doubled)], RELEASE),
        0,
        "ADOPTED",
    );
    let past_floor = Arm {
        short: spread(1.5, 0.01),
        ..improved(spread(95.0, 2.0))
    };
    let result = verdict(&dir, &[both_cells(&base, &past_floor)], RELEASE);
    assert_verdict(&result, 1, "REJECTED_BY_PROFILE");
    assert!(
        result.stdout.contains("short regressed") && result.stdout.contains("noise floor"),
        "{}",
        result.stdout
    );
}

#[test]
fn the_wall_time_and_peak_rss_must_not_regress_either() {
    let dir = TestDir::new("wall-rss");
    let base = Arm::steady();
    let slower = Arm {
        wall: spread(210.0, 2.0),
        ..improved(spread(95.0, 2.0))
    };
    let result = verdict(&dir, &[both_cells(&base, &slower)], RELEASE);
    assert_verdict(&result, 1, "REJECTED_BY_PROFILE");
    assert!(
        result.stdout.contains("wall regressed"),
        "{}",
        result.stdout
    );
    let heavier = Arm {
        rss: vec![1100.0; 5],
        ..improved(spread(95.0, 2.0))
    };
    let result = verdict(&dir, &[both_cells(&base, &heavier)], RELEASE);
    assert_verdict(&result, 1, "REJECTED_BY_PROFILE");
    assert!(
        result.stdout.contains("peak RSS (KiB) regressed"),
        "{}",
        result.stdout
    );
    // The wall time as the target is not also checked as another timing.
    let faster = Arm {
        wall: spread(180.0, 2.0),
        ..Arm::steady()
    };
    let result = Command::new("python3")
        .arg("-I")
        .arg(script())
        .args(["--target", "wall", "--noise-floor-ms", "1.0"])
        .args(RELEASE)
        .arg({
            let path = dir.path.join("wall.json");
            fs::write(&path, both_cells(&base, &faster).to_string()).unwrap();
            path
        })
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[test]
fn a_changed_graph_is_rejected() {
    let dir = TestDir::new("graph");
    let candidate = improved(spread(95.0, 2.0));
    let changed = report(
        vec![
            cell(32, &Arm::steady(), &candidate),
            cell(4, &Arm::steady(), &candidate),
        ],
        false,
    );
    let result = verdict(&dir, &[changed], RELEASE);
    assert_verdict(&result, 1, "REJECTED_BY_PROFILE");
    assert!(
        result.stdout.contains("graph differs in edges"),
        "{}",
        result.stdout
    );
}

/// Without the golden attestation, a missing cell, or short of five measured
/// runs a cell, there is no verdict; a fast build gives attribution only.
#[test]
fn missing_evidence_gives_no_verdict() {
    let dir = TestDir::new("incomplete");
    let base = Arm::steady();
    let candidate = improved(spread(95.0, 2.0));
    let full = both_cells(&base, &candidate);
    assert_verdict(
        &verdict(&dir, std::slice::from_ref(&full), &["--profile", "release"]),
        3,
        "INCOMPLETE",
    );
    let one_cell = report(vec![cell(32, &base, &candidate)], true);
    let result = verdict(&dir, std::slice::from_ref(&one_cell), RELEASE);
    assert_verdict(&result, 3, "INCOMPLETE");
    assert!(
        result.stdout.contains("cell tiny/4 is in no report"),
        "{}",
        result.stdout
    );
    let mut short = full.clone();
    let runs = short["corpora"][0]["cells"][0]["baseline"]["runs"]
        .as_array_mut()
        .unwrap();
    runs[3]["void"] = json!("0.90 foreign CPUs, over the 0.5 limit");
    let result = verdict(&dir, &[short], RELEASE);
    assert_verdict(&result, 3, "INCOMPLETE");
    assert!(
        result.stdout.contains("4 and 5 measured runs"),
        "{}",
        result.stdout
    );
    let fast = verdict(&dir, &[full], &["--profile", "fast", "--goldens-ok"]);
    assert_verdict(&fast, 3, "ADOPTED (attribution only");
}

/// The 32- and 4-thread cells may come from two reports, each cell from one.
#[test]
fn cells_combine_across_reports_but_never_repeat() {
    let dir = TestDir::new("reports");
    let base = Arm::steady();
    let candidate = improved(spread(95.0, 2.0));
    let local = report(vec![cell(32, &base, &candidate)], true);
    let runner = report(vec![cell(4, &base, &candidate)], true);
    assert_verdict(
        &verdict(&dir, &[local.clone(), runner], RELEASE),
        0,
        "ADOPTED",
    );
    let result = verdict(&dir, &[local.clone(), local], RELEASE);
    assert_eq!(result.code, Some(2), "{}", result.stdout);
    assert!(result.stderr.contains("is in both"), "{}", result.stderr);
    let mut old = both_cells(&base, &candidate);
    old["schema_version"] = json!(1);
    let result = verdict(&dir, &[old], RELEASE);
    assert_eq!(result.code, Some(2));
    assert!(result.stderr.contains("schema-2"), "{}", result.stderr);
}

/// A report the harness wrote, with its own field names, reads: the target
/// comes from the stand-ins' phase lines.
#[test]
fn the_verdict_reads_a_report_the_harness_wrote() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use codegraph_bench::ab::{AbConfig, AbCorpus, AbOp, Threads, run_ab, write_ab_report};

    let dir = TestDir::new("harness");
    let corpus = dir.path.join("corpus");
    fs::create_dir_all(&corpus).unwrap();
    fs::write(corpus.join("main.go"), b"package main\n").unwrap();
    let golden = workspace().join("reference/golden/go/colby.db");
    let stand_in = |name: &str, micros: u32| {
        let path = dir.path.join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n[ \"$1\" = init ] || exit 64\n\
                 printf 'codegraph-phase\\tinit\\treference_resolution\\t{micros}\\n' >&2\n\
                 mkdir \"$2/.codegraph\" && cp '{}' \"$2/.codegraph/codegraph.db\"\n",
                golden.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        // A freshly written script may still be open in a sibling test's
        // fork (ETXTBSY): run it once before the harness relies on it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match Command::new(&path).output() {
                Ok(_) => break,
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("cannot run {}: {error}", path.display()),
            }
        }
        path
    };
    let config = AbConfig {
        baseline: stand_in("baseline", 8000),
        candidates: vec![stand_in("candidate", 6000)],
        corpora: vec![AbCorpus {
            name: "tiny".to_string(),
            commit: None,
            tag: None,
            source: corpus,
        }],
        threads: vec![Threads::Count(2)],
        runs: 6,
        run_timeout: Duration::from_secs(60),
        op: AbOp::Init,
        load_guard: None,
        workspace_root: None,
        argv: Vec::new(),
    };
    let report = run_ab(&config).expect("the A/B succeeds");
    let path = dir.path.join("harness.json");
    write_ab_report(&path, &report).unwrap();
    let output = Command::new("python3")
        .arg("-I")
        .arg(script())
        .args([
            "--target",
            "reference_resolution",
            "--noise-floor-ms",
            "1.0",
            "--threads",
            "2",
            "--json",
        ])
        .args(RELEASE)
        .arg(&path)
        .output()
        .expect("python3 runs the verdict script");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The stand-ins' wall times and RSS are whatever the host gives, so the
    // verdict may go either way; the target is the stand-ins' 8 -> 6 ms.
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_str(&stdout).expect("--json prints JSON");
    let passed = result["passed"].as_array().unwrap();
    assert!(
        passed.iter().any(|line| line
            .as_str()
            .unwrap()
            .starts_with("(b) tiny/2: target 8.000 -> 6.000 ms")),
        "{stdout}"
    );
}
