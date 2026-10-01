//! Deterministic symlink following for the project scan (upstream #935).
//!
//! The project's real tree is scanned first. A symlinked directory met during
//! a walk is queued under `(hops, logical path)` and followed only after every
//! walk with fewer hops, so a canonical directory is scanned once, under the
//! logical path that reaches it through the fewest symlinks, ties going to the
//! lexicographically smallest path. Within one hop level no queued link is a
//! logical prefix of another, so popping the queue in order realizes exactly
//! that rule, and the outcome depends on the filesystem alone, never on
//! `read_dir` order.
//!
//! A link is never followed to the project root or one of its ancestors (that
//! would rescan the project, or the tree above it, under an alias), nor into
//! the canonical `.git` or a reserved index root. A target that cannot be
//! canonicalized or read is skipped like a broken link, so a stray link never
//! fails an index.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// A symlinked directory to scan next, already recorded as scanned.
pub(crate) struct LinkedDir<T> {
    pub(crate) hops: usize,
    pub(crate) relative: String,
    pub(crate) path: PathBuf,
    pub(crate) canonical: PathBuf,
    pub(crate) state: T,
}

pub(crate) struct LinkWalk<T> {
    canonical_root: Option<PathBuf>,
    blocked: Vec<PathBuf>,
    /// Canonical path of every scanned directory → the logical path it was
    /// scanned under.
    scanned: HashMap<PathBuf, String>,
    queue: BTreeMap<(usize, String), (PathBuf, T)>,
}

impl<T> LinkWalk<T> {
    /// `blocked` names the directories no link may lead into. Those that do
    /// not exist are dropped; the rest are compared canonically.
    pub(crate) fn new(root: &Path, blocked: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            canonical_root: fs::canonicalize(root).ok(),
            blocked: blocked
                .into_iter()
                .filter_map(|path| fs::canonicalize(path).ok())
                .collect(),
            scanned: HashMap::new(),
            queue: BTreeMap::new(),
        }
    }

    /// The project root's canonical path. `None` when it cannot be resolved,
    /// and then no link is ever followed.
    pub(crate) fn canonical_root(&self) -> Option<&Path> {
        self.canonical_root.as_deref()
    }

    /// Record a directory about to be scanned under `relative`. `false` when
    /// its canonical path was scanned already or lies in a blocked prefix, and
    /// the directory is then skipped.
    pub(crate) fn enter(&mut self, canonical: PathBuf, relative: &str) -> bool {
        if self.is_blocked(&canonical) {
            return false;
        }
        match self.scanned.entry(canonical) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(relative.to_string());
                true
            }
        }
    }

    pub(crate) fn queue(&mut self, hops: usize, relative: String, path: PathBuf, state: T) {
        self.queue.entry((hops, relative)).or_insert((path, state));
    }

    /// The next queued directory link to follow.
    pub(crate) fn next(&mut self) -> Option<LinkedDir<T>> {
        let root = self.canonical_root.clone()?;
        while let Some(((hops, relative), (path, state))) = self.queue.pop_first() {
            let Ok(canonical) = fs::canonicalize(&path) else {
                continue;
            };
            if root.starts_with(&canonical) || fs::read_dir(&path).is_err() {
                continue;
            }
            if !self.enter(canonical.clone(), &relative) {
                continue;
            }
            return Some(LinkedDir {
                hops,
                relative,
                path,
                canonical,
                state,
            });
        }
        None
    }

    /// The canonical target of a file link, unless it cannot be resolved or
    /// lies in a blocked prefix.
    pub(crate) fn file_target(&self, path: &Path) -> Option<PathBuf> {
        self.canonical_root.as_ref()?;
        let canonical = fs::canonicalize(path).ok()?;
        (!self.is_blocked(&canonical)).then_some(canonical)
    }

    /// The logical path a canonical directory was scanned under.
    pub(crate) fn logical_dir(&self, canonical: &Path) -> Option<&str> {
        self.scanned.get(canonical).map(String::as_str)
    }

    fn is_blocked(&self, canonical: &Path) -> bool {
        self.blocked
            .iter()
            .any(|blocked| canonical.starts_with(blocked))
    }
}
