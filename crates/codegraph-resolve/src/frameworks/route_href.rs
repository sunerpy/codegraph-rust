//! The destination a navigation call names, read off its source: the href
//! readers `upstream resolution/frameworks/expo-router.ts` exports at
//! `b635dd46`, which every router's resolver reads its calls with (Next.js
//! and React Router here), and the matching `nextjs.ts` exports
//! (`destinationsForHref`).
//!
//! `router.push('/users')` names its route by path, so extraction records a
//! call that resolves to nothing. A router's `resolve` reads the call's
//! argument (a string, a template with holes, an `Href` object's `pathname`,
//! a conditional's arms, a local `const href = …`), matches it against its
//! route table and returns a `navigates` edge. Anything that is not static
//! text stays unresolved.
//!
//! The scanners walk bytes: every delimiter they look for is ASCII, so a
//! multibyte character is never mistaken for one.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use codegraph_core::types::Node;
use regex::Regex;

/// Stands for an interpolated `${…}` inside a template-literal href
/// (`HOLE`).
pub(crate) const HOLE: char = '\u{0}';

/// Lines a single navigation call may span (`MAX_CALL_LINES`).
const MAX_CALL_LINES: usize = 12;

/// An identifier (`IDENT`).
const IDENT: &str = "[A-Za-z_$][A-Za-z0-9_$]*";

/// An href read off a call (`HrefLiteral`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HrefLiteral {
    /// The path part, holes kept as [`HOLE`]; query and hash removed.
    pub path: String,
    /// The literal as written, holes rendered as `${…}`: the edge's `href`.
    pub display: String,
    /// The other destinations, when the argument was a conditional:
    /// `!isAdmin ? keyword ? '/search/…' : '/page/…' : '/admin/…'` names
    /// three places a user can end up.
    pub alternates: Vec<HrefLiteral>,
}

impl HrefLiteral {
    /// Every destination this href names: itself first, then its other arms
    /// (`hrefArms`).
    pub(crate) fn arms(&self) -> impl Iterator<Item = &HrefLiteral> {
        std::iter::once(self).chain(&self.alternates)
    }

    fn leaf(&self) -> Self {
        Self {
            path: self.path.clone(),
            display: self.display.clone(),
            alternates: Vec::new(),
        }
    }
}

/// The name a file exports as its default, and where (`defaultExportName`):
/// a wrapper (`memo(Screen)`, `React.forwardRef(Screen)`) is looked through
/// to its first identifier argument; an anonymous default has no name.
pub(crate) fn default_export_name(stripped: &str) -> Option<(String, usize)> {
    static PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            format!(r"export\s+default\s+(?:async\s+)?function\s*\*?\s*({IDENT})"),
            format!(r"export\s+default\s+class\s+({IDENT})"),
            format!(r"export\s+default\s+(?:{IDENT}\.)?{IDENT}\s*\(\s*({IDENT})\s*[,)]"),
            format!(r"(?m)export\s+default\s+({IDENT})\s*;?\s*$"),
            format!(r"export\s*\{{\s*({IDENT})\s+as\s+default\s*\}}"),
        ]
        .map(|pattern| Regex::new(&pattern).expect("default export pattern"))
    });
    patterns.iter().find_map(|pattern| {
        let captures = pattern.captures(stripped)?;
        let name = captures.get(1)?.as_str();
        (name != "default").then(|| {
            (
                name.to_string(),
                captures.get(0).expect("whole match").start(),
            )
        })
    })
}

