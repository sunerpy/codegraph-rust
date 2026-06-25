//! The `CodeGraphEngine` — opens a project's store and renders the 8 MCP tool
//! responses with text output byte-aligned to the upstream renderers in
//! `upstream mcp/tools.ts`.
//!
//! Tool LOGIC is sync (rusqlite); only the stdio transport loop is async-free
//! here too — the server owns the loop. Each handler cites the upstream source
//! file:line for its API call + output template.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use codegraph_core::types::{FileRecord, Node, NodeKind};
use codegraph_graph::graph::{GodotReach, GraphTraverser, NodeEdge};
use codegraph_graph::query::{search_nodes, SearchOptions};
use codegraph_store::Store;
use serde_json::Value;

use crate::dynamic_boundaries::scan_dynamic_dispatch;
use crate::explore_budget::{get_explore_budget, get_explore_output_budget, ExploreOutputBudget};
use crate::protocol::ToolResult;

/// Default caller/callee recursion depth for callers/callees tools. The upstream
/// `getCallers`/`getCallees` default to `maxDepth: 1` (`traversal.ts` callers
/// list a single hop); the MCP tools call them with no depth override.
const CALL_DEPTH: usize = 1;

/// `TRAIL_CAP` from `tools.ts` — max callees/callers shown in a node trail.
const TRAIL_CAP: usize = 8;

/// `Read`-tool cap mirrored by file-mode `codegraph_node` (`tools.ts:489`).
const FILE_MODE_MAX_LINES: usize = 2000;

/// Holds an opened project store. One engine per project path; the server keeps
/// a cache keyed by resolved project path (mirrors `ToolHandler.projectCache`,
/// `tools.ts:591`).
pub struct CodeGraphEngine {
    store: Store,
    project_root: PathBuf,
}

impl CodeGraphEngine {
    /// Open the store at `<project_root>/.codegraph/codegraph.db`
    /// (`upstream directory.ts`; learnings Task 4).
    pub fn open(project_root: &Path) -> anyhow::Result<Self> {
        let db_path = project_root.join(".codegraph").join("codegraph.db");
        let store = Store::open(&db_path)?;
        Ok(Self {
            store,
            project_root: project_root.to_path_buf(),
        })
    }

    fn project_name_tokens(&self) -> HashSet<String> {
        HashSet::new()
    }

    // === Tool dispatch ===================================================

    /// Dispatch by tool name. Mirrors `ToolHandler.execute` switch
    /// (`tools.ts:1033-1056`). Unknown tool → `errorResult` content
    /// (`tools.ts:1054-1055`); the SERVER rejects unknown names earlier with a
    /// JSON-RPC `-32602` (`session.ts:217-225`), so this branch is a backstop.
    pub fn execute(&self, tool_name: &str, args: &Value) -> ToolResult {
        let result = match tool_name {
            "codegraph_search" => self.handle_search(args),
            "codegraph_callers" => self.handle_callers(args),
            "codegraph_callees" => self.handle_callees(args),
            "codegraph_impact" => self.handle_impact(args),
            "codegraph_node" => self.handle_node(args),
            "codegraph_explore" => self.handle_explore(args),
            "codegraph_status" => self.handle_status(args),
            "codegraph_files" => self.handle_files(args),
            "codegraph_check" => self.handle_check(args),
            "codegraph_export" => self.handle_export(args),
            other => return ToolResult::error(format!("Unknown tool: {other}")),
        };
        match result {
            Ok(tr) => tr,
            Err(e) => ToolResult::error(format!("Tool execution failed: {e}")),
        }
    }

    /// `codegraph_check` — additive cycle-detection tool (not in the upstream pin).
    /// Renders `find_circular_dependencies`
    /// (`upstream graph/queries.ts:225-263`) as file chains.
    fn handle_check(&self, _args: &Value) -> anyhow::Result<ToolResult> {
        let traverser = GraphTraverser::new(&self.store);
        let cycles = traverser.find_circular_dependencies()?;
        if cycles.is_empty() {
            return Ok(ToolResult::text(
                "No circular dependencies found".to_string(),
            ));
        }
        let mut out = format!("Found {} circular dependencies:\n", cycles.len());
        for cycle in &cycles {
            let mut chain = cycle.clone();
            if let Some(first) = cycle.first() {
                chain.push(first.clone());
            }
            out.push_str(&format!("\n  {}", chain.join(" \u{2192} ")));
        }
        Ok(ToolResult::text(truncate_output(&out)))
    }

    /// `codegraph_export` — additive full-graph export (not in the upstream pin).
    /// Returns the whole code graph as NetworkX node-link JSON
    /// (`codegraph_graph::export::node_link_graph`). NOT size-capped: the caller
    /// asked for the entire graph, so `truncate_output` must not apply.
    fn handle_export(&self, _args: &Value) -> anyhow::Result<ToolResult> {
        let graph = codegraph_graph::export::node_link_graph(&self.store)?;
        Ok(ToolResult::text(serde_json::to_string(&graph)?))
    }

    // === codegraph_search ================================================

    /// `handleSearch` (`tools.ts:1067-1096`). Calls `searchNodes`; renders via
    /// `formatSearchResults` (`tools.ts:3324-3338`). Empty → `No results found
    /// for "<query>"` (`tools.ts:1082`).
    fn handle_search(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let query = match require_string(args, "query") {
            Ok(q) => q,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10);
        let kinds = args
            .get("kind")
            .and_then(Value::as_str)
            .and_then(parse_node_kind)
            .map(|k| vec![k])
            .unwrap_or_default();

        let options = SearchOptions {
            kinds,
            languages: Vec::new(),
            limit: Some(limit),
            offset: None,
        };
        let results = search_nodes(&self.store, &query, &options, &self.project_name_tokens())?;
        if results.is_empty() {
            return Ok(ToolResult::text(format!(
                "No results found for \"{query}\""
            )));
        }
        let nodes: Vec<Node> = results.into_iter().map(|r| r.node).collect();
        Ok(ToolResult::text(truncate_output(&format_search_results(
            &nodes,
        ))))
    }

    // === codegraph_callers / callees =====================================

    /// `handleCallers` (`tools.ts:1101-1131`). Aggregates `getCallers` over all
    /// name matches; renders via `formatNodeList` (`tools.ts:3340-3350`).
    fn handle_callers(&self, args: &Value) -> anyhow::Result<ToolResult> {
        self.handle_call_direction(args, CallDir::Callers)
    }

    /// `handleCallees` (`tools.ts:1136-1166`).
    fn handle_callees(&self, args: &Value) -> anyhow::Result<ToolResult> {
        self.handle_call_direction(args, CallDir::Callees)
    }

    fn handle_call_direction(&self, args: &Value, dir: CallDir) -> anyhow::Result<ToolResult> {
        let symbol = match require_string(args, "symbol") {
            Ok(s) => s,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20) as usize;

        let all_matches = self.find_all_symbols(&symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(ToolResult::text(format!(
                "Symbol \"{symbol}\" not found in the codebase"
            )));
        }

        let traverser = GraphTraverser::new(&self.store);
        let mut aggregated: Vec<Node> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for node in &all_matches.nodes {
            let edges: Vec<NodeEdge> = match dir {
                CallDir::Callers => traverser.get_callers(&node.id, CALL_DEPTH)?,
                CallDir::Callees => traverser.get_callees(&node.id, CALL_DEPTH)?,
            };
            for ne in edges {
                if seen.insert(ne.node.id.clone()) {
                    aggregated.push(ne.node);
                }
            }
        }

        let godot = match dir {
            CallDir::Callers => self.godot_honesty(&all_matches.nodes)?,
            CallDir::Callees => GodotHonesty::default(),
        };

        if aggregated.is_empty() {
            let label = match dir {
                CallDir::Callers => "callers",
                CallDir::Callees => "callees",
            };
            return Ok(ToolResult::text(format!(
                "No {label} found for \"{symbol}\"{}{}",
                all_matches.note,
                godot.annotation(true)
            )));
        }

        aggregated.truncate(limit);
        let title = match dir {
            CallDir::Callers => format!("Callers of {symbol}"),
            CallDir::Callees => format!("Callees of {symbol}"),
        };
        let formatted = format!(
            "{}{}{}",
            format_node_list(&title, &aggregated),
            all_matches.note,
            godot.annotation(false)
        );
        Ok(ToolResult::text(truncate_output(&formatted)))
    }

    // === codegraph_impact ================================================

    /// `handleImpact` (`tools.ts:1171-1210`). Merges `getImpactRadius` over all
    /// matches; renders via `formatImpact` (`tools.ts:3352-3378`).
    fn handle_impact(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let symbol = match require_string(args, "symbol") {
            Ok(s) => s,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };
        let depth = args.get("depth").and_then(Value::as_i64).unwrap_or(2) as usize;

        let all_matches = self.find_all_symbols(&symbol)?;
        if all_matches.nodes.is_empty() {
            return Ok(ToolResult::text(format!(
                "Symbol \"{symbol}\" not found in the codebase"
            )));
        }

        let traverser = GraphTraverser::new(&self.store);
        let mut merged_order: Vec<String> = Vec::new();
        let mut merged: HashMap<String, Node> = HashMap::new();
        for node in &all_matches.nodes {
            let sub = traverser.get_impact_radius(&node.id, depth)?;
            for id in &sub.node_order {
                if let Some(n) = sub.nodes.get(id) {
                    if !merged.contains_key(id) {
                        merged_order.push(id.clone());
                    }
                    merged.insert(id.clone(), n.clone());
                }
            }
        }

        let ordered: Vec<&Node> = merged_order
            .iter()
            .filter_map(|id| merged.get(id))
            .collect();
        let godot = self.godot_honesty(&all_matches.nodes)?;
        let match_ids: HashSet<&str> = all_matches.nodes.iter().map(|n| n.id.as_str()).collect();
        let no_static_dependents = ordered.iter().all(|n| match_ids.contains(n.id.as_str()));
        let formatted = format!(
            "{}{}{}",
            format_impact(&symbol, &ordered),
            all_matches.note,
            godot.annotation(no_static_dependents)
        );
        Ok(ToolResult::text(truncate_output(&formatted)))
    }

