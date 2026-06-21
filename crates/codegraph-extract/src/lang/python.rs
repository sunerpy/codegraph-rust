use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct PythonSpec;

pub static PYTHON_SPEC: PythonSpec = PythonSpec;

impl LanguageSpec for PythonSpec {
    fn language(&self) -> Language {
        Language::Python
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_python::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &'static [&'static str] {
        &["class_definition"]
    }
    fn method_types(&self) -> &'static [&'static str] {
        &["function_definition"]
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
        &[]
    }
    fn type_alias_types(&self) -> &'static [&'static str] {
        &[]
    }
    fn import_types(&self) -> &'static [&'static str] {
        &["import_statement", "import_from_statement"]
    }
    fn call_types(&self) -> &'static [&'static str] {
        &["call"]
    }
    fn variable_types(&self) -> &'static [&'static str] {
        &["assignment"]
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

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        let mut signature = node_text(params, source);
        if let Some(return_type) = child_by_field(node, "return_type") {
            signature.push_str(" -> ");
            signature.push_str(&node_text(return_type, source));
        }
        Some(signature)
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        node.prev_sibling()
            .is_some_and(|prev| prev.kind() == "async")
    }

    fn is_static(&self, node: Node<'_>, source: &str) -> bool {
        node.prev_named_sibling().is_some_and(|prev| {
            prev.kind() == "decorator" && node_text(prev, source).contains("staticmethod")
        })
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        if node.kind() != "import_from_statement" {
            return None;
        }
        let module_node = child_by_field(node, "module_name")?;
        Some(ImportInfo {
            module_name: node_text(module_node, source),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
}
