//! Awaited JS/TS receiver inference (`inferEsmAwaitedCallType`, upstream
//! v1.6.1, #1840/#1885).
//!
//! For `const handle = await makeEngine(); handle.run()`, the receiver's type is
//! the declared return type of the awaited callee, unwrapped from one
//! `Promise<T>` layer. Every step is proof-carrying: the binding must be
//! visible in the brace scope of the call, not re-declared or re-assigned
//! before it, and bound to a bare call (no trailing member/index/call); the
//! callee must be an imported function or the one lexically reachable local
//! function; and the annotation must name a project class or interface. An
//! awaited shape that fails any step is [`AwaitedInference::Unresolved`], which
//! callers must not turn into a name-based guess.
//!
//! Offsets: the comment- and literal-blanked view replaces character for
//! character, so a source position maps to the view by its line and the
//! number of characters before it on that line.

use crate::import_resolver::resolve_via_import;
use crate::source_facts::{LineIndex, SourceFacts};
use crate::strip_comments::{CommentLang, blank_string_contents, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext};
use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

/// Upstream `TS_PRIMITIVE_TYPES`.
const TS_PRIMITIVE_TYPES: [&str; 12] = [
    "string",
    "number",
    "boolean",
    "bigint",
    "symbol",
    "void",
    "undefined",
    "null",
    "never",
    "unknown",
    "any",
    "object",
];

/// Outcome of inspecting a receiver for an awaited binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AwaitedInference {
    /// No awaited binding of this name is visible: other strategies proceed.
    NotApplicable,
    /// The awaited shape is present but its type is unknown, primitive or
    /// unsafe; callers must not continue into name heuristics.
    Unresolved,
    /// The project class/interface the awaited value has, and its file.
    Inferred {
        type_name: String,
        file_path: String,
    },
}

/// One `{ … }` scope of the blanked view; the root has no opening brace.
#[derive(Debug)]
struct Scope {
    start: Option<usize>,
    end: usize,
    parent: Option<usize>,
}

/// Per-file awaited-binding index (`AWAITED_FILES`, prepared lazily).
#[derive(Debug)]
pub struct AwaitedIndex {
    lines: LineIndex,
    names: HashSet<String>,
    scopes: Vec<Scope>,
    declarations: HashMap<String, Vec<(usize, usize)>>,
}

fn awaited_binding_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)(?:const|let|var)\s+([0-9A-Za-z_$]+)\s*=\s*await\s+[0-9A-Za-z_$]+\s*\(",
        )
        .expect("awaited binding pattern")
    })
}

/// Names that raw source binds as `const x = await f(` — the cheap
/// eligibility check that runs before any sanitising.
pub(crate) fn raw_awaited_names(source: &str) -> HashSet<String> {
    awaited_binding_pattern()
        .captures_iter(source)
        .filter_map(|captures| captures.get(1).map(|name| name.as_str().to_string()))
        .collect()
}

impl AwaitedIndex {
    /// Index the blanked view `code`: its awaited names, brace scope tree and
    /// the declarations of those names.
    pub(crate) fn new(code: &str) -> Self {
        let names: HashSet<String> = awaited_binding_pattern()
            .captures_iter(code)
            .filter_map(|captures| captures.get(1).map(|name| name.as_str().to_string()))
            .collect();
        let mut scopes = vec![Scope {
            start: None,
            end: code.len(),
            parent: None,
        }];
        let mut stack = vec![0usize];
        for (offset, byte) in code.bytes().enumerate() {
            if byte == b'{' {
                let parent = *stack.last().expect("root scope");
                scopes.push(Scope {
                    start: Some(offset),
                    end: code.len(),
                    parent: Some(parent),
                });
                stack.push(scopes.len() - 1);
            } else if byte == b'}' && stack.len() > 1 {
                let closed = stack.pop().expect("open scope");
                scopes[closed].end = offset;
            }
        }
        static DECLARATION: OnceLock<Regex> = OnceLock::new();
        let declaration = DECLARATION.get_or_init(|| {
            Regex::new(r"(?-u:\b)(?:const|let|var)\s+([0-9A-Za-z_$]+)\s*=\s*").expect("declaration")
        });
        let mut declarations: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for captures in declaration.captures_iter(code) {
            let (Some(whole), Some(name)) = (captures.get(0), captures.get(1)) else {
                continue;
            };
            if names.contains(name.as_str()) {
                declarations
                    .entry(name.as_str().to_string())
                    .or_default()
                    .push((whole.start(), whole.len()));
            }
        }
        Self {
            lines: LineIndex::new(code),
            names,
            scopes,
            declarations,
        }
    }

