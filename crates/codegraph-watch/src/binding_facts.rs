//! The facts lane B of a sync reads from a module other files bind (W7-31,
//! OD-B72): one digest row per file.
//!
//! A reference written through an import binding (`logger.info()` after
//! `import { logger } from './logging'`) resolves through text of the bound
//! module that no edge into it records: which of its declarations the
//! binding names, the value's declaration with its initializer or
//! annotation (#1292's `resolve_imported_instance_member`), that file's
//! import of the class the declaration names (#2644's
//! `ts_imported_type_file`), and an object literal's members and the
//! imports they name (#1932's `resolve_object_literal_binding`). The digest
//! covers, in order:
//!
//! - the import mappings resolution reads from the file's text
//!   (`extract_import_mappings`: a regex over the whole text, so import
//!   text in a comment counts, and the first mapping of a name wins);
//! - the kind, name and export flag of each module-level value, and of
//!   each module-level declaration sharing its name with another (which of
//!   them a binding names: the first exported node of the name, or
//!   Python's first top-level one), in the order the store lists a file's
//!   nodes;
//! - the full source lines of each such constant or variable.
//!
//! An edit that leaves all three alone (a function body, a comment, a line
//! shift) moves no such answer.
//!
//! Every full index and migration writes a row for each text file of the
//! lane's languages, one with no fact included. A sync withdraws a
//! re-extracted or removed file's row before the file changes and writes
//! the new one once its resolution is persisted: a missing row (an index
//! from before the rows, a sync that failed in between) counts as changed,
//! so the lane fires. No marker: the rows need no bootstrap.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::source_file::SourceText;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_store::Store;

/// `project_metadata` key prefix of a file's row.
pub const ROW_KEY_PREFIX: &str = "binding_facts:";

/// The `project_metadata` key of `path`'s row.
pub fn row_key(path: &str) -> String {
    format!("{ROW_KEY_PREFIX}{path}")
}

/// Whether lane B reads the references a file of `language` writes through
/// its bindings, or the facts of a module of it: the languages whose named
/// imports bind a value another file declares, which
/// `resolve_imported_instance_member` types and an object literal's binding
/// answers (the ES-module languages and their components' scripts, and
/// Python).
pub fn is_lane_language(file_path: &str, language: Language) -> bool {
    language == Language::Python
        || codegraph_resolve::module_surface::has_module_surface(file_path, language)
}

/// The digest of `relative`'s facts, or `None` for a file of another
/// language or one that is not text.
pub fn row_of(
    relative: &str,
    language: Language,
    source: &SourceText,
    nodes: &[Node],
    edges: &[Edge],
) -> Option<String> {
    if !is_lane_language(relative, language) {
        return None;
    }
    let SourceText::Text(text) = source else {
        return None;
    };
    Some(hash_content(&facts_text(text, language, nodes, edges)))
}

