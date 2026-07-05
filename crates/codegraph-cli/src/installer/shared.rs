//! Helpers shared across `AgentTarget` implementations.
//!
//! Ports `upstream installer/targets/shared.ts`,
//! `instructions-template.ts`, and `toml.ts`.

use std::fs;
use std::path::Path;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstInputValue, CstRootNode};
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

/// Idempotency sentinel: presence of this substring means the HTTP hint is
/// already injected, so [`inject_kiro_http_comment`] becomes a no-op.
pub const KIRO_HTTP_COMMENT_SENTINEL: &str = "// HTTP alternative";

/// The `//`-commented HTTP alternative injected into Kiro's JSONC `mcp.json`
/// alongside the active stdio entry. localhost is mandatory: Kiro allows http
/// only for localhost (remote servers must be https; a non-local http url is
/// silently rejected), and localhost also sidesteps HTTP_PROXY.
pub fn kiro_http_comment_block() -> &'static str {
    "// HTTP alternative — run `codegraph serve --http` (defaults to 127.0.0.1:8111),\n\
     // then uncomment below and remove the stdio entry above. stdio is primary\n\
     // (works out of the box; live watch when project-local). HTTP needs a\n\
     // separate `codegraph serve --http`, and Kiro allows http ONLY for localhost\n\
     // (remote servers must be https), so the url MUST use localhost:\n\
     // \"codegraph\": { \"url\": \"http://localhost:8111/mcp\" }"
}

/// Idempotency sentinel for [`zed_remote_comment_block`].
pub const ZED_REMOTE_COMMENT_SENTINEL: &str = "// Remote development alternatives";

/// The `//`-commented remote-development alternatives injected into Zed's JSONC
/// `settings.json` alongside the active stdio `context_servers.codegraph` entry.
///
/// Two options: (1) SSH stdio — run codegraph on the remote host over `ssh -T`;
/// (2) HTTP, marked RECOMMENDED for remote (a single `codegraph serve --http`
/// avoids the extra ssh hop + stdio buffering fragility). Zed's HTTP context-
/// server shape is a bare `{ "url": … }` (verified against Zed's MCP docs +
/// source: untagged enum, `url` is the sole discriminator, no `type`/`source`).
pub fn zed_remote_comment_block() -> &'static str {
    "// Remote development alternatives (uncomment ONE; remove the stdio entry above):\n\
     // 1) SSH stdio — run codegraph on the remote host over `ssh -T`:\n\
     //    \"codegraph\": { \"command\": \"ssh\", \"args\": [\"-T\", \"<host>\", \"cd <project> && codegraph serve --mcp --path <project>\"], \"env\": {} }\n\
     // 2) HTTP (RECOMMENDED for remote) — start `codegraph serve --http` (default\n\
     //    127.0.0.1:8111) on the reachable host, or port-forward it, then point Zed\n\
     //    at the url. HTTP avoids the extra ssh hop + stdio buffering that make\n\
     //    ssh-stdio fragile over Zed remote development. Zed's HTTP context-server\n\
     //    shape is a bare `url` (see https://zed.dev/docs/ai/mcp):\n\
     //    \"codegraph\": { \"url\": \"http://localhost:8111/mcp\" }"
}

/// Best-effort idempotent injection of the [`kiro_http_comment_block`] after the
/// active stdio `codegraph` entry inside `mcpServers`. See
/// [`inject_commented_alternative`] for the shared mechanics.
pub fn inject_kiro_http_comment(path: &Path) -> bool {
    inject_commented_alternative(
        path,
        "mcpServers",
        "codegraph",
        KIRO_HTTP_COMMENT_SENTINEL,
        kiro_http_comment_block(),
    )
}

/// Best-effort idempotent injection of the [`zed_remote_comment_block`] after the
/// active stdio `codegraph` entry inside `context_servers`.
pub fn inject_zed_remote_comment(path: &Path) -> bool {
    inject_commented_alternative(
        path,
        "context_servers",
        "codegraph",
        ZED_REMOTE_COMMENT_SENTINEL,
        zed_remote_comment_block(),
    )
}

