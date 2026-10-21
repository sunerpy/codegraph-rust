//! A name an enclosing declaration declares as a TYPE PARAMETER means that
//! parameter, never a project symbol that happens to share it (upstream #2123
//! `gateTypeParameter`).
//!
//! `def zipWith[A, B, C](fa: ISeq[A], …)` in cats sent its `A` to an
//! `implicit def A` elsewhere and its `B` to a `case class B()` declared inside
//! some test; getx's `T` landed on a constant named `T`. The index does not
//! record type parameters, so they are read from the declaration heads around
//! the reference: after the declared name (`class Foo<T>`, `fn f<T: Display>`,
//! `func F[T any]`, `def f[F[_]: Monad, A]`), before it where the language puts
//! them (Java's `<T extends X> T max(…)`, Kotlin's `fun <T> f()`), a C++
//! `template <typename T>` line above it, and a Go method's receiver
//! (`func (p *Pool[T]) Get() T`). Applied at the resolution seam, so every
//! strategy's result obeys it. Scala adds two rules: a bare name a `def` takes
//! as a value parameter is that parameter, and a bare type name reaches a
//! member of another type only from inside it, a subtype, or a file importing
//! its members.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use crate::types::{RefView, ResolutionContext, ResolvedRef};

/// How far past a declaration's first line its head can run.
const HEAD_LINES: usize = 40;

/// Per-pass memos: declaration id → its type parameters / Scala value
/// parameters.
#[derive(Default)]
pub(crate) struct TypeParameterMemos {
    types: HashMap<String, Arc<HashSet<String>>>,
    values: HashMap<String, Arc<HashSet<String>>>,
}

fn is_generic_language(language: Language) -> bool {
    matches!(
        language,
        Language::Scala
            | Language::Java
            | Language::Kotlin
            | Language::CSharp
            | Language::TypeScript
            | Language::Tsx
            | Language::Swift
            | Language::Rust
            | Language::Dart
            | Language::Go
            | Language::Cpp
            | Language::ArkTs
    )
}

fn is_type_ref_kind(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::References | EdgeKind::TypeOf | EdgeKind::Returns | EdgeKind::Instantiates
    )
}

fn is_declaration_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Protocol
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Function
            | NodeKind::Method
            | NodeKind::Union
            | NodeKind::Module
    )
}

fn is_scala_owner_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Trait
            | NodeKind::Module
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Enum
    )
}

fn is_scala_type_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class
            | NodeKind::Trait
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::TypeAlias
    )
}

/// `/^[A-Za-z_]\w*$/`.
fn is_simple_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The resolved reference, or `None` when a declaration around it declares
/// its name: as a type parameter (a type reference), or, in Scala, as a value
/// parameter of the enclosing `def`. A bare Scala type name out of its
/// member's scope takes the type of that name in scope, when one fits.
pub(crate) fn gate_type_parameter(
    resolved: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
    memos: &Mutex<TypeParameterMemos>,
) -> Option<ResolvedRef> {
    let mut result = resolved?;
    let name = reference.reference_name.as_str();
    if reference.language == Language::Scala
        && matches!(
            reference.reference_kind,
            EdgeKind::Calls | EdgeKind::References
        )
        && is_simple_identifier(name)
    {
        for declaration in enclosing_declarations(reference, context) {
            if matches!(declaration.kind, NodeKind::Function | NodeKind::Method)
                && declared_value_parameters(&declaration, context, memos).contains(name)
            {
                return None;
            }
        }
    }
    if reference.language == Language::Scala && !scala_member_in_scope(&result, reference, context)
    {
        // Out of scope: the type of that name that IS in scope (cats' own
        // `trait FlatMap`, not `Eval`'s nested `FlatMap` case class), when
        // one fits.
        let target = context.get_node_by_id_shared(&result.target_node_id)?;
        if !is_scala_type_kind(target.kind) {
            return None;
        }
        let in_scope: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|n| {
                n.id != target.id
                    && n.language == Language::Scala
                    && is_scala_type_kind(n.kind)
                    && scala_member_in_scope(
                        &ResolvedRef {
                            target_node_id: n.id.clone(),
                            ..result.clone()
                        },
                        reference,
                        context,
                    )
            })
            .collect();
        let chosen = match in_scope.as_slice() {
            [only] => Some(only),
            _ => in_scope.iter().find(|n| n.file_path == reference.file_path),
        }?;
        result.target_node_id = chosen.id.clone();
    }
    if !is_generic_language(reference.language) || !is_type_ref_kind(reference.reference_kind) {
        return Some(result);
    }
    let capitalized = name.starts_with(|c: char| c.is_ascii_uppercase())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !capitalized {
        return Some(result);
    }
    for declaration in enclosing_declarations(reference, context) {
        if declared_type_parameters(&declaration, context, memos).contains(name) {
            return None;
        }
    }
    Some(result)
}

