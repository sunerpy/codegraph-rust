//! The framework dispatch passes (upstream `callback-synthesizer.ts` at
//! `e8ddc4e2`). Each framework links a dispatch site to the code it reaches
//! by a name or a type the call only carries at run time:
//!
//! - a redux-thunk constant to each thunk its body dispatches,
//!   `dispatch(next(…))` (`redux-thunk`);
//! - a Celery `task.delay(…)` or `task.apply_async(…)` to the decorated task
//!   (`celery-dispatch`);
//! - Spring's `publishEvent(new X(…))` to every listener method of `X`
//!   (`spring-event`);
//! - MediatR's `Send(x)` or `Publish(x)` to the `Handle` of the handler of
//!   `x`'s type (`mediatr-dispatch`);
//! - a Sidekiq worker's `perform_async` to its `perform`
//!   (`sidekiq-dispatch`);
//! - Laravel's `event(new X(…))` to every listener's `handle` for `X`
//!   (`laravel-event`).
//!
//! Each reads the indexed files' text and the base graph's nodes, and links
//! from the function the dispatch site is in.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::json;

use super::dispatch::synthesized;
use super::stage::{LineAt, View, enclosing_fn, slice_lines};
use crate::frameworks::js_scan::{JS_SPACE, is_js_space, js_trim};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};

/// The thunks one thunk constant links at most (`THUNK_FANOUT_CAP`).
const THUNK_FANOUT_CAP: usize = 24;
/// The edges one file's Celery, Spring, MediatR or Sidekiq dispatch sites
/// link at most (`CELERY_FANOUT_CAP` and the others).
const FILE_FANOUT_CAP: usize = 80;
/// The edges one file's Laravel dispatch sites link at most
/// (`LARAVEL_FANOUT_CAP`).
const LARAVEL_FANOUT_CAP: usize = 200;
/// The lines above a `def` read for its decorators
/// (`CELERY_DECORATOR_LOOKBACK`).
const CELERY_DECORATOR_LOOKBACK: i64 = 12;
/// The lines from a class's first that hold its base list
/// (`MEDIATR_HANDLER_DECL_LOOKAHEAD`).
const MEDIATR_HANDLER_DECL_LOOKAHEAD: usize = 4;

