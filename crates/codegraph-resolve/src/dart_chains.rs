//! A later link of a Dart call chain (upstream #2376, #2383).
//!
//! The extractor keeps one receiver level, so a later link of a chain arrives
//! by its bare name (`events.map(mapper).transform(…)` → `transform`) or, past
//! a member chain, by its last segment (`FutureProvider.autoDispose.family(…)`
//! → `autoDispose.family`, #1496). Such a link calls a member of what the
//! chain before it evaluates to, typed from evidence alone: the chain's head (a
//! type, a constructor call, a string, `this` / `super`, an import prefix, a
//! local, parameter, field, getter or constant with a declared type) and each
//! link's declared type (a method's or getter's return type with the method's
//! own type parameters substituted, a field's or abstract getter's type, what
//! a static constant is initialized with). A lookup nothing declared answers
//! for hands back its one type argument (`context.read<LoginCubit>()`), which
//! is also how a chain on a generic lookup (`BlocProvider.of<T>(…)`, #2383)
//! is typed. Without evidence it links nothing — never a member found by its
//! name.
//!
//! The chain is read from the file's code with comments and string contents
//! blanked, backwards from the link. KEEP-RUST: the extractor places a chain's
//! references where the chain starts, so the link is looked for after that
//! position, written `.m(` (or `.s.m(`) as the extractor recorded the call,
//! with the chain read back from it starting exactly there. Several such
//! links in one chain (`x.m().m()`) carry the same reference, which links only
//! what all of them reach.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::dart_members::{dart_lineage, dart_member_of, is_dart_getter};
use crate::dart_scope::{CHAIN_LINES, in_order, is_dart_identifier, is_dart_type_kind};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Lookups that hand back an instance of the one type they are given:
/// provider's and flutter_bloc's `context.read<T>()` / `watch<T>()` /
/// `BlocProvider.of<T>(context)`, get_it's `get<T>()`, GetX's `find<T>()`,
/// and Flutter's `findAncestorStateOfType<T>()` family.
const TYPE_ARGUMENT_LOOKUPS: &[&str] = &[
    "read",
    "watch",
    "get",
    "find",
    "of",
    "maybeOf",
    "call",
    "dependOnInheritedWidgetOfExactType",
    "getInheritedWidgetOfExactType",
    "findAncestorStateOfType",
    "findAncestorWidgetOfExactType",
    "findRootAncestorStateOfType",
];

/// Words that can stand before a Dart name without being its declared type.
const NON_TYPE_WORDS: &[&str] = &[
    "final",
    "var",
    "late",
    "const",
    "required",
    "covariant",
    "static",
    "external",
    "return",
    "await",
    "yield",
    "throw",
    "new",
    "in",
    "is",
    "as",
    "case",
    "else",
    "if",
    "for",
    "while",
    "do",
    "switch",
    "assert",
    "this",
    "super",
    "get",
    "set",
    "operator",
    "async",
    "sync",
    "show",
    "hide",
    "typedef",
    "extends",
    "with",
    "implements",
    "on",
];

/// How deep a chain's typing goes through the chains it reads its parts from.
const MAX_DEPTH: u32 = 3;

/// A Dart file's code with comments and string contents blanked, byte
/// offsets kept, and where each line starts (upstream `DartCode`).
#[derive(Debug)]
pub struct DartCode {
    pub text: String,
    line_starts: Vec<usize>,
    /// The calls written after a `.` that the extractor records by their
    /// member alone or past a segment, by where the chain each is read back
    /// from starts ([`link_index`]).
    links: OnceLock<HashMap<usize, Vec<LinkAt>>>,
    /// Where each name asked for is written other than as a member
    /// ([`name_uses`]), read once per name.
    uses: Mutex<HashMap<String, Arc<Vec<usize>>>>,
}

impl DartCode {
    pub fn of(source: &str) -> Self {
        let text = blank_dart_code(source);
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(at, _)| at + 1),
        );
        Self {
            text,
            line_starts,
            links: OnceLock::new(),
            uses: Mutex::new(HashMap::new()),
        }
    }

    /// Where `name` is written in the code other than as a member, in order
    /// (upstream `dartNameUses`).
    pub(crate) fn name_uses(&self, name: &str) -> Arc<Vec<usize>> {
        if let Some(hit) = self
            .uses
            .lock()
            .ok()
            .and_then(|memo| memo.get(name).cloned())
        {
            return hit;
        }
        let uses = Arc::new(name_uses(&self.text, name));
        if let Ok(mut memo) = self.uses.lock() {
            memo.insert(name.to_string(), Arc::clone(&uses));
        }
        uses
    }

    /// The calls a chain starting at `position` links ([`link_index`]), in
    /// the order they are written.
    fn links_at(&self, position: usize) -> &[LinkAt] {
        self.links
            .get_or_init(|| link_index(&self.text))
            .get(&position)
            .map_or(&[], Vec::as_slice)
    }

    /// The byte offset 1-based `line` starts at.
    pub fn line_start(&self, line: i64) -> Option<usize> {
        let index = usize::try_from(line).ok()?.checked_sub(1)?;
        self.line_starts.get(index).copied()
    }

    /// The text of 1-based `line`, without its newline.
    fn line_text(&self, line: i64) -> Option<&str> {
        let start = self.line_start(line)?;
        let end = self
            .line_start(line + 1)
            .map_or(self.text.len(), |next| next - 1);
        self.text.get(start..end)
    }

    /// The byte offset of `column` on 1-based `line`.
    fn offset(&self, line: i64, column: i64) -> Option<usize> {
        let start = self.line_start(line)?;
        let at = start + usize::try_from(column).ok()?;
        (at <= self.text.len()).then_some(at)
    }
}

/// The byte length of the UTF-8 sequence at `at` (1 past the end).
fn char_len(src: &[u8], at: usize) -> usize {
    match src.get(at) {
        Some(byte) if *byte >= 0xF0 => 4,
        Some(byte) if *byte >= 0xE0 => 3,
        Some(byte) if *byte >= 0xC0 => 2,
        _ => 1,
    }
}

/// A byte of `[\w$]`.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// A byte JavaScript's `\s` matches (ASCII).
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The largest char boundary of `text` at or below `at`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Dart source with comments and string contents blanked, offsets kept
/// (upstream `blankDartCode`): a literal keeps its quotes, and an
/// interpolation's code (`'${a.b()}'`) stays, so a chain inside one reads
/// like any other. Whole characters are blanked, so the result is as long, in
/// bytes, as the source.
pub fn blank_dart_code(source: &str) -> String {
    #[derive(Clone, Copy)]
    struct Literal {
        quote: u8,
        triple: bool,
        raw: bool,
    }
    fn blank(out: &mut [u8], from: usize, to: usize) {
        let to = to.min(out.len());
        for byte in &mut out[from.min(to)..to] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    let src = source.as_bytes();
    let n = src.len();
    let mut out = src.to_vec();
    // The literals an open `${…}` returns to, with the brace depth around each.
    let mut outer: Vec<(Literal, usize)> = Vec::new();
    let mut literal: Option<Literal> = None;
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < n {
        let c = src[i];
        if let Some(lit) = literal {
            let closing = if lit.triple {
                src.get(i..i + 3) == Some(&[lit.quote; 3][..])
            } else {
                c == lit.quote
            };
            if !lit.raw && c == b'\\' {
                let to = (i + 1 + char_len(src, i + 1)).min(n);
                blank(&mut out, i, to);
                i = to;
            } else if !lit.raw && c == b'$' && src.get(i + 1) == Some(&b'{') {
                blank(&mut out, i, i + 2);
                outer.push((lit, depth));
                literal = None;
                depth = 0;
                i += 2;
            } else if closing {
                i += if lit.triple { 3 } else { 1 };
                literal = None;
            } else if !lit.triple && c == b'\n' {
                literal = None;
                i += 1;
            } else {
                let to = (i + char_len(src, i)).min(n);
                blank(&mut out, i, to);
                i = to;
            }
            continue;
        }
        if c == b'/' && src.get(i + 1) == Some(&b'/') {
            let stop = src[i..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(n, |at| i + at);
            blank(&mut out, i, stop);
            i = stop;
        } else if c == b'/' && src.get(i + 1) == Some(&b'*') {
            let mut nest = 1usize;
            let mut j = i + 2;
            while j < n && nest > 0 {
                if src[j] == b'/' && src.get(j + 1) == Some(&b'*') {
                    nest += 1;
                    j += 2;
                } else if src[j] == b'*' && src.get(j + 1) == Some(&b'/') {
                    nest -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            let j = j.min(n);
            blank(&mut out, i, j);
            i = j;
        } else if c == b'\'' || c == b'"' {
            let triple = src.get(i..i + 3) == Some(&[c; 3][..]);
            let raw = i >= 1 && src[i - 1] == b'r' && !(i >= 2 && is_word_byte(src[i - 2]));
            literal = Some(Literal {
                quote: c,
                triple,
                raw,
            });
            i += if triple { 3 } else { 1 };
        } else {
            if !outer.is_empty() && c == b'{' {
                depth += 1;
            } else if !outer.is_empty() && c == b'}' {
                if depth == 0 {
                    blank(&mut out, i, i + 1);
                    if let Some((lit, around)) = outer.pop() {
                        literal = Some(lit);
                        depth = around;
                    }
                    i += 1;
                    continue;
                }
                depth -= 1;
            }
            i += 1;
        }
    }
    // Only whole characters were replaced, by spaces.
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// Where `name` is written in `text` other than as a member (`.name`): a
/// whole word (upstream `dartNameUses`, `(?<![\w$.])name(?![\w$])`).
pub(crate) fn name_uses(text: &str, name: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(found) = text.get(from..).and_then(|rest| rest.find(name)) {
        let at = from + found;
        let end = at + name.len();
        let before = at
            .checked_sub(1)
            .is_some_and(|prev| is_word_byte(bytes[prev]) || bytes[prev] == b'.');
        let after = bytes.get(end).is_some_and(|next| is_word_byte(*next));
        if !before && !after {
            out.push(at);
        }
        from = end;
    }
    out
}

/// The offset of the `open` bracket matching the `close` one at `at`, read
/// backwards; `None` past `limit` bytes (upstream `dartOpening`).
fn opening(text: &[u8], at: usize, open: u8, close: u8, limit: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut k = at;
    loop {
        if at - k >= limit {
            return None;
        }
        let ch = *text.get(k)?;
        if ch == close {
            depth += 1;
        } else if ch == open {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        } else if open == b'<' && matches!(ch, b';' | b'{' | b'}' | b'=') {
            return None;
        }
        k = k.checked_sub(1)?;
    }
}

/// The simple name a Dart type is written as: `Foo` for `p.Foo<Bar>?`
/// (upstream `dartSimpleTypeName`).
pub(crate) fn simple_type_name(text: &str) -> Option<String> {
    let cut = text.find('<').map_or(text, |at| &text[..at]);
    let unwrapped: String = cut.chars().filter(|c| *c != '?').collect();
    let last = unwrapped.trim().rsplit('.').next()?.trim();
    is_dart_identifier(last).then(|| last.to_string())
}

/// What the nearest declaration of a name before a place says of its type
/// ([`local_declaration`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalDeclaration {
    /// The type written before the name (`final Ticker _ticker`, `Foo? x`,
    /// `(BuildContext context)`).
    Typed(String),
    /// What the initializer starting at this offset evaluates to (`final
    /// todo = Todo(…)`).
    Initializer(usize),
    /// None said: `final x;`, a `for (final x in …)` variable, an untyped
    /// closure parameter, a `catch` variable.
    Untyped,
}

/// The nearest declaration of `name` in `code` before offset `at`, within the
/// scope starting at offset `start`, read from `uses` (where `name` is
/// written, [`name_uses`]); `None` for none (upstream `dartLocalType`).
pub(crate) fn local_declaration(
    code: &DartCode,
    uses: &[usize],
    start: usize,
    at: usize,
    name: &str,
) -> Option<LocalDeclaration> {
    static DECLARED: OnceLock<Regex> = OnceLock::new();
    static TYPED_AFTER: OnceLock<Regex> = OnceLock::new();
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    static CALLEE: OnceLock<Regex> = OnceLock::new();
    static CALLEE_TAIL: OnceLock<Regex> = OnceLock::new();
    static BODY: OnceLock<Regex> = OnceLock::new();
    if start > at {
        return None;
    }
    let text = code.text.as_str();
    let scope = &text[start..at];
    let declared = DECLARED.get_or_init(|| {
        Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)\s*(<[^;{}()=]*>)?\s*\??\s+$")
            .expect("dart declared")
    });
    let typed_after = TYPED_AFTER
        .get_or_init(|| Regex::new(r"^\s*(?:[=;,)}]|in(?-u:\b))").expect("dart typed after"));
    let keyword = KEYWORD.get_or_init(|| {
        Regex::new(r"(?:^|[^A-Za-z0-9_$])(?:final|var|late|const)\s+$").expect("dart keyword")
    });
    let callee = CALLEE.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_$]*)\s*(?:<[^;{}()]*>)?\s*$").expect("dart callee")
    });
    let callee_tail =
        CALLEE_TAIL.get_or_init(|| Regex::new(r"[A-Za-z0-9_$>)\]]\s*$").expect("dart tail"));
    let body = BODY.get_or_init(|| {
        Regex::new(r"^\)\s*(?:async\s*\*?|sync\s*\*)?\s*(?:=>|\{)").expect("dart body")
    });
    // The last use before the call: uses are in file order.
    let lo = uses.partition_point(|used| *used < at);
    for &used in uses[..lo].iter().rev() {
        if used < start {
            break;
        }
        let pos = used - start;
        let before = &scope[floor_boundary(scope, pos.saturating_sub(120))..pos];
        let after = scope.get(pos + name.len()..).unwrap_or("");
        let typed = declared.captures(before).and_then(|caps| {
            let word = caps.get(1)?.as_str();
            (!NON_TYPE_WORDS.contains(&word)
                && typed_after.is_match(after)
                && !after.trim_start().starts_with("=="))
            .then(|| word.to_string())
        });
        if let Some(typed) = typed {
            return Some(LocalDeclaration::Typed(typed));
        }
        // `final x = …`, `var x = …`: the initializer says the type.
        if keyword.is_match(before) {
            let rest = after.trim_start();
            let initialized =
                rest.starts_with('=') && !rest[1..].starts_with('=') && !rest[1..].starts_with('>');
            if !initialized {
                return Some(LocalDeclaration::Untyped);
            }
            return Some(LocalDeclaration::Initializer(
                start + pos + name.len() + (after.len() - rest.len()) + 1,
            ));
        }
        // A closure's untyped parameter, `(context, state) =>`, or a
        // `catch (e)`; not an argument of a call, `foo(x, (a) {…})`, nor
        // an `if (x) {`.
        let bytes = text.as_bytes();
        if after.trim_start().starts_with([',', ')']) && before.trim_end().ends_with(['(', ',']) {
            let open = group_open(bytes, start + pos);
            let close = open.and_then(|open| group_close(bytes, open));
            let lead = open.map_or("", |open| {
                &text[floor_boundary(text, open.saturating_sub(60))..open]
            });
            let called = callee
                .captures(lead)
                .and_then(|caps| caps.get(1))
                .map_or("", |m| m.as_str());
            if called == "catch" {
                return Some(LocalDeclaration::Untyped);
            }
            if let Some(close) = close
                && !callee_tail.is_match(lead)
                && body.is_match(&text[close..floor_boundary(text, close + 30)])
            {
                return Some(LocalDeclaration::Untyped);
            }
        }
    }
    None
}

