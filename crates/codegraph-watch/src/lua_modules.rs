//! What a Lua call reads of other files (W4-LUA-01,
//! [`codegraph_resolve::lua_scope`]), so that a sync re-resolves an
//! unchanged file as a fresh index would.
//!
//! A Lua `local` is invisible outside its file (upstream #2143), and a bare
//! call through a `local` alias reaches what the alias names in a module the
//! file `require`s, through what that module hands out under the name or
//! re-exports from a module of its own, a few hops deep (#2188). Locality
//! and exports are read from the files' text, so every full index and
//! migration writes a row per Lua file ([`LuaModuleRow`]): its `local`
//! declarations, the modules its aliases `require`, and a digest of what an
//! alias through it reads. A sync compares a changed file's row with its new
//! text:
//!
//! - the name of a declaration that became or stopped being `local` is
//!   touched for the name lane: another file's call of it gains or loses it;
//! - a file whose digest changed, or that came or went, is a changed module:
//!   the unchanged Lua files whose aliases `require` it re-resolve whole, and
//!   so do the files requiring those, as deep as an alias is followed
//!   ([`DEPENDENT_DEPTH`]), through changed files too (what they read before
//!   the change and after it);
//! - an unchanged file whose aliases or exports read a touched name through
//!   a global table (`local snooze = ngx.sleep` reads every node named
//!   `sleep`, though its calls are named `snooze`) re-resolves whole, and is
//!   a changed module for the files requiring it.
//!
//! The rows are written last, with the other rows. An index without the
//! marker at [`VERSION`], or with a row it cannot read, re-resolves every Lua
//! file once and rebuilds the rows.
//!
//! The project's custom Lua module loaders and module roots (#1617,
//! [`codegraph_extract::lua_loaders`]) are recorded beside them under
//! [`LOADERS_KEY`], in their canonical form, by every full index, migration
//! and persisted sync, a project naming none included. A sync compares the
//! record with the settings it runs under before its first change
//! ([`loaders_change`]): when the loader names differ, or the record is
//! missing or unreadable, every Lua and Luau file is extracted again, since
//! a loader call is an import only under the settings that name it. The
//! record is withdrawn before anything changes, so a sync stopping partway
//! leaves the next one to extract them all again.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use codegraph_core::source_file::{SourceText, read_source_file};
use codegraph_core::types::{Language, Node};
use codegraph_extract::LuaLoaders;
use codegraph_resolve::lua_scope::{
    LocalDecl, LuaModuleRow, MODULE_ROW_KEY_PREFIX, lua_module_may_be, module_row_key,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a Lua dependent unrefreshed.
pub const VERSION_KEY: &str = "lua_modules_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";
/// Metadata key of the Lua loader settings the index was extracted and
/// resolved under, as [`LuaLoaders::canonical_json`] writes them.
pub const LOADERS_KEY: &str = "lua_loaders";

/// How the Lua loader settings a sync runs under differ from the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadersChange {
    /// The record holds these settings.
    Same,
    /// The record is missing or unreadable: a sync stopped partway, or an
    /// older build made the index. Every Lua file is extracted again.
    Bootstrap,
    /// The loader names differ (the roots may too): every Lua file is
    /// extracted again.
    Loaders,
    /// Only the module roots differ, their order included.
    Roots,
}

/// How `current` differs from the settings `store` records, read before a
/// sync's first change.
pub(crate) fn loaders_change(store: &Store, current: &LuaLoaders) -> Result<LoadersChange> {
    let Some(recorded) = store.get_project_metadata(LOADERS_KEY)? else {
        return Ok(LoadersChange::Bootstrap);
    };
    if recorded == current.canonical_json() {
        return Ok(LoadersChange::Same);
    }
    Ok(match LuaLoaders::from_canonical_json(&recorded) {
        None => LoadersChange::Bootstrap,
        Some(recorded) if recorded.loader_functions() != current.loader_functions() => {
            LoadersChange::Loaders
        }
        Some(_) => LoadersChange::Roots,
    })
}

/// Record `current` as the settings the index is extracted and resolved
/// under: a full index, a migration, or a sync whose resolution is
/// persisted.
pub fn write_loaders(store: &Store, current: &LuaLoaders) -> Result<()> {
    store.set_project_metadata(LOADERS_KEY, &current.canonical_json())?;
    Ok(())
}

/// Withdraw the record before a sync's first change for new settings.
pub(crate) fn withdraw_loaders(store: &Store) -> Result<()> {
    store.delete_project_metadata(LOADERS_KEY)?;
    Ok(())
}