/// The declarations whose line range holds the reference.
fn enclosing_declarations(reference: &RefView, context: &dyn ResolutionContext) -> Vec<Arc<Node>> {
    context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|n| {
            is_declaration_kind(n.kind)
                && n.start_line <= reference.line
                && n.end_line >= reference.line
        })
        .collect()
}

/// A bare Scala name reaches a member of another type (`FreeT.FlatMapped`'s
/// `type A`, a test suite's `val P`) only from inside that type, a type that
/// extends it, or a file importing its members (`import Owner._`,
/// `Owner.{A}`, `Owner.*`); a def's local only from inside the def (upstream
/// `scalaMemberInScope`).
fn scala_member_in_scope(
    result: &ResolvedRef,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    let name = reference.reference_name.as_str();
    if !is_simple_identifier(name) || reference.reference_kind != EdgeKind::References {
        return true;
    }
    // Bare in the SOURCE: a qualified name reaches any member.
    let Some(line) = line_text(&reference.file_path, reference.line, context) else {
        return true;
    };
    let column = usize::try_from(reference.column).unwrap_or(0);
    let (Some(before), Some(at)) = (line.get(..column), line.get(column..)) else {
        return true;
    };
    if !at.starts_with(name) || before.trim_end().ends_with('.') {
        return true;
    }
    let Some(target) = context.get_node_by_id_shared(&result.target_node_id) else {
        return true;
    };
    if target.language != Language::Scala {
        return true;
    }
    let Some(cut) = target.qualified_name.rfind("::") else {
        return true;
    };
    let owner_qualified = &target.qualified_name[..cut];
    let containers: Vec<Arc<Node>> = context
        .get_nodes_by_qualified_name_shared(owner_qualified)
        .into_iter()
        .filter(|o| {
            o.file_path == target.file_path
                && o.start_line <= target.start_line
                && o.end_line >= target.end_line
        })
        .collect();
    let inside = |o: &Node| {
        reference.file_path == o.file_path
            && reference.line >= o.start_line
            && reference.line <= o.end_line
    };
    if let Some(def) = containers
        .iter()
        .find(|o| matches!(o.kind, NodeKind::Function | NodeKind::Method))
    {
        return inside(def);
    }
    let Some(owner) = containers.iter().find(|o| is_scala_owner_kind(o.kind)) else {
        return true; // a package member
    };
    if inside(owner) {
        return true;
    }
    // Inherited: the reference sits in a type that extends the owner, by the
    // supertypes its declaration head names.
    for around in enclosing_declarations(reference, context) {
        if !is_scala_owner_kind(around.kind) {
            continue;
        }
        if around.name == owner.name
            || crate::class_heads::scala_supertypes_of(&around.name, context).contains(&owner.name)
        {
            return true;
        }
    }
    if !is_simple_identifier(&owner.name) || !is_simple_identifier(&target.name) {
        return true;
    }
    // Imported: `import cats.data.FreeT.FlatMapped._`, `Owner.{A, B}`, `Owner.*`.
    let text = context.read_file(&reference.file_path).unwrap_or_default();
    Regex::new(&format!(
        r"(?-u:\b)import\s+[\w.]*(?-u:\b){owner}\.(?:_|\*|given(?-u:\b)|\{{[^}}]*(?-u:\b)(?:_|\*|{target})(?-u:\b)[^}}]*\}}|{target}(?-u:\b))",
        owner = regex::escape(&owner.name),
        target = regex::escape(&target.name),
    ))
    .is_ok_and(|imports| imports.is_match(&text))
}

/// Line `line` (1-based) of `file`.
fn line_text(file: &str, line: i64, context: &dyn ResolutionContext) -> Option<String> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    let facts = context.source_facts(file)?;
    (index < facts.line_count()).then(|| facts.line(index).to_string())
}

/// A declaration's head: its first lines, up to [`HEAD_LINES`], joined by
/// newlines.
fn declaration_head(node: &Node, context: &dyn ResolutionContext) -> Option<String> {
    let facts = context.source_facts(&node.file_path)?;
    let start = usize::try_from(node.start_line).ok()?.checked_sub(1)?;
    let last = usize::try_from(node.end_line).unwrap_or(0);
    let end = last.min(start + HEAD_LINES).min(facts.line_count());
    (start < end).then(|| facts.join_lines(start, end, "\n"))
}

