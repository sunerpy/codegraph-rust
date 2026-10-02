//! Syntax classification for the code blocks — the engine half of upstream
//! `__tests__/ui-highlight.test.ts` (`v1.6.1`). Its link-placement cases
//! (`decodeLine` + `assignRefs`, which claim a token for a graph link) run in
//! `ui/tests/` against literal token lines; here every case asserts the
//! classified tokens the server sends.

use std::time::Instant;

use codegraph_core::types::Language;
use codegraph_extract::syntax_tokens::{
    SYNTAX_TOKEN_CLASSES, grammar_for, syntax_regions_for, tokenize_source,
};
use codegraph_ui::highlight::{
    HighlightCache, HighlightResult, MAX_HIGHLIGHT_CHARS, SLICE_CACHE_LINES, highlight_lines,
};

fn highlight(lines: &[&str], language: Language) -> HighlightResult {
    let owned: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    highlight_lines(&HighlightCache::default(), &owned, Some(language), None)
}

/// What the code block renders for one line: `class:text` per token.
fn shape(result: &HighlightResult, line: usize) -> Vec<String> {
    result.lines[line]
        .iter()
        .map(|token| format!("{}:{}", result.classes[token.0 as usize], token.1))
        .collect()
}

fn rebuilt(result: &HighlightResult, line: usize) -> String {
    result.lines[line]
        .iter()
        .map(|token| token.1.as_str())
        .collect()
}

/* ------------------------------------------------- which languages classify */

#[test]
fn answers_for_every_language_the_engine_indexes_without_throwing() {
    for language in Language::ALL {
        let _ = grammar_for(language);
    }
    for language in [
        Language::TypeScript,
        Language::Go,
        Language::Python,
        Language::Rust,
        Language::Swift,
        Language::CSharp,
        Language::Ruby,
        Language::Php,
    ] {
        assert!(grammar_for(language).is_some(), "{}", language.as_str());
    }
}

#[test]
fn reads_a_single_file_component_through_its_script_block() {
    assert_eq!(grammar_for(Language::Svelte), Some(Language::TypeScript));
    let regions = syntax_regions_for(
        "<p>{x}</p>\n<script lang=\"ts\">\nlet x = 1;\n</script>\n",
        Language::Svelte,
    )
    .expect("regions");
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].language, Language::TypeScript);
}

#[test]
fn has_no_grammar_for_the_formats_that_only_have_file_level_extraction() {
    for language in [
        Language::Yaml,
        Language::Xml,
        Language::Properties,
        Language::Twig,
        Language::Unknown,
    ] {
        assert!(grammar_for(language).is_none(), "{}", language.as_str());
    }
}

/* ---------------------------------------------------------- classification */

#[test]
fn reads_typescript_with_the_classes_the_theme_paints() {
    let result = highlight(&["const answer = 42; // note"], Language::TypeScript);
    assert_eq!(result.engine, "tree-sitter");
    assert_eq!(result.grammar.as_deref(), Some("typescript"));
    assert_eq!(result.classes, SYNTAX_TOKEN_CLASSES);
    let rendered = shape(&result, 0);
    for token in [
        "keyword:const",
        "ident:answer",
        "number:42",
        "comment:// note",
    ] {
        assert!(
            rendered.contains(&token.to_string()),
            "{token} in {rendered:?}"
        );
    }
}

#[test]
fn reads_a_hash_comment_as_a_comment_in_python_and_as_code_in_typescript() {
    let python = highlight(&["x = 1  # note"], Language::Python);
    assert_eq!(
        shape(&python, 0).last().map(String::as_str),
        Some("comment:# note")
    );
    let ts = highlight(&["x = 1  # note"], Language::TypeScript);
    assert_ne!(
        shape(&ts, 0).last().map(String::as_str),
        Some("comment:# note")
    );
}

#[test]
fn carries_a_block_comment_across_lines_within_one_slice() {
    let result = highlight(
        &["/* open", "still comment", "done */ const x = 1;"],
        Language::TypeScript,
    );
    assert_eq!(shape(&result, 1), ["comment:still comment"]);
    assert_eq!(shape(&result, 2)[0], "comment:done */");
    assert!(shape(&result, 2).contains(&"keyword:const".to_string()));
}

#[test]
fn reads_go_which_has_its_own_idea_of_what_a_keyword_is() {
    let rendered = shape(
        &highlight(&["func Greet(name string) string {"], Language::Go),
        0,
    );
    assert!(
        rendered.contains(&"keyword:func".to_string()),
        "{rendered:?}"
    );
    assert!(rendered.contains(&"def:Greet".to_string()), "{rendered:?}");
}

#[test]
fn reads_arkts_with_its_own_grammar_not_typescripts() {
    let result = highlight(&["@Entry struct Index { build() {} }"], Language::ArkTs);
    assert_eq!(result.engine, "tree-sitter");
    assert_eq!(result.grammar.as_deref(), Some("arkts"));
}

