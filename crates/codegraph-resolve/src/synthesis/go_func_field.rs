//! The Go func-field pass (upstream `goFuncFieldEdges` in
//! `go-func-field-synthesizer.ts` at `e8ddc4e2`, #2745, W7-15d): a call
//! through a struct field holding a function links to each function or
//! method value the field is given.
//!
//! A Go struct can hold a function in a field and call it through the field:
//! `d.refreshf(ctx)` in `(*Discovery).refresh`, which no method answers, so a
//! flow stops at the field. Prometheus' service discovery is the canonical
//! shape: `NewDiscovery` copies `refreshf` from `Options.RefreshF`, which each
//! provider fills with its own method value (`refresh.Options{…, RefreshF:
//! d.refresh}`). The values are keyed by struct and field: a composite literal
//! (keyed, or positional with every field), an assignment `x.F = v`, and one
//! level of copy from another func field. A value is the function the
//! resolver linked at that spot, else for `x.m` the method of x's type (an
//! interface's when x is an interface value). A call or an assignment counts
//! only where its receiver's type is known: the receiver, parameter or local
//! in scope there (`go_scope`), with the type its declaration writes or its
//! value gives it, at most one field hop away. Generated files are not read.
//!
//! Upstream's patterns are JavaScript: `\s` is JavaScript's whitespace, `\w`
//! is ASCII, and the lookarounds the `regex` crate lacks are checked around a
//! match, a start they reject retried one character on, as a JavaScript
//! global search retries it.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use codegraph_core::collation::locale_compare_numeric;
use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_store::queries::ReferenceSite;
use rayon::prelude::*;
use regex::{Captures, Regex};
use serde_json::{Map, Value};

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::dispatch::synthesized;
use super::stage::{View, enclosing_fn};
use crate::context::load_go_modules_pub;
use crate::frameworks::js_scan::{JS_SPACE, is_js_space, js_trim};
use crate::go_packages::go_module_package_dir;
use crate::go_scope::{GoScopeIndex, blank_go_code};
use crate::import_resolver::extract_import_mappings;
use crate::types::{GoModule, ImportMapping};

/// `pattern` with JavaScript's `\s` and `\S`.
fn js(pattern: &str) -> String {
    let non_space = format!("[^{}", &JS_SPACE[1..]);
    pattern.replace(r"\S", &non_space).replace(r"\s", JS_SPACE)
}

/// A Go identifier (upstream `IDENT`).
const IDENT: &str = r"[A-Za-z_][A-Za-z0-9_]*";

fn compile(pattern: &str) -> Regex {
    Regex::new(&js(pattern)).expect("a go-func-field pattern compiles")
}

/// `x.F(` / `x.y.F(` (upstream `SELECTOR_CALL_RE`, its lookbehind checked by
/// [`selector_matches`]).
static SELECTOR_CALL: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"({IDENT}(?:\s*\.\s*{IDENT})?)\s*\.\s*({IDENT})\s*\("
    ))
});
/// `x.F = v` / `x.y.F = v` (upstream `SELECTOR_ASSIGN_RE`; never `==`).
static SELECTOR_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"({IDENT}(?:\s*\.\s*{IDENT})?)\s*\.\s*({IDENT})\s*="
    ))
});
/// `.F(` or `.F =` for some name (upstream `SELECTOR_USE_RE`; see
/// [`FieldUses`]).
static SELECTOR_USE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*([(=])"));
/// A type written as a name: `T`, `*T`, `pkg.T`, `T[int]` (upstream
/// `NAMED_TYPE_RE`).
static NAMED_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"^[*&\s]*({IDENT})(?:\s*\.\s*({IDENT}))?\s*(?:\[(?s:.)*\])?\s*$"
    ))
});
/// A plain value: `h`, `d.refresh`, `pkg.Handle` (upstream `PLAIN_VALUE_RE`).
static PLAIN_VALUE: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"^{IDENT}(?:\s*\.\s*{IDENT})?$")));
/// The head of a `type` spec, from its name to its type (upstream
/// `TYPE_SPEC_RE`).
static TYPE_SPEC: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"^{IDENT}\s*(?:\[[^\]]*\])?\s*=?\s*([^\n;]+)")));
/// The names before the one a declaration is read at (upstream
/// `NAMES_BEFORE_RE`).
static NAMES_BEFORE: LazyLock<Regex> = LazyLock::new(|| compile(&format!(r"(?:{IDENT}\s*,\s*)*$")));
/// A declaration's names from the one it is read at, to its `:=` or `=`
/// (upstream `NAMES_FROM_RE`, its `=(?!=)` checked by [`names_from`]).
static NAMES_FROM: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"^{IDENT}((?:\s*,\s*{IDENT})*)\s*(:=|=)")));
/// The keyword opening a struct's fields (upstream's `/\bstruct\s*\{/`).
static STRUCT_KEYWORD: LazyLock<Regex> = LazyLock::new(|| compile(r"(?-u:\b)struct\s*\{"));
/// A file's package clause (upstream's `/^\s*package\s+([A-Za-z_]\w*)/m`).
static PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"(?m)^\s*package\s+([A-Za-z_][A-Za-z0-9_]*)"));
/// A `range` clause on the right of a declaration.
static RANGE: LazyLock<Regex> = LazyLock::new(|| compile(r"^\s*range(?-u:\b)"));
/// A composite literal's type.
static LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"^&?\s*({IDENT}(?:\s*\.\s*{IDENT})?)\s*(?:\[[^\]]*\])?\s*\{{"
    ))
});
/// `new(T)`.
static CREATED: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"^new\s*\(\s*({IDENT}(?:\s*\.\s*{IDENT})?)\s*(?:\[[^\]]*\])?\s*\)$"
    ))
});
/// A call's callee: `f(`, `pkg.F(`, `x.M(`.
static CALL: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"^({IDENT}(?:\s*\.\s*{IDENT})*)\s*\(")));
/// A conversion's type: `HandlerFunc(` (upstream's `conv`).
static CONVERSION: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"^({IDENT}(?:\s*\.\s*{IDENT})?)\s*\(")));
/// A keyed literal element's key (upstream's `/^([A-Za-z_]\w*)\s*:(?!=)\s*/`,
/// its lookahead checked by [`literal_key`]).
static KEY: LazyLock<Regex> = LazyLock::new(|| compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s*:"));
/// A result list's `name type` item.
static NAMED_ITEM: LazyLock<Regex> =
    LazyLock::new(|| compile(r"^([A-Za-z_][A-Za-z0-9_]*)\s+(\S(?s:.)*)$"));
/// A struct field's `name[, name] type`.
static FIELD_DECL: LazyLock<Regex> = LazyLock::new(|| {
    compile(r"^([A-Za-z_][A-Za-z0-9_]*(?:\s*,\s*[A-Za-z_][A-Za-z0-9_]*)*)\s+(\S(?s:.)*)$")
});
/// A function type (upstream's `/^func\s*\(/`).
static FUNC_TYPE: LazyLock<Regex> = LazyLock::new(|| compile(r"^func\s*\("));
/// A run of whitespace (upstream's `/\s+/g`).
static SPACES: LazyLock<Regex> = LazyLock::new(|| compile(r"\s+"));

/// Words a result list's `x T` item can start with that are a type.
fn is_type_keyword(word: &str) -> bool {
    matches!(word, "chan" | "func" | "map" | "struct" | "interface")
}

/// Go's predeclared types: never a project type.
fn is_predeclared_type(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "bool"
            | "byte"
            | "comparable"
            | "complex64"
            | "complex128"
            | "error"
            | "float32"
            | "float64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "rune"
            | "string"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
    )
}

/// The Go kinds a type name can be (upstream `GO_TYPE_KINDS`).
fn is_go_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct | NodeKind::Interface | NodeKind::TypeAlias
    )
}

/// `text` with its whitespace dropped.
fn squeezed(text: &str) -> String {
    SPACES.replace_all(text, "").into_owned()
}

/// JavaScript's `trimStart`.
fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(is_js_space)
}

/// Whether the byte before `at` is an identifier character or `.`
/// (upstream's `(?<![\w.])` rejects such a start).
fn follows_word_or_dot(text: &str, at: usize) -> bool {
    at > 0
        && matches!(
            text.as_bytes()[at - 1],
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.'
        )
}

/// The offset one character on from `at`.
fn next_start(text: &str, at: usize) -> usize {
    at + text[at..].chars().next().map_or(1, char::len_utf8)
}

/// Each match of `re` in `text` in a JavaScript global search's order, with
/// upstream's `(?<![\w.])` before it and `accept` after it: a start either
/// rejects is retried one character on.
fn selector_matches<'t>(
    re: &Regex,
    text: &'t str,
    accept: impl Fn(&Captures<'t>) -> bool,
) -> Vec<Captures<'t>> {
    selector_matches_from(re, text, 0, accept)
}

/// [`selector_matches`] with the search starting at offset `from`.
fn selector_matches_from<'t>(
    re: &Regex,
    text: &'t str,
    mut from: usize,
    accept: impl Fn(&Captures<'t>) -> bool,
) -> Vec<Captures<'t>> {
    let mut out = Vec::new();
    while from <= text.len() {
        let Some(captures) = re.captures_at(text, from) else {
            break;
        };
        let whole = captures.get(0).expect("a match has a whole");
        if follows_word_or_dot(text, whole.start()) || !accept(&captures) {
            from = next_start(text, whole.start());
            continue;
        }
        from = whole.end();
        out.push(captures);
    }
    out
}

/// Whether the byte at `at` is not `=` (upstream's `(?!=)` there).
fn no_equals_at(text: &str, at: usize) -> bool {
    text.as_bytes().get(at) != Some(&b'=')
}

/// The most func field names, and name bytes, [`FieldUses`] spells out in
/// one pattern. Its compile time grows with them (about 0.6 ms at 47
/// names, 1.2 ms at 256, 5 ms at 1024, and at 16384 the pattern passes the
/// regex crate's size limit), so past these the search over every name is
/// read instead.
const MAX_PATTERN_NAMES: usize = 256;
const MAX_PATTERN_NAME_BYTES: usize = 8 * 1024;

/// How the pass finds the uses `.F(` and `.F =` of the func fields'
/// names: one pattern naming them (W7-15d's cost revision), or, past
/// [`MAX_PATTERN_NAMES`] names or [`MAX_PATTERN_NAME_BYTES`] bytes or when
/// that pattern does not compile, upstream's `SELECTOR_USE_RE` over every
/// name with the name checked at each use.
enum FieldUses<'n> {
    Named(Regex),
    AnyName(&'n HashSet<String>),
}

