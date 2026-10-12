//! Java `LanguageSpec`.
//!
//! Direct port of `upstream extraction/languages/java.ts:5-106`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct JavaSpec;

pub static JAVA_SPEC: JavaSpec = JavaSpec;

impl LanguageSpec for JavaSpec {
    fn language(&self) -> Language {
        Language::Java
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_java::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["method_declaration", "constructor_declaration"]
    }
    fn interface_types(&self) -> &'static [&'static str] {
        &["interface_declaration", "annotation_type_declaration"]
    }
    fn struct_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn enum_types(&self) -> &'static [&'static str] {
        &["enum_declaration"]
    }
    fn enum_member_types(&self) -> &'static [&'static str] {
        &["enum_constant"]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["import_declaration"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["method_invocation"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["local_variable_declaration"]
    }
    fn field_types(&self) -> &'static [&'static str] {
        &["field_declaration"]
    }
    fn package_types(&self) -> &'static [&'static str] {
        &["package_declaration"]
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
        "type"
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let params_text = node_text(params, source);
        Some(match child_by_field(node, "type") {
            Some(return_type) => format!("{} {params_text}", node_text(return_type, source)),
            None => params_text,
        })
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        normalize_java_type(child_by_field(node, "type")?, source)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        modifier_kinds(node).and_then(|kinds| {
            if kinds.iter().any(|kind| *kind == "public") {
                Some("public".to_string())
            } else if kinds.iter().any(|kind| *kind == "private") {
                Some("private".to_string())
            } else if kinds.iter().any(|kind| *kind == "protected") {
                Some("protected".to_string())
            } else {
                None
            }
        })
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        modifier_kinds(node).is_some_and(|kinds| kinds.iter().any(|kind| *kind == "static"))
    }

    /// A `static final` field is a constant (upstream #897); an instance or
    /// `final`-only field stays a field.
    fn is_const(&self, node: Node<'_>) -> bool {
        modifier_kinds(node)
            .is_some_and(|kinds| kinds.contains(&"static") && kinds.contains(&"final"))
    }

    fn extract_package(&self, node: Node<'_>, source: &str) -> Option<String> {
        node.named_children(&mut node.walk())
            .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
            .map(|id| node_text(id, source).trim().to_string())
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let import_text = node_text(node, source).trim().to_string();
        let scoped = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "scoped_identifier")?;
        Some(ImportInfo {
            module_name: node_text(scoped, source),
            signature: import_text,
            handled_refs: false,
        })
    }
}

/// A Java type's class name as a receiver type (upstream
/// `normalizeJavaType`): `List` of `java.util.List<Foo>`; none for a
/// primitive, `void` or an array.
pub(crate) fn normalize_java_type(type_node: Node<'_>, source: &str) -> Option<String> {
    if matches!(
        type_node.kind(),
        "void_type" | "integral_type" | "floating_point_type" | "boolean_type" | "array_type"
    ) {
        return None;
    }
    let raw = strip_angle_args(&node_text(type_node, source));
    let last = raw.split('.').next_back()?.trim();
    valid_ident(last).then(|| last.to_string())
}

/// The Lombok logging annotations; each generates a field named `log`
/// (upstream #912 `LOMBOK_LOG_ANNOTATIONS`).
pub(crate) const LOMBOK_LOG_ANNOTATIONS: [&str; 9] = [
    "Slf4j",
    "Log4j",
    "Log4j2",
    "Log",
    "CommonsLog",
    "JBossLog",
    "Flogger",
    "XSlf4j",
    "CustomLog",
];

/// The simple names of the annotations in a declaration's `modifiers`, in
/// source order and each once (`@lombok.Getter` is `Getter`; upstream #912
/// `lombokAnnotationNames`).
pub(crate) fn lombok_annotation_names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Some(modifiers) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "modifiers")
    else {
        return names;
    };
    for child in modifiers.named_children(&mut modifiers.walk()) {
        if !matches!(child.kind(), "marker_annotation" | "annotation") {
            continue;
        }
        let Some(name) = child_by_field(child, "name") else {
            continue;
        };
        let text = node_text(name, source);
        let simple = text.trim().rsplit('.').next().unwrap_or_default();
        if !simple.is_empty() && !names.iter().any(|seen| seen == simple) {
            names.push(simple.to_string());
        }
    }
    names
}

