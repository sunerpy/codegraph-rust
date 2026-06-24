//! End-to-end tests for the `codegraph skill` subcommand group.
//!
//! Each test runs the built `codegraph` binary as a subprocess with an isolated
//! `HOME` / `XDG_CONFIG_HOME` / `HERMES_HOME` (all pointed into a temp dir, so no
//! test ever touches the developer's real home), then asserts the on-disk skill
//! files (`<parent>/codegraph/SKILL.md` + `.codegraph-skill.json`) and the stdout
//! strings the T10 orchestrator emits. The harness mirrors `tests/installer.rs`
//! (temp `HOME` + assert_cmd-style `Command` builder + `Drop` cleanup), adding a
//! `current_dir` override for the codex/antigravity local-scope scenario.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    // CARGO_BIN_EXE_<name> points at the freshly built binary under test.
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
            "codegraph-skill-test-{label}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&project).unwrap();
        Self {
            root,
            home,
            project,
        }
    }

    /// Build a hermetic command in the temp project dir. Callers add args, then
    /// `.output()`; env is pinned so HOME/XDG/HERMES never touch the real home.
    fn command(&self) -> Command {
        let mut cmd = Command::new(bin());
        cmd.current_dir(&self.project)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .env("HERMES_HOME", self.root.join("hermes"))
            .env_remove("APPDATA");
        cmd
    }

    /// Run, asserting the process exits 0, and return stdout.
    fn run(&self, args: &[&str]) -> String {
        let output = self.command().args(args).output().expect("run codegraph");
        assert!(
            output.status.success(),
            "command {args:?} failed (exit {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
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

// --- Scenario 1: claude global install -------------------------------------

#[test]
fn skill_install_claude_global_writes_skill_and_sidecar() {
    let fx = Fixture::new("claude-global");
    let dir = fx.home.join(".claude/skills/codegraph");
    let skill_md = dir.join("SKILL.md");
    let sidecar = dir.join(".codegraph-skill.json");

    let out = fx.run(&["skill", "install", "--target=claude", "--global", "-y"]);

    assert!(skill_md.exists(), "SKILL.md must exist at {skill_md:?}");
    assert!(sidecar.exists(), "sidecar must exist at {sidecar:?}");
    assert!(
        out.contains("Created") || out.contains("Installed"),
        "stdout should mention Created/Installed:\n{out}"
    );

    // Content sanity (NOT byte-exact): frontmatter fence + skill name.
    let body = fs::read_to_string(&skill_md).unwrap();
    assert!(
        body.starts_with("---\n"),
        "SKILL.md starts with frontmatter"
    );
    assert!(
        body.contains("name: codegraph"),
        "SKILL.md declares the name"
    );
}

// --- Scenario 2: --target=all global install -------------------------------

#[test]
fn skill_install_all_global_writes_every_supporting_agent() {
    let fx = Fixture::new("all-global");
    fx.run(&["skill", "install", "--target=all", "--global", "-y"]);

    // PARENT dir + codegraph/SKILL.md for each home-rooted agent.
    let home_cases: &[(&str, &str)] = &[
        ("claude", ".claude/skills"),
        ("kiro", ".kiro/skills"),
        ("gemini", ".gemini/skills"),
        ("codex", ".agents/skills"),
        ("antigravity", ".gemini/config/skills"),
    ];
    for (agent, rel) in home_cases {
        let skill_md = fx.home.join(rel).join("codegraph/SKILL.md");
        assert!(
            skill_md.exists(),
            "{agent}: expected skill at {skill_md:?} (rel {rel})"
        );
    }

    // opencode's global config dir honors $XDG_CONFIG_HOME (set by the Fixture),
    // resolving to `$XDG_CONFIG_HOME/opencode/skill` (SINGULAR), NOT ~/.config.
    let opencode_md = fx.root.join("xdg/opencode/skill/codegraph/SKILL.md");
    assert!(
        opencode_md.exists(),
        "opencode: expected skill at {opencode_md:?}"
    );

    // hermes resolves its root from $HERMES_HOME (set by the Fixture), so its
    // skill lands at `$HERMES_HOME/skills`, NOT ~/.hermes.
    let hermes_md = fx.root.join("hermes/skills/codegraph/SKILL.md");
    assert!(
        hermes_md.exists(),
        "hermes: expected skill at {hermes_md:?}"
    );
}

// --- Scenario 3: codex --local proves the supports_skills gate -------------

#[test]
fn skill_install_codex_local_writes_into_project() {
    // codex's MCP config is global-only (supports_location(Local)==false), but it
    // DOES support local skills (gated on supports_skills). So a local skill
    // install must WRITE under the project dir, NOT be skipped like local MCP.
    let fx = Fixture::new("codex-local");
    let skill_md = fx.project.join(".agents/skills/codegraph/SKILL.md");

    // cwd is the temp project (Fixture::command sets current_dir).
    fx.run(&["skill", "install", "--target=codex", "--local", "-y"]);

    assert!(
        skill_md.exists(),
        "codex local skill must be written under the project at {skill_md:?}"
    );
    let body = fs::read_to_string(&skill_md).unwrap();
    assert!(body.contains("name: codegraph"));
}

// --- Scenario 4: update with no change reports Unchanged -------------------

#[test]
fn skill_update_no_change_reports_unchanged() {
    let fx = Fixture::new("update-noop");
    fx.run(&["skill", "install", "--target=claude", "--global", "-y"]);

    let out = fx.run(&["skill", "update", "--target=claude", "--global"]);
    assert!(
        out.contains("Unchanged"),
        "re-running update with no change should report Unchanged:\n{out}"
    );
}

// --- Scenario 5: locally-modified protection + --force restore -------------

#[test]
fn skill_update_protects_local_edits_until_force() {
    let fx = Fixture::new("update-protect");
    let skill_md = fx.home.join(".claude/skills/codegraph/SKILL.md");

    fx.run(&["skill", "install", "--target=claude", "--global", "-y"]);

    // User edits the installed skill — provenance no longer matches the sidecar.
    fs::write(&skill_md, "hacked\n").unwrap();

    // update WITHOUT --force → protected: "locally modified", exit 0, file kept.
    let out = fx.run(&["skill", "update", "--target=claude", "--global"]);
    assert!(
        out.contains("locally modified"),
        "update without --force must report locally modified:\n{out}"
    );
    assert_eq!(
        fs::read_to_string(&skill_md).unwrap(),
        "hacked\n",
        "the locally modified file must be left untouched without --force"
    );

    // update WITH --force → restored to embedded content.
    fx.run(&["skill", "update", "--target=claude", "--global", "--force"]);
    let restored = fs::read_to_string(&skill_md).unwrap();
    assert_ne!(restored, "hacked\n", "--force must overwrite the edit");
    assert!(
        restored.contains("name: codegraph"),
        "restored content must be the embedded skill:\n{restored}"
    );
}

// --- Scenario 6: sidecar-None branch (deleted marker) ----------------------

#[test]
fn skill_update_without_sidecar_is_conservative() {
    let fx = Fixture::new("update-no-sidecar");
    let dir = fx.home.join(".claude/skills/codegraph");
    let skill_md = dir.join("SKILL.md");
    let sidecar = dir.join(".codegraph-skill.json");

    fx.run(&["skill", "install", "--target=claude", "--global", "-y"]);

    // Drop the provenance sidecar and leave a modified SKILL.md: unknown
    // provenance ⇒ decide() is conservative ⇒ LocallyModified.
    fs::remove_file(&sidecar).unwrap();
    fs::write(&skill_md, "drifted without provenance\n").unwrap();

    let out = fx.run(&["skill", "update", "--target=claude", "--global"]);
    assert!(
        out.contains("locally modified"),
        "missing sidecar + drift must be treated as locally modified:\n{out}"
    );
    assert_eq!(
        fs::read_to_string(&skill_md).unwrap(),
        "drifted without provenance\n",
        "the file must be left untouched"
    );
}

// --- Scenario 7: uninstall removes skill + sidecar -------------------------

#[test]
fn skill_uninstall_removes_skill_and_sidecar() {
    let fx = Fixture::new("uninstall");
    let dir = fx.home.join(".claude/skills/codegraph");
    let skill_md = dir.join("SKILL.md");
    let sidecar = dir.join(".codegraph-skill.json");

    fx.run(&["skill", "install", "--target=claude", "--global", "-y"]);
    assert!(skill_md.exists() && sidecar.exists());

    let out = fx.run(&["skill", "uninstall", "--target=claude", "--global", "-y"]);
    assert!(
        out.contains("removed") || out.contains("Removed"),
        "uninstall should mention removed:\n{out}"
    );
    assert!(!skill_md.exists(), "SKILL.md must be gone after uninstall");
    assert!(!sidecar.exists(), "sidecar must be gone after uninstall");
}

// --- Scenario 8: status transitions not-installed → up to date -------------

#[test]
fn skill_status_reports_install_state() {
    let fx = Fixture::new("status");

    let before = fx.run(&["skill", "status", "--target=claude", "--global"]);
    assert!(
        before.contains("not installed"),
        "fresh home should report not installed:\n{before}"
    );

    fx.run(&["skill", "install", "--target=claude", "--global", "-y"]);

    let after = fx.run(&["skill", "status", "--target=claude", "--global"]);
    assert!(
        after.contains("up to date"),
        "after install status should be up to date:\n{after}"
    );
}

// --- Scenario 9: uninstall when nothing installed (success exit) -----------

#[test]
fn skill_uninstall_absent_is_success_with_note() {
    let fx = Fixture::new("uninstall-absent");
    // No prior install; uninstall must exit 0 and report the empty state.
    let out = fx.run(&["skill", "uninstall", "--target=claude", "--global", "-y"]);
    assert!(
        out.contains("not configured — nothing to remove"),
        "uninstalling an absent skill must report the empty state, not fail:\n{out}"
    );
}

// --- Scenario 10: unsupported scope (hermes local) -------------------------

#[test]
fn skill_install_hermes_local_is_unsupported_but_succeeds() {
    let fx = Fixture::new("hermes-local");
    // hermes is global-only: a local skill install must report "not supported"
    // for --location=local and exit 0 (a note, not an error).
    let out = fx.run(&["skill", "install", "--target=hermes", "--local", "-y"]);
    assert!(
        out.contains("not supported"),
        "hermes local skill install must report not supported:\n{out}"
    );
}