impl<'n> FieldUses<'n> {
    fn new(names: &'n HashSet<String>) -> Self {
        let bytes: usize = names.iter().map(String::len).sum();
        if names.len() <= MAX_PATTERN_NAMES && bytes <= MAX_PATTERN_NAME_BYTES {
            let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            let names: Vec<String> = sorted.into_iter().map(regex::escape).collect();
            let pattern = format!(r"\.\s*({})\s*([(=])", names.join("|"));
            if let Ok(named) = Regex::new(&js(&pattern)) {
                return Self::Named(named);
            }
        }
        Self::AnyName(names)
    }

    /// The first use from `from` on in `text`, a `.F =` never `==`
    /// (upstream's `(?!=)`): its start, the `.`, and its end. A use holds
    /// one `.`, its first character, so no two overlap, and the uses the
    /// pattern naming the func fields finds are exactly those of their names
    /// a search over every name finds.
    fn next(&self, text: &str, mut from: usize) -> Option<(usize, usize)> {
        let pattern = match self {
            Self::Named(named) => named,
            Self::AnyName(_) => &*SELECTOR_USE,
        };
        while from <= text.len() {
            let captures = pattern.captures_at(text, from)?;
            let whole = captures.get(0).expect("a match has a whole");
            if &captures[2] == "=" && !no_equals_at(text, whole.end()) {
                from = next_start(text, whole.start());
                continue;
            }
            if let Self::AnyName(names) = self
                && !names.contains(&captures[1])
            {
                from = whole.end();
                continue;
            }
            return Some((whole.start(), whole.end()));
        }
        None
    }

    /// Every use in `text`, in order.
    fn all(&self, text: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some((start, end)) = self.next(text, from) {
            out.push((start, end));
            from = end;
        }
        out
    }
}

/// Where the run of identifier characters, spaces and dots ending at `at`
/// starts.
fn receiver_run_start(text: &str, at: usize) -> usize {
    text[..at]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_ascii_alphanumeric() || c == '_' || c == '.' || is_js_space(c))
        .last()
        .map_or(at, |(start, _)| start)
}

/// The calls `x.F(` and the assignments `x.F = v` through the func fields
/// in `text`, each in text order: the matches of upstream's
/// `SELECTOR_CALL_RE` and `SELECTOR_ASSIGN_RE` whose field is one, read
/// at `uses`, the fields' uses in `text`.
///
/// A match is its receiver's identifier characters, spaces and dots
/// followed by a use, so it starts in the run of such characters before one
/// use and ends where that use does, and two matches that overlap end at
/// one use. The search over the whole text reaches each run before any
/// start in it, and finds nothing else there: searching each run on its own
/// finds the same matches.
fn field_selectors<'t>(
    text: &'t str,
    uses: &[(usize, usize)],
) -> (Vec<Captures<'t>>, Vec<Captures<'t>>) {
    let mut calls = Vec::new();
    let mut assignments = Vec::new();
    for &(start, end) in uses {
        let run = receiver_run_start(text, start);
        let until_use = &text[..end];
        if until_use.ends_with('(') {
            calls.extend(selector_matches_from(
                &SELECTOR_CALL,
                until_use,
                run,
                |_| true,
            ));
        } else {
            assignments.extend(selector_matches_from(
                &SELECTOR_ASSIGN,
                until_use,
                run,
                |captures| no_equals_at(text, captures.get(0).expect("a match has a whole").end()),
            ));
        }
    }
    (calls, assignments)
}

/// A keyed element's key and where its value starts (upstream's
/// `/^([A-Za-z_]\w*)\s*:(?!=)\s*/`).
fn literal_key(item: &str) -> Option<(&str, usize)> {
    let captures = KEY.captures(item)?;
    let colon_end = captures.get(0)?.end();
    if !no_equals_at(item, colon_end) {
        return None;
    }
    let rest = &item[colon_end..];
    let value_at = colon_end + (rest.len() - js_trim_start(rest).len());
    Some((captures.get(1)?.as_str(), value_at))
}

/// A declaration's names from the one at its start, to its `:=` or `=`:
/// the names after the first (upstream's `from[1]`) and the match's length.
fn names_from(text: &str) -> Option<(&str, usize)> {
    let captures = NAMES_FROM.captures(text)?;
    let whole = captures.get(0)?;
    if &captures[2] == "=" && !no_equals_at(text, whole.end()) {
        return None;
    }
    Some((
        captures.get(1).map_or("", |rest| rest.as_str()),
        whole.end(),
    ))
}

/// A type assertion's type at the end of `value` (upstream's
/// `/\.\s*\(\s*(?!type\s*\))(\*?\s*IDENT(?:\s*\.\s*IDENT)?)\s*\)$/`): the
/// leftmost `.(` whose rest is a type closing the value, the spaces after
/// `(` given back one by one as a backtracking search gives them back.
fn asserted_type(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    for (dot, _) in value.match_indices('.') {
        let after_dot = &value[dot + 1..];
        let open = dot + 1 + (after_dot.len() - js_trim_start(after_dot).len());
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let inner = &value[open + 1..];
        let spaces = inner.len() - js_trim_start(inner).len();
        // The longest run of spaces first, then each shorter one.
        let mut starts: Vec<usize> = inner[..spaces]
            .char_indices()
            .map(|(at, _)| open + 1 + at)
            .collect();
        starts.push(open + 1 + spaces);
        for start in starts.into_iter().rev() {
            let rest = &value[start..];
            if TYPE_CLOSE.is_match(rest) {
                continue;
            }
            if let Some(captures) = ASSERTED_TAIL.captures(rest) {
                return captures.get(1).map(|ty| &rest[ty.start()..ty.end()]);
            }
        }
    }
    None
}

/// The `type)` a type switch writes (upstream's `(?!type\s*\))`).
static TYPE_CLOSE: LazyLock<Regex> = LazyLock::new(|| compile(r"^type\s*\)"));
/// An asserted type closing its value.
static ASSERTED_TAIL: LazyLock<Regex> =
    LazyLock::new(|| compile(&format!(r"^(\*?\s*{IDENT}(?:\s*\.\s*{IDENT})?)\s*\)$")));

/// Source with comments and literal contents blanked, and its line starts
/// (upstream `MaskedFile`).
struct Masked {
    text: String,
    line_starts: Vec<usize>,
}

impl Masked {
    fn new(raw: &str) -> Self {
        let text = blank_go_code(raw);
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Self { text, line_starts }
    }

    /// The 1-based line of `offset` (upstream `lineAt`).
    fn line_at(&self, offset: usize) -> i64 {
        self.line_starts.partition_point(|&start| start <= offset) as i64
    }

    /// The offset of `line` (1-based) and `column` (upstream `offsetOf`).
    fn offset_of(&self, line: i64, column: i64) -> usize {
        let start = usize::try_from(line - 1)
            .ok()
            .and_then(|index| self.line_starts.get(index).copied())
            .unwrap_or(self.text.len());
        start + usize::try_from(column).unwrap_or(0)
    }

    /// The start of the line after `line` (1-based), or the text's end.
    fn line_end(&self, line: i64) -> usize {
        usize::try_from(line)
            .ok()
            .and_then(|index| self.line_starts.get(index).copied())
            .unwrap_or(self.text.len())
    }
}

/// The scope index of a file whose text is `raw` and masked text `masked`
/// ([`GoScopeIndex::read`]). Without a `\r` the code it blanks is `raw`
/// less one final newline, and blanking that is `masked`'s text less its
/// final newline, so that is read rather than blanking `raw` again;
/// otherwise, or when blanking left no text, `raw` is read.
fn scope_index_of(raw: &str, masked: &Masked) -> GoScopeIndex {
    if raw.contains('\r') || masked.text.len() != raw.len() {
        return GoScopeIndex::read(raw);
    }
    let code = masked.text.strip_suffix('\n').unwrap_or(&masked.text);
    GoScopeIndex::from_blanked(code.to_string())
}

/// The offset of the bracket matching the one at `open`, any kind counting
/// (upstream `matchingBracket`).
fn matching_bracket(text: &str, open: usize) -> Option<usize> {
    let mut depth: i64 = 0;
    for (at, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The top-level items between the bracket at `open` and its match at
/// `close`, trimmed, each with where it starts (upstream `listItems`).
fn list_items(text: &str, open: usize, close: usize) -> Vec<(&str, usize)> {
    let mut items = Vec::new();
    let mut depth: i64 = 0;
    let mut start = open + 1;
    for (at, byte) in text.bytes().enumerate().take(close).skip(open + 1) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                push_item(&mut items, text, start, at);
                start = at + 1;
            }
            _ => {}
        }
    }
    push_item(&mut items, text, start, close);
    items
}

/// `text[start..end]` trimmed and where it starts, unless it is blank.
fn push_item<'t>(items: &mut Vec<(&'t str, usize)>, text: &'t str, start: usize, end: usize) {
    let raw = &text[start..end];
    let trimmed = js_trim(raw);
    if !trimmed.is_empty() {
        items.push((trimmed, start + (raw.len() - js_trim_start(raw).len())));
    }
}

/// Where the statement from `from` ends: a line break or `;` outside
/// brackets, or a bracket closing around it (upstream `statementEnd`).
fn statement_end(text: &str, from: usize) -> usize {
    let mut depth: i64 = 0;
    for (at, byte) in text.bytes().enumerate().skip(from) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return at;
                }
                depth -= 1;
            }
            b'\n' | b';' if depth == 0 => return at,
            _ => {}
        }
    }
    text.len()
}

/// Each item's type in a Go result list: a named list's names take the type
/// after them, a list of bare types is its items (upstream `goListTypes`).
fn go_list_types(items: &[&str]) -> Vec<String> {
    let split = |item: &str| -> Option<String> {
        let captures = NAMED_ITEM.captures(js_trim(item))?;
        (!is_type_keyword(&captures[1])).then(|| js_trim(&captures[2]).to_string())
    };
    let Some(last) = items.last() else {
        return Vec::new();
    };
    if split(last).is_none() {
        return items.iter().map(|item| js_trim(item).to_string()).collect();
    }
    let mut types = vec![String::new(); items.len()];
    let mut ty = String::new();
    for (at, item) in items.iter().enumerate().rev() {
        if let Some(named) = split(item) {
            ty = named;
        }
        types[at] = ty.clone();
    }
    types
}

