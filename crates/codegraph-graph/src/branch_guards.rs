//! Branch guards — the conditions under which a call site runs. A port of
//! the guard half of upstream `src/graph/branch-guards.ts` (`b635dd46`,
//! #1655); its call arguments, triggers, loops and return sites are the
//! viewer's Steps (W7-22).
//!
//! An edge says `handlePress → openObjectDetail`. What a reader wants to know
//! is that it happens **when `isCollected`** and **not while `isUploading`**:
//!
//! ```text
//! if (isUploading) return            ← early-return guard: !isUploading
//! if (isCollected) {                 ← if: isCollected
//!   openObjectDetail(item)           ← the call site
//! ```
//!
//! Given a file's source, its language and a call site (line, column), the
//! walk climbs from the innermost node at that position to the enclosing
//! function and records every branch it passes through: `if` / `else` /
//! `else if`, the arms of a ternary, `switch` cases, the right side of `&&` /
//! `||`, a `catch`, and — at each statement block on the way — the early exits
//! that precede the site (`if (x) return`, Swift `guard x else { return }`).
//!
//! Nothing is stored in the index: labels are computed where they are shown,
//! from the source as it is now. Only what the syntax tree states is
//! reported. Loops are not conditions; a condition that cannot be read (a
//! language without rules here, a file that will not parse) yields no label
//! rather than a wrong one.
//!
//! Positions are tree-sitter's: a 1-based line and a 0-based BYTE column, the
//! column an edge's `col` records.

use std::sync::{Mutex, OnceLock};

use codegraph_core::types::Language;
use codegraph_extract::lang::spec_for_language;
use regex::Regex;
use tree_sitter::{Node, Parser, Point, Tree};

/// The construct a guard comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardForm {
    If,
    Else,
    Ternary,
    Case,
    Guard,
    And,
    Or,
    Catch,
}

/// How an arm leaves the flow: back to the caller, or by an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardExit {
    Return,
    Throw,
    Exit,
}

/// One condition a call site runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchGuard {
    /// The condition's source, whitespace-collapsed, outer parens dropped,
    /// capped in length.
    pub text: String,
    /// The site runs when the condition is FALSE (an else arm, an
    /// early-return guard, `||`).
    pub negated: bool,
    pub form: GuardForm,
    /// Line of the condition (1-based); 0 when the guard has no condition
    /// node (`catch`).
    pub line: usize,
    /// Where the branching construct starts, `line:column` — the identity of
    /// the FORK rather than of the arm: the `if` an `if` guard and its `else`
    /// guard both come from, the `switch` every one of its cases comes from.
    /// Empty when the walk could not place it.
    pub branch: String,
    /// How the arm the site is IN leaves, when it always does.
    pub arm_exit: Option<GuardExit>,
    /// For an early exit (`form: Guard`), how the arm that was NOT taken
    /// leaves.
    pub exit: Option<GuardExit>,
}

/// Longest condition text kept before it is cut with an ellipsis, in UTF-16
/// code units as upstream counts.
const MAX_TEXT: usize = 80;

/// Sources above this size are not parsed for labels: a call site in such a
/// file simply shows no condition.
pub const MAX_PARSE_BYTES: usize = 256 * 1024;

/// Languages with walk rules here. Others yield no guards (never a wrong one).
pub fn supports_branch_guards(language: Language) -> bool {
    rules_for(language).is_some()
}

/// The label a flow connector prints: the conditions in execution order,
/// joined with `&&`, each negated one written as `!x`. Empty when the site is
/// unconditional.
pub fn guard_label(guards: &[BranchGuard]) -> String {
    guards
        .iter()
        .map(render_guard)
        .collect::<Vec<_>>()
        .join(" && ")
}

fn render_guard(guard: &BranchGuard) -> String {
    let text = guard.text.as_str();
    if guard.form == GuardForm::Catch {
        return text.to_string();
    }
    // `if (!object?.id || !object?.name)` joined to the guard before it with
    // `&&` would read as two conditions: it keeps its parentheses.
    if !guard.negated {
        return if has_top_level_or(text) {
            format!("({text})")
        } else {
            text.to_string()
        };
    }
    // `!x` negated reads back as `x`; a simple operand takes a bare `!`;
    // anything with operators is parenthesised so the negation is unambiguous.
    if let Some(rest) = text.strip_prefix('!')
        && !rest.starts_with('=')
        && is_simple_operand(rest)
    {
        return rest.to_string();
    }
    if not_prefix_regex().is_match(text) {
        let rest = text.get(4..).unwrap_or("").trim();
        if is_simple_operand(rest) {
            return rest.to_string();
        }
    }
    // One comparison flips instead of wrapping: the reader of a Go `if err !=
    // nil { return }` wants `err == nil`, not `!(err != nil)`.
    if let Some(flipped) = flip_comparison(text) {
        return flipped;
    }
    if is_simple_operand(text) {
        format!("!{text}")
    } else {
        format!("!({text})")
    }
}

fn not_prefix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^not\s+").expect("not prefix"))
}

/// `a != b` → `a == b`, `x is None` → `x is not None`; `None` when the text is
/// not one plain comparison.
fn flip_comparison(text: &str) -> Option<String> {
    static COMPOUND: OnceLock<Regex> = OnceLock::new();
    static COMPARISON: OnceLock<Regex> = OnceLock::new();
    let compound = COMPOUND.get_or_init(|| {
        Regex::new(r"&&|\|\||(?-u:\b)and(?-u:\b)|(?-u:\b)or(?-u:\b)|\?").expect("compound")
    });
    if compound.is_match(text) || has_top_level_or(text) {
        return None;
    }
    let comparison = COMPARISON.get_or_init(|| {
        Regex::new(
            r"^([^=!<>]+?)\s*(===|!==|==|!=|(?-u:\b)is not(?-u:\b)|(?-u:\b)is(?-u:\b))\s*([^=!<>]+)$",
        )
        .expect("comparison")
    });
    let captures = comparison.captures(text)?;
    let op = match &captures[2] {
        "===" => "!==",
        "!==" => "===",
        "==" => "!=",
        "!=" => "==",
        "is" => "is not",
        "is not" => "is",
        _ => return None,
    };
    Some(format!(
        "{} {op} {}",
        captures[1].trim(),
        captures[3].trim()
    ))
}

/// A `||` outside every bracket and string — the condition is a disjunction
/// as written.
fn has_top_level_or(text: &str) -> bool {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(open) = quote {
            if ch == '\\' {
                chars.next();
            } else if ch == open {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '|' if depth == 0 && chars.peek() == Some(&'|') => return true,
            _ => {}
        }
    }
    false
}

/// A name, a member chain, or one call on it — `Objects.equals(owner.getId(),
/// id)` included: the parens must balance and nothing may sit outside them.
fn is_simple_operand(text: &str) -> bool {
    static OPERATOR: OnceLock<Regex> = OnceLock::new();
    static CONNECTIVE: OnceLock<Regex> = OnceLock::new();
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    let operator = OPERATOR.get_or_init(|| Regex::new(r"[=<>]").expect("operator"));
    let connective =
        CONNECTIVE.get_or_init(|| Regex::new(r"\s(?:&&|\|\||and|or)\s").expect("connective"));
    if operator.is_match(text) || connective.is_match(text) {
        return false;
    }
    let shape =
        SHAPE.get_or_init(|| Regex::new(r"(?s)^([A-Za-z0-9_$.?!]+)(\(.*\))?$").expect("shape"));
    let Some(captures) = shape.captures(text) else {
        return false;
    };
    let Some(call) = captures.get(2) else {
        return true;
    };
    let call = call.as_str();
    let mut depth = 0i64;
    for (index, ch) in call.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 && index + 1 < call.len() {
                return false;
            }
        }
    }
    depth == 0
}

// =============================================================================
// Trees, cached per file version
// =============================================================================

struct CachedTree {
    key: String,
    language: Language,
    source: String,
    tree: Tree,
}

/// Parsed trees kept across requests, most recent last, so a Flow whose hops
/// share a caller file parses it once.
const TREE_CACHE_SIZE: usize = 8;

fn tree_cache() -> &'static Mutex<Vec<CachedTree>> {
    static CACHE: OnceLock<Mutex<Vec<CachedTree>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn parse(source: &str, language: Language) -> Option<Tree> {
    let spec = spec_for_language(language)?;
    let mut parser = Parser::new();
    parser
        .set_language(&spec.tree_sitter_language_for_source(source))
        .ok()?;
    parser.parse(source, None)
}

