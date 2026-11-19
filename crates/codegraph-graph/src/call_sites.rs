//! Call sites — what a site calls and passes, what fires it, the loops it is
//! written inside, and what a definition returns, has written on it and
//! declares its members to be. A port of the rest of upstream
//! `src/graph/branch-guards.ts` (`e8ddc4e2`): the call as written and its
//! arguments (`callSiteInTree` and its helpers, 328-750), triggers (752-905),
//! loops (953-1063), returns (#2103's `returnsForFile`, 2070-2168),
//! decorators (2170-2280) and member types (2283-2398). The guard half is
//! [`crate::branch_guards`]; the viewer's Steps read both.
//!
//! Like the guards, nothing here is stored in the index: every reading comes
//! from the source as it is now, through the tree `branch_guards` keeps per
//! file version, so a file's guards and its sites cost one parse. The
//! languages are the guards'
//! ([`supports_branch_guards`](crate::branch_guards::supports_branch_guards));
//! a trigger is read
//! in the JS family only, as upstream reads it. A position the reader does
//! not recognise yields nothing, never a guess.
//!
//! Positions are tree-sitter's: a 1-based line and a 0-based BYTE column.
//! Texts are cut in UTF-16 code units, as upstream counts them.
//!
//! Upstream's rules read fwcd's tree-sitter-kotlin; this port's grammar is
//! tree-sitter-kotlin-ng, which differs where the readers look, and they
//! read both shapes for the same output: a call holds its `value_arguments`
//! and a trailing `annotated_lambda` itself (fwcd wraps them in a
//! `call_suffix`), a name is an `identifier` (fwcd: `simple_identifier`),
//! `val`/`var` is a bare token (fwcd: a `binding_pattern_kind`) and `return`
//! is a `return_expression` (fwcd: a `jump_expression`). Comments are
//! skipped by every comment kind the grammars name (`comment`,
//! `line_comment`, `block_comment`).
//!
//! The regular expressions upstream writes are kept where the `regex` crate
//! reads them the same, with ASCII classes where JavaScript's are ASCII. The
//! status set by the statement before a reply (`res.status(202);
//! res.json(…)`) splices the reply's receiver into its pattern with
//! [`regex::escape`], as upstream escapes it (#2769).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};

use codegraph_core::types::Language;
use regex::Regex;
use tree_sitter::{Node, Point};

use crate::branch_guards::{
    BLOCKISH, branch_key, children, collapse, cut, first_non_blank_column, innermost_at, is_field,
    last_named, named_children, preceding_siblings, rules_for, text_of, with_tree,
};

// =============================================================================
// What the readers return
// =============================================================================

/// One call site, both halves: what is called, as written, and what it is
/// passed (upstream `CallSiteText`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    /// The callee as written, normalised: `prisma.article.findFirst`,
    /// `this.owners.findById(id).orElseThrow`, `res.status(404).json` —
    /// member chains whole, nested argument lists emptied unless they hold
    /// one short literal or name, `await`/`new` dropped, `?.` as `.`.
    pub callee: String,
    /// The argument list, abbreviated: literals and names whole, an object
    /// as its keys, an array as `[…]`, a function as `() => …`, a nested
    /// call as `f(…)`; cut at 96 UTF-16 code units.
    pub args: String,
    /// The same arguments one by one, uncut.
    pub arg_list: Vec<String>,
    /// A status code written as an object property in the arguments
    /// (`{ status: 201 }`), or set by the statement just before a reply
    /// (`res.status(202); res.json(user)`).
    pub status: Option<u16>,
    /// Where the call starts and ends; `None` for a Swift call whose only
    /// argument is a trailing closure, as upstream leaves it.
    pub span: Option<SiteSpan>,
    /// The call this one is written inside the arguments of, normalised
    /// like `callee` — `res.json` for the `generateToken(…)` in
    /// `res.json({ token: generateToken(…) })`. Never reaches out of the
    /// function or block the call is in.
    pub within: Option<String>,
}

/// A position: a 1-based line and a 0-based byte column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SitePoint {
    pub line: usize,
    pub column: usize,
}

/// Where a call starts and ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteSpan {
    pub start: SitePoint,
    pub end: SitePoint,
}

/// How a site is bound to an event, read at the site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Under a JSX attribute: an event prop, or any prop given a function.
    Prop,
    /// Under an `on*` option of a call's object argument.
    Option,
    /// Inside a function passed to a call that runs it later.
    Callback,
}

impl TriggerKind {
    /// Upstream's wire word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prop => "prop",
            Self::Option => "option",
            Self::Callback => "callback",
        }
    }
}

/// What fires a call site (upstream `SiteTrigger`, the kinds read from the
/// tree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteTrigger {
    pub kind: TriggerKind,
    /// `onPress`, `onSubmit`, `useEffect`, `addListener`.
    pub name: String,
    /// The element for a prop (`Button`), the call for an option
    /// (`useFormik`), the first string argument for a callback; `None` when
    /// unknown.
    pub of: Option<String>,
}

/// How a loop repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopKind {
    /// A `for` / `foreach` / `for … in`.
    Each,
    /// A `while` / `do` / `repeat`.
    While,
}

impl LoopKind {
    /// Upstream's wire word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Each => "each",
            Self::While => "while",
        }
    }
}

/// A loop a site is written inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteLoop {
    /// The header without its keyword: `item of items`, `queue.length > 0`.
    pub text: String,
    pub kind: LoopKind,
    /// Where the loop starts, `line:column` — the identity a guard's
    /// `branch` carries.
    pub branch: String,
}

/// A `return` a definition makes itself — never one inside a lambda, a
/// local function or a nested class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnSite {
    pub line: usize,
    pub column: usize,
    /// What it returns, as written, whitespace collapsed.
    pub expression: String,
    /// The string a same-class constant holds, when `expression` is the
    /// constant's bare name (Spring MVC returns `VIEWS_OWNER_FORM`, and the
    /// view is its value).
    pub value: Option<String>,
}

/// The decorators / annotations / attributes on a definition and on the
/// class that holds it, as written without the `@` or the brackets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionDecorators {
    pub own: Vec<String>,
    /// The enclosing class's, when the definition is a member.
    pub class: Vec<String>,
}

// =============================================================================
// Entry points
// =============================================================================

/// The call at a site: `column` `None` means the first non-blank column.
/// `callee`, when known (the index's last segment, `json` for
/// `res.status(201).json(…)`), picks the call in a chain that names it: a
/// position at a chain's start sits on its innermost call.
pub fn call_site_at(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
    callee: Option<&str>,
) -> Option<CallSite> {
    with_tree(key, source, language, |root, source| {
        call_site_in_tree(root, source, line, column, callee)
    })
    .flatten()
}

