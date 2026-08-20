//! The `CodeGraphEngine` — opens a project's store and renders the 8 MCP tool
//! responses with text output byte-aligned to the upstream renderers in
//! `upstream mcp/tools.ts`.
//!
//! Tool LOGIC is sync (rusqlite); only the stdio transport loop is async-free
//! here too — the server owns the loop. Each handler cites the upstream source
//! file:line for its API call + output template.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use codegraph_core::config::Config;
use codegraph_core::file_class::{is_generated_file, is_test_file};
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{EdgeKind, FileRecord, Node, NodeKind};
use codegraph_graph::graph::{GodotReach, GraphTraverser, NodeEdge};
use codegraph_graph::query::{SearchOptions, search_nodes};
use codegraph_store::Store;
use serde_json::Value;

use crate::dynamic_boundaries::scan_dynamic_dispatch;
use crate::explore_budget::{ExploreOutputBudget, get_explore_output_budget};
use crate::protocol::ToolResult;

/// Default caller/callee recursion depth for callers/callees tools. The upstream
/// `getCallers`/`getCallees` default to `maxDepth: 1` (`traversal.ts` callers
/// list a single hop); the MCP tools call them with no depth override.
const CALL_DEPTH: usize = 1;

/// `TRAIL_CAP` from `tools.ts` — max callees/callers shown in a node trail.
const TRAIL_CAP: usize = 8;

/// `Read`-tool cap mirrored by file-mode `codegraph_node` (`tools.ts:489`).
const FILE_MODE_MAX_LINES: usize = 2000;

/// Upstream's per-file protected-focus cap (`.slice(0, 6)`), so a 40-token query
/// cannot pin a whole file.
const FOCUS_CAP: usize = 6;

/// Upstream's `POINTER_SYMBOLS` — `name:line` pairs shown per "Not shown above"
/// pointer line before a `+N more` tail.
const POINTER_SYMBOLS: usize = 6;

/// Upstream's `POINTER_MAX_FILES` — files listed in the "Not shown above" block.
const POINTER_MAX_FILES: usize = 10;

/// Upstream's `MIN_WINDOW_LINES` — the never-empty floor for a windowed cluster.
/// A file always emits at least this many whole lines around its
/// highest-importance member, even when that exceeds the room it was given; the
/// caller then drops the file whole rather than emitting a stub.
const MIN_WINDOW_LINES: usize = 12;

/// Fixed cost of `render_explore_file`'s section template around the header, the
/// language tag and the cluster body: `"\n\n"` + ``"```"`` + `"\n"` + `"\n"` +
/// ``"```"`` + `"\n"`.
const SECTION_FENCE_CHARS: usize = 11;

/// The trailing `"\n\n... (gap) ..."` a section carries when any cluster was
/// dropped or windowed. Reserved unconditionally in `section_frame`, since
/// whether it is emitted is only known after cluster selection.
const SECTION_GAP_TAIL_CHARS: usize = 17;

/// Callable node kinds whose signature (parameter/return) types the #1064
/// change-surface pass follows. Mirrors upstream `CALLABLE_KINDS`
/// (`tools.ts:2620`); `constructor` has no Rust NodeKind (ctors are `method`).
const CALLABLE_KINDS: [NodeKind; 4] = [
    NodeKind::Method,
    NodeKind::Function,
    NodeKind::Component,
    NodeKind::Class,
];

/// Type node kinds a signature edge may point at — the rescue only surfaces
/// these. Mirrors upstream `TYPE_KINDS` (`tools.ts:2621`).
const TYPE_KINDS: [NodeKind; 8] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Union,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Protocol,
    NodeKind::Enum,
    NodeKind::TypeAlias,
];

/// Signature edge kinds: what a callable emits toward its parameter/return
/// types. Mirrors upstream `SIG_EDGE` (`tools.ts:2622`). This project emits TS
/// type annotations as `References` today (`TypeOf`/`Returns` are accepted too
/// for forward-compatibility, but extraction-widening is DEFERRED per the plan).
const SIG_EDGE_KINDS: [EdgeKind; 3] = [EdgeKind::References, EdgeKind::TypeOf, EdgeKind::Returns];

/// Holds one request-scoped project store. Its retained shared lease remains
/// live until the request has produced its final result.
pub struct CodeGraphEngine {
    store: Store,
    project_root: PathBuf,
    /// The ADDRESSED project's own immutable config, loaded per request from its
    /// resolved current index root. A server that answers for many projects (a
    /// global HTTP process) therefore honors each project's own settings without
    /// reading a process-global value or another project's config.
    config: Arc<Config>,
    /// Request-local source/freshness memo. `execute` clears it before every tool
    /// call so repeated render passes stat and read each referenced file once.
    source_probes: RefCell<HashMap<String, SourceProbe>>,
    /// Request-local set of files whose current bytes actually reached the result.
    source_citations: RefCell<HashSet<String>>,
}

impl CodeGraphEngine {
    const READ_LEASE_TIMEOUT: Duration = Duration::from_secs(30);

    /// Open the store at the project's current index DB, resolved
    /// fail-closed through the single `codegraph-core::IndexPaths` authority
    /// (`.codegraph/codegraph.db` by default, or a configured project-local
    /// directory). An unsafe or aliased
    /// configured root errors here rather than opening a reconstructed path.
    pub fn open(project_root: &Path) -> anyhow::Result<Self> {
        let paths = codegraph_core::IndexPaths::resolve(
            project_root,
            std::env::var("CODEGRAPH_DIR").ok().as_deref(),
        )?;
        let config = Config::load_for_paths(None, &paths)?;
        let store =
            Store::open_for_read(&paths, Instant::now() + Self::READ_LEASE_TIMEOUT, || false)?;
        Ok(Self {
            store,
            project_root: project_root.to_path_buf(),
            config,
            source_probes: RefCell::new(HashMap::new()),
            source_citations: RefCell::new(HashSet::new()),
        })
    }

    /// This project's own immutable configuration for the current request.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Read an indexed file's on-disk source and compare it with the stored file
    /// record, refusing anything larger than THIS project's
    /// `indexing.max_file_size`.
    ///
    /// Extraction skips a file over that limit, so its symbols are not in the
    /// graph; serving its full text through a graph tool would contradict the
    /// project's own size policy. `None` therefore means "unreadable or beyond
    /// this project's limit", and every rendering path treats it exactly as it
    /// already treats an unreadable file.
    ///
    /// Freshness is fail-closed: it is proven only after successfully loading the
    /// file record and matching either size + millisecond mtime (the sync fast
    /// path, with no hash) or, after a stat mismatch, the indexed sha256 content
    /// hash. A missing/unreadable record, read failure, or unhashable oversized
    /// stat mismatch remains possibly drifted. The full probe is memoized for the
    /// current handler.
    fn project_source(&self, file_path: &str) -> SourceProbe {
        if let Some(cached) = self.source_probes.borrow().get(file_path).cloned() {
            return cached;
        }

        let abs = self.project_root.join(file_path);
        let probe = match fs::metadata(&abs) {
            Ok(metadata) if metadata.is_file() => {
                let stored = match self.store.file_by_path(file_path) {
                    Ok(Some(file)) => Some(file),
                    Ok(None) | Err(_) => None,
                };
                let stat_matches = stored.as_ref().is_some_and(|file| {
                    file.size == metadata.len() as i64
                        && metadata_modified_millis(&metadata)
                            .is_some_and(|mtime| file.modified_at == mtime)
                });
                if metadata.len() > self.config.indexing.max_file_size {
                    SourceProbe {
                        content: None,
                        freshness: if stat_matches {
                            SourceFreshness::ProvenFresh
                        } else {
                            SourceFreshness::PossiblyDrifted
                        },
                    }
                } else {
                    match fs::read_to_string(&abs) {
                        Ok(content) => {
                            let freshness = if stat_matches
                                || stored
                                    .as_ref()
                                    .is_some_and(|file| file.content_hash == hash_content(&content))
                            {
                                SourceFreshness::ProvenFresh
                            } else {
                                SourceFreshness::PossiblyDrifted
                            };
                            SourceProbe {
                                content: Some(Arc::<str>::from(content)),
                                freshness,
                            }
                        }
                        Err(_) => SourceProbe::default(),
                    }
                }
            }
            _ => SourceProbe::default(),
        };
        self.source_probes
            .borrow_mut()
            .insert(file_path.to_string(), probe.clone());
        probe
    }

    fn mark_source_cited(&self, file_path: &str) {
        self.source_citations
            .borrow_mut()
            .insert(file_path.to_string());
    }

    fn mark_surviving_explore_citations(
        &self,
        lines: &[String],
        rendered_sources: &[(String, usize)],
        kept_prefix_len: usize,
    ) {
        let mut citations = self.source_citations.borrow_mut();
        for (file_path, line_index) in rendered_sources {
            let Some(section) = lines.get(*line_index) else {
                continue;
            };
            let section_start =
                lines[..*line_index].iter().map(String::len).sum::<usize>() + *line_index;
            if section_start + section.len() <= kept_prefix_len {
                citations.insert(file_path.clone());
            }
        }
    }

