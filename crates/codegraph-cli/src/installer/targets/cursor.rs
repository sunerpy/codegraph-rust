//! Cursor target. Ports `upstream installer/targets/cursor.ts`.
//!
//! Writes the MCP entry to `~/.cursor/mcp.json` (global) or `./.cursor/mcp.json`
//! (local). Cursor needs an explicit `--path` arg because it launches MCP
//! subprocesses with a non-workspace cwd and no `rootUri` (cursor.ts:13-31):
//! local uses the absolute project path, global uses `${workspaceFolder}`.
//! No permissions concept; instructions are no longer written (a stale rules
//! file is swept on install for self-heal).

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::super::shared::{
    CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, ConfigRead, mcp_server_config,
    read_config_file, read_json_file, remove_codegraph_from_mcp_servers, to_upstream_json,
    upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::claude::upsert_mcp_server;

pub struct CursorTarget;

const MDC_FRONTMATTER: &str =
    "---\ndescription: CodeGraph MCP usage guide — when to use which tool\nalwaysApply: true\n---";

fn mcp_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => ctx.home.join(".cursor").join("mcp.json"),
        Location::Local => ctx.cwd.join(".cursor").join("mcp.json"),
    }
}
fn rules_path(ctx: &InstallContext) -> PathBuf {
    ctx.cwd.join(".cursor").join("rules").join("codegraph.mdc")
}

// Ports buildCursorMcpConfig (cursor.ts:171).
fn build_cursor_mcp_config(ctx: &InstallContext, loc: Location) -> Value {
    let path_arg = match loc {
        Location::Local => ctx.cwd.to_string_lossy().to_string(),
        Location::Global => "${workspaceFolder}".to_string(),
    };
    let mut base = mcp_server_config();
    if let Some(args) = base.get_mut("args").and_then(|a| a.as_array_mut()) {
        args.push(json!("--path"));
        args.push(json!(path_arg));
    }
    base
}

impl AgentTarget for CursorTarget {
    fn id(&self) -> TargetId {
        TargetId::Cursor
    }
    fn display_name(&self) -> &'static str {
        "Cursor"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let mcp_path = mcp_json_path(ctx, loc);
        let config = read_json_file(&mcp_path);
        let already_configured = config
            .get("mcpServers")
            .and_then(|s| s.get("codegraph"))
            .is_some();
        let installed = match loc {
            Location::Global => ctx.home.join(".cursor").exists(),
            Location::Local => ctx.cwd.join(".cursor").exists(),
        };
        DetectionResult {
            installed,
            already_configured,
        }
    }

    // Ports cursorTarget.install (cursor.ts:108).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let mut files = vec![write_mcp_entry(ctx, loc)];
        if loc == Location::Local {
            let cleanup = remove_rules_entry(ctx);
            if cleanup.action == FileAction::Removed {
                files.push(cleanup);
            }
        }
        WriteResult {
            files,
            notes: vec!["Restart Cursor for MCP changes to take effect.".to_string()],
        }
    }

    // Ports cursorTarget.uninstall (cursor.ts:128).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mut files = Vec::new();
        let mcp_path = mcp_json_path(ctx, loc);
        let mut config = read_json_file(&mcp_path);
        if remove_codegraph_from_mcp_servers(&mut config) {
            let _ = write_json_file(&mcp_path, &config);
            files.push(FileWrite {
                path: mcp_path,
                action: FileAction::Removed,
            });
        } else {
            files.push(FileWrite {
                path: mcp_path,
                action: FileAction::NotFound,
            });
        }
        if loc == Location::Local {
            files.push(remove_rules_entry(ctx));
        }
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports cursorTarget.printConfig (cursor.ts:151).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let target = mcp_json_path(ctx, loc);
        let snippet = to_upstream_json(
            &json!({ "mcpServers": { "codegraph": build_cursor_mcp_config(ctx, loc) } }),
        );
        format!("# Add to {}\n\n{snippet}\n", target.display())
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }
    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        let parent = match loc {
            Location::Global => ctx.home.join(".cursor").join("skills"),
            Location::Local => ctx.cwd.join(".cursor").join("skills"),
        };
        Some(parent)
    }
}

// Ports writeMcpEntry (cursor.ts:177).
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = mcp_json_path(ctx, loc);
    let after = build_cursor_mcp_config(ctx, loc);
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

