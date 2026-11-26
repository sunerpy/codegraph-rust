//! `CODEGRAPH_PHASE_TIMINGS=1` (W9-00a): `init` and `sync` print one stderr
//! line per phase, `codegraph-phase\t<command>\t<phase>\t<microseconds>`,
//! each phase once, and print none with the variable unset.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const TAG: &str = "codegraph-phase";
/// The tag as the first field of a line, so a path holding the word is no match.
const TAG_FIELD: &str = "codegraph-phase\t";

/// The phases every `init` reports, the synthesis passes aside: each pass
/// whose gate the project opens adds `synthesis.<pass>`.
const INIT_PHASES: &[&str] = &[
    "scan",
    "parse_write",
    "parse_write.store_busy",
    "node_write",
    "edge_write",
    "reference_write",
    "framework_extract",
    "framework_finalize",
    "reference_resolution",
    "reference_resolution.setup",
    "reference_resolution.read",
    "reference_resolution.snapshot",
    "reference_resolution.resolve",
    "reference_resolution.create_edges",
    "reference_resolution.insert_edges",
    "reference_resolution.move_refs",
    "reference_resolution.wal_fold",
    "reference_resolution.conformance",
    "reference_resolution.this_members",
    "synthesis",
    "synthesis.read",
    "synthesis.merge",
    "synthesis.replace",
    "synthesis.inputs",
    "publish",
];

/// The phases every incremental `sync` reports; a sync that runs the
/// synthesis stage adds the stage's `synthesis.*` parts.
const SYNC_PHASES: &[&str] = &[
    "sync",
    "sync.scan",
    "sync.files",
    "sync.frameworks",
    "sync.resolve",
    "sync.synthesis",
    "sync.finish",
];

const STAGE_PARTS: &[&str] = &[
    "synthesis.read",
    "synthesis.merge",
    "synthesis.replace",
    "synthesis.inputs",
];

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-timing-lines-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Run {
    stdout: String,
    stderr: String,
}

fn run(cwd: &Path, args: &[&str], timings: bool) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codegraph"));
    command
        .current_dir(cwd)
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", cwd.join("http-registry"))
        .env("CODEGRAPH_MCP_REGISTRY_DIR", cwd.join("mcp-registry"))
        .env_remove("CODEGRAPH_DIR");
    if timings {
        command.env("CODEGRAPH_PHASE_TIMINGS", "1");
    } else {
        command.env_remove("CODEGRAPH_PHASE_TIMINGS");
    }
    let output = command.output().expect("run codegraph");
    let run = Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };
    assert!(
        output.status.success(),
        "codegraph {args:?} failed:\n{}\n{}",
        run.stdout,
        run.stderr
    );
    run
}

/// Every phase-timing line of `stderr` as (command, phase, microseconds).
fn phase_lines(stderr: &str) -> Vec<(String, String, u128)> {
    stderr
        .lines()
        .filter(|line| line.starts_with(TAG_FIELD))
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 4, "malformed phase line {line:?}");
            assert_eq!(fields[0], TAG, "{line:?}");
            let micros = fields[3]
                .parse()
                .unwrap_or_else(|_| panic!("microseconds in {line:?}"));
            (fields[1].to_string(), fields[2].to_string(), micros)
        })
        .collect()
}

/// The phases of `lines`, each name once (panics on a repeat), all from
/// `command`.
fn phases_once(lines: &[(String, String, u128)], command: &str) -> BTreeMap<String, u128> {
    let mut phases = BTreeMap::new();
    for (from, phase, micros) in lines {
        assert_eq!(from, command, "{phase} came from {from}");
        assert!(
            phases.insert(phase.clone(), *micros).is_none(),
            "{phase} was reported twice: {lines:?}"
        );
    }
    phases
}

fn project(temp: &TestDir) -> PathBuf {
    let project = temp.path.join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/math.ts"),
        "export function add(a: number, b: number) { return a + b; }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/app.ts"),
        "import { add } from './math';\nexport function run() { return add(1, 2); }\n",
    )
    .unwrap();
    project
}

