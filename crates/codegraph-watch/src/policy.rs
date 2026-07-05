use std::fs;
use std::path::{Path, PathBuf};

use codegraph_extract::detect_language;

pub const CODEGRAPH_NO_WATCH: &str = "CODEGRAPH_NO_WATCH";

const DEFAULT_IGNORE_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "jspm_packages",
    "web_modules",
    ".yarn",
    ".pnpm-store",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".vite",
    ".parcel-cache",
    ".angular",
    ".docusaurus",
    "storybook-static",
    ".vinxi",
    ".nitro",
    "out-tsc",
    ".vercel",
    ".netlify",
    ".wrangler",
    "dist",
    "build",
    "out",
    ".output",
    "coverage",
    ".nyc_output",
    "__pycache__",
    "__pypackages__",
    ".venv",
    "venv",
    ".pixi",
    ".pdm-build",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".hypothesis",
    ".ipynb_checkpoints",
    ".eggs",
    "target",
    ".gradle",
    "obj",
    "vendor",
    ".build",
    "Pods",
    "Carthage",
    "DerivedData",
    ".swiftpm",
    ".dart_tool",
    ".pub-cache",
    ".cxx",
    ".externalNativeBuild",
    "vcpkg_installed",
    ".bloop",
    ".metals",
    "lua_modules",
    ".luarocks",
    "__history",
    "__recovery",
    ".cache",
];

#[derive(Debug, Clone)]
struct IgnoreRule {
    pattern: String,
    negated: bool,
}

#[derive(Debug, Clone)]
pub struct WatchPolicy {
    root: PathBuf,
    rules: Vec<IgnoreRule>,
}

impl WatchPolicy {
    pub fn new(root: impl AsRef<Path>) -> Self {
        // Mirrors the upstream built-in ignore seed and root .gitignore merge from
        // `upstream extraction/index.ts:117-161,242-246` so
        // `.gitignore` negations can opt default-excluded dirs back in.
        let root = root.as_ref().to_path_buf();
        let mut rules = DEFAULT_IGNORE_DIRS
            .iter()
            .map(|dir| IgnoreRule {
                pattern: format!("{dir}/"),
                negated: false,
            })
            .collect::<Vec<_>>();
        rules.extend([
            IgnoreRule {
                pattern: "*.egg-info/".to_string(),
                negated: false,
            },
            IgnoreRule {
                pattern: "cmake-build-*/".to_string(),
                negated: false,
            },
            IgnoreRule {
                pattern: "bazel-*/".to_string(),
                negated: false,
            },
        ]);
        rules.extend(read_gitignore_rules(&root));
        Self { root, rules }
    }

