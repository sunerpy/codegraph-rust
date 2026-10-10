//! Same-file value references (upstream #895, #897): a `references` edge,
//! marked `{"valueRef": true}`, from a symbol to a constant or variable of its
//! own file that it reads, so impact analysis reaches the readers of a shared
//! value. Only a distinctive name is a target, and a name the file binds again
//! in an inner scope is none, since its nested readers read that binding.
//! Always on (OD-8). The sources are upstream's own test fixtures where they
//! exist.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// The names of the symbols with a value reference to a node named `target`.
fn readers(path: &str, source: &str, language: Language, target: &str) -> Vec<String> {
    let result = extract_source(path, source, Some(language));
    let name_of = |id: &str| {
        result
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(|| id.to_string(), |node| node.name.clone())
    };
    let mut names: Vec<_> = result
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::References
                && edge.metadata == Some(serde_json::json!({ "valueRef": true }))
                && result
                    .nodes
                    .iter()
                    .any(|node| node.id == edge.target && node.name == target)
        })
        .map(|edge| name_of(&edge.source))
        .collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn typescript_readers_of_a_file_scope_constant() {
    let source = "export const TABLE_CONFIG = { rows: 10, cols: 4 };\nexport function rowCount() { return TABLE_CONFIG.rows; }\nexport function describeTable() { return `${TABLE_CONFIG.rows}x${TABLE_CONFIG.cols}`; }\nexport const HEADER = TABLE_CONFIG.cols;";
    assert_eq!(
        readers("config.ts", source, Language::TypeScript, "TABLE_CONFIG"),
        ["HEADER", "describeTable", "rowCount"]
    );
}

/// The bundled pattern: a file-scope `const Module` bound again by an inner
/// `var Module`. Nested readers read the inner binding, so the outer constant
/// is no target.
#[test]
fn a_constant_bound_again_in_an_inner_scope_has_no_readers() {
    let source = "const Module = (function () {\n  return function (Module) {\n    var Module = typeof Module !== \"undefined\" ? Module : {};\n    function locate() { return Module.path; }\n    function getFunc() { return Module.lookup; }\n    return { locate, getFunc };\n  };\n})();\nexport default Module;";
    assert!(readers("bundled.ts", source, Language::TypeScript, "Module").is_empty());
}

#[test]
fn tsx_readers_inside_jsx() {
    let source = "export const THEME_TOKENS = { color: \"red\", size: 12 };\nexport function Label() {\n  return <span style={{ color: THEME_TOKENS.color }}>hi</span>;\n}\nexport const Box = () => <div data-size={THEME_TOKENS.size} />;";
    assert_eq!(
        readers("widget.tsx", source, Language::Tsx, "THEME_TOKENS"),
        ["Box", "Label"]
    );
}

/// A name of fewer than three characters, or with no uppercase letter or
/// `_`, is too common to be a target; a symbol never reads itself.
#[test]
fn only_distinctive_names_are_targets() {
    let source = "const ID = 1;\nconst limit = 2;\nconst MAX_ROWS = ID + limit;\nfunction rows() { return ID + limit + MAX_ROWS; }\n";
    assert!(readers("rows.js", source, Language::JavaScript, "ID").is_empty());
    assert!(readers("rows.js", source, Language::JavaScript, "limit").is_empty());
    assert_eq!(
        readers("rows.js", source, Language::JavaScript, "MAX_ROWS"),
        ["rows"]
    );
}

#[test]
fn go_readers_of_a_package_constant_and_variable() {
    let source = "package main\n\nconst MaxRetries = 3\nvar DefaultLabels = map[string]string{\"env\": \"prod\"}\n\nfunc retry() int { return MaxRetries }\nfunc labels() map[string]string { return DefaultLabels }";
    assert_eq!(
        readers("main.go", source, Language::Go, "MaxRetries"),
        ["retry"]
    );
    assert_eq!(
        readers("main.go", source, Language::Go, "DefaultLabels"),
        ["labels"]
    );
}

#[test]
fn a_go_constant_shadowed_by_a_short_variable_has_no_readers() {
    let source = "package main\n\nconst Timeout = 30\n\nfunc usesConst() int { return Timeout }\nfunc shadows() int {\n\tTimeout := 5\n\treturn Timeout\n}";
    assert!(readers("shadow.go", source, Language::Go, "Timeout").is_empty());
}

/// A module constant defined on both branches of a `try` is one value, not a
/// shadow; its two halves never read each other.
#[test]
fn a_conditionally_defined_python_constant_keeps_its_reader() {
    let source = "try:\n\tHAS_SSL = True\nexcept ImportError:\n\tHAS_SSL = False\n\ndef uses_ssl():\n\treturn HAS_SSL";
    assert_eq!(
        readers("cond.py", source, Language::Python, "HAS_SSL"),
        ["uses_ssl"]
    );
}

/// A Vue SFC's script is the TypeScript extractor's, so it reads values the
/// same way; a `<script setup>` top-level constant is the component's, so
/// what it reads is read by the component.
#[test]
fn a_vue_script_inherits_value_references() {
    let source = "<script setup lang=\"ts\">\nconst PAGE_SIZE = 20;\nconst LIMITS = { max: PAGE_SIZE };\nfunction pages(total: number) { return Math.ceil(total / PAGE_SIZE); }\n</script>\n\n<template>\n  <p>{{ pages(100) }}</p>\n</template>\n";
    assert_eq!(
        readers("src/App.vue", source, Language::Vue, "PAGE_SIZE"),
        ["App", "pages"]
    );
}

/// Svelte and Astro scripts are the script extractor's too.
#[test]
fn svelte_and_astro_scripts_inherit_value_references() {
    let svelte = "<script>\n  const PAGE_SIZE = 20;\n  function pages(total) { return total / PAGE_SIZE; }\n</script>\n\n<p>{pages(100)}</p>\n";
    assert_eq!(
        readers("src/App.svelte", svelte, Language::Svelte, "PAGE_SIZE"),
        ["pages"]
    );
    let astro = "---\nconst PAGE_SIZE = 20;\nfunction pages(total: number) { return total / PAGE_SIZE; }\n---\n<p>{pages(100)}</p>\n";
    assert_eq!(
        readers("src/pages/index.astro", astro, Language::Astro, "PAGE_SIZE"),
        ["pages"]
    );
}
