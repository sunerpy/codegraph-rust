//! Resolution for a binding whose initializer is only another callable, and
//! the local export clauses that name one (upstream `alias-binding.ts`).

use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;

use crate::frameworks::js_scan::{JS_SPACE, is_js_space, js_trim};
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

/// A name a module exports for a local binding (`{ exportedName, localName }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalExportAlias {
    pub exported_name: String,
    pub local_name: String,
}

/// The identifier pattern upstream writes `[A-Za-z_$][\w$]*`.
const IDENT: &str = "[A-Za-z_$][A-Za-z0-9_$]*";

/// Local export clauses — `export { realImpl as alias }`, `export {
/// realImpl }` with no `from` source — and CommonJS exports of a local
/// binding, in source order of each form (`extractLocalExportAliases`).
/// Type-only specifiers are skipped: they carry no runtime value.
pub(crate) fn extract_local_export_aliases(content: &str) -> Vec<LocalExportAlias> {
    static RENAMED: OnceLock<Regex> = OnceLock::new();
    static PLAIN: OnceLock<Regex> = OnceLock::new();
    static MEMBER: OnceLock<Regex> = OnceLock::new();
    static OBJECT: OnceLock<Regex> = OnceLock::new();
    static ENTRY: OnceLock<Regex> = OnceLock::new();
    let renamed = RENAMED.get_or_init(|| {
        Regex::new(&format!("^({IDENT}){JS_SPACE}+as{JS_SPACE}+({IDENT})$")).expect("renamed")
    });
    let plain = PLAIN.get_or_init(|| Regex::new(&format!("^{IDENT}$")).expect("plain"));
    let mut out = Vec::new();
    // `export { … }` NOT followed by `from`: the `from` form is a re-export.
    for body in local_export_clauses(content) {
        for raw in body.split(',') {
            let specifier = js_trim(raw);
            if specifier.is_empty()
                || specifier
                    .strip_prefix("type")
                    .and_then(|rest| rest.chars().next())
                    .is_some_and(is_js_space)
            {
                continue;
            }
            if let Some(captures) = renamed.captures(specifier) {
                out.push(LocalExportAlias {
                    exported_name: captures[2].to_string(),
                    local_name: captures[1].to_string(),
                });
            } else if plain.is_match(specifier) {
                out.push(LocalExportAlias {
                    exported_name: specifier.to_string(),
                    local_name: specifier.to_string(),
                });
            }
        }
    }
    // CommonJS: `exports.setCharset = function setCharset(…)`,
    // `module.exports.x = impl`.
    let member = MEMBER.get_or_init(|| {
        Regex::new(&format!(
            r"(?m)^[ \t]*(?:module\.)?exports\.({IDENT}){JS_SPACE}*={JS_SPACE}*(?:(?:async{JS_SPACE}+)?function{JS_SPACE}*\*?{JS_SPACE}*({IDENT}){JS_SPACE}*\(|({IDENT}){JS_SPACE}*;?[ \t]*$)"
        ))
        .expect("commonjs member")
    });
    for captures in member.captures_iter(content) {
        let local = captures
            .get(2)
            .or_else(|| captures.get(3))
            .expect("a local");
        out.push(LocalExportAlias {
            exported_name: captures[1].to_string(),
            local_name: local.as_str().to_string(),
        });
    }
    // `module.exports = { parse, format: formatDate }`.
    let object = OBJECT.get_or_init(|| {
        Regex::new(&format!(
            r"(?m)^[ \t]*module\.exports{JS_SPACE}*={JS_SPACE}*\{{([^{{}}]*)\}}"
        ))
        .expect("commonjs object")
    });
    let entry = ENTRY.get_or_init(|| {
        Regex::new(&format!(
            r"^{JS_SPACE}*({IDENT}){JS_SPACE}*(?::{JS_SPACE}*({IDENT}){JS_SPACE}*)?$"
        ))
        .expect("commonjs entry")
    });
    for captures in object.captures_iter(content) {
        for raw in captures[1].split(',') {
            if let Some(entry) = entry.captures(raw) {
                let exported = entry[1].to_string();
                let local = entry
                    .get(2)
                    .map_or_else(|| exported.clone(), |m| m.as_str().to_string());
                out.push(LocalExportAlias {
                    exported_name: exported,
                    local_name: local,
                });
            }
        }
    }
    out
}

