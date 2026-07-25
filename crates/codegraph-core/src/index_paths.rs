//! Single source of truth for CodeGraph's on-disk index paths.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md`, Batch M ("Product boundary and
//! selected storage layout", plan lines 236-338): `IndexPaths` is the ONE path
//! authority shared by CLI, store, daemon, watch, and MCP. Production code must
//! NOT reconstruct `.codegraph*` paths independently.
//!
//! This module implements the PATH LAYER ONLY (the first Batch M slice). It
//! computes, for a given project and optional `CODEGRAPH_DIR` override:
//!
//! - the physical [`project_identity`](IndexPaths::project_identity) (full
//!   lowercase SHA-256 of a versioned binary payload of the OS filesystem
//!   identifiers — `st_dev`/`st_ino` on Unix, volume serial + 128-bit file id on
//!   Windows). Unsupported filesystems fail closed; there is NO lexical-spelling
//!   fallback;
//! - the normalized [`legacy_roots`](IndexPaths::legacy_roots) set: the fixed
//!   `<project>/.codegraph` root plus, when `CODEGRAPH_DIR` is set, the old CLI
//!   root (relative-or-absolute), modeling v0.40.4's inconsistent env handling;
//! - the isolated [`current_root`](IndexPaths::current_root) — `<project>/.codegraph-v2`
//!   by default, or a `<name>-v2-<projectIdentity>` SIBLING of the normalized
//!   legacy root when `CODEGRAPH_DIR` is set;
//! - every current-root-owned artifact path (DB, permanent lock, the two fixed
//!   state slots, the `uninitialized` tombstone, `config.toml`, `codegraph.json`,
//!   and the daemon pid/log/socket identities).
//!
//! Path identity is PHYSICAL, not lexical: the project and each existing legacy
//! root are canonicalized; a not-yet-created root canonicalizes its nearest
//! existing ancestor and appends the normalized remainder. Empty/root/dot/parent
//! aliases, symlink (or Windows reparse-point) components below the canonical
//! ancestor, equality with any legacy identity, and any ancestor/descendant
//! overlap with a legacy identity all fail closed with typed, stable diagnostics.
//!
//! This slice provides the path VALUES only; the state-slot / lease / Store open
//! protocol that CONSUMES the state-slot, tombstone, and lock paths lands in a
//! later Batch M task.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Default current-root directory name (no `CODEGRAPH_DIR`): a sibling of the
/// fixed legacy `.codegraph` root. This is the ONE place the `.codegraph-v2`
/// literal is defined; every production consumer derives its default root from
/// [`IndexPaths::resolve`].
pub const DEFAULT_CURRENT_DIR: &str = ".codegraph-v2";
/// Fixed legacy root directory name used by old daemon/watch/MCP paths.
pub const LEGACY_DIR: &str = ".codegraph";
/// Infix inserted into a configured legacy root's final component to form the
/// sibling current root (`<name>-v2-<projectIdentity>`).
const CONFIGURED_SUFFIX_INFIX: &str = "-v2-";

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

    /// The derived current root equals, contains, or is contained by a legacy
    /// root. The current namespace must be fully disjoint from every legacy one.
    #[error(
        "refusing current index root {current}: it overlaps the legacy root \
         {legacy} ({reason})"
    )]
    LegacyOverlap {
        current: PathBuf,
        legacy: PathBuf,
        reason: String,
    },
}

/// The resolved, physical, fail-closed set of index paths for one project.
///
/// Construct via [`IndexPaths::resolve`]. Every artifact path is derived from a
/// single validated [`current_root`](IndexPaths::current_root); callers consume
/// these accessors instead of rebuilding `.codegraph-v2` strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexPaths {
    project: PathBuf,
    project_identity: String,
    legacy_roots: Vec<PathBuf>,
    current_root: PathBuf,
}

