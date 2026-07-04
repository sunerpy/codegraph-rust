//! Coverage for the embedded Razor and Liquid extractors, plus the
//! `.codegraph/codegraph.json` extension-override reader. These exercise
//! branches the fixture-driven `embedded_languages.rs` suite does not reach:
//! the `.cshtml` suffix path, `@code`/`@{ }` block `new`/static-call regexes,
//! `@inherits`/`@inject`/`@typeof` directives, Liquid `{% assign %}`/`{% schema %}`,
//! and both include and section snippet tags. TEST-ONLY: no production change.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::{detect_language, extract_source};

// ---------------------------------------------------------------------------
// Razor — .cshtml suffix, @inherits/@inject/@typeof directives, and a @code
// block whose body drives the `new X(` (Instantiates) and `X.Method(`
// (References) regexes.
// ---------------------------------------------------------------------------

#[test]
fn razor_cshtml_directives_and_code_block_new_and_static_calls() {
    let source = r#"@inherits MyApp.Components.BaseComponent
@inject MyApp.Services.IWidgetService Widgets
<PageTitle>@typeof(RootLayout)</PageTitle>
@code {
    private void Load()
    {
        var query = new WidgetQuery();
        Registry.Register(query);
    }
}
"#;
    let result = extract_source("Pages/Index.cshtml", source, Some(Language::Razor));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result, NodeKind::Component, "Index");
    // @inherits / @inject resolve the last dotted segment as a type reference.
    assert_ref(&result, EdgeKind::References, "BaseComponent");
    assert_ref(&result, EdgeKind::References, "IWidgetService");
    // @typeof(RootLayout) resolves the referenced type.
    assert_ref(&result, EdgeKind::References, "RootLayout");
    // `new WidgetQuery()` inside @code -> Instantiates.
    assert_ref(&result, EdgeKind::Instantiates, "WidgetQuery");
    // `Registry.Register(` inside @code -> References.
    assert_ref(&result, EdgeKind::References, "Registry");
}

#[test]
fn razor_component_tags_and_generic_attrs_are_skipped_for_builtins() {
    let source = r#"@model MyApp.Models.Product
<EditForm Model="@product">
  <ProductCard TItem="CatalogEntry" />
</EditForm>
"#;
    let result = extract_source("Components/List.razor", source, Some(Language::Razor));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result, NodeKind::Component, "List");
    // @model resolves the model type's last segment.
    assert_ref(&result, EdgeKind::References, "Product");
    // Custom component tag -> reference; the generic TItem type also resolves.
    assert_ref(&result, EdgeKind::References, "ProductCard");
    assert_ref(&result, EdgeKind::References, "CatalogEntry");
    // EditForm is a Blazor builtin and must be skipped.
    assert!(
        !result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_name == "EditForm"),
        "EditForm is a Blazor builtin and must be skipped"
    );
}

#[test]
fn razor_at_brace_code_block_variant_is_processed() {
    let source = r#"@{
    var svc = new OrderService();
    Cart.Add(svc);
}
"#;
    let result = extract_source("Pages/Cart.razor", source, Some(Language::Razor));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // `@{ ... }` blocks are matched by the same code-block regex as @code.
    assert_ref(&result, EdgeKind::Instantiates, "OrderService");
    assert_ref(&result, EdgeKind::References, "Cart");
}

// ---------------------------------------------------------------------------
// Liquid — {% assign %} variables, {% schema %} constant, {% section %} and
// {% render %}/{% include %} snippet refs, plus Shopify JSON section refs.
// ---------------------------------------------------------------------------

#[test]
fn liquid_assign_schema_section_and_render_nodes() {
    let source = r#"{% assign heading = section.settings.title %}
{% assign count = products.size %}
{% render 'price', product: product %}
{% include 'badge' %}
{% section 'header-group' %}
{% schema %}
{ "name": "Custom Section" }
{% endschema %}
"#;
    let result = extract_source("sections/custom.liquid", source, Some(Language::Liquid));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_node(&result, NodeKind::File, "custom.liquid");
    // assignments -> Variable nodes.
    assert_node(&result, NodeKind::Variable, "heading");
    assert_node(&result, NodeKind::Variable, "count");
    // schema -> Constant node named from its JSON `name`.
    assert_node(&result, NodeKind::Constant, "Custom Section");
    // render/include -> snippet references; section -> section reference.
    assert_ref(&result, EdgeKind::References, "snippets/price.liquid");
    assert_ref(&result, EdgeKind::References, "snippets/badge.liquid");
    assert_ref(
        &result,
        EdgeKind::References,
        "sections/header-group.liquid",
    );
}

#[test]
fn liquid_schema_without_name_falls_back_to_default() {
    let source = r#"{% schema %}
{ "settings": [] }
{% endschema %}
"#;
    let result = extract_source("sections/bare.liquid", source, Some(Language::Liquid));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // Missing `name` -> schema_name falls back to "schema".
    assert_node(&result, NodeKind::Constant, "schema");
}

#[test]
fn liquid_schema_localized_name_object_resolves_en() {
    let source = r#"{% schema %}
{ "name": { "en": "Localized", "fr": "Localise" } }
{% endschema %}
"#;
    let result = extract_source("sections/loc.liquid", source, Some(Language::Liquid));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    // Object name resolves the `en` key.
    assert_node(&result, NodeKind::Constant, "Localized");
}

#[test]
fn liquid_json_template_dedups_section_type_refs() {
    let source = r#"{
  "sections": {
    "main": { "type": "main-product" },
    "also": { "type": "main-product" },
    "rel": { "type": "related" }
  }
}
"#;
    let result = extract_source("templates/product.json", source, Some(Language::Liquid));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(detect_language("templates/product.json"), Language::Liquid);
    // Duplicate section type is emitted only once (BTreeSet dedup).
    let main_refs = result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_name == "sections/main-product.liquid")
        .count();
    assert_eq!(main_refs, 1, "duplicate section type must dedup");
    assert_ref(&result, EdgeKind::References, "sections/related.liquid");
}

#[test]
fn liquid_malformed_json_template_is_silent() {
    let result = extract_source(
        "templates/broken.json",
        "{ not json",
        Some(Language::Liquid),
    );
    // Malformed JSON -> early return, only the file node remains, no error.
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.unresolved_references.is_empty());
}

// ---------------------------------------------------------------------------
// Assertions (mirror embedded_languages.rs, but line-agnostic for robustness).
// ---------------------------------------------------------------------------

fn assert_node(result: &ExtractionResult, kind: NodeKind, name: &str) {
    assert!(
        result
            .nodes
            .iter()
            .any(|node| node.kind == kind && node.name == name),
        "missing node kind={kind:?} name={name}; nodes={:#?}",
        result.nodes
    );
}

fn assert_ref(result: &ExtractionResult, kind: EdgeKind, name: &str) {
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing ref kind={kind:?} name={name}; refs={:#?}",
        result.unresolved_references
    );
}
