//! A C or C++ base class (`class DynamicMessage final : public Message`) is
//! the class C++ name lookup finds for it (upstream #2407,
//! `cpp-supertypes.ts`).
//!
//! The base used to be matched by its name alone, so one named like a class
//! of another namespace, or of another language, bound to whichever namesake
//! ranked first: protobuf's `google::protobuf::DynamicMessage` derived from
//! `json_internal::ResolverPool::Message`, leveldb's iterators from
//! `SkipList::Iterator`. A wrong supertype reaches everything that walks the
//! hierarchy: the supertype walk that finds an inherited method and the type
//! hierarchy `codegraph_explore` prints.
//!
//! The name is looked up as C++ looks it up:
//!  - from the scope the class is declared in (its own members are not in
//!    scope in its base clause), outwards to the global scope; a class scope
//!    includes its bases, a qualified name is resolved a segment at a time and
//!    an alias is followed to the class it names (`cpp_type_aliases`);
//!    `::Base` is the global one;
//!  - then through what the file writes before the class: a namespace alias
//!    (`namespace _pbi = ::google::protobuf::internal;`), a using-declaration
//!    (`using leveldb::FilterPolicy;`), a using-directive (`using namespace
//!    ROCKSDB_NAMESPACE;`);
//!  - a class a source file (`.cc`, `.cpp`, `.c`) defines belongs to that
//!    translation unit, so only that file sees it (leveldb's skiplist test
//!    declares its own `leveldb::Comparator`);
//!  - a template parameter (`template <class Base> class X : public Base`) or
//!    a member of one (`public T::Base`) is whatever a template argument makes
//!    it: no edge.
//!
//! When none of that finds the class, a class of that name that is the only
//! one the file can see is taken: the index loses a class's namespace in a few
//! shapes (`class PROTOBUF_EXPORT FieldDescriptor`, the classes after a
//! partial specialization), and the lookup from the wrong scope finds nothing.
//! Several such classes would be a guess, so there is no edge. Only C and C++
//! declarations are candidates, and no other strategy sees these references.
//!
//! Upstream also reads the project's namespace aliases and fmt's
//! macro-opened namespaces here; both come with #2176 (W4-CPP-07).

use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node};
use regex::Regex;

use crate::cpp_type_aliases::{
    CppNamedClass, cpp_class_named, cpp_type_segments, is_c_family, is_class_kind, parent_scope,
    scopes_within, strip_template_arguments, template_parameters_around,
};
use crate::source_facts::SourceFacts;
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Is `reference` a C or C++ class's base?
pub(crate) fn is_cpp_supertype_ref(reference: &RefView) -> bool {
    matches!(reference.language, Language::Cpp | Language::C)
        && matches!(
            reference.reference_kind,
            EdgeKind::Extends | EdgeKind::Implements
        )
}

