//! A name a Dart function, closure or block around a call declares — a
//! parameter, a local variable, a loop or `catch` variable, a pattern
//! variable — is what a bare call of that name means there (upstream #2379
//! `dart-local-scope.ts`).
//!
//! riverpod's `overrideWith(CreatedT Function(Ref ref, ArgT arg) create)`
//! calls its parameter, `create(ref, arg)`, and a bloc event handler's
//! `emit(…)` calls its `Emitter` parameter, never `Bloc.emit`. The index holds
//! no node for either, so such a call, or a function passed by name, links
//! nothing, whichever strategy chose a namesake. Read from the code of the
//! call's file:
//!
//! - a parameter list that a body follows (`{ … }` or `=> …`), the body being
//!   its scope: a function's, a method's, a closure's, a `catch` clause's, a
//!   constructor's (its initializer list too);
//! - the variables a `for` head declares, for the loop;
//! - a local a block declares (`final x = …`, `Foo? x;`, `var (a, b) = …`, a
//!   pattern's `final x`), from there to the block's end.
//!
//! Fields and top-level variables are not locals. The nearest binding around
//! the call decides: a local function declared there is a node, and the call
//! keeps what it resolved to.
//!
//! KEEP-RUST: offsets are bytes of the file (upstream's are UTF-16 code
//! units), and a Dart reference's column is where its name starts.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Language};
use regex::Regex;

use crate::csharp_types::js_regex;
use crate::dart_scope::{is_dart_identifier, is_dart_name_char, is_js_space};
use crate::types::{RefView, ResolutionContext, ResolvedRef};

/// Where a name is bound: byte offsets into the file's code, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scope {
    start: usize,
    end: usize,
    /// A local function's name, which a call can reach.
    function: bool,
}

/// Every parameter, loop variable and local a Dart file binds, with its
/// scope.
#[derive(Debug, Default)]
pub struct DartLocalScopes {
    names: HashMap<String, Vec<Scope>>,
}

impl DartLocalScopes {
    /// The scopes of the Dart file whose code, comments and string contents
    /// blanked ([`crate::dart_chains::blank_dart_code`]), is `code`.
    pub fn of(code: &str) -> Self {
        let code = blank_type_arguments(code);
        let brackets = match_brackets(code.as_bytes());
        let mut scopes = Self::default();
        let parameter_lists = bind_parameters(&code, &brackets, &mut scopes);
        bind_locals(&code, &brackets, &parameter_lists, &mut scopes);
        scopes
    }

    fn bind(&mut self, name: &str, start: usize, end: usize, function: bool) {
        if name == "_" || is_reserved(name) {
            return;
        }
        self.names.entry(name.to_string()).or_default().push(Scope {
            start,
            end,
            function,
        });
    }

    /// The nearest binding of `name` around byte offset `at`: scopes nest, so
    /// the one that starts last.
    fn nearest(&self, name: &str, at: usize) -> Option<Scope> {
        self.names
            .get(name)?
            .iter()
            .filter(|scope| scope.start <= at && at <= scope.end)
            .fold(None, |nearest: Option<Scope>, scope| match nearest {
                Some(nearest) if nearest.start >= scope.start => Some(nearest),
                _ => Some(*scope),
            })
    }
}

/// The resolved reference, or `None` when it is a receiver-less Dart call
/// (or a function passed by name) whose name a parameter or local around it
/// binds (upstream `gateDartLocal`).
pub(crate) fn gate_dart_local(
    resolved: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = resolved?;
    if reference.language != Language::Dart
        || !(reference.reference_kind == EdgeKind::Calls
            || (reference.reference_kind == EdgeKind::References && reference.is_function_ref))
        || !is_dart_identifier(&reference.reference_name)
    {
        return Some(result);
    }
    match dart_local_binding(reference, context) {
        Some(scope) if !scope.function => None,
        _ => Some(result),
    }
}

/// The nearest binding of a reference's name around where it is written, when
/// it is written without a receiver.
fn dart_local_binding(reference: &RefView, context: &dyn ResolutionContext) -> Option<Scope> {
    let name = reference.reference_name.as_str();
    let facts = context.source_facts(&reference.file_path)?;
    let scopes = facts.dart_local_scopes();
    if !scopes.names.contains_key(name) {
        return None;
    }
    // KEEP-RUST: the extractor records a chain's later links where the chain
    // starts, so whether a call has a receiver (`this.create()`, `x.create()`)
    // is read as the bare-call rules read it (W4-DART-01, W4-DART-03b), where
    // upstream reads a `.` before the name, on its line or ending the line
    // before. A spread's `...create()` is a call without one, as those rules
    // and upstream's `isReceiverLessDartCall` read it. A function value is
    // recorded at its name, and one written after a `.` with its receiver.
    if reference.reference_kind == EdgeKind::Calls
        && !crate::dart_scope::is_receiver_less_dart_call(reference, context)
    {
        return None;
    }
    let index = usize::try_from(reference.line - 1).ok()?;
    let lines = facts.lines();
    let line = lines.raw_line(facts.source(), index)?;
    let column = name_column(line, name, reference.column)?;
    scopes.nearest(name, lines.raw_line_start(index)? + column)
}

