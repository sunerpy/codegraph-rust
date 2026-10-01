//! Is a C/C++ call's name a function-like macro visible at the call site?
//! (upstream `cpp-macro-visibility.ts`, #1838, #2069)
//!
//! `TRACE_POINT(1)` parses as a call, so extraction records a `calls` reference
//! named `TRACE_POINT`. If the translation unit defines `#define
//! TRACE_POINT(value) ...` before that line, in the file itself or through an
//! include, the "call" is a macro expansion, and binding it to a same-spelled
//! function in an unrelated file fabricates a caller and a callee that never
//! existed.
//!
//! Directive summaries are cached per file but evaluated in translation-unit
//! order on every inclusion. Only `#pragma once` and actual guard state
//! suppress reinclusion; an active include stack breaks cycles. Unknown build
//! flags stay possible, so only DEFINITE macro visibility suppresses a call: a
//! macro that exists in one build configuration only keeps the call to the
//! function the other configuration compiles, and a wrapper macro that calls
//! its own name is how that function gets called and hides nothing.
//! Object-like definitions take part in conditions but never enter the
//! per-root timelines.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::import_resolver::resolve_import_path;
use crate::types::{RefView, ResolutionContext};

/// Three-valued truth: `Some` is decided, `None` depends on a build flag.
type Truth = Option<bool>;

fn and(a: Truth, b: Truth) -> Truth {
    if a == Some(false) || b == Some(false) {
        Some(false)
    } else if a == Some(true) && b == Some(true) {
        Some(true)
    } else {
        None
    }
}

fn or(a: Truth, b: Truth) -> Truth {
    if a == Some(true) || b == Some(true) {
        Some(true)
    } else if a == Some(false) && b == Some(false) {
        Some(false)
    } else {
        None
    }
}

