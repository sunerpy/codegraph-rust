//! The MCP tool definitions (`tools/list` payload).
//!
//! The first 8 entries are embedded VERBATIM from the upstream live `tools/list`
//! response (`upstream mcp/tools.ts:378-568`), captured into
//! `tools_list.json`, so their names + input schemas are byte-identical to
//! the upstream (`codegraph_trace`/`codegraph_context` do NOT exist in the pinned
//! snapshot). `codegraph_check` is an ADDITIVE, non-upstream analysis tool (cycle
//! detection) that runs ahead of the v1.0.1 pin; it is NOT part of the default
//! surface, so the golden default `tools/list` stays byte-equal.

use serde_json::Value;

/// The raw tool-definition array (8 verbatim upstream tools + additive
/// `codegraph_check`).
const TOOLS_JSON: &str = include_str!("tools_list.json");

/// Every known tool name, in definition order. The first 8 are the upstream's
/// (`tools.ts:378-568`); `codegraph_explore` is the PRIMARY tool.
/// `codegraph_check` (cycle detection) and `codegraph_export` (full node-link
/// graph dump) are additive, non-upstream analysis tools.
pub const TOOL_NAMES: [&str; 10] = [
    "codegraph_search",
    "codegraph_callers",
    "codegraph_callees",
    "codegraph_impact",
    "codegraph_node",
    "codegraph_explore",
    "codegraph_status",
    "codegraph_files",
    "codegraph_check",
    "codegraph_export",
];

/// Returns the `tools/list` array exactly as the upstream serves it.
///
/// Mirrors `ToolHandler.getTools()` → the static `tools` array
/// (`tools.ts:378`, dispatched by the session at `session.ts:197-202`).
pub fn tool_definitions() -> Value {
    serde_json::from_str(TOOLS_JSON).expect("embedded tools_list.json is valid JSON")
}

/// Short names of the tools served by DEFAULT (`DEFAULT_MCP_TOOLS`,
/// `tools.ts:740`). The other tools stay fully functional and callable; they
/// are just not LISTED unless re-enabled via `CODEGRAPH_MCP_TOOLS`.
const DEFAULT_MCP_TOOLS: [&str; 4] = ["explore", "node", "search", "callers"];

fn short_name(name: &str) -> &str {
    name.strip_prefix("codegraph_").unwrap_or(name)
}

/// Optional allowlist parsed from `CODEGRAPH_MCP_TOOLS` (comma-separated short
/// names). `None` when unset/empty. Ports `toolAllowlist` (`tools.ts:711`).
fn tool_allowlist() -> Option<std::collections::HashSet<String>> {
    let raw = std::env::var("CODEGRAPH_MCP_TOOLS").ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let set: std::collections::HashSet<String> = raw
        .split(',')
        .map(|s| short_name(s.trim()).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!set.is_empty()).then_some(set)
}

/// The `tools/list` surface: the default 4-tool set, or — when
/// `CODEGRAPH_MCP_TOOLS` is set — exactly that allowlist (any known tool).
/// Ports `ToolHandler.getTools()` allowlist branch (`tools.ts:733-740`).
pub fn visible_tool_definitions() -> Value {
    let all = tool_definitions();
    let arr = match all.as_array() {
        Some(a) => a,
        None => return all,
    };
    let allow = tool_allowlist();
    let visible: Vec<Value> = arr
        .iter()
        .filter(|tool| {
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .map(short_name)
                .unwrap_or("");
            match &allow {
                Some(set) => set.contains(name),
                None => DEFAULT_MCP_TOOLS.contains(&name),
            }
        })
        .cloned()
        .collect();
    Value::Array(visible)
}

/// True if `name` is one of the 8 known tools. Used to reject unknown tool
/// names with a JSON-RPC `-32602` error (`session.ts:217-225`).
pub fn is_known_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_has_upstream_eight_plus_additive_check() {
        let tools = tool_definitions();
        let arr = tools.as_array().expect("tools is an array");
        assert_eq!(
            arr.len(),
            10,
            "8 verbatim upstream tools + additive codegraph_check + codegraph_export"
        );
        let names: Vec<&str> = arr
            .iter()
            .map(|t| t["name"].as_str().expect("each tool has a name"))
            .collect();
        assert_eq!(names, TOOL_NAMES);
        assert_eq!(
            &names[..8],
            &TOOL_NAMES[..8],
            "first 8 stay byte-identical to the golden pinned tools/list"
        );
        assert!(
            !DEFAULT_MCP_TOOLS.contains(&"check"),
            "codegraph_check is additive, not part of the default surface"
        );
    }

    #[test]
    fn trace_and_context_tools_do_not_exist() {
        let tools = tool_definitions();
        let arr = tools.as_array().unwrap();
        for forbidden in ["codegraph_trace", "codegraph_context"] {
            assert!(
                !arr.iter().any(|t| t["name"] == forbidden),
                "{forbidden} must NOT exist in the pinned snapshot"
            );
        }
    }
}