/// The last character of the Dart expression starting at `from`, where its
/// statement, argument or element ends (upstream `dartInitializerType`).
fn initializer_end(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut nest = 0i64;
    let mut k = from;
    while k < bytes.len() && k - from < 4000 {
        let ch = bytes[k];
        // `AsyncNotifierProvider.family<A, B, C>(…)`: a type argument
        // list's commas end nothing.
        if ch == b'<'
            && k > 0
            && is_word_byte(bytes[k - 1])
            && let Some(close) = type_arguments_end(bytes, k)
            && close > 0
        {
            k = close + 1;
            continue;
        }
        if matches!(ch, b'(' | b'[' | b'{') {
            nest += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            if nest == 0 {
                break;
            }
            nest -= 1;
        } else if (ch == b';' || ch == b',') && nest == 0 {
            break;
        }
        k += 1;
    }
    let mut end = k.checked_sub(1)?;
    while end >= from && is_space(bytes[end]) {
        end = end.checked_sub(1)?;
    }
    (end >= from).then_some(end)
}

/// The type a parenthesized Dart expression casts to (`(x as Foo)`), as
/// written.
fn dart_cast(inner: &str) -> Option<&str> {
    static CAST: OnceLock<Regex> = OnceLock::new();
    let cast = CAST.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)as\s+((?:[A-Za-z_$][A-Za-z0-9_$]*\.)?[A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^()]*>)?\s*\??\s*$",
        )
        .expect("dart cast")
    });
    cast.captures(inner)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str())
}

/// A link of a chain: `.name`, `.name<…>(…)` (`call`), or a subscript `[…]`
/// (`index`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Link {
    name: String,
    type_args: Vec<String>,
    call: bool,
    index: bool,
}

/// What a chain starts from: a name (`x`, `Foo`, `foo(…)`, `Foo<T>(…)`, with
/// the keyword written before it), a string literal, or a parenthesized
/// expression.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Head {
    Name {
        name: String,
        type_args: Vec<String>,
        call: bool,
        keyword: String,
        /// Where the name starts.
        start: usize,
        /// Where the keyword starts.
        keyword_start: usize,
    },
    String,
    Paren {
        open: usize,
        close: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Chain {
    head: Head,
    links: Vec<Link>,
}

impl Chain {
    /// Where the expression the chain reads starts, as the extractor places
    /// its references: the name's start, a `const` or `new` before it, or the
    /// parenthesis.
    fn start(&self) -> Option<usize> {
        match &self.head {
            Head::Name {
                keyword,
                start,
                keyword_start,
                ..
            } => Some(if keyword == "const" || keyword == "new" {
                *keyword_start
            } else {
                *start
            }),
            Head::Paren { open, .. } => Some(*open),
            Head::String => None,
        }
    }
}

/// The last word of `text[..=j]` within its last eleven bytes, a letter or
/// `_` first (upstream's `/([A-Za-z_]\w*)$/` over that slice), with where it
/// starts.
fn trailing_keyword(text: &[u8], j: isize) -> (String, usize) {
    let Ok(j) = usize::try_from(j) else {
        return (String::new(), 0);
    };
    let from = j.saturating_sub(10);
    let slice = &text[from..=j.min(text.len() - 1)];
    let mut start = slice.len();
    while start > 0 && (slice[start - 1].is_ascii_alphanumeric() || slice[start - 1] == b'_') {
        start -= 1;
    }
    let run = &slice[start..];
    match run
        .iter()
        .position(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        Some(first) => (
            String::from_utf8_lossy(&run[first..]).into_owned(),
            from + start + first,
        ),
        None => (String::new(), 0),
    }
}

/// The links and head of the Dart expression whose last character is at
/// `end`, read backwards (upstream `readDartChain`): `FutureProvider.
/// autoDispose` is `FutureProvider` then `.autoDispose`; `events.map(mapper)`
/// is `events` then `.map(…)`. `None` for a shape the walk does not read: a
/// cascade section, a number, a call of an expression.
fn read_chain(text: &str, end: usize) -> Option<Chain> {
    let bytes = text.as_bytes();
    let at = |i: isize| -> u8 {
        usize::try_from(i)
            .ok()
            .and_then(|i| bytes.get(i).copied())
            .unwrap_or(0)
    };
    let skip = |i: &mut isize| {
        while *i >= 0 && is_space(at(*i)) {
            *i -= 1;
        }
    };
    let mut links: Vec<Link> = Vec::new();
    let mut i = isize::try_from(end).ok()?;
    for _ in 0..32 {
        skip(&mut i);
        while at(i) == b'!' {
            i -= 1;
            skip(&mut i);
        }
        if at(i) == b']' {
            let open = opening(bytes, i as usize, b'[', b']', 20_000)?;
            links.push(Link {
                name: String::new(),
                type_args: Vec::new(),
                call: false,
                index: true,
            });
            i = open as isize - 1;
            continue;
        }
        if at(i) == b'\'' || at(i) == b'"' {
            links.reverse();
            return Some(Chain {
                head: Head::String,
                links,
            });
        }
        let mut call = false;
        let mut type_args: Vec<String> = Vec::new();
        if at(i) == b')' {
            let close = i as usize;
            let open = opening(bytes, close, b'(', b')', 20_000)?;
            i = open as isize - 1;
            skip(&mut i);
            if at(i) == b'>' {
                let lt = opening(bytes, i as usize, b'<', b'>', 400)?;
                type_args = crate::cpp_namespaces::split_cpp_top_level(&text[lt + 1..i as usize]);
                i = lt as isize - 1;
                skip(&mut i);
            }
            if !is_word_byte(at(i)) {
                // `(…)` with no name before it is the head, unless an
                // expression is called: `f()(…)`.
                if !type_args.is_empty() || at(i) == b')' || at(i) == b']' {
                    return None;
                }
                links.reverse();
                return Some(Chain {
                    head: Head::Paren { open, close },
                    links,
                });
            }
            call = true;
        } else if at(i) == b'>' {
            // `BlocProvider<CounterCubit>.value(…)`: a type written with its
            // arguments.
            let lt = opening(bytes, i as usize, b'<', b'>', 400)?;
            type_args = crate::cpp_namespaces::split_cpp_top_level(&text[lt + 1..i as usize]);
            i = lt as isize - 1;
            skip(&mut i);
        }
        let name_end = usize::try_from(i + 1).ok()?;
        while i >= 0 && is_word_byte(at(i)) {
            i -= 1;
        }
        let name_start = usize::try_from(i + 1).ok()?;
        let name = text.get(name_start..name_end)?;
        if !is_dart_identifier(name) {
            return None;
        }
        let mut j = i;
        while j >= 0 && is_space(at(j)) {
            j -= 1;
        }
        if at(j) == b'.' {
            // `obj..a().b()`: a cascade section's receiver is the cascade's
            // target.
            if at(j - 1) == b'.' {
                return None;
            }
            links.push(Link {
                name: name.to_string(),
                type_args,
                call,
                index: false,
            });
            i = j - 1;
            if at(i) == b'?' {
                i -= 1;
            }
            continue;
        }
        let (keyword, keyword_start) = trailing_keyword(bytes, j);
        links.reverse();
        return Some(Chain {
            head: Head::Name {
                name: name.to_string(),
                type_args,
                call,
                keyword,
                start: name_start,
                keyword_start,
            },
            links,
        });
    }
    None
}

/// The `>` closing a Dart type argument list opened at `open`, one followed by
/// a call, a member or another `>` (which a comparison's `<` never is)
/// (upstream `dartTypeArgumentsEnd`).
pub(crate) fn type_arguments_end(text: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut k = open;
    while k < text.len() && k - open < 400 {
        let ch = text[k];
        if ch == b'<' {
            depth += 1;
        } else if ch == b'>' {
            depth -= 1;
            if depth == 0 {
                let next = &text[k + 1..(k + 4).min(text.len())];
                let first = next.iter().copied().find(|byte| !is_space(*byte));
                return matches!(first, Some(b'(' | b'.' | b'>' | b'?')).then_some(k);
            }
        } else if matches!(ch, b';' | b'{' | b'}' | b'=') {
            return None;
        }
        k += 1;
    }
    None
}

/// The `(` of the parenthesized group around offset `at`, read backwards
/// (upstream `dartGroupOpen`).
fn group_open(text: &[u8], at: usize) -> Option<usize> {
    let mut parens = 0i64;
    let mut braces = 0i64;
    let mut k = at;
    while k > 0 && at - (k - 1) < 4000 {
        k -= 1;
        let ch = text[k];
        if ch == b'}' {
            braces += 1;
        } else if ch == b'{' {
            if braces == 0 {
                return None;
            }
            braces -= 1;
        } else if braces > 0 {
            continue;
        } else if ch == b')' {
            parens += 1;
        } else if ch == b'(' {
            if parens == 0 {
                return Some(k);
            }
            parens -= 1;
        } else if ch == b';' {
            return None;
        }
    }
    None
}

/// The `)` closing the `(` at `open` (upstream `dartGroupClose`).
fn group_close(text: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut k = open;
    while k < text.len() && k - open < 8000 {
        if text[k] == b'(' {
            depth += 1;
        } else if text[k] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
        k += 1;
    }
    None
}

/// What a chain evaluates to: an instance of `type_name`, or (`is_static`)
/// the type itself, before a static member — of the type the file `owner`
/// declares, when the chain says which (`p.Box` names the one `p` brings in,
/// upstream #2389) (upstream `DartValue`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Value {
    type_name: String,
    is_static: bool,
    via_super: bool,
    owner: Option<String>,
}

