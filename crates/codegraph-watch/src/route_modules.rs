//! The route and value references a sync follows to the modules they
//! lazily load (W7-23a: upstream #2422's module keys, #2560's barrels and
//! #2452's `lazyModules`).
//!
//! A router names a page or layout it loads lazily by the module's path:
//! React Router's `lazy-import:./pages/Team`, Vue Router's and Angular's
//! `import:./home/home.component#HomeComponent`. React renders a value its
//! own file declares, `const Docs = lazy(() => import('./pages/Docs'))`, as
//! the component that module exports, wherever the file names it: a route's
//! `element`, a JSX tag. None of these references names a symbol the module
//! declares, so neither the retry by name nor the re-resolution of the edges
//! into a changed file reaches them when their module appears, gains its
//! export or re-points the page a barrel forwards. Two lanes read them once
//! the sync's files and framework nodes are written:
//!
//! - R, a route's module reference. A parked one waits under its module
//!   path's stem (upstream's `module:<stem>` tail), which the changed files
//!   and the barrels that forward them carry among their
//!   [`import_path_keys`]; the barrels are found through the module-surface
//!   rows, which record what each barrel forwards, since an `export … from`
//!   makes no import edge. A resolved one goes back when its module now
//!   resolves into the changed files or those barrels.
//! - V, a reference naming a module-level value of its own file. It goes
//!   back when the files React's lookup reads through the value
//!   ([`ReferenceResolver::lazy_modules`], read after the change) include a
//!   changed file.
//!
//! Each reference that goes back is re-resolved at its site. Neither lane
//! has a ceiling: a reference left out is a sync that differs from a fresh
//! index (memory `name-tail-retry-ceiling`).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::Result;
use codegraph_core::types::Language;
use codegraph_resolve::{RefView, ReferenceResolver};
use codegraph_store::queries::{FileReferenceSite, LaneReferences, ReferenceSite};
use codegraph_store::{Store, import_path_keys, route_module_path, route_module_stem};

/// Whether `path` can be a module a route or a value loads: a file of a
/// language with a module surface (the JavaScript family, Vue, Svelte,
/// Astro, ArkTS). A removed file's language is gone with its row, so its
/// path decides.
pub(crate) fn may_be_a_module(path: &str, language: Option<Language>) -> bool {
    codegraph_resolve::module_surface::has_module_surface(
        path,
        language.unwrap_or_else(|| language_of_path(path)),
    )
}

/// The language a removed file's path gives, for [`may_be_a_module`].
fn language_of_path(path: &str) -> Language {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".vue") {
        Language::Vue
    } else if lower.ends_with(".svelte") {
        Language::Svelte
    } else if lower.ends_with(".astro") {
        Language::Astro
    } else if lower.ends_with(".ets") {
        Language::ArkTs
    } else {
        Language::Unknown
    }
}

/// The sites of the route and value references whose answer the sync's
/// `changed` module files may move (lanes R and V). `barrels` gives the
/// files that forward one of them, read only when a route references a
/// module. A site in `reindexed` is left out: its file was resolved anew.
pub(crate) fn route_module_sites(
    store: &Store,
    resolver: &ReferenceResolver,
    changed: &BTreeSet<String>,
    barrels: impl FnOnce() -> Result<BTreeSet<String>>,
    reindexed: &HashSet<String>,
) -> Result<Vec<FileReferenceSite>> {
    let mut sites: BTreeSet<FileReferenceSite> = BTreeSet::new();
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let context = resolver.store_context(store);
    // Lane R.
    let routes: Vec<(String, codegraph_store::queries::LaneReference)> = store
        .lane_references(&LaneReferences::JsRoutes)?
        .into_iter()
        .filter(|reference| !reindexed.contains(&reference.site.file_path))
        .filter_map(|reference| {
            let path =
                route_module_path(&reference.reference_name, reference.reference_kind)?.to_string();
            Some((path, reference))
        })
        .collect();
    if !routes.is_empty() {
        let mut moved: BTreeSet<String> = changed.clone();
        moved.extend(barrels()?);
        let keys: HashSet<String> = moved
            .iter()
            .flat_map(|file| import_path_keys(file))
            .collect();
        for (path, reference) in routes {
            let back = if reference.resolved {
                resolver
                    .route_module_file(&path, &reference.site.file_path, &context)
                    .is_some_and(|file| moved.contains(&file))
            } else {
                route_module_stem(&path).is_some_and(|stem| keys.contains(stem))
            };
            if back {
                sites.insert(reference.site);
            }
        }
    }
    // Lane V.
    if resolver
        .framework_names()
        .iter()
        .any(|name| name == "react")
    {
        let files: Vec<String> = store
            .files_with_dynamic_imports()?
            .into_iter()
            .filter(|file| !reindexed.contains(file))
            .collect();
        // One lookup a value, a language, whether a route names it and
        // whether it is a JavaScript file's tag, the one JavaScript
        // reference React's lookup reads (W7-28): every reference so alike
        // reads the same files.
        type ValueKey = (String, String, &'static str, bool, bool);
        let mut by_value: BTreeMap<ValueKey, Vec<FileReferenceSite>> = BTreeMap::new();
        let mut views: BTreeMap<ValueKey, RefView> = BTreeMap::new();
        for reference in store.same_file_value_references(&files)? {
            let view = RefView {
                row_id: reference.id,
                from_node_id: reference.from_node_id,
                reference_name: reference.reference_name,
                reference_kind: reference.reference_kind,
                line: reference.line,
                column: reference.col,
                file_path: reference.file_path,
                language: reference.language,
                is_function_ref: reference.is_function_ref,
                reference_subkind: reference.reference_subkind,
            };
            let key: ValueKey = (
                view.file_path.clone(),
                view.reference_name.clone(),
                view.language.as_str(),
                view.from_node_id.starts_with("route:"),
                resolver.is_javascript_tag(&view, &context),
            );
            by_value
                .entry(key.clone())
                .or_default()
                .push(FileReferenceSite {
                    file_path: view.file_path.clone(),
                    site: Some(ReferenceSite {
                        from_node_id: view.from_node_id.clone(),
                        line: view.line,
                        col: view.column,
                    }),
                });
            views.entry(key).or_insert(view);
        }
        for (key, view) in &views {
            let back = resolver
                .lazy_modules(view, &context)
                .iter()
                .any(|file| changed.contains(file));
            if back {
                sites.extend(by_value.remove(key).unwrap_or_default());
            }
        }
    }
    Ok(sites.into_iter().collect())
}
