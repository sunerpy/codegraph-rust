//! The C and C++ function-pointer dispatch pass (upstream
//! `c-fnptr-synthesizer.ts` at `e8ddc4e2`).
//!
//! A struct carries a function-pointer field, functions are registered into
//! it by a table (`{"add", cmd_add}`, `.fn = cmd_add`) or by assignment, and
//! a dispatcher calls through it (`p->fn(argv)`). The pass keys both ends by
//! the struct type and the field and links the dispatcher to every function
//! registered there (`fn-pointer-dispatch`); a bare array of function
//! pointers, `opcodes[op](…)`, is keyed by its name. Tables built by macros,
//! headers read with their includer's macros and the `#ifdef` arms the
//! includer switches on are read as upstream reads them.
//!
//! Where upstream takes the first or the last of several candidates, this
//! pass fails closed (KEEP-RUST, hard invariant 6): a struct is its name and
//! its fields, a type name means the only struct of that name in the
//! project, else the only one the file's translation unit (the file and the
//! files its includes reach) defines, and a table entry names the
//! only function of that name in the table's file, else in its includer's,
//! else in the project. Anything still ambiguous is not linked.
//!
//! Upstream's survival filters, caches and native sweep only save time and
//! memory: every file runs through each stage here. An `#include` resolves
//! to an indexed file, else to a file the stage reads beside the index
//! (W7-17b): within the project root and the scan's ignore model, through
//! no symbolic link and no larger than `indexing.max_file_size`, which
//! upstream does not require (KEEP-RUST).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Node, NodeKind};
use regex::{Captures, Regex, RegexBuilder};
use serde_json::json;

use super::stage::{LineAt, View, node_order};
use crate::frameworks::js_scan::{JS_SPACE, is_js_space, js_trim};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};

/// The edges one dispatching function links at most (`FANOUT_CAP`).
const FANOUT_CAP: usize = 300;
/// The macro calls one `expandMacroCalls` expands at most.
const MACRO_EXPANSIONS: usize = 6;
/// The object-macro aliases a type name is read through at most
/// (`resolveTypeName`).
const TYPE_ALIAS_HOPS: usize = 5;
/// The rounds field-to-field propagation runs at most.
const PROPAGATION_ROUNDS: usize = 3;
/// The include levels below a file its macro environment reads
/// (`buildEnv(file, 2, …)`).
const ENV_DEPTH: i32 = 2;
/// The most names, and name bytes, one [`Names`] pattern spells out. Its
/// compile time grows with them (release build: 2 ms at redis-medium's 391
/// field names, 14 ms at 4096, 273 ms at 65536), so past these no text is
/// skipped.
const MAX_PATTERN_NAMES: usize = 4096;
const MAX_PATTERN_NAME_BYTES: usize = 64 * 1024;
/// The compiled size a [`Names`] pattern may reach: the regex crate's
/// default, named so a test can ask for a pattern it refuses.
const PATTERN_SIZE_LIMIT: usize = 10 * (1 << 20);

/// The paths the pass reads (`C_CPP_EXT`).
const C_CPP_EXTENSIONS: [&str; 12] = [
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "cppm", "ipp", "inl", "tcc",
];
/// The `#include`d paths it follows (`INCLUDABLE_EXT`).
const INCLUDABLE_EXTENSIONS: [&str; 13] = [
    "def", "inc", "h", "hh", "hpp", "hxx", "c", "cc", "cpp", "cxx", "ipp", "tcc", "tbl",
];
/// Words a declared type never is (`C_TYPE_KEYWORDS`).
const C_TYPE_KEYWORDS: [&str; 16] = [
    "void", "int", "char", "short", "long", "unsigned", "signed", "float", "double", "const",
    "struct", "union", "enum", "static", "volatile", "register",
];

/// Whether `path` ends in `.` and one of `extensions`, in any ASCII case.
fn has_extension(path: &str, extensions: &[&str]) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        extensions
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// JavaScript's `\S`: what [`JS_SPACE`] does not hold.
fn js_non_space() -> String {
    format!("[^{}", &JS_SPACE[1..])
}

/// JavaScript's `.`: any character but a line terminator.
const JS_DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

/// An upstream pattern with JavaScript's `\s` ([`JS_SPACE`]) and `\S` for
/// the `regex` crate's Unicode ones. Every pattern here spells `\w` as
/// `[A-Za-z0-9_]`, `\b` as `(?-u:\b)` and `.` as [`JS_DOT`].
fn js_regex(pattern: &str, what: &str) -> Regex {
    let pattern = pattern
        .replace(r"\S", "\u{0}")
        .replace(r"\s", JS_SPACE)
        .replace('\u{0}', &js_non_space());
    Regex::new(&pattern).unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// A function-pointer declarator, `(*name)(` with any words before the `*`
/// (`FNPTR_DECL_RE`).
static FNPTR_DECL: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"\(\s*(?:[A-Za-z0-9_]+\s+)*\*\s*([A-Za-z0-9_]+)\s*\)\s*\(",
        "fn-pointer declarator",
    )
});
/// `typedef RET (*NAME)(…)` (`FNPTR_TYPEDEF_RE`).
static FNPTR_TYPEDEF: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)typedef(?-u:\b)[^;{}]*?\(\s*(?:[A-Za-z0-9_]+\s+)*\*\s*([A-Za-z0-9_]+)\s*\)\s*\(",
        "fn-pointer typedef",
    )
});
/// A brace-free `typedef …;` statement (`FNTYPE_TYPEDEF_STMT_RE`).
static TYPEDEF_STATEMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)typedef(?-u:\b)([^;{}]*);").expect("typedef statement"));
/// The first word before a `(` in a typedef statement.
static WORD_BEFORE_PAREN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?-u:\b)([A-Za-z0-9_]+)\s*\(", "word before paren"));
/// `#include "path"`, read from the raw text (`INCLUDE_RE`).
static INCLUDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"#[ \t]*include[ \t]+"([^"\n]+)""#).expect("include"));
/// `TYPE name[opt] = {` after a statement boundary (`INIT_RE`).
static INIT: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?:^|[;{}])\s*(?:(?:static|const|extern|register|volatile)\s+)*(?:(?:struct|union)\s+)?([A-Za-z0-9_]+)\s+([A-Za-z0-9_]+)\s*(\[[^\]]*\])?\s*=\s*\{",
        "initializer",
    )
});
/// `struct TAG {` (`INLINE_STRUCT_RE`).
static INLINE_STRUCT: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)(?:struct|union)\s+([A-Za-z0-9_]+)\s*\{",
        "inline struct",
    )
});
/// The variable an inline struct's `}` declares, and its table.
static INLINE_TABLE_VAR: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"^\s*([A-Za-z0-9_]+)\s*(\[[^\]]*\])?\s*(=\s*\{)?",
        "inline table variable",
    )
});
/// `ELEMTYPE [*] name[…] = {` (`ARRAY_TABLE_RE`).
static ARRAY_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?:^|[;{}])\s*(?:(?:static|const|extern|register|volatile)\s+)*([A-Za-z0-9_]+)\s+(\*\s*)?([A-Za-z0-9_]+)\s*\[[^\]]*\]\s*=\s*\{",
        "array table",
    )
});
/// `base->…->field(` or `(base[i].field)(` (`DISPATCH_RE`).
static DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"((?:[A-Za-z0-9_]+(?:\s*\[[^\]\[]*\])?\s*(?:->|\.)\s*)+)([A-Za-z0-9_]+)\s*\)?\s*\(",
        "dispatch",
    )
});
/// `tbl[i](` or `(*tbl[i])(` (`ARRAY_DISPATCH_RE`).
static ARRAY_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?:\(\s*\*\s*)?(?-u:\b)([A-Za-z0-9_]+)\s*\[[^\]\[]*\]\s*\)?\s*\(",
        "array dispatch",
    )
});
/// `a->f = b->g` (`FIELD_ASSIGN_RE`).
static FIELD_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"([A-Za-z0-9_]+)\s*(?:->|\.)\s*([A-Za-z0-9_]+)\s*=\s*([A-Za-z0-9_]+)\s*(?:->|\.)\s*([A-Za-z0-9_]+)",
        "field assignment",
    )
});
/// `#define NAME(params) expansion` (`parseFunctionMacros`); `(?mR)` reads
/// `\r`, `\n` and `\r\n` as line ends, as JavaScript's `m` does.
static FUNCTION_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        &format!(r"(?mR)^[ \t]*#[ \t]*define[ \t]+([A-Za-z0-9_]+)\(([^)]*)\)\s+({JS_DOT}+)$"),
        "function macro",
    )
});
/// `#define NAME value` (`parseObjectMacros`).
static OBJECT_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?mR)^[ \t]*#[ \t]*define[ \t]+([A-Za-z0-9_]+)[ \t]+(\S[^\n]*)$",
        "object macro",
    )
});
/// `#define NAME` of either kind (`parseDefinedNames`).
static DEFINED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?mR)^[ \t]*#[ \t]*define[ \t]+([A-Za-z0-9_]+)").expect("defined name")
});
/// A `\` line continuation.
static LINE_CONTINUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\\r?\n").expect("line continuation"));
/// Any `#if…` directive.
static HAS_IF: LazyLock<Regex> = LazyLock::new(|| js_regex(r"#\s*if", "has if"));
/// A directive that can change a struct body's fields between translation
/// units: a conditional or an `#include`.
static LAYOUT_DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?mR)^[ \t]*#[ \t]*(?:if|elif|else|endif|include)").expect("layout directive")
});
static IFDEF: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^#\s*ifdef\s+([A-Za-z0-9_]+)", "ifdef"));
static IFNDEF: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^#\s*ifndef\s+([A-Za-z0-9_]+)", "ifndef"));
static IF: LazyLock<Regex> = LazyLock::new(|| js_regex(&format!(r"^#\s*if\s+({JS_DOT}+)$"), "if"));
static ELIF: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^#\s*elif(?-u:\b)", "elif"));
static ELSE: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^#\s*else(?-u:\b)", "else"));
static ENDIF: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^#\s*endif(?-u:\b)", "endif"));
static DEFINED_EXPR: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"^defined\s*\(?\s*([A-Za-z0-9_]+)\s*\)?$",
        "defined expression",
    )
});
static NOT_DEFINED_EXPR: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"^!\s*defined\s*\(?\s*([A-Za-z0-9_]+)\s*\)?$",
        "not defined expression",
    )
});
/// An object macro's value that names a type (`resolveTypeName`).
static TYPE_ALIAS_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"^(?:(?:struct|union)\s+)?([A-Za-z0-9_]+)$",
        "type alias value",
    )
});
/// A whole word, as `substituteMacro` replaces them.
static WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)[A-Za-z0-9_]+(?-u:\b)").expect("word"));
/// A call `NAME(` that may be a macro's (`expandMacroCalls`).
static MACRO_CALL: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?-u:\b)([A-Za-z0-9_]+)\s*\(", "macro call"));
/// A field declaration's type and first name.
static FIRST_TYPED: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"([A-Za-z0-9_]+)\s+\**\s*([A-Za-z0-9_]+)\s*$",
        "first typed declarator",
    )
});
/// A later declarator of a shared type, `*name`.
static LATER_DECLARATOR: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^\**\s*([A-Za-z0-9_]+)", "later declarator"));
/// `.field = fn` in a struct value.
static DESIGNATED: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"^\.\s*([A-Za-z0-9_]+)\s*=\s*(?:&\s*)?([A-Za-z0-9_]+)\s*$",
        "designated item",
    )
});
/// A positional function, `fn` or `&fn`.
static POSITIONAL_FN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^&?\s*([A-Za-z0-9_]+)\s*$", "positional function"));
/// `[IDX] = value` in an array value.
static ARRAY_DESIGNATOR: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^\[[^\]]*\]\s*=\s*((?s:.)*)$", "array designator"));
/// A leading `(cast)` in an array value.
static ARRAY_CAST: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        &format!(
            r"^\((?:[A-Za-z0-9_*{}]+)\)\s*",
            &JS_SPACE[1..JS_SPACE.len() - 1]
        ),
        "array cast",
    )
});
/// A leading `&` in an array value.
static ARRAY_AMP: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^&\s*", "array address"));
/// A bare identifier.
static IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_]+)$").expect("identifier"));
/// An array subscript in a receiver chain.
static SUBSCRIPT: LazyLock<Regex> = LazyLock::new(|| js_regex(r"\s*\[[^\]]*\]", "subscript"));
/// The member access between a chain's segments.
static MEMBER_ACCESS: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"\s*(?:->|\.)\s*", "member access"));
/// The member access ending a dispatch's receiver.
static TRAILING_MEMBER_ACCESS: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"\s*(?:->|\.)\s*$", "trailing member access"));