impl IndexPaths {
    /// Resolve the physical index paths for `project`, honoring an optional
    /// `CODEGRAPH_DIR` override (relative or absolute). Production callers pass
    /// `std::env::var("CODEGRAPH_DIR").ok().as_deref()`.
    ///
    /// `project` MUST exist (it is canonicalized to derive the physical
    /// identity). Fails closed on unsafe roots, symlink aliases, overlap with a
    /// legacy root, and unsupported filesystems.
    pub fn resolve(project: &Path, codegraph_dir: Option<&str>) -> Result<Self, IndexPathsError> {
        let project =
            project
                .canonicalize()
                .map_err(|source| IndexPathsError::ProjectInaccessible {
                    path: project.to_path_buf(),
                    source,
                })?;

        let project_identity = physical_identity(&project)?;

        // Legacy root #1: the fixed `<project>/.codegraph` (old daemon/watch/MCP).
        let legacy_fixed = physical_normalize(&project, &project.join(LEGACY_DIR))?;
        let mut legacy_roots = vec![legacy_fixed];

        // Legacy root #2 + configured current root: only when CODEGRAPH_DIR is set.
        let configured = codegraph_dir.filter(|value| !value.is_empty());
        let current_root = if let Some(value) = configured {
            let raw = Path::new(value);
            let cli_root = if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                project.join(raw)
            };
            let cli_root = lexical_normalize(&cli_root);
            reject_project_alias(&project, &cli_root, value)?;
            let cli_root = physical_normalize(&project, &cli_root)?;

            // Sibling `<name>-v2-<projectIdentity>`, never a child.
            let parent = cli_root
                .parent()
                .ok_or_else(|| IndexPathsError::RootAlias {
                    path: cli_root.clone(),
                    reason: "configured legacy root has no parent directory".to_string(),
                })?;
            let name = cli_root
                .file_name()
                .ok_or_else(|| IndexPathsError::RootAlias {
                    path: cli_root.clone(),
                    reason: "configured legacy root has no final component".to_string(),
                })?
                .to_string_lossy()
                .into_owned();
            let sibling_name = format!("{name}{CONFIGURED_SUFFIX_INFIX}{project_identity}");
            let current = parent.join(sibling_name);

            if !legacy_roots.contains(&cli_root) {
                legacy_roots.push(cli_root);
            }
            physical_normalize(&project, &current)?
        } else {
            physical_normalize(&project, &project.join(DEFAULT_CURRENT_DIR))?
        };

        // Fail closed on any equality or ancestor/descendant overlap with a
        // legacy identity: the current namespace must be fully disjoint.
        for legacy in &legacy_roots {
            if let Some(reason) = overlap_reason(&current_root, legacy) {
                return Err(IndexPathsError::LegacyOverlap {
                    current: current_root.clone(),
                    legacy: legacy.clone(),
                    reason: reason.to_string(),
                });
            }
        }

