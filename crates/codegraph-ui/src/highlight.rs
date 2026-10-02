//! Server-side syntax classification for the viewer's code block — upstream
//! `src/ui-server/highlight/index.ts`.
//!
//! The classes come off the engine's own tree-sitter parse
//! ([`codegraph_extract::syntax_tokens`]). Three properties hold, because they
//! are what make this safe to depend on:
//!
//! - it never fails a request: anything that cannot be classified answers
//!   `engine: "plain"` with a reason, and the source still goes out;
//! - identifiers survive whatever token boundaries the grammar chose: every code
//!   token is split into identifier runs, so the viewer can wrap a call site as a
//!   link by claiming one token;
//! - a class is a name, not a colour, so one token stream serves both themes.

use std::collections::VecDeque;
use std::sync::Mutex;

use codegraph_core::types::Language;
use codegraph_extract::syntax_tokens::{
    SYNTAX_TOKEN_CLASSES, SyntaxClass, grammar_for, tokenize_source,
};
use serde::Serialize;

/// Lines above this are not classified (matches the source endpoint's cap).
pub const MAX_HIGHLIGHT_LINES: usize = 4000;
/// Characters (UTF-16 units, as upstream counts) above this are not classified.
pub const MAX_HIGHLIGHT_CHARS: usize = 600_000;
/// Classified slices kept in memory.
pub const SLICE_CACHE_LIMIT: usize = 96;
/// Total cached lines — the bound that actually matters for memory.
pub const SLICE_CACHE_LINES: usize = 20_000;

/// One token on the wire: its class id, then its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireToken(pub u8, pub String);

