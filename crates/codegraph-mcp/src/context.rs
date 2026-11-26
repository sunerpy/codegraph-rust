//! `codegraph context` (W9-01): upstream `ContextBuilder.buildContext`
//! (`src/context/index.ts` and `src/context/formatter.ts` at `b635dd46`, plus
//! #2428 from `e8ddc4e2`), the CLI face of a task's context: the entry points,
//! the symbols around them, their code and the call paths among them, as
//! Markdown or JSON.
//!
//! The engine gathers ([`crate::engine::CodeGraphEngine::build_context`]):
//! the entry points are the first `searchLimit` seeds of explore's search,
//! with no `minScore` floor (OD-B81), and the subgraph is explore's walk at
//! context's limits (OD-B79). This module holds the rest as plain functions:
//! the options and the CLI's checks, colby's low-confidence judgment
//! (OD-B80), the order and the cut of the code blocks, the call paths, the
//! summary and the two formats. What differs from colby is KNOWN_DIFFS'
//! Tier-2 entry for `codegraph context`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use codegraph_graph::query::scoring::{extract_search_terms, is_distinctive_identifier};
use codegraph_graph::synthesized_hop::describe_synthesized_hop;
use regex::Regex;
use serde::Serialize;

/// The heading of the low-confidence note (upstream `LOW_CONFIDENCE_MARKER`,
/// `src/context/markers.ts` 19).
pub const LOW_CONFIDENCE_MARKER: &str = "### ⚠️ Low-confidence match";

/// The longest chain the call paths follow (`MAX_HOPS`, `context/index.ts`
/// e8ddc4e2 361).
const MAX_HOPS: usize = 6;

/// The call paths' depth-first steps at most (`budget`, e8ddc4e2 363).
const CALL_PATH_BUDGET: i64 = 2000;

/// What a cut code block ends with (`extractCodeBlocks`, b635dd46 1472-1477).
const TRUNCATED_TAIL: &str = "\n... (truncated) ...";

/// `codegraph context`'s options, upstream `DEFAULT_BUILD_OPTIONS`
/// (`context/index.ts` b635dd46 151-160) by default.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextOptions {
    /// The subgraph's node budget (`maxNodes`, `-n/--max-nodes`).
    pub max_nodes: usize,
    /// Code blocks at most (`maxCodeBlocks`).
    pub max_code_blocks: usize,
    /// UTF-16 code units a code block keeps before its cut
    /// (`maxCodeBlockSize`).
    pub max_code_block_size: usize,
    /// Whether code blocks are read (`includeCode`; `--no-code` turns it off).
    pub include_code: bool,
    pub format: ContextFormat,
    /// Entry points at most (`searchLimit`).
    pub search_limit: usize,
    /// The walk's depth (`traversalDepth`).
    pub traversal_depth: usize,
    /// Upstream's `minScore`, kept as the documented default only: the entry
    /// points are explore's search, whose scores are on another scale, so
    /// nothing is filtered by it (OD-B81).
    pub min_score: f64,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            max_nodes: 20,
            max_code_blocks: 5,
            max_code_block_size: 1500,
            include_code: true,
            format: ContextFormat::Markdown,
            search_limit: 3,
            traversal_depth: 1,
            min_score: 0.3,
        }
    }
}

/// `-f/--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFormat {
    Markdown,
    Json,
}

impl ContextFormat {
    /// `markdown` or `json`, compared exactly as upstream compares them
    /// (`src/bin/codegraph.ts` b635dd46 1440-1444): `JSON` is refused.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "markdown" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Upstream's sentence for an unknown `--format` (b635dd46 1442).
pub fn unknown_format_message(raw: &str) -> String {
    format!("Unknown format \"{raw}\" — use \"markdown\" or \"json\".")
}

/// Upstream's sentence for a `--max-nodes` that is not a positive integer
/// (b635dd46 1449).
pub fn max_nodes_message(raw: &str) -> String {
    format!("--max-nodes expects a positive integer, got \"{raw}\".")
}

/// `--max-nodes` as upstream reads it (b635dd46 1445-1452):
/// `parseInt(raw, 10)`, refused when that is `NaN` or below one. So leading
/// JavaScript whitespace and one sign are skipped and the leading decimal
/// digits are the number: `5abc` is 5 and `1.9` is 1 (OD-B81). A number past
/// `usize::MAX` saturates; the walk treats it as no limit, as upstream's
/// floating-point budget does.
pub fn parse_max_nodes(raw: &str) -> Option<usize> {
    let unsigned = raw.trim_start_matches(is_js_whitespace);
    let (negative, digits) = match unsigned.as_bytes().first() {
        Some(b'-') => (true, &unsigned[1..]),
        Some(b'+') => (false, &unsigned[1..]),
        _ => (false, unsigned),
    };
    let end = digits
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let value = digits.as_bytes()[..end]
        .iter()
        .fold(0usize, |value, digit| {
            value
                .saturating_mul(10)
                .saturating_add(usize::from(digit - b'0'))
        });
    // `-0` and every negative number are below one.
    (!negative && value >= 1).then_some(value)
}

/// JavaScript's `WhiteSpace` and `LineTerminator` code points, which
/// `parseInt` skips before the number.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// Upstream `extractSymbolsFromQuery`'s patterns (`context/index.ts` b635dd46
/// 52-141). JavaScript's `\b` and character classes are ASCII, and so are
/// these (`(?-u)`).
static CAMEL_CASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)\b([A-Z][a-z]+(?:[A-Z][a-z]*)*|[a-z]+(?:[A-Z][a-z]*)+)\b")
        .expect("camelCase pattern")
});
static SNAKE_CASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)\b([a-z][a-z0-9]*(?:_[a-z0-9]+)+)\b").expect("snake_case pattern")
});
static SCREAMING_SNAKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)\b([A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)\b").expect("SCREAMING_SNAKE pattern")
});
static ACRONYM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)\b([A-Z]{2,})\b").expect("acronym pattern"));
static DOTTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u)\b([a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)+)\b")
        .expect("dot.notation pattern")
});
static LOWERCASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)\b([a-z][a-z0-9]{2,})\b").expect("lowercase pattern"));

/// The common English words `extractSymbolsFromQuery` drops (b635dd46
/// 116-138).
const COMMON_WORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "from",
    "this",
    "that",
    "have",
    "been",
    "will",
    "would",
    "could",
    "should",
    "does",
    "done",
    "make",
    "made",
    "use",
    "used",
    "using",
    "work",
    "works",
    "find",
    "found",
    "show",
    "call",
    "called",
    "calling",
    "get",
    "set",
    "add",
    "all",
    "any",
    "how",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "not",
    "but",
    "are",
    "was",
    "were",
    "has",
    "had",
    "its",
    "can",
    "did",
    "may",
    "also",
    "into",
    "than",
    "then",
    "them",
    "each",
    "other",
    "some",
    "such",
    "only",
    "same",
    "about",
    "after",
    "before",
    "between",
    "through",
    "during",
    "without",
    "again",
    "further",
    "once",
    "here",
    "there",
    "both",
    "just",
    "more",
    "most",
    "very",
    "being",
    "having",
    "doing",
    "system",
    "need",
    "needs",
    "want",
    "wants",
    "like",
    "look",
    "change",
    "changes",
    "changed",
    "changing",
    "layer",
    "handle",
    "handles",
    "handling",
    "incoming",
    "outgoing",
    "data",
    "flow",
    "flows",
    "level",
    "levels",
    "request",
    "requests",
    "response",
    "responses",
    "implement",
    "implements",
    "implementation",
    "interface",
    "interfaces",
    "class",
    "classes",
    "method",
    "methods",
    "trigger",
    "triggers",
    "affected",
    "affect",
    "affects",
    "else",
    "code",
    "failing",
    "failed",
    "silently",
    "decide",
    "decides",
    "return",
    "returns",
    "returned",
    "take",
    "takes",
    "taken",
    "check",
    "checks",
    "checked",
    "create",
    "creates",
    "created",
    "read",
    "reads",
    "write",
    "writes",
    "written",
    "start",
    "starts",
    "stop",
    "stops",
    "run",
    "runs",
    "running",
];

