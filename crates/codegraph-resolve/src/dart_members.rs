//! The Dart members a value of a type reaches (upstream #2338, #2360): getter
//! reads, the members an extension adds to a type, and a type's static
//! constants.
//!
//! A member is found as Dart finds it: the nearest one the type, or a type it
//! extends, mixes in or implements, declares, else one an extension `on` such
//! a type adds. An extension applies only where no instance member answers,
//! and an unnamed one only in its own library.
//!
//! A member read (`x.area`, `s?.label`, `Config.instance`) arrives as a
//! `references` ref named `<receiver>.<member>`. It runs code only when the
//! member is a getter, so it links, as a call, the getter the receiver's type
//! reaches, or nothing: a field read (fields mint no nodes), a getter of a type
//! outside the project, a receiver whose type is not written down. Never a
//! same-named getter found by name alone.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::dart_scope::{
    EXTENSION_RANK, HIERARCHY_LIMIT, dart_extension_named, dart_head_of, dart_supertypes_of,
    in_order, is_dart_identifier, is_dart_member, is_dart_name_char, is_dart_type_kind,
    is_js_space, member_owner, source_line,
};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The extension a Dart member is declared in: the types it is `on`, and
/// whether it is named (upstream `dartExtensionOwner`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionOwner {
    pub on: Vec<String>,
    pub named: bool,
}

/// A Dart type and every type it extends, mixes in or implements, by
/// supertype distance, at most [`HIERARCHY_LIMIT`] of them (upstream
/// `dartLineage`).
pub(crate) fn dart_lineage(
    type_name: &str,
    context: &dyn ResolutionContext,
) -> Arc<HashMap<String, u64>> {
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .lineages
        .lock()
        .ok()
        .and_then(|memo| memo.get(type_name).cloned())
    {
        return hit;
    }
    let mut depths: HashMap<String, u64> = HashMap::new();
    let mut queue: VecDeque<(String, u64)> = VecDeque::from([(type_name.to_string(), 0)]);
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
    if let Ok(mut memo) = tables.lineages.lock() {
        memo.insert(type_name.to_string(), Arc::clone(&depths));
    }
    depths
}

/// The extension `member` is declared in, or `None` for a class's, mixin's or
/// enum's member (upstream `dartExtensionOwner`): the class node whose
/// qualified name owns it and whose lines hold it, read as an extension's
/// head.
pub(crate) fn dart_extension_owner(
    member: &Node,
    context: &dyn ResolutionContext,
) -> Option<Arc<ExtensionOwner>> {
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .extension_owners
        .lock()
        .ok()
        .and_then(|memo| memo.get(&member.id).cloned())
    {
        return hit;
    }
    let found = member
        .qualified_name
        .rsplit_once("::")
        .filter(|(owner_qn, _)| !owner_qn.is_empty())
        .and_then(|(owner_qn, _)| {
            in_order(context.get_nodes_in_file_shared(&member.file_path))
                .into_iter()
                .find(|node| {
                    node.qualified_name == owner_qn
                        && node.kind == NodeKind::Class
                        && node.start_line <= member.start_line
                        && node.end_line >= member.start_line
                })
        })
        .and_then(|owner| {
            let named = dart_extension_named(&owner, context)?;
            Some(Arc::new(ExtensionOwner {
                on: dart_head_of(&owner, context).supers.clone(),
                named,
            }))
        });
    if let Ok(mut memo) = tables.extension_owners.lock() {
        memo.insert(member.id.clone(), found.clone());
    }
    found
}