/// What a site passes, abbreviated (upstream `callArgumentsInTree`): empty
/// for an empty argument list, `None` outside a call.
pub fn call_arguments_at(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Option<String> {
    call_site_at(key, source, language, line, column, None).map(|site| site.args)
}

/// What fires a site, when something does: the JSX prop it sits under, the
/// `on*` option it is written in, or the runs-later call it is an argument
/// of. The JS family only.
pub fn trigger_at(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Option<SiteTrigger> {
    if !matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
    ) {
        return None;
    }
    with_tree(key, source, language, |root, source| {
        trigger_in_tree(root, source, line, column)
    })
    .flatten()
}

/// The loops a site is written inside, outermost first.
pub fn loops_at(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Vec<SiteLoop> {
    with_tree(key, source, language, |root, source| {
        loops_in_tree(root, source, language, line, column)
    })
    .unwrap_or_default()
}

/// The returns of the definition at `line`, in source order.
pub fn returns_of(key: &str, source: &str, language: Language, line: usize) -> Vec<ReturnSite> {
    with_tree(key, source, language, |root, source| {
        returns_in_tree(root, source, line)
    })
    .unwrap_or_default()
}

/// The decorators on the definition at `line` and on its class.
pub fn decorators_of(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
) -> Option<DefinitionDecorators> {
    with_tree(key, source, language, |root, source| {
        decorators_in_tree(root, source, line)
    })
    .flatten()
}

/// The declared types of the members of the class around `line`, by member
/// name, the type as written (`Repository<Cat>` whole): constructor
/// parameter properties, fields and typed properties. The first declaration
/// of a name wins.
pub fn member_types_of(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
) -> BTreeMap<String, String> {
    with_tree(key, source, language, |root, source| {
        member_types_in_tree(root, source, line)
    })
    .unwrap_or_default()
}

// =============================================================================
// The call as written, and what it is passed
// =============================================================================

/// Longest argument list kept before it is cut with an ellipsis.
const MAX_ARGS_TEXT: usize = 96;
/// Longest single argument (a string literal, a name) kept whole.
const MAX_ARG_TEXT: usize = 40;
/// Object keys listed before `…` stands for the rest.
const MAX_OBJECT_KEYS: usize = 4;
/// Longest callee text kept before it is cut.
const MAX_CALLEE_TEXT: usize = 96;
/// A closure, as an argument.
const CLOSURE: &str = "{ … }";

/// Call nodes, across the grammars with rules: JS, Swift, Python, Java,
/// Kotlin, C#, Go, C.
const CALL_TYPES: &[&str] = &[
    "call_expression",
    "new_expression",
    "call",
    "method_invocation",
    "object_creation_expression",
    "invocation_expression",
    "constructor_invocation",
];
const ARGUMENT_CONTAINERS: &[&str] = &["arguments", "value_arguments", "argument_list"];
const STRING_TYPES: &[&str] = &[
    "string",
    "template_string",
    "line_string_literal",
    "multi_line_string_literal",
    "raw_string_literal",
    "string_literal",
    "interpreted_string_literal",
    "concatenated_string",
    "verbatim_string_literal",
    "interpolated_string_expression",
    "char_literal",
];
const OBJECT_TYPES: &[&str] = &[
    "object",
    "object_expression",
    "dictionary",
    "anonymous_object_creation_expression",
];
const ARRAY_TYPES: &[&str] = &[
    "array",
    "array_literal",
    "dictionary_literal",
    "list",
    "tuple",
    "set",
    "list_comprehension",
    "array_creation_expression",
    "array_initializer",
    "initializer_list",
    "collection_expression",
    "collection_literal",
];
const FUNCTION_TYPES: &[&str] = &[
    "arrow_function",
    "function_expression",
    "function",
    "lambda",
    "lambda_expression",
    "func_literal",
    "anonymous_function",
    "anonymous_method_expression",
];
/// Words in a node type the climb to an enclosing call must not cross: the
/// call is then a statement of its own inside a callback (upstream's
/// `CALL_BOUNDARY` pattern, an unanchored alternation).
const CALL_BOUNDARY_WORDS: &[&str] = &[
    "function",
    "lambda",
    "closure",
    "block",
    "statement",
    "body",
    "declaration",
];
/// The containers a reply's statement is one of, for the status before it.
const STATEMENT_BLOCKS: &[&str] = &[
    "statement_block",
    "program",
    "block",
    "class_body",
    "module",
];
/// Statements looked back through for a status the reply's own chain does
/// not carry.
const STATUS_LOOKBACK: usize = 6;

fn is_comment(node: Node<'_>) -> bool {
    node.kind().ends_with("comment")
}

fn point(position: Point) -> SitePoint {
    SitePoint {
        line: position.row + 1,
        column: position.column,
    }
}

/// Where a call's arguments are: the node holding them, and the arguments.
struct Arguments<'t> {
    node: Node<'t>,
    items: Vec<Node<'t>>,
}

impl<'t> Arguments<'t> {
    fn of(node: Node<'t>) -> Self {
        let items = named_children(node)
            .into_iter()
            .filter(|child| !is_comment(*child))
            .collect();
        Self { node, items }
    }
}

/// The node holding a call's arguments: the `arguments` field, a container
/// child, or the contents of Swift's `call_suffix`; for kotlin-ng, a call
/// with nothing but a trailing lambda, whose `annotated_lambda` stands for
/// fwcd's `call_suffix` around it.
fn arguments_of(call: Node<'_>) -> Option<Arguments<'_>> {
    if let Some(field) = call.child_by_field_name("arguments") {
        return Some(Arguments::of(field));
    }
    for child in named_children(call) {
        if ARGUMENT_CONTAINERS.contains(&child.kind()) {
            return Some(Arguments::of(child));
        }
        if child.kind() == "call_suffix" {
            for inner in named_children(child) {
                if ARGUMENT_CONTAINERS.contains(&inner.kind()) || inner.kind() == "lambda_literal" {
                    return Some(Arguments::of(inner));
                }
            }
            return Some(Arguments::of(child));
        }
    }
    named_children(call)
        .into_iter()
        .find(|child| child.kind() == "annotated_lambda")
        .map(|lambda| Arguments {
            node: lambda,
            items: vec![lambda],
        })
}

fn call_site_in_tree(
    root: Node<'_>,
    source: &str,
    line: usize,
    column: Option<usize>,
    want: Option<&str>,
) -> Option<CallSite> {
    let call = call_at(root, source, line, column, want)?;
    let arguments = arguments_of(call)?;
    let callee = callee_chain_text(call, arguments.node, source);
    if arguments.node.kind() == "lambda_literal" {
        return Some(CallSite {
            callee,
            args: CLOSURE.to_string(),
            arg_list: vec![CLOSURE.to_string()],
            status: None,
            span: None,
            within: None,
        });
    }
    let mut parts: Vec<String> = Vec::new();
    let mut status = None;
    for item in &arguments.items {
        parts.push(abbreviate_argument(*item, source));
        if status.is_none() {
            status = status_property_in(*item, source, 0);
        }
    }
    if status.is_none() {
        status = status_set_before(call, &callee, source);
    }
    Some(CallSite {
        callee,
        args: cut(&parts.join(", "), MAX_ARGS_TEXT),
        arg_list: parts,
        status,
        span: Some(SiteSpan {
            start: point(call.start_position()),
            end: point(call.end_position()),
        }),
        within: enclosing_call_text(call, source),
    })
}

/// The call node a site belongs to: climb from the callee to the call. The
/// recorded column may sit a character off the callee (a 1-based column,
/// the space before it), so a near miss is tried before giving up.
fn call_at<'t>(
    root: Node<'t>,
    source: &str,
    line: usize,
    column: Option<usize>,
    callee: Option<&str>,
) -> Option<Node<'t>> {
    if line == 0 {
        return None;
    }
    let row = line - 1;
    let first_non_blank = first_non_blank_column(source, row);
    let columns = match column {
        None => vec![first_non_blank],
        Some(column) => vec![
            column,
            column + 1,
            column.saturating_sub(1),
            first_non_blank,
        ],
    };
    let want = callee.map(last_piece);
    for col in columns {
        let Some(start) = innermost_at(root, row, col) else {
            continue;
        };
        let mut node = Some(start);
        let mut first = None;
        for _ in 0..10 {
            let Some(here) = node else {
                break;
            };
            if CALL_TYPES.contains(&here.kind()) {
                first = first.or(Some(here));
                let Some(want) = want else {
                    return Some(here);
                };
                // A chain's position is its start: `res.status(201).json(…)`
                // at `res` meets `res.status(…)` first; the call that is THIS
                // one names `json`.
                let text = arguments_of(here)
                    .map(|arguments| callee_chain_text(here, arguments.node, source))
                    .unwrap_or_default();
                if last_piece(&argument_lists().replace_all(&text, "")) == want {
                    return Some(here);
                }
            }
            node = here.parent();
        }
        if first.is_some() {
            return first;
        }
    }
    None
}

/// The last `.`- or `:`-separated piece of a name.
fn last_piece(text: &str) -> &str {
    text.rsplit(['.', ':']).next().unwrap_or(text)
}

/// `\([^()]*\)`: an argument list with no list inside it.
fn argument_lists() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\([^()]*\)").expect("valid regex"))
}

/// `\(([^()]*)\)`, the same with its contents captured.
fn argument_list_contents() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(([^()]*)\)").expect("valid regex"))
}

/// `<[^<>]*>`: a type-argument list with none inside it.
fn type_arguments() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^<>]*>").expect("valid regex"))
}

/// `\([^()]*\)$`: a trailing argument list.
fn trailing_argument_list() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\([^()]*\)$").expect("valid regex"))
}

/// `/^\s*[\w.]{1,28}\s*$/`, JavaScript's ASCII `\w`.
fn is_short_name(inner: &str) -> bool {
    let trimmed = inner.trim();
    let count = trimmed.chars().count();
    (1..=28).contains(&count)
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
}

/// `text` without a leading `word` and the whitespace after it, when one of
/// `words` is followed by whitespace there.
fn strip_leading_word<'s>(text: &'s str, words: &[&str]) -> &'s str {
    for word in words {
        if let Some(rest) = text.strip_prefix(word)
            && rest.starts_with(char::is_whitespace)
        {
            return rest.trim_start();
        }
    }
    text
}

/// The text of a call before its arguments, normalised to a member chain.
fn callee_chain_text(call: Node<'_>, container: Node<'_>, source: &str) -> String {
    // Kotlin and Swift wrap the arguments in a `call_suffix`; the callee is
    // everything before that suffix.
    let mut end = container.start_byte();
    if let Some(suffix) = container.parent()
        && suffix.kind() == "call_suffix"
    {
        end = suffix.start_byte();
    }
    let start = call.start_byte();
    let before = source.get(start..end.max(start)).unwrap_or("");
    let collapsed = collapse(before);
    let mut text = strip_leading_word(&collapsed, &["await", "new", "yield", "return"]);
    text = strip_leading_word(text, &["await", "new"]);
    let mut text = text.to_string();
    // Empty every nested argument list, innermost first: `a(b(c)).d` →
    // `a().d` — keeping one short literal or name (`res.status(404).json`,
    // `ResponseEntity.status(HttpStatus.NOT_FOUND).body`), which is the fact
    // a reader of the chain wants.
    for _ in 0..6 {
        if !argument_lists().is_match(&text) {
            break;
        }
        text = argument_list_contents()
            .replace_all(&text, |captures: &regex::Captures<'_>| {
                let inner = &captures[1];
                if is_short_name(inner) {
                    format!("({})", inner.trim())
                } else {
                    "()".to_string()
                }
            })
            .into_owned();
    }
    let text = text.replace("?.", ".").replace("!.", ".");
    let text: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    let text = type_arguments().replace_all(&text, "");
    let text = trailing_argument_list().replace(&text, "");
    cut(&text, MAX_CALLEE_TEXT)
}

/// The nearest call whose ARGUMENTS contain `call`, as its callee chain;
/// `None` when there is none this side of a function or block.
fn enclosing_call_text(call: Node<'_>, source: &str) -> Option<String> {
    let mut node = call.parent();
    for _ in 0..12 {
        let here = node?;
        let kind = here.kind();
        if CALL_BOUNDARY_WORDS.iter().any(|word| kind.contains(word)) {
            return None;
        }
        if CALL_TYPES.contains(&kind) {
            let holder = arguments_of(here)?.node;
            if holder.start_byte() <= call.start_byte() && call.end_byte() <= holder.end_byte() {
                return Some(callee_chain_text(here, holder, source));
            }
            return None;
        }
        node = here.parent();
    }
    None
}

/// `/^(?:status|statusCode|status_code|code)$/`.
fn is_status_key(key: &str) -> bool {
    matches!(key, "status" | "statusCode" | "status_code" | "code")
}

/// `/^[1-5]\d{2}$/`.
fn status_code(text: &str) -> Option<u16> {
    let bytes = text.as_bytes();
    (bytes.len() == 3
        && (b'1'..=b'5').contains(&bytes[0])
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit())
    .then(|| text.parse().ok())
    .flatten()
}

/// `{ status: 201 }` inside an argument — the literal an abbreviated object
/// hides.
fn status_property_in(node: Node<'_>, source: &str, depth: usize) -> Option<u16> {
    if depth > 2 {
        return None;
    }
    for child in named_children(node) {
        if matches!(
            child.kind(),
            "pair"
                | "keyword_argument"
                | "named_argument"
                | "property_assignment"
                | "object_property"
        ) {
            let key = child
                .child_by_field_name("key")
                .or_else(|| child.child_by_field_name("name"))
                .or_else(|| child.named_child(0));
            let value = child
                .child_by_field_name("value")
                .or_else(|| last_named(child));
            if let (Some(key), Some(value)) = (key, value) {
                let key: String = text_of(key, source)
                    .chars()
                    .filter(|ch| *ch != '\'' && *ch != '"')
                    .collect();
                if is_status_key(&key)
                    && let Some(code) = status_code(text_of(value, source))
                {
                    return Some(code);
                }
            }
        }
        if let Some(inner) = status_property_in(child, source, depth + 1) {
            return Some(inner);
        }
    }
    None
}