/// The indexed Lua and Luau files, by the language their rows record, but
/// `skip`: what a change of the loader names extracts again.
pub(crate) fn every_lua_file(store: &Store, skip: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
    Ok(store
        .all_files()?
        .into_iter()
        .filter(|file| is_lua(file.language) && !skip(&file.path))
        .map(|file| file.path)
        .collect())
}

/// How many `require` hops a changed module's dependents are followed: an
/// alias is followed through three modules' exports, and a module's own
/// functions are read one hop further.
pub const DEPENDENT_DEPTH: usize = 4;

fn is_lua(language: Language) -> bool {
    matches!(language, Language::Lua | Language::Luau)
}

/// `source`'s module row, `None` for a file that is no Lua file or no text.
pub fn row_of(language: Language, source: &SourceText, nodes: &[Node]) -> Option<LuaModuleRow> {
    match source {
        SourceText::Text(text) => row_of_text(language, text, nodes),
        SourceText::Oversize(_) | SourceText::MpegTransportStream => None,
    }
}

fn row_of_text(language: Language, text: &str, nodes: &[Node]) -> Option<LuaModuleRow> {
    is_lua(language).then(|| LuaModuleRow::of(text, nodes))
}

/// Write the rows of a full index or a migration (any order).
pub fn write_index_rows(store: &Store, rows: &mut [(String, LuaModuleRow)]) -> Result<()> {
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, row) in rows.iter() {
        store.set_project_metadata(&module_row_key(path), &row.encode())?;
    }
    Ok(())
}

/// Whether this index needs every Lua file re-resolved and its rows rebuilt:
/// its marker is missing or at another version, or a row cannot be read
/// (failing closed even in a sync with nothing changed).
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    if store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION) {
        return Ok(true);
    }
    Ok(store
        .project_metadata_with_prefix(MODULE_ROW_KEY_PREFIX)?
        .iter()
        .any(|(_, value)| LuaModuleRow::decode(value).is_none()))
}

/// Record that every Lua dependent is current: a full index, a migration, or
/// a sync whose resolution is persisted.
pub fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

fn read_row(store: &Store, path: &str) -> Result<Option<LuaModuleRow>> {
    Ok(store
        .get_project_metadata(&module_row_key(path))?
        .and_then(|value| LuaModuleRow::decode(&value)))
}

/// What one sync recorded of a changed file.
#[derive(Debug, Default)]
struct Change {
    old_row: Option<LuaModuleRow>,
    new_row: Option<LuaModuleRow>,
}

/// What a sync's changed files held for the Lua lane.
#[derive(Debug, Default)]
pub(crate) struct LuaChanges {
    changes: BTreeMap<String, Change>,
}

