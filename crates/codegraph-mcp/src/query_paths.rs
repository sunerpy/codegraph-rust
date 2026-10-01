//! Pure file-path recognition for `codegraph_explore` queries.
//!
//! Explicit paths are resolved against the indexed file list, pinned, and
//! removed from the normal text query so bracketed routes and common basenames
//! cannot dissolve into noisy symbol seeds.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

const MAX_PREFIX_DROPS: usize = 8;
const MAX_CANDIDATE_SPANS: usize = 8;
const MAX_PINS: usize = 8;
const MAX_UNRESOLVED: usize = 4;

static DOTTED_BASENAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[^\s/\\]+\.[A-Za-z][A-Za-z0-9]{0,7}$").expect("dotted basename regex is valid")
});
static KEBAB_BASENAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9]+(?:-[A-Za-z0-9]+)+$").expect("kebab basename regex is valid")
});
/// Line references that ride along in agent-written paths: `foo.ts:123`,
/// `foo.ts:12-40`, `foo.ts#L88`, `foo.ts#L88-L120`. ASCII digits only, as in
/// upstream's JavaScript `\d`.
static LINE_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?::([0-9]+)(?:-([0-9]+))?|#L([0-9]+)(?:-L?([0-9]+))?)$")
        .expect("line-reference regex is valid")
});
/// A standalone line-number token: `900`, `900-1003`, `900–1003`, `900..1003`,
/// `L900`, `L900-L1003`. The `L` form is a line number on its own; a bare number
/// only counts after a `line`/`lines` word or directly after a path.
static LINE_NUMBER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(L?)([0-9]+)(?:(?:-|\x{2013}|\.\.)L?([0-9]+))?$")
        .expect("line-number regex is valid")
});
static LINE_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^lines?$").expect("line-word regex is valid"));
/// `lines 900 to 1003`: the connective between two bare numbers.
static RANGE_CONNECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:to|through|thru)$").expect("range-connective regex is valid")
});
/// Nothing real is this long; a larger number is a port, an id or a typo.
const MAX_LINE_NUMBER: u64 = 1_000_000;
static LAST_EXTENSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.[A-Za-z][A-Za-z0-9]{0,7}$").expect("last-extension regex is valid")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryPathExtraction {
    pub stripped_query: String,
    pub pinned_files: Vec<String>,
    pub unresolved_path_spans: Vec<String>,
    /// Line spans the query anchored to a pinned file, 1-based and inclusive:
    /// `compiler.py:776`, `foo.ts:12-40`, `foo.ts#L88-L120`, or prose next to a
    /// path (`compiler.py lines 900-1003`). An agent writes these when it wants
    /// THOSE lines, so a pin that drops them answers a different question.
    /// Only a span whose path resolved to exactly one file is kept: a line
    /// number means nothing across two candidate files (upstream #2063).
    pub line_anchors: Vec<QueryLineAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryLineAnchor {
    pub file: String,
    pub start: usize,
    pub end: usize,
}

pub(crate) fn query_might_contain_paths(query: &str) -> bool {
    query.split_whitespace().any(|token| {
        let (stripped, _) = strip_wrapping(token);
        stripped.contains(['/', '\\'])
            || DOTTED_BASENAME.is_match(&stripped)
            || KEBAB_BASENAME.is_match(&stripped)
    })
}

#[cfg(test)]
pub(crate) fn extract_query_paths(
    query: &str,
    indexed_paths: &[String],
    max_pins: usize,
) -> QueryPathExtraction {
    extract_query_paths_inner(query, indexed_paths, max_pins, None)
}

pub(crate) fn extract_query_paths_with_file_probe(
    query: &str,
    indexed_paths: &[String],
    max_pins: usize,
    exists_on_disk: &dyn Fn(&str) -> bool,
) -> QueryPathExtraction {
    extract_query_paths_inner(query, indexed_paths, max_pins, Some(exists_on_disk))
}

