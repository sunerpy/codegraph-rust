use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use codegraph_extract::{detect_language, extract_source};
use std::fs;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/markup_risk");

#[test]
fn lang_markup_risk_file_level_only_languages_are_empty() {
    // Upstream grammars.ts:332-334 and tree-sitter.ts:4382-4387 return no extractor nodes.
    for (path, language) in [
        ("config.yaml", Language::Yaml),
        ("page.twig", Language::Twig),
        ("application.properties", Language::Properties),
    ] {
        assert_eq!(detect_language(path), language);
        let result = extract_fixture(path, None);
        assert_empty_extraction(path, &result);
    }
}

#[test]
fn lang_markup_risk_sql_html_css_json_stay_unsupported() {
    // Upstream EXTENSION_MAP (grammars.ts:46-115) has no .sql/.html/.css entries
    // and standalone .json maps nowhere (only Shopify templates/sections JSON
    // routes to liquid via grammars.ts:135-139). LANGUAGES (types.ts:66-97)
    // has no sql language, so these stay Unknown in the Rust port.
    for path in ["query.sql", "index.html", "style.css", "data.json"] {
        assert_eq!(detect_language(path), Language::Unknown, "{path}");
    }
}

#[test]
fn lang_markup_risk_dfm_uses_custom_component_extractor() {
    // Upstream grammars.ts:100-101 maps DFM/FMX to pascal; tree-sitter.ts:4388-4394 routes DfmExtractor.
    // DfmExtractor behavior is from upstream extraction/dfm-extractor.ts:55-158.
    assert_eq!(detect_language("MainForm.dfm"), Language::Pascal);
    assert_eq!(detect_language("Phone.fmx"), Language::Pascal);
    let result = extract_fixture("MainForm.dfm", None);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let file = assert_node(&result, NodeKind::File, "MainForm.dfm");
    // dfm-extractor.ts:58 hashes the file node id (NOT the tree-sitter literal file:{path}).
    assert!(file.id.starts_with("file:") && !file.id.contains("MainForm"));
    let form = assert_node(&result, NodeKind::Component, "MainForm");
    assert_eq!(form.signature.as_deref(), Some("TMainForm"));
    assert_eq!(form.qualified_name, "MainForm.dfm#MainForm");
    assert_eq!(form.start_line, 1);
    let button = assert_node(&result, NodeKind::Component, "SaveButton");
    assert_eq!(button.signature.as_deref(), Some("TButton"));
    let list = assert_node(&result, NodeKind::Component, "Items");
    assert_eq!(list.signature.as_deref(), Some("TListView"));
    // Nesting (dfm-extractor.ts:130-135): file contains MainForm; MainForm
    // contains SaveButton and Items.
    assert_contains(&result, &file.id, &form.id);
    assert_contains(&result, &form.id, &button.id);
    assert_contains(&result, &form.id, &list.id);
    // Event handlers (dfm-extractor.ts:139-151) become references with col 0.
    assert_ref(&result, EdgeKind::References, "FormCreate");
    assert_ref(&result, EdgeKind::References, "SaveButtonClick");
    // ItemsDblClick sits after the multiline `Columns = <...>` block, proving
    // the multi-line property skip (dfm-extractor.ts:95-109).
    assert_ref(&result, EdgeKind::References, "ItemsDblClick");
    assert_eq!(
        result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Component)
            .count(),
        3
    );
}