impl LuaChanges {
    /// `path` changed (its old row still in the store) to `new` (its
    /// language, text and nodes), or left the index (`None`).
    pub(crate) fn note(
        &mut self,
        store: &Store,
        path: &str,
        new: Option<(Language, &SourceText, &[Node])>,
    ) -> Result<()> {
        let old_row = read_row(store, path)?;
        let new_row = new.and_then(|(language, source, nodes)| row_of(language, source, nodes));
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

/// What a sync's Lua lane calls for.
#[derive(Debug, Default)]
pub(crate) struct LuaRefresh {
    /// Unchanged files to re-resolve whole.
    pub(crate) files: BTreeSet<String>,
    /// Names whose locality changed, to touch for the name lane.
    pub(crate) names: BTreeSet<String>,
    /// Rows to write once the sync's resolution is persisted (`None`
    /// deletes).
    rows: BTreeMap<String, Option<LuaModuleRow>>,
    /// Whether every row was rebuilt, so the prefix is cleared first.
    rebuilt: bool,
}

impl LuaRefresh {
    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        if self.rebuilt {
            store.delete_project_metadata_with_prefix(MODULE_ROW_KEY_PREFIX)?;
        }
        for (written, (path, row)) in self.rows.iter().enumerate() {
            #[cfg(test)]
            if self.rebuilt
                && written == 1
                && crate::sync::tests::FAIL_AFTER_LUA_ROW_PURGE.with(std::cell::Cell::get)
            {
                anyhow::bail!("injected failure in a Lua row rebuild");
            }
            #[cfg(not(test))]
            let _ = written;
            let key = module_row_key(path);
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

fn read_text(project_root: &Path, path: &str, max_file_size: u64) -> Option<String> {
    match read_source_file(&project_root.join(path), path, max_file_size) {
        Ok((_, SourceText::Text(text))) => Some(text),
        _ => None,
    }
}

/// What the Lua lane calls for after the changed files were re-extracted
/// (the store holds the new nodes; the rows are still the old ones), given
/// every name the sync touched. With `bootstrap`, every row is rebuilt from
/// disk instead and every Lua file re-resolves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lua_refresh(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &LuaChanges,
    touched: &HashSet<String>,
    reindexed: &HashSet<String>,
    removed: &BTreeSet<String>,
    bootstrap: bool,
) -> Result<LuaRefresh> {
    let unchanged = |path: &str| !reindexed.contains(path) && !removed.contains(path);
    if bootstrap {
        return rebuild(store, project_root, max_file_size, changes, &unchanged);
    }
    // What every unchanged Lua file reads; a row that cannot be read fails
    // closed into a rebuild.
    let mut rows: Vec<(String, LuaModuleRow)> = Vec::new();
    for (key, value) in store.project_metadata_with_prefix(MODULE_ROW_KEY_PREFIX)? {
        let Some(path) = key.strip_prefix(MODULE_ROW_KEY_PREFIX) else {
            continue;
        };
        if !unchanged(path) {
            continue;
        }
        let Some(row) = LuaModuleRow::decode(&value) else {
            return rebuild(store, project_root, max_file_size, changes, &unchanged);
        };
        rows.push((path.to_string(), row));
    }
    let reads_touched = |row: &LuaModuleRow| row.globals.iter().any(|name| touched.contains(name));
    let mut refresh = LuaRefresh::default();
    let mut modules: BTreeSet<String> = BTreeSet::new();
    for (path, row) in &rows {
        if reads_touched(row) {
            refresh.files.insert(path.clone());
            modules.insert(path.clone());
        }
    }
    // What each file reads: an unchanged file its row, a changed one what it
    // read before the change and after it, so the closure passes through it.
    let mut reads: Vec<(String, BTreeSet<String>)> = rows
        .iter()
        .map(|(path, row)| (path.clone(), row.reads.iter().cloned().collect()))
        .collect();
    for (path, change) in &changes.changes {
        // A changed file re-resolves anyway; what it hands out through a
        // global table may still have moved for the files requiring it.
        if change.new_row.as_ref().is_some_and(reads_touched) {
            modules.insert(path.clone());
        }
        refresh.rows.insert(path.clone(), change.new_row.clone());
        let locals = |row: &Option<LuaModuleRow>| -> BTreeSet<LocalDecl> {
            row.iter()
                .flat_map(|row| row.locals.iter().cloned())
                .collect()
        };
        let (old_locals, new_locals) = (locals(&change.old_row), locals(&change.new_row));
        refresh.names.extend(
            old_locals
                .symmetric_difference(&new_locals)
                .map(|(name, _, _)| name.clone()),
        );
        let surface = |row: &Option<LuaModuleRow>| row.as_ref().map(|row| row.surface.clone());
        if surface(&change.old_row) != surface(&change.new_row) {
            modules.insert(path.clone());
        }
        reads.push((
            path.clone(),
            [&change.old_row, &change.new_row]
                .into_iter()
                .flatten()
                .flat_map(|row| row.reads.iter().cloned())
                .collect(),
        ));
    }
    let mut reached = modules.clone();
    let mut frontier = modules;
    for _ in 0..DEPENDENT_DEPTH {
        if frontier.is_empty() {
            break;
        }
        let next: BTreeSet<String> = reads
            .iter()
            .filter(|(path, _)| !reached.contains(path))
            .filter(|(_, read)| {
                read.iter().any(|module| {
                    frontier
                        .iter()
                        .any(|changed| lua_module_may_be(module, changed))
                })
            })
            .map(|(path, _)| path.clone())
            .collect();
        refresh
            .files
            .extend(next.iter().filter(|path| unchanged(path)).cloned());
        reached.extend(next.iter().cloned());
        frontier = next;
    }
    Ok(refresh)
}

/// Every Lua file re-resolves and every row is rebuilt from disk (a changed
/// file's from its new text).
fn rebuild(
    store: &Store,
    project_root: &Path,
    max_file_size: u64,
    changes: &LuaChanges,
    unchanged: &dyn Fn(&str) -> bool,
) -> Result<LuaRefresh> {
    let mut refresh = LuaRefresh {
        rebuilt: true,
        ..LuaRefresh::default()
    };
    for file in store.all_files()? {
        if !is_lua(file.language) {
            continue;
        }
        let row = match changes.changes.get(&file.path) {
            Some(change) => change.new_row.clone(),
            None => read_text(project_root, &file.path, max_file_size).and_then(|text| {
                store
                    .nodes_by_file_path(&file.path)
                    .ok()
                    .and_then(|nodes| row_of_text(file.language, &text, &nodes))
            }),
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

    fn temp_store(label: &str) -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!(
            "cg-lua-loaders-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("index.db")).unwrap();
        (dir, store)
    }

    fn settings(loaders: &[&str], roots: &[&str]) -> LuaLoaders {
        LuaLoaders::merge(
            Some(&serde_json::json!({"loader_functions": loaders, "module_roots": roots})),
            None,
        )
    }

    /// A record that is missing or that this build would not write is a
    /// bootstrap, for a project naming no loader too.
    #[test]
    fn a_missing_loader_record_is_a_bootstrap() {
        let (dir, store) = temp_store("missing");
        for current in [settings(&[], &[]), settings(&["Require"], &["Script"])] {
            assert_eq!(
                loaders_change(&store, &current).unwrap(),
                LoadersChange::Bootstrap
            );
        }
        for unreadable in ["", "{", r#"{"loaderFunctions":["B","A"],"moduleRoots":[]}"#] {
            store.set_project_metadata(LOADERS_KEY, unreadable).unwrap();
            assert_eq!(
                loaders_change(&store, &settings(&[], &[])).unwrap(),
                LoadersChange::Bootstrap,
                "{unreadable}"
            );
        }
        write_loaders(&store, &settings(&[], &[])).unwrap();
        assert_eq!(
            loaders_change(&store, &settings(&[], &[])).unwrap(),
            LoadersChange::Same
        );
        withdraw_loaders(&store).unwrap();
        assert_eq!(
            loaders_change(&store, &settings(&[], &[])).unwrap(),
            LoadersChange::Bootstrap
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A change of the loader names, with or without one of the roots, is
    /// a loader change; one of the roots alone, their order included, a
    /// roots change.
    #[test]
    fn loaders_change_tells_loader_names_from_roots() {
        let (dir, store) = temp_store("kinds");
        write_loaders(&store, &settings(&["Require"], &["Script", "Lib"])).unwrap();
        for (current, expected) in [
            (
                settings(&["Require"], &["Script", "Lib"]),
                LoadersChange::Same,
            ),
            (
                settings(&["Require", "Include"], &["Script", "Lib"]),
                LoadersChange::Loaders,
            ),
            (settings(&[], &["Script", "Lib"]), LoadersChange::Loaders),
            (settings(&["Include"], &["Lib"]), LoadersChange::Loaders),
            (
                settings(&["Require"], &["Lib", "Script"]),
                LoadersChange::Roots,
            ),
            (settings(&["Require"], &[]), LoadersChange::Roots),
            (
                settings(&["Require"], &["Script", "Lib", "."]),
                LoadersChange::Roots,
            ),
        ] {
            assert_eq!(
                loaders_change(&store, &current).unwrap(),
                expected,
                "{}",
                current.canonical_json()
            );
        }
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The loader names are a set: written in another order, or twice, they
    /// are no change.
    #[test]
    fn reordering_loader_names_is_no_change() {
        let (dir, store) = temp_store("order");
        write_loaders(&store, &settings(&["Require", "Include"], &["Script"])).unwrap();
        for current in [
            settings(&["Include", "Require"], &["Script"]),
            settings(&["Require", "Include", "Require"], &["./Script/"]),
        ] {
            assert_eq!(
                loaders_change(&store, &current).unwrap(),
                LoadersChange::Same
            );
        }
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_module_path_is_what_a_require_of_its_dotted_name_loads() {
        assert!(lua_module_may_be("kong.pdk.phases", "kong/pdk/phases.lua"));
        assert!(lua_module_may_be(
            "kong.pdk.phases",
            "src/kong/pdk/phases.lua"
        ));
        assert!(lua_module_may_be("kong.pdk", "kong/pdk/init.lua"));
        assert!(lua_module_may_be("app", "app.luau"));
        assert!(!lua_module_may_be(
            "kong.pdk.phases",
            "kong/pdk/phases_x.lua"
        ));
        assert!(!lua_module_may_be("phases", "kong/pdk/myphases.lua"));
    }
}
