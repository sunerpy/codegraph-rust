//! Kotlin `LanguageSpec`.
//!
//! Ports `upstream extraction/languages/kotlin.ts:71-308` onto the
//! `tree-sitter-kotlin-ng` node names used by this Rust workspace.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct KotlinSpec;

pub static KOTLIN_SPEC: KotlinSpec = KotlinSpec;

impl LanguageSpec for KotlinSpec {
    fn language(&self) -> Language {
        Language::Kotlin
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_kotlin_ng::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_declaration"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["function_declaration"]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_entry"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_alias"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }

    fn field_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
    }

    fn extra_class_node_types(&self) -> &'static [&'static str] {
        &["object_declaration"]
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "function_body"
    }

    fn params_field(&self) -> &'static str {
        "function_value_parameters"
    }

    fn return_field(&self) -> &'static str {
        "type"
    }

    fn pre_parse(&self, source: &str, _file_path: &str) -> String {
        rewrite_newer_kotlin_syntax(source)
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, _body_field: &str) -> Option<Node<'tree>> {
        // Upstream kotlin.ts:174-196 resolves Kotlin bodies by child type rather
        // than field name because the upstream grammar historically had no fields.
        node.named_children(&mut node.walk()).find(|child| {
            matches!(
                child.kind(),
                "function_body" | "class_body" | "enum_class_body"
            )
        })
    }

    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        if node.kind() == "type_alias" {
            return child_by_field(node, "type").map(|name| node_text(name, source));
        }
        None
    }

    fn classify_class_node(&self, node: Node<'_>) -> NodeKind {
        // Upstream kotlin.ts:197-210 scans DIRECT keyword children only
        // ('interface' / 'enum'); a whole-subtree scan would misclassify a
        // class containing a nested enum. tree-sitter-kotlin-ng nests the
        // 'enum' keyword under modifiers > class_modifier, so the modifiers
        // child is also scanned; the scan stops at the class body.
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx) else {
                continue;
            };
            match child.kind() {
                "interface" => return NodeKind::Interface,
                "enum" => return NodeKind::Enum,
                "modifiers" => {
                    if has_descendant_kind(child, "enum") {
                        return NodeKind::Enum;
                    }
                }
                "class_body" | "enum_class_body" => break,
                _ => {}
            }
        }
        NodeKind::Class
    }

    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:211-231: extension receiver is the user_type before '.'.
        let mut found_user_type = None;
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx) else {
                continue;
            };
            if child.kind() == "user_type" {
                found_user_type = Some(child);
            } else if child.kind() == "." {
                let receiver = found_user_type?;
                return Some(bare_type_name(receiver, source));
            } else if child.kind() == "identifier" || child.kind() == "function_value_parameters" {
                break;
            }
        }
        None
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:232-242 intends to emit parameters plus an optional
        // return type, but looks up function_value_parameters through a missing
        // grammar field. Diverge by locating that direct child by KIND, matching
        // the traversal already used for Kotlin declared return types below.
        let mut signature = None;
        for child in node.named_children(&mut node.walk()) {
            if signature.is_none() {
                if child.kind() == "function_value_parameters" {
                    signature = Some(node_text(child, source));
                }
                continue;
            }

            if matches!(child.kind(), "function_body" | "type_constraints") {
                break;
            }
            if matches!(
                child.kind(),
                "dynamic"
                    | "function_type"
                    | "non_nullable_type"
                    | "nullable_type"
                    | "parenthesized_type"
                    | "user_type"
            ) {
                let mut signature = signature.expect("parameters found above");
                signature.push_str(": ");
                signature.push_str(&node_text(child, source));
                return Some(signature);
            }
        }
        signature
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:5-43 normalizes declared return types to bare class names.
        let return_type = declared_return_type_node(node)?;
        let name = bare_type_name(return_type, source);
        if name.is_empty() || matches!(name.as_str(), "Unit" | "Nothing") || !is_ident(&name) {
            return None;
        }
        Some(name)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        // Upstream kotlin.ts:243-256 defaults Kotlin visibility to public.
        for child in node.children(&mut node.walk()) {
            if child.kind() != "modifiers" {
                continue;
            }
            if has_descendant_kind(child, "public") {
                return Some("public".to_string());
            }
            if has_descendant_kind(child, "private") {
                return Some("private".to_string());
            }
            if has_descendant_kind(child, "protected") {
                return Some("protected".to_string());
            }
            if has_descendant_kind(child, "internal") {
                return Some("internal".to_string());
            }
        }
        Some("public".to_string())
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        node.children(&mut node.walk())
            .any(|child| child.kind() == "modifiers" && has_descendant_kind(child, "suspend"))
    }

    fn extract_modifiers(&self, node: Node<'_>) -> Vec<String> {
        // Upstream kotlin.ts:271-293: Kotlin Multiplatform expect/actual markers
        // under modifiers > platform_modifier, matched by AST node not text.
        let mut mods = Vec::new();
        for child in node.children(&mut node.walk()) {
            if child.kind() != "modifiers" {
                continue;
            }
            for pm in child.children(&mut child.walk()) {
                if pm.kind() != "platform_modifier" {
                    continue;
                }
                for kw in pm.children(&mut pm.walk()) {
                    if matches!(kw.kind(), "expect" | "actual") {
                        mods.push(kw.kind().to_string());
                    }
                }
            }
        }
        mods
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        // Upstream kotlin.ts:294-301 emits import_header text and module name.
        let identifier = first_descendant_named(node, "qualified_identifier")
            .or_else(|| first_descendant_named(node, "identifier"))?;
        Some(ImportInfo {
            module_name: node_text(identifier, source),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }

    fn package_types(&self) -> &'static [&'static str] {
        &["package_header"]
    }

    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        // Upstream kotlin.ts:302-307 package_header -> dotted identifier.
        first_descendant_named(node, "qualified_identifier").map(|id| node_text(id, source))
    }
}

