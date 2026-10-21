//! How a Go call is written, read back from its line (W4-GO-03).
//!
//! The extractor keeps one receiver level of a call (#1496), and records a
//! call on an expression — `child.Flags().String("f", …)`,
//! `w.Header().Get(…)` — by its bare name at the column where the expression
//! starts. What can answer such a call is read from the source instead
//! (upstream #2167's Go half, with #2177's later-line links and `Header()`).

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::types::{RefView, ResolutionContext};

/// How a bare-named Go call is written (upstream `rustGoCallShape`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoCallShape {
    /// After `::`: left to the path strategies.
    Path,
    /// No `.` before it: a function, a conversion, a closure.
    Bare,
    /// After a `.`, on the receiver written before it, with call and type
    /// arguments dropped (`c.Flags`, `JSON`).
    Chained { receiver: String },
}

/// Methods of Go's standard types and interfaces — `fmt.Stringer`, `error`,
/// `http.ResponseWriter`, `sync` locks, `reflect`, `time` (upstream
/// `GO_STD_METHODS`). Ones a project type often carries itself (`Get`, `Set`,
/// `Close`, `Value`, `Next`) are left out: gin's `c.Set(…)` is its Context's.
pub(crate) const GO_STD_METHODS: [&str; 33] = [
    "String",
    "Error",
    "Unwrap",
    "Is",
    "As",
    "Header",
    "WriteHeader",
    "WriteString",
    "Lock",
    "Unlock",
    "RLock",
    "RUnlock",
    "Err",
    "Deadline",
    "Int",
    "Bool",
    "Float64",
    "Int64",
    "Uint64",
    "Bytes",
    "Len",
    "Cap",
    "Seconds",
    "Unix",
    "Before",
    "After",
    "Equal",
    "IsNil",
    "IsValid",
    "Elem",
    "NumField",
    "Interface",
    "Kind",
];

/// Whether `name` is one of Go's standard-library method names.
pub(crate) fn is_go_std_method(name: &str) -> bool {
    GO_STD_METHODS.contains(&name)
}

fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The offset of the first `name(` (or `name::<`, `name!`) in `line` that no
/// word character or `$` precedes (upstream's
/// `(?<![\w$])name\s*(?:\(|::<|!)`).
fn call_of(line: &str, name: &str) -> Option<usize> {
    line.match_indices(name).map(|(at, _)| at).find(|&at| {
        let free = line[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !(is_word_char(before) || before == '$'));
        let after = line[at + name.len()..].trim_start();
        free && (after.starts_with('(') || after.starts_with("::<") || after.starts_with('!'))
    })
}