fn instance(type_name: String) -> Value {
    Value {
        type_name,
        is_static: false,
        via_super: false,
        owner: None,
    }
}

/// A Dart function's declared return type as a chain reads it, with the
/// function's own type parameters (upstream `DART_RETURN_TYPES`).
#[derive(Debug)]
pub struct ReturnInfo {
    type_name: String,
    own: Vec<String>,
}

/// Where an expression is typed: a reference of the file, and where its call
/// is written (the name a declaration of the code before it is looked for
/// up to).
#[derive(Debug, Clone)]
struct Site {
    view: RefView,
    at: Option<usize>,
}

/// One chain link's resolution.
struct Typing<'c> {
    context: &'c dyn ResolutionContext,
    /// The link's member name, which a site in another file looks for on its
    /// line, as upstream's do.
    name: &'c str,
}

impl Typing<'_> {
    fn code(&self, file: &str) -> Option<Arc<DartCode>> {
        Some(self.context.source_facts(file)?.dart_code())
    }

    /// A site at the start of 1-based `line` of `file`, as upstream's
    /// `{ ...ref, filePath, line, column: 0 }`: its call is where the link's
    /// name first shows on that line.
    fn site_at(&self, base: &Site, file: &str, line: i64) -> Site {
        let mut view = base.view.clone();
        view.file_path = file.to_string();
        view.line = line;
        view.column = 0;
        let at = self.code(file).and_then(|code| {
            let start = code.line_start(line)?;
            let text = code.line_text(line)?;
            text.find(self.name).map(|found| start + found)
        });
        Site { view, at }
    }

    fn is_type_name(&self, name: &str) -> bool {
        self.context
            .get_nodes_by_name_shared(name)
            .iter()
            .any(|node| node.language == Language::Dart && is_dart_type_kind(node.kind))
    }

    /// What a chain evaluates to, link by link, or `None` where the evidence
    /// runs out (upstream `typeDartChain`).
    fn type_chain(&self, chain: &Chain, site: &Site, depth: u32) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut links: &[Link] = &chain.links;
        let mut value = match &chain.head {
            Head::String => Some(instance("String".to_string())),
            Head::Paren { open, close } => self.type_paren(*open, *close, site, depth),
            Head::Name {
                name,
                type_args,
                call,
                keyword,
                start,
                ..
            } => {
                // `p.Provider.autoDispose`: an import prefix names a library,
                // so the chain starts at the link after it, which names what
                // that library exports (upstream #2389). A parameter, local
                // or member named like the prefix hides it.
                if let Some(first) = links.first()
                    && !*call
                    && type_args.is_empty()
                    && !first.index
                    && crate::dart_libraries::dart_import_prefixes(
                        &site.view.file_path,
                        self.context,
                    )
                    .contains(name.as_str())
                    && !crate::dart_prefixes::is_dart_name_hidden(
                        name,
                        &site.view.file_path,
                        site.view.line,
                        *start,
                        self.context,
                    )
                {
                    links = &links[1..];
                    self.prefixed_head(name, first, site, depth)
                } else {
                    // `const Foo.named(…)`: the named constructor is the
                    // head's own link.
                    if (keyword == "const" || keyword == "new")
                        && !*call
                        && links.first().is_some_and(|link| link.call)
                    {
                        links = &links[1..];
                    }
                    self.type_head(name, type_args, *call, keyword, site, depth)
                }
            }
        };
        for link in links {
            // Every Dart object's `toString()` returns a String.
            if link.name == "toString" && link.call && link.type_args.is_empty() {
                value = Some(instance("String".to_string()));
                continue;
            }
            let next = value
                .as_ref()
                .and_then(|value| self.type_link(value, link, site, depth));
            // A lookup nothing declared answers for hands back the type it is
            // given, whatever it is called on: `context.read<LoginCubit>()`.
            let given = if next.is_none()
                && link.call
                && link.type_args.len() == 1
                && TYPE_ARGUMENT_LOOKUPS.contains(&link.name.as_str())
                && !value
                    .as_ref()
                    .is_some_and(|value| self.link_declared(value, link, site))
            {
                simple_type_name(&link.type_args[0])
            } else {
                None
            };
            // An untyped value stays untyped until a link says what it makes.
            value = next.or_else(|| given.map(instance));
        }
        value
    }

    /// Whether the type a chain has reached declares the member a link names.
    fn link_declared(&self, value: &Value, link: &Link, site: &Site) -> bool {
        if value.is_static {
            self.static_member(&value.type_name, &link.name, site, value.owner.as_deref())
                .is_some()
        } else {
            dart_member_of(
                &value.type_name,
                &link.name,
                &site.view,
                self.context,
                &|node| !is_setter(node, self.context),
                u64::from(value.via_super),
            )
            .is_some()
        }
    }

    /// `(x as Foo)` is a Foo; any other parenthesized expression is what its
    /// last operand is: `(a ?? Todo(…))` (upstream `typeDartParen`).
    fn type_paren(&self, open: usize, close: usize, site: &Site, depth: u32) -> Option<Value> {
        let code = self.code(&site.view.file_path)?;
        let inner = code.text.get(open + 1..close)?;
        if let Some(cast) = dart_cast(inner) {
            return simple_type_name(cast).map(instance);
        }
        let bytes = code.text.as_bytes();
        let mut end = close.checked_sub(1)?;
        while end > open && is_space(bytes[end]) {
            end -= 1;
        }
        self.expression_value(&code.text, end, site, depth)
    }

    /// The value of the expression ending at `end` of the site's code: an
    /// instance, never a type, and never an awaited one.
    fn expression_value(&self, text: &str, end: usize, site: &Site, depth: u32) -> Option<Value> {
        let chain = read_chain(text, end)?;
        if matches!(&chain.head, Head::Name { keyword, .. } if keyword == "await") {
            return None;
        }
        let value = self.type_chain(&chain, site, depth + 1)?;
        (!value.is_static).then_some(value)
    }

    /// What a chain's head evaluates to (upstream `typeDartHead`).
    fn type_head(
        &self,
        name: &str,
        type_args: &[String],
        call: bool,
        keyword: &str,
        site: &Site,
        depth: u32,
    ) -> Option<Value> {
        if name == "this" || name == "super" {
            let own = self.enclosing_type(site)?;
            return Some(Value {
                type_name: own.name.clone(),
                is_static: false,
                via_super: name == "super",
                owner: None,
            });
        }
        if matches!(keyword, "const" | "new" | "as") {
            return Some(instance(name.to_string()));
        }
        if keyword == "is" {
            return None;
        }
        let is_type = self.is_type_name(name);
        if call {
            // `Foo(…)` constructs a Foo; `foo(…)` is what the function says it
            // returns, or, a variable holding a callable, what its `call`
            // does.
            if is_type {
                return Some(instance(name.to_string()));
            }
            if let Some(function) = self.function_named(name, site) {
                return self.return_type(&function, type_args).map(instance);
            }
            let held = self.variable_type(name, site, depth)?;
            return self.call_result(&held, type_args, site);
        }
        // `Foo.` before a static member, a type outside the project included,
        // whose members nothing reaches.
        let uppercase = name.starts_with(|c: char| c.is_ascii_uppercase());
        if is_type
            || (uppercase
                && !self
                    .context
                    .get_nodes_by_name_shared(name)
                    .iter()
                    .any(|node| {
                        node.language == Language::Dart
                            && matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
                    }))
        {
            return Some(Value {
                type_name: name.to_string(),
                is_static: true,
                via_super: false,
                owner: None,
            });
        }
        self.variable_type(name, site, depth).map(instance)
    }

    /// What one link of a chain makes of the value before it (upstream
    /// `typeDartLink`).
    fn type_link(&self, value: &Value, link: &Link, site: &Site, depth: u32) -> Option<Value> {
        if link.index {
            return None;
        }
        if value.is_static {
            let Some(member) =
                self.static_member(&value.type_name, &link.name, site, value.owner.as_deref())
            else {
                // `BlocProvider.value(…)`: a constructor the index holds no
                // node for.
                return (link.call && self.is_type_name(&value.type_name))
                    .then(|| instance(value.type_name.clone()));
            };
            if member.kind == NodeKind::EnumMember {
                return (!link.call).then(|| instance(value.type_name.clone()));
            }
            if member.kind == NodeKind::Constant {
                // `FutureProvider.autoDispose`: the value the static constant
                // holds.
                let held = self.constant_type(&member, site, depth)?;
                return if link.call {
                    self.call_result(&held, &link.type_args, site)
                } else {
                    Some(instance(held))
                };
            }
            return self.member_use(&member, link, site);
        }
        if let Some((member, _)) = dart_member_of(
            &value.type_name,
            &link.name,
            &site.view,
            self.context,
            &|node| !is_setter(node, self.context),
            u64::from(value.via_super),
        ) {
            return self.member_use(&member, link, site);
        }
        // A field mints no node: its declared type is read from its class.
        if link.call {
            return None;
        }
        self.field_type(&value.type_name, &link.name, site, depth)
            .map(instance)
    }

    /// What using a member makes: a method's call or a getter's read is its
    /// declared return type; a getter called is its value called (upstream
    /// `typeDartMemberUse`).
    fn member_use(&self, member: &Node, link: &Link, site: &Site) -> Option<Value> {
        let getter = is_dart_getter(member, self.context);
        let type_args: &[String] = if getter { &[] } else { &link.type_args };
        let type_name = self.return_type(member, type_args)?;
        if getter {
            return if link.call {
                self.call_result(&type_name, &link.type_args, site)
            } else {
                Some(instance(type_name))
            };
        }
        // A method named without a call is a tear-off: a function, not its
        // result.
        link.call.then(|| instance(type_name))
    }

    /// What calling a value of `type_name` returns: its `call` method's
    /// declared return type (upstream `dartCallResult`).
    fn call_result(&self, type_name: &str, type_args: &[String], site: &Site) -> Option<Value> {
        let (call, _) = dart_member_of(
            type_name,
            "call",
            &site.view,
            self.context,
            &|node| !is_dart_getter(node, self.context),
            0,
        )?;
        self.return_type(&call, type_args).map(instance)
    }

    /// A Dart function's declared return type as a simple name, a type
    /// parameter of its own replaced by the type argument the call gives
    /// (`T read<T>()` called as `read<Foo>()` is a Foo); a type parameter of
    /// its class (`State get state` in `BlocBase<State>`) says nothing
    /// without the receiver's own arguments (upstream `dartReturnType`).
    fn return_type(&self, function: &Node, type_args: &[String]) -> Option<String> {
        let tables = self.context.dart_tables();
        let memo = tables
            .return_types
            .lock()
            .ok()
            .and_then(|memo| memo.get(&function.id).cloned());
        let info = match memo {
            Some(info) => info,
            None => {
                let info = self.read_return_type(function).map(Arc::new);
                if let Ok(mut memo) = tables.return_types.lock() {
                    memo.insert(function.id.clone(), info.clone());
                }
                info
            }
        }?;
        match info.own.iter().position(|own| *own == info.type_name) {
            None => Some(info.type_name.clone()),
            Some(at) => type_args.get(at).and_then(|arg| simple_type_name(arg)),
        }
    }

    fn read_return_type(&self, function: &Node) -> Option<ReturnInfo> {
        let type_name = function.return_type.as_deref().and_then(simple_type_name)?;
        // `static T of<T …>(…)` in `BlocProvider<T …>`: the function's own
        // parameter is the one meant.
        let own = crate::member_types::class_type_parameters(function, self.context);
        if !own.contains(&type_name)
            && let Some((owner_qn, _)) = function.qualified_name.rsplit_once("::")
            && !owner_qn.is_empty()
        {
            let owner = in_order(self.context.get_nodes_in_file_shared(&function.file_path))
                .into_iter()
                .find(|node| {
                    is_dart_type_kind(node.kind)
                        && node.qualified_name == owner_qn
                        && node.start_line <= function.start_line
                        && node.end_line >= function.end_line
                });
            if let Some(owner) = owner
                && crate::member_types::class_type_parameters(&owner, self.context)
                    .contains(&type_name)
            {
                return None;
            }
        }
        Some(ReturnInfo { type_name, own })
    }

    /// The innermost Dart class, mixin, enum or extension the site is in
    /// (upstream `dartEnclosingType`).
    fn enclosing_type(&self, site: &Site) -> Option<Arc<Node>> {
        let mut own: Option<Arc<Node>> = None;
        for node in in_order(self.context.get_nodes_in_file_shared(&site.view.file_path)) {
            if is_dart_type_kind(node.kind)
                && node.start_line <= site.view.line
                && node.end_line >= site.view.line
                && own
                    .as_ref()
                    .is_none_or(|own| node.start_line >= own.start_line)
            {
                own = Some(node);
            }
        }
        own
    }

    /// The static member `name` the Dart type `type_name` itself declares
    /// ([`crate::dart_members::dart_static_member`]), preferring the site's
    /// file; of the type the file `owner` declares, when given.
    fn static_member(
        &self,
        type_name: &str,
        name: &str,
        site: &Site,
        owner: Option<&str>,
    ) -> Option<Arc<Node>> {
        crate::dart_members::dart_static_member(
            type_name,
            name,
            &site.view.file_path,
            self.context,
            &|type_node| owner.is_none_or(|owner| type_node.file_path == owner),
        )
    }

    /// What the first link after an import prefix evaluates to —
    /// `p.Provider` before a static member, `p.Report(…)`, `p.make(…)`,
    /// `p.config` — read from the top-level declarations the prefixed import
    /// brings in, and nothing when it brings in none of that name: a package
    /// outside the repository, whatever the project declares under the name
    /// elsewhere (upstream #2389 `typeDartPrefixedHead`).
    fn prefixed_head(&self, prefix: &str, link: &Link, site: &Site, depth: u32) -> Option<Value> {
        let found = crate::dart_prefixes::dart_prefixed_decls(
            prefix,
            &link.name,
            &site.view.file_path,
            self.context,
        );
        let types: Vec<&Arc<Node>> = found
            .iter()
            .filter(|node| is_dart_type_kind(node.kind))
            .collect();
        if !types.is_empty() {
            return Some(Value {
                type_name: link.name.clone(),
                is_static: !link.call,
                via_super: false,
                owner: (types.len() == 1).then(|| types[0].file_path.clone()),
            });
        }
        let [decl] = found.as_slice() else {
            return None;
        };
        if decl.kind == NodeKind::Function {
            return if link.call {
                self.return_type(decl, &link.type_args).map(instance)
            } else {
                None
            };
        }
        if !matches!(decl.kind, NodeKind::Constant | NodeKind::Variable) {
            return None;
        }
        let held = self.constant_type(decl, site, depth)?;
        if link.call {
            self.call_result(&held, &link.type_args, site)
        } else {
            Some(instance(held))
        }
    }

    /// The function a bare Dart call `name(…)` means: a member of the type
    /// around it (an implicit `this.`), else a function of the file, else
    /// the one function of that name the library holds or the project
    /// declares (upstream `dartFunctionNamed`).
    fn function_named(&self, name: &str, site: &Site) -> Option<Arc<Node>> {
        if let Some(own) = self.enclosing_type(site)
            && let Some((member, _)) = dart_member_of(
                &own.name,
                name,
                &site.view,
                self.context,
                &|node| !is_dart_getter(node, self.context) && !is_setter(node, self.context),
                0,
            )
        {
            return Some(member);
        }
        let functions: Vec<Arc<Node>> = self
            .context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|node| {
                node.language == Language::Dart
                    && node.kind == NodeKind::Function
                    && !node.qualified_name.contains("::")
            })
            .collect();
        self.library_pick(&functions, site)
    }

    /// Of same-named top-level Dart declarations, the one the site's library
    /// holds: its own file, then another file of the library, else the only
    /// one (upstream `dartLibraryPick`; KEEP-RUST: the library is
    /// W4-DART-02's, upstream reads the file's `part` directives itself).
    fn library_pick(&self, nodes: &[Arc<Node>], site: &Site) -> Option<Arc<Node>> {
        if let Some(own) = nodes
            .iter()
            .find(|node| node.file_path == site.view.file_path)
        {
            return Some(Arc::clone(own));
        }
        let shared: Vec<&Arc<Node>> = nodes
            .iter()
            .filter(|node| {
                crate::dart_libraries::in_same_dart_library(
                    &site.view.file_path,
                    &node.file_path,
                    self.context,
                )
            })
            .collect();
        if shared.len() == 1 {
            return Some(Arc::clone(shared[0]));
        }
        (nodes.len() == 1).then(|| Arc::clone(&nodes[0]))
    }

    /// The declared type of a Dart variable a chain starts from: a local or
    /// parameter the code around the call declares (the nearest declaration
    /// wins, and an untyped one ends the search), a field or getter of the
    /// class around it, or a top-level constant (upstream `dartVariableType`).
    fn variable_type(&self, name: &str, site: &Site, depth: u32) -> Option<String> {
        if let Some(local) = self.local_type(name, site, depth) {
            return local;
        }
        if let Some(field) =
            crate::member_types::infer_member_receiver_type(name, &site.view, self.context)
        {
            return simple_type_name(&field);
        }
        if let Some(own) = self.enclosing_type(site)
            && let Some((getter, _)) = dart_member_of(
                &own.name,
                name,
                &site.view,
                self.context,
                &|node| is_dart_getter(node, self.context),
                0,
            )
        {
            return self.return_type(&getter, &[]);
        }
        let constants: Vec<Arc<Node>> = self
            .context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|node| {
                node.language == Language::Dart
                    && node.kind == NodeKind::Constant
                    && !node.qualified_name.contains("::")
            })
            .collect();
        let top = self.library_pick(&constants, site)?;
        self.constant_type(&top, site, depth)
    }

    /// The type of the nearest declaration of `name` before the call, within
    /// the function around it ([`local_declaration`]): its declared type,
    /// else what its initializer evaluates to. `Some(None)` for a declaration
    /// that says no type; `None` for none (upstream `dartLocalType`).
    fn local_type(&self, name: &str, site: &Site, depth: u32) -> Option<Option<String>> {
        let facts = self.context.source_facts(&site.view.file_path)?;
        let code = facts.dart_code();
        let at = site.at?;
        let scope_line = crate::name_matcher::enclosing_scope_start_line(&site.view, self.context);
        let start = code.line_start(scope_line).unwrap_or(0);
        Some(
            match local_declaration(&code, &code.name_uses(name), start, at, name)? {
                LocalDeclaration::Typed(typed) => Some(typed),
                LocalDeclaration::Initializer(from) => {
                    self.initializer_type(&code.text, from, site, depth)
                }
                LocalDeclaration::Untyped => None,
            },
        )
    }

    /// What the Dart expression starting at `from` evaluates to, read back
    /// from where its statement ends (upstream `dartInitializerType`).
    fn initializer_type(&self, text: &str, from: usize, site: &Site, depth: u32) -> Option<String> {
        let end = initializer_end(text, from)?;
        self.expression_value(text, end, site, depth)
            .map(|value| value.type_name)
    }

    /// The type a Dart constant is declared with (`static const Foo x = …`,
    /// `final Foo? x`) or, with none written, what its initializer evaluates
    /// to: riverpod's `static const autoDispose =
    /// AutoDisposeFutureProviderBuilder();` (upstream `dartConstantType`).
    fn constant_type(&self, constant: &Node, site: &Site, depth: u32) -> Option<String> {
        let code = self.code(&constant.file_path)?;
        let from = code.line_start(constant.start_line)?;
        let window = &code.text[from..floor_boundary(&code.text, from + 600)];
        let head = Regex::new(&format!(
            r"(?-u:\b)(?:const|final|var)\s+(?:([A-Za-z_$][A-Za-z0-9_$.]*)\s*(?:<[^;=]*>)?\s*\??\s+)?{}\s*(=)(?:[^=>]|$)",
            regex::escape(&constant.name)
        ))
        .ok()?;
        let caps = head.captures(window)?;
        if let Some(written) = caps.get(1)
            && !NON_TYPE_WORDS.contains(&written.as_str())
        {
            return simple_type_name(written.as_str());
        }
        let init = from + caps.get(2)?.end();
        let at = self.site_at(site, &constant.file_path, constant.start_line);
        self.initializer_type(&code.text, init, &at, depth + 1)
    }

    /// The type a Dart class, or a type it extends, mixes in or implements,
    /// declares a field `name` with (`final ProviderPointerManager
    /// _pointerManager`), or what the field is initialized with (`final
    /// _cache = Cache()`) (upstream `dartFieldType`). KEEP-RUST: the
    /// initializer is read only where a field is declared, at the class's
    /// member level; upstream's search runs over the class's text, method
    /// bodies included.
    fn field_type(&self, type_name: &str, name: &str, site: &Site, depth: u32) -> Option<String> {
        let lineage = dart_lineage(type_name, self.context);
        let mut order: Vec<&(String, u64)> = lineage.order.iter().collect();
        order.sort_by_key(|(_, depth)| *depth);
        let getter = Regex::new(&format!(
            r"(?:^|[\s;{{}}])([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^;{{}}=]*>)?\s*\??\s+get\s+{}\s*;",
            regex::escape(name)
        ))
        .ok()?;
        let initializer = Regex::new(&format!(
            r"(?-u:\b)(?:final|var|late)\s+{}\s*(=)(?:[^=>]|$)",
            regex::escape(name)
        ))
        .ok()?;
        for (owner, _) in order {
            for decl in self.context.get_nodes_by_name_shared(owner) {
                if decl.language != Language::Dart || !is_dart_type_kind(decl.kind) {
                    continue;
                }
                if let Some(declared) =
                    crate::member_types::class_member_type(&decl, name, self.context)
                {
                    return simple_type_name(&declared);
                }
                // An abstract getter mints no node: `ProviderElement? get
                // _listenedElement;`.
                for (text, _) in crate::member_types::class_member_lines(&decl, self.context).iter()
                {
                    if !text.contains(name) {
                        continue;
                    }
                    if let Some(caps) = getter.captures(text)
                        && !NON_TYPE_WORDS.contains(&&caps[1])
                    {
                        return simple_type_name(&caps[1]);
                    }
                }
                let Some(code) = self.code(&decl.file_path) else {
                    continue;
                };
                let Some(from) = code.line_start(decl.start_line) else {
                    continue;
                };
                let to = code
                    .line_start(decl.end_line + 1)
                    .unwrap_or(code.text.len());
                let body = &code.text[from..to.max(from)];
                let bytes = body.as_bytes();
                let mut braces = 0i64;
                let mut scanned = 0usize;
                for caps in initializer.captures_iter(body) {
                    let Some(whole) = caps.get(0) else {
                        continue;
                    };
                    for byte in &bytes[scanned..whole.start()] {
                        match byte {
                            b'{' => braces += 1,
                            b'}' => braces -= 1,
                            _ => {}
                        }
                    }
                    scanned = whole.start();
                    if braces != 1 {
                        continue;
                    }
                    let init = from + caps.get(1).map_or(whole.end(), |eq| eq.end());
                    let at = self.site_at(site, &decl.file_path, decl.start_line);
                    return self.initializer_type(&code.text, init, &at, depth + 1);
                }
            }
        }
        None
    }
}