fn declared_return_type_node(node: Node<'_>) -> Option<Node<'_>> {
    let mut seen_params = false;
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "function_value_parameters" {
            seen_params = true;
            continue;
        }
        if !seen_params {
            continue;
        }
        if matches!(child.kind(), "function_body" | "type_constraints") {
            return None;
        }
        if matches!(child.kind(), "user_type" | "nullable_type") {
            return Some(child);
        }
    }
    None
}

fn bare_type_name(node: Node<'_>, source: &str) -> String {
    let leaf = first_descendant_named(node, "identifier").unwrap_or(node);
    node_text(leaf, source)
        .trim_matches('`')
        .trim_end_matches('?')
        .to_string()
}

fn first_descendant_named<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    for child in node.named_children(&mut node.walk()) {
        if let Some(found) = first_descendant_named(child, kind) {
            return Some(found);
        }
    }
    None
}

fn has_descendant_kind(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    for idx in 0..node.child_count() {
        if node
            .child(idx)
            .is_some_and(|child| has_descendant_kind(child, kind))
        {
            return true;
        }
    }
    false
}

/// Kotlin syntax newer than tree-sitter-kotlin-ng 1.1.0, rewritten to an
/// older equivalent of the same length so every offset survives (upstream
/// #2182 `rewriteNewerKotlinSyntax`); either one otherwise made the file
/// contribute nothing:
///
/// - a `when` guard (Kotlin 2.1), `is H2Dialect if dialect.mode == X ->`,
///   becomes a second condition, `is H2Dialect,   dialect.mode == X ->`, so
///   the guard's references are kept;
/// - a multi-dollar string (2.1), `$$"?(@.a == $x)"`, loses its prefix.
///
/// The grammar parses an open-ended range (`1..<n`) and a nullable
/// function-type receiver (`Op<Boolean>?.() -> …`) itself, so upstream's
/// other two rewrites have nothing to do here.
pub(crate) fn rewrite_newer_kotlin_syntax(source: &str) -> String {
    let mut out = source.as_bytes().to_vec();
    // Upstream's guard: a file with no ` if` is left alone, even when a
    // guard is written after a tab.
    if source.contains(" if") {
        rewrite_when_guards(&mut out);
    }
    if source.contains("$$") {
        blank_multi_dollar_prefixes(&mut out);
    }
    // Only ASCII bytes were replaced, by ASCII bytes.
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// Upstream's `WHEN_GUARD`,
/// `^([ \t]*!?(?:is|in)[ \t]+(?:(?!->)[^\n])*?[\w>?)\]'"])([ \t]+)if(?=[ \t(])`
/// (global, multiline), replaced by the condition, a comma and the gap and
/// `if` as spaces. The condition stops short of `->`: `is X -> if (…)` is a
/// branch whose body is an `if`.
fn rewrite_when_guards(bytes: &mut [u8]) {
    let mut resume = 0;
    for start in js_line_starts(bytes) {
        if start < resume {
            continue;
        }
        if let Some((comma, end)) = when_guard_at(bytes, start) {
            bytes[comma] = b',';
            bytes[comma + 1..end].fill(b' ');
            resume = end;
        }
    }
}

/// The positions JavaScript's multiline `^` matches at: the start and after
/// every line terminator (`\n`, `\r`, U+2028, U+2029).
fn js_line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    for (at, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' || byte == b'\r' {
            starts.push(at + 1);
        } else if byte == 0xE2
            && bytes.get(at + 1) == Some(&0x80)
            && matches!(bytes.get(at + 2), Some(0xA8 | 0xA9))
        {
            starts.push(at + 3);
        }
    }
    starts
}