fn not(a: Truth) -> Truth {
    a.map(|value| !value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchOp {
    If,
    Ifdef,
    Ifndef,
    Elif,
    Else,
    Endif,
}

#[derive(Debug, Clone)]
enum FileEvent {
    Define {
        name: String,
        line: usize,
        function_like: bool,
        value: String,
        wraps_itself: bool,
    },
    Undef {
        name: String,
        line: usize,
    },
    Include {
        quote: u8,
        spec: String,
        line: usize,
    },
    Branch {
        op: BranchOp,
        expression: String,
        guard: bool,
    },
    Once,
}

/// Per root file: macro name → `(root line, defined)` events in root order.
type Timeline = BTreeMap<String, Vec<(usize, Truth)>>;

/// Roots kept at once; a pure cache, so eviction only costs a recomputation.
const ROOT_TIMELINE_CAP: usize = 32;

/// Per resolution pass: cleared with the resolver's other caches.
#[derive(Default)]
pub(crate) struct MacroVisibility {
    summaries: HashMap<String, Arc<Vec<FileEvent>>>,
    includes: HashMap<(Language, String, u8, String), Option<String>>,
    /// Indexed files by basename, for `#include "dir/name.h"` no include root
    /// explains.
    by_basename: Option<HashMap<String, Vec<String>>>,
    roots: HashMap<(Language, String), Arc<Timeline>>,
    root_order: VecDeque<(Language, String)>,
}

/// A `constant` minted for a function-like `#define` (`CPP_DEFINE_SIGNATURE`).
pub(crate) fn is_define_constant(node: &Node) -> bool {
    node.kind == NodeKind::Constant
        && node.signature.as_deref().is_some_and(|signature| {
            let rest = signature.trim_start();
            rest.strip_prefix('#').is_some_and(|rest| {
                rest.trim_start()
                    .strip_prefix("define")
                    .is_some_and(|tail| {
                        !tail
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                    })
            })
        })
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_word(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(is_word_byte)
}

/// Whether this C/C++ `calls` reference is a macro expansion rather than a
/// call: the index knows the name as a function-like macro and either nothing
/// but macros bears it (there is no function to call, and a loose match such
/// as `SWAP` → `swap` must not invent one) or the macro is definitely visible
/// at the call site. The name checks run without the cache's lock; only a
/// reference that needs its translation unit's timeline takes it.
pub(crate) fn is_visible_macro(
    cache: &std::sync::Mutex<MacroVisibility>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if !matches!(reference.language, Language::C | Language::Cpp)
        || reference.reference_kind != EdgeKind::Calls
        || !is_word(&reference.reference_name)
    {
        return false;
    }
    let same_name = context.get_nodes_by_name_shared(&reference.reference_name);
    if !same_name.iter().any(|node| is_define_constant(node)) {
        return false;
    }
    if !same_name.iter().any(|node| !is_define_constant(node)) {
        return true;
    }
    let timeline = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .timeline_for(&reference.file_path, reference.language, context);
    let line = usize::try_from(reference.line).unwrap_or(0);
    timeline
        .get(&reference.reference_name)
        .and_then(|events| events.iter().rev().find(|(at, _)| *at <= line))
        .is_some_and(|(_, defined)| *defined == Some(true))
}

impl MacroVisibility {
    fn timeline_for(
        &mut self,
        root: &str,
        language: Language,
        context: &dyn ResolutionContext,
    ) -> Arc<Timeline> {
        let key = (language, root.to_string());
        if let Some(timeline) = self.roots.get(&key) {
            return Arc::clone(timeline);
        }
        let timeline = Arc::new(self.walk_translation_unit(root, language, context));
        if self.roots.len() >= ROOT_TIMELINE_CAP
            && let Some(oldest) = self.root_order.pop_front()
        {
            self.roots.remove(&oldest);
        }
        self.root_order.push_back(key.clone());
        self.roots.insert(key, Arc::clone(&timeline));
        timeline
    }

    /// Cache syntax, never conditional truth: an included file can change its
    /// flags.
    fn summarize(&mut self, file: &str, context: &dyn ResolutionContext) -> Arc<Vec<FileEvent>> {
        if let Some(cached) = self.summaries.get(file) {
            return Arc::clone(cached);
        }
        let source = context.read_file(file).unwrap_or_default();
        let lines = directive_lines(&source);
        let code = code_line_flags(&source);
        let mut events = Vec::new();
        let mut continued_until = 0;
        for (index, text) in lines.iter().enumerate() {
            // A continuation line belongs to the directive above it.
            if index < continued_until {
                continue;
            }
            if let Some(branch) = BRANCH.captures(text) {
                let op = match &branch[1] {
                    "ifdef" => BranchOp::Ifdef,
                    "ifndef" => BranchOp::Ifndef,
                    "if" => BranchOp::If,
                    "elif" => BranchOp::Elif,
                    "else" => BranchOp::Else,
                    _ => BranchOp::Endif,
                };
                let expression = if matches!(op, BranchOp::If | BranchOp::Elif) {
                    let (logical, taken) = spliced(&lines, index, &branch[2]);
                    continued_until = index + 1 + taken;
                    logical
                } else {
                    branch[2].to_string()
                };
                let guard = guards_itself(&lines, &code, index, op, &expression);
                events.push(FileEvent::Branch {
                    op,
                    expression,
                    guard,
                });
                continue;
            }
            if let Some(directive) = DEFINE_OR_UNDEF.captures(text) {
                let name = directive[2].to_string();
                let line = index + 1;
                if &directive[1] == "define" {
                    let function_like = !directive[3].is_empty();
                    let matched = directive.get(0).map_or(0, |m| m.end());
                    // An object-like value is read as `#if` reads it, so it
                    // gets its continuation lines too.
                    let value = if function_like {
                        text[matched..].to_string()
                    } else {
                        let (logical, taken) = spliced(&lines, index, &text[matched..]);
                        continued_until = index + 1 + taken;
                        logical
                    };
                    events.push(FileEvent::Define {
                        wraps_itself: function_like && calls_itself(&lines, index, &name),
                        value,
                        name,
                        line,
                        function_like,
                    });
                } else {
                    events.push(FileEvent::Undef { name, line });
                }
                continue;
            }
            if let Some(include) = INCLUDE.captures(text) {
                events.push(FileEvent::Include {
                    quote: include[1].as_bytes()[0],
                    spec: include[2].to_string(),
                    line: index + 1,
                });
            }
            if PRAGMA_ONCE.is_match(text) {
                events.push(FileEvent::Once);
            }
        }
        let events = Arc::new(events);
        self.summaries.insert(file.to_string(), Arc::clone(&events));
        events
    }

    fn resolve_include(
        &mut self,
        file: &str,
        quote: u8,
        spec: &str,
        language: Language,
        context: &dyn ResolutionContext,
    ) -> Option<String> {
        let key = (language, file.to_string(), quote, spec.to_string());
        if let Some(cached) = self.includes.get(&key) {
            return cached.clone();
        }
        let spec_slash = spec.replace('\\', "/");
        let dir = crate::pathutil::dirname(file);
        let local = crate::pathutil::normalize(&if dir.is_empty() {
            spec_slash.clone()
        } else {
            format!("{dir}/{spec_slash}")
        });
        let mut target = if quote == b'"'
            && !local.starts_with("../")
            && !local.starts_with('/')
            && context.file_exists(&local)
        {
            Some(local)
        } else {
            resolve_import_path(spec, file, language, context)
        };
        if target.is_none() {
            let by_basename = self.by_basename.get_or_insert_with(|| {
                let mut map: HashMap<String, Vec<String>> = HashMap::new();
                for indexed in context.get_all_files() {
                    let base = indexed.rsplit('/').next().unwrap_or(&indexed).to_string();
                    map.entry(base).or_default().push(indexed);
                }
                map
            });
            let base = spec_slash.rsplit('/').next().unwrap_or(&spec_slash);
            let matches: Vec<&String> = by_basename
                .get(base)
                .map(|files| {
                    files
                        .iter()
                        .filter(|f| **f == spec_slash || f.ends_with(&format!("/{spec_slash}")))
                        .collect()
                })
                .unwrap_or_default();
            if let [only] = matches.as_slice() {
                target = Some((*only).clone());
            }
        }
        self.includes.insert(key, target.clone());
        target
    }

    fn walk_translation_unit(
        &mut self,
        root: &str,
        language: Language,
        context: &dyn ResolutionContext,
    ) -> Timeline {
        let mut walk = Walk::default();
        if let Some(frame) = walk.enter(root, Some(true), None, self, context) {
            walk.stack.push(frame);
        }
        while let Some(frame) = walk.stack.last_mut() {
            let Some(event) = frame.events.get(frame.next).cloned() else {
                let done = walk.stack.pop().expect("a frame to pop");
                walk.scanning.remove(&done.file);
                continue;
            };
            frame.next += 1;
            let (file, active, include_line) =
                (frame.file.clone(), frame.active, frame.include_line);
            let at = |line: usize| include_line.unwrap_or(line);
            match event {
                FileEvent::Branch {
                    op,
                    expression,
                    guard,
                } => {
                    let known = walk
                        .definitions
                        .get(expression.trim())
                        .and_then(|d| d.defined);
                    let condition = walk.condition(&expression);
                    let frame = walk.stack.last_mut().expect("current frame");
                    match op {
                        BranchOp::If | BranchOp::Ifdef | BranchOp::Ifndef => {
                            let mut selected = match op {
                                BranchOp::If => condition,
                                BranchOp::Ifndef => not(known),
                                _ => known,
                            };
                            if selected.is_none() && guard {
                                selected = Some(true);
                            }
                            frame.branches.push((frame.active, selected));
                            frame.active = and(frame.active, selected);
                        }
                        BranchOp::Endif => {
                            frame.active = frame
                                .branches
                                .pop()
                                .map_or(frame.inherited, |(parent, _)| parent);
                        }
                        BranchOp::Elif | BranchOp::Else => {
                            let test = if op == BranchOp::Else {
                                Some(true)
                            } else {
                                condition
                            };
                            if let Some((parent, taken)) = frame.branches.last_mut() {
                                frame.active = and(*parent, and(not(*taken), test));
                                *taken = or(*taken, test);
                            }
                        }
                    }
                }
                _ if active == Some(false) => {}
                FileEvent::Once => {
                    let prior = walk.once.get(&file).copied().unwrap_or(Some(false));
                    walk.once.insert(file, or(prior, active));
                }
                FileEvent::Include { quote, spec, line } => {
                    if let Some(target) =
                        self.resolve_include(&file, quote, &spec, language, context)
                        && let Some(child) =
                            walk.enter(&target, active, Some(at(line)), self, context)
                    {
                        walk.stack.push(child);
                    }
                }
                FileEvent::Define {
                    name,
                    line,
                    function_like,
                    value,
                    wraps_itself,
                } => {
                    let macro_now = function_like && !wraps_itself;
                    walk.record(
                        &name,
                        at(line),
                        active,
                        true,
                        macro_now,
                        function_like,
                        &value,
                    );
                }
                FileEvent::Undef { name, line } => {
                    walk.record(&name, at(line), active, false, false, false, "");
                }
            }
        }
        walk.timeline
    }
}

#[derive(Default, Clone, Copy)]
struct Definition {
    defined: Truth,
    value: Truth,
    macro_: Truth,
}

struct Frame {
    file: String,
    events: Arc<Vec<FileEvent>>,
    next: usize,
    inherited: Truth,
    active: Truth,
    /// Open `#if` frames: `(active before it, some branch taken)`.
    branches: Vec<(Truth, Truth)>,
    /// The root file's line of the outermost `#include`, for nested events.
    include_line: Option<usize>,
}

#[derive(Default)]
struct Walk {
    stack: Vec<Frame>,
    definitions: HashMap<String, Definition>,
    scanning: HashSet<String>,
    macro_names: HashSet<String>,
    once: HashMap<String, Truth>,
    timeline: Timeline,
}

impl Walk {
    fn enter(
        &mut self,
        file: &str,
        inherited: Truth,
        include_line: Option<usize>,
        cache: &mut MacroVisibility,
        context: &dyn ResolutionContext,
    ) -> Option<Frame> {
        if inherited == Some(false)
            || self.scanning.contains(file)
            || self.once.get(file).copied() == Some(Some(true))
        {
            return None;
        }
        self.scanning.insert(file.to_string());
        Some(Frame {
            file: file.to_string(),
            events: cache.summarize(file, context),
            next: 0,
            inherited,
            active: inherited,
            branches: Vec::new(),
            include_line,
        })
    }

    /// What an `#if` expression says, as far as the source decides it: the
    /// whole expression, three-valued (see [`evaluate`]). Upstream reads a
    /// literal, one `defined` test or a bare name only (KEEP-RUST).
    fn condition(&self, expression: &str) -> Truth {
        evaluate(expression, &|name| {
            let known = self.definitions.get(name);
            NameState {
                defined: known.and_then(|d| d.defined),
                value: known.and_then(|d| d.value),
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        name: &str,
        line: usize,
        active: Truth,
        defining: bool,
        macro_now: bool,
        function_like: bool,
        value: &str,
    ) {
        let prior = self.definitions.get(name).copied();
        let macro_ = (active == Some(true) || prior.and_then(|p| p.macro_) == Some(macro_now))
            .then_some(macro_now);
        // A name no directive has touched is unknown, not undefined: the build
        // can set it on the command line. So an `#undef` under an undecidable
        // `#if` leaves it unknown (#2069); only a certain one clears it.
        let prior_defined = prior.and_then(|p| p.defined);
        let defined = if defining {
            or(prior_defined, active)
        } else {
            and(prior_defined, not(active))
        };
        let value = if defining && active == Some(true) {
            self.condition(value)
        } else {
            None
        };
        self.definitions.insert(
            name.to_string(),
            Definition {
                defined,
                value,
                macro_,
            },
        );
        if function_like {
            self.macro_names.insert(name.to_string());
        }
        if self.macro_names.contains(name) {
            self.timeline
                .entry(name.to_string())
                .or_default()
                .push((line, macro_));
        }
    }
}

static BRANCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*#\s*(ifdef|ifndef|if|elif|else|endif)\b(.*)$")
        .expect("branch directive regex is valid")
});
static DEFINE_OR_UNDEF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*#\s*(define|undef)\s+([A-Za-z0-9_]+)(\(?)")
        .expect("define directive regex is valid")
});
static INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*#\s*include\s*([<"])([^>"]+)[>"]"#).expect("include regex is valid")
});
static PRAGMA_ONCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*#\s*pragma\s+once\b").expect("pragma once regex is valid"));
static NOT_DEFINED_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*!\s*defined\s*(?:\(\s*([A-Za-z0-9_]+)\s*\)|([A-Za-z0-9_]+))\s*$")
        .expect("guard test regex is valid")
});

