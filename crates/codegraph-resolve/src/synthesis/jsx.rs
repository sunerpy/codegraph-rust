//! The JSX render pass (upstream `reactJsxChildEdges` at `e8ddc4e2`, built
//! on the base graph as OD-A4 decided): a function, method or component
//! that renders `<Child />` links to what the tag's static `references` edge
//! reaches (`jsx-render`). The tags and their targets are the base graph's;
//! only a tag's position is checked against the file text.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use serde_json::json;

use super::dispatch::synthesized;
use super::stage::View;
use crate::frameworks::js_scan::is_js_space;
use crate::name_matcher::crosses_code_boundary;

/// The children one parent links at most (upstream `MAX_JSX_CHILDREN`).
const MAX_CHILDREN: usize = 30;
/// The kinds that render the tags their whole line range holds.
const PARENT_KINDS: [NodeKind; 3] = [NodeKind::Function, NodeKind::Method, NodeKind::Component];
/// The kinds of a top-level value that renders the tags it owns.
const VALUE_KINDS: [NodeKind; 2] = [NodeKind::Constant, NodeKind::Variable];
/// The kinds a tag renders: upstream's components, functions and classes,
/// and the values it reads as components. Not a method, which a tag's
/// static reference reaches only through a name match.
const CHILD_KINDS: [NodeKind; 5] = [
    NodeKind::Component,
    NodeKind::Function,
    NodeKind::Class,
    NodeKind::Constant,
    NodeKind::Variable,
];

/// Upstream's gate (`has(...JS_FAMILY)`): the index has JavaScript or
/// TypeScript files.
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    [
        Language::TypeScript,
        Language::JavaScript,
        Language::Tsx,
        Language::Jsx,
    ]
    .iter()
    .any(|language| languages.contains(language))
}

/// One JSX tag: where it is, its name, the node its static reference comes
/// from and the node that reference reaches.
struct Tag {
    line: i64,
    col: i64,
    name: String,
    source: String,
    target: String,
}

