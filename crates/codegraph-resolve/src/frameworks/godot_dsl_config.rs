//! OPT-IN DSL config for Godot `.tres` resource fields (L5 / T9).
//!
//! Reads an OPTIONAL, OFF-by-default block from ONE project's current-root
//! `codegraph.json` ([`IndexPaths::extension_config`]):
//!
//! ```jsonc
//! { "godot": { "dsl": { "resourceFields": ["skill_effect", "effect_on"] } } }
//! ```
//!
//! Each listed field name is a `.tres` `[resource]` property whose VALUE should
//! become a reference edge from the resource to that target (see
//! [`super::godot_resource`]). WITHOUT this config the `.tres` parser emits ZERO
//! DSL edges — the config is the only trigger. The field list is entirely
//! project-supplied; nothing is hardcoded (`skill_effect`/`effect_on` are mere
//! examples).
//!
//! # `idFields` — opt-in bare/compound ID capture (PR2)
//!
//! A SECOND, independent opt-in block captures bare or compound IDs inside a
//! `.tres` `[resource]` body as `godot:id:<kind>:<value>` sentinel references:
//!
//! ```jsonc
//! { "godot": { "dsl": { "idFields": {
//!     "buff_id":      { "kind": "buff" },
//!     "skill_effect": { "kind": "skill", "separator": ":", "idSegments": [2, 4] }
//! } } } }
//! ```
//!
//! # Explicit, project-scoped, cache-free
//!
//! The config is loaded ONCE per operation from the addressed project's resolved
//! current root and threaded into extraction as an immutable
//! [`GodotDslConfig`]. Nothing here walks up the directory tree, consults the
//! process working directory, reads another project's `codegraph.json`, or caches
//! across calls — so two projects handled by one process can never see each
//! other's DSL fields and no mtime cache can go stale.
//!
//! Parsing stays tolerant (`#[serde(default)]` at every level, a malformed file
//! yielding the empty config), preserving the documented opt-in contract.

use codegraph_core::IndexPaths;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Top-level `codegraph.json` shape — only the `godot` key matters here; other
/// keys (e.g. `extensions`) are ignored. `#[serde(default)]` makes a file with no
/// `godot` key parse to an empty config.
#[derive(Debug, Default, Deserialize)]
struct CodegraphJson {
    #[serde(default)]
    godot: GodotConfig,
}

/// The `godot` block. Only `dsl` is read at L5.
#[derive(Debug, Default, Deserialize)]
struct GodotConfig {
    #[serde(default)]
    dsl: GodotDslConfigFile,
}

/// The raw `godot.dsl` block as it appears on disk.
#[derive(Debug, Default, Deserialize)]
struct GodotDslConfigFile {
    #[serde(default, rename = "resourceFields")]
    resource_fields: Vec<String>,
    #[serde(default, rename = "idFields")]
    id_fields: BTreeMap<String, IdFieldSpec>,
}

/// One opt-in `idFields` entry: how to turn a `.tres` `[resource]` property's
/// value into one or more `godot:id:<kind>:<value>` sentinel references.
///
/// `separator` + `id_segments` together select compound parts; with neither, the
/// whole quote-stripped value is the single ID. All fields are project-supplied.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
pub struct IdFieldSpec {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub separator: Option<String>,
    #[serde(default, rename = "idSegments")]
    pub id_segments: Option<Vec<usize>>,
}

/// One project's parsed, immutable Godot DSL configuration: the `resourceFields`
/// list and the `idFields` spec map. Both empty for a project that declares none
/// (the off-by-default case).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GodotDslConfig {
    resource_fields: Vec<String>,
    id_fields: BTreeMap<String, IdFieldSpec>,
}

impl GodotDslConfig {
    /// The empty config — no DSL resource fields, no ID fields.
    #[must_use]
    pub fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Load the DSL config declared by ONE project's current index root
    /// (`<current_root>/codegraph.json`). A missing, unreadable, or malformed
    /// file yields the empty config.
    #[must_use]
    pub fn load_for_paths(paths: &IndexPaths) -> Arc<Self> {
        Self::load_from_file(&paths.extension_config())
    }

