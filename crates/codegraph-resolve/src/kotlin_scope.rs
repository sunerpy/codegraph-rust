//! Kotlin names by scope (upstream #2146, #2172, #2168, #2206, #2232, in the
//! shape at `b635dd46`): a top-level declaration is visible from its package,
//! an import or a star import; a bare call reaches a member of a type in reach
//! (the classes, anonymous objects, extension receivers and lambdas around
//! it, what they inherit, a type the project's function types take as a
//! receiver, an imported object); a standard-library name on an untyped chain
//! link needs a receiver named after its owner; and a Gradle script's bare
//! call never reaches a member of a project class.
//!
//! Heads and receivers are read from source, as upstream reads them: a first
//! resolution pass has no `extends` edges yet.

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::strip_comments::{CommentLang, blank_string_contents, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext};

/// Packages every Kotlin file imports without writing it.
const KOTLIN_DEFAULT_IMPORTS: &[&str] = &[
    "kotlin",
    "kotlin.annotation",
    "kotlin.collections",
    "kotlin.comparisons",
    "kotlin.io",
    "kotlin.ranges",
    "kotlin.sequences",
    "kotlin.text",
    "kotlin.jvm",
    "java.lang",
    "kotlin.js",
];

/// Kotlin's scope functions and standard collection / string / conversion
/// methods (upstream #2168 `KOTLIN_STD_METHODS`): on an untyped chain link
/// (`builder.apply { … }`, `list.map { … }`) they are the standard
/// library's, never a project type's same-named member. Names project types
/// commonly carry (`get`, `write`, `close`, `add`) are left out.
const KOTLIN_STD_METHODS: &[&str] = &[
    "apply",
    "also",
    "let",
    "run",
    "takeIf",
    "takeUnless",
    "toString",
    "equals",
    "hashCode",
    "map",
    "mapNotNull",
    "mapIndexed",
    "filter",
    "filterNot",
    "filterIsInstance",
    "forEach",
    "forEachIndexed",
    "first",
    "firstOrNull",
    "last",
    "lastOrNull",
    "single",
    "singleOrNull",
    "isEmpty",
    "isNotEmpty",
    "isNullOrEmpty",
    "isNullOrBlank",
    "isBlank",
    "isNotBlank",
    "orEmpty",
    "joinToString",
    "toList",
    "toMutableList",
    "toSet",
    "toMutableSet",
    "toMap",
    "toTypedArray",
    "any",
    "all",
    "none",
    "count",
    "sumOf",
    "maxOf",
    "minOf",
    "maxOrNull",
    "minOrNull",
    "sortedBy",
    "sortedByDescending",
    "sorted",
    "sortedWith",
    "reversed",
    "drop",
    "dropLast",
    "take",
    "takeLast",
    "zip",
    "flatMap",
    "flatten",
    "distinct",
    "groupBy",
    "associate",
    "associateBy",
    "associateWith",
    "partition",
    "contains",
    "containsKey",
    "getOrElse",
    "getOrNull",
    "getOrPut",
    "getOrDefault",
    "trim",
    "trimEnd",
    "trimStart",
    "split",
    "substring",
    "startsWith",
    "endsWith",
    "replace",
    "lowercase",
    "uppercase",
    "toInt",
    "toLong",
    "toDouble",
    "toFloat",
    "toIntOrNull",
    "toLongOrNull",
    "encodeToByteArray",
    "decodeToString",
    "copyOf",
    "copyOfRange",
    "indexOf",
    "lastIndexOf",
    "withIndex",
    "asSequence",
    "asList",
    "ifEmpty",
    "ifBlank",
    "padStart",
    "padEnd",
    "repeat",
    "lines",
    "toCharArray",
    "coerceAtLeast",
    "coerceAtMost",
    "coerceIn",
];

/// Words before a `{` that open a block, not a lambda argument.
const KOTLIN_BLOCK_WORDS: &[&str] = &[
    "if",
    "else",
    "for",
    "while",
    "do",
    "when",
    "try",
    "catch",
    "finally",
    "init",
    "get",
    "set",
    "constructor",
    "fun",
    "class",
    "object",
    "interface",
    "return",
    "by",
    "lazy",
    "apply",
    "run",
    "also",
    "let",
    "with",
    "use",
];

/// What the Android framework and AndroidX classes a Kotlin class commonly
/// extends inherit (upstream `KOTLIN_PLATFORM_SUPERS`), so an extension on
/// `ComponentCallbacks` or `ComponentActivity` is in reach of an
/// `AppCompatActivity` subclass.
fn platform_supers(name: &str) -> &'static [&'static str] {
    match name {
        "AppCompatActivity" => &["FragmentActivity"],
        "FragmentActivity" => &["ComponentActivity"],
        "ComponentActivity" => &[
            "Activity",
            "LifecycleOwner",
            "ViewModelStoreOwner",
            "SavedStateRegistryOwner",
        ],
        "Activity" => &["ContextThemeWrapper", "ComponentCallbacks2"],
        "ContextThemeWrapper" => &["ContextWrapper"],
        "ContextWrapper" => &["Context"],
        "Application" | "Service" => &["ContextWrapper", "ComponentCallbacks2"],
        "ComponentCallbacks2" => &["ComponentCallbacks"],
        "Fragment" => &[
            "ComponentCallbacks",
            "LifecycleOwner",
            "ViewModelStoreOwner",
            "SavedStateRegistryOwner",
        ],
        "DialogFragment" => &["Fragment"],
        "AppCompatDialogFragment" => &["DialogFragment"],
        "BottomSheetDialogFragment" => &["AppCompatDialogFragment"],
        "AndroidViewModel" => &["ViewModel"],
        _ => &[],
    }
}

/// How many type names a bare call's hierarchy walk visits.
const HIERARCHY_LIMIT: usize = 60;

/// How many receiver types the project-wide set grows to through
/// supertypes.
const RECEIVER_TYPES_LIMIT: usize = 5000;

/// Upstream `MEMBER_CLASS_KINDS` (no node has `record`).
pub(crate) fn is_member_class_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Interface | NodeKind::Enum
    )
}

/// Upstream `isMethodOwnerKind`.
fn is_method_owner_kind(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
    ) || (node.language == Language::Scala && node.kind == NodeKind::Module)
}

/// `^[A-Za-z_$][\w$]*$` (ASCII).
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// A Kotlin call by a bare name (upstream `kotlinCall`).
pub(crate) fn is_kotlin_call(reference: &RefView) -> bool {
    reference.language == Language::Kotlin
        && reference.reference_kind == EdgeKind::Calls
        && is_identifier(&reference.reference_name)
}

/// A Kotlin file's `package` and the names and packages its `import`s bring
/// in (upstream `kotlinFileScope`): block comments blanked and backticks
/// dropped first.
#[derive(Debug, Default)]
pub(crate) struct KotlinFileScope {
    pub(crate) package: String,
    imports: HashSet<String>,
    stars: HashSet<String>,
}