/// `/^(?:res|response|reply|rep|ctx|c|context)\.(?:json|jsonp|send|render|sendFile|download|end|text|html|body)$/`.
fn is_body_reply(bare: &str) -> bool {
    bare.split_once('.').is_some_and(|(receiver, method)| {
        matches!(
            receiver,
            "res" | "response" | "reply" | "rep" | "ctx" | "c" | "context"
        ) && matches!(
            method,
            "json"
                | "jsonp"
                | "send"
                | "render"
                | "sendFile"
                | "download"
                | "end"
                | "text"
                | "html"
                | "body"
        )
    })
}

/// `/\b(?:status|code|sendStatus|writeHead)\(/`: the chain sets its own
/// status.
fn sets_its_own_status(callee: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?-u:\b)(?:status|code|sendStatus|writeHead)\(").expect("valid regex")
    })
    .is_match(callee)
}

/// The statement that sets `receiver`'s status by itself, one pattern per
/// receiver; [`is_body_reply`] bounds the receivers to seven names.
fn status_statement(receiver: &str) -> Regex {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .entry(receiver.to_string())
        .or_insert_with(|| {
            let receiver = regex::escape(receiver);
            Regex::new(&format!(
                r"^\s*(?:await\s+)?{receiver}\s*\.\s*(?:status|code)\s*\(\s*([1-5][0-9]{{2}})\s*\)\s*;?\s*$|^\s*{receiver}\s*\.\s*statusCode\s*=\s*([1-5][0-9]{{2}})\s*;?\s*$"
            ))
            .expect("an escaped receiver makes a valid regex")
        })
        .clone()
}

/// `res.status(202); res.json(user)` — the status set by an earlier
/// statement in the same block, when the reply's own chain sets none. Only a
/// statement that IS the status call counts (`res.status(404)` inside an
/// `if` before it is another path, not this reply's); the first one found
/// walking back wins.
fn status_set_before(call: Node<'_>, callee: &str, source: &str) -> Option<u16> {
    let bare = argument_lists().replace_all(callee, "");
    if !is_body_reply(&bare) || sets_its_own_status(callee) {
        return None;
    }
    let receiver = bare.split('.').next().unwrap_or_default();
    let mut statement = call;
    while let Some(parent) = statement.parent() {
        if STATEMENT_BLOCKS.contains(&parent.kind()) {
            break;
        }
        statement = parent;
    }
    let pattern = status_statement(receiver);
    let mut previous = statement.prev_named_sibling();
    for _ in 0..STATUS_LOOKBACK {
        let Some(earlier) = previous else {
            break;
        };
        if !is_comment(earlier)
            && let Some(captures) = pattern.captures(text_of(earlier, source))
        {
            let code = captures.get(1).or_else(|| captures.get(2))?;
            return code.as_str().parse().ok();
        }
        previous = earlier.prev_named_sibling();
    }
    None
}

/// One argument, abbreviated to what a reader scans for.
fn abbreviate_argument(node: Node<'_>, source: &str) -> String {
    let kind = node.kind();
    let whole = || cut(&collapse(text_of(node, source)), MAX_ARG_TEXT);
    // Python `name=value` — the name is half the meaning.
    if kind == "keyword_argument" {
        let name = node
            .child_by_field_name("name")
            .map_or("", |name| text_of(name, source));
        let value = node
            .child_by_field_name("value")
            .map(|value| abbreviate_argument(value, source))
            .unwrap_or_default();
        return format!("{name}={value}");
    }
    // C#: an `argument` wraps the expression, optionally with a name.
    if kind == "argument" {
        let name = node.child_by_field_name("name");
        let inner = last_named(node);
        let value = inner.map_or_else(whole, |inner| abbreviate_argument(inner, source));
        return match (name, inner) {
            (Some(name), Some(inner)) if name.id() != inner.id() => {
                format!("{}: {value}", text_of(name, source))
            }
            _ => value,
        };
    }
    // Go `gin.H{"error": err}` / `User{Name: n}`: the type, then the braces.
    if kind == "composite_literal" {
        let type_name = node
            .child_by_field_name("type")
            .map(|type_node| cut(&collapse(text_of(type_node, source)), 24))
            .unwrap_or_default();
        return format!("{type_name}{{…}}");
    }
    if STRING_TYPES.contains(&kind) {
        return whole();
    }
    if OBJECT_TYPES.contains(&kind) {
        return object_keys(node, source);
    }
    if ARRAY_TYPES.contains(&kind) {
        return "[…]".to_string();
    }
    if FUNCTION_TYPES.contains(&kind) {
        return "() => …".to_string();
    }
    if kind == "lambda_literal" {
        return CLOSURE.to_string();
    }
    if kind == "spread_element" {
        return whole();
    }
    if kind == "await_expression" {
        return match node.named_child(0) {
            Some(inner) => format!("await {}", abbreviate_argument(inner, source)),
            None => "await …".to_string(),
        };
    }
    if CALL_TYPES.contains(&kind) {
        let callee = node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("constructor"))
            .or_else(|| node.named_child(0));
        let name = callee
            .map(|callee| cut(&collapse(text_of(callee, source)), 28))
            .unwrap_or_default();
        let new = if kind == "new_expression" { "new " } else { "" };
        return format!("{new}{name}(…)");
    }
    // Swift `label: value` — the label is half the meaning (`withName:`);
    // kotlin-ng's `name = value` reads the same way.
    if kind == "value_argument" {
        let named = named_children(node);
        if named.len() >= 2
            && matches!(
                named[0].kind(),
                "simple_identifier" | "value_argument_label" | "identifier"
            )
            && let Some(last) = named.last()
        {
            return format!(
                "{}: {}",
                text_of(named[0], source),
                abbreviate_argument(*last, source)
            );
        }
        return named
            .last()
            .map_or_else(whole, |last| abbreviate_argument(*last, source));
    }
    if matches!(
        kind,
        "lambda_argument" | "trailing_closure" | "annotated_lambda"
    ) {
        return CLOSURE.to_string();
    }
    whole()
}

/// `{ email, password, … }` — the keys an object literal passes, not its
/// bulk.
fn object_keys(node: Node<'_>, source: &str) -> String {
    let mut keys: Vec<String> = Vec::new();
    let mut more = 0usize;
    for child in named_children(node) {
        if is_comment(child) {
            continue;
        }
        let key = match child.kind() {
            "pair" => child
                .child_by_field_name("key")
                .map(|key| text_of(key, source).to_string()),
            "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => {
                Some(text_of(child, source).to_string())
            }
            "spread_element" => Some(collapse(text_of(child, source))),
            "method_definition" => child
                .child_by_field_name("name")
                .map(|name| text_of(name, source).to_string()),
            _ => None,
        };
        let Some(key) = key else {
            continue;
        };
        if keys.len() >= MAX_OBJECT_KEYS {
            more += 1;
            continue;
        }
        keys.push(cut(&key, 24));
    }
    if keys.is_empty() {
        return "{…}".to_string();
    }
    let rest = if more > 0 { ", …" } else { "" };
    format!("{{ {}{rest} }}", keys.join(", "))
}

// =============================================================================
// Triggers — what fires a site
// =============================================================================

/// Callees whose function argument runs LATER — a callback, not a call.
/// Matched on the last segment.
const LATER_CALLEES: &[&str] = &[
    "useEffect",
    "useLayoutEffect",
    "useFocusEffect",
    "useImperativeHandle",
    "setTimeout",
    "setInterval",
    "requestAnimationFrame",
    "requestIdleCallback",
    "runAfterInteractions",
    "addListener",
    "addEventListener",
    "on",
    "once",
    "subscribe",
    "then",
    "catch",
    "finally",
    "runOnJS",
    "runOnUI",
    "scheduleOnRN",
];
/// Hooks that bind a function to a name: whoever uses the name fires what
/// is inside.
const NAMING_HOOKS: &[&str] = &["useCallback", "useMemo", "useEffectEvent", "useEvent"];
/// The climb never leaves the function the site belongs to — unless that
/// function is inline.
const TRIGGER_BOUNDARIES: &[&str] = &[
    "function_declaration",
    "method_definition",
    "class_declaration",
    "class_body",
    "program",
];
const MAX_TRIGGER_CLIMB: usize = 24;

/// `/^on[A-Z]/`.
fn is_event_name(name: &str) -> bool {
    name.strip_prefix("on")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

/// `/^on[A-Z]\w*$/`.
fn is_event_key(key: &str) -> bool {
    is_event_name(key)
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// A key without one quote character at each end (`/^['"`]|['"`]$/g`).
fn unquoted(text: &str) -> &str {
    let quote = |ch: char| matches!(ch, '\'' | '"' | '`');
    let text = text.strip_prefix(quote).unwrap_or(text);
    text.strip_suffix(quote).unwrap_or(text)
}

/// A call's callee as written: `nativeEmitter.addListener`, `Alert.alert`.
fn callee_text(call: Node<'_>, source: &str) -> Option<String> {
    call.child_by_field_name("function")
        .or_else(|| call.child_by_field_name("constructor"))
        .map(|callee| cut(&collapse(text_of(callee, source)), 40))
}

/// The last identifier of a callee (`/([A-Za-z_$][\w$]*)\s*$/`), or the
/// text itself when it ends in none.
fn last_segment(text: Option<String>) -> Option<String> {
    let text = text?;
    let trimmed = text.trim_end();
    let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$';
    let run_start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, ch)| word(*ch))
        .last()
        .map(|(index, _)| index);
    let found = run_start.and_then(|start| {
        trimmed[start..]
            .char_indices()
            .find(|(_, ch)| ch.is_ascii_alphabetic() || *ch == '_' || *ch == '$')
            .map(|(offset, _)| trimmed[start + offset..].to_string())
    });
    Some(found.unwrap_or(text))
}