/// How the Go call `reference` is written at its site, `None` when it is no
/// bare-named Go call or its line does not spell it.
pub(crate) fn go_call_shape(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<GoCallShape> {
    let name = reference.reference_name.as_str();
    if reference.language != Language::Go
        || reference.reference_kind != EdgeKind::Calls
        || !is_identifier(name)
    {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line.checked_sub(1)?).ok()?;
    let first = facts.raw_line(index)?;
    let column = usize::try_from(reference.column.max(0)).ok()?;
    let mut line = first;
    let mut start = if first
        .get(column..)
        .is_some_and(|rest| rest.starts_with(name))
    {
        Some(column)
    } else {
        call_of(first, name)
    };
    // A link further down a chain the call's line starts — `New()\n\t.Use(…)`
    // — is recorded at the chain's first line (upstream #2177).
    let mut next = index + 1;
    while start.is_none() && next < index + 21 {
        let Some(candidate) = facts.raw_line(next) else {
            break;
        };
        next += 1;
        if !candidate.trim_start().starts_with('.') {
            continue;
        }
        if let Some(at) = call_of(candidate, name) {
            // Named after the chain's head.
            if candidate[..at].trim() == "." {
                let head = first.trim_end().trim_end_matches(['?', ';']);
                return Some(GoCallShape::Chained {
                    receiver: go_receiver_name(head),
                });
            }
            line = candidate;
            start = Some(at);
        }
    }
    let before = &line[..start?];
    let trimmed = before.trim_end();
    if trimmed.ends_with("::") {
        return Some(GoCallShape::Path);
    }
    if !trimmed.ends_with('.') {
        return Some(GoCallShape::Bare);
    }
    Some(GoCallShape::Chained {
        receiver: go_receiver_name(before),
    })
}

/// The receiver a Go `….name(` is written on, as its dotted identifiers with
/// arguments dropped — `c.Flags()` → `c.Flags`, a composite literal
/// `(JSON{data})` → `JSON` — read backwards to the expression's start
/// (upstream `rustGoReceiverName`).
fn go_receiver_name(text: &str) -> String {
    let src = text.trim_end();
    let src = src.strip_suffix('.').unwrap_or(src).trim_end();
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut i = chars.len();
    while i > 0 {
        let ch = chars[i - 1];
        if matches!(ch, ')' | ']' | '}') {
            let open = match ch {
                ')' => '(',
                ']' => '[',
                _ => '{',
            };
            let mut depth = 0usize;
            let mut j = i;
            let mut found = None;
            while j > 0 {
                let at = chars[j - 1];
                if at == ch {
                    depth += 1;
                } else if at == open {
                    depth -= 1;
                    if depth == 0 {
                        found = Some(j - 1);
                        break;
                    }
                }
                j -= 1;
            }
            let Some(open_at) = found else {
                return String::new();
            };
            // `(JSON{data})` — a parenthesized composite literal names its
            // type.
            if ch == ')' && out.is_empty() {
                let group: String = chars[open_at..i].iter().collect();
                if let Some(literal) = composite_literal_type(&group) {
                    return literal;
                }
            }
            i = open_at;
        } else if is_word_char(ch) || matches!(ch, '$' | '.' | ':' | '!') {
            if ch != '!' {
                out.push(ch);
            }
            i -= 1;
        } else {
            break;
        }
    }
    out.reverse();
    let out: String = out.into_iter().collect();
    out.trim_matches(['.', ':']).to_string()
}

/// The type of a parenthesized composite literal, `(&JSON{data})` → `JSON`
/// (upstream's `/^\(\s*&?\s*([A-Za-z_][\w.]*)\s*\{/`).
fn composite_literal_type(group: &str) -> Option<String> {
    let rest = group.strip_prefix('(')?.trim_start();
    let rest = rest.strip_prefix('&').unwrap_or(rest).trim_start();
    let first = rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = rest
        .find(|c: char| !(is_word_char(c) || c == '.'))
        .unwrap_or(rest.len());
    rest[end..]
        .trim_start()
        .starts_with('{')
        .then(|| rest[..end].to_string())
}

/// Whether a Go call of that shape can mean `node` (upstream
/// `isRustGoCallTarget`, its Go half). A bare call never reaches a method,
/// which Go reaches only through a receiver. A chained call reaches a method,
/// never a free function, and a standard-library method name only on a
/// receiver named after its owner: cobra's `c.Flags().String(…)` is no test
/// type's `String`, but `c.Root().Name()` is `Command::Name`. On
/// `w.Header()` or `r.Header`, `Get` / `Set` / … are net/http's Header map's
/// (upstream #2177).
pub(crate) fn is_go_call_target(node: &Node, shape: &GoCallShape) -> bool {
    let member = node.kind == NodeKind::Method;
    match shape {
        GoCallShape::Path => true,
        GoCallShape::Bare => !member,
        GoCallShape::Chained { receiver } => {
            if !member {
                return node.kind != NodeKind::Function;
            }
            if node.language == Language::Go
                && (receiver == "Header" || receiver.ends_with(".Header"))
                && matches!(
                    node.name.as_str(),
                    "Get" | "Set" | "Add" | "Del" | "Values" | "Clone" | "Write"
                )
            {
                return false;
            }
            !is_go_std_method(&node.name)
                || matches!(receiver.as_str(), "self" | "Self")
                || (!receiver.is_empty()
                    && crate::kotlin_scope::shares_receiver_word(receiver, node))
        }
    }
}

/// How many lines past a Go call's first its chain is read on (upstream
/// `GO_CHAIN_LINES`).
const GO_CHAIN_LINES: usize = 30;

/// One link of a Go expression's chain (upstream `GoChainLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChainLink {
    /// The operand: a name or a parenthesized expression.
    Operand(String),
    /// `.name`.
    Select(String),
    /// `.(T)`, with the type as written.
    Assert(String),
    /// `(…)`.
    Call,
    /// `[…]`.
    Index,
}