/// JSX children (`reactJsxChildEdges`): each function, method or component
/// links to what the tags in its line range render, a top-level value or
/// the file to what the tags it owns render, at most [`MAX_CHILDREN`] a
/// parent.
pub(super) fn jsx_render_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut targets: HashMap<String, Option<Node>> = HashMap::new();
    // Only tsx and jsx files have JSX tags (`extract_jsx_component_ref`).
    for file in view.files_of_languages(&[Language::Tsx, Language::Jsx])? {
        let nodes = view.nodes_in_file(&file)?;
        let Some(content) = view.text(&file) else {
            continue;
        };
        let starts = line_starts(&content);
        let mut tags = Vec::new();
        for node in &nodes {
            for edge in view.edges_from(&node.id, EdgeKind::References)? {
                let (Some(line), Some(col)) = (edge.line, edge.col) else {
                    continue;
                };
                if let Some(name) = tag_at(&content, &starts, line, col) {
                    tags.push(Tag {
                        line,
                        col,
                        name: name.to_string(),
                        source: node.id.clone(),
                        target: edge.target.clone(),
                    });
                }
            }
        }
        if tags.is_empty() {
            continue;
        }
        tags.sort_by_key(|tag| (tag.line, tag.col));

        // A line a function, method, component or class spans is theirs;
        // the others belong to the value or the file the syntax tree gives
        // them to.
        let held = |line: i64| {
            nodes.iter().any(|node| {
                (PARENT_KINDS.contains(&node.kind) || node.kind == NodeKind::Class)
                    && node.start_line <= line
                    && line <= node.end_line
            })
        };
        let mut parents: Vec<(&Node, Vec<&Tag>)> = Vec::new();
        for node in nodes
            .iter()
            .filter(|node| PARENT_KINDS.contains(&node.kind))
        {
            let spanned = tags
                .iter()
                .filter(|tag| node.start_line <= tag.line && tag.line <= node.end_line)
                .collect();
            parents.push((node, spanned));
        }
        for node in nodes.iter().filter(|node| VALUE_KINDS.contains(&node.kind)) {
            parents.push((node, owned(&tags, node, &held)));
        }
        if let Some(node) = nodes.iter().find(|node| node.kind == NodeKind::File) {
            parents.push((node, owned(&tags, node, &held)));
        }

        for (parent, tags) in parents {
            let mut added = 0;
            let mut names: HashSet<&str> = HashSet::new();
            for tag in tags {
                if added >= MAX_CHILDREN {
                    break;
                }
                if !names.insert(&tag.name) {
                    continue;
                }
                if !targets.contains_key(&tag.target) {
                    targets.insert(tag.target.clone(), view.node(&tag.target)?);
                }
                let Some(child) = targets[&tag.target].as_ref() else {
                    continue;
                };
                if !CHILD_KINDS.contains(&child.kind)
                    || child.id == parent.id
                    || crosses_code_boundary(parent.language, child.language)
                    || !seen.insert((parent.id.clone(), child.id.clone()))
                {
                    continue;
                }
                edges.push(synthesized(
                    parent,
                    child,
                    EdgeKind::Calls,
                    parent.start_line,
                    json!({"synthesizedBy": "jsx-render", "via": tag.name}),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// The tags `owner` (a top-level value or the file) holds: those on lines no
/// function, method, component or class spans whose reference it makes.
fn owned<'t>(tags: &'t [Tag], owner: &Node, held: &impl Fn(i64) -> bool) -> Vec<&'t Tag> {
    tags.iter()
        .filter(|tag| tag.source == owner.id && !held(tag.line))
        .collect()
}

/// The byte offset each line of `content` starts at.
fn line_starts(content: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            content
                .bytes()
                .enumerate()
                .filter(|&(_, byte)| byte == b'\n')
                .map(|(at, _)| at + 1),
        )
        .collect()
}

/// The JSX tag name a reference at `line` and byte column `col` starts, as
/// upstream's `JSX_TAG_RE` reads one: right after a `<`, an ASCII capital
/// and then letters, digits or `_`, followed by whitespace, `/`, `>` or `<`
/// (so a member tag `<Foo.Bar>` is none). A `<` right after an identifier
/// or a `.` opens a type argument (`Array<Box>`), not a tag.
fn tag_at<'t>(content: &'t str, starts: &[usize], line: i64, col: i64) -> Option<&'t str> {
    let start = *starts.get(usize::try_from(line - 1).ok()?)?;
    let at = start.checked_add(usize::try_from(col).ok()?)?;
    let bytes = content.as_bytes();
    if at == 0 || at >= bytes.len() || bytes[at - 1] != b'<' {
        return None;
    }
    if at >= 2
        && (bytes[at - 2].is_ascii_alphanumeric() || matches!(bytes[at - 2], b'_' | b'$' | b'.'))
    {
        return None;
    }
    let rest = &content[at..];
    if !rest.as_bytes()[0].is_ascii_uppercase() {
        return None;
    }
    let len = rest
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    let next = rest[len..].chars().next()?;
    (is_js_space(next) || matches!(next, '/' | '>' | '<')).then(|| &rest[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(content: &str, line: i64, col: i64) -> Option<&str> {
        tag_at(content, &line_starts(content), line, col)
    }

    #[test]
    fn a_tag_is_a_capital_name_right_after_a_lone_angle_bracket() {
        assert_eq!(at("x\n  return <Card />;", 2, 10), Some("Card"));
        assert_eq!(at("(<Row>x</Row>)", 1, 2), Some("Row"));
        assert_eq!(at("<A\u{FEFF}b={1}/>", 1, 1), Some("A"));
        assert_eq!(at("<Card\n/>", 1, 1), Some("Card"));
        // lowercase, member, type argument, generic call, no `<`
        assert_eq!(at("<div />", 1, 1), None);
        assert_eq!(at("<Foo.Bar />", 1, 1), None);
        assert_eq!(at("Array<Box> = []", 1, 6), None);
        assert_eq!(at("foo<Box>(1)", 1, 4), None);
        assert_eq!(at("x.y<Box>", 1, 4), None);
        assert_eq!(at(" Card />", 1, 1), None);
        assert_eq!(at("<Card", 1, 1), None);
    }

    #[test]
    fn columns_are_bytes_of_the_line() {
        // `é` is two bytes.
        assert_eq!(at("é <Card />", 1, 4), Some("Card"));
        assert_eq!(at("é <Card />", 1, 3), None);
    }
}
