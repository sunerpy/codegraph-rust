//! Claude Code target. Ports `upstream installer/targets/claude.ts`.
//!
//! Writes the MCP entry to `~/.claude.json` (global) or `./.mcp.json` (local),
//! permissions to `<dir>/.claude/settings.json` (gated on `auto_allow`), and the
//! marker-fenced instructions block to `<dir>/.claude/CLAUDE.md`.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::super::shared::{
    self, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, ConfigRead, codegraph_permissions,
    mcp_server_config, read_config_file, read_json_file, remove_codegraph_from_mcp_servers,
    to_upstream_json, upsert_instructions_entry, upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct ClaudeCodeTarget;

fn config_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => ctx.home.join(".claude"),
        Location::Local => ctx.cwd.join(".claude"),
    }
}
fn mcp_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    // global → ~/.claude.json; local → ./.mcp.json (claude.ts:49-56).
    match loc {
        Location::Global => ctx.home.join(".claude.json"),
        Location::Local => ctx.cwd.join(".mcp.json"),
    }
}
fn legacy_local_mcp_path(ctx: &InstallContext) -> PathBuf {
    ctx.cwd.join(".claude.json")
}
fn settings_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("settings.json")
}
fn instructions_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("CLAUDE.md")
}

impl AgentTarget for ClaudeCodeTarget {
    fn id(&self) -> TargetId {
        TargetId::Claude
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
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
        let installed = config_dir(ctx, loc).exists() || mcp_path.exists();
        DetectionResult {
            installed,
            already_configured,
        }
    }

    // Ports claudeTarget.install (claude.ts:96).
    fn install(&self, ctx: &InstallContext, loc: Location, opts: InstallOptions) -> WriteResult {
        let mut files = Vec::new();
        files.push(write_mcp_entry(ctx, loc));
        if loc == Location::Local
            && let Some(migrated) = cleanup_legacy_local_mcp(ctx)
        {
            files.push(migrated);
        }
        if opts.auto_allow {
            files.push(write_permissions_entry(ctx, loc));
        }
        if opts.front_load_hook {
            files.push(write_prompt_hook_entry(ctx, loc));
        }
        files.push(upsert_instructions_entry(&instructions_path(ctx, loc)));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports claudeTarget.uninstall (claude.ts:134).
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

        if loc == Location::Local
            && let Some(migrated) = cleanup_legacy_local_mcp(ctx)
        {
            files.push(migrated);
        }

        files.push(remove_permissions_entry(ctx, loc));
        files.push(remove_prompt_hook_entry(ctx, loc));
        files.push(remove_instructions_entry(ctx, loc));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports claudeTarget.printConfig (claude.ts:196).
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

// Ports writeMcpEntry (claude.ts:214).
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = mcp_json_path(ctx, loc);
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

/// Insert/replace `mcpServers.<key>` in a JSON object, creating the wrapper.
/// Shared by the `mcpServers`-shaped targets.
pub fn upsert_mcp_server(config: &mut serde_json::Map<String, Value>, key: &str, entry: Value) {
    let servers = config
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(serde_json::Map::new());
    }
    if let Value::Object(map) = servers {
        map.insert(key.to_string(), entry);
    }
}

// Ports cleanupLegacyLocalMcp (claude.ts:246).
fn cleanup_legacy_local_mcp(ctx: &InstallContext) -> Option<FileWrite> {
    let file = legacy_local_mcp_path(ctx);
    if !file.exists() {
        return None;
    }
    let mut config = read_json_file(&file);
    config.get("mcpServers").and_then(|s| s.get("codegraph"))?;
    remove_codegraph_from_mcp_servers(&mut config);
    if config.is_empty() {
        let _ = fs::remove_file(&file);
    } else {
        let _ = write_json_file(&file, &config);
    }
    Some(FileWrite {
        path: file,
        action: FileAction::Removed,
    })
}

// Ports writePermissionsEntry (claude.ts:340).
fn write_permissions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = settings_json_path(ctx, loc);
    let created = !file.exists();
    let mut settings = read_json_file(&file);

    let permissions = settings
        .entry("permissions")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !permissions.is_object() {
        *permissions = Value::Object(serde_json::Map::new());
    }
    let perms_obj = permissions.as_object_mut().expect("permissions is object");
    let allow = perms_obj
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !allow.is_array() {
        *allow = Value::Array(Vec::new());
    }
    let allow_arr = allow.as_array_mut().expect("allow is array");
    let before = allow_arr.clone();
    for perm in codegraph_permissions() {
        let perm_value = Value::String(perm.to_string());
        if !allow_arr.contains(&perm_value) {
            allow_arr.push(perm_value);
        }
    }
    if *allow_arr == before && !created {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    let _ = write_json_file(&file, &settings);
    FileWrite {
        path: file,
        action: if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    }
}

// Ports the permissions-stripping branch of claudeTarget.uninstall (claude.ts:158-180).
fn remove_permissions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = settings_json_path(ctx, loc);
    let mut settings = read_json_file(&file);
    let removed = (|| {
        let permissions = settings.get_mut("permissions")?.as_object_mut()?;
        let allow = permissions.get_mut("allow")?.as_array_mut()?;
        let before = allow.len();
        allow.retain(|p| {
            p.as_str()
                .is_none_or(|s| !s.starts_with("mcp__codegraph__"))
        });
        if allow.len() == before {
            return Some(false);
        }
        if allow.is_empty() {
            permissions.remove("allow");
        }
        if permissions.is_empty() {
            settings.remove("permissions");
        }
        Some(true)
    })()
    .unwrap_or(false);

