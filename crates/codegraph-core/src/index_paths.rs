//! Single source of truth for CodeGraph's on-disk index paths.
//!
//! `IndexPaths` is the ONE path authority shared by CLI, store, daemon, watch,
//! and MCP. Production code must NOT reconstruct `.codegraph*` paths
//! independently. For a given project and optional `CODEGRAPH_DIR` override it
//! computes:
//!
//! - the physical [`project_identity`](IndexPaths::project_identity) (full
//!   lowercase SHA-256 of a versioned binary payload of the OS filesystem
//!   identifiers — `st_dev`/`st_ino` on Unix, volume serial + 128-bit file id on
//!   Windows). Unsupported filesystems fail closed; there is NO lexical-spelling
//!   fallback;
//! - the [`current_root`](IndexPaths::current_root) — `<project>/.codegraph` by
//!   default, or `<project>/<CODEGRAPH_DIR>` when the override is a safe single
//!   directory name;
//! - every current-root-owned artifact path (DB, permanent lock, the two fixed
//!   state slots, the `uninitialized` tombstone, `config.toml`, `codegraph.json`,
//!   and the daemon pid/log/socket identities).
//!
//! Path identity is PHYSICAL, not lexical: the project and existing index root
//! are canonicalized; a not-yet-created root canonicalizes its nearest
//! existing ancestor and appends the normalized remainder. Empty/root/dot/parent
//! aliases, symlink (or Windows reparse-point) components below the canonical
//! ancestor, absolute overrides, and overrides containing path separators all
//! fail closed with typed, stable diagnostics.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Default per-project index-root directory name. This is the ONE place the
/// `.codegraph` literal is defined; every production consumer derives its root
/// from [`IndexPaths::resolve`].
pub const DEFAULT_CURRENT_DIR: &str = ".codegraph";

/// Version byte of the binary payload hashed into the project identity. Bump
/// only with a deliberate, reviewed identity-format change.
const IDENTITY_PAYLOAD_VERSION: u8 = 1;
/// Magic prefix so the identity payload can never collide with an unrelated
/// SHA-256 preimage.
const IDENTITY_MAGIC: &[u8; 5] = b"cgpid";
#[cfg(unix)]
const IDENTITY_PLATFORM_UNIX: u8 = 1;
#[cfg(windows)]
const IDENTITY_PLATFORM_WINDOWS: u8 = 2;

/// Errors from [`IndexPaths::resolve`]. All variants carry stable, deterministic
/// diagnostics; there is never a fallback from physical identity to path
/// spelling on unsupported filesystems.
#[derive(Debug, Error)]
pub enum IndexPathsError {
    /// The project path could not be canonicalized (missing or inaccessible).
    #[error("project path is not accessible: {path} ({source})")]
    ProjectInaccessible {
        path: PathBuf,
        source: std::io::Error,
    },

    /// The filesystem does not expose the stable physical identifiers required to
    /// derive a project identity. Fail closed — never fall back to path spelling.
    #[error(
        "cannot derive a physical project identity for {path}: \
         the filesystem does not expose stable identifiers ({detail})"
    )]
    UnsupportedFilesystem { path: PathBuf, detail: String },

    /// A path component could not be canonicalized (an existing ancestor became
    /// inaccessible mid-resolution).
    #[error("cannot canonicalize index path component {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },

    /// An empty/filesystem-root/`.`/`..` alias was supplied or derived.
    #[error("refusing an unsafe index root {path}: {reason}")]
    RootAlias { path: PathBuf, reason: String },

    /// A symlink (or Windows reparse-point) component was found below the
    /// canonical ancestor of an index root. Fail closed rather than follow it.
    #[error(
        "refusing index root: {path} is (or descends through) a symlink / \
         reparse point below its canonical ancestor"
    )]
    SymlinkComponent { path: PathBuf },
}

