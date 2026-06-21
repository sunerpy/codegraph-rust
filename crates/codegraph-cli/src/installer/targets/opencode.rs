//! opencode target. Ports `upstream installer/targets/opencode.ts`.
//!
//! Writes the MCP entry to `$XDG_CONFIG_HOME/opencode/opencode.jsonc` (global,
//! XDG on every platform) or `./opencode.jsonc` (local), falling back to an
//! existing `.json`. Instructions go to `<dir>/AGENTS.md`. opencode uses the
//! `mcp.<name>` wrapper with a string-array `command` and an `enabled` flag —
//! not `mcpServers`.
//!
//! DIVERGENCE FROM UPSTREAM: the upstream edits through `jsonc-parser` to preserve `//`
//! comments on idempotent re-runs. This port uses serde_json (the task's JSON
//! tool of choice): the written keys/shape are byte-faithful, but a user's
//! hand-added JSONC comments are not preserved across an install rewrite. The
//! emitted config remains valid opencode JSON.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use super::super::shared::{
    self, parse_json_object, read_config_file, to_upstream_json, upsert_instructions_entry,
    write_json_file, ConfigRead, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START,
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
}

// Ports writeMcpEntry (opencode.ts:189).
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = config_path(ctx, loc);
    let existed = file.exists();
    let mut config = match read_config_file(&file) {
        ConfigRead::Missing => Map::new(),
        ConfigRead::Parsed(map) => map,
        ConfigRead::Unparseable => {
            return FileWrite {
                path: file,
                action: FileAction::Skipped,
            };
        }
    };
    let before = config.get("mcp").and_then(|m| m.get("codegraph"));
    let after = opencode_server_entry();
    if before == Some(&after) {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    if !config.contains_key("$schema") {
        // Insert $schema first so it leads the file, matching the seeded shape.
        let mut reordered = Map::new();
        reordered.insert("$schema".to_string(), json!(SCHEMA_URL));
        for (k, v) in config {
            reordered.insert(k, v);
        }
        config = reordered;
    }
    let mcp = config
        .entry("mcp")
        .or_insert_with(|| Value::Object(Map::new()));
    if !mcp.is_object() {
        *mcp = Value::Object(Map::new());
    }
    if let Value::Object(map) = mcp {
        map.insert("codegraph".to_string(), after);
    }
    let _ = write_json_file(&file, &config);
    FileWrite {
        path: file,
        action: if existed {
            FileAction::Updated
        } else {
            FileAction::Created
        },
    }
}

// Ports removeMcpEntryAt (opencode.ts:233).
fn remove_mcp_entry_at(file: &Path) -> FileWrite {
    if !file.exists() {
        return FileWrite {
            path: file.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    let text = fs::read_to_string(file).unwrap_or_default();
    let mut config = parse_config(&text);
    if config.get("mcp").and_then(|m| m.get("codegraph")).is_none() {
        return FileWrite {
            path: file.to_path_buf(),
            action: FileAction::NotFound,
        };
    }
    if let Some(Value::Object(mcp)) = config.get_mut("mcp") {
        mcp.remove("codegraph");
        if mcp.is_empty() {
            config.remove("mcp");
        }
    }
    let _ = write_json_file(file, &config);
    FileWrite {
        path: file.to_path_buf(),
        action: FileAction::Removed,
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
