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
        let mut events = Vec::new();
        for (index, text) in lines.iter().enumerate() {
            if let Some(branch) = BRANCH.captures(text) {
                let op = match &branch[1] {
                    "ifdef" => BranchOp::Ifdef,
                    "ifndef" => BranchOp::Ifndef,
                    "if" => BranchOp::If,
                    "elif" => BranchOp::Elif,
                    "else" => BranchOp::Else,
                    _ => BranchOp::Endif,
                };
                let expression = branch[2].to_string();
                let guard = guards_itself(&lines, index, op, &expression);
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
                    events.push(FileEvent::Define {
                        wraps_itself: function_like && calls_itself(&lines, index, &name),
                        value: text[matched..].to_string(),
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

    /// What an `#if` expression says, as far as the source decides it:
    /// integer literals, `defined(NAME)` / `!defined NAME`, and a bare name's
    /// known value. Anything else depends on the build.
    fn condition(&self, expression: &str) -> Truth {
        let text = expression.trim();
        if let Some(literal) = INTEGER_LITERAL.captures(text) {
            let digits = literal.get(1).map_or(&literal[2], |hex| hex.as_str());
            return Some(digits.bytes().any(|digit| digit != b'0'));
        }
        if let Some(defined) = DEFINED_TEST.captures(text) {
            let name = defined
                .get(2)
                .or_else(|| defined.get(3))
                .map_or("", |m| m.as_str());
            let known = self.definitions.get(name).and_then(|d| d.defined);
            return if defined.get(1).is_some() {
                not(known)
            } else {
                known
            };
        }
        if is_word(text) {
            return self.definitions.get(text).and_then(|d| d.value);
        }
        None
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
static INTEGER_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:0x([0-9a-f]+)|([0-9]+))[ul]*$").expect("integer literal regex is valid")
});
static DEFINED_TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(!)?\s*defined\s*(?:\(\s*([A-Za-z0-9_]+)\s*\)|([A-Za-z0-9_]+))$")
        .expect("defined test regex is valid")
});
static NOT_DEFINED_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*!\s*defined\s*(?:\(\s*([A-Za-z0-9_]+)\s*\)|([A-Za-z0-9_]+))\s*$")
        .expect("guard test regex is valid")
});

/// The include-guard idiom: `#ifndef X_H` (or `#if !defined(X_H)`) whose next
/// directive is `#define X_H`. Nothing defines the guard before the test, so
/// this is the first inclusion and the guarded body is active. A fallback
/// function-like macro (`#ifndef MIN` / `#define MIN(a, b) …`) reads the same
/// way. A default VALUE (`#ifndef ENABLE_X` / `#define ENABLE_X 0`) does not:
/// that is the flag a build overrides on the command line.
fn guards_itself(lines: &[String], index: usize, op: BranchOp, expression: &str) -> bool {
    let name = match op {
        BranchOp::Ifndef => expression.trim().to_string(),
        BranchOp::If => NOT_DEFINED_GUARD
            .captures(expression)
            .and_then(|c| c.get(1).or_else(|| c.get(2)))
            .map_or(String::new(), |m| m.as_str().to_string()),
        _ => return false,
    };
    if !is_word(&name) {
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
    let rest = &next[captures.get(2).map_or(0, |m| m.end())..];
    rest.starts_with('(') || rest.trim().is_empty()
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

    #[test]
    fn guards_are_the_ifndef_define_idiom_and_not_a_default_value() {
        let guard = ["#ifndef X_H", "#define X_H"].map(str::to_string).to_vec();
        assert!(guards_itself(&guard, 0, BranchOp::Ifndef, " X_H"));
        let fallback = ["#if !defined(MIN)", "#define MIN(a, b) a"]
            .map(str::to_string)
            .to_vec();
        assert!(guards_itself(&fallback, 0, BranchOp::If, " !defined(MIN)"));
        let default = ["#ifndef ENABLE_X", "#define ENABLE_X 0"]
            .map(str::to_string)
            .to_vec();
        assert!(!guards_itself(&default, 0, BranchOp::Ifndef, " ENABLE_X"));
    }
}
