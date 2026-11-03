//! The Go dispatch passes (upstream `goGrpcStubImplEdges` and
//! `ginMiddlewareChainEdges` at `e8ddc4e2`, W7-15c): a generated gRPC server
//! stub's methods to the hand-written server's, and gin's handler-chain
//! dispatcher to the named handlers its registrations pass.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::file_class::is_generated_file;
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::json;

use super::MAX_CALLBACKS_PER_CHANNEL;
use super::dispatch::{methods_of_each, place, synthesized};
use super::stage::{View, slice_lines};
use crate::frameworks::js_scan::{JS_SPACE, js_trim};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};

/// A protoc-gen-go-grpc server stub's name (upstream `STUB_RE`).
static STUB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Unimplemented.*Server$").expect("the stub pattern compiles"));

/// Whether `node` is a protoc-gen-go-grpc server stub: named as one and in a
/// generated file, so a hand-written struct of the name is none.
fn is_stub(node: &Node) -> bool {
    STUB.is_match(&node.name) && is_generated_file(&node.file_path)
}

/// The stub's methods that are no RPC: gRPC's embedding markers (upstream
/// `isInternalMarker`).
fn is_internal_marker(name: &str) -> bool {
    name.starts_with("mustEmbed") || name == "testEmbeddedByValue"
}

/// gRPC stub dispatch (upstream `goGrpcStubImplEdges`): protoc-gen-go-grpc
/// emits an `UnimplementedXxxServer` struct in a generated file with one
/// method per RPC; the server is a hand-written struct elsewhere, which Go's
/// structural typing never names. Each method of a stub links to the
/// method of the same name of every struct outside generated files that
/// declares all the stub's RPC names, at most [`MAX_CALLBACKS_PER_CHANNEL`]
/// a stub and struct: a service often has a production server and test
/// doubles, and linking to all keeps a trace through either.
pub(super) fn grpc_stub_impl_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let structs = view.nodes_of_kinds_in(&[NodeKind::Struct], &[Language::Go])?;
    // Without a stub nothing links, so no method is read.
    if !structs.iter().any(is_stub) {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = structs.iter().map(|node| node.id.clone()).collect();
    let methods = methods_of_each(view, &ids)?;
    let names: HashMap<&str, HashSet<&str>> = methods
        .iter()
        .map(|(id, methods)| {
            (
                id.as_str(),
                methods.iter().map(|method| method.name.as_str()).collect(),
            )
        })
        .collect();
    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for stub in &structs {
        if !is_stub(stub) {
            continue;
        }
        let rpcs: Vec<&Node> = methods[&stub.id]
            .iter()
            .filter(|method| !is_internal_marker(&method.name))
            .collect();
        if rpcs.is_empty() {
            continue;
        }
        for server in &structs {
            if server.id == stub.id || is_generated_file(&server.file_path) {
                continue;
            }
            let declared = &names[server.id.as_str()];
            if !rpcs.iter().all(|rpc| declared.contains(rpc.name.as_str())) {
                continue;
            }
            let mut added = 0;
            'rpcs: for rpc in &rpcs {
                for method in &methods[&server.id] {
                    if added >= MAX_CALLBACKS_PER_CHANNEL {
                        break 'rpcs;
                    }
                    if method.name != rpc.name || !seen.insert((rpc.id.clone(), method.id.clone()))
                    {
                        continue;
                    }
                    edges.push(synthesized(
                        rpc,
                        method,
                        EdgeKind::Calls,
                        rpc.start_line,
                        json!({
                            "synthesizedBy": "go-grpc-stub-impl",
                            "via": method.name,
                            "registeredAt": place(method),
                        }),
                    ));
                    added += 1;
                }
            }
        }
    }
    Ok(edges)
}

/// A Go method body invoking a `handlers` slice by index, gin's chain
/// dispatcher `c.handlers[c.index](c)` (upstream `GIN_DISPATCH_RE`).
static GIN_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&r"\.handlers\s*\[[^\]]*\]\s*\(".replace(r"\s", JS_SPACE))
        .expect("the dispatch pattern compiles")
});