/// Where on its line a reference's name starts: at its column, or just
/// before it (upstream's column sits past the name), else the nearest
/// whole-word occurrence.
fn name_column(line: &str, name: &str, column: i64) -> Option<usize> {
    let bytes = line.as_bytes();
    let is_word = |at: usize| {
        line.get(at..).is_some_and(|rest| rest.starts_with(name))
            && !(at > 0 && is_name_byte(bytes[at - 1]))
            && !bytes.get(at + name.len()).is_some_and(|b| is_name_byte(*b))
    };
    if let Ok(at) = usize::try_from(column) {
        if is_word(at) {
            return Some(at);
        }
        if at >= name.len() && is_word(at - name.len()) {
            return Some(at - name.len());
        }
    }
    let distance = |at: usize| (i64::try_from(at).unwrap_or(i64::MAX) - column).abs();
    line.match_indices(name)
        .map(|(at, _)| at)
        .filter(|at| is_word(*at))
        .fold(None, |best: Option<usize>, at| match best {
            Some(best) if distance(best) <= distance(at) => Some(best),
            _ => Some(at),
        })
}

/// `[\w$]` for a byte.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// `[A-Za-z_$]` for a byte: where an identifier can start.
fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

/// Keywords that head a parenthesised expression, not a parameter list.
const CONTROL_HEADS: &[&str] = &["if", "while", "switch"];

/// Keywords a declaration's name can follow.
const DECLARATION_KEYWORDS: &[&str] = &["final", "var", "const"];

/// Dart's reserved words: never a name a declaration binds.
const DART_RESERVED: &[&str] = &[
    "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else",
    "enum", "extends", "false", "final", "finally", "for", "if", "in", "is", "new", "null",
    "rethrow", "return", "super", "switch", "this", "throw", "true", "try", "var", "void", "while",
    "with",
];

/// The built-in and contextual words that, with the reserved ones, never
/// name a declaration's type or a function there. Several are fine parameter
/// names (`R Function(DartType item) set`), so only [`DART_RESERVED`] keeps a
/// name from binding.
const DART_CONTEXTUAL: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "base",
    "covariant",
    "deferred",
    "export",
    "extension",
    "external",
    "factory",
    "get",
    "hide",
    "implements",
    "import",
    "interface",
    "late",
    "library",
    "mixin",
    "of",
    "on",
    "operator",
    "part",
    "required",
    "sealed",
    "set",
    "show",
    "static",
    "sync",
    "typedef",
    "when",
    "yield",
];

fn is_reserved(word: &str) -> bool {
    DART_RESERVED.contains(&word)
}

/// Upstream `DART_KEYWORDS`: the reserved words and the contextual ones.
fn is_keyword(word: &str) -> bool {
    is_reserved(word) || DART_CONTEXTUAL.contains(&word)
}

// ---------------------------------------------------------------- tokens

/// The word right before an offset (blanks skipped) and where it starts, or
/// else no word and where the character there starts (`None` at the start).
#[derive(Debug, Clone, Copy)]
struct Token<'c> {
    word: Option<&'c str>,
    at: Option<usize>,
}

/// The character ending right before byte `at`, with where it starts.
fn char_before(code: &str, at: usize) -> Option<(usize, char)> {
    code.get(..at)?.char_indices().next_back()
}

fn token_before(code: &str, at: usize) -> Token<'_> {
    let mut end = at;
    while let Some((start, c)) = char_before(code, end) {
        if !is_js_space(c) {
            break;
        }
        end = start;
    }
    let mut start = end;
    while let Some((at, c)) = char_before(code, start) {
        if !is_dart_name_char(c) {
            break;
        }
        start = at;
    }
    if start == end {
        return Token {
            word: None,
            at: char_before(code, end).map(|(at, _)| at),
        };
    }
    Token {
        word: Some(&code[start..end]),
        at: Some(start),
    }
}

/// The character starting at `at`, when there is one.
fn char_at(code: &str, at: Option<usize>) -> Option<char> {
    code.get(at?..)?.chars().next()
}

fn skip_space(code: &str, at: usize) -> usize {
    code.get(at..).map_or(at, |rest| {
        at + rest.len() - rest.trim_start_matches(is_js_space).len()
    })
}

/// The end of the identifier starting at byte `at` (`[A-Za-z_$][\w$]*`).
fn identifier_end(bytes: &[u8], at: usize) -> Option<usize> {
    if !is_name_start(*bytes.get(at)?) {
        return None;
    }
    Some(at + bytes[at..].iter().take_while(|b| is_name_byte(**b)).count())
}

// ---------------------------------------------------------------- brackets

/// Every bracket of the code, in order, with its partner and the open
/// bracket around it (indexes into `at`).
struct Brackets {
    at: Vec<usize>,
    partner: Vec<Option<usize>>,
    parent: Vec<Option<usize>>,
}

