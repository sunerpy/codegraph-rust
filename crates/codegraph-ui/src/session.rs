//! Opening the project's index for one request — the counterpart of upstream
//! `src/ui-server/api/session.ts`.
//!
//! Upstream keeps one long-lived connection and re-opens it when the database
//! file is replaced. The Rust store already has a request-scoped, leased
//! read-only open (`Store::open_for_read`, the one the MCP engine uses), so each
//! API request opens the published namespace, answers, and drops it: a rebuild
//! or a namespace swap between two requests is simply picked up by the next one,
//! and no request ever holds a handle across one.
//!
//! The viewer never creates, migrates or syncs an index. A missing index is
//! `no-index`; one that will not open read-only — an outdated extraction, a build
//! in progress, a newer format — is `index-unusable` with the one command that
//! fixes it.

use std::path::Path;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{ExtractionStatus, Store, StoreError};

use crate::respond::{ApiError, ApiErrorCode, ApiResult};

/// How long a request may wait for a read lease before it gives up.
pub const READ_LEASE_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolve the project's index paths through the one authority every other
/// command uses (`CODEGRAPH_DIR` included).
pub fn resolve_paths(project_root: &Path) -> anyhow::Result<IndexPaths> {
    Ok(IndexPaths::resolve(
        project_root,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )?)
}

fn no_index(project_root: &Path) -> ApiError {
    ApiError::new(
        ApiErrorCode::NoIndex,
        format!("No CodeGraph index found for {}.", project_root.display()),
        Some(
            "The viewer reads an index that already exists — it never creates one. \
             Run \"codegraph init\" in that project, or start the viewer against a project \
             that has one: codegraph ui /path/to/indexed/project"
                .to_string(),
        ),
    )
}

fn unusable(project_root: &Path, detail: impl std::fmt::Display, hint: String) -> ApiError {
    ApiError::new(
        ApiErrorCode::IndexUnusable,
        format!(
            "The CodeGraph index for {} could not be opened: {detail}",
            project_root.display()
        ),
        Some(hint),
    )
}

/// The hint for an index state this binary will not read.
fn hint_for(status: &ExtractionStatus, project_root: &Path) -> String {
    let root = project_root.display();
    match status {
        ExtractionStatus::Outdated { .. } => format!(
            "Nothing was opened or changed. Upgrade it in place with \"codegraph sync {root}\"; saved trails are kept."
        ),
        ExtractionStatus::Building { .. } => {
            "Another CodeGraph process is building it; wait for that to finish, then reload."
                .to_string()
        }
        ExtractionStatus::Future { .. } => {
            "It was written by a newer CodeGraph. Upgrade this codegraph binary to read it."
                .to_string()
        }
        _ => format!(
            "If another CodeGraph process is rebuilding it, wait for that to finish. \
             If the index is damaged, rebuild it with \"codegraph init {root}\"."
        ),
    }
}

/// Open the published index read-only for one request.
pub fn open_store(project_root: &Path, paths: &IndexPaths) -> ApiResult<Store> {
    let status = Store::extraction_status(paths);
    if status == ExtractionStatus::Missing && !paths.current_db().is_file() {
        return Err(no_index(project_root));
    }
    match Store::open_for_read(paths, Instant::now() + READ_LEASE_TIMEOUT, || false) {
        Ok(store) => Ok(store),
        Err(StoreError::StateRejected { status }) => {
            let hint = hint_for(&status, project_root);
            Err(unusable(
                project_root,
                format!("its state is {status}"),
                hint,
            ))
        }
        Err(err) => {
            let hint = hint_for(&status, project_root);
            Err(unusable(project_root, err, hint))
        }
    }
}
