//! Ranking-only project path de-prioritization.
//!
//! `codegraph.json` and `[indexing].deprioritize` describe source that must stay
//! indexed and directly retrievable while losing search/explore ranking ties to
//! first-party code. Matching a rule never changes files, nodes, edges, node
//! ids, or extraction goldens.

use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};

use crate::{IndexPaths, config::Config};

/// Ordered, immutable last-match-wins matcher for project-relative paths.
#[derive(Debug, Clone, Default)]
pub struct DeprioritizeMatcher {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Rule {
    matchers: Vec<GlobMatcher>,
    deprioritize: bool,
}

impl DeprioritizeMatcher {
    /// Load one project's ranking rules.
    ///
    /// The tolerant JSON compatibility source runs first. TOML then appends its
    /// rules and therefore has the final say under last-match-wins semantics.
    #[must_use]
    pub fn load_for_paths(paths: &IndexPaths, config: &Config) -> Self {
        let mut patterns = load_json_patterns(&paths.extension_config());
        patterns.extend(config.indexing.deprioritize.iter().cloned());
        Self::from_patterns(patterns)
    }

    /// Compile an ordered rule stream. Blank or invalid rules are ignored.
    #[must_use]
    pub fn from_patterns(patterns: impl IntoIterator<Item = String>) -> Self {
        let rules = patterns
            .into_iter()
            .filter_map(|pattern| compile_rule(&pattern))
            .collect();
        Self { rules }
    }

    /// Whether `path` is de-prioritized after applying every matching rule.
    #[must_use]
    pub fn is_match(&self, path: &str) -> bool {
        let normalized = normalize_path(path);
        let mut deprioritized = false;
        for rule in &self.rules {
            if rule
                .matchers
                .iter()
                .any(|matcher| matcher.is_match(&normalized))
            {
                deprioritized = rule.deprioritize;
            }
        }
        deprioritized
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

fn load_json_patterns(path: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let parsed: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                target: "codegraph_core::deprioritize",
                path = %path.display(),
                %error,
                "ignoring malformed codegraph.json ranking config"
            );
            return Vec::new();
        }
    };
    let Some(raw) = parsed.get("deprioritize") else {
        return Vec::new();
    };
    let Some(entries) = raw.as_array() else {
        tracing::warn!(
            target: "codegraph_core::deprioritize",
            path = %path.display(),
            "ignoring codegraph.json deprioritize: expected an array"
        );
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let Some(pattern) = entry.as_str() else {
                tracing::warn!(
                    target: "codegraph_core::deprioritize",
                    path = %path.display(),
                    "ignoring non-string codegraph.json deprioritize rule"
                );
                return None;
            };
            let trimmed = pattern.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect()
}

fn compile_rule(raw: &str) -> Option<Rule> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (deprioritize, body) = match trimmed.strip_prefix('!') {
        Some(body) => (false, body.trim()),
        None => (true, trimmed),
    };
    if body.is_empty() {
        return None;
    }

    let normalized = normalize_pattern(body);
    if normalized.is_empty() {
        return None;
    }
    let mut globs = vec![normalized.clone()];

    // Gitignore-style bare patterns match a path component at any depth.
    if !normalized.contains('/') {
        globs.push(format!("**/{normalized}"));
        globs.push(format!("**/{normalized}/**"));
    } else if !has_glob_meta(&normalized) {
        // A literal directory pattern also covers its descendants.
        globs.push(format!("{normalized}/**"));
    }

    let mut matchers = Vec::new();
    for glob in globs {
        match GlobBuilder::new(&glob)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
        {
            Ok(glob) => matchers.push(glob.compile_matcher()),
            Err(error) => {
                tracing::warn!(
                    target: "codegraph_core::deprioritize",
                    pattern = %raw,
                    %error,
                    "ignoring invalid deprioritize glob"
                );
            }
        }
    }
    (!matchers.is_empty()).then_some(Rule {
        matchers,
        deprioritize,
    })
}

fn normalize_pattern(pattern: &str) -> String {
    let trailing_slash = pattern.trim().ends_with(['/', '\\']);
    let mut normalized = normalize_path(pattern);
    if trailing_slash {
        normalized.push_str("/**");
    }
    normalized
}

fn normalize_path(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_string();
    }
    normalized = normalized.trim_start_matches('/').to_string();
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }
    normalized.trim_end_matches('/').to_string()
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'{'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(patterns: &[&str]) -> DeprioritizeMatcher {
        DeprioritizeMatcher::from_patterns(patterns.iter().map(|p| (*p).to_string()))
    }

    #[test]
    fn ordered_rules_are_last_match_wins() {
        let matcher = matcher(&[
            "vendor/**",
            "!vendor/first-party/**",
            "vendor/first-party/generated/**",
        ]);
        assert!(matcher.is_match("vendor/sdk/client.rs"));
        assert!(!matcher.is_match("vendor/first-party/api.rs"));
        assert!(matcher.is_match("vendor/first-party/generated/api.rs"));
    }

    #[test]
    fn bare_and_windows_patterns_match_components() {
        let matcher = matcher(&["fixtures", "generated/**"]);
        assert!(matcher.is_match(r"crates\demo\fixtures\sample.rs"));
        assert!(matcher.is_match("generated/schema.rs"));
        assert!(!matcher.is_match("src/generated_helper.rs"));
    }

    #[test]
    fn blanks_and_invalid_globs_are_ignored() {
        let matcher = matcher(&["", "   ", "["]);
        assert!(matcher.is_empty());
    }

    #[test]
    fn json_rules_run_before_authoritative_toml_rules() {
        let root = std::env::temp_dir().join(format!(
            "codegraph-deprioritize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let paths = IndexPaths::resolve(&root, None).unwrap();
        std::fs::create_dir_all(paths.current_root()).unwrap();
        std::fs::write(
            paths.extension_config(),
            r#"{"deprioritize":["vendor/**","!vendor/keep/**",42,""]}"#,
        )
        .unwrap();
        let mut config = Config::default();
        config.indexing.deprioritize = vec!["vendor/keep/**".to_string()];

        let matcher = DeprioritizeMatcher::load_for_paths(&paths, &config);
        assert!(matcher.is_match("vendor/sdk/a.rs"));
        assert!(
            matcher.is_match("vendor/keep/a.rs"),
            "later TOML rule must override JSON negation"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