        Ok(Self {
            project,
            project_identity,
            legacy_roots,
            current_root,
        })
    }

    /// The EXACT resolved physical index-root PATHS the scanner/watcher must
    /// exclude before descending: every fixed/configured legacy root plus the
    /// current root, as full normalized paths (not basenames). The scanner
    /// compares each candidate directory path against this set, so a nested
    /// configured root such as `<project>/cache/index` is excluded at its true
    /// depth while an unrelated directory that merely shares a basename is not.
    ///
    /// In-project roots are re-anchored onto the caller's `project` spelling
    /// (`resolve` canonicalizes) so scanner candidates compare directly; roots
    /// outside the project keep their absolute form. Always includes the fixed
    /// `<project>/.codegraph`. On a `resolve` failure it degrades to just the two
    /// safe default paths (fixed `.codegraph` and the default `.codegraph-v2`)
    /// rather than reconstructing a configured-root path; the fail-closed DB
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
                roots.insert(project.join(LEGACY_DIR));
                for legacy in paths.legacy_roots() {
                    roots.insert(reanchor(legacy));
                }
                roots.insert(reanchor(paths.current_root()));
            }
            Err(_) => {
                roots.insert(project.join(LEGACY_DIR));
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
    /// payload. Same value seeds state ownership and configured-root suffixes.
    pub fn project_identity(&self) -> &str {
        &self.project_identity
    }

    /// The normalized legacy-root set: the fixed `<project>/.codegraph` plus,
    /// when `CODEGRAPH_DIR` is set, the old CLI root. New binaries never open,
    /// migrate, or write these.
    pub fn legacy_roots(&self) -> &[PathBuf] {
        &self.legacy_roots
    }

    /// The isolated current index root (`.codegraph-v2` by default; a
    /// `<name>-v2-<projectIdentity>` sibling of the configured legacy root when
    /// `CODEGRAPH_DIR` is set).
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
    /// POSIX tmp-socket fallback and Windows namespaced-pipe forms, which carry
    /// the v2 protocol discriminator, are owned by the daemon lifecycle layer in
    /// a later slice.)
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

/// Reject a configured legacy root that aliases the project itself, an ancestor
/// of the project, or the filesystem root — the `.`/`..`/root alias cases.
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

/// If `current` and `legacy` overlap (equal, or one contains the other), return
/// a stable reason string; otherwise `None`.
fn overlap_reason(current: &Path, legacy: &Path) -> Option<&'static str> {
    if current == legacy {
        Some("equal paths")
    } else if current.starts_with(legacy) {
        Some("current root is inside the legacy root")
    } else if legacy.starts_with(current) {
        Some("legacy root is inside the current root")
    } else {
        None
    }
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
    fn default_current_root_is_sibling_codegraph_v2() {
        let project = temp_dir("default");
        let paths = IndexPaths::resolve(&project, None).expect("resolve default");

        assert_eq!(paths.current_root(), project.join(".codegraph-v2"));
        assert_eq!(paths.legacy_roots(), [project.join(".codegraph")]);
        assert_eq!(
            paths.current_db(),
            project.join(".codegraph-v2").join("codegraph.db")
        );
        // The default current root is a SIBLING of the legacy root, never a child.
        assert!(!paths.current_root().starts_with(project.join(".codegraph")));

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn every_derived_artifact_lives_under_current_root() {
        let project = temp_dir("artifacts");
        let paths = IndexPaths::resolve(&project, None).expect("resolve");
        let root = project.join(".codegraph-v2");

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
    fn relative_codegraph_dir_current_root_is_sibling_with_identity_suffix() {
        let project = temp_dir("rel-dir");
        let paths = IndexPaths::resolve(&project, Some("cache")).expect("resolve relative");

        // Legacy roots: the fixed `.codegraph` plus the configured relative root.
        assert_eq!(
            paths.legacy_roots(),
            [project.join(".codegraph"), project.join("cache")]
        );
        // Current root is the SIBLING `cache-v2-<identity>`, not a child.
        let expected = project.join(format!("cache-v2-{}", paths.project_identity()));
        assert_eq!(paths.current_root(), expected);
        assert!(!paths.current_root().starts_with(project.join("cache")));

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn absolute_codegraph_dir_current_root_is_sibling_of_configured_root() {
        let project = temp_dir("abs-proj");
        let cache = temp_dir("abs-cache");
        let configured = cache.join("cg");
        let paths = IndexPaths::resolve(&project, Some(configured.to_str().unwrap()))
            .expect("resolve absolute");

        assert!(
            paths.legacy_roots().contains(&configured),
            "configured absolute root is a legacy root: {:?}",
            paths.legacy_roots()
        );
        // Sibling `<name>-v2-<identity>` next to the configured root.
        let expected = cache.join(format!("cg-v2-{}", paths.project_identity()));
        assert_eq!(paths.current_root(), expected);
        assert!(!paths.current_root().starts_with(&configured));

        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn two_projects_do_not_collide_on_a_shared_configured_root() {
        // Two DISTINCT physical projects given the SAME configured root must
        // receive DISTINCT current roots (identity-suffixed), so one cannot open
        // the other's state.
        let project_a = temp_dir("collide-a");
        let project_b = temp_dir("collide-b");
        let shared = temp_dir("collide-shared");
        let configured = shared.join("cg");

        let a = IndexPaths::resolve(&project_a, Some(configured.to_str().unwrap())).expect("a");
        let b = IndexPaths::resolve(&project_b, Some(configured.to_str().unwrap())).expect("b");

        assert_ne!(
            a.project_identity(),
            b.project_identity(),
            "distinct physical projects have distinct identities"
        );
        assert_ne!(
            a.current_root(),
            b.current_root(),
            "shared configured root must not collapse two projects onto one current root"
        );

        let _ = std::fs::remove_dir_all(&project_a);
        let _ = std::fs::remove_dir_all(&project_b);
        let _ = std::fs::remove_dir_all(&shared);
    }

    #[test]
    fn two_projects_do_not_collide_on_an_escaping_relative_root() {
        // A relative CODEGRAPH_DIR that escapes to a shared external directory
        // must still yield distinct, identity-suffixed current roots.
        let base = temp_dir("escape-base");
        let project_a = base.join("a");
        let project_b = base.join("b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        std::fs::create_dir_all(base.join("shared")).unwrap();

        let a = IndexPaths::resolve(&project_a, Some("../shared/cg")).expect("a");
        let b = IndexPaths::resolve(&project_b, Some("../shared/cg")).expect("b");

        // Both configured roots normalize to the SAME external path…
        assert_eq!(a.legacy_roots()[1], b.legacy_roots()[1]);
        // …yet the current roots differ by physical identity.
        assert_ne!(a.current_root(), b.current_root());

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
    fn empty_codegraph_dir_is_treated_as_unset_default() {
        let project = temp_dir("empty-dir");
        // An empty override string is ignored (treated as unset), matching the
        // shell semantics where an empty env value is a no-op override.
        let paths = IndexPaths::resolve(&project, Some("")).expect("empty resolves to default");
        assert_eq!(paths.current_root(), project.join(".codegraph-v2"));
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

        // A configured root that descends THROUGH a symlink component below the
        // project must fail closed rather than follow the link.
        let err = IndexPaths::resolve(&project, Some("link/cg"))
            .expect_err("symlink component must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_in_project_root_reached_through_intermediate_symlink() {
        use std::os::unix::fs::symlink;

        // In-project spelling of the intermediate-alias case: `<project>/link`
        // is a symlink to `<project>/real`, so `<project>/link/child` EXISTS as
        // an ordinary directory reached THROUGH the alias. A relative
        // `CODEGRAPH_DIR=link/child/cg` must fail closed on the `link` component
        // even though the nearest existing ancestor (`.../child`) is ordinary.
        let project = temp_dir("symlink-inproj-mid");
        let real = project.join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let link = project.join("link");
        symlink(&real, &link).unwrap();

        let err = IndexPaths::resolve(&project, Some("link/child/cg"))
            .expect_err("in-project intermediate symlink must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_absolute_configured_root() {
        use std::os::unix::fs::symlink;

        let project = temp_dir("symlink-abs-proj");
        let cache = temp_dir("symlink-abs-cache");
        let real = cache.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = cache.join("link");
        symlink(&real, &link).unwrap();

        // An absolute configured root that IS a symlink must fail closed.
        let err = IndexPaths::resolve(&project, Some(link.to_str().unwrap()))
            .expect_err("symlinked absolute root must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_external_configured_root_reached_through_intermediate_symlink() {
        use std::os::unix::fs::symlink;

        // `<cache>/link` is a symlink to `<cache>/real`; `<cache>/link/child`
        // therefore EXISTS as an ordinary directory reached through the alias.
        // An absolute CODEGRAPH_DIR of `<cache>/link/child/cg` must fail closed
        // even though its nearest existing ancestor (`.../child`) is itself an
        // ordinary directory — the intermediate `link` component is the alias.
        let project = temp_dir("symlink-mid-proj");
        let cache = temp_dir("symlink-mid-cache");
        let real = cache.join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let link = cache.join("link");
        symlink(&real, &link).unwrap();

        let configured = link.join("child").join("cg");
        let err = IndexPaths::resolve(&project, Some(configured.to_str().unwrap()))
            .expect_err("intermediate symlink component must fail closed");
        assert!(
            matches!(err, IndexPathsError::SymlinkComponent { .. }),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn rejects_current_root_overlapping_legacy_root() {
        // A configured root whose derived sibling would nest under the fixed
        // legacy `.codegraph` root must fail closed. Point CODEGRAPH_DIR at a
        // child of `.codegraph`; the sibling `<name>-v2-<id>` then still lives
        // inside `.codegraph`, overlapping the fixed legacy root.
        let project = temp_dir("overlap");
        std::fs::create_dir_all(project.join(".codegraph")).unwrap();
        let err = IndexPaths::resolve(&project, Some(".codegraph/inner"))
            .expect_err("overlap with legacy root must fail closed");
        assert!(
            matches!(err, IndexPathsError::LegacyOverlap { .. }),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn current_root_never_equals_a_legacy_root() {
        let project = temp_dir("disjoint");
        let paths = IndexPaths::resolve(&project, Some("cache")).expect("resolve");
        for legacy in paths.legacy_roots() {
            assert_ne!(paths.current_root(), legacy);
            assert!(!paths.current_root().starts_with(legacy));
            assert!(!legacy.starts_with(paths.current_root()));
        }
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
    fn reserved_roots_default_are_exactly_legacy_and_v2_paths() {
        let project = temp_dir("reserved-default");
        let roots = IndexPaths::reserved_index_roots(&project, None);
        assert!(roots.contains(&project.join(".codegraph")));
        assert!(roots.contains(&project.join(".codegraph-v2")));
        // A user source directory sharing the `.codegraph-` prefix is NOT a
        // reserved root and must remain scannable.
        assert!(!roots.contains(&project.join(".codegraph-sources")));
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn reserved_roots_include_relative_configured_sibling_as_full_path() {
        let project = temp_dir("reserved-configured");
        let roots = IndexPaths::reserved_index_roots(&project, Some("cache"));
        let identity = IndexPaths::resolve(&project, Some("cache"))
            .unwrap()
            .project_identity()
            .to_string();
        assert!(roots.contains(&project.join(".codegraph")), "{roots:?}");
        assert!(
            roots.contains(&project.join("cache")),
            "configured legacy root: {roots:?}"
        );
        assert!(
            roots.contains(&project.join(format!("cache-v2-{identity}"))),
            "configured current sibling: {roots:?}"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn reserved_roots_include_nested_configured_roots_at_true_depth() {
        // A nested `CODEGRAPH_DIR=cache/index` puts the legacy root at
        // `<project>/cache/index` and the current root at
        // `<project>/cache/index-v2-<identity>`; both must be reserved as their
        // full nested paths, so the scanner prunes them at depth rather than
        // descending into and indexing the index's own storage.
        let project = temp_dir("reserved-nested");
        let roots = IndexPaths::reserved_index_roots(&project, Some("cache/index"));
        let identity = IndexPaths::resolve(&project, Some("cache/index"))
            .unwrap()
            .project_identity()
            .to_string();
        assert!(
            roots.contains(&project.join("cache").join("index")),
            "nested configured legacy root: {roots:?}"
        );
        assert!(
            roots.contains(&project.join("cache").join(format!("index-v2-{identity}"))),
            "nested configured current sibling: {roots:?}"
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn reserved_roots_degrade_to_default_paths_on_invalid_configured_root() {
        let project = temp_dir("reserved-invalid");
        // `.` resolves to the project root itself — an invalid alias. Root
        // derivation degrades to the safe default paths, never errors.
        let roots = IndexPaths::reserved_index_roots(&project, Some("."));
        assert!(roots.contains(&project.join(".codegraph")));
        assert!(roots.contains(&project.join(".codegraph-v2")));
        let _ = std::fs::remove_dir_all(&project);
    }
}
