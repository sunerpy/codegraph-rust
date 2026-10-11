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

/// `(kind, qualified name)` of every constant, variable and field in `source`.
fn values(path: &str, source: &str, language: Language) -> Vec<(String, String)> {
    let result = extract_source(path, source, Some(language));
    let mut values: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                codegraph_core::types::NodeKind::Constant
                    | codegraph_core::types::NodeKind::Variable
                    | codegraph_core::types::NodeKind::Field
            )
        })
        .map(|node| (format!("{:?}", node.kind), node.qualified_name.clone()))
        .collect();
    values.sort();
    values
}

/// PHP: a file-scope `const` and a class `const` are constants, read bare or
/// through `self::` / `Config::`; a static property is mutable class state
/// and no target.
#[test]
fn php_readers_of_file_and_class_constants() {
    let source = "<?php\nconst APP_VERSION = \"1.0\";\nclass Config {\n  const MAX_ITEMS = 100;\n  const STATUS_NAMES = [\"ok\", \"fail\"];\n  public static $counter = 0;\n  function capped($n) { return $n > self::MAX_ITEMS ? self::MAX_ITEMS : $n; }\n  function label($i) { return Config::STATUS_NAMES[$i]; }\n  function version() { return APP_VERSION; }\n}";
    assert_eq!(
        values("Config.php", source, Language::Php),
        [
            ("Constant".to_string(), "APP_VERSION".to_string()),
            ("Constant".to_string(), "Config::MAX_ITEMS".to_string()),
            ("Constant".to_string(), "Config::STATUS_NAMES".to_string()),
        ]
    );
    assert_eq!(
        readers("Config.php", source, Language::Php, "MAX_ITEMS"),
        ["capped"]
    );
    assert_eq!(
        readers("Config.php", source, Language::Php, "STATUS_NAMES"),
        ["label"]
    );
    assert_eq!(
        readers("Config.php", source, Language::Php, "APP_VERSION"),
        ["version"]
    );
    assert!(readers("Config.php", source, Language::Php, "counter").is_empty());
}

/// Scala: an `object` is a singleton, so its `val`s are shared constants, as
/// a top-level `val` is; a `class` `val` is a per-instance field.
#[test]
fn scala_readers_of_top_level_and_object_values() {
    let source = "val AppVersion = \"1.0\"\nobject Config {\n  val TIMEOUT_MS = 30\n  val STATUS_NAMES = List(\"ok\", \"fail\")\n  def capped(n: Int): Int = if (n > TIMEOUT_MS) TIMEOUT_MS else n\n  def label(i: Int): String = STATUS_NAMES(i)\n}\nclass Widget {\n  val MaxItems = 100\n  def within(n: Int): Int = if (n < MaxItems) n else MaxItems\n}";
    assert_eq!(
        values("Demo.scala", source, Language::Scala),
        [
            ("Constant".to_string(), "AppVersion".to_string()),
            ("Constant".to_string(), "Config::STATUS_NAMES".to_string()),
            ("Constant".to_string(), "Config::TIMEOUT_MS".to_string()),
            ("Field".to_string(), "Widget::MaxItems".to_string()),
        ]
    );
    assert_eq!(
        readers("Demo.scala", source, Language::Scala, "TIMEOUT_MS"),
        ["capped"]
    );
    assert_eq!(
        readers("Demo.scala", source, Language::Scala, "STATUS_NAMES"),
        ["label"]
    );
    assert!(readers("Demo.scala", source, Language::Scala, "MaxItems").is_empty());
}

#[test]
fn a_scala_object_value_shadowed_by_a_local_has_no_readers() {
    let source = "object Config {\n  val TIMEOUT = 30\n  def usesConst(): Int = TIMEOUT\n  def shadows(): Int = { val TIMEOUT = 5; TIMEOUT }\n}";
    assert!(readers("Shadow.scala", source, Language::Scala, "TIMEOUT").is_empty());
}

