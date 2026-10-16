//! Dart calls written without a receiver and the class hierarchy they stay
//! in (upstream #2145), core method names on untyped receivers (#2207) and
//! unnamed extensions (#2237).
//!
//! A bare Dart call (`emit(…)`, `refresh()`) reaches a method only through
//! the class it is written in: that class's own members, what it extends,
//! mixes in or implements, or an extension `on` one of those types, the
//! nearest one winning; outside any class, no method. The hierarchy is read
//! from each type declaration's head, comments and type arguments dropped.
//! An unnamed `extension on X` applies only where it is declared.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::types::{RefView, ResolutionContext};

/// How many types a call site's hierarchy reaches at most (upstream's 40).
pub const HIERARCHY_LIMIT: usize = 40;

/// How many lines past a declaration's first a head is read from
/// (upstream's 40).
const HEAD_LINES: i64 = 40;

/// How many lines below a chain's start its later links are looked for.
const CHAIN_LINES: i64 = 40;

/// Past any real member's depth: an extension member only applies when no
/// instance member does (upstream `DART_EXTENSION_RANK`).
const EXTENSION_RANK: u64 = 1000;

/// No path from the call site's classes to a member's owner.
const OUT_OF_SCOPE: u64 = u64::MAX;

/// Methods of Dart's String, List, Iterable, Map and Set, names a project
/// type rarely carries itself (upstream #2207 `DART_STD_METHODS`).
pub(crate) const DART_STD_METHODS: [&str; 61] = [
    "endsWith",
    "startsWith",
    "contains",
    "split",
    "substring",
    "trim",
    "trimLeft",
    "trimRight",
    "toLowerCase",
    "toUpperCase",
    "replaceAll",
    "replaceFirst",
    "replaceRange",
    "indexOf",
    "lastIndexOf",
    "padLeft",
    "padRight",
    "codeUnitAt",
    "allMatches",
    "firstMatch",
    "hasMatch",
    "addAll",
    "removeAt",
    "removeWhere",
    "removeLast",
    "retainWhere",
    "insertAll",
    "where",
    "whereType",
    "forEach",
    "toList",
    "toSet",
    "join",
    "reduce",
    "fold",
    "any",
    "every",
    "firstWhere",
    "lastWhere",
    "singleWhere",
    "containsKey",
    "containsValue",
    "putIfAbsent",
    "sublist",
    "take",
    "takeWhile",
    "skip",
    "skipWhile",
    "expand",
    "cast",
    "compareTo",
    "elementAt",
    "followedBy",
    "asMap",
    "getRange",
    "setAll",
    "fillRange",
    "shuffle",
    "sort",
    "indexWhere",
    "lastIndexWhere",
];

/// JavaScript's `\s`.
pub(crate) fn is_js_space(c: char) -> bool {
    codegraph_core::generated_header::is_js_whitespace(c)
}

/// An ASCII word character or `$` (`[\w$]`).
pub(crate) fn is_dart_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Whether `name` is written as a Dart identifier (`^[A-Za-z_$][\w$]*$`).
pub(crate) fn is_dart_identifier(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && name.chars().all(is_dart_name_char)
}

/// Whether a node kind is one a Dart type declaration takes (upstream
/// `DART_TYPE_KINDS`; the extractor files mixins and extensions as classes).
pub fn is_dart_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Interface | NodeKind::Enum | NodeKind::Struct | NodeKind::Trait
    )
}

/// A member of a Dart type: a method, or an abstract member written without
/// a body, which the extractor records as a function the type owns
/// (upstream `isDartMember`).
pub fn is_dart_member(node: &Node) -> bool {
    node.kind == NodeKind::Method
        || (node.kind == NodeKind::Function && node.qualified_name.contains("::"))
}

/// The name of the type that owns a member, the last segment before its
/// own name, or `None` for a top-level declaration.
pub fn member_owner(qualified_name: &str) -> Option<&str> {
    let (owner, _) = qualified_name.rsplit_once("::")?;
    Some(owner.rsplit("::").next().unwrap_or(owner))
}

/// Line `line` (1-based) of `file`, as stored.
pub(crate) fn source_line(
    file: &str,
    line: i64,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let facts = context.source_facts(file)?;
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    facts.raw_line(index).map(str::to_string)
}

