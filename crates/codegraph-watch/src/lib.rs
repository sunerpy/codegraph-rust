mod git;
mod policy;
mod sync;
mod watcher;
mod worktree;

pub use git::{
    DEFAULT_SYNC_HOOKS, GitHookName, GitHookResult, install_git_sync_hooks, is_git_repo,
    is_sync_hook_installed, remove_git_sync_hooks,
};
pub use policy::{
    CODEGRAPH_NO_WATCH, TooBroadRoot, WatchPolicy, too_broad_root_reason, watch_disabled_reason,
};
pub use sync::{
    SyncOutcome, sync_changed_paths, sync_project_once, sync_project_once_with_progress,
};
pub use watcher::{PendingFile, ProjectWatcher, WatchOptions, start_serve_watcher};
pub use worktree::{
    WorktreeIndexMismatch, detect_worktree_index_mismatch, git_worktree_root,
    worktree_mismatch_notice, worktree_mismatch_warning,
};
