use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use regex::Regex;

use crate::embedded::shared::{
    contains_edge, default_node, empty_result, file_like_node, line_number_for_offset, line_starts,
    unresolved_ref,
};

pub struct MyBatisExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> MyBatisExtractor<'a> {
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
        let file_node = file_like_node(self.file_path, self.source, Language::Xml);
        let file_id = file_node.id.clone();
        result.nodes.push(file_node);

        if let Some((namespace, body_start, body_end)) = self.find_mapper_root() {
            self.extract_mapper(&mut result, &file_id, &namespace, body_start, body_end);
        }
        result.duration_ms += start.elapsed().as_millis() as i64;
        result
    }

    fn find_mapper_root(&self) -> Option<(String, usize, usize)> {
        let open_re = Regex::new(r#"<mapper\b([^>]*)>"#).unwrap();
        let ns_re = Regex::new(r#"\bnamespace\s*=\s*"([^"]+)""#).unwrap();
        let open = open_re.find(self.source)?;
        let attrs = open_re
            .captures(open.as_str())
            .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))?;
        let namespace = ns_re
            .captures(&attrs)
            .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))?;
        let body_start = open.end();
        let body_end = self.source[body_start..]
            .find("</mapper>")
            .map_or(self.source.len(), |idx| body_start + idx);
        Some((namespace, body_start, body_end))
    }

    fn extract_mapper(
        &self,
        result: &mut ExtractionResult,
        file_id: &str,
        namespace: &str,
        body_start: usize,
        body_end: usize,
    ) {
        let open_re = Regex::new(r#"<(select|insert|update|delete|sql)\b([^>]*)>"#).unwrap();
        let id_re = Regex::new(r#"\bid\s*=\s*"([^"]+)""#).unwrap();
        let body = &self.source[body_start..body_end];
        let mut search_start = 0;
        while let Some(open) = open_re.find(&body[search_start..]) {
            let open_abs_body = search_start + open.start();
            let full_open = &body[search_start + open.start()..search_start + open.end()];
            let Some(captures) = open_re.captures(full_open) else {
                search_start += open.end();
                continue;
            };
            let elem_type = captures.get(1).unwrap().as_str();
            let attrs = captures.get(2).map_or("", |m| m.as_str());
            let close_tag = format!("</{elem_type}>");
            let content_start_body = search_start + open.end();
            let Some(close_rel) = body[content_start_body..].find(&close_tag) else {
                search_start += open.end();
                continue;
            };
            let close_abs_body = content_start_body + close_rel;
            let full_end_body = close_abs_body + close_tag.len();
            search_start = full_end_body;

            let Some(id) = id_re
                .captures(attrs)
                .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            else {
                continue;
            };
            let absolute_index = body_start + open_abs_body;
            let start_line = line_number_for_offset(&self.line_starts, absolute_index);
            let end_line = line_number_for_offset(&self.line_starts, body_start + full_end_body);
            let qualified = format!("{namespace}::{id}");
            let elem_body = &body[content_start_body..close_abs_body];
            let mut node = default_node(
                self.file_path,
                Language::Xml,
                NodeKind::Method,
                id,
                qualified.clone(),
                start_line,
                end_line,
                0,
                0,
            );
            node.signature = Some(build_signature(elem_type, attrs));
            node.docstring = Some(preview_sql(elem_body));
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
            self.extract_includes(
                result,
                &node_id,
                namespace,
                elem_body,
                body_start + content_start_body,
            );
        }
    }

    fn extract_includes(
        &self,
        result: &mut ExtractionResult,
        node_id: &str,
        namespace: &str,
        elem_body: &str,
        elem_body_abs: usize,
    ) {
        let include_re = Regex::new(r#"<include\b[^>]*\brefid\s*=\s*"([^"]+)""#).unwrap();
        for cap in include_re.captures_iter(elem_body) {
            let full = cap.get(0).unwrap();
            let refid = cap.get(1).unwrap().as_str();
            let ref_qualified = if refid.contains('.') {
                refid.replace('.', "::")
            } else {
                format!("{namespace}::{refid}")
            };
            let line = line_number_for_offset(&self.line_starts, elem_body_abs + full.start());
            result.unresolved_references.push(unresolved_ref(
                node_id,
                ref_qualified,
                EdgeKind::References,
                line,
                0,
                self.file_path,
                Language::Xml,
            ));
        }
    }
}

fn build_signature(elem_type: &str, attrs: &str) -> String {
    if elem_type == "sql" {
        return "<sql>".to_string();
    }
    let result_re = Regex::new(r#"\bresultType\s*=\s*"([^"]+)""#).unwrap();
    let param_re = Regex::new(r#"\bparameterType\s*=\s*"([^"]+)""#).unwrap();
    let mut parts = vec![elem_type.to_ascii_uppercase()];
    if let Some(param) = param_re.captures(attrs).and_then(|cap| cap.get(1)) {
        parts.push(format!("param={}", param.as_str()));
    }
    if let Some(result) = result_re.captures(attrs).and_then(|cap| cap.get(1)) {
        parts.push(format!("result={}", result.as_str()));
    }
    parts.join(" ")
}

fn preview_sql(body: &str) -> String {
    Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(body, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}