/// Lines `first..=last` (1-based) of `file`, joined by newlines.
fn source_lines(file: &str, first: i64, last: i64, context: &dyn ResolutionContext) -> String {
    let Some(facts) = context.source_facts(file) else {
        return String::new();
    };
    let mut out = Vec::new();
    for line in first.max(1)..=last {
        let Some(text) = usize::try_from(line - 1)
            .ok()
            .and_then(|index| facts.raw_line(index))
        else {
            break;
        };
        out.push(text.trim_end_matches('\r').to_string());
    }
    out.join("\n")
}

/// Whether a Dart reference is written without a receiver at its call site
/// (upstream `isReceiverLessDartCall`): the extractor keeps one receiver
/// level, so the later links of a chain (`LoginState().withEmail(e)
/// .withPassword(p)`) arrive as bare names, their line showing
/// `.withPassword(` all the same. A spread's `...items()` is no receiver.
pub(crate) fn is_receiver_less_dart_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(line) = source_line(&reference.file_path, reference.line, context) else {
        return true;
    };
    let name = reference.reference_name.as_str();
    let column = usize::try_from(reference.column.max(0)).unwrap_or(0);
    // KEEP-RUST: this extractor places a chain link's reference at the
    // chain's start, so the name is read as a whole word: `ticker.tick(…)`
    // arrives at `ticker`, which begins with `tick`.
    let word_at = |text: &str, at: usize| {
        text.get(at..).is_some_and(|rest| rest.starts_with(name))
            && !text[..at]
                .chars()
                .next_back()
                .is_some_and(is_dart_name_char)
            && !text[at + name.len()..]
                .chars()
                .next()
                .is_some_and(is_dart_name_char)
    };
    let first_word = |text: &str| {
        text.match_indices(name)
            .map(|(at, _)| at)
            .find(|at| word_at(text, *at))
    };
    let found = if word_at(&line, column) {
        Some((line.clone(), column))
    } else if column >= name.len() && word_at(&line, column - name.len()) {
        Some((line.clone(), column - name.len()))
    } else if let Some(at) = first_word(&line) {
        Some((line.clone(), at))
    } else {
        // KEEP-RUST: a link written on a later line (`LoginState()`, then
        // `.withEmail(e)` below it) is read on the first line below that
        // holds its name.
        (1..=CHAIN_LINES).find_map(|offset| {
            let text = source_line(&reference.file_path, reference.line + offset, context)?;
            let at = first_word(&text)?;
            Some((text, at))
        })
    };
    let Some((text, start)) = found else {
        return true;
    };
    let before = text[..start].trim_end_matches(is_js_space);
    if !before.ends_with('.') {
        return true;
    }
    before
        .strip_suffix("...")
        .is_some_and(|rest| !rest.ends_with('.'))
}

/// A Dart `calls` reference written as a bare identifier at its call site.
pub(crate) fn is_dart_bare_call(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    reference.language == Language::Dart
        && reference.reference_kind == EdgeKind::Calls
        && is_dart_identifier(&reference.reference_name)
        && is_receiver_less_dart_call(reference, context)
}

/// What a Dart type declaration's head names: its supertypes (what it
/// extends, mixes in or implements, or an extension or mixin sits `on`) and
/// whether it is an extension, whose supertypes are the types it extends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DartHead {
    pub supers: Vec<String>,
    pub extension: bool,
}

/// `text` with `/* … */` and then `// …` comments replaced by a space
/// (upstream's two replacements).
fn strip_dart_comments(text: &str) -> String {
    let mut blockless = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/*") {
        let Some(end) = rest[at + 2..].find("*/") else {
            break;
        };
        blockless.push_str(&rest[..at]);
        blockless.push(' ');
        rest = &rest[at + 2 + end + 2..];
    }
    blockless.push_str(rest);
    let mut out = String::with_capacity(blockless.len());
    let mut rest = blockless.as_str();
    while let Some(at) = rest.find("//") {
        out.push_str(&rest[..at]);
        out.push(' ');
        let end = rest[at..].find('\n').map_or(rest.len(), |end| at + end);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The position of the first whole word of `words` in `text` (ASCII word
/// boundaries, as JavaScript's `\b`), and the word.
fn first_word<'w>(text: &str, words: &[&'w str]) -> Option<(usize, &'w str)> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let bytes = text.as_bytes();
    (0..text.len())
        .filter(|&at| text.is_char_boundary(at))
        .find_map(|at| {
            if at > 0 && text[..at].chars().next_back().is_some_and(is_word) {
                return None;
            }
            words.iter().find_map(|word| {
                let end = at + word.len();
                (bytes.get(at..end) == Some(word.as_bytes())
                    && !text[end..].chars().next().is_some_and(is_word))
                .then_some((at, *word))
            })
        })
}

