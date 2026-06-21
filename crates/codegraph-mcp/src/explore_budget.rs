//! Size-adaptive output budget for `codegraph_explore`, scaled to project size.
//!
//! Faithful port of the upstream `getExploreOutputBudget` / `getExploreBudget`
//! (upstream `tools.ts:102-258`). The budget is a CEILING
//! (relevance still gates WHAT is included): smaller codebases get a tighter
//! total cap, fewer default files, a smaller per-file cap, and tighter
//! clustering, and gate off the meta-text (relationships map, "additional
//! relevant files" list, completeness signal, budget note) where one rich call
//! is the whole story. Larger codebases keep the generous defaults but cap at
//! the ~24K inline tool-result ceiling — more files indexed means more CALLS
//! via `getExploreBudget`, not a bigger single response.
//!
//! This is query-time output formatting only: it does NOT touch extraction or
//! golden byte-equivalence.

/// Adaptive output budget for one `codegraph_explore` response
/// (`tools.ts:128-158`).
#[derive(Debug, Clone, Copy)]
pub struct ExploreOutputBudget {
    /// Hard cap on total output characters.
    pub max_output_chars: usize,
    /// Default `maxFiles` when the caller didn't specify one.
    pub default_max_files: usize,
    /// Cap on contiguous source returned per file (across all its clusters).
    pub max_chars_per_file: usize,
    /// Cluster gap threshold in lines — tighter clustering on small projects.
    pub gap_threshold: usize,
    /// Max symbols listed in the per-file header.
    pub max_symbols_in_file_header: usize,
    /// Max edges shown per relationship kind in the Relationships section.
    pub max_edges_per_relationship_kind: usize,
    /// Include the "Relationships" section.
    pub include_relationships: bool,
    /// Include the "Additional relevant files (not shown)" trailing list.
    pub include_additional_files: bool,
    /// Include the "Complete source code is included above…" reminder.
    pub include_completeness_signal: bool,
    /// Include the explore-budget reminder at the end.
    pub include_budget_note: bool,
    /// Hard-drop test/spec/icon/i18n files from the relevant-file set unless
    /// the query itself mentions tests (`tools.ts:149-157`).
    pub exclude_low_value_files: bool,
}

/// Tiered budget, scaled to project size (`tools.ts:160-258`). Tier
/// breakpoints (<150, <500, <5000, <15000, else) mirror `getExploreBudget` so a
/// project sits in the same tier across both knobs. Invariant: a larger tier
/// never gets a smaller `max_chars_per_file` than a smaller tier.
pub fn get_explore_output_budget(file_count: i64) -> ExploreOutputBudget {
    if file_count < 150 {
        // ITER3 shape (13K/4/3.8K) with the test-file hard-exclude
        // (`tools.ts:172-191`).
        return ExploreOutputBudget {
            max_output_chars: 13000,
            default_max_files: 4,
            max_chars_per_file: 3800,
            gap_threshold: 7,
            max_symbols_in_file_header: 5,
            max_edges_per_relationship_kind: 4,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        };
    }
    if file_count < 500 {
        // `tools.ts:192-207`.
        return ExploreOutputBudget {
            max_output_chars: 18000,
            default_max_files: 5,
            max_chars_per_file: 3800,
            gap_threshold: 8,
            max_symbols_in_file_header: 6,
            max_edges_per_relationship_kind: 6,
            include_relationships: false,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
            exclude_low_value_files: true,
        };
    }
    if file_count < 5000 {
        // ~150-line per-file window × ~6 files, capped at the ~24K inline
        // ceiling (`tools.ts:208-224`).
        return ExploreOutputBudget {
            max_output_chars: 24000,
            default_max_files: 8,
            max_chars_per_file: 6500,
            gap_threshold: 12,
            max_symbols_in_file_header: 10,
            max_edges_per_relationship_kind: 10,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
            exclude_low_value_files: false,
        };
    }
    // Large + very-large repos share the same ~24K inline ceiling with per-file
    // 7000 (`tools.ts:226-257`); the >=15000 tier is identical to <15000.
    ExploreOutputBudget {
        max_output_chars: 24000,
        default_max_files: 8,
        max_chars_per_file: 7000,
        gap_threshold: 15,
        max_symbols_in_file_header: 15,
        max_edges_per_relationship_kind: 15,
        include_relationships: true,
        include_additional_files: true,
        include_completeness_signal: true,
        include_budget_note: true,
        exclude_low_value_files: false,
    }
}

/// Recommended number of `codegraph_explore` calls for a project of this size
/// (`tools.ts:102-108`).
pub fn get_explore_budget(file_count: i64) -> u32 {
    if file_count < 500 {
        1
    } else if file_count < 5000 {
        2
    } else if file_count < 15000 {
        3
    } else if file_count < 25000 {
        4
    } else {
        5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_breakpoints_match_upstream() {
        // <150: tight 13K cap, all meta gated off, low-value excluded.
        let tiny = get_explore_output_budget(3);
        assert_eq!(tiny.max_output_chars, 13000);
        assert_eq!(tiny.default_max_files, 4);
        assert_eq!(tiny.max_chars_per_file, 3800);
        assert_eq!(tiny.gap_threshold, 7);
        assert_eq!(tiny.max_symbols_in_file_header, 5);
        assert_eq!(tiny.max_edges_per_relationship_kind, 4);
        assert!(!tiny.include_relationships);
        assert!(!tiny.include_additional_files);
        assert!(!tiny.include_completeness_signal);
        assert!(!tiny.include_budget_note);
        assert!(tiny.exclude_low_value_files);

        // <500: 18K, still meta-gated, still low-value excluded.
        let small = get_explore_output_budget(200);
        assert_eq!(small.max_output_chars, 18000);
        assert_eq!(small.default_max_files, 5);
        assert!(!small.include_relationships);
        assert!(small.exclude_low_value_files);

        // <5000: meta turns on, low-value exclude turns off.
        let mid = get_explore_output_budget(1000);
        assert_eq!(mid.max_output_chars, 24000);
        assert_eq!(mid.max_chars_per_file, 6500);
        assert!(mid.include_relationships);
        assert!(mid.include_additional_files);
        assert!(mid.include_completeness_signal);
        assert!(mid.include_budget_note);
        assert!(!mid.exclude_low_value_files);

        // <15000 and >=15000 share the 7000 per-file cap.
        assert_eq!(get_explore_output_budget(10_000).max_chars_per_file, 7000);
        assert_eq!(get_explore_output_budget(50_000).max_chars_per_file, 7000);
    }

    #[test]
    fn per_file_cap_is_monotonic_across_tiers() {
        // A larger tier must never get a smaller per-file cap (`tools.ts:171`).
        let caps =
            [3, 200, 1000, 10_000, 50_000].map(|n| get_explore_output_budget(n).max_chars_per_file);
        for w in caps.windows(2) {
            assert!(w[1] >= w[0], "per-file cap regressed: {caps:?}");
        }
    }

    #[test]
    fn call_budget_tiers_match_upstream() {
        assert_eq!(get_explore_budget(3), 1);
        assert_eq!(get_explore_budget(499), 1);
        assert_eq!(get_explore_budget(500), 2);
        assert_eq!(get_explore_budget(4999), 2);
        assert_eq!(get_explore_budget(5000), 3);
        assert_eq!(get_explore_budget(14_999), 3);
        assert_eq!(get_explore_budget(15_000), 4);
        assert_eq!(get_explore_budget(24_999), 4);
        assert_eq!(get_explore_budget(25_000), 5);
    }
}
