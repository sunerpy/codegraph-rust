//! C `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:98-142`.

use std::sync::OnceLock;

use codegraph_core::types::{Language, NodeKind};
use regex::Regex;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct CSpec;

pub static C_SPEC: CSpec = CSpec;

impl LanguageSpec for CSpec {
    fn language(&self) -> Language {
        Language::C
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_c::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &["struct_specifier"]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_specifier"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enumerator"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &["type_definition"]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["preproc_include"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["declaration"]
    }
    fn name_field(&self) -> &'static str {
        "declarator"
    }
    fn body_field(&self) -> &'static str {
        "body"
    }
    fn params_field(&self) -> &'static str {
        "parameters"
    }
    fn return_field(&self) -> &'static str {
        "type"
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        normalize_c_return_type(&node_text(child_by_field(node, "type")?, source))
    }
    fn resolve_type_alias_kind(&self, node: Node<'_>, _source: &str) -> Option<NodeKind> {
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "enum_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Enum);
            }
            if child.kind() == "struct_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Struct);
            }
        }
        None
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        include_import(node, source)
    }
    fn pre_parse(&self, source: &str, _file_path: &str) -> String {
        let blanked = blank_c_leading_attr_macros(source);
        if crate::lang::cpp::looks_like_cuda_source(&blanked) {
            crate::lang::cpp::blank_cuda_constructs_str(&blanked)
        } else {
            blanked
        }
    }
}

/// Blank an unknown attribute macro sitting in front of a C function
/// definition's return type: `SEC_ATTR UINT32 LostName(VOID) { … }` (a macro
/// wrapping `__attribute__((…))`, common in embedded/kernel/firmware C).
/// tree-sitter's C grammar reads the macro as the declaration's type and the real
/// return type as the declarator, so the function indexes under the RETURN TYPE's
/// name (`UINT32`) or, in other spacings, under its parameter list — the real name
/// is lost and the symbol is unfindable (#1311).
///
/// Attribute macros are project-specific, so this keys on STRUCTURE, not a
/// curated list, matched tightly: a line-leading (declaration-position) ALL-CAPS
/// token of ≥3 chars, followed by TWO identifier tokens (return type, then name;
/// `*` allowed between them for pointer returns) and then `(` — i.e. exactly the
/// `MACRO Ret name(` definition shape. `MACRO name(` calls, `#define` lines (they
/// start with `#`, which `^[ \t]*` cannot skip), multi-word builtin returns
/// (`MACRO unsigned int f(`, where the grammar already keeps the name), and
/// mid-line uses are all rejected by construction.
///
/// Blanking replaces the macro with equal-length ASCII spaces, so every byte
/// offset — and therefore every line/column — is preserved, exactly like the C++
/// blanks in `lang/cpp.rs`.
fn blank_c_leading_attr_macros(source: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*([A-Z][A-Z0-9_]{2,})\s+[A-Za-z_]\w*[\s*]+[A-Za-z_]\w*\s*\(")
            .expect("c-leading-attr-macro regex")
    });
    let spans: Vec<(usize, usize)> = re
        .captures_iter(source)
        .filter_map(|caps| caps.get(1).map(|m| (m.start(), m.end())))
        .collect();
    if spans.is_empty() {
        return source.to_string();
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end) in spans {
        for b in bytes.iter_mut().take(end).skip(start) {
            *b = b' ';
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_string())
}

pub(crate) fn include_import(node: Node<'_>, source: &str) -> Option<ImportInfo> {
    let signature = node_text(node, source).trim().to_string();
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == "system_lib_string" {
            return Some(ImportInfo {
                module_name: node_text(child, source)
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string(),
                signature,
                handled_refs: false,
            });
        }
        if child.kind() == "string_literal" {
            if let Some(content) = child
                .named_children(&mut child.walk())
                .find(|c| c.kind() == "string_content")
            {
                return Some(ImportInfo {
                    module_name: node_text(content, source),
                    signature,
                    handled_refs: false,
                });
            }
        }
    }
    None
}

pub(crate) fn normalize_c_return_type(raw: &str) -> Option<String> {
    let mut text = raw.trim().to_string();
    for wrapper in ["unique_ptr", "shared_ptr", "weak_ptr", "optional"] {
        if let Some(start) = text.find(&format!("{wrapper}<")) {
            let inner_start = start + wrapper.len() + 1;
            if let Some(end) = text[inner_start..].find(['>', ',']) {
                text = text[inner_start..inner_start + end].to_string();
            }
        }
    }
    let cleaned = text
        .replace('*', " ")
        .replace('&', " ")
        .split_whitespace()
        .filter(|part| {
            !matches!(
                *part,
                "const" | "volatile" | "typename" | "struct" | "class" | "enum"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let last = cleaned.rsplit("::").next()?.trim();
    if last.is_empty() || CPP_NON_CLASS_RETURN.contains(&last) || !valid_ident(last) {
        return None;
    }
    Some(last.to_string())
}

const CPP_NON_CLASS_RETURN: [&str; 28] = [
    "void",
    "bool",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "unsigned",
    "signed",
    "size_t",
    "ssize_t",
    "auto",
    "wchar_t",
    "char8_t",
    "char16_t",
    "char32_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "intptr_t",
    "uintptr_t",
    "nullptr_t",
];

fn valid_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_header_cuda_content_blanked() {
        let out = CSpec.pre_parse("__device__ int helper() { return 0; }", "h.h");
        assert!(!out.contains("__device__"));
        assert_eq!(out.len(), "__device__ int helper() { return 0; }".len());
    }

    #[test]
    fn c_plain_untouched() {
        let src = "int add(int a, int b) { return a + b; }";
        assert_eq!(CSpec.pre_parse(src, "m.c"), src);
    }

    #[test]
    fn blank_c_leading_attr_macro_blanks_the_definition_shape() {
        let src = "SEC_ATTR UINT32 f(void) {}";
        let out = blank_c_leading_attr_macros(src);
        assert_eq!(out, "         UINT32 f(void) {}");
        assert_eq!(out.len(), src.len());
    }

    #[test]
    fn blank_c_leading_attr_macro_is_offset_preserving() {
        let src = "SEC_ATTR\nUINT32 f(void) {}\nSEC_ATTR VOID g(VOID) {}\n";
        let out = blank_c_leading_attr_macros(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(
            out.bytes().filter(|&b| b == b'\n').count(),
            src.bytes().filter(|&b| b == b'\n').count()
        );
    }

    #[test]
    fn blank_c_leading_attr_macro_leaves_other_shapes_untouched() {
        for src in [
            "UINT32 helper(void) {}",
            "MY_ASSERT(x);",
            "#define SEC_ATTR __attribute__((section(\".init\")))",
            "SEC_ATTR unsigned int f(void) {}",
            "x = SEC_ATTR UINT32 y(z);",
        ] {
            assert_eq!(blank_c_leading_attr_macros(src), src, "changed: {src}");
        }
    }

    #[test]
    fn blank_c_leading_attr_macro_handles_pointer_returns() {
        let src = "SEC_ATTR UINT32 *f(void) {}";
        assert_eq!(
            blank_c_leading_attr_macros(src),
            "         UINT32 *f(void) {}"
        );
    }
}
