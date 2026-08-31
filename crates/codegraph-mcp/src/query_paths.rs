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
static LINE_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?::\d+(?:-\d+)?|#L\d+(?:-L?\d+)?)$").expect("line-reference regex is valid")
});
static LAST_EXTENSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.[A-Za-z][A-Za-z0-9]{0,7}$").expect("last-extension regex is valid")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryPathExtraction {
    pub stripped_query: String,
    pub pinned_files: Vec<String>,
    pub unresolved_path_spans: Vec<String>,
}

pub(crate) fn query_might_contain_paths(query: &str) -> bool {
    query.split_whitespace().any(|token| {
        let stripped = strip_wrapping(token);
        stripped.contains(['/', '\\'])
            || DOTTED_BASENAME.is_match(&stripped)
            || KEBAB_BASENAME.is_match(&stripped)
    })
}

pub(crate) fn extract_query_paths(
    query: &str,
    indexed_paths: &[String],
    max_pins: usize,
) -> QueryPathExtraction {
    let passthrough = || QueryPathExtraction {
        stripped_query: query.to_string(),
        pinned_files: Vec::new(),
        unresolved_path_spans: Vec::new(),
    };
    if query.trim().is_empty() || indexed_paths.is_empty() {
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
    let mut candidates_examined = 0usize;

    for (index, token) in tokens.iter().enumerate() {
        if pinned.len() >= max_pins || candidates_examined >= MAX_CANDIDATE_SPANS {
            break;
        }
        let stripped = strip_wrapping(token);
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
            for path in resolved.matches {
                if pinned.len() >= max_pins {
                    break;
                }
                if pinned_seen.insert(path.clone()) {
                    pinned.push(path);
                }
            }
        } else if resolved.ambiguous || is_clearly_path_shaped(&normalized) {
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
        let stripped = strip_wrapping(token);
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
    }

    if consumed.is_empty() {
        return passthrough();
    }
    QueryPathExtraction {
        stripped_query: tokens
            .into_iter()
            .enumerate()
            .filter_map(|(index, token)| (!consumed.contains(&index)).then_some(token))
            .collect::<Vec<_>>()
            .join(" "),
        pinned_files: pinned,
        unresolved_path_spans: unresolved,
    }
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

fn strip_wrapping(token: &str) -> String {
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
    LINE_REFERENCE.replace(&value, "").to_string()
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
}