/// A Dart type declaration's head read from its text (the declaration's
/// lines, at most its first 41; upstream `dartHeadOf`): comments and type
/// arguments are dropped first, the head ends at its body's `{` or a `;`,
/// and the supertypes are the names after the first `extends`, `with`,
/// `implements`, `on` or `=`.
pub fn dart_head(text: &str) -> DartHead {
    let text = strip_dart_comments(text);
    let mut depth = 0usize;
    let mut flat = String::new();
    for ch in text.chars() {
        if ch == '<' {
            depth += 1;
        } else if ch == '>' {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            if ch == '{' || ch == ';' {
                break;
            }
            flat.push(ch);
        }
    }
    let keyword = first_word(&flat, &["class", "mixin", "extension", "enum"]);
    let head = keyword.map_or(flat.as_str(), |(at, _)| &flat[at..]);
    // The leftmost `extends`, `with`, `implements`, `on` or `=`.
    let word = first_word(head, &["extends", "with", "implements", "on"]);
    let clause_start = match (word, head.find('=')) {
        (Some((at, _)), Some(equals)) if equals < at => Some(equals + 1),
        (Some((at, word)), _) => Some(at + word.len()),
        (None, equals) => equals.map(|equals| equals + 1),
    };
    let clause = clause_start.map_or("", |start| &head[start..]);
    let mut supers = Vec::new();
    let mut chars = clause.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if !(c.is_ascii_alphabetic() || c == '_' || c == '$') {
            continue;
        }
        let mut end = at + c.len_utf8();
        while let Some(&(next, n)) = chars.peek() {
            if !is_dart_name_char(n) {
                break;
            }
            end = next + n.len_utf8();
            chars.next();
        }
        let word = &clause[at..end];
        if !matches!(word, "extends" | "with" | "implements" | "on") {
            supers.push(word.to_string());
        }
    }
    let extension =
        keyword.is_some_and(|(_, word)| word == "extension") && !is_extension_type(head);
    DartHead { supers, extension }
}