/// The names a query seems to spell as code symbols, upstream
/// `extractSymbolsFromQuery` (b635dd46 52-141): camelCase and PascalCase
/// words, snake_case and SCREAMING_SNAKE identifiers, acronyms, dotted paths
/// and their parts, and plain lowercase words, once each in that order, the
/// common English words left out. The COBOL copybook names it also reads are
/// not ported (Rust indexes no COBOL).
pub fn extract_symbols_from_query(query: &str) -> Vec<String> {
    let mut symbols: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut add = |symbol: &str| {
        if seen.insert(symbol.to_string()) {
            symbols.push(symbol.to_string());
        }
    };
    let found = |pattern: &Regex| -> Vec<String> {
        pattern
            .captures_iter(query)
            .filter_map(|captures| captures.get(1))
            .map(|matched| matched.as_str().to_string())
            .collect()
    };
    for symbol in found(&CAMEL_CASE) {
        if symbol.len() >= 2 {
            add(&symbol);
        }
    }
    for symbol in found(&SNAKE_CASE) {
        if symbol.len() >= 3 {
            add(&symbol);
        }
    }
    for symbol in found(&SCREAMING_SNAKE) {
        add(&symbol);
    }
    for symbol in found(&ACRONYM) {
        add(&symbol);
    }
    for dotted in found(&DOTTED) {
        add(&dotted);
        for part in dotted.split('.') {
            if part.len() >= 2 {
                add(part);
            }
        }
    }
    for symbol in found(&LOWERCASE) {
        add(&symbol);
    }
    symbols.retain(|symbol| !COMMON_WORDS.contains(&symbol.to_ascii_lowercase().as_str()));
    symbols
}

/// Whether colby would call these entry points a low-confidence match for
/// `query` (`findRelevantContext`, `context/index.ts` b635dd46 1087-1116,
/// OD-B80): the query has at least two terms of three or more characters,
/// and no entry point is named as a distinctive identifier the query spells
/// or carries two of the terms in its name or its directory. A one-term
/// query and an empty answer are never low.
pub fn is_low_confidence(query: &str, entry_points: &[Node]) -> bool {
    let terms: Vec<String> = extract_search_terms(query, false)
        .into_iter()
        .filter(|term| utf16_len(term) >= 3)
        .collect();
    if terms.len() < 2 || entry_points.is_empty() {
        return false;
    }
    let distinctive: HashSet<String> = extract_symbols_from_query(query)
        .into_iter()
        .filter(|symbol| is_distinctive_identifier(symbol))
        .map(|symbol| symbol.to_lowercase())
        .collect();
    let strong = entry_points.iter().any(|node| {
        let name = node.name.to_lowercase();
        if distinctive.contains(&name) {
            return true;
        }
        let directory = posix_dirname(&node.file_path).to_lowercase();
        let segments: Vec<&str> = directory.split('/').collect();
        let mut hits = 0;
        for term in &terms {
            if name.contains(term.as_str()) || segments.contains(&term.as_str()) {
                hits += 1;
                if hits >= 2 {
                    return true;
                }
            }
        }
        false
    });
    !strong
}

/// Node's POSIX `path.dirname`: the path up to its last separator, trailing
/// separators ignored; `.` without one, `/` for a path directly under the
/// root.
fn posix_dirname(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".";
    }
    let has_root = bytes[0] == b'/';
    let mut end = None;
    let mut matched_slash = true;
    for at in (1..bytes.len()).rev() {
        if bytes[at] == b'/' {
            if !matched_slash {
                end = Some(at);
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    match end {
        None if has_root => "/",
        None => ".",
        Some(1) if has_root => "//",
        Some(end) => &path[..end],
    }
}

/// A string's length as JavaScript counts it: UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JavaScript's default `sort()` order for strings: by UTF-16 code units.
fn utf16_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// A node's code as a block, upstream's `CodeBlock`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub node: Node,
    /// The node's lines, cut at the block size.
    pub content: String,
}

/// The nodes whose code the blocks come from, in upstream's priority
/// (`extractCodeBlocks`, b635dd46 1437-1464): the entry points in their
/// order (one the walk dropped is skipped), then the other functions and
/// methods, then the other classes, each in the subgraph's order.
pub fn code_block_candidates<'n>(nodes: &'n [Node], roots: &[String]) -> Vec<&'n Node> {
    let by_id: HashMap<&str, &Node> = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let root_ids: HashSet<&str> = roots.iter().map(String::as_str).collect();
    let mut candidates: Vec<&Node> = roots
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect();
    candidates.extend(nodes.iter().filter(|node| {
        !root_ids.contains(node.id.as_str())
            && matches!(node.kind, NodeKind::Function | NodeKind::Method)
    }));
    candidates.extend(
        nodes
            .iter()
            .filter(|node| !root_ids.contains(node.id.as_str()) && node.kind == NodeKind::Class),
    );
    candidates
}

/// A YAML or properties key's block (#383, `extractNodeCode` b635dd46
/// 1389-1395): its signature, else its qualified name, else its name. The
/// file is never read: the key's line holds its value, routinely a secret.
pub fn config_leaf_code(node: &Node) -> &str {
    match node.signature.as_deref() {
        Some(signature) if !signature.is_empty() => signature,
        _ if !node.qualified_name.is_empty() => &node.qualified_name,
        _ => &node.name,
    }
}

/// `code` as a block (b635dd46 1472-1477): past `max` UTF-16 code units, the
/// longest prefix of at most `max` units and the truncation tail; a cut that
/// would split a surrogate pair stops before it.
pub fn truncate_code_block(code: &str, max: usize) -> String {
    let mut units = 0usize;
    let mut end = None;
    for (at, c) in code.char_indices() {
        if units + c.len_utf16() > max {
            end = Some(at);
            break;
        }
        units += c.len_utf16();
    }
    match end {
        Some(end) => format!("{}{TRUNCATED_TAIL}", &code[..end]),
        None => code.to_string(),
    }
}

/// What `buildContext` hands its formatters: the query, the subgraph in the
/// walk's order, the walk's entry points (one the walk dropped is no longer
/// a node), the code blocks, and whether the low-confidence note applies.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskContext {
    pub query: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub roots: Vec<String>,
    pub code_blocks: Vec<CodeBlock>,
    pub low_confidence: bool,
}

impl TaskContext {
    /// The entry points still in the subgraph, in rank order (upstream
    /// `getEntryPoints`, b635dd46 1421-1425).
    pub fn entry_points(&self) -> Vec<&Node> {
        let by_id: HashMap<&str, &Node> = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        self.roots
            .iter()
            .filter_map(|id| by_id.get(id.as_str()).copied())
            .collect()
    }

