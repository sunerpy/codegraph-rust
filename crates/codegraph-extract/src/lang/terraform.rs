//! Terraform / OpenTofu (`.tf` / `.tfvars` / `.tofu`) `LanguageSpec`.
//!
//! Port of `upstream extraction/languages/terraform.ts` (#1173, #1174,
//! behaviour as of `b635dd46`). HCL is intentionally generic:
//! ALL Terraform top-level constructs share the AST node kind `block`,
//! distinguished only by the first `identifier` child (the block "type":
//! `resource` / `data` / `module` / `variable` / `output` / `provider` /
//! `locals` / `terraform`). There are no C-family `class`/`struct`/`method`/
//! `enum`/`interface`/`function` node kinds, so — faithful to upstream's
//! entirely empty `terraformExtractor` config — [`TerraformSpec`] returns `&[]`
//! for every type-set and the extraction is driven entirely by the
//! `Language::Terraform`-guarded `visit_terraform_node` walker extension in
//! [`crate::walker`].
//!
//! Besides the declarations and their references, the walker emits what only
//! the Terraform framework resolver (`codegraph_resolve::frameworks::terraform`)
//! reads: a module block's `:`-scoped wiring (`module.M:file`,
//! `module.M:var.X`), a module chain's `module.M:output.O` and
//! `module.M:remote-output.X`, a `.tfvars` assignment's `var.X`, a provider
//! selection's `provider.P[.alias]`, and the state-migration and check-assert
//! blocks' references from the file. A reference is where its variable is
//! written (`variable_expr`), as upstream records it.
//!
//! The pure AST helper fns below are `pub(crate)` and re-exported through
//! [`crate::lang`] so the walker can reach them as `crate::lang::<fn>`.

use codegraph_core::types::{Language, NodeKind};
use tree_sitter::{Language as TsLanguage, Node};

use crate::spec::LanguageSpec;
use crate::walker::node_text;

pub struct TerraformSpec;

pub static TERRAFORM_SPEC: TerraformSpec = TerraformSpec;

impl LanguageSpec for TerraformSpec {
    fn language(&self) -> Language {
        Language::Terraform
    }