/// Whether a head starting `extension` declares an `extension type`
/// (`^extension\s+type\b`).
fn is_extension_type(head: &str) -> bool {
    let Some(rest) = head.strip_prefix("extension") else {
        return false;
    };
    let after = rest.trim_start_matches(is_js_space);
    after.len() < rest.len()
        && after.strip_prefix("type").is_some_and(|after| {
            !after
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Per-pass memos of the Dart hierarchy: each declaration's head, each type
/// name's supertypes, and each call site's hierarchy.
#[derive(Debug, Default)]
pub struct DartTables {
    heads: Mutex<HashMap<String, Arc<DartHead>>>,
    supers: Mutex<HashMap<String, Arc<Vec<String>>>>,
    hierarchies: Mutex<HashMap<(String, i64), Arc<Hierarchy>>>,
}

/// The types around a call site, by supertype distance.
type Hierarchy = HashMap<String, u64>;

impl DartTables {
    pub fn new() -> Self {
        Self::default()
    }
}

/// `decl`'s head, read once per pass.
fn dart_head_of(decl: &Node, context: &dyn ResolutionContext) -> Arc<DartHead> {
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .heads
        .lock()
        .ok()
        .and_then(|memo| memo.get(&decl.id).cloned())
    {
        return hit;
    }
    let last = decl.end_line.min(decl.start_line + HEAD_LINES);
    let head = Arc::new(dart_head(&source_lines(
        &decl.file_path,
        decl.start_line,
        last,
        context,
    )));
    if let Ok(mut memo) = tables.heads.lock() {
        memo.insert(decl.id.clone(), Arc::clone(&head));
    }
    head
}

/// `nodes` in source order (start line, column, id).
fn in_order(mut nodes: Vec<Arc<Node>>) -> Vec<Arc<Node>> {
    nodes.sort_by(|a, b| {
        (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
    });
    nodes
}

/// The names every Dart declaration of `type_name` extends, mixes in,
/// implements or sits `on` (upstream `dartSupertypesOf`), the declarations
/// in path and line order.
fn dart_supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Arc<Vec<String>> {
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .supers
        .lock()
        .ok()
        .and_then(|memo| memo.get(type_name).cloned())
    {
        return hit;
    }
    let mut decls: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(type_name)
        .into_iter()
        .filter(|node| node.language == Language::Dart && is_dart_type_kind(node.kind))
        .collect();
    decls.sort_by(|a, b| {
        (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
    });
    let names = Arc::new(
        decls
            .iter()
            .flat_map(|decl| dart_head_of(decl, context).supers.clone())
            .collect::<Vec<_>>(),
    );
    if let Ok(mut memo) = tables.supers.lock() {
        memo.insert(type_name.to_string(), Arc::clone(&names));
    }
    names
}

/// Every type the classes around a Dart call site are, by supertype
/// distance, at most [`HIERARCHY_LIMIT`] of them (upstream
/// `dartHierarchyAt`).
fn dart_hierarchy_at(reference: &RefView, context: &dyn ResolutionContext) -> Arc<Hierarchy> {
    let tables = context.dart_tables();
    let key = (reference.file_path.clone(), reference.line);
    if let Some(hit) = tables
        .hierarchies
        .lock()
        .ok()
        .and_then(|memo| memo.get(&key).cloned())
    {
        return hit;
    }
    let mut depths: Hierarchy = HashMap::new();
    let mut queue: VecDeque<(String, u64)> =
        in_order(context.get_nodes_in_file_shared(&reference.file_path))
            .into_iter()
            .filter(|node| {
                is_dart_type_kind(node.kind)
                    && node.start_line <= reference.line
                    && node.end_line >= reference.line
            })
            .map(|node| (node.name.clone(), 0))
            .collect();
    while depths.len() < HIERARCHY_LIMIT {
        let Some((name, depth)) = queue.pop_front() else {
            break;
        };
        if depths.contains_key(&name) {
            continue;
        }
        depths.insert(name.clone(), depth);
        for sup in dart_supertypes_of(&name, context).iter() {
            queue.push_back((sup.clone(), depth + 1));
        }
    }
    let depths = Arc::new(depths);
    if let Ok(mut memo) = tables.hierarchies.lock() {
        memo.insert(key, Arc::clone(&depths));
    }
    depths
}

/// How many supertype steps separate the classes around a Dart call site
/// from `method`'s owner (upstream `dartMemberDepth`): 0 for their own
/// member or a top-level function, [`OUT_OF_SCOPE`] when the owner is not in
/// their hierarchy. An extension's member is in scope when the extension is
/// `on` a type of the hierarchy, ranked after every real member.
fn dart_member_depth(method: &Node, reference: &RefView, context: &dyn ResolutionContext) -> u64 {
    let Some(owner) = member_owner(&method.qualified_name) else {
        return 0;
    };
    let hierarchy = dart_hierarchy_at(reference, context);
    if let Some(&own) = hierarchy.get(owner) {
        return own;
    }
    let decl = in_order(context.get_nodes_in_file_shared(&method.file_path))
        .into_iter()
        .find(|node| {
            node.name == owner
                && is_dart_type_kind(node.kind)
                && node.start_line <= method.start_line
                && node.end_line >= method.end_line
        });
    let Some(decl) = decl else {
        return OUT_OF_SCOPE;
    };
    let head = dart_head_of(&decl, context);
    if !head.extension {
        return OUT_OF_SCOPE;
    }
    head.supers
        .iter()
        .filter_map(|on| hierarchy.get(on))
        .min()
        .map_or(OUT_OF_SCOPE, |on| EXTENSION_RANK + on)
}

/// Whether a bare Dart call can reach `method` (upstream
/// `isDartMethodInScope`): a member of the class it is written in, or of
/// what that class extends, mixes in or implements, an extension `on` one of
/// those included. Outside any class (`main`), no method.
pub(crate) fn is_dart_method_in_scope(
    method: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    dart_member_depth(method, reference, context) < OUT_OF_SCOPE
}

/// Of the in-scope members a bare Dart call could mean, the nearest
/// (upstream `nearestDartMembers`): a subclass's override, or the class that
/// implements what an interface only declares.
pub(crate) fn nearest_dart_members(
    candidates: Vec<Arc<Node>>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Vec<Arc<Node>> {
    if candidates
        .iter()
        .filter(|node| is_dart_member(node))
        .count()
        < 2
    {
        return candidates;
    }
    let depths: HashMap<String, u64> = candidates
        .iter()
        .filter(|node| is_dart_member(node))
        .map(|node| (node.id.clone(), dart_member_depth(node, reference, context)))
        .collect();
    let nearest = depths.values().copied().min().unwrap_or(OUT_OF_SCOPE);
    candidates
        .into_iter()
        .filter(|node| depths.get(&node.id).is_none_or(|depth| *depth == nearest))
        .collect()
}

/// Leading annotations and `//` comments of a declaration's head.
fn leading_annotations() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^[\s\x{FEFF}]*(?:(?:@[0-9A-Za-z_$.]+(?:[\s\x{FEFF}]*\((?:[^()]|\([^()]*\))*\))?|//[^\n]*)[\s\x{FEFF}]*)*",
        )
        .expect("dart annotations")
    })
}

/// An `extension` head with no name: `extension on X`, or a generic
/// `extension<T> on X`.
fn unnamed_extension() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^extension[\s\x{FEFF}]*(?:<(?:[^<>]|<(?:[^<>]|<[^<>]*>)*>)*>)?[\s\x{FEFF}]*on(?:[^0-9A-Za-z_]|$)",
        )
        .expect("dart unnamed extension")
    })
}

