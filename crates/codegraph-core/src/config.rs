//! Configuration module for CodeGraph.
//!
//! Reads `<project_root>/.codegraph/config.toml` via Pattern B (runtime discovery).
//! Config is optional — missing file uses all defaults, matching the upstream zero-config UX.
//!
//! ### Config Sources
//! - `max_file_size`: upstream extraction/index.ts:101 (skip files >1MB)
//! - `ignore_dirs`: upstream extraction/index.ts:117-145 (default per-ecosystem dirs)
//! - `watch`: upstream sync/watch-policy.ts (debounce, enable/disable)
//!
//! ### Defaults
//! - app.log_level: "info"
//! - indexing.max_file_size: 1048576 bytes
//! - indexing.ignore_dirs: standard per-ecosystem names (node_modules, target, dist, etc.)
//! - watch.enabled: true
//! - watch.debounce_ms: 2000
//!
//! Loaded once into a global OnceLock; consumers borrow &'static Config.
//! For libraries: this module returns Result; the binary owns the failure policy.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub app: AppConfig,
    #[serde(default)]
    pub indexing: IndexingConfig,
    #[serde(default)]
    pub watch: WatchConfig,
}

/// Application settings.
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub name: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Indexing configuration.
/// upstream extraction/index.ts:101 (MAX_FILE_SIZE)
/// upstream extraction/index.ts:117-145 (DEFAULT_IGNORE_DIRS)
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IndexingConfig {
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,
    #[serde(default = "default_ignore_dirs")]
    pub ignore_dirs: Vec<String>,
    /// Root-relative path patterns excluded by default, expressed in the
    /// `.gitignore`-style matcher (see [`default_ignore_paths`]). Unlike
    /// [`ignore_dirs`] (single directory basenames matched anywhere), these are
    /// PATH patterns so an Android `res/values` subtree can be excluded while a
    /// same-named component elsewhere is not. Overridable in `config.toml`.
    #[serde(default = "default_ignore_paths")]
    pub ignore_paths: Vec<String>,
    /// Root-relative path patterns skipped during the walk, alongside
    /// `ignore_dirs`/`.gitignore`. Same matcher as `.gitignore` (`static/`,
    /// `docs/gen`, `gen*`); honored by index and sync. Off by default.
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_max_file_size() -> u64 {
    // upstream extraction/index.ts:101
    // Skip files larger than this (bytes). Generated bundles, minified JS, and
    // vendored blobs blow the WASM heap. 1 MB covers essentially all hand-written source.
    1024 * 1024
}

fn default_ignore_dirs() -> Vec<String> {
    // upstream extraction/index.ts:117-145
    // Directory names that are dependency, build, cache, or tooling output across the
    // languages/frameworks CodeGraph supports. Excluded by default so the graph reflects
    // your code, not third-party noise, without requiring a .gitignore.
    vec![
        // JS / TS — dependency directories
        "node_modules".to_string(),
        "bower_components".to_string(),
        "jspm_packages".to_string(),
        "web_modules".to_string(),
        ".yarn".to_string(),
        ".pnpm-store".to_string(),
        // JS / TS — framework & bundler build / cache / deploy output
        ".next".to_string(),
        ".nuxt".to_string(),
        ".svelte-kit".to_string(),
        ".turbo".to_string(),
        ".vite".to_string(),
        ".parcel-cache".to_string(),
        ".angular".to_string(),
        ".docusaurus".to_string(),
        "storybook-static".to_string(),
        ".vinxi".to_string(),
        ".nitro".to_string(),
        "out-tsc".to_string(),
        ".vercel".to_string(),
        ".netlify".to_string(),
        ".wrangler".to_string(),
        // Build output (common across ecosystems)
        "dist".to_string(),
        "build".to_string(),
        "out".to_string(),
        ".output".to_string(),
        // Test / coverage
        "coverage".to_string(),
        ".nyc_output".to_string(),
        // Python
        "__pycache__".to_string(),
        "__pypackages__".to_string(),
        ".venv".to_string(),
        "venv".to_string(),
        ".pixi".to_string(),
        ".pdm-build".to_string(),
        ".mypy_cache".to_string(),
        ".pytest_cache".to_string(),
        ".ruff_cache".to_string(),
        ".tox".to_string(),
        ".nox".to_string(),
        ".hypothesis".to_string(),
        ".ipynb_checkpoints".to_string(),
        ".eggs".to_string(),
        // Rust / JVM (Maven, Gradle, Scala)
        "target".to_string(),
        ".gradle".to_string(),
        // .NET
        "obj".to_string(),
        // Vendored deps (Go, PHP/Composer, Ruby/Bundler)
        "vendor".to_string(),
        // Swift / iOS
        ".build".to_string(),
        "Pods".to_string(),
        "Carthage".to_string(),
        "DerivedData".to_string(),
        ".swiftpm".to_string(),
        // Dart / Flutter
        ".dart_tool".to_string(),
        ".pub-cache".to_string(),
        // Godot — .godot is the regenerated engine import/cache dir (never source);
        // addons holds vendored third-party editor plugins / GDScript. Both are
        // re-includable via a .gitignore negation or a custom indexing.ignore_dirs.
        ".godot".to_string(),
        "addons".to_string(),
    ]
}

/// Root-relative `.gitignore`-style path patterns excluded by default.
///
/// #1047: Android `res/` resource subdirs hold no code symbols but often make up
/// the bulk of an Android project's files, bloating the index. Each standard
/// subdir is excluded via a `res/<kind>*` prefix pattern so the SAME rule also
/// swallows locale/density variants (`res/values-es/`, `res/drawable-hdpi/`).
///
/// Deliberately NOT excluded: `res/raw/` (real assets) and MyBatis mapper XML
/// under `src/main/resources/` — the per-subdir `res/<kind>` prefixes never
/// match either. Re-include any of these with a `.gitignore` negation
/// (`!res/values/`).
fn default_ignore_paths() -> Vec<String> {
    [
        "res/layout",
        "res/values",
        "res/drawable",
        "res/menu",
        "res/mipmap",
        "res/anim",
        "res/color",
        "res/xml",
        "res/navigation",
    ]
    .iter()
    .map(|stem| format!("{stem}*"))
    .collect()
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            max_file_size: default_max_file_size(),
            ignore_dirs: default_ignore_dirs(),
            ignore_paths: default_ignore_paths(),
            exclude: Vec::new(),
        }
    }
}

