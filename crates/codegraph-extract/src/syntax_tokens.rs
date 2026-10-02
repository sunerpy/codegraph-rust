//! Syntax classification from the engine's own tree-sitter parse — a port of
//! upstream `src/extraction/syntax-tokens.ts` (CG-57, `v1.6.1`), used by the
//! browser viewer's code blocks.
//!
//! The classes are deliberately few and the rules language-agnostic: a node
//! whose type mentions `comment` is a comment, whole; inside a string node every
//! leaf is string except below an interpolation; a numeric literal is a number;
//! an anonymous leaf is a keyword when its text is a bare word and punctuation
//! otherwise; a named identifier-shaped leaf is an identifier unless the grammar
//! called it a type name, or the extractor's own definition tables say it is the
//! name of a definition. That last rule reuses [`LanguageSpec`]'s node-type
//! lists, so a language that learns a declaration form gets its name bolded for
//! free.
//!
//! Offsets are BYTE offsets into the source (tree-sitter's), not upstream's
//! UTF-16 indices; the viewer only ever receives the classified text.

use codegraph_core::types::Language;
use tree_sitter::{Node, Parser};

use crate::lang::spec_for_language;
use crate::spec::LanguageSpec;

/// Every class a token can carry, in wire order.
pub const SYNTAX_TOKEN_CLASSES: [&str; 8] = [
    "other", "ident", "comment", "string", "keyword", "number", "type", "def",
];

/// A token class, by its wire index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SyntaxClass {
    Other = 0,
    Ident = 1,
    Comment = 2,
    String = 3,
    Keyword = 4,
    Number = 5,
    Type = 6,
    Def = 7,
}

impl SyntaxClass {
    pub fn id(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        SYNTAX_TOKEN_CLASSES[self as usize]
    }
}

/// A classified run of the source, by byte offset. Half-open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxSpan {
    pub start: usize,
    pub end: usize,
    pub class: SyntaxClass,
}

fn is_comment_type(kind: &str) -> bool {
    kind.contains("comment")
}

fn is_string_type(kind: &str) -> bool {
    kind.contains("string")
        || kind.contains("heredoc")
        || kind.contains("regex")
        || matches!(
            kind,
            "char_literal"
                | "character"
                | "character_literal"
                | "rune_literal"
                | "quoted_attribute_value"
        )
}

fn is_interpolation_type(kind: &str) -> bool {
    kind.contains("interpolation")
        || kind.contains("substitution")
        || matches!(
            kind,
            "template_substitution" | "string_interpolation" | "format_expression"
        )
}

fn is_number_type(kind: &str) -> bool {
    matches!(
        kind,
        "number"
            | "integer"
            | "float"
            | "number_literal"
            | "integer_literal"
            | "float_literal"
            | "decimal_integer_literal"
            | "decimal_floating_point_literal"
            | "hex_integer_literal"
            | "real_literal"
            | "numeric_literal"
            | "int_literal"
            | "imaginary_literal"
    )
}

fn is_type_name_type(kind: &str) -> bool {
    kind.contains("type_identifier") || kind == "type_name" || kind == "class_type"
}

/// Built-in type words, emitted whole and undescended with the `type` class.
fn is_builtin_type_type(kind: &str) -> bool {
    matches!(
        kind,
        "primitive_type" | "predefined_type" | "builtin_type" | "sized_type_specifier"
    )
}

/// Literal constants a theme groups with numbers.
fn is_constant_type(kind: &str) -> bool {
    matches!(
        kind,
        "true"
            | "false"
            | "null"
            | "nil"
            | "none"
            | "undefined"
            | "null_literal"
            | "nil_literal"
            | "boolean_literal"
            | "true_literal"
            | "false_literal"
    )
}

/// Upstream's `IDENT_SHAPE`: `[A-Za-z_$À-￿][\w$À-￿-]*`.
/// Every char at or above U+00C0 counts, astral ones included (JavaScript sees
/// those as two surrogates, both inside the range).
fn is_ident_shape(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let start = |c: char| c.is_ascii_alphabetic() || c == '_' || c == '$' || c >= '\u{C0}';
    start(first) && chars.all(|c| start(c) || c.is_ascii_digit() || c == '-')
}

