//! C++ type aliases in a declared type: `typedef X Y;` and `using Y = X;`
//! (upstream #2399, `cpp-type-aliases.ts`).
//!
//! The extractor indexes an alias as a `type_alias` node and records nothing
//! about the type it names, so a receiver declared through one used to be
//! looked up by the alias's own name. google/leveldb's `MemTable::Add` calls
//! `table_.Insert(buf)` on a member `Table table_;`, with
//! `typedef SkipList<const char*, KeyComparator> Table;` declared beside it in
//! the class. No class `Table` has an `Insert` (the one class `Table` is the
//! unrelated `leveldb::Table`), so the call fell through to a guess by the
//! receiver's name and reached `HandleTable::Insert`.
//!
//! A name is looked up the way C++ looks it up: from the innermost scope of
//! the code that uses it outwards. That is an alias declared earlier in the
//! calling function, then the caller's class and that class's bases, then the
//! classes and namespaces around it, and the global scope. The first scope
//! that declares the name decides; a class there is the type itself. An alias
//! there is read from its own declaration and followed from the alias's own
//! scope, for a few hops at most. A qualified name is resolved a segment at a
//! time, so `Table::Iterator` is `SkipList::Iterator` and `MemTable::Table` is
//! `SkipList`. Template arguments are dropped. An alias of what only a
//! template argument decides (`using Type = GenericType;` in the template that
//! declares `GenericType`, `typename Traits::Field`) or of a `decltype(…)`
//! names no particular class, and is reported as such.
//!
//! Every answer is a pure function of the files and the node snapshot of the
//! pass; the per-file memos live in [`SourceFacts`](crate::source_facts::SourceFacts).

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use crate::source_facts::SourceFacts;
use crate::types::{RefView, ResolutionContext};

/// How many aliases one name is followed through (`A` → `B` → `C`).
const MAX_HOPS: u32 = 4;
/// How many base classes deep a member is looked for.
const MAX_BASE_DEPTH: u32 = 4;
/// How far past a class's first line its base clause may run.
const HEAD_LINES: usize = 12;

/// What a type named through an alias resolved to: the aliased type as written
/// in `scope` (`["SkipList"]`), then the rest of the name that followed the
/// alias (`["Iterator"]` for `Table::Iterator`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CppAliasedType {
    pub target: Vec<String>,
    pub rest: Vec<String>,
    /// The qualified scope `target` is written in (`""` for the global scope).
    pub scope: String,
    /// An alias on the way named a pointer (`using Field = const FieldDescriptor*;`).
    pub pointer: bool,
    /// The type is a class template of the project (or a class nested in one),
    /// whose specializations can declare members the primary template doesn't.
    pub class_template: bool,
}

/// What a declared C++ type names once the aliases in it are followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CppAliased {
    /// The type is not an alias; the caller keeps its own reading.
    NotAnAlias,
    /// An alias that names no particular class: a template parameter's type
    /// (`using Type = GenericType;`, `typename Traits::Field`), a
    /// `decltype(…)` or a function type, or a chain of aliases too long to
    /// follow.
    Unreadable,
    /// The type the alias names.
    Followed(CppAliasedType),
}

/// A written type's `::` segments, each with whether template arguments
/// followed it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TypeName {
    names: Vec<String>,
    templated: Vec<bool>,
}

/// `SkipList<const char*, Cmp<int>>::Iterator` → `SkipList::Iterator`.
fn strip_template_arguments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The `::` segments of a written type, or `None` when it is not a plain
/// (possibly qualified) type name.
fn cpp_type_name(raw: &str) -> Option<TypeName> {
    static NOISE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let noise = NOISE.get_or_init(|| {
        Regex::new(
            r"\b(?:const|volatile|mutable|typename|template|class|struct|union|enum)\b|[&*]+",
        )
        .expect("type noise")
    });
    let text = noise.replace_all(raw, " ");
    let mut names = Vec::new();
    let mut templated = Vec::new();
    let mut depth = 0usize;
    let mut part = String::new();
    let mut args = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            depth += 1;
            args = true;
        } else if c == '>' {
            depth = depth.saturating_sub(1);
        } else if depth > 0 {
            continue;
        } else if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            names.push(part.trim().to_string());
            templated.push(args);
            part.clear();
            args = false;
        } else {
            part.push(c);
        }
    }
    names.push(part.trim().to_string());
    templated.push(args);
    if names.first().is_some_and(String::is_empty) {
        // A leading `::` names the global scope.
        names.remove(0);
        templated.remove(0);
    }
    if names.is_empty() || !names.iter().all(|name| is_identifier(name)) {
        return None;
    }
    Some(TypeName { names, templated })
}

