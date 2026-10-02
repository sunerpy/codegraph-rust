//! `GET /api/search?q=&limit=` — the palette's ranked symbol search. Upstream
//! `src/ui-server/api/search.ts`.
//!
//! Exact names first, then prefix, substring, qualified-name and file-path
//! matches, then whatever full-text search found through a signature or a
//! docstring; within a class, definitions before mentions, production before
//! tests, shorter names first.

use std::collections::{HashMap, HashSet};

use codegraph_core::types::{Node, NodeKind};
use codegraph_graph::query::parser::{ParsedQuery, parse_query};
use codegraph_graph::query::{SearchOptions, search_nodes};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ctx;
use super::wire::{WireNodeRef, locale_compare, to_node_ref, wire_list};
use crate::respond::{ApiResult, Query};

/// Candidates each lookup contributes before ranking.
const CANDIDATE_POOL: i64 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchKind {
    Exact,
    Prefix,
    Substring,
    Qualified,
    File,
    Related,
}

impl MatchKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::Substring => "substring",
            Self::Qualified => "qualified",
            Self::File => "file",
            Self::Related => "related",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireSearchResult {
    #[serde(flatten)]
    node: WireNodeRef,
    match_kind: &'static str,
}

fn kind_rank(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::Class
        | NodeKind::Component
        | NodeKind::Interface
        | NodeKind::Struct
        | NodeKind::Trait
        | NodeKind::Protocol
        | NodeKind::Enum
        | NodeKind::Union
        | NodeKind::TypeAlias
        | NodeKind::Route => 0,
        NodeKind::Constant
        | NodeKind::Property
        | NodeKind::Field
        | NodeKind::Variable
        | NodeKind::EnumMember => 1,
        NodeKind::File | NodeKind::Module | NodeKind::Namespace => 2,
        _ => 3,
    }
}

fn classify(node: &Node, needle: &str) -> Option<MatchKind> {
    let name = node.name.to_lowercase();
    if name == needle {
        Some(MatchKind::Exact)
    } else if name.starts_with(needle) {
        Some(MatchKind::Prefix)
    } else if name.contains(needle) {
        Some(MatchKind::Substring)
    } else if node.qualified_name.to_lowercase().contains(needle) {
        Some(MatchKind::Qualified)
    } else if node
        .file_path
        .to_lowercase()
        .replace('\\', "/")
        .contains(needle)
    {
        Some(MatchKind::File)
    } else {
        None
    }
}

/// The search's own narrow test check (a suite, not an example or a fixture).
fn is_test_path(file_path: &str) -> bool {
    let lower = file_path.to_lowercase().replace('\\', "/");
    let dir_hit = lower
        .split('/')
        .rev()
        .skip(1)
        .any(|segment| matches!(segment, "test" | "tests" | "spec" | "specs" | "__tests__"));
    if dir_hit {
        return true;
    }
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if ext.is_empty()
        || !ext
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return false;
    }
    ["test", "tests", "spec", "specs"].iter().any(|marker| {
        stem.strip_suffix(marker)
            .and_then(|rest| rest.chars().last())
            .is_some_and(|sep| matches!(sep, '.' | '_' | '-'))
    })
}

fn matches_filters(node: &Node, parsed: &ParsedQuery) -> bool {
    if !parsed.kinds.is_empty() && !parsed.kinds.contains(&node.kind) {
        return false;
    }
    if !parsed.languages.is_empty() && !parsed.languages.contains(&node.language) {
        return false;
    }
    if !parsed.path_filters.is_empty() {
        let file = node.file_path.to_lowercase();
        if !parsed
            .path_filters
            .iter()
            .any(|p| file.contains(&p.to_lowercase()))
        {
            return false;
        }
    }
    if !parsed.name_filters.is_empty() {
        let name = node.name.to_lowercase();
        if !parsed
            .name_filters
            .iter()
            .any(|n| name.contains(&n.to_lowercase()))
        {
            return false;
        }
    }
    true
}

