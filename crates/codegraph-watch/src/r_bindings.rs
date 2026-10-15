//! The crash marker of the R base-binding names (W4-R-01).
//!
//! An R call of a base function's name skips a data binding of it unless the
//! binding's line holds a function (upstream #2239), and that line is read
//! from source, which no node records: a changed R file touches the names it
//! binds that way. Those names live in one sync's memory, so a sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once everything
//! resolved, and an index without it re-resolves every R file once.

use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::Language;
use codegraph_store::Store;

/// Metadata key present when no sync left an R base-binding name untouched.
pub const VERSION_KEY: &str = "r_base_bindings_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every R file.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no R base-binding name is pending: a full index, a
/// migration, or a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// Every indexed R file, but the ones this sync re-indexed or removed.
pub(crate) fn every_r_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .all_files()?
        .into_iter()
        .filter(|file| {
            file.language == Language::R
                && !reindexed.contains(&file.path)
                && !removed.contains(&file.path)
        })
        .map(|file| file.path)
        .collect())
}