    // === codegraph_node ==================================================

    /// `handleNode` (`tools.ts:2543-2657`). Two modes: file-read (file, no
    /// symbol) and symbol. File mode delegates to `handleFileView`
    /// (`tools.ts:2559-2561`).
    fn handle_node(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let symbol_raw = args.get("symbol").and_then(Value::as_str);
        let file_hint = args.get("file").and_then(Value::as_str);

        if symbol_raw.is_none() {
            if let Some(file_hint) = file_hint {
                return self.handle_file_view(args, file_hint);
            }
        }

        let symbol = match symbol_raw {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                // No symbol and no file → the upstream validates the symbol string and
                // errors (`tools.ts:2552`).
                return Ok(ToolResult::error("symbol must be a non-empty string"));
            }
        };

        let include_code = args
            .get("includeCode")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let matches = self.find_symbol_matches(symbol)?;
        if matches.is_empty() {
            return Ok(ToolResult::text(format!(
                "Symbol \"{symbol}\" not found in the codebase"
            )));
        }
        if matches.len() == 1 {
            let rendered = self.render_node_section(&matches[0], include_code)?;
            return Ok(ToolResult::text(truncate_output(&rendered)));
        }
        Ok(ToolResult::text(truncate_output(
            &self.render_ambiguous_node(symbol, &matches, include_code)?,
        )))
    }

    /// `renderNodeSection` (`tools.ts:2802-2819`): container outline OR full
    /// body, then the trail.
    fn render_node_section(&self, node: &Node, include_code: bool) -> anyhow::Result<String> {
        let outline = if is_container(node.kind) {
            Some(self.build_container_outline(node)?)
        } else {
            None
        };
        let code = if outline.is_none() && include_code {
            self.get_code(node)?
        } else {
            None
        };
        Ok(format!(
            "{}{}",
            format_node_details(node, code.as_deref(), outline.as_deref()),
            self.format_trail(node)?
        ))
    }

    /// `buildContainerOutline` (`tools.ts:3387-3400`): the container's `contains`
    /// children as a member list.
    fn build_container_outline(&self, node: &Node) -> anyhow::Result<String> {
        let traverser = GraphTraverser::new(&self.store);
        let children = traverser.get_children(&node.id)?;
        let mut lines = vec![format!("**Members ({}):**", children.len()), String::new()];
        for c in &children {
            let loc = if c.start_line != 0 {
                format!(":{}", c.start_line)
            } else {
                String::new()
            };
            let sig = c
                .signature
                .as_deref()
                .map(|s| format!(" — `{s}`"))
                .unwrap_or_default();
            lines.push(format!("- {} ({}){}{}", c.name, c.kind.as_str(), loc, sig));
        }
        Ok(lines.join("\n"))
    }

    /// `formatTrail` (`tools.ts:2830-2858`): `Calls →` / `Called by ←` lines.
    fn format_trail(&self, node: &Node) -> anyhow::Result<String> {
        let traverser = GraphTraverser::new(&self.store);
        let callees = traverser.get_callees(&node.id, CALL_DEPTH)?;
        let callers = traverser.get_callers(&node.id, CALL_DEPTH)?;
        if callees.is_empty() && callers.is_empty() {
            return Ok(String::new());
        }
        let fmt = |e: &NodeEdge| -> String {
            format!(
                "{} ({}:{})",
                e.node.name, e.node.file_path, e.node.start_line
            )
        };
        let mut lines = vec![
            String::new(),
            "### Trail — codegraph_node any of these to follow it (no Read needed)".to_string(),
        ];
        if !callees.is_empty() {
            let shown: Vec<String> = callees.iter().take(TRAIL_CAP).map(fmt).collect();
            let more = if callees.len() > TRAIL_CAP {
                format!(", +{} more", callees.len() - TRAIL_CAP)
            } else {
                String::new()
            };
            lines.push(format!("**Calls →** {}{}", shown.join(", "), more));
        }
        if !callers.is_empty() {
            let shown: Vec<String> = callers.iter().take(TRAIL_CAP).map(fmt).collect();
            let more = if callers.len() > TRAIL_CAP {
                format!(", +{} more", callers.len() - TRAIL_CAP)
            } else {
                String::new()
            };
            lines.push(format!("**Called by ←** {}{}", shown.join(", "), more));
        }
        Ok(lines.join("\n"))
    }

    /// Ambiguous-symbol render (`tools.ts:2607-2655`).
    fn render_ambiguous_node(
        &self,
        symbol: &str,
        matches: &[Node],
        include_code: bool,
    ) -> anyhow::Result<String> {
        let header = format!("**{} definitions named \"{symbol}\"**", matches.len());
        if !include_code {
            let mut out = vec![
                header,
                String::new(),
                "Re-query with `includeCode: true` to get every body in one call — no need to pick one first.".to_string(),
                String::new(),
            ];
            for n in matches {
                out.push(format!(
                    "- `{}` ({}) — {}:{}",
                    n.name,
                    n.kind.as_str(),
                    n.file_path,
                    n.start_line
                ));
            }
            return Ok(out.join("\n"));
        }
        let mut rendered: Vec<String> = Vec::new();
        for n in matches {
            rendered.push(self.render_node_section(n, true)?);
        }
        let mut out = vec![
            header,
            format!(
                "Returning {} in full — pick the one you need (no Read required).",
                rendered.len()
            ),
            String::new(),
            rendered.join("\n\n---\n\n"),
        ];
        out.push(String::new());
        Ok(out.join("\n"))
    }

    /// `handleFileView` (`tools.ts:2659-2799`): read an indexed file with line
    /// numbers + a dependents note (Read-equivalent).
    fn handle_file_view(&self, args: &Value, file_arg: &str) -> anyhow::Result<ToolResult> {
        let offset = args
            .get("offset")
            .and_then(Value::as_i64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args.get("limit").and_then(Value::as_i64);
        let symbols_only = args
            .get("symbolsOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let files = self.store.all_files()?;
        if files.is_empty() {
            return Ok(ToolResult::text(
                "No files indexed. Run `codegraph index` first.",
            ));
        }

        let candidates = resolve_file_candidates(&files, file_arg);
        let file_path = match candidates.as_slice() {
            [one] => one.clone(),
            [] => {
                return Ok(ToolResult::text(format!(
                    "No indexed file matches \"{file_arg}\". Codegraph indexes source files; configs/docs it doesn't parse won't appear — Read those directly."
                )));
            }
            many => {
                let mut out = vec![
                    format!(
                        "\"{file_arg}\" matches {} indexed files — pass a longer path:",
                        many.len()
                    ),
                    String::new(),
                ];
                for f in many.iter().take(25) {
                    out.push(format!("- {f}"));
                }
                return Ok(ToolResult::text(out.join("\n")));
            }
        };

        let mut nodes = self.store.nodes_by_file_path(&file_path)?;
        nodes.retain(|n| n.kind != NodeKind::File);
        nodes.sort_by_key(|n| n.start_line);
        let dependents = self.store.dependent_file_paths(&file_path)?;
        let dep_summary = dependents_summary(&dependents);

        if symbols_only {
            let mut out = vec![
                format!(
                    "**{}** — {} symbol{}, {}",
                    file_path,
                    nodes.len(),
                    plural(nodes.len()),
                    dep_summary
                ),
                String::new(),
            ];
            if nodes.is_empty() {
                out.push("_No indexed symbols in this file._".to_string());
            } else {
                out.extend(symbol_map("### Symbols", &nodes, FILE_MODE_MAX_LINES));
            }
            out.push(String::new());
            out.push(
                "> Drop `symbolsOnly` (or pass `offset`/`limit`) to read the source, like Read."
                    .to_string(),
            );
            return Ok(ToolResult::text(out.join("\n")));
        }

        let abs = self.project_root.join(&file_path);
        let content = match fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => {
                let mut out = vec![
                    format!(
                        "**{file_path}** — could not read from disk (it may have moved since indexing). {dep_summary}"
                    ),
                    String::new(),
                ];
                if !nodes.is_empty() {
                    out.extend(symbol_map("### Symbols", &nodes, FILE_MODE_MAX_LINES));
                }
                out.push(String::new());
                out.push(format!(
                    "> Read `{file_path}` directly for its current content."
                ));
                return Ok(ToolResult::text(out.join("\n")));
            }
        };

        let file_lines: Vec<&str> = content.split('\n').collect();
        let total = file_lines.len();
        if offset > total {
            return Ok(ToolResult::text(format!(
                "**{file_path}** has {total} line{} — offset {offset} is past the end. {dep_summary}",
                plural(total)
            )));
        }
        let cap = limit
            .map(|l| l.max(0) as usize)
            .unwrap_or(FILE_MODE_MAX_LINES)
            .min(FILE_MODE_MAX_LINES);
        let start_idx = offset - 1;
        let end_idx = (start_idx + cap).min(total);
        let slice = &file_lines[start_idx..end_idx];
        let numbered = number_source_lines_at(slice, offset);
        let complete = start_idx == 0 && end_idx == total;

        let header = format!(
            "**{file_path}** — {total} lines, {} symbol{} · {dep_summary}",
            nodes.len(),
            plural(nodes.len())
        );
        let mut out = vec![header, String::new(), numbered];
        if !complete {
            out.push(String::new());
            out.push(format!(
                "(lines {offset}–{} of {total} — pass `offset`/`limit` for another range, or `codegraph_node <symbol>` for one symbol in full)",
                end_idx
            ));
        }
        Ok(ToolResult::text(out.join("\n")))
    }

    // === codegraph_explore ===============================================

    /// `handleExplore` (`tools.ts:2014-2977`). PRIMARY tool. Renders the
    /// DETERMINISTIC structure the upstream renders — header, blast radius,
    /// relationships, dynamic boundaries, then per-file source grouped by file —
    /// under a SIZE-ADAPTIVE OUTPUT BUDGET (`getExploreOutputBudget`) scaled by
    /// the project's indexed file count. The relevance-ranking heuristics
    /// (RWR/PageRank, flow detection, off-spine skeletonization) are NOT ported;
    /// see KNOWN_DIFFS.md.
    fn handle_explore(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let query = match require_string(args, "query") {
            Ok(q) => q,
            Err(msg) => return Ok(ToolResult::error(msg)),
        };

        let subgraph = self.find_relevant_context(&query)?;
        if subgraph.nodes.is_empty() {
            return Ok(ToolResult::text(format!(
                "No relevant code found for \"{query}\""
            )));
        }

        // Resolve the adaptive output budget from the project's indexed file
        // count (`tools.ts:2024-2029`), falling back to the largest tier if
        // stats are unavailable.
        let budget = match self.store.counts() {
            Ok(c) => get_explore_output_budget(c.file_count),
            Err(_) => get_explore_output_budget(i64::MAX),
        };
        // `clamp((args.maxFiles) || budget.defaultMaxFiles, 1, 20)`
        // (`tools.ts:2030`).
        let max_files = args
            .get("maxFiles")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .filter(|n| *n > 0)
            .unwrap_or(budget.default_max_files)
            .clamp(1, 20);

        let file_count = subgraph
            .nodes
            .iter()
            .map(|n| n.file_path.as_str())
            .collect::<HashSet<_>>()
            .len();

        let mut lines = vec![
            format!("## Exploration: {query}"),
            String::new(),
            format!(
                "Found {} symbols across {file_count} files.",
                subgraph.nodes.len()
            ),
            String::new(),
        ];

        if let Some(blast) = self.build_blast_radius(&subgraph)? {
            lines.push(blast);
        }

        // Relationships, gated + capped by the budget (`tools.ts:2386-2414`).
        if budget.include_relationships {
            if let Some(rel) =
                self.build_relationships(&subgraph, budget.max_edges_per_relationship_kind)
            {
                lines.push(rel);
            }
        }

        // Ports `buildDynamicBoundaries` (#687, `tools.ts:1706-1760`). The upstream
        // emits this inside the flow output, before the source bodies; the
        // simplified port keeps that relative order: after relationships.
        if let Some(boundaries) = self.build_dynamic_boundaries(&subgraph)? {
            lines.push(boundaries);
        }

        lines.push("### Source Code".to_string());
        lines.push(String::new());
        lines.push("> The code below is the **verbatim, current on-disk source** of these files — re-read from disk on this call and line-numbered, byte-for-byte identical to what the Read tool returns. It is NOT a summary, outline, or stale cache. Treat each block as a Read you have already performed: do not Read a file shown here.".to_string());
        lines.push(String::new());

        // `excludeLowValueFiles` (`tools.ts:2219-2235`): hard-drop test/spec/
        // icon/i18n files unless the query mentions tests, and only when >=2
        // non-low-value files remain (else tests are the only signal here).
        let mut file_order: Vec<&String> = subgraph.file_order.iter().collect();
        if budget.exclude_low_value_files && !query_mentions_tests(&query) {
            let non_low: Vec<&String> = file_order
                .iter()
                .filter(|f| !is_low_value_file(f))
                .copied()
                .collect();
            if non_low.len() >= 2 {
                file_order = non_low;
            }
        }

        // Step through ranked files under the total + per-file budget. A file's
        // source is rendered WHOLE when small enough, else clustered; an
        // incidental file that doesn't fit is DROPPED whole (never sliced
        // mid-method) and surfaced in the trailing "Additional relevant files"
        // list (`tools.ts:2483-2905`).
        let mut total_chars: usize = lines.join("\n").len();
        let mut files_included = 0usize;
        let mut any_file_trimmed = false;
        let mut excluded_files: Vec<&String> = Vec::new();

        for file_path in &file_order {
            if files_included >= max_files {
                excluded_files.push(file_path);
                continue;
            }
            let abs = self.project_root.join(file_path);
            let content = match fs::read_to_string(&abs) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let file_lines: Vec<&str> = content.split('\n').collect();
            let lang = subgraph.file_language(file_path);

            let section =
                self.render_explore_file(&subgraph, file_path, &file_lines, &lang, &budget);
            // Past the total cap an incidental file is skipped whole — the source
            // section never slices through a method body (`tools.ts:2888-2894`).
            if total_chars + section.len() + 200 > budget.max_output_chars {
                any_file_trimmed = true;
                excluded_files.push(file_path);
                continue;
            }
            if section.contains("... (gap) ...") || section.contains("more (signatures elided)") {
                any_file_trimmed = true;
            }
            let section_len = section.len();
            lines.push(section);
            total_chars += section_len + 200;
            files_included += 1;
        }

        // "Additional relevant files (not shown)" — the excluded set, so the
        // agent can request specifics. Gated by the budget (`tools.ts:2910-2927`).
        if budget.include_additional_files && !excluded_files.is_empty() {
            lines.push("### Not shown above — explore these names for their source".to_string());
            lines.push(String::new());
            for file_path in excluded_files.iter().take(10) {
                let names = subgraph.file_node_locations(file_path);
                lines.push(format!("- {file_path}: {names}"));
            }
            if excluded_files.len() > 10 {
                lines.push(format!(
                    "- ... and {} more files",
                    excluded_files.len() - 10
                ));
            }
            lines.push(String::new());
        }

        // Completeness signal / trim note, gated by the budget
        // (`tools.ts:2933-2940`).
        if budget.include_completeness_signal {
            lines.push("---".to_string());
            lines.push(format!(
                "> **Complete source for {files_included} files is included above — do NOT re-read them.** If your question also needs files/symbols listed under \"Not shown above\" (or any area this call didn't cover), make ANOTHER codegraph_explore targeting those names — it returns the same source with line numbers and is cheaper and more complete than reading. Reserve Read for a single specific line range explore can't surface."
            ));
            lines.push(String::new());
        } else if any_file_trimmed {
            lines.push("> Some file sections were trimmed for size. For a specific symbol you still need, run another `codegraph_explore` (or `codegraph_node`) with its exact name — line-numbered source, cheaper and more complete than Read.".to_string());
            lines.push(String::new());
        }

        // Explore budget note (call-count recommendation), gated
        // (`tools.ts:2943-2952`).
        if budget.include_budget_note {
            if let Ok(c) = self.store.counts() {
                let call_budget = get_explore_budget(c.file_count);
                lines.push(format!(
                    "> **Explore budget: {call_budget} calls for this project ({} files indexed).** Each call covers ~6 files; if your question spans more, spend your remaining calls on the uncovered area BEFORE falling back to Read — another explore is cheaper and more complete than reading those files. Synthesize once you've used {call_budget}.",
                    c.file_count
                ));
                lines.push(String::new());
            }
        }

        // Final ABSOLUTE inline ceiling — cut at a `#### ` file-section boundary
        // so trailing whole sections drop rather than slicing a method body
        // (`tools.ts:2954-2975`).
        let output = lines.join("\n");
        let hard_ceiling = ((budget.max_output_chars as f64 * 1.5).round() as usize).min(25000);
        Ok(ToolResult::text(cut_at_section_boundary(
            &output,
            hard_ceiling,
        )))
    }

    /// Render one file's source section under the per-file budget. Small files
    /// come back WHOLE; larger ones are clustered by `gapThreshold` and capped
    /// at `maxCharsPerFile`, dropping whole low-importance clusters (never
    /// slicing mid-method) (`tools.ts:2625-2905`).
    fn render_explore_file(
        &self,
        subgraph: &ExploreSubgraph,
        file_path: &str,
        file_lines: &[&str],
        lang: &str,
        budget: &ExploreOutputBudget,
    ) -> String {
        let total_lines = file_lines.len();
        let content = file_lines.join("\n");
        let body = content.trim_end_matches('\n');

        // Whole-file rule (`tools.ts:2645-2672`): a relevant file small enough to
        // afford comes back ENTIRELY — clustering exists only to tame god-files.
        const WHOLE_FILE_MAX_LINES: usize = 220;
        let whole_file_max_chars = budget.max_chars_per_file * 3;
        if total_lines <= WHOLE_FILE_MAX_LINES && content.len() <= whole_file_max_chars {
            let numbered = number_source_lines_at(&body.split('\n').collect::<Vec<_>>(), 1);
            let names =
                subgraph.file_header_names_capped(file_path, budget.max_symbols_in_file_header);
            return format!("#### {file_path} — {names}\n\n```{lang}\n{numbered}\n```\n");
        }

        // Cluster nearby symbol ranges; merge ranges within `gapThreshold`
        // lines, rank by importance, and select clusters until the per-file cap
        // is hit (`tools.ts:2674-2848`).
        let mut ranges: Vec<ClusterRange> = subgraph
            .nodes
            .iter()
            .filter(|n| {
                n.file_path == file_path
                    && n.kind != NodeKind::Import
                    && n.kind != NodeKind::Export
                    && n.start_line > 0
                    && n.end_line > 0
            })
            // Drop whole-file envelope containers (>50% of the file) so a class
            // body doesn't merge every method into one whole-file cluster
            // (`tools.ts:2693-2718`).
            .filter(|n| {
                !(is_container(n.kind)
                    && (n.end_line - n.start_line + 1) as usize > total_lines / 2)
            })
            .map(|n| {
                let importance = if subgraph.roots.iter().any(|r| r == &n.id) {
                    10
                } else {
                    1
                };
                ClusterRange {
                    start: n.start_line as usize,
                    end: n.end_line as usize,
                    label: format!("{}({})", n.name, n.kind.as_str()),
                    importance,
                }
            })
            .collect();
        ranges.sort_by_key(|r| r.start);

        if ranges.is_empty() {
            return String::new();
        }

        let mut clusters: Vec<Cluster> = Vec::new();
        let mut current = Cluster::from_range(&ranges[0]);
        for r in &ranges[1..] {
            if r.start <= current.end + budget.gap_threshold {
                current.end = current.end.max(r.end);
                current.symbols.push(r.label.clone());
                current.score += r.importance;
                current.max_importance = current.max_importance.max(r.importance);
            } else {
                clusters.push(current);
                current = Cluster::from_range(r);
            }
        }
        clusters.push(current);

        const CONTEXT_PADDING: usize = 3;
        const GAP_MARKER: &str = "\n\n... (gap) ...\n\n";
        let build_section = |c: &Cluster| -> String {
            let start_idx = c.start.saturating_sub(1).saturating_sub(CONTEXT_PADDING);
            let end_idx = (c.end + CONTEXT_PADDING).min(total_lines);
            let slice = &file_lines[start_idx..end_idx];
            number_source_lines_at(slice, start_idx + 1)
        };

        // Rank clusters: entry-point importance first, then density, then span
        // (`tools.ts:2803-2812`).
        let mut ranked: Vec<usize> = (0..clusters.len()).collect();
        ranked.sort_by(|&a, &b| {
            let ca = &clusters[a];
            let cb = &clusters[b];
            let span_a = (ca.end - ca.start + 1) as f64;
            let span_b = (cb.end - cb.start + 1) as f64;
            cb.max_importance
                .cmp(&ca.max_importance)
                .then((cb.score as f64 / span_b).total_cmp(&(ca.score as f64 / span_a)))
                .then(cb.score.cmp(&ca.score))
                .then((span_a as usize).cmp(&(span_b as usize)))
        });

        let file_budget = budget.max_chars_per_file;
        let mut chosen: HashSet<usize> = HashSet::new();
        let mut projected = 0usize;
        for &idx in &ranked {
            let section_len = build_section(&clusters[idx]).len()
                + if chosen.is_empty() {
                    0
                } else {
                    GAP_MARKER.len()
                };
            // Always take the top-ranked cluster even if oversize, so the file
            // section is never empty (`tools.ts:2828-2835`).
            if chosen.is_empty() {
                chosen.insert(idx);
                projected += section_len;
                continue;
            }
            if projected + section_len > file_budget {
                continue;
            }
            chosen.insert(idx);
            projected += section_len;
        }

        let mut file_section = String::new();
        let mut symbols: Vec<String> = Vec::new();
        for (i, cluster) in clusters.iter().enumerate() {
            if !chosen.contains(&i) {
                continue;
            }
            if !file_section.is_empty() {
                file_section.push_str(GAP_MARKER);
            }
            file_section.push_str(&build_section(cluster));
            symbols.extend(cluster.symbols.iter().cloned());
        }
        if chosen.len() < clusters.len() {
            file_section.push_str("\n\n... (gap) ...");
        }

        let header = explore_file_header(file_path, &symbols, budget.max_symbols_in_file_header);
        format!("{header}\n\n```{lang}\n{file_section}\n```\n")
    }

    /// `buildBlastRadiusSection` (`tools.ts:1441-1491`).
    fn build_blast_radius(&self, subgraph: &ExploreSubgraph) -> anyhow::Result<Option<String>> {
        const ROOT_CAP: usize = 5;
        const FILE_CAP: usize = 4;
        let traverser = GraphTraverser::new(&self.store);
        let roots: Vec<&Node> = subgraph
            .roots
            .iter()
            .filter_map(|id| subgraph.node(id))
            .filter(|n| is_meaningful_kind(n.kind))
            .take(ROOT_CAP)
            .collect();
        if roots.is_empty() {
            return Ok(None);
        }
        let mut entries: Vec<String> = Vec::new();
        for root in roots {
            let callers = traverser.get_callers(&root.id, CALL_DEPTH)?;
            let mut seen = HashSet::new();
            let mut uniq: Vec<&Node> = Vec::new();
            for c in &callers {
                if seen.insert(c.node.id.clone()) {
                    uniq.push(&c.node);
                }
            }
            if uniq.is_empty() {
                continue;
            }
            let mut caller_files: Vec<String> = Vec::new();
            let mut fseen = HashSet::new();
            for n in &uniq {
                if fseen.insert(n.file_path.clone()) {
                    caller_files.push(n.file_path.clone());
                }
            }
            let test_files: Vec<&String> =
                caller_files.iter().filter(|f| is_test_file(f)).collect();
            let non_test: Vec<&String> = caller_files.iter().filter(|f| !is_test_file(f)).collect();

            let shown = non_test
                .iter()
                .take(FILE_CAP)
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = if non_test.len() > FILE_CAP {
                format!(" +{} more", non_test.len() - FILE_CAP)
            } else {
                String::new()
            };
            let where_clause = if !non_test.is_empty() {
                format!(" in {shown}{more}")
            } else {
                String::new()
            };
            let tests = if !test_files.is_empty() {
                let t = test_files
                    .iter()
                    .take(FILE_CAP)
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let tmore = if test_files.len() > FILE_CAP {
                    format!(" +{}", test_files.len() - FILE_CAP)
                } else {
                    String::new()
                };
                format!("; tests: {t}{tmore}")
            } else {
                "; ⚠️ no covering tests found".to_string()
            };
            entries.push(format!(
                "- `{}` ({}:{}) — {} caller{}{where_clause}{tests}",
                root.name,
                root.file_path,
                root.start_line,
                uniq.len(),
                plural(uniq.len())
            ));
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let mut out = vec![
            "### Blast radius — what depends on these (update/verify before editing)".to_string(),
            String::new(),
        ];
        out.extend(entries);
        out.push(String::new());
        Ok(Some(out.join("\n")))
    }

    /// Relationship map (`tools.ts:2386-2414`): non-`contains` edges grouped by
    /// kind, rendered `source → target`, each kind capped at
    /// `maxEdgesPerRelationshipKind` with a `... and N more` tail.
    fn build_relationships(&self, subgraph: &ExploreSubgraph, edge_cap: usize) -> Option<String> {
        let mut by_kind: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for e in &subgraph.edges {
            if e.kind == codegraph_core::types::EdgeKind::Contains {
                continue;
            }
            let (src, tgt) = match (subgraph.node(&e.source), subgraph.node(&e.target)) {
                (Some(s), Some(t)) => (s.name.clone(), t.name.clone()),
                _ => continue,
            };
            let kind = e.kind.as_str().to_string();
            if let Some(slot) = by_kind.iter_mut().find(|(k, _)| *k == kind) {
                slot.1.push((src, tgt));
            } else {
                by_kind.push((kind, vec![(src, tgt)]));
            }
        }
        if by_kind.is_empty() {
            return None;
        }
        let mut lines = vec!["### Relationships".to_string(), String::new()];
        for (kind, edges) in by_kind {
            lines.push(format!("**{kind}:**"));
            for (s, t) in edges.iter().take(edge_cap) {
                lines.push(format!("- {s} → {t}"));
            }
            if edges.len() > edge_cap {
                lines.push(format!("- ... and {} more", edges.len() - edge_cap));
            }
            lines.push(String::new());
        }
        Some(lines.join("\n"))
    }

    // === codegraph_status ================================================

    /// `handleStatus` (`tools.ts:2863-2948`). Renders counts + nodes-by-kind +
    /// languages. Pending-sync (watcher) + worktree-mismatch sections are
    /// daemon-only (see KNOWN_DIFFS.md), so a static index omits them.
    fn handle_status(&self, _args: &Value) -> anyhow::Result<ToolResult> {
        let counts = self.store.counts()?;
        let db_size = fs::metadata(self.store.path())
            .map(|m| m.len())
            .unwrap_or(0);
        let mut lines = vec!["## CodeGraph Status".to_string(), String::new()];
        lines.push(format!("**Files indexed:** {}", counts.file_count));
        lines.push(format!("**Total nodes:** {}", counts.node_count));
        lines.push(format!("**Total edges:** {}", counts.edge_count));
        lines.push(format!(
            "**Database size:** {:.2} MB",
            db_size as f64 / 1024.0 / 1024.0
        ));
        lines.push("**Backend:** node:sqlite (Node built-in) — full WAL + FTS5".to_string());
        lines.push("**Journal mode:** wal (concurrent reads safe)".to_string());

        let by_kind = self.store.node_counts_by_kind()?;
        if !by_kind.is_empty() {
            lines.push(String::new());
            lines.push("### Nodes by Kind:".to_string());
            for (kind, count) in by_kind {
                lines.push(format!("- {kind}: {count}"));
            }
        }
        let by_lang = self.store.file_counts_by_language()?;
        if !by_lang.is_empty() {
            lines.push(String::new());
            lines.push("### Languages:".to_string());
            for (lang, count) in by_lang {
                lines.push(format!("- {lang}: {count}"));
            }
        }
        Ok(ToolResult::text(lines.join("\n")))
    }

    // === codegraph_files =================================================

    /// `handleFiles` (`tools.ts:2953-3010`). tree/flat/grouped formats.
    fn handle_files(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let mut files = self.store.all_files()?;
        if files.is_empty() {
            return Ok(ToolResult::text(
                "No files indexed. Run `codegraph index` first.",
            ));
        }
        if let Some(prefix) = args.get("path").and_then(Value::as_str) {
            let prefix = prefix.trim_end_matches('/');
            files.retain(|f| f.path.starts_with(prefix));
        }
        if let Some(pattern) = args.get("pattern").and_then(Value::as_str) {
            files.retain(|f| glob_match(pattern, &f.path));
        }
        if files.is_empty() {
            return Ok(ToolResult::text("No files found matching the criteria."));
        }
        let include_metadata = args
            .get("includeMetadata")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let format = args.get("format").and_then(Value::as_str).unwrap_or("tree");

        let out = match format {
            "flat" => format_files_flat(&files, include_metadata),
            "grouped" => format_files_grouped(&files, include_metadata),
            _ => format_files_tree(&files, include_metadata),
        };
        Ok(ToolResult::text(truncate_output(&out)))
    }

    // === Symbol resolution ==============================================

    /// `findSymbolMatches` (`tools.ts:3220-3265`): exact-name enumeration for a
    /// bare name, falling back to the top fuzzy result.
    fn find_symbol_matches(&self, symbol: &str) -> anyhow::Result<Vec<Node>> {
        let is_qualified = symbol.contains(['.', '/']) || symbol.contains("::");
        if !is_qualified {
            let exact = self.store.nodes_by_name(symbol)?;
            if !exact.is_empty() {
                let mut sorted = exact;
                sorted.sort_by_key(|n| is_generated_file(&n.file_path) as u8);
                return Ok(sorted);
            }
            let fuzzy = search_nodes(
                &self.store,
                symbol,
                &SearchOptions {
                    limit: Some(10),
                    ..Default::default()
                },
                &self.project_name_tokens(),
            )?;
            return Ok(fuzzy
                .into_iter()
                .next()
                .map(|r| vec![r.node])
                .unwrap_or_default());
        }
        let results = self.search_qualified(symbol, 50)?;
        if results.is_empty() {
            return Ok(Vec::new());
        }
        let exact: Vec<Node> = results
            .iter()
            .filter(|n| matches_symbol(n, symbol))
            .cloned()
            .collect();
        if exact.is_empty() {
            return Ok(Vec::new());
        }
        let mut sorted = exact;
        sorted.sort_by_key(|n| is_generated_file(&n.file_path) as u8);
        Ok(sorted)
    }

    /// `findAllSymbols` (`tools.ts:3271-3307`): aggregate matches + the
    /// multi-match "Aggregated results" note.
    fn find_all_symbols(&self, symbol: &str) -> anyhow::Result<AllSymbols> {
        let results = self.search_qualified(symbol, 50)?;
        if results.is_empty() {
            return Ok(AllSymbols {
                nodes: Vec::new(),
                note: String::new(),
            });
        }
        let exact: Vec<Node> = results
            .iter()
            .filter(|n| matches_symbol(n, symbol))
            .cloned()
            .collect();
        if exact.len() <= 1 {
            let node = exact
                .into_iter()
                .next()
                .or_else(|| results.into_iter().next());
            return Ok(AllSymbols {
                nodes: node.into_iter().collect(),
                note: String::new(),
            });
        }
        let mut ranked = exact;
        ranked.sort_by_key(|n| is_generated_file(&n.file_path) as u8);
        let locations = ranked
            .iter()
            .map(|n| format!("{} at {}:{}", n.kind.as_str(), n.file_path, n.start_line))
            .collect::<Vec<_>>()
            .join(", ");
        let note = format!(
            "\n\n> **Note:** Aggregated results across {} symbols named \"{symbol}\": {locations}",
            ranked.len()
        );
        Ok(AllSymbols {
            nodes: ranked,
            note,
        })
    }

    /// Aggregate the Godot dynamic-reachability honesty signal across the name
    /// matches. Returns an all-empty summary for any project without Godot links
    /// to those matches — the gate that keeps non-Godot output byte-unchanged.
    fn godot_honesty(&self, nodes: &[Node]) -> anyhow::Result<GodotHonesty> {
        let traverser = GraphTraverser::new(&self.store);
        let mut summary = GodotHonesty::default();
        let mut seen = HashSet::new();
        for node in nodes {
            let reach = traverser.godot_dynamic_reachability(node)?;
            for r in &reach.reached_by {
                match r {
                    GodotReach::SceneOrResourceLink => summary.reached_via_scene = true,
                    GodotReach::Autoload => summary.reached_via_autoload = true,
                }
            }
            for name in reach.dynamic_unresolved {
                if seen.insert(name.clone()) {
                    summary.dynamic_unresolved.push(name);
                }
            }
        }
        summary.dynamic_unresolved.sort();
        Ok(summary)
    }

    /// FTS search + the colon-strip fallback for qualified names
    /// (`tools.ts:3241-3248`).
    fn search_qualified(&self, symbol: &str, limit: i64) -> anyhow::Result<Vec<Node>> {
        let opts = SearchOptions {
            limit: Some(limit),
            ..Default::default()
        };
        let mut results = search_nodes(&self.store, symbol, &opts, &self.project_name_tokens())?;
        let is_qualified = symbol.contains(['.', '/']) || symbol.contains("::");
        if results.is_empty() && is_qualified {
            if let Some(tail) = last_qualifier_part(symbol) {
                if tail != symbol {
                    results = search_nodes(&self.store, tail, &opts, &self.project_name_tokens())?;
                }
            }
        }
        Ok(results.into_iter().map(|r| r.node).collect())
    }

    /// Build the explore subgraph. Ports the deterministic spine of
    /// `findRelevantContext` (`context/index.ts:900-940`): the entry points
    /// (`roots`) are the FTS search results for the query (`searchLimit: 8`,
    /// `tools.ts:1597-1602`), in search-rank order. We then pull each root's
    /// callers/callees + `contains` children into the subgraph so the blast
    /// radius, relationship map, and source section have content. The RWR
    /// relevance re-ranking / file gating is NOT ported (see KNOWN_DIFFS.md).
    fn find_relevant_context(&self, query: &str) -> anyhow::Result<ExploreSubgraph> {
        let mut sub = ExploreSubgraph::default();
        let traverser = GraphTraverser::new(&self.store);

        let mut seed_ids: Vec<String> = Vec::new();
        let results = search_nodes(
            &self.store,
            query,
            &SearchOptions {
                limit: Some(8),
                ..Default::default()
            },
            &self.project_name_tokens(),
        )?;
        for r in results {
            if sub.insert(r.node.clone()) {
                seed_ids.push(r.node.id.clone());
                sub.roots.push(r.node.id.clone());
            }
        }

        for root_id in seed_ids.clone() {
            for c in traverser.get_callers(&root_id, CALL_DEPTH)? {
                sub.add_edge(c.edge.clone());
                sub.insert(c.node);
            }
            for c in traverser.get_callees(&root_id, CALL_DEPTH)? {
                sub.add_edge(c.edge.clone());
                sub.insert(c.node);
            }
            for child in traverser.get_children(&root_id)? {
                sub.insert(child);
            }
        }
        sub.finalize();
        Ok(sub)
    }

    /// Ports `buildDynamicBoundaries` (`tools.ts:1717-1760`): scan the explored
    /// symbols' bodies for dynamic-dispatch sites and ANNOUNCE the boundary.
    /// The port's scan list is the subgraph roots (the FTS seeds = the symbols
    /// the query targeted) — the analog of the upstream `named`/`scanList`, since the
    /// simplified explore has no flow-connectivity pass.
    fn build_dynamic_boundaries(
        &self,
        subgraph: &ExploreSubgraph,
    ) -> anyhow::Result<Option<String>> {
        const MAX_NOTES: usize = 4;
        const MAX_SCAN: usize = 8;
        const MAX_TOTAL_CHARS: usize = 200_000;

        let scan_list: Vec<&Node> = subgraph
            .roots
            .iter()
            .filter_map(|id| subgraph.node(id))
            .collect();

        let mut notes: Vec<String> = Vec::new();
        let mut seen_node: HashSet<&str> = HashSet::new();
        let mut seen_site: HashSet<String> = HashSet::new();
        let mut scanned = 0usize;
        let mut chars_scanned = 0usize;

        for node in scan_list {
            if notes.len() >= MAX_NOTES || scanned >= MAX_SCAN || chars_scanned > MAX_TOTAL_CHARS {
                break;
            }
            if seen_node.contains(node.id.as_str()) || node.start_line <= 0 || node.end_line <= 0 {
                continue;
            }
            seen_node.insert(node.id.as_str());
            let abs = self.project_root.join(&node.file_path);
            let content = match fs::read_to_string(&abs) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let file_lines: Vec<&str> = content.split('\n').collect();
            let start_idx = ((node.start_line - 1).max(0) as usize).min(file_lines.len());
            let end_idx = (node.end_line as usize).min(file_lines.len());
            if start_idx >= end_idx {
                continue;
            }
            let body = file_lines[start_idx..end_idx].join("\n");
            scanned += 1;
            chars_scanned += body.len();

            for m in scan_dynamic_dispatch(&body, node.language.as_str(), node.start_line) {
                if notes.len() >= MAX_NOTES {
                    break;
                }
                let site_key = format!("{}:{}:{}", node.file_path, m.line, m.form);
                if !seen_site.insert(site_key) {
                    continue;
                }
                let more = if m.more_sites > 0 {
                    let s = if m.more_sites > 1 { "s" } else { "" };
                    format!(" (+{} more such site{s} in this body)", m.more_sites)
                } else {
                    String::new()
                };
                notes.push(format!(
                    "- `{}` ({}:{}) — {}: `{}`{more}",
                    node.name, node.file_path, m.line, m.label, m.snippet
                ));
                if let Some(key) = &m.key {
                    if let Some(cand) =
                        self.boundary_candidates(key, m.key_is_type, subgraph, &node.id)?
                    {
                        notes.push(format!("  {cand}"));
                    }
                }
            }
        }

        if notes.is_empty() {
            return Ok(None);
        }
        let mut out = vec![
            "## Dynamic boundaries (the static path ends at runtime dispatch)".to_string(),
            String::new(),
        ];
        out.extend(notes);
        out.push(String::new());
        out.push("> These sites choose their call target at runtime (registry / bus / reflection) — the site shown IS where the flow continues. To follow it, run codegraph_explore or codegraph_node on a candidate; source for the sites above is included below.".to_string());
        out.push(String::new());
        Ok(Some(out.join("\n")))
    }

    /// Ports `boundaryCandidates` (`tools.ts:1770-1827`): shortlist candidate
    /// runtime targets for a dispatch key — exact conventional names first
    /// (`save` → `onSave`/`handleSave`; `CreateCmd` → `CreateCmdHandler`), then
    /// FTS, with a normalized-containment post-filter. `named` symbols sort
    /// first and are marked.
    fn boundary_candidates(
        &self,
        key: &str,
        key_is_type: bool,
        subgraph: &ExploreSubgraph,
        self_id: &str,
    ) -> anyhow::Result<Option<String>> {
        // The upstream CALLABLE includes `constructor`; the Rust extractor has no
        // such NodeKind (ctors are emitted as `method`), so it is omitted here.
        const CALLABLE: [NodeKind; 4] = [
            NodeKind::Method,
            NodeKind::Function,
            NodeKind::Component,
            NodeKind::Class,
        ];
        let norm = |s: &str| -> String {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_lowercase())
                .collect()
        };
        let key_norm = norm(key);
        if key_norm.len() < 3 {
            return Ok(None);
        }

        let mut cands: Vec<Node> = Vec::new();
        let mut cand_ids: HashSet<String> = HashSet::new();
        let consider = |n: &Node, cands: &mut Vec<Node>, cand_ids: &mut HashSet<String>| {
            if n.id == self_id || !CALLABLE.contains(&n.kind) || cand_ids.contains(&n.id) {
                return;
            }
            let name_norm = norm(&n.name);
            if name_norm.len() < 3 {
                return;
            }
            if !name_norm.contains(&key_norm) && !key_norm.contains(&name_norm) {
                return;
            }
            cand_ids.insert(n.id.clone());
            cands.push(n.clone());
        };

        let mut chars = key.chars();
        let cap = match chars.next() {
            Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
            None => String::new(),
        };
        let probes: Vec<String> = if key_is_type {
            vec![format!("{key}Handler"), key.to_string()]
        } else {
            vec![
                key.to_string(),
                format!("on{cap}"),
                format!("handle{cap}"),
                format!("{key}Handler"),
                format!("handle_{key}"),
            ]
        };
        for p in &probes {
            if let Ok(found) = self.store.nodes_by_name(p) {
                for n in &found {
                    consider(n, &mut cands, &mut cand_ids);
                }
            }
        }

        let mut raw = 0usize;
        let opts = SearchOptions {
            limit: Some(12),
            ..Default::default()
        };
        if let Ok(results) = search_nodes(&self.store, key, &opts, &self.project_name_tokens()) {
            raw = results.len();
            for r in &results {
                consider(&r.node, &mut cands, &mut cand_ids);
            }
        }

        if cands.is_empty() {
            return Ok(if raw >= 12 && key.chars().count() < 5 {
                Some(format!(
                    "key `{key}` is too generic to shortlist ({raw}+ matches)"
                ))
            } else {
                None
            });
        }

        // A constructor candidate duplicates its class (extractors emit ctors as
        // METHOD nodes named like the class) — keep the class (`tools.ts:1799`).
        let class_key: HashSet<String> = cands
            .iter()
            .filter(|n| n.kind == NodeKind::Class)
            .map(|n| format!("{}|{}", n.name, n.file_path))
            .collect();
        let named_names: HashSet<&str> = subgraph
            .roots
            .iter()
            .filter_map(|id| subgraph.node(id))
            .map(|n| n.name.as_str())
            .collect();
        let named_ids: HashSet<&str> = subgraph.roots.iter().map(|s| s.as_str()).collect();
        let is_named =
            |n: &Node| named_ids.contains(n.id.as_str()) || named_names.contains(n.name.as_str());

        let mut list: Vec<&Node> = cands
            .iter()
            .filter(|n| {
                !(n.kind != NodeKind::Class
                    && class_key.contains(&format!("{}|{}", n.name, n.file_path)))
            })
            .collect();
        list.sort_by_key(|n| if is_named(n) { 0 } else { 1 });
        list.truncate(4);

        let traverser = GraphTraverser::new(&self.store);
        let mut rendered: Vec<String> = Vec::new();
        for n in list {
            let mut display = if n.qualified_name.is_empty() {
                n.name.clone()
            } else {
                n.qualified_name.clone()
            };
            let mut at = format!("{}:{}", n.file_path, n.start_line);
            // Typed-bus convention: the runtime target is the candidate class's
            // Handle/Execute/Consume method (`tools.ts:1814-1822`).
            if key_is_type && n.kind == NodeKind::Class {
                if let Ok(children) = traverser.get_children(&n.id) {
                    if let Some(method) = children
                        .iter()
                        .find(|c| c.kind == NodeKind::Method && is_handler_method_name(&c.name))
                    {
                        display = format!("{}.{}", n.name, method.name);
                        at = format!("{}:{}", method.file_path, method.start_line);
                    }
                }
            }
            let mark = if is_named(n) {
                " ← you named this"
            } else {
                ""
            };
            rendered.push(format!("`{display}` ({at}){mark}"));
        }
        Ok(Some(format!(
            "candidates for key `{key}`: {}",
            rendered.join(", ")
        )))
    }

    /// `getCode` (`context/index.ts:1151-1191`): slice `[startLine, endLine]`
    /// out of the on-disk file (1-based inclusive).
    fn get_code(&self, node: &Node) -> anyhow::Result<Option<String>> {
        let abs = self.project_root.join(&node.file_path);
        let content = match fs::read_to_string(&abs) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let lines: Vec<&str> = content.split('\n').collect();
        let start_idx = (node.start_line - 1).max(0) as usize;
        let end_idx = (node.end_line as usize).min(lines.len());
        if start_idx >= lines.len() {
            return Ok(None);
        }
        Ok(Some(lines[start_idx..end_idx].join("\n")))
    }
}

