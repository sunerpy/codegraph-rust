//! The Terraform wiring lane of a sync, its rows and its crash marker
//! (W7-24 cost, OD-B73; KEEP-RUST).
//!
//! A module block's `:`-scoped references (`module.M:file`, `module.M:var.X`,
//! `module.M:output.O`, `module.M:remote-output.O`) resolve through what no
//! node records and no reference's name matches
//! ([`codegraph_resolve::frameworks::terraform`]): a `module` block's
//! `source`, `component` and `component = var.X` lines, a variable's
//! `default` line, the module, variable and output names a directory
//! declares, and the `.tf` and `.tofu` files it holds. W7-24c re-resolved
//! whole every file holding such a reference on every sync that touched a
//! Terraform file, which on a module-heavy project is nearly every reference
//! (6876 of tf-cloudposse's 7000 for a comment line).
//!
//! Every full index and migration writes a row per Terraform file declaring
//! a module, a variable or an output (`terraform_wiring:{path}`,
//! [`WiringRow`]): those declarations in the order the file holds them, each
//! with what the bridge reads of its lines. A sync compares each changed
//! Terraform file's row before the change with its row after, and
//! re-resolves those files whole, as W7-24c did, only when a row moved, a
//! `.tf` or `.tofu` file came or went or gained or lost its file node, or
//! the marker is missing. A target that moved to another line is the
//! generic dependents' work: an edge into a node that went is re-resolved at
//! its site.
//!
//! The rows are written last, with the other rows. A sync withdraws
//! [`VERSION_KEY`] before its first change and writes it back once
//! everything resolved; an index without it re-resolves every such file
//! once and rebuilds the rows.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
pub use codegraph_resolve::frameworks::terraform::{
    WIRING_ROW_KEY_PREFIX as ROW_KEY_PREFIX, WiringEntry, WiringRow, wiring_row,
    wiring_row_key as row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a module bridge unrewired.
pub const VERSION_KEY: &str = "terraform_wiring_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// Whether `path` is a Terraform source file (`.tf`, `.tfvars`, `.tofu`).
fn is_terraform_path(path: &str) -> bool {
    path.ends_with(".tf") || path.ends_with(".tfvars") || path.ends_with(".tofu")
}

/// Whether `path` is a file a module's `:file` entry is chosen among.
pub(crate) fn is_module_entry_candidate(path: &str) -> bool {
    path.ends_with(".tf") || path.ends_with(".tofu")
}

/// `source`'s row, `None` when it records nothing: a file of another
/// language, one declaring no module, variable or output, or one that is
/// not text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<WiringRow> {
    if language != Language::Terraform {
        return None;
    }
    match source {
        SourceText::Text(text) => wiring_row(text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, WiringRow)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every file holding a `module.M:` reference
/// re-resolved and its rows rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every module bridge is current: a full index, a migration,
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

fn read_row(store: &Store, path: &str) -> Result<Option<WiringRow>> {
    Ok(store
        .get_project_metadata(&row_key(path))?
        .and_then(|value| WiringRow::decode(&value)))
}

/// What one sync recorded of a changed Terraform file: its row before the
/// first change and after the last.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<WiringRow>,
    new_row: Option<WiringRow>,
}

/// What a sync's changed Terraform files held for this lane.
#[derive(Debug, Default)]
pub(crate) struct WiringChanges {
    changes: BTreeMap<String, Change>,
}

impl WiringChanges {
    /// `path` changed (its old row still in the store) to `new` (its
    /// language, text and nodes), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        if !is_terraform_path(path) {
            return Ok(());
        }
        let new_row = new.and_then(|(language, source, nodes)| row_of(language, source, nodes));
        if let Some(change) = self.changes.get_mut(path) {
            // A file changed twice in one sync keeps what it held before the
            // first change.
            change.new_row = new_row;
            return Ok(());
        }
        let old_row = read_row(store, path)?;
        self.changes
            .insert(path.to_string(), Change { old_row, new_row });
        Ok(())
    }

    /// Whether a changed file's row moved: a declaration came, went, moved
    /// among the file's others, or reads another `source`, `component`,
    /// `component = var.X` or `default`.
    fn moved(&self) -> bool {
        self.changes
            .values()
            .any(|change| change.old_row != change.new_row)
    }
}

/// What a sync's Terraform wiring lane calls for.
#[derive(Debug, Default)]
pub(crate) struct WiringRefresh {
    /// Whether every file holding a `module.M:` reference re-resolves whole.
    pub(crate) rewire: bool,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<WiringRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl WiringRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = row_key(path);
            match row {
                Some(row) if !row.entries.is_empty() => {
                    store.set_project_metadata(&key, &row.encode())?;
                }
                _ if !self.rebuilt => {
                    store.delete_project_metadata(&key)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn read_text(project_root: &Path, path: &str, max_file_size: u64) -> Option<String> {
    match read_source_file(&project_root.join(path), path, max_file_size) {
        Ok((_, SourceText::Text(text))) => Some(text),
        _ => None,
    }
}

/// The lane's work for one sync, once the paths are processed:
/// `entry_files_moved` is whether a `.tf` or `.tofu` file came or went or
/// gained or lost its file node. Without the marker, every Terraform file's
/// row is rebuilt from its stored nodes and its text, and every file holding
/// a `module.M:` reference re-resolves once.
pub(crate) fn wiring_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &WiringChanges,
    entry_files_moved: bool,
    bootstrap: bool,
) -> Result<WiringRefresh> {
    if bootstrap {
        let mut rows = BTreeMap::new();
        for path in store.file_paths_of_language(Language::Terraform)? {
            let nodes = store.nodes_by_file_path(&path)?;
            let row = read_text(project_root, &path, max_file_size)
                .and_then(|text| wiring_row(&text, &nodes));
            rows.insert(path, row);
        }
        return Ok(WiringRefresh {
            rewire: true,
            rows,
            rebuilt: true,
        });
    }
    let rows = changes
        .changes
        .iter()
        .map(|(path, change)| (path.clone(), change.new_row.clone()))
        .collect();
    Ok(WiringRefresh {
        rewire: entry_files_moved || changes.moved(),
        rows,
        rebuilt: false,
    })
}