/// The resolved, physical, fail-closed set of index paths for one project.
///
/// Construct via [`IndexPaths::resolve`]. Every artifact path is derived from a
/// single validated [`current_root`](IndexPaths::current_root); callers consume
/// these accessors instead of rebuilding `.codegraph` strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexPaths {
    project: PathBuf,
    project_identity: String,
    current_root: PathBuf,
}

impl IndexPaths {
    /// Resolve the physical index paths for `project`, honoring an optional
    /// `CODEGRAPH_DIR` override (one project-local directory name). Production callers pass
    /// `std::env::var("CODEGRAPH_DIR").ok().as_deref()`.
    ///
    /// `project` MUST exist (it is canonicalized to derive the physical
    /// identity). Fails closed on unsafe names, symlink aliases, and unsupported
    /// filesystems.
    pub fn resolve(project: &Path, codegraph_dir: Option<&str>) -> Result<Self, IndexPathsError> {
        let project =
            project
                .canonicalize()
                .map_err(|source| IndexPathsError::ProjectInaccessible {
                    path: project.to_path_buf(),
                    source,
                })?;

        let project_identity = physical_identity(&project)?;

        let directory_name = configured_directory_name(codegraph_dir)?;
        let candidate = lexical_normalize(&project.join(directory_name));
        reject_project_alias(&project, &candidate, directory_name)?;
        let current_root = physical_normalize(&project, &candidate)?;

        Ok(Self {
            project,
            project_identity,
            current_root,
        })
    }

    /// The EXACT resolved physical index-root PATHS the scanner/watcher must
    /// exclude before descending: the default root plus the resolved current
    /// root, as full normalized paths (not basenames). The scanner
    /// compares each candidate directory path against this set, so a nested
    /// configured root such as `<project>/cache/index` is excluded at its true
    /// depth while an unrelated directory that merely shares a basename is not.
    ///
    /// In-project roots are re-anchored onto the caller's `project` spelling
    /// (`resolve` canonicalizes) so scanner candidates compare directly; roots
    /// outside the project keep their absolute form. Always includes
    /// `<project>/.codegraph`, even when an override is active, so an index from
    /// another environment is never scanned as source. On a `resolve` failure it
    /// degrades to that safe default path rather than reconstructing an invalid
    /// configured-root path; the fail-closed DB
    /// contract is enforced separately at the CLI/MCP/watch boundary by
    /// [`IndexPaths::resolve`].
    pub fn reserved_index_roots(
        project: &Path,
        codegraph_dir: Option<&str>,
    ) -> std::collections::BTreeSet<PathBuf> {
        let mut roots = std::collections::BTreeSet::new();
        match Self::resolve(project, codegraph_dir) {
            Ok(paths) => {
                let canonical = paths.project();
                let reanchor = |root: &Path| -> PathBuf {
                    match root.strip_prefix(canonical) {
                        Ok(rel) => project.join(rel),
                        Err(_) => root.to_path_buf(),
                    }
                };
                roots.insert(project.join(DEFAULT_CURRENT_DIR));
                roots.insert(reanchor(paths.current_root()));
            }
            Err(_) => {
                roots.insert(project.join(DEFAULT_CURRENT_DIR));
            }
        }
        roots
    }

    /// Canonical project root.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// Full lowercase SHA-256 (64 hex chars) of the versioned physical-identity
    /// payload. The value seeds state ownership.
    pub fn project_identity(&self) -> &str {
        &self.project_identity
    }

    /// The current index root (`<project>/.codegraph` by default, or the safe
    /// project-local `CODEGRAPH_DIR` override).
    pub fn current_root(&self) -> &Path {
        &self.current_root
    }

    /// `<current_root>/codegraph.db`.
    pub fn current_db(&self) -> PathBuf {
        self.current_root.join("codegraph.db")
    }

    /// `<current_root>/index.lock` — the permanent lock file (never truncated or
    /// deleted by normal operation).
    pub fn permanent_lock(&self) -> PathBuf {
        self.current_root.join("index.lock")
    }

