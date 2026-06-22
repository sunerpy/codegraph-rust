use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

const MARKER_BEGIN: &str = "# >>> codegraph sync hook >>>";
const MARKER_END: &str = "# <<< codegraph sync hook <<<";

pub type GitHookName = &'static str;
pub const DEFAULT_SYNC_HOOKS: &[GitHookName] = &["post-commit", "post-merge", "post-checkout"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHookResult {
    pub installed: Vec<String>,
    pub hooks_dir: Option<PathBuf>,
    pub skipped: Option<String>,
}

pub fn is_git_repo(project_root: impl AsRef<Path>) -> bool {
    git_output(
        project_root.as_ref(),
        &["rev-parse", "--is-inside-work-tree"],
    )
    .is_some_and(|out| out.trim() == "true")
}

pub fn install_git_sync_hooks(project_root: impl AsRef<Path>) -> Result<GitHookResult> {
    let Some(hooks_dir) = git_hooks_dir(project_root.as_ref()) else {
        return Ok(skipped("not a git repository"));
    };
    fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("create hooks dir {}", hooks_dir.display()))?;
    let block = marker_block();
    let mut installed = Vec::new();
    for hook in DEFAULT_SYNC_HOOKS {
        let file = hooks_dir.join(hook);
        let content = if file.exists() {
            let base = strip_marker_block(&fs::read_to_string(&file)?);
            let base = base.trim_end();
            if base.is_empty() {
                format!("#!/bin/sh\n{block}\n")
            } else {
                format!("{base}\n\n{block}\n")
            }
        } else {
            format!("#!/bin/sh\n{block}\n")
        };
        fs::write(&file, content)?;
        chmod_executable(&file);
        installed.push((*hook).to_string());
    }
    Ok(GitHookResult {
        installed,
        hooks_dir: Some(hooks_dir),
        skipped: None,
    })
}

pub fn remove_git_sync_hooks(project_root: impl AsRef<Path>) -> Result<GitHookResult> {
    let Some(hooks_dir) = git_hooks_dir(project_root.as_ref()) else {
        return Ok(skipped("not a git repository"));
    };
    let mut removed = Vec::new();
    for hook in DEFAULT_SYNC_HOOKS {
        let file = hooks_dir.join(hook);
        if !file.exists() {
            continue;
        }
        let original = fs::read_to_string(&file)?;
        if !original.contains(MARKER_BEGIN) {
            continue;
        }
        let stripped = strip_marker_block(&original);
        if effectively_empty(&stripped) {
            fs::remove_file(&file)?;
        } else {
            fs::write(&file, format!("{}\n", stripped.trim_end()))?;
            chmod_executable(&file);
        }
        removed.push((*hook).to_string());
    }
    Ok(GitHookResult {
        installed: removed,
        hooks_dir: Some(hooks_dir),
        skipped: None,
    })
}

pub fn is_sync_hook_installed(project_root: impl AsRef<Path>) -> bool {
    git_hooks_dir(project_root.as_ref()).is_some_and(|hooks_dir| {
        DEFAULT_SYNC_HOOKS.iter().any(|hook| {
            fs::read_to_string(hooks_dir.join(hook))
                .is_ok_and(|content| content.contains(MARKER_BEGIN))
        })
    })
}

fn git_hooks_dir(project_root: &Path) -> Option<PathBuf> {
    let out = git_output(project_root, &["rev-parse", "--git-path", "hooks"])?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    Some(if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    })
}

fn git_output(project_root: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
}

fn marker_block() -> String {
    // Port of the upstream marker-delimited hook snippet from
    // `upstream sync/git-hooks.ts:74-85`.
    [
        MARKER_BEGIN,
        "# Keeps the CodeGraph index fresh while the live file watcher is off",
        "# (e.g. WSL2 /mnt drives). Runs in the background so it never blocks git.",
        "# Managed by codegraph; remove with `codegraph uninit` or delete this block.",
        "if command -v codegraph >/dev/null 2>&1; then",
        "  ( codegraph sync >/dev/null 2>&1 & ) >/dev/null 2>&1",
        "fi",
        MARKER_END,
    ]
    .join("\n")
}

fn strip_marker_block(content: &str) -> String {
    let mut kept = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        match line.trim() {
            MARKER_BEGIN => in_block = true,
            MARKER_END => in_block = false,
            _ if !in_block => kept.push(line),
            _ => {}
        }
    }
    kept.join("\n")
}

fn effectively_empty(content: &str) -> bool {
    content
        .lines()
        .map(str::trim)
        .all(|line| line.is_empty() || line.starts_with("#!"))
}

// `file` is only consumed by the Unix `set_mode` path; on Windows the cfg block
// is compiled out, so the parameter is intentionally unused there.
#[cfg_attr(not(unix), allow(unused_variables))]
fn chmod_executable(file: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(file) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o755);
            let _ = fs::set_permissions(file, permissions);
        }
    }
}

fn skipped(reason: &str) -> GitHookResult {
    GitHookResult {
        installed: Vec::new(),
        hooks_dir: None,
        skipped: Some(reason.to_string()),
    }
}