/// The Dart `extension` declaration a class node stands for, read from its
/// head past the annotations it starts with (upstream `dartExtensionDecl`):
/// `Some(false)` for an unnamed `extension on X`, `None` for a class, mixin
/// or enum, or an `extension type`.
fn dart_extension_named(node: &Node, context: &dyn ResolutionContext) -> Option<bool> {
    if node.language != Language::Dart || node.kind != NodeKind::Class {
        return None;
    }
    let text = source_lines(
        &node.file_path,
        node.start_line,
        node.end_line.min(node.start_line + 5),
        context,
    );
    extension_named_in(&text)
}

/// [`dart_extension_named`] of a declaration's first lines.
fn extension_named_in(text: &str) -> Option<bool> {
    let head = &text[leading_annotations()
        .find(text)
        .map_or(0, |found| found.end())..];
    let rest = head.strip_prefix("extension")?;
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    if is_extension_type(head) {
        return None;
    }
    Some(!unnamed_extension().is_match(head))
}

/// Whether a Dart method belongs to an unnamed `extension on X`, which
/// applies only in its own library (upstream `isDartUnnamedExtensionMember`;
/// the resolver asks [`crate::dart_libraries::in_same_dart_library`]).
pub(crate) fn is_dart_unnamed_extension_member(
    method: &Node,
    context: &dyn ResolutionContext,
) -> bool {
    if method.language != Language::Dart || method.kind != NodeKind::Method {
        return false;
    }
    let Some((owner_qn, _)) = method.qualified_name.rsplit_once("::") else {
        return false;
    };
    if owner_qn.is_empty() {
        return false;
    }
    in_order(context.get_nodes_in_file_shared(&method.file_path))
        .into_iter()
        .find(|node| {
            node.qualified_name == owner_qn
                && node.kind == NodeKind::Class
                && node.start_line <= method.start_line
                && node.end_line >= method.start_line
        })
        .is_some_and(|owner| dart_extension_named(&owner, context) == Some(false))
}

/// `extension Name<T> on Type {` on a declaration's first line, the type.
fn extension_on() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:^|[^0-9A-Za-z_])extension[\s\x{FEFF}]+[0-9A-Za-z_]*[\s\x{FEFF}]*(?:<[^>]*>)?[\s\x{FEFF}]*on[\s\x{FEFF}]+([0-9A-Za-z_<>, ?]+?)[\s\x{FEFF}]*\{",
        )
        .expect("dart extension on")
    })
}