/// Pairs `()`, `[]` and `{}`; a stray close is left unpaired, and so are the
/// opens a close skips past.
fn match_brackets(code: &[u8]) -> Brackets {
    let (mut at, mut partner, mut parent) = (Vec::new(), Vec::new(), Vec::new());
    let mut stack: Vec<usize> = Vec::new();
    for (i, ch) in code.iter().enumerate() {
        let want = match ch {
            b'(' | b'[' | b'{' => {
                at.push(i);
                partner.push(None);
                parent.push(stack.last().copied());
                stack.push(at.len() - 1);
                continue;
            }
            b')' => b'(',
            b']' => b'[',
            b'}' => b'{',
            _ => continue,
        };
        let open = stack.iter().rposition(|k| code[at[*k]] == want);
        at.push(i);
        partner.push(None);
        let index = at.len() - 1;
        match open {
            None => parent.push(stack.last().copied()),
            Some(s) => {
                let open = stack[s];
                stack.truncate(s);
                partner[open] = Some(index);
                partner[index] = Some(open);
                parent.push(parent[open]);
            }
        }
    }
    Brackets {
        at,
        partner,
        parent,
    }
}

impl Brackets {
    /// The index of the first bracket at or after byte `from`.
    fn first_from(&self, from: usize) -> usize {
        self.at.partition_point(|at| *at < from)
    }

    /// The index of the bracket at byte `at`.
    fn index_at(&self, at: usize) -> Option<usize> {
        let k = self.first_from(at);
        (self.at.get(k) == Some(&at)).then_some(k)
    }

    /// The index of the innermost open bracket around byte `at`.
    fn innermost(&self, code: &[u8], at: usize) -> Option<usize> {
        let k = self.first_from(at).checked_sub(1)?;
        if matches!(code[self.at[k]], b'(' | b'[' | b'{') {
            Some(k)
        } else {
            self.parent[k]
        }
    }

    /// Where the bracket at index `k` closes: the end of the code when it
    /// never does.
    fn end(&self, code_len: usize, k: usize) -> usize {
        self.partner[k].map_or(code_len, |close| self.at[close])
    }
}

// ---------------------------------------------------------------- parameters

/// Each parameter list a body follows, the body being where its names are
/// bound, and a `for` head's variables, for the loop. A group a control-flow
/// keyword heads (`if (create) {`) holds an expression, and a call's
/// arguments (`: super(x) {`, `: _f = make(x) {` in an initializer list) bind
/// nothing. A function declared in a block binds its own name there, to the
/// block's end. Returns where the groups read as parameter lists open.
fn bind_parameters(code: &str, b: &Brackets, scopes: &mut DartLocalScopes) -> HashSet<usize> {
    let bytes = code.as_bytes();
    let mut lists = HashSet::new();
    for k in 0..b.at.len() {
        let Some(open) = b.partner[k] else {
            continue;
        };
        if bytes[b.at[k]] != b')' {
            continue;
        }
        let from = b.at[open];
        let head = token_before(code, from);
        let mut i = skip_space(code, b.at[k] + 1);
        if let Some(modifier) = body_modifier(&code[i..]) {
            i = skip_space(code, i + modifier);
        }
        let list = &code[from + 1..b.at[k]];
        let block = if bytes.get(i) == Some(&b'{') && b.at.get(k + 1) == Some(&i) {
            b.partner[k + 1].map(|close| b.at[close])
        } else {
            None
        };
        if head.word == Some("for") {
            let end = block.unwrap_or_else(|| end_of_statement(code, b, i, false));
            for name in for_head_names(list) {
                scopes.bind(&name, from, end, false);
            }
            lists.insert(from);
            continue;
        }
        if head.word.is_some_and(|word| CONTROL_HEADS.contains(&word)) {
            continue;
        }
        let scope = if let Some(end) = block {
            Some((i, end))
        } else if code[i..].starts_with("=>") {
            Some((i, end_of_statement(code, b, i + 2, true)))
        } else if bytes.get(i) == Some(&b':') && is_constructor_head(code, head) {
            Some((i, end_of_constructor(code, b, i + 1)))
        } else {
            None
        };
        let Some((start, end)) = scope else {
            continue;
        };
        if is_call_arguments(code, head) {
            continue;
        }
        lists.insert(from);
        for name in parameter_names(list) {
            scopes.bind(&name, start, end, false);
        }
        // `int make() => 2;` in a block: a local function, a node of its own.
        if let (Some(word), Some(word_at)) = (head.word, head.at)
            && !is_keyword(word)
            && let Some(around) = enclosing_block(code, b, word_at)
        {
            scopes.bind(word, word_at, b.end(code.len(), around), true);
        }
    }
    lists
}

/// The length of a body's `async`, `async*` or `sync*` modifier at the start
/// of `text` (`^(?:async\s*\*?|sync\s*\*)` over its next eight characters).
fn body_modifier(text: &str) -> Option<usize> {
    let window = text
        .char_indices()
        .nth(8)
        .map_or(text, |(at, _)| &text[..at]);
    let spaces = |rest: &str| rest.len() - rest.trim_start_matches(is_js_space).len();
    if let Some(rest) = window.strip_prefix("async") {
        let blank = spaces(rest);
        let star = usize::from(rest[blank..].starts_with('*'));
        return Some("async".len() + blank + star);
    }
    let rest = window.strip_prefix("sync")?;
    let blank = spaces(rest);
    rest[blank..]
        .starts_with('*')
        .then_some("sync".len() + blank + 1)
}

