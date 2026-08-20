//! C `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:98-142`.

use std::collections::BTreeSet;
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
    fn union_types(&self) -> &'static [&'static str] {
        &["union_specifier"]
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
            if child.kind() == "union_specifier" && child_by_field(child, "body").is_some() {
                return Some(NodeKind::Union);
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

/// Blank an attribute macro sitting in front of a C function definition's return
/// type: `SEC_ATTR UINT32 LostName(VOID) { … }` where `SEC_ATTR` is a macro
/// wrapping `__attribute__((…))` (common in embedded/kernel/firmware C).
/// tree-sitter's C grammar reads the macro as the declaration's type and the real
/// return type as the declarator, so the function indexes under the RETURN TYPE's
/// name (`UINT32`) or, in other spacings, under its parameter list — the real name
/// is lost and the symbol is unfindable (#1311).
///
/// # Why the structural heuristic alone is WRONG
///
/// The obvious rule — "line-leading ALL-CAPS token, then two identifiers, then
/// `(`" — is purely lexical, and the shape `MACRO Ret name(` is INDISTINGUISHABLE
/// from `Ret CALLCONV name(` and from `KEYWORD_ALIAS Ret name(`. Measured on real
/// firmware-flavoured C, the permissive form DAMAGED correct extraction:
///
/// | source                                 | correct      | permissive rule |
/// | -------------------------------------- | ------------ | --------------- |
/// | `EFI_STATUS EFIAPI DriverEntry (VOID)` | `EFI_STATUS` | `EFIAPI` ✗       |
/// | `STATIC void helper (void)`             | `STATIC`     | dropped ✗       |
/// | `CONST CHAR8 *GetName (VOID)`           | `CONST`      | `CHAR8`         |
///
/// `EFI_STATUS` / `UINT32` / `CHAR8` are typedef'd RETURN TYPES and `STATIC` /
/// `EXTERN` / `INLINE` / `CONST` are macro aliases for keywords; none is an
/// attribute macro, yet all are all-caps identifiers of ≥3 chars in declaration
/// position. No all-caps-plus-length heuristic can separate them, so the token's
/// spelling is not admissible evidence.
///
/// # The rule actually applied
///
/// Blank ONLY when the SAME translation unit proves the token is attribute-like:
/// it has an object-like `#define TOKEN …` in this file whose replacement text is
/// either EMPTY or contains an attribute construct (`__attribute__`, `__attribute`,
/// `__declspec`, `__asm`, `__pragma`, `_Pragma`). Every other leading
/// token — typedef'd return type, keyword alias, calling convention, or a macro
/// defined in a header this pass cannot see — is left untouched, so the parse is
/// byte-identical to not having this pass at all.
///
/// Function-like `#define TOKEN(x) …` is deliberately NOT accepted: those are used
/// as `TOKEN(arg) …`, not as a bare leading token, and the `(`-suffixed name would
/// be a different lexeme anyway.
///
/// # Documented limitation
///
/// `pre_parse` sees one file, so a macro `#define SEC_ATTR __attribute__((…))`
/// living in a HEADER and used in a `.c` yields NO evidence and the source is left
/// untouched — #1311's symptom persists for that (very common) layout. That is
/// deliberate: a cross-file macro table would be non-deterministic with respect to
/// include order, and guessing from spelling is what produced the regression
/// above. Under-fixing is recoverable; corrupting ordinary C is not.
///
/// Blanking replaces the macro with equal-length ASCII spaces, so every byte
/// offset — and therefore every line/column and every node ID — is preserved,
/// exactly like the C++ blanks in `lang/cpp.rs`.
fn blank_c_leading_attr_macros(source: &str) -> String {
    let attr_macros = attribute_like_defines(source);
    if attr_macros.is_empty() {
        return source.to_string();
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*([A-Za-z_]\w*)\s+[A-Za-z_]\w*[\s*]+[A-Za-z_]\w*\s*\(")
            .expect("c-leading-attr-macro regex")
    });
    let spans: Vec<(usize, usize)> = re
        .captures_iter(source)
        .filter_map(|caps| caps.get(1))
        .filter(|m| attr_macros.contains(m.as_str()))
        .map(|m| (m.start(), m.end()))
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

/// Attribute constructs whose presence in a `#define`'s replacement text proves
/// the macro is an attribute wrapper rather than a type or keyword alias.
const C_ATTRIBUTE_CONSTRUCTS: [&str; 6] = [
    "__attribute__",
    "__attribute",
    "__declspec",
    "__asm",
    "__pragma",
    "_Pragma",
];

/// Collect the object-like `#define`s in THIS file whose replacement text is empty
/// or contains an attribute construct — i.e. the tokens that are provably safe to
/// blank in declaration position. A `BTreeSet` keeps membership order-independent
/// so extraction stays deterministic.
fn attribute_like_defines(source: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    if !source.contains("#define") {
        return names;
    }
    for line in source.lines() {
        let rest = match line.trim_start().strip_prefix('#') {
            Some(rest) => rest.trim_start(),
            None => continue,
        };
        let rest = match rest.strip_prefix("define") {
            Some(rest) => rest,
            None => continue,
        };
        // `#defineFOO` is not a define; a separator is required.
        if !rest.starts_with([' ', '\t']) {
            continue;
        }
        let rest = rest.trim_start();
        let name_end = rest
            .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .unwrap_or(rest.len());
        let (name, replacement) = rest.split_at(name_end);
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        // Function-like macro (`#define F(x) …`) — never used as a bare leading
        // token, so it is not a candidate.
        if replacement.starts_with('(') {
            continue;
        }
        let replacement = replacement.trim();
        if replacement.is_empty()
            || C_ATTRIBUTE_CONSTRUCTS
                .iter()
                .any(|marker| replacement.contains(marker))
        {
            names.insert(name);
        }
    }
    names
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

    const ATTR_DEFINE: &str = "#define SEC_ATTR __attribute__((section(\".init\")))\n";

    #[test]
    fn blank_c_leading_attr_macro_blanks_a_same_file_proved_macro() {
        let src = format!("{ATTR_DEFINE}SEC_ATTR UINT32 f(void) {{}}");
        let out = blank_c_leading_attr_macros(&src);
        assert_eq!(out, format!("{ATTR_DEFINE}         UINT32 f(void) {{}}"));
        assert_eq!(out.len(), src.len());
    }

    #[test]
    fn blank_c_leading_attr_macro_accepts_an_empty_define() {
        let src = "#define SEC_ATTR\nSEC_ATTR UINT32 f(void) {}";
        assert_eq!(
            blank_c_leading_attr_macros(src),
            "#define SEC_ATTR\n         UINT32 f(void) {}"
        );
    }

    #[test]
    fn blank_c_leading_attr_macro_accepts_declspec_and_asm_defines() {
        for define in ["#define DLL_A __declspec(dllexport)", "#define NAKED __asm"] {
            let name = define.split_whitespace().nth(1).expect("macro name");
            let src = format!("{define}\n{name} UINT32 f(void) {{}}");
            let out = blank_c_leading_attr_macros(&src);
            assert!(
                out.lines().nth(1).expect("body line").starts_with("     "),
                "{define} should have proved `{name}` blankable, got: {out}"
            );
            assert_eq!(out.len(), src.len());
        }
    }

    #[test]
    fn blank_c_leading_attr_macro_is_offset_preserving() {
        let src =
            format!("{ATTR_DEFINE}SEC_ATTR\nUINT32 f(void) {{}}\nSEC_ATTR VOID g(VOID) {{}}\n");
        let out = blank_c_leading_attr_macros(&src);
        assert_eq!(out.len(), src.len());
        assert_eq!(
            out.bytes().filter(|&b| b == b'\n').count(),
            src.bytes().filter(|&b| b == b'\n').count()
        );
    }

    #[test]
    fn blank_c_leading_attr_macro_handles_pointer_returns() {
        let src = format!("{ATTR_DEFINE}SEC_ATTR UINT32 *f(void) {{}}");
        assert_eq!(
            blank_c_leading_attr_macros(&src),
            format!("{ATTR_DEFINE}         UINT32 *f(void) {{}}")
        );
    }

    #[test]
    fn blank_c_leading_attr_macro_leaves_unproved_leading_tokens_untouched() {
        for src in [
            "SEC_ATTR UINT32 f(void) {}",
            "UINT32 helper(void) {}",
            "MY_ASSERT(x);",
            ATTR_DEFINE,
            "SEC_ATTR unsigned int f(void) {}",
            "x = SEC_ATTR UINT32 y(z);",
        ] {
            assert_eq!(blank_c_leading_attr_macros(src), src, "changed: {src}");
        }
    }

    // The rows that the earlier ALL-CAPS-only rule DAMAGED: a typedef'd return type
    // in front of a calling-convention macro, and keyword-alias macros. None has an
    // attribute-like `#define`, so none may be touched even though `SEC_ATTR`'s
    // define makes the pass active in the same file.
    #[test]
    fn blank_c_leading_attr_macro_leaves_return_types_and_keyword_aliases_untouched() {
        for line in [
            "EFI_STATUS EFIAPI DriverEntry (VOID) {}",
            "CONST CHAR8 *GetName (VOID) {}",
            "STATIC void helper (void) {}",
            "UINT32 Untouched (void) {}",
        ] {
            let src = format!("{ATTR_DEFINE}{line}\n");
            assert_eq!(
                blank_c_leading_attr_macros(&src),
                src,
                "must stay untouched: {line}"
            );
        }
    }

    #[test]
    fn attribute_like_defines_rejects_types_keywords_and_function_like_macros() {
        let src = concat!(
            "#define VOID void\n",
            "#define STATIC static\n",
            "#define CONST const\n",
            "#define EFIAPI\n",
            "#define CHECK(x) __attribute__((unused)) x\n",
            "#define SEC_ATTR __attribute__((section(\".init\")))\n"
        );
        let names: Vec<&str> = attribute_like_defines(src).into_iter().collect();
        assert_eq!(names, ["EFIAPI", "SEC_ATTR"]);
    }
}