/// Append a `//`-commented `comment_block` right after the active
/// `<parent_key>.<entry_key>` object inside a JSONC file, matching the entry's
/// indentation. Idempotent (no-op if `sentinel` is already present) and
/// non-corrupting: no-op when the file is unreadable or the parent/entry cannot
/// be located, and the active entry itself is never touched. Correctness over
/// always injecting.
pub fn inject_commented_alternative(
    path: &Path,
    parent_key: &str,
    entry_key: &str,
    sentinel: &str,
    comment_block: &str,
) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    if text.contains(sentinel) {
        return false;
    }
    let parent_needle = format!("\"{parent_key}\"");
    let Some(anchor) = text.find(&parent_needle) else {
        return false;
    };
    let Some(brace_rel) = text[anchor..].find('{') else {
        return false;
    };
    let obj_open = anchor + brace_rel;
    let entry_needle = format!("\"{entry_key}\"");
    let Some(cg_rel) = text[obj_open..].find(&entry_needle) else {
        return false;
    };
    let cg_at = obj_open + cg_rel;
    let bytes = text.as_bytes();
    let Some(val_open_rel) = text[cg_at..].find('{') else {
        return false;
    };
    let Some(mut end) = balanced_object_end(bytes, cg_at + val_open_rel) else {
        return false;
    };
    if bytes.get(end) == Some(&b',') {
        end += 1;
    }
    let line_start = text[..cg_at].rfind('\n').map_or(0, |n| n + 1);
    let indent: String = text[line_start..cg_at]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let block = comment_block
        .lines()
        .map(|l| format!("{indent}{l}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::with_capacity(text.len() + block.len() + indent.len() + 2);
    out.push_str(&text[..end]);
    out.push('\n');
    out.push_str(&block);
    out.push_str(&text[end..]);
    atomic_write_file(path, &out).is_ok()
}

/// Return the index just past the matching `}` for the object that opens at
/// `open` (which must index a `{`), tracking string/escape state so braces
/// inside string literals do not miscount. `None` if unbalanced.
fn balanced_object_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
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

/// Data-safety invariant: a missing file is safe to treat as empty (fresh
/// install), but an existing-yet-unparseable file must NEVER become `{}` — that
/// empty map, written back, destroys the user's real config. Write paths abort
/// on `Unparseable`.
pub enum ConfigRead {
    Missing,
    Parsed(Map<String, Value>),
    Unparseable,
}

/// Strip `//` line comments and `/* */` block comments from JSONC, preserving
/// anything inside string literals (so `"a//b"` and `"a/*b*/c"` survive). Also
/// drops a single trailing comma before `}`/`]`. Pure string transform — no I/O.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    drop_trailing_commas(&out)
}

/// Remove `,` that is immediately followed (ignoring whitespace) by `}` or `]`.
/// Runs on comment-stripped text, so it never sees commas inside comments;
/// commas inside strings are guarded by tracking string state.
fn drop_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b',' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                i += 1;
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Parse JSON, then JSONC (comments / trailing commas) as a fallback. Returns the
/// top-level object map, or `None` if neither parse yields a JSON object.
pub fn parse_json_object(text: &str) -> Option<Map<String, Value>> {
    if text.trim().is_empty() {
        return Some(Map::new());
    }
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) {
        return Some(map);
    }
    match serde_json::from_str::<Value>(&strip_jsonc(text)) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// Read an agent config file, distinguishing missing / parsed / unparseable.
///
/// Tolerates JSONC (comments + trailing commas). A present-but-unparseable file
/// is backed up to `<path>.backup` and reported as [`ConfigRead::Unparseable`]
/// WITHOUT being modified, so the caller can skip writing instead of clobbering
/// the user's config.
pub fn read_config_file(path: &Path) -> ConfigRead {
    let Ok(text) = fs::read_to_string(path) else {
        return ConfigRead::Missing;
    };
    match parse_json_object(&text) {
        Some(map) => ConfigRead::Parsed(map),
        None => {
            let _ = fs::copy(path, path.with_extension("backup"));
            ConfigRead::Unparseable
        }
    }
}

