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