    /// The innermost scope containing `offset` (binary search over openings,
    /// then climb out of scopes already closed).
    fn scope_at(&self, offset: usize) -> usize {
        let (mut lo, mut hi) = (0usize, self.scopes.len());
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.scopes[mid].start.is_some_and(|start| start < offset) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        while lo > 0 && self.scopes[lo].end < offset {
            lo = self.scopes[lo].parent.unwrap_or(0);
        }
        lo
    }

    fn visible_at(&self, declaration: usize, use_at: usize) -> bool {
        let ancestor = self.scope_at(declaration);
        let mut scope = Some(self.scope_at(use_at));
        while let Some(current) = scope {
            if current == ancestor {
                return true;
            }
            scope = self.scopes[current].parent;
        }
        false
    }
}

/// Byte offset in `to` of the position that `from_col` bytes into line
/// `line_index` of `from` denotes, matching by character count.
fn map_column(
    from: &str,
    from_lines: &LineIndex,
    to: &str,
    to_lines: &LineIndex,
    line_index: usize,
    from_col: usize,
) -> Option<usize> {
    let from_line = from_lines.raw_line(from, line_index)?;
    let from_col = from_col.min(from_line.len());
    if !from_line.is_char_boundary(from_col) {
        return None;
    }
    let chars = from_line[..from_col].chars().count();
    let to_start = to_lines.raw_line_start(line_index)?;
    let to_line = to_lines.raw_line(to, line_index)?;
    let within = to_line
        .char_indices()
        .nth(chars)
        .map_or(to_line.len(), |(offset, _)| offset);
    Some(to_start + within)
}

fn is_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// `\bNAME\b` (ASCII) somewhere in `text`.
fn contains_word(text: &str, name: &str) -> bool {
    word_positions(text, name).next().is_some()
}

/// Start offsets of `name` in `text` with an ASCII word boundary on both sides.
fn word_positions<'a>(text: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    let first_is_word = name.chars().next().is_some_and(is_word);
    let last_is_word = name.chars().next_back().is_some_and(is_word);
    text.match_indices(name).filter_map(move |(at, _)| {
        let before = text[..at].chars().next_back().is_some_and(is_word);
        let after = text[at + name.len()..].chars().next().is_some_and(is_word);
        (before != first_is_word && after != last_is_word).then_some(at)
    })
}

/// `hasParameterBinding`: `name` is an arrow parameter (`name =>`) or appears
/// inside a balanced parameter list that opens a function or arrow body.
pub(crate) fn has_parameter_binding(code: &str, name: &str) -> bool {
    for at in word_positions(code, name) {
        let rest = code[at + name.len()..].trim_start_matches(char::is_whitespace);
        if rest.starts_with("=>") {
            return true;
        }
    }
    static BODY: OnceLock<Regex> = OnceLock::new();
    let body =
        BODY.get_or_init(|| Regex::new(r"^\s*(?::[^=;{]*)?(?:=>|\{)").expect("parameter body"));
    let bytes = code.as_bytes();
    for (open, _) in code.match_indices('(') {
        let head = code[..open].trim_end_matches(char::is_whitespace);
        let keyword = ["if", "while", "for", "switch", "with"]
            .iter()
            .any(|keyword| {
                head.strip_suffix(keyword)
                    .is_some_and(|before| !before.chars().next_back().is_some_and(is_word))
            });
        if keyword {
            continue;
        }
        let mut depth = 1usize;
        let mut close = open + 1;
        while close < bytes.len() && depth > 0 {
            match bytes[close] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            close += 1;
        }
        if depth == 0
            && contains_word(&code[open + 1..close - 1], name)
            && body.is_match(&code[close..])
        {
            return true;
        }
    }
    false
}

