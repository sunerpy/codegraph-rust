//! OpenAI Codex CLI target. Ports `upstream installer/targets/codex.ts`.
//!
//! Writes the MCP entry to `~/.codex/config.toml` as the dotted-key table
//! `[mcp_servers.codegraph]`, and the instructions block to `~/.codex/AGENTS.md`.
//! Codex is global-only (no project-local config concept).

use std::fs;
use std::path::PathBuf;

use super::super::shared::{
    self, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, TomlUpsert, TomlValue, atomic_write_file,
    build_toml_table, mcp_server_config, remove_toml_table, upsert_instructions_entry,
    upsert_toml_table,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct CodexTarget;

const TOML_HEADER: &str = "mcp_servers.codegraph";

fn config_dir(ctx: &InstallContext) -> PathBuf {
    ctx.home.join(".codex")
}
fn toml_config_path(ctx: &InstallContext) -> PathBuf {
    config_dir(ctx).join("config.toml")
}
fn instructions_path(ctx: &InstallContext) -> PathBuf {
    config_dir(ctx).join("AGENTS.md")
}

// Ports buildCodegraphBlock (codex.ts:136). The MCP server config command/args
// come from mcp_server_config(); the TOML table omits the `type` field (only
// command + args, as the upstream does).
fn build_codegraph_block() -> String {
    let mcp = mcp_server_config();
    let command = mcp["command"].as_str().unwrap_or("codegraph");
    let args = mcp["args"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    build_toml_table(
        TOML_HEADER,
        &[
            ("command", TomlValue::Str(command)),
            ("args", TomlValue::Array(args)),
        ],
    )
}

impl AgentTarget for CodexTarget {
    fn id(&self) -> TargetId {
        TargetId::Codex
    }
    fn display_name(&self) -> &'static str {
        "Codex CLI"
    }
    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult::default();
        }
        let toml_path = toml_config_path(ctx);
        let already_configured = fs::read_to_string(&toml_path)
            .map(|c| c.contains(&format!("[{TOML_HEADER}]")))
            .unwrap_or(false);
        DetectionResult {
            installed: config_dir(ctx).exists(),
            already_configured,
        }
    }

    // Ports codexTarget.install (codex.ts:76).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return WriteResult {
                files: Vec::new(),
                notes: vec![
                    "Codex CLI has no project-local config — re-run with --location=global to install."
                        .to_string(),
                ],
            };
        }
        let files = vec![
            write_mcp_entry(ctx),
            upsert_instructions_entry(&instructions_path(ctx)),
        ];
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports codexTarget.uninstall (codex.ts:95).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return WriteResult::default();
        }
        let mut files = Vec::new();
        let toml_path = toml_config_path(ctx);
        if let Ok(content) = fs::read_to_string(&toml_path) {
            let (next_content, removed) = remove_toml_table(&content, TOML_HEADER);
            if removed {
                if next_content.trim().is_empty() {
                    let _ = fs::remove_file(&toml_path);
                } else {
                    let _ =
                        atomic_write_file(&toml_path, &format!("{}\n", next_content.trim_end()));
                }
                files.push(FileWrite {
                    path: toml_path,
                    action: FileAction::Removed,
                });
            } else {
                files.push(FileWrite {
                    path: toml_path,
                    action: FileAction::NotFound,
                });
            }
        } else {
            files.push(FileWrite {
                path: toml_path,
                action: FileAction::NotFound,
            });
        }
        files.push(remove_instructions_entry(ctx));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports codexTarget.printConfig (codex.ts:122).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        if loc != Location::Global {
            return "# Codex CLI has no project-local config — use --location=global.\n"
                .to_string();
        }
        format!(
            "# Add to {}\n\n{}\n",
            toml_config_path(ctx).display(),
            build_codegraph_block()
        )
    }

    // Skill support is INTENTIONALLY decoupled from `supports_location`: Codex
    // MCP config is global-only, yet Codex DOES scan project-local skills, so
    // skills are gated on `supports_skills` (true for BOTH locations), never on
    // `supports_location`. Codex + Antigravity LOCAL both target `.agents/skills`
    // — co-installing them is idempotent (same content, same hash).
    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }
    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        let parent = match loc {
            Location::Global => ctx.home.join(".agents").join("skills"),
            Location::Local => ctx.cwd.join(".agents").join("skills"),
        };
        Some(parent)
    }
}