fn extract_query_paths_inner(
    query: &str,
    indexed_paths: &[String],
    max_pins: usize,
    exists_on_disk: Option<&dyn Fn(&str) -> bool>,
) -> QueryPathExtraction {
    let passthrough = || QueryPathExtraction {
        stripped_query: query.to_string(),
        pinned_files: Vec::new(),
        unresolved_path_spans: Vec::new(),
        line_anchors: Vec::new(),
    };
    if query.trim().is_empty() || (indexed_paths.is_empty() && exists_on_disk.is_none()) {
        return passthrough();
    }
    let max_pins = max_pins.clamp(1, MAX_PINS);
    let lower_to_original = indexed_paths
        .iter()
        .map(|path| (path.to_lowercase(), path.clone()))
        .collect::<BTreeMap<_, _>>();
    let tokens = query
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut consumed = BTreeSet::new();
    let mut pinned = Vec::new();
    let mut pinned_seen = BTreeSet::new();
    let mut unresolved = Vec::new();
    let mut anchors: Vec<QueryLineAnchor> = Vec::new();
    // Token index → the ONE file it pinned, in insertion order, for binding
    // prose line ranges to their nearest path.
    let mut single_file_at: Vec<(usize, String)> = Vec::new();
    let mut candidates_examined = 0usize;

    for (index, token) in tokens.iter().enumerate() {
        if pinned.len() >= max_pins || candidates_examined >= MAX_CANDIDATE_SPANS {
            break;
        }
        let (stripped, lines) = strip_wrapping(token);
        if stripped.chars().count() < 4 {
            continue;
        }
        let has_slash = stripped.contains(['/', '\\']);
        if !has_slash && !DOTTED_BASENAME.is_match(&stripped) {
            continue;
        }
        let normalized = normalize_span(&stripped);
        if normalized.is_empty() {
            continue;
        }
        candidates_examined += 1;
        let resolved = resolve_span(&normalized.to_lowercase(), &lower_to_original, 3);
        if !resolved.matches.is_empty() {
            consumed.insert(index);
            for path in &resolved.matches {
                if pinned.len() >= max_pins {
                    break;
                }
                if pinned_seen.insert(path.clone()) {
                    pinned.push(path.clone());
                }
            }
            record_single_file(
                index,
                &resolved.matches,
                lines,
                &pinned_seen,
                &mut single_file_at,
                &mut anchors,
            );
        } else if resolved.ambiguous
            || is_clearly_path_shaped(&normalized)
            || (normalized.contains('/') && exists_on_disk.is_some_and(|probe| probe(&normalized)))
        {
            consumed.insert(index);
            if unresolved.len() < MAX_UNRESOLVED {
                unresolved.push(normalized);
            }
        }
    }

    // Extensionless kebab basenames run after explicit paths, which therefore
    // own the shared pin budget.
    let stems = build_basename_stems(indexed_paths);
    for (index, token) in tokens.iter().enumerate() {
        if pinned.len() >= max_pins || candidates_examined >= MAX_CANDIDATE_SPANS {
            break;
        }
        if consumed.contains(&index) {
            continue;
        }
        let (stripped, lines) = strip_wrapping(token);
        if stripped.chars().count() < 4 || !KEBAB_BASENAME.is_match(&stripped) {
            continue;
        }
        candidates_examined += 1;
        let Some(matches) = stems.get(&stripped.to_lowercase()) else {
            continue;
        };
        if matches.len() > 3 {
            continue;
        }
        consumed.insert(index);
        for path in matches {
            if pinned.len() >= max_pins {
                break;
            }
            if pinned_seen.insert(path.clone()) {
                pinned.push(path.clone());
            }
        }
        record_single_file(
            index,
            matches,
            lines,
            &pinned_seen,
            &mut single_file_at,
            &mut anchors,
        );
    }

    // Third pass: prose line ranges (`lines 900-1003`, `line 42`, `L88-L120`,
    // `lines 900 to 1003`) bound to the NEAREST single-file path token. Left
    // in the query the numbers match nothing and `lines` feeds FTS a word
    // every file contains; a range with no path to bind to says nothing about
    // which file, so it is left alone.
    if !single_file_at.is_empty() {
        let nearest_file = |index: usize| -> String {
            let mut best = &single_file_at[0];
            for entry in &single_file_at {
                if entry.0.abs_diff(index) < best.0.abs_diff(index) {
                    best = entry;
                }
            }
            best.1.clone()
        };
        for index in 0..tokens.len() {
            if consumed.contains(&index) {
                continue;
            }
            let token = strip_line_token_punctuation(&tokens[index]);
            let after_word = index > 0
                && !consumed.contains(&(index - 1))
                && LINE_WORD.is_match(strip_line_token_punctuation(&tokens[index - 1]));
            let after_path = index > 0 && single_file_at.iter().any(|(at, _)| *at == index - 1);
            let Some(number) = LINE_NUMBER_TOKEN.captures(token) else {
                continue;
            };
            // A bare number is a line number only in a line context; `L900` is
            // one on its own.
            if number[1].is_empty() && !after_word && !after_path {
                continue;
            }
            let mut span = line_span(&number[2], number.get(3).map(|m| m.as_str()));
            let mut used = vec![index];
            if span.is_some()
                && number.get(3).is_none()
                && index + 2 < tokens.len()
                && RANGE_CONNECTIVE.is_match(strip_line_token_punctuation(&tokens[index + 1]))
                && let Some(tail) =
                    LINE_NUMBER_TOKEN.captures(strip_line_token_punctuation(&tokens[index + 2]))
                && tail.get(3).is_none()
            {
                span = line_span(&number[2], Some(&tail[2]));
                used.extend([index + 1, index + 2]);
            }
            let Some((start, end)) = span else {
                continue;
            };
            anchors.push(QueryLineAnchor {
                file: nearest_file(index),
                start,
                end,
            });
            consumed.extend(used);
            if after_word {
                consumed.insert(index - 1);
            }
        }
    }

    if consumed.is_empty() {
        return passthrough();
    }
    let mut seen_anchor = BTreeSet::new();
    anchors.retain(|a| seen_anchor.insert((a.file.clone(), a.start, a.end)));
    QueryPathExtraction {
        stripped_query: tokens
            .into_iter()
            .enumerate()
            .filter_map(|(index, token)| (!consumed.contains(&index)).then_some(token))
            .collect::<Vec<_>>()
            .join(" "),
        pinned_files: pinned,
        unresolved_path_spans: unresolved,
        line_anchors: anchors,
    }
}

