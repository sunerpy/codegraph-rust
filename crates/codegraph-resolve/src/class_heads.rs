//! The supertypes a class declaration names in its own head, read from source
//! (upstream #2180 `declaredSupertypes` and the per-language head readers it
//! uses). Source is read rather than the graph's `extends` edges because a
//! first resolution pass has no edges yet, and a sync must see what a fresh
//! index sees.

use std::sync::OnceLock;

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

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

/// `text` with `/* … */` and `// …` comments blanked.
fn strip_c_comments(text: &str) -> String {
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    let block = BLOCK.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").expect("block comment"));
    let line = LINE.get_or_init(|| Regex::new(r"//[^\n]*").expect("line comment"));
    line.replace_all(&block.replace_all(text, " "), " ")
        .into_owned()
}

/// The supertypes `class` names in its own head: Pascal `TX = class(TBase)`,
/// Python `class X(Base):`, Ruby `class X < Base`, PHP / TS / JS
/// `extends Base`, and the Java-family, Dart, Swift, Objective-C and Scala
/// heads.
pub(crate) fn declared_supertypes(class: &Node, context: &dyn ResolutionContext) -> Vec<String> {
    match class.language {
        Language::Java | Language::CSharp | Language::Kotlin => {
            return class_head_supertypes(class, context);
        }
        Language::Dart => return dart_supertypes_of(&class.name, context),
        Language::Swift => return swift_supertypes_of(&class.name, context),
        Language::ObjC => return objc_supertypes_of(&class.name, context),
        Language::Scala => return scala_supertypes_of(&class.name, context),
        _ => {}
    }
    static PASCAL: OnceLock<Regex> = OnceLock::new();
    static PYTHON: OnceLock<Regex> = OnceLock::new();
    static PYTHON_KEYWORD: OnceLock<Regex> = OnceLock::new();
    static RUBY: OnceLock<Regex> = OnceLock::new();
    static EXTENDS: OnceLock<Regex> = OnceLock::new();
    let head = head_lines(class, 3, " ", context);
    let clause = match class.language {
        Language::Pascal => PASCAL
            .get_or_init(|| Regex::new(r"(?i)=\s*class\s*\(([^)]*)\)").expect("pascal head"))
            .captures(&head)
            .map(|c| c[1].to_string()),
        Language::Python => PYTHON
            .get_or_init(|| Regex::new(r"\bclass\s+\w+\s*\(([^)]*)\)").expect("python head"))
            .captures(&head)
            .map(|c| {
                PYTHON_KEYWORD
                    .get_or_init(|| Regex::new(r"\w+\s*=\s*[\w.]+").expect("python keyword"))
                    .replace_all(&c[1], "")
                    .into_owned()
            }),
        Language::Ruby => RUBY
            .get_or_init(|| Regex::new(r"\bclass\s+[\w:]+\s*<\s*([\w:]+)").expect("ruby head"))
            .captures(&head)
            .map(|c| c[1].to_string()),
        Language::Php
        | Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Jsx => EXTENDS
            .get_or_init(|| Regex::new(r"\bextends\s+([\w.\\]+)").expect("extends head"))
            .captures(&head)
            .map(|c| c[1].to_string()),
        _ => None,
    };
    clause.map(|clause| head_names(&clause)).unwrap_or_default()
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
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let keyword = KEYWORD
        .get_or_init(|| Regex::new(r"\b(?:class|mixin|extension|enum)\b").expect("dart keyword"));
    let clause_re = CLAUSE.get_or_init(|| {
        Regex::new(r"(?s)(?:\b(?:extends|with|implements|on)\b|=)(.*)$").expect("dart clause")
    });
    let word = WORD.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").expect("dart word"));
    let mut names = Vec::new();
    for decl in context.get_nodes_by_name_shared(type_name) {
        if !is_dart_type(&decl) {
            continue;
        }
        let span = usize::try_from(decl.end_line - decl.start_line + 1)
            .unwrap_or(1)
            .clamp(1, 41);
        let text = strip_c_comments(&head_lines(&decl, span, "\n", context));
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
        names.extend(
            word.find_iter(&clause)
                .map(|m| m.as_str())
                .filter(|w| !matches!(*w, "extends" | "with" | "implements" | "on"))
                .map(str::to_string),
        );
    }
    names
}

/// Upstream `SWIFT_TYPE_KINDS` (a protocol is extracted as its own kind here).
fn is_swift_type(node: &Node) -> bool {
    node.language == Language::Swift
        && matches!(
            node.kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::Interface
                | NodeKind::Protocol
        )
}

/// What a Swift type's declarations inherit or conform to (upstream
/// `swiftDeclOf` / `swiftHeadOf`), without the standard library's.
fn swift_supertypes_of(type_name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static LAST_WORD: OnceLock<Regex> = OnceLock::new();
    let head_re = HEAD.get_or_init(|| {
        Regex::new(r"(?s)\b(?:class|struct|enum|protocol|extension|actor)\s+[\w.]+\s*(.*)$")
            .expect("swift head")
    });
    let clause_re =
        CLAUSE.get_or_init(|| Regex::new(r"(?s)^:(.*?)(?:\bwhere\b|$)").expect("swift clause"));
    let last_word =
        LAST_WORD.get_or_init(|| Regex::new(r"([A-Za-z_]\w*)\s*$").expect("swift word"));
    let mut names = Vec::new();
    for decl in context.get_nodes_by_name_shared(type_name) {
        if !is_swift_type(&decl) {
            continue;
        }
        let span = usize::try_from(decl.end_line - decl.start_line + 1)
            .unwrap_or(1)
            .clamp(1, 21);
        let text = strip_c_comments(&head_lines(&decl, span, "\n", context));
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
            continue;
        };
        let rest = head[1].trim().to_string();
        let clause = clause_re
            .captures(&rest)
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        names.extend(
            clause
                .split(',')
                .filter_map(|item| last_word.captures(item.trim()).map(|c| c[1].to_string())),
        );
    }
    names
}

/// The superclasses an Objective-C class's `@interface` declarations name
/// (upstream `objcSupertypesOf`), without the system classes'.
fn objc_supertypes_of(name: &str, context: &dyn ResolutionContext) -> Vec<String> {
    static INTERFACE: OnceLock<Regex> = OnceLock::new();
    let interface = INTERFACE
        .get_or_init(|| Regex::new(r"@interface\s+\w+\s*:\s*(\w+)").expect("objc interface"));
    let mut supers: Vec<String> = Vec::new();
    for decl in context.get_nodes_by_name_shared(name) {
        if decl.kind != NodeKind::Class || decl.language != Language::ObjC {
            continue;
        }
        let line = head_lines(&decl, 1, " ", context);
        if let Some(sup) = interface.captures(&line).map(|c| c[1].to_string())
            && !supers.contains(&sup)
        {
            supers.push(sup);
        }
    }
    supers
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
    static CLAUSE: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    let clause_re =
        CLAUSE.get_or_init(|| Regex::new(r"(?s)\bextends\b(.*)$").expect("scala clause"));
    let name_re = NAME.get_or_init(|| Regex::new(r"[A-Za-z_][\w.]*").expect("scala name"));
    let mut names = Vec::new();
    for decl in context.get_nodes_by_name_shared(type_name) {
        if !is_scala_type(&decl) {
            continue;
        }
        let text = head_lines(&decl, 13, " ", context);
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
        names.extend(name_re.find_iter(&clause).filter_map(|m| {
            let simple = m.as_str().rsplit('.').next().unwrap_or_default();
            (simple != "with" && simple != "derives" && simple != type_name)
                .then(|| simple.to_string())
        }));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::clause_type_names;

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