/// A C or C++ source file, whose definitions belong to its own translation
/// unit.
fn is_source_file(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        ["c", "cc", "cpp", "cxx", "c++", "cu", "metal"]
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// The base of `reference`, as C++ name lookup finds it; `None` when it names
/// no class (or several).
pub(crate) fn match_cpp_supertype(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let derived = context.get_node_by_id_shared(&reference.from_node_id)?;
    if !is_c_family(&derived) || !is_class_kind(derived.kind) {
        return None;
    }
    let written = written_base(reference, context);
    let segments = cpp_type_segments(&written)?;
    let global = written.starts_with("::");
    if !global && template_parameters_around(&derived, context).contains(&segments[0]) {
        return None;
    }

    let derived_file = derived.file_path.clone();
    let visible =
        move |node: &Node| node.file_path == derived_file || !is_source_file(&node.file_path);
    let scopes = scopes_within(parent_scope(&derived.qualified_name));
    let resolved = |target: Arc<Node>| ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::QualifiedName,
    };
    let look_up = |spelled: &str, scopes: &[String]| {
        class_node(
            cpp_class_named(spelled, scopes, reference, context, Some(&visible)),
            &derived,
            context,
        )
    };
    let global_scope = [String::new()];

    // The scopes around the class, or only the global one for `::Base`.
    match look_up(&written, if global { &global_scope } else { &scopes }) {
        Lookup::Found(target) => return Some(resolved(target)),
        Lookup::NoClass => return None,
        Lookup::NotFound => {}
    }

    // What the file writes before the class, the nearest first: each stands
    // for the name's first segment, and the rest stays as written.
    let path = segments.join("::");
    let after = after_first_segment(&written);
    let usings = context
        .source_facts(&derived.file_path)
        .map(|facts| usings_in(&facts))
        .unwrap_or_default();
    let before: Vec<&CppUsing> = usings
        .iter()
        .filter(|using| using.line < reference.line)
        .rev()
        .collect();
    let spelled_from = |spelled: String| {
        let scopes = if spelled.starts_with("::") {
            &global_scope[..]
        } else {
            &scopes[..]
        };
        match look_up(&spelled, scopes) {
            Lookup::Found(target) => Some(target),
            Lookup::NoClass | Lookup::NotFound => None,
        }
    };
    // `::_pbi::MessageGlobalsBase` under `namespace _pbi = ::google::protobuf::internal;`.
    if segments.len() > 1
        && let Some(alias) = before
            .iter()
            .find(|using| using.kind == UsingKind::Alias && using.name == segments[0])
        && let Some(hit) = spelled_from(format!("{}{after}", alias.path))
    {
        return Some(resolved(hit));
    }
    if !global {
        // `using leveldb::FilterPolicy;` … `public FilterPolicy`.
        for using in &before {
            if using.kind == UsingKind::Declaration
                && using.name == segments[0]
                && let Some(hit) = spelled_from(format!("{}{after}", using.path))
            {
                return Some(resolved(hit));
            }
        }
        // `using namespace ROCKSDB_NAMESPACE;` … `public EventListener`.
        for using in &before {
            if using.kind == UsingKind::Directive
                && let Some(hit) = spelled_from(format!("{}::{written}", using.path))
            {
                return Some(resolved(hit));
            }
        }
    }
    // The only class of that name the file can see, by its qualified tail.
    let suffix = format!("::{path}");
    let tail: Vec<Arc<Node>> = named_classes(&segments, &derived, context, &visible)
        .into_iter()
        .filter(|node| node.qualified_name == path || node.qualified_name.ends_with(&suffix))
        .collect();
    let own: Vec<&Arc<Node>> = tail
        .iter()
        .filter(|node| node.file_path == derived.file_path)
        .collect();
    let sole: Vec<&Arc<Node>> = if own.is_empty() {
        tail.iter().collect()
    } else {
        own
    };
    match sole.as_slice() {
        [only] => Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: only.id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::ExactMatch,
        }),
        _ => None,
    }
}

/// `written` past its first segment: `::Inner` of `::_pbi::Inner`, `` of
/// `Base`.
fn after_first_segment(written: &str) -> &str {
    static FIRST: OnceLock<Regex> = OnceLock::new();
    let first =
        FIRST.get_or_init(|| Regex::new(r"^\s*(?:::\s*)?[A-Za-z_]\w*").expect("first segment"));
    first
        .find(written)
        .map_or(written, |found| &written[found.end()..])
}

/// The base as the base clause writes it, template arguments included
/// (`bool_constant<is_utf8_enabled>`): the reference carries the name without
/// them, and an alias template is only ever named with them. The reference's
/// own name when the source there reads otherwise.
fn written_base(reference: &RefView, context: &dyn ResolutionContext) -> String {
    let fallback = || reference.reference_name.trim().to_string();
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return fallback();
    };
    let start = usize::try_from(reference.line - 1).unwrap_or(0);
    let end = start.saturating_add(12).min(facts.line_count());
    if start >= end {
        return fallback();
    }
    let joined = facts.join_lines(start, end, "\n");
    let column = usize::try_from(reference.column).unwrap_or(0);
    let Some(text) = joined.get(column..) else {
        return fallback();
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut depth = 0usize;
    let mut stop = text.len();
    for (index, &(at, c)) in chars.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| chars[i].1);
        let next = chars.get(index + 1).map(|&(_, c)| c);
        // A template's `<` follows its name; `sizeof(T) <= 8` and `1 << n`
        // compare and shift.
        let opens = c == '('
            || (c == '<'
                && previous.is_some_and(|p| p == '_' || p.is_ascii_alphanumeric())
                && next != Some('=')
                && next != Some('<'));
        let closes = c == ')' || (c == '>' && previous != Some('-') && next != Some('='));
        if opens {
            depth += 1;
        } else if closes {
            if depth == 0 {
                stop = at;
                break;
            }
            depth -= 1;
        } else if depth == 0 && matches!(c, ',' | '{' | ';') {
            stop = at;
            break;
        }
    }
    let written = text[..stop].trim();
    let squash = |text: &str| text.split_whitespace().collect::<String>();
    if squash(&strip_template_arguments(written)) == squash(&reference.reference_name) {
        written.to_string()
    } else {
        fallback()
    }
}

