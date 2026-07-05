//! Parameterized regression tests for `codegraph install` config-file safety,
//! using `rstest` (cases), `assert_fs` (temp HOME/project), and `assert_cmd`
//! (the built binary). These lock the two bugs that shipped before:
//! 1. JSONC configs (with `//` comments) were clobbered into an empty stub.
//! 2. Re-serialization dropped comments and re-sorted keys.

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use rstest::rstest;

fn run_install(home: &TempDir, project: &std::path::Path, target: &str) {
    Command::cargo_bin("codegraph")
        .unwrap()
        .args(["install", &format!("--target={target}"), "--local", "--yes"])
        .current_dir(project)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.child("xdg").path())
        .env("HERMES_HOME", home.child("hermes").path())
        .env_remove("APPDATA")
        .assert()
        .success();
}

#[rstest]
#[case("claude", ".mcp.json", "mcpServers")]
#[case("cursor", ".cursor/mcp.json", "mcpServers")]
#[case("opencode", "opencode.jsonc", "mcp")]
fn install_preserves_jsonc_comments_and_user_keys(
    #[case] target: &str,
    #[case] rel_path: &str,
    #[case] parent_key: &str,
) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    // A hand-maintained JSONC config: comments + a sibling server + a key that
    // must keep its position relative to the injected entry.
    let cfg = project.child(rel_path);
    cfg.write_str(&format!(
        "{{\n  // user comment — must survive\n  \"{parent_key}\": {{\n    \"existing-server\": {{ \"command\": \"foo\" }}\n  }},\n  \"zzz_last\": \"keep me last\"\n}}\n"
    ))
    .unwrap();

    run_install(&home, project.path(), target);

    let after = std::fs::read_to_string(cfg.path()).unwrap();
    assert!(
        after.contains("// user comment — must survive"),
        "[{target}] comment was dropped:\n{after}"
    );
    assert!(
        after.contains("existing-server"),
        "[{target}] sibling server lost:\n{after}"
    );
    assert!(
        after.contains("\"codegraph\""),
        "[{target}] codegraph not added:\n{after}"
    );
    assert!(
        after.contains("zzz_last"),
        "[{target}] user key lost:\n{after}"
    );
    // Key order preserved: zzz_last stays after the parent object.
    let parent_at = after.find(&format!("\"{parent_key}\"")).unwrap();
    let zzz_at = after.find("\"zzz_last\"").unwrap();
    assert!(
        parent_at < zzz_at,
        "[{target}] key order scrambled:\n{after}"
    );
}

#[rstest]
#[case("claude", ".mcp.json")]
#[case("cursor", ".cursor/mcp.json")]
#[case("opencode", "opencode.jsonc")]
fn install_does_not_clobber_unparseable_config(#[case] target: &str, #[case] rel_path: &str) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let cfg = project.child(rel_path);
    let corrupt = "{ this is : not valid json at all";
    cfg.write_str(corrupt).unwrap();

    run_install(&home, project.path(), target);

    let after = std::fs::read_to_string(cfg.path()).unwrap();
    assert_eq!(
        after, corrupt,
        "[{target}] unparseable config must be left byte-for-byte unchanged"
    );
}

#[rstest]
#[case("claude", ".mcp.json", "mcpServers")]
#[case("cursor", ".cursor/mcp.json", "mcpServers")]
#[case("opencode", "opencode.jsonc", "mcp")]
fn install_is_idempotent(#[case] target: &str, #[case] rel_path: &str, #[case] parent_key: &str) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    run_install(&home, project.path(), target);
    let cfg = project.child(rel_path);
    let first = std::fs::read_to_string(cfg.path()).unwrap();

    run_install(&home, project.path(), target);
    let second = std::fs::read_to_string(cfg.path()).unwrap();

    assert_eq!(
        first, second,
        "[{target}] re-install must not churn the file"
    );
    // exactly one codegraph entry under the parent
    let count = second.matches("\"codegraph\"").count();
    assert!(
        count >= 1 && second.contains(parent_key),
        "[{target}] expected a single codegraph entry under {parent_key}"
    );
}

/// Kiro's `mcp.json` must keep the ACTIVE stdio codegraph entry AND carry a
/// `//`-commented HTTP localhost alternative — parseable as JSONC, idempotent,
/// and non-corrupting of a user's pre-existing config.
#[test]
fn kiro_install_writes_stdio_plus_commented_http_localhost_alternative() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let cfg = project.child(".kiro/settings/mcp.json");
    cfg.write_str(
        "{\n  // user comment — must survive\n  \"mcpServers\": {\n    \"other\": { \"command\": \"foo\" }\n  }\n}\n",
    )
    .unwrap();

    run_install(&home, project.path(), "kiro");

    let after = std::fs::read_to_string(cfg.path()).unwrap();
    assert!(
        after.contains("\"codegraph\""),
        "no codegraph entry:\n{after}"
    );
    assert!(after.contains("\"stdio\""), "stdio type missing:\n{after}");
    assert!(after.contains("// user comment — must survive"), "{after}");
    assert!(after.contains("\"other\""), "sibling lost:\n{after}");
    assert!(
        after.contains("// \"codegraph\": { \"url\": \"http://localhost:8111/mcp\" }"),
        "commented localhost HTTP url missing:\n{after}"
    );
    assert!(
        after.contains("codegraph serve --http"),
        "WHY note missing:\n{after}"
    );
    assert!(
        !after.contains("127.0.0.1:8111/mcp") && !after.contains("://0.0.0.0"),
        "HTTP example must be localhost, not a LAN/loopback IP:\n{after}"
    );

    run_install(&home, project.path(), "kiro");
    let second = std::fs::read_to_string(cfg.path()).unwrap();
    assert_eq!(after, second, "re-install churned the file");
    assert_eq!(
        second.matches("// HTTP alternative").count(),
        1,
        "HTTP comment duplicated:\n{second}"
    );
}

#[test]
fn version_subcommand_matches_flag() {
    let sub = Command::cargo_bin("codegraph")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let flag = Command::cargo_bin("codegraph")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        sub, flag,
        "`version` and `--version` must print the same line"
    );
    assert!(String::from_utf8_lossy(&sub).starts_with("codegraph "));
}

#[test]
fn self_update_help_lists_flags() {
    Command::cargo_bin("codegraph")
        .unwrap()
        .args(["self-update", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--check"))
        .stdout(predicates::str::contains("--force"))
        .stdout(predicates::str::contains("--tag"));
}
