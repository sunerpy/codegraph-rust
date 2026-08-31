//! OpenAI Codex CLI target. Ports `upstream installer/targets/codex.ts`.
//!
//! Writes the MCP entry as the dotted-key table `[mcp_servers.codegraph]` and
//! installs the managed instructions block:
//! - global: `~/.codex/config.toml` + `~/.codex/AGENTS.md`
//! - local: `<project>/.codex/config.toml` + `<project>/AGENTS.md`

use std::fs;
use std::path::PathBuf;

use super::super::shared::{
    self, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, TomlUpsert, TomlValue, atomic_write_file,
    build_toml_table, contains_toml_table, mcp_server_config, remove_toml_table,
    upsert_instructions_entry, upsert_toml_table,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct CodexTarget;

const TOML_HEADER: &str = "mcp_servers.codegraph";

fn config_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => ctx.home.join(".codex"),
        Location::Local => ctx.cwd.join(".codex"),
    }
}
fn toml_config_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("config.toml")
}
fn instructions_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => config_dir(ctx, loc).join("AGENTS.md"),
        Location::Local => ctx.cwd.join("AGENTS.md"),
    }
}

fn trust_note(ctx: &InstallContext) -> String {
    format!(
        "Codex applies {} only after this project is marked trusted. Trust the project in Codex to activate its local MCP configuration.",
        toml_config_path(ctx, Location::Local).display()
    )
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
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let toml_path = toml_config_path(ctx, loc);
        let already_configured = fs::read_to_string(&toml_path)
            .map(|content| contains_toml_table(&content, TOML_HEADER))
            .unwrap_or(false);
        DetectionResult {
            installed: config_dir(ctx, loc).exists() || toml_path.exists(),
            already_configured,
        }
    }

    // Ports codexTarget.install (codex.ts:76).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let files = vec![
            write_mcp_entry(ctx, loc),
            upsert_instructions_entry(&instructions_path(ctx, loc)),
        ];
        WriteResult {
            files,
            notes: (loc == Location::Local)
                .then(|| trust_note(ctx))
                .into_iter()
                .collect(),
        }
    }

    // Ports codexTarget.uninstall (codex.ts:95).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mut files = Vec::new();
        let toml_path = toml_config_path(ctx, loc);
        if let Ok(content) = fs::read_to_string(&toml_path) {
            let line_ending = if content.contains("\r\n") {
                "\r\n"
            } else {
                "\n"
            };
            let (next_content, removed) = remove_toml_table(&content, TOML_HEADER);
            if removed {
                if next_content.trim().is_empty() {
                    let _ = fs::remove_file(&toml_path);
                } else {
                    let _ = atomic_write_file(
                        &toml_path,
                        &format!("{}{line_ending}", next_content.trim_end()),
                    );
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
        files.push(remove_instructions_entry(ctx, loc));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports codexTarget.printConfig (codex.ts:122).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        format!(
            "# Add to {}\n\n{}\n",
            toml_config_path(ctx, loc).display(),
            build_codegraph_block()
        )
    }

    fn managed_instructions_path(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        Some(instructions_path(ctx, loc))
    }

    // Codex + Antigravity LOCAL both target `.agents/skills`; co-installing them
    // is idempotent (same content, same hash).
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
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = toml_config_path(ctx, loc);
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
fn remove_instructions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = instructions_path(ctx, loc);
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
        assert!(t.supports_location(Location::Local));
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
        let toml = toml_config_path(&fx.ctx, Location::Global);

        let before = target.detect(&fx.ctx, Location::Global);
        assert!(!before.installed);
        assert!(!before.already_configured);

        target.install(&fx.ctx, Location::Global, opts());
        let content = fs::read_to_string(&toml).unwrap();
        assert!(content.contains("[mcp_servers.codegraph]"));
        assert!(content.contains("command = \"codegraph\""));
        assert!(content.contains("args = [\"serve\", \"--mcp\"]"));
        assert!(instructions_path(&fx.ctx, Location::Global).exists());

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
        let toml = toml_config_path(&fx.ctx, Location::Global);
        target.install(&fx.ctx, Location::Global, opts());
        let first = fs::read_to_string(&toml).unwrap();
        let again = write_mcp_entry(&fx.ctx, Location::Global);
        assert_eq!(again.action, FileAction::Unchanged);
        assert_eq!(fs::read_to_string(&toml).unwrap(), first);
    }

    #[test]
    fn detect_uses_toml_lexer_for_real_and_decoy_headers() {
        let fx = TempCodex::new("detect-toml-header");
        let target = CodexTarget;
        let toml = toml_config_path(&fx.ctx, Location::Global);
        fs::create_dir_all(toml.parent().unwrap()).unwrap();
        fs::write(
            &toml,
            concat!(
                "[settings]\n",
                "instructions = \"\"\"\n",
                "  [mcp_servers.codegraph]\n",
                "\"\"\"\n",
            ),
        )
        .unwrap();

        assert!(!target.detect(&fx.ctx, Location::Global).already_configured);

        fs::write(
            &toml,
            "[settings]\nenabled = true\n\n  [mcp_servers.codegraph]\n  command = \"old\"\n",
        )
        .unwrap();
        assert!(target.detect(&fx.ctx, Location::Global).already_configured);
    }

    #[test]
    fn local_install_uses_project_layout_and_reports_trust_requirement() {
        let fx = TempCodex::new("local-layout");
        let target = CodexTarget;
        let install = target.install(&fx.ctx, Location::Local, opts());
        assert_eq!(install.files.len(), 2);
        assert!(install.notes.join(" ").contains("trusted"));
        let toml = toml_config_path(&fx.ctx, Location::Local);
        assert_eq!(toml, fx.ctx.cwd.join(".codex/config.toml"));
        assert!(
            fs::read_to_string(&toml)
                .unwrap()
                .contains("[mcp_servers.codegraph]")
        );
        assert_eq!(
            instructions_path(&fx.ctx, Location::Local),
            fx.ctx.cwd.join("AGENTS.md")
        );
        assert!(instructions_path(&fx.ctx, Location::Local).exists());
        assert!(
            !toml_config_path(&fx.ctx, Location::Global).exists(),
            "local install must not touch the global config"
        );
        assert!(target.detect(&fx.ctx, Location::Local).already_configured);
        assert!(
            target
                .print_config(&fx.ctx, Location::Local)
                .contains(&toml.display().to_string())
        );
    }

    #[test]
    fn local_uninstall_leaves_global_install_intact() {
        let fx = TempCodex::new("local-uninstall");
        let target = CodexTarget;
        target.install(&fx.ctx, Location::Global, opts());
        target.install(&fx.ctx, Location::Local, opts());

        let removed = target.uninstall(&fx.ctx, Location::Local);
        assert!(
            removed
                .files
                .iter()
                .any(|file| file.action == FileAction::Removed)
        );
        assert!(!target.detect(&fx.ctx, Location::Local).already_configured);
        assert!(target.detect(&fx.ctx, Location::Global).already_configured);
        assert!(
            fs::read_to_string(toml_config_path(&fx.ctx, Location::Global))
                .unwrap()
                .contains("[mcp_servers.codegraph]")
        );
    }

    #[test]
    fn local_round_trip_preserves_user_toml_and_agents_text() {
        let fx = TempCodex::new("local-preserve");
        let target = CodexTarget;
        let toml = toml_config_path(&fx.ctx, Location::Local);
        let agents = instructions_path(&fx.ctx, Location::Local);
        fs::create_dir_all(toml.parent().unwrap()).unwrap();
        fs::create_dir_all(&fx.ctx.cwd).unwrap();
        fs::write(
            &toml,
            "# user comment\n[features]\nexperimental = true\n\n[mcp_servers.other]\ncommand = \"other\"\n",
        )
        .unwrap();
        fs::write(&agents, "# User rules\n\nKeep this text.\n").unwrap();

        target.install(&fx.ctx, Location::Local, opts());
        let installed_toml = fs::read_to_string(&toml).unwrap();
        let installed_agents = fs::read_to_string(&agents).unwrap();
        assert!(installed_toml.contains("# user comment"));
        assert!(installed_toml.contains("[features]"));
        assert!(installed_toml.contains("[mcp_servers.other]"));
        assert!(installed_toml.contains("[mcp_servers.codegraph]"));
        assert!(installed_agents.contains("# User rules"));
        assert!(installed_agents.contains("Keep this text."));
        assert!(installed_agents.contains(CODEGRAPH_SECTION_START));

        target.uninstall(&fx.ctx, Location::Local);
        let uninstalled_toml = fs::read_to_string(&toml).unwrap();
        let uninstalled_agents = fs::read_to_string(&agents).unwrap();
        assert!(uninstalled_toml.contains("# user comment"));
        assert!(uninstalled_toml.contains("[features]"));
        assert!(uninstalled_toml.contains("[mcp_servers.other]"));
        assert!(!uninstalled_toml.contains("[mcp_servers.codegraph]"));
        assert!(uninstalled_agents.contains("# User rules"));
        assert!(uninstalled_agents.contains("Keep this text."));
        assert!(!uninstalled_agents.contains(CODEGRAPH_SECTION_START));
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
        let toml = toml_config_path(&fx.ctx, Location::Global);
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
    fn uninstall_preserves_crlf_line_endings() {
        let fx = TempCodex::new("uninstall-crlf");
        let target = CodexTarget;
        let toml = toml_config_path(&fx.ctx, Location::Global);
        fs::create_dir_all(toml.parent().unwrap()).unwrap();
        fs::write(
            &toml,
            concat!(
                "[mcp_servers.codegraph]\r\n",
                "command = \"codegraph\"\r\n",
                "args = [\"serve\", \"--mcp\"]\r\n",
                "\r\n",
                "[mcp_servers.other]\r\n",
                "name = \"keep-crlf\"\r\n",
            ),
        )
        .unwrap();
        let expected = b"[mcp_servers.other]\r\nname = \"keep-crlf\"\r\n";

        target.uninstall(&fx.ctx, Location::Global);

        let actual = fs::read(&toml).unwrap();
        assert_eq!(
            actual,
            expected,
            "uninstall must preserve CRLF bytes; got {:?}",
            String::from_utf8_lossy(&actual)
        );
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