/// Remember a token that resolved to exactly one pinned file, and the line
/// reference it carried, if any.
fn record_single_file(
    index: usize,
    matches: &[String],
    lines: Option<(usize, usize)>,
    pinned_seen: &BTreeSet<String>,
    single_file_at: &mut Vec<(usize, String)>,
    anchors: &mut Vec<QueryLineAnchor>,
) {
    let [file] = matches else {
        return;
    };
    if !pinned_seen.contains(file) {
        return;
    }
    single_file_at.push((index, file.clone()));
    if let Some((start, end)) = lines {
        anchors.push(QueryLineAnchor {
            file: file.clone(),
            start,
            end,
        });
    }
}

/// A 1-based inclusive span, in order, or `None` for a zero, an overflow, or a
/// number no file has.
fn line_span(start: &str, end: Option<&str>) -> Option<(usize, usize)> {
    let start = start.parse::<u64>().ok()?;
    let end = match end {
        Some(end) => end.parse::<u64>().ok()?,
        None => start,
    };
    if start < 1 || end < 1 || start > MAX_LINE_NUMBER || end > MAX_LINE_NUMBER {
        return None;
    }
    let (start, end) = (start as usize, end as usize);
    Some((start.min(end), start.max(end)))
}

/// Prose punctuation a line-number token can carry: `lines 900-1003,`, `(L88)`.
fn strip_line_token_punctuation(token: &str) -> &str {
    token
        .trim_start_matches(['(', '\'', '"', '`', '['])
        .trim_end_matches([')', '\'', '"', '`', ']', '.', ',', ';', ':', '!', '?'])
}

struct SpanResolution {
    matches: Vec<String>,
    ambiguous: bool,
}

fn resolve_span(
    normalized_lower: &str,
    lower_to_original: &BTreeMap<String, String>,
    max_matches: usize,
) -> SpanResolution {
    if let Some(exact) = lower_to_original.get(normalized_lower) {
        return SpanResolution {
            matches: vec![exact.clone()],
            ambiguous: false,
        };
    }
    let segments = normalized_lower
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let max_drop = MAX_PREFIX_DROPS.min(segments.len().saturating_sub(1));
    for drop_count in 0..=max_drop {
        let suffix = segments[drop_count..].join("/");
        if suffix.is_empty() {
            break;
        }
        let with_slash = format!("/{suffix}");
        let mut matches = Vec::new();
        for (lower, original) in lower_to_original {
            if lower == &suffix || lower.ends_with(&with_slash) {
                matches.push(original.clone());
                if matches.len() > max_matches {
                    return SpanResolution {
                        matches: Vec::new(),
                        ambiguous: true,
                    };
                }
            }
        }
        if !matches.is_empty() {
            return SpanResolution {
                matches,
                ambiguous: false,
            };
        }
    }
    SpanResolution {
        matches: Vec::new(),
        ambiguous: false,
    }
}

