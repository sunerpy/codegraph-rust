//! A CommonJS `require('./x')` as a file import (upstream #2263
//! `commonJsRequireRefs`).
//!
//! An ESM `import … from './application'` is an import statement the walker
//! turns into an `imports` reference from the file, which the import
//! resolver lands on the module's file. A `require('./application')` is only
//! a call, so a CommonJS file depended on nothing at file level: Express's
//! `lib/express.js` requires `./application`, `./request` and `./response`
//! and the index said it imports nothing. Each local specifier a
//! `require(…)` call names is now an `imports` reference from the file. A
//! package (`express`, `lodash/fp`) stays outside the index, and the names a
//! binding is called through keep resolving through the require binding.
//!
//! Upstream reads the comment-stripped source with a regex; the syntax tree
//! is read here, which leaves comments, strings and TypeScript's
//! `import x = require('y')` (an import statement already) out by
//! construction.

use codegraph_core::types::{EdgeKind, Language, UnresolvedRef};
use tree_sitter::Node as SyntaxNode;

/// The `imports` references of `root`'s local `require('…')` calls, once per
/// specifier in source order, from the file node of `file_path`. Empty
/// outside JavaScript, JSX, TypeScript and TSX.
pub(crate) fn require_refs(
    file_path: &str,
    source: &str,
    root: SyntaxNode<'_>,
    language: Language,
) -> Vec<UnresolvedRef> {
    if !matches!(
        language,
        Language::JavaScript | Language::Jsx | Language::TypeScript | Language::Tsx
    ) || !source.contains("require")
    {
        return Vec::new();
    }
    let mut out: Vec<UnresolvedRef> = Vec::new();
    let mut stack = vec![root];
    let mut found: Vec<(usize, String, i64, i64)> = Vec::new();
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression"
            && let Some(specifier) = required_specifier(node, source)
        {
            let position = node.start_position();
            found.push((
                node.start_byte(),
                specifier,
                position.row as i64 + 1,
                position.column as i64,
            ));
        }
        let mut cursor = node.walk();
        let children: Vec<SyntaxNode<'_>> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    found.sort_by_key(|(at, ..)| *at);
    for (_, specifier, line, col) in found {
        if !is_local_specifier(&specifier) || out.iter().any(|r| r.reference_name == specifier) {
            continue;
        }
        out.push(UnresolvedRef {
            id: None,
            from_node_id: format!("file:{file_path}"),
            reference_name: specifier,
            reference_kind: EdgeKind::Imports,
            line,
            col,
            candidates: None,
            file_path: file_path.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        });
    }
    out
}

/// The specifier of `require('x')`: a bare `require` (not `foo.require(…)`,
/// not `require.resolve(…)`) called with one literal string, or a template
/// literal with no `${…}` hole, on one line.
fn required_specifier(call: SyntaxNode<'_>, source: &str) -> Option<String> {
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" || function.utf8_text(source.as_bytes()).ok()? != "require" {
        return None;
    }
    let arguments = call.child_by_field_name("arguments")?;
    if arguments.kind() != "arguments"
        || !source
            .get(function.end_byte()..arguments.start_byte())?
            .trim()
            .is_empty()
    {
        return None;
    }
    let mut cursor = arguments.walk();
    let mut named = arguments.named_children(&mut cursor);
    let literal = named.next()?;
    if named.next().is_some() {
        return None;
    }
    let text = literal.utf8_text(source.as_bytes()).ok()?;
    let inner = match literal.kind() {
        "string" => text
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .or_else(|| {
                text.strip_prefix('"')
                    .and_then(|rest| rest.strip_suffix('"'))
            })?,
        "template_string" => {
            let inner = text.strip_prefix('`')?.strip_suffix('`')?;
            if inner.contains('$') {
                return None;
            }
            inner
        }
        _ => return None,
    };
    let specifier = inner.trim();
    (!specifier.is_empty() && !inner.contains('\n')).then(|| specifier.to_string())
}

/// A module of this project: a relative or rooted path, or a project alias
/// (`@/lib/x`, `~/x`, `#internal`). A package is outside the index.
fn is_local_specifier(specifier: &str) -> bool {
    matches!(specifier, "." | "..")
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with('/')
        || specifier.starts_with("@/")
        || specifier.starts_with("~/")
        || specifier.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::is_local_specifier;

    #[test]
    fn local_specifiers_are_paths_and_project_aliases() {
        for local in [
            ".",
            "..",
            "./x",
            "../x",
            "/abs",
            "@/lib/x",
            "~/x",
            "#internal",
        ] {
            assert!(is_local_specifier(local), "{local}");
        }
        for package in [
            "express",
            "lodash/fp",
            "@scope/pkg",
            "node:fs",
            ".hidden",
            "~x",
        ] {
            assert!(!is_local_specifier(package), "{package}");
        }
    }
}