/// A struct field in declaration order (`FieldInfo`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FieldInfo {
    name: String,
    index: usize,
    is_fn_ptr: bool,
    /// The declared type token, which a chained receiver walks; empty for a
    /// `(*name)(…)` declarator.
    ty: String,
    /// Whether `index` is the field's position in every translation unit:
    /// it is declared before the body's first conditional or `#include`.
    positional: bool,
}

/// A field as the sweep parses it, before the typedef sets are complete
/// (`RawFieldDecl`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawField {
    name: Option<String>,
    index: usize,
    ptr: bool,
    ty: String,
    positional: bool,
}

/// A function-like macro (`MacroDef`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MacroDef {
    params: Vec<String>,
    expansion: String,
}

type FnMacros = HashMap<String, Rc<MacroDef>>;
type ObjMacros = HashMap<String, Rc<str>>;

/// A macro table a unit reads: one map, or a [`Chain`] of them.
trait Lookup<T> {
    fn lookup(&self, name: &str) -> Option<&T>;
    fn holds_none(&self) -> bool;
}

impl<T> Lookup<T> for HashMap<String, T> {
    fn lookup(&self, name: &str) -> Option<&T> {
        self.get(name)
    }

    fn holds_none(&self) -> bool {
        self.is_empty()
    }
}

/// Maps read in order, the first holding a name winning: a file's macro
/// environment as `buildEnv` builds it (its own macros, then its includes'
/// in the order it visits them, the first definition of a name winning),
/// read without copying the maps, and an included unit's own macros
/// before its includer's (upstream sets them over a copy).
struct Chain<T> {
    maps: Vec<Rc<HashMap<String, T>>>,
}

impl<T> Chain<T> {
    fn new() -> Self {
        Self { maps: Vec::new() }
    }

    /// `first` read before every map of `self`.
    fn under(&self, first: Rc<HashMap<String, T>>) -> Self {
        let mut maps = Vec::with_capacity(self.maps.len() + 1);
        maps.push(first);
        maps.extend(self.maps.iter().cloned());
        Self { maps }
    }
}

impl<T> Lookup<T> for Chain<T> {
    fn lookup(&self, name: &str) -> Option<&T> {
        self.maps.iter().find_map(|map| map.get(name))
    }

    fn holds_none(&self) -> bool {
        self.maps.iter().all(|map| map.is_empty())
    }
}

/// The names a translation unit has defined: one set, or the sets of the
/// files its environment reads.
trait Defines {
    fn defines(&self, name: &str) -> bool;
}

impl Defines for HashSet<String> {
    fn defines(&self, name: &str) -> bool {
        self.contains(name)
    }
}

impl Defines for [Rc<HashSet<String>>] {
    fn defines(&self, name: &str) -> bool {
        self.iter().any(|set| set.contains(name))
    }
}

/// Ids in the order they were first added (a JavaScript `Set`).
#[derive(Debug, Clone, Default)]
struct OrderedIds {
    order: Vec<String>,
    members: HashSet<String>,
}

impl OrderedIds {
    fn insert(&mut self, id: &str) -> bool {
        if self.members.contains(id) {
            return false;
        }
        self.members.insert(id.to_string());
        self.order.push(id.to_string());
        true
    }
}

/// A text's line starts, to slice it by lines without copying.
struct Lines<'t> {
    text: &'t str,
    starts: Vec<usize>,
}

impl<'t> Lines<'t> {
    fn new(text: &'t str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Self { text, starts }
    }

    /// Lines `start..=end` (1-based) as they join back (`sliceLinesPre`):
    /// empty for a start line of 0.
    fn slice(&self, start: i64, end: i64) -> &'t str {
        if start < 1 {
            return "";
        }
        let from = (start - 1) as usize;
        let to = (end.max(0) as usize).min(self.starts.len());
        if from >= to {
            return "";
        }
        let stop = if to < self.starts.len() {
            self.starts[to] - 1
        } else {
            self.text.len()
        };
        &self.text[self.starts[from]..stop]
    }
}

/// Whether `text` holds an `=` whose next non-space character is `{`, as
/// every [`INIT`] and [`ARRAY_TABLE`] match ends.
fn holds_brace_initializer(text: &str) -> bool {
    text.match_indices('=').any(|(at, _)| {
        text[at + 1..]
            .trim_start_matches(is_js_space)
            .starts_with('{')
    })
}

/// The `}` matching the `{` at `open` (`matchBrace`).
fn match_brace(text: &str, open: usize) -> Option<usize> {
    match_pair(text, open, b'{', b'}')
}

/// The `)` matching the `(` at `open` (`matchParen`).
fn match_paren(text: &str, open: usize) -> Option<usize> {
    match_pair(text, open, b'(', b')')
}

fn match_pair(text: &str, open: usize, opener: u8, closer: u8) -> Option<usize> {
    let mut depth = 0i64;
    for (at, &byte) in text.as_bytes().iter().enumerate().skip(open) {
        if byte == opener {
            depth += 1;
        } else if byte == closer {
            depth -= 1;
            if depth == 0 {
                return Some(at);
            }
        }
    }
    None
}

/// `body` split at `separator` outside brackets (`splitTopLevel`); string
/// literals are not set apart.
fn split_top_level(body: &str, separator: u8) -> Vec<&str> {
    top_level_spans(body, separator)
        .into_iter()
        .map(|(start, end)| &body[start..end])
        .collect()
}

/// The byte ranges [`split_top_level`] splits `body` into.
fn top_level_spans(body: &str, separator: u8) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut start = 0;
    for (at, &byte) in body.as_bytes().iter().enumerate() {
        match byte {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            _ if byte == separator && depth == 0 => {
                out.push((start, at));
                start = at + 1;
            }
            _ => {}
        }
    }
    out.push((start, body.len()));
    out
}

/// A text's function-like macros (`parseFunctionMacros`): `\`-continued
/// lines joined, variadic ones skipped, a later definition replacing an
/// earlier one.
fn parse_function_macros(stripped: &str) -> FnMacros {
    let mut out = FnMacros::new();
    if !stripped.contains("#define") && !stripped.contains("# define") {
        return out;
    }
    let joined = LINE_CONTINUATION.replace_all(stripped, " ");
    for found in FUNCTION_MACRO.captures_iter(&joined) {
        let params: Vec<String> = found[2]
            .split(',')
            .map(|param| js_trim(param).to_string())
            .filter(|param| !param.is_empty())
            .collect();
        if params.iter().any(|param| param.ends_with("...")) {
            continue;
        }
        out.insert(
            found[1].to_string(),
            Rc::new(MacroDef {
                params,
                expansion: js_trim(&found[3]).to_string(),
            }),
        );
    }
    out
}

/// A text's object-like macros, `#define NAME value` (`parseObjectMacros`).
fn parse_object_macros(stripped: &str) -> ObjMacros {
    let mut out = ObjMacros::new();
    if !stripped.contains("#define") && !stripped.contains("# define") {
        return out;
    }
    let joined = LINE_CONTINUATION.replace_all(stripped, " ");
    for found in OBJECT_MACRO.captures_iter(&joined) {
        out.insert(found[1].to_string(), Rc::from(js_trim(&found[2])));
    }
    out
}

/// Every name a text `#define`s (`parseDefinedNames`).
fn parse_defined_names(stripped: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if !stripped.contains("#define") && !stripped.contains("# define") {
        return out;
    }
    for found in DEFINED_NAME.captures_iter(stripped) {
        out.insert(found[1].to_string());
    }
    out
}

/// `text` with the inactive arms of its conditionals blanked, lines kept
/// (`evalConditionals`): only `defined X` and `!defined X` are evaluated,
/// an expression it cannot evaluate keeps its arm, and an `#elif` is taken
/// when no arm before it was.
fn eval_conditionals(text: &str, defined: &(impl Defines + ?Sized)) -> String {
    if !HAS_IF.is_match(text) {
        return text.to_string();
    }
    struct Frame {
        parent_active: bool,
        active: bool,
        taken: bool,
    }
    fn active_now(stack: &[Frame]) -> bool {
        stack.last().is_none_or(|frame| frame.active)
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut out: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let trimmed = js_trim(line);
        // Every directive pattern below starts with `#`.
        if !trimmed.starts_with('#') {
            out.push(if active_now(&stack) { line } else { "" });
            continue;
        }
        let opened = if let Some(found) = IFDEF.captures(trimmed) {
            Some(defined.defines(&found[1]))
        } else if let Some(found) = IFNDEF.captures(trimmed) {
            Some(!defined.defines(&found[1]))
        } else {
            IF.captures(trimmed)
                .map(|found| condition_defined(js_trim(&found[1]), defined).unwrap_or(true))
        };
        if let Some(condition) = opened {
            let parent_active = active_now(&stack);
            stack.push(Frame {
                parent_active,
                active: parent_active && condition,
                taken: condition,
            });
            out.push("");
            continue;
        }
        if ELIF.is_match(trimmed) || ELSE.is_match(trimmed) {
            if let Some(top) = stack.last_mut() {
                top.active = top.parent_active && !top.taken;
                top.taken = true;
            }
            out.push("");
            continue;
        }
        if ENDIF.is_match(trimmed) {
            stack.pop();
            out.push("");
            continue;
        }
        out.push(if active_now(&stack) { line } else { "" });
    }
    out.join("\n")
}

/// `defined X` or `!defined X` against `defined`; `None` for any other
/// expression.
fn condition_defined(expression: &str, defined: &(impl Defines + ?Sized)) -> Option<bool> {
    if let Some(found) = DEFINED_EXPR.captures(expression) {
        return Some(defined.defines(&found[1]));
    }
    if let Some(found) = NOT_DEFINED_EXPR.captures(expression) {
        return Some(!defined.defines(&found[1]));
    }
    None
}