/// Whether a Dart member is a getter, declared `get <name>`: a method its
/// readers run (upstream `isDartGetter`, which reads the declaration from its
/// start and the next line, for a return type that wraps).
pub(crate) fn is_dart_getter(node: &Node, context: &dyn ResolutionContext) -> bool {
    if node.language != Language::Dart || node.kind != NodeKind::Method {
        return false;
    }
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .getters
        .lock()
        .ok()
        .and_then(|memo| memo.get(&node.id).copied())
    {
        return hit;
    }
    let line = source_line(&node.file_path, node.start_line, context).unwrap_or_default();
    let next = source_line(&node.file_path, node.start_line + 1, context).unwrap_or_default();
    let from = usize::try_from(node.start_column.max(0)).unwrap_or(0);
    let head = format!("{} {next}", line.get(from..).unwrap_or(""));
    let getter = declares_getter(&head, &node.name);
    if let Ok(mut memo) = tables.getters.lock() {
        memo.insert(node.id.clone(), getter);
    }
    getter
}

/// Whether `head` declares the getter `name` before any `=`, `{` or `;`
/// (upstream's `^[^={;]*?\bget\s+<name>(?![\w$])`, JavaScript's `\b` and
/// `\s`).
pub fn declares_getter(head: &str, name: &str) -> bool {
    let stop = head.find(['=', '{', ';']).unwrap_or(head.len());
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    head.match_indices("get").any(|(at, _)| {
        if at > stop || head[..at].chars().next_back().is_some_and(is_word) {
            return false;
        }
        let rest = &head[at + 3..];
        let after = rest.trim_start_matches(is_js_space);
        after.len() < rest.len()
            && after.starts_with(name)
            && !after[name.len()..]
                .chars()
                .next()
                .is_some_and(is_dart_name_char)
    })
}

/// The Dart member `name` a value of type `type_name` reaches (upstream
/// `dartMemberOf`): the nearest one the type or a type of its lineage
/// declares, else one an extension `on` such a type adds, an unnamed one only
/// in the reference's library. `accept` narrows the candidates (a read wants a
/// getter). Ties go to the reference's own file, then the nearest directory.
/// The flag says whether the member is an extension's.
pub(crate) fn dart_member_of(
    type_name: &str,
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    accept: &dyn Fn(&Node) -> bool,
) -> Option<(Arc<Node>, bool)> {
    let lineage = dart_lineage(type_name, context);
    let mut best: Vec<Arc<Node>> = Vec::new();
    let mut best_rank = u64::MAX;
    for member in context.get_nodes_by_name_shared(name) {
        if member.language != Language::Dart || !is_dart_member(&member) || !accept(&member) {
            continue;
        }
        let rank = if let Some(extension) = dart_extension_owner(&member, context) {
            if !extension.named
                && !crate::dart_libraries::in_same_dart_library(
                    &reference.file_path,
                    &member.file_path,
                    context,
                )
            {
                continue;
            }
            let Some(on) = extension.on.iter().filter_map(|on| lineage.get(on)).min() else {
                continue;
            };
            EXTENSION_RANK + on
        } else {
            let Some(depth) =
                member_owner(&member.qualified_name).and_then(|owner| lineage.get(owner))
            else {
                continue;
            };
            *depth
        };
        if rank < best_rank {
            best_rank = rank;
            best = vec![member];
        } else if rank == best_rank {
            best.push(member);
        }
    }
    let own = best
        .iter()
        .find(|node| node.file_path == reference.file_path)
        .cloned();
    let node = own.or_else(|| {
        best.iter().cloned().reduce(|chosen, next| {
            let proximity = |node: &Node| {
                crate::name_matcher::compute_path_proximity(&reference.file_path, &node.file_path)
            };
            if proximity(&next) > proximity(&chosen) {
                next
            } else {
                chosen
            }
        })
    })?;
    Some((node, best_rank >= EXTENSION_RANK))
}

/// A member an extension adds to `type_name` (`s.shout()` on an enum `Shape`
/// through `extension ShapeInfo on Shape`) when the type itself declares none
/// (upstream `dartExtensionMemberOf`). An inherited instance member is left to
/// the strategies that resolve it.
pub(crate) fn dart_extension_member_of(
    type_name: &str,
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let (node, via_extension) = dart_member_of(type_name, name, reference, context, &|_| true)?;
    via_extension.then_some(node)
}

/// `x.area` / `Config.instance`: the receiver and member of a Dart member read.
fn member_read_parts(name: &str) -> Option<(&str, &str)> {
    let (receiver, member) = name.split_once('.')?;
    (is_dart_identifier(receiver) && is_dart_identifier(member)).then_some((receiver, member))
}