/// The guards of one call site in a file, given the file's source as the
/// caller read and checked it. `key` names the file (its absolute path); the
/// tree is reused while the same source is asked about again. A language
/// without rules, a source above [`MAX_PARSE_BYTES`] or one that does not
/// parse yields no guards.
pub fn guards_at(
    key: &str,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Vec<BranchGuard> {
    if !supports_branch_guards(language) || source.len() > MAX_PARSE_BYTES {
        return Vec::new();
    }
    let mut cache = tree_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let hit = cache
        .iter()
        .position(|entry| entry.key == key && entry.language == language && entry.source == source);
    let entry = match hit {
        Some(index) => cache.remove(index),
        None => {
            let Some(tree) = parse(source, language) else {
                return Vec::new();
            };
            cache.retain(|entry| entry.key != key);
            CachedTree {
                key: key.to_string(),
                language,
                source: source.to_string(),
                tree,
            }
        }
    };
    let guards = guards_in_tree(
        entry.tree.root_node(),
        &entry.source,
        language,
        line,
        column,
    );
    cache.push(entry);
    if cache.len() > TREE_CACHE_SIZE {
        cache.remove(0);
    }
    guards
}

/// Guards for one site in source text, parsed afresh — the test seam.
pub fn guards_in_source(
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Vec<BranchGuard> {
    if !supports_branch_guards(language) {
        return Vec::new();
    }
    match parse(source, language) {
        Some(tree) => guards_in_tree(tree.root_node(), source, language, line, column),
        None => Vec::new(),
    }
}

/// The walk. `line` is 1-based, `column` a 0-based byte column (`None` → the
/// first non-blank column). Returns the guards outermost first — execution
/// order, the way a reader would list them.
pub fn guards_in_tree(
    root: Node<'_>,
    source: &str,
    language: Language,
    line: usize,
    column: Option<usize>,
) -> Vec<BranchGuard> {
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
    let mut found: Vec<BranchGuard> = Vec::new();

    // Innermost → outermost. `found` is reversed at the end, so within one
    // level anything meant to read as OUTER must be pushed LATER.
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
        // Anything `enclosing` pushed came from THIS construct, and the arm
        // the site is in is the child the walk came up through: stamp both,
        // unless the rule already named a different branch (a `switch` case's
        // is the switch).
        let before = found.len();
        (rules.enclosing)(parent, node, source, &mut found);
        let arm = exit_kind(Some(node), source);
        for guard in &mut found[before..] {
            if guard.branch.is_empty() {
                guard.branch = branch_key(parent);
            }
            if arm.is_some() && guard.arm_exit.is_none() {
                guard.arm_exit = arm;
            }
        }
        if rules.blocks.contains(&parent.kind()) {
            (rules.early_exits)(parent, node, source, &mut found);
        }
        node = parent;
    }
    found.reverse();
    found
}

fn first_non_blank_column(source: &str, row: usize) -> usize {
    let line = source.split('\n').nth(row).unwrap_or("");
    line.find(|ch: char| !ch.is_whitespace()).unwrap_or(0)
}

/// The innermost named node containing (row, col). `descendant_for_point_range`
/// is the fast path, but some grammars (Swift's `statements`) answer with the
/// container, so the result is refined by descending while a named child
/// still contains the point.
fn innermost_at(root: Node<'_>, row: usize, col: usize) -> Option<Node<'_>> {
    let point = Point { row, column: col };
    let mut node = root.descendant_for_point_range(point, point)?;
    loop {
        let next = named_children(node).into_iter().find(|child| {
            let start = child.start_position();
            let end = child.end_position();
            let after_start = start.row < row || (start.row == row && start.column <= col);
            let before_end = end.row > row || (end.row == row && end.column > col);
            after_start && before_end
        });
        match next {
            Some(child) => node = child,
            None => return Some(node),
        }
    }
}

// =============================================================================
// Language rules
// =============================================================================

type Visit = for<'t> fn(Node<'t>, Node<'t>, &str, &mut Vec<BranchGuard>);

struct Rules {
    /// Node types the walk never climbs past: the function the site belongs
    /// to. An INLINE function — an arrow passed as an argument, a closure in
    /// an object literal, a trailing closure — is not a boundary: the
    /// conditions around its definition are the conditions under which it
    /// exists at all. A function that is declared, or assigned to a name,
    /// starts its own story.
    boundaries: &'static [&'static str],
    /// Function-expression types that are boundaries only when named/assigned.
    inline_functions: &'static [&'static str],
    /// Parent types under which an inline function counts as named/assigned.
    binding_parents: &'static [&'static str],
    /// Statement containers whose earlier children may be early exits.
    blocks: &'static [&'static str],
    /// `parent` encloses `child` (the node the walk came up through): record
    /// any branch.
    enclosing: Visit,
    /// `child` is a statement of block `parent`: record the exits before it.
    early_exits: Visit,
}

fn rules_for(language: Language) -> Option<&'static Rules> {
    match language {
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => Some(&JS),
        Language::Swift => Some(&SWIFT),
        Language::Python => Some(&PYTHON),
        Language::Java => Some(&JAVA),
        Language::Kotlin => Some(&KOTLIN),
        Language::CSharp => Some(&CSHARP),
        Language::Go => Some(&GO),
        Language::C | Language::Cpp | Language::ObjC => Some(&C),
        _ => None,
    }
}

