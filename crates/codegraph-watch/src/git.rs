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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::TestDir;

    /// Initialize a real git repo in `dir` via `git init`. Configures a local
    /// user so any commit-adjacent git command works; returns whether git init
    /// succeeded (tests skip themselves cleanly if git is unavailable).
    fn git_init(dir: &Path) -> bool {
        let ok = Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if ok {
            for args in [
                ["config", "user.email", "test@example.com"],
                ["config", "user.name", "Test"],
            ] {
                let _ = Command::new("git").args(args).current_dir(dir).output();
            }
        }
        ok
    }

    #[test]
    fn is_git_repo_true_inside_repo_false_outside() {
        let repo = TestDir::new("git-is-repo");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        assert!(
            is_git_repo(repo.path()),
            "an initialized repo is a git repo"
        );

        let plain = TestDir::new("git-not-repo");
        assert!(
            !is_git_repo(plain.path()),
            "a bare temp dir is not a git repo"
        );
    }

    #[test]
    fn install_then_detect_then_remove_sync_hooks_roundtrip() {
        let repo = TestDir::new("git-hooks-roundtrip");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }

        // Before install: no sync hook.
        assert!(!is_sync_hook_installed(repo.path()));

        // Install writes every DEFAULT_SYNC_HOOKS file with the marker block.
        let result = install_git_sync_hooks(repo.path()).unwrap();
        assert_eq!(result.installed.len(), DEFAULT_SYNC_HOOKS.len());
        assert!(result.hooks_dir.is_some());
        assert!(result.skipped.is_none());
        let hooks_dir = result.hooks_dir.clone().unwrap();
        for hook in DEFAULT_SYNC_HOOKS {
            let content = fs::read_to_string(hooks_dir.join(hook)).unwrap();
            assert!(content.contains(MARKER_BEGIN), "{hook} has begin marker");
            assert!(content.contains(MARKER_END), "{hook} has end marker");
            assert!(content.starts_with("#!/bin/sh"), "{hook} is a shell script");
        }

        assert!(is_sync_hook_installed(repo.path()));

        let removed = remove_git_sync_hooks(repo.path()).unwrap();
        assert_eq!(removed.installed.len(), DEFAULT_SYNC_HOOKS.len());
        assert!(!is_sync_hook_installed(repo.path()));
        for hook in DEFAULT_SYNC_HOOKS {
            assert!(
                !hooks_dir.join(hook).exists(),
                "{hook} file removed when only the marker block remained"
            );
        }
    }

    #[test]
    fn install_preserves_existing_hook_body_and_remove_keeps_it() {
        let repo = TestDir::new("git-hooks-preserve");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        let hooks_dir = git_hooks_dir(repo.path()).unwrap();
        fs::create_dir_all(&hooks_dir).unwrap();
        // A pre-existing post-commit hook with real user content.
        let existing = "#!/bin/sh\necho existing-user-hook\n";
        fs::write(hooks_dir.join("post-commit"), existing).unwrap();

        install_git_sync_hooks(repo.path()).unwrap();
        let after_install = fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
        assert!(
            after_install.contains("echo existing-user-hook"),
            "install must preserve the user's existing hook body"
        );
        assert!(after_install.contains(MARKER_BEGIN));

        remove_git_sync_hooks(repo.path()).unwrap();
        let after_remove = fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
        assert!(after_remove.contains("echo existing-user-hook"));
        assert!(!after_remove.contains(MARKER_BEGIN));
        assert!(hooks_dir.join("post-commit").exists());
    }

    #[test]
    fn install_idempotent_does_not_duplicate_marker_block() {
        let repo = TestDir::new("git-hooks-idempotent");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        install_git_sync_hooks(repo.path()).unwrap();
        install_git_sync_hooks(repo.path()).unwrap();
        let hooks_dir = git_hooks_dir(repo.path()).unwrap();
        let content = fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
        assert_eq!(
            content.matches(MARKER_BEGIN).count(),
            1,
            "a second install must not duplicate the marker block"
        );
    }

    #[test]
    fn hook_ops_skip_when_not_a_git_repo() {
        let plain = TestDir::new("git-hooks-nonrepo");
        let install = install_git_sync_hooks(plain.path()).unwrap();
        assert!(install.installed.is_empty());
        assert!(install.hooks_dir.is_none());
        assert_eq!(install.skipped.as_deref(), Some("not a git repository"));

        let remove = remove_git_sync_hooks(plain.path()).unwrap();
        assert!(remove.installed.is_empty());
        assert_eq!(remove.skipped.as_deref(), Some("not a git repository"));

        assert!(!is_sync_hook_installed(plain.path()));
    }

    #[test]
    fn remove_is_noop_when_hooks_absent_or_lack_marker() {
        let repo = TestDir::new("git-hooks-remove-noop");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        // No hooks installed yet: remove touches nothing.
        let removed = remove_git_sync_hooks(repo.path()).unwrap();
        assert!(removed.installed.is_empty());

        // A hook file WITHOUT the marker is left untouched by remove.
        let hooks_dir = git_hooks_dir(repo.path()).unwrap();
        fs::create_dir_all(&hooks_dir).unwrap();
        let user_only = "#!/bin/sh\necho no-marker-here\n";
        fs::write(hooks_dir.join("post-commit"), user_only).unwrap();
        let removed = remove_git_sync_hooks(repo.path()).unwrap();
        assert!(
            removed.installed.is_empty(),
            "a marker-less hook is not counted as removed"
        );
        assert_eq!(
            fs::read_to_string(hooks_dir.join("post-commit")).unwrap(),
            user_only,
            "a marker-less hook body is preserved verbatim"
        );
    }

    #[test]
    fn strip_marker_block_removes_only_the_delimited_region() {
        let content = format!(
            "#!/bin/sh\necho keep-me\n{}\ninner\n{}\necho keep-tail\n",
            MARKER_BEGIN, MARKER_END
        );
        let stripped = strip_marker_block(&content);
        assert!(stripped.contains("echo keep-me"));
        assert!(stripped.contains("echo keep-tail"));
        assert!(!stripped.contains("inner"));
        assert!(!stripped.contains(MARKER_BEGIN));
        assert!(!stripped.contains(MARKER_END));
    }

    #[test]
    fn effectively_empty_recognizes_blank_and_shebang_only() {
        assert!(effectively_empty(""));
        assert!(effectively_empty("\n  \n"));
        assert!(effectively_empty("#!/bin/sh\n"));
        assert!(effectively_empty("#!/bin/sh\n   \n"));
        assert!(!effectively_empty("#!/bin/sh\necho hi\n"));
        assert!(!effectively_empty("echo hi\n"));
    }

    #[test]
    fn marker_block_is_well_formed() {
        let block = marker_block();
        assert!(block.starts_with(MARKER_BEGIN));
        assert!(block.ends_with(MARKER_END));
        assert!(block.contains("codegraph sync"));
    }

    #[test]
    fn install_removes_empty_hook_files_on_remove() {
        // A hook file that is JUST a shebang plus the marker collapses to empty
        // after stripping, so remove deletes the file entirely.
        let repo = TestDir::new("git-hooks-empty-remove");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        install_git_sync_hooks(repo.path()).unwrap();
        let hooks_dir = git_hooks_dir(repo.path()).unwrap();
        // post-merge was created with just shebang + marker.
        assert!(hooks_dir.join("post-merge").exists());
        remove_git_sync_hooks(repo.path()).unwrap();
        assert!(
            !hooks_dir.join("post-merge").exists(),
            "a shebang+marker-only hook is deleted on remove"
        );
    }
}