fn trigger_in_tree(
    root: Node<'_>,
    source: &str,
    line: usize,
    column: Option<usize>,
) -> Option<SiteTrigger> {
    if line == 0 {
        return None;
    }
    let row = line - 1;
    let col = column.unwrap_or_else(|| first_non_blank_column(source, row));
    let mut node = innermost_at(root, row, col);
    let mut previous: Option<Node<'_>> = None;
    // Whether the climb crossed an inline function: `onPress={() => go()}`
    // fires later, `behavior={isAndroid() ? 'a' : 'b'}` runs at render.
    let mut deferred = false;
    for _ in 0..MAX_TRIGGER_CLIMB {
        let Some(here) = node else {
            break;
        };
        let kind = here.kind();
        if TRIGGER_BOUNDARIES.contains(&kind) {
            return None;
        }
        // A named handler is its own story: `const handleX = useCallback(() =>
        // …)` binds a name, and whoever uses the name triggers what is inside.
        if kind == "arrow_function" || kind == "function_expression" {
            let parent = here.parent();
            if parent.is_some_and(|parent| parent.kind() == "variable_declarator") {
                return None;
            }
            if let Some(parent) = parent
                && parent.kind() == "arguments"
                && let Some(call) = parent.parent()
                && last_segment(callee_text(call, source))
                    .is_some_and(|callee| NAMING_HOOKS.contains(&callee.as_str()))
            {
                return None;
            }
            deferred = true;
        }
        if kind == "jsx_attribute" {
            let prop = here.named_child(0).map_or_else(
                || "prop".to_string(),
                |name| text_of(name, source).to_string(),
            );
            // An event prop, or any prop given a function: fired later. A
            // value computed in the attribute is not.
            if !deferred && !is_event_name(&prop) {
                return None;
            }
            let tag = here
                .parent()
                .and_then(|element| element.child_by_field_name("name"));
            return Some(SiteTrigger {
                kind: TriggerKind::Prop,
                name: prop,
                of: tag.map(|tag| collapse(text_of(tag, source))),
            });
        }
        if kind == "pair" {
            let key = here
                .child_by_field_name("key")
                .map_or("", |key| unquoted(text_of(key, source)));
            if is_event_key(key) {
                // `useFormik({ onSubmit: … })`, `Alert.alert(t, m, [{ onPress:
                // … }])`: the object — possibly inside an array — is an
                // argument of a call.
                let mut holder = here.parent();
                for _ in 0..4 {
                    match holder {
                        Some(object) if matches!(object.kind(), "object" | "array" | "pair") => {
                            holder = object.parent();
                        }
                        _ => break,
                    }
                }
                let call = holder
                    .filter(|holder| holder.kind() == "arguments")
                    .and_then(|holder| holder.parent());
                return Some(SiteTrigger {
                    kind: TriggerKind::Option,
                    name: key.to_string(),
                    of: call
                        .filter(|call| CALL_TYPES.contains(&call.kind()))
                        .and_then(|call| callee_text(call, source)),
                });
            }
        }
        if kind == "arguments"
            && previous.is_some()
            && let Some(call) = here.parent()
            && CALL_TYPES.contains(&call.kind())
            && let Some(callee) = last_segment(callee_text(call, source))
            && LATER_CALLEES.contains(&callee.as_str())
        {
            let of = here
                .named_child(0)
                .filter(|first| STRING_TYPES.contains(&first.kind()))
                .map(|first| cut(&collapse(text_of(first, source)), MAX_ARG_TEXT));
            return Some(SiteTrigger {
                kind: TriggerKind::Callback,
                name: callee,
                of,
            });
        }
        previous = Some(here);
        node = here.parent();
    }
    None
}

// =============================================================================
// Loops — what a site repeats under
// =============================================================================

/// Loop node types across the grammars with rules. A type absent yields
/// nothing, never a wrong label.
const LOOP_TYPES: &[(&str, LoopKind)] = &[
    ("for_statement", LoopKind::Each),
    ("for_in_statement", LoopKind::Each),
    ("for_of_statement", LoopKind::Each),
    ("for_each_statement", LoopKind::Each),
    ("enhanced_for_statement", LoopKind::Each),
    ("foreach_statement", LoopKind::Each),
    ("for_range_loop", LoopKind::Each),
    ("for_expression", LoopKind::Each),
    ("while_statement", LoopKind::While),
    ("while_expression", LoopKind::While),
    ("do_statement", LoopKind::While),
    ("do_while_statement", LoopKind::While),
    ("repeat_while_statement", LoopKind::While),
];

fn loop_kind(kind: &str) -> Option<LoopKind> {
    LOOP_TYPES
        .iter()
        .find(|(name, _)| *name == kind)
        .map(|(_, loop_kind)| *loop_kind)
}

/// The loops a site is written inside, outermost first — what tells a
/// reading in the code's order that a run of calls happens once PER ITEM.
/// The climb is the guards' climb (the same boundaries, the same transparent
/// inline functions), so a callback's body is read in its own function and
/// a `.forEach` body under the loop it is written in.
fn loops_in_tree(
    root: Node<'_>,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Vec<SiteLoop> {
    let Some(rules) = rules_for(language) else {
        return Vec::new();
    };
    if line == 0 {
        return Vec::new();
    }
    let row = line - 1;
    let col = column.unwrap_or_else(|| first_non_blank_column(source, row));
    let Some(mut node) = innermost_at(root, row, col) else {
        return Vec::new();
    };
    let mut found: Vec<SiteLoop> = Vec::new();
    while let Some(parent) = node.parent() {
        if rules.boundaries.contains(&parent.kind()) {
            break;
        }
        if rules.inline_functions.contains(&parent.kind()) {
            let holder = parent.parent().map_or("", |holder| holder.kind());
            if rules.binding_parents.contains(&holder) {
                break;
            }
            node = parent;
            continue;
        }
        // The header, not the body: a site is in the loop only when it is
        // under it.
        if let Some(kind) = loop_kind(parent.kind())
            && !is_field(parent, "condition", node)
            && !is_field(parent, "value", node)
        {
            let text = loop_header(parent, source);
            if !text.is_empty() {
                found.push(SiteLoop {
                    text,
                    kind,
                    branch: branch_key(parent),
                });
            }
        }
        node = parent;
    }
    found.reverse();
    found
}

/// `text` without a leading loop keyword (`/^(?:for|foreach|while|do|repeat)\b\s*/i`).
fn strip_loop_keyword(text: &str) -> &str {
    let word_end = text
        .char_indices()
        .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
        .map_or(text.len(), |(index, _)| index);
    let word = text[..word_end].to_ascii_lowercase();
    if matches!(word.as_str(), "for" | "foreach" | "while" | "do" | "repeat") {
        text[word_end..].trim_start()
    } else {
        text
    }
}

/// A loop's header as written, keyword and braces dropped: `item of items`,
/// `queue.length > 0`.
fn loop_header(node: Node<'_>, source: &str) -> String {
    let body = node.child_by_field_name("body").or_else(|| {
        named_children(node)
            .into_iter()
            .find(|child| BLOCKISH.contains(&child.kind()))
    });
    let raw = match body {
        Some(body) if body.start_byte() > node.start_byte() => source
            .get(node.start_byte()..body.start_byte())
            .unwrap_or(""),
        _ => text_of(node, source),
    };
    let collapsed = collapse(raw);
    let mut text = strip_loop_keyword(&collapsed);
    // `/[{:]\s*$/`, then trimmed.
    text = text.trim_end();
    text = text.strip_suffix(['{', ':']).unwrap_or(text).trim();
    // `/^\((.*)\)$/s`: the parentheses around the whole header.
    if text.len() >= 2 && text.starts_with('(') && text.ends_with(')') {
        text = text[1..text.len() - 1].trim();
    }
    // `const item of items` reads as `item of items`; the binding word is
    // noise here.
    for word in ["const", "let", "var", "val", "final"] {
        if let Some(rest) = text.strip_prefix(word)
            && rest.starts_with(char::is_whitespace)
        {
            text = rest.trim_start();
            break;
        }
    }
    cut(text, 60)
}

// =============================================================================
// Definitions — their returns, decorators and member types
// =============================================================================

/// The nodes a line's definition can be.
const DEFINITION_TYPES: &[&str] = &[
    "function_declaration",
    "function_definition",
    "method_definition",
    "method_declaration",
    "constructor_declaration",
    "class_declaration",
    "class_definition",
    "decorated_definition",
    "local_function_statement",
    "lexical_declaration",
    "variable_declaration",
    "public_field_definition",
];
const CLASS_TYPES: &[&str] = &[
    "class_declaration",
    "class_definition",
    "class",
    "object_declaration",
    "struct_declaration",
    "record_declaration",
];
const DECORATOR_TYPES: &[&str] = &["decorator", "annotation", "marker_annotation", "attribute"];
const MAX_DECORATOR_TEXT: usize = 80;
/// Scopes whose `return`s belong to something else.
const NESTED_SCOPE_TYPES: &[&str] = &[
    "lambda_expression",
    "class_body",
    "lambda_literal",
    "anonymous_function",
    "object_literal",
    "arrow_function",
    "function_expression",
    "function_declaration",
    "function_definition",
    "method_declaration",
    "method_definition",
    "lambda",
];
const CLASS_BODY_TYPES: &[&str] = &["class_body", "declaration_list", "field_declaration_list"];

/// The definition the line belongs to: up from its first non-blank column,
/// at most twelve levels.
fn definition_at<'t>(root: Node<'t>, source: &str, line: usize) -> Option<Node<'t>> {
    if line == 0 {
        return None;
    }
    let row = line - 1;
    let mut node = innermost_at(root, row, first_non_blank_column(source, row));
    for _ in 0..12 {
        let here = node?;
        if DEFINITION_TYPES.contains(&here.kind()) {
            return Some(here);
        }
        node = here.parent();
    }
    None
}