    /// The two fixed state-slot files `index-state.{0,1}.json`, in slot order.
    pub fn state_slots(&self) -> [PathBuf; 2] {
        [
            self.current_root.join("index-state.0.json"),
            self.current_root.join("index-state.1.json"),
        ]
    }

    /// `<current_root>/uninitialized` — the interrupted-uninit tombstone marker.
    pub fn tombstone(&self) -> PathBuf {
        self.current_root.join("uninitialized")
    }

    /// `<current_root>/config.toml` — the project-scoped default config.
    pub fn config_toml(&self) -> PathBuf {
        self.current_root.join("config.toml")
    }

    /// `<current_root>/codegraph.json` — the project-scoped custom-extension /
    /// Godot DSL config.
    pub fn extension_config(&self) -> PathBuf {
        self.current_root.join("codegraph.json")
    }

    /// `<current_root>/daemon.pid`.
    pub fn daemon_pid(&self) -> PathBuf {
        self.current_root.join("daemon.pid")
    }

    /// `<current_root>/daemon.log`.
    pub fn daemon_log(&self) -> PathBuf {
        self.current_root.join("daemon.log")
    }

    /// `<current_root>/daemon.sock` — the in-root daemon socket identity. (The
    /// POSIX tmp-socket fallback and Windows namespaced-pipe forms are owned by
    /// the daemon lifecycle layer.)
    pub fn daemon_socket(&self) -> PathBuf {
        self.current_root.join("daemon.sock")
    }
}

/// Lexically normalize an absolute path: drop `.` components and fold each `..`
/// into the preceding normal component. Never touches the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn configured_directory_name(codegraph_dir: Option<&str>) -> Result<&str, IndexPathsError> {
    let Some(raw_value) = codegraph_dir else {
        return Ok(DEFAULT_CURRENT_DIR);
    };
    let value = raw_value.trim();
    let invalid = value.is_empty()
        || value == "."
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || Path::new(value).is_absolute()
        || !matches!(
            Path::new(value).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        );
    if invalid {
        return Err(IndexPathsError::RootAlias {
            path: PathBuf::from(raw_value),
            reason: format!(
                "CODEGRAPH_DIR={raw_value:?} must be one non-empty project-local directory name"
            ),
        });
    }
    Ok(value)
}

/// Reject a configured root that aliases the project itself, an ancestor of the
/// project, or the filesystem root — the `.`/`..`/root alias cases.
fn reject_project_alias(
    project: &Path,
    cli_root: &Path,
    raw_value: &str,
) -> Result<(), IndexPathsError> {
    if cli_root.parent().is_none() {
        return Err(IndexPathsError::RootAlias {
            path: cli_root.to_path_buf(),
            reason: format!("CODEGRAPH_DIR={raw_value:?} resolves to the filesystem root"),
        });
    }
    if cli_root == project {
        return Err(IndexPathsError::RootAlias {
            path: cli_root.to_path_buf(),
            reason: format!("CODEGRAPH_DIR={raw_value:?} resolves to the project root itself"),
        });
    }
    if project.starts_with(cli_root) {
        return Err(IndexPathsError::RootAlias {
            path: cli_root.to_path_buf(),
            reason: format!("CODEGRAPH_DIR={raw_value:?} resolves to an ancestor of the project"),
        });
    }
    Ok(())
}