#[derive(Clone, Copy)]
enum CallDir {
    Callers,
    Callees,
}

struct AllSymbols {
    nodes: Vec<Node>,
    note: String,
}

/// Godot honesty signal for the callers/impact tools: runtime-reachability
/// reasons (so a symbol reached only via a Godot link is never reported dead)
/// and the matched symbols' own `godot:dynamic:` computed call-sites. All-empty
/// for non-Godot projects, which keeps the tool text byte-unchanged.
#[derive(Default)]
struct GodotHonesty {
    reached_via_scene: bool,
    reached_via_autoload: bool,
    dynamic_unresolved: Vec<String>,
}

impl GodotHonesty {
    fn is_dynamically_reachable(&self) -> bool {
        self.reached_via_scene || self.reached_via_autoload
    }

    fn reachability_sources(&self) -> String {
        let mut parts = Vec::new();
        if self.reached_via_scene {
            parts.push("signal/get_node/group");
        }
        if self.reached_via_autoload {
            parts.push("autoload");
        }
        parts.join("/")
    }

    /// The text appended to the callers/impact body. `no_static_callers` gates
    /// the "may be reached dynamically" line so it replaces a bare "no callers"
    /// only when there genuinely are none.
    fn annotation(&self, no_static_callers: bool) -> String {
        let mut out = String::new();
        if self.is_dynamically_reachable() && no_static_callers {
            out.push_str(&format!(
                "\n\n> No static callers — may be reached dynamically (Godot {}).",
                self.reachability_sources()
            ));
        }
        if !self.dynamic_unresolved.is_empty() {
            out.push_str(
                "\n\n### Dynamic / unresolved references (cannot be statically confirmed)\n",
            );
            for name in &self.dynamic_unresolved {
                out.push_str(&format!("\n- `{name}`"));
            }
        }
        out
    }
}

