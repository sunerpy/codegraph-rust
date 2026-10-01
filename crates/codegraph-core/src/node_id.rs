//! Upstream-compatible node and content identifiers.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::types::NodeKind;

/// Generate the same symbol node ID as the upstream `generateNodeId()` helper.
///
/// The upstream hashes the exact UTF-8 string
/// `{filePath}:{kind}:{name}:{line}`, hex-encodes the SHA-256 digest, keeps the
/// first 32 hex characters, then prefixes it with `{kind}:`.
pub fn generate_node_id(file_path: &str, kind: NodeKind, name: &str, line: u32) -> String {
    let kind = kind.as_str();
    let mut hasher = Sha256::new();
    hasher.update(format!("{file_path}:{kind}:{name}:{line}"));
    let digest = hasher.finalize();
    let hex = hex_lower(&digest);

    format!("{}:{}", kind, &hex[..32])
}

/// Per-extraction node identities (upstream `NodeIdAllocator`, #1349).
///
/// A declaration keeps its [`generate_node_id`] identity unless an EARLIER
/// declaration of the same extraction already holds that identity at a
/// different source position — a same-kind, same-name declaration on the same
/// line, such as a getter/setter pair. The later one then appends
/// `:{column}`, its zero-based UTF-16 column, instead of overwriting the first
/// in the store. Revisiting a declaration yields the identity it had.
#[derive(Debug, Default)]
pub struct NodeIdAllocator {
    first_columns: HashMap<String, u32>,
}

impl NodeIdAllocator {
    /// The identity of the declaration of `kind`/`name` at `line` (1-based)
    /// and `utf16_column` (0-based UTF-16 code units).
    pub fn generate(
        &mut self,
        file_path: &str,
        kind: NodeKind,
        name: &str,
        line: u32,
        utf16_column: u32,
    ) -> String {
        let id = generate_node_id(file_path, kind, name, line);
        let first = *self.first_columns.entry(id.clone()).or_insert(utf16_column);
        if first == utf16_column {
            id
        } else {
            format!("{id}:{utf16_column}")
        }
    }
}

/// Zero-based UTF-16 column of byte offset `byte` in `source` — the column
/// unit upstream's identities use, never tree-sitter's byte column.
pub fn utf16_column(source: &str, byte: usize) -> u32 {
    let byte = byte.min(source.len());
    let line_start = source[..byte].rfind('\n').map_or(0, |newline| newline + 1);
    source
        .get(line_start..byte)
        .map_or(0, |prefix| prefix.encode_utf16().count() as u32)
}

/// Generate the literal file-node ID used by the upstream tree-sitter extractor.
pub fn file_node_id(file_path: &str) -> String {
    format!("file:{file_path}")
}

/// Generate the same full SHA-256 content hash the upstream stores in the `files` table.
pub fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }

    out
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[test]
    fn collisions_only_suffix_later_distinct_columns() {
        // Upstream kernel `collision_only_identity_vectors`.
        let mut ids = NodeIdAllocator::default();
        let base = generate_node_id("src/a.ts", NodeKind::Function, "foo", 3);
        for (column, expected) in [
            (2, base.clone()),
            (24, format!("{base}:24")),
            (48, format!("{base}:48")),
            (24, format!("{base}:24")),
            (2, base.clone()),
        ] {
            assert_eq!(
                ids.generate("src/a.ts", NodeKind::Function, "foo", 3, column),
                expected
            );
        }
        assert_eq!(
            ids.generate("src/a.ts", NodeKind::Function, "bar", 3, 24),
            generate_node_id("src/a.ts", NodeKind::Function, "bar", 3)
        );
    }

    #[test]
    fn utf16_columns_count_code_units_from_the_line_start() {
        let source = "x\n/* é😀 */ set";
        let byte = source.find("set").unwrap();
        // `/* ` 3, `é` 1, `😀` 2, ` */ ` 4.
        assert_eq!(utf16_column(source, byte), 10);
        assert_eq!(utf16_column(source, 0), 0);
        assert_eq!(utf16_column("ab", 99), 2);
    }

    #[derive(Debug, Deserialize)]
    struct GoldenNode {
        id: String,
        kind: NodeKind,
        name: String,
        file_path: String,
        start_line: u32,
    }

    #[test]
    fn reproduces_upstream_golden_node_ids_byte_for_byte() {
        let nodes: Vec<GoldenNode> = serde_json::from_str(include_str!(
            "../../../reference/golden/mini/colby.nodes.json"
        ))
        .expect("golden nodes deserialize");

        let mut assertion_count = 0;
        for node in &nodes {
            let actual = if node.kind == NodeKind::File {
                file_node_id(&node.file_path)
            } else {
                generate_node_id(&node.file_path, node.kind, &node.name, node.start_line)
            };

            assert_eq!(
                actual, node.id,
                "node id mismatch for file_path={} kind={} name={} start_line={}",
                node.file_path, node.kind, node.name, node.start_line
            );
            assertion_count += 1;
        }

        assert_eq!(assertion_count, 13);
        println!("golden node ids reproduced: {assertion_count}");
    }

    #[test]
    fn hashes_fixture_content_like_upstream_files_table() {
        let fixtures = [
            (
                "src/app.ts",
                include_str!("../../../crates/codegraph-bench/fixtures/mini/src/app.ts"),
                "10857ef49b4fb2f611c10181f9fa4c955e86b1ec1a54b2a272b17ffb848598cd",
            ),
            (
                "src/math.ts",
                include_str!("../../../crates/codegraph-bench/fixtures/mini/src/math.ts"),
                "caebbaef45cf4da7e66dd1479300307d465c03a7a9be5c9e358877bcbc81efc8",
            ),
            (
                "tools/greeter.py",
                include_str!("../../../crates/codegraph-bench/fixtures/mini/tools/greeter.py"),
                "256033248f73c030955a522a62420fc54a5bdc1fc1c7aff58e55403e7b27cc3b",
            ),
        ];

        for (path, content, expected) in fixtures {
            assert_eq!(
                hash_content(content),
                expected,
                "content hash mismatch for {path}"
            );
        }
    }
}
