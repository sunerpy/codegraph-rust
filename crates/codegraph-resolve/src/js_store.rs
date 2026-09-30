//! Store-action binding for JS/TS state stores (upstream v1.6.1 #1862).
//!
//! A Zustand-style store — `export const useStore = create((set, get) => ({
//! reset: () => set({}) }))` — owns its actions as members of the store
//! constant. Four call forms reach one of them:
//!
//! * `get().reset()` inside the store factory, where `get` is a parameter of
//!   the factory arrow;
//! * `useStore.getState().reset()` anywhere the store name is bound;
//! * a bare `reset()` destructured from `useStore.getState()` in scope;
//! * a bare `selected()` bound by a selector `useStore((s) => s.reset)` of a
//!   store created by Zustand's `create`.
//!
//! Each form identifies exactly one store and resolves the member inside that
//! store's own object literal — never a same-named function elsewhere, and
//! never an interface signature. Anything less certain stays unresolved.

use std::collections::HashSet;
use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;

use crate::awaited::{declaration_offsets, has_parameter_binding, import_shadowed_at};
use crate::import_resolver::resolve_via_import;
use crate::name_matcher::{
    enclosing_scope_start_line, is_bare_js_call, is_js_family, is_lexically_reachable,
    range_within, resolve_object_literal_member, same_language_family,
};
use crate::source_facts::SourceFacts;
use crate::strip_comments::{CommentLang, blank_string_contents, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// `get().m` / `getState().m` / `store.getState().m` for a TS/JS/Python call
/// whose receiver is itself a call — the one fallback such a chain keeps
/// (#1683). TS/JS resolves the member inside the identified store; Python
/// keeps upstream's unique-callable fallback.
pub(crate) fn match_store_accessor_chain(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let (inner, method) = split_accessor_chain(&reference.reference_name)?;
    if !(inner == "get" || inner == "getState" || inner.ends_with(".getState")) {
        return None;
    }
    if is_js_family(reference.language) {
        return resolve_store_action(inner, method, reference, context, false);
    }
    let callables = context
        .get_nodes_by_name_shared(method)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && same_language_family(node.language, reference.language)
                && node.id != reference.from_node_id
        })
        .collect::<Vec<_>>();
    let [only] = callables.as_slice() else {
        return None;
    };
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: only.id.clone(),
        confidence: 0.6,
        resolved_by: ResolvedBy::ExactMatch,
    })
}

/// A bare JS/TS call bound to a store action by a destructuring of
/// `store.getState()` or by a selector — the binding need not share the
/// action's name, so the resolver's symbol-existence prefilter must let it
/// through.
pub(crate) fn match_js_store_binding_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != EdgeKind::Calls || !is_bare_js_call(reference, context) {
        return None;
    }
    match_destructured_store_call(reference, context)
        .or_else(|| match_selected_store_call(reference, context))
}

/// `^([\w$.]+)\(\)\.(\w+)$`.
fn split_accessor_chain(name: &str) -> Option<(&str, &str)> {
    let (inner, method) = name.split_once("().")?;
    let inner_ok = !inner.is_empty()
        && inner
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '.'));
    let method_ok = !method.is_empty()
        && method
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    (inner_ok && method_ok).then_some((inner, method))
}

/// Resolve `member` inside the store `inner` names: the enclosing factory for
/// `get`/`getState`, else the store bound to `<name>.getState`.
fn resolve_store_action(
    inner: &str,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
    selector: bool,
) -> Option<ResolvedRef> {
    let holders: Vec<Node> = if inner == "get" || inner == "getState" {
        let caller = context.get_node_by_id_shared(&reference.from_node_id)?;
        let facts = context.source_facts(&reference.file_path)?;
        context
            .get_nodes_in_file_shared(&reference.file_path)
            .into_iter()
            .filter(|node| {
                matches!(node.kind, NodeKind::Constant | NodeKind::Variable)
                    && range_within(&caller, node)
                    && factory_takes_accessor(&facts, node, &caller, inner)
            })
            .map(|node| (*node).clone())
            .collect()
    } else {
        let name = inner.strip_suffix(".getState")?;
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$'))
        {
            return None;
        }
        let mut lookup = reference.clone();
        lookup.reference_name = name.to_string();
        lookup.reference_kind = EdgeKind::References;
        let imported = resolve_via_import(&lookup, context)
            .and_then(|resolved| context.get_node_by_id_shared(&resolved.target_node_id));
        match imported {
            Some(node) => {
                if import_shadowed_at(name, reference, context) {
                    return None;
                }
                vec![(*node).clone()]
            }
            None => context
                .get_nodes_by_name_shared(name)
                .into_iter()
                .filter(|node| {
                    node.file_path == reference.file_path
                        && is_lexically_reachable(node, reference, context)
                })
                .map(|node| (*node).clone())
                .collect(),
        }
    };
    let [holder] = holders.as_slice() else {
        return None;
    };
    if selector && !is_zustand_hook(holder, context) {
        return None;
    }
    resolve_object_literal_member(
        holder,
        member,
        reference,
        context,
        0.9,
        ResolvedBy::InstanceMethod,
    )
}

