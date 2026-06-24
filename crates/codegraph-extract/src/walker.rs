//! Generic tree-sitter AST walker.
//!
//! Source map: `upstream extraction/tree-sitter.ts:211-352` becomes
//! [`TreeSitterWalker::extract`], `:355-578` becomes [`TreeSitterWalker::visit_node`],
//! `:580-650` becomes [`TreeSitterWalker::create_node`], `:748-983` maps to
//! symbol extractors, and `:1872-2632` maps to import/call reference extraction.

use codegraph_core::node_id::{file_node_id, generate_node_id};
use codegraph_core::types::{
    Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedRef,
};
use regex::Regex;
use std::path::Path;
use tree_sitter::Node as SyntaxNode;

use crate::spec::{has_type, LanguageSpec};

pub fn node_text(node: SyntaxNode<'_>, source: &str) -> String {
    let bytes = source.as_bytes();
    let start = node.start_byte();
    let end = node.end_byte();
    if start > end || end > bytes.len() {
        return String::new();
    }
    node.utf8_text(bytes).unwrap_or_default().to_string()
}

pub fn child_by_field<'tree>(node: SyntaxNode<'tree>, field: &str) -> Option<SyntaxNode<'tree>> {
    node.child_by_field_name(field)
}

pub struct TreeSitterWalker<'a, 'tree> {
    file_path: &'a str,
    source: &'a str,
    spec: &'static dyn LanguageSpec,
    root: SyntaxNode<'tree>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedRef>,
    errors: Vec<String>,
    node_stack: Vec<String>,
    fn_ref_candidates: Vec<(crate::function_ref::FnRefCandidate, String)>,
}

impl<'a, 'tree> TreeSitterWalker<'a, 'tree> {
    pub fn new(
        file_path: &'a str,
        source: &'a str,
        spec: &'static dyn LanguageSpec,
        root: SyntaxNode<'tree>,
    ) -> Self {
        Self {
            file_path,
            source,
            spec,
            root,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            node_stack: Vec::new(),
            fn_ref_candidates: Vec::new(),
        }
    }

