//! Git-stamped fast path for pending status (upstream #1829 / #1878).
//!
//! `status` reports the work the next whole-tree sync would do. The reference
//! answer is the full inventory: scan, stat every file, hash on a stat
//! mismatch. That costs a walk of the whole tree, so a full build or full sync
//! also records, under [`GIT_PENDING_KEY`], the commit it started at and every
//! path that may differ from that commit: the paths git reported dirty around
//! the build, rows whose stat moved while it ran, and paths a fresh scan and
//! the database disagree on. Incremental syncs add the paths they are handed.
//!
//! The fast path then asks git for the candidates — what changed between the
//! recorded commit and HEAD, plus what `git status` reports now — and
//! classifies only those, plus the recorded paths, exactly as the full
//! inventory would. Git supplies candidates, never the verdict. It declines,
//! leaving the full inventory to answer, whenever git might not see a change:
//!
//! - there is no record, or its scope fingerprint differs from now;
//! - the index was built through symlinks, or the repository has submodules;
//! - git is missing, slow (10 s), loud (64 MiB), or has no HEAD;
//! - the recorded commit is no longer in the repository;
//! - an untracked nested repository, a directory or symlink candidate, an
//!   `assume-unchanged` or `skip-worktree` entry, or an ignored path the scan
//!   would keep;
//! - git may call changed bytes clean: `core.autocrlf`, any gitattributes
//!   source, or a case-insensitive or Unicode-precomposing name match.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::{IndexPaths, source_file::is_source_file, source_file::read_source_file};
use codegraph_extract::ExtractOptions;
use codegraph_extract::engine::{Membership, scan_membership, scan_project};
use codegraph_store::{CURRENT_EXTRACTION_VERSION, Store};
use serde::{Deserialize, Serialize};

use crate::sync::{PendingChanges, modified_millis};

/// `project_metadata` key holding the git record (one JSON value, so it can
/// never be read half-written).
pub const GIT_PENDING_KEY: &str = "git_pending_state";

const RECORD_VERSION: u32 = 1;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_MAX_STDOUT: u64 = 64 * 1024 * 1024;

/// Which computation answered a pending-changes query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingSource {
    GitFastPath,
    FullInventory,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    v: u32,
    commit: String,
    /// Project-relative paths that are always re-classified.
    dirty: Vec<String>,
    scope: String,
    links: bool,
    submodules: bool,
}

/// A stable identity of everything that decides scan membership. A record
/// made under another scope (config, extension overrides, root `.gitignore`,
/// index root, extraction rules, binary) proves nothing about this one.
pub fn scope_fingerprint(root: &Path, options: &ExtractOptions) -> String {
    let gitignore = fs::read(root.join(".gitignore")).unwrap_or_default();
    let reserved =
        IndexPaths::reserved_index_roots(root, std::env::var("CODEGRAPH_DIR").ok().as_deref());
    let identity = serde_json::json!({
        "ignore_dirs": options.ignore_dirs,
        "ignore_paths": options.ignore_paths,
        "include": options.include,
        "exclude": options.exclude,
        "max_file_size": options.max_file_size,
        "extensions": options
            .extensions
            .entries()
            .map(|(extension, language)| (extension.to_string(), format!("{language:?}")))
            .collect::<Vec<_>>(),
        "gitignore": hash_content(&String::from_utf8_lossy(&gitignore)),
        "reserved": reserved
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        "extraction_version": CURRENT_EXTRACTION_VERSION,
        "version": env!("CARGO_PKG_VERSION"),
    });
    hash_content(&identity.to_string())
}

/// Run `git --no-optional-locks <args>` in `root`, with the ambient
/// repository-selection environment stripped. `None` on any failure, a
/// timeout, or output over the cap.
fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = std::process::Command::new(test_hooks::git_program());
    command.current_dir(root);
    for var in crate::git::GIT_REPO_SELECTION_ENV {
        command.env_remove(var);
    }
    command
        .arg("--no-optional-locks")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout
            .take(GIT_MAX_STDOUT + 1)
            .read_to_end(&mut out)
            .ok()
            .map(|_| out)
    });
    let deadline = Instant::now() + test_hooks::git_timeout(GIT_TIMEOUT);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()??;
    (status.success() && out.len() as u64 <= GIT_MAX_STDOUT).then_some(out)
}

