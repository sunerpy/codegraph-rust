//! The ArkUI passes (upstream `callback-synthesizer.ts`, #1186): a reactive
//! property's assignment re-running its struct's `build()` (`arkui-state`),
//! a HarmonyOS `@ohos.events.emitter` emit reaching the handlers of its
//! event (`arkui-emitter`), and a literal `router.pushUrl` reaching its
//! page's `@Entry` struct (`arkui-route`). Each runs when the project has
//! ArkTS files.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::{Captures, Regex};
use serde_json::json;

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::dispatch::{place, synthesized};
use super::stage::{LineAt, View};
use crate::strip_comments::strip_js_comments_keeping_offsets;

/// Whether the project has ArkTS files, which every pass here reads.
pub(super) fn gate(languages: &HashSet<Language>) -> bool {
    languages.contains(&Language::ArkTs)
}

/// The reactive property decorators: assigning a property one marks re-runs
/// its struct's `build()` (upstream `ARKUI_REACTIVE_DECORATORS`, ArkUI's V1
/// and V2 state models).
const REACTIVE_DECORATORS: [&str; 13] = [
    "State",
    "Prop",
    "Link",
    "Provide",
    "Consume",
    "StorageLink",
    "StorageProp",
    "LocalStorageLink",
    "LocalStorageProp",
    "ObjectLink",
    "Local",
    "Provider",
    "Consumer",
];

/// The array mutators ArkUI observes like an assignment (upstream
/// `ARKUI_ARRAY_MUTATORS`).
const ARRAY_MUTATORS: &str = "push|pop|shift|unshift|splice|sort|reverse|fill";

/// A key bucket's emit or handler sites at most: more is dynamic routing,
/// not a static pair (upstream `ARKUI_EMITTER_FANOUT_CAP`).
const EMITTER_FANOUT_CAP: usize = 8;

/// The UTF-16 units an emitter's `{ … eventId: K }` argument and a router's
/// `{ … url: '…' }` argument may hold before the key (upstream's
/// `[^)]{0,120}?` and `[^)]{0,200}?`).
const EMITTER_WINDOW: usize = 120;
const ROUTER_WINDOW: usize = 200;

/// An emit or subscription of `@ohos.events.emitter` (upstream
/// `ARKUI_EMITTER_CALL_RE`, ASCII `\b` and `\w`): the verb, then the first
/// argument, a name or an `{ … eventId: K … }` literal whose leading
/// window [`EMITTER_WINDOW`] bounds.
static EMITTER_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?-u:\b)emitter\s*\.\s*(emit|on|once)\s*\(\s*([A-Za-z_$][0-9A-Za-z_$.]*|\{([^)]{0,120}?)(?-u:\b)eventId\s*:\s*[^,}]+[^)]*?\})",
    )
    .expect("emitter call")
});

/// An `{ eventId: K }` literal's key (upstream's `/\beventId\s*:\s*([\w$.]+)/`).
static EVENT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)eventId\s*:\s*([0-9A-Za-z_$.]+)").expect("event id"));

/// A literal `router.pushUrl` / `replaceUrl` url (upstream `ARKUI_ROUTER_RE`),
/// its leading window [`ROUTER_WINDOW`] bounding the text before `url`.
static ROUTER_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?-u:\b)router\s*\.\s*(?:pushUrl|replaceUrl)\s*\(\s*\{([^)]{0,200}?)(?-u:\b)url\s*:\s*['"]([0-9A-Za-z_\-./]+)['"]"#,
    )
    .expect("router call")
});

/// Whether `text` is ASCII digits only (upstream's `/^\d+$/`).
fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `text` is upstream's `/^[\w$.]+$/`.
fn is_dotted_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.'))
}

/// The UTF-16 units of `text`.
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Each match of `pattern` in `text` whose group `window` holds at most
/// `limit` UTF-16 units, as a JavaScript global regex finds them: a match
/// whose window is longer fails where it starts, and the search goes on
/// from the next character.
fn windowed_matches<'t>(
    pattern: &Regex,
    text: &'t str,
    window: usize,
    limit: usize,
) -> Vec<Captures<'t>> {
    let mut found = Vec::new();
    let mut at = 0;
    while at <= text.len() {
        let Some(captures) = pattern.captures_at(text, at) else {
            break;
        };
        let whole = captures.get(0).expect("the whole match");
        if captures
            .get(window)
            .is_some_and(|group| utf16_len(group.as_str()) > limit)
        {
            at = whole.start()
                + text[whole.start()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
            continue;
        }
        at = if whole.end() > whole.start() {
            whole.end()
        } else {
            whole.end() + 1
        };
        found.push(captures);
    }
    found
}

