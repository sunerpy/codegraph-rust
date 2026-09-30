//! Own properties of a JS-family object literal (upstream v1.6.1 #1932).
//!
//! `export const api = { getUser, rename: fetchUser, inline() {} }` is the
//! usual way an API module assembles a namespace. Resolution needs to know,
//! for one member name, which own property of the literal defines it — only
//! the literal's top-level members count, the last one wins, and an unknown
//! spread or computed key invalidates earlier evidence — and, when that
//! property is a shorthand or an identifier-valued pair, which binding it
//! names. The binding is then followed lexically from where the literal is
//! written: a visible declaration of the file first, else one of its imports,
//! and never through a shadowing parameter or nearer value.

use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Node, NodeKind};
use regex::Regex;

use crate::awaited::has_parameter_binding;
use crate::import_resolver::resolve_via_import;
use crate::name_matcher::range_within;
use crate::source_facts::SourceFacts;
use crate::strip_comments::{CommentLang, blank_string_contents, strip_comments_for_regex};
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// What a literal says about one member name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralLookup {
    /// The holder's source is not a readable object literal.
    Unknown,
    /// No own property defines the member (or a later spread may).
    Absent,
    /// The own property that defines the member.
    Property(LiteralProperty),
}

/// One own property: its character span within the holder's extent, and the
/// identifier it binds when it is `{ name }` or `{ key: name }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralProperty {
    pub binding: Option<String>,
    start: usize,
    end: usize,
}

/// The own property of `container`'s object literal that defines `member`.
pub(crate) fn object_literal_property(
    container: &Node,
    member: &str,
    context: &dyn ResolutionContext,
) -> LiteralLookup {
    let Some(facts) = context.source_facts(&container.file_path) else {
        return LiteralLookup::Unknown;
    };
    facts.literal_property(&container.id, member, |facts| {
        read_literal_property(facts, container, member)
    })
}

/// Whether `node` starts inside `property` of `container`'s literal.
pub(crate) fn property_contains(
    property: &LiteralProperty,
    container: &Node,
    node: &Node,
    context: &dyn ResolutionContext,
) -> bool {
    let Some(facts) = context.source_facts(&container.file_path) else {
        return false;
    };
    let (Some(from), Some(at)) = (
        byte_position(&facts, container.start_line, container.start_column),
        byte_position(&facts, node.start_line, node.start_column),
    ) else {
        return false;
    };
    let source = facts.source();
    if at < from || !source.is_char_boundary(from) || !source.is_char_boundary(at) {
        return false;
    }
    let offset = source[from..at].chars().count();
    offset >= property.start && offset < property.end
}

/// Follow the binding `member` names in `container`'s literal (#1932): a
/// lexically visible declaration of the literal's file, else one of its
/// imports. A shadowing parameter or nearer non-callable value leaves it
/// unresolved.
pub(crate) fn resolve_object_literal_binding(
    container: &Node,
    member: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let LiteralLookup::Property(property) = object_literal_property(container, member, context)
    else {
        return None;
    };
    let binding = property.binding?;
    let calls = reference.reference_kind == EdgeKind::Calls;
    let facts = context.source_facts(&container.file_path)?;
    let key = format!("{member}\0{}", u8::from(calls));
    let target = facts.literal_binding_target(&container.id, &key, |facts| {
        binding_target(facts, container, &binding, calls, reference, context)
    })?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: target,
        confidence: 0.85,
        resolved_by: ResolvedBy::InstanceMethod,
    })
}

fn accepts(node: &Node, calls: bool) -> bool {
    matches!(
        node.kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Class
    ) || (!calls
        && matches!(
            node.kind,
            NodeKind::Constant | NodeKind::Variable | NodeKind::Component
        ))
}