fn build_basename_stems(indexed_paths: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut stems = BTreeMap::<String, Vec<String>>::new();
    for path in indexed_paths {
        let basename = path.rsplit(['/', '\\']).next().unwrap_or(path.as_str());
        if !basename.contains('-') {
            continue;
        }
        let stem = LAST_EXTENSION.replace(basename, "").to_lowercase();
        if stem.is_empty() {
            continue;
        }
        stems.entry(stem).or_default().push(path.clone());
    }
    stems
}

/// Strip prose punctuation around a token without eating punctuation that is
/// part of the path, then split off a trailing line reference: the file is what
/// resolves, the lines are what the agent wants from it.
fn strip_wrapping(token: &str) -> (String, Option<(usize, usize)>) {
    let mut value = token.to_string();
    while let Some(first) = value.chars().next() {
        let strip = matches!(first, '\'' | '"' | '`' | '<')
            || (first == '(' && !value.contains(')'))
            || (first == '[' && !value.contains(']'))
            || (first == '{' && !value.contains('}'));
        if !strip {
            break;
        }
        value.remove(0);
    }
    while let Some(last) = value.chars().last() {
        let strip = matches!(last, '\'' | '"' | '`' | '>' | '.' | ',' | ';' | '!' | '?')
            || (last == ')' && !value.contains('('))
            || (last == ']' && !value.contains('['))
            || (last == '}' && !value.contains('{'));
        if !strip {
            break;
        }
        value.pop();
    }
    let Some(reference) = LINE_REFERENCE.captures(&value) else {
        return (value, None);
    };
    let lines = match (reference.get(1), reference.get(3)) {
        (Some(start), _) => line_span(start.as_str(), reference.get(2).map(|m| m.as_str())),
        (None, Some(start)) => line_span(start.as_str(), reference.get(4).map(|m| m.as_str())),
        (None, None) => None,
    };
    let path_end = reference.get(0).map_or(value.len(), |m| m.start());
    value.truncate(path_end);
    (value, lines)
}

fn normalize_span(span: &str) -> String {
    let mut normalized = span.replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    normalized.trim_end_matches('/').to_string()
}