fn sum_of(phases: &BTreeMap<String, u128>, prefix: &str) -> u128 {
    phases
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(_, micros)| micros)
        .sum()
}

#[test]
fn init_and_sync_print_each_phase_once_under_the_variable() {
    let temp = TestDir::new("set");
    let project = project(&temp);
    let project_arg = project.to_str().unwrap();

    let init = run(&temp.path, &["init", project_arg], true);
    assert!(!init.stdout.contains(TAG_FIELD), "stdout: {}", init.stdout);
    let phases = phases_once(&phase_lines(&init.stderr), "init");
    for phase in INIT_PHASES {
        assert!(
            phases.contains_key(*phase),
            "init lacks {phase}: {phases:?}"
        );
    }
    for phase in phases.keys() {
        assert!(
            INIT_PHASES.contains(&phase.as_str()) || phase.starts_with("synthesis."),
            "init reported an unknown phase {phase}"
        );
    }
    // The passes every project runs are listed by their upstream names.
    assert!(phases.contains_key("synthesis.fieldEdges"), "{phases:?}");
    // A part never takes longer than its phase.
    assert!(phases["parse_write.store_busy"] <= phases["parse_write"]);
    assert!(sum_of(&phases, "reference_resolution.") <= phases["reference_resolution"]);
    assert!(sum_of(&phases, "synthesis.") <= phases["synthesis"]);

    // A new function is a base-graph change, so the sync runs the stage.
    fs::write(
        project.join("src/math.ts"),
        "export function add(a: number, b: number) { return a + b; }\n\
         export function sub(a: number, b: number) { return a - b; }\n",
    )
    .unwrap();
    let sync = run(&temp.path, &["sync", "--quiet", project_arg], true);
    assert!(sync.stdout.is_empty(), "--quiet stdout: {}", sync.stdout);
    let phases = phases_once(&phase_lines(&sync.stderr), "sync");
    for phase in SYNC_PHASES {
        assert!(
            phases.contains_key(*phase),
            "sync lacks {phase}: {phases:?}"
        );
    }
    for phase in phases.keys() {
        assert!(
            SYNC_PHASES.contains(&phase.as_str()) || phase.starts_with("synthesis."),
            "sync reported an unknown phase {phase}"
        );
    }
    // The edit changed the base graph, so the stage ran, deciding step by
    // step (W9-00 row 27): the field pass, which the new function does not
    // reach, kept its output.
    for part in STAGE_PARTS {
        assert!(phases.contains_key(*part), "sync lacks {part}: {phases:?}");
    }
    assert!(phases.contains_key("synthesis.decide"), "{phases:?}");
    assert!(
        phases.contains_key("synthesis.fieldEdges.reused"),
        "{phases:?}"
    );
    assert!(!phases.contains_key("synthesis.fieldEdges"), "{phases:?}");
    // The six parts follow one another inside the whole sync.
    assert!(sum_of(&phases, "sync.") <= phases["sync"]);
    assert!(sum_of(&phases, "synthesis.") <= phases["sync.synthesis"]);
}

#[test]
fn init_and_sync_print_no_phase_line_without_the_variable() {
    let temp = TestDir::new("unset");
    let project = project(&temp);
    let project_arg = project.to_str().unwrap();

    let init = run(&temp.path, &["init", project_arg], false);
    assert!(
        phase_lines(&init.stderr).is_empty(),
        "stderr: {}",
        init.stderr
    );
    assert!(!init.stdout.contains(TAG_FIELD), "stdout: {}", init.stdout);
    fs::write(
        project.join("src/app.ts"),
        "import { add } from './math';\nexport function run() { return add(2, 3); }\n",
    )
    .unwrap();
    let sync = run(&temp.path, &["sync", project_arg], false);
    assert!(
        phase_lines(&sync.stderr).is_empty(),
        "stderr: {}",
        sync.stderr
    );
    assert!(!sync.stdout.contains(TAG_FIELD), "stdout: {}", sync.stdout);
}
