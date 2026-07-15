//! Terraform / OpenTofu (`.tf` / `.tfvars` / `.tofu`) `LanguageSpec`.
//!
//! Extraction-tier port of `upstream extraction/languages/terraform.ts` (commit
//! `6c24f4b`, #1173 — the extraction slice only). HCL is intentionally generic:
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
//! The module-boundary framework RESOLVER (`resolution/frameworks/terraform.ts`),
//! its registration, the directory-scoping resolution gate, and the
//! extraction-side emitters that feed ONLY that resolver (`emitModuleWiring`'s
//! `module.M:file` / `module.M:var.X` / `module.M:output.X` `:`-scoped refs, the
//! `.tfvars` top-level-assignment `var.X` ref, and the `module.M:output.<out>`
//! scoped half of `qualifyReference`) are DEFERRED — the port has exactly one
//! concrete `FrameworkResolver` (`GodotResolver`).
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
/// `qualifyReference` (terraform.ts:158-189) MINUS the DEFERRED
/// `module.M:output.<out>` scoped half — a `module.M.out` chain emits ONLY the
/// plain `module.M`.
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
        "module" => {
            // Plain `module.M` only; the `module.M:output.<out>` scoped half is
            // DEFERRED (it feeds the deferred TerraformResolver).
            attrs
                .first()
                .map(|a| vec![format!("module.{a}")])
                .unwrap_or_default()
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

    let mut attrs: Vec<String> = Vec::new();
    let mut cursor = var_expr.next_named_sibling();
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

    #[test]
    fn qualify_terraform_reference_module_output_is_plain() {
        // A two-segment module.M.out chain emits ONLY plain module.M (the
        // module.M:output.out scoped half is DEFERRED).
        assert_eq!(
            qualify_terraform_reference("module", &["vpc".into(), "id".into()]),
            vec!["module.vpc".to_string()]
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