/// Is a written type a pointer (`Table*`, `const Foo* const`), not counting
/// its template arguments?
pub(crate) fn is_cpp_pointer_type(raw: &str) -> bool {
    strip_template_arguments(raw).contains('*')
}

/// The `::` segments of a written type: `const ns::Foo<T>&` → `["ns", "Foo"]`.
/// `None` when what is written is not a plain (possibly qualified) type name:
/// `unsigned int`, `decltype(…)`, a function type.
#[cfg(test)]
fn cpp_type_segments(raw: &str) -> Option<Vec<String>> {
    cpp_type_name(raw).map(|name| name.names)
}

/// C and C++ declare the names a C++ file can see (a header may be either).
fn is_c_family(node: &Node) -> bool {
    matches!(node.language, Language::Cpp | Language::C)
}

/// Kinds that declare a type name in a scope, besides an alias.
fn is_declaring_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Union
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Namespace
            | NodeKind::Module
    )
}

fn is_class_kind(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Class | NodeKind::Struct | NodeKind::Union)
}

/// Scopes visible from inside the scope `qualified`, innermost first, ending
/// with the global scope.
fn scopes_within(qualified: &str) -> Vec<String> {
    let parts: Vec<&str> = if qualified.is_empty() {
        Vec::new()
    } else {
        qualified.split("::").collect()
    };
    (0..=parts.len())
        .rev()
        .map(|end| parts[..end].join("::"))
        .collect()
}

/// The scope a declaration sits in: `leveldb::MemTable` for
/// `leveldb::MemTable::Table`.
fn parent_scope(qualified: &str) -> &str {
    qualified.rsplit_once("::").map_or("", |(scope, _)| scope)
}

/// The scope the code that makes a reference is written in: its class for a
/// method, itself for a class.
fn caller_scope(caller: Option<&Node>) -> String {
    match caller {
        None => String::new(),
        Some(caller) if caller.kind == NodeKind::File => String::new(),
        Some(caller) if is_declaring_kind(caller.kind) => caller.qualified_name.clone(),
        Some(caller) => parent_scope(&caller.qualified_name).to_string(),
    }
}

/// A node's last line, its first when the end is unknown.
fn end_line(node: &Node) -> i64 {
    if node.end_line != 0 {
        node.end_line
    } else {
        node.start_line
    }
}

