//! The crash marker of the case-folded retries (W4-PHP-03).
//!
//! PHP, Pascal and CFML names ignore case, so a sync retries their parked
//! references by a touched name in any case and re-resolves the edges to a
//! touched name's namesakes in other cases. Those retries live in one
//! sync's memory: a sync withdraws [`VERSION_KEY`] before its first change
//! and writes it back once everything resolved, and an index without it
//! re-resolves every file of those languages once.

use std::collections::{BTreeSet, HashSet};

use anyhow::Result;
use codegraph_store::Store;

/// Metadata key present when no sync left a case-folded retry undone.
pub const VERSION_KEY: &str = "case_folded_names_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether this sync must re-resolve every PHP, Pascal and CFML file.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that no case-folded retry is pending: a full index, a migration,
/// or a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// Every indexed file of a language whose names ignore case, but the ones
/// this sync re-indexed or removed.
pub(crate) fn every_case_insensitive_file(
    store: &Store,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    Ok(store
        .all_files()?
        .into_iter()
        .filter(|file| {
            codegraph_resolve::CASE_INSENSITIVE_LANGUAGES.contains(&file.language)
                && !reindexed.contains(&file.path)
                && !removed.contains(&file.path)
        })
        .map(|file| file.path)
        .collect())
}
