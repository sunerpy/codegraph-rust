//! What a Dart name reads of other files' directives (W4-DART-02,
//! [`codegraph_resolve::dart_libraries`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A receiver-less Dart name reaches what its library declares and what the
//! libraries it imports export (upstream #2386), an unnamed extension applies
//! in every file of its library, and an `import`, `export` or `part` links the
//! file its URI names (#2388, #2391). The resolver reads each file's
//! directives from its head, and which packages hold Dart files from the
//! pubspec.yaml files above them: text that is gone once a file changes on
//! disk, so every full index and migration writes a row per Dart file that
//! has a directive ([`LibraryRow`]). A sync compares a changed file's row with
//! its new text:
//!
//! - a change to what makes up a library or what it exports (`library`,
//!   `part`, `part of`, `export`), a Dart file added or removed that has one
//!   of those, or a Dart file that gives a package its first Dart file or
//!   takes its last, re-resolves every Dart file: an importer of any library
//!   on an export chain through it can see other names;
//! - a change to a file's imports alone re-resolves the files whose scope
//!   takes its imports, the parts below it (the file itself re-resolves as
//!   every changed file does);
//! - a Dart file added, the parked Dart imports whose URI ends in its name
//!   are retried at their sites, however many there are (the shared
//!   import-key lookup caps them).
//!
//! A pubspec.yaml change is W5-06's control-file lane.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`], or with a row it cannot read, re-resolves every
//! Dart file once and rebuilds the rows.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{EdgeKind, Language};
use codegraph_resolve::ResolutionContext;
use codegraph_resolve::dart_libraries::{
    DartDirectives, DartLibraries, LIBRARY_ROW_KEY_PREFIX, LibraryRow, NameFilter,
    is_dart_library_decl, library_row_key,
};
use codegraph_store::Store;
use codegraph_store::queries::{FileReferenceSite, ReferenceSite};

/// Metadata key present when no sync left a Dart dependent unrefreshed.
pub const VERSION_KEY: &str = "dart_libraries_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";

/// `source`'s library row, `None` when it records nothing: a file that is no
/// Dart file, has no directive, or is not text.
pub fn row_of(language: Language, source: &SourceText) -> Option<LibraryRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str) -> Option<LibraryRow> {
    if language != Language::Dart {
        return None;
    }
    let row = LibraryRow::of(text);
    (!row.is_empty()).then_some(row)
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, LibraryRow)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&library_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every Dart file re-resolved and its rows
/// rebuilt: its marker is missing or at another version, or a row cannot be
/// read (failing closed even in a sync with nothing changed).
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    if store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION) {
        return Ok(true);
    }
    Ok(store
        .project_metadata_with_prefix(LIBRARY_ROW_KEY_PREFIX)?
        .iter()
        .any(|(_, value)| LibraryRow::decode(value).is_none()))
}

/// Record that every Dart dependent is current: a full index, a migration,
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

fn read_row(store: &Store, path: &str) -> Result<Option<LibraryRow>> {
    Ok(store
        .get_project_metadata(&library_row_key(path))?
        .and_then(|value| LibraryRow::decode(&value)))
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<LibraryRow>,
    new_row: Option<LibraryRow>,
}

/// What a sync's changed files held for the Dart library lane.
#[derive(Debug, Default)]
pub(crate) struct DartLibraryChanges {
    changes: BTreeMap<String, Change>,
}

impl DartLibraryChanges {
    /// `path` changed (its old row still in the store) to `new` (its
    /// language and text), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        new: Option<(Language, &SourceText)>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let new_row = new.and_then(|(language, source)| row_of(language, source));
        if old_row.is_none() && new_row.is_none() {
            return Ok(());
        }
        // A file changed twice in one sync keeps what it held before the
        // first change.
        let change = self
            .changes
            .entry(path.to_string())
            .or_insert_with(|| Change {
                old_row,
                ..Change::default()
            });
        change.new_row = new_row;
        Ok(())
    }
}

