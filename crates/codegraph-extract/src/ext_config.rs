//! Reader for `.codegraph/codegraph.json` custom extension overrides:
//! `{"extensions": {".x": "lua"}}`. Keys normalized (dot stripped, lowercased);
//! languages parse via `Language` serde names (unknown skipped). The parsed map
//! is mtime-cached per config-file path; "absent" is cached too, so a project
//! with no codegraph.json pays no repeated I/O.

use codegraph_core::types::Language;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Debug, Deserialize)]
struct CodegraphJson {
    #[serde(default)]
    extensions: HashMap<String, String>,
}

type ExtMap = HashMap<String, Language>;

#[derive(Clone)]
enum CacheEntry {
    Absent,
    Present { mtime: SystemTime, map: Arc<ExtMap> },
}

fn cache() -> &'static Mutex<HashMap<PathBuf, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Override language for `ext` (lowercased, no dot), resolving the nearest
/// `.codegraph/codegraph.json` walking up from `path`. `None` when no config is
/// reachable, parse failed, or the extension is unmapped.
pub fn override_language_for(path: &Path, ext: &str) -> Option<Language> {
    let config_path = find_config_path(path)?;
    let map = load_cached(&config_path)?;
    map.get(ext).copied()
}

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

fn load_cached(config_path: &Path) -> Option<Arc<ExtMap>> {
    let current_mtime = std::fs::metadata(config_path)
        .and_then(|m| m.modified())
        .ok();
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());

    if let Some(entry) = guard.get(config_path) {
        match (entry, current_mtime) {
            (CacheEntry::Present { mtime, map }, Some(now)) if *mtime == now => {
                return Some(Arc::clone(map));
            }
            (CacheEntry::Absent, None) => return None,
            _ => {}
        }
    }

    let Some(mtime) = current_mtime else {
        guard.insert(config_path.to_path_buf(), CacheEntry::Absent);
        return None;
    };

    let map = Arc::new(parse_config(config_path));
    guard.insert(
        config_path.to_path_buf(),
        CacheEntry::Present {
            mtime,
            map: Arc::clone(&map),
        },
    );
    Some(map)
}

fn parse_config(config_path: &Path) -> ExtMap {
    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return ExtMap::new();
    };
    let parsed: CodegraphJson = match serde_json::from_str(&contents) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                target: "codegraph_extract::ext_config",
                path = %config_path.display(),
                %error,
                "ignoring malformed .codegraph/codegraph.json"
            );
            return ExtMap::new();
        }
    };

    let mut map = ExtMap::new();
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
    map
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
