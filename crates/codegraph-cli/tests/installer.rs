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
    assert!(
        allow
            .as_array()
            .unwrap()
            .contains(&Value::String("mcp__codegraph__codegraph_explore".into()))
    );
    assert!(
        fs::read_to_string(&claude_md)
            .unwrap()
            .contains("<!-- CODEGRAPH_START -->")
    );

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
fn codex_round_trip_preserves_trailing_array_of_tables() {
    let fx = Fixture::new("codex-array-table");
    let toml = fx.home.join(".codex/config.toml");
    let trailing = "[[mcp_servers.other.env]]\nname = \"KEEP_ME\"\nvalue = \"critical\"\n";

    fx.run(&["install", "--target=codex", "--global", "--yes"]);
    let installed = fs::read_to_string(&toml).unwrap();
    assert!(installed.contains("[mcp_servers.codegraph]"));

    fs::write(&toml, format!("{installed}\n{trailing}")).unwrap();
    let before_reinstall = fs::read_to_string(&toml).unwrap();

    fx.run(&["install", "--target=codex", "--global", "--yes"]);
    assert_eq!(
        fs::read_to_string(&toml).unwrap(),
        before_reinstall,
        "re-install must preserve the complete config byte-for-byte"
    );

    fx.run(&["uninstall", "--target=codex", "--global"]);
    let uninstalled = fs::read_to_string(&toml).unwrap();
    assert_eq!(uninstalled, trailing);
    assert!(!uninstalled.contains("[mcp_servers.codegraph]"));
}

#[test]
fn codex_replaces_indented_table_then_uninstalls_it() {
    let fx = Fixture::new("codex-indented-table");
    let toml = fx.home.join(".codex/config.toml");
    let original = concat!(
        "# fixture — valid TOML\n",
        "[[user.tools]]\n",
        "name = \"keep\"\n",
        "\n",
        "  [mcp_servers.codegraph]\n",
        "  command = \"old\"\n",
    );
    fs::create_dir_all(toml.parent().unwrap()).unwrap();
    fs::write(&toml, original).unwrap();
    toml::from_str::<toml::Value>(original).expect("fixture must be valid TOML");

    fx.run(&["install", "--target=codex", "--global", "--yes"]);
    let installed = fs::read_to_string(&toml).unwrap();
    toml::from_str::<toml::Value>(&installed)
        .expect("install must replace the indented table without duplicating it");
    assert_eq!(installed.matches("[mcp_servers.codegraph]").count(), 1);
    assert!(
        installed
            .lines()
            .any(|line| line == "[mcp_servers.codegraph]")
    );
    assert!(
        !installed
            .lines()
            .any(|line| line == "  [mcp_servers.codegraph]")
    );
    assert!(installed.contains("name = \"keep\""));

    fx.run(&["uninstall", "--target=codex", "--global"]);
    let uninstalled = fs::read_to_string(&toml).unwrap();
    toml::from_str::<toml::Value>(&uninstalled).expect("uninstall result must be valid TOML");
    assert!(!uninstalled.contains("[mcp_servers.codegraph]"));
    assert!(uninstalled.contains("name = \"keep\""));
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
fn zuno_global_migrates_legacy_entry_and_preserves_user_content() {
    let fx = Fixture::new("zuno-global");
    let config = fx.root.join("xdg/zuno/zuno.json");
    let agents = fx.root.join("xdg/zuno/AGENTS.md");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        &config,
        concat!(
            "{\n",
            "  // user comment\n",
            "  \"mcp\": {\n",
            "    \"other\": { \"type\": \"local\", \"command\": [\"other\"] },\n",
            "    \"codegraph-mcp-server\": { \"type\": \"local\", \"command\": [\"codegraph\", \"serve\", \"--mcp\"] }\n",
            "  }\n",
            "}\n",
        ),
    )
    .unwrap();
    fs::write(
        &agents,
        "user before\n\n<!-- CODEGRAPH_START -->\nold\n<!-- CODEGRAPH_END -->\n\nuser after\n",
    )
    .unwrap();

    fx.run(&["install", "--target=zuno", "--global", "--yes"]);
    let raw = fs::read_to_string(&config).unwrap();
    assert!(raw.contains("// user comment"));
    let parsed = crate_jsonc(&raw);
    let mcp = parsed["mcp"].as_object().unwrap();
    assert!(mcp.get("other").is_some());
    assert!(mcp.get("codegraph-mcp-server").is_none());
    assert_eq!(
        mcp["codegraph"]["command"],
        serde_json::json!(["codegraph", "serve", "--mcp"])
    );
    assert_eq!(mcp["codegraph"]["type"], "local");
    assert_eq!(mcp["codegraph"]["enabled"], true);

    let instructions = fs::read_to_string(&agents).unwrap();
    assert!(instructions.contains("user before"));
    assert!(instructions.contains("user after"));
    assert!(instructions.contains("`codegraph_status`"));
    assert!(!instructions.contains("\nold\n"));

    fx.run(&["uninstall", "--target=zuno", "--global"]);
    let parsed = crate_jsonc(&fs::read_to_string(&config).unwrap());
    assert!(parsed["mcp"].get("codegraph").is_none());
    assert!(parsed["mcp"].get("other").is_some());
    assert_eq!(
        fs::read_to_string(&agents).unwrap(),
        "user before\n\nuser after\n"
    );
}

