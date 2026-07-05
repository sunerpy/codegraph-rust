//! Zed editor target.
//!
//! Writes the MCP entry to Zed's `settings.json` under the `context_servers`
//! parent key (NOT `mcpServers`) as `context_servers.codegraph`. The entry has
//! NO `type` field — Zed's context-server shape is `{command, args, env}`.
//!
//! Config paths (Zed uses `~/.config/zed` on macOS too, so we do NOT reuse
//! `config_base_for`, which would give macOS `Library/Application Support`):
//!   - Global unix (Linux AND macOS): `$XDG_CONFIG_HOME/zed/settings.json`,
//!     falling back to `~/.config/zed/settings.json`.
//!   - Global Windows: `%APPDATA%\Zed\settings.json` (fall back to
//!     `~/AppData/Roaming/Zed/settings.json` when `app_data` is absent).
//!   - Local: `<project>/.zed/settings.json`.
//!
//! The GLOBAL entry is a bare `serve --mcp` (read-only off any existing index —
//! Zed's global config cannot inject a per-project path). The LOCAL entry pins
//! an absolute `--path = ctx.cwd`, which is exactly what `codegraph init
//! --target=zed` writes so the watcher can resolve the project.
//!
//! `detect.installed` is true only when the Zed config file/dir actually exists
//! (mirrors Kiro/Qoder), so `--target=auto` only wires Zed when Zed is present.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::shared::{
    ConfigRead, inject_zed_remote_comment, read_config_file, read_json_file,
    remove_nested_key_jsonc, to_upstream_json, upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct ZedTarget;

const ZED_GLOBAL_WHY: &str = "The global Zed entry is read-only off any existing index (Zed's global config cannot inject a per-project path); the agent passes the project path per call.";
const ZED_GLOBAL_HOWTO: &str = "For LIVE auto-update (watcher) run `codegraph init --target=zed` in each project (writes the project's absolute --path).";

/// Resolve the `settings.json` path for the given location.
///
/// Do NOT use `config_base_for` for the macOS path: Zed uses `~/.config/zed`
/// on macOS too, whereas `config_base_for` would resolve macOS to
/// `Library/Application Support`.
fn settings_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Local => ctx.cwd.join(".zed").join("settings.json"),
        Location::Global => {
            if cfg!(windows) {
                ctx.app_data
                    .as_ref()
                    .map(|d| d.join("Zed"))
                    .unwrap_or_else(|| ctx.home.join("AppData").join("Roaming").join("Zed"))
                    .join("settings.json")
            } else {
                ctx.xdg_config_home
                    .clone()
                    .unwrap_or_else(|| ctx.home.join(".config"))
                    .join("zed")
                    .join("settings.json")
            }
        }
    }
}

/// Build the Zed-specific context-server entry: `{command, args, env:{}}`.
///
/// Do NOT call `mcp_server_config()` — it injects `"type":"stdio"`, which Zed's
/// context-server shape does not use.
fn zed_entry(args: Vec<Value>) -> Value {
    json!({
        "command": "codegraph",
        "args": args,
        "env": {},
    })
}

/// The entry that WOULD be written for the given location: bare `serve --mcp`
/// globally, `serve --mcp --path <cwd>` locally.
fn build_zed_entry(ctx: &InstallContext, loc: Location) -> Value {
    match loc {
        Location::Global => zed_entry(vec![json!("serve"), json!("--mcp")]),
        Location::Local => zed_entry(vec![
            json!("serve"),
            json!("--mcp"),
            json!("--path"),
            json!(ctx.cwd.to_string_lossy().to_string()),
        ]),
    }
}