/// `line[from..to]` in byte columns, clamped to the line and to character
/// boundaries, as upstream's `slice` clamps.
fn columns(line: &str, from: usize, to: usize) -> &str {
    let floor = |mut at: usize| {
        at = at.min(line.len());
        while !line.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let to = floor(to);
    &line[floor(from).min(to)..to]
}

fn column(value: i64) -> usize {
    usize::try_from(value).unwrap_or(0)
}

/// 0-based line index of a 1-based line number.
fn line_index(line: i64) -> usize {
    usize::try_from(line).unwrap_or(1).saturating_sub(1)
}

fn word_regex(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

/// The text of `node`'s declaration, cut to its columns when they fit the
/// lines.
fn declaration_text(node: &Node, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(&node.file_path)?;
    let start = line_index(node.start_line);
    let end = usize::try_from(end_line(node))
        .unwrap_or(0)
        .min(facts.line_count());
    if start >= end {
        return None;
    }
    let span: Vec<&str> = (start..end).map(|index| facts.line(index)).collect();
    let whole = span.join("\n");
    let mut cut: Vec<&str> = span.clone();
    let last = cut.len() - 1;
    if last == 0 {
        cut[0] = columns(span[0], column(node.start_column), column(node.end_column));
    } else {
        cut[0] = columns(span[0], column(node.start_column), usize::MAX);
        cut[last] = columns(span[last], 0, column(node.end_column));
    }
    let cut = cut.join("\n");
    static DECLARES: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let declares =
        DECLARES.get_or_init(|| Regex::new(r"\b(?:typedef|using)\b").expect("alias keyword"));
    Some(if declares.is_match(&cut) { cut } else { whole })
}

/// Split `text` on commas outside `<>`, `()`, `[]` and `{}`.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (at, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// What an alias of `name` declares in `text`, as written: `SkipList<const
/// char*, KeyComparator>` for `typedef SkipList<const char*, KeyComparator>
/// Table;` and for `using Table = SkipList<const char*, KeyComparator>;`.
/// `None` when `text` declares no alias of that name.
fn aliased_type_in(text: &str, name: &str) -> Option<String> {
    let escaped = regex::escape(name);
    if let Some(using) = word_regex(&format!(
        r"\busing\s+{escaped}\s*(?:\[\[[^\]]*\]\]\s*)?=\s*([^;]+)"
    ))
    .and_then(|pattern| pattern.captures(text))
    {
        return Some(using[1].trim().to_string());
    }
    static TYPEDEF: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let typedef = TYPEDEF.get_or_init(|| Regex::new(r"\btypedef\b([^;]+)").expect("typedef"));
    let typedef = typedef.captures(text)?;
    // `typedef Foo* FooPtr, FooObj;` indexes its first declarator; the type is
    // what precedes that declarator's name.
    let first = split_top_level(typedef.get(1)?.as_str())[0];
    let declared = word_regex(&format!(r"^([\s\S]*?)\b{escaped}\s*$"))?.captures(first)?;
    let written = declared[1].trim();
    (!written.is_empty()).then(|| written.to_string())
}

/// Lines `start_line - 4 ..= start_line - 1` of `facts`' text, the last cut
/// before `start_column`, joined by `separator`: the text a declaration's
/// `template <…>` header ends.
fn text_before(facts: &SourceFacts, start_line: i64, start_column: i64, separator: &str) -> String {
    let at = line_index(start_line);
    let mut parts: Vec<&str> = (at.saturating_sub(3)..at.min(facts.line_count()))
        .map(|index| facts.line(index))
        .collect();
    let own = if at < facts.line_count() {
        facts.line(at)
    } else {
        ""
    };
    parts.push(columns(own, 0, column(start_column)));
    parts.join(separator)
}

/// Is `alias` an alias template (`template <typename T> using Vec =
/// std::vector<T>;`)?
fn is_alias_template(alias: &Node, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(&alias.file_path) else {
        return false;
    };
    static HEADER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let header =
        HEADER.get_or_init(|| Regex::new(r"\btemplate\s*<[^;{}]*>\s*$").expect("template header"));
    header.is_match(&text_before(
        &facts,
        alias.start_line,
        alias.start_column,
        " ",
    ))
}

/// The lines of a function's body that declare an alias (`typedef …;`,
/// `using X = …;`), last first.
fn local_alias_lines(caller: &Node, facts: &SourceFacts) -> Arc<Vec<usize>> {
    facts.cpp_local_alias_lines(&caller.id, |facts| {
        static DECLARES: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let declares = DECLARES
            .get_or_init(|| Regex::new(r"\b(?:typedef\b|using\s+\w+\s*=)").expect("local alias"));
        let last = usize::try_from(end_line(caller))
            .unwrap_or(0)
            .min(facts.line_count());
        let first = line_index(caller.start_line);
        (first..last)
            .rev()
            .filter(|&index| declares.is_match(facts.line(index)))
            .collect()
    })
}

/// The type an alias declared earlier in the calling function names, as
/// written. Those aliases are not indexed, so the function's own lines are
/// read, from the call back to the function's first line. `None` when no
/// such alias declares `name`; `Some(None)` when one does but its type can't
/// be read.
fn local_aliased_type(
    name: &str,
    caller: Option<&Node>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<String>> {
    let caller = caller?;
    if !matches!(caller.kind, NodeKind::Function | NodeKind::Method)
        || caller.file_path != reference.file_path
    {
        return None;
    }
    let facts = context.source_facts(&reference.file_path)?;
    let escaped = regex::escape(name);
    let declares = word_regex(&format!(
        r"\b(?:using\s+{escaped}\s*=|typedef\b[^;]*\b{escaped}\s*[;,])"
    ))?;
    let call_line = line_index(reference.line);
    for &index in local_alias_lines(caller, &facts).iter() {
        if index > call_line {
            continue;
        }
        let line = facts.line(index);
        if line.contains(name) && declares.is_match(line) {
            return Some(aliased_type_in(line, name));
        }
    }
    None
}

/// What a scope declares under a name.
#[derive(Debug, Clone)]
enum Declared {
    Alias(Arc<Node>),
    /// A class, struct or union, by its qualified name.
    Class(String),
    /// An enum, namespace or other non-class type.
    Other,
    /// Aliases naming different types, none of them the calling file's.
    Ambiguous,
}

/// C and C++ declarations named `name` that can stand for a type or scope.
fn type_declarations(name: &str, context: &dyn ResolutionContext) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            is_c_family(node) && (node.kind == NodeKind::TypeAlias || is_declaring_kind(node.kind))
        })
        .collect()
}

