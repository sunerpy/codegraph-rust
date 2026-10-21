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
    is_js_space, member_owner,
};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// Past any lineage's depth (at most [`HIERARCHY_LIMIT`]): an extension on
/// a type parameter of its own is the least specific (upstream
/// `DART_UNIVERSAL_RANK`, #2376).
const UNIVERSAL_RANK: u64 = 100;

/// The extension a Dart member is declared in: the types it is `on`, whether
/// it is named, and whether it is `on` a type parameter of its own
/// (`extension ObjectX<T> on T`, which every type has) (upstream
/// `dartExtensionOwner`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionOwner {
    pub on: Vec<String>,
    pub named: bool,
    pub universal: bool,
}

/// A Dart type's lineage: each type of it by supertype distance, and the
/// order the walk reached them in.
#[derive(Debug, Default)]
pub(crate) struct Lineage {
    pub(crate) depths: HashMap<String, u64>,
    pub(crate) order: Vec<(String, u64)>,
}

impl Lineage {
    pub(crate) fn get(&self, name: &str) -> Option<&u64> {
        self.depths.get(name)
    }
}

/// A Dart type and every type it extends, mixes in or implements, by
/// supertype distance, at most [`HIERARCHY_LIMIT`] of them (upstream
/// `dartLineage`).
pub(crate) fn dart_lineage(type_name: &str, context: &dyn ResolutionContext) -> Arc<Lineage> {
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .lineages
        .lock()
        .ok()
        .and_then(|memo| memo.get(type_name).cloned())
    {
        return hit;
    }
    let mut lineage = Lineage::default();
    let mut queue: VecDeque<(String, u64)> = VecDeque::from([(type_name.to_string(), 0)]);
    while lineage.depths.len() < HIERARCHY_LIMIT {
        let Some((name, depth)) = queue.pop_front() else {
            break;
        };
        if lineage.depths.contains_key(&name) {
            continue;
        }
        lineage.depths.insert(name.clone(), depth);
        lineage.order.push((name.clone(), depth));
        for sup in dart_supertypes_of(&name, context).iter() {
            queue.push_back((sup.clone(), depth + 1));
        }
    }
    let lineage = Arc::new(lineage);
    if let Ok(mut memo) = tables.lineages.lock() {
        memo.insert(type_name.to_string(), Arc::clone(&lineage));
    }
    lineage
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
            let on = dart_head_of(&owner, context).supers.clone();
            let universal = on.len() == 1
                && crate::member_types::class_type_parameters(&owner, context).contains(&on[0]);
            Some(Arc::new(ExtensionOwner {
                on,
                named,
                universal,
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
    let (signature, next) = crate::dart_scope::dart_member_head(node, context);
    let getter = declares_getter(&format!("{signature} {next}"), &node.name);
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
/// `min_depth` 1 skips the type's own members, as `super.` does (#2376). The
/// flag says whether the member is an extension's. A named constructor is
/// the type's, never a value's (KEEP-RUST: upstream's `dartMemberOf` reads
/// one as an instance member, so `x.value()` on a `Foo` reached `Foo.value`'s
/// constructor over an extension's `value()`).
pub(crate) fn dart_member_of(
    type_name: &str,
    name: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    accept: &dyn Fn(&Node) -> bool,
    min_depth: u64,
) -> Option<(Arc<Node>, bool)> {
    let lineage = dart_lineage(type_name, context);
    let depth_of = |t: &str| lineage.get(t).copied().filter(|depth| *depth >= min_depth);
    let mut is_project_type: Option<bool> = None;
    let mut best: Vec<Arc<Node>> = Vec::new();
    let mut best_rank = u64::MAX;
    for member in context.get_nodes_by_name_shared(name) {
        if member.language != Language::Dart
            || !is_dart_member(&member)
            || !accept(&member)
            || is_dart_constructor(&member, context)
        {
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
            // `extension ObjectX<T> on T` reaches every type, after every
            // extension that names one of its lineage, when the type is the
            // project's, so what it declares is known: a List's own `cast`
            // is not ObjectX's.
            let on = if extension.universal {
                let project = *is_project_type.get_or_insert_with(|| {
                    context
                        .get_nodes_by_name_shared(type_name)
                        .iter()
                        .any(|node| node.language == Language::Dart && is_dart_type_kind(node.kind))
                });
                if !project {
                    continue;
                }
                UNIVERSAL_RANK
            } else {
                let Some(on) = extension.on.iter().filter_map(|on| depth_of(on)).min() else {
                    continue;
                };
                on
            };
            EXTENSION_RANK + on
        } else {
            let Some(depth) = member_owner(&member.qualified_name).and_then(depth_of) else {
                continue;
            };
            depth
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
    let (node, via_extension) = dart_member_of(type_name, name, reference, context, &|_| true, 0)?;
    via_extension.then_some(node)
}

/// `x.area` / `Config.instance`: the receiver and member of a Dart member read.
pub(crate) fn member_read_parts(name: &str) -> Option<(&str, &str)> {
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
    let (getter, _) = dart_member_of(
        &type_name,
        member,
        reference,
        context,
        &|node| is_dart_getter(node, context),
        0,
    )?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: getter.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::InstanceMethod,
    })
}

/// Whether a Dart method is a named constructor — `Foo.named(…)`, `factory
/// Foo.of(…)`, `const Foo.value(…)`, `const factory Foo.x() = _X;` — read
/// from its declaration (upstream #2380 `isDartConstructor`). Dart reaches
/// one only through its class (`Foo.value(…)`, the tear-off `Foo.value`),
/// never by a bare name: a parameter `value` passed on is not `const
/// BlocProvider.value(…)`.
pub(crate) fn is_dart_constructor(node: &Node, context: &dyn ResolutionContext) -> bool {
    if node.language != Language::Dart || node.kind != NodeKind::Method {
        return false;
    }
    let tables = context.dart_tables();
    if let Some(hit) = tables
        .constructors
        .lock()
        .ok()
        .and_then(|memo| memo.get(&node.id).copied())
    {
        return hit;
    }
    let owner = member_owner(&node.qualified_name).unwrap_or("");
    let constructor = !owner.is_empty()
        && declares_constructor(
            &crate::dart_scope::dart_member_head(node, context).0,
            owner,
            &node.name,
        );
    if let Ok(mut memo) = tables.constructors.lock() {
        memo.insert(node.id.clone(), constructor);
    }
    constructor
}

/// Whether a declaration's head opens a constructor `owner.name`
/// (`^(?:(?:const|factory|external)\s+)*owner\s*\.\s*name(?![\w$])`).
fn declares_constructor(head: &str, owner: &str, name: &str) -> bool {
    let mut rest = head;
    while let Some(after) = ["const", "factory", "external"]
        .iter()
        .find_map(|keyword| rest.strip_prefix(keyword))
    {
        let spaced = after.trim_start_matches(is_js_space);
        if spaced.len() == after.len() {
            break;
        }
        rest = spaced;
    }
    let Some(rest) = rest.strip_prefix(owner) else {
        return false;
    };
    let Some(rest) = rest.trim_start_matches(is_js_space).strip_prefix('.') else {
        return false;
    };
    rest.trim_start_matches(is_js_space)
        .strip_prefix(name)
        .is_some_and(|after| !after.starts_with(is_dart_name_char))
}

/// Whether a member of a Dart type is reached through the type itself: a
/// static member, constant or enum value, or a named constructor or factory
/// (`factory ProviderContainer.test(…)`), not an instance member sharing a
/// constructor's name (`AsyncValue`'s `error` getter beside its
/// `AsyncValue.error(…)` factory, even declared on the line above it;
/// upstream `isDartStaticMember`).
pub(crate) fn is_dart_static_member(member: &Node, context: &dyn ResolutionContext) -> bool {
    member.is_static
        || member.kind == NodeKind::Constant
        || member.kind == NodeKind::EnumMember
        || is_dart_constructor(member, context)
}

/// The static member `name` the Dart type `type_name` itself declares — a
/// static method or getter, a named constructor or factory, a static
/// constant, an enum value — preferring the file `file_path`, then the
/// nearest (upstream `dartStaticMember`). Static members are not inherited.
/// `is_owner` narrows the types of that name to the one meant (the one an
/// import prefix brings in, upstream #2389).
pub(crate) fn dart_static_member(
    type_name: &str,
    name: &str,
    file_path: &str,
    context: &dyn ResolutionContext,
    is_owner: &dyn Fn(&Node) -> bool,
) -> Option<Arc<Node>> {
    let owners: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(type_name)
        .into_iter()
        .filter(|node| {
            node.language == Language::Dart && is_dart_type_kind(node.kind) && is_owner(node)
        })
        .collect();
    if owners.is_empty() {
        return None;
    }
    let found: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|member| {
            member.language == Language::Dart
                && matches!(
                    member.kind,
                    NodeKind::Method
                        | NodeKind::Function
                        | NodeKind::Constant
                        | NodeKind::EnumMember
                )
                && owners.iter().any(|owner| {
                    owner.file_path == member.file_path
                        && member.qualified_name == format!("{}::{name}", owner.qualified_name)
                })
                && !crate::dart_chains::is_setter(member, context)
                && is_dart_static_member(member, context)
        })
        .collect();
    if found.len() <= 1 {
        return found.into_iter().next();
    }
    if let Some(own) = found.iter().find(|member| member.file_path == file_path) {
        return Some(Arc::clone(own));
    }
    found.into_iter().reduce(|chosen, next| {
        let proximity =
            |node: &Node| crate::name_matcher::compute_path_proximity(file_path, &node.file_path);
        if proximity(&next) > proximity(&chosen) {
            next
        } else {
            chosen
        }
    })
}

/// A Dart call through a type's name — `BlocProvider.value(…)`, riverpod's
/// `FutureProvider.autoDispose(…)` — is to one of that type's own
/// constructors or static members, or to nothing the project declares
/// (upstream #2380 `matchDartTypeMemberCall`). Dart inherits none of them, so
/// a guess past the type is always wrong: flutter_bloc's
/// `BlocProvider.value(…)` went to `RepositoryProvider`'s `value`, and a
/// static constant's callee is the value it holds (`static const autoDispose
/// = AutoDisposeFutureProviderBuilder();`). `None` when the receiver names no
/// Dart type of the project. KEEP-RUST: a call through a type written behind
/// an import prefix (`p.Box.create()`, `p.Box<int>.named()`) is a chain's
/// link here, typed through the prefix ([`crate::dart_chains`]), where
/// upstream #2389 reads the prefix back from the line in this matcher
/// (`dartTypeCallPrefix`, for its `Box.named` record of such a call).
pub(crate) fn match_dart_type_member_call(
    type_name: &str,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<Option<ResolvedRef>> {
    if !context
        .get_nodes_by_name_shared(type_name)
        .iter()
        .any(|node| node.language == Language::Dart && is_dart_type_kind(node.kind))
    {
        return None;
    }
    let own = dart_static_member(type_name, member, &reference.file_path, context, &|_| true);
    Some(
        own.filter(|own| own.kind != NodeKind::EnumMember)
            .map(|own| ResolvedRef {
                original: reference.clone(),
                target_node_id: own.id.clone(),
                confidence: 0.85,
                resolved_by: ResolvedBy::QualifiedName,
            }),
    )
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

    /// Expected values from upstream #2380's `isDartConstructor` pattern on
    /// the same heads.
    #[test]
    fn a_constructor_is_declared_owner_dot_name_after_its_modifiers() {
        let cases: [(&str, &str, &str, bool); 14] = [
            (
                "const BlocProvider.value({required T value})",
                "BlocProvider",
                "value",
                true,
            ),
            (
                "const factory NewCarState.loading() = _Loading;",
                "NewCarState",
                "loading",
                true,
            ),
            (
                "factory Base.named() => const _BaseImpl();",
                "Base",
                "named",
                true,
            ),
            ("external const Foo.ext();", "Foo", "ext", true),
            (
                "RepositoryProvider.value({required T value});",
                "RepositoryProvider",
                "value",
                true,
            ),
            ("Foo . bar()", "Foo", "bar", true),
            ("const  factory  Foo.bar()", "Foo", "bar", true),
            ("const _$Foo.create()", "_$Foo", "create", true),
            ("Object? get error => null;", "AsyncValue", "error", false),
            (
                "static Family make() => Family._();",
                "Family",
                "make",
                false,
            ),
            ("const Foo.barBaz()", "Foo", "bar", false),
            ("const Foo.bar$x()", "Foo", "bar", false),
            ("constFoo.bar()", "Foo", "bar", false),
            ("@internal const Foo.bar()", "Foo", "bar", false),
        ];
        for (head, owner, name, expected) in cases {
            assert_eq!(
                declares_constructor(head, owner, name),
                expected,
                "{head:?}"
            );
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
