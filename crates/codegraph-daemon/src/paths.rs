#[cfg(unix)]
use std::env;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[cfg(unix)]
const POSIX_SOCKET_PATH_LIMIT: usize = 100;

pub(crate) fn codegraph_dir(project_root: &Path) -> PathBuf {
    project_root.join(".codegraph")
}

pub fn daemon_pid_path(project_root: &Path) -> PathBuf {
    codegraph_dir(project_root).join("daemon.pid")
}

/// Path of the appended log file the detached daemon's stdout+stderr are
/// redirected to (`.codegraph/daemon.log`).
pub fn daemon_log_path(project_root: &Path) -> PathBuf {
    codegraph_dir(project_root).join("daemon.log")
}

#[cfg(unix)]
pub fn daemon_socket_path(project_root: &Path) -> PathBuf {
    let in_project = codegraph_dir(project_root).join("daemon.sock");
    if in_project.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT {
        return in_project;
    }
    env::temp_dir().join(format!("codegraph-{}.sock", project_hash(project_root)))
}

/// Ordered, deterministic socket-bind candidates for `project_root`
/// (`f83a1ec`). Candidate #1 is the project-dir socket (when its path fits the
/// POSIX limit); candidate #2 is the hashed-tmpdir socket. On filesystems that
/// reject `bind()` for an AF_UNIX socket (ExFAT/FAT, some network mounts, WSL
/// DrvFs), the daemon falls through to the next candidate. The list is
/// deduplicated and never empty: when the project path is too long for #1, the
/// tmpdir socket IS candidate #1 (matching `daemon_socket_path`).
#[cfg(unix)]
pub fn daemon_socket_candidates(project_root: &Path) -> Vec<PathBuf> {
    let in_project = codegraph_dir(project_root).join("daemon.sock");
    let tmp = env::temp_dir().join(format!("codegraph-{}.sock", project_hash(project_root)));
    if in_project.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT && in_project != tmp {
        vec![in_project, tmp]
    } else {
        vec![tmp]
    }
}

// Windows has no filesystem socket: the rendezvous is a BARE namespaced name.
// interprocess `GenericNamespaced` prepends `\\.\pipe\` itself, so storing the
// prefix here would double it (Locked decision #8/#9).
#[cfg(windows)]
pub fn daemon_socket_path(project_root: &Path) -> PathBuf {
    PathBuf::from(format!("codegraph-{}", project_hash(project_root)))
}

fn project_hash(project_root: &Path) -> String {
    let resolved = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(resolved.to_string_lossy().as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn socket_path_uses_project_dir_for_short_paths() {
        let root = PathBuf::from("/tmp/cg-short");
        assert_eq!(
            daemon_socket_path(&root),
            root.join(".codegraph/daemon.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn candidate_chain_starts_with_project_socket_then_tmpdir() {
        // Given a short project path, candidate #1 is the project-dir socket and
        // candidate #2 is the hashed-tmpdir socket (the bind-fallback target).
        let root = PathBuf::from("/tmp/cg-short");
        let candidates = daemon_socket_candidates(&root);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0], root.join(".codegraph/daemon.sock"));
        assert!(candidates[1].starts_with(env::temp_dir()));
        assert!(candidates[1]
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("codegraph-") && n.ends_with(".sock")));
        // The default socket path equals candidate #1.
        assert_eq!(daemon_socket_path(&root), candidates[0]);
    }

    #[cfg(unix)]
    #[test]
    fn candidate_chain_collapses_to_tmpdir_for_long_paths() {
        // Given a project path too long for an AF_UNIX socket, the only candidate
        // is the hashed-tmpdir socket (so the chain is never empty).
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(120)));
        let candidates = daemon_socket_candidates(&long);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].starts_with(env::temp_dir()));
        assert_eq!(daemon_socket_path(&long), candidates[0]);
    }

    #[cfg(windows)]
    #[test]
    fn socket_path_is_a_bare_namespaced_name() {
        let root = PathBuf::from(r"C:\tmp\cg-short");
        let name = daemon_socket_path(&root);
        let name = name.to_string_lossy();
        assert!(name.starts_with("codegraph-"));
        assert!(!name.contains(r"\\.\pipe\"));
    }
}
