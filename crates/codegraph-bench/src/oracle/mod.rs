pub mod canonicalize;
pub mod diff;
pub mod golden;

use std::path::Path;

use anyhow::Result;

pub use canonicalize::{CanonicalDb, CanonicalRow, canonicalize_db};

pub use diff::{DiffEntry, DiffError, KnownDiffs, Tier, diff_canonical};
pub use golden::{load_golden, write_golden};

pub fn assert_equivalent(rust_db: &Path, golden_dir: &Path) -> Result<()> {
    assert_equivalent_with_known_diffs(rust_db, golden_dir, &KnownDiffs::repo_doc_path())
}

/// The allowlist is loaded from `known_diffs_path` BEFORE any comparison, so an
/// unparseable `KNOWN_DIFFS.md` fails the assertion instead of being ignored.
pub fn assert_equivalent_with_known_diffs(
    rust_db: &Path,
    golden_dir: &Path,
    known_diffs_path: &Path,
) -> Result<()> {
    let known_diffs = KnownDiffs::load(known_diffs_path)?;
    let expected = load_golden(golden_dir)?;
    let actual = canonicalize_db(rust_db)?;
    diff_canonical(&expected, &actual, Some(&known_diffs))?;
    Ok(())
}
