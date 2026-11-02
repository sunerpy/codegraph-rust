//! The callback passes (upstream `callback-synthesizer.ts:131-394`): field
//! observers (`callback`), closure collections (`closure-collection`) and
//! string-keyed emitters (`event-emitter`). The regexes keep upstream's
//! ASCII word and case semantics.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use codegraph_store::queries::ReferenceSite;
use regex::Regex;
use serde_json::{Value, json};

use super::stage::{LineAt, View, enclosing_fn, slice_lines};

/// A registrar's name: `onUpdate`, `subscribe`, `addListener`, ...
static REGISTRAR_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:on[A-Z][A-Za-z0-9_]*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$",
    )
    .expect("registrar name")
});
/// A dispatcher's name: one holding `emit`, `trigger`, ... in any ASCII case.
static DISPATCHER_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)emit|trigger|notify|dispatch|fire|publish|flush").expect("dispatcher name")
});
/// The SQL superset of the two names: GLOBs, LIKEs and whole names.
const OBSERVER_GLOBS: &[&str] = &["on[A-Z]*"];
const OBSERVER_LIKES: &[&str] = &[
    "%emit%",
    "%trigger%",
    "%notify%",
    "%dispatch%",
    "%fire%",
    "%publish%",
    "%flush%",
];
const OBSERVER_NAMES: &[&str] = &[
    "subscribe",
    "addListener",
    "addEventListener",
    "register",
    "watch",
    "listen",
    "addCallback",
];
const MAX_CALLBACKS_PER_CHANNEL: usize = 40;
/// An event with more handlers or dispatchers than this is too generic to
/// pair without receiver types.
const EVENT_FANOUT_CAP: usize = 6;
/// A closure-collection field name with more dispatchers or registrars than
/// this is too generic to pair.
const CC_FANOUT_CAP: usize = 8;

static REGISTRAR_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"this\.([A-Za-z0-9_]+)\.(?:add|push|set)\(").expect("registrar field")
});
static FOR_OF_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)of\s+(?:Array\.from\(\s*)?this\.([A-Za-z0-9_]+)").expect("for-of field")
});
static ANY_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)[A-Za-z0-9_]+\s*\(").expect("call"));
static FOR_EACH_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"this\.([A-Za-z0-9_]+)\.forEach\(").expect("forEach field"));
static ON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\.(?:on|once|addListener)\(\s*['"]([^'"]+)['"]\s*,\s*(?:function\s+([A-Za-z0-9_]+)|(?:this\.)?([A-Za-z0-9_]+))"#,
    )
    .expect("on")
});
static EMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.(?:emit|fire|dispatchEvent)\(\s*['"]([^'"]+)['"]"#).expect("emit")
});
static CC_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z0-9_]+)\.forEach\s*\{\s*(?:\$0|it)\s*\(").expect("cc dispatch")
});
static CC_APPEND_WRITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"([A-Za-z0-9_]+)\.write\s*\{\s*\$0(?:\.([A-Za-z0-9_]+))?\.(?:append|add|push|insert)\s*\(",
    )
    .expect("cc append write")
});
static CC_APPEND_DIRECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z0-9_]+)\.(?:append|add|push|insert)\s*\(").expect("cc append")
});