/// The method or function of `nodes` whose lines hold `line` with the
/// fewest lines, the earliest of `nodes` on a tie (upstream's emitter and
/// router `encl`).
fn innermost_callable(nodes: &[Node], line: i64) -> Option<&Node> {
    let mut best: Option<&Node> = None;
    for node in nodes {
        if !matches!(node.kind, NodeKind::Method | NodeKind::Function)
            || node.start_line > line
            || node.end_line < line
        {
            continue;
        }
        let span = node.end_line - node.start_line;
        if best.is_none_or(|best| span < best.end_line - best.start_line) {
            best = Some(node);
        }
    }
    best
}

/// The member directories of the project's workspace, longest first
/// (upstream's `moduleDirs`), whose first one holding `file` is its module.
struct ModuleScopes(Vec<String>);

impl ModuleScopes {
    fn of(view: &View<'_>) -> Self {
        let context = view.context();
        let mut dirs: Vec<String> =
            crate::types::ResolutionContext::get_workspace_packages(&context)
                .map(|packages| packages.by_name.into_values().collect())
                .unwrap_or_default();
        dirs.sort();
        dirs.dedup();
        dirs.sort_by_key(|dir| std::cmp::Reverse(dir.len()));
        Self(dirs)
    }

    /// `file`'s module directory, `""` outside every one.
    fn scope_of(&self, file: &str) -> &str {
        self.0
            .iter()
            .find(|dir| {
                file == dir.as_str()
                    || file
                        .strip_prefix(dir.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map_or("", String::as_str)
    }
}

/// The nodes `parent` contains, in its `contains` edges' order.
fn children_of(view: &View<'_>, parent: &str) -> Result<Vec<Node>> {
    let mut children = Vec::new();
    for edge in view.edges_from(parent, EdgeKind::Contains)? {
        if let Some(node) = view.node(&edge.target)? {
            children.push(node);
        }
    }
    Ok(children)
}

/// Whether `source` writes one of the properties `mutation` names: an
/// assignment that is no comparison (`=` not followed by another `=`, the
/// lookahead upstream's `=(?!=)` writes), an increment or a compound
/// assignment, or an array mutator's call.
fn writes_state(mutation: &Regex, source: &str) -> bool {
    mutation
        .captures_iter(source)
        .any(|captures| match captures.get(1) {
            Some(assign) => !source[assign.end()..].starts_with('='),
            None => true,
        })
}

/// ArkUI state → build (`arkuiStateBuildEdges`): each method of a struct
/// that writes one of the struct's reactive properties reaches its
/// `build()`, at most [`MAX_CALLBACKS_PER_CHANNEL`] a struct; a method that
/// only reads them, and a struct with none, get nothing.
pub(super) fn state_build_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for item in view.nodes_of_kinds_in(&[NodeKind::Struct], &[Language::ArkTs])? {
        let children = children_of(view, &item.id)?;
        let Some(build) = children
            .iter()
            .find(|node| node.kind == NodeKind::Method && node.name == "build")
        else {
            continue;
        };
        let reactive: Vec<String> = children
            .iter()
            .filter(|node| {
                node.kind == NodeKind::Property
                    && node
                        .decorators
                        .iter()
                        .any(|decorator| REACTIVE_DECORATORS.contains(&decorator.as_str()))
            })
            .map(|node| regex::escape(&node.name))
            .collect();
        if reactive.is_empty() {
            continue;
        }
        let Ok(mutation) = Regex::new(&format!(
            r"this\.(?:{})\s*(?:(=)|\+\+|--|[+\-*/%&|^]=|\.(?:{ARRAY_MUTATORS})\s*\()",
            reactive.join("|")
        )) else {
            continue;
        };
        let mut added = 0;
        for method in &children {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            if method.kind != NodeKind::Method || method.id == build.id {
                continue;
            }
            let Some(source) =
                view.text_lines(&method.file_path, method.start_line, method.end_line)
            else {
                continue;
            };
            if source.is_empty()
                || !writes_state(&mutation, &strip_js_comments_keeping_offsets(&source))
                || !seen.insert((method.id.clone(), build.id.clone()))
            {
                continue;
            }
            edges.push(synthesized(
                method,
                build,
                EdgeKind::Calls,
                method.start_line,
                json!({
                    "synthesizedBy": "arkui-state",
                    "via": "state assignment",
                    "registeredAt": place(build),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// An emit or a handler site of one event: the method or function it is
/// written in, its file and its line.
struct EmitterSite {
    node: Node,
    file: String,
    line: i64,
}

/// The event key of an emitter call's first argument `argument` in `file`
/// (upstream's key recovery): an `{ eventId: K }` literal's `K`, or the
/// argument itself; digits pair within the file, a dotted name within the
/// module, and a local is chased one level through its declaration in the
/// file (`new EventsId(K)`, `= K`).
fn event_key(argument: &str, file: &str, safe: &str, scopes: &ModuleScopes) -> Option<String> {
    let literal = argument
        .starts_with('{')
        .then(|| EVENT_ID.captures(argument))
        .flatten()
        .and_then(|captures| captures.get(1))
        .map(|key| key.as_str());
    let token = literal.unwrap_or(argument);
    if is_digits(token) {
        return Some(format!("num:{file}:{token}"));
    }
    if token.contains('.') {
        return Some(format!("name:{}:{token}", scopes.scope_of(file)));
    }
    let declaration = Regex::new(&format!(
        r"(?-u:\b){}(?-u:\b)\s*(?::[^=\n]+)?=\s*(?:new\s+[0-9A-Za-z_$.]+\(\s*([^)\n]+?)\s*\)|([0-9A-Za-z_$.]+))",
        regex::escape(token)
    ))
    .ok()?;
    let captures = declaration.captures(safe)?;
    let inner = captures.get(1).or_else(|| captures.get(2))?.as_str().trim();
    if is_digits(inner) {
        Some(format!("num:{file}:{inner}"))
    } else if is_dotted_name(inner) {
        Some(format!("name:{}:{inner}", scopes.scope_of(file)))
    } else {
        None
    }
}

/// The `@ohos.events.emitter` bridge (`arkuiEmitterEdges`): an emit's
/// method or function reaches each one subscribing (`on`, `once`) to the
/// same statically recovered event key, a key with more than
/// [`EMITTER_FANOUT_CAP`] emit or handler sites none. Keys are taken in the
/// order their first emit appears (upstream iterates its emit map), and a
/// pair two keys join keeps the first one's `event`.
pub(super) fn emitter_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let scopes = ModuleScopes::of(view);
    let mut keys: Vec<String> = Vec::new();
    let mut emits: HashMap<String, Vec<EmitterSite>> = HashMap::new();
    let mut handlers: HashMap<String, Vec<EmitterSite>> = HashMap::new();
    for file in view.files()? {
        if !file.ends_with(".ets") {
            continue;
        }
        let Some(content) = view.text(&file) else {
            continue;
        };
        if !content.contains("emitter.") {
            continue;
        }
        let safe = strip_js_comments_keeping_offsets(&content);
        let nodes: Vec<Node> = view
            .nodes_in_file(&file)?
            .into_iter()
            .filter(|node| matches!(node.kind, NodeKind::Method | NodeKind::Function))
            .collect();
        let lines = LineAt::new(&safe, 1);
        for captures in windowed_matches(&EMITTER_CALL, &safe, 3, EMITTER_WINDOW) {
            let whole = captures.get(0).expect("the whole match");
            let verb = &captures[1];
            let argument = captures[2].trim();
            let line = lines.line(whole.start());
            let Some(enclosing) = innermost_callable(&nodes, line) else {
                continue;
            };
            let Some(key) = event_key(argument, &file, &safe, &scopes) else {
                continue;
            };
            if verb == "emit" && !emits.contains_key(&key) {
                keys.push(key.clone());
            }
            let site = EmitterSite {
                node: enclosing.clone(),
                file: file.clone(),
                line,
            };
            if verb == "emit" {
                emits.entry(key).or_default().push(site);
            } else {
                handlers.entry(key).or_default().push(site);
            }
        }
    }
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for key in &keys {
        let (Some(emit_sites), Some(handler_sites)) = (emits.get(key), handlers.get(key)) else {
            continue;
        };
        if emit_sites.len() > EMITTER_FANOUT_CAP || handler_sites.len() > EMITTER_FANOUT_CAP {
            continue;
        }
        let event = &key[key.rfind(':').map_or(0, |colon| colon + 1)..];
        for emit in emit_sites {
            for handler in handler_sites {
                if emit.node.id == handler.node.id
                    || !seen.insert((emit.node.id.clone(), handler.node.id.clone()))
                {
                    continue;
                }
                edges.push(synthesized(
                    &emit.node,
                    &handler.node,
                    EdgeKind::Calls,
                    emit.line,
                    json!({
                        "synthesizedBy": "arkui-emitter",
                        "event": event,
                        "registeredAt": format!("{}:{}", handler.file, handler.line),
                    }),
                ));
            }
        }
    }
    Ok(edges)
}

/// HarmonyOS page navigation (`arkuiRouterEdges`): a literal
/// `router.pushUrl` / `replaceUrl` url reaches the `@Entry` struct of
/// `<module>/src/main/ets/<url>.ets`; with that page in several modules the
/// caller's own module's is taken, and a name still ambiguous links nothing.
pub(super) fn router_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let scopes = ModuleScopes::of(view);
    let files = view.files()?;
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in &files {
        if !file.ends_with(".ets") {
            continue;
        }
        let Some(content) = view.text(file) else {
            continue;
        };
        if !content.contains("router.") {
            continue;
        }
        let safe = strip_js_comments_keeping_offsets(&content);
        let nodes: Vec<Node> = view
            .nodes_in_file(file)?
            .into_iter()
            .filter(|node| matches!(node.kind, NodeKind::Method | NodeKind::Function))
            .collect();
        let lines = LineAt::new(&safe, 1);
        for captures in windowed_matches(&ROUTER_CALL, &safe, 1, ROUTER_WINDOW) {
            let whole = captures.get(0).expect("the whole match");
            let url = &captures[2];
            let line = lines.line(whole.start());
            let Some(enclosing) = innermost_callable(&nodes, line) else {
                continue;
            };
            let suffix = format!("/src/main/ets/{url}.ets");
            let mut candidates: Vec<&String> = files
                .iter()
                .filter(|candidate| candidate.ends_with(&suffix))
                .collect();
            if candidates.len() > 1 {
                let scope = scopes.scope_of(file);
                let same: Vec<&String> = candidates
                    .iter()
                    .copied()
                    .filter(|candidate| scopes.scope_of(candidate) == scope)
                    .collect();
                if !same.is_empty() {
                    candidates = same;
                }
            }
            let [page_file] = candidates.as_slice() else {
                continue;
            };
            let Some(page) = view.nodes_in_file(page_file)?.into_iter().find(|node| {
                node.kind == NodeKind::Struct && node.decorators.iter().any(|d| d == "Entry")
            }) else {
                continue;
            };
            if !seen.insert((enclosing.id.clone(), page.id.clone())) {
                continue;
            }
            edges.push(synthesized(
                enclosing,
                &page,
                EdgeKind::Calls,
                line,
                json!({
                    "synthesizedBy": "arkui-route",
                    "event": url,
                    "registeredAt": format!("{page_file}:{}", page.start_line),
                }),
            ));
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_writes_are_assignments_increments_and_mutators_never_comparisons() {
        let mutation = Regex::new(&format!(
            r"this\.(?:count|todos)\s*(?:(=)|\+\+|--|[+\-*/%&|^]=|\.(?:{ARRAY_MUTATORS})\s*\()"
        ))
        .unwrap();
        for written in [
            "this.count = 1",
            "this.count=1",
            "this.count++",
            "this.count -= 2",
            "this.todos.push(x)",
            "this.todos .splice (0, 1)",
            "if (this.count == 0) { this.count = 1 }",
        ] {
            assert!(writes_state(&mutation, written), "{written}");
        }
        for read in [
            "return this.count == 0",
            "return this.count === 0",
            "return this.count !== 0",
            "return this.count <= 0",
            "return this.todos.length",
            "return this.counter = 1",
        ] {
            assert!(!writes_state(&mutation, read), "{read}");
        }
    }

    #[test]
    fn windows_count_utf16_units_and_a_failed_window_retries_further_on() {
        let pattern = Regex::new(r"a([^)]{0,3}?)b").unwrap();
        let text = "a😀😀bab";
        let found: Vec<String> = windowed_matches(&pattern, text, 1, 3)
            .into_iter()
            .map(|captures| captures[0].to_string())
            .collect();
        assert_eq!(found, ["ab"]);
    }

    #[test]
    fn the_innermost_callable_has_the_fewest_lines_and_the_earliest_on_a_tie() {
        let node = |id: &str, start: i64, end: i64| Node {
            id: id.to_string(),
            kind: NodeKind::Method,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: "a.ets".to_string(),
            language: Language::ArkTs,
            start_line: start,
            end_line: end,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        };
        let nodes = [node("outer", 1, 20), node("a", 5, 8), node("b", 6, 9)];
        assert_eq!(
            innermost_callable(&nodes, 7).map(|n| n.id.as_str()),
            Some("a")
        );
        assert_eq!(
            innermost_callable(&nodes, 9).map(|n| n.id.as_str()),
            Some("b")
        );
        assert_eq!(innermost_callable(&nodes, 30), None);
    }
}
