use std::cell::RefCell;
use std::sync::OnceLock;

use codegraph_core::node_id::{NodeIdAllocator, utf16_column};
use codegraph_core::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind};
use regex::Regex;

use crate::embedded::shared::{
    contains_edge, default_node, empty_result, file_like_node, line_number_for_offset,
    line_start_for, line_starts, unresolved_ref,
};

/// The tags the extractor reads, each as its name pattern and the argument
/// pattern that follows it.
#[derive(Debug, Clone, Copy)]
enum LiquidTag {
    /// `render 'name'` / `include 'name'`.
    Snippet,
    /// `section 'name'`.
    Section,
    /// `assign name =`.
    Assign,
}

impl LiquidTag {
    /// The braced spelling (matched against one whole `{% … %}` tag) and the
    /// bare spelling (matched against one line of a `{% liquid %}` body).
    fn patterns(self) -> &'static (Regex, Regex) {
        static PATTERNS: OnceLock<[(Regex, Regex); 3]> = OnceLock::new();
        let patterns = PATTERNS.get_or_init(|| {
            [
                ("render|include", r#"['"]([^'"]+)['"]"#),
                ("section", r#"['"]([^'"]+)['"]"#),
                ("assign", r"([0-9A-Za-z_]+)\s*="),
            ]
            .map(|(name, argument)| {
                (
                    Regex::new(&format!(r"^\{{%-?\s*({name})\s+{argument}")).unwrap(),
                    Regex::new(&format!(r"^[ \t]*({name})[ \t]+{argument}")).unwrap(),
                )
            })
        });
        &patterns[self as usize]
    }
}

/// One occurrence of a tag in either spelling.
struct TagOccurrence<'s> {
    /// From the tag's start through its argument: `{% render 'card'` for a
    /// braced tag, `render 'card'` for a bare line of a `{% liquid %}` block.
    text: &'s str,
    /// The tag name, then the argument's captures.
    groups: Vec<&'s str>,
    /// Byte offset of `text` in the source.
    offset: usize,
}

pub struct LiquidExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
    line_starts: Vec<usize>,
    node_ids: RefCell<NodeIdAllocator>,
}

