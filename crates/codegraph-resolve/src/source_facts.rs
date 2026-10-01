//! Per-file source facts shared by the resolver's text-scanning gates.
//!
//! Several resolution gates inspect a file's source text for every reference
//! they see: the call site's own line, a comment-stripped view of the whole
//! file, or a JS/TS module-shape decision. Deriving those views afresh per
//! reference made resolution `O(references × file size)` on large files.
//!
//! A [`SourceFacts`] is built once per file and per resolution pass (production
//! contexts memoise it next to their file-content cache), and every derived
//! view is computed lazily, at most once. Each value is a pure function of the
//! source text — the memo slots additionally depend only on the immutable node
//! snapshot of the pass — so a cached answer is identical to recomputing it.

use crate::awaited::AwaitedIndex;
use crate::object_literal::LiteralLookup;
use crate::strip_comments::{CommentLang, blank_string_contents, strip_comments_for_regex};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// Newline positions of a text.
///
/// Answers both [`str::lines`]-compatible queries and the raw
/// `split('\n')` view that upstream's column arithmetic uses, without
/// materialising a `Vec<&str>` for every caller.
#[derive(Debug)]
pub struct LineIndex {
    newlines: Vec<usize>,
    len: usize,
}

impl LineIndex {
    /// Index every `\n` in `text`.
    pub fn new(text: &str) -> Self {
        Self {
            newlines: text
                .bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset))
                .collect(),
            len: text.len(),
        }
    }

    /// Byte offset where 0-based line `index` starts when every `\n` counts
    /// as a line break, or `None` past the last newline.
    pub fn raw_line_start(&self, index: usize) -> Option<usize> {
        if index == 0 {
            Some(0)
        } else {
            self.newlines.get(index - 1).map(|newline| newline + 1)
        }
    }

    /// Segment `index` of `text.split('\n')` (a `\r` before the newline is
    /// kept, and a final empty segment after a trailing newline exists).
    pub fn raw_line<'t>(&self, text: &'t str, index: usize) -> Option<&'t str> {
        let start = self.raw_line_start(index)?;
        let end = self.newlines.get(index).copied().unwrap_or(self.len);
        Some(&text[start..end])
    }

    /// Number of lines [`str::lines`] yields for the indexed text.
    pub fn line_count(&self) -> usize {
        let tail_start = self.newlines.last().map_or(0, |newline| newline + 1);
        self.newlines.len() + usize::from(tail_start < self.len)
    }

    /// Line `index` exactly as [`str::lines`] yields it: a `\n` terminator is
    /// removed together with one `\r` before it; an unterminated final line is
    /// returned verbatim.
    ///
    /// # Panics
    /// When `index >= self.line_count()` or `text` is not the indexed text.
    pub fn line<'t>(&self, text: &'t str, index: usize) -> &'t str {
        let start = self
            .raw_line_start(index)
            .expect("line index within the indexed text");
        match self.newlines.get(index) {
            Some(&newline) => {
                let line = &text[start..newline];
                line.strip_suffix('\r').unwrap_or(line)
            }
            None => &text[start..self.len],
        }
    }

    /// `lines[start..end].join(separator)` over [`str::lines`].
    pub fn join(&self, text: &str, start: usize, end: usize, separator: &str) -> String {
        let mut joined = String::new();
        for index in start..end {
            if index > start {
                joined.push_str(separator);
            }
            joined.push_str(self.line(text, index));
        }
        joined
    }
}

/// Offsets where a JS/TS local binding can start (`localBindingSites`,
/// upstream v1.6.1): every `const`/`let`/`var` keyword, every
/// `function`/`class` keyword, and every `=>`.
#[derive(Debug, Default)]
pub struct LocalBindingSites {
    pub var_decls: Vec<usize>,
    pub fn_decls: Vec<usize>,
    pub arrows: Vec<usize>,
}

/// The declared type of a TS/JS class field, read off the class's own lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsFieldDeclaration {
    /// `field: typeof Value` — the type OF a value (an object-literal namespace).
    pub value_type: bool,
    /// The declared or constructed type as spelled (`ns.Mailer`, `Mailer`).
    pub type_name: String,
}

