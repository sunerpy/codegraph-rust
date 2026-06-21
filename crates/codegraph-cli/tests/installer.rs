//! End-to-end tests for `codegraph install` / `codegraph uninstall`.
//!
//! Each test runs the built `codegraph` binary as a subprocess with an isolated
//! `HOME` and working directory (temp dirs), then asserts the written config
//! files match the per-agent shapes the upstream targets produce — install →
//! re-install (no dup) → uninstall (removed, siblings kept).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

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
            "codegraph-installer-test-{label}-{}-{}",
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

    fn run(&self, args: &[&str]) -> String {
        let output = Command::new(bin())
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            // Pin the opencode/hermes env inputs so the test is hermetic and
            // never reads the developer's real ~/.config or $HERMES_HOME.
            .env("XDG_CONFIG_HOME", self.root.join("xdg"))
            .env("HERMES_HOME", self.root.join("hermes"))
            .env_remove("APPDATA")
            .output()
            .expect("run codegraph");
        assert!(
            output.status.success(),
            "command {args:?} failed: {}",
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

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn claude_local_install_idempotent_then_uninstall() {
    let fx = Fixture::new("claude");
    let mcp = fx.project.join(".mcp.json");
    let settings = fx.project.join(".claude/settings.json");
    let claude_md = fx.project.join(".claude/CLAUDE.md");

    // install writes the three files
    fx.run(&["install", "--target=claude", "--local", "--yes"]);
    let entry = &read_json(&mcp)["mcpServers"]["codegraph"];
    assert_eq!(entry["command"], "codegraph");
    assert_eq!(entry["args"], serde_json::json!(["serve", "--mcp"]));
    assert_eq!(entry["type"], "stdio");
    assert!(settings.exists());
    let allow = read_json(&settings)["permissions"]["allow"].clone();
    assert!(allow
        .as_array()
        .unwrap()
        .contains(&Value::String("mcp__codegraph__codegraph_explore".into())));
    assert!(fs::read_to_string(&claude_md)
        .unwrap()
        .contains("<!-- CODEGRAPH_START -->"));

    // a sibling MCP server the user owns must survive every operation
    let mut config = read_json(&mcp);
    config["mcpServers"]["other"] = serde_json::json!({ "command": "foo" });
    fs::write(&mcp, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    // re-install: no duplication, sibling preserved
    fx.run(&["install", "--target=claude", "--local", "--yes"]);
    let servers = read_json(&mcp)["mcpServers"].clone();
    let keys: Vec<&str> = servers
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.len(), 2, "exactly codegraph + other, got {keys:?}");
    assert!(keys.contains(&"codegraph") && keys.contains(&"other"));

    // uninstall: codegraph entry gone, sibling kept, CLAUDE.md removed
    fx.run(&["uninstall", "--target=claude", "--local"]);
    let servers = read_json(&mcp)["mcpServers"].clone();
    assert!(servers.get("codegraph").is_none());
    assert!(servers.get("other").is_some());
    assert!(
        !claude_md.exists(),
        "CLAUDE.md should be deleted when emptied"
    );
    let allow = read_json(&settings).get("permissions").cloned();
    assert!(allow.is_none(), "permissions key removed on uninstall");
}

#[test]
fn cursor_local_injects_path_arg() {
    let fx = Fixture::new("cursor");
    let mcp = fx.project.join(".cursor/mcp.json");
    fx.run(&["install", "--target=cursor", "--local", "--yes"]);
    let args = read_json(&mcp)["mcpServers"]["codegraph"]["args"].clone();
    let args = args.as_array().unwrap();
    assert_eq!(args[0], "serve");
    assert_eq!(args[1], "--mcp");
    assert_eq!(args[2], "--path");
    assert_eq!(
        args[3],
        Value::String(fx.project.to_string_lossy().into_owned())
    );

    fx.run(&["uninstall", "--target=cursor", "--local"]);
    assert!(read_json(&mcp)["mcpServers"].get("codegraph").is_none());
}

#[test]
fn codex_global_writes_toml_idempotent_then_uninstall() {
    let fx = Fixture::new("codex");
    let toml = fx.home.join(".codex/config.toml");

    fx.run(&["install", "--target=codex", "--global", "--yes"]);
    let content = fs::read_to_string(&toml).unwrap();
    assert!(content.contains("[mcp_servers.codegraph]"));
    assert!(content.contains("command = \"codegraph\""));
    assert!(content.contains("args = [\"serve\", \"--mcp\"]"));

    // a sibling table must survive
    fs::write(
        &toml,
        format!("{content}\n[mcp_servers.other]\ncommand = \"foo\"\n"),
    )
    .unwrap();
    fx.run(&["install", "--target=codex", "--global", "--yes"]);
    let content = fs::read_to_string(&toml).unwrap();
    assert_eq!(
        content.matches("[mcp_servers.codegraph]").count(),
        1,
        "no duplicate codegraph table"
    );
    assert!(content.contains("[mcp_servers.other]"));

    fx.run(&["uninstall", "--target=codex", "--global"]);
    let content = fs::read_to_string(&toml).unwrap();
    assert!(!content.contains("[mcp_servers.codegraph]"));
    assert!(content.contains("[mcp_servers.other]"));
}

#[test]
fn opencode_local_uses_mcp_wrapper() {
    let fx = Fixture::new("opencode");
    let cfg = fx.project.join("opencode.jsonc");
    fx.run(&["install", "--target=opencode", "--local", "--yes"]);
    let json = read_json(&cfg);
    assert_eq!(json["$schema"], "https://opencode.ai/config.json");
    let entry = &json["mcp"]["codegraph"];
    assert_eq!(entry["type"], "local");
    assert_eq!(
        entry["command"],
        serde_json::json!(["codegraph", "serve", "--mcp"])
    );
    assert_eq!(entry["enabled"], true);

    fx.run(&["install", "--target=opencode", "--local", "--yes"]);
    fx.run(&["uninstall", "--target=opencode", "--local"]);
    assert!(read_json(&cfg)["mcp"].get("codegraph").is_none());
}

#[test]
fn print_config_does_not_write() {
    let fx = Fixture::new("print");
    let out = fx.run(&["install", "--print-config", "codex"]);
    assert!(out.contains("[mcp_servers.codegraph]"));
    assert!(!fx.home.join(".codex/config.toml").exists());
}

#[test]
fn codex_local_is_skipped_global_only() {
    let fx = Fixture::new("codex-skip");
    let out = fx.run(&["install", "--target=codex", "--local", "--yes"]);
    assert!(out.contains("skipped"));
    assert!(!fx.home.join(".codex/config.toml").exists());
}

#[test]
fn unknown_target_fails() {
    let fx = Fixture::new("unknown");
    let output = Command::new(bin())
        .args(["install", "--target=nope", "--yes"])
        .current_dir(&fx.project)
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.join("xdg"))
        .env("HERMES_HOME", fx.root.join("hermes"))
        .env_remove("APPDATA")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown --target"));
}