/// The owner a receiver must be named after for a Dart method: for an
/// extension's member, the type it is `on` (upstream #2207
/// `receiverNamesOwner`), as `Owner::name`; `None` keeps the method's own.
pub(crate) fn dart_receiver_owner(
    method: &Node,
    context: &dyn ResolutionContext,
) -> Option<String> {
    if method.language != Language::Dart {
        return None;
    }
    let owner = member_owner(&method.qualified_name).filter(|owner| !owner.is_empty())?;
    let mut decls: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(owner)
        .into_iter()
        .filter(|node| node.language == Language::Dart && node.file_path == method.file_path)
        .collect();
    decls.sort_by(|a, b| {
        (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
    });
    let decl = decls.first()?;
    let line = source_line(&decl.file_path, decl.start_line, context)?;
    Some(format!("{}::{}", extension_on_in(&line)?, method.name))
}

/// The type an `extension Name on Type {` line is on, type arguments and
/// `?` dropped (`Rx<String?>` reads `RxString`).
fn extension_on_in(line: &str) -> Option<String> {
    let on = extension_on().captures(line)?.get(1)?.as_str();
    Some(
        on.chars()
            .filter(|c| !matches!(c, '<' | '>' | ',' | ' ' | '?'))
            .collect(),
    )
}

/// What another file's Dart resolution reads of one type declaration's head
/// (W4-DART-01): the supertypes it names, whether it is an extension, and
/// the type its name's first declaration line puts an extension `on`, type
/// arguments kept ([`dart_receiver_owner`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HeadEntry {
    pub name: String,
    pub extension: bool,
    pub supers: Vec<String>,
    pub on: Option<String>,
}

/// Key prefix of the Dart head rows (W4-DART-01).
pub const HEAD_ROW_KEY_PREFIX: &str = "dart_heads:";

/// The `project_metadata` key of `file_path`'s Dart head row.
pub fn head_row_key(file_path: &str) -> String {
    format!("{HEAD_ROW_KEY_PREFIX}{file_path}")
}

