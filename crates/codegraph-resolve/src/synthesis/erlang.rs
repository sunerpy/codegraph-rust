//! The Erlang behaviour pass (upstream `callback-synthesizer.ts`
//! `erlangBehaviourDispatchEdges`, #1169): a behaviour module declares
//! `-callback f/N`, its implementers declare `-behaviour(B)` and export
//! `f/N`, and the framework calls through a variable module
//! (`Mod:handle_thing(Arg)`), a hop extraction leaves silent. Each such site
//! reaches every in-repo implementer of the one behaviour declaring the
//! site's name and arity (`erlang-behaviour`). It runs when the project has
//! Erlang files.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::generated_header::is_js_whitespace;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::json;

use super::dispatch::synthesized;
use super::stage::{LineAt, View, enclosing_fn};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};

/// Whether the project has Erlang files, which the pass reads.
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    languages.contains(&Language::Erlang)
}

/// The implementers of one callback a site links at most: above it the site
/// stays dynamic (upstream `ERLANG_BEHAVIOUR_FANOUT_CAP`).
const FANOUT_CAP: usize = 24;

/// The UTF-16 units an argument list is read over (upstream
/// `erlangArityAt`'s `openIdx + 4000`).
const ARITY_WINDOW: usize = 4000;

/// A `-callback name(` declaration (upstream `ERLANG_CALLBACK_DECL_RE`).
static CALLBACK_DECL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\n)\s*-callback\s+('[^'\n]+'|[a-z][A-Za-z0-9_@]*)\s*\(")
        .expect("callback declaration")
});

/// A call through a variable module, `Var:fn(` (upstream
/// `ERLANG_DISPATCH_RE`, ASCII `\w`): never `?MODULE:fn(`, a static
/// `a:b(`, or a match inside a word.
static DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^?A-Za-z0-9_@'])([A-Z][A-Za-z0-9_@]*):([a-z][A-Za-z0-9_@]*)\(")
        .expect("dispatch")
});

/// A variable-module call's shape anywhere, the cheap file gate.
static VARIABLE_MODULE_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Z][A-Za-z0-9_@]*:[a-z]").expect("variable module call"));

/// Whether `file` is an Erlang module or header (upstream `ERLANG_EXT`).
fn is_erlang_source(file: &str) -> bool {
    file.ends_with(".erl") || file.ends_with(".hrl")
}

/// The argument count of the list whose `(` is at byte `open` of `src`
/// (upstream `erlangArityAt`): top-level commas plus one, `()` none; the
/// nested brackets, `<<…>>` binaries (whose commas separate elements),
/// strings, quoted atoms and `$c` characters inside count no comma. `None`
/// when the list does not close within [`ARITY_WINDOW`] UTF-16 units.
fn arity_at(src: &str, open: usize) -> Option<usize> {
    let chars: Vec<char> = src[open + 1..].chars().collect();
    // Each character's UTF-16 offset from the one after `(`.
    let mut offsets = Vec::with_capacity(chars.len());
    let mut units = 0usize;
    for c in &chars {
        offsets.push(units);
        units += c.len_utf16();
    }
    let within = |k: usize| k < chars.len() && offsets[k] + 1 < ARITY_WINDOW;
    let (mut depth, mut binary_depth, mut commas, mut saw_argument) = (1usize, 0usize, 0, false);
    let mut k = 0;
    while within(k) {
        let c = chars[k];
        if c == '"' || c == '\'' {
            k += 1;
            while within(k) && chars[k] != c {
                if chars[k] == '\\' {
                    k += 1;
                }
                k += 1;
            }
            saw_argument = true;
            k += 1;
            continue;
        }
        if c == '$' {
            k += 1;
            if within(k) && chars[k] == '\\' {
                k += 1;
            }
            saw_argument = true;
            k += 1;
            continue;
        }
        if c == '<' && chars.get(k + 1) == Some(&'<') {
            binary_depth += 1;
            saw_argument = true;
            k += 2;
            continue;
        }
        if c == '>' && chars.get(k + 1) == Some(&'>') && binary_depth > 0 {
            binary_depth -= 1;
            k += 2;
            continue;
        }
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                saw_argument = true;
            }
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(if saw_argument { commas + 1 } else { 0 });
                }
            }
            ',' if depth == 1 && binary_depth == 0 => commas += 1,
            _ if !is_js_whitespace(c) => saw_argument = true,
            _ => {}
        }
        k += 1;
    }
    None
}

/// The arity a function's qualified name ends with (`mod::f/2` → 2).
fn qualified_arity(qualified_name: &str) -> Option<usize> {
    let (_, arity) = qualified_name.rsplit_once('/')?;
    if arity.is_empty() || arity.len() > 3 || !arity.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    arity.parse().ok()
}