#[test]
fn does_not_read_a_type_annotations_string_as_a_string_literal() {
    for (language, line) in [
        (Language::TypeScript, "function put(key: string): void {}"),
        (Language::Php, "<?php function put(string $key): void {}"),
    ] {
        let rendered = shape(&highlight(&[line], language), 0);
        assert!(
            rendered.contains(&"type:string".to_string()),
            "{} {rendered:?}",
            language.as_str()
        );
        assert!(!rendered.contains(&"string:string".to_string()));
    }
}

#[test]
fn paints_a_built_in_type_the_same_way_in_every_language() {
    for (language, line) in [
        (Language::TypeScript, "let a: string;"),
        (Language::Go, "var a string"),
        (Language::CSharp, "string a;"),
        (Language::Rust, "let a: u32 = 1;"),
    ] {
        let rendered = shape(&highlight(&[line], language), 0);
        assert!(
            rendered.iter().any(|t| t.starts_with("type:")),
            "{} {rendered:?}",
            language.as_str()
        );
        assert!(
            !rendered
                .iter()
                .any(|t| t == "keyword:string" || t == "keyword:u32")
        );
    }
}

#[test]
fn keeps_a_template_literals_interpolated_call_as_code_so_it_can_link() {
    let rendered = shape(
        &highlight(
            &["const s = `n=${store.size()} done`;"],
            Language::TypeScript,
        ),
        0,
    );
    assert!(rendered.contains(&"ident:size".to_string()), "{rendered:?}");
}

#[test]
fn marks_a_definitions_own_name_from_the_extractors_tables() {
    for (language, line, name) in [
        (Language::TypeScript, "export class Store {}", "Store"),
        (Language::Python, "def put(self):", "put"),
        (Language::Rust, "pub fn put(&self) {}", "put"),
        (Language::Ruby, "class Store", "Store"),
        (Language::CSharp, "public class Store {}", "Store"),
        (Language::Swift, "final class Store {}", "Store"),
    ] {
        let rendered = shape(&highlight(&[line], language), 0);
        assert!(
            rendered.contains(&format!("def:{name}")),
            "{} {rendered:?}",
            language.as_str()
        );
    }
}

#[test]
fn emits_one_entry_per_source_line_always() {
    let lines = ["a();", "", "b();", ""];
    let result = highlight(&lines, Language::TypeScript);
    assert_eq!(result.lines.len(), lines.len());
    assert!(result.lines[1].is_empty());
}

#[test]
fn reproduces_every_line_of_a_real_file_exactly() {
    let text = include_str!("../../../ui/src/lib/api.ts");
    let lines: Vec<&str> = text.split('\n').collect();
    let result = highlight(&lines, Language::TypeScript);
    assert_eq!(result.engine, "tree-sitter");
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(rebuilt(&result, i), *line, "line {}", i + 1);
    }
}

#[test]
fn classifies_a_components_script_and_leaves_its_markup_plain() {
    let lines = [
        "<script lang=\"ts\">",
        "  let count = 0;",
        "</script>",
        "",
        "<button onclick={bump}>{count}</button>",
    ];
    let result = highlight(&lines, Language::Svelte);
    assert_eq!(result.engine, "tree-sitter");
    assert!(shape(&result, 1).contains(&"keyword:let".to_string()));
    // The markup still splits into identifiers, so a call site in it can link.
    assert!(
        shape(&result, 4).contains(&"ident:bump".to_string()),
        "{:?}",
        shape(&result, 4)
    );
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(rebuilt(&result, i), *line);
    }
}

/* -------------------------------------------------------- the plain fallback */

#[test]
fn answers_plain_with_a_reason_for_a_language_no_grammar_covers() {
    let result = highlight(&["whatever this is"], Language::Unknown);
    assert_eq!(result.engine, "plain");
    assert!(result.grammar.is_none());
    assert!(result.reason.is_some());
    assert_eq!(result.lines.len(), 1);
}

#[test]
fn still_splits_identifiers_when_it_cannot_highlight_so_the_links_land() {
    let result = highlight(&["  return this.mutex.withLock();"], Language::Unknown);
    assert!(
        shape(&result, 0).contains(&"ident:withLock".to_string()),
        "{:?}",
        shape(&result, 0)
    );
}

#[test]
fn refuses_to_classify_a_minified_line_rather_than_wedging_on_it() {
    let enormous = "a".repeat(MAX_HIGHLIGHT_CHARS + 1);
    let result = highlight(&[&enormous], Language::JavaScript);
    assert_eq!(result.engine, "plain");
    assert!(result.reason.as_deref().unwrap().contains("minified"));
    assert_eq!(rebuilt(&result, 0).len(), enormous.len());
}

#[test]
fn answers_plain_for_a_component_whose_script_block_is_empty() {
    let result = highlight(&["<p>hello</p>"], Language::Svelte);
    assert_eq!(result.engine, "plain");
    assert_eq!(rebuilt(&result, 0), "<p>hello</p>");
}