/// Rust: a module-level `const` and `static` are values (upstream's generic
/// path names them `variable`).
#[test]
fn rust_readers_of_a_module_const_and_static() {
    let source = "const MAX_RETRIES: u32 = 3;\nstatic DEFAULT_LABEL: &str = \"prod\";\n\nfn retry() -> u32 { MAX_RETRIES }\nfn label() -> &'static str { DEFAULT_LABEL }";
    assert_eq!(
        values("lib.rs", source, Language::Rust),
        [
            ("Variable".to_string(), "DEFAULT_LABEL".to_string()),
            ("Variable".to_string(), "MAX_RETRIES".to_string()),
        ]
    );
    assert_eq!(
        readers("lib.rs", source, Language::Rust, "MAX_RETRIES"),
        ["retry"]
    );
    assert_eq!(
        readers("lib.rs", source, Language::Rust, "DEFAULT_LABEL"),
        ["label"]
    );
}

#[test]
fn a_rust_const_shadowed_by_a_let_has_no_readers() {
    let source = "const TIMEOUT: u32 = 30;\n\nfn uses_const() -> u32 { TIMEOUT }\nfn shadows() -> u32 {\n    let TIMEOUT = 5;\n    TIMEOUT\n}";
    assert!(readers("shadow.rs", source, Language::Rust, "TIMEOUT").is_empty());
}

/// Ruby keeps most constants inside a class or module; both a top-level and
/// a class constant are values, read by the methods around them.
#[test]
fn ruby_readers_of_top_level_and_class_constants() {
    let source = "MAX_RETRIES = 3\n\ndef retry_count\n  MAX_RETRIES\nend\n\nclass Config\n  TIMEOUT = 30\n  def self.get_timeout\n    TIMEOUT\n  end\n  def describe\n    \"timeout=#{TIMEOUT}\"\n  end\nend";
    assert_eq!(
        values("app.rb", source, Language::Ruby),
        [
            ("Variable".to_string(), "Config::TIMEOUT".to_string()),
            ("Variable".to_string(), "MAX_RETRIES".to_string()),
        ]
    );
    assert_eq!(
        readers("app.rb", source, Language::Ruby, "MAX_RETRIES"),
        ["retry_count"]
    );
    assert_eq!(
        readers("app.rb", source, Language::Ruby, "TIMEOUT"),
        ["describe", "get_timeout"]
    );
}

/// C: a file-scope `static const` scalar and lookup table are constants,
/// plain globals are variables; a prototype is none.
#[test]
fn c_readers_of_file_scope_constants() {
    let source = "static const int MAX_ITEMS = 100;\nstatic const char *const STATUS_NAMES[] = { \"ok\", \"fail\", \"pending\" };\nint counter_total = 0;\nint capped(int n);\n\nint capped(int n) { return n > MAX_ITEMS ? MAX_ITEMS : n; }\nconst char *label(int i) { return STATUS_NAMES[i]; }";
    assert_eq!(
        values("config.c", source, Language::C),
        [
            ("Constant".to_string(), "MAX_ITEMS".to_string()),
            ("Constant".to_string(), "STATUS_NAMES".to_string()),
            ("Variable".to_string(), "counter_total".to_string()),
        ]
    );
    assert_eq!(
        readers("config.c", source, Language::C, "MAX_ITEMS"),
        ["capped"]
    );
    assert_eq!(
        readers("config.c", source, Language::C, "STATUS_NAMES"),
        ["label"]
    );
}

#[test]
fn a_c_constant_shadowed_by_a_local_has_no_readers() {
    let source = "static const int TIMEOUT = 30;\n\nint uses_const(void) { return TIMEOUT; }\nint shadows(void) {\n    int TIMEOUT = 5;\n    return TIMEOUT;\n}";
    assert!(readers("shadow.c", source, Language::C, "TIMEOUT").is_empty());
}

