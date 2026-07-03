use std::collections::HashSet;

use codegraph_core::types::{Language, Node, NodeKind};
use codegraph_graph::query::{SearchOptions, parse_query, search_nodes};
use codegraph_store::Store;

fn temp_db_path(test_name: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "codegraph-graph-search-{test_name}-{}-{nanos}.db",
        std::process::id()
    ));
    path
}

#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: Language,
    start_line: i64,
    end_line: i64,
    signature: Option<&str>,
    visibility: Option<&str>,
    is_exported: bool,
) -> Node {
    Node {
        id: id.to_string(),
        kind,
        name: name.to_string(),
        qualified_name: qualified_name.to_string(),
        file_path: file_path.to_string(),
        language,
        start_line,
        end_line,
        start_column: 0,
        end_column: 0,
        docstring: None,
        signature: signature.map(str::to_string),
        visibility: visibility.map(str::to_string),
        is_exported,
        is_async: false,
        is_static: false,
        is_abstract: false,
        decorators: Vec::new(),
        type_parameters: Vec::new(),
        return_type: None,
        updated_at: 1,
    }
}

/// The 13-node mini corpus byte-mirrored from `reference/golden/mini/colby.nodes.json`.
/// File nodes use the literal `file:{relpath}` id; symbols use the hashed ids.
fn mini_corpus() -> Vec<Node> {
    use Language::{Python, TypeScript};
    use NodeKind::{Class, File, Function, Import, Method};
    vec![
        node(
            "file:src/app.ts",
            File,
            "app.ts",
            "src/app.ts",
            "src/app.ts",
            TypeScript,
            1,
            10,
            None,
            None,
            false,
        ),
        node(
            "import:4ec14e7f870d20bf811e565c9993468c",
            Import,
            "./math",
            "./math",
            "src/app.ts",
            TypeScript,
            1,
            1,
            Some("import { Counter, add } from './math';"),
            None,
            false,
        ),
        node(
            "function:60629aa3876961b8bd3c07c43bbe6a37",
            Function,
            "runDemo",
            "runDemo",
            "src/app.ts",
            TypeScript,
            3,
            7,
            Some("(): number"),
            None,
            true,
        ),
        node(
            "file:src/math.ts",
            File,
            "math.ts",
            "src/math.ts",
            "src/math.ts",
            TypeScript,
            1,
            13,
            None,
            None,
            false,
        ),
        node(
            "function:cce15011e0125d59f6bef014ae79c04f",
            Function,
            "add",
            "add",
            "src/math.ts",
            TypeScript,
            1,
            3,
            Some("(left: number, right: number): number"),
            None,
            true,
        ),
        node(
            "class:7ac92ae0dce4208dfad9148e025726c4",
            Class,
            "Counter",
            "Counter",
            "src/math.ts",
            TypeScript,
            5,
            12,
            None,
            None,
            true,
        ),
        node(
            "method:89ae38f0afd5d9ed9a7eb3286a91a590",
            Method,
            "value",
            "Counter::value",
            "src/math.ts",
            TypeScript,
            6,
            6,
            None,
            Some("private"),
            false,
        ),
        node(
            "method:f501ba98441869bd251c636e30d31e3d",
            Method,
            "increment",
            "Counter::increment",
            "src/math.ts",
            TypeScript,
            8,
            11,
            Some("(step: number = 1): number"),
            None,
            false,
        ),
        node(
            "file:tools/greeter.py",
            File,
            "greeter.py",
            "tools/greeter.py",
            "tools/greeter.py",
            Python,
            1,
            12,
            None,
            None,
            false,
        ),
        node(
            "class:7ea38da25781893f519b36a80bfe6fb6",
            Class,
            "Greeter",
            "Greeter",
            "tools/greeter.py",
            Python,
            1,
            6,
            None,
            None,
            false,
        ),
        node(
            "method:d9785bc27131eb51e96712a75cf8b935",
            Method,
            "__init__",
            "Greeter::__init__",
            "tools/greeter.py",
            Python,
            2,
            3,
            Some("(self, prefix: str) -> None"),
            None,
            false,
        ),
        node(
            "method:3c01f33894e563f457be2f29fffca19c",
            Method,
            "greet",
            "Greeter::greet",
            "tools/greeter.py",
            Python,
            5,
            6,
            Some("(self, name: str) -> str"),
            None,
            false,
        ),
        node(
            "function:8a6f7270b7e9d62aee91dd9e70f126f3",
            Function,
            "make_greeting",
            "make_greeting",
            "tools/greeter.py",
            Python,
            9,
            11,
            Some("(name: str) -> str"),
            None,
            false,
        ),
    ]
}

fn corpus_store(test_name: &str) -> Store {
    let mut store = Store::open(&temp_db_path(test_name)).expect("open temp store");
    store.upsert_nodes(&mini_corpus()).expect("insert corpus");
    store
}

#[derive(serde::Deserialize)]
struct GoldenCase {
    query: String,
    #[serde(default)]
    options: GoldenOptions,
    results: Vec<GoldenResult>,
}

#[derive(serde::Deserialize, Default)]
struct GoldenOptions {
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(serde::Deserialize)]
struct GoldenResult {
    id: String,
    name: String,
    kind: String,
    score: f64,
}

fn load_golden() -> std::collections::BTreeMap<String, GoldenCase> {
    let raw = include_str!("../../../reference/golden/mini/search.golden.json");
    serde_json::from_str(raw).expect("parse golden")
}

fn options_from_golden(opts: &GoldenOptions) -> SearchOptions {
    let kinds = opts
        .kinds
        .iter()
        .map(|k| {
            NodeKind::ALL
                .into_iter()
                .find(|nk| nk.as_str() == k)
                .unwrap_or_else(|| panic!("unknown golden kind {k}"))
        })
        .collect();
    SearchOptions {
        kinds,
        languages: Vec::new(),
        limit: opts.limit,
        offset: None,
    }
}

