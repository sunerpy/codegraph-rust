//! A Dart annotation is a constant expression (upstream #2390): a `const`
//! variable (`@riverpod`, `@meta.immutable`, a type's static constant
//! `@Riverpod.scoped` or enum value `@Retry.always`) or a call of a `const`
//! constructor (`@Riverpod(keepAlive: true)`, `@Riverpod.forever()`). It
//! was ranked like a Python decorator, which favours a function or method of
//! that name over a class and scores a constant not at all, so riverpod's
//! `@riverpod` annotations went to riverpod_analyzer_utils' extension getter
//! `riverpod`, not riverpod_annotation's `const riverpod = Riverpod();`.
//!
//! An annotation links, as it is written from its `@`, a constant or (called)
//! a class its library can see, through the import prefix it is written
//! with; a static constant, an enum value or (called) a named constructor of
//! the type it is written through; or, on a member, a static constant of the
//! type around it. Never a method, a getter or a function.

use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::dart_libraries::{
    dart_import_prefixes, is_dart_library_decl, is_dart_top_level_visible, prefer_own_dart_library,
};
use crate::dart_scope::{dart_extension_named, in_order, is_dart_type_kind};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Whether a reference is a Dart annotation, which the extractor records by
/// its last name, at the `@` (upstream `isDartAnnotation`).
pub(crate) fn is_dart_annotation(reference: &RefView) -> bool {
    reference.language == Language::Dart && reference.reference_kind == EdgeKind::Decorates
}

/// What a Dart annotation links (upstream `matchDartAnnotation`).
pub(crate) fn match_dart_annotation(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let name = reference.reference_name.as_str();
    if !context
        .get_nodes_by_name_shared(name)
        .iter()
        .any(|node| node.language == Language::Dart)
    {
        return None;
    }
    let written = dart_annotation_at(reference, context)?;
    let names = &written.names;
    let prefixes = dart_import_prefixes(&reference.file_path, context);
    let prefixed = |count: usize| names.len() == count && prefixes.contains(&names[0]);
    let (pool, resolved_by) = if names.len() == 1 || prefixed(2) {
        let prefix = (names.len() == 2).then(|| names[0].as_str());
        if prefix.is_none()
            && !written.call
            && let Some(own) = dart_enclosing_static(name, reference, context)
        {
            return Some(resolved(reference, &own, 0.9, ResolvedBy::ExactMatch));
        }
        let mut pool: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|node| {
                is_dart_library_decl(node)
                    && if written.call {
                        is_annotation_class(node, context)
                    } else {
                        node.kind == NodeKind::Constant
                    }
                    && is_dart_top_level_visible(node, reference, context, prefix)
            })
            .collect();
        if prefix.is_none() {
            pool = prefer_own_dart_library(pool, reference, context);
        }
        (pool, ResolvedBy::ExactMatch)
    } else if names.len() == 2 || prefixed(3) {
        let prefix = (names.len() == 3).then(|| names[0].as_str());
        let mut owners: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(&names[names.len() - 2])
            .into_iter()
            .filter(|owner| {
                is_dart_library_decl(owner)
                    && is_dart_type_kind(owner.kind)
                    && is_dart_top_level_visible(owner, reference, context, prefix)
            })
            .collect();
        if prefix.is_none() {
            owners = prefer_own_dart_library(owners, reference, context);
        }
        let pool: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(name)
            .into_iter()
            .filter(|member| {
                member.language == Language::Dart
                    && owners.iter().any(|owner| {
                        owner.file_path == member.file_path
                            && member.qualified_name == format!("{}::{name}", owner.qualified_name)
                    })
                    && if written.call {
                        crate::dart_members::is_dart_constructor(member, context)
                    } else {
                        matches!(member.kind, NodeKind::Constant | NodeKind::EnumMember)
                    }
            })
            .collect();
        (pool, ResolvedBy::QualifiedName)
    } else {
        return None;
    };
    if pool.len() <= 1 {
        return pool
            .first()
            .map(|node| resolved(reference, node, 0.9, resolved_by));
    }
    // Libraries this cannot establish see every file's declarations.
    let best = crate::name_matcher::find_best_match(reference, &pool)?;
    let near =
        crate::name_matcher::compute_path_proximity(&reference.file_path, &best.file_path) >= 30.0;
    Some(resolved(
        reference,
        best,
        if near { 0.7 } else { 0.4 },
        resolved_by,
    ))
}

fn resolved(
    reference: &RefView,
    target: &Node,
    confidence: f64,
    resolved_by: ResolvedBy,
) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence,
        resolved_by,
    }
}

/// A class a Dart annotation can call a constructor of: not an extension,
/// which has none (upstream `isDartAnnotationClass`).
fn is_annotation_class(node: &Node, context: &dyn ResolutionContext) -> bool {
    node.kind == NodeKind::Class && dart_extension_named(node, context).is_none()
}

