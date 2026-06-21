//! C++ `LanguageSpec`, ported from `upstream extraction/languages/c-cpp.ts:144-213`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::lang::c::{include_import, normalize_c_return_type};
use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct CppSpec;

pub static CPP_SPEC: CppSpec = CppSpec;

impl LanguageSpec for CppSpec {
    fn language(&self) -> Language {
        Language::Cpp
    }
    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_cpp::LANGUAGE.into()
    }
    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_specifier"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["function_definition"]
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
        &["type_definition", "alias_declaration"]
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
    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        let qid = declarator_qualified_id(child_by_field(node, "declarator")?)?;
        node_text(qid, source)
            .rsplit("::")
            .filter(|part| !part.is_empty())
            .next()
            .map(str::to_string)
    }
    fn get_receiver_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        let qid = declarator_qualified_id(child_by_field(node, "declarator")?)?;
        let parts = node_text(qid, source)
            .split("::")
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        (parts.len() > 1).then(|| parts[..parts.len() - 1].join("::"))
    }
    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        normalize_c_return_type(&node_text(child_by_field(node, "type")?, source))
    }
    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        let parent = node.parent()?;
        for child in parent.children(&mut parent.walk()) {
            if child.kind() == "access_specifier" {
                return Some(child.child(0)?.kind().trim_end_matches(':').to_string());
            }
        }
        None
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
    fn is_misparsed_function(&self, name: &str, _node: Node<'_>, _source: &str) -> bool {
        name.starts_with("namespace")
            || matches!(
                name,
                "switch" | "if" | "for" | "while" | "do" | "case" | "return"
            )
    }
    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        include_import(node, source)
    }
}

fn declarator_qualified_id<'tree>(declarator: Node<'tree>) -> Option<Node<'tree>> {
    let mut queue = vec![declarator];
    while let Some(current) = queue.pop() {
        if current.kind() == "qualified_identifier" {
            return Some(current);
        }
        for child in current.named_children(&mut current.walk()) {
            if !matches!(child.kind(), "parameter_list" | "trailing_return_type") {
                queue.push(child);
            }
        }
    }
    None
}