/// Every name [`eval_conditionals`] asks `defined` about in `text`, sorted:
/// its result depends on `defined` only through these.
fn condition_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    if !HAS_IF.is_match(text) {
        return names;
    }
    for line in text.split('\n') {
        let trimmed = js_trim(line);
        if !trimmed.starts_with('#') {
            continue;
        }
        if let Some(found) = IFDEF.captures(trimmed) {
            names.push(found[1].to_string());
        } else if let Some(found) = IFNDEF.captures(trimmed) {
            names.push(found[1].to_string());
        } else if let Some(found) = IF.captures(trimmed) {
            let expression = js_trim(&found[1]);
            if let Some(found) = DEFINED_EXPR.captures(expression) {
                names.push(found[1].to_string());
            } else if let Some(found) = NOT_DEFINED_EXPR.captures(expression) {
                names.push(found[1].to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// `name` read through object-macro aliases, at most
/// [`TYPE_ALIAS_HOPS`] of them (`resolveTypeName`).
fn resolve_type_name(name: &str, objects: &impl Lookup<Rc<str>>) -> String {
    let mut current = name.to_string();
    for _ in 0..TYPE_ALIAS_HOPS {
        let Some(value) = objects.lookup(&current) else {
            break;
        };
        let Some(found) = TYPE_ALIAS_VALUE.captures(js_trim(value)) else {
            break;
        };
        current = found[1].to_string();
    }
    current
}

/// A macro's expansion with each whole-word parameter replaced by its
/// argument, a missing one by nothing (`substituteMacro`).
fn substitute_macro(definition: &MacroDef, args: &[String]) -> String {
    let mut by_param: HashMap<&str, &str> = HashMap::new();
    for (index, param) in definition.params.iter().enumerate() {
        by_param.insert(param, args.get(index).map_or("", String::as_str));
    }
    WORD.replace_all(&definition.expansion, |found: &Captures<'_>| {
        by_param
            .get(&found[0])
            .map_or_else(|| found[0].to_string(), |arg| (*arg).to_string())
    })
    .into_owned()
}

/// `text` with known macro calls expanded, at most [`MACRO_EXPANSIONS`] of
/// them, each from the start again (`expandMacroCalls`).
fn expand_macro_calls(text: &str, env: &impl Lookup<Rc<MacroDef>>) -> String {
    if env.holds_none() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for _ in 0..MACRO_EXPANSIONS {
        let mut expanded = None;
        let mut at = 0;
        while let Some(found) = MACRO_CALL.captures_at(&out, at) {
            let whole = found.get(0).expect("match");
            at = whole.end();
            let Some(definition) = env.lookup(&found[1]) else {
                continue;
            };
            let open = whole.end() - 1;
            let Some(close) = match_paren(&out, open) else {
                continue;
            };
            let args: Vec<String> = split_top_level(&out[open + 1..close], b',')
                .into_iter()
                .map(|arg| js_trim(arg).to_string())
                .collect();
            expanded = Some(format!(
                "{}{}{}",
                &out[..whole.start()],
                substitute_macro(definition, &args),
                &out[close + 1..]
            ));
            break;
        }
        let Some(next) = expanded else {
            break;
        };
        out = next;
    }
    out
}

/// A struct body's fields, each declarator in its own position, one that
/// does not parse too (`parseStructFieldsRaw`).
fn parse_struct_fields_raw(inner: &str) -> Vec<RawField> {
    let mut fields = Vec::new();
    let mut index = 0;
    // KEEP-RUST: a conditional or an `#include` can change the fields after
    // it from one translation unit to the next.
    let first_directive = LAYOUT_DIRECTIVE.find(inner).map(|found| found.start());
    for (start, end) in top_level_spans(inner, b';') {
        let declaration = js_trim(&inner[start..end]);
        if declaration.is_empty() {
            continue;
        }
        let positional = first_directive.is_none_or(|at| end <= at);
        let parts = split_top_level(declaration, b',');
        let first_typed = FIRST_TYPED.captures(parts[0]);
        let shared_type = first_typed
            .as_ref()
            .map_or(String::new(), |found| found[1].to_string());
        for (part_index, raw_part) in parts.iter().enumerate() {
            let part = js_trim(raw_part);
            let mut field = RawField {
                name: None,
                index,
                ptr: false,
                ty: String::new(),
                positional,
            };
            if let Some(found) = FNPTR_DECL.captures(part) {
                field.name = Some(found[1].to_string());
                field.ptr = true;
            } else if part_index == 0 {
                if let Some(found) = &first_typed {
                    field.name = Some(found[2].to_string());
                    field.ty = shared_type.clone();
                }
            } else if let Some(found) = LATER_DECLARATOR.captures(part) {
                field.name = Some(found[1].to_string());
                field.ty = shared_type.clone();
            }
            fields.push(field);
            index += 1;
        }
    }
    fields
}

/// `path.posix.normalize` for the relative paths an include resolves.
pub(super) fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if !absolute {
                    segments.push("..");
                }
            }
            other => segments.push(other),
        }
    }
    let mut out = segments.join("/");
    if out.is_empty() {
        if absolute {
            return "/".to_string();
        }
        return if trailing {
            "./".to_string()
        } else {
            ".".to_string()
        };
    }
    if trailing {
        out.push('/');
    }
    if absolute { format!("/{out}") } else { out }
}

/// `path.posix.dirname` of a project path.
pub(super) fn posix_dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(at) => &path[..at],
        None => ".",
    }
}

/// The types a variable `name` is declared with in `source`, in the order
/// upstream's `(?:(?:struct|union)\s+)?(\w+)\s*\*?\s*\bNAME\b\s*(?:[,)=;]|\[)`
/// finds them (`recvTypeIn`, `varTypeIn`), without compiling a pattern per
/// name: each whole-word `name` followed by one of `,)=;[`, its type the
/// word before it across spaces and at most one `*`, a match never starting
/// inside the one before.
fn declared_types<'s>(source: &'s str, name: &str) -> Vec<&'s str> {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut last_end = 0;
    if name.is_empty() {
        return out;
    }
    for (at, _) in source.match_indices(name) {
        let after = at + name.len();
        if (at > 0 && is_word(bytes[at - 1])) || (after < bytes.len() && is_word(bytes[after])) {
            continue;
        }
        let rest = source[after..].trim_start_matches(is_js_space);
        if !rest.starts_with([',', ')', '=', ';', '[']) {
            continue;
        }
        let end = source.len() - rest.len() + 1;
        let before = source[..at].trim_end_matches(is_js_space);
        let before = before.strip_suffix('*').unwrap_or(before);
        let before = before.trim_end_matches(is_js_space);
        let type_start = before
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .len();
        if type_start == before.len() {
            continue;
        }
        // The match starts at a `struct ` or `union ` before the type when
        // one is there and after the previous match.
        let ahead = &source[..type_start];
        let keyword = ahead.trim_end_matches(is_js_space);
        let prefixed = (keyword.len() < ahead.len())
            .then(|| {
                ["struct", "union"]
                    .iter()
                    .find(|word| keyword.ends_with(**word))
                    .map(|word| keyword.len() - word.len())
            })
            .flatten()
            .filter(|start| *start >= last_end);
        if prefixed.unwrap_or(type_start) < last_end {
            continue;
        }
        out.push(&source[type_start..before.len()]);
        last_end = end;
    }
    out
}

/// A unit the registration stage reads: a file's text, or an included
/// file's as its includer sees it.
struct Unit<'u> {
    text: &'u str,
    /// The file the text is from.
    file: &'u str,
    /// The file whose translation unit reads it: `file` itself, or the
    /// file that includes it.
    includer: &'u str,
    functions: &'u Chain<Rc<MacroDef>>,
    objects: &'u Chain<Rc<str>>,
}

/// An included file's text as one includer's defined names leave it, and
/// its own macros read from that text.
struct Evaluated {
    text: String,
    functions: Rc<FnMacros>,
    objects: Rc<ObjMacros>,
}

/// One struct or union definition: a node's, or an inline struct's that no
/// node holds. Two definitions are two layouts even when their names and
/// fields agree.
#[derive(Debug)]
struct Layout {
    name: String,
    fields: Vec<FieldInfo>,
    /// The file defining it.
    file: String,
    has_fn_ptr: bool,
}

/// One run of the pass.
struct Pass<'v, 'a> {
    view: &'v View<'a>,
    /// The paths the pass reads, by path.
    files: Vec<String>,
    /// Every indexed path, which an include may name.
    indexed: HashSet<String>,
    stripped: HashMap<String, Option<Rc<str>>>,
    includes: HashMap<String, Rc<Vec<String>>>,
    translation_units: HashMap<String, Rc<HashSet<String>>>,
    function_macros: HashMap<String, Rc<FnMacros>>,
    object_macros: HashMap<String, Rc<ObjMacros>>,
    defined_names: HashMap<String, Rc<HashSet<String>>>,
    functions_named: HashMap<String, Rc<Vec<Node>>>,
    /// Each read file's nodes, by line and id.
    file_nodes: HashMap<String, Rc<Vec<Node>>>,
    /// Each included file's [`condition_names`] and `#if` presence.
    conditions: HashMap<String, Rc<(Vec<String>, bool)>>,
    /// Each included file evaluated, by the names of its conditions an
    /// includer defines: two includers defining the same of them read the
    /// same text.
    evaluated: HashMap<(String, Vec<String>), Rc<Evaluated>>,
    fn_ptr_typedefs: HashSet<String>,
    fn_type_typedefs: HashSet<String>,
    raw_fields: HashMap<String, Vec<RawField>>,
    layout_nodes: Vec<Node>,
    layouts: Vec<Layout>,
    /// The layouts of the nodes defining a name at a line of a file.
    node_layouts: HashMap<(String, String, i64), Vec<usize>>,
    /// The layouts of inline structs no node holds, by file, name and line.
    site_layouts: HashMap<(String, String, i64), usize>,
    layouts_named: HashMap<String, Vec<usize>>,
    /// Each function-pointer field name's layouts.
    field_owners: HashMap<String, BTreeSet<usize>>,
    /// Each file-scope table's file and layout, by variable name.
    global_tables: HashMap<String, Vec<(String, usize)>>,
    reg: HashMap<(usize, String), OrderedIds>,
    array_reg: HashMap<String, Vec<(String, OrderedIds)>>,
    /// A function-pointer field name of a layout, any of them (built after
    /// registration): a body holding none dispatches through no field.
    owner_field_names: Names,
}

/// The dispatch edges C and C++ function pointers carry
/// (`cFnPointerDispatchEdges`).
pub(super) fn c_fn_pointer_dispatch_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let all = view.files()?;
    let files: Vec<String> = all
        .iter()
        .filter(|path| has_extension(path, &C_CPP_EXTENSIONS))
        .cloned()
        .collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut pass = Pass {
        view,
        files,
        indexed: all.into_iter().collect(),
        stripped: HashMap::new(),
        includes: HashMap::new(),
        translation_units: HashMap::new(),
        function_macros: HashMap::new(),
        object_macros: HashMap::new(),
        defined_names: HashMap::new(),
        functions_named: HashMap::new(),
        file_nodes: HashMap::new(),
        conditions: HashMap::new(),
        evaluated: HashMap::new(),
        fn_ptr_typedefs: HashSet::new(),
        fn_type_typedefs: HashSet::new(),
        raw_fields: HashMap::new(),
        layout_nodes: Vec::new(),
        layouts: Vec::new(),
        node_layouts: HashMap::new(),
        site_layouts: HashMap::new(),
        layouts_named: HashMap::new(),
        field_owners: HashMap::new(),
        global_tables: HashMap::new(),
        reg: HashMap::new(),
        array_reg: HashMap::new(),
        owner_field_names: Names::None,
    };
    pass.sweep()?;
    pass.link_layouts();
    pass.register()?;
    pass.owner_field_names = Names::new(pass.field_owners.keys());
    pass.propagate();
    if pass.reg.is_empty() && pass.array_reg.is_empty() {
        return Ok(Vec::new());
    }
    Ok(pass.dispatch())
}

impl Pass<'_, '_> {
    /// `file`'s text: an indexed file's as the index holds it, any other's
    /// as the stage reads the files the index does not hold (W7-17b).
    fn raw(&self, file: &str) -> Option<Arc<str>> {
        if self.is_indexed(file) {
            self.view.text(file)
        } else {
            self.view.unindexed_text(file)
        }
    }

