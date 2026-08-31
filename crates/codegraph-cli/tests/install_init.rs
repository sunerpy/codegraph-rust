//! End-to-end coverage for the explicit one-shot bootstrap flags:
//! `codegraph init --yes` and `codegraph install --init`.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codegraph-install-init-{label}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("main.rs"),
            "pub fn greet() -> &'static str { \"hello\" }\n",
        )
        .unwrap();
        Self {
            root,
            home,
            project,
        }
    }

    fn command(&self, cwd: &std::path::Path, args: &[&str]) -> Output {
        Command::new(bin())
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .env("HERMES_HOME", self.root.join("hermes"))
            .env_remove("APPDATA")
            .output()
            .expect("run codegraph")
    }

    fn run_ok(&self, args: &[&str]) -> Output {
        let output = self.command(&self.project, args);
        assert!(
            output.status.success(),
            "command {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[test]
fn install_yes_target_none_init_builds_the_current_project() {
    let fixture = Fixture::new("one-shot");

    let output = fixture.run_ok(&["install", "--yes", "--target=none", "--init"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("No agent targets selected"));
    assert!(stdout.contains("Initialized in"));
    assert!(fixture.project.join(".codegraph/codegraph.db").is_file());
}

#[test]
fn install_yes_init_still_bootstraps_when_no_agent_was_detected() {
    let fixture = Fixture::new("auto-fallback");

    let output = fixture.run_ok(&["install", "--yes", "--init"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Claude Code"),
        "auto detection has no installed agent in the isolated HOME, so the existing Claude fallback should run: {stdout}"
    );
    assert!(fixture.project.join(".codegraph/codegraph.db").is_file());
}

#[test]
fn install_init_is_idempotent_for_an_already_initialized_project() {
    let fixture = Fixture::new("already-initialized");
    fixture.run_ok(&["init", "--yes"]);

    let output = fixture.run_ok(&["install", "--yes", "--target=none", "--init"]);

    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Already initialized"),
        "the shared init flow should preserve the existing-index fast path"
    );
    assert!(fixture.project.join(".codegraph/codegraph.db").is_file());
}

#[test]
fn install_without_init_never_creates_an_index() {
    let fixture = Fixture::new("no-implicit-index");

    fixture.run_ok(&["install", "--yes", "--target=none"]);

    assert!(!fixture.project.join(".codegraph").exists());
}

#[test]
fn print_config_returns_before_explicit_init() {
    let fixture = Fixture::new("print-config");

    let output = fixture.run_ok(&["install", "--print-config=codex", "--local", "--init"]);

    assert!(String::from_utf8_lossy(&output.stdout).contains("[mcp_servers.codegraph]"));
    assert!(!fixture.project.join(".codegraph").exists());
    assert!(!fixture.project.join(".codex/config.toml").exists());
}

#[test]
fn install_init_runs_only_after_a_successful_installer() {
    let fixture = Fixture::new("failed-installer");

    let output = fixture.command(
        &fixture.project,
        &["install", "--yes", "--target=unknown", "--init"],
    );

    assert!(!output.status.success());
    assert!(!fixture.project.join(".codegraph").exists());
}

#[test]
fn init_yes_is_behavior_neutral_and_non_interactive() {
    let fixture = Fixture::new("init-yes");

    let output = fixture.run_ok(&["init", "--yes"]);

    assert!(String::from_utf8_lossy(&output.stdout).contains("Initialized in"));
    assert!(fixture.project.join(".codegraph/codegraph.db").is_file());
}

#[test]
fn install_init_keeps_the_init_unsafe_root_guard() {
    let fixture = Fixture::new("unsafe-home");

    let output = fixture.command(
        &fixture.home,
        &["install", "--yes", "--target=none", "--init"],
    );

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to index"));
    assert!(!fixture.home.join(".codegraph").exists());
}

#[test]
fn help_lists_both_bootstrap_flags() {
    let fixture = Fixture::new("help");
    let init = fixture.run_ok(&["init", "--help"]);
    let install = fixture.run_ok(&["install", "--help"]);

    assert!(String::from_utf8_lossy(&init.stdout).contains("-y, --yes"));
    assert!(String::from_utf8_lossy(&install.stdout).contains("-i, --init"));
}
