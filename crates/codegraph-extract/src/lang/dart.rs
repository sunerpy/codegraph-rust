//! Dart `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/dart.ts:5-358`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct DartSpec;

pub static DART_SPEC: DartSpec = DartSpec;

impl LanguageSpec for DartSpec {
    fn language(&self) -> Language {
        Language::Dart
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_dart::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &["function_signature", "function_declaration"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        &["class_definition", "class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        &[
            "method_signature",
            "method_declaration",
            "constructor_signature",
        ]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        &[]
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
        &["type_alias"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        &["import_or_export"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn extra_class_node_types(&self) -> &'static [&'static str] {
        &[
            "mixin_declaration",
            "extension_declaration",
            "extension_type_declaration",
        ]
    }

    fn name_field(&self) -> &'static str {
        "name"
    }

    fn body_field(&self) -> &'static str {
        "body"
    }

    fn params_field(&self) -> &'static str {
        "formal_parameter_list"
    }

    fn return_field(&self) -> &'static str {
        "type"
    }

    fn resolve_body<'tree>(&self, node: Node<'tree>, body_field: &str) -> Option<Node<'tree>> {
        if matches!(node.kind(), "function_signature" | "method_signature") {
            return node
                .next_named_sibling()
                .filter(|next| next.kind() == "function_body");
        }
        if matches!(node.kind(), "function_declaration" | "method_declaration") {
            return node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "function_body");
        }
        child_by_field(node, body_field).or_else(|| {
            node.named_children(&mut node.walk())
                .find(|child| matches!(child.kind(), "class_body" | "extension_body"))
        })
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        if let Some((class_name, _)) = dart_ctor_info(node, source) {
            return Some(class_name);
        }
        let sig = dart_inner_signature(node);
        let ret_type = sig
            .named_children(&mut sig.walk())
            .find(|child| child.kind() == "type_identifier")?;
        let text = strip_angle_generics(&node_text(ret_type, source));
        let last = text.split('.').next_back()?.trim().to_string();
        is_ident(&last).then_some(last)
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        let sig = dart_inner_signature(node);
        let params = sig
            .named_children(&mut sig.walk())
            .find(|child| child.kind() == "formal_parameter_list");
        let ret_type = sig
            .named_children(&mut sig.walk())
            .find(|child| matches!(child.kind(), "type_identifier" | "void_type"));
        if params.is_none() && ret_type.is_none() {
            return None;
        }
        let mut result = String::new();
        if let Some(ret_type) = ret_type {
            result.push_str(&node_text(ret_type, source));
            result.push(' ');
        }
        if let Some(params) = params {
            result.push_str(&node_text(params, source));
        }
        let result = result.trim().to_string();
        (!result.is_empty()).then_some(result)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        let _ = node;
        Some("public".to_string())
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        node.next_named_sibling()
            .filter(|next| next.kind() == "function_body")
            .is_some_and(|body| {
                (0..body.child_count()).any(|i| body.child(i).is_some_and(|c| c.kind() == "async"))
            })
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        node.kind() == "method_signature"
            && (0..node.child_count()).any(|i| node.child(i).is_some_and(|c| c.kind() == "static"))
    }

    fn resolve_name(&self, node: Node<'_>, _source: &str) -> Option<String> {
        // #2145 — `class A = B with C;`, a mixin application, names its class
        // inside the `mixin_application_class`, not in a `name` field.
        if node.kind() == "class_declaration" {
            let application = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "mixin_application_class");
            if let Some(application) = application {
                return application
                    .named_children(&mut application.walk())
                    .find(|child| child.kind() == "identifier")
                    .map(|id| node_text(id, _source));
            }
        }
        if let Some((class_name, ctor_name)) = dart_ctor_info(node, _source) {
            if ctor_name != class_name {
                return Some(ctor_name);
            }
            return None;
        }
        if matches!(
            node.kind(),
            "function_declaration" | "method_declaration" | "method_signature"
        ) {
            let sig = dart_inner_signature(node);
            return sig
                .named_children(&mut sig.walk())
                .find(|child| child.kind() == "identifier")
                .map(|id| node_text(id, _source));
        }
        None
    }

    fn is_misparsed_function(&self, _name: &str, node: Node<'_>, source: &str) -> bool {
        dart_ctor_info(node, source).is_some_and(|(class_name, ctor_name)| class_name == ctor_name)
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        let signature = node_text(node, source).trim().to_string();
        let module_name = node
            .named_children(&mut node.walk())
            .find(|child| matches!(child.kind(), "library_import" | "library_export"))
            .and_then(|lib| first_descendant_kind(lib, "string_literal"))
            .map(|lit| node_text(lit, source).replace(['\'', '"'], ""));
        module_name
            .filter(|m| !m.is_empty())
            .map(|module_name| ImportInfo {
                module_name,
                signature,
                handled_refs: false,
            })
    }

    fn extract_bare_call(&self, node: Node<'_>, source: &str) -> Option<String> {
        if matches!(node.kind(), "new_expression" | "const_object_expression") {
            if let Some(type_id) = node
                .named_children(&mut node.walk())
                .find(|child| child.kind() == "type_identifier")
            {
                if node.kind() == "const_object_expression" {
                    if let Some(name_id) = node
                        .named_children(&mut node.walk())
                        .find(|child| child.kind() == "identifier")
                    {
                        return Some(format!(
                            "{}.{}",
                            node_text(type_id, source),
                            node_text(name_id, source)
                        ));
                    }
                }
                return Some(node_text(type_id, source));
            }
        }
        None
    }
}

