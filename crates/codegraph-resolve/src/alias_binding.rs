//! Resolution for a binding whose initializer is only another callable.

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;

use crate::name_matcher::{is_object_literal_language, same_language_family};
use crate::object_literal::resolve_object_literal_binding;
use crate::types::{RefView, ResolutionContext};

fn is_alias_binding_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Constant | NodeKind::Variable | NodeKind::Property
    )
}

fn is_callable_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Component
    )
}

fn regex_escape(value: &str) -> String {
    regex::escape(value)
}

/// Return the callable name a pure alias initializer names.
///
/// `member_name` selects an object-literal property (`= { run: impl }` or
/// `= { impl }`); `None` accepts only a bare identifier initializer.
pub(crate) fn alias_target_name(
    signature: Option<&str>,
    member_name: Option<&str>,
) -> Option<String> {
    let initializer = signature?.trim();
    if let Some(member) = member_name {
        let key = regex_escape(member);
        let explicit = Regex::new(&format!(
            r"(?:\{{|,)\s*{key}\s*:\s*([A-Za-z_$][\w$]*)\s*(?:,|\}})"
        ))
        .ok()?;
        if let Some(target) = explicit
            .captures(initializer)
            .and_then(|captures| captures.get(1))
        {
            return Some(target.as_str().to_string());
        }
        let shorthand = Regex::new(&format!(r"(?:\{{|,)\s*({key})\s*(?:,|\}})")).ok()?;
        return shorthand
            .captures(initializer)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str().to_string());
    }

    let bare =
        Regex::new(r"^=\s*([A-Za-z_$][\w$]*)\s*(?:as\s+[A-Za-z_$][\w.$<>\[\]]*\s*)?;?$").ok()?;
    bare.captures(initializer)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_string())
}

/// Follow one pure alias binding to the callable it names. Same-file targets
/// are preferred; a cross-file target must be unique.
pub(crate) fn resolve_alias_binding(
    alias: &Node,
    member_name: Option<&str>,
    context: &dyn ResolutionContext,
) -> Option<Node> {
    if !is_alias_binding_kind(alias.kind) {
        return None;
    }
    // A JS-family object member is read off the literal's own properties and
    // followed lexically from where the literal is written (#1932), not
    // pattern-matched in the (truncated) signature.
    if let Some(member) = member_name
        && is_object_literal_language(alias.language)
    {
        let reference = RefView {
            row_id: None,
            from_node_id: alias.id.clone(),
            reference_name: member.to_string(),
            reference_kind: EdgeKind::Calls,
            line: alias.start_line,
            column: alias.start_column,
            file_path: alias.file_path.clone(),
            language: alias.language,
            is_function_ref: false,
            reference_subkind: None,
        };
        let resolved = resolve_object_literal_binding(alias, member, &reference, context)?;
        return context
            .get_node_by_id_shared(&resolved.target_node_id)
            .map(|node| node.as_ref().clone());
    }
    let target_name = alias_target_name(alias.signature.as_deref(), member_name)?;
    if target_name == alias.name {
        return None;
    }
    let candidates = context
        .get_nodes_by_name_shared(&target_name)
        .into_iter()
        .filter(|candidate| {
            is_callable_kind(candidate.kind)
                && same_language_family(candidate.language, alias.language)
        })
        .collect::<Vec<_>>();
    let same_file = candidates
        .iter()
        .filter(|candidate| candidate.file_path == alias.file_path)
        .collect::<Vec<_>>();
    if same_file.len() == 1 {
        return Some(same_file[0].as_ref().clone());
    }
    if same_file.is_empty() && candidates.len() == 1 {
        return Some(candidates[0].as_ref().clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::alias_target_name;

    #[test]
    fn parses_only_pure_alias_initializers() {
        assert_eq!(
            alias_target_name(Some("= realImpl"), None).as_deref(),
            Some("realImpl")
        );
        assert_eq!(
            alias_target_name(Some("= realImpl as Handler;"), None).as_deref(),
            Some("realImpl")
        );
        assert_eq!(
            alias_target_name(Some("= { run: realImpl }"), Some("run")).as_deref(),
            Some("realImpl")
        );
        assert_eq!(
            alias_target_name(Some("= { realImpl }"), Some("realImpl")).as_deref(),
            Some("realImpl")
        );
        assert_eq!(alias_target_name(Some("= realImpl()"), None), None);
        assert_eq!(alias_target_name(Some("= () => realImpl()"), None), None);
    }
}