    pub fn extract(mut self, duration_ms: i64) -> ExtractionResult {
        let file_node = self.create_file_node();
        let file_id = file_node.id.clone();
        self.nodes.push(file_node);
        self.node_stack.push(file_id);
        let package_id = self.extract_file_package();
        if let Some(package_id) = package_id.as_ref() {
            self.node_stack.push(package_id.clone());
        }
        self.visit_node(self.root);
        if package_id.is_some() {
            self.node_stack.pop();
        }
        self.node_stack.pop();

        self.flush_fn_ref_candidates();

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms,
        }
    }

    fn visit_node(&mut self, node: SyntaxNode<'tree>) {
        let node_type = node.kind();
        let mut skip_children = false;

        self.maybe_capture_fn_refs(node, node_type);

        if self.visit_language_specific(node) {
            return;
        }

        if is_jsx_element_kind(node_type) {
            self.extract_jsx_component_ref(node);
        } else if has_type(self.spec.function_types(), node_type) {
            if self.is_inside_class_like_node() && has_type(self.spec.method_types(), node_type) {
                if self.spec.class_member_is_method(node, self.source) {
                    self.extract_method(node);
                } else {
                    self.extract_property(node);
                }
            } else {
                self.extract_function(node, None);
            }
            skip_children = true;
        } else if has_type(self.spec.class_types(), node_type) {
            self.extract_classified_class(node);
            skip_children = true;
        } else if has_type(self.spec.module_types(), node_type) {
            self.extract_class(node, NodeKind::Module);
            skip_children = true;
        } else if has_type(self.spec.extra_class_node_types(), node_type) {
            self.extract_class(node, NodeKind::Class);
            skip_children = true;
        } else if has_type(self.spec.method_types(), node_type) {
            if self.spec.class_member_is_method(node, self.source) {
                self.extract_method(node);
            } else {
                self.extract_property(node);
            }
            skip_children = true;
        } else if has_type(self.spec.interface_types(), node_type) {
            self.extract_interface(node);
            skip_children = true;
        } else if has_type(self.spec.struct_types(), node_type) {
            self.extract_struct(node);
            skip_children = true;
        } else if has_type(self.spec.enum_types(), node_type) {
            self.extract_enum(node);
            skip_children = true;
        } else if has_type(self.spec.type_alias_types(), node_type) {
            self.extract_type_alias(node);
            skip_children = true;
        } else if has_type(self.spec.variable_types(), node_type)
            && !self.is_inside_class_like_node()
        {
            self.extract_variable(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if self.spec.language() == Language::Swift
            && node_type == "property_declaration"
            && self.is_inside_class_like_node()
        {
            // tree-sitter.ts:453-487 — Swift stored properties inside a type
            // are not their own nodes; their property-wrapper attributes and
            // declared type attach to the enclosing type. Children stay
            // visited so initializer calls are captured.
            if let Some(owner_id) = self.node_stack.last().cloned() {
                self.extract_decorators_for(node, &owner_id);
                self.extract_variable_type_annotation(node, &owner_id);
            }
        } else if has_type(self.spec.import_types(), node_type) {
            self.extract_import(node);
        } else if has_type(self.spec.property_types(), node_type)
            && self.is_inside_class_like_node()
        {
            self.extract_property(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if has_type(self.spec.field_types(), node_type) && self.is_inside_class_like_node() {
            self.extract_field(node);
            self.scan_fn_ref_subtree(node, 0);
            skip_children = true;
        } else if is_rust_impl_item(self.spec.language(), node_type) {
            self.extract_rust_impl_item(node);
        } else if has_type(self.spec.call_types(), node_type) {
            self.extract_call(node);
        } else if let Some(callee_name) = self.spec.extract_bare_call(node, self.source) {
            if let Some(caller_id) = self.node_stack.last().cloned() {
                self.push_ref(&caller_id, &callee_name, EdgeKind::Calls, node);
            }
        } else if node_type == "new_expression" {
            self.extract_instantiation(node);
        } else if (node_type == "property_signature" || node_type == "method_signature")
            && self.is_inside_class_like_node()
        {
            if let Some(parent_id) = self.node_stack.last().cloned() {
                self.extract_type_annotations(node, &parent_id);
            }
        }

        if !skip_children {
            self.visit_named_children(node);
        }
    }

    fn visit_named_children(&mut self, node: SyntaxNode<'tree>) {
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                self.visit_node(child);
            }
        }
    }

    fn visit_language_specific(&mut self, node: SyntaxNode<'tree>) -> bool {
        match self.spec.language() {
            Language::Scala => self.visit_scala_node(node),
            Language::Lua | Language::Luau => self.visit_lua_node(node),
            Language::ObjC => self.visit_objc_node(node),
            Language::Ruby => self.visit_ruby_node(node),
            Language::Php => self.visit_php_node(node),
            Language::R => self.visit_r_node(node),
            Language::Gdscript => self.visit_gdscript_node(node),
            _ => false,
        }
    }

    fn visit_ruby_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        if node.kind() != "call" || child_by_field(node, "receiver").is_some() {
            return false;
        }
        let Some(method) = child_by_field(node, "method") else {
            return false;
        };
        let method_name = node_text(method, self.source);
        if !matches!(method_name.as_str(), "include" | "extend" | "prepend") {
            return false;
        }
        let Some(parent_id) = self.node_stack.last().cloned() else {
            return false;
        };
        let Some(args) = child_by_field(node, "arguments").or_else(|| {
            node.named_children(&mut node.walk())
                .find(|c| c.kind() == "argument_list")
        }) else {
            return false;
        };
        for arg in args.named_children(&mut args.walk()) {
            if matches!(arg.kind(), "constant" | "scope_resolution") {
                self.push_ref(
                    &parent_id,
                    &node_text(arg, self.source),
                    EdgeKind::Implements,
                    node,
                );
            }
        }
        true
    }

    fn visit_php_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        if node.kind() == "use_declaration" {
            let Some(parent_id) = self.node_stack.last().cloned() else {
                return true;
            };
            for name in node
                .named_children(&mut node.walk())
                .filter(|child| matches!(child.kind(), "name" | "qualified_name"))
            {
                self.push_ref(
                    &parent_id,
                    &node_text(name, self.source),
                    EdgeKind::Implements,
                    node,
                );
            }
            return true;
        }
        if node.kind() == "const_declaration" && self.is_inside_class_like_node() {
            for elem in node
                .named_children(&mut node.walk())
                .filter(|child| child.kind() == "const_element")
            {
                let Some(name_node) = elem
                    .named_children(&mut elem.walk())
                    .find(|child| child.kind() == "name")
                else {
                    continue;
                };
                self.create_node(
                    NodeKind::Constant,
                    &node_text(name_node, self.source),
                    elem,
                    NodeExtra::default(),
                );
            }
            return true;
        }
        false
    }

    /// R extraction (upstream `languages/r.ts`, #828). Named functions are
    /// `name <- function(...)` assignments; classes are
    /// `setClass`/`setRefClass`/`R6Class`/`ggproto` calls; generics are
    /// `setGeneric`/`setMethod`; imports are `library`/`require`/`source`. The
    /// generic walker has no R node types, so all of this is hook-driven.
    fn visit_r_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        match node.kind() {
            "call" => self.visit_r_call(node),
            "binary_operator" => self.visit_r_binary(node),
            _ => false,
        }
    }

    fn visit_gdscript_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        match node.kind() {
            "enumerator" => {
                let Some(left) = child_by_field(node, "left") else {
                    return false;
                };
                let name = node_text(left, self.source);
                self.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
                true
            }
            "const_statement" => {
                let Some(name_node) = child_by_field(node, "name") else {
                    return false;
                };
                let name = node_text(name_node, self.source);
                self.create_node(NodeKind::Constant, &name, node, NodeExtra::default());
                true
            }
            // `var`, `@export var`, `@onready var` all parse as
            // `variable_statement` (annotation is a child, not a distinct
            // kind); the other two arms guard against a future grammar split.
            "variable_statement" | "export_variable_statement" | "onready_variable_statement" => {
                let Some(name_node) = child_by_field(node, "name") else {
                    return false;
                };
                let name = node_text(name_node, self.source);
                self.create_node(NodeKind::Variable, &name, node, NodeExtra::default());
                true
            }
            // `extends X` is a SIBLING statement, never a child clause of
            // extract_inheritance. Target is a non-field named child of kind
            // `type` or `string`; an `annotations` child is skipped. Strip
            // quotes for a `string` target.
            "extends_statement" => {
                if let Some((target, text)) = self.gdscript_extends_target(node) {
                    if let Some(parent_id) = self.node_stack.last().cloned() {
                        self.push_ref(&parent_id, &text, EdgeKind::Extends, target);
                    }
                }
                true
            }
            // An inner `class X extends Y:` keeps its `extends_statement` as the
            // `extends:` field, which class extraction never re-visits. Emit the
            // edge here (before extract_classified_class runs) and return false
            // so normal class extraction still owns the Class node.
            "class_definition" => {
                if let Some(extends) = child_by_field(node, "extends") {
                    if let Some((target, text)) = self.gdscript_extends_target(extends) {
                        if let Some(parent_id) = self.node_stack.last().cloned() {
                            self.push_ref(&parent_id, &text, EdgeKind::Extends, target);
                        }
                    }
                }
                false
            }
            // `class_name X` is the script file's own class; members are
            // file-level siblings, so it is NOT pushed onto node_stack (keeps
            // file-level funcs top-level Functions). Sole owner of this kind.
            "class_name_statement" => {
                let Some(name_node) = child_by_field(node, "name") else {
                    return false;
                };
                let name = node_text(name_node, self.source);
                self.create_node(NodeKind::Class, &name, node, NodeExtra::default());
                true
            }
            _ => false,
        }
    }

    fn gdscript_extends_target(
        &self,
        node: SyntaxNode<'tree>,
    ) -> Option<(SyntaxNode<'tree>, String)> {
        let target = node
            .named_children(&mut node.walk())
            .find(|child| matches!(child.kind(), "type" | "string"))?;
        let raw = node_text(target, self.source);
        let text = if target.kind() == "string" {
            raw.trim_matches(|c| c == '"' || c == '\'').to_string()
        } else {
            raw
        };
        Some((target, text))
    }

    fn visit_r_call(&mut self, node: SyntaxNode<'tree>) -> bool {
        let Some(fname) = r_callee_name(node, self.source) else {
            return false;
        };

        if matches!(
            fname.as_str(),
            "library" | "require" | "requireNamespace" | "loadNamespace" | "source"
        ) {
            let Some(module) = r_literal_or_identifier(r_first_arg_value(node), self.source) else {
                return true;
            };
            let signature: String = node_text(node, self.source)
                .trim()
                .chars()
                .take(100)
                .collect();
            self.create_node(
                NodeKind::Import,
                &module,
                node,
                NodeExtra {
                    signature: Some(signature),
                    ..NodeExtra::default()
                },
            );
            if let Some(parent_id) = self.node_stack.last().cloned() {
                self.push_ref(&parent_id, &module, EdgeKind::Imports, node);
            }
            return true;
        }

        if matches!(
            fname.as_str(),
            "setClass" | "setRefClass" | "R6Class" | "ggproto"
        ) {
            let Some(name) = r_literal_or_identifier(r_first_arg_value(node), self.source) else {
                return false;
            };
            if let Some(cls) = self.create_node(NodeKind::Class, &name, node, NodeExtra::default())
            {
                let cls_id = cls.id.clone();
                self.node_stack.push(cls_id);
                self.extract_r_class_members(node, &cls.id);
                self.node_stack.pop();
            }
            return true;
        }

        if matches!(fname.as_str(), "setGeneric" | "setMethod") {
            let Some(name) = r_literal_or_identifier(r_first_arg_value(node), self.source) else {
                return false;
            };
            let impl_fn = child_by_field(node, "arguments").and_then(|args| {
                args.named_children(&mut args.walk())
                    .filter(|a| a.kind() == "argument")
                    .find_map(|a| {
                        child_by_field(a, "value").filter(|v| v.kind() == "function_definition")
                    })
            });
            let params = impl_fn.and_then(|f| child_by_field(f, "parameters"));
            let fn_node = self.create_node(
                NodeKind::Function,
                &name,
                node,
                NodeExtra {
                    signature: params.map(|p| node_text(p, self.source)),
                    ..NodeExtra::default()
                },
            );
            if let (Some(fn_node), Some(body)) =
                (fn_node, impl_fn.and_then(|f| child_by_field(f, "body")))
            {
                let fn_id = fn_node.id.clone();
                self.node_stack.push(fn_id);
                self.visit_node(body);
                self.node_stack.pop();
            }
            return true;
        }

        false
    }

    fn visit_r_binary(&mut self, node: SyntaxNode<'tree>) -> bool {
        let Some(op) = child_by_field(node, "operator").map(|o| node_text(o, self.source)) else {
            return false;
        };
        let lhs = child_by_field(node, "lhs");
        let rhs = child_by_field(node, "rhs");
        let assign_left = matches!(op.as_str(), "<-" | "<<-" | "=");
        let assign_right = matches!(op.as_str(), "->" | "->>");

        if assign_left {
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if lhs.kind() == "identifier" && rhs.kind() == "function_definition" {
                    let params = child_by_field(rhs, "parameters");
                    let fn_node = self.create_node(
                        NodeKind::Function,
                        &node_text(lhs, self.source),
                        node,
                        NodeExtra {
                            signature: params.map(|p| node_text(p, self.source)),
                            ..NodeExtra::default()
                        },
                    );
                    if let (Some(fn_node), Some(body)) = (fn_node, child_by_field(rhs, "body")) {
                        let fn_id = fn_node.id.clone();
                        self.node_stack.push(fn_id);
                        self.visit_node(body);
                        self.node_stack.pop();
                    }
                    return true;
                }
            }
        }

        let top_level = node.parent().map(|p| p.kind()) == Some("program");

        if top_level && assign_left {
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if lhs.kind() == "identifier" {
                    let rhs_callee = if rhs.kind() == "call" {
                        r_callee_name(rhs, self.source)
                    } else {
                        None
                    };
                    let is_class_idiom = rhs_callee.is_some_and(|c| {
                        matches!(
                            c.as_str(),
                            "setClass"
                                | "setRefClass"
                                | "R6Class"
                                | "ggproto"
                                | "setGeneric"
                                | "setMethod"
                        )
                    });
                    if !is_class_idiom {
                        let name = node_text(lhs, self.source);
                        let kind = if is_r_constant_name(&name) {
                            NodeKind::Constant
                        } else {
                            NodeKind::Variable
                        };
                        self.create_node(kind, &name, node, NodeExtra::default());
                    }
                    self.visit_node(rhs);
                    return true;
                }
            }
        }

        if top_level && assign_right {
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                if rhs.kind() == "identifier" {
                    let name = node_text(rhs, self.source);
                    let kind = if is_r_constant_name(&name) {
                        NodeKind::Constant
                    } else {
                        NodeKind::Variable
                    };
                    self.create_node(kind, &name, node, NodeExtra::default());
                    self.visit_node(lhs);
                    return true;
                }
            }
        }

        false
    }

    /// Extract methods + parent of an R class call. Ports `extractClassMembers`
    /// (r.ts:118-164): list(...) of named functions (R5/R6), direct named
    /// function args (ggproto), and the parent class (ggproto 2nd positional,
    /// R6 `inherit=`, S4 `contains=`).
    fn extract_r_class_members(&mut self, class_call: SyntaxNode<'tree>, class_id: &str) {
        let Some(args) = child_by_field(class_call, "arguments") else {
            return;
        };
        let mut positional = 0;
        for arg in args.named_children(&mut args.walk()) {
            if arg.kind() != "argument" {
                continue;
            }
            let arg_name = child_by_field(arg, "name");
            let Some(value) = child_by_field(arg, "value") else {
                continue;
            };
            if arg_name.is_none() {
                positional += 1;
                if positional == 2 && value.kind() == "identifier" {
                    self.push_ref(
                        class_id,
                        &node_text(value, self.source),
                        EdgeKind::Extends,
                        value,
                    );
                }
                continue;
            }
            let arg_name_text = node_text(arg_name.unwrap(), self.source);
            if arg_name_text == "inherit" || arg_name_text == "contains" {
                if let Some(parent) = r_literal_or_identifier(Some(value), self.source) {
                    self.push_ref(class_id, &parent, EdgeKind::Extends, value);
                }
                continue;
            }
            if value.kind() == "function_definition" {
                self.emit_r_method_arg(arg);
                continue;
            }
            if value.kind() == "call"
                && r_callee_name(value, self.source).as_deref() == Some("list")
            {
                if let Some(list_args) = child_by_field(value, "arguments") {
                    for entry in list_args.named_children(&mut list_args.walk()) {
                        if entry.kind() == "argument" {
                            self.emit_r_method_arg(entry);
                        }
                    }
                }
            }
        }
    }

    /// Emit one `name = function(...)` argument as a method (ports `emitMethodArg`).
    fn emit_r_method_arg(&mut self, entry: SyntaxNode<'tree>) {
        let Some(entry_name) = child_by_field(entry, "name") else {
            return;
        };
        let Some(entry_value) = child_by_field(entry, "value") else {
            return;
        };
        if entry_value.kind() != "function_definition" {
            return;
        }
        let params = child_by_field(entry_value, "parameters");
        let method = self.create_node(
            NodeKind::Method,
            &node_text(entry_name, self.source),
            entry,
            NodeExtra {
                signature: params.map(|p| node_text(p, self.source)),
                ..NodeExtra::default()
            },
        );
        if let (Some(method), Some(body)) = (method, child_by_field(entry_value, "body")) {
            let method_id = method.id.clone();
            self.node_stack.push(method_id);
            self.visit_node(body);
            self.node_stack.pop();
        }
    }

    fn visit_scala_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        match node.kind() {
            "val_definition" | "var_definition" => {
                let Some(name_node) = child_by_field(node, "pattern")
                    .filter(|n| n.kind() == "identifier")
                    .or_else(|| first_descendant_kind(node, "identifier"))
                else {
                    return false;
                };
                let name = node_text(name_node, self.source);
                let in_class = self.is_inside_class_like_node();
                let kind = if in_class {
                    NodeKind::Field
                } else if node.kind() == "val_definition" {
                    NodeKind::Constant
                } else {
                    NodeKind::Variable
                };
                let signature = child_by_field(node, "type").map(|type_node| {
                    format!(
                        "{} {}: {}",
                        if node.kind() == "val_definition" {
                            "val"
                        } else {
                            "var"
                        },
                        name,
                        node_text(type_node, self.source)
                    )
                });
                if let Some(created) = self.create_node(
                    kind,
                    &name,
                    node,
                    NodeExtra {
                        signature,
                        visibility: self.spec.get_visibility(node),
                        ..NodeExtra::default()
                    },
                ) {
                    if let Some(type_node) = child_by_field(node, "type") {
                        self.extract_type_refs_from_subtree(type_node, &created.id);
                    }
                }
                true
            }
            "enum_case_definitions" => {
                for child in node.named_children(&mut node.walk()) {
                    if matches!(child.kind(), "simple_enum_case" | "full_enum_case") {
                        if let Some(name_node) = child_by_field(child, "name") {
                            self.create_node(
                                NodeKind::EnumMember,
                                &node_text(name_node, self.source),
                                child,
                                NodeExtra::default(),
                            );
                        }
                    }
                }
                true
            }
            "extension_definition" => {
                if let Some(body) = child_by_field(node, "body") {
                    self.visit_named_children(body);
                }
                true
            }
            _ => false,
        }
    }

    fn visit_lua_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        if node.kind() == "function_call" {
            if self.emit_lua_require(node) {
                return true;
            }
        }
        if node.kind() == "variable_declaration" {
            for call in descendants_of_kind(node, "function_call") {
                self.emit_lua_require(call);
            }
        }
        false
    }

    fn emit_lua_require(&mut self, call_node: SyntaxNode<'tree>) -> bool {
        let Some(module_name) = lua_require_module(call_node, self.source) else {
            return false;
        };
        self.create_node(
            NodeKind::Import,
            &module_name,
            call_node,
            NodeExtra {
                signature: Some(
                    node_text(call_node, self.source)
                        .chars()
                        .take(100)
                        .collect(),
                ),
                ..NodeExtra::default()
            },
        );
        if let Some(parent_id) = self.node_stack.last().cloned() {
            self.push_ref(&parent_id, &module_name, EdgeKind::Imports, call_node);
        }
        true
    }

    fn visit_objc_node(&mut self, node: SyntaxNode<'tree>) -> bool {
        if node.kind() != "class_implementation" {
            return false;
        }
        let Some(class_name_node) = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "identifier")
        else {
            return true;
        };
        let class_name = node_text(class_name_node, self.source);
        let class_id = self
            .nodes
            .iter()
            .find(|n| {
                n.name == class_name && n.file_path == self.file_path && n.kind == NodeKind::Class
            })
            .map(|n| n.id.clone())
            .or_else(|| {
                self.create_node(NodeKind::Class, &class_name, node, NodeExtra::default())
                    .map(|n| n.id)
            });
        let Some(class_id) = class_id else {
            return true;
        };
        self.node_stack.push(class_id);
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "implementation_definition" {
                self.visit_named_children(child);
            }
        }
        self.node_stack.pop();
        true
    }

    fn create_file_node(&self) -> Node {
        let name = Path::new(self.file_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        Node {
            id: file_node_id(self.file_path),
            kind: NodeKind::File,
            name,
            qualified_name: self.file_path.to_string(),
            file_path: self.file_path.to_string(),
            language: self.spec.language(),
            start_line: 1,
            end_line: self.source.split('\n').count() as i64,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn extract_file_package(&mut self) -> Option<String> {
        if self.spec.package_types().is_empty() {
            return None;
        }
        for child in self.root.named_children(&mut self.root.walk()) {
            if !has_type(self.spec.package_types(), child.kind()) {
                continue;
            }
            let Some(package_name) = self.spec.extract_package(child, self.source) else {
                continue;
            };
            let namespace = self.create_node(
                NodeKind::Namespace,
                &package_name,
                child,
                NodeExtra::default(),
            )?;
            return Some(namespace.id);
        }
        None
    }

    fn create_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        node: SyntaxNode<'tree>,
        extra: NodeExtra,
    ) -> Option<Node> {
        if name.is_empty() {
            return None;
        }

        let start_line = node.start_position().row as u32 + 1;
        let id = generate_node_id(self.file_path, kind, name, start_line);
        let mut new_node = Node {
            id: id.clone(),
            kind,
            name: name.to_string(),
            qualified_name: self.build_qualified_name(name),
            file_path: self.file_path.to_string(),
            language: self.spec.language(),
            start_line: start_line as i64,
            end_line: node.end_position().row as i64 + 1,
            start_column: node.start_position().column as i64,
            end_column: node.end_position().column as i64,
            docstring: extra.docstring,
            signature: extra.signature,
            visibility: extra.visibility,
            is_exported: extra.is_exported,
            is_async: extra.is_async,
            is_static: extra.is_static,
            is_abstract: extra.is_abstract,
            decorators: extra.decorators,
            type_parameters: extra.type_parameters,
            return_type: extra.return_type,
            updated_at: 0,
        };
        if let Some(qualified_name) = extra.qualified_name {
            new_node.qualified_name = qualified_name;
        }
        // Upstream tree-sitter.ts:626-634: extractModifiers output (Kotlin
        // expect/actual) merges into decorators.
        let mods = self.spec.extract_modifiers(node);
        if !mods.is_empty() {
            new_node.decorators.extend(mods);
        }

        self.nodes.push(new_node.clone());

        if let Some(parent_id) = self.node_stack.last() {
            self.edges.push(Edge {
                id: None,
                source: parent_id.clone(),
                target: id,
                kind: EdgeKind::Contains,
                metadata: None,
                line: None,
                col: None,
                provenance: None,
            });
        }

        Some(new_node)
    }

    fn extract_function(&mut self, node: SyntaxNode<'tree>, name_override: Option<String>) {
        if self.spec.get_receiver_type(node, self.source).is_some() {
            self.extract_method(node);
            return;
        }
        let mut name = name_override.unwrap_or_else(|| self.extract_name(node));
        if name == "<anonymous>"
            && (node.kind() == "arrow_function" || node.kind() == "function_expression")
        {
            if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declarator" {
                    if let Some(var_name) = child_by_field(parent, "name") {
                        name = node_text(var_name, self.source);
                    }
                }
            }
        }
        if name == "<anonymous>" {
            if let Some(body) = self.resolve_body(node) {
                self.visit_function_body(body);
            }
            return;
        }
        if self.spec.is_misparsed_function(&name, node, self.source) {
            if let Some(body) = self.resolve_body(node) {
                self.visit_function_body(body);
            }
            return;
        }

        let func_node = self.create_node(
            NodeKind::Function,
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                signature: self.spec.get_signature(node, self.source),
                visibility: self.spec.get_visibility(node),
                is_exported: self.spec.is_exported(node, self.source),
                is_async: self.spec.is_async(node),
                is_static: self.spec.is_static(node, self.source),
                return_type: self.spec.get_return_type(node, self.source),
                ..NodeExtra::default()
            },
        );
        let Some(func_node) = func_node else { return };
        self.extract_type_annotations(node, &func_node.id);
        self.extract_decorators_for(node, &func_node.id);
        self.node_stack.push(func_node.id);
        if let Some(body) = self.resolve_body(node) {
            self.visit_function_body(body);
        }
        self.node_stack.pop();
    }

    // Upstream tree-sitter.ts:389-404: classifyClassNode routes a classTypes node
    // to the struct/enum/interface/trait/class extractor.
    fn extract_classified_class(&mut self, node: SyntaxNode<'tree>) {
        match self.spec.classify_class_node(node) {
            NodeKind::Struct => self.extract_struct(node),
            NodeKind::Enum => self.extract_enum(node),
            NodeKind::Interface => self.extract_interface(node),
            NodeKind::Trait => self.extract_class(node, NodeKind::Trait),
            _ => self.extract_class(node, NodeKind::Class),
        }
    }

    fn extract_class(&mut self, node: SyntaxNode<'tree>, kind: NodeKind) {
        let name = self.extract_name(node);
        let class_node = self.create_node(
            kind,
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                visibility: self.spec.get_visibility(node),
                is_exported: self.spec.is_exported(node, self.source),
                ..NodeExtra::default()
            },
        );
        let Some(class_node) = class_node else { return };
        self.extract_inheritance(node, &class_node.id);
        self.extract_decorators_for(node, &class_node.id);
        self.node_stack.push(class_node.id);
        let body = self.resolve_body(node).unwrap_or(node);
        self.visit_named_children(body);
        self.node_stack.pop();
    }

    fn extract_struct(&mut self, node: SyntaxNode<'tree>) {
        let Some(body) = self.resolve_body(node) else {
            return;
        };
        let name = self.extract_name(node);
        let struct_node = self.create_node(
            NodeKind::Struct,
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                visibility: self.spec.get_visibility(node),
                is_exported: self.spec.is_exported(node, self.source),
                ..NodeExtra::default()
            },
        );
        let Some(struct_node) = struct_node else {
            return;
        };
        self.extract_inheritance(node, &struct_node.id);
        self.node_stack.push(struct_node.id);
        self.visit_named_children(body);
        self.node_stack.pop();
    }

    fn extract_method(&mut self, node: SyntaxNode<'tree>) {
        let receiver_type = self.spec.get_receiver_type(node, self.source);
        if !self.is_inside_class_like_node()
            && receiver_type.is_none()
            && !self.spec.methods_are_top_level()
        {
            if node
                .parent()
                .is_some_and(|p| p.kind() == "object" || p.kind() == "object_expression")
            {
                if let Some(body) = self.resolve_body(node) {
                    self.visit_function_body(body);
                }
                return;
            }
            self.extract_function(node, None);
            return;
        }

        let name = self.extract_name(node);
        if self.spec.is_misparsed_function(&name, node, self.source) {
            if let Some(body) = self.resolve_body(node) {
                self.visit_function_body(body);
            }
            return;
        }
        let method_node = self.create_node(
            NodeKind::Method,
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                signature: self.spec.get_signature(node, self.source),
                visibility: self.spec.get_visibility(node),
                is_async: self.spec.is_async(node),
                is_static: self.spec.is_static(node, self.source),
                return_type: self.spec.get_return_type(node, self.source),
                qualified_name: receiver_type.map(|receiver| format!("{receiver}::{name}")),
                ..NodeExtra::default()
            },
        );
        let Some(method_node) = method_node else {
            return;
        };
        if let Some(receiver) = self.spec.get_receiver_type(node, self.source) {
            if !self.is_inside_class_like_node() {
                if let Some(owner_id) = self
                    .nodes
                    .iter()
                    .find(|n| {
                        n.name == receiver
                            && n.file_path == self.file_path
                            && matches!(
                                n.kind,
                                NodeKind::Struct
                                    | NodeKind::Class
                                    | NodeKind::Enum
                                    | NodeKind::Trait
                            )
                    })
                    .map(|n| n.id.clone())
                {
                    self.edges.push(Edge {
                        id: None,
                        source: owner_id,
                        target: method_node.id.clone(),
                        kind: EdgeKind::Contains,
                        metadata: None,
                        line: None,
                        col: None,
                        provenance: None,
                    });
                }
            }
        }
        self.extract_type_annotations(node, &method_node.id);
        self.extract_decorators_for(node, &method_node.id);
        self.node_stack.push(method_node.id);
        if let Some(body) = self.resolve_body(node) {
            self.visit_function_body(body);
        }
        self.node_stack.pop();
    }

    fn extract_interface(&mut self, node: SyntaxNode<'tree>) {
        let name = self.extract_name(node);
        let interface_node = self.create_node(
            self.spec.interface_kind(),
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                is_exported: self.spec.is_exported(node, self.source),
                ..NodeExtra::default()
            },
        );
        let Some(interface_node) = interface_node else {
            return;
        };
        self.extract_inheritance(node, &interface_node.id);
        self.node_stack.push(interface_node.id);
        let body = self.resolve_body(node).unwrap_or(node);
        self.visit_named_children(body);
        self.node_stack.pop();
    }

    fn extract_enum(&mut self, node: SyntaxNode<'tree>) {
        let Some(body) = self.resolve_body(node) else {
            return;
        };
        let name = self.extract_name(node);
        let enum_node = self.create_node(
            NodeKind::Enum,
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                visibility: self.spec.get_visibility(node),
                is_exported: self.spec.is_exported(node, self.source),
                ..NodeExtra::default()
            },
        );
        let Some(enum_node) = enum_node else { return };
        self.extract_inheritance(node, &enum_node.id);
        self.node_stack.push(enum_node.id);
        for i in 0..body.named_child_count() {
            if let Some(child) = body.named_child(i as u32) {
                if has_type(self.spec.enum_member_types(), child.kind()) {
                    self.extract_enum_member(child);
                } else {
                    self.visit_node(child);
                }
            }
        }
        self.node_stack.pop();
    }

    fn extract_enum_member(&mut self, node: SyntaxNode<'tree>) {
        // tree-sitter.ts:1105-1131. The upstream WASM swift grammar exposes no
        // `name` field on enum_entry, so its identifier loop emits one member
        // per simple_identifier (`case put, delete` → put AND delete).
        // tree-sitter-swift 0.7.3 marks EVERY case identifier with the `name`
        // field, so a single child_by_field lookup would drop all but the
        // first; collect all name fields to preserve the upstream output.
        let mut cursor = node.walk();
        let name_nodes: Vec<SyntaxNode<'tree>> =
            node.children_by_field_name("name", &mut cursor).collect();
        drop(cursor);
        if name_nodes.len() == 1 {
            self.create_node(
                NodeKind::EnumMember,
                &node_text(name_nodes[0], self.source),
                node,
                NodeExtra::default(),
            );
            return;
        }
        if name_nodes.len() > 1 {
            for name_node in name_nodes {
                self.create_node(
                    NodeKind::EnumMember,
                    &node_text(name_node, self.source),
                    name_node,
                    NodeExtra::default(),
                );
            }
            return;
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                if matches!(
                    child.kind(),
                    "simple_identifier" | "identifier" | "property_identifier"
                ) {
                    self.create_node(
                        NodeKind::EnumMember,
                        &node_text(child, self.source),
                        child,
                        NodeExtra::default(),
                    );
                }
            }
        }
    }

    fn extract_type_alias(&mut self, node: SyntaxNode<'tree>) {
        let name = self.extract_name(node);
        if name == "<anonymous>" {
            return;
        }
        let alias_kind = self
            .spec
            .resolve_type_alias_kind(node, self.source)
            .unwrap_or(NodeKind::TypeAlias);
        let type_alias_node = self.create_node(
            alias_kind,
            &name,
            node,
            NodeExtra {
                docstring: self.preceding_docstring(node),
                is_exported: self.spec.is_exported(node, self.source),
                ..NodeExtra::default()
            },
        );
        if let Some(type_alias_node) = type_alias_node {
            if matches!(
                alias_kind,
                NodeKind::Class
                    | NodeKind::Struct
                    | NodeKind::Interface
                    | NodeKind::Trait
                    | NodeKind::Enum
            ) {
                if let Some(type_child) = child_by_field(node, "type") {
                    self.extract_inheritance(type_child, &type_alias_node.id);
                }
                self.node_stack.push(type_alias_node.id.clone());
                if self.spec.language() == Language::Go && alias_kind == NodeKind::Interface {
                    if let Some(type_child) = child_by_field(node, "type") {
                        self.extract_go_interface_methods(type_child);
                    }
                } else if let Some(type_child) = child_by_field(node, "type") {
                    let body =
                        child_by_field(type_child, self.spec.body_field()).unwrap_or(type_child);
                    self.visit_named_children(body);
                } else {
                    self.visit_named_children(node);
                }
                self.node_stack.pop();
                return;
            }
            if let Some(value) = child_by_field(node, "value") {
                self.extract_type_refs_from_subtree(value, &type_alias_node.id);
            }
        }
    }

    fn extract_go_interface_methods(&mut self, interface_type: SyntaxNode<'tree>) {
        for method in interface_type.named_children(&mut interface_type.walk()) {
            if !matches!(method.kind(), "method_elem" | "method_spec") {
                continue;
            }
            let Some(name_node) = child_by_field(method, "name").or_else(|| method.named_child(0))
            else {
                continue;
            };
            let name = node_text(name_node, self.source);
            if !name.is_empty() {
                self.create_node(
                    NodeKind::Method,
                    &name,
                    method,
                    NodeExtra {
                        signature: self.spec.get_signature(method, self.source),
                        ..NodeExtra::default()
                    },
                );
            }
        }
    }

    fn extract_variable(&mut self, node: SyntaxNode<'tree>) {
        if matches!(self.spec.language(), Language::Lua | Language::Luau) {
            self.extract_lua_variable(node);
            return;
        }
        if self.spec.language() == Language::Python {
            self.extract_python_assignment(node);
            return;
        }
        if self.spec.language() == Language::Go {
            self.extract_go_variable(node);
            return;
        }
        if !matches!(
            self.spec.language(),
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
        ) {
            return;
        }
        let kind = if self.spec.is_const(node) {
            NodeKind::Constant
        } else {
            NodeKind::Variable
        };
        let docstring = self.preceding_docstring(node);
        let is_exported = self.spec.is_exported(node, self.source);

        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i as u32) else {
                continue;
            };
            if child.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = child_by_field(child, "name") else {
                continue;
            };
            if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
                continue;
            }
            let value_node = child_by_field(child, "value");
            if let Some(value) = value_node {
                if value.kind() == "arrow_function" || value.kind() == "function_expression" {
                    self.extract_function(value, None);
                    continue;
                }
            }
            let name = node_text(name_node, self.source);
            let signature = value_node.map(|value| {
                let init = node_text(value, self.source)
                    .chars()
                    .take(100)
                    .collect::<String>();
                format!("= {}{}", init, if init.len() >= 100 { "..." } else { "" })
            });
            if let Some(var_node) = self.create_node(
                kind,
                &name,
                child,
                NodeExtra {
                    docstring: docstring.clone(),
                    signature,
                    is_exported,
                    ..NodeExtra::default()
                },
            ) {
                self.extract_variable_type_annotation(child, &var_node.id);
            }
            if let Some(value) = value_node {
                if value.kind() != "object" && value.kind() != "object_expression" {
                    self.visit_function_body(value);
                }
            }
        }
    }

    fn extract_python_assignment(&mut self, node: SyntaxNode<'tree>) {
        let Some(left) = child_by_field(node, "left").or_else(|| node.named_child(0)) else {
            return;
        };
        if left.kind() != "identifier" {
            return;
        }
        let name = node_text(left, self.source);
        let signature = child_by_field(node, "right").map(|value| {
            let init = node_text(value, self.source)
                .chars()
                .take(100)
                .collect::<String>();
            format!("= {}{}", init, if init.len() >= 100 { "..." } else { "" })
        });
        self.create_node(
            NodeKind::Variable,
            &name,
            node,
            NodeExtra {
                signature,
                ..NodeExtra::default()
            },
        );
    }

    fn extract_go_variable(&mut self, node: SyntaxNode<'tree>) {
        let kind = if node.kind() == "const_declaration" {
            NodeKind::Constant
        } else {
            NodeKind::Variable
        };
        for ident in descendants_of_kind(node, "identifier") {
            let Some(parent) = ident.parent() else {
                continue;
            };
            if !matches!(
                parent.kind(),
                "var_spec" | "const_spec" | "short_var_declaration"
            ) {
                continue;
            }
            self.create_node(
                kind,
                &node_text(ident, self.source),
                ident,
                NodeExtra::default(),
            );
            break;
        }
    }

    fn extract_lua_variable(&mut self, node: SyntaxNode<'tree>) {
        let is_local = node_text(node, self.source)
            .trim_start()
            .starts_with("local");
        let _ = is_local;
        let kind = NodeKind::Variable;
        for name_node in descendants_of_kind(node, "identifier") {
            let Some(parent) = name_node.parent() else {
                continue;
            };
            if !matches!(parent.kind(), "variable_list" | "variable_declarator") {
                continue;
            }
            let name = node_text(name_node, self.source);
            if name == "require" {
                continue;
            }
            self.create_node(
                kind,
                &name,
                name_node,
                NodeExtra {
                    is_exported: false,
                    ..NodeExtra::default()
                },
            );
            break;
        }
    }

    fn extract_property(&mut self, node: SyntaxNode<'tree>) {
        let name = self
            .spec
            .extract_property_name(node, self.source)
            .unwrap_or_else(|| self.extract_name(node));
        let signature = property_or_field_signature(node, &name, self.source);
        if let Some(prop_node) = self.create_node(
            NodeKind::Property,
            &name,
            node,
            NodeExtra {
                signature,
                visibility: self.spec.get_visibility(node),
                is_static: self.spec.is_static(node, self.source),
                ..NodeExtra::default()
            },
        ) {
            self.extract_type_annotations(node, &prop_node.id);
        }
    }

    fn extract_field(&mut self, node: SyntaxNode<'tree>) {
        let declarators = field_declarators(node).collect::<Vec<_>>();
        if declarators.is_empty() && self.spec.language() == Language::Php {
            for elem in node
                .named_children(&mut node.walk())
                .filter(|child| child.kind() == "property_element")
            {
                let Some(name_node) = elem
                    .named_children(&mut elem.walk())
                    .find(|child| child.kind() == "name")
                else {
                    continue;
                };
                let name = node_text(name_node, self.source);
                let signature = property_or_field_signature(node, &format!("${name}"), self.source);
                if let Some(field_node) = self.create_node(
                    NodeKind::Field,
                    &name,
                    elem,
                    NodeExtra {
                        signature,
                        visibility: self.spec.get_visibility(node),
                        is_static: self.spec.is_static(node, self.source),
                        ..NodeExtra::default()
                    },
                ) {
                    self.extract_type_annotations(node, &field_node.id);
                }
            }
            return;
        }

        for declarator in declarators {
            let Some(name_node) = child_by_field(declarator, "name").or_else(|| {
                declarator
                    .named_children(&mut declarator.walk())
                    .find(|c| c.kind() == "identifier")
            }) else {
                continue;
            };
            let name = node_text(name_node, self.source);
            let signature = property_or_field_signature(node, &name, self.source);
            if let Some(field_node) = self.create_node(
                NodeKind::Field,
                &name,
                declarator,
                NodeExtra {
                    signature,
                    visibility: self.spec.get_visibility(node),
                    is_static: self.spec.is_static(node, self.source),
                    ..NodeExtra::default()
                },
            ) {
                self.extract_type_annotations(node, &field_node.id);
            }
        }
    }

    fn extract_import(&mut self, node: SyntaxNode<'tree>) {
        if self.spec.language() == Language::Python && node.kind() == "import_statement" {
            self.extract_python_import_statement(node);
            return;
        }
        if self.spec.language() == Language::Go {
            self.extract_go_import_declaration(node);
            return;
        }
        let Some(info) = self.spec.extract_import(node, self.source) else {
            return;
        };
        self.create_node(
            NodeKind::Import,
            &info.module_name,
            node,
            NodeExtra {
                signature: Some(info.signature),
                ..NodeExtra::default()
            },
        );
        let Some(parent_id) = self.node_stack.last().cloned() else {
            return;
        };
        if !info.handled_refs {
            self.push_ref(&parent_id, &info.module_name, EdgeKind::Imports, node);
        }
        if matches!(
            self.spec.language(),
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
        ) {
            self.emit_import_binding_refs(node, &parent_id);
        }
        if self.spec.language() == Language::Python && node.kind() == "import_from_statement" {
            self.emit_python_from_import_refs(node, &parent_id);
        }
        if self.spec.language() == Language::Rust && node.kind() == "use_declaration" {
            self.emit_rust_use_binding_refs(node, &parent_id);
        }
    }

    fn extract_python_import_statement(&mut self, node: SyntaxNode<'tree>) {
        let parent_id = self.node_stack.last().cloned();
        let signature = node_text(node, self.source).trim().to_string();
        for child in node.named_children(&mut node.walk()) {
            let module_node = if child.kind() == "dotted_name" {
                Some(child)
            } else if child.kind() == "aliased_import" {
                child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "dotted_name")
            } else {
                None
            };
            let Some(module_node) = module_node else {
                continue;
            };
            let module_name = node_text(module_node, self.source);
            self.create_node(
                NodeKind::Import,
                &module_name,
                node,
                NodeExtra {
                    signature: Some(signature.clone()),
                    ..NodeExtra::default()
                },
            );
            if let Some(parent_id) = parent_id.as_ref() {
                self.push_ref(parent_id, &module_name, EdgeKind::Imports, module_node);
            }
        }
    }

    fn extract_go_import_declaration(&mut self, node: SyntaxNode<'tree>) {
        let parent_id = self.node_stack.last().cloned();
        for spec in descendants_of_kind(node, "import_spec") {
            let Some(string_node) = spec.named_children(&mut spec.walk()).find(|child| {
                child.kind() == "interpreted_string_literal" || child.kind() == "raw_string_literal"
            }) else {
                continue;
            };
            let import_path = node_text(string_node, self.source).replace(['\'', '"', '`'], "");
            if import_path.is_empty() {
                continue;
            }
            self.create_node(
                NodeKind::Import,
                &import_path,
                spec,
                NodeExtra {
                    signature: Some(node_text(spec, self.source).trim().to_string()),
                    ..NodeExtra::default()
                },
            );
            if let Some(parent_id) = parent_id.as_ref() {
                self.push_ref(parent_id, &import_path, EdgeKind::Imports, spec);
            }
        }
    }

    fn emit_import_binding_refs(&mut self, node: SyntaxNode<'tree>, from_node_id: &str) {
        let Some(clause) = node
            .named_children(&mut node.walk())
            .find(|c| c.kind() == "import_clause")
        else {
            return;
        };
        for child in clause.named_children(&mut clause.walk()) {
            match child.kind() {
                "identifier" => self.push_ref(
                    from_node_id,
                    &node_text(child, self.source),
                    EdgeKind::Imports,
                    child,
                ),
                "named_imports" => {
                    for spec in child.named_children(&mut child.walk()) {
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let name_node = child_by_field(spec, "alias")
                            .or_else(|| child_by_field(spec, "name"))
                            .or_else(|| spec.named_child(0));
                        if let Some(name_node) = name_node {
                            self.push_ref(
                                from_node_id,
                                &node_text(name_node, self.source),
                                EdgeKind::Imports,
                                name_node,
                            );
                        }
                    }
                }
                "namespace_import" => {
                    let name_node = child
                        .named_children(&mut child.walk())
                        .find(|c| c.kind() == "identifier")
                        .or_else(|| child.named_child(0));
                    if let Some(name_node) = name_node {
                        self.push_ref(
                            from_node_id,
                            &node_text(name_node, self.source),
                            EdgeKind::Imports,
                            name_node,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn emit_python_from_import_refs(&mut self, node: SyntaxNode<'tree>, from_node_id: &str) {
        let module = child_by_field(node, "module_name");
        for child in node.named_children(&mut node.walk()) {
            if module.is_some_and(|m| {
                m.start_byte() == child.start_byte() && m.end_byte() == child.end_byte()
            }) {
                continue;
            }
            if child.kind() == "wildcard_import" {
                continue;
            }
            let name_node = if child.kind() == "aliased_import" {
                child_by_field(child, "alias")
                    .or_else(|| child_by_field(child, "name"))
                    .or_else(|| child.named_child(0))
            } else if child.kind() == "dotted_name" {
                Some(child)
            } else {
                None
            };
            let Some(name_node) = name_node else { continue };
            let raw = node_text(name_node, self.source);
            let local = raw.rsplit('.').next().unwrap_or(raw.as_str());
            if !local.is_empty() {
                self.push_ref(from_node_id, local, EdgeKind::Imports, name_node);
            }
        }
    }

    fn emit_rust_use_binding_refs(&mut self, node: SyntaxNode<'tree>, from_node_id: &str) {
        let mut paths = Vec::new();
        collect_rust_use_paths(node, "", self.source, &mut paths);
        for (path, path_node) in paths {
            let leaf = path.rsplit("::").next().unwrap_or(path.as_str());
            if matches!(leaf, "self" | "super" | "crate" | "*") || leaf.is_empty() {
                continue;
            }
            self.push_ref(from_node_id, &path, EdgeKind::Imports, path_node);
        }
    }

    fn extract_call(&mut self, node: SyntaxNode<'tree>) {
        let Some(caller_id) = self.node_stack.last().cloned() else {
            return;
        };
        // Java/Kotlin `method_invocation` and PHP `member_call_expression` /
        // `scoped_call_expression` carry `object`/`scope` + `name` fields instead
        // of a `function` member-expression (tree-sitter.ts:2573-2663).
        if matches!(
            node.kind(),
            "method_invocation" | "member_call_expression" | "scoped_call_expression"
        ) {
            if let Some(name_field) = child_by_field(node, "name") {
                if let Some(object_field) =
                    child_by_field(node, "object").or_else(|| child_by_field(node, "scope"))
                {
                    self.extract_object_name_call(node, &caller_id, name_field, object_field);
                    return;
                }
            }
        }
        let mut callee_name = String::new();
        let func = child_by_field(node, "function").or_else(|| node.named_child(0));
        if let Some(func) = func {
            if matches!(
                func.kind(),
                "member_expression"
                    | "attribute"
                    | "selector_expression"
                    | "navigation_expression"
                    | "field_expression"
                    | "qualified_identifier"
            ) {
                let property = child_by_field(func, "property")
                    .or_else(|| child_by_field(func, "field"))
                    .or_else(|| func.named_child(1))
                    .map(|prop| {
                        // tree-sitter.ts:2503-2506 — Swift wraps the method
                        // name in navigation_suffix; unwrap its
                        // simple_identifier (kotlin-ng exposes identifiers
                        // directly, Swift does not).
                        if prop.kind() == "navigation_suffix" {
                            prop.named_children(&mut prop.walk())
                                .find(|c| c.kind() == "simple_identifier")
                                .unwrap_or(prop)
                        } else {
                            prop
                        }
                    });
                if let Some(property) = property {
                    let method_name = node_text(property, self.source);
                    let receiver = child_by_field(func, "object")
                        .or_else(|| child_by_field(func, "operand"))
                        .or_else(|| child_by_field(func, "argument"))
                        .or_else(|| func.named_child(0));
                    if let Some(receiver) = receiver {
                        if matches!(
                            receiver.kind(),
                            "identifier" | "simple_identifier" | "field_identifier"
                        ) {
                            let receiver_name = node_text(receiver, self.source);
                            if matches!(receiver_name.as_str(), "self" | "this" | "cls" | "super") {
                                callee_name = method_name;
                            } else {
                                callee_name = format!("{receiver_name}.{method_name}");
                            }
                        } else if receiver.kind() == "call_expression"
                            && matches!(self.spec.language(), Language::Kotlin | Language::Swift)
                        {
                            // tree-sitter.ts:2548-2563 — Kotlin/Swift chained
                            // factory calls `Foo.getInstance().bar()` re-encode
                            // as `<inner>().<method>` ONLY when the inner
                            // callee starts with a capitalized type; lowercase
                            // instance chains keep the bare method name.
                            let inner = receiver
                                .named_child(0)
                                .map(|inner| {
                                    node_text(inner, self.source).replace(char::is_whitespace, "")
                                })
                                .unwrap_or_default();
                            let reencode =
                                inner.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                            callee_name = if reencode && !inner.is_empty() {
                                format!("{inner}().{method_name}")
                            } else {
                                method_name
                            };
                        } else if receiver.kind() == "call_expression"
                            && matches!(self.spec.language(), Language::Rust | Language::Go)
                        {
                            let inner = child_by_field(receiver, "function")
                                .map(|inner| {
                                    node_text(inner, self.source).replace(char::is_whitespace, "")
                                })
                                .unwrap_or_default();
                            let reencode = if self.spec.language() == Language::Rust {
                                child_by_field(receiver, "function")
                                    .is_some_and(|inner| inner.kind() == "scoped_identifier")
                            } else {
                                child_by_field(receiver, "function")
                                    .is_some_and(|inner| inner.kind() == "identifier")
                            };
                            callee_name = if reencode && !inner.is_empty() {
                                format!("{inner}().{method_name}")
                            } else {
                                method_name
                            };
                        } else {
                            callee_name = method_name;
                        }
                    } else {
                        callee_name = method_name;
                    }
                }
            } else if matches!(func.kind(), "scoped_identifier" | "scoped_call_expression") {
                callee_name = node_text(func, self.source);
            } else {
                callee_name = node_text(func, self.source);
            }
        }
        if let Some(converted) = normalize_parenthesized_go_conversion(&callee_name) {
            callee_name = converted;
        }
        if !callee_name.is_empty() {
            self.push_ref(&caller_id, &callee_name, EdgeKind::Calls, node);
        }
    }

    /// Encode a Java/Kotlin/PHP `object`/`scope` + `name` call into a resolver
    /// reference (tree-sitter.ts:2578-2663). Handles the PHP `Cls::for()->m()`
    /// and Java `Foo.factory().m()` chain re-encodings (`<inner>().<method>`),
    /// `this.field` field-access unwrap, and `self/this/...` receiver skipping.
    fn extract_object_name_call(
        &mut self,
        node: SyntaxNode<'tree>,
        caller_id: &str,
        name_field: SyntaxNode<'tree>,
        object_field: SyntaxNode<'tree>,
    ) {
        let method_name = node_text(name_field, self.source);
        if method_name.is_empty() {
            return;
        }

        // PHP `Cls::for($x)->method()`: receiver is a static call, re-encode as
        // `<Cls::factory>().<method>` (#608).
        if self.spec.language() == Language::Php && object_field.kind() == "scoped_call_expression"
        {
            let callee = match (
                child_by_field(object_field, "scope"),
                child_by_field(object_field, "name"),
            ) {
                (Some(inner_scope), Some(inner_name)) => format!(
                    "{}::{}().{method_name}",
                    node_text(inner_scope, self.source),
                    node_text(inner_name, self.source)
                ),
                _ => method_name.clone(),
            };
            self.push_ref(caller_id, &callee, EdgeKind::Calls, node);
            return;
        }

        // Java `Foo.getInstance().bar()`: receiver is itself a method call,
        // re-encode as `<inner-obj>.<inner-name>().<method>` (#645/#608).
        if self.spec.language() == Language::Java && object_field.kind() == "method_invocation" {
            if let (Some(inner_obj), Some(inner_name)) = (
                child_by_field(object_field, "object"),
                child_by_field(object_field, "name"),
            ) {
                let callee = format!(
                    "{}.{}().{method_name}",
                    node_text(inner_obj, self.source),
                    node_text(inner_name, self.source)
                );
                self.push_ref(caller_id, &callee, EdgeKind::Calls, node);
                return;
            }
        }

        // `this.userbo.toLogin2()` parses as object=field_access(this, userbo);
        // the receiver is the field name (`userbo`) (tree-sitter.ts:2641-2651).
        let mut receiver_name = if object_field.kind() == "field_access" {
            match (
                child_by_field(object_field, "object"),
                child_by_field(object_field, "field"),
            ) {
                (Some(inner), Some(field))
                    if matches!(inner.kind(), "this" | "this_expression") =>
                {
                    node_text(field, self.source)
                }
                _ => node_text(object_field, self.source),
            }
        } else {
            node_text(object_field, self.source)
        };
        receiver_name = receiver_name
            .strip_prefix('$')
            .map_or(receiver_name.clone(), str::to_string);

        let callee = if matches!(
            receiver_name.as_str(),
            "self" | "this" | "cls" | "super" | "parent" | "static"
        ) {
            method_name
        } else {
            format!("{receiver_name}.{method_name}")
        };
        self.push_ref(caller_id, &callee, EdgeKind::Calls, node);
    }

    fn extract_instantiation(&mut self, node: SyntaxNode<'tree>) {
        let Some(from_id) = self.node_stack.last().cloned() else {
            return;
        };
        let ctor = child_by_field(node, "constructor")
            .or_else(|| child_by_field(node, "type"))
            .or_else(|| child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };
        let mut class_name = node_text(ctor, self.source);
        if let Some(idx) = class_name.find('<') {
            class_name.truncate(idx);
        }
        if let Some(idx) = class_name.rfind(['.', ':']) {
            class_name = class_name[idx + 1..]
                .trim_start_matches([':', '.'])
                .to_string();
        }
        class_name = class_name.trim().to_string();
        if !class_name.is_empty() {
            self.push_ref(&from_id, &class_name, EdgeKind::Instantiates, node);
        }
    }

    fn visit_function_body(&mut self, body: SyntaxNode<'tree>) {
        self.visit_body_node(body);
    }

    fn visit_body_node(&mut self, node: SyntaxNode<'tree>) {
        let node_type = node.kind();
        self.maybe_capture_fn_refs(node, node_type);
        if is_jsx_element_kind(node_type) {
            self.extract_jsx_component_ref(node);
        } else if has_type(self.spec.call_types(), node_type) {
            self.extract_call(node);
        } else if node_type == "new_expression" {
            self.extract_instantiation(node);
        } else if let Some(callee_name) = self.spec.extract_bare_call(node, self.source) {
            if let Some(caller_id) = self.node_stack.last().cloned() {
                self.push_ref(&caller_id, &callee_name, EdgeKind::Calls, node);
            }
        }

        if node_type == "variable_declarator" {
            if let Some(owner_id) = self.node_stack.last().cloned() {
                self.extract_variable_type_annotation(node, &owner_id);
            }
        }

        if has_type(self.spec.function_types(), node_type) {
            let nested_name = self.extract_name(node);
            if !nested_name.is_empty() && nested_name != "<anonymous>" {
                self.extract_function(node, None);
                return;
            }
        }

        if has_type(self.spec.class_types(), node_type) {
            self.extract_classified_class(node);
            return;
        }
        if has_type(self.spec.enum_types(), node_type) {
            self.extract_enum(node);
            return;
        }
        if has_type(self.spec.interface_types(), node_type) {
            self.extract_interface(node);
            return;
        }

        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                self.visit_body_node(child);
            }
        }
    }

    fn extract_inheritance(&mut self, node: SyntaxNode<'tree>, class_id: &str) {
        for child in node.named_children(&mut node.walk()) {
            if matches!(
                child.kind(),
                "extends_clause" | "superclass" | "extends_interfaces"
            ) {
                if let Some(target) = child.named_child(0) {
                    self.push_ref(
                        class_id,
                        &node_text(target, self.source),
                        EdgeKind::Extends,
                        target,
                    );
                }
            }
            if matches!(child.kind(), "implements_clause" | "super_interfaces") {
                for iface in child.named_children(&mut child.walk()) {
                    self.push_ref(
                        class_id,
                        &node_text(iface, self.source),
                        EdgeKind::Implements,
                        iface,
                    );
                }
            }
            // tree-sitter.ts:3445-3465 — Kotlin `class Foo : Bar(), Baz`:
            // each delegation_specifier wraps a user_type (interface) or
            // constructor_invocation > user_type (superclass call); the upstream
            // emits `extends` named by the inner type identifier for both.
            // kotlin-ng groups them under a delegation_specifiers container.
            if child.kind() == "delegation_specifiers" {
                self.extract_inheritance(child, class_id);
            }
            if child.kind() == "delegation_specifier" {
                let user_type = child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "user_type")
                    .or_else(|| {
                        child
                            .named_children(&mut child.walk())
                            .find(|c| c.kind() == "constructor_invocation")
                            .and_then(|inv| {
                                inv.named_children(&mut inv.walk())
                                    .find(|c| c.kind() == "user_type")
                            })
                    });
                if let Some(user_type) = user_type {
                    let type_id = user_type
                        .named_children(&mut user_type.walk())
                        .find(|c| matches!(c.kind(), "type_identifier" | "identifier"))
                        .unwrap_or(user_type);
                    self.push_ref(
                        class_id,
                        &node_text(type_id, self.source),
                        EdgeKind::Extends,
                        type_id,
                    );
                }
            }
            // tree-sitter.ts:3467-3483 — Swift `class Sub: Base, Proto`:
            // inheritance_specifier > user_type > type_identifier, all
            // emitted as `extends`.
            if child.kind() == "inheritance_specifier" {
                let type_id = child
                    .named_children(&mut child.walk())
                    .find(|c| c.kind() == "user_type")
                    .and_then(|ut| {
                        ut.named_children(&mut ut.walk())
                            .find(|c| c.kind() == "type_identifier")
                    });
                if let Some(type_id) = type_id {
                    self.push_ref(
                        class_id,
                        &node_text(type_id, self.source),
                        EdgeKind::Extends,
                        type_id,
                    );
                }
            }
            if child.kind() == "class_heritage" {
                self.extract_inheritance(child, class_id);
            }
        }
    }

    fn extract_type_annotations(&mut self, node: SyntaxNode<'tree>, node_id: &str) {
        if !matches!(self.spec.language(), Language::TypeScript | Language::Tsx) {
            return;
        }
        if let Some(params) = child_by_field(node, self.spec.params_field()) {
            self.extract_type_refs_from_subtree(params, node_id);
        }
        if let Some(return_type) = child_by_field(node, self.spec.return_field()) {
            self.extract_type_refs_from_subtree(return_type, node_id);
        }
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "type_annotation" {
                self.extract_type_refs_from_subtree(child, node_id);
            }
        }
    }

    fn extract_variable_type_annotation(&mut self, node: SyntaxNode<'tree>, node_id: &str) {
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "type_annotation" {
                self.extract_type_refs_from_subtree(child, node_id);
            }
        }
    }

    fn extract_type_refs_from_subtree(&mut self, node: SyntaxNode<'tree>, from_node_id: &str) {
        if node.kind() == "type_identifier" {
            let type_name = node_text(node, self.source);
            if !is_builtin_type(&type_name) {
                self.push_ref(from_node_id, &type_name, EdgeKind::References, node);
            }
            return;
        }
        for child in node.named_children(&mut node.walk()) {
            self.extract_type_refs_from_subtree(child, from_node_id);
        }
    }

    fn extract_decorators_for(&mut self, decl_node: SyntaxNode<'tree>, decorated_id: &str) {
        for child in decl_node.named_children(&mut decl_node.walk()) {
            self.consider_decorator(child, decorated_id);
            // tree-sitter.ts:2940-2948 — Java/Kotlin/C#/Swift annotations and
            // attributes nest INSIDE a `modifiers` child; descend one level so
            // they are not silently dropped.
            if child.kind() == "modifiers" {
                for inner in child.named_children(&mut child.walk()) {
                    self.consider_decorator(inner, decorated_id);
                }
            }
        }
        let Some(parent) = decl_node.parent() else {
            return;
        };
        let decl_start = decl_node.start_byte();
        let mut decl_index = None;
        for (idx, sibling) in parent.named_children(&mut parent.walk()).enumerate() {
            if sibling.start_byte() == decl_start {
                decl_index = Some(idx);
                break;
            }
        }
        let Some(decl_index) = decl_index else { return };
        let siblings = parent
            .named_children(&mut parent.walk())
            .collect::<Vec<_>>();
        for sibling in siblings[..decl_index].iter().rev() {
            if sibling.kind() != "decorator"
                && sibling.kind() != "annotation"
                && sibling.kind() != "marker_annotation"
            {
                break;
            }
            self.consider_decorator(*sibling, decorated_id);
        }
    }

    fn consider_decorator(&mut self, node: SyntaxNode<'tree>, decorated_id: &str) {
        if !matches!(
            node.kind(),
            "decorator" | "annotation" | "marker_annotation" | "attribute"
        ) {
            return;
        }
        let mut target = None;
        for child in node.named_children(&mut node.walk()) {
            if child.kind() == "call_expression" {
                target = child_by_field(child, "function").or_else(|| child.named_child(0));
                break;
            }
            if matches!(
                child.kind(),
                "identifier"
                    | "member_expression"
                    | "scoped_identifier"
                    | "navigation_expression"
                    | "user_type"
                    | "type_identifier"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let mut name = node_text(target, self.source);
        if let Some(idx) = name.find('<') {
            name.truncate(idx);
        }
        if let Some(idx) = name.rfind(['.', ':']) {
            name = name[idx + 1..].trim_start_matches([':', '.']).to_string();
        }
        let name = name.trim();
        if !name.is_empty() {
            self.push_ref(decorated_id, name, EdgeKind::Decorates, node);
        }
    }

    fn extract_name(&self, node: SyntaxNode<'tree>) -> String {
        if let Some(name) = self.spec.resolve_name(node, self.source) {
            return name;
        }
        if let Some(name_node) = child_by_field(node, self.spec.name_field()) {
            let resolved = unwrap_declarator_name(name_node);
            if resolved.kind() == "dot_index_expression" {
                if let Some(field) = child_by_field(resolved, "field") {
                    return node_text(field, self.source);
                }
            }
            if resolved.kind() == "method_index_expression" {
                if let Some(method) = child_by_field(resolved, "method") {
                    return node_text(method, self.source);
                }
            }
            return node_text(resolved, self.source);
        }
        if node.kind() == "method_signature" {
            for child in node.named_children(&mut node.walk()) {
                if matches!(
                    child.kind(),
                    "function_signature"
                        | "getter_signature"
                        | "setter_signature"
                        | "constructor_signature"
                        | "factory_constructor_signature"
                ) {
                    for inner in child.named_children(&mut child.walk()) {
                        if inner.kind() == "identifier" {
                            return node_text(inner, self.source);
                        }
                    }
                }
            }
        }
        if node.kind() == "arrow_function" || node.kind() == "function_expression" {
            return "<anonymous>".to_string();
        }
        for child in node.named_children(&mut node.walk()) {
            if matches!(
                child.kind(),
                "identifier" | "type_identifier" | "simple_identifier" | "constant"
            ) {
                return node_text(child, self.source);
            }
        }
        "<anonymous>".to_string()
    }

    fn resolve_body(&self, node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        self.spec
            .resolve_body(node, self.spec.body_field())
            .or_else(|| child_by_field(node, self.spec.body_field()))
    }

    fn build_qualified_name(&self, name: &str) -> String {
        let mut parts = Vec::new();
        for node_id in &self.node_stack {
            if let Some(node) = self.nodes.iter().find(|node| &node.id == node_id) {
                if node.kind != NodeKind::File {
                    parts.push(node.name.as_str());
                }
            }
        }
        parts.push(name);
        parts.join("::")
    }

    fn is_inside_class_like_node(&self) -> bool {
        let Some(parent_id) = self.node_stack.last() else {
            return false;
        };
        self.nodes.iter().any(|node| {
            &node.id == parent_id
                && matches!(
                    node.kind,
                    NodeKind::Class
                        | NodeKind::Struct
                        | NodeKind::Interface
                        | NodeKind::Trait
                        | NodeKind::Enum
                        | NodeKind::Module
                )
        })
    }

    fn maybe_capture_fn_refs(&mut self, node: SyntaxNode<'tree>, node_type: &str) {
        let Some(spec) = crate::function_ref::fn_ref_spec(self.spec.language()) else {
            return;
        };
        let Some((mode, field)) = crate::function_ref::dispatch_rule(spec, node_type) else {
            return;
        };
        let Some(from_node_id) = self.node_stack.last().cloned() else {
            return;
        };
        for cand in
            crate::function_ref::capture_fn_ref_candidates(node, mode, field, spec, self.source)
        {
            self.fn_ref_candidates.push((cand, from_node_id.clone()));
        }
    }

    /// Candidates-only scan of a top-level var-initializer subtree, halting at
    /// nested functions. Ports `scanFnRefSubtree` (tree-sitter.ts:392-409).
    fn scan_fn_ref_subtree(&mut self, node: SyntaxNode<'tree>, depth: u32) {
        if crate::function_ref::fn_ref_spec(self.spec.language()).is_none() || depth > 12 {
            return;
        }
        let node_type = node.kind();
        if depth > 0
            && (has_type(self.spec.function_types(), node_type)
                || matches!(
                    node_type,
                    "arrow_function"
                        | "function_expression"
                        | "lambda_literal"
                        | "lambda_expression"
                ))
        {
            return;
        }
        self.maybe_capture_fn_refs(node, node_type);
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                self.scan_fn_ref_subtree(child, depth + 1);
            }
        }
    }

    /// Gate captured function-value candidates and push survivors as
    /// `function_ref` references. A bare-name candidate survives only if its
    /// name is a function/method DEFINED in this file or an imported name;
    /// `this.<member>` candidates always flush (class-scoped at resolution).
    /// Ports `flushFnRefCandidates` (tree-sitter.ts:429-521), TS/JS gate.
    fn flush_fn_ref_candidates(&mut self) {
        if self.fn_ref_candidates.is_empty() {
            return;
        }
        let candidates = std::mem::take(&mut self.fn_ref_candidates);
        let Some(spec) = crate::function_ref::fn_ref_spec(self.spec.language()) else {
            return;
        };
        let address_of_only = crate::function_ref::is_address_of_only(spec);

        let mut defined_here: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for n in &self.nodes {
            if matches!(n.kind, NodeKind::Function | NodeKind::Method) {
                defined_here.insert(n.name.as_str());
            }
        }
        // Import-binding names + last segment of dotted/backslashed (JVM/PHP)
        // imports (tree-sitter.ts:498-507).
        let mut imported_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for r in &self.unresolved_references {
            if r.reference_kind != EdgeKind::Imports || r.reference_name.is_empty() {
                continue;
            }
            if r.reference_name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            {
                imported_names.insert(r.reference_name.clone());
            } else if let Some(last) = r.reference_name.rsplit(['.', '\\']).next().filter(|s| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            }) {
                imported_names.insert(last.to_string());
            }
        }

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pending: Vec<(String, String, i64, i64)> = Vec::new();
        for (cand, from_node_id) in &candidates {
            let at_file_scope = from_node_id.starts_with("file:");
            // C++ (addressOfOnly): a BARE id qualifies only inside a file-scope
            // initializer table; elsewhere only explicit `&`/`Cls::m` forms
            // count (tree-sitter.ts:478-492).
            if address_of_only
                && !cand.explicit_ref
                && !(at_file_scope
                    && matches!(
                        cand.mode,
                        crate::function_ref::CaptureMode::Value
                            | crate::function_ref::CaptureMode::List
                    ))
            {
                continue;
            }
            // Gate by candidate shape: `this.`/`::` always flush; C-family
            // file-scope initializers (ungated_modes) skip; PHP HOF strings
            // (skip_gate) skip; everything else must be a same-file fn/method
            // or an import (tree-sitter.ts:493-512).
            if !cand.name.starts_with("this.") && !cand.name.contains("::") {
                let skip_gate = (at_file_scope
                    && crate::function_ref::mode_is_ungated(spec, cand.mode))
                    || cand.skip_gate;
                if !skip_gate
                    && !defined_here.contains(cand.name.as_str())
                    && !imported_names.contains(cand.name.as_str())
                {
                    continue;
                }
            }
            let key = format!("{from_node_id}|{}", cand.name);
            if !seen.insert(key) {
                continue;
            }
            pending.push((
                from_node_id.clone(),
                cand.name.clone(),
                cand.line,
                cand.column,
            ));
        }
        for (from_node_id, name, line, col) in pending {
            self.unresolved_references.push(UnresolvedRef {
                id: None,
                from_node_id,
                reference_name: name,
                reference_kind: EdgeKind::References,
                line,
                col,
                candidates: None,
                file_path: self.file_path.to_string(),
                language: self.spec.language(),
                is_function_ref: true,
            });
        }
    }

    fn push_ref(
        &mut self,
        from_node_id: &str,
        reference_name: &str,
        kind: EdgeKind,
        node: SyntaxNode<'tree>,
    ) {
        self.unresolved_references.push(UnresolvedRef {
            id: None,
            from_node_id: from_node_id.to_string(),
            reference_name: reference_name.to_string(),
            reference_kind: kind,
            line: node.start_position().row as i64 + 1,
            col: node.start_position().column as i64,
            candidates: None,
            file_path: self.file_path.to_string(),
            language: self.spec.language(),
            is_function_ref: false,
        });
    }

    fn preceding_docstring(&self, node: SyntaxNode<'tree>) -> Option<String> {
        // Climb out of any wrapper(s) so a comment preceding the WHOLE construct
        // (export-, decorator-, or const-arrow-wrapped) is reachable as a
        // sibling; the inner node's own prev_named_sibling is empty in those
        // cases. Ports getPrecedingDocstring (tree-sitter-helpers.ts:101-103,
        // upstream #780 / 0df92467).
        let mut anchor = node;
        while let Some(parent) = anchor.parent() {
            if is_docstring_wrapper_type(parent.kind()) {
                anchor = parent;
            } else {
                break;
            }
        }
        let mut sibling = anchor.prev_named_sibling();
        let mut comments = Vec::new();
        while let Some(current) = sibling {
            if matches!(
                current.kind(),
                "comment" | "line_comment" | "block_comment" | "documentation_comment"
            ) {
                comments.push(clean_comment(&node_text(current, self.source)));
                sibling = current.prev_named_sibling();
            } else {
                break;
            }
        }
        if comments.is_empty() {
            return None;
        }
        comments.reverse();
        Some(comments.join("\n").trim().to_string())
    }

    fn extract_rust_impl_item(&mut self, node: SyntaxNode<'tree>) {
        let mut type_nodes = node
            .named_children(&mut node.walk())
            .filter(|child| child.kind() == "type_identifier")
            .collect::<Vec<_>>();
        if type_nodes.len() < 2 {
            return;
        }
        let Some(target) = type_nodes.pop() else {
            return;
        };
        let Some(trait_node) = type_nodes.first().copied() else {
            return;
        };
        let target_name = node_text(target, self.source);
        let trait_name = node_text(trait_node, self.source);
        if let Some(owner_id) = self
            .nodes
            .iter()
            .find(|n| n.file_path == self.file_path && n.name == target_name)
            .map(|n| n.id.clone())
        {
            self.push_ref(&owner_id, &trait_name, EdgeKind::Implements, trait_node);
        }
    }

    fn extract_jsx_component_ref(&mut self, node: SyntaxNode<'tree>) {
        if !matches!(self.spec.language(), Language::Tsx | Language::Jsx) {
            return;
        }
        let Some(from_id) = self.node_stack.last().cloned() else {
            return;
        };
        let tag = first_descendant_matching(node, |kind| {
            matches!(
                kind,
                "identifier" | "nested_identifier" | "member_expression"
            )
        });
        let Some(tag) = tag else { return };
        let name = node_text(tag, self.source);
        let component = name.rsplit('.').next().unwrap_or(name.as_str()).trim();
        if component
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_uppercase())
        {
            self.push_ref(&from_id, component, EdgeKind::References, tag);
        }
    }
}