/// Whether a group is a call's arguments rather than a declaration's
/// parameters: a call in a constructor's initializer list (`: _value =
/// compute(x) {`, `: super.named(x) {`, `: assert(x) {`).
fn is_call_arguments(code: &str, head: Token<'_>) -> bool {
    let (Some(word), Some(word_at)) = (head.word, head.at) else {
        return false;
    };
    if matches!(word, "super" | "this" | "assert") {
        return true;
    }
    let before = token_before(code, word_at);
    if before.word.is_some() {
        return false;
    }
    match char_at(code, before.at) {
        Some('=' | ':' | ',') => true,
        // `super.named(x)` / `this.named(x)`, never a named constructor
        // `Foo.named(x)`.
        Some('.') => before
            .at
            .is_some_and(|dot| matches!(token_before(code, dot).word, Some("super" | "this"))),
        _ => false,
    }
}

/// Whether a group followed by `:` is a constructor's parameter list —
/// `Foo(this.x) :`, `const Foo.named(int x) :` at the start of a member — and
/// not a call in a conditional (`c ? f(x) : y`) or a `case Foo(x):`.
fn is_constructor_head(code: &str, head: Token<'_>) -> bool {
    let (Some(word), Some(word_at)) = (head.word, head.at) else {
        return false;
    };
    if is_keyword(word) {
        return false;
    }
    let mut type_name = word;
    let mut before = token_before(code, word_at);
    if before.word.is_none()
        && let Some(dot) = before.at
        && code.as_bytes()[dot] == b'.'
    {
        let owner = token_before(code, dot);
        let (Some(owner_word), Some(owner_at)) = (owner.word, owner.at) else {
            return false;
        };
        type_name = owner_word;
        before = token_before(code, owner_at);
    }
    if !type_name
        .trim_start_matches('_')
        .starts_with(|c: char| c.is_ascii_uppercase() || c == '$')
    {
        return false;
    }
    if let Some(word) = before.word {
        return matches!(word, "const" | "external");
    }
    before
        .at
        .is_none_or(|at| matches!(code.as_bytes()[at], b';' | b'{' | b'}'))
}

/// The end of a constructor's initializer list and body, from just past its
/// `:` — the body being the first `{` that does not start a value (`: _m =
/// {}`).
fn end_of_constructor(code: &str, b: &Brackets, from: usize) -> usize {
    let bytes = code.as_bytes();
    let mut k = b.first_from(from);
    let mut pos = from;
    loop {
        let next = b.at.get(k).copied().unwrap_or(code.len());
        if let Some(semicolon) = bytes[pos.min(next)..next].iter().position(|c| *c == b';') {
            return pos + semicolon;
        }
        if k >= b.at.len() {
            return code.len();
        }
        if matches!(bytes[next], b')' | b']' | b'}') {
            return next;
        }
        let Some(close) = b.partner[k] else {
            return code.len();
        };
        if bytes[next] == b'{' {
            let prior = token_before(code, next);
            if prior.word.is_some()
                || matches!(char_at(code, prior.at), Some(')' | ']' | '\'' | '"'))
            {
                return b.at[close];
            }
        }
        pos = b.at[close] + 1;
        k = close + 1;
    }
}

/// The names a parameter list declares: `(Ref ref, {required this.create,
/// int count = 0})`, `([String? label])`, old-style `void cb(int x)` (`cb`),
/// untyped closure parameters `(create, {name})`, and a switch arm's pattern
/// `Foo(:final value) =>`.
fn parameter_names(list: &str) -> Vec<String> {
    static ANNOTATION: OnceLock<Regex> = OnceLock::new();
    let annotation = ANNOTATION.get_or_init(|| js_regex(r"@[A-Za-z0-9_$.]+(?:\s*\([^()]*\))?"));
    let mut names = Vec::new();
    for raw in split_parameters(list) {
        let mut item = annotation.replace_all(raw, " ").into_owned();
        if let Some(eq) = item.find('=') {
            item.truncate(eq);
        }
        let mut item = item.trim_end_matches(is_js_space);
        // `void cb(int x)`: the name is before its own parameter list.
        if item.ends_with(')') {
            let mut depth = 0i64;
            let mut cut = 0;
            for (at, byte) in item.bytes().enumerate().rev() {
                if byte == b')' {
                    depth += 1;
                } else if byte == b'(' {
                    depth -= 1;
                    if depth == 0 {
                        cut = at;
                        break;
                    }
                }
            }
            item = item[..cut].trim_end_matches(is_js_space);
        }
        if let Some(name) = trailing_identifier(item) {
            names.push(name.to_string());
        }
    }
    names
}

/// The identifier `item` ends with (`([A-Za-z_$][\w$]*)$`).
fn trailing_identifier(item: &str) -> Option<&str> {
    let bytes = item.as_bytes();
    let run = bytes.len() - bytes.iter().rev().take_while(|b| is_name_byte(**b)).count();
    let first = bytes[run..].iter().position(|b| is_name_start(*b))?;
    Some(&item[run + first..])
}

/// A parameter list's items, the optional group's (`{…}` / `[…]`, always
/// last) included.
fn split_parameters(list: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0i64;
    let mut item_start = 0;
    let blank = |text: &str| text.trim_matches(is_js_space).is_empty();
    for (i, ch) in list.char_indices() {
        if depth == 0 && (ch == '{' || ch == '[') && blank(&list[item_start..i]) {
            let close = list.rfind(if ch == '{' { '}' } else { ']' });
            let end = close.filter(|close| *close > i).unwrap_or(list.len());
            items.extend(split_parameters(&list[i + 1..end]));
            return items;
        }
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                if !blank(&list[item_start..i]) {
                    items.push(&list[item_start..i]);
                }
                item_start = i + 1;
            }
            _ => {}
        }
    }
    if !blank(&list[item_start..]) {
        items.push(&list[item_start..]);
    }
    items
}

