//! Helpers shared across `AgentTarget` implementations.
//!
//! Ports `upstream installer/targets/shared.ts`,
//! `instructions-template.ts`, and `toml.ts`.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::types::{FileAction, FileWrite};

/// Markers for the marker-based section write/removal.
/// Ports `instructions-template.ts:29-30`.
pub const CODEGRAPH_SECTION_START: &str = "<!-- CODEGRAPH_START -->";
pub const CODEGRAPH_SECTION_END: &str = "<!-- CODEGRAPH_END -->";

/// The marker-fenced agent-instructions block, byte-identical to
/// `instructions-template.ts:42-51` (`CODEGRAPH_INSTRUCTIONS_BLOCK`).
pub const CODEGRAPH_INSTRUCTIONS_BLOCK: &str = "<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tools** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them. `codegraph_node` returns one symbol's source + callers, or reads a whole file with line numbers. If the tools are listed but deferred, load them by name via tool search.
- **Shell** (always works): `codegraph explore \"<symbol names or question>\"` and `codegraph node <symbol-or-file>` print the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
<!-- CODEGRAPH_END -->";

/// The MCP-server config block codegraph injects. Ports `getMcpServerConfig`
/// (shared.ts:24). The Rust port launches the `codegraph` binary directly with
/// `serve --mcp` (the CLI's `Serve { mcp, path }` command, main.rs:158).
pub fn mcp_server_config() -> Value {
    serde_json::json!({
        "type": "stdio",
        "command": "codegraph",
        "args": ["serve", "--mcp"],
    })
}

/// Permissions list for Claude `settings.json`. Ports `getCodeGraphPermissions`
/// (shared.ts:37) — order preserved.
pub fn codegraph_permissions() -> Vec<&'static str> {
    vec![
        "mcp__codegraph__codegraph_explore",
        "mcp__codegraph__codegraph_search",
        "mcp__codegraph__codegraph_node",
        "mcp__codegraph__codegraph_callers",
        "mcp__codegraph__codegraph_callees",
        "mcp__codegraph__codegraph_impact",
        "mcp__codegraph__codegraph_files",
        "mcp__codegraph__codegraph_status",
    ]
}

/// Read a JSON file, returning `{}` when missing or unparseable. Ports
/// `readJsonFile` (shared.ts:57): an unparseable file is backed up to
/// `<path>.backup` before returning empty.
pub fn read_json_file(path: &Path) -> Map<String, Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => map,
        Ok(_) => Map::new(),
        Err(_) => {
            let _ = fs::copy(path, path.with_extension("backup"));
            Map::new()
        }
    }
}

/// Write a file atomically: write to `<path>.tmp.<pid>`, then rename.
/// Ports `atomicWriteFileSync` (shared.ts:80).
pub fn atomic_write_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    match fs::write(&tmp, content).and_then(|()| fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Atomic JSON write with a trailing newline. Ports `writeJsonFile`
/// (shared.ts:99) — `JSON.stringify(data, null, 2) + '\n'`.
pub fn write_json_file(path: &Path, data: &Map<String, Value>) -> std::io::Result<()> {
    let mut content = to_upstream_json(&Value::Object(data.clone()));
    content.push('\n');
    atomic_write_file(path, &content)
}

/// Serialize a JSON value exactly as `JSON.stringify(value, null, 2)` does:
/// 2-space indent, `": "` separators, no trailing spaces. `serde_json`'s pretty
/// printer matches this for the object/array/string/number shapes we write.
pub fn to_upstream_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
}

/// Replace or append a marker-delimited section. Ports
/// `replaceOrAppendMarkedSection` (shared.ts:141).
pub fn replace_or_append_marked_section(
    path: &Path,
    body: &str,
    start_marker: &str,
    end_marker: &str,
) -> FileAction {
    let Ok(content) = fs::read_to_string(path) else {
        let _ = atomic_write_file(path, &format!("{body}\n"));
        return FileAction::Created;
    };

    if let (Some(start_idx), Some(end_idx)) = (content.find(start_marker), content.find(end_marker))
    {
        if end_idx > start_idx {
            let block_end = end_idx + end_marker.len();
            let existing_block = &content[start_idx..block_end];
            if existing_block == body {
                return FileAction::Unchanged;
            }
            let before = &content[..start_idx];
            let after = &content[block_end..];
            let _ = atomic_write_file(path, &format!("{before}{body}{after}"));
            return FileAction::Updated;
        }
    }

    let trimmed = content.trim_end();
    let sep = if trimmed.is_empty() { "" } else { "\n\n" };
    let _ = atomic_write_file(path, &format!("{trimmed}{sep}{body}\n"));
    // The TS returns 'appended'; callers fold it into 'updated'.
    FileAction::Updated
}

/// Upsert the CodeGraph instructions block. Ports `upsertInstructionsEntry`
/// (shared.ts:185) — the `appended` action is folded into `Updated` there too.
pub fn upsert_instructions_entry(path: &Path) -> FileWrite {
    let action = replace_or_append_marked_section(
        path,
        CODEGRAPH_INSTRUCTIONS_BLOCK,
        CODEGRAPH_SECTION_START,
        CODEGRAPH_SECTION_END,
    );
    FileWrite {
        path: path.to_path_buf(),
        action,
    }
}