/// The result types of a signature `(params) results` as extraction stores
/// it; `None` for another shape (upstream `goResultTypes`).
fn go_result_types(signature: Option<&str>) -> Option<Vec<String>> {
    let blanked = signature.map(blank_go_code).unwrap_or_default();
    let sig = js_trim(&blanked);
    if !sig.starts_with('(') {
        return None;
    }
    let close = matching_bracket(sig, 0)?;
    let rest = js_trim(&sig[close + 1..]);
    if !rest.starts_with('(') {
        return Some(if rest.is_empty() {
            Vec::new()
        } else {
            vec![rest.to_string()]
        });
    }
    let end = matching_bracket(rest, 0)?;
    let items: Vec<&str> = list_items(rest, 0, end)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    Some(go_list_types(&items))
}

/// A file's package directory, `.` at the root (upstream `pkgDir`).
fn pkg_dir(file: &str) -> String {
    let file = file.replace('\\', "/");
    match file.rfind('/') {
        Some(slash) => file[..slash].to_string(),
        None => ".".to_string(),
    }
}

/// One field of a Go struct, in declaration order (upstream `GoField`).
struct GoField {
    name: String,
    /// The type as written, comments and tag dropped.
    ty: String,
    embedded: bool,
}

/// A struct type, by package directory and name; build-tag twins share one
/// (upstream `GoStructType`, its key the struct's index in
/// [`Reader::structs`]).
struct GoStruct {
    name: String,
    dir: String,
    pkg: String,
    node: Node,
    fields: Vec<GoField>,
    func_fields: HashSet<String>,
}

/// What a type expression names (upstream `ResolvedType`).
#[derive(Clone)]
enum Resolved {
    Struct(usize),
    Interface(Box<Node>),
}

/// A function put in a field, and where (upstream `FieldValue`).
#[derive(Clone)]
struct FieldValue {
    target: Node,
    site: String,
    test: bool,
    via: Option<String>,
}

/// What a value put in a field is (upstream `ValueOutcome`).
enum Outcome {
    Fn(Box<Node>),
    Copy { holder: usize, field: String },
}

/// A type as written, with the file whose package and imports qualify it
/// (upstream `TypeText`).
#[derive(Clone)]
struct TypeText {
    ty: String,
    file: String,
}

/// The project types a name written in a file can mean (upstream
/// `namedTypes`).
struct NamedTypes {
    name: String,
    dir: String,
    nodes: Vec<Node>,
}

/// Production wiring before test wiring, then by site, then by target
/// (upstream `wiringOrder`): the order a capped channel keeps its values in.
fn wiring_order(a: &FieldValue, b: &FieldValue) -> Ordering {
    a.test
        .cmp(&b.test)
        .then_with(|| locale_compare_numeric(&a.site, &b.site))
        .then_with(|| a.target.id.cmp(&b.target.id))
}

/// One pass's reads, each kept once.
struct Reader<'v, 'a> {
    view: &'v View<'a>,
    modules: Vec<GoModule>,
    masked: RefCell<HashMap<String, Option<Arc<Masked>>>>,
    scopes: RefCell<HashMap<String, Option<Arc<GoScopeIndex>>>>,
    imports: RefCell<HashMap<String, Arc<Vec<ImportMapping>>>>,
    by_name: RefCell<HashMap<String, Arc<Vec<Node>>>>,
    import_dirs: RefCell<HashMap<(String, String), Option<String>>>,
    fn_refs: RefCell<HashMap<String, Arc<Vec<Edge>>>>,
    static_calls: RefCell<HashMap<String, Arc<Vec<Edge>>>>,
    /// A `type_alias` node's declared type as written after its name.
    specs: HashMap<String, Option<String>>,
    structs: Vec<GoStruct>,
    struct_at: HashMap<String, usize>,
}

impl<'v, 'a> Reader<'v, 'a> {
    /// `file`'s masked text, `None` when it is not what the index holds.
    fn masked(&self, file: &str) -> Option<Arc<Masked>> {
        if let Some(hit) = self.masked.borrow().get(file) {
            return hit.clone();
        }
        let masked = self
            .view
            .text(file)
            .map(|text| Arc::new(Masked::new(&text)));
        self.masked
            .borrow_mut()
            .insert(file.to_string(), masked.clone());
        masked
    }

    fn scope_index(&self, file: &str) -> Option<Arc<GoScopeIndex>> {
        if let Some(hit) = self.scopes.borrow().get(file) {
            return hit.clone();
        }
        let index = match (self.view.text(file), self.masked(file)) {
            (Some(text), Some(masked)) => Some(Arc::new(scope_index_of(&text, &masked))),
            _ => None,
        };
        self.scopes
            .borrow_mut()
            .insert(file.to_string(), index.clone());
        index
    }

    /// The Go nodes named `name`, in node order.
    fn nodes_named(&self, name: &str) -> Result<Arc<Vec<Node>>> {
        if let Some(hit) = self.by_name.borrow().get(name) {
            return Ok(hit.clone());
        }
        let nodes: Arc<Vec<Node>> = Arc::new(
            self.view
                .nodes_named(name)?
                .into_iter()
                .filter(|node| node.language == Language::Go)
                .collect(),
        );
        self.by_name
            .borrow_mut()
            .insert(name.to_string(), nodes.clone());
        Ok(nodes)
    }

    /// The directory of the package `file` imports as `qualifier` (upstream
    /// `importDir`).
    fn import_dir(&self, qualifier: &str, file: &str) -> Option<String> {
        let key = (file.to_string(), qualifier.to_string());
        if let Some(hit) = self.import_dirs.borrow().get(&key) {
            return hit.clone();
        }
        let imports = self.imports_of(file);
        // Upstream's `getGoPackageDir` names the root package `.`, as
        // `pkg_dir` does; the resolver's own rule names it `""`.
        let dir = imports
            .iter()
            .find(|import| import.local_name == qualifier)
            .and_then(|import| go_module_package_dir(&import.source, Some(file), &self.modules))
            .map(|dir| if dir.is_empty() { ".".to_string() } else { dir });
        self.import_dirs.borrow_mut().insert(key, dir.clone());
        dir
    }

    fn imports_of(&self, file: &str) -> Arc<Vec<ImportMapping>> {
        if let Some(hit) = self.imports.borrow().get(file) {
            return hit.clone();
        }
        let imports = Arc::new(
            self.view
                .text(file)
                .map(|text| extract_import_mappings(&text, Language::Go))
                .unwrap_or_default(),
        );
        self.imports
            .borrow_mut()
            .insert(file.to_string(), imports.clone());
        imports
    }

    /// The Go types named `name` in the package at `dir` (upstream
    /// `typeNodes`).
    fn type_nodes(&self, name: &str, dir: &str) -> Result<Vec<Node>> {
        if is_predeclared_type(name) {
            return Ok(Vec::new());
        }
        Ok(self
            .nodes_named(name)?
            .iter()
            .filter(|node| is_go_type_kind(node.kind) && pkg_dir(&node.file_path) == dir)
            .cloned()
            .collect())
    }

    /// The declared type of a `type` spec node (upstream `specType`).
    fn spec_type(&self, node: &Node) -> Option<&str> {
        self.specs.get(&node.id).and_then(Option::as_deref)
    }

    /// Whether a `type` spec declares a function type (upstream
    /// `isFuncTypeNode`).
    fn is_func_type_node(&self, node: &Node) -> bool {
        node.kind == NodeKind::TypeAlias && FUNC_TYPE.is_match(self.spec_type(node).unwrap_or(""))
    }

    /// The project types a name written in `file` can mean: `T` in its own
    /// package, `pkg.T` in the imported one (upstream `namedTypes`).
    fn named_types(&self, text: &str, file: &str) -> Result<Option<NamedTypes>> {
        let Some(captures) = NAMED_TYPE.captures(js_trim(text)) else {
            return Ok(None);
        };
        let first = captures.get(1).expect("a named type has a name").as_str();
        let second = captures.get(2).map(|name| name.as_str());
        if second.is_none() && is_predeclared_type(first) {
            return Ok(None);
        }
        let name = second.unwrap_or(first);
        // No Go type has the name, so none in any package: `None`, which no
        // caller tells from an empty list, without reading the imports
        // (W7-15d's cost revision).
        if !self
            .nodes_named(name)?
            .iter()
            .any(|node| is_go_type_kind(node.kind))
        {
            return Ok(None);
        }
        let dir = match second {
            Some(_) => self.import_dir(first, file),
            None => Some(pkg_dir(file)),
        };
        let Some(dir) = dir.filter(|dir| !dir.is_empty()) else {
            return Ok(None);
        };
        let name = name.to_string();
        let nodes = self.type_nodes(&name, &dir)?;
        Ok(Some(NamedTypes { name, dir, nodes }))
    }

    /// Whether a field's declared type can be called: `func(…)…`, or a
    /// project func type, not a pointer to one (upstream
    /// `isFuncFieldType`).
    fn is_func_field_type(&self, ty: &str, file: &str) -> Result<bool> {
        if FUNC_TYPE.is_match(ty) {
            return Ok(true);
        }
        if ty.starts_with('*') {
            return Ok(false);
        }
        Ok(self.named_types(ty, file)?.is_some_and(|named| {
            !named.nodes.is_empty() && named.nodes.iter().all(|node| self.is_func_type_node(node))
        }))
    }