impl AgentTarget for ZedTarget {
    fn id(&self) -> TargetId {
        TargetId::Zed
    }
    fn display_name(&self) -> &'static str {
        "Zed"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let file = settings_path(ctx, loc);
        let config = read_json_file(&file);
        let already_configured = config
            .get("context_servers")
            .and_then(|s| s.get("codegraph"))
            .is_some();
        // `installed` is true only when Zed's config file/dir actually exists —
        // so `--target=auto` only wires Zed when Zed is really present.
        let installed = file.exists() || file.parent().map(|d| d.exists()).unwrap_or(false);
        DetectionResult {
            installed,
            already_configured,
        }
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let file = settings_path(ctx, loc);
        if let Some(dir) = file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let entry = build_zed_entry(ctx, loc);
        let action = match read_config_file(&file) {
            // Never clobber a config we cannot parse.
            ConfigRead::Unparseable => FileAction::Skipped,
            // `upsert_nested_key_jsonc` reads the file with `?` and errors on a
            // missing file, so seed it directly under the RIGHT parent
            // (`context_servers`, NOT `mcpServers`).
            ConfigRead::Missing => {
                let mut config = serde_json::Map::new();
                let mut ctx_servers = serde_json::Map::new();
                ctx_servers.insert("codegraph".to_string(), entry.clone());
                config.insert("context_servers".to_string(), Value::Object(ctx_servers));
                let _ = write_json_file(&file, &config);
                inject_zed_remote_comment(&file);
                FileAction::Created
            }
            ConfigRead::Parsed(_) => {
                let action =
                    upsert_nested_key_jsonc(&file, "context_servers", "codegraph", &entry, None)
                        .unwrap_or(FileAction::Skipped);
                if action != FileAction::Skipped {
                    inject_zed_remote_comment(&file);
                }
                action
            }
        };
        let notes = match loc {
            Location::Local => vec![
                format!(
                    "CodeGraph MCP configured for project {}.",
                    ctx.cwd.display()
                ),
                "Restart Zed for MCP changes to take effect.".to_string(),
            ],
            Location::Global => vec![ZED_GLOBAL_WHY.to_string(), ZED_GLOBAL_HOWTO.to_string()],
        };
        WriteResult {
            files: vec![FileWrite { path: file, action }],
            notes,
        }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let file = settings_path(ctx, loc);
        let action = remove_nested_key_jsonc(&file, "context_servers", "codegraph")
            .unwrap_or(FileAction::NotFound);
        WriteResult {
            files: vec![FileWrite { path: file, action }],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let file = settings_path(ctx, loc);
        let snippet = to_upstream_json(
            &json!({ "context_servers": { "codegraph": build_zed_entry(ctx, loc) } }),
        );
        match loc {
            Location::Local => format!("# Add to {}\n\n{snippet}\n", file.display()),
            Location::Global => format!(
                "# Add to {}\n# {ZED_GLOBAL_WHY}\n# {ZED_GLOBAL_HOWTO}\n\n{snippet}\n",
                file.display()
            ),
        }
    }
}

