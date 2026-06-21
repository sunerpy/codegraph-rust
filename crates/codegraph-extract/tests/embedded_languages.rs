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