/// The explore subgraph: nodes (insertion-ordered), edges, roots, and per-file
/// ordering for the source section.
#[derive(Default)]
struct ExploreSubgraph {
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    edges: Vec<codegraph_core::types::Edge>,
    edge_seen: HashSet<(String, String, String)>,
    roots: Vec<String>,
    file_order: Vec<String>,
}

impl ExploreSubgraph {
    fn insert(&mut self, node: Node) -> bool {
        if self.index.contains_key(&node.id) {
            return false;
        }
        self.index.insert(node.id.clone(), self.nodes.len());
        self.nodes.push(node);
        true
    }

    fn add_edge(&mut self, edge: codegraph_core::types::Edge) {
        let key = (
            edge.source.clone(),
            edge.target.clone(),
            edge.kind.as_str().to_string(),
        );
        if self.edge_seen.insert(key) {
            self.edges.push(edge);
        }
    }

    fn node(&self, id: &str) -> Option<&Node> {
        self.index.get(id).map(|i| &self.nodes[*i])
    }

    fn finalize(&mut self) {
        for n in &self.nodes {
            if !self.file_order.contains(&n.file_path) {
                self.file_order.push(n.file_path.clone());
            }
        }
        // Relevance-aware file ranking (lightweight RWR approximation): a file
        // scores by whether it hosts a query seed/root (definition site), then
        // its graph proximity to those seeds, then its relevant-symbol count.
        // This floats the definition + sibling-API files to the top so a single
        // explore call covers the answer, instead of ranking incidental
        // high-symbol-count files first. Deterministic: ties break on the file
        // path, keeping output byte-stable.
        let root_files: HashSet<&str> = self
            .roots
            .iter()
            .filter_map(|id| self.node(id).map(|n| n.file_path.as_str()))
            .collect();
        // Files one graph hop from a seed (edge source or target is a seed).
        let mut neighbor_files: HashSet<&str> = HashSet::new();
        for e in &self.edges {
            if self.roots.iter().any(|r| r == &e.source) {
                if let Some(n) = self.node(&e.target) {
                    neighbor_files.insert(n.file_path.as_str());
                }
            }
            if self.roots.iter().any(|r| r == &e.target) {
                if let Some(n) = self.node(&e.source) {
                    neighbor_files.insert(n.file_path.as_str());
                }
            }
        }
        let scores: HashMap<String, (u8, usize)> = self
            .file_order
            .iter()
            .map(|fp| {
                let tier = if root_files.contains(fp.as_str()) {
                    2
                } else if neighbor_files.contains(fp.as_str()) {
                    1
                } else {
                    0
                };
                let sym_count = self
                    .nodes
                    .iter()
                    .filter(|n| &n.file_path == fp && n.kind != NodeKind::File)
                    .count();
                (fp.clone(), (tier, sym_count))
            })
            .collect();
        self.file_order
            .sort_by(|a, b| scores[b].cmp(&scores[a]).then_with(|| a.cmp(b)));
    }

