//! OPT-IN DSL config reader for Godot `.tres` resource fields (L5 / T9).
//!
//! Reads an OPTIONAL, OFF-by-default block from `.codegraph/codegraph.json`:
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
//! # Mirrors `codegraph_extract::ext_config`
//!
//! The reading strategy is intentionally identical to the existing
//! `.codegraph/codegraph.json` extension-override reader: walk up the directory
//! tree from the file being parsed to find the nearest config, parse it with
//! `serde` using `#[serde(default)]` at every level (so any missing key yields
//! the empty list), tolerate a malformed file (logged + ignored, never panics),
//! and mtime-cache the parsed result per config-file path — caching "absent"
//! too, so a project with no `godot.dsl` block pays no repeated I/O.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Top-level `.codegraph/codegraph.json` shape — only the `godot` key matters
/// here; other keys (e.g. `extensions`) are ignored. `#[serde(default)]` makes a
/// file with no `godot` key parse to an empty config.
#[derive(Debug, Default, Deserialize)]
struct CodegraphJson {
    #[serde(default)]
    godot: GodotConfig,
}

/// The `godot` block. Only `dsl` is read at L5.
#[derive(Debug, Default, Deserialize)]
struct GodotConfig {
    #[serde(default)]
    dsl: GodotDslConfig,
}

/// The `godot.dsl` block. `resourceFields` lists the `.tres` `[resource]`
/// property names that should emit a reference edge from their value.
#[derive(Debug, Default, Deserialize)]
struct GodotDslConfig {
    #[serde(default, rename = "resourceFields")]
    resource_fields: Vec<String>,
}

/// The cached, parsed DSL field list for one config-file path.
type DslFields = Vec<String>;

#[derive(Clone)]
enum CacheEntry {
    Absent,
    Present {
        mtime: SystemTime,
        fields: Arc<DslFields>,
    },
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The configured DSL resource-field names for the `.tres` at `path`, resolving
/// the nearest `.codegraph/codegraph.json` walking up from `path`. Returns an
/// EMPTY list when no config is reachable, the file is malformed, or no
/// `godot.dsl.resourceFields` block is present — i.e. the off-by-default case
/// yields no DSL behavior.
pub(crate) fn dsl_resource_fields(path: &Path) -> Arc<DslFields> {
    let Some(config_path) = find_config_path(path) else {
        return Arc::new(Vec::new());
    };
    load_cached(&config_path)
}

/// Walk up from `path` to the nearest `.codegraph/codegraph.json`. IDENTICAL in
/// shape to `ext_config::find_config_path` (absolute paths use the file's own
/// parent; relative paths are joined onto the cwd first).
fn find_config_path(file_path: &Path) -> Option<PathBuf> {
    let start = if file_path.is_absolute() {
        file_path.parent().map(Path::to_path_buf)
    } else {
        std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join(file_path))
            .and_then(|abs| abs.parent().map(Path::to_path_buf))
    }?;
    let mut dir: Option<&Path> = Some(start.as_path());
    while let Some(current) = dir {
        let candidate = current.join(".codegraph").join("codegraph.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = current.parent();
    }
    None
}

/// Return the cached DSL field list for `config_path`, re-parsing when the
/// file's mtime changed. Mirrors `ext_config::load_cached`; "absent" (unreadable
/// file) is cached as an empty list so it is not re-read.
fn load_cached(config_path: &Path) -> Arc<DslFields> {
    let current_mtime = std::fs::metadata(config_path)
        .and_then(|m| m.modified())
        .ok();
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());

    if let Some(entry) = guard.get(config_path) {
        match (entry, current_mtime) {
            (CacheEntry::Present { mtime, fields }, Some(now)) if *mtime == now => {
                return Arc::clone(fields);
            }
            (CacheEntry::Absent, None) => return Arc::new(Vec::new()),
            _ => {}
        }
    }

    let Some(mtime) = current_mtime else {
        guard.insert(config_path.to_path_buf(), CacheEntry::Absent);
        return Arc::new(Vec::new());
    };

    let fields = Arc::new(parse_config(config_path));
    guard.insert(
        config_path.to_path_buf(),
        CacheEntry::Present {
            mtime,
            fields: Arc::clone(&fields),
        },
    );
    fields
}

/// Parse `godot.dsl.resourceFields` out of the config file, tolerating any
/// read/parse failure as an empty list (logged). Field names are trimmed; empty
/// names are dropped. Mirrors `ext_config::parse_config`'s tolerance.
fn parse_config(config_path: &Path) -> DslFields {
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return Vec::new();
    };
    // Malformed config is swallowed silently (not logged): `codegraph-resolve`
    // has no logging dependency, and the no-new-dep posture forbids adding one.
    let Ok(parsed) = serde_json::from_str::<CodegraphJson>(&contents) else {
        return Vec::new();
    };

    parsed
        .godot
        .dsl
        .resource_fields
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect()
}
