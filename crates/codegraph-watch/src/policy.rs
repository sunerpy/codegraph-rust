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
        return candidate == format!("{dir}/") || candidate.starts_with(&format!("{dir}/"));
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

    #[test]
    fn gitignore_negation_reincludes_default_ignored_dir() {
        let dir = crate::sync::tests::TestDir::new("watch-policy-negation");
        fs::write(dir.path().join(".gitignore"), "!vendor/\n").unwrap();
        let policy = WatchPolicy::new(dir.path());
        assert!(policy.should_handle_file("vendor/first_party.ts"));
        assert!(!policy.should_handle_file("node_modules/pkg/index.ts"));
    }
}