/// HEAD and the project's prefix inside its work tree.
struct Repo {
    head: String,
    /// The project root relative to the work tree, with a trailing `/`, or
    /// empty at the top level.
    prefix: String,
    toplevel: String,
    /// The repository's common git directory (holding `info/attributes`).
    common_dir: std::path::PathBuf,
}

impl Repo {
    fn read(root: &Path) -> Option<Self> {
        let out = git(
            root,
            &[
                "rev-parse",
                "HEAD",
                "--show-prefix",
                "--show-toplevel",
                "--git-common-dir",
            ],
        )?;
        let text = String::from_utf8(out).ok()?;
        let mut lines = text.lines();
        let head = lines.next()?.trim().to_string();
        let prefix = lines.next()?.to_string();
        let toplevel = lines.next()?.to_string();
        // Printed relative to the working directory unless it lies elsewhere.
        let common_dir = root.join(lines.next()?);
        (!head.is_empty()).then_some(Self {
            head,
            prefix,
            toplevel,
            common_dir,
        })
    }

    /// A repo-root-relative git path as a project-relative one; `None` when it
    /// lies outside the project.
    fn rebase(&self, path: &str) -> Option<String> {
        path.strip_prefix(self.prefix.as_str())
            .filter(|rest| !rest.is_empty())
            .map(str::to_string)
    }
}

fn nul_fields(out: &[u8]) -> impl Iterator<Item = String> + '_ {
    out.split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
}

/// Project-relative paths `git status` reports. An untracked directory entry
/// (an embedded repository `-uall` cannot expand) keeps its trailing `/`.
fn status_paths(root: &Path, repo: &Repo) -> Option<BTreeSet<String>> {
    let out = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "-uall",
            "--ignore-submodules=none",
            "--",
            ".",
        ],
    )?;
    let mut paths = BTreeSet::new();
    for entry in nul_fields(&out) {
        let path = entry.get(3..)?;
        if let Some(relative) = repo.rebase(path) {
            paths.insert(relative);
        }
    }
    Some(paths)
}

/// Project-relative paths that differ between `commit` and HEAD.
fn committed_paths(root: &Path, repo: &Repo, commit: &str) -> Option<BTreeSet<String>> {
    let out = git(
        root,
        &[
            "diff",
            "--name-status",
            "--no-renames",
            "--no-relative",
            "-z",
            "--ignore-submodules=none",
            commit,
            "HEAD",
            "--",
            ".",
        ],
    )?;
    let fields = nul_fields(&out).collect::<Vec<_>>();
    let mut paths = BTreeSet::new();
    for pair in fields.chunks(2) {
        let [_, path] = pair else {
            return None;
        };
        if let Some(relative) = repo.rebase(path) {
            paths.insert(relative);
        }
    }
    Some(paths)
}