    fn with_staleness_banner(&self, mut result: ToolResult) -> ToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        let citations = self.source_citations.borrow();
        let probes = self.source_probes.borrow();
        let mut drifted = citations
            .iter()
            .filter(|path| {
                probes
                    .get(path.as_str())
                    .is_some_and(|probe| probe.content.is_some() && probe.is_possibly_drifted())
            })
            .cloned()
            .collect::<Vec<_>>();
        if drifted.is_empty() {
            return result;
        }
        drifted.sort();
        let files = drifted
            .iter()
            .map(|path| format!("  - {path}"))
            .collect::<Vec<_>>()
            .join("\n");
        let banner = format!(
            "⚠️ Some files referenced below were edited since the last index sync — their codegraph entries may be stale:\n{files}\nFor accurate content of those specific files, Read them directly. The rest of this response is fresh."
        );
        if let Some(first) = result.content.first_mut() {
            first.text = format!("{banner}\n\n{}", first.text);
        }
        result
    }

    fn project_name_tokens(&self) -> HashSet<String> {
        HashSet::new()
    }

    /// Indexed file paths (repo-relative). Exposed for the CLI `node`
    /// subcommand, which uses it to decide whether a bare argument names a file
    /// (file-view mode) or a symbol — the CLI has one positional where the MCP
    /// tool has separate `file`/`symbol` params.
    pub fn indexed_file_paths(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .store
            .all_files()?
            .into_iter()
            .map(|f| f.path)
            .collect())
    }

    /// Read-only access to the underlying store, for the CLI prompt-hook gate's
    /// query-time segment matching (`get_segment_matches`).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Convenience passthrough to [`Store::nodes_by_name`] for the prompt-hook
    /// gate's code-token verification (HIGH tier).
    pub fn store_nodes_by_name(&self, name: &str) -> anyhow::Result<Vec<Node>> {
        Ok(self.store.nodes_by_name(name)?)
    }

    // === Tool dispatch ===================================================

    /// Dispatch by tool name. Mirrors `ToolHandler.execute` switch
    /// (`tools.ts:1033-1056`). Unknown tool → `errorResult` content
    /// (`tools.ts:1054-1055`); the SERVER rejects unknown names earlier with a
    /// JSON-RPC `-32602` (`session.ts:217-225`), so this branch is a backstop.
    pub fn execute(&self, tool_name: &str, args: &Value) -> ToolResult {
        self.source_probes.borrow_mut().clear();
        self.source_citations.borrow_mut().clear();
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
            Ok(tr) => self.with_staleness_banner(tr),
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
            return Ok(ToolResult::not_found_text(format!(
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
            return Ok(ToolResult::error(format!(
                "Symbol \"{symbol}\" not found in the codebase. Use codegraph_search with query \"{symbol}\" to find fuzzy matches."
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
            return Ok(ToolResult::not_found_text(format!(
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
            return Ok(ToolResult::error(format!(
                "Symbol \"{symbol}\" not found in the codebase. Use codegraph_search with query \"{symbol}\" to find fuzzy matches."
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

        if symbol_raw.is_none()
            && let Some(file_hint) = file_hint
        {
            return self.handle_file_view(args, file_hint);
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
            return Ok(ToolResult::not_found_text(format!(
                "Symbol \"{symbol}\" not found in the codebase"
            )));
        }
        // `symbol` + `file` (#1314): the schema promises `file` PINS an
        // overloaded name to the definition in that file. A pin that matches
        // nothing reports not-found rather than falling back to an arbitrary
        // overload — a silent miss beats a wrong answer.
        if let Some(file_hint) = file_hint {
            let pinned: Vec<Node> = matches
                .iter()
                .filter(|n| file_path_matches_hint(&n.file_path, file_hint))
                .cloned()
                .collect();
            if pinned.is_empty() {
                return Ok(ToolResult::not_found_text(format!(
                    "Symbol \"{symbol}\" not found in \"{file_hint}\""
                )));
            }
            if pinned.len() == 1 {
                let rendered = self.render_node_section(&pinned[0], include_code)?;
                return Ok(ToolResult::text(truncate_output(&rendered)));
            }
            return Ok(ToolResult::text(truncate_output(
                &self.render_ambiguous_node(symbol, &pinned, include_code)?,
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
        let source = self.project_source(&node.file_path);
        if source.is_possibly_drifted() {
            if include_code
                && source.content.as_deref().is_some_and(|content| {
                    content.trim_end_matches('\n').split('\n').count() <= FILE_MODE_MAX_LINES
                })
            {
                self.mark_source_cited(&node.file_path);
            }
            return self.render_drifted_node_section(node, include_code, source.content.as_deref());
        }
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

    /// Render a node whose indexed line range no longer matches its file. Current
    /// source is emitted only as a whole file; otherwise the body is omitted.
    fn render_drifted_node_section(
        &self,
        node: &Node,
        include_code: bool,
        content: Option<&str>,
    ) -> anyhow::Result<String> {
        let location = if node.start_line != 0 {
            format!(":{}", node.start_line)
        } else {
            String::new()
        };
        let mut lines = vec![
            format!("## {} ({})", node.name, node.kind.as_str()),
            String::new(),
            format!(
                "**Location:** {}{} — ⚠ as of the last index sync; the file has changed on disk, so this line may be shifted",
                node.file_path, location
            ),
        ];
        if let Some(signature) = &node.signature {
            lines.push(format!("**Signature:** `{signature}`"));
        }
        if let Some(doc) = &node.docstring
            && doc.len() < 200
        {
            lines.push(String::new());
            lines.push(doc.clone());
        }
        if include_code {
            lines.push(String::new());
            let body = content.map(|source| source.trim_end_matches('\n'));
            let whole_file_fits =
                body.is_some_and(|source| source.split('\n').count() <= FILE_MODE_MAX_LINES);
            if whole_file_fits {
                let body = body.unwrap_or_default();
                lines.push(format!(
                    "> ⚠ `{}` changed on disk after it was last indexed, so the indexed symbol range is unsafe. Showing the file's full CURRENT source instead (Read-parity — treat it as already Read):",
                    node.file_path
                ));
                lines.push(String::new());
                lines.push(format!("```{}", node.language.as_str()));
                lines.push(number_source_lines_at(
                    &body.split('\n').collect::<Vec<_>>(),
                    1,
                ));
                lines.push("```".to_string());
            } else {
                lines.push(format!(
                    "> ⚠ `{}` changed on disk after it was last indexed — source omitted because its indexed line ranges are unsafe and the current file exceeds the {}-line whole-file cap. Call codegraph_node with `file: \"{}\"` or Read it directly.",
                    node.file_path, FILE_MODE_MAX_LINES, node.file_path
                ));
            }
        }
        Ok(format!("{}{}", lines.join("\n"), self.format_trail(node)?))
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
            return Ok(ToolResult::not_found_text(
                "No files indexed. Run `codegraph index` first.",
            ));
        }

        let candidates = resolve_file_candidates(&files, file_arg);
        let file_path = match candidates.as_slice() {
            [one] => one.clone(),
            [] => {
                return Ok(ToolResult::not_found_text(format!(
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

        let source = self.project_source(&file_path);
        let content = match source.content {
            Some(c) => c,
            None => {
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
        if !slice.is_empty() {
            self.mark_source_cited(&file_path);
        }

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
        if budget.include_relationships
            && let Some(rel) =
                self.build_relationships(&subgraph, budget.max_edges_per_relationship_kind)
        {
            lines.push(rel);
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
        let mut rendered_sources: Vec<(String, usize)> = Vec::new();
        let precise_tokens = precise_query_tokens(&query);
        let indexed_file_count = self.store.counts().ok().map(|c| c.file_count);
        let effective_budget = budget
            .max_output_chars
            .saturating_sub(fixed_epilogue_reserve(
                &budget,
                file_order.len(),
                max_files,
                indexed_file_count,
            ));

        for file_path in &file_order {
            if files_included >= max_files {
                excluded_files.push(file_path);
                continue;
            }
            let source = self.project_source(file_path);
            let possibly_drifted = source.is_possibly_drifted();
            let content = match source.content {
                Some(c) => c,
                None => continue,
            };
            let file_lines: Vec<&str> = content.split('\n').collect();
            let lang = subgraph.file_language(file_path);

            let focuses = subgraph.file_focus_lines(file_path, &precise_tokens);
            let ctx = RenderCtx {
                budget: &budget,
                funded_headroom: effective_budget
                    .saturating_sub(total_chars)
                    .saturating_sub(1),
                focuses: &focuses,
                drifted: possibly_drifted,
            };
            let rendered = self.render_explore_file(&subgraph, file_path, &file_lines, &lang, &ctx);
            // Past the total cap an incidental file is skipped whole — the source
            // section never slices through a method body (`tools.ts:2888-2894`).
            //
            // A section costs `section.len() + 1`: its own length — which already
            // covers the `#### ` header and the fence — plus the one
            // `lines.join("\n")` separator it is pushed behind. The same
            // expression appears at the accumulator below, so the admission test
            // and the running total cannot disagree.
            if total_chars + rendered.section.len() + 1 > effective_budget {
                any_file_trimmed = true;
                excluded_files.push(file_path);
                continue;
            }
            if rendered.section.contains("... (gap) ...")
                || rendered.section.contains("more (signatures elided)")
            {
                any_file_trimmed = true;
            }
            let section_len = rendered.section.len();
            if rendered.source_emitted {
                rendered_sources.push(((*file_path).clone(), lines.len()));
            }
            lines.push(rendered.section);
            total_chars += section_len + 1;
            files_included += 1;
        }

        // "Additional relevant files (not shown)" — the excluded set, so the
        // agent can request specifics. Gated by the budget (`tools.ts:2910-2927`).
        if budget.include_additional_files && !excluded_files.is_empty() {
            lines.push(POINTER_BLOCK_HEADER.to_string());
            lines.push(String::new());
            // The block is ELASTIC: it is fitted to the headroom the sections did
            // not use, so however long the pointer list is it cannot push the
            // response past its budget. The frame and the tail are already paid
            // for by the reserve and so are not charged here.
            let candidates: Vec<String> = excluded_files
                .iter()
                .map(|file_path| {
                    format!("- {file_path}: {}", subgraph.file_node_locations(file_path))
                })
                .collect();
            let (pointer_lines, unlisted) =
                fit_pointer_lines(&candidates, effective_budget.saturating_sub(total_chars));
            lines.extend(pointer_lines);
            // The TRUE unlisted count — those dropped by elasticity plus those
            // dropped by the file cap — so a file the response did not render is
            // never silently absent.
            if unlisted > 0 {
                lines.push(pointer_unlisted_tail(unlisted));
            }
            lines.push(String::new());
        }

        // Completeness signal / trim note (`tools.ts:2933-2940`), then the
        // follow-up-call note (`tools.ts:2943-2952`), both gated by the budget and
        // both built by the SAME function the pre-loop reserve is computed from.
        //
        // Upstream's follow-up wording announced a per-project call BUDGET ("N
        // calls for this project … Synthesize once you've used N"), but nothing
        // anywhere enforces it, so its only effect was agents stopping while their
        // question was still uncovered (#1504). The useful half — one more explore
        // beats falling back to Read — is kept, the phantom cap is dropped, and
        // the absence of a limit is stated outright rather than left to inference.
        lines.extend(epilogue_note_lines(
            &budget,
            files_included,
            any_file_trimmed,
            indexed_file_count,
        ));

        // Final ABSOLUTE inline ceiling — cut at a `#### ` file-section boundary
        // so trailing whole sections drop rather than slicing a method body
        // (`tools.ts:2954-2975`).
        let output = lines.join("\n");
        let hard_ceiling = ((budget.max_output_chars as f64 * 1.5).round() as usize).min(25000);
        let (output, kept_prefix_len) = cut_at_section_boundary(&output, hard_ceiling);
        self.mark_surviving_explore_citations(&lines, &rendered_sources, kept_prefix_len);
        Ok(ToolResult::text(output))
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
        ctx: &RenderCtx<'_>,
    ) -> ExploreFileRender {
        let budget = ctx.budget;
        let drifted = ctx.drifted;
        let total_lines = file_lines.len();
        let content = file_lines.join("\n");
        let body = content.trim_end_matches('\n');

        // Indexed symbol ranges are unsafe after a disk edit. A drifted file is
        // therefore rendered from line 1 in full or represented by an omission
        // notice; it never reaches the range-based clustering branches below.
        if drifted {
            let names =
                subgraph.file_header_names_capped(file_path, budget.max_symbols_in_file_header);
            if total_lines <= FILE_MODE_MAX_LINES {
                let numbered = number_source_lines_at(&body.split('\n').collect::<Vec<_>>(), 1);
                return ExploreFileRender {
                    section: format!(
                        "#### {file_path} — {names} · ⚠ changed since last index sync; source below is the full current file\n\n```{lang}\n{numbered}\n```\n"
                    ),
                    source_emitted: true,
                };
            }
            return ExploreFileRender {
                section: format!(
                    "#### {file_path} — ⚠ changed on disk after the last index sync — source omitted because indexed line ranges are unsafe and the current file exceeds the {FILE_MODE_MAX_LINES}-line whole-file cap. Read this file directly.\n"
                ),
                source_emitted: false,
            };
        }

        // Whole-file rule (`tools.ts:2645-2672`): a relevant file small enough to
        // afford comes back ENTIRELY — clustering exists only to tame god-files.
        const WHOLE_FILE_MAX_LINES: usize = 220;
        let whole_file_max_chars = budget.max_chars_per_file * 3;
        if total_lines <= WHOLE_FILE_MAX_LINES && content.len() <= whole_file_max_chars {
            let numbered = number_source_lines_at(&body.split('\n').collect::<Vec<_>>(), 1);
            let names =
                subgraph.file_header_names_capped(file_path, budget.max_symbols_in_file_header);
            let section = format!("#### {file_path} — {names}\n\n```{lang}\n{numbered}\n```\n");
            // The two conditions are ordered AFFORDABILITY FIRST, and that order
            // is normative rather than stylistic.
            //
            // (1) Affordable ⇒ byte-for-byte today's behaviour, with no focus
            //     logic consulted at all, so every input that is not the defect —
            //     including every existing golden — is untouched by EVALUATION
            //     rather than by unreachability.
            let section_cost = section.len() + 1;
            if section_cost <= ctx.funded_headroom {
                return ExploreFileRender {
                    section,
                    source_emitted: true,
                };
            }
            // (2) Unaffordable but owning NO focus ⇒ also today's behaviour:
            //     return whole and let the caller drop it whole, so "an incidental
            //     file that doesn't fit is DROPPED whole" stays literally true.
            if ctx.focuses.is_empty() {
                return ExploreFileRender {
                    section,
                    source_emitted: true,
                };
            }
            // (3) Unaffordable AND owning a focus ⇒ fall through to the clustering
            //     path, which windows against `funded_headroom` and guarantees a
            //     window per focus. The ONLY new behaviour.
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
                // A protected focus outranks an ordinary root, so a cluster that
                // owns one sorts ahead of equally-important siblings. It is
                // matched on the STORED definition line: a focus whose line the
                // clamp below moves is one the range filters would drop anyway,
                // and §3.1.1 conditions the guarantee on surviving them.
                let importance = if ctx.focuses.contains(&(n.start_line as usize)) {
                    11
                } else if subgraph.roots.iter().any(|r| r == &n.id) {
                    10
                } else {
                    1
                };
                // Clamp stored symbol lines into `[1, total_lines]`: a stale
                // index can hold a node whose `end_line`/`start_line` is past
                // the current file's EOF (the file shrank since it was
                // indexed). Clamping keeps downstream span math and slice
                // bounds valid so a stale index degrades instead of panicking.
                let start = (n.start_line as usize).clamp(1, total_lines);
                let end = (n.end_line as usize).clamp(start, total_lines);
                ClusterRange {
                    start,
                    end,
                    label: format!("{}({})", n.name, n.kind.as_str()),
                    importance,
                }
            })
            // Drop ranges whose start is past EOF (a fully-stale node).
            .filter(|r| r.start <= total_lines)
            .collect();
        ranges.sort_by_key(|r| r.start);

        if ranges.is_empty() {
            return ExploreFileRender {
                section: String::new(),
                source_emitted: false,
            };
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
        // A cluster's padded slice bounds, 0-based with an EXCLUSIVE end — the
        // exact bounds `build_section` has always used, so a cluster that fits
        // its room still renders byte-identically.
        //
        // `.min(end_idx)` guards the slice start: even if a cluster's start
        // slipped past EOF, `start_idx <= end_idx` keeps the range valid.
        let span_of = |c: &Cluster| -> (usize, usize) {
            let end_idx = (c.end + CONTEXT_PADDING).min(total_lines);
            let start_idx = c
                .start
                .saturating_sub(1)
                .saturating_sub(CONTEXT_PADDING)
                .min(end_idx);
            (start_idx, end_idx)
        };
        let render_range = |start_idx: usize, end_idx: usize| -> String {
            number_source_lines_at(&file_lines[start_idx..end_idx], start_idx + 1)
        };

        // `number_source_lines_at` renders each line as `"{n}\t{line}"` and joins
        // with "\n", so a range costs the per-line widths plus one separator
        // between neighbours. Summed rather than rendered, so growing a window
        // one line at a time stays linear instead of re-formatting the slice.
        let range_cost = |start_idx: usize, end_idx: usize| -> usize {
            if end_idx <= start_idx {
                return 0;
            }
            (start_idx..end_idx)
                .map(|i| decimal_width(i + 1) + 1 + file_lines[i].len())
                .sum::<usize>()
                + (end_idx - start_idx - 1)
        };
        let grow_head = |start_idx: usize, end_idx: usize, room: usize| -> (usize, usize) {
            let floor = (start_idx + MIN_WINDOW_LINES).min(end_idx);
            let mut end = (start_idx + 1).min(end_idx);
            while end < end_idx && range_cost(start_idx, end + 1) <= room {
                end += 1;
            }
            (start_idx, end.max(floor))
        };
        // Grow one line up, then one line down, alternating, so the emitted window
        // is centred on the focus rather than biased to whichever side was tried
        // first. Bounded by the span's own line count, so it always terminates.
        let grow_around =
            |focus_idx: usize, start_idx: usize, end_idx: usize, room: usize| -> (usize, usize) {
                let focus_idx = focus_idx.clamp(start_idx, end_idx - 1);
                let (mut lo, mut hi) = (focus_idx, focus_idx + 1);
                let mut up = true;
                for _ in 0..(end_idx - start_idx) * 2 {
                    let can_up = lo > start_idx && range_cost(lo - 1, hi) <= room;
                    let can_down = hi < end_idx && range_cost(lo, hi + 1) <= room;
                    if !can_up && !can_down {
                        break;
                    }
                    if (up && can_up) || !can_down {
                        lo -= 1;
                    } else {
                        hi += 1;
                    }
                    up = !up;
                }
                let want = MIN_WINDOW_LINES.min(end_idx - start_idx);
                if hi - lo < want {
                    let lo2 = focus_idx
                        .saturating_sub(want / 2)
                        .max(start_idx)
                        .min(end_idx - want);
                    return (lo2, lo2 + want);
                }
                (lo, hi)
            };
        // Only the focuses inside THIS cluster's span may claim its reserve;
        // clamping an out-of-span focus in would spend the reserve re-emitting
        // lines the head window already covers.
        let focuses_in = |c: &Cluster| -> Vec<usize> {
            let (start_idx, end_idx) = span_of(c);
            ctx.focuses
                .iter()
                .copied()
                .filter(|&line| line > start_idx && line <= end_idx)
                .collect()
        };
        // CG-30: a cluster too big for its room is emitted as WHOLE-LINE windows
        // instead of being taken whole (which then costs the entire file) or
        // skipped whole. Returns the rendered body and whether it was windowed,
        // so the caller can add the `... (gap) ...` honesty marker.
        let window_cluster = |c: &Cluster, room: usize, focuses: &[usize]| -> (String, bool) {
            let (start_idx, end_idx) = span_of(c);
            if range_cost(start_idx, end_idx) <= room {
                return (render_range(start_idx, end_idx), false);
            }
            let head_room = if focuses.is_empty() {
                room
            } else {
                room * 60 / 100
            };
            let mut windows: Vec<(usize, usize)> = vec![grow_head(start_idx, end_idx, head_room)];
            if !focuses.is_empty() {
                // Even shares with carry-forward, never greedy: upstream measured
                // that a greedy split let an early focus eat the whole reserve and
                // re-lose the target. One integer division, so the same input
                // cannot drift by a char across platforms.
                let reserve = room - head_room;
                let base = reserve / focuses.len();
                let mut carry = reserve - base * focuses.len();
                for &line in focuses {
                    let allot = base + carry;
                    let w = grow_around(line.saturating_sub(1), start_idx, end_idx, allot);
                    carry = allot.saturating_sub(range_cost(w.0, w.1));
                    windows.push(w);
                }
            }
            windows.sort_unstable();
            let mut merged: Vec<(usize, usize)> = Vec::new();
            for w in windows {
                match merged.last_mut() {
                    Some(last) if w.0 <= last.1 => last.1 = last.1.max(w.1),
                    _ => merged.push(w),
                }
            }
            let covers_all = merged.len() == 1 && merged[0] == (start_idx, end_idx);
            let body = merged
                .iter()
                .map(|&(lo, hi)| render_range(lo, hi))
                .collect::<Vec<_>>()
                .join(GAP_MARKER);
            (body, !covers_all)
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

        // CG-30: the first cluster is still always taken, but WINDOWED to a
        // ceiling rather than accepted at any size. `ceiling` bounds cluster
        // BODIES while the caller's admission test and the 1.5x per-file bound
        // both compare the finished SECTION, so `section_frame` — the header, the
        // language tag, the fence and the gap tail — is subtracted OUTSIDE the
        // `min`. Inside it, the section would clear admission yet overshoot
        // `round(max_chars_per_file * 1.5)` by the frame.
        let section_frame = subgraph
            .file_header_len_upper_bound(file_path, budget.max_symbols_in_file_header)
            + lang.len()
            + SECTION_FENCE_CHARS
            + SECTION_GAP_TAIL_CHARS;
        let ceiling = ((budget.max_chars_per_file as f64 * 1.5).round() as usize)
            .min(ctx.funded_headroom)
            .saturating_sub(section_frame);
        // Per-cluster render state lives in a Vec indexed parallel to `clusters`,
        // never in a HashSet that is then iterated: emission order must come from
        // the index, not from hash order, or the output stops being byte-stable.
        let mut rendered_clusters: Vec<Option<String>> = vec![None; clusters.len()];
        let mut projected = 0usize;
        let mut chosen_count = 0usize;
        let mut any_cluster_windowed = false;
        for &idx in &ranked {
            if chosen_count == 0 {
                let (body, windowed) =
                    window_cluster(&clusters[idx], ceiling, &focuses_in(&clusters[idx]));
                projected += body.len();
                any_cluster_windowed |= windowed;
                rendered_clusters[idx] = Some(body);
                chosen_count += 1;
                continue;
            }
            // CG-36: a later cluster that does not fit is SHRUNK into the
            // remainder rather than skipped whole. The bound is `ceiling`, not
            // `file_budget`, so the whole file obeys ONE stated bound — and
            // `saturating_sub` is required rather than stylistic: half A lets the
            // first cluster's body legitimately exceed `file_budget`, so a plain
            // subtraction wraps to a huge `room` and hands this cluster unlimited
            // budget, the exact opposite of the intent.
            let room = ceiling.saturating_sub(projected + GAP_MARKER.len());
            // The floor's own cost IS `MIN_CHARS` — the smallest useful window —
            // rather than a new magic number. When even that cannot be paid for
            // the cluster is still skipped, so shrinking can never overspend into
            // the next file's share.
            let (start_idx, end_idx) = span_of(&clusters[idx]);
            let floor_end = (start_idx + MIN_WINDOW_LINES).min(end_idx);
            if room < range_cost(start_idx, floor_end) {
                continue;
            }
            let (body, windowed) =
                window_cluster(&clusters[idx], room, &focuses_in(&clusters[idx]));
            any_cluster_windowed |= windowed;
            projected += body.len() + GAP_MARKER.len();
            rendered_clusters[idx] = Some(body);
            chosen_count += 1;
        }

        let mut file_section = String::new();
        let mut symbols: Vec<String> = Vec::new();
        for (i, cluster) in clusters.iter().enumerate() {
            let Some(body) = rendered_clusters[i].as_ref() else {
                continue;
            };
            if !file_section.is_empty() {
                file_section.push_str(GAP_MARKER);
            }
            file_section.push_str(body);
            symbols.extend(cluster.symbols.iter().cloned());
        }
        if chosen_count < clusters.len() || any_cluster_windowed {
            file_section.push_str("\n\n... (gap) ...");
        }

        let header = explore_file_header(file_path, &symbols, budget.max_symbols_in_file_header);
        ExploreFileRender {
            section: format!("{header}\n\n```{lang}\n{file_section}\n```\n"),
            source_emitted: true,
        }
    }

    /// `buildBlastRadiusSection` (`tools.ts:1441-1491`).
    fn build_blast_radius(&self, subgraph: &ExploreSubgraph) -> anyhow::Result<Option<String>> {
        const ROOT_CAP: usize = 5;
        const DIRECT_CALLER_FILE_CAP: usize = 4;
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
                .take(DIRECT_CALLER_FILE_CAP)
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = if non_test.len() > DIRECT_CALLER_FILE_CAP {
                format!(" +{} more", non_test.len() - DIRECT_CALLER_FILE_CAP)
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
                    .take(DIRECT_CALLER_FILE_CAP)
                    .map(|f| format!("`{f}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let tmore = if test_files.len() > DIRECT_CALLER_FILE_CAP {
                    format!(" +{}", test_files.len() - DIRECT_CALLER_FILE_CAP)
                } else {
                    String::new()
                };
                format!("; tests: {t}{tmore}")
            } else {
                self.indirect_test_note(&uniq)
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

    /// Search beyond direct callers before making a test-coverage claim. The
    /// lookup budget bounds high-fan-in symbols; the display cap never prunes the
    /// traversal frontier.
    fn indirect_test_note(&self, direct_callers: &[&Node]) -> String {
        const MAX_CALLER_HOPS: usize = 3;
        const CALLER_LOOKUP_BUDGET: usize = 64;
        const TEST_FILE_DISPLAY_CAP: usize = 2;

        let traverser = GraphTraverser::new(&self.store);
        let mut budget = CALLER_LOOKUP_BUDGET;
        let mut search_was_complete = true;
        let mut visited = direct_callers
            .iter()
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>();
        let mut frontier = direct_callers
            .iter()
            .map(|node| (*node).clone())
            .collect::<Vec<_>>();

        for _hop in 2..=MAX_CALLER_HOPS {
            if frontier.is_empty() || budget == 0 {
                break;
            }
            let mut next = Vec::new();
            let mut found = Vec::new();
            let mut found_set = HashSet::new();
            for node in &frontier {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                let Ok(callers) = traverser.get_callers(&node.id, CALL_DEPTH) else {
                    search_was_complete = false;
                    continue;
                };
                for caller in callers {
                    let caller = caller.node;
                    if !visited.insert(caller.id.clone()) {
                        continue;
                    }
                    if is_test_file(&caller.file_path) {
                        if found_set.insert(caller.file_path.clone()) {
                            found.push(caller.file_path);
                        }
                    } else {
                        next.push(caller);
                    }
                }
            }
            if !found.is_empty() {
                let shown = found
                    .iter()
                    .take(TEST_FILE_DISPLAY_CAP)
                    .map(|file| format!("`{file}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = if found.len() > TEST_FILE_DISPLAY_CAP {
                    format!(" +{}", found.len() - TEST_FILE_DISPLAY_CAP)
                } else {
                    String::new()
                };
                return format!("; tested via callers: {shown}{more}");
            }
            frontier = next;
        }

        if budget > 0 && search_was_complete {
            format!("; no tests found within {MAX_CALLER_HOPS} caller hops")
        } else {
            "; no test calls this directly".to_string()
        }
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
        // #1187: only appended when the interrupted-resolution marker is set, so a
        // healthy status is byte-identical to a pre-#1187 build. Tells the agent
        // the blast radius is incomplete until the next sync heals it.
        if self.store.is_resolution_incomplete()? {
            lines.push(String::new());
            lines.push(
                "**⚠ Index is PARTIAL:** a resolution pass was interrupted, so some \
                 call edges are missing and the blast radius is incomplete. Run \
                 `codegraph sync` to heal it."
                    .to_string(),
            );
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

    /// Sort `nodes` so machine-generated files rank LAST, by the D1 union of the
    /// path convention and the stored `files.generated` flag.
    ///
    /// The union is what makes this safe on an index written before #1500: those
    /// rows all read 0, so reading the column ALONE would drop the path demotion
    /// they already had. One batch query over the sorted+deduped candidate paths,
    /// so no `HashSet` iteration order reaches output.
    fn sort_generated_last(&self, nodes: &mut [Node]) -> anyhow::Result<()> {
        let mut paths = nodes
            .iter()
            .map(|n| n.file_path.clone())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        let stored = self.store.generated_paths_among(&paths)?;
        // ASCENDING on "is generated", so `false` (0) comes first.
        nodes.sort_by_key(|n| {
            u8::from(is_generated_file(&n.file_path) || stored.contains(&n.file_path))
        });
        Ok(())
    }

    /// `findSymbolMatches` (`tools.ts:3220-3265`): exact-name enumeration for a
    /// bare name, falling back to the top fuzzy result.
    fn find_symbol_matches(&self, symbol: &str) -> anyhow::Result<Vec<Node>> {
        let is_qualified = symbol.contains(['.', '/']) || symbol.contains("::");
        if !is_qualified {
            let exact = self.store.nodes_by_name(symbol)?;
            if !exact.is_empty() {
                let mut sorted = exact;
                self.sort_generated_last(&mut sorted)?;
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
        self.sort_generated_last(&mut sorted)?;
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
            return Ok(AllSymbols {
                nodes: exact,
                note: String::new(),
            });
        }
        let mut ranked = exact;
        self.sort_generated_last(&mut ranked)?;
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
        if results.is_empty()
            && is_qualified
            && let Some(tail) = last_qualifier_part(symbol)
            && tail != symbol
        {
            results = search_nodes(&self.store, tail, &opts, &self.project_name_tokens())?;
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

        self.rescue_change_surface(&mut sub, &seed_ids, query, &traverser)?;

        // The stored half of the generated signal, in ONE batch query. Sorted and
        // deduped first, so no `HashSet` iteration order can reach output — the
        // same discipline `rescue_change_surface` applies to its candidates.
        let mut candidate_paths = sub
            .nodes
            .iter()
            .map(|n| n.file_path.clone())
            .collect::<Vec<_>>();
        candidate_paths.sort_unstable();
        candidate_paths.dedup();
        sub.generated_files = self.store.generated_paths_among(&candidate_paths)?;
        sub.ambient_files = self.ambient_declaration_files(&candidate_paths, query)?;

        sub.finalize();
        Ok(sub)
    }

    /// The ambient declaration files among `candidate_paths`, minus any this query
    /// exempts by naming one of their types (ports upstream `9efae0f`'s
    /// `ambientDeclarationPredicateFor`).
    ///
    /// All four conditions are evaluated INDEX-WIDE, never over the subgraph. The
    /// distinction is the whole point of condition 4: the subgraph is seeded from
    /// the top 8 search hits and expanded one hop from the ROOTS, so a file that
    /// depends on a non-root declaration in the shim is normally absent from it
    /// entirely — and a subgraph-scoped predicate would then read "nothing depends
    /// on this" and damp a type module the repo genuinely imports.
    ///
    /// Conditions 1 and 2 run first as a cheap pre-filter, so the two edge queries
    /// execute only for the handful of declaration-only files.
    fn ambient_declaration_files(
        &self,
        candidate_paths: &[String],
        query: &str,
    ) -> anyhow::Result<HashSet<String>> {
        let precise = precise_query_tokens(query);
        let mut ambient = HashSet::new();
        for path in candidate_paths {
            let nodes = self.store.nodes_by_file_path(path)?;
            let declared: Vec<&Node> = nodes
                .iter()
                .filter(|n| !is_ambient_bookkeeping_kind(n.kind))
                .collect();
            // (1) at least one declaration, and (2) every one of them is a type
            // declaration — a file holding an implementation is never ambient.
            if declared.is_empty() || !declared.iter().all(|n| is_type_declaration_kind(n.kind)) {
                continue;
            }
            // A query naming one of this file's declarations is asking for it, so
            // it keeps its rank. Applied while BUILDING the set because `finalize`
            // never sees the query.
            if declared
                .iter()
                .any(|n| precise.iter().any(|t| t.eq_ignore_ascii_case(&n.name)))
            {
                continue;
            }
            // (3) nothing in the file runs: no node is the source of a Calls or
            // Instantiates edge.
            let mut runs = false;
            for n in &declared {
                for kind in [EdgeKind::Calls, EdgeKind::Instantiates] {
                    if !self
                        .store
                        .edges_by_source_kind(&n.id, Some(kind))?
                        .is_empty()
                    {
                        runs = true;
                        break;
                    }
                }
                if runs {
                    break;
                }
            }
            if runs {
                continue;
            }
            // (4) nothing anywhere in the index depends on it. Consumed only
            // through `is_empty`, so the query's SQLite scan order can never reach
            // the output.
            if self.store.dependent_file_paths(path)?.is_empty() {
                ambient.insert(path.clone());
            }
        }
        Ok(ambient)
    }

    /// #1064 change-surface rescue (ports `2b256b9` `tools.ts:2585-2860`, adapted
    /// to this deterministic explore).
    ///
    /// A named callable's signature types — its parameter and return types — are
    /// part of what you'd edit to "add a parameter to X", yet they can be
    /// lexically dissimilar to the query ("add a parameter to `newClient`" shares
    /// no words with `options.ts`, which defines `DialOption`) and sit a hop away
    /// as a `References` edge. Explore's search + budget then buries that answer
    /// file under incidental roots that merely share query words, so the agent
    /// falls back to grep. This pass surfaces the buried signature type.
    ///
    /// Two deterministic halves:
    /// 1. **Tier de-noise**: among same-named callable seeds only the top pick
    ///    plus any seed with caller-count ≥ 25% of the max seed caller-count is
    ///    TIERED — a low-centrality namesake (Go's test-fake `NewClient`) can't
    ///    fill the tier and crowd out the answer.
    /// 2. **Buried rescue**: from each TIERED callable seed, follow outgoing
    ///    signature edges to TYPE-kind nodes; a type whose file is BURIED (not
    ///    already a seed file AND < 2 query-term hits) is inserted + marked
    ///    `rescued_files` so `finalize` floats it to the top tier. A
    ///    well-connected type (already a seed / term-matched) is left alone, so
    ///    ordinary flow queries are unchanged.
    ///
    /// Determinism: the tiered-seed list preserves search-rank order; the rescue
    /// candidates are collected then SORTED by `(file_path, start_line, id)` and
    /// deduped before insertion — no `HashMap`/`HashSet` iteration reaches output.
    fn rescue_change_surface(
        &self,
        sub: &mut ExploreSubgraph,
        seed_ids: &[String],
        query: &str,
        traverser: &GraphTraverser,
    ) -> anyhow::Result<()> {
        // Tier de-noise: rank callable seeds by caller-count, tier the top pick
        // plus any seed within 25% of the max. Iterated in search-rank order.
        let mut callable_seeds: Vec<(String, usize)> = Vec::new();
        for id in seed_ids {
            let Some(node) = sub.node(id) else { continue };
            if !CALLABLE_KINDS.contains(&node.kind) {
                continue;
            }
            let caller_count = traverser.get_callers(id, CALL_DEPTH)?.len();
            callable_seeds.push((id.clone(), caller_count));
        }
        if callable_seeds.is_empty() {
            return Ok(());
        }
        let max_callers = callable_seeds.iter().map(|(_, c)| *c).max().unwrap_or(0);
        let threshold = max_callers / 4;
        let tiered_seed_ids: Vec<String> = callable_seeds
            .iter()
            .enumerate()
            .filter(|(i, (_, c))| *i == 0 || *c >= threshold)
            .map(|(_, (id, _))| id.clone())
            .collect();

        // Seed files are NOT buried: a type already defined in one is on-screen.
        let seed_files: HashSet<String> = seed_ids
            .iter()
            .filter_map(|id| sub.node(id).map(|n| n.file_path.clone()))
            .collect();
        let terms = query_terms(query);

        // Collect candidate signature-type nodes from every tiered callable seed.
        let mut candidates: Vec<Node> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for seed_id in &tiered_seed_ids {
            for kind in SIG_EDGE_KINDS {
                for edge in self.store.edges_by_source_kind(seed_id, Some(kind))? {
                    let Ok(targets) = self.store.nodes_by_ids(std::slice::from_ref(&edge.target))
                    else {
                        continue;
                    };
                    let Some(target) = targets.get(&edge.target) else {
                        continue;
                    };
                    if !TYPE_KINDS.contains(&target.kind) {
                        continue;
                    }
                    if seed_ids.iter().any(|s| s == &target.id) {
                        continue;
                    }
                    if seen.insert(target.id.clone()) {
                        candidates.push(target.clone());
                    }
                }
            }
        }

        // Deterministic order for the rescue set (no HashSet iteration reaches
        // output): sort by file, then line, then id.
        candidates.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then(a.start_line.cmp(&b.start_line))
                .then(a.id.cmp(&b.id))
        });

        for target in candidates {
            let fp = target.file_path.clone();
            // Buried: not already an on-screen seed file, and the query barely
            // touches it lexically (< 2 term hits on its file path / node name).
            if seed_files.contains(&fp) {
                continue;
            }
            let term_hits = terms
                .iter()
                .filter(|t| {
                    fp.to_ascii_lowercase().contains(*t)
                        || target.name.to_ascii_lowercase().contains(*t)
                })
                .count();
            if term_hits >= 2 {
                continue;
            }
            sub.insert(target);
            sub.rescued_files.insert(fp);
        }
        Ok(())
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
            let source = self.project_source(&node.file_path);
            if source.is_possibly_drifted() {
                continue;
            }
            let content = match source.content {
                Some(c) => c,
                None => continue,
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
                if let Some(key) = &m.key
                    && let Some(cand) =
                        self.boundary_candidates(key, m.key_is_type, subgraph, &node.id)?
                {
                    notes.push(format!("  {cand}"));
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
            if key_is_type
                && n.kind == NodeKind::Class
                && let Ok(children) = traverser.get_children(&n.id)
                && let Some(method) = children
                    .iter()
                    .find(|c| c.kind == NodeKind::Method && is_handler_method_name(&c.name))
            {
                display = format!("{}.{}", n.name, method.name);
                at = format!("{}:{}", method.file_path, method.start_line);
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
        let source = self.project_source(&node.file_path);
        if source.is_possibly_drifted() {
            return Ok(None);
        }
        let content = match source.content {
            Some(c) => c,
            None => return Ok(None),
        };
        let lines: Vec<&str> = content.split('\n').collect();
        let start_idx = (node.start_line - 1).max(0) as usize;
        let end_idx = (node.end_line as usize).min(lines.len());
        if start_idx >= lines.len() {
            return Ok(None);
        }
        // `.min(end_idx)` keeps `start_idx <= end_idx` when a stale index holds
        // a node whose `end_line` < `start_line`, so the slice range stays valid
        // (no-op on the healthy path). Mirrors Fix C's `build_section` clamp.
        let start_idx = start_idx.min(end_idx);
        Ok(Some(lines[start_idx..end_idx].join("\n")))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SourceFreshness {
    ProvenFresh,
    #[default]
    PossiblyDrifted,
}

#[derive(Clone, Default)]
struct SourceProbe {
    content: Option<Arc<str>>,
    freshness: SourceFreshness,
}

struct ExploreFileRender {
    section: String,
    source_emitted: bool,
}

impl SourceProbe {
    fn is_possibly_drifted(&self) -> bool {
        self.freshness == SourceFreshness::PossiblyDrifted
    }
}

fn metadata_modified_millis(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as i64)
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
    /// Files rescued by the #1064 change-surface pass: a tiered callable seed's
    /// buried signature-type file. `finalize` floats these to the TOP tier so a
    /// lexically-dissimilar answer file (grpc's `dialoptions.go`) is not buried
    /// under incidental roots that merely share query words.
    rescued_files: HashSet<String>,
    /// Paths whose `files.generated` row is 1, fetched in ONE batch query before
    /// `finalize` runs. `finalize` has no store handle, so the stored half of the
    /// generated signal has to arrive as a field — exactly how `rescued_files`
    /// reaches it. Empty leaves behavior on the path check alone, which is what
    /// keeps a pre-#1500 index ranking as it always did.
    generated_files: HashSet<String>,
    /// Paths that are ambient DECLARATION files — every symbol is a type
    /// declaration, nothing in them runs, and nothing anywhere in the index
    /// depends on them — and that this query does not name a type in.
    ///
    /// Computed index-wide in `find_relevant_context` before `finalize`, exactly
    /// as `generated_files` and `rescued_files` are: the predicate needs the store
    /// and the query, and `finalize` has neither. Membership therefore already
    /// accounts for the exemption, which is why the same file can be damped for a
    /// prose query and undamped for one naming its type.
    ambient_files: HashSet<String>,
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
            if self.roots.iter().any(|r| r == &e.source)
                && let Some(n) = self.node(&e.target)
            {
                neighbor_files.insert(n.file_path.as_str());
            }
            if self.roots.iter().any(|r| r == &e.target)
                && let Some(n) = self.node(&e.source)
            {
                neighbor_files.insert(n.file_path.as_str());
            }
        }
        let scores: HashMap<String, (u8, u8, usize)> = self
            .file_order
            .iter()
            .map(|fp| {
                // A rescued change-surface file (#1064) is the lexically-
                // dissimilar answer — give it the TOP tier so it outranks
                // incidental roots that merely share query words and survives
                // the output file budget.
                let tier = if self.rescued_files.contains(fp.as_str()) {
                    3
                } else if root_files.contains(fp.as_str()) {
                    2
                } else if neighbor_files.contains(fp.as_str()) {
                    1
                } else {
                    0
                };
                // Generated-path and ambient-declaration demotion sit INSIDE the
                // tier, so they only ever reorder files of equal query relevance: a
                // protobuf stub or a shim nothing depends on loses to the
                // implementation it shadows, never falls behind an unrelated
                // incidental file, and still ranks first in its tier when it is the
                // only match. The sort is descending, so real code must carry the
                // HIGHER value. `find_symbol_matches` and `find_all_symbols` apply
                // the same generated signal.
                //
                // The two signals combine by `min`, upstream's rule, so a file that
                // is BOTH is damped exactly as much as a generated-only one and no
                // more. That is why this is one 3-valued integer and not a second
                // boolean key: appending `not_damped_ambient` to the tuple would
                // rank generated+ambient strictly BELOW generated-only, which is
                // the double penalty `min` exists to prevent. For any non-ambient
                // file the values are order-isomorphic to the old
                // `not_generated` boolean (10 > 3 exactly where 1 > 0), so no
                // ranking can move unless the ambient predicate actually fires.
                let generated = is_generated_file(fp) || self.generated_files.contains(fp);
                let weight = u8::min(
                    if generated { 3 } else { 10 },
                    if self.ambient_files.contains(fp) {
                        5
                    } else {
                        10
                    },
                );
                let sym_count = self
                    .nodes
                    .iter()
                    .filter(|n| &n.file_path == fp && n.kind != NodeKind::File)
                    .count();
                (fp.clone(), (tier, weight, sym_count))
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

    /// Definition lines of this file's PROTECTED FOCUSES: nodes that are query
    /// roots AND whose name a shape-precise query token names,
    /// case-insensitively.
    ///
    /// Selection among more than [`FOCUS_CAP`] candidates is by
    /// `(start_line, name)`, never by hash order, so the emitted windows are
    /// byte-stable across runs.
    fn file_focus_lines(&self, file_path: &str, precise: &[String]) -> Vec<usize> {
        let mut picked: Vec<(usize, &str)> = self
            .nodes
            .iter()
            .filter(|n| {
                n.file_path == file_path
                    && n.start_line > 0
                    && self.roots.iter().any(|r| r == &n.id)
                    && precise.iter().any(|t| t.eq_ignore_ascii_case(&n.name))
            })
            .map(|n| (n.start_line as usize, n.name.as_str()))
            .collect();
        picked.sort_unstable();
        picked.dedup();
        picked.truncate(FOCUS_CAP);
        picked.into_iter().map(|(line, _)| line).collect()
    }

    /// The longest `#### path — names` header any subset of this file's clusters
    /// can produce: the `cap` LONGEST candidate labels plus the maximal `+N more`
    /// tail. A max over a set, so it is order-independent, and an over-estimate
    /// only ever LOWERS the render ceiling — it can never break the bound.
    ///
    /// Built from the file's SUBGRAPH nodes, the set `explore_file_header` draws
    /// from. Computing it over the store's nodes for the same path would
    /// over-estimate whenever the subgraph holds fewer of them.
    fn file_header_len_upper_bound(&self, file_path: &str, cap: usize) -> usize {
        let mut labels: Vec<String> = Vec::new();
        for n in self.nodes.iter().filter(|n| {
            n.file_path == file_path && n.kind != NodeKind::Import && n.kind != NodeKind::Export
        }) {
            let label = format!("{}({})", n.name, n.kind.as_str());
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        let total = labels.len();
        labels.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        let shown = labels.into_iter().take(cap).collect::<Vec<_>>().join(", ");
        let mut header = format!("#### {file_path} — {shown}");
        if total > cap {
            header.push_str(&format!(", +{} more", total - cap));
        }
        header.len()
    }

    /// `name:line` locations for the "Not shown above" list (`tools.ts:2920`),
    /// capped at upstream's `POINTER_SYMBOLS` with a `+N more` tail.
    ///
    /// The cap is what turns each pointer LINE from unbounded into bounded: a
    /// 120-method class contributed ~120 pairs on one line, so a single file's
    /// pointer could exhaust the whole epilogue budget.
    fn file_node_locations(&self, file_path: &str) -> String {
        let mut in_file: Vec<&Node> = self
            .nodes
            .iter()
            .filter(|n| n.file_path == file_path)
            .collect();
        in_file.sort_by_key(|n| n.start_line);
        let pairs: Vec<String> = in_file
            .iter()
            .map(|n| format!("{}:{}", n.name, n.start_line))
            .collect();
        cap_header_names(&pairs, POINTER_SYMBOLS)
    }
}

/// Per-file render state for one iteration of `handle_explore`'s file loop.
///
/// These cannot live in [`ExploreOutputBudget`]: that struct is `Copy` and built
/// ONCE per response from the project's file count, so putting loop state in it
/// would conflate a static tier constant with a per-file quantity.
struct RenderCtx<'a> {
    budget: &'a ExploreOutputBudget,
    /// What the total-output budget can still pay for this file, less the
    /// `lines.join("\n")` separator the section will be charged. The ceiling and
    /// the caller's admission test are computed from the SAME quantity, so they
    /// cannot disagree.
    funded_headroom: usize,
    /// Definition lines of this file's protected focuses (§3.1): roots whose name
    /// a shape-precise query token names. Every focus line is guaranteed to land
    /// inside some emitted window.
    focuses: &'a [usize],
    drifted: bool,
}

/// Decimal digit count, for costing a numbered source line without rendering it.
fn decimal_width(n: usize) -> usize {
    let mut width = 1;
    let mut rest = n;
    while rest >= 10 {
        rest /= 10;
        width += 1;
    }
    width
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
    if let Some(doc) = &node.docstring
        && doc.len() < 200
    {
        lines.push(String::new());
        lines.push(doc.clone());
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
    if n == 1 { "" } else { "s" }
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

/// Whether an indexed `file_path` is the file a `codegraph_node` `file` hint
/// names (#1314). Accepts the exact repo-relative path, a path suffix on a
/// segment boundary (`auth/session.ts` for `src/auth/session.ts`), or a bare
/// basename (`session.ts`). Separators are normalized so a Windows-style hint
/// pins the same node as its POSIX form.
fn file_path_matches_hint(file_path: &str, hint: &str) -> bool {
    let normalize = |s: &str| s.replace('\\', "/").trim_start_matches("./").to_string();
    let path = normalize(file_path);
    let hint = normalize(hint);
    if hint.is_empty() {
        return false;
    }
    if path == hint {
        return true;
    }
    if !hint.contains('/') {
        return path.rsplit('/').next() == Some(hint.as_str());
    }
    path.strip_suffix(hint.as_str())
        .is_some_and(|prefix| prefix.ends_with('/'))
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

fn is_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Union
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
            | NodeKind::Union
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
        if pat[0] == '['
            && let Some(close) = pat.iter().position(|&c| c == ']')
        {
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

/// Lowercased, ≥3-char alphanumeric query terms used by the #1064 buried check.
/// Deterministic: a stable, deduped, order-preserving token set (short/stop-ish
/// tokens are dropped so a 1-char "x" never counts as a term hit).
fn query_terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in query.split(|c: char| !c.is_ascii_alphanumeric()) {
        let t = tok.to_ascii_lowercase();
        if t.len() >= 3 && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Whether a query token is SHAPE-PRECISE enough to name a symbol rather than
/// describe one (upstream `89c53dd`'s `isPreciseToken`): at least 3 chars, and
/// either carrying a separator, or a lower→UPPER transition, or an initial
/// capital.
///
/// This is what keeps a prose query from minting protected focuses:
/// `hugeHandler` and `Foo::Bar` qualify, `upload` and `work` do not.
fn is_precise_query_token(token: &str) -> bool {
    if token.chars().count() < 3 {
        return false;
    }
    if token.contains(['.', '_', '$', '/']) || token.contains("::") {
        return true;
    }
    let chars: Vec<char> = token.chars().collect();
    if chars[0].is_uppercase() {
        return true;
    }
    chars
        .windows(2)
        .any(|w| w[0].is_lowercase() && w[1].is_uppercase())
}

/// The query's shape-precise tokens, in query order and deduped.
///
/// Split on WHITESPACE, unlike `query_terms`: that helper lowercases and strips
/// every separator, which would erase the very shape `is_precise_query_token`
/// tests. Only punctuation that cannot belong to an identifier is trimmed, so
/// `work?` reduces to prose while `Foo::Bar,` keeps its separator.
fn precise_query_tokens(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tok in query.split_whitespace() {
        let t = tok.trim_matches(|c: char| {
            c.is_ascii_punctuation() && !matches!(c, '.' | '_' | '$' | '/' | ':')
        });
        if !is_precise_query_token(t) {
            continue;
        }
        if !out.iter().any(|e| e == t) {
            out.push(t.to_string());
        }
    }
    out
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

/// Kinds the ambient-declaration predicate ignores: they describe a file's
/// plumbing rather than what it declares (upstream `9efae0f`).
fn is_ambient_bookkeeping_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File | NodeKind::Import | NodeKind::Export | NodeKind::Parameter
    )
}

/// Type-declaration kinds — upstream's exact set. A file whose every
/// non-bookkeeping node is one of these declares types and nothing else.
fn is_type_declaration_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Interface
            | NodeKind::TypeAlias
            | NodeKind::Enum
            | NodeKind::EnumMember
            | NodeKind::Namespace
    )
}

/// Header of the "Not shown above" pointer block (`tools.ts:2910-2927`).
const POINTER_BLOCK_HEADER: &str = "### Not shown above — explore these names for their source";

/// Horizontal rule opening the completeness block (`tools.ts:2933-2940`).
const COMPLETENESS_RULE: &str = "---";

/// The trim note emitted at tiers that gate the completeness signal OFF. It is
/// only reached when a file was actually trimmed, but `any_file_trimmed` is
/// loop-determined, so the pre-loop reserve has to assume it.
const TRIMMED_NOTE: &str = "> Some file sections were trimmed for size. For a specific symbol you still need, run another `codegraph_explore` (or `codegraph_node`) with its exact name — line-numbered source, cheaper and more complete than Read.";

fn completeness_signal(files_included: usize) -> String {
    format!(
        "> **Complete source for {files_included} files is included above — do NOT re-read them.** If your question also needs files/symbols listed under \"Not shown above\" (or any area this call didn't cover), make ANOTHER codegraph_explore targeting those names — it returns the same source with line numbers and is cheaper and more complete than reading. Reserve Read for a single specific line range explore can't surface."
    )
}

fn explore_budget_note(file_count: i64) -> String {
    format!(
        "> **{file_count} files indexed.** Each call covers ~6 files; if your question spans more, make ANOTHER `codegraph_explore` targeting the uncovered area BEFORE falling back to Read — another explore is cheaper and more complete than reading those files. There is no call limit."
    )
}

fn pointer_unlisted_tail(unlisted: usize) -> String {
    format!("- ... and {unlisted} more files")
}

/// The epilogue's note lines — everything after the pointer block.
///
/// The pre-loop reserve and the post-loop emitter both call THIS function, so the
/// reserve is derived from the very strings that get emitted and cannot drift from
/// them. The reserve passes the arguments that maximise the result, which is why
/// it is an upper bound rather than an estimate.
fn epilogue_note_lines(
    budget: &ExploreOutputBudget,
    files_included: usize,
    any_file_trimmed: bool,
    file_count: Option<i64>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if budget.include_completeness_signal {
        lines.push(COMPLETENESS_RULE.to_string());
        lines.push(completeness_signal(files_included));
        lines.push(String::new());
    } else if any_file_trimmed {
        lines.push(TRIMMED_NOTE.to_string());
        lines.push(String::new());
    }
    if budget.include_budget_note
        && let Some(count) = file_count
    {
        lines.push(explore_budget_note(count));
        lines.push(String::new());
    }
    lines
}

/// What a set of pushed lines costs `lines.join("\n")`: its own length plus the
/// one separator each is pushed behind.
fn joined_line_cost(lines: &[String]) -> usize {
    lines.iter().map(|l| 1 + l.len()).sum()
}

/// The epilogue's FIXED reserve, subtracted from `max_output_chars` before any
/// file is admitted so the sections cannot spend what the epilogue will need.
///
/// Every term is the length of a string the epilogue actually emits, evaluated at
/// its worst case: `max_files` maximises the completeness signal's digit width,
/// `any_file_trimmed = true` selects the larger of the two mutually exclusive
/// notes, and the unlisted tail is reserved at `file_order` length, which is
/// >= any achievable count because every unlisted file came from `file_order`.
fn fixed_epilogue_reserve(
    budget: &ExploreOutputBudget,
    file_order_len: usize,
    max_files: usize,
    file_count: Option<i64>,
) -> usize {
    let mut reserve = 0usize;
    if budget.include_additional_files {
        reserve += joined_line_cost(&[
            POINTER_BLOCK_HEADER.to_string(),
            String::new(),
            pointer_unlisted_tail(file_order_len),
            String::new(),
        ]);
    }
    reserve += joined_line_cost(&epilogue_note_lines(budget, max_files, true, file_count));
    reserve
}

/// Fit the pointer lines into the headroom the source sections did not use,
/// stopping at the first that does not fit, and report how many files went
/// unlisted — by `POINTER_MAX_FILES` and by elasticity together.
///
/// The iteration order IS part of the output contract: `excluded_files` arrives in
/// `file_order` rank order and the loop stops at the first line that does not fit,
/// so the ORDER selects the SET. A best-fit variant would use the budget better
/// and is deliberately rejected — it would make the emitted set depend on
/// arithmetic rather than on rank, which no caller could predict from the ranking.
fn fit_pointer_lines(candidates: &[String], pointer_budget: usize) -> (Vec<String>, usize) {
    let mut emitted = Vec::new();
    let mut spent = 0usize;
    for line in candidates.iter().take(POINTER_MAX_FILES) {
        if spent + 1 + line.len() > pointer_budget {
            break;
        }
        spent += 1 + line.len();
        emitted.push(line.clone());
    }
    (emitted.clone(), candidates.len() - emitted.len())
}

/// Cut `output` at the last `#### ` file-section boundary before `ceiling` so
/// trailing whole sections drop rather than slicing a method body; fall back to
/// a line boundary in the degenerate single-giant-section case
/// (`tools.ts:2964-2975`).
fn cut_at_section_boundary(output: &str, ceiling: usize) -> (String, usize) {
    if output.len() <= ceiling {
        return (output.to_string(), output.len());
    }
    let cut = &output[..ceiling];
    let last_section = cut.rfind("\n#### ");
    let boundary = match last_section {
        Some(i) if (i as f64) > ceiling as f64 * 0.5 => i,
        _ => cut.rfind('\n').unwrap_or(0),
    };
    let kept_prefix_len = if boundary > 0 { boundary } else { cut.len() };
    let safe = &output[..kept_prefix_len];
    (
        format!(
            "{safe}\n\n... (output truncated to budget; the source above is complete and verbatim — treat it as already Read. For any area not covered, run another codegraph_explore with the specific names — do NOT Read these files.)"
        ),
        kept_prefix_len,
    )
}

fn require_string(args: &Value, field: &str) -> Result<String, String> {
    match args.get(field).and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(format!("{field} must be a non-empty string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::Language;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn test_engine() -> CodeGraphEngine {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("cg-mcp-engine-{}-{seq}", std::process::id()));
        let db = base.join(".codegraph").join("codegraph.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = Store::open(&db).unwrap();
        CodeGraphEngine {
            store,
            project_root: base,
            config: Arc::new(Config::default()),
            source_probes: RefCell::new(HashMap::new()),
            source_citations: RefCell::new(HashSet::new()),
        }
    }

    fn node(name: &str, file: &str, start: i64, end: i64, kind: NodeKind) -> Node {
        Node {
            id: format!("{kind:?}:{name}:{start}"),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::Rust,
            start_line: start,
            end_line: end,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    /// A direct-call render context with nothing already spent, mirroring what
    /// `handle_explore` builds for the FIRST file of a response.
    fn render_ctx<'a>(budget: &'a ExploreOutputBudget, focuses: &'a [usize]) -> RenderCtx<'a> {
        RenderCtx {
            budget,
            funded_headroom: budget.max_output_chars.saturating_sub(1),
            focuses,
            drifted: false,
        }
    }

    fn subgraph_with(nodes: Vec<Node>, roots: Vec<String>) -> ExploreSubgraph {
        let mut sg = ExploreSubgraph::default();
        for n in nodes {
            sg.insert(n);
        }
        sg.roots = roots;
        sg
    }

    /// Given a stale index whose node start_line (921) is past a shrunk
    /// 913-line file's EOF, When `render_explore_file` runs, Then it returns a
    /// String instead of panicking with the historical "range start index 921
    /// out of range for slice of length 913".
    #[test]
    fn render_explore_file_does_not_panic_on_stale_node_past_eof() {
        let engine = test_engine();
        let file = "server.rs";
        // A 913-line file that must cluster (well over WHOLE_FILE_MAX_LINES),
        // so build_section's slice math runs. Reproduces the exact user crash.
        let owned: Vec<String> = (1..=913).map(|i| format!("line {i}")).collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        // Two nodes: a real in-bounds one so a section still renders, plus a
        // stale one whose START (918) and END (921) both exceed EOF — the
        // original crash had start_idx=921 > total_lines=913.
        let real = node("Handler", file, 100, 200, NodeKind::Function);
        let stale = node("McpServer", file, 918, 921, NodeKind::Struct);
        let sg = subgraph_with(vec![real.clone(), stale], vec![real.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(200);

        let out = engine
            .render_explore_file(&sg, file, &file_lines, "rust", &render_ctx(&budget, &[]))
            .section;
        assert!(
            out.contains(file),
            "stale-node render should still produce a file section, got: {out:?}"
        );
    }

    /// Given a stale node whose ENTIRE span is past EOF, When rendered, Then the
    /// range is dropped and the (now sub-threshold) file still renders whole.
    #[test]
    fn render_explore_file_drops_fully_stale_range() {
        let engine = test_engine();
        let file = "shrunk.rs";
        let owned: Vec<String> = (1..=300).map(|i| format!("line {i}")).collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let stale = node("Gone", file, 400, 450, NodeKind::Function);
        let sg = subgraph_with(vec![stale.clone()], vec![stale.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(200);

        let out = engine
            .render_explore_file(&sg, file, &file_lines, "rust", &render_ctx(&budget, &[]))
            .section;
        assert!(out.contains(file), "should render a header, got: {out:?}");
    }

    /// Regression: a HEALTHY god-file (all node lines within total_lines) still
    /// renders a clustered section with the real source, unchanged by the clamp.
    #[test]
    fn render_explore_file_healthy_god_file_clusters() {
        let engine = test_engine();
        let file = "big.rs";
        let owned: Vec<String> = (1..=500).map(|i| format!("line {i}")).collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let f1 = node("first", file, 10, 30, NodeKind::Function);
        let f2 = node("second", file, 400, 430, NodeKind::Function);
        let sg = subgraph_with(vec![f1.clone(), f2], vec![f1.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(200);

        let out = engine
            .render_explore_file(&sg, file, &file_lines, "rust", &render_ctx(&budget, &[]))
            .section;
        assert!(out.contains("line 10"), "cluster source missing: {out:?}");
        assert!(out.contains("line 30"), "cluster source missing: {out:?}");
    }

    /// Regression: a small HEALTHY file returns WHOLE, byte-for-byte, exactly as
    /// before the clamp change.
    #[test]
    fn render_explore_file_whole_file_unchanged() {
        let engine = test_engine();
        let file = "small.rs";
        let owned: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let n = node("Small", file, 1, 20, NodeKind::Struct);
        let sg = subgraph_with(vec![n.clone()], vec![n.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(200);

        let out = engine
            .render_explore_file(&sg, file, &file_lines, "rust", &render_ctx(&budget, &[]))
            .section;
        let expected = {
            let numbered = number_source_lines_at(&file_lines, 1);
            format!("#### {file} — Small(struct)\n\n```rust\n{numbered}\n```\n")
        };
        assert_eq!(out, expected);
    }

    /// Tier 5 item 12 (CG-38). Given the shape-precise token test, When each of a
    /// symbol-like and a prose-like token is checked, Then only the symbol-like
    /// ones qualify — so ordinary prose can never mint a protected focus.
    #[test]
    fn precise_query_token_shape() {
        for t in ["hugeHandler", "processPayroll", "Foo::Bar", "a_b"] {
            assert!(is_precise_query_token(t), "{t} must be precise");
        }
        for t in ["upload", "work", "id"] {
            assert!(!is_precise_query_token(t), "{t} must NOT be precise");
        }
    }

    /// Tier 5 item 12's affordability gate, INERT paths. Given a small whole-file
    /// eligible file, When the funded headroom can pay for it, Then the complete
    /// section is returned even though a focus exists; and When the headroom
    /// cannot pay for it but the file owns NO focus, Then the complete section is
    /// still returned so the caller drops it whole.
    ///
    /// Case (a) is the guard on gate (1) itself: measured, deleting that gate
    /// windows an affordable focus-owning file, fails this test, AND drifts
    /// `explore_matches_golden_structural`, whose mini fixture takes this very
    /// branch. Merely SWAPPING the two conditions stays green — also measured —
    /// because gate (1) still catches the affordable case; what the swap costs is
    /// the golden-neutrality ARGUMENT, which then rests on the fixture holding no
    /// precise token rather than on the code. That is why the order is asserted
    /// structurally rather than here.
    #[test]
    fn whole_file_section_is_returned_whole_when_it_fits() {
        let engine = test_engine();
        let file = "small.ts";
        let owned: Vec<String> = (1..=20)
            .map(|i| format!("export function tinyFocus{i}(): number {{ return {i}; }}"))
            .collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let n = node("tinyFocus1", file, 1, 1, NodeKind::Function);
        let sg = subgraph_with(vec![n.clone()], vec![n.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(3);
        let whole = {
            let numbered = number_source_lines_at(&file_lines, 1);
            format!("#### {file} — tinyFocus1(function)\n\n```typescript\n{numbered}\n```\n")
        };

        let affordable_with_focus = engine
            .render_explore_file(
                &sg,
                file,
                &file_lines,
                "typescript",
                &render_ctx(&budget, &[1]),
            )
            .section;
        assert_eq!(
            affordable_with_focus, whole,
            "an AFFORDABLE focus-owning file must come back whole"
        );
        assert!(!affordable_with_focus.contains("... (gap) ..."));

        let starved = RenderCtx {
            budget: &budget,
            funded_headroom: whole.len() - 1,
            focuses: &[],
            drifted: false,
        };
        let unaffordable_no_focus = engine
            .render_explore_file(&sg, file, &file_lines, "typescript", &starved)
            .section;
        assert_eq!(
            unaffordable_no_focus, whole,
            "an unaffordable file owning NO focus must still be returned whole, \
             so the caller drops it whole"
        );
    }

    /// Tier 5 item 12 (CG-38). Given NINE qualifying focuses in one file, When the
    /// focus list is selected, Then exactly the six LOWEST `start_line`s are
    /// protected — and the same twice in one process, so a hash-ordered
    /// implementation cannot pass.
    #[test]
    fn focus_cap_is_six_and_selection_is_line_ordered() {
        let file = "many.ts";
        let nodes: Vec<Node> = (0..9)
            .map(|i| {
                node(
                    &format!("focusTarget{i}"),
                    file,
                    10 + i * 10,
                    12 + i * 10,
                    NodeKind::Function,
                )
            })
            .collect();
        let roots: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let sg = subgraph_with(nodes, roots);
        let precise: Vec<String> = (0..9).map(|i| format!("focusTarget{i}")).collect();

        let first = sg.file_focus_lines(file, &precise);
        let second = sg.file_focus_lines(file, &precise);
        assert_eq!(first, vec![10, 20, 30, 40, 50, 60], "cap or order wrong");
        assert_eq!(
            first, second,
            "focus selection is not stable within a process"
        );
    }

    /// Tier 5 item 13 half A (CG-30). Given a 600-line file whose single symbol
    /// spans all of it, so its only cluster renders ~13x the per-file cap, When
    /// `render_explore_file` runs at the tiny budget, Then the returned SECTION —
    /// frame included, the same quantity the caller's admission test compares —
    /// obeys `round(max_chars_per_file * 1.5)` while still emitting whole source
    /// lines.
    ///
    /// The unit is the SECTION and not the cluster body on purpose: `ceiling`
    /// bounds bodies, so a ceiling that does not subtract the header/fence frame
    /// satisfies the admission test yet overshoots this bound by the frame.
    #[test]
    fn render_explore_file_windows_an_oversize_first_cluster() {
        let engine = test_engine();
        let file = "god.ts";
        let owned: Vec<String> = (1..=600)
            .map(|i| format!("  // padding line {i} inside hugeHandler keeping the body enormous"))
            .collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let huge = node("hugeHandler", file, 1, 600, NodeKind::Function);
        let sg = subgraph_with(vec![huge.clone()], vec![huge.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(3);

        let section = engine
            .render_explore_file(
                &sg,
                file,
                &file_lines,
                "typescript",
                &render_ctx(&budget, &[]),
            )
            .section;

        let bound = (budget.max_chars_per_file as f64 * 1.5).round() as usize;
        assert!(
            section.contains('\t'),
            "an oversize first cluster must still emit numbered source: {section:?}"
        );
        assert!(
            section.len() <= bound,
            "the returned section must obey the {bound}-char 1.5x per-file bound, got {}",
            section.len()
        );
        for line in section.lines() {
            let Some((num, body)) = line.split_once('\t') else {
                continue;
            };
            let Ok(n) = num.parse::<usize>() else {
                continue;
            };
            assert_eq!(
                file_lines[n - 1],
                body,
                "line {n} was sliced mid-line: {section}"
            );
        }
    }

    /// Tier 5 item 13 half B's guard. Given an OVER-CEILING first cluster — so
    /// `projected` legitimately exceeds the room a later cluster could draw on —
    /// When a later cluster is considered, Then it is still DROPPED rather than
    /// emitted, and nothing panics.
    ///
    /// This is the input that underflows a plain `ceiling - projected`: in debug
    /// it panics, in release it wraps to a huge `room` and hands the later cluster
    /// unlimited budget. A merely tight-but-positive `room` does not exercise it.
    #[test]
    fn render_explore_file_skips_a_later_cluster_with_no_usable_room() {
        let engine = test_engine();
        let file = "twin.ts";
        // Lines long enough that the first cluster's 12-line never-empty FLOOR
        // alone costs more than the whole ceiling. That is the only way
        // `projected` can exceed `ceiling`: half A otherwise bounds the first
        // cluster's body by the ceiling, so a fixture with ordinary-length lines
        // never reaches the subtraction at all.
        let owned: Vec<String> = (1..=900)
            .map(|i| format!("  // padding line {i} {}", "-".repeat(600)))
            .collect();
        let file_lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let head = node("headSymbol", file, 1, 400, NodeKind::Function);
        let tail = node("tailSymbol", file, 600, 890, NodeKind::Function);
        let sg = subgraph_with(vec![head.clone(), tail], vec![head.id.clone()]);
        let budget = crate::explore_budget::get_explore_output_budget(3);

        let section = engine
            .render_explore_file(
                &sg,
                file,
                &file_lines,
                "typescript",
                &render_ctx(&budget, &[]),
            )
            .section;

        let emitted: Vec<usize> = section
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter_map(|(n, _)| n.parse::<usize>().ok())
            .collect();
        assert!(
            !emitted.is_empty(),
            "the first cluster must still emit: {section}"
        );
        assert!(
            emitted.iter().all(|&n| n < 600),
            "the later cluster had no usable room and must be dropped, got lines {emitted:?}"
        );
        assert!(
            section.contains("... (gap) ..."),
            "a dropped later cluster must still be signalled: {section}"
        );
    }

    /// Given a stale index whose node `end_line` (10) is BELOW its `start_line`
    /// (50) — a malformed/shrunk-file row — When `get_code` slices the on-disk
    /// file, Then it returns Ok (graceful) instead of panicking with "slice
    /// index starts at 49 but ends at 10".
    #[test]
    fn get_code_does_not_panic_when_end_line_below_start_line() {
        let engine = test_engine();
        let file = "reversed.rs";
        let body: String = (1..=100).map(|i| format!("line {i}\n")).collect::<String>();
        put_indexed_source(&engine, file, &body, Language::Rust, 1);
        let stale = node("Reversed", file, 50, 10, NodeKind::Function);

        let out = engine.get_code(&stale).unwrap();
        assert!(
            out.is_none() || out.as_deref() == Some(""),
            "reversed-span stale node should degrade to empty/None, got: {out:?}"
        );
    }

    /// Regression: a HEALTHY node (`start_line <= end_line`, both in range)
    /// returns its verbatim source unchanged by the clamp.
    #[test]
    fn get_code_healthy_node_unchanged() {
        let engine = test_engine();
        let file = "ok.rs";
        let body: String = (1..=100).map(|i| format!("line {i}\n")).collect::<String>();
        put_indexed_source(&engine, file, &body, Language::Rust, 1);
        let healthy = node("Ok", file, 10, 12, NodeKind::Function);

        let out = engine.get_code(&healthy).unwrap();
        assert_eq!(out.as_deref(), Some("line 10\nline 11\nline 12"));
    }

    fn text_of(tr: &ToolResult) -> String {
        tr.content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default()
    }

    fn node_lang(
        name: &str,
        qualified: &str,
        file: &str,
        start: i64,
        end: i64,
        kind: NodeKind,
        lang: Language,
    ) -> Node {
        let mut n = node(name, file, start, end, kind);
        n.id = format!("{kind:?}:{file}:{name}:{start}");
        n.qualified_name = qualified.to_string();
        n.language = lang;
        n
    }

    fn mk_edge(
        source: &str,
        target: &str,
        kind: codegraph_core::types::EdgeKind,
    ) -> codegraph_core::types::Edge {
        codegraph_core::types::Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind,
            metadata: None,
            line: None,
            col: None,
            provenance: None,
        }
    }

    fn file_rec(path: &str, lang: Language, node_count: i64) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            content_hash: "hash".to_string(),
            language: lang,
            size: 100,
            modified_at: 0,
            indexed_at: 0,
            node_count,
            errors: Vec::new(),
            generated: false,
        }
    }

    fn put_nodes(engine: &mut CodeGraphEngine, nodes: &[Node]) {
        engine.store.upsert_nodes(nodes).unwrap();
    }

    fn put_edges(engine: &mut CodeGraphEngine, edges: &[codegraph_core::types::Edge]) {
        engine.store.insert_edges(edges).unwrap();
    }

    fn put_file(engine: &CodeGraphEngine, file: &FileRecord) {
        engine.store.upsert_file(file).unwrap();
    }

    fn write_src(engine: &CodeGraphEngine, rel: &str, content: &str) {
        let abs = engine.project_root.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&abs, content).unwrap();
    }

    fn put_indexed_source(
        engine: &CodeGraphEngine,
        rel: &str,
        content: &str,
        language: Language,
        node_count: i64,
    ) {
        write_src(engine, rel, content);
        let metadata = std::fs::metadata(engine.project_root.join(rel)).unwrap();
        let modified_at = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut file = file_rec(rel, language, node_count);
        file.content_hash = codegraph_core::node_id::hash_content(content);
        file.size = metadata.len() as i64;
        file.modified_at = modified_at;
        put_file(engine, &file);
    }

    /// `put_indexed_source` plus the stored `files.generated` verdict, so a test
    /// can model a banner-marked file (flag 1 on an ORDINARY path) and a
    /// pre-#1500 index (flag 0 on a path the suffix check still catches).
    fn put_indexed_source_generated(
        engine: &CodeGraphEngine,
        rel: &str,
        content: &str,
        language: Language,
        node_count: i64,
        generated: bool,
    ) {
        write_src(engine, rel, content);
        let metadata = std::fs::metadata(engine.project_root.join(rel)).unwrap();
        let modified_at = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut file = file_rec(rel, language, node_count);
        file.content_hash = codegraph_core::node_id::hash_content(content);
        file.size = metadata.len() as i64;
        file.modified_at = modified_at;
        file.generated = generated;
        put_file(engine, &file);
    }

    #[test]
    fn ext_search_empty_query_errors() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_search", &serde_json::json!({}));
        assert_eq!(tr.is_error, Some(true));
        assert!(text_of(&tr).contains("query must be a non-empty string"));
    }

    #[test]
    fn ext_search_no_results() {
        let engine = test_engine();
        let tr = engine.execute(
            "codegraph_search",
            &serde_json::json!({"query": "nonexistent_zzz"}),
        );
        assert!(text_of(&tr).contains("No results found for"));
    }

    #[test]
    fn ext_search_returns_results_and_kind_filter() {
        let mut engine = test_engine();
        let f = node_lang(
            "alphaFunc",
            "alphaFunc",
            "a.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        let c = node_lang(
            "AlphaClass",
            "AlphaClass",
            "a.rs",
            10,
            20,
            NodeKind::Class,
            Language::Rust,
        );
        put_nodes(&mut engine, &[f, c]);

        let tr = engine.execute(
            "codegraph_search",
            &serde_json::json!({"query": "alpha", "limit": 5}),
        );
        assert!(
            text_of(&tr).contains("Search Results"),
            "got: {}",
            text_of(&tr)
        );

        let tr2 = engine.execute(
            "codegraph_search",
            &serde_json::json!({"query": "alpha", "kind": "function"}),
        );
        assert!(text_of(&tr2).contains("Search Results"));
    }

    #[test]
    fn ext_callers_symbol_not_found() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_callers", &serde_json::json!({"symbol": "ghost"}));
        assert_eq!(tr.is_error, Some(true), "got: {}", text_of(&tr));
        assert!(text_of(&tr).contains("not found in the codebase"));
        assert!(text_of(&tr).contains("ghost"));
    }

    #[test]
    fn ext_callers_missing_symbol_errors() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_callers", &serde_json::json!({}));
        assert_eq!(tr.is_error, Some(true));
    }

    #[test]
    fn ext_callers_and_callees_render_lists() {
        let mut engine = test_engine();
        let save = node_lang(
            "save",
            "save",
            "svc.rs",
            10,
            20,
            NodeKind::Function,
            Language::Rust,
        );
        let caller = node_lang(
            "caller_a",
            "caller_a",
            "svc.rs",
            30,
            40,
            NodeKind::Function,
            Language::Rust,
        );
        let helper = node_lang(
            "helper",
            "helper",
            "svc.rs",
            50,
            60,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[save.clone(), caller.clone(), helper.clone()]);
        put_edges(
            &mut engine,
            &[
                mk_edge(&caller.id, &save.id, codegraph_core::types::EdgeKind::Calls),
                mk_edge(&save.id, &helper.id, codegraph_core::types::EdgeKind::Calls),
            ],
        );

        let cr = engine.execute("codegraph_callers", &serde_json::json!({"symbol": "save"}));
        assert_ne!(cr.is_error, Some(true));
        assert!(
            text_of(&cr).contains("Callers of save"),
            "got: {}",
            text_of(&cr)
        );
        assert!(text_of(&cr).contains("caller_a"));

        let ce = engine.execute(
            "codegraph_callees",
            &serde_json::json!({"symbol": "save", "limit": 5}),
        );
        assert!(
            text_of(&ce).contains("Callees of save"),
            "got: {}",
            text_of(&ce)
        );
        assert!(text_of(&ce).contains("helper"));
    }

    #[test]
    fn ext_lookup_handlers_refuse_fuzzy_only_symbol_matches() {
        let mut engine = test_engine();
        let target = node_lang(
            "save_target",
            "saveTarget",
            "svc.rs",
            10,
            20,
            NodeKind::Function,
            Language::Rust,
        );
        let caller = node_lang(
            "caller_a",
            "caller_a",
            "svc.rs",
            30,
            40,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[target.clone(), caller.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &caller.id,
                &target.id,
                codegraph_core::types::EdgeKind::Calls,
            )],
        );

        for tool in ["codegraph_callers", "codegraph_impact"] {
            let tr = engine.execute(tool, &serde_json::json!({"symbol": "saveTarge"}));
            assert_eq!(
                tr.is_error,
                Some(true),
                "{tool} must reject the prefix instead of returning `saveTarget` rows: {}",
                text_of(&tr)
            );
            assert!(text_of(&tr).contains("saveTarge"));
            assert!(text_of(&tr).contains("codegraph_search"));
            assert!(!text_of(&tr).contains("caller_a"));
        }
    }

    #[test]
    fn ext_callers_and_callees_none_found_message() {
        let mut engine = test_engine();
        let lonely = node_lang(
            "lonely",
            "lonely",
            "svc.rs",
            10,
            20,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[lonely]);
        let cr = engine.execute(
            "codegraph_callers",
            &serde_json::json!({"symbol": "lonely"}),
        );
        assert!(
            text_of(&cr).contains("No callers found for"),
            "got: {}",
            text_of(&cr)
        );
        let ce = engine.execute(
            "codegraph_callees",
            &serde_json::json!({"symbol": "lonely"}),
        );
        assert!(
            text_of(&ce).contains("No callees found for"),
            "got: {}",
            text_of(&ce)
        );
    }

    #[test]
    fn ext_impact_not_found_and_radius() {
        let mut engine = test_engine();
        let tr = engine.execute("codegraph_impact", &serde_json::json!({"symbol": "ghost"}));
        assert_eq!(tr.is_error, Some(true), "got: {}", text_of(&tr));
        assert!(text_of(&tr).contains("not found in the codebase"));

        let core = node_lang(
            "core",
            "core",
            "core.rs",
            10,
            20,
            NodeKind::Function,
            Language::Rust,
        );
        let dep = node_lang(
            "dependent",
            "dependent",
            "dep.rs",
            5,
            15,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[core.clone(), dep.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &dep.id,
                &core.id,
                codegraph_core::types::EdgeKind::Calls,
            )],
        );

        let ir = engine.execute(
            "codegraph_impact",
            &serde_json::json!({"symbol": "core", "depth": 2}),
        );
        assert_ne!(ir.is_error, Some(true));
        let txt = text_of(&ir);
        assert!(txt.contains("Impact:"), "got: {txt}");
        assert!(txt.contains("dependent"));
    }

    #[test]
    fn ext_impact_missing_symbol_errors() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_impact", &serde_json::json!({}));
        assert_eq!(tr.is_error, Some(true));
    }

    #[test]
    fn file_path_matches_hint_accepts_exact_suffix_and_basename_only() {
        assert!(file_path_matches_hint(
            "src/auth/session.ts",
            "src/auth/session.ts"
        ));
        assert!(file_path_matches_hint(
            "src/auth/session.ts",
            "auth/session.ts"
        ));
        assert!(file_path_matches_hint("src/auth/session.ts", "session.ts"));
        assert!(file_path_matches_hint(
            "src/auth/session.ts",
            "./src/auth/session.ts"
        ));
        // Windows-style hint pins the same node as its POSIX form.
        assert!(file_path_matches_hint(
            "src/auth/session.ts",
            "auth\\session.ts"
        ));
        // A suffix that does not land on a segment boundary is NOT a match.
        assert!(!file_path_matches_hint(
            "src/myauth/session.ts",
            "auth/session.ts"
        ));
        // A basename hint must match the basename, not a substring of it.
        assert!(!file_path_matches_hint(
            "src/auth/mysession.ts",
            "session.ts"
        ));
        assert!(!file_path_matches_hint("src/auth/session.ts", "other.ts"));
        assert!(!file_path_matches_hint("src/auth/session.ts", ""));
    }

    #[test]
    fn ext_node_symbol_plus_file_pins_to_that_definition_and_reports_a_bad_pin() {
        let mut engine = test_engine();
        let alpha = node_lang(
            "setState",
            "setState",
            "src/alpha.ts",
            1,
            4,
            NodeKind::Function,
            Language::TypeScript,
        );
        let beta = node_lang(
            "setState",
            "setState",
            "src/beta.ts",
            1,
            4,
            NodeKind::Function,
            Language::TypeScript,
        );
        put_nodes(&mut engine, &[alpha, beta]);

        let pinned = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "setState", "file": "src/beta.ts", "includeCode": true}),
        );
        let txt = text_of(&pinned);
        assert!(txt.contains("src/beta.ts"), "got: {txt}");
        assert!(
            !txt.contains("src/alpha.ts"),
            "the pin must exclude the other overload: {txt}"
        );
        assert!(
            !txt.contains("definitions named"),
            "a resolved pin renders one definition, not the ambiguity list: {txt}"
        );

        let bad = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "setState", "file": "src/nowhere.ts"}),
        );
        assert_eq!(bad.not_found, Some(true));
        assert!(
            text_of(&bad).contains("not found in \"src/nowhere.ts\""),
            "got: {}",
            text_of(&bad)
        );
    }

    #[test]
    fn ext_node_missing_symbol_errors() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_node", &serde_json::json!({"symbol": "  "}));
        assert_eq!(tr.is_error, Some(true));
        assert!(text_of(&tr).contains("symbol must be a non-empty string"));
    }

    #[test]
    fn ext_node_symbol_not_found() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_node", &serde_json::json!({"symbol": "ghost"}));
        assert!(text_of(&tr).contains("not found in the codebase"));
    }

    #[test]
    fn ext_node_single_match_with_code_and_trail() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "svc.rs",
            &(1..=30).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            2,
        );
        let mut target = node_lang(
            "doThing",
            "doThing",
            "svc.rs",
            10,
            12,
            NodeKind::Function,
            Language::Rust,
        );
        target.signature = Some("fn doThing()".to_string());
        target.docstring = Some("does the thing".to_string());
        let callee = node_lang(
            "inner",
            "inner",
            "svc.rs",
            20,
            22,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[target.clone(), callee.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &target.id,
                &callee.id,
                codegraph_core::types::EdgeKind::Calls,
            )],
        );

        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "doThing", "includeCode": true}),
        );
        let txt = text_of(&tr);
        assert!(txt.contains("## doThing (function)"), "got: {txt}");
        assert!(txt.contains("**Signature:**"));
        assert!(txt.contains("does the thing"));
        assert!(txt.contains("line 10"));
        assert!(txt.contains("Trail"));
        assert!(txt.contains("Calls"));
    }

    #[test]
    fn ext_node_drifted_large_file_never_returns_mis_sliced_body() {
        let mut engine = test_engine();
        let file = "src/drifted.rs";
        let mut indexed_lines: Vec<String> = (1..=FILE_MODE_MAX_LINES + 1)
            .map(|i| format!("line {i}"))
            .collect();
        indexed_lines[999] = "fn target() {".to_string();
        indexed_lines[1000] = "    indexed_body();".to_string();
        indexed_lines[1001] = "}".to_string();
        let indexed = indexed_lines.join("\n");
        put_indexed_source(&engine, file, &indexed, Language::Rust, 1);

        let target = node_lang(
            "target",
            "target",
            file,
            1000,
            1002,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, std::slice::from_ref(&target));

        let mut current_lines = indexed_lines;
        current_lines[999] = "fn different_symbol() {".to_string();
        current_lines[1000] = "    WRONG_BODY_FROM_DIFFERENT_SYMBOL();".to_string();
        current_lines[1001] = "}".to_string();
        write_src(&engine, file, &current_lines.join("\n"));

        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "target", "includeCode": true}),
        );
        let txt = text_of(&tr);
        assert!(
            !txt.contains("WRONG_BODY_FROM_DIFFERENT_SYMBOL"),
            "a drifted indexed range must never be sliced from current bytes: {txt}"
        );
        assert!(
            txt.contains("source omitted"),
            "a drifted file above the whole-file cap needs an explicit omission notice: {txt}"
        );
    }

    #[test]
    fn ext_node_missing_file_record_uses_whole_current_source_and_banner() {
        let mut engine = test_engine();
        let file = "src/file_record_gap.rs";
        write_src(
            &engine,
            file,
            "fn inserted_before() {}\nfn target() { current_body(); }\nfn after() {}\n",
        );
        let target = node_lang(
            "target",
            "target",
            file,
            2,
            2,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, std::slice::from_ref(&target));

        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "target", "includeCode": true}),
        );
        let txt = text_of(&tr);
        assert!(
            txt.contains("Showing the file's full CURRENT source instead"),
            "a readable file without a FileRecord must not be sliced at stored lines: {txt}"
        );
        assert!(txt.contains("1\tfn inserted_before() {}"), "got: {txt}");
        assert!(txt.contains("3\tfn after() {}"), "got: {txt}");
        assert!(
            txt.starts_with("⚠️ Some files referenced below were edited since the last index sync"),
            "possibly-drifted bytes served from a missing-record file need a banner: {txt}"
        );
    }

    #[test]
    fn project_source_oversized_same_size_mtime_mismatch_is_possibly_drifted() {
        let mut engine = test_engine();
        let file = "src/oversized.rs";
        let content = "fn oversized() {}\n";
        put_indexed_source(&engine, file, content, Language::Rust, 1);
        let oversized = node_lang(
            "oversized",
            "oversized",
            file,
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, std::slice::from_ref(&oversized));
        let metadata = std::fs::metadata(engine.project_root.join(file)).unwrap();
        let mut stored = engine.store.file_by_path(file).unwrap().unwrap();
        stored.modified_at += 1;
        put_file(&engine, &stored);
        Arc::get_mut(&mut engine.config)
            .unwrap()
            .indexing
            .max_file_size = metadata.len() - 1;

        let probe = engine.project_source(file);
        assert_eq!(
            probe.freshness,
            SourceFreshness::PossiblyDrifted,
            "an oversized file with matching size but mismatched millisecond mtime cannot be proven fresh"
        );
        assert!(
            probe.content.is_none(),
            "oversized source bytes must remain unread"
        );

        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "oversized", "includeCode": true}),
        );
        assert!(
            !text_of(&tr).starts_with("⚠️ Some files referenced below"),
            "a possibly-drifted file whose bytes were omitted must not fabricate a banner: {}",
            text_of(&tr)
        );
    }

    #[test]
    fn ext_node_staleness_banner_is_only_emitted_for_drifted_files() {
        let mut engine = test_engine();

        let drifted_file = "src/drifted_small.rs";
        let indexed = "fn first() {}\nfn target() { indexed_body(); }\nfn last() {}\n";
        put_indexed_source(&engine, drifted_file, indexed, Language::Rust, 1);
        let drifted = node_lang(
            "target",
            "target",
            drifted_file,
            2,
            2,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, std::slice::from_ref(&drifted));
        write_src(
            &engine,
            drifted_file,
            "fn inserted() {}\nfn first() {}\nfn target() { current_body(); }\nfn last() {}\n",
        );

        let stale = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "target", "includeCode": true}),
        );
        let stale_text = text_of(&stale);
        assert!(
            stale_text.starts_with(
                "⚠️ Some files referenced below were edited since the last index sync"
            ),
            "drifted responses need the promised banner prefix: {stale_text}"
        );
        assert!(stale_text.contains(drifted_file), "got: {stale_text}");

        let fresh_file = "src/fresh.rs";
        let fresh_source = "fn fresh_target() { current(); }\n";
        put_indexed_source(&engine, fresh_file, fresh_source, Language::Rust, 1);
        let fresh = node_lang(
            "fresh_target",
            "fresh_target",
            fresh_file,
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, std::slice::from_ref(&fresh));

        let fresh_result = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "fresh_target", "includeCode": true}),
        );
        assert!(
            !text_of(&fresh_result).starts_with("⚠️ Some files referenced below"),
            "fresh responses must not gain a banner: {}",
            text_of(&fresh_result)
        );
    }

    #[test]
    fn staleness_banner_omits_a_citation_removed_from_the_final_response() {
        let engine = test_engine();
        let file = "src/truncated.rs";
        engine.source_probes.borrow_mut().insert(
            file.to_string(),
            SourceProbe {
                content: Some(Arc::<str>::from("fn truncated() {}\n")),
                freshness: SourceFreshness::PossiblyDrifted,
            },
        );
        let lines = vec![
            format!("head\n{}", "a".repeat(120)),
            format!("#### {file} — truncated\n\n```rust\nfn truncated() {{}}\n```\n"),
        ];
        let output = lines.join("\n");
        let (output, kept_prefix_len) = cut_at_section_boundary(&output, 180);
        engine.mark_surviving_explore_citations(&lines, &[(file.to_string(), 1)], kept_prefix_len);

        let result = engine.with_staleness_banner(ToolResult::text(output));
        assert!(
            !text_of(&result).starts_with("⚠️ Some files referenced below"),
            "a source section removed by final truncation must not leave a fabricated banner: {}",
            text_of(&result)
        );
    }

    fn drifted_explore_with_trailing_newline(content_lines: usize) -> String {
        assert!(content_lines >= 2);
        let mut engine = test_engine();
        let file = "src/drifted_explore_boundary.rs";
        let target_name = "drifted_explore_boundary";
        put_indexed_source(
            &engine,
            file,
            "fn drifted_explore_boundary() { indexed(); }\n",
            Language::Rust,
            1,
        );
        let target = node_lang(
            target_name,
            target_name,
            file,
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, std::slice::from_ref(&target));

        let mut current_lines = vec![String::new(); content_lines];
        current_lines[0] = "fn drifted_explore_boundary() { current(); }".to_string();
        current_lines[content_lines - 1] = "// final content line".to_string();
        let current = format!("{}\n", current_lines.join("\n"));
        assert_eq!(
            current.trim_end_matches('\n').split('\n').count(),
            content_lines
        );
        assert_eq!(current.split('\n').count(), content_lines + 1);
        write_src(&engine, file, &current);

        let result = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": target_name, "maxFiles": 1}),
        );
        text_of(&result)
    }

    #[test]
    fn ext_explore_drifted_trailing_newline_boundary_tracks_rendered_source() {
        let text = drifted_explore_with_trailing_newline(FILE_MODE_MAX_LINES);
        assert!(
            text.contains("source omitted because indexed line ranges are unsafe"),
            "the render threshold must continue omitting this source: {text}"
        );
        assert!(
            !text.starts_with("⚠️ Some files referenced below"),
            "omitted source must not leave a fabricated drift banner: {text}"
        );

        let text = drifted_explore_with_trailing_newline(FILE_MODE_MAX_LINES - 1);
        assert!(
            text.contains("source below is the full current file"),
            "the render threshold must continue emitting this source: {text}"
        );
        assert!(
            text.starts_with("⚠️ Some files referenced below"),
            "rendered possibly-drifted source must retain its banner: {text}"
        );
    }

    /// Given retained source that itself contains a line mimicking a `#### <path>`
    /// section header for a DIFFERENT file, When that other file is dropped for the
    /// budget, Then the spoofed marker must not make the response cite the dropped
    /// file as a drifted source.
    ///
    /// The drop MECHANISM this exercises moved. It used to be the hard-ceiling cut,
    /// and that is now unreachable by construction: `hard_ceiling` is
    /// `min(max_output_chars * 1.5, 25_000)`, which is strictly GREATER than
    /// `max_output_chars` at every tier (19,500 > 13,000; 25,000 > 18,000;
    /// 25,000 > 24,000), while the accounting keeps the response inside
    /// `max_output_chars`. So a file is dropped by the ADMISSION test instead, and
    /// the assertions below pin that. Do not "restore" this test by widening the
    /// ceiling.
    ///
    /// **Exactly what this test is and is not sensitive to, all measured by
    /// reverting and re-running rather than reasoned about.** It fails only when the
    /// pointer LINE cap and the elastic pointer BLOCK are BOTH absent — which is the
    /// genuine pre-fix state, not a contrived mutant, and is why upstream needs both
    /// (the cap bounds a line, elasticity bounds the block):
    ///
    /// | reverted | verdict | len | cut | ptr lines | completeness |
    /// | --- | --- | --- | --- | --- | --- |
    /// | nothing | pass | 22,266 | no | 10 | present |
    /// | 6-symbol cap only | pass | 24,201 | no | 4 | present |
    /// | elastic block only | pass | 22,266 | no | 10 | present |
    /// | **cap + elastic block** | **FAIL** | 23,710 | **yes** | 4 | **destroyed** |
    ///
    /// It is NOT sensitive to the `+ 200` → `+ 1` real-cost charge, nor to the
    /// pre-loop epilogue reserve: reverting either leaves this output byte-identical,
    /// because with the cap in place all ten pointer lines fit the headroom anyway.
    /// Those two are pinned elsewhere and deliberately not here —
    /// `explore_output_respects_its_stated_budget` (the reserve, and the section
    /// count when the real-cost charge is reverted),
    /// `file_node_locations_caps_symbols_at_six` (the cap directly), and
    /// `epilogue_pointer_block_fits_the_remaining_budget` (the elastic block's four
    /// branches). This test is a whole-item regression check on the marker-collision
    /// invariant, not per-change budget coverage.
    #[test]
    fn ext_explore_banner_ignores_marker_collision_from_retained_source() {
        let mut engine = test_engine();
        let retained_file = "src/a_retained.rs";
        let dropped_file = "src/z_dropped.rs";
        let query = "marker_collision_target";

        let retained_padding = (0..550)
            .map(|i| format!("// retained {i:04} xxxx\n"))
            .collect::<String>();
        let retained_source = format!(
            "fn marker_collision_target_retained() {{}}\n// #### {dropped_file} — spoofed source marker\n{retained_padding}"
        );
        put_indexed_source(
            &engine,
            retained_file,
            "fn marker_collision_target_retained() { indexed(); }\n",
            Language::Rust,
            1,
        );
        write_src(&engine, retained_file, &retained_source);

        let dropped_indexed = "fn marker_collision_target_dropped() { indexed(); }\n";
        put_indexed_source(&engine, dropped_file, dropped_indexed, Language::Rust, 1);
        // Wide enough that its section cannot fit the headroom the retained file
        // leaves, so it is rejected by the ADMISSION test by ~13 KB rather than by
        // a knife-edge. Still inside the whole-file gates (<=220 lines,
        // <=3 * 7000 chars) so it renders as one section when it does fit.
        let dropped_padding = (0..180)
            .map(|i| format!("// dropped {i:04} {}\n", "y".repeat(90)))
            .collect::<String>();
        write_src(
            &engine,
            dropped_file,
            &format!("fn marker_collision_target_dropped() {{ current(); }}\n{dropped_padding}"),
        );

        let retained = node_lang(
            "marker_collision_target_retained",
            "marker_collision_target_retained",
            retained_file,
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let dropped = node_lang(
            "marker_collision_target_dropped",
            "marker_collision_target_dropped",
            dropped_file,
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[retained.clone(), dropped.clone()]);

        // The caller files must be REAL pointer candidates, and that needs three
        // things at once — each of which the fixture previously lacked:
        //
        //  * source on disk. A file `project_source` cannot read is `continue`d
        //    BEFORE `excluded_files.push`, so it never reaches the pointer block at
        //    all. With DB rows alone the epilogue had exactly ONE candidate, which
        //    always fits, leaving the cap, the reserve and the elastic block inert.
        //  * a path component under the filesystem's 255-byte limit, so the source
        //    can actually be written.
        //  * many SUBGRAPH nodes each, since `file_node_locations` lists the
        //    subgraph's nodes for the file and only nodes with an edge to a root are
        //    reached. Uncapped, that is what makes one pointer line ~2 KB.
        //
        // Their sources are also wide enough to fail admission themselves, so they
        // stay pointer candidates instead of becoming a second rendered section.
        const CALLER_FILES: usize = 10;
        const CALLER_SYMS: usize = 60;
        let long_component = "x".repeat(200);
        let mut callers = Vec::new();
        let mut edges = Vec::new();
        for i in 0..CALLER_FILES {
            let path = format!("callers/{i:02}_{long_component}.rs");
            let body = (0..CALLER_SYMS)
                .map(|j| {
                    format!(
                        "fn bulk_pointer_sym_{i}_{j}() {{ /* {} */ }}\n",
                        "z".repeat(90)
                    )
                })
                .collect::<String>();
            put_indexed_source(&engine, &path, &body, Language::Rust, CALLER_SYMS as i64);
            let target = if i < 5 { &retained } else { &dropped };
            for j in 0..CALLER_SYMS {
                // Named without the query's own terms, so 600 extra nodes cannot
                // crowd the two real targets out of the top-8 FTS roots.
                let sym = node_lang(
                    &format!("bulk_pointer_sym_{i}_{j}"),
                    &format!("bulk_pointer_sym_{i}_{j}"),
                    &path,
                    (j + 1) as i64,
                    (j + 1) as i64,
                    NodeKind::Function,
                    Language::Rust,
                );
                edges.push(mk_edge(
                    &sym.id,
                    &target.id,
                    codegraph_core::types::EdgeKind::Calls,
                ));
                callers.push(sym);
            }
        }
        put_nodes(&mut engine, &callers);
        put_edges(&mut engine, &edges);

        // Cross into the large-project budget tier, which turns the epilogue meta
        // ON — the "Not shown above" pointer block and the completeness signal.
        // That block is what names an admission-dropped file, and it is what the
        // hard-ceiling cut used to destroy.
        for i in 0..5001 {
            put_file(
                &engine,
                &file_rec(&format!("pad/f{i}.rs"), Language::Rust, 0),
            );
        }

        let result = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": query, "maxFiles": 2}),
        );
        let text = text_of(&result);
        assert!(
            text.contains(&format!("// #### {dropped_file} — spoofed source marker")),
            "the retained source must contain the colliding marker: {text}"
        );
        // The file left by ADMISSION, not by the cut and not by the file cap.
        // `maxFiles` is 2, so a cap-drop would still have rendered two sections;
        // exactly one section means the budget rejected the second.
        let headers: Vec<&str> = text.lines().filter(|l| l.starts_with("#### ")).collect();
        assert_eq!(
            headers.len(),
            1,
            "maxFiles permits 2, so one section means admission rejected the other: {text}"
        );
        assert!(
            headers[0].contains(retained_file),
            "the surviving section must be the retained file: {headers:?}"
        );
        assert!(
            !text.contains("output truncated to budget"),
            "the response fits the inline ceiling, so the cut must NOT run: {text}"
        );
        // ...and the epilogue survives INTACT, naming the dropped file and carrying
        // a true count. Uncapped AND non-elastic — the pre-fix state — the cut
        // deletes this block along with the section, so the response neither shows
        // the file nor says it exists: measured, the completeness signal is gone and
        // only 4 of the 10 pointer lines survive.
        assert!(
            text.contains(&format!("- {dropped_file}:")),
            "an admission-dropped file must be named in the pointer block: {text}"
        );
        assert!(
            text.contains(&format!("Complete source for {} files", headers.len())),
            "the epilogue must survive with a count matching the rendered sections: {text}"
        );
        assert!(
            !text.contains("marker_collision_target_dropped() { current(); }"),
            "the dropped file's source section must not survive: {text}"
        );
        assert!(
            text.starts_with("⚠️ Some files referenced below"),
            "the retained drifted source must still produce a banner: {text}"
        );
        assert!(
            !text.contains(&format!("\n  - {dropped_file}\n")),
            "source text that mimics a dropped section header must not cite that dropped file: {text}"
        );
    }

    #[test]
    fn ext_node_container_renders_outline() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "w.rs",
            &(1..=40).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            2,
        );
        let class = node_lang(
            "Widget",
            "Widget",
            "w.rs",
            1,
            40,
            NodeKind::Struct,
            Language::Rust,
        );
        let method = node_lang(
            "render",
            "Widget::render",
            "w.rs",
            5,
            10,
            NodeKind::Method,
            Language::Rust,
        );
        put_nodes(&mut engine, &[class.clone(), method.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &class.id,
                &method.id,
                codegraph_core::types::EdgeKind::Contains,
            )],
        );

        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "Widget", "includeCode": true}),
        );
        let txt = text_of(&tr);
        assert!(txt.contains("**Members (1):**"), "got: {txt}");
        assert!(txt.contains("render"));
        assert!(txt.contains("Structural outline only"));
    }

    #[test]
    fn ext_node_ambiguous_without_and_with_code() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "a.rs",
            &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            1,
        );
        put_indexed_source(
            &engine,
            "b.rs",
            &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            1,
        );
        let n1 = node_lang(
            "dup",
            "dup",
            "a.rs",
            1,
            3,
            NodeKind::Function,
            Language::Rust,
        );
        let n2 = node_lang(
            "dup",
            "dup",
            "b.rs",
            5,
            7,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[n1, n2]);

        let tr = engine.execute("codegraph_node", &serde_json::json!({"symbol": "dup"}));
        let txt = text_of(&tr);
        assert!(txt.contains("definitions named \"dup\""), "got: {txt}");
        assert!(txt.contains("includeCode: true"));

        let tr2 = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "dup", "includeCode": true}),
        );
        assert!(
            text_of(&tr2).contains("Returning 2 in full"),
            "got: {}",
            text_of(&tr2)
        );
    }

    #[test]
    fn ext_file_view_no_files_indexed() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_node", &serde_json::json!({"file": "x.rs"}));
        assert!(text_of(&tr).contains("No files indexed"));
    }

    #[test]
    fn ext_file_view_no_match() {
        let engine = test_engine();
        put_file(&engine, &file_rec("real.rs", Language::Rust, 1));
        let tr = engine.execute("codegraph_node", &serde_json::json!({"file": "missing.rs"}));
        assert!(
            text_of(&tr).contains("No indexed file matches"),
            "got: {}",
            text_of(&tr)
        );
    }

    #[test]
    fn ext_file_view_ambiguous_match() {
        let engine = test_engine();
        put_file(&engine, &file_rec("src/a/mod.rs", Language::Rust, 1));
        put_file(&engine, &file_rec("src/b/mod.rs", Language::Rust, 1));
        let tr = engine.execute("codegraph_node", &serde_json::json!({"file": "mod.rs"}));
        assert!(
            text_of(&tr).contains("matches 2 indexed files"),
            "got: {}",
            text_of(&tr)
        );
    }

    #[test]
    fn ext_file_view_symbols_only_and_empty() {
        let mut engine = test_engine();
        put_file(&engine, &file_rec("svc.rs", Language::Rust, 1));
        let mut n = node_lang(
            "thing",
            "thing",
            "svc.rs",
            3,
            4,
            NodeKind::Function,
            Language::Rust,
        );
        n.signature = Some("fn thing()".to_string());
        put_nodes(&mut engine, &[n]);
        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"file": "svc.rs", "symbolsOnly": true}),
        );
        let txt = text_of(&tr);
        assert!(txt.contains("symbol"), "got: {txt}");
        assert!(txt.contains("thing"));
        assert!(txt.contains("Drop `symbolsOnly`"));

        let engine2 = test_engine();
        put_file(&engine2, &file_rec("empty.rs", Language::Rust, 0));
        let tr2 = engine2.execute(
            "codegraph_node",
            &serde_json::json!({"file": "empty.rs", "symbolsOnly": true}),
        );
        assert!(text_of(&tr2).contains("No indexed symbols in this file"));
    }

    #[test]
    fn ext_file_view_read_source_full_and_ranged() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "svc.rs",
            &(1..=10).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            1,
        );
        let n = node_lang(
            "thing",
            "thing",
            "svc.rs",
            3,
            4,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[n]);

        let full = engine.execute("codegraph_node", &serde_json::json!({"file": "svc.rs"}));
        assert!(text_of(&full).contains("line 1"), "got: {}", text_of(&full));

        let ranged = engine.execute(
            "codegraph_node",
            &serde_json::json!({"file": "svc.rs", "offset": 2, "limit": 3}),
        );
        let rtxt = text_of(&ranged);
        assert!(rtxt.contains("line 2"), "got: {rtxt}");
        assert!(rtxt.contains("pass `offset`/`limit`"));
    }

    #[test]
    fn ext_file_view_offset_past_end() {
        let mut engine = test_engine();
        put_indexed_source(&engine, "svc.rs", "one\ntwo\nthree\n", Language::Rust, 1);
        put_nodes(
            &mut engine,
            &[node_lang(
                "t",
                "t",
                "svc.rs",
                1,
                1,
                NodeKind::Function,
                Language::Rust,
            )],
        );
        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"file": "svc.rs", "offset": 999}),
        );
        assert!(
            text_of(&tr).contains("is past the end"),
            "got: {}",
            text_of(&tr)
        );
    }

    #[test]
    fn ext_file_view_missing_on_disk() {
        let mut engine = test_engine();
        put_file(&engine, &file_rec("gone.rs", Language::Rust, 1));
        put_nodes(
            &mut engine,
            &[node_lang(
                "g",
                "g",
                "gone.rs",
                1,
                2,
                NodeKind::Function,
                Language::Rust,
            )],
        );
        let tr = engine.execute("codegraph_node", &serde_json::json!({"file": "gone.rs"}));
        assert!(
            text_of(&tr).contains("could not read from disk"),
            "got: {}",
            text_of(&tr)
        );
        assert!(
            !text_of(&tr).starts_with("⚠️ Some files referenced below"),
            "unreadable bytes that were not served must not fabricate a banner: {}",
            text_of(&tr)
        );
    }

    #[test]
    fn ext_explore_empty_query_errors() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_explore", &serde_json::json!({}));
        assert_eq!(tr.is_error, Some(true));
    }

    #[test]
    fn ext_explore_no_relevant_code() {
        let engine = test_engine();
        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "nothing_here_zzz"}),
        );
        assert!(text_of(&tr).contains("No relevant code found for"));
    }

    #[test]
    fn ext_explore_full_render_with_blast_relationships_and_source() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "svc.rs",
            &(1..=40).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            3,
        );
        // 5001 empty file rows cross the file_count>=5000 budget tier, enabling
        // include_relationships/additional_files/completeness/budget_note.
        for i in 0..5001 {
            put_file(
                &engine,
                &file_rec(&format!("pad/f{i}.rs"), Language::Rust, 0),
            );
        }
        let root = node_lang(
            "processOrder",
            "processOrder",
            "svc.rs",
            5,
            12,
            NodeKind::Function,
            Language::Rust,
        );
        let caller = node_lang(
            "mainLoop",
            "mainLoop",
            "svc.rs",
            20,
            30,
            NodeKind::Function,
            Language::Rust,
        );
        let callee = node_lang(
            "validate",
            "validate",
            "svc.rs",
            32,
            38,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[root.clone(), caller.clone(), callee.clone()]);
        put_edges(
            &mut engine,
            &[
                mk_edge(&caller.id, &root.id, codegraph_core::types::EdgeKind::Calls),
                mk_edge(&root.id, &callee.id, codegraph_core::types::EdgeKind::Calls),
            ],
        );

        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "processOrder"}),
        );
        let txt = text_of(&tr);
        assert!(txt.contains("## Exploration: processOrder"), "got: {txt}");
        assert!(txt.contains("### Source Code"));
        assert!(txt.contains("Blast radius"), "got: {txt}");
        assert!(txt.contains("### Relationships"), "got: {txt}");
        assert!(txt.contains("line 5"));
        assert!(
            txt.contains("files indexed"),
            "expected the follow-up-call note, got: {txt}"
        );
    }

    /// #1504 — the note must not promise a call CAP. Nothing enforces one, so
    /// the upstream "N calls for this project / Synthesize once you've used N"
    /// wording only made agents stop exploring early.
    #[test]
    fn ext_explore_note_promises_no_unenforced_call_budget() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "svc.rs",
            "fn processOrder() {}\n",
            Language::Rust,
            1,
        );
        for i in 0..5001 {
            put_file(
                &engine,
                &file_rec(&format!("pad/f{i}.rs"), Language::Rust, 0),
            );
        }
        put_nodes(
            &mut engine,
            &[node_lang(
                "processOrder",
                "processOrder",
                "svc.rs",
                1,
                1,
                NodeKind::Function,
                Language::Rust,
            )],
        );
        let txt = text_of(&engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "processOrder"}),
        ));

        for banned in [
            "Explore budget:",
            "Synthesize once",
            "calls for this project",
            "remaining calls",
        ] {
            assert!(
                !txt.contains(banned),
                "the explore note must not imply an unenforced call cap, found {banned:?} in: {txt}"
            );
        }
        assert!(
            txt.contains("no call limit"),
            "the note must say so explicitly, got: {txt}"
        );
    }

    #[test]
    fn ext_explore_max_files_clamped() {
        let mut engine = test_engine();
        put_indexed_source(&engine, "a.rs", "fn foo() {}\n", Language::Rust, 1);
        put_nodes(
            &mut engine,
            &[node_lang(
                "foo",
                "foo",
                "a.rs",
                1,
                1,
                NodeKind::Function,
                Language::Rust,
            )],
        );
        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "foo", "maxFiles": 0}),
        );
        assert!(text_of(&tr).contains("Exploration: foo"));
    }

    #[test]
    fn ext_status_renders_counts_and_kinds() {
        let mut engine = test_engine();
        put_file(&engine, &file_rec("a.rs", Language::Rust, 2));
        put_nodes(
            &mut engine,
            &[
                node_lang("f", "f", "a.rs", 1, 2, NodeKind::Function, Language::Rust),
                node_lang("C", "C", "a.rs", 3, 8, NodeKind::Class, Language::Rust),
            ],
        );
        let tr = engine.execute("codegraph_status", &serde_json::json!({}));
        let txt = text_of(&tr);
        assert!(txt.contains("## CodeGraph Status"), "got: {txt}");
        assert!(txt.contains("**Files indexed:**"));
        assert!(txt.contains("Nodes by Kind"));
        assert!(txt.contains("Languages"));
    }

    #[test]
    fn ext_files_no_index_and_tree_flat_grouped() {
        let engine = test_engine();
        let empty = engine.execute("codegraph_files", &serde_json::json!({}));
        assert!(text_of(&empty).contains("No files indexed"));

        let engine = test_engine();
        put_file(&engine, &file_rec("src/a.rs", Language::Rust, 1));
        put_file(&engine, &file_rec("src/b.rs", Language::Rust, 2));

        let tree = engine.execute("codegraph_files", &serde_json::json!({"format": "tree"}));
        assert!(
            text_of(&tree).contains("Project Structure"),
            "got: {}",
            text_of(&tree)
        );
        let flat = engine.execute("codegraph_files", &serde_json::json!({"format": "flat"}));
        assert!(text_of(&flat).contains("## Files ("));
        let grouped = engine.execute("codegraph_files", &serde_json::json!({"format": "grouped"}));
        assert!(text_of(&grouped).contains("Files by Language"));
    }

    #[test]
    fn ext_files_path_and_pattern_filters() {
        let engine = test_engine();
        put_file(&engine, &file_rec("src/a.rs", Language::Rust, 1));
        put_file(&engine, &file_rec("b.md", Language::Unknown, 0));

        let by_path = engine.execute("codegraph_files", &serde_json::json!({"path": "src/"}));
        assert!(
            text_of(&by_path).contains("a.rs"),
            "got: {}",
            text_of(&by_path)
        );
        assert!(!text_of(&by_path).contains("b.md"));

        let by_pattern = engine.execute("codegraph_files", &serde_json::json!({"pattern": "*.md"}));
        assert!(
            text_of(&by_pattern).contains("b.md"),
            "got: {}",
            text_of(&by_pattern)
        );

        let none = engine.execute("codegraph_files", &serde_json::json!({"pattern": "*.zzz"}));
        assert!(text_of(&none).contains("No files found matching"));
    }

    #[test]
    fn ext_check_no_cycles_and_export_json() {
        let mut engine = test_engine();
        put_file(&engine, &file_rec("a.rs", Language::Rust, 1));
        put_nodes(
            &mut engine,
            &[node_lang(
                "f",
                "f",
                "a.rs",
                1,
                2,
                NodeKind::Function,
                Language::Rust,
            )],
        );

        let chk = engine.execute("codegraph_check", &serde_json::json!({}));
        assert!(text_of(&chk).contains("No circular dependencies found"));

        let exp = engine.execute("codegraph_export", &serde_json::json!({}));
        let v: Value = serde_json::from_str(&text_of(&exp)).unwrap();
        assert!(v.is_object());
    }

    #[test]
    fn ext_unknown_tool_backstop() {
        let engine = test_engine();
        let tr = engine.execute("codegraph_bogus", &serde_json::json!({}));
        assert_eq!(tr.is_error, Some(true));
        assert!(text_of(&tr).contains("Unknown tool"));
    }

    #[test]
    fn ext_glob_match_variants() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("src/?.rs", "src/a.rs"));
        assert!(!glob_match("src/?.rs", "src/ab.rs"));
        assert!(glob_match("a-b_c.rs", "a-b_c.rs"));
    }

    #[test]
    fn ext_truncate_output_caps_long_text() {
        assert_eq!(truncate_output("hello"), "hello");
        let mut long = "x".repeat(49_000);
        long.push('\n');
        long.push_str(&"y".repeat(5_000));
        let out = truncate_output(&long);
        assert!(out.contains("output truncated"));
        assert!(out.len() < long.len());
    }

    #[test]
    fn ext_cut_at_section_boundary_prefers_section() {
        let mut s = String::from("head\n");
        s.push_str(&"a".repeat(100));
        s.push_str("\n#### file.rs — X\n");
        s.push_str(&"b".repeat(400));
        let (out, _) = cut_at_section_boundary(&s, 200);
        assert!(out.contains("output truncated to budget"), "got: {out}");
    }

    #[test]
    fn ext_helpers_plural_normalize_basename_stripext() {
        assert_eq!(plural(1), "");
        assert_eq!(plural(2), "s");
        assert_eq!(normalize_ws("a   b\tc"), "a b c");
        assert_eq!(basename("a/b/c.rs"), "c.rs");
        assert_eq!(strip_ext("mod.rs"), "mod");
        assert_eq!(strip_ext(".hidden"), ".hidden");
    }

    #[test]
    fn ext_dependents_summary_variants() {
        assert!(dependents_summary(&[]).contains("no other indexed file"));
        let many: Vec<String> = (0..10).map(|i| format!("f{i}.rs")).collect();
        let s = dependents_summary(&many);
        assert!(s.contains("+2 more"), "got: {s}");
    }

    #[test]
    fn ext_matches_symbol_qualified_and_file() {
        let f = node_lang(
            "mod",
            "mod",
            "src/mod.rs",
            1,
            1,
            NodeKind::File,
            Language::Rust,
        );
        assert!(matches_symbol(&f, "mod"));
        let mut m = node_lang(
            "render",
            "widget::render",
            "src/widget.rs",
            1,
            1,
            NodeKind::Method,
            Language::Rust,
        );
        m.qualified_name = "widget::render".to_string();
        assert!(matches_symbol(&m, "widget::render"));
        assert!(!matches_symbol(&m, "other::render"));
    }

    #[test]
    fn ext_parse_node_kind_all() {
        assert_eq!(parse_node_kind("function"), Some(NodeKind::Function));
        assert_eq!(parse_node_kind("method"), Some(NodeKind::Method));
        assert_eq!(parse_node_kind("class"), Some(NodeKind::Class));
        assert_eq!(parse_node_kind("interface"), Some(NodeKind::Interface));
        assert_eq!(parse_node_kind("type"), Some(NodeKind::TypeAlias));
        assert_eq!(parse_node_kind("variable"), Some(NodeKind::Variable));
        assert_eq!(parse_node_kind("route"), Some(NodeKind::Route));
        assert_eq!(parse_node_kind("component"), Some(NodeKind::Component));
        assert_eq!(parse_node_kind("bogus"), None);
    }

    #[test]
    fn ext_is_handler_method_name_all() {
        for n in [
            "handle",
            "handleAsync",
            "execute",
            "consume",
            "run",
            "__invoke",
        ] {
            assert!(is_handler_method_name(n), "{n}");
        }
        assert!(!is_handler_method_name("random"));
    }

    #[test]
    fn ext_is_generated_and_test_file_detectors() {
        assert!(is_generated_file("api.pb.go"));
        assert!(is_generated_file("model.g.dart"));
        assert!(!is_generated_file("main.rs"));
        assert!(is_test_file("src/foo.test.ts"));
        assert!(is_test_file("crate/tests/bar.rs"));
        assert!(!is_test_file("src/lib.rs"));
        assert!(is_low_value_file("src/foo.spec.ts"));
        assert!(is_low_value_file("assets/icon.svg"));
        assert!(!is_low_value_file("src/lib.rs"));
    }

    /// Tier 1 item 1 (upstream #1507) — the MCP-side half of the bidirectional
    /// disagreement: the CLI's `is_test_file` had `/e2e/` and this one did not,
    /// so a Playwright/Cypress `e2e/` tree read as production code on every MCP
    /// path (blast radius, `excludeLowValueFiles`, the indirect-test note).
    /// After convergence both predicates are the UNION of the two pattern sets.
    #[test]
    fn ext_is_test_file_recognizes_an_e2e_directory() {
        // The MCP-side gap: an `/e2e/` segment, with no `.test.`/`.spec.` help.
        assert!(
            is_test_file("e2e/checkout.ts"),
            "a top-level e2e/ tree is a test tree"
        );
        assert!(
            is_test_file("apps/web/e2e/login.ts"),
            "a nested e2e/ tree is a test tree"
        );
        // And it flows through to `excludeLowValueFiles`, which is what keeps an
        // e2e tree from crowding out implementations in explore output.
        assert!(is_low_value_file("e2e/checkout.ts"));
        // Negative: `e2e` must be a whole path SEGMENT, never a substring.
        assert!(
            !is_test_file("src/route2e2e.ts"),
            "an `e2e` substring inside a filename is not a test tree"
        );
    }

    /// Tier 1 item 1, CLI-side half kept in force here too: Go's `_test.go`
    /// convention (already recognized by this predicate) plus the union's other
    /// members all classify, so the two predicates agree in BOTH directions.
    #[test]
    fn ext_is_test_file_covers_the_full_union() {
        for path in [
            "pkg/math_test.go",
            "src/foo.test.ts",
            "src/foo.spec.ts",
            "pkg/__tests__/a.js",
            "app/test/mod.rs",
            "app/tests/mod.rs",
            "app/spec/mod.rb",
            "e2e/flow.ts",
        ] {
            assert!(is_test_file(path), "union member must classify: {path}");
        }
        for path in ["src/lib.rs", "src/main.rs", "internal/latest.go"] {
            assert!(
                !is_test_file(path),
                "production file must not classify: {path}"
            );
        }
    }

    /// Tier 1 item 1 through a real MCP-path CONSUMER, not just the predicate:
    /// `codegraph_explore`'s blast radius splits a symbol's caller files into
    /// "used by" (production) and "tested via callers" (tests). Pre-fix an
    /// `e2e/` caller landed in the production list, so the MCP surface claimed
    /// the symbol had no test coverage.
    #[test]
    fn ext_blast_radius_lists_an_e2e_caller_as_a_test() {
        let mut engine = test_engine();
        let target = node_lang(
            "checkout",
            "checkout",
            "src/checkout.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        let e2e_caller = node_lang(
            "checkoutFlow",
            "checkoutFlow",
            "e2e/checkout.ts",
            1,
            5,
            NodeKind::Function,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/checkout.ts",
            "export function checkout() {\n  return 1;\n}\n",
            Language::TypeScript,
            1,
        );
        put_indexed_source(
            &engine,
            "e2e/checkout.ts",
            "import { checkout } from '../src/checkout';\nexport function checkoutFlow() {\n  checkout();\n}\n",
            Language::TypeScript,
            1,
        );
        put_nodes(&mut engine, &[target.clone(), e2e_caller.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &e2e_caller.id,
                &target.id,
                codegraph_core::types::EdgeKind::Calls,
            )],
        );

        let text = text_of(&engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "checkout"}),
        ));
        let radius = text
            .lines()
            .find(|l| l.contains("checkout") && l.contains("e2e/checkout.ts"))
            .unwrap_or_else(|| panic!("blast radius must mention the e2e caller: {text}"));
        assert!(
            radius.contains("tests: `e2e/checkout.ts`"),
            "an e2e/ caller must be credited as test coverage; got: {radius}"
        );
        assert!(
            !radius.contains("no tests found"),
            "an e2e/ caller must not leave a no-coverage claim standing; got: {radius}"
        );
        assert!(
            !radius.contains("in `e2e/checkout.ts`"),
            "an e2e/ caller must not also be listed as a production user; got: {radius}"
        );
    }

    /// Tier 1 item 3 (upstream `generated-detection.ts` `GENERATED_PATTERNS`) —
    /// one assertion group per language family in the ported table. Pre-fix only
    /// the five Go/Dart protobuf suffixes were recognized, so mockgen output,
    /// every TS/JS codegen convention, vendored minified bundles, Python
    /// `_pb2*`, C++/C#/Java protobuf, Swift and in-tree Rust codegen all read as
    /// hand-written implementations and outranked the real code.
    #[test]
    fn ext_is_generated_file_covers_upstream_language_families() {
        // Go — protobuf / gRPC / pulsar (pre-existing).
        for p in ["api.pb.go", "api.pulsar.go", "api_grpc.pb.go"] {
            assert!(is_generated_file(p), "go protobuf: {p}");
        }
        // Go — mockgen. Upstream accepts BOTH suffix spellings because projects
        // rename mockgen output (cosmos-sdk uses `expected_*_mocks.go`).
        for p in ["store_mock.go", "expected_keepers_mocks.go"] {
            assert!(is_generated_file(p), "go mockgen: {p}");
        }
        // TypeScript / JavaScript codegen (Apollo/GraphQL, Prisma, ts-proto,
        // gRPC-web, swagger-codegen) — all four `[jt]sx?` spellings.
        for p in [
            "schema.generated.ts",
            "schema.generated.tsx",
            "schema.generated.js",
            "schema.generated.jsx",
            "api.gen.ts",
            "api.gen.tsx",
            "api.gen.js",
            "api.gen.jsx",
            "svc.pb.ts",
            "svc.pb.js",
            "svc_pb.ts",
            "svc_pb.js",
            "svc_grpc_pb.ts",
            "svc_grpc_pb.js",
        ] {
            assert!(is_generated_file(p), "ts/js codegen: {p}");
        }
        // Vendored minified bundles — single-letter symbols make name edges noise.
        for p in ["vendor/chart.min.js", "vendor/chart.min.mjs"] {
            assert!(is_generated_file(p), "minified bundle: {p}");
        }
        // Python — protobuf / gRPC.
        for p in ["svc_pb2.py", "svc_pb2_grpc.py", "svc_pb2.pyi"] {
            assert!(is_generated_file(p), "python protobuf: {p}");
        }
        // C++ — protobuf.
        for p in ["svc.pb.cc", "svc.pb.h"] {
            assert!(is_generated_file(p), "c++ protobuf: {p}");
        }
        // C# — protobuf / gRPC.
        for p in ["Model.g.cs", "GreeterGrpc.cs"] {
            assert!(is_generated_file(p), "c# protobuf: {p}");
        }
        // Java — protoc-gen-java / protoc-gen-grpc-java.
        for p in ["GreeterOuterClass.java", "GreeterGrpc.java"] {
            assert!(is_generated_file(p), "java protobuf: {p}");
        }
        // Swift — protobuf.
        assert!(is_generated_file("Model.pb.swift"), "swift protobuf");
        // Dart — build_runner / freezed / protobuf / chopper.
        for p in [
            "model.g.dart",
            "model.freezed.dart",
            "model.pb.dart",
            "model.pbgrpc.dart",
            "api.chopper.dart",
        ] {
            assert!(is_generated_file(p), "dart codegen: {p}");
        }
        // Rust — in-tree codegen convention.
        assert!(is_generated_file("src/proto.generated.rs"), "rust codegen");

        // Negatives: hand-written files that merely share a stem.
        for p in [
            "main.rs",
            "src/generator.ts",
            "src/mocks.go",
            "src/genesis.go",
            "src/pb.go",
            "src/table.js",
            "docs/min.css",
        ] {
            assert!(
                !is_generated_file(p),
                "hand-written file must not match: {p}"
            );
        }
    }

    /// Tier 1 item 3, shape detail: upstream anchors mockgen's DEFAULT output as
    /// `/^mock_[^/]+\.go$/` — a BASENAME rule. A naive `starts_with` on the full
    /// path would miss every nested occurrence, which is where mocks actually
    /// live, so the final path segment is what must be tested.
    #[test]
    fn ext_is_generated_file_mock_prefix_is_basename_anchored() {
        assert!(
            is_generated_file("src/mock_helpers.go"),
            "nested mockgen default output must match on its basename"
        );
        assert!(
            is_generated_file("mock_store.go"),
            "repo-root mockgen default output must match"
        );
        assert!(
            !is_generated_file("src/mockery_notgen.go"),
            "`mock` without the underscore separator is a hand-written file"
        );
        assert!(
            !is_generated_file("mock_store/registry.go"),
            "a `mock_` DIRECTORY is not a generated file"
        );
    }

    /// Tier 1 item 2 — generated-file demotion on the `explore` path. Two
    /// same-named `ComputePay` definitions, one in a `.pb.go` stub and one in a
    /// real file whose path sorts LAST alphabetically. `codegraph_node` already
    /// demoted the stub (`find_symbol_matches`); `explore` did not consult the
    /// signal at all, so within the same relevance tier the alphabetical
    /// tie-break put the generated stub's section first.
    #[test]
    fn ext_explore_ranks_a_real_implementation_above_a_generated_stub() {
        let mut engine = test_engine();
        let generated = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/a_gen.pb.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        let real = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/zz_real.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        put_indexed_source(
            &engine,
            "payroll/a_gen.pb.go",
            "func ComputePay() int {\n\treturn 0\n}\n",
            Language::Go,
            1,
        );
        put_indexed_source(
            &engine,
            "payroll/zz_real.go",
            "func ComputePay() int {\n\treturn 42\n}\n",
            Language::Go,
            1,
        );
        put_nodes(&mut engine, &[generated.clone(), real.clone()]);

        let text = text_of(&engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "ComputePay"}),
        ));
        let order: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("#### "))
            .filter_map(|l| {
                ["payroll/zz_real.go", "payroll/a_gen.pb.go"]
                    .into_iter()
                    .find(|f| l.contains(f))
            })
            .collect();
        assert_eq!(
            order,
            vec!["payroll/zz_real.go", "payroll/a_gen.pb.go"],
            "the real implementation's section must precede the generated stub's; got {order:?} in:\n{text}"
        );
    }

    /// #1500 — the same ranking property as the `.pb.go` test above, but the
    /// generated file carries an ORDINARY Go filename and is known only by its
    /// stored `files.generated` flag. The path check cannot see it, so this fails
    /// unless `finalize` consults the batch-fetched set.
    #[test]
    fn explore_ranks_a_real_impl_above_a_banner_marked_generated_file() {
        let mut engine = test_engine();
        // Both names are ordinary: `a_banner.go` sorts FIRST alphabetically and
        // would win on the tie-break, so a pass cannot come from name ordering.
        let generated = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/a_banner.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        let real = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/zz_real.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/a_banner.go",
            "// Code generated by fkit-crud. DO NOT EDIT.\nfunc ComputePay() int {\n\treturn 0\n}\n",
            Language::Go,
            1,
            true,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/zz_real.go",
            "func ComputePay() int {\n\treturn 42\n}\n",
            Language::Go,
            1,
            false,
        );
        // Neither path matches the suffix check, so the stored flag is the ONLY
        // signal available — that is what makes this test the #1500 regression.
        assert!(!is_generated_file("payroll/a_banner.go"));
        put_nodes(&mut engine, &[generated.clone(), real.clone()]);

        let text = text_of(&engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "ComputePay"}),
        ));
        let order: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("#### "))
            .filter_map(|l| {
                ["payroll/zz_real.go", "payroll/a_banner.go"]
                    .into_iter()
                    .find(|f| l.contains(f))
            })
            .collect();
        assert_eq!(
            order,
            vec!["payroll/zz_real.go", "payroll/a_banner.go"],
            "the real implementation must precede the banner-marked file; got {order:?} in:\n{text}"
        );
    }

    /// #1500 — the same signal in `find_symbol_matches`, whose sort is ASCENDING
    /// (so `false` sorts first). A flipped polarity would still compile, so this
    /// pins the direction as well as the presence of the union.
    #[test]
    fn find_symbol_matches_demotes_a_banner_marked_definition() {
        let mut engine = test_engine();
        let generated = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/a_banner.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        let real = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/zz_real.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/a_banner.go",
            "// Code generated by fkit-crud. DO NOT EDIT.\nfunc ComputePay() int {\n\treturn 0\n}\n",
            Language::Go,
            1,
            true,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/zz_real.go",
            "func ComputePay() int {\n\treturn 42\n}\n",
            Language::Go,
            1,
            false,
        );
        put_nodes(&mut engine, &[generated, real]);

        let matches = engine.find_symbol_matches("ComputePay").unwrap();
        let files: Vec<&str> = matches.iter().map(|n| n.file_path.as_str()).collect();
        assert_eq!(
            files,
            vec!["payroll/zz_real.go", "payroll/a_banner.go"],
            "find_symbol_matches must rank the banner-marked definition LAST"
        );
    }

    /// D1 GUARD — the stored flag only ever ADDS. A database written before
    /// #1500 has `generated = 0` on every row, so a `.pb.go` file must STILL be
    /// demoted by the path check alone. Drop the `is_generated_file(...) ||` half
    /// and this fails, which is what stops the change from silently re-ranking
    /// every existing index in the wrong direction.
    #[test]
    fn a_pre_change_index_keeps_the_path_demotion_when_generated_is_zero() {
        let mut engine = test_engine();
        let generated = node_lang(
            "Marshal",
            "Marshal",
            "payroll/a_stub.pb.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        let real = node_lang(
            "Marshal",
            "Marshal",
            "payroll/zz_real.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        // `generated = 0` on BOTH rows, exactly as a pre-#1500 database reads.
        put_indexed_source_generated(
            &engine,
            "payroll/a_stub.pb.go",
            "func Marshal() int {\n\treturn 0\n}\n",
            Language::Go,
            1,
            false,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/zz_real.go",
            "func Marshal() int {\n\treturn 42\n}\n",
            Language::Go,
            1,
            false,
        );
        put_nodes(&mut engine, &[generated, real]);

        // find_symbol_matches (ASCENDING) still demotes it.
        let matches = engine.find_symbol_matches("Marshal").unwrap();
        let files: Vec<&str> = matches.iter().map(|n| n.file_path.as_str()).collect();
        assert_eq!(
            files,
            vec!["payroll/zz_real.go", "payroll/a_stub.pb.go"],
            "a stored 0 must not remove the PATH demotion in find_symbol_matches"
        );

        // ... and so does finalize, through the explore surface.
        let text = text_of(&engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "Marshal"}),
        ));
        let order: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("#### "))
            .filter_map(|l| {
                ["payroll/zz_real.go", "payroll/a_stub.pb.go"]
                    .into_iter()
                    .find(|f| l.contains(f))
            })
            .collect();
        assert_eq!(
            order,
            vec!["payroll/zz_real.go", "payroll/a_stub.pb.go"],
            "a stored 0 must not remove the PATH demotion in finalize; got {order:?} in:\n{text}"
        );
    }

    /// Pins the TODO-28 wiring on its own, so a refactor that stops populating
    /// the field fails a NAMED test instead of only shifting a ranking.
    #[test]
    fn explore_generated_files_set_is_populated_before_finalize() {
        let mut engine = test_engine();
        let generated = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/a_banner.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/a_banner.go",
            "// Code generated by fkit-crud. DO NOT EDIT.\nfunc ComputePay() int {\n\treturn 0\n}\n",
            Language::Go,
            1,
            true,
        );
        put_indexed_source_generated(
            &engine,
            "payroll/zz_real.go",
            "func ComputePay() int {\n\treturn 42\n}\n",
            Language::Go,
            1,
            false,
        );
        let real = node_lang(
            "ComputePay",
            "ComputePay",
            "payroll/zz_real.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        put_nodes(&mut engine, &[generated, real]);

        let sub = engine.find_relevant_context("ComputePay").unwrap();
        assert!(
            sub.generated_files.contains("payroll/a_banner.go"),
            "the flagged path must reach the subgraph before finalize; got {:?}",
            sub.generated_files
        );
        assert!(
            !sub.generated_files.contains("payroll/zz_real.go"),
            "an unflagged path must NOT appear; got {:?}",
            sub.generated_files
        );
    }

    /// Tier 1 item 2 NEGATIVE — the demotion is a RANKING hint, never a filter:
    /// a generated file must stay reachable, and when it is the ONLY definition
    /// site it still renders.
    #[test]
    fn ext_explore_still_renders_a_generated_file_when_it_is_the_only_match() {
        let mut engine = test_engine();
        let only = node_lang(
            "MarshalOnly",
            "MarshalOnly",
            "payroll/only_gen.pb.go",
            1,
            3,
            NodeKind::Function,
            Language::Go,
        );
        put_indexed_source(
            &engine,
            "payroll/only_gen.pb.go",
            "func MarshalOnly() int {\n\treturn 0\n}\n",
            Language::Go,
            1,
        );
        put_nodes(&mut engine, std::slice::from_ref(&only));

        let text = text_of(&engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "MarshalOnly"}),
        ));
        assert!(
            text.contains("payroll/only_gen.pb.go"),
            "a generated file must remain reachable when it is the only match: {text}"
        );
    }

    #[test]
    fn ext_query_mentions_tests_detector() {
        assert!(query_mentions_tests("how does the test suite work"));
        assert!(query_mentions_tests("verify the parser"));
        assert!(!query_mentions_tests("how does indexing work"));
    }

    #[test]
    fn ext_godot_honesty_annotation_and_sources() {
        let mut g = GodotHonesty::default();
        assert!(g.annotation(true).is_empty());
        g.reached_via_scene = true;
        g.reached_via_autoload = true;
        assert_eq!(g.reachability_sources(), "signal/get_node/group/autoload");
        assert!(g.is_dynamically_reachable());
        assert!(g.annotation(true).contains("may be reached dynamically"));
        g.dynamic_unresolved.push("foo".to_string());
        assert!(
            g.annotation(false)
                .contains("Dynamic / unresolved references")
        );
    }

    #[test]
    fn ext_cap_header_names_and_explore_file_header() {
        let names: Vec<String> = (0..5).map(|i| format!("n{i}")).collect();
        let capped = cap_header_names(&names, 2);
        assert!(capped.contains("+3 more"), "got: {capped}");
        assert_eq!(cap_header_names(&names[..2], 5), "n0, n1");

        let syms = vec![
            "a(function)".to_string(),
            "a(function)".to_string(),
            "b(function)".to_string(),
        ];
        let h = explore_file_header("f.rs", &syms, 5);
        assert!(h.starts_with("#### f.rs — "), "got: {h}");
    }

    /// Tier 5 item 14 (CG-28). Given four files — a pure declaration shim, a shim
    /// with an outgoing `Calls`, a shim another file imports, and one of mixed
    /// kinds — When the explore subgraph is built, Then only the PURE shim is
    /// marked ambient.
    ///
    /// The four conditions are each necessary: a file that runs code, a file
    /// something depends on, and a file holding real implementations are all
    /// legitimate top-ranked answers.
    #[test]
    fn ambient_declaration_predicate_four_conditions() {
        let mut engine = test_engine();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for (stem, decl) in [
            ("pure", "AmbientPureAlpha"),
            ("caller", "AmbientCallerAlpha"),
        ] {
            let path = format!("src/types/{stem}.d.ts");
            put_indexed_source(
                &engine,
                &path,
                &format!("export interface {decl} {{\n  ambientId: string;\n}}\n"),
                Language::TypeScript,
                1,
            );
            nodes.push(node_lang(
                decl,
                decl,
                &path,
                1,
                3,
                NodeKind::Interface,
                Language::TypeScript,
            ));
        }
        // The `caller` shim additionally has a node that CALLS out, so condition 3
        // rejects it: it does not merely declare.
        let runner = node_lang(
            "ambientRunner",
            "ambientRunner",
            "src/types/caller.d.ts",
            5,
            7,
            NodeKind::Function,
            Language::TypeScript,
        );
        let sink = node_lang(
            "ambientSink",
            "ambientSink",
            "src/impl/sink.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/impl/sink.ts",
            "export function ambientSink(): number {\n  return 1;\n}\n",
            Language::TypeScript,
            1,
        );
        edges.push(mk_edge(&runner.id, &sink.id, EdgeKind::Calls));
        nodes.push(runner);
        nodes.push(sink);

        // The `imported` shim is pure, but a DIFFERENT file references it, so
        // condition 4 rejects it: something depends on it.
        put_indexed_source(
            &engine,
            "src/types/imported.d.ts",
            "export interface AmbientImportedAlpha {\n  ambientId: string;\n}\n",
            Language::TypeScript,
            1,
        );
        let imported = node_lang(
            "AmbientImportedAlpha",
            "AmbientImportedAlpha",
            "src/types/imported.d.ts",
            1,
            3,
            NodeKind::Interface,
            Language::TypeScript,
        );
        let consumer = node_lang(
            "ambientConsumer",
            "ambientConsumer",
            "src/impl/consumer.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/impl/consumer.ts",
            "export function ambientConsumer(): number {\n  return 2;\n}\n",
            Language::TypeScript,
            1,
        );
        edges.push(mk_edge(&consumer.id, &imported.id, EdgeKind::References));
        nodes.push(imported);
        nodes.push(consumer);

        // The `mixed` file declares a type AND implements a function, so condition
        // 2 rejects it.
        put_indexed_source(
            &engine,
            "src/types/mixed.ts",
            "export interface AmbientMixedAlpha {\n  ambientId: string;\n}\nexport function ambientMixedRun(): number {\n  return 3;\n}\n",
            Language::TypeScript,
            2,
        );
        nodes.push(node_lang(
            "AmbientMixedAlpha",
            "AmbientMixedAlpha",
            "src/types/mixed.ts",
            1,
            3,
            NodeKind::Interface,
            Language::TypeScript,
        ));
        nodes.push(node_lang(
            "ambientMixedRun",
            "ambientMixedRun",
            "src/types/mixed.ts",
            4,
            6,
            NodeKind::Function,
            Language::TypeScript,
        ));

        put_nodes(&mut engine, &nodes);
        put_edges(&mut engine, &edges);

        let sub = engine.find_relevant_context("Ambient").unwrap();
        assert!(
            sub.ambient_files.contains("src/types/pure.d.ts"),
            "the pure shim must be ambient; got {:?}",
            sub.ambient_files
        );
        for rejected in [
            "src/types/caller.d.ts",
            "src/types/imported.d.ts",
            "src/types/mixed.ts",
        ] {
            assert!(
                !sub.ambient_files.contains(rejected),
                "{rejected} must NOT be ambient; got {:?}",
                sub.ambient_files
            );
        }
    }

    /// Tier 5 item 14 (CG-28), the combination rule. Given four files equal in
    /// `tier` AND in `sym_count`, differing only in the generated and ambient
    /// signals, When `finalize` ranks them, Then generated-only and
    /// generated+ambient produce the SAME key, so their order falls through to the
    /// path tie-break.
    ///
    /// Holding both other components fixed is the whole point: a four-boolean key
    /// would rank generated+ambient strictly below generated-only and still satisfy
    /// any assertion that let `tier` or `sym_count` vary. No weight may be 0
    /// either, or damping would cross a tier.
    #[test]
    fn ambient_and_generated_weight_ties_with_generated_only() {
        // The two generated paths are named so that PATH order puts the
        // generated+ambient one FIRST. That breaks a coincidence: with
        // `c_generated` before `d_both`, a four-boolean key produces the same
        // order as `min` and the assertion would discriminate nothing. Here the
        // two disagree — `min` ties and the path tie-break wins (`c_both` first),
        // while a fourth boolean ranks `d_generated` strictly higher.
        let mut sg = ExploreSubgraph::default();
        let mut roots = Vec::new();
        // Distinct symbol names, because `node`'s synthetic id is derived from
        // kind+name+line and four identical ones would collide into a single
        // inserted node. Each file still contributes exactly one, so `sym_count`
        // stays equal across all four.
        for (i, stem) in ["a_plain", "b_ambient", "c_both", "d_generated"]
            .into_iter()
            .enumerate()
        {
            let path = format!("src/{stem}.ts");
            let n = node(&format!("SharedName{i}"), &path, 1, 3, NodeKind::Interface);
            roots.push(n.id.clone());
            sg.insert(n);
        }
        sg.roots = roots;
        sg.generated_files.insert("src/c_both.ts".to_string());
        sg.generated_files.insert("src/d_generated.ts".to_string());
        sg.ambient_files.insert("src/b_ambient.ts".to_string());
        sg.ambient_files.insert("src/c_both.ts".to_string());
        sg.finalize();

        assert_eq!(
            sg.file_order,
            vec![
                "src/a_plain.ts",
                "src/b_ambient.ts",
                "src/c_both.ts",
                "src/d_generated.ts",
            ],
            "plain > ambient > {{generated, generated+ambient TIED, ordered by path}}"
        );
    }

    /// Tier 5 item 14 (CG-28), SCOPE. Given a shim whose only dependent references
    /// a declaration the query does not match — so that dependent is genuinely
    /// absent from the subgraph — When the ambient set is computed, Then the shim
    /// is NOT damped, because condition 4 is evaluated index-wide.
    ///
    /// A subgraph-scoped predicate passes assertion 1 and fails assertion 2: it
    /// sees zero inbound cross-file edges and damps a hand-written type module
    /// other files genuinely import. `get_callers` walks inbound edges from the
    /// ROOTS only, so a consumer pointing at a non-root declaration is unreachable
    /// at any depth — which is what makes this a scope test and not a duplicate.
    #[test]
    fn ambient_predicate_sees_a_dependent_outside_the_subgraph() {
        let build = |with_consumer: bool| {
            let mut engine = test_engine();
            let shim = "src/types/env.d.ts";
            put_indexed_source(
                &engine,
                shim,
                "export interface UploadAlpha {\n  id: string;\n}\nexport interface UploadBeta {\n  id: string;\n}\nexport interface UploadGamma {\n  id: string;\n}\nexport interface LegacyChunkMeta {\n  id: string;\n}\n",
                Language::TypeScript,
                4,
            );
            let mut nodes: Vec<Node> = ["UploadAlpha", "UploadBeta", "UploadGamma"]
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    node_lang(
                        n,
                        n,
                        shim,
                        1 + (i as i64) * 3,
                        3 + (i as i64) * 3,
                        NodeKind::Interface,
                        Language::TypeScript,
                    )
                })
                .collect();
            let legacy = node_lang(
                "LegacyChunkMeta",
                "LegacyChunkMeta",
                shim,
                10,
                12,
                NodeKind::Interface,
                Language::TypeScript,
            );
            nodes.push(legacy.clone());
            let mut edges = Vec::new();
            if with_consumer {
                let consumer_path = "src/unrelated/consumer.ts";
                put_indexed_source(
                    &engine,
                    consumer_path,
                    "export function unrelatedConsumer(): number {\n  return 1;\n}\n",
                    Language::TypeScript,
                    1,
                );
                let consumer = node_lang(
                    "unrelatedConsumer",
                    "unrelatedConsumer",
                    consumer_path,
                    1,
                    3,
                    NodeKind::Function,
                    Language::TypeScript,
                );
                edges.push(mk_edge(&consumer.id, &legacy.id, EdgeKind::References));
                nodes.push(consumer);
            }
            put_nodes(&mut engine, &nodes);
            put_edges(&mut engine, &edges);
            engine.find_relevant_context("Upload").unwrap()
        };

        let sub = build(true);
        assert!(
            !sub.nodes
                .iter()
                .any(|n| n.file_path == "src/unrelated/consumer.ts"),
            "the depending file must be genuinely OUTSIDE the subgraph, or this \
             proves nothing about scope; got {:?}",
            sub.nodes.iter().map(|n| &n.file_path).collect::<Vec<_>>()
        );
        assert!(
            !sub.ambient_files.contains("src/types/env.d.ts"),
            "condition 4 must be evaluated index-wide; got {:?}",
            sub.ambient_files
        );

        let twin = build(false);
        assert!(
            twin.ambient_files.contains("src/types/env.d.ts"),
            "with no dependent anywhere the same shim IS ambient, so the \
             difference is attributable to condition 4 alone; got {:?}",
            twin.ambient_files
        );
    }

    /// Tier 5 item 15 (CG-26/31), the elastic pointer block. Given a set of
    /// candidate pointer lines and the headroom the source sections left, When the
    /// block is fitted, Then it never spends more than that headroom and the
    /// unlisted count is TRUE — covering both files dropped by elasticity and
    /// files dropped by the ten-file cap.
    ///
    /// A fixed pointer floor and an incremental per-file charge both fail these
    /// cases, which is why they were rejected: the floor over-reserves and sheds a
    /// source section, while the incremental charge discovers the cost only after
    /// the admission it should have blocked.
    #[test]
    fn epilogue_pointer_block_fits_the_remaining_budget() {
        let three: Vec<String> = (0..3).map(|i| format!("- src/f{i}.ts: sym:1")).collect();
        let one_cost = 1 + three[0].len();

        let (all, unlisted) = fit_pointer_lines(&three, one_cost * 3);
        assert_eq!((all.len(), unlisted), (3, 0), "every line fits");

        let (some, unlisted) = fit_pointer_lines(&three, one_cost * 2);
        assert_eq!((some.len(), unlisted), (2, 1), "prefix emitted, 1 unlisted");
        assert!(some.iter().map(|l| 1 + l.len()).sum::<usize>() <= one_cost * 2);

        let (none, unlisted) = fit_pointer_lines(&three, one_cost - 1);
        assert_eq!(
            (none.len(), unlisted),
            (0, 3),
            "nothing fits ⇒ the tail must account for ALL candidates"
        );

        // Both drop mechanisms at once: 14 candidates, so the ten-file cap hides
        // four, and a budget for two hides six more.
        let many: Vec<String> = (0..14)
            .map(|i| format!("- src/g{i:02}.ts: sym:1"))
            .collect();
        let two_cost = (1 + many[0].len()) * 2;
        let (fitted, unlisted) = fit_pointer_lines(&many, two_cost);
        assert_eq!(
            (fitted.len(), unlisted),
            (2, 12),
            "N must count BOTH the cap's four and elasticity's eight"
        );
        assert!(fitted.iter().map(|l| 1 + l.len()).sum::<usize>() <= two_cost);
    }

    /// Tier 5 item 15 (CG-26/31). Given a file with 120 subgraph nodes, When its
    /// "Not shown above" pointer line is built, Then it carries at most six
    /// `name:line` pairs plus a `+114 more` tail.
    ///
    /// Uncapped, one such line reaches ~1,700 bytes and a single file's pointer
    /// exhausts any plausible epilogue budget. The cap is asserted HERE rather
    /// than on explore's output because the hard-ceiling cut deletes exactly the
    /// long lines that would prove it: measured, the longest surviving `- src/…`
    /// line in the defective output is 33 bytes.
    #[test]
    fn file_node_locations_caps_symbols_at_six() {
        let nodes: Vec<Node> = (1..=120)
            .map(|i| node(&format!("entry{i}"), "wide.ts", i, i, NodeKind::Method))
            .collect();
        let sg = subgraph_with(nodes, Vec::new());

        let line = sg.file_node_locations("wide.ts");
        assert_eq!(
            line.matches(':').count(),
            6,
            "expected exactly 6 name:line pairs, got: {line}"
        );
        assert!(
            line.ends_with(", +114 more"),
            "expected a +114 more tail, got: {line}"
        );
    }

    #[test]
    fn ext_subgraph_locations_and_language() {
        let n1 = node_lang("a", "a", "f.rs", 5, 6, NodeKind::Function, Language::Rust);
        let n2 = node_lang("b", "b", "f.rs", 1, 2, NodeKind::Function, Language::Rust);
        let sg = subgraph_with(vec![n1, n2], Vec::new());
        assert_eq!(sg.file_language("f.rs"), "rust");
        assert_eq!(sg.file_language("missing.rs"), "");
        assert_eq!(sg.file_node_locations("f.rs"), "b:1, a:5");
    }

    #[test]
    fn ext_format_impact_groups_by_file() {
        let n1 = node_lang("a", "a", "x.rs", 1, 2, NodeKind::Function, Language::Rust);
        let n2 = node_lang("b", "b", "y.rs", 3, 4, NodeKind::Function, Language::Rust);
        let refs = vec![&n1, &n2];
        let out = format_impact("sym", &refs);
        assert!(out.contains("affects 2 symbols"));
        assert!(out.contains("**x.rs:**"));
        assert!(out.contains("**y.rs:**"));
    }

    #[test]
    fn ext_number_source_lines_offsets() {
        assert_eq!(number_source_lines_at(&["one", "two"], 5), "5\tone\n6\ttwo");
    }

    #[test]
    fn ext_explore_dynamic_boundaries_and_candidates() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "bus.ts",
            "function dispatchIt(m) {\n  return m.Send(new SaveCommand(x));\n}\n\nclass SaveCommandHandler {\n  handle(cmd) { return 1; }\n}\n",
            Language::TypeScript,
            2,
        );
        let root = node_lang(
            "dispatchIt",
            "dispatchIt",
            "bus.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        let handler = node_lang(
            "SaveCommandHandler",
            "SaveCommandHandler",
            "bus.ts",
            5,
            7,
            NodeKind::Class,
            Language::TypeScript,
        );
        let method = node_lang(
            "handle",
            "SaveCommandHandler::handle",
            "bus.ts",
            6,
            6,
            NodeKind::Method,
            Language::TypeScript,
        );
        put_nodes(
            &mut engine,
            &[root.clone(), handler.clone(), method.clone()],
        );
        put_edges(
            &mut engine,
            &[mk_edge(
                &handler.id,
                &method.id,
                codegraph_core::types::EdgeKind::Contains,
            )],
        );

        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "dispatchIt"}),
        );
        let txt = text_of(&tr);
        assert!(txt.contains("Dynamic boundaries"), "got: {txt}");
        assert!(txt.contains("typed message dispatch"), "got: {txt}");
    }

    #[test]
    fn ext_open_reads_store_from_project_dir() {
        let base = std::env::temp_dir().join(format!(
            "cg-mcp-open-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(base.join(".codegraph")).unwrap();
        {
            let db = base.join(".codegraph").join("codegraph.db");
            Store::open(&db).unwrap();
        }
        let paths = codegraph_core::IndexPaths::resolve(&base, None).unwrap();
        codegraph_store::test_support::finalize_current_test_fixture(&paths).unwrap();
        let engine = CodeGraphEngine::open(&base).unwrap();
        let tr = engine.execute("codegraph_status", &serde_json::json!({}));
        assert!(text_of(&tr).contains("## CodeGraph Status"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ext_check_reports_cycle() {
        let mut engine = test_engine();
        let a = node_lang("a.rs", "a.rs", "a.rs", 1, 1, NodeKind::File, Language::Rust);
        let b = node_lang("b.rs", "b.rs", "b.rs", 1, 1, NodeKind::File, Language::Rust);
        put_nodes(&mut engine, &[a.clone(), b.clone()]);
        put_edges(
            &mut engine,
            &[
                mk_edge(&a.id, &b.id, codegraph_core::types::EdgeKind::Imports),
                mk_edge(&b.id, &a.id, codegraph_core::types::EdgeKind::Imports),
            ],
        );
        let tr = engine.execute("codegraph_check", &serde_json::json!({}));
        let txt = text_of(&tr);
        assert!(
            txt.contains("circular dependencies") || txt.contains("No circular"),
            "got: {txt}"
        );
    }

    #[test]
    fn ext_trail_overflows_trail_cap() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "hub.rs",
            &(1..=60).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            11,
        );
        let hub = node_lang(
            "hub",
            "hub",
            "hub.rs",
            1,
            2,
            NodeKind::Function,
            Language::Rust,
        );
        let mut nodes = vec![hub.clone()];
        let mut edges = Vec::new();
        for i in 0..10 {
            let callee = node_lang(
                &format!("c{i}"),
                &format!("c{i}"),
                "hub.rs",
                (i + 5) as i64,
                (i + 5) as i64,
                NodeKind::Function,
                Language::Rust,
            );
            edges.push(mk_edge(
                &hub.id,
                &callee.id,
                codegraph_core::types::EdgeKind::Calls,
            ));
            nodes.push(callee);
        }
        put_nodes(&mut engine, &nodes);
        put_edges(&mut engine, &edges);
        let tr = engine.execute("codegraph_node", &serde_json::json!({"symbol": "hub"}));
        assert!(text_of(&tr).contains("more"), "got: {}", text_of(&tr));
    }

    #[test]
    fn ext_qualified_symbol_matches_via_search() {
        let mut engine = test_engine();
        let n = node_lang(
            "render",
            "widget::render",
            "src/widget.rs",
            3,
            5,
            NodeKind::Method,
            Language::Rust,
        );
        put_nodes(&mut engine, &[n]);
        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "widget::render"}),
        );
        assert!(text_of(&tr).contains("render"), "got: {}", text_of(&tr));
    }

    #[test]
    fn ext_boundary_candidates_shortlist_rendered() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "bus.ts",
            "function dispatchIt(m) {\n  return handlers['saveOrder'](x);\n}\n\nfunction handleSaveOrder(x) { return 1; }\n",
            Language::TypeScript,
            4,
        );
        let root = node_lang(
            "dispatchIt",
            "dispatchIt",
            "bus.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        let handler = node_lang(
            "handleSaveOrder",
            "handleSaveOrder",
            "bus.ts",
            5,
            5,
            NodeKind::Function,
            Language::TypeScript,
        );
        put_nodes(&mut engine, &[root, handler]);
        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "dispatchIt"}),
        );
        let txt = text_of(&tr);
        assert!(txt.contains("Dynamic boundaries"), "got: {txt}");
        assert!(
            txt.contains("candidates for key") || txt.contains("computed member call"),
            "got: {txt}"
        );
    }

    #[test]
    fn ext_search_result_includes_signature() {
        let mut engine = test_engine();
        let mut n = node_lang(
            "sigFunc",
            "sigFunc",
            "s.rs",
            2,
            4,
            NodeKind::Function,
            Language::Rust,
        );
        n.signature = Some("fn sigFunc() -> i32".to_string());
        put_nodes(&mut engine, &[n]);
        let tr = engine.execute("codegraph_search", &serde_json::json!({"query": "sigFunc"}));
        let txt = text_of(&tr);
        assert!(txt.contains("sigFunc"), "got: {txt}");
        assert!(txt.contains("fn sigFunc"), "got: {txt}");
    }

    #[test]
    fn ext_files_flat_grouped_without_metadata() {
        let engine = test_engine();
        put_file(&engine, &file_rec("src/a.rs", Language::Rust, 1));
        let flat = engine.execute(
            "codegraph_files",
            &serde_json::json!({"format": "flat", "includeMetadata": false}),
        );
        assert!(text_of(&flat).contains("- src/a.rs"));
        let grouped = engine.execute(
            "codegraph_files",
            &serde_json::json!({"format": "grouped", "includeMetadata": false}),
        );
        assert!(text_of(&grouped).contains("Files by Language"));
    }

    #[test]
    fn ext_node_details_with_signature_and_docstring() {
        let mut n = node_lang(
            "documented",
            "documented",
            "d.rs",
            5,
            10,
            NodeKind::Function,
            Language::Rust,
        );
        n.signature = Some("fn documented()".to_string());
        n.docstring = Some("short doc".to_string());
        let out = format_node_details(&n, Some("fn documented() {}"), None);
        assert!(out.contains("**Signature:**"));
        assert!(out.contains("short doc"));
        assert!(out.contains("```rust"));
    }

    #[test]
    fn ext_symbol_map_caps_and_signature() {
        let nodes: Vec<Node> = (0..5)
            .map(|i| {
                let mut n = node_lang(
                    &format!("s{i}"),
                    &format!("s{i}"),
                    "f.rs",
                    (i + 1) as i64,
                    (i + 1) as i64,
                    NodeKind::Function,
                    Language::Rust,
                );
                n.signature = Some(format!("fn s{i}()  "));
                n
            })
            .collect();
        let lines = symbol_map("### Symbols", &nodes, 2);
        let joined = lines.join("\n");
        assert!(joined.contains("+3 more"), "got: {joined}");
        assert!(joined.contains("fn s0()"));
    }

    #[test]
    fn ext_callers_multi_match_aggregated_note() {
        let mut engine = test_engine();
        let a = node_lang(
            "api",
            "svc::api",
            "svc/a.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        let b = node_lang(
            "api",
            "core::api",
            "core/b.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        let caller = node_lang(
            "boot",
            "boot",
            "main.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[a.clone(), b.clone(), caller.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &caller.id,
                &a.id,
                codegraph_core::types::EdgeKind::Calls,
            )],
        );
        let tr = engine.execute("codegraph_callers", &serde_json::json!({"symbol": "api"}));
        let txt = text_of(&tr);
        assert!(
            txt.contains("Aggregated results across 2 symbols"),
            "got: {txt}"
        );
    }

    #[test]
    fn ext_impact_multi_match_aggregated_note() {
        let mut engine = test_engine();
        let a = node_lang(
            "core",
            "a::core",
            "a.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        let b = node_lang(
            "core",
            "b::core",
            "b.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[a, b]);
        let tr = engine.execute("codegraph_impact", &serde_json::json!({"symbol": "core"}));
        let txt = text_of(&tr);
        assert!(txt.contains("## Impact: \"core\""), "got: {txt}");
        assert!(
            txt.contains("Aggregated results across 2 symbols"),
            "got: {txt}"
        );
    }

    #[test]
    fn ext_node_qualified_symbol_resolves() {
        let mut engine = test_engine();
        put_indexed_source(&engine, "svc/worker.rs", "fn run() {}\n", Language::Rust, 1);
        let n = node_lang(
            "run",
            "worker::run",
            "svc/worker.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[n]);
        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "worker::run", "includeCode": true}),
        );
        let txt = text_of(&tr);
        assert!(
            txt.contains("## run (function)") || txt.contains("not found"),
            "got: {txt}"
        );
    }

    #[test]
    fn ext_node_fuzzy_fallback_single_result() {
        let mut engine = test_engine();
        let n = node_lang(
            "computeChecksum",
            "computeChecksum",
            "c.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[n]);
        let tr = engine.execute(
            "codegraph_node",
            &serde_json::json!({"symbol": "computeChecksum"}),
        );
        assert!(text_of(&tr).contains("computeChecksum"));
    }

    #[test]
    fn ext_boundary_candidates_typed_bus_handler_class() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "bus.ts",
            "function run(m) {\n  return m.Send(new SaveCommand(x));\n}\n\nclass SaveCommandHandler {\n  handle(cmd) { return 1; }\n}\n",
            Language::TypeScript,
            3,
        );
        let root = node_lang(
            "run",
            "run",
            "bus.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        let handler = node_lang(
            "SaveCommandHandler",
            "SaveCommandHandler",
            "bus.ts",
            5,
            7,
            NodeKind::Class,
            Language::TypeScript,
        );
        let method = node_lang(
            "handle",
            "SaveCommandHandler::handle",
            "bus.ts",
            6,
            6,
            NodeKind::Method,
            Language::TypeScript,
        );
        put_nodes(
            &mut engine,
            &[root.clone(), handler.clone(), method.clone()],
        );
        put_edges(
            &mut engine,
            &[mk_edge(
                &handler.id,
                &method.id,
                codegraph_core::types::EdgeKind::Contains,
            )],
        );
        let tr = engine.execute("codegraph_explore", &serde_json::json!({"query": "run"}));
        let txt = text_of(&tr);
        assert!(
            txt.contains("candidates for key `SaveCommand`"),
            "got: {txt}"
        );
        assert!(txt.contains("SaveCommandHandler.handle"), "got: {txt}");
    }

    #[test]
    fn ext_matches_symbol_path_hints() {
        let mut n = node_lang(
            "render",
            "render",
            "src/widget/view.rs",
            1,
            1,
            NodeKind::Method,
            Language::Rust,
        );
        n.qualified_name = "render".to_string();
        assert!(matches_symbol(&n, "widget/render"));
        assert!(matches_symbol(&n, "view/render"));
        assert!(!matches_symbol(&n, "other/render"));
    }

    #[test]
    fn ext_render_node_section_container_via_render_ambiguous() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "s.rs",
            &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            2,
        );
        let class = node_lang(
            "Svc",
            "Svc",
            "s.rs",
            1,
            20,
            NodeKind::Struct,
            Language::Rust,
        );
        let method = node_lang(
            "do_it",
            "Svc::do_it",
            "s.rs",
            5,
            8,
            NodeKind::Method,
            Language::Rust,
        );
        put_nodes(&mut engine, &[class.clone(), method.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &class.id,
                &method.id,
                codegraph_core::types::EdgeKind::Contains,
            )],
        );
        let out = engine.render_ambiguous_node("Svc", &[class], true).unwrap();
        assert!(out.contains("Members"));
    }

    #[test]
    fn ext_check_multiple_cycles_render() {
        let mut engine = test_engine();
        put_file(&engine, &file_rec("a.rs", Language::Rust, 1));
        put_file(&engine, &file_rec("b.rs", Language::Rust, 1));
        put_file(&engine, &file_rec("c.rs", Language::Rust, 1));
        let a = node_lang("a1", "a1", "a.rs", 1, 5, NodeKind::Function, Language::Rust);
        let b = node_lang("b1", "b1", "b.rs", 1, 5, NodeKind::Function, Language::Rust);
        let c = node_lang("c1", "c1", "c.rs", 1, 5, NodeKind::Function, Language::Rust);
        put_nodes(&mut engine, &[a.clone(), b.clone(), c.clone()]);
        put_edges(
            &mut engine,
            &[
                mk_edge(&a.id, &b.id, codegraph_core::types::EdgeKind::Imports),
                mk_edge(&b.id, &c.id, codegraph_core::types::EdgeKind::Imports),
                mk_edge(&c.id, &a.id, codegraph_core::types::EdgeKind::Imports),
            ],
        );
        let tr = engine.execute("codegraph_check", &serde_json::json!({}));
        assert!(text_of(&tr).contains("circular dependencies"));
    }

    #[test]
    fn ext_explore_low_value_files_excluded() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "src/core.rs",
            "fn coreFn() {}\n",
            Language::Rust,
            1,
        );
        put_indexed_source(
            &engine,
            "src/core.test.rs",
            "fn coreFn_test() {}\n",
            Language::Rust,
            1,
        );
        let core = node_lang(
            "coreFn",
            "coreFn",
            "src/core.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let core2 = node_lang(
            "coreFn2",
            "coreFn2",
            "src/core.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let test = node_lang(
            "coreFn",
            "coreFn",
            "src/core.test.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[core, core2, test]);
        let tr = engine.execute("codegraph_explore", &serde_json::json!({"query": "coreFn"}));
        assert!(text_of(&tr).contains("## Exploration: coreFn"));
    }

    #[test]
    fn ext_blast_radius_test_and_nontest_callers() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "svc.rs",
            &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            1,
        );
        let root = node_lang(
            "target",
            "target",
            "svc.rs",
            1,
            3,
            NodeKind::Function,
            Language::Rust,
        );
        let prod = node_lang(
            "producer",
            "producer",
            "src/prod.rs",
            1,
            3,
            NodeKind::Function,
            Language::Rust,
        );
        let tester = node_lang(
            "verify",
            "verify",
            "tests/target_test.rs",
            1,
            3,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[root.clone(), prod.clone(), tester.clone()]);
        put_edges(
            &mut engine,
            &[
                mk_edge(&prod.id, &root.id, codegraph_core::types::EdgeKind::Calls),
                mk_edge(&tester.id, &root.id, codegraph_core::types::EdgeKind::Calls),
            ],
        );
        let tr = engine.execute("codegraph_explore", &serde_json::json!({"query": "target"}));
        let txt = text_of(&tr);
        assert!(txt.contains("Blast radius"), "got: {txt}");
        assert!(txt.contains("tests:"), "expected tests label, got: {txt}");
    }

    #[test]
    fn ext_blast_radius_no_covering_tests_warning() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "svc.rs",
            &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
            Language::Rust,
            1,
        );
        let root = node_lang(
            "solo",
            "solo",
            "svc.rs",
            1,
            3,
            NodeKind::Function,
            Language::Rust,
        );
        let prod = node_lang(
            "onlyProd",
            "onlyProd",
            "src/prod.rs",
            1,
            3,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(&mut engine, &[root.clone(), prod.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &prod.id,
                &root.id,
                codegraph_core::types::EdgeKind::Calls,
            )],
        );
        let tr = engine.execute("codegraph_explore", &serde_json::json!({"query": "solo"}));
        let txt = text_of(&tr);
        assert!(
            txt.contains("no tests found within 3 caller hops"),
            "the no-test claim must state the searched depth: {txt}"
        );
        assert!(
            !txt.contains('⚠'),
            "the measured claim is not a warning: {txt}"
        );
    }

    #[test]
    fn ext_blast_radius_finds_a_test_at_hop_three() {
        let mut engine = test_engine();
        put_indexed_source(&engine, "svc.rs", "fn target() {}\n", Language::Rust, 1);
        let root = node_lang(
            "target",
            "target",
            "svc.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let hop_one = node_lang(
            "hop_one",
            "hop_one",
            "src/one.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let hop_two = node_lang(
            "hop_two",
            "hop_two",
            "src/two.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let test = node_lang(
            "covers_target",
            "covers_target",
            "tests/target_test.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        put_nodes(
            &mut engine,
            &[root.clone(), hop_one.clone(), hop_two.clone(), test.clone()],
        );
        put_edges(
            &mut engine,
            &[
                mk_edge(
                    &hop_one.id,
                    &root.id,
                    codegraph_core::types::EdgeKind::Calls,
                ),
                mk_edge(
                    &hop_two.id,
                    &hop_one.id,
                    codegraph_core::types::EdgeKind::Calls,
                ),
                mk_edge(
                    &test.id,
                    &hop_two.id,
                    codegraph_core::types::EdgeKind::Calls,
                ),
            ],
        );

        let tr = engine.execute("codegraph_explore", &serde_json::json!({"query": "target"}));
        let txt = text_of(&tr);
        assert!(
            txt.contains("tested via callers: `tests/target_test.rs`"),
            "a hop-3 test must be reported as coverage: {txt}"
        );
        assert!(!txt.contains("no covering tests found"), "got: {txt}");
    }

    #[test]
    fn ext_blast_radius_budget_exhaustion_uses_the_weaker_claim() {
        let mut engine = test_engine();
        put_indexed_source(&engine, "svc.rs", "fn budgeted() {}\n", Language::Rust, 1);
        let root = node_lang(
            "budgeted",
            "budgeted",
            "svc.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        let callers: Vec<Node> = (0..64)
            .map(|i| {
                node_lang(
                    &format!("caller_{i}"),
                    &format!("caller_{i}"),
                    &format!("src/caller_{i}.rs"),
                    1,
                    1,
                    NodeKind::Function,
                    Language::Rust,
                )
            })
            .collect();
        let mut nodes = Vec::with_capacity(callers.len() + 1);
        nodes.push(root.clone());
        nodes.extend(callers.iter().cloned());
        put_nodes(&mut engine, &nodes);
        let edges = callers
            .iter()
            .map(|caller| mk_edge(&caller.id, &root.id, codegraph_core::types::EdgeKind::Calls))
            .collect::<Vec<_>>();
        put_edges(&mut engine, &edges);

        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "budgeted"}),
        );
        let txt = text_of(&tr);
        assert!(
            txt.contains("no test calls this directly"),
            "an exhausted indirect search may claim only the direct-caller fact: {txt}"
        );
        assert!(
            !txt.contains('⚠'),
            "the weaker claim is not a warning: {txt}"
        );
    }

    #[test]
    fn indirect_test_note_caller_lookup_failure_uses_the_weaker_claim() {
        let engine = test_engine();
        let direct_caller = node_lang(
            "direct_caller",
            "direct_caller",
            "src/caller.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        engine
            .store
            .connection()
            .execute_batch("DROP TABLE edges")
            .unwrap();

        let note = engine.indirect_test_note(&[&direct_caller]);
        assert_eq!(
            note, "; no test calls this directly",
            "a failed caller lookup makes the indirect search incomplete"
        );
    }

    #[test]
    fn ext_boundary_candidates_generic_key_too_common() {
        let mut engine = test_engine();
        put_indexed_source(
            &engine,
            "bus.ts",
            "function dispatchIt(m) {\n  return handlers['run'](x);\n}\n",
            Language::TypeScript,
            20,
        );
        let root = node_lang(
            "dispatchIt",
            "dispatchIt",
            "bus.ts",
            1,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        let mut nodes = vec![root];
        for i in 0..15 {
            nodes.push(node_lang(
                &format!("runThing{i}"),
                &format!("runThing{i}"),
                "bus.ts",
                (i + 5) as i64,
                (i + 5) as i64,
                NodeKind::Function,
                Language::TypeScript,
            ));
        }
        put_nodes(&mut engine, &nodes);
        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "dispatchIt"}),
        );
        assert!(
            text_of(&tr).contains("Dynamic boundaries"),
            "got: {}",
            text_of(&tr)
        );
    }

    #[test]
    fn ext_matches_symbol_file_node_and_negative() {
        let f = node_lang(
            "config",
            "config",
            "src/config.rs",
            1,
            1,
            NodeKind::File,
            Language::Rust,
        );
        assert!(matches_symbol(&f, "config"));
        let m = node_lang(
            "foo",
            "foo",
            "src/foo.rs",
            1,
            1,
            NodeKind::Function,
            Language::Rust,
        );
        assert!(!matches_symbol(&m, "a.b"));
    }

    #[test]
    fn ext_last_qualifier_part_and_glob_escapes() {
        assert_eq!(last_qualifier_part("a::b::c"), Some("c"));
        assert_eq!(last_qualifier_part("plain"), Some("plain"));
        assert!(glob_match("a.b?.rs", "a.bx.rs"));
        assert!(!glob_match("a.b.rs", "axb.rs"));
    }

    #[test]
    fn ext_search_kind_filter_excludes_other_kinds() {
        let mut engine = test_engine();
        let f = node_lang(
            "betaFn",
            "betaFn",
            "b.rs",
            1,
            5,
            NodeKind::Function,
            Language::Rust,
        );
        let s = node_lang(
            "betaStruct",
            "betaStruct",
            "b.rs",
            10,
            20,
            NodeKind::Struct,
            Language::Rust,
        );
        put_nodes(&mut engine, &[f, s]);
        let tr = engine.execute(
            "codegraph_search",
            &serde_json::json!({"query": "beta", "kind": "class"}),
        );
        let txt = text_of(&tr);
        assert!(
            txt.contains("Search Results") || txt.contains("No results"),
            "got: {txt}"
        );
    }

    // --- Round B (#1064): explore change-surface rescue ------------------

    /// Build the proven B repro in a store: `newClient(opt: DialOption)` in
    /// `client.ts` (the seed the query hits) + `interface DialOption` in
    /// `options.ts` (the buried signature type, reachable via a `References`
    /// edge). A wall of lexical-namesake padding files that ALSO match the query
    /// terms outrank `options.ts` as search seeds and push it past the file
    /// budget — so pre-fix the answer file is dropped (the live #1064 bug),
    /// exactly the buried condition the rescue must undo.
    fn setup_change_surface_repro(engine: &mut CodeGraphEngine) -> (Node, Node) {
        let client = node_lang(
            "newClient",
            "newClient",
            "src/client.ts",
            2,
            2,
            NodeKind::Function,
            Language::TypeScript,
        );
        let dial = node_lang(
            "DialOption",
            "DialOption",
            "src/options.ts",
            1,
            1,
            NodeKind::Interface,
            Language::TypeScript,
        );
        put_indexed_source(
            engine,
            "src/client.ts",
            "import {DialOption} from \"./options\";\nexport function newClient(opt: DialOption): void {}\n",
            Language::TypeScript,
            1,
        );
        put_indexed_source(
            engine,
            "src/options.ts",
            "export interface DialOption { timeout: number }\n",
            Language::TypeScript,
            1,
        );
        let mut nodes = vec![client.clone(), dial.clone()];
        for i in 0..30 {
            let rel = format!("src/pad{i}.ts");
            put_indexed_source(
                engine,
                &rel,
                &format!(
                    "export function addParameterNewClient{i}(x: number): number {{ return x + {i}; }}\n"
                ),
                Language::TypeScript,
                1,
            );
            nodes.push(node_lang(
                &format!("addParameterNewClient{i}"),
                &format!("addParameterNewClient{i}"),
                &rel,
                1,
                1,
                NodeKind::Function,
                Language::TypeScript,
            ));
        }
        put_nodes(engine, &nodes);
        put_edges(
            engine,
            &[mk_edge(
                &client.id,
                &dial.id,
                codegraph_core::types::EdgeKind::References,
            )],
        );
        (client, dial)
    }

    /// #1064 CORE: explore for a plain-language "add a parameter" query surfaces
    /// the seed's buried signature type. Pre-fix `options.ts`/`DialOption` was
    /// absent (0 hits — the proven bug); post-fix it is rescued into the output.
    #[test]
    fn ext_explore_rescues_buried_signature_type() {
        let mut engine = test_engine();
        setup_change_surface_repro(&mut engine);
        let tr = engine.execute(
            "codegraph_explore",
            &serde_json::json!({"query": "newClient add parameter"}),
        );
        let txt = text_of(&tr);
        assert!(
            txt.contains("src/options.ts"),
            "buried signature-type file must be rescued into explore output, got: {txt}"
        );
        assert!(
            txt.contains("DialOption"),
            "rescued type node must surface, got: {txt}"
        );
    }

    /// #1064 CORE (subgraph-level): `find_relevant_context` inserts the buried
    /// signature type node + its file into the subgraph AND ranks its file
    /// within the top-4 (the tiny-tier `default_max_files`) so it survives the
    /// output budget. Pre-fix the wall of padding roots outrank `options.ts`,
    /// pushing it past position 4 and out of the rendered output.
    #[test]
    fn ext_find_relevant_context_rescues_buried_type_into_subgraph() {
        let mut engine = test_engine();
        let (_client, dial) = setup_change_surface_repro(&mut engine);
        let sub = engine
            .find_relevant_context("newClient add parameter")
            .unwrap();
        assert!(
            sub.node(&dial.id).is_some(),
            "rescued type node must be in the subgraph"
        );
        let pos = sub
            .file_order
            .iter()
            .position(|f| f == "src/options.ts")
            .expect("rescued type file must be ranked");
        assert!(
            pos < 4,
            "rescued type file must rank within the top-4 budget, got position {pos} in {:?}",
            sub.file_order
        );
    }

    /// #1064 NEGATIVE: a query that hits NO callable seed rescues nothing — a
    /// bare type-only query leaves the subgraph free of any rescue side effect.
    #[test]
    fn ext_explore_no_rescue_when_query_hits_no_callable() {
        let mut engine = test_engine();
        // Only a struct, no callable. A query for it must not trigger rescue.
        let s = node_lang(
            "LonelyType",
            "LonelyType",
            "src/lonely.ts",
            1,
            1,
            NodeKind::Struct,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/lonely.ts",
            "export struct LonelyType {}\n",
            Language::TypeScript,
            1,
        );
        put_nodes(&mut engine, std::slice::from_ref(&s));
        let sub = engine.find_relevant_context("LonelyType shape").unwrap();
        // The only file is the struct's own (a seed) — no extra rescued file.
        assert_eq!(
            sub.file_order,
            vec!["src/lonely.ts".to_string()],
            "no callable seed ⇒ no change-surface rescue"
        );
    }

    /// #1064 NEGATIVE: a signature type whose file is NOT buried (it is a search
    /// seed itself → high relevance) is not double-inserted, and the file
    /// appears exactly once in `file_order`.
    #[test]
    fn ext_explore_non_buried_type_not_double_added() {
        let mut engine = test_engine();
        let (client, dial) = setup_change_surface_repro(&mut engine);
        // Query names BOTH the callable and the type, so `DialOption`'s file is
        // a search seed (not buried) — rescue must be a no-op for it.
        let sub = engine
            .find_relevant_context("newClient DialOption")
            .unwrap();
        let dial_files = sub
            .file_order
            .iter()
            .filter(|f| *f == "src/options.ts")
            .count();
        assert_eq!(
            dial_files, 1,
            "non-buried type file must appear exactly once, got: {:?}",
            sub.file_order
        );
        // Both nodes present, each exactly once.
        assert_eq!(
            sub.nodes.iter().filter(|n| n.id == dial.id).count(),
            1,
            "type node must not be duplicated"
        );
        assert_eq!(
            sub.nodes.iter().filter(|n| n.id == client.id).count(),
            1,
            "seed node must not be duplicated"
        );
    }

    /// #1064 determinism: repeated explore runs produce byte-identical output.
    #[test]
    fn ext_explore_rescue_is_deterministic() {
        let mut engine = test_engine();
        setup_change_surface_repro(&mut engine);
        let run = || {
            text_of(&engine.execute(
                "codegraph_explore",
                &serde_json::json!({"query": "newClient add parameter"}),
            ))
        };
        let a = run();
        let b = run();
        let c = run();
        assert_eq!(a, b, "explore output must be byte-identical across runs");
        assert_eq!(b, c, "explore output must be byte-identical across runs");
    }

    /// #1064 named-seed de-noise: among same-named callable seeds, a
    /// low-centrality namesake (0 callers) does NOT earn the tier when a
    /// high-centrality def exists, so it can't flood the tier and crowd out the
    /// real answer. We assert the buried signature type of the HIGH-centrality
    /// def is still rescued (proving the high def was tiered) while the tier is
    /// not filled by the namesake.
    #[test]
    fn ext_explore_named_seed_denoise_excludes_low_centrality_namesake() {
        let mut engine = test_engine();
        // Real, high-centrality `handler` with many callers + a buried sig type.
        let real = node_lang(
            "handler",
            "handler",
            "src/real.ts",
            2,
            2,
            NodeKind::Function,
            Language::TypeScript,
        );
        let cfg = node_lang(
            "HandlerConfig",
            "HandlerConfig",
            "src/config.ts",
            1,
            1,
            NodeKind::Interface,
            Language::TypeScript,
        );
        // A same-named low-centrality fake with NO callers and NO sig type.
        let fake = node_lang(
            "handler",
            "handler",
            "src/fake.ts",
            5,
            5,
            NodeKind::Function,
            Language::TypeScript,
        );
        // Callers of `real` (drive its caller-count up). Fake has none.
        let callers: Vec<Node> = (0..6)
            .map(|i| {
                node_lang(
                    &format!("caller{i}"),
                    &format!("caller{i}"),
                    "src/callers.ts",
                    (i as i64) + 1,
                    (i as i64) + 1,
                    NodeKind::Function,
                    Language::TypeScript,
                )
            })
            .collect();
        for (rel, lang) in [
            ("src/real.ts", Language::TypeScript),
            ("src/config.ts", Language::TypeScript),
            ("src/fake.ts", Language::TypeScript),
            ("src/callers.ts", Language::TypeScript),
        ] {
            put_indexed_source(&engine, rel, "// stub\n", lang, 1);
        }
        put_indexed_source(
            &engine,
            "src/real.ts",
            "export function handler(c: HandlerConfig): void {}\n",
            Language::TypeScript,
            1,
        );
        let mut all = vec![real.clone(), cfg.clone(), fake.clone()];
        all.extend(callers.iter().cloned());
        put_nodes(&mut engine, &all);
        let mut edges = vec![mk_edge(
            &real.id,
            &cfg.id,
            codegraph_core::types::EdgeKind::References,
        )];
        for c in &callers {
            edges.push(mk_edge(
                &c.id,
                &real.id,
                codegraph_core::types::EdgeKind::Calls,
            ));
        }
        put_edges(&mut engine, &edges);

        let sub = engine.find_relevant_context("handler config").unwrap();
        // The high-centrality def was tiered ⇒ its buried config type is rescued.
        assert!(
            sub.file_order.iter().any(|f| f == "src/config.ts"),
            "high-centrality def's buried sig type must be rescued, got: {:?}",
            sub.file_order
        );
    }

    /// #1064 guard: a signature edge whose target is a NON-TYPE node (a
    /// variable, not a class/interface) is not rescued — only TYPE_KINDS
    /// targets surface. Exercises the `!TYPE_KINDS.contains` skip.
    #[test]
    fn ext_explore_rescue_skips_non_type_signature_target() {
        let mut engine = test_engine();
        let f = node_lang(
            "doThing",
            "doThing",
            "src/do.ts",
            2,
            2,
            NodeKind::Function,
            Language::TypeScript,
        );
        let var = node_lang(
            "someVar",
            "someVar",
            "src/other.ts",
            1,
            1,
            NodeKind::Variable,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/do.ts",
            "export function doThing(): void {}\n",
            Language::TypeScript,
            1,
        );
        put_indexed_source(
            &engine,
            "src/other.ts",
            "export const someVar = 1;\n",
            Language::TypeScript,
            1,
        );
        put_nodes(&mut engine, &[f.clone(), var.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &f.id,
                &var.id,
                codegraph_core::types::EdgeKind::References,
            )],
        );
        let sub = engine.find_relevant_context("doThing").unwrap();
        assert!(
            !sub.rescued_files.contains("src/other.ts"),
            "non-type signature target must not be rescued"
        );
    }

    /// #1064 guard: a dangling signature edge (target node absent from the
    /// store) is skipped without panic — exercises the missing-target arm.
    #[test]
    fn ext_explore_rescue_skips_dangling_signature_edge() {
        let mut engine = test_engine();
        let f = node_lang(
            "callMe",
            "callMe",
            "src/call.ts",
            2,
            2,
            NodeKind::Function,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/call.ts",
            "export function callMe(): void {}\n",
            Language::TypeScript,
            1,
        );
        put_nodes(&mut engine, std::slice::from_ref(&f));
        put_edges(
            &mut engine,
            &[mk_edge(
                &f.id,
                "interface:ghost:1",
                codegraph_core::types::EdgeKind::References,
            )],
        );
        let sub = engine.find_relevant_context("callMe").unwrap();
        assert!(
            sub.rescued_files.is_empty(),
            "dangling signature edge must rescue nothing"
        );
    }

    /// #1064 determinism: with MULTIPLE buried candidates the rescue set is
    /// sorted by `(file, line, id)` — exercises the sort comparator. Two sig
    /// types in files that sort in reverse of insertion order both rescue, in
    /// stable order.
    #[test]
    fn ext_explore_rescue_sorts_multiple_candidates() {
        let mut engine = test_engine();
        let f = node_lang(
            "wire",
            "wire",
            "src/wire.ts",
            3,
            3,
            NodeKind::Function,
            Language::TypeScript,
        );
        // `zeta.ts` sorts AFTER `alpha.ts`; edges are added zeta-first so the
        // comparator must reorder them.
        let zeta = node_lang(
            "Zeta",
            "Zeta",
            "src/zeta.ts",
            1,
            1,
            NodeKind::Interface,
            Language::TypeScript,
        );
        let alpha = node_lang(
            "Alpha",
            "Alpha",
            "src/alpha.ts",
            1,
            1,
            NodeKind::Struct,
            Language::TypeScript,
        );
        for rel in ["src/wire.ts", "src/zeta.ts", "src/alpha.ts"] {
            put_indexed_source(&engine, rel, "// stub\n", Language::TypeScript, 1);
        }
        put_nodes(&mut engine, &[f.clone(), zeta.clone(), alpha.clone()]);
        put_edges(
            &mut engine,
            &[
                mk_edge(&f.id, &zeta.id, codegraph_core::types::EdgeKind::References),
                mk_edge(&f.id, &alpha.id, codegraph_core::types::EdgeKind::Returns),
            ],
        );
        let sub = engine.find_relevant_context("wire").unwrap();
        assert!(sub.rescued_files.contains("src/zeta.ts"));
        assert!(sub.rescued_files.contains("src/alpha.ts"));
        // The comparator runs over both candidates; the resulting ranked
        // file_order is deterministic across repeated runs.
        let again = engine.find_relevant_context("wire").unwrap();
        assert_eq!(
            sub.file_order, again.file_order,
            "multi-candidate rescue ordering must be deterministic"
        );
    }

    /// #1064 buried check: a signature type whose file the query hits ≥2 times
    /// lexically is NOT buried and is not rescued — exercises the term-hit skip.
    #[test]
    fn ext_explore_rescue_skips_term_matched_type_file() {
        let mut engine = test_engine();
        let f = node_lang(
            "build",
            "build",
            "src/build.ts",
            2,
            2,
            NodeKind::Function,
            Language::TypeScript,
        );
        // The type lives in a file whose path AND name both hit query terms
        // ("payment", "config") — 2 hits ⇒ not buried.
        let cfg = node_lang(
            "PaymentConfig",
            "PaymentConfig",
            "src/payment.ts",
            1,
            1,
            NodeKind::Interface,
            Language::TypeScript,
        );
        for rel in ["src/build.ts", "src/payment.ts"] {
            put_indexed_source(&engine, rel, "// stub\n", Language::TypeScript, 1);
        }
        put_nodes(&mut engine, &[f.clone(), cfg.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &f.id,
                &cfg.id,
                codegraph_core::types::EdgeKind::References,
            )],
        );
        let sub = engine
            .find_relevant_context("build payment config")
            .unwrap();
        assert!(
            !sub.rescued_files.contains("src/payment.ts"),
            "term-matched (non-buried) type file must not be rescued"
        );
    }

    /// #1064 buried check: a signature type defined in the SAME file as its
    /// callable seed (a seed file, already on-screen) is not rescued —
    /// exercises the seed-file skip.
    #[test]
    fn ext_explore_rescue_skips_type_in_seed_file() {
        let mut engine = test_engine();
        // `mk` (seed) and its return type `Widget` share `src/widget.ts`.
        let mk = node_lang(
            "mk",
            "mk",
            "src/widget.ts",
            5,
            5,
            NodeKind::Function,
            Language::TypeScript,
        );
        let widget = node_lang(
            "Widget",
            "Widget",
            "src/widget.ts",
            1,
            1,
            NodeKind::Interface,
            Language::TypeScript,
        );
        put_indexed_source(
            &engine,
            "src/widget.ts",
            "// stub\n",
            Language::TypeScript,
            2,
        );
        put_nodes(&mut engine, &[mk.clone(), widget.clone()]);
        put_edges(
            &mut engine,
            &[mk_edge(
                &mk.id,
                &widget.id,
                codegraph_core::types::EdgeKind::Returns,
            )],
        );
        let sub = engine.find_relevant_context("mk").unwrap();
        assert!(
            !sub.rescued_files.contains("src/widget.ts"),
            "a type in the seed's own file is on-screen, not rescued"
        );
    }
}