/// The text of a declaration's `modifiers`, annotations included (upstream
/// `modifierTextOf`).
pub(crate) fn modifier_text(node: Node<'_>, source: &str) -> String {
    node.named_children(&mut node.walk())
        .find(|child| child.kind() == "modifiers")
        .map(|modifiers| node_text(modifiers, source))
        .unwrap_or_default()
}

/// Whether `word` appears in `text` between non-word characters (a regex
/// `\b<word>\b`).
pub(crate) fn has_word(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(is_word)
            && !text[at + word.len()..].chars().next().is_some_and(is_word)
    })
}

/// `name` with its first character upper-cased as JavaScript's
/// `charAt(0).toUpperCase()` does (a character outside the BMP is a lone
/// surrogate there, and stays).
fn capitalize_java(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.len_utf16() == 1 => first.to_uppercase().chain(chars).collect(),
        _ => name.to_string(),
    }
}

/// Whether `name` starts `is` and an ASCII capital (`/^is[A-Z]/`).
fn is_prefixed_boolean_name(name: &str) -> bool {
    name.strip_prefix("is")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// Lombok's getter name: `getX`, or `isX` for a primitive boolean, which keeps
/// a field name already `isFoo` (upstream `lombokGetterName`).
pub(crate) fn lombok_getter_name(field: &str, boolean_primitive: bool) -> String {
    if boolean_primitive {
        if is_prefixed_boolean_name(field) {
            field.to_string()
        } else {
            format!("is{}", capitalize_java(field))
        }
    } else {
        format!("get{}", capitalize_java(field))
    }
}

/// Lombok's setter name: `setX`; a primitive boolean `isFoo` sets through
/// `setFoo` (upstream `lombokSetterName`).
pub(crate) fn lombok_setter_name(field: &str, boolean_primitive: bool) -> String {
    let base = if boolean_primitive && is_prefixed_boolean_name(field) {
        &field[2..]
    } else {
        field
    };
    format!("set{}", capitalize_java(base))
}

fn modifier_kinds(node: Node<'_>) -> Option<Vec<&str>> {
    let modifiers = node
        .children(&mut node.walk())
        .find(|child| child.kind() == "modifiers")?;
    Some(
        modifiers
            .children(&mut modifiers.walk())
            .map(|child| child.kind())
            .collect(),
    )
}

fn strip_angle_args(input: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for ch in input.trim().chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

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
    fn lombok_accessor_names_follow_lombok() {
        assert_eq!(lombok_getter_name("name", false), "getName");
        assert_eq!(lombok_getter_name("active", true), "isActive");
        // A primitive boolean already named `isFoo` keeps its name, and sets
        // through `setFoo`; a boxed one or another type does not.
        assert_eq!(lombok_getter_name("isRunning", true), "isRunning");
        assert_eq!(lombok_setter_name("isRunning", true), "setRunning");
        assert_eq!(lombok_getter_name("isRunning", false), "getIsRunning");
        assert_eq!(lombok_setter_name("isRunning", false), "setIsRunning");
        assert_eq!(lombok_getter_name("island", true), "isIsland");
        assert_eq!(lombok_setter_name("island", true), "setIsland");
        assert_eq!(lombok_getter_name("x", false), "getX");
        assert_eq!(lombok_getter_name("_id", false), "get_id");
        assert_eq!(lombok_getter_name("état", false), "getÉtat");
    }

    #[test]
    fn has_word_matches_whole_words_only() {
        assert!(has_word("private static final", "static"));
        assert!(has_word("@Getter final", "final"));
        assert!(!has_word("private finalize", "final"));
        assert!(!has_word("staticky", "static"));
        assert!(!has_word("non_static", "static"));
        assert!(has_word("@Value(\"static\")", "static"));
    }
}