#[test]
fn lang_markup_risk_kotlin_extracts_upstream_symbol_set() {
    // Golden run: upstream extractFromSource on this exact fixture (kotlin.ts:71-308).
    let result = extract_fixture("Service.kt", None);
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    assert_node(&result, NodeKind::Namespace, "com.example.demo");
    let import = assert_node(&result, NodeKind::Import, "com.example.db.Db");
    assert_eq!(
        import.signature.as_deref(),
        Some("import com.example.db.Db")
    );

    // interface visibility: the upstream getVisibility runs only for class/enum paths,
    // and extractInterface (tree-sitter.ts:988-1018) passes no visibility.
    let service = assert_node(&result, NodeKind::Interface, "Service");
    assert_eq!(service.visibility, None);
    let run = assert_node(&result, NodeKind::Method, "run");
    assert_eq!(run.visibility.as_deref(), Some("public"));
    assert_eq!(run.return_type.as_deref(), Some("Entity"));
    // kotlin.ts:232-242 getSignature uses a field lookup the kotlin grammar
    // does not expose, so the upstream emits NO kotlin signatures.
    assert_eq!(run.signature, None);

    let level = assert_node(&result, NodeKind::Enum, "Level");
    assert_eq!(level.visibility.as_deref(), Some("public"));
    assert_node(&result, NodeKind::EnumMember, "LOW");
    assert_node(&result, NodeKind::EnumMember, "HIGH");

    let repo = assert_node(&result, NodeKind::Class, "Repo");
    assert_eq!(repo.visibility.as_deref(), Some("public"));
    // The upstream emits NO field/property/variable nodes for kotlin
    // property_declaration: extractField (tree-sitter.ts:1180-1278) and
    // extractVariable (tree-sitter.ts:1382-1463) find no declarators in the
    // kotlin grammar shape. `val name` / `val topLevel` must NOT be nodes.
    assert!(
        !result.nodes.iter().any(|node| matches!(
            node.kind,
            NodeKind::Field | NodeKind::Property | NodeKind::Variable | NodeKind::Constant
        )),
        "kotlin must not emit field/property/variable nodes: {:#?}",
        result.nodes
    );

    let fetch = assert_node(&result, NodeKind::Method, "fetch");
    assert_eq!(fetch.visibility.as_deref(), Some("public"));
    assert!(fetch.is_async, "suspend → isAsync (kotlin.ts:261-270)");
    assert_eq!(fetch.signature, None);
    // nullable return `Entity?` normalizes to bare class name (kotlin.ts:17-43).
    assert_eq!(fetch.return_type.as_deref(), Some("Entity"));

    // Nested enum classification must not leak onto the outer class
    // (kotlin.ts:197-210 checks keyword children of the class node itself).
    let outer = assert_node(&result, NodeKind::Class, "Outer");
    let inner = assert_node(&result, NodeKind::Enum, "Inner");
    assert_eq!(inner.qualified_name, "com.example.demo::Outer::Inner");
    assert_contains(&result, &outer.id, &inner.id);
    assert_node(&result, NodeKind::EnumMember, "A");

    // object_declaration → extraClassNodeTypes → class (kotlin.ts:84).
    let registry = assert_node(&result, NodeKind::Class, "Registry");
    let lookup = assert_node(&result, NodeKind::Method, "lookup");
    assert_eq!(lookup.return_type.as_deref(), Some("Repo"));
    assert_contains(&result, &registry.id, &lookup.id);

    let alias = assert_node(&result, NodeKind::TypeAlias, "EntityId");
    assert_eq!(alias.start_line, 31);

    // Extension function `String.shout` → method with receiver qualified name
    // (kotlin.ts:211-231 + tree-sitter.ts extractMethod receiver path).
    let shout = assert_node(&result, NodeKind::Method, "shout");
    assert_eq!(shout.qualified_name, "String::shout");
    assert_eq!(shout.return_type.as_deref(), Some("String"));

    // expect marker lands in decorators (kotlin.ts:271-293 extractModifiers,
    // merged by tree-sitter.ts:626-634).
    let platform = assert_node(&result, NodeKind::Function, "platform");
    assert_eq!(platform.decorators, vec!["expect".to_string()]);
    assert_eq!(platform.return_type.as_deref(), Some("String"));

    // Golden unresolved references from the upstream run on this fixture.
    assert_ref_at(&result, EdgeKind::Imports, "com.example.db.Db", 3);
    assert_ref_at(&result, EdgeKind::Calls, "db.query", 15);
    assert_ref_at(&result, EdgeKind::Calls, "helper", 23);
    assert_ref_at(&result, EdgeKind::Calls, "Repo", 28);
    assert_ref_at(&result, EdgeKind::Calls, "Db", 28);
    assert_ref_at(&result, EdgeKind::Calls, "uppercase", 33);
}

