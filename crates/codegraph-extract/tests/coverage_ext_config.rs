//! Coverage for the `.codegraph/codegraph.json` custom extension-override
//! reader (`ext_config.rs`) driven through the public `detect_language` API.
//! Exercises: successful override, unknown-language skip, malformed-JSON
//! tolerance, and the absent-config fast path. TEST-ONLY: no production change.

use codegraph_core::types::Language;
use codegraph_extract::detect_language;
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

fn write_config(root: &Path, contents: &str) {
    let dir = root.join(".codegraph");
    fs::create_dir_all(&dir).expect("create .codegraph");
    fs::write(dir.join("codegraph.json"), contents).expect("write codegraph.json");
}

#[test]
fn override_maps_custom_extension_to_a_known_language() {
    let project = unique_project("known");
    write_config(
        &project,
        r#"{ "extensions": { ".blade": "php", "X": "lua" } }"#,
    );

    // A custom extension unmapped by the built-in table resolves via the config.
    let blade = project.join("views/home.blade");
    assert_eq!(detect_language(&blade), Language::Php);
    // Keys are dot-stripped and lowercased before matching.
    let x = project.join("script.x");
    assert_eq!(detect_language(&x), Language::Lua);

    fs::remove_dir_all(&project).ok();
}

#[test]
fn override_ignores_unknown_language_names() {
    let project = unique_project("unknown_lang");
    write_config(&project, r#"{ "extensions": { ".foo": "klingon" } }"#);

    let foo = project.join("thing.foo");
    assert_eq!(detect_language(&foo), Language::Unknown);

    fs::remove_dir_all(&project).ok();
}

#[test]
fn malformed_config_is_tolerated_and_yields_no_override() {
    let project = unique_project("malformed");
    write_config(&project, "{ this is not valid json ");

    let bar = project.join("thing.bar");
    assert_eq!(detect_language(&bar), Language::Unknown);

    fs::remove_dir_all(&project).ok();
}

#[test]
fn builtin_extension_never_consults_the_override() {
    let project = unique_project("builtin_wins");
    // Even if the config tries to remap `.rs`, the built-in table wins because
    // the override is consulted only for extensions the built-ins do not claim.
    write_config(&project, r#"{ "extensions": { ".rs": "python" } }"#);

    let rs = project.join("src/lib.rs");
    assert_eq!(detect_language(&rs), Language::Rust);

    fs::remove_dir_all(&project).ok();
}

#[test]
fn absent_config_leaves_custom_extension_unknown() {
    let project = unique_project("absent");
    let baz = project.join("thing.baz");
    assert_eq!(detect_language(&baz), Language::Unknown);

    fs::remove_dir_all(&project).ok();
}
