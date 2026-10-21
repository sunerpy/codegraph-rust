//! Shopify themes (upstream #2393 and #2396, in the shape at `b635dd46`): a
//! theme is a directory holding `layout/theme.liquid`, which every theme
//! must have, or its settings schema `config/settings_schema.json`, beside
//! its theme folders. JSON under a theme's `templates/` or `sections/` (OS
//! 2.0 templates and section groups) is Liquid, which links each section
//! `"type"` to its `sections/<type>.liquid`; JSON in folders of those names
//! anywhere else (a .NET project template's
//! `templates/<name>/.template.config/template.json`, schematics, CMS
//! content) is not.

use std::path::Path;

/// What marks a Shopify theme's root, beside its theme folders.
pub const SHOPIFY_THEME_MARKERS: [&str; 2] = ["layout/theme.liquid", "config/settings_schema.json"];

/// The folders a Shopify theme is made of.
pub const SHOPIFY_THEME_FOLDERS: [&str; 8] = [
    "assets",
    "blocks",
    "config",
    "layout",
    "locales",
    "sections",
    "snippets",
    "templates",
];

/// The folders a theme's JSON templates and section groups sit in.
pub const SHOPIFY_JSON_FOLDERS: [&str; 2] = ["templates", "sections"];

/// Whether a directory named `name` (in any case) is where a theme keeps one
/// of its markers: `layout` or `config`.
pub fn is_shopify_marker_folder(name: &str) -> bool {
    SHOPIFY_THEME_MARKERS
        .iter()
        .filter_map(|marker| marker.split_once('/'))
        .any(|(folder, _)| name.eq_ignore_ascii_case(folder))
}

/// The marker paths a theme's marker folder `folder` (project-relative, a
/// `layout` or `config` directory in any case) may hold: what leaves with it
/// when it is removed or moved away.
pub fn shopify_markers_in_folder(folder: &str) -> Vec<String> {
    let name = folder.rsplit('/').next().unwrap_or(folder);
    SHOPIFY_THEME_MARKERS
        .iter()
        .filter_map(|marker| marker.split_once('/'))
        .filter(|(marker_folder, _)| name.eq_ignore_ascii_case(marker_folder))
        .map(|(_, file)| format!("{folder}/{file}"))
        .collect()
}

/// Whether the directory `theme` (project-relative, `""` for the project
/// root) holds one of [`SHOPIFY_THEME_MARKERS`]; never for a path not
/// lexically inside the project. `exists` says whether a project-relative
/// path exists.
pub fn is_shopify_theme(theme: &str, exists: &dyn Fn(&str) -> bool) -> bool {
    if !theme.is_empty() && !is_contained(theme) {
        return false;
    }
    SHOPIFY_THEME_MARKERS.iter().any(|marker| {
        exists(&if theme.is_empty() {
            (*marker).to_string()
        } else {
            format!("{theme}/{marker}")
        })
    })
}

/// Whether a directory named `name` (in any case) holds a theme's JSON
/// templates or section groups: `templates` or `sections`.
pub fn is_shopify_json_folder(name: &str) -> bool {
    SHOPIFY_JSON_FOLDERS
        .iter()
        .any(|folder| name.eq_ignore_ascii_case(folder))
}

/// Whether `relative` stays inside the project lexically: no absolute path,
/// no `..` or empty segment.
fn is_contained(relative: &str) -> bool {
    !relative.is_empty()
        && !relative.starts_with('/')
        && !relative.contains('\\')
        && relative
            .split('/')
            .all(|segment| !segment.is_empty() && segment != ".." && segment != ".")
}