/// The variables a `for` head declares: `final x in xs`, `var (a, b) in
/// pairs`, `var i = 0, j = n; …`.
fn for_head_names(head: &str) -> Vec<String> {
    static LOOP: OnceLock<Regex> = OnceLock::new();
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    static DECLARED: OnceLock<Regex> = OnceLock::new();
    static INIT: OnceLock<Regex> = OnceLock::new();
    static ASSIGNED: OnceLock<Regex> = OnceLock::new();
    let looped = LOOP.get_or_init(|| js_regex(r"^([^;]*?)(?-u:\b)in(?-u:\b)"));
    if let Some(caps) = looped.captures(head) {
        let decl = caps[1].trim_matches(is_js_space);
        let pattern = PATTERN
            .get_or_init(|| js_regex(r"^(?:final|var)\s*(?:[A-Za-z0-9_$.]+\s*)?([(\[{](?s:.)*)$"));
        if let Some(caps) = pattern.captures(decl) {
            return pattern_binders(&caps[1]);
        }
        let declared = DECLARED.get_or_init(|| {
            js_regex(
                r"^(?:(?:final|var|const)\s+(?:[A-Za-z0-9_$.]+\s*\??\s+)?|[A-Za-z0-9_$.]+\s*\??\s+)([A-Za-z_$][A-Za-z0-9_$]*)$",
            )
        });
        return declared
            .captures(decl)
            .map(|caps| vec![caps[1].to_string()])
            .unwrap_or_default();
    }
    let init = head.split(';').next().unwrap_or("");
    let declares = INIT.get_or_init(|| {
        js_regex(
            r"^\s*(?:(?:final|var|const|late)(?-u:\b)|[A-Za-z0-9_$.]+\s*\??\s+[A-Za-z_$][A-Za-z0-9_$]*\s*=)",
        )
    });
    if !declares.is_match(init) {
        return Vec::new();
    }
    // `(?:^|,|\s)([A-Za-z_$][\w$]*)\s*=(?![=>])`, the lookahead read here.
    let assigned = ASSIGNED.get_or_init(|| js_regex(r"(?:^|,|\s)([A-Za-z_$][A-Za-z0-9_$]*)\s*="));
    let mut names = Vec::new();
    let mut from = 0;
    while let Some(caps) = assigned.captures_at(init, from) {
        let whole = caps.get(0).expect("the whole match");
        if matches!(init.as_bytes().get(whole.end()), Some(b'=' | b'>')) {
            // The search goes on from the next position.
            from = whole.start()
                + init[whole.start()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
        } else {
            names.push(caps[1].to_string());
            from = whole.end();
        }
    }
    names
}

/// The names a destructuring pattern binds: `(a, b)`, `[x, ...rest]`,
/// `Point(:x, y: final py)`, `{'k': v}`.
fn pattern_binders(pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (start, end) in identifier_runs(pattern) {
        let after = &pattern[end..];
        let rest = after.trim_start_matches(is_js_space);
        // `(?!\s*[:(])(?!\s+[A-Za-z_$(])`
        if rest.starts_with([':', '('])
            || (rest.len() < after.len()
                && rest
                    .bytes()
                    .next()
                    .is_some_and(|b| is_name_start(b) || b == b'('))
        {
            continue;
        }
        let name = &pattern[start..end];
        if name != "_" && !is_reserved(name) {
            names.push(name.to_string());
        }
    }
    names
}

/// Every identifier of `text` written whole and not after a `.`
/// (`(?<![\w$.])[A-Za-z_$][\w$]*(?![\w$])`), as byte ranges.
fn identifier_runs(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut runs = Vec::new();
    let mut k = 0;
    while k < bytes.len() {
        if !is_name_byte(bytes[k]) {
            k += 1;
            continue;
        }
        let start = k;
        while k < bytes.len() && is_name_byte(bytes[k]) {
            k += 1;
        }
        // A run that opens with a digit holds no match: each later start
        // follows a word character.
        if is_name_start(bytes[start]) && !(start > 0 && bytes[start - 1] == b'.') {
            runs.push((start, k));
        }
    }
    runs
}

/// The locals a block declares, each bound from its declaration to the
/// block's end: `final x = …`, `var x;`, `Foo<T>? x = …`, `void Function() x
/// = …`, `final (a, b) = …`, and a pattern's `final x` / `var x` (`case
/// Foo(:final x)`, `if (v case final x?)`). A declaration directly in a class
/// body is a field, and one at the top of the file a library variable.
fn bind_locals(
    code: &str,
    b: &Brackets,
    parameter_lists: &HashSet<usize>,
    scopes: &mut DartLocalScopes,
) {
    let bytes = code.as_bytes();
    let mut type_bodies: HashMap<usize, bool> = HashMap::new();
    // `direct`: a statement's own declaration, whose block is the bracket
    // right around it; a pattern's variable is in the block around its
    // `case` or `if`.
    let mut bind_in_block = |scopes: &mut DartLocalScopes, name: &str, at: usize, direct: bool| {
        let mut k = b.innermost(bytes, at);
        while !direct
            && let Some(index) = k
            && bytes[b.at[index]] != b'{'
        {
            // A parameter list's `final x` is the parameter, bound where the
            // list is read.
            if parameter_lists.contains(&b.at[index]) {
                return;
            }
            k = b.parent[index];
        }
        if let Some(index) = k
            && bytes[b.at[index]] == b'{'
            && !*type_bodies
                .entry(b.at[index])
                .or_insert_with(|| is_type_body(code, b.at[index]))
        {
            scopes.bind(name, at, b.end(code.len(), index), false);
        }
    };
    // `(?<![\w$.])([A-Za-z_$][\w$]*)(?=\s*(?:=(?![=>])|;))`
    for (start, end) in identifier_runs(code) {
        let rest = code[end..].trim_start_matches(is_js_space);
        let declares =
            rest.starts_with(';') || (rest.starts_with('=') && !rest[1..].starts_with(['=', '>']));
        if declares && is_declaration_type(code, b, token_before(code, start)) {
            bind_in_block(scopes, &code[start..end], start, true);
        }
    }
    // A pattern variable: `case Foo(:final value)`, `if (x case final y?)` —
    // not the type of `final Foo? x = …`.
    for (keyword_at, name_start, name_end) in pattern_variables(code) {
        bind_in_block(scopes, &code[name_start..name_end], keyword_at, false);
    }
    // A destructuring declaration: `final (a, b) = …`, `var Point(:x, :y) =
    // …`.
    for (keyword_at, open) in destructurings(code) {
        let Some(close) = b
            .index_at(open)
            .and_then(|k| b.partner[k])
            .map(|close| b.at[close])
        else {
            continue;
        };
        // `/^\s*=(?![=>])/` over the next three characters.
        let after = &code[close + 1..];
        let after = after
            .char_indices()
            .nth(3)
            .map_or(after, |(at, _)| &after[..at]);
        let rest = after.trim_start_matches(is_js_space);
        if !(rest.starts_with('=') && !rest[1..].starts_with(['=', '>'])) {
            continue;
        }
        for name in pattern_binders(&code[open + 1..close]) {
            bind_in_block(scopes, &name, keyword_at, true);
        }
    }
}

/// Each pattern variable,
/// `(?<![\w$.])(?:final|var)\s+(?:[\w$.]+\s*\??\s+)?([A-Za-z_$][\w$]*)(?=\s*(?:[)\]},:]|\?(?!\s*[\w$])|=>|\bwhen\b|&&|\|\|))`:
/// where its keyword starts and its name's byte range.
fn pattern_variables(code: &str) -> Vec<(usize, usize, usize)> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut resume = 0;
    for (keyword_at, keyword_end) in identifier_runs(code) {
        if keyword_at < resume || !matches!(&code[keyword_at..keyword_end], "final" | "var") {
            continue;
        }
        let spaced = skip_space(code, keyword_end);
        if spaced == keyword_end {
            continue;
        }
        // With a type written first, then without.
        let mut names = Vec::with_capacity(2);
        let type_end = spaced
            + bytes[spaced..]
                .iter()
                .take_while(|b| is_name_byte(**b) || **b == b'.')
                .count();
        if type_end > spaced {
            let after = skip_space(code, type_end);
            if bytes.get(after) == Some(&b'?') {
                let named = skip_space(code, after + 1);
                if named > after + 1 {
                    names.push(named);
                }
            } else if after > type_end {
                names.push(after);
            }
        }
        names.push(spaced);
        for name_at in names {
            if let Some(name_end) = identifier_end(bytes, name_at)
                && pattern_variable_follows(code, name_end)
            {
                found.push((keyword_at, name_at, name_end));
                resume = name_end;
                break;
            }
        }
    }
    found
}

