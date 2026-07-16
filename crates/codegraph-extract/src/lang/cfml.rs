//! CFML / ColdFusion (`.cfc` / `.cfm` / `.cfs`) `LanguageSpec`.
//!
//! Extraction-tier port of `upstream extraction/cfml-extractor.ts` +
//! `languages/cfscript.ts` (commit `816bacb`, #1153 — the extraction slice
//! only, scope B). CFML is DUAL-GRAMMAR: the `tree-sitter-cfml` crate bundles a
//! `cfscript` grammar (modern script style) and a `cfml` tag grammar. A file's
//! dialect is chosen by a first-token sniff (`is_bare_script_cfml`): a `<`
//! first token is a tag file, anything else is script. [`CfmlSpec`] wires the
//! cfscript grammar as the DEFAULT and overrides
//! [`LanguageSpec::tree_sitter_language_for_source`] to hand back the cfml tag
//! grammar for tag files, so a `.cfc`/`.cfm`/`.cfs` file is parsed with EXACTLY
//! one grammar.
//!
//! Bare-script files drive the generic type-set dispatch (`component` → Class,
//! `function_declaration` → function/method, `property_declaration` → Property,
//! `call_expression` → Calls, `import_statement`/`include_statement` → Import);
//! the `Language::Cfml`-guarded `visit_cfml_node` walker extension owns the
//! script `component` (file-name rename + script-style `extends`) and the tag
//! grammar's `cf_component_open_tag`/`cf_function_tag`.
//!
//! DEFERRED (recorded follow-ups, consistent with H1–H5): the
//! `<cfscript>`-in-tag-body re-parse delegation, the `cfquery` SQL-body
//! extraction (`LANGUAGE_CFQUERY`), and the CFML framework RESOLVER bridges.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct CfmlSpec;

pub static CFML_SPEC: CfmlSpec = CfmlSpec;

impl LanguageSpec for CfmlSpec {
    fn language(&self) -> Language {
        Language::Cfml
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_cfml::LANGUAGE_CFSCRIPT.into()
    }

    fn tree_sitter_language_for_source(&self, source: &str) -> TsLanguage {
        if is_bare_script_cfml(source) {
            tree_sitter_cfml::LANGUAGE_CFSCRIPT.into()
        } else {
            tree_sitter_cfml::LANGUAGE_CFML.into()
        }
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[
            "function_declaration",
            "function_expression",
            "arrow_function",
        ]
    }

    fn class_types(&self) -> &'static [&'static str] {
        // The script `component`/`interface` is handled directly by
        // `visit_cfml_node` (file-name rename + script-style extends), which
        // short-circuits this dispatch; declared for spec self-description.
        &["component"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &["function_declaration", "method_definition"]
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
        &["import_statement", "include_statement"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &["variable_declaration"]
    }

    fn property_types(&self) -> &'static [&'static str] {
        &["property_declaration"]
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
        ""
    }

    fn classify_class_node(&self, node: Node<'_>) -> codegraph_core::types::NodeKind {
        if node.child(0).map(|c| c.kind()) == Some("interface") {
            codegraph_core::types::NodeKind::Interface
        } else {
            codegraph_core::types::NodeKind::Class
        }
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let params = child_by_field(node, "parameters")?;
        Some(node_text(params, source))
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        let access = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "access_type")?;
        let keyword = access.child(0).map(|c| c.kind()).unwrap_or("");
        let visibility = match keyword {
            "private" => "private",
            "package" => "internal",
            "remote" | "public" => "public",
            _ => return None,
        };
        Some(visibility.to_string())
    }

    fn extract_property_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        cfml_property_name(node, source)
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let module_name = if node.kind() == "include_statement" {
            let string = node
                .named_children(&mut node.walk())
                .find(|c| c.kind() == "string")?;
            cfml_unquote_string(string, source)
        } else {
            let source_node = child_by_field(node, "source")?;
            match source_node.kind() {
                "import_path" => {
                    let parts: Vec<String> = source_node
                        .named_children(&mut source_node.walk())
                        .filter(|c| c.kind() == "identifier")
                        .map(|c| node_text(c, source))
                        .collect();
                    parts.join(".")
                }
                "string" => cfml_unquote_string(source_node, source),
                _ => node_text(source_node, source),
            }
        };
        if module_name.is_empty() {
            return None;
        }
        Some(ImportInfo {
            module_name,
            signature: node_text(node, source).trim().chars().take(200).collect(),
            handled_refs: false,
        })
    }
}

/// First-token dialect sniff. Skips a leading UTF-8 BOM, whitespace, and `//`
/// and `/* */` comments to the first real token; returns `true` (bare script)
/// when that token is NOT `<` (tag files open with `<`). Empty / all-trivia
/// source → `true` (a script no-op). Ports `isBareScriptCfml`
/// (cfml-extractor.ts).
pub(crate) fn is_bare_script_cfml(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut i = 0;
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        i = 3;
    }
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'/' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'*' => {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        return b != b'<';
    }
    true
}

/// The component name from a file path: the basename with a `.cfc`/`.cfm`/`.cfs`
/// extension stripped (case-insensitive). Ports `componentNameFromPath`.
pub(crate) fn cfml_component_name_from_path(file_path: &str) -> String {
    let base = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    let lower = base.to_ascii_lowercase();
    for ext in [".cfc", ".cfm", ".cfs"] {
        if lower.ends_with(ext) {
            return base[..base.len() - ext.len()].to_string();
        }
    }
    base.to_string()
}

