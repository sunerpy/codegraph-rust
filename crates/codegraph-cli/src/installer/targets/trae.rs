//! Trae IDE target. Ports the cross-OS VS Code-fork MCP install pattern.
//!
//! Trae has MULTIPLE MCP config layouts; the global path is resolved by probing
//! for the one that exists:
//!   1. server/remote mode — when `~/.trae-server` exists, the working file is
//!      `~/.trae-server/data/Machine/mcp.json`;
//!   2. desktop — otherwise the VS Code-fork `User/mcp.json` via
//!      [`vscode_user_mcp_json`] (mac `~/Library/Application Support/Trae/User`,
//!      win `%APPDATA%\Trae\User`, linux `~/.config/Trae/User`).
//!
//! Both layouts key the codegraph entry under `mcpServers`. The GLOBAL entry
//! uses `--path ${workspaceFolder}` — Trae officially expands the substitution
//! (docs.trae.cn), so one global config auto-follows each project window; in
//! server mode the server runs inside the workspace, so it resolves there too.
//! The LOCAL install writes `<project>/.trae/mcp.json` with an absolute
//! `--path = cwd` (project-level MCP requires enabling "Enable project-level
//! MCP / 启用项目级 MCP" in Trae settings).
//!
//! The upsert touches ONLY the `codegraph` key, preserving any sibling servers
//! byte-faithfully — a real Trae `Machine/mcp.json` may already hold others.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::super::shared::{
    ConfigRead, mcp_server_config, read_config_file, read_json_file,
    remove_codegraph_from_mcp_servers, to_upstream_json, upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::super::vscode_user::{config_base_for, vscode_user_mcp_json};
use super::claude::upsert_mcp_server;

pub struct TraeTarget;

/// The server/remote-mode marker dir (`~/.trae-server`) and the working file
/// inside it (`data/Machine/mcp.json`).
fn server_dir(ctx: &InstallContext) -> PathBuf {
    ctx.home.join(".trae-server")
}

/// Resolve the GLOBAL `mcp.json` path: server mode when `~/.trae-server` exists,
/// else the desktop VS Code-fork `Trae/User/mcp.json`.
fn trae_global_mcp_json(ctx: &InstallContext) -> PathBuf {
    if server_dir(ctx).exists() {
        server_dir(ctx)
            .join("data")
            .join("Machine")
            .join("mcp.json")
    } else {
        vscode_user_mcp_json(ctx, "Trae")
    }
}

fn mcp_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => trae_global_mcp_json(ctx),
        Location::Local => ctx.cwd.join(".trae").join("mcp.json"),
    }
}

/// Build the codegraph MCP entry: global uses `${workspaceFolder}`, local pins
/// the absolute project path. Mirrors `build_cursor_mcp_config`.
fn build_trae_mcp_config(ctx: &InstallContext, loc: Location) -> Value {
    let path_arg = match loc {
        Location::Local => ctx.cwd.to_string_lossy().to_string(),
        Location::Global => "${workspaceFolder}".to_string(),
    };
    let mut base = mcp_server_config();
    // Trae's stdio schema (docs.trae.ai/ide/add-mcp-servers) has NO `type` key
    // (stdio is implied by `command`); strip it here only, since shared
    // `mcp_server_config()` must keep `type` for Cursor/Kiro/Codex.
    if let Some(obj) = base.as_object_mut() {
        obj.remove("type");
    }
    if let Some(args) = base.get_mut("args").and_then(|a| a.as_array_mut()) {
        args.push(json!("--path"));
        args.push(json!(path_arg));
    }
    base
}