/// An upstream pattern with JavaScript's `\s` ([`JS_SPACE`]) for the
/// `regex` crate's Unicode one. Every pattern here spells `\w` as
/// `[A-Za-z0-9_]` and `\b` as `(?-u:\b)`, as JavaScript reads them.
fn js_regex(pattern: &str, what: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// A thunk's initializer (`THUNK_DECL_RE`).
static THUNK_DECL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"create(?:Async)?Thunk").expect("thunk declaration"));
/// `dispatch(next(…))` in a thunk's body (`THUNK_DISPATCH_RE`).
static THUNK_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)dispatch\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*[(),]",
        "thunk dispatch",
    )
});
/// `task.delay(` or `task.apply_async(` (`CELERY_DISPATCH_RE`).
static CELERY_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*(?:delay|apply_async)\s*\(",
        "celery dispatch",
    )
});
/// The declaration above a task's decorators, where reading them stops.
static CELERY_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^(?:async\s+def|def|class)(?-u:\b)", "celery declaration"));
/// A task decorator, `@shared_task` or `@app.task`, called or not
/// (`CELERY_TASK_DECORATOR_RE`).
static CELERY_TASK_DECORATOR: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"@\s*(?:[A-Za-z_][A-Za-z0-9_.]*\.)?(?:shared_task|task)(?-u:\b)",
        "celery task decorator",
    )
});
/// `publishEvent(new X` (`SPRING_PUBLISH_RE`).
static SPRING_PUBLISH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"\.publishEvent\s*\(\s*new\s+([A-Z][A-Za-z0-9_]*)",
        "spring publish",
    )
});
/// A listener annotation (`SPRING_LISTENER_ANNO_RE`).
static SPRING_LISTENER_ANNOTATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"@(?:EventListener|TransactionalEventListener)(?-u:\b)")
        .expect("spring listener annotation")
});
/// The event type a listener annotation names, `@EventListener(X.class)`
/// (`SPRING_ANNO_TYPE_RE`).
static SPRING_ANNOTATED_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"@(?:EventListener|TransactionalEventListener)\s*\(\s*([A-Z][A-Za-z0-9_]*)\.class",
        "spring annotated type",
    )
});
/// `ApplicationListener<` (`SPRING_APP_LISTENER_RE`).
static SPRING_APPLICATION_LISTENER: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)ApplicationListener\s*<",
        "spring application listener",
    )
});
/// A class name, as an event type must be.
static CLASS_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Za-z0-9_]*$").expect("class name"));
/// A MediatR handler's base and the request it handles
/// (`MEDIATR_HANDLER_BASE_RE`).
static MEDIATR_HANDLER_BASE: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?:IRequestHandler|INotificationHandler)\s*<\s*([A-Za-z_][A-Za-z0-9_]*)",
        "mediatr handler base",
    )
});
/// `receiver.Send(argument` or `.Publish(` (`MEDIATR_DISPATCH_RE`).
static MEDIATR_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"([A-Za-z_][A-Za-z0-9_.]*)\s*\.\s*(?:Send|Publish)\s*\(\s*(new\s+[A-Z][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_]*)",
        "mediatr dispatch",
    )
});
/// A sent `new X`.
static MEDIATR_NEW: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"^new\s+([A-Z][A-Za-z0-9_]*)", "mediatr new"));
/// A sent identifier.
static IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("identifier"));
/// `Worker.perform_async`, `perform_in` or `perform_at`, the worker's name
/// qualified or not (`SIDEKIQ_DISPATCH_RE`).
static SIDEKIQ_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"([A-Z][A-Za-z0-9_]*(?:::[A-Z][A-Za-z0-9_]*)*)\s*\.\s*perform_(?:async|in|at)(?-u:\b)",
        "sidekiq dispatch",
    )
});
/// The cheap check a Ruby file passes before it is scanned.
static SIDEKIQ_ENQUEUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.perform_(?:async|in|at)(?-u:\b)").expect("sidekiq enqueue"));
/// A worker's mixin (`SIDEKIQ_WORKER_RE`).
static SIDEKIQ_WORKER: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)include\s+Sidekiq::(?:Job|Worker)(?-u:\b)",
        "sidekiq worker",
    )
});
/// `event(new X` (`LARAVEL_DISPATCH_RE`).
static LARAVEL_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)event\s*\(\s*new\s+\\?([A-Za-z_][A-Za-z0-9_\\]*)",
        "laravel dispatch",
    )
});
/// An event service provider's map, `$listen = [`.
static LARAVEL_LISTEN: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"\$listen\s*=\s*\[", "laravel listen"));
/// One `Event::class => [Listener::class, …]` entry of the map
/// (`LISTEN_ENTRY_RE`).
static LARAVEL_LISTEN_ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r#"(?:([A-Za-z_\\][A-Za-z0-9_\\]*)::class|'([^']+)'|"([^"]+)")\s*=>\s*\[([^\]]*)\]"#,
        "laravel listen entry",
    )
});
/// One class of an entry's list (`LISTEN_CLASS_RE`).
static LARAVEL_LISTEN_CLASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:([A-Za-z_\\][A-Za-z0-9_\\]*)::class|'([^']+)'|"([^"]+)")"#)
        .expect("laravel listen class")
});
/// A `handle` declaration's first parameter type (`laravelHandleEventTypes`).
static LARAVEL_HANDLE: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"function\s+handle\s*\(\s*(?:\.\.\.\s*)?(\??[A-Za-z_\\][A-Za-z0-9_\\|]*)\s+&?\s*(?:\.\.\.\s*)?\$",
        "laravel handle",
    )
});

