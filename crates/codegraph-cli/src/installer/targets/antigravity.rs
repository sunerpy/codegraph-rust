//! Google Antigravity IDE target.
//! Ports `upstream installer/targets/antigravity.ts`.
//!
//! Writes the MCP entry to the unified `~/.gemini/config/mcp_config.json` (when
//! the `.migrated` marker or unified file exists) or the legacy
//! `~/.gemini/antigravity/mcp_config.json`, under `mcpServers.codegraph`.
//! The entry OMITS the `type` field (Antigravity rejects it) and on macOS
//! resolves `codegraph` to an absolute path; elsewhere it uses the bare command.
//! Global-only; shares GEMINI.md with the gemini target (not touched here).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::super::shared::{
    read_config_file, read_json_file, remove_codegraph_from_mcp_servers, to_upstream_json,
    upsert_nested_key_jsonc, write_json_file, ConfigRead,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::claude::upsert_mcp_server;

pub struct AntigravityTarget;

fn unified_config_dir(ctx: &InstallContext) -> PathBuf {
    ctx.home.join(".gemini").join("config")
}
fn unified_mcp_config_path(ctx: &InstallContext) -> PathBuf {
    unified_config_dir(ctx).join("mcp_config.json")
}
fn legacy_config_dir(ctx: &InstallContext) -> PathBuf {
    ctx.home.join(".gemini").join("antigravity")
}
fn legacy_mcp_config_path(ctx: &InstallContext) -> PathBuf {
    legacy_config_dir(ctx).join("mcp_config.json")
}
fn migrated_marker_path(ctx: &InstallContext) -> PathBuf {
    unified_config_dir(ctx).join(".migrated")
}

// Ports preferredMcpConfigPath (antigravity.ts:99).
fn preferred_mcp_config_path(ctx: &InstallContext) -> PathBuf {
    if migrated_marker_path(ctx).exists() {
        return unified_mcp_config_path(ctx);
    }
    if unified_mcp_config_path(ctx).exists() {
        return unified_mcp_config_path(ctx);
    }
    legacy_mcp_config_path(ctx)
}

// Ports resolveCodegraphCommand (antigravity.ts:120): macOS-only absolute-path
// resolution. On Linux/Windows GUI apps inherit PATH, so the bare name is used.
fn resolve_codegraph_command() -> String {
    if !cfg!(target_os = "macos") {
        return "codegraph".to_string();
    }
    let output = std::process::Command::new("/bin/bash")
        .args(["-c", "command -v codegraph || which codegraph"])
        .output();
    if let Ok(out) = output {
        let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !resolved.is_empty() && std::path::Path::new(&resolved).exists() {
            return resolved;
        }
    }
    "codegraph".to_string()
}

// Ports buildAntigravityEntry (antigravity.ts:142): no `type` field.
fn build_antigravity_entry() -> Value {
    json!({
        "command": resolve_codegraph_command(),
        "args": ["serve", "--mcp"],
    })
}

impl AgentTarget for AntigravityTarget {
    fn id(&self) -> TargetId {
        TargetId::Antigravity
    }
    fn display_name(&self) -> &'static str {
        "Antigravity IDE"
    }
    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult::default();
        }
        let file = preferred_mcp_config_path(ctx);
        let config = read_json_file(&file);
        let already_configured = config
            .get("mcpServers")
            .and_then(|s| s.get("codegraph"))
            .is_some();
        let installed =
            unified_config_dir(ctx).exists() || legacy_config_dir(ctx).exists() || file.exists();
        DetectionResult {
            installed,
            already_configured,
        }
    }

    // Ports antigravityTarget.install (antigravity.ts:175).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return WriteResult {
                files: Vec::new(),
                notes: vec![
                    "Antigravity IDE has no project-local config — re-run with --location=global."
                        .to_string(),
                ],
            };
        }
        let mut files = vec![write_mcp_entry(ctx)];
        if let Some(cleanup) = cleanup_legacy_entry(ctx) {
            files.push(cleanup);
        }
        WriteResult {
            files,
            notes: vec!["Restart Antigravity for MCP changes to take effect.".to_string()],
        }
    }

    // Ports antigravityTarget.uninstall (antigravity.ts:195).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return WriteResult::default();
        }
        let mut files = Vec::new();
        let preferred = preferred_mcp_config_path(ctx);
        files.push(remove_codegraph_from_file(&preferred));

        let other = if preferred == unified_mcp_config_path(ctx) {
            legacy_mcp_config_path(ctx)
        } else {
            unified_mcp_config_path(ctx)
        };
        if preferred != other {
            let other_result = remove_codegraph_from_file(&other);
            if other_result.action == FileAction::Removed {
                files.push(other_result);
            }
        }
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports antigravityTarget.printConfig (antigravity.ts:219).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        if loc != Location::Global {
            return "# Antigravity IDE has no project-local config — use --location=global.\n"
                .to_string();
        }
        let file = preferred_mcp_config_path(ctx);
        let snippet =
            to_upstream_json(&json!({ "mcpServers": { "codegraph": build_antigravity_entry() } }));
        format!("# Add to {}\n\n{snippet}\n", file.display())
    }
}

// Ports writeMcpEntry (antigravity.ts:234).
fn write_mcp_entry(ctx: &InstallContext) -> FileWrite {
    let file = preferred_mcp_config_path(ctx);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let after = build_antigravity_entry();
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

// Ports cleanupLegacyEntry (antigravity.ts:261).
fn cleanup_legacy_entry(ctx: &InstallContext) -> Option<FileWrite> {
    if preferred_mcp_config_path(ctx) != unified_mcp_config_path(ctx) {
        return None;
    }
    let legacy = legacy_mcp_config_path(ctx);
    if !legacy.exists() {
        return None;
    }
    let mut config = read_json_file(&legacy);
    config.get("mcpServers").and_then(|s| s.get("codegraph"))?;
    remove_codegraph_from_mcp_servers(&mut config);
    let _ = write_json_file(&legacy, &config);
    Some(FileWrite {
        path: legacy,
        action: FileAction::Removed,
    })
}

// Ports removeCodegraphFromFile (antigravity.ts:275): leaves an emptied `{}` in
// place (Antigravity manages the file; a stray empty file is less surprising).
fn remove_codegraph_from_file(file: &Path) -> FileWrite {
    if !file.exists() {
        return FileWrite {
            path: file.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    let mut config = read_json_file(file);
    if config
        .get("mcpServers")
        .and_then(|s| s.get("codegraph"))
        .is_none()
    {
        return FileWrite {
            path: file.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    remove_codegraph_from_mcp_servers(&mut config);
    let _ = write_json_file(file, &config);
    FileWrite {
        path: file.to_path_buf(),
        action: FileAction::Removed,
    }
}

pub static ANTIGRAVITY_TARGET: AntigravityTarget = AntigravityTarget;
