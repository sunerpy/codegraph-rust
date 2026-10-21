//! Calls written in a Vue single-file component's `<template>` (upstream
//! #2357 `vue-template-calls.ts`, issue #2340).
//!
//! A template runs code as surely as the `<script>` does: `{{ formatDate(at)
//! }}`, `:to="localePath(link)"`, `v-if="canEdit(user)"` and
//! `@click="save(item)"` call a function every time the component renders or
//! the event fires. The Vue extractor hands only `<script>` blocks to the
//! TypeScript extractor, so none of these were calls: `callers` on a Nuxt
//! composable used in markup came back empty.
//!
//! This finds them without a template compiler. A tag scanner reads the root
//! `<template>` (quote-aware, so a `>` inside an attribute value never ends a
//! tag) and collects the expressions Vue compiles: `{{ }}` interpolations in
//! text, and the values of directives (`:x` / `v-bind`, `@x` / `v-on`, `v-if`,
//! `v-show`, `v-model`, `v-for`'s source, custom `v-*`). A small JS lexer then
//! names each call's callee the way a script-side call is named (`useBar`,
//! `store.fetchUsers`), skipping strings and comments and reading into
//! template-literal substitutions.
//!
//! What is never a call to project code is dropped: names the template binds
//! itself (`v-for` aliases and slot props for their element's subtree, arrow
//! and function parameters for the rest of their expression), `$`-prefixed
//! instance helpers (`$t`, `$emit`, `$router.push`), the globals Vue lets a
//! template reach (`Math`, `JSON`, `Date`…), keywords, `new X()`, and an
//! object literal's method shorthand. A `v-pre` subtree is not compiled by
//! Vue and is skipped, as is a template in another language (`lang="pug"`).
//! Offsets are bytes of the SFC source.

use std::collections::HashSet;
use std::sync::OnceLock;

use codegraph_core::generated_header::is_js_whitespace;
use regex::Regex;

/// One call in the template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VueTemplateCall {
    /// The callee as a script-side call names it: `useBar`,
    /// `store.fetchUsers`.
    pub name: String,
    /// Byte offset of the callee's first character in the SFC source.
    pub offset: usize,
}

/// Elements that never have children: no closing tag, no scope of their own.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// The globals a Vue template can reach (Vue's `isGloballyAllowed`). Every
/// other name in a template is the component's: a script binding, a prop, an
/// auto-import.
const TEMPLATE_GLOBALS: [&str; 26] = [
    "Infinity",
    "undefined",
    "NaN",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "Math",
    "Number",
    "Date",
    "Array",
    "Object",
    "Boolean",
    "String",
    "RegExp",
    "Map",
    "Set",
    "JSON",
    "Intl",
    "BigInt",
    "console",
    "Error",
];

/// `Symbol` closes upstream's list; kept apart so the array above stays a
/// readable size.
fn is_template_global(name: &str) -> bool {
    name == "Symbol" || TEMPLATE_GLOBALS.contains(&name)
}

/// Words that can stand before `(` without being a call.
const KEYWORDS: [&str; 40] = [
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "return",
    "throw",
    "try",
    "catch",
    "finally",
    "function",
    "class",
    "new",
    "delete",
    "typeof",
    "void",
    "instanceof",
    "in",
    "of",
    "var",
    "let",
    "const",
    "this",
    "super",
    "import",
    "export",
    "await",
    "async",
    "yield",
    "with",
    "debugger",
    "true",
    "false",
    "null",
    "undefined",
];

/// Keywords that end an operand, as an identifier does (`this`, `true`).
const VALUE_KEYWORDS: [&str; 6] = ["this", "super", "true", "false", "null", "undefined"];

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn is_ident_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// The byte length of the JavaScript whitespace character at `at`, 0 when
/// there is none.
fn ws_len(s: &str, at: usize) -> usize {
    s.get(at..)
        .and_then(|rest| rest.chars().next())
        .filter(|&c| is_js_whitespace(c))
        .map_or(0, char::len_utf8)
}

