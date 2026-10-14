//! A bare JS-family call through a name destructured from a call's result
//! (upstream #2163 `matchDestructuredCallResult`).
//!
//! `const { getDefaultActivityRoute } = useDefaultActivity();
//! getDefaultActivityRoute()` binds the name locally, which rules out every
//! cross-file candidate, so the call resolved to nothing. It is the function
//! the callee returns under that key: one declared in the callee's own body,
//! else a top-level one of the callee's module it returns. The callee is
//! resolved through the file's imports, else found in the same file, else the
//! project's one function of that name (an alias the import resolver cannot
//! follow, like Nuxt's `~/`), and its source must return the key. A later
//! declaration of the name at the call's scope shadows the binding.
//!
//! The file's destructuring statements are read once per pass (upstream
//! re-blanks the text above each call; #2362's lesson: memoise per file).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;

use crate::name_matcher::{range_within, same_language_family, same_range};
use crate::source_facts::SourceFacts;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// One `const { … } = f(…)` statement.
#[derive(Debug)]
struct Binding {
    /// Just past the `(` of its call.
    end: usize,
    /// The function it calls.
    callee: String,
    /// Where the block around it closes (the code's length at top level).
    scope_end: usize,
}

/// A file's destructuring statements, read once.
#[derive(Debug, Default)]
pub struct DestructuredFile {
    bindings: Vec<Binding>,
    /// Each name a binding introduces → (binding, the key it reads), in file
    /// order.
    by_name: HashMap<String, Vec<(usize, String)>>,
}

/// The function a bare call through a destructured name reaches, if any.
pub(crate) fn match_destructured_call_result(
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let facts = context.source_facts(&reference.file_path)?;
    let file = facts.js_destructured_calls(read_destructured_file)?;
    let candidates = file.by_name.get(&reference.reference_name)?;
    let code = facts.ts_blanked();
    let cut = call_offset(&facts, reference)?;
    // The nearest binding above the call whose block is still open there.
    let (binding, key) = candidates.iter().rev().find_map(|(index, key)| {
        let binding = &file.bindings[*index];
        (binding.end <= cut && cut <= binding.scope_end).then_some((binding, key))
    })?;
    destructured_call_target(
        &binding.callee,
        key,
        code.get(binding.end..cut).unwrap_or_default(),
        reference,
        context,
    )
}

/// The byte offset of the reference in its file's source.
fn call_offset(facts: &SourceFacts, reference: &RefView) -> Option<usize> {
    let index = usize::try_from(reference.line).ok()?.checked_sub(1)?;
    let start = facts.lines().raw_line_start(index)?;
    let line = facts.raw_line(index)?;
    let mut column = usize::try_from(reference.column)
        .unwrap_or(0)
        .min(line.len());
    while !line.is_char_boundary(column) {
        column -= 1;
    }
    Some(start + column)
}

fn read_destructured_file(facts: &SourceFacts) -> Option<DestructuredFile> {
    static PRESENT: OnceLock<Regex> = OnceLock::new();
    static BINDING: OnceLock<Regex> = OnceLock::new();
    static IDENTIFIER: OnceLock<Regex> = OnceLock::new();
    static DEFAULT: OnceLock<Regex> = OnceLock::new();
    if !PRESENT
        .get_or_init(|| Regex::new(r"(?-u:\b)(?:const|let|var)\s*\{").expect("destructuring"))
        .is_match(facts.source())
    {
        return None;
    }
    let code = facts.ts_blanked();
    let binding = BINDING.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)(?:const|let|var)\s*\{([^{}]*)\}\s*=\s*(?:await\s+)?([A-Za-z_$][0-9A-Za-z_$]*)\s*(?:<[^<>()]*>)?\s*\(",
        )
        .expect("destructuring binding")
    });
    let identifier =
        IDENTIFIER.get_or_init(|| Regex::new(r"^[A-Za-z_$][0-9A-Za-z_$]*$").expect("identifier"));
    let default = DEFAULT.get_or_init(|| Regex::new(r"\s*=.*$").expect("default"));
    let found: Vec<(usize, usize, String, String)> = binding
        .captures_iter(code)
        .filter_map(|captures| {
            let whole = captures.get(0)?;
            Some((
                whole.start(),
                whole.end(),
                captures[2].to_string(),
                captures[1].to_string(),
            ))
        })
        .collect();
    // Each binding's block: the `}` closing the innermost `{` open at its
    // start, found in one pass over the code.
    let mut open: Vec<usize> = Vec::new();
    let mut innermost: Vec<Option<usize>> = vec![None; found.len()];
    let mut closes: HashMap<usize, usize> = HashMap::new();
    let mut next = 0;
    for (at, byte) in code.bytes().enumerate() {
        while next < found.len() && found[next].0 == at {
            innermost[next] = open.last().copied();
            next += 1;
        }
        match byte {
            b'{' => open.push(at),
            b'}' => {
                if let Some(brace) = open.pop() {
                    closes.insert(brace, at);
                }
            }
            _ => {}
        }
    }
    let mut file = DestructuredFile::default();
    for (index, (_, end, callee, pattern)) in found.into_iter().enumerate() {
        let scope_end = innermost[index].map_or(code.len(), |brace| {
            closes.get(&brace).copied().unwrap_or(code.len())
        });
        file.bindings.push(Binding {
            end,
            callee,
            scope_end,
        });
        for part in pattern.split(',') {
            let mut sides = part
                .split(':')
                .map(|side| default.replace(side.trim(), "").into_owned());
            let key = sides.next().unwrap_or_default();
            let local = sides.next().unwrap_or_else(|| key.clone());
            if identifier.is_match(&key) {
                file.by_name.entry(local).or_default().push((index, key));
            }
        }
    }
    Some(file)
}