/// The type a Go call's receiver is asserted to, as written (`KVServer`,
/// `*pipe`, `storage.Store`; upstream `goAssertedType`, #2444). Every call of a
/// chain records the chain's start, so the call is a `.name(…)` link of the
/// chain read from there, and the link before it is the assertion. Two links
/// of one name in a chain (`b.(*Builder).Add(1).Add(2)`) are told apart by
/// nothing, and both are taken for the one made through the assertion: that
/// call is there either way. `None` when no link of the name follows an
/// assertion, or no chain starts at the column.
pub(crate) fn go_asserted_type(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line.checked_sub(1)?).ok()?;
    if index >= facts.line_count() {
        return None;
    }
    let line = facts.line(index);
    let column = usize::try_from(reference.column.max(0)).ok()?;
    if column >= line.len() || !line.is_char_boundary(column) {
        return None;
    }
    // Most calls through an expression assert nothing: no `.(` after the
    // column, nor a chain that goes on past the line.
    if !line[column..].contains(".(") && !line.trim_end().ends_with('.') {
        return None;
    }
    let end = (index + GO_CHAIN_LINES + 1).min(facts.line_count());
    let text = facts.join_lines(index, end, "\n");
    let links = go_chain_links(&text, column);
    links.windows(3).find_map(|window| match window {
        [
            ChainLink::Assert(asserted),
            ChainLink::Select(name),
            ChainLink::Call,
        ] if *name == reference.reference_name => Some(asserted.clone()),
        _ => None,
    })
}

/// The links of the Go expression that starts at `start`: its operand (a name
/// or a parenthesized expression), then each selector, type assertion, call
/// and index. A line break ends it, except right after a `.`, where Go
/// inserts no semicolon (upstream `goChainLinks`).
fn go_chain_links(text: &str, start: usize) -> Vec<ChainLink> {
    let bytes = text.as_bytes();
    let mut links = Vec::new();
    let mut i = start;
    let name = |i: &mut usize| -> Option<String> {
        let first = *bytes.get(*i)?;
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return None;
        }
        let begin = *i;
        while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
            *i += 1;
        }
        Some(text[begin..*i].to_string())
    };
    let group = |i: &mut usize| -> Option<String> {
        let close = go_closing_bracket(text, *i)?;
        let inner = text[*i + 1..close].to_string();
        *i = close + 1;
        Some(inner)
    };
    let operand = if bytes.get(i) == Some(&b'(') {
        group(&mut i)
    } else {
        name(&mut i)
    };
    let Some(operand) = operand else {
        return links;
    };
    links.push(ChainLink::Operand(operand));
    loop {
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        match bytes.get(i) {
            Some(b'.') => {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'(') {
                    match group(&mut i) {
                        Some(asserted) => {
                            links.push(ChainLink::Assert(asserted.trim().to_string()))
                        }
                        None => break,
                    }
                } else {
                    match name(&mut i) {
                        Some(member) => links.push(ChainLink::Select(member)),
                        None => break,
                    }
                }
            }
            Some(b'(') => {
                // A call whose arguments run past what was read is still a
                // call.
                links.push(ChainLink::Call);
                if group(&mut i).is_none() {
                    break;
                }
            }
            Some(b'[') => {
                links.push(ChainLink::Index);
                if group(&mut i).is_none() {
                    break;
                }
            }
            _ => break,
        }
    }
    links
}