impl AgentTarget for TraeTarget {
    fn id(&self) -> TargetId {
        TargetId::Trae
    }
    fn display_name(&self) -> &'static str {
        "Trae"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let file = mcp_json_path(ctx, loc);
        let config = read_json_file(&file);
        let already_configured = config
            .get("mcpServers")
            .and_then(|s| s.get("codegraph"))
            .is_some();
        let installed = match loc {
            Location::Global => {
                server_dir(ctx).exists()
                    || file.parent().is_some_and(std::path::Path::exists)
                    || file.exists()
            }
            Location::Local => ctx.cwd.join(".trae").exists() || file.exists(),
        };
        DetectionResult {
            installed,
            already_configured,
        }
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let files = vec![write_mcp_entry(ctx, loc)];
        let notes = match loc {
            Location::Global => vec![
                "Trae expands ${workspaceFolder}, so one global config auto-follows each project window.".to_string(),
                "Restart Trae for MCP changes to take effect.".to_string(),
            ],
            Location::Local => vec![
                format!(
                    "CodeGraph MCP configured for project {}.",
                    ctx.cwd.display()
                ),
                "Trae: enable \"Enable project-level MCP / 启用项目级 MCP\" in settings to use the project-level .trae/mcp.json."
                    .to_string(),
                "Restart Trae for MCP changes to take effect.".to_string(),
            ],
        };
        WriteResult { files, notes }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mcp_path = mcp_json_path(ctx, loc);
        let mut config = read_json_file(&mcp_path);
        let action = if remove_codegraph_from_mcp_servers(&mut config) {
            let _ = write_json_file(&mcp_path, &config);
            FileAction::Removed
        } else {
            FileAction::NotFound
        };
        WriteResult {
            files: vec![FileWrite {
                path: mcp_path,
                action,
            }],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let target = mcp_json_path(ctx, loc);
        let snippet = to_upstream_json(
            &json!({ "mcpServers": { "codegraph": build_trae_mcp_config(ctx, loc) } }),
        );
        format!("# Add to {}\n\n{snippet}\n", target.display())
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        // Skills are independent of the MCP file location: global skills always
        // live under the desktop VS Code-fork `Trae/User/skills` dir; local
        // skills under `<project>/.trae/skills`.
        let dir = match loc {
            Location::Global => config_base_for(
                &ctx.home,
                ctx.app_data.as_deref(),
                ctx.xdg_config_home.as_deref(),
                std::env::consts::OS,
            )
            .join("Trae")
            .join("User")
            .join("skills"),
            Location::Local => ctx.cwd.join(".trae").join("skills"),
        };
        Some(dir)
    }
}

// Mirrors cursor.rs / kiro.rs `write_mcp_entry`, but ALWAYS `create_dir_all`s the
// parent first (mirroring kiro.rs:171-173) — neither `.trae-server/data/Machine/`
// nor `Trae/User/` is guaranteed to pre-exist on a fresh machine.
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = mcp_json_path(ctx, loc);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let after = build_trae_mcp_config(ctx, loc);
    match read_config_file(&file) {
        ConfigRead::Unparseable => FileWrite {
            path: file,
            action: FileAction::Skipped,
        },
        ConfigRead::Missing => {
            let mut config = Map::new();
            upsert_mcp_server(&mut config, "codegraph", after);
            let _ = write_json_file(&file, &config);
            FileWrite {
                path: file,
                action: FileAction::Created,
            }
        }
        ConfigRead::Parsed(_) => {
            let action = upsert_nested_key_jsonc(&file, "mcpServers", "codegraph", &after, None)
                .unwrap_or(FileAction::Skipped);
            FileWrite { path: file, action }
        }
    }
}

