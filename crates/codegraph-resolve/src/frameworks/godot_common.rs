//! Shared parsing helpers for the Godot resource-format parsers
//! (`project.godot` / `.tscn` / `.tres`).
//!
//! The Godot resource family is a flat INI-like text grammar: `[section]`
//! headers, `key = value` lines, double-quoted strings, and `res://` project
//! URIs. L1 (`project.godot`, T3) established the canonical handling for the
//! pieces every later layer shares — quote stripping, `res://` → repo-relative
//! mapping, and pulling every quoted token out of an array literal. T4 (`.tscn`)
//! and T5 (`.tres`) reuse the EXACT same rules so a `res://path` resolves to the
//! identical repo-relative string regardless of which file referenced it. This
//! module is the single source of truth for those rules; the per-file parsers
//! own only their own section/line shapes.

/// Map a quoted Godot value (`"[*]res://path"`) to a repo-relative path.
/// Strips the surrounding quotes, a leading `*`, the `res://` scheme, and any
/// remaining leading `/`. Returns `None` if the result is not a `res://` path
/// or is empty.
pub(crate) fn map_res_path(value: &str) -> Option<String> {
    map_res_path_inner(strip_quotes(value))
}

/// Same as [`map_res_path`] but for an already-unquoted token.
pub(crate) fn map_res_path_inner(token: &str) -> Option<String> {
    let token = token.trim();
    let token = token.strip_prefix('*').unwrap_or(token);
    let rest = token.strip_prefix("res://")?;
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_string())
}

/// Strip one pair of surrounding double quotes, if present.
pub(crate) fn strip_quotes(value: &str) -> &str {
    let v = value.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v)
}

/// Yield every double-quoted substring's inner text from `s`
/// (`PackedStringArray("a", "b")` → `["a", "b"]`). An unterminated final quote
/// stops the scan (its content is not yielded).
pub(crate) fn quoted_strings(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' {
                j += 1;
            }
            if j < bytes.len() {
                out.push(&s[start..j]);
                i = j + 1;
                continue;
            }
            break; // unterminated quote — stop
        }
        i += 1;
    }
    out
}