/// What a lookup made of a base.
enum Lookup {
    Found(Arc<Node>),
    /// The name names no particular class.
    NoClass,
    /// Keep looking.
    NotFound,
}

/// The node of the class a lookup found, as the deriving class's file sees
/// it.
fn class_node(named: CppNamedClass, derived: &Node, context: &dyn ResolutionContext) -> Lookup {
    match named {
        CppNamedClass::NoClass => Lookup::NoClass,
        CppNamedClass::NotFound => Lookup::NotFound,
        CppNamedClass::Alias(alias) => Lookup::Found(alias),
        CppNamedClass::Class(class) => {
            let classes: Vec<Arc<Node>> = context
                .get_nodes_by_qualified_name_shared(&class)
                .into_iter()
                .filter(|node| {
                    is_c_family(node) && is_class_kind(node.kind) && node.id != derived.id
                })
                .collect();
            // A lookup that ended on the class itself (its namespace lost)
            // found nothing.
            if classes.is_empty() {
                return Lookup::NotFound;
            }
            let seen: Vec<Arc<Node>> = classes
                .iter()
                .filter(|node| {
                    node.file_path == derived.file_path || !is_source_file(&node.file_path)
                })
                .cloned()
                .collect();
            match pick(if seen.is_empty() { classes } else { seen }, derived) {
                Some(target) => Lookup::Found(target),
                None => Lookup::NotFound,
            }
        }
    }
}

/// One of several declarations of a class (separate translation units,
/// vendored copies): the deriving class's own file's, then its header's, then
/// the nearest by path.
fn pick(classes: Vec<Arc<Node>>, derived: &Node) -> Option<Arc<Node>> {
    if classes.len() <= 1 {
        return classes.into_iter().next();
    }
    if let Some(own) = classes
        .iter()
        .find(|node| node.file_path == derived.file_path)
    {
        return Some(Arc::clone(own));
    }
    let stem = |path: &str| -> String {
        match path.rfind('.') {
            Some(dot) if !path[dot + 1..].contains(['/', '\\']) && dot + 1 < path.len() => {
                path[..dot].to_string()
            }
            _ => path.to_string(),
        }
    };
    let own_stem = stem(&derived.file_path);
    if let Some(header) = classes
        .iter()
        .find(|node| stem(&node.file_path) == own_stem)
    {
        return Some(Arc::clone(header));
    }
    let shared = |path: &str| -> usize {
        let a: Vec<&str> = path.split('/').collect();
        let b: Vec<&str> = derived.file_path.split('/').collect();
        let mut i = 0usize;
        while i + 1 < a.len() && i + 1 < b.len() && a[i] == b[i] {
            i += 1;
        }
        i
    };
    let mut best = Arc::clone(&classes[0]);
    for node in &classes[1..] {
        if shared(&node.file_path) > shared(&best.file_path) {
            best = Arc::clone(node);
        }
    }
    Some(best)
}

/// C and C++ classes named like the last of `segments` that `derived`'s file
/// can see.
fn named_classes(
    segments: &[String],
    derived: &Node,
    context: &dyn ResolutionContext,
    visible: &dyn Fn(&Node) -> bool,
) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(&segments[segments.len() - 1])
        .into_iter()
        .filter(|node| {
            is_c_family(node) && is_class_kind(node.kind) && node.id != derived.id && visible(node)
        })
        .collect()
}

/// What a `using` line brings in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsingKind {
    /// `using leveldb::FilterPolicy;`
    Declaration,
    /// `using namespace rocksdb;`
    Directive,
    /// `namespace _pbi = ::google::protobuf::internal;`
    Alias,
}

