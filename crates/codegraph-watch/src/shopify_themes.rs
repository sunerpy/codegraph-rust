//! What a Shopify theme's files read of the theme's markers (W4-LIQ-01,
//! [`codegraph_extract::embedded::shopify`]), so that a sync keeps them as a
//! fresh index would.
//!
//! JSON under a theme's `templates/` or `sections/` is Liquid only while the
//! directory holding those folders has a marker (`layout/theme.liquid` or
//! `config/settings_schema.json`, upstream #2393), and a section or snippet
//! reference made inside a theme reaches only that theme's file (#2396). A
//! marker that comes or goes reclassifies the theme's JSON and moves the
//! references made in its files, though none of them changed. Every full
//! index, migration and sync writes the theme roots its indexed Liquid files
//! sit in to one row ([`ROW_KEY`]). A sync reads the roots its paths and the
//! indexed Liquid files sit in now; for every root that came or went since
//! the row it hands the theme's JSON templates, on disk and indexed, to the
//! per-path pass, which keeps or drops each as the scan would, and
//! re-resolves the theme's unchanged Liquid files whole. The watcher hands a
//! marker's events to the sync for this, though a marker need not be
//! source, and git's pending fast path declines a marker among its
//! candidates.
//!
//! The row is written last, with the other rows. Before the per-path pass
//! the roots that came or went are recorded ([`PENDING_KEY`]), and the next
//! sync takes them up again until one finishes, so a sync that stops
//! partway leaves no theme's JSON behind, even one no indexed file points
//! at. An index without the marker at [`VERSION`] hands every indexed JSON
//! template to the per-path pass and re-resolves every Liquid file once,
//! then rebuilds the row.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use codegraph_core::types::Language;
use codegraph_extract::embedded::shopify::{
    SHOPIFY_THEME_FOLDERS, is_shopify_json_folder, is_shopify_marker_folder, is_shopify_theme,
    shopify_marker_theme, shopify_theme_root,
};
use codegraph_store::Store;

/// Metadata key present when no sync left a theme's files unrefreshed.
pub const VERSION_KEY: &str = "shopify_themes_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";
/// Metadata key of the row: the theme roots, sorted, as JSON.
pub const ROW_KEY: &str = "shopify_themes";
/// Metadata key of the roots a sync found moved and has not yet finished,
/// sorted, as JSON; absent once a sync finishes.
pub const PENDING_KEY: &str = "shopify_themes_pending";

/// Whether this index needs its theme roots rebuilt.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Record that every theme's files are current: a full index, a migration,
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

/// The theme roots the indexed Liquid files sit in, read from disk under
/// `project_root`: what the row holds.
fn indexed_theme_roots(store: &Store, project_root: &Path) -> Result<BTreeSet<String>> {
    let liquid = store.file_paths_of_language(Language::Liquid)?;
    Ok(theme_roots_of(
        project_root,
        liquid.iter().map(String::as_str),
    ))
}

/// Write the row of a full index or a migration, once every file is stored.
pub fn write_index_row(store: &Store, project_root: &Path) -> Result<()> {
    write_row_of(store, &indexed_theme_roots(store, project_root)?)
}

fn write_row_of(store: &Store, roots: &BTreeSet<String>) -> Result<()> {
    store.set_project_metadata(ROW_KEY, &serde_json::to_string(roots)?)?;
    Ok(())
}

fn read_row(store: &Store) -> Result<Option<BTreeSet<String>>> {
    read_roots(store, ROW_KEY)
}

fn read_roots(store: &Store, key: &str) -> Result<Option<BTreeSet<String>>> {
    Ok(store
        .get_project_metadata(key)?
        .and_then(|value| serde_json::from_str(&value).ok()))
}

/// The theme roots `paths` sit in, read from disk under `project_root`
/// (each path probed once). A marker sits in a theme folder of the theme it
/// marks, so its own path names that theme while it is there.
fn theme_roots_of<'p>(
    project_root: &Path,
    paths: impl IntoIterator<Item = &'p str>,
) -> BTreeSet<String> {
    let probed: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
    let exists = |relative: &str| -> bool {
        if let Some(&hit) = probed.borrow().get(relative) {
            return hit;
        }
        let found = project_root.join(relative).exists();
        probed.borrow_mut().insert(relative.to_string(), found);
        found
    };
    paths
        .into_iter()
        .filter_map(|path| shopify_theme_root(path, &exists, &SHOPIFY_THEME_FOLDERS))
        .collect()
}

