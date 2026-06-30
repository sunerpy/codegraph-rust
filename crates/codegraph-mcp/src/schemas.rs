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

/// Clone `tools` and ensure every tool's `inputSchema.required` array contains
/// `"projectPath"`. Used when no default project is resolved so a roots-less
/// client's agent is nudged to supply the path per call. Ports colby
/// `withRequiredProjectPath` (`tools.ts:736-746`, #993 / PR#1007). Pure +
/// idempotent: applying twice equals applying once (never duplicates
/// `"projectPath"`). Does NOT mutate the embedded `tools_list.json` or the
/// caller's value — operates on a clone.
fn with_required_project_path(tools: Value) -> Value {
    let Value::Array(arr) = tools else {
        return tools;
    };
    let mapped: Vec<Value> = arr
        .into_iter()
        .map(|mut tool| {
            if let Some(schema) = tool.get_mut("inputSchema").and_then(Value::as_object_mut) {
                match schema.get_mut("required").and_then(Value::as_array_mut) {
                    Some(required) => {
                        if !required.iter().any(|v| v == "projectPath") {
                            required.push(Value::String("projectPath".to_string()));
                        }
                    }
                    None => {
                        schema.insert(
                            "required".to_string(),
                            Value::Array(vec![Value::String("projectPath".to_string())]),
                        );
                    }
                }
            }
            tool
        })
        .collect();
    Value::Array(mapped)
}

/// The visible tool surface with `projectPath` marked required in every tool's
/// `inputSchema.required` — served when no default project is resolved (#94 /
/// colby #964 always-expose + #993 required-projectPath).
pub fn visible_tool_definitions_requiring_project_path() -> Value {
    with_required_project_path(visible_tool_definitions())
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

    fn required_of<'a>(tools: &'a Value, name: &str) -> &'a Vec<Value> {
        tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} present"))["inputSchema"]["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} has a required array"))
    }

    #[test]
    fn with_required_project_path_covers_all_three_shapes() {
        let fixture = serde_json::json!([
            { "name": "with_keys", "inputSchema": { "type": "object", "required": ["query"] } },
            { "name": "empty", "inputSchema": { "type": "object", "required": [] } },
            { "name": "absent", "inputSchema": { "type": "object" } }
        ]);
        let out = with_required_project_path(fixture);

        let with_keys = required_of(&out, "with_keys");
        assert!(with_keys.iter().any(|v| v == "query"), "keeps existing key");
        assert!(with_keys.iter().any(|v| v == "projectPath"), "appends");

        assert_eq!(
            required_of(&out, "empty"),
            &vec![Value::String("projectPath".to_string())],
            "empty required gains projectPath"
        );
        assert_eq!(
            required_of(&out, "absent"),
            &vec![Value::String("projectPath".to_string())],
            "absent required key is inserted as [\"projectPath\"]"
        );
    }

    #[test]
    fn with_required_project_path_is_idempotent() {
        let once = with_required_project_path(visible_tool_definitions());
        let twice = with_required_project_path(once.clone());
        assert_eq!(once, twice, "applying twice equals applying once");
        for tool in twice.as_array().unwrap() {
            let count = tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| *v == "projectPath")
                .count();
            assert_eq!(count, 1, "no duplicate projectPath in {}", tool["name"]);
        }
    }

    #[test]
    fn visible_definitions_unchanged_by_required_transform() {
        let plain = visible_tool_definitions();
        let _ = visible_tool_definitions_requiring_project_path();
        assert_eq!(
            plain,
            visible_tool_definitions(),
            "the transform must not mutate the embedded definitions"
        );
        for tool in plain.as_array().unwrap() {
            let has_pp = tool["inputSchema"]["required"]
                .as_array()
                .map(|r| r.iter().any(|v| v == "projectPath"))
                .unwrap_or(false);
            assert!(
                !has_pp,
                "plain visible tool {} keeps projectPath OPTIONAL",
                tool["name"]
            );
        }
    }
}