fn assert_case(store: &Store, case_name: &str, case: &GoldenCase) {
    let options = options_from_golden(&case.options);
    let project_tokens: HashSet<String> = HashSet::new();
    let results = search_nodes(store, &case.query, &options, &project_tokens)
        .unwrap_or_else(|e| panic!("search '{}' failed: {e}", case.query));

    let got_ids: Vec<&str> = results.iter().map(|r| r.node.id.as_str()).collect();
    let want_ids: Vec<&str> = case.results.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        got_ids, want_ids,
        "case `{case_name}` query `{}`: id ordering mismatch\n got:  {got_ids:?}\n want: {want_ids:?}",
        case.query
    );

    for (got, want) in results.iter().zip(case.results.iter()) {
        assert_eq!(got.node.name, want.name, "case `{case_name}` name mismatch");
        assert_eq!(
            got.node.kind.as_str(),
            want.kind,
            "case `{case_name}` kind mismatch"
        );
        let diff = (got.score - want.score).abs();
        assert!(
            diff < 1e-6,
            "case `{case_name}` query `{}` node `{}`: score mismatch got {} want {}",
            case.query,
            want.name,
            got.score,
            want.score
        );
    }
}

#[test]
fn search_reproduces_all_golden_cases() {
    let store = corpus_store("golden-all");
    let golden = load_golden();
    for (name, case) in &golden {
        assert_case(&store, name, case);
    }
}

#[test]
fn search_single_term_counter_orders_class_first() {
    let store = corpus_store("single-term");
    let project_tokens: HashSet<String> = HashSet::new();
    let results = search_nodes(
        &store,
        "Counter",
        &SearchOptions::default(),
        &project_tokens,
    )
    .unwrap();
    assert_eq!(results[0].node.id, "class:7ac92ae0dce4208dfad9148e025726c4");
    assert_eq!(results.len(), 4);
}

#[test]
fn search_kind_filter_function_restricts_results() {
    let store = corpus_store("kind-filter");
    let project_tokens: HashSet<String> = HashSet::new();
    let opts = SearchOptions {
        kinds: vec![NodeKind::Function],
        ..SearchOptions::default()
    };
    let results = search_nodes(&store, "add", &opts, &project_tokens).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node.kind, NodeKind::Function);
    assert_eq!(results[0].node.name, "add");
}

#[test]
fn search_limit_is_applied_after_rescoring() {
    let store = corpus_store("limit");
    let project_tokens: HashSet<String> = HashSet::new();
    let opts = SearchOptions {
        limit: Some(2),
        ..SearchOptions::default()
    };
    let results = search_nodes(&store, "greet", &opts, &project_tokens).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].node.name, "greet");
    assert_eq!(results[1].node.name, "Greeter");
}

#[test]
fn search_no_results_for_unmatched_query() {
    let store = corpus_store("no-results");
    let project_tokens: HashSet<String> = HashSet::new();
    let results = search_nodes(
        &store,
        "zzzzqqqq",
        &SearchOptions::default(),
        &project_tokens,
    )
    .unwrap();
    assert!(results.is_empty());
}

#[test]
fn search_field_kind_filter_in_query_string() {
    let store = corpus_store("field-kind");
    let project_tokens: HashSet<String> = HashSet::new();
    let results = search_nodes(
        &store,
        "kind:method increment",
        &SearchOptions::default(),
        &project_tokens,
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node.name, "increment");
    assert_eq!(results[0].node.kind, NodeKind::Method);
}

#[test]
fn search_parse_query_tokenizes_fields_and_text() {
    let parsed = parse_query("kind:function name:auth path:src/api authenticate");
    assert_eq!(parsed.kinds, vec![NodeKind::Function]);
    assert_eq!(parsed.name_filters, vec!["auth".to_string()]);
    assert_eq!(parsed.path_filters, vec!["src/api".to_string()]);
    assert_eq!(parsed.text, "authenticate");
}

#[test]
fn search_parse_query_strips_quotes_in_path_filter() {
    let parsed = parse_query("path:\"src/some path/with spaces\" authenticate");
    assert_eq!(
        parsed.path_filters,
        vec!["src/some path/with spaces".to_string()]
    );
    assert_eq!(parsed.text, "authenticate");
}

#[test]
fn search_parse_query_unknown_field_falls_through_to_text() {
    let parsed = parse_query("TODO: fixme");
    assert!(parsed.kinds.is_empty());
    assert!(parsed.name_filters.is_empty());
    assert_eq!(parsed.text, "TODO: fixme");
}

#[test]
fn search_parse_query_lang_alias_and_language() {
    let parsed = parse_query("lang:python language:typescript greet");
    assert_eq!(
        parsed.languages,
        vec![Language::Python, Language::TypeScript]
    );
    assert_eq!(parsed.text, "greet");
}

#[test]
fn search_parse_query_unterminated_quote_is_forgiving() {
    let parsed = parse_query("path:\"unterminated rest");
    assert_eq!(parsed.path_filters, vec!["\"unterminated rest".to_string()]);
    assert_eq!(parsed.text, "");
}

#[test]
fn search_parse_query_invalid_kind_value_falls_through() {
    let parsed = parse_query("kind:notakind real");
    assert!(parsed.kinds.is_empty());
    assert_eq!(parsed.text, "kind:notakind real");
}

#[test]
fn search_parse_query_rust_qualifier_stays_in_text() {
    let parsed = parse_query("Counter::increment");
    assert_eq!(parsed.text, "Counter::increment");
    assert!(parsed.kinds.is_empty());
}
