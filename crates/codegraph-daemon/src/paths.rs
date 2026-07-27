//! Daemon rendezvous paths, derived from the ONE `IndexPaths` authority.
//!
//! Frozen plan lines 590-592: current daemon paths use the centralized current
//! root and the v2 socket identity. Nothing here reconstructs a `.codegraph*`
//! path: every value comes from [`IndexPaths::resolve`], which fails closed on an
//! unsafe/aliased/overlapping configured root, so each accessor is fallible.

use std::path::{Path, PathBuf};

use anyhow::Result;
use codegraph_core::IndexPaths;
use sha2::{Digest, Sha256};

#[cfg(unix)]
const POSIX_SOCKET_PATH_LIMIT: usize = 100;

/// The v2 rendezvous discriminator carried by the out-of-root socket identities
/// (the POSIX tmpdir fallback and the Windows namespaced pipe), so a current
/// daemon can never collide with a legacy `v0.40.4` rendezvous name.
const V2_RENDEZVOUS_PREFIX: &str = "codegraph-v2-";

/// Resolve the project's index paths, honoring `CODEGRAPH_DIR`.
pub(crate) fn index_paths(project_root: &Path) -> Result<IndexPaths> {
    Ok(IndexPaths::resolve(
        project_root,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )?)
}

/// The rendezvous directory: the resolved current index root.
pub(crate) fn rendezvous_dir(project_root: &Path) -> Result<PathBuf> {
    Ok(index_paths(project_root)?.current_root().to_path_buf())
}

pub fn daemon_pid_path(project_root: &Path) -> Result<PathBuf> {
    Ok(index_paths(project_root)?.daemon_pid())
}

/// Path of the appended log file the detached daemon's stdout+stderr are
/// redirected to.
pub fn daemon_log_path(project_root: &Path) -> Result<PathBuf> {
    Ok(index_paths(project_root)?.daemon_log())
}

#[cfg(unix)]
pub fn daemon_socket_path(project_root: &Path) -> Result<PathBuf> {
    Ok(socket_identity(&index_paths(project_root)?))
}

#[cfg(unix)]
fn socket_identity(paths: &IndexPaths) -> PathBuf {
    let in_root = paths.daemon_socket();
    if in_root.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT {
        return in_root;
    }
    tmp_socket(paths)
}

#[cfg(unix)]
fn tmp_socket(paths: &IndexPaths) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{V2_RENDEZVOUS_PREFIX}{}.sock",
        rendezvous_name(paths)
    ))
}

/// Ordered, deterministic socket-bind candidates for `project_root`
/// (`f83a1ec`). Candidate #1 is the in-root socket (when its path fits the POSIX
/// limit); candidate #2 is the hashed-tmpdir socket. On filesystems that reject
/// `bind()` for an AF_UNIX socket (ExFAT/FAT, some network mounts, WSL DrvFs),
/// the daemon falls through to the next candidate. The list is deduplicated and
/// never empty: when the in-root path is too long for #1, the tmpdir socket IS
/// candidate #1 (matching [`daemon_socket_path`]).
#[cfg(unix)]
pub fn daemon_socket_candidates(project_root: &Path) -> Result<Vec<PathBuf>> {
    let paths = index_paths(project_root)?;
    let in_root = paths.daemon_socket();
    let tmp = tmp_socket(&paths);
    Ok(
        if in_root.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT && in_root != tmp {
            vec![in_root, tmp]
        } else {
            vec![tmp]
        },
    )
}

// Windows has no filesystem socket: the rendezvous is a BARE namespaced name.
// interprocess `GenericNamespaced` prepends `\\.\pipe\` itself, so storing the
// prefix here would double it (Locked decision #8/#9).
#[cfg(windows)]
pub fn daemon_socket_path(project_root: &Path) -> Result<PathBuf> {
    let paths = index_paths(project_root)?;
    Ok(PathBuf::from(format!(
        "{V2_RENDEZVOUS_PREFIX}{}",
        rendezvous_name(&paths)
    )))
}