/// The canonical facts of a file: its import mappings as resolution
/// extracts them, then its declarations as the module's bindings name them
/// (see the module docs), in the store's order.
fn facts_text(text: &str, language: Language, nodes: &[Node], edges: &[Edge]) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let slice = |node: &Node| -> String {
        let start = usize::try_from(node.start_line.max(1) - 1).unwrap_or(0);
        let end = usize::try_from(node.end_line.max(node.start_line))
            .unwrap_or(0)
            .min(lines.len());
        lines
            .get(start..end)
            .map(|span| span.join("\n"))
            .unwrap_or_default()
    };
    let parents: HashMap<&str, &str> = edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains)
        .map(|edge| (edge.target.as_str(), edge.source.as_str()))
        .collect();
    let kinds: HashMap<&str, NodeKind> = nodes
        .iter()
        .map(|node| (node.id.as_str(), node.kind))
        .collect();
    // A declaration inside a function or method is a local: no import binds
    // it.
    let module_level = |node: &Node| -> bool {
        let mut at = node.id.as_str();
        let mut seen = BTreeSet::new();
        while let Some(&parent) = parents.get(at) {
            if !seen.insert(parent) {
                break;
            }
            if matches!(
                kinds.get(parent),
                Some(NodeKind::Function | NodeKind::Method)
            ) {
                return false;
            }
            at = parent;
        }
        true
    };
    let is_value = |node: &Node| matches!(node.kind, NodeKind::Constant | NodeKind::Variable);
    // What a binding of a name can name: the module-level declarations.
    let named: Vec<(usize, &Node)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            !matches!(node.kind, NodeKind::File | NodeKind::Import) && module_level(node)
        })
        .collect();
    let mut sharing: HashMap<&str, usize> = HashMap::new();
    for (_, node) in &named {
        *sharing.entry(node.name.as_str()).or_default() += 1;
    }
    // A declaration enters when another shares its name, so which one a
    // binding names can move; a value always does, with its lines. The
    // store lists a file's nodes by start line (`nodes_by_file_path`), the
    // extraction's order within a line.
    let mut declarations: Vec<(i64, usize, String)> = named
        .into_iter()
        .filter(|(_, node)| is_value(node) || sharing[node.name.as_str()] > 1)
        .map(|(index, node)| {
            let mut entry = format!(
                "node\t{}\t{}\t{}",
                node.kind.as_str(),
                node.name,
                u8::from(node.is_exported)
            );
            if is_value(node) {
                entry.push('\t');
                entry.push_str(&slice(node));
            }
            (node.start_line, index, entry)
        })
        .collect();
    declarations.sort_by_key(|(line, index, _)| (*line, *index));
    // Resolution reads a file's imports with one extractor per family
    // (`extract_import_mappings`): the JavaScript one for every language
    // of the lane but Python.
    let mapping_language = if language == Language::Python {
        Language::Python
    } else {
        Language::TypeScript
    };
    let mappings =
        codegraph_resolve::import_resolver::extract_import_mappings(text, mapping_language);
    let mut canonical = String::from("binding-facts/2\n");
    for mapping in &mappings {
        canonical.push_str(&format!(
            "mapping\t{}\t{}\t{}\t{}\t{}\n\u{1e}\n",
            mapping.local_name,
            mapping.exported_name,
            mapping.source,
            u8::from(mapping.is_default),
            u8::from(mapping.is_namespace)
        ));
    }
    for (_, _, entry) in &declarations {
        canonical.push_str(entry);
        canonical.push_str("\n\u{1e}\n");
    }
    canonical
}

/// Write the rows of a full index or a migration.
pub fn write_index_rows(store: &Store, rows: &mut [(String, String)]) -> Result<()> {
    rows.sort();
    for (path, digest) in rows.iter() {
        store.set_project_metadata(&row_key(path), digest)?;
    }
    Ok(())
}

/// `path`'s row as the store holds it.
pub(crate) fn read_row(store: &Store, path: &str) -> Result<Option<String>> {
    Ok(store.get_project_metadata(&row_key(path))?)
}

/// What a sync recorded of the rows of the files it rewrote: the row each
/// will hold once the sync's resolution is persisted (`None`: no row).
#[derive(Debug, Default)]
pub(crate) struct RowChanges {
    rows: BTreeMap<String, Option<String>>,
}