/// A `using` declaration or directive, or a namespace alias, a C or C++ file
/// writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CppUsing {
    /// 1-based.
    line: i64,
    kind: UsingKind,
    /// The name it brings in: the declared name, or the alias.
    name: String,
    /// What it names, as written: `leveldb::FilterPolicy`, `ROCKSDB_NAMESPACE`,
    /// `::google::protobuf::internal`.
    path: String,
}

/// The `using` declarations and directives and namespace aliases of a file,
/// in order; read once per file.
fn usings_in(facts: &SourceFacts) -> Arc<Vec<CppUsing>> {
    facts.cpp_usings(|source| {
        if !(source.contains("using") || source.contains("namespace")) {
            return Vec::new();
        }
        static USING: OnceLock<Regex> = OnceLock::new();
        let using = USING.get_or_init(|| {
            const PATH: &str = r"(?:::\s*)?[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*";
            Regex::new(&format!(
                r"\busing\s+namespace\s+({PATH})\s*;|\busing\s+({PATH}\s*::\s*[A-Za-z_]\w*)\s*;|\bnamespace\s+([A-Za-z_]\w*)\s*=\s*({PATH})\s*;"
            ))
            .expect("using regex")
        });
        let text = strip_comments_for_regex(source, CommentLang::Cpp);
        let squash = |text: &str| text.split_whitespace().collect::<String>();
        let mut found = Vec::new();
        let mut line = 1i64;
        let mut at = 0usize;
        for captures in using.captures_iter(&text) {
            let start = captures.get(0).expect("match").start();
            line += text.as_bytes()[at..start]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count() as i64;
            at = start;
            if let Some(directive) = captures.get(1) {
                found.push(CppUsing {
                    line,
                    kind: UsingKind::Directive,
                    name: String::new(),
                    path: squash(directive.as_str()),
                });
            } else if let Some(declaration) = captures.get(2) {
                let path = squash(declaration.as_str());
                let name = path.rsplit("::").next().unwrap_or_default().to_string();
                found.push(CppUsing {
                    line,
                    kind: UsingKind::Declaration,
                    name,
                    path,
                });
            } else if let (Some(name), Some(path)) = (captures.get(3), captures.get(4)) {
                found.push(CppUsing {
                    line,
                    kind: UsingKind::Alias,
                    name: name.as_str().to_string(),
                    path: squash(path.as_str()),
                });
            }
        }
        found
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_reads_its_usings_and_namespace_aliases_in_order() {
        let facts = SourceFacts::new(Arc::from(
            "// using namespace commented;\nnamespace _pbi = ::google :: protobuf::internal;\nusing leveldb::FilterPolicy;\nusing namespace rocksdb;\nusing Alias = Type;\n",
        ));
        let usings = usings_in(&facts);
        assert_eq!(
            usings
                .iter()
                .map(|using| (
                    using.line,
                    using.kind,
                    using.name.as_str(),
                    using.path.as_str()
                ))
                .collect::<Vec<_>>(),
            [
                (2, UsingKind::Alias, "_pbi", "::google::protobuf::internal"),
                (
                    3,
                    UsingKind::Declaration,
                    "FilterPolicy",
                    "leveldb::FilterPolicy"
                ),
                (4, UsingKind::Directive, "", "rocksdb"),
            ]
        );
    }

    #[test]
    fn a_name_past_its_first_segment_keeps_the_rest_as_written() {
        assert_eq!(
            after_first_segment("::_pbi::MessageGlobalsBase"),
            "::MessageGlobalsBase"
        );
        assert_eq!(after_first_segment("FilterPolicy"), "");
        assert_eq!(after_first_segment("Base<T>::Inner"), "<T>::Inner");
    }

    #[test]
    fn source_files_are_their_own_translation_units() {
        for path in [
            "db/skiplist_test.cc",
            "a.C",
            "b.cpp",
            "k.cu",
            "s.metal",
            "x.c++",
        ] {
            assert!(is_source_file(path), "{path}");
        }
        for path in ["include/leveldb/comparator.h", "a.hpp", "b.hxx", "noext"] {
            assert!(!is_source_file(path), "{path}");
        }
    }
}
