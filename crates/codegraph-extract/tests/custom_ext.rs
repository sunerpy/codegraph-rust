//! T11: custom file-extension -> language via the project's CURRENT-ROOT
//! `codegraph.json` (`IndexPaths::extension_config`).
//!
//! Golden-safety contract: the override may ONLY add a language for an
//! extension that has NO built-in match arm AND NO embedded mapping. A
//! hostile attempt to re-map a real golden extension (`.ts`, `.py`) MUST be
//! ignored, so the byte-stable golden oracle is never perturbed.
//!
//! The overrides are loaded EXPLICITLY from the project's resolved index paths
//! and passed into `detect_language_with`, exactly as the pipeline threads them
//! through `ExtractOptions` — no directory walk, no process-CWD lookup, no cache.

use codegraph_core::IndexPaths;
use codegraph_core::types::Language;
use codegraph_extract::{ExtensionOverrides, detect_language, detect_language_with};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

/// A unique temp project directory per call (no external `tempfile` dep).
fn unique_project(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "cg_custom_ext_{tag}_{}_{nanos}_{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp project");
    dir
}

/// Write the project's current-root `codegraph.json` (the only config consulted).
fn write_codegraph_json(project: &Path, contents: &str) {
    let paths = IndexPaths::resolve(project, None).expect("resolve index paths");
    fs::create_dir_all(paths.current_root()).expect("create current root");
    fs::write(paths.extension_config(), contents).expect("write codegraph.json");
}

/// Load the project's overrides the way the pipeline does.
fn overrides_for(project: &Path) -> Arc<ExtensionOverrides> {
    let paths = IndexPaths::resolve(project, None).expect("resolve index paths");
    ExtensionOverrides::load_for_paths(&paths)
}

