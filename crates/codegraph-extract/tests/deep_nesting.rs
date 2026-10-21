use codegraph_core::types::Language;
use codegraph_extract::extract_source;

const DEEP: usize = 30_000;
const SHALLOW: usize = 200;
const EXPECTED_ERROR: &str = "AST nesting exceeds safe traversal limit (256)";

fn nested_source(language: Language, depth: usize) -> (&'static str, String) {
    match language {
        Language::C => (
            "deep.c",
            format!(
                "void before(void) {{}}\nvoid deep(void) {{{}{}}}\nvoid after(void) {{}}\n",
                "{".repeat(depth),
                "}".repeat(depth)
            ),
        ),
        Language::Cpp => (
            "deep.cpp",
            format!(
                "void before() {{}}\nvoid deep() {{{}{}}}\nvoid after() {{}}\n",
                "{".repeat(depth),
                "}".repeat(depth)
            ),
        ),
        Language::Rust => (
            "deep.rs",
            format!(
                "fn before() {{}}\nfn deep() {{{}{}}}\nfn after() {{}}\n",
                "{".repeat(depth),
                "}".repeat(depth)
            ),
        ),
        Language::TypeScript => (
            "deep.ts",
            format!(
                "function before() {{}}\nfunction deep() {{ return {}value{}; }}\nfunction after() {{}}\n",
                "(".repeat(depth),
                ")".repeat(depth)
            ),
        ),
        Language::Python => (
            "deep.py",
            format!(
                "def before():\n    pass\nvalue = {}0{}\ndef after():\n    pass\n",
                "[".repeat(depth),
                "]".repeat(depth)
            ),
        ),
        _ => unreachable!("test helper only supports the five guarded languages"),
    }
}

fn extract_on_small_stack(
    language: Language,
    depth: usize,
) -> codegraph_core::types::ExtractionResult {
    let (file_path, source) = nested_source(language, depth);
    std::thread::Builder::new()
        .name(format!("deep-nesting-{language}"))
        .stack_size(1024 * 1024)
        .spawn(move || extract_source(file_path, &source, Some(language)))
        .expect("spawn 1 MiB extraction thread")
        .join()
        .expect("deep extraction must return instead of overflowing")
}

#[test]
fn deep_native_walkers_fail_per_file_without_partial_graph() {
    for language in [
        Language::C,
        Language::Cpp,
        Language::Rust,
        Language::TypeScript,
        Language::Python,
    ] {
        let result = extract_on_small_stack(language, DEEP);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains(EXPECTED_ERROR)),
            "{language}: errors={:?}",
            result.errors
        );
        assert!(
            result.nodes.is_empty(),
            "{language}: partial nodes survived"
        );
        assert!(
            result.edges.is_empty(),
            "{language}: partial edges survived"
        );
        assert!(
            result.unresolved_references.is_empty(),
            "{language}: partial references survived"
        );
    }
}

#[test]
fn ordinary_two_hundred_level_sources_remain_clean() {
    for language in [
        Language::C,
        Language::Cpp,
        Language::Rust,
        Language::TypeScript,
        Language::Python,
    ] {
        let result = extract_on_small_stack(language, SHALLOW);
        assert!(result.errors.is_empty(), "{language}: {:?}", result.errors);
        assert!(
            result.nodes.iter().any(|node| node.name == "before"),
            "{language}: normal leading symbol disappeared"
        );
        assert!(
            result.nodes.iter().any(|node| node.name == "after"),
            "{language}: normal trailing symbol disappeared"
        );
    }
}

#[test]
fn logical_depth_limit_is_inclusive_and_deterministic() {
    // For this C shape the root + function + outer body contribute three
    // named-node levels, so 253 nested compounds land exactly at depth 256.
    let at_limit = extract_on_small_stack(Language::C, 253);
    assert!(at_limit.errors.is_empty(), "{:?}", at_limit.errors);

    let beyond_limit = extract_on_small_stack(Language::C, 254);
    assert_eq!(beyond_limit.errors.len(), 1, "{:?}", beyond_limit.errors);
    assert!(beyond_limit.errors[0].contains(EXPECTED_ERROR));
    assert!(beyond_limit.nodes.is_empty());
}

#[test]
fn depth_abort_is_scoped_to_one_extraction() {
    let deep = extract_on_small_stack(Language::C, DEEP);
    assert_eq!(deep.errors.len(), 1, "{:?}", deep.errors);

    let normal = extract_source(
        "normal.c",
        "int add(int a, int b) { return a + b; }\n",
        Some(Language::C),
    );
    assert!(normal.errors.is_empty(), "{:?}", normal.errors);
    assert!(normal.nodes.iter().any(|node| node.name == "add"));
}

/// A CFML component whose tag tree is shallow but whose `<cfscript>` body or
/// `<cfquery>` `#…#` expression nests `depth` parentheses: the re-parsed
/// snippet is what runs deep.
fn cfml_snippet_source(shape: &str, depth: usize) -> String {
    let nested = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
    match shape {
        "script" => format!(
            "<cfcomponent>\n<cffunction name=\"before\"></cffunction>\n<cfscript>\nx = {nested};\n</cfscript>\n</cfcomponent>\n"
        ),
        "query" => format!(
            "<cfcomponent>\n<cffunction name=\"before\">\n<cfquery name=\"q\">SELECT * FROM t WHERE id = #{nested}#</cfquery>\n</cffunction>\n</cfcomponent>\n"
        ),
        _ => unreachable!("cfml snippet shapes are script and query"),
    }
}

fn extract_cfml_on_small_stack(
    shape: &'static str,
    depth: usize,
) -> codegraph_core::types::ExtractionResult {
    let source = cfml_snippet_source(shape, depth);
    std::thread::Builder::new()
        .name(format!("deep-nesting-cfml-{shape}"))
        .stack_size(1024 * 1024)
        .spawn(move || extract_source("Deep.cfc", &source, Some(Language::Cfml)))
        .expect("spawn 1 MiB extraction thread")
        .join()
        .expect("deep extraction must return instead of overflowing")
}

#[test]
fn a_deep_cfml_snippet_fails_its_file_without_partial_graph() {
    for shape in ["script", "query"] {
        let result = extract_cfml_on_small_stack(shape, DEEP);
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains(EXPECTED_ERROR)),
            "{shape}: errors={:?}",
            result.errors
        );
        assert!(result.nodes.is_empty(), "{shape}: partial nodes survived");
        assert!(result.edges.is_empty(), "{shape}: partial edges survived");
        assert!(
            result.unresolved_references.is_empty(),
            "{shape}: partial references survived"
        );
        let shallow = extract_cfml_on_small_stack(shape, 20);
        assert!(shallow.errors.is_empty(), "{shape}: {:?}", shallow.errors);
        assert!(
            shallow.nodes.iter().any(|node| node.name == "before"),
            "{shape}: the component's function disappeared"
        );
    }
}