pub static TRAE_TARGET: TraeTarget = TraeTarget;

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp-rooted context so the probe never hits the real `~/.trae-server`.
    fn temp_ctx(label: &str) -> (InstallContext, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "cg-trae-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ctx = InstallContext {
            home: base.join("home"),
            cwd: base.join("cwd"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        };
        (ctx, base)
    }

    #[test]
    fn global_path_is_server_mode_when_trae_server_exists() {
        // Given a temp home that HAS a `.trae-server` dir
        let (ctx, base) = temp_ctx("servermode");
        fs::create_dir_all(ctx.home.join(".trae-server")).unwrap();

        // When resolving the global mcp.json path
        let path = mcp_json_path(&ctx, Location::Global);

        // Then it is the server-mode `data/Machine/mcp.json`
        assert!(
            path.ends_with(".trae-server/data/Machine/mcp.json"),
            "got {}",
            path.display()
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn global_path_is_desktop_user_when_no_trae_server() {
        // Given a temp home with NO `.trae-server` and an xdg base (Linux runner)
        let (mut ctx, base) = temp_ctx("desktop");
        ctx.xdg_config_home = Some(ctx.home.join(".config"));

        // When resolving the global mcp.json path
        let path = mcp_json_path(&ctx, Location::Global);

        // Then it is the desktop VS Code-fork `Trae/User/mcp.json`
        assert!(
            path.ends_with("Trae/User/mcp.json"),
            "got {}",
            path.display()
        );
        assert!(
            !path.to_string_lossy().contains(".trae-server"),
            "must NOT be server mode, got {}",
            path.display()
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn global_build_uses_workspace_folder() {
        let (ctx, base) = temp_ctx("globalbuild");
        let entry = build_trae_mcp_config(&ctx, Location::Global);
        assert!(
            entry.get("type").is_none(),
            "Trae entry must not carry a `type` key"
        );
        let args = entry["args"].as_array().expect("args array");
        assert_eq!(
            args,
            &vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!("${workspaceFolder}"),
            ]
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn local_build_uses_absolute_cwd() {
        let (ctx, base) = temp_ctx("localbuild");
        let entry = build_trae_mcp_config(&ctx, Location::Local);
        assert!(
            entry.get("type").is_none(),
            "Trae entry must not carry a `type` key"
        );
        let args = entry["args"].as_array().expect("args array");
        assert_eq!(
            args,
            &vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!(ctx.cwd.to_string_lossy().to_string()),
            ]
        );
        // The local path is `<cwd>/.trae/mcp.json`.
        let path = mcp_json_path(&ctx, Location::Local);
        assert!(path.ends_with(".trae/mcp.json"), "got {}", path.display());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn trae_config_omits_type_field() {
        // Given both install locations
        let (ctx, base) = temp_ctx("notype");

        // When building the global entry
        let global = build_trae_mcp_config(&ctx, Location::Global);
        // Then it has command + args (ending in ${workspaceFolder}) and NO type
        assert_eq!(global["command"], json!("codegraph"));
        assert!(global.get("type").is_none(), "global must omit `type`");
        assert_eq!(
            global["args"].as_array().expect("global args").last(),
            Some(&json!("${workspaceFolder}"))
        );

        // When building the local entry
        let local = build_trae_mcp_config(&ctx, Location::Local);
        // Then it has command + args (ending in the abs cwd) and NO type
        assert_eq!(local["command"], json!("codegraph"));
        assert!(local.get("type").is_none(), "local must omit `type`");
        assert_eq!(
            local["args"].as_array().expect("local args").last(),
            Some(&json!(ctx.cwd.to_string_lossy().to_string()))
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn upsert_preserves_sibling_servers() {
        // Given a server-mode global mcp.json pre-seeded with another server
        let (ctx, base) = temp_ctx("siblings");
        let server = ctx.home.join(".trae-server");
        fs::create_dir_all(&server).unwrap();
        let file = server.join("data").join("Machine").join("mcp.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  \"mcpServers\": {\n    \"github\": { \"command\": \"gh-mcp\", \"args\": [] }\n  }\n}\n",
        )
        .unwrap();

        // When codegraph is upserted via a global install
        let result = TraeTarget.install(
            &ctx,
            Location::Global,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );
        assert_eq!(result.files[0].action, FileAction::Updated);

        // Then the github server SURVIVES and codegraph is present
        let config = read_json_file(&file);
        assert!(
            config["mcpServers"].get("github").is_some(),
            "sibling github server must survive the upsert"
        );
        assert_eq!(
            config["mcpServers"]["github"]["command"],
            json!("gh-mcp"),
            "sibling server contents must be untouched"
        );
        assert!(
            config["mcpServers"].get("codegraph").is_some(),
            "codegraph entry must be added"
        );
        let cg_args = config["mcpServers"]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args");
        assert!(
            cg_args.iter().any(|a| a == &json!("${workspaceFolder}")),
            "global codegraph entry uses ${{workspaceFolder}}"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn create_dir_all_makes_file_on_fresh_home() {
        // Given a brand-new temp home (no .trae-server, desktop mode)
        let (mut ctx, base) = temp_ctx("freshhome");
        ctx.xdg_config_home = Some(ctx.home.join(".config"));
        let file = mcp_json_path(&ctx, Location::Global);
        assert!(!file.exists(), "precondition: file absent");

        // When a global install runs
        let result = TraeTarget.install(
            &ctx,
            Location::Global,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );

        // Then the parent dir was created and the file now exists (Created)
        assert_eq!(result.files[0].action, FileAction::Created);
        assert!(
            file.exists(),
            "file must exist after install: {}",
            file.display()
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_removes_only_codegraph() {
        // Given a server-mode mcp.json with github + codegraph
        let (ctx, base) = temp_ctx("uninstall");
        let server = ctx.home.join(".trae-server");
        fs::create_dir_all(&server).unwrap();
        let file = server.join("data").join("Machine").join("mcp.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  \"mcpServers\": {\n    \"github\": { \"command\": \"gh-mcp\" },\n    \"codegraph\": { \"command\": \"codegraph\" }\n  }\n}\n",
        )
        .unwrap();

        // When uninstalling
        let result = TraeTarget.uninstall(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::Removed);

        // Then github survives and codegraph is gone
        let config = read_json_file(&file);
        assert!(
            config["mcpServers"].get("github").is_some(),
            "github survives"
        );
        assert!(
            config["mcpServers"].get("codegraph").is_none(),
            "codegraph removed"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_reports_not_found_when_absent() {
        let (ctx, base) = temp_ctx("uninstall-absent");
        let result = TraeTarget.uninstall(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn skill_dir_global_ends_with_trae_user_skills() {
        let (mut ctx, base) = temp_ctx("skillglobal");
        ctx.xdg_config_home = Some(ctx.home.join(".config"));
        let dir = TraeTarget.skill_dir(&ctx, Location::Global).unwrap();
        assert!(dir.ends_with("Trae/User/skills"), "got {}", dir.display());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn skill_dir_local_ends_with_dot_trae_skills() {
        let (ctx, base) = temp_ctx("skilllocal");
        let dir = TraeTarget.skill_dir(&ctx, Location::Local).unwrap();
        assert!(dir.ends_with(".trae/skills"), "got {}", dir.display());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn supports_skills_both_locations() {
        assert!(TraeTarget.supports_skills(Location::Global));
        assert!(TraeTarget.supports_skills(Location::Local));
    }

    fn opts() -> InstallOptions {
        InstallOptions {
            auto_allow: false,
            front_load_hook: false,
        }
    }

    #[test]
    fn detect_reflects_presence_both_locations() {
        let (mut ctx, base) = temp_ctx("detect");
        ctx.xdg_config_home = Some(ctx.home.join(".config"));

        let global_before = TraeTarget.detect(&ctx, Location::Global);
        assert!(!global_before.installed);
        assert!(!global_before.already_configured);

        TraeTarget.install(&ctx, Location::Global, opts());
        let global_after = TraeTarget.detect(&ctx, Location::Global);
        assert!(global_after.installed);
        assert!(global_after.already_configured);

        let local_before = TraeTarget.detect(&ctx, Location::Local);
        assert!(!local_before.installed);
        TraeTarget.install(&ctx, Location::Local, opts());
        let local_after = TraeTarget.detect(&ctx, Location::Local);
        assert!(local_after.installed);
        assert!(local_after.already_configured);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn local_install_writes_dot_trae_with_project_notes() {
        let (ctx, base) = temp_ctx("local-install");
        let result = TraeTarget.install(&ctx, Location::Local, opts());
        assert_eq!(result.files[0].action, FileAction::Created);
        assert!(result.notes.iter().any(|n| n.contains("project-level MCP")));
        let file = mcp_json_path(&ctx, Location::Local);
        assert!(file.exists());
        let config = read_json_file(&file);
        assert!(config["mcpServers"]["codegraph"].is_object());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn global_install_notes_mention_workspace_folder() {
        let (mut ctx, base) = temp_ctx("global-notes");
        ctx.xdg_config_home = Some(ctx.home.join(".config"));
        let result = TraeTarget.install(&ctx, Location::Global, opts());
        assert!(
            result
                .notes
                .iter()
                .any(|n| n.contains("${workspaceFolder}"))
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn install_skips_unparseable_config() {
        let (ctx, base) = temp_ctx("unparseable");
        let file = mcp_json_path(&ctx, Location::Local);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let corrupt = "{ not json";
        fs::write(&file, corrupt).unwrap();
        let entry = write_mcp_entry(&ctx, Location::Local);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&file).unwrap(), corrupt);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn install_is_idempotent() {
        let (ctx, base) = temp_ctx("idempotent");
        let file = mcp_json_path(&ctx, Location::Local);
        TraeTarget.install(&ctx, Location::Local, opts());
        let first = fs::read_to_string(&file).unwrap();
        TraeTarget.install(&ctx, Location::Local, opts());
        assert_eq!(fs::read_to_string(&file).unwrap(), first);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn print_config_shows_mcp_servers_and_path() {
        let (mut ctx, base) = temp_ctx("print");
        ctx.xdg_config_home = Some(ctx.home.join(".config"));
        let out = TraeTarget.print_config(&ctx, Location::Global);
        assert!(out.contains("mcpServers"));
        assert!(out.contains("--path"));
        assert!(out.contains("${workspaceFolder}"));
        assert!(out.contains("mcp.json"));

        let local = TraeTarget.print_config(&ctx, Location::Local);
        assert!(local.replace('\\', "/").contains(".trae/mcp.json"));
        let _ = fs::remove_dir_all(base);
    }
}
