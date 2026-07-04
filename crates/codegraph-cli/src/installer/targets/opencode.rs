//! opencode target. Ports `upstream installer/targets/opencode.ts`.
//!
//! Writes the MCP entry to `$XDG_CONFIG_HOME/opencode/opencode.jsonc` (global,
//! XDG on every platform) or `./opencode.jsonc` (local), falling back to an
//! existing `.json`. Instructions go to `<dir>/AGENTS.md`. opencode uses the
//! `mcp.<name>` wrapper with a string-array `command` and an `enabled` flag —
//! not `mcpServers`.
//!
//! Existing configs are edited surgically via `jsonc-parser` (see
//! `shared::upsert_nested_key_jsonc`), preserving the user's comments, key
//! order, and formatting; only the `codegraph` entry changes. Fresh files are
//! seeded with the canonical shape.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::shared::{
    self, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, ConfigRead, parse_json_object,
    read_config_file, remove_nested_key_jsonc, to_upstream_json, upsert_instructions_entry,
    upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct OpencodeTarget;

const SCHEMA_URL: &str = "https://opencode.ai/config.json";

// Ports globalConfigDir (opencode.ts:59): XDG_CONFIG_HOME if set, else ~/.config.
fn global_config_dir(ctx: &InstallContext) -> PathBuf {
    let xdg = ctx
        .xdg_config_home
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".config"));
    xdg.join("opencode")
}

// Ports legacyWindowsConfigDir (opencode.ts:76).
fn legacy_windows_config_dir(ctx: &InstallContext) -> Option<PathBuf> {
    let app_data = ctx
        .app_data
        .as_ref()
        .filter(|p| !p.as_os_str().is_empty())?;
    let legacy = app_data.join("opencode");
    if legacy == global_config_dir(ctx) {
        None
    } else {
        Some(legacy)
    }
}

fn config_base_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => global_config_dir(ctx),
        Location::Local => ctx.cwd.clone(),
    }
}

// Ports configPath (opencode.ts:90): existing .jsonc, then .json, default .jsonc.
fn config_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    let dir = config_base_dir(ctx, loc);
    let jsonc = dir.join("opencode.jsonc");
    let json = dir.join("opencode.json");
    if jsonc.exists() {
        return jsonc;
    }
    if json.exists() {
        return json;
    }
    jsonc
}

fn instructions_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_base_dir(ctx, loc).join("AGENTS.md")
}

// Ports getOpencodeServerEntry (opencode.ts:118).
fn opencode_server_entry() -> Value {
    json!({
        "type": "local",
        "command": ["codegraph", "serve", "--mcp"],
        "enabled": true,
    })
}

fn parse_config(text: &str) -> Map<String, Value> {
    parse_json_object(text).unwrap_or_default()
}

impl AgentTarget for OpencodeTarget {
    fn id(&self) -> TargetId {
        TargetId::Opencode
    }
    fn display_name(&self) -> &'static str {
        "opencode"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let file = config_path(ctx, loc);
        let config = parse_config(&fs::read_to_string(&file).unwrap_or_default());
        let already_configured = config.get("mcp").and_then(|m| m.get("codegraph")).is_some();
        let installed = match loc {
            Location::Global => {
                let legacy = legacy_windows_config_dir(ctx);
                global_config_dir(ctx).exists()
                    || legacy.as_ref().map(|l| l.exists()).unwrap_or(false)
            }
            Location::Local => file.exists(),
        };
        DetectionResult {
            installed,
            already_configured,
        }
    }

    // Ports opencodeTarget.install (opencode.ts:151).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let mut files = vec![write_mcp_entry(ctx, loc)];
        files.push(upsert_instructions_entry(&instructions_path(ctx, loc)));
        if loc == Location::Global {
            files.extend(cleanup_legacy_windows_state(ctx));
        }
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports opencodeTarget.uninstall (opencode.ts:167).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mut files = vec![remove_mcp_entry_at(&config_path(ctx, loc))];
        files.push(remove_instructions_entry(ctx, loc));
        if loc == Location::Global {
            files.extend(cleanup_legacy_windows_state(ctx));
        }
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports opencodeTarget.printConfig (opencode.ts:175).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let target = config_path(ctx, loc);
        let snippet = to_upstream_json(&json!({
            "$schema": SCHEMA_URL,
            "mcp": { "codegraph": opencode_server_entry() },
        }));
        format!("# Add to {}\n\n{snippet}\n", target.display())
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        let parent = match loc {
            Location::Global => global_config_dir(ctx).join("skill"),
            Location::Local => ctx.cwd.join(".opencode").join("skill"),
        };
        Some(parent)
    }
}

