//! A JSX tag naming a local of the component it is written in (upstream
//! #2442).
//!
//! `(Widget, index) => <Widget />` and outline's `const Content = variant ===
//! "dropdown" ? DropdownMenu.SubContent : ContextMenu.SubContent; … <Content
//! />` render the value the component binds, never a same-named component of
//! another file. A tag whose name its component binds itself, a parameter or
//! a plain `const` / `let` / `var` by the tag's line, renders only a
//! component, function or class the parent declares inside itself (`const
//! Row = ({ row }) => <tr />; … <Row />`), else nothing. A destructured name
//! still links by name, as a destructured call does, and a tag above the
//! local keeps the outer one.
//!
//! Upstream reads tags off the parent's text with a pattern, so it also had
//! to skip a name written in a type argument or parameter list
//! (`useState<User>()`, `<PaginatedList<Document>`, `<Entry extends Base>(…)
//! =>`); the walker emits a tag's reference from the syntax tree, where those
//! are no tags.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::name_matcher::js_function_local_scope;
use crate::types::{RefView, ResolutionContext, ResolvedRef};

/// `result` for a JSX tag's reference, unless its component binds the name
/// itself: then a component the parent declares inside itself, or nothing.
pub(crate) fn gate_jsx_local_tag(
    result: Option<ResolvedRef>,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let result = result?;
    let name = reference.reference_name.as_str();
    if reference.reference_kind != EdgeKind::References
        || !matches!(reference.language, Language::Tsx | Language::Jsx)
        || !is_tag_at(reference, context)
        || js_function_local_scope(name, reference, context).is_none()
    {
        return Some(result);
    }
    let Some(parent) = context.get_node_by_id_shared(&reference.from_node_id) else {
        return Some(result);
    };
    let inside = |node: &Node| {
        node.id != parent.id
            && node.file_path == parent.file_path
            && node.start_line >= parent.start_line
            && node.start_line <= parent.end_line
    };
    if context
        .get_node_by_id_shared(&result.target_node_id)
        .is_some_and(|target| inside(&target))
    {
        return Some(result);
    }
    let inner = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .find(|node| {
            matches!(
                node.kind,
                NodeKind::Component | NodeKind::Function | NodeKind::Class
            ) && inside(node)
        })?;
    Some(ResolvedRef {
        target_node_id: inner.id.clone(),
        ..result
    })
}

/// Whether the reference is a JSX tag's name: written right after `<`, a
/// plain name (not `<Menu.Item`).
fn is_tag_at(reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(text) = usize::try_from(reference.line)
        .ok()
        .and_then(|line| line.checked_sub(1))
        .and_then(|index| facts.raw_line(index))
    else {
        return false;
    };
    let Ok(column) = usize::try_from(reference.column) else {
        return false;
    };
    let (Some(before), Some(rest)) = (text.get(..column), text.get(column..)) else {
        return false;
    };
    before.ends_with('<')
        && rest
            .strip_prefix(reference.reference_name.as_str())
            .is_some_and(|after| {
                !after.starts_with(|c: char| {
                    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.')
                })
            })
}