fn declared_in(
    scope: &str,
    name: &str,
    named: &[Arc<Node>],
    templated: bool,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Declared> {
    let qualified = if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}::{name}")
    };
    let here: Vec<&Arc<Node>> = named
        .iter()
        .filter(|node| node.qualified_name == qualified)
        // An alias template is only ever named with template arguments.
        .filter(|node| {
            templated || node.kind != NodeKind::TypeAlias || !is_alias_template(node, context)
        })
        .collect();
    if here.is_empty() {
        return None;
    }
    // `typedef struct Node Node;` declares the struct as well.
    if here.iter().any(|node| is_class_kind(node.kind)) {
        return Some(Declared::Class(qualified));
    }
    if here.iter().any(|node| node.kind != NodeKind::TypeAlias) {
        return Some(Declared::Other);
    }
    Some(pick_alias(&here, reference, context).map_or(Declared::Ambiguous, Declared::Alias))
}

/// One of several aliases a scope declares under one name (a translation unit
/// sees one of them): the one in the calling file, then in its header, then
/// any of them when they all name the same type.
fn pick_alias(
    aliases: &[&Arc<Node>],
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    if let [only] = aliases {
        return Some(Arc::clone(only));
    }
    let same_file: Vec<&&Arc<Node>> = aliases
        .iter()
        .filter(|alias| alias.file_path == reference.file_path)
        .collect();
    if let [only] = same_file.as_slice() {
        return Some(Arc::clone(only));
    }
    let stem = |path: &str| -> String {
        match path.rfind('.') {
            Some(dot) if !path[dot + 1..].contains(['/', '\\']) && dot + 1 < path.len() => {
                path[..dot].to_string()
            }
            _ => path.to_string(),
        }
    };
    let own_stem = stem(&reference.file_path);
    let header: Vec<&&Arc<Node>> = aliases
        .iter()
        .filter(|alias| stem(&alias.file_path) == own_stem)
        .collect();
    if let [only] = header.as_slice() {
        return Some(Arc::clone(only));
    }
    let named: HashSet<Option<String>> = aliases
        .iter()
        .map(|alias| {
            let written = declaration_text(alias, context)
                .and_then(|text| aliased_type_in(&text, &alias.name))?;
            Some(cpp_type_name(&written).map_or(written, |name| name.names.join("::")))
        })
        .collect();
    (named.len() == 1 && !named.contains(&None)).then(|| Arc::clone(aliases[0]))
}

