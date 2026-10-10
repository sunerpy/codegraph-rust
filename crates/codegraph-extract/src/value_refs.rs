//! Same-file value references (upstream #895, #897, `flushValueRefs`): a
//! `references` edge from a symbol to a constant or variable of its own file
//! that it reads, so impact analysis reaches the readers of a shared value
//! ("change this table, affect its readers"). The edge carries
//! `{"valueRef": true}`.
//!
//! Same-file only, so the target is never a guess. Only a distinctive name is
//! a target: three or more characters, one of them an uppercase letter or
//! `_`. A name the file binds again in an inner scope is no target at all,
//! since its nested readers read that binding. Always on (OD-8): upstream's
//! `CODEGRAPH_VALUE_REFS=0` switch is not ported.

use std::collections::{HashMap, HashSet};

use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::{Edge, EdgeKind, Language, NodeKind};
use tree_sitter::Node as SyntaxNode;

use crate::walker::{child_by_field, node_text};

/// How many syntax nodes one scan visits at most.
const MAX_VALUE_REF_NODES: usize = 20_000;

/// The languages whose symbols get value references.
pub(crate) fn has_value_refs(language: Language) -> bool {
    matches!(
        language,
        Language::TypeScript
            | Language::JavaScript
            | Language::Tsx
            | Language::ArkTs
            | Language::Go
            | Language::Python
    )
}

/// A symbol whose syntax is scanned for the values it reads.
struct Reader<'tree> {
    id: String,
    name: String,
    node: SyntaxNode<'tree>,
}

/// What one file's walk records for its value references.
#[derive(Default)]
pub(crate) struct ValueRefs<'tree> {
    /// Target name -> node id; a later declaration of the name wins.
    targets: HashMap<String, String>,
    /// How many target nodes carry each name.
    target_counts: HashMap<String, usize>,
    readers: Vec<Reader<'tree>>,
}

impl<'tree> ValueRefs<'tree> {
    /// Record a node as it is created: a constant or variable declared at file
    /// scope, or in a class, module, struct or enum, with a distinctive name
    /// is a target; a function, method, constant or variable is a reader.
    pub(crate) fn capture(
        &mut self,
        kind: NodeKind,
        name: &str,
        id: &str,
        node: SyntaxNode<'tree>,
        parent_id: Option<&str>,
    ) {
        if matches!(kind, NodeKind::Constant | NodeKind::Variable)
            && is_distinctive(name)
            && parent_id.is_some_and(is_target_scope)
        {
            self.targets.insert(name.to_string(), id.to_string());
            *self.target_counts.entry(name.to_string()).or_default() += 1;
        }
        if matches!(
            kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constant | NodeKind::Variable
        ) {
            self.readers.push(Reader {
                id: id.to_string(),
                name: name.to_string(),
                node,
            });
        }
    }

    /// The value-reference edges of the file whose tree is `root`.
    pub(crate) fn into_edges(
        mut self,
        language: Language,
        file_path: &str,
        root: SyntaxNode<'tree>,
        source: &str,
    ) -> Vec<Edge> {
        if !has_value_refs(language)
            || self.targets.is_empty()
            || self.readers.is_empty()
            || is_generated_file(file_path)
        {
            return Vec::new();
        }
        self.drop_shadowed_targets(root, source);
        if self.targets.is_empty() {
            return Vec::new();
        }
        let mut edges = Vec::new();
        for reader in &self.readers {
            let mut seen = HashSet::new();
            let mut stack = vec![reader.node];
            let mut visited = 0;
            while let Some(node) = stack.pop() {
                if visited >= MAX_VALUE_REF_NODES {
                    break;
                }
                visited += 1;
                if matches!(
                    node.kind(),
                    "identifier" | "constant" | "name" | "simple_identifier"
                ) {
                    let name = node_text(node, source);
                    if let Some(target) = self.targets.get(&name)
                        && *target != reader.id
                        && name != reader.name
                        && seen.insert(target.clone())
                    {
                        edges.push(value_ref_edge(&reader.id, target));
                    }
                }
                let mut cursor = node.walk();
                stack.extend(node.named_children(&mut cursor));
            }
        }
        edges
    }

    /// A name the file declares more often than it has target nodes of that
    /// name is bound again in an inner scope; it is no target. A conditional
    /// definition at file scope (`try: X = a` / `except: X = b`) declares it
    /// as often as it has nodes and stays.
    fn drop_shadowed_targets(&mut self, root: SyntaxNode<'tree>, source: &str) {
        let mut declared: HashMap<String, usize> = HashMap::new();
        let mut bump = |name_node: Option<SyntaxNode<'tree>>| {
            if let Some(name_node) = name_node
                && matches!(name_node.kind(), "identifier" | "simple_identifier")
            {
                let name = node_text(name_node, source);
                if self.targets.contains_key(&name) {
                    *declared.entry(name).or_default() += 1;
                }
            }
        };
        let mut stack = vec![root];
        let mut visited = 0;
        while let Some(node) = stack.pop() {
            if visited >= MAX_VALUE_REF_NODES {
                break;
            }
            visited += 1;
            match node.kind() {
                // TS/JS `const X = …`; Go `const X = …` / `var X = …`.
                "variable_declarator" | "const_spec" | "var_spec" => bump(node.named_child(0)),
                // Go `x, Y := …`; Python `X = …`, `A, B = …`.
                "short_var_declaration" | "assignment" => {
                    let left = child_by_field(node, "left").or_else(|| node.named_child(0));
                    match left {
                        Some(left) if left.kind() == "identifier" => bump(Some(left)),
                        Some(left) => {
                            for child in left.named_children(&mut left.walk()) {
                                bump(Some(child));
                            }
                        }
                        None => {}
                    }
                }
                _ => {}
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        for (name, count) in declared {
            if count > self.target_counts.get(&name).copied().unwrap_or(1) {
                self.targets.remove(&name);
            }
        }
    }
}

/// Three or more UTF-16 code units, one of them an ASCII uppercase letter or
/// `_` (upstream `name.length >= 3 && /[A-Z_]/`).
fn is_distinctive(name: &str) -> bool {
    name.encode_utf16().count() >= 3 && name.chars().any(|c| c.is_ascii_uppercase() || c == '_')
}

/// File, class, module, struct and enum scope hold shared values.
fn is_target_scope(parent_id: &str) -> bool {
    ["file:", "class:", "module:", "struct:", "enum:"]
        .iter()
        .any(|prefix| parent_id.starts_with(prefix))
}

fn value_ref_edge(source: &str, target: &str) -> Edge {
    Edge {
        id: None,
        source: source.to_string(),
        target: target.to_string(),
        kind: EdgeKind::References,
        metadata: Some(serde_json::json!({ "valueRef": true })),
        line: None,
        col: None,
        provenance: None,
    }
}
