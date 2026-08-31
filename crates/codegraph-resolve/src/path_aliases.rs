//! Project-level import-path alias loading.
//!
//! Ports `upstream resolution/path-aliases.ts`. Reads
//! `compilerOptions.paths` from `tsconfig.json` / `jsconfig.json` at the project
//! root and converts the patterns into a form the import resolver can consult
//! (`path-aliases.ts:1-24`). Reads `tsconfig.json`, then `jsconfig.json`, then
//! `tsconfig.base.json`; follows bounded `extends` chains; honors config-relative
//! `baseUrl` + `paths`; and supports the single `*` wildcard. Vite/webpack
//! configs remain out of scope.

use crate::pathutil;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Default)]
struct EffectiveOptions {
    /// Already resolved against the config file that declared it.
    base_url: Option<String>,
    /// `paths` replaces its inherited value wholesale, matching TypeScript.
    paths: Option<serde_json::Map<String, Value>>,
    /// Directory of the config that declared `paths`; used when no `baseUrl`
    /// exists anywhere in the effective chain.
    paths_dir: Option<String>,
}

/// Real-world chains are normally one to three files deep. Keep malformed or
/// adversarial projects bounded and deterministic.
const MAX_EXTENDS_DEPTH: usize = 32;

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

fn path_to_posix(path: &Path) -> String {
    pathutil::normalize(&path.to_string_lossy().replace('\\', "/"))
}

fn resolve_config_path(from_dir: &Path, spec: &str) -> PathBuf {
    let path = Path::new(spec);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        from_dir.join(path)
    }
}

fn append_json_extension(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(".json");
    PathBuf::from(raw)
}

fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Locate a TypeScript `extends` target. Relative and absolute values are
/// resolved from the referencing config; package specifiers are searched under
/// `node_modules` while walking toward the filesystem root. Every form accepts
/// an explicit file, an implied `.json`, or a directory `tsconfig.json`.
fn resolve_extends_target(spec: &str, from_dir: &Path) -> Option<PathBuf> {
    let candidates_for = |base: PathBuf| {
        [
            base.clone(),
            append_json_extension(&base),
            base.join("tsconfig.json"),
        ]
    };

    if spec.starts_with("./") || spec.starts_with("../") || Path::new(spec).is_absolute() {
        return candidates_for(resolve_config_path(from_dir, spec))
            .into_iter()
            .find(|candidate| is_file(candidate));
    }

    for dir in from_dir.ancestors() {
        let base = dir.join("node_modules").join(spec);
        if let Some(candidate) = candidates_for(base)
            .into_iter()
            .find(|candidate| is_file(candidate))
        {
            return Some(candidate);
        }
    }
    None
}

fn read_tsconfig_like(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<Value>(&strip_jsonc(&text)).ok()?;
    parsed.is_object().then_some(parsed)
}