pub static ZED_TARGET: ZedTarget = ZedTarget;

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp-rooted context so probes never hit the real `~/.config/zed`. Sets
    /// `home`, `cwd`, `app_data`, and `xdg_config_home` under a unique temp base.
    fn temp_ctx(label: &str) -> (InstallContext, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "cg-zed-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = base.join("home");
        let ctx = InstallContext {
            home: home.clone(),
            cwd: base.join("cwd"),
            app_data: Some(home.join("AppData").join("Roaming")),
            xdg_config_home: Some(home.join(".config")),
            hermes_home: None,
        };
        (ctx, base)
    }

    fn run_install(ctx: &InstallContext, loc: Location) -> WriteResult {
        ZedTarget.install(
            ctx,
            loc,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        )
    }

    // (i) Global install writes a full context_servers.codegraph entry with NO
    // `type` field and an empty `env`, preserving a pre-seeded sibling.
    #[test]
    fn global_install_writes_full_entry_no_type_preserving_sibling() {
        let (ctx, base) = temp_ctx("global-full");
        let file = settings_path(&ctx, Location::Global);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Pre-seed a sibling context server that must survive.
        fs::write(
            &file,
            "{\n  \"context_servers\": {\n    \"other\": { \"command\": \"other-mcp\", \"args\": [], \"env\": {} }\n  }\n}\n",
        )
        .unwrap();

        let result = run_install(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::Updated);

        let config = read_json_file(&file);
        let entry = &config["context_servers"]["codegraph"];
        assert_eq!(
            entry,
            &json!({ "command": "codegraph", "args": ["serve", "--mcp"], "env": {} }),
            "global entry must be the full bare context-server object"
        );
        assert!(
            entry.get("type").is_none(),
            "entry must NOT have a type field"
        );
        assert_eq!(entry["env"], json!({}), "env must be an empty object");
        // Sibling survives.
        assert_eq!(
            config["context_servers"]["other"]["command"],
            json!("other-mcp"),
            "sibling context server must survive"
        );

        let _ = fs::remove_dir_all(base);
    }

    // (ii) Local install writes args ending [--path, <cwd>] into
    // <cwd>/.zed/settings.json.
    #[test]
    fn local_install_writes_absolute_path_in_dot_zed() {
        let (ctx, base) = temp_ctx("local-path");
        let file = settings_path(&ctx, Location::Local);
        assert!(
            file.ends_with(PathBuf::from(".zed").join("settings.json")),
            "local path must be <cwd>/.zed/settings.json, got {}",
            file.display()
        );

        let result = run_install(&ctx, Location::Local);
        assert_eq!(result.files[0].action, FileAction::Created);

        let config = read_json_file(&file);
        let args = config["context_servers"]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args array");
        assert_eq!(
            args,
            &vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!(ctx.cwd.to_string_lossy().to_string()),
            ],
            "local args must end with --path <abs cwd>"
        );

        let _ = fs::remove_dir_all(base);
    }

    // (iii) Install into a NON-EXISTENT settings.json creates
    // context_servers.codegraph — parent MUST be context_servers, NOT mcpServers.
    #[test]
    fn install_into_missing_file_creates_context_servers_parent() {
        let (ctx, base) = temp_ctx("missing");
        let file = settings_path(&ctx, Location::Global);
        assert!(!file.exists(), "precondition: settings.json absent");

        let result = run_install(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::Created);
        assert!(file.exists(), "settings.json must be created");

        let config = read_json_file(&file);
        assert!(
            config.get("context_servers").is_some(),
            "parent key must be context_servers"
        );
        assert!(
            config.get("mcpServers").is_none(),
            "parent key must NOT be mcpServers"
        );
        assert_eq!(
            config["context_servers"]["codegraph"],
            json!({ "command": "codegraph", "args": ["serve", "--mcp"], "env": {} })
        );

        let _ = fs::remove_dir_all(base);
    }

    // (iv) Install twice → second is Unchanged (idempotent, byte-identical).
    #[test]
    fn install_twice_is_idempotent() {
        let (ctx, base) = temp_ctx("idempotent");

        let first = run_install(&ctx, Location::Global);
        assert_eq!(first.files[0].action, FileAction::Created);

        let second = run_install(&ctx, Location::Global);
        assert_eq!(
            second.files[0].action,
            FileAction::Unchanged,
            "re-installing identical config must be Unchanged"
        );

        let _ = fs::remove_dir_all(base);
    }

    // (v) uninstall removes only codegraph, leaves the sibling.
    #[test]
    fn uninstall_removes_only_codegraph() {
        let (ctx, base) = temp_ctx("uninstall");
        let file = settings_path(&ctx, Location::Global);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  \"context_servers\": {\n    \"other\": { \"command\": \"other-mcp\" },\n    \"codegraph\": { \"command\": \"codegraph\", \"args\": [], \"env\": {} }\n  }\n}\n",
        )
        .unwrap();

        let result = ZedTarget.uninstall(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::Removed);

        let config = read_json_file(&file);
        assert!(
            config["context_servers"].get("other").is_some(),
            "sibling context server must survive"
        );
        assert!(
            config["context_servers"].get("codegraph").is_none(),
            "codegraph must be removed"
        );

        let _ = fs::remove_dir_all(base);
    }

    // (vi) Unparseable settings.json → Skipped (no clobber).
    #[test]
    fn unparseable_settings_is_skipped_no_clobber() {
        let (ctx, base) = temp_ctx("unparseable");
        let file = settings_path(&ctx, Location::Global);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let garbage = "{ this is not valid json ]]] ";
        fs::write(&file, garbage).unwrap();

        let result = run_install(&ctx, Location::Global);
        assert_eq!(
            result.files[0].action,
            FileAction::Skipped,
            "unparseable config must be Skipped"
        );
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            garbage,
            "unparseable config must NOT be clobbered"
        );

        let _ = fs::remove_dir_all(base);
    }

    // (vii) per-OS global path: xdg_config_home=Some → <xdg>/zed/settings.json;
    // None → <home>/.config/zed/settings.json. (unix only — Windows uses
    // app_data; guard with cfg!(windows).)
    #[test]
    fn global_unix_path_respects_xdg_then_falls_back_to_home_config() {
        if cfg!(windows) {
            return;
        }
        let (ctx, base) = temp_ctx("xdg");
        // With XDG set → <xdg>/zed/settings.json
        let with_xdg = settings_path(&ctx, Location::Global);
        let expected_xdg = ctx
            .xdg_config_home
            .as_ref()
            .unwrap()
            .join("zed")
            .join("settings.json");
        assert_eq!(with_xdg, expected_xdg, "must honor XDG_CONFIG_HOME");

        // With XDG unset → <home>/.config/zed/settings.json
        let ctx_no_xdg = InstallContext {
            xdg_config_home: None,
            ..ctx.clone()
        };
        let without_xdg = settings_path(&ctx_no_xdg, Location::Global);
        let expected_home = ctx.home.join(".config").join("zed").join("settings.json");
        assert_eq!(
            without_xdg, expected_home,
            "must fall back to ~/.config/zed"
        );

        let _ = fs::remove_dir_all(base);
    }

    // (viii) supports_skills is false (Zed has no agent-skill dir).
    #[test]
    fn zed_does_not_support_skills() {
        assert!(!ZedTarget.supports_skills(Location::Global));
        assert!(!ZedTarget.supports_skills(Location::Local));
    }

    #[test]
    fn detect_reflects_config_presence_and_configuration() {
        let (ctx, base) = temp_ctx("detect");
        let before = ZedTarget.detect(&ctx, Location::Global);
        assert!(!before.installed);
        assert!(!before.already_configured);

        run_install(&ctx, Location::Global);
        let after = ZedTarget.detect(&ctx, Location::Global);
        assert!(after.installed);
        assert!(after.already_configured);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_missing_is_not_found() {
        let (ctx, base) = temp_ctx("uninstall-missing");
        let result = ZedTarget.uninstall(&ctx, Location::Local);
        assert_eq!(result.files[0].action, FileAction::NotFound);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn print_config_local_and_global() {
        let (ctx, base) = temp_ctx("print");
        let local = ZedTarget.print_config(&ctx, Location::Local);
        assert!(local.contains("context_servers"));
        assert!(local.contains("--path"));
        assert!(!local.contains("\"type\""));

        let global = ZedTarget.print_config(&ctx, Location::Global);
        assert!(global.contains("context_servers"));
        assert!(global.contains(ZED_GLOBAL_WHY));
        assert!(global.contains(ZED_GLOBAL_HOWTO));

        let _ = fs::remove_dir_all(base);
    }

    fn read_text(ctx: &InstallContext, loc: Location) -> String {
        fs::read_to_string(settings_path(ctx, loc)).unwrap()
    }

    fn assert_active_stdio_and_commented_alternatives(text: &str) {
        let parsed =
            super::super::super::shared::parse_json_object(text).expect("settings.json parses");
        let entry = &parsed["context_servers"]["codegraph"];
        assert_eq!(entry["command"], json!("codegraph"), "active stdio intact");
        assert!(entry.get("url").is_none(), "active entry must stay stdio");
        assert!(
            text.contains(super::super::super::shared::ZED_REMOTE_COMMENT_SENTINEL),
            "remote-alternatives sentinel missing:\n{text}"
        );
        assert!(
            text.contains("\"command\": \"ssh\""),
            "ssh remote alternative missing:\n{text}"
        );
        assert!(
            text.contains("http://localhost:8111/mcp"),
            "http alternative url missing:\n{text}"
        );
        assert!(
            text.contains("RECOMMENDED for remote"),
            "http must be marked recommended for remote:\n{text}"
        );
    }

    // (ix) Fresh install → active stdio context_servers.codegraph AND commented
    // ssh + http(recommended) alternatives; parses as JSONC. Covers both
    // Global (missing-file seed) and Local (init-equivalent) paths.
    #[test]
    fn fresh_install_has_active_stdio_plus_commented_ssh_and_http_alternatives() {
        for loc in [Location::Global, Location::Local] {
            let (ctx, base) = temp_ctx("fresh-alt");
            let result = run_install(&ctx, loc);
            assert_eq!(result.files[0].action, FileAction::Created);
            assert_active_stdio_and_commented_alternatives(&read_text(&ctx, loc));
            let _ = fs::remove_dir_all(base);
        }
    }

    // (x) Re-install is idempotent — no duplicated comment block, stdio intact.
    #[test]
    fn remote_comment_injection_is_idempotent() {
        let (ctx, base) = temp_ctx("alt-idem");
        run_install(&ctx, Location::Global);
        let first = read_text(&ctx, Location::Global);

        run_install(&ctx, Location::Global);
        let second = read_text(&ctx, Location::Global);

        assert_eq!(first, second, "re-install churned the file");
        assert_eq!(
            second
                .matches(super::super::super::shared::ZED_REMOTE_COMMENT_SENTINEL)
                .count(),
            1,
            "remote comment duplicated on re-run"
        );
        assert_active_stdio_and_commented_alternatives(&second);
        let _ = fs::remove_dir_all(base);
    }

    // (xi) Install into an existing settings.json preserves the user's other
    // context_servers + settings + comments, and still injects the alternatives.
    #[test]
    fn install_into_existing_settings_preserves_user_content_and_injects() {
        let (ctx, base) = temp_ctx("alt-existing");
        let file = settings_path(&ctx, Location::Global);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  // user setting — must survive\n  \"theme\": \"One Dark\",\n  \"context_servers\": {\n    \"other\": { \"command\": \"other-mcp\", \"args\": [], \"env\": {} }\n  }\n}\n",
        )
        .unwrap();

        run_install(&ctx, Location::Global);

        let text = read_text(&ctx, Location::Global);
        assert!(text.contains("// user setting — must survive"), "{text}");
        assert!(text.contains("\"theme\""), "user setting lost:\n{text}");
        let parsed =
            super::super::super::shared::parse_json_object(&text).expect("still parses as JSONC");
        assert_eq!(
            parsed["context_servers"]["other"]["command"],
            json!("other-mcp"),
            "sibling context server lost"
        );
        assert_eq!(parsed["theme"], json!("One Dark"), "user setting lost");
        assert_active_stdio_and_commented_alternatives(&text);

        run_install(&ctx, Location::Global);
        assert_eq!(read_text(&ctx, Location::Global), text, "second churned");

        let _ = fs::remove_dir_all(base);
    }
}