/// A prototype led by an unknown macro (`CURL_EXTERN CURLcode fn(int);`)
/// never mints a value named by its return type.
#[test]
fn a_macro_prefixed_c_prototype_mints_no_value() {
    let source = "typedef enum { CURLE_OK, CURLE_FAIL } CURLcode;\nCURL_EXTERN CURLcode curl_easy_init(int x);\nCURL_EXTERN CURLcode curl_easy_setopt(int y);\n\nstatic const int REAL_LIMIT = 42;\nint use_real(void) { return REAL_LIMIT; }";
    assert_eq!(
        values("api.c", source, Language::C),
        [("Constant".to_string(), "REAL_LIMIT".to_string())]
    );
    assert_eq!(
        readers("api.c", source, Language::C, "REAL_LIMIT"),
        ["use_real"]
    );
}

/// Pascal: a unit `const` is a constant; a routine's body is its header's
/// sibling, and a routine's own `const` section binds the name again.
#[test]
fn pascal_readers_of_unit_constants() {
    let source = "unit Demo;\ninterface\nconst\n  MAX_ITEMS = 100;\n  APP_NAME = 'MyApp';\nimplementation\nfunction Capped(n: Integer): Integer;\nbegin\n  if n > MAX_ITEMS then Capped := MAX_ITEMS else Capped := n;\nend;\nfunction AppLabel: string;\nbegin\n  AppLabel := APP_NAME;\nend;\nend.";
    assert_eq!(
        values("demo.pas", source, Language::Pascal),
        [
            ("Constant".to_string(), "APP_NAME".to_string()),
            ("Constant".to_string(), "MAX_ITEMS".to_string()),
        ]
    );
    assert_eq!(
        readers("demo.pas", source, Language::Pascal, "MAX_ITEMS"),
        ["Capped"]
    );
    assert_eq!(
        readers("demo.pas", source, Language::Pascal, "APP_NAME"),
        ["AppLabel"]
    );
    let shadow = "unit Shadow;\ninterface\nconst\n  TIMEOUT = 30;\nimplementation\nfunction UsesConst: Integer;\nbegin\n  UsesConst := TIMEOUT;\nend;\nfunction Shadows: Integer;\nconst TIMEOUT = 5;\nbegin\n  Shadows := TIMEOUT;\nend;\nend.";
    assert_eq!(
        values("shadow.pas", shadow, Language::Pascal),
        [("Constant".to_string(), "TIMEOUT".to_string())]
    );
    assert!(readers("shadow.pas", shadow, Language::Pascal, "TIMEOUT").is_empty());
}

fn row(kind: &str, name: &str) -> (String, String) {
    (kind.to_string(), name.to_string())
}

/// Java: a `static final` field is a constant (OD-5: it was a field); an
/// instance `final` field is per-object state, a field and no target.
#[test]
fn java_readers_of_static_final_constants() {
    let source = "class Limits {\n  public static final int MAX_ITEMS = 100;\n  static final String[] STATUS_NAMES = { \"ok\", \"fail\" };\n  final int instanceId = 1;\n  int capped(int n) { return n > MAX_ITEMS ? MAX_ITEMS : n; }\n  String label(int i) { return STATUS_NAMES[i]; }\n  int id() { return instanceId; }\n}";
    assert_eq!(
        values("Limits.java", source, Language::Java),
        [
            row("Constant", "Limits::MAX_ITEMS"),
            row("Constant", "Limits::STATUS_NAMES"),
            row("Field", "Limits::instanceId"),
        ]
    );
    assert_eq!(
        readers("Limits.java", source, Language::Java, "MAX_ITEMS"),
        ["capped"]
    );
    assert_eq!(
        readers("Limits.java", source, Language::Java, "STATUS_NAMES"),
        ["label"]
    );
    assert!(readers("Limits.java", source, Language::Java, "instanceId").is_empty());
}

#[test]
fn a_java_constant_shadowed_by_a_local_has_no_readers() {
    let source = "class Shadow {\n  static final int TIMEOUT = 30;\n  int usesConst() { return TIMEOUT; }\n  int shadows() { int TIMEOUT = 5; return TIMEOUT; }\n}";
    assert!(readers("Shadow.java", source, Language::Java, "TIMEOUT").is_empty());
}