/// A `when` guard starting at line start `start`: the position of the gap
/// after the condition and the end of `if`.
fn when_guard_at(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let blank = |at: usize| matches!(bytes.get(at), Some(b' ' | b'\t'));
    let mut at = start;
    while blank(at) {
        at += 1;
    }
    if bytes.get(at) == Some(&b'!') {
        at += 1;
    }
    if !matches!(bytes.get(at..at + 2), Some(b"is" | b"in")) {
        return None;
    }
    at += 2;
    if !blank(at) {
        return None;
    }
    while blank(at) {
        at += 1;
    }
    loop {
        let byte = *bytes.get(at)?;
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'>' | b'?' | b')' | b']' | b'\'' | b'"')
        {
            let mut gap_end = at + 1;
            while blank(gap_end) {
                gap_end += 1;
            }
            if gap_end > at + 1
                && bytes.get(gap_end..gap_end + 2) == Some(b"if")
                && matches!(bytes.get(gap_end + 2), Some(b' ' | b'\t' | b'('))
            {
                return Some((at + 1, gap_end + 2));
            }
        }
        // The condition goes on through any character but a line feed and
        // the start of `->`.
        if byte == b'\n' || (byte == b'-' && bytes.get(at + 1) == Some(&b'>')) {
            return None;
        }
        at += 1;
    }
}