impl Serialize for WireToken {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let mut tuple = serializer.serialize_tuple(2)?;
        tuple.serialize_element(&self.0)?;
        tuple.serialize_element(&self.1)?;
        tuple.end()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HighlightResult {
    /// `tree-sitter` when a grammar produced the classes; `plain` otherwise.
    pub engine: &'static str,
    pub grammar: Option<String>,
    pub classes: [&'static str; 8],
    /// One entry per source line, in order.
    pub lines: Vec<Vec<WireToken>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The slice cache: a recency-ordered map bounded by entries and total lines.
#[derive(Default)]
pub struct HighlightCache {
    inner: Mutex<(VecDeque<(String, HighlightResult)>, usize)>,
}

impl HighlightCache {
    fn get(&self, key: &str) -> Option<HighlightResult> {
        let mut guard = self.inner.lock().ok()?;
        let (entries, _) = &mut *guard;
        let at = entries.iter().position(|(k, _)| k == key)?;
        let entry = entries.remove(at)?;
        let value = entry.1.clone();
        entries.push_back(entry);
        Some(value)
    }

    fn put(&self, key: String, value: HighlightResult) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        let (entries, lines) = &mut *guard;
        if let Some(at) = entries.iter().position(|(k, _)| *k == key)
            && let Some((_, old)) = entries.remove(at)
        {
            *lines -= old.lines.len();
        }
        *lines += value.lines.len();
        entries.push_back((key, value));
        while entries.len() > SLICE_CACHE_LIMIT || (*lines > SLICE_CACHE_LINES && entries.len() > 1)
        {
            let Some((_, oldest)) = entries.pop_front() else {
                break;
            };
            *lines -= oldest.lines.len();
        }
    }

    /// `(entries, lines)` — for tests and diagnostics.
    pub fn stats(&self) -> (usize, usize) {
        self.inner
            .lock()
            .map(|g| (g.0.len(), g.1))
            .unwrap_or((0, 0))
    }
}

/// Classify `lines` for the code block. `cache_key` must change whenever the
/// text does (the content hash plus the range); `None` classifies every time.
pub fn highlight_lines(
    cache: &HighlightCache,
    lines: &[String],
    language: Option<Language>,
    cache_key: Option<&str>,
) -> HighlightResult {
    let grammar = language.and_then(grammar_for);
    let key = cache_key.map(|k| format!("{} {k}", grammar.map(|g| g.as_str()).unwrap_or("-")));
    if let Some(key) = &key
        && let Some(hit) = cache.get(key)
    {
        return hit;
    }
    let result = highlight_uncached(lines, language, grammar);
    if let Some(key) = key {
        cache.put(key, result.clone());
    }
    result
}

fn highlight_uncached(
    lines: &[String],
    language: Option<Language>,
    grammar: Option<Language>,
) -> HighlightResult {
    let Some(grammar) = grammar else {
        return plain(
            lines,
            None,
            Some("No syntax grammar covers this file type."),
        );
    };
    let grammar_name = grammar.as_str().to_string();
    if lines.len() > MAX_HIGHLIGHT_LINES {
        return plain(
            lines,
            Some(grammar_name),
            Some(&format!(
                "Too many lines to highlight (over {MAX_HIGHLIGHT_LINES})."
            )),
        );
    }
    let text = lines.join("\n");
    if text.encode_utf16().count() > MAX_HIGHLIGHT_CHARS {
        return plain(
            lines,
            Some(grammar_name),
            Some("Too much text on too few lines to highlight (minified?)."),
        );
    }
    let tokenized = language.and_then(|l| tokenize_source(&text, l));
    let Some(tokenized) = tokenized.filter(|t| !t.spans.is_empty()) else {
        return plain(
            lines,
            Some(grammar_name.clone()),
            Some(&format!(
                "The {grammar_name} grammar is not available in this build."
            )),
        );
    };
    let names: Vec<&str> = tokenized.grammars.iter().map(|g| g.as_str()).collect();
    HighlightResult {
        engine: "tree-sitter",
        grammar: Some(if names.is_empty() {
            grammar_name
        } else {
            names.join("+")
        }),
        classes: SYNTAX_TOKEN_CLASSES,
        lines: to_wire_lines(lines, &text, &tokenized.spans),
        reason: None,
    }
}

fn plain(lines: &[String], grammar: Option<String>, reason: Option<&str>) -> HighlightResult {
    HighlightResult {
        engine: "plain",
        grammar,
        classes: SYNTAX_TOKEN_CLASSES,
        lines: lines.iter().map(|l| atomize_plain(l)).collect(),
        reason: reason.map(str::to_string),
    }
}

/// Cut the classifier's spans into one token list per line; the gaps become
/// `other`, multi-line spans are split at the newlines, and every line's tokens
/// concatenate back to exactly that line.
fn to_wire_lines(
    lines: &[String],
    text: &str,
    spans: &[codegraph_extract::syntax_tokens::SyntaxSpan],
) -> Vec<Vec<WireToken>> {
    let mut pieces: Vec<(usize, usize, SyntaxClass)> = Vec::new();
    let mut cursor = 0usize;
    for span in spans {
        if span.end <= cursor {
            continue;
        }
        let start = span.start.max(cursor);
        if start > cursor {
            pieces.push((cursor, start, SyntaxClass::Other));
        }
        pieces.push((start, span.end, span.class));
        cursor = span.end;
    }
    if cursor < text.len() {
        pieces.push((cursor, text.len(), SyntaxClass::Other));
    }

    let mut out = Vec::with_capacity(lines.len());
    let mut line_start = 0usize;
    let mut first = 0usize;
    for line in lines {
        let line_end = line_start + line.len();
        let mut row: Vec<WireToken> = Vec::new();
        while first < pieces.len() && pieces[first].1 <= line_start {
            first += 1;
        }
        for &(start, end, class) in &pieces[first..] {
            if start >= line_end {
                break;
            }
            let from = start.max(line_start);
            let to = end.min(line_end);
            if to > from
                && let Some(slice) = text.get(from..to)
            {
                push_piece(&mut row, slice, class);
            }
        }
        out.push(row);
        line_start = line_end + 1;
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$' || c >= '\u{C0}'
}

fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

fn unmergeable(class: SyntaxClass) -> bool {
    matches!(
        class,
        SyntaxClass::Ident | SyntaxClass::Type | SyntaxClass::Def
    )
}

fn push_piece(row: &mut Vec<WireToken>, text: &str, class: SyntaxClass) {
    if matches!(class, SyntaxClass::Comment | SyntaxClass::String) {
        push(row, class, text);
        return;
    }
    split_identifiers(row, text, class);
}

fn atomize_plain(line: &str) -> Vec<WireToken> {
    let mut out = Vec::new();
    split_identifiers(&mut out, line, SyntaxClass::Other);
    out
}

/// Emit `text` as alternating non-identifier and identifier runs.
fn split_identifiers(out: &mut Vec<WireToken>, text: &str, class: SyntaxClass) {
    if text.is_empty() {
        return;
    }
    let gap = if unmergeable(class) {
        SyntaxClass::Other
    } else {
        class
    };
    let ident_class = if class == SyntaxClass::Other {
        SyntaxClass::Ident
    } else {
        class
    };
    let mut at = 0usize;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if !is_ident_start(c) {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, d)) = iter.peek() {
            if is_ident_continue(d) {
                end = j + d.len_utf8();
                iter.next();
            } else {
                break;
            }
        }
        if i > at {
            push(out, gap, &text[at..i]);
        }
        push(out, ident_class, &text[i..end]);
        at = end;
    }
    if at < text.len() {
        push(out, gap, &text[at..]);
    }
}

/// Append, merging into the previous token when it carries the same class.
fn push(out: &mut Vec<WireToken>, class: SyntaxClass, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.0 == class.id()
        && !unmergeable(class)
    {
        last.1.push_str(text);
        return;
    }
    out.push(WireToken(class.id(), text.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    fn joined(row: &[WireToken]) -> String {
        row.iter().map(|t| t.1.as_str()).collect()
    }

    // Ported from upstream __tests__/ui-highlight.test.ts.
    #[test]
    fn emits_one_entry_per_source_line_always() {
        let cache = HighlightCache::default();
        let src = lines("fn a() {\n    /* two\n lines */\n}\n");
        let result = highlight_lines(&cache, &src, Some(Language::Rust), None);
        assert_eq!(result.engine, "tree-sitter");
        assert_eq!(result.lines.len(), src.len());
        for (row, line) in result.lines.iter().zip(&src) {
            assert_eq!(&joined(row), line, "tokens reproduce the line exactly");
        }
    }

    #[test]
    fn answers_plain_with_a_reason_for_a_language_no_grammar_covers() {
        let cache = HighlightCache::default();
        let result = highlight_lines(&cache, &lines("a: b.c"), Some(Language::Yaml), None);
        assert_eq!(result.engine, "plain");
        assert!(result.reason.is_some());
        // Still splits identifiers so the links land.
        let idents: Vec<&str> = result.lines[0]
            .iter()
            .filter(|t| t.0 == SyntaxClass::Ident.id())
            .map(|t| t.1.as_str())
            .collect();
        assert_eq!(idents, vec!["a", "b", "c"]);
    }

    #[test]
    fn refuses_to_classify_a_minified_line() {
        let cache = HighlightCache::default();
        let long = vec!["x".repeat(MAX_HIGHLIGHT_CHARS + 1)];
        let result = highlight_lines(&cache, &long, Some(Language::JavaScript), None);
        assert_eq!(result.engine, "plain");
    }

    #[test]
    fn keeps_every_identifier_separately_claimable() {
        let cache = HighlightCache::default();
        let result = highlight_lines(
            &cache,
            &lines("this.mutex.withLock(fn)"),
            Some(Language::TypeScript),
            None,
        );
        let texts: Vec<&str> = result.lines[0].iter().map(|t| t.1.as_str()).collect();
        assert!(texts.contains(&"withLock"));
        assert!(texts.contains(&"mutex"));
    }

    #[test]
    fn leaves_a_word_inside_a_comment_or_string_alone() {
        let cache = HighlightCache::default();
        let result = highlight_lines(
            &cache,
            &lines("const s = \"call me\"; // and me"),
            Some(Language::TypeScript),
            None,
        );
        assert!(result.lines[0].iter().any(|t| t.1 == "\"call me\""));
        assert!(result.lines[0].iter().any(|t| t.1 == "// and me"));
    }

    #[test]
    fn caches_by_key_and_bounds_by_total_lines() {
        let cache = HighlightCache::default();
        let src = lines("fn a() {}");
        let first = highlight_lines(&cache, &src, Some(Language::Rust), Some("h:1:1"));
        let second = highlight_lines(&cache, &src, Some(Language::Rust), Some("h:1:1"));
        assert_eq!(first, second);
        assert_eq!(cache.stats(), (1, 1));
        let big: Vec<String> = (0..3000).map(|i| format!("let v{i} = {i};")).collect();
        for i in 0..10 {
            highlight_lines(
                &cache,
                &big,
                Some(Language::Rust),
                Some(&format!("big:{i}")),
            );
        }
        assert!(cache.stats().1 <= SLICE_CACHE_LINES + big.len());
    }

    #[test]
    fn keys_on_the_content_so_an_edited_file_reclassifies() {
        let cache = HighlightCache::default();
        let a = highlight_lines(
            &cache,
            &lines("fn a() {}"),
            Some(Language::Rust),
            Some("hashA:1:1"),
        );
        let b = highlight_lines(
            &cache,
            &lines("// a"),
            Some(Language::Rust),
            Some("hashB:1:1"),
        );
        assert_ne!(a, b);
    }
}
