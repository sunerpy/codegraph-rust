use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};
use regex::Regex;
use std::path::Path;

use crate::embedded::shared::{contains_edge, default_node, empty_result, unresolved_ref};

/// Custom Delphi DFM/FMX extractor.
///
/// Mirrors `upstream extraction/dfm-extractor.ts:32-158`: DFM/FMX
/// form files emit a custom file node, component nodes for object blocks,
/// contains edges for nesting, and unresolved references for event handlers.
pub struct DfmExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
}

impl<'a> DfmExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self { file_path, source }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = empty_result(0);
        let file_node = self.create_file_node();
        let file_id = file_node.id.clone();
        result.nodes.push(file_node);
        self.parse_components(&mut result, &file_id);
        result.duration_ms = start.elapsed().as_millis() as i64;
        result
    }

    fn create_file_node(&self) -> Node {
        let mut node = default_node(
            self.file_path,
            Language::Pascal,
            NodeKind::File,
            Path::new(self.file_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            self.file_path.to_string(),
            1,
            self.source.split('\n').count().max(1) as i64,
            0,
            self.source
                .split('\n')
                .next_back()
                .map_or(0, |line| line.len()) as i64,
        );
        // The upstream DfmExtractor uses generateNodeId(filePath, 'file', filePath, 1)
        // at upstream extraction/dfm-extractor.ts:55-75.
        node.id = generate_node_id(self.file_path, NodeKind::File, self.file_path, 1);
        node
    }

    fn parse_components(&self, result: &mut ExtractionResult, file_id: &str) {
        let object_re = Regex::new(r"^\s*(object|inherited|inline)\s+(\w+)\s*:\s*(\w+)").unwrap();
        let event_re = Regex::new(r"^\s*(On\w+)\s*=\s*(\w+)\s*$").unwrap();
        let end_re = Regex::new(r"^\s*end\s*$").unwrap();
        let multiline_start_re = Regex::new(r"=\s*\(\s*$").unwrap();
        let multiline_item_start_re = Regex::new(r"=\s*<\s*$").unwrap();

        let mut stack = vec![file_id.to_string()];
        let mut in_multiline = false;
        let mut multiline_end = ')';

        for (idx, line) in self.source.split('\n').enumerate() {
            let line_num = idx as i64 + 1;

            if in_multiline {
                if line.trim_end().ends_with(multiline_end) {
                    in_multiline = false;
                }
                continue;
            }
            if multiline_start_re.is_match(line) {
                in_multiline = true;
                multiline_end = ')';
                continue;
            }
            if multiline_item_start_re.is_match(line) {
                in_multiline = true;
                multiline_end = '>';
                continue;
            }

            if let Some(captures) = object_re.captures(line) {
                let name = captures.get(2).unwrap().as_str().to_string();
                let type_name = captures.get(3).unwrap().as_str().to_string();
                let mut node = default_node(
                    self.file_path,
                    Language::Pascal,
                    NodeKind::Component,
                    name.clone(),
                    format!("{}#{name}", self.file_path),
                    line_num,
                    line_num,
                    0,
                    line.len() as i64,
                );
                node.signature = Some(type_name);
                let node_id = node.id.clone();
                result.nodes.push(node);
                result
                    .edges
                    .push(contains_edge(stack.last().unwrap(), &node_id));
                stack.push(node_id);
                continue;
            }

            if let Some(captures) = event_re.captures(line) {
                let method_name = captures.get(2).unwrap().as_str().to_string();
                result.unresolved_references.push(unresolved_ref(
                    stack.last().unwrap(),
                    method_name,
                    EdgeKind::References,
                    line_num,
                    0,
                    self.file_path,
                    Language::Pascal,
                ));
                continue;
            }

            if end_re.is_match(line) && stack.len() > 1 {
                stack.pop();
            }
        }
    }
}