/// Whether a Dart member is a setter, declared `set <name>(…)`, which no call
/// or read runs (upstream `isDartSetter`).
pub(crate) fn is_setter(node: &Node, context: &dyn ResolutionContext) -> bool {
    if node.language != Language::Dart || node.kind != NodeKind::Method {
        return false;
    }
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .setters
        .lock()
        .ok()
        .and_then(|memo| memo.get(&node.id).copied())
    {
        return hit;
    }
    let (head, _) = crate::dart_scope::dart_member_head(node, context);
    // The pattern writes `set` and the name; a line writing neither compiles
    // nothing.
    let head = head.as_str();
    let setter = head.contains("set")
        && head.contains(node.name.as_str())
        && Regex::new(&format!(
            r"^[^={{;]*?(?-u:\b)set\s+{}\s*\(",
            regex::escape(&node.name)
        ))
        .is_ok_and(|setter| setter.is_match(head));
    if let Ok(mut memo) = tables.setters.lock() {
        memo.insert(node.id.clone(), setter);
    }
    setter
}

/// How the extractor records a call written `<receiver>.<name>(…)`, read
/// from the text before the dot: through a plain name (`x.name`), through a
/// member chain's last segment (`s.name`, #1496), or by the name alone (a
/// call, a parenthesis, an index, `!`, `this` or `super`).
#[derive(Debug, PartialEq, Eq)]
enum Encoding<'t> {
    Ident(&'t str),
    Segment(&'t str),
    Bare,
}

fn encoding_before<'t>(text: &'t str, end: isize) -> Encoding<'t> {
    let bytes = text.as_bytes();
    let at = |i: isize| -> u8 {
        usize::try_from(i)
            .ok()
            .and_then(|i| bytes.get(i).copied())
            .unwrap_or(0)
    };
    if !is_word_byte(at(end)) {
        return Encoding::Bare;
    }
    let mut start = end;
    while start > 0 && is_word_byte(at(start - 1)) {
        start -= 1;
    }
    let word = &text[start as usize..=end as usize];
    if matches!(word, "this" | "super") {
        return Encoding::Bare;
    }
    let mut before = start - 1;
    while before >= 0 && is_space(at(before)) {
        before -= 1;
    }
    if at(before) == b'.' && at(before - 1) != b'.' {
        return Encoding::Segment(word);
    }
    Encoding::Ident(word)
}