    pub fn normalize_relative(&self, path: impl AsRef<Path>) -> Option<String> {
        let path = path.as_ref();
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.root).ok()?
        } else {
            path
        };
        let normalized = normalize_path(relative);
        if normalized.is_empty() || normalized == "." || normalized.starts_with("../") {
            return None;
        }
        Some(normalized)
    }

    pub fn should_handle_file(&self, relative: &str) -> bool {
        !self.is_always_ignored(relative)
            && !self.is_ignored(relative, false)
            && detect_language(relative) != codegraph_core::types::Language::Unknown
    }

    pub fn allows_file_path(&self, relative: &str) -> bool {
        !self.is_always_ignored(relative) && !self.is_ignored(relative, false)
    }

    pub fn should_watch_dir(&self, relative: &str) -> bool {
        !self.is_always_ignored(relative) && !self.is_ignored(relative, true)
    }

    fn is_always_ignored(&self, relative: &str) -> bool {
        // Same always-ignore rule as the upstream watcher for .git and every
        // CodeGraph data dir variant (`watcher.ts:427-436`).
        let top = relative.split('/').next().unwrap_or(relative);
        top == ".git" || top == ".codegraph" || top.starts_with(".codegraph-")
    }

    fn is_ignored(&self, relative: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule_matches(&rule.pattern, relative, is_dir) {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

pub fn watch_disabled_reason(project_root: impl AsRef<Path>, no_watch: bool) -> Option<String> {
    // Port of `upstream sync/watch-policy.ts:77-95`: explicit opt-out
    // wins, force-watch overrides auto detection, WSL /mnt drives are disabled.
    if no_watch || std::env::var(CODEGRAPH_NO_WATCH).as_deref() == Ok("1") {
        return Some("CODEGRAPH_NO_WATCH=1 is set".to_string());
    }
    // The home/too-broad-root guard sits BEFORE the FORCE_WATCH escape on
    // purpose. A single global MCP config (e.g. Kiro) launches `serve --mcp`
    // with no --path and the client's FIRST workspace root as CWD, which often
    // resolves to HOME; a recursive watch there walks every nested project's
    // node_modules/.venv and exhausts inotify. That is catastrophic regardless
    // of intent, so FORCE_WATCH (a WSL `/mnt/` escape) must NOT re-enable it.
    // Tool queries still serve off any existing index; only the watcher stops.
    if let Some(reason) = home_or_too_broad_root_reason(project_root.as_ref()) {
        return Some(reason);
    }
    if std::env::var("CODEGRAPH_FORCE_WATCH").as_deref() == Ok("1") {
        return None;
    }
    if detect_wsl() && is_windows_drive_mount(project_root.as_ref()) {
        return Some(
            "project is on a WSL2 /mnt/ drive, where recursive fs.watch is too slow to be reliable"
                .to_string(),
        );
    }
    None
}

fn home_or_too_broad_root_reason(project_root: &Path) -> Option<String> {
    // The watcher keeps its original "refusing to watch …" wording, so its
    // tests and user-facing messages are unchanged. The home/filesystem-root
    // DECISION, however, is shared with the daemon/catch-up guard via the
    // public `too_broad_root_reason` below — both must agree on what counts as
    // "too broad to run background services in".
    classify_too_broad_root(project_root).map(|kind| match kind {
        TooBroadRoot::FilesystemRoot(resolved) => format!(
            "refusing to watch the filesystem root ({}); launch with --path <project> or open the workspace as the working directory",
            resolved.display()
        ),
        TooBroadRoot::HomeDirectory(resolved) => format!(
            "refusing to watch the home directory ({}); launch with --path <project> or open the workspace as the working directory",
            resolved.display()
        ),
    })
}

/// Classifies a resolved project root that is too broad to run background
/// services (watcher, daemon, catch-up sync) against.
///
/// An EXACT `$HOME` or filesystem-root match is too broad; a project nested
/// under `$HOME` (e.g. `~/workspace/proj`) is NOT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TooBroadRoot {
    /// The resolved root is the filesystem root (e.g. `/` or `C:\`).
    FilesystemRoot(PathBuf),
    /// The resolved root is exactly the user's home directory.
    HomeDirectory(PathBuf),
}

/// Returns `Some(reason)` when `project_root` resolves to a root too broad to
/// run daemon services (watcher, detached daemon, catch-up sync) against —
/// namely an exact `$HOME` or filesystem-root match. Returns `None` for any
/// real project root, including projects nested under `$HOME`.
///
/// Paths are canonicalized leniently first, so `~/.` resolves to `$HOME` and a
/// user-supplied `/config/.` compares equal to `$HOME`.
///
/// This is the single source of truth shared by the watcher guard
/// (`watch_disabled_reason`) and the daemon/catch-up guard in the CLI; the
/// message is phrased generically because it now governs more than the watcher.
pub fn too_broad_root_reason(project_root: &Path) -> Option<String> {
    classify_too_broad_root(project_root).map(|kind| match kind {
        TooBroadRoot::FilesystemRoot(resolved) => format!(
            "launched at the filesystem root ({}); daemon, watcher, and catch-up are disabled — launch with --path <project> or open a project folder",
            resolved.display()
        ),
        TooBroadRoot::HomeDirectory(resolved) => format!(
            "launched at the home directory ({}); daemon, watcher, and catch-up are disabled — launch with --path <project> or open a project folder",
            resolved.display()
        ),
    })
}

fn classify_too_broad_root(project_root: &Path) -> Option<TooBroadRoot> {
    let resolved = canonicalize_lenient(project_root);

    if is_filesystem_root(&resolved) {
        return Some(TooBroadRoot::FilesystemRoot(resolved));
    }

    if let Some(home) = home_dir()
        && resolved == canonicalize_lenient(&home)
    {
        return Some(TooBroadRoot::HomeDirectory(resolved));
    }

    None
}

fn canonicalize_lenient(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| path.components().collect::<PathBuf>())
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn is_filesystem_root(path: &Path) -> bool {
    use std::path::Component;
    let mut components = path.components();
    match components.next() {
        Some(Component::RootDir) => components.next().is_none(),
        Some(Component::Prefix(_)) => {
            matches!(components.next(), None | Some(Component::RootDir))
                && components.next().is_none()
        }
        _ => false,
    }
}

pub fn normalize_path(path: impl AsRef<Path>) -> String {
    path.as_ref()
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_gitignore_rules(root: &Path) -> Vec<IgnoreRule> {
    fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (negated, pattern) = trimmed
                .strip_prefix('!')
                .map_or((false, trimmed), |pattern| (true, pattern));
            Some(IgnoreRule {
                pattern: pattern.trim_start_matches('/').to_string(),
                negated,
            })
        })
        .collect()
}

