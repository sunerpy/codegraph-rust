//! The Nix module-system pass (upstream `callback-synthesizer.ts`
//! `nixOptionPathEdges`, #1190): an option is declared in one module
//! (`options.launchd.user.agents = mkOption { … }`) and set in others
//! (`launchd.user.agents.yabai = { … }`), and the module system joins them
//! by path at evaluation, so no static edge leads from a write to its
//! declaration. This pass links each write to the declaration whose path is
//! the longest static prefix of the write's (`nix-option-path`). It runs
//! when the project has Nix files.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, NodeKind};
use serde_json::json;

use super::stage::View;

/// Whether the project has Nix files, which the pass reads.
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    languages.contains(&Language::Nix)
}

/// A binding's place and its path's leading static segments.
#[derive(Clone)]
struct Binding {
    id: String,
    file_path: String,
    start_line: i64,
    end_line: i64,
    segments: Vec<String>,
}

/// The segment upstream marks a submodule's own `options` with: a path
/// holding it is never registered.
const SUBMODULE: &str = "\u{0}submodule";

/// The longest matched prefix a write is tried with, in segments.
const MAX_PREFIX: usize = 6;

/// Whether `segment` is a plain Nix identifier segment (upstream's
/// `/^[A-Za-z_][A-Za-z0-9_'-]*$/`).
fn is_plain_segment(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'' | b'-'))
}

/// The leading static segments of a binding's attribute path (upstream
/// `nixLeadingPlainSegments`): plain identifiers, and quoted segments kept
/// verbatim with their quotes, as opaque tokens that match only themselves.
/// An interpolation (`${name}`), an unterminated quote or anything else ends
/// the static head.
fn leading_static_segments(name: &str) -> Vec<String> {
    let bytes = name.as_bytes();
    let n = bytes.len();
    let mut segments = Vec::new();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < n && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if j >= n {
                return segments;
            }
            let token = &name[i..=j];
            if token.contains("${") {
                return segments;
            }
            segments.push(token.to_string());
            i = j + 1;
            if i >= n {
                break;
            }
            if bytes[i] != b'.' {
                return segments;
            }
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && bytes[j] != b'.' {
            if bytes[j] == b'"' || (bytes[j] == b'$' && bytes.get(j + 1) == Some(&b'{')) {
                return segments;
            }
            j += 1;
        }
        let segment = &name[i..j];
        if !is_plain_segment(segment) {
            return segments;
        }
        segments.push(segment.to_string());
        i = j + 1;
    }
    segments
}

/// Each Nix variable's and function's binding with a static head, by file in
/// the order the files first appear (upstream's `byFile`: the variables,
/// then the functions, each in node order).
fn bindings_by_file(view: &View<'_>) -> Result<Vec<Vec<Binding>>> {
    let mut order: Vec<String> = Vec::new();
    let mut by_file: HashMap<String, Vec<Binding>> = HashMap::new();
    for kind in [NodeKind::Variable, NodeKind::Function] {
        for node in view.nodes_of_kinds_in(&[kind], &[Language::Nix])? {
            let segments = leading_static_segments(&node.name);
            if segments.is_empty() {
                continue;
            }
            if !by_file.contains_key(&node.file_path) {
                order.push(node.file_path.clone());
            }
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(Binding {
                    id: node.id,
                    file_path: node.file_path,
                    start_line: node.start_line,
                    end_line: node.end_line,
                    segments,
                });
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|file| by_file.remove(&file))
        .collect())
}

/// An option span on the walk's stack: its lines and the path it declares
/// below it.
struct Span {
    start: i64,
    end: i64,
    prefix: Vec<String>,
}