    if removed {
        let _ = write_json_file(&file, &settings);
        FileWrite {
            path: file,
            action: FileAction::Removed,
        }
    } else {
        FileWrite {
            path: file,
            action: FileAction::NotFound,
        }
    }
}

// Ports removeInstructionsEntry (claude.ts:371).
fn remove_instructions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = instructions_path(ctx, loc);
    let action =
        shared::remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

fn prompt_hook_command() -> &'static str {
    "codegraph prompt-hook"
}

/// True if a `UserPromptSubmit` group already holds a codegraph `prompt-hook`
/// command — the idempotency guard that stops re-install duplicating the entry.
fn group_has_codegraph_hook(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains("prompt-hook"))
            })
        })
}

/// Write the opt-in `UserPromptSubmit` front-load hook into `settings.json`
/// (additive + idempotent; sibling groups survive; OPT-IN, never default-on).
fn write_prompt_hook_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = settings_json_path(ctx, loc);
    let created = !file.exists();
    let mut settings = read_json_file(&file);

    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks_obj = hooks.as_object_mut().expect("hooks is object");
    let groups = hooks_obj
        .entry("UserPromptSubmit")
        .or_insert_with(|| Value::Array(Vec::new()));
    if !groups.is_array() {
        *groups = Value::Array(Vec::new());
    }
    let groups_arr = groups.as_array_mut().expect("UserPromptSubmit is array");

    if groups_arr.iter().any(group_has_codegraph_hook) {
        return FileWrite {
            path: file,
            action: if created {
                FileAction::Created
            } else {
                FileAction::Unchanged
            },
        };
    }
    groups_arr.push(json!({
        "hooks": [{ "type": "command", "command": prompt_hook_command() }],
    }));

    let _ = write_json_file(&file, &settings);
    FileWrite {
        path: file,
        action: if created {
            FileAction::Created
        } else {
            FileAction::Updated
        },
    }
}

/// Strip codegraph's `UserPromptSubmit` front-load hook on uninstall, dropping
/// empty `UserPromptSubmit`/`hooks` wrappers. A user's own hooks survive.
fn remove_prompt_hook_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = settings_json_path(ctx, loc);
    let mut settings = read_json_file(&file);
    let removed = (|| {
        let hooks = settings.get_mut("hooks")?.as_object_mut()?;
        let groups = hooks.get_mut("UserPromptSubmit")?.as_array_mut()?;
        let before = groups.len();
        groups.retain(|g| !group_has_codegraph_hook(g));
        if groups.len() == before {
            return Some(false);
        }
        if groups.is_empty() {
            hooks.remove("UserPromptSubmit");
        }
        if hooks.is_empty() {
            settings.remove("hooks");
        }
        Some(true)
    })()
    .unwrap_or(false);

    if removed {
        let _ = write_json_file(&file, &settings);
        FileWrite {
            path: file,
            action: FileAction::Removed,
        }
    } else {
        FileWrite {
            path: file,
            action: FileAction::NotFound,
        }
    }
}