/// The function `callee` returns under `key`, for a bare call `rest` (the
/// code between the binding and the call) does not shadow.
fn destructured_call_target(
    callee_name: &str,
    key: &str,
    rest: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let name = regex::escape(&reference.reference_name);
    let shadow = Regex::new(&format!(
        r"(?-u:\b)(?:const|let|var|function|class)\s+(?:{name}(?-u:\b)|\{{[^}}]*(?-u:\b){name}(?-u:\b))"
    ))
    .ok()?;
    if shadow.is_match(rest) {
        return None;
    }
    let imported = crate::import_resolver::resolve_via_import(
        &RefView {
            reference_name: callee_name.to_string(),
            reference_kind: EdgeKind::Calls,
            ..reference.clone()
        },
        context,
    )
    .and_then(|resolved| context.get_node_by_id_shared(&resolved.target_node_id));
    // Through the import; else the same file's; else the one function of that
    // name in the project (an alias the import resolver cannot follow, like
    // Nuxt's `~/composables/…`). The returned key is checked below either way.
    let holders: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(callee_name)
        .into_iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function | NodeKind::Constant | NodeKind::Variable
            ) && same_language_family(n.language, reference.language)
        })
        .collect();
    let callee = imported
        .or_else(|| {
            holders
                .iter()
                .find(|n| n.file_path == reference.file_path)
                .cloned()
        })
        .or_else(|| (holders.len() == 1).then(|| Arc::clone(&holders[0])))?;
    if !same_language_family(callee.language, reference.language) {
        return None;
    }
    let facts = context.source_facts(&callee.file_path)?;
    let first = usize::try_from(callee.start_line).ok()?.checked_sub(1)?;
    let last = usize::try_from(callee.end_line)
        .ok()?
        .min(facts.line_count());
    if first >= last {
        return None;
    }
    let callee_text = facts.join_lines(first, last, "\n");
    let returns = Regex::new(&format!(
        r"(?s)(?-u:\b)return\s*\{{.*?(?-u:\b){}(?-u:\b)",
        regex::escape(key)
    ))
    .ok()?;
    if !returns.is_match(&callee_text) {
        return None;
    }
    let callable = |n: &Node| {
        matches!(
            n.kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Constant | NodeKind::Variable
        )
    };
    let is_function = |n: &Node| matches!(n.kind, NodeKind::Function | NodeKind::Method);
    let in_file = context.get_nodes_in_file_shared(&callee.file_path);
    // Declared in the callee's own body, not inside a function of it.
    let inner: Vec<&Arc<Node>> = in_file
        .iter()
        .filter(|n| {
            n.name == key
                && callable(n)
                && n.id != callee.id
                && range_within(n, &callee)
                && !in_file.iter().any(|f| {
                    f.id != callee.id
                        && f.id != n.id
                        && is_function(f)
                        && range_within(f, &callee)
                        && range_within(n, f)
                        && !same_range(f, n)
                })
        })
        .collect();
    // Else a top-level one of the callee's module.
    let pool: Vec<&Arc<Node>> = if inner.is_empty() {
        in_file
            .iter()
            .filter(|n| {
                n.name == key
                    && callable(n)
                    && !n.qualified_name.contains("::")
                    && !in_file.iter().any(|f| {
                        is_function(f) && f.id != n.id && range_within(n, f) && !same_range(f, n)
                    })
            })
            .collect()
    } else {
        inner
    };
    // A function before a value, in file order otherwise.
    let target = pool
        .iter()
        .find(|n| n.kind == NodeKind::Function)
        .or_else(|| pool.first())?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence: 0.85,
        resolved_by: ResolvedBy::InstanceMethod,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::read_destructured_file;
    use crate::source_facts::SourceFacts;

    #[test]
    fn bindings_name_their_keys_callees_and_blocks() {
        let source = "const { login, signOut: leave, retries = 3 } = useAuth();\nfunction go() {\n  const { t } = await useI18n<Messages>();\n  return t('x');\n}\nconst { 'quoted': q } = load();\n";
        let facts = SourceFacts::new(Arc::from(source));
        let file = read_destructured_file(&facts).expect("destructuring");
        let key = |local: &str| -> Vec<(String, String)> {
            file.by_name
                .get(local)
                .map(|found| {
                    found
                        .iter()
                        .map(|(index, key)| (file.bindings[*index].callee.clone(), key.clone()))
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(key("login"), [("useAuth".to_string(), "login".to_string())]);
        assert_eq!(
            key("leave"),
            [("useAuth".to_string(), "signOut".to_string())]
        );
        assert_eq!(
            key("retries"),
            [("useAuth".to_string(), "retries".to_string())]
        );
        assert_eq!(key("t"), [("useI18n".to_string(), "t".to_string())]);
        // A quoted key is no identifier.
        assert!(key("q").is_empty());
        // The top-level binding's block is the whole file; `t`'s is `go`'s body.
        assert_eq!(file.bindings[0].scope_end, facts.ts_blanked().len());
        assert_eq!(
            file.bindings[1].scope_end,
            source
                .find("}\nconst { 'quoted'")
                .expect("go's closing brace")
        );
        assert!(read_destructured_file(&SourceFacts::new(Arc::from("const a = b();\n"))).is_none());
    }
}
