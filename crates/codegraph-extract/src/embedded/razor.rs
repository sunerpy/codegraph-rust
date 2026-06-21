use codegraph_core::types::{EdgeKind, ExtractionResult, Language};
use regex::Regex;

use crate::embedded::shared::{component_node, empty_result, unresolved_ref};

const BLAZOR_BUILTINS: &[&str] = &[
    "Router",
    "Found",
    "NotFound",
    "RouteView",
    "AuthorizeRouteView",
    "LayoutView",
    "CascadingValue",
    "CascadingAuthenticationState",
    "AuthorizeView",
    "Authorized",
    "NotAuthorized",
    "Authorizing",
    "EditForm",
    "DataAnnotationsValidator",
    "ValidationSummary",
    "ValidationMessage",
    "InputText",
    "InputNumber",
    "InputCheckbox",
    "InputSelect",
    "InputDate",
    "InputTextArea",
    "InputRadio",
    "InputRadioGroup",
    "InputFile",
    "PageTitle",
    "HeadContent",
    "HeadOutlet",
    "Virtualize",
    "DynamicComponent",
    "ErrorBoundary",
    "SectionContent",
    "SectionOutlet",
    "FocusOnNavigate",
    "NavLink",
    "Microsoft",
];

pub struct RazorExtractor<'a> {
    file_path: &'a str,
    source: &'a str,
}

struct CodeBlock {
    content: String,
    line_offset: i64,
}

impl<'a> RazorExtractor<'a> {
    pub fn new(file_path: &'a str, source: &'a str) -> Self {
        Self { file_path, source }
    }

    pub fn extract(self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let mut result = empty_result(0);
        let suffix = if self.file_path.to_ascii_lowercase().ends_with(".cshtml") {
            ".cshtml"
        } else {
            ".razor"
        };
        let component = component_node(self.file_path, self.source, Language::Razor, suffix);
        let component_id = component.id.clone();
        result.nodes.push(component);

        self.extract_directives(&mut result, &component_id);
        if self.file_path.to_ascii_lowercase().ends_with(".razor") {
            self.extract_component_tags(&mut result, &component_id);
        }
        self.process_code_blocks(&mut result, &component_id);
        result.duration_ms += start.elapsed().as_millis() as i64;
        result
    }

    fn extract_directives(&self, result: &mut ExtractionResult, component_id: &str) {
        let model_re =
            Regex::new(r"^\s*@(?:model|inherits)\s+([A-Za-z_][\w.]*(?:\s*<[^>]+>)?)").unwrap();
        let inject_re =
            Regex::new(r"^\s*@inject\s+([A-Za-z_][\w.]*(?:\s*<[^>]+>)?)\s+[A-Za-z_]").unwrap();
        let typeof_re = Regex::new(r"@typeof\(\s*([A-Za-z_][\w.]*)\s*\)").unwrap();

        for (idx, line) in self.source.split('\n').enumerate() {
            if let Some(cap) = model_re.captures(line) {
                self.push_type_refs(
                    result,
                    component_id,
                    cap.get(1).unwrap().as_str(),
                    idx + 1,
                    0,
                );
            }
            if let Some(cap) = inject_re.captures(line) {
                self.push_type_refs(
                    result,
                    component_id,
                    cap.get(1).unwrap().as_str(),
                    idx + 1,
                    0,
                );
            }
            for cap in typeof_re.captures_iter(line) {
                let seg = last_segment(cap.get(1).unwrap().as_str());
                if starts_upper(&seg) {
                    result.unresolved_references.push(unresolved_ref(
                        component_id,
                        seg,
                        EdgeKind::References,
                        idx as i64 + 1,
                        cap.get(0).unwrap().start() as i64,
                        self.file_path,
                        Language::Razor,
                    ));
                }
            }
        }
    }