fn is_clearly_path_shaped(normalized: &str) -> bool {
    let Some(slash) = normalized.rfind('/') else {
        return false;
    };
    slash > 0 && DOTTED_BASENAME.is_match(&normalized[slash + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> Vec<String> {
        [
            "src/routes/m/projects/[id]/runs/[runId]/+page.svelte",
            "src/routes/m/projects/[id]/chat/[scope]/+page.svelte",
            "src/routes/m/projects/[id]/+page.svelte",
            "src/routes/(protected)/chat-window/+page.svelte",
            "src/lib/chat-manager.ts",
            "src/lib/task-runner-manager.ts",
            "src/components/training-set-page/training-set-page.tsx",
            "src/components/training-set-page/training-set-page.module.scss",
            "src/components/training-set-page/background-image-table.tsx",
            "src/x/generic-modal.tsx",
            "src/y/generic-modal.tsx",
            "scripts/pre-commit",
            "src/a/user-profile.tsx",
            "src/b/user-profile.tsx",
            "src/c/user-profile.tsx",
            "src/d/user-profile.tsx",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn gate_distinguishes_paths_from_plain_prose_and_flags() {
        assert!(query_might_contain_paths("see src/lib/chat-manager.ts"));
        assert!(query_might_contain_paths("see background-image-table"));
        assert!(!query_might_contain_paths("how does scroll pinning work"));
        assert!(!query_might_contain_paths("use --no-cache"));
    }

    #[test]
    fn resolves_bracketed_absolute_windows_and_line_referenced_paths() {
        let paths = index();
        let bracketed = extract_query_paths(
            "scroll in src/routes/m/projects/[id]/runs/[runId]/+page.svelte atBottom",
            &paths,
            8,
        );
        assert_eq!(
            bracketed.pinned_files,
            vec!["src/routes/m/projects/[id]/runs/[runId]/+page.svelte"]
        );
        assert_eq!(bracketed.stripped_query, "scroll in atBottom");

        let absolute =
            extract_query_paths(r#"fix C:\dev\repo\src\lib\chat-manager.ts:243"#, &paths, 8);
        assert_eq!(absolute.pinned_files, vec!["src/lib/chat-manager.ts"]);

        let hash = extract_query_paths("see src/lib/task-runner-manager.ts#L88-L120", &paths, 8);
        assert_eq!(hash.pinned_files, vec!["src/lib/task-runner-manager.ts"]);

        let eight_prefixes =
            extract_query_paths("see /a/b/c/d/e/f/g/h/src/lib/chat-manager.ts", &paths, 8);
        assert_eq!(
            eight_prefixes.pinned_files,
            vec!["src/lib/chat-manager.ts"],
            "exactly eight discarded prefix segments must stay within the bound"
        );

        let nine_prefixes =
            extract_query_paths("see /a/b/c/d/e/f/g/h/i/src/lib/chat-manager.ts", &paths, 8);
        assert!(nine_prefixes.pinned_files.is_empty());
        assert_eq!(
            nine_prefixes.unresolved_path_spans,
            vec!["/a/b/c/d/e/f/g/h/i/src/lib/chat-manager.ts"]
        );
    }

    #[test]
    fn reports_only_unambiguous_path_misses() {
        let paths = index();
        let ambiguous = extract_query_paths("why all +page.svelte files flash", &paths, 8);
        assert_eq!(ambiguous.unresolved_path_spans, vec!["+page.svelte"]);
        assert_eq!(ambiguous.stripped_query, "why all files flash");

        let missing = extract_query_paths(
            "crash in src/routes/gone/missing-page.svelte on load",
            &paths,
            8,
        );
        assert_eq!(
            missing.unresolved_path_spans,
            vec!["src/routes/gone/missing-page.svelte"]
        );
        assert_eq!(missing.stripped_query, "crash in on load");

        let prose = "does gen_server:call/2 block and/or timeout";
        assert_eq!(extract_query_paths(prose, &paths, 8).stripped_query, prose);
    }

    #[test]
    fn kebab_basenames_are_bounded_and_explicit_paths_win() {
        let paths = index();
        let one = extract_query_paths("background-image-table Source", &paths, 8);
        assert_eq!(
            one.pinned_files,
            vec!["src/components/training-set-page/background-image-table.tsx"]
        );
        assert_eq!(one.stripped_query, "Source");

        let shared = extract_query_paths("generic-modal close", &paths, 8);
        assert_eq!(
            shared.pinned_files,
            vec!["src/x/generic-modal.tsx", "src/y/generic-modal.tsx"]
        );

        let hot = "refactor user-profile rendering";
        assert_eq!(extract_query_paths(hot, &paths, 8).stripped_query, hot);

        let capped = extract_query_paths(
            "background-image-table then src/lib/chat-manager.ts",
            &paths,
            1,
        );
        assert_eq!(capped.pinned_files, vec!["src/lib/chat-manager.ts"]);
        assert_eq!(capped.stripped_query, "background-image-table then");
    }

    #[test]
    fn dotless_unindexed_file_is_reported_only_when_the_safe_probe_confirms_it() {
        let seen = std::cell::RefCell::new(Vec::new());
        let exists = |path: &str| {
            seen.borrow_mut().push(path.to_string());
            path == "scripts/deploy"
        };
        let out = extract_query_paths_with_file_probe(
            "why does scripts/deploy fail on release",
            &index(),
            8,
            &exists,
        );
        assert!(out.pinned_files.is_empty());
        assert_eq!(out.unresolved_path_spans, vec!["scripts/deploy"]);
        assert_eq!(out.stripped_query, "why does fail on release");
        assert!(seen.borrow().iter().any(|path| path == "scripts/deploy"));

        let prose = extract_query_paths_with_file_probe(
            "does gen_server:call/2 block and/or timeout",
            &index(),
            8,
            &exists,
        );
        assert!(prose.unresolved_path_spans.is_empty());
        assert_eq!(
            prose.stripped_query,
            "does gen_server:call/2 block and/or timeout"
        );
        assert!(seen.borrow().iter().any(|path| path == "and/or"));
    }

    #[test]
    fn indexed_dotless_path_wins_without_consulting_the_file_probe() {
        let exists = |_path: &str| panic!("indexed path must not reach filesystem probe");
        let out = extract_query_paths_with_file_probe(
            "what does scripts/pre-commit run",
            &index(),
            8,
            &exists,
        );
        assert_eq!(out.pinned_files, vec!["scripts/pre-commit"]);
        assert!(out.unresolved_path_spans.is_empty());
    }

    fn anchor(file: &str, start: usize, end: usize) -> QueryLineAnchor {
        QueryLineAnchor {
            file: file.to_string(),
            start,
            end,
        }
    }

    const CHAT: &str = "src/lib/chat-manager.ts";

    #[test]
    fn keeps_a_line_suffix_as_an_anchor_on_the_pinned_file() {
        let paths = index();
        let anchors = |query: &str| extract_query_paths(query, &paths, 8).line_anchors;
        assert_eq!(
            anchors(&format!("body of {CHAT}:776")),
            vec![anchor(CHAT, 776, 776)]
        );
        assert_eq!(
            anchors(&format!("see {CHAT}:12-40")),
            vec![anchor(CHAT, 12, 40)]
        );
        assert_eq!(
            anchors("regression at src/lib/task-runner-manager.ts#L88-L120"),
            vec![anchor("src/lib/task-runner-manager.ts", 88, 120)]
        );
    }

    #[test]
    fn binds_a_prose_line_range_to_the_adjacent_path_and_strips_it() {
        let paths = index();
        let out = extract_query_paths(&format!("{CHAT} lines 900-1003 flushQueue tail"), &paths, 8);
        assert_eq!(out.line_anchors, vec![anchor(CHAT, 900, 1003)]);
        // `lines` would feed FTS a word every file holds; the numbers match nothing.
        assert_eq!(out.stripped_query, "flushQueue tail");
    }

    #[test]
    fn accepts_the_other_line_range_spellings() {
        let paths = index();
        let anchors = |query: &str| extract_query_paths(query, &paths, 8).line_anchors;
        for query in [
            format!("lines 900 to 1003 of {CHAT}"),
            format!("L900-L1003 in {CHAT}"),
            format!("{CHAT} 900-1003"),
            format!("{CHAT} (lines 1003-900)"),
            format!("{CHAT} lines 900\u{2013}1003"),
            format!("{CHAT} lines 900..1003"),
            format!("{CHAT} lines 900 through 1003"),
            format!("{CHAT} lines 900 thru L1003."),
        ] {
            assert_eq!(anchors(&query), vec![anchor(CHAT, 900, 1003)], "{query}");
        }
        assert_eq!(
            anchors(&format!("{CHAT} line 42")),
            vec![anchor(CHAT, 42, 42)]
        );
    }

    #[test]
    fn binds_each_range_to_its_nearest_path() {
        let paths = index();
        let out = extract_query_paths(
            &format!("{CHAT} lines 10-20 and src/lib/task-runner-manager.ts lines 30-40"),
            &paths,
            8,
        );
        assert_eq!(
            out.line_anchors,
            vec![
                anchor(CHAT, 10, 20),
                anchor("src/lib/task-runner-manager.ts", 30, 40)
            ]
        );
        assert_eq!(out.stripped_query, "and");
    }

    #[test]
    fn leaves_line_numbers_alone_without_one_resolved_file() {
        let paths = index();
        let no_path = extract_query_paths("flushQueue lines 900-1003", &paths, 8);
        assert!(no_path.line_anchors.is_empty());
        assert_eq!(no_path.stripped_query, "flushQueue lines 900-1003");
        // A bare number with no line context is not a line number either.
        let prose = extract_query_paths(&format!("{CHAT} retries 3 times"), &paths, 8);
        assert!(prose.line_anchors.is_empty());
        assert_eq!(prose.stripped_query, "retries 3 times");
        // `generic-modal.tsx` pins two files; a line number means nothing across both.
        let two_files = extract_query_paths("generic-modal.tsx:40", &paths, 8);
        assert_eq!(two_files.pinned_files.len(), 2);
        assert!(two_files.line_anchors.is_empty());
    }

    #[test]
    fn rejects_line_numbers_no_file_has_and_dedupes_repeats() {
        let paths = index();
        let anchors = |query: &str| extract_query_paths(query, &paths, 8).line_anchors;
        assert!(anchors(&format!("{CHAT}:0")).is_empty());
        assert!(anchors(&format!("{CHAT} line 1000001")).is_empty());
        assert!(anchors(&format!("{CHAT} line 99999999999999999999999")).is_empty());
        assert_eq!(
            anchors(&format!("{CHAT}:7 and {CHAT} line 7")),
            vec![anchor(CHAT, 7, 7)]
        );
    }
}