// Ports removeRulesEntry (cursor.ts:208).
fn remove_rules_entry(ctx: &InstallContext) -> FileWrite {
    let file = rules_path(ctx);
    if !file.exists() {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    }
    let Ok(content) = fs::read_to_string(&file) else {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    };
    let our_frontmatter = MDC_FRONTMATTER.trim();

    if let (Some(start_idx), Some(end_idx)) = (
        content.find(CODEGRAPH_SECTION_START),
        content.find(CODEGRAPH_SECTION_END),
    ) && end_idx > start_idx
    {
        let before = content[..start_idx].trim_end();
        let after = content[end_idx + CODEGRAPH_SECTION_END.len()..].trim_start();
        let sep = if !before.is_empty() && !after.is_empty() {
            "\n\n"
        } else {
            ""
        };
        let remainder = format!("{before}{sep}{after}");
        let remainder = remainder.trim();
        if remainder.is_empty() || remainder == our_frontmatter {
            let _ = fs::remove_file(&file);
        } else {
            let _ = super::super::shared::atomic_write_file(&file, &format!("{remainder}\n"));
        }
        return FileWrite {
            path: file,
            action: FileAction::Removed,
        };
    }

    if content.trim() == our_frontmatter {
        let _ = fs::remove_file(&file);
        return FileWrite {
            path: file,
            action: FileAction::Removed,
        };
    }

    FileWrite {
        path: file,
        action: FileAction::NotFound,
    }
}

