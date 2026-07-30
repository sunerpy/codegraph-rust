//! End-to-end regression: the opt-in Godot `idFields` / `resourceFields` DSL
//! must fire under the REAL `codegraph index` CLI even when the process CWD is
//! NOT the project root.
//!
//! The pipeline (`extract_and_persist_frameworks`) hands the framework resolver a
//! repo-RELATIVE `.tres` path plus the project's EXPLICITLY loaded DSL config,
//! read from the addressed project's own index root — so the process CWD cannot
//! influence which config is used. This test drives the binary from a DIFFERENT
//! temp dir and asserts the `godot:id:*` (idFields) and `resourceFields` literal
//! sentinels land in `unresolved_refs`; a companion target asserts a LEGACY
//! `.codegraph/codegraph.json` is never adopted.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_core::IndexPaths;
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
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("project.godot"), PROJECT_GODOT).unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::write(root.join("data").join("spell.tres"), SPELL_TRES).unwrap();
}

/// Write the project's CURRENT-ROOT `codegraph.json` — the only DSL config a
/// project-scoped run consults. Call after `init` published the namespace.
fn write_dsl_config(root: &Path) {
    let paths = IndexPaths::resolve(root, None).expect("resolve index paths");
    fs::write(paths.extension_config(), DSL_CONFIG).unwrap();
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
    // Batch M: both the index DB and the opt-in DSL config live in the isolated
    // v2 namespace.
    let db = project.join(".codegraph-v2").join("codegraph.db");
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
    write_dsl_config(&project);
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
    // Given a Godot project with NO DSL config at all,
    let project_dir = TestDir::new("noconfig");
    let project = project_dir.path().join("game");
    write_godot_project(&project);
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

/// A LEGACY `.codegraph/codegraph.json` must NEVER supply the DSL config: with the
/// same block written only there, the run emits zero sentinels.
#[test]
fn legacy_dsl_config_is_never_adopted() {
    // Given a Godot project whose ONLY DSL config is the legacy one,
    let project_dir = TestDir::new("legacy");
    let project = project_dir.path().join("game");
    write_godot_project(&project);
    fs::create_dir_all(project.join(".codegraph")).unwrap();
    fs::write(
        project.join(".codegraph").join("codegraph.json"),
        DSL_CONFIG,
    )
    .unwrap();
    let foreign = TestDir::new("legacy-cwd");

    // When indexed,
    let project_str = project.to_string_lossy().into_owned();
    let (out, err, ok) = cli_from(foreign.path(), &["init", &project_str]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli_from(foreign.path(), &["index", "--force", &project_str]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");

    // Then nothing fired: neither the id sentinel nor the resourceFields literal.
    let names = unresolved_ref_names(&project);
    assert!(
        !names.iter().any(|n| n.starts_with("godot:id:")),
        "a legacy DSL config must not emit id sentinels: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "Fireball"),
        "a legacy DSL config must not emit resourceFields literals: {names:?}"
    );
}