/// Upstream's `MULTI_DOLLAR_STRING`, `(?<![\w$"])\$\$+(?=")` (global),
/// blanked.
fn blank_multi_dollar_prefixes(bytes: &mut [u8]) {
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'$' {
            at += 1;
            continue;
        }
        let mut end = at;
        while bytes.get(end) == Some(&b'$') {
            end += 1;
        }
        let after_word = at > 0
            && (bytes[at - 1].is_ascii_alphanumeric()
                || matches!(bytes[at - 1], b'_' | b'$' | b'"'));
        if !after_word && end - at >= 2 && bytes.get(end) == Some(&b'"') {
            bytes[at..end].fill(b' ');
        }
        at = end;
    }
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    fn first_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        for i in 0..node.named_child_count() {
            let child = node.named_child(i as u32)?;
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn trait_field_constants_are_stable() {
        assert_eq!(KOTLIN_SPEC.name_field(), "name");
        assert_eq!(KOTLIN_SPEC.body_field(), "function_body");
        assert_eq!(KOTLIN_SPEC.params_field(), "function_value_parameters");
        assert_eq!(KOTLIN_SPEC.return_field(), "type");
    }

    #[test]
    fn nullable_return_and_missing_return() {
        let src = "class C {\n  fun a(): Widget? = null\n  fun b() {}\n}\n";
        let tree = parse(src);
        let mut fns = Vec::new();
        fn walk<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
            if n.kind() == "function_declaration" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i as u32) {
                    walk(c, out);
                }
            }
        }
        walk(tree.root_node(), &mut fns);
        assert_eq!(
            KOTLIN_SPEC.get_return_type(fns[0], src).as_deref(),
            Some("Widget")
        );
        assert!(KOTLIN_SPEC.get_return_type(fns[1], src).is_none());
    }

    #[test]
    fn direct_kotlin_signatures_are_exact() {
        let cases = [
            (
                "fun greet(name: String, times: Int = 1): String = name",
                "(name: String, times: Int = 1): String",
            ),
            ("fun inferred(flag: Boolean) = flag", "(flag: Boolean)"),
            ("fun <T> identity(value: T): T = value", "(value: T): T"),
            (
                "fun factory(): (Int) -> String = TODO()",
                "(): (Int) -> String",
            ),
            (
                "fun String.decorate(prefix: String): String = prefix + this",
                "(prefix: String): String",
            ),
        ];

        for (src, expected) in cases {
            let tree = parse(src);
            let function = first_of_kind(tree.root_node(), "function_declaration").unwrap();
            assert_eq!(
                KOTLIN_SPEC.get_signature(function, src).as_deref(),
                Some(expected),
                "source: {src}"
            );
        }
    }

    #[test]
    fn extracted_kotlin_signatures_preserve_scope_and_shape() {
        let src = r#"fun greet(name: String, times: Int = 1): String = name
fun inferred(flag: Boolean) = flag
fun <T> identity(value: T): T = value

class Processor(val id: Int) {
    fun factory(): (Int) -> String = TODO()
    val mapper = { value: Int -> value.toString() }
}

val topLevelMapper = { value: Int -> value + 1 }
fun String.decorate(prefix: String): String = prefix + this
"#;

        let result = crate::extract_source("signatures.kt", src, Some(Language::Kotlin));
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let processor = result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Class && node.name == "Processor")
            .expect("Processor class");
        assert_eq!(processor.signature, None);

        let callables = result
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
            .collect::<Vec<_>>();
        assert_eq!(callables.len(), 5);
        assert!(callables.iter().all(|node| node.name != "mapper"));
        assert!(callables.iter().all(|node| node.name != "topLevelMapper"));

        let expected = [
            (
                "greet",
                NodeKind::Function,
                "(name: String, times: Int = 1): String",
            ),
            ("inferred", NodeKind::Function, "(flag: Boolean)"),
            ("identity", NodeKind::Function, "(value: T): T"),
            ("factory", NodeKind::Method, "(): (Int) -> String"),
            ("decorate", NodeKind::Method, "(prefix: String): String"),
        ];
        for (name, kind, signature) in expected {
            let node = callables
                .iter()
                .find(|node| node.name == name)
                .unwrap_or_else(|| panic!("missing callable {name}"));
            assert_eq!(node.kind, kind, "callable {name}");
            assert_eq!(
                node.signature.as_deref(),
                Some(signature),
                "callable {name}"
            );
        }
    }

    #[test]
    fn visibility_public_default_and_modifiers() {
        let src = "class C {\n  private fun a() {}\n  protected fun b() {}\n  internal fun c() {}\n  fun d() {}\n}\n";
        let tree = parse(src);
        let mut fns = Vec::new();
        fn walk<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
            if n.kind() == "function_declaration" {
                out.push(n);
            }
            for i in 0..n.named_child_count() {
                if let Some(c) = n.named_child(i as u32) {
                    walk(c, out);
                }
            }
        }
        walk(tree.root_node(), &mut fns);
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[0]).as_deref(),
            Some("private")
        );
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[1]).as_deref(),
            Some("protected")
        );
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[2]).as_deref(),
            Some("internal")
        );
        assert_eq!(
            KOTLIN_SPEC.get_visibility(fns[3]).as_deref(),
            Some("public")
        );
    }

    #[test]
    fn qualified_import_produces_module() {
        let src = "import com.example.util.Helper\n";
        let tree = parse(src);
        let import = first_of_kind(tree.root_node(), "import").unwrap();
        let info = KOTLIN_SPEC.extract_import(import, src).unwrap();
        assert!(info.module_name.contains("Helper") || !info.module_name.is_empty());
    }

    /// Upstream #2182's fixture: a `when` guard and a multi-dollar string
    /// are rewritten with every offset intact, and the result parses; the
    /// grammar's own `..<` and nullable function-type receiver stay.
    #[test]
    fn newer_syntax_is_rewritten_with_every_offset_intact() {
        let src = "package app\n\nclass Dialect(private val mode: Int) {\n    fun sql(value: Any?): String = when (value) {\n        is String if mode == 2 -> \"text\"\n        is Int if (\n            mode > 0\n        ) -> \"int\"\n        is Long -> if (mode == 1) \"long\" else \"big\"\n        else -> \"other\"\n    }\n\n    fun ranges(n: Int) {\n        for (i in 0..<n) println(i)\n    }\n\n    fun path(team: String) = $$\"?(@.user.team == $team)\"\n\n    fun adjust(body: Op<Boolean>?.() -> Op<Boolean>): Dialect = this\n\n    fun after(): Int = 1\n}\n\nclass Op<T>\n";
        assert!(parse(src).root_node().has_error());
        let out = rewrite_newer_kotlin_syntax(src);
        assert_eq!(out.len(), src.len());
        assert!(out.contains("is String,   mode == 2 ->"));
        assert!(out.contains("is Int,   (\n"));
        assert!(out.contains("is Long -> if (mode == 1)"));
        assert!(out.contains("0..<n"));
        assert!(out.contains("=   \"?(@.user.team == $team)\""));
        assert!(out.contains("Op<Boolean>?.() -> Op<Boolean>"));
        assert!(!parse(&out).root_node().has_error());
    }

    #[test]
    fn when_guards_follow_upstreams_pattern() {
        let rewrite = |src: &str| rewrite_newer_kotlin_syntax(src);
        // `!is`, `in`, a type argument and a closing bracket end a condition.
        assert_eq!(
            rewrite("  !in 1..3 if ok -> 1\n  is Map<*, *> if (x) -> 2\n  is A? if y -> 3\n"),
            "  !in 1..3,   ok -> 1\n  is Map<*, *>,   (x) -> 2\n  is A?,   y -> 3\n"
        );
        // Not a guard: `if` after `->`, not at a line's condition, not
        // followed by a blank or `(`, not after a blank.
        for src in [
            "  is Long -> if (a) 1 else 2\n",
            "  x is Long if (a) -> 1\n",
            "  is Long ifx -> 1\n",
            "  is Long iff (a) -> 1\n",
            "  isLong if (a) -> 1\n",
            "  is Long,if (a) -> 1\n",
        ] {
            assert_eq!(rewrite(src), src);
        }
        // The first ` if` on the line is the guard's.
        assert_eq!(
            rewrite("  is A if b if c -> 1\n"),
            "  is A,   b if c -> 1\n"
        );
        // A tab gap counts, but only in a file with a ` if` somewhere.
        assert_eq!(rewrite("  is A\tif b -> 1\n"), "  is A\tif b -> 1\n");
        assert_eq!(
            rewrite("  is A\tif b -> 1\n  // if\n"),
            "  is A,   b -> 1\n  // if\n"
        );
        // CRLF lines and a guard on the line after a lone carriage return.
        assert_eq!(
            rewrite("  is A if b -> 1\r\n  is B if c -> 2\r  is C if d -> 3"),
            "  is A,   b -> 1\r\n  is B,   c -> 2\r  is C,   d -> 3"
        );
        // Non-ASCII text survives.
        assert_eq!(rewrite("  is Ä if é -> \"ü\"\n"), "  is Ä if é -> \"ü\"\n");
        assert_eq!(
            rewrite("  is Aä if é -> \"ü\"\n"),
            "  is Aä if é -> \"ü\"\n"
        );
        assert_eq!(rewrite("  is A if é -> \"ü\"\n"), "  is A,   é -> \"ü\"\n");
    }

    #[test]
    fn multi_dollar_prefixes_follow_upstreams_pattern() {
        let rewrite = |src: &str| rewrite_newer_kotlin_syntax(src);
        assert_eq!(rewrite("val a = $$\"x $y\""), "val a =   \"x $y\"");
        assert_eq!(rewrite("val a = $$$\"x\""), "val a =    \"x\"");
        assert_eq!(rewrite("f($$\"x\")"), "f(  \"x\")");
        // After a word character, a `$` or a quote, or not before a quote.
        for src in ["x$$\"y\"", "\"$$\"", "a $$ b", "a $\"b\""] {
            assert_eq!(rewrite(src), src);
        }
        // Upstream blanks a run closing a plain string, too.
        assert_eq!(rewrite("\"cost $$\""), "\"cost   \"");
    }
}