    /// The subgraph's files, once each, in JavaScript's sort order (upstream
    /// `getRelatedFiles`, b635dd46 1496-1502).
    pub fn related_files(&self) -> Vec<&str> {
        let mut files: Vec<&str> = self
            .nodes
            .iter()
            .map(|node| node.file_path.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        files.sort_by(|a, b| utf16_order(a, b));
        files
    }

    /// Upstream `generateSummary` (b635dd46 1504-1522).
    pub fn summary(&self) -> String {
        let entry_points = self.entry_points();
        let names = entry_points
            .iter()
            .take(3)
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let remaining = if entry_points.len() > 3 {
            format!(" and {} more", entry_points.len() - 3)
        } else {
            String::new()
        };
        format!(
            "Found {} relevant code symbols across {} files. Key entry points: {names}{remaining}. {} relationships identified.",
            self.nodes.len(),
            self.related_files().len(),
            self.edges.len()
        )
    }
}

/// The Markdown answer (upstream `formatContextAsMarkdown`, `formatter.ts`
/// 18-98, then `buildContext`'s two additions, `context/index.ts` b635dd46
/// 300-302): the query, the entry points, up to ten related symbols, the code,
/// the call paths, and the low-confidence note when it applies.
/// `is_generated` is upstream's `generatedPredicateFor`: entry points and
/// code of generated files come last, and their symbols are not listed.
pub fn format_markdown(context: &TaskContext, is_generated: &dyn Fn(&str) -> bool) -> String {
    let entry_points = context.entry_points();
    let mut lines: Vec<String> = vec![
        "## Code Context\n".to_string(),
        format!("**Query:** {}\n", context.query),
    ];
    let mut ordered = entry_points.clone();
    ordered.sort_by_key(|node| is_generated(&node.file_path));
    if !ordered.is_empty() {
        lines.push("### Entry Points\n".to_string());
        for node in &ordered {
            let location = if node.start_line != 0 {
                format!(":{}", node.start_line)
            } else {
                String::new()
            };
            lines.push(format!(
                "- **{}** ({}) - {}{location}",
                node.name,
                node.kind.as_str(),
                node.file_path
            ));
            if let Some(signature) = node.signature.as_deref().filter(|s| !s.is_empty()) {
                lines.push(format!("  `{signature}`"));
            }
        }
        lines.push(String::new());
    }

    let entry_ids: HashSet<&str> = entry_points.iter().map(|node| node.id.as_str()).collect();
    let related: Vec<&Node> = context
        .nodes
        .iter()
        .filter(|node| !entry_ids.contains(node.id.as_str()))
        .filter(|node| !is_generated(&node.file_path))
        .take(10)
        .collect();
    if !related.is_empty() {
        lines.push("### Related Symbols\n".to_string());
        let mut by_file: Vec<(&str, Vec<&Node>)> = Vec::new();
        for node in related {
            match by_file
                .iter_mut()
                .find(|(file, _)| *file == node.file_path.as_str())
            {
                Some((_, members)) => members.push(node),
                None => by_file.push((node.file_path.as_str(), vec![node])),
            }
        }
        for (file, members) in by_file {
            let list = members
                .iter()
                .map(|node| format!("{}:{}", node.name, node.start_line))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("- {file}: {list}"));
        }
        lines.push(String::new());
    }

    if !context.code_blocks.is_empty() {
        let mut blocks: Vec<&CodeBlock> = context.code_blocks.iter().collect();
        blocks.sort_by_key(|block| is_generated(&block.node.file_path));
        lines.push("### Code\n".to_string());
        for block in blocks {
            lines.push(format!(
                "#### {} ({}:{})\n",
                block.node.name, block.node.file_path, block.node.start_line
            ));
            lines.push(format!("```{}", block.node.language.as_str()));
            lines.push(block.content.clone());
            lines.push("```\n".to_string());
        }
    }

    let mut out = lines.join("\n");
    out.push_str(&call_paths_section(context));
    if context.low_confidence {
        out.push_str(&low_confidence_note(&entry_points));
    }
    out
}

/// The call paths among the subgraph's symbols (upstream
/// `buildCallPathsSection`, `context/index.ts` b635dd46 340-454 with #2428 from
/// `e8ddc4e2` 411-418 and 437): chains of three to [`MAX_HOPS`] symbols along
/// the `calls` edges between kept nodes, walked depth first from the first
/// five entry points that call anything (from every caller when there are no
/// entry points) within [`CALL_PATH_BUDGET`] steps, those through two or more
/// entry points, most entry points then longest first, a chain inside a kept
/// one dropped, three at most. A synthesized hop reads `→[<what bridged it>]`,
/// in `synthesized_hop`'s wording. Empty when no chain qualifies.
fn call_paths_section(context: &TaskContext) -> String {
    let by_id: HashMap<&str, &Node> = context
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut callers: Vec<&str> = Vec::new();
    for edge in &context.edges {
        if edge.kind != EdgeKind::Calls
            || !by_id.contains_key(edge.source.as_str())
            || !by_id.contains_key(edge.target.as_str())
        {
            continue;
        }
        match adjacency.get_mut(edge.source.as_str()) {
            Some(targets) => targets.push(edge.target.as_str()),
            None => {
                adjacency.insert(edge.source.as_str(), vec![edge.target.as_str()]);
                callers.push(edge.source.as_str());
            }
        }
    }
    if adjacency.is_empty() {
        return String::new();
    }

    let starts: Vec<&str> = if context.roots.is_empty() {
        callers.clone()
    } else {
        context
            .roots
            .iter()
            .map(String::as_str)
            .filter(|id| adjacency.contains_key(id))
            .collect()
    };
    let mut walk = ChainWalk {
        adjacency: &adjacency,
        budget: CALL_PATH_BUDGET,
        chains: Vec::new(),
    };
    for start in starts.into_iter().take(5) {
        let mut path = vec![start];
        let mut seen = HashSet::from([start]);
        walk.visit(start, &mut path, &mut seen);
    }
    if walk.chains.is_empty() {
        return String::new();
    }

    let root_ids: HashSet<&str> = context.roots.iter().map(String::as_str).collect();
    let root_count = |chain: &[&str]| chain.iter().filter(|id| root_ids.contains(*id)).count();
    let mut relevant: Vec<Vec<&str>> = walk
        .chains
        .into_iter()
        .filter(|chain| root_count(chain) >= 2)
        .collect();
    relevant.sort_by(|a, b| {
        root_count(b)
            .cmp(&root_count(a))
            .then_with(|| b.len().cmp(&a.len()))
    });
    let mut kept: Vec<Vec<&str>> = Vec::new();
    for chain in relevant {
        let key = chain.join(">");
        if kept.iter().any(|other| other.join(">").contains(&key)) {
            continue;
        }
        kept.push(chain);
        if kept.len() >= 3 {
            break;
        }
    }
    if kept.is_empty() {
        return String::new();
    }

    let name = |id: &str| {
        by_id
            .get(id)
            .map_or(id.to_string(), |node| node.name.clone())
    };
    let mut synthesized: HashMap<(&str, &str), String> = HashMap::new();
    for edge in &context.edges {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        if let Some(hop) = describe_synthesized_hop(edge) {
            synthesized.insert(
                (edge.source.as_str(), edge.target.as_str()),
                hop.call_path_label(),
            );
        }
    }
    let render = |chain: &[&str]| {
        let mut line = name(chain[0]);
        for pair in chain.windows(2) {
            match synthesized.get(&(pair[0], pair[1])) {
                Some(label) => line.push_str(&format!(" →[{label}] {}", name(pair[1]))),
                None => line.push_str(&format!(" → {}", name(pair[1]))),
            }
        }
        line
    };
    let has_synthesized = kept.iter().any(|chain| {
        chain
            .windows(2)
            .any(|pair| synthesized.contains_key(&(pair[0], pair[1])))
    });
    let mut lines = vec![
        String::new(),
        "## Call paths".to_string(),
        String::new(),
        "Execution flow among the key symbols (traced through the call graph):".to_string(),
        String::new(),
    ];
    lines.extend(kept.iter().map(|chain| format!("- {}", render(chain))));
    lines.push(String::new());
    lines.push(if has_synthesized {
        "_Hops marked `[…]` are dynamic dispatch bridged by codegraph (with the registration site); the rest are direct calls. codegraph_node any symbol for its body._".to_string()
    } else {
        "_codegraph_node any symbol above for its source + its own callers/callees._".to_string()
    });
    format!("\n{}\n", lines.join("\n"))
}