/// Erlang behaviour dispatch (`erlangBehaviourDispatchEdges`): each
/// `Var:fn(args)` site whose name and arity exactly one in-repo behaviour
/// declares reaches each implementer's exported `fn` of that arity, at most
/// [`FANOUT_CAP`]; a name and arity two behaviours declare links nothing.
pub(super) fn behaviour_dispatch_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let modules = view.nodes_of_kinds_in(&[NodeKind::Namespace], &[Language::Erlang])?;
    if modules.is_empty() {
        return Ok(Vec::new());
    }
    let mut module_by_file: HashMap<&str, &Node> = HashMap::new();
    for module in &modules {
        module_by_file
            .entry(module.file_path.as_str())
            .or_insert(module);
    }
    let files = view.files()?;

    // Pass 1: every behaviour's callbacks, by `name/arity`.
    let mut declaring: HashMap<String, Vec<&Node>> = HashMap::new();
    let mut callback_names: HashSet<String> = HashSet::new();
    for file in &files {
        if !is_erlang_source(file) {
            continue;
        }
        let Some(behaviour) = module_by_file.get(file.as_str()).copied() else {
            continue;
        };
        let Some(content) = view.text(file) else {
            continue;
        };
        if !content.contains("-callback") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Erlang);
        for captures in CALLBACK_DECL.captures_iter(&safe) {
            let whole = captures.get(0).expect("the whole match");
            let raw = &captures[1];
            let name = raw.strip_prefix('\'').unwrap_or(raw);
            let name = name.strip_suffix('\'').unwrap_or(name);
            let Some(arity) = arity_at(&safe, whole.end() - 1) else {
                continue;
            };
            let declared = declaring.entry(format!("{name}/{arity}")).or_default();
            if !declared.iter().any(|known| known.id == behaviour.id) {
                declared.push(behaviour);
            }
            callback_names.insert(name.to_string());
        }
    }
    if declaring.is_empty() {
        return Ok(Vec::new());
    }

    // Each behaviour's implementers' exported functions of a name and
    // arity, read once.
    let mut targets_by_callback: HashMap<(String, String, usize), Vec<Node>> = HashMap::new();
    let mut targets_of = |behaviour: &Node, name: &str, arity: usize| -> Result<Vec<Node>> {
        let key = (behaviour.id.clone(), name.to_string(), arity);
        if let Some(found) = targets_by_callback.get(&key) {
            return Ok(found.clone());
        }
        let mut found = Vec::new();
        for edge in view.edges_into(&behaviour.id, EdgeKind::Implements)? {
            let Some(implementer) = view.node(&edge.source)? else {
                continue;
            };
            if implementer.language != Language::Erlang || implementer.kind != NodeKind::Namespace {
                continue;
            }
            if let Some(function) = view
                .nodes_in_file(&implementer.file_path)?
                .into_iter()
                .find(|node| {
                    node.kind == NodeKind::Function
                        && node.name == name
                        && qualified_arity(&node.qualified_name) == Some(arity)
                        && node.is_exported
                })
            {
                found.push(function);
            }
        }
        targets_by_callback.insert(key, found.clone());
        Ok(found)
    };

    // Pass 2: the dispatch sites.
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in &files {
        if !is_erlang_source(file) {
            continue;
        }
        let Some(content) = view.text(file) else {
            continue;
        };
        if !VARIABLE_MODULE_CALL.is_match(&content) {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Erlang);
        let nodes = view.nodes_in_file(file)?;
        let lines = LineAt::new(&safe, 1);
        for captures in DISPATCH.captures_iter(&safe) {
            let whole = captures.get(0).expect("the whole match");
            let name = &captures[2];
            if !callback_names.contains(name) {
                continue;
            }
            let Some(arity) = arity_at(&safe, whole.end() - 1) else {
                continue;
            };
            let Some(&[behaviour]) = declaring.get(&format!("{name}/{arity}")).map(Vec::as_slice)
            else {
                continue; // undeclared, or two behaviours declare it
            };
            let targets = targets_of(behaviour, name, arity)?;
            if targets.is_empty() || targets.len() > FANOUT_CAP {
                continue;
            }
            let line = lines.line(whole.start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            for target in &targets {
                if target.id == dispatcher.id
                    || !seen.insert((dispatcher.id.clone(), target.id.clone()))
                {
                    continue;
                }
                edges.push(synthesized(
                    dispatcher,
                    target,
                    EdgeKind::Calls,
                    line,
                    json!({
                        "synthesizedBy": "erlang-behaviour",
                        "via": format!("{}:{name}/{arity}", behaviour.name),
                        "registeredAt": format!("{file}:{line}"),
                    }),
                ));
            }
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arity_skips_nested_lists_binaries_strings_atoms_and_characters() {
        let at = |src: &str| arity_at(src, src.find('(').expect("an open paren"));
        assert_eq!(at("f()"), Some(0));
        assert_eq!(at("f(A)"), Some(1));
        assert_eq!(at("f(A, B)"), Some(2));
        assert_eq!(at("f(<<1,2,3>>, [])"), Some(2));
        assert_eq!(at("f(fun((a, b) -> ok), #{k => v})"), Some(2));
        assert_eq!(at("f(\"a, b\", 'c, d', $,)"), Some(3));
        assert_eq!(at("f(A < B, C > D)"), Some(2));
        assert_eq!(at("f(A, "), None);
        // JavaScript's `\s`: U+FEFF is whitespace, U+0085 is not.
        assert_eq!(at("f(\u{feff})"), Some(0));
        assert_eq!(at("f(\u{85})"), Some(1));
        // The window is in UTF-16 units, upstream's string indices.
        let long = |n: usize| format!("f({})", "a".repeat(n));
        assert_eq!(at(&long(ARITY_WINDOW - 2)), Some(1));
        assert_eq!(at(&long(ARITY_WINDOW)), None);
        let wide = |n: usize| format!("f({})", "\u{1F600}".repeat(n));
        assert_eq!(at(&wide(ARITY_WINDOW / 2 - 1)), Some(1));
        assert_eq!(at(&wide(ARITY_WINDOW / 2)), None);
    }

    #[test]
    fn qualified_arity_reads_the_tail() {
        assert_eq!(qualified_arity("mod::f/2"), Some(2));
        assert_eq!(qualified_arity("mod::f"), None);
        assert_eq!(qualified_arity("mod::f/x"), None);
    }
}