    fn file_language(&self, file_path: &str) -> String {
        self.nodes
            .iter()
            .find(|n| n.file_path == file_path)
            .map(|n| n.language.as_str().to_string())
            .unwrap_or_default()
    }

    /// Whole-file header symbol list, capped at `cap` with a `+N more` tail
    /// (`tools.ts:2652-2659`).
    fn file_header_names_capped(&self, file_path: &str, cap: usize) -> String {
        let mut in_file: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|n| {
                n.file_path == file_path && n.kind != NodeKind::Import && n.kind != NodeKind::Export
            })
            .collect();
        in_file.sort_by_key(|n| n.start_line);
        let mut names: Vec<String> = Vec::new();
        for n in in_file {
            let label = format!("{}({})", n.name, n.kind.as_str());
            if !names.contains(&label) {
                names.push(label);
            }
        }
        cap_header_names(&names, cap)
    }

    /// `name:line` locations for the "Not shown above" list (`tools.ts:2920`).
    fn file_node_locations(&self, file_path: &str) -> String {
        let mut in_file: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|n| n.file_path == file_path)
            .collect();
        in_file.sort_by_key(|n| n.start_line);
        in_file
            .iter()
            .map(|n| format!("{}:{}", n.name, n.start_line))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Cluster source range used when sizing a god-file (`tools.ts:2708-2718`).