/// A directive's text with its backslash-continued lines spliced on, as the
/// preprocessor reads one logical line, and how many continuation lines that
/// took. A continuation line loses its comments and its trailing backslash.
fn spliced(lines: &[String], index: usize, first: &str) -> (String, usize) {
    let mut text = first.trim_end().to_string();
    let mut taken = 0;
    while let Some(head) = text.strip_suffix('\\') {
        let head = head.trim_end().to_string();
        let Some(next) = lines.get(index + 1 + taken) else {
            text = head;
            break;
        };
        taken += 1;
        text = format!("{head} {}", strip_line_comments(next).trim())
            .trim_end()
            .to_string();
    }
    (text, taken)
}

/// One line without its `//` and `/* … */` comments; an unclosed `/*` drops
/// the rest of the line. Quoted text is kept as is.
fn strip_line_comments(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut quote: Option<u8> = None;
    let mut kept_from = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(q) = quote {
            if byte == b'\\' {
                i += 1;
            } else if byte == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                out.push_str(&line[kept_from..i]);
                return out;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                out.push_str(&line[kept_from..i]);
                match line[i + 2..].find("*/") {
                    Some(end) => {
                        out.push(' ');
                        i += 2 + end + 2;
                        kept_from = i;
                        continue;
                    }
                    None => return out,
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&line[kept_from.min(line.len())..]);
    out
}

/// What the walk knows about a name an `#if` mentions.
struct NameState {
    defined: Truth,
    /// The truth of its value, when a definitely active definition gave one.
    value: Truth,
}

/// An `#if` operand: a known integer, or a truth that may depend on the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    Int(i64),
    Truth(Truth),
}