fn rule_matches(pattern: &str, relative: &str, is_dir: bool) -> bool {
    let candidate = if is_dir {
        format!("{}/", relative.trim_end_matches('/'))
    } else {
        relative.to_string()
    };
    let pattern = pattern.trim_start_matches('/');
    if let Some(dir) = pattern.strip_suffix('/') {
        // gitignore semantics: a `dir/` rule matches that directory at ANY
        // segment depth, not just the path root. Match on segment boundaries
        // (`== "{dir}/"` whole, `"{dir}/"` prefix, `"/{dir}/"` interior) so a
        // nested `.../node_modules/...` is pruned while a partial-segment name
        // like `mynode_modules/` never matches the `node_modules/` rule.
        let dir_slash = format!("{dir}/");
        let nested = format!("/{dir}/");
        return candidate == dir_slash
            || candidate.starts_with(&dir_slash)
            || candidate.contains(&nested);
    }
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        let tail = relative.rsplit('/').next().unwrap_or(relative);
        return tail.starts_with(prefix) && tail.ends_with(suffix);
    }
    relative == pattern || relative.ends_with(&format!("/{pattern}"))
}

fn detect_wsl() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    fs::read_to_string("/proc/version")
        .map(|version| {
            let version = version.to_ascii_lowercase();
            version.contains("microsoft") || version.contains("wsl")
        })
        .unwrap_or(false)
}