/// Read a JSON/JSONC file into a map. Backward-compatible helper that maps
/// [`ConfigRead::Missing`] to `{}`. Callers on the WRITE path must NOT use this
/// (it cannot signal the unparseable case); use [`read_config_file`] and abort
/// on [`ConfigRead::Unparseable`] there.
pub fn read_json_file(path: &Path) -> Map<String, Value> {
    match read_config_file(path) {
        ConfigRead::Parsed(map) => map,
        ConfigRead::Missing | ConfigRead::Unparseable => Map::new(),
    }
}

/// Write a file atomically: write to `<path>.tmp.<pid>`, then rename.
/// Ports `atomicWriteFileSync` (shared.ts:80).
pub fn atomic_write_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)?;
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

// jsonc-parser 0.26 `CstStringLit::new_escaped` escapes only `"`, not `\` or
// control chars, so a Windows path `C:\Users` emits invalid JSON `"C:\Users"`.
// Pre-escape everything JSON-significant EXCEPT `"` (the library owns quotes).
fn escape_for_cst_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn to_cst_input(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(b) => CstInputValue::Bool(*b),
        Value::Number(n) => CstInputValue::Number(n.to_string()),
        Value::String(s) => CstInputValue::String(escape_for_cst_string(s)),
        Value::Array(arr) => CstInputValue::Array(arr.iter().map(to_cst_input).collect()),
        Value::Object(map) => CstInputValue::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), to_cst_input(v)))
                .collect(),
        ),
    }
}

/// Surgically upsert `<parent_key>.<leaf_key> = value` in JSONC, preserving the
/// file's comments, key order, and formatting. `Unchanged` when the leaf already
/// equals `value` (no write — avoids comment churn). Caller must pre-confirm the
/// file parses (see `read_config_file`); a parse failure returns an error so the
/// caller skips rather than clobbers.
pub fn upsert_nested_key_jsonc(
    path: &Path,
    parent_key: &str,
    leaf_key: &str,
    value: &Value,
    schema_url: Option<&str>,
) -> std::io::Result<FileAction> {
    let text = fs::read_to_string(path)?;
    let existed = !text.trim().is_empty();

    let parsed = parse_json_object(&text);
    if let Some(map) = &parsed {
        let current = map.get(parent_key).and_then(|p| p.get(leaf_key));
        if current == Some(value) && (schema_url.is_none() || map.contains_key("$schema")) {
            return Ok(FileAction::Unchanged);
        }
    }

    let root = CstRootNode::parse(&text, &ParseOptions::default())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let root_obj = root.object_value_or_set();
    let parent = root_obj.object_value_or_create(parent_key).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("`{parent_key}` exists but is not an object"),
        )
    })?;
    match parent.get(leaf_key) {
        Some(prop) => prop.set_value(to_cst_input(value)),
        None => {
            parent.append(leaf_key, to_cst_input(value));
        }
    }
    if let Some(schema) = schema_url
        && root_obj.get("$schema").is_none()
    {
        root_obj.insert(0, "$schema", CstInputValue::String(schema.to_string()));
    }

    let mut out = root.to_string();
    if text.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    atomic_write_file(path, &out)?;
    Ok(if existed {
        FileAction::Updated
    } else {
        FileAction::Created
    })
}