/// Watch configuration.
/// upstream sync/watch-policy.ts
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WatchConfig {
    #[serde(default = "default_watch_enabled")]
    pub enabled: bool,
    #[serde(default = "default_watch_debounce_ms")]
    pub debounce_ms: u64,
}

fn default_watch_enabled() -> bool {
    // upstream sync/watch-policy.ts
    // File watcher enabled by default; disabled via CODEGRAPH_NO_WATCH=1 or on WSL2 /mnt/* drives
    true
}

fn default_watch_debounce_ms() -> u64 {
    // upstream sync/watch-policy.ts
    // Debounce window for file-watcher events (default 2000ms, clamped to [100ms, 60s])
    2000
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            enabled: default_watch_enabled(),
            debounce_ms: default_watch_debounce_ms(),
        }
    }
}

impl Config {
    /// Read and parse a TOML file at `path`.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file: {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parsing TOML: {}", path.display()))?;
        Ok(cfg)
    }

    /// Discover the config file with a clear precedence:
    ///   1. explicit `cli_path` (passed in directly)
    ///   2. `APP_CONFIG` env var
    ///   3. `./.codegraph/config.toml` (current working directory)
    ///   4. `<project_root>/.codegraph/config.toml` (if provided)
    ///
    /// If no file is found, returns all defaults.
    pub fn discover(cli_path: Option<&Path>, project_root: &Path) -> Result<Self> {
        if let Some(p) = cli_path {
            return Self::from_path(p);
        }
        if let Ok(p) = std::env::var("APP_CONFIG") {
            return Self::from_path(p);
        }

        // Try .codegraph/config.toml relative to project root
        let project_config = project_root.join(".codegraph").join("config.toml");
        if project_config.exists() {
            return Self::from_path(&project_config);
        }

        // Try ./.codegraph/config.toml (CWD)
        let cwd_config = PathBuf::from(".codegraph/config.toml");
        if cwd_config.exists() {
            return Self::from_path(&cwd_config);
        }

        // No file found — return all defaults
        Ok(Self {
            app: AppConfig {
                name: "codegraph".to_string(),
                log_level: default_log_level(),
            },
            indexing: IndexingConfig::default(),
            watch: WatchConfig::default(),
        })
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Initialize the global config once, early in `main`. Returns the parsed config
/// so `main` can react to errors before continuing.
pub fn init_config(cli_path: Option<&Path>, project_root: &Path) -> Result<&'static Config> {
    let cfg = Config::discover(cli_path, project_root)?;
    CONFIG
        .set(cfg)
        .map_err(|_| anyhow::anyhow!("config already initialized"))?;
    Ok(CONFIG.get().expect("just set"))
}