/// Whether the store factory enclosing `caller` takes `accessor` as its
/// second parameter — `(set, get) =>` / `(set, get, api) =>`.
fn factory_takes_accessor(
    facts: &SourceFacts,
    holder: &Node,
    caller: &Node,
    accessor: &str,
) -> bool {
    static GET: OnceLock<Regex> = OnceLock::new();
    static GET_STATE: OnceLock<Regex> = OnceLock::new();
    let pattern = |cell: &'static OnceLock<Regex>, name: &str| {
        cell.get_or_init(|| {
            Regex::new(&format!(
                r"\(\s*[A-Za-z0-9_$]+\s*,\s*{name}\s*(?:,\s*[A-Za-z0-9_$]+\s*)?\)\s*=>"
            ))
            .expect("store accessor parameter pattern")
        })
    };
    let pattern = match accessor {
        "get" => pattern(&GET, "get"),
        _ => pattern(&GET_STATE, "getState"),
    };
    let start = holder.start_line.saturating_sub(1).max(0) as usize;
    let end = (caller.start_line.max(0) as usize).min(facts.line_count());
    start < end && pattern.is_match(&facts.join_lines(start, end, "\n"))
}

/// Only a Zustand hook promises to return a selector's result:
/// `const useStore = create(...)` with `create` imported from `zustand`.
fn is_zustand_hook(holder: &Node, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(&holder.file_path) else {
        return false;
    };
    let start = holder.start_line.saturating_sub(1).max(0) as usize;
    let end = (holder.end_line.max(holder.start_line).max(0) as usize).min(facts.line_count());
    if start >= end {
        return false;
    }
    let text = facts.join_lines(start, end, "\n");
    let Ok(pattern) = Regex::new(&format!(
        r"(?-u:\b)(?:const|let)\s+{}\s*=\s*([A-Za-z0-9_$]+)\s*[<(]",
        regex::escape(&holder.name)
    )) else {
        return false;
    };
    let Some(factory) = pattern
        .captures(&text)
        .and_then(|captures| captures.get(1))
        .map(|factory| factory.as_str())
    else {
        return false;
    };
    context
        .get_import_mappings(&holder.file_path, holder.language)
        .iter()
        .any(|mapping| {
            mapping.local_name == factory
                && mapping.source == "zustand"
                && (mapping.exported_name == "create" || mapping.is_default)
        })
}

/// A bare call to a name destructured from `store.getState()` in an
/// enclosing block of the same function: `const { reset } =
/// useStore.getState(); reset();`.
fn match_destructured_store_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let facts = context.source_facts(&reference.file_path)?;
    if !facts.js_get_state_file(|source| source.contains(".getState")) {
        return None;
    }
    let start = enclosing_scope_start_line(reference, context).saturating_sub(1);
    let code = code_before(&facts, start.max(0) as usize, reference)?;
    static BINDING: OnceLock<Regex> = OnceLock::new();
    let binding = BINDING.get_or_init(|| {
        Regex::new(r"(?-u:\b)const\s*\{([^{}]*)\}\s*=\s*([A-Za-z0-9_$]+)\.getState\s*\(\s*\)")
            .expect("destructured store binding pattern")
    });
    let call_scope = brace_stack(&code, code.len());
    let name = reference.reference_name.as_str();
    let bindings = binding.captures_iter(&code).collect::<Vec<_>>();
    for captures in bindings.iter().rev() {
        let (Some(whole), Some(names), Some(store)) =
            (captures.get(0), captures.get(1), captures.get(2))
        else {
            continue;
        };
        // Plain named bindings only; defaults, rest and computed keys need
        // their own value tracing rather than a same-name guess.
        if !names.as_str().split(',').any(|part| part.trim() == name) {
            continue;
        }
        if !encloses(&brace_stack(&code, whole.start()), &call_scope) {
            continue;
        }
        // Another declaration between the binding and the call shadows it.
        if !declaration_offsets(&code[whole.end()..], name).is_empty() {
            return None;
        }
        return resolve_store_action(
            &format!("{}.getState", store.as_str()),
            name,
            reference,
            context,
            false,
        );
    }
    None
}

