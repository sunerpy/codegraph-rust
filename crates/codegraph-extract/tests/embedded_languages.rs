use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::{detect_language, extract_source};
use std::fs;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[test]
fn svelte_extracts_scripts_template_calls_and_components_on_original_lines() {
    // Mirrors upstream extraction/svelte-extractor.ts:44-58,125-151,246-274,301-318.
    let result = extract_fixture("sample.svelte", Some(Language::Svelte));
    assert_no_errors(&result);

    assert_node(&result, NodeKind::Component, "sample", 1);
    assert_node(&result, NodeKind::Function, "handleClick", 6);
    assert_ref(&result, EdgeKind::Imports, "./Child.svelte", 5);
    assert_ref(&result, EdgeKind::Calls, "handleClick", 10);
    assert_ref(&result, EdgeKind::Calls, "cn", 10);
    assert_ref(&result, EdgeKind::Calls, "formatValue", 12);
    assert_ref(&result, EdgeKind::References, "Child", 11);
    assert_ref(&result, EdgeKind::References, "Widget", 12);
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_name == "$state"),
        "Svelte runes must be filtered"
    );
    println!("svelte original-line assertions passed");
}

#[test]
fn razor_extracts_markup_type_refs_and_code_block_refs_on_original_lines() {
    // Mirrors upstream extraction/razor-extractor.ts:63-77,148-183,224-278.
    let result = extract_fixture("sample.razor", Some(Language::Razor));
    assert_no_errors(&result);

    assert_node(&result, NodeKind::Component, "sample", 1);
    assert_ref(&result, EdgeKind::References, "ProductModel", 1);
    assert_ref(&result, EdgeKind::References, "ICatalogService", 2);
    assert_ref(&result, EdgeKind::References, "MainLayout", 3);
    assert_ref(&result, EdgeKind::References, "ProductGrid", 4);
    assert_ref(&result, EdgeKind::References, "CatalogItem", 4);
    assert_ref(&result, EdgeKind::Instantiates, "ProductQuery", 9);
    println!("razor original-line assertions passed");
}

#[test]
fn liquid_extracts_template_nodes_and_shopify_json_section_refs() {
    // Mirrors upstream extraction/liquid-extractor.ts:40-53,130-197,204-271,278-371.
    let result = extract_fixture("sample.liquid", Some(Language::Liquid));
    assert_no_errors(&result);

    assert_node(&result, NodeKind::File, "sample.liquid", 1);
    assert_node(&result, NodeKind::Variable, "title", 1);
    assert_node(&result, NodeKind::Constant, "Featured", 5);
    assert_ref(
        &result,
        EdgeKind::References,
        "snippets/card-product.liquid",
        2,
    );
    assert_ref(
        &result,
        EdgeKind::References,
        "snippets/icon-star.liquid",
        3,
    );
    assert_ref(
        &result,
        EdgeKind::References,
        "sections/featured-collection.liquid",
        4,
    );

    let json = extract_fixture("templates/product.json", None);
    assert_no_errors(&json);
    assert_ref(
        &json,
        EdgeKind::References,
        "sections/main-product.liquid",
        1,
    );
    assert_ref(
        &json,
        EdgeKind::References,
        "sections/related-products.liquid",
        1,
    );
    assert_eq!(detect_language("templates/product.json"), Language::Liquid);
    println!("liquid original-line assertions passed");
}

#[test]
fn mybatis_extracts_mapper_methods_and_include_refs_on_original_lines() {
    // Mirrors upstream extraction/mybatis-extractor.ts:45-50,94-160,180-197.
    let result = extract_fixture("mapper.xml", Some(Language::Xml));
    assert_no_errors(&result);

    assert_node(&result, NodeKind::File, "mapper.xml", 1);
    assert_node(&result, NodeKind::Method, "BaseColumns", 2);
    let select = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Method && node.name == "findById")
        .expect("select node");
    assert_eq!(select.start_line, 5);
    assert_eq!(select.qualified_name, "com.example.UserMapper::findById");
    assert_eq!(
        select.signature.as_deref(),
        Some("SELECT param=int result=User")
    );
    assert_ref(
        &result,
        EdgeKind::References,
        "com.example.UserMapper::BaseColumns",
        6,
    );
    println!("mybatis original-line assertions passed");
}

#[test]
fn mybatis_accepts_the_ibatis_sqlmap_root_form() {
    // Upstream #1182: the iBatis 2 `<sqlMap namespace="…">` root carries the same
    // statement elements as MyBatis `<mapper>`; before this port the root regex
    // only matched `<mapper`, so a `.xml` sqlMap produced ONLY a file node.
    let result = extract_fixture("legacy_sqlmap.xml", Some(Language::Xml));
    assert_no_errors(&result);

    assert_node(&result, NodeKind::File, "legacy_sqlmap.xml", 1);
    assert_node(&result, NodeKind::Method, "legacyColumns", 4);
    assert_node(&result, NodeKind::Method, "legacySelect", 7);
    assert_node(&result, NodeKind::Method, "legacyUpdate", 10);

    let select = result
        .nodes
        .iter()
        .find(|node| node.name == "legacySelect")
        .expect("legacySelect node");
    assert_eq!(select.qualified_name, "Legacy.AccountMap::legacySelect");
    assert_ref(
        &result,
        EdgeKind::References,
        "Legacy.AccountMap::legacyColumns",
        8,
    );
}

