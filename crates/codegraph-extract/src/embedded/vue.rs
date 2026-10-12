use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{
    Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedRef,
};
use regex::Regex;
use std::path::Path;

use crate::embedded::shared::{
    ScriptFold, contains_edge, empty_result, fold_script_result, sfc_file_node,
};

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
    is_setup: bool,
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

        // The SFC's one file node holds the component (upstream #2268).
        let file_node = sfc_file_node(self.file_path, self.source, Language::Vue);
        let component_node = self.create_component_node();
        let component_id = component_node.id.clone();
        self.edges.push(contains_edge(&file_node.id, &component_id));
        self.nodes.push(file_node);
        self.nodes.push(component_node);

        let script_blocks = self.extract_script_blocks();

        for block in script_blocks {
            self.process_script_block(&block, &component_id);
        }

        self.extract_template_components(&component_id);

        // Calls the template makes (`{{ useBar(link) }}`, `:to="localePath(x)"`,
        // `@click="save(item)"`) are the component's, like the calls its
        // `<script setup>` makes (upstream #2357).
        self.extract_template_calls(&component_id);

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
                is_setup,
                is_typescript,
            });
        }

        blocks
    }

    /// A script block is extracted by the TypeScript (or JavaScript)
    /// extractor, as upstream's Vue extractor delegates it (G8), and folded
    /// into the component at the block's file lines; `<script setup>` runs
    /// per component instance.
    fn process_script_block(&mut self, block: &ScriptBlock, component_id: &str) {
        let language = if block.is_typescript {
            Language::TypeScript
        } else {
            Language::JavaScript
        };
        let mut delegated =
            crate::engine::extract_source(self.file_path, &block.content, Some(language));
        // An Options API component's functions (`methods`, `computed`,
        // `watch`, lifecycle hooks) are object-literal members the script
        // extractor leaves to the file: name each one, and hand it the calls
        // written inside it (upstream #2140).
        if !block.is_setup {
            self.add_options_members(block, &mut delegated, component_id);
        }
        let mut merged = empty_result(0);
        fold_script_result(
            &mut merged,
            delegated,
            &ScriptFold {
                file_path: self.file_path,
                component_id,
                line_offset: block.line_offset,
                language: Language::Vue,
                per_instance: block.is_setup,
            },
        );
        self.nodes.append(&mut merged.nodes);
        self.edges.append(&mut merged.edges);
        self.unresolved_references
            .append(&mut merged.unresolved_references);
        self.errors.append(&mut merged.errors);
    }

    /// Method nodes for an Options API component's members (see
    /// [`crate::embedded::vue_options_api`]), and the references and edges
    /// written inside each, which the script extractor attributed to the
    /// file, re-attributed to it. Lines are block-relative here; the fold
    /// offsets them with the rest.
    fn add_options_members(
        &self,
        block: &ScriptBlock,
        result: &mut ExtractionResult,
        component_id: &str,
    ) {
        let members = crate::embedded::vue_options_api::vue_options_members(&block.content);
        if members.is_empty() {
            return;
        }
        let owner = self
            .nodes
            .iter()
            .find(|node| node.id == component_id)
            .map_or_else(|| "component".to_string(), |node| node.name.clone());
        let content = block.content.as_str();
        let line_at = |offset: usize| content[..offset].matches('\n').count() as i64 + 1;
        let column_at = |offset: usize| {
            content[..offset]
                .rfind('\n')
                .map_or(offset, |newline| offset - newline - 1) as i64
        };
        let created: Vec<Node> = members
            .iter()
            .map(|member| {
                let start_line = line_at(member.start);
                Node {
                    id: generate_node_id(
                        self.file_path,
                        NodeKind::Method,
                        &format!("{owner}.{}", member.name),
                        u32::try_from(start_line + block.line_offset).unwrap_or(u32::MAX),
                    ),
                    kind: NodeKind::Method,
                    name: member.name.clone(),
                    qualified_name: format!("{owner}::{}", member.name),
                    file_path: self.file_path.to_string(),
                    language: Language::Vue,
                    start_line,
                    end_line: line_at(member.end),
                    start_column: column_at(member.start),
                    end_column: column_at(member.end),
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
            })
            .collect();
        // The innermost member for a line: `computed: { x: { get() {…} } }`
        // is one member.
        let member_at = |line: i64| -> Option<&Node> {
            let mut best: Option<&Node> = None;
            for node in &created {
                if node.start_line <= line
                    && node.end_line >= line
                    && best.is_none_or(|best| node.start_line >= best.start_line)
                {
                    best = Some(node);
                }
            }
            best
        };
        // What the script extractor attributed to the file.
        let file_id = format!("file:{}", self.file_path);
        for reference in &mut result.unresolved_references {
            if reference.from_node_id != file_id {
                continue;
            }
            if let Some(member) = member_at(reference.line) {
                reference.from_node_id = member.id.clone();
            }
        }
        for edge in &mut result.edges {
            if edge.kind == EdgeKind::Contains || edge.source != file_id {
                continue;
            }
            if let Some(member) = edge.line.and_then(member_at) {
                edge.source = member.id.clone();
            }
        }
        result.nodes.extend(created);
    }

    /// Calls written in the `<template>` (see
    /// [`crate::embedded::vue_template_calls`]), each made by the component
    /// at the line it is written on, the same `calls` reference a call in
    /// `<script setup>` makes. A template calls an Options API component's
    /// own methods by their bare name (`{{ price(item) }}`,
    /// `@click="save(form)"`) where script code writes `this.save()`;
    /// resolution binds a bare call to a component method only through
    /// `this.`, so these are linked here, where the method is known to be this
    /// component's.
    fn extract_template_calls(&mut self, component_id: &str) {
        let calls = crate::embedded::vue_template_calls::vue_template_calls(self.source);
        if calls.is_empty() {
            return;
        }
        let owner = self
            .nodes
            .iter()
            .find(|node| node.id == component_id)
            .map(|node| node.name.clone())
            .unwrap_or_default();
        let own_methods: std::collections::HashMap<String, String> = self
            .nodes
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Method
                    && node.qualified_name == format!("{owner}::{}", node.name)
            })
            .map(|node| (node.name.clone(), node.id.clone()))
            .collect();
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(self.source.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let mut line = 0usize;
        for call in calls {
            // Calls come in source order, so the line only moves forward.
            while line + 1 < line_starts.len() && line_starts[line + 1] <= call.offset {
                line += 1;
            }
            let column = (call.offset - line_starts[line]) as i64;
            let own_method = (!call.name.contains('.'))
                .then(|| own_methods.get(&call.name))
                .flatten();
            if let Some(method_id) = own_method {
                self.edges.push(Edge {
                    id: None,
                    source: component_id.to_string(),
                    target: method_id.clone(),
                    kind: EdgeKind::Calls,
                    metadata: None,
                    line: Some(line as i64 + 1),
                    col: Some(column),
                    provenance: None,
                });
                continue;
            }
            self.unresolved_references.push(UnresolvedRef {
                id: None,
                from_node_id: component_id.to_string(),
                reference_name: call.name,
                reference_kind: EdgeKind::Calls,
                line: line as i64 + 1,
                col: column,
                candidates: None,
                file_path: self.file_path.to_string(),
                language: Language::Vue,
                is_function_ref: false,
                reference_subkind: None,
            });
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
                    reference_subkind: None,
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
