//! Query-time graph-derived segment matching for the prompt-hook MEDIUM tier.
//!
//! Rust port of upstream `CodeGraph.getSegmentMatches` (`src/index.ts:908-1030`,
//! #1136 `e699ee9` + #1144/#1145/#1146 hardening from `35611b9`). Instead of
//! reading an index-time `name_segment_vocab` table (which would break this
//! project's golden `.schema` parity), the segment / co-occurrence / rarity map
//! is derived on demand from the already-indexed node names
//! ([`Store::distinct_non_file_node_names`]). The tier is a pure function of the
//! prompt prose words + the current node-name set, and its output is
//! deterministically ordered (matching this project's determinism invariant).

use std::collections::BTreeMap;

use codegraph_core::types::NodeKind;
use codegraph_store::Store;

use crate::segments::{segment_lookup_variants, split_identifier_segments};

/// A single word matching hundreds of names in a big repo is noise, not signal:
/// the single-word (Tier B) rarity ceiling. Co-occurrence (Tier A) is exempt —
/// two words on one name is already discriminative. Mirrors upstream
/// `SEGMENT_RARITY_CEILING = 25`.
const SEGMENT_RARITY_CEILING: usize = 25;

/// A symbol whose name-segments match prose words from a prompt — the
/// graph-derived signal behind the front-load hook's MEDIUM tier. Always
/// verified to exist in `nodes` at the time it is returned. Mirrors upstream
/// `SegmentMatch` (`types.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentMatch {
    pub name: String,
    pub kind: NodeKind,
    pub file_path: String,
    pub start_line: i64,
    pub matched_words: Vec<String>,
}

/// Which indexed symbols do these prose `words` name? "state machine des
/// commandes" → `OrderStateMachine`, in any human language whose technical
/// nouns are Latin script — no keyword list involved.
///
/// Precision comes from the repo's own naming statistics, not a vocabulary:
/// - CO-OCCURRENCE (Tier A): ≥2 DISTINCT prompt words that are segments of the
///   SAME name is strong evidence and always qualifies. Variants of one word
///   are folded back to that word (#1146), so a plural pair can't tie a genuine
///   two-word match.
/// - RARITY (Tier B, only if Tier A empty): a single matched word qualifies
///   only when it is ≥5 chars, its segment clusters across ≥2 and
///   ≤[`SEGMENT_RARITY_CEILING`] distinct names, and the candidate name has ≥2
///   segments.
///
/// Every candidate is re-verified against `nodes` (via [`Store::nodes_by_name`])
/// and a non-file/non-import representative is picked (#1144), so a returned
/// symbol is guaranteed to exist right now. The result is sorted by coverage
/// (desc) then name length (asc) then name (asc) for determinism.
pub fn get_segment_matches(store: &Store, words: &[String], limit: usize) -> Vec<SegmentMatch> {
    if words.is_empty() {
        return Vec::new();
    }

    // variant → original word (plural folding), for coverage accounting. First
    // writer wins so a variant maps to a single stable original word.
    let mut variant_to_word: BTreeMap<String, String> = BTreeMap::new();
    for word in words {
        for variant in segment_lookup_variants(word) {
            variant_to_word
                .entry(variant)
                .or_insert_with(|| word.clone());
        }
    }

    let names = match store.distinct_non_file_node_names() {
        Ok(names) => names,
        Err(_) => return Vec::new(),
    };

    // segment → set of names (deterministic insertion via the sorted name feed);
    // name → its segment count (for the Tier-B ≥2-segment rule).
    let mut segment_to_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name_segment_count: BTreeMap<String, usize> = BTreeMap::new();
    for (name, _kind) in &names {
        let segs = split_identifier_segments(name);
        name_segment_count.insert(name.clone(), segs.len());
        for seg in segs {
            let entry = segment_to_names.entry(seg).or_default();
            if !entry.contains(name) {
                entry.push(name.clone());
            }
        }
    }

    // Candidates: (name, matched original words).
    let mut candidates: Vec<(String, Vec<String>)> = Vec::new();

    // Tier A: co-occurrence — a name covering ≥2 DISTINCT prompt words.
    for name in name_segment_count.keys() {
        let matched = words_matching_name(name, &variant_to_word);
        if matched.len() >= 2 {
            candidates.push((name.clone(), matched));
        }
    }

    // Tier B: single rare word — only if co-occurrence found nothing.
    if candidates.is_empty() {
        let single_word_variants: Vec<&String> = variant_to_word
            .keys()
            .filter(|v| {
                variant_to_word
                    .get(*v)
                    .is_some_and(|w| w.chars().count() >= 5)
            })
            .collect();

        // (variant, distinct-name count) for the eligible variants.
        let mut rare: Vec<(String, usize)> = single_word_variants
            .iter()
            .filter_map(|v| segment_to_names.get(*v).map(|ns| ((*v).clone(), ns.len())))
            .filter(|(_, n)| *n >= 2 && *n <= SEGMENT_RARITY_CEILING)
            .collect();
        // Rarest first, then variant name for a stable tie-break; take 2.
        rare.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        rare.truncate(2);

        for (variant, _count) in rare {
            let word = variant_to_word.get(&variant).cloned().unwrap_or_default();
            if let Some(candidate_names) = segment_to_names.get(&variant) {
                // Names for a segment: shortest name first, capped at 12 (upstream).
                let mut sorted = candidate_names.clone();
                sorted.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
                for name in sorted.into_iter().take(12) {
                    if name_segment_count.get(&name).copied().unwrap_or(0) < 2 {
                        continue;
                    }
                    candidates.push((name, vec![word.clone()]));
                }
            }
        }
    }

    // Sort by coverage (desc), then name length (asc), then name (asc).
    candidates.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| a.0.len().cmp(&b.0.len()))
            .then_with(|| a.0.cmp(&b.0))
    });

    // Verify against nodes (the honesty gate) and pick a representative.
    let mut out: Vec<SegmentMatch> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (name, matched) in candidates {
        if out.len() >= limit {
            break;
        }
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let nodes = match store.nodes_by_name(&name) {
            Ok(nodes) => nodes,
            Err(_) => continue,
        };
        let rep = nodes
            .iter()
            .find(|n| n.kind != NodeKind::File && n.kind != NodeKind::Import);
        let Some(rep) = rep else {
            continue;
        };
        let mut matched_sorted = matched;
        matched_sorted.sort();
        out.push(SegmentMatch {
            name,
            kind: rep.kind,
            file_path: rep.file_path.clone(),
            start_line: rep.start_line,
            matched_words: matched_sorted,
        });
    }
    out
}

/// Which of the prompt's original words match `name`'s segments (via variants).
fn words_matching_name(name: &str, variant_to_word: &BTreeMap<String, String>) -> Vec<String> {
    let segments = split_identifier_segments(name);
    let mut matched: Vec<String> = Vec::new();
    for (variant, word) in variant_to_word {
        if segments.iter().any(|s| s == variant) && !matched.contains(word) {
            matched.push(word.clone());
        }
    }
    matched
}