/// Whether git compares content after a conversion, or matches names in a
/// way, that can make it call a path clean although its bytes (or its exact
/// name) differ from what was indexed: `core.autocrlf`, any gitattributes
/// source (`text`, `eol`, `filter` such as LFS, `ident`, encodings), a
/// case-insensitive or Unicode-precomposing name match. `None` when git fails.
fn conversion_risk(root: &Path, repo: &Repo) -> Option<bool> {
    let config = git(root, &["config", "--list", "-z"])?;
    // `--list` prints every scope in precedence order (system, global, local,
    // worktree), so the last value of a key is the one git uses.
    let mut effective = std::collections::BTreeMap::new();
    for entry in nul_fields(&config) {
        let (key, value) = entry.split_once('\n').unwrap_or((entry.as_str(), ""));
        effective.insert(key.to_ascii_lowercase(), value.trim().to_ascii_lowercase());
    }
    for (key, value) in &effective {
        let truthy = value.is_empty() || matches!(value.as_str(), "true" | "yes" | "on" | "1");
        let falsy = matches!(value.as_str(), "false" | "no" | "off" | "0");
        let risky = match key.as_str() {
            "core.autocrlf" => !falsy,
            "core.ignorecase" | "core.precomposeunicode" => truthy,
            "core.attributesfile" => !value.is_empty(),
            _ => false,
        };
        if risky {
            return Some(true);
        }
    }
    // The system (`$(prefix)/etc/gitattributes`) and global (`core.attributesFile`,
    // else the XDG default) attributes files, exactly as git resolves them.
    // `GIT_ATTR_NOSYSTEM` switches the system file off, as it does for git.
    let system_disabled = std::env::var_os("GIT_ATTR_NOSYSTEM")
        .is_some_and(|value| !matches!(value.to_str(), Some("" | "0" | "false" | "no" | "off")));
    if !system_disabled && attributes_file_applies(root, "GIT_ATTR_SYSTEM") {
        return Some(true);
    }
    if attributes_file_applies(root, "GIT_ATTR_GLOBAL")
        || repo.common_dir.join("info").join("attributes").exists()
        || ancestor_attributes_exist(repo)
    {
        return Some(true);
    }
    let attributes = git(
        root,
        &[
            "ls-files",
            "--full-name",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ":(glob)**/.gitattributes",
        ],
    )?;
    Some(nul_fields(&attributes).next().is_some())
}

/// Whether a `.gitattributes` sits in a directory between the work-tree root
/// and the project root: git applies it to the project's files, but a pathspec
/// rooted at the project cannot list it. The project root itself and
/// everything below are covered by the `ls-files` query.
fn ancestor_attributes_exist(repo: &Repo) -> bool {
    let top = Path::new(&repo.toplevel);
    let segments = repo
        .prefix
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    (0..segments.len()).any(|depth| {
        segments[..depth]
            .iter()
            .fold(top.to_path_buf(), |dir, segment| dir.join(segment))
            .join(".gitattributes")
            .exists()
    })
}

/// Whether the attributes file git names for `var` (`GIT_ATTR_SYSTEM`,
/// `GIT_ATTR_GLOBAL`, git 2.42+) exists. A git that cannot say counts as one
/// that applies it.
fn attributes_file_applies(root: &Path, var: &str) -> bool {
    let Some(out) = git(root, &["var", var]) else {
        return true;
    };
    let text = String::from_utf8_lossy(&out);
    let path = text.trim_end_matches(['\r', '\n']);
    !path.is_empty() && Path::new(path).exists()
}

/// What the index file says that `git status` cannot: a gitlink (submodule),
/// or an entry marked `assume-unchanged` or `skip-worktree`, whose working-tree
/// changes git hides.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct IndexFlags {
    submodules: bool,
    hidden_changes: bool,
}

fn index_flags(root: &Path, repo: &Repo) -> Option<IndexFlags> {
    let out = git(
        root,
        &["ls-files", "-s", "-v", "-z", "--full-name", "--", "."],
    )?;
    let mut flags = IndexFlags {
        submodules: Path::new(&repo.toplevel).join(".gitmodules").exists(),
        hidden_changes: false,
    };
    for entry in nul_fields(&out) {
        // `<tag> <mode> <object> <stage>\t<path>`.
        let mut fields = entry.split(' ');
        let tag = fields.next()?;
        let mode = fields.next()?;
        flags.submodules |= mode == "160000";
        flags.hidden_changes |= tag == "S" || tag.chars().all(|c| c.is_ascii_lowercase());
    }
    Some(flags)
}

/// Whether git ignores a path the scan would keep: a working-tree change
/// there is invisible to `git status`. `None` when git fails.
fn ignores_scanned_path(root: &Path, repo: &Repo, options: &ExtractOptions) -> Option<bool> {
    let out = git(
        root,
        &[
            "ls-files",
            "--full-name",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "--",
            ".",
        ],
    )?;
    for entry in nul_fields(&out) {
        let Some(relative) = repo.rebase(&entry) else {
            continue;
        };
        // An attributes file applies whether or not git ignores it.
        if Path::new(&relative).file_name() == Some(std::ffi::OsStr::new(".gitattributes")) {
            return Some(true);
        }
        let membership = match relative.strip_suffix('/') {
            Some(dir) => scan_membership(root, options, dir, true),
            None => scan_membership(root, options, &relative, false),
        };
        if membership != Membership::Rejected {
            return Some(true);
        }
    }
    Some(false)
}