/// The root of the Shopify theme `relative` is in (upstream
/// `shopifyThemeRoot`): the directory holding the theme folder (one of
/// `folders`, in any case) the file sits in, the folder nearest the file
/// first, when that directory also holds one of [`SHOPIFY_THEME_MARKERS`].
/// Project-relative and `/`-separated, `""` for the project root; `None` when
/// the file is in no theme, or its path is not lexically inside the project.
/// `exists` says whether a project-relative path exists.
pub fn shopify_theme_root(
    relative: &str,
    exists: &dyn Fn(&str) -> bool,
    folders: &[&str],
) -> Option<String> {
    if !is_contained(relative) {
        return None;
    }
    let segments: Vec<&str> = relative.split('/').collect();
    for i in (0..segments.len().saturating_sub(1)).rev() {
        if !folders
            .iter()
            .any(|folder| segments[i].eq_ignore_ascii_case(folder))
        {
            continue;
        }
        let theme = segments[..i].join("/");
        if is_shopify_theme(&theme, exists) {
            return Some(theme);
        }
    }
    None
}

/// Whether `relative` names JSON under a `templates/` or `sections/` folder,
/// nested or not, in any case (`/(^|\/)(templates|sections)\/.+\.json$/i`:
/// something precedes `.json` past the folder). Called for every path a
/// scan or a watcher weighs, so it allocates nothing.
fn is_template_or_section_json(relative: &str) -> bool {
    let is_json = relative
        .len()
        .checked_sub(".json".len())
        .and_then(|start| relative.get(start..))
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".json"));
    if !is_json {
        return false;
    }
    let mut rest = relative;
    while let Some((segment, after)) = rest.split_once('/') {
        if after.len() > ".json".len()
            && SHOPIFY_JSON_FOLDERS
                .iter()
                .any(|folder| segment.eq_ignore_ascii_case(folder))
        {
            return true;
        }
        rest = after;
    }
    false
}

/// Whether `relative` is a Shopify theme's JSON template or section group
/// (upstream `isShopifyLiquidJson`): JSON under `templates/` or `sections/`
/// whose directory holding that folder is a theme, looked for under
/// `project_root`. Without a root there is no theme to find.
pub fn is_shopify_liquid_json(relative: &str, project_root: Option<&Path>) -> bool {
    let Some(root) = project_root else {
        return false;
    };
    is_template_or_section_json(relative)
        && shopify_theme_root(
            relative,
            &|path| root.join(path).exists(),
            &SHOPIFY_JSON_FOLDERS,
        )
        .is_some()
}

/// Whether `relative` is a Shopify theme marker (`layout/theme.liquid`,
/// `config/settings_schema.json`, at the project root or under a directory,
/// in any case, as a case-insensitive file system finds one): its coming or
/// going makes or unmakes the theme around it.
pub fn is_shopify_theme_marker(relative: &str) -> bool {
    shopify_marker_theme(relative).is_some()
}