/// Add the parameter names of the `template <…>` header that ends right where
/// a declaration starts.
fn collect_template_header(
    facts: &SourceFacts,
    start_line: i64,
    start_column: i64,
    names: &mut BTreeSet<String>,
) {
    let before = text_before(facts, start_line, start_column, "\n");
    let Some(at) = before.rfind("template") else {
        return;
    };
    static HEADER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let header =
        HEADER.get_or_init(|| Regex::new(r"^template\s*<([\s\S]*)>\s*$").expect("template list"));
    let Some(list) = header.captures(&before[at..]) else {
        return;
    };
    static LAST_NAME: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let last_name =
        LAST_NAME.get_or_init(|| Regex::new(r"([A-Za-z_]\w*)\s*$").expect("parameter name"));
    for item in split_top_level(&list[1]) {
        // `typename T = int`, `template <typename> class Policy`, `typename... Ts`, `int N`.
        let stripped = strip_template_arguments(item);
        let declared = stripped.split('=').next().unwrap_or_default();
        if let Some(name) = last_name.captures(declared).map(|caps| caps[1].to_string())
            && name != "typename"
            && name != "class"
        {
            names.insert(name);
        }
    }
}

/// The template parameters in scope where `node` is declared: its own
/// `template <…>` header and those of the class templates around it.
fn template_parameters_around(
    node: &Node,
    context: &dyn ResolutionContext,
) -> Arc<BTreeSet<String>> {
    let Some(facts) = context.source_facts(&node.file_path) else {
        return Arc::default();
    };
    facts.cpp_template_parameters(&node.id, |facts| {
        let mut names = BTreeSet::new();
        collect_template_header(facts, node.start_line, node.start_column, &mut names);
        let mut scope = parent_scope(&node.qualified_name);
        while !scope.is_empty() {
            for class in context.get_nodes_by_qualified_name_shared(scope) {
                if !is_c_family(&class)
                    || !is_class_kind(class.kind)
                    || class.file_path != node.file_path
                    || node.start_line < class.start_line
                    || node.start_line > end_line(&class)
                {
                    continue;
                }
                collect_template_header(facts, class.start_line, class.start_column, &mut names);
            }
            scope = parent_scope(scope);
        }
        names
    })
}

/// The first `:` of `text` that is not half of a `::`.
fn single_colon(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find(|&at| {
        bytes[at] == b':' && (at == 0 || bytes[at - 1] != b':') && bytes.get(at + 1) != Some(&b':')
    })
}

/// The qualified names of the classes `class` derives from, read from the
/// base clause of each of its own declarations (`struct ParseProto2Descriptor
/// : Proto2Descriptor {`) and looked up from the scope the class is declared
/// in.
fn base_classes_of(class: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let mut bases: Vec<String> = Vec::new();
    for declaration in context.get_nodes_by_qualified_name_shared(class) {
        if !is_c_family(&declaration) || !is_class_kind(declaration.kind) {
            continue;
        }
        let Some(facts) = context.source_facts(&declaration.file_path) else {
            continue;
        };
        let own = facts.cpp_class_bases(&declaration.id, |facts| {
            declared_bases(&declaration, facts, context)
        });
        for base in own.iter() {
            if base != class && !bases.contains(base) {
                bases.push(base.clone());
            }
        }
    }
    bases
}

/// The bases one class declaration's head names, looked up from the scope the
/// class is declared in.
fn declared_bases(
    declaration: &Node,
    facts: &SourceFacts,
    context: &dyn ResolutionContext,
) -> Vec<String> {
    let start = line_index(declaration.start_line);
    let end = start.saturating_add(HEAD_LINES).min(facts.line_count());
    if start >= end {
        return Vec::new();
    }
    let mut head: Vec<&str> = (start..end).map(|index| facts.line(index)).collect();
    head[0] = columns(head[0], column(declaration.start_column), usize::MAX);
    let text = head.join("\n");
    let Some(brace) = text.find('{') else {
        return Vec::new();
    };
    // A forward declaration (`class Foo;`) has no base clause of its own.
    if text[..brace].contains(';') {
        return Vec::new();
    }
    let Some(colon) = single_colon(&text[..brace]) else {
        return Vec::new();
    };
    static ACCESS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let access = ACCESS.get_or_init(|| {
        Regex::new(r"\b(?:public|protected|private|virtual)\b").expect("access specifier")
    });
    let scopes = scopes_within(parent_scope(&declaration.qualified_name));
    let mut bases = Vec::new();
    for part in split_top_level(&text[colon + 1..brace]) {
        let Some(name) = cpp_type_name(&access.replace_all(part, " ")) else {
            continue;
        };
        if let Some(base) = class_named(&name.names, &scopes, context)
            && !bases.contains(&base)
        {
            bases.push(base);
        }
    }
    bases
}