    /// What a type written in `file` names: a project struct or interface,
    /// a defined type or alias followed at most three deep (upstream
    /// `resolveType`).
    fn resolve_type(&self, text: &str, file: &str, depth: u32) -> Result<Option<Resolved>> {
        if depth > 3 {
            return Ok(None);
        }
        let Some(named) = self.named_types(text, file)? else {
            return Ok(None);
        };
        if let Some(&at) = self
            .struct_at
            .get(&format!("{}\0{}", named.dir, named.name))
        {
            return Ok(Some(Resolved::Struct(at)));
        }
        if let Some(interface) = named
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Interface)
        {
            return Ok(Some(Resolved::Interface(Box::new(interface.clone()))));
        }
        let Some(first) = named.nodes.first() else {
            return Ok(None);
        };
        match self.spec_type(first) {
            Some(under) => self.resolve_type(under, &first.file_path, depth + 1),
            None => Ok(None),
        }
    }

    /// Whether `s` declares a method named `name` (upstream `hasMethod`).
    fn has_method(&self, s: &GoStruct, name: &str) -> Result<bool> {
        let qualified = format!("{}::{name}", s.name);
        Ok(self.nodes_named(name)?.iter().any(|node| {
            node.kind == NodeKind::Method
                && node.qualified_name == qualified
                && pkg_dir(&node.file_path) == s.dir
        }))
    }

    /// The struct whose func field `field` a value of the struct at `at`
    /// reaches: its own, or the one embedding promotes it from (upstream
    /// `fieldOwner`).
    fn field_owner(&self, at: usize, field: &str) -> Result<Option<usize>> {
        let s = &self.structs[at];
        if s.func_fields.contains(field) {
            return Ok(Some(at));
        }
        if s.fields.iter().any(|f| f.name == field) || self.has_method(s, field)? {
            return Ok(None);
        }
        let mut holders = Vec::new();
        for embedded in s.fields.iter().filter(|f| f.embedded) {
            if let Some(Resolved::Struct(holder)) =
                self.resolve_type(&embedded.ty, &s.node.file_path, 0)?
                && self.structs[holder].fields.iter().any(|f| f.name == field)
            {
                holders.push(holder);
            }
        }
        Ok(match holders.as_slice() {
            [holder] if self.structs[*holder].func_fields.contains(field) => Some(*holder),
            _ => None,
        })
    }

    /// The base edges of `kind` out of `owner` on `line` whose stored
    /// reference there, as `is_ref` tells it apart, is named `name`: the
    /// targets, each once (upstream's `e.line === line && e.metadata.refName
    /// === name`; the edges carry no reference name, the stored references
    /// of the spot do).
    fn targets_named(
        &self,
        edges: &[Edge],
        owner: &str,
        line: i64,
        name: &str,
        is_ref: impl Fn(&codegraph_core::types::UnresolvedRef) -> bool,
    ) -> Result<Vec<String>> {
        let mut targets: Vec<String> = Vec::new();
        for edge in edges.iter().filter(|edge| edge.line == Some(line)) {
            let Some(col) = edge.col else {
                continue;
            };
            let named = self
                .view
                .refs_at(ReferenceSite {
                    from_node_id: owner.to_string(),
                    line,
                    col,
                })?
                .iter()
                .any(|stored| is_ref(stored) && stored.reference_name == name);
            if named && !targets.contains(&edge.target) {
                targets.push(edge.target.clone());
            }
        }
        Ok(targets)
    }

    /// `owner`'s function-value `references` edges (upstream `fnRefs`).
    fn fn_refs(&self, owner: &str) -> Result<Arc<Vec<Edge>>> {
        if let Some(hit) = self.fn_refs.borrow().get(owner) {
            return Ok(hit.clone());
        }
        let edges: Arc<Vec<Edge>> = Arc::new(
            self.view
                .edges_from(owner, EdgeKind::References)?
                .into_iter()
                .filter(|edge| {
                    edge.metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("fnRef"))
                        .and_then(Value::as_bool)
                        == Some(true)
                })
                .collect(),
        );
        self.fn_refs
            .borrow_mut()
            .insert(owner.to_string(), edges.clone());
        Ok(edges)
    }

    /// `function`'s resolved `calls` edges (upstream `staticCalls`).
    fn static_calls(&self, function: &str) -> Result<Arc<Vec<Edge>>> {
        if let Some(hit) = self.static_calls.borrow().get(function) {
            return Ok(hit.clone());
        }
        let edges: Arc<Vec<Edge>> = Arc::new(
            self.view
                .edges_from(function, EdgeKind::Calls)?
                .into_iter()
                .filter(|edge| edge.provenance.as_deref() != Some("heuristic"))
                .collect(),
        );
        self.static_calls
            .borrow_mut()
            .insert(function.to_string(), edges.clone());
        Ok(edges)
    }

    /// What the right side of a declaration gives its `index`th name of
    /// `names` (upstream `rhsType`).
    fn rhs_type(
        &self,
        f: &Masked,
        function: &Node,
        rhs_start: usize,
        index: usize,
        names: usize,
    ) -> Result<Option<TypeText>> {
        let rhs = &f.text[rhs_start..statement_end(&f.text, rhs_start)];
        if RANGE.is_match(rhs) {
            return Ok(None);
        }
        let lead = rhs.len() - js_trim_start(rhs).len();
        let wrapped = format!("({rhs})");
        let values = list_items(&wrapped, 0, rhs.len() + 1);
        let (value, result) = if names > 1 && values.len() == names {
            (values[index].0, 0)
        } else if values.len() == 1 {
            (values[0].0, index)
        } else {
            return Ok(None);
        };
        // A literal, `new(T)` or an assertion types its value's only result;
        // for another result upstream's `typeAt` gives nothing and the call
        // below is tried.
        if result == 0 {
            let first_group = |re: &Regex| {
                re.captures(value)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str())
            };
            if let Some(found) = first_group(&LITERAL)
                .or_else(|| first_group(&CREATED))
                .or_else(|| asserted_type(value))
            {
                return Ok(Some(TypeText {
                    ty: found.to_string(),
                    file: function.file_path.clone(),
                }));
            }
        }
        // `f(…)`, `pkg.F(…)`, `x.M(…)`: the whole value one call the resolver
        // linked.
        let Some(call) = CALL.captures(value) else {
            return Ok(None);
        };
        let open = call.get(0).expect("a match has a whole").end() - 1;
        if matching_bracket(value, open) != Some(value.len() - 1) {
            return Ok(None);
        }
        let name = squeezed(&call[1]);
        let line = f.line_at(rhs_start + lead);
        let calls = self.static_calls(&function.id)?;
        let callees = self.targets_named(&calls, &function.id, line, &name, |stored| {
            stored.reference_kind == EdgeKind::Calls && !stored.is_function_ref
        })?;
        let [callee] = callees.as_slice() else {
            return Ok(None);
        };
        let Some(callee) = self.view.node(callee)? else {
            return Ok(None);
        };
        if !matches!(callee.kind, NodeKind::Function | NodeKind::Method) {
            return Ok(None);
        }
        Ok(go_result_types(callee.signature.as_deref())
            .and_then(|types| types.get(result).cloned())
            .filter(|ty| !ty.is_empty())
            .map(|ty| TypeText {
                ty,
                file: callee.file_path.clone(),
            }))
    }

    /// What `name` is where offset `at` uses it in `function`: the receiver,
    /// parameter or local in scope there with its type, or `Some(None)` when
    /// that can't be told; `None` when the function declares no such name
    /// (upstream `lookupName`).
    fn lookup_name(
        &self,
        function: &Node,
        f: &Masked,
        name: &str,
        at: usize,
    ) -> Result<Option<Option<TypeText>>> {
        let line = f.line_at(at);
        let column = at - f.line_starts[usize::try_from(line - 1).unwrap_or(0)];
        let Some(index) = self.scope_index(&function.file_path) else {
            return Ok(None);
        };
        let Some(decl) = index.local_decl(name, line, column as i64) else {
            return Ok(None);
        };
        if let Some(ty) = &decl.declared_type {
            return Ok(Some((!ty.starts_with("...")).then(|| TypeText {
                ty: ty.clone(),
                file: function.file_path.clone(),
            })));
        }
        let (decl_line, decl_column) = index.position(&decl);
        let p = f.offset_of(decl_line, decl_column);
        let window_end = floor_char_boundary(&f.text, (p + 400).min(f.text.len()));
        let Some(window) = f.text.get(p..window_end) else {
            return Ok(Some(None));
        };
        let Some((later, from_len)) = names_from(window) else {
            return Ok(Some(None));
        };
        let line_start = f.line_starts[usize::try_from(decl_line - 1).unwrap_or(0)];
        let before = NAMES_BEFORE
            .find(&f.text[line_start..p])
            .map_or("", |found| found.as_str());
        let index = if before.is_empty() {
            0
        } else {
            before.split(',').count() - 1
        };
        let names = index
            + if later.is_empty() {
                1
            } else {
                later.split(',').count()
            };
        Ok(Some(self.rhs_type(
            f,
            function,
            p + from_len,
            index,
            names,
        )?))
    }

    /// The struct or interface `x` / `x.y` has where offset `at` uses it in
    /// `function` (upstream `typeOfExpr`).
    fn type_of_expr(
        &self,
        function: &Node,
        f: &Masked,
        expr: &str,
        at: usize,
    ) -> Result<Option<Resolved>> {
        let mut segments = expr.split('.');
        let base = segments.next().unwrap_or("");
        let base = self.lookup_name(function, f, base, at)?.flatten();
        let mut resolved = match base {
            Some(base) => self.resolve_type(&base.ty, &base.file, 0)?,
            None => None,
        };
        for segment in segments {
            let Some(Resolved::Struct(at)) = resolved else {
                return Ok(None);
            };
            let mut holder = at;
            let mut field = self.structs[holder]
                .fields
                .iter()
                .position(|f| f.name == segment);
            if field.is_none() {
                // Promoted from a struct embedded one level down.
                let mut owners = Vec::new();
                for embedded in self.structs[holder].fields.iter().filter(|f| f.embedded) {
                    if let Some(Resolved::Struct(owner)) =
                        self.resolve_type(&embedded.ty, &self.structs[holder].node.file_path, 0)?
                        && self.structs[owner].fields.iter().any(|f| f.name == segment)
                    {
                        owners.push(owner);
                    }
                }
                let [owner] = owners.as_slice() else {
                    return Ok(None);
                };
                holder = *owner;
                field = self.structs[holder]
                    .fields
                    .iter()
                    .position(|f| f.name == segment);
            }
            resolved = match field {
                Some(field) => {
                    let s = &self.structs[holder];
                    self.resolve_type(&s.fields[field].ty, &s.node.file_path, 0)?
                }
                None => None,
            };
        }
        Ok(resolved)
    }

    /// The one method named `member` a type declares: an interface's, or a
    /// struct's own (upstream `methodOf`).
    fn method_of(&self, type_node: &Node, member: &str) -> Result<Option<Node>> {
        let mut found = Vec::new();
        for edge in self.view.edges_from(&type_node.id, EdgeKind::Contains)? {
            if let Some(node) = self.view.node(&edge.target)?
                && node.kind == NodeKind::Method
                && node.name == member
            {
                found.push(node);
            }
        }
        Ok(match <[Node; 1]>::try_from(found) {
            Ok([method]) => Some(method),
            Err(_) => None,
        })
    }

    /// The Go functions named `name` in the package at `dir`.
    fn functions_in(&self, name: &str, dir: &str) -> Result<Vec<Node>> {
        Ok(self
            .nodes_named(name)?
            .iter()
            .filter(|node| node.kind == NodeKind::Function && pkg_dir(&node.file_path) == dir)
            .cloned()
            .collect())
    }

    /// What a value put in a field is: a function or method, another func
    /// field's values, or nothing known (upstream `valueOf`). `owner`'s
    /// function-value references at the value's `line` are the resolver's
    /// reading of it; `function` is the function the value is written in,
    /// when `owner` is one.
    fn value_of(
        &self,
        owner: &Node,
        function: Option<&Node>,
        f: &Masked,
        value: &str,
        at: usize,
        line: i64,
    ) -> Result<Option<Outcome>> {
        let mut v = js_trim(value);
        // A conversion to a func type is the value it converts.
        if let Some(conversion) = CONVERSION.captures(v) {
            let open = conversion.get(0).expect("a match has a whole").end() - 1;
            if matching_bracket(v, open) == Some(v.len() - 1) {
                let named = self.named_types(&conversion[1], &owner.file_path)?;
                let converts = named.is_some_and(|named| {
                    !named.nodes.is_empty()
                        && named.nodes.iter().all(|node| self.is_func_type_node(node))
                });
                if !converts {
                    return Ok(None);
                }
                v = js_trim(&v[open + 1..v.len() - 1]);
            }
        }
        if !PLAIN_VALUE.is_match(v) {
            return Ok(None);
        }
        let name = squeezed(v);
        let (base, member) = match name.split_once('.') {
            Some((base, member)) => (base.to_string(), member.to_string()),
            None => (name.clone(), String::new()),
        };
        // What `base` names in the enclosing function: a local, a parameter
        // or the receiver; else something package-level.
        let local = match function {
            Some(function) => self.lookup_name(function, f, &base, at)?,
            None => None,
        };
        // The resolver's own reading of this function value.
        let refs = self.fn_refs(&owner.id)?;
        let refs = self.targets_named(&refs, &owner.id, line, &name, |stored| {
            stored.is_function_ref
        })?;
        let reference = match refs.as_slice() {
            [only] => self.view.node(only)?,
            _ => None,
        };
        let ref_fn =
            reference.filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method));
        if member.is_empty() {
            // A bare name is a package-level function: Go names no method
            // bare, and a local holds some other function.
            if local.is_some() {
                return Ok(None);
            }
            if let Some(found) = ref_fn {
                return Ok((found.kind == NodeKind::Function).then(|| Outcome::Fn(Box::new(found))));
            }
            if !refs.is_empty() {
                return Ok(None);
            }
            // Extraction keeps a bare function value only when its file
            // declares or imports it.
            let functions = self.functions_in(&name, &pkg_dir(&owner.file_path))?;
            return Ok(match <[Node; 1]>::try_from(functions) {
                Ok([only]) => Some(Outcome::Fn(Box::new(only))),
                Err(_) => None,
            });
        }
        let resolved = match local.as_ref().and_then(Option::as_ref) {
            Some(local) => self.resolve_type(&local.ty, &local.file, 0)?,
            None => None,
        };
        match resolved {
            Some(Resolved::Interface(interface)) => {
                if let Some(method) = self.method_of(&interface, &member)? {
                    return Ok(Some(Outcome::Fn(Box::new(method))));
                }
            }
            Some(Resolved::Struct(at)) => {
                // `opts.RefreshF`: another func field, whose functions this
                // one gets.
                if let Some(holder) = self.field_owner(at, &member)? {
                    return Ok(Some(Outcome::Copy {
                        holder,
                        field: member,
                    }));
                }
                if ref_fn.is_none()
                    && let Some(method) = self.method_of(&self.structs[at].node, &member)?
                {
                    return Ok(Some(Outcome::Fn(Box::new(method))));
                }
            }
            None => {}
        }
        if let Some(found) = ref_fn {
            return Ok(Some(Outcome::Fn(Box::new(found))));
        }
        if local.is_some() || !refs.is_empty() {
            return Ok(None);
        }
        // `pkg.Func`.
        let Some(dir) = self.import_dir(&base, &owner.file_path) else {
            return Ok(None);
        };
        let functions = self.functions_in(&member, &dir)?;
        Ok(match <[Node; 1]>::try_from(functions) {
            Ok([only]) => Some(Outcome::Fn(Box::new(only))),
            Err(_) => None,
        })
    }

    /// A struct's fields, read from its declaration in `f` (upstream
    /// `readFields`).
    fn read_fields(f: &Masked, node: &Node) -> Option<Vec<GoField>> {
        let from = f.offset_of(node.start_line, node.start_column);
        let to = f.line_end(node.end_line);
        let found = STRUCT_KEYWORD.find(f.text.get(from..to)?)?;
        let open = from + found.end() - 1;
        let close = matching_bracket(&f.text, open)?;
        let mut fields = Vec::new();
        let take = |fields: &mut Vec<GoField>, start: usize, end: usize| {
            let mut decl = js_trim(&f.text[start..end]);
            // A tag is a string literal, blanked down to its quotes.
            if let Some(quote @ ('`' | '"')) = decl.chars().last() {
                let head = &decl[..decl.len() - 1];
                let cut = head.rfind(quote).unwrap_or(0);
                decl = js_trim(&decl[..cut]);
            }
            if decl.is_empty() {
                return;
            }
            if let Some(embedded) = NAMED_TYPE.captures(decl) {
                let name = embedded.get(2).or_else(|| embedded.get(1));
                fields.push(GoField {
                    name: name.map_or("", |name| name.as_str()).to_string(),
                    ty: decl.to_string(),
                    embedded: true,
                });
                return;
            }
            let Some(named) = FIELD_DECL.captures(decl) else {
                return;
            };
            let ty = js_trim(&SPACES.replace_all(&named[2], " ")).to_string();
            for name in named[1].split(',') {
                fields.push(GoField {
                    name: js_trim(name).to_string(),
                    ty: ty.clone(),
                    embedded: false,
                });
            }
        };
        let mut depth: i64 = 0;
        let mut start = open + 1;
        for (at, byte) in f.text.bytes().enumerate().take(close).skip(open + 1) {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b'\n' | b';' if depth == 0 => {
                    take(&mut fields, start, at);
                    start = at + 1;
                }
                _ => {}
            }
        }
        take(&mut fields, start, close);
        Some(fields)
    }
}

