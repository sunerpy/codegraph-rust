//! The React Native and Expo passes (upstream `callback-synthesizer.ts` at
//! `e8ddc4e2`: `rnEventEdges`, `fabricNativeImplEdges`,
//! `expoCrossPlatformEdges` and `rnCrossPlatformEdges`; `4d1a2b3c` #430,
//! `07af3db6` #708, `6a02dcaa` #2138's constant event names, `6bd217a2`
//! #2769's escaping), W7-21:
//!
//! - `rn-event-channel`: a native method sending an event
//!   (`sendEventWithName:`, `sendEvent(withName:)`, `.emit(…)`, a
//!   `sendEvent(…)` wrapper) reaches the JavaScript handler a
//!   `.on|once|addListener(…)` registers for the same name, a literal or a
//!   constant holding one;
//! - `fabric-native-impl`: a view component (`fabric-component:`) reaches
//!   the Objective-C, Kotlin, Java or C++ class named for it;
//! - `expo-cross-platform`: an Expo module function's Swift and Kotlin
//!   declarations reach each other;
//! - `rn-cross-platform`: a native method a JavaScript caller reaches and the
//!   same-named method of another native language reach each other.
//!
//! Rust's own (KNOWN_DIFFS "The bridge a name means", KEEP-RUST): a named
//! handler is the function or method the name means by W7-14's rule (its
//! file's sole one, else the project's), nothing when several have it;
//! upstream takes the first.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::json;

use super::callback::{NamedHandler, Ordered, named_handler};
use super::context::ViewContext;
use super::stage::{LineAt, View, enclosing_fn, enclosing_value};
use crate::frameworks::js_language_for;
use crate::frameworks::react_native::{is_js_family_path, js_regex};
use crate::frameworks::swift_objc::split_lines;
use crate::import_resolver::resolve_import_path;
use crate::types::ResolutionContext;

/// An event with more handlers or dispatchers than this is too generic to
/// pair without receiver types (upstream `EVENT_FANOUT_CAP`).
const EVENT_FANOUT_CAP: usize = 6;

/// The kinds a constant event name is declared as (upstream
/// `CONSTANT_KINDS`).
const CONSTANT_KINDS: [NodeKind; 5] = [
    NodeKind::Constant,
    NodeKind::Variable,
    NodeKind::Field,
    NodeKind::Property,
    NodeKind::EnumMember,
];

/// The suffixes a view component's native class is named with, in the
/// order they are tried (upstream `FABRIC_NATIVE_SUFFIXES`).
const FABRIC_NATIVE_SUFFIXES: [&str; 5] = ["", "View", "ViewManager", "ComponentView", "Manager"];

/// The languages of a view component's native class.
const FABRIC_NATIVE_LANGUAGES: [Language; 4] = [
    Language::ObjC,
    Language::Kotlin,
    Language::Java,
    Language::Cpp,
];

/// The native languages `rn-cross-platform` pairs (upstream `NATIVE`: not
/// Swift).
const RN_NATIVE_LANGUAGES: [Language; 4] = [
    Language::Java,
    Language::Kotlin,
    Language::ObjC,
    Language::Cpp,
];

/// The JavaScript languages whose caller makes a native method a bridge one.
const JS_LANGUAGES: [Language; 4] = [
    Language::TypeScript,
    Language::Tsx,
    Language::JavaScript,
    Language::Jsx,
];

/// The methods every React Native module has, which the runtime calls
/// (upstream `RN_INFRA`).
const RN_INFRA: [&str; 11] = [
    "addListener",
    "removeListeners",
    "getConstants",
    "constantsToExport",
    "getName",
    "invalidate",
    "initialize",
    "getDefaultEventTypes",
    "supportedEvents",
    "requiresMainQueueSetup",
    "methodQueue",
];

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            static $name: LazyLock<Regex> = LazyLock::new(|| js_regex($pattern));
        )*
    };
}