/// `(?=\s*(?:[)\]},:]|\?(?!\s*[\w$])|=>|\bwhen\b|&&|\|\|))` at byte `at`, right
/// after a name.
fn pattern_variable_follows(code: &str, at: usize) -> bool {
    let rest = code[at..].trim_start_matches(is_js_space);
    if rest.starts_with([')', ']', '}', ',', ':'])
        || rest.starts_with("=>")
        || rest.starts_with("&&")
        || rest.starts_with("||")
    {
        return true;
    }
    if let Some(after) = rest.strip_prefix('?') {
        return !after
            .trim_start_matches(is_js_space)
            .bytes()
            .next()
            .is_some_and(is_name_byte);
    }
    // `\bwhen\b`: blanks were skipped, or the name ends with `$`.
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    rest.strip_prefix("when")
        .is_some_and(|after| !after.starts_with(is_word))
        && (rest.len() < code.len() - at || !code[..at].ends_with(is_word))
}

/// Each destructuring declaration's head,
/// `(?<![\w$.])(?:final|var)\s*(?:[A-Z][\w$]*\s*)?([([{])`: where its keyword
/// starts and where its bracket opens.
fn destructurings(code: &str) -> Vec<(usize, usize)> {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| js_regex(r"(?:final|var)\s*(?:[A-Z][A-Za-z0-9_$]*\s*)?[(\[{]"));
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(m) = head.find_at(code, from) {
        if m.start() > 0 && (is_name_byte(bytes[m.start() - 1]) || bytes[m.start() - 1] == b'.') {
            from = m.start() + 1;
            continue;
        }
        found.push((m.start(), m.end() - 1));
        from = m.end();
    }
    found
}

