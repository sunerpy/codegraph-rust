//! Scanning JavaScript source for the framework resolvers that read a
//! declaration's text (upstream `frameworks/object-literal.ts` and the
//! statement helpers of `frameworks/react.ts`).
//!
//! This is a scanner, not a parser: it knows brackets, quotes and template
//! interpolation, and nothing else about JavaScript. Every offset is a byte
//! offset into the text handed in, and every byte it stops at is ASCII, so a
//! slice there is always on a character boundary. Upstream counts UTF-16
//! code units; the bytes it compares are ASCII either way.

/// The offset of the bracket, brace or paren closing the one at `open`, or
/// `None` (upstream `matchBracket`).
pub(crate) fn match_bracket(s: &str, open: usize) -> Option<usize> {
    scan(s, open, Frame::Brackets(0))
}

/// The offset of the quote closing the string or template opened at `at`, or
/// `None` (upstream `skipString`).
pub(crate) fn skip_string(s: &str, at: usize) -> Option<usize> {
    let quote = *s.as_bytes().get(at)?;
    scan(s, at + 1, Frame::Quoted(quote))
}

/// What [`scan`] is inside: brackets at a depth, or a string, template or
/// character literal opened by its quote.
#[derive(Clone, Copy)]
enum Frame {
    Brackets(usize),
    Quoted(u8),
}

/// Where the construct `outer` opens, scanning from `from`, closes.
///
/// Upstream's `matchBracket` and `skipString` call each other for a string
/// inside brackets and a `${…}` inside a template; this keeps the nesting on
/// a heap stack instead, so no input runs it out of call stack, with the
/// same answers: brackets count every kind together, and an interpolation
/// ends at the first closer that balances its `{`.
fn scan(s: &str, from: usize, outer: Frame) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut frames = vec![outer];
    let mut i = from;
    while i < bytes.len() {
        let ch = bytes[i];
        let top = frames.last_mut()?;
        match top {
            Frame::Brackets(depth) => match ch {
                b'"' | b'\'' | b'`' => frames.push(Frame::Quoted(ch)),
                b'[' | b'{' | b'(' => *depth += 1,
                b']' | b'}' | b')' => {
                    *depth = depth.checked_sub(1)?;
                    if *depth == 0 {
                        frames.pop();
                        if frames.is_empty() {
                            return Some(i);
                        }
                    }
                }
                _ => {}
            },
            Frame::Quoted(quote) => {
                let quote = *quote;
                if ch == b'\\' {
                    i += 2;
                    continue;
                }
                if ch == quote {
                    frames.pop();
                    if frames.is_empty() {
                        return Some(i);
                    }
                } else if quote == b'`' && ch == b'$' && bytes.get(i + 1) == Some(&b'{') {
                    // The `{` opens the interpolation's brackets.
                    frames.push(Frame::Brackets(0));
                }
            }
        }
        i += 1;
    }
    None
}

