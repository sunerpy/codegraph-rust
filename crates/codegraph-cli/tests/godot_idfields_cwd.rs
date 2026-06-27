//! End-to-end regression: the opt-in Godot `idFields` / `resourceFields` DSL
//! must fire under the REAL `codegraph index` CLI even when the process CWD is
//! NOT the project root.
//!
//! The pipeline (`extract_and_persist_frameworks`) hands the framework resolver
//! a repo-RELATIVE `.tres` path; the DSL config reader used to resolve that path
//! against the process CWD, so `$PROJECT/.codegraph/codegraph.json` was only
//! found when the CLI happened to run with its CWD == the project root. The A3
//! unit tests masked this by passing ABSOLUTE `.tres` paths. This test drives
//! the binary from a DIFFERENT temp dir and asserts the `godot:id:*` (idFields)
//! and the `resourceFields` literal sentinels land in `unresolved_refs` —
//! proving config lookup now resolves against the project root.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_store::Store;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-idfields-cwd-{label}-{}-{}",
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

const DSL_CONFIG: &str = r#"{
  "godot": {
    "dsl": {
      "idFields": {
        "buff_id": { "kind": "buff" }
      },
      "resourceFields": ["effect_name"]
    }
  }
}
"#;

const PROJECT_GODOT: &str = "[application]\nconfig/name=\"idfields-cwd\"\n";

const SPELL_TRES: &str = "\
[gd_resource type=\"Resource\" format=3]

[resource]
buff_id = 7005
effect_name = \"Fireball\"
duration = 5.0
";

fn write_godot_project(root: &Path) {
    fs::create_dir_all(root.join(".codegraph")).unwrap();
    fs::write(root.join(".codegraph").join("codegraph.json"), DSL_CONFIG).unwrap();
    fs::write(root.join("project.godot"), PROJECT_GODOT).unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::write(root.join("data").join("spell.tres"), SPELL_TRES).unwrap();
}

/// Run the binary from `cwd` (a FOREIGN directory) against an absolute project
/// path. `CODEGRAPH_NO_DAEMON`/`NO_WATCH` keep the run foreground so the test
/// never blocks on a background daemon.
fn cli_from(cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_codegraph"));
    cmd.current_dir(cwd);
    cmd.args(args);
    cmd.env("CODEGRAPH_NO_DAEMON", "1");
    cmd.env("CODEGRAPH_NO_WATCH", "1");
    let output = cmd.output().expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn unresolved_ref_names(project: &Path) -> Vec<String> {
    let db = project.join(".codegraph").join("codegraph.db");
    let store = Store::open(&db).expect("open store");
    store
        .all_unresolved_refs()
        .expect("read unresolved_refs")
        .into_iter()
        .map(|r| r.reference_name)
        .collect()
}

/// Driving the real `init` + `index --force` from a foreign cwd against an
/// absolute project path must still discover the project's DSL config and emit
/// the `godot:id:buff:7005` sentinel (idFields) and the `Fireball` literal
/// (resourceFields) into `unresolved_refs`.
#[test]
fn idfields_dsl_fires_when_cwd_is_not_project_root() {
    // Given a Godot project with an opt-in DSL config, and a SEPARATE foreign cwd,
    let project_dir = TestDir::new("project");
    let project = project_dir.path().join("game");
    write_godot_project(&project);
    let foreign = TestDir::new("foreign-cwd");

    // When the real binary is run from the foreign cwd against the project's
    // ABSOLUTE path (cwd != project root — the case the bug silently no-op'd),
    let project_str = project.to_string_lossy().into_owned();
    let (out, err, ok) = cli_from(foreign.path(), &["init", &project_str]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli_from(foreign.path(), &["index", "--force", &project_str]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    // Then the idFields sentinel AND the resourceFields literal are captured.
    let names = unresolved_ref_names(&project);
    assert!(
        names.iter().any(|n| n == "godot:id:buff:7005"),
        "idFields sentinel `godot:id:buff:7005` missing from unresolved_refs: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "Fireball"),
        "resourceFields literal `Fireball` missing from unresolved_refs: {names:?}"
    );
}

/// Off-by-default guard, also from a foreign cwd: an identical project with NO
/// DSL config emits ZERO `godot:id:*` sentinels.
#[test]
fn no_config_emits_zero_id_sentinels_from_foreign_cwd() {
    // Given a Godot project with NO `.codegraph/codegraph.json` DSL block,
    let project_dir = TestDir::new("noconfig");
    let project = project_dir.path().join("game");
    fs::create_dir_all(project.join(".codegraph")).unwrap();
    fs::write(project.join("project.godot"), PROJECT_GODOT).unwrap();
    fs::create_dir_all(project.join("data")).unwrap();
    fs::write(project.join("data").join("spell.tres"), SPELL_TRES).unwrap();
    let foreign = TestDir::new("noconfig-cwd");

    // When indexed from a foreign cwd,
    let project_str = project.to_string_lossy().into_owned();
    let (out, err, ok) = cli_from(foreign.path(), &["init", &project_str]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli_from(foreign.path(), &["index", "--force", &project_str]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    // Then no `godot:id:*` sentinel exists.
    let names = unresolved_ref_names(&project);
    assert!(
        !names.iter().any(|n| n.starts_with("godot:id:")),
        "off-by-default violated: godot:id:* sentinel present without config: {names:?}"
    );
}
