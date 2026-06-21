use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use regex::Regex;

use crate::embedded::shared::{
    contains_edge, default_node, empty_result, file_like_node, line_number_for_offset,
    line_start_for, line_starts, unresolved_ref,
};

pub struct LiquidExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> LiquidExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self {
            file_path,
            source,
            line_starts: line_starts(source),
        }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = empty_result(0);
        let file_node = file_like_node(self.file_path, self.source, Language::Liquid);
        let file_id = file_node.id.clone();
        result.nodes.push(file_node);
        if self.file_path.ends_with(".json") {
            self.extract_shopify_json_sections(&mut result, &file_id);
        } else {
            self.extract_snippets(&mut result, &file_id);
            self.extract_sections(&mut result, &file_id);
            self.extract_schema(&mut result, &file_id);
            self.extract_assignments(&mut result, &file_id);
        }
        result.duration_ms += start.elapsed().as_millis() as i64;
        result
    }

    fn extract_shopify_json_sections(&self, result: &mut ExtractionResult, file_id: &str) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(self.source) else {
            return;
        };
        let Some(sections) = parsed.get("sections").and_then(|value| value.as_object()) else {
            return;
        };
        let mut seen = std::collections::BTreeSet::new();
        for section in sections.values() {
            let Some(section_type) = section.get("type").and_then(|value| value.as_str()) else {
                continue;
            };
            if seen.insert(section_type.to_string()) {
                result.unresolved_references.push(unresolved_ref(
                    file_id,
                    format!("sections/{section_type}.liquid"),
                    EdgeKind::References,
                    1,
                    0,
                    self.file_path,
                    Language::Liquid,
                ));
            }
        }
    }

    fn extract_snippets(&self, result: &mut ExtractionResult, file_id: &str) {
        let regex = Regex::new(r#"\{%[-]?\s*(render|include)\s+['"]([^'"]+)['"]"#).unwrap();
        for cap in regex.captures_iter(self.source) {
            let full = cap.get(0).unwrap();
            let tag_type = cap.get(1).unwrap().as_str();
            let name = cap.get(2).unwrap().as_str();
            let line = line_number_for_offset(&self.line_starts, full.start());
            let col = full.start() as i64 - line_start_for(&self.line_starts, line) as i64;
            self.push_import_node(result, file_id, name, full.as_str(), line, col);
            let node = default_node(
                self.file_path,
                Language::Liquid,
                NodeKind::Component,
                name.to_string(),
                format!("{}::{}:{}", self.file_path, tag_type, name),
                line,
                line,
                col,
                col + full.as_str().len() as i64,
            );
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
            result.unresolved_references.push(unresolved_ref(
                file_id,
                format!("snippets/{name}.liquid"),
                EdgeKind::References,
                line,
                col,
                self.file_path,
                Language::Liquid,
            ));
        }
    }

    fn extract_sections(&self, result: &mut ExtractionResult, file_id: &str) {
        let regex = Regex::new(r#"\{%[-]?\s*section\s+['"]([^'"]+)['"]"#).unwrap();
        for cap in regex.captures_iter(self.source) {
            let full = cap.get(0).unwrap();
            let name = cap.get(1).unwrap().as_str();
            let line = line_number_for_offset(&self.line_starts, full.start());
            let col = full.start() as i64 - line_start_for(&self.line_starts, line) as i64;
            self.push_import_node(result, file_id, name, full.as_str(), line, col);
            let node = default_node(
                self.file_path,
                Language::Liquid,
                NodeKind::Component,
                name.to_string(),
                format!("{}::section:{}", self.file_path, name),
                line,
                line,
                col,
                col + full.as_str().len() as i64,
            );
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
            result.unresolved_references.push(unresolved_ref(
                file_id,
                format!("sections/{name}.liquid"),
                EdgeKind::References,
                line,
                col,
                self.file_path,
                Language::Liquid,
            ));
        }
    }

    fn extract_schema(&self, result: &mut ExtractionResult, file_id: &str) {
        let regex = Regex::new(r"(?s)\{%[-]?\s*schema\s*[-]?%\}(.*?)\{%[-]?\s*endschema\s*[-]?%\}")
            .unwrap();
        for cap in regex.captures_iter(self.source) {
            let full = cap.get(0).unwrap();
            let content = cap.get(1).map_or("", |m| m.as_str());
            let start_line = line_number_for_offset(&self.line_starts, full.start());
            let end_line = line_number_for_offset(&self.line_starts, full.end());
            let schema_name = schema_name(content);
            let mut node = default_node(
                self.file_path,
                Language::Liquid,
                NodeKind::Constant,
                schema_name.clone(),
                format!("{}::schema:{}", self.file_path, schema_name),
                start_line,
                end_line,
                full.start() as i64 - line_start_for(&self.line_starts, start_line) as i64,
                0,
            );
            node.docstring = None;
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
        }
    }

    fn extract_assignments(&self, result: &mut ExtractionResult, file_id: &str) {
        let regex = Regex::new(r"\{%[-]?\s*assign\s+(\w+)\s*=").unwrap();
        for cap in regex.captures_iter(self.source) {
            let full = cap.get(0).unwrap();
            let name = cap.get(1).unwrap().as_str();
            let line = line_number_for_offset(&self.line_starts, full.start());
            let col = full.start() as i64 - line_start_for(&self.line_starts, line) as i64;
            let node = default_node(
                self.file_path,
                Language::Liquid,
                NodeKind::Variable,
                name.to_string(),
                format!("{}::{}", self.file_path, name),
                line,
                line,
                col,
                col + full.as_str().len() as i64,
            );
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
        }
    }

    fn push_import_node(
        &self,
        result: &mut ExtractionResult,
        file_id: &str,
        name: &str,
        signature: &str,
        line: i64,
        col: i64,
    ) {
        let mut node = default_node(
            self.file_path,
            Language::Liquid,
            NodeKind::Import,
            name.to_string(),
            format!("{}::import:{}", self.file_path, name),
            line,
            line,
            col,
            col + signature.len() as i64,
        );
        node.signature = Some(signature.to_string());
        let node_id = node.id.clone();
        result.nodes.push(node);
        result.edges.push(contains_edge(file_id, &node_id));
    }
}

fn schema_name(content: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return "schema".to_string();
    };
    match value.get("name") {
        Some(serde_json::Value::String(name)) => name.clone(),
        Some(serde_json::Value::Object(map)) => map
            .get("en")
            .and_then(|value| value.as_str())
            .or_else(|| map.values().find_map(|value| value.as_str()))
            .unwrap_or("schema")
            .to_string(),
        _ => "schema".to_string(),
    }
}