impl Value {
    fn truth(self) -> Truth {
        match self {
            Value::Int(number) => Some(number != 0),
            Value::Truth(truth) => truth,
        }
    }

    fn int(self) -> Option<i64> {
        match self {
            Value::Int(number) => Some(number),
            Value::Truth(truth) => truth.map(i64::from),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    /// `None` for a number the evaluator cannot read (a float, an overflow).
    Number(Option<i64>),
    Word(&'a str),
    Punct(&'static str),
    /// A character or string literal, or a byte no operator uses.
    Opaque,
}

const PUNCTUATORS: [&str; 25] = [
    "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "&", "^",
    "|", "!", "~", "?", ":", "(", ")", ",",
];

fn tokens(expression: &str) -> Vec<Token<'_>> {
    let bytes = expression.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte.is_ascii_whitespace() {
            i += 1;
        } else if byte.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (is_word_byte(bytes[i]) || matches!(bytes[i], b'\'' | b'.')) {
                i += 1;
            }
            out.push(Token::Number(integer_literal(&expression[start..i])));
        } else if is_word_byte(byte) {
            let start = i;
            while i < bytes.len() && is_word_byte(bytes[i]) {
                i += 1;
            }
            out.push(Token::Word(&expression[start..i]));
        } else if byte == b'"' || byte == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != byte {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push(Token::Opaque);
        } else if let Some(punct) = PUNCTUATORS
            .iter()
            .find(|punct| expression[i..].starts_with(**punct))
        {
            i += punct.len();
            out.push(Token::Punct(punct));
        } else {
            i += expression[i..].chars().next().map_or(1, char::len_utf8);
            out.push(Token::Opaque);
        }
    }
    out
}

/// A C integer literal — decimal, `0x`, `0b` or octal, with digit separators
/// and `u`/`l`/`z` suffixes — or `None`.
fn integer_literal(text: &str) -> Option<i64> {
    let digits: String = text
        .trim_end_matches(['u', 'U', 'l', 'L', 'z', 'Z'])
        .chars()
        .filter(|c| *c != '\'')
        .collect();
    let (radix, body) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, hex)
    } else if let Some(binary) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        (2, binary)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits.as_str())
    };
    i64::from_str_radix(body, radix).ok()
}

/// Evaluate an `#if` expression three-valued. `&&` and `||` decide whenever
/// one side does (`1 || FLAG` is true, `0 && FLAG` false); `?:` with an
/// unknown condition is known only when both branches agree; every other
/// operator needs known operands. An unseen name, a definitely undefined
/// name's `0` aside, is unknown; so are a call-like `__has_include(…)`, a
/// character literal, overflow, division by zero, and anything malformed.
fn evaluate(expression: &str, name: &dyn Fn(&str) -> NameState) -> Truth {
    let tokens = tokens(expression);
    let mut parser = ConditionParser {
        tokens: &tokens,
        at: 0,
        name,
    };
    let value = parser.ternary()?;
    (parser.at == tokens.len()).then_some(())?;
    value.truth()
}

