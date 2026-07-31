use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use codegraph_extract::{ExtensionOverrides, detect_language_with};

pub const CODEGRAPH_NO_WATCH: &str = "CODEGRAPH_NO_WATCH";

const WATCH_ONLY_DEFAULT_IGNORE_DIRS: &[&str] = &[
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
    /// STRUCTURAL skips, mirroring the scan's pre-include prune in `scan_dir`:
    /// the project's configured `ignore_dirs`, the watch-only defaults, and the
    /// egg/cmake/bazel forms. Matched with the watcher's `.gitignore`-style
    /// [`rule_matches`]; nothing can undo one — not a `.gitignore` negation, not
    /// `include` — exactly as `scan_dir` `continue`s on them before it evaluates
    /// any pattern set.
    structural_ignores: Vec<String>,
    /// The root `.gitignore` rules in file order — the NEGOTIABLE tail of the
    /// last-match-wins stream (`exclude` first, these second, mirroring the
    /// scan's `pattern_sets`), so a `!pattern` line here re-includes what an
    /// `exclude` or an earlier `.gitignore` line dropped.
    gitignore_rules: Vec<IgnoreRule>,
    include: Vec<String>,
    exclude: Vec<String>,
    /// The addressed project's custom extension→language overrides, so a file the
    /// project declared as source is HANDLED by the watcher exactly as the scan
    /// indexes it. Empty by default (built-in detection only).
    extensions: Arc<ExtensionOverrides>,
}

impl WatchPolicy {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let indexing = codegraph_core::config::IndexingConfig::default();
        Self::with_config(root, &indexing.ignore_dirs, &[], &[])
    }

    /// Like [`new`](Self::new) but threads the project's configured `ignore_dirs`,
    /// `include`, and `exclude` scope into the watcher. Configured ignored dirs
    /// mirror the scan's structural prune and cannot be re-included; an included
    /// gitignored dir is watched, while an explicit `exclude` always wins.
    pub fn with_config(
        root: impl AsRef<Path>,
        ignore_dirs: &[String],
        include: &[String],
        exclude: &[String],
    ) -> Self {
        // Mirrors the upstream built-in ignore seed and root .gitignore merge from
        // `upstream extraction/index.ts:117-161,242-246`, split into the scan's
        // two tiers: the structural prune nothing can undo, then the negotiable
        // `.gitignore` stream whose negations can opt an excluded path back in.
        let root = root.as_ref().to_path_buf();
        let structural_ignores = ignore_dirs
            .iter()
            .map(String::as_str)
            .chain(WATCH_ONLY_DEFAULT_IGNORE_DIRS.iter().copied())
            .map(|dir| format!("{dir}/"))
            .chain(
                ["*.egg-info/", "cmake-build-*/", "bazel-*/"]
                    .into_iter()
                    .map(str::to_string),
            )
            .collect::<Vec<_>>();
        let gitignore_rules = read_gitignore_rules(&root);
        Self {
            root,
            structural_ignores,
            gitignore_rules,
            include: include.to_vec(),
            exclude: exclude.to_vec(),
            extensions: ExtensionOverrides::empty(),
        }
    }

    /// Adopt the addressed project's extension overrides, so
    /// [`should_handle_file`](Self::should_handle_file) treats a project-declared
    /// custom extension as source. Without this the policy uses built-in
    /// detection only, which is the zero-config behavior.
    #[must_use]
    pub fn with_extension_overrides(mut self, extensions: Arc<ExtensionOverrides>) -> Self {
        self.extensions = extensions;
        self
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
            && detect_language_with(relative, &self.extensions)
                != codegraph_core::types::Language::Unknown
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

    /// The watcher's counterpart to the scan's three-stage decision in
    /// `scan_project`/`scan_dir`, in the SAME order, so `sync`/watch keeps
    /// exactly the files `index --force` keeps.
    fn is_ignored(&self, relative: &str, is_dir: bool) -> bool {
        // 1. Structural prune. `scan_dir` `continue`s on a configured ignore dir
        //    before any pattern set or include runs, so neither a `.gitignore`
        //    negation nor `include` can resurface one here either.
        if self.matches_structural(relative, is_dir) {
            return true;
        }
        // 2. The negotiable last-match-wins stream, evaluated for EVERY path —
        //    which is what makes a configured `exclude` apply whether or not
        //    `include` is set.
        if !self.matches_negotiable(relative, is_dir) {
            return false;
        }
        // 3. Post-model include force-inclusion (#1063), mirroring
        //    `IncludeSet::forces`/`wants_descend`: a stream-ignored path returns
        //    iff `include` matches and no explicit `exclude` knocks it out.
        if self.include.is_empty() {
            return true;
        }
        if self.matches_exclude(relative) {
            return true;
        }
        let force = if is_dir {
            self.include
                .iter()
                .any(|pattern| include_touches_dir(relative, pattern))
        } else {
            self.include
                .iter()
                .any(|pattern| include_file_matches(relative, pattern))
        };
        !force
    }

    fn matches_structural(&self, relative: &str, is_dir: bool) -> bool {
        self.structural_ignores
            .iter()
            .any(|pattern| rule_matches(pattern, relative, is_dir))
    }

    /// Last-match-wins over `exclude` then `.gitignore`, the watcher's port of
    /// the scan's `is_path_ignored` over its ordered `pattern_sets`. Each side
    /// keeps its own matcher: `exclude` uses the SHARED whole-path matcher the
    /// scan uses, `.gitignore` the watcher's [`rule_matches`] glob semantics.
    fn matches_negotiable(&self, relative: &str, is_dir: bool) -> bool {
        let mut ignored = self.matches_exclude(relative);
        for rule in &self.gitignore_rules {
            if rule_matches(&rule.pattern, relative, is_dir) {
                ignored = !rule.negated;
            }
        }
        ignored
    }

    fn matches_exclude(&self, relative: &str) -> bool {
        self.exclude
            .iter()
            .any(|pattern| codegraph_extract::include_exclude_pattern_matches(pattern, relative))
    }
}