/// The type parameters `node`'s head declares.
fn declared_type_parameters(
    node: &Node,
    context: &dyn ResolutionContext,
    memos: &Mutex<TypeParameterMemos>,
) -> Arc<HashSet<String>> {
    if let Some(hit) = memos
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .types
        .get(&node.id)
    {
        return Arc::clone(hit);
    }
    let mut names = HashSet::new();
    if let Some(head) = declaration_head(node, context) {
        if let Some(at) = name_index(&head, &node.name) {
            // After the declared name: `Foo<T>`, `f[A]`.
            let bytes = head.as_bytes();
            let mut open = at + node.name.len();
            while open < bytes.len() && (bytes[open] == b' ' || bytes[open] == b'\t') {
                open += 1;
            }
            if matches!(bytes.get(open), Some(b'<' | b'[')) {
                collect(&head, open, &mut names);
            }
            // Before it, detached from any type name: `static <T> T max(`,
            // `fun <T> f(`.
            let before = &head[..at];
            let before_bytes = before.as_bytes();
            for (i, &b) in before_bytes.iter().enumerate() {
                if b == b'<'
                    && (i == 0
                        || before_bytes[i - 1].is_ascii_whitespace()
                        || matches!(before_bytes[i - 1], b'(' | b','))
                {
                    collect(before, i, &mut names);
                }
            }
        }
        // Go: a method of a generic type declares the type's parameters again
        // in its receiver (`func (p *Pool[T]) Get() T`).
        if node.language == Language::Go && node.kind == NodeKind::Method {
            static RECEIVER: OnceLock<Regex> = OnceLock::new();
            let receiver = RECEIVER.get_or_init(|| {
                Regex::new(r"^\s*func\s*\(\s*(?:[A-Za-z_]\w*\s+)?\*?\s*[A-Za-z_]\w*\s*\[")
                    .expect("go receiver")
            });
            if let Some(m) = receiver.find(&head) {
                collect(&head, m.end() - 1, &mut names);
            }
        }
        // C++: `template <typename T, class U>` on the lines just above.
        if node.language == Language::Cpp
            && let Some(facts) = context.source_facts(&node.file_path)
        {
            let start = usize::try_from(node.start_line)
                .unwrap_or(1)
                .saturating_sub(1);
            let first = start.saturating_sub(2);
            let end = start.min(facts.line_count());
            if first < end {
                let above = facts.join_lines(first, end, "\n");
                static TEMPLATE: OnceLock<Regex> = OnceLock::new();
                let template = TEMPLATE
                    .get_or_init(|| Regex::new(r"(?-u:\b)template\s*<").expect("cpp template"));
                for m in template.find_iter(&above) {
                    collect(&above, m.end() - 1, &mut names);
                }
            }
        }
    }
    let names = Arc::new(names);
    memos
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .types
        .insert(node.id.clone(), Arc::clone(&names));
    names
}

/// A Scala def's value parameters, every curried list: `def f[A](fa: F[A])(f:
/// A => B)(implicit G: Applicative[G])` → fa, f, G.
fn declared_value_parameters(
    node: &Node,
    context: &dyn ResolutionContext,
    memos: &Mutex<TypeParameterMemos>,
) -> Arc<HashSet<String>> {
    if let Some(hit) = memos
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values
        .get(&node.id)
    {
        return Arc::clone(hit);
    }
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let param = PARAM.get_or_init(|| {
        Regex::new(r"^\s*(?:(?:implicit|using|val|var|override|private|protected|final|inline|erased)\s+)*([A-Za-z_]\w*)\s*:")
            .expect("scala value parameter")
    });
    let mut names = HashSet::new();
    if let Some(head) = declaration_head(node, context)
        && let Some(at) = name_index(&head, &node.name)
    {
        let bytes = head.as_bytes();
        let mut i = at + node.name.len();
        let skip_space = |i: &mut usize| {
            while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
                *i += 1;
            }
        };
        skip_space(&mut i);
        if bytes.get(i) == Some(&b'[') {
            i = matching_bracket(&head, i).map_or(bytes.len(), |close| close + 1);
        }
        skip_space(&mut i);
        while bytes.get(i) == Some(&b'(') {
            let Some(close) = matching_bracket(&head, i) else {
                break;
            };
            for item in split_top_level(&head[i + 1..close], false) {
                let bare = strip_annotations(item);
                if let Some(c) = param.captures(bare) {
                    names.insert(c[1].to_string());
                }
            }
            i = close + 1;
            skip_space(&mut i);
        }
    }
    let names = Arc::new(names);
    memos
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values
        .insert(node.id.clone(), Arc::clone(&names));
    names
}