fn byte(s: &str, at: usize) -> u8 {
    s.as_bytes().get(at).copied().unwrap_or(0)
}

/// The identifiers in `text`, `[A-Za-z_$][\w$]*`.
fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    static IDENTIFIERS: OnceLock<Regex> = OnceLock::new();
    IDENTIFIERS
        .get_or_init(|| Regex::new(r"[A-Za-z_$][0-9A-Za-z_$]*").expect("identifiers"))
        .find_iter(text)
        .map(|found| found.as_str())
}

/// Every call written in the root `<template>` of a Vue SFC, in source order.
pub(crate) fn vue_template_calls(source: &str) -> Vec<VueTemplateCall> {
    let masked = mask_blocks_and_comments(source);
    let Some((start, end)) = root_template(&masked) else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    scan_markup(&masked, start, end, &mut calls);
    calls.sort_by_key(|call| call.offset);
    calls
}

/// `source` with its `<script>` / `<style>` blocks and HTML comments blanked,
/// every byte but a line break a space, so every offset survives.
fn mask_blocks_and_comments(source: &str) -> String {
    static OPEN: OnceLock<Regex> = OnceLock::new();
    static SCRIPT_CLOSE: OnceLock<Regex> = OnceLock::new();
    static STYLE_CLOSE: OnceLock<Regex> = OnceLock::new();
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    let open = OPEN.get_or_init(|| Regex::new(r"<(script|style)(\s[^>]*)?>").expect("block open"));
    let script_close =
        SCRIPT_CLOSE.get_or_init(|| Regex::new(r"</script\s*>").expect("script close"));
    let style_close = STYLE_CLOSE.get_or_init(|| Regex::new(r"</style\s*>").expect("style close"));
    let mut bytes = source.as_bytes().to_vec();
    let blank = |from: usize, to: usize, bytes: &mut Vec<u8>| {
        for byte in &mut bytes[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    // One pass, leftmost first: a block runs to the first closing tag of its
    // own kind, as `<(script|style)…>…</\1\s*>` reads it.
    let mut at = 0;
    while let Some(found) = open.captures_at(source, at) {
        let whole = found.get(0).expect("match");
        let close = if &found[1] == "script" {
            script_close
        } else {
            style_close
        };
        match close.find_at(source, whole.end()) {
            Some(end) => {
                blank(whole.start(), end.end(), &mut bytes);
                at = end.end();
            }
            None => at = whole.start() + 1,
        }
    }
    let masked = String::from_utf8(bytes).expect("blanking keeps UTF-8");
    let mut bytes = masked.as_bytes().to_vec();
    for comment in COMMENT
        .get_or_init(|| Regex::new(r"(?s)<!--.*?-->").expect("html comment"))
        .find_iter(&masked)
    {
        blank(comment.start(), comment.end(), &mut bytes);
    }
    String::from_utf8(bytes).expect("blanking keeps UTF-8")
}

/// The content range of the SFC's root `<template>`; `None` for none or a
/// non-HTML one.
fn root_template(s: &str) -> Option<(usize, usize)> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static LANG: OnceLock<Regex> = OnceLock::new();
    let tag = TAG.get_or_init(|| {
        Regex::new(r#"<(/?)template((?:[\s/](?:[^>"']|"[^"]*"|'[^']*')*)?)>"#)
            .expect("template tag")
    });
    let lang = LANG.get_or_init(|| {
        Regex::new(r#"(?-u:\b)lang\s*=\s*["']?([0-9A-Za-z_-]+)"#).expect("template lang")
    });
    let mut start: Option<usize> = None;
    let mut depth = 0usize;
    for found in tag.captures_iter(s) {
        let whole = found.get(0).expect("match");
        let closing = &found[1] == "/";
        let attrs = found.get(2).map_or("", |attrs| attrs.as_str());
        let self_closing = attrs.trim_end().ends_with('/');
        match start {
            None => {
                if closing || self_closing {
                    continue;
                }
                if let Some(language) = lang.captures(attrs)
                    && !language[1].eq_ignore_ascii_case("html")
                {
                    return None;
                }
                start = Some(whole.end());
                depth = 1;
            }
            Some(open) if closing => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, whole.start()));
                }
            }
            Some(_) if !self_closing => depth += 1,
            Some(_) => {}
        }
    }
    start.map(|open| (open, s.len()))
}