/// Surgically remove `<parent_key>.<leaf_key>` from a JSONC file, preserving
/// surrounding comments/formatting. Drops the now-empty parent object too.
/// Returns `Removed` if the key was present, `NotFound` otherwise.
pub fn remove_nested_key_jsonc(
    path: &Path,
    parent_key: &str,
    leaf_key: &str,
) -> std::io::Result<FileAction> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(FileAction::NotFound);
    };
    let present = parse_json_object(&text)
        .and_then(|m| m.get(parent_key).and_then(|p| p.get(leaf_key)).cloned())
        .is_some();
    if !present {
        return Ok(FileAction::NotFound);
    }
    let root = CstRootNode::parse(&text, &ParseOptions::default())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if let Some(parent) = root.object_value().and_then(|o| o.object_value(parent_key)) {
        if let Some(prop) = parent.get(leaf_key) {
            prop.remove();
        }
        if parent.properties().is_empty()
            && let Some(parent_prop) = root.object_value().and_then(|o| o.get(parent_key))
        {
            parent_prop.remove();
        }
    }
    let mut out = root.to_string();
    if text.ends_with('\n') && !out.ends_with('\n') {
        out.push('\n');
    }
    atomic_write_file(path, &out)?;
    Ok(FileAction::Removed)
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
        && end_idx > start_idx
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonc_with_line_and_block_comments() {
        let text = r#"{
            // a line comment
            "a": 1, /* inline */
            "b": "keep // and /* */ inside string"
        }"#;
        let map = parse_json_object(text).expect("JSONC must parse");
        assert_eq!(map.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(
            map.get("b"),
            Some(&serde_json::json!("keep // and /* */ inside string"))
        );
    }

    #[test]
    fn parses_jsonc_with_trailing_commas() {
        let map = parse_json_object("{ \"a\": [1, 2,], \"b\": 3, }").expect("trailing commas ok");
        assert_eq!(map.get("a"), Some(&serde_json::json!([1, 2])));
        assert_eq!(map.get("b"), Some(&serde_json::json!(3)));
    }

    #[test]
    fn truly_corrupt_text_is_unparseable() {
        assert!(parse_json_object("{ this is not json").is_none());
    }

    #[test]
    fn jsonc_preserves_escaped_quote_inside_string() {
        let map = parse_json_object(r#"{ "a": "he said \"hi\" // x" }"#).expect("parse");
        assert_eq!(
            map.get("a"),
            Some(&serde_json::json!("he said \"hi\" // x"))
        );
    }

    #[test]
    fn empty_text_parses_to_empty_map() {
        assert_eq!(parse_json_object("   \n"), Some(Map::new()));
    }

    #[test]
    fn to_upstream_json_renders_null_and_number() {
        let value = serde_json::json!({ "n": null, "x": 42, "arr": [1, 2] });
        let out = to_upstream_json(&value);
        assert!(out.contains("\"n\": null"));
        assert!(out.contains("\"x\": 42"));
    }

    #[test]
    fn upsert_nested_key_via_cst_handles_null_and_number_values() {
        let p = tmp_path("cst.json");
        fs::write(&p, "{\n  \"cfg\": {}\n}\n").unwrap();
        let value = serde_json::json!({ "n": null, "x": 7, "on": true, "name": "cg" });
        let action = upsert_nested_key_jsonc(&p, "cfg", "codegraph", &value, None).unwrap();
        assert_eq!(action, FileAction::Updated);
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("\"codegraph\""));
        assert!(out.contains("null"));
        assert!(out.contains('7'));
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn upsert_nested_key_via_cst_handles_backslash_and_control_chars() {
        let p = tmp_path("cst-backslash.json");
        fs::write(
            &p,
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();
        let win = "C:\\Users\\me\\proj\"q\ttab";
        let value = serde_json::json!({ "command": "codegraph", "args": ["--path", win] });
        upsert_nested_key_jsonc(&p, "mcpServers", "codegraph", &value, None).unwrap();
        let out = fs::read_to_string(&p).unwrap();
        let reparsed = parse_json_object(&out).expect("emitted JSONC must re-parse");
        assert_eq!(
            reparsed["mcpServers"]["codegraph"]["args"][1]
                .as_str()
                .unwrap(),
            win
        );
        assert!(reparsed["mcpServers"]["other"].is_object());
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn remove_nested_key_absent_is_not_found() {
        let p = tmp_path("absent.json");
        fs::write(&p, "{\n  \"other\": 1\n}\n").unwrap();
        let action = remove_nested_key_jsonc(&p, "mcpServers", "codegraph").unwrap();
        assert_eq!(action, FileAction::NotFound);
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn marked_section_created_then_updated_then_removed() {
        let p = tmp_path("AGENTS.md");
        let start = CODEGRAPH_SECTION_START;
        let end = CODEGRAPH_SECTION_END;

        let block_v1 = format!("{start}\nv1\n{end}");
        let created = replace_or_append_marked_section(&p, &block_v1, start, end);
        assert_eq!(created, FileAction::Created);

        let unchanged = replace_or_append_marked_section(&p, &block_v1, start, end);
        assert_eq!(unchanged, FileAction::Unchanged);

        let block_v2 = format!("{start}\nv2\n{end}");
        let updated = replace_or_append_marked_section(&p, &block_v2, start, end);
        assert_eq!(updated, FileAction::Updated);
        assert!(fs::read_to_string(&p).unwrap().contains("v2"));

        let removed = remove_marked_section(&p, start, end);
        assert_eq!(removed, FileAction::Removed);
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn marked_section_appends_to_existing_body() {
        let p = tmp_path("doc.md");
        fs::write(&p, "existing user content\n").unwrap();
        let block = format!("{CODEGRAPH_SECTION_START}\nours\n{CODEGRAPH_SECTION_END}");
        let action = replace_or_append_marked_section(
            &p,
            &block,
            CODEGRAPH_SECTION_START,
            CODEGRAPH_SECTION_END,
        );
        assert_eq!(action, FileAction::Updated);
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("existing user content"));
        assert!(out.contains("ours"));
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn remove_marked_section_kept_when_missing_file() {
        let p = tmp_path("nope.md");
        assert_eq!(
            remove_marked_section(&p, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END),
            FileAction::Kept
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn remove_marked_section_not_found_when_no_markers() {
        let p = tmp_path("plain.md");
        fs::write(&p, "no markers here\n").unwrap();
        assert_eq!(
            remove_marked_section(&p, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END),
            FileAction::NotFound
        );
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn remove_codegraph_from_mcp_servers_variants() {
        let mut absent = Map::new();
        assert!(!remove_codegraph_from_mcp_servers(&mut absent));

        let mut no_key: Map<String, Value> =
            serde_json::from_str("{ \"mcpServers\": { \"other\": { \"command\": \"x\" } } }")
                .unwrap();
        assert!(!remove_codegraph_from_mcp_servers(&mut no_key));

        let mut only_cg: Map<String, Value> =
            serde_json::from_str("{ \"mcpServers\": { \"codegraph\": {} } }").unwrap();
        assert!(remove_codegraph_from_mcp_servers(&mut only_cg));
        assert!(
            only_cg.get("mcpServers").is_none(),
            "emptied wrapper pruned"
        );

        let mut with_sibling: Map<String, Value> =
            serde_json::from_str("{ \"mcpServers\": { \"codegraph\": {}, \"other\": {} } }")
                .unwrap();
        assert!(remove_codegraph_from_mcp_servers(&mut with_sibling));
        assert!(with_sibling["mcpServers"].get("other").is_some());
    }

    #[test]
    fn toml_table_insert_replace_unchanged_remove() {
        let block = build_toml_table(
            "mcp_servers.codegraph",
            &[
                ("command", TomlValue::Str("codegraph")),
                ("args", TomlValue::Array(vec!["serve", "--mcp"])),
            ],
        );
        assert!(block.contains("[mcp_servers.codegraph]"));
        assert!(block.contains("command = \"codegraph\""));
        assert!(block.contains("args = [\"serve\", \"--mcp\"]"));

        let (inserted, kind) = upsert_toml_table("", "mcp_servers.codegraph", &block);
        assert_eq!(kind, TomlUpsert::Inserted);
        assert!(inserted.contains("[mcp_servers.codegraph]"));

        let (unchanged, kind) = upsert_toml_table(&inserted, "mcp_servers.codegraph", &block);
        assert_eq!(kind, TomlUpsert::Unchanged);
        assert_eq!(unchanged, inserted);

        let stale = "[mcp_servers.codegraph]\ncommand = \"old\"\n\n[other]\nx = 1\n";
        let (replaced, kind) = upsert_toml_table(stale, "mcp_servers.codegraph", &block);
        assert_eq!(kind, TomlUpsert::Replaced);
        assert!(replaced.contains("command = \"codegraph\""));
        assert!(replaced.contains("[other]"));

        let (removed, did_remove) = remove_toml_table(&replaced, "mcp_servers.codegraph");
        assert!(did_remove);
        assert!(!removed.contains("[mcp_servers.codegraph]"));
        assert!(removed.contains("[other]"));

        let (unchanged_remove, did_remove) =
            remove_toml_table("[other]\nx = 1\n", "mcp_servers.codegraph");
        assert!(!did_remove);
        assert!(unchanged_remove.contains("[other]"));
    }

    #[test]
    fn toml_string_escaping_quotes_and_backslashes() {
        let block = build_toml_table("h", &[("path", TomlValue::Str("C:\\a\\\"b\""))]);
        assert!(block.contains("\\\\"));
        assert!(block.contains("\\\""));
    }

    #[test]
    fn toml_next_header_skips_array_of_tables() {
        // Skipping `\n[[` folds the `[[b]]` block into `[a]`, so removing `[a]`
        // also removes `[[b]]`, stopping at `[c]`.
        let content = "[a]\nx = 1\n\n[[b]]\ny = 2\n\n[c]\nz = 3\n";
        let (removed, did) = remove_toml_table(content, "a");
        assert!(did);
        assert!(!removed.contains("[a]"));
        assert!(removed.contains("[c]"));
    }

    #[test]
    fn read_outcome_missing_parsed_unparseable() {
        let dir = std::env::temp_dir().join(format!(
            "cg-shared-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("missing.json");
        assert!(matches!(read_config_file(&missing), ConfigRead::Missing));

        let jsonc = dir.join("config.jsonc");
        fs::write(&jsonc, "{\n  // c\n  \"x\": 1,\n}\n").unwrap();
        match read_config_file(&jsonc) {
            ConfigRead::Parsed(map) => assert_eq!(map.get("x"), Some(&serde_json::json!(1))),
            other => panic!("expected Parsed, got {:?}", std::mem::discriminant(&other)),
        }

        let corrupt = dir.join("corrupt.json");
        let raw = "{ broken not json";
        fs::write(&corrupt, raw).unwrap();
        assert!(matches!(
            read_config_file(&corrupt),
            ConfigRead::Unparseable
        ));
        assert_eq!(
            fs::read_to_string(&corrupt).unwrap(),
            raw,
            "unparseable file must be left byte-for-byte unchanged"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-jsonc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn upsert_preserves_comments_and_key_order() {
        let p = tmp_path("opencode.json");
        let original = "{\n  // lead comment\n  \"$schema\": \"https://x\",\n  \"theme\": \"dark\", // trailing\n  \"mcp\": {\n    \"existing\": { \"enabled\": true }\n  },\n  \"zzz\": \"last\"\n}\n";
        fs::write(&p, original).unwrap();
        let value = serde_json::json!({"type": "local", "enabled": true});
        let action =
            upsert_nested_key_jsonc(&p, "mcp", "codegraph", &value, Some("https://x")).unwrap();
        assert_eq!(action, FileAction::Updated);
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("// lead comment"), "lead comment preserved");
        assert!(out.contains("// trailing"), "trailing comment preserved");
        assert!(out.contains("\"existing\""), "sibling preserved");
        assert!(out.contains("\"codegraph\""), "codegraph inserted");
        let theme_at = out.find("\"theme\"").unwrap();
        let zzz_at = out.find("\"zzz\"").unwrap();
        assert!(theme_at < zzz_at, "original key order preserved");
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn upsert_is_idempotent() {
        let p = tmp_path("conf.json");
        fs::write(&p, "{\n  // keep\n  \"mcpServers\": {}\n}\n").unwrap();
        let value = serde_json::json!({"command": "codegraph"});
        let first = upsert_nested_key_jsonc(&p, "mcpServers", "codegraph", &value, None).unwrap();
        assert_eq!(first, FileAction::Updated);
        let after_first = fs::read_to_string(&p).unwrap();
        let second = upsert_nested_key_jsonc(&p, "mcpServers", "codegraph", &value, None).unwrap();
        assert_eq!(second, FileAction::Unchanged);
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            after_first,
            "no churn on re-run"
        );
        assert!(after_first.contains("// keep"));
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn remove_preserves_comments() {
        let p = tmp_path("conf.json");
        fs::write(
            &p,
            "{\n  // keep me\n  \"mcpServers\": {\n    \"other\": 1,\n    \"codegraph\": { \"x\": 1 }\n  }\n}\n",
        )
        .unwrap();
        let action = remove_nested_key_jsonc(&p, "mcpServers", "codegraph").unwrap();
        assert_eq!(action, FileAction::Removed);
        let out = fs::read_to_string(&p).unwrap();
        assert!(out.contains("// keep me"), "comment preserved on remove");
        assert!(out.contains("\"other\""), "sibling preserved");
        assert!(!out.contains("\"codegraph\""), "codegraph removed");
        let _ = fs::remove_dir_all(p.parent().unwrap());
    }
}