/// (a) A custom extension with NO built-in/embedded mapping maps to the
/// configured language.
#[test]
fn custom_ext_maps_unknown_extension() {
    let project = unique_project("a");
    write_codegraph_json(&project, r#"{"extensions": {".myext": "lua"}}"#);
    let file = project.join("foo.myext");
    fs::write(&file, "-- lua").unwrap();

    assert_eq!(
        detect_language_with(&file, &overrides_for(&project)),
        Language::Lua
    );

    fs::remove_dir_all(&project).ok();
}

/// (b) Absent codegraph.json => exact current behavior (Unknown for an
/// unmapped extension).
#[test]
fn custom_ext_absent_config_is_unknown() {
    let project = unique_project("b");
    // No current-root codegraph.json at all.
    let file = project.join("foo.myext");
    fs::write(&file, "whatever").unwrap();

    assert_eq!(
        detect_language_with(&file, &overrides_for(&project)),
        Language::Unknown
    );
    assert_eq!(detect_language(&file), Language::Unknown);

    fs::remove_dir_all(&project).ok();
}

/// (c) HOSTILE-REMAP GOLDEN-SAFETY: a config that tries to re-map the real
/// golden extensions is IGNORED for those extensions (skip-list).
#[test]
fn custom_ext_hostile_remap_is_ignored_for_builtins() {
    let project = unique_project("c");
    write_codegraph_json(
        &project,
        r#"{"extensions": {".ts": "lua", ".py": "go", ".rs": "python", ".go": "ruby"}}"#,
    );
    let overrides = overrides_for(&project);
    let ts = project.join("x.ts");
    let py = project.join("a.py");
    let rs = project.join("lib.rs");
    let go = project.join("main.go");
    fs::write(&ts, "const x = 1;").unwrap();
    fs::write(&py, "x = 1").unwrap();
    fs::write(&rs, "fn main() {}").unwrap();
    fs::write(&go, "package main").unwrap();

    assert_eq!(
        detect_language_with(&ts, &overrides),
        Language::TypeScript,
        ".ts must stay TypeScript"
    );
    assert_eq!(
        detect_language_with(&py, &overrides),
        Language::Python,
        ".py must stay Python"
    );
    assert_eq!(
        detect_language_with(&rs, &overrides),
        Language::Rust,
        ".rs must stay Rust"
    );
    assert_eq!(
        detect_language_with(&go, &overrides),
        Language::Go,
        ".go must stay Go"
    );

    fs::remove_dir_all(&project).ok();
}

/// Also: an override MUST NOT change an extension resolved by the EMBEDDED
/// pre-pass (e.g. `.vue`, `.svelte`, `.xml`).
#[test]
fn custom_ext_hostile_remap_is_ignored_for_embedded() {
    let project = unique_project("emb");
    write_codegraph_json(&project, r#"{"extensions": {".vue": "lua", ".xml": "go"}}"#);
    let overrides = overrides_for(&project);
    let vue = project.join("widget.vue");
    let xml = project.join("mapper.xml");
    fs::write(&vue, "<template></template>").unwrap();
    fs::write(&xml, "<x/>").unwrap();

    assert_eq!(
        detect_language_with(&vue, &overrides),
        Language::Vue,
        ".vue stays Vue (embedded)"
    );
    assert_eq!(
        detect_language_with(&xml, &overrides),
        Language::Xml,
        ".xml stays Xml (embedded)"
    );

    fs::remove_dir_all(&project).ok();
}

/// (d) ADVERSARIAL — no cache to go stale: rewriting `codegraph.json` and
/// reloading picks up the change immediately, with no mtime dependence (so a
/// same-second edit is honored too).
#[test]
fn custom_ext_reload_picks_up_changes_immediately() {
    let project = unique_project("d");
    write_codegraph_json(&project, r#"{"extensions": {".zz": "lua"}}"#);
    let file = project.join("foo.zz");
    fs::write(&file, "x").unwrap();

    assert_eq!(
        detect_language_with(&file, &overrides_for(&project)),
        Language::Lua
    );

    // No sleep: the reader never caches, so the very next load sees the change.
    write_codegraph_json(&project, r#"{"extensions": {".zz": "go"}}"#);

    assert_eq!(
        detect_language_with(&file, &overrides_for(&project)),
        Language::Go,
        "an explicit reload must observe the new config"
    );

    fs::remove_dir_all(&project).ok();
}

/// (e) ADVERSARIAL — malformed codegraph.json is ignored gracefully (no panic,
/// falls back to current behavior). Also: an unknown language string is
/// skipped, not fatal.
#[test]
fn custom_ext_malformed_config_is_ignored() {
    let project = unique_project("e");
    write_codegraph_json(&project, "{ this is not json ");
    let file = project.join("foo.broken");
    fs::write(&file, "x").unwrap();

    // No panic; unmapped extension -> Unknown.
    assert_eq!(
        detect_language_with(&file, &overrides_for(&project)),
        Language::Unknown
    );

    // Unknown language string is skipped (warning), other valid entries still apply.
    let project2 = unique_project("e2");
    write_codegraph_json(
        &project2,
        r#"{"extensions": {".aa": "not_a_language", ".bb": "lua"}}"#,
    );
    let overrides2 = overrides_for(&project2);
    let aa = project2.join("f.aa");
    let bb = project2.join("f.bb");
    fs::write(&aa, "x").unwrap();
    fs::write(&bb, "x").unwrap();
    assert_eq!(
        detect_language_with(&aa, &overrides2),
        Language::Unknown,
        "unknown lang skipped"
    );
    assert_eq!(
        detect_language_with(&bb, &overrides2),
        Language::Lua,
        "valid entry still applies"
    );

    fs::remove_dir_all(&project).ok();
    fs::remove_dir_all(&project2).ok();
}

/// A LEGACY `.codegraph/codegraph.json` never supplies an override, and neither
/// does another project's config.
#[test]
fn custom_ext_ignores_legacy_and_other_projects() {
    let project = unique_project("legacy");
    let other = unique_project("other");
    fs::create_dir_all(project.join(".codegraph")).unwrap();
    fs::write(
        project.join(".codegraph/codegraph.json"),
        r#"{"extensions": {".myext": "lua"}}"#,
    )
    .unwrap();
    write_codegraph_json(&other, r#"{"extensions": {".myext": "go"}}"#);
    let file = project.join("foo.myext");
    fs::write(&file, "x").unwrap();

    assert_eq!(
        detect_language_with(&file, &overrides_for(&project)),
        Language::Unknown,
        "neither the legacy config nor another project's may apply"
    );
    assert_eq!(
        detect_language_with(&file, &overrides_for(&other)),
        Language::Go,
        "the other project's own config applies to its own operations"
    );

    fs::remove_dir_all(&project).ok();
    fs::remove_dir_all(&other).ok();
}