/// The declared name's position in its head: the whole word (`[\w$]` on
/// neither side). A plain search: a declared name can be an operator (Scala's
/// `*`, `|+|`), which no pattern built from it could match safely.
fn name_index(head: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = head.as_bytes();
    head.match_indices(name).map(|(at, _)| at).find(|&at| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + name.len()).copied();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// Past annotations: `@sp(Int, Long) A`, `@NonNull T`.
fn strip_annotations(item: &str) -> &str {
    static ANNOTATIONS: OnceLock<Regex> = OnceLock::new();
    let annotations = ANNOTATIONS
        .get_or_init(|| Regex::new(r"^\s*(?:@[\w.]+(?:\([^()]*\))?\s*)+").expect("annotations"));
    annotations.find(item).map_or(item, |m| &item[m.end()..])
}

/// Add the parameter names of the bracket group opening at `open`.
fn collect(text: &str, open: usize, names: &mut HashSet<String>) {
    static PARAM: OnceLock<Regex> = OnceLock::new();
    let param = PARAM.get_or_init(|| {
        Regex::new(
            r"^\s*(?:(?:in|out|reified|const|typename|class|struct)\s+|[+\-']\s*)*([A-Za-z_]\w*)",
        )
        .expect("type parameter")
    });
    let Some(close) = matching_bracket(text, open) else {
        return;
    };
    let angle = text.as_bytes()[open] == b'<';
    for item in split_top_level(&text[open + 1..close], angle) {
        let bare = strip_annotations(item);
        if let Some(c) = param.captures(bare) {
            let name = &c[1];
            if name != "typename" && name != "class" {
                names.insert(name.to_string());
            }
        }
    }
}

fn is_opener(c: u8, angle: bool) -> bool {
    c == b'[' || c == b'(' || (angle && c == b'<')
}

fn is_closer(c: u8, angle: bool, previous: Option<u8>) -> bool {
    if c == b']' || c == b')' {
        return true;
    }
    angle && c == b'>' && previous != Some(b'-') && previous != Some(b'=')
}

/// The bracket closing the one at `open`. A `[…]` list counts only square
/// brackets and parentheses (Scala's bounds `B >: A` are not angle brackets),
/// and a `<…>` list skips the `->` / `=>` inside its bounds.
fn matching_bracket(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let angle = bytes[open] == b'<';
    let closer = match bytes[open] {
        b'<' => b'>',
        b'[' => b']',
        _ => b')',
    };
    let mut depth = 0usize;
    for i in open..bytes.len() {
        let c = bytes[i];
        if c == b'{' || c == b';' {
            return None;
        }
        if is_opener(c, angle) {
            depth += 1;
        } else if is_closer(c, angle, i.checked_sub(1).map(|p| bytes[p])) {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return (c == closer).then_some(i);
            }
        }
    }
    None
}

fn split_top_level(inner: &str, angle: bool) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut start = 0;
    for i in 0..bytes.len() {
        let c = bytes[i];
        if is_opener(c, angle) {
            depth += 1;
        } else if is_closer(c, angle, i.checked_sub(1).map(|p| bytes[p])) {
            depth -= 1;
        } else if c == b',' && depth == 0 {
            out.push(&inner[start..i]);
            start = i + 1;
        }
    }
    out.push(&inner[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{collect, matching_bracket, name_index};
    use std::collections::HashSet;

    fn params(text: &str, open: usize) -> Vec<String> {
        let mut names = HashSet::new();
        collect(text, open, &mut names);
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        names
    }

    #[test]
    fn heads_name_their_type_parameters() {
        let head = "def zipWith[A, B, C](fa: List[A])";
        assert_eq!(params(head, head.find('[').unwrap()), ["A", "B", "C"]);
        let head = "trait Heyting[@sp(Int, Long) A] {";
        assert_eq!(params(head, head.find('[').unwrap()), ["A"]);
        let head = "def updatedRef[A, B >: A](fa: List[A])";
        assert_eq!(params(head, head.find('[').unwrap()), ["A", "B"]);
        let head = "static <T extends Comparable<T>> T max(";
        assert_eq!(params(head, head.find('<').unwrap()), ["T"]);
        let head = "template <typename T, class U>";
        assert_eq!(params(head, head.find('<').unwrap()), ["T", "U"]);
        let head = "fun <in T, reified R> f(";
        assert_eq!(params(head, head.find('<').unwrap()), ["R", "T"]);
    }

    #[test]
    fn a_bracket_list_is_not_closed_by_a_bound_or_an_arrow() {
        assert_eq!(matching_bracket("[A, B >: A]", 0), Some(10));
        assert_eq!(matching_bracket("<F extends (x) => y>", 0), Some(19));
        assert_eq!(matching_bracket("[A { }", 0), None);
    }

    #[test]
    fn a_declared_name_is_found_as_a_whole_word_operators_included() {
        assert_eq!(name_index("def |+|[A](x: A)", "|+|"), Some(4));
        assert_eq!(name_index("def *[B](b: B)", "*"), Some(4));
        assert_eq!(
            name_index("class FooBar<T> extends Foo<T>", "Foo"),
            Some(24)
        );
    }
}
