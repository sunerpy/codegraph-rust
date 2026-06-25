//! Kiro CLI / IDE target. Ports `upstream installer/targets/kiro.ts`.
//!
//! Writes the MCP entry to `<dir>/.kiro/settings/mcp.json` under
//! `mcpServers.codegraph`, and (formerly) a steering doc at
//! `<dir>/.kiro/steering/codegraph.md` — now only swept on install for
//! self-heal and deleted on uninstall. No permissions concept.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map};

use super::super::shared::{
    mcp_server_config, read_config_file, read_json_file, remove_codegraph_from_mcp_servers,
    to_upstream_json, upsert_nested_key_jsonc, write_json_file, ConfigRead,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::claude::upsert_mcp_server;

pub struct KiroTarget;

fn config_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => ctx.home.join(".kiro"),
        Location::Local => ctx.cwd.join(".kiro"),
    }
}
fn mcp_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("settings").join("mcp.json")
}
fn steering_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("steering").join("codegraph.md")
}

impl AgentTarget for KiroTarget {
    fn id(&self) -> TargetId {
        TargetId::Kiro
    }
    fn display_name(&self) -> &'static str {
        "Kiro"
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
        let installed = config_dir(ctx, loc).exists() || file.exists();
        DetectionResult {
            installed,
            already_configured,
        }
    }

    // Ports kiroTarget.install (kiro.ts:74).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let mut files = vec![write_mcp_entry(ctx, loc)];
        let cleanup = remove_steering_entry(ctx, loc);
        if cleanup.action == FileAction::Removed {
            files.push(cleanup);
        }
        WriteResult {
            files,
            notes: vec![
                "Restart Kiro for MCP changes to take effect.".to_string(),
                "Kiro IDE: also enable MCP in Settings (search \"MCP\" → \"Enabled\"). Kiro CLI users can skip this step."
                    .to_string(),
            ],
        }
    }

    // Ports kiroTarget.uninstall (kiro.ts:99).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mut files = Vec::new();
        let file = mcp_json_path(ctx, loc);
        let mut config = read_json_file(&file);
        if remove_codegraph_from_mcp_servers(&mut config) {
            let _ = write_json_file(&file, &config);
            files.push(FileWrite {
                path: file,
                action: FileAction::Removed,
            });
        } else {
            files.push(FileWrite {
                path: file,
                action: FileAction::NotFound,
            });
        }
        files.push(remove_steering_entry(ctx, loc));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports kiroTarget.printConfig (kiro.ts:120).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let target = mcp_json_path(ctx, loc);
        let snippet =
            to_upstream_json(&json!({ "mcpServers": { "codegraph": mcp_server_config() } }));
        format!("# Add to {}\n\n{snippet}\n", target.display())
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        Some(config_dir(ctx, loc).join("skills"))
    }
}

// Ports writeMcpEntry (kiro.ts:131).
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = mcp_json_path(ctx, loc);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let after = mcp_server_config();
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

// Ports removeSteeringEntry (kiro.ts:158).
fn remove_steering_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = steering_path(ctx, loc);
    if !file.exists() {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    }
    let _ = fs::remove_file(&file);
    FileWrite {
        path: file,
        action: FileAction::Removed,
    }
}

pub static KIRO_TARGET: KiroTarget = KiroTarget;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> InstallContext {
        InstallContext {
            home: PathBuf::from("/home/user"),
            cwd: PathBuf::from("/work/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    #[test]
    fn kiro_supports_and_locates_skills_at_both_locations() {
        // Given the Kiro target
        let target = KiroTarget;
        let ctx = ctx();

        // Then it supports skills at both locations
        assert!(target.supports_skills(Location::Global));
        assert!(target.supports_skills(Location::Local));

        // And the PARENT skill dir is `<root>/.kiro/skills` (engine appends codegraph/SKILL.md)
        let global = target.skill_dir(&ctx, Location::Global).unwrap();
        assert!(global.ends_with(".kiro/skills"));
        assert_eq!(global, PathBuf::from("/home/user/.kiro/skills"));

        let local = target.skill_dir(&ctx, Location::Local).unwrap();
        assert!(local.ends_with(".kiro/skills"));
        assert_eq!(local, PathBuf::from("/work/proj/.kiro/skills"));
    }
}
