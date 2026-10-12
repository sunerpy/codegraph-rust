//! The module-surface rows of a project's JavaScript-family files
//! ([`codegraph_resolve::module_surface`], W4a-F3).
//!
//! Other files' resolution reads a JS-family module's own text (its
//! re-exports, export aliases and lists, CommonJS default, sealed-ness,
//! component scripts), which no node records and which is gone once the file
//! changes on disk. Every full index and migration writes a row for each file
//! whose surface is not the default one, holding its digest, the specifiers it
//! forwards and what they resolved to, and the version marker, once the file
//! rows are stored and before resolution.
//!
//! A sync compares each changed file's row with its new surface. A file whose
//! digest moved, and every file added or removed, seeds an upward closure
//! over the barrels that forward it (by the targets their rows recorded, or by
//! what their sources resolve to now); every direct dependent of the closure
//! re-resolves whole, and the names the moved files declare are touched for
//! the name matcher. An index without the marker was built before the rows
//! existed and may hold edges an older sync left stale: the sync reconciles
//! every JS-family file once and writes the marker last.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::SourceText;
use codegraph_core::types::{Language, Node};
use codegraph_resolve::StoreResolutionContext;
use codegraph_resolve::module_surface::{
    self, ModuleSurface, ROW_KEY_PREFIX, SurfaceRow, VERSION, VERSION_KEY, has_module_surface,
};
use codegraph_store::Store;

/// `source`'s surface, as an index computes it from the text it extracts.
/// An MPEG transport stream named `.ts` is no source and has none.
pub fn surface_of(
    relative: &str,
    language: Language,
    source: &SourceText,
) -> Option<ModuleSurface> {
    match source {
        SourceText::Text(text) => module_surface::module_surface(relative, language, Some(text)),
        SourceText::Oversize(_) => module_surface::module_surface(relative, language, None),
        SourceText::MpegTransportStream => None,
    }
}

/// Write the rows of a full index's `surfaces` (by path) and the version
/// marker, resolving each surface's sources in the store as it stands. Call
/// once every file row is stored and before resolution starts.
pub fn write_index_rows(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    surfaces: &[(String, ModuleSurface)],
) -> Result<()> {
    let context = StoreResolutionContext::new(store, project_root.to_string_lossy())
        .with_max_file_size(max_file_size);
    for (path, surface) in surfaces {
        let row = SurfaceRow::resolved(path, surface, &context);
        store.set_project_metadata(&module_surface::row_key(path), &row.encode())?;
    }
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Whether the index holds no trustworthy rows: it was built before them, or
/// by a build that wrote another row format.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that bootstrap finished: every row is current. Called last, once
/// the sync's resolution is persisted.
pub(crate) fn finish_bootstrap(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// What a file's row said before a change.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum OldRow {
    /// No row: the default surface.
    #[default]
    Default,
    Row(SurfaceRow),
    /// A row this build cannot read, which says nothing to trust.
    Unreadable,
}

impl OldRow {
    fn digest(&self) -> Option<&str> {
        match self {
            Self::Row(row) => Some(&row.digest),
            Self::Default | Self::Unreadable => None,
        }
    }

    fn targets(&self) -> &[String] {
        match self {
            Self::Row(row) => &row.targets,
            Self::Default | Self::Unreadable => &[],
        }
    }
}

/// `relative`'s row as the store holds it.
pub(crate) fn old_row(store: &Store, relative: &str) -> Result<OldRow> {
    Ok(
        match store.get_project_metadata(&module_surface::row_key(relative))? {
            None => OldRow::Default,
            Some(value) => SurfaceRow::decode(&value).map_or(OldRow::Unreadable, OldRow::Row),
        },
    )
}

/// What a sync recorded of one changed JS-family file.
#[derive(Debug, Default)]
pub(crate) struct SurfaceChange {
    pub(crate) old: OldRow,
    /// Its surface after the change; `None` for the default one, or when the
    /// file was removed.
    pub(crate) new: Option<ModuleSurface>,
    pub(crate) added: bool,
    pub(crate) removed: bool,
    /// The files with an edge into it before the change (gone with its nodes
    /// by the time the closure is read).
    pub(crate) importers: BTreeSet<String>,
    /// The names of its nodes before and after the change.
    pub(crate) names: BTreeSet<String>,
}

impl SurfaceChange {
    /// Note the nodes it held before or holds after the change.
    pub(crate) fn note_nodes<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Node>) {
        self.names
            .extend(nodes.into_iter().map(|node| node.name.clone()));
    }

    /// Whether its surface moved, or it came or went.
    fn seeds(&self) -> bool {
        self.added
            || self.removed
            || self.old == OldRow::Unreadable
            || self.old.digest() != self.new.as_ref().map(|surface| surface.digest.as_str())
    }
}

