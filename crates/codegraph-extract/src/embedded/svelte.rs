use codegraph_core::types::{EdgeKind, ExtractionResult, Language, NodeKind};
use regex::Regex;

use crate::embedded::shared::{
    ScriptFold, block_line_ranges, component_node, contains_edge, empty_result,
    extract_script_blocks, fold_script_result, sfc_file_node, unresolved_ref,
};

const SVELTE_RUNES: &[&str] = &[
    "$props",
    "$state",
    "$derived",
    "$effect",
    "$bindable",
    "$inspect",
    "$host",
    "$snippet",
];

pub struct SvelteExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
}

impl<'a> SvelteExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self { file_path, source }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = empty_result(0);
        // The SFC's one file node holds the component (upstream #2268).
        let file_node = sfc_file_node(self.file_path, self.source, Language::Svelte);
        let component = component_node(self.file_path, self.source, Language::Svelte, ".svelte");
        let component_id = component.id.clone();
        result
            .edges
            .push(contains_edge(&file_node.id, &component_id));
        result.nodes.push(file_node);
        result.nodes.push(component);

        // Svelte 4's `context="module"`, Svelte 5's bare `module` attribute.
        let module_regex =
            Regex::new(r#"context\s*=\s*["']module["']|(?:^|\s)module(?:[\s=]|$)"#).unwrap();
        for block in extract_script_blocks(self.source) {
            let is_module = module_regex.is_match(&block.attrs);
            let language = if block.is_typescript {
                Language::TypeScript
            } else {
                Language::JavaScript
            };
            let delegated =
                crate::engine::extract_source(self.file_path, &block.content, Some(language));
            // An instance script runs per component instance; a module script
            // runs once, as the file.
            fold_script_result(
                &mut result,
                delegated,
                &ScriptFold {
                    file_path: self.file_path,
                    component_id: &component_id,
                    line_offset: block.line_offset,
                    language: Language::Svelte,
                    per_instance: !is_module,
                },
            );
        }

        self.extract_template_calls(&mut result, &component_id);
        self.extract_template_components(&mut result, &component_id);
        result
            .unresolved_references
            .retain(|reference| !SVELTE_RUNES.contains(&reference.reference_name.as_str()));
        result.duration_ms += start.elapsed().as_millis() as i64;
        result
    }

    fn extract_template_calls(&self, result: &mut ExtractionResult, component_id: &str) {
        let covered = block_line_ranges(self.source, &["script", "style"]);
        let expr_regex = Regex::new(r"\{([^}#/:@][^}]*)\}").unwrap();
        let call_regex = Regex::new(r"\b([a-zA-Z_$][\w$.]*)\s*\(").unwrap();

        for (line_idx, line) in self.source.split('\n').enumerate() {
            if covered
                .iter()
                .any(|&(start, end)| line_idx >= start && line_idx <= end)
            {
                continue;
            }
            for expr in expr_regex.captures_iter(line) {
                let Some(expr_match) = expr.get(1) else {
                    continue;
                };
                for call in call_regex.captures_iter(expr_match.as_str()) {
                    let Some(name) = call.get(1).map(|m| m.as_str()) else {
                        continue;
                    };
                    if SVELTE_RUNES.contains(&name)
                        || matches!(name, "if" | "else" | "each" | "await")
                    {
                        continue;
                    }
                    result.unresolved_references.push(unresolved_ref(
                        component_id,
                        name.to_string(),
                        EdgeKind::Calls,
                        line_idx as i64 + 1,
                        expr_match.start() as i64 + call.get(1).unwrap().start() as i64,
                        self.file_path,
                        Language::Svelte,
                    ));
                }
            }
        }
    }

    fn extract_template_components(&self, result: &mut ExtractionResult, component_id: &str) {
        let covered = block_line_ranges(self.source, &["script", "style"]);
        let tag_regex = Regex::new(r"<([A-Z][a-zA-Z0-9_$]*)\b").unwrap();
        for (line_idx, line) in self.source.split('\n').enumerate() {
            if covered
                .iter()
                .any(|&(start, end)| line_idx >= start && line_idx <= end)
            {
                continue;
            }
            for cap in tag_regex.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str().to_string();
                result.unresolved_references.push(unresolved_ref(
                    component_id,
                    name,
                    EdgeKind::References,
                    line_idx as i64 + 1,
                    cap.get(0).unwrap().start() as i64 + 1,
                    self.file_path,
                    Language::Svelte,
                ));
            }
        }
    }
}

#[allow(dead_code)]
fn _assert_node_kind_component_available(kind: NodeKind) -> bool {
    kind == NodeKind::Component
}