// Ports writeMcpEntry (opencode.ts:189).
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = config_path(ctx, loc);
    let after = opencode_server_entry();
    match read_config_file(&file) {
        ConfigRead::Unparseable => FileWrite {
            path: file,
            action: FileAction::Skipped,
        },
        ConfigRead::Missing => {
            let mut config = Map::new();
            config.insert("$schema".to_string(), json!(SCHEMA_URL));
            let mut mcp = Map::new();
            mcp.insert("codegraph".to_string(), after);
            config.insert("mcp".to_string(), Value::Object(mcp));
            let _ = write_json_file(&file, &config);
            FileWrite {
                path: file,
                action: FileAction::Created,
            }
        }
        ConfigRead::Parsed(_) => {
            let action =
                upsert_nested_key_jsonc(&file, "mcp", "codegraph", &after, Some(SCHEMA_URL))
                    .unwrap_or(FileAction::Skipped);
            FileWrite { path: file, action }
        }
    }
}

// Ports removeMcpEntryAt (opencode.ts:233).
fn remove_mcp_entry_at(file: &Path) -> FileWrite {
    let action = remove_nested_key_jsonc(file, "mcp", "codegraph").unwrap_or(FileAction::NotFound);
    FileWrite {
        path: file.to_path_buf(),
        action,
    }
}