struct ConditionParser<'t, 'n> {
    tokens: &'t [Token<'t>],
    at: usize,
    name: &'n dyn Fn(&str) -> NameState,
}

impl<'t> ConditionParser<'t, '_> {
    fn peek(&self) -> Option<Token<'t>> {
        self.tokens.get(self.at).copied()
    }

    fn next(&mut self) -> Option<Token<'t>> {
        let token = self.tokens.get(self.at).copied();
        self.at += 1;
        token
    }

    fn eat(&mut self, punct: &str) -> bool {
        let found = matches!(self.peek(), Some(Token::Punct(p)) if p == punct);
        if found {
            self.at += 1;
        }
        found
    }

    fn ternary(&mut self) -> Option<Value> {
        let condition = self.binary(1)?;
        if !self.eat("?") {
            return Some(condition);
        }
        let yes = self.ternary()?;
        if !self.eat(":") {
            return None;
        }
        let no = self.ternary()?;
        Some(match condition.truth() {
            Some(true) => yes,
            Some(false) => no,
            None => match (yes.int(), no.int()) {
                (Some(a), Some(b)) if a == b => Value::Int(a),
                _ => Value::Truth(None),
            },
        })
    }

    fn binary(&mut self, minimum: u8) -> Option<Value> {
        let mut left = self.unary()?;
        while let Some(Token::Punct(op)) = self.peek() {
            let Some(precedence) = binary_precedence(op).filter(|p| *p >= minimum) else {
                break;
            };
            self.at += 1;
            let right = self.binary(precedence + 1)?;
            left = combine(op, left, right);
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Value> {
        let Some(Token::Punct(op @ ("!" | "~" | "-" | "+"))) = self.peek() else {
            return self.primary();
        };
        self.at += 1;
        let value = self.unary()?;
        Some(match op {
            "!" => Value::Truth(not(value.truth())),
            "~" => value.int().map_or(Value::Truth(None), |n| Value::Int(!n)),
            "-" => value
                .int()
                .and_then(i64::checked_neg)
                .map_or(Value::Truth(None), Value::Int),
            _ => value.int().map_or(Value::Truth(None), Value::Int),
        })
    }

    fn primary(&mut self) -> Option<Value> {
        match self.next()? {
            Token::Number(number) => Some(number.map_or(Value::Truth(None), Value::Int)),
            Token::Punct("(") => {
                let value = self.ternary()?;
                self.eat(")").then_some(value)
            }
            Token::Word("defined") => {
                let parenthesized = self.eat("(");
                let Some(Token::Word(word)) = self.next() else {
                    return None;
                };
                if parenthesized && !self.eat(")") {
                    return None;
                }
                Some(Value::Truth((self.name)(word).defined))
            }
            Token::Word(word) => {
                if matches!(self.peek(), Some(Token::Punct("("))) {
                    // `__has_include(<x>)`, a function-like macro: the build decides.
                    self.skip_group()?;
                    return Some(Value::Truth(None));
                }
                let state = (self.name)(word);
                Some(if state.defined == Some(false) {
                    Value::Int(0)
                } else {
                    Value::Truth(state.value)
                })
            }
            Token::Opaque => Some(Value::Truth(None)),
            Token::Punct(_) => None,
        }
    }

    /// Skip one balanced `( … )` group, whatever it holds.
    fn skip_group(&mut self) -> Option<()> {
        let mut depth = 0usize;
        loop {
            match self.next()? {
                Token::Punct("(") => depth += 1,
                Token::Punct(")") => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(());
                    }
                }
                _ => {}
            }
        }
    }
}

