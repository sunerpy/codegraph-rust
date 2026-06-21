use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{
    Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedRef,
};
use regex::Regex;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ScriptBlock {
    pub content: String,
    pub line_offset: i64,
    pub attrs: String,
    pub is_typescript: bool,
}

pub fn empty_result(duration_ms: i64) -> ExtractionResult {
    ExtractionResult {
        nodes: Vec::new(),
        edges: Vec::new(),
        unresolved_references: Vec::new(),
        errors: Vec::new(),
        duration_ms,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn default_node(
    file_path: &str,
    language: Language,
    kind: NodeKind,
    name: String,
    qualified_name: String,
    start_line: i64,
    end_line: i64,
    start_column: i64,
    end_column: i64,
) -> Node {
    Node {
        id: generate_node_id(file_path, kind, &name, start_line.max(1) as u32),
        kind,
        name,
        qualified_name,
        file_path: file_path.to_string(),
        language,
        start_line,
        end_line,
        start_column,
        end_column,
        docstring: None,
        signature: None,
        visibility: None,
        is_exported: false,
        is_async: false,
        is_static: false,
        is_abstract: false,
        decorators: vec![],
        type_parameters: vec![],
        return_type: None,
        updated_at: 0,
    }
}

pub fn component_node(file_path: &str, source: &str, language: Language, suffix: &str) -> Node {
    let lines: Vec<&str> = source.split('\n').collect();
    let file_name = Path::new(file_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let component_name = file_name
        .strip_suffix(suffix)
        .unwrap_or(&file_name)
        .to_string();
    let mut node = default_node(
        file_path,
        language,
        NodeKind::Component,
        component_name.clone(),
        format!("{file_path}::{component_name}"),
        1,
        lines.len() as i64,
        0,
        lines.last().map_or(0, |line| line.len()) as i64,
    );
    node.is_exported = true;
    node
}

pub fn file_like_node(file_path: &str, source: &str, language: Language) -> Node {
    let lines: Vec<&str> = source.split('\n').collect();
    default_node(
        file_path,
        language,
        NodeKind::File,
        Path::new(file_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        file_path.to_string(),
        1,
        lines.len().max(1) as i64,
        0,
        lines.last().map_or(0, |line| line.len()) as i64,
    )
}

pub fn contains_edge(source: &str, target: &str) -> Edge {
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

pub fn unresolved_ref(
    from_node_id: &str,
    reference_name: String,
    reference_kind: EdgeKind,
    line: i64,
    col: i64,
    file_path: &str,
    language: Language,
) -> UnresolvedRef {
    UnresolvedRef {
        id: None,
        from_node_id: from_node_id.to_string(),
        reference_name,
        reference_kind,
        line,
        col,
        candidates: None,
        file_path: file_path.to_string(),
        language,
        is_function_ref: false,
    }
}

pub fn extract_script_blocks(source: &str) -> Vec<ScriptBlock> {
    let mut blocks = Vec::new();
    let script_regex = Regex::new(r"(?s)<script(\s[^>]*)?>(.*?)</script>").unwrap();
    let lang_ts_regex = Regex::new(r#"lang\s*=\s*["'](ts|typescript)["']"#).unwrap();

    for cap in script_regex.captures_iter(source) {
        let attrs = cap.get(1).map_or("", |m| m.as_str()).to_string();
        let content = cap.get(2).map_or("", |m| m.as_str()).to_string();
        let full_match = cap.get(0).unwrap();
        let before_script = &source[..full_match.start()];
        let script_tag_line = before_script.matches('\n').count();
        let opening_tag = &full_match.as_str()[..full_match.as_str().find('>').unwrap() + 1];
        let opening_tag_lines = opening_tag.matches('\n').count();

        blocks.push(ScriptBlock {
            content,
            line_offset: (script_tag_line + opening_tag_lines) as i64,
            is_typescript: lang_ts_regex.is_match(&attrs),
            attrs,
        });
    }

    blocks
}

pub fn block_line_ranges(source: &str, tag_names: &[&str]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for tag_name in tag_names {
        let pattern = format!(r"(?is)<{}(\s[^>]*)?>.*?</{}>", tag_name, tag_name);
        let regex = Regex::new(&pattern).unwrap();
        for mat in regex.find_iter(source) {
            let start_line = source[..mat.start()].matches('\n').count();
            let end_line = start_line + mat.as_str().matches('\n').count();
            ranges.push((start_line, end_line));
        }
    }
    ranges
}

pub fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (idx, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

pub fn line_number_for_offset(line_starts: &[usize], offset: usize) -> i64 {
    match line_starts.binary_search(&offset) {
        Ok(idx) => idx as i64 + 1,
        Err(0) => 1,
        Err(idx) => idx as i64,
    }
}

pub fn line_start_for(line_starts: &[usize], line_number: i64) -> usize {
    if line_number <= 0 {
        return 0;
    }
    line_starts
        .get((line_number - 1) as usize)
        .copied()
        .unwrap_or(0)
}

pub fn merge_delegated_result(
    target: &mut ExtractionResult,
    mut delegated: ExtractionResult,
    parent_id: &str,
    file_path: &str,
    language: Language,
    line_offset: i64,
) {
    let file_ids = delegated
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::File)
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();

    for mut node in delegated.nodes.drain(..) {
        if node.kind == NodeKind::File {
            continue;
        }
        node.start_line += line_offset;
        node.end_line += line_offset;
        node.file_path = file_path.to_string();
        node.language = language;
        let node_id = node.id.clone();
        target.nodes.push(node);
        target.edges.push(contains_edge(parent_id, &node_id));
    }

    for mut edge in delegated.edges.drain(..) {
        if file_ids.iter().any(|id| id == &edge.target) {
            continue;
        }
        if file_ids.iter().any(|id| id == &edge.source) {
            edge.source = parent_id.to_string();
        }
        if let Some(line) = edge.line.as_mut() {
            *line += line_offset;
        }
        target.edges.push(edge);
    }

    for mut reference in delegated.unresolved_references.drain(..) {
        reference.line += line_offset;
        reference.file_path = file_path.to_string();
        reference.language = language;
        target.unresolved_references.push(reference);
    }

    target.errors.append(&mut delegated.errors);
    target.duration_ms += delegated.duration_ms;
}