/// Whether a call follows offset `from`: `(`, or type arguments and `(`.
fn call_follows(text: &[u8], from: usize) -> bool {
    let mut k = from;
    while k < text.len() && is_space(text[k]) {
        k += 1;
    }
    if text.get(k) == Some(&b'<') {
        let Some(close) = type_arguments_end(text, k) else {
            return false;
        };
        k = close + 1;
        while k < text.len() && is_space(text[k]) {
            k += 1;
        }
    }
    text.get(k) == Some(&b'(')
}

/// The parts of a chain link's reference: the segment the extractor kept
/// before the member, if any, and the member.
fn link_parts(name: &str) -> Option<(Option<&str>, &str)> {
    if is_dart_identifier(name) {
        return Some((None, name));
    }
    let (segment, member) = name.split_once('.')?;
    (is_dart_identifier(segment) && is_dart_identifier(member)).then_some((Some(segment), member))
}

/// A call written `<receiver>.<member>(…)` (or `?.`) that the extractor
/// records by its member alone (after a call, a parenthesis, an index, `!`,
/// `this` or `super`) or past the receiver's last segment (`s.m`, #1496),
/// with the chain read back from its receiver.
#[derive(Debug)]
struct LinkAt {
    /// Where the member starts and ends.
    at: usize,
    end: usize,
    /// The segment the extractor records before the member, if any.
    segment: Option<String>,
    chain: Chain,
}

/// Every call of `text` [`LinkAt`] reads, by where its chain starts as the
/// extractor places its references ([`Chain::start`]); a cascade's `..m` and
/// a spread's `...m` are none, and neither is a call through a plain name
/// (`x.m`).
fn link_index(text: &str) -> HashMap<usize, Vec<LinkAt>> {
    let bytes = text.as_bytes();
    let at = |i: isize| -> u8 {
        usize::try_from(i)
            .ok()
            .and_then(|i| bytes.get(i).copied())
            .unwrap_or(0)
    };
    let mut out: HashMap<usize, Vec<LinkAt>> = HashMap::new();
    let mut k = 0usize;
    while k < bytes.len() {
        if !is_word_byte(bytes[k]) {
            k += 1;
            continue;
        }
        let mut end = k;
        while end < bytes.len() && is_word_byte(bytes[end]) {
            end += 1;
        }
        let (start, member_end) = (k, end);
        k = end;
        if !is_dart_identifier(&text[start..member_end]) {
            continue;
        }
        let mut dot = start as isize - 1;
        while dot >= 0 && is_space(at(dot)) {
            dot -= 1;
        }
        if at(dot) != b'.' || at(dot - 1) == b'.' {
            continue;
        }
        let mut receiver_end = dot - 1;
        if at(receiver_end) == b'?' {
            receiver_end -= 1;
        }
        while receiver_end >= 0 && is_space(at(receiver_end)) {
            receiver_end -= 1;
        }
        let segment = match encoding_before(text, receiver_end) {
            Encoding::Bare => None,
            Encoding::Segment(word) => Some(word.to_string()),
            Encoding::Ident(_) => continue,
        };
        if receiver_end < 0 || !call_follows(bytes, member_end) {
            continue;
        }
        let Some(chain) = read_chain(text, receiver_end as usize) else {
            continue;
        };
        if let Some(chain_start) = chain.start() {
            out.entry(chain_start).or_default().push(LinkAt {
                at: start,
                end: member_end,
                segment,
                chain,
            });
        }
    }
    out
}

/// The links a chain link's reference stands for (KEEP-RUST): the calls of
/// its member written after the reference's position, on its line or the
/// lines below, that the extractor records as this reference, each with the
/// chain read back from it, which starts at the reference's position. A
/// bare member is one written after a call, a parenthesis, an index, `!`,
/// `this` or `super`; `s.m` one written after a member `s` of something.
fn link_sites(code: &DartCode, line: i64, column: i64, name: &str) -> Vec<(usize, Chain)> {
    let Some((segment, member)) = link_parts(name) else {
        return Vec::new();
    };
    let Some(position) = code.offset(line, column) else {
        return Vec::new();
    };
    let limit = code
        .line_start(line + CHAIN_LINES + 1)
        .unwrap_or(code.text.len());
    code.links_at(position)
        .iter()
        .filter(|link| {
            link.end <= limit
                && &code.text[link.at..link.end] == member
                && link.segment.as_deref() == segment
        })
        .map(|link| (link.at, link.chain.clone()))
        .collect()
}

/// A later link of a Dart call chain (upstream #2376 `isDartChainLink`, and
/// `matchDartChainLink`): `Some` with what it links, or nothing, when the
/// reference is one; `None` when it is not. A bare `calls` reference is one
/// unless it is written without a receiver; KEEP-RUST: `s.m` is one when `s`
/// is itself written after a `.`.
pub fn dart_chain_link(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if reference.language != Language::Dart
        || reference.reference_kind != EdgeKind::Calls
        || reference.is_function_ref
    {
        return None;
    }
    let (_, member) = link_parts(&reference.reference_name)?;
    let code = context.source_facts(&reference.file_path)?.dart_code();
    let sites = chain_link_sites(
        &code,
        &reference.reference_name,
        reference.line,
        reference.column,
    )?;
    Some(match_link(reference, context, member, &sites))
}

/// What a chain link links: the member of what each of its sites' chains
/// evaluates to, when they all agree.
fn match_link(
    reference: &RefView,
    context: &dyn ResolutionContext,
    member: &str,
    sites: &[(usize, Chain)],
) -> Option<ResolvedRef> {
    if !context
        .get_nodes_by_name_shared(member)
        .iter()
        .any(|node| node.language == Language::Dart)
    {
        return None;
    }
    let typing = Typing {
        context,
        name: member,
    };
    let mut chosen: Option<Option<(String, ResolvedBy)>> = None;
    for (at, chain) in sites {
        let site = Site {
            view: reference.clone(),
            at: Some(*at),
        };
        let target = typing.type_chain(chain, &site, 0).and_then(|receiver| {
            if receiver.is_static {
                // Static members are the type's own: `p.Provider.autoDispose(…)`
                // calls the constant, as `Provider.autoDispose(…)` does.
                let found = typing.static_member(
                    &receiver.type_name,
                    member,
                    &site,
                    receiver.owner.as_deref(),
                )?;
                (found.kind != NodeKind::EnumMember)
                    .then(|| (found.id.clone(), ResolvedBy::QualifiedName))
            } else {
                let (found, _) = dart_member_of(
                    &receiver.type_name,
                    member,
                    reference,
                    context,
                    &|node| !is_setter(node, context),
                    u64::from(receiver.via_super),
                )?;
                Some((found.id.clone(), ResolvedBy::InstanceMethod))
            }
        });
        match &chosen {
            None => chosen = Some(target),
            Some(earlier) if *earlier == target => {}
            Some(_) => return None,
        }
    }
    let (target_node_id, resolved_by) = chosen.flatten()?;
    Some(ResolvedRef::new(
        reference.clone(),
        target_node_id,
        0.85,
        resolved_by,
    ))
}

/// Whether a constructor call written at `column` of 1-based `line` names
/// `name` through an import prefix: `new p.Foo()`, `const p.Foo()`, which
/// the extractor records as `Foo` where its `new` or `const` is.
fn constructed_through_prefix(code: &DartCode, name: &str, line: i64, column: usize) -> bool {
    let Some(text) = code.line_text(line) else {
        return false;
    };
    let rest = text.get(column..).unwrap_or("");
    let Some(rest) = rest
        .strip_prefix("const")
        .or_else(|| rest.strip_prefix("new"))
        .filter(|after| after.starts_with(|c: char| c.is_ascii_whitespace()))
    else {
        return false;
    };
    let rest = rest.trim_start();
    let prefix_len = rest.bytes().take_while(|byte| is_word_byte(*byte)).count();
    if prefix_len == 0 {
        return false;
    }
    let after = rest[prefix_len..].trim_start();
    let Some(after) = after.strip_prefix('.') else {
        return false;
    };
    let after = after.trim_start();
    after.starts_with(name)
        && !after
            .as_bytes()
            .get(name.len())
            .is_some_and(|byte| is_word_byte(*byte))
}

/// How a bare Dart call reference reads where the extractor placed it.
enum BareCall {
    /// Written without a receiver.
    ReceiverLess,
    /// A constructor called through an import prefix (`const p.Foo()`,
    /// placed at its `const`): it has a receiver, and is no chain link.
    ThroughPrefix,
    /// A later link of a chain, at the sites the extractor recorded it at
    /// (none when the chain cannot be read).
    Link(Vec<(usize, Chain)>),
}

