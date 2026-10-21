//! The supertypes a class declaration names in its own head, read from source
//! (upstream #2180 `declaredSupertypes` and the per-language head readers it
//! uses). Source is read rather than the graph's `extends` edges because a
//! first resolution pass has no edges yet, and a sync must see what a fresh
//! index sees.

use std::sync::OnceLock;

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use crate::source_facts::SourceFacts;
use crate::types::ResolutionContext;

/// Lines `start_line - 1 .. start_line - 1 + count` of `node`'s file joined
/// by `separator`, clamped to the file; empty when the file is unreadable.
fn head_lines(
    node: &Node,
    count: usize,
    separator: &str,
    context: &dyn ResolutionContext,
) -> String {
    let Some(facts) = context.source_facts(&node.file_path) else {
        return String::new();
    };
    let start = usize::try_from(node.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = start.saturating_add(count).min(facts.line_count());
    if start >= end {
        return String::new();
    }
    facts.join_lines(start, end, separator)
}

/// Lines `start_line - 1 .. start_line - 1 + count` of `node` in `facts`
/// joined by `separator`, clamped to the file ([`head_lines`] over facts
/// already read).
fn facts_lines(facts: &SourceFacts, node: &Node, count: usize, separator: &str) -> String {
    let start = usize::try_from(node.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = start.saturating_add(count).min(facts.line_count());
    if start >= end {
        return String::new();
    }
    facts.join_lines(start, end, separator)
}

/// `text` with `/* … */` and `// …` comments blanked.
fn strip_c_comments(text: &str) -> String {
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    let block = BLOCK.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").expect("block comment"));
    let line = LINE.get_or_init(|| Regex::new(r"//[^\n]*").expect("line comment"));
    line.replace_all(&block.replace_all(text, " "), " ")
        .into_owned()
}

/// Whether [`declared_supertypes`] of a `language` declaration reads only
/// that declaration's own head, so a class-head row's entry for it
/// ([`declaration_heads`]) is its answer: the Java family and the
/// three-line heads, not the languages that gather a name's declarations.
pub fn reads_own_head(language: Language) -> bool {
    matches!(
        language,
        Language::Java
            | Language::CSharp
            | Language::Kotlin
            | Language::Pascal
            | Language::Python
            | Language::Ruby
            | Language::Php
            | Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
    )
}

/// `project_metadata` key prefix of a file's class-head row (W5-07).
pub const ROW_KEY_PREFIX: &str = "class_heads:";

/// The `project_metadata` key of `path`'s class-head row.
pub fn row_key(path: &str) -> String {
    format!("{ROW_KEY_PREFIX}{path}")
}

/// One type declaration of a file: the supertypes its own head writes
/// ([`declaration_heads`]), as a sync's class-head row keeps it (W5-07).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeadEntry {
    pub language: Language,
    pub name: String,
    pub heads: Vec<String>,
}

impl Ord for HeadEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.language.as_str(), &self.name, &self.heads).cmp(&(
            other.language.as_str(),
            &other.name,
            &other.heads,
        ))
    }
}

impl PartialOrd for HeadEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A file's class-head row: its entries, sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeadsRow {
    pub entries: Vec<HeadEntry>,
}