/// The theme directory a marker path marks (`shop` for
/// `shop/layout/theme.liquid`, `""` at the project root).
pub fn shopify_marker_theme(relative: &str) -> Option<&str> {
    if !is_contained(relative) {
        return None;
    }
    SHOPIFY_THEME_MARKERS.iter().find_map(|marker| {
        let start = relative.len().checked_sub(marker.len())?;
        if !relative.get(start..)?.eq_ignore_ascii_case(marker) {
            return None;
        }
        match start {
            0 => Some(""),
            _ => relative.get(..start)?.strip_suffix('/'),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn exists_in(paths: &[&str]) -> impl Fn(&str) -> bool {
        let set: BTreeSet<String> = paths.iter().map(|path| (*path).to_string()).collect();
        move |path: &str| set.contains(path)
    }

    #[test]
    fn a_theme_root_is_the_nearest_folders_directory_with_a_marker() {
        let exists = exists_in(&["layout/theme.liquid", "shop/config/settings_schema.json"]);
        let root = |path: &str| shopify_theme_root(path, &exists, &SHOPIFY_THEME_FOLDERS);
        assert_eq!(root("sections/header.liquid").as_deref(), Some(""));
        assert_eq!(root("templates/customers/login.json").as_deref(), Some(""));
        assert_eq!(root("shop/snippets/price.liquid").as_deref(), Some("shop"));
        assert_eq!(root("shop/Sections/x.liquid").as_deref(), Some("shop"));
        assert_eq!(root("other/sections/x.liquid"), None);
        assert_eq!(root("src/app.liquid"), None);
        assert_eq!(root("../sections/x.liquid"), None);
        assert_eq!(root("/abs/sections/x.liquid"), None);
    }

    /// Upstream #2396's `finds the theme a file is in from the theme folder
    /// it sits in`.
    #[test]
    fn pr2396_the_theme_is_found_from_the_theme_folder_a_file_sits_in() {
        let exists = exists_in(&[
            "layout/theme.liquid",
            "examples/tailwind/config/settings_schema.json",
        ]);
        let root = |path: &str| shopify_theme_root(path, &exists, &SHOPIFY_THEME_FOLDERS);
        assert_eq!(root("sections/header.liquid").as_deref(), Some(""));
        assert_eq!(root("templates/customers/login.json").as_deref(), Some(""));
        assert_eq!(
            root("examples/tailwind/snippets/icon.liquid").as_deref(),
            Some("examples/tailwind")
        );
        // A copy kept elsewhere under the root theme is not part of it, nor
        // is a file beside its folders.
        assert_eq!(root("vendor/legacy/snippets/icon.liquid"), None);
        assert_eq!(root("theme.liquid"), None);
    }

    #[test]
    fn a_marker_is_recognized_with_its_theme() {
        assert_eq!(shopify_marker_theme("layout/theme.liquid"), Some(""));
        assert_eq!(
            shopify_marker_theme("shop/config/settings_schema.json"),
            Some("shop")
        );
        assert_eq!(shopify_marker_theme("shop/layout/other.liquid"), None);
        assert_eq!(
            shopify_marker_theme("Shop/Config/Settings_Schema.JSON"),
            Some("Shop")
        );
        assert!(!is_shopify_theme_marker("xlayout/theme.liquid"));
        assert!(!is_shopify_theme_marker("../layout/theme.liquid"));
        assert!(!is_shopify_theme_marker("config/settings_data.json"));
        assert!(is_shopify_marker_folder("Config") && is_shopify_marker_folder("layout"));
        assert!(!is_shopify_marker_folder("snippets"));
        assert!(is_shopify_json_folder("Templates") && !is_shopify_json_folder("config"));
        assert_eq!(
            shopify_markers_in_folder("shop/Config"),
            ["shop/Config/settings_schema.json"]
        );
        assert_eq!(shopify_markers_in_folder("layout"), ["layout/theme.liquid"]);
        assert!(shopify_markers_in_folder("shop/snippets").is_empty());
        let exists = exists_in(&["layout/theme.liquid", "shop/config/settings_schema.json"]);
        assert!(is_shopify_theme("", &exists) && is_shopify_theme("shop", &exists));
        assert!(!is_shopify_theme("other", &exists) && !is_shopify_theme("../shop", &exists));
    }

    #[test]
    fn only_template_and_section_json_can_be_liquid() {
        assert!(is_template_or_section_json("templates/product.json"));
        assert!(is_template_or_section_json(
            "shop/Sections/header-group.JSON"
        ));
        assert!(is_template_or_section_json(
            "templates/customers/login.json"
        ));
        assert!(!is_template_or_section_json("config/settings_data.json"));
        assert!(!is_template_or_section_json("templates.json"));
        // Nothing before `.json` past the folder, as the upstream pattern
        // reads it; a folder further up still counts.
        assert!(!is_template_or_section_json("templates/.json"));
        assert!(is_template_or_section_json("templates/a/.json"));
        assert!(!is_template_or_section_json("templates/product.liquid"));
        assert!(!is_shopify_liquid_json("templates/product.json", None));
    }
}