/// Upstream's `WORD_SHAPE`: `[A-Za-z_][A-Za-z_0-9-]*` — separates a keyword from
/// punctuation among anonymous nodes.
fn is_word_shape(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn definition_types(spec: &dyn LanguageSpec) -> Vec<&'static str> {
    let mut out = Vec::new();
    out.extend_from_slice(spec.function_types());
    out.extend_from_slice(spec.class_types());
    out.extend_from_slice(spec.method_types());
    out.extend_from_slice(spec.interface_types());
    out.extend_from_slice(spec.struct_types());
    out.extend_from_slice(spec.enum_types());
    out.extend_from_slice(spec.type_alias_types());
    out.extend_from_slice(spec.union_types());
    out.extend_from_slice(spec.extra_class_node_types());
    out
}

struct Walk<'a> {
    source: &'a [u8],
    out: Vec<SyntaxSpan>,
    def_types: Vec<&'static str>,
    name_field: &'static str,
    def_starts: std::collections::HashSet<usize>,
    offset: usize,
}

impl Walk<'_> {
    fn emit(&mut self, start: usize, end: usize, class: SyntaxClass) {
        if end <= start {
            return;
        }
        let from = start + self.offset;
        let to = end + self.offset;
        if let Some(last) = self.out.last_mut()
            && last.class == class
            && last.end == from
        {
            last.end = to;
            return;
        }
        self.out.push(SyntaxSpan {
            start: from,
            end: to,
            class,
        });
    }

    fn text(&self, node: Node<'_>) -> &str {
        std::str::from_utf8(&self.source[node.start_byte()..node.end_byte()]).unwrap_or("")
    }

    fn visit(&mut self, node: Node<'_>, in_string: bool) {
        let kind = node.kind();
        if node.is_named() && is_comment_type(kind) {
            self.emit(node.start_byte(), node.end_byte(), SyntaxClass::Comment);
            return;
        }
        if node.is_named() && is_builtin_type_type(kind) {
            self.emit(node.start_byte(), node.end_byte(), SyntaxClass::Type);
            return;
        }
        // Mark the definition's own name before descending to it.
        if self.def_types.contains(&kind)
            && let Some(name) = node.child_by_field_name(self.name_field)
        {
            self.def_starts.insert(name.start_byte());
        }
        let count = node.child_count();
        if count == 0 {
            let class = self.leaf_class(node, in_string);
            self.emit(node.start_byte(), node.end_byte(), class);
            return;
        }
        let nested = if is_interpolation_type(kind) {
            false
        } else {
            in_string || is_string_type(kind)
        };
        for i in 0..count {
            if let Some(child) = node.child(i) {
                self.visit(child, nested);
            }
        }
    }

    fn leaf_class(&self, node: Node<'_>, in_string: bool) -> SyntaxClass {
        let kind = node.kind();
        // An anonymous node's kind is its own text: keyword or punctuation by shape.
        if !node.is_named() {
            if in_string {
                return SyntaxClass::String;
            }
            if is_constant_type(kind) {
                return SyntaxClass::Number;
            }
            return if is_word_shape(kind) {
                SyntaxClass::Keyword
            } else {
                SyntaxClass::Other
            };
        }
        if in_string || is_string_type(kind) {
            return SyntaxClass::String;
        }
        if is_number_type(kind) || is_constant_type(kind) {
            return SyntaxClass::Number;
        }
        let text = self.text(node);
        if self.def_starts.contains(&node.start_byte()) && is_ident_shape(text) {
            return SyntaxClass::Def;
        }
        if is_type_name_type(kind) {
            return SyntaxClass::Type;
        }
        if is_ident_shape(text) {
            SyntaxClass::Ident
        } else {
            SyntaxClass::Other
        }
    }
}

/// Classify one parsed tree. `offset` shifts every span (a component's script
/// block sits inside its file).
pub fn classify_tree(
    root: Node<'_>,
    source: &str,
    language: Language,
    offset: usize,
) -> Vec<SyntaxSpan> {
    let spec = spec_for_language(language);
    let mut walk = Walk {
        source: source.as_bytes(),
        out: Vec::new(),
        def_types: spec.map(definition_types).unwrap_or_default(),
        name_field: spec.map(|s| s.name_field()).unwrap_or("name"),
        def_starts: std::collections::HashSet::new(),
        offset,
    };
    walk.visit(root, false);
    walk.out
}

/// A stretch of a file written in a different language from the file itself:
/// a single-file component's script block (or Astro's frontmatter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxRegion {
    pub start: usize,
    pub end: usize,
    pub language: Language,
}

fn find_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let lower = haystack.get(from..)?.to_ascii_lowercase();
    lower.find(&needle.to_ascii_lowercase()).map(|i| i + from)
}