impl KotlinFileScope {
    pub(crate) fn read(source: &str) -> Self {
        static BLOCK: OnceLock<Regex> = OnceLock::new();
        static PACKAGE: OnceLock<Regex> = OnceLock::new();
        static IMPORT: OnceLock<Regex> = OnceLock::new();
        let block = BLOCK.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").expect("kotlin block"));
        let package = PACKAGE.get_or_init(|| {
            Regex::new(r"(?m)^\s*package\s+([A-Za-z0-9_.]+)").expect("kotlin package")
        });
        let import = IMPORT.get_or_init(|| {
            Regex::new(
                r"(?m)^\s*import\s+([A-Za-z0-9_.]+?)(\.\*)?(?:\s+as\s+[A-Za-z0-9_]+)?\s*;?\s*(?://.*)?$",
            )
            .expect("kotlin import")
        });
        let text = block.replace_all(source, " ").replace('`', "");
        let mut scope = Self {
            package: package
                .captures(&text)
                .map(|caps| caps[1].to_string())
                .unwrap_or_default(),
            ..Self::default()
        };
        for caps in import.captures_iter(&text) {
            let path = caps[1].to_string();
            if caps.get(2).is_some() {
                scope.stars.insert(path);
            } else {
                scope.imports.insert(path);
            }
        }
        scope
    }
}

/// `file`'s [`KotlinFileScope`], read once per resolution pass; empty when
/// the file is unreadable.
pub(crate) fn file_scope(file: &str, context: &dyn ResolutionContext) -> Arc<KotlinFileScope> {
    context.source_facts(file).map_or_else(
        || Arc::new(KotlinFileScope::default()),
        |facts| facts.kotlin_file_scope(),
    )
}

/// Upstream `KOTLIN_ENCLOSING_KINDS`.
fn is_enclosing_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Enum
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Module
            | NodeKind::Namespace
            | NodeKind::Function
            | NodeKind::Method
    )
}

/// Whether a top-level Kotlin declaration (a function, an extension function
/// indexed under its receiver type, `JdbcTransaction::assertEquals`, a
/// property) can be named from the file a call is written in (upstream
/// #2146 `isKotlinTopLevelVisible`): its own package, an `import` of it, a
/// star import of its package, or a package every file imports. A member of a
/// class is not judged here; a lambda's receiver can put any type's members
/// in scope.
pub(crate) fn is_kotlin_top_level_visible(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if candidate.language != Language::Kotlin || candidate.file_path == reference.file_path {
        return true;
    }
    let enclosed = context
        .get_nodes_in_file_shared(&candidate.file_path)
        .iter()
        .any(|other| {
            other.id != candidate.id
                && is_enclosing_kind(other.kind)
                && other.start_line <= candidate.start_line
                && other.end_line >= candidate.end_line
                && (other.start_line < candidate.start_line || other.end_line > candidate.end_line)
        });
    if enclosed {
        return true;
    }
    let candidate_scope = file_scope(&candidate.file_path, context);
    let package = candidate_scope.package.as_str();
    let here = file_scope(&reference.file_path, context);
    let imported = if package.is_empty() {
        candidate.name.clone()
    } else {
        format!("{package}.{}", candidate.name)
    };
    package == here.package
        || here.stars.contains(package)
        || here.imports.contains(&imported)
        || KOTLIN_DEFAULT_IMPORTS.contains(&package)
}

/// Segment `line - 1` of the reference's file split at `\n` (a `\r` before
/// it kept), as upstream reads a call's line.
fn raw_line(reference: &RefView, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(&reference.file_path)?;
    let index = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    facts.raw_line(index).map(str::to_string)
}

/// Where the reference's name starts on `line`: at its column, or else its
/// first occurrence (not after a word character or `$`) followed, past any
/// whitespace, by one of `followers`.
fn name_start(
    reference: &RefView,
    line: &str,
    followers: &[char],
    at_column_needs_boundary: bool,
) -> Option<usize> {
    let name = reference.reference_name.as_str();
    let column = usize::try_from(reference.column).unwrap_or(0);
    if line
        .get(column..)
        .is_some_and(|rest| rest.starts_with(name))
        && (!at_column_needs_boundary
            || !line[..column]
                .chars()
                .next_back()
                .is_some_and(|c| is_word_char(c) || c == '$'))
    {
        return Some(column);
    }
    line.match_indices(name).map(|(at, _)| at).find(|&at| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| is_word_char(c) || c == '$')
            && line[at + name.len()..].trim_start().starts_with(followers)
    })
}

/// Whether a bare Kotlin name is written with no receiver at its call, not
/// as a later link of a chain (`….name(`; upstream #2172
/// `isReceiverLessKotlinCall`).
pub(crate) fn is_receiver_less_kotlin_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(line) = raw_line(reference, context) else {
        return false;
    };
    name_start(reference, &line, &['(', '{', '<'], true)
        .is_some_and(|start| !line[..start].trim_end().ends_with(['.', ':']))
}

/// Whether a bare `require(…)` / `check(…)` / `assert(…)` is Kotlin's
/// precondition (upstream `isKotlinPreconditionCall`): its condition is a
/// comparison or boolean expression, or a lazy message follows it.
fn is_kotlin_precondition_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    static CONDITION: OnceLock<Regex> = OnceLock::new();
    if !matches!(
        reference.reference_name.as_str(),
        "require" | "check" | "assert"
    ) {
        return false;
    }
    let Some(line) = raw_line(reference, context) else {
        return false;
    };
    let name = reference.reference_name.as_str();
    // `(?<![\w.])name\s*\(`, the lookbehind checked by hand.
    let Some(open) = line.match_indices(name).find_map(|(at, _)| {
        if line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| is_word_char(c) || c == '.')
        {
            return None;
        }
        let rest = &line[at + name.len()..];
        let trimmed = rest.trim_start();
        trimmed
            .starts_with('(')
            .then(|| at + name.len() + (rest.len() - trimmed.len()) + 1)
    }) else {
        return false;
    };
    let mut depth = 0usize;
    let mut args = String::new();
    let mut rest = "";
    for (offset, c) in line[open..].char_indices() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            if depth == 0 {
                rest = &line[open + offset + 1..];
                break;
            }
            depth -= 1;
        }
        args.push(c);
    }
    CONDITION
        .get_or_init(|| {
            Regex::new(
                r"[<>=!]=|&&|\|\||(?:^|[\s(])!|\s[<>]\s|(?-u:\b)is(?-u:\b)|(?-u:\b)in(?-u:\b)|(?-u:\b)true(?-u:\b)|(?-u:\b)false(?-u:\b)",
            )
            .expect("kotlin precondition")
        })
        .is_match(&args)
        || rest.trim_start().starts_with('{')
}

/// The project-wide Kotlin facts a bare call reads, computed once per
/// resolution pass.
#[derive(Debug, Default)]
pub struct KotlinTables {
    receiver_types: OnceLock<HashSet<String>>,
}

impl KotlinTables {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every type a Kotlin function type in the project takes as its
    /// receiver, every type from outside the project it writes extensions on,
    /// and what those inherit (upstream #2172 `kotlinReceiverTypes`).
    fn receiver_types(&self, context: &dyn ResolutionContext) -> &HashSet<String> {
        self.receiver_types
            .get_or_init(|| receiver_types_of(context))
    }
}

