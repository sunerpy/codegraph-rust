//! Nix (`.nix`) `LanguageSpec`.
//!
//! Extraction-tier port of `upstream extraction/languages/nix.ts` (commit
//! `7f32513`, #1190 — the extraction slice only). Nix is an expression-based
//! language with no C-family `class`/`struct`/`method`/`enum`/`interface` node
//! kinds; its "symbols" are `binding`s (an attrpath name + a value) and lambda
//! `function_expression`s, and a "call" is an `apply_expression` (`f a b`), not
//! a `call_expression`. So — faithful to upstream's entirely empty
//! `nixExtractor` config — [`NixSpec`] returns `&[]` for every type-set and the
//! extraction is driven entirely by the `Language::Nix`-guarded
//! `visit_nix_node` walker extension in [`crate::walker`].
//!
//! The module-system option-path synthesizer, lexical-scope resolution gates,
//! callback synthesizer, and import-resolver module-list wiring that upstream
//! bundles with the same commit are DEFERRED — the port has no
//! callback-synthesis / option-wiring subsystem.
//!
//! The pure AST helper fns below are `pub(crate)` and re-exported through
//! [`crate::lang`] so the walker can reach them as `crate::lang::<fn>`.

use codegraph_core::types::Language;
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::LanguageSpec;
use crate::walker::node_text;

pub struct NixSpec;

pub static NIX_SPEC: NixSpec = NixSpec;

impl LanguageSpec for NixSpec {
    fn language(&self) -> Language {
        Language::Nix
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_nix::LANGUAGE.into()
    }

    fn function_types(&self) -> &'static [&'static str] {
        &[]
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
        &[]
    }

    fn call_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn variable_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn name_field(&self) -> &'static str {
        ""
    }

    fn body_field(&self) -> &'static str {
        ""
    }

    fn params_field(&self) -> &'static str {
        ""
    }

    fn return_field(&self) -> &'static str {
        ""
    }
}

/// `variable_expression` → its first named child (the `identifier`); any other
/// node is returned unchanged. Ports `unwrapVariableExpression` (nix.ts:5-8).
pub(crate) fn unwrap_variable_expression(node: Node<'_>) -> Node<'_> {
    if node.kind() != "variable_expression" {
        return node;
    }
    node.named_child(0).unwrap_or(node)
}

/// Callee name of an `apply_expression`: unroll the `function` field to the
/// innermost node, unwrap a `variable_expression`, and return the text of an
/// `identifier` / `select_expression`. Ports `getCalleeName` (nix.ts:10-22).
pub(crate) fn nix_callee_name(node: Node<'_>, source: &str) -> Option<String> {
    let mut current = node;
    while current.kind() == "apply_expression" {
        let Some(func) = current
            .child_by_field_name("function")
            .or_else(|| current.named_child(0))
        else {
            break;
        };
        current = func;
    }
    current = unwrap_variable_expression(current);
    if matches!(current.kind(), "identifier" | "select_expression") {
        return Some(node_text(current, source).trim().to_string());
    }
    None
}

/// Direct callee: the `function` field (or first named child), unwrapped, its
/// text. Ports `getDirectCalleeName` (nix.ts:24-29).
pub(crate) fn nix_direct_callee_name(node: Node<'_>, source: &str) -> Option<String> {
    let func = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))?;
    let func = unwrap_variable_expression(func);
    Some(node_text(func, source).trim().to_string())
}

/// A `./` or `../` project-relative path with no shell/interpolation
/// metacharacters. Ports `isStaticProjectPath` (nix.ts:31-36).
pub(crate) fn is_static_project_path(value: &str) -> bool {
    (value.starts_with("./") || value.starts_with("../"))
        && !value
            .chars()
            .any(|c| c.is_whitespace() || "{}()[];\"'<>$".contains(c))
}

/// Unwrap `parenthesized_expression`, strip matching quotes, gate on
/// [`is_static_project_path`]. Ports `getStaticImportPath` (nix.ts:38-56).
pub(crate) fn nix_static_import_path(arg: Node<'_>, source: &str) -> Option<String> {
    let mut current = arg;
    while current.kind() == "parenthesized_expression" {
        let Some(inner) = current.named_child(0) else {
            break;
        };
        current = inner;
    }

    let mut text = node_text(current, source).trim().to_string();
    let bytes = text.as_bytes();
    if text.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        text = text[1..text.len() - 1].to_string();
    }

    if is_static_project_path(&text) {
        Some(text)
    } else {
        None
    }
}

/// True when `node` is a member of a RETURNED attrset (not a `let` binding, a
/// nested `binding` value, or a lambda `formals` parameter). Parent walk per
/// upstream `isReturnedAttrsetMember` (nix.ts:58-87).
pub(crate) fn is_returned_attrset_member(node: Node<'_>) -> bool {
    let mut current = node;
    let mut seen_returned_attrset = false;

    while let Some(parent) = current.parent() {
        if parent.kind() == "let_expression" {
            let body = parent
                .child_by_field_name("body")
                .or_else(|| parent.child_by_field_name("expression"));
            match body {
                Some(body) if body == current => {}
                _ => return false,
            }
        }

        if parent.kind() == "binding" && current != node {
            return false;
        }
        if matches!(parent.kind(), "formal_parameters" | "formals") {
            return false;
        }

        if matches!(
            parent.kind(),
            "attrset" | "rec_attrset" | "attrset_expression" | "rec_attrset_expression"
        ) {
            seen_returned_attrset = true;
        }

        current = parent;
    }

    seen_returned_attrset
}