/// The sub-language regions of a file, or `None` when the file is one language.
/// `Some(vec![])` means "a grammar covers none of it".
pub fn syntax_regions_for(source: &str, language: Language) -> Option<Vec<SyntaxRegion>> {
    if !matches!(language, Language::Svelte | Language::Vue | Language::Astro) {
        return None;
    }
    let mut regions = Vec::new();
    if language == Language::Astro
        && let Some(rest) = source.strip_prefix("---")
    {
        let fence = if rest.starts_with("\r\n") {
            Some(5)
        } else if rest.starts_with('\n') {
            Some(4)
        } else {
            None
        };
        if let Some(start) = fence {
            // The body runs to the next line that is exactly `---`.
            let body = &source[start..];
            let mut end = None;
            let mut cursor = 0;
            for line in body.split_inclusive('\n') {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if trimmed == "---" {
                    end = Some(cursor);
                    break;
                }
                cursor += line.len();
            }
            if let Some(end) = end {
                let mut body_end = start + end;
                // Upstream's lazy `([\s\S]*?)\r?\n---` stops before the newline.
                if source[..body_end].ends_with('\n') {
                    body_end -= 1;
                    if source[..body_end].ends_with('\r') {
                        body_end -= 1;
                    }
                }
                if body_end > start {
                    regions.push(SyntaxRegion {
                        start,
                        end: body_end,
                        language: Language::TypeScript,
                    });
                }
            }
        }
    }
    let mut from = 0;
    while let Some(open) = find_ci(source, "<script", from) {
        let after = open + "<script".len();
        // `<script` must end the tag name: whitespace or `>`.
        let next = source[after..].chars().next();
        if !matches!(next, Some(c) if c == '>' || c.is_whitespace()) {
            from = after;
            continue;
        }
        let Some(tag_end) = source[after..].find('>').map(|i| i + after) else {
            break;
        };
        let attrs = &source[after..tag_end];
        let body_start = tag_end + 1;
        let Some(close) = find_ci(source, "</script>", body_start) else {
            break;
        };
        let body = &source[body_start..close];
        if !body.trim().is_empty() {
            let lower = attrs.to_ascii_lowercase();
            let is_ts = [
                "lang=\"ts\"",
                "lang='ts'",
                "lang=\"typescript\"",
                "lang='typescript'",
            ]
            .iter()
            .any(|p| lower.replace(' ', "").contains(p));
            regions.push(SyntaxRegion {
                start: body_start,
                end: close,
                language: if is_ts {
                    Language::TypeScript
                } else {
                    Language::JavaScript
                },
            });
        }
        from = close + "</script>".len();
    }
    Some(regions)
}

/// The classified spans and the grammar(s) that produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizeResult {
    pub spans: Vec<SyntaxSpan>,
    pub grammars: Vec<Language>,
}

fn tokenize_region(source: &str, language: Language, offset: usize) -> Option<Vec<SyntaxSpan>> {
    let spec = spec_for_language(language)?;
    let mut parser = Parser::new();
    parser
        .set_language(&spec.tree_sitter_language_for_source(source))
        .ok()?;
    let tree = parser.parse(source, None)?;
    Some(classify_tree(tree.root_node(), source, language, offset))
}

/// Parse `source` and classify it; `None` when nothing in it has a grammar.
/// Never panics on a bad parse: a grammar that will not load is the same
/// outcome as not having one.
pub fn tokenize_source(source: &str, language: Language) -> Option<TokenizeResult> {
    match syntax_regions_for(source, language) {
        None => {
            let spans = tokenize_region(source, language, 0)?;
            Some(TokenizeResult {
                spans,
                grammars: vec![language],
            })
        }
        Some(regions) => {
            if regions.is_empty() {
                return None;
            }
            let mut spans = Vec::new();
            let mut grammars: Vec<Language> = Vec::new();
            for region in regions {
                let Some(part) = tokenize_region(
                    &source[region.start..region.end],
                    region.language,
                    region.start,
                ) else {
                    continue;
                };
                if !grammars.contains(&region.language) {
                    grammars.push(region.language);
                }
                spans.extend(part);
            }
            if spans.is_empty() {
                return None;
            }
            spans.sort_by_key(|s| s.start);
            Some(TokenizeResult { spans, grammars })
        }
    }
}