fn is_windows_drive_mount(path: &Path) -> bool {
    let normalized = normalize_path(path);
    let mut parts = normalized.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(""), Some("mnt"), Some(drive)) if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        home: Option<std::ffi::OsString>,
        force: Option<std::ffi::OsString>,
        no_watch: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn capture() -> Self {
            Self {
                home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }),
                force: std::env::var_os("CODEGRAPH_FORCE_WATCH"),
                no_watch: std::env::var_os(CODEGRAPH_NO_WATCH),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
            restore(home_key, self.home.take());
            restore("CODEGRAPH_FORCE_WATCH", self.force.take());
            restore(CODEGRAPH_NO_WATCH, self.no_watch.take());
        }
    }

    fn restore(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn watch_disabled_when_root_is_home() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let home = crate::sync::tests::TestDir::new("watch-policy-home");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, home.path()) };
        unsafe { std::env::remove_var("CODEGRAPH_FORCE_WATCH") };
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };

        let reason = watch_disabled_reason(home.path(), false);
        assert!(reason.is_some(), "watching HOME must be disabled");
        assert!(reason.unwrap().contains("home directory"));
    }

    #[test]
    fn watch_disabled_for_home_even_with_force_watch() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let home = crate::sync::tests::TestDir::new("watch-policy-home-force");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, home.path()) };
        unsafe { std::env::set_var("CODEGRAPH_FORCE_WATCH", "1") };
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };

        assert!(
            watch_disabled_reason(home.path(), false).is_some(),
            "CODEGRAPH_FORCE_WATCH must NOT re-enable a home walk"
        );
    }

    #[test]
    fn watch_disabled_when_root_is_filesystem_root() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        unsafe { std::env::remove_var("CODEGRAPH_FORCE_WATCH") };
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };

        let reason = watch_disabled_reason(Path::new("/"), false);
        assert!(reason.is_some(), "watching `/` must be disabled");
        assert!(reason.unwrap().contains("filesystem root"));
    }

    #[test]
    fn watch_allowed_for_normal_project_subdir() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let home = crate::sync::tests::TestDir::new("watch-policy-subdir-home");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, home.path()) };
        unsafe { std::env::remove_var("CODEGRAPH_FORCE_WATCH") };
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };

        let project = home.path().join("workspace/proj");
        fs::create_dir_all(&project).unwrap();
        assert!(
            watch_disabled_reason(&project, false).is_none(),
            "a normal project subdir must be watchable"
        );
    }

    #[test]
    fn gitignore_negation_reincludes_default_ignored_dir() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-negation");
        fs::write(dir.path().join(".gitignore"), "!vendor/\n").unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(policy.should_handle_file("vendor/first_party.ts"));
        assert!(!policy.should_handle_file("node_modules/pkg/index.ts"));
    }

    #[test]
    fn nested_ignore_dirs_are_pruned() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-nested");
        let policy = WatchPolicy::new(dir.path());
        assert!(!policy.should_watch_dir("workspace/app/node_modules"));
        assert!(!policy.should_watch_dir("a/b/.venv"));
        assert!(!policy.should_watch_dir("x/y/__pycache__"));
        assert!(!policy.should_watch_dir("examples/demo/node_modules/.pnpm/vue-demi/node_modules"));
        assert!(policy.should_watch_dir("src/components"));
    }

    #[test]
    fn partial_segment_names_are_not_false_positives() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-partial");
        let policy = WatchPolicy::new(dir.path());
        assert!(policy.should_watch_dir("a/mynode_modules"));
        assert!(policy.should_watch_dir("node_modules_old"));
        assert!(policy.should_watch_dir("a/mynode_modules/b"));
    }

    #[test]
    fn multi_segment_dir_rule_matches_on_boundaries() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-multiseg");
        fs::write(dir.path().join(".gitignore"), "a/b/\n").unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(!policy.should_watch_dir("a/b"));
        assert!(!policy.should_watch_dir("x/a/b"));
        assert!(!policy.should_watch_dir("a/b/c"));
        assert!(policy.should_watch_dir("a/bb"));
        assert!(policy.should_watch_dir("za/b"));
    }

    #[test]
    fn too_broad_reason_flags_home_and_filesystem_root_but_not_nested_project() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let home = crate::sync::tests::TestDir::new("too-broad-home");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, home.path()) };

        assert!(
            too_broad_root_reason(home.path()).is_some(),
            "$HOME must be too broad"
        );
        assert!(
            too_broad_root_reason(Path::new("/")).is_some(),
            "the filesystem root must be too broad"
        );

        let nested = home.path().join("workspace/ProdDir/AI/codegraph-rust");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            too_broad_root_reason(&nested).is_none(),
            "a project nested under $HOME must NOT be too broad"
        );
    }

    #[test]
    fn too_broad_reason_normalizes_trailing_dot_to_home() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let home = crate::sync::tests::TestDir::new("too-broad-home-dot");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, home.path()) };

        let with_dot = home.path().join(".");
        assert!(
            too_broad_root_reason(&with_dot).is_some(),
            "`$HOME/.` must normalize to `$HOME` and be too broad"
        );
    }

    #[test]
    fn watch_disabled_when_root_is_home_with_trailing_dot() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let home = crate::sync::tests::TestDir::new("watch-policy-home-dot");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, home.path()) };
        unsafe { std::env::remove_var("CODEGRAPH_FORCE_WATCH") };
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };

        let with_dot = home.path().join(".");
        assert!(
            watch_disabled_reason(&with_dot, false).is_some(),
            "`$HOME/.` must normalize to `$HOME` and be disabled"
        );
    }

    #[test]
    fn watch_disabled_when_no_watch_flag_is_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let project = crate::sync::tests::TestDir::new("watch-policy-flag");
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };

        // The explicit `no_watch` parameter wins even for a normal project dir.
        let reason = watch_disabled_reason(project.path(), true);
        assert_eq!(reason.as_deref(), Some("CODEGRAPH_NO_WATCH=1 is set"));
    }

    #[test]
    fn watch_disabled_when_no_watch_env_is_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let project = crate::sync::tests::TestDir::new("watch-policy-env");
        unsafe { std::env::set_var(CODEGRAPH_NO_WATCH, "1") };

        let reason = watch_disabled_reason(project.path(), false);
        assert_eq!(reason.as_deref(), Some("CODEGRAPH_NO_WATCH=1 is set"));
    }

    #[test]
    fn gitignore_comments_and_blank_lines_are_skipped() {
        // A .gitignore with comments and blank lines contributes only real rules.
        let dir = crate::sync::tests::TestDir::new("watch-policy-comments");
        fs::write(
            dir.path().join(".gitignore"),
            "# a comment line\n\n   \nbuildcache/\n",
        )
        .unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(
            !policy.should_watch_dir("buildcache"),
            "the single real rule from .gitignore is honored"
        );
        assert!(
            policy.should_watch_dir("src"),
            "commented/blank lines add no spurious rules"
        );
    }

    #[test]
    fn star_glob_rule_matches_on_basename_prefix_and_suffix() {
        // A slashless gitignore glob like `*.log` matches by basename
        // prefix/suffix (the `split_once('*')` branch), so a matching leaf file
        // is ignored while a partial-name sibling is not.
        let dir = crate::sync::tests::TestDir::new("watch-policy-glob");
        fs::write(dir.path().join(".gitignore"), "*.log\ntmp*\n").unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(!policy.allows_file_path("build.log"));
        assert!(!policy.allows_file_path("logs/server.log"));
        assert!(!policy.allows_file_path("tmpfile"));
        assert!(policy.allows_file_path("src/app.ts"));
    }

    #[test]
    fn exact_file_rule_matches_root_and_nested_suffix() {
        // A gitignore rule without a trailing slash matches the exact relative
        // path AND any `/name` suffix, but not a partial-segment name.
        let dir = crate::sync::tests::TestDir::new("watch-policy-exact");
        fs::write(dir.path().join(".gitignore"), "secret.env\n").unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(!policy.allows_file_path("secret.env"));
        assert!(!policy.allows_file_path("config/secret.env"));
        assert!(policy.allows_file_path("secret.env.example"));
    }

    #[test]
    fn always_ignored_covers_git_codegraph_and_variants() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-always");
        let policy = WatchPolicy::new(dir.path());
        assert!(!policy.should_watch_dir(".git"));
        assert!(!policy.should_watch_dir(".git/objects"));
        assert!(!policy.should_watch_dir(".codegraph"));
        assert!(!policy.should_watch_dir(".codegraph-daemon"));
        assert!(!policy.allows_file_path(".codegraph/codegraph.db"));
    }

    #[test]
    fn normalize_relative_rejects_root_and_escaping_paths() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-normalize");
        let policy = WatchPolicy::new(dir.path());
        // unix-absolute-path semantics: on Windows `/etc/passwd` is NOT absolute,
        // so normalize_relative treats it as relative instead of rejecting it.
        #[cfg(unix)]
        assert_eq!(policy.normalize_relative("/etc/passwd"), None);
        // The root itself normalizes to empty/".", which is rejected.
        assert_eq!(policy.normalize_relative(dir.path()), None);
        // A relative source file under the root normalizes cleanly.
        assert_eq!(
            policy.normalize_relative("src/app.ts").as_deref(),
            Some("src/app.ts")
        );
    }

    #[test]
    fn normalize_path_converts_separators_and_collapses_dots() {
        assert_eq!(normalize_path("a/./b"), "a/b");
        assert_eq!(normalize_path("a/b/c"), "a/b/c");
    }

    #[test]
    fn should_handle_file_requires_known_language() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-handle");
        let policy = WatchPolicy::new(dir.path());
        // A source extension is handled; a non-source file is allowed but not
        // handled (it has no known language).
        assert!(policy.should_handle_file("src/app.ts"));
        assert!(!policy.should_handle_file("README.md"));
        assert!(policy.allows_file_path("README.md"));
    }

    #[test]
    fn force_watch_re_enables_a_wsl_drive_mount_path() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::capture();
        let project = crate::sync::tests::TestDir::new("watch-policy-force");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        unsafe { std::env::set_var(home_key, project.path()) };
        unsafe { std::env::remove_var(CODEGRAPH_NO_WATCH) };
        unsafe { std::env::set_var("CODEGRAPH_FORCE_WATCH", "1") };

        // A normal (non-home, non-root) project with FORCE_WATCH set returns
        // None — the force escape short-circuits the WSL/mount check below it.
        let nested = project.path().join("workspace/proj");
        fs::create_dir_all(&nested).unwrap();
        assert!(
            watch_disabled_reason(&nested, false).is_none(),
            "FORCE_WATCH must re-enable a normal project directory"
        );
    }

    #[test]
    fn is_windows_drive_mount_recognizes_mnt_drive_paths() {
        // The `/mnt/<letter>` shape is a WSL Windows-drive mount; other paths
        // (missing letter, multi-char, non-mnt root) are not.
        assert!(is_windows_drive_mount(Path::new("/mnt/c")));
        assert!(is_windows_drive_mount(Path::new("/mnt/d/project")));
        assert!(!is_windows_drive_mount(Path::new("/mnt/abc/project")));
        assert!(!is_windows_drive_mount(Path::new("/home/user/project")));
        assert!(!is_windows_drive_mount(Path::new("/mnt")));
    }

    #[test]
    fn is_filesystem_root_recognizes_unix_root_only() {
        assert!(is_filesystem_root(Path::new("/")));
        assert!(!is_filesystem_root(Path::new("/usr")));
        assert!(!is_filesystem_root(Path::new("/home/user")));
    }
}