/// The offset at or before `at` that starts a character.
fn floor_char_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// A composite literal of a struct with func fields, by its
/// `instantiates` edge.
struct Literal {
    s: usize,
    owner: Node,
    line: i64,
    column: i64,
}

/// A func field: its struct's index in [`Reader::structs`] and its name
/// (upstream's `${key}\0${field}`).
type Channel = (usize, String);

/// What the parallel read of one Go file found (W7-15d's cost revision).
struct FileRead {
    /// Whether upstream's prefilter finds a use of a func field.
    mentions: bool,
    /// The uses in the file's masked text, when it does.
    uses: Vec<(usize, usize)>,
    /// The file's scope index, when the file is read for its literals or
    /// uses.
    scope: Option<Arc<GoScopeIndex>>,
}

/// A call through a func field (upstream `Dispatch`).
struct Dispatch {
    function: Node,
    line: i64,
    channel: Channel,
    field: String,
}

/// What the values recorded so far hold.
#[derive(Default)]
struct Channels {
    /// Channel → the functions put in it, in record order.
    direct: HashMap<Channel, Vec<FieldValue>>,
    /// Channel → the channels copied into it, with their labels.
    copies: HashMap<Channel, Vec<(Channel, String)>>,
}

impl Channels {
    /// Note what a value put in the struct at `into`'s field `field` is
    /// (upstream `record`).
    fn record(
        &mut self,
        structs: &[GoStruct],
        into: usize,
        field: &str,
        outcome: Option<Outcome>,
        file: &str,
        line: i64,
    ) {
        let Some(outcome) = outcome else {
            return;
        };
        let channel = (into, field.to_string());
        match outcome {
            Outcome::Copy {
                holder,
                field: from_field,
            } => {
                let from = (holder, from_field.clone());
                if from == channel {
                    return;
                }
                let holder_struct = &structs[holder];
                let label = format!(
                    "{}{}.{from_field}",
                    if holder_struct.dir == structs[into].dir {
                        String::new()
                    } else {
                        format!("{}.", holder_struct.pkg)
                    },
                    holder_struct.name
                );
                let list = self.copies.entry(channel).or_default();
                if !list.iter().any(|(existing, _)| *existing == from) {
                    list.push((from, label));
                }
            }
            Outcome::Fn(target) => {
                let value = FieldValue {
                    target: *target,
                    site: format!("{file}:{line}"),
                    test: file.ends_with("_test.go"),
                    via: None,
                };
                let list = self.direct.entry(channel).or_default();
                match list
                    .iter()
                    .position(|existing| existing.target.id == value.target.id)
                {
                    None => list.push(value),
                    Some(had) => {
                        if wiring_order(&value, &list[had]) == Ordering::Less {
                            list[had] = value;
                        }
                    }
                }
            }
        }
    }

    /// A channel's values: its own and those of the channels copied into
    /// it, each target once, in wiring order, capped (upstream `valuesOf`).
    fn values_of(&self, channel: &Channel) -> Vec<FieldValue> {
        let mut by_target: Vec<FieldValue> = Vec::new();
        let mut add = |value: FieldValue| match by_target
            .iter()
            .position(|existing| existing.target.id == value.target.id)
        {
            None => by_target.push(value),
            Some(had) => {
                if wiring_order(&value, &by_target[had]) == Ordering::Less {
                    by_target[had] = value;
                }
            }
        };
        for value in self.direct.get(channel).into_iter().flatten() {
            add(value.clone());
        }
        for (from, label) in self.copies.get(channel).into_iter().flatten() {
            for value in self.direct.get(from).into_iter().flatten() {
                add(FieldValue {
                    via: Some(label.clone()),
                    ..value.clone()
                });
            }
        }
        by_target.sort_by(wiring_order);
        by_target.truncate(MAX_CALLBACKS_PER_CHANNEL);
        by_target
    }
}