/// What a sync's theme lane calls for.
#[derive(Debug, Default)]
pub(crate) struct ThemeRefresh {
    /// The theme roots that came or went.
    pub(crate) moved: BTreeSet<String>,
    /// The JSON templates of those themes, on disk or indexed, for the
    /// per-path pass.
    pub(crate) json_paths: BTreeSet<String>,
    /// The row as stored, `None` when absent or unreadable.
    before: Option<BTreeSet<String>>,
    /// Whether the index has no marker: every Liquid file is re-resolved.
    bootstrap: bool,
    /// Whether [`PENDING_KEY`] is stored.
    pending: bool,
}

impl ThemeRefresh {
    /// Write the row: the theme roots the Liquid files the sync left indexed
    /// sit in. Called after the sync's resolution is persisted.
    pub(crate) fn write_row(&self, store: &Store, project_root: &Path) -> Result<()> {
        let after = indexed_theme_roots(store, project_root)?;
        if self.bootstrap || self.before.as_ref() != Some(&after) {
            write_row_of(store, &after)?;
        }
        if self.pending {
            store.delete_project_metadata(PENDING_KEY)?;
        }
        Ok(())
    }

    /// The indexed Liquid files of the themes that came or went, every one
    /// on a bootstrap, for the whole-file lane.
    pub(crate) fn liquid_files(&self, store: &Store) -> Result<Vec<String>> {
        if self.moved.is_empty() && !self.bootstrap {
            return Ok(Vec::new());
        }
        Ok(store
            .file_paths_of_language(Language::Liquid)?
            .into_iter()
            .filter(|path| self.bootstrap || self.moved.iter().any(|theme| is_under(path, theme)))
            .collect())
    }
}

/// Whether `path` lies under the directory `theme` (`""` holds everything).
fn is_under(path: &str, theme: &str) -> bool {
    theme.is_empty() || path.starts_with(&format!("{theme}/"))
}

/// The `templates/` and `sections/` folders (in any case) the directory
/// `theme` holds on disk, project-relative, never through a symlink.
pub(crate) fn json_folders_of(project_root: &Path, theme: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(project_root.join(theme)) else {
        return Vec::new();
    };
    let mut folders: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| is_shopify_json_folder(name))
        .map(|name| {
            if theme.is_empty() {
                name
            } else {
                format!("{theme}/{name}")
            }
        })
        .collect();
    folders.sort();
    folders
}

/// Whether a theme root the row records holds no marker on disk any more: its
/// JSON templates leave and its references move, though git, which may never
/// have seen an ignored marker, reports nothing of it.
pub(crate) fn a_recorded_theme_lost_its_marker(store: &Store, project_root: &Path) -> Result<bool> {
    let Some(roots) = read_row(store)? else {
        return Ok(false);
    };
    Ok(roots
        .iter()
        .any(|theme| !is_shopify_theme(theme, &|path| project_root.join(path).exists())))
}

/// Whether git ignoring `entry` (project-relative, a directory written with
/// a trailing `/`) may hide a theme marker whose coming or going moves JSON
/// the scan keeps: a marker, or a `config/` or `layout/` directory (in any
/// case), in a directory holding a `templates/` or `sections/` folder.
pub(crate) fn may_hide_a_marker(project_root: &Path, entry: &str) -> bool {
    let theme = match entry.strip_suffix('/') {
        Some(dir) => {
            let (parent, name) = dir.rsplit_once('/').unwrap_or(("", dir));
            if !is_shopify_marker_folder(name) {
                return false;
            }
            parent
        }
        None => match shopify_marker_theme(entry) {
            Some(theme) => theme,
            None => return false,
        },
    };
    !json_folders_of(project_root, theme).is_empty()
}

/// Whether the indexed `path` is JSON in a `templates/` or `sections/`
/// folder (in any case) the directory `theme` holds.
fn is_json_of_theme(path: &str, theme: &str) -> bool {
    let rest = if theme.is_empty() {
        Some(path)
    } else {
        path.strip_prefix(theme)
            .and_then(|rest| rest.strip_prefix('/'))
    };
    rest.and_then(|rest| rest.split_once('/'))
        .is_some_and(|(folder, _)| is_shopify_json_folder(folder))
        && path.to_ascii_lowercase().ends_with(".json")
}

