//! Reader for the PROJECT-SCOPED `codegraph.json` extension overrides:
//! `{"extensions": {".x": "lua"}}`. Keys normalized (dot stripped, lowercased);
//! languages parse via `Language` serde names (unknown skipped).
//!
//! The file read is ALWAYS the caller-supplied current-root
//! [`IndexPaths::extension_config`](codegraph_core::IndexPaths::extension_config)
//! path: nothing here walks the directory tree, consults the process working
//! directory, or reads a legacy `.codegraph/codegraph.json`. The parsed result is
//! an immutable [`ExtensionOverrides`] value that the caller threads through
//! [`crate::ExtractOptions`], so two projects served by ONE process cannot see
//! each other's overrides and no cache can go stale.
//!
//! Absence and malformed content stay tolerant (empty overrides, logged), which
//! is the documented opt-in contract: a project with no `codegraph.json` behaves
//! byte-identically to a build without the feature.

use codegraph_core::IndexPaths;
use codegraph_core::types::Language;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct CodegraphJson {
    #[serde(default)]
    extensions: BTreeMap<String, String>,
}

/// One project's parsed extension overrides: lowercased dot-free extension →
/// language. Empty when the project declares none.
///
/// Deterministic: the backing map is ordered, and the value is immutable once
/// built, so every consumer of the same `Arc` observes identical bytes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExtensionOverrides {
    map: BTreeMap<String, Language>,
}

impl ExtensionOverrides {
    /// No overrides — the zero-config default.
    #[must_use]
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Load the overrides declared by ONE project's current index root.
    ///
    /// Reads exactly `paths.extension_config()`. A missing file yields empty
    /// overrides; an unreadable or malformed file is tolerated as empty and
    /// logged, matching the opt-in contract.
    #[must_use]
    pub fn load_for_paths(paths: &IndexPaths) -> Arc<Self> {
        Self::load_from_file(&paths.extension_config())
    }

    /// Load overrides from an explicit `codegraph.json` path. Exposed so a
    /// caller that already resolved the file (or a test fixture) can supply it
    /// directly; the tolerance rules are identical to [`Self::load_for_paths`].
    #[must_use]
    pub fn load_from_file(config_path: &Path) -> Arc<Self> {
        let Ok(contents) = std::fs::read_to_string(config_path) else {
            return Self::empty();
        };
        Arc::new(Self::parse(&contents, config_path))
    }

    fn parse(contents: &str, config_path: &Path) -> Self {
        let parsed: CodegraphJson = match serde_json::from_str(contents) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(
                    target: "codegraph_extract::ext_config",
                    path = %config_path.display(),
                    %error,
                    "ignoring malformed codegraph.json"
                );
                return Self::default();
            }
        };

        let mut map = BTreeMap::new();
        for (raw_ext, raw_lang) in parsed.extensions {
            let ext = normalize_ext(&raw_ext);
            if ext.is_empty() {
                continue;
            }
            match parse_language(&raw_lang) {
                Some(language) => {
                    map.insert(ext, language);
                }
                None => {
                    tracing::warn!(
                        target: "codegraph_extract::ext_config",
                        extension = %raw_ext,
                        language = %raw_lang,
                        "ignoring unknown language in codegraph.json extensions"
                    );
                }
            }
        }
        Self { map }
    }

    /// `true` when the project declared no usable override.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The override language for `ext` (lowercased, no leading dot), if any.
    #[must_use]
    pub fn language_for(&self, ext: &str) -> Option<Language> {
        self.map.get(ext).copied()
    }
}

fn normalize_ext(raw: &str) -> String {
    raw.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn parse_language(raw: &str) -> Option<Language> {
    let language: Language =
        serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()?;
    if language == Language::Unknown {
        return None;
    }
    Some(language)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(json: &str) -> ExtensionOverrides {
        ExtensionOverrides::parse(json, Path::new("codegraph.json"))
    }

    #[test]
    fn parses_normalized_keys_and_known_languages() {
        let parsed = overrides(r#"{"extensions":{".MyExt":"lua","X":"go"}}"#);
        assert_eq!(parsed.language_for("myext"), Some(Language::Lua));
        assert_eq!(parsed.language_for("x"), Some(Language::Go));
        assert!(!parsed.is_empty());
    }

    #[test]
    fn skips_unknown_languages_and_empty_keys() {
        let parsed = overrides(r#"{"extensions":{".foo":"klingon",".":"lua","  ":"go"}}"#);
        assert_eq!(parsed.language_for("foo"), None);
        assert!(parsed.is_empty());
    }

    #[test]
    fn malformed_json_yields_empty_overrides() {
        assert!(overrides("{ not json ").is_empty());
    }

    #[test]
    fn missing_file_yields_empty_overrides() {
        let missing = std::env::temp_dir().join(format!(
            "codegraph-ext-missing-{}-{}/codegraph.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(ExtensionOverrides::load_from_file(&missing).is_empty());
    }

    /// The reader consults the caller's current-root path only: a legacy
    /// `.codegraph/codegraph.json` next to it is never adopted.
    #[test]
    fn load_for_paths_reads_only_the_current_root_config() {
        let project = std::env::temp_dir().join(format!(
            "codegraph-ext-scoped-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(project.join(".codegraph")).unwrap();
        std::fs::write(
            project.join(".codegraph/codegraph.json"),
            r#"{"extensions":{".zz":"go"}}"#,
        )
        .unwrap();
        let paths = IndexPaths::resolve(&project, None).expect("resolve paths");
        std::fs::create_dir_all(paths.current_root()).unwrap();
        std::fs::write(paths.extension_config(), r#"{"extensions":{".zz":"lua"}}"#).unwrap();

        let loaded = ExtensionOverrides::load_for_paths(&paths);
        assert_eq!(loaded.language_for("zz"), Some(Language::Lua));

        let _ = std::fs::remove_dir_all(&project);
    }
}