#[derive(Default)]
struct NodeExtra {
    docstring: Option<String>,
    signature: Option<String>,
    visibility: Option<String>,
    is_exported: bool,
    is_async: bool,
    is_static: bool,
    is_abstract: bool,
    decorators: Vec<String>,
    type_parameters: Vec<String>,
    return_type: Option<String>,
    qualified_name: Option<String>,
}

/// The call's callee name when it is a bare identifier or `pkg::fn` (yields
/// `fn`). Ports `calleeName` (r.ts:44-54).
fn r_callee_name(call: SyntaxNode<'_>, source: &str) -> Option<String> {
    let f = child_by_field(call, "function")?;
    match f.kind() {
        "identifier" => Some(node_text(f, source)),
        "namespace_operator" => child_by_field(f, "rhs").map(|rhs| node_text(rhs, source)),
        _ => None,
    }
}

/// First positional argument's value node of a call. Ports `firstArgValue`.
fn r_first_arg_value(call: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let args = child_by_field(call, "arguments")?;
    args.named_children(&mut args.walk())
        .find(|a| a.kind() == "argument")
        .and_then(|a| child_by_field(a, "value"))
}

/// Text of a string node's content, or an identifier's text. Ports
/// `literalOrIdentifier` (r.ts:70-82).
fn r_literal_or_identifier(node: Option<SyntaxNode<'_>>, source: &str) -> Option<String> {
    let node = node?;
    match node.kind() {
        "identifier" => Some(node_text(node, source)),
        "string" => Some(
            node.named_children(&mut node.walk())
                .find(|c| c.kind() == "string_content")
                .map(|c| node_text(c, source))
                .unwrap_or_default(),
        ),
        _ => None,
    }
}

