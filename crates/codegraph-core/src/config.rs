//! Configuration module for CodeGraph.
//!
//! Project-scoped callers load immutable [`Config`] values from the current
//! [`IndexPaths`] root. Config is optional — a missing current-root file uses all
//! defaults, matching the upstream zero-config UX.
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
//! There is NO process-global config. Every project-scoped operation loads its own
//! immutable [`Config`] with [`Config::load_for_paths`] and passes the returned
//! [`Arc`] down; process bootstrap (which has no addressed project yet) uses
//! [`Config::load_env_or_default`], whose result may only configure the logger.

use crate::IndexPaths;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// Root-relative path patterns for first-party source to force INTO the
    /// index even when `.gitignore` (or the default `ignore_paths`) would drop
    /// it — the case being a project under a second VCS (SVN, Perforce) that
    /// `.gitignore`s its own real source out of Git yet still wants it indexed.
    /// Same matcher as `.gitignore`/`exclude` (`Tools/`, `Local/ts/`, `gen*`),
    /// matched against root-relative paths. Complements `exclude` (its
    /// opposite). Precedence: a built-in `ignore_dirs` skip
    /// (`node_modules`/`dist`/…) is NEVER re-included, and an explicit
    /// `exclude` still wins over `include`. Off by default (empty = today's
    /// behavior byte-identical). #1063.
    #[serde(default)]
    pub include: Vec<String>,
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
        // addons holds vendored third-party editor plugins / GDScript. Re-include
        // either one by dropping it from a custom indexing.ignore_dirs; a
        // .gitignore negation cannot, because scan_dir prunes every ignore_dirs
        // entry before it evaluates any pattern set.
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
            include: Vec::new(),
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

