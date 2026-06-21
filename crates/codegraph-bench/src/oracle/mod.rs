pub mod canonicalize;
pub mod diff;
pub mod golden;

use std::path::Path;

use anyhow::Result;

pub use canonicalize::{canonicalize_db, CanonicalDb, CanonicalRow};
pub use diff::{diff_canonical, DiffEntry, DiffError, KnownDiffs, Tier};
pub use golden::{load_golden, write_golden};

pub fn assert_equivalent(rust_db: &Path, golden_dir: &Path) -> Result<()> {
    let expected = load_golden(golden_dir)?;
    let actual = canonicalize_db(rust_db)?;
    diff_canonical(&expected, &actual, None)?;
    Ok(())
}