    /// `file`'s text with its comments blanked, once a pass (`src`); `None`
    /// for an unreadable or empty file.
    fn src(&mut self, file: &str) -> Option<Rc<str>> {
        if let Some(cached) = self.stripped.get(file) {
            return cached.clone();
        }
        let text = self
            .raw(file)
            .filter(|raw| !raw.is_empty())
            .map(|raw| Rc::from(strip_comments_for_regex(&raw, CommentLang::Cpp)));
        self.stripped.insert(file.to_string(), text.clone());
        text
    }

    /// The path `#include "inc"` in `includer` names (`resolveInclude`):
    /// the includer's directory's first, then `inc` itself, each an indexed
    /// file or else a file the stage reads beside the index (upstream's
    /// `fileExists`: the known files, then the disk), by its project-relative
    /// form (W7-17b).
    fn resolve_include(&self, includer: &str, inc: &str) -> Option<String> {
        let dir = posix_dirname(&includer.replace('\\', "/")).to_string();
        let candidate = posix_normalize(&format!("{dir}/{inc}"));
        for path in [candidate.as_str(), inc] {
            if self.is_indexed(path) {
                return Some(path.to_string());
            }
            let Some(relative) = View::unindexed_path(path) else {
                continue;
            };
            if self.is_indexed(&relative) {
                return Some(relative);
            }
            if self.view.unindexed_text(&relative).is_some() {
                return Some(relative);
            }
        }
        None
    }

    fn is_indexed(&self, path: &str) -> bool {
        self.indexed.contains(path) || self.indexed.contains(&path.replace('\\', "/"))
    }

    /// The files `file` includes, in source order, read from its raw text
    /// (`scanIncludes`, `localIncludesOf`).
    fn local_includes_of(&mut self, file: &str) -> Rc<Vec<String>> {
        if let Some(cached) = self.includes.get(file) {
            return cached.clone();
        }
        let mut out = Vec::new();
        if let Some(raw) = self.raw(file)
            && raw.contains("include")
        {
            for found in INCLUDE.captures_iter(&raw) {
                if !has_extension(&found[1], &INCLUDABLE_EXTENSIONS) {
                    continue;
                }
                if let Some(target) = self.resolve_include(file, &found[1]) {
                    out.push(target);
                }
            }
        }
        let out = Rc::new(out);
        self.includes.insert(file.to_string(), out.clone());
        out
    }

    /// `file` and every file its includes reach.
    fn translation_unit(&mut self, file: &str) -> Rc<HashSet<String>> {
        if let Some(cached) = self.translation_units.get(file) {
            return cached.clone();
        }
        let mut unit: HashSet<String> = HashSet::new();
        let mut queue = vec![file.to_string()];
        while let Some(next) = queue.pop() {
            if !unit.insert(next.clone()) {
                continue;
            }
            for include in self.local_includes_of(&next).iter() {
                if !unit.contains(include) {
                    queue.push(include.clone());
                }
            }
        }
        let unit = Rc::new(unit);
        self.translation_units
            .insert(file.to_string(), unit.clone());
        unit
    }

    fn file_function_macros(&mut self, file: &str) -> Rc<FnMacros> {
        if let Some(cached) = self.function_macros.get(file) {
            return cached.clone();
        }
        let text = self.src(file);
        let parsed = Rc::new(parse_function_macros(text.as_deref().unwrap_or("")));
        self.function_macros
            .insert(file.to_string(), parsed.clone());
        parsed
    }

    fn file_object_macros(&mut self, file: &str) -> Rc<ObjMacros> {
        if let Some(cached) = self.object_macros.get(file) {
            return cached.clone();
        }
        let text = self.src(file);
        let parsed = Rc::new(parse_object_macros(text.as_deref().unwrap_or("")));
        self.object_macros.insert(file.to_string(), parsed.clone());
        parsed
    }

    fn file_defined_names(&mut self, file: &str) -> Rc<HashSet<String>> {
        if let Some(cached) = self.defined_names.get(file) {
            return cached.clone();
        }
        let text = self.src(file);
        let parsed = Rc::new(parse_defined_names(text.as_deref().unwrap_or("")));
        self.defined_names.insert(file.to_string(), parsed.clone());
        parsed
    }

    /// `file`'s macro environment: its own macros and defined names, then
    /// its includes' to [`ENV_DEPTH`] levels, the first definition of a
    /// name winning (`buildEnv`), as chains in the order it visits them.
    fn build_env(
        &mut self,
        file: &str,
        depth: i32,
        seen: &mut HashSet<String>,
        functions: &mut Chain<Rc<MacroDef>>,
        objects: &mut Chain<Rc<str>>,
        defined: &mut Vec<Rc<HashSet<String>>>,
    ) {
        if depth < 0 || !seen.insert(file.to_string()) {
            return;
        }
        functions.maps.push(self.file_function_macros(file));
        objects.maps.push(self.file_object_macros(file));
        defined.push(self.file_defined_names(file));
        for include in self.local_includes_of(file).iter() {
            self.build_env(include, depth - 1, seen, functions, objects, defined);
        }
    }

    /// `target` evaluated against `defined` (`evalConditionals`) with its
    /// own macros read from the result, once per set of its condition
    /// names `defined` holds.
    fn evaluated(
        &mut self,
        target: &str,
        included: &str,
        defined: &[Rc<HashSet<String>>],
    ) -> Rc<Evaluated> {
        let names = self.conditions_of(target, included);
        let key = (
            target.to_string(),
            names
                .0
                .iter()
                .filter(|name| defined.defines(name))
                .cloned()
                .collect::<Vec<String>>(),
        );
        if let Some(cached) = self.evaluated.get(&key) {
            return cached.clone();
        }
        let text = eval_conditionals(included, defined);
        let functions = Rc::new(parse_function_macros(&text));
        let objects = Rc::new(parse_object_macros(&text));
        let evaluated = Rc::new(Evaluated {
            text,
            functions,
            objects,
        });
        self.evaluated.insert(key, evaluated.clone());
        evaluated
    }

    /// `target`'s condition names and whether it has an `#if`.
    fn conditions_of(&mut self, target: &str, included: &str) -> Rc<(Vec<String>, bool)> {
        if let Some(cached) = self.conditions.get(target) {
            return cached.clone();
        }
        let facts = Rc::new((condition_names(included), HAS_IF.is_match(included)));
        self.conditions.insert(target.to_string(), facts.clone());
        facts
    }

    /// Stage A: the typedefs and each struct and union node's fields.
    fn sweep(&mut self) -> Result<()> {
        for file in self.files.clone() {
            let Some(text) = self.src(&file) else {
                continue;
            };
            if text.contains("typedef") {
                for found in FNPTR_TYPEDEF.captures_iter(&text) {
                    self.fn_ptr_typedefs.insert(found[1].to_string());
                }
                for found in TYPEDEF_STATEMENT.captures_iter(&text) {
                    let guts = &found[1];
                    if guts.contains("(*") || guts.contains("( *") {
                        continue;
                    }
                    if let Some(name) = WORD_BEFORE_PAREN.captures(guts)
                        && !C_TYPE_KEYWORDS.contains(&&name[1])
                    {
                        self.fn_type_typedefs.insert(name[1].to_string());
                    }
                }
            }
            // The pass reads no other kind of the file's nodes.
            let nodes = self.view.layouts_and_callables_in_file(&file)?;
            let lines = Lines::new(&text);
            for node in &nodes {
                if !matches!(node.kind, NodeKind::Struct | NodeKind::Union) {
                    continue;
                }
                let body = lines.slice(node.start_line, node.end_line);
                let Some(open) = body.find('{') else {
                    continue;
                };
                let Some(close) = match_brace(body, open) else {
                    continue;
                };
                self.raw_fields.insert(
                    node.id.clone(),
                    parse_struct_fields_raw(&body[open + 1..close]),
                );
                self.layout_nodes.push(node.clone());
            }
            self.file_nodes.insert(file, Rc::new(nodes));
        }
        Ok(())
    }

    fn classify(&self, raw: &[RawField]) -> Vec<FieldInfo> {
        raw.iter()
            .map(|field| FieldInfo {
                name: field.name.clone().unwrap_or_default(),
                index: field.index,
                is_fn_ptr: field.name.is_some()
                    && (field.ptr
                        || (!field.ty.is_empty()
                            && (self.fn_ptr_typedefs.contains(&field.ty)
                                || self.fn_type_typedefs.contains(&field.ty)))),
                ty: field.ty.clone(),
                positional: field.positional,
            })
            .collect()
    }

    /// A new layout: the definition of `name` with `fields` in `file`.
    fn add_layout(&mut self, name: &str, fields: Vec<FieldInfo>, file: &str) -> usize {
        let id = self.layouts.len();
        for field in &fields {
            if !field.name.is_empty() && field.is_fn_ptr {
                self.field_owners
                    .entry(field.name.clone())
                    .or_default()
                    .insert(id);
            }
        }
        self.layouts.push(Layout {
            name: name.to_string(),
            has_fn_ptr: fields.iter().any(|field| field.is_fn_ptr),
            fields,
            file: file.to_string(),
        });
        self.layouts_named
            .entry(name.to_string())
            .or_default()
            .push(id);
        id
    }

    /// Stage B: every struct node's layout, then every union node's, each in
    /// node order.
    fn link_layouts(&mut self) {
        let mut nodes = std::mem::take(&mut self.layout_nodes);
        nodes.sort_by(|a, b| {
            (a.kind != NodeKind::Struct)
                .cmp(&(b.kind != NodeKind::Struct))
                .then_with(|| node_order(a, b))
        });
        for node in &nodes {
            let Some(raw) = self.raw_fields.get(&node.id) else {
                continue;
            };
            let fields = self.classify(raw);
            let id = self.add_layout(&node.name, fields, &node.file_path);
            self.node_layouts
                .entry((node.file_path.clone(), node.name.clone(), node.start_line))
                .or_default()
                .push(id);
        }
        self.raw_fields.clear();
    }

    /// Whether some layout named `name` has a function pointer.
    fn has_fn_ptr_layout_named(&self, name: &str) -> bool {
        self.layouts_named
            .get(name)
            .is_some_and(|ids| ids.iter().any(|&id| self.layouts[id].has_fn_ptr))
    }

    /// The layout `name` means in `file` (KEEP-RUST, fail closed): the only
    /// one of that name in the project, else the only one defined in
    /// `file`'s translation unit; `fn_ptr_only` reads only layouts with a
    /// function pointer.
    fn resolve_layout(&mut self, name: &str, file: &str, fn_ptr_only: bool) -> Option<usize> {
        let candidates: Vec<usize> = self
            .layouts_named
            .get(name)?
            .iter()
            .copied()
            .filter(|&id| !fn_ptr_only || self.layouts[id].has_fn_ptr)
            .collect();
        match candidates.as_slice() {
            [] => None,
            [only] => Some(*only),
            _ => {
                let unit = self.translation_unit(file);
                let visible: Vec<usize> = candidates
                    .into_iter()
                    .filter(|&id| unit.contains(&self.layouts[id].file))
                    .collect();
                (visible.len() == 1).then(|| visible[0])
            }
        }
    }

