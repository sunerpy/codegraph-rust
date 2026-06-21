use std::env;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const POSIX_SOCKET_PATH_LIMIT: usize = 100;

pub(crate) fn codegraph_dir(project_root: &Path) -> PathBuf {
    project_root.join(".codegraph")
}

pub fn daemon_pid_path(project_root: &Path) -> PathBuf {
    codegraph_dir(project_root).join("daemon.pid")
}

pub fn daemon_socket_path(project_root: &Path) -> PathBuf {
    let in_project = codegraph_dir(project_root).join("daemon.sock");
    if in_project.as_os_str().len() <= POSIX_SOCKET_PATH_LIMIT {
        return in_project;
    }
    env::temp_dir().join(format!("codegraph-{}.sock", project_hash(project_root)))
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

    #[test]
    fn socket_path_uses_project_dir_for_short_paths() {
        let root = PathBuf::from("/tmp/cg-short");
        assert_eq!(
            daemon_socket_path(&root),
            root.join(".codegraph/daemon.sock")
        );
    }
}