/// Whether a grammar covers `language` here (component languages read their
/// script through TypeScript), as upstream's `grammarFor`.
pub fn grammar_for(language: Language) -> Option<Language> {
    if matches!(language, Language::Svelte | Language::Vue | Language::Astro) {
        return Some(Language::TypeScript);
    }
    spec_for_language(language).map(|_| language)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(source: &str, language: Language) -> Vec<(String, &'static str)> {
        let result = tokenize_source(source, language).expect("grammar");
        result
            .spans
            .iter()
            .map(|s| (source[s.start..s.end].to_string(), s.class.name()))
            .collect()
    }

    fn class_of<'a>(spans: &'a [(String, &'static str)], text: &str) -> Option<&'a str> {
        spans.iter().find(|(t, _)| t == text).map(|(_, c)| *c)
    }

    // Ported from upstream __tests__/ui-highlight.test.ts "classification".
    #[test]
    fn reads_typescript_with_the_classes_the_theme_paints() {
        let spans = classes(
            "const total = add(1, 2); // sum\nfunction add(a: number, b: number) { return a + b; }\n",
            Language::TypeScript,
        );
        assert_eq!(class_of(&spans, "const"), Some("keyword"));
        assert_eq!(class_of(&spans, "total"), Some("ident"));
        assert_eq!(class_of(&spans, "1"), Some("number"));
        assert_eq!(class_of(&spans, "// sum"), Some("comment"));
        assert_eq!(class_of(&spans, "number"), Some("type"));
        assert!(
            spans.iter().any(|(t, c)| t == "add" && *c == "def"),
            "the declared name is bold"
        );
    }

    #[test]
    fn a_hash_comment_is_a_comment_in_python() {
        let spans = classes("x = 1  # note\n", Language::Python);
        assert_eq!(class_of(&spans, "# note"), Some("comment"));
    }

    #[test]
    fn a_type_annotation_string_is_not_a_string_literal() {
        let spans = classes("let key: string = \"v\";\n", Language::TypeScript);
        assert_eq!(class_of(&spans, "string"), Some("type"));
        assert_eq!(class_of(&spans, "\"v\""), Some("string"));
    }

    #[test]
    fn a_template_literal_interpolation_stays_code() {
        let spans = classes("const s = `a ${user.name()} b`;\n", Language::TypeScript);
        assert_eq!(class_of(&spans, "user"), Some("ident"));
        assert_eq!(class_of(&spans, "name"), Some("ident"));
    }

    #[test]
    fn go_keywords_land_without_naming_them() {
        let spans = classes(
            "package main\nfunc main() { var x int = 1; _ = x }\n",
            Language::Go,
        );
        assert_eq!(class_of(&spans, "func"), Some("keyword"));
        assert_eq!(class_of(&spans, "package"), Some("keyword"));
    }

    #[test]
    fn rust_declarations_and_builtin_types() {
        let spans = classes("pub fn resolve(p: &str) -> u32 { 0 }\n", Language::Rust);
        assert_eq!(class_of(&spans, "resolve"), Some("def"));
        assert_eq!(class_of(&spans, "u32"), Some("type"));
        assert_eq!(class_of(&spans, "fn"), Some("keyword"));
    }

    #[test]
    fn spans_are_ordered_and_non_overlapping() {
        let source = "fn a() { let b = \"c\"; /* d */ }\n";
        let result = tokenize_source(source, Language::Rust).unwrap();
        assert!(result.spans.windows(2).all(|w| w[0].end <= w[1].start));
        assert!(result.spans.iter().all(|s| s.end <= source.len()));
    }

    #[test]
    fn a_component_is_read_through_its_script_block() {
        let source =
            "<template><div/></template>\n<script lang=\"ts\">\nconst a: number = 1;\n</script>\n";
        let regions = syntax_regions_for(source, Language::Vue).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].language, Language::TypeScript);
        let spans = classes(source, Language::Vue);
        assert_eq!(class_of(&spans, "const"), Some("keyword"));
        assert!(
            !spans.iter().any(|(t, _)| t.contains("template")),
            "markup stays unclassified"
        );
    }

    #[test]
    fn an_empty_script_block_has_no_grammar() {
        let regions = syntax_regions_for("<script>  </script>", Language::Svelte).unwrap();
        assert!(regions.is_empty());
        assert!(tokenize_source("<script>  </script>", Language::Svelte).is_none());
    }

    #[test]
    fn languages_without_a_grammar_answer_none() {
        assert!(tokenize_source("a: 1\n", Language::Yaml).is_none());
        assert_eq!(grammar_for(Language::Yaml), None);
        assert_eq!(grammar_for(Language::Svelte), Some(Language::TypeScript));
    }
}