patterns! {
    /// `sendEventWithName:@"x"` (upstream `RN_OBJC_SEND_RE`).
    OBJC_SEND = r#"(?-u:\b)sendEventWithName{S}*:{S}*@"([^"]+)""#;
    /// `sendEvent(withName: "x"` (upstream `RN_SWIFT_SEND_RE`).
    SWIFT_SEND = r#"(?-u:\b)sendEvent{S}*\({S}*withName{S}*:{S}*"([^"]+)""#;
    /// `.emit("x",` (upstream `RN_JVM_EMIT_RE`).
    JVM_EMIT = r#"\.emit{S}*\({S}*"([^"]+)"{S}*,"#;
    /// A `sendEvent(…"x"` wrapper call, within one statement (upstream
    /// `RN_NATIVE_SENDEVENT_RE`).
    NATIVE_SENDEVENT = r#"(?-u:\b)sendEvent{S}*\([^;{}]*?"([^"]+)""#;
    /// `sendEventWithName:CONST` (upstream `RN_OBJC_SEND_CONST_RE`).
    OBJC_SEND_CONST = r"(?-u:\b)sendEventWithName{S}*:{S}*([A-Za-z_][{W}]*)(?-u:\b)";
    /// `sendEvent(withName: CONST` (upstream `RN_SWIFT_SEND_CONST_RE`).
    SWIFT_SEND_CONST = r"(?-u:\b)sendEvent{S}*\({S}*withName{S}*:{S}*([A-Za-z_][{W}.]*)";
    /// `.emit(CONST,` (upstream `RN_JVM_EMIT_CONST_RE`).
    JVM_EMIT_CONST = r"\.emit{S}*\({S}*([A-Za-z_][{W}.]*){S}*,";
    /// `sendEvent(ctx, CONST,` (upstream `RN_NATIVE_SENDEVENT_CONST_RE`).
    NATIVE_SENDEVENT_CONST = r"(?-u:\b)sendEvent{S}*\({S}*[A-Za-z_][{W}.]*{S}*,{S}*([A-Za-z_][{W}.]*){S}*,";
    /// `.on(CONST, handler)` (upstream `RN_JS_LISTEN_CONST_RE`).
    JS_LISTEN_CONST = r"\.(?:on|once|addListener)\({S}*((?:[A-Za-z_$][{W}$]*\.)*[A-Za-z_$][{W}$]*){S}*,{S}*([A-Za-z_$][{W}$.]*|(?:async{S}*)?(?:\([^)]*\){S}*=>|[A-Za-z_$][{W}$]*{S}*=>|function{S}*\())";
    /// `.on('x', name)` (upstream `ADDLISTENER_ANY`).
    LISTEN_ANY = r#"\.(?:on|once|addListener)\({S}*['"]([^'"]+)['"]{S}*,{S}*([A-Za-z_][{W}.]*)"#;
    /// `.on('x', (data) => …)` (upstream `ADDLISTENER_INLINE`).
    LISTEN_INLINE = r#"\.(?:on|once|addListener)\({S}*['"]([^'"]+)['"]{S}*,{S}*(?:async{S}*)?(?:\([^)]*\){S}*=>|[A-Za-z_$][{W}$]*{S}*=>|function{S}*\()"#;
    /// A name a constant expression can end in.
    CONSTANT_NAME = r"^[A-Za-z_$][{W}$]*$";
    /// A handler written as a name path.
    HANDLER_PATH = r"^[A-Za-z_$][{W}$.]*$";
    /// A literal after a declared name: `= 'x'`, `: "x"`, `= @"x"`, the
    /// backreference of upstream's pattern written as its three quotes.
    DECLARED_LITERAL = r#"^{S}*(?::[^=;]*)?[=:]{S}*@?(?:"([^"'`]+)"|'([^"'`]+)'|`([^"'`]+)`)"#;
    /// A `#define kEvent @"x"` literal.
    DEFINED_LITERAL = r#"^{S}+@?"([^"]+)""#;
}