/// A gin registration call (upstream `GIN_REG_RE`).
static GIN_REGISTRATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        &r"\.(?:Use|GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\s*\("
            .replace(r"\s", JS_SPACE),
    )
    .expect("the registration pattern compiles")
});

/// The registration prefilter's verb calls (upstream's
/// `/\.(?:GET|…|Handle)\(/`, with no space before the parenthesis).
static GIN_VERB_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(?:GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\(")
        .expect("the verb-call pattern compiles")
});

/// A trailing empty call (upstream's `/\(\s*\)$/`).
static EMPTY_CALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&r"\(\s*\)$".replace(r"\s", JS_SPACE)).expect("the empty-call pattern compiles")
});

/// The identifier an argument ends with (upstream's `/(?:\.|^)([A-Za-z_]\w*)$/`,
/// whose `\w` is ASCII).
static TAIL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$").expect("the tail-name pattern compiles")
});

/// The text between the `(` at `open` and its matching `)`, counting
/// parentheses only (upstream `goBalancedArgs`); `None` when it never
/// closes.
fn balanced_args(text: &str, open: usize) -> Option<&str> {
    let mut depth: i64 = 0;
    for (at, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..at]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A comma list split at its top level, nesting in `()`, `[]` and `{}`
/// (upstream `goSplitArgs`): every piece but the last is kept, the last
/// only when it is not blank.
fn split_args(args: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut depth: i64 = 0;
    let mut start = 0;
    for (at, byte) in args.bytes().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                pieces.push(&args[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    let last = &args[start..];
    if !js_trim(last).is_empty() {
        pieces.push(last);
    }
    pieces
}

/// The handler a registration argument names (upstream `goHandlerIdent`):
/// `gin.Logger()` names `Logger`, `mw` names `mw`; a string or a function
/// literal names none.
fn handler_name(argument: &str) -> Option<&str> {
    let trimmed = js_trim(argument);
    let cleaned = match EMPTY_CALL.find(trimmed) {
        Some(call) => &trimmed[..call.start()],
        None => trimmed,
    };
    if cleaned.is_empty()
        || cleaned.starts_with('"')
        || cleaned.starts_with('`')
        || cleaned.starts_with("func")
    {
        return None;
    }
    TAIL_NAME
        .captures(cleaned)
        .and_then(|captures| captures.get(1))
        .map(|name| name.as_str())
}

/// The Go function or method `name` means at a registration in `file`: the
/// one of that name in `file`, else the one in the project, nothing when
/// either holds several (KEEP-RUST, as `callback::unique_handler`; upstream
/// takes the first of `getNodesByName`).
fn unique_go_handler(view: &View<'_>, name: &str, file: &str) -> Result<Option<Node>> {
    let handlers: Vec<Node> = view
        .nodes_named(name)?
        .into_iter()
        .filter(|node| {
            node.language == Language::Go
                && matches!(node.kind, NodeKind::Function | NodeKind::Method)
        })
        .collect();
    let in_file: Vec<&Node> = handlers
        .iter()
        .filter(|node| node.file_path == file)
        .collect();
    Ok(match (in_file.len(), handlers.len()) {
        (1, _) => Some(in_file[0].clone()),
        (0, 1) => Some(handlers[0].clone()),
        _ => None,
    })
}

/// Gin's middleware chain (upstream `ginMiddlewareChainEdges`): gin runs
/// every handler through `c.handlers[c.index](c)` in `(*Context).Next`, a
/// computed call no reference names, and the chain is built from the
/// functions passed to `.Use`, `.GET` and the other registrations. The
/// dispatcher links to each named handler a registration passes, at most
/// [`MAX_CALLBACKS_PER_CHANNEL`] a dispatcher; with no dispatcher the pass
/// reads no registration.
pub(super) fn gin_middleware_chain_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let mut dispatchers = Vec::new();
    for method in view.nodes_of_kinds_in(&[NodeKind::Method], &[Language::Go])? {
        let Some(content) = view.text(&method.file_path) else {
            continue;
        };
        // The pattern needs `.handlers`, so a file without it is not sliced.
        if !content.contains(".handlers") {
            continue;
        }
        if slice_lines(&content, method.start_line, method.end_line)
            .is_some_and(|body| GIN_DISPATCH.is_match(&body))
        {
            dispatchers.push(method);
        }
    }
    if dispatchers.is_empty() {
        return Ok(Vec::new());
    }

    // Each handler name with the place of its first registration.
    let mut registered: Vec<(String, String, i64)> = Vec::new();
    let mut names: HashSet<String> = HashSet::new();
    for file in view.files()? {
        if !file.ends_with(".go") {
            continue;
        }
        let Some(content) = view.text(&file) else {
            continue;
        };
        if !content.contains(".Use(") && !GIN_VERB_CALL.is_match(&content) {
            continue;
        }
        let safe = strip_comments_for_regex(&content, CommentLang::Go);
        for call in GIN_REGISTRATION.find_iter(&safe) {
            let Some(args) = balanced_args(&safe, call.end() - 1) else {
                continue;
            };
            let line = safe[..call.start()].matches('\n').count() as i64 + 1;
            for argument in split_args(args) {
                if let Some(name) = handler_name(argument)
                    && names.insert(name.to_string())
                {
                    registered.push((name.to_string(), file.clone(), line));
                }
            }
        }
    }
    if registered.is_empty() {
        return Ok(Vec::new());
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut chosen: HashMap<&str, Option<Node>> = HashMap::new();
    for dispatcher in &dispatchers {
        let mut added = 0;
        for (name, file, line) in &registered {
            if added >= MAX_CALLBACKS_PER_CHANNEL {
                break;
            }
            let handler = match chosen.entry(name.as_str()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(unique_go_handler(view, name, file)?),
            };
            let Some(handler) = handler.as_ref() else {
                continue;
            };
            if handler.id == dispatcher.id
                || !seen.insert((dispatcher.id.clone(), handler.id.clone()))
            {
                continue;
            }
            edges.push(synthesized(
                dispatcher,
                handler,
                EdgeKind::Calls,
                dispatcher.start_line,
                json!({
                    "synthesizedBy": "gin-middleware-chain",
                    "via": name,
                    "registeredAt": format!("{file}:{line}"),
                }),
            ));
            added += 1;
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_args_count_parentheses_only() {
        let text = "r.GET(\"/a\", h(x), m)";
        assert_eq!(balanced_args(text, 5), Some("\"/a\", h(x), m"));
        assert_eq!(balanced_args("e.Use(a, (b)", 5), None);
        // Brackets and braces do not count, strings neither.
        assert_eq!(balanced_args("e.Use([)], c)", 5), Some("["));
        assert_eq!(balanced_args("e.Use(\")\", c)", 5), Some("\""));
    }

    #[test]
    fn split_args_keep_nested_commas_and_drop_a_blank_tail() {
        assert_eq!(split_args("a, b"), ["a", " b"]);
        assert_eq!(
            split_args("f(a, b), []int{1, 2}, m[k, v]"),
            ["f(a, b)", " []int{1, 2}", " m[k, v]"]
        );
        assert_eq!(split_args("a, b, "), ["a", " b"]);
        assert_eq!(split_args(", a"), ["", " a"]);
        assert!(split_args("  ").is_empty());
        // An unbalanced closer leaves the rest one level down.
        assert_eq!(split_args("a), b"), ["a), b"]);
    }

    #[test]
    fn handler_name_reads_the_tail_identifier() {
        assert_eq!(handler_name(" Logger"), Some("Logger"));
        assert_eq!(handler_name("gin.Logger()"), Some("Logger"));
        assert_eq!(handler_name("gin.Logger( )"), Some("Logger"));
        assert_eq!(handler_name("mw.Auth\u{a0}"), Some("Auth"));
        assert_eq!(handler_name("\"/users\""), None);
        assert_eq!(handler_name("`/raw`"), None);
        assert_eq!(handler_name("func(c *Context) {}"), None);
        assert_eq!(handler_name("handlers..."), None);
        assert_eq!(handler_name("auth(cfg)"), None);
        assert_eq!(handler_name("x ()"), None);
        assert_eq!(handler_name("h2"), Some("h2"));
        // The name follows a dot or nothing.
        assert_eq!(handler_name("*mw"), None);
        assert_eq!(handler_name("2fa"), None);
        assert_eq!(handler_name("é"), None);
        assert_eq!(handler_name(""), None);
    }
}
