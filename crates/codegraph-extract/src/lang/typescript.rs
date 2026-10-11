//! TypeScript `LanguageSpec`.
//!
//! Direct port of `upstream extraction/languages/typescript.ts:4-118`.

use std::sync::OnceLock;

use codegraph_core::types::Language;
use regex::Regex;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct TypeScriptSpec;

pub static TYPESCRIPT_SPEC: TypeScriptSpec = TypeScriptSpec;

impl LanguageSpec for TypeScriptSpec {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[
            "function_declaration",
            "generator_function_declaration",
            "arrow_function",
            "function_expression",
            "generator_function",
        ]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration", "abstract_class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &[
            "method_definition",
            "public_field_definition",
            "method_signature",
        ]
    }

    fn property_types(&self) -> &'static [&'static str] {
        &["property_signature"]
    }

    fn class_member_is_method(&self, node: Node<'_>, _source: &str) -> bool {
        class_field_is_callable(node, "public_field_definition")
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &["interface_declaration"]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_declaration"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        &["property_identifier", "enum_assignment"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_alias_declaration"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import_statement"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["lexical_declaration", "variable_declaration"]
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "body"
    }

    fn params_field(&self) -> &'static str {
        "parameters"
    }

    fn return_field(&self) -> &'static str {
        "return_type"
    }

    fn pre_parse(&self, source: &str, file_path: &str) -> String {
        blank_flow_syntax(source, file_path)
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        if node.kind() != "public_field_definition" {
            return None;
        }

        for i in 0..node.named_child_count() {
            let child = node.named_child(i as u32)?;
            if matches!(
                child.kind(),
                "arrow_function" | "function_expression" | "generator_function"
            ) {
                return child_by_field(child, body_field);
            }
            if child.kind() == "call_expression" {
                if let Some(args) = child_by_field(child, "arguments") {
                    for j in 0..args.named_child_count() {
                        if let Some(arg) = args.named_child(j as u32) {
                            if matches!(
                                arg.kind(),
                                "arrow_function" | "function_expression" | "generator_function"
                            ) {
                                return child_by_field(arg, body_field);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let mut signature = node_text(params, source);
        if let Some(return_type) = child_by_field(node, "return_type") {
            signature.push_str(": ");
            signature.push_str(
                node_text(return_type, source)
                    .trim_start_matches(':')
                    .trim_start(),
            );
        }
        Some(signature)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        for i in 0..node.child_count() {
            let child = node.child(i)?;
            if child.kind() == "accessibility_modifier" {
                for j in 0..child.child_count() {
                    let modifier = child.child(j)?;
                    match modifier.kind() {
                        "public" => return Some("public".to_string()),
                        "private" => return Some("private".to_string()),
                        "protected" => return Some("protected".to_string()),
                        _ => {}
                    }
                }
            }
        }
        None
    }

    fn is_exported(&self, node: Node<'_>, _source: &str) -> bool {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.kind() == "export_statement" {
                return true;
            }
            current = parent.parent();
        }
        false
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        has_direct_child_kind(node, "async")
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        has_direct_child_kind(node, "static")
    }

    fn is_const(&self, node: Node<'_>) -> bool {
        node.kind() == "lexical_declaration" && has_direct_child_kind(node, "const")
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let source_node = child_by_field(node, "source")?;
        let module_name = node_text(source_node, source).replace(['\'', '"'], "");
        if module_name.is_empty() {
            return None;
        }
        Some(ImportInfo {
            module_name,
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
}

/// JavaScript's `\s` as a regex class: whitespace and line terminators,
/// `U+FEFF` included.
const JS_SPACE: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// Flow's own syntax blanked to spaces, so a Flow-typed file reads as TSX
/// (upstream #2161 `blankFlowSyntax`): exact object types `{| a: T |}`, a
/// maybe type's `?` (`x: ?string`, `Array<?T>`), an inexact object's bare
/// `...`, `import typeof` and `opaque type`. Only a `.js`, `.jsx`, `.mjs` or
/// `.cjs` file with the `@flow` pragma changes, the files detection reads as
/// TSX for it. Every byte offset survives, and so does every line: a line
/// break inside `import typeof` stays, where upstream's blanks it too.
pub(crate) fn blank_flow_syntax(source: &str, file_path: &str) -> String {
    static IMPORT_TYPEOF: OnceLock<Regex> = OnceLock::new();
    static OPAQUE: OnceLock<Regex> = OnceLock::new();
    static INEXACT: OnceLock<Regex> = OnceLock::new();
    let script = [".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .any(|extension| file_path.ends_with(extension));
    if !script || !crate::engine::has_flow_pragma(source) {
        return source.to_string();
    }
    let code = source.replace("{|", "{ ").replace("|}", " }");
    // A maybe type's `?` after `:`, `<` or `,`, before what it qualifies.
    let mut bytes = code.clone().into_bytes();
    for (at, punctuation) in code.char_indices() {
        if !matches!(punctuation, ':' | '<' | ',') {
            continue;
        }
        let rest = &code[at + 1..];
        let mark = rest.trim_start_matches(codegraph_core::generated_header::is_js_whitespace);
        let qualifies = mark
            .strip_prefix('?')
            .and_then(|after| after.chars().next());
        if qualifies.is_some_and(|c| c.is_ascii_alphanumeric() || "_$({[".contains(c)) {
            bytes[at + 1 + rest.len() - mark.len()] = b' ';
        }
    }
    let code = String::from_utf8(bytes).expect("a `?` blanked to a space keeps UTF-8");
    let code = IMPORT_TYPEOF
        .get_or_init(|| {
            Regex::new(&format!(r"(?-u:\b)import{JS_SPACE}+typeof(?-u:\b)")).expect("import typeof")
        })
        .replace_all(&code, |found: &regex::Captures<'_>| {
            let typeof_clause = &found[0]["import".len()..];
            let blank: String = typeof_clause
                .bytes()
                .map(|byte| {
                    if matches!(byte, b'\n' | b'\r') {
                        byte as char
                    } else {
                        ' '
                    }
                })
                .collect();
            format!("import{blank}")
        });
    let code = OPAQUE
        .get_or_init(|| {
            Regex::new(&format!(r"(?-u:\b)opaque({JS_SPACE}+type(?-u:\b))")).expect("opaque type")
        })
        .replace_all(&code, "      ${1}");
    // An inexact object type's bare `...` (`{a: T, ...}`), never a spread.
    INEXACT
        .get_or_init(|| Regex::new(&format!(r"\.\.\.({JS_SPACE}*[}},])")).expect("inexact object"))
        .replace_all(&code, "   ${1}")
        .into_owned()
}

fn has_direct_child_kind(node: Node<'_>, kind: &str) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|child| child.kind() == kind))
}

pub(crate) fn class_field_is_callable(node: Node<'_>, field_kind: &str) -> bool {
    if node.kind() != field_kind {
        return true;
    }
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i as u32) else {
            continue;
        };
        if matches!(
            child.kind(),
            "arrow_function" | "function_expression" | "generator_function"
        ) {
            return true;
        }
        if child.kind() == "call_expression" {
            if let Some(args) = child_by_field(child, "arguments") {
                for j in 0..args.named_child_count() {
                    if let Some(arg) = args.named_child(j as u32) {
                        if matches!(
                            arg.kind(),
                            "arrow_function" | "function_expression" | "generator_function"
                        ) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::blank_flow_syntax;

    fn blank(code: &str) -> String {
        let source = format!("// @flow\n{code}");
        let out = blank_flow_syntax(&source, "a.js");
        assert_eq!(out.len(), source.len(), "{code:?}");
        out.strip_prefix("// @flow\n")
            .expect("pragma line")
            .to_string()
    }

    #[test]
    fn flow_only_syntax_becomes_spaces() {
        assert_eq!(blank("type A = {| a: T |};"), "type A = {  a: T  };");
        assert_eq!(
            blank("let x: ?string = f(y, ?z);"),
            "let x:  string = f(y,  z);"
        );
        assert_eq!(blank("type L = Array<?T>;"), "type L = Array< T>;");
        assert_eq!(blank("f: ?(e: E) => void"), "f:  (e: E) => void");
        assert_eq!(blank("type O = {a: T, ...};"), "type O = {a: T,    };");
        assert_eq!(
            blank("import typeof * as R from 'r';"),
            "import        * as R from 'r';"
        );
        assert_eq!(
            blank("export opaque type T = string;"),
            "export        type T = string;"
        );
    }

    #[test]
    fn javascript_that_only_looks_like_flow_stays() {
        for code in [
            "const b = a ? c : d;",
            "const v = a?.b ?? c;",
            "const o = {...rest};",
            "f(...args, [...xs]);",
            "const opaque = 1;",
            "opaque.type = 2;",
            "importtypeof(x);",
            "import typeofx from 'y';",
        ] {
            assert_eq!(blank(code), code);
        }
    }

    #[test]
    fn blanking_keeps_every_byte_offset_and_line() {
        // A no-break space is two bytes; it becomes two spaces.
        assert_eq!(
            blank("import\u{a0}typeof R from 'r';"),
            "import         R from 'r';"
        );
        assert_eq!(
            blank("import\ntypeof R from 'r';"),
            "import\n       R from 'r';"
        );
        assert_eq!(blank("x:\n  ?T"), "x:\n   T");
    }

    #[test]
    fn only_flow_typed_javascript_paths_are_blanked() {
        let flow = "// @flow\ntype A = {| a: ?T |};";
        for path in ["a.js", "a.jsx", "a.mjs", "a.cjs"] {
            assert_ne!(blank_flow_syntax(flow, path), flow, "{path}");
        }
        for path in ["a.ts", "a.tsx", "a.JS", "a.vue"] {
            assert_eq!(blank_flow_syntax(flow, path), flow, "{path}");
        }
        let plain = "type A = {| a: ?T |};";
        assert_eq!(blank_flow_syntax(plain, "a.js"), plain);
    }
}