// Ports writeMcpEntry (codex.ts:144).
fn write_mcp_entry(ctx: &InstallContext) -> FileWrite {
    let file = toml_config_path(ctx);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let block = build_codegraph_block();
    let existing = fs::read_to_string(&file).unwrap_or_default();
    let created = existing.is_empty();
    let (next_content, action) = upsert_toml_table(&existing, TOML_HEADER, &block);
    if action == TomlUpsert::Unchanged {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    let _ = atomic_write_file(&file, &next_content);
    FileWrite {
        path: file,
        action: if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    }
}

// Ports removeInstructionsEntry (codex.ts:169).
fn remove_instructions_entry(ctx: &InstallContext) -> FileWrite {
    let file = instructions_path(ctx);
    let action =
        shared::remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static CODEX_TARGET: CodexTarget = CodexTarget;

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

    #[test]
    fn global_skill_dir_ends_agents_skills() {
        let t = CodexTarget;
        let dir = t.skill_dir(&ctx(), Location::Global).unwrap();
        assert!(dir.ends_with(".agents/skills"), "got {}", dir.display());
    }

    #[test]
    fn local_skill_dir_ends_agents_skills() {
        let t = CodexTarget;
        let dir = t.skill_dir(&ctx(), Location::Local).unwrap();
        assert!(dir.ends_with(".agents/skills"), "got {}", dir.display());
    }

    #[test]
    fn skills_are_decoupled_from_mcp_location() {
        let t = CodexTarget;
        assert!(t.supports_skills(Location::Local));
        assert!(t.supports_skills(Location::Global));
        assert!(!t.supports_location(Location::Local));
        assert!(t.supports_location(Location::Global));
    }

    struct TempCodex {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempCodex {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-codex-{label}-{}-{}",
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
            Self { base, ctx }
        }
    }

    impl Drop for TempCodex {
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

    #[test]
    fn install_creates_toml_table_and_instructions_then_uninstall() {
        let fx = TempCodex::new("lifecycle");
        let target = CodexTarget;
        let toml = toml_config_path(&fx.ctx);

        let before = target.detect(&fx.ctx, Location::Global);
        assert!(!before.installed);
        assert!(!before.already_configured);

        target.install(&fx.ctx, Location::Global, opts());
        let content = fs::read_to_string(&toml).unwrap();
        assert!(content.contains("[mcp_servers.codegraph]"));
        assert!(content.contains("command = \"codegraph\""));
        assert!(content.contains("args = [\"serve\", \"--mcp\"]"));
        assert!(instructions_path(&fx.ctx).exists());

        let after = target.detect(&fx.ctx, Location::Global);
        assert!(after.installed);
        assert!(after.already_configured);

        let removed = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(removed.files[0].action, FileAction::Removed);
    }

    #[test]
    fn install_is_idempotent() {
        let fx = TempCodex::new("idempotent");
        let target = CodexTarget;
        let toml = toml_config_path(&fx.ctx);
        target.install(&fx.ctx, Location::Global, opts());
        let first = fs::read_to_string(&toml).unwrap();
        let again = write_mcp_entry(&fx.ctx);
        assert_eq!(again.action, FileAction::Unchanged);
        assert_eq!(fs::read_to_string(&toml).unwrap(), first);
    }

    #[test]
    fn local_location_is_rejected() {
        let fx = TempCodex::new("local-reject");
        let target = CodexTarget;
        let install = target.install(&fx.ctx, Location::Local, opts());
        assert!(install.files.is_empty());
        assert!(install.notes[0].contains("--location=global"));

        let uninstall = target.uninstall(&fx.ctx, Location::Local);
        assert!(uninstall.files.is_empty());

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(!detect.installed);

        let printed = target.print_config(&fx.ctx, Location::Local);
        assert!(printed.contains("--location=global"));
    }

    #[test]
    fn uninstall_missing_config_is_not_found() {
        let fx = TempCodex::new("uninstall-missing");
        let target = CodexTarget;
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
    }

    #[test]
    fn uninstall_preserves_sibling_table() {
        let fx = TempCodex::new("uninstall-sibling");
        let target = CodexTarget;
        let toml = toml_config_path(&fx.ctx);
        target.install(&fx.ctx, Location::Global, opts());
        let content = fs::read_to_string(&toml).unwrap();
        fs::write(
            &toml,
            format!("{content}\n[mcp_servers.other]\ncommand = \"foo\"\n"),
        )
        .unwrap();

        target.uninstall(&fx.ctx, Location::Global);
        let content = fs::read_to_string(&toml).unwrap();
        assert!(!content.contains("[mcp_servers.codegraph]"));
        assert!(content.contains("[mcp_servers.other]"));
    }

    #[test]
    fn print_config_global_shows_toml_block() {
        let fx = TempCodex::new("print");
        let target = CodexTarget;
        let out = target.print_config(&fx.ctx, Location::Global);
        assert!(out.contains("[mcp_servers.codegraph]"));
        assert!(out.contains("config.toml"));
    }
}