/* ------------------------------------------- graph links land on a token -- */

#[test]
fn leaves_a_word_inside_a_comment_or_a_string_alone() {
    let result = highlight(
        &["  // call render here", "  const s = \"render\";"],
        Language::TypeScript,
    );
    assert!(!shape(&result, 0).iter().any(|t| t == "ident:render"));
    assert!(!shape(&result, 1).iter().any(|t| t == "ident:render"));
}

#[test]
fn keeps_every_identifier_separately_claimable() {
    let rendered = shape(
        &highlight(&["render(); render();"], Language::TypeScript),
        0,
    );
    assert_eq!(
        rendered.iter().filter(|t| *t == "ident:render").count(),
        2,
        "{rendered:?}"
    );
}

#[test]
fn keeps_a_type_name_claimable_it_is_a_distinct_class_not_an_excluded_one() {
    let rendered = shape(
        &highlight(&["let store: Store = make();"], Language::TypeScript),
        0,
    );
    assert!(rendered.contains(&"type:Store".to_string()), "{rendered:?}");
}

#[test]
fn reproduces_the_line_exactly_the_code_block_renders_these_tokens() {
    let line = "  const s = `a ${b.c()} d`; // 1 + 2";
    assert_eq!(rebuilt(&highlight(&[line], Language::TypeScript), 0), line);
}

/* --------------------------------------------------------------------- cost */

#[test]
fn classifies_three_thousand_lines_of_typescript_well_inside_the_budget() {
    let text = include_str!("../../../ui/src/lib/symbol-model.ts");
    let mut lines: Vec<&str> = text.split('\n').collect();
    while lines.len() < 3000 {
        lines.extend(text.split('\n'));
    }
    lines.truncate(3000);
    let _ = highlight(&lines[..5], Language::TypeScript);
    let started = Instant::now();
    let result = highlight(&lines, Language::TypeScript);
    assert_eq!(result.engine, "tree-sitter");
    // Generous for a debug build on a loaded CI box: this is a smoke test for
    // a quadratic walk, not a benchmark.
    assert!(
        started.elapsed().as_millis() < 5000,
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn answers_a_cached_slice_without_re_classifying_it() {
    let cache = HighlightCache::default();
    let lines: Vec<String> = include_str!("../../../ui/src/lib/api.ts")
        .split('\n')
        .map(str::to_string)
        .collect();
    let cold = Instant::now();
    let first = highlight_lines(&cache, &lines, Some(Language::TypeScript), Some("a:1:9999"));
    let cold = cold.elapsed();
    let warm = Instant::now();
    let second = highlight_lines(&cache, &lines, Some(Language::TypeScript), Some("a:1:9999"));
    let warm = warm.elapsed();
    assert_eq!(second, first);
    assert!(
        warm <= cold.max(std::time::Duration::from_millis(5)),
        "{warm:?} vs {cold:?}"
    );
}

#[test]
fn bounds_the_cache_by_total_lines_not_just_by_entry_count() {
    let cache = HighlightCache::default();
    let big: Vec<String> = vec!["x".to_string(); SLICE_CACHE_LINES / 2 + 10];
    for key in ["one", "two", "three"] {
        highlight_lines(&cache, &big, Some(Language::Unknown), Some(key));
    }
    let (entries, lines) = cache.stats();
    assert!(entries < 3);
    assert!(lines <= SLICE_CACHE_LINES);
}

#[test]
fn keys_the_cache_on_the_content_so_an_edited_file_re_classifies() {
    let cache = HighlightCache::default();
    let first = highlight_lines(
        &cache,
        &["const a = 1;".to_string()],
        Some(Language::TypeScript),
        Some("hash-one:1:1"),
    );
    let second = highlight_lines(
        &cache,
        &["const bbb = 2;".to_string()],
        Some(Language::TypeScript),
        Some("hash-two:1:1"),
    );
    assert_eq!(rebuilt(&first, 0), "const a = 1;");
    assert_eq!(rebuilt(&second, 0), "const bbb = 2;");
}

/* ----------------------------------------------------- the classifier itself */

#[test]
fn covers_the_source_with_ordered_non_overlapping_spans() {
    let source = include_str!("../src/api/flow.rs");
    let spans = tokenize_source(source, Language::Rust)
        .expect("the Rust grammar")
        .spans;
    assert!(spans.len() > 1000, "{}", spans.len());
    let mut previous = 0;
    let mut uncovered = String::new();
    for span in &spans {
        assert!(span.start >= previous);
        assert!(span.end > span.start);
        uncovered.push_str(&source[previous..span.start]);
        previous = span.end;
    }
    assert!(previous <= source.len());
    // Everything the walk did not claim is whitespace the caller fills in.
    assert!(uncovered.trim().is_empty());
}