#[test]
fn lang_markup_risk_swift_extracts_upstream_symbol_set() {
    // Upstream swift.ts:43-138 onto tree-sitter-swift 0.7.3 (same alex-pinkus
    // grammar family as the upstream WASM build; manifest tier-a, no vendoring).
    assert_eq!(detect_language("src/Greeter.swift"), Language::Swift);
    let result = extract_fixture("Greeter.swift", None);
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let import = assert_node(&result, NodeKind::Import, "Foundation");
    assert_eq!(import.signature.as_deref(), Some("import Foundation"));

    // classifyClassNode (swift.ts:112-120): class/struct/enum keyword split.
    let greeter = assert_node(&result, NodeKind::Class, "Greeter");
    assert_eq!(greeter.visibility.as_deref(), Some("public"));
    assert_node(&result, NodeKind::Struct, "Point");
    let http = assert_node(&result, NodeKind::Enum, "HTTPMethod");
    assert_eq!(http.visibility.as_deref(), Some("internal"));

    // Multi-case `case put, delete` (tree-sitter.ts:1105-1131) emits BOTH.
    assert_node(&result, NodeKind::EnumMember, "put");
    assert_node(&result, NodeKind::EnumMember, "delete");

    // protocol_declaration → interface (swift.ts:47); the upstream's
    // extractInterface passes no visibility.
    let drawable = assert_node(&result, NodeKind::Interface, "Drawable");
    assert_eq!(drawable.visibility, None);

    assert_node(&result, NodeKind::TypeAlias, "Alias");

    // resolveName (swift.ts:60-75): `extension KF.Builder` names by the LAST
    // segment so it merges with the extended type's simple name.
    let builder = assert_node(&result, NodeKind::Class, "Builder");
    assert_contains(
        &result,
        &builder.id,
        &assert_node(&result, NodeKind::Method, "build").id,
    );

    // getVisibility default internal (swift.ts:99) + isStatic from modifiers
    // static/class (swift.ts:101-111) + positional return type unwrapping
    // optionals (swift.ts:14-41: `-> Greeter?` → "Greeter").
    let make = assert_node(&result, NodeKind::Method, "make");
    assert_eq!(make.visibility.as_deref(), Some("public"));
    assert!(make.is_static);
    assert_eq!(make.return_type.as_deref(), Some("Greeter"));
    let greet = assert_node(&result, NodeKind::Method, "greet");
    assert_eq!(greet.visibility.as_deref(), Some("internal"));
    assert!(!greet.is_static);
    assert_eq!(greet.return_type.as_deref(), Some("String"));
    // getSignature (swift.ts:76-86) reads the `parameter` FIELD which this
    // grammar does not define, so the upstream emits NO swift signatures.
    assert_eq!(greet.signature, None);
    assert_contains(&result, &greeter.id, &make.id);
    assert_contains(&result, &greeter.id, &greet.id);

    // isAsync (swift.ts:121-129): only `async` INSIDE modifiers counts; the
    // bare effect token after params does not, so topLevel stays non-async.
    let top_level = assert_node(&result, NodeKind::Function, "topLevel");
    assert!(!top_level.is_async);

    // Stored properties are NOT nodes (tree-sitter.ts:453-487); the wrapper
    // attribute decorates the enclosing type instead.
    assert!(
        !result.nodes.iter().any(|node| matches!(
            node.kind,
            NodeKind::Field | NodeKind::Property | NodeKind::Variable | NodeKind::Constant
        )),
        "swift must not emit field/property/variable nodes: {:#?}",
        result.nodes
    );

    // Golden unresolved references from the upstream branches:
    // inheritance_specifier extends (tree-sitter.ts:3467-3483).
    assert_ref_at(&result, EdgeKind::Extends, "Base", 3);
    assert_ref_at(&result, EdgeKind::Extends, "Drawable", 3);
    // @Published wrapper → decorates (tree-sitter.ts:2882-2948 attribute path).
    assert_ref_at(&result, EdgeKind::Decorates, "Published", 4);
    // navigation_suffix unwrap + capitalized-chain re-encode
    // (tree-sitter.ts:2503-2563): Greeter.make().greet → `Greeter.make().greet`.
    assert_ref_at(&result, EdgeKind::Calls, "Greeter.make().greet", 26);
    assert_ref_at(&result, EdgeKind::Calls, "Greeter.make", 26);
    // lowercase instance receiver keeps `obj.method` form (ts:2510-2526).
    assert_ref_at(&result, EdgeKind::Calls, "session.request", 27);
    assert_ref_at(&result, EdgeKind::Calls, "helper", 9);
    assert_ref_at(&result, EdgeKind::Imports, "Foundation", 1);
}

#[test]
fn lang_markup_risk_xml_keeps_existing_mybatis_model() {
    // Upstream grammars.ts:108-110 and mybatis-extractor.ts:94-160: mapper XML emits SQL methods.
    let mapper = r#"<mapper namespace="com.example.Mapper">
  <sql id="Base">id, name</sql>
  <select id="find" resultType="User"><include refid="Base" /></select>
</mapper>"#;
    let mapper_result = extract_source("mapper.xml", mapper, None);
    assert!(
        mapper_result.errors.is_empty(),
        "{:?}",
        mapper_result.errors
    );
    assert_node(&mapper_result, NodeKind::File, "mapper.xml");
    assert_node(&mapper_result, NodeKind::Method, "Base");
    assert_node(&mapper_result, NodeKind::Method, "find");
    assert_ref(
        &mapper_result,
        EdgeKind::References,
        "com.example.Mapper::Base",
    );

    // Non-mapper XML returns only a file node (tree-sitter.ts:4377-4380).
    let plain_result = extract_source("plain.xml", "<root><item /></root>", None);
    assert!(plain_result.errors.is_empty(), "{:?}", plain_result.errors);
    assert_eq!(
        plain_result.nodes.len(),
        1,
        "plain XML keeps only file node"
    );
    assert_eq!(plain_result.nodes[0].kind, NodeKind::File);
}