/// Lazily derived, memoised facts about one file's source text.
#[derive(Debug)]
pub struct SourceFacts {
    source: Arc<str>,
    lines: OnceLock<LineIndex>,
    ts_comment_free: OnceLock<(String, LineIndex)>,
    python_comment_free: OnceLock<(String, LineIndex)>,
    ts_blanked: OnceLock<String>,
    js_sealed_module: OnceLock<bool>,
    js_binding_sites: OnceLock<LocalBindingSites>,
    js_local_bindings: Mutex<HashMap<String, bool>>,
    node_decisions: Mutex<HashMap<(&'static str, String), bool>>,
    ts_field_declarations: Mutex<HashMap<(String, String), Option<TsFieldDeclaration>>>,
    js_get_state_file: OnceLock<bool>,
    js_selector_names: OnceLock<HashSet<String>>,
    literal_properties: Mutex<HashMap<(String, String), LiteralLookup>>,
    literal_binding_targets: Mutex<HashMap<(String, String), Option<String>>>,
    awaited_raw_names: OnceLock<HashSet<String>>,
    awaited_index: OnceLock<Arc<AwaitedIndex>>,
}

impl SourceFacts {
    /// Facts over `source`; nothing is derived until first asked for.
    pub fn new(source: Arc<str>) -> Self {
        Self {
            source,
            lines: OnceLock::new(),
            ts_comment_free: OnceLock::new(),
            python_comment_free: OnceLock::new(),
            ts_blanked: OnceLock::new(),
            js_sealed_module: OnceLock::new(),
            js_binding_sites: OnceLock::new(),
            js_local_bindings: Mutex::new(HashMap::new()),
            node_decisions: Mutex::new(HashMap::new()),
            ts_field_declarations: Mutex::new(HashMap::new()),
            js_get_state_file: OnceLock::new(),
            js_selector_names: OnceLock::new(),
            literal_properties: Mutex::new(HashMap::new()),
            literal_binding_targets: Mutex::new(HashMap::new()),
            awaited_raw_names: OnceLock::new(),
            awaited_index: OnceLock::new(),
        }
    }

    /// The raw source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Line index of the raw source.
    pub fn lines(&self) -> &LineIndex {
        self.lines.get_or_init(|| LineIndex::new(&self.source))
    }

    /// Number of lines [`str::lines`] yields for the raw source.
    pub fn line_count(&self) -> usize {
        self.lines().line_count()
    }

    /// Raw source line `index`, as [`str::lines`] yields it.
    pub fn line(&self, index: usize) -> &str {
        self.lines().line(&self.source, index)
    }

    /// Segment `index` of `source.split('\n')`.
    pub fn raw_line(&self, index: usize) -> Option<&str> {
        self.lines().raw_line(&self.source, index)
    }

    /// `source.lines()[start..end].join(separator)`.
    pub fn join_lines(&self, start: usize, end: usize, separator: &str) -> String {
        self.lines().join(&self.source, start, end, separator)
    }

    /// The whole file through [`strip_comments_for_regex`] with the
    /// TypeScript rules (shared by every JS-family language), plus its line
    /// index. Comment bytes become spaces; string contents are kept.
    pub fn ts_comment_free(&self) -> (&str, &LineIndex) {
        let (code, lines) = self.ts_comment_free.get_or_init(|| {
            let code = strip_comments_for_regex(&self.source, CommentLang::TypeScript);
            let lines = LineIndex::new(&code);
            (code, lines)
        });
        (code, lines)
    }

    /// The whole file through [`strip_comments_for_regex`] with the Python
    /// rules, plus its line index.
    pub fn python_comment_free(&self) -> (&str, &LineIndex) {
        let (code, lines) = self.python_comment_free.get_or_init(|| {
            let code = strip_comments_for_regex(&self.source, CommentLang::Python);
            let lines = LineIndex::new(&code);
            (code, lines)
        });
        (code, lines)
    }

    /// [`Self::ts_comment_free`] with every string and template literal's
    /// contents blanked too (`blankStringContents(stripCommentsForRegex(…))`),
    /// so line-anchored patterns see only code.
    pub fn ts_blanked(&self) -> &str {
        self.ts_blanked
            .get_or_init(|| blank_string_contents(self.ts_comment_free().0))
    }

    /// Where a JS/TS local binding can start in the raw source.
    pub fn js_binding_sites(&self) -> &LocalBindingSites {
        self.js_binding_sites.get_or_init(|| {
            let source: &str = &self.source;
            LocalBindingSites {
                var_decls: keyword_sites(source, &["const", "let", "var"]),
                fn_decls: keyword_sites(source, &["function", "class"]),
                arrows: source.match_indices("=>").map(|(at, _)| at).collect(),
            }
        })
    }

    /// Memoised sealed-module decision for this file; `compute` runs once.
    pub fn js_sealed_module(&self, compute: impl FnOnce(&Self) -> bool) -> bool {
        *self.js_sealed_module.get_or_init(|| compute(self))
    }