/// Whether an include `pattern` matches the FILE at root-relative `relative`
/// (watcher side, byte-identical to the engine's `include_file_matches`): a
/// `dir/**` (or bare `**`) matches every file under its prefix, and every other
/// form defers to the SHARED whole-path matcher `include_exclude_pattern_matches`
/// — NOT the watcher's basename-glob `rule_matches` — so `gen*` matches
/// `gen/helper.ts` here exactly as it does in the scan.
fn include_file_matches(relative: &str, pattern: &str) -> bool {
    if let Some(prefix) = pattern
        .strip_suffix("/**")
        .or_else(|| (pattern == "**").then_some(""))
    {
        return prefix.is_empty()
            || relative == prefix
            || relative.starts_with(&format!("{prefix}/"));
    }
    codegraph_extract::include_exclude_pattern_matches(pattern, relative)
}

/// Whether an include `pattern` matches, or is an ancestor of, the DIRECTORY
/// `relative` — so a gitignored ancestor of an included path is still watched
/// (watcher side, mirroring the engine's `include_touches_dir`). A bare-name
/// pattern matches at any depth, so it touches every dir.
fn include_touches_dir(relative: &str, pattern: &str) -> bool {
    if !pattern.contains('/') && !pattern.ends_with('*') {
        return true;
    }
    let stem = {
        let trimmed = pattern.trim_end_matches('/');
        match trimmed.split_once('*') {
            Some((prefix, _)) => prefix.trim_end_matches('/'),
            None => trimmed,
        }
    };
    if stem.is_empty() {
        return true;
    }
    relative == stem
        || relative.starts_with(&format!("{stem}/"))
        || stem.starts_with(&format!("{relative}/"))
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
    use crate::test_env::{EnvGuard, env_guard};

    #[test]
    fn watch_disabled_when_root_is_home() {
        let mut env = env_guard();
        let home = crate::sync::tests::TestDir::new("watch-policy-home");
        env.set(EnvGuard::home_key(), home.path());
        env.remove("CODEGRAPH_FORCE_WATCH");
        env.remove(CODEGRAPH_NO_WATCH);

        let reason = watch_disabled_reason(home.path(), false);
        assert!(reason.is_some(), "watching HOME must be disabled");
        assert!(reason.unwrap().contains("home directory"));
    }

    #[test]
    fn watch_disabled_for_home_even_with_force_watch() {
        let mut env = env_guard();
        let home = crate::sync::tests::TestDir::new("watch-policy-home-force");
        env.set(EnvGuard::home_key(), home.path());
        env.set("CODEGRAPH_FORCE_WATCH", "1");
        env.remove(CODEGRAPH_NO_WATCH);

        assert!(
            watch_disabled_reason(home.path(), false).is_some(),
            "CODEGRAPH_FORCE_WATCH must NOT re-enable a home walk"
        );
    }

    #[test]
    fn watch_disabled_when_root_is_filesystem_root() {
        let mut env = env_guard();
        env.remove("CODEGRAPH_FORCE_WATCH");
        env.remove(CODEGRAPH_NO_WATCH);

        let reason = watch_disabled_reason(Path::new("/"), false);
        assert!(reason.is_some(), "watching `/` must be disabled");
        assert!(reason.unwrap().contains("filesystem root"));
    }

    #[test]
    fn watch_allowed_for_normal_project_subdir() {
        let mut env = env_guard();
        let home = crate::sync::tests::TestDir::new("watch-policy-subdir-home");
        env.set(EnvGuard::home_key(), home.path());
        env.remove("CODEGRAPH_FORCE_WATCH");
        env.remove(CODEGRAPH_NO_WATCH);

        let project = home.path().join("workspace/proj");
        fs::create_dir_all(&project).unwrap();
        assert!(
            watch_disabled_reason(&project, false).is_none(),
            "a normal project subdir must be watchable"
        );
    }

    #[test]
    fn gitignore_negation_reincludes_a_gitignored_dir_but_not_a_structural_skip() {
        // A `.gitignore` negation is part of the NEGOTIABLE last-match-wins
        // stream, so it flips back what an earlier `.gitignore` line dropped. It
        // can NOT resurface a configured `ignore_dirs` entry (`vendor`,
        // `node_modules`): `scan_dir` prunes those before any pattern set runs,
        // so the watcher must prune them too.
        let dir = crate::sync::tests::TestDir::new("watch-policy-negation");
        fs::write(
            dir.path().join(".gitignore"),
            "Tools/\n!Tools/\n!vendor/\n!node_modules/\n",
        )
        .unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(policy.should_handle_file("Tools/first_party.ts"));
        assert!(!policy.should_handle_file("vendor/first_party.ts"));
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
    fn default_godot_dirs_are_ignored_without_gitignore() {
        // Given: a project with no `.gitignore`, so only the default policy applies.
        let dir = crate::sync::tests::TestDir::new("watch-policy-godot-defaults");
        assert!(!dir.path().join(".gitignore").exists());

        // When: the watcher evaluates Godot noise and normal business paths.
        let policy = WatchPolicy::new(dir.path());

        // Then: both Godot noise trees are pruned, while normal source stays watched.
        assert!(!policy.should_watch_dir(".godot"));
        assert!(!policy.should_handle_file(".godot/cache_junk.gd"));
        assert!(!policy.should_watch_dir("addons"));
        assert!(!policy.should_handle_file("addons/vendor_plugin/plugin.gd"));
        assert!(policy.should_watch_dir("src/gameplay"));
        assert!(policy.should_handle_file("src/gameplay/player.gd"));
    }

    #[test]
    fn with_config_include_does_not_reinclude_configured_ignore_dir() {
        // Given: no `.gitignore`, the default ignored dirs, and a first-party addon
        // explicitly included by config.
        let dir = crate::sync::tests::TestDir::new("watch-policy-addons-include");
        assert!(!dir.path().join(".gitignore").exists());
        let ignore_dirs = codegraph_core::config::IndexingConfig::default().ignore_dirs;
        let include = ["addons/first_party/**".to_string()];
        let included = WatchPolicy::with_config(dir.path(), &ignore_dirs, &include, &[]);

        // When/Then: configured ignored dirs are structural scan skips, so include
        // cannot resurface the directory or any source file below it.
        assert!(!included.should_watch_dir("addons"));
        assert!(!included.should_watch_dir("addons/first_party"));
        assert!(!included.should_handle_file("addons/first_party/tool.gd"));
        assert!(!included.should_handle_file("addons/vendor_plugin/plugin.gd"));
    }

    #[test]
    fn configured_ignore_dirs_override_reincludes_addons() {
        // Given: the project drops `addons` from its configured ignore dirs while
        // retaining `.godot`, mirroring the scan-side override contract.
        let dir = crate::sync::tests::TestDir::new("watch-policy-addons-override");
        assert!(!dir.path().join(".gitignore").exists());
        let mut ignore_dirs = codegraph_core::config::IndexingConfig::default().ignore_dirs;
        ignore_dirs.retain(|entry| entry != "addons");

        // When: the watcher builds its policy from that project's configured list.
        let policy = WatchPolicy::with_config(dir.path(), &ignore_dirs, &[], &[]);

        // Then: first-party addons are watched, while the retained Godot cache skip
        // remains structurally ignored.
        assert!(policy.should_watch_dir("addons"));
        assert!(policy.should_watch_dir("addons/first_party"));
        assert!(policy.should_handle_file("addons/first_party/tool.gd"));
        assert!(!policy.should_watch_dir(".godot"));
        assert!(!policy.should_handle_file(".godot/cache_junk.gd"));
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
        let mut env = env_guard();
        let home = crate::sync::tests::TestDir::new("too-broad-home");
        env.set(EnvGuard::home_key(), home.path());

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
        let mut env = env_guard();
        let home = crate::sync::tests::TestDir::new("too-broad-home-dot");
        env.set(EnvGuard::home_key(), home.path());

        let with_dot = home.path().join(".");
        assert!(
            too_broad_root_reason(&with_dot).is_some(),
            "`$HOME/.` must normalize to `$HOME` and be too broad"
        );
    }

    #[test]
    fn watch_disabled_when_root_is_home_with_trailing_dot() {
        let mut env = env_guard();
        let home = crate::sync::tests::TestDir::new("watch-policy-home-dot");
        env.set(EnvGuard::home_key(), home.path());
        env.remove("CODEGRAPH_FORCE_WATCH");
        env.remove(CODEGRAPH_NO_WATCH);

        let with_dot = home.path().join(".");
        assert!(
            watch_disabled_reason(&with_dot, false).is_some(),
            "`$HOME/.` must normalize to `$HOME` and be disabled"
        );
    }

    #[test]
    fn watch_disabled_when_no_watch_flag_is_set() {
        let mut env = env_guard();
        let project = crate::sync::tests::TestDir::new("watch-policy-flag");
        env.remove(CODEGRAPH_NO_WATCH);

        // The explicit `no_watch` parameter wins even for a normal project dir.
        let reason = watch_disabled_reason(project.path(), true);
        assert_eq!(reason.as_deref(), Some("CODEGRAPH_NO_WATCH=1 is set"));
    }

    #[test]
    fn watch_disabled_when_no_watch_env_is_set() {
        let mut env = env_guard();
        let project = crate::sync::tests::TestDir::new("watch-policy-env");
        env.set(CODEGRAPH_NO_WATCH, "1");

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
    fn with_config_include_watches_gitignored_path_but_not_builtin_skip() {
        // #1063: an included gitignored dir is watched and its files handled,
        // while a built-in skip named in include stays pruned and an explicit
        // exclude still wins.
        let dir = crate::sync::tests::TestDir::new("watch-policy-include");
        fs::write(dir.path().join(".gitignore"), "Tools/\nLocal/\n").unwrap();
        let ignore_dirs = codegraph_core::config::IndexingConfig::default().ignore_dirs;
        let policy = WatchPolicy::with_config(
            dir.path(),
            &ignore_dirs,
            &["Tools/".to_string(), "Local/ts/app.ts".to_string()],
            &[],
        );

        // An included gitignored dir (and a file under it) is now watched/handled.
        assert!(policy.should_watch_dir("Tools"));
        assert!(policy.should_handle_file("Tools/helper.ts"));
        // A file include under a gitignored ancestor: the ancestor is watched and
        // the file handled, but a non-included sibling stays ignored.
        assert!(policy.should_watch_dir("Local"));
        assert!(policy.should_watch_dir("Local/ts"));
        assert!(policy.should_handle_file("Local/ts/app.ts"));
        assert!(!policy.should_handle_file("Local/ts/other.ts"));
        // A built-in skip is NEVER re-watched, even if named in include.
        let builtin = WatchPolicy::with_config(
            dir.path(),
            &ignore_dirs,
            &["node_modules/".to_string()],
            &[],
        );
        assert!(!builtin.should_watch_dir("node_modules"));
        assert!(!builtin.should_handle_file("node_modules/pkg/index.ts"));
        // An explicit exclude wins over include.
        let excluded = WatchPolicy::with_config(
            dir.path(),
            &ignore_dirs,
            &["Tools/".to_string()],
            &["Tools/".to_string()],
        );
        assert!(!excluded.should_watch_dir("Tools"));
        assert!(!excluded.should_handle_file("Tools/helper.ts"));
    }

    #[test]
    fn empty_include_leaves_policy_byte_identical() {
        // #1063: WatchPolicy::new equals with_config using the default ignore dirs
        // and empty include/exclude; an empty include adds no force-inclusion, so
        // it changes no policy decision. It does NOT disable the `exclude` tier —
        // that is evaluated for every path (see
        // `configured_exclude_applies_with_empty_include` in tests/).
        let dir = crate::sync::tests::TestDir::new("watch-policy-include-empty");
        fs::write(dir.path().join(".gitignore"), "vendor/\n").unwrap();
        let ignore_dirs = codegraph_core::config::IndexingConfig::default().ignore_dirs;
        let policy = WatchPolicy::with_config(dir.path(), &ignore_dirs, &[], &[]);
        assert!(!policy.should_watch_dir("vendor"));
        assert!(!policy.should_handle_file("vendor/dep.ts"));
        assert!(!policy.should_watch_dir("node_modules"));
        assert!(policy.should_watch_dir("src"));
        assert!(policy.should_handle_file("src/app.ts"));
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
        let mut env = env_guard();
        let project = crate::sync::tests::TestDir::new("watch-policy-force");
        env.set(EnvGuard::home_key(), project.path());
        env.remove(CODEGRAPH_NO_WATCH);
        env.set("CODEGRAPH_FORCE_WATCH", "1");

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