/// ALL_CAPS or DOTTED.CAPS top-level assignment names a constant (r.ts:42).
fn is_r_constant_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.' || c == '_')
}

fn is_docstring_wrapper_type(kind: &str) -> bool {
    matches!(
        kind,
        "export_statement"
            | "decorated_definition"
            | "lexical_declaration"
            | "variable_declaration"
            | "variable_declarator"
            | "ambient_declaration"
    )
}

fn clean_comment(comment: &str) -> String {
    use std::sync::OnceLock;

    static BLOCK_C_OPEN: OnceLock<Regex> = OnceLock::new();
    static BLOCK_C_CLOSE: OnceLock<Regex> = OnceLock::new();
    static BLOCK_LUA_OPEN: OnceLock<Regex> = OnceLock::new();
    static BLOCK_LUA_CLOSE: OnceLock<Regex> = OnceLock::new();
    static LINE_SLASH: OnceLock<Regex> = OnceLock::new();
    static LINE_DASH: OnceLock<Regex> = OnceLock::new();
    static LINE_HASH: OnceLock<Regex> = OnceLock::new();
    static LINE_STAR: OnceLock<Regex> = OnceLock::new();

    let mut c = comment.trim().to_string();
    if c.starts_with("/*") {
        c = BLOCK_C_OPEN
            .get_or_init(|| Regex::new(r"^/\*+!?").expect("block-c-open regex"))
            .replace(&c, "")
            .into_owned();
        c = BLOCK_C_CLOSE
            .get_or_init(|| Regex::new(r"\*+/$").expect("block-c-close regex"))
            .replace(&c, "")
            .into_owned();
    } else if c.starts_with("--[") {
        c = BLOCK_LUA_OPEN
            .get_or_init(|| Regex::new(r"^--\[=*\[").expect("block-lua-open regex"))
            .replace(&c, "")
            .into_owned();
        c = BLOCK_LUA_CLOSE
            .get_or_init(|| Regex::new(r"\]=*\]$").expect("block-lua-close regex"))
            .replace(&c, "")
            .into_owned();
    } else if c.starts_with("(*") {
        c = c
            .strip_prefix("(*")
            .unwrap_or(&c)
            .strip_suffix("*)")
            .map_or_else(|| c.trim_start_matches("(*").to_string(), str::to_string);
    } else if c.starts_with('{') {
        c = c
            .strip_prefix('{')
            .unwrap_or(&c)
            .strip_suffix('}')
            .map_or_else(|| c.trim_start_matches('{').to_string(), str::to_string);
    }

    let c = LINE_SLASH
        .get_or_init(|| Regex::new(r"(?m)^//[/!]?\s?").expect("line-slash regex"))
        .replace_all(&c, "");
    let c = LINE_DASH
        .get_or_init(|| Regex::new(r"(?m)^--\s?").expect("line-dash regex"))
        .replace_all(&c, "");
    let c = LINE_HASH
        .get_or_init(|| Regex::new(r"(?m)^#\s?").expect("line-hash regex"))
        .replace_all(&c, "");
    let c = LINE_STAR
        .get_or_init(|| Regex::new(r"(?m)^\s*\*\s?").expect("line-star regex"))
        .replace_all(&c, "");
    c.trim().to_string()
}