pub static CLAUDE_TARGET: ClaudeCodeTarget = ClaudeCodeTarget;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ctx() -> InstallContext {
        let base = std::env::temp_dir().join(format!(
            "codegraph-claude-skill-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        InstallContext {
            home: base.join("home"),
            cwd: base.join("cwd"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    #[test]
    fn claude_supports_and_locates_skills_at_both_locations() {
        // Given the Claude Code target and a temp install context
        let target = ClaudeCodeTarget;
        let ctx = temp_ctx();

        // Then it supports skills at both Global and Local
        assert!(target.supports_skills(Location::Global));
        assert!(target.supports_skills(Location::Local));

        // And the parent skills dir is `.claude/skills` for each location
        // (the engine appends `codegraph/SKILL.md` itself).
        let global = target.skill_dir(&ctx, Location::Global).unwrap();
        assert!(global.ends_with("skills"));
        assert!(global.parent().unwrap().ends_with(".claude"));
        assert_eq!(global, ctx.home.join(".claude").join("skills"));

        let local = target.skill_dir(&ctx, Location::Local).unwrap();
        assert!(local.ends_with("skills"));
        assert!(local.parent().unwrap().ends_with(".claude"));
        assert_eq!(local, ctx.cwd.join(".claude").join("skills"));
    }

    struct TempClaude {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempClaude {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-claude-{label}-{}-{}",
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
            fs::create_dir_all(&ctx.home).unwrap();
            fs::create_dir_all(&ctx.cwd).unwrap();
            Self { base, ctx }
        }
        fn read(&self, p: &PathBuf) -> Value {
            serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
        }
    }

    impl Drop for TempClaude {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn opts(auto_allow: bool) -> InstallOptions {
        InstallOptions {
            auto_allow,
            front_load_hook: false,
        }
    }

    #[test]
    fn detect_reflects_config_presence() {
        let fx = TempClaude::new("detect");
        let target = ClaudeCodeTarget;
        let before = target.detect(&fx.ctx, Location::Local);
        assert!(!before.installed);
        assert!(!before.already_configured);

        target.install(&fx.ctx, Location::Local, opts(false));
        let after = target.detect(&fx.ctx, Location::Local);
        assert!(after.installed);
        assert!(after.already_configured);
    }

    #[test]
    fn install_with_auto_allow_writes_permissions() {
        let fx = TempClaude::new("perms");
        let target = ClaudeCodeTarget;
        target.install(&fx.ctx, Location::Local, opts(true));
        let settings = settings_json_path(&fx.ctx, Location::Local);
        let allow = fx.read(&settings)["permissions"]["allow"].clone();
        let arr = allow.as_array().unwrap();
        assert!(arr.contains(&Value::String("mcp__codegraph__codegraph_explore".into())));

        let again = write_permissions_entry(&fx.ctx, Location::Local);
        assert_eq!(again.action, FileAction::Unchanged);
    }

    #[test]
    fn cleanup_legacy_local_mcp_migrates_old_entry() {
        let fx = TempClaude::new("legacy");
        let legacy = legacy_local_mcp_path(&fx.ctx);
        fs::write(
            &legacy,
            "{\n  \"mcpServers\": { \"codegraph\": { \"command\": \"old\" }, \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();

        let migrated = cleanup_legacy_local_mcp(&fx.ctx).expect("legacy entry migrated");
        assert_eq!(migrated.action, FileAction::Removed);
        let json = fx.read(&legacy);
        assert!(json["mcpServers"].get("codegraph").is_none());
        assert!(json["mcpServers"]["other"].is_object());
    }

    #[test]
    fn cleanup_legacy_local_mcp_deletes_file_when_emptied() {
        let fx = TempClaude::new("legacy-empty");
        let legacy = legacy_local_mcp_path(&fx.ctx);
        fs::write(
            &legacy,
            "{\n  \"mcpServers\": { \"codegraph\": { \"command\": \"old\" } }\n}\n",
        )
        .unwrap();
        let migrated = cleanup_legacy_local_mcp(&fx.ctx).expect("migrated");
        assert_eq!(migrated.action, FileAction::Removed);
        assert!(!legacy.exists(), "emptied legacy file removed");
    }

    #[test]
    fn cleanup_legacy_local_mcp_none_when_absent() {
        let fx = TempClaude::new("legacy-none");
        assert!(cleanup_legacy_local_mcp(&fx.ctx).is_none());
    }

    #[test]
    fn install_skips_unparseable_mcp_config() {
        let fx = TempClaude::new("unparseable");
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        let corrupt = "{ not json";
        fs::write(&mcp, corrupt).unwrap();
        let entry = write_mcp_entry(&fx.ctx, Location::Local);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&mcp).unwrap(), corrupt);
    }

    #[test]
    fn remove_permissions_entry_not_found_when_absent() {
        let fx = TempClaude::new("perms-absent");
        let result = remove_permissions_entry(&fx.ctx, Location::Local);
        assert_eq!(result.action, FileAction::NotFound);
    }

    #[test]
    fn upsert_mcp_server_resets_non_object_wrapper() {
        let mut config = Map::new();
        config.insert("mcpServers".to_string(), json!("not an object"));
        upsert_mcp_server(&mut config, "codegraph", json!({ "command": "codegraph" }));
        assert!(config["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn print_config_shows_global_target_path() {
        let fx = TempClaude::new("print");
        let target = ClaudeCodeTarget;
        let out = target.print_config(&fx.ctx, Location::Global);
        assert!(out.contains("mcpServers"));
        assert!(out.contains("codegraph"));
        assert!(out.contains(".claude.json"));
    }

    #[test]
    fn full_uninstall_removes_all_codegraph_artifacts() {
        let fx = TempClaude::new("uninstall-full");
        let target = ClaudeCodeTarget;
        target.install(&fx.ctx, Location::Local, opts(true));
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        assert!(fx.read(&mcp)["mcpServers"]["codegraph"].is_object());

        target.uninstall(&fx.ctx, Location::Local);
        let json = fx.read(&mcp);
        assert!(json.get("mcpServers").is_none());
        let settings = settings_json_path(&fx.ctx, Location::Local);
        if settings.exists() {
            assert!(fx.read(&settings).get("permissions").is_none());
        }
    }
}
