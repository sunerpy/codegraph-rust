use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeIndexMismatch {
    pub worktree_root: PathBuf,
    pub index_root: PathBuf,
}

pub fn git_worktree_root(dir: impl AsRef<Path>) -> Option<PathBuf> {
    let out = crate::git::git_command(dir.as_ref())
        .args(["rev-parse", "--show-toplevel"])
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::tests::TestDir;

    fn git_init(dir: &Path) -> bool {
        crate::git::git_command(dir)
            .args(["init"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn git_worktree_root_returns_repo_root_inside_repo() {
        let repo = TestDir::new("worktree-root");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        let sub = repo.path().join("src/inner");
        fs::create_dir_all(&sub).unwrap();

        let from_root = git_worktree_root(repo.path());
        let from_sub = git_worktree_root(&sub);
        assert!(from_root.is_some(), "root resolves to a worktree root");
        assert_eq!(
            from_root, from_sub,
            "a nested dir resolves to the same worktree root"
        );
        assert_eq!(from_root.unwrap(), realpath(repo.path()));
    }

    #[test]
    fn git_worktree_root_is_none_outside_repo() {
        let plain = TestDir::new("worktree-nonrepo");
        assert!(
            git_worktree_root(plain.path()).is_none(),
            "a non-git dir has no worktree root"
        );
    }

    #[test]
    fn detect_mismatch_is_none_when_paths_match() {
        let repo = TestDir::new("worktree-match");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        assert!(
            detect_worktree_index_mismatch(repo.path(), repo.path()).is_none(),
            "same worktree and index root is not a mismatch"
        );
    }

    #[test]
    fn detect_mismatch_is_none_when_index_root_is_not_a_repo() {
        let repo = TestDir::new("worktree-idx-nonrepo-work");
        if !git_init(repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        let non_repo_index = TestDir::new("worktree-idx-nonrepo-index");
        assert!(
            detect_worktree_index_mismatch(repo.path(), non_repo_index.path()).is_none(),
            "an index root that is not its own worktree root does not warn"
        );
    }

    #[test]
    fn detect_mismatch_is_some_for_two_distinct_worktrees() {
        let work = TestDir::new("worktree-distinct-work");
        let index = TestDir::new("worktree-distinct-index");
        if !git_init(work.path()) || !git_init(index.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        let mismatch = detect_worktree_index_mismatch(work.path(), index.path());
        assert!(
            mismatch.is_some(),
            "two distinct git worktree roots are a mismatch"
        );
        let mismatch = mismatch.unwrap();
        assert_eq!(mismatch.worktree_root, realpath(work.path()));
        assert_eq!(mismatch.index_root, realpath(index.path()));
    }

    #[test]
    fn detect_mismatch_is_none_when_start_path_is_not_a_repo() {
        let plain = TestDir::new("worktree-start-nonrepo");
        let index = TestDir::new("worktree-start-index");
        assert!(
            detect_worktree_index_mismatch(plain.path(), index.path()).is_none(),
            "a non-repo start path yields no worktree root, so no mismatch"
        );
    }

    #[test]
    fn detect_mismatch_is_none_when_index_root_is_a_repo_subdir() {
        // A repo SUBDIR resolves via git to the repo root (!= itself), so line 37
        // suppresses the warning: a subdir index is not a distinct worktree.
        let work = TestDir::new("worktree-idx-subdir-work");
        let index_repo = TestDir::new("worktree-idx-subdir-index");
        if !git_init(work.path()) || !git_init(index_repo.path()) {
            eprintln!("skipping: git init unavailable");
            return;
        }
        let index_subdir = index_repo.path().join("nested/pkg");
        fs::create_dir_all(&index_subdir).unwrap();
        assert!(
            detect_worktree_index_mismatch(work.path(), &index_subdir).is_none(),
            "an index root that is a repo subdir (not the repo root) does not warn"
        );
    }

    #[test]
    fn warning_and_notice_render_both_roots() {
        let mismatch = WorktreeIndexMismatch {
            worktree_root: PathBuf::from("/work/tree"),
            index_root: PathBuf::from("/index/tree"),
        };
        let warning = worktree_mismatch_warning(&mismatch);
        assert!(warning.contains("/work/tree"));
        assert!(warning.contains("/index/tree"));
        assert!(warning.contains("codegraph init"));

        let notice = worktree_mismatch_notice(&mismatch);
        assert!(notice.contains("/work/tree"));
        assert!(notice.contains("/index/tree"));
        assert!(notice.contains("codegraph init"));
    }
}