/// The deepest ancestor of `path` (inclusive) that currently exists, if any.
fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(path);
    while let Some(candidate) = current {
        if candidate.symlink_metadata().is_ok() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

/// Whether `path` currently exists AND is a symlink or reparse point.
///
/// On Windows `FileType::is_symlink` misses directory junctions and other
/// reparse points, so the raw `FILE_ATTRIBUTE_REPARSE_POINT` attribute bit is
/// checked as well; those are the alias forms an index root must refuse.
fn is_symlink(path: &Path) -> bool {
    let Ok(meta) = path.symlink_metadata() else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Physically normalize an index root: reject empty/root aliases, reject any
/// symlink component below the canonical ancestor, and return the path anchored
/// on the canonicalized nearest-existing ancestor.
fn physical_normalize(project: &Path, target: &Path) -> Result<PathBuf, IndexPathsError> {
    let target = lexical_normalize(target);
    if target.parent().is_none() {
        return Err(IndexPathsError::RootAlias {
            path: target.clone(),
            reason: "path is the filesystem root".to_string(),
        });
    }

    let existing =
        nearest_existing_ancestor(&target).ok_or_else(|| IndexPathsError::RootAlias {
            path: target.clone(),
            reason: "no existing ancestor directory".to_string(),
        })?;

    // `project` is already canonical (no symlink components). For a target under
    // the project, probe only the tail below it and reject symlink components —
    // the components above are the trusted physical base.
    if let Ok(tail) = existing.strip_prefix(project) {
        let mut probe = project.to_path_buf();
        for component in tail.components() {
            probe.push(component);
            if is_symlink(&probe) {
                return Err(IndexPathsError::SymlinkComponent { path: probe });
            }
        }
        let remainder = target
            .strip_prefix(&existing)
            .expect("existing is an ancestor of target");
        return Ok(existing.join(remainder));
    }

    // Target outside the project (an absolute CODEGRAPH_DIR or its sibling):
    // reject an alias in ANY existing component, not just the last one.
    // `<base>/link/child` where `link` is a symlink/junction reaches an ordinary
    // `child` directory through the alias, so checking only `existing` would
    // silently follow it. Walk every prefix from the filesystem root down to
    // `existing` and reject the first symlink/reparse component.
    let mut probe = PathBuf::new();
    for component in existing.components() {
        probe.push(component);
        if is_symlink(&probe) {
            return Err(IndexPathsError::SymlinkComponent { path: probe });
        }
    }
    let base = existing
        .canonicalize()
        .map_err(|source| IndexPathsError::Canonicalize {
            path: existing.clone(),
            source,
        })?;
    let remainder = target
        .strip_prefix(&existing)
        .expect("existing is an ancestor of target");
    Ok(base.join(remainder))
}

/// Compute the full lowercase SHA-256 of the versioned physical-identity payload
/// for a canonical project path. Fails closed on unsupported filesystems.
fn physical_identity(project: &Path) -> Result<String, IndexPathsError> {
    let payload = identity_payload(project)?;
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    Ok(hex_lower(&hasher.finalize()))
}

#[cfg(unix)]
fn identity_payload(project: &Path) -> Result<Vec<u8>, IndexPathsError> {
    use std::os::unix::fs::MetadataExt as _;

    // `st_dev` + `st_ino` uniquely identify a filesystem object on POSIX; std
    // exposes them without an external crate. A path that cannot be stat'd fails
    // closed rather than falling back to spelling.
    let meta = project
        .metadata()
        .map_err(|source| IndexPathsError::UnsupportedFilesystem {
            path: project.to_path_buf(),
            detail: format!("stat failed: {source}"),
        })?;
    let dev = meta.dev();
    let ino = meta.ino();

    let mut payload = Vec::with_capacity(IDENTITY_MAGIC.len() + 2 + 16);
    payload.extend_from_slice(IDENTITY_MAGIC);
    payload.push(IDENTITY_PAYLOAD_VERSION);
    payload.push(IDENTITY_PLATFORM_UNIX);
    payload.extend_from_slice(&dev.to_le_bytes());
    payload.extend_from_slice(&ino.to_le_bytes());
    Ok(payload)
}

#[cfg(windows)]
fn identity_payload(project: &Path) -> Result<Vec<u8>, IndexPathsError> {
    // The volume serial number + 128-bit file id from
    // GetFileInformationByHandleEx(FileIdInfo) are the stable physical
    // identifiers on Windows (the 64-bit BY_HANDLE index is insufficient on
    // ReFS). Raw kernel32 FFI avoids adding a dependency; the whole block is
    // `cfg(windows)` and never compiled on Unix CI.
    let (volume_serial, file_id) = windows_identity::file_id_info(project)?;

    let mut payload = Vec::with_capacity(IDENTITY_MAGIC.len() + 2 + 8 + 16);
    payload.extend_from_slice(IDENTITY_MAGIC);
    payload.push(IDENTITY_PAYLOAD_VERSION);
    payload.push(IDENTITY_PLATFORM_WINDOWS);
    payload.extend_from_slice(&volume_serial.to_le_bytes());
    payload.extend_from_slice(&file_id);
    Ok(payload)
}

#[cfg(not(any(unix, windows)))]
fn identity_payload(project: &Path) -> Result<Vec<u8>, IndexPathsError> {
    // Fail closed: no stable physical identifier available. NEVER fall back to
    // path spelling.
    Err(IndexPathsError::UnsupportedFilesystem {
        path: project.to_path_buf(),
        detail: "platform exposes no stable filesystem object identifier".to_string(),
    })
}

#[cfg(windows)]
mod windows_identity {
    //! Minimal raw kernel32 FFI for the physical file identity. Compiled only on
    //! Windows; adds no crate dependency.

    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;

    use super::IndexPathsError;

    // FILE_INFO_BY_HANDLE_CLASS::FileIdInfo
    const FILE_ID_INFO_CLASS: i32 = 18;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const OPEN_EXISTING: u32 = 3;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    struct FileIdInfo {
        volume_serial_number: u64,
        file_id: [u8; 16],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut core::ffi::c_void,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: isize,
        ) -> isize;
        fn GetFileInformationByHandleEx(
            h_file: isize,
            file_information_class: i32,
            lp_file_information: *mut core::ffi::c_void,
            dw_buffer_size: u32,
        ) -> i32;
        fn CloseHandle(h_object: isize) -> i32;
    }

    /// Return `(volume_serial_number, 128-bit file id)` for `path`, failing
    /// closed on any filesystem that does not support the query.
    pub(super) fn file_id_info(path: &Path) -> Result<(u64, [u8; 16]), IndexPathsError> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);

        // SAFETY: `wide` is a NUL-terminated UTF-16 path; all other pointers are
        // null/valid and the handle is closed on every path below.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                core::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(IndexPathsError::UnsupportedFilesystem {
                path: path.to_path_buf(),
                detail: format!("CreateFileW failed: {}", std::io::Error::last_os_error()),
            });
        }

        let mut info = FileIdInfo {
            volume_serial_number: 0,
            file_id: [0u8; 16],
        };
        // SAFETY: `handle` is valid; `info` is a correctly sized FILE_ID_INFO.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FILE_ID_INFO_CLASS,
                (&mut info as *mut FileIdInfo).cast(),
                core::mem::size_of::<FileIdInfo>() as u32,
            )
        };
        // Capture the query's OS error BEFORE CloseHandle, which itself sets the
        // thread-local last-error and would otherwise mask the real failure.
        let query_err = (ok == 0).then(std::io::Error::last_os_error);
        // SAFETY: close the handle exactly once regardless of the query result.
        unsafe {
            CloseHandle(handle);
        }

        if let Some(err) = query_err {
            return Err(IndexPathsError::UnsupportedFilesystem {
                path: path.to_path_buf(),
                detail: format!("GetFileInformationByHandleEx(FileIdInfo) failed: {err}"),
            });
        }
        Ok((info.volume_serial_number, info.file_id))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-indexpaths-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Canonicalize so expectations compare against the same physical base
        // `resolve` uses (macOS `/tmp` is a symlink to `/private/tmp`).
        dir.canonicalize().unwrap()
    }

    #[test]
    fn default_current_root_is_codegraph() {
        let project = temp_dir("default");
        let paths = IndexPaths::resolve(&project, None).expect("resolve default");

        assert_eq!(paths.current_root(), project.join(".codegraph"));
        assert_eq!(
            paths.current_db(),
            project.join(".codegraph").join("codegraph.db")
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn every_derived_artifact_lives_under_current_root() {
        let project = temp_dir("artifacts");
        let paths = IndexPaths::resolve(&project, None).expect("resolve");
        let root = project.join(".codegraph");

        assert_eq!(paths.current_root(), root);
        assert_eq!(paths.current_db(), root.join("codegraph.db"));
        assert_eq!(paths.permanent_lock(), root.join("index.lock"));
        assert_eq!(
            paths.state_slots(),
            [
                root.join("index-state.0.json"),
                root.join("index-state.1.json"),
            ]
        );
        assert_eq!(paths.tombstone(), root.join("uninitialized"));
        assert_eq!(paths.config_toml(), root.join("config.toml"));
        assert_eq!(paths.extension_config(), root.join("codegraph.json"));
        assert_eq!(paths.daemon_pid(), root.join("daemon.pid"));
        assert_eq!(paths.daemon_log(), root.join("daemon.log"));
        assert_eq!(paths.daemon_socket(), root.join("daemon.sock"));

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn project_identity_is_full_lowercase_sha256() {
        let project = temp_dir("identity");
        let paths = IndexPaths::resolve(&project, None).expect("resolve");
        let id = paths.project_identity();

        assert_eq!(id.len(), 64, "identity is a full SHA-256 hex string");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "identity is lowercase hex: {id}"
        );

        // Deterministic: a second resolve of the same physical project agrees.
        let again = IndexPaths::resolve(&project, None).expect("resolve again");
        assert_eq!(again.project_identity(), id);

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn codegraph_dir_is_plain_project_local_root() {
        let project = temp_dir("rel-dir");
        let paths =
            IndexPaths::resolve(&project, Some(".codegraph-win")).expect("resolve override");

        assert_eq!(paths.current_root(), project.join(".codegraph-win"));
        assert_eq!(
            paths.current_db(),
            project.join(".codegraph-win").join("codegraph.db")
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn absolute_codegraph_dir_is_rejected() {
        let project = temp_dir("abs-proj");
        let cache = temp_dir("abs-cache");
        let configured = cache.join("cg");
        let error = IndexPaths::resolve(&project, Some(configured.to_str().unwrap()))
            .expect_err("absolute override must fail closed");
        assert!(
            matches!(error, IndexPathsError::RootAlias { .. }),
            "{error:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn configured_directory_name_stays_local_to_each_project() {
        let project_a = temp_dir("collide-a");
        let project_b = temp_dir("collide-b");

        let a = IndexPaths::resolve(&project_a, Some("cache")).expect("a");
        let b = IndexPaths::resolve(&project_b, Some("cache")).expect("b");

        assert_eq!(a.current_root(), project_a.join("cache"));
        assert_eq!(b.current_root(), project_b.join("cache"));
        assert_ne!(a.current_root(), b.current_root());

        let _ = std::fs::remove_dir_all(&project_a);
        let _ = std::fs::remove_dir_all(&project_b);
    }

    #[test]
    fn path_separators_and_parent_traversal_are_rejected() {
        let base = temp_dir("escape-base");
        for invalid in ["../shared", "cache/index", "cache\\index", "name..other"] {
            let error = IndexPaths::resolve(&base, Some(invalid))
                .expect_err("non-plain override must fail closed");
            assert!(
                matches!(error, IndexPathsError::RootAlias { .. }),
                "{invalid}: {error:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rejects_nonexistent_project() {
        let missing = std::env::temp_dir().join("cg-indexpaths-does-not-exist-xyz");
        let err = IndexPaths::resolve(&missing, None).expect_err("missing project fails closed");
        assert!(matches!(err, IndexPathsError::ProjectInaccessible { .. }));
    }

    #[test]
    fn rejects_root_dot_and_parent_codegraph_dir_aliases() {
        let project = temp_dir("aliases");

        // A configured root resolving to the project root itself.
        let dot = IndexPaths::resolve(&project, Some(".")).expect_err("`.` must fail closed");
        assert!(matches!(dot, IndexPathsError::RootAlias { .. }), "{dot:?}");

        // A configured root resolving to an ancestor of the project.
        let parent = IndexPaths::resolve(&project, Some("..")).expect_err("`..` must fail closed");
        assert!(
            matches!(parent, IndexPathsError::RootAlias { .. }),
            "{parent:?}"
        );

        // The filesystem root.
        let root = IndexPaths::resolve(&project, Some("/")).expect_err("`/` must fail closed");
        assert!(
            matches!(root, IndexPathsError::RootAlias { .. }),
            "{root:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn empty_codegraph_dir_is_rejected() {
        let project = temp_dir("empty-dir");
        let error = IndexPaths::resolve(&project, Some(""))
            .expect_err("an explicitly empty override must fail closed");
        assert!(
            matches!(error, IndexPathsError::RootAlias { .. }),
            "{error:?}"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_ancestor_below_project() {
        use std::os::unix::fs::symlink;

        let project = temp_dir("symlink");
        let real = project.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = project.join("link");
        symlink(&real, &link).unwrap();

        let err = IndexPaths::resolve(&project, Some("link"))
            .expect_err("symlink component must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_configured_root_symlink_with_hidden_name() {
        use std::os::unix::fs::symlink;

        let project = temp_dir("symlink-inproj-mid");
        let real = project.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = project.join(".codegraph-win");
        symlink(&real, &link).unwrap();

        let err = IndexPaths::resolve(&project, Some(".codegraph-win"))
            .expect_err("configured symlink must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_default_root() {
        use std::os::unix::fs::symlink;

        let project = temp_dir("symlink-abs-proj");
        let real = project.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = project.join(DEFAULT_CURRENT_DIR);
        symlink(&real, &link).unwrap();

        let err = IndexPaths::resolve(&project, None)
            .expect_err("symlinked default root must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn lexical_normalize_folds_dot_and_parent() {
        assert_eq!(
            lexical_normalize(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(lexical_normalize(Path::new("/a/b/..")), PathBuf::from("/a"));
    }

    #[test]
    fn reserved_roots_default_contains_only_codegraph_path() {
        let project = temp_dir("reserved-default");
        let roots = IndexPaths::reserved_index_roots(&project, None);
        assert_eq!(roots, [project.join(".codegraph")].into_iter().collect());
        // A user source directory sharing the `.codegraph-` prefix is NOT a
        // reserved root and must remain scannable.
        assert!(!roots.contains(&project.join(".codegraph-sources")));
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn reserved_roots_include_default_and_configured_paths() {
        let project = temp_dir("reserved-configured");
        let roots = IndexPaths::reserved_index_roots(&project, Some("cache"));
        assert!(roots.contains(&project.join(".codegraph")), "{roots:?}");
        assert!(
            roots.contains(&project.join("cache")),
            "configured current root: {roots:?}"
        );
        assert_eq!(roots.len(), 2);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn reserved_roots_degrade_to_default_on_nested_configured_root() {
        let project = temp_dir("reserved-nested");
        let roots = IndexPaths::reserved_index_roots(&project, Some("cache/index"));
        assert_eq!(roots, [project.join(".codegraph")].into_iter().collect());
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn reserved_roots_degrade_to_default_paths_on_invalid_configured_root() {
        let project = temp_dir("reserved-invalid");
        // `.` resolves to the project root itself — an invalid alias. Root
        // derivation degrades to the safe default paths, never errors.
        let roots = IndexPaths::reserved_index_roots(&project, Some("."));
        assert_eq!(roots, [project.join(".codegraph")].into_iter().collect());
        let _ = std::fs::remove_dir_all(&project);
    }
}