fn unwrap_declarator_name<'tree>(node: SyntaxNode<'tree>) -> SyntaxNode<'tree> {
    let mut resolved = node;
    while matches!(
        resolved.kind(),
        "pointer_declarator" | "reference_declarator"
    ) {
        let Some(inner) =
            child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0))
        else {
            break;
        };
        resolved = inner;
    }
    if matches!(resolved.kind(), "function_declarator" | "declarator") {
        if let Some(inner) =
            child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0))
        {
            return unwrap_declarator_name(inner);
        }
    }
    resolved
}

fn field_declarators(node: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    let direct = node
        .named_children(&mut node.walk())
        .filter(|child| child.kind() == "variable_declarator")
        .collect::<Vec<_>>();
    let wrapped = node
        .named_children(&mut node.walk())
        .filter(|child| child.kind() == "variable_declaration")
        .flat_map(|decl| {
            decl.named_children(&mut decl.walk())
                .filter(|child| child.kind() == "variable_declarator")
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    direct.into_iter().chain(wrapped)
}

fn property_or_field_signature(node: SyntaxNode<'_>, name: &str, source: &str) -> Option<String> {
    if matches!(node.kind(), "public_field_definition" | "field_definition") {
        return match child_by_field(node, "type") {
            Some(type_node) => {
                let type_text = node_text(type_node, source);
                let type_text = type_text.trim_start_matches(':').trim_start();
                Some(format!("{type_text} {name}"))
            }
            None => Some(name.to_string()),
        };
    }
    let type_search = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "variable_declaration")
        .unwrap_or(node);
    let type_node = type_search
        .named_children(&mut type_search.walk())
        .find(|child| {
            !matches!(
                child.kind(),
                "modifiers"
                    | "modifier"
                    | "visibility_modifier"
                    | "static_modifier"
                    | "readonly_modifier"
                    | "var_modifier"
                    | "identifier"
                    | "name"
                    | "variable_declarator"
                    | "variable_declaration"
                    | "property_element"
                    | "accessor_list"
                    | "accessors"
                    | "equals_value_clause"
                    | "marker_annotation"
                    | "annotation"
            )
        });
    type_node.map(|node| format!("{} {name}", node_text(node, source)))
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "string"
            | "number"
            | "boolean"
            | "void"
            | "null"
            | "undefined"
            | "never"
            | "any"
            | "unknown"
            | "object"
            | "symbol"
            | "bigint"
            | "Int"
            | "Long"
            | "Short"
            | "Byte"
            | "Float"
            | "Double"
            | "Boolean"
            | "Char"
            | "Unit"
            | "String"
            | "Any"
            | "AnyRef"
            | "AnyVal"
            | "Nothing"
            | "Null"
            | "true"
            | "false"
    )
}

