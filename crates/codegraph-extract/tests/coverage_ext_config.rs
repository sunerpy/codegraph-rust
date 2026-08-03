//! Coverage for the PROJECT-SCOPED `codegraph.json` extension-override reader
//! (`ext_config.rs`) driven through its public API plus `detect_language_with`.
//! Exercises: successful override, unknown-language skip, malformed-JSON
//! tolerance, the absent-config fast path, and the built-in skip-list.
//! TEST-ONLY: no production change.

use codegraph_core::IndexPaths;
use codegraph_core::types::Language;
use codegraph_extract::{ExtensionOverrides, detect_language, detect_language_with};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

fn unique_project(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("cg_ext_{tag}_{}_{nanos}_{n}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp project");
    dir
}

/// Write the project's CURRENT-ROOT `codegraph.json` and load it the way the
/// pipeline does — explicitly, from the resolved index paths.
fn load_overrides(project: &Path, contents: &str) -> std::sync::Arc<ExtensionOverrides> {
    let paths = IndexPaths::resolve(project, None).expect("resolve index paths");
    fs::create_dir_all(paths.current_root()).expect("create current root");
    fs::write(paths.extension_config(), contents).expect("write codegraph.json");
    ExtensionOverrides::load_for_paths(&paths)
}

#[test]
fn override_maps_custom_extension_to_a_known_language() {
    let project = unique_project("known");
    let overrides = load_overrides(
        &project,
        r#"{ "extensions": { ".blade": "php", "X": "lua" } }"#,
    );

    // A custom extension unmapped by the built-in table resolves via the config.
    assert_eq!(
        detect_language_with("views/home.blade", &overrides),
        Language::Php
    );
    // Keys are dot-stripped and lowercased before matching.
    assert_eq!(detect_language_with("script.x", &overrides), Language::Lua);

    fs::remove_dir_all(&project).ok();
}

#[test]
fn override_ignores_unknown_language_names() {
    let project = unique_project("unknown_lang");
    let overrides = load_overrides(&project, r#"{ "extensions": { ".foo": "klingon" } }"#);

    assert_eq!(
        detect_language_with("thing.foo", &overrides),
        Language::Unknown
    );

    fs::remove_dir_all(&project).ok();
}

#[test]
fn malformed_config_is_tolerated_and_yields_no_override() {
    let project = unique_project("malformed");
    let overrides = load_overrides(&project, "{ this is not valid json ");

    assert!(overrides.is_empty());
    assert_eq!(
        detect_language_with("thing.bar", &overrides),
        Language::Unknown
    );

    fs::remove_dir_all(&project).ok();
}

#[test]
fn builtin_extension_never_consults_the_override() {
    let project = unique_project("builtin_wins");
    // Even if the config tries to remap `.rs`, the built-in table wins because
    // the override is consulted only for extensions the built-ins do not claim.
    let overrides = load_overrides(&project, r#"{ "extensions": { ".rs": "python" } }"#);

    assert_eq!(
        detect_language_with("src/lib.rs", &overrides),
        Language::Rust
    );

    fs::remove_dir_all(&project).ok();
}

#[test]
fn absent_config_leaves_custom_extension_unknown() {
    let project = unique_project("absent");
    let paths = IndexPaths::resolve(&project, None).expect("resolve index paths");
    let overrides = ExtensionOverrides::load_for_paths(&paths);

    assert!(overrides.is_empty());
    assert_eq!(
        detect_language_with("thing.baz", &overrides),
        Language::Unknown
    );
    // The override-free entry point agrees.
    assert_eq!(detect_language("thing.baz"), Language::Unknown);

    fs::remove_dir_all(&project).ok();
}

#[test]
fn project_codegraph_json_is_read() {
    let project = unique_project("project_config");
    fs::create_dir_all(project.join(".codegraph")).unwrap();
    fs::write(
        project.join(".codegraph/codegraph.json"),
        r#"{ "extensions": { ".legacyext": "lua" } }"#,
    )
    .unwrap();
    let paths = IndexPaths::resolve(&project, None).expect("resolve index paths");

    let overrides = ExtensionOverrides::load_for_paths(&paths);
    assert_eq!(
        detect_language_with("plugin.legacyext", &overrides),
        Language::Lua
    );

    fs::remove_dir_all(&project).ok();
}