/// The qualified name of the class `names` spells, looked up from `scopes`.
fn class_named(
    names: &[String],
    scopes: &[String],
    context: &dyn ResolutionContext,
) -> Option<String> {
    let spelled = names.join("::");
    scopes.iter().find_map(|scope| {
        let qualified = if scope.is_empty() {
            spelled.clone()
        } else {
            format!("{scope}::{spelled}")
        };
        context
            .get_nodes_by_qualified_name_shared(&qualified)
            .iter()
            .any(|node| is_c_family(node) && is_class_kind(node.kind))
            .then_some(qualified)
    })
}

/// `name` declared in class `class` or one of its bases.
#[allow(clippy::too_many_arguments)]
fn member_of(
    class: &str,
    name: &str,
    named: &[Arc<Node>],
    templated: bool,
    reference: &RefView,
    context: &dyn ResolutionContext,
    depth: u32,
    seen: &mut HashSet<String>,
) -> Option<Declared> {
    let own = declared_in(class, name, named, templated, reference, context);
    if own.is_some() || depth >= MAX_BASE_DEPTH {
        return own;
    }
    seen.insert(class.to_string());
    for base in base_classes_of(class, context) {
        if seen.contains(&base) {
            continue;
        }
        let inherited = member_of(
            &base,
            name,
            named,
            templated,
            reference,
            context,
            depth + 1,
            seen,
        );
        if inherited.is_some() {
            return inherited;
        }
    }
    None
}

