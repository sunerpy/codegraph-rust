//! WSL detection, shared by the index-root default (upstream #995) and the
//! watcher's `/mnt/` opt-out.

use std::path::Path;

/// Whether this process runs under WSL: Linux, and `WSL_DISTRO_NAME` /
/// `WSL_INTEROP` set or `/proc/version` naming Microsoft or WSL.
pub fn is_wsl() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .map(|version| {
            let version = version.to_ascii_lowercase();
            version.contains("microsoft") || version.contains("wsl")
        })
        .unwrap_or(false)
}

/// Whether `path` is a Windows drive as WSL mounts it (`/mnt/c`,
/// `/mnt/d/project`). A pure path check.
pub fn is_windows_drive_mount(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let mut parts = normalized.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(""), Some("mnt"), Some(drive))
            if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic()
    )
}

/// Whether `path` is on a Windows drive under WSL, where Windows-native
/// CodeGraph opens the same tree. Off a `/mnt/<drive>` path this never reads
/// the environment or `/proc/version`.
pub fn is_wsl_windows_drive(path: &Path) -> bool {
    is_windows_drive_mount(path) && is_wsl()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_mounts_are_single_letter_mnt_paths() {
        assert!(is_windows_drive_mount(Path::new("/mnt/c")));
        assert!(is_windows_drive_mount(Path::new("/mnt/D/project")));
        assert!(!is_windows_drive_mount(Path::new("/mnt/abc/project")));
        assert!(!is_windows_drive_mount(Path::new("/home/user/project")));
        assert!(!is_windows_drive_mount(Path::new("/mnt")));
        assert!(!is_wsl_windows_drive(Path::new("/home/user/project")));
    }
}