/// C#: `const` and `static readonly` fields are constants; an instance
/// `readonly` field stays a field.
#[test]
fn csharp_readers_of_const_and_static_readonly_fields() {
    let source = "class Limits {\n  const int MAX_ITEMS = 100;\n  static readonly string[] STATUS_NAMES = { \"ok\", \"fail\" };\n  readonly int instanceId = 1;\n  int Capped(int n) { return n > MAX_ITEMS ? MAX_ITEMS : n; }\n  string Label(int i) { return STATUS_NAMES[i]; }\n  int Id() { return instanceId; }\n}";
    assert_eq!(
        values("Limits.cs", source, Language::CSharp),
        [
            row("Constant", "Limits::MAX_ITEMS"),
            row("Constant", "Limits::STATUS_NAMES"),
            row("Field", "Limits::instanceId"),
        ]
    );
    assert_eq!(
        readers("Limits.cs", source, Language::CSharp, "MAX_ITEMS"),
        ["Capped"]
    );
    assert_eq!(
        readers("Limits.cs", source, Language::CSharp, "STATUS_NAMES"),
        ["Label"]
    );
    assert!(readers("Limits.cs", source, Language::CSharp, "instanceId").is_empty());
}

#[test]
fn a_csharp_constant_shadowed_by_a_local_has_no_readers() {
    let source = "class Shadow {\n  const int TIMEOUT = 30;\n  int UsesConst() { return TIMEOUT; }\n  int Shadows() { int TIMEOUT = 5; return TIMEOUT; }\n}";
    assert!(readers("Shadow.cs", source, Language::CSharp, "TIMEOUT").is_empty());
}

/// Kotlin: a top-level property and an `object`'s or companion object's are
/// shared (`val` a constant, `var` a variable); a class property is a field;
/// a local is no node.
#[test]
fn kotlin_readers_of_top_level_object_and_companion_constants() {
    let source = "const val TOP_LEVEL_MAX = 100\nobject Config {\n  const val TIMEOUT_MS = 30\n  val STATUS_NAMES = listOf(\"ok\", \"fail\")\n  fun capped(n: Int): Int = if (n > TIMEOUT_MS) TIMEOUT_MS else n\n  fun label(i: Int): String = STATUS_NAMES[i]\n}\nclass Widget {\n  companion object { const val MAX_RETRIES = 3 }\n  val instanceField = 1\n  fun retries(): Int = MAX_RETRIES\n  fun within(n: Int): Int = if (n < TOP_LEVEL_MAX) n else TOP_LEVEL_MAX\n}";
    assert_eq!(
        values("Demo.kt", source, Language::Kotlin),
        [
            row("Constant", "Config::STATUS_NAMES"),
            row("Constant", "Config::TIMEOUT_MS"),
            row("Constant", "TOP_LEVEL_MAX"),
            row("Constant", "Widget::MAX_RETRIES"),
            row("Field", "Widget::instanceField"),
        ]
    );
    assert_eq!(
        readers("Demo.kt", source, Language::Kotlin, "STATUS_NAMES"),
        ["label"]
    );
    assert_eq!(
        readers("Demo.kt", source, Language::Kotlin, "MAX_RETRIES"),
        ["retries"]
    );
    assert_eq!(
        readers("Demo.kt", source, Language::Kotlin, "TOP_LEVEL_MAX"),
        ["within"]
    );
    assert!(readers("Demo.kt", source, Language::Kotlin, "instanceField").is_empty());
}

#[test]
fn a_kotlin_constant_shadowed_by_a_local_has_no_readers() {
    let source = "object Config {\n  const val TIMEOUT = 30\n  fun usesConst(): Int = TIMEOUT\n  fun shadows(): Int { val TIMEOUT = 5; return TIMEOUT }\n}";
    assert!(readers("Shadow.kt", source, Language::Kotlin, "TIMEOUT").is_empty());
}