/// Unroll nested `function_expression`s, collecting the source-slice param text
/// before each body start; return `(params, body)`. Ports
/// `getCurriedParamsAndBody` (nix.ts:89-112).
pub(crate) fn nix_curried_params_and_body<'t>(
    node: Node<'t>,
    source: &str,
) -> (Vec<String>, Option<Node<'t>>) {
    let mut params: Vec<String> = Vec::new();
    let mut current = node;

    while current.kind() == "function_expression" && current.named_child_count() > 0 {
        let Some(body) = current.named_child(current.named_child_count() as u32 - 1) else {
            break;
        };

        let start = current.start_byte();
        let end = body.start_byte();
        let param_part = source.get(start..end).unwrap_or_default().trim();
        let param_text = param_part
            .strip_suffix(':')
            .map(str::trim)
            .unwrap_or(param_part);
        if !param_text.is_empty() {
            params.push(param_text.to_string());
        }

        if body.kind() == "function_expression" {
            current = body;
        } else {
            return (params, Some(body));
        }
    }

    let body = if current.named_child_count() > 0 {
        current.named_child(current.named_child_count() as u32 - 1)
    } else {
        None
    };
    (params, body)
}

/// Format a curried-lambda signature. Ports `formatFunctionSignature`
/// (nix.ts:114-121): `()` for none, `p : q` for ≥2, and `(p)` for a single bare
/// param (unless it already starts `(` or contains `{`/`@`).
pub(crate) fn format_function_signature(params: &[String]) -> String {
    if params.is_empty() {
        return "()".to_string();
    }
    if params.len() > 1 {
        return params.join(" : ");
    }
    let param = &params[0];
    if param.is_empty() {
        return "()".to_string();
    }
    if param.starts_with('(') || param.contains('{') || param.contains('@') {
        param.clone()
    } else {
        format!("({param})")
    }
}

/// The `inherited_attrs` named child of an `inherit` / `inherit_from`. Ports
/// `inheritedAttrs` (nix.ts:123-125).
pub(crate) fn nix_inherited_attrs(node: Node<'_>) -> Option<Node<'_>> {
    node.named_children(&mut node.walk())
        .find(|child| child.kind() == "inherited_attrs")
}

/// `callPackage`/`callPackages` or a `.callPackage`/`.callPackages` suffix (the
/// nixpkgs auto-wiring idiom). Ports `isCallPackageName` (nix.ts:131-138).
pub(crate) fn is_callpackage_name(name: &str) -> bool {
    name == "callPackage"
        || name == "callPackages"
        || name.ends_with(".callPackage")
        || name.ends_with(".callPackages")
}

/// Innermost `argument` of a curried apply chain (`f a b` → `a`). Ports
/// `getFirstApplyArgument` (nix.ts:141-152).
pub(crate) fn nix_first_apply_argument(node: Node<'_>) -> Option<Node<'_>> {
    let mut inner = node;
    loop {
        let fnn = inner
            .child_by_field_name("function")
            .or_else(|| inner.named_child(0));
        match fnn {
            Some(f) if f.kind() == "apply_expression" => {
                inner = f;
            }
            _ => break,
        }
    }
    inner
        .child_by_field_name("argument")
        .or_else(|| inner.named_child(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_nix::LANGUAGE.into())
            .unwrap();
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
    fn nix_spec_has_empty_type_sets() {
        assert!(NIX_SPEC.function_types().is_empty());
        assert!(NIX_SPEC.class_types().is_empty());
        assert!(NIX_SPEC.method_types().is_empty());
        assert!(NIX_SPEC.call_types().is_empty());
        assert!(NIX_SPEC.variable_types().is_empty());
        assert!(NIX_SPEC.import_types().is_empty());
        assert_eq!(NIX_SPEC.language(), Language::Nix);
    }

    #[test]
    fn nix_format_function_signature() {
        assert_eq!(format_function_signature(&[]), "()");
        assert_eq!(format_function_signature(&["pkgs".to_string()]), "(pkgs)");
        assert_eq!(
            format_function_signature(&["{ pkgs }".to_string()]),
            "{ pkgs }"
        );
        assert_eq!(
            format_function_signature(&["a".to_string(), "b".to_string()]),
            "a : b"
        );
    }

    #[test]
    fn is_static_project_path_gate() {
        assert!(is_static_project_path("./foo.nix"));
        assert!(is_static_project_path("../common/base.nix"));
        assert!(!is_static_project_path("nixpkgs"));
        assert!(!is_static_project_path("./a b"));
        assert!(!is_static_project_path("./${x}.nix"));
    }

    #[test]
    fn nix_parses_lambda_and_binding() {
        let tree = parse("{ pkgs }: let x = 1; in x");
        let root = tree.root_node();
        assert!(
            first_of_kind(root, "function_expression").is_some(),
            "tree-sitter-nix parses a lambda"
        );
        assert!(
            first_of_kind(root, "binding").is_some(),
            "tree-sitter-nix parses a let binding"
        );
    }
}
