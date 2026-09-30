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

    fn body_docstring(&self, node: Node<'_>, source: &str) -> Option<String> {
        python_body_docstring(node, source)
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

/// Python states intent in a docstring — a bare string literal as the first
/// statement of a module, class or function body (upstream #1905). Reads the
/// grammar's `string_content`, so `r`/`u` prefixes and both triple-quote forms
/// work; bytes and f-strings are not docstrings, and a tuple is not a string.
fn python_body_docstring(node: Node<'_>, source: &str) -> Option<String> {
    let body = if node.kind() == "module" {
        node
    } else {
        child_by_field(node, "body")?
    };
    let first = body
        .named_children(&mut body.walk())
        .find(|child| child.kind() != "comment")?;
    if first.kind() != "expression_statement"
        || first.named_child_count() != 1
        || first
            .children(&mut first.walk())
            .any(|child| child.kind() == ",")
    {
        return None;
    }
    let mut literal = first.named_child(0)?;
    while literal.kind() == "parenthesized_expression" {
        literal = literal
            .named_children(&mut literal.walk())
            .find(|child| child.kind() != "comment")?;
    }
    let strings = if literal.kind() == "concatenated_string" {
        literal
            .named_children(&mut literal.walk())
            .filter(|child| child.kind() != "comment")
            .collect::<Vec<_>>()
    } else {
        vec![literal]
    };
    let mut raw = String::new();
    for string in strings {
        if string.kind() != "string" {
            return None;
        }
        let parts = string
            .named_children(&mut string.walk())
            .collect::<Vec<_>>();
        let start = parts.iter().find(|part| part.kind() == "string_start")?;
        if node_text(*start, source)
            .chars()
            .any(|ch| matches!(ch, 'b' | 'B' | 'f' | 'F'))
            || !parts.iter().any(|part| part.kind() == "string_end")
        {
            return None;
        }
        if let Some(content) = parts.iter().find(|part| part.kind() == "string_content") {
            raw.push_str(&node_text(*content, source));
        }
    }
    let docstring = dedent_docstring(&raw);
    (!docstring.is_empty()).then_some(docstring)
}

/// PEP 257 cleaning: tabs expand to 8-column stops, every line after the first
/// loses the common indentation of the non-blank ones (the first line starts
/// right after the quotes), and blank edges are dropped.
fn dedent_docstring(raw: &str) -> String {
    let lines = raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| {
            let mut column = 0usize;
            let mut expanded = String::with_capacity(line.len());
            for ch in line.chars() {
                if ch == '\t' {
                    let width = 8 - column % 8;
                    expanded.extend(std::iter::repeat_n(' ', width));
                    column += width;
                } else {
                    expanded.push(ch);
                    column += 1;
                }
            }
            expanded
        })
        .collect::<Vec<_>>();
    let leading = |line: &str| line.chars().take_while(|ch| ch.is_whitespace()).count();
    let indent = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| leading(line))
        .min()
        .unwrap_or(0);
    let mut out = Vec::with_capacity(lines.len());
    out.push(
        lines
            .first()
            .map(|line| line.trim().to_string())
            .unwrap_or_default(),
    );
    for line in lines.iter().skip(1) {
        out.push(
            line.chars()
                .skip(indent)
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
    }
    while out.first().is_some_and(|line| line.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|line| line.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod docstring_tests {
    use super::dedent_docstring;

    #[test]
    fn dedent_follows_pep_257() {
        assert_eq!(
            dedent_docstring("Summary line.\n\n    Details here.\n      indented\n    "),
            "Summary line.\n\nDetails here.\n  indented"
        );
        assert_eq!(dedent_docstring("\n    Only body.\n    "), "Only body.");
        assert_eq!(dedent_docstring("a\r\n\tb"), "a\nb");
        assert_eq!(dedent_docstring("   "), "");
    }
}