    /// Memoised "is `name` bound locally in this file" decision; `compute`
    /// runs at most once per name.
    pub fn js_local_binding(&self, name: &str, compute: impl FnOnce(&Self) -> bool) -> bool {
        if let Some(&known) = self
            .js_local_bindings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
        {
            return known;
        }
        let bound = compute(self);
        self.js_local_bindings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_string(), bound);
        bound
    }

    /// Whether the raw source can destructure a store's `getState()`;
    /// `scan` runs once.
    pub(crate) fn js_get_state_file(&self, scan: impl FnOnce(&str) -> bool) -> bool {
        *self.js_get_state_file.get_or_init(|| scan(&self.source))
    }

    /// Names the raw source binds to a selector call; `scan` runs once.
    pub(crate) fn js_selector_names(
        &self,
        scan: impl FnOnce(&str) -> HashSet<String>,
    ) -> &HashSet<String> {
        self.js_selector_names.get_or_init(|| scan(&self.source))
    }

    /// Memoised own property of the object literal `container_id` (defined in
    /// this file) for `member`; `compute` runs at most once per key.
    pub(crate) fn literal_property(
        &self,
        container_id: &str,
        member: &str,
        compute: impl FnOnce(&Self) -> LiteralLookup,
    ) -> LiteralLookup {
        let key = (container_id.to_string(), member.to_string());
        if let Some(known) = self
            .literal_properties
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let lookup = compute(self);
        self.literal_properties
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, lookup.clone());
        lookup
    }

    /// Memoised target node id of an object-literal member's binding;
    /// `compute` runs at most once per `(container, key)`.
    pub(crate) fn literal_binding_target(
        &self,
        container_id: &str,
        key: &str,
        compute: impl FnOnce(&Self) -> Option<String>,
    ) -> Option<String> {
        let key = (container_id.to_string(), key.to_string());
        if let Some(known) = self
            .literal_binding_targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let target = compute(self);
        self.literal_binding_targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, target.clone());
        target
    }

    /// Names the raw source binds as `const x = await f(`; `scan` runs once.
    pub(crate) fn awaited_raw_names(
        &self,
        scan: impl FnOnce(&str) -> HashSet<String>,
    ) -> &HashSet<String> {
        self.awaited_raw_names.get_or_init(|| scan(&self.source))
    }

    /// The awaited-binding index over [`Self::ts_blanked`]; `build` runs once.
    pub(crate) fn awaited_index(
        &self,
        build: impl FnOnce(&str) -> Arc<AwaitedIndex>,
    ) -> Arc<AwaitedIndex> {
        Arc::clone(self.awaited_index.get_or_init(|| build(self.ts_blanked())))
    }

    /// Memoised per-node decision (`kind` names the gate, `node_id` the
    /// candidate defined in this file); `compute` runs at most once per key.
    pub fn node_decision(
        &self,
        kind: &'static str,
        node_id: &str,
        compute: impl FnOnce(&Self) -> bool,
    ) -> bool {
        let key = (kind, node_id.to_string());
        if let Some(&known) = self
            .node_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known;
        }
        let decision = compute(self);
        self.node_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, decision);
        decision
    }

    /// Memoised declaration of `field` on the class `owner_id` defined in this
    /// file; `compute` runs at most once per `(owner, field)`.
    pub fn ts_field_declaration(
        &self,
        owner_id: &str,
        field: &str,
        compute: impl FnOnce(&Self) -> Option<TsFieldDeclaration>,
    ) -> Option<TsFieldDeclaration> {
        let key = (owner_id.to_string(), field.to_string());
        if let Some(known) = self
            .ts_field_declarations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
        {
            return known.clone();
        }
        let declaration = compute(self);
        self.ts_field_declarations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, declaration.clone());
        declaration
    }
}

/// Offsets of `\b(?:kw1|kw2|…)\s` matches: a keyword preceded by a non-word
/// character (or the start) and followed by whitespace. `\b`/`\w` are ASCII,
/// as in upstream's JavaScript regexes.
fn keyword_sites(source: &str, keywords: &[&str]) -> Vec<usize> {
    let bytes = source.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut sites = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if (at == 0 || !is_word(bytes[at - 1]))
            && let Some(keyword) = keywords.iter().find(|keyword| {
                bytes[at..].starts_with(keyword.as_bytes())
                    && source[at + keyword.len()..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace)
            })
        {
            sites.push(at);
            at += keyword.len();
            continue;
        }
        at += 1;
    }
    sites
}

#[cfg(test)]
mod tests {
    use super::*;

