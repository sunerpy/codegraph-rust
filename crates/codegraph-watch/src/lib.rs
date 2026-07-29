mod git;
mod migrate;
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
    SyncCancellation, SyncOutcome, sync_changed_paths, sync_project_once,
    sync_project_once_cancellable, sync_project_once_with_progress,
};
pub use watcher::{
    PendingFile, ProjectWatcher, WatchOptions, start_serve_watcher, watch_options_for_project,
};
pub use worktree::{
    WorktreeIndexMismatch, detect_worktree_index_mismatch, git_worktree_root,
    worktree_mismatch_notice, worktree_mismatch_warning,
};

/// Test-only: the ONE process-wide environment lock for this crate.
///
/// `HOME`, `CODEGRAPH_NO_WATCH`, and `CODEGRAPH_FORCE_WATCH` are process-global,
/// and `cargo test` runs this crate's unit tests as threads of ONE process. The
/// `policy` tests mutate all three; the `watcher` tests call
/// `ProjectWatcher::start`, which READS them through `watch_disabled_reason` and
/// returns `Ok(None)` when they say "don't watch". A watcher test that does not
/// hold this lock can therefore observe another test's `CODEGRAPH_NO_WATCH=1`
/// and get `None` back. Both sides must go through [`test_env::env_guard`].
#[cfg(test)]
pub(crate) mod test_env {
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(String, Option<OsString>)>,
        expected: Vec<(String, Option<OsString>)>,
    }

    pub(crate) fn env_guard() -> EnvGuard {
        EnvGuard {
            _lock: ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
            saved: Vec::new(),
            expected: Vec::new(),
        }
    }

    impl EnvGuard {
        fn remember(&mut self, key: &str) {
            if !self.saved.iter().any(|(k, _)| k == key) {
                self.saved.push((key.to_string(), std::env::var_os(key)));
            }
        }

        fn expect(&mut self, key: &str, value: Option<OsString>) {
            match self.expected.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => self.expected.push((key.to_string(), value)),
            }
        }

        pub(crate) fn set(&mut self, key: &str, value: impl AsRef<OsStr>) -> &mut Self {
            let value = value.as_ref().to_os_string();
            self.remember(key);
            // SAFETY: ENV_LOCK is held for this guard's whole lifetime, so no
            // other test thread of this crate reads or writes env concurrently.
            unsafe { std::env::set_var(key, &value) };
            self.expect(key, Some(value));
            self.assert_intact();
            self
        }

        pub(crate) fn remove(&mut self, key: &str) -> &mut Self {
            self.remember(key);
            // SAFETY: as in `set` — serialized by ENV_LOCK.
            unsafe { std::env::remove_var(key) };
            self.expect(key, None);
            self.assert_intact();
            self
        }

        /// Panic if a variable this guard wrote changed underneath it — the
        /// escape detector for an unlocked env mutation elsewhere.
        pub(crate) fn assert_intact(&self) {
            for (key, want) in &self.expected {
                assert_eq!(
                    &std::env::var_os(key),
                    want,
                    "env var {key} changed underneath this guard: another test \
                     mutated process-global env without holding the shared lock"
                );
            }
        }

        pub(crate) fn home_key() -> &'static str {
            if cfg!(windows) { "USERPROFILE" } else { "HOME" }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..).rev() {
                match value {
                    // SAFETY: still holding ENV_LOCK; single-threaded here.
                    Some(v) => unsafe { std::env::set_var(&key, v) },
                    None => unsafe { std::env::remove_var(&key) },
                }
            }
        }
    }
}