/// What one Kotlin file's text contributes to the project's receiver types:
/// the type before each `.(` (`Module` in `Module.() -> Unit`), and each
/// segment of an extension function's receiver.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReceiverMentions {
    pub function_types: Vec<String>,
    pub extensions: Vec<String>,
}

impl ReceiverMentions {
    pub fn read(source: &str) -> Self {
        static FUNCTION_TYPE: OnceLock<Regex> = OnceLock::new();
        static EXTENSION: OnceLock<Regex> = OnceLock::new();
        let function_type = FUNCTION_TYPE.get_or_init(|| {
            Regex::new(
                r"(?-u:\b)([A-Z][A-Za-z0-9_]*)(?:<[^<>()]*(?:<[^<>()]*>[^<>()]*)*>)?\s*\.\s*\(",
            )
            .expect("kotlin function type")
        });
        let extension = EXTENSION.get_or_init(|| {
            Regex::new(
                r"(?-u:\b)fun\s+(?:<[^>]*>\s*)?([A-Z][A-Za-z0-9_]*(?:\.[A-Z][A-Za-z0-9_]*)*)(?:<[^<>()]*(?:<[^<>()]*>[^<>()]*)*>)?\??\.[A-Za-z_`][A-Za-z0-9_`]*\s*\(",
            )
            .expect("kotlin extension")
        });
        let text = strip_comments_for_regex(source, CommentLang::Java);
        let mut mentions = Self::default();
        for caps in function_type.captures_iter(&text) {
            push_unique(&mut mentions.function_types, &caps[1]);
        }
        for caps in extension.captures_iter(&text) {
            for part in caps[1].split('.') {
                push_unique(&mut mentions.extensions, part);
            }
        }
        mentions
    }
}

fn push_unique(list: &mut Vec<String>, name: &str) {
    if !list.iter().any(|known| known == name) {
        list.push(name.to_string());
    }
}

/// Whether `path` is a Kotlin source or script (`\.kts?$`).
pub fn is_kotlin_path(path: &str) -> bool {
    path.ends_with(".kt") || path.ends_with(".kts")
}

/// Upstream `kotlinReceiverTypes`, over the Kotlin files in path order.
fn receiver_types_of(context: &dyn ResolutionContext) -> HashSet<String> {
    let mut files: Vec<String> = context
        .get_all_files_shared()
        .iter()
        .filter(|file| is_kotlin_path(file))
        .cloned()
        .collect();
    files.sort();
    let mentions: Vec<ReceiverMentions> = files
        .iter()
        .filter_map(|file| context.read_file_shared(file))
        .map(|source| ReceiverMentions::read(&source))
        .collect();
    receiver_types_from(
        &mentions,
        |name| is_declared_class(name, context),
        |name| kotlin_declared_supertypes(name, context),
    )
}

/// Whether a Kotlin or Java class, struct, interface or enum of `name`
/// exists: an extension on such a type is no outside type's.
fn is_declared_class(name: &str, context: &dyn ResolutionContext) -> bool {
    context.get_nodes_by_name_shared(name).iter().any(|node| {
        is_member_class_kind(node.kind)
            && matches!(node.language, Language::Kotlin | Language::Java)
    })
}

/// The receiver types `mentions` (each Kotlin file's, in path order) make:
/// every function type's receiver, then every extension receiver no
/// `declared` class names, grown by `supertypes`.
pub fn receiver_types_from(
    mentions: &[ReceiverMentions],
    declared: impl Fn(&str) -> bool,
    supertypes: impl Fn(&str) -> Vec<String>,
) -> HashSet<String> {
    let mut types: Vec<String> = Vec::new();
    let mut outside: Vec<String> = Vec::new();
    for file in mentions {
        for name in &file.function_types {
            push_unique(&mut types, name);
        }
        for name in &file.extensions {
            push_unique(&mut outside, name);
        }
    }
    for name in outside {
        if !declared(&name) {
            push_unique(&mut types, &name);
        }
    }
    expand_receiver_types(types, supertypes)
}

/// `types` grown, breadth first, by what their Kotlin declarations' heads
/// extend, up to [`RECEIVER_TYPES_LIMIT`] names.
pub fn expand_receiver_types(
    types: Vec<String>,
    supertypes: impl Fn(&str) -> Vec<String>,
) -> HashSet<String> {
    let mut set: HashSet<String> = types.iter().cloned().collect();
    let mut queue: VecDeque<String> = types.into();
    while set.len() < RECEIVER_TYPES_LIMIT {
        let Some(name) = queue.pop_front() else {
            break;
        };
        for supertype in supertypes(&name) {
            if set.insert(supertype.clone()) {
                queue.push_back(supertype);
            }
        }
    }
    set
}

/// The supertypes every Kotlin member class named `name` declares in its
/// head (upstream `classHeadSupertypes`), the declarations in path order.
fn kotlin_declared_supertypes(name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let mut declarations: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| node.language == Language::Kotlin && is_member_class_kind(node.kind))
        .collect();
    declarations.sort_by(|a, b| {
        (&a.file_path, a.start_line, a.start_column, &a.id).cmp(&(
            &b.file_path,
            b.start_line,
            b.start_column,
            &b.id,
        ))
    });
    declarations
        .iter()
        .flat_map(|declaration| crate::class_heads::class_head_supertypes(declaration, context))
        .collect()
}

/// What a `{` of a Kotlin file opens, read from the text before it
/// (upstream `kotlinBraceFrames`): a `with(x)` / `.apply` / `.run` block, a
/// type or extension head's names, or a lambda passed to the named call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BraceHead {
    Scoped,
    Names(Vec<String>),
    Lambda(String),
}

/// A brace pair of a Kotlin file whose head names something, by line range,
/// in the order the pairs close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScannedBrace {
    start: i64,
    end: i64,
    head: BraceHead,
}