// Ports cleanupLegacyWindowsState (opencode.ts:263).
fn cleanup_legacy_windows_state(ctx: &InstallContext) -> Vec<FileWrite> {
    let Some(dir) = legacy_windows_config_dir(ctx) else {
        return Vec::new();
    };
    if !dir.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for name in ["opencode.jsonc", "opencode.json"] {
        let res = remove_mcp_entry_at(&dir.join(name));
        if res.action == FileAction::Removed {
            out.push(res);
        }
    }
    let agents = dir.join("AGENTS.md");
    let action =
        shared::remove_marked_section(&agents, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    if action == FileAction::Removed {
        out.push(FileWrite {
            path: agents,
            action,
        });
    }
    out
}

// Ports removeInstructionsEntry (opencode.ts:282).
fn remove_instructions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = instructions_path(ctx, loc);
    let action =
        shared::remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static OPENCODE_TARGET: OpencodeTarget = OpencodeTarget;

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

    struct TempOc {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempOc {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-opencode-{label}-{}-{}",
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
                xdg_config_home: Some(base.join("xdg")),
                hermes_home: None,
            };
            fs::create_dir_all(&ctx.cwd).unwrap();
            Self { base, ctx }
        }
        fn read(&self, p: &PathBuf) -> Value {
            let text = fs::read_to_string(p).unwrap();
            Value::Object(parse_json_object(&text).unwrap())
        }
    }

    impl Drop for TempOc {
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
    fn local_install_writes_mcp_wrapper_and_agents_md() {
        let fx = TempOc::new("local-install");
        let target = OpencodeTarget;
        let cfg = config_path(&fx.ctx, Location::Local);

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(!detect.installed);

        let result = target.install(&fx.ctx, Location::Local, opts());
        assert!(result.files.len() >= 2);
        let json = fx.read(&cfg);
        assert_eq!(json["$schema"], SCHEMA_URL);
        let entry = &json["mcp"]["codegraph"];
        assert_eq!(entry["type"], "local");
        assert_eq!(
            entry["command"],
            serde_json::json!(["codegraph", "serve", "--mcp"])
        );
        assert_eq!(entry["enabled"], true);
        assert!(instructions_path(&fx.ctx, Location::Local).exists());

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(detect.installed);
        assert!(detect.already_configured);
    }

    #[test]
    fn global_install_uses_xdg_config_dir() {
        let fx = TempOc::new("global-install");
        let target = OpencodeTarget;
        target.install(&fx.ctx, Location::Global, opts());
        let cfg = config_path(&fx.ctx, Location::Global);
        assert!(cfg.starts_with(fx.ctx.xdg_config_home.as_ref().unwrap()));
        assert!(fx.read(&cfg)["mcp"]["codegraph"].is_object());
    }

    #[test]
    fn install_is_idempotent_preserving_siblings() {
        let fx = TempOc::new("idempotent");
        let target = OpencodeTarget;
        let cfg = fx.ctx.cwd.join("opencode.jsonc");
        fs::write(
            &cfg,
            "{\n  // keep this\n  \"mcp\": { \"other\": { \"type\": \"local\" } }\n}\n",
        )
        .unwrap();

        target.install(&fx.ctx, Location::Local, opts());
        let first = fs::read_to_string(&cfg).unwrap();
        assert!(first.contains("// keep this"), "comment preserved");
        assert!(first.contains("\"other\""), "sibling preserved");
        assert!(first.contains("\"codegraph\""), "codegraph inserted");
        target.install(&fx.ctx, Location::Local, opts());
        assert_eq!(fs::read_to_string(&cfg).unwrap(), first, "no churn");
    }

    #[test]
    fn config_path_prefers_existing_json_over_default_jsonc() {
        let fx = TempOc::new("prefer-json");
        let json_file = fx.ctx.cwd.join("opencode.json");
        fs::write(&json_file, "{}\n").unwrap();
        assert_eq!(config_path(&fx.ctx, Location::Local), json_file);
    }

    #[test]
    fn uninstall_removes_entry_and_instructions() {
        let fx = TempOc::new("uninstall");
        let target = OpencodeTarget;
        let cfg = config_path(&fx.ctx, Location::Local);
        target.install(&fx.ctx, Location::Local, opts());
        let result = target.uninstall(&fx.ctx, Location::Local);
        assert_eq!(result.files[0].action, FileAction::Removed);
        let json = fx.read(&cfg);
        assert!(json.get("mcp").is_none() || json["mcp"].get("codegraph").is_none());
    }

    #[test]
    fn install_skips_unparseable_config() {
        let fx = TempOc::new("unparseable");
        let cfg = fx.ctx.cwd.join("opencode.jsonc");
        let corrupt = "{ not json";
        fs::write(&cfg, corrupt).unwrap();
        let entry = write_mcp_entry(&fx.ctx, Location::Local);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), corrupt);
    }

    #[test]
    fn legacy_windows_dir_none_when_no_app_data() {
        let fx = TempOc::new("no-appdata");
        assert!(legacy_windows_config_dir(&fx.ctx).is_none());
    }

    #[test]
    fn legacy_windows_dir_some_when_distinct_app_data() {
        let base = std::env::temp_dir().join(format!(
            "cg-oc-legacy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ctx = InstallContext {
            home: base.join("home"),
            cwd: base.join("cwd"),
            app_data: Some(base.join("appdata")),
            xdg_config_home: Some(base.join("xdg")),
            hermes_home: None,
        };
        let legacy = legacy_windows_config_dir(&ctx).expect("distinct app_data yields legacy dir");
        assert!(legacy.ends_with("opencode"));
        assert!(
            cleanup_legacy_windows_state(&ctx).is_empty(),
            "no legacy files to clean when dir absent"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn cleanup_legacy_windows_state_removes_stale_entries() {
        let base = std::env::temp_dir().join(format!(
            "cg-oc-legacy-clean-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ctx = InstallContext {
            home: base.join("home"),
            cwd: base.join("cwd"),
            app_data: Some(base.join("appdata")),
            xdg_config_home: Some(base.join("xdg")),
            hermes_home: None,
        };
        let legacy = legacy_windows_config_dir(&ctx).unwrap();
        fs::create_dir_all(&legacy).unwrap();
        fs::write(
            legacy.join("opencode.jsonc"),
            "{\n  \"mcp\": { \"codegraph\": { \"type\": \"local\" } }\n}\n",
        )
        .unwrap();
        fs::write(
            legacy.join("AGENTS.md"),
            format!("body\n\n{CODEGRAPH_SECTION_START}\nx\n{CODEGRAPH_SECTION_END}\n"),
        )
        .unwrap();

        let removed = cleanup_legacy_windows_state(&ctx);
        assert!(
            removed.iter().any(|f| f.action == FileAction::Removed),
            "stale legacy entries removed"
        );
        let json: Value =
            serde_json::from_str(&fs::read_to_string(legacy.join("opencode.jsonc")).unwrap())
                .unwrap();
        assert!(json.get("mcp").is_none() || json["mcp"].get("codegraph").is_none());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn print_config_shows_mcp_wrapper() {
        let target = OpencodeTarget;
        let out = target.print_config(&ctx(), Location::Local);
        assert!(out.contains("$schema"));
        assert!(out.contains(SCHEMA_URL));
        assert!(out.contains("\"mcp\""));
        assert!(out.contains("codegraph"));
    }

    #[test]
    fn opencode_supports_skills_both_locations() {
        let t = OpencodeTarget;
        assert!(t.supports_skills(Location::Global));
        assert!(t.supports_skills(Location::Local));
    }

    #[test]
    fn opencode_global_skill_dir_is_singular_skill_under_opencode() {
        // Given no XDG override → falls back to ~/.config/opencode.
        let dir = OpencodeTarget
            .skill_dir(&ctx(), Location::Global)
            .expect("global skill dir");
        // SINGULAR `skill`, parent dir (engine appends codegraph/SKILL.md).
        assert!(
            dir.ends_with("opencode/skill"),
            "expected to end with opencode/skill, got {}",
            dir.display()
        );
    }

    #[test]
    fn opencode_local_skill_dir_is_dot_opencode_skill() {
        let dir = OpencodeTarget
            .skill_dir(&ctx(), Location::Local)
            .expect("local skill dir");
        assert_eq!(dir, PathBuf::from("/work/proj/.opencode/skill"));
        assert!(dir.ends_with(".opencode/skill"));
    }
}