/// The comma-separated items written directly in `text`, trimmed, an empty
/// item kept for each hole (`[, second]` is `["", "second"]`) but none for a
/// trailing comma (upstream `topLevelItems`).
pub(crate) fn top_level_items(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0isize;
    let mut from = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => match skip_string(text, i) {
                Some(end) => i = end,
                None => break,
            },
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(text[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = text[from.min(text.len())..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

/// Where a declarator's initializer starts in `body`, scanning from `at`
/// past its `: Type` and `=`; `None` when it has none (upstream
/// `initializerAt`).
pub(crate) fn initializer_at(body: &str, at: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut i = at;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(body, i)?,
            b'{' | b'(' | b'[' => i = match_bracket(body, i)?,
            b'=' => {
                if matches!(bytes.get(i + 1), Some(b'>' | b'=')) {
                    i += 1;
                } else {
                    return Some(i + 1);
                }
            }
            b';' | b',' | b'}' | b')' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Where the statement or declarator from `from` ends: at a `;`, a closing
/// bracket, a `,` when `comma`, or at a line break neither line continues
/// across, where JavaScript ends a statement without a semicolon (upstream
/// `statementEnd`).
pub(crate) fn statement_end(text: &str, from: usize, comma: bool) -> usize {
    const CONTINUES_AFTER: &[u8] = b"=(,[{?:&|+-*/%<>!.";
    const CONTINUES_BEFORE: &[u8] = b".?:&|+-*/%=<>,([";
    let bytes = text.as_bytes();
    let mut last: Option<u8> = None;
    let mut i = from;
    while i < bytes.len() {
        let ch = bytes[i];
        if matches!(ch, b'"' | b'\'' | b'`' | b'{' | b'(' | b'[') {
            let end = if matches!(ch, b'{' | b'(' | b'[') {
                match_bracket(text, i)
            } else {
                skip_string(text, i)
            };
            let Some(end) = end else {
                return text.len();
            };
            i = end;
            last = Some(bytes[end]);
            i += 1;
            continue;
        }
        if matches!(ch, b';' | b'}' | b')' | b']') || (comma && ch == b',') {
            return i;
        }
        if ch == b'\n' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if let Some(last) = last
                && !CONTINUES_AFTER.contains(&last)
                && !bytes
                    .get(j)
                    .is_some_and(|next| CONTINUES_BEFORE.contains(next))
            {
                return i;
            }
            i += 1;
            continue;
        }
        if !ch.is_ascii_whitespace() {
            last = Some(ch);
        }
        i += 1;
    }
    text.len()
}

/// Whether `text` is one JavaScript identifier (ASCII form).
pub(crate) fn is_identifier(text: &str) -> bool {
    let mut chars = text.bytes();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_' || first == b'$')
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

/// The offset of the `(` after the callee a wrapper call starts with —
/// `observer`, `React.memo`, `forwardRef<Props>` — or `None` when `s`
/// starts with no such callee (upstream's
/// `^[A-Za-z_$][\w$]*(?:\s*\.\s*[A-Za-z_$][\w$]*)*\s*(?:<[^()]*>)?\s*(?=\()`).
fn callee_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let ident = |at: usize| -> Option<usize> {
        let first = *bytes.get(at)?;
        if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
            return None;
        }
        let mut end = at + 1;
        while bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
        {
            end += 1;
        }
        Some(end)
    };
    let skip_ws = |mut at: usize| {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        at
    };
    let mut end = ident(0)?;
    loop {
        let dot = skip_ws(end);
        if bytes.get(dot) != Some(&b'.') {
            break;
        }
        match ident(skip_ws(dot + 1)) {
            Some(next) => end = next,
            None => break,
        }
    }
    let at = skip_ws(end);
    if bytes.get(at) != Some(&b'<') {
        return (bytes.get(at) == Some(&b'(')).then_some(at);
    }
    // `<[^()]*>\s*` before the `(`: the type arguments run to the first
    // paren, which must open the call, and end at the `>` before it.
    let paren = at + bytes[at..].iter().position(|b| matches!(b, b'(' | b')'))?;
    if bytes[paren] != b'(' {
        return None;
    }
    let close = bytes[..paren]
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())?;
    (close > at && bytes[close] == b'>').then_some(paren)
}

/// The component a wrapper call hands over — `observer(Login)`,
/// `React.memo(Login)`, `connect(mapState)(Login)`,
/// `withTranslation()(withRouter(Login))` — its last argument list's lone
/// argument, or a component ahead of the options after it
/// (`React.memo(TableBody, areEqual)`), past a type assertion on the call
/// (`forwardRef(Inner) as { … }`). `None` for any other expression (upstream
/// `wrappedName`).
pub(crate) fn wrapped_name(expression: &str) -> Option<&str> {
    // Upstream recurses into the argument; the loop keeps no stack.
    let mut s = expression.trim();
    loop {
        let bytes = s.as_bytes();
        let mut open = callee_end(s)?;
        let mut end = open;
        let mut arguments: Vec<&str> = Vec::new();
        while bytes.get(open) == Some(&b'(') {
            let close = match_bracket(s, open)?;
            arguments = top_level_items(&s[open + 1..close]);
            end = close + 1;
            open = end;
            while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                open += 1;
            }
        }
        if !is_type_assertion_or_empty(&s[end..]) {
            return None;
        }
        let first = arguments.first().copied().unwrap_or("");
        // `connect(mapState, mapDispatch)` hands over no component yet.
        if arguments.len() > 1 && !first.starts_with(|c: char| c.is_ascii_uppercase()) {
            return None;
        }
        if is_identifier(first) {
            return Some(first);
        }
        if first.is_empty() {
            return None;
        }
        s = first;
    }
}

/// Whether `rest`, what follows a wrapper call, is nothing or a type
/// assertion on it (upstream's `^(?:\s+(?:as|satisfies)\s[\s\S]*)?$`).
fn is_type_assertion_or_empty(rest: &str) -> bool {
    if rest.is_empty() {
        return true;
    }
    let assertion = rest.trim_start();
    assertion.len() < rest.len()
        && ["as", "satisfies"].iter().any(|keyword| {
            assertion
                .strip_prefix(keyword)
                .is_some_and(|after| after.starts_with(char::is_whitespace))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_and_strings_match_across_templates() {
        let s = "f(a, `x${g(1)}y`, ')') + 1";
        assert_eq!(match_bracket(s, 1), Some(21));
        assert_eq!(skip_string("'a\\'b'", 0), Some(5));
        assert_eq!(skip_string("`a${`b${c}`}d` + e", 0), Some(13));
        assert_eq!(match_bracket("(unclosed", 0), None);
        assert_eq!(skip_string("`a${b", 0), None);
        // An interpolation ends at the first closer that balances its `{`,
        // whatever its kind, as upstream's does.
        assert_eq!(skip_string("`${a)`}`", 0), Some(5));
    }

    #[test]
    fn deep_nesting_needs_no_call_stack() {
        let depth = 200_000;
        let nested = format!("{}X{}", "a(".repeat(depth), ")".repeat(depth));
        assert_eq!(match_bracket(&nested, 1), Some(nested.len() - 1));
        let templates = format!("{}{}", "`${".repeat(depth), "}`".repeat(depth));
        assert_eq!(skip_string(&templates, 0), Some(templates.len() - 1));
        // Each wrapper rescans what it wraps, so keep this one short.
        let wrappers = format!("{}X{}", "a(".repeat(2_000), ")".repeat(2_000));
        assert_eq!(wrapped_name(&wrappers), Some("X"));
    }

    #[test]
    fn top_level_items_keep_holes_but_no_trailing_comma() {
        assert_eq!(
            top_level_items(" a, f(b, c), [d, e] ,"),
            ["a", "f(b, c)", "[d, e]"]
        );
        assert_eq!(top_level_items(", second"), ["", "second"]);
    }

    #[test]
    fn an_initializer_starts_past_its_type_and_equals() {
        let s = "Panel: FC<{ a: () => void }> = observer(Inner)";
        assert_eq!(&s[initializer_at(s, 5).unwrap()..], " observer(Inner)");
        assert_eq!(initializer_at("Panel;", 5), None);
        assert_eq!(initializer_at("x => y", 1), None);
    }

    #[test]
    fn a_statement_ends_where_no_line_continues() {
        let s = "observer(Inner)\nconst next = 1";
        assert_eq!(statement_end(s, 0, false), 15);
        let s = "observer(\n  Inner,\n)\n  .x";
        assert_eq!(statement_end(s, 0, false), s.len());
        assert_eq!(statement_end("a; b", 0, false), 1);
    }

    #[test]
    fn the_wrapped_name_is_the_component_a_wrapper_hands_over() {
        for (expression, name) in [
            ("observer(Login)", Some("Login")),
            ("React.memo(Login)", Some("Login")),
            ("connect(mapState)(Login)", Some("Login")),
            ("withTranslation()(withRouter(Login))", Some("Login")),
            ("React.memo(TableBody, areEqual)", Some("TableBody")),
            (
                "forwardRef(Inner) as { (props: P): JSX.Element }",
                Some("Inner"),
            ),
            ("forwardRef<HTMLDivElement, Props>(Inner)", Some("Inner")),
            ("forwardRef<A<B>, C> (Inner)", Some("Inner")),
            ("observer(Login) satisfies FC", Some("Login")),
            ("a<b>c(Login)", None),
            ("observer(Login)as FC", None),
            ("connect(mapState, mapDispatch)", None),
            ("lazy(() => import('./x'))", None),
            ("observer(Login) + 1", None),
            ("Login", None),
        ] {
            assert_eq!(wrapped_name(expression), name, "{expression}");
        }
    }
}