/// Each brace pair of `source` whose head names something (upstream
/// `kotlinBraceFrames`, before the lambda receivers are read): comments and
/// strings blanked, the head the text since the last `{`, `}` or `;` (at
/// most 600 characters).
pub(crate) fn scan_braces(source: &str) -> Vec<ScannedBrace> {
    static SCOPED: OnceLock<Regex> = OnceLock::new();
    static CALL: OnceLock<Regex> = OnceLock::new();
    let scoped_re = SCOPED.get_or_init(|| {
        Regex::new(r"(?:(?-u:\b)with\s*\([^{}]*\)|\.\s*(?:apply|run)(?:\s*<[^<>]*>)?)\s*$")
            .expect("kotlin scoped block")
    });
    let call_re = CALL.get_or_init(|| {
        Regex::new(
            r"(?:^|[^A-Za-z0-9_$])([a-z_][A-Za-z0-9_]*)\s*(?:<[^<>{}]*>)?\s*(?:\([^(){}]*\))?\s*$",
        )
        .expect("kotlin lambda call")
    });
    let text = blank_string_contents(&strip_comments_for_regex(source, CommentLang::Java));
    let mut braces = Vec::new();
    let mut stack: Vec<(i64, Option<BraceHead>)> = Vec::new();
    let mut line = 1i64;
    let mut pending: VecDeque<char> = VecDeque::new();
    for c in text.chars() {
        if c == '\n' {
            line += 1;
        }
        match c {
            '{' => {
                let head: String = pending.iter().collect();
                let opened = if scoped_re.is_match(&head) {
                    Some(BraceHead::Scoped)
                } else {
                    match head_names(&head) {
                        Some(names) if !names.is_empty() => Some(BraceHead::Names(names)),
                        // `single { get() }`: a lambda runs on the receiver
                        // its function's parameter type names (upstream
                        // #2206).
                        _ => call_re
                            .captures(&head)
                            .and_then(|caps| caps.get(1))
                            .map(|found| found.as_str())
                            .filter(|call| !KOTLIN_BLOCK_WORDS.contains(call))
                            .map(|call| BraceHead::Lambda(call.to_string())),
                    }
                };
                stack.push((line, opened));
                pending.clear();
            }
            '}' => {
                if let Some((start, Some(head))) = stack.pop() {
                    braces.push(ScannedBrace {
                        start,
                        end: line,
                        head,
                    });
                }
                pending.clear();
            }
            ';' => pending.clear(),
            _ => {
                if pending.len() >= 600 {
                    pending.drain(..300);
                }
                pending.push_back(c);
            }
        }
    }
    braces
}

/// A type body of a Kotlin file by line range (upstream
/// `kotlinBraceFrames`): each `class` / `object` / `interface` body with its
/// name and supertypes, an anonymous `object : Base(…)` with its base, an
/// extension function's body with its receiver, a lambda with the receiver
/// its function's parameter type names, or `*` for a `with(x)` / `.apply` /
/// `.run` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BraceFrame {
    start: i64,
    end: i64,
    names: Vec<String>,
}

/// Upstream `kotlinBraceFrames`.
pub(crate) fn brace_frames(source: &str, context: &dyn ResolutionContext) -> Vec<BraceFrame> {
    scan_braces(source)
        .into_iter()
        .filter_map(|brace| {
            let names = match brace.head {
                BraceHead::Scoped => vec!["*".to_string()],
                BraceHead::Names(names) => names,
                BraceHead::Lambda(call) => vec![lambda_receiver(&call, context)?],
            };
            Some(BraceFrame {
                start: brace.start,
                end: brace.end,
                names,
            })
        })
        .collect()
}

/// The type names a Kotlin block head introduces (upstream
/// `kotlinHeadNames`): a type declaration's name and supertypes, or an
/// extension function's receiver; `None` for any other head.
fn head_names(head: &str) -> Option<Vec<String>> {
    static DECLARATION: OnceLock<Regex> = OnceLock::new();
    static SUPERS: OnceLock<Regex> = OnceLock::new();
    static WHERE: OnceLock<Regex> = OnceLock::new();
    static RECEIVER: OnceLock<Regex> = OnceLock::new();
    let mut depth = 0usize;
    let mut flat = String::new();
    for c in head.chars() {
        match c {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let declaration = DECLARATION.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)(?:class|interface|object)(?-u:\b)(?:\s+([A-Za-z_][A-Za-z0-9_]*))?([^=]*)$",
        )
        .expect("kotlin declaration head")
    });
    if let Some(caps) = declaration.captures(&flat) {
        let rest = caps.get(2).map_or("", |found| found.as_str());
        let supers = SUPERS
            .get_or_init(|| Regex::new(r"(?s):\s*(.*)$").expect("kotlin supers"))
            .captures(rest)
            .and_then(|caps| caps.get(1))
            .map_or("", |found| found.as_str());
        let supers = WHERE
            .get_or_init(|| Regex::new(r"(?s)(?-u:\b)where(?-u:\b).*$").expect("kotlin where"))
            .replace(supers, "");
        let mut names: Vec<String> = caps
            .get(1)
            .map(|name| name.as_str().to_string())
            .into_iter()
            .collect();
        names.extend(listed_capitalized_names(&supers));
        return Some(names);
    }
    RECEIVER
        .get_or_init(|| {
            Regex::new(r"(?-u:\b)fun\s+(?:<[^>]*>\s*)?([A-Z][A-Za-z0-9_]*)(?:<[^>]*>)?\??\.[A-Za-z_][A-Za-z0-9_]*\s*\(")
                .expect("kotlin extension head")
        })
        .captures(head)
        .map(|caps| vec![caps[1].to_string()])
}

/// Each `([A-Z]\w*)\s*(?=,|$|\bby\b)` of `text`, the lookahead checked by
/// hand: a capitalized word, from its first capital, followed past any
/// whitespace by a `,`, the end, or the word `by`.
fn listed_capitalized_names(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut names = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if !is_word(bytes[at]) {
            at += 1;
            continue;
        }
        let run = at;
        let mut end = at;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        let Some(first) = bytes[run..end].iter().position(u8::is_ascii_uppercase) else {
            at = end;
            continue;
        };
        let rest = &text[end..];
        let after = end + (rest.len() - rest.trim_start().len());
        let follower = &text[after..];
        let followed = follower.is_empty()
            || follower.starts_with(',')
            || (after > end
                && follower
                    .strip_prefix("by")
                    .is_some_and(|tail| tail.bytes().next().is_none_or(|b| !is_word(b))));
        if followed {
            names.push(text[run + first..end].to_string());
            at = after.max(end);
        } else {
            at = end;
        }
    }
    names
}

/// The type a Kotlin function or method declaration writes for its last
/// parameter, as the lambda-receiver lookup reads it (upstream
/// `kotlinLambdaReceiver`): its head's twelve lines, the parameter list after
/// its name, `noinline` / `crossinline` dropped, a default value dropped.
pub(crate) fn lambda_parameter_type(
    facts: &crate::source_facts::SourceFacts,
    function: &Node,
) -> Option<String> {
    static TYPE: OnceLock<Regex> = OnceLock::new();
    static MODIFIER: OnceLock<Regex> = OnceLock::new();
    let type_re = TYPE.get_or_init(|| {
        Regex::new(r"(?s):\s*(.+?)(?:\s*=\s*[^=>].*)?$").expect("kotlin parameter type")
    });
    let modifier = MODIFIER.get_or_init(|| {
        Regex::new(r"^\s*(?:noinline|crossinline)\s+").expect("kotlin parameter modifier")
    });
    let open_re = Regex::new(&format!(r"(?-u:\b){}\s*\(", regex::escape(&function.name))).ok()?;
    let start = usize::try_from(function.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = start.saturating_add(12).min(facts.line_count());
    if start >= end {
        return None;
    }
    let head = facts.join_lines(start, end, " ");
    let open = open_re.find(&head)?;
    let paren = head[open.start()..].find('(').map(|at| open.start() + at)?;
    let mut depth = 0usize;
    let mut close = None;
    for (offset, c) in head[paren..].char_indices() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                close = Some(paren + offset);
                break;
            }
        }
    }
    let parameters = crate::cpp_namespaces::split_cpp_top_level(&head[paren + 1..close?]);
    let last = modifier.replace(parameters.last()?, "");
    type_re
        .captures(&last)
        .and_then(|caps| caps.get(1))
        .map(|written| written.as_str().to_string())
}