/// A tag attribute value by name (case-insensitive), for the tag grammar. Scans
/// the tag's `cf_attribute` children — directly (`cf_function_tag`) and inside
/// each `cf_tag_attributes` wrapper (`cf_component_open_tag`) — matching the
/// `cf_attribute_name` leaf, and reads the value from the
/// `quoted_cf_attribute_value` / `cf_attribute_value`'s inner `attribute_value`.
/// Ports `tagAttr`.
pub(crate) fn cfml_tag_attr(tag: Node<'_>, name: &str, source: &str) -> Option<String> {
    fn attr_value(attr: Node<'_>, source: &str) -> Option<String> {
        let value = attr
            .named_children(&mut attr.walk())
            .find(|c| matches!(c.kind(), "quoted_cf_attribute_value" | "cf_attribute_value"))?;
        let inner = value
            .named_children(&mut value.walk())
            .find(|c| c.kind() == "attribute_value")
            .unwrap_or(value);
        Some(node_text(inner, source))
    }
    fn matches_name(attr: Node<'_>, name: &str, source: &str) -> bool {
        attr.named_children(&mut attr.walk())
            .find(|c| c.kind() == "cf_attribute_name")
            .map(|n| node_text(n, source).eq_ignore_ascii_case(name))
            .unwrap_or(false)
    }
    for child in tag.named_children(&mut tag.walk()) {
        match child.kind() {
            "cf_attribute" if matches_name(child, name, source) => {
                return attr_value(child, source);
            }
            "cf_tag_attributes" => {
                for attr in child.named_children(&mut child.walk()) {
                    if attr.kind() == "cf_attribute" && matches_name(attr, name, source) {
                        return attr_value(attr, source);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// A script-style `component_attribute(identifier, string)` value by name
/// (case-insensitive), for the cfscript grammar — used for script-style
/// `extends="Base"`. Matches the `identifier` child and returns the `string`
/// child's value with quotes stripped.
pub(crate) fn cfml_string_attr_value(node: Node<'_>, name: &str, source: &str) -> Option<String> {
    for attr in node.named_children(&mut node.walk()) {
        if attr.kind() != "component_attribute" {
            continue;
        }
        let ident = attr
            .named_children(&mut attr.walk())
            .find(|c| c.kind() == "identifier");
        let Some(ident) = ident else { continue };
        if !node_text(ident, source).eq_ignore_ascii_case(name) {
            continue;
        }
        if let Some(string) = attr
            .named_children(&mut attr.walk())
            .find(|c| c.kind() == "string")
        {
            let value = cfml_unquote_string(string, source);
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// The name of a `property_declaration`. Tries the optional `name` field (the
/// typed form `property String x;`) then the attribute form `property name="x";`
/// whose name lives in a `component_attribute(identifier="name", string)` pair.
pub(crate) fn cfml_property_name(node: Node<'_>, source: &str) -> Option<String> {
    if let Some(name_node) = child_by_field(node, "name") {
        let text = node_text(name_node, source);
        if !text.is_empty() {
            return Some(text);
        }
    }
    cfml_string_attr_value(node, "name", source)
}

/// Strip the surrounding quotes from a cfscript `string` node, reading the
/// `string_fragment` child when present.
pub(crate) fn cfml_unquote_string(string: Node<'_>, source: &str) -> String {
    if let Some(fragment) = string
        .named_children(&mut string.walk())
        .find(|c| c.kind() == "string_fragment")
    {
        return node_text(fragment, source);
    }
    node_text(string, source)
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lang: TsLanguage, src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&lang).unwrap();
        parser.parse(src, None).unwrap()
    }

    fn first_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if node.kind() == kind {
            return Some(node);
        }
        for i in 0..node.named_child_count() as u32 {
            let child = node.named_child(i)?;
            if let Some(found) = first_of_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn cfml_spec_dialect_switch() {
        assert!(is_bare_script_cfml("component {}"));
        assert!(!is_bare_script_cfml("<cfcomponent>"));
        assert!(is_bare_script_cfml("\u{FEFF}// x\ncomponent{}"));
        assert!(is_bare_script_cfml("/* c */ component {}"));
        assert!(is_bare_script_cfml(""));
        assert!(is_bare_script_cfml("   \n  "));
    }

    #[test]
    fn cfml_component_name_from_path_strips_ext() {
        assert_eq!(cfml_component_name_from_path("a/Gadget.cfs"), "Gadget");
        assert_eq!(cfml_component_name_from_path("X.CFC"), "X");
        assert_eq!(cfml_component_name_from_path("dir\\Widget.cfm"), "Widget");
    }

    #[test]
    fn cfml_script_component_parses() {
        let tree = parse(
            tree_sitter_cfml::LANGUAGE_CFSCRIPT.into(),
            "component extends=\"Base\" { }",
        );
        let component = first_of_kind(tree.root_node(), "component").expect("component");
        assert_eq!(
            cfml_string_attr_value(component, "extends", "component extends=\"Base\" { }")
                .as_deref(),
            Some("Base")
        );
    }

    #[test]
    fn cfml_tag_component_parses() {
        let src = "<cfcomponent name=\"W\" extends=\"B\"></cfcomponent>";
        let tree = parse(tree_sitter_cfml::LANGUAGE_CFML.into(), src);
        let open = first_of_kind(tree.root_node(), "cf_component_open_tag").expect("open tag");
        assert_eq!(cfml_tag_attr(open, "name", src).as_deref(), Some("W"));
        assert_eq!(cfml_tag_attr(open, "extends", src).as_deref(), Some("B"));
    }
}