/// Up from `start` to a class, at most six levels; past that, the node the
/// climb stopped at, as upstream's loop leaves it.
fn class_above(start: Option<Node<'_>>) -> Option<Node<'_>> {
    let mut class = start;
    for _ in 0..6 {
        match class {
            Some(node) if !CLASS_TYPES.contains(&node.kind()) => class = node.parent(),
            _ => break,
        }
    }
    class
}

/// `/^[A-Za-z_$][\w$]*$/`.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

fn returns_in_tree(root: Node<'_>, source: &str, line: usize) -> Vec<ReturnSite> {
    let Some(definition) = definition_at(root, source, line) else {
        return Vec::new();
    };
    let mut out: Vec<ReturnSite> = Vec::new();
    collect_returns(definition, source, &mut out);
    // A bare name: the string a same-class constant holds (`static final
    // String X = "…"`).
    if let Some(class) = class_above(definition.parent()) {
        let mut strings: HashMap<String, String> = HashMap::new();
        collect_string_constants(class, source, &mut strings);
        for site in &mut out {
            if is_identifier(&site.expression)
                && let Some(value) = strings.get(&site.expression)
            {
                site.value = Some(value.clone());
            }
        }
    }
    out
}

/// `/^return\b/`.
fn starts_with_return(text: &str) -> bool {
    text.strip_prefix("return").is_some_and(|rest| {
        !rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

fn collect_returns(node: Node<'_>, source: &str, out: &mut Vec<ReturnSite>) {
    for child in named_children(node) {
        if NESTED_SCOPE_TYPES.contains(&child.kind()) {
            continue;
        }
        let is_return = match child.kind() {
            "return_statement" | "return_expression" => true,
            "jump_expression" => starts_with_return(text_of(child, source)),
            _ => false,
        };
        if is_return {
            // kotlin-ng holds a `return@label`'s label as a named child.
            let expression = named_children(child).into_iter().find(|expression| {
                child
                    .child_by_field_name("label")
                    .is_none_or(|label| label.id() != expression.id())
            });
            if let Some(expression) = expression {
                let start = child.start_position();
                out.push(ReturnSite {
                    line: start.row + 1,
                    column: start.column,
                    expression: collapse(text_of(expression, source)),
                    value: None,
                });
            }
            continue;
        }
        collect_returns(child, source, out);
    }
}

fn collect_string_constants(node: Node<'_>, source: &str, strings: &mut HashMap<String, String>) {
    for child in named_children(node) {
        match child.kind() {
            "variable_declarator" => {
                let name = child
                    .child_by_field_name("name")
                    .map_or("", |name| text_of(name, source));
                if let Some(value) = child.child_by_field_name("value")
                    && value.kind() == "string_literal"
                    && !name.is_empty()
                {
                    let text = text_of(value, source);
                    let text = text.strip_prefix('"').unwrap_or(text);
                    let text = text.strip_suffix('"').unwrap_or(text);
                    strings.insert(name.to_string(), text.to_string());
                }
            }
            "field_declaration" | "class_body" => collect_string_constants(child, source, strings),
            _ => {}
        }
    }
}

fn decorators_in_tree(root: Node<'_>, source: &str, line: usize) -> Option<DefinitionDecorators> {
    let definition = definition_at(root, source, line)?;
    // A Python decorated function is the child of the node that holds the
    // decorators.
    let holder = match definition.parent() {
        Some(parent) if parent.kind() == "decorated_definition" => parent,
        _ => definition,
    };
    let own = decorators_on(holder, source);
    let class_holder = class_above(holder.parent()).map(|class| match class.parent() {
        Some(parent) if parent.kind() == "decorated_definition" => parent,
        _ => class,
    });
    Some(DefinitionDecorators {
        own,
        class: class_holder.map_or_else(Vec::new, |holder| decorators_on(holder, source)),
    })
}

/// Decorator texts on one definition node: its own decorator children, its
/// modifiers / attribute lists, or — JS — the decorator siblings before it.
fn decorators_on(definition: Node<'_>, source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |node: Node<'_>| {
        if node.kind() == "attribute_list" {
            for attribute in named_children(node) {
                if attribute.kind() == "attribute" {
                    out.push(decorator_text(attribute, source));
                }
            }
            return;
        }
        if DECORATOR_TYPES.contains(&node.kind()) {
            out.push(decorator_text(node, source));
        }
    };
    for child in named_children(definition) {
        if child.kind() == "modifiers" {
            for modifier in named_children(child) {
                add(modifier);
            }
        } else {
            add(child);
        }
    }
    // JS: decorators are siblings that precede the member in the class body.
    if out.is_empty()
        && let Some(parent) = definition.parent()
    {
        for sibling in preceding_siblings(parent, definition).iter().rev() {
            if sibling.kind() != "decorator" {
                break;
            }
            out.insert(0, decorator_text(*sibling, source));
        }
    }
    out
}

fn decorator_text(node: Node<'_>, source: &str) -> String {
    let collapsed = collapse(text_of(node, source));
    let mut text = collapsed
        .strip_prefix('@')
        .map_or(collapsed.as_str(), str::trim_start);
    if node.kind() == "attribute_list" {
        text = text.strip_prefix('[').unwrap_or(text);
        text = text.strip_suffix(']').unwrap_or(text);
    }
    cut(text, MAX_DECORATOR_TEXT)
}

/// A declared type as written, without a leading `:`.
fn type_text(node: Option<Node<'_>>, source: &str) -> String {
    let Some(node) = node else {
        return String::new();
    };
    let collapsed = collapse(text_of(node, source));
    collapsed
        .strip_prefix(':')
        .map_or(collapsed.as_str(), str::trim_start)
        .trim()
        .to_string()
}

/// `/^\s*(?:public|private|protected|readonly)\b/`.
fn starts_with_member_modifier(text: &str) -> bool {
    let text = text.trim_start();
    ["public", "private", "protected", "readonly"]
        .iter()
        .any(|word| {
            text.strip_prefix(word).is_some_and(|rest| {
                !rest
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
        })
}

fn member_types_in_tree(root: Node<'_>, source: &str, line: usize) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    if line == 0 {
        return out;
    }
    let row = line - 1;
    let mut node = innermost_at(root, row, first_non_blank_column(source, row));
    let mut class = None;
    for _ in 0..16 {
        let Some(here) = node else {
            break;
        };
        if CLASS_TYPES.contains(&here.kind()) {
            class = Some(here);
            break;
        }
        node = here.parent();
    }
    let Some(class) = class else {
        return out;
    };
    let mut put = |name: Option<Node<'_>>, type_name: String| {
        let name = name.map_or("", |name| text_of(name, source));
        if !name.is_empty() && !type_name.is_empty() && !out.contains_key(name) {
            out.insert(name.to_string(), type_name);
        }
    };
    // Kotlin's primary constructor sits on the class node itself.
    for child in named_children(class) {
        if child.kind() == "primary_constructor" {
            let parameters = named_children(child)
                .into_iter()
                .find(|node| node.kind() == "class_parameters")
                .unwrap_or(child);
            parameter_types(Some(parameters), source, &mut put);
        }
    }
    let body = named_children(class)
        .into_iter()
        .find(|child| CLASS_BODY_TYPES.contains(&child.kind()))
        .or_else(|| class.child_by_field_name("body"));
    let Some(body) = body else {
        return out;
    };
    for member in named_children(body) {
        match member.kind() {
            "public_field_definition" | "field_definition" => put(
                member.child_by_field_name("name"),
                type_text(member.child_by_field_name("type"), source),
            ),
            "method_definition" => {
                if member
                    .child_by_field_name("name")
                    .is_some_and(|name| text_of(name, source) == "constructor")
                {
                    parameter_types(member.child_by_field_name("parameters"), source, &mut put);
                }
            }
            "field_declaration" => {
                // Java: `type` + `declarator`; C#: a `variable_declaration`
                // inside.
                if let Some(type_node) = member.child_by_field_name("type") {
                    for declarator in named_children(member) {
                        if declarator.kind() == "variable_declarator" {
                            put(
                                declarator.child_by_field_name("name"),
                                type_text(Some(type_node), source),
                            );
                        }
                    }
                } else if let Some(declaration) = named_children(member)
                    .into_iter()
                    .find(|child| child.kind() == "variable_declaration")
                {
                    let type_node = declaration.child_by_field_name("type");
                    for declarator in named_children(declaration) {
                        if declarator.kind() == "variable_declarator" {
                            put(
                                declarator.child_by_field_name("name"),
                                type_text(type_node, source),
                            );
                        }
                    }
                }
            }
            "property_declaration" => {
                // C#: `type` + `name`; Kotlin: `variable_declaration (name)
                // (type)`.
                if let Some(type_node) = member.child_by_field_name("type") {
                    put(
                        member.child_by_field_name("name"),
                        type_text(Some(type_node), source),
                    );
                } else {
                    let parts = named_children(member)
                        .into_iter()
                        .find(|child| child.kind() == "variable_declaration")
                        .map(named_children)
                        .unwrap_or_default();
                    put(kotlin_name(&parts), type_text(kotlin_type(&parts), source));
                }
            }
            "constructor_declaration" => {
                parameter_types(member.child_by_field_name("parameters"), source, &mut put);
            }
            _ => {}
        }
    }
    out
}

/// A Kotlin declaration's name: fwcd's `simple_identifier`, kotlin-ng's
/// `identifier`.
fn kotlin_name<'t>(parts: &[Node<'t>]) -> Option<Node<'t>> {
    parts
        .iter()
        .copied()
        .find(|part| matches!(part.kind(), "simple_identifier" | "identifier"))
}

/// A Kotlin declaration's type, when it is a plain or nullable type.
fn kotlin_type<'t>(parts: &[Node<'t>]) -> Option<Node<'t>> {
    parts
        .iter()
        .copied()
        .find(|part| matches!(part.kind(), "user_type" | "nullable_type"))
}