    /// The layout a file-scope table `variable` has in `file`: the only one
    /// the name has in the project, else the only one among the tables of
    /// `file`'s translation unit.
    fn global_table_layout(&mut self, variable: &str, file: &str) -> Option<usize> {
        let entries = self.global_tables.get(variable)?.clone();
        let layouts: BTreeSet<usize> = entries.iter().map(|(_, id)| *id).collect();
        if layouts.len() == 1 {
            return layouts.into_iter().next();
        }
        let unit = self.translation_unit(file);
        let visible: BTreeSet<usize> = entries
            .iter()
            .filter(|(defining, _)| unit.contains(defining))
            .map(|(_, id)| *id)
            .collect();
        (visible.len() == 1)
            .then(|| visible.into_iter().next())
            .flatten()
    }

    fn add_global_table(&mut self, variable: &str, file: &str, layout: usize) {
        let entries = self.global_tables.entry(variable.to_string()).or_default();
        if !entries
            .iter()
            .any(|(defining, id)| defining == file && *id == layout)
        {
            entries.push((file.to_string(), layout));
        }
    }

    fn fn_ptr_field_of(&self, layout: usize, field: &str) -> bool {
        self.layouts[layout]
            .fields
            .iter()
            .any(|f| f.name == field && f.is_fn_ptr)
    }

    /// The function or method a table entry names (KEEP-RUST, fail closed,
    /// as W7-14's `unique_handler`): the only one in the unit's file, else
    /// the only one in its includer's, else the only one in the project;
    /// none when the name is ambiguous where it is looked up.
    fn resolve_fn(&mut self, name: &str, file: &str, includer: &str) -> Result<Option<String>> {
        let candidates = match self.functions_named.get(name) {
            Some(cached) => cached.clone(),
            None => {
                let found: Vec<Node> = self
                    .view
                    .nodes_named(name)?
                    .into_iter()
                    .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
                    .collect();
                let found = Rc::new(found);
                self.functions_named.insert(name.to_string(), found.clone());
                found
            }
        };
        for scope in [file, includer] {
            let in_scope: Vec<&Node> = candidates
                .iter()
                .filter(|node| node.file_path == scope)
                .collect();
            match in_scope.as_slice() {
                [] => {}
                [only] => return Ok(Some(only.id.clone())),
                _ => return Ok(None),
            }
        }
        Ok(match candidates.as_slice() {
            [only] => Some(only.id.clone()),
            _ => None,
        })
    }

    fn add_reg(&mut self, layout: usize, field: &str, id: &str) {
        self.reg
            .entry((layout, field.to_string()))
            .or_default()
            .insert(id);
    }

    fn add_array_reg(&mut self, name: &str, file: &str, id: &str) {
        let entries = self.array_reg.entry(name.to_string()).or_default();
        let at = match entries
            .iter()
            .position(|(entry_file, _)| entry_file == file)
        {
            Some(at) => at,
            None => {
                entries.push((file.to_string(), OrderedIds::default()));
                entries.len() - 1
            }
        };
        entries[at].1.insert(id);
    }

    /// One struct value: designated `.field = fn` items, which keep the
    /// position, and positional ones by field index (`registerStructValue`).
    fn register_struct_value(&mut self, layout: usize, value: &str, unit: &Unit<'_>) -> Result<()> {
        let fields = self.layouts[layout].fields.clone();
        let expanded = expand_macro_calls(value, unit.functions);
        let mut body = js_trim(&expanded);
        if body.starts_with('{')
            && let Some(end) = match_brace(body, 0)
            && end > 0
            && js_trim(&body[end + 1..]).is_empty()
        {
            body = &body[1..end];
        }
        let mut position = 0;
        for raw_item in split_top_level(body, b',') {
            let item = js_trim(raw_item);
            if item.is_empty() {
                continue;
            }
            if let Some(found) = DESIGNATED.captures(item) {
                if self.fn_ptr_field_of(layout, &found[1])
                    && let Some(id) = self.resolve_fn(&found[2], unit.file, unit.includer)?
                {
                    self.add_reg(layout, &found[1], &id);
                }
                continue;
            }
            if let Some(field) = fields.iter().find(|field| field.index == position)
                && field.is_fn_ptr
                && field.positional
                && let Some(found) = POSITIONAL_FN.captures(item)
                && let Some(id) = self.resolve_fn(&found[1], unit.file, unit.includer)?
            {
                self.add_reg(layout, &field.name, &id);
            }
            position += 1;
        }
        Ok(())
    }

    /// An array of function pointers: each literal function, after an
    /// `[IDX] =`, a cast or a `&` (`registerArrayValue`).
    fn register_array_value(&mut self, name: &str, body: &str, unit: &Unit<'_>) -> Result<()> {
        let body = expand_macro_calls(body, unit.functions);
        for raw_item in split_top_level(&body, b',') {
            let mut item = js_trim(raw_item).to_string();
            if item.is_empty() {
                continue;
            }
            if let Some(found) = ARRAY_DESIGNATOR.captures(&item) {
                item = js_trim(&found[1]).to_string();
            }
            let item = ARRAY_CAST.replace(&item, "").into_owned();
            let item = ARRAY_AMP.replace(&item, "").into_owned();
            let item = js_trim(&item);
            if let Some(found) = IDENTIFIER.captures(item)
                && let Some(id) = self.resolve_fn(&found[1], unit.file, unit.includer)?
            {
                self.add_array_reg(name, unit.file, &id);
            }
        }
        Ok(())
    }

    /// A `{ … }` initializer: an array's elements or one struct value
    /// (`processInit`).
    fn process_init(
        &mut self,
        layout: usize,
        body: &str,
        is_array: bool,
        unit: &Unit<'_>,
    ) -> Result<()> {
        if !is_array {
            return self.register_struct_value(layout, body, unit);
        }
        for element in split_top_level(body, b',') {
            let element = js_trim(element);
            if element.starts_with('{') {
                if let Some(end) = match_brace(element, 0)
                    && end > 0
                {
                    self.register_struct_value(layout, &element[1..end], unit)?;
                }
            } else if !element.is_empty() {
                self.register_struct_value(layout, element, unit)?;
            }
        }
        Ok(())
    }

    /// The definition an inline `struct TAG {` at `line`, column `column`
    /// of `unit` is: the only node of that name starting there, else the
    /// layout first read at that place, its fields as the file writes them;
    /// none when several nodes start there or the file's text has no such
    /// struct there.
    fn inline_layout(
        &mut self,
        tag: &str,
        line: i64,
        column: usize,
        unit: &Unit<'_>,
    ) -> Option<usize> {
        let key = (unit.file.to_string(), tag.to_string(), line);
        match self.node_layouts.get(&key).map(Vec::as_slice) {
            Some([only]) => return Some(*only),
            Some([_, _, ..]) => return None,
            _ => {}
        }
        if let Some(&id) = self.site_layouts.get(&key) {
            return Some(id);
        }
        let fields = self.written_fields(unit.file, tag, line, column)?;
        let id = self.add_layout(tag, fields, unit.file);
        self.site_layouts.insert(key, id);
        Some(id)
    }

    /// The fields of the `struct TAG {` that `file` writes at `line`,
    /// `column`, conditionals unevaluated.
    fn written_fields(
        &mut self,
        file: &str,
        tag: &str,
        line: i64,
        column: usize,
    ) -> Option<Vec<FieldInfo>> {
        let text = self.src(file)?;
        let line_start = if line <= 1 {
            0
        } else {
            text.match_indices('\n').nth((line - 2) as usize)?.0 + 1
        };
        let at = line_start + column;
        let found = INLINE_STRUCT.captures_at(&text, at)?;
        let whole = found.get(0).expect("match");
        if whole.start() != at || &found[1] != tag {
            return None;
        }
        let open = whole.end() - 1;
        let close = match_brace(&text, open)?;
        Some(self.classify(&parse_struct_fields_raw(&text[open + 1..close])))
    }

    /// One unit's tables (`processUnit`): inline struct tables, struct
    /// initializers and bare arrays of function pointers.
    fn process_unit(&mut self, unit: &Unit<'_>) -> Result<()> {
        let text = unit.text;
        if text.is_empty() || !text.contains('{') {
            return Ok(());
        }
        let mut lines: Option<LineAt> = None;
        let mut at = 0;
        while let Some(found) = INLINE_STRUCT.captures_at(text, at) {
            let whole = found.get(0).expect("match");
            at = whole.end();
            let open = whole.end() - 1;
            let Some(close) = match_brace(text, open) else {
                continue;
            };
            let after = &text[close + 1..];
            let Some(table) = INLINE_TABLE_VAR.captures(after) else {
                continue;
            };
            let fields = self.classify(&parse_struct_fields_raw(&text[open + 1..close]));
            if !fields.iter().any(|field| field.is_fn_ptr) {
                continue;
            }
            let line = lines
                .get_or_insert_with(|| LineAt::new(text, 1))
                .line(whole.start());
            let column = whole.start() - text[..whole.start()].rfind('\n').map_or(0, |at| at + 1);
            let layout = self.inline_layout(&found[1], line, column, unit);
            if let Some(layout) = layout {
                self.add_global_table(&table[1], unit.file, layout);
            }
            if table.get(3).is_some() {
                let table_open = close + table.get(0).expect("match").end();
                if let Some(table_close) = match_brace(text, table_open)
                    && table_close > 0
                {
                    // An unresolved table is skipped as upstream skips a
                    // read one.
                    if let Some(layout) = layout {
                        self.process_init(
                            layout,
                            &text[table_open + 1..table_close],
                            table.get(2).is_some(),
                            unit,
                        )?;
                    }
                    at = table_close;
                }
            }
        }
        // Every initializer and array table ends `= {`.
        if !holds_brace_initializer(text) {
            return Ok(());
        }
        let mut at = 0;
        while let Some(found) = INIT.captures_at(text, at) {
            let whole = found.get(0).expect("match");
            at = whole.end();
            let mut type_name = found[1].to_string();
            if !self.has_fn_ptr_layout_named(&type_name) {
                type_name = resolve_type_name(&type_name, unit.objects);
            }
            if !self.has_fn_ptr_layout_named(&type_name) {
                continue;
            }
            let open = whole.end() - 1;
            let Some(close) = match_brace(text, open) else {
                continue;
            };
            at = close;
            let Some(layout) = self.resolve_layout(&type_name, unit.includer, true) else {
                continue;
            };
            self.add_global_table(&found[2], unit.file, layout);
            self.process_init(layout, &text[open + 1..close], found.get(3).is_some(), unit)?;
        }
        let mut at = 0;
        while let Some(found) = ARRAY_TABLE.captures_at(text, at) {
            let whole = found.get(0).expect("match");
            at = whole.end();
            let element = &found[1];
            let starred = found.get(2).is_some();
            if !((starred && self.fn_type_typedefs.contains(element))
                || self.fn_ptr_typedefs.contains(element))
            {
                continue;
            }
            let open = whole.end() - 1;
            let Some(close) = match_brace(text, open) else {
                continue;
            };
            self.register_array_value(&found[3], &text[open + 1..close], unit)?;
            at = close;
        }
        Ok(())
    }