/// The redux-thunk pass (`reduxThunkEdges`): a thunk constant
/// (`createAsyncThunk(…)` or a `createThunk(…)` wrapper, its async body an
/// argument and no node of its own) links to each thunk or action creator
/// its body dispatches, at most [`THUNK_FANOUT_CAP`] a constant.
pub(super) fn redux_thunk_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    // Every signature the pattern matches holds `Thunk`: the store returns a
    // superset of the constants upstream tests, and the test narrows it.
    for thunk in view.nodes_of_kind_whose_signature_holds(NodeKind::Constant, "Thunk")? {
        if !thunk
            .signature
            .as_deref()
            .is_some_and(|signature| THUNK_DECL.is_match(signature))
        {
            continue;
        }
        let Some(content) = view.text(&thunk.file_path) else {
            continue;
        };
        let Some(source) = slice_lines(&content, thunk.start_line, thunk.end_line) else {
            continue;
        };
        if source.is_empty() {
            continue;
        }
        let language = if matches!(thunk.language, Language::JavaScript | Language::Jsx) {
            CommentLang::JavaScript
        } else {
            CommentLang::TypeScript
        };
        let safe = strip_comments_for_regex(&source, language);
        let lines = LineAt::new(&safe, thunk.start_line);
        let mut added = 0;
        for captures in THUNK_DISPATCH.captures_iter(&safe) {
            if added >= THUNK_FANOUT_CAP {
                break;
            }
            let name = &captures[1];
            if name == thunk.name {
                continue;
            }
            // A thunk or an action creator, never a helper that only shares
            // the name: the first tier holding a candidate decides, a thunk,
            // then any constant, then one in the same file, then any, and
            // only its one candidate links; several leave the dispatch
            // unlinked (KEEP-RUST, OD-B14: upstream takes the tier's first).
            let candidates: Vec<Node> = view
                .nodes_named(name)?
                .into_iter()
                .filter(|node| {
                    matches!(
                        node.kind,
                        NodeKind::Constant | NodeKind::Function | NodeKind::Method
                    )
                })
                .collect();
            let tier = [
                candidates
                    .iter()
                    .filter(|node| {
                        node.signature
                            .as_deref()
                            .is_some_and(|signature| THUNK_DECL.is_match(signature))
                    })
                    .collect::<Vec<_>>(),
                candidates
                    .iter()
                    .filter(|node| node.kind == NodeKind::Constant)
                    .collect(),
                candidates
                    .iter()
                    .filter(|node| node.file_path == thunk.file_path)
                    .collect(),
                candidates.iter().collect(),
            ]
            .into_iter()
            .find(|tier| !tier.is_empty())
            .unwrap_or_default();
            let [target] = tier.as_slice() else {
                continue;
            };
            if target.id == thunk.id || !seen.insert((thunk.id.clone(), target.id.clone())) {
                continue;
            }
            let line = lines.line(captures.get(0).expect("match").start());
            edges.push(synthesized(
                &thunk,
                target,
                EdgeKind::Calls,
                line,
                json!({
                    "synthesizedBy": "redux-thunk",
                    "via": name,
                    "registeredAt": format!("{}:{line}", thunk.file_path),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// The indexed files of `language` whose path ends in `suffix`: what
/// upstream's walk of every file with the suffix test reads, since a path
/// with these suffixes is always indexed as that language.
fn files_ending(view: &View<'_>, language: Language, suffix: &str) -> Result<Vec<String>> {
    Ok(view
        .files_of_languages(&[language])?
        .into_iter()
        .filter(|file| file.ends_with(suffix))
        .collect())
}

/// The Celery pass (`celeryDispatchEdges`): the function around each
/// `task.delay(…)` or `task.apply_async(…)` links to the task, a Python
/// function under a task decorator, at most [`FILE_FANOUT_CAP`] a file.
pub(super) fn celery_dispatch_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut tasks: HashMap<String, bool> = HashMap::new();
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in files_ending(view, Language::Python, ".py")? {
        let Some(content) = view.text(&file) else {
            continue;
        };
        if content.is_empty()
            || (!content.contains(".delay(") && !content.contains(".apply_async("))
        {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Python);
        let lines = LineAt::new(&safe, 1);
        let nodes = view.callables_in_file(&file)?;
        let mut added = 0;
        for captures in CELERY_DISPATCH.captures_iter(&safe) {
            if added >= FILE_FANOUT_CAP {
                break;
            }
            let name = &captures[1];
            let line = lines.line(captures.get(0).expect("match").start());
            // A dispatch at module level has no symbol to come from.
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let candidates: Vec<Node> = view
                .nodes_named(name)?
                .into_iter()
                .filter(|node| {
                    node.kind == NodeKind::Function && is_celery_task(view, node, &mut tasks)
                })
                .collect();
            let target = match candidates.len() {
                0 => None,
                1 => candidates.first(),
                // Tasks of one name in several modules: the dispatching
                // file's, or none.
                _ => candidates
                    .iter()
                    .find(|candidate| candidate.file_path == file),
            };
            let Some(target) = target else {
                continue;
            };
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
                    "synthesizedBy": "celery-dispatch",
                    "via": name,
                    "registeredAt": format!("{file}:{line}"),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// Whether `node` is a Celery task: a Python function with a task decorator
/// among the [`CELERY_DECORATOR_LOOKBACK`] lines above its `def`, read
/// upward until the declaration before it. Decided once a node.
fn is_celery_task(view: &View<'_>, node: &Node, tasks: &mut HashMap<String, bool>) -> bool {
    if let Some(&task) = tasks.get(&node.id) {
        return task;
    }
    let mut task = false;
    if node.kind == NodeKind::Function
        && node.file_path.ends_with(".py")
        && let Some(content) = view.text(&node.file_path)
        && !content.is_empty()
    {
        let lines: Vec<&str> = content.split('\n').collect();
        task = decorated_as_task(&lines, node.start_line);
    }
    tasks.insert(node.id.clone(), task);
    task
}

/// Whether the lines above the `def` at 1-based `start` hold a task
/// decorator before an earlier `def` or `class`.
fn decorated_as_task(lines: &[&str], start: i64) -> bool {
    let stop = (start - 1 - CELERY_DECORATOR_LOOKBACK).max(0);
    let mut at = start - 2;
    while at >= stop {
        let line = js_trim(lines.get(at as usize).copied().unwrap_or(""));
        if CELERY_DECLARATION.is_match(line) {
            return false;
        }
        if CELERY_TASK_DECORATOR.is_match(line) {
            return true;
        }
        at -= 1;
    }
    false
}

/// The Spring pass (`springEventEdges`): the method around each
/// `publishEvent(new X(…))` links to every listener method of `X`, an
/// `@EventListener` or `@TransactionalEventListener` method or an
/// `ApplicationListener`'s `onApplicationEvent`, at most
/// [`FILE_FANOUT_CAP`] a file, a dispatch site's listeners all linked.
pub(super) fn spring_event_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut listeners: HashMap<String, Vec<Node>> = HashMap::new();
    let mut publishers: Vec<String> = Vec::new();
    for file in files_ending(view, Language::Java, ".java")? {
        let Some(content) = view.text(&file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        if content.contains(".publishEvent(") {
            publishers.push(file.clone());
        }
        let annotated_file =
            content.contains("@EventListener") || content.contains("@TransactionalEventListener");
        let application_listener = SPRING_APPLICATION_LISTENER.is_match(&content);
        if !annotated_file && !application_listener {
            continue;
        }
        let lines: Vec<&str> = content.split('\n').collect();
        for method in view.callables_in_file(&file)? {
            if method.kind != NodeKind::Method {
                continue;
            }
            // The method's own annotations: the `@` lines it starts with,
            // so nothing of the next method's is read.
            let mut annotations: Vec<&str> = Vec::new();
            let mut at = method.start_line - 1;
            while (at as usize) < lines.len() && at < method.start_line + 7 {
                let line = js_trim(lines[at as usize]);
                if !line.starts_with('@') {
                    break;
                }
                annotations.push(line);
                at += 1;
            }
            let head = annotations.join("\n");
            let annotated = annotated_file && SPRING_LISTENER_ANNOTATION.is_match(&head);
            let applied = application_listener && method.name == "onApplicationEvent";
            if !annotated && !applied {
                continue;
            }
            let mut event = spring_first_param_type(method.signature.as_deref());
            if event.is_none() && annotated {
                event = SPRING_ANNOTATED_TYPE
                    .captures(&head)
                    .map(|captures| captures[1].to_string());
            }
            let Some(event) = event else {
                continue;
            };
            listeners.entry(event).or_default().push(method);
        }
    }
    let mut edges = Vec::new();
    if listeners.is_empty() {
        return Ok(edges);
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in &publishers {
        let Some(content) = view.text(file) else {
            continue;
        };
        if content.is_empty() || !content.contains(".publishEvent(") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Java);
        let lines = LineAt::new(&safe, 1);
        let nodes = view.callables_in_file(file)?;
        let mut added = 0;
        for captures in SPRING_PUBLISH.captures_iter(&safe) {
            if added >= FILE_FANOUT_CAP {
                break;
            }
            let event = &captures[1];
            let Some(targets) = listeners.get(event).filter(|targets| !targets.is_empty()) else {
                continue;
            };
            let line = lines.line(captures.get(0).expect("match").start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            for target in targets {
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
                        "synthesizedBy": "spring-event",
                        "via": event,
                        "registeredAt": format!("{file}:{line}"),
                    }),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// The first parameter's type in a Java method's signature
/// (`springFirstParamType`): `void (XEvent e)` gives `XEvent`. A leading
/// `final` or annotation is skipped and generic arguments dropped; a
/// method without parameters, or whose first is not a class, gives none.
fn spring_first_param_type(signature: Option<&str>) -> Option<String> {
    let signature = signature.filter(|signature| !signature.is_empty())?;
    let open = signature.find('(')?;
    let close = signature[open..]
        .find(')')
        .map_or(signature.len(), |at| open + at);
    let inner = js_trim(&signature[open + 1..close]);
    if inner.is_empty() {
        return None;
    }
    let first = js_trim(inner.split(',').next().unwrap_or_default());
    let tokens: Vec<&str> = first
        .split(is_js_space)
        .filter(|token| !token.is_empty() && *token != "final" && !token.starts_with('@'))
        .collect();
    if tokens.len() < 2 {
        return None;
    }
    let ty = tokens[tokens.len() - 2];
    let ty = ty.split('<').next().unwrap_or(ty);
    CLASS_NAME.is_match(ty).then(|| ty.to_string())
}

/// The MediatR pass (`mediatrDispatchEdges`): the method around each
/// mediator `Send(x)` or `Publish(x)` links to the `Handle` method of each
/// handler of `x`'s type, at most [`FILE_FANOUT_CAP`] a file, a dispatch
/// site's handlers all linked.
pub(super) fn mediatr_dispatch_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let files = files_ending(view, Language::CSharp, ".cs")?;
    let mut handlers: HashMap<String, Vec<Node>> = HashMap::new();
    for file in &files {
        let Some(content) = view.text(file) else {
            continue;
        };
        if content.is_empty()
            || (!content.contains("IRequestHandler<") && !content.contains("INotificationHandler<"))
        {
            continue;
        }
        let lines: Vec<&str> = content.split('\n').collect();
        let nodes = view.nodes_in_file(file)?;
        for class in nodes.iter().filter(|node| node.kind == NodeKind::Class) {
            let head = lines
                .iter()
                .skip((class.start_line - 1).max(0) as usize)
                .take(MEDIATR_HANDLER_DECL_LOOKAHEAD)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            let Some(captures) = MEDIATR_HANDLER_BASE.captures(&head) else {
                continue;
            };
            let Some(handle) = nodes.iter().find(|node| {
                node.kind == NodeKind::Method
                    && node.name == "Handle"
                    && node.start_line >= class.start_line
                    && node.start_line <= class.end_line
            }) else {
                continue;
            };
            handlers
                .entry(captures[1].to_string())
                .or_default()
                .push(handle.clone());
        }
    }
    let mut edges = Vec::new();
    if handlers.is_empty() {
        return Ok(edges);
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut patterns = SentTypePatterns::default();
    for file in &files {
        let Some(content) = view.text(file) else {
            continue;
        };
        if content.is_empty() || (!content.contains(".Send(") && !content.contains(".Publish(")) {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::CSharp);
        let safe_lines: Vec<&str> = safe.split('\n').collect();
        let lines = LineAt::new(&safe, 1);
        let nodes = view.callables_in_file(file)?;
        let mut added = 0;
        for captures in MEDIATR_DISPATCH.captures_iter(&safe) {
            if added >= FILE_FANOUT_CAP {
                break;
            }
            // A mediator, never `MessagingCenter.Send` or an HTTP client.
            if !mediator_receiver(&captures[1]) {
                continue;
            }
            let line = lines.line(captures.get(0).expect("match").start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let Some(request) =
                patterns.sent_type(&captures[2], &safe_lines, dispatcher.start_line, line)
            else {
                continue;
            };
            let Some(targets) = handlers.get(&request) else {
                continue;
            };
            for target in targets {
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
                        "synthesizedBy": "mediatr-dispatch",
                        "via": request,
                        "registeredAt": format!("{file}:{line}"),
                    }),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// Whether a `Send` or `Publish` receiver is a mediator (upstream's
/// `/(?:mediator|sender|publisher)/i` over an ASCII name).
fn mediator_receiver(receiver: &str) -> bool {
    let receiver = receiver.to_ascii_lowercase();
    ["mediator", "sender", "publisher"]
        .iter()
        .any(|name| receiver.contains(name))
}

/// The two patterns that type a sent identifier, compiled once a name.
#[derive(Default)]
struct SentTypePatterns {
    by_name: HashMap<String, (Regex, Regex)>,
}

impl SentTypePatterns {
    /// The type a MediatR site sends (`resolveMediatrArgType`): an inline
    /// `new X`'s `X`; for an identifier, read from the 1-based line `start`
    /// of the method through the dispatch line, the first `arg = new X`'s
    /// `X`, else the first `X arg` declaration's.
    fn sent_type(
        &mut self,
        argument: &str,
        lines: &[&str],
        start: i64,
        dispatch_line: i64,
    ) -> Option<String> {
        if let Some(captures) = MEDIATR_NEW.captures(argument) {
            return Some(captures[1].to_string());
        }
        if !IDENTIFIER.is_match(argument) {
            return None;
        }
        // The argument holds identifier characters only, so it goes into
        // the patterns as it is.
        let (assigned, declared) = self.by_name.entry(argument.to_string()).or_insert_with(|| {
            (
                js_regex(
                    &format!(r"(?-u:\b){argument}(?-u:\b)\s*=\s*new\s+([A-Z][A-Za-z0-9_]*)"),
                    "mediatr assignment",
                ),
                js_regex(
                    &format!(r"(?-u:\b)([A-Z][A-Za-z0-9_]*)(?-u:\b)\s+{argument}(?-u:\b)"),
                    "mediatr declaration",
                ),
            )
        });
        let mut declaration = None;
        let mut at = (start - 1).max(0);
        while at < dispatch_line && (at as usize) < lines.len() {
            let line = lines[at as usize];
            if let Some(captures) = assigned.captures(line) {
                return Some(captures[1].to_string());
            }
            if declaration.is_none() {
                declaration = declared
                    .captures(line)
                    .map(|captures| captures[1].to_string());
            }
            at += 1;
        }
        declaration
    }
}

/// The Sidekiq pass (`sidekiqDispatchEdges`): the method around each
/// `Worker.perform_async`, `perform_in` or `perform_at` links to the
/// worker's `perform`, at most [`FILE_FANOUT_CAP`] a file.
pub(super) fn sidekiq_dispatch_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut performs: HashMap<String, Option<Node>> = HashMap::new();
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in files_ending(view, Language::Ruby, ".rb")? {
        let Some(content) = view.text(&file) else {
            continue;
        };
        if content.is_empty() || !SIDEKIQ_ENQUEUE.is_match(&content) {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Ruby);
        let lines = LineAt::new(&safe, 1);
        let nodes = view.callables_in_file(&file)?;
        let mut added = 0;
        for captures in SIDEKIQ_DISPATCH.captures_iter(&safe) {
            if added >= FILE_FANOUT_CAP {
                break;
            }
            let line = lines.line(captures.get(0).expect("match").start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let worker = &captures[1];
            let Some(target) = sidekiq_perform(view, worker, &mut performs)? else {
                continue;
            };
            if target.id == dispatcher.id
                || !seen.insert((dispatcher.id.clone(), target.id.clone()))
            {
                continue;
            }
            edges.push(synthesized(
                dispatcher,
                &target,
                EdgeKind::Calls,
                line,
                json!({
                    "synthesizedBy": "sidekiq-dispatch",
                    "via": worker,
                    "registeredAt": format!("{file}:{line}"),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

/// The `perform` of the worker `reference` names: by exact qualified name
/// first when it is qualified, then by its last segment, each when exactly
/// one worker class has it, so same-named workers of other modules stay
/// apart. Several classes of the qualified name leave it unlinked, even when
/// their `perform` is one node (KEEP-RUST, OD-B14: upstream takes the
/// first).
fn sidekiq_perform(
    view: &View<'_>,
    reference: &str,
    performs: &mut HashMap<String, Option<Node>>,
) -> Result<Option<Node>> {
    if reference.contains("::") {
        let mut qualified = Vec::new();
        for class in view.nodes_qualified(reference)? {
            if class.kind == NodeKind::Class
                && let Some(perform) = perform_of(view, &class, performs)?
            {
                qualified.push(perform);
            }
        }
        match qualified.len() {
            0 => {}
            1 => return Ok(qualified.pop()),
            _ => return Ok(None),
        }
    }
    let last = reference.rsplit("::").next().unwrap_or(reference);
    let mut workers = Vec::new();
    for class in view.nodes_named(last)? {
        if class.kind == NodeKind::Class
            && let Some(perform) = perform_of(view, &class, performs)?
        {
            workers.push(perform);
        }
    }
    Ok(if workers.len() == 1 {
        workers.pop()
    } else {
        None
    })
}

/// A Sidekiq worker's instance `perform`: the class's text includes
/// `Sidekiq::Job` or `Sidekiq::Worker` (the gem's module forms no edge), and
/// the method is the first `perform` of its file inside the class. Decided
/// once a class.
fn perform_of(
    view: &View<'_>,
    class: &Node,
    performs: &mut HashMap<String, Option<Node>>,
) -> Result<Option<Node>> {
    if let Some(perform) = performs.get(&class.id) {
        return Ok(perform.clone());
    }
    let mut perform = None;
    if let Some(content) = view.text(&class.file_path)
        && !content.is_empty()
    {
        let body = content
            .split('\n')
            .skip((class.start_line - 1).max(0) as usize)
            .take((class.end_line - class.start_line + 1).max(0) as usize)
            .collect::<Vec<_>>()
            .join("\n");
        if SIDEKIQ_WORKER.is_match(&body) {
            perform = first_method_in(view, class, "perform")?;
        }
    }
    performs.insert(class.id.clone(), perform.clone());
    Ok(perform)
}

/// The first method named `name` of `class`'s file that starts inside the
/// class.
fn first_method_in(view: &View<'_>, class: &Node, name: &str) -> Result<Option<Node>> {
    Ok(view
        .callables_in_file(&class.file_path)?
        .into_iter()
        .find(|node| {
            node.kind == NodeKind::Method
                && node.name == name
                && node.start_line >= class.start_line
                && node.start_line <= class.end_line
        }))
}

/// The Laravel pass (`laravelEventEdges`): the method around each
/// `event(new X(…))` links to every listener's `handle` for `X`, a listener
/// a `handle(X $event)` declares or an event service provider's `$listen`
/// map registers, at most [`LARAVEL_FANOUT_CAP`] a file, a dispatch site's
/// listeners all linked. A queued job's `dispatch` is no event.
pub(super) fn laravel_event_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let files = files_ending(view, Language::Php, ".php")?;
    // Each event's listeners, a listener once, in the order first recorded.
    let mut listeners: HashMap<String, Vec<Node>> = HashMap::new();
    let mut listen = |event: String, handle: Node| {
        let handles = listeners.entry(event).or_default();
        if !handles.iter().any(|known| known.id == handle.id) {
            handles.push(handle);
        }
    };
    for file in &files {
        let Some(content) = view.text(file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        if content.contains("function handle") {
            let lines: Vec<&str> = content.split('\n').collect();
            for method in view.callables_in_file(file)? {
                if method.kind != NodeKind::Method || method.name != "handle" {
                    continue;
                }
                let declaration = lines
                    .iter()
                    .skip((method.start_line - 1).max(0) as usize)
                    .take(3)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                for event in laravel_handle_event_types(&declaration) {
                    listen(event, method.clone());
                }
            }
        }
        if content.contains("$listen") {
            // Read without comments, so a map commented out registers
            // nothing.
            let safe = strip_comments_for_regex(&content, CommentLang::Php);
            let body = LARAVEL_LISTEN.find(&safe).and_then(|found| {
                let open = found.start() + safe[found.start()..].find('[')?;
                php_array_body(&safe, open)
            });
            if let Some(body) = body {
                for entry in LARAVEL_LISTEN_ENTRY.captures_iter(body) {
                    let event = php_simple_name(first_group(&entry));
                    for listed in LARAVEL_LISTEN_CLASS.captures_iter(&entry[4]) {
                        let name = php_simple_name(first_group(&listed));
                        for class in view.nodes_named(&name)? {
                            if class.kind != NodeKind::Class {
                                continue;
                            }
                            if let Some(handle) = first_method_in(view, &class, "handle")? {
                                listen(event.clone(), handle);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut edges = Vec::new();
    if listeners.is_empty() {
        return Ok(edges);
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for file in &files {
        let Some(content) = view.text(file) else {
            continue;
        };
        if content.is_empty() || !content.contains("event(") {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Php);
        let lines = LineAt::new(&safe, 1);
        let nodes = view.callables_in_file(file)?;
        let mut added = 0;
        for captures in LARAVEL_DISPATCH.captures_iter(&safe) {
            if added >= LARAVEL_FANOUT_CAP {
                break;
            }
            let event = php_simple_name(&captures[1]);
            let Some(targets) = listeners.get(&event) else {
                continue;
            };
            let line = lines.line(captures.get(0).expect("match").start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            for target in targets {
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
                        "synthesizedBy": "laravel-event",
                        "via": event,
                        "registeredAt": format!("{file}:{line}"),
                    }),
                ));
                added += 1;
            }
        }
    }
    Ok(edges)
}

/// The text of the first of a `$listen` pattern's three alternatives that
/// matched (`m[1] ?? m[2] ?? m[3] ?? ''`).
fn first_group<'t>(captures: &regex::Captures<'t>) -> &'t str {
    (1..=3)
        .find_map(|group| captures.get(group))
        .map_or("", |found| found.as_str())
}

/// The class types of a `handle(…)` declaration's first parameter
/// (`laravelHandleEventTypes`): a union split, each short-named, the
/// primitives dropped. `handle(A|B $e)` gives `A` and `B`; `handle(string
/// $x)` and `handle()` give none.
fn laravel_handle_event_types(declaration: &str) -> Vec<String> {
    let Some(captures) = LARAVEL_HANDLE.captures(declaration) else {
        return Vec::new();
    };
    let types = &captures[1];
    let types = types.strip_prefix('?').unwrap_or(types);
    types
        .split('|')
        .map(php_simple_name)
        .filter(|name| CLASS_NAME.is_match(name))
        .collect()
}

/// A PHP class reference's short name (`phpSimpleName`): `\App\Events\Foo`
/// and `App\Events::Foo` give `Foo`.
fn php_simple_name(reference: &str) -> String {
    let reference = reference.strip_prefix('\\').unwrap_or(reference);
    let last = reference.rsplit('\\').next().unwrap_or(reference);
    let last = last.rsplit("::").next().unwrap_or(last);
    js_trim(last).to_string()
}

/// The text between the `[` at `open` and its matching `]` (`phpArrayBody`):
/// brackets counted only, nothing when they never balance.
fn php_array_body(source: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    for (at, byte) in source.bytes().enumerate().skip(open) {
        if byte == b'[' {
            depth += 1;
        } else if byte == b']' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(&source[open + 1..at]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spring_listeners_event_is_its_first_parameters_class() {
        let cases: [(Option<&str>, Option<&str>); 13] = [
            (
                Some("void (PasswordChangedEvent event)"),
                Some("PasswordChangedEvent"),
            ),
            (
                Some("void (final OrderShippedEvent event)"),
                Some("OrderShippedEvent"),
            ),
            (
                Some("void (@Valid OrderShippedEvent event, String other)"),
                Some("OrderShippedEvent"),
            ),
            (
                Some("void (GenericEvent<Order> event)"),
                Some("GenericEvent"),
            ),
            (Some("void ( \u{A0}Spaced\u{2003} e )"), Some("Spaced")),
            // A generic argument list's comma ends the first parameter.
            (Some("void (Map<String, X> m)"), None),
            (Some("void ()"), None),
            (Some("void (int count)"), None),
            (Some("void (event)"), None),
            (Some("void"), None),
            (Some(""), None),
            (None, None),
            // No closing parenthesis: the rest of the signature.
            (Some("void (Unclosed e"), Some("Unclosed")),
        ];
        for (signature, expected) in cases {
            assert_eq!(
                spring_first_param_type(signature).as_deref(),
                expected,
                "{signature:?}"
            );
        }
    }

    #[test]
    fn a_laravel_handle_declares_the_events_of_its_first_parameter() {
        let cases: [(&str, &[&str]); 10] = [
            (
                "public function handle(SongLiked $event): void {}",
                &["SongLiked"],
            ),
            (
                "public function handle(OwnerTest|UserTest $event)",
                &["OwnerTest", "UserTest"],
            ),
            ("public function handle(?SongLiked $event)", &["SongLiked"]),
            (
                "public function handle(\\App\\Events\\ScanDone $event)",
                &["ScanDone"],
            ),
            ("public function handle(SongLiked &$event)", &["SongLiked"]),
            (
                "public function handle(SongLiked ...$events)",
                &["SongLiked"],
            ),
            ("public function handle(string $name)", &[]),
            ("public function handle(): void {}", &[]),
            ("public function handleSongLiked(SongLiked $e)", &[]),
            (
                "public function handle(\n    SongLiked $event\n)",
                &["SongLiked"],
            ),
        ];
        for (declaration, expected) in cases {
            assert_eq!(
                laravel_handle_event_types(declaration),
                expected,
                "{declaration}"
            );
        }
    }

    #[test]
    fn a_php_reference_is_named_by_its_last_segment() {
        for (reference, expected) in [
            ("\\App\\Events\\Foo", "Foo"),
            ("App\\Events\\Foo", "Foo"),
            ("App\\Events::Foo", "Foo"),
            ("Foo", "Foo"),
            ("  Foo  ", "Foo"),
            ("\\\\Foo", "Foo"),
            ("", ""),
        ] {
            assert_eq!(php_simple_name(reference), expected, "{reference:?}");
        }
    }

    #[test]
    fn a_php_array_body_ends_at_its_matching_bracket() {
        let source = "x = [ A::class => [B::class, [C]], ]; [later]";
        let open = source.find('[').expect("open");
        assert_eq!(
            php_array_body(source, open),
            Some(" A::class => [B::class, [C]], ")
        );
        assert_eq!(php_array_body("[ never closed", 0), None);
        assert_eq!(php_array_body("[]", 0), Some(""));
        assert_eq!(php_array_body("[ ünïcödé ]", 0), Some(" ünïcödé "));
    }

    #[test]
    fn a_sent_identifier_is_typed_by_its_assignment_then_its_declaration() {
        let lines = [
            "public async Task Delete(int id) {",
            "    DeleteThingCommand command;",
            "    command = new OtherCommand(id);",
            "    await _mediator.Send(command);",
            "}",
        ];
        let mut patterns = SentTypePatterns::default();
        // The assignment wins over the earlier declaration.
        assert_eq!(
            patterns.sent_type("command", &lines, 1, 4).as_deref(),
            Some("OtherCommand")
        );
        // Without the assignment in reach, the declaration.
        assert_eq!(
            patterns.sent_type("command", &lines, 1, 2).as_deref(),
            Some("DeleteThingCommand")
        );
        // The dispatch line itself is read.
        let one_line = ["void Run() { var x = new Ping(); _mediator.Send(x); }"];
        assert_eq!(
            patterns.sent_type("x", &one_line, 1, 1).as_deref(),
            Some("Ping")
        );
        assert_eq!(
            patterns
                .sent_type("new GetThingsQuery", &lines, 1, 4)
                .as_deref(),
            Some("GetThingsQuery")
        );
        assert_eq!(patterns.sent_type("unknown", &lines, 1, 4), None);
        // `new lower(…)` is read as the identifier `new`, which nothing types.
        assert_eq!(patterns.sent_type("new", &lines, 1, 4), None);
        assert!(!mediator_receiver("MessagingCenter"));
        assert!(mediator_receiver("_mediator"));
        assert!(mediator_receiver("this.Sender"));
        assert!(mediator_receiver("eventPublisher"));
    }

    #[test]
    fn a_celery_task_has_a_task_decorator_above_its_def() {
        let lines: Vec<&str> = [
            "from celery import shared_task", // 1
            "",                               // 2
            "@shared_task",                   // 3
            "def send_email(to):",            // 4
            "    return to",                  // 5
            "@app.task(bind=True)",           // 6
            "# a comment",                    // 7
            "def crunch(self, n):",           // 8
            "    return n",                   // 9
            "def plain():",                   // 10
            "    pass",                       // 11
            "@mytask",                        // 12
            "def other():",                   // 13
            "\t@celery_app.task",             // 14
            "async def later():",             // 15
        ]
        .to_vec();
        assert!(decorated_as_task(&lines, 4));
        assert!(decorated_as_task(&lines, 8));
        // The task decorator above `crunch`'s `def` belongs to it, not to
        // `plain`: reading up stops at `crunch`'s `def`.
        assert!(!decorated_as_task(&lines, 10));
        assert!(!decorated_as_task(&lines, 13));
        assert!(decorated_as_task(&lines, 15));
        assert!(!decorated_as_task(&lines, 1));
        // Thirteen lines down, the decorator is out of reach.
        let mut far = vec!["@shared_task"];
        far.extend(std::iter::repeat_n("", 12));
        far.push("def far():");
        assert!(!decorated_as_task(&far, 14));
        far.remove(1);
        assert!(decorated_as_task(&far, 13));
    }
}