/// The wrappers tree-sitter-dart puts around a member, innermost first: the
/// `declaration` a member with no body opens (`Foo._();`, `void m();`,
/// `external int f();`), the `external_function_declaration` of a top-level
/// `external` function, and the `class_member` every class, mixin and
/// extension member sits in. The member's annotations are written inside
/// them, before it, and its dartdoc before the outermost one (#2382). A
/// wrapper counts only while nothing but annotations, comments and
/// `external` comes before the node inside it.
pub(crate) fn dart_member_wrappers(node: Node<'_>) -> Vec<Node<'_>> {
    let mut wrappers = Vec::new();
    let mut inner = node;
    while let Some(parent) = inner.parent() {
        let wraps = match parent.kind() {
            "declaration" | "external_function_declaration" => wrappers.is_empty(),
            "class_member" => true,
            _ => false,
        };
        if !wraps
            || !dart_prefix(parent, inner)
                .iter()
                .all(|prefix| dart_is_member_prefix(prefix.kind()))
        {
            break;
        }
        wrappers.push(parent);
        if parent.kind() == "class_member" {
            break;
        }
        inner = parent;
    }
    wrappers
}

/// The named children of `wrapper` written before `inner`.
pub(crate) fn dart_prefix<'tree>(wrapper: Node<'tree>, inner: Node<'tree>) -> Vec<Node<'tree>> {
    wrapper
        .named_children(&mut wrapper.walk())
        .take_while(|child| child.start_byte() < inner.start_byte())
        .collect()
}

/// What may stand before a member inside its wrapper.
fn dart_is_member_prefix(kind: &str) -> bool {
    matches!(
        kind,
        "annotation" | "comment" | "documentation_comment" | "external"
    )
}

/// The name of a supertype written in a Dart heritage list (G12): a `type`
/// holding the `type_identifier` it names, the last one past an import
/// prefix (`p.Base`). Its type arguments are `type` siblings that open with
/// `<` and hold no `type_identifier` of their own, so they name nothing.
pub(crate) fn dart_supertype_name(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "type_identifier" => Some(node),
        "type" => node
            .named_children(&mut node.walk())
            .filter(|child| child.kind() == "type_identifier")
            .last(),
        _ => None,
    }
}

fn dart_inner_signature(node: Node<'_>) -> Node<'_> {
    if matches!(node.kind(), "function_declaration" | "method_declaration") {
        if let Some(sig) = node
            .named_children(&mut node.walk())
            .find(|child| matches!(child.kind(), "method_signature" | "function_signature"))
        {
            return dart_inner_signature(sig);
        }
    }
    if node.kind() == "method_signature" {
        if let Some(inner) = node.named_children(&mut node.walk()).find(|child| {
            matches!(
                child.kind(),
                "function_signature" | "getter_signature" | "setter_signature"
            )
        }) {
            return inner;
        }
    }
    node
}

fn dart_constructor_signature(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "factory_constructor_signature" | "constructor_signature"
    ) {
        return Some(node);
    }
    if node.kind() == "method_signature" {
        return node.named_children(&mut node.walk()).find(|child| {
            matches!(
                child.kind(),
                "factory_constructor_signature" | "constructor_signature"
            )
        });
    }
    if node.kind() == "method_declaration" {
        return node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "method_signature")
            .and_then(dart_constructor_signature);
    }
    None
}

fn dart_enclosing_type_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if matches!(
            current.kind(),
            "class_definition"
                | "class_declaration"
                | "mixin_declaration"
                | "extension_declaration"
                | "extension_type_declaration"
                | "enum_declaration"
        ) {
            return child_by_field(current, "name").map(|name| node_text(name, source));
        }
        parent = current.parent();
    }
    None
}

fn dart_ctor_info(node: Node<'_>, source: &str) -> Option<(String, String)> {
    let ctor = dart_constructor_signature(node)?;
    let ids = ctor
        .named_children(&mut ctor.walk())
        .filter(|child| child.kind() == "identifier")
        .collect::<Vec<_>>();
    let class_name = dart_enclosing_type_name(node, source)?;
    let first = node_text(*ids.first()?, source);
    if first != class_name {
        return None;
    }
    let ctor_name = ids.get(1).map(|id| node_text(*id, source)).unwrap_or(first);
    Some((class_name, ctor_name))
}

fn first_descendant_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == kind {
            return Some(child);
        }
        if let Some(found) = first_descendant_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

fn strip_angle_generics(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in raw.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn is_ident(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
