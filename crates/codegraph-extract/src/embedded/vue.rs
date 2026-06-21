use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{
    Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedRef,
};
use regex::Regex;
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

// Vue built-in components to skip
fn is_vue_builtin(name: &str) -> bool {
    matches!(
        name,
        "Transition"
            | "TransitionGroup"
            | "KeepAlive"
            | "Suspense"
            | "Teleport"
            | "Component"
            | "Slot"
    )
}

fn kebab_to_pascal(name: &str) -> String {
    name.split('-')
        .map(|part| {
            if part.is_empty() {
                String::new()
            } else {
                let mut c = part.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            }
        })
        .collect()
}

struct ScriptBlock {
    content: String,
    line_offset: i64, // 0-indexed original .vue line containing content byte 0
    _is_setup: bool,
    is_typescript: bool,
}

pub struct VueExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedRef>,
    errors: Vec<String>,
}

impl<'a> VueExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self {
            file_path,
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn extract(mut self) -> ExtractionResult {
        let start_time = std::time::Instant::now();

        let component_node = self.create_component_node();
        let component_id = component_node.id.clone();
        self.nodes.push(component_node);

        let script_blocks = self.extract_script_blocks();

        for block in script_blocks {
            self.process_script_block(&block, &component_id);
        }

        self.extract_template_components(&component_id);

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start_time.elapsed().as_millis() as i64,
        }
    }

    fn create_component_node(&self) -> Node {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let file_name = Path::new(self.file_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let component_name = file_name
            .strip_suffix(".vue")
            .unwrap_or(&file_name)
            .to_string();

        let id = generate_node_id(self.file_path, NodeKind::Component, &component_name, 1);

        Node {
            id,
            kind: NodeKind::Component,
            name: component_name.clone(),
            qualified_name: format!("{}::{}", self.file_path, component_name),
            file_path: self.file_path.to_string(),
            language: Language::Vue,
            start_line: 1,
            end_line: lines.len() as i64,
            start_column: 0,
            end_column: lines.last().map(|l| l.len()).unwrap_or(0) as i64,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: vec![],
            type_parameters: vec![],
            return_type: None,
            updated_at: 0,
        }
    }

    fn extract_script_blocks(&self) -> Vec<ScriptBlock> {
        let mut blocks = Vec::new();
        let script_regex = Regex::new(r"(?s)<script(\s[^>]*)?>(.*?)</script>").unwrap();
        let lang_ts_regex = Regex::new(r#"lang\s*=\s*["'](ts|typescript)["']"#).unwrap();
        let setup_regex = Regex::new(r"\bsetup\b").unwrap();

        for cap in script_regex.captures_iter(self.source) {
            let attrs = cap.get(1).map_or("", |m| m.as_str());
            let content = cap.get(2).map_or("", |m| m.as_str());

            let is_typescript = lang_ts_regex.is_match(attrs);
            let is_setup = setup_regex.is_match(attrs);

            let full_match = cap.get(0).unwrap();
            let before_script = &self.source[..full_match.start()];
            let script_tag_line = before_script.matches('\n').count();

            let opening_tag = &full_match.as_str()[..full_match.as_str().find('>').unwrap() + 1];
            let opening_tag_lines = opening_tag.matches('\n').count();

            let content_line_offset = script_tag_line + opening_tag_lines;

            blocks.push(ScriptBlock {
                content: content.to_string(),
                line_offset: content_line_offset as i64,
                _is_setup: is_setup,
                is_typescript,
            });
        }

        blocks
    }

    fn process_script_block(&mut self, block: &ScriptBlock, component_id: &str) {
        let mut parser = Parser::new();
        let language = if block.is_typescript {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        } else {
            tree_sitter_javascript::LANGUAGE.into()
        };
        parser.set_language(&language).unwrap();

        let tree = parser.parse(&block.content, None).unwrap();

        // Simple query to find functions and imports for the prototype
        let query_str = r#"
            (function_declaration
                name: (identifier) @func.name) @func
            
            (import_statement
                source: (string) @import.source) @import
        "#;

        let query = Query::new(&language, query_str).unwrap();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), block.content.as_bytes());

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node;
                let capture_name = query.capture_names()[capture.index as usize];

                if capture_name == "func" {
                    let name_node = m
                        .captures
                        .iter()
                        .find(|c| query.capture_names()[c.index as usize] == "func.name")
                        .unwrap()
                        .node;
                    let name = name_node
                        .utf8_text(block.content.as_bytes())
                        .unwrap()
                        .to_string();

                    let start_pos = node.start_position();
                    let end_pos = node.end_position();

                    let id = generate_node_id(
                        self.file_path,
                        NodeKind::Function,
                        &name,
                        start_pos.row as u32 + 1,
                    );
                    let qualified_name = format!("{}::{}", self.file_path, name);

                    self.nodes.push(Node {
                        id: id.clone(),
                        kind: NodeKind::Function,
                        name,
                        qualified_name,
                        file_path: self.file_path.to_string(),
                        language: Language::Vue,
                        start_line: start_pos.row as i64 + block.line_offset + 1, // 1-indexed
                        end_line: end_pos.row as i64 + block.line_offset + 1,
                        start_column: start_pos.column as i64 + 1,
                        end_column: end_pos.column as i64 + 1,
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
                    });

                    self.edges.push(Edge {
                        id: None,
                        source: component_id.to_string(),
                        target: id,
                        kind: EdgeKind::Contains,
                        metadata: None,
                        line: None,
                        col: None,
                        provenance: None,
                    });
                } else if capture_name == "import" {
                    let source_node = m
                        .captures
                        .iter()
                        .find(|c| query.capture_names()[c.index as usize] == "import.source")
                        .unwrap()
                        .node;
                    let source_text = source_node
                        .utf8_text(block.content.as_bytes())
                        .unwrap()
                        .to_string();
                    let clean_source = source_text
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string();

                    let start_pos = node.start_position();

                    self.unresolved_references.push(UnresolvedRef {
                        id: None,
                        from_node_id: component_id.to_string(),
                        reference_name: clean_source,
                        reference_kind: EdgeKind::Imports,
                        line: start_pos.row as i64 + block.line_offset + 1,
                        col: start_pos.column as i64 + 1,
                        candidates: None,
                        file_path: self.file_path.to_string(),
                        language: Language::Vue,
                        is_function_ref: false,
                    });
                }
            }
        }
    }

    fn extract_template_components(&mut self, component_id: &str) {
        let mut covered_ranges = Vec::new();
        let block_regex =
            Regex::new(r"(?s)<script(\s[^>]*)?>.*?</script>|<style(\s[^>]*)?>.*?</style>").unwrap();

        for mat in block_regex.find_iter(self.source) {
            let start_line = self.source[..mat.start()].matches('\n').count();
            let end_line = start_line + mat.as_str().matches('\n').count();
            covered_ranges.push((start_line, end_line));
        }

        let lines: Vec<&str> = self.source.split('\n').collect();
        let tag_regex = Regex::new(r"<([A-Za-z][A-Za-z0-9_-]*)\b").unwrap();

        for (line_idx, line) in lines.iter().enumerate() {
            if covered_ranges
                .iter()
                .any(|&(start, end)| line_idx >= start && line_idx <= end)
            {
                continue;
            }

            for cap in tag_regex.captures_iter(line) {
                let raw = cap.get(1).unwrap().as_str();
                let component_name = if raw.chars().next().unwrap().is_uppercase() {
                    raw.to_string()
                } else if raw.contains('-') {
                    kebab_to_pascal(raw)
                } else {
                    continue;
                };

                if is_vue_builtin(&component_name) {
                    continue;
                }

                self.unresolved_references.push(UnresolvedRef {
                    id: None,
                    from_node_id: component_id.to_string(),
                    reference_name: component_name,
                    reference_kind: EdgeKind::References,
                    line: line_idx as i64 + 1,
                    col: cap.get(0).unwrap().start() as i64 + 1,
                    candidates: None,
                    file_path: self.file_path.to_string(),
                    language: Language::Vue,
                    is_function_ref: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn vue_prototype() {
        let source = fs::read_to_string("tests/fixtures/sample.vue").unwrap();
        let extractor = VueExtractor::new("tests/fixtures/sample.vue", &source);
        let result = extractor.extract();

        // Check component node
        let component = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Component)
            .unwrap();
        assert_eq!(component.name, "sample");
        assert_eq!(component.start_line, 1);

        // Check function node
        let func = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function)
            .unwrap();
        assert_eq!(func.name, "doSomething");
        // In sample.vue, <script> is on line 11.
        // import { ref } is line 12.
        // import MyComponent is line 13.
        // empty line 14.
        // export function doSomething() is line 15.
        assert_eq!(func.start_line, 15);
        println!(
            "asserted Vue original line: function {} starts at {}",
            func.name, func.start_line
        );

        // Check import
        let import = result
            .unresolved_references
            .iter()
            .find(|r| {
                r.reference_kind == EdgeKind::Imports && r.reference_name == "./MyComponent.vue"
            })
            .unwrap();
        assert_eq!(import.line, 13);
        println!(
            "asserted Vue original line: import {} is at {}",
            import.reference_name, import.line
        );

        // Check template components
        let my_comp = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::References && r.reference_name == "MyComponent")
            .unwrap();
        assert_eq!(my_comp.line, 3);

        let my_other_comp = result
            .unresolved_references
            .iter()
            .find(|r| {
                r.reference_kind == EdgeKind::References && r.reference_name == "MyOtherComponent"
            })
            .unwrap();
        assert_eq!(my_other_comp.line, 4);

        // Transition should be skipped
        let transition = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::References && r.reference_name == "Transition");
        assert!(transition.is_none());
    }
}
