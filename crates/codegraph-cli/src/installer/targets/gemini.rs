//! Gemini CLI target. Ports `upstream installer/targets/gemini.ts`.
//!
//! Writes the MCP entry to `~/.gemini/settings.json` (global) or
//! `./.gemini/settings.json` (local) under `mcpServers.codegraph`, and the
//! instructions block to `~/.gemini/GEMINI.md` (global) or `./GEMINI.md` (local
//! — project root, NOT under `.gemini/`). No permissions concept.

use std::path::PathBuf;

use serde_json::{Map, json};

use super::super::shared::{
    self, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, ConfigRead, mcp_server_config,
    read_config_file, read_json_file, remove_codegraph_from_mcp_servers, to_upstream_json,
    upsert_instructions_entry, upsert_nested_key_jsonc, write_json_file,
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
    use std::fs;

    fn ctx() -> InstallContext {
        InstallContext {
            home: PathBuf::from("/home/u"),
            cwd: PathBuf::from("/work/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    struct TempGemini {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempGemini {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-gemini-{label}-{}-{}",
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
            Self { base, ctx }
        }
        fn read(&self, p: &PathBuf) -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
        }
    }

    impl Drop for TempGemini {
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
    fn install_global_writes_settings_and_instructions() {
        let fx = TempGemini::new("install-global");
        let target = GeminiTarget;
        let settings = settings_json_path(&fx.ctx, Location::Global);
        let md = instructions_path(&fx.ctx, Location::Global);

        let detect = target.detect(&fx.ctx, Location::Global);
        assert!(!detect.installed);
        assert!(!detect.already_configured);

        let result = target.install(&fx.ctx, Location::Global, opts());
        assert_eq!(result.files.len(), 2);
        let json = fx.read(&settings);
        let entry = &json["mcpServers"]["codegraph"];
        assert_eq!(entry["command"], "codegraph");
        assert_eq!(entry["args"], serde_json::json!(["serve", "--mcp"]));
        assert_eq!(entry["type"], "stdio");
        assert!(md.ends_with("GEMINI.md"));
        assert!(
            fs::read_to_string(&md)
                .unwrap()
                .contains(CODEGRAPH_SECTION_START)
        );

        let detect = target.detect(&fx.ctx, Location::Global);
        assert!(detect.installed);
        assert!(detect.already_configured);
    }

    #[test]
    fn install_local_uses_project_root_gemini_md() {
        let fx = TempGemini::new("install-local");
        let target = GeminiTarget;

        target.install(&fx.ctx, Location::Local, opts());
        let settings = settings_json_path(&fx.ctx, Location::Local);
        assert!(settings.starts_with(&fx.ctx.cwd));
        let md = instructions_path(&fx.ctx, Location::Local);
        assert_eq!(md, fx.ctx.cwd.join("GEMINI.md"));
        assert!(md.exists());
        assert!(fx.read(&settings)["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn install_is_idempotent() {
        let fx = TempGemini::new("idempotent");
        let target = GeminiTarget;
        let settings = settings_json_path(&fx.ctx, Location::Global);

        target.install(&fx.ctx, Location::Global, opts());
        let first = fs::read_to_string(&settings).unwrap();
        target.install(&fx.ctx, Location::Global, opts());
        assert_eq!(fs::read_to_string(&settings).unwrap(), first);
    }

    #[test]
    fn install_preserves_sibling_mcp_server() {
        let fx = TempGemini::new("preserve");
        let target = GeminiTarget;
        let settings = settings_json_path(&fx.ctx, Location::Global);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(
            &settings,
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();

        target.install(&fx.ctx, Location::Global, opts());
        let json = fx.read(&settings);
        assert!(json["mcpServers"]["other"].is_object());
        assert!(json["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn uninstall_removes_entry_and_instructions() {
        let fx = TempGemini::new("uninstall");
        let target = GeminiTarget;
        let settings = settings_json_path(&fx.ctx, Location::Global);

        target.install(&fx.ctx, Location::Global, opts());
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files[0].action, FileAction::Removed);
        let json = fx.read(&settings);
        assert!(json.get("mcpServers").is_none());
    }

    #[test]
    fn uninstall_missing_settings_is_not_found() {
        let fx = TempGemini::new("uninstall-missing");
        let target = GeminiTarget;
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
    }

    #[test]
    fn install_skips_unparseable_settings() {
        let fx = TempGemini::new("unparseable");
        let settings = settings_json_path(&fx.ctx, Location::Global);
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let corrupt = "{ this is not json";
        fs::write(&settings, corrupt).unwrap();

        let entry = write_mcp_entry(&fx.ctx, Location::Global);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&settings).unwrap(), corrupt);
    }

    #[test]
    fn print_config_shows_settings_target() {
        let target = GeminiTarget;
        let ctx = ctx();
        let out = target.print_config(&ctx, Location::Global);
        assert!(out.contains("mcpServers"));
        assert!(out.contains("codegraph"));
        assert!(out.replace('\\', "/").contains(".gemini/settings.json"));
        assert!(out.contains("command"));
    }

    #[test]
    fn config_and_instructions_paths_differ_by_location() {
        let ctx = ctx();
        assert!(config_dir(&ctx, Location::Global).ends_with(".gemini"));
        assert!(config_dir(&ctx, Location::Local).starts_with(&ctx.cwd));
        assert_eq!(
            instructions_path(&ctx, Location::Global),
            ctx.home.join(".gemini").join("GEMINI.md")
        );
        assert_eq!(
            instructions_path(&ctx, Location::Local),
            ctx.cwd.join("GEMINI.md")
        );
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