/// The JSON files on disk under `dir` (project-relative), its subdirectories
/// included, never through a symlink.
fn json_files_under(project_root: &Path, dir: &str, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(project_root.join(dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let relative = format!("{dir}/{name}");
        if kind.is_dir() {
            json_files_under(project_root, &relative, out);
        } else if kind.is_file() && name.to_ascii_lowercase().ends_with(".json") {
            out.insert(relative);
        }
    }
}

/// Read what the theme lane calls for, before the per-path pass: the theme
/// roots `paths` (a marker's own included) and the indexed Liquid files sit
/// in, read from disk now, against the row, and the roots an unfinished sync
/// recorded. The moved roots are recorded before the pass changes anything.
/// A bootstrap also hands every indexed JSON template to the pass, which
/// drops one no theme holds any more.
pub(crate) fn theme_refresh<'p>(
    store: &Store,
    project_root: &Path,
    paths: impl IntoIterator<Item = &'p str>,
    bootstrap: bool,
) -> Result<ThemeRefresh> {
    let stored_row = if bootstrap { None } else { read_row(store)? };
    let before = stored_row.clone().unwrap_or_default();
    let stored = store.file_paths_of_language(Language::Liquid)?;
    let mut read: Vec<&str> = paths.into_iter().collect();
    read.extend(stored.iter().map(String::as_str));
    let roots = theme_roots_of(project_root, read);
    let unfinished = read_roots(store, PENDING_KEY)?;
    let mut moved: BTreeSet<String> = before.symmetric_difference(&roots).cloned().collect();
    moved.extend(unfinished.iter().flatten().cloned());
    if !moved.is_empty() && unfinished.as_ref() != Some(&moved) {
        store.set_project_metadata(PENDING_KEY, &serde_json::to_string(&moved)?)?;
    }
    let pending = unfinished.is_some() || !moved.is_empty();
    let mut json_paths = BTreeSet::new();
    if bootstrap {
        json_paths.extend(
            stored
                .iter()
                .filter(|path| path.to_ascii_lowercase().ends_with(".json"))
                .cloned(),
        );
    }
    for theme in &moved {
        for folder in json_folders_of(project_root, theme) {
            json_files_under(project_root, &folder, &mut json_paths);
        }
        json_paths.extend(
            stored
                .iter()
                .filter(|path| is_json_of_theme(path, theme))
                .cloned(),
        );
    }
    Ok(ThemeRefresh {
        moved,
        json_paths,
        before: stored_row,
        bootstrap,
        pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_roots_are_read_from_files_and_markers() {
        let dir = std::env::temp_dir().join(format!("codegraph-themes-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("shop/layout")).unwrap();
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("shop/layout/theme.liquid"), "").unwrap();
        std::fs::write(dir.join("config/settings_schema.json"), "[]").unwrap();
        let roots = theme_roots_of(
            &dir,
            [
                "shop/sections/header.liquid",
                "other/sections/footer.liquid",
                "config/settings_schema.json",
                "layout/theme.liquid",
            ],
        );
        assert_eq!(roots, BTreeSet::from(["".to_string(), "shop".to_string()]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_themes_json_folders_are_found_in_any_case() {
        let dir = std::env::temp_dir().join(format!("codegraph-theme-json-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Shop/Templates/customers")).unwrap();
        std::fs::create_dir_all(dir.join("Shop/sections")).unwrap();
        std::fs::create_dir_all(dir.join("Shop/snippets")).unwrap();
        assert_eq!(
            json_folders_of(&dir, "Shop"),
            ["Shop/Templates", "Shop/sections"]
        );
        assert!(is_json_of_theme(
            "Shop/Templates/customers/login.json",
            "Shop"
        ));
        assert!(is_json_of_theme("templates/page.JSON", ""));
        assert!(!is_json_of_theme("Shop/snippets/a.json", "Shop"));
        assert!(!is_json_of_theme("Shopping/templates/a.json", "Shop"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_may_hide_a_marker_beside_a_themes_json_folders() {
        let dir = std::env::temp_dir().join(format!("codegraph-theme-hide-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("shop/templates")).unwrap();
        std::fs::create_dir_all(dir.join("app/config")).unwrap();
        assert!(may_hide_a_marker(&dir, "shop/config/settings_schema.json"));
        assert!(may_hide_a_marker(&dir, "shop/Config/"));
        assert!(may_hide_a_marker(&dir, "shop/layout/"));
        assert!(!may_hide_a_marker(&dir, "shop/config/settings_data.json"));
        assert!(!may_hide_a_marker(&dir, "shop/assets/"));
        // No JSON folder beside it: nothing for a marker to move.
        assert!(!may_hide_a_marker(&dir, "app/config/"));
        assert!(!may_hide_a_marker(&dir, "config/"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_path_is_under_its_theme() {
        assert!(is_under("shop/sections/a.liquid", "shop"));
        assert!(!is_under("shopping/sections/a.liquid", "shop"));
        assert!(is_under("sections/a.liquid", ""));
    }
}