/// The call paths' depth-first walk over the subgraph's `calls` edges.
struct ChainWalk<'a> {
    adjacency: &'a HashMap<&'a str, Vec<&'a str>>,
    /// Steps left; every visit spends one, as upstream's `budget--` does.
    budget: i64,
    chains: Vec<Vec<&'a str>>,
}

impl<'a> ChainWalk<'a> {
    fn visit(&mut self, id: &'a str, path: &mut Vec<&'a str>, seen: &mut HashSet<&'a str>) {
        let left = self.budget;
        self.budget -= 1;
        if left <= 0 {
            return;
        }
        let next: Vec<&'a str> = self
            .adjacency
            .get(id)
            .map(|targets| {
                targets
                    .iter()
                    .copied()
                    .filter(|target| !seen.contains(target))
                    .collect()
            })
            .unwrap_or_default();
        if next.is_empty() || path.len() >= MAX_HOPS {
            if path.len() >= 3 {
                self.chains.push(path.clone());
            }
            return;
        }
        for target in next {
            seen.insert(target);
            path.push(target);
            self.visit(target, path, seen);
            path.pop();
            seen.remove(target);
        }
    }
}

/// The note that admits a weak match (upstream `buildLowConfidenceNote`,
/// `context/index.ts` b635dd46 310-338): it points at the precise tools and at
/// the first four directories of the entry points.
fn low_confidence_note(entry_points: &[&Node]) -> String {
    let mut directories: Vec<&str> = Vec::new();
    for node in entry_points {
        let path = node.file_path.as_str();
        let directory = match path.rfind('/') {
            Some(slash) if slash > 0 => &path[..slash],
            _ => path,
        };
        if !directories.contains(&directory) {
            directories.push(directory);
        }
        if directories.len() >= 4 {
            break;
        }
    }
    let directory_line = if directories.is_empty() {
        String::new()
    } else {
        format!(
            "\n- `codegraph_files` a likely area: {}",
            directories
                .iter()
                .map(|directory| format!("`{directory}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "\n\n{LOW_CONFIDENCE_MARKER}\n\nThis query matched mostly on common words, so the entry points above may be off-target — treat them as a starting point, not a complete answer. For a reliable result:\n- `codegraph_explore` with the **exact symbol names** you are after (class / function / method names), or\n- `codegraph_search <name>` for one specific symbol{directory_line}\n\nDo not assume the list above is comprehensive."
    )
}

/// The JSON answer (upstream `formatContextAsJson`, `formatter.ts` 105-127,
/// `JSON.stringify(…, null, 2)`): the query, the summary, the entry points,
/// the subgraph, the code blocks, the related files and the counts. As
/// upstream's rows read (`rowToNode`, `rowToEdge`, `db/queries.ts` b635dd46
/// 171-210), a missing signature, docstring, line or column leaves its key
/// out, and a missing visibility is `null`. Entry points and code blocks keep
/// their order; the call paths and the note are Markdown only.
pub fn format_json(context: &TaskContext) -> serde_json::Result<String> {
    let entry_points = context.entry_points();
    let related_files = context.related_files();
    let code_blocks: Vec<JsonCodeBlock<'_>> = context
        .code_blocks
        .iter()
        .map(|block| JsonCodeBlock {
            file_path: &block.node.file_path,
            start_line: block.node.start_line,
            end_line: block.node.end_line,
            language: block.node.language.as_str(),
            content: &block.content,
            node_name: &block.node.name,
            node_kind: block.node.kind.as_str(),
        })
        .collect();
    let stats = JsonStats {
        node_count: context.nodes.len(),
        edge_count: context.edges.len(),
        file_count: related_files.len(),
        code_block_count: code_blocks.len(),
        total_code_size: context
            .code_blocks
            .iter()
            .map(|block| utf16_len(&block.content))
            .sum(),
    };
    serde_json::to_string_pretty(&JsonContext {
        query: &context.query,
        summary: context.summary(),
        entry_points: entry_points.into_iter().map(JsonNode::from).collect(),
        nodes: context.nodes.iter().map(JsonNode::from).collect(),
        edges: context.edges.iter().map(JsonEdge::from).collect(),
        code_blocks,
        related_files,
        stats,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonContext<'a> {
    query: &'a str,
    summary: String,
    entry_points: Vec<JsonNode<'a>>,
    nodes: Vec<JsonNode<'a>>,
    edges: Vec<JsonEdge<'a>>,
    code_blocks: Vec<JsonCodeBlock<'a>>,
    related_files: Vec<&'a str>,
    stats: JsonStats,
}

/// Upstream `serializeNode` (`formatter.ts` 245-262).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonNode<'a> {
    id: &'a str,
    kind: &'a str,
    name: &'a str,
    qualified_name: &'a str,
    file_path: &'a str,
    language: &'a str,
    start_line: i64,
    end_line: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    docstring: Option<&'a str>,
    visibility: Option<&'a str>,
    is_exported: bool,
    is_async: bool,
    is_static: bool,
}

impl<'a> From<&'a Node> for JsonNode<'a> {
    fn from(node: &'a Node) -> Self {
        Self {
            id: &node.id,
            kind: node.kind.as_str(),
            name: &node.name,
            qualified_name: &node.qualified_name,
            file_path: &node.file_path,
            language: node.language.as_str(),
            start_line: node.start_line,
            end_line: node.end_line,
            signature: node.signature.as_deref(),
            docstring: node.docstring.as_deref(),
            visibility: node.visibility.as_deref(),
            is_exported: node.is_exported,
            is_async: node.is_async,
            is_static: node.is_static,
        }
    }
}

/// Upstream `serializeEdge` (`formatter.ts` 264-275).
#[derive(Serialize)]
struct JsonEdge<'a> {
    source: &'a str,
    target: &'a str,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<i64>,
}

impl<'a> From<&'a Edge> for JsonEdge<'a> {
    fn from(edge: &'a Edge) -> Self {
        Self {
            source: &edge.source,
            target: &edge.target,
            kind: edge.kind.as_str(),
            line: edge.line,
            column: edge.col,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonCodeBlock<'a> {
    file_path: &'a str,
    start_line: i64,
    end_line: i64,
    language: &'a str,
    content: &'a str,
    node_name: &'a str,
    node_kind: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonStats {
    node_count: usize,
    edge_count: usize,
    file_count: usize,
    code_block_count: usize,
    total_code_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::Language;
    use serde_json::json;

    #[allow(clippy::too_many_arguments)]
    fn node(
        id: &str,
        kind: NodeKind,
        name: &str,
        qualified: &str,
        file: &str,
        lines: (i64, i64),
        signature: Option<&str>,
        exported: bool,
    ) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: qualified.to_string(),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: signature.map(str::to_string),
            visibility: None,
            is_exported: exported,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn function(name: &str, file: &str, line: i64) -> Node {
        node(
            &format!("function:{file}:{name}"),
            NodeKind::Function,
            name,
            name,
            file,
            (line, line + 2),
            None,
            false,
        )
    }

    fn edge(source: &Node, target: &Node, kind: EdgeKind, at: Option<(i64, i64)>) -> Edge {
        Edge {
            id: None,
            source: source.id.clone(),
            target: target.id.clone(),
            kind,
            metadata: None,
            line: at.map(|(line, _)| line),
            col: at.map(|(_, column)| column),
            provenance: None,
        }
    }

    fn calls(source: &Node, target: &Node) -> Edge {
        edge(source, target, EdgeKind::Calls, None)
    }

    fn context(nodes: &[Node], edges: Vec<Edge>, roots: &[&Node]) -> TaskContext {
        TaskContext {
            query: "q".to_string(),
            nodes: nodes.to_vec(),
            edges,
            roots: roots.iter().map(|node| node.id.clone()).collect(),
            code_blocks: Vec::new(),
            low_confidence: false,
        }
    }

    fn not_generated(_: &str) -> bool {
        false
    }

    /// The mini fixture's `Counter add runDemo` context, as upstream's
    /// `e8ddc4e2` build gathers it (`W9/prestart/context/probe/upstream/
    /// mini.q1.json.stdout`).
    fn mini() -> TaskContext {
        let run_demo = node(
            "function:60629aa3876961b8bd3c07c43bbe6a37",
            NodeKind::Function,
            "runDemo",
            "runDemo",
            "src/app.ts",
            (3, 7),
            Some("(): number"),
            true,
        );
        let add = node(
            "function:cce15011e0125d59f6bef014ae79c04f",
            NodeKind::Function,
            "add",
            "add",
            "src/math.ts",
            (1, 3),
            Some("(left: number, right: number): number"),
            true,
        );
        let counter = node(
            "class:7ac92ae0dce4208dfad9148e025726c4",
            NodeKind::Class,
            "Counter",
            "Counter",
            "src/math.ts",
            (5, 12),
            None,
            true,
        );
        let increment = node(
            "method:f501ba98441869bd251c636e30d31e3d",
            NodeKind::Method,
            "increment",
            "Counter::increment",
            "src/math.ts",
            (8, 11),
            Some("(step: number = 1): number"),
            false,
        );
        let increment_code = "  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }";
        let blocks = [
            (
                &run_demo,
                "export function runDemo(): number {\n  const counter = new Counter();\n  counter.increment(add(1, 2));\n  return counter.increment();\n}".to_string(),
            ),
            (
                &add,
                "export function add(left: number, right: number): number {\n  return left + right;\n}".to_string(),
            ),
            (
                &counter,
                format!("export class Counter {{\n  private value = 0;\n\n{increment_code}\n}}"),
            ),
            (&increment, increment_code.to_string()),
        ];
        TaskContext {
            query: "Counter add runDemo".to_string(),
            code_blocks: blocks
                .into_iter()
                .map(|(node, content)| CodeBlock {
                    node: node.clone(),
                    content,
                })
                .collect(),
            edges: vec![
                edge(&run_demo, &add, EdgeKind::Calls, Some((5, 20))),
                edge(&run_demo, &increment, EdgeKind::Calls, Some((5, 2))),
                edge(&run_demo, &counter, EdgeKind::Instantiates, Some((4, 18))),
                edge(&increment, &add, EdgeKind::Calls, Some((9, 17))),
                edge(&counter, &increment, EdgeKind::Contains, None),
            ],
            roots: vec![run_demo.id.clone(), add.id.clone(), counter.id.clone()],
            nodes: vec![run_demo, add, counter, increment],
            low_confidence: false,
        }
    }

    #[test]
    fn the_defaults_are_upstream_s() {
        let options = ContextOptions::default();
        assert_eq!(options.max_nodes, 20);
        assert_eq!(options.max_code_blocks, 5);
        assert_eq!(options.max_code_block_size, 1500);
        assert!(options.include_code);
        assert_eq!(options.format, ContextFormat::Markdown);
        assert_eq!(options.search_limit, 3);
        assert_eq!(options.traversal_depth, 1);
        assert!((options.min_score - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn the_format_is_markdown_or_json_exactly() {
        assert_eq!(
            ContextFormat::parse("markdown"),
            Some(ContextFormat::Markdown)
        );
        assert_eq!(ContextFormat::parse("json"), Some(ContextFormat::Json));
        for refused in ["JSON", "Markdown", "yaml", "", " json", "md"] {
            assert_eq!(ContextFormat::parse(refused), None, "{refused:?}");
        }
        assert_eq!(
            unknown_format_message("yaml"),
            "Unknown format \"yaml\" — use \"markdown\" or \"json\"."
        );
        assert_eq!(
            max_nodes_message("-5"),
            "--max-nodes expects a positive integer, got \"-5\"."
        );
    }

    /// OD-B81: upstream's `parseInt(raw, 10)`, then `NaN` or below one is
    /// refused (`probe/upstream-cli/summary.tsv` rows 3-8).
    #[test]
    fn max_nodes_reads_its_leading_number_as_parse_int_does() {
        for (raw, want) in [
            ("8", Some(8)),
            ("+8", Some(8)),
            (" 8", Some(8)),
            ("\t\n8", Some(8)),
            ("\u{a0}\u{feff}\u{3000}8", Some(8)),
            ("8 ", Some(8)),
            ("5abc", Some(5)),
            ("1.9", Some(1)),
            ("1e3", Some(1)),
            ("007", Some(7)),
            ("0", None),
            ("-0", None),
            ("-5", None),
            ("abc", None),
            ("", None),
            ("+", None),
            ("-", None),
            ("+-1", None),
            (" ", None),
            ("0x10", None),
            ("\u{85}8", None),
            ("\u{663}", None),
        ] {
            assert_eq!(parse_max_nodes(raw), want, "{raw:?}");
        }
        assert_eq!(
            parse_max_nodes("99999999999999999999999999"),
            Some(usize::MAX)
        );
    }

    #[test]
    fn the_query_s_symbols_are_extract_symbols_from_query_s() {
        assert_eq!(
            extract_symbols_from_query(
                "how does UserService call parse_token, MAX_RETRIES and app.isPackaged over REST"
            ),
            [
                "UserService",
                "isPackaged",
                "parse_token",
                "MAX_RETRIES",
                "REST",
                "app.isPackaged",
                "app",
                "over",
            ]
        );
        // JavaScript's `\b` and classes are ASCII: `é` ends a word.
        assert_eq!(extract_symbols_from_query("café flat"), ["caf", "flat"]);
        assert_eq!(
            extract_symbols_from_query("the flow and the data"),
            Vec::<String>::new()
        );
        // XMLHttpRequest has no word boundary where a hump starts.
        assert_eq!(
            extract_symbols_from_query("XMLHttpRequest"),
            Vec::<String>::new()
        );
    }

    /// Upstream `__tests__/context-ranking.test.ts` (e8ddc4e2 108-189) and
    /// `probe/upstream/ctxrank.*.confidence.txt`.
    #[test]
    fn low_confidence_follows_upstream_s_rule() {
        let screen = function("CaptureIntroScreen", "src/app/capture/intro.tsx", 1);
        let mut flat = function("FLAT", "scripts/dataset/download.ts", 1);
        flat.kind = NodeKind::Constant;
        assert!(!is_low_confidence(
            "capture intro onboarding screen flat object",
            &[flat.clone(), screen.clone()]
        ));
        assert!(is_low_confidence(
            "flat object thing",
            std::slice::from_ref(&flat)
        ));
        assert!(!is_low_confidence(
            "CaptureIntroScreen",
            std::slice::from_ref(&screen)
        ));
        // One term, or nothing found: never low.
        assert!(!is_low_confidence("flat", std::slice::from_ref(&flat)));
        assert!(!is_low_confidence("flat object thing", &[]));
        // A distinctive identifier the query spells is enough by itself.
        assert!(!is_low_confidence(
            "FLAT object thing",
            std::slice::from_ref(&flat)
        ));
        // Two terms in the name and the directory corroborate an entry.
        assert!(!is_low_confidence(
            "dataset flat thing",
            std::slice::from_ref(&flat)
        ));
        assert!(is_low_confidence("datasets flat thing", &[flat]));
    }

    #[test]
    fn dirname_is_node_s_posix_dirname() {
        for (path, want) in [
            ("a/b/c.ts", "a/b"),
            ("c.ts", "."),
            ("", "."),
            ("/c.ts", "/"),
            ("a/", "."),
            ("a/b/", "a"),
            ("//a", "//"),
        ] {
            assert_eq!(posix_dirname(path), want, "{path:?}");
        }
    }

    #[test]
    fn code_blocks_come_in_upstream_s_priority() {
        let mut class = function("Shape", "src/shape.ts", 1);
        class.kind = NodeKind::Class;
        let method = node(
            "method:area",
            NodeKind::Method,
            "area",
            "Shape::area",
            "src/shape.ts",
            (2, 4),
            None,
            false,
        );
        let mut variable = function("config", "src/config.ts", 1);
        variable.kind = NodeKind::Variable;
        let helper = function("helper", "src/helper.ts", 1);
        let entry = function("entry", "src/entry.ts", 1);
        let nodes = [
            class.clone(),
            variable.clone(),
            method.clone(),
            helper.clone(),
            entry.clone(),
        ];
        let dropped = "function:gone".to_string();
        let roots = vec![entry.id.clone(), dropped, variable.id.clone()];
        let order: Vec<&str> = code_block_candidates(&nodes, &roots)
            .into_iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(order, ["entry", "config", "area", "helper", "Shape"]);
    }

    #[test]
    fn a_config_leaf_s_block_is_its_key() {
        let mut key = function("password", "src/main/resources/application.properties", 1);
        key.qualified_name = "db.password".to_string();
        key.signature = Some("db.password".to_string());
        assert_eq!(config_leaf_code(&key), "db.password");
        key.signature = Some(String::new());
        assert_eq!(config_leaf_code(&key), "db.password");
        key.qualified_name = String::new();
        assert_eq!(config_leaf_code(&key), "password");
    }

    #[test]
    fn a_long_block_is_cut_at_1500_utf16_units() {
        let exact = "a".repeat(1500);
        assert_eq!(truncate_code_block(&exact, 1500), exact);
        let long = "b".repeat(1501);
        assert_eq!(
            truncate_code_block(&long, 1500),
            format!("{}\n... (truncated) ...", "b".repeat(1500))
        );
        // `é` is one unit, `😀` two: 1499 + 2 units go past 1500, and the
        // pair is not split.
        let pair = format!("{}😀", "é".repeat(1499));
        assert_eq!(utf16_len(&pair), 1501);
        assert_eq!(
            truncate_code_block(&pair, 1500),
            format!("{}\n... (truncated) ...", "é".repeat(1499))
        );
        let fits = format!("{}😀", "é".repeat(1498));
        assert_eq!(truncate_code_block(&fits, 1500), fits);
        assert_eq!(truncate_code_block("abc", 0), "\n... (truncated) ...");
    }

    #[test]
    fn the_summary_is_upstream_s_template() {
        let empty = context(&[], Vec::new(), &[]);
        assert_eq!(
            empty.summary(),
            "Found 0 relevant code symbols across 0 files. Key entry points: . 0 relationships identified."
        );
        assert_eq!(
            mini().summary(),
            "Found 4 relevant code symbols across 2 files. Key entry points: runDemo, add, Counter. 5 relationships identified."
        );
        let five: Vec<Node> = (1..=5)
            .map(|at| function(&format!("f{at}"), "src/a.ts", at * 10))
            .collect();
        let roots: Vec<&Node> = five.iter().collect();
        assert_eq!(
            context(&five, Vec::new(), &roots).summary(),
            "Found 5 relevant code symbols across 1 files. Key entry points: f1, f2, f3 and 2 more. 0 relationships identified."
        );
    }

    /// The Markdown layout, byte for byte as upstream's `e8ddc4e2` build
    /// prints the mini context (`probe/upstream/mini.q1.md.stdout`, less
    /// `console.log`'s newline).
    #[test]
    fn markdown_matches_upstream_s_mini_answer() {
        let want = "## Code Context\n\n**Query:** Counter add runDemo\n\n### Entry Points\n\n- **runDemo** (function) - src/app.ts:3\n  `(): number`\n- **add** (function) - src/math.ts:1\n  `(left: number, right: number): number`\n- **Counter** (class) - src/math.ts:5\n\n### Related Symbols\n\n- src/math.ts: increment:8\n\n### Code\n\n#### runDemo (src/app.ts:3)\n\n```typescript\nexport function runDemo(): number {\n  const counter = new Counter();\n  counter.increment(add(1, 2));\n  return counter.increment();\n}\n```\n\n#### add (src/math.ts:1)\n\n```typescript\nexport function add(left: number, right: number): number {\n  return left + right;\n}\n```\n\n#### Counter (src/math.ts:5)\n\n```typescript\nexport class Counter {\n  private value = 0;\n\n  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }\n}\n```\n\n#### increment (src/math.ts:8)\n\n```typescript\n  increment(step: number = 1): number {\n    this.value = add(this.value, step);\n    return this.value;\n  }\n```\n\n\n## Call paths\n\nExecution flow among the key symbols (traced through the call graph):\n\n- runDemo → increment → add\n\n_codegraph_node any symbol above for its source + its own callers/callees._\n";
        assert_eq!(format_markdown(&mini(), &not_generated), want);
        let mut no_code = mini();
        no_code.code_blocks.clear();
        let markdown = format_markdown(&no_code, &not_generated);
        assert!(
            markdown.contains("- src/math.ts: increment:8\n\n\n## Call paths\n"),
            "{markdown}"
        );
        assert!(!markdown.contains("### Code"), "{markdown}");
    }

    #[test]
    fn markdown_puts_generated_files_last_and_leaves_their_symbols_out() {
        let generated = function("Marshal", "api/user.pb.go", 0);
        let plain = function("Handle", "api/user.go", 4);
        let other = function("Helper", "api/helper.go", 9);
        let mut proto_helper = function("Size", "api/user.pb.go", 7);
        proto_helper.signature = Some(String::new());
        let mut ctx = context(
            &[
                generated.clone(),
                plain.clone(),
                other.clone(),
                proto_helper,
            ],
            Vec::new(),
            &[&generated, &plain],
        );
        ctx.code_blocks = vec![
            CodeBlock {
                node: generated.clone(),
                content: "func Marshal() {}".to_string(),
            },
            CodeBlock {
                node: plain.clone(),
                content: "func Handle() {}".to_string(),
            },
        ];
        let is_generated = |path: &str| path.ends_with(".pb.go");
        assert_eq!(
            format_markdown(&ctx, &is_generated),
            "## Code Context\n\n**Query:** q\n\n### Entry Points\n\n- **Handle** (function) - api/user.go:4\n- **Marshal** (function) - api/user.pb.go\n\n### Related Symbols\n\n- api/helper.go: Helper:9\n\n### Code\n\n#### Handle (api/user.go:4)\n\n```typescript\nfunc Handle() {}\n```\n\n#### Marshal (api/user.pb.go:0)\n\n```typescript\nfunc Marshal() {}\n```\n"
        );
    }

    #[test]
    fn markdown_lists_ten_related_symbols_by_file_in_their_order() {
        let entry = function("entry", "src/a.ts", 1);
        let mut nodes = vec![entry.clone()];
        for at in 0..12 {
            let file = if at % 2 == 0 {
                "src/even.ts"
            } else {
                "src/odd.ts"
            };
            nodes.push(function(&format!("s{at}"), file, at + 1));
        }
        let markdown = format_markdown(&context(&nodes, Vec::new(), &[&entry]), &not_generated);
        assert!(
            markdown.ends_with("\n\n### Related Symbols\n\n- src/even.ts: s0:1, s2:3, s4:5, s6:7, s8:9\n- src/odd.ts: s1:2, s3:4, s5:6, s7:8, s9:10\n"),
            "{markdown}"
        );
    }

    /// `JSON.stringify(…, null, 2)` of upstream's serialized context: a
    /// missing signature, docstring, line or column has no key, a missing
    /// visibility is `null`, the files sort by UTF-16 code units and the code
    /// size counts them.
    #[test]
    fn json_follows_upstream_s_keys_and_omissions() {
        let mut full = function("run", "src/\u{1F600}.ts", 2);
        full.signature = Some("(): void".to_string());
        full.docstring = Some("Runs.".to_string());
        full.visibility = Some("public".to_string());
        full.is_async = true;
        let bare = function("bare", "src/\u{FF5E}.ts", 5);
        let mut ctx = context(
            &[bare.clone(), full.clone()],
            vec![
                edge(&full, &bare, EdgeKind::Calls, Some((3, 4))),
                edge(&bare, &full, EdgeKind::References, None),
            ],
            &[&full, &bare],
        );
        ctx.code_blocks = vec![CodeBlock {
            node: full.clone(),
            content: "run(😀)".to_string(),
        }];
        let want = json!({
            "query": "q",
            "summary": "Found 2 relevant code symbols across 2 files. Key entry points: run, bare. 2 relationships identified.",
            "entryPoints": [
                {"id": full.id, "kind": "function", "name": "run", "qualifiedName": "run", "filePath": "src/\u{1F600}.ts", "language": "typescript", "startLine": 2, "endLine": 4, "signature": "(): void", "docstring": "Runs.", "visibility": "public", "isExported": false, "isAsync": true, "isStatic": false},
                {"id": bare.id, "kind": "function", "name": "bare", "qualifiedName": "bare", "filePath": "src/\u{FF5E}.ts", "language": "typescript", "startLine": 5, "endLine": 7, "visibility": null, "isExported": false, "isAsync": false, "isStatic": false}
            ],
            "nodes": [
                {"id": bare.id, "kind": "function", "name": "bare", "qualifiedName": "bare", "filePath": "src/\u{FF5E}.ts", "language": "typescript", "startLine": 5, "endLine": 7, "visibility": null, "isExported": false, "isAsync": false, "isStatic": false},
                {"id": full.id, "kind": "function", "name": "run", "qualifiedName": "run", "filePath": "src/\u{1F600}.ts", "language": "typescript", "startLine": 2, "endLine": 4, "signature": "(): void", "docstring": "Runs.", "visibility": "public", "isExported": false, "isAsync": true, "isStatic": false}
            ],
            "edges": [
                {"source": full.id, "target": bare.id, "kind": "calls", "line": 3, "column": 4},
                {"source": bare.id, "target": full.id, "kind": "references"}
            ],
            "codeBlocks": [
                {"filePath": "src/\u{1F600}.ts", "startLine": 2, "endLine": 4, "language": "typescript", "content": "run(😀)", "nodeName": "run", "nodeKind": "function"}
            ],
            "relatedFiles": ["src/\u{1F600}.ts", "src/\u{FF5E}.ts"],
            "stats": {"nodeCount": 2, "edgeCount": 2, "fileCount": 2, "codeBlockCount": 1, "totalCodeSize": 7}
        });
        let rendered = format_json(&ctx).unwrap();
        // serde_json's `json!` keeps no key order, so compare the parsed value
        // and then the order of the keys as written.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            want
        );
        assert!(
            rendered.starts_with("{\n  \"query\": \"q\",\n  \"summary\": "),
            "{rendered}"
        );
        assert!(
            rendered.contains("\n      \"startLine\": 5,\n      \"endLine\": 7,\n      \"visibility\": null,\n      \"isExported\": false,"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\"kind\": \"references\"\n    }"),
            "{rendered}"
        );
        let keys: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("  \"") && !line.starts_with("   "))
            .map(|line| line.trim_start().split('"').nth(1).unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "query",
                "summary",
                "entryPoints",
                "nodes",
                "edges",
                "codeBlocks",
                "relatedFiles",
                "stats"
            ]
        );
        assert!(
            !rendered.contains("## Call paths") && !rendered.contains(LOW_CONFIDENCE_MARKER),
            "{rendered}"
        );
    }

    #[test]
    fn json_writes_empty_lists_for_an_empty_answer() {
        assert_eq!(
            format_json(&context(&[], Vec::new(), &[])).unwrap(),
            "{\n  \"query\": \"q\",\n  \"summary\": \"Found 0 relevant code symbols across 0 files. Key entry points: . 0 relationships identified.\",\n  \"entryPoints\": [],\n  \"nodes\": [],\n  \"edges\": [],\n  \"codeBlocks\": [],\n  \"relatedFiles\": [],\n  \"stats\": {\n    \"nodeCount\": 0,\n    \"edgeCount\": 0,\n    \"fileCount\": 0,\n    \"codeBlockCount\": 0,\n    \"totalCodeSize\": 0\n  }\n}"
        );
    }

    /// Upstream `__tests__/synthesized-hop.test.ts` (e8ddc4e2 18-83): the
    /// interface hop reads inside the arrow, and the footnote says so.
    #[test]
    fn a_synthesized_hop_reads_inside_the_arrow() {
        let run = function("runExport", "src/run.ts", 3);
        let iface = node(
            "method:iface",
            NodeKind::Method,
            "exportReport",
            "Exporter::exportReport",
            "src/exporter.ts",
            (2, 2),
            None,
            false,
        );
        let implementation = node(
            "method:impl",
            NodeKind::Method,
            "exportReport",
            "PdfExporter::exportReport",
            "src/pdf-exporter.ts",
            (5, 7),
            None,
            false,
        );
        let render = function("renderPdf", "src/render.ts", 1);
        let mut hop = calls(&iface, &implementation);
        hop.provenance = Some("heuristic".to_string());
        hop.metadata = Some(json!({
            "synthesizedBy": "interface-impl",
            "via": "exportReport",
            "registeredAt": "src/pdf-exporter.ts:5"
        }));
        let ctx = context(
            &[
                run.clone(),
                render.clone(),
                implementation.clone(),
                iface.clone(),
            ],
            vec![calls(&run, &iface), hop, calls(&implementation, &render)],
            &[&run, &render, &implementation],
        );
        assert_eq!(
            call_paths_section(&ctx),
            "\n\n## Call paths\n\nExecution flow among the key symbols (traced through the call graph):\n\n- runExport → exportReport →[interface → impl @src/pdf-exporter.ts:5] exportReport → renderPdf\n\n_Hops marked `[…]` are dynamic dispatch bridged by codegraph (with the registration site); the rest are direct calls. codegraph_node any symbol for its body._\n"
        );
    }

    #[test]
    fn call_paths_need_three_symbols_two_entry_points_and_calls() {
        let a = function("a", "src/a.ts", 1);
        let b = function("b", "src/b.ts", 1);
        let c = function("c", "src/c.ts", 1);
        let nodes = [a.clone(), b.clone(), c.clone()];
        // Two symbols are one call, not a flow.
        assert_eq!(
            call_paths_section(&context(&nodes, vec![calls(&a, &b)], &[&a, &b])),
            ""
        );
        // One entry point on the chain is not enough.
        assert_eq!(
            call_paths_section(&context(&nodes, vec![calls(&a, &b), calls(&b, &c)], &[&a])),
            ""
        );
        // Only `calls` edges count.
        assert_eq!(
            call_paths_section(&context(
                &nodes,
                vec![
                    edge(&a, &b, EdgeKind::References, None),
                    edge(&b, &c, EdgeKind::References, None)
                ],
                &[&a, &c]
            )),
            ""
        );
        // No entry points: no chain has two.
        assert_eq!(
            call_paths_section(&context(&nodes, vec![calls(&a, &b), calls(&b, &c)], &[])),
            ""
        );
        // An edge to a node the subgraph does not hold is not followed.
        let gone = function("gone", "src/gone.ts", 1);
        assert_eq!(
            call_paths_section(&context(
                &[a.clone(), b.clone()],
                vec![calls(&a, &b), calls(&b, &gone)],
                &[&a, &gone]
            )),
            ""
        );
        assert_eq!(
            call_paths_section(&context(
                &nodes,
                vec![calls(&a, &b), calls(&b, &c)],
                &[&a, &c]
            )),
            "\n\n## Call paths\n\nExecution flow among the key symbols (traced through the call graph):\n\n- a → b → c\n\n_codegraph_node any symbol above for its source + its own callers/callees._\n"
        );
    }

    fn chain_lines(section: &str) -> Vec<&str> {
        section
            .lines()
            .filter(|line| line.starts_with("- "))
            .collect()
    }

    #[test]
    fn a_chain_stops_at_six_symbols() {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let nodes: Vec<Node> = names
            .iter()
            .map(|name| function(name, "src/x.ts", 1))
            .collect();
        let edges = nodes
            .windows(2)
            .map(|pair| calls(&pair[0], &pair[1]))
            .collect();
        let section = call_paths_section(&context(&nodes, edges, &[&nodes[0], &nodes[2]]));
        assert_eq!(chain_lines(&section), ["- a → b → c → d → e → f"]);
    }

    #[test]
    fn a_chain_inside_a_kept_one_is_dropped_and_three_are_kept() {
        let names = ["a", "b", "c", "d"];
        let nodes: Vec<Node> = names
            .iter()
            .map(|name| function(name, "src/x.ts", 1))
            .collect();
        let edges = nodes
            .windows(2)
            .map(|pair| calls(&pair[0], &pair[1]))
            .collect();
        let section =
            call_paths_section(&context(&nodes, edges, &[&nodes[0], &nodes[1], &nodes[3]]));
        assert_eq!(chain_lines(&section), ["- a → b → c → d"]);

        let hub = function("hub", "src/hub.ts", 1);
        let mut nodes = vec![hub.clone()];
        let mut edges = Vec::new();
        let mut roots = vec![hub.id.clone()];
        for at in 1..=4 {
            let middle = function(&format!("m{at}"), "src/m.ts", at);
            let end = function(&format!("e{at}"), "src/e.ts", at);
            edges.push(calls(&hub, &middle));
            edges.push(calls(&middle, &end));
            roots.push(end.id.clone());
            nodes.push(middle);
            nodes.push(end);
        }
        let mut ctx = context(&nodes, edges, &[]);
        ctx.roots = roots;
        assert_eq!(
            chain_lines(&call_paths_section(&ctx)),
            ["- hub → m1 → e1", "- hub → m2 → e2", "- hub → m3 → e3"]
        );
    }

    /// The chains through the most entry points lead, then the longest; only
    /// the first five entry points that call anything start a walk.
    #[test]
    fn chains_rank_by_entry_points_then_length_from_five_starts() {
        let target = function("target", "src/t.ts", 1);
        let mut nodes = vec![target.clone()];
        let mut edges = Vec::new();
        let mut roots = Vec::new();
        for at in 1..=6 {
            let start = function(&format!("s{at}"), "src/s.ts", at);
            let middle = function(&format!("m{at}"), "src/m.ts", at);
            edges.push(calls(&start, &middle));
            if at == 6 {
                // The sixth start's chain would be the longest.
                let extra = function("n6", "src/n.ts", 6);
                edges.push(calls(&middle, &extra));
                edges.push(calls(&extra, &target));
                nodes.push(extra);
            } else {
                edges.push(calls(&middle, &target));
            }
            roots.push(start.id.clone());
            nodes.push(start);
            nodes.push(middle);
        }
        roots.push(target.id.clone());
        let mut ctx = context(&nodes, edges, &[]);
        ctx.roots = roots;
        assert_eq!(
            chain_lines(&call_paths_section(&ctx)),
            [
                "- s1 → m1 → target",
                "- s2 → m2 → target",
                "- s3 → m3 → target"
            ]
        );
    }

    /// `if (budget-- <= 0) return;`: a budget of `n` lets `n` visits through,
    /// so the fifth visit, `e`, is the first one refused at four.
    #[test]
    fn the_walk_spends_one_step_per_visit() {
        let (a, b, c, d, e) = ("a", "b", "c", "d", "e");
        let adjacency: HashMap<&str, Vec<&str>> =
            HashMap::from([(a, vec![b, d]), (b, vec![c]), (d, vec![e])]);
        let run = |budget| {
            let mut walk = ChainWalk {
                adjacency: &adjacency,
                budget,
                chains: Vec::new(),
            };
            walk.visit(a, &mut vec![a], &mut HashSet::from([a]));
            walk.chains
        };
        assert_eq!(run(3), [vec![a, b, c]]);
        assert_eq!(run(4), [vec![a, b, c]]);
        assert_eq!(run(5), [vec![a, b, c], vec![a, d, e]]);
        assert_eq!(CALL_PATH_BUDGET, 2000);
    }

    #[test]
    fn the_low_confidence_note_is_upstream_s_text() {
        let flat = function("FLAT", "scripts/dataset/download.ts", 1);
        let mut ctx = context(std::slice::from_ref(&flat), Vec::new(), &[&flat]);
        ctx.low_confidence = true;
        let markdown = format_markdown(&ctx, &not_generated);
        assert!(
            markdown.ends_with("\n\n\n### ⚠️ Low-confidence match\n\nThis query matched mostly on common words, so the entry points above may be off-target — treat them as a starting point, not a complete answer. For a reliable result:\n- `codegraph_explore` with the **exact symbol names** you are after (class / function / method names), or\n- `codegraph_search <name>` for one specific symbol\n- `codegraph_files` a likely area: `scripts/dataset`\n\nDo not assume the list above is comprehensive."),
            "{markdown}"
        );
        let entries = [
            function("a", "src/a.ts", 1),
            function("b", "src/a.ts", 9),
            function("c", "root.ts", 1),
            function("d", "/abs.ts", 1),
            function("e", "lib/e.ts", 1),
            function("f", "lib/f/f.ts", 1),
        ];
        let refs: Vec<&Node> = entries.iter().collect();
        assert!(
            low_confidence_note(&refs).contains(
                "\n- `codegraph_files` a likely area: `src`, `root.ts`, `/abs.ts`, `lib`\n"
            ),
            "{}",
            low_confidence_note(&refs)
        );
        assert!(!low_confidence_note(&[]).contains("codegraph_files"));
    }
}