#[test]
fn lang_markup_risk_r_extracts_functions_classes_generics_imports() {
    // R extraction (upstream languages/r.ts, #828): function assignments, S4/R6
    // classes + their methods, setGeneric/setMethod, library/source imports,
    // and call edges. Node/edge shapes verified byte-identical to the upstream 1.0.1.
    assert_eq!(detect_language("model.R"), Language::R);
    assert_eq!(detect_language("model.r"), Language::R);

    let source = concat!(
        "library(ggplot2)\n",
        "source(\"helpers.R\")\n\n",
        "add <- function(a, b) {\n  a + b\n}\n\n",
        "MAX_SIZE <- 100\n\n",
        "setGeneric(\"describe\", function(obj) standardGeneric(\"describe\"))\n",
        "setMethod(\"describe\", \"Patient\", function(obj) {\n  add(1, 2)\n})\n\n",
        "Stack <- R6Class(\"Stack\",\n  inherit = Base,\n  public = list(\n    push = function(v) { v }\n  )\n)\n",
    );
    let result = extract_source("model.R", source, Some(Language::R));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let add = assert_node(&result, NodeKind::Function, "add");
    assert_node(&result, NodeKind::Function, "describe");
    assert_node(&result, NodeKind::Constant, "MAX_SIZE");
    let stack = assert_node(&result, NodeKind::Class, "Stack");
    let push = assert_node(&result, NodeKind::Method, "push");
    assert_eq!(push.qualified_name, "Stack::push");
    assert_node(&result, NodeKind::Import, "ggplot2");
    assert_node(&result, NodeKind::Import, "helpers.R");

    assert_contains(&result, &stack.id, &push.id);
    let _ = add;

    assert_ref(&result, EdgeKind::Imports, "ggplot2");
    assert_ref(&result, EdgeKind::Imports, "helpers.R");
    assert_ref(&result, EdgeKind::Calls, "add");
    assert_ref(&result, EdgeKind::Extends, "Base");
}

fn extract_fixture(path: &str, language: Option<Language>) -> ExtractionResult {
    let source = fs::read_to_string(format!("{FIXTURES}/{path}")).unwrap();
    extract_source(path, &source, language)
}

fn assert_empty_extraction(path: &str, result: &ExtractionResult) {
    assert!(result.nodes.is_empty(), "{path} nodes={:#?}", result.nodes);
    assert!(result.edges.is_empty(), "{path} edges={:#?}", result.edges);
    assert!(
        result.unresolved_references.is_empty(),
        "{path} refs={:#?}",
        result.unresolved_references
    );
    assert!(
        result.errors.is_empty(),
        "{path} errors={:?}",
        result.errors
    );
}

fn assert_node<'a>(
    result: &'a ExtractionResult,
    kind: NodeKind,
    name: &str,
) -> &'a codegraph_core::types::Node {
    result
        .nodes
        .iter()
        .find(|node| node.kind == kind && node.name == name)
        .unwrap_or_else(|| {
            panic!(
                "missing node kind={kind} name={name}; nodes={:#?}",
                result.nodes
            )
        })
}
fn assert_contains(result: &ExtractionResult, source: &str, target: &str) {
    assert!(
        result
            .edges
            .iter()
            .any(|edge| edge.kind == EdgeKind::Contains
                && edge.source == source
                && edge.target == target),
        "missing contains edge {source} -> {target}; edges={:#?}",
        result.edges
    );
}

fn assert_ref(result: &ExtractionResult, kind: EdgeKind, name: &str) {
    assert!(
        result
            .unresolved_references
            .iter()
            .any(|reference| reference.reference_kind == kind && reference.reference_name == name),
        "missing ref kind={kind} name={name}; refs={:#?}",
        result.unresolved_references
    );
}

fn assert_ref_at(result: &ExtractionResult, kind: EdgeKind, name: &str, line: i64) {
    assert!(
        result.unresolved_references.iter().any(|reference| {
            reference.reference_kind == kind
                && reference.reference_name == name
                && reference.line == line
        }),
        "missing ref kind={kind} name={name} line={line}; refs={:#?}",
        result.unresolved_references
    );
}