impl HeadsRow {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn decode(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// The supertypes `class` names in its own head: Pascal `TX = class(TBase)`,
/// Python `class X(Base):`, Ruby `class X < Base`, PHP / TS / JS
/// `extends Base`, and the Java-family, Dart, Swift, Objective-C and Scala
/// heads.
pub(crate) fn declared_supertypes(class: &Node, context: &dyn ResolutionContext) -> Vec<String> {
    // A declaration whose own head is what it reads: the sync's row of its
    // unchanged file holds that answer already (W5-08).
    if reads_own_head(class.language)
        && let Some(heads) = context.recorded_heads(class)
    {
        return heads;
    }
    match class.language {
        Language::Java | Language::CSharp | Language::Kotlin => {
            return class_head_supertypes(class, context);
        }
        Language::Dart => return dart_supertypes_of(&class.name, context),
        Language::Swift => return swift_supertypes_of(&class.name, context),
        Language::ObjC => {
            return crate::objc_scope::objc_supertypes_of(&class.name, context).to_vec();
        }
        Language::Scala => return scala_supertypes_of(&class.name, context),
        _ => {}
    }
    three_line_supertypes(class.language, &head_lines(class, 3, " ", context))
}

/// The supertypes the first three lines of a Pascal, Python, Ruby, PHP or
/// TS/JS type declaration, `head`, name.
fn three_line_supertypes(language: Language, head: &str) -> Vec<String> {
    static PASCAL: OnceLock<Regex> = OnceLock::new();
    static PYTHON: OnceLock<Regex> = OnceLock::new();
    static PYTHON_KEYWORD: OnceLock<Regex> = OnceLock::new();
    static RUBY: OnceLock<Regex> = OnceLock::new();
    static EXTENDS: OnceLock<Regex> = OnceLock::new();
    let clause = match language {
        Language::Pascal => PASCAL
            .get_or_init(|| Regex::new(r"(?i)=\s*class\s*\(([^)]*)\)").expect("pascal head"))
            .captures(head)
            .map(|c| c[1].to_string()),
        Language::Python => PYTHON
            .get_or_init(|| Regex::new(r"\bclass\s+\w+\s*\(([^)]*)\)").expect("python head"))
            .captures(head)
            .map(|c| {
                PYTHON_KEYWORD
                    .get_or_init(|| Regex::new(r"\w+\s*=\s*[\w.]+").expect("python keyword"))
                    .replace_all(&c[1], "")
                    .into_owned()
            }),
        Language::Ruby => RUBY
            .get_or_init(|| Regex::new(r"\bclass\s+[\w:]+\s*<\s*([\w:]+)").expect("ruby head"))
            .captures(head)
            .map(|c| c[1].to_string()),
        Language::Php
        | Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Jsx => EXTENDS
            .get_or_init(|| Regex::new(r"\bextends\s+([\w.\\]+)").expect("extends head"))
            .captures(head)
            .map(|c| c[1].to_string()),
        _ => None,
    };
    clause.map(|clause| head_names(&clause)).unwrap_or_default()
}

/// A node an S1 walk starts from or steps to in the languages whose heads it
/// reads per declaration: a class, struct, union or interface.
fn is_method_owner_kind(node: &Node) -> bool {
    matches!(
        node.kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Interface
    )
}

/// The supertypes `decl` writes in its own head: what one declaration adds
/// to [`declared_supertypes`] of its type, read from its own file's
/// `facts`, or `None` for a node no such read covers (the sync's
/// `class_heads` rows hold these, W5-07). In the Java-family, Pascal,
/// Python, Ruby, PHP and TS/JS heads a class, struct, union or interface is
/// read as itself; Dart, Swift, Objective-C and Scala read every declaration
/// of the type's name, one of which `decl` is, and [`name_supertypes`] adds
/// what the name alone brings.
pub fn declaration_heads(facts: &SourceFacts, decl: &Node) -> Option<Vec<String>> {
    match decl.language {
        Language::Java | Language::CSharp | Language::Kotlin => {
            is_method_owner_kind(decl).then(|| class_head_supertypes_in(facts, decl))
        }
        Language::Dart => is_dart_type(decl).then(|| dart_declaration_supertypes(facts, decl)),
        Language::Swift => crate::swift_scope::is_swift_type_kind(decl.kind)
            .then(|| swift_head_in(facts, decl).supers),
        Language::ObjC => (decl.kind == NodeKind::Class).then(|| {
            crate::objc_scope::written_superclass_of(facts, decl)
                .into_iter()
                .collect()
        }),
        Language::Scala => is_scala_type(decl).then(|| scala_declaration_supertypes(facts, decl)),
        Language::Pascal
        | Language::Python
        | Language::Ruby
        | Language::Php
        | Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Jsx => is_method_owner_kind(decl)
            .then(|| three_line_supertypes(decl.language, &facts_lines(facts, decl, 3, " "))),
        _ => None,
    }
}

/// What [`declared_supertypes`] reads for a `language` type named `name`
/// beyond its declarations' own heads: a Swift standard collection's
/// refinements or a UIKit / AppKit class's superclass, and for
/// Objective-C that superclass when no declaration writes one
/// (`written` is false).
pub fn name_supertypes(language: Language, name: &str, written: bool) -> Vec<String> {
    match language {
        Language::Swift => match crate::swift_scope::swift_std_supers(name) {
            Some(supers) => supers.iter().map(|s| (*s).to_string()).collect(),
            None => crate::swift_scope::objc_system_super(name)
                .map(str::to_string)
                .into_iter()
                .collect(),
        },
        Language::ObjC if !written => crate::objc_scope::system_superclass(name)
            .map(str::to_string)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// The simple names in a supertype clause: each `[A-Za-z_][\w.:\\]*` by its
/// last `::` / `.` / `\` segment, without `metaclass` and `object`.
fn head_names(text: &str) -> Vec<String> {
    static NAME: OnceLock<Regex> = OnceLock::new();
    NAME.get_or_init(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_.:\\]*").expect("head name"))
        .find_iter(text)
        .filter_map(|m| {
            m.as_str()
                .rsplit([':', '.', '\\'])
                .next()
                .filter(|last| !last.is_empty() && *last != "metaclass" && *last != "object")
                .map(str::to_string)
        })
        .collect()
}

/// The simple names a Java / C# / Kotlin class declaration's head extends or
/// implements (upstream `classHeadSupertypes`): the head up to its body at
/// type-argument and parenthesis depth 0, then the `extends` / `implements`
/// clause or the part after a `:`, without a C# `where` constraint.
pub(crate) fn class_head_supertypes(class: &Node, context: &dyn ResolutionContext) -> Vec<String> {
    head_supertypes(&head_lines(class, 9, " ", context))
}

/// [`class_head_supertypes`] of the declaration starting on `class`'s line of
/// a file whose facts are `facts` (for the sync's Kotlin surface rows, which
/// read the same heads).
pub(crate) fn class_head_supertypes_in(
    facts: &crate::source_facts::SourceFacts,
    class: &Node,
) -> Vec<String> {
    let start = usize::try_from(class.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = start.saturating_add(9).min(facts.line_count());
    if start >= end {
        return head_supertypes("");
    }
    head_supertypes(&facts.join_lines(start, end, " "))
}

/// The supertypes the joined head lines `joined` name.
fn head_supertypes(joined: &str) -> Vec<String> {
    static COMMENTS: OnceLock<Regex> = OnceLock::new();
    static KEYWORD_CLAUSE: OnceLock<Regex> = OnceLock::new();
    static COLON_CLAUSE: OnceLock<Regex> = OnceLock::new();
    static WHERE: OnceLock<Regex> = OnceLock::new();
    let joined = COMMENTS
        .get_or_init(|| Regex::new(r"//[^\n]*|/\*.*?\*/").expect("head comments"))
        .replace_all(joined, " ");
    let mut depth = 0usize;
    let mut head = String::new();
    for ch in joined.chars() {
        if ch == '{' && depth == 0 {
            break;
        }
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => head.push(ch),
            _ => {}
        }
    }
    let clause = KEYWORD_CLAUSE
        .get_or_init(|| Regex::new(r"(?s)\b(?:extends|implements)\b(.*)$").expect("keyword clause"))
        .captures(&head)
        .or_else(|| {
            COLON_CLAUSE
                .get_or_init(|| {
                    Regex::new(r"(?s)\b(?:class|interface|struct|record|object)\s+\w+[^:]*:(.*)$")
                        .expect("colon clause")
                })
                .captures(&head)
        })
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let clause = WHERE
        .get_or_init(|| Regex::new(r"(?s)\bwhere\b.*$").expect("where clause"))
        .replace(&clause, "");
    clause_type_names(&clause)
}

/// `([A-Z]\w*)\s*(?=,|$|\bimplements\b)` over `clause`: a capitalized word
/// followed by a comma, the end, or the word `implements`.
fn clause_type_names(clause: &str) -> Vec<String> {
    let bytes = clause.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut names = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_uppercase() {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        let mut next = end;
        while next < bytes.len() && bytes[next].is_ascii_whitespace() {
            next += 1;
        }
        let rest = &clause[next..];
        let implements = next > end
            && rest.starts_with("implements")
            && !rest
                .as_bytes()
                .get("implements".len())
                .copied()
                .is_some_and(is_word);
        if next == bytes.len() || rest.starts_with(',') || implements {
            names.push(clause[at..end].to_string());
            at = next;
        } else {
            at += 1;
        }
    }
    names
}

/// Upstream `DART_TYPE_KINDS`.
fn is_dart_type(node: &Node) -> bool {
    node.language == Language::Dart
        && matches!(
            node.kind,
            NodeKind::Class
                | NodeKind::Interface
                | NodeKind::Enum
                | NodeKind::Struct
                | NodeKind::Trait
        )
}

/// The simple names a Dart type's declarations extend, mix in, implement or
/// sit `on` (upstream `dartSupertypesOf` / `dartHeadOf`).
fn dart_supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let mut names = Vec::new();
    for decl in context.get_nodes_by_name_shared(type_name) {
        if !is_dart_type(&decl) {
            continue;
        }
        if let Some(facts) = context.source_facts(&decl.file_path) {
            names.extend(dart_declaration_supertypes(&facts, &decl));
        }
    }
    names
}

/// The simple names one Dart type declaration extends, mixes in, implements
/// or sits `on`, read from its own head (the per-declaration part of
/// [`dart_supertypes_of`]).
fn dart_declaration_supertypes(facts: &SourceFacts, decl: &Node) -> Vec<String> {
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let keyword = KEYWORD
        .get_or_init(|| Regex::new(r"\b(?:class|mixin|extension|enum)\b").expect("dart keyword"));
    let clause_re = CLAUSE.get_or_init(|| {
        Regex::new(r"(?s)(?:\b(?:extends|with|implements|on)\b|=)(.*)$").expect("dart clause")
    });
    let word = WORD.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").expect("dart word"));
    let span = usize::try_from(decl.end_line - decl.start_line + 1)
        .unwrap_or(1)
        .clamp(1, 41);
    let text = strip_c_comments(&facts_lines(facts, decl, span, "\n"));
    let mut depth = 0usize;
    let mut flat = String::new();
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            '{' | ';' if depth == 0 => break,
            _ if depth == 0 => flat.push(ch),
            _ => {}
        }
    }
    let head = keyword
        .find(&flat)
        .map_or(flat.as_str(), |m| &flat[m.start()..]);
    let clause = clause_re
        .captures(head)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    word.find_iter(&clause)
        .map(|m| m.as_str())
        .filter(|w| !matches!(*w, "extends" | "with" | "implements" | "on"))
        .map(str::to_string)
        .collect()
}

/// A Swift declaration's head, read up to its body with comments, attribute
/// arguments and generic parameters dropped (upstream `swiftHeadOf`):
/// `@objc(ChartViewBase) open class ChartViewBase<T>: NSUIView,
/// ChartDataProvider where T: Equatable {` names `NSUIView`,
/// `ChartDataProvider`; and whether the declaration is an `extension`.
pub(crate) struct SwiftHead {
    pub(crate) supers: Vec<String>,
    pub(crate) extension: bool,
}

pub(crate) fn swift_head_of(decl: &Node, context: &dyn ResolutionContext) -> SwiftHead {
    match context.source_facts(&decl.file_path) {
        Some(facts) => swift_head_in(&facts, decl),
        None => SwiftHead {
            supers: Vec::new(),
            extension: false,
        },
    }
}

/// [`swift_head_of`] over a file's facts.
pub(crate) fn swift_head_in(facts: &crate::source_facts::SourceFacts, decl: &Node) -> SwiftHead {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static LAST_WORD: OnceLock<Regex> = OnceLock::new();
    let head_re = HEAD.get_or_init(|| {
        Regex::new(r"(?s)\b(class|struct|enum|protocol|extension|actor)\s+[\w.]+\s*(.*)$")
            .expect("swift head")
    });
    let clause_re =
        CLAUSE.get_or_init(|| Regex::new(r"(?s)^:(.*?)(?:\bwhere\b|$)").expect("swift clause"));
    let last_word =
        LAST_WORD.get_or_init(|| Regex::new(r"([A-Za-z_]\w*)\s*$").expect("swift word"));
    let span = usize::try_from(decl.end_line - decl.start_line + 1)
        .unwrap_or(1)
        .clamp(1, 21);
    let start = usize::try_from(decl.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = start.saturating_add(span).min(facts.line_count());
    let lines = if start < end {
        facts.join_lines(start, end, "\n")
    } else {
        String::new()
    };
    let text = strip_c_comments(&lines);
    let mut depth = 0usize;
    let mut flat = String::new();
    for ch in text.chars() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => break,
            _ if depth == 0 => flat.push(ch),
            _ => {}
        }
    }
    let Some(head) = head_re.captures(&flat) else {
        return SwiftHead {
            supers: Vec::new(),
            extension: false,
        };
    };
    let rest = head[2].trim().to_string();
    let clause = clause_re
        .captures(&rest)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    SwiftHead {
        supers: clause
            .split(',')
            .filter_map(|item| last_word.captures(item.trim()).map(|c| c[1].to_string()))
            .collect(),
        extension: &head[1] == "extension",
    }
}

