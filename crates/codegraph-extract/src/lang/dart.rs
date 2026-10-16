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
        // `part 'x.g.dart';` is a library naming one of its own files — a
        // generated part, often. Its URI resolves as an import's does, so the
        // library links the part's file the way it links a file it imports,
        // and a change to the part reaches whatever depends on the library.
        // `part of` adds nothing to that edge, and stays out (upstream #2391).
        &["import_or_export", "part_directive"]
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
        // KEEP-RUST: this grammar wraps the return type in the signature's
        // `return_type` field, a `type` whose own `type_identifier`s are the
        // name and any import prefix before it (`p.Foo<T>?` holds `p`, `Foo`
        // and the type arguments). The last of them is the type upstream
        // names; a function, record or `void` type names none.
        if let Some(written) = child_by_field(sig, "return_type") {
            let last = written
                .named_children(&mut written.walk())
                .filter(|child| child.kind() == "type_identifier")
                .last()?;
            let name = node_text(last, source);
            return is_ident(&name).then_some(name);
        }
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
        // KEEP-RUST: a member with a body is a `method_declaration` here,
        // its `static` in the `method_signature` it holds; upstream's grammar
        // makes that signature the member itself.
        let signature = if node.kind() == "method_declaration" {
            child_by_field(node, "signature").unwrap_or(node)
        } else {
            node
        };
        signature.kind() == "method_signature"
            && (0..signature.child_count())
                .any(|i| signature.child(i).is_some_and(|c| c.kind() == "static"))
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
        // A part holds its `uri` directly; an import or export inside its
        // `library_import` / `library_export`.
        let holder = if node.kind() == "part_directive" {
            node.child_by_field_name("uri")
        } else {
            node.named_children(&mut node.walk())
                .find(|child| matches!(child.kind(), "library_import" | "library_export"))
        };
        let module_name = holder
            .and_then(|holder| first_descendant_kind(holder, "string_literal"))
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
            return dart_constructed_callee(node, source);
        }
        None
    }
}

/// The class, and the named constructor, a `new` or `const` constructor call
/// names (`EdgeInsets.all` for `const EdgeInsets.all(8)`). KEEP-RUST: this
/// grammar writes them in the call's `type` child, a named constructor as a
/// second `type_identifier` there (`Foo.named`), so `p.Foo` (an import
/// prefix) and `Foo.named` read alike; Dart writes types UpperCamel and
/// prefixes and constructors not, which tells them apart. Behind a prefix,
/// the constructor is the call's `constructor` field (`const p.Foo.named()`).
pub(crate) fn dart_constructed_callee(node: Node<'_>, source: &str) -> Option<String> {
    let ty = node.child_by_field_name("type")?;
    let ids: Vec<String> = ty
        .named_children(&mut ty.walk())
        .filter(|child| child.kind() == "type_identifier")
        .map(|child| node_text(child, source))
        .collect();
    let constructor = node
        .child_by_field_name("constructor")
        .map(|name| node_text(name, source));
    let (class, named) = match (ids.as_slice(), constructor) {
        ([.., class], Some(named)) => (class.clone(), Some(named)),
        ([class], None) => (class.clone(), None),
        ([prefix_or_class, second], None) => {
            if dart_writes_a_type(second) {
                (second.clone(), None)
            } else {
                (prefix_or_class.clone(), Some(second.clone()))
            }
        }
        ([_, class, named, ..], None) => (class.clone(), Some(named.clone())),
        ([], _) => return None,
    };
    Some(match named {
        Some(named) => format!("{class}.{named}"),
        None => class,
    })
}

/// Whether a Dart name is written as a type is: UpperCamel, past any leading
/// `_` or `$` (`^[_$]*[A-Z]`).
pub(crate) fn dart_writes_a_type(name: &str) -> bool {
    name.trim_start_matches(['_', '$'])
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// The expression an arrow function's body ends with, when this grammar ended
/// the arrow in front of a call or member access on that expression
/// (KEEP-RUST, the crate grammar's misparse; upstream #2375 reads a receiver
/// past the arrow the same way): `(ref) => createDio()` parses as a call of
/// `(ref) => createDio`, and `(ref) => ref.read(p)` as `.read(p)` on
/// `(ref) => ref`. Dart writes a call or member access on a function literal
/// itself in parentheses, so a function expression in that place is never
/// the literal. Any other node is itself.
pub(crate) fn dart_arrow_tail(node: Node<'_>) -> Node<'_> {
    if node.kind() != "function_expression" {
        return node;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return node;
    };
    if body.kind() != "function_expression_body" {
        return node;
    }
    match body.named_children(&mut body.walk()).last() {
        Some(tail) if tail.kind() != "block" => tail,
        _ => node,
    }
}

/// Where a Dart call whose callee runs through a misparsed arrow function
/// starts in the code as written (see [`dart_arrow_tail`]): the start of the
/// arrow's tail expression, `createDio` in `(ref) => createDio()` and `ref`
/// in `(ref) => ref.watch(p).select(s)`. `None` for any other call.
pub(crate) fn dart_misparsed_call_start(call: Node<'_>) -> Option<Node<'_>> {
    let mut current = call;
    loop {
        let next = match current.kind() {
            "call_expression" | "instantiation_expression" => {
                current.child_by_field_name("function")
            }
            "member_expression" | "null_aware_member_expression" => {
                current.child_by_field_name("object")
            }
            "function_expression" => {
                let tail = dart_arrow_tail(current);
                return (tail.id() != current.id()).then_some(tail);
            }
            _ => None,
        };
        current = next?;
    }
}

/// The bodies a Dart field `declaration` sits in: a class's or mixin's, an
/// extension's, an enum's (upstream `DART_MEMBER_BODIES`).
fn is_dart_member_body(kind: &str) -> bool {
    matches!(kind, "class_body" | "extension_body" | "enum_body")
}

/// Whether a Dart `declaration` declares fields of the type around it
/// (`final Report report;`, `static const zero = Score(0);`): it sits in a
/// member body, inside the `class_member` this grammar wraps it in, and
/// names variables rather than a constructor or an abstract method.
pub(crate) fn is_dart_field_declaration(node: Node<'_>) -> bool {
    if node.kind() != "declaration" {
        return false;
    }
    let body = match node.parent() {
        Some(parent) if parent.kind() == "class_member" => parent.parent(),
        parent => parent,
    };
    body.is_some_and(|body| is_dart_member_body(body.kind()))
        && node.named_children(&mut node.walk()).any(|child| {
            matches!(
                child.kind(),
                "initialized_identifier_list" | "static_final_declaration_list"
            )
        })
}

/// Whether an `initialized_identifier` declares a field or a top-level
/// variable (`var cache = load();`, `final _ctl = TextEditingController();`
/// in a class), rather than the second variable of a local declaration
/// (`for (var i = 0, j = n(); …)`), which a function body's walk covers
/// (upstream #2367 `isDartFieldOrTopLevelEntry`).
pub(crate) fn is_dart_field_or_top_level_entry(node: Node<'_>) -> bool {
    let Some(list) = node.parent() else {
        return false;
    };
    if list.kind() != "initialized_identifier_list" {
        return false;
    }
    list.parent().is_some_and(|owner| {
        owner.kind() == "top_level_variable_declaration" || is_dart_field_declaration(owner)
    })
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