    /// Stage C: each file's unit, then each file it includes that it can
    /// change: a file outside the read paths always, a read one only when
    /// the includer defines a name it does not and it has an `#if`, read
    /// with its conditionals evaluated against the includer's names and its
    /// own macros from that text.
    fn register(&mut self) -> Result<()> {
        let mut seen_includes: HashSet<(String, String)> = HashSet::new();
        let read: HashSet<String> = self.files.iter().cloned().collect();
        for file in self.files.clone() {
            let mut functions = Chain::new();
            let mut objects = Chain::new();
            let mut defined = Vec::new();
            self.build_env(
                &file,
                ENV_DEPTH,
                &mut HashSet::new(),
                &mut functions,
                &mut objects,
                &mut defined,
            );
            if let Some(text) = self.src(&file) {
                self.process_unit(&Unit {
                    text: &text,
                    file: &file,
                    includer: &file,
                    functions: &functions,
                    objects: &objects,
                })?;
            }
            for target in self.local_includes_of(&file).iter() {
                if seen_includes.contains(&(file.clone(), target.clone())) {
                    continue;
                }
                let Some(included) = self.src(target) else {
                    continue;
                };
                if read.contains(target) {
                    let own = self.file_defined_names(target);
                    if !defined
                        .iter()
                        .any(|set| set.iter().any(|name| !own.contains(name)))
                        || !self.conditions_of(target, &included).1
                    {
                        continue;
                    }
                }
                seen_includes.insert((file.clone(), target.clone()));
                let evaluated = self.evaluated(target, &included, &defined);
                let unit_functions = functions.under(evaluated.functions.clone());
                let unit_objects = objects.under(evaluated.objects.clone());
                self.process_unit(&Unit {
                    text: &evaluated.text,
                    file: target,
                    includer: &file,
                    functions: &unit_functions,
                    objects: &unit_objects,
                })?;
            }
        }
        Ok(())
    }

    /// The layout of `receiver` in `source` in `file`: its first declared
    /// type that names a layout with a function pointer, resolved there
    /// (`recvTypeIn`).
    fn receiver_layout(&mut self, source: &str, receiver: &str, file: &str) -> Option<usize> {
        let name = declared_types(source, receiver)
            .into_iter()
            .find(|ty| self.has_fn_ptr_layout_named(ty))?
            .to_string();
        self.resolve_layout(&name, file, true)
    }

    /// The layout of `variable` in `source` in `file`: its first declared
    /// type that is not a C keyword, else a file-scope table's
    /// (`varTypeIn`).
    fn variable_layout(&mut self, source: &str, variable: &str, file: &str) -> Option<usize> {
        match declared_types(source, variable)
            .into_iter()
            .find(|ty| !C_TYPE_KEYWORDS.contains(ty))
        {
            Some(name) => {
                let name = name.to_string();
                self.resolve_layout(&name, file, false)
            }
            None => self.global_table_layout(variable, file),
        }
    }

    /// The layout a member chain such as `c->cmd` is in `file`, each
    /// segment's field type resolved there (`resolveChainType`).
    fn chain_layout(&mut self, source: &str, chain: &str, file: &str) -> Option<usize> {
        let without_subscripts = SUBSCRIPT.replace_all(chain, "");
        let segments: Vec<String> = MEMBER_ACCESS
            .split(&without_subscripts)
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect();
        let first = segments.first()?;
        let mut current = self.variable_layout(source, first, file)?;
        for segment in &segments[1..] {
            // A field the body declares twice, in two arms of a
            // conditional, has no one type.
            let mut named = self.layouts[current]
                .fields
                .iter()
                .filter(|field| field.name == *segment);
            let field = named.next()?;
            if named.next().is_some() || field.ty.is_empty() {
                return None;
            }
            let ty = field.ty.clone();
            current = self.resolve_layout(&ty, file, false)?;
        }
        Some(current)
    }

    /// Stage D: `a->f = b->g` merges `B.g`'s functions into `A.f`, up to
    /// [`PROPAGATION_ROUNDS`] rounds.
    fn propagate(&mut self) {
        let mut propagations: Vec<((usize, String), (usize, String))> = Vec::new();
        for file in self.files.clone() {
            let Some(text) = self.src(&file) else {
                continue;
            };
            if !text.contains('=') {
                continue;
            }
            let names = self.owner_field_names.clone();
            if matches!(names, Names::None) {
                break;
            }
            if !names.in_text(&text) {
                continue;
            }
            let lines = Lines::new(&text);
            let Some(nodes) = self.file_nodes.get(&file).cloned() else {
                continue;
            };
            for node in nodes.iter() {
                if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                    continue;
                }
                let body = lines.slice(node.start_line, node.end_line);
                // Both sides' fields are function-pointer fields.
                if !body.contains('=') || !names.in_text(body) {
                    continue;
                }
                for found in FIELD_ASSIGN.captures_iter(body) {
                    let (left_field, right_field) = (&found[2], &found[4]);
                    if !self.field_owners.contains_key(left_field)
                        || !self.field_owners.contains_key(right_field)
                    {
                        continue;
                    }
                    let left = self.receiver_layout(body, &found[1], &node.file_path);
                    let right = self.receiver_layout(body, &found[3], &node.file_path);
                    if let (Some(left), Some(right)) = (left, right)
                        && self.fn_ptr_field_of(left, left_field)
                        && self.fn_ptr_field_of(right, right_field)
                    {
                        propagations.push((
                            (left, left_field.to_string()),
                            (right, right_field.to_string()),
                        ));
                    }
                }
            }
        }
        for _ in 0..PROPAGATION_ROUNDS {
            if propagations.is_empty() {
                break;
            }
            let mut changed = false;
            for (to, from) in &propagations {
                let Some(from_ids) = self.reg.get(from).map(|ids| ids.order.clone()) else {
                    continue;
                };
                let to_ids = self.reg.entry(to.clone()).or_default();
                for id in &from_ids {
                    changed |= to_ids.insert(id);
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Stage E: each function's dispatch sites, through a struct field and
    /// through a bare array.
    fn dispatch(&mut self) -> Vec<Edge> {
        let mut edges = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let field_names = self.owner_field_names.clone();
        let array_names = Names::new(self.array_reg.keys());
        for file in self.files.clone() {
            let Some(text) = self.src(&file) else {
                continue;
            };
            let in_text = |names: &Names| names.in_text(&text);
            if !in_text(&field_names) && !in_text(&array_names) {
                continue;
            }
            let lines = Lines::new(&text);
            let Some(nodes) = self.file_nodes.get(&file).cloned() else {
                continue;
            };
            for node in nodes.iter() {
                if !matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                    continue;
                }
                let body = lines.slice(node.start_line, node.end_line);
                let in_body = |names: &Names| names.in_text(body);
                let mut line_at: Option<LineAt> = None;
                let mut added = 0;
                // A match whose field no layout owns adds nothing.
                let fields_here = in_body(&field_names);
                let dispatches = fields_here
                    .then(|| DISPATCH.captures_iter(body))
                    .into_iter()
                    .flatten();
                for found in dispatches {
                    if added >= FANOUT_CAP {
                        break;
                    }
                    let base = TRAILING_MEMBER_ACCESS.replace(&found[1], "");
                    let base = js_trim(&base);
                    let field = &found[2];
                    let Some(owners) = self.field_owners.get(field).cloned() else {
                        continue;
                    };
                    if owners.is_empty() {
                        continue;
                    }
                    let mut owner = self
                        .chain_layout(body, base, &node.file_path)
                        .filter(|id| owners.contains(id));
                    if owner.is_none() {
                        let without_subscripts = SUBSCRIPT.replace_all(base, "");
                        let last = MEMBER_ACCESS
                            .split(&without_subscripts)
                            .last()
                            .unwrap_or("")
                            .to_string();
                        owner = self
                            .receiver_layout(body, &last, &node.file_path)
                            .filter(|id| owners.contains(id));
                    }
                    if owner.is_none() && owners.len() == 1 {
                        owner = owners.iter().next().copied();
                    }
                    let Some(owner) = owner else {
                        continue;
                    };
                    let Some(targets) = self.reg.get(&(owner, field.to_string())) else {
                        continue;
                    };
                    let via = format!("{}.{field}", self.layouts[owner].name);
                    let line = line_at
                        .get_or_insert_with(|| LineAt::new(body, node.start_line))
                        .line(found.get(0).expect("match").start());
                    for target in &targets.order {
                        if *target == node.id || !seen.insert((node.id.clone(), target.clone())) {
                            continue;
                        }
                        edges.push(dispatch_edge(node, target, line, &via));
                        added += 1;
                        if added >= FANOUT_CAP {
                            break;
                        }
                    }
                }
                if self.array_reg.is_empty() || added >= FANOUT_CAP || !in_body(&array_names) {
                    continue;
                }
                for found in ARRAY_DISPATCH.captures_iter(body) {
                    if added >= FANOUT_CAP {
                        break;
                    }
                    let Some(entries) = self.array_reg.get(&found[1]) else {
                        continue;
                    };
                    let ids = if entries.len() == 1 {
                        Some(&entries[0].1)
                    } else {
                        entries
                            .iter()
                            .find(|(entry_file, _)| *entry_file == node.file_path)
                            .map(|(_, ids)| ids)
                    };
                    let Some(ids) = ids else {
                        continue;
                    };
                    let via = format!("{}[]", &found[1]);
                    let line = line_at
                        .get_or_insert_with(|| LineAt::new(body, node.start_line))
                        .line(found.get(0).expect("match").start());
                    for target in &ids.order {
                        if *target == node.id || !seen.insert((node.id.clone(), target.clone())) {
                            continue;
                        }
                        edges.push(dispatch_edge(node, target, line, &via));
                        added += 1;
                        if added >= FANOUT_CAP {
                            break;
                        }
                    }
                }
            }
        }
        edges
    }
}

/// Whether a text may hold one of a set of names as a substring: no text
/// for no name, a pattern naming them, or every text past
/// [`MAX_PATTERN_NAMES`] names or [`MAX_PATTERN_NAME_BYTES`] bytes or when
/// the pattern does not compile. The pass only skips the scans a text
/// holding none of the names cannot feed, so reading every text finds the
/// same matches.
#[derive(Clone)]
enum Names {
    None,
    Pattern(Regex),
    Any,
}

impl Names {
    fn new<'n>(names: impl Iterator<Item = &'n String>) -> Self {
        Self::bounded(
            names,
            MAX_PATTERN_NAMES,
            MAX_PATTERN_NAME_BYTES,
            PATTERN_SIZE_LIMIT,
        )
    }

    fn bounded<'n>(
        names: impl Iterator<Item = &'n String>,
        most: usize,
        most_bytes: usize,
        size_limit: usize,
    ) -> Self {
        let mut names: Vec<&str> = names.map(String::as_str).collect();
        if names.is_empty() {
            return Self::None;
        }
        let bytes: usize = names.iter().map(|name| name.len()).sum();
        if names.len() > most || bytes > most_bytes {
            return Self::Any;
        }
        names.sort_unstable();
        let pattern = names
            .iter()
            .map(|name| regex::escape(name))
            .collect::<Vec<_>>()
            .join("|");
        RegexBuilder::new(&pattern)
            .size_limit(size_limit)
            .build()
            .map_or(Self::Any, Self::Pattern)
    }

    fn in_text(&self, text: &str) -> bool {
        match self {
            Self::None => false,
            Self::Pattern(pattern) => pattern.is_match(text),
            Self::Any => true,
        }
    }
}