fn crate_jsonc(text: &str) -> Value {
    let without_comments = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&without_comments).unwrap()
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

fn user_prompt_groups(settings: &Value) -> Vec<Value> {
    settings["hooks"]["UserPromptSubmit"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn group_is_codegraph(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .map(|hooks| {
            hooks.iter().any(|h| {
                h["command"]
                    .as_str()
                    .map(|c| c.contains("prompt-hook"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[test]
fn claude_prompt_hook_is_opt_in_idempotent_then_uninstall() {
    let fx = Fixture::new("claude-hook");
    let settings = fx.project.join(".claude/settings.json");

    // Opt-out (default): a plain install must NOT write the front-load hook.
    fx.run(&["install", "--target=claude", "--local", "--yes"]);
    if settings.exists() {
        assert!(
            user_prompt_groups(&read_json(&settings))
                .iter()
                .all(|g| !group_is_codegraph(g)),
            "front-load hook must NOT be written without --prompt-hook"
        );
    }

    // A user-owned hook the installer must never touch.
    let mut cfg = read_json(&settings);
    cfg["hooks"]["UserPromptSubmit"] = serde_json::json!([
        { "hooks": [{ "type": "command", "command": "my-own-hook" }] }
    ]);
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();

    // Opt-in: --prompt-hook writes exactly one codegraph UserPromptSubmit group.
    fx.run(&[
        "install",
        "--target=claude",
        "--local",
        "--yes",
        "--prompt-hook",
    ]);
    let groups = user_prompt_groups(&read_json(&settings));
    assert_eq!(
        groups.iter().filter(|g| group_is_codegraph(g)).count(),
        1,
        "exactly one codegraph front-load hook, got {groups:?}"
    );
    assert!(
        groups
            .iter()
            .any(|g| g["hooks"][0]["command"] == "my-own-hook"),
        "user's own hook must survive"
    );

    // Re-install with --prompt-hook: idempotent, still exactly one.
    fx.run(&[
        "install",
        "--target=claude",
        "--local",
        "--yes",
        "--prompt-hook",
    ]);
    let groups = user_prompt_groups(&read_json(&settings));
    assert_eq!(
        groups.iter().filter(|g| group_is_codegraph(g)).count(),
        1,
        "re-install must not duplicate the hook"
    );

    // Uninstall: codegraph hook gone, user's own hook kept.
    fx.run(&["uninstall", "--target=claude", "--local"]);
    let groups = user_prompt_groups(&read_json(&settings));
    assert!(
        groups.iter().all(|g| !group_is_codegraph(g)),
        "codegraph front-load hook removed on uninstall"
    );
    assert!(
        groups
            .iter()
            .any(|g| g["hooks"][0]["command"] == "my-own-hook"),
        "user's own hook preserved through uninstall"
    );
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

// ===================== Copilot targets (#23) =================================
//
// The three Copilot surfaces are additive registry entries, so the end-to-end
// contract is "the id resolves and writes the documented file with the documented
// wrapper key". Before this change all three failed with
// `Unknown target "vscode". Known: claude, cursor, …`.

#[test]
fn vscode_local_install_writes_servers_wrapper_then_uninstalls() {
    let fx = Fixture::new("vscode-local");
    let mcp = fx.project.join(".vscode/mcp.json");

    fx.run(&["install", "--target=vscode", "--local", "--yes"]);
    let config = read_json(&mcp);
    assert!(
        config.get("servers").is_some(),
        "VS Code uses the `servers` wrapper, got {config}"
    );
    assert!(
        config.get("mcpServers").is_none(),
        "must NOT write `mcpServers`: {config}"
    );
    let args = config["servers"]["codegraph"]["args"].clone();
    let args = args.as_array().unwrap();
    assert!(
        args.contains(&Value::String("--path".into())),
        "the LOCAL entry pins a project path: {args:?}"
    );

    // A sibling server the user owns must survive.
    let mut edited = read_json(&mcp);
    edited["servers"]["other"] = serde_json::json!({ "command": "foo" });
    fs::write(&mcp, serde_json::to_string_pretty(&edited).unwrap()).unwrap();

    fx.run(&["uninstall", "--target=vscode", "--local"]);
    let after = read_json(&mcp);
    assert!(after["servers"].get("codegraph").is_none());
    assert!(after["servers"].get("other").is_some());
}

/// Every `Code/User/mcp.json` written anywhere under `root`. The user-level base
/// is per-OS (`.config` / `AppData/Roaming` / `Library/Application Support`), and
/// the fixture pins HOME + XDG under `root`, so searching the fixture root keeps
/// the assertion OS-agnostic instead of hardcoding one platform's layout.
fn find_written(root: &Path, needle: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.ends_with(needle) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn vscode_global_install_never_writes_workspace_folder() {
    // VS Code expands ${workspaceFolder} only in a WORKSPACE mcp.json; in the
    // user-level file it stays literal and would point the server at a directory
    // named "${workspaceFolder}". Asserted against the WRITTEN FILE — reading
    // `--print-config` instead would miss a wrong entry in `install`.
    let fx = Fixture::new("vscode-global");
    let out = fx.run(&["install", "--target=vscode", "--global", "--yes"]);
    assert!(
        !out.contains("Unknown target"),
        "the vscode id must resolve: {out}"
    );
    let wanted = PathBuf::from("Code").join("User").join("mcp.json");
    let written = find_written(&fx.root, &wanted);
    assert_eq!(
        written.len(),
        1,
        "exactly one user-level Code/User/mcp.json must be written, got {written:?}"
    );
    let config = read_json(&written[0]);
    assert_eq!(
        config["servers"]["codegraph"]["args"],
        serde_json::json!(["serve", "--mcp"]),
        "the global entry must be BARE — no --path, no ${{workspaceFolder}}: {config}"
    );
    let raw = fs::read_to_string(&written[0]).unwrap();
    assert!(
        !raw.contains("${workspaceFolder}"),
        "the global config must not contain ${{workspaceFolder}}: {raw}"
    );
}

#[test]
fn copilot_cli_global_install_declares_all_tools() {
    let fx = Fixture::new("copilot-cli");
    fx.run(&["install", "--target=copilot-cli", "--global", "--yes"]);
    let config = read_json(&fx.home.join(".copilot/mcp-config.json"));
    assert_eq!(
        config["mcpServers"]["codegraph"]["tools"],
        serde_json::json!(["*"]),
        "without tools:[\"*\"] the CLI exposes no tools: {config}"
    );

    fx.run(&["uninstall", "--target=copilot-cli", "--global"]);
    let after = read_json(&fx.home.join(".copilot/mcp-config.json"));
    assert!(after.get("mcpServers").is_none() || after["mcpServers"].get("codegraph").is_none());
}

#[test]
fn jetbrains_global_install_writes_github_copilot_intellij() {
    let fx = Fixture::new("jetbrains");
    fx.run(&["install", "--target=jetbrains", "--global", "--yes"]);
    // The fixture pins XDG_CONFIG_HOME, which is where this target resolves on
    // every OS.
    let file = fx.root.join("xdg/github-copilot/intellij/mcp.json");
    assert!(file.is_file(), "missing {}", file.display());
    let config = read_json(&file);
    assert!(
        config.get("servers").is_some(),
        "JetBrains uses the `servers` wrapper: {config}"
    );
    assert_eq!(
        config["servers"]["codegraph"]["args"],
        serde_json::json!(["serve", "--mcp"]),
        "global-only target writes a bare entry"
    );
}

#[test]
fn all_three_copilot_ids_resolve_in_print_config() {
    let fx = Fixture::new("copilot-ids");
    for id in ["vscode", "copilot-cli", "jetbrains"] {
        let out = fx.run(&["install", "--print-config", id]);
        assert!(!out.contains("Unknown target"), "{id} must resolve: {out}");
        assert!(
            out.contains("codegraph"),
            "{id} must print a codegraph entry: {out}"
        );
    }
}