impl RowChanges {
    /// `path` is about to change: withdraw its row, so a sync that stops
    /// before it writes the new one leaves the file to fire again, and
    /// return whether the facts moved. `new` is its row after the change
    /// (`None` for a removed file, or one the lane no longer reads); a file
    /// that had no row counts as moved.
    pub(crate) fn note(&mut self, store: &Store, path: &str, new: Option<String>) -> Result<bool> {
        let old = read_row(store, path)?;
        if old.is_some() {
            store.delete_project_metadata(&row_key(path))?;
        }
        let moved = old.is_none() || old != new;
        self.rows.insert(path.to_string(), new);
        Ok(moved)
    }

    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        for (path, row) in &self.rows {
            match row {
                Some(digest) => {
                    store.set_project_metadata(&row_key(path), digest)?;
                }
                None => {
                    store.delete_project_metadata(&row_key(path))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: NodeKind, name: &str, lines: (i64, i64)) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/logging.ts".to_string(),
            language: Language::TypeScript,
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn contains(source: &str, target: &str) -> Edge {
        Edge {
            id: None,
            source: source.to_string(),
            target: target.to_string(),
            kind: EdgeKind::Contains,
            metadata: None,
            line: None,
            col: None,
            provenance: None,
        }
    }

    fn digest(text: &str, nodes: &[Node], edges: &[Edge]) -> Option<String> {
        row_of(
            "src/logging.ts",
            Language::TypeScript,
            &SourceText::Text(text.to_string()),
            nodes,
            edges,
        )
    }

    /// The import mappings, the module-level values with their lines and
    /// the namesakes are the facts, in order; a body, a comment, a line
    /// shift and a function of a new name are not, and a local is none.
    #[test]
    fn the_digest_reads_mappings_declarations_and_values_only() {
        let text = "import { Logger } from './log/console';\n\nexport const logger = new Logger();\n\nfunction f() {\n  const local = 1;\n  return local;\n}\n";
        let nodes = [
            node(
                "file:src/logging.ts",
                NodeKind::File,
                "src/logging.ts",
                (1, 9),
            ),
            node("import:1", NodeKind::Import, "./log/console", (1, 1)),
            node("constant:logger", NodeKind::Constant, "logger", (3, 3)),
            node("function:f", NodeKind::Function, "f", (5, 8)),
            node("constant:local", NodeKind::Constant, "local", (6, 6)),
        ];
        let edges = [
            contains("file:src/logging.ts", "import:1"),
            contains("file:src/logging.ts", "constant:logger"),
            contains("file:src/logging.ts", "function:f"),
            contains("function:f", "constant:local"),
        ];
        let base = digest(text, &nodes, &edges).expect("facts");
        // A body edit, also of a local, moves nothing.
        let body = text.replace("const local = 1;", "const local = 2;");
        assert_eq!(
            digest(&body, &nodes, &edges).as_deref(),
            Some(base.as_str())
        );
        // Nor does a function of a name no other declaration has.
        let unrelated = format!("{text}\nexport function unrelated() {{}}\n");
        let mut more = nodes.to_vec();
        more.push(node(
            "function:unrelated",
            NodeKind::Function,
            "unrelated",
            (10, 10),
        ));
        let mut more_edges = edges.to_vec();
        more_edges.push(contains("file:src/logging.ts", "function:unrelated"));
        assert_eq!(
            digest(&unrelated, &more, &more_edges).as_deref(),
            Some(base.as_str())
        );
        // A comment above shifts every line: the facts stay.
        let shifted = format!("// one line down\n{text}");
        let moved: Vec<Node> = nodes
            .iter()
            .cloned()
            .map(|mut node| {
                node.start_line += 1;
                node.end_line += 1;
                node
            })
            .collect();
        assert_eq!(
            digest(&shifted, &moved, &edges).as_deref(),
            Some(base.as_str())
        );
        // The import re-pointed, the initializer's class or an annotation
        // changed: each moves the digest.
        for changed in [
            text.replace("'./log/console'", "'./log/file'"),
            text.replace("new Logger()", "new Auditor()"),
            text.replace("logger = new", "logger: Logger = new"),
        ] {
            assert_ne!(
                digest(&changed, &nodes, &edges),
                Some(base.clone()),
                "{changed}"
            );
        }
        // Import text in a comment ahead of the statement, on its own line.
        let spaced = format!("\n{text}");
        let commented = format!("// import {{ Logger }} from './log/file';\n{text}");
        assert_ne!(
            digest(&spaced, &moved, &edges),
            digest(&commented, &moved, &edges)
        );
        // Two values of the name trade places: every node keeps its line.
        let twice = "logger = Logger()\nlogger = Auditor()\n";
        let swapped = "logger = Auditor()\nlogger = Logger()\n";
        let values = [
            node("variable:a", NodeKind::Variable, "logger", (1, 1)),
            node("variable:b", NodeKind::Variable, "logger", (2, 2)),
        ];
        assert_ne!(digest(twice, &values, &[]), digest(swapped, &values, &[]));
        // A namesake declaration comes ahead of the value.
        let namesake = text.replacen("\n\n", "\nexport interface logger {}\n", 1);
        let mut ahead = nodes.to_vec();
        ahead.push(node(
            "interface:logger",
            NodeKind::Interface,
            "logger",
            (2, 2),
        ));
        assert_ne!(digest(&namesake, &ahead, &edges), Some(base.clone()));
        // A file with nothing to record has its row too, which its edits
        // leave alone.
        let only = |text: &str| {
            digest(
                text,
                &[node(
                    "file:src/logging.ts",
                    NodeKind::File,
                    "src/logging.ts",
                    (1, 2),
                )],
                &[],
            )
        };
        assert!(only("\n").is_some());
        assert_eq!(only("\n"), only("// a comment\n"));
        // Another language records none.
        assert_eq!(
            row_of(
                "src/lib.rs",
                Language::Rust,
                &SourceText::Text(text.to_string()),
                &nodes,
                &edges
            ),
            None
        );
    }
}