/// Go func-field dispatch (upstream `goFuncFieldEdges`): each Go function
/// calling through a func field links to the functions the field is given,
/// at most [`MAX_CALLBACKS_PER_CHANNEL`] a channel.
pub(super) fn func_field_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let all_files = view.files()?;
    let go_files: Vec<String> = all_files
        .iter()
        .filter(|file| file.ends_with(".go"))
        .cloned()
        .collect();
    let is_generated = |file: &str| view.is_flagged_generated(file) || is_generated_file(file);
    let mut reader = Reader {
        view,
        modules: load_go_modules_pub(&view.project_root().to_string_lossy(), &all_files),
        masked: RefCell::default(),
        scopes: RefCell::default(),
        imports: RefCell::default(),
        by_name: RefCell::default(),
        import_dirs: RefCell::default(),
        fn_refs: RefCell::default(),
        static_calls: RefCell::default(),
        specs: HashMap::new(),
        structs: Vec::new(),
        struct_at: HashMap::new(),
    };

    // The Go files' texts, each masked once, in parallel (W7-15d's cost
    // revision): the reads below take them from the reader, which still
    // reads a file outside them when asked for one.
    let texts: Vec<(String, Arc<str>)> = go_files
        .iter()
        .filter(|file| !is_generated(file))
        .filter_map(|file| view.text(file).map(|text| (file.clone(), text)))
        .collect();
    let masks: Vec<Arc<Masked>> = texts
        .par_iter()
        .map(|(_, text)| Arc::new(Masked::new(text)))
        .collect();
    reader.masked.get_mut().extend(
        texts
            .iter()
            .zip(&masks)
            .map(|((file, _), masked)| (file.clone(), Some(Arc::clone(masked)))),
    );

    // Each file declaring a struct or another type is read once: its
    // structs' fields and its specs' types.
    let mut decls_by_file: Vec<(String, Vec<Node>)> = Vec::new();
    let mut file_at: HashMap<String, usize> = HashMap::new();
    for kind in [NodeKind::Struct, NodeKind::TypeAlias] {
        for node in view.nodes_of_kinds_in(&[kind], &[Language::Go])? {
            if is_generated(&node.file_path) {
                continue;
            }
            match file_at.get(&node.file_path) {
                Some(&at) => decls_by_file[at].1.push(node),
                None => {
                    file_at.insert(node.file_path.clone(), decls_by_file.len());
                    decls_by_file.push((node.file_path.clone(), vec![node]));
                }
            }
        }
    }
    let mut decl_files: HashSet<String> = HashSet::new();
    for (file, decls) in &decls_by_file {
        let Some(f) = reader.masked(file) else {
            continue;
        };
        decl_files.insert(file.clone());
        let dir = pkg_dir(file);
        let pkg = PACKAGE
            .captures(&f.text)
            .map(|captures| captures[1].to_string())
            .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(&dir).to_string());
        for node in decls {
            if node.kind == NodeKind::TypeAlias {
                let at = f.offset_of(node.start_line, node.start_column);
                let end = floor_char_boundary(&f.text, (at + 400).min(f.text.len()));
                let spec = f
                    .text
                    .get(at..end)
                    .and_then(|window| TYPE_SPEC.captures(window))
                    .map(|captures| js_trim(&captures[1]).to_string());
                reader.specs.insert(node.id.clone(), spec);
                continue;
            }
            let key = format!("{dir}\0{}", node.name);
            if reader.struct_at.contains_key(&key) {
                continue; // a build-tag twin: the first declaration speaks for both
            }
            let Some(fields) = Reader::read_fields(&f, node) else {
                continue;
            };
            reader.struct_at.insert(key, reader.structs.len());
            reader.structs.push(GoStruct {
                name: node.name.clone(),
                dir: dir.clone(),
                pkg: pkg.clone(),
                node: node.clone(),
                fields,
                func_fields: HashSet::new(),
            });
        }
    }
    let mut func_field_names: HashSet<String> = HashSet::new();
    for at in 0..reader.structs.len() {
        let mut func_fields = HashSet::new();
        for field in reader.structs[at].fields.iter().filter(|f| !f.embedded) {
            if reader.is_func_field_type(&field.ty, &reader.structs[at].node.file_path)? {
                func_fields.insert(field.name.clone());
                func_field_names.insert(field.name.clone());
            }
        }
        reader.structs[at].func_fields = func_fields;
    }
    if func_field_names.is_empty() {
        return Ok(Vec::new());
    }

    // The composite literals of the structs with func fields, by the file
    // they are in: every declaration's `instantiates` edges and their
    // sources read at once (W7-15d's cost revision).
    let mut literal_types: Vec<(usize, Node)> = Vec::new();
    for at in 0..reader.structs.len() {
        if reader.structs[at].func_fields.is_empty() {
            continue;
        }
        let s = &reader.structs[at];
        for decl in reader.type_nodes(&s.name, &s.dir)? {
            if decl.kind == NodeKind::Struct {
                literal_types.push((at, decl));
            }
        }
    }
    let decl_ids: Vec<String> = literal_types
        .iter()
        .map(|(_, decl)| decl.id.clone())
        .collect();
    let instantiations = view.edges_into_each(&decl_ids, EdgeKind::Instantiates)?;
    let owner_ids: Vec<String> = instantiations
        .values()
        .flatten()
        .map(|edge| edge.source.clone())
        .collect();
    let owners = view.nodes(&owner_ids)?;
    let mut literals_by_file: HashMap<String, Vec<Literal>> = HashMap::new();
    for (at, decl) in &literal_types {
        for edge in instantiations.get(&decl.id).into_iter().flatten() {
            let Some(line) = edge.line.filter(|&line| line != 0) else {
                continue;
            };
            let Some(owner) = owners.get(&edge.source) else {
                continue;
            };
            if owner.language != Language::Go {
                continue;
            }
            literals_by_file
                .entry(owner.file_path.clone())
                .or_default()
                .push(Literal {
                    s: *at,
                    owner: owner.clone(),
                    line,
                    column: edge.col.unwrap_or(0),
                });
        }
    }

    // Whether each file uses a func field as `.F(` / `.F =` (a file read for
    // its declarations masked, another raw, as upstream's prefilter reads
    // it), its uses in its masked text, and the scope index of each file
    // read below, in parallel.
    let field_uses = FieldUses::new(&func_field_names);
    let reads: Vec<FileRead> = texts
        .par_iter()
        .zip(masks.par_iter())
        .map(|((file, raw), masked)| {
            let prefiltered: &str = if decl_files.contains(file) {
                &masked.text
            } else {
                raw
            };
            let mentions = field_uses.next(prefiltered, 0).is_some();
            let uses = if mentions {
                field_uses.all(&masked.text)
            } else {
                Vec::new()
            };
            let scope = (mentions || literals_by_file.contains_key(file))
                .then(|| Arc::new(scope_index_of(raw, masked)));
            FileRead {
                mentions,
                uses,
                scope,
            }
        })
        .collect();
    let read_at: HashMap<&str, usize> = texts
        .iter()
        .enumerate()
        .map(|(at, (file, _))| (file.as_str(), at))
        .collect();
    reader
        .scopes
        .get_mut()
        .extend(texts.iter().zip(&reads).filter_map(|((file, _), read)| {
            read.scope.clone().map(|scope| (file.clone(), Some(scope)))
        }));

    let mut channels = Channels::default();
    let mut dispatches: Vec<Dispatch> = Vec::new();
    for file in &go_files {
        if is_generated(file) {
            continue;
        }
        // Without its text a file is not read.
        let Some(&at) = read_at.get(file.as_str()) else {
            continue;
        };
        let read = &reads[at];
        let literals = literals_by_file.get(file);
        if !read.mentions && literals.is_none() {
            continue;
        }
        let Some(f) = reader.masked(file) else {
            continue;
        };
        for literal in literals.into_iter().flatten() {
            read_literal(&reader, &mut channels, &f, literal)?;
        }
        if !read.mentions {
            continue;
        }
        let nodes = view.callables_in_file(file)?;
        let (calls, assignments) = field_selectors(&f.text, &read.uses);
        for call in calls {
            let field = call[2].to_string();
            let start = call.get(0).expect("a match has a whole").start();
            let line = f.line_at(start);
            let Some(function) = enclosing_fn(&nodes, line) else {
                continue;
            };
            if function.language != Language::Go {
                continue;
            }
            let resolved = reader.type_of_expr(function, &f, &squeezed(&call[1]), start)?;
            let holder = match resolved {
                Some(Resolved::Struct(at)) => reader.field_owner(at, &field)?,
                _ => None,
            };
            if let Some(holder) = holder {
                dispatches.push(Dispatch {
                    function: function.clone(),
                    line,
                    field: format!("{}.{field}", reader.structs[holder].name),
                    channel: (holder, field),
                });
            }
        }
        for assignment in assignments {
            let field = assignment[2].to_string();
            let whole = assignment.get(0).expect("a match has a whole");
            let Some(function) = enclosing_fn(&nodes, f.line_at(whole.start())) else {
                continue;
            };
            if function.language != Language::Go {
                continue;
            }
            let resolved =
                reader.type_of_expr(function, &f, &squeezed(&assignment[1]), whole.start())?;
            let holder = match resolved {
                Some(Resolved::Struct(at)) => reader.field_owner(at, &field)?,
                _ => None,
            };
            let Some(holder) = holder else {
                continue;
            };
            let value_start = whole.end();
            let value = &f.text[value_start..statement_end(&f.text, value_start)];
            let at = value_start + (value.len() - js_trim_start(value).len());
            let line = f.line_at(at);
            let outcome = reader.value_of(function, Some(function), &f, value, at, line)?;
            channels.record(&reader.structs, holder, &field, outcome, file, line);
        }
    }

    // The edges.
    let mut values_memo: HashMap<Channel, Vec<FieldValue>> = HashMap::new();
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for dispatch in &dispatches {
        let test_call = dispatch.function.file_path.ends_with("_test.go");
        let values = values_memo
            .entry(dispatch.channel.clone())
            .or_insert_with(|| channels.values_of(&dispatch.channel));
        for value in values.iter() {
            if value.target.id == dispatch.function.id {
                continue;
            }
            // Each package's tests build a binary of their own: what one
            // package's test puts in a field, another package's test never
            // calls.
            let site_file = value.site.rsplit_once(':').map_or("", |(file, _)| file);
            if test_call
                && value.test
                && pkg_dir(site_file) != pkg_dir(&dispatch.function.file_path)
            {
                continue;
            }
            if !seen.insert((dispatch.function.id.clone(), value.target.id.clone())) {
                continue;
            }
            let mut metadata = Map::new();
            metadata.insert("synthesizedBy".into(), Value::from("go-func-field"));
            metadata.insert("field".into(), Value::from(dispatch.field.clone()));
            if let Some(via) = &value.via {
                metadata.insert("via".into(), Value::from(via.clone()));
            }
            metadata.insert("registeredAt".into(), Value::from(value.site.clone()));
            edges.push(synthesized(
                &dispatch.function,
                &value.target,
                EdgeKind::Calls,
                dispatch.line,
                Value::Object(metadata),
            ));
        }
    }
    Ok(edges)
}