/// `name` looked up unqualified from `scopes`, innermost first; a class scope
/// includes its bases.
fn lookup_unqualified(
    name: &str,
    templated: bool,
    scopes: &[String],
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Declared> {
    let named = type_declarations(name, context);
    if named.is_empty() {
        return None;
    }
    scopes.iter().find_map(|scope| {
        if scope.is_empty() {
            declared_in("", name, &named, templated, reference, context)
        } else {
            member_of(
                scope,
                name,
                &named,
                templated,
                reference,
                context,
                0,
                &mut HashSet::new(),
            )
        }
    })
}

/// The type a C++ declared type names once the aliases in it are followed,
/// looked up from the code that makes `reference`.
pub(crate) fn resolve_cpp_aliased_type(
    raw_type: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> CppAliased {
    let Some(name) = cpp_type_name(raw_type) else {
        return CppAliased::NotAnAlias;
    };
    let caller = context.get_node_by_id_shared(&reference.from_node_id);
    let scope = caller_scope(caller.as_deref());
    let resolved = match local_aliased_type(&name.names[0], caller.as_deref(), reference, context) {
        Some(local) => {
            let parameters = caller.map_or_else(Arc::default, |caller| {
                template_parameters_around(&caller, context)
            });
            match expand(
                local.as_deref(),
                &name.names[1..],
                &name.templated[1..],
                &scope,
                reference,
                context,
                1,
                Some(&parameters),
            ) {
                Some(followed) => CppAliased::Followed(followed),
                None => CppAliased::Unreadable,
            }
        }
        None => follow(&name, &scope, reference, context, 0),
    };
    // `Table* table` is a pointer whatever `Table` names.
    match resolved {
        CppAliased::Followed(mut followed) if is_cpp_pointer_type(raw_type) => {
            followed.pointer = true;
            CppAliased::Followed(followed)
        }
        other => other,
    }
}

/// The name receiver inference gives a type resolved through aliases: the
/// aliased type's last segment (`SkipList`), as for any declared type, with
/// the nested part of a name reached through the alias kept on it
/// (`SkipList::Iterator`), since that owner is what names the nested type.
pub(crate) fn cpp_aliased_type_name(resolved: &CppAliasedType) -> String {
    resolved
        .target
        .last()
        .into_iter()
        .chain(resolved.rest.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("::")
}

fn follow(
    name: &TypeName,
    scope: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    hops: u32,
) -> CppAliased {
    let scopes = scopes_within(scope);
    let mut found = lookup_unqualified(
        &name.names[0],
        name.templated[0],
        &scopes,
        reference,
        context,
    );
    let mut next = 1usize;
    if found.is_none() && name.names.len() > 1 {
        // Namespaces are not indexed: `detail::buffer_t` from inside `fmt` is
        // found by its spelling in an enclosing scope, longest prefix first.
        let mut end = name.names.len();
        while end >= 2 && found.is_none() {
            let spelled = &name.names[..end];
            let last = &spelled[end - 1];
            let named = type_declarations(last, context);
            let prefix = spelled[..end - 1].join("::");
            found = scopes.iter().find_map(|outer| {
                let within = if outer.is_empty() {
                    prefix.clone()
                } else {
                    format!("{outer}::{prefix}")
                };
                declared_in(
                    &within,
                    last,
                    &named,
                    name.templated[end - 1],
                    reference,
                    context,
                )
            });
            next = end;
            end -= 1;
        }
    }
    // `MemTable::Table`, `Table::Iterator`: one segment at a time.
    while next < name.names.len() {
        let Some(Declared::Class(class)) = &found else {
            break;
        };
        let class = class.clone();
        let segment = &name.names[next];
        found = member_of(
            &class,
            segment,
            &type_declarations(segment, context),
            name.templated[next],
            reference,
            context,
            0,
            &mut HashSet::new(),
        );
        next += 1;
    }
    let Some(Declared::Alias(alias)) = found else {
        return CppAliased::NotAnAlias;
    };
    if hops >= MAX_HOPS {
        return CppAliased::Unreadable;
    }
    let written =
        declaration_text(&alias, context).and_then(|text| aliased_type_in(&text, &alias.name));
    // The aliased type is written in the alias's own scope.
    let parameters = template_parameters_around(&alias, context);
    match expand(
        written.as_deref(),
        &name.names[next..],
        &name.templated[next..],
        parent_scope(&alias.qualified_name),
        reference,
        context,
        hops + 1,
        Some(&parameters),
    ) {
        Some(followed) => CppAliased::Followed(followed),
        None => CppAliased::Unreadable,
    }
}

/// Continue from the type an alias names, as written (`rest` followed the
/// alias in the name; `parameters` are the template parameters where it is
/// written). `None` when that names no particular class.
#[allow(clippy::too_many_arguments)]
fn expand(
    written: Option<&str>,
    rest: &[String],
    rest_templated: &[bool],
    scope: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    hops: u32,
    parameters: Option<&BTreeSet<String>>,
) -> Option<CppAliasedType> {
    // `typename Traits::Field`, `PolicyTemplate<const T>` inside the template
    // that declares `PolicyTemplate`: whatever a template argument makes it.
    let written = written?;
    static TYPENAME: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    if TYPENAME
        .get_or_init(|| Regex::new(r"\btypename\b").expect("typename"))
        .is_match(written)
    {
        return None;
    }
    let target = cpp_type_name(written)?;
    if parameters.is_some_and(|parameters| parameters.contains(&target.names[0])) {
        return None;
    }
    let pointer = is_cpp_pointer_type(written);
    let names: Vec<String> = target.names.iter().chain(rest).cloned().collect();
    let templated: Vec<bool> = target
        .templated
        .iter()
        .chain(rest_templated)
        .copied()
        .collect();
    match follow(
        &TypeName {
            names: names.clone(),
            templated,
        },
        scope,
        reference,
        context,
        hops,
    ) {
        CppAliased::Unreadable => return None,
        CppAliased::Followed(further) => {
            return Some(CppAliasedType {
                pointer: further.pointer || pointer,
                ..further
            });
        }
        CppAliased::NotAnAlias => {}
    }
    let class = class_named(&names, &scopes_within(scope), context);
    let class_template = class.is_some_and(|class| {
        context
            .get_nodes_by_qualified_name_shared(&class)
            .iter()
            .any(|node| {
                is_c_family(node)
                    && is_class_kind(node.kind)
                    && !template_parameters_around(node, context).is_empty()
            })
    });
    Some(CppAliasedType {
        target: target.names,
        rest: rest.to_vec(),
        scope: scope.to_string(),
        pointer,
        class_template,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(raw: &str) -> Option<Vec<String>> {
        cpp_type_segments(raw)
    }

    fn strings(names: &[&str]) -> Option<Vec<String>> {
        Some(names.iter().map(|name| (*name).to_string()).collect())
    }

    #[test]
    fn a_written_type_reads_as_its_segments_without_qualifiers_or_template_arguments() {
        assert_eq!(
            segments("SkipList<const char*, KeyComparator>"),
            strings(&["SkipList"])
        );
        assert_eq!(
            segments("const ::leveldb::SkipList<Key, Cmp<int>>::Iterator&"),
            strings(&["leveldb", "SkipList", "Iterator"])
        );
        assert_eq!(
            segments("typename Base<T>::Iter"),
            strings(&["Base", "Iter"])
        );
        assert_eq!(segments("struct Node*"), strings(&["Node"]));
        assert_eq!(segments("unsigned int"), None);
        assert_eq!(segments("void (*)(int)"), None);
    }

    #[test]
    fn templated_segments_are_marked() {
        let name = cpp_type_name("ns::Table<int>::Iterator").expect("type name");
        assert_eq!(name.names, ["ns", "Table", "Iterator"]);
        assert_eq!(name.templated, [false, true, false]);
    }

    #[test]
    fn an_alias_declaration_reads_as_the_type_it_names() {
        assert_eq!(
            aliased_type_in(
                "typedef SkipList<const char*, KeyComparator> Table;",
                "Table"
            )
            .as_deref(),
            Some("SkipList<const char*, KeyComparator>")
        );
        assert_eq!(
            aliased_type_in(
                "using Table = SkipList<const char*, KeyComparator>;",
                "Table"
            )
            .as_deref(),
            Some("SkipList<const char*, KeyComparator>")
        );
        assert_eq!(
            aliased_type_in(
                "using Field [[deprecated]] = const FieldDescriptor*;",
                "Field"
            )
            .as_deref(),
            Some("const FieldDescriptor*")
        );
        // The first declarator of a typedef is the indexed one.
        assert_eq!(
            aliased_type_in("typedef Foo* FooPtr, FooObj;", "FooPtr").as_deref(),
            Some("Foo*")
        );
        assert_eq!(
            aliased_type_in("typedef Foo* FooPtr, FooObj;", "FooObj"),
            None
        );
        assert_eq!(aliased_type_in("int Table = 3;", "Table"), None);
    }

    #[test]
    fn pointers_ignore_template_arguments() {
        assert!(is_cpp_pointer_type("const Foo* const"));
        assert!(is_cpp_pointer_type("Table*"));
        assert!(!is_cpp_pointer_type("std::vector<int*>"));
    }

    #[test]
    fn commas_split_only_at_the_top_level() {
        assert_eq!(
            split_top_level("A<B, C>, D(e, f), G"),
            ["A<B, C>", " D(e, f)", " G"]
        );
    }

    #[test]
    fn scopes_run_from_the_innermost_to_the_global_scope() {
        assert_eq!(scopes_within("a::b"), ["a::b", "a", ""]);
        assert_eq!(scopes_within(""), [""]);
        assert_eq!(
            parent_scope("leveldb::MemTable::Table"),
            "leveldb::MemTable"
        );
        assert_eq!(parent_scope("Table"), "");
    }

    #[test]
    fn a_single_colon_is_not_half_of_a_scope_operator() {
        assert_eq!(single_colon("struct A : B"), Some(9));
        assert_eq!(single_colon("struct ns::A : ns::B"), Some(13));
        assert_eq!(single_colon("struct ns::A"), None);
    }

    #[test]
    fn columns_clamp_to_the_line_and_character_boundaries() {
        assert_eq!(columns("typedef A B;", 0, 99), "typedef A B;");
        assert_eq!(columns("ab", 5, 9), "");
        // A column inside a character moves back to its start.
        assert_eq!(columns("aé", 0, 2), "a");
    }
}