/// The bodies of a module's local export clauses, in order: upstream's
/// `/export\s*\{([^}]*)\}\s*(?!\s*from)[;\n]/g`. After the clause's `}`
/// and the white space after it come a `;`, or a line break inside that
/// white space when no `from` follows it.
fn local_export_clauses(content: &str) -> Vec<&str> {
    let bytes = content.as_bytes();
    let skip_space = |at: usize| {
        at + (content[at..].len() - content[at..].trim_start_matches(is_js_space).len())
    };
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = content[from..].find("export") {
        let start = from + found;
        from = start + 1;
        let open = skip_space(start + "export".len());
        if bytes.get(open) != Some(&b'{') {
            continue;
        }
        let Some(close) = content[open + 1..].find('}').map(|at| open + 1 + at) else {
            continue;
        };
        let after = skip_space(close + 1);
        let end = if bytes.get(after) == Some(&b';') {
            after
        } else if content[after..].starts_with("from") {
            continue;
        } else if let Some(line_break) = content[close + 1..after].rfind('\n') {
            close + 1 + line_break
        } else {
            continue;
        };
        out.push(&content[open + 1..close]);
        from = end + 1;
    }
    out
}

/// The binding a module's default export is as a whole — `export default
/// Page;`, `export { Page as default }` — or `None` for a declaration or an
/// expression: `export default withAuth(Page)` hands over a call, whose first
/// identifier is no binding of the module's (`defaultExportBinding`).
pub(crate) fn default_export_binding(content: &str) -> Option<String> {
    static DEFAULT: OnceLock<Regex> = OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            Regex::new(&format!(
                r"(?m)(?:^|;){JS_SPACE}*export{JS_SPACE}+default{JS_SPACE}+({IDENT}){JS_SPACE}*(?:;|$)"
            ))
            .expect("default export binding")
        })
        .captures(content)
        .map(|captures| captures[1].to_string())
        .or_else(|| {
            extract_local_export_aliases(content)
                .into_iter()
                .find(|alias| alias.exported_name == "default")
                .map(|alias| alias.local_name)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        LocalExportAlias, alias_target_name, default_export_binding, extract_local_export_aliases,
    };

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

    fn aliases(content: &str) -> Vec<(String, String)> {
        extract_local_export_aliases(content)
            .into_iter()
            .map(
                |LocalExportAlias {
                     exported_name,
                     local_name,
                 }| (exported_name, local_name),
            )
            .collect()
    }

    fn pair(exported: &str, local: &str) -> (String, String) {
        (exported.to_string(), local.to_string())
    }

    /// Upstream `alias-binding.ts`, `extractLocalExportAliases`: a clause
    /// with no `from`, its renames and plain names, type-only specifiers
    /// skipped, and the CommonJS forms.
    #[test]
    fn a_local_export_clause_names_its_bindings() {
        assert_eq!(
            aliases("const a = 1;\nexport { a as b, c, type T };\n"),
            vec![pair("b", "a"), pair("c", "c")]
        );
        assert_eq!(
            aliases("export { Page as default }\n"),
            vec![pair("default", "Page")]
        );
        // A re-export is not a local clause, on the same line or the next.
        assert!(aliases("export { a } from './a';\n").is_empty());
        assert!(aliases("export { a }\nfrom './a';\n").is_empty());
        // Nothing ends the clause: no `;` and no line break after it.
        assert!(aliases("export { a }").is_empty());
        assert!(aliases("export { a } // note\n").is_empty());
        // A line break inside the white space after the clause ends it.
        assert_eq!(
            aliases("export { a }\n\nconst x = 1;"),
            vec![pair("a", "a")]
        );
        assert_eq!(
            aliases(
                "exports.setCharset = function setCharset(type) {};\nmodule.exports.x = impl;\nmodule.exports = { parse, format: formatDate }\n"
            ),
            vec![
                pair("setCharset", "setCharset"),
                pair("x", "impl"),
                pair("parse", "parse"),
                pair("format", "formatDate"),
            ]
        );
    }

    /// Upstream `alias-binding.ts`, `defaultExportBinding`.
    #[test]
    fn a_default_export_binding_is_a_name_never_an_expression() {
        assert_eq!(
            default_export_binding("export default Page;").as_deref(),
            Some("Page")
        );
        assert_eq!(
            default_export_binding("const a = 1; export default Page").as_deref(),
            Some("Page")
        );
        assert_eq!(
            default_export_binding("export { Page as default };").as_deref(),
            Some("Page")
        );
        assert_eq!(
            default_export_binding("export default withAuth(Page);"),
            None
        );
        assert_eq!(
            default_export_binding("export default function Page() {}"),
            None
        );
    }
}
