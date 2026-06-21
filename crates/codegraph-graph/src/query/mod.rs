pub mod parser;
pub mod scoring;

use std::collections::HashSet;

use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_store::queries::SearchResult;
use codegraph_store::Store;

pub use parser::{bounded_edit_distance, parse_query, ParsedQuery};

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub kinds: Vec<NodeKind>,
    pub languages: Vec<Language>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn merge_unique<T: Clone + PartialEq>(base: &[T], extra: &[T]) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for item in base.iter().chain(extra.iter()) {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

pub fn search_nodes(
    store: &Store,
    query: &str,
    options: &SearchOptions,
    project_name_tokens: &HashSet<String>,
) -> rusqlite::Result<Vec<SearchResult>> {
    let limit = options.limit.unwrap_or(100);
    let offset = options.offset.unwrap_or(0);

    let parsed = parse_query(query);

    let merged_kinds = if !parsed.kinds.is_empty() {
        merge_unique(&options.kinds, &parsed.kinds)
    } else {
        options.kinds.clone()
    };
    let merged_languages = if !parsed.languages.is_empty() {
        merge_unique(&options.languages, &parsed.languages)
    } else {
        options.languages.clone()
    };
    let path_filters = parsed.path_filters.clone();
    let name_filters = parsed.name_filters.clone();
    let text = parsed.text.clone();
    let kinds = merged_kinds;
    let languages = merged_languages;

    let mut results: Vec<SearchResult> = if !text.is_empty() {
        store.search_nodes_fts_filtered(&text, &kinds, &languages, limit, offset)?
    } else {
        store.search_all_by_filters(&kinds, &languages, limit * 5)?
    };

    if results.is_empty() && text.chars().count() >= 2 {
        results = store.search_nodes_like(&text, &kinds, &languages, limit, offset)?;
    }

    if results.is_empty() && text.chars().count() >= 3 {
        results = search_nodes_fuzzy(store, &text, &kinds, &languages, limit)?;
    }

    if !results.is_empty() && !query.is_empty() {
        let mut existing_ids: HashSet<String> = results.iter().map(|r| r.node.id.clone()).collect();
        let max_fts_score = results
            .iter()
            .map(|r| r.score)
            .fold(f64::NEG_INFINITY, f64::max);
        let terms: Vec<&str> = query
            .split_whitespace()
            .filter(|t| t.chars().count() >= 2)
            .collect();
        for term in terms {
            let rows = store.nodes_by_exact_name_nocase(term, &kinds, &languages)?;
            for node in rows {
                if !existing_ids.contains(&node.id) {
                    existing_ids.insert(node.id.clone());
                    results.push(SearchResult {
                        node,
                        score: max_fts_score,
                    });
                }
            }
        }
    }

    if !results.is_empty() && (!text.is_empty() || !query.is_empty()) {
        let scoring_query = if !text.is_empty() { &text } else { query };
        for result in &mut results {
            result.score += scoring::kind_bonus(result.node.kind)
                + scoring::score_path_relevance(
                    &result.node.file_path,
                    scoring_query,
                    project_name_tokens,
                )
                + scoring::name_match_bonus(&result.node.name, scoring_query);
        }
        sort_by_score_desc(&mut results);
        if results.len() > limit as usize {
            results.truncate(limit as usize);
        }
    }

    if !path_filters.is_empty() {
        let lowered: Vec<String> = path_filters.iter().map(|p| p.to_lowercase()).collect();
        results.retain(|r| {
            let fp = r.node.file_path.to_lowercase();
            lowered.iter().any(|p| fp.contains(p.as_str()))
        });
    }
    if !name_filters.is_empty() {
        let lowered: Vec<String> = name_filters.iter().map(|n| n.to_lowercase()).collect();
        results.retain(|r| {
            let nm = r.node.name.to_lowercase();
            lowered.iter().any(|n| nm.contains(n.as_str()))
        });
    }

    Ok(results)
}

fn sort_by_score_desc(results: &mut [SearchResult]) {
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn search_nodes_fuzzy(
    store: &Store,
    text: &str,
    kinds: &[NodeKind],
    languages: &[Language],
    limit: i64,
) -> rusqlite::Result<Vec<SearchResult>> {
    let lowered = text.to_lowercase();
    let max_dist = if lowered.chars().count() <= 4 { 1 } else { 2 };

    let all_names = store.all_node_names()?;
    let mut candidates: Vec<(String, usize)> = Vec::new();
    for name in all_names {
        let dist = bounded_edit_distance(&name.to_lowercase(), &lowered, max_dist);
        if dist <= max_dist {
            candidates.push((name, dist));
        }
    }
    candidates.sort_by_key(|a| a.1);

    let followup_cap = std::cmp::max(limit * 2, 50) as usize;
    let capped: Vec<(String, usize)> = candidates.into_iter().take(followup_cap).collect();

    let mut results: Vec<SearchResult> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (name, dist) in capped {
        if results.len() >= limit as usize {
            break;
        }
        let rows: Vec<Node> = store.nodes_by_exact_name_filtered(&name, kinds, languages)?;
        for node in rows {
            if seen.contains(&node.id) {
                continue;
            }
            seen.insert(node.id.clone());
            results.push(SearchResult {
                node,
                score: 1.0 / (1.0 + dist as f64),
            });
            if results.len() >= limit as usize {
                break;
            }
        }
    }
    Ok(results)
}