fn binary_precedence(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | "<=" | ">" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

fn combine(op: &str, left: Value, right: Value) -> Value {
    match op {
        "||" => Value::Truth(or(left.truth(), right.truth())),
        "&&" => Value::Truth(and(left.truth(), right.truth())),
        _ => left
            .int()
            .zip(right.int())
            .and_then(|(a, b)| arithmetic(op, a, b))
            .map_or(Value::Truth(None), Value::Int),
    }
}

fn arithmetic(op: &str, a: i64, b: i64) -> Option<i64> {
    Some(match op {
        "*" => a.checked_mul(b)?,
        "/" => a.checked_div(b)?,
        "%" => a.checked_rem(b)?,
        "+" => a.checked_add(b)?,
        "-" => a.checked_sub(b)?,
        "<<" => a.checked_shl(u32::try_from(b).ok()?)?,
        ">>" => a.checked_shr(u32::try_from(b).ok()?)?,
        "<" => i64::from(a < b),
        "<=" => i64::from(a <= b),
        ">" => i64::from(a > b),
        ">=" => i64::from(a >= b),
        "==" => i64::from(a == b),
        "!=" => i64::from(a != b),
        "&" => a & b,
        "^" => a ^ b,
        "|" => a | b,
        _ => return None,
    })
}

/// A whole-file include guard: `#ifndef X_H` (or `#if !defined(X_H)`) is
/// the file's first code line, its next directive is an empty `#define X_H`,
/// and its matching `#endif` — with no `#else`/`#elif` at its depth — is the
/// file's last code line. Nothing defines the guard before the test, so this
/// is the first inclusion and the guarded body is active.
///
/// Upstream reads any `#ifndef X` / `#define X` pair, and a fallback
/// function-like macro (`#ifndef MIN` / `#define MIN(a, b) …`), the same way.
/// The port does not (KEEP-RUST): a feature-flag default
/// (`#ifndef FEATURE` / `#define FEATURE` among other code) is skipped by a
/// build with `-DFEATURE`, and a prior `MIN` from an unseen header or `-D` may
/// be a wrapper that calls the function. A valued default
/// (`#ifndef ENABLE_X` / `#define ENABLE_X 0`) is the flag a build overrides,
/// never a guard.
fn guards_itself(
    lines: &[String],
    code: &[bool],
    index: usize,
    op: BranchOp,
    expression: &str,
) -> bool {
    let name = match op {
        BranchOp::Ifndef => expression.trim().to_string(),
        BranchOp::If => NOT_DEFINED_GUARD
            .captures(expression)
            .and_then(|c| c.get(1).or_else(|| c.get(2)))
            .map_or(String::new(), |m| m.as_str().to_string()),
        _ => return false,
    };
    if !is_word(&name) || code.iter().take(index).any(|has_code| *has_code) {
        return false;
    }
    let Some(next) = lines[index + 1..]
        .iter()
        .find(|text| text.trim_start().starts_with('#'))
    else {
        return false;
    };
    let Some(captures) = DEFINE_OR_UNDEF.captures(next) else {
        return false;
    };
    if &captures[1] != "define" || captures[2] != *name {
        return false;
    }
    if !next[captures.get(2).map_or(0, |m| m.end())..]
        .trim()
        .is_empty()
    {
        return false;
    }
    let mut depth = 0usize;
    for (at, text) in lines.iter().enumerate().skip(index + 1) {
        let Some(branch) = BRANCH.captures(text) else {
            continue;
        };
        match (&branch[1], depth) {
            ("if" | "ifdef" | "ifndef", _) => depth += 1,
            ("else" | "elif", 0) => return false,
            ("endif", 0) => return !code.iter().skip(at + 1).any(|has_code| *has_code),
            ("endif", _) => depth -= 1,
            _ => {}
        }
    }
    false
}

/// Per line: whether anything but comments and whitespace is on it, with raw
/// string bodies masked first, as [`directive_lines`] does.
fn code_line_flags(source: &str) -> Vec<bool> {
    let masked = mask_cpp_raw_strings(source);
    let mut in_block = false;
    masked
        .split('\n')
        .map(|raw| {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let bytes = line.as_bytes();
            let mut has_code = false;
            let mut quote: Option<u8> = None;
            let mut i = 0;
            while i < bytes.len() {
                if in_block {
                    let Some(end) = line[i..].find("*/") else {
                        break;
                    };
                    i += end + 2;
                    in_block = false;
                    continue;
                }
                let byte = bytes[i];
                if let Some(q) = quote {
                    if byte == b'\\' {
                        i += 1;
                    } else if byte == q {
                        quote = None;
                    }
                    i += 1;
                    continue;
                }
                match byte {
                    b'/' if bytes.get(i + 1) == Some(&b'/') => break,
                    b'/' if bytes.get(i + 1) == Some(&b'*') => {
                        in_block = true;
                        i += 2;
                        continue;
                    }
                    b'"' | b'\'' => {
                        quote = Some(byte);
                        has_code = true;
                    }
                    _ if !byte.is_ascii_whitespace() => has_code = true,
                    _ => {}
                }
                i += 1;
            }
            has_code
        })
        .collect()
}

/// Does the body of the `#define NAME(` at `index` (continuation lines
/// included) call `NAME`, directly or as `(NAME)(…)`?
fn calls_itself(lines: &[String], index: usize, name: &str) -> bool {
    let mut text = lines[index].clone();
    let mut j = index;
    while lines[j].trim_end().ends_with('\\') && j + 1 < lines.len() {
        text.push(' ');
        text.push_str(&lines[j + 1]);
        j += 1;
    }
    let Some(open) = text.find('(') else {
        return false;
    };
    let body = &text[open + 1..];
    let bytes = body.as_bytes();
    let mut from = 0;
    while let Some(found) = body[from..].find(name) {
        let start = from + found;
        let end = start + name.len();
        from = start + 1;
        if start > 0 && is_word_byte(bytes[start - 1]) {
            continue;
        }
        if bytes.get(end).is_some_and(|b| is_word_byte(*b)) {
            continue;
        }
        let after = body[end..].trim_start();
        if after.starts_with('(') {
            return true;
        }
        // `(NAME)(...)`
        let before = body[..start].trim_end();
        if before.ends_with('(')
            && let Some(close) = after.strip_prefix(')')
            && close.trim_start().starts_with('(')
        {
            return true;
        }
    }
    false
}

/// The file's lines with comments removed only as far as the preprocessor
/// needs: a line inside a block comment is blank, a directive line loses its
/// trailing `//` / `/* … */`, and every other line is kept verbatim (its
/// content is never read, only whether a block comment opens on it). Raw
/// string bodies are masked first, so a `#define` inside one is not a
/// directive.
fn directive_lines(source: &str) -> Vec<String> {
    let masked = mask_cpp_raw_strings(source);
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in masked.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let mut text = raw.to_string();
        if in_block {
            let Some(end) = text.find("*/") else {
                out.push(String::new());
                continue;
            };
            text = text[end + 2..].to_string();
            in_block = false;
        }
        let directive = text.trim_start().starts_with('#');
        let mut kept: Option<String> = None;
        let mut quote: Option<u8> = None;
        let mut i = 0;
        while i < text.len() {
            let byte = text.as_bytes()[i];
            if let Some(q) = quote {
                if byte == b'\\' {
                    i += 1;
                } else if byte == q {
                    quote = None;
                }
                i += 1;
                continue;
            }
            if byte == b'"' || byte == b'\'' {
                quote = Some(byte);
                i += 1;
                continue;
            }
            if byte == b'/' && text.as_bytes().get(i + 1) == Some(&b'/') {
                kept = Some(text[..i].to_string());
                break;
            }
            if byte == b'/' && text.as_bytes().get(i + 1) == Some(&b'*') {
                match text[i + 2..].find("*/") {
                    None => {
                        in_block = true;
                        kept = Some(text[..i].to_string());
                        break;
                    }
                    Some(end) => {
                        text = format!("{} {}", &text[..i], &text[i + 2 + end + 2..]);
                        continue;
                    }
                }
            }
            i += 1;
        }
        out.push(if directive {
            kept.filter(|k| !k.is_empty()).unwrap_or(text)
        } else {
            raw.to_string()
        });
    }
    out
}