/// The static constant or enum value `name` of the type a Dart annotation is
/// written in, which a member's annotation names bare. A class's own
/// annotations stand outside its body (upstream `dartEnclosingStatic`).
fn dart_enclosing_static(
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let mut own: Option<Arc<Node>> = None;
    for node in in_order(context.get_nodes_in_file_shared(&reference.file_path)) {
        if is_dart_type_kind(node.kind)
            && node.id != reference.from_node_id
            && node.start_line <= reference.line
            && node.end_line >= reference.line
            && own
                .as_ref()
                .is_none_or(|own| node.start_line >= own.start_line)
        {
            own = Some(node);
        }
    }
    let qualified_name = format!("{}::{name}", own?.qualified_name);
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .find(|member| {
            member.language == Language::Dart
                && member.file_path == reference.file_path
                && member.qualified_name == qualified_name
                && matches!(member.kind, NodeKind::Constant | NodeKind::EnumMember)
        })
}

/// How a Dart annotation is written: the names of `@p.Foo.named(…)` up to
/// the one its reference records, and whether it is called.
#[derive(Debug, PartialEq, Eq)]
struct Written {
    names: Vec<String>,
    call: bool,
}

/// How a Dart annotation is written, read from its `@` (upstream
/// `dartAnnotationAt`). The extractor records the last name, or the one a
/// type argument list follows (`@Foo<int>.named(…)` is `Foo`'s). On a line
/// with non-ASCII text before it, the `@` whose names fit stands in.
fn dart_annotation_at(reference: &RefView, context: &dyn ResolutionContext) -> Option<Written> {
    let facts = context.source_facts(&reference.file_path)?;
    let code = facts.dart_code();
    let from = code.line_start(reference.line)?;
    let to = code
        .line_start(reference.line + 1)
        .unwrap_or(code.text.len());
    let column = from + usize::try_from(reference.column).ok()?;
    let line = code.text.get(from..to)?;
    let ats = std::iter::once(column).chain(
        line.match_indices('@')
            .map(|(at, _)| from + at)
            .filter(|at| *at != column),
    );
    for at in ats {
        if code.text.as_bytes().get(at) != Some(&b'@') {
            continue;
        }
        let Some(read) = read_dart_annotation(&code.text, at + 1) else {
            continue;
        };
        let end = read.type_args_at.unwrap_or(read.names.len() - 1);
        if read.names[end] == reference.reference_name {
            return Some(Written {
                names: read.names[..=end].to_vec(),
                call: read.call,
            });
        }
    }
    None
}

/// A Dart annotation's dotted names, which of them a type argument list
/// follows, and whether `(` comes next (upstream `readDartAnnotation`).
#[derive(Debug, PartialEq, Eq)]
struct Read {
    names: Vec<String>,
    type_args_at: Option<usize>,
    call: bool,
}

fn read_dart_annotation(text: &str, mut at: usize) -> Option<Read> {
    let bytes = text.as_bytes();
    let blank = |at: usize| matches!(bytes.get(at), Some(b' ' | b'\t'));
    let mut names = Vec::new();
    let mut type_args_at = None;
    while names.len() < 8 {
        while blank(at) {
            at += 1;
        }
        let start = at;
        if !bytes
            .get(at)
            .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
        {
            return None;
        }
        while bytes
            .get(at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
        {
            at += 1;
        }
        names.push(text[start..at].to_string());
        while blank(at) {
            at += 1;
        }
        if bytes.get(at) == Some(&b'<') && type_args_at.is_none() {
            let close = crate::dart_chains::type_arguments_end(bytes, at)?;
            type_args_at = Some(names.len() - 1);
            at = close + 1;
            while blank(at) {
                at += 1;
            }
        }
        if bytes.get(at) != Some(&b'.') {
            break;
        }
        at += 1;
    }
    Some(Read {
        names,
        type_args_at,
        call: bytes.get(at) == Some(&b'('),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Option<(Vec<String>, Option<usize>, bool)> {
        read_dart_annotation(text, 1).map(|read| (read.names, read.type_args_at, read.call))
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn an_annotation_is_read_as_written_from_its_at() {
        assert_eq!(
            read("@riverpod\n"),
            Some((names(&["riverpod"]), None, false))
        );
        assert_eq!(
            read("@Riverpod(keepAlive: true)"),
            Some((names(&["Riverpod"]), None, true))
        );
        assert_eq!(
            read("@meta.immutable\n"),
            Some((names(&["meta", "immutable"]), None, false))
        );
        assert_eq!(
            read("@Riverpod.forever()"),
            Some((names(&["Riverpod", "forever"]), None, true))
        );
        assert_eq!(
            read("@Foo<int>.named(1)"),
            Some((names(&["Foo", "named"]), Some(0), true))
        );
        assert_eq!(
            read("@ p . Foo . named (1)"),
            Some((names(&["p", "Foo", "named"]), None, true))
        );
        assert_eq!(read("@1x"), None);
    }
}