/// `\b(?:const|let|var|function|class)\s+(?:NAME\b|\{[^}]*\bNAME\b)` starts.
fn declaration_offsets(code: &str, name: &str) -> Vec<usize> {
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    let keyword = KEYWORD.get_or_init(|| {
        Regex::new(r"(?-u:\b)(?:const|let|var|function|class)\s+").expect("declaration keyword")
    });
    let last_is_word = name.chars().next_back().is_some_and(is_word);
    keyword
        .find_iter(code)
        .filter(|found| {
            let rest = &code[found.end()..];
            let direct = rest.starts_with(name)
                && rest[name.len()..].chars().next().is_some_and(is_word) != last_is_word;
            let destructured = rest.strip_prefix('{').is_some_and(|inner| {
                let inner = inner.split('}').next().unwrap_or(inner);
                contains_word(inner, name)
            });
            direct || destructured
        })
        .map(|found| found.start())
        .collect()
}

/// `\bNAME\s*=(?!=)`: an assignment (not a comparison) to `name`.
fn is_reassigned(code: &str, name: &str) -> bool {
    word_positions(code, name).any(|at| {
        let rest = code[at + name.len()..].trim_start_matches(char::is_whitespace);
        rest.starts_with('=') && !rest[1..].starts_with('=')
    })
}

/// `importShadowedAt`: a parameter of the enclosing function, or a declaration
/// in a scope enclosing the position, rebinds the imported `name`.
fn import_shadowed_at(name: &str, reference: &RefView, context: &dyn ResolutionContext) -> bool {
    let enclosing_parameter = context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .any(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.start_line <= reference.line
                && node.end_line >= reference.line
                && node.signature.as_deref().is_some_and(|signature| {
                    has_parameter_binding(&format!("{signature} {{"), name)
                })
        });
    if enclosing_parameter {
        return true;
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return false;
    };
    let Some(line_index) = (reference.line as usize).checked_sub(1) else {
        return false;
    };
    let lines = facts.lines();
    let Some(line_start) = lines.raw_line_start(line_index) else {
        return false;
    };
    let column =
        (reference.column.max(0) as usize).min(facts.raw_line(line_index).map_or(0, str::len));
    let end = line_start + column;
    if !facts.source().is_char_boundary(end) {
        return false;
    }
    let code = blank_string_contents(&strip_comments_for_regex(
        &facts.source()[..end],
        CommentLang::TypeScript,
    ));
    let stack_at = |end: usize| {
        let mut stack = Vec::new();
        for (offset, byte) in code.bytes().enumerate().take(end) {
            if byte == b'{' {
                stack.push(offset);
            } else if byte == b'}' {
                stack.pop();
            }
        }
        stack
    };
    let scope = stack_at(code.len());
    declaration_offsets(&code, name).into_iter().any(|at| {
        let declared = stack_at(at);
        declared
            .iter()
            .enumerate()
            .all(|(depth, open)| scope.get(depth) == Some(open))
    })
}