/// The right side of a Kotlin typealias declaration's first line
/// (`Scope.(Int) -> T` of `typealias Definition<T> = Scope.(Int) -> T`).
pub(crate) fn alias_rhs(
    facts: &crate::source_facts::SourceFacts,
    declaration: &Node,
) -> Option<String> {
    static RHS: OnceLock<Regex> = OnceLock::new();
    let index = usize::try_from(declaration.start_line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .filter(|index| *index < facts.line_count())?;
    RHS.get_or_init(|| Regex::new(r"=\s*(.+)$").expect("kotlin alias rhs"))
        .captures(facts.line(index))
        .map(|caps| caps[1].to_string())
}

/// The receiver a lambda passed to the project's `name` runs with (upstream
/// #2206 `kotlinLambdaReceiver`): the type before `.(` in its last
/// parameter's function type, directly or through a typealias. `None`
/// unless every Kotlin function or method of that name agrees.
fn lambda_receiver(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let written: Vec<String> = context
        .get_nodes_by_name_shared(name)
        .iter()
        .filter(|function| {
            function.language == Language::Kotlin
                && matches!(function.kind, NodeKind::Function | NodeKind::Method)
        })
        .filter_map(|function| {
            lambda_parameter_type(&*context.source_facts(&function.file_path)?, function)
        })
        .collect();
    lambda_receiver_from(written.iter().map(String::as_str), &|alias| {
        alias_rhs_named(alias, context)
    })
}

/// The first right side, in path order, of a Kotlin typealias named
/// `alias` (upstream reads them in index order; KEEP-RUST).
fn alias_rhs_named(alias: &str, context: &dyn ResolutionContext) -> Option<String> {
    let mut declarations: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(alias)
        .into_iter()
        .filter(|node| node.language == Language::Kotlin && node.kind == NodeKind::TypeAlias)
        .collect();
    declarations.sort_by(|a, b| (&a.file_path, a.start_line).cmp(&(&b.file_path, b.start_line)));
    declarations.iter().find_map(|declaration| {
        alias_rhs(&*context.source_facts(&declaration.file_path)?, declaration)
    })
}

/// The one receiver the parameter types `written` name, through
/// `alias_rhs` (a typealias name's right side), or `None` when they name
/// none or disagree.
pub fn lambda_receiver_from<'w>(
    written: impl IntoIterator<Item = &'w str>,
    alias_rhs: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let found: BTreeSet<String> = written
        .into_iter()
        .filter_map(|written| receiver_of(written, 0, alias_rhs))
        .collect();
    if found.len() == 1 {
        found.into_iter().next()
    } else {
        None
    }
}

/// The receiver a Kotlin function type names (`Scope` of `Scope.(Int) ->
/// T`), through up to two typealiases (upstream `receiverOf`).
fn receiver_of(
    written: &str,
    depth: usize,
    alias_rhs: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    static DIRECT: OnceLock<Regex> = OnceLock::new();
    static ALIAS: OnceLock<Regex> = OnceLock::new();
    let direct = DIRECT.get_or_init(|| {
        Regex::new(
            r"^\s*(?:suspend\s+)?([A-Z][A-Za-z0-9_]*)(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\s*\.\s*\(",
        )
        .expect("kotlin direct receiver")
    });
    if let Some(caps) = direct.captures(written) {
        return Some(caps[1].to_string());
    }
    let alias = ALIAS
        .get_or_init(|| Regex::new(r"^\s*([A-Z][A-Za-z0-9_]*)(?-u:\b)").expect("kotlin alias"))
        .captures(written)?
        .get(1)?
        .as_str()
        .to_string();
    if depth > 2 {
        return None;
    }
    receiver_of(&alias_rhs(&alias)?, depth + 1, alias_rhs)
}

/// The Kotlin types a bare call is written inside and what they inherit
/// (upstream #2172 `kotlinHierarchyAt`): the classes and objects around it,
/// the receiver of the extension function it is in, a Gradle script's
/// `Project` or `Settings`, and the brace frames around it; through
/// platform ancestry and the heads of their Kotlin declarations, at most
/// [`HIERARCHY_LIMIT`] names.
fn hierarchy_at(reference: &RefView, context: &dyn ResolutionContext) -> HashSet<String> {
    static OWNER_SPLIT: OnceLock<Regex> = OnceLock::new();
    let owner_split = OWNER_SPLIT.get_or_init(|| Regex::new(r"::|\.").expect("owner split"));
    let mut queue: VecDeque<String> = VecDeque::new();
    for node in context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
    {
        if node.start_line > reference.line || node.end_line < reference.line {
            continue;
        }
        if is_member_class_kind(node.kind) {
            queue.push_back(node.name.clone());
        } else if matches!(node.kind, NodeKind::Method | NodeKind::Function)
            && let Some((owner, _)) = node.qualified_name.rsplit_once("::")
        {
            // `fun Foo.bar() { baz() }`: Foo is the implicit receiver.
            if let Some(last) = owner_split.split(owner).last() {
                queue.push_back(last.to_string());
            }
        }
    }
    // A Gradle build script runs on the Project, a settings script on
    // Settings.
    if reference.file_path.ends_with(".gradle.kts") {
        let settings = reference.file_path == "settings.gradle.kts"
            || reference.file_path.ends_with("/settings.gradle.kts");
        queue.push_back(if settings { "Settings" } else { "Project" }.to_string());
    }
    if let Some(facts) = context.source_facts(&reference.file_path) {
        for frame in facts
            .kotlin_brace_frames(|source| brace_frames(source, context))
            .iter()
        {
            if frame.start <= reference.line && frame.end >= reference.line {
                queue.extend(frame.names.iter().cloned());
            }
        }
    }
    let mut names = HashSet::new();
    while names.len() < HIERARCHY_LIMIT {
        let Some(name) = queue.pop_front() else {
            break;
        };
        if names.contains(&name) {
            continue;
        }
        queue.extend(platform_supers(&name).iter().map(|s| (*s).to_string()));
        queue.extend(kotlin_declared_supertypes(&name, context));
        names.insert(name);
    }
    names
}

/// A method's owner as a bare call reads it: the last segment of its
/// qualified name's parent, the type around a companion object for one of its
/// members; `None` without a parent.
fn member_owner(qualified_name: &str) -> Option<(String, bool)> {
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    let split = SPLIT.get_or_init(|| Regex::new(r"::|\.").expect("owner split"));
    let (parent, _) = qualified_name.rsplit_once("::")?;
    if parent.is_empty() {
        return None;
    }
    let mut path: Vec<&str> = split.split(parent).collect();
    let mut owner = path.pop()?.to_string();
    let companion = owner == "Companion" && !path.is_empty();
    if companion {
        owner = path.pop()?.to_string();
    }
    Some((owner, companion))
}