fn binding_target(
    facts: &SourceFacts,
    container: &Node,
    binding: &str,
    calls: bool,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let in_file = context.get_nodes_in_file_shared(&container.file_path);
    // A parameter of a function enclosing the literal rebinds the name.
    if in_file.iter().any(|node| {
        matches!(node.kind, NodeKind::Function | NodeKind::Method)
            && range_within(container, node)
            && node
                .signature
                .as_deref()
                .is_some_and(|signature| has_parameter_binding(&format!("{signature} {{"), binding))
    }) {
        return None;
    }
    let scope = brace_scope(facts, container)?;
    // Select the lexical binding BEFORE checking callability: a nearer value
    // shadows an outer function even if that value cannot be called.
    let nearest = in_file
        .iter()
        .filter(|node| {
            node.name == binding
                && node.id != container.id
                && matches!(
                    node.kind,
                    NodeKind::Function
                        | NodeKind::Class
                        | NodeKind::Constant
                        | NodeKind::Variable
                        | NodeKind::Component
                )
        })
        .filter_map(|node| {
            let declared = brace_scope(facts, node)?;
            declared
                .iter()
                .enumerate()
                .all(|(depth, open)| scope.get(depth) == Some(open))
                .then_some((declared.len(), node))
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| right.1.start_line.cmp(&left.1.start_line))
                .then_with(|| right.1.start_column.cmp(&left.1.start_column))
        })
        .map(|(_, node)| node);
    if let Some(local) = nearest {
        return accepts(local, calls).then(|| local.id.clone());
    }
    let mut at = reference.clone();
    at.reference_name = binding.to_string();
    at.file_path = container.file_path.clone();
    at.language = container.language;
    at.from_node_id = container.id.clone();
    at.line = container.start_line;
    at.column = container.start_column;
    let imported = resolve_via_import(&at, context)?;
    let target = context.get_node_by_id_shared(&imported.target_node_id)?;
    accepts(&target, calls).then(|| target.id.clone())
}

/// Byte offset of a 1-based line and byte column in the file.
fn byte_position(facts: &SourceFacts, line: i64, column: i64) -> Option<usize> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    let start = facts.lines().raw_line_start(index)?;
    let length = facts.raw_line(index).map_or(0, str::len);
    Some(start + usize::try_from(column).ok()?.min(length))
}