/// The functions a struct literal puts in func fields: `S{F: v}`, or
/// positional `S{a, v}` (upstream `readLiteral`).
fn read_literal(
    reader: &Reader<'_, '_>,
    channels: &mut Channels,
    f: &Masked,
    literal: &Literal,
) -> Result<()> {
    let s = &reader.structs[literal.s];
    // The literal's `{` is on its type's line: Go puts no line break between
    // them.
    let line_start = usize::try_from(literal.line - 1)
        .ok()
        .and_then(|index| f.line_starts.get(index).copied())
        .unwrap_or(0);
    let line_end = f.line_end(literal.line);
    let line_text = &f.text[line_start..line_end.max(line_start)];
    let mut open: Option<usize> = None;
    let mut nearest = i64::MAX;
    for (start, end, qualifier) in literal_types(line_text, &s.name) {
        // The type as written must name this struct's package:
        // `refresh.Options` through its import, a bare name its own.
        let dir = match qualifier {
            Some(qualifier) => reader.import_dir(qualifier, &literal.owner.file_path),
            None => Some(pkg_dir(&literal.owner.file_path)),
        };
        if dir.as_deref() != Some(s.dir.as_str()) {
            continue;
        }
        let distance = (start as i64 - literal.column).abs();
        if distance < nearest {
            nearest = distance;
            open = Some(line_start + end - 1);
        }
    }
    let Some(open) = open else {
        return Ok(());
    };
    let Some(close) = matching_bracket(&f.text, open) else {
        return Ok(());
    };
    let function = matches!(literal.owner.kind, NodeKind::Function | NodeKind::Method)
        .then_some(&literal.owner);
    let items = list_items(&f.text, open, close);
    let keys: Vec<Option<(&str, usize)>> =
        items.iter().map(|(text, _)| literal_key(text)).collect();
    let mut fields_at: Vec<(String, &str, usize)> = Vec::new();
    if keys.iter().all(Option::is_some) {
        for ((text, start), key) in items.iter().zip(&keys) {
            let (field, value_at) = key.expect("every item is keyed");
            fields_at.push((field.to_string(), &text[value_at..], start + value_at));
        }
    } else if keys.iter().all(Option::is_none) && items.len() == s.fields.len() {
        for ((text, start), field) in items.iter().zip(&s.fields) {
            fields_at.push((field.name.clone(), text, *start));
        }
    }
    for (field, value, at) in fields_at {
        if !s.func_fields.contains(&field) {
            continue;
        }
        let line = f.line_at(at);
        let outcome = reader.value_of(&literal.owner, function, f, value, at, line)?;
        channels.record(
            &reader.structs,
            literal.s,
            &field,
            outcome,
            &literal.owner.file_path,
            line,
        );
    }
    Ok(())
}

/// The composite literals of the struct `name` written in `line`, in a
/// JavaScript global search's order with upstream's `(?<![\w.])` before
/// each: its start, its end and the qualifier written before the name
/// (upstream's literal pattern, compiled with the name in it).
///
/// For a name that is an ASCII identifier one pattern is tried at each
/// start instead of one compiled a name (W7-15d's cost revision): at a start
/// upstream's pattern matches exactly when [`LITERAL_TYPE`] does there and
/// its second identifier is the name, with the same span and qualifier, as
/// both read an identifier at the start and then a `.` and another one or
/// not, the same way; and a search's next match is the first start at or
/// after its own where one does. Another name keeps upstream's pattern.
fn literal_types<'t>(line: &'t str, name: &str) -> Vec<(usize, usize, Option<&'t str>)> {
    let is_ascii_identifier = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !is_ascii_identifier {
        let pattern = LITERAL_TYPES.with(|cache| {
            cache
                .borrow_mut()
                .entry(name.to_string())
                .or_insert_with(|| {
                    compile(&format!(
                        r"(?:({IDENT})\s*\.\s*)?{}\s*(?:\[[^\]{{}}]*\])?\s*\{{",
                        regex::escape(name)
                    ))
                })
                .clone()
        });
        return selector_matches(&pattern, line, |_| true)
            .iter()
            .map(|found| {
                let whole = found.get(0).expect("a match has a whole");
                (whole.start(), whole.end(), found.get(1).map(|q| q.as_str()))
            })
            .collect();
    }
    let mut out = Vec::new();
    let mut from = 0;
    for (at, c) in line.char_indices() {
        if at < from || !(c.is_ascii_alphabetic() || c == '_') {
            continue;
        }
        let Some(found) = LITERAL_TYPE.captures(&line[at..]) else {
            continue;
        };
        if &found[2] != name || follows_word_or_dot(line, at) {
            continue;
        }
        let end = at + found.get(0).expect("a match has a whole").end();
        out.push((at, end, found.get(1).map(|q| q.as_str())));
        from = end;
    }
    out
}

/// A composite literal's type at the start of the text: `T{`, `pkg.T{`,
/// `T[int]{` (upstream's literal pattern, its name any identifier).
static LITERAL_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    compile(&format!(
        r"^(?:({IDENT})\s*\.\s*)?({IDENT})\s*(?:\[[^\]{{}}]*\])?\s*\{{"
    ))
});