/// The git state a full build or full sync starts from, captured before it
/// scans or reads a file.
pub struct GitIndexCapture {
    repo: Repo,
    status: BTreeSet<String>,
    submodules: bool,
    scope: String,
}

impl GitIndexCapture {
    /// `None` outside a git work tree, without a HEAD, or when git fails;
    /// such a build then leaves no record and the fast path stays declined.
    pub fn begin(root: &Path, options: &ExtractOptions) -> Option<Self> {
        let scope = scope_fingerprint(root, options);
        let repo = Repo::read(root)?;
        let status = status_paths(root, &repo)?;
        let flags = index_flags(root, &repo)?;
        let capture = Self {
            repo,
            status,
            submodules: flags.submodules,
            scope,
        };
        test_hooks::after_git_begin(root);
        Some(capture)
    }

    /// Write the record after every row is written. A scope that changed
    /// while the build ran, or any git failure, removes the record instead.
    pub fn record(
        self,
        store: &Store,
        root: &Path,
        options: &ExtractOptions,
        links_followed: bool,
    ) -> Result<()> {
        test_hooks::before_git_record(root);
        let after = (scope_fingerprint(root, options) == self.scope)
            .then(|| status_paths(root, &self.repo))
            .flatten();
        let Some(after) = after else {
            store.delete_project_metadata(GIT_PENDING_KEY)?;
            return Ok(());
        };
        let mut dirty = self.status;
        dirty.extend(after);
        // A row whose file changed after it was read (and possibly back), and
        // any path a fresh scan and the database disagree on (a file the first
        // scan missed while it was briefly gone or excluded).
        let stored = store.all_files()?;
        let mut stored_paths = BTreeSet::new();
        for file in stored {
            let unchanged = fs::metadata(root.join(&file.path)).is_ok_and(|metadata| {
                file.size == metadata.len() as i64 && file.modified_at == modified_millis(&metadata)
            });
            if !unchanged {
                dirty.insert(file.path.clone());
            }
            stored_paths.insert(file.path);
        }
        let fresh = scan_project(root, options)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        dirty.extend(fresh.symmetric_difference(&stored_paths).cloned());
        let record = Record {
            v: RECORD_VERSION,
            commit: self.repo.head,
            dirty: dirty.into_iter().collect(),
            scope: self.scope,
            links: links_followed,
            submodules: self.submodules,
        };
        store.set_project_metadata(GIT_PENDING_KEY, &serde_json::to_string(&record)?)?;
        Ok(())
    }
}

/// Forget the record before a full sync rewrites rows, so an interrupted sync
/// leaves none rather than a stale one.
pub(crate) fn forget(store: &Store) -> Result<()> {
    store.delete_project_metadata(GIT_PENDING_KEY)?;
    Ok(())
}

/// Before an incremental sync writes a row: keep the record only if it was made
/// under the current scope, with every requested path added to `dirty`.
pub(crate) fn extend_for_incremental(
    store: &Store,
    root: &Path,
    options: &ExtractOptions,
    requested: &BTreeSet<String>,
) -> Result<()> {
    let record = store
        .get_project_metadata(GIT_PENDING_KEY)?
        .and_then(|value| serde_json::from_str::<Record>(&value).ok());
    match record {
        Some(mut record)
            if record.v == RECORD_VERSION && record.scope == scope_fingerprint(root, options) =>
        {
            let mut dirty = record.dirty.into_iter().collect::<BTreeSet<_>>();
            dirty.extend(requested.iter().cloned());
            record.dirty = dirty.into_iter().collect();
            store.set_project_metadata(GIT_PENDING_KEY, &serde_json::to_string(&record)?)?;
        }
        _ => forget(store)?,
    }
    test_hooks::after_incremental_extend(root);
    Ok(())
}