    fn std_lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn line_index_matches_str_lines_for_every_terminator_shape() {
        for text in [
            "",
            "a",
            "a\n",
            "a\nb",
            "a\nb\n",
            "\n",
            "\n\n",
            "a\r\nb\r\n",
            "a\r\nb\r",
            "a\rb\n",
            "\r\n",
            "x\n\ny\r\n\r\nz",
            "é\nü\r\n中",
        ] {
            let index = LineIndex::new(text);
            let expected = std_lines(text);
            assert_eq!(index.line_count(), expected.len(), "count for {text:?}");
            for (i, line) in expected.iter().enumerate() {
                assert_eq!(index.line(text, i), *line, "line {i} of {text:?}");
            }
            for start in 0..=expected.len() {
                for end in start..=expected.len() {
                    assert_eq!(
                        index.join(text, start, end, "\n"),
                        expected[start..end].join("\n"),
                        "join {start}..{end} of {text:?}"
                    );
                }
            }
            let split: Vec<&str> = text.split('\n').collect();
            for (i, segment) in split.iter().enumerate() {
                assert_eq!(
                    index.raw_line(text, i),
                    Some(*segment),
                    "raw {i} of {text:?}"
                );
            }
            assert_eq!(index.raw_line(text, split.len()), None);
        }
    }

    #[test]
    fn raw_line_start_counts_every_newline() {
        let text = "ab\r\ncd\n\nef";
        let index = LineIndex::new(text);
        assert_eq!(index.raw_line_start(0), Some(0));
        assert_eq!(index.raw_line_start(1), Some(4));
        assert_eq!(index.raw_line_start(2), Some(7));
        assert_eq!(index.raw_line_start(3), Some(8));
        assert_eq!(index.raw_line_start(4), None);
    }

    #[test]
    fn keyword_sites_require_ascii_boundary_and_trailing_space() {
        let source = "const a = 1; xconst b; let\tc; var(d); function f() {} class C {}";
        let facts = SourceFacts::new(Arc::from(source));
        let sites = facts.js_binding_sites();
        let at = |needle: &str| source.find(needle).unwrap();
        assert_eq!(sites.var_decls, vec![at("const a"), at("let\t")]);
        assert_eq!(sites.fn_decls, vec![at("function"), at("class")]);
        assert_eq!(sites.arrows, Vec::<usize>::new());
    }

    #[test]
    fn local_binding_decision_is_computed_once_per_name() {
        let facts = SourceFacts::new(Arc::from("const a = 1; // b\n"));
        let mut runs = 0;
        assert!(facts.js_local_binding("a", |facts| {
            runs += 1;
            facts.source().contains("const a")
        }));
        assert!(facts.js_local_binding("a", |_| unreachable!("memoised")));
        assert!(!facts.js_local_binding("b", |facts| {
            runs += 1;
            facts.source().contains("const b")
        }));
        assert_eq!(runs, 2);
    }

    #[test]
    fn sealed_and_node_decisions_are_computed_once() {
        let facts = SourceFacts::new(Arc::from("import x from 'y';\n"));
        assert!(facts.js_sealed_module(|_| true));
        assert!(facts.js_sealed_module(|_| unreachable!("memoised")));
        assert!(facts.node_decision("c_static", "function:1", |_| true));
        assert!(facts.node_decision("c_static", "function:1", |_| unreachable!("memoised")));
        assert!(!facts.node_decision("rust_trait", "function:1", |_| false));
    }

    #[test]
    fn ts_field_declarations_are_computed_once_per_owner_and_field() {
        let facts = SourceFacts::new(Arc::from("class A { #m = new M(); }\n"));
        let declared = TsFieldDeclaration {
            value_type: false,
            type_name: "M".to_string(),
        };
        let mut runs = 0;
        let mut compute = |_: &SourceFacts| {
            runs += 1;
            Some(declared.clone())
        };
        assert_eq!(
            facts.ts_field_declaration("class:1", "#m", &mut compute),
            Some(declared.clone())
        );
        assert_eq!(
            facts.ts_field_declaration("class:1", "#m", |_| unreachable!("memoised")),
            Some(declared)
        );
        assert_eq!(facts.ts_field_declaration("class:1", "m", |_| None), None);
        assert_eq!(
            facts.ts_field_declaration("class:1", "m", |_| unreachable!("memoised")),
            None
        );
        assert_eq!(runs, 1);
    }

    #[test]
    fn ts_blanked_hides_template_imports() {
        let facts = SourceFacts::new(Arc::from(
            "const t = `\nimport fake from \"fake\";\n`;\n// import x\n",
        ));
        assert!(!facts.ts_blanked().contains("import"));
        assert_eq!(facts.ts_blanked().matches('\n').count(), 4);
    }
}