/// Fold one config's parents first, then apply the nearest config. A cycle or
/// overlong chain stops that parent branch while preserving options already
/// reached through the rest of the chain.
fn load_effective_options(
    file_path: &Path,
    stack: &mut HashSet<String>,
    depth: usize,
) -> Option<EffectiveOptions> {
    let key = path_to_posix(file_path);
    if depth > MAX_EXTENDS_DEPTH || stack.contains(&key) {
        return None;
    }
    let raw = read_tsconfig_like(file_path)?;
    stack.insert(key.clone());

    let dir = file_path.parent().unwrap_or_else(|| Path::new(""));
    let mut effective = EffectiveOptions::default();
    let parents: Vec<&str> = match raw.get("extends") {
        Some(Value::String(parent)) => vec![parent.as_str()],
        Some(Value::Array(parents)) => parents.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    for parent in parents {
        let Some(target) = resolve_extends_target(parent, dir) else {
            continue;
        };
        let Some(inherited) = load_effective_options(&target, stack, depth + 1) else {
            continue;
        };
        if inherited.base_url.is_some() {
            effective.base_url = inherited.base_url;
        }
        if inherited.paths.is_some() {
            effective.paths = inherited.paths;
            effective.paths_dir = inherited.paths_dir;
        }
    }
    stack.remove(&key);

    let compiler_options = raw.get("compilerOptions").and_then(Value::as_object);
    if let Some(base_url) = compiler_options
        .and_then(|options| options.get("baseUrl"))
        .and_then(Value::as_str)
    {
        effective.base_url = Some(path_to_posix(&resolve_config_path(dir, base_url)));
    }
    if let Some(paths) = compiler_options
        .and_then(|options| options.get("paths"))
        .and_then(Value::as_object)
    {
        effective.paths = Some(paths.clone());
        effective.paths_dir = Some(path_to_posix(dir));
    }

    Some(effective)
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
/// Returns `None` when no candidate config or inherited parent has usable
/// `paths`. A nearer config replaces inherited `paths`; `baseUrl` is resolved
/// relative to the file that declared it.
pub fn load_project_aliases(project_root: &str) -> Option<AliasMap> {
    let candidates = ["tsconfig.json", "jsconfig.json", "tsconfig.base.json"];
    let mut effective: Option<EffectiveOptions> = None;
    for name in candidates {
        let p = Path::new(project_root).join(name);
        if !is_file(&p) {
            continue;
        }
        let Some(options) = load_effective_options(&p, &mut HashSet::new(), 0) else {
            continue;
        };
        if effective.is_none() {
            effective = Some(options.clone());
        }
        if options.paths.is_some() {
            effective = Some(options);
            break;
        }
    }
    let effective = effective?;
    let base_url = effective
        .base_url
        .or(effective.paths_dir)
        .unwrap_or_else(|| pathutil::normalize(project_root));
    let paths = effective.paths?;

    let mut patterns: Vec<AliasPattern> = Vec::new();
    for (pattern, targets) in &paths {
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
            if rel == ".." || rel.starts_with("../") {
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

    fn write_json(path: impl AsRef<Path>, value: Value) {
        let path = path.as_ref();
        std::fs::create_dir_all(path.parent().expect("config parent")).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
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
    fn load_project_aliases_follows_relative_multi_hop_extends() {
        let root = temp_dir("extends-relative");
        write_json(
            root.join("tsconfig.root.json"),
            serde_json::json!({
                "compilerOptions": {
                    "baseUrl": ".",
                    "paths": { "@app/*": ["packages/*/src"] }
                }
            }),
        );
        write_json(
            root.join("tsconfig.mid.json"),
            serde_json::json!({ "extends": "./tsconfig.root" }),
        );
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "extends": "./tsconfig.mid.json" }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("@app/ui", &map, root.to_str().unwrap()),
            vec!["packages/ui/src"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_accepts_absolute_extends_target() {
        let root = temp_dir("extends-absolute");
        let base = root.join("config/aliases.json");
        write_json(
            &base,
            serde_json::json!({
                "compilerOptions": { "paths": { "~/*": ["src/*"] } }
            }),
        );
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "extends": base.to_string_lossy() }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("~/thing", &map, root.to_str().unwrap()),
            vec!["config/src/thing"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_project_aliases_resolves_node_modules_package_extends() {
        let root = temp_dir("extends-package");
        write_json(
            root.join("node_modules/@acme/tsconfig/tsconfig.json"),
            serde_json::json!({
                "compilerOptions": { "paths": { "@acme/*": ["../../../src/*"] } }
            }),
        );
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "extends": "@acme/tsconfig" }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("@acme/thing", &map, root.to_str().unwrap()),
            vec!["src/thing"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn inherited_paths_and_baseurl_are_anchored_at_declaring_config() {
        let root = temp_dir("extends-anchor");
        write_json(
            root.join("config/tsconfig.base.json"),
            serde_json::json!({
                "compilerOptions": {
                    "baseUrl": "..",
                    "paths": { "~/*": ["src/*"] }
                }
            }),
        );
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "extends": "./config/tsconfig.base.json" }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("~/foo", &map, root.to_str().unwrap()),
            vec!["src/foo"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn child_paths_replace_parent_paths_and_nearest_baseurl_wins() {
        let root = temp_dir("extends-override");
        write_json(
            root.join("tsconfig.base.json"),
            serde_json::json!({
                "compilerOptions": {
                    "baseUrl": "base-dir",
                    "paths": {
                        "@x/*": ["from-base/*"],
                        "@parent/*": ["parent/*"]
                    }
                }
            }),
        );
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({
                "extends": "./tsconfig.base.json",
                "compilerOptions": {
                    "baseUrl": "own-dir",
                    "paths": { "@x/*": ["from-child/*"] }
                }
            }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("@x/y", &map, root.to_str().unwrap()),
            vec!["own-dir/from-child/y"]
        );
        assert!(apply_aliases("@parent/y", &map, root.to_str().unwrap()).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cyclic_extends_terminates_and_keeps_reached_options() {
        let root = temp_dir("extends-cycle");
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "extends": "./a.json" }),
        );
        write_json(
            root.join("a.json"),
            serde_json::json!({
                "extends": "./b.json",
                "compilerOptions": { "paths": { "@cycle/*": ["from-a/*"] } }
            }),
        );
        write_json(
            root.join("b.json"),
            serde_json::json!({ "extends": "./a.json" }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("@cycle/x", &map, root.to_str().unwrap()),
            vec!["from-a/x"]
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn extends_depth_is_bounded() {
        let root = temp_dir("extends-depth");
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "extends": "./c0.json" }),
        );
        for index in 0..32 {
            write_json(
                root.join(format!("c{index}.json")),
                serde_json::json!({ "extends": format!("./c{}.json", index + 1) }),
            );
        }
        write_json(
            root.join("c32.json"),
            serde_json::json!({
                "compilerOptions": { "paths": { "@too-deep/*": ["src/*"] } }
            }),
        );

        assert!(load_project_aliases(root.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn solution_root_falls_back_to_tsconfig_base() {
        let root = temp_dir("base-fallback");
        write_json(
            root.join("tsconfig.json"),
            serde_json::json!({ "files": [], "references": [] }),
        );
        write_json(
            root.join("tsconfig.base.json"),
            serde_json::json!({
                "compilerOptions": {
                    "baseUrl": ".",
                    "paths": { "@scope/*": ["libs/*/src/index.ts"] }
                }
            }),
        );

        let map = load_project_aliases(root.to_str().unwrap()).expect("aliases");
        assert_eq!(
            apply_aliases("@scope/lib", &map, root.to_str().unwrap()),
            vec!["libs/lib/src/index.ts"]
        );
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