/// The pending changes from git's candidates, or `None` to defer to the full
/// inventory.
pub(crate) fn git_fast_pending(
    root: &Path,
    store: &Store,
    options: &ExtractOptions,
) -> Result<Option<PendingChanges>> {
    let Some(record) = store
        .get_project_metadata(GIT_PENDING_KEY)?
        .and_then(|value| serde_json::from_str::<Record>(&value).ok())
    else {
        return Ok(None);
    };
    if record.v != RECORD_VERSION
        || record.links
        || record.submodules
        || record.scope != scope_fingerprint(root, options)
    {
        return Ok(None);
    }
    let Some(repo) = Repo::read(root) else {
        return Ok(None);
    };
    let exists = format!("{}^{{commit}}", record.commit);
    if git(root, &["cat-file", "-e", &exists]).is_none() {
        return Ok(None);
    }
    let Some(flags) = index_flags(root, &repo) else {
        return Ok(None);
    };
    if flags.submodules || flags.hidden_changes {
        return Ok(None);
    }
    if conversion_risk(root, &repo) != Some(false) {
        return Ok(None);
    }
    let (Some(status), Some(committed)) = (
        status_paths(root, &repo),
        committed_paths(root, &repo, &record.commit),
    ) else {
        return Ok(None);
    };
    if status.iter().any(|path| path.ends_with('/')) {
        return Ok(None);
    }
    if ignores_scanned_path(root, &repo, options) != Some(false) {
        return Ok(None);
    }

    let mut candidates = status;
    candidates.extend(committed);
    candidates.extend(record.dirty);
    let mut pending = PendingChanges::default();
    for relative in candidates {
        let full = root.join(&relative);
        if fs::symlink_metadata(&full)
            .is_ok_and(|metadata| metadata.file_type().is_symlink() || metadata.is_dir())
        {
            return Ok(None);
        }
        let membership = scan_membership(root, options, &relative, false);
        if membership == Membership::Unknown {
            return Ok(None);
        }
        let stored = store.file_by_path(&relative)?;
        match (stored, membership) {
            (None, Membership::Admitted) => {
                // An untracked video clip named `.ts` is not source sync would add.
                if is_source_file(&full, &relative, options.max_file_size)? {
                    pending.added.push(relative);
                }
            }
            (None, _) => {}
            (Some(_), _) if membership != Membership::Admitted => pending.removed.push(relative),
            (Some(stored), _) => {
                let metadata = fs::metadata(&full)?;
                if stored.size == metadata.len() as i64
                    && stored.modified_at == modified_millis(&metadata)
                {
                    continue;
                }
                let (_, source) = read_source_file(&full, &relative, options.max_file_size)?;
                match source.hash_input() {
                    // A tracked file that became a video clip leaves the index.
                    None => pending.removed.push(relative),
                    Some(input) if stored.content_hash != hash_content(&input) => {
                        pending.modified.push(relative);
                    }
                    Some(_) => {}
                }
            }
        }
    }
    pending.added.sort();
    pending.modified.sort();
    pending.removed.sort();
    Ok(Some(pending))
}

/// Seams for tests that race or break the writers: hooks right after
/// [`GitIndexCapture::begin`] captures git (before the build scans), right
/// before [`GitIndexCapture::record`] (after every file was read), and right
/// after an incremental sync updated the record (before any row moves). A
/// panicking hook must not poison the next test, so locks tolerate poison.
pub mod test_hooks {
    use std::path::Path;

    #[cfg(feature = "test-hooks")]
    type Hook = Box<dyn Fn(&Path) + Send + Sync>;

    #[cfg(feature = "test-hooks")]
    static AFTER_GIT_BEGIN: std::sync::Mutex<Option<Hook>> = std::sync::Mutex::new(None);
    #[cfg(feature = "test-hooks")]
    static BEFORE_GIT_RECORD: std::sync::Mutex<Option<Hook>> = std::sync::Mutex::new(None);
    #[cfg(feature = "test-hooks")]
    static AFTER_INCREMENTAL_EXTEND: std::sync::Mutex<Option<Hook>> = std::sync::Mutex::new(None);

