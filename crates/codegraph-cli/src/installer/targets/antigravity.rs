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

use serde_json::{Map, Value, json};

use super::super::shared::{
    ConfigRead, read_config_file, read_json_file, remove_codegraph_from_mcp_servers,
    to_upstream_json, upsert_nested_key_jsonc, write_json_file,
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

    // Skill support is DECOUPLED from supports_location: MCP is global-only
    // (supports_location(Local)==false above) but Antigravity reads workspace
    // skills, so both locations support skills here. Global skill dir is
    // `~/.gemini/config/skills` (NOT gemini's `~/.gemini/skills`); local is
    // `<cwd>/.agents/skills`.
    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }
    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        let parent = match loc {
            Location::Global => unified_config_dir(ctx).join("skills"),
            Location::Local => ctx.cwd.join(".agents").join("skills"),
        };
        Some(parent)
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

    struct TempAg {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempAg {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-antigravity-{label}-{}-{}",
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
        fn read(&self, p: &Path) -> Value {
            serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
        }
    }

    impl Drop for TempAg {
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
    fn install_fresh_writes_legacy_path_omitting_type() {
        let fx = TempAg::new("install-fresh");
        let target = AntigravityTarget;

        let detect = target.detect(&fx.ctx, Location::Global);
        assert!(!detect.installed);
        assert!(!detect.already_configured);

        let result = target.install(&fx.ctx, Location::Global, opts());
        assert!(!result.files.is_empty());
        assert!(!result.notes.is_empty());
        let legacy = legacy_mcp_config_path(&fx.ctx);
        assert!(legacy.exists(), "fresh install writes to legacy path");
        let entry = &fx.read(&legacy)["mcpServers"]["codegraph"];
        assert_eq!(entry["command"], "codegraph");
        assert_eq!(entry["args"], serde_json::json!(["serve", "--mcp"]));
        assert!(
            entry.get("type").is_none(),
            "Antigravity rejects type field"
        );

        let detect = target.detect(&fx.ctx, Location::Global);
        assert!(detect.installed);
        assert!(detect.already_configured);
    }

    #[test]
    fn install_prefers_unified_path_when_migrated_marker_present() {
        let fx = TempAg::new("install-migrated");
        let target = AntigravityTarget;
        let marker = migrated_marker_path(&fx.ctx);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "").unwrap();

        target.install(&fx.ctx, Location::Global, opts());
        let unified = unified_mcp_config_path(&fx.ctx);
        assert!(unified.exists(), "migrated marker routes to unified path");
        assert!(fx.read(&unified)["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn install_prefers_unified_when_unified_file_exists() {
        let fx = TempAg::new("install-unified-exists");
        let target = AntigravityTarget;
        let unified = unified_mcp_config_path(&fx.ctx);
        fs::create_dir_all(unified.parent().unwrap()).unwrap();
        fs::write(&unified, "{}\n").unwrap();
        assert_eq!(preferred_mcp_config_path(&fx.ctx), unified);

        target.install(&fx.ctx, Location::Global, opts());
        assert!(fx.read(&unified)["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn install_is_idempotent() {
        let fx = TempAg::new("idempotent");
        let target = AntigravityTarget;
        target.install(&fx.ctx, Location::Global, opts());
        let legacy = legacy_mcp_config_path(&fx.ctx);
        let first = fs::read_to_string(&legacy).unwrap();
        target.install(&fx.ctx, Location::Global, opts());
        assert_eq!(fs::read_to_string(&legacy).unwrap(), first);
    }

    #[test]
    fn install_cleans_up_legacy_when_writing_unified() {
        let fx = TempAg::new("cleanup-legacy");
        let target = AntigravityTarget;
        let legacy = legacy_mcp_config_path(&fx.ctx);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            "{\n  \"mcpServers\": { \"codegraph\": { \"command\": \"old\" } }\n}\n",
        )
        .unwrap();
        let marker = migrated_marker_path(&fx.ctx);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, "").unwrap();

        let result = target.install(&fx.ctx, Location::Global, opts());
        let unified = unified_mcp_config_path(&fx.ctx);
        assert!(fx.read(&unified)["mcpServers"]["codegraph"].is_object());
        assert!(
            fx.read(&legacy)["mcpServers"].get("codegraph").is_none(),
            "legacy codegraph entry removed"
        );
        assert!(
            result.files.iter().any(|f| f.action == FileAction::Removed),
            "cleanup emits a Removed file"
        );
    }

    #[test]
    fn install_skips_unparseable_config() {
        let fx = TempAg::new("unparseable");
        let legacy = legacy_mcp_config_path(&fx.ctx);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let corrupt = "{ not json at all";
        fs::write(&legacy, corrupt).unwrap();

        let entry = write_mcp_entry(&fx.ctx);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&legacy).unwrap(), corrupt);
    }

    #[test]
    fn uninstall_removes_codegraph_preserving_siblings() {
        let fx = TempAg::new("uninstall");
        let target = AntigravityTarget;
        let legacy = legacy_mcp_config_path(&fx.ctx);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();
        target.install(&fx.ctx, Location::Global, opts());

        let result = target.uninstall(&fx.ctx, Location::Global);
        assert!(result.files.iter().any(|f| f.action == FileAction::Removed));
        let json = fx.read(&legacy);
        assert!(json["mcpServers"].get("codegraph").is_none());
        assert!(json["mcpServers"]["other"].is_object());
    }

    #[test]
    fn uninstall_missing_config_is_not_found() {
        let fx = TempAg::new("uninstall-missing");
        let target = AntigravityTarget;
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
    }

    #[test]
    fn uninstall_config_without_codegraph_is_not_found() {
        let fx = TempAg::new("uninstall-absent");
        let legacy = legacy_mcp_config_path(&fx.ctx);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();
        let result = remove_codegraph_from_file(&legacy);
        assert_eq!(result.action, FileAction::NotFound);
    }

    #[test]
    fn local_location_is_rejected_for_mcp_ops() {
        let fx = TempAg::new("local-reject");
        let target = AntigravityTarget;
        assert!(!target.supports_location(Location::Local));

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
    fn print_config_global_shows_entry_without_type() {
        let fx = TempAg::new("print");
        let target = AntigravityTarget;
        let out = target.print_config(&fx.ctx, Location::Global);
        assert!(out.contains("mcpServers"));
        assert!(out.contains("codegraph"));
        assert!(out.contains("serve"));
        assert!(!out.contains("\"type\""));
    }

    #[test]
    fn preferred_path_defaults_to_legacy_without_markers() {
        let fx = TempAg::new("preferred-default");
        assert_eq!(
            preferred_mcp_config_path(&fx.ctx),
            legacy_mcp_config_path(&fx.ctx)
        );
    }

    #[test]
    fn resolve_command_is_bare_on_non_macos() {
        if !cfg!(target_os = "macos") {
            assert_eq!(resolve_codegraph_command(), "codegraph");
        }
    }

    #[test]
    fn antigravity_skills_decoupled_from_location_with_distinct_paths() {
        // Given the Antigravity target
        let target = AntigravityTarget;
        let ctx = ctx();

        // Then skill support is decoupled from MCP location support: it supports
        // skills at BOTH locations even though supports_location(Local) is false.
        assert!(target.supports_skills(Location::Global));
        assert!(target.supports_skills(Location::Local));
        assert!(
            target.supports_skills(Location::Local) && !target.supports_location(Location::Local)
        );

        // And global skill_dir is ~/.gemini/config/skills (DISTINCT from gemini's ~/.gemini/skills)
        let global = target.skill_dir(&ctx, Location::Global).unwrap();
        assert!(global.ends_with(".gemini/config/skills"));
        assert!(!global.ends_with(".gemini/skills"));

        // And local skill_dir is ./.agents/skills
        let local = target.skill_dir(&ctx, Location::Local).unwrap();
        assert!(local.ends_with(".agents/skills"));
    }
}