#[test]
fn mybatis_ignores_the_ibatis_sqlmapconfig_root() {
    // `<sqlMapConfig>` is the iBatis *config* file: it only points at the real
    // statement maps, so the `\b` in the root regex must keep the `sqlMap` prefix
    // of its name from being read as a statement-map root. The stray `<select>`
    // is the hostile part: without the word boundary the config becomes a root
    // whose body runs to EOF and that statement gets attributed to it.
    let source = concat!(
        "<sqlMapConfig namespace=\"Config\">\n",
        "  <sqlMap resource=\"Legacy.xml\" />\n",
        "  <select id=\"strayStatement\">SELECT 1</select>\n",
        "</sqlMapConfig>\n",
    );
    let result = extract_source("sqlMapConfig.xml", source, Some(Language::Xml));
    assert_no_errors(&result);
    assert_eq!(
        result.nodes.len(),
        1,
        "sqlMapConfig keeps only the file node; nodes={:#?}",
        result.nodes
    );
    assert_eq!(result.nodes[0].kind, NodeKind::File);
}

#[test]
fn mybatis_qualified_refid_keeps_its_namespace_and_only_splits_the_fragment_id() {
    // Upstream #1209: a namespace-qualified `refid` must become the exact
    // `{namespace}::{id}` the `<sql>` node carries — every dot BEFORE the last one
    // belongs to the namespace. Rewriting them all to `::` produced
    // `com::example::UserMapper::baseColumns`, a name no node has, so the include
    // stayed unresolved.
    let result = extract_fixture("qualified_mapper.xml", Some(Language::Xml));
    assert_no_errors(&result);

    assert_node(&result, NodeKind::Method, "baseColumns", 4);
    assert_ref(
        &result,
        EdgeKind::References,
        "com.example.UserMapper::baseColumns",
        8,
    );
    assert_ref(
        &result,
        EdgeKind::References,
        "com.example.OrderMapper::orderColumns",
        14,
    );

    let bare = result
        .unresolved_references
        .iter()
        .find(|reference| reference.line == 8)
        .expect("bare refid on line 8");
    let qualified = result
        .unresolved_references
        .iter()
        .find(|reference| reference.line == 11)
        .expect("qualified refid on line 11");
    assert_eq!(
        bare.reference_name, qualified.reference_name,
        "a bare refid and the same fragment written out in full must name one node"
    );
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_name.starts_with("com::example")),
        "namespace dots must survive; refs={:#?}",
        result.unresolved_references
    );
}

#[test]
fn astro_extracts_frontmatter_scripts_and_template_refs_on_original_lines() {
    // Mirrors upstream extraction/astro-extractor.ts:48-69,123-235.
    let result = extract_fixture("sample.astro", Some(Language::Astro));
    assert_no_errors(&result);

    // Component node + delegated TS frontmatter/script symbols on their
    // .astro lines (the upstream keeps the delegated file node; its line depends on
    // node-id dedup ordering, so it is not asserted here).
    assert_node(&result, NodeKind::Component, "sample", 1);
    assert_node(&result, NodeKind::Import, "./Layout.astro", 2);
    assert_node(&result, NodeKind::Import, "../utils/date", 3);
    assert_node(&result, NodeKind::Function, "getTitle", 5);
    assert_node(&result, NodeKind::Constant, "posts", 9);
    assert_node(&result, NodeKind::Function, "clientHandler", 21);

    // Template component usages (PascalCase) — Fragment is a builtin, skipped.
    assert_ref(&result, EdgeKind::References, "Layout", 12);
    assert_ref(&result, EdgeKind::References, "PostCard", 15);
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_name == "Fragment"),
        "Fragment is an Astro builtin and must be skipped"
    );
    // Template expression calls.
    assert_ref(&result, EdgeKind::Calls, "formatDate", 13);
    assert_ref(&result, EdgeKind::Calls, "posts.map", 14);
    println!("astro original-line assertions passed");
}

fn extract_fixture(path: &str, language: Option<Language>) -> ExtractionResult {
    let full = format!("{FIXTURES}/{path}");
    let source = fs::read_to_string(full).unwrap();
    extract_source(path, &source, language)
}

fn assert_no_errors(result: &ExtractionResult) {
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

fn assert_node(result: &ExtractionResult, kind: NodeKind, name: &str, line: i64) {
    let node = result
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| {
            panic!(
                "missing node kind={kind} name={name}; nodes={:#?}",
                result.nodes
            )
        });
    assert_eq!(node.start_line, line, "node line for {name}");
}

fn assert_ref(result: &ExtractionResult, kind: EdgeKind, name: &str, line: i64) {
    let reference = result
        .unresolved_references
        .iter()
        .find(|reference| reference.reference_kind == kind && reference.reference_name == name)
        .unwrap_or_else(|| {
            panic!(
                "missing ref kind={kind} name={name}; refs={:#?}",
                result.unresolved_references
            )
        });
    assert_eq!(reference.line, line, "reference line for {name}");
}