/// Infer the project type of an awaited receiver (`inferEsmAwaitedCallType`).
pub(crate) fn infer_awaited_receiver(
    receiver: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> AwaitedInference {
    let plain = receiver
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && receiver.chars().all(|ch| is_word(ch) || ch == '$');
    if !plain {
        return AwaitedInference::NotApplicable;
    }
    let Some(facts) = context.source_facts(&reference.file_path) else {
        return AwaitedInference::NotApplicable;
    };
    // Raw eligibility is cheap; the sanitised index is built only once a
    // reference actually uses an awaited name.
    if !facts
        .awaited_raw_names(raw_awaited_names)
        .contains(receiver)
    {
        return AwaitedInference::NotApplicable;
    }
    let index = facts.awaited_index(|code| Arc::new(AwaitedIndex::new(code)));
    if !index.names.contains(receiver) {
        return AwaitedInference::NotApplicable;
    }
    resolve_awaited(receiver, &facts, &index, reference, context)
}

fn resolve_awaited(
    receiver: &str,
    facts: &SourceFacts,
    index: &AwaitedIndex,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> AwaitedInference {
    let unknown = AwaitedInference::Unresolved;
    let code_full = facts.ts_blanked();
    let Some(line_index) = (reference.line as usize).checked_sub(1) else {
        return AwaitedInference::NotApplicable;
    };
    let Some(end) = map_column(
        facts.source(),
        facts.lines(),
        code_full,
        &index.lines,
        line_index,
        reference.column.max(0) as usize,
    ) else {
        return AwaitedInference::NotApplicable;
    };
    let code = &code_full[..end.min(code_full.len())];
    let Some(&(binding_at, binding_len)) = index.declarations.get(receiver).and_then(|found| {
        found
            .iter()
            .rev()
            .find(|(at, _)| *at < end && index.visible_at(*at, end))
    }) else {
        return AwaitedInference::NotApplicable;
    };
    let init = &code[(binding_at + binding_len).min(code.len())..];
    if !init.starts_with("await") || init[5..].chars().next().is_some_and(is_word) {
        return AwaitedInference::NotApplicable;
    }
    static CALL: OnceLock<Regex> = OnceLock::new();
    let call = CALL.get_or_init(|| {
        Regex::new(r"^await\s+([A-Za-z_$][0-9A-Za-z_$]*)\s*\(").expect("awaited call")
    });
    let Some(captures) = call.captures(init) else {
        return AwaitedInference::NotApplicable;
    };
    let callee = captures
        .get(1)
        .map_or("", |callee| callee.as_str())
        .to_string();
    let mut depth = 1usize;
    let mut call_end = captures.get(0).map_or(0, |whole| whole.end());
    let bytes = init.as_bytes();
    while call_end < bytes.len() && depth > 0 {
        match bytes[call_end] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        call_end += 1;
    }
    if depth != 0 {
        return unknown;
    }
    let tail = &init[call_end..];
    // Only a bare call result: `;` or a line end not continued by a member,
    // index, call or conditional.
    let after_blanks = tail.trim_start_matches([' ', '\t']);
    let bare_result = after_blanks.starts_with(';')
        || after_blanks
            .strip_prefix("\r\n")
            .or_else(|| after_blanks.strip_prefix('\n'))
            .is_some_and(|next| {
                !next
                    .trim_start_matches([' ', '\t'])
                    .starts_with(['.', '(', '[', '?'])
            });
    if !bare_result {
        return unknown;
    }
    if !declaration_offsets(tail, receiver).is_empty()
        || is_reassigned(tail, receiver)
        || has_parameter_binding(tail, receiver)
    {
        return unknown;
    }

    let binding_line_index = code[..binding_at].matches('\n').count();
    let binding_line = binding_line_index as i64 + 1;
    let Some(code_line_start) = index.lines.raw_line_start(binding_line_index) else {
        return unknown;
    };
    let binding_col = map_column(
        code_full,
        &index.lines,
        facts.source(),
        facts.lines(),
        binding_line_index,
        binding_at - code_line_start,
    )
    .and_then(|absolute| {
        facts
            .lines()
            .raw_line_start(binding_line_index)
            .map(|start| absolute - start)
    })
    .unwrap_or(0);
    let mut binding_ref = reference.clone();
    binding_ref.line = binding_line;
    binding_ref.column = binding_col as i64;

    let callee_is_parameter = context
        .get_nodes_in_file_shared(&reference.file_path)
        .iter()
        .any(|node| {
            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.start_line <= binding_line
                && node.end_line >= binding_line
                && node.signature.as_deref().is_some_and(|signature| {
                    has_parameter_binding(&format!("{signature} {{"), &callee)
                })
        });
    if callee_is_parameter {
        return unknown;
    }

    let imported = context
        .get_import_mappings(&reference.file_path, reference.language)
        .iter()
        .any(|mapping| mapping.local_name == callee);
    let declaring: Option<Arc<Node>> = if imported {
        if import_shadowed_at(&callee, &binding_ref, context) {
            return unknown;
        }
        let mut callee_ref = binding_ref.clone();
        callee_ref.reference_name = callee.clone();
        callee_ref.reference_kind = EdgeKind::Calls;
        resolve_via_import(&callee_ref, context)
            .and_then(|resolved| context.get_node_by_id_shared(&resolved.target_node_id))
    } else {
        let local: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(&callee)
            .into_iter()
            .filter(|node| {
                node.kind == NodeKind::Function
                    && node.file_path == reference.file_path
                    && crate::name_matcher::is_esm_module_family(node.language)
                    && crate::name_matcher::is_lexically_reachable(node, &binding_ref, context)
            })
            .collect();
        (local.len() == 1).then(|| Arc::clone(&local[0]))
    };
    let Some(declaring) = declaring else {
        return unknown;
    };
    let Some(signature) = declaring
        .signature
        .as_deref()
        .filter(|_| declaring.kind == NodeKind::Function)
    else {
        return unknown;
    };
    if !imported {
        let before_binding = &code[..binding_at];
        static LOCAL_DECL: OnceLock<Regex> = OnceLock::new();
        let local_decl = LOCAL_DECL.get_or_init(|| {
            Regex::new(r"(?-u:\b)(?:const|let|var)\s+").expect("local declaration")
        });
        for found in local_decl.find_iter(before_binding) {
            let rest = &before_binding[found.end()..];
            let names_callee = rest.starts_with(callee.as_str())
                && rest[callee.len()..].chars().next().is_some_and(is_word)
                    != callee.chars().next_back().is_some_and(is_word);
            if !names_callee || !index.visible_at(found.start(), binding_at) {
                continue;
            }
            // A typed arrow function may itself be the declared local factory.
            let shadow_line_index = code[..found.start()].matches('\n').count();
            let shadow_col = index
                .lines
                .raw_line_start(shadow_line_index)
                .and_then(|start| {
                    map_column(
                        code_full,
                        &index.lines,
                        facts.source(),
                        facts.lines(),
                        shadow_line_index,
                        found.start() - start,
                    )
                })
                .and_then(|absolute| {
                    facts
                        .lines()
                        .raw_line_start(shadow_line_index)
                        .map(|start| absolute - start)
                })
                .unwrap_or(usize::MAX);
            if shadow_line_index as i64 + 1 != declaring.start_line
                || shadow_col as i64 > declaring.start_column
            {
                return unknown;
            }
        }
    }
    static ANNOTATION: OnceLock<Regex> = OnceLock::new();
    let annotation =
        ANNOTATION.get_or_init(|| Regex::new(r"^\s*:\s*([\s\S]+)$").expect("return annotation"));
    let after_params = &signature[signature
        .rfind(')')
        .map_or(signature.len(), |close| close + 1)..];
    let Some(annotation) = annotation
        .captures(after_params)
        .and_then(|captures| captures.get(1))
        .map(|found| found.as_str().trim())
    else {
        return unknown;
    };
    static PROMISE: OnceLock<Regex> = OnceLock::new();
    let promise = PROMISE
        .get_or_init(|| Regex::new(r"^Promise\s*<\s*([0-9A-Za-z_$]+)\s*>$").expect("Promise<T>"));
    let returned = promise
        .captures(annotation)
        .and_then(|captures| captures.get(1))
        .map_or(annotation, |inner| inner.as_str());
    let named = returned
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_' || first == '$')
        && returned.chars().all(|ch| is_word(ch) || ch == '$');
    if !named || TS_PRIMITIVE_TYPES.contains(&returned) {
        // A primitive awaited value is a built-in, never a project method.
        return unknown;
    }
    let mut type_ref = binding_ref.clone();
    type_ref.from_node_id = declaring.id.clone();
    type_ref.file_path = declaring.file_path.clone();
    type_ref.language = declaring.language;
    type_ref.line = declaring.start_line;
    type_ref.column = declaring.start_column;
    type_ref.reference_name = returned.to_string();
    type_ref.reference_kind = EdgeKind::References;
    let type_import = context
        .get_import_mappings(&declaring.file_path, declaring.language)
        .iter()
        .any(|mapping| mapping.local_name == returned);
    let type_node = if type_import {
        resolve_via_import(&type_ref, context)
            .and_then(|resolved| context.get_node_by_id_shared(&resolved.target_node_id))
    } else {
        context
            .get_nodes_by_name_shared(returned)
            .into_iter()
            .find(|node| {
                node.file_path == declaring.file_path
                    && crate::name_matcher::is_esm_module_family(node.language)
                    && matches!(node.kind, NodeKind::Class | NodeKind::Interface)
            })
    };
    match type_node {
        Some(node) if matches!(node.kind, NodeKind::Class | NodeKind::Interface) => {
            AwaitedInference::Inferred {
                type_name: node.name.clone(),
                file_path: node.file_path.clone(),
            }
        }
        _ => unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_binding_matches_upstream_shapes() {
        assert!(has_parameter_binding(
            "(a, handle) => handle.run()",
            "handle"
        ));
        assert!(has_parameter_binding("handle => 1", "handle"));
        assert!(has_parameter_binding(
            "function f(handle: Engine) {",
            "handle"
        ));
        assert!(!has_parameter_binding("if (handle) { run(); }", "handle"));
        assert!(!has_parameter_binding("call(handle);", "handle"));
        assert!(!has_parameter_binding("(handler) => 1", "handle"));
    }

    #[test]
    fn declaration_and_reassignment_are_word_bounded() {
        assert_eq!(declaration_offsets("let handle = 1;", "handle"), vec![0]);
        assert_eq!(
            declaration_offsets("const { a, handle } = o;", "handle"),
            vec![0]
        );
        assert!(declaration_offsets("const handler = 1;", "handle").is_empty());
        assert!(is_reassigned("handle = other;", "handle"));
        assert!(!is_reassigned("if (handle == other) {}", "handle"));
        assert!(!is_reassigned("handler = x;", "handle"));
    }

    #[test]
    fn scope_tree_resolves_nested_visibility() {
        let code = "const a = await f();\n{ const b = await g(); }\nuse(a);\n";
        let index = AwaitedIndex::new(code);
        assert!(index.names.contains("a") && index.names.contains("b"));
        let a = index.declarations["a"][0].0;
        let b = index.declarations["b"][0].0;
        let use_at = code.find("use(a)").unwrap();
        assert!(index.visible_at(a, use_at));
        assert!(
            !index.visible_at(b, use_at),
            "a block-scoped binding is not visible after its block"
        );
    }

    #[test]
    fn map_column_counts_characters_across_blanked_views() {
        let source = "/* é */ x\n";
        let code = strip_comments_for_regex(source, CommentLang::TypeScript);
        let source_lines = LineIndex::new(source);
        let code_lines = LineIndex::new(&code);
        let column = source.find('x').unwrap();
        let mapped = map_column(source, &source_lines, &code, &code_lines, 0, column).unwrap();
        assert_eq!(&code[mapped..mapped + 1], "x");
    }
}