/// What the surfaces of a sync's changed files call for.
#[derive(Debug, Default)]
pub(crate) struct SurfaceRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names to touch: the nodes of each file whose surface moved.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None` deletes).
    rows: BTreeMap<String, Option<SurfaceRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl SurfaceRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(ROW_KEY_PREFIX)?;
        }
        for (path, row) in &self.rows {
            let key = module_surface::row_key(path);
            match row {
                Some(row) => {
                    store.set_project_metadata(&key, &row.encode())?;
                }
                None if !self.rebuilt => {
                    store.delete_project_metadata(&key)?;
                }
                None => {}
            }
        }
        Ok(())
    }
}

/// A file that forwards other modules, for the closure.
struct Barrel {
    path: String,
    /// What its sources resolved to when its row was written.
    recorded: Vec<String>,
    /// What they resolve to now.
    now: Vec<String>,
}

/// Read what the surfaces of a sync's changed files call for, after every
/// changed file is re-extracted (the store holds the new file set) and
/// before the dependents are refreshed. `file_set_changed` says the sync
/// added or removed a file, which can move what any barrel's sources resolve
/// to. With `bootstrap`, every indexed JS-family file is reconciled instead.
pub(crate) fn note_surface_changes(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &BTreeMap<String, SurfaceChange>,
    reindexed: &HashSet<String>,
    file_set_changed: bool,
    bootstrap: bool,
) -> Result<SurfaceRefresh> {
    let context = StoreResolutionContext::new(store, project_root.to_string_lossy())
        .with_max_file_size(max_file_size);
    let mut refresh = SurfaceRefresh::default();
    if bootstrap {
        refresh.rebuilt = true;
        for file in store.all_files()? {
            if !has_module_surface(&file.path, file.language) {
                continue;
            }
            let surface = match changes.get(&file.path) {
                Some(change) => change.new.clone(),
                None => {
                    let full = project_root.join(&file.path);
                    match codegraph_core::source_file::read_source_file(
                        &full,
                        &file.path,
                        max_file_size,
                    ) {
                        Ok((_, source)) => surface_of(&file.path, file.language, &source),
                        // Gone or unreadable since it was indexed: its own
                        // sync removes it, and its row with it.
                        Err(_) => None,
                    }
                }
            };
            if !reindexed.contains(&file.path) {
                refresh.files.insert(file.path.clone());
            }
            let row = surface.map(|surface| SurfaceRow::resolved(&file.path, &surface, &context));
            refresh.rows.insert(file.path, row);
        }
        return Ok(refresh);
    }

    // Seeds: the changed files whose surface moved, and the files that came
    // or went. A sync that moved no surface and kept the file set (a body
    // edit) rewrites nothing: every row still says what it said.
    let mut closure: BTreeSet<String> = BTreeSet::new();
    for (path, change) in changes {
        if change.seeds() {
            closure.insert(path.clone());
            if !change.added && !change.removed {
                refresh.names.extend(change.names.iter().cloned());
            }
        }
    }
    if closure.is_empty() && !file_set_changed {
        return Ok(refresh);
    }
    let mut barrels: Vec<Barrel> = Vec::new();
    for (path, change) in changes {
        let row = change
            .new
            .as_ref()
            .map(|surface| SurfaceRow::resolved(path, surface, &context));
        let now = row
            .as_ref()
            .map(|row| row.targets.clone())
            .unwrap_or_default();
        if !change.old.targets().is_empty() || !now.is_empty() {
            barrels.push(Barrel {
                path: path.clone(),
                recorded: change.old.targets().to_vec(),
                now,
            });
        }
        refresh.rows.insert(path.clone(), row);
    }
    // The unchanged barrels: what their rows recorded, and, when the file set
    // moved, what their sources resolve to now. A barrel whose targets moved
    // seeds the closure too and has its row rewritten; one whose row cannot
    // be read is reconciled from disk.
    for (key, value) in store.project_metadata_with_prefix(ROW_KEY_PREFIX)? {
        let path = key[ROW_KEY_PREFIX.len()..].to_string();
        if changes.contains_key(&path) {
            continue;
        }
        let Some(row) = SurfaceRow::decode(&value) else {
            closure.insert(path.clone());
            let file = store.file_by_path(&path)?;
            let surface = file.and_then(|file| {
                let full = project_root.join(&path);
                codegraph_core::source_file::read_source_file(&full, &path, max_file_size)
                    .ok()
                    .and_then(|(_, source)| surface_of(&path, file.language, &source))
            });
            refresh.rows.insert(
                path.clone(),
                surface.map(|surface| SurfaceRow::resolved(&path, &surface, &context)),
            );
            continue;
        };
        if row.sources.is_empty() {
            continue;
        }
        let now = if file_set_changed {
            module_surface::resolve_targets(&path, &row.sources, &context)
        } else {
            row.targets.clone()
        };
        if now != row.targets {
            closure.insert(path.clone());
            refresh.rows.insert(
                path.clone(),
                Some(SurfaceRow {
                    targets: now.clone(),
                    ..row.clone()
                }),
            );
        }
        barrels.push(Barrel {
            path,
            recorded: row.targets,
            now,
        });
    }
    // Up the barrels that forward a member, by what they recorded or resolve
    // to now: a deleted or renamed member is only found the first way.
    loop {
        let before = closure.len();
        for barrel in &barrels {
            if !closure.contains(&barrel.path)
                && barrel
                    .recorded
                    .iter()
                    .chain(&barrel.now)
                    .any(|target| closure.contains(target))
            {
                closure.insert(barrel.path.clone());
            }
        }
        if closure.len() == before {
            break;
        }
    }
    // Every direct dependent of the closure re-resolves whole.
    for member in &closure {
        let importers = match changes.get(member) {
            Some(change) => change.importers.clone(),
            None => store
                .reference_sites_dependent_on_file(member)?
                .into_iter()
                .map(|site| site.file_path)
                .collect(),
        };
        refresh.files.extend(
            importers
                .into_iter()
                .filter(|importer| !reindexed.contains(importer)),
        );
    }
    Ok(refresh)
}