/// The 16-hex-character discriminated name used by the OUT-OF-ROOT rendezvous
/// identities (the POSIX tmpdir fallback and the Windows namespaced pipe).
///
/// It is `sha256(V2_RENDEZVOUS_PREFIX || projectIdentity)` truncated, NOT a
/// prefix of the identity itself: the legacy `v0.40.4` name is
/// `sha256(<project path>)[..16]`, and mixing in this version's own domain
/// separator makes the two provably distinct names for the same project even if
/// a future identity scheme ever coincided with a path hash. The authoritative
/// owner binding always carries the FULL identity; this value only has to be a
/// stable, collision-resistant NAME.
fn rendezvous_name(paths: &IndexPaths) -> String {
    let mut hasher = Sha256::new();
    hasher.update(V2_RENDEZVOUS_PREFIX.as_bytes());
    hasher.update(paths.project_identity().as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cg-daemon-paths-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    #[test]
    fn rendezvous_paths_come_from_the_resolved_current_root() {
        let project = temp_project("current-root");
        let paths = index_paths(&project).unwrap();
        assert_eq!(daemon_pid_path(&project).unwrap(), paths.daemon_pid());
        assert_eq!(daemon_log_path(&project).unwrap(), paths.daemon_log());
        assert_eq!(rendezvous_dir(&project).unwrap(), paths.current_root());
        assert!(
            paths
                .current_root()
                .starts_with(project.join(".codegraph-v2")),
            "the default rendezvous dir is the v2 current root: {}",
            paths.current_root().display()
        );
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn rendezvous_paths_fail_closed_for_an_unresolvable_project() {
        // No `CODEGRAPH_DIR` mutation: that variable is process-global and this
        // crate's unit tests share one binary, so an env-mutating test would race
        // every other rendezvous resolution. An absent project is an equally
        // authoritative fail-closed input, because `IndexPaths` derives the
        // PHYSICAL project identity and cannot invent one for a path that does
        // not exist.
        let missing = std::env::temp_dir().join(format!(
            "cg-daemon-paths-absent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert!(daemon_pid_path(&missing).is_err());
        assert!(daemon_log_path(&missing).is_err());
        assert!(daemon_socket_path(&missing).is_err());
        assert!(rendezvous_dir(&missing).is_err());
        #[cfg(unix)]
        assert!(daemon_socket_candidates(&missing).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn socket_path_uses_the_current_root_for_short_paths() {
        let project = temp_project("short");
        let paths = index_paths(&project).unwrap();
        assert_eq!(daemon_socket_path(&project).unwrap(), paths.daemon_socket());
        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn candidate_chain_starts_with_the_in_root_socket_then_tmpdir() {
        let project = temp_project("chain");
        let paths = index_paths(&project).unwrap();
        let candidates = daemon_socket_candidates(&project).unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0], paths.daemon_socket());
        assert!(candidates[1].starts_with(std::env::temp_dir()));
        assert!(
            candidates[1]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(
                    |name| name.starts_with(V2_RENDEZVOUS_PREFIX) && name.ends_with(".sock")
                ),
            "the tmpdir fallback carries the v2 discriminator: {:?}",
            candidates[1]
        );
        assert_eq!(daemon_socket_path(&project).unwrap(), candidates[0]);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[cfg(unix)]
    #[test]
    fn candidate_chain_collapses_to_tmpdir_for_long_paths() {
        let base = temp_project("long");
        let project = base.join("x".repeat(120));
        std::fs::create_dir_all(&project).unwrap();
        let candidates = daemon_socket_candidates(&project).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].starts_with(std::env::temp_dir()));
        assert_eq!(daemon_socket_path(&project).unwrap(), candidates[0]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn socket_path_is_a_bare_namespaced_v2_name() {
        let project = temp_project("windows-name");
        let name = daemon_socket_path(&project).unwrap();
        let name = name.to_string_lossy();
        assert!(name.starts_with(V2_RENDEZVOUS_PREFIX));
        assert!(!name.contains(r"\\.\pipe\"));
        let _ = std::fs::remove_dir_all(&project);
    }
}
