//! Upstream-compatible node and content identifiers.

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