fn first_descendant_kind<'tree>(node: SyntaxNode<'tree>, kind: &str) -> Option<SyntaxNode<'tree>> {
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

fn first_descendant_matching<'tree>(
    node: SyntaxNode<'tree>,
    predicate: impl Fn(&str) -> bool + Copy,
) -> Option<SyntaxNode<'tree>> {
    for child in node.named_children(&mut node.walk()) {
        if predicate(child.kind()) {
            return Some(child);
        }
        if let Some(found) = first_descendant_matching(child, predicate) {
            return Some(found);
        }
    }
    None
}

fn is_jsx_element_kind(kind: &str) -> bool {
    matches!(kind, "jsx_element" | "jsx_self_closing_element")
}

fn is_rust_impl_item(language: Language, kind: &str) -> bool {
    language == Language::Rust && kind == "impl_item"
}

fn normalize_parenthesized_go_conversion(name: &str) -> Option<String> {
    let trimmed = name.trim();
    let inner = trimmed.strip_prefix('(')?.strip_suffix(')')?.trim();
    let clean = inner.trim_start_matches('*').trim();
    if clean
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && clean
            .chars()
            .all(|c| c == '_' || c == '.' || c.is_ascii_alphanumeric())
    {
        Some(clean.to_string())
    } else {
        None
    }
}