    /// Runs once an incremental sync has extended (or forgotten) the record,
    /// before it writes a row.
    #[cfg(feature = "test-hooks")]
    pub fn set_after_incremental_extend(hook: Option<Hook>) {
        *AFTER_INCREMENTAL_EXTEND
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hook;
    }

    pub(super) fn after_incremental_extend(root: &Path) {
        #[cfg(feature = "test-hooks")]
        if let Some(hook) = AFTER_INCREMENTAL_EXTEND
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            hook(root);
        }
        let _ = root;
    }

    #[cfg(feature = "test-hooks")]
    pub fn set_after_git_begin(hook: Option<Hook>) {
        *AFTER_GIT_BEGIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hook;
    }

    #[cfg(feature = "test-hooks")]
    pub fn set_before_git_record(hook: Option<Hook>) {
        *BEFORE_GIT_RECORD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hook;
    }

    #[cfg(feature = "test-hooks")]
    static GIT_PROGRAM: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
    #[cfg(feature = "test-hooks")]
    static GIT_TIMEOUT: std::sync::Mutex<Option<std::time::Duration>> = std::sync::Mutex::new(None);

    /// Run this program instead of `git` (a stand-in that hangs, say).
    #[cfg(feature = "test-hooks")]
    pub fn set_git_program(program: Option<std::path::PathBuf>) {
        *GIT_PROGRAM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = program;
    }

    /// Give up on git after this long instead of the default.
    #[cfg(feature = "test-hooks")]
    pub fn set_git_timeout(timeout: Option<std::time::Duration>) {
        *GIT_TIMEOUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = timeout;
    }

    pub(super) fn git_program() -> std::ffi::OsString {
        #[cfg(feature = "test-hooks")]
        if let Some(program) = GIT_PROGRAM
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            return program.clone().into_os_string();
        }
        "git".into()
    }

    pub(super) fn git_timeout(default: std::time::Duration) -> std::time::Duration {
        #[cfg(feature = "test-hooks")]
        if let Some(timeout) = *GIT_TIMEOUT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return timeout;
        }
        default
    }

    pub(super) fn after_git_begin(root: &Path) {
        #[cfg(feature = "test-hooks")]
        if let Some(hook) = AFTER_GIT_BEGIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            hook(root);
        }
        let _ = root;
    }

    pub(super) fn before_git_record(root: &Path) {
        #[cfg(feature = "test-hooks")]
        if let Some(hook) = BEFORE_GIT_RECORD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            hook(root);
        }
        let _ = root;
    }
}

#[cfg(all(test, unix, feature = "test-hooks"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A stand-in git that answers only what `conversion_risk` asks.
    fn fake_git(dir: &Path, system_attributes: &Path) -> std::path::PathBuf {
        let script = dir.join("git");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do\n  case \"$a\" in\n    GIT_ATTR_SYSTEM) echo '{}'; exit 0;;\n    GIT_ATTR_GLOBAL) echo '{}'; exit 0;;\n  esac\ndone\nexit 0\n",
                system_attributes.display(),
                dir.join("no-global-attributes").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    #[test]
    fn a_system_attributes_file_is_a_conversion_risk() {
        let dir = std::env::temp_dir().join(format!("cg_fake_git_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let system = dir.join("etc-gitattributes");
        let repo = Repo {
            head: "0".repeat(40),
            prefix: String::new(),
            toplevel: dir.to_string_lossy().into_owned(),
            common_dir: dir.join("dot-git"),
        };
        test_hooks::set_git_program(Some(fake_git(&dir, &system)));
        let without = conversion_risk(&dir, &repo);
        fs::write(&system, "* text=auto\n").unwrap();
        let with = conversion_risk(&dir, &repo);
        test_hooks::set_git_program(None);
        fs::remove_dir_all(&dir).ok();
        assert_eq!(without, Some(false), "no attributes anywhere");
        assert_eq!(
            with,
            Some(true),
            "git reads $(prefix)/etc/gitattributes too"
        );
    }
}