    /// Load from an explicit `codegraph.json` path, with the same tolerance as
    /// [`Self::load_for_paths`].
    #[must_use]
    pub fn load_from_file(config_path: &Path) -> Arc<Self> {
        let Ok(contents) = std::fs::read_to_string(config_path) else {
            return Self::empty();
        };
        Arc::new(Self::parse(&contents))
    }

    /// Parse the `godot.dsl` block out of `contents`, tolerating any parse
    /// failure as the empty config. Field names are trimmed; empty names are
    /// dropped, and an `idFields` entry needs a non-empty key AND kind.
    #[must_use]
    pub fn parse(contents: &str) -> Self {
        // A malformed config is swallowed silently (not logged):
        // `codegraph-resolve` has no logging dependency, and the no-new-dep
        // posture forbids adding one.
        let Ok(parsed) = serde_json::from_str::<CodegraphJson>(contents) else {
            return Self::default();
        };
        let resource_fields = parsed
            .godot
            .dsl
            .resource_fields
            .into_iter()
            .map(|field| field.trim().to_string())
            .filter(|field| !field.is_empty())
            .collect();
        let id_fields = parsed
            .godot
            .dsl
            .id_fields
            .into_iter()
            .map(|(key, spec)| (key.trim().to_string(), spec))
            .filter(|(key, spec)| !key.is_empty() && !spec.kind.trim().is_empty())
            .collect();
        Self {
            resource_fields,
            id_fields,
        }
    }

    /// `true` when the project declared neither `resourceFields` nor `idFields`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resource_fields.is_empty() && self.id_fields.is_empty()
    }

    /// The configured `.tres` `[resource]` property names whose value becomes a
    /// reference target, in declaration order.
    #[must_use]
    pub fn resource_fields(&self) -> &[String] {
        &self.resource_fields
    }

    /// The configured `idFields` spec map, keyed by property name.
    #[must_use]
    pub fn id_fields(&self) -> &BTreeMap<String, IdFieldSpec> {
        &self.id_fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resource_and_id_fields() {
        let config = GodotDslConfig::parse(
            r#"{"godot":{"dsl":{"resourceFields":[" skill_effect ",""],
               "idFields":{"buff_id":{"kind":"buff"},
                           " skill ":{"kind":"skill","separator":":","idSegments":[0,2]},
                           "bad":{"kind":"  "}}}}}"#,
        );
        assert_eq!(config.resource_fields(), ["skill_effect"]);
        let ids = config.id_fields();
        assert_eq!(ids.len(), 2, "empty-kind entries are dropped: {ids:?}");
        assert_eq!(ids["buff_id"].kind, "buff");
        assert_eq!(ids["skill"].separator.as_deref(), Some(":"));
        assert_eq!(ids["skill"].id_segments.as_deref(), Some([0, 2].as_slice()));
    }

    #[test]
    fn absent_block_and_malformed_json_are_empty() {
        assert!(GodotDslConfig::parse(r#"{"extensions":{".zz":"lua"}}"#).is_empty());
        assert!(GodotDslConfig::parse("{ not json ").is_empty());
        assert!(GodotDslConfig::default().is_empty());
    }

    #[test]
    fn load_for_paths_reads_only_the_resolved_root_config() {
        let project = std::env::temp_dir().join(format!(
            "codegraph-godot-dsl-scoped-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(project.join(".codegraph")).unwrap();
        std::fs::write(
            project.join(".codegraph/codegraph.json"),
            r#"{"godot":{"dsl":{"resourceFields":["default_field"]}}}"#,
        )
        .unwrap();
        let paths = IndexPaths::resolve(&project, Some(".custom-codegraph"))
            .expect("resolve overridden paths");
        std::fs::create_dir_all(paths.current_root()).unwrap();
        std::fs::write(
            paths.extension_config(),
            r#"{"godot":{"dsl":{"resourceFields":["current_field"]}}}"#,
        )
        .unwrap();

        let config = GodotDslConfig::load_for_paths(&paths);
        assert_eq!(config.resource_fields(), ["current_field"]);

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn missing_current_root_config_is_empty() {
        let project = std::env::temp_dir().join(format!(
            "codegraph-godot-dsl-absent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&project).unwrap();
        let paths = IndexPaths::resolve(&project, None).expect("resolve paths");

        assert!(GodotDslConfig::load_for_paths(&paths).is_empty());

        let _ = std::fs::remove_dir_all(&project);
    }
}