/// What a sync's Dart library lane calls for.
#[derive(Debug, Default)]
pub(crate) struct DartLibraryRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names whose reach moved, for the name lane.
    pub(crate) names: BTreeSet<String>,
    /// Parked imports in unchanged files to retry at their sites.
    pub(crate) sites: Vec<FileReferenceSite>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<LibraryRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl DartLibraryRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(LIBRARY_ROW_KEY_PREFIX)?;
        }
        for (written, (path, row)) in self.rows.iter().enumerate() {
            #[cfg(test)]
            if self.rebuilt
                && written == 1
                && crate::sync::tests::FAIL_AFTER_DART_LIBRARY_ROW_PURGE.with(std::cell::Cell::get)
            {
                anyhow::bail!("injected failure in a Dart library row rebuild");
            }
            #[cfg(not(test))]
            let _ = written;
            let key = library_row_key(path);
            match row {
                Some(row) if !row.is_empty() => {
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

/// Whether a project-relative path is a Dart file, as the resolver counts
/// one for its packages.
fn is_dart_path(path: &str) -> bool {
    path.ends_with(".dart")
}

/// The last segment of an import's URI: the file name of every project file
/// it can name.
fn uri_file_name(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or(uri)
}

/// Read what the Dart library lane calls for, after every changed file is
/// re-extracted (the store holds the new files and nodes; the rows are still
/// the old ones). With `bootstrap`, every row is rebuilt from disk instead
/// and every Dart file re-resolves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dart_library_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &DartLibraryChanges,
    heads: &crate::dart_heads::DartChanges,
    added: &BTreeSet<String>,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<DartLibraryRefresh> {
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        return rebuild(store, project_root, max_file_size, changes, &unchanged);
    }
    let mut refresh = DartLibraryRefresh::default();
    for (path, change) in &changes.changes {
        refresh.rows.insert(path.clone(), change.new_row.clone());
    }
    let added_dart: BTreeSet<&str> = added
        .iter()
        .map(String::as_str)
        .filter(|path| is_dart_path(path))
        .collect();
    let removed_dart: BTreeSet<&str> = removed
        .iter()
        .map(String::as_str)
        .filter(|path| is_dart_path(path))
        .collect();
    let rows_moved = changes
        .changes
        .values()
        .any(|change| change.old_row != change.new_row);
    if rows_moved || !added_dart.is_empty() || !removed_dart.is_empty() {
        let mut stored: HashMap<String, DartDirectives> = HashMap::new();
        for (key, value) in store.project_metadata_with_prefix(LIBRARY_ROW_KEY_PREFIX)? {
            let Some(path) = key.strip_prefix(LIBRARY_ROW_KEY_PREFIX) else {
                continue;
            };
            if changes.changes.contains_key(path) {
                continue;
            }
            let Some(row) = LibraryRow::decode(&value) else {
                return rebuild(store, project_root, max_file_size, changes, &unchanged);
            };
            stored.insert(path.to_string(), row.directives());
        }
        let after_files: Vec<String> = store
            .all_files()?
            .into_iter()
            .filter(|file| file.language == Language::Dart)
            .map(|file| file.path)
            .collect();
        let before_files: Vec<String> = after_files
            .iter()
            .filter(|path| !added_dart.contains(path.as_str()))
            .cloned()
            .chain(removed_dart.iter().map(|path| path.to_string()))
            .collect();
        let mut before_directives = stored.clone();
        let mut after_directives = stored;
        for (path, change) in &changes.changes {
            if let Some(row) = &change.old_row {
                before_directives.insert(path.clone(), row.directives());
            }
            if let Some(row) = &change.new_row {
                after_directives.insert(path.clone(), row.directives());
            }
        }
        let context =
            codegraph_resolve::StoreResolutionContext::new(store, project_root.to_string_lossy())
                .with_max_file_size(max_file_size);
        let before = DartLibraries::of_state(before_files.clone(), before_directives);
        let after = DartLibraries::of_state(after_files.clone(), after_directives);
        // A file whose scope, imported libraries or directive targets moved
        // re-resolves whole: what its names can see changed, however many
        // names that is.
        for path in after_files.iter().filter(|path| unchanged(path)) {
            if before.structure_of(path, &context) != after.structure_of(path, &context) {
                refresh.files.insert(path.clone());
            }
        }
        // A library whose namespace moved changes what every importer, and
        // every file of the library, sees of the files in it: the names whose
        // reach moved are touched.
        let libraries: BTreeSet<String> = before_files
            .iter()
            .filter_map(|path| before.library_of_file(path, &context))
            .chain(
                after_files
                    .iter()
                    .filter_map(|path| after.library_of_file(path, &context)),
            )
            .collect();
        // Each library apart: a name can leave one library's reach while
        // another brings it in.
        let mut moved: Vec<(String, Vec<NameFilter>, Vec<NameFilter>)> = Vec::new();
        for library in &libraries {
            let (old, new) = (
                before.namespace_of(library, &context),
                after.namespace_of(library, &context),
            );
            if old == new {
                continue;
            }
            let files: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
            for file in files {
                let (from, to) = (old.get(file), new.get(file));
                if from != to {
                    moved.push((
                        file.clone(),
                        from.cloned().unwrap_or_default(),
                        to.cloned().unwrap_or_default(),
                    ));
                }
            }
        }
        let mut nodes_of: HashMap<String, Vec<codegraph_core::types::Node>> = HashMap::new();
        for (file, from, to) in &moved {
            if !nodes_of.contains_key(file) {
                nodes_of.insert(file.clone(), store.nodes_by_file_path(file)?);
            }
            for node in &nodes_of[file] {
                if node.language != Language::Dart {
                    continue;
                }
                // An unnamed extension's members apply in its library's files.
                if node.qualified_name.starts_with("<anonymous>::") {
                    refresh.names.insert(node.name.clone());
                    continue;
                }
                if !is_dart_library_decl(node) {
                    continue;
                }
                let reach = |filters: &[NameFilter]| filters.iter().any(|f| f.lets(&node.name));
                if reach(from) != reach(to) {
                    refresh.names.insert(node.name.clone());
                }
            }
        }
        // A subclass whose `extends` / `implements` names a type its file now
        // sees otherwise gains or loses that type's members, so a call on it
        // elsewhere moves without any name of it changing (bloc's `export
        // 'src/cubit.dart' hide Cubit` moved its tests' `cubit.close()`): the
        // names of the members reachable through such a type are touched.
        let mut types: BTreeSet<String> = refresh
            .names
            .iter()
            .filter(|name| {
                context.get_nodes_by_name_shared(name).iter().any(|node| {
                    is_dart_library_decl(node)
                        && codegraph_resolve::dart_scope::is_dart_type_kind(node.kind)
                })
            })
            .cloned()
            .collect();
        for file in &refresh.files {
            types.extend(store.inheritance_ref_names_in_file(file)?);
        }
        let mut through: BTreeSet<String> = BTreeSet::new();
        for name in &types {
            let decls: Vec<String> = context
                .get_nodes_by_name_shared(name)
                .iter()
                .filter(|node| {
                    is_dart_library_decl(node)
                        && codegraph_resolve::dart_scope::is_dart_type_kind(node.kind)
                })
                .map(|node| node.file_path.clone())
                .collect();
            let moved = store
                .files_with_inheritance_refs_to(Language::Dart, name)?
                .iter()
                .any(|file| {
                    decls.iter().any(|decl| {
                        before.reach_of(file, decl, name, &context)
                            != after.reach_of(file, decl, name, &context)
                    })
                });
            if moved {
                through.insert(name.clone());
            }
        }
        let members = crate::dart_heads::member_names_through(store, heads, &through)?;
        // A call written with a receiver waits for its member's name, which
        // the shared tail lookup caps at 500 per tail: the parked Dart ones
        // to such a name are retried at their sites, however many there are.
        if !members.is_empty() {
            let list: Vec<String> = members.iter().cloned().collect();
            for reference in store.unresolved_refs_of_language_by_members(Language::Dart, &list)? {
                if unchanged(&reference.file_path) && !refresh.files.contains(&reference.file_path)
                {
                    refresh.sites.push(FileReferenceSite {
                        file_path: reference.file_path,
                        site: Some(ReferenceSite {
                            from_node_id: reference.from_node_id,
                            line: reference.line,
                            col: reference.col,
                        }),
                    });
                }
            }
        }
        refresh.names.extend(members);
    }
    // An import or part of a file that appears waits for it under its URI,
    // which no node's name matches.
    if !added_dart.is_empty() {
        let names: BTreeSet<&str> = added_dart.iter().map(|path| uri_file_name(path)).collect();
        for reference in store.unresolved_refs_of_kind(EdgeKind::Imports)? {
            if reference.language == Language::Dart
                && unchanged(&reference.file_path)
                && !refresh.files.contains(&reference.file_path)
                && names.contains(uri_file_name(&reference.reference_name))
            {
                refresh.sites.push(FileReferenceSite {
                    file_path: reference.file_path,
                    site: Some(ReferenceSite {
                        from_node_id: reference.from_node_id,
                        line: reference.line,
                        col: reference.col,
                    }),
                });
            }
        }
    }
    Ok(refresh)
}

/// Every Dart file re-resolves and every row is rebuilt from disk (a changed
/// file's from its new text).
fn rebuild(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &DartLibraryChanges,
    unchanged: &dyn Fn(&str) -> bool,
) -> Result<DartLibraryRefresh> {
    let mut refresh = DartLibraryRefresh {
        rebuilt: true,
        ..DartLibraryRefresh::default()
    };
    for file in store.all_files()? {
        if file.language != Language::Dart {
            continue;
        }
        let row = match changes.changes.get(&file.path) {
            Some(change) => change.new_row.clone(),
            None => read_text(project_root, &file.path, max_file_size)
                .and_then(|text| row_of_text(file.language, &text)),
        };
        if unchanged(&file.path) {
            refresh.files.insert(file.path.clone());
        }
        refresh.rows.insert(file.path, row);
    }
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row records the directives the resolver reads, and reads back as
    /// them.
    #[test]
    fn a_row_reads_back_as_the_directives_it_records() {
        let text =
            "library a.b;\nimport 'x.dart' as x hide y;\nexport 'z.dart' show w;\npart 'p.dart';\n";
        let row = LibraryRow::of(text);
        assert_eq!(LibraryRow::decode(&row.encode()), Some(row.clone()));
        assert_eq!(
            row.directives(),
            codegraph_resolve::dart_libraries::read_dart_directives(text)
        );
        let part = LibraryRow::of("part of 'lib.dart';\n");
        assert_eq!(
            part.directives(),
            codegraph_resolve::dart_libraries::read_dart_directives("part of 'lib.dart';\n")
        );
        assert!(LibraryRow::of("void main() {}\n").is_empty());
        assert_eq!(LibraryRow::decode("{\"unknown\":1}"), None);
    }

    #[test]
    fn an_import_names_a_file_by_its_last_segment() {
        assert_eq!(uri_file_name("package:app/src/x.dart"), "x.dart");
        assert_eq!(uri_file_name("../x.dart"), "x.dart");
        assert_eq!(uri_file_name("x.dart"), "x.dart");
    }
}
