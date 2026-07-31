pub mod parser;
pub mod scoring;

use std::collections::HashSet;

use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_store::Store;
use codegraph_store::queries::SearchResult;

pub use parser::{ParsedQuery, bounded_edit_distance, parse_query};

/// #1319 infix-seeding bounds: how many callable candidates the store returns
/// per token, and how many survive the segment-boundary filter into the result
/// set. Both are hard caps so a hot substring can never flood the ranking.
const INFIX_CANDIDATE_CAP: i64 = 50;
const INFIX_SEED_CAP: usize = 3;

/// Base score for an infix-seeded definer. Deliberately small: the definer earns
/// its rank from the `kind_bonus` + `name_match_bonus` pass that follows, so it
/// can never outrank a genuine exact-name hit.
const INFIX_SEED_SCORE: f64 = 1.0;

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

    seed_multi_segment_definers(store, query, &kinds, &mut results)?;

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

        sort_by_exact_name_then_score_desc(&mut results, scoring_query);
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

fn sort_by_exact_name_then_score_desc(results: &mut [SearchResult], query: &str) {
    // Both sorts are stable: establish score order first, then group exact whole-name
    // matches ahead of non-exact rows without mutating the externally visible scores.
    sort_by_score_desc(results);
    results.sort_by(|a, b| {
        let a_is_exact = scoring::is_exact_name_match(&a.node.name, query);
        let b_is_exact = scoring::is_exact_name_match(&b.node.name, query);
        b_is_exact.cmp(&a_is_exact)
    });
}

/// Multi-hump field-name seeding (upstream `1de7e8f` #1319).
///
/// A query token that names a multi-SEGMENT field (`userProfileId`,
/// `user_profile_id`, `ProfileInfo`) but matches no symbol EXACTLY is seeded
/// with the callables that DEFINE it — the ones whose own segments contain the
/// query's segment run contiguously. Scoped to multi-segment tokens so the
/// natural-language guard on bare words is untouched, and applied only when the
/// token is not already an exact symbol name so exact matches always win.
///
/// A candidate whose name merely CONTAINS the lowercase run without a segment
/// boundary (`xxprofileinfoxx`) is rejected: a silent miss beats a wrong answer.
fn seed_multi_segment_definers(
    store: &Store,
    query: &str,
    kinds: &[NodeKind],
    results: &mut Vec<SearchResult>,
) -> rusqlite::Result<()> {
    let mut existing: HashSet<String> = results.iter().map(|r| r.node.id.clone()).collect();
    for token in query.split_whitespace() {
        if !scoring::is_multi_segment_identifier(token) {
            continue;
        }
        if results.iter().any(|r| r.node.name == token) {
            continue;
        }
        if !store.nodes_by_lower_name(&token.to_lowercase())?.is_empty() {
            continue;
        }
        let needle: String = token
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let mut seeded = 0usize;
        for node in store.callable_nodes_by_name_infix(&needle, INFIX_CANDIDATE_CAP)? {
            if seeded >= INFIX_SEED_CAP {
                break;
            }
            if !kinds.is_empty() && !kinds.contains(&node.kind) {
                continue;
            }
            if !scoring::name_segments_contain_run(&node.name, token) {
                continue;
            }
            if !existing.insert(node.id.clone()) {
                continue;
            }
            results.push(SearchResult {
                node,
                score: INFIX_SEED_SCORE,
            });
            seeded += 1;
        }
    }
    Ok(())
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