/// What a Swift type's declarations inherit or conform to, the standard
/// collections' refinements and UIKit / AppKit ancestry first (upstream
/// `swiftDeclOf(…).supers`, #2170).
fn swift_supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    crate::swift_scope::swift_decl_of(type_name, context)
        .supers
        .clone()
}

/// Upstream `SCALA_TYPE_KINDS`.
fn is_scala_type(node: &Node) -> bool {
    node.language == Language::Scala
        && matches!(
            node.kind,
            NodeKind::Class
                | NodeKind::Trait
                | NodeKind::Interface
                | NodeKind::Enum
                | NodeKind::Struct
                | NodeKind::Module
                | NodeKind::Namespace
        )
}

/// The simple names a Scala type's declarations extend or mix in (upstream
/// `scalaSupertypesOf`).
pub(crate) fn scala_supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    let mut names = Vec::new();
    for decl in context.get_nodes_by_name_shared(type_name) {
        if !is_scala_type(&decl) {
            continue;
        }
        if let Some(facts) = context.source_facts(&decl.file_path) {
            names.extend(scala_declaration_supertypes(&facts, &decl));
        }
    }
    names
}

/// The simple names one Scala type declaration extends or mixes in, read
/// from the 13 lines from its start up to its body or `=` (the per-declaration
/// part of upstream `scalaSupertypesOf`).
pub(crate) fn scala_declaration_supertypes(
    facts: &crate::source_facts::SourceFacts,
    decl: &Node,
) -> Vec<String> {
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    let clause_re =
        CLAUSE.get_or_init(|| Regex::new(r"(?s)\bextends\b(.*)$").expect("scala clause"));
    let name_re = NAME.get_or_init(|| Regex::new(r"[A-Za-z_][\w.]*").expect("scala name"));
    let start = usize::try_from(decl.start_line)
        .unwrap_or(1)
        .saturating_sub(1);
    let end = start.saturating_add(13).min(facts.line_count());
    let text = if start < end {
        facts.join_lines(start, end, " ")
    } else {
        String::new()
    };
    let mut depth = 0usize;
    let mut flat = String::new();
    for ch in text.chars() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            '{' | '=' if depth == 0 => break,
            _ if depth == 0 => flat.push(ch),
            _ => {}
        }
    }
    let clause = clause_re
        .captures(&flat)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    name_re
        .find_iter(&clause)
        .filter_map(|m| {
            let simple = m.as_str().rsplit('.').next().unwrap_or_default();
            (simple != "with" && simple != "derives" && simple != decl.name)
                .then(|| simple.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{clause_type_names, declaration_heads, name_supertypes};
    use crate::source_facts::SourceFacts;
    use codegraph_core::types::{Language, Node, NodeKind};

    fn decl(language: Language, kind: NodeKind, name: &str, lines: (i64, i64)) -> Node {
        Node {
            id: format!("{}:{name}", kind.as_str()),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "f".to_string(),
            language,
            start_line: lines.0,
            end_line: lines.1,
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

    fn heads(text: &str, node: &Node) -> Option<Vec<String>> {
        declaration_heads(&SourceFacts::new(text.into()), node)
    }

    /// W5-07: each language's head is read as `declared_supertypes` reads
    /// it, one declaration at a time: the TS/JS head's first three lines, a
    /// Java head up to its body, a Dart type's `extends`/`with`/
    /// `implements`, a Swift extension's conformances, an Objective-C
    /// `@interface`'s superclass, a Scala object's `extends`; a node no walk
    /// reads has none.
    #[test]
    fn declaration_heads_read_each_head_as_the_walk_does() {
        use Language::*;
        assert_eq!(
            heads(
                "class User(Base, metaclass=Meta):\n    pass\n",
                &decl(Python, NodeKind::Class, "User", (1, 2))
            ),
            Some(vec!["Base".to_string()])
        );
        // Past the third line a TS/JS head is not read.
        assert_eq!(
            heads(
                "export class User\n\n\nextends Base {}\n",
                &decl(TypeScript, NodeKind::Class, "User", (1, 4))
            ),
            Some(Vec::new())
        );
        assert_eq!(
            heads(
                "public class User\n    extends Base\n    implements Runnable {\n}\n",
                &decl(Java, NodeKind::Class, "User", (1, 4))
            ),
            Some(vec!["Base".to_string(), "Runnable".to_string()])
        );
        assert_eq!(
            heads(
                "class User extends Base with Mixin implements Api {}\n",
                &decl(Dart, NodeKind::Class, "User", (1, 1))
            ),
            Some(vec![
                "Base".to_string(),
                "Mixin".to_string(),
                "Api".to_string()
            ])
        );
        assert_eq!(
            heads(
                "extension User: Codable {\n}\n",
                &decl(Swift, NodeKind::Class, "User", (1, 2))
            ),
            Some(vec!["Codable".to_string()])
        );
        assert_eq!(
            heads(
                "@interface User : Base <Api>\n@end\n",
                &decl(ObjC, NodeKind::Class, "User", (1, 2))
            ),
            Some(vec!["Base".to_string()])
        );
        assert_eq!(
            heads(
                "object User extends Base with Api\n",
                &decl(Scala, NodeKind::Module, "User", (1, 1))
            ),
            Some(vec!["Base".to_string(), "Api".to_string()])
        );
        assert_eq!(
            heads(
                "def make():\n    pass\n",
                &decl(Python, NodeKind::Function, "make", (1, 2))
            ),
            None
        );
        assert_eq!(
            heads(
                "package main\n",
                &decl(Go, NodeKind::Struct, "User", (1, 1))
            ),
            None
        );
    }

    /// W5-07: what a walk reads for a name beyond its declarations' heads: a
    /// Swift standard collection's refinements, a UIKit class's superclass,
    /// and for Objective-C that superclass only when no declaration writes
    /// one.
    #[test]
    fn name_supertypes_add_what_the_name_alone_brings() {
        assert_eq!(
            name_supertypes(Language::Swift, "Collection", true),
            ["Sequence"]
        );
        assert_eq!(
            name_supertypes(Language::Swift, "UIViewController", true),
            ["UIResponder"]
        );
        assert_eq!(
            name_supertypes(Language::ObjC, "UIViewController", false),
            ["UIResponder"]
        );
        assert!(name_supertypes(Language::ObjC, "UIViewController", true).is_empty());
        assert!(name_supertypes(Language::Python, "Collection", false).is_empty());
    }

    #[test]
    fn a_clause_names_each_type_before_a_comma_the_end_or_implements() {
        assert_eq!(
            clause_type_names(" Base implements Runnable, Comparable"),
            ["Base", "Runnable", "Comparable"]
        );
        assert_eq!(clause_type_names(" B , C"), ["B", "C"]);
        assert!(clause_type_names(" Base other").is_empty());
    }
}