/// Borrow the global config after `init_config` has run.
/// Panics if not initialized; for library use, prefer init_config().
pub fn get_config() -> &'static Config {
    CONFIG
        .get()
        .expect("config not initialized; call init_config() first")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_parses() {
        let toml_str = r#"
[app]
name = "test-project"
log_level = "debug"

[indexing]
max_file_size = 2097152

[watch]
enabled = true
debounce_ms = 5000
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.app.name, "test-project");
        assert_eq!(cfg.app.log_level, "debug");
        assert_eq!(cfg.indexing.max_file_size, 2097152);
        assert_eq!(cfg.watch.debounce_ms, 5000);
    }

    #[test]
    fn test_empty_toml_uses_defaults() {
        let toml_str = r#"
[app]
name = "my-project"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.app.name, "my-project");
        assert_eq!(cfg.app.log_level, "info"); // default
        assert_eq!(cfg.indexing.max_file_size, 1048576); // default
        assert!(cfg.indexing.ignore_dirs.len() >= 40); // should have many defaults
        assert!(cfg.watch.enabled); // default
        assert_eq!(cfg.watch.debounce_ms, 2000); // default
        assert!(cfg.indexing.exclude.is_empty()); // off by default
    }

    #[test]
    fn test_exclude_parses_and_defaults_empty() {
        let with_exclude = r#"
[app]
name = "p"

[indexing]
exclude = ["static/", "docs/gen"]
"#;
        let cfg: Config = toml::from_str(with_exclude).expect("should parse");
        assert_eq!(cfg.indexing.exclude, vec!["static/", "docs/gen"]);

        let without = r#"
[app]
name = "p"

[indexing]
max_file_size = 2097152
"#;
        let cfg: Config = toml::from_str(without).expect("should parse");
        assert!(cfg.indexing.exclude.is_empty());
    }

    #[test]
    fn test_missing_file_returns_defaults() {
        let cfg = Config::discover(None, Path::new("/tmp/nonexistent"))
            .expect("should not error on missing file");
        assert_eq!(cfg.app.log_level, "info");
        assert_eq!(cfg.indexing.max_file_size, 1048576);
        assert!(cfg.watch.enabled);
        assert_eq!(cfg.watch.debounce_ms, 2000);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-config-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn from_path_reads_and_parses_a_toml_file() {
        let dir = temp_dir("from-path");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[app]\nname = \"disk-project\"\nlog_level = \"warn\"\n",
        )
        .unwrap();

        let cfg = Config::from_path(&path).expect("from_path parses");
        assert_eq!(cfg.app.name, "disk-project");
        assert_eq!(cfg.app.log_level, "warn");
        assert_eq!(cfg.indexing.max_file_size, default_max_file_size());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_path_errors_on_missing_and_malformed_files() {
        let missing = Config::from_path("/tmp/cg-config-does-not-exist.toml");
        assert!(missing.is_err());

        let dir = temp_dir("malformed");
        let path = dir.join("config.toml");
        std::fs::write(&path, "this is not = valid toml [[[").unwrap();
        assert!(Config::from_path(&path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_prefers_explicit_cli_path() {
        let dir = temp_dir("cli-path");
        let path = dir.join("explicit.toml");
        std::fs::write(&path, "[app]\nname = \"explicit\"\nlog_level = \"error\"\n").unwrap();

        let cfg = Config::discover(Some(&path), Path::new("/tmp/ignored")).expect("discover");
        assert_eq!(cfg.app.name, "explicit");
        assert_eq!(cfg.app.log_level, "error");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_reads_project_root_codegraph_config() {
        let project = temp_dir("project-root");
        let cfg_dir = project.join(".codegraph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[app]\nname = \"rooted\"\nlog_level = \"debug\"\n",
        )
        .unwrap();

        let cfg = Config::discover(None, &project).expect("discover project config");
        assert_eq!(cfg.app.name, "rooted");
        assert_eq!(cfg.app.log_level, "debug");

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn indexing_config_default_carries_ignore_dirs_and_empty_exclude() {
        let indexing = IndexingConfig::default();
        assert_eq!(indexing.max_file_size, default_max_file_size());
        assert!(indexing.ignore_dirs.contains(&"node_modules".to_string()));
        assert!(indexing.ignore_dirs.contains(&"target".to_string()));
        assert!(indexing.exclude.is_empty());

        let watch = WatchConfig::default();
        assert!(watch.enabled);
        assert_eq!(watch.debounce_ms, default_watch_debounce_ms());
    }

    #[test]
    fn default_ignore_paths_cover_android_res_dirs() {
        // #1047: Android res/ resource subdirs hold no code symbols and bloat the
        // index. Each standard subdir is excluded by default via a prefix pattern
        // whose form also swallows locale/density variants (res/values-es, ...).
        let paths = default_ignore_paths();
        for stem in [
            "res/layout",
            "res/values",
            "res/drawable",
            "res/menu",
            "res/mipmap",
            "res/anim",
            "res/color",
            "res/xml",
            "res/navigation",
        ] {
            assert!(
                paths.iter().any(|p| p == &format!("{stem}*")),
                "expected a `{stem}*` default ignore pattern, got: {paths:?}"
            );
        }
    }

    #[test]
    fn default_ignore_paths_preserve_res_raw_and_resources() {
        // #1047 exclusions to PRESERVE: res/raw/ holds real assets, and MyBatis
        // mapper XML lives under src/main/resources/ (NOT res/). No default
        // pattern may match either.
        let paths = default_ignore_paths();
        assert!(
            !paths.iter().any(|p| p.contains("res/raw")),
            "res/raw must never be excluded: {paths:?}"
        );
        assert!(
            !paths
                .iter()
                .any(|p| p.starts_with("res*") || p == "res/" || p == "res"),
            "a bare res/ rule would wrongly catch resources/: {paths:?}"
        );
    }

    #[test]
    fn indexing_config_default_carries_android_res_ignore_paths() {
        let indexing = IndexingConfig::default();
        assert!(indexing.ignore_paths.contains(&"res/values*".to_string()));
        assert!(indexing.ignore_paths.contains(&"res/drawable*".to_string()));
    }

    #[test]
    fn ignore_paths_parses_and_overrides_default() {
        let with_override = r#"
[app]
name = "p"

[indexing]
ignore_paths = ["custom/gen*"]
"#;
        let cfg: Config = toml::from_str(with_override).expect("should parse");
        assert_eq!(cfg.indexing.ignore_paths, vec!["custom/gen*"]);
    }
    #[test]
    fn init_and_get_config_share_the_global_singleton() {
        let dir = std::env::temp_dir().join(format!(
            "codegraph-config-init-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cg = dir.join(".codegraph");
        std::fs::create_dir_all(&cg).unwrap();
        std::fs::write(
            cg.join("config.toml"),
            "[app]\nname = \"global-singleton\"\n",
        )
        .unwrap();
        let first = init_config(None, &dir).expect("first init succeeds");
        assert_eq!(first.app.name, "global-singleton");
        assert_eq!(get_config().app.name, "global-singleton");
        assert!(
            init_config(None, &dir).is_err(),
            "a second init must fail (already initialized)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