fn filters_json(parsed: Option<&ParsedQuery>) -> Value {
    match parsed {
        None => json!({ "kinds": [], "languages": [], "paths": [], "names": [] }),
        Some(p) => json!({
            "kinds": p.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "languages": p.languages.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
            "paths": p.path_filters,
            "names": p.name_filters,
        }),
    }
}

pub fn build(ctx: &Ctx<'_>, query: &Query) -> ApiResult<Value> {
    let raw = query.optional_text("q")?.to_string();
    let limit = query.int("limit", 1, 200, Some(60))? as usize;
    if raw.trim().is_empty() {
        return Ok(json!({
            "query": raw,
            "text": "",
            "filters": filters_json(None),
            "results": wire_list(Vec::<WireSearchResult>::new(), 0),
            "groups": [],
        }));
    }
    let parsed = parse_query(&raw);
    let text = parsed.text.trim().to_string();
    let needle = text.to_lowercase();

    let mut order: Vec<String> = Vec::new();
    let mut candidates: HashMap<String, Node> = HashMap::new();
    let mut remember = |node: Node| {
        if !candidates.contains_key(&node.id) {
            order.push(node.id.clone());
            candidates.insert(node.id.clone(), node);
        }
    };
    if !text.is_empty() {
        for node in ctx.store.nodes_by_name(&text)? {
            remember(node);
        }
        for node in ctx.store.nodes_by_name_substring(&text, CANDIDATE_POOL)? {
            remember(node);
        }
    }
    let options = SearchOptions {
        limit: Some(CANDIDATE_POOL),
        ..SearchOptions::default()
    };
    for result in search_nodes(ctx.store, &raw, &options, &HashSet::new())? {
        remember(result.node);
    }

    let mut scored: Vec<(Node, MatchKind)> = Vec::new();
    for id in &order {
        let Some(node) = candidates.remove(id) else {
            continue;
        };
        if !matches_filters(&node, &parsed) {
            continue;
        }
        let kind = if needle.is_empty() {
            MatchKind::Related
        } else {
            classify(&node, &needle).unwrap_or(MatchKind::Related)
        };
        scored.push((node, kind));
    }
    scored.sort_by(|(a, ak), (b, bk)| {
        ak.cmp(bk)
            .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
            .then_with(|| is_test_path(&a.file_path).cmp(&is_test_path(&b.file_path)))
            .then_with(|| {
                a.name
                    .encode_utf16()
                    .count()
                    .cmp(&b.name.encode_utf16().count())
            })
            .then_with(|| locale_compare(&a.file_path, &b.file_path))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    let total = scored.len();
    let top: Vec<&(Node, MatchKind)> = scored.iter().take(limit).collect();
    let paths: Vec<String> = top.iter().map(|(n, _)| n.file_path.clone()).collect();
    let generated = ctx.store.generated_paths_among(&paths)?;
    let results: Vec<WireSearchResult> = top
        .iter()
        .map(|(node, kind)| {
            let mut wire = to_node_ref(node);
            if generated.contains(&node.file_path) {
                wire.generated = Some(true);
            }
            WireSearchResult {
                node: wire,
                match_kind: kind.as_str(),
            }
        })
        .collect();

    let mut groups: Vec<(&'static str, Vec<WireSearchResult>)> = Vec::new();
    for result in &results {
        match groups.iter_mut().find(|(k, _)| *k == result.node.kind) {
            Some((_, items)) => items.push(result.clone()),
            None => groups.push((result.node.kind, vec![result.clone()])),
        }
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(kind, items)| json!({ "kind": kind, "count": items.len(), "items": items }))
        .collect();

    Ok(json!({
        "query": raw,
        "text": text,
        "filters": filters_json(Some(&parsed)),
        "results": wire_list(results, total),
        "groups": groups,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_narrow_test_check_matches_suites_only() {
        assert!(is_test_path("crates/a/tests/x.rs"));
        assert!(is_test_path("src/__tests__/a.ts"));
        assert!(is_test_path("src/a.test.ts"));
        assert!(is_test_path("src/a_spec.rb"));
        assert!(!is_test_path("examples/demo.rs"));
        assert!(!is_test_path("src/contest.ts"));
        assert!(!is_test_path("src/latest.rs"));
    }
}
