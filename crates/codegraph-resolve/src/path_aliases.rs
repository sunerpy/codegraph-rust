//! Project-level import-path alias loading.
//!
//! Ports `upstream resolution/path-aliases.ts`. Reads
//! `compilerOptions.paths` from `tsconfig.json` / `jsconfig.json` at the project
//! root and converts the patterns into a form the import resolver can consult
//! (`path-aliases.ts:1-24`). Scope mirrors the upstream v1: reads tsconfig then
//! jsconfig, honors `baseUrl` + `paths`, supports the single `*` wildcard, does
//! NOT follow `extends` chains or read Vite/webpack configs (`path-aliases.ts:14-20`).

use crate::pathutil;
use serde_json::Value;
use std::path::Path;

/// A single alias pattern from `compilerOptions.paths`
/// (`AliasPattern`, `path-aliases.ts:31-45`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasPattern {
    /// The literal prefix before `*` (or the whole pattern when no `*`).
    pub prefix: String,
    /// The literal suffix after `*` (almost always empty).
    pub suffix: String,
    /// Whether the pattern contains a `*` wildcard.
    pub has_wildcard: bool,
    /// Replacement templates (tsconfig priority order), relative to `base_url`.
    pub replacements: Vec<String>,
}

/// The resolved alias map for a project (`AliasMap`, `path-aliases.ts:47-55`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasMap {
    /// Absolute path the `paths` patterns are rooted at.
    pub base_url: String,
    /// Patterns ordered by specificity (longer prefix first, literal before
    /// wildcard).
    pub patterns: Vec<AliasPattern>,
}