/// How a bare Dart call reference reads (upstream `isReceiverLessDartCall`):
/// written without a receiver when its name is where it is placed and no `.`
/// is written before it there (a spread's `...items()` is none). KEEP-RUST:
/// the extractor places a chain link's reference where the chain starts, so
/// one placed elsewhere has a receiver when it is a link the extractor
/// recorded there ([`link_sites`]), or a constructor called through an
/// import prefix; else when the first whole word of its name after that
/// place, on its line or the first line below holding one, is written after
/// a `.`.
fn bare_call(code: &DartCode, name: &str, line: i64, column: i64) -> BareCall {
    let Some(text) = code.line_text(line) else {
        return BareCall::ReceiverLess;
    };
    let position = usize::try_from(column.max(0)).unwrap_or(0);
    let bytes = text.as_bytes();
    let word_at = |at: usize| {
        text.get(at..).is_some_and(|rest| rest.starts_with(name))
            && !(at > 0 && is_word_byte(bytes[at - 1]))
            && !bytes.get(at + name.len()).is_some_and(|b| is_word_byte(*b))
    };
    let without_receiver = |text: &str, start: usize| {
        let before = text[..start].trim_end_matches(crate::dart_scope::is_js_space);
        if !before.ends_with('.') {
            return true;
        }
        before
            .strip_suffix("...")
            .is_some_and(|rest| !rest.ends_with('.'))
    };
    // Upstream places a reference on its name or just past it.
    let at_name = if word_at(position) {
        Some(position)
    } else if position >= name.len() && word_at(position - name.len()) {
        Some(position - name.len())
    } else {
        None
    };
    if let Some(at) = at_name {
        return if without_receiver(text, at) {
            BareCall::ReceiverLess
        } else {
            BareCall::Link(link_sites(code, line, column, name))
        };
    }
    if constructed_through_prefix(code, name, line, position) {
        return BareCall::ThroughPrefix;
    }
    let sites = link_sites(code, line, column, name);
    if !sites.is_empty() {
        return BareCall::Link(sites);
    }
    for offset in 0..=CHAIN_LINES {
        let Some(current) = code.line_text(line + offset) else {
            break;
        };
        let bytes = current.as_bytes();
        let from = if offset == 0 { position } else { 0 };
        let first = current.match_indices(name).map(|(at, _)| at).find(|at| {
            *at >= from
                && !(*at > 0 && is_word_byte(bytes[*at - 1]))
                && !bytes
                    .get(*at + name.len())
                    .is_some_and(|b| is_word_byte(*b))
        });
        if let Some(at) = first {
            return if without_receiver(current, at) {
                BareCall::ReceiverLess
            } else {
                BareCall::Link(Vec::new())
            };
        }
    }
    BareCall::ReceiverLess
}

/// Whether a bare Dart call reference is written without a receiver
/// ([`bare_call`]).
pub(crate) fn receiver_less_at(code: &DartCode, name: &str, line: i64, column: i64) -> bool {
    matches!(bare_call(code, name, line, column), BareCall::ReceiverLess)
}

/// Whether `segment.member` is written where the reference is placed: a call
/// through a plain name, which the extractor records as `segment.member`
/// where the name is.
fn written_through_name(
    code: &DartCode,
    line: i64,
    column: i64,
    segment: &str,
    member: &str,
) -> bool {
    let Some(text) = code.line_text(line) else {
        return false;
    };
    let at = usize::try_from(column.max(0)).unwrap_or(0);
    let Some(rest) = text.get(at..).and_then(|rest| rest.strip_prefix(segment)) else {
        return false;
    };
    if at > 0 && is_word_byte(text.as_bytes()[at - 1]) {
        return false;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('?').unwrap_or(rest);
    let Some(rest) = rest.strip_prefix('.') else {
        return false;
    };
    let rest = rest.trim_start();
    rest.starts_with(member)
        && !rest
            .as_bytes()
            .get(member.len())
            .is_some_and(|byte| is_word_byte(*byte))
}

/// The sites of a Dart call reference that is a later link of a chain, or
/// `None` when it is none (KEEP-RUST: read where the extractor placed it).
fn chain_link_sites(
    code: &DartCode,
    name: &str,
    line: i64,
    column: i64,
) -> Option<Vec<(usize, Chain)>> {
    match link_parts(name)? {
        (None, _) => match bare_call(code, name, line, column) {
            BareCall::Link(sites) => Some(sites),
            BareCall::ReceiverLess | BareCall::ThroughPrefix => None,
        },
        (Some(segment), member) => {
            if written_through_name(code, line, column, segment, member) {
                return None;
            }
            let sites = link_sites(code, line, column, name);
            (!sites.is_empty()).then_some(sites)
        }
    }
}

/// Whether a Dart call reference of the file whose code is `code` is a later
/// link of a chain, read from the text alone (a sync selecting the references
/// a changed declaration may move), as [`dart_chain_link`] tells one.
pub fn is_chain_link_in(code: &DartCode, name: &str, line: i64, column: i64) -> bool {
    chain_link_sites(code, name, line, column).is_some()
}

/// The offset of the `}` closing the `{` at `open`, or `to` when none does.
fn matching_brace(bytes: &[u8], open: usize, to: usize) -> usize {
    let mut depth = 0usize;
    let mut k = open;
    while k < to {
        match bytes[k] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
        k += 1;
    }
    to
}

/// The offset of the `)` closing the `(` at `open`, or `to` when none does.
fn matching_paren(bytes: &[u8], open: usize, to: usize) -> usize {
    let mut depth = 0usize;
    let mut k = open;
    while k < to {
        match bytes[k] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
        k += 1;
    }
    to
}

/// The declarations written between `from` and `to` of a Dart file's code
/// (W4-DART-03b): a class body's members, or the file's top level, each with
/// its whitespace collapsed and what no chain's typing reads of it dropped: a
/// block (`{…}` of a method, a closure, a nested class, a map) shows as `{}`,
/// an argument or parameter list as `()`, a function's arrow body as `=> ;`.
/// What a typing reads is left: a field's or constant's type and the chain
/// its initializer ends with, a method's head, a getter's or setter's
/// keyword, a type's head.
pub fn dart_statements(text: &str, from: usize, to: usize) -> Vec<String> {
    let bytes = text.as_bytes();
    let to = to.min(bytes.len());
    let mut out = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, out: &mut Vec<String>| {
        let statement = current.split_whitespace().collect::<Vec<_>>().join(" ");
        if !statement.is_empty() {
            out.push(statement);
        }
        current.clear();
    };
    let mut parens = 0usize;
    // Whether the declaration assigns, so an `=>` after it is a closure's.
    let mut assigned = false;
    let mut k = from;
    while k < to {
        let c = bytes[k];
        let prev = current.trim_end().bytes().next_back();
        match c {
            b'{' => {
                let close = matching_brace(bytes, k, to);
                current.push_str("{}");
                k = close + 1;
                if parens == 0 {
                    // A body ends the declaration; what follows a literal or a
                    // closure (`;`, `,`, `.`, `)`) goes on with it.
                    let next = bytes[k.min(to)..to]
                        .iter()
                        .copied()
                        .find(|byte| !is_space(*byte));
                    if !matches!(
                        next,
                        Some(b';' | b',' | b'.' | b'?' | b')' | b']' | b'[' | b'(')
                    ) {
                        flush(&mut current, &mut out);
                        assigned = false;
                    }
                }
                continue;
            }
            // An argument or parameter list: `name(…)`, `name<T>(…)`.
            b'(' if prev.is_some_and(|prev| is_word_byte(prev) || prev == b'>') => {
                current.push_str("()");
                k = matching_paren(bytes, k, to) + 1;
                continue;
            }
            b'=' if parens == 0 && bytes.get(k + 1) == Some(&b'>') && !assigned => {
                // A function's body: up to the `;` that ends it.
                let mut depth = 0i64;
                let mut j = k + 2;
                while j < to {
                    match bytes[j] {
                        b'(' | b'[' | b'{' => depth += 1,
                        b')' | b']' | b'}' => depth -= 1,
                        b';' if depth <= 0 => break,
                        _ => {}
                    }
                    j += 1;
                }
                current.push_str("=> ;");
                flush(&mut current, &mut out);
                assigned = false;
                k = j + 1;
                continue;
            }
            b'=' if parens == 0 => {
                let before = k.checked_sub(1).map(|p| bytes[p]);
                let next = bytes.get(k + 1).copied();
                if !matches!(before, Some(b'=' | b'!' | b'<' | b'>'))
                    && !matches!(next, Some(b'=' | b'>'))
                {
                    assigned = true;
                }
            }
            b'(' | b'[' => parens += 1,
            b')' | b']' => parens = parens.saturating_sub(1),
            b';' if parens == 0 => {
                current.push(';');
                flush(&mut current, &mut out);
                assigned = false;
                k += 1;
                continue;
            }
            _ => {}
        }
        let len = char_len(bytes, k).min(to - k);
        current.push_str(text.get(k..k + len).unwrap_or(" "));
        k += len;
    }
    flush(&mut current, &mut out);
    out
}

/// Dart's reserved words: no declaration is named by one, so no typing looks
/// one up.
const RESERVED_WORDS: &[&str] = &[
    "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else",
    "enum", "extends", "false", "final", "finally", "for", "if", "in", "is", "new", "null",
    "rethrow", "return", "super", "switch", "this", "throw", "true", "try", "var", "void", "while",
    "with",
];

/// Whether `word` (a [`dart_words`] word) can name a declaration: no reserved
/// word or number does.
pub fn can_name_a_declaration(word: &str) -> bool {
    !word.starts_with(|c: char| c.is_ascii_digit()) && !RESERVED_WORDS.contains(&word)
}

/// Words that open a Dart declaration without naming it.
const MODIFIERS: &[&str] = &[
    "abstract",
    "base",
    "final",
    "sealed",
    "interface",
    "external",
    "static",
    "const",
    "late",
    "var",
    "covariant",
    "factory",
    "augment",
];

/// Where a Dart declaration statement's annotations end: `@override`,
/// `@Riverpod(keepAlive: true)`.
fn annotations_end(statement: &str) -> usize {
    let bytes = statement.as_bytes();
    let mut k = 0usize;
    loop {
        while k < bytes.len() && is_space(bytes[k]) {
            k += 1;
        }
        if bytes.get(k) != Some(&b'@') {
            break;
        }
        k += 1;
        while k < bytes.len() && (is_word_byte(bytes[k]) || bytes[k] == b'.') {
            k += 1;
        }
        while k < bytes.len() && is_space(bytes[k]) {
            k += 1;
        }
        if bytes.get(k) == Some(&b'(') {
            let mut depth = 0usize;
            while k < bytes.len() {
                match bytes[k] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
        }
    }
    k
}

/// The words of a Dart declaration statement ([`dart_statements`]) that say
/// the type of `name`, which it declares, as a chain's typing reads them, and
/// whether no type is written for it (W4-DART-03b): a type's whole head;
/// nothing of a constructor of `class` (it makes a `class`); else the words
/// written before the name (a field's or variable's type, a method's or
/// getter's return type) or, with no type among them, the initializer's.
/// The whole statement when the name is not found before its parameters,
/// its initializer or its body.
pub fn dart_type_words<'s>(
    statement: &'s str,
    name: &str,
    class: Option<&str>,
) -> (Vec<&'s str>, bool) {
    let rest = &statement[annotations_end(statement).min(statement.len())..];
    let bytes = rest.as_bytes();
    let mut spans: Vec<(usize, &str)> = Vec::new();
    let mut k = 0usize;
    while k < bytes.len() {
        if !is_word_byte(bytes[k]) {
            k += 1;
            continue;
        }
        let start = k;
        while k < bytes.len() && is_word_byte(bytes[k]) {
            k += 1;
        }
        spans.push((start, &rest[start..k]));
    }
    let all = || spans.iter().map(|(_, word)| *word).collect::<Vec<_>>();
    if let Some((at, lead)) = spans.iter().find(|(_, word)| !MODIFIERS.contains(word)) {
        if matches!(*lead, "class" | "mixin" | "enum" | "extension" | "typedef") {
            return (all(), false);
        }
        if class == Some(*lead) && rest[at + lead.len()..].trim_start().starts_with(['(', '.']) {
            return (Vec::new(), false);
        }
    }
    // Where the parameters, the initializer or the body start.
    let end = bytes
        .iter()
        .position(|byte| matches!(byte, b'(' | b'=' | b';' | b'{'))
        .unwrap_or(bytes.len());
    let Some(&(name_at, _)) = spans.iter().rfind(|(at, word)| *at < end && *word == name) else {
        return (all(), true);
    };
    let written: Vec<&str> = spans
        .iter()
        .filter(|(at, word)| {
            *at < name_at && !MODIFIERS.contains(word) && !NON_TYPE_WORDS.contains(word)
        })
        .map(|(_, word)| *word)
        .collect();
    if !written.is_empty() {
        return (written, false);
    }
    // No type written: the initializer's, past its `=`.
    let mut depth = 0i64;
    for (at, byte) in bytes.iter().enumerate().skip(name_at) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = at.checked_sub(1).map(|p| bytes[p]);
                let next = bytes.get(at + 1).copied();
                if !matches!(prev, Some(b'=' | b'!' | b'<' | b'>'))
                    && !matches!(next, Some(b'=' | b'>'))
                {
                    return (dart_words(&rest[at + 1..]).collect(), true);
                }
            }
            _ => {}
        }
    }
    (Vec::new(), true)
}