/// The bracket closing the one at `open`, past Go strings, runes and
/// comments; `None` when it does not close (upstream `goClosingBracket`).
fn go_closing_bracket(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'"' | b'\'') => {
                i += 1;
                while i < bytes.len() && bytes[i] != quote && bytes[i] != b'\n' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'`' => {
                i += 1 + text[i + 1..].find('`')?;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += text[i..].find('\n')?;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2 + text[i + 2..].find("*/")?;
                i += 1;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
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

/// An asserted type as written: `KVServer`, `*pipe`, `storage.Store`,
/// `*List[int]` → `(qualifier, name)`; `None` for a type literal
/// (`interface{ Flush() }`; upstream's
/// `/^\*?\s*(?:([A-Za-z_]\w*)\s*\.\s*)?([A-Za-z_]\w*)\s*(?:\[[\s\S]*\])?$/`).
pub(crate) fn asserted_type_name(asserted: &str) -> Option<(Option<&str>, &str)> {
    let rest = asserted.strip_prefix('*').unwrap_or(asserted).trim_start();
    let word = |text: &str| -> usize {
        let bytes = text.as_bytes();
        if !bytes
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return 0;
        }
        bytes
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
            .unwrap_or(bytes.len())
    };
    let first_end = word(rest);
    if first_end == 0 {
        return None;
    }
    let (first, mut tail) = rest.split_at(first_end);
    let mut result = (None, first);
    let after = tail.trim_start();
    if let Some(after_dot) = after.strip_prefix('.') {
        let after_dot = after_dot.trim_start();
        let second_end = word(after_dot);
        if second_end == 0 {
            return None;
        }
        let (second, rest) = after_dot.split_at(second_end);
        result = (Some(first), second);
        tail = rest;
    }
    let tail = tail.trim();
    if tail.is_empty() || (tail.starts_with('[') && tail.ends_with(']')) {
        Some(result)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_is_read_through_assertions_calls_and_indexes() {
        let text = "srv.(KVServer).Range(ctx, in)";
        assert_eq!(
            go_chain_links(text, 0),
            [
                ChainLink::Operand("srv".into()),
                ChainLink::Assert("KVServer".into()),
                ChainLink::Select("Range".into()),
                ChainLink::Call,
            ]
        );
        let text = "c.Reader.(*pipe).Close() // a \")\" here";
        assert_eq!(
            go_chain_links(text, 0)[2],
            ChainLink::Assert("*pipe".into())
        );
        let text = "b := srv.(*Builder).\n\t\tAdd(1)";
        assert_eq!(
            go_chain_links(text, 5),
            [
                ChainLink::Operand("srv".into()),
                ChainLink::Assert("*Builder".into()),
                ChainLink::Select("Add".into()),
                ChainLink::Call,
            ]
        );
        assert_eq!(
            go_chain_links("items[i].(fmt.Stringer).String()", 0)[1],
            ChainLink::Index
        );
        assert_eq!(go_closing_bracket("(a, \"(\", ')', `)`)", 0), Some(17));
    }

    #[test]
    fn an_asserted_type_is_a_name_or_a_literal() {
        assert_eq!(asserted_type_name("KVServer"), Some((None, "KVServer")));
        assert_eq!(asserted_type_name("*pipe"), Some((None, "pipe")));
        assert_eq!(
            asserted_type_name("storage.Store"),
            Some((Some("storage"), "Store"))
        );
        assert_eq!(asserted_type_name("*List[int]"), Some((None, "List")));
        assert_eq!(asserted_type_name("interface{ Flush() }"), None);
    }

    #[test]
    fn a_receiver_is_read_back_without_its_arguments() {
        assert_eq!(go_receiver_name("\tc.GetLogger()."), "c.GetLogger");
        assert_eq!(go_receiver_name("child.Flags()."), "child.Flags");
        assert_eq!(go_receiver_name("return (JSON{data})."), "JSON");
        assert_eq!(go_receiver_name("x := (&Render{w: w})."), "Render");
        assert_eq!(go_receiver_name("\tw.Header()."), "w.Header");
        assert_eq!(go_receiver_name("items[i]."), "items");
        assert_eq!(go_receiver_name("f(a, b)."), "f");
    }

    #[test]
    fn a_call_of_a_name_is_found_past_receivers() {
        assert_eq!(call_of("\tc.GetLogger().Error(\"x\")", "Error"), Some(15));
        assert_eq!(call_of("\tWalk(v, n)", "Walk"), Some(1));
        assert_eq!(call_of("\tmyWalk(v)", "Walk"), None);
        assert_eq!(call_of("\tWalk := 1", "Walk"), None);
    }
}