/// Swift: a top-level `let` and a type's `static let` are constants; an
/// instance stored `let` is a field; a computed property is a property.
#[test]
fn swift_readers_of_top_level_and_static_constants() {
    let source = "let topLevelMax = 100\nenum Constants {\n  static let TIMEOUT_MS = 30\n  static let STATUS_NAMES = [\"ok\", \"fail\"]\n}\nstruct Widget {\n  static let MAX_RETRIES = 3\n  let instanceField = 1\n  func retries() -> Int { return Widget.MAX_RETRIES }\n  func within(_ n: Int) -> Int { return n < topLevelMax ? n : topLevelMax }\n}\nfunc labels(_ i: Int) -> String { return Constants.STATUS_NAMES[i] }";
    assert_eq!(
        values("Demo.swift", source, Language::Swift),
        [
            row("Constant", "Constants::STATUS_NAMES"),
            row("Constant", "Constants::TIMEOUT_MS"),
            row("Constant", "Widget::MAX_RETRIES"),
            row("Constant", "topLevelMax"),
            row("Field", "Widget::instanceField"),
        ]
    );
    assert_eq!(
        readers("Demo.swift", source, Language::Swift, "STATUS_NAMES"),
        ["labels"]
    );
    assert_eq!(
        readers("Demo.swift", source, Language::Swift, "MAX_RETRIES"),
        ["retries"]
    );
    assert_eq!(
        readers("Demo.swift", source, Language::Swift, "topLevelMax"),
        ["within"]
    );
    assert!(readers("Demo.swift", source, Language::Swift, "instanceField").is_empty());
}

#[test]
fn a_swift_constant_shadowed_by_a_local_has_no_readers() {
    let source = "enum Config {\n  static let TIMEOUT = 30\n  static func usesConst() -> Int { return TIMEOUT }\n  static func shadows() -> Int { let TIMEOUT = 5; return TIMEOUT }\n}";
    assert!(readers("Shadow.swift", source, Language::Swift, "TIMEOUT").is_empty());
}

/// Dart: a top-level `const`/`final` and a class `static const`/`static
/// final` (`static_final_declaration`) are constants; an instance field is
/// none.
#[test]
fn dart_readers_of_top_level_and_static_constants() {
    let source = "const TOP_LEVEL_MAX = 100;\nclass Config {\n  static const TIMEOUT_MS = 30;\n  static final STATUS_NAMES = [\"ok\", \"fail\"];\n  final int instanceField = 1;\n  int capped(int n) => n > TIMEOUT_MS ? TIMEOUT_MS : n;\n  String label(int i) { return STATUS_NAMES[i]; }\n  int withinLimit(int n) => n < TOP_LEVEL_MAX ? n : TOP_LEVEL_MAX;\n}";
    assert_eq!(
        values("demo.dart", source, Language::Dart),
        [
            row("Constant", "Config::STATUS_NAMES"),
            row("Constant", "Config::TIMEOUT_MS"),
            row("Constant", "TOP_LEVEL_MAX"),
        ]
    );
    assert_eq!(
        readers("demo.dart", source, Language::Dart, "TIMEOUT_MS"),
        ["capped"]
    );
    assert_eq!(
        readers("demo.dart", source, Language::Dart, "STATUS_NAMES"),
        ["label"]
    );
    assert_eq!(
        readers("demo.dart", source, Language::Dart, "TOP_LEVEL_MAX"),
        ["withinLimit"]
    );
}

#[test]
fn a_dart_constant_shadowed_by_a_local_has_no_readers() {
    let source = "const TIMEOUT = 30;\nclass C {\n  int usesConst() => TIMEOUT;\n  int shadows() { const TIMEOUT = 5; return TIMEOUT; }\n}";
    assert!(readers("shadow.dart", source, Language::Dart, "TIMEOUT").is_empty());
}
