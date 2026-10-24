//! A file-routed page is served by the component its file is — ports
//! `upstream resolution/frameworks/page-component.ts` (#2267).
//!
//! Nuxt's `pages/admin.vue` is the single-file-component extractor's one
//! component of that file, named after it. Without a link a page route stood
//! alone, so the viewer's Steps tab drew nothing for a page and the routes
//! list never named what serves it. The route says so with a `calls`
//! reference by that name, and [`resolve_page_component`] lands it on the
//! SAME file's component, never on a same-named one elsewhere: every
//! `index.vue` is a component named `index`.

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};

use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The component name a single-file-component extractor gives a file: its
/// basename, `extension` off (`pageComponentName`).
pub(crate) fn page_component_name<'a>(file_path: &'a str, extension: &str) -> &'a str {
    let base = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    base.strip_suffix(extension).unwrap_or(base)
}

/// A page route's `calls` reference to its file's own component
/// (`pageComponentRef`).
pub(crate) fn page_component_ref(route: &Node, extension: &str, language: Language) -> RefView {
    RefView {
        row_id: None,
        from_node_id: route.id.clone(),
        reference_name: page_component_name(&route.file_path, extension).to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: route.file_path.clone(),
        language,
        is_function_ref: false,
        reference_subkind: None,
    }
}

/// A page route's reference to its file's own component, or `None` when
/// this is not one (`resolvePageComponent`).
pub(crate) fn resolve_page_component(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.reference_kind != EdgeKind::Calls
        || !reference
            .from_node_id
            .strip_prefix("route:")
            .and_then(|rest| rest.strip_prefix(reference.file_path.as_str()))
            .is_some_and(|rest| rest.starts_with(':'))
    {
        return None;
    }
    context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .find(|node| node.kind == NodeKind::Component && node.name == reference.reference_name)
        .map(|component| {
            ResolvedRef::new(
                reference.clone(),
                component.id.clone(),
                0.95,
                ResolvedBy::Framework,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_component_is_named_by_its_file() {
        assert_eq!(page_component_name("pages/admin.vue", ".vue"), "admin");
        assert_eq!(page_component_name("pages/users/[id].vue", ".vue"), "[id]");
        assert_eq!(page_component_name("pages\\index.vue", ".vue"), "index");
        assert_eq!(page_component_name("README", ".vue"), "README");
    }
}
