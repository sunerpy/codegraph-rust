//! Windows and WSL sharing one index on a Windows drive (upstream #995).
//!
//! A repo under `/mnt/<drive>/` can be used from both sides: Windows-native
//! CodeGraph and WSL CodeGraph then open the same `.codegraph/codegraph.db`
//! across the 9p/DrvFs bridge, where SQLite's file locking and its `-shm`
//! shared memory do not hold between the two systems, and WSL's connection
//! fails with a bare "disk I/O error". A fresh WSL index there already gets its
//! own `.codegraph-wsl` (`IndexPaths::resolve`); an index kept in `.codegraph`
//! that fails this way gets the instruction below instead of an unexplained
//! I/O failure.

use std::error::Error;
use std::path::Path;

use codegraph_core::index_paths::{DEFAULT_CURRENT_DIR, WSL_CURRENT_DIR};

use crate::StoreError;

/// Guidance for `error` when a SQLite I/O failure hit the default-named index
/// of a project on a Windows drive under WSL; `None` otherwise. Only the
/// default `.codegraph` counts: it is the directory Windows CodeGraph opens
/// too, and an I/O failure on any other root has some other cause.
pub fn wsl_shared_index_guidance(error: &(dyn Error + 'static)) -> Option<String> {
    guidance_for(error, codegraph_core::wsl::is_wsl_windows_drive)
}

fn guidance_for(
    error: &(dyn Error + 'static),
    on_wsl_windows_drive: impl Fn(&Path) -> bool,
) -> Option<String> {
    let mut cause = Some(error);
    let db = std::iter::from_fn(|| {
        let current = cause?;
        cause = current.source();
        Some(current)
    })
    .find_map(|error| io_failure_database(error.downcast_ref::<StoreError>()?))?;
    let data_dir = db.parent()?;
    if data_dir.file_name()? != DEFAULT_CURRENT_DIR || !on_wsl_windows_drive(db) {
        return None;
    }
    Some(format!(
        "This project is on a Windows drive, where SQLite's file locking doesn't work across \
         the Windows/WSL boundary, so this usually means CodeGraph on Windows is using the same \
         index at {}. Windows and WSL can't share one index on a Windows drive; give WSL its own:\n\
         \x20 1. Set CODEGRAPH_DIR={WSL_CURRENT_DIR} in the WSL environment (your shell profile, \
         or the env of the MCP server your agent starts).\n\
         \x20 2. Run \"codegraph init\" in WSL to build that index. Windows keeps using \
         {DEFAULT_CURRENT_DIR}.\n",
        data_dir.display()
    ))
}

/// The database a SQLite I/O failure was reported against, if `error` is one.
fn io_failure_database(error: &StoreError) -> Option<&Path> {
    let (path, source) = match error {
        StoreError::Open { path, source }
        | StoreError::Configure { path, source }
        | StoreError::Migrate { path, source }
        | StoreError::ReadExtractionStamp { path, source }
        | StoreError::Close { path, source }
        | StoreError::CheckpointWal { path, source } => (path, source),
        _ => return None,
    };
    is_sqlite_io_error(source).then_some(path.as_path())
}

/// SQLite reports an I/O failure as `SQLITE_IOERR`, whose extended codes keep
/// the primary code in their low byte.
fn is_sqlite_io_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _) if failure.extended_code & 0xff == 10
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn io_error(path: &str) -> StoreError {
        StoreError::Configure {
            path: PathBuf::from(path),
            // SQLITE_IOERR_SHMMAP: the -shm mapping the bridge cannot share.
            source: rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5130), None),
        }
    }

    #[test]
    fn an_io_error_on_a_shared_default_index_explains_the_wsl_fix() {
        let error = io_error("/mnt/c/repo/.codegraph/codegraph.db");
        let guidance = guidance_for(&error, |_| true).expect("guidance");
        assert!(
            guidance.contains("CODEGRAPH_DIR=.codegraph-wsl"),
            "{guidance}"
        );
        assert!(guidance.contains("/mnt/c/repo/.codegraph"), "{guidance}");

        assert_eq!(guidance_for(&error, |_| false), None, "not WSL");
        let own_root = io_error("/mnt/c/repo/.codegraph-wsl/codegraph.db");
        assert_eq!(guidance_for(&own_root, |_| true), None, "WSL's own index");
        let not_io = StoreError::Configure {
            path: PathBuf::from("/mnt/c/repo/.codegraph/codegraph.db"),
            source: rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None),
        };
        assert_eq!(guidance_for(&not_io, |_| true), None, "SQLITE_BUSY");
    }
}
