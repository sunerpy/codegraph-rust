//! Swift `LanguageSpec`.
//!
//! Ports `upstream extraction/languages/swift.ts:43-138` onto
//! `tree-sitter-swift = 0.7.3` (the same alex-pinkus grammar family as
//! the upstream WASM build; docs/grammar-manifest.md tier-a PASS against core 0.26,
//! so no vendored `cc` build is required).

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::{ImportInfo, LanguageSpec};
use crate::walker::{child_by_field, node_text};

pub struct SwiftSpec;

pub static SWIFT_SPEC: SwiftSpec = SwiftSpec;

impl LanguageSpec for SwiftSpec {
    fn language(&self) -> Language {
        Language::Swift
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_swift::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        // swift.ts:44
        &["function_declaration"]
    }

    fn class_types(&self) -> &'static [&'static str] {
        // swift.ts:45 — also covers structs/enums/extensions, split by
        // classify_class_node.
        &["class_declaration"]
    }

    fn method_types(&self) -> &'static [&'static str] {
        // swift.ts:46 — methods are functions inside types.
        &["function_declaration"]
    }

    fn interface_types(&self) -> &'static [&'static str] {
        // swift.ts:47
        &["protocol_declaration"]
    }

    fn struct_types(&self) -> &'static [&'static str] {
        // swift.ts:48 — config parity: this grammar parses structs as
        // class_declaration, so the kind never fires; classification handles it.
        &["struct_declaration"]
    }

    fn enum_types(&self) -> &'static [&'static str] {
        // swift.ts:49 — same: enums classify out of class_declaration.
        &["enum_declaration"]
    }

    fn enum_member_types(&self) -> &'static [&'static str] {
        // swift.ts:50
        &["enum_entry"]
    }

    fn type_alias_types(&self) -> &'static [&'static str] {
        // swift.ts:51
        &["typealias_declaration"]
    }

    fn import_types(&self) -> &'static [&'static str] {
        // swift.ts:52
        &["import_declaration"]
    }

    fn call_types(&self) -> &'static [&'static str] {
        // swift.ts:53
        &["call_expression"]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        // swift.ts:54
        &["property_declaration", "constant_declaration"]
    }

    fn name_field(&self) -> &'static str {
        // swift.ts:55
        "name"
    }

    fn body_field(&self) -> &'static str {
        // swift.ts:56
        "body"
    }

    fn params_field(&self) -> &'static str {
        // swift.ts:57 — function_declaration exposes no `parameter` FIELD in
        // this grammar, so field lookups return None, matching the upstream
        // getChildByField result on the same grammar family.
        "parameter"
    }

    fn return_field(&self) -> &'static str {
        // swift.ts:58
        "return_type"
    }

    fn get_return_type(&self, node: Node<'_>, source: &str) -> Option<String> {
        // swift.ts:14-41 — positional return type: the first
        // user_type/optional_type after the simple_identifier name, before the
        // body. Generics stripped; LAST dotted segment kept; Void/invalid → None.
        let mut seen_name = false;
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "simple_identifier" && !seen_name {
                seen_name = true;
                continue;
            }
            if !seen_name {
                continue;
            }
            if child.kind() == "function_body" {
                return None;
            }
            let type_node = if child.kind() == "user_type" {
                Some(child)
            } else if child.kind() == "optional_type" {
                child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "user_type")
            } else {
                None
            };
            if let Some(type_node) = type_node {
                let raw = node_text(type_node, source);
                let name = strip_angle_generics(raw.trim());
                let last = name.split('.').next_back()?.trim().to_string();
                if last.is_empty() || !is_ident(&last) || last == "Void" {
                    return None;
                }
                return Some(last);
            }
        }
        None
    }

    fn resolve_name(&self, node: Node<'_>, source: &str) -> Option<String> {
        // swift.ts:60-75 — `extension KF.Builder` parses as class_declaration
        // whose name is a multi-segment user_type; name by the LAST segment so
        // the extension shares the extended type's simple name.
        if node.kind() != "class_declaration" {
            return None;
        }
        let name_node = child_by_field(node, "name")?;
        if name_node.kind() != "user_type" {
            return None;
        }
        let ids: Vec<Node<'_>> = name_node
            .named_children(&mut name_node.walk())
            .filter(|c| c.kind() == "type_identifier")
            .collect();
        if ids.len() > 1 {
            Some(node_text(*ids.last().unwrap(), source))
        } else {
            None
        }
    }

    fn get_signature(&self, node: Node<'_>, source: &str) -> Option<String> {
        // swift.ts:76-86 — params come from the `parameter` field; absent in
        // this grammar so the hook yields None like the upstream WASM run.
        let params = child_by_field(node, "parameter")?;
        let mut sig = node_text(params, source);
        if let Some(return_type) = child_by_field(node, "return_type") {
            sig.push_str(" -> ");
            sig.push_str(&node_text(return_type, source));
        }
        Some(sig)
    }

    fn get_visibility(&self, node: Node<'_>) -> Option<String> {
        // swift.ts:87-100 — modifiers scan; the upstream text.includes("private")
        // also matches `fileprivate`, so both keyword kinds map to private;
        // Swift defaults to internal.
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            if child.kind() != "modifiers" {
                continue;
            }
            if has_descendant_kind(child, "public") {
                return Some("public".to_string());
            }
            if has_descendant_kind(child, "private") || has_descendant_kind(child, "fileprivate") {
                return Some("private".to_string());
            }
            if has_descendant_kind(child, "internal") {
                return Some("internal".to_string());
            }
        }
        Some("internal".to_string())
    }

    fn is_static(&self, node: Node<'_>, _source: &str) -> bool {
        // swift.ts:101-111 — modifiers containing `static` or `class`
        // (class methods are static-dispatch members).
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            if child.kind() == "modifiers"
                && (has_descendant_kind(child, "static") || has_descendant_kind(child, "class"))
            {
                return true;
            }
        }
        false
    }

    fn classify_class_node(&self, node: Node<'_>) -> NodeKind {
        // swift.ts:112-120 — class_declaration covers classes, structs, enums;
        // split on the direct keyword child.
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            if child.kind() == "struct" {
                return NodeKind::Struct;
            }
            if child.kind() == "enum" {
                return NodeKind::Enum;
            }
        }
        NodeKind::Class
    }

    fn is_async(&self, node: Node<'_>) -> bool {
        // swift.ts:121-129 — only `async` inside a modifiers child counts
        // (a bare `async` effect token between params and body does not).
        for idx in 0..node.child_count() {
            let Some(child) = node.child(idx as u32) else {
                continue;
            };
            if child.kind() == "modifiers" && has_descendant_kind(child, "async") {
                return true;
            }
        }
        false
    }

    fn extract_import(&self, node: Node<'_>, source: &str) -> Option<ImportInfo> {
        // swift.ts:130-137 — module name from the `identifier` child.
        let identifier = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "identifier")?;
        Some(ImportInfo {
            module_name: node_text(identifier, source),
            signature: node_text(node, source).trim().to_string(),
            handled_refs: false,
        })
    }
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

fn has_descendant_kind(node: Node<'_>, kind: &str) -> bool {
    if node.kind() == kind {
        return true;
    }
    for idx in 0..node.child_count() {
        if node
            .child(idx as u32)
            .is_some_and(|child| has_descendant_kind(child, kind))
        {
            return true;
        }
    }
    false
}