fn collect_rust_use_paths<'tree>(
    node: SyntaxNode<'tree>,
    prefix: &str,
    source: &str,
    out: &mut Vec<(String, SyntaxNode<'tree>)>,
) {
    let join = |prefix: &str, segment: &str| {
        if prefix.is_empty() {
            segment.to_string()
        } else {
            format!("{prefix}::{segment}")
        }
    };
    match node.kind() {
        "identifier" => out.push((join(prefix, &node_text(node, source)), node)),
        "scoped_identifier" => {
            let full = node_text(node, source).trim().to_string();
            out.push((join(prefix, &full), node));
        }
        "scoped_use_list" => {
            let segment = child_by_field(node, "path")
                .map(|path| node_text(path, source).trim().to_string())
                .unwrap_or_default();
            let next_prefix = if segment.is_empty() {
                prefix.to_string()
            } else {
                join(prefix, &segment)
            };
            if let Some(list) = child_by_field(node, "list").or_else(|| {
                node.named_children(&mut node.walk())
                    .find(|c| c.kind() == "use_list")
            }) {
                collect_rust_use_paths(list, &next_prefix, source, out);
            }
        }
        "use_list" => {
            for child in node.named_children(&mut node.walk()) {
                collect_rust_use_paths(child, prefix, source, out);
            }
        }
        "use_as_clause" => {
            if let Some(path) = child_by_field(node, "path").or_else(|| node.named_child(0)) {
                collect_rust_use_paths(path, prefix, source, out);
            }
        }
        _ => {
            for child in node.named_children(&mut node.walk()) {
                collect_rust_use_paths(child, prefix, source, out);
            }
        }
    }
}

