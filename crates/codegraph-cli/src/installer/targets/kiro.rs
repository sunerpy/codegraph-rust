//! Kiro CLI / IDE target. Ports `upstream installer/targets/kiro.ts`.
//!
//! Writes the MCP entry to `<dir>/.kiro/settings/mcp.json` under
//! `mcpServers.codegraph`, and (formerly) a steering doc at
//! `<dir>/.kiro/steering/codegraph.md` — now only swept on install for
//! self-heal and deleted on uninstall. No permissions concept.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

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

const KIRO_GLOBAL_WHY: &str = "Kiro is project-scoped, so a global install is intentionally NOT written: Kiro CLI does not expand ${workspaceFolder} in mcp.json args, so a global --path would resolve to a literal, broken directory.";
const KIRO_GLOBAL_HOWTO: &str = "Install per project instead — cd into each project root, then run `codegraph install --target=kiro --local` (writes that project's absolute --path). Repeat for every project you open in Kiro.";

/// Build the project-local Kiro MCP entry with an explicit `--path = ctx.cwd`.
///
/// Kiro's `initialize` carries no `rootUri`/`workspaceFolders` and does not
/// advertise `capabilities.roots`, so a bare `serve --mcp` cannot discover the
/// project and degrades to home safe mode. Pinning the concrete project root
/// fixes that. Only Local installs write an entry: Kiro CLI does not expand
/// `${workspaceFolder}`, so a global `--path` variable would resolve to a
/// literal, non-existent directory (the watcher/sync then fail on it).
fn build_kiro_local_mcp_config(ctx: &InstallContext) -> Value {
    let mut base = mcp_server_config();
    if let Some(args) = base.get_mut("args").and_then(|a| a.as_array_mut()) {
        args.push(json!("--path"));
        args.push(json!(ctx.cwd.to_string_lossy().to_string()));
    }
    base
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
        let notes = match loc {
            Location::Local => vec![
                format!(
                    "CodeGraph MCP configured for project {}.",
                    ctx.cwd.display()
                ),
                "Restart Kiro for MCP changes to take effect.".to_string(),
                "Kiro IDE: also enable MCP in Settings (search \"MCP\" → \"Enabled\"). Kiro CLI users can skip this step."
                    .to_string(),
                "Each project you open in Kiro needs its own install: cd into it and re-run `codegraph install --target=kiro --local`."
                    .to_string(),
            ],
            Location::Global => vec![KIRO_GLOBAL_WHY.to_string(), KIRO_GLOBAL_HOWTO.to_string()],
        };
        WriteResult { files, notes }
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
        match loc {
            Location::Local => {
                let target = mcp_json_path(ctx, loc);
                let snippet = to_upstream_json(
                    &json!({ "mcpServers": { "codegraph": build_kiro_local_mcp_config(ctx) } }),
                );
                format!("# Add to {}\n\n{snippet}\n", target.display())
            }
            Location::Global => format!("# {KIRO_GLOBAL_WHY}\n# {KIRO_GLOBAL_HOWTO}\n"),
        }
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        Some(config_dir(ctx, loc).join("skills"))
    }
}

// Ports writeMcpEntry (kiro.ts:131).
//
// Global installs intentionally write NO entry: Kiro CLI does not expand
// ${workspaceFolder}, so a global --path is broken. A global install instead
// self-heals — it removes any stale codegraph entry a prior version wrote — and
// the caller emits per-project guidance. Only Local installs write the entry.
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = mcp_json_path(ctx, loc);
    if loc == Location::Global {
        return remove_global_codegraph_entry(file);
    }
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let after = build_kiro_local_mcp_config(ctx);
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

fn remove_global_codegraph_entry(file: PathBuf) -> FileWrite {
    let mut config = read_json_file(&file);
    if remove_codegraph_from_mcp_servers(&mut config) {
        let _ = write_json_file(&file, &config);
        FileWrite {
            path: file,
            action: FileAction::Removed,
        }
    } else {
        FileWrite {
            path: file,
            action: FileAction::Skipped,
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

    #[test]
    fn kiro_local_mcp_entry_pins_concrete_project_path() {
        // Given a local Kiro install context
        let ctx = ctx();

        // When the MCP entry is built for the local location
        let entry = build_kiro_local_mcp_config(&ctx);

        // Then args end with --path pinned to the concrete cwd
        let args = entry["args"].as_array().expect("args array");
        assert_eq!(
            args,
            &vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!("/work/proj"),
            ]
        );
    }

    #[test]
    fn kiro_global_install_writes_no_mcp_entry_and_emits_local_guidance() {
        // Given a global Kiro install into a temp HOME with no prior config
        let home = std::env::temp_dir().join(format!("cg-kiro-global-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let ctx = InstallContext {
            home: home.clone(),
            cwd: PathBuf::from("/work/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        };

        // When a global install runs
        let result = KiroTarget.install(
            &ctx,
            Location::Global,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );

        // Then no codegraph MCP entry is written to the global mcp.json
        let mcp = home.join(".kiro").join("settings").join("mcp.json");
        let written = std::fs::read_to_string(&mcp).unwrap_or_default();
        assert!(
            !written.contains("codegraph"),
            "global install must not write a codegraph entry, got: {written}"
        );
        // And the user is told to install per project
        assert!(
            result
                .notes
                .iter()
                .any(|n| n.contains("--target=kiro --local")),
            "global install must emit per-project guidance"
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
