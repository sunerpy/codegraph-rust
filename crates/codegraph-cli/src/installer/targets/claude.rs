//! Claude Code target. Ports `upstream installer/targets/claude.ts`.
//!
//! Writes the MCP entry to `~/.claude.json` (global) or `./.mcp.json` (local),
//! permissions to `<dir>/.claude/settings.json` (gated on `auto_allow`), and the
//! marker-fenced instructions block to `<dir>/.claude/CLAUDE.md`.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use super::super::shared::{
    self, codegraph_permissions, mcp_server_config, read_config_file, read_json_file,
    remove_codegraph_from_mcp_servers, to_upstream_json, upsert_instructions_entry,
    upsert_nested_key_jsonc, write_json_file, ConfigRead, CODEGRAPH_SECTION_END,
    CODEGRAPH_SECTION_START,
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
        if loc == Location::Local {
            if let Some(migrated) = cleanup_legacy_local_mcp(ctx) {
                files.push(migrated);
            }
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

        if loc == Location::Local {
            if let Some(migrated) = cleanup_legacy_local_mcp(ctx) {
                files.push(migrated);
            }
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
                .map_or(true, |s| !s.starts_with("mcp__codegraph__"))
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