fn descendants_of_kind<'tree>(node: SyntaxNode<'tree>, kind: &str) -> Vec<SyntaxNode<'tree>> {
    let mut out = Vec::new();
    collect_descendants_of_kind(node, kind, &mut out);
    out
}

fn collect_descendants_of_kind<'tree>(
    node: SyntaxNode<'tree>,
    kind: &str,
    out: &mut Vec<SyntaxNode<'tree>>,
) {
    for child in node.named_children(&mut node.walk()) {
        if child.kind() == kind {
            out.push(child);
        }
        collect_descendants_of_kind(child, kind, out);
    }
}

fn lua_require_module(call_node: SyntaxNode<'_>, source: &str) -> Option<String> {
    let name =
        child_by_field(call_node, "name").or_else(|| child_by_field(call_node, "function"))?;
    if name.kind() != "identifier" || node_text(name, source) != "require" {
        return None;
    }
    let args = child_by_field(call_node, "arguments").or_else(|| call_node.named_child(1))?;
    if let Some(content) = first_descendant_kind(args, "string_content") {
        let module_name = node_text(content, source).trim().to_string();
        if !module_name.is_empty() {
            return Some(module_name);
        }
    }
    if let Some(string_node) = first_descendant_kind(args, "string") {
        let module_name = node_text(string_node, source)
            .trim()
            .trim_start_matches("[[")
            .trim_end_matches("]]")
            .trim_matches(['\'', '"'])
            .to_string();
        if !module_name.is_empty() {
            return Some(module_name);
        }
    }
    let index = first_descendant_kind(args, "dot_index_expression")
        .or_else(|| first_descendant_kind(args, "method_index_expression"));
    if let Some(index) = index {
        let field = child_by_field(index, "field").or_else(|| child_by_field(index, "method"));
        if let Some(field) = field {
            let module_name = node_text(field, source).trim().to_string();
            if !module_name.is_empty() {
                return Some(module_name);
            }
        }
    }
    None
}