thread_local! {
    /// A struct name's literal pattern, compiled once.
    static LITERAL_TYPES: RefCell<HashMap<String, Regex>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_bracket_counts_every_kind() {
        assert_eq!(matching_bracket("f(a[b{c}])", 1), Some(9));
        assert_eq!(matching_bracket("x{a(b}", 1), None);
        // Any closing bracket closes one level, whichever opened it.
        assert_eq!(matching_bracket("{(})", 0), Some(3));
    }

    #[test]
    fn list_items_split_at_the_top_level_and_trim() {
        let text = "{ a: f(x, y), b: []int{1, 2} ,\n c }";
        let items = list_items(text, 0, text.len() - 1);
        assert_eq!(
            items.iter().map(|(item, _)| *item).collect::<Vec<_>>(),
            ["a: f(x, y)", "b: []int{1, 2}", "c"]
        );
        assert_eq!(items[0].1, 2);
        assert_eq!(&text[items[2].1..items[2].1 + 1], "c");
        assert!(list_items("{ }", 0, 2).is_empty());
    }

    #[test]
    fn statement_end_stops_at_a_break_or_a_closing_bracket() {
        assert_eq!(statement_end("a = f(x,\n y)\nb", 0), 12);
        assert_eq!(statement_end("v; w", 0), 1);
        assert_eq!(statement_end("{ x = v }", 5), 8);
        assert_eq!(statement_end("tail", 0), 4);
    }

    #[test]
    fn go_list_types_read_named_grouped_and_bare_lists() {
        assert_eq!(go_list_types(&["int", "error"]), ["int", "error"]);
        assert_eq!(go_list_types(&["n int", "err error"]), ["int", "error"]);
        assert_eq!(go_list_types(&["a", "b int"]), ["int", "int"]);
        assert_eq!(go_list_types(&["chan int", "error"]), ["chan int", "error"]);
        assert!(go_list_types(&[]).is_empty());
    }

    #[test]
    fn go_result_types_read_a_stored_signature() {
        assert_eq!(
            go_result_types(Some("(ctx context.Context) ([]*Group, error)")),
            Some(vec!["[]*Group".to_string(), "error".to_string()])
        );
        assert_eq!(
            go_result_types(Some("(endpoint string) (r refresher, err error)")),
            Some(vec!["refresher".to_string(), "error".to_string()])
        );
        assert_eq!(
            go_result_types(Some("() *Client")),
            Some(vec!["*Client".to_string()])
        );
        assert_eq!(go_result_types(Some("()")), Some(Vec::new()));
        assert_eq!(go_result_types(Some("int")), None);
        assert_eq!(go_result_types(None), None);
        assert_eq!(go_result_types(Some("(a int")), None);
    }

    #[test]
    fn struct_fields_read_tags_embeddings_semicolons_and_names() {
        let source = "type S struct {\n\tName, Alias string `json:\"name\"`\n\t*refresh.Discovery\n\tBase\n\tf func(ctx context.Context) error; g int\n}\n";
        let f = Masked::new(source);
        let node = Node {
            start_line: 1,
            end_line: 6,
            start_column: 5,
            ..test_node()
        };
        let fields = Reader::read_fields(&f, &node).expect("fields");
        let read: Vec<(&str, &str, bool)> = fields
            .iter()
            .map(|field| (field.name.as_str(), field.ty.as_str(), field.embedded))
            .collect();
        assert_eq!(
            read,
            [
                ("Name", "string", false),
                ("Alias", "string", false),
                ("Discovery", "*refresh.Discovery", true),
                ("Base", "Base", true),
                ("f", "func(ctx context.Context) error", false),
                ("g", "int", false),
            ]
        );
    }

    #[test]
    fn selector_matches_reject_a_start_after_a_word_or_a_dot() {
        let found: Vec<String> =
            selector_matches(&SELECTOR_CALL, "a.b.c.F(x); d.F(y); s.opts.F(z)", |_| true)
                .iter()
                .map(|captures| format!("{} {}", &captures[1], &captures[2]))
                .collect();
        assert_eq!(found, ["d F", "s.opts F"]);
        let text = "x.F == y; x.F = y";
        let assigned: Vec<usize> = selector_matches(&SELECTOR_ASSIGN, text, |captures| {
            no_equals_at(text, captures.get(0).unwrap().end())
        })
        .iter()
        .map(|captures| captures.get(0).unwrap().start())
        .collect();
        assert_eq!(assigned, [10]);
    }

    #[test]
    fn asserted_types_skip_a_type_switch() {
        assert_eq!(asserted_type("y.(*T)"), Some("*T"));
        assert_eq!(asserted_type("y.( pkg.T )"), Some("pkg.T"));
        assert_eq!(asserted_type("y.(type)"), None);
        // A space after `(` gives the type switch's `type` back as a name,
        // as upstream's backtracking does.
        assert_eq!(asserted_type("y.( type)"), Some(" type"));
        assert_eq!(asserted_type("f(y.(T), z)"), None);
    }

    #[test]
    fn literal_keys_and_declaration_names() {
        assert_eq!(literal_key("RefreshF: d.refresh"), Some(("RefreshF", 10)));
        assert_eq!(literal_key("x := 1"), None);
        assert_eq!(literal_key("d.refresh"), None);
        assert_eq!(names_from("r, err := newRefresher(x)"), Some((", err", 9)));
        assert_eq!(names_from("x = f()"), Some(("", 3)));
        assert_eq!(names_from("x == f()"), None);
    }

    #[test]
    fn pkg_dir_is_dot_at_the_root() {
        assert_eq!(pkg_dir("main.go"), ".");
        assert_eq!(pkg_dir("discovery/aws/ec2.go"), "discovery/aws");
    }

    /// The func field names the reads below are restricted to.
    fn field_names() -> HashSet<String> {
        ["F", "Fo", "refreshf", "RefreshF"].map(String::from).into()
    }

    /// Texts of `pieces` strung together by a fixed generator, so the reads
    /// meet selectors, literals, comments and strings next to one another.
    fn generated_texts(pieces: &[&str], count: usize) -> Vec<String> {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..count)
            .map(|_| {
                let len = (next() % 24) as usize;
                (0..len)
                    .map(|_| pieces[(next() % pieces.len() as u64) as usize])
                    .collect()
            })
            .collect()
    }

    /// The Go fixtures of the synthesis corpus.
    fn fixture_texts() -> Vec<String> {
        [
            include_str!("../../../codegraph-bench/fixtures/synthesis/go/ec2/ec2.go"),
            include_str!("../../../codegraph-bench/fixtures/synthesis/go/refresh/refresh.go"),
            include_str!("../../../codegraph-bench/fixtures/synthesis/go/gin/gin.go"),
            include_str!("../../../codegraph-bench/fixtures/synthesis/go/gin/app.go"),
            include_str!("../../../codegraph-bench/fixtures/synthesis/go/rpc/server.go"),
            include_str!("../../../codegraph-bench/fixtures/synthesis/go/store/store.go"),
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// Selector pieces: receivers, dots and spaces (JavaScript's among
    /// them), the field names and others, calls, assignments and `==`.
    const SELECTOR_PIECES: &[&str] = &[
        "x",
        "ab",
        "s.opts",
        "a.b.c",
        "9",
        "é",
        "_",
        ".",
        ". ",
        "\u{a0}",
        "\u{3000}",
        " ",
        "\t",
        "\n",
        "F",
        "Fo",
        "Foo",
        "G",
        "refreshf",
        "RefreshF",
        "(",
        ")",
        "=",
        "==",
        " = ",
        ":=",
        ",",
        ";",
        "{",
        "}",
        "\"",
        "//",
        "*/",
        "x.F(",
        "d.F = v",
        "s.opts.F(",
        "x.F == y",
        "f().F(",
        "a.b.c.F(",
        "9x.F(",
        "é.F(",
        " .F(",
        "x.\u{a0}F\t(",
        "x.Fo =",
        "x.\nF(",
    ];

    /// Every `.F(` / `.F =` use of every name in `text`, by upstream's
    /// `SELECTOR_USE_RE` search: its start, its end and its name.
    fn uses_of_every_name(text: &str) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        let mut from = 0;
        while from <= text.len() {
            let Some(captures) = SELECTOR_USE.captures_at(text, from) else {
                break;
            };
            let whole = captures.get(0).unwrap();
            if &captures[2] == "=" && !no_equals_at(text, whole.end()) {
                from = next_start(text, whole.start());
                continue;
            }
            from = whole.end();
            out.push((whole.start(), whole.end(), captures[1].to_string()));
        }
        out
    }

    /// The uses [`FieldUses`] finds, whichever way it reads them, are the
    /// uses of `names` a search over every name finds; returns how many.
    fn assert_uses_of(uses: &FieldUses<'_>, names: &HashSet<String>) -> usize {
        let mut matched = 0;
        for text in generated_texts(SELECTOR_PIECES, 6000)
            .into_iter()
            .chain(fixture_texts())
        {
            let expected: Vec<(usize, usize)> = uses_of_every_name(&text)
                .into_iter()
                .filter(|(_, _, name)| names.contains(name))
                .map(|(start, end, _)| (start, end))
                .collect();
            assert_eq!(uses.all(&text), expected, "{text:?}");
            assert_eq!(
                uses.next(&text, 0).is_some(),
                !expected.is_empty(),
                "{text:?}"
            );
            matched += expected.len();
        }
        matched
    }

    /// Restricting the use pattern to the func fields' names finds the uses
    /// of those names a search over every name finds (W7-15d's cost
    /// revision).
    #[test]
    fn field_uses_are_the_uses_a_search_over_every_name_finds() {
        let names = field_names();
        let uses = FieldUses::new(&names);
        assert!(matches!(uses, FieldUses::Named(_)));
        let matched = assert_uses_of(&uses, &names);
        assert!(matched > 1000, "{matched}");
    }

    /// Past the bound on the names the pattern spells out, the uses are read
    /// over every name and their names checked: the same uses, and no
    /// pattern the regex crate refuses to compile (16384 names pass its
    /// size limit).
    #[test]
    fn field_uses_past_the_pattern_bound_check_every_name() {
        let mut many = field_names();
        many.extend((0..MAX_PATTERN_NAMES).map(|i| format!("Field{i}")));
        let uses = FieldUses::new(&many);
        assert!(matches!(uses, FieldUses::AnyName(_)));
        let matched = assert_uses_of(&uses, &many);
        assert!(matched > 1000, "{matched}");

        let mut long = field_names();
        long.insert("G".repeat(MAX_PATTERN_NAME_BYTES));
        assert!(matches!(FieldUses::new(&long), FieldUses::AnyName(_)));

        let mut at_the_bound = field_names();
        at_the_bound
            .extend((0..MAX_PATTERN_NAMES - at_the_bound.len()).map(|i| format!("Field{i}")));
        assert!(matches!(FieldUses::new(&at_the_bound), FieldUses::Named(_)));

        let huge: HashSet<String> = (0..16384).map(|i| format!("RefreshField{i}")).collect();
        assert!(matches!(FieldUses::new(&huge), FieldUses::AnyName(_)));
    }

    /// The selector matches read at the uses are the whole-text search's
    /// matches whose field is a func field, in the same order.
    #[test]
    fn field_selectors_are_the_whole_text_matches_of_the_func_fields() {
        let names = field_names();
        let uses = FieldUses::new(&names);
        let rows = |found: &[Captures<'_>]| -> Vec<(usize, usize, String, String)> {
            found
                .iter()
                .map(|captures| {
                    let whole = captures.get(0).unwrap();
                    (
                        whole.start(),
                        whole.end(),
                        captures[1].to_string(),
                        captures[2].to_string(),
                    )
                })
                .collect()
        };
        let (mut calls_seen, mut assignments_seen) = (0, 0);
        for text in generated_texts(SELECTOR_PIECES, 6000)
            .into_iter()
            .chain(fixture_texts())
        {
            let calls: Vec<Captures<'_>> = selector_matches(&SELECTOR_CALL, &text, |_| true)
                .into_iter()
                .filter(|captures| names.contains(&captures[2]))
                .collect();
            let assignments: Vec<Captures<'_>> =
                selector_matches(&SELECTOR_ASSIGN, &text, |captures| {
                    no_equals_at(&text, captures.get(0).unwrap().end())
                })
                .into_iter()
                .filter(|captures| names.contains(&captures[2]))
                .collect();
            let (found_calls, found_assignments) = field_selectors(&text, &uses.all(&text));
            assert_eq!(rows(&found_calls), rows(&calls), "{text:?}");
            assert_eq!(rows(&found_assignments), rows(&assignments), "{text:?}");
            calls_seen += calls.len();
            assignments_seen += assignments.len();
        }
        assert!(
            calls_seen > 500 && assignments_seen > 100,
            "{calls_seen} {assignments_seen}"
        );
    }

    /// Literal pieces: a struct's name bare, qualified, instantiated, with a
    /// longer name, after a dot or a letter, and in an unclosed bracket.
    const LITERAL_PIECES: &[&str] = &[
        "Options{",
        "refresh.Options{",
        "Options[int]{",
        "pkg . Options {",
        "xOptions{",
        "Options.Options{",
        "Foo[a[b]{",
        "Foo[Options]{",
        "&Options{",
        "é.Options{",
        "Opt{",
        "OptionsX{",
        "Options [ x ] {",
        "[]Options{",
        "Größe{",
        "p.Größe{",
        "a{",
        "x.a{",
        "Options\u{a0}{",
        "Options\n{",
        " ",
        ".",
        "{",
        "}",
        "[",
        "]",
        "x",
        "(",
        "_",
        "9",
    ];

    /// Upstream's literal pattern, compiled with the name in it.
    fn literal_pattern(name: &str) -> Regex {
        compile(&format!(
            r"(?:({IDENT})\s*\.\s*)?{}\s*(?:\[[^\]{{}}]*\])?\s*\{{",
            regex::escape(name)
        ))
    }

    /// One pattern tried at each start finds what the pattern compiled with
    /// the name finds; a name that is not an ASCII identifier keeps it
    /// (W7-15d's cost revision).
    #[test]
    fn literal_types_are_what_the_pattern_with_the_name_finds() {
        let mut matched = 0;
        let lines = generated_texts(LITERAL_PIECES, 3000);
        for name in ["Options", "Opt", "a", "Größe", "_"] {
            let pattern = literal_pattern(name);
            for line in &lines {
                let expected: Vec<(usize, usize, Option<&str>)> =
                    selector_matches(&pattern, line, |_| true)
                        .iter()
                        .map(|found| {
                            let whole = found.get(0).unwrap();
                            (whole.start(), whole.end(), found.get(1).map(|q| q.as_str()))
                        })
                        .collect();
                assert_eq!(literal_types(line, name), expected, "{name} {line:?}");
                matched += expected.len();
            }
        }
        assert!(matched > 1000, "{matched}");
    }

    /// Reading a file's scope index from its masked text gives the index
    /// read from its raw text (W7-15d's cost revision).
    #[test]
    fn a_scope_index_from_the_masked_text_is_the_one_read_from_the_raw_text() {
        let edges = [
            "",
            "\n",
            "\n\n",
            "x := 1\n",
            "x := 1",
            "func f(a int) {\n\tb := a // c\n}\n",
            "func f(a int) {\n\tb := a // c",
            "s := \"abc\\\n",
            "s := \"abc\\",
            "/* open\n",
            "/* open",
            "/*",
            "/*\n",
            "`raw\n",
            "x := `raw",
            "r := 'x\n",
            "a := 1\r\nb := 2\r\n",
            "a := 1\rb := 2\n",
            "é := 1\n",
            "s := \"\\é\"\n",
        ];
        let pieces = [
            "x", " := ", "f(", ")", "{", "}", "\n", "\r\n", "\"", "'", "`", "\\", "//", "/*", "*/",
            "é", "var ", "func ", "a int", ", ", "\t",
        ];
        for raw in edges
            .iter()
            .map(|text| text.to_string())
            .chain(generated_texts(&pieces, 3000))
            .chain(fixture_texts())
        {
            let masked = Masked::new(&raw);
            assert!(
                scope_index_of(&raw, &masked).same_as(&GoScopeIndex::read(&raw)),
                "{raw:?}"
            );
        }
    }

    fn test_node() -> Node {
        Node {
            id: "struct:x".into(),
            kind: NodeKind::Struct,
            name: "S".into(),
            qualified_name: "S".into(),
            file_path: "s.go".into(),
            language: Language::Go,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }
}