/// The members a parameter list declares: TS parameter properties, Kotlin
/// `val`/`var` class parameters, Java and C# constructor parameters.
fn parameter_types<'t>(
    parameters: Option<Node<'t>>,
    source: &str,
    put: &mut impl FnMut(Option<Node<'t>>, String),
) {
    let Some(parameters) = parameters else {
        return;
    };
    for parameter in named_children(parameters) {
        match parameter.kind() {
            // TS: `private readonly x: T`, a parameter property.
            "required_parameter" | "optional_parameter" => {
                let modified = named_children(parameter).iter().any(|child| {
                    matches!(child.kind(), "accessibility_modifier" | "override_modifier")
                });
                if !modified && !starts_with_member_modifier(text_of(parameter, source)) {
                    continue;
                }
                put(
                    parameter.child_by_field_name("pattern"),
                    type_text(parameter.child_by_field_name("type"), source),
                );
            }
            // Kotlin: `val x: T`. fwcd marks it with a `binding_pattern_kind`;
            // kotlin-ng writes the bare `val` / `var` token.
            "class_parameter" => {
                let parts = named_children(parameter);
                let property = parts
                    .iter()
                    .any(|part| part.kind() == "binding_pattern_kind")
                    || children(parameter)
                        .iter()
                        .any(|token| !token.is_named() && matches!(token.kind(), "val" | "var"));
                if property {
                    put(kotlin_name(&parts), type_text(kotlin_type(&parts), source));
                }
            }
            // C# / Java: `T x` — a field of the same name may follow.
            "parameter" | "formal_parameter" => put(
                parameter.child_by_field_name("name"),
                type_text(parameter.child_by_field_name("type"), source),
            ),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    //! Ported from upstream `__tests__/branch-guards.test.ts` (call arguments
    //! and triggers, 390-510), `__tests__/branch-guards-languages.test.ts`
    //! (the call as written, decorators, member types and loops) and
    //! `__tests__/steps-server-rendered-replies.test.ts` (`returnsInSource`,
    //! 21-46), at `e8ddc4e2`; the expected values are upstream's. Positions
    //! are found by needle the way upstream's helpers find them. The cases
    //! after `// This port's own` pin what upstream's tests leave implicit.
    use super::*;

    /// Line (1-based) of the first line containing `needle`.
    fn line_of(src: &str, needle: &str) -> usize {
        src.split('\n')
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line contains {needle}"))
            + 1
    }

    /// The line and the byte column of `needle`.
    fn at(src: &str, needle: &str) -> (usize, usize) {
        let line = line_of(src, needle);
        let column = src.split('\n').nth(line - 1).unwrap().find(needle).unwrap();
        (line, column)
    }

    fn args_at(src: &str, needle: &str, language: Language) -> Option<String> {
        let (line, column) = at(src, needle);
        call_arguments_at("/call-sites-test/args", src, language, line, Some(column))
    }

    fn site_at(src: &str, needle: &str, language: Language) -> Option<CallSite> {
        let (line, column) = at(src, needle);
        call_site_at(
            "/call-sites-test/site",
            src,
            language,
            line,
            Some(column),
            None,
        )
    }

    /// `(callee, args)` of the site, upstream's `toMatchObject` pair.
    fn called(src: &str, needle: &str, language: Language) -> (String, String) {
        let site =
            site_at(src, needle, language).unwrap_or_else(|| panic!("no call site at {needle}"));
        (site.callee, site.args)
    }

    fn pair(callee: &str, args: &str) -> (String, String) {
        (callee.to_string(), args.to_string())
    }

    fn trigger_of(src: &str, needle: &str, language: Language) -> Option<SiteTrigger> {
        let (line, column) = at(src, needle);
        trigger_at(
            "/call-sites-test/trigger",
            src,
            language,
            line,
            Some(column),
        )
    }

    fn trigger(kind: TriggerKind, name: &str, of: Option<&str>) -> Option<SiteTrigger> {
        Some(SiteTrigger {
            kind,
            name: name.to_string(),
            of: of.map(str::to_string),
        })
    }

    /// The loop headers at the site, outermost first, as `<kind> <text>`.
    fn loops_of(src: &str, needle: &str, language: Language) -> Vec<String> {
        let (line, column) = at(src, needle);
        loops_at("/call-sites-test/loops", src, language, line, Some(column))
            .into_iter()
            .map(|found| {
                let kind = match found.kind {
                    LoopKind::Each => "each",
                    LoopKind::While => "while",
                };
                format!("{kind} {}", found.text)
            })
            .collect()
    }

    fn decorators(src: &str, needle: &str, language: Language) -> Option<DefinitionDecorators> {
        decorators_of(
            "/call-sites-test/decorators",
            src,
            language,
            line_of(src, needle),
        )
    }

    fn written(own: &[&str], class: &[&str]) -> Option<DefinitionDecorators> {
        Some(DefinitionDecorators {
            own: own.iter().map(|text| text.to_string()).collect(),
            class: class.iter().map(|text| text.to_string()).collect(),
        })
    }

    fn member_types(src: &str, needle: &str, language: Language) -> BTreeMap<String, String> {
        member_types_of(
            "/call-sites-test/members",
            src,
            language,
            line_of(src, needle),
        )
    }

    fn types(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, kind)| (name.to_string(), kind.to_string()))
            .collect()
    }

    // ------------------------------------------------------- call arguments --

    const LOGIN_ARGS: &str = "
async function handleLogin(values) {
  await SecureStore.setItemAsync('userEmail', values.email)
  const res = await client.post('/auth/login', { email: values.email, password, ...rest })
  Alert.alert(i18n.t('error_login_failed'), err.message, [{ text: 'OK' }])
  router.push({ pathname: '/item/[id]', params: { id } })
  captureView.finalizeCaptureSession()
  run(() => go(), async (x) => x, new Thing(1))
  const big = fetch(`/api/${id}`, { method: 'POST', headers, body, mode, cache, credentials })
}
";

    #[test]
    fn keeps_literals_and_names_whole_folds_objects_to_their_keys_arrays_and_functions_to_a_shape()
    {
        let tsx = |needle: &str| args_at(LOGIN_ARGS, needle, Language::Tsx);
        assert_eq!(
            tsx("SecureStore.setItemAsync(").as_deref(),
            Some("'userEmail', values.email")
        );
        assert_eq!(
            tsx("client.post(").as_deref(),
            Some("'/auth/login', { email, password, ...rest }")
        );
        assert_eq!(
            tsx("Alert.alert(").as_deref(),
            Some("i18n.t(…), err.message, […]")
        );
        assert_eq!(tsx("router.push(").as_deref(), Some("{ pathname, params }"));
        assert_eq!(
            tsx("run(").as_deref(),
            Some("() => …, () => …, new Thing(…)")
        );
        assert_eq!(
            tsx("fetch(").as_deref(),
            Some("`/api/${id}`, { method, headers, body, mode, … }")
        );
    }

    #[test]
    fn an_empty_argument_list_is_an_empty_string_and_a_position_outside_a_call_is_none() {
        assert_eq!(
            args_at(
                LOGIN_ARGS,
                "captureView.finalizeCaptureSession(",
                Language::Tsx
            )
            .as_deref(),
            Some("")
        );
        assert_eq!(
            args_at(LOGIN_ARGS, "async function handleLogin", Language::Tsx),
            None
        );
    }

    #[test]
    fn swift_labels_stay_with_their_values_and_a_trailing_closure_is_a_shape() {
        let src = "
class CaptureEvents {
  func emitZipComplete(result: ZipResult) {
    sendEvent(withName: \"onZipComplete\", body: [\"zipURL\": result.url])
    tracker.setup(side: side, angle: 45)
    DispatchQueue.main.async { finish() }
  }
}
";
        let swift = |needle: &str| args_at(src, needle, Language::Swift);
        assert_eq!(
            swift("sendEvent(").as_deref(),
            Some("withName: \"onZipComplete\", body: […]")
        );
        assert_eq!(
            swift("tracker.setup(").as_deref(),
            Some("side: side, angle: 45")
        );
        assert_eq!(swift("DispatchQueue.main.async").as_deref(), Some("{ … }"));
    }

    // ------------------------------------------------------------- triggers --

    const LOGIN_TRIGGERS: &str = "
function LoginButton({ values }) {
  const formik = useFormik({
    initialValues: values,
    onSubmit: (v) => {
      handleLogin(v.email, v.password)
    },
  })
  useEffect(() => {
    warmUp()
  }, [])
  useEffect(() => {
    const sub = nativeEmitter.addListener('onZipComplete', (data) => { finish(data) })
    return () => sub.remove()
  }, [])
  const handleRemove = useCallback(() => {
    removeCredential(values.email)
  }, [values])
  fetchThing().then(() => done())
  return (
    <View>
      <Button onPress={formik.submitForm} />
      <TouchableOpacity onPress={() => handleSelectAccount(account)} />
      <Pressable onPress={handleRemove} />
      <Row.Item onLongPress={() => { if (ok) confirm() }} />
      <KeyboardAvoidingView behavior={isAndroid() ? 'height' : 'padding'} />
      <FlatList renderItem={({ item }) => renderRow(item)} keyExtractor={keyOf} />
    </View>
  )
}
function warn() {
  Alert.alert('Remove?', 'Sure?', [{ text: 'OK', onPress: () => removeAll() }], { cancelable: true })
}
";

    fn tsx_trigger(needle: &str) -> Option<SiteTrigger> {
        trigger_of(LOGIN_TRIGGERS, needle, Language::Tsx)
    }

    #[test]
    fn a_call_under_a_jsx_prop_names_the_prop_and_the_element() {
        assert_eq!(
            tsx_trigger("handleSelectAccount("),
            trigger(TriggerKind::Prop, "onPress", Some("TouchableOpacity"))
        );
        assert_eq!(
            tsx_trigger("confirm()"),
            trigger(TriggerKind::Prop, "onLongPress", Some("Row.Item"))
        );
        // A handler passed as a value: the site IS the attribute.
        assert_eq!(
            tsx_trigger("handleRemove} />"),
            trigger(TriggerKind::Prop, "onPress", Some("Pressable"))
        );
        // A function under any prop fires later; a value computed in a prop
        // runs at render.
        assert_eq!(
            tsx_trigger("renderRow(item)"),
            trigger(TriggerKind::Prop, "renderItem", Some("FlatList"))
        );
        assert_eq!(tsx_trigger("isAndroid()"), None);
        assert_eq!(tsx_trigger("keyOf}"), None);
    }

    #[test]
    fn a_call_under_an_on_option_names_the_key_and_the_call_it_configures() {
        assert_eq!(
            tsx_trigger("handleLogin("),
            trigger(TriggerKind::Option, "onSubmit", Some("useFormik"))
        );
        // The option's object inside an array argument: still the call it
        // configures.
        assert_eq!(
            tsx_trigger("removeAll()"),
            trigger(TriggerKind::Option, "onPress", Some("Alert.alert"))
        );
    }

    #[test]
    fn a_call_inside_a_runs_later_callback_names_the_callee_and_its_first_literal() {
        assert_eq!(
            tsx_trigger("warmUp()"),
            trigger(TriggerKind::Callback, "useEffect", None)
        );
        assert_eq!(
            tsx_trigger("finish(data)"),
            trigger(
                TriggerKind::Callback,
                "addListener",
                Some("'onZipComplete'")
            )
        );
        assert_eq!(
            tsx_trigger("done()"),
            trigger(TriggerKind::Callback, "then", None)
        );
    }

    #[test]
    fn a_named_handler_is_its_own_story_and_nothing_fires_the_call_inside_it_from_here() {
        assert_eq!(tsx_trigger("removeCredential("), None);
        // A plain call in a component body is fired by nothing in particular.
        assert_eq!(tsx_trigger("fetchThing()"), None);
        assert_eq!(
            trigger_of(LOGIN_TRIGGERS, "handleLogin(", Language::Swift),
            None
        );
    }

    // ------------------------------------------------- the call as written --

    const PYTHON: &str = "
@router.post(\"/\", dependencies=[Depends(auth)])
def create_item(session: SessionDep, item_in: ItemCreate) -> Any:
    if not item_in.title:
        raise HTTPException(status_code=400, detail=\"no title\")
    try:
        item = Item.model_validate(item_in, update={\"owner_id\": 1})
    except ValueError as e:
        return None
    if item.count > 0 and item.ok:
        session.add(item)
    elif item.count == 0:
        session.delete(item)
    else:
        pass
    match item.kind:
        case \"a\":
            session.commit()
        case _:
            pass
    x = a if cond else b
    for i in items:
        if i is None:
            continue
        session.refresh(i)
    return item
";

    const JAVA: &str = "
@RestController
@RequestMapping(\"/api\")
public class OwnerController {
  @PostMapping(\"/owners/new\")
  @PreAuthorize(\"hasRole('ADMIN')\")
  public String processCreationForm(@Valid Owner owner, BindingResult result) {
    if (result.hasErrors()) {
      return VIEWS;
    }
    try {
      this.owners.save(owner);
    } catch (IllegalStateException e) {
      throw new ResponseStatusException(HttpStatus.NOT_FOUND, \"x\");
    }
    switch (owner.kind) {
      case A: owners.delete(owner); break;
      default: return \"b\";
    }
    String s = cond ? a() : b();
    Owner o = new Owner(\"x\", 3);
    return cond && !late ? \"redirect:/owners/\" + owner.getId() : \"x\";
  }
}
";

    const KOTLIN: &str = "
@RestController
class OwnerController(val owners: OwnerRepository) {
  @PostMapping(\"/owners/new\")
  fun processCreationForm(@Valid owner: Owner, result: BindingResult): String {
    if (result.hasErrors()) {
      return VIEWS
    }
    try { owners.save(owner) } catch (e: IllegalStateException) { throw NotFound(\"x\") }
    when (owner.kind) {
      A -> owners.delete(owner)
      else -> return \"b\"
    }
    val s = if (cond) a() else b()
    owner.let { owners.save(it) }
    return \"redirect:/owners/\"
  }
}
";

    const CSHARP: &str = "
[ApiController]
public class TodoController : ControllerBase {
  [HttpPost(\"items\")]
  [Authorize(Roles = \"Admin\")]
  public async Task<IActionResult> Create([FromBody] Item item) {
    if (item == null) return BadRequest();
    try { await _context.Items.AddAsync(item); } catch (DbUpdateException e) { return Conflict(); }
    switch (item.Kind) { case 1: _bus.Publish(item); break; default: break; }
    var x = cond ? Ok(item) : NotFound();
    return item.Ok && !late ? Created(\"x\", item) : StatusCode(500);
  }
}
";

    const GO: &str = "
package main
func createUser(c *gin.Context) {
  if err := c.BindJSON(&u); err != nil {
    c.JSON(http.StatusBadRequest, gin.H{\"error\": err.Error()})
    return
  }
  if u.Name == \"\" && !ok {
    c.AbortWithStatus(404)
  } else if u.Age > 3 {
    db.Create(&u)
  } else {
    db.Save(&u)
  }
  switch u.Kind {
  case \"a\":
    q.Publish(\"x\", u)
  default:
    return
  }
  go worker(u)
  c.JSON(http.StatusCreated, u)
}
";

    const C: &str = "
int main(int argc, char **argv) {
  FILE *f = fopen(argv[1], \"r\");
  if (!f) { perror(\"open\"); return 1; }
  if (argc > 2 && flag) fprintf(stderr, \"x %d\", argc);
  else exit(2);
  switch (argc) { case 1: fclose(f); break; default: break; }
  int x = argc ? read(fd, buf, 10) : 0;
  return 0;
}
";

    #[test]
    fn python_reads_the_call_as_written_with_keyword_arguments() {
        assert_eq!(
            called(PYTHON, "HTTPException(", Language::Python),
            pair("HTTPException", "status_code=400, detail=\"no title\"")
        );
        assert_eq!(
            called(PYTHON, "Item.model_validate", Language::Python),
            pair("Item.model_validate", "item_in, update={ \"owner_id\" }")
        );
    }

    #[test]
    fn java_reads_the_call_as_written() {
        assert_eq!(
            called(JAVA, "new Owner(", Language::Java),
            pair("Owner", "\"x\", 3")
        );
        assert_eq!(
            called(JAVA, "this.owners.save", Language::Java),
            pair("this.owners.save", "owner")
        );
        assert_eq!(
            called(JAVA, "new ResponseStatusException", Language::Java),
            pair("ResponseStatusException", "HttpStatus.NOT_FOUND, \"x\"")
        );
    }

    #[test]
    fn kotlin_reads_the_call_as_written_and_a_trailing_lambda_as_a_shape() {
        assert_eq!(
            called(KOTLIN, "owners.delete(owner)", Language::Kotlin),
            pair("owners.delete", "owner")
        );
        // A trailing lambda is `{ … }`, as Swift's closure is — not its body.
        let lambda = "class A(val prefs: DataStore<P>) {\n  suspend fun set(b: Boolean) {\n    prefs.updateData { it.copy { bookmarked = b } }\n  }\n}\n";
        assert_eq!(
            called(lambda, "prefs.updateData", Language::Kotlin),
            pair("prefs.updateData", "{ … }")
        );
    }

    #[test]
    fn csharp_reads_the_call_as_written() {
        assert_eq!(
            called(CSHARP, "_context.Items.AddAsync", Language::CSharp),
            pair("_context.Items.AddAsync", "item")
        );
        assert_eq!(
            called(CSHARP, "Created(\"x\"", Language::CSharp),
            pair("Created", "\"x\", item")
        );
    }

    #[test]
    fn go_reads_the_call_as_written_and_a_composite_literal_as_its_type() {
        assert_eq!(
            called(GO, "c.JSON(http.StatusBadRequest", Language::Go),
            pair("c.JSON", "http.StatusBadRequest, gin.H{…}")
        );
    }

    #[test]
    fn c_reads_the_call_as_written() {
        assert_eq!(
            called(C, "fprintf(", Language::C),
            pair("fprintf", "stderr, \"x %d\", argc")
        );
    }

    // ----------------------------------------------------------- decorators --

    #[test]
    fn python_reads_the_decorators_on_the_definition() {
        assert_eq!(
            decorators(PYTHON, "def create_item", Language::Python),
            written(&["router.post(\"/\", dependencies=[Depends(auth)])"], &[])
        );
    }

    #[test]
    fn java_reads_the_annotations_on_the_method_and_its_class() {
        assert_eq!(
            decorators(JAVA, "public String processCreationForm", Language::Java),
            written(
                &[
                    "PostMapping(\"/owners/new\")",
                    "PreAuthorize(\"hasRole('ADMIN')\")"
                ],
                &["RestController", "RequestMapping(\"/api\")"]
            )
        );
    }

    #[test]
    fn kotlin_reads_the_annotations() {
        assert_eq!(
            decorators(KOTLIN, "fun processCreationForm", Language::Kotlin),
            written(&["PostMapping(\"/owners/new\")"], &["RestController"])
        );
    }

    #[test]
    fn csharp_reads_the_attributes_on_the_action_and_its_controller() {
        assert_eq!(
            decorators(
                CSHARP,
                "public async Task<IActionResult> Create",
                Language::CSharp
            ),
            written(
                &["HttpPost(\"items\")", "Authorize(Roles = \"Admin\")"],
                &["ApiController"]
            )
        );
    }

    // --------------------------------------------------------- member types --

    #[test]
    fn typescript_member_types_are_constructor_parameter_properties_and_typed_fields() {
        let src = "
@Injectable()
export class CatsService {
  private readonly log: Logger = new Logger()
  constructor(
    @InjectRepository(Cat) private readonly catsRepository: Repository<Cat>,
    private readonly mailer: MailerService,
    plain: string
  ) {}
  async create(dto) {
    return this.catsRepository.save(dto)
  }
}
";
        assert_eq!(
            member_types(src, "async create", Language::TypeScript),
            types(&[
                ("log", "Logger"),
                ("catsRepository", "Repository<Cat>"),
                ("mailer", "MailerService"),
            ])
        );
    }

    #[test]
    fn java_member_types_are_fields_and_constructor_parameters() {
        let src = "
public class OwnerController {
  private final OwnerRepository owners;
  private VisitService visits;
  public OwnerController(OwnerRepository owners, Clock clock) { this.owners = owners; }
  public String create(Owner owner) { return owners.save(owner); }
}
";
        assert_eq!(
            member_types(src, "public String create", Language::Java),
            types(&[
                ("owners", "OwnerRepository"),
                ("visits", "VisitService"),
                ("clock", "Clock"),
            ])
        );
    }

    #[test]
    fn kotlin_member_types_are_the_primary_constructor_and_properties() {
        let src = "
class OwnerController(val owners: OwnerRepository, private val visits: VisitService, plain: String) {
  val clock: Clock = Clock.systemUTC()
  fun create(owner: Owner): String = owners.save(owner)
}
";
        assert_eq!(
            member_types(src, "fun create", Language::Kotlin),
            types(&[
                ("owners", "OwnerRepository"),
                ("visits", "VisitService"),
                ("clock", "Clock"),
            ])
        );
    }

    #[test]
    fn csharp_member_types_are_fields_properties_and_constructor_parameters() {
        let src = "
public class OrderService : IOrderService {
  private readonly IRepository<Order> _orderRepository;
  public IEmailSender Mailer { get; }
  public OrderService(IRepository<Order> orderRepository, IUriComposer uriComposer) { _orderRepository = orderRepository; }
  public async Task Create(Order o) { await _orderRepository.AddAsync(o); }
}
";
        assert_eq!(
            member_types(src, "public async Task Create", Language::CSharp),
            types(&[
                ("_orderRepository", "IRepository<Order>"),
                ("Mailer", "IEmailSender"),
                ("orderRepository", "IRepository<Order>"),
                ("uriComposer", "IUriComposer"),
            ])
        );
    }

    // ---------------------------------------------------------------- loops --

    #[test]
    fn loops_read_a_js_for_of_and_a_while() {
        let src = "
function run(items) {
  for (const item of items) {
    save(item)
  }
  while (queue.length > 0) {
    drain()
  }
}";
        assert_eq!(
            loops_of(src, "save(item)", Language::JavaScript),
            ["each item of items"]
        );
        assert_eq!(
            loops_of(src, "drain()", Language::JavaScript),
            ["while queue.length > 0"]
        );
    }

    #[test]
    fn loops_read_nested_loops_outermost_first() {
        let src = "
function run(rows) {
  for (const row of rows) {
    for (const cell of row) {
      draw(cell)
    }
  }
}";
        assert_eq!(
            loops_of(src, "draw(cell)", Language::JavaScript),
            ["each row of rows", "each cell of row"]
        );
    }

    #[test]
    fn loops_read_nothing_for_a_site_outside_every_loop() {
        let src = "
function run(items) {
  begin()
  for (const item of items) { save(item) }
}";
        assert!(loops_of(src, "begin()", Language::JavaScript).is_empty());
    }

    #[test]
    fn loops_read_a_python_for_and_a_while() {
        let src = "
def run(items):
    for item in items:
        save(item)
    while pending:
        drain()
";
        assert_eq!(
            loops_of(src, "save(item)", Language::Python),
            ["each item in items"]
        );
        assert_eq!(
            loops_of(src, "drain()", Language::Python),
            ["while pending"]
        );
    }

    #[test]
    fn loops_read_a_java_enhanced_for() {
        let src = "
class A {
  void run(List<Item> items) {
    for (Item item : items) {
      save(item);
    }
  }
}";
        assert_eq!(
            loops_of(src, "save(item)", Language::Java),
            ["each Item item : items"]
        );
    }

    #[test]
    fn loops_read_a_go_range_loop() {
        let src = "
func run(items []Item) {
\tfor _, item := range items {
\t\tsave(item)
\t}
}";
        assert_eq!(
            loops_of(src, "save(item)", Language::Go),
            ["each _, item := range items"]
        );
    }

    #[test]
    fn loops_read_a_csharp_foreach() {
        let src = "
class A {
  void Run(List<Item> items) {
    foreach (var item in items) {
      Save(item);
    }
  }
}";
        // The binding word is noise in a header a person reads: `var` goes.
        assert_eq!(
            loops_of(src, "Save(item)", Language::CSharp),
            ["each item in items"]
        );
    }

    #[test]
    fn loops_read_a_swift_for_in() {
        let src = "
func run(items: [Item]) {
  for item in items {
    save(item)
  }
}";
        assert_eq!(
            loops_of(src, "save(item)", Language::Swift),
            ["each item in items"]
        );
    }

    #[test]
    fn loops_read_a_kotlin_for() {
        let src = "
fun run(items: List<Item>) {
    for (item in items) {
        save(item)
    }
}";
        assert_eq!(
            loops_of(src, "save(item)", Language::Kotlin),
            ["each item in items"]
        );
    }

    #[test]
    fn loops_read_nothing_for_a_language_without_rules() {
        assert!(
            loops_at(
                "/call-sites-test/loops-ruby",
                "def f\n  xs.each { save }\nend\n",
                Language::Ruby,
                2,
                Some(2)
            )
            .is_empty()
        );
    }

    // -------------------------------------------------------------- returns --

    #[test]
    fn lists_a_methods_own_returns_a_constants_value_included_and_none_from_lambdas_or_nested_classes()
     {
        let src = "@Controller
class OwnerController {
    private static final String VIEWS_FORM = \"owners/createOrUpdateOwnerForm\";
    @PostMapping(\"/owners/new\")
    public String processCreationForm(@Valid Owner owner, BindingResult result) {
        if (result.hasErrors()) {
            return VIEWS_FORM;
        }
        list.forEach(x -> { return; });
        Runnable r = () -> { return \"not-mine\"; };
        Object o = new Object() { public String toString() { return \"nested\"; } };
        this.owners.save(owner);
        return \"redirect:/owners/\" + owner.getId();
    }
}
";
        let returns: Vec<(usize, String, Option<String>)> = returns_of(
            "/call-sites-test/returns",
            src,
            Language::Java,
            line_of(src, "@PostMapping"),
        )
        .into_iter()
        .map(|site| (site.line, site.expression, site.value))
        .collect();
        assert_eq!(
            returns,
            [
                (
                    line_of(src, "return VIEWS_FORM"),
                    "VIEWS_FORM".to_string(),
                    Some("owners/createOrUpdateOwnerForm".to_string())
                ),
                (
                    line_of(src, "return \"redirect"),
                    "\"redirect:/owners/\" + owner.getId()".to_string(),
                    None
                ),
            ]
        );
    }

    // This port's own: what upstream's tests leave implicit.

    #[test]
    fn within_names_the_call_whose_arguments_hold_the_site_and_stops_at_a_function() {
        let src = "
function login(req, res) {
  res.json({ token: generateToken(user) })
  items.map((x) => save(x))
  audit(user)
}
";
        let site = |needle: &str| site_at(src, needle, Language::TypeScript).unwrap();
        assert_eq!(site("generateToken(").within.as_deref(), Some("res.json"));
        // A call that is a statement of its own inside a callback is not
        // written inside the arguments of the call that takes the callback.
        assert_eq!(site("save(x)").within, None);
        assert_eq!(site("audit(user)").within, None);
    }

    #[test]
    fn the_span_is_where_the_call_starts_and_ends() {
        let src = "function f() {\n  go(a,\n     b)\n}\n";
        assert_eq!(
            site_at(src, "go(", Language::TypeScript).unwrap().span,
            Some(SiteSpan {
                start: SitePoint { line: 2, column: 2 },
                end: SitePoint { line: 3, column: 7 },
            })
        );
    }

    #[test]
    fn a_status_is_read_from_an_object_argument_or_from_the_statement_before_the_reply() {
        let src = "
function create(req, res) {
  res.json({ status: 201, user })
}
function update(req, res) {
  res.status(202);
  res.json(user)
}
function guarded(req, res) {
  if (missing) res.status(404)
  res.json(user)
}
function other(req, c) {
  cc.status(203)
  c.json(user)
}
";
        let status = |needle: &str| site_at(src, needle, Language::TypeScript).unwrap().status;
        assert_eq!(status("res.json({ status"), Some(201));
        let (line, column) = at(src, "res.json(user)");
        assert_eq!(
            call_site_at(
                "/call-sites-test/status",
                src,
                Language::TypeScript,
                line,
                Some(column),
                None
            )
            .unwrap()
            .status,
            Some(202)
        );
        // Only a statement that IS the status call counts: one inside an `if`
        // before the reply is another path's.
        let guarded = line_of(src, "if (missing)") + 1;
        assert_eq!(
            call_site_at(
                "/call-sites-test/status",
                src,
                Language::TypeScript,
                guarded,
                Some(2),
                None
            )
            .unwrap()
            .status,
            None
        );
        // The receiver is matched whole: `cc` is not `c`.
        assert_eq!(status("c.json(user)"), None);
    }

    #[test]
    fn a_callee_name_climbs_from_the_start_of_a_chain_to_the_call_it_names() {
        let src = "function f(res) {\n  res.status(201).json({ ok })\n}\n";
        let read = |callee: Option<&str>| {
            call_site_at(
                "/call-sites-test/chain",
                src,
                Language::TypeScript,
                2,
                Some(2),
                callee,
            )
            .map(|site| (site.callee, site.args))
        };
        assert_eq!(read(None), Some(pair("res.status", "201")));
        assert_eq!(
            read(Some("json")),
            Some(pair("res.status(201).json", "{ ok }"))
        );
        // A column one off the callee (a 1-based one) still finds the call.
        assert_eq!(
            call_site_at(
                "/call-sites-test/chain",
                src,
                Language::TypeScript,
                2,
                Some(3),
                Some("json")
            )
            .map(|site| site.callee),
            Some("res.status(201).json".to_string())
        );
    }

    #[test]
    fn kotlin_keeps_a_named_arguments_name_with_its_value() {
        // kotlin-ng names the argument with an `identifier`, where fwcd's
        // grammar had a `simple_identifier`: the same reading either way.
        let src = "class A {\n  fun f() {\n    repo.save(owner, flush = true)\n  }\n}\n";
        assert_eq!(
            called(src, "repo.save(", Language::Kotlin),
            pair("repo.save", "owner, flush: true")
        );
    }

    #[test]
    fn kotlin_lists_a_functions_own_returns_and_none_from_its_lambdas() {
        // kotlin-ng writes `return` as a `return_expression`.
        let src = "class C {
  fun f(x: Boolean): String {
    if (x) {
      return \"a\"
    }
    items.forEach { return@forEach }
    return VIEW
  }
}
";
        let returns: Vec<(usize, String)> = returns_of(
            "/call-sites-test/returns-kotlin",
            src,
            Language::Kotlin,
            line_of(src, "fun f"),
        )
        .into_iter()
        .map(|site| (site.line, site.expression))
        .collect();
        assert_eq!(
            returns,
            [
                (line_of(src, "return \"a\""), "\"a\"".to_string()),
                (line_of(src, "return VIEW"), "VIEW".to_string()),
            ]
        );
    }

    #[test]
    fn a_language_without_rules_or_an_oversized_source_reads_nothing() {
        let ruby = "def f\n  go(1)\nend\n";
        let key = "/call-sites-test/none";
        assert_eq!(
            call_site_at(key, ruby, Language::Ruby, 2, Some(2), None),
            None
        );
        assert_eq!(
            call_arguments_at(key, ruby, Language::Ruby, 2, Some(2)),
            None
        );
        assert_eq!(trigger_at(key, ruby, Language::Ruby, 2, Some(2)), None);
        assert_eq!(returns_of(key, ruby, Language::Ruby, 1), Vec::new());
        assert_eq!(decorators_of(key, ruby, Language::Ruby, 1), None);
        assert!(member_types_of(key, ruby, Language::Ruby, 1).is_empty());
        let small = "function f() {\n  go(1)\n}\n";
        assert!(call_site_at(key, small, Language::TypeScript, 2, Some(2), None).is_some());
        let huge = format!(
            "{small}{}",
            " ".repeat(crate::branch_guards::MAX_PARSE_BYTES)
        );
        assert_eq!(
            call_site_at(key, &huge, Language::TypeScript, 2, Some(2), None),
            None
        );
    }
}