/// Whether a reference is a Dart member read (upstream `isDartMemberRead`):
/// `x.area`, which the extractor records as a `references` ref named
/// `<receiver>.<member>`. No other Dart reference carries a dotted name: a
/// type is one identifier, a static read names only its type, and a function
/// value has its own path.
pub fn is_dart_member_read(reference: &RefView) -> bool {
    reference.language == Language::Dart
        && reference.reference_kind == EdgeKind::References
        && !reference.is_function_ref
        && member_read_parts(&reference.reference_name).is_some()
}

/// The getter a Dart member read runs (upstream `matchDartMemberRead`), or
/// nothing. A type named directly reads its static getter
/// (`Config.instance`); a variable is typed by its declaration, a parameter
/// or local in scope, else a field of the class the read is written in
/// (`final Box box;`).
pub fn match_dart_member_read(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (receiver, member) = member_read_parts(&reference.reference_name)?;
    // Most reads are of fields, which mint no nodes: a getter first.
    if !context
        .get_nodes_by_name_shared(member)
        .iter()
        .any(|node| is_dart_getter(node, context))
    {
        return None;
    }
    let names_a_type = receiver.starts_with(|c: char| c.is_ascii_uppercase())
        && context
            .get_nodes_by_name_shared(receiver)
            .iter()
            .any(|node| node.language == Language::Dart && is_dart_type_kind(node.kind));
    let type_name = if names_a_type {
        receiver.to_string()
    } else {
        crate::name_matcher::infer_local_receiver_declaration(receiver, reference, context)
            .map(|(name, _)| name)
            .or_else(|| {
                crate::member_types::infer_member_receiver_type(receiver, reference, context)
            })?
    };
    let (getter, _) = dart_member_of(&type_name, member, reference, context, &|node| {
        is_dart_getter(node, context)
    })?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: getter.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::InstanceMethod,
    })
}

/// The static constant `name` a Dart type declares, which a call through the
/// type invokes (upstream #2367): riverpod's `FutureProvider.autoDispose(…)`
/// calls `static const autoDispose = AutoDisposeFutureProviderBuilder();`. A
/// Dart type cannot also declare a method of that name, and static members
/// are not inherited, so its own constant is the callee.
pub(crate) fn dart_static_constant_of(
    class_node: &Node,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let qualified_name = format!("{}::{name}", class_node.qualified_name);
    in_order(context.get_nodes_in_file_shared(&class_node.file_path))
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::Constant
                && node.name == name
                && node.qualified_name == qualified_name
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values from upstream's `isDartGetter` pattern on the same
    /// heads.
    #[test]
    fn a_getter_is_declared_get_name_before_any_body() {
        let cases: [(&str, &str, bool); 9] = [
            ("int get area => size * size;", "area", true),
            ("static Square get unit => const Square(1);", "unit", true),
            ("String get label => name.toUpperCase();", "label", true),
            ("Map<String,\n int> get  table {", "table", true),
            ("int get areaSquared => 1;", "area", false),
            ("int grow() => size + 1;", "grow", false),
            ("set area(int v) {}", "area", false),
            ("int forget area() => 1;", "area", false),
            ("int x = get area;", "area", false),
        ];
        for (head, name, expected) in cases {
            assert_eq!(declares_getter(head, name), expected, "{head:?}");
        }
    }

    #[test]
    fn a_member_read_is_one_dotted_pair_of_names() {
        assert_eq!(member_read_parts("x.area"), Some(("x", "area")));
        assert_eq!(
            member_read_parts("Config.instance"),
            Some(("Config", "instance"))
        );
        assert_eq!(
            member_read_parts("_box.\u{24}size"),
            Some(("_box", "$size"))
        );
        assert_eq!(member_read_parts("a.b.c"), None);
        assert_eq!(member_read_parts("area"), None);
        assert_eq!(member_read_parts("x."), None);
    }
}
