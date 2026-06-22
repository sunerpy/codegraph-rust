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

use serde_json::{json, Map, Value};

use super::super::shared::{
    mcp_server_config, read_config_file, read_json_file, remove_codegraph_from_mcp_servers,
    to_upstream_json, upsert_nested_key_jsonc, write_json_file, ConfigRead, CODEGRAPH_SECTION_END,
    CODEGRAPH_SECTION_START,
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
    ) {
        if end_idx > start_idx {
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