/// The names a Dart declaration statement ([`dart_statements`]) declares: a
/// type's, a function's, method's, getter's, setter's or named
/// constructor's, or each variable's of a field or constant declaration.
pub fn dart_declared_names(statement: &str) -> Vec<String> {
    let k = annotations_end(statement);
    let rest = &statement[k.min(statement.len())..];
    let words: Vec<&str> = rest
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .filter(|word| !word.is_empty())
        .collect();
    let mut lead = words.iter().skip_while(|word| MODIFIERS.contains(word));
    if let Some(first) = lead.next() {
        match *first {
            "class" | "mixin" | "enum" | "typedef" => {
                return lead
                    .find(|word| !matches!(**word, "class"))
                    .map(|word| vec![(*word).to_string()])
                    .unwrap_or_default();
            }
            "extension" => {
                return lead
                    .find(|word| !matches!(**word, "type"))
                    .filter(|word| **word != "on")
                    .map(|word| vec![(*word).to_string()])
                    .unwrap_or_default();
            }
            "import" | "export" | "part" | "library" => return Vec::new(),
            _ => {}
        }
    }
    let rest_bytes = rest.as_bytes();
    // Where the first `(` and the first assignment `=` sit outside any
    // bracket; a function type's (`void Function() onDone;`) opens no
    // parameters of the declaration.
    let mut depth = 0i64;
    let mut paren: Option<usize> = None;
    let mut assign: Option<usize> = None;
    for (at, byte) in rest_bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => {
                if depth == 0
                    && *byte == b'('
                    && paren.is_none()
                    && !rest[..at].trim_end().ends_with("Function")
                {
                    paren = Some(at);
                }
                depth += 1;
            }
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 && assign.is_none() => {
                let prev = at.checked_sub(1).map(|p| rest_bytes[p]);
                let next = rest_bytes.get(at + 1).copied();
                if !matches!(prev, Some(b'=' | b'!' | b'<' | b'>'))
                    && !matches!(next, Some(b'=' | b'>'))
                {
                    assign = Some(at);
                }
            }
            _ => {}
        }
    }
    let word_after = |keyword: &str| -> Option<String> {
        let at = words.iter().position(|word| *word == keyword)?;
        words.get(at + 1).map(|word| (*word).to_string())
    };
    if assign.is_none_or(|assign| paren.is_some_and(|paren| paren < assign)) {
        if let Some(name) = word_after("get").or_else(|| word_after("set")) {
            return vec![name];
        }
        if let Some(paren) = paren {
            if words.contains(&"operator") {
                return Vec::new();
            }
            // The name before the parameters, past its type parameters.
            let mut end = paren;
            while end > 0 && is_space(rest_bytes[end - 1]) {
                end -= 1;
            }
            if end > 0
                && rest_bytes[end - 1] == b'>'
                && let Some(open) = opening(rest_bytes, end - 1, b'<', b'>', 400)
            {
                end = open;
                while end > 0 && is_space(rest_bytes[end - 1]) {
                    end -= 1;
                }
            }
            let mut start = end;
            while start > 0 && is_word_byte(rest_bytes[start - 1]) {
                start -= 1;
            }
            let name = &rest[start..end];
            return if is_dart_identifier(name) {
                vec![name.to_string()]
            } else {
                Vec::new()
            };
        }
    }
    // Variables: each declarator's name, before its `=` or its end.
    let body = rest.trim_end().trim_end_matches(';');
    let body_bytes = body.as_bytes();
    let mut names = Vec::new();
    let mut depth = 0i64;
    let mut angle = 0i64;
    let mut initializing = false;
    let mut last_word: Option<(usize, usize)> = None;
    let push = |last_word: Option<(usize, usize)>, names: &mut Vec<String>| {
        if let Some((start, end)) = last_word
            && let Some(name) = body.get(start..end)
            && is_dart_identifier(name)
            && !MODIFIERS.contains(&name)
        {
            names.push(name.to_string());
        }
    };
    let mut k = 0usize;
    while k < body_bytes.len() {
        let byte = body_bytes[k];
        // An initializer's type arguments (`Provider<A, B>()`, `<K, V>{}`)
        // hold commas no declarator ends at.
        if initializing
            && byte == b'<'
            && let Some(close) = literal_type_arguments_end(body_bytes, k)
        {
            k = close + 1;
            continue;
        }
        if is_word_byte(byte) && !initializing && depth == 0 && angle == 0 {
            let start = k;
            while k < body_bytes.len() && is_word_byte(body_bytes[k]) {
                k += 1;
            }
            last_word = Some((start, k));
            continue;
        }
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'<' if !initializing && depth == 0 => angle += 1,
            b'>' if !initializing && depth == 0 && angle > 0 => angle -= 1,
            b'=' if depth == 0 && angle == 0 && !initializing => {
                push(last_word.take(), &mut names);
                initializing = true;
            }
            b',' if depth == 0 && angle == 0 => {
                if !initializing {
                    push(last_word.take(), &mut names);
                }
                initializing = false;
                last_word = None;
            }
            _ => {}
        }
        k += 1;
    }
    if !initializing {
        push(last_word, &mut names);
    }
    names
}

/// The `>` closing type arguments opened at `open` in an expression, one a
/// call, a member, a literal (`<K, V>{}`, `<T>[]`) or another `>` follows.
fn literal_type_arguments_end(text: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut k = open;
    while k < text.len() && k - open < 400 {
        match text[k] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    let first = text[k + 1..].iter().copied().find(|byte| !is_space(*byte));
                    return matches!(first, Some(b'(' | b'.' | b'>' | b'?' | b'{' | b'['))
                        .then_some(k);
                }
            }
            b';' | b'{' | b'}' | b'=' => return None,
            _ => {}
        }
        k += 1;
    }
    None
}

/// The words of a Dart text (`[A-Za-z0-9_$]` runs), in order.
pub fn dart_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
        .filter(|word| !word.is_empty())
}

/// The statements of the body of the Dart type declared by `node` in the file
/// whose code is `code` ([`dart_statements`]), none for one without a body.
pub fn dart_class_statements(code: &DartCode, node: &Node) -> Vec<String> {
    let Some(from) = code.line_start(node.start_line) else {
        return Vec::new();
    };
    let to = code
        .line_start(node.end_line + 1)
        .unwrap_or(code.text.len());
    let bytes = code.text.as_bytes();
    let mut parens = 0usize;
    let mut k = from + usize::try_from(node.start_column.max(0)).unwrap_or(0);
    while k < to {
        match bytes[k] {
            b'(' | b'[' => parens += 1,
            b')' | b']' => parens = parens.saturating_sub(1),
            b'{' if parens == 0 => {
                let close = matching_brace(bytes, k, to);
                return dart_statements(&code.text, k + 1, close);
            }
            b';' if parens == 0 => return Vec::new(),
            _ => {}
        }
        k += 1;
    }
    Vec::new()
}