/// A Dart file's head row: every type declaration's head, in source order
/// (positions are no part of it, so an edit above a class changes nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadRow {
    /// The row of a Dart file whose text is `source` and whose nodes are
    /// `nodes`: what [`dart_head`] reads of each of its type declarations.
    pub fn of(source: &str, nodes: &[Node]) -> Self {
        let lines: Vec<&str> = source.split('\n').collect();
        let mut decls: Vec<&Node> = nodes
            .iter()
            .filter(|node| node.language == Language::Dart && is_dart_type_kind(node.kind))
            .collect();
        decls.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let mut in_file: Vec<&Node> = nodes
            .iter()
            .filter(|node| node.language == Language::Dart)
            .collect();
        in_file.sort_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        });
        let line_at = |line: i64| -> &str {
            usize::try_from(line - 1)
                .ok()
                .and_then(|index| lines.get(index))
                .map_or("", |text| text.trim_end_matches('\r'))
        };
        let entries = decls
            .into_iter()
            .map(|decl| {
                let first = usize::try_from(decl.start_line.max(1) - 1).unwrap_or(0);
                let last = usize::try_from(decl.end_line.min(decl.start_line + HEAD_LINES))
                    .unwrap_or(0)
                    .min(lines.len());
                let text = lines
                    .get(first..last.max(first))
                    .unwrap_or_default()
                    .iter()
                    .map(|line| line.trim_end_matches('\r'))
                    .collect::<Vec<_>>()
                    .join("\n");
                let head = dart_head(&text);
                // As `dart_receiver_owner` reads it: the first declaration of
                // the name in the file, of any kind.
                let on = in_file
                    .iter()
                    .find(|node| node.name == decl.name)
                    .and_then(|first| extension_on_in(line_at(first.start_line)));
                HeadEntry {
                    name: decl.name.clone(),
                    extension: head.extension,
                    supers: head.supers,
                    on,
                }
            })
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("dart head row encodes")
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values from upstream's `dartHeadOf` on the same text.
    #[test]
    fn a_dart_head_names_its_supertypes_past_comments_and_type_arguments() {
        let cases: [(&str, &[&str], bool); 9] = [
            ("class CounterCubit extends Cubit<int> {", &["Cubit"], false),
            (
                "abstract class BlocBase<S> implements Emittable<S> {",
                &["Emittable"],
                false,
            ),
            ("mixin SingleMixin on Listenable {", &["Listenable"], false),
            (
                "class ListNotifierSingle = ListNotifier with SingleMixin;",
                &["ListNotifier", "SingleMixin"],
                false,
            ),
            (
                "extension NotifierX<S> on AnyNotifier<S> {",
                &["AnyNotifier"],
                true,
            ),
            (
                "class Notifier<\n  // the state\n  S\n> //\n    extends\n        AnyNotifier<\n          S\n        > {",
                &["AnyNotifier"],
                false,
            ),
            ("extension on String {", &["String"], true),
            (
                "extension type Meters(int value) implements num {",
                &["num"],
                false,
            ),
            (
                "@freezed /* x */ sealed class A extends B with C, D implements E {",
                &["B", "C", "D", "E"],
                false,
            ),
        ];
        for (text, supers, extension) in cases {
            let found = dart_head(text);
            assert_eq!(found.supers, supers, "{text:?}");
            assert_eq!(found.extension, extension, "{text:?}");
        }
    }

    /// Expected values from upstream's `dartHeadOf`, `dartExtensionDecl` and
    /// `receiverNamesOwner` patterns, run under Node on the same text
    /// (/tmp/v055-dart-tools/heads.js).
    #[test]
    fn the_head_readers_match_upstreams_patterns() {
        type Case<'a> = (&'a str, &'a [&'a str], bool, Option<bool>, Option<&'a str>);
        let cases: [Case; 27] = [
            (
                "class A extends B<C> with D implements E, F {",
                &["B", "D", "E", "F"],
                false,
                None,
                None,
            ),
            (
                "abstract base class X<T extends Y<Z>> extends W {",
                &["W"],
                false,
                None,
                None,
            ),
            (
                "class Q = R with S, T;",
                &["R", "S", "T"],
                false,
                None,
                None,
            ),
            (
                "mixin M on Base, Other {",
                &["Base", "Other"],
                false,
                None,
                None,
            ),
            (
                "enum E with M implements I { a, b }",
                &["M", "I"],
                false,
                None,
                None,
            ),
            (
                "extension on String {",
                &["String"],
                true,
                Some(false),
                Some("String"),
            ),
            (
                "extension<T extends Object> on List<T> {",
                &["List"],
                true,
                Some(false),
                None,
            ),
            (
                "extension Named<T> on Map<String, T> {",
                &["Map"],
                true,
                Some(true),
                Some("MapStringT"),
            ),
            (
                "@internal\nextension ContainerReadElement on ProviderContainer {",
                &["ProviderContainer"],
                true,
                Some(true),
                None,
            ),
            (
                "@Deprecated('x (y)')\n// note\nextension on Foo {",
                &["Foo"],
                true,
                Some(false),
                None,
            ),
            (
                "extension type Meters(int v) implements num {",
                &["num"],
                false,
                None,
                None,
            ),
            (
                "extension type const Id(String v) {",
                &[],
                false,
                None,
                None,
            ),
            ("class Plain {", &[], false, None, None),
            (
                "class /* c */ A /* d */ extends /* e */ B {",
                &["B"],
                false,
                None,
                None,
            ),
            (
                "class A extends B // trailing\n    implements C {",
                &["B", "C"],
                false,
                None,
                None,
            ),
            ("sealed class Result<T> {", &[], false, None, None),
            (
                "class A extends B<Map<String, List<int>>> with C {",
                &["B", "C"],
                false,
                None,
                None,
            ),
            ("class $Foo extends $Bar {", &["$Bar"], false, None, None),
            (
                "class A implements B, C<D> {",
                &["B", "C"],
                false,
                None,
                None,
            ),
            ("mixin class MC {", &[], false, None, None),
            ("extensionFoo on X {", &["X"], false, None, None),
            ("class A extends B with on {", &["B"], false, None, None),
            ("/* x */ class C extends D {", &["D"], false, None, None),
            (
                "extension X on Y<Z?> {",
                &["Y"],
                true,
                Some(true),
                Some("YZ"),
            ),
            ("class A<T = int> extends B {", &["B"], false, None, None),
            ("extension\non Foo {", &["Foo"], true, Some(false), None),
            (
                "  extension   on   Bar  {",
                &["Bar"],
                true,
                Some(false),
                Some("Bar"),
            ),
        ];
        for (text, supers, extension, named, on) in cases {
            let head = dart_head(text);
            assert_eq!(head.supers, supers, "{text:?}");
            assert_eq!(head.extension, extension, "{text:?}");
            assert_eq!(extension_named_in(text), named, "{text:?}");
            let first = text.split('\n').next().unwrap_or_default();
            assert_eq!(extension_on_in(first).as_deref(), on, "{text:?}");
        }
    }

    #[test]
    fn a_dart_identifier_is_a_word_or_dollar_name() {
        assert!(is_dart_identifier("emit"));
        assert!(is_dart_identifier("$foo"));
        assert!(is_dart_identifier("_x1"));
        assert!(!is_dart_identifier("a.b"));
        assert!(!is_dart_identifier("1a"));
    }
}