    fn tree_sitter_language(&self) -> TsLanguage {
        tree_sitter_hcl::LANGUAGE.into()
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

/// Built-in reference heads that should NOT be resolved to project nodes.
/// Ports `BUILTIN_HEADS` (terraform.ts:37-43).
const BUILTIN_HEADS: [&str; 5] = ["each", "count", "self", "path", "terraform"];

/// Bare strings never treated as references. Ports `BUILTIN_KEYWORDS`
/// (terraform.ts:46).
const BUILTIN_KEYWORDS: [&str; 3] = ["null", "true", "false"];

/// A described top-level Terraform block: its `NodeKind`, symbol name, qualified
/// name, and signature. Ports the `BlockDecl` interface (terraform.ts:361-366).
pub(crate) struct TerraformBlockDecl {
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub signature: String,
}

/// Read a `string_lit` value (its `template_literal` named child text), else
/// `""` (an empty `""` string parses with no `template_literal`). Ports
/// `stringLitValue` (terraform.ts:49-55).
pub(crate) fn terraform_string_lit_value(node: Node<'_>, source: &str) -> String {
    node.named_children(&mut node.walk())
        .find(|c| c.kind() == "template_literal")
        .map(|literal| node_text(literal, source))
        .unwrap_or_default()
}

/// Block "type" and its label values. `None` if the block is malformed (first
/// named child is not an `identifier`). Ports `readBlockHeader`
/// (terraform.ts:58-79).
pub(crate) fn read_terraform_block_header(
    block: Node<'_>,
    source: &str,
) -> Option<(String, Vec<String>)> {
    let named: Vec<Node<'_>> = block.named_children(&mut block.walk()).collect();
    let first = named.first()?;
    if first.kind() != "identifier" {
        return None;
    }
    let block_type = node_text(*first, source);
    let mut labels: Vec<String> = Vec::new();
    for child in named.iter().skip(1) {
        match child.kind() {
            "string_lit" => labels.push(terraform_string_lit_value(*child, source)),
            // HCL allows unquoted identifier labels (rare in Terraform, legal).
            "identifier" => labels.push(node_text(*child, source)),
            _ => break,
        }
    }
    Some((block_type, labels))
}

/// The `body` named child of a block. Ports `getBlockBody`
/// (terraform.ts:82-84).
pub(crate) fn terraform_block_body(block: Node<'_>) -> Option<Node<'_>> {
    block
        .named_children(&mut block.walk())
        .find(|c| c.kind() == "body")
}

/// Map a block type + labels to a symbol declaration. Ports `describeBlock`
/// (terraform.ts:368-435): resource/data → Class, module → Module,
/// variable/output → Variable, provider → Namespace. `None` for unknown types
/// or missing required labels.
pub(crate) fn describe_terraform_block(
    block_type: &str,
    labels: &[String],
) -> Option<TerraformBlockDecl> {
    let first = labels.first();
    let second = labels.get(1);
    match block_type {
        "resource" => {
            let (first, second) = (first?, second?);
            Some(TerraformBlockDecl {
                kind: NodeKind::Class,
                name: format!("{first}.{second}"),
                qualified_name: format!("{first}.{second}"),
                signature: format!("resource \"{first}\" \"{second}\""),
            })
        }
        "data" => {
            let (first, second) = (first?, second?);
            Some(TerraformBlockDecl {
                kind: NodeKind::Class,
                name: format!("{first}.{second}"),
                qualified_name: format!("data.{first}.{second}"),
                signature: format!("data \"{first}\" \"{second}\""),
            })
        }
        "module" => {
            let first = first?;
            Some(TerraformBlockDecl {
                kind: NodeKind::Module,
                name: first.clone(),
                qualified_name: format!("module.{first}"),
                signature: format!("module \"{first}\""),
            })
        }
        "variable" => {
            let first = first?;
            Some(TerraformBlockDecl {
                kind: NodeKind::Variable,
                name: first.clone(),
                qualified_name: format!("var.{first}"),
                signature: format!("variable \"{first}\""),
            })
        }
        "output" => {
            let first = first?;
            Some(TerraformBlockDecl {
                kind: NodeKind::Variable,
                name: first.clone(),
                qualified_name: format!("output.{first}"),
                signature: format!("output \"{first}\""),
            })
        }
        "provider" => {
            let first = first?;
            Some(TerraformBlockDecl {
                kind: NodeKind::Namespace,
                name: first.clone(),
                qualified_name: format!("provider.{first}"),
                signature: format!("provider \"{first}\""),
            })
        }
        _ => None,
    }
}

/// Turn a reference head + attribute chain into qualified name(s). Ports
/// `qualifyReference` (terraform.ts:158-189).
fn qualify_terraform_reference(head: &str, attrs: &[String]) -> Vec<String> {
    match head {
        "var" => attrs
            .first()
            .map(|a| vec![format!("var.{a}")])
            .unwrap_or_default(),
        "local" => attrs
            .first()
            .map(|a| vec![format!("local.{a}")])
            .unwrap_or_default(),
        // module.M[.OUTPUT]: the module, and for a two-segment chain the
        // output inside it, `module.M:output.O`, a scoped spelling only the
        // Terraform resolver reads; `module.M.outputs.X`, the cloudposse
        // remote-state shape, also names the other component's output X
        // (upstream #1174).
        "module" => {
            let Some(module) = attrs.first() else {
                return Vec::new();
            };
            let mut refs = vec![format!("module.{module}")];
            if let Some(output) = attrs.get(1) {
                refs.push(format!("module.{module}:output.{output}"));
                if output == "outputs"
                    && let Some(remote) = attrs.get(2)
                {
                    refs.push(format!("module.{module}:remote-output.{remote}"));
                }
            }
            refs
        }
        "data" => match (attrs.first(), attrs.get(1)) {
            (Some(t), Some(n)) => vec![format!("data.{t}.{n}")],
            _ => Vec::new(),
        },
        _ => {
            // <type>.<name>[.<attr>...] — managed resource (e.g.
            // aws_s3_bucket.my). Skip a bare head with no dotted chain.
            attrs
                .first()
                .map(|a| vec![format!("{head}.{a}")])
                .unwrap_or_default()
        }
    }
}

/// Read a `variable_expr` head + its `get_attr` sibling chain and emit qualified
/// reference(s). Ports `emitRefFromVariableExpr` (terraform.ts:126-155).
fn emit_ref_from_variable_expr(
    var_expr: Node<'_>,
    source: &str,
    out: &mut Vec<(String, u32, u32)>,
) {
    let Some(id) = var_expr
        .named_children(&mut var_expr.walk())
        .find(|c| c.kind() == "identifier")
    else {
        return;
    };
    let head = node_text(id, source);
    if BUILTIN_HEADS.contains(&head.as_str()) || BUILTIN_KEYWORDS.contains(&head.as_str()) {
        return;
    }

    // tree-sitter-hcl 1.1.0 hangs the traversal of an operation's last
    // operand on the whole operation: `var.left + var.right` reads
    // `operation(binary_operation(var.left + var)) .right`, and `!var.flag`
    // `operation(unary_operation(!var)) .flag`. A variable that ends the
    // operations around it takes the chain after the outermost of them, as
    // upstream's grammar gives it (OD-B49). A parenthesised operation is an
    // expression of its own, so a chain after the parentheses stays theirs.
    let mut anchor = var_expr;
    while anchor.next_named_sibling().is_none() {
        let Some(parent) = anchor.parent().filter(|parent| {
            matches!(
                parent.kind(),
                "operation" | "binary_operation" | "unary_operation"
            )
        }) else {
            break;
        };
        anchor = parent;
    }
    let mut attrs: Vec<String> = Vec::new();
    let mut cursor = anchor.next_named_sibling();
    while let Some(node) = cursor {
        match node.kind() {
            "get_attr" => {
                let Some(attr_id) = node
                    .named_children(&mut node.walk())
                    .find(|c| c.kind() == "identifier")
                else {
                    break;
                };
                attrs.push(node_text(attr_id, source));
                cursor = node.next_named_sibling();
            }
            "index" | "new_index" | "legacy_index" | "splat" | "attr_splat" | "full_splat" => {
                // foo[0], foo[*], foo.* — keep walking, add no segment.
                cursor = node.next_named_sibling();
            }
            _ => break,
        }
    }

    let line = var_expr.start_position().row as u32 + 1;
    let col = var_expr.start_position().column as u32;
    for qname in qualify_terraform_reference(&head, &attrs) {
        out.push((qname, line, col));
    }
}

/// BFS an `expression` subtree, emitting a qualified reference for every dotted
/// name whose head is a Terraform reference root. Ports `collectReferences`
/// (terraform.ts:106-121). Returns `(qualified_name, line, col)` tuples in
/// traversal order.
pub(crate) fn collect_terraform_references(
    expr: Node<'_>,
    source: &str,
) -> Vec<(String, u32, u32)> {
    let mut out: Vec<(String, u32, u32)> = Vec::new();
    let mut queue: std::collections::VecDeque<Node<'_>> = std::collections::VecDeque::new();
    queue.push_back(expr);
    while let Some(node) = queue.pop_front() {
        if node.kind() == "variable_expr" {
            emit_ref_from_variable_expr(node, source, &mut out);
        }
        for child in node.named_children(&mut node.walk()) {
            queue.push_back(child);
        }
    }
    out
}

/// The name of a body `attribute` (`alias` of `alias = "east"`).
pub(crate) fn terraform_attribute_name(attr: Node<'_>, source: &str) -> Option<String> {
    attr.named_children(&mut attr.walk())
        .find(|c| c.kind() == "identifier")
        .map(|id| node_text(id, source))
}

/// An `attribute`'s value expression.
pub(crate) fn terraform_attribute_expression(attr: Node<'_>) -> Option<Node<'_>> {
    attr.named_children(&mut attr.walk())
        .find(|c| c.kind() == "expression")
}

/// The first `string_lit` anywhere under `expr`, breadth first (upstream
/// `findStringLit`).
pub(crate) fn find_terraform_string_lit(expr: Node<'_>) -> Option<Node<'_>> {
    let mut queue: std::collections::VecDeque<Node<'_>> = std::collections::VecDeque::new();
    queue.push_back(expr);
    while let Some(node) = queue.pop_front() {
        if node.kind() == "string_lit" {
            return Some(node);
        }
        for child in node.named_children(&mut node.walk()) {
            queue.push_back(child);
        }
    }
    None
}

/// The value of `body`'s first attribute named `name` when it holds a
/// string literal (upstream `readStringAttr`): `alias = "east"`.
pub(crate) fn terraform_string_attr(body: Node<'_>, name: &str, source: &str) -> Option<String> {
    for attr in body.named_children(&mut body.walk()) {
        if attr.kind() != "attribute"
            || terraform_attribute_name(attr, source).as_deref() != Some(name)
        {
            continue;
        }
        let literal = terraform_attribute_expression(attr).and_then(find_terraform_string_lit)?;
        return Some(terraform_string_lit_value(literal, source));
    }
    None
}

/// The provider configuration an expression selects (upstream
/// `providerSelectionFromExpr`): `aws`, `aws.east`, `google-beta`; `None` for
/// anything more (a longer chain, a call, a conditional, a `for`).
pub(crate) fn terraform_provider_selection(expr: Node<'_>, source: &str) -> Option<String> {
    let mut queue: std::collections::VecDeque<Node<'_>> = std::collections::VecDeque::new();
    queue.push_back(expr);
    while let Some(node) = queue.pop_front() {
        if node.kind() == "variable_expr" {
            let id = node
                .named_children(&mut node.walk())
                .find(|c| c.kind() == "identifier")?;
            let head = node_text(id, source);
            return match node.next_named_sibling() {
                Some(next) if next.kind() == "get_attr" => {
                    let attr = next
                        .named_children(&mut next.walk())
                        .find(|c| c.kind() == "identifier")?;
                    if next.next_named_sibling().is_some() {
                        return None;
                    }
                    Some(format!("{head}.{}", node_text(attr, source)))
                }
                Some(_) => None,
                None => Some(head),
            };
        }
        if matches!(node.kind(), "function_call" | "conditional" | "for_expr") {
            return None;
        }
        for child in node.named_children(&mut node.walk()) {
            queue.push_back(child);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_hcl::LANGUAGE.into())
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
    fn terraform_spec_has_empty_type_sets() {
        assert!(TERRAFORM_SPEC.function_types().is_empty());
        assert!(TERRAFORM_SPEC.class_types().is_empty());
        assert!(TERRAFORM_SPEC.method_types().is_empty());
        assert!(TERRAFORM_SPEC.call_types().is_empty());
        assert!(TERRAFORM_SPEC.variable_types().is_empty());
        assert!(TERRAFORM_SPEC.import_types().is_empty());
        assert_eq!(TERRAFORM_SPEC.language(), Language::Terraform);
    }

    #[test]
    fn describe_terraform_block_resource() {
        let decl = describe_terraform_block("resource", &["aws_s3_bucket".into(), "b".into()])
            .expect("resource decl");
        assert_eq!(decl.kind, NodeKind::Class);
        assert_eq!(decl.name, "aws_s3_bucket.b");
        assert_eq!(decl.qualified_name, "aws_s3_bucket.b");
    }

    #[test]
    fn describe_terraform_block_data() {
        let decl = describe_terraform_block("data", &["aws_ami".into(), "ubuntu".into()])
            .expect("data decl");
        assert_eq!(decl.kind, NodeKind::Class);
        assert_eq!(decl.qualified_name, "data.aws_ami.ubuntu");
    }

    #[test]
    fn describe_terraform_block_module() {
        let decl = describe_terraform_block("module", &["vpc".into()]).expect("module decl");
        assert_eq!(decl.kind, NodeKind::Module);
        assert_eq!(decl.qualified_name, "module.vpc");
    }

    #[test]
    fn describe_terraform_block_variable() {
        let decl = describe_terraform_block("variable", &["region".into()]).expect("variable decl");
        assert_eq!(decl.kind, NodeKind::Variable);
        assert_eq!(decl.qualified_name, "var.region");
    }

    #[test]
    fn describe_terraform_block_provider() {
        let decl = describe_terraform_block("provider", &["aws".into()]).expect("provider decl");
        assert_eq!(decl.kind, NodeKind::Namespace);
        assert_eq!(decl.qualified_name, "provider.aws");
    }

    #[test]
    fn describe_terraform_block_unknown_is_none() {
        assert!(describe_terraform_block("terraform", &[]).is_none());
        assert!(describe_terraform_block("resource", &["only_one".into()]).is_none());
    }

    /// W7-24c: a two-segment module chain also names the output inside the
    /// module, and `module.M.outputs.X` a remote-state output (upstream
    /// #1174's `qualifyReference`).
    #[test]
    fn qualify_terraform_reference_module_output_has_its_scoped_half() {
        assert_eq!(
            qualify_terraform_reference("module", &["vpc".into(), "id".into()]),
            vec!["module.vpc".to_string(), "module.vpc:output.id".to_string()]
        );
        assert_eq!(
            qualify_terraform_reference(
                "module",
                &["vpc".into(), "outputs".into(), "vpc_id".into()]
            ),
            vec![
                "module.vpc".to_string(),
                "module.vpc:output.outputs".to_string(),
                "module.vpc:remote-output.vpc_id".to_string()
            ]
        );
    }

    #[test]
    fn terraform_parses_block() {
        let tree = parse("resource \"aws_s3_bucket\" \"b\" { bucket = var.name }");
        let root = tree.root_node();
        let block = first_of_kind(root, "block").expect("a block node");
        let (block_type, labels) = read_terraform_block_header(
            block,
            "resource \"aws_s3_bucket\" \"b\" { bucket = var.name }",
        )
        .expect("header");
        assert_eq!(block_type, "resource");
        assert_eq!(labels, vec!["aws_s3_bucket".to_string(), "b".to_string()]);
    }
}