/// The names a chain link calling `member` at `line` / `column` is recorded
/// as where a chain starting there links one of its calls ([`link_sites`]):
/// bare, or past the segment written before it.
pub fn chain_link_names(code: &DartCode, member: &str, line: i64, column: i64) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Some(position) = code.offset(line, column) else {
        return names;
    };
    let limit = code
        .line_start(line + CHAIN_LINES + 1)
        .unwrap_or(code.text.len());
    for link in code.links_at(position) {
        if link.end > limit || code.text[link.at..link.end] != *member {
            continue;
        }
        let name = match &link.segment {
            Some(segment) => format!("{segment}.{member}"),
            None => member.to_string(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// What the typing of a later link of a Dart call chain reads by name
/// (W4-DART-03b): what it looks a declaration up by, and what it asks only
/// whether a type is named ([`chain_link_reads`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChainReads {
    /// The names a typing looks a declaration up by: the member each link
    /// calls or reads, a type argument, a function or constructor the head
    /// calls, a name it reads that no local or parameter declares (a field,
    /// a getter, a constant, a type), a cast's type, a local's declared type,
    /// the type the chain is written in, and so on through what a local's
    /// initializer reads.
    pub names: BTreeSet<String>,
    /// The locals and parameters a chain's head names: the typing asks only
    /// whether a type of the name exists, which would take their place.
    pub types: BTreeSet<String>,
}

/// What the typing of a Dart call recorded as `name` at `line` / `column` of
/// the file whose code is `code` and whose nodes are `nodes` reads by name,
/// when it is a later link of a chain (`None` otherwise, or when no chain is
/// read where the extractor recorded it, which links nothing whatever
/// changes). Read as [`Typing`] reads it: the chain from its head to each
/// site of the link, the type it is written in (what `this`, `super` and a
/// receiver-less head are typed by), and for a name the code before the
/// chain declares as a local or parameter ([`local_declaration`], in the
/// function around it, else the file) that declaration's type or its
/// initializer's reading in turn, as deep as the typing reads; another
/// file's declarations only by the names read, the `call` a called value
/// runs included.
pub fn chain_link_reads(
    code: &DartCode,
    nodes: &[Node],
    name: &str,
    line: i64,
    column: i64,
) -> Option<ChainReads> {
    let (_, member) = link_parts(name)?;
    let sites = chain_link_sites(code, name, line, column)?;
    if sites.is_empty() {
        return None;
    }
    let scope_line = crate::name_matcher::scope_start_line_in(nodes.iter(), Language::Dart, line);
    let start = code.line_start(scope_line).unwrap_or(0);
    let mut reads = ChainReads::default();
    reads.names.insert(member.to_string());
    reads.names.extend(
        nodes
            .iter()
            .filter(|node| {
                node.language == Language::Dart
                    && is_dart_type_kind(node.kind)
                    && node.start_line <= line
                    && node.end_line.max(node.start_line) >= line
            })
            .map(|node| node.name.clone()),
    );
    for (at, chain) in &sites {
        let mut reader = ChainReader {
            code,
            start,
            at: *at,
            reads: &mut reads,
            visited: HashSet::new(),
        };
        reader.chain(chain, 0);
    }
    Some(reads)
}

/// [`chain_link_reads`] at one site of a link: its scope starts at `start`,
/// and a local is declared before `at`.
struct ChainReader<'r> {
    code: &'r DartCode,
    start: usize,
    at: usize,
    reads: &'r mut ChainReads,
    visited: HashSet<(String, u32)>,
}

impl ChainReader<'_> {
    /// [`Typing::type_chain`]'s reading, one level deeper than it reads.
    fn chain(&mut self, chain: &Chain, depth: u32) {
        if depth > MAX_DEPTH + 1 {
            return;
        }
        for link in &chain.links {
            if !link.name.is_empty() {
                self.reads.names.insert(link.name.clone());
            }
            self.type_arguments(&link.type_args);
        }
        match &chain.head {
            Head::String => {}
            Head::Paren { open, close } => self.paren(*open, *close, depth),
            Head::Name {
                name,
                type_args,
                call,
                keyword,
                ..
            } => {
                self.type_arguments(type_args);
                if name == "this" || name == "super" || keyword == "is" {
                    return;
                }
                if matches!(keyword.as_str(), "const" | "new" | "as") {
                    self.reads.names.insert(name.clone());
                    return;
                }
                // An import prefix names a library and the head is the link
                // after it, read as a head too.
                if !*call
                    && type_args.is_empty()
                    && let Some(first) = chain.links.first()
                    && !first.index
                {
                    self.variable(&first.name, depth);
                }
                if *call {
                    // A function or type by name, else a variable's `call`.
                    self.reads.names.insert(name.clone());
                    self.variable(name, depth);
                } else if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                    // A type, or a constant written in capitals.
                    self.reads.names.insert(name.clone());
                    self.variable(name, depth);
                } else {
                    self.reads.types.insert(name.clone());
                    self.variable(name, depth);
                }
            }
        }
    }

    fn type_arguments(&mut self, type_args: &[String]) {
        for arg in type_args {
            self.reads.names.extend(dart_words(arg).map(str::to_string));
        }
    }

    /// [`Typing::variable_type`]'s reading of `name`: a local or parameter
    /// the scope declares, else a field, getter or constant by name.
    fn variable(&mut self, name: &str, depth: u32) {
        if !self.visited.insert((name.to_string(), depth)) {
            return;
        }
        let uses = self.code.name_uses(name);
        match local_declaration(self.code, &uses, self.start, self.at, name) {
            None => {
                self.reads.names.insert(name.to_string());
            }
            Some(LocalDeclaration::Typed(typed)) => {
                self.reads.names.insert(typed);
            }
            Some(LocalDeclaration::Untyped) => {}
            Some(LocalDeclaration::Initializer(from)) => {
                if let Some(end) = initializer_end(&self.code.text, from) {
                    self.expression(end, depth);
                }
            }
        }
    }

    /// [`Typing::expression_value`]'s reading of the expression ending at
    /// `end`.
    fn expression(&mut self, end: usize, depth: u32) {
        let Some(chain) = read_chain(&self.code.text, end) else {
            return;
        };
        if matches!(&chain.head, Head::Name { keyword, .. } if keyword == "await") {
            return;
        }
        self.chain(&chain, depth + 1);
    }

    /// [`Typing::type_paren`]'s reading: a cast's type, else the last
    /// operand.
    fn paren(&mut self, open: usize, close: usize, depth: u32) {
        let Some(inner) = self.code.text.get(open + 1..close) else {
            return;
        };
        if let Some(cast) = dart_cast(inner) {
            self.reads
                .names
                .extend(dart_words(cast).map(str::to_string));
            return;
        }
        let bytes = self.code.text.as_bytes();
        let Some(mut end) = close.checked_sub(1) else {
            return;
        };
        while end > open && is_space(bytes[end]) {
            end -= 1;
        }
        self.expression(end, depth);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanking_keeps_offsets_quotes_and_interpolated_code() {
        let source =
            "var a = 'x.y()'; // c.d()\nvar b = \"${c.e()}\"; /* f.g() */ r'\\n';\n'''é\n''';";
        let blanked = blank_dart_code(source);
        assert_eq!(blanked.len(), source.len());
        assert_eq!(
            blanked,
            "var a = '     ';         \nvar b = \"  c.e() \";             r'  ';\n'''  \n''';"
        );
    }

    #[test]
    fn a_name_use_is_a_whole_word_not_a_member() {
        let text = "x = foo; a.foo; foo2; _foo; foo(";
        assert_eq!(name_uses(text, "foo"), vec![4, 28]);
    }

    fn chain_of(text: &str) -> Chain {
        read_chain(text, text.len() - 1).expect("a chain")
    }

    #[test]
    fn a_chain_reads_back_its_links_and_head() {
        let chain = chain_of("return events\n    .map(mapper)\n    .transform<dynamic>(x)");
        assert_eq!(
            chain
                .links
                .iter()
                .map(|l| l.name.as_str())
                .collect::<Vec<_>>(),
            ["map", "transform"]
        );
        assert!(chain.links[1].call);
        assert_eq!(chain.links[1].type_args, ["dynamic"]);
        assert!(
            matches!(&chain.head, Head::Name { name, keyword, .. } if name == "events" && keyword == "return")
        );
        let chain = chain_of("  final c = const SignUpState().withEmail('a')");
        assert!(
            matches!(&chain.head, Head::Name { name, keyword, call: true, keyword_start, .. } if name == "SignUpState" && keyword == "const" && *keyword_start == 12)
        );
        assert_eq!(chain.start(), Some(12));
        let chain = chain_of("(state.initialTodo ?? const Todo()).copyWith(title: 'x')");
        assert!(matches!(chain.head, Head::Paren { open: 0, .. }));
        assert_eq!(chain.links.len(), 1);
        let chain = chain_of("x!.a?.b[0].c");
        assert_eq!(chain.links.len(), 4);
        assert!(chain.links[2].index);
        assert!(read_chain("obj..a().b", 9).is_none());
    }

    #[test]
    fn a_simple_type_name_drops_prefix_arguments_and_nullability() {
        assert_eq!(simple_type_name("p.Foo<Bar>?").as_deref(), Some("Foo"));
        assert_eq!(simple_type_name(" Todo? ").as_deref(), Some("Todo"));
        assert_eq!(simple_type_name("Function(int)"), None);
        assert_eq!(simple_type_name("Map<String, int>").as_deref(), Some("Map"));
    }

    #[test]
    fn a_body_is_cut_into_its_declarations() {
        let text = "  final Todo? initialTodo;\n  @override\n  void add(E e) {\n    go();\n  }\n  static const auto = Builder();\n  final p = Provider((ref) {\n    return 1;\n  }).select(1);\n  int get x => 1;\n  final m = {1: 2};\n  Repo make() => Repo(a, (b) => c);\n  final f = (x) => x.y;\n  final c = (a ?? b).copyWith(t: 1);\n";
        assert_eq!(
            dart_statements(text, 0, text.len()),
            [
                "final Todo? initialTodo;",
                "@override void add() {}",
                "static const auto = Builder();",
                "final p = Provider().select();",
                "int get x => ;",
                "final m = {};",
                "Repo make() => ;",
                "final f = (x) => x.y;",
                "final c = (a ?? b).copyWith();",
            ]
        );
    }

    #[test]
    fn a_declaration_names_what_it_declares() {
        let names = |statement: &str| dart_declared_names(statement);
        assert_eq!(names("final Todo? initialTodo;"), ["initialTodo"]);
        assert_eq!(names("@override void add(E e) {}"), ["add"]);
        assert_eq!(names("static const auto = Builder();"), ["auto"]);
        assert_eq!(names("final Map<String, int> a = {}, b;"), ["a", "b"]);
        assert_eq!(names("int get x => 1;"), ["x"]);
        assert_eq!(names("set value(int v) {}"), ["value"]);
        assert_eq!(names("T read<T>(Object p) => throw 0;"), ["read"]);
        assert_eq!(names("const Foo.named();"), ["named"]);
        assert_eq!(names("factory Foo.from(Map m) = _Impl;"), ["from"]);
        assert_eq!(
            names("@Riverpod(keepAlive: true) Repo repo(Ref ref) {}"),
            ["repo"]
        );
        assert_eq!(
            names("bool operator ==(Object o) => true;"),
            Vec::<String>::new()
        );
        assert_eq!(names("abstract class Foo<T> extends Bar {}"), ["Foo"]);
        assert_eq!(names("extension on Shape {}"), Vec::<String>::new());
        assert_eq!(names("import '    ';"), Vec::<String>::new());
        assert_eq!(names("late final x;"), ["x"]);
        assert_eq!(
            names("final todos = StateNotifierProvider<Todos, List<Todo>>();"),
            ["todos"]
        );
        assert_eq!(
            names("final calls = <String, void Function()>{};"),
            ["calls"]
        );
        assert_eq!(names("final a = b < c, d = 2;"), ["a", "d"]);
        // A function type's parameters are none of the declaration's.
        assert_eq!(names("final void Function() onDispose;"), ["onDispose"]);
        assert_eq!(names("final void Function()? onError;"), ["onError"]);
        assert_eq!(names("void Function() callback() => ;"), ["callback"]);
        assert_eq!(names("typedef Create<T> = T Function();"), ["Create"]);
    }

    #[test]
    fn a_declaration_says_its_type_where_the_typing_reads_it() {
        let words = |statement: &str, name: &str, class: Option<&str>| {
            let (words, untyped) = dart_type_words(statement, name, class);
            (words.join(" "), untyped)
        };
        // A type's whole head.
        assert_eq!(
            words("abstract class Foo<T> extends Bar<T> {}", "Foo", None),
            ("abstract class Foo T extends Bar T".to_string(), false)
        );
        // A constructor makes its class.
        assert_eq!(
            words("Foo.from() : parent = node.parent;", "from", Some("Foo")),
            (String::new(), false)
        );
        assert_eq!(
            words("const Foo._() : super._();", "_", Some("Foo")),
            (String::new(), false)
        );
        // A method returning its class is no constructor.
        assert_eq!(
            words("Foo copy() => ;", "copy", Some("Foo")),
            ("Foo".to_string(), false)
        );
        // What is written before the name, else the initializer.
        assert_eq!(
            words(
                "@override ProviderContainer get container => ;",
                "container",
                Some("Ref")
            ),
            ("ProviderContainer".to_string(), false)
        );
        assert_eq!(
            words(
                "late final List<ContainerId> parentIds = container.parents .map();",
                "parentIds",
                None
            ),
            ("List ContainerId".to_string(), false)
        );
        assert_eq!(
            words("static const auto = Builder();", "auto", None),
            ("Builder".to_string(), true)
        );
        assert_eq!(
            words(
                "@ProviderFor() final fooProvider = FooProvider._();",
                "fooProvider",
                None
            ),
            ("FooProvider _".to_string(), true)
        );
        assert_eq!(words("get bar => ;", "bar", None), (String::new(), true));
        assert_eq!(
            words("final void Function() onDispose;", "onDispose", None),
            ("final void Function onDispose".to_string(), true)
        );
    }

    fn dart_node(kind: NodeKind, name: &str, start_line: i64, end_line: i64) -> Node {
        Node {
            id: format!("{}:{name}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "lib/a.dart".to_string(),
            language: Language::Dart,
            start_line,
            end_line,
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

    #[test]
    fn a_chain_link_reads_its_type_and_the_declarations_its_names_name() {
        let code = DartCode::of(
            "class Child extends Base {\n  void f(Repo r) {\n    final a = Probe();\n    final b = Other();\n    this.go().stop();\n    b.make().stop();\n    b.make(Arg()).stop();\n    b.make<Typed>().stop();\n    r.load().stop();\n    field.make().stop();\n    final c = b.make();\n    c.go().stop();\n  }\n}\n",
        );
        let nodes = [
            dart_node(NodeKind::Class, "Child", 1, 14),
            dart_node(NodeKind::Method, "f", 2, 13),
        ];
        let reads = |line: i64| chain_link_reads(&code, &nodes, "stop", line, 4).expect("a link");
        let names = |line: i64| reads(line).names.into_iter().collect::<Vec<_>>();
        // `this` is typed by the type the chain is written in.
        assert_eq!(names(5), ["Child", "go", "stop"]);
        // A local is read through its declaration, not by its name; a call's
        // arguments are no part of what the chain is typed by, its type
        // arguments are.
        assert_eq!(names(6), ["Child", "Other", "make", "stop"]);
        assert_eq!(reads(6).types, BTreeSet::from(["b".to_string()]));
        assert_eq!(names(7), ["Child", "Other", "make", "stop"]);
        assert_eq!(names(8), ["Child", "Other", "Typed", "make", "stop"]);
        // A parameter by its declared type.
        assert_eq!(names(9), ["Child", "Repo", "load", "stop"]);
        // A name no local declares is looked up by it (a field, a getter).
        assert_eq!(names(10), ["Child", "field", "make", "stop"]);
        // A local's initializer is read on in turn.
        assert_eq!(names(12), ["Child", "Other", "go", "make", "stop"]);
        assert_eq!(
            reads(12).types,
            BTreeSet::from(["b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn a_link_is_found_where_the_extractor_recorded_it() {
        let code = DartCode::of(
            "void f() {\n  x.m12().m12().m12();\n  make().make();\n  this.a.m6();\n  y.go(z.go()).go();\n}\n",
        );
        let found = |line, column, name| {
            link_sites(&code, line, column, name)
                .into_iter()
                .map(|(at, _)| at)
                .collect::<Vec<_>>()
        };
        // `x.m12` is the first call's, recorded through `x`.
        assert_eq!(found(2, 2, "m12"), vec![21, 27]);
        assert_eq!(found(3, 2, "make"), vec![43]);
        assert_eq!(found(4, 2, "a.m6"), vec![60]);
        // The inner `z.go` is another chain's; only the outer link counts.
        assert_eq!(found(5, 2, "go"), vec![81]);
    }
}
