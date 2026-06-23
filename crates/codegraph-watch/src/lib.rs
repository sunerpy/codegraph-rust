mod git;
mod policy;
mod sync;
mod watcher;
mod worktree;

pub use git::{
    install_git_sync_hooks, is_git_repo, is_sync_hook_installed, remove_git_sync_hooks,
    GitHookName, GitHookResult, DEFAULT_SYNC_HOOKS,
};
pub use policy::{watch_disabled_reason, WatchPolicy, CODEGRAPH_NO_WATCH};
pub use sync::{
    sync_changed_paths, sync_project_once, sync_project_once_with_progress, SyncOutcome,
};
pub use watcher::{start_serve_watcher, PendingFile, ProjectWatcher, WatchOptions};
pub use worktree::{
    detect_worktree_index_mismatch, git_worktree_root, worktree_mismatch_notice,
    worktree_mismatch_warning, WorktreeIndexMismatch,
};