fn text_of<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text`, cut to `max` UTF-16 code units with an ellipsis.
fn cut(text: &str, max: usize) -> String {
    if text.encode_utf16().count() <= max {
        return text.to_string();
    }
    let mut units = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        units += ch.len_utf16();
        if units > max - 1 {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn cond_text(node: Option<Node<'_>>, source: &str) -> String {
    let Some(mut node) = node else {
        return String::new();
    };
    // `(x)` — the parens are the statement's, not the condition's.
    while node.kind() == "parenthesized_expression" && node.named_child_count() == 1 {
        let Some(inner) = node.named_child(0) else {
            break;
        };
        node = inner;
    }
    let text = collapse(text_of(node, source));
    // A bare keyword is a grammar's error recovery (`let` from an `if let` it
    // could not parse), never a condition a reader can use.
    if matches!(
        text.as_str(),
        "let" | "var" | "case" | "try" | "await" | "guard" | "if" | "else" | "some" | "any"
    ) {
        return String::new();
    }
    cut(&text, MAX_TEXT)
}

/// One guard. `branch` names the construct the arm belongs to: by default
/// the node the walk is climbing THROUGH (an `if`, a ternary, a `catch`),
/// stamped by the walk, and overridden where it is not the site's parent, by
/// a `switch` case (whose parent is the case) and by an early exit (whose
/// branch is the `if` that returned, several statements back).
fn guard(
    form: GuardForm,
    cond: Option<Node<'_>>,
    negated: bool,
    text: Option<String>,
    branch: Option<Node<'_>>,
    exit: Option<GuardExit>,
    source: &str,
) -> Option<BranchGuard> {
    let text = text.unwrap_or_else(|| cond_text(cond, source));
    if text.is_empty() {
        return None;
    }
    Some(BranchGuard {
        text,
        negated,
        form,
        line: cond.map_or(0, |cond| cond.start_position().row + 1),
        branch: branch.map(branch_key).unwrap_or_default(),
        arm_exit: None,
        exit,
    })
}

/// A branching construct's identity: where it starts, `line:column`.
fn branch_key(node: Node<'_>) -> String {
    let start = node.start_position();
    format!("{}:{}", start.row + 1, start.column)
}

/// Containers whose LAST statement decides how the whole thing leaves: a
/// block, and the clause wrappers a grammar puts an arm's block inside
/// (`else_clause`, `except_clause`). A CONDITIONAL wrapper is not one of them:
/// an `elif` whose body raises does not mean the arm it is in always raises.
const BLOCKISH: &[&str] = &[
    "statement_block",
    "block",
    "statement_list",
    "statements",
    "function_body",
    "compound_statement",
    "control_structure_body",
    "else_clause",
    "else_statement",
    "catch_clause",
    "catch_block",
    "except_clause",
    "finally_clause",
];

/// How a statement — or a block, by its last statement — leaves: `Throw` for
/// a raised error, `Return` for a return / break / continue, `Exit` for a
/// language's other way out that the rules count as an exit but no keyword
/// names. `None` when it does not always leave.
fn exit_kind(node: Option<Node<'_>>, source: &str) -> Option<GuardExit> {
    let node = node?;
    let kind = node.kind();
    if matches!(
        kind,
        "throw_statement" | "raise_statement" | "throw_expression"
    ) {
        return Some(GuardExit::Throw);
    }
    if let Some(keyword) = kind.strip_suffix("_statement")
        && matches!(keyword, "return" | "break" | "continue" | "goto" | "yield")
    {
        return Some(GuardExit::Return);
    }
    // kotlin-ng writes `return x` as `return_expression`, and a bare `break`
    // or `continue` as an identifier of that text (both are hard keywords, so
    // no name can be spelled that way).
    if kind == "return_expression" || is_kotlin_jump_identifier(node, source) {
        return Some(GuardExit::Return);
    }
    // Swift's `control_transfer_statement` and Kotlin's `jump_expression` say
    // which in their first word.
    if kind == "control_transfer_statement" || kind == "jump_expression" {
        return Some(if starts_with_word(text_of(node, source), "throw") {
            GuardExit::Throw
        } else {
            GuardExit::Return
        });
    }
    if BLOCKISH.contains(&kind) {
        return exit_kind(last_named(node), source);
    }
    None
}

/// `/^\s*word\b/`.
fn starts_with_word(text: &str, word: &str) -> bool {
    text.trim_start().strip_prefix(word).is_some_and(|rest| {
        !rest
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// [`exit_kind`], or `Exit` when the language's rules call it an exit and no
/// keyword names it.
fn exit_kind_or(node: Option<Node<'_>>, exits: bool, source: &str) -> Option<GuardExit> {
    if !exits {
        return None;
    }
    exit_kind(node, source).or(Some(GuardExit::Exit))
}

fn push(out: &mut Vec<BranchGuard>, guard: Option<BranchGuard>) {
    if let Some(guard) = guard {
        out.push(guard);
    }
}

fn is_field(parent: Node<'_>, field: &str, child: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|node| node.id() == child.id())
}

fn last_named(node: Node<'_>) -> Option<Node<'_>> {
    let count = node.named_child_count();
    if count == 0 {
        return None;
    }
    node.named_child((count - 1) as u32)
}

/// The named children of a node, in order.
fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .filter_map(|index| node.named_child(index as u32))
        .collect()
}

/// Every child of a node, named or not, in order.
fn children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.child_count())
        .filter_map(|index| node.child(index))
        .collect()
}

/// The named children of `parent` that come before `child`, in source order.
fn preceding_siblings<'t>(parent: Node<'t>, child: Node<'t>) -> Vec<Node<'t>> {
    named_children(parent)
        .into_iter()
        .take_while(|sibling| sibling.id() != child.id())
        .collect()
}

/// Where `child` sits among ALL of `parent`'s children.
fn index_of(parent: Node<'_>, child: Node<'_>) -> Option<u32> {
    (0..parent.child_count()).find(|&index| {
        parent
            .child(index)
            .is_some_and(|node| node.id() == child.id())
    })
}

/// The field `child` is held under in `parent`, if any.
fn field_of<'t>(parent: Node<'t>, child: Node<'_>) -> Option<&'t str> {
    index_of(parent, child).and_then(|index| parent.field_name_for_child(index))
}

fn case_text(subject: &str, value: &str, equals: &str) -> String {
    if subject.is_empty() {
        value.to_string()
    } else {
        format!("{subject} {equals} {value}")
    }
}

fn default_text(subject: &str, word: &str) -> String {
    if subject.is_empty() {
        word.to_string()
    } else {
        format!("{subject}: {word}")
    }
}

/// The `&&` / `||` arm of a binary expression whose right side the walk
/// came up through.
fn and_or(
    parent: Node<'_>,
    child: Node<'_>,
    and: &str,
    or: &str,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    if !is_field(parent, "right", child) {
        return;
    }
    let op = parent
        .child_by_field_name("operator")
        .map(|op| text_of(op, source));
    let left = parent.child_by_field_name("left");
    if op == Some(and) {
        push(
            out,
            guard(GuardForm::And, left, false, None, None, None, source),
        );
    } else if op == Some(or) {
        push(
            out,
            guard(GuardForm::Or, left, true, None, None, None, source),
        );
    }
}

/// `if` / `else` through the `consequence` and `alternative` fields.
fn if_else(
    parent: Node<'_>,
    child: Node<'_>,
    form: GuardForm,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    let cond = parent.child_by_field_name("condition");
    let (then, other) = if form == GuardForm::Ternary {
        (GuardForm::Ternary, GuardForm::Ternary)
    } else {
        (GuardForm::If, GuardForm::Else)
    };
    if is_field(parent, "consequence", child) {
        push(out, guard(then, cond, false, None, None, None, source));
    } else if is_field(parent, "alternative", child) {
        push(out, guard(other, cond, true, None, None, None, source));
    }
}

fn on_error() -> Option<BranchGuard> {
    Some(BranchGuard {
        text: "on error".to_string(),
        negated: false,
        form: GuardForm::Catch,
        line: 0,
        branch: String::new(),
        arm_exit: None,
        exit: None,
    })
}

/// Shared shape of "an early exit before the site": the preceding statements
/// of the block that are an `if` with no else whose body always leaves.
#[allow(clippy::too_many_arguments)]
fn guards_before<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
    is_if: fn(Node<'_>) -> bool,
    has_else: fn(Node<'_>) -> bool,
    body: fn(Node<'t>) -> Option<Node<'t>>,
    always_exits: fn(Option<Node<'_>>) -> bool,
    condition: fn(Node<'t>) -> Option<Node<'t>>,
) {
    for statement in preceding_siblings(parent, child).into_iter().rev() {
        if !is_if(statement) || has_else(statement) {
            continue;
        }
        let arm = body(statement);
        if !always_exits(arm) {
            continue;
        }
        push(
            out,
            guard(
                GuardForm::Guard,
                condition(statement),
                true,
                None,
                Some(statement),
                exit_kind_or(arm, true, source),
                source,
            ),
        );
    }
}

fn consequence<'t>(node: Node<'t>) -> Option<Node<'t>> {
    node.child_by_field_name("consequence")
}

fn condition<'t>(node: Node<'t>) -> Option<Node<'t>> {
    node.child_by_field_name("condition")
}

fn has_alternative(node: Node<'_>) -> bool {
    node.child_by_field_name("alternative").is_some()
}

fn is_if_statement(node: Node<'_>) -> bool {
    node.kind() == "if_statement"
}

/// A statement that always leaves: one of `exits`, or a `block` ending in one.
fn always_exits(node: Option<Node<'_>>, exits: &[&str], block: &str) -> bool {
    let Some(node) = node else {
        return false;
    };
    if exits.contains(&node.kind()) {
        return true;
    }
    node.kind() == block && always_exits(last_named(node), exits, block)
}

// ----------------------------------------------------------------------- JS --

const JS_EXITS: &[&str] = &[
    "return_statement",
    "throw_statement",
    "break_statement",
    "continue_statement",
];

fn js_always_exits(node: Option<Node<'_>>) -> bool {
    always_exits(node, JS_EXITS, "statement_block")
}

static JS: Rules = Rules {
    boundaries: &[
        "function_declaration",
        "method_definition",
        "generator_function_declaration",
        "class_declaration",
        "class_body",
        "class",
        "program",
    ],
    inline_functions: &[
        "arrow_function",
        "function_expression",
        "function",
        "generator_function",
    ],
    binding_parents: &[
        "variable_declarator",
        "assignment_expression",
        "export_statement",
        "public_field_definition",
        "field_definition",
        "lexical_declaration",
    ],
    blocks: &[
        "statement_block",
        "program",
        "switch_case",
        "switch_default",
    ],
    enclosing: js_enclosing,
    early_exits: js_early_exits,
};

fn js_enclosing<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    match parent.kind() {
        "if_statement" => if_else(parent, child, GuardForm::If, source, out),
        "ternary_expression" => if_else(parent, child, GuardForm::Ternary, source, out),
        "switch_case" | "switch_default" => {
            // `child` is one of the case's body statements (not its value).
            if parent.kind() == "switch_case" && is_field(parent, "value", child) {
                return;
            }
            let statement = parent.parent().and_then(|body| body.parent());
            let subject_node = statement.and_then(|s| s.child_by_field_name("value"));
            let subject = cond_text(subject_node, source);
            if parent.kind() == "switch_default" {
                push(
                    out,
                    guard(
                        GuardForm::Case,
                        subject_node,
                        false,
                        Some(default_text(&subject, "default")),
                        statement,
                        None,
                        source,
                    ),
                );
            } else {
                let value_node = parent.child_by_field_name("value");
                let value = cond_text(value_node, source);
                push(
                    out,
                    guard(
                        GuardForm::Case,
                        value_node,
                        false,
                        Some(case_text(&subject, &value, "===")),
                        statement,
                        None,
                        source,
                    ),
                );
            }
        }
        "binary_expression" => and_or(parent, child, "&&", "||", source, out),
        "catch_clause" if !is_field(parent, "parameter", child) => push(out, on_error()),
        _ => {}
    }
}

fn js_early_exits<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    // Outer-most last (the list is reversed once at the end): walk the
    // preceding statements backwards so the FIRST guard in the source ends up
    // first in the final order.
    guards_before(
        parent,
        child,
        source,
        out,
        is_if_statement,
        has_alternative,
        consequence,
        js_always_exits,
        condition,
    );
}

// -------------------------------------------------------------------- Swift --

fn swift_always_exits(node: Option<Node<'_>>) -> bool {
    always_exits(node, &["control_transfer_statement"], "statements")
}

/// For a Swift `if`: is `child` after the `else` keyword?
fn after_else(parent: Node<'_>, child: Node<'_>) -> bool {
    let mut seen_else = false;
    for node in children(parent) {
        if node.id() == child.id() {
            return seen_else;
        }
        if node.kind() == "else" {
            seen_else = true;
        }
    }
    false
}

/// All `condition` fields of a Swift `if`/`guard`, as one text — `if let x,
/// y > 0`. The grammar labels several tokens of `if let x = y, z > 0` as
/// `condition` (the binding's own pieces included), so the readable text is
/// the SPAN from the first to the last of them, not the pieces joined.
fn swift_conditions<'t>(node: Node<'t>, source: &str) -> (Option<Node<'t>>, String) {
    let parts: Vec<Node<'t>> = (0..node.child_count())
        .filter(|&index| node.field_name_for_child(index) == Some("condition"))
        .filter_map(|index| node.child(index))
        .collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return (None, String::new());
    };
    let raw = source
        .get(first.start_byte()..last.end_byte())
        .unwrap_or("");
    let text = collapse(raw);
    if matches!(text.as_str(), "let" | "var" | "case" | "try" | "await") {
        return (None, String::new());
    }
    (Some(*first), cut(&text, MAX_TEXT))
}