    fn extract_component_tags(&self, result: &mut ExtractionResult, component_id: &str) {
        let tag_re = Regex::new(r#"<([A-Z][A-Za-z0-9_]*)\b([^>]*)>"#).unwrap();
        let generic_re = Regex::new(r#"\bT[A-Za-z]*\s*=\s*"([A-Za-z_][\w.]*)""#).unwrap();
        for (idx, line) in self.source.split('\n').enumerate() {
            for cap in tag_re.captures_iter(line) {
                let name = cap.get(1).unwrap().as_str();
                if !BLAZOR_BUILTINS.contains(&name) {
                    result.unresolved_references.push(unresolved_ref(
                        component_id,
                        name.to_string(),
                        EdgeKind::References,
                        idx as i64 + 1,
                        cap.get(0).unwrap().start() as i64 + 1,
                        self.file_path,
                        Language::Razor,
                    ));
                }
                for generic in generic_re.captures_iter(cap.get(2).map_or("", |m| m.as_str())) {
                    let seg = last_segment(generic.get(1).unwrap().as_str());
                    if starts_upper(&seg) {
                        result.unresolved_references.push(unresolved_ref(
                            component_id,
                            seg,
                            EdgeKind::References,
                            idx as i64 + 1,
                            0,
                            self.file_path,
                            Language::Razor,
                        ));
                    }
                }
            }
        }
    }

    fn process_code_blocks(&self, result: &mut ExtractionResult, component_id: &str) {
        let new_re = Regex::new(r"\bnew\s+([A-Z][A-Za-z0-9_]*)\s*[<(]").unwrap();
        let static_call_re = Regex::new(r"\b([A-Z][A-Za-z0-9_]*)\.[A-Za-z_][\w]*\s*\(").unwrap();

        for block in self.extract_code_blocks() {
            if block.content.trim().is_empty() {
                continue;
            }
            let _delegated = crate::engine::extract_source(
                self.file_path,
                &format!("class __RazorCode__ {{\n{}\n}}", block.content),
                Some(Language::CSharp),
            );
            for (regex, kind) in [
                (&new_re, EdgeKind::Instantiates),
                (&static_call_re, EdgeKind::References),
            ] {
                for cap in regex.captures_iter(&block.content) {
                    let line_in_content = block.content[..cap.get(0).unwrap().start()]
                        .matches('\n')
                        .count() as i64
                        + 1;
                    let wrapper_ref_line = line_in_content + 1;
                    result.unresolved_references.push(unresolved_ref(
                        component_id,
                        cap.get(1).unwrap().as_str().to_string(),
                        kind,
                        wrapper_ref_line + block.line_offset - 1,
                        0,
                        self.file_path,
                        Language::Razor,
                    ));
                }
            }
        }
    }

    fn extract_code_blocks(&self) -> Vec<CodeBlock> {
        let mut blocks = Vec::new();
        let re = Regex::new(r"@(?:code|functions)\b\s*\{|@\{").unwrap();
        for mat in re.find_iter(self.source) {
            let Some(open_rel) = self.source[mat.start()..mat.end()].find('{') else {
                continue;
            };
            let open_idx = mat.start() + open_rel;
            let Some(close) = match_brace(self.source, open_idx) else {
                continue;
            };
            let content = self.source[open_idx + 1..close].to_string();
            let line_offset = self.source[..open_idx + 1].matches('\n').count() as i64;
            blocks.push(CodeBlock {
                content,
                line_offset,
            });
        }
        blocks
    }

    fn push_type_refs(
        &self,
        result: &mut ExtractionResult,
        component_id: &str,
        expr: &str,
        line: usize,
        col: i64,
    ) {
        for token in
            expr.split(|ch: char| matches!(ch, '<' | '>' | ',' | ' ') || ch.is_whitespace())
        {
            let seg = last_segment(token.trim());
            if starts_upper(&seg) {
                result.unresolved_references.push(unresolved_ref(
                    component_id,
                    seg,
                    EdgeKind::References,
                    line as i64,
                    col,
                    self.file_path,
                    Language::Razor,
                ));
            }
        }
    }
}

fn match_brace(source: &str, open_idx: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0_i32;
    let mut i = open_idx;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn last_segment(input: &str) -> String {
    input.rsplit('.').next().unwrap_or(input).to_string()
}

fn starts_upper(input: &str) -> bool {
    input
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}