/// A bare Kotlin call's site, read once for every candidate.
pub(crate) struct KotlinSite<'r> {
    reference: &'r RefView,
    hierarchy: OnceCell<HashSet<String>>,
    precondition: OnceCell<bool>,
}

impl<'r> KotlinSite<'r> {
    pub(crate) fn new(reference: &'r RefView) -> Self {
        Self {
            reference,
            hierarchy: OnceCell::new(),
            precondition: OnceCell::new(),
        }
    }

    fn hierarchy(&self, context: &dyn ResolutionContext) -> &HashSet<String> {
        self.hierarchy
            .get_or_init(|| hierarchy_at(self.reference, context))
    }

    /// Whether the bare call can reach `candidate` (upstream #2172
    /// `isKotlinMemberReachable`, with #2232's Gradle script rule): a member
    /// of a type around the call or of what it inherits, of a type the
    /// project's function types take as a receiver, or of an object the file
    /// imports. koin's `error("…")`, Kotlin's, went to a Logger's `error`.
    pub(crate) fn reaches(&self, candidate: &Node, context: &dyn ResolutionContext) -> bool {
        let reference = self.reference;
        // A Gradle script's bare calls run on the build tool's own types:
        // never a member of a class the project declares (nowinandroid's
        // `Graph.plugins()`); build logic's extensions on Gradle's types stay.
        if reference.file_path.ends_with(".kts") {
            if !matches!(
                candidate.kind,
                NodeKind::Method | NodeKind::Field | NodeKind::Property
            ) {
                return true;
            }
            let Some((owner, _)) = member_owner(&candidate.qualified_name) else {
                return false;
            };
            return !context
                .get_nodes_by_name_shared(&owner)
                .iter()
                .any(|node| is_method_owner_kind(node) || node.kind == NodeKind::Module);
        }
        // A Java class's method, too: Kotlin calls it bare only from a
        // subclass or through a static import.
        if candidate.kind != NodeKind::Method
            || !matches!(candidate.language, Language::Kotlin | Language::Java)
        {
            return true;
        }
        // `require(n >= 0) { … }`, `check(!closed)`: Kotlin's preconditions.
        if *self
            .precondition
            .get_or_init(|| is_kotlin_precondition_call(reference, context))
        {
            return false;
        }
        let Some((owner, companion)) = member_owner(&candidate.qualified_name) else {
            return true;
        };
        // A (Java) constructor is the type's, called by its name.
        if candidate.name == owner {
            return true;
        }
        let hierarchy = self.hierarchy(context);
        if hierarchy.contains("*")
            || hierarchy.contains(&owner)
            || context
                .kotlin_tables()
                .receiver_types(context)
                .contains(&owner)
        {
            return true;
        }
        // `import okio.TestUtil.deepCopy` / `import okio.TestUtil.*` names an
        // object's members.
        let package = file_scope(&candidate.file_path, context).package.clone();
        let mut object_path = if package.is_empty() {
            owner
        } else {
            format!("{package}.{owner}")
        };
        if companion {
            object_path.push_str(".Companion");
        }
        let here = file_scope(&reference.file_path, context);
        here.imports
            .contains(&format!("{object_path}.{}", candidate.name))
            || here.stars.contains(&object_path)
    }

    /// Of the members a bare call can reach, those the code around it
    /// reaches (its class, an extension's receiver, the lambda it is in)
    /// before those only the project-wide receiver set reaches (upstream
    /// #2206 `lexicalKotlinMembers`): koin's `get()` in `Scope.new(…)` is
    /// Scope's, not Koin's.
    pub(crate) fn lexical(
        &self,
        candidates: Vec<Arc<Node>>,
        context: &dyn ResolutionContext,
    ) -> Vec<Arc<Node>> {
        if candidates.len() < 2 {
            return candidates;
        }
        let hierarchy = self.hierarchy(context);
        if hierarchy.contains("*") {
            return candidates;
        }
        let lexical: Vec<Arc<Node>> = candidates
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Method
                    && member_owner(&node.qualified_name)
                        .is_some_and(|(owner, _)| hierarchy.contains(&owner))
            })
            .cloned()
            .collect();
        if lexical.is_empty() {
            candidates
        } else {
            lexical
        }
    }
}

/// `text`'s receiver as upstream's `rustGoReceiverName` reads it: the
/// expression before a trailing `.`, from the right, skipping balanced
/// brackets; empty when it cannot tell.
pub(crate) fn receiver_name(text: &str) -> String {
    static TRAILING_DOT: OnceLock<Regex> = OnceLock::new();
    static LITERAL: OnceLock<Regex> = OnceLock::new();
    let trailing = TRAILING_DOT.get_or_init(|| Regex::new(r"\s*\.\s*$").expect("trailing dot"));
    let literal = LITERAL
        .get_or_init(|| Regex::new(r"^\(\s*&?\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\{").expect("literal"));
    let src: Vec<char> = trailing.replace(text, "").chars().collect();
    let mut out: Vec<char> = Vec::new();
    let mut i = src.len() as isize - 1;
    while i >= 0 {
        let c = src[i as usize];
        if matches!(c, ')' | ']' | '}') {
            let open = match c {
                ')' => '(',
                ']' => '[',
                _ => '{',
            };
            let mut depth = 0usize;
            let mut j = i;
            while j >= 0 {
                let at = src[j as usize];
                if at == c {
                    depth += 1;
                } else if at == open {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j -= 1;
            }
            if j < 0 {
                return String::new();
            }
            // `(JSON{data})`: a parenthesized composite literal names its
            // type.
            if c == ')' && out.is_empty() {
                let slice: String = src[j as usize..=i as usize].iter().collect();
                if let Some(caps) = literal.captures(&slice) {
                    return caps[1].to_string();
                }
            }
            i = j - 1;
        } else if is_word_char(c) || matches!(c, '$' | '.' | ':' | '!') {
            if c != '!' {
                out.insert(0, c);
            }
            i -= 1;
        } else {
            break;
        }
    }
    let joined: String = out.into_iter().collect();
    joined
        .trim_start_matches(['.', ':'])
        .trim_end_matches(['.', ':'])
        .to_string()
}

/// The receiver a Kotlin chain link `….name(` / `?.name {` is written on, or
/// `None` for a call with none (upstream #2168 `kotlinChainReceiver`; the
/// infix-call branch #2174 added is W4-JVM-04's).
fn chain_receiver(reference: &RefView, context: &dyn ResolutionContext) -> Option<String> {
    static SAFE_CALL: OnceLock<Regex> = OnceLock::new();
    static BANG: OnceLock<Regex> = OnceLock::new();
    let line = raw_line(reference, context)?;
    let start = name_start(reference, &line, &['(', '{'], false)?;
    let before = &line[..start];
    if !before.trim_end().ends_with('.') {
        return None;
    }
    let before = SAFE_CALL
        .get_or_init(|| Regex::new(r"\?\s*\.\s*$").expect("safe call"))
        .replace(before, "");
    let before = BANG
        .get_or_init(|| Regex::new(r"!!\s*$").expect("not-null"))
        .replace(&before, "");
    Some(receiver_name(&before))
}

/// The receiver of a Kotlin `calls` reference named like a standard-library
/// method and written as a chain link, or `None` (upstream `kotlinStdChain`).
pub(crate) fn std_chain_receiver(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    (reference.language == Language::Kotlin
        && reference.reference_kind == EdgeKind::Calls
        && KOTLIN_STD_METHODS.contains(&reference.reference_name.as_str()))
    .then(|| chain_receiver(reference, context))
    .flatten()
}

/// Whether a standard-named Kotlin chain link can mean `candidate`: only
/// through a receiver named after its owner (upstream
/// `isKotlinStdChainTarget`).
pub(crate) fn is_std_chain_target(candidate: &Node, receiver: &str) -> bool {
    if !matches!(candidate.kind, NodeKind::Method | NodeKind::Function) {
        return true;
    }
    receiver == "this" || (!receiver.is_empty() && shares_receiver_word(receiver, candidate))
}

/// Upstream `sharesReceiverWord`: the receiver's last link names the owner
/// (letters and digits only, a trailing number dropped, any case), or one of
/// its words is one of the owner's words longer than two letters.
pub(crate) fn shares_receiver_word(receiver: &str, method: &Node) -> bool {
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    let split = SPLIT.get_or_init(|| Regex::new(r"::|\.").expect("owner split"));
    let Some((owner_qualified, _)) = method.qualified_name.rsplit_once("::") else {
        return false;
    };
    let flat = |word: &str| {
        let letters: String = word.chars().filter(char::is_ascii_alphanumeric).collect();
        letters
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .to_ascii_lowercase()
    };
    let receiver_last = receiver.rsplit('.').next().unwrap_or(receiver);
    let owner_last = split
        .split(owner_qualified)
        .last()
        .unwrap_or(owner_qualified);
    if flat(receiver_last) == flat(owner_last) {
        return true;
    }
    let owner: HashSet<String> = crate::name_matcher::split_camel_case(owner_qualified)
        .iter()
        .map(|word| flat(word))
        .filter(|word| word.len() > 2)
        .collect();
    crate::name_matcher::split_camel_case(receiver)
        .iter()
        .any(|word| owner.contains(&flat(word)))
}

/// Key prefix of the Kotlin surface rows (W4-JVM-03): one
/// `project_metadata` row per Kotlin file, so that a sync can tell which of
/// the facts other files' bare calls read a change moved once the old text
/// is gone.
pub const SURFACE_ROW_KEY_PREFIX: &str = "kotlin_surface:";

/// The `project_metadata` key of `file_path`'s surface row.
pub fn surface_row_key(file_path: &str) -> String {
    format!("{SURFACE_ROW_KEY_PREFIX}{file_path}")
}

/// A Kotlin type declaration's head, as the hierarchy walk reads it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct KotlinHead {
    pub name: String,
    pub qualified_name: String,
    pub supertypes: Vec<String>,
}

