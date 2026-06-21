//! Astro (`.astro`) embedded extractor.
//!
//! Ports `upstream extraction/astro-extractor.ts` (upstream 823ffd1c /
//! #768). An `.astro` file has a `---`-fenced TypeScript frontmatter block, a
//! JSX-like template, and optional client `<script>` blocks. Like Vue/Svelte
//! it has no grammar of its own — frontmatter + scripts delegate to the TS
//! extractor (offsets remapped), and the template contributes `{fn(...)}` call
//! refs plus `<Component>` usage refs. The `.astro` file itself is one
//! `component` node. Astro's framework resolution (`astro:*` imports, the
//! `Astro` global, `src/pages` routing) is part of the deferred
//! `FrameworkResolver` layer (see KNOWN_DIFFS) and is NOT ported here.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language};
use regex::Regex;

use crate::embedded::shared::{component_node, contains_edge, empty_result, unresolved_ref};

/// Compiler-provided (`<Fragment>`) or `astro:components`-shipped components,
/// not user code (astro-extractor.ts:10).
const ASTRO_BUILTIN_COMPONENTS: &[&str] = &["Fragment", "Code", "Debug"];

pub struct AstroExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
}

impl<'a> AstroExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self { file_path, source }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = empty_result(0);
        let component = component_node(self.file_path, self.source, Language::Astro, ".astro");
        let component_id = component.id.clone();
        result.nodes.push(component);

        let frontmatter = self.extract_frontmatter();
        if let Some(fm) = &frontmatter {
            let delegated = crate::engine::extract_source(
                self.file_path,
                &fm.content,
                Some(Language::TypeScript),
            );
            self.merge_astro_block(&mut result, delegated, &component_id, fm.start_line);
        }

        for block in self.extract_script_blocks() {
            let delegated = crate::engine::extract_source(
                self.file_path,
                &block.content,
                Some(Language::TypeScript),
            );
            self.merge_astro_block(&mut result, delegated, &component_id, block.start_line);
        }

        let covered = self.covered_ranges(frontmatter.as_ref());
        self.extract_template_calls(&mut result, &component_id, &covered);
        self.extract_template_components(&mut result, &component_id, &covered);

        result.duration_ms += start.elapsed().as_millis() as i64;
        result
    }

    /// Merge a delegated TS block (frontmatter or `<script>`) into the result.
    /// Ports `processScriptContent` (astro-extractor.ts:185-235): EVERY
    /// delegated node is kept (including its `file:` node) with lines offset and
    /// language set to astro, and each gets an added `component -> node`
    /// contains edge; delegated edges and refs are kept as-is (only line
    /// offset), so the TS extraction's own `file -> symbol` / `file -> import`
    /// edges survive — yielding the upstream double containment.
    fn merge_astro_block(
        &self,
        result: &mut ExtractionResult,
        mut delegated: ExtractionResult,
        component_id: &str,
        line_offset: i64,
    ) {
        for mut node in delegated.nodes.drain(..) {
            node.start_line += line_offset;
            node.end_line += line_offset;
            node.language = Language::Astro;
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(component_id, &node_id));
        }
        for mut edge in delegated.edges.drain(..) {
            if let Some(line) = edge.line.as_mut() {
                *line += line_offset;
            }
            result.edges.push(edge);
        }
        for mut reference in delegated.unresolved_references.drain(..) {
            reference.line += line_offset;
            reference.file_path = self.file_path.to_string();
            reference.language = Language::Astro;
            result.unresolved_references.push(reference);
        }
        result.errors.append(&mut delegated.errors);
        result.duration_ms += delegated.duration_ms;
    }

    /// Frontmatter is the content between the opening `---` fence (the first
    /// non-blank line) and the closing `---` fence. An unclosed fence yields
    /// none. Ports `extractFrontmatter` (astro-extractor.ts:123-152).
    fn extract_frontmatter(&self) -> Option<Frontmatter> {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let mut open_idx = None;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "---" {
                open_idx = Some(i);
            }
            break;
        }
        let open_idx = open_idx?;
        let close_idx = (open_idx + 1..lines.len()).find(|&i| lines[i].trim() == "---")?;
        Some(Frontmatter {
            content: lines[open_idx + 1..close_idx].join("\n"),
            // 0-indexed content start; merge offset is added to delegated
            // (1-based) lines, so the upstream `startLine = openIdx + 1`.
            start_line: (open_idx + 1) as i64,
            open_idx,
            close_idx,
        })
    }

    /// `<script>` block contents + their 0-indexed content start line. Ports
    /// `extractScriptBlocks` (astro-extractor.ts:157-180).
    fn extract_script_blocks(&self) -> Vec<ScriptBlock> {
        static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(r"(?s)<script(\s[^>]*)?>(.*?)</script>").expect("astro script regex")
        });
        let mut blocks = Vec::new();
        for m in re.captures_iter(self.source) {
            let whole = m.get(0).unwrap();
            let content = m.get(2).map(|c| c.as_str().to_string()).unwrap_or_default();
            let before = &self.source[..whole.start()];
            let script_tag_line = before.matches('\n').count();
            let opening_tag = &whole.as_str()[..whole.as_str().find('>').map_or(0, |i| i + 1)];
            let opening_tag_lines = opening_tag.matches('\n').count();
            blocks.push(ScriptBlock {
                content,
                start_line: (script_tag_line + opening_tag_lines) as i64,
            });
        }
        blocks
    }

    /// 0-indexed inclusive line ranges the template scan skips: frontmatter +
    /// `<script>`/`<style>` blocks. Ports `getCoveredRanges` (astro-extractor.ts:255-265).
    fn covered_ranges(&self, frontmatter: Option<&Frontmatter>) -> Vec<(usize, usize)> {
        let mut covered = Vec::new();
        if let Some(fm) = frontmatter {
            covered.push((fm.open_idx, fm.close_idx));
        }
        static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(r"(?s)<(script|style)(\s[^>]*)?>.*?</(script|style)>")
                .expect("astro tag regex")
        });
        for m in re.captures_iter(self.source) {
            let whole = m.get(0).unwrap();
            let start_line = self.source[..whole.start()].matches('\n').count();
            let end_line = start_line + whole.as_str().matches('\n').count();
            covered.push((start_line, end_line));
        }
        covered
    }

    /// Function calls in template expressions (`{fn(...)}`). Ports
    /// `extractTemplateCalls` (astro-extractor.ts:278-326).
    fn extract_template_calls(
        &self,
        result: &mut ExtractionResult,
        component_id: &str,
        covered: &[(usize, usize)],
    ) {
        static EXPR_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        static OPEN_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        static CALL_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let expr_re = EXPR_RE.get_or_init(|| Regex::new(r"\{([^}/][^}]*)\}").unwrap());
        let open_re = OPEN_RE.get_or_init(|| Regex::new(r"\{([^}/][^}]*)$").unwrap());
        let call_re = CALL_RE.get_or_init(|| Regex::new(r"\b([a-zA-Z_$][\w$.]*)\s*\(").unwrap());

        for (line_idx, line) in self.source.split('\n').enumerate() {
            if covered.iter().any(|&(s, e)| line_idx >= s && line_idx <= e) {
                continue;
            }
            let mut exprs: Vec<(String, usize)> = Vec::new();
            for cap in expr_re.captures_iter(line) {
                if let Some(g) = cap.get(1) {
                    exprs.push((g.as_str().to_string(), cap.get(0).unwrap().start()));
                }
            }
            let stripped = expr_re.replace_all(line, "");
            if let Some(cap) = open_re.captures(&stripped) {
                if let Some(g) = cap.get(1) {
                    exprs.push((g.as_str().to_string(), line.rfind('{').unwrap_or(0)));
                }
            }
            for (text, offset) in exprs {
                for call in call_re.captures_iter(&text) {
                    let Some(name) = call.get(1).map(|m| m.as_str()) else {
                        continue;
                    };
                    if matches!(name, "if" | "await" | "function") {
                        continue;
                    }
                    result.unresolved_references.push(unresolved_ref(
                        component_id,
                        name.to_string(),
                        EdgeKind::Calls,
                        line_idx as i64 + 1,
                        offset as i64 + call.get(0).unwrap().start() as i64,
                        self.file_path,
                        Language::Astro,
                    ));
                }
            }
        }
    }

    /// PascalCase `<Component>` usage refs. Ports `extractTemplateComponents`
    /// (astro-extractor.ts:340-365).
    fn extract_template_components(
        &self,
        result: &mut ExtractionResult,
        component_id: &str,
        covered: &[(usize, usize)],
    ) {
        static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"<([A-Z][a-zA-Z0-9_$]*)\b").unwrap());
        for (line_idx, line) in self.source.split('\n').enumerate() {
            if covered.iter().any(|&(s, e)| line_idx >= s && line_idx <= e) {
                continue;
            }
            for cap in re.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str();
                if ASTRO_BUILTIN_COMPONENTS.contains(&name) {
                    continue;
                }
                result.unresolved_references.push(unresolved_ref(
                    component_id,
                    name.to_string(),
                    EdgeKind::References,
                    line_idx as i64 + 1,
                    cap.get(0).unwrap().start() as i64 + 1,
                    self.file_path,
                    Language::Astro,
                ));
            }
        }
    }
}

struct Frontmatter {
    content: String,
    start_line: i64,
    open_idx: usize,
    close_idx: usize,
}

struct ScriptBlock {
    content: String,
    start_line: i64,
}