fn swift_statements<'t>(node: Node<'t>) -> Option<Node<'t>> {
    named_children(node)
        .into_iter()
        .find(|child| child.kind() == "statements")
}

static SWIFT: Rules = Rules {
    boundaries: &[
        "function_declaration",
        "init_declaration",
        "deinit_declaration",
        "class_declaration",
        "protocol_declaration",
        "computed_property",
        "source_file",
    ],
    inline_functions: &["lambda_literal"],
    binding_parents: &["property_declaration", "assignment"],
    blocks: &["statements", "function_body"],
    enclosing: swift_enclosing,
    early_exits: swift_early_exits,
};

fn swift_enclosing<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    match parent.kind() {
        "if_statement" => {
            if field_of(parent, child) == Some("condition") {
                return;
            }
            let (cond, text) = swift_conditions(parent, source);
            let otherwise = after_else(parent, child);
            let form = if otherwise {
                GuardForm::Else
            } else {
                GuardForm::If
            };
            push(
                out,
                guard(form, cond, otherwise, Some(text), None, None, source),
            );
        }
        "guard_statement" => {
            // Inside the guard's body the condition FAILED.
            if field_of(parent, child) == Some("condition") {
                return;
            }
            let (cond, text) = swift_conditions(parent, source);
            push(
                out,
                guard(GuardForm::Else, cond, true, Some(text), None, None, source),
            );
        }
        "ternary_expression" => {
            let cond = parent.child_by_field_name("condition");
            if is_field(parent, "if_true", child) {
                push(
                    out,
                    guard(GuardForm::Ternary, cond, false, None, None, None, source),
                );
            } else if is_field(parent, "if_false", child) {
                push(
                    out,
                    guard(GuardForm::Ternary, cond, true, None, None, None, source),
                );
            }
        }
        "switch_entry" => {
            let statement = parent.parent();
            let subject = cond_text(
                statement.and_then(|s| s.child_by_field_name("expr")),
                source,
            );
            let pattern = named_children(parent)
                .into_iter()
                .find(|node| node.kind() == "switch_pattern");
            if pattern.is_some_and(|pattern| pattern.id() == child.id()) {
                return;
            }
            let is_default = children(parent)
                .iter()
                .any(|node| node.kind() == "default_keyword");
            let value = pattern
                .map(|p| cond_text(Some(p), source))
                .unwrap_or_default();
            let text = if is_default {
                default_text(&subject, "default")
            } else {
                case_text(&subject, &value, "==")
            };
            let cond = pattern.or_else(|| statement.and_then(|s| s.child_by_field_name("expr")));
            push(
                out,
                guard(
                    GuardForm::Case,
                    cond,
                    false,
                    Some(text),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "catch_block" => push(out, on_error()),
        _ => {}
    }
}

fn swift_early_exits<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    for statement in preceding_siblings(parent, child).into_iter().rev() {
        if statement.kind() == "guard_statement" {
            let (cond, text) = swift_conditions(statement, source);
            let body = swift_statements(statement);
            push(
                out,
                guard(
                    GuardForm::Guard,
                    cond,
                    false,
                    Some(text),
                    Some(statement),
                    exit_kind_or(body, true, source),
                    source,
                ),
            );
        } else if statement.kind() == "if_statement"
            && !children(statement).iter().any(|node| node.kind() == "else")
        {
            let body = swift_statements(statement);
            if !swift_always_exits(body) {
                continue;
            }
            let (cond, text) = swift_conditions(statement, source);
            push(
                out,
                guard(
                    GuardForm::Guard,
                    cond,
                    true,
                    Some(text),
                    Some(statement),
                    exit_kind_or(body, true, source),
                    source,
                ),
            );
        }
    }
}

// ------------------------------------------------------------------- Python --

const PY_EXITS: &[&str] = &[
    "return_statement",
    "raise_statement",
    "break_statement",
    "continue_statement",
];

fn py_always_exits(node: Option<Node<'_>>) -> bool {
    always_exits(node, PY_EXITS, "block")
}

fn py_operator<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    if let Some(op) = node.child_by_field_name("operator") {
        return text_of(op, source);
    }
    children(node)
        .into_iter()
        .find(|child| !child.is_named() && matches!(text_of(*child, source), "and" | "or"))
        .map_or("", |child| text_of(child, source))
}

static PYTHON: Rules = Rules {
    boundaries: &["function_definition", "class_definition", "module"],
    inline_functions: &["lambda"],
    binding_parents: &["assignment", "augmented_assignment"],
    blocks: &["block", "module"],
    enclosing: python_enclosing,
    early_exits: python_early_exits,
};