/// A bare call to a selector binding `const selected = useStore((s) =>
/// s.reset)` in an enclosing block — closures may capture it; sibling scopes
/// and shadowing parameters or declarations cannot donate it.
fn match_selected_store_call(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let facts = context.source_facts(&reference.file_path)?;
    if !facts.source().contains("=>") {
        return None;
    }
    let name = reference.reference_name.as_str();
    if !facts.js_selector_names(selector_bound_names).contains(name) {
        return None;
    }
    let code = code_before(&facts, 0, reference)?;
    let binding = Regex::new(&format!(
        r"(?-u:\b)const\s+{}\s*=\s*([A-Za-z0-9_$]+)\s*\(\s*(?:\(\s*([A-Za-z0-9_$]+)\s*\)|([A-Za-z0-9_$]+))\s*=>\s*([A-Za-z0-9_$]+)\.([A-Za-z0-9_$]+)\s*\)",
        regex::escape(name)
    ))
    .ok()?;
    let call_scope = brace_stack(&code, code.len());
    let bindings = binding.captures_iter(&code).collect::<Vec<_>>();
    for captures in bindings.iter().rev() {
        let (Some(whole), Some(store), Some(object), Some(member)) = (
            captures.get(0),
            captures.get(1),
            captures.get(4),
            captures.get(5),
        ) else {
            continue;
        };
        let parameter = captures.get(2).or_else(|| captures.get(3));
        if parameter.map(|parameter| parameter.as_str()) != Some(object.as_str()) {
            continue;
        }
        if !encloses(&brace_stack(&code, whole.start()), &call_scope) {
            continue;
        }
        let rest = &code[whole.end()..];
        if !declaration_offsets(rest, name).is_empty() || has_parameter_binding(rest, name) {
            return None;
        }
        return resolve_store_action(
            &format!("{}.getState", store.as_str()),
            member.as_str(),
            reference,
            context,
            true,
        );
    }
    None
}

/// Names a file binds with `const name = fn((s) => …` or `fn(s => …`.
fn selector_bound_names(source: &str) -> HashSet<String> {
    static SELECTOR: OnceLock<Regex> = OnceLock::new();
    SELECTOR
        .get_or_init(|| {
            Regex::new(
                r"(?-u:\b)const\s+([A-Za-z0-9_$]+)\s*=\s*[A-Za-z0-9_$]+\s*\(\s*(?:\(\s*[A-Za-z0-9_$]+\s*\)|[A-Za-z0-9_$]+)\s*=>",
            )
            .expect("selector binding pattern")
        })
        .captures_iter(source)
        .filter_map(|captures| captures.get(1).map(|name| name.as_str().to_string()))
        .collect()
}

/// The comment-free, string-blanked source from 0-based line `start` up to
/// the reference's column.
fn code_before(facts: &SourceFacts, start: usize, reference: &RefView) -> Option<String> {
    let line_index = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    let lines = facts.lines();
    let from = lines.raw_line_start(start.min(line_index))?;
    let line_start = lines.raw_line_start(line_index)?;
    let column =
        (reference.column.max(0) as usize).min(facts.raw_line(line_index).map_or(0, str::len));
    let end = line_start + column;
    let source = facts.source();
    if !source.is_char_boundary(from) || !source.is_char_boundary(end) || from > end {
        return None;
    }
    Some(blank_string_contents(&strip_comments_for_regex(
        &source[from..end],
        CommentLang::TypeScript,
    )))
}

/// Offsets of the `{` open at `end` in `code`, outermost first.
fn brace_stack(code: &str, end: usize) -> Vec<usize> {
    let mut stack = Vec::new();
    for (offset, byte) in code.bytes().enumerate().take(end) {
        match byte {
            b'{' => stack.push(offset),
            b'}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
}

/// Whether the block stack of a binding is a prefix of the call's — the
/// binding's block encloses the call rather than being a sibling of it.
fn encloses(binding: &[usize], call: &[usize]) -> bool {
    binding
        .iter()
        .enumerate()
        .all(|(depth, open)| call.get(depth) == Some(open))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::Language;

    #[test]
    fn accessor_chains_split_only_the_encoded_shape() {
        assert_eq!(
            split_accessor_chain("useStore.getState().reset"),
            Some(("useStore.getState", "reset"))
        );
        assert_eq!(split_accessor_chain("get().reset"), Some(("get", "reset")));
        assert_eq!(split_accessor_chain("get().a.b"), None);
        assert_eq!(split_accessor_chain("().reset"), None);
        assert_eq!(split_accessor_chain("make(x).reset"), None);
    }

    #[test]
    fn selector_names_need_a_callback_argument() {
        let names = selector_bound_names(
            "const a = useStore((s) => s.reset);\nconst b = other(s => s.x);\nconst c = plain(1);\n",
        );
        assert_eq!(names, HashSet::from(["a".to_string(), "b".to_string()]));
    }

    #[test]
    fn brace_stacks_distinguish_enclosing_from_sibling_blocks() {
        let code = "{ a { b } c { d";
        let call = brace_stack(code, code.len());
        assert!(encloses(&brace_stack(code, 2), &call));
        assert!(!encloses(&brace_stack(code, 6), &call));
        assert!(encloses(&brace_stack(code, 14), &call));
    }

    #[test]
    fn python_chains_are_not_js_family() {
        assert!(!is_js_family(Language::Python));
    }
}
