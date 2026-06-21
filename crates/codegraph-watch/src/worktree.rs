use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeIndexMismatch {
    pub worktree_root: PathBuf,
    pub index_root: PathBuf,
}

pub fn git_worktree_root(dir: impl AsRef<Path>) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir.as_ref())
        .output()
        .ok()?
        .stdout;
    let root = String::from_utf8_lossy(&out).trim().to_string();
    if root.is_empty() {
        return None;
    }
    Some(realpath(root))
}

pub fn detect_worktree_index_mismatch(
    start_path: impl AsRef<Path>,
    index_root: impl AsRef<Path>,
) -> Option<WorktreeIndexMismatch> {
    // Mirrors `upstream sync/worktree.ts:64-80`: only warn when the
    // active path and index root are distinct real git worktree roots.
    let worktree_root = git_worktree_root(start_path)?;
    let index_root = realpath(index_root);
    if worktree_root == index_root {
        return None;
    }
    if git_worktree_root(&index_root)? != index_root {
        return None;
    }
    Some(WorktreeIndexMismatch {
        worktree_root,
        index_root,
    })
}

pub fn worktree_mismatch_warning(mismatch: &WorktreeIndexMismatch) -> String {
    format!(
        "This CodeGraph index belongs to a different git working tree.\n  Running in: {}\n  Index from: {}\nResults reflect that tree's code (often a different branch), not this worktree - symbols changed only here are missing. Run \"codegraph init -i\" in this worktree for a worktree-local index.",
        mismatch.worktree_root.display(),
        mismatch.index_root.display()
    )
}

pub fn worktree_mismatch_notice(mismatch: &WorktreeIndexMismatch) -> String {
    format!(
        "CodeGraph results below come from a different git worktree ({}), not where you're working ({}) - they may reflect another branch, and symbols changed only here are missing. Run \"codegraph init -i\" here for a worktree-local index.",
        mismatch.index_root.display(),
        mismatch.worktree_root.display()
    )
}

fn realpath(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