/// A `fn-pointer-dispatch` edge from `source`'s dispatch site at `line`.
fn dispatch_edge(source: &Node, target: &str, line: i64, via: &str) -> Edge {
    Edge {
        id: None,
        source: source.id.clone(),
        target: target.to_string(),
        kind: EdgeKind::Calls,
        metadata: Some(json!({
            "synthesizedBy": "fn-pointer-dispatch",
            "via": via,
            "registeredAt": format!("{}:{line}", source.file_path),
        })),
        line: Some(line),
        col: None,
        provenance: Some("heuristic".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_read_by_its_last_extension_in_any_case() {
        assert!(has_extension("src/a.c", &C_CPP_EXTENSIONS));
        assert!(has_extension("src/A.HPP", &C_CPP_EXTENSIONS));
        assert!(has_extension(".h", &C_CPP_EXTENSIONS));
        assert!(!has_extension("src/kernel.cu", &C_CPP_EXTENSIONS));
        assert!(!has_extension("dir.c/file", &C_CPP_EXTENSIONS));
        assert!(!has_extension("x.tar", &C_CPP_EXTENSIONS));
        assert!(has_extension("gen/ops.def", &INCLUDABLE_EXTENSIONS));
        assert!(!has_extension("ops.inl", &INCLUDABLE_EXTENSIONS));
    }

    #[test]
    fn a_body_splits_outside_brackets_only() {
        assert_eq!(
            split_top_level("a, (b, c), {d, e}, [f, g], h", b','),
            ["a", " (b, c)", " {d, e}", " [f, g]", " h"]
        );
        // A closer before its opener leaves the depth below zero, where a
        // separator does not split; string literals are not set apart.
        assert_eq!(split_top_level(") , a", b','), [") , a"]);
        assert_eq!(split_top_level("\"a,b\", fn", b','), ["\"a", "b\"", " fn"]);
        assert_eq!(split_top_level("", b','), [""]);
    }

    #[test]
    fn a_bracket_matches_its_closer_or_none() {
        assert_eq!(match_brace("x{ { } }", 1), Some(7));
        assert_eq!(match_brace("{ {", 0), None);
        assert_eq!(match_paren("f(a(b), c)", 1), Some(9));
        assert_eq!(match_paren("f(", 1), None);
    }

    #[test]
    fn function_macros_join_continuations_and_skip_variadic_ones() {
        let macros = parse_function_macros(concat!(
            "#define MK(nm, fn) nm, \\\n  fn\n",
            "# define V(...) x\n",
            "#define W(a, rest...) y\n",
            "#define MK(a) again\r\n",
            "#define OBJ value\n",
        ));
        let mut names: Vec<&String> = macros.keys().collect();
        names.sort();
        assert_eq!(names, ["MK"]);
        // A later definition replaces an earlier one; `\r\n` ends the line.
        assert_eq!(macros["MK"].params, ["a"]);
        assert_eq!(macros["MK"].expansion, "again");
        let joined = parse_function_macros("#define MK(nm, fn) nm, \\\n  fn\n");
        assert_eq!(joined["MK"].expansion, "nm,    fn");
        // Upstream's `\s+` before the expansion crosses a line end, so a
        // macro with an empty expansion takes the next line as its own.
        let empty = parse_function_macros("#define UNUSED(x)\nint run(void);\n");
        assert_eq!(empty["UNUSED"].expansion, "int run(void);");
        assert!(parse_function_macros("int x;\n").is_empty());
    }

    #[test]
    fn object_macros_and_defined_names_read_every_define() {
        let objects =
            parse_object_macros("#define CMD_T command\r\n#define F(x) x\n#define EMPTY\n");
        assert_eq!(objects.len(), 1);
        assert_eq!(&*objects["CMD_T"], "command");
        let defined =
            parse_defined_names("#define CMD_T command\n  #  define F(x) x\n#define EMPTY\n");
        let mut names: Vec<&String> = defined.iter().collect();
        names.sort();
        assert_eq!(names, ["CMD_T", "EMPTY", "F"]);
    }

    #[test]
    fn conditionals_keep_the_active_arms_and_every_line() {
        let defined: HashSet<String> = ["ON".to_string()].into_iter().collect();
        let text = concat!(
            "#ifdef ON\n",
            "a\n",
            "#else\n",
            "b\n",
            "#endif\n",
            "#ifndef ON\n",
            "c\n",
            "#elif X\n",
            "d\n",
            "#elif Y\n",
            "e\n",
            "#endif\n",
            "#if defined(OFF)\n",
            "f\n",
            "#elif\n",
            "  #  if SOME > 1\n",
            "g\n",
            "#endif\n",
            "#endif\n",
            "#if !defined OFF\n",
            "h\n",
            "#endif\n",
            "i",
        );
        let out = eval_conditionals(text, &defined);
        assert_eq!(out.split('\n').count(), text.split('\n').count());
        let kept: Vec<&str> = out.split('\n').filter(|line| !line.is_empty()).collect();
        // `#elif` is taken when no arm before it was, whatever it tests;
        // `SOME > 1` cannot be evaluated and keeps its arm.
        assert_eq!(kept, ["a", "d", "g", "h", "i"]);
        assert_eq!(
            eval_conditionals("no directives\n", &defined),
            "no directives\n"
        );
    }

    /// The names [`condition_names`] lists are the only ones
    /// [`eval_conditionals`] asks about: the defined names among them give
    /// the result the whole set gives.
    #[test]
    fn a_condition_reads_only_the_names_its_directives_test() {
        let text = concat!(
            "#ifdef A\n",
            "a\n",
            "#endif\n",
            "  # ifndef B\n",
            "b\n",
            "#elif C\n",
            "c\n",
            "#endif\n",
            "#if defined(D)\n",
            "d\n",
            "#endif\n",
            "#if !defined E\n",
            "e\n",
            "#endif\n",
            "#if F > 1 && defined(G)\n",
            "f\n",
            "#endif\n",
            "#define H 1\n",
        );
        assert_eq!(condition_names(text), ["A", "B", "D", "E"]);
        assert!(condition_names("#define A 1\nx\n").is_empty());
        let names = condition_names(text);
        for defined in [
            vec![],
            vec!["A", "C", "F", "G", "H"],
            vec!["B", "D", "E", "Z"],
            vec!["A", "B", "C", "D", "E", "F", "G", "H"],
        ] {
            let all: HashSet<String> = defined.iter().map(|name| name.to_string()).collect();
            let asked: HashSet<String> = all
                .iter()
                .filter(|name| names.contains(name))
                .cloned()
                .collect();
            assert_eq!(
                eval_conditionals(text, &all),
                eval_conditionals(text, &asked),
                "{defined:?}"
            );
        }
    }

    /// A chain reads a name from its first map that holds it, as merging
    /// the maps in order with the first definition winning would.
    #[test]
    fn a_chain_reads_its_first_map_holding_a_name() {
        let map = |pairs: &[(&str, &str)]| -> Rc<ObjMacros> {
            Rc::new(
                pairs
                    .iter()
                    .map(|(name, value)| (name.to_string(), Rc::from(*value)))
                    .collect(),
            )
        };
        let own = map(&[("T", "own"), ("U", "own")]);
        let included = map(&[("T", "included"), ("V", "included")]);
        let chain = Chain {
            maps: vec![own, included],
        };
        assert_eq!(chain.lookup("T").map(|value| &**value), Some("own"));
        assert_eq!(chain.lookup("V").map(|value| &**value), Some("included"));
        assert_eq!(chain.lookup("W"), None);
        assert!(!chain.holds_none());
        let unit = chain.under(map(&[("V", "unit")]));
        assert_eq!(unit.lookup("V").map(|value| &**value), Some("unit"));
        assert_eq!(unit.lookup("T").map(|value| &**value), Some("own"));
        assert!(
            Chain::<Rc<str>> {
                maps: vec![map(&[]), map(&[])]
            }
            .holds_none()
        );
        let defined: Vec<Rc<HashSet<String>>> = vec![
            Rc::new(["A".to_string()].into_iter().collect()),
            Rc::new(["B".to_string()].into_iter().collect()),
        ];
        assert!(defined.defines("B"));
        assert!(!defined.defines("C"));
    }

    /// Every initializer and array table ends `= {`, spaces between.
    #[test]
    fn a_brace_initializer_is_an_equals_then_a_brace() {
        assert!(holds_brace_initializer("static struct ops t = { f };"));
        assert!(holds_brace_initializer("int a[] =\n\t{ 1 };"));
        assert!(holds_brace_initializer("x =\u{A0}{"));
        assert!(!holds_brace_initializer("int a = 1; f(b == c); { }"));
        assert!(!holds_brace_initializer("x = (y) {"));
        assert!(!holds_brace_initializer(""));
        // A text either pattern matches holds one.
        for text in [
            "static struct ops t = { f };",
            "static const handler_t *tbl[N] =\u{3000}{ f };",
            "; struct ops t[2]\u{FEFF}=\u{A0}\n{ f, g };",
            "int a = 1;",
        ] {
            if INIT.is_match(text) || ARRAY_TABLE.is_match(text) {
                assert!(holds_brace_initializer(text), "{text}");
            }
        }
        assert!(INIT.is_match("; struct ops t[2]\u{FEFF}=\u{A0}\n{ f, g };"));
    }

    /// A name set reads a text as holding a name when the text holds one as
    /// a substring; the empty set reads none.
    #[test]
    fn a_name_set_reads_the_texts_holding_one_of_its_names() {
        let sets: [&[&str]; 3] = [&[], &["run"], &["run", "stop", "a.b"]];
        for set in sets {
            let names: Vec<String> = set.iter().map(|name| (*name).to_string()).collect();
            let filter = Names::new(names.iter());
            assert_eq!(matches!(filter, Names::None), names.is_empty(), "{set:?}");
            for text in ["", "p->run()", "p->runner()", "stop", "a.b", "axb", "x"] {
                let holds = names.iter().any(|name| text.contains(name.as_str()));
                assert_eq!(filter.in_text(text), holds, "{set:?} {text:?}");
            }
        }
    }

    /// Past the names or bytes one pattern spells out, or when the pattern
    /// does not compile, every text may hold a name, and no pattern is built.
    #[test]
    fn past_the_name_bound_every_text_may_hold_a_name() {
        let names =
            |count: usize| -> Vec<String> { (0..count).map(|i| format!("field_{i}")).collect() };
        let at_the_bound = names(MAX_PATTERN_NAMES);
        assert!(matches!(Names::new(at_the_bound.iter()), Names::Pattern(_)));
        let past = names(MAX_PATTERN_NAMES + 1);
        assert!(matches!(Names::new(past.iter()), Names::Any));
        let long = ["f".repeat(MAX_PATTERN_NAME_BYTES + 1)];
        assert!(matches!(Names::new(long.iter()), Names::Any));
        let huge = names(65536);
        assert!(matches!(Names::new(huge.iter()), Names::Any));
        let refused = Names::bounded(
            names(3).iter(),
            MAX_PATTERN_NAMES,
            MAX_PATTERN_NAME_BYTES,
            1,
        );
        assert!(matches!(refused, Names::Any));
        for filter in [Names::new(past.iter()), refused] {
            assert!(filter.in_text(""));
            assert!(filter.in_text("no name here"));
        }
    }

    /// Lines sliced out of a text are the lines joined back.
    #[test]
    fn a_line_slice_is_the_lines_joined_back() {
        for text in ["", "a", "a\n", "a\nb\nc", "\n\n", "a\r\nb\n\nc\n"] {
            let lines = Lines::new(text);
            let split: Vec<&str> = text.split('\n').collect();
            for start in -1..6 {
                for end in -1..6 {
                    let joined = if start < 1 {
                        String::new()
                    } else {
                        let from = (start - 1) as usize;
                        let to = (end.max(0) as usize).min(split.len());
                        if from >= to {
                            String::new()
                        } else {
                            split[from..to].join("\n")
                        }
                    };
                    assert_eq!(lines.slice(start, end), joined, "{text:?} {start} {end}");
                }
            }
        }
    }

    #[test]
    fn a_type_alias_is_read_through_at_most_five_macros() {
        let objects: ObjMacros = [
            ("T1", "T2"),
            ("T2", "union T3"),
            ("T3", "T4"),
            ("T4", "T5"),
            ("T5", "struct T6"),
            ("T6", "T7"),
            ("NUM", "0x10 + 1"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), Rc::from(value)))
        .collect();
        assert_eq!(resolve_type_name("T1", &objects), "T6");
        assert_eq!(resolve_type_name("T2", &objects), "T7");
        assert_eq!(resolve_type_name("NUM", &objects), "NUM");
        assert_eq!(resolve_type_name("other", &objects), "other");
    }

    #[test]
    fn a_macro_call_expands_its_whole_word_parameters() {
        let definition = MacroDef {
            params: vec!["name".to_string(), "impl".to_string()],
            expansion: "{ 1, impl, #name, impl_x }".to_string(),
        };
        assert_eq!(
            substitute_macro(&definition, &["abs".to_string(), "absImpl".to_string()]),
            "{ 1, absImpl, #abs, impl_x }"
        );
        assert_eq!(substitute_macro(&definition, &[]), "{ 1, , #, impl_x }");
    }

    #[test]
    fn macro_calls_expand_at_most_six_times() {
        let env: FnMacros = [(
            "M".to_string(),
            Rc::new(MacroDef {
                params: vec!["x".to_string()],
                expansion: "x".to_string(),
            }),
        )]
        .into_iter()
        .collect();
        assert_eq!(
            expand_macro_calls("M(a), M(b), M(c), M(d), M(e), M(f), M(g)", &env),
            "a, b, c, d, e, f, M(g)"
        );
        assert_eq!(expand_macro_calls("f(M(a)), M(", &env), "f(a), M(");
        assert_eq!(expand_macro_calls("M(a)", &FnMacros::new()), "M(a)");
    }

    #[test]
    fn struct_fields_take_a_position_each_parsed_or_not() {
        let fields = parse_struct_fields_raw(concat!(
            " int a, b; union { int x; float y; }; struct command *cmd, **last;",
            " int (*fn)(int argc); hook_func hook; ",
        ));
        let summary: Vec<(Option<&str>, usize, bool, &str)> = fields
            .iter()
            .map(|field| {
                (
                    field.name.as_deref(),
                    field.index,
                    field.ptr,
                    field.ty.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (Some("a"), 0, false, "int"),
                (Some("b"), 1, false, "int"),
                (None, 2, false, ""),
                (Some("cmd"), 3, false, "command"),
                (Some("last"), 4, false, "command"),
                (Some("fn"), 5, true, ""),
                (Some("hook"), 6, false, "hook_func"),
            ]
        );
    }

    /// KEEP-RUST: from the declaration holding a body's first conditional or
    /// `#include` on, a field's position may differ between translation
    /// units; the fields before it keep theirs.
    #[test]
    fn positions_hold_before_a_bodys_first_conditional() {
        let positional = |inner: &str| -> Vec<(Option<String>, bool)> {
            parse_struct_fields_raw(inner)
                .into_iter()
                .map(|field| (field.name, field.positional))
                .collect()
        };
        let named = |name: &str, positional: bool| (Some(name.to_string()), positional);
        assert_eq!(
            positional(
                " int (*a)(void); int b;\n#ifdef X\n int (*c)(void);\n#else\n int d;\n#endif\n int (*e)(void); "
            ),
            [
                named("a", true),
                named("b", true),
                named("c", false),
                named("d", false),
                named("e", false),
            ]
        );
        assert_eq!(
            positional(" int (*a)(void);\n  #  include \"more.def\"\n int (*b)(void); "),
            [named("a", true), named("b", false)]
        );
        // `#define` and `#pragma` add no fields; a CRLF line still starts a
        // directive.
        assert_eq!(
            positional(" int (*a)(void);\n#define N 2\n#pragma pack(1)\n int (*b)(void); "),
            [named("a", true), named("b", true)]
        );
        assert_eq!(
            positional(" int (*a)(void);\r\n#if N\r\n int (*b)(void);\r\n#endif\r\n"),
            [named("a", true), named("b", false), (None, false)]
        );
    }

    #[test]
    fn an_include_path_normalizes_as_posix_does() {
        assert_eq!(posix_normalize("src/./inc/../a.h"), "src/a.h");
        assert_eq!(posix_normalize("./a.h"), "a.h");
        assert_eq!(posix_normalize("../a.h"), "../a.h");
        assert_eq!(posix_normalize("a/../../b.h"), "../b.h");
        assert_eq!(posix_normalize("a//b.h"), "a/b.h");
        assert_eq!(posix_normalize("."), ".");
        assert_eq!(posix_dirname("src/a.c"), "src");
        assert_eq!(posix_dirname("a.c"), ".");
    }

    /// Upstream's per-name pattern, compiled, for comparison.
    fn declared_by_pattern(source: &str, name: &str) -> Vec<String> {
        let pattern = js_regex(
            &format!(
                r"(?:(?:struct|union)\s+)?([A-Za-z0-9_]+)\s*\*?\s*(?-u:\b){name}(?-u:\b)\s*(?:[,)=;]|\[)"
            ),
            "declared type",
        );
        pattern
            .captures_iter(source)
            .map(|found| found[1].to_string())
            .collect()
    }

    #[test]
    fn declared_types_scan_as_the_per_name_pattern_matches() {
        let cases = [
            ("void run(struct cmd *p, int argc) { p->fn(argc); }", "p"),
            ("struct context *ctx = get(); ctx->cmd->proc(x);", "ctx"),
            ("p p p; int p, q; struct a *p[2];", "p"),
            ("xstruct foo p; mystruct  bar\t* p) union baz p=", "p"),
            ("a **p; b * * p; c*p; d p\n,", "p"),
            ("struct p; union p, x", "p"),
            ("ap p; pa p; _p p; p_ p;", "p"),
            ("é p; ×p; a\u{2028}p;", "p"),
            ("int x = cmdnames[i].cmd_func;", "cmdnames"),
            ("", "p"),
        ];
        for (source, name) in cases {
            let scanned: Vec<String> = declared_types(source, name)
                .into_iter()
                .map(str::to_string)
                .collect();
            assert_eq!(scanned, declared_by_pattern(source, name), "{source:?}");
        }
        // Generated text over the characters the pattern reads.
        let alphabet: Vec<char> = "ps truconi*;,)=[]( \n\t_ab".chars().collect();
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..4000 {
            let mut source = String::new();
            for _ in 0..40 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                source.push(alphabet[(state >> 33) as usize % alphabet.len()]);
            }
            for name in ["p", "ab", "struct"] {
                let scanned: Vec<String> = declared_types(&source, name)
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                assert_eq!(
                    scanned,
                    declared_by_pattern(&source, name),
                    "{source:?} {name}"
                );
            }
        }
    }

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::{FileRecord, Language};
    use codegraph_store::Store;
    use std::path::{Path, PathBuf};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cg-c-fnptr-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn c_node(id: &str, kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language: Language::C,
            start_line: line,
            end_line: line,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    /// A C project of `files` whose index holds exactly `nodes`, with no
    /// parser in between: the rows a parse that kept no symbols leaves.
    fn hand_indexed(dir: &Path, files: &[(&str, &str)], nodes: &[Node]) -> Store {
        let mut store = Store::open(&dir.join("index.db")).unwrap();
        store.upsert_nodes(nodes).unwrap();
        for (path, text) in files {
            std::fs::write(dir.join(path), text).unwrap();
            store
                .upsert_file(&FileRecord {
                    path: path.to_string(),
                    content_hash: hash_content(text),
                    language: Language::C,
                    size: 0,
                    modified_at: 0,
                    indexed_at: 0,
                    node_count: 0,
                    errors: Vec::new(),
                    generated: false,
                })
                .unwrap();
        }
        store
    }

    /// The pass's links as `source -> target` node ids, sorted.
    fn dispatch_links(store: &Store, dir: &Path) -> Vec<String> {
        let mut links: Vec<String> = crate::synthesis::synthesize(store, dir, 1 << 20)
            .unwrap()
            .into_iter()
            .filter(|edge| {
                edge.metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata["synthesizedBy"] == "fn-pointer-dispatch")
            })
            .map(|edge| format!("{} -> {}", edge.source, edge.target))
            .collect();
        links.sort();
        links
    }

    /// vim's `ex_cmds.h` shape when the parse keeps no symbols: the header's
    /// inline struct has no node, so the first includer to read its table
    /// registers a layout at that place, its fields as the header writes
    /// them, and the second reuses it. `fn` follows an `#ifdef`, so its
    /// position is no includer's: the positional `do_a` is not registered,
    /// the designated `do_b` of each includer is, and both dispatches reach
    /// both.
    #[test]
    fn an_inline_struct_without_a_node_is_one_layout_at_its_place() {
        const CMDS_H: &str = concat!(
            "#ifdef DECLARE\n",
            "static struct cmdname { char *name;\n",
            "#ifdef EXTRA\n",
            "  int extra;\n",
            "#endif\n",
            "  void (*fn)(int); } cmdnames[] =\n",
            "#else\n",
            "enum cmd_index\n",
            "#endif\n",
            "{ { \"a\", do_a }, { .fn = do_b } };\n",
        );
        let includer = |run: &str| {
            format!(
                "#define DECLARE\n#include \"cmds.h\"\nstatic void do_a(int x) {{}}\nstatic void do_b(int x) {{}}\nvoid {run}(int i) {{ (cmdnames[i].fn)(1); }}\n"
            )
        };
        let (a_c, b_c) = (includer("run_a"), includer("run_b"));
        let dir = temp_dir("site");
        let mut nodes = Vec::new();
        for (file, run) in [("a.c", "run_a"), ("b.c", "run_b")] {
            let prefix = &file[..1];
            nodes.push(c_node(
                &format!("{prefix}:do_a"),
                NodeKind::Function,
                "do_a",
                file,
                3,
            ));
            nodes.push(c_node(
                &format!("{prefix}:do_b"),
                NodeKind::Function,
                "do_b",
                file,
                4,
            ));
            nodes.push(c_node(
                &format!("{prefix}:{run}"),
                NodeKind::Function,
                run,
                file,
                5,
            ));
        }
        let store = hand_indexed(
            &dir,
            &[("cmds.h", CMDS_H), ("a.c", &a_c), ("b.c", &b_c)],
            &nodes,
        );
        let mut expected = Vec::new();
        for run in ["a:run_a", "b:run_b"] {
            for target in ["a:do_b", "b:do_b"] {
                expected.push(format!("{run} -> {target}"));
            }
        }
        assert_eq!(dispatch_links(&store, &dir), expected);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An inline struct is the node of its name that starts at its line;
    /// two such nodes leave its table unread.
    #[test]
    fn an_inline_struct_of_two_nodes_at_its_line_is_not_read() {
        const OPS_C: &str = concat!(
            "static struct ops { void (*run)(void); } t = { f };\n",
            "static void f(void) {}\n",
            "void go(void) { t.run(); }\n",
        );
        let functions = [
            c_node("f", NodeKind::Function, "f", "ops.c", 2),
            c_node("go", NodeKind::Function, "go", "ops.c", 3),
        ];
        let dir = temp_dir("one-node");
        let mut nodes = vec![c_node("s1", NodeKind::Struct, "ops", "ops.c", 1)];
        nodes.extend(functions.iter().cloned());
        let store = hand_indexed(&dir, &[("ops.c", OPS_C)], &nodes);
        assert_eq!(dispatch_links(&store, &dir), ["go -> f"]);
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();

        let dir = temp_dir("two-nodes");
        nodes.push(c_node("s2", NodeKind::Struct, "ops", "ops.c", 1));
        let store = hand_indexed(&dir, &[("ops.c", OPS_C)], &nodes);
        assert!(dispatch_links(&store, &dir).is_empty());
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