struct ClusterRange {
    start: usize,
    end: usize,
    label: String,
    importance: u32,
}

/// A merged run of adjacent ranges (`tools.ts:2744-2771`).
struct Cluster {
    start: usize,
    end: usize,
    symbols: Vec<String>,
    score: u32,
    max_importance: u32,
}

impl Cluster {
    fn from_range(r: &ClusterRange) -> Self {
        Self {
            start: r.start,
            end: r.end,
            symbols: vec![r.label.clone()],
            score: r.importance,
            max_importance: r.importance,
        }
    }
}

// === Free-function renderers (1:1 with upstream helpers) ====================

/// `formatSearchResults` (`tools.ts:3324-3338`).
fn format_search_results(nodes: &[Node]) -> String {
    let mut lines = vec![
        format!("## Search Results ({} found)", nodes.len()),
        String::new(),
    ];
    for node in nodes {
        let location = if node.start_line != 0 {
            format!(":{}", node.start_line)
        } else {
            String::new()
        };
        lines.push(format!("### {} ({})", node.name, node.kind.as_str()));
        lines.push(format!("{}{}", node.file_path, location));
        if let Some(sig) = &node.signature {
            lines.push(format!("`{sig}`"));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// `formatNodeList` (`tools.ts:3340-3350`).
fn format_node_list(title: &str, nodes: &[Node]) -> String {
    let mut lines = vec![format!("## {title} ({} found)", nodes.len()), String::new()];
    for node in nodes {
        let location = if node.start_line != 0 {
            format!(":{}", node.start_line)
        } else {
            String::new()
        };
        lines.push(format!(
            "- {} ({}) - {}{}",
            node.name,
            node.kind.as_str(),
            node.file_path,
            location
        ));
    }
    lines.join("\n")
}

/// `formatImpact` (`tools.ts:3352-3378`): group by file (insertion order).
fn format_impact(symbol: &str, nodes: &[&Node]) -> String {
    let mut lines = vec![
        format!("## Impact: \"{symbol}\" affects {} symbols", nodes.len()),
        String::new(),
    ];
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, Vec<&Node>> = HashMap::new();
    for n in nodes {
        if !by_file.contains_key(&n.file_path) {
            order.push(n.file_path.clone());
        }
        by_file.entry(n.file_path.clone()).or_default().push(n);
    }
    for file in order {
        lines.push(format!("**{file}:**"));
        let list = by_file[&file]
            .iter()
            .map(|n| format!("{}:{}", n.name, n.start_line))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(list);
        lines.push(String::new());
    }
    lines.join("\n")
}

/// `formatNodeDetails` (`tools.ts:3402-3430`).
fn format_node_details(node: &Node, code: Option<&str>, outline: Option<&str>) -> String {
    let location = if node.start_line != 0 {
        format!(":{}", node.start_line)
    } else {
        String::new()
    };
    let mut lines = vec![
        format!("## {} ({})", node.name, node.kind.as_str()),
        String::new(),
        format!("**Location:** {}{}", node.file_path, location),
    ];
    if let Some(sig) = &node.signature {
        lines.push(format!("**Signature:** `{sig}`"));
    }
    if let Some(doc) = &node.docstring {
        if doc.len() < 200 {
            lines.push(String::new());
            lines.push(doc.clone());
        }
    }
    if let Some(outline) = outline {
        lines.push(String::new());
        lines.push(outline.to_string());
        lines.push(String::new());
        lines.push(format!(
            "> Structural outline only. Read `{}` or call codegraph_node on a specific member for its body.",
            node.file_path
        ));
    } else if let Some(code) = code {
        let numbered = if node.start_line != 0 {
            number_source_lines_at(
                &code.split('\n').collect::<Vec<_>>(),
                node.start_line as usize,
            )
        } else {
            code.to_string()
        };
        lines.push(String::new());
        lines.push(format!("```{}", node.language.as_str()));
        lines.push(numbered);
        lines.push("```".to_string());
    }
    lines.join("\n")
}

/// `symbolMap` (`tools.ts:2716-2724`).
fn symbol_map(heading: &str, nodes: &[Node], limit: usize) -> Vec<String> {
    let mut lines = vec![heading.to_string()];
    for n in nodes.iter().take(limit) {
        let sig = n
            .signature
            .as_deref()
            .map(|s| format!(" {}", normalize_ws(s)))
            .unwrap_or_default();
        lines.push(format!(
            "- `{}` ({}){} — :{}",
            n.name,
            n.kind.as_str(),
            sig,
            n.start_line
        ));
    }
    if nodes.len() > limit {
        lines.push(format!("- … +{} more", nodes.len() - limit));
    }
    lines
}

/// `formatFilesFlat` (`tools.ts:3028-3040`).
fn format_files_flat(files: &[FileRecord], include_metadata: bool) -> String {
    let mut sorted: Vec<&FileRecord> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut lines = vec![format!("## Files ({})", files.len()), String::new()];
    for f in sorted {
        if include_metadata {
            lines.push(format!(
                "- {} ({}, {} symbols)",
                f.path,
                f.language.as_str(),
                f.node_count
            ));
        } else {
            lines.push(format!("- {}", f.path));
        }
    }
    lines.join("\n")
}

/// `formatFilesGrouped` (`tools.ts:3045-3072`).
fn format_files_grouped(files: &[FileRecord], include_metadata: bool) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut by_lang: HashMap<String, Vec<&FileRecord>> = HashMap::new();
    for f in files {
        let lang = f.language.as_str().to_string();
        if !by_lang.contains_key(&lang) {
            order.push(lang.clone());
        }
        by_lang.entry(lang).or_default().push(f);
    }
    let mut lines = vec![
        format!("## Files by Language ({} total)", files.len()),
        String::new(),
    ];
    for lang in order {
        let mut lang_files = by_lang[&lang].clone();
        lang_files.sort_by(|a, b| a.path.cmp(&b.path));
        lines.push(format!("### {lang} ({})", lang_files.len()));
        for f in lang_files {
            if include_metadata {
                lines.push(format!("- {} ({} symbols)", f.path, f.node_count));
            } else {
                lines.push(format!("- {}", f.path));
            }
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// `formatFilesTree` (`tools.ts:3077-3147`).
fn format_files_tree(files: &[FileRecord], include_metadata: bool) -> String {
    let mut sorted: Vec<&FileRecord> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut root = TreeDir::default();
    for f in &sorted {
        root.insert(&f.path, f);
    }
    let mut lines = vec![
        format!("## Project Structure ({} files)", files.len()),
        String::new(),
    ];
    root.render("", &mut lines, include_metadata);
    lines.join("\n")
}

#[derive(Default)]
struct TreeDir<'a> {
    dirs: Vec<(String, TreeDir<'a>)>,
    files: Vec<(String, &'a FileRecord)>,
}

impl<'a> TreeDir<'a> {
    fn insert(&mut self, path: &str, rec: &'a FileRecord) {
        let mut parts = path.splitn(2, '/');
        let head = parts.next().unwrap_or("");
        match parts.next() {
            Some(rest) => {
                if let Some(slot) = self.dirs.iter_mut().find(|(n, _)| n == head) {
                    slot.1.insert(rest, rec);
                } else {
                    let mut d = TreeDir::default();
                    d.insert(rest, rec);
                    self.dirs.push((head.to_string(), d));
                }
            }
            None => self.files.push((head.to_string(), rec)),
        }
    }

    fn render(&self, prefix: &str, lines: &mut Vec<String>, include_metadata: bool) {
        let total = self.dirs.len() + self.files.len();
        let mut i = 0;
        for (name, dir) in &self.dirs {
            let is_last = i == total - 1;
            let connector = if is_last { "└── " } else { "├── " };
            lines.push(format!("{prefix}{connector}{name}"));
            let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            dir.render(&child_prefix, lines, include_metadata);
            i += 1;
        }
        for (name, rec) in &self.files {
            let is_last = i == total - 1;
            let connector = if is_last { "└── " } else { "├── " };
            let meta = if include_metadata {
                format!(" ({}, {} symbols)", rec.language.as_str(), rec.node_count)
            } else {
                String::new()
            };
            lines.push(format!("{prefix}{connector}{name}{meta}"));
            i += 1;
        }
    }
}

// === Small helpers =======================================================

/// `numberSourceLines` (`tools.ts:279-286`): `<n>\t<line>` per line.
fn number_source_lines_at(slice: &[&str], first_line_number: usize) -> String {
    slice
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}\t{}", first_line_number + i, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `truncateOutput` (`tools.ts:3312-3318`). `MAX_OUTPUT_LENGTH` is 50_000
/// (`tools.ts` constant).
fn truncate_output(text: &str) -> String {
    const MAX: usize = 50_000;
    if text.len() <= MAX {
        return text.to_string();
    }
    let truncated = &text[..MAX];
    let last_newline = truncated.rfind('\n').unwrap_or(0);
    let cut = if last_newline as f64 > MAX as f64 * 0.8 {
        last_newline
    } else {
        MAX
    };
    format!("{}\n\n... (output truncated)", &truncated[..cut])
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// File-mode dependents note (`tools.ts:2711-2713`).
fn dependents_summary(dependents: &[String]) -> String {
    if dependents.is_empty() {
        return "no other indexed file depends on it".to_string();
    }
    let shown = dependents
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = if dependents.len() > 8 {
        format!(", +{} more", dependents.len() - 8)
    } else {
        String::new()
    };
    format!(
        "used by {} file{}: {shown}{more}",
        dependents.len(),
        plural(dependents.len())
    )
}

fn resolve_file_candidates(files: &[FileRecord], file_arg: &str) -> Vec<String> {
    let arg = file_arg.replace('\\', "/");
    if let Some(exact) = files.iter().find(|f| f.path == arg) {
        return vec![exact.path.clone()];
    }
    files
        .iter()
        .filter(|f| {
            f.path == arg || f.path.ends_with(&format!("/{arg}")) || basename(&f.path) == arg
        })
        .map(|f| f.path.clone())
        .collect()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `matchesSymbol` (`tools.ts:3175-3210`).
fn matches_symbol(node: &Node, symbol: &str) -> bool {
    if node.name == symbol {
        return true;
    }
    if node.kind == NodeKind::File && strip_ext(&node.name) == symbol {
        return true;
    }
    if !(symbol.contains(['.', '/']) || symbol.contains("::")) {
        return false;
    }
    let parts: Vec<&str> = symbol
        .split("::")
        .flat_map(|p| p.split(['.', '/']))
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return false;
    }
    let last = parts[parts.len() - 1];
    if node.name != last {
        return false;
    }
    let colon_suffix = parts.join("::");
    if node.qualified_name.contains(&colon_suffix) {
        return true;
    }
    let hints: Vec<&str> = parts[..parts.len() - 1].to_vec();
    if hints.is_empty() {
        return false;
    }
    let segments: Vec<&str> = node
        .file_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    hints.iter().all(|hint| {
        segments
            .iter()
            .any(|seg| *seg == *hint || strip_ext(seg) == *hint)
    })
}

fn strip_ext(s: &str) -> &str {
    match s.rfind('.') {
        Some(i) if i > 0 => &s[..i],
        _ => s,
    }
}

fn last_qualifier_part(symbol: &str) -> Option<&str> {
    let parts: Vec<&str> = symbol
        .split("::")
        .flat_map(|p| p.split(['.', '/']))
        .filter(|p| !p.is_empty())
        .collect();
    parts.last().copied()
}

/// Ports the typed-bus `HANDLER_METHODS` regex (`tools.ts:1816`): the runtime
/// target convention for a request type's handler class.
fn is_handler_method_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "handle"
            | "handleasync"
            | "execute"
            | "executeasync"
            | "consume"
            | "consumeasync"
            | "run"
            | "__invoke"
    )
}

fn is_generated_file(path: &str) -> bool {
    let p = path;
    p.ends_with(".pb.go")
        || p.ends_with(".pulsar.go")
        || p.ends_with("_grpc.pb.go")
        || p.ends_with(".g.dart")
        || p.ends_with(".freezed.dart")
}

fn is_test_file(path: &str) -> bool {
    let p = path;
    p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains("/spec/")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_test.")
}

fn is_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
    )
}

fn is_meaningful_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Component
            | NodeKind::Constant
            | NodeKind::Variable
            | NodeKind::Property
            | NodeKind::Field
    )
}

/// `codegraph_search`'s `kind` enum → `NodeKind`
/// (`tools.ts:392`: function/method/class/interface/type/variable/route/
/// component). Note `type` maps to `type_alias`.
fn parse_node_kind(s: &str) -> Option<NodeKind> {
    Some(match s {
        "function" => NodeKind::Function,
        "method" => NodeKind::Method,
        "class" => NodeKind::Class,
        "interface" => NodeKind::Interface,
        "type" => NodeKind::TypeAlias,
        "variable" => NodeKind::Variable,
        "route" => NodeKind::Route,
        "component" => NodeKind::Component,
        _ => return None,
    })
}

/// `globToRegex` matcher (`tools.ts:3149-3170`) — supports `*`, `**`, `?`.
fn glob_match(pattern: &str, path: &str) -> bool {
    let mut re = String::from("^");
    let bytes: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            '*' => {
                if i + 1 < bytes.len() && bytes[i + 1] == '*' {
                    re.push_str(".*");
                    i += 1;
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            '.' => re.push_str("\\."),
            '/' => re.push('/'),
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => re.push(c),
            c => {
                re.push('\\');
                re.push(c);
            }
        }
        i += 1;
    }
    re.push('$');
    simple_regex_match(&re, path)
}

/// Minimal regex matcher for the glob-derived patterns (anchored, supports
/// `.*`, `[^/]*`, `[^/]`, literal/escaped chars). Avoids a regex dependency for
/// the narrow `codegraph_files` pattern surface.
fn simple_regex_match(re: &str, text: &str) -> bool {
    fn matches(pat: &[char], txt: &[char]) -> bool {
        if pat.is_empty() {
            return txt.is_empty();
        }
        // Class [^/]
        if pat[0] == '[' {
            if let Some(close) = pat.iter().position(|&c| c == ']') {
                let star = pat.get(close + 1) == Some(&'*');
                let next = if star { close + 2 } else { close + 1 };
                let test = |c: char| c != '/';
                if star {
                    let mut k = 0;
                    loop {
                        if matches(&pat[next..], &txt[k..]) {
                            return true;
                        }
                        if k < txt.len() && test(txt[k]) {
                            k += 1;
                        } else {
                            return false;
                        }
                    }
                } else {
                    return !txt.is_empty() && test(txt[0]) && matches(&pat[next..], &txt[1..]);
                }
            }
        }
        if pat[0] == '.' && pat.get(1) == Some(&'*') {
            let mut k = 0;
            loop {
                if matches(&pat[2..], &txt[k..]) {
                    return true;
                }
                if k < txt.len() {
                    k += 1;
                } else {
                    return false;
                }
            }
        }
        let (lit, rest) = if pat[0] == '\\' && pat.len() > 1 {
            (pat[1], &pat[2..])
        } else {
            (pat[0], &pat[1..])
        };
        !txt.is_empty() && txt[0] == lit && matches(rest, &txt[1..])
    }
    let pat: Vec<char> = re
        .trim_start_matches('^')
        .trim_end_matches('$')
        .chars()
        .collect();
    matches(&pat, &text.chars().collect::<Vec<_>>())
}

/// Cap a deduped symbol-label list to `cap`, appending a `, +N more` tail when
/// truncated (`tools.ts:2657-2659`, `:2872-2875`).
fn cap_header_names(names: &[String], cap: usize) -> String {
    if names.len() <= cap {
        return names.join(", ");
    }
    let shown = names[..cap].join(", ");
    format!("{shown}, +{} more", names.len() - cap)
}

/// Build a clustered file's `#### path — symbols` header, ranking symbols by
/// frequency and capping at `cap` (`tools.ts:2859-2876`).
fn explore_file_header(file_path: &str, symbols: &[String], cap: usize) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for s in symbols {
        if let Some(slot) = counts.iter_mut().find(|(name, _)| name == s) {
            slot.1 += 1;
        } else {
            counts.push((s.clone(), 1));
        }
    }
    counts.sort_by_key(|b| std::cmp::Reverse(b.1));
    let ranked: Vec<String> = counts.into_iter().map(|(name, _)| name).collect();
    format!("#### {file_path} — {}", cap_header_names(&ranked, cap))
}