/// Offsets of the `{` open where `node` starts, in the comment-free and
/// string-blanked whole file.
fn brace_scope(facts: &SourceFacts, node: &Node) -> Option<Vec<usize>> {
    let end = byte_position(facts, node.start_line, node.start_column)?;
    let mut stack = Vec::new();
    for ((offset, _), blanked) in facts
        .source()
        .char_indices()
        .zip(facts.ts_blanked().chars())
    {
        if offset >= end {
            break;
        }
        match blanked {
            '{' => stack.push(offset),
            '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    Some(stack)
}

fn read_literal_property(facts: &SourceFacts, container: &Node, member: &str) -> LiteralLookup {
    let (Some(from), Some(to)) = (
        byte_position(facts, container.start_line, container.start_column),
        byte_position(facts, container.end_line, container.end_column),
    ) else {
        return LiteralLookup::Unknown;
    };
    let source = facts.source();
    if from >= to || !source.is_char_boundary(from) || !source.is_char_boundary(to) {
        return LiteralLookup::Unknown;
    }
    let extent = strip_comments_for_regex(&source[from..to], CommentLang::TypeScript);
    let code = blank_string_contents(&extent);
    // Start at THIS declarator, never a sibling on the same line; a literal
    // handed to `Object.freeze`/`Object.seal` or parenthesized still counts.
    static OPEN: OnceLock<Regex> = OnceLock::new();
    let open = OPEN.get_or_init(|| {
        Regex::new(r"^[^=]*=\s*(?:(?:Object\.(?:freeze|seal)\s*)?\(\s*)*\{")
            .expect("object literal opening pattern")
    });
    let Some(opening) = open.find(&code) else {
        return LiteralLookup::Unknown;
    };
    let code = code.chars().collect::<Vec<_>>();
    let extent = extent.chars().collect::<Vec<_>>();
    let mut members = Vec::new();
    let mut depth = 0usize;
    let mut start = opening.as_str().chars().count();
    let mut index = start;
    while index < code.len() {
        match code[index] {
            '{' | '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '}' if depth == 0 => {
                members.push((start, index));
                break;
            }
            '}' => depth -= 1,
            ',' if depth == 0 => {
                members.push((start, index));
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }

    let mut selected = None;
    for (start, end) in members {
        let raw = extent[start..end].iter().collect::<String>();
        let text = raw.trim();
        if text.starts_with("...") || text.starts_with('[') {
            selected = None;
            continue;
        }
        let Some((key, key_len)) = property_key(text) else {
            continue;
        };
        if key != member {
            continue;
        }
        let value = text[key_len..].trim();
        let binding = if value.is_empty() {
            Some(member.to_string())
        } else {
            value
                .strip_prefix(':')
                .map(str::trim_start)
                .filter(|name| is_identifier(name))
                .map(str::to_string)
        };
        selected = Some(LiteralProperty {
            binding,
            start,
            end,
        });
    }
    selected.map_or(LiteralLookup::Absent, LiteralLookup::Property)
}

/// A property's key and the byte length of the text that spells it:
/// `^(?:(?:async|get|set)\s+)?\*?\s*(?:ident|'key'|"key")(?=\s*(?:[:(<,=]|$))`.
fn property_key(text: &str) -> Option<(String, usize)> {
    fn key_at(text: &str, from: usize) -> Option<(String, usize)> {
        let rest = text[from..].trim_start();
        let star = rest.strip_prefix('*').unwrap_or(rest);
        let spaced = star.trim_start();
        let offset = text.len() - spaced.len();
        let (key, len) = if let Some(quoted) = spaced.strip_prefix(['\'', '"']) {
            let close = quoted.find(['\'', '"', '\\'])?;
            if quoted[close..].starts_with('\\') {
                return None;
            }
            (quoted[..close].to_string(), close + 2)
        } else {
            let len = spaced
                .char_indices()
                .take_while(|(at, ch)| {
                    if *at == 0 {
                        ch.is_ascii_alphabetic() || matches!(ch, '_' | '$')
                    } else {
                        ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$')
                    }
                })
                .count();
            if len == 0 {
                return None;
            }
            (spaced[..len].to_string(), len)
        };
        let after = spaced[len..].trim_start();
        (after.is_empty() || after.starts_with([':', '(', '<', ',', '=']))
            .then_some((key, offset + len))
    }
    for prefix in ["async", "get", "set"] {
        if let Some(rest) = text.strip_prefix(prefix)
            && rest.starts_with(char::is_whitespace)
            && let Some(found) = key_at(text, prefix.len())
        {
            return Some(found);
        }
    }
    key_at(text, 0)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || matches!(ch, '_' | '$'))
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn property_keys_follow_the_upstream_key_grammar() {
        assert_eq!(property_key("run: right"), Some(("run".into(), 3)));
        assert_eq!(property_key("run"), Some(("run".into(), 3)));
        assert_eq!(property_key("async run() {}"), Some(("run".into(), 9)));
        assert_eq!(property_key("get value() {}"), Some(("value".into(), 9)));
        assert_eq!(property_key("get: fn"), Some(("get".into(), 3)));
        assert_eq!(property_key("get () {}"), Some(("get".into(), 3)));
        assert_eq!(property_key("*gen() {}"), Some(("gen".into(), 4)));
        assert_eq!(property_key("async *gen() {}"), Some(("gen".into(), 10)));
        assert_eq!(property_key("'wrong': 0"), Some(("wrong".into(), 7)));
        assert_eq!(property_key("foo bar"), None);
        assert_eq!(property_key("1: x"), None);
    }
}