/// A Kotlin function or method declaration's last parameter type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LambdaParameter {
    pub name: String,
    pub written: String,
}

/// A Kotlin typealias's right side.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AliasRhs {
    pub name: String,
    pub rhs: String,
}

/// What other files' Kotlin resolution reads of one Kotlin file, by the
/// readers resolution uses: its type heads, the receiver types it mentions,
/// its functions' last parameter types, its typealiases (each list in the
/// order of the file), the names its hierarchy walks start from and the
/// calls its lambdas are passed to (each sorted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KotlinSurface {
    pub heads: Vec<KotlinHead>,
    pub function_types: Vec<String>,
    pub extensions: Vec<String>,
    pub lambdas: Vec<LambdaParameter>,
    pub aliases: Vec<AliasRhs>,
    pub starts: Vec<String>,
    pub lambda_calls: Vec<String>,
}

impl KotlinSurface {
    /// The row of the Kotlin file `path` whose text is `source` and whose
    /// nodes are `nodes`.
    pub fn of(path: &str, source: &str, nodes: &[Node]) -> Self {
        static SPLIT: OnceLock<Regex> = OnceLock::new();
        let split = SPLIT.get_or_init(|| Regex::new(r"::|\.").expect("owner split"));
        let facts = crate::source_facts::SourceFacts::new(Arc::from(source));
        let mut ordered: Vec<&Node> = nodes.iter().collect();
        ordered.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let mut surface = Self::default();
        let mut starts: BTreeSet<String> = BTreeSet::new();
        for node in ordered {
            if is_member_class_kind(node.kind) {
                starts.insert(node.name.clone());
            } else if matches!(node.kind, NodeKind::Method | NodeKind::Function)
                && let Some((owner, _)) = node.qualified_name.rsplit_once("::")
                && let Some(last) = split.split(owner).last()
            {
                starts.insert(last.to_string());
            }
            if node.language != Language::Kotlin {
                continue;
            }
            if is_member_class_kind(node.kind) {
                let supertypes = crate::class_heads::class_head_supertypes_in(&facts, node);
                if !supertypes.is_empty() {
                    surface.heads.push(KotlinHead {
                        name: node.name.clone(),
                        qualified_name: node.qualified_name.clone(),
                        supertypes,
                    });
                }
            } else if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                if let Some(written) = lambda_parameter_type(&facts, node) {
                    surface.lambdas.push(LambdaParameter {
                        name: node.name.clone(),
                        written,
                    });
                }
            } else if node.kind == NodeKind::TypeAlias
                && let Some(rhs) = alias_rhs(&facts, node)
            {
                surface.aliases.push(AliasRhs {
                    name: node.name.clone(),
                    rhs,
                });
            }
        }
        let mentions = ReceiverMentions::read(source);
        surface.function_types = mentions.function_types;
        surface.extensions = mentions.extensions;
        let mut lambda_calls: BTreeSet<String> = BTreeSet::new();
        for brace in scan_braces(source) {
            match brace.head {
                BraceHead::Scoped => {}
                BraceHead::Names(names) => starts.extend(names),
                BraceHead::Lambda(call) => {
                    lambda_calls.insert(call);
                }
            }
        }
        if path.ends_with(".gradle.kts") {
            let settings = path == "settings.gradle.kts" || path.ends_with("/settings.gradle.kts");
            starts.insert(if settings { "Settings" } else { "Project" }.to_string());
        }
        surface.starts = starts.into_iter().collect();
        surface.lambda_calls = lambda_calls.into_iter().collect();
        surface
    }

    /// No fact: no row records it.
    pub fn is_empty(&self) -> bool {
        self.heads.is_empty()
            && self.function_types.is_empty()
            && self.extensions.is_empty()
            && self.lambdas.is_empty()
            && self.aliases.is_empty()
            && self.starts.is_empty()
            && self.lambda_calls.is_empty()
    }

    pub fn mentions(&self) -> ReceiverMentions {
        ReceiverMentions {
            function_types: self.function_types.clone(),
            extensions: self.extensions.clone(),
        }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("kotlin surface row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// The names Android's platform ancestry table maps to `name`: the types
/// whose walk reaches it through the table alone.
pub fn platform_subtypes(name: &str) -> Vec<&'static str> {
    const ROOTS: [&str; 14] = [
        "AppCompatActivity",
        "FragmentActivity",
        "ComponentActivity",
        "Activity",
        "ContextThemeWrapper",
        "ContextWrapper",
        "Application",
        "Service",
        "ComponentCallbacks2",
        "Fragment",
        "DialogFragment",
        "AppCompatDialogFragment",
        "BottomSheetDialogFragment",
        "AndroidViewModel",
    ];
    ROOTS
        .into_iter()
        .filter(|root| platform_supers(root).contains(&name))
        .collect()
}

/// The owner a bare call reads for a member whose qualified name is
/// `qualified_name` (a companion's members by the type around it).
pub fn member_owner_name(qualified_name: &str) -> Option<String> {
    member_owner(qualified_name).map(|(owner, _)| owner)
}

/// Whether `node` is a Kotlin / Java class-like declaration an extension
/// receiver's outside test reads (upstream `MEMBER_CLASS_KINDS`).
pub fn is_receiver_class(node: &Node) -> bool {
    is_member_class_kind(node.kind) && matches!(node.language, Language::Kotlin | Language::Java)
}

/// Whether `node` makes its name a project class for a Gradle script's bare
/// call (upstream #2232: `isMethodOwnerKind` or a module).
pub fn is_gradle_owner(node: &Node) -> bool {
    is_method_owner_kind(node) || node.kind == NodeKind::Module
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_scope_reads_package_imports_and_stars() {
        let scope = KotlinFileScope::read(
            "/* package nope */\npackage app.jdbc\n\nimport app.jdbc.tests.assertEquals\nimport `app`.r2dbc.tests.*\nimport okio.Buffer as Buf // a comment\n",
        );
        assert_eq!(scope.package, "app.jdbc");
        assert!(scope.imports.contains("app.jdbc.tests.assertEquals"));
        assert!(scope.imports.contains("okio.Buffer"));
        assert!(scope.stars.contains("app.r2dbc.tests"));
    }

    #[test]
    fn head_names_read_declarations_and_extension_receivers() {
        assert_eq!(
            head_names("\nclass AppTest : Base(1), Runnable by impl"),
            Some(vec![
                "AppTest".to_string(),
                "Base".to_string(),
                "Runnable".to_string()
            ])
        );
        assert_eq!(
            head_names("val table = object : Table() "),
            Some(vec!["Table".to_string()])
        );
        assert_eq!(
            head_names("fun <T> Scope.new(x: T) "),
            Some(vec!["Scope".to_string()])
        );
        assert_eq!(head_names("if (x) "), None);
        assert_eq!(head_names("companion object "), Some(Vec::new()));
    }

    #[test]
    fn listed_capitalized_names_follow_the_lookahead() {
        assert_eq!(
            listed_capitalized_names(" Base, Runnable by x"),
            ["Base", "Runnable"]
        );
        assert_eq!(listed_capitalized_names(" fooBar,"), ["Bar"]);
        // `where` is cut before this runs; on its own the bound is a name.
        assert_eq!(listed_capitalized_names(" Base where T"), ["T"]);
        // `byX` is no `by`; its capital starts a name, as in the regex.
        assert_eq!(listed_capitalized_names(" Base byX, Y"), ["X", "Y"]);
    }

    #[test]
    fn receiver_mentions_read_function_types_and_outside_extensions() {
        let mentions = ReceiverMentions::read(
            "typealias Def<T> = Scope.(Int) -> T\nfun withTables(statement: JdbcTransaction.() -> Unit) {}\nfun StringBuilder.padInt(x: Int) {}\nfun <T> a.B.C<T>.deep() {}\n// Hidden.() -> Unit\n",
        );
        assert_eq!(mentions.function_types, ["Scope", "JdbcTransaction"]);
        assert_eq!(mentions.extensions, ["StringBuilder"]);
    }

    #[test]
    fn receiver_names_read_from_the_right() {
        assert_eq!(receiver_name("    list."), "list");
        assert_eq!(receiver_name("x = foo(bar)."), "foo");
        assert_eq!(receiver_name("a.b.c."), "a.b.c");
        assert_eq!(receiver_name("(JSON{data})."), "JSON");
        assert_eq!(receiver_name("x!!."), "x");
    }

    #[test]
    fn expansion_stops_at_the_limit() {
        let expanded = expand_receiver_types(vec!["A".to_string()], |name| {
            if name == "A" {
                vec!["B".to_string(), "C".to_string()]
            } else {
                Vec::new()
            }
        });
        assert_eq!(expanded.len(), 3);
    }

    fn node(kind: NodeKind, name: &str, qualified: &str, line: i64) -> Node {
        Node {
            id: format!("{}:{qualified}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: qualified.to_string(),
            file_path: "app/build.gradle.kts".to_string(),
            language: Language::Kotlin,
            start_line: line,
            end_line: line,
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
    fn surface_rows_read_heads_mentions_lambdas_aliases_starts_and_calls() {
        let source = "class Child : Base(), Runnable {\n  fun go() { single { get() } }\n}\ntypealias Def = Scope.(Int) -> Unit\nfun single(definition: Def) {}\nfun Foo.ext() = 1\nval t = object : Table() { }\n";
        let nodes = vec![
            node(NodeKind::Class, "Child", "Child", 1),
            node(NodeKind::Method, "go", "Child::go", 2),
            node(NodeKind::TypeAlias, "Def", "Def", 4),
            node(NodeKind::Function, "single", "single", 5),
            node(NodeKind::Method, "ext", "Foo::ext", 6),
        ];
        let surface = KotlinSurface::of("app/build.gradle.kts", source, &nodes);
        assert_eq!(
            surface.heads,
            [KotlinHead {
                name: "Child".to_string(),
                qualified_name: "Child".to_string(),
                supertypes: vec!["Base".to_string(), "Runnable".to_string()],
            }]
        );
        assert_eq!(surface.function_types, ["Scope"]);
        assert_eq!(surface.extensions, ["Foo"]);
        assert_eq!(
            surface.lambdas,
            [LambdaParameter {
                name: "single".to_string(),
                written: "Def".to_string(),
            }]
        );
        assert_eq!(
            surface.aliases,
            [AliasRhs {
                name: "Def".to_string(),
                rhs: "Scope.(Int) -> Unit".to_string(),
            }]
        );
        assert_eq!(
            surface.starts,
            ["Base", "Child", "Foo", "Project", "Runnable", "Table"]
        );
        // `fun go() {` opens a block the call reader takes for a lambda
        // passed to `go`, as upstream's does: `go` names no lambda receiver.
        assert_eq!(surface.lambda_calls, ["go", "single"]);
        assert_eq!(KotlinSurface::decode(&surface.encode()), Some(surface));
    }

    #[test]
    fn std_method_table_is_upstreams() {
        let unique: HashSet<&str> = KOTLIN_STD_METHODS.iter().copied().collect();
        assert_eq!(unique.len(), KOTLIN_STD_METHODS.len());
        assert_eq!(KOTLIN_STD_METHODS.len(), 105);
        assert!(unique.contains("apply") && unique.contains("coerceIn"));
    }
}