/// The files the event channel reads JavaScript handlers in (upstream's six
/// extensions).
fn is_js_handler_file(file: &str) -> bool {
    [".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

/// Whether `owner` is one of `prefix`'s `.`/`:`-separated segments
/// (upstream's escaped `(?:^|[.:])OWNER(?:[.:]|$)`), at any position, an
/// occurrence starting inside another included.
fn has_owner_segment(prefix: &str, owner: &str) -> bool {
    let separator = |ch: char| ch == '.' || ch == ':';
    let mut at = 0;
    loop {
        if prefix[at..].starts_with(owner) {
            let before = prefix[..at].chars().next_back();
            let after = prefix[at + owner.len()..].chars().next();
            if before.is_none_or(separator) && after.is_none_or(separator) {
                return true;
            }
        }
        match prefix[at..].chars().next() {
            Some(ch) => at += ch.len_utf8(),
            None => return false,
        }
    }
}

/// The event names constant expressions hold, read once a pass.
struct ConstantEvents<'v, 'a> {
    view: &'v View<'a>,
    context: ViewContext<'v, 'a>,
    memo: HashMap<(String, String), Option<String>>,
}

impl<'v, 'a> ConstantEvents<'v, 'a> {
    fn new(view: &'v View<'a>) -> Self {
        Self {
            view,
            context: view.context(),
            memo: HashMap::new(),
        }
    }

    /// The string `expr`, written in `file`, holds (upstream
    /// `constantEventName`): a bare `NAME` is a constant of the file, or
    /// (JavaScript) of the file its import of that name points at;
    /// `Owner.NAME` is a constant in `Owner`, or (JavaScript) `NAME` of the
    /// file an import of `Owner` points at. `None` unless those
    /// declarations' lines hold one literal.
    fn name_of(&mut self, expr: &str, file: &str) -> Result<Option<String>> {
        let key = (file.to_string(), expr.to_string());
        if let Some(found) = self.memo.get(&key) {
            return Ok(found.clone());
        }
        let value = self.read(expr, file)?;
        self.memo.insert(key, value.clone());
        Ok(value)
    }

    fn read(&self, expr: &str, file: &str) -> Result<Option<String>> {
        let parts: Vec<&str> = expr
            .strip_suffix(".rawValue")
            .unwrap_or(expr)
            .split('.')
            .collect();
        let name = parts[parts.len() - 1];
        let owner = (parts.len() > 1).then(|| parts[parts.len() - 2]);
        if !CONSTANT_NAME.is_match(name)
            || name == "this"
            || (parts.len() > 2 && parts[0] == "this")
        {
            return Ok(None);
        }
        let declared_in = |path: &str| -> Result<Vec<Node>> {
            Ok(self
                .view
                .nodes_in_file(path)?
                .into_iter()
                .filter(|node| node.name == name && CONSTANT_KINDS.contains(&node.kind))
                .collect())
        };
        let imported_file = |local: &str| -> Option<String> {
            if !is_js_family_path(file) {
                return None;
            }
            let language = js_language_for(file);
            let mapping = self
                .context
                .get_import_mappings(file, language)
                .into_iter()
                .find(|mapping| mapping.local_name == local)?;
            resolve_import_path(&mapping.source, file, language, &self.context)
        };
        let declarations = match owner {
            None => {
                let own = declared_in(file)?;
                if own.is_empty() {
                    match imported_file(name) {
                        Some(from) => declared_in(&from)?,
                        None => own,
                    }
                } else {
                    own
                }
            }
            Some(owner) => match imported_file(owner) {
                Some(from) => declared_in(&from)?,
                None => self
                    .view
                    .nodes_named(name)?
                    .into_iter()
                    .filter(|node| {
                        CONSTANT_KINDS.contains(&node.kind)
                            && has_owner_segment(
                                qualified_prefix(&node.qualified_name, name),
                                owner,
                            )
                    })
                    .collect(),
            },
        };
        let mut values: Vec<String> = declarations
            .iter()
            .filter_map(|node| self.literal_on_line(node, name))
            .collect();
        values.sort();
        values.dedup();
        Ok((values.len() == 1).then(|| values.remove(0)))
    }

    /// The literal `node`'s declaration line holds after the first
    /// occurrence of `name` (upstream `read`).
    fn literal_on_line(&self, node: &Node, name: &str) -> Option<String> {
        let text = self.view.text(&node.file_path)?;
        let lines = split_lines(&text);
        let line = usize::try_from(node.start_line - 1)
            .ok()
            .and_then(|at| lines.get(at))
            .copied()
            .unwrap_or("");
        let at = line.find(name)?;
        let rest = &line[at + name.len()..];
        if let Some(found) = DECLARED_LITERAL.captures(rest) {
            return found
                .get(1)
                .or_else(|| found.get(2))
                .or_else(|| found.get(3))
                .map(|value| value.as_str().to_string());
        }
        DEFINED_LITERAL
            .captures(rest)
            .map(|found| found[1].to_string())
    }
}

/// `qualified` up to the last occurrence of `name` (upstream
/// `qualifiedName.slice(0, qualifiedName.lastIndexOf(name))`, whose `-1`
/// drops the last character).
fn qualified_prefix<'q>(qualified: &'q str, name: &str) -> &'q str {
    match qualified.rfind(name) {
        Some(at) => &qualified[..at],
        None => {
            let cut = qualified.char_indices().next_back().map_or(0, |(at, _)| at);
            &qualified[..cut]
        }
    }
}