struct Attr {
    name: String,
    /// The value, `None` for a bare attribute, with the offset of its first
    /// character (inside any quotes).
    value: Option<(usize, usize)>,
}

struct Frame {
    tag: String,
    locals: Option<HashSet<String>>,
    pre: bool,
}

/// The markup of `s[start, end)`: element nesting, directive values,
/// interpolations.
fn scan_markup(s: &str, start: usize, end: usize, out: &mut Vec<VueTemplateCall>) {
    static CLOSE_TAG: OnceLock<Regex> = OnceLock::new();
    static FOR_ALIAS: OnceLock<Regex> = OnceLock::new();
    let close_tag =
        CLOSE_TAG.get_or_init(|| Regex::new(r"^</([A-Za-z][^\s/>]*)\s*>").expect("close tag"));
    let for_alias = FOR_ALIAS
        .get_or_init(|| Regex::new(r"(?s)(.*?)\s+(?:in|of)\s+(\S.*)").expect("v-for alias"));
    let mut stack: Vec<Frame> = Vec::new();
    // The next `{{` at or after `i`, searched for again only once `i` passes
    // it.
    let mut next_mustache: Option<usize> = None;
    let mut i = start;
    while i < end {
        if next_mustache.is_none_or(|at| at < i) {
            next_mustache = Some(s[i..].find("{{").map_or(end, |at| (i + at).min(end)));
        }
        let mustache = next_mustache.unwrap_or(end);
        let next_tag = s[i..].find('<').map_or(end, |at| (i + at).min(end));
        if mustache < next_tag {
            // A `{{ }}` interpolation runs to the first `}}`, as Vue's
            // parser reads it.
            let expr_end = s[mustache + 2..]
                .find("}}")
                .map_or(end, |at| (mustache + 2 + at).min(end));
            if !stack.iter().any(|frame| frame.pre) {
                let local = |name: &str| in_scope(&stack, name, None);
                scan_expression(s, mustache + 2, expr_end, &local, out);
            }
            i = expr_end + 2;
            continue;
        }
        if next_tag >= end {
            break;
        }
        if let Some(found) = close_tag.captures(&s[next_tag..end]) {
            let name = &found[1];
            if let Some(at) = stack
                .iter()
                .rposition(|frame| frame.tag == name || frame.tag.eq_ignore_ascii_case(name))
            {
                stack.truncate(at);
            }
            i = next_tag + found[0].len();
            continue;
        }
        if !byte(s, next_tag + 1).is_ascii_alphabetic() {
            i = next_tag + 1; // a `<` in text, not a tag
            continue;
        }
        let (name, attrs, close, self_closing) = parse_open_tag(s, next_tag, end);
        i = close;
        // Names this element binds for itself and its subtree: `v-for`
        // aliases, slot props (`#item="{ item }"`, `v-slot="props"`, Vue 2
        // `slot-scope`).
        let mut own: Option<HashSet<String>> = None;
        let mut pre = false;
        let mut for_source: Option<(usize, usize)> = None;
        for attr in &attrs {
            if attr.name == "v-pre" {
                pre = true;
            }
            let Some((value_start, value_end)) = attr.value else {
                continue;
            };
            let value = &s[value_start..value_end];
            match directive_kind(&attr.name) {
                Some(Directive::For) => {
                    if let Some(found) = for_alias.captures(value) {
                        let names = own.get_or_insert_with(HashSet::new);
                        names.extend(identifiers(&found[1]).map(str::to_string));
                        let source_len = found[2].len();
                        for_source = Some((value_end - source_len, value_end));
                    }
                }
                Some(Directive::Slot) => {
                    let names = own.get_or_insert_with(HashSet::new);
                    names.extend(identifiers(value).map(str::to_string));
                }
                _ => {}
            }
        }
        if !pre && !stack.iter().any(|frame| frame.pre) {
            let local = |name: &str| in_scope(&stack, name, own.as_ref());
            for attr in &attrs {
                let Some((value_start, value_end)) = attr.value else {
                    continue;
                };
                match directive_kind(&attr.name) {
                    Some(Directive::Expression | Directive::On) => {
                        scan_expression(s, value_start, value_end, &local, out);
                    }
                    Some(Directive::For) => {
                        if let Some((from, to)) = for_source {
                            scan_expression(s, from, to, &local, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !self_closing && !VOID_ELEMENTS.contains(&name.to_ascii_lowercase().as_str()) {
            stack.push(Frame {
                tag: name,
                locals: own,
                pre,
            });
        }
    }
}

fn in_scope(stack: &[Frame], name: &str, own: Option<&HashSet<String>>) -> bool {
    own.is_some_and(|names| names.contains(name))
        || stack.iter().any(|frame| {
            frame
                .locals
                .as_ref()
                .is_some_and(|names| names.contains(name))
        })
}

/// What an attribute is to Vue.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Directive {
    For,
    Slot,
    On,
    Expression,
}

fn directive_kind(name: &str) -> Option<Directive> {
    if name == "v-for" {
        return Some(Directive::For);
    }
    if name.starts_with('#')
        || name == "v-slot"
        || name.starts_with("v-slot:")
        || name == "slot-scope"
        || name == "scope"
    {
        return Some(Directive::Slot);
    }
    if name.starts_with('@') || name == "v-on" || name.starts_with("v-on:") {
        return Some(Directive::On);
    }
    // `:x` / `.x` (v-bind and its `.prop` shorthand), `v-bind`, `v-if`,
    // `v-model`, custom `v-*`.
    (name.starts_with(':') || name.starts_with('.') || name.starts_with("v-"))
        .then_some(Directive::Expression)
}

/// An opening tag at `at`: its name, its attributes, where it ends, and
/// whether it closes itself. Quote-aware.
fn parse_open_tag(s: &str, at: usize, end: usize) -> (String, Vec<Attr>, usize, bool) {
    let mut p = at + 1;
    let name_start = p;
    while p < end && !matches!(byte(s, p), b'/' | b'>') && ws_len(s, p) == 0 {
        p += 1;
    }
    let name = s[name_start..p].to_string();
    let mut attrs = Vec::new();
    while p < end {
        while p < end && ws_len(s, p) > 0 {
            p += ws_len(s, p);
        }
        if p >= end {
            break;
        }
        match byte(s, p) {
            b'>' => return (name, attrs, p + 1, false),
            b'/' if byte(s, p + 1) == b'>' => return (name, attrs, p + 2, true),
            _ => {}
        }
        let attr_start = p;
        while p < end
            && !matches!(byte(s, p), b'"' | b'\'' | b'>' | b'/' | b'=')
            && ws_len(s, p) == 0
        {
            p += 1;
        }
        if p == attr_start {
            p += 1; // a stray `/`, quote or `=`
            continue;
        }
        let attr_name = s[attr_start..p].to_string();
        let mut q = p;
        while q < end && ws_len(s, q) > 0 {
            q += ws_len(s, q);
        }
        if byte(s, q) != b'=' {
            attrs.push(Attr {
                name: attr_name,
                value: None,
            });
            continue;
        }
        q += 1;
        while q < end && ws_len(s, q) > 0 {
            q += ws_len(s, q);
        }
        let quote = byte(s, q);
        if quote == b'"' || quote == b'\'' {
            let value_end = s[q + 1..]
                .find(quote as char)
                .map_or(end, |at| (q + 1 + at).min(end));
            attrs.push(Attr {
                name: attr_name,
                value: Some((q + 1, value_end)),
            });
            p = value_end + 1;
        } else {
            let value_start = q;
            while q < end && byte(s, q) != b'>' && ws_len(s, q) == 0 {
                q += 1;
            }
            attrs.push(Attr {
                name: attr_name,
                value: Some((value_start, q)),
            });
            p = q;
        }
    }
    (name, attrs, end, false)
}

/// The lexer's view of the previous token: what may follow it, and what a
/// `/` means after it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    Start,
    Operand,
    Dot,
    New,
    Decl,
}

struct Paren {
    open: usize,
    /// Index in `found` of the call this paren opens.
    call: Option<usize>,
    /// The parameter list of a `function`.
    params: bool,
}

/// The calls in one template expression `s[start, end)`. `outer_local` is
/// the template scope (aliases and slot props in force); arrow and function
/// parameters bound inside the expression join it as they are met.
fn scan_expression(
    s: &str,
    start: usize,
    end: usize,
    outer_local: &dyn Fn(&str) -> bool,
    out: &mut Vec<VueTemplateCall>,
) {
    let params: std::cell::RefCell<HashSet<String>> = std::cell::RefCell::new(HashSet::new());
    let is_local = |name: &str| params.borrow().contains(name) || outer_local(name);
    let mut found: Vec<(VueTemplateCall, bool)> = Vec::new();
    let mut parens: Vec<Paren> = Vec::new();
    let mut last_group: Option<(usize, usize)> = None;
    let mut last_ident = String::new();
    let mut function_pending = false;
    let mut prev = Prev::Start;

    let skip_ws = |mut k: usize| {
        while k < end && ws_len(s, k) > 0 {
            k += ws_len(s, k);
        }
        k
    };
    let word_end = |mut k: usize| {
        while k < end && is_ident_char(byte(s, k)) {
            k += 1;
        }
        k
    };
    let add_params = |from: usize, to: usize| {
        params
            .borrow_mut()
            .extend(identifiers(&s[from..to]).map(str::to_string));
    };

    let mut i = start;
    while i < end {
        let skipped = ws_len(s, i);
        if skipped > 0 {
            i += skipped;
            continue;
        }
        let c = byte(s, i);
        if c == b'"' || c == b'\'' {
            i = skip_string(s, i, end);
            prev = Prev::Operand;
            continue;
        }
        if c == b'`' {
            i = skip_template_literal(s, i, end, &mut |from, to| {
                scan_expression(s, from, to, &is_local, out);
            });
            prev = Prev::Operand;
            continue;
        }
        if c == b'/' {
            if byte(s, i + 1) == b'/' {
                i = s[i..].find('\n').map_or(end, |nl| (i + nl).min(end));
                continue;
            }
            if byte(s, i + 1) == b'*' {
                i = s[i + 2..]
                    .find("*/")
                    .map_or(end, |close| (i + 2 + close + 2).min(end));
                continue;
            }
            if prev != Prev::Operand {
                i = skip_regex(s, i, end);
                prev = Prev::Operand;
                continue;
            }
            i += 1;
            prev = Prev::Start;
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && byte(s, i + 1).is_ascii_digit()) {
            i += 1;
            while i < end && (is_ident_char(byte(s, i)) || byte(s, i) == b'.') && byte(s, i) != b'$'
            {
                i += 1;
            }
            prev = Prev::Operand;
            continue;
        }
        if is_ident_start(c) {
            let w_end = word_end(i);
            let word = &s[i..w_end];
            if prev == Prev::Dot {
                // A member tail of something that is not a plain path:
                // `a().b`, `x[0].y`.
                i = w_end;
                prev = Prev::Operand;
                last_ident.clear();
                continue;
            }
            if KEYWORDS.contains(&word) {
                i = w_end;
                last_ident.clear();
                prev = if word == "new" {
                    Prev::New
                } else if word == "function" || word == "class" {
                    if word == "function" {
                        function_pending = true;
                    }
                    Prev::Decl
                } else if VALUE_KEYWORDS.contains(&word) {
                    Prev::Operand
                } else {
                    Prev::Start
                };
                continue;
            }
            if prev == Prev::Decl {
                // A function or class name, not a call.
                i = w_end;
                prev = Prev::Operand;
                continue;
            }
            // The member path rooted here: `a`, `a.b.c`, `a?.b`, TS `a!.b`.
            let mut segments = vec![word.to_string()];
            let mut p = w_end;
            loop {
                let mut q = skip_ws(p);
                if byte(s, q) == b'!' && byte(s, q + 1) == b'.' {
                    q += 1;
                }
                let r = if byte(s, q) == b'.' && byte(s, q + 1) != b'.' {
                    skip_ws(q + 1)
                } else if byte(s, q) == b'?'
                    && byte(s, q + 1) == b'.'
                    && !byte(s, q + 2).is_ascii_digit()
                {
                    skip_ws(q + 2)
                } else {
                    break;
                };
                if !is_ident_start(byte(s, r)) {
                    break;
                }
                let segment_end = word_end(r);
                segments.push(s[r..segment_end].to_string());
                p = segment_end;
            }
            // Called? `path(`, or optionally `path?.(`.
            let mut q = skip_ws(p);
            if byte(s, q) == b'?' && byte(s, q + 1) == b'.' && byte(s, skip_ws(q + 2)) == b'(' {
                q = skip_ws(q + 2);
            }
            let root = segments[0].as_str();
            if byte(s, q) == b'(' {
                let mut call = None;
                if prev != Prev::New
                    && !root.starts_with('$')
                    && !is_template_global(root)
                    && !is_local(root)
                {
                    found.push((
                        VueTemplateCall {
                            name: segments.join("."),
                            offset: i,
                        },
                        false,
                    ));
                    call = Some(found.len() - 1);
                }
                parens.push(Paren {
                    open: q,
                    call,
                    params: false,
                });
                i = q + 1;
                prev = Prev::Start;
                last_ident.clear();
                continue;
            }
            i = p;
            prev = Prev::Operand;
            last_ident = if segments.len() == 1 {
                word.to_string()
            } else {
                String::new()
            };
            continue;
        }
        // Punctuation.
        if c == b'(' {
            parens.push(Paren {
                open: i,
                call: None,
                params: function_pending,
            });
            function_pending = false;
            i += 1;
            prev = Prev::Start;
            continue;
        }
        if c == b')' {
            if let Some(paren) = parens.pop() {
                last_group = Some((paren.open, i));
                if paren.params {
                    add_params(paren.open + 1, i);
                }
                // `{ fmt(v) { … } }`: an object literal's method, not a call.
                if let Some(call) = paren.call
                    && byte(s, skip_ws(i + 1)) == b'{'
                {
                    found[call].1 = true;
                }
            }
            i += 1;
            prev = Prev::Operand;
            last_ident.clear();
            continue;
        }
        if c == b'=' && byte(s, i + 1) == b'>' {
            // Arrow parameters: `(a, { b }) =>` or `a =>`.
            if !last_ident.is_empty() {
                params.borrow_mut().insert(last_ident.clone());
            } else if let Some((open, close)) = last_group
                && previous_non_ws(s, i, start) == Some(close)
            {
                add_params(open + 1, close);
            }
            i += 2;
            prev = Prev::Start;
            last_ident.clear();
            continue;
        }
        if c == b'.' {
            if byte(s, i + 1) == b'.' && byte(s, i + 2) == b'.' {
                i += 3;
                prev = Prev::Start;
            } else {
                i += 1;
                prev = Prev::Dot;
            }
            last_ident.clear();
            continue;
        }
        if c == b'?' && byte(s, i + 1) == b'.' && !byte(s, i + 2).is_ascii_digit() {
            i += 2;
            prev = Prev::Dot;
            last_ident.clear();
            continue;
        }
        i += s[i..].chars().next().map_or(1, char::len_utf8);
        prev = if c == b']' || c == b'}' {
            Prev::Operand
        } else {
            Prev::Start
        };
        last_ident.clear();
    }

    out.extend(
        found
            .into_iter()
            .filter(|(_, dropped)| !dropped)
            .map(|(call, _)| call),
    );
}

/// Index of the last non-whitespace byte before `i`, not before `floor`.
fn previous_non_ws(s: &str, i: usize, floor: usize) -> Option<usize> {
    let before = s.get(floor..i)?;
    let trimmed = before.trim_end_matches(is_js_whitespace);
    trimmed.char_indices().next_back().map(|(at, _)| floor + at)
}

/// Past a `'…'` / `"…"` string starting at `i`.
fn skip_string(s: &str, i: usize, end: usize) -> usize {
    let quote = byte(s, i);
    let mut k = i + 1;
    while k < end {
        let c = byte(s, k);
        if c == b'\\' {
            k += 2;
        } else if c == quote {
            return k + 1;
        } else {
            k += 1;
        }
    }
    end
}

/// Past a template literal starting at `i`, reporting each `${…}`
/// substitution's range.
fn skip_template_literal(
    s: &str,
    i: usize,
    end: usize,
    on_substitution: &mut dyn FnMut(usize, usize),
) -> usize {
    let mut k = i + 1;
    while k < end {
        let c = byte(s, k);
        if c == b'\\' {
            k += 2;
        } else if c == b'`' {
            return k + 1;
        } else if c == b'$' && byte(s, k + 1) == b'{' {
            let close = matching_brace(s, k + 2, end);
            on_substitution(k + 2, close);
            k = close + 1;
        } else {
            k += 1;
        }
    }
    end
}

/// The `}` closing a brace opened just before `i` (strings and template
/// literals skipped).
fn matching_brace(s: &str, i: usize, end: usize) -> usize {
    let mut depth = 0usize;
    let mut k = i;
    while k < end {
        match byte(s, k) {
            b'"' | b'\'' => k = skip_string(s, k, end),
            b'`' => k = skip_template_literal(s, k, end, &mut |_, _| {}),
            b'{' => {
                depth += 1;
                k += 1;
            }
            b'}' => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
                k += 1;
            }
            _ => k += 1,
        }
    }
    end
}

/// Past a regex literal starting at `i` (a `/` where an operand is
/// expected); `i + 1` when it is none after all.
fn skip_regex(s: &str, i: usize, end: usize) -> usize {
    let mut k = i + 1;
    let mut in_class = false;
    while k < end {
        let c = byte(s, k);
        if c == b'\\' {
            k += 2;
            continue;
        }
        if c == b'\n' {
            return i + 1;
        }
        if c == b'[' {
            in_class = true;
        } else if c == b']' {
            in_class = false;
        } else if c == b'/' && !in_class {
            k += 1;
            while k < end && byte(s, k).is_ascii_alphabetic() {
                k += 1;
            }
            return k;
        }
        k += 1;
    }
    i + 1
}

#[cfg(test)]
mod tests {
    use super::vue_template_calls;

    fn names(source: &str) -> Vec<String> {
        vue_template_calls(source)
            .into_iter()
            .map(|call| call.name)
            .collect()
    }

    #[test]
    fn finds_calls_in_interpolations_bindings_directives_and_handlers() {
        let source = "<template>
  <NuxtLink :to=\"useBar(link.location)\" v-if=\"canShow(link)\">{{ useBar(link) }}</NuxtLink>
  <li v-for=\"row in rowsFor(user)\" v-show=\"visible()\" v-my-dir=\"setup(1)\">{{ label(row) }}</li>
  <button @click=\"save(item)\" v-on:keyup.enter=\"submit()\" @input=\"onInput\">go</button>
  <Comp :style=\"{ color: tint() }\" :title=\"`#${index(n)}`\" @change=\"(v) => update(v)\" />
</template>
";
        let calls = vue_template_calls(source);
        assert_eq!(
            calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            [
                "useBar", "canShow", "useBar", "rowsFor", "visible", "setup", "label", "save",
                "submit", "tint", "index", "update"
            ]
        );
        // Offsets point at the callee in the SFC source.
        for call in &calls {
            assert_eq!(
                &source[call.offset..call.offset + call.name.len()],
                call.name
            );
        }
    }

    #[test]
    fn names_a_member_call_by_its_path() {
        assert_eq!(
            names(
                "<template>
  <p @click=\"store.fetchUsers()\" :a=\"user?.profile?.load()\" :b=\"fn?.(1)\">{{ props.format(v) }}</p>
  <p :c=\"list[0].go()\" :d=\"make().then(x => x.done())\">{{ (a as B).run() }}</p>
</template>"
            ),
            ["store.fetchUsers", "user.profile.load", "fn", "props.format", "make"]
        );
    }

    #[test]
    fn skips_names_the_template_binds() {
        assert_eq!(
            names(
                "<template>
  <ul>
    <li v-for=\"(item, i) in items\" :key=\"item.id\" @click=\"item.select(i)\">
      {{ item.label() }} {{ format(item) }}
      <Row #default=\"{ row, toggle }\">{{ toggle(row) }} {{ rowLabel(row) }}</Row>
    </li>
  </ul>
  <Comp :cb=\"function (x) { return x.go() }\" :keys=\"(k) => k.trim()\" @done=\"cb => cb()\" />
  <p>{{ toggle() }} {{ item() }}</p>
</template>"
            ),
            ["format", "rowLabel", "toggle", "item"]
        );
    }

    #[test]
    fn skips_what_is_never_project_code() {
        assert_eq!(
            names(
                "<template>
  <p :title=\"$t('x')\" :class=\"'a(b)'\">{{ Math.max(a(), 1) }} {{ JSON.stringify(c) }} {{ $route.query.x }}</p>
  <p v-if=\"typeof x === 'function' && /f(o)/.test(s)\">{{ new Date(stamp()).getTime() }} {{ \"no()\" }}</p>
  <Chart :options=\"{ formatter(v) { return fmt(v) } }\" @click=\"$emit('close')\" />
</template>"
            ),
            ["a", "stamp", "fmt"]
        );
    }

    #[test]
    fn reads_a_crlf_file_the_same_way() {
        let lf = "<template>\n  <p\n    :title=\"label(\n      item)\"\n  >{{ useBar(x) }}</p>\n</template>\n";
        let crlf = lf.replace('\n', "\r\n");
        assert_eq!(names(&crlf), ["label", "useBar"]);
        for call in vue_template_calls(&crlf) {
            assert_eq!(&crlf[call.offset..call.offset + call.name.len()], call.name);
        }
    }

    #[test]
    fn reads_only_the_root_template_and_only_what_vue_compiles() {
        assert_eq!(
            names(
                "<template>
  <!-- {{ commented() }} -->
  <div v-pre>{{ raw() }} <span :x=\"raw()\"></span></div>
  <template v-if=\"cond()\"><span>{{ inner() }}</span></template>
  <p data-x=\"plain(1)\">{{ after() }}</p>
</template>
<script setup>
const x = scriptCall()
</script>
<i18n>{ \"en\": { \"a\": \"{{ notTemplate() }}\" } }</i18n>"
            ),
            ["cond", "inner", "after"]
        );
        assert!(names("<template lang=\"pug\">\ndiv {{ pug() }}\n</template>").is_empty());
        assert!(names("<script setup>\nconst a = b()\n</script>").is_empty());
    }
}