pub static CURSOR_TARGET: CursorTarget = CursorTarget;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> InstallContext {
        InstallContext {
            home: PathBuf::from("/home/u"),
            cwd: PathBuf::from("/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    struct TempCursor {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempCursor {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-cursor-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&base).unwrap();
            let ctx = InstallContext {
                home: base.join("home"),
                cwd: base.join("cwd"),
                app_data: None,
                xdg_config_home: None,
                hermes_home: None,
            };
            fs::create_dir_all(&ctx.cwd).unwrap();
            fs::create_dir_all(&ctx.home).unwrap();
            Self { base, ctx }
        }
        fn read(&self, p: &PathBuf) -> Value {
            serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
        }
    }

    impl Drop for TempCursor {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn opts() -> InstallOptions {
        InstallOptions {
            auto_allow: false,
            front_load_hook: false,
        }
    }

    fn args_of(json: &Value) -> Vec<Value> {
        json["mcpServers"]["codegraph"]["args"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn local_install_injects_absolute_path_arg() {
        let fx = TempCursor::new("local");
        let target = CursorTarget;
        let mcp = mcp_json_path(&fx.ctx, Location::Local);

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(!detect.installed);

        target.install(&fx.ctx, Location::Local, opts());
        let args = args_of(&fx.read(&mcp));
        assert_eq!(args[0], "serve");
        assert_eq!(args[1], "--mcp");
        assert_eq!(args[2], "--path");
        assert_eq!(args[3], fx.ctx.cwd.to_string_lossy().as_ref());

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(detect.installed);
        assert!(detect.already_configured);
    }

    #[test]
    fn global_install_uses_workspace_folder_placeholder() {
        let fx = TempCursor::new("global");
        let target = CursorTarget;
        let mcp = mcp_json_path(&fx.ctx, Location::Global);
        target.install(&fx.ctx, Location::Global, opts());
        let args = args_of(&fx.read(&mcp));
        assert_eq!(args[3], "${workspaceFolder}");
    }

    #[test]
    fn install_is_idempotent_preserving_siblings() {
        let fx = TempCursor::new("idempotent");
        let target = CursorTarget;
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        fs::write(
            &mcp,
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();

        target.install(&fx.ctx, Location::Local, opts());
        let first = fs::read_to_string(&mcp).unwrap();
        target.install(&fx.ctx, Location::Local, opts());
        assert_eq!(fs::read_to_string(&mcp).unwrap(), first);
        let json = fx.read(&mcp);
        assert!(json["mcpServers"]["other"].is_object());
        assert!(json["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn uninstall_removes_entry_reports_removed() {
        let fx = TempCursor::new("uninstall");
        let target = CursorTarget;
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        target.install(&fx.ctx, Location::Local, opts());
        let result = target.uninstall(&fx.ctx, Location::Local);
        assert_eq!(result.files[0].action, FileAction::Removed);
        let json = fx.read(&mcp);
        assert!(json.get("mcpServers").is_none());
    }

    #[test]
    fn uninstall_missing_is_not_found() {
        let fx = TempCursor::new("uninstall-missing");
        let target = CursorTarget;
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
    }

    #[test]
    fn install_skips_unparseable_config() {
        let fx = TempCursor::new("unparseable");
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        let corrupt = "{ not json";
        fs::write(&mcp, corrupt).unwrap();
        let entry = write_mcp_entry(&fx.ctx, Location::Local);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&mcp).unwrap(), corrupt);
    }

    #[test]
    fn install_local_sweeps_stale_rules_file() {
        let fx = TempCursor::new("sweep-rules");
        let target = CursorTarget;
        let rules = rules_path(&fx.ctx);
        fs::create_dir_all(rules.parent().unwrap()).unwrap();
        fs::write(
            &rules,
            format!(
                "{MDC_FRONTMATTER}\n\n{CODEGRAPH_SECTION_START}\nbody\n{CODEGRAPH_SECTION_END}\n"
            ),
        )
        .unwrap();

        let result = target.install(&fx.ctx, Location::Local, opts());
        assert!(
            result.files.iter().any(|f| f.action == FileAction::Removed),
            "stale rules swept"
        );
        assert!(!rules.exists(), "rules file removed when only our content");
    }

    #[test]
    fn remove_rules_entry_preserves_user_body() {
        let fx = TempCursor::new("rules-keep");
        let rules = rules_path(&fx.ctx);
        fs::create_dir_all(rules.parent().unwrap()).unwrap();
        fs::write(
            &rules,
            format!("user rule text\n\n{CODEGRAPH_SECTION_START}\nours\n{CODEGRAPH_SECTION_END}\n"),
        )
        .unwrap();
        let result = remove_rules_entry(&fx.ctx);
        assert_eq!(result.action, FileAction::Removed);
        let remaining = fs::read_to_string(&rules).unwrap();
        assert!(remaining.contains("user rule text"));
        assert!(!remaining.contains(CODEGRAPH_SECTION_START));
    }

    #[test]
    fn remove_rules_entry_deletes_bare_frontmatter_only_file() {
        let fx = TempCursor::new("rules-frontmatter");
        let rules = rules_path(&fx.ctx);
        fs::create_dir_all(rules.parent().unwrap()).unwrap();
        fs::write(&rules, format!("{MDC_FRONTMATTER}\n")).unwrap();
        let result = remove_rules_entry(&fx.ctx);
        assert_eq!(result.action, FileAction::Removed);
        assert!(!rules.exists());
    }

    #[test]
    fn remove_rules_entry_missing_is_not_found() {
        let fx = TempCursor::new("rules-missing");
        let result = remove_rules_entry(&fx.ctx);
        assert_eq!(result.action, FileAction::NotFound);
    }

    #[test]
    fn remove_rules_entry_foreign_file_is_not_found() {
        let fx = TempCursor::new("rules-foreign");
        let rules = rules_path(&fx.ctx);
        fs::create_dir_all(rules.parent().unwrap()).unwrap();
        fs::write(&rules, "totally unrelated rule\n").unwrap();
        let result = remove_rules_entry(&fx.ctx);
        assert_eq!(result.action, FileAction::NotFound);
        assert!(rules.exists());
    }

    #[test]
    fn print_config_shows_path_arg() {
        let target = CursorTarget;
        let ctx = ctx();
        let out = target.print_config(&ctx, Location::Global);
        assert!(out.contains("mcpServers"));
        assert!(out.contains("--path"));
        assert!(out.contains("${workspaceFolder}"));
        assert!(out.replace('\\', "/").contains(".cursor/mcp.json"));
    }

    #[test]
    fn supports_skills_both_locations() {
        let t = CursorTarget;
        assert!(t.supports_skills(Location::Global));
        assert!(t.supports_skills(Location::Local));
    }

    #[test]
    fn global_skill_dir_ends_cursor_skills() {
        let t = CursorTarget;
        let dir = t.skill_dir(&ctx(), Location::Global).unwrap();
        assert!(dir.ends_with(".cursor/skills"), "got {}", dir.display());
    }

    #[test]
    fn local_skill_dir_ends_cursor_skills() {
        let t = CursorTarget;
        let dir = t.skill_dir(&ctx(), Location::Local).unwrap();
        assert!(dir.ends_with(".cursor/skills"), "got {}", dir.display());
    }
}