/// Blank the contents of C++ raw string literals (`R"tag(...)tag"`, with an
/// optional `u8`/`L`/`u`/`U` prefix), keeping line breaks, after skipping
/// comments and ordinary literals (upstream `maskCppRawStrings`). A char
/// literal must not follow a word character, so a digit separator (`1'000`)
/// opens none.
fn mask_cpp_raw_strings(source: &str) -> String {
    if !source.contains("R\"") {
        return source.to_string();
    }
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        if boundary && let Some(open) = raw_string_open(bytes, i) {
            let (delimiter_start, paren) = open;
            let mut closer = Vec::with_capacity(paren - delimiter_start + 2);
            closer.push(b')');
            closer.extend_from_slice(&bytes[delimiter_start..paren]);
            closer.push(b'"');
            let mut end = bytes.len();
            let mut j = paren + 1;
            while j + closer.len() <= bytes.len() {
                if bytes[j..j + closer.len()] == closer[..] {
                    end = j + closer.len();
                    break;
                }
                j += 1;
            }
            for byte in &mut out[i..end] {
                if *byte != b'\n' && *byte != b'\r' {
                    *byte = b' ';
                }
            }
            i = end;
            continue;
        }
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            quote @ b'"' => i = skip_literal(bytes, i, quote),
            quote @ b'\'' if boundary => i = skip_literal(bytes, i, quote),
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

/// `(delimiter start, opening paren)` of a raw string literal starting at `i`.
fn raw_string_open(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    let rest = &bytes[i..];
    let prefix = [&b"u8R\""[..], b"LR\"", b"uR\"", b"UR\"", b"R\""]
        .into_iter()
        .find(|prefix| rest.starts_with(prefix))?;
    let delimiter_start = i + prefix.len();
    let mut j = delimiter_start;
    while j < bytes.len() && j - delimiter_start <= 16 {
        match bytes[j] {
            b'(' => return Some((delimiter_start, j)),
            b' ' | b'\t' | b'\x0b' | b'\x0c' | b'\r' | b'\n' | b')' | b'\\' => return None,
            _ => j += 1,
        }
    }
    None
}

/// Past an ordinary string or char literal, which may run over line breaks as
/// upstream's pattern does.
fn skip_literal(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_lines_drop_comments_but_keep_directive_text() {
        let lines = directive_lines(
            "#define A(x) x // trailing\n/* open\n#define HIDDEN(x) x\n*/ #define AFTER(x) x\nconst char *s = \"/* not a comment\";\n#define B(x) x /* inline */ + 1\n",
        );
        assert_eq!(lines[0], "#define A(x) x ");
        assert_eq!(lines[2], "");
        assert_eq!(lines[5], "#define B(x) x   + 1");
    }

    #[test]
    fn raw_string_bodies_are_masked_and_digit_separators_open_nothing() {
        let masked = mask_cpp_raw_strings(
            "int n = 1'000;\nconst char *r = R\"doc(\n#define helper(x) x\n)doc\";\n#define REAL(x) x\n",
        );
        assert!(!masked.contains("helper"));
        assert!(masked.contains("#define REAL(x) x"));
        assert_eq!(masked.lines().count(), 5);
    }

    #[test]
    fn a_wrapper_macro_calling_its_own_name_is_recognized() {
        let lines: Vec<String> = [
            "#define vec_splice(v, start, count)\\",
            "  ( vec_splice((char **)(v), start, count),\\",
            "    (v)->length -= (count) )",
        ]
        .map(str::to_string)
        .to_vec();
        assert!(calls_itself(&lines, 0, "vec_splice"));
        let plain = vec!["#define helper(x) ((x) + 1)".to_string()];
        assert!(!calls_itself(&plain, 0, "helper"));
        let parenthesized = vec!["#define wrap(x) (wrap)(x)".to_string()];
        assert!(calls_itself(&parenthesized, 0, "wrap"));
    }

    /// Evaluate with `A` definitely defined as 1, `Z` definitely undefined,
    /// and every other name unseen.
    fn eval(expression: &str) -> Truth {
        evaluate(expression, &|name| match name {
            "A" => NameState {
                defined: Some(true),
                value: Some(true),
            },
            "Z" => NameState {
                defined: Some(false),
                value: None,
            },
            _ => NameState {
                defined: None,
                value: None,
            },
        })
    }

    #[test]
    fn conditions_are_whole_expressions_decided_three_valued() {
        for (expression, expected) in [
            // The forms upstream reads.
            ("1", Some(true)),
            ("0x0", Some(false)),
            ("10UL", Some(true)),
            ("defined(A)", Some(true)),
            ("!defined A", Some(false)),
            ("defined FLAG", None),
            ("A", Some(true)),
            ("FLAG", None),
            // `||` and `&&` decide whenever one side does.
            ("1 || FLAG", Some(true)),
            ("FLAG || 1", Some(true)),
            ("0 && FLAG", Some(false)),
            ("FLAG && 1", None),
            ("defined(A) || defined(B)", Some(true)),
            ("defined A && !defined(Z)", Some(true)),
            // C precedence and arithmetic on known operands.
            ("1 + 2 * 3 == 7", Some(true)),
            ("(1 + 2) * 3 == 9", Some(true)),
            ("1 << 4 == 0x10 && 07 == 7 && 0b101 == 5", Some(true)),
            ("1'000 > 999", Some(true)),
            ("-1 < 0 && ~0 == -1", Some(true)),
            ("!0 == 1", Some(true)),
            // A definitely undefined name is 0; an unseen one is unknown.
            ("Z == 0", Some(true)),
            ("FLAG == 0", None),
            // `?:` with an unknown condition needs agreeing branches.
            ("1 ? FLAG : 0", None),
            ("FLAG ? 1 : 1", Some(true)),
            ("0 ? FLAG : A", Some(true)),
            // The build decides: calls, characters, overflow, division by zero.
            ("__has_include(<stdio.h>)", None),
            ("__has_include(<stdio.h>) || 1", Some(true)),
            ("'a' == 97", None),
            ("9223372036854775807 + 1", None),
            ("1 / 0", None),
            ("1.5", None),
            // Malformed text decides nothing.
            ("", None),
            ("1 +", None),
            ("1 2", None),
            ("(1", None),
            ("1 ? 2", None),
            ("defined", None),
        ] {
            assert_eq!(eval(expression), expected, "{expression:?}");
        }
    }

    #[test]
    fn continued_directives_are_one_logical_line() {
        let lines: Vec<String> = [
            "#if 1 || \\",
            "    FLAG // trailing",
            "#elif A && \\",
            "  /* note */ B \\",
            "  && C",
            "#if DONE",
        ]
        .map(str::to_string)
        .to_vec();
        assert_eq!(
            spliced(&lines, 0, " 1 || \\"),
            (" 1 || FLAG".to_string(), 1)
        );
        assert_eq!(
            spliced(&lines, 2, " A && \\"),
            (" A && B && C".to_string(), 2)
        );
        assert_eq!(spliced(&lines, 5, " DONE"), (" DONE".to_string(), 0));
        // A last line that still ends in a backslash just stops.
        assert_eq!(spliced(&lines[..1], 0, " 1 \\"), (" 1".to_string(), 0));
    }

    #[test]
    fn guards_are_whole_file_ifndef_define_idioms_only() {
        // `(file, line of the test, op, expression, guard?)`.
        let cases = [
            (
                "/* License */\n#ifndef X_H\n#define X_H\nint x;\n#endif // X_H\n",
                1,
                BranchOp::Ifndef,
                " X_H",
                true,
            ),
            (
                "#if !defined(X_H)\n#define X_H\n#if A\n#else\n#endif\n#endif\n",
                0,
                BranchOp::If,
                " !defined(X_H)",
                true,
            ),
            // A fallback function-like macro is no guard (KEEP-RUST).
            (
                "#if !defined(MIN)\n#define MIN(a, b) a\n#endif\n",
                0,
                BranchOp::If,
                " !defined(MIN)",
                false,
            ),
            // A default VALUE is the flag a build overrides.
            (
                "#ifndef ENABLE_X\n#define ENABLE_X 0\n#endif\n",
                0,
                BranchOp::Ifndef,
                " ENABLE_X",
                false,
            ),
            // Code before the test or after the `#endif`, or an `#else` at
            // the guard's depth: not the whole file.
            (
                "int before;\n#ifndef X_H\n#define X_H\n#endif\n",
                1,
                BranchOp::Ifndef,
                " X_H",
                false,
            ),
            (
                "#ifndef X_H\n#define X_H\n#endif\nint after;\n",
                0,
                BranchOp::Ifndef,
                " X_H",
                false,
            ),
            (
                "#ifndef X_H\n#define X_H\n#else\n#endif\n",
                0,
                BranchOp::Ifndef,
                " X_H",
                false,
            ),
        ];
        for (source, index, op, expression, expected) in cases {
            let lines = directive_lines(source);
            let code = code_line_flags(source);
            assert_eq!(
                guards_itself(&lines, &code, index, op, expression),
                expected,
                "{source:?}"
            );
        }
    }

    #[test]
    fn code_lines_ignore_comments_and_whitespace() {
        let flags = code_line_flags(
            "/* a\n * b */\n// c\n  \nint x; // d\n/* e */ y\nchar *s = \"/* f\";\n",
        );
        assert_eq!(
            flags,
            vec![false, false, false, false, true, true, true, false]
        );
    }
}