/// Strip the marker block, deleting the file if it becomes empty. Ports
/// `removeMarkedSection` (shared.ts:204).
pub fn remove_marked_section(path: &Path, start_marker: &str, end_marker: &str) -> FileAction {
    if !path.exists() {
        return FileAction::Kept;
    }
    let Ok(content) = fs::read_to_string(path) else {
        return FileAction::Kept;
    };
    let (Some(start_idx), Some(end_idx)) = (content.find(start_marker), content.find(end_marker))
    else {
        return FileAction::NotFound;
    };
    if end_idx <= start_idx {
        return FileAction::NotFound;
    }

    let before = content[..start_idx].trim_end();
    let after = content[end_idx + end_marker.len()..].trim_start();
    let join_sep = if !before.is_empty() && !after.is_empty() {
        "\n\n"
    } else {
        ""
    };
    let joined = format!("{before}{join_sep}{after}");

    if joined.trim().is_empty() {
        let _ = fs::remove_file(path);
    } else {
        let _ = atomic_write_file(path, &format!("{}\n", joined.trim()));
    }
    FileAction::Removed
}

/// Remove the `codegraph` MCP server from a `mcpServers` JSON config, pruning an
/// emptied `mcpServers` wrapper. Shared by the `mcpServers`-shaped targets
/// (Claude/Cursor/Gemini/Kiro/Antigravity uninstall). Returns whether it removed
/// anything; the caller persists.
pub fn remove_codegraph_from_mcp_servers(config: &mut Map<String, Value>) -> bool {
    let Some(Value::Object(servers)) = config.get_mut("mcpServers") else {
        return false;
    };
    if servers.remove("codegraph").is_none() {
        return false;
    }
    if servers.is_empty() {
        config.remove("mcpServers");
    }
    true
}

// === TOML helpers (port of toml.ts) ===

fn quote_toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Build a TOML table block: header line + body. Ports `buildTomlTable`
/// (toml.ts:52) restricted to string / string-array values.
pub fn build_toml_table(header: &str, values: &[(&str, TomlValue<'_>)]) -> String {
    let mut lines = vec![format!("[{header}]")];
    for (key, value) in values {
        match value {
            TomlValue::Str(s) => lines.push(format!("{key} = {}", quote_toml_string(s))),
            TomlValue::Array(items) => {
                let parts = items
                    .iter()
                    .map(|v| quote_toml_string(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("{key} = [{parts}]"));
            }
        }
    }
    lines.join("\n")
}

pub enum TomlValue<'a> {
    Str(&'a str),
    Array(Vec<&'a str>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum TomlUpsert {
    Inserted,
    Replaced,
    Unchanged,
}

/// Insert or replace a top-level dotted-key TOML table block. Ports
/// `upsertTomlTable` (toml.ts:64).
pub fn upsert_toml_table(content: &str, header: &str, block: &str) -> (String, TomlUpsert) {
    let header_line = format!("[{header}]");
    let Some(header_idx) = find_header_index(content, &header_line) else {
        let trimmed = content.trim_end();
        let sep = if trimmed.is_empty() { "" } else { "\n\n" };
        return (format!("{trimmed}{sep}{block}\n"), TomlUpsert::Inserted);
    };

    let block_end = find_next_table_header(content, header_idx + header_line.len());
    let existing_block = content[header_idx..block_end].trim_end_matches('\n');
    if existing_block == block {
        return (content.to_string(), TomlUpsert::Unchanged);
    }

    let before_clean = content[..header_idx].trim_end_matches('\n');
    let after_clean = content[block_end..].trim_start_matches('\n');
    let sep_before = if before_clean.is_empty() { "" } else { "\n\n" };
    let sep_after = if after_clean.is_empty() { "\n" } else { "\n\n" };
    (
        format!("{before_clean}{sep_before}{block}{sep_after}{after_clean}"),
        TomlUpsert::Replaced,
    )
}

/// Remove a top-level dotted-key TOML table block. Ports `removeTomlTable`
/// (toml.ts:108).
pub fn remove_toml_table(content: &str, header: &str) -> (String, bool) {
    let header_line = format!("[{header}]");
    let Some(header_idx) = find_header_index(content, &header_line) else {
        return (content.to_string(), false);
    };
    let block_end = find_next_table_header(content, header_idx + header_line.len());
    let before = content[..header_idx].trim_end_matches('\n');
    let after = content[block_end..].trim_start_matches('\n');
    let sep = if !before.is_empty() && !after.is_empty() {
        "\n\n"
    } else {
        ""
    };
    (format!("{before}{sep}{after}"), true)
}

/// Ports `findHeaderIndex` (toml.ts:127): header at BOL or right after a newline.
fn find_header_index(content: &str, header_line: &str) -> Option<usize> {
    if content.starts_with(header_line) {
        return Some(0);
    }
    let needle = format!("\n{header_line}");
    content.find(&needle).map(|idx| idx + 1)
}

/// Ports `findNextTableHeader` (toml.ts:140): next `\n[` skipping `\n[[`.
fn find_next_table_header(content: &str, from: usize) -> usize {
    let bytes = content.as_bytes();
    let mut i = from;
    while i < content.len() {
        match content[i..].find("\n[") {
            None => return content.len(),
            Some(rel) => {
                let nl_idx = i + rel;
                if bytes.get(nl_idx + 2) == Some(&b'[') {
                    i = nl_idx + 2;
                    continue;
                }
                return nl_idx + 1;
            }
        }
    }
    content.len()
}