/// Nix option wiring (`nixOptionPathEdges`): each config write reaches the
/// declaration of the longest prefix of its path, two to six static
/// segments, that one file declares. Declarations register flat
/// (`options.a.b = …`) by name and nested (`options = { a.b = …; }`) by line
/// span; a submodule's own `options` register nothing; writes inside an
/// option span are its internals, no writes; and a longest prefix that two
/// files declare links nothing rather than a shorter one.
pub(super) fn option_path_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut declarations: HashMap<String, Vec<Binding>> = HashMap::new();
    let mut writes: Vec<Binding> = Vec::new();
    for mut bindings in bindings_by_file(view)? {
        // Outermost first: by start line, the longer span first on a tie;
        // the sort is stable, as upstream's.
        bindings.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then(b.end_line.cmp(&a.end_line))
        });
        let mut stack: Vec<Span> = Vec::new();
        for binding in bindings {
            while stack
                .last()
                .is_some_and(|span| span.end < binding.start_line)
            {
                stack.pop();
            }
            // Strict containment at line granularity: a one-line nested
            // binding spans what its container does, so it is no member.
            let enclosing = stack.last().filter(|span| {
                binding.start_line >= span.start
                    && binding.end_line <= span.end
                    && !(binding.start_line == span.start && binding.end_line == span.end)
            });
            let register = |path: &[String], declarations: &mut HashMap<String, Vec<Binding>>| {
                if path.len() < 2 || path.iter().any(|segment| segment == SUBMODULE) {
                    return;
                }
                declarations
                    .entry(path.join("."))
                    .or_default()
                    .push(binding.clone());
            };
            if binding.segments[0] == "options" {
                let prefix = if enclosing.is_some() {
                    vec![SUBMODULE.to_string()]
                } else {
                    binding.segments[1..].to_vec()
                };
                register(&prefix, &mut declarations);
                stack.push(Span {
                    start: binding.start_line,
                    end: binding.end_line,
                    prefix,
                });
                continue;
            }
            if let Some(enclosing) = enclosing {
                let mut composed = enclosing.prefix.clone();
                composed.extend(binding.segments.iter().cloned());
                register(&composed, &mut declarations);
                stack.push(Span {
                    start: binding.start_line,
                    end: binding.end_line,
                    prefix: composed,
                });
                continue;
            }
            if binding.segments.len() >= 2 {
                writes.push(binding);
            }
        }
    }
    if declarations.is_empty() || writes.is_empty() {
        return Ok(Vec::new());
    }
    let mut edges = Vec::new();
    for write in &writes {
        // `config.services.x = …` writes the same path with an explicit head.
        let segments = if write.segments[0] == "config" {
            &write.segments[1..]
        } else {
            &write.segments[..]
        };
        if segments.len() < 2 {
            continue;
        }
        for len in (2..=segments.len().min(MAX_PREFIX)).rev() {
            let path = segments[..len].join(".");
            let Some(candidates) = declarations.get(&path).filter(|found| !found.is_empty()) else {
                continue;
            };
            let files: HashSet<&str> = candidates
                .iter()
                .map(|candidate| candidate.file_path.as_str())
                .collect();
            if files.len() == 1 {
                let target = &candidates[0];
                if target.id != write.id {
                    edges.push(Edge {
                        id: None,
                        source: write.id.clone(),
                        target: target.id.clone(),
                        kind: EdgeKind::References,
                        metadata: Some(json!({
                            "synthesizedBy": "nix-option-path",
                            "optionPath": path,
                            "registeredAt": format!("{}:{}", target.file_path, target.start_line),
                        })),
                        line: Some(write.start_line),
                        col: None,
                        provenance: Some("heuristic".to_string()),
                    });
                }
            }
            // The longest declared prefix decides, matched or ambiguous.
            break;
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_heads_stop_at_an_interpolation_or_a_bad_segment() {
        let heads = |name: &str| leading_static_segments(name);
        assert_eq!(heads("launchd.user.agents"), ["launchd", "user", "agents"]);
        assert_eq!(
            heads(r#"xdg.configFile."git/config".text"#),
            ["xdg", "configFile", "\"git/config\"", "text"]
        );
        assert_eq!(heads("services.${name}.enable"), ["services"]);
        assert_eq!(heads(r#"a."${b}".c"#), ["a"]);
        assert_eq!(heads(r#"a."unterminated"#), ["a"]);
        assert_eq!(heads(r#"a."q"x"#), ["a", "\"q\""]);
        assert_eq!(heads("pkgs.foo-bar.baz'"), ["pkgs", "foo-bar", "baz'"]);
        assert_eq!(heads("1bad.x"), Vec::<String>::new());
        assert_eq!(heads(r#"a."x\"y".b"#), ["a", "\"x\\\"y\"", "b"]);
    }
}
