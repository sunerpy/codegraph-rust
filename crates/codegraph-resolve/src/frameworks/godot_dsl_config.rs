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
//! Same off-by-default + tolerant-parse + mtime-cache discipline as
//! `resourceFields`. All field names / kinds / separators / segment indices are
//! project-supplied; nothing is hardcoded. See [`dsl_id_fields`] and
//! [`super::godot_resource`]'s `dsl_id_targets`.
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
use std::collections::{BTreeMap, HashMap};
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
    #[serde(default, rename = "idFields")]
    id_fields: BTreeMap<String, IdFieldSpec>,
}

/// One opt-in `idFields` entry: how to turn a `.tres` `[resource]` property's
/// value into one or more `godot:id:<kind>:<value>` sentinel references.
///
/// `separator` + `id_segments` together select compound parts; with neither, the
/// whole quote-stripped value is the single ID. All fields are project-supplied.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct IdFieldSpec {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub separator: Option<String>,
    #[serde(default, rename = "idSegments")]
    pub id_segments: Option<Vec<usize>>,
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

/// Walk up from `path` to the nearest `.codegraph/codegraph.json`. The starting
/// directory is the file's own parent. `path` is expected to be ABSOLUTE — the
/// pipeline resolves the `.tres` against the project root before calling
/// (`godot_resource::config_lookup_path`), and the unit tests pass absolute
/// paths. A relative `path` is still tolerated (joined onto the CWD, matching
/// `ext_config::find_config_path`), but the pipeline never relies on that CWD
/// join: doing so silently mislocated the config whenever the CLI ran with its
/// CWD != the project root, which is the bug this resolution was hardened
/// against.
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

// ---------------------------------------------------------------------------
// `idFields` reader — a second, independent opt-in block (PR2). Same shape as
// the `resourceFields` reader above: tree-walk find_config_path + mtime cache +
// tolerant parse + "absent" caching, kept separate so `resourceFields` behavior
// is provably untouched.
// ---------------------------------------------------------------------------

/// The cached, parsed `idFields` spec map for one config-file path.
type IdFields = BTreeMap<String, IdFieldSpec>;

#[derive(Clone)]
enum IdCacheEntry {
    Absent,
    Present {
        mtime: SystemTime,
        fields: Arc<IdFields>,
    },
}

fn id_cache() -> &'static Mutex<HashMap<PathBuf, IdCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, IdCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The configured `idFields` spec map for the `.tres` at `path`, resolving the
/// nearest `.codegraph/codegraph.json` walking up from `path`. Returns an EMPTY
/// map when no config is reachable, the file is malformed, or no
/// `godot.dsl.idFields` block is present — the off-by-default case yields no ID
/// behavior.
pub(crate) fn dsl_id_fields(path: &Path) -> Arc<IdFields> {
    let Some(config_path) = find_config_path(path) else {
        return Arc::new(BTreeMap::new());
    };
    load_cached_id(&config_path)
}

/// Return the cached `idFields` map for `config_path`, re-parsing when the
/// file's mtime changed. Mirrors [`load_cached`]; "absent" is cached as an empty
/// map so it is not re-read.
fn load_cached_id(config_path: &Path) -> Arc<IdFields> {
    let current_mtime = std::fs::metadata(config_path)
        .and_then(|m| m.modified())
        .ok();
    let mut guard = id_cache().lock().unwrap_or_else(|p| p.into_inner());

    if let Some(entry) = guard.get(config_path) {
        match (entry, current_mtime) {
            (IdCacheEntry::Present { mtime, fields }, Some(now)) if *mtime == now => {
                return Arc::clone(fields);
            }
            (IdCacheEntry::Absent, None) => return Arc::new(BTreeMap::new()),
            _ => {}
        }
    }

    let Some(mtime) = current_mtime else {
        guard.insert(config_path.to_path_buf(), IdCacheEntry::Absent);
        return Arc::new(BTreeMap::new());
    };

    let fields = Arc::new(parse_id_config(config_path));
    guard.insert(
        config_path.to_path_buf(),
        IdCacheEntry::Present {
            mtime,
            fields: Arc::clone(&fields),
        },
    );
    fields
}

/// Parse `godot.dsl.idFields` out of the config file, tolerating any read/parse
/// failure as an empty map. Entry keys are trimmed; entries with an empty key or
/// empty `kind` are dropped (a sentinel needs a non-empty key + kind). Mirrors
/// [`parse_config`]'s tolerance.
fn parse_id_config(config_path: &Path) -> IdFields {
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return BTreeMap::new();
    };
    let Ok(parsed) = serde_json::from_str::<CodegraphJson>(&contents) else {
        return BTreeMap::new();
    };

    parsed
        .godot
        .dsl
        .id_fields
        .into_iter()
        .map(|(k, spec)| (k.trim().to_string(), spec))
        .filter(|(k, spec)| !k.is_empty() && !spec.kind.trim().is_empty())
        .collect()
}