/// Whether the token before a name followed by `=` / `;` makes it a
/// declaration: `final` / `var` / `const`, a type (`Foo`, `Foo?`, `List<int>`
/// — type arguments are blanked — or a function type ending `Function(…)`).
fn is_declaration_type(code: &str, b: &Brackets, before: Token<'_>) -> bool {
    if let Some(word) = before.word {
        return DECLARATION_KEYWORDS.contains(&word) || !is_keyword(word);
    }
    let Some(at) = before.at else {
        return false;
    };
    match code.as_bytes()[at] {
        b'?' => {
            at > 0 && {
                let previous = code.as_bytes()[at - 1];
                is_name_byte(previous) || previous == b')'
            }
        }
        b')' => b
            .index_at(at)
            .and_then(|k| b.partner[k])
            .is_some_and(|open| token_before(code, b.at[open]).word == Some("Function")),
        _ => false,
    }
}

/// Whether the `{` at `brace` opens a class, mixin, enum or extension body.
fn is_type_body(code: &str, brace: usize) -> bool {
    static TYPE_BODY_HEAD: OnceLock<Regex> = OnceLock::new();
    let head = TYPE_BODY_HEAD.get_or_init(|| {
        js_regex(
            r"^\s*(?:@[A-Za-z0-9_$.]+(?:\s*\((?:[^()]|\([^()]*\))*\))?\s*)*(?:(?:abstract|base|final|sealed|interface|mixin|augment)\s+)*(?:class|mixin|enum|extension)(?-u:\b)",
        )
    });
    let from = code[..brace].rfind([';', '{', '}']).map_or(0, |at| at + 1);
    head.is_match(&code[from..brace])
}

/// The index of the `{` right around byte `at` when it holds statements (no
/// class or extension body).
fn enclosing_block(code: &str, b: &Brackets, at: usize) -> Option<usize> {
    let k = b.innermost(code.as_bytes(), at)?;
    (code.as_bytes()[b.at[k]] == b'{' && !is_type_body(code, b.at[k])).then_some(k)
}

/// Where the expression or statement starting at `from` ends: the first `;`
/// (and, for an expression, `,`) outside the brackets it opens, or the close
/// of the bracket around it — the end of a `=> …` body in an argument list,
/// a switch arm or a declaration.
fn end_of_statement(code: &str, b: &Brackets, from: usize, expression: bool) -> usize {
    let bytes = code.as_bytes();
    let mut k = b.first_from(from);
    let mut pos = from;
    loop {
        let next = b.at.get(k).copied().unwrap_or(code.len());
        if let Some(stop) = bytes[pos.min(next)..next]
            .iter()
            .position(|c| *c == b';' || (expression && *c == b','))
        {
            return pos + stop;
        }
        if k >= b.at.len() {
            return code.len();
        }
        if matches!(bytes[next], b')' | b']' | b'}') {
            return next;
        }
        let Some(close) = b.partner[k] else {
            return code.len();
        };
        pos = b.at[close] + 1;
        k = close + 1;
    }
}

// ---------------------------------------------------------------- type arguments

/// The code with type-argument lists blanked — `Map<String, int>`, `<T>(x)
/// =>`, `<int>[]` — so their commas never end an argument and their angle
/// brackets never read as comparisons. A `<` opens one when it follows a
/// name with no space (`List<int>`), or when what it closes is followed by
/// `(`, `[`, `{` or `.` (a generic function or a typed literal).
fn blank_type_arguments(code: &str) -> String {
    if !code.contains('<') {
        return code.to_string();
    }
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut copied = 0;
    let mut next_open = code.find('<');
    while let Some(i) = next_open {
        if let Some(end) = type_arguments_end(code, i) {
            let glued = i > 0 && is_name_byte(bytes[i - 1]);
            let after = bytes.get(skip_space(code, end + 1)).copied();
            if glued || matches!(after, Some(b'(' | b'[' | b'{' | b'.')) {
                out.push_str(&code[copied..i]);
                // Whole characters: the span opens and closes on `<` / `>`.
                for c in code[i..=end].chars() {
                    match c {
                        '\n' | '\r' => out.push(c),
                        _ => out.extend(std::iter::repeat_n(' ', c.len_utf8())),
                    }
                }
                copied = end + 1;
                next_open = code[end + 1..].find('<').map(|at| end + 1 + at);
                continue;
            }
        }
        next_open = code[i + 1..].find('<').map(|at| i + 1 + at);
    }
    out.push_str(&code[copied..]);
    out
}