/// Index of the `)` matching the `(` at `open`, skipping string bodies
/// (`matchParen`).
fn match_paren(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < s.len() && s[i] != quote {
                    if s[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'`' => i = skip_template(s, i),
            b'(' => depth += 1,
            b')' => {
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

/// Index of the backtick closing the template opening at `open`, or the end
/// of `s` (`skipTemplate`).
fn skip_template(s: &[u8], open: usize) -> usize {
    let mut i = open + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'`' => return i,
            b'$' if s.get(i + 1) == Some(&b'{') => {
                let mut depth = 0i64;
                i += 1;
                while i < s.len() {
                    match s[i] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        b'`' => i = skip_template(s, i),
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// Index of the quote closing the string literal opening at `at`, or the end
/// of `s` (`stringEnd`).
pub(crate) fn string_end(s: &[u8], at: usize) -> usize {
    let quote = s[at];
    if quote == b'`' {
        return skip_template(s, at);
    }
    let mut i = at + 1;
    while i < s.len() {
        if s[i] == b'\\' {
            i += 1;
        } else if s[i] == quote {
            return i;
        }
        i += 1;
    }
    s.len()
}

/// The string literal (`'…'`, `"…"` or a template) starting at `at`, every
/// `${…}` replaced by [`HOLE`]; `None` when `at` starts no string
/// (`readStringAt`).
pub(crate) fn read_string_at(s: &str, at: usize) -> Option<String> {
    let bytes = s.as_bytes();
    let quote = *bytes.get(at)?;
    match quote {
        b'"' | b'\'' => {
            let mut out = String::new();
            let mut chars = s[at + 1..].chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    out.extend(chars.next());
                } else if ch as u32 == u32::from(quote) {
                    return Some(out);
                } else {
                    out.push(ch);
                }
            }
            None
        }
        b'`' => {
            let end = skip_template(bytes, at);
            let body = &s[at + 1..end.max(at + 1)];
            let mut out = String::new();
            let mut chars = body.char_indices().peekable();
            while let Some((i, ch)) = chars.next() {
                match ch {
                    '\\' => {
                        if let Some((_, next)) = chars.next() {
                            out.push(next);
                        }
                    }
                    '$' if body.as_bytes().get(i + 1) == Some(&b'{') => {
                        let mut depth = 0i64;
                        for (_, inner) in chars.by_ref() {
                            match inner {
                                '{' => depth += 1,
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        out.push(HOLE);
                    }
                    _ => out.push(ch),
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Index of the first `ch` at bracket depth 0, outside strings, from
/// `from` (`indexAtDepth0`). `?.` and `??` are never a `?`.
fn index_at_depth0(s: &[u8], ch: u8, from: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut i = from;
    while i < s.len() {
        let c = s[i];
        match c {
            b'"' | b'\'' => {
                i += 1;
                while i < s.len() && s[i] != c {
                    if s[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'`' => i = skip_template(s, i),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'?' if matches!(s.get(i + 1), Some(b'.' | b'?')) => i += 1,
            _ if depth == 0 && c == ch => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// The href a literal names (`toHref`): its path is the text before any `?`
/// or `#`; an empty literal or path is none.
pub(crate) fn to_href(literal: Option<String>) -> Option<HrefLiteral> {
    let literal = literal.filter(|literal| !literal.is_empty())?;
    let path = literal
        .find(['?', '#'])
        .map_or(literal.as_str(), |cut| &literal[..cut]);
    if path.is_empty() {
        return None;
    }
    Some(HrefLiteral {
        path: path.to_string(),
        display: literal.replace(HOLE, "${…}"),
        alternates: Vec::new(),
    })
}

/// The `:` closing the ternary opened at `q`, honouring nested ones
/// (`ternaryColon`): taking the first `:` would split
/// `a ? b ? '/x' : '/y' : '/z'` between the wrong arms.
fn ternary_colon(s: &[u8], q: usize) -> Option<usize> {
    let mut depth = 0u32;
    let mut i = q + 1;
    for _ in 0..64 {
        let colon = index_at_depth0(s, b':', i)?;
        if let Some(next_q) = index_at_depth0(s, b'?', i).filter(|&next_q| next_q < colon) {
            depth += 1;
            i = next_q + 1;
            continue;
        }
        if depth == 0 {
            return Some(colon);
        }
        depth -= 1;
        i = colon + 1;
    }
    None
}

/// `expr as any` / `expr satisfies Href` off the end of an expression: a
/// cast says nothing about the value.
fn strip_cast(expr: &str) -> &str {
    static CAST_TAIL: OnceLock<Regex> = OnceLock::new();
    let cast = CAST_TAIL.get_or_init(|| {
        Regex::new(r"\s+(?:as|satisfies)\s+[A-Za-z0-9_$.<>\[\]|&\s]+$").expect("cast tail")
    });
    cast.find(expr).map_or(expr, |m| &expr[..m.start()])
}

/// How much of an expression a destination literal must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Literal {
    /// A literal at the head of the expression is read even when the
    /// expression goes on past it (`'/a/' + id` reads `/a/`), as upstream's
    /// readers do.
    Head,
    /// A literal is read only when it is the whole expression, or the whole
    /// arm of a conditional, and a template's holes fill whole segments:
    /// `'/a/' + id` and `` `/a${rest}` `` are computed and read nothing.
    Whole,
}

/// True when every hole of `href`'s path is a whole segment: `/a/${id}`,
/// not `/a${rest}`, whose value may add segments of its own.
fn holes_fill_segments(href: &HrefLiteral) -> bool {
    href.path
        .split('/')
        .all(|seg| !seg.contains(HOLE) || seg.chars().all(|c| c == HOLE))
}

/// True when the string or template opened at `at` is a whole value: past
/// it and any comments, no operator, member access or call continues the
/// expression, only its end, a `;`, or the `,` `}` or `)` closing what holds
/// it (`'/a' /* x */ + b` goes on). An unclosed comment is not known to end.
fn literal_is_whole(s: &str, at: usize) -> bool {
    let bytes = s.as_bytes();
    if !matches!(bytes.get(at), Some(b'"' | b'\'' | b'`')) {
        return false;
    }
    let end = string_end(bytes, at);
    if end >= bytes.len() {
        return false;
    }
    let mut rest = s[end + 1..].trim_start();
    loop {
        if let Some(line) = rest.strip_prefix("//") {
            rest = line.find('\n').map_or("", |nl| line[nl + 1..].trim_start());
        } else if let Some(block) = rest.strip_prefix("/*") {
            let Some(close) = block.find("*/") else {
                return false;
            };
            rest = block[close + 2..].trim_start();
        } else {
            break;
        }
    }
    rest.is_empty() || rest.starts_with([';', ',', '}', ')'])
}

/// The string a whole string literal `value` spells, or `None` when
/// `value` is no literal or an expression continues past it.
pub(crate) fn whole_string(value: &str) -> Option<String> {
    let value = value.trim();
    literal_is_whole(value, 0)
        .then(|| read_string_at(value, 0))
        .flatten()
}

/// The href one argument expression names (`parseHrefExpression`): a
/// string, a template, an `Href` object with a literal `pathname`, or a
/// conditional whose arms are those. Each readable arm of a conditional is a
/// destination; anything else is not static.
pub(crate) fn parse_href_expression(expr: &str) -> Option<HrefLiteral> {
    parse_href(expr, Literal::Head)
}

/// [`parse_href_expression`], with `literal` saying how much of the
/// expression (or of each arm) a literal must be.
pub(crate) fn parse_href(expr: &str, literal: Literal) -> Option<HrefLiteral> {
    let mut args = strip_cast(expr.trim());
    // `(a ? b : c)`: unwrap grouping parens.
    while args.starts_with('(') && match_paren(args.as_bytes(), 0) == Some(args.len() - 1) {
        args = strip_cast(args[1..args.len() - 1].trim());
    }
    if args.is_empty() {
        return None;
    }
    let bytes = args.as_bytes();
    if let Some(q) = index_at_depth0(bytes, b'?', 0).filter(|&q| q > 0) {
        if let Some(colon) = ternary_colon(bytes, q).filter(|&colon| colon > q) {
            let yes = parse_href(&args[q + 1..colon], literal);
            let no = parse_href(&args[colon + 1..], literal);
            let mut arms = yes
                .iter()
                .chain(no.iter())
                .flat_map(HrefLiteral::arms)
                .map(HrefLiteral::leaf);
            let mut head = arms.next()?;
            head.alternates = arms.collect();
            return Some(head);
        }
    }
    if bytes[0] == b'{' {
        static PATHNAME: OnceLock<Regex> = OnceLock::new();
        let key = PATHNAME
            .get_or_init(|| Regex::new(r"\bpathname\s*:\s*").expect("pathname key"))
            .find(args)?;
        if literal == Literal::Whole && !literal_is_whole(args, key.end()) {
            return None;
        }
        return to_href(read_string_at(args, key.end()))
            .filter(|href| literal == Literal::Head || holes_fill_segments(href));
    }
    if literal == Literal::Whole && !literal_is_whole(args, 0) {
        return None;
    }
    to_href(read_string_at(args, 0))
        .filter(|href| literal == Literal::Head || holes_fill_segments(href))
}

/// The source text of the navigation call's first argument, or `None` when
/// there is no call there (`firstArgumentText`).
pub(crate) fn first_argument_text(
    lines: &[&str],
    line: i64,
    column: i64,
    method: &str,
) -> Option<String> {
    nth_argument_text(lines, line, column, method, 0)
}

/// The source text of a call's `index`th argument (0-based), or `None` when
/// there is no call there or it has too few arguments (`nthArgumentText`).
/// The call is the first `method(` at or after `column` of `line` (1-based),
/// within [`MAX_CALL_LINES`] lines.
pub(crate) fn nth_argument_text(
    lines: &[&str],
    line: i64,
    column: i64,
    method: &str,
    index: usize,
) -> Option<String> {
    let first = usize::try_from(line - 1).ok()?;
    if first >= lines.len() {
        return None;
    }
    let text = lines[first..(first + MAX_CALL_LINES).min(lines.len())].join("\n");
    let mut from = usize::try_from(column.max(0)).ok()?;
    if from > text.len() {
        return None;
    }
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let name_at = from + text[from..].find(method)?;
    let after = name_at + method.len();
    let open = after + (text[after..].len() - text[after..].trim_start().len());
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let close = match_paren(bytes, open).unwrap_or(text.len());
    let mut args = &text[open + 1..close];
    for _ in 0..index {
        let comma = index_at_depth0(args.as_bytes(), b',', 0)?;
        args = &args[comma + 1..];
    }
    Some(match index_at_depth0(args.as_bytes(), b',', 0) {
        Some(comma) => args[..comma].to_string(),
        None => args.to_string(),
    })
}

/// True when every bracket and string opened in `s` is closed (`balanced`).
fn balanced(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut depth = 0i64;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => {
                let end = string_end(bytes, i);
                if end >= bytes.len() {
                    return false;
                }
                i = end;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth <= 0
}

/// The first match of `pattern` in `text` that an `=` ends without a second
/// `=` after it: `x = …`, never `x == …` (the `=(?!=)` upstream's
/// expressions end with).
fn assignment(pattern: &Regex, text: &str) -> Option<usize> {
    pattern
        .find_iter(text)
        .find(|m| text.as_bytes().get(m.end()) != Some(&b'='))
        .map(|m| m.end())
}

/// The href a navigation call reaches through a local variable
/// (`readHrefViaLocal`):
///
/// ```text
/// const href = params.length ? `/barcode-scan?${q}` : '/barcode-scan'
/// router.navigate(href as any)
/// ```
///
/// When the argument is a bare identifier, its most recent `const`/`let`/`var`
/// declaration between `enclosing_start` and the call is parsed as a literal
/// argument would be. A reassignment in between, or an initializer that is
/// not static, is nothing.
pub(crate) fn read_href_via_local(
    lines: &[&str],
    line: i64,
    column: i64,
    method: &str,
    enclosing_start: i64,
) -> Option<HrefLiteral> {
    read_href_via_local_with(lines, line, column, method, enclosing_start, Literal::Head)
}

/// [`read_href_via_local`], with `literal` saying how much of the
/// initializer a literal must be.
pub(crate) fn read_href_via_local_with(
    lines: &[&str],
    line: i64,
    column: i64,
    method: &str,
    enclosing_start: i64,
    literal: Literal,
) -> Option<HrefLiteral> {
    static IDENTIFIER: OnceLock<Regex> = OnceLock::new();
    static CONTINUATION: OnceLock<Regex> = OnceLock::new();
    let arg = first_argument_text(lines, line, column, method)?;
    let ident = strip_cast(arg.trim());
    if !IDENTIFIER
        .get_or_init(|| Regex::new(&format!("^{IDENT}$")).expect("identifier"))
        .is_match(ident)
    {
        return None;
    }
    let escaped = regex::escape(ident);
    let decl = Regex::new(&format!(
        r"(?-u:\b)(?:const|let|var)\s+{escaped}\s*(?::[^=]*?)?="
    ))
    .ok()?;
    let reassign = Regex::new(&format!(r"(?:^|[^.A-Za-z0-9_$]){escaped}\s*=")).ok()?;
    let continuation =
        CONTINUATION.get_or_init(|| Regex::new(r#"^[?:.)\]}`'"+&|]"#).expect("continuation"));
    let call = usize::try_from(line - 1).ok()?;
    let from = usize::try_from((enclosing_start - 1).max(0)).ok()?;
    for i in (from..call.min(lines.len())).rev() {
        let text = lines[i];
        let Some(init_at) = assignment(&decl, text) else {
            // Assigned again between the declaration and the call: not static.
            if assignment(&reassign, text).is_some() {
                return None;
            }
            continue;
        };
        // The initializer: the rest of this line, plus continuation lines.
        let mut init = text[init_at..].to_string();
        let mut j = i + 1;
        while j < call && j < i + MAX_CALL_LINES {
            let next = lines[j];
            if !continuation.is_match(next.trim_start()) && balanced(&init) {
                break;
            }
            init.push('\n');
            init.push_str(next);
            j += 1;
        }
        return parse_href(&init, literal);
    }
    None
}

/// A route that takes a path segment's value: Expo's `[id]` or the `:id`
/// every other router's routes use.
fn is_param_segment(seg: &str) -> bool {
    (seg.starts_with('[') && seg.ends_with(']')) || (seg.starts_with(':') && !seg.ends_with('*'))
}

/// A route segment that takes the rest of the path: `[...slug]` or `:slug*`.
fn is_catch_all_segment(seg: &str) -> bool {
    (seg.starts_with("[...") && seg.ends_with(']')) || (seg.starts_with(':') && seg.ends_with('*'))
}

/// How well `href`'s segments fit a dynamic `route`'s, or `None` when they
/// do not (`scoreMatch`): a literal segment beats a parameter, which beats a
/// hole.
fn score_match(href: &[String], route: &[String]) -> Option<usize> {
    let mut score = 0;
    let mut i = 0;
    for seg in route {
        if is_catch_all_segment(seg) {
            // Needs at least one segment and takes the rest.
            if i >= href.len() {
                return None;
            }
            score += href.len() - i;
            i = href.len();
            continue;
        }
        let h = href.get(i)?;
        if is_param_segment(seg) {
            score += 2;
        } else if h == seg {
            score += 3;
        } else if h == "*" {
            score += 1;
        } else {
            return None;
        }
        i += 1;
    }
    (i == href.len()).then_some(score)
}

/// A route with a parameter or catch-all segment, by its segments
/// (`RouteEntry`).
#[derive(Debug, Clone)]
pub(crate) struct RouteEntry {
    pub node: Arc<Node>,
    pub segs: Vec<String>,
}

impl RouteEntry {
    /// The route `node` at the path it is named by.
    pub(crate) fn new(node: Arc<Node>) -> Self {
        let segs = node.name.split('/').skip(1).map(str::to_string).collect();
        Self { node, segs }
    }

    /// The route `node` at `path`, one of the paths it serves (React
    /// Router's `/cart` for `/cart/:id?`).
    pub(crate) fn at(node: Arc<Node>, path: &str) -> Self {
        let segs = path.split('/').skip(1).map(str::to_string).collect();
        Self { node, segs }
    }
}

/// A router's routes by the path each serves, with the root of the app each
/// belongs to (`exact`). Several routes may serve one path: two apps of a
/// monorepo each with `/login`, or `app/index.tsx` beside
/// `app/(tabs)/index.tsx`. Upstream keeps the last in node order; the store
/// lists nodes in insertion order, which a sync changes, so a call takes its
/// own app's route (KEEP-RUST).
#[derive(Debug, Default)]
pub(crate) struct ExactTable {
    by_path: BTreeMap<String, Vec<(String, Arc<Node>)>>,
}

impl ExactTable {
    /// Register `node`, which serves `path` in the app at `root`.
    pub(crate) fn insert(&mut self, path: String, root: String, node: Arc<Node>) {
        self.by_path.entry(path).or_default().push((root, node));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// The route at `path` for a call in `caller`: the one route serving it,
    /// else the one of the call's own app (the longest root above `caller`);
    /// several or none there is [`ExactRoute::Ambiguous`].
    pub(crate) fn route(&self, path: &str, caller: &str) -> ExactRoute {
        let Some(routes) = self.by_path.get(path) else {
            return ExactRoute::Absent;
        };
        if let [(_, route)] = routes.as_slice() {
            return ExactRoute::Found(Arc::clone(route));
        }
        let Some(own) = routes
            .iter()
            .map(|(root, _)| root.as_str())
            .filter(|root| caller.starts_with(root))
            .max_by_key(|root| root.len())
        else {
            return ExactRoute::Ambiguous;
        };
        match routes
            .iter()
            .filter(|(root, _)| root == own)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [(_, route)] => ExactRoute::Found(Arc::clone(route)),
            _ => ExactRoute::Ambiguous,
        }
    }
}

/// What a router's table holds at a path an href spells out exactly.
pub(crate) enum ExactRoute {
    /// The route serving it.
    Found(Arc<Node>),
    /// Several routes serve it and none is the call's: the href resolves to
    /// nothing, not to a dynamic route.
    Ambiguous,
    /// No route has this path; a dynamic route may still take it.
    Absent,
}

/// The single route `segs` denote, or `None` when none or several do
/// (`matchRoute`): the route of exactly that path, else the best-scoring
/// dynamic route, where a tie is ambiguity, and ambiguity is nothing.
pub(crate) fn match_route(
    segs: &[String],
    exact: ExactRoute,
    dynamic: &[RouteEntry],
) -> Option<Arc<Node>> {
    match exact {
        ExactRoute::Found(node) => return Some(node),
        ExactRoute::Ambiguous => return None,
        ExactRoute::Absent => {}
    }
    let mut best: Option<(&RouteEntry, usize)> = None;
    let mut tied = false;
    for entry in dynamic {
        let Some(score) = score_match(segs, &entry.segs) else {
            continue;
        };
        match best {
            Some((_, top)) if score == top => tied = true,
            Some((_, top)) if score < top => {}
            _ => {
                best = Some((entry, score));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied)
        .map(|(entry, _)| Arc::clone(&entry.node))
}

/// The path an href's segments spell (`'/' + segs.join('/')`).
pub(crate) fn route_path(segs: &[String]) -> String {
    format!("/{}", segs.join("/"))
}

/// `/users/${…}?tab=x` → `["users", "*"]`; an absolute URL keeps its path; a
/// relative href is nothing (`hrefSegments`).
pub(crate) fn href_segments(href: &HrefLiteral) -> Option<Vec<String>> {
    static ABSOLUTE: OnceLock<Regex> = OnceLock::new();
    let absolute = ABSOLUTE.get_or_init(|| {
        Regex::new(r"(?i)^(?:[a-z][a-z0-9+.\-]*:)?//[^/]*(/.*)?$").expect("absolute href")
    });
    let path = match absolute.captures(&href.path) {
        Some(url) => url.get(1).map_or("/", |path| path.as_str()),
        None => href.path.as_str(),
    };
    if !path.starts_with('/') {
        return None;
    }
    Some(
        path.split('/')
            .skip(1)
            .filter(|seg| !seg.is_empty())
            .map(|seg| {
                if seg.contains(HOLE) {
                    "*".to_string()
                } else {
                    decode_segment(seg)
                }
            })
            .collect(),
    )
}

/// Every route a destination names, one per arm of a conditional, each with
/// the arm that named it (`destinationsForHref`): an edge says the path it
/// took. `exact` and `dynamic` are a router's routes; `caller` is the file
/// the navigation is written in.
pub(crate) fn destinations_for_href<'h>(
    href: &'h HrefLiteral,
    exact: &ExactTable,
    dynamic: &[RouteEntry],
    caller: &str,
) -> Vec<(Arc<Node>, &'h HrefLiteral)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for arm in href.arms() {
        let Some(segs) = href_segments(arm) else {
            continue;
        };
        let exact = exact.route(&route_path(&segs), caller);
        // An arm naming no route drops out; the arms that do name one are
        // still places this navigation goes.
        let Some(target) = match_route(&segs, exact, dynamic) else {
            continue;
        };
        if seen.insert(target.id.clone()) {
            out.push((target, arm));
        }
    }
    out
}

/// What a router's link pass reads of its route table (W7-18a): whether the
/// project has any of its routes, whether a file's app has some, and where an
/// href written in a file goes. The tables stay private to their routers.
pub(crate) trait LinkRoutes: Send + Sync {
    /// Whether the table holds no route (upstream's early `return []`).
    fn is_empty(&self) -> bool;
    /// Whether a link written in `file` can reach a route: its app's routes
    /// are not empty (upstream `routesForFile` and `exact.size`).
    fn serves(&self, file: &str) -> bool;
    /// Every route `href`, written in `file`, names, one per arm
    /// ([`destinations_for_href`] over `file`'s app).
    fn destinations<'h>(
        &self,
        file: &str,
        href: &'h HrefLiteral,
    ) -> Vec<(Arc<Node>, &'h HrefLiteral)>;
    /// The one route of `file`'s app named `name` (Vue's `{ name }`): none
    /// for a name two routes carry, and none for the other routers.
    fn named(&self, _file: &str, _name: &str) -> Option<Arc<Node>> {
        None
    }
}

/// The app a route file belongs to: the directory above its first
/// conventional source directory (`src`, `pages`, `app`, `routes`), else the
/// file's own folder (`appRootFor`). `frontend/src/App.js` is `frontend/`'s,
/// `src/App.tsx` the repository's.
pub(crate) fn app_root_for(file_path: &str) -> String {
    // `^((?:[^/]+\/)*?)(?:src|pages|app|routes)\/`: the first whole segment
    // that is one of those, every segment before it non-empty.
    let mut start = 0;
    while let Some(slash) = file_path[start..].find('/') {
        let segment = &file_path[start..start + slash];
        if segment.is_empty() {
            break;
        }
        if matches!(segment, "src" | "pages" | "app" | "routes") {
            return file_path[..start].to_string();
        }
        start += slash + 1;
    }
    file_path
        .rfind('/')
        .map_or_else(String::new, |slash| file_path[..=slash].to_string())
}

/// `decodeURIComponent`, or the segment as written when that throws: a `%`
/// not followed by two hex digits, or bytes that are not UTF-8.
pub(crate) fn decode_segment(seg: &str) -> String {
    if !seg.contains('%') {
        return seg.to_string();
    }
    let bytes = seg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            let Some(byte) = hex else {
                return seg.to_string();
            };
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| seg.to_string())
}

/// The route tables of one resolution pass, one per router: a navigation
/// call reads its router's table, and no route node changes within a pass
/// (upstream keeps one per context). Production contexts hold one for the
/// pass ([`crate::types::ResolutionContext::route_tables`]).
#[derive(Default)]
pub struct RouteTables {
    tables: Mutex<HashMap<&'static str, Arc<dyn Any + Send + Sync>>>,
}

impl RouteTables {
    /// `router`'s table, built by `build` the first time this pass asks.
    pub(crate) fn get_or_build<T: Any + Send + Sync>(
        &self,
        router: &'static str,
        build: impl FnOnce() -> T,
    ) -> Arc<T> {
        let mut tables = self.tables.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(table) = tables
            .get(router)
            .and_then(|table| Arc::clone(table).downcast::<T>().ok())
        {
            return table;
        }
        let table = Arc::new(build());
        tables.insert(router, Arc::clone(&table) as Arc<dyn Any + Send + Sync>);
        table
    }
}

impl std::fmt::Debug for RouteTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteTables").finish_non_exhaustive()
    }
}

/// A resolution context over a few files and the route nodes a router's
/// extraction made of them, for the routers' unit tests.
#[cfg(test)]
pub(crate) mod test_ctx {
    use std::collections::HashMap;

    use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

    use crate::types::{ImportMapping, ReExport, RefView, ResolutionContext};

    #[derive(Default)]
    pub(crate) struct RouteCtx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
        imports: HashMap<String, Vec<ImportMapping>>,
    }

    impl RouteCtx {
        /// A source file.
        pub(crate) fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self
        }

        /// A route file and the nodes its router extracted from it.
        pub(crate) fn routes(mut self, file: &str, nodes: Vec<Node>) -> Self {
            self.nodes.extend(nodes);
            self.files.entry(file.to_string()).or_default();
            self
        }

        /// An import `file` declares.
        pub(crate) fn import(mut self, file: &str, mapping: ImportMapping) -> Self {
            self.imports
                .entry(file.to_string())
                .or_default()
                .push(mapping);
            self
        }

        /// The id of the route `name` in `file`.
        pub(crate) fn route(&self, name: &str, file: &str) -> String {
            self.nodes
                .iter()
                .find(|n| n.name == name && n.file_path == file)
                .unwrap_or_else(|| panic!("no route {name} in {file}"))
                .id
                .clone()
        }

        pub(crate) fn nodes(&self) -> &[Node] {
            &self.nodes
        }
    }

    impl ResolutionContext for RouteCtx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, q: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == q)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains_key(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.files.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, file: &str, _language: Language) -> Vec<ImportMapping> {
            self.imports.get(file).cloned().unwrap_or_default()
        }
        fn get_re_exports(&self, file: &str, language: Language) -> Vec<ReExport> {
            self.files
                .get(file)
                .map(|content| crate::import_resolver::extract_re_exports(content, language))
                .unwrap_or_default()
        }
    }

    /// A `calls` reference named `name` at `line`:`column` of `file`.
    pub(crate) fn call_in(file: &str, name: &str, line: i64, column: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "function:nav".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line,
            column,
            file_path: file.to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(src: &str, method: &str, line: i64, column: i64) -> Option<HrefLiteral> {
        let lines: Vec<&str> = src.split('\n').collect();
        first_argument_text(&lines, line, column, method)
            .and_then(|arg| parse_href_expression(&arg))
    }

    fn href(path: &str, display: &str) -> HrefLiteral {
        HrefLiteral {
            path: path.to_string(),
            display: display.to_string(),
            alternates: Vec::new(),
        }
    }

    fn paths(href: &HrefLiteral) -> Vec<&str> {
        href.arms().map(|arm| arm.path.as_str()).collect()
    }

    /// Upstream `expo-router.test.ts`, `defaultExportName`.
    #[test]
    fn the_default_export_is_named_through_a_wrapper() {
        for (src, name) in [
            ("export default function ObjectDetail() {}", "ObjectDetail"),
            ("export default async function Screen() {}", "Screen"),
            (
                "export default class Legacy extends React.Component {}",
                "Legacy",
            ),
            ("function Home() {}\nexport default Home", "Home"),
            ("function Home() {}\nexport default Home;", "Home"),
            ("function Home() {}\r\nexport default Home;\r\n", "Home"),
            ("export default memo(Home)", "Home"),
            ("export default React.memo(Home)", "Home"),
            ("export default observer(Home, opts)", "Home"),
            ("export { Home as default }", "Home"),
        ] {
            assert_eq!(
                default_export_name(src).map(|(name, _)| name).as_deref(),
                Some(name),
                "{src}"
            );
        }
        assert_eq!(
            default_export_name("const x = 1;\nexport default function Page() {}"),
            Some(("Page".to_string(), 13))
        );
        assert_eq!(default_export_name("export default () => null"), None);
        assert_eq!(default_export_name("export default function () {}"), None);
    }

    /// Upstream `expo-router.test.ts`, `readHrefArgument`.
    #[test]
    fn an_href_argument_is_a_string_a_template_or_a_pathname() {
        assert_eq!(
            read("router.push('/capture-queue')", "push", 1, 0),
            Some(href("/capture-queue", "/capture-queue"))
        );
        assert_eq!(
            read(
                "router.push(\"/sheets/setup-guide?kind=lighting\")",
                "push",
                1,
                0
            ),
            Some(href(
                "/sheets/setup-guide",
                "/sheets/setup-guide?kind=lighting"
            ))
        );
        assert_eq!(
            read(
                "router.navigate(\n  `/object-detail?detectionItem=${encodeParam(JSON.stringify(item))}${folderParam}` as any\n)",
                "navigate",
                1,
                0
            ),
            Some(href(
                "/object-detail",
                "/object-detail?detectionItem=${…}${…}"
            ))
        );
        let hole = read("router.push(`/terms-of-service/term/${id}`)", "push", 1, 0).unwrap();
        assert_eq!(hole.display, "/terms-of-service/term/${…}");
        assert_eq!(hole.path, format!("/terms-of-service/term/{HOLE}"));
        assert_eq!(
            read(
                "router.push({\n  pathname: '/detection/result/[id]',\n  params: { id: result.id },\n})",
                "push",
                1,
                0
            ),
            Some(href("/detection/result/[id]", "/detection/result/[id]"))
        );
        assert_eq!(
            read("router.push('/home', { withAnchor: true })", "push", 1, 0).map(|href| href.path),
            Some("/home".to_string())
        );
        let src = "list.push(x); router.push('/home')";
        let column = src.find("router").unwrap() as i64;
        assert_eq!(
            read(src, "push", 1, column).map(|href| href.path),
            Some("/home".to_string())
        );
        // A multibyte character before the call: the column is a byte offset.
        let src = "const é = 1; router.push('/home')";
        let column = src.find("router").unwrap() as i64;
        assert_eq!(
            read(src, "push", 1, column).map(|href| href.path),
            Some("/home".to_string())
        );
        for src in [
            "router.push(href)",
            "router.push(buildHref(item))",
            "router.push({ pathname, params })",
            "router.push()",
            "router.push",
        ] {
            assert_eq!(read(src, "push", 1, 0), None, "{src}");
        }
        // The call ends where its parenthesis closes.
        assert_eq!(
            read("router.back()\nrouter.push('/home')", "back", 1, 0),
            None
        );
        assert_eq!(read("router.push('/home')", "push", 2, 0), None);
    }

    /// Upstream `expo-router.test.ts`, the conditional cases.
    #[test]
    fn every_readable_arm_of_a_conditional_is_a_destination() {
        let both = read(
            "router.navigate(\n  (folder.id\n    ? `/sheets/create-detection-item?folderId=${folder.id}`\n    : '/sheets/create-detection-item') as any\n)",
            "navigate",
            1,
            0,
        )
        .unwrap();
        assert_eq!(both.path, "/sheets/create-detection-item");
        assert_eq!(both.display, "/sheets/create-detection-item?folderId=${…}");
        assert_eq!(
            paths(&both),
            [
                "/sheets/create-detection-item",
                "/sheets/create-detection-item"
            ]
        );
        let one = read("router.push(ready ? '/home' : fallback)", "push", 1, 0).unwrap();
        assert_eq!(paths(&one), ["/home"]);
        assert_eq!(
            read("router.push(ready ? fallback : '/home')", "push", 1, 0).map(|href| href.path),
            Some("/home".to_string())
        );
        assert_eq!(read("router.push(ready ? a : b)", "push", 1, 0), None);
        let nested = read(
            "router.push(!isAdmin ? keyword ? '/search' : '/page' : '/admin')",
            "push",
            1,
            0,
        )
        .unwrap();
        assert_eq!(paths(&nested), ["/search", "/page", "/admin"]);
        // `?.` and `??` open no conditional.
        assert_eq!(read("router.push(user?.id ?? '/home')", "push", 1, 0), None);
    }

    /// Upstream `expo-router.test.ts`, `readHrefViaLocal`.
    #[test]
    fn a_local_variable_names_the_href_its_static_initializer_does() {
        let via_local = |src: &str| {
            let lines: Vec<&str> = src.split('\n').collect();
            let line = lines
                .iter()
                .position(|line| line.contains(".navigate("))
                .unwrap() as i64
                + 1;
            read_href_via_local(&lines, line, 0, "navigate", 1)
        };
        assert_eq!(
            via_local("function f() {\n  const href = '/home'\n  router.navigate(href as any)\n}")
                .map(|href| href.path),
            Some("/home".to_string())
        );
        let ternary = via_local(
            "function f(params) {\n  const href = params.length\n    ? `/barcode-scan?${params.join(\"&\")}`\n    : '/barcode-scan'\n  if (options?.replace) {\n    router.navigate(href as any)\n  }\n}",
        )
        .unwrap();
        assert_eq!(paths(&ternary), ["/barcode-scan", "/barcode-scan"]);
        assert_eq!(
            via_local("const href: Href = '/home'\nrouter.navigate(href)").map(|href| href.path),
            Some("/home".to_string())
        );
        assert_eq!(
            via_local(
                "const href = { pathname: '/item/[id]', params: { id } }\nrouter.navigate(href)"
            )
            .map(|href| href.path),
            Some("/item/[id]".to_string())
        );
        for src in [
            "const href = build()\nrouter.navigate(href)",
            "const href = '/home'\nhref = other\nrouter.navigate(href)",
            "router.navigate(a.b)",
            "const href = options?.href ?? '/home'\nrouter.navigate(href)",
            "router.navigate(href)",
        ] {
            assert_eq!(via_local(src), None, "{src}");
        }
        // A comparison is no reassignment; a member of that name is no
        // declaration.
        assert_eq!(
            via_local("const href = '/home'\nif (href == null) {}\nrouter.navigate(href)")
                .map(|href| href.path),
            Some("/home".to_string())
        );
        assert_eq!(
            via_local("const href = '/home'\nthis.href = other\nrouter.navigate(href)")
                .map(|href| href.path),
            Some("/home".to_string())
        );
        // The declaration must sit inside the enclosing function.
        let lines = [
            "const href = '/home'",
            "function f() {",
            "  router.navigate(href)",
            "}",
        ];
        assert_eq!(read_href_via_local(&lines, 3, 0, "navigate", 2), None);
        assert_eq!(
            read_href_via_local(&lines, 3, 0, "navigate", 1).map(|href| href.path),
            Some("/home".to_string())
        );
    }

    fn entry(name: &str) -> RouteEntry {
        let mut node = crate::frameworks::framework_node(
            format!("route:x:{name}"),
            codegraph_core::types::NodeKind::Route,
            name.to_string(),
            name.to_string(),
            "x".to_string(),
            1,
            1,
            0,
            0,
            codegraph_core::types::Language::Tsx,
            true,
        );
        node.updated_at = 0;
        RouteEntry::new(Arc::new(node))
    }

    fn segs(path: &str) -> Vec<String> {
        path.split('/')
            .skip(1)
            .filter(|seg| !seg.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_literal_segment_beats_a_parameter_and_a_tie_is_nothing() {
        let dynamic = [
            entry("/users/:id"),
            entry("/users/:id/edit"),
            entry("/docs/:all*"),
            entry("/item/[id]"),
            entry("/shop/[...slug]"),
        ];
        let found = |path: &str| {
            match_route(&segs(path), ExactRoute::Absent, &dynamic).map(|node| node.name.clone())
        };
        assert_eq!(found("/users/7").as_deref(), Some("/users/:id"));
        assert_eq!(found("/users/*").as_deref(), Some("/users/:id"));
        assert_eq!(found("/users/7/edit").as_deref(), Some("/users/:id/edit"));
        assert_eq!(found("/docs/a/b").as_deref(), Some("/docs/:all*"));
        assert_eq!(found("/item/3").as_deref(), Some("/item/[id]"));
        assert_eq!(found("/shop/a/b").as_deref(), Some("/shop/[...slug]"));
        // A catch-all needs a segment.
        assert_eq!(found("/docs"), None);
        assert_eq!(found("/users"), None);
        assert_eq!(found("/nowhere/1"), None);
        // Two routes fitting as well: nothing.
        let tied = [entry("/a/:x"), entry("/:y/b")];
        assert_eq!(match_route(&segs("/a/b"), ExactRoute::Absent, &tied), None);
        // The exact route wins, and an ambiguous exact path is nothing.
        let exact = entry("/users/new").node;
        assert_eq!(
            match_route(
                &segs("/users/new"),
                ExactRoute::Found(Arc::clone(&exact)),
                &dynamic
            ),
            Some(exact)
        );
        assert_eq!(
            match_route(&segs("/users/new"), ExactRoute::Ambiguous, &dynamic),
            None
        );
    }

    #[test]
    fn segments_decode_as_decode_uri_component_does() {
        assert_eq!(decode_segment("a%20b"), "a b");
        assert_eq!(decode_segment("%E2%9C%93"), "✓");
        assert_eq!(decode_segment("plain"), "plain");
        // What decodeURIComponent throws on is kept as written.
        assert_eq!(decode_segment("100%"), "100%");
        assert_eq!(decode_segment("%zz"), "%zz");
        assert_eq!(decode_segment("%E2%9C"), "%E2%9C");
    }

    #[test]
    fn a_pass_builds_each_routers_table_once() {
        let tables = RouteTables::default();
        let mut built = 0;
        let first = tables.get_or_build("nextjs", || {
            built += 1;
            7u32
        });
        let second = tables.get_or_build("nextjs", || {
            built += 1;
            8u32
        });
        assert_eq!((*first, *second, built), (7, 7, 1));
        assert_eq!(*tables.get_or_build("expo", || 9u32), 9);
    }
}