/// Test/spec/icon/i18n detector for `excludeLowValueFiles` (`tools.ts:2200-2217`).
fn is_low_value_file(path: &str) -> bool {
    let lp = path.to_lowercase();
    is_test_file(&lp) || lp.contains("icon") || lp.contains("i18n")
}

/// Whether the query itself is about tests — keeps the legitimate "explore the
/// tests" case (`tools.ts:2228`).
fn query_mentions_tests(query: &str) -> bool {
    let q = query.to_lowercase();
    q.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| {
        matches!(
            w,
            "test" | "tests" | "testing" | "spec" | "verify" | "verifies"
        )
    })
}

/// Cut `output` at the last `#### ` file-section boundary before `ceiling` so
/// trailing whole sections drop rather than slicing a method body; fall back to
/// a line boundary in the degenerate single-giant-section case
/// (`tools.ts:2964-2975`).
fn cut_at_section_boundary(output: &str, ceiling: usize) -> String {
    if output.len() <= ceiling {
        return output.to_string();
    }
    let cut = &output[..ceiling];
    let last_section = cut.rfind("\n#### ");
    let boundary = match last_section {
        Some(i) if (i as f64) > ceiling as f64 * 0.5 => i,
        _ => cut.rfind('\n').unwrap_or(0),
    };
    let safe = if boundary > 0 { &cut[..boundary] } else { cut };
    format!("{safe}\n\n... (output truncated to budget; the source above is complete and verbatim — treat it as already Read. For any area not covered, run another codegraph_explore with the specific names — do NOT Read these files.)")
}

fn require_string(args: &Value, field: &str) -> Result<String, String> {
    match args.get(field).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(format!("{field} must be a non-empty string")),
    }
}