fn python_enclosing<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    match parent.kind() {
        "if_statement" => {
            let cond = parent.child_by_field_name("condition");
            if is_field(parent, "consequence", child) {
                push(
                    out,
                    guard(GuardForm::If, cond, false, None, None, None, source),
                );
            } else if matches!(child.kind(), "elif_clause" | "else_clause") {
                // An elif/else arm runs when the `if` and every earlier elif
                // failed.
                push(
                    out,
                    guard(GuardForm::Else, cond, true, None, None, None, source),
                );
                for sibling in preceding_siblings(parent, child) {
                    if sibling.kind() == "elif_clause" {
                        push(
                            out,
                            guard(
                                GuardForm::Else,
                                sibling.child_by_field_name("condition"),
                                true,
                                None,
                                None,
                                None,
                                source,
                            ),
                        );
                    }
                }
            }
        }
        "elif_clause" => {
            if is_field(parent, "consequence", child) {
                push(
                    out,
                    guard(
                        GuardForm::If,
                        parent.child_by_field_name("condition"),
                        false,
                        None,
                        None,
                        None,
                        source,
                    ),
                );
            }
        }
        "conditional_expression" => {
            // `a if cond else b`: children are [a, cond, b].
            let kids = named_children(parent);
            if kids.len() < 3 {
                return;
            }
            if child.id() == kids[0].id() {
                push(
                    out,
                    guard(
                        GuardForm::Ternary,
                        Some(kids[1]),
                        false,
                        None,
                        None,
                        None,
                        source,
                    ),
                );
            } else if child.id() == kids[2].id() {
                push(
                    out,
                    guard(
                        GuardForm::Ternary,
                        Some(kids[1]),
                        true,
                        None,
                        None,
                        None,
                        source,
                    ),
                );
            }
        }
        "case_clause" => {
            if !is_field(parent, "consequence", child) {
                return;
            }
            let statement = parent.parent().and_then(|block| block.parent());
            let subject = cond_text(
                statement.and_then(|s| s.child_by_field_name("subject")),
                source,
            );
            let pattern = named_children(parent)
                .into_iter()
                .find(|node| node.kind() == "case_pattern");
            let value = pattern
                .map(|p| cond_text(Some(p), source))
                .unwrap_or_default();
            let text = if value == "_" || value.is_empty() {
                default_text(&subject, "default")
            } else {
                case_text(&subject, &value, "==")
            };
            push(
                out,
                guard(
                    GuardForm::Case,
                    pattern,
                    false,
                    Some(text),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "boolean_operator" => {
            if !is_field(parent, "right", child) {
                return;
            }
            let left = parent.child_by_field_name("left");
            match py_operator(parent, source) {
                "and" => push(
                    out,
                    guard(GuardForm::And, left, false, None, None, None, source),
                ),
                "or" => push(
                    out,
                    guard(GuardForm::Or, left, true, None, None, None, source),
                ),
                _ => {}
            }
        }
        "except_clause" | "except_group_clause" if child.kind() == "block" => {
            push(out, on_error());
        }
        _ => {}
    }
}

fn python_early_exits<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    guards_before(
        parent,
        child,
        source,
        out,
        is_if_statement,
        |s| {
            named_children(s)
                .iter()
                .any(|n| matches!(n.kind(), "elif_clause" | "else_clause"))
        },
        consequence,
        py_always_exits,
        condition,
    );
}

// --------------------------------------------------------------------- Java --

const JAVA_EXITS: &[&str] = &[
    "return_statement",
    "throw_statement",
    "break_statement",
    "continue_statement",
    "yield_statement",
];

fn java_always_exits(node: Option<Node<'_>>) -> bool {
    always_exits(node, JAVA_EXITS, "block")
}

/// `case A:` / `case A ->` / `default` labels of a Java switch group or
/// rule, as one condition text.
fn java_case_text(labels: &[Node<'_>], subject: &str, source: &str) -> String {
    let values: Vec<String> = labels
        .iter()
        .map(|label| {
            named_children(*label)
                .into_iter()
                .map(|value| cond_text(Some(value), source))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|text| !text.is_empty())
        .collect();
    if values.is_empty() {
        return default_text(subject, "default");
    }
    case_text(subject, &values.join(", "), "==")
}

static JAVA: Rules = Rules {
    boundaries: &[
        "method_declaration",
        "constructor_declaration",
        "class_declaration",
        "class_body",
        "interface_declaration",
        "enum_declaration",
        "record_declaration",
        "program",
    ],
    inline_functions: &["lambda_expression"],
    binding_parents: &[
        "variable_declarator",
        "assignment_expression",
        "field_declaration",
    ],
    blocks: &[
        "block",
        "switch_block_statement_group",
        "program",
        "constructor_body",
    ],
    enclosing: java_enclosing,
    early_exits: java_early_exits,
};

fn java_enclosing<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    match parent.kind() {
        "if_statement" => if_else(parent, child, GuardForm::If, source, out),
        "ternary_expression" => if_else(parent, child, GuardForm::Ternary, source, out),
        "switch_block_statement_group" | "switch_rule" => {
            if child.kind() == "switch_label" {
                return;
            }
            let statement = parent.parent().and_then(|block| block.parent());
            let subject = cond_text(
                statement.and_then(|s| s.child_by_field_name("condition")),
                source,
            );
            let labels: Vec<Node<'t>> = named_children(parent)
                .into_iter()
                .filter(|node| node.kind() == "switch_label")
                .collect();
            let text = java_case_text(&labels, &subject, source);
            push(
                out,
                guard(
                    GuardForm::Case,
                    labels.first().copied(),
                    false,
                    Some(text),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "binary_expression" => and_or(parent, child, "&&", "||", source, out),
        "catch_clause" if is_field(parent, "body", child) => push(out, on_error()),
        _ => {}
    }
}

fn java_early_exits<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    guards_before(
        parent,
        child,
        source,
        out,
        is_if_statement,
        has_alternative,
        consequence,
        java_always_exits,
        condition,
    );
}

// ------------------------------------------------------------------- Kotlin --
//
// Upstream's rules read fwcd's tree-sitter-kotlin; this port's grammar is
// tree-sitter-kotlin-ng, whose tree differs where the rules look: a function
// body is a `block` (no `statements`), an `if` arm and a `when` branch are
// the expression itself (no `control_structure_body`), a `when` condition is
// a `condition` field (no `when_condition`), `&&` / `||` are a
// `binary_expression`, `return` is a `return_expression`, and a bare `break`
// or `continue` is an identifier. The rules below read that tree for the
// same labels.

fn is_kotlin_jump_identifier(node: Node<'_>, source: &str) -> bool {
    node.kind() == "identifier" && matches!(text_of(node, source), "break" | "continue")
}

fn kt_always_exits(node: Option<Node<'_>>, source: &str) -> bool {
    let Some(node) = node else {
        return false;
    };
    if matches!(
        node.kind(),
        "return_expression" | "throw_expression" | "jump_expression"
    ) || is_kotlin_jump_identifier(node, source)
    {
        return true;
    }
    node.kind() == "block" && kt_always_exits(last_named(node), source)
}

/// The arms of a Kotlin `if`: the then arm, and the else arm after the
/// `else` keyword — its named children other than the condition and
/// comments.
fn kt_arms(if_expression: Node<'_>) -> (Option<Node<'_>>, Option<Node<'_>>) {
    let condition = if_expression.child_by_field_name("condition");
    let mut then = None;
    let mut otherwise = None;
    let mut seen_else = false;
    for node in children(if_expression) {
        if !node.is_named() {
            seen_else |= node.kind() == "else";
            continue;
        }
        if condition.is_some_and(|condition| condition.id() == node.id())
            || node.kind().ends_with("comment")
        {
            continue;
        }
        if seen_else {
            otherwise = otherwise.or(Some(node));
        } else {
            then = then.or(Some(node));
        }
    }
    (then, otherwise)
}

static KOTLIN: Rules = Rules {
    boundaries: &[
        "function_declaration",
        "secondary_constructor",
        "class_declaration",
        "class_body",
        "object_declaration",
        "getter",
        "setter",
        "source_file",
    ],
    inline_functions: &["lambda_literal", "anonymous_function"],
    binding_parents: &["property_declaration", "assignment"],
    blocks: &["block", "source_file"],
    enclosing: kotlin_enclosing,
    early_exits: kotlin_early_exits,
};

fn kotlin_enclosing<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    match parent.kind() {
        "if_expression" => {
            let cond = parent.child_by_field_name("condition");
            if cond.is_some_and(|cond| cond.id() == child.id()) {
                return;
            }
            let (then, otherwise) = kt_arms(parent);
            if then.is_some_and(|arm| arm.id() == child.id()) {
                push(
                    out,
                    guard(GuardForm::If, cond, false, None, None, None, source),
                );
            } else if otherwise.is_some_and(|arm| arm.id() == child.id()) {
                push(
                    out,
                    guard(GuardForm::Else, cond, true, None, None, None, source),
                );
            }
        }
        "when_entry" => {
            // The branch's body, not one of its conditions.
            if !child.is_named() || field_of(parent, child) == Some("condition") {
                return;
            }
            let when = parent.parent();
            let subject_node = when.and_then(|when| {
                named_children(when)
                    .into_iter()
                    .find(|node| node.kind() == "when_subject")
            });
            let subject = strip_outer_parens(cond_text(subject_node, source));
            let conditions: Vec<Node<'t>> = (0..parent.child_count())
                .filter(|&index| parent.field_name_for_child(index) == Some("condition"))
                .filter_map(|index| parent.child(index))
                .collect();
            if conditions.is_empty() {
                push(
                    out,
                    guard(
                        GuardForm::Case,
                        None,
                        false,
                        Some(default_text(&subject, "else")),
                        when,
                        None,
                        source,
                    ),
                );
            } else {
                let value = conditions
                    .iter()
                    .map(|cond| cond_text(Some(*cond), source))
                    .collect::<Vec<_>>()
                    .join(", ");
                push(
                    out,
                    guard(
                        GuardForm::Case,
                        conditions.first().copied(),
                        false,
                        Some(case_text(&subject, &value, "==")),
                        when,
                        None,
                        source,
                    ),
                );
            }
        }
        "binary_expression" => and_or(parent, child, "&&", "||", source, out),
        "catch_block" if child.kind() == "block" => push(out, on_error()),
        _ => {}
    }
}

/// `.replace(/^\((.*)\)$/, '$1')`.
fn strip_outer_parens(text: String) -> String {
    match text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        Some(inner) => inner.to_string(),
        None => text,
    }
}

fn kotlin_early_exits<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    for statement in preceding_siblings(parent, child).into_iter().rev() {
        if statement.kind() != "if_expression" {
            continue;
        }
        let (then, otherwise) = kt_arms(statement);
        if otherwise.is_some() || !kt_always_exits(then, source) {
            continue;
        }
        push(
            out,
            guard(
                GuardForm::Guard,
                statement.child_by_field_name("condition"),
                true,
                None,
                Some(statement),
                exit_kind_or(then, true, source),
                source,
            ),
        );
    }
}

// ----------------------------------------------------------------------- C# --

const CS_EXITS: &[&str] = &[
    "return_statement",
    "throw_statement",
    "break_statement",
    "continue_statement",
];

fn cs_always_exits(node: Option<Node<'_>>) -> bool {
    always_exits(node, CS_EXITS, "block")
}

static CSHARP: Rules = Rules {
    boundaries: &[
        "method_declaration",
        "constructor_declaration",
        "local_function_statement",
        "class_declaration",
        "struct_declaration",
        "record_declaration",
        "interface_declaration",
        "declaration_list",
        "property_declaration",
        "accessor_declaration",
        "compilation_unit",
    ],
    inline_functions: &["lambda_expression", "anonymous_method_expression"],
    binding_parents: &[
        "variable_declarator",
        "assignment_expression",
        "equals_value_clause",
    ],
    blocks: &["block", "switch_section", "compilation_unit"],
    enclosing: csharp_enclosing,
    early_exits: csharp_early_exits,
};

fn is_csharp_label(node: Node<'_>) -> bool {
    node.kind().ends_with("pattern") || node.kind().ends_with("switch_label")
}

fn csharp_enclosing<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    match parent.kind() {
        "if_statement" => if_else(parent, child, GuardForm::If, source, out),
        "conditional_expression" => if_else(parent, child, GuardForm::Ternary, source, out),
        "switch_section" => {
            if is_csharp_label(child) {
                return;
            }
            let statement = parent.parent().and_then(|body| body.parent());
            let subject = cond_text(
                statement.and_then(|s| s.child_by_field_name("value")),
                source,
            );
            let labels: Vec<Node<'t>> = named_children(parent)
                .into_iter()
                .filter(|node| is_csharp_label(*node))
                .collect();
            let value = labels
                .iter()
                .map(|label| cond_text(Some(*label), source))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            let text = if value.is_empty() {
                default_text(&subject, "default")
            } else {
                case_text(&subject, &value, "==")
            };
            push(
                out,
                guard(
                    GuardForm::Case,
                    labels.first().copied(),
                    false,
                    Some(text),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "switch_expression_arm" => {
            if !is_field(parent, "expression", child) {
                return;
            }
            let statement = parent.parent();
            let subject = cond_text(
                statement.and_then(|s| s.child_by_field_name("value")),
                source,
            );
            let pattern = parent.child_by_field_name("pattern");
            let value = cond_text(pattern, source);
            let text = if value == "_" || value.is_empty() {
                default_text(&subject, "default")
            } else {
                case_text(&subject, &value, "==")
            };
            push(
                out,
                guard(
                    GuardForm::Case,
                    pattern,
                    false,
                    Some(text),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "binary_expression" => and_or(parent, child, "&&", "||", source, out),
        "catch_clause" if is_field(parent, "body", child) => push(out, on_error()),
        _ => {}
    }
}

fn csharp_early_exits<'t>(
    parent: Node<'t>,
    child: Node<'t>,
    source: &str,
    out: &mut Vec<BranchGuard>,
) {
    guards_before(
        parent,
        child,
        source,
        out,
        is_if_statement,
        has_alternative,
        consequence,
        cs_always_exits,
        condition,
    );
}

// ----------------------------------------------------------------------- Go --

const GO_EXITS: &[&str] = &[
    "return_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
];

/// The function a statement-level call names (`panic`, `os.Exit`), as
/// written.
fn called_function<'s>(statement: Node<'_>, source: &'s str) -> &'s str {
    statement
        .named_child(0)
        .filter(|call| call.kind() == "call_expression")
        .and_then(|call| call.child_by_field_name("function"))
        .map_or("", |function| text_of(function, source))
}

fn go_always_exits(node: Option<Node<'_>>, source: &str) -> bool {
    let Some(node) = node else {
        return false;
    };
    if GO_EXITS.contains(&node.kind()) {
        return true;
    }
    match node.kind() {
        // tree-sitter-go 0.25 puts a block's statements in a `statement_list`.
        "block" | "statement_list" => go_always_exits(last_named(node), source),
        "expression_statement" => matches!(
            called_function(node, source),
            "panic" | "os.Exit" | "log.Fatal" | "log.Fatalf" | "log.Fatalln"
        ),
        _ => false,
    }
}

static GO: Rules = Rules {
    boundaries: &["function_declaration", "method_declaration", "source_file"],
    inline_functions: &["func_literal"],
    binding_parents: &[
        "short_var_declaration",
        "var_spec",
        "assignment_statement",
        "const_spec",
    ],
    // tree-sitter-go 0.25 holds the statements of a block and of a case in a
    // `statement_list`, where upstream's grammar held them directly.
    blocks: &[
        "block",
        "statement_list",
        "expression_case",
        "default_case",
        "type_case",
        "communication_case",
        "source_file",
    ],
    enclosing: go_enclosing,
    early_exits: go_early_exits,
};

fn go_enclosing<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    match parent.kind() {
        "if_statement" => if_else(parent, child, GuardForm::If, source, out),
        "expression_case" | "type_case" | "communication_case" => {
            let value = parent
                .child_by_field_name("value")
                .or_else(|| parent.child_by_field_name("type"))
                .or_else(|| parent.child_by_field_name("communication"));
            if value.is_some_and(|value| value.id() == child.id()) {
                return;
            }
            let statement = parent.parent();
            let subject = cond_text(
                statement.and_then(|s| s.child_by_field_name("value")),
                source,
            );
            let v = cond_text(value, source);
            let text = if parent.kind() == "communication_case" {
                v
            } else {
                case_text(&subject, &v, "==")
            };
            push(
                out,
                guard(
                    GuardForm::Case,
                    value,
                    false,
                    Some(text),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "default_case" => {
            let statement = parent.parent();
            let subject_node = statement.and_then(|s| s.child_by_field_name("value"));
            let subject = cond_text(subject_node, source);
            push(
                out,
                guard(
                    GuardForm::Case,
                    subject_node,
                    false,
                    Some(default_text(&subject, "default")),
                    statement,
                    None,
                    source,
                ),
            );
        }
        "binary_expression" => and_or(parent, child, "&&", "||", source, out),
        _ => {}
    }
}

fn go_early_exits<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    for statement in preceding_siblings(parent, child).into_iter().rev() {
        if statement.kind() != "if_statement" || has_alternative(statement) {
            continue;
        }
        let arm = consequence(statement);
        if !go_always_exits(arm, source) {
            continue;
        }
        push(
            out,
            guard(
                GuardForm::Guard,
                condition(statement),
                true,
                None,
                Some(statement),
                exit_kind_or(arm, true, source),
                source,
            ),
        );
    }
}

// -------------------------------------------------------------------- C/C++ --

const C_EXITS: &[&str] = &[
    "return_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
    "throw_statement",
];

fn c_always_exits(node: Option<Node<'_>>, source: &str) -> bool {
    let Some(node) = node else {
        return false;
    };
    if C_EXITS.contains(&node.kind()) {
        return true;
    }
    match node.kind() {
        "compound_statement" => c_always_exits(last_named(node), source),
        "expression_statement" => matches!(
            called_function(node, source),
            "exit" | "_exit" | "abort" | "longjmp"
        ),
        _ => false,
    }
}

static C: Rules = Rules {
    boundaries: &[
        "function_definition",
        "class_specifier",
        "struct_specifier",
        "namespace_definition",
        "translation_unit",
        "field_declaration_list",
    ],
    inline_functions: &["lambda_expression"],
    binding_parents: &["init_declarator", "assignment_expression"],
    blocks: &["compound_statement", "case_statement", "translation_unit"],
    enclosing: c_enclosing,
    early_exits: c_early_exits,
};

fn c_enclosing<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    match parent.kind() {
        "if_statement" => {
            let cond = parent.child_by_field_name("condition");
            if is_field(parent, "consequence", child) {
                push(
                    out,
                    guard(GuardForm::If, cond, false, None, None, None, source),
                );
            } else if is_field(parent, "alternative", child) || child.kind() == "else_clause" {
                push(
                    out,
                    guard(GuardForm::Else, cond, true, None, None, None, source),
                );
            }
        }
        "conditional_expression" => if_else(parent, child, GuardForm::Ternary, source, out),
        "case_statement" => {
            let value = parent.child_by_field_name("value");
            if value.is_some_and(|value| value.id() == child.id()) {
                return;
            }
            let statement = parent.parent().and_then(|body| body.parent());
            let subject_node = statement.and_then(|s| s.child_by_field_name("condition"));
            let subject = cond_text(subject_node, source);
            match value {
                None => push(
                    out,
                    guard(
                        GuardForm::Case,
                        subject_node,
                        false,
                        Some(default_text(&subject, "default")),
                        statement,
                        None,
                        source,
                    ),
                ),
                Some(value) => {
                    let v = cond_text(Some(value), source);
                    push(
                        out,
                        guard(
                            GuardForm::Case,
                            Some(value),
                            false,
                            Some(case_text(&subject, &v, "==")),
                            statement,
                            None,
                            source,
                        ),
                    );
                }
            }
        }
        "binary_expression" => and_or(parent, child, "&&", "||", source, out),
        "catch_clause" if is_field(parent, "body", child) => push(out, on_error()),
        _ => {}
    }
}

fn c_early_exits<'t>(parent: Node<'t>, child: Node<'t>, source: &str, out: &mut Vec<BranchGuard>) {
    for statement in preceding_siblings(parent, child).into_iter().rev() {
        if statement.kind() != "if_statement"
            || has_alternative(statement)
            || named_children(statement)
                .iter()
                .any(|node| node.kind() == "else_clause")
        {
            continue;
        }
        let arm = consequence(statement);
        if !c_always_exits(arm, source) {
            continue;
        }
        push(
            out,
            guard(
                GuardForm::Guard,
                condition(statement),
                true,
                None,
                Some(statement),
                exit_kind_or(arm, true, source),
                source,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    //! Ported from upstream `__tests__/branch-guards.test.ts` and
    //! `__tests__/branch-guards-languages.test.ts` (`b635dd46`), their guard
    //! cases; the expected labels are upstream's.
    use super::*;

    /// Line (1-based) of the first line containing `needle`.
    fn line_of(src: &str, needle: &str) -> usize {
        src.split('\n')
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line contains {needle}"))
            + 1
    }

    fn guards_at_needle(src: &str, needle: &str, language: Language) -> Vec<BranchGuard> {
        let line = line_of(src, needle);
        let column = src.split('\n').nth(line - 1).unwrap().find(needle).unwrap();
        guards_in_source(src, language, line, Some(column))
    }

    fn label_at(src: &str, needle: &str, language: Language) -> String {
        guard_label(&guards_at_needle(src, needle, language))
    }

    fn tsx(src: &str, needle: &str) -> String {
        label_at(src, needle, Language::Tsx)
    }

    fn swift(src: &str, needle: &str) -> String {
        label_at(src, needle, Language::Swift)
    }

    fn is_branch_key(key: &str) -> bool {
        key.split_once(':').is_some_and(|(line, column)| {
            !line.is_empty()
                && !column.is_empty()
                && line.bytes().all(|b| b.is_ascii_digit())
                && column.bytes().all(|b| b.is_ascii_digit())
        })
    }

    const HANDLE_PRESS: &str = "
export function ItemCard(props) {
  const handlePress = useCallback(() => {
    if (isUploading) return
    if (isCollected) {
      openObjectDetail(item, folderName)
      return
    }
    if (queueHasItems) {
      handleAddToQueue()
      return
    }
    handleStartCapture()
  }, [])
  return null
}
";

    #[test]
    fn js_reads_an_if_branch_and_the_early_return_guards_before_it() {
        assert_eq!(
            tsx(HANDLE_PRESS, "openObjectDetail("),
            "!isUploading && isCollected"
        );
    }

    #[test]
    fn js_keeps_a_disjunctive_guard_in_parentheses() {
        let src = "
function go(object) {
  if (isUploading) return
  if (!object?.id || !object?.name) {
    bail()
    return
  }
  proceed()
}
";
        assert_eq!(
            tsx(src, "bail("),
            "!isUploading && (!object?.id || !object?.name)"
        );
        assert_eq!(
            tsx(src, "proceed("),
            "!isUploading && !(!object?.id || !object?.name)"
        );
    }

    #[test]
    fn js_turns_each_earlier_early_return_into_a_negated_guard_in_source_order() {
        assert_eq!(
            tsx(HANDLE_PRESS, "handleAddToQueue("),
            "!isUploading && !isCollected && queueHasItems"
        );
        assert_eq!(
            tsx(HANDLE_PRESS, "handleStartCapture("),
            "!isUploading && !isCollected && !queueHasItems"
        );
    }

    #[test]
    fn js_does_not_climb_past_a_declared_or_assigned_function() {
        let src = "
function outer() {
  if (outerCond) {
    const cb = () => {
      if (inner) run()
    }
    function named() { if (deep) walk() }
  }
}";
        assert_eq!(tsx(src, "run()"), "inner");
        assert_eq!(tsx(src, "walk()"), "deep");
    }

    #[test]
    fn js_an_inline_callback_inherits_the_conditions_its_definition_sits_under() {
        let src = "
function verify(total) {
  if (selectedHasBarcode) {
    if (total > 1) {
      return { proceed: () => router.navigate('/barcode-matches') }
    }
    return { ok: true, proceed: () => captureObject(item) }
  }
  list.forEach((x) => { if (x.ok) keep(x) })
}";
        assert_eq!(
            tsx(src, "captureObject(item)"),
            "selectedHasBarcode && !(total > 1)"
        );
        assert_eq!(
            tsx(src, "router.navigate("),
            "selectedHasBarcode && total > 1"
        );
        assert_eq!(tsx(src, "keep(x)"), "!selectedHasBarcode && x.ok");
    }

    #[test]
    fn js_reads_else_else_if_and_the_arms_of_a_ternary() {
        let src = "
function f() {
  if (a) { one() } else if (b) { two() } else { three() }
  const x = ready ? go() : wait()
}";
        assert_eq!(tsx(src, "one()"), "a");
        assert_eq!(tsx(src, "two()"), "!a && b");
        assert_eq!(tsx(src, "three()"), "!a && !b");
        assert_eq!(tsx(src, "go()"), "ready");
        assert_eq!(tsx(src, "wait()"), "!ready");
    }

    #[test]
    fn js_reads_switch_cases_short_circuits_and_catch() {
        let src = "
function f() {
  switch (mode) {
    case 'verify': scan(); break
    default: capture()
  }
  ok && fire()
  ok || fallback()
  try { risky() } catch (e) { report(e) }
}";
        assert_eq!(tsx(src, "scan()"), "mode === 'verify'");
        assert_eq!(tsx(src, "capture()"), "mode: default");
        assert_eq!(tsx(src, "fire()"), "ok");
        assert_eq!(tsx(src, "fallback()"), "!ok");
        assert_eq!(tsx(src, "report(e)"), "on error");
        assert_eq!(tsx(src, "risky()"), "");
    }

    #[test]
    fn js_negates_readably() {
        let src = "
function f() {
  if (!ready) return
  if (a && b) { } else { alt() }
  if (count > 0) go()
  if (options?.verify !== false && (item.barcodes?.length ?? 0) > 0) verify()
}";
        assert_eq!(tsx(src, "alt()"), "ready && !(a && b)");
        assert_eq!(tsx(src, "go()"), "ready && count > 0");
        assert_eq!(
            tsx(src, "verify()"),
            "ready && options?.verify !== false && (item.barcodes?.length ?? 0) > 0"
        );
    }

    #[test]
    fn js_a_call_inside_a_condition_is_not_guarded_by_it() {
        let src = "
function f() {
  if (isReady()) run()
}";
        assert_eq!(tsx(src, "isReady()"), "");
        assert_eq!(tsx(src, "run()"), "isReady()");
    }

    #[test]
    fn js_an_if_whose_body_does_not_always_exit_is_not_a_guard() {
        let src = "
function f() {
  if (x) { log() }
  go()
}";
        assert_eq!(tsx(src, "go()"), "");
    }

    #[test]
    fn js_caps_a_very_long_condition() {
        let cond = "a".repeat(120);
        let src = format!("function f() {{\n  if ({cond}) go()\n}}");
        let label = tsx(&src, "go()");
        assert!(label.encode_utf16().count() < 90, "{label}");
        assert!(label.ends_with('…'), "{label}");
    }

    #[test]
    fn swift_reads_guard_if_else_ternary_and_switch() {
        let src = "
func decide() {
  guard ready else { bail(); return }
  if isCollected { open() } else if other { two() } else { close() }
  let x = flag ? a() : b()
  switch mode { case .verify: scan() default: capture() }
}";
        assert_eq!(swift(src, "bail()"), "!ready");
        assert_eq!(swift(src, "open()"), "ready && isCollected");
        assert_eq!(swift(src, "two()"), "ready && !isCollected && other");
        assert_eq!(swift(src, "close()"), "ready && !isCollected && !other");
        assert_eq!(swift(src, "a()"), "ready && flag");
        assert_eq!(swift(src, "b()"), "ready && !flag");
        assert_eq!(swift(src, "scan()"), "ready && mode == .verify");
        assert_eq!(swift(src, "capture()"), "ready && mode: default");
    }

    #[test]
    fn swift_joins_multi_clause_conditions_and_treats_an_early_return_as_a_guard() {
        let src = "
func f() {
  if let item = current, item.count > 0 { use(item) }
  if busy { return }
  go()
}";
        assert_eq!(
            swift(src, "use(item)"),
            "let item = current, item.count > 0"
        );
        assert_eq!(swift(src, "go()"), "!busy");
    }

    const IF_ELSE: &str = "
export async function authUser(req, res) {
  const user = await User.findOne({ email })
  if (user && (await user.matchPassword(password))) {
    res.json({ token: generateToken(user._id) })
  } else {
    res.status(401)
    throw new Error('Invalid email or password')
  }
}";

    #[test]
    fn an_if_and_its_else_share_a_branch_with_negated_flipped() {
        let yes = guards_at_needle(IF_ELSE, "res.json", Language::Tsx);
        let no = guards_at_needle(IF_ELSE, "res.status", Language::Tsx);
        assert_eq!(yes.len(), 1, "{yes:?}");
        assert_eq!(no.len(), 1, "{no:?}");
        assert_eq!(yes[0].text, no[0].text);
        assert!(!yes[0].negated);
        assert!(no[0].negated);
        // The identity of the FORK, not of the arm: both arms of one `if`.
        assert_eq!(yes[0].branch, no[0].branch);
        assert!(is_branch_key(&yes[0].branch), "{:?}", yes[0].branch);
        // The else arm ends by throwing; the then arm runs on.
        assert_eq!(no[0].arm_exit, Some(GuardExit::Throw));
        assert_eq!(yes[0].arm_exit, None);
    }

    #[test]
    fn an_early_exit_and_the_code_it_guards_share_a_branch() {
        let src = "
export async function createReview(req, res) {
  const product = await Product.findById(req.params.id)
  if (!product) {
    res.status(404)
    throw new Error('Product not found')
  }
  await product.save()
}";
        let inside = guards_at_needle(src, "res.status", Language::Tsx);
        let after = guards_at_needle(src, "product.save", Language::Tsx);
        assert_eq!(inside.len(), 1, "{inside:?}");
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(inside[0].branch, after[0].branch);
        assert!(!inside[0].negated);
        assert!(after[0].negated);
        // The arm NOT taken throws — what a rail draws as the fork's terminal.
        assert_eq!(after[0].form, GuardForm::Guard);
        assert_eq!(after[0].exit, Some(GuardExit::Throw));
        assert_eq!(inside[0].arm_exit, Some(GuardExit::Throw));
    }

    #[test]
    fn every_case_of_one_switch_shares_a_branch() {
        let src = "
export function route(kind) {
  switch (kind) {
    case 'a':
      first()
      break
    case 'b':
      second()
      break
    default:
      other()
  }
}";
        let a = guards_at_needle(src, "first()", Language::Tsx);
        let b = guards_at_needle(src, "second()", Language::Tsx);
        let d = guards_at_needle(src, "other()", Language::Tsx);
        assert_eq!(a[0].branch, b[0].branch);
        assert_eq!(a[0].branch, d[0].branch);
        assert_eq!(
            [a[0].text.as_str(), b[0].text.as_str(), d[0].text.as_str()],
            ["kind === 'a'", "kind === 'b'", "kind: default"]
        );
    }

    #[test]
    fn two_try_catch_blocks_have_branches_of_their_own() {
        let src = "
export async function save() {
  try { await a() } catch (e) { first(e) }
  try { await b() } catch (e) { second(e) }
}";
        let one = guards_at_needle(src, "first(e)", Language::Tsx);
        let two = guards_at_needle(src, "second(e)", Language::Tsx);
        assert_eq!(one[0].text, "on error");
        assert_eq!(two[0].text, "on error");
        assert_ne!(one[0].branch, two[0].branch);
    }

    #[test]
    fn a_later_elif_that_raises_does_not_make_an_arm_an_exit() {
        let src = "
def handler(user):
    if not user:
        raise HTTPException(400)
    elif not user.is_active:
        raise HTTPException(400)
    go(user)
";
        // The `elif` arm raises; the arm it is written in runs on to `go(user)`.
        let after = guards_in_source(src, Language::Python, line_of(src, "go(user)"), Some(4));
        assert!(
            after.iter().all(|guard| guard.arm_exit.is_none()),
            "{after:?}"
        );
    }

    #[test]
    fn a_swift_guard_reads_as_an_exit() {
        let src = "
func load() {
  guard let user = current else { return }
  fetch(user)
}";
        let after = guards_at_needle(src, "fetch(user)", Language::Swift);
        assert_eq!(after[0].form, GuardForm::Guard);
        assert_eq!(after[0].exit, Some(GuardExit::Return));
        assert!(is_branch_key(&after[0].branch), "{:?}", after[0].branch);
    }

    #[test]
    fn a_language_without_rules_has_no_guards() {
        assert!(!supports_branch_guards(Language::Ruby));
        assert!(!supports_branch_guards(Language::Php));
        assert!(
            guards_in_source(
                "def f\n  if x\n    go()\n  end\nend\n",
                Language::Ruby,
                3,
                Some(4)
            )
            .is_empty()
        );
        for language in [
            Language::Python,
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Go,
            Language::C,
            Language::Cpp,
        ] {
            assert!(supports_branch_guards(language), "{language:?}");
        }
    }

    #[test]
    fn python_reads_if_elif_match_early_exits_and_the_ternary_form() {
        let src = "
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
        let py = |needle: &str| label_at(src, needle, Language::Python);
        assert_eq!(py("raise HTTPException"), "not item_in.title");
        assert_eq!(
            py("session.add(item)"),
            "item_in.title && item.count > 0 and item.ok"
        );
        assert_eq!(
            py("session.delete(item)"),
            "item_in.title && !(item.count > 0 and item.ok) && item.count == 0"
        );
        assert_eq!(
            py("session.commit()"),
            "item_in.title && item.kind == \"a\""
        );
        assert_eq!(py("session.refresh(i)"), "item_in.title && i is not None");
        assert_eq!(py("return None"), "item_in.title && on error");
    }

    #[test]
    fn java_reads_early_exits_try_catch_switch_and_the_ternary() {
        let src = "
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
        let java = |needle: &str| label_at(src, needle, Language::Java);
        assert_eq!(java("this.owners.save"), "!result.hasErrors()");
        // A negated guard on one call with nested parentheses stays a bare `!`.
        let nested = "class A {\n  void f(Owner owner, int ownerId) {\n    if (!Objects.equals(owner.getId(), ownerId)) {\n      return;\n    }\n    owners.save(owner);\n  }\n}\n";
        assert_eq!(
            label_at(nested, "owners.save", Language::Java),
            "Objects.equals(owner.getId(), ownerId)"
        );
        assert_eq!(
            java("new ResponseStatusException"),
            "!result.hasErrors() && on error"
        );
        assert_eq!(
            java("owners.delete(owner)"),
            "!result.hasErrors() && owner.kind == A"
        );
        assert_eq!(
            java("return \"b\""),
            "!result.hasErrors() && owner.kind: default"
        );
        assert_eq!(java("a() : b()"), "!result.hasErrors() && cond");
        assert_eq!(
            java("owner.getId()"),
            "!result.hasErrors() && cond && !late"
        );
    }

    #[test]
    fn kotlin_reads_early_exits_try_catch_when_and_the_if_expression() {
        let src = "
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
        let kt = |needle: &str| label_at(src, needle, Language::Kotlin);
        assert_eq!(kt("owners.save(owner)"), "!result.hasErrors()");
        assert_eq!(kt("NotFound(\"x\")"), "!result.hasErrors() && on error");
        assert_eq!(
            kt("owners.delete(owner)"),
            "!result.hasErrors() && owner.kind == A"
        );
        assert_eq!(
            kt("return \"b\""),
            "!result.hasErrors() && owner.kind: else"
        );
        assert_eq!(kt("a() else"), "!result.hasErrors() && cond");
        assert_eq!(kt("b()"), "!result.hasErrors() && !cond");
        // A lambda is inline: the conditions around it are the conditions it
        // runs under.
        assert_eq!(kt("owners.save(it)"), "!result.hasErrors()");
    }

    #[test]
    fn csharp_reads_early_exits_try_catch_switch_and_the_conditional() {
        let src = "
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
        let cs = |needle: &str| label_at(src, needle, Language::CSharp);
        assert_eq!(cs("_context.Items.AddAsync"), "item != null");
        assert_eq!(cs("Conflict()"), "item != null && on error");
        assert_eq!(cs("_bus.Publish"), "item != null && item.Kind == 1");
        assert_eq!(cs("Ok(item)"), "item != null && cond");
        assert_eq!(cs("NotFound()"), "item != null && !cond");
        assert_eq!(cs("Created(\"x\""), "item != null && item.Ok && !late");
        assert_eq!(cs("StatusCode(500)"), "item != null && !(item.Ok && !late)");
    }

    #[test]
    fn go_reads_the_error_guard_flipped_else_if_chains_and_the_switch() {
        let src = "
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
        let go = |needle: &str| label_at(src, needle, Language::Go);
        assert_eq!(go("c.JSON(http.StatusBadRequest"), "err != nil");
        assert_eq!(
            go("c.AbortWithStatus"),
            "err == nil && u.Name == \"\" && !ok"
        );
        assert_eq!(
            go("db.Create"),
            "err == nil && !(u.Name == \"\" && !ok) && u.Age > 3"
        );
        assert_eq!(
            go("db.Save"),
            "err == nil && !(u.Name == \"\" && !ok) && !(u.Age > 3)"
        );
        assert_eq!(go("q.Publish"), "err == nil && u.Kind == \"a\"");
        assert_eq!(go("worker(u)"), "err == nil");
    }

    #[test]
    fn c_reads_the_null_check_guard_if_else_switch_and_the_ternary() {
        let src = "
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
        let c = |needle: &str| label_at(src, needle, Language::C);
        assert_eq!(c("perror("), "!f");
        assert_eq!(c("fprintf("), "f && argc > 2 && flag");
        assert_eq!(c("exit(2)"), "f && !(argc > 2 && flag)");
        assert_eq!(c("fclose(f)"), "f && argc == 1");
        assert_eq!(c("read(fd"), "f && argc");
    }

    #[test]
    fn guards_at_reuses_a_tree_only_for_the_same_source() {
        let first = "function f() {\n  if (a) go()\n}\n";
        let second = "function f() {\n  if (b) go()\n}\n";
        let key = "/branch-guards-test/reuse.ts";
        assert_eq!(
            guard_label(&guards_at(key, first, Language::TypeScript, 2, Some(9))),
            "a"
        );
        assert_eq!(
            guard_label(&guards_at(key, first, Language::TypeScript, 2, Some(9))),
            "a"
        );
        assert_eq!(
            guard_label(&guards_at(key, second, Language::TypeScript, 2, Some(9))),
            "b"
        );
        let huge = format!("{first}{}", " ".repeat(MAX_PARSE_BYTES));
        assert!(guards_at(key, &huge, Language::TypeScript, 2, Some(9)).is_empty());
        assert!(guards_at(key, first, Language::Ruby, 2, Some(9)).is_empty());
    }
}
