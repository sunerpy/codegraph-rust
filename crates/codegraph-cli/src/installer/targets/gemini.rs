//! Gemini CLI target. Ports `upstream installer/targets/gemini.ts`.
//!
//! Writes the MCP entry to `~/.gemini/settings.json` (global) or
//! `./.gemini/settings.json` (local) under `mcpServers.codegraph`, and the
//! instructions block to `~/.gemini/GEMINI.md` (global) or `./GEMINI.md` (local
//! — project root, NOT under `.gemini/`). No permissions concept.

use std::path::PathBuf;

use serde_json::{json, Map};

use super::super::shared::{
    self, mcp_server_config, read_config_file, read_json_file, remove_codegraph_from_mcp_servers,
    to_upstream_json, upsert_instructions_entry, upsert_nested_key_jsonc, write_json_file,
    ConfigRead, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::claude::upsert_mcp_server;

pub struct GeminiTarget;

fn config_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => ctx.home.join(".gemini"),
        Location::Local => ctx.cwd.join(".gemini"),
    }
}
fn settings_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("settings.json")
}
fn instructions_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => config_dir(ctx, Location::Global).join("GEMINI.md"),
        Location::Local => ctx.cwd.join("GEMINI.md"),
    }
}

impl AgentTarget for GeminiTarget {
    fn id(&self) -> TargetId {
        TargetId::Gemini
    }
    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let file = settings_json_path(ctx, loc);
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

    // Ports geminiTarget.install (gemini.ts:84).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let files = vec![
            write_mcp_entry(ctx, loc),
            upsert_instructions_entry(&instructions_path(ctx, loc)),
        ];
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports geminiTarget.uninstall (gemini.ts:96).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mut files = Vec::new();
        let file = settings_json_path(ctx, loc);
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
        files.push(remove_instructions_entry(ctx, loc));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports geminiTarget.printConfig (gemini.ts:120).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let target = settings_json_path(ctx, loc);
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

// Ports writeMcpEntry (gemini.ts:131).
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = settings_json_path(ctx, loc);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
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

// Ports removeInstructionsEntry (gemini.ts:156).
fn remove_instructions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = instructions_path(ctx, loc);
    let action =
        shared::remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static GEMINI_TARGET: GeminiTarget = GeminiTarget;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> InstallContext {
        InstallContext {
            home: PathBuf::from("/home/u"),
            cwd: PathBuf::from("/work/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    #[test]
    fn gemini_supports_and_locates_skills_at_both_locations() {
        // Given the Gemini target
        let target = GeminiTarget;
        let ctx = ctx();

        // Then it supports skills at both locations
        assert!(target.supports_skills(Location::Global));
        assert!(target.supports_skills(Location::Local));

        // And global skill_dir is ~/.gemini/skills
        let global = target.skill_dir(&ctx, Location::Global).unwrap();
        assert!(global.ends_with(".gemini/skills"));

        // And local skill_dir is ./.gemini/skills
        let local = target.skill_dir(&ctx, Location::Local).unwrap();
        assert!(local.ends_with(".gemini/skills"));
    }
}