impl Default for Config {
    fn default() -> Self {
        Self {
            app: AppConfig {
                name: "codegraph".to_string(),
                log_level: default_log_level(),
            },
            indexing: IndexingConfig::default(),
            watch: WatchConfig::default(),
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

    /// Load an immutable config for one resolved project's current index root.
    ///
    /// Precedence is deliberately narrow and project-authoritative:
    ///
    /// 1. explicit `cli_path`;
    /// 2. the process-wide `APP_CONFIG` override;
    /// 3. [`IndexPaths::config_toml`] for this project.
    ///
    /// A missing current-root config returns defaults. An explicitly selected
    /// CLI or environment path must exist and parse successfully. This API never
    /// consults a legacy `.codegraph/config.toml`, the process working directory,
    /// or another project's paths, and it does not cache across calls.
    pub fn load_for_paths(cli_path: Option<&Path>, paths: &IndexPaths) -> Result<Arc<Self>> {
        if let Some(path) = cli_path {
            return Self::from_path(path).map(Arc::new);
        }
        if let Some(path) = std::env::var_os("APP_CONFIG") {
            return Self::from_path(PathBuf::from(path)).map(Arc::new);
        }

        let project_config = paths.config_toml();
        let config = if project_config
            .try_exists()
            .with_context(|| format!("checking config file: {}", project_config.display()))?
        {
            Self::from_path(project_config)?
        } else {
            Self::default()
        };
        Ok(Arc::new(config))
    }

    /// Load the PROCESS-BOOTSTRAP config, before any project is addressed.
    ///
    /// Precedence is exactly the project-independent prefix of
    /// [`Config::load_for_paths`]: an explicit `cli_path`, then the process-wide
    /// `APP_CONFIG` override, then all defaults. It never reads a project root,
    /// a legacy `.codegraph/config.toml`, or the process working directory.
    ///
    /// The result may ONLY configure process-wide bootstrap concerns (the logger
    /// level). It is never the configuration source for a project operation — a
    /// global HTTP server, sync, watcher, or extraction pass loads the addressed
    /// project's own config through [`Config::load_for_paths`].
    pub fn load_env_or_default(cli_path: Option<&Path>) -> Result<Arc<Self>> {
        if let Some(path) = cli_path {
            return Self::from_path(path).map(Arc::new);
        }
        if let Some(path) = std::env::var_os("APP_CONFIG") {
            return Self::from_path(PathBuf::from(path)).map(Arc::new);
        }
        Ok(Arc::new(Self::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static APP_CONFIG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Panic-safe process-environment mutation for APP_CONFIG precedence tests.
    /// The serialization lock is retained until after the previous value is
    /// restored, including when an assertion unwinds through this guard.
    struct AppConfigEnvGuard {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl AppConfigEnvGuard {
        fn set(value: Option<&Path>) -> Self {
            let lock = APP_CONFIG_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let previous = std::env::var_os("APP_CONFIG");
            // SAFETY: every APP_CONFIG read/mutation in this test module that can
            // overlap these tests is serialized by APP_CONFIG_ENV_LOCK. Drop
            // restores the prior value while the same lock is still held.
            match value {
                Some(path) => unsafe { std::env::set_var("APP_CONFIG", path) },
                None => unsafe { std::env::remove_var("APP_CONFIG") },
            }
            Self {
                previous,
                _lock: lock,
            }
        }

        fn unset() -> Self {
            Self::set(None)
        }
    }

    impl Drop for AppConfigEnvGuard {
        fn drop(&mut self) {
            // SAFETY: the serialization guard is still held while restoring the
            // process environment on both normal return and panic unwind.
            match self.previous.take() {
                Some(value) => unsafe { std::env::set_var("APP_CONFIG", value) },
                None => unsafe { std::env::remove_var("APP_CONFIG") },
            }
        }
    }

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
    fn test_include_parses_and_defaults_empty() {
        let with_include = r#"
[app]
name = "p"

[indexing]
include = ["Tools/", "Local/ts/"]
"#;
        let cfg: Config = toml::from_str(with_include).expect("should parse");
        assert_eq!(cfg.indexing.include, vec!["Tools/", "Local/ts/"]);

        let without = r#"
[app]
name = "p"

[indexing]
max_file_size = 2097152
"#;
        let cfg: Config = toml::from_str(without).expect("should parse");
        assert!(cfg.indexing.include.is_empty());

        assert!(IndexingConfig::default().include.is_empty());
    }

    #[test]
    fn test_missing_file_returns_defaults() {
        let _env = AppConfigEnvGuard::unset();
        let cfg = Config::load_env_or_default(None).expect("should not error on missing file");
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
    fn load_env_or_default_prefers_explicit_cli_path() {
        let dir = temp_dir("cli-path");
        let path = dir.join("explicit.toml");
        std::fs::write(&path, "[app]\nname = \"explicit\"\nlog_level = \"error\"\n").unwrap();

        let cfg = Config::load_env_or_default(Some(&path)).expect("bootstrap config");
        assert_eq!(cfg.app.name, "explicit");
        assert_eq!(cfg.app.log_level, "error");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_env_or_default_honors_app_config_then_falls_back_to_defaults() {
        let dir = temp_dir("bootstrap-env");
        let path = dir.join("env.toml");
        std::fs::write(&path, "[app]\nname = \"env\"\nlog_level = \"trace\"\n").unwrap();
        {
            let _env = AppConfigEnvGuard::set(Some(&path));
            let cfg = Config::load_env_or_default(None).expect("bootstrap config");
            assert_eq!(cfg.app.name, "env");
            assert_eq!(cfg.app.log_level, "trace");
        }
        {
            let _env = AppConfigEnvGuard::unset();
            let cfg = Config::load_env_or_default(None).expect("bootstrap config");
            assert_eq!(cfg.app.log_level, default_log_level());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bootstrap must never adopt a project or CWD legacy `.codegraph/config.toml`:
    /// with `APP_CONFIG` unset it is defaults-only, so it can never become the
    /// configuration source for a later per-project operation.
    #[test]
    fn load_env_or_default_ignores_legacy_project_and_cwd_configs() {
        const CHILD_MARKER: &str = "CODEGRAPH_CONFIG_BOOTSTRAP_CHILD";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let cfg = Config::load_env_or_default(None).unwrap();
            assert_eq!(cfg.app.name, "codegraph");
            assert_eq!(cfg.app.log_level, default_log_level());
            return;
        }

        let outer = temp_dir("bootstrap-no-legacy");
        std::fs::create_dir_all(outer.join(".codegraph")).unwrap();
        std::fs::write(
            outer.join(".codegraph/config.toml"),
            "[app]\nname = \"legacy-cwd\"\nlog_level = \"error\"\n",
        )
        .unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("config::tests::load_env_or_default_ignores_legacy_project_and_cwd_configs")
            .arg("--nocapture")
            .current_dir(&outer)
            .env(CHILD_MARKER, "1")
            .env_remove("APP_CONFIG")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = std::fs::remove_dir_all(outer);
    }

    fn project_paths(label: &str) -> (PathBuf, IndexPaths) {
        let project = temp_dir(label);
        let paths = IndexPaths::resolve(&project, None).expect("resolve project paths");
        (project, paths)
    }

    fn write_current_config(paths: &IndexPaths, contents: &str) {
        let path = paths.config_toml();
        std::fs::create_dir_all(path.parent().expect("config has parent")).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn load_for_paths_prefers_explicit_cli_path_over_app_config_and_project() {
        let (project, paths) = project_paths("scoped-explicit");
        write_current_config(&paths, "[app]\nname = \"project\"\n");

        let override_path = project.join("override.toml");
        std::fs::write(&override_path, "[app]\nname = \"override\"\n").unwrap();
        let explicit_path = project.join("explicit.toml");
        std::fs::write(&explicit_path, "[app]\nname = \"explicit\"\n").unwrap();
        let _env = AppConfigEnvGuard::set(Some(&override_path));

        let config = Config::load_for_paths(Some(&explicit_path), &paths).unwrap();
        assert_eq!(config.app.name, "explicit");

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn load_for_paths_reads_only_the_resolved_current_root() {
        let _env = AppConfigEnvGuard::unset();
        let (project, paths) = project_paths("scoped-current-root");
        write_current_config(
            &paths,
            "[app]\nname = \"current-root\"\n[indexing]\nmax_file_size = 321\n",
        );

        let config = Config::load_for_paths(None, &paths).unwrap();
        assert_eq!(config.app.name, "current-root");
        assert_eq!(config.indexing.max_file_size, 321);

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn load_for_paths_missing_current_config_returns_defaults() {
        let _env = AppConfigEnvGuard::unset();
        let (project, paths) = project_paths("scoped-missing");

        let config = Config::load_for_paths(None, &paths).unwrap();
        assert_eq!(config.app.name, "codegraph");
        assert_eq!(config.app.log_level, "info");
        assert_eq!(config.indexing.max_file_size, default_max_file_size());
        assert!(config.indexing.include.is_empty());
        assert!(config.indexing.exclude.is_empty());

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn load_for_paths_reports_malformed_current_config() {
        let _env = AppConfigEnvGuard::unset();
        let (project, paths) = project_paths("scoped-malformed");
        write_current_config(&paths, "this is not = valid toml [[[");

        let error = Config::load_for_paths(None, &paths).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("parsing TOML"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains(&paths.config_toml().display().to_string()),
            "error must name the project config: {message}"
        );

        let _ = std::fs::remove_dir_all(project);
    }

    #[test]
    fn load_for_paths_keeps_two_projects_isolated_without_app_config() {
        let _env = AppConfigEnvGuard::unset();
        let (alpha_project, alpha_paths) = project_paths("scoped-alpha");
        let (beta_project, beta_paths) = project_paths("scoped-beta");
        write_current_config(
            &alpha_paths,
            "[app]\nname = \"alpha\"\n[indexing]\nmax_file_size = 111\ninclude = [\"alpha/**\"]\nexclude = [\"beta/**\"]\n",
        );
        write_current_config(
            &beta_paths,
            "[app]\nname = \"beta\"\n[indexing]\nmax_file_size = 222\ninclude = [\"beta/**\"]\nexclude = [\"alpha/**\"]\n",
        );

        let alpha = Config::load_for_paths(None, &alpha_paths).unwrap();
        let beta = Config::load_for_paths(None, &beta_paths).unwrap();
        assert!(!Arc::ptr_eq(&alpha, &beta));
        assert_eq!(alpha.indexing.max_file_size, 111);
        assert_eq!(alpha.indexing.include, ["alpha/**"]);
        assert_eq!(alpha.indexing.exclude, ["beta/**"]);
        assert_eq!(beta.indexing.max_file_size, 222);
        assert_eq!(beta.indexing.include, ["beta/**"]);
        assert_eq!(beta.indexing.exclude, ["alpha/**"]);

        let _ = std::fs::remove_dir_all(alpha_project);
        let _ = std::fs::remove_dir_all(beta_project);
    }

    #[test]
    fn load_for_paths_app_config_intentionally_overrides_two_projects() {
        let (alpha_project, alpha_paths) = project_paths("override-alpha");
        let (beta_project, beta_paths) = project_paths("override-beta");
        write_current_config(&alpha_paths, "[app]\nname = \"alpha\"\n");
        write_current_config(&beta_paths, "[app]\nname = \"beta\"\n");
        let override_path = alpha_project.join("global-override.toml");
        std::fs::write(
            &override_path,
            "[app]\nname = \"global\"\n[indexing]\nmax_file_size = 777\ninclude = [\"shared/**\"]\nexclude = [\"private/**\"]\n",
        )
        .unwrap();
        let _env = AppConfigEnvGuard::set(Some(&override_path));

        let alpha = Config::load_for_paths(None, &alpha_paths).unwrap();
        let beta = Config::load_for_paths(None, &beta_paths).unwrap();
        for config in [&alpha, &beta] {
            assert_eq!(config.app.name, "global");
            assert_eq!(config.indexing.max_file_size, 777);
            assert_eq!(config.indexing.include, ["shared/**"]);
            assert_eq!(config.indexing.exclude, ["private/**"]);
        }

        let _ = std::fs::remove_dir_all(alpha_project);
        let _ = std::fs::remove_dir_all(beta_project);
    }

    #[test]
    fn load_for_paths_ignores_legacy_project_and_cwd_configs() {
        const CHILD_PROJECT: &str = "CODEGRAPH_CONFIG_CWD_CHILD_PROJECT";

        if let Some(project) = std::env::var_os(CHILD_PROJECT) {
            let paths = IndexPaths::resolve(Path::new(&project), None).unwrap();
            let config = Config::load_for_paths(None, &paths).unwrap();
            assert_eq!(config.app.name, "codegraph");
            return;
        }

        let outer = temp_dir("scoped-no-legacy");
        let project = outer.join("project");
        std::fs::create_dir_all(project.join(".codegraph")).unwrap();
        std::fs::write(
            project.join(".codegraph/config.toml"),
            "[app]\nname = \"legacy-project\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(outer.join(".codegraph")).unwrap();
        std::fs::write(
            outer.join(".codegraph/config.toml"),
            "[app]\nname = \"legacy-cwd\"\n",
        )
        .unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("config::tests::load_for_paths_ignores_legacy_project_and_cwd_configs")
            .arg("--nocapture")
            .current_dir(&outer)
            .env(CHILD_PROJECT, &project)
            .env_remove("APP_CONFIG")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let _ = std::fs::remove_dir_all(outer);
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
    /// Two loads of the SAME project return independent immutable values, so no
    /// caller can observe (or mutate) a shared process-wide config instance.
    #[test]
    fn repeated_loads_are_independent_immutable_values() {
        let _env = AppConfigEnvGuard::unset();
        let (project, paths) = project_paths("no-singleton");
        write_current_config(&paths, "[app]\nname = \"scoped\"\n");

        let first = Config::load_for_paths(None, &paths).unwrap();
        let second = Config::load_for_paths(None, &paths).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "load_for_paths must not hand back a cached/shared instance"
        );
        assert_eq!(first.app.name, "scoped");
        assert_eq!(second.app.name, "scoped");

        let _ = std::fs::remove_dir_all(project);
    }
}