/// Strip JSONC comments + trailing commas (`stripJsonc`, `path-aliases.ts:65-104`).
///
/// Walks the source as a tiny state machine tracking string context so that a
/// `//` inside a string value (e.g. a URL) is never truncated.
fn strip_jsonc(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_string = false;
    while i < n {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if ch == '\\' && i + 1 < n {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == '/' && chars.get(i + 1).copied() == Some('/') {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(i + 1).copied() == Some('*') {
            i += 2;
            while i < n && !(chars[i] == '*' && chars.get(i + 1).copied() == Some('/')) {
                i += 1;
            }
            i += 2;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    // Trailing commas before `}` or `]` (path-aliases.ts:102-103).
    strip_trailing_commas(&out)
}

/// Remove a comma that directly precedes `}` / `]` (ignoring whitespace).
fn strip_trailing_commas(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < n {
        if chars[i] == ',' {
            let mut j = i + 1;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            if j < n && (chars[j] == '}' || chars[j] == ']') {
                // Drop the comma but keep the whitespace run.
                i += 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Split a pattern around its `*` wildcard (`splitWildcard`, `path-aliases.ts:124-136`).
fn split_wildcard(pattern: &str) -> (String, String, bool) {
    match pattern.find('*') {
        None => (pattern.to_string(), String::new(), false),
        Some(star) => (
            pattern[..star].to_string(),
            pattern[star + 1..].to_string(),
            true,
        ),
    }
}

/// Load aliases for `project_root` (`loadProjectAliases`, `path-aliases.ts:145-200`).
///
/// Returns `None` when no tsconfig/jsconfig with usable `paths` is present.
pub fn load_project_aliases(project_root: &str) -> Option<AliasMap> {
    let candidates = ["tsconfig.json", "jsconfig.json"];
    let mut raw: Option<Value> = None;
    for name in candidates {
        let p = Path::new(project_root).join(name);
        if p.exists() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(parsed) = serde_json::from_str::<Value>(&strip_jsonc(&text)) {
                    if parsed.is_object() {
                        raw = Some(parsed);
                        break;
                    }
                }
            }
        }
    }
    let raw = raw?;

    let compiler_options = raw.get("compilerOptions").and_then(Value::as_object);
    let base_url_rel = compiler_options
        .and_then(|co| co.get("baseUrl"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let base_url = pathutil::resolve(project_root, base_url_rel);

    let paths = compiler_options.and_then(|co| co.get("paths"))?;
    let paths = paths.as_object()?;

    let mut patterns: Vec<AliasPattern> = Vec::new();
    for (pattern, targets) in paths {
        let Some(targets) = targets.as_array() else {
            continue;
        };
        let filtered: Vec<String> = targets
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if filtered.is_empty() {
            continue;
        }
        let (prefix, suffix, has_wildcard) = split_wildcard(pattern);
        patterns.push(AliasPattern {
            prefix,
            suffix,
            has_wildcard,
            replacements: filtered,
        });
    }
    if patterns.is_empty() {
        return None;
    }

    // Specificity sort (path-aliases.ts:187-191): longer prefix first, then
    // literal before wildcard. Use a stable sort to keep equal items in order.
    patterns.sort_by(|a, b| {
        if a.prefix.len() != b.prefix.len() {
            return b.prefix.len().cmp(&a.prefix.len());
        }
        if a.has_wildcard != b.has_wildcard {
            return if a.has_wildcard {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            };
        }
        std::cmp::Ordering::Equal
    });

    Some(AliasMap { base_url, patterns })
}

/// Resolve an import path through an [`AliasMap`] (`applyAliases`,
/// `path-aliases.ts:211-242`).
///
/// Returns candidate project-relative paths in tsconfig priority order, or an
/// empty vec when no alias matches. Callers still apply the language's extension
/// list to each candidate.
pub fn apply_aliases(import_path: &str, aliases: &AliasMap, project_root: &str) -> Vec<String> {
    for pat in &aliases.patterns {
        if !import_path.starts_with(&pat.prefix) {
            continue;
        }
        if !pat.suffix.is_empty() && !import_path.ends_with(&pat.suffix) {
            continue;
        }

        let captured = if pat.has_wildcard {
            import_path[pat.prefix.len()..import_path.len() - pat.suffix.len()].to_string()
        } else if import_path != pat.prefix {
            // Literal pattern must match exactly.
            continue;
        } else {
            String::new()
        };

        let mut out: Vec<String> = Vec::new();
        for target in &pat.replacements {
            let filled = if pat.has_wildcard {
                target.replacen('*', &captured, 1)
            } else {
                target.clone()
            };
            let absolute = pathutil::resolve(&aliases.base_url, &filled);
            let rel = pathutil::relative(project_root, &absolute);
            // Skip rewrites that escape the project root (path-aliases.ts:235-236).
            if rel.starts_with("..") {
                continue;
            }
            out.push(rel);
        }
        return out;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("cg-aliases-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).expect("mkdir temp");
        p
    }

    #[test]
    fn strip_jsonc_removes_line_and_block_comments() {
        let src = "{\n  // line\n  \"a\": 1, /* block */ \"b\": 2\n}";
        let out = strip_jsonc(src);
        assert!(!out.contains("line"));
        assert!(!out.contains("block"));
        let parsed: Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["b"], 2);
    }

    #[test]
    fn strip_jsonc_keeps_slashes_inside_strings() {
        let src = "{ \"url\": \"http://x/y\", \"c\": \"a//b\" }";
        let out = strip_jsonc(src);
        assert!(out.contains("http://x/y"));
        assert!(out.contains("a//b"));
    }

    #[test]
    fn strip_jsonc_handles_escaped_quote_in_string() {
        let src = "{ \"a\": \"esc\\\"//still\" }";
        let out = strip_jsonc(src);
        assert!(out.contains("esc\\\"//still"));
    }

    #[test]
    fn strip_trailing_commas_before_close() {
        assert_eq!(strip_trailing_commas("[1, 2, ]"), "[1, 2 ]");
        assert_eq!(strip_trailing_commas("{\"a\":1, }"), "{\"a\":1 }");
        assert_eq!(strip_trailing_commas("[1,2]"), "[1,2]");
    }

    #[test]
    fn split_wildcard_with_and_without_star() {
        assert_eq!(
            split_wildcard("@app/*"),
            ("@app/".to_string(), String::new(), true)
        );
        assert_eq!(
            split_wildcard("@lib"),
            ("@lib".to_string(), String::new(), false)
        );
        assert_eq!(
            split_wildcard("a/*.ext"),
            ("a/".to_string(), ".ext".to_string(), true)
        );
    }

    #[test]
    fn load_project_aliases_reads_tsconfig_paths() {
        let root = temp_dir("ts");
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": "./src", "paths": { "@app/*": ["app/*"], "@lib": ["lib/index"] } } }"#,
        )
        .unwrap();
        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert!(map.base_url.ends_with("/src"));
        // Longer prefix first (specificity sort): "@app/" (5) before "@lib" (4).
        assert_eq!(map.patterns[0].prefix, "@app/");
        assert!(map.patterns[0].has_wildcard);
        assert_eq!(map.patterns[1].prefix, "@lib");
        assert!(!map.patterns[1].has_wildcard);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_falls_back_to_jsconfig() {
        let root = temp_dir("js");
        std::fs::write(
            root.join("jsconfig.json"),
            r#"{ "compilerOptions": { "paths": { "~/*": ["./*"] } } }"#,
        )
        .unwrap();
        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(map.patterns[0].prefix, "~/");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_none_when_no_config() {
        let root = temp_dir("none");
        assert!(load_project_aliases(root.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_none_when_no_paths_key() {
        let root = temp_dir("nopaths");
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": "." } }"#,
        )
        .unwrap();
        assert!(load_project_aliases(root.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_skips_empty_and_non_array_targets() {
        let root = temp_dir("empty");
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{ "compilerOptions": { "paths": { "a/*": [], "b": "notarray", "c/*": ["c/*"] } } }"#,
        )
        .unwrap();
        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(map.patterns.len(), 1);
        assert_eq!(map.patterns[0].prefix, "c/");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_default_baseurl_is_root() {
        let root = temp_dir("defbase");
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#,
        )
        .unwrap();
        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(map.base_url, root.to_str().unwrap());
        std::fs::remove_dir_all(&root).ok();
    }

    fn wildcard_map() -> AliasMap {
        AliasMap {
            base_url: "/proj/src".to_string(),
            patterns: vec![
                AliasPattern {
                    prefix: "@app/".to_string(),
                    suffix: String::new(),
                    has_wildcard: true,
                    replacements: vec!["app/*".to_string(), "fallback/*".to_string()],
                },
                AliasPattern {
                    prefix: "@lib".to_string(),
                    suffix: String::new(),
                    has_wildcard: false,
                    replacements: vec!["lib/index".to_string()],
                },
            ],
        }
    }

    #[test]
    fn apply_aliases_wildcard_expands_all_targets() {
        let out = apply_aliases("@app/widgets/x", &wildcard_map(), "/proj");
        assert_eq!(out, vec!["src/app/widgets/x", "src/fallback/widgets/x"]);
    }

    #[test]
    fn apply_aliases_literal_exact_match() {
        let out = apply_aliases("@lib", &wildcard_map(), "/proj");
        assert_eq!(out, vec!["src/lib/index"]);
    }

    #[test]
    fn apply_aliases_literal_no_partial_match() {
        let out = apply_aliases("@libextra", &wildcard_map(), "/proj");
        assert!(out.is_empty());
    }

    #[test]
    fn apply_aliases_no_match_returns_empty() {
        let out = apply_aliases("react", &wildcard_map(), "/proj");
        assert!(out.is_empty());
    }

    #[test]
    fn apply_aliases_skips_rewrites_escaping_root() {
        let map = AliasMap {
            base_url: "/proj/src".to_string(),
            patterns: vec![AliasPattern {
                prefix: "@up/".to_string(),
                suffix: String::new(),
                has_wildcard: true,
                replacements: vec!["../../outside/*".to_string()],
            }],
        };
        let out = apply_aliases("@up/x", &map, "/proj");
        assert!(out.is_empty());
    }

    #[test]
    fn apply_aliases_suffix_must_match() {
        let map = AliasMap {
            base_url: "/proj".to_string(),
            patterns: vec![AliasPattern {
                prefix: "a/".to_string(),
                suffix: ".vue".to_string(),
                has_wildcard: true,
                replacements: vec!["comp/*.vue".to_string()],
            }],
        };
        assert!(apply_aliases("a/Button.ts", &map, "/proj").is_empty());
        assert_eq!(
            apply_aliases("a/Button.vue", &map, "/proj"),
            vec!["comp/Button.vue"]
        );
    }

    #[test]
    fn alias_types_derive_debug_clone_eq() {
        let m = wildcard_map();
        let cloned = m.clone();
        assert_eq!(m, cloned);
        assert!(format!("{m:?}").contains("AliasMap"));
        assert!(format!("{:?}", m.patterns[0]).contains("AliasPattern"));
    }
}