/// The React Native event channel (`rnEventEdges`): each native method
/// sending an event reaches each JavaScript handler registered for it, an
/// event with more than [`EVENT_FANOUT_CAP`] of either skipped. The native
/// dispatchers are found first: with none no edge is made, so a project
/// without native sends reads no JavaScript handler.
pub(super) fn rn_event_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let files = view.files()?;
    let mut constants = ConstantEvents::new(view);
    // event -> dispatcher ids, in the order they were found.
    let mut dispatchers: Ordered<String, Ordered<String, ()>> = Ordered::default();
    for file in &files {
        let objc = file.ends_with(".m") || file.ends_with(".mm");
        let swift = file.ends_with(".swift");
        let jvm = file.ends_with(".java") || file.ends_with(".kt");
        if !objc && !swift && !jvm {
            continue;
        }
        let Some(content) = view.text(file) else {
            continue;
        };
        if !content.contains("sendEvent") && !(jvm && content.contains(".emit")) {
            continue;
        }
        let nodes = view.callables_in_file(file)?;
        let lines = LineAt::new(&content, 1);
        let mut add = |event: &str, at: usize| {
            if let Some(dispatcher) = enclosing_fn(&nodes, lines.line(at)) {
                dispatchers
                    .entry(event.to_string(), Ordered::default)
                    .insert(dispatcher.id.clone(), ());
            }
        };
        let literal: &[&LazyLock<Regex>] = if objc {
            &[&OBJC_SEND]
        } else if swift {
            &[&SWIFT_SEND, &NATIVE_SENDEVENT]
        } else {
            &[&JVM_EMIT, &NATIVE_SENDEVENT]
        };
        let constant: &[&LazyLock<Regex>] = if objc {
            &[&OBJC_SEND_CONST]
        } else if swift {
            &[&SWIFT_SEND_CONST]
        } else {
            &[&JVM_EMIT_CONST, &NATIVE_SENDEVENT_CONST]
        };
        // Upstream runs each file's literal patterns, then its constant
        // ones (Objective-C's literal then constant; Swift's two literal
        // patterns then the constant one; the JVM's likewise).
        for pattern in literal {
            for found in pattern.captures_iter(&content) {
                add(&found[1], found.get(0).expect("a send").start());
            }
        }
        for pattern in constant {
            for found in pattern.captures_iter(&content) {
                if let Some(event) = constants.name_of(&found[1], file)? {
                    add(&event, found.get(0).expect("a send").start());
                }
            }
        }
    }
    if dispatchers.is_empty() {
        return Ok(Vec::new());
    }

    // event -> handler id -> registration site.
    let mut handlers: Ordered<String, Ordered<String, String>> = Ordered::default();
    for file in &files {
        if !is_js_handler_file(file) {
            continue;
        }
        let Some(content) = view.text(file) else {
            continue;
        };
        if ![".on(", ".once(", ".addListener("]
            .iter()
            .any(|call| content.contains(call))
        {
            continue;
        }
        let nodes = view.nodes_in_file(file)?;
        let lines = LineAt::new(&content, 1);
        // A named handler is the one the name means, else (no callable has
        // the name: a parameter passed through) the enclosing function, else
        // the enclosing value; the last registration's site is kept.
        for found in LISTEN_ANY.captures_iter(&content) {
            let event = &found[1];
            let argument = &found[2];
            let bare = argument.rsplit('.').next().unwrap_or(argument);
            let line = lines.line(found.get(0).expect("a registration").start());
            let target = match named_handler(view, bare, file)? {
                NamedHandler::Unique(node) => node.id,
                NamedHandler::Ambiguous => continue,
                NamedHandler::None => {
                    match enclosing_fn(&nodes, line).or_else(|| enclosing_value(&nodes, line)) {
                        Some(node) => node.id.clone(),
                        None => continue,
                    }
                }
            };
            handlers
                .entry(event.to_string(), Ordered::default)
                .insert(target, format!("{file}:{line}"));
        }
        // An inline handler lands in the enclosing function; the first
        // registration's site is kept.
        for found in LISTEN_INLINE.captures_iter(&content) {
            let line = lines.line(found.get(0).expect("a registration").start());
            if let Some(enclosing) = enclosing_fn(&nodes, line) {
                handlers
                    .entry(found[1].to_string(), Ordered::default)
                    .insert_if_absent(enclosing.id.clone(), format!("{file}:{line}"));
            }
        }
        // A constant event name: the handler the file names when it names
        // one function or method of its own, else the enclosing function.
        for found in JS_LISTEN_CONST.captures_iter(&content) {
            let Some(event) = constants.name_of(&found[1], file)? else {
                continue;
            };
            let argument = &found[2];
            let line = lines.line(found.get(0).expect("a registration").start());
            let bare = HANDLER_PATH
                .is_match(argument)
                .then(|| argument.rsplit('.').next().unwrap_or(argument));
            let named: Vec<&Node> = bare
                .map(|bare| {
                    nodes
                        .iter()
                        .filter(|node| {
                            matches!(node.kind, NodeKind::Function | NodeKind::Method)
                                && node.name == bare
                        })
                        .collect()
                })
                .unwrap_or_default();
            let target = match named.as_slice() {
                [one] => Some(*one),
                [] => enclosing_fn(&nodes, line),
                _ => None,
            };
            if let Some(target) = target {
                handlers
                    .entry(event, Ordered::default)
                    .insert_if_absent(target.id.clone(), format!("{file}:{line}"));
            }
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (event, sending) in dispatchers.iter() {
        let Some(registered) = handlers.get(event) else {
            continue;
        };
        if sending.len() > EVENT_FANOUT_CAP || registered.len() > EVENT_FANOUT_CAP {
            continue;
        }
        for (dispatcher, ()) in sending.iter() {
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
                        "synthesizedBy": "rn-event-channel",
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

/// View components to their native classes (`fabricNativeImplEdges`): each
/// `fabric-component:` component reaches every Objective-C, Kotlin, Java or
/// C++ class named for it with a [`FABRIC_NATIVE_SUFFIXES`] suffix.
pub(super) fn fabric_native_impl_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let components: Vec<Node> = view
        .nodes_of_kind(NodeKind::Component)?
        .into_iter()
        .filter(|node| node.id.starts_with("fabric-component:"))
        .collect();
    if components.is_empty() {
        return Ok(Vec::new());
    }
    let mut classes: HashMap<String, Vec<Node>> = HashMap::new();
    for class in view.nodes_of_kinds_in(&[NodeKind::Class], &FABRIC_NATIVE_LANGUAGES)? {
        classes.entry(class.name.clone()).or_default().push(class);
    }
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for component in &components {
        for suffix in FABRIC_NATIVE_SUFFIXES {
            let Some(natives) = classes.get(&format!("{}{suffix}", component.name)) else {
                continue;
            };
            for native in natives {
                if !seen.insert((component.id.clone(), native.id.clone())) {
                    continue;
                }
                let via = if suffix.is_empty() { "(exact)" } else { suffix };
                edges.push(Edge {
                    id: None,
                    source: component.id.clone(),
                    target: native.id.clone(),
                    kind: EdgeKind::Calls,
                    metadata: Some(json!({
                        "synthesizedBy": "fabric-native-impl",
                        "viaSuffix": via,
                        "componentName": component.name,
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

/// An Expo module function's platforms to each other
/// (`expoCrossPlatformEdges`): the `expo-module:` methods of one
/// `Module.function`, every ordered pair of two languages.
pub(super) fn expo_cross_platform_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    // The extraction writes Expo declarations as Swift or Kotlin only.
    let mut groups: Ordered<String, Vec<Node>> = Ordered::default();
    for method in
        view.nodes_of_kinds_in(&[NodeKind::Method], &[Language::Swift, Language::Kotlin])?
    {
        if !method.id.starts_with("expo-module:") {
            continue;
        }
        let key = method
            .qualified_name
            .rsplit("::")
            .next()
            .unwrap_or_default()
            .to_string();
        if key.is_empty() {
            continue;
        }
        groups.entry(key, Vec::new).push(method);
    }
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (_, group) in groups.iter() {
        if group.len() < 2 {
            continue;
        }
        for a in group {
            for b in group {
                if a.id == b.id
                    || a.language == b.language
                    || !seen.insert((a.id.clone(), b.id.clone()))
                {
                    continue;
                }
                edges.push(Edge {
                    id: None,
                    source: a.id.clone(),
                    target: b.id.clone(),
                    kind: EdgeKind::Calls,
                    metadata: Some(json!({
                        "synthesizedBy": "expo-cross-platform",
                        "via": a.name,
                    })),
                    line: Some(a.start_line),
                    col: None,
                    provenance: Some("heuristic".to_string()),
                });
            }
        }
    }
    Ok(edges)
}

/// A bridge method's native platforms to each other
/// (`rnCrossPlatformEdges`): the Java, Kotlin, Objective-C and C++ methods
/// of one name up to its first `:`, when a JavaScript node calls one of
/// them, that one and each other-language one both ways; the names every
/// module has ([`RN_INFRA`]) and one-language names skipped.
pub(super) fn rn_cross_platform_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let norm = |name: &str| name.split(':').next().unwrap_or(name).to_string();
    let mut groups: Ordered<String, Vec<Node>> = Ordered::default();
    for method in view.nodes_of_kinds_in(&[NodeKind::Method], &RN_NATIVE_LANGUAGES)? {
        groups.entry(norm(&method.name), Vec::new).push(method);
    }
    let candidates: Vec<(&String, &Vec<Node>)> = groups
        .iter()
        .filter(|(name, group)| {
            !RN_INFRA.contains(&name.as_str())
                && group
                    .iter()
                    .map(|method| method.language)
                    .collect::<HashSet<_>>()
                    .len()
                    >= 2
        })
        .collect();
    let ids: Vec<String> = candidates
        .iter()
        .flat_map(|(_, group)| group.iter().map(|method| method.id.clone()))
        .collect();
    let calls = view.edges_into_each(&ids, EdgeKind::Calls)?;
    let callers: Vec<String> = calls
        .values()
        .flatten()
        .map(|edge| edge.source.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let sources = view.nodes(&callers)?;
    let called_from_js = |method: &Node| {
        calls.get(&method.id).is_some_and(|edges| {
            edges.iter().any(|edge| {
                sources
                    .get(&edge.source)
                    .is_some_and(|source| JS_LANGUAGES.contains(&source.language))
            })
        })
    };
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (name, group) in candidates {
        for method in group {
            if !called_from_js(method) {
                continue;
            }
            for sibling in group {
                if sibling.id == method.id || sibling.language == method.language {
                    continue;
                }
                for (a, b) in [(method, sibling), (sibling, method)] {
                    if !seen.insert((a.id.clone(), b.id.clone())) {
                        continue;
                    }
                    edges.push(Edge {
                        id: None,
                        source: a.id.clone(),
                        target: b.id.clone(),
                        kind: EdgeKind::Calls,
                        metadata: Some(json!({
                            "synthesizedBy": "rn-cross-platform",
                            "via": name,
                        })),
                        line: Some(a.start_line),
                        col: None,
                        provenance: Some("heuristic".to_string()),
                    });
                }
            }
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_is_a_whole_segment_of_the_qualified_prefix() {
        assert!(has_owner_segment("src/a.ts::PrivateTypes.", "PrivateTypes"));
        assert!(has_owner_segment("Events:", "Events"));
        assert!(has_owner_segment("pkg.Events", "Events"));
        assert!(!has_owner_segment("src/a.ts::MyEvents.", "Events"));
        assert!(!has_owner_segment("src/a.ts::EventsX.", "Events"));
        // An occurrence starting inside an earlier one still counts, and an
        // empty owner is two separators in a row or one at an end.
        assert!(has_owner_segment("aaa.aa", "aa"));
        assert!(!has_owner_segment("aaa.", "aa"));
        assert!(has_owner_segment("a..b", ""));
        assert!(has_owner_segment("", ""));
        assert!(!has_owner_segment("a.b", ""));
        // Special characters are matched as written (upstream #2769).
        assert!(!has_owner_segment("a.b.", "a.*"));
        assert!(has_owner_segment("x.a.*:", "a.*"));
    }

    #[test]
    fn the_prefix_ends_at_the_names_last_occurrence() {
        assert_eq!(
            qualified_prefix("a.ts::Events.EVENT", "EVENT"),
            "a.ts::Events."
        );
        assert_eq!(qualified_prefix("EVENT", "EVENT"), "");
        // Upstream's `slice(0, -1)` without the name drops one character.
        assert_eq!(qualified_prefix("Owner.X", "Y"), "Owner.");
        assert_eq!(qualified_prefix("", "Y"), "");
    }

    #[test]
    fn the_patterns_read_upstreams_shapes() {
        let names = |pattern: &Regex, text: &str| -> Vec<String> {
            pattern
                .captures_iter(text)
                .map(|found| found[1].to_string())
                .collect()
        };
        assert_eq!(
            names(&OBJC_SEND, r#"[self sendEventWithName:@"onScan" body:@{}]"#),
            ["onScan"]
        );
        assert_eq!(
            names(
                &OBJC_SEND_CONST,
                "[self sendEventWithName:kLocationEvent body:b];"
            ),
            ["kLocationEvent"]
        );
        assert_eq!(
            names(
                &SWIFT_SEND,
                r#"sendEvent(withName: "geolocationDidChange", body: data)"#
            ),
            ["geolocationDidChange"]
        );
        assert_eq!(
            names(
                &SWIFT_SEND_CONST,
                "sendEvent(withName: Events.change.rawValue, body: b)"
            ),
            ["Events.change.rawValue"]
        );
        assert_eq!(
            names(&JVM_EMIT, r#"emitter.emit("onTick", body)"#),
            ["onTick"]
        );
        assert_eq!(
            names(&JVM_EMIT_CONST, "emitter.emit(EVENT_NAME, body)"),
            ["EVENT_NAME"]
        );
        assert_eq!(
            names(
                &NATIVE_SENDEVENT,
                r#"sendEvent(reactContext, "RNDeviceInfo_batteryLevelDidChange", body);"#
            ),
            ["RNDeviceInfo_batteryLevelDidChange"]
        );
        // The wrapper's definition opens a block before any literal.
        assert!(
            names(
                &NATIVE_SENDEVENT,
                "void sendEvent(ReactContext c, String name) {\n  x(\"y\");\n}"
            )
            .is_empty()
        );
        assert_eq!(
            names(
                &NATIVE_SENDEVENT_CONST,
                "sendEvent(reactContext, BATTERY_EVENT, body);"
            ),
            ["BATTERY_EVENT"]
        );
        let listen = |text: &str| -> Vec<(String, String)> {
            JS_LISTEN_CONST
                .captures_iter(text)
                .map(|found| (found[1].to_string(), found[2].to_string()))
                .collect()
        };
        assert_eq!(
            listen("emitter.addListener(PrivateTypes.DEVICE_CONNECTIVITY_EVENT, this._handle)"),
            [(
                "PrivateTypes.DEVICE_CONNECTIVITY_EVENT".to_string(),
                "this._handle".to_string()
            )]
        );
        assert_eq!(
            listen("emitter.on(EVENT, async (data) => {})"),
            [("EVENT".to_string(), "async".to_string())]
        );
        assert_eq!(
            listen("emitter.on(EVENT, (data) => {})"),
            [("EVENT".to_string(), "(data) =>".to_string())]
        );
        assert!(listen("emitter.on('literal', h)").is_empty());
        assert_eq!(
            names(
                &LISTEN_ANY,
                "emitter.addListener('onScan', handlers.onScan)"
            ),
            ["onScan"]
        );
        assert_eq!(
            names(
                &LISTEN_INLINE,
                "emitter.addListener('onScan', async () => {})"
            ),
            ["onScan"]
        );
        assert_eq!(
            names(&LISTEN_INLINE, "emitter.once(\"x\", function () {})"),
            ["x"]
        );
        let literal = |rest: &str| -> Option<String> {
            DECLARED_LITERAL.captures(rest).and_then(|found| {
                found
                    .get(1)
                    .or_else(|| found.get(2))
                    .or_else(|| found.get(3))
                    .map(|value| value.as_str().to_string())
            })
        };
        assert_eq!(
            literal(" = 'netInfo.networkStatusDidChange'").as_deref(),
            Some("netInfo.networkStatusDidChange")
        );
        assert_eq!(literal(": String = \"x\"").as_deref(), Some("x"));
        assert_eq!(literal(" = @\"kEvent\";").as_deref(), Some("kEvent"));
        assert_eq!(literal(" = `tpl`").as_deref(), Some("tpl"));
        // The closing quote is the opening one.
        assert_eq!(literal(" = 'x\""), None);
        assert_eq!(
            DEFINED_LITERAL
                .captures(" @\"x\"")
                .map(|found| found[1].to_string())
                .as_deref(),
            Some("x")
        );
    }
}