/// Field-backed observer channels (`fieldChannelEdges`): a registrar adds a
/// callback to `this.<field>` and a dispatcher of the same file loops over
/// it. Each call of the registrar whose line passes a function value, bound
/// by resolution at that site, links every dispatcher of the channel to it.
pub(super) fn field_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut registrars: Vec<(Node, String)> = Vec::new();
    let mut dispatchers: Vec<(Node, String)> = Vec::new();
    for node in view.callables_named_like(OBSERVER_GLOBS, OBSERVER_LIKES, OBSERVER_NAMES)? {
        let is_registrar = REGISTRAR_NAME.is_match(&node.name);
        let is_dispatcher = DISPATCHER_NAME.is_match(&node.name);
        if !is_registrar && !is_dispatcher {
            continue;
        }
        let Some(content) = view.text(&node.file_path) else {
            continue;
        };
        let Some(source) = slice_lines(&content, node.start_line, node.end_line) else {
            continue;
        };
        if is_registrar && let Some(field) = registrar_field(&source) {
            registrars.push((node.clone(), field));
        }
        if is_dispatcher && let Some(field) = dispatcher_field(&source) {
            dispatchers.push((node, field));
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (registrar, field) in &registrars {
        let channel: Vec<&Node> = dispatchers
            .iter()
            .filter(|(dispatcher, dispatched)| {
                dispatcher.file_path == registrar.file_path && dispatched == field
            })
            .map(|(dispatcher, _)| dispatcher)
            .collect();
        if channel.is_empty() {
            continue;
        }
        let argument = Regex::new(&format!(
            r"{}\s*\(\s*(this\.[A-Za-z0-9_]+|[A-Za-z0-9_]+)\s*[,)]",
            regex::escape(&registrar.name)
        ))?;
        let mut registrations = Vec::new();
        for call in view.edges_into(&registrar.id, EdgeKind::Calls)? {
            let Some(line) = call.line.filter(|&line| line > 0) else {
                continue;
            };
            let Some(caller) = view.node(&call.source)? else {
                continue;
            };
            registrations.push((caller, line, call.col));
        }
        registrations.sort_by(|a, b| {
            (&a.0.file_path, a.1, a.2, &a.0.id).cmp(&(&b.0.file_path, b.1, b.2, &b.0.id))
        });
        let mut added = 0;
        for (caller, line, _) in registrations {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            let Some(text) = view.text(&caller.file_path) else {
                continue;
            };
            let Some(source_line) = text.split('\n').nth((line - 1) as usize) else {
                continue;
            };
            let Some(captures) = argument.captures(source_line) else {
                continue;
            };
            let written = &captures[1];
            let Some(value) = function_value_at(view, &caller, line, written)? else {
                continue;
            };
            let Some(function) = view.node(&value.target)? else {
                continue;
            };
            if !matches!(function.kind, NodeKind::Method | NodeKind::Function) {
                continue;
            }
            if !written.starts_with("this.")
                && function.file_path != caller.file_path
                && metadata_str(&value, "resolvedBy") != Some("import")
            {
                continue;
            }
            for dispatcher in &channel {
                if dispatcher.id == function.id
                    || !seen.insert((dispatcher.id.clone(), function.id.clone()))
                {
                    continue;
                }
                edges.push(Edge {
                    id: None,
                    source: dispatcher.id.clone(),
                    target: function.id.clone(),
                    kind: EdgeKind::Calls,
                    metadata: Some(json!({
                        "synthesizedBy": "callback",
                        "via": registrar.name,
                        "field": field,
                        "registeredAt": format!("{}:{line}", caller.file_path),
                    })),
                    line: Some(dispatcher.start_line),
                    col: None,
                    provenance: Some("heuristic".to_string()),
                });
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// The one function-value `references` edge `caller` has on `line` for the
/// value written `written` (upstream reads the edges' `refName`; Rust's edges
/// do not record it, so the site's stored reference names the value).
fn function_value_at(
    view: &View<'_>,
    caller: &Node,
    line: i64,
    written: &str,
) -> Result<Option<Edge>> {
    let mut found = Vec::new();
    for reference in view.edges_from(&caller.id, EdgeKind::References)? {
        if reference.line != Some(line)
            || reference
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("fnRef"))
                .and_then(Value::as_bool)
                != Some(true)
        {
            continue;
        }
        let Some(col) = reference.col else {
            continue;
        };
        let names_it = view
            .refs_at(ReferenceSite {
                from_node_id: caller.id.clone(),
                line,
                col,
            })?
            .iter()
            .any(|stored| stored.is_function_ref && stored.reference_name == written);
        if names_it {
            found.push(reference);
        }
    }
    Ok(if found.len() == 1 { found.pop() } else { None })
}

fn metadata_str<'a>(edge: &'a Edge, key: &str) -> Option<&'a str> {
    edge.metadata.as_ref()?.get(key)?.as_str()
}

fn registrar_field(source: &str) -> Option<String> {
    Some(REGISTRAR_FIELD.captures(source)?[1].to_string())
}

fn dispatcher_field(source: &str) -> Option<String> {
    if let Some(captures) = FOR_OF_FIELD.captures(source)
        && ANY_CALL.is_match(source)
    {
        return Some(captures[1].to_string());
    }
    Some(FOR_EACH_FIELD.captures(source)?[1].to_string())
}

/// Closure-collection dispatch (`closureCollectionEdges`): a Swift or Kotlin
/// dispatcher invokes each element of a collection property
/// (`validators.forEach { $0() }`) that a registrar appends to; paired by
/// field name across files, a generic name skipped.
pub(super) fn closure_collection_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut dispatchers: Ordered<String, Vec<(Node, i64)>> = Ordered::default();
    let mut registrars: Ordered<String, Vec<(Node, i64)>> = Ordered::default();
    for node in view.callables_in_languages(&[Language::Swift, Language::Kotlin])? {
        let Some(content) = view.text(&node.file_path) else {
            continue;
        };
        let Some(source) = slice_lines(&content, node.start_line, node.end_line) else {
            continue;
        };
        let has_for_each = source.contains(".forEach");
        let has_append = [".append(", ".add(", ".push(", ".insert("]
            .iter()
            .any(|call| source.contains(call));
        if !has_for_each && !has_append {
            continue;
        }
        let lines = LineAt::new(&source, node.start_line);
        if has_for_each {
            for captures in CC_DISPATCH.captures_iter(&source) {
                let line = lines.line(captures.get(0).map_or(0, |whole| whole.start()));
                add_member(&mut dispatchers, &captures[1], &node, line);
            }
        }
        if has_append {
            for captures in CC_APPEND_WRITE.captures_iter(&source) {
                let line = lines.line(captures.get(0).map_or(0, |whole| whole.start()));
                // A nested `$0.streams` else the `.write` receiver.
                let field = captures
                    .get(2)
                    .map(|nested| nested.as_str())
                    .filter(|nested| !nested.is_empty())
                    .unwrap_or(&captures[1]);
                add_registrar(&mut registrars, field, &node, line);
            }
            for captures in CC_APPEND_DIRECT.captures_iter(&source) {
                let line = lines.line(captures.get(0).map_or(0, |whole| whole.start()));
                add_registrar(&mut registrars, &captures[1], &node, line);
            }
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (field, invoking) in dispatchers.iter() {
        let Some(appending) = registrars.get(field) else {
            continue;
        };
        if appending.is_empty() || invoking.len() > CC_FANOUT_CAP || appending.len() > CC_FANOUT_CAP
        {
            continue;
        }
        for (dispatcher, line) in invoking {
            for (registrar, registered) in appending {
                if dispatcher.id == registrar.id
                    || !seen.insert((dispatcher.id.clone(), registrar.id.clone()))
                {
                    continue;
                }
                edges.push(Edge {
                    id: None,
                    source: dispatcher.id.clone(),
                    target: registrar.id.clone(),
                    kind: EdgeKind::Calls,
                    metadata: Some(json!({
                        "synthesizedBy": "closure-collection",
                        "field": field,
                        "registeredAt": format!("{}:{registered}", registrar.file_path),
                    })),
                    line: Some(*line),
                    col: None,
                    provenance: Some("heuristic".to_string()),
                });
            }
        }
    }
    Ok(edges)
}

/// Record `node` under `field` once, at its first line there.
fn add_member(
    channels: &mut Ordered<String, Vec<(Node, i64)>>,
    field: &str,
    node: &Node,
    line: i64,
) {
    let members = channels.entry(field.to_string(), Vec::new);
    if !members.iter().any(|(member, _)| member.id == node.id) {
        members.push((node.clone(), line));
    }
}

/// [`add_member`] for a registrar: `$0.append` mis-captures the `0`, which
/// the write regex owns.
fn add_registrar(
    channels: &mut Ordered<String, Vec<(Node, i64)>>,
    field: &str,
    node: &Node,
    line: i64,
) {
    if field.is_empty() || field.bytes().all(|byte| byte.is_ascii_digit()) {
        return;
    }
    add_member(channels, field, node, line);
}

/// String-keyed emitter channels (`eventEmitterEdges`): the function holding
/// `x.emit('e')` reaches the handler `y.on('e', handler)` registers, an event
/// with too many handlers or dispatchers skipped.
pub(super) fn event_emitter_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    // event -> dispatcher ids; event -> handler id -> registration site.
    let mut emits: Ordered<String, Ordered<String, ()>> = Ordered::default();
    let mut handlers: Ordered<String, Ordered<String, String>> = Ordered::default();
    for file in view.files()? {
        let Some(content) = view.text(&file) else {
            continue;
        };
        let has_emit = [".emit(", ".fire(", ".dispatchEvent("]
            .iter()
            .any(|call| content.contains(call));
        let has_on = [".on(", ".once(", ".addListener("]
            .iter()
            .any(|call| content.contains(call));
        if !has_emit && !has_on {
            continue;
        }
        let nodes = view.nodes_in_file(&file)?;
        let lines = LineAt::new(&content, 1);
        if has_emit {
            for captures in EMIT.captures_iter(&content) {
                let line = lines.line(captures.get(0).map_or(0, |whole| whole.start()));
                let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                    continue;
                };
                emits
                    .entry(captures[1].to_string(), Ordered::default)
                    .insert(dispatcher.id.clone(), ());
            }
        }
        if has_on {
            for captures in ON.captures_iter(&content) {
                let Some(name) = captures.get(2).or_else(|| captures.get(3)) else {
                    continue;
                };
                let Some(handler) = unique_handler(view, name.as_str(), &file)? else {
                    continue;
                };
                let line = lines.line(captures.get(0).map_or(0, |whole| whole.start()));
                handlers
                    .entry(captures[1].to_string(), Ordered::default)
                    .insert(handler.id, format!("{file}:{line}"));
            }
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (event, dispatchers) in emits.iter() {
        let Some(registered) = handlers.get(event) else {
            continue;
        };
        if dispatchers.len() > EVENT_FANOUT_CAP || registered.len() > EVENT_FANOUT_CAP {
            continue;
        }
        for (dispatcher, ()) in dispatchers.iter() {
            for (handler, registered_at) in registered.iter() {
                if dispatcher == handler || !seen.insert((dispatcher.clone(), handler.clone())) {
                    continue;
                }
                edges.push(Edge {
                    id: None,
                    source: dispatcher.clone(),
                    target: handler.clone(),
                    kind: EdgeKind::Calls,
                    metadata: Some(json!({
                        "synthesizedBy": "event-emitter",
                        "event": event,
                        "registeredAt": registered_at,
                    })),
                    line: None,
                    col: None,
                    provenance: Some("heuristic".to_string()),
                });
            }
        }
    }
    Ok(edges)
}

/// The function or method `name` means at a registration in `file`
/// (KEEP-RUST: upstream takes the first of `getNodesByName` in row order):
/// the one of that name in `file`, else the one in the project, nothing when
/// either holds several.
fn unique_handler(view: &View<'_>, name: &str, file: &str) -> Result<Option<Node>> {
    let candidates: Vec<Node> = view
        .nodes_named(name)?
        .into_iter()
        .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
        .collect();
    let in_file: Vec<&Node> = candidates
        .iter()
        .filter(|node| node.file_path == file)
        .collect();
    Ok(match (in_file.len(), candidates.len()) {
        (1, _) => Some(in_file[0].clone()),
        (0, 1) => Some(candidates[0].clone()),
        _ => None,
    })
}

/// An insertion-ordered map: upstream's channels are `Map`s, iterated in the
/// order their keys arrived; inserting a present key replaces its value in
/// place.
struct Ordered<K, V> {
    entries: Vec<(K, V)>,
    index: HashMap<K, usize>,
}

impl<K, V> Default for Ordered<K, V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone, V> Ordered<K, V> {
    fn entry(&mut self, key: K, default: impl FnOnce() -> V) -> &mut V {
        let at = match self.index.get(&key) {
            Some(&at) => at,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, default()));
                self.entries.len() - 1
            }
        };
        &mut self.entries[at].1
    }

    fn insert(&mut self, key: K, value: V) {
        match self.index.get(&key) {
            Some(&at) => self.entries[at].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.index.get(key).map(|&at| &self.entries[at].1)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }
}
