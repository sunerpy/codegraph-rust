//! The installed executable a long-lived process runs (upstream #2346).
//!
//! A daemon outlives an upgrade: it keeps running the code it started with
//! while the install it came from is replaced or removed. Recording the
//! executable's identity at start and comparing it later tells a daemon, or a
//! session, that this happened, so it can step aside for the current install
//! instead of holding the project with replaced code.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Env var name: milliseconds between a daemon's checks of its own install
/// (default 30000). `0` turns the check off. Mirrors upstream.
pub const CODEGRAPH_DAEMON_INSTALL_CHECK_MS: &str = "CODEGRAPH_DAEMON_INSTALL_CHECK_MS";

const DEFAULT_INSTALL_CHECK_MS: u64 = 30_000;

/// One installed executable as it was when recorded: its canonical path, and
/// the file's physical identity, length and modification time there. Replacing
/// or rewriting the binary changes at least one of them, and removing it leaves
/// none to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallIdentity {
    path: PathBuf,
    stamp: InstallStamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallStamp {
    identity: codegraph_store::PathIdentity,
    len: u64,
    modified: Option<SystemTime>,
}

impl InstallIdentity {
    /// The running executable's install as it is now, or `None` when the
    /// executable cannot be located or read (then nothing can be checked).
    #[must_use]
    pub fn current() -> Option<Self> {
        Self::of(&std::env::current_exe().ok()?)
    }

    /// The install at `path` as it is now. A symlink is resolved first, so a
    /// package manager that retargets the link to a new version reads as a
    /// changed install once the old target is gone.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let path = std::fs::canonicalize(path).ok()?;
        let stamp = stamp(&path)?;
        Some(Self { path, stamp })
    }

    /// The canonical path recorded.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file at the recorded path is no longer the one recorded:
    /// replaced, rewritten, or gone.
    #[must_use]
    pub fn changed(&self) -> bool {
        stamp(&self.path).as_ref() != Some(&self.stamp)
    }
}

fn stamp(path: &Path) -> Option<InstallStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(InstallStamp {
        identity: codegraph_store::PathIdentity::of(path).ok()?,
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// How often a daemon checks its install: [`CODEGRAPH_DAEMON_INSTALL_CHECK_MS`],
/// or `None` when it is `0`. Unset, empty or malformed values use the default.
#[must_use]
pub fn install_check_interval() -> Option<Duration> {
    parse_install_check_ms(
        std::env::var(CODEGRAPH_DAEMON_INSTALL_CHECK_MS)
            .ok()
            .as_deref(),
    )
}

fn parse_install_check_ms(raw: Option<&str>) -> Option<Duration> {
    let millis = raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INSTALL_CHECK_MS);
    (millis > 0).then(|| Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "codegraph-install-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn an_untouched_install_is_unchanged() {
        let dir = TempDir::new("untouched");
        let binary = dir.0.join("codegraph");
        std::fs::write(&binary, b"build one").unwrap();
        let install = InstallIdentity::of(&binary).expect("readable install");
        assert!(!install.changed());
    }

    #[test]
    fn a_replaced_install_is_changed() {
        let dir = TempDir::new("replaced");
        let binary = dir.0.join("codegraph");
        std::fs::write(&binary, b"build one").unwrap();
        let install = InstallIdentity::of(&binary).expect("readable install");
        // An upgrade writes the new build beside the old one and renames it
        // over, the way installers replace a running executable.
        let staged = dir.0.join("codegraph.new");
        std::fs::write(&staged, b"build two!").unwrap();
        std::fs::rename(&staged, &binary).unwrap();
        assert!(install.changed());
    }

    #[test]
    fn a_removed_install_is_changed() {
        let dir = TempDir::new("removed");
        let binary = dir.0.join("codegraph");
        std::fs::write(&binary, b"build one").unwrap();
        let install = InstallIdentity::of(&binary).expect("readable install");
        std::fs::remove_file(&binary).unwrap();
        assert!(install.changed());
    }

    #[test]
    fn a_missing_executable_records_nothing() {
        let dir = TempDir::new("missing");
        assert_eq!(InstallIdentity::of(&dir.0.join("absent")), None);
    }

    #[test]
    fn the_check_interval_parses_its_env_value() {
        assert_eq!(parse_install_check_ms(None), Some(Duration::from_secs(30)));
        assert_eq!(
            parse_install_check_ms(Some("")),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_install_check_ms(Some("soon")),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_install_check_ms(Some("0")), None);
        assert_eq!(
            parse_install_check_ms(Some("250")),
            Some(Duration::from_millis(250))
        );
    }
}