/// The `>` closing a type-argument list opened at `open`, when everything
/// inside reads as type syntax.
fn type_arguments_end(code: &str, open: usize) -> Option<usize> {
    let mut angles = 0i64;
    let mut parens = 0i64;
    for (offset, ch) in code[open..].char_indices().take(400) {
        let i = open + offset;
        match ch {
            '<' => angles += 1,
            '>' => {
                if code[..i].ends_with('=') {
                    return None;
                }
                angles -= 1;
                if angles == 0 {
                    return (parens == 0).then_some(i);
                }
            }
            '(' | '{' => parens += 1,
            ')' | '}' => {
                parens -= 1;
                if parens < 0 {
                    return None;
                }
            }
            c if c.is_ascii_alphanumeric()
                || matches!(c, '_' | '$' | '.' | ',' | '?')
                || is_js_space(c) => {}
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `name` is bound, as a parameter or local, where `marker`
    /// starts in `source`.
    fn bound(source: &str, name: &str, marker: &str) -> bool {
        let code = crate::dart_chains::blank_dart_code(source);
        let at = source.find(marker).expect("marker");
        DartLocalScopes::of(&code)
            .nearest(name, at)
            .is_some_and(|scope| !scope.function)
    }

    #[test]
    fn a_parameter_binds_its_body_only() {
        let source = "class Holder {\n  void run(void Function() create) {\n    create();\n  }\n  void other() {\n    create();\n  }\n}\n";
        assert!(bound(source, "create", "create();\n  }\n  void"));
        assert!(!bound(source, "create", "create();\n  }\n}"));
    }

    #[test]
    fn a_closure_and_a_loop_bind_only_their_bodies() {
        let source = "void wire(List<int> values) {\n  values.forEach((notify) => print(notify));\n  notify(1);\n  for (final notify in values) {\n    print(notify);\n  }\n  notify(2);\n}\n";
        assert!(bound(source, "notify", "notify));"));
        assert!(!bound(source, "notify", "notify(1)"));
        assert!(bound(source, "notify", "notify);\n  }"));
        assert!(!bound(source, "notify", "notify(2)"));
    }

    #[test]
    fn a_local_binds_from_its_declaration_and_a_field_binds_nothing() {
        let source = "class Box {\n  final listener = 0;\n  void main() {\n    listener(0);\n    final build = () {};\n    build();\n    final (create, _) = (() => 0, 1);\n    create();\n  }\n}\n";
        assert!(!bound(source, "listener", "listener(0)"));
        assert!(bound(source, "build", "build();"));
        assert!(bound(source, "create", "create();"));
    }

    #[test]
    fn a_local_function_is_nearer_than_the_parameter_it_shadows() {
        let source = "int outer(int Function() make) {\n  int helper() {\n    int make() => 2;\n    return make();\n  }\n  return helper() + make();\n}\n";
        let code = crate::dart_chains::blank_dart_code(source);
        let scopes = DartLocalScopes::of(&code);
        let inner = source.find("make();").expect("inner call");
        let outer = source.find("make();\n}").expect("outer call");
        assert!(
            scopes
                .nearest("make", inner)
                .is_some_and(|scope| scope.function)
        );
        assert!(
            scopes
                .nearest("make", outer)
                .is_some_and(|scope| !scope.function)
        );
    }

    #[test]
    fn pattern_variables_and_typed_locals_bind_their_names_not_their_types() {
        let source = "void f(Object o) {\n  if (o case final Box box?) {\n    box();\n  }\n  switch (o) {\n    case Foo(:final value) when value != null:\n      value();\n  }\n  final Box? other = null;\n  Box();\n}\n";
        assert!(bound(source, "box", "box();"));
        assert!(bound(source, "value", "value();"));
        assert!(!bound(source, "Box", "Box();"));
    }

    #[test]
    fn a_constructor_binds_its_parameters_through_its_initializer_list() {
        let source = "class Foo {\n  Foo(this.make, int Function() build) : _x = build() {\n    build();\n  }\n  final int Function() make;\n  final int _x;\n}\n";
        assert!(bound(source, "build", "build() {"));
        assert!(bound(source, "build", "build();"));
    }

    #[test]
    fn a_control_head_and_a_calls_arguments_bind_nothing() {
        let source = "class Foo {\n  Foo(int a) : _b = make(other) {\n    other();\n  }\n  final int _b;\n}\n\nvoid f(bool ready) {\n  if (go) {\n    go();\n  }\n}\n";
        assert!(!bound(source, "other", "other();"));
        assert!(!bound(source, "go", "go();"));
        assert!(bound(source, "a", "make(other)"));
    }

    #[test]
    fn type_arguments_never_end_an_arrow_body() {
        let source = "final f = (create) => wrap<int, String>(create());\n";
        assert!(bound(source, "create", "create());"));
    }

    #[test]
    fn body_modifiers_are_read_within_eight_characters() {
        assert_eq!(body_modifier("async {"), Some(6));
        assert_eq!(body_modifier("async* {"), Some(6));
        assert_eq!(body_modifier("sync* {"), Some(5));
        assert_eq!(body_modifier("sync {"), None);
        assert_eq!(body_modifier("{"), None);
    }

    #[test]
    fn type_arguments_are_blanked_where_they_open() {
        assert_eq!(
            blank_type_arguments("Map<String, int> m"),
            format!("Map{}m", " ".repeat(14))
        );
        assert_eq!(blank_type_arguments("a < b && c > d"), "a < b && c > d");
        assert_eq!(blank_type_arguments("if (a < b) {}"), "if (a < b) {}");
        assert_eq!(blank_type_arguments("<T>(x) => x"), "   (x) => x");
    }
}