impl<'a> LiquidExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self {
            file_path,
            source,
            line_starts: line_starts(source),
            node_ids: RefCell::new(NodeIdAllocator::default()),
        }
    }

    /// Identify `node`, created at byte `offset`, through the per-file
    /// allocator: a later same-kind, same-name node on the same line gains a
    /// column suffix instead of overwriting the first (#1349).
    fn identify(&self, mut node: Node, offset: usize) -> Node {
        node.id = self.node_ids.borrow_mut().generate(
            self.file_path,
            node.kind,
            &node.name,
            node.start_line.max(1) as u32,
            utf16_column(self.source, offset),
        );
        node
    }

    /// Every occurrence of `tag`, in both spellings Liquid allows (upstream
    /// #1906). Inside a `{% liquid %}` tag each body line is a tag without
    /// braces of its own:
    ///
    /// ```liquid
    /// {% liquid
    ///   assign heading = section.settings.title
    ///   render 'card', title: heading
    /// %}
    /// ```
    ///
    /// Whole braced tags are consumed one at a time, so a tag-like string or an
    /// inline `{% # … %}` comment never starts a second match inside a tag, and
    /// `{% comment %}` / `{% raw %}` regions are skipped in both spellings.
    /// Bare tags are anchored at a body line's start, which keeps prose,
    /// filters and `#` comment lines out. Sorted by offset.
    fn tag_occurrences(&self, tag: LiquidTag) -> Vec<TagOccurrence<'a>> {
        static TAGS: OnceLock<Regex> = OnceLock::new();
        static FIRST_WORD: OnceLock<Regex> = OnceLock::new();
        let tags =
            TAGS.get_or_init(|| Regex::new(r"\{%-?\s*([0-9A-Za-z_]+|#)((?s:.*?))-?%\}").unwrap());
        let first_word = FIRST_WORD.get_or_init(|| Regex::new(r"^[ \t]*([0-9A-Za-z_]+)").unwrap());
        let (braced, bare) = tag.patterns();
        let groups = |captures: &regex::Captures<'a>| {
            captures
                .iter()
                .skip(1)
                .map(|group| group.map_or("", |m| m.as_str()))
                .collect::<Vec<_>>()
        };

        let mut found = Vec::new();
        let mut blocks = Vec::new();
        let mut suppressed: Option<&str> = None;
        for captures in tags.captures_iter(self.source) {
            let whole = captures.get(0).unwrap();
            let name = captures.get(1).unwrap();
            if let Some(region) = suppressed {
                if name.as_str().strip_prefix("end") == Some(region) {
                    suppressed = None;
                }
                continue;
            }
            match name.as_str() {
                "comment" | "raw" => suppressed = Some(name.as_str()),
                "liquid" => blocks.push((name.end(), captures.get(2).map_or("", |m| m.as_str()))),
                _ => {
                    if let Some(matched) = braced.captures(whole.as_str()) {
                        found.push(TagOccurrence {
                            text: matched.get(0).unwrap().as_str(),
                            groups: groups(&matched),
                            offset: whole.start(),
                        });
                    }
                }
            }
        }

        for (body_start, body) in blocks {
            let mut offset = body_start;
            let mut suppressed: Option<&str> = None;
            for line in body.split('\n') {
                let name = first_word
                    .captures(line)
                    .and_then(|captures| captures.get(1))
                    .map(|m| m.as_str());
                if let Some(region) = suppressed {
                    if name.and_then(|name| name.strip_prefix("end")) == Some(region) {
                        suppressed = None;
                    }
                } else if let Some(region @ ("comment" | "raw")) = name {
                    suppressed = Some(region);
                } else if let Some(matched) = bare.captures(line) {
                    let whole = matched.get(0).unwrap().as_str();
                    let text = whole.trim_start();
                    found.push(TagOccurrence {
                        text,
                        groups: groups(&matched),
                        offset: offset + (whole.len() - text.len()),
                    });
                }
                offset += line.len() + 1;
            }
        }

        found.sort_by_key(|occurrence| occurrence.offset);
        found
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
        for occurrence in self.tag_occurrences(LiquidTag::Snippet) {
            let (tag_type, name) = (occurrence.groups[0], occurrence.groups[1]);
            let line = line_number_for_offset(&self.line_starts, occurrence.offset);
            let col = occurrence.offset as i64 - line_start_for(&self.line_starts, line) as i64;
            self.push_import_node(result, file_id, name, occurrence.text, occurrence.offset);
            let node = self.identify(
                default_node(
                    self.file_path,
                    Language::Liquid,
                    NodeKind::Component,
                    name.to_string(),
                    format!("{}::{}:{}", self.file_path, tag_type, name),
                    line,
                    line,
                    col,
                    col + occurrence.text.len() as i64,
                ),
                occurrence.offset,
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
        for occurrence in self.tag_occurrences(LiquidTag::Section) {
            let name = occurrence.groups[1];
            let line = line_number_for_offset(&self.line_starts, occurrence.offset);
            let col = occurrence.offset as i64 - line_start_for(&self.line_starts, line) as i64;
            self.push_import_node(result, file_id, name, occurrence.text, occurrence.offset);
            let node = self.identify(
                default_node(
                    self.file_path,
                    Language::Liquid,
                    NodeKind::Component,
                    name.to_string(),
                    format!("{}::section:{}", self.file_path, name),
                    line,
                    line,
                    col,
                    col + occurrence.text.len() as i64,
                ),
                occurrence.offset,
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
            let mut node = self.identify(
                default_node(
                    self.file_path,
                    Language::Liquid,
                    NodeKind::Constant,
                    schema_name.clone(),
                    format!("{}::schema:{}", self.file_path, schema_name),
                    start_line,
                    end_line,
                    full.start() as i64 - line_start_for(&self.line_starts, start_line) as i64,
                    0,
                ),
                full.start(),
            );
            node.docstring = None;
            let node_id = node.id.clone();
            result.nodes.push(node);
            result.edges.push(contains_edge(file_id, &node_id));
        }
    }

    fn extract_assignments(&self, result: &mut ExtractionResult, file_id: &str) {
        for occurrence in self.tag_occurrences(LiquidTag::Assign) {
            let name = occurrence.groups[1];
            let line = line_number_for_offset(&self.line_starts, occurrence.offset);
            let col = occurrence.offset as i64 - line_start_for(&self.line_starts, line) as i64;
            let node = self.identify(
                default_node(
                    self.file_path,
                    Language::Liquid,
                    NodeKind::Variable,
                    name.to_string(),
                    format!("{}::{}", self.file_path, name),
                    line,
                    line,
                    col,
                    col + occurrence.text.len() as i64,
                ),
                occurrence.offset,
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
        offset: usize,
    ) {
        let line = line_number_for_offset(&self.line_starts, offset);
        let col = offset as i64 - line_start_for(&self.line_starts, line) as i64;
        let mut node = self.identify(
            default_node(
                self.file_path,
                Language::Liquid,
                NodeKind::Import,
                name.to_string(),
                format!("{}::import:{}", self.file_path, name),
                line,
                line,
                col,
                col + signature.len() as i64,
            ),
            offset,
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
