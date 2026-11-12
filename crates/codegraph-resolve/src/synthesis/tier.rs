//! Cross-tier channels and test requests (upstream
//! `resolution/tier-synthesizer.ts` at `e8ddc4e2`, W7-20): the web's
//! equivalent of a native bridge.
//!
//! A web app is programs that talk over a wire the graph cannot see. Each
//! hop is a string on both sides, which is the evidence that closes it:
//!
//! - **`http-client`**: a literal path in a client call (`fetch`, `axios`,
//!   `ky`, `got`, `$fetch`, `useFetch`, `useSWR`, a project instance made by
//!   `axios.create(…)` or `ky.extend(…)`) reaches the one route node
//!   `METHOD path` it denotes. A template hole fills a route parameter, a
//!   hole in front of the path (`${API_URL}/users`) matches a route by its
//!   tail, and a variable URL, a path no route serves or a path two routes
//!   serve alike draws nothing.
//! - **`queue-job`**: `queue.add('job', …)` on a named queue reaches the
//!   `@Process('job')` method of the `@Processor('queue')` class, a worker
//!   host's `process`, a `new Worker('queue', handler)` or Bull's
//!   `queue.process('job', handler)`.
//! - **`event-bus`**: `eventEmitter.emit('user.created')` reaches
//!   `@OnEvent('user.created')` (globs honoured), and sockets in both
//!   directions: a client's `socket.emit('x')` reaches the server's
//!   `@SubscribeMessage('x')` or `socket.on('x', …)` (`client→server`), the
//!   server's `server.emit('x')` the client's `socket.on('x', …)`
//!   (`server→client`). The in-process `.on`/`.emit` pairing stays the
//!   emitter pass's.
//! - **`test-request`** ([`test_request_edges`]): a Spring or Laravel
//!   test's request (`mockMvc.perform(post("/owners/new"))`,
//!   `$this->postJson('api/me')`) reaches the route it names, from the test
//!   method (the file, for a Pest closure). These are the one kind of edge
//!   whose source is a test; the viewer's fan-in leaves them out.
//!
//! Every edge is `calls`, `provenance: "heuristic"`, with its line and
//! column, and metadata naming the pass, the `channel` (`http`, `queue`,
//! `event` or `socket`), the `callee` as written, the `tier` when the
//! direction is known, the `method`, `href`, `event` or `queue` it was
//! paired on, and `registeredAt`, where the other side was wired up.
//! Ambiguity stays unresolved: a tied route, a handler name with several
//! candidates, more than six queue candidates, dispatches or handlers of one
//! event, a call outside every function.
//!
//! Sources are the indexed JavaScript and TypeScript files by path, never a
//! test or a generated file; the Spring and Laravel test files for the test
//! pass. Each is read through [`View::text`] and matched on its
//! comment-stripped text, aligned byte for byte with the source
//! ([`OffsetText`]): a match's offset indexes the source, its line counts the
//! newlines before it and its column the UTF-16 code units from its line's
//! start, as upstream's one-space-per-code-unit blanking gives them. A
//! receiver imported from another file reads that file's client and queue
//! bindings, the import resolved among the indexed files only
//! ([`ViewContext::indexed_files_only`]). The regexes are upstream's, written
//! for the `regex` crate: JavaScript's `\s` is [`JS_SPACE`], its `\w` and
//! `\b` ASCII; a lookbehind that only skips starts bound to fail is dropped,
//! one that reads the character before a start is checked by hand, and a
//! backreference to a quote over a body that holds no quote is an
//! alternation of the three quotes.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;

use anyhow::Result;
use codegraph_core::file_class::{is_generated_file, is_test_path};
use codegraph_core::types::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::{Captures, Regex};
use serde_json::{Value, json};

use super::context::ViewContext;
use super::dispatch::place;
use super::stage::{View, enclosing_fn, enclosing_value, slice_lines};
use crate::frameworks::js_scan::{JS_SPACE, is_js_space, js_trim};
use crate::frameworks::offset_text::OffsetText;
use crate::frameworks::route_href::{HOLE, read_string_at};
use crate::import_resolver::resolve_import_path;
use crate::types::ResolutionContext;

/// Upstream `JS_FILE`: a JavaScript or TypeScript file (not a Vue or
/// Svelte component).
static JS_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?:[cm]?[jt]sx?)$").expect("js file"));

/// Events with more handlers or dispatchers than this are too generic to
/// pair without type information (upstream `EVENT_FANOUT_CAP`).
const EVENT_FANOUT_CAP: usize = 6;

/// The methods a route name may start with (upstream `HTTP_VERBS`).
const HTTP_VERBS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "ALL", "ANY",
];

const TIER_CLIENT_TO_SERVER: &str = "client→server";
const TIER_SERVER_TO_CLIENT: &str = "server→client";

/// JavaScript's `.`: any character but a line terminator.
const JS_DOT: &str = r"[^\n\r\x{2028}\x{2029}]";

/// `pattern` compiled with JavaScript's `\s` ([`JS_SPACE`]); the patterns
/// spell JavaScript's `\w` as `[0-9A-Za-z_]` and its `\b` as `(?-u:\b)`.
fn js(pattern: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).expect("a tier pattern compiles")
}

/// A string literal: upstream's `(['"`])BODY\1` over a body that holds no
/// quote, as an alternation of the three quotes.
fn quoted(body: &str) -> String {
    format!("(?:'({body})'|\"({body})\"|`({body})`)")
}

/// The text of the first group from `first` on that took part in the match:
/// the body of a [`quoted`] literal.
fn literal_group<'t>(captures: &Captures<'t>, first: usize) -> Option<&'t str> {
    (first..first + 3).find_map(|group| captures.get(group).map(|found| found.as_str()))
}

// ---- Source reading --------------------------------------------------------

/// Index of the `)` closing the `(` at `open`, strings and templates
/// skipped (upstream `closeParen`); `None` when unbalanced.
fn close_paren(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i64;
    let mut i = open;
    while i < s.len() {
        let ch = s[i];
        if ch == b'"' || ch == b'\'' {
            i += 1;
            while i < s.len() && s[i] != ch {
                if s[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if ch == b'`' {
            i = template_end(s, i) + 1;
            continue;
        }
        if ch == b'(' {
            depth += 1;
        } else if ch == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// What [`template_end`] is inside: a template, or a `${…}` at a depth.
enum Frame {
    Template,
    Interpolation(i64),
}

/// Index of the backtick closing the template opening at `open`, or the
/// end of `s` (upstream `templateEnd`). An interpolation counts braces and
/// nothing else, a template inside it opens a nested one; upstream recurses
/// for those, this keeps them on a heap stack with the same answers.
fn template_end(s: &[u8], open: usize) -> usize {
    let mut frames = vec![Frame::Template];
    let mut i = open + 1;
    while i < s.len() {
        let Some(frame) = frames.last_mut() else {
            break;
        };
        match frame {
            Frame::Template => match s[i] {
                b'\\' => i += 2,
                b'`' => {
                    frames.pop();
                    if frames.is_empty() {
                        return i;
                    }
                    // The interpolation's loop steps past the backtick.
                    i += 1;
                }
                b'$' if s.get(i + 1) == Some(&b'{') => {
                    // The `{` is the interpolation's first brace.
                    i += 1;
                    frames.push(Frame::Interpolation(0));
                }
                _ => i += 1,
            },
            Frame::Interpolation(depth) => {
                match s[i] {
                    b'{' => *depth += 1,
                    b'}' => {
                        *depth -= 1;
                        if *depth == 0 {
                            frames.pop();
                        }
                    }
                    b'`' => frames.push(Frame::Template),
                    _ => {}
                }
                i += 1;
            }
        }
    }
    s.len()
}

/// The arguments of the call whose `(` is at `open`, split at depth-0
/// commas and trimmed, an empty one dropped unless it is the first
/// (upstream `argumentsAt`); `None` when the call is unbalanced.
fn arguments_at(s: &str, open: usize) -> Option<Vec<String>> {
    let close = close_paren(s.as_bytes(), open)?;
    let inner = &s[open + 1..close];
    let bytes = inner.as_bytes();
    let mut pieces: Vec<&str> = Vec::new();
    let mut depth = 0i64;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' {
            i += 1;
            while i < bytes.len() && bytes[i] != c {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == b'`' {
            i = template_end(bytes, i) + 1;
            continue;
        }
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                pieces.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    pieces.push(&inner[start..]);
    Some(
        pieces
            .into_iter()
            .map(js_trim)
            .enumerate()
            .filter(|(at, piece)| !piece.is_empty() || *at == 0)
            .map(|(_, piece)| piece.to_string())
            .collect(),
    )
}

/// `String.prototype.trimStart`.
fn js_trim_start(s: &str) -> &str {
    s.trim_start_matches(is_js_space)
}

/// `s` without JavaScript white space (upstream `.replace(/\s+/g, '')`).
fn without_space(s: &str) -> String {
    s.chars().filter(|&ch| !is_js_space(ch)).collect()
}

/// The offset past the JavaScript white space starting at `at`.
fn skip_space(s: &str, mut at: usize) -> usize {
    while let Some(ch) = s.get(at..).and_then(|rest| rest.chars().next()) {
        if !is_js_space(ch) {
            break;
        }
        at += ch.len_utf8();
    }
    at
}

static LEADING_PAREN: LazyLock<Regex> = LazyLock::new(|| js(r"^\(\s*"));
static NEW_URL: LazyLock<Regex> = LazyLock::new(|| js(r"^new\s+URL\s*\("));

/// The first string literal of `text`, holes kept; `None` when it opens
/// with anything else (upstream `leadingString`). `new URL('/x', base)` is
/// its first argument.
fn leading_string(text: &str) -> Option<String> {
    let trimmed = js_trim(text);
    let t = LEADING_PAREN
        .find(trimmed)
        .map_or(trimmed, |paren| &trimmed[paren.end()..]);
    if let Some(url) = NEW_URL.find(t) {
        let args = arguments_at(t, url.end() - 1)?;
        return args
            .first()
            .filter(|first| !first.is_empty())
            .and_then(|first| leading_string(first));
    }
    match t.as_bytes().first() {
        Some(b'"' | b'\'' | b'`') => read_string_at(t, 0),
        _ => None,
    }
}

static METHOD_IN: LazyLock<Regex> =
    LazyLock::new(|| js(&format!(r"(?-u:\b)method\s*:\s*{}", quoted("[A-Za-z]+"))));

/// `method: 'POST'` in an options object, upper-cased (upstream
/// `methodIn`); `None` when absent or computed.
fn method_in(text: &str) -> Option<String> {
    let captures = METHOD_IN.captures(text)?;
    Some(literal_group(&captures, 1)?.to_ascii_uppercase())
}

static LITERAL: LazyLock<Regex> = LazyLock::new(|| js(&quoted(r#"[^'"`]+"#)));

/// The first string literal anywhere in a decorator's arguments (upstream
/// `firstLiteral`).
fn first_literal(args: &str) -> Option<String> {
    let captures = LITERAL.captures(args)?;
    literal_group(&captures, 1).map(str::to_string)
}

/// Every string literal in a decorator's arguments, for
/// `@OnEvent(['a', 'b'])` (upstream `allLiterals`).
fn all_literals(args: &str) -> Vec<String> {
    LITERAL
        .captures_iter(args)
        .filter_map(|captures| literal_group(&captures, 1).map(str::to_string))
        .collect()
}

static DECORATOR_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@[0-9A-Za-z_.]+").expect("decorator name"));
static MODIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:public|private|protected|async|static|readonly|override)(?-u:\b)")
        .expect("modifier")
});
static IDENT_CALL: LazyLock<Regex> = LazyLock::new(|| js(r"^([A-Za-z_$][0-9A-Za-z_$]*)\s*[<(]"));

/// The name of the method a decorator ending at `from` sits on, and where
/// it starts: further stacked decorators and modifiers skipped, the
/// identifier before `(` or `<` (upstream `methodNameAfter`).
fn method_name_after(safe: &str, from: usize) -> Option<(String, usize)> {
    let bytes = safe.as_bytes();
    let mut i = from;
    loop {
        i = skip_space(safe, i);
        if bytes.get(i) != Some(&b'@') {
            break;
        }
        let Some(name) = DECORATOR_NAME.find(&safe[i..]) else {
            break;
        };
        i += name.end();
        i = skip_space(safe, i);
        if bytes.get(i) == Some(&b'(') {
            i = close_paren(bytes, i)? + 1;
        }
    }
    loop {
        i = skip_space(safe, i);
        match MODIFIER.find(&safe[i..]) {
            Some(modifier) if modifier.end() > 0 => i += modifier.end(),
            _ => break,
        }
    }
    i = skip_space(safe, i);
    let captures = IDENT_CALL.captures(&safe[i..])?;
    Some((captures[1].to_string(), i))
}

/// One `@Name(…)` decorator: its arguments, where it starts and where its
/// `)` ends.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decorator {
    args: String,
    index: usize,
    end: usize,
}

static PROCESSOR: LazyLock<Regex> = LazyLock::new(|| js(r"@Processor\s*\("));
static PROCESS: LazyLock<Regex> = LazyLock::new(|| js(r"@Process\s*\("));
static ON_EVENT: LazyLock<Regex> = LazyLock::new(|| js(r"@OnEvent\s*\("));
static SUBSCRIBE_MESSAGE: LazyLock<Regex> = LazyLock::new(|| js(r"@SubscribeMessage\s*\("));

/// Every decorator `pattern` (`@Name\s*\(`) opens in `safe`, with its
/// arguments (upstream `decorators`): the scan resumes past a balanced
/// call's `)`, else past the opening.
fn decorators(safe: &str, pattern: &Regex) -> Vec<Decorator> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = pattern.find_at(safe, from) {
        let open = found.end() - 1;
        match close_paren(safe.as_bytes(), open) {
            Some(close) => {
                out.push(Decorator {
                    args: safe[open + 1..close].to_string(),
                    index: found.start(),
                    end: close + 1,
                });
                from = close + 1;
            }
            None => from = found.end(),
        }
    }
    out
}

/// The UTF-16 column of `at` in `text`: the code units of the source from
/// its line's start (upstream `columnOf`, which counts its stripped text's,
/// blanked one space per code unit).
fn column_of(text: &OffsetText<'_>, at: usize) -> i64 {
    let start = text.safe[..at].rfind('\n').map_or(0, |newline| newline + 1);
    text.units(start, at)
}

// ---- Per-file bindings -----------------------------------------------------

/// What a file binds names to (upstream `FileFacts`' clients and queues):
/// its client instances with their literal base URLs, its default export's
/// client and its named queues.
#[derive(Debug, Default)]
struct Bindings {
    clients: HashMap<String, Option<String>>,
    default_client: Option<Option<String>>,
    queues: HashMap<String, String>,
}

/// The client factories of axios, ky, got, ofetch, wretch and redaxios.
const FACTORY: &str = r"(?:(?:axios|ky|got|ofetch|\$fetch|wretch|redaxios)\s*\.\s*(?:create|extend)|new\s+Axios|wretch)\s*\(";

static CLIENT_FACTORY: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?-u:\b)(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][0-9A-Za-z_$]*)\s*(?::[^=;]*?)?=\s*(?:await\s+)?{FACTORY}"
    ))
});
static DEFAULT_CLIENT_FACTORY: LazyLock<Regex> =
    LazyLock::new(|| js(&format!(r"(?-u:\b)export\s+default\s+{FACTORY}")));
static QUEUE_BINDING: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?-u:\b)(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][0-9A-Za-z_$]*)\s*(?::[^=;]*?)?=\s*(?:new\s+)?(?:Queue|Bull|BullQueue)\s*(?:<[^>]*>)?\s*\(\s*{}",
        quoted(r#"[^'"`]+"#)
    ))
});
static INJECT_QUEUE: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"@InjectQueue\s*\(\s*{}\s*\)\s*(?:(?:private|public|protected|readonly)\s+)*([A-Za-z_$][0-9A-Za-z_$]*)",
        quoted(r#"[^'"`]+"#)
    ))
});
static SOCKET_SERVER_FILE: LazyLock<Regex> = LazyLock::new(|| {
    js(
        r#"@WebSocketGateway\s*\(|@SubscribeMessage\s*\(|@WebSocketServer\s*\(|(?-u:\b)new\s+(?:Server|SocketIOServer|WebSocketServer|WebSocket\.Server|WSServer)(?-u:\b)|\.on\s*\(\s*['"]connection['"]"#,
    )
});
static BASE_URL_KEY: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?-u:\b)(?:baseURL|baseUrl|prefixUrl|baseURI)\s*:\s*"));

/// The literal base URL of the client config opening at `open` (upstream
/// `baseUrlIn`).
fn base_url_in(safe: &str, open: usize) -> Option<String> {
    let args = arguments_at(safe, open);
    let config = args
        .as_ref()
        .and_then(|args| args.first())
        .map_or("", String::as_str);
    let key = BASE_URL_KEY.find(config)?;
    read_string_at(config, key.end())
}

/// The bindings `safe`, a file's comment-stripped text, declares (upstream
/// `readFacts`); a later binding of a name replaces an earlier one.
fn read_bindings(safe: &str) -> Bindings {
    let mut bindings = Bindings::default();
    for captures in CLIENT_FACTORY.captures_iter(safe) {
        let open = captures.get(0).expect("the whole match").end() - 1;
        bindings
            .clients
            .insert(captures[1].to_string(), base_url_in(safe, open));
    }
    bindings.default_client = DEFAULT_CLIENT_FACTORY
        .find(safe)
        .map(|found| base_url_in(safe, found.end() - 1));
    for captures in QUEUE_BINDING.captures_iter(safe) {
        if let Some(queue) = literal_group(&captures, 2) {
            bindings
                .queues
                .insert(captures[1].to_string(), queue.to_string());
        }
    }
    for captures in INJECT_QUEUE.captures_iter(safe) {
        if let Some(queue) = literal_group(&captures, 1) {
            bindings
                .queues
                .insert(captures[4].to_string(), queue.to_string());
        }
    }
    bindings
}

// ---- 1. HTTP client → route ------------------------------------------------

/// A receiver that is an HTTP client by its name alone (upstream
/// `CLIENT_NAMES`).
const CLIENT_NAMES: [&str; 24] = [
    "axios",
    "ky",
    "got",
    "superagent",
    "http",
    "https",
    "httpClient",
    "httpService",
    "api",
    "apiClient",
    "client",
    "restClient",
    "request",
    "agent",
    "fetcher",
    "instance",
    "$api",
    "$http",
    "$axios",
    "axiosInstance",
    "Axios",
    "HttpClient",
    "backend",
    "server",
];

/// A receiver that registers routes, never a client, unless a client
/// factory made it (upstream `SERVER_NAMES`).
const SERVER_NAMES: [&str; 13] = [
    "app",
    "router",
    "route",
    "routes",
    "express",
    "fastify",
    "koa",
    "hono",
    "elysia",
    "apiRouter",
    "v1",
    "v2",
    "r",
];

/// A type argument between a callee and its `(` (upstream `GENERIC`).
const GENERIC: &str = r"(?:<[^()<>]*(?:<[^()<>]*>[^()<>]*)*>)?";

static BARE_CLIENT_CALL: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?:(?:window|globalThis|global)\s*\.\s*)?(?-u:\b)(fetch|\$fetch|ofetch|axios|ky|got|useFetch|useSWR)\s*{GENERIC}\s*\("
    ))
});
static GLOBAL_PREFIX: LazyLock<Regex> = LazyLock::new(|| js(r"^(?:window|globalThis|global)\s*\."));
/// A receiver chain (upstream's, without `CHAIN_START`'s lookbehind: a
/// match from inside a word implies one from the word's first
/// identifier-start character, which a left-to-right scan reaches first,
/// and each match ends on a non-word character).
const CHAIN: &str =
    r"((?:this\s*\.\s*)?[A-Za-z_$][0-9A-Za-z_$]*(?:\s*\.\s*[A-Za-z_$][0-9A-Za-z_$]*)*)";
static MEMBER_CLIENT_CALL: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"{CHAIN}\s*\.\s*(get|post|put|patch|delete|head|options|request|\$get|\$post|\$put|\$patch|\$delete)\s*{GENERIC}\s*\("
    ))
});

/// A route node a client call can reach: a name `METHOD /path`.
#[derive(Debug, Clone)]
struct HttpRoute {
    node: Node,
    method: String,
    segs: Vec<String>,
}

/// The method and path segments of a route named `METHOD /path` (upstream
/// `httpRoutes`' test); `None` for a name without a known method or a path
/// starting with `/`, such as a Next.js page.
fn route_shape(name: &str) -> Option<(String, Vec<String>)> {
    let space = name.find(' ')?;
    if space == 0 {
        return None;
    }
    let method = name[..space].to_uppercase();
    if !HTTP_VERBS.contains(&method.as_str()) {
        return None;
    }
    let path = js_trim(&name[space + 1..]);
    if !path.starts_with('/') {
        return None;
    }
    Some((method, segments(path)))
}

/// The route nodes a client call can reach, in upstream's
/// `getNodesByKind` order.
fn http_routes(view: &View<'_>) -> Result<Vec<HttpRoute>> {
    Ok(view
        .nodes_of_kind(NodeKind::Route)?
        .into_iter()
        .filter_map(|node| {
            let (method, segs) = route_shape(&node.name)?;
            Some(HttpRoute { node, method, segs })
        })
        .collect())
}

/// `path`'s non-empty `/`-separated segments.
fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|seg| !seg.is_empty())
        .map(str::to_string)
        .collect()
}

static CATCH_ALL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\*|\[\.\.\.|\{\*|:[0-9A-Za-z_$]+\*$|\{[0-9A-Za-z_$]+:\*\}|\*[0-9A-Za-z_$]*$)")
        .expect("catch-all")
});

/// A route's parameter segment (upstream `PARAM_SEG`).
fn is_param_seg(seg: &str) -> bool {
    seg.starts_with([':', '{', '[', '<', '*']) || seg.ends_with('?')
}

/// How well a client's segments match a route's, `None` when they do not:
/// a literal segment +3, a parameter +2, a catch-all +1 for each segment it
/// takes; a hole fills a parameter, never a literal (upstream `scorePath`).
fn score_path(client: &[String], route: &[String]) -> Option<i64> {
    let mut score = 0i64;
    let mut i = 0;
    for seg in route {
        if CATCH_ALL.is_match(seg) {
            if i >= client.len() {
                return None;
            }
            score += (client.len() - i) as i64;
            i = client.len();
            continue;
        }
        let c = client.get(i)?;
        if is_param_seg(seg) {
            score += 2;
        } else if c == seg {
            score += 3;
        } else {
            return None;
        }
        i += 1;
    }
    (i == client.len()).then_some(score)
}

/// The one best route of `method` (or `ALL`/`ANY`) for `segs`, `None` on a
/// tie (upstream `matchHttp`). With `suffix`, a base URL hid the path's
/// start: the route's tail is matched, the path having two segments at
/// least and the route two more at most.
fn match_http<'r>(
    method: &str,
    segs: &[String],
    suffix: bool,
    routes: &'r [HttpRoute],
) -> Option<&'r HttpRoute> {
    let mut best = None;
    let mut best_score = -1;
    let mut tied = false;
    for route in routes {
        if route.method != "ALL" && route.method != "ANY" && route.method != method {
            continue;
        }
        let score = if suffix {
            if segs.len() < 2 || route.segs.len() < segs.len() || route.segs.len() - segs.len() > 2
            {
                continue;
            }
            score_path(segs, &route.segs[route.segs.len() - segs.len()..])
        } else {
            score_path(segs, &route.segs)
        };
        let Some(score) = score else {
            continue;
        };
        if score > best_score {
            best = Some(route);
            best_score = score;
            tied = false;
        } else if score == best_score {
            tied = true;
        }
    }
    if tied { None } else { best }
}

/// A path a client call names: its segments, a hole as `*`; whether a base
/// URL hid its start; and how the edge's `href` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientPath {
    segs: Vec<String>,
    suffix: bool,
    display: String,
}

static ABSOLUTE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^(?:[A-Za-z][A-Za-z0-9+.\-]*:)?//[^/]*(/{JS_DOT}*)?$"
    ))
    .expect("absolute url")
});

/// `raw` up to its first `?` or `#`.
fn without_query(raw: &str) -> &str {
    raw.find(['?', '#']).map_or(raw, |cut| &raw[..cut])
}

/// `raw`'s path when it is an absolute URL (`scheme://host/…`, `//host/…`).
fn absolute_path(raw: &str) -> Option<String> {
    let captures = ABSOLUTE_URL.captures(raw)?;
    Some(
        captures
            .get(1)
            .map_or("/", |path| path.as_str())
            .to_string(),
    )
}

/// `segs` and `rest`'s segments as one `/` path.
fn joined(segs: &[String], rest: &str) -> String {
    let parts: Vec<&str> = segs
        .iter()
        .map(String::as_str)
        .chain(rest.split('/'))
        .filter(|part| !part.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// `path`'s segments, one holding a hole as `*`, and the `href` showing
/// them (upstream's `display`).
fn hole_segments(path: &str) -> (Vec<String>, String) {
    let segs: Vec<String> = path
        .split('/')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            if seg.contains(HOLE) {
                "*".to_string()
            } else {
                seg.to_string()
            }
        })
        .collect();
    let shown: Vec<&str> = segs
        .iter()
        .map(|seg| if seg == "*" { "${…}" } else { seg.as_str() })
        .collect();
    let display = format!("/{}", shown.join("/"));
    (segs, display)
}

/// The path a client call names, joined to its instance's base URL
/// (upstream `clientPath`); `None` when it is not literal enough: relative
/// with no base, or nothing but holes.
fn client_path(raw: &str, base_url: Option<&str>) -> Option<ClientPath> {
    let mut p = without_query(raw).to_string();
    let mut suffix = false;
    if let Some(path) = absolute_path(&p) {
        p = path;
    } else if !p.starts_with('/') {
        if let Some(rest) = p.strip_prefix(HOLE) {
            if !rest.starts_with('/') {
                return None;
            }
            p = rest.to_string();
            suffix = true;
        } else {
            // A relative path joins its instance's base, and needs one.
            let base_url = base_url?;
            match client_path(base_url, None) {
                None => {
                    // A base that is itself a hole: match by the tail.
                    if !base_url.contains(HOLE) {
                        return None;
                    }
                    suffix = true;
                    p = format!("/{p}");
                }
                Some(base) => {
                    suffix = base.suffix;
                    p = joined(&base.segs, &p);
                }
            }
        }
    } else if let Some(base_url) = base_url {
        // An instance with a literal path base: axios joins `baseURL + url`.
        match client_path(base_url, None) {
            Some(base) if !base.segs.is_empty() => {
                suffix = base.suffix;
                p = joined(&base.segs, &p);
            }
            None if base_url.contains(HOLE) => suffix = true,
            _ => {}
        }
    }
    let (segs, display) = hole_segments(&p);
    if !segs.is_empty() && segs.iter().all(|seg| seg == "*") {
        return None;
    }
    Some(ClientPath {
        segs,
        suffix,
        display,
    })
}

static URL_KEY: LazyLock<Regex> = LazyLock::new(|| js(r"(?-u:\b)url\s*:\s*"));
static GENERIC_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("<{JS_DOT}*>$")).expect("generic tail"));

/// A call's callee as the edge names it: the matched text up to its `(`
/// without white space or a trailing type argument (`useSWR<T>` is
/// `useSWR`).
fn callee_of(text: &str) -> String {
    GENERIC_TAIL.replace(&without_space(text), "").into_owned()
}

/// A client call the HTTP channel pairs with a route.
#[derive(Debug, Clone)]
struct HttpSite {
    function: Node,
    line: i64,
    column: i64,
    callee: String,
    method: String,
    path: ClientPath,
}

// ---- 2. Queue job → consumer -----------------------------------------------

static QUEUE_ADD: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"{CHAIN}\s*\.\s*add\s*\(\s*{}",
        quoted(r#"[^'"`]+"#)
    ))
});
static QUEUE_SHAPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)queue|jobs?$|worker|bull|flow|producer").expect("queue shaped")
});
static NEW_WORKER: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?-u:\b)new\s+Worker\s*(?:<[^>]*>)?\s*\(\s*{}\s*,\s*",
        quoted(r#"[^'"`]+"#)
    ))
});
static QUEUE_PROCESS: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"{CHAIN}\s*\.\s*process\s*\(\s*(?:{}\s*,\s*)?(?:[0-9]+\s*,\s*)?",
        quoted(r#"[^'"`]+"#)
    ))
});
/// A handler argument: a named function (group 1), or an inline one.
static HANDLER_ARG: LazyLock<Regex> = LazyLock::new(|| {
    js(
        r"^(?:(?:async\s+)?([A-Za-z_$][0-9A-Za-z_$.]*)\s*(?:[,)]|$)|(?:async\s*)?(?:\(|function(?-u:\b)|[A-Za-z_$][0-9A-Za-z_$]*\s*=>))",
    )
});
static CLASS_DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| js(r"(?-u:\b)class\s+([A-Za-z_$][0-9A-Za-z_$]*)"));

/// Whether a receiver's last segment names a queue (upstream
/// `QUEUE_SHAPED`, ASCII case-insensitive).
fn queue_shaped(last: &str) -> bool {
    QUEUE_SHAPED.is_match(last)
}

/// `receiver` without white space and a leading `this.`, as segments.
fn chain_of(receiver: &str) -> Vec<String> {
    let compact = without_space(receiver);
    let chain = compact.strip_prefix("this.").unwrap_or(&compact);
    chain.split('.').map(str::to_string).collect()
}

/// The last `.` segment of `receiver` without white space (a leading
/// `this` kept).
fn last_segment(receiver: &str) -> String {
    let compact = without_space(receiver);
    compact.rsplit('.').next().unwrap_or_default().to_string()
}

/// A job a producer puts on a queue.
#[derive(Debug, Clone)]
struct QueueProducer {
    function: Node,
    line: i64,
    column: i64,
    callee: String,
    queue: Option<String>,
    job: String,
}

/// A consumer of a queue's jobs: `job` `None` takes every job on it.
#[derive(Debug, Clone)]
struct QueueConsumer {
    node: Node,
    file: String,
    line: i64,
    queue: Option<String>,
    job: Option<String>,
}

/// Each producer to its queue's most specific consumers (upstream
/// `pairQueue`): the job by name on the named queue, else the job by name,
/// else the queue's consumers of every job; none or more than
/// [`EVENT_FANOUT_CAP`] draw nothing.
fn pair_queue(
    producers: &[QueueProducer],
    consumers: &[QueueConsumer],
    edges: &mut Vec<Edge>,
    seen: &mut HashSet<(String, String)>,
) {
    for p in producers {
        let candidates: Vec<&QueueConsumer> = consumers
            .iter()
            .filter(|c| {
                (p.queue.is_none() || c.queue.is_none() || c.queue == p.queue)
                    && c.job.as_ref().is_none_or(|job| *job == p.job)
            })
            .collect();
        let exact: Vec<&QueueConsumer> = candidates
            .iter()
            .copied()
            .filter(|c| c.job.as_ref() == Some(&p.job) && c.queue == p.queue && p.queue.is_some())
            .collect();
        let chosen: Vec<&QueueConsumer> = if !exact.is_empty() {
            exact
        } else {
            let by_job: Vec<&QueueConsumer> = candidates
                .iter()
                .copied()
                .filter(|c| c.job.as_ref() == Some(&p.job))
                .collect();
            if !by_job.is_empty() {
                by_job
            } else if p.queue.is_none() {
                // An unnamed queue and no consumer naming the job: a guess.
                continue;
            } else {
                candidates
                    .into_iter()
                    .filter(|c| c.queue == p.queue)
                    .collect()
            }
        };
        if chosen.is_empty() || chosen.len() > EVENT_FANOUT_CAP {
            continue;
        }
        for c in chosen {
            if c.node.id == p.function.id
                || !seen.insert((p.function.id.clone(), c.node.id.clone()))
            {
                continue;
            }
            let mut metadata = json!({
                "synthesizedBy": "queue-job",
                "channel": "queue",
                "callee": p.callee,
                "event": p.job,
                "registeredAt": format!("{}:{}", c.file, c.line),
            });
            if let Some(queue) = p
                .queue
                .as_ref()
                .or(c.queue.as_ref())
                .filter(|queue| !queue.is_empty())
            {
                metadata["queue"] = json!(queue);
            }
            edges.push(tier_edge(&p.function, &c.node, p.line, p.column, metadata));
        }
    }
}

// ---- 3. Events: a bus, and sockets both ways -------------------------------

/// A dispatch's receiver chain (upstream's, without the `(?<![\w$])`
/// lookbehind: a word character opens the chain, so a match from inside a
/// word implies one from its first character, and each match ends on a
/// quote).
const EVENT_CHAIN: &str = r"((?:[0-9A-Za-z_$]+(?:\([^()]*\))?\s*\.\s*)*[0-9A-Za-z_$]+)";

static EMIT: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"{EVENT_CHAIN}\s*\.\s*(emit|emitAsync)\s*\(\s*{}",
        quoted(r#"[^'"`\n]+"#)
    ))
});
static SOCKET_ON: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"{EVENT_CHAIN}\s*\.\s*(?:on|once)\s*\(\s*{}\s*,\s*",
        quoted(r#"[^'"`\n]+"#)
    ))
});
static CALL_ARGUMENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\([^()]*\)").expect("call arguments"));

/// A receiver segment that names a socket (upstream `SOCKET_WORDS`).
const SOCKET_WORDS: [&str; 28] = [
    "socket",
    "io",
    "ws",
    "wss",
    "client",
    "server",
    "namespace",
    "nsp",
    "conn",
    "connection",
    "gateway",
    "broadcast",
    "to",
    "in",
    "of",
    "except",
    "volatile",
    "local",
    "sockets",
    "socketServer",
    "wsServer",
    "room",
    "channel",
    "pusher",
    "ably",
    "ioClient",
    "socketClient",
    "sock",
];

/// A receiver segment that names an event bus (upstream `BUS_WORDS`).
const BUS_WORDS: [&str; 18] = [
    "eventEmitter",
    "emitter",
    "events",
    "eventBus",
    "bus",
    "dispatcher",
    "pubsub",
    "publisher",
    "eventPublisher",
    "ee",
    "hub",
    "mediator",
    "broker",
    "messageBus",
    "appEvents",
    "domainEvents",
    "eventsService",
    "eventService",
];

/// The transport's own events, which every socket emits and handles
/// (upstream `GENERIC_EVENT`).
const GENERIC_EVENTS: [&str; 28] = [
    "error",
    "connect",
    "connect_error",
    "connect_failed",
    "connection",
    "disconnect",
    "disconnecting",
    "reconnect",
    "reconnect_attempt",
    "reconnecting",
    "reconnect_error",
    "reconnect_failed",
    "close",
    "open",
    "end",
    "data",
    "ready",
    "drain",
    "finish",
    "pipe",
    "unpipe",
    "listening",
    "timeout",
    "ping",
    "pong",
    "upgrade",
    "newListener",
    "removeListener",
];

/// What carries an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Bus,
    Socket,
}

impl Shape {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bus => "bus",
            Self::Socket => "socket",
        }
    }
}

/// Which end of a socket a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Server,
    Client,
}

/// An event a function emits.
#[derive(Debug, Clone)]
struct Dispatch {
    function: Node,
    line: i64,
    column: i64,
    callee: String,
    event: String,
    shape: Shape,
    side: Side,
}

/// A handler of an event, or of an `@OnEvent` glob.
#[derive(Debug, Clone)]
struct Handler {
    node: Node,
    file: String,
    line: i64,
    pattern: String,
    kind: Shape,
    side: Side,
}

/// What a receiver carries (upstream `shapeOf`): a socket when a segment
/// names one, else a bus, else nothing.
fn shape_of(receiver: &str) -> Option<Shape> {
    let without_calls = CALL_ARGUMENTS.replace_all(receiver, "");
    let compact = without_space(&without_calls);
    let segs: Vec<&str> = compact.split('.').filter(|seg| *seg != "this").collect();
    if segs.iter().any(|seg| SOCKET_WORDS.contains(seg)) {
        return Some(Shape::Socket);
    }
    if segs.iter().any(|seg| BUS_WORDS.contains(seg)) {
        return Some(Shape::Bus);
    }
    None
}

/// Whether `pattern` names `event` (upstream `eventMatches`): `*` stands
/// for one `.`-free segment, `**` for anything, the rest is literal.
fn event_matches(
    pattern: &str,
    event: &str,
    compiled: &mut HashMap<String, Option<Regex>>,
) -> bool {
    if pattern == event {
        return true;
    }
    if !pattern.contains('*') {
        return false;
    }
    compiled
        .entry(pattern.to_string())
        .or_insert_with(|| {
            let body: Vec<String> = pattern
                .split("**")
                .map(|part| {
                    part.split('*')
                        .map(regex::escape)
                        .collect::<Vec<_>>()
                        .join("[^.]+")
                })
                .collect();
            Regex::new(&format!("^(?:{})$", body.join(&format!("{JS_DOT}*")))).ok()
        })
        .as_ref()
        .is_some_and(|glob| glob.is_match(event))
}

/// Each dispatch to the handlers of its event (upstream `pairEvents`): a
/// bus dispatch to the bus handlers whose pattern matches, a client's
/// socket dispatch to the server's handlers and a server's to the client's.
/// An event dispatched more than [`EVENT_FANOUT_CAP`] times, or matching
/// none or more than that many handlers, draws nothing.
fn pair_events(
    dispatches: &[Dispatch],
    handlers: &[Handler],
    edges: &mut Vec<Edge>,
    seen: &mut HashSet<(String, String)>,
) {
    let mut groups: Vec<Vec<&Dispatch>> = Vec::new();
    let mut group_of: HashMap<String, usize> = HashMap::new();
    for d in dispatches {
        let key = format!("{}:{}", d.shape.as_str(), d.event);
        let at = *group_of.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[at].push(d);
    }
    let mut compiled = HashMap::new();
    for group in &groups {
        if group.len() > EVENT_FANOUT_CAP {
            continue;
        }
        for d in group {
            let (matched, tier): (Vec<&Handler>, Option<&str>) = match (d.shape, d.side) {
                (Shape::Bus, _) => (
                    handlers
                        .iter()
                        .filter(|h| {
                            h.kind == Shape::Bus
                                && event_matches(&h.pattern, &d.event, &mut compiled)
                        })
                        .collect(),
                    None,
                ),
                (Shape::Socket, Side::Client) => (
                    handlers
                        .iter()
                        .filter(|h| {
                            h.kind == Shape::Socket
                                && h.side == Side::Server
                                && h.pattern == d.event
                        })
                        .collect(),
                    Some(TIER_CLIENT_TO_SERVER),
                ),
                (Shape::Socket, Side::Server) => (
                    handlers
                        .iter()
                        .filter(|h| {
                            h.kind == Shape::Socket
                                && h.side == Side::Client
                                && h.pattern == d.event
                        })
                        .collect(),
                    Some(TIER_SERVER_TO_CLIENT),
                ),
            };
            if matched.is_empty() || matched.len() > EVENT_FANOUT_CAP {
                continue;
            }
            for h in matched {
                if h.node.id == d.function.id
                    || !seen.insert((d.function.id.clone(), h.node.id.clone()))
                {
                    continue;
                }
                let mut metadata = json!({
                    "synthesizedBy": "event-bus",
                    "channel": if d.shape == Shape::Bus { "event" } else { "socket" },
                    "callee": d.callee,
                    "event": d.event,
                    "registeredAt": format!("{}:{}", h.file, h.line),
                });
                if let Some(tier) = tier {
                    metadata["tier"] = json!(tier);
                }
                edges.push(tier_edge(&d.function, &h.node, d.line, d.column, metadata));
            }
        }
    }
}

/// A synthesized `calls` edge at a site's line and column.
fn tier_edge(source: &Node, target: &Node, line: i64, column: i64, metadata: Value) -> Edge {
    Edge {
        id: None,
        source: source.id.clone(),
        target: target.id.clone(),
        kind: EdgeKind::Calls,
        metadata: Some(metadata),
        line: Some(line),
        col: Some(column),
        provenance: Some("heuristic".to_string()),
    }
}

// ---- The cross-tier pass ---------------------------------------------------

static HTTP_GATE: LazyLock<Regex> = LazyLock::new(|| {
    js(
        r"(?-u:\b)(?:fetch|\$fetch|ofetch|axios|ky|got|useFetch|useSWR)(?-u:\b)|\.\s*(?:get|post|put|patch|delete|head|options|request|\$get|\$post)\s*[<(]",
    )
});
static QUEUE_GATE: LazyLock<Regex> = LazyLock::new(|| {
    js(r"\.\s*add\s*\(|@Processor\s*\(|(?-u:\b)new\s+Worker\s*[<(]|\.\s*process\s*\(")
});
static EVENT_GATE: LazyLock<Regex> = LazyLock::new(|| {
    js(r"\.\s*(?:emit|emitAsync|on|once)\s*\(|@OnEvent\s*\(|@SubscribeMessage\s*\(")
});

/// A receiver's binding in the file an import of its head resolves to.
#[derive(Clone)]
struct Imported {
    bindings: Rc<Bindings>,
    file: String,
    exported_name: String,
    is_default: bool,
}

/// One JavaScript file the pass reads its sites from: its comment-stripped
/// text, its bindings, whether it holds a socket server, and its nodes,
/// read on first use (most files that pass a gate hold no site).
struct Scan<'t> {
    file: &'t str,
    text: OffsetText<'t>,
    bindings: Rc<Bindings>,
    socket_server: bool,
    nodes: OnceCell<Vec<Node>>,
    route_lines: OnceCell<HashSet<i64>>,
}

impl Scan<'_> {
    /// The file's nodes by line and id (upstream `getNodesByFile`).
    fn nodes(&self, view: &View<'_>) -> Result<&[Node]> {
        if let Some(nodes) = self.nodes.get() {
            return Ok(nodes);
        }
        let nodes = view.nodes_in_file(self.file)?;
        Ok(self.nodes.get_or_init(|| nodes))
    }

    /// The lines a framework resolver made a route node on: registrations,
    /// never client calls.
    fn route_lines(&self, view: &View<'_>) -> Result<&HashSet<i64>> {
        if let Some(lines) = self.route_lines.get() {
            return Ok(lines);
        }
        let lines = self
            .nodes(view)?
            .iter()
            .filter(|node| node.kind == NodeKind::Route)
            .map(|node| node.start_line)
            .collect();
        Ok(self.route_lines.get_or_init(|| lines))
    }

    fn line(&self, at: usize) -> i64 {
        self.text.line_at(at)
    }

    fn column(&self, at: usize) -> i64 {
        column_of(&self.text, at)
    }
}

/// The cross-tier pass's state: the view, an import context answering from
/// the indexed files, and what it has read once.
struct Tier<'v, 'a> {
    view: &'v View<'a>,
    context: ViewContext<'v, 'a>,
    bindings: RefCell<HashMap<String, Option<Rc<Bindings>>>>,
    imports: RefCell<HashMap<(String, String), Option<Imported>>>,
    named: RefCell<HashMap<String, Rc<Vec<Node>>>>,
}

impl<'v, 'a> Tier<'v, 'a> {
    fn new(view: &'v View<'a>) -> Self {
        Self {
            view,
            context: view.context().indexed_files_only(),
            bindings: RefCell::new(HashMap::new()),
            imports: RefCell::new(HashMap::new()),
            named: RefCell::new(HashMap::new()),
        }
    }

    /// The bindings of `path`, read once: `None` unless it is a JavaScript
    /// file whose text the view reads and is not empty.
    fn bindings_of(&self, path: &str) -> Option<Rc<Bindings>> {
        if let Some(found) = self.bindings.borrow().get(path) {
            return found.clone();
        }
        let read = if JS_FILE.is_match(path) {
            self.view
                .text(path)
                .filter(|text| !text.is_empty())
                .map(|text| Rc::new(read_bindings(&OffsetText::js(&text).safe)))
        } else {
            None
        };
        self.bindings
            .borrow_mut()
            .insert(path.to_string(), read.clone());
        read
    }

    /// The bindings of `file`, which the pass scans, from its stripped text
    /// `safe` unless an import read them first.
    fn scanned_bindings(&self, file: &str, safe: &str) -> Option<Rc<Bindings>> {
        if let Some(found) = self.bindings.borrow().get(file) {
            return found.clone();
        }
        let read = Rc::new(read_bindings(safe));
        self.bindings
            .borrow_mut()
            .insert(file.to_string(), Some(Rc::clone(&read)));
        Some(read)
    }

    /// The file `local` is imported from in `file`, with its bindings,
    /// when the import resolves to an indexed JavaScript file (upstream
    /// `importedFacts`).
    fn imported(&self, file: &str, local: &str) -> Option<Imported> {
        let key = (file.to_string(), local.to_string());
        if let Some(found) = self.imports.borrow().get(&key) {
            return found.clone();
        }
        let found = self.resolve_imported(file, local);
        self.imports.borrow_mut().insert(key, found.clone());
        found
    }

    fn resolve_imported(&self, file: &str, local: &str) -> Option<Imported> {
        let language = if file.ends_with('x') {
            Language::Tsx
        } else {
            Language::TypeScript
        };
        let mapping = self
            .context
            .get_import_mappings(file, language)
            .into_iter()
            .find(|mapping| mapping.local_name == local)?;
        let resolved = resolve_import_path(&mapping.source, file, language, &self.context)?;
        let bindings = self.bindings_of(&resolved)?;
        Some(Imported {
            bindings,
            file: resolved,
            exported_name: mapping.exported_name,
            is_default: mapping.is_default,
        })
    }

    /// What a member call's receiver is (upstream `clientFor`): a client
    /// with its base URL (`Some(base)`), or nothing (`None`).
    fn client_for(&self, file: &str, own: &Bindings, receiver: &str) -> Option<Option<String>> {
        let chain = chain_of(receiver);
        let head = chain[0].as_str();
        let last = chain[chain.len() - 1].as_str();
        if let Some(local) = own.clients.get(head) {
            return Some(local.clone());
        }
        if let Some(imported) = self.imported(file, head) {
            let bound = if imported.is_default {
                imported.bindings.default_client.clone()
            } else {
                imported
                    .bindings
                    .clients
                    .get(&imported.exported_name)
                    .cloned()
            };
            if bound.is_some() {
                return bound;
            }
        }
        if SERVER_NAMES.contains(&last) || SERVER_NAMES.contains(&head) {
            return None;
        }
        CLIENT_NAMES.contains(&last).then_some(None)
    }

    /// The queue a receiver is bound to in its file or the file its head
    /// is imported from (upstream `queueFor`).
    fn queue_for(&self, file: &str, own: &Bindings, receiver: &str) -> Option<String> {
        let chain = chain_of(receiver);
        let head = chain[0].as_str();
        if let Some(queue) = own.queues.get(head) {
            return Some(queue.clone());
        }
        let imported = self.imported(file, head)?;
        imported
            .bindings
            .queues
            .get(&imported.exported_name)
            .cloned()
    }

    /// The nodes named `name` (upstream `getNodesByName`, cached as
    /// upstream's context caches it).
    fn nodes_named(&self, name: &str) -> Result<Rc<Vec<Node>>> {
        if let Some(found) = self.named.borrow().get(name) {
            return Ok(Rc::clone(found));
        }
        let nodes = Rc::new(self.view.nodes_named(name)?);
        self.named
            .borrow_mut()
            .insert(name.to_string(), Rc::clone(&nodes));
        Ok(nodes)
    }

    /// The function a handler argument names or encloses (upstream
    /// `handlerNode`): a named one is the only function or method of that
    /// name in this file (several there: none), else in the file its head
    /// is imported from, else in the project; an inline one is the
    /// function, else the value, holding the registration's line.
    fn handler_node(&self, scan: &Scan<'_>, text: &str, line: i64) -> Result<Option<Node>> {
        let Some(captures) = HANDLER_ARG.captures(js_trim_start(text)) else {
            return Ok(None);
        };
        if let Some(named) = captures.get(1) {
            let named = named.as_str();
            let name = named.rsplit('.').next().unwrap_or(named);
            let candidates: Vec<Node> = self
                .nodes_named(name)?
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Method))
                .cloned()
                .collect();
            let local: Vec<&Node> = candidates
                .iter()
                .filter(|node| node.file_path == scan.file)
                .collect();
            if local.len() == 1 {
                return Ok(Some(local[0].clone()));
            }
            if local.len() > 1 {
                return Ok(None);
            }
            let head = named.split('.').next().unwrap_or(named);
            if let Some(imported) = self.imported(scan.file, head) {
                let via: Vec<&Node> = candidates
                    .iter()
                    .filter(|node| node.file_path == imported.file)
                    .collect();
                if via.len() == 1 {
                    return Ok(Some(via[0].clone()));
                }
            }
            return Ok((candidates.len() == 1).then(|| candidates[0].clone()));
        }
        let nodes = scan.nodes(self.view)?;
        Ok(enclosing_fn(nodes, line)
            .or_else(|| enclosing_value(nodes, line))
            .cloned())
    }

    /// The nearest class declared at or after `index` (upstream
    /// `classAfter`).
    fn class_after(&self, scan: &Scan<'_>, index: usize) -> Result<Option<Node>> {
        let Some(hit) = CLASS_DECLARATION.captures_at(&scan.text.safe, index) else {
            return Ok(None);
        };
        let line = scan.line(hit.get(0).expect("the whole match").start());
        Ok(scan
            .nodes(self.view)?
            .iter()
            .find(|node| {
                node.kind == NodeKind::Class && node.name == hit[1] && node.start_line >= line - 2
            })
            .cloned())
    }

    /// The method a decorator ending at `end` sits on, inside `class` when
    /// given (upstream `decoratedMethod`).
    fn decorated_method(
        &self,
        scan: &Scan<'_>,
        end: usize,
        class: Option<&Node>,
    ) -> Result<Option<Node>> {
        let Some((name, index)) = method_name_after(&scan.text.safe, end) else {
            return Ok(None);
        };
        let line = scan.line(index);
        Ok(scan
            .nodes(self.view)?
            .iter()
            .find(|node| {
                matches!(node.kind, NodeKind::Method | NodeKind::Function)
                    && node.name == name
                    && node.start_line >= line - 1
                    && node.start_line <= line + 1
                    && class.is_none_or(|class| {
                        node.start_line >= class.start_line && node.end_line <= class.end_line
                    })
            })
            .cloned())
    }

    /// The client calls of `scan` (upstream `collectHttpSites`): bare calls
    /// first, then member calls on a client.
    fn collect_http_sites(&self, scan: &Scan<'_>, sites: &mut Vec<HttpSite>) -> Result<()> {
        let safe = &scan.text.safe;
        for captures in BARE_CLIENT_CALL.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            // `this.fetch(…)` / `repo.fetch(…)` is a project method.
            if whole.start() > 0
                && safe.as_bytes()[whole.start() - 1] == b'.'
                && !GLOBAL_PREFIX.is_match(whole.as_str())
            {
                continue;
            }
            self.add_http_site(scan, whole.start(), whole.end() - 1, None, None, sites)?;
        }
        for captures in MEMBER_CLIENT_CALL.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            let Some(base_url) = self.client_for(scan.file, &scan.bindings, &captures[1]) else {
                continue;
            };
            let verb = &captures[2];
            let verb = verb.strip_prefix('$').unwrap_or(verb).to_ascii_uppercase();
            let verb = (verb != "REQUEST").then_some(verb);
            self.add_http_site(scan, whole.start(), whole.end() - 1, verb, base_url, sites)?;
        }
        Ok(())
    }

    /// The site of the call at `index` whose `(` is at `open`, when its
    /// first argument names a path.
    fn add_http_site(
        &self,
        scan: &Scan<'_>,
        index: usize,
        open: usize,
        verb: Option<String>,
        base_url: Option<String>,
        sites: &mut Vec<HttpSite>,
    ) -> Result<()> {
        let safe = &scan.text.safe;
        let line = scan.line(index);
        let callee = callee_of(&safe[index..open]);
        if scan.route_lines(self.view)?.contains(&line) {
            // A registration the resolver already read.
            return Ok(());
        }
        let Some(function) = enclosing_fn(scan.nodes(self.view)?, line).cloned() else {
            return Ok(());
        };
        let Some(args) = arguments_at(safe, open) else {
            return Ok(());
        };
        let Some(first) = args.first().filter(|first| !first.is_empty()) else {
            return Ok(());
        };
        let mut first = first.as_str();
        let mut method = verb;
        // `axios({ url, method })`, `.request({ url, method })`.
        if js_trim_start(first).starts_with('{') {
            let Some(url) = URL_KEY.find(first) else {
                return Ok(());
            };
            method = Some(
                method
                    .or_else(|| method_in(first))
                    .unwrap_or_else(|| "GET".to_string()),
            );
            first = &first[url.end()..];
        } else if method.is_none() {
            method = Some(method_in(&args[1..].join(",")).unwrap_or_else(|| "GET".to_string()));
        }
        let Some(literal) = leading_string(first) else {
            return Ok(());
        };
        let Some(path) = client_path(&literal, base_url.as_deref()) else {
            return Ok(());
        };
        sites.push(HttpSite {
            function,
            line,
            column: scan.column(index),
            callee,
            method: method.unwrap_or_else(|| "GET".to_string()),
            path,
        });
        Ok(())
    }

    /// The queue producers and consumers of `scan` (upstream
    /// `collectQueue`).
    fn collect_queue(
        &self,
        scan: &Scan<'_>,
        producers: &mut Vec<QueueProducer>,
        consumers: &mut Vec<QueueConsumer>,
    ) -> Result<()> {
        let safe = &scan.text.safe;
        for captures in QUEUE_ADD.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            let receiver = &captures[1];
            let queue = self.queue_for(scan.file, &scan.bindings, receiver);
            if queue.is_none() && !queue_shaped(&last_segment(receiver)) {
                continue;
            }
            let line = scan.line(whole.start());
            let Some(function) = enclosing_fn(scan.nodes(self.view)?, line).cloned() else {
                continue;
            };
            producers.push(QueueProducer {
                function,
                line,
                column: scan.column(whole.start()),
                callee: format!("{}.add", without_space(receiver)),
                queue,
                job: literal_group(&captures, 2).unwrap_or_default().to_string(),
            });
        }

        // Nest: `@Processor('email')` on a class; `@Process('welcome')` on
        // its methods, or a worker host's `process(job)`.
        let jobs = decorators(safe, &PROCESS);
        for processor in decorators(safe, &PROCESSOR) {
            let queue = first_literal(&processor.args);
            let Some(class) = self.class_after(scan, processor.end)? else {
                continue;
            };
            let mut any = false;
            for job in &jobs {
                let line = scan.line(job.index);
                if line < class.start_line || line > class.end_line {
                    continue;
                }
                let Some(method) = self.decorated_method(scan, job.end, Some(&class))? else {
                    continue;
                };
                any = true;
                consumers.push(QueueConsumer {
                    node: method,
                    file: scan.file.to_string(),
                    line,
                    queue: queue.clone(),
                    job: first_literal(&job.args),
                });
            }
            if !any
                && let Some(process) = scan.nodes(self.view)?.iter().find(|node| {
                    node.kind == NodeKind::Method
                        && node.name == "process"
                        && node.start_line >= class.start_line
                        && node.end_line <= class.end_line
                })
            {
                consumers.push(QueueConsumer {
                    node: process.clone(),
                    file: scan.file.to_string(),
                    line: process.start_line,
                    queue: queue.clone(),
                    job: None,
                });
            }
        }

        // BullMQ: `new Worker('email', handler)`.
        for captures in NEW_WORKER.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            let line = scan.line(whole.start());
            let window = scan.text.units_after(whole.end(), 200);
            let Some(node) = self.handler_node(scan, &window, line)? else {
                continue;
            };
            consumers.push(QueueConsumer {
                node,
                file: scan.file.to_string(),
                line,
                queue: literal_group(&captures, 1).map(str::to_string),
                job: None,
            });
        }

        // Bull: `queue.process('welcome', handler)` / `queue.process(handler)`.
        for captures in QUEUE_PROCESS.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            let receiver = &captures[1];
            let queue = self.queue_for(scan.file, &scan.bindings, receiver);
            if queue.is_none() && !queue_shaped(&last_segment(receiver)) {
                continue;
            }
            let line = scan.line(whole.start());
            let window = scan.text.units_after(whole.end(), 200);
            let Some(node) = self.handler_node(scan, &window, line)? else {
                continue;
            };
            consumers.push(QueueConsumer {
                node,
                file: scan.file.to_string(),
                line,
                queue,
                job: literal_group(&captures, 2).map(str::to_string),
            });
        }
        Ok(())
    }

    /// The event dispatches and handlers of `scan` (upstream
    /// `collectEvents`).
    fn collect_events(
        &self,
        scan: &Scan<'_>,
        dispatches: &mut Vec<Dispatch>,
        handlers: &mut Vec<Handler>,
    ) -> Result<()> {
        let safe = &scan.text.safe;
        let side = if scan.socket_server {
            Side::Server
        } else {
            Side::Client
        };
        for captures in EMIT.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            let receiver = &captures[1];
            let event = literal_group(&captures, 3).unwrap_or_default();
            let Some(shape) = shape_of(receiver) else {
                continue;
            };
            if GENERIC_EVENTS.contains(&event) {
                continue;
            }
            let line = scan.line(whole.start());
            let Some(function) = enclosing_fn(scan.nodes(self.view)?, line).cloned() else {
                continue;
            };
            dispatches.push(Dispatch {
                function,
                line,
                column: scan.column(whole.start()),
                callee: format!("{}.{}", without_space(receiver), &captures[2]),
                event: event.to_string(),
                shape,
                side,
            });
        }
        for decorator in decorators(safe, &ON_EVENT) {
            let Some(method) = self.decorated_method(scan, decorator.end, None)? else {
                continue;
            };
            let patterns = if js_trim_start(&decorator.args).starts_with('[') {
                all_literals(&decorator.args)
            } else {
                first_literal(&decorator.args).into_iter().collect()
            };
            let line = scan.line(decorator.index);
            for pattern in patterns {
                handlers.push(Handler {
                    node: method.clone(),
                    file: scan.file.to_string(),
                    line,
                    pattern,
                    kind: Shape::Bus,
                    side,
                });
            }
        }
        for decorator in decorators(safe, &SUBSCRIBE_MESSAGE) {
            let method = self.decorated_method(scan, decorator.end, None)?;
            let event = first_literal(&decorator.args);
            let (Some(method), Some(event)) = (method, event) else {
                continue;
            };
            handlers.push(Handler {
                node: method,
                file: scan.file.to_string(),
                line: scan.line(decorator.index),
                pattern: event,
                kind: Shape::Socket,
                side: Side::Server,
            });
        }
        for captures in SOCKET_ON.captures_iter(safe) {
            let whole = captures.get(0).expect("the whole match");
            let event = literal_group(&captures, 2).unwrap_or_default();
            if shape_of(&captures[1]) != Some(Shape::Socket) || GENERIC_EVENTS.contains(&event) {
                continue;
            }
            let line = scan.line(whole.start());
            let window = scan.text.units_after(whole.end(), 200);
            let Some(node) = self.handler_node(scan, &window, line)? else {
                continue;
            };
            handlers.push(Handler {
                node,
                file: scan.file.to_string(),
                line,
                pattern: event.to_string(),
                kind: Shape::Socket,
                side,
            });
        }
        Ok(())
    }
}

/// Cross-tier channels (upstream `crossTierEdges`): each client call to its
/// route, each queue job to its consumers, each event to its handlers, in
/// that order, one edge a pair.
pub(super) fn cross_tier_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let tier = Tier::new(view);
    let routes = http_routes(view)?;
    let mut http_sites = Vec::new();
    let mut producers = Vec::new();
    let mut consumers = Vec::new();
    let mut dispatches = Vec::new();
    let mut handlers = Vec::new();
    for file in view.files()? {
        if !JS_FILE.is_match(&file) || is_test_path(&file) || is_generated_file(&file) {
            continue;
        }
        let Some(content) = view.text(&file).filter(|content| !content.is_empty()) else {
            continue;
        };
        let wants_http = !routes.is_empty() && HTTP_GATE.is_match(&content);
        let wants_queue = QUEUE_GATE.is_match(&content);
        let wants_events = EVENT_GATE.is_match(&content);
        if !wants_http && !wants_queue && !wants_events {
            continue;
        }
        let text = OffsetText::js(&content);
        let Some(bindings) = tier.scanned_bindings(&file, &text.safe) else {
            continue;
        };
        let scan = Scan {
            file: &file,
            socket_server: SOCKET_SERVER_FILE.is_match(&text.safe),
            text,
            bindings,
            nodes: OnceCell::new(),
            route_lines: OnceCell::new(),
        };
        if wants_http {
            tier.collect_http_sites(&scan, &mut http_sites)?;
        }
        if wants_queue {
            tier.collect_queue(&scan, &mut producers, &mut consumers)?;
        }
        if wants_events {
            tier.collect_events(&scan, &mut dispatches, &mut handlers)?;
        }
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for site in &http_sites {
        let Some(route) = match_http(&site.method, &site.path.segs, site.path.suffix, &routes)
        else {
            continue;
        };
        if route.node.id == site.function.id
            || !seen.insert((site.function.id.clone(), route.node.id.clone()))
        {
            continue;
        }
        let metadata = json!({
            "synthesizedBy": "http-client",
            "channel": "http",
            "callee": site.callee,
            "tier": TIER_CLIENT_TO_SERVER,
            "method": site.method,
            "href": site.path.display,
            "registeredAt": place(&route.node),
        });
        edges.push(tier_edge(
            &site.function,
            &route.node,
            site.line,
            site.column,
            metadata,
        ));
    }
    pair_queue(&producers, &consumers, &mut edges, &mut seen);
    pair_events(&dispatches, &handlers, &mut edges, &mut seen);
    Ok(edges)
}

// ---- 4. Test request → route -----------------------------------------------

static TEST_REQUEST_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?:java|kt|kts|php)$").expect("test request file"));
/// `get|post|put|patch|delete|head|options` (upstream `TEST_VERBS`).
const TEST_VERBS: &str = "get|post|put|patch|delete|head|options";
static MOCKMVC_BUILDERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)MockMvcRequestBuilders(?-u:\b)").expect("mockmvc builders")
});
/// Upstream's `(?<![\w$.])` is checked by hand ([`matches_after`]).
static MOCKMVC_BUILDER: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?:MockMvcRequestBuilders\s*\.\s*)?({TEST_VERBS}|multipart)\s*\("
    ))
});
/// Upstream's `(?<![\w$.])` is checked by hand ([`matches_after`]).
static MOCKMVC_REQUEST: LazyLock<Regex> = LazyLock::new(|| {
    js(r"(?:MockMvcRequestBuilders\s*\.\s*)?request\s*\(\s*HttpMethod\s*\.\s*([A-Z]+)\s*,")
});
static KOTLIN_MOCKMVC: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?-u:\b)[0-9A-Za-z_]*[mM]ockMvc\s*\.\s*({TEST_VERBS})\s*\("
    ))
});
static WEB_TEST_CLIENT: LazyLock<Regex> =
    LazyLock::new(|| js(&format!(r"\.\s*({TEST_VERBS})\s*\(\s*\)\s*\.\s*uri\s*\(")));
static REST_TEMPLATE: LazyLock<Regex> = LazyLock::new(|| {
    js(
        r"(?-u:\b)[0-9A-Za-z_]*[Rr]estTemplate\s*\.\s*(getForEntity|getForObject|postForEntity|postForObject|postForLocation|put|delete|patchForObject|exchange)\s*\(",
    )
});
static HTTP_METHOD_ARGUMENT: LazyLock<Regex> = LazyLock::new(|| js(r"HttpMethod\s*\.\s*([A-Z]+)"));
static REST_ASSURED: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"(?:\.\s*when\s*\(\s*\)\s*|(?-u:\b)RestAssured\s*)\.\s*({TEST_VERBS})\s*\("
    ))
});
static LARAVEL_REQUEST: LazyLock<Regex> =
    LazyLock::new(|| js(&format!(r"->\s*({TEST_VERBS})(Json)?\s*\(")));
static LARAVEL_JSON_CALL: LazyLock<Regex> =
    LazyLock::new(|| js(r#"->\s*(?:json|call)\s*\(\s*(?:'([A-Za-z]+)'|"([A-Za-z]+)")\s*,"#));
/// A project's own request helper, named for its verb: koel's
/// `getAs($url, $user)`, `postAsAdmin(…)` (upstream `LARAVEL_HELPER`).
static LARAVEL_HELPER: LazyLock<Regex> =
    LazyLock::new(|| js(&format!(r"->\s*(({TEST_VERBS})[A-Z][0-9A-Za-z_]*)\s*\(")));
/// A Laravel request made in a method body: the call a request helper
/// must end in (upstream `LARAVEL_REQUEST_IN_BODY`).
static LARAVEL_REQUEST_IN_BODY: LazyLock<Regex> = LazyLock::new(|| {
    js(&format!(
        r"->\s*(?:(?:{TEST_VERBS})(?:Json)?|json|call)\s*\("
    ))
});
static THIS_CALL: LazyLock<Regex> = LazyLock::new(|| js(r"\$this\s*->\s*([0-9A-Za-z_]+)\s*\("));
/// Pest's `get('/x')` / `postJson(…)` helpers; upstream's
/// `(?<![\w$>:\\])` is checked by hand ([`matches_after`]).
static PEST_REQUEST: LazyLock<Regex> =
    LazyLock::new(|| js(&format!(r"({TEST_VERBS})(Json)?\s*\(")));
static TEST_REQUEST_GATE: LazyLock<Regex> = LazyLock::new(|| {
    js(
        r"[mM]ockMvc|[wW]ebTestClient(?-u:\b)|[rR]estTemplate(?-u:\b)|(?-u:\b)RestAssured(?-u:\b)|\.\s*when\s*\(\s*\)|\$this\s*->|Pest\\Laravel",
    )
});
static SPRING_TEMPLATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{[^{}/]*\}").expect("spring template"));

/// Whether a project has the files the test pass reads (upstream's gate
/// `has('java', 'kotlin', 'php')`).
pub(super) fn test_request_gate(languages: &HashSet<Language>) -> bool {
    [Language::Java, Language::Kotlin, Language::Php]
        .iter()
        .any(|language| languages.contains(language))
}

/// The language a test file's requests are read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestLang {
    Java,
    Kotlin,
    Php,
}

/// A request a test makes.
#[derive(Debug, Clone)]
struct TestSite {
    from: Node,
    line: i64,
    column: i64,
    callee: String,
    method: String,
    segs: Vec<String>,
    display: String,
}

/// A JavaScript identifier byte, `[\w$]`.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// An ASCII word byte, JavaScript's `\w`.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Each match of `pattern` in `text` whose start does not follow a byte
/// `excluded` takes, as a JavaScript `g` regex with the one-character
/// lookbehind `(?<![…])` finds them: a start that fails is retried one
/// character later, a match continues after its end. A non-ASCII
/// character before a start is no excluded byte, as no excluded code unit.
fn matches_after<'t>(
    pattern: &Regex,
    text: &'t str,
    excluded: impl Fn(u8) -> bool,
) -> Vec<Captures<'t>> {
    let mut out = Vec::new();
    let mut from = 0;
    while from <= text.len() {
        let Some(found) = pattern.captures_at(text, from) else {
            break;
        };
        let whole = found.get(0).expect("the whole match");
        if whole.start() == 0 || !excluded(text.as_bytes()[whole.start() - 1]) {
            from = whole.end().max(whole.start() + 1);
            out.push(found);
        } else {
            from = whole.start() + 1;
        }
        while from < text.len() && !text.is_char_boundary(from) {
            from += 1;
        }
    }
    out
}

/// The string a request's path argument builds from `start` (upstream
/// `readRequestPath`): literals verbatim, an escape's character kept, each
/// computed piece (a concatenated operand, an interpolation, in Java and
/// Kotlin a Spring `{var}` template) as one [`HOLE`]. `None` unless it
/// starts with a literal: `route('songs.index')` or `$url` names no path.
fn read_request_path(s: &str, start: usize, lang: TestLang) -> Option<String> {
    let b = s.as_bytes();
    let concat = if lang == TestLang::Php { b'.' } else { b'+' };
    let is_concat = |at: usize| {
        b.get(at) == Some(&concat)
            && !(concat == b'.' && b.get(at + 1).is_some_and(u8::is_ascii_digit))
    };
    let word_at = |at: usize| b.get(at).copied().is_some_and(is_word_byte);
    let mut out = String::new();
    let mut literal = false;
    let mut i = start;
    loop {
        i = skip_space(s, i);
        match b.get(i).copied() {
            Some(quote @ (b'"' | b'\'')) => {
                let interpolates = quote == b'"' && lang != TestLang::Java;
                i += 1;
                while i < b.len() && b[i] != quote {
                    let c = b[i];
                    if c == b'\\' {
                        i += 1;
                        if let Some(escaped) = s[i..].chars().next() {
                            out.push(escaped);
                            i += escaped.len_utf8();
                        }
                        continue;
                    }
                    if interpolates
                        && c == b'$'
                        && b.get(i + 1)
                            .is_some_and(|&next| next == b'{' || is_word_byte(next))
                    {
                        // `${expr}`, `$name`, PHP's `$user->id` / `$row['k']`.
                        if b[i + 1] == b'{' {
                            i = close_brace(b, i + 1);
                        } else {
                            while word_at(i + 1) {
                                i += 1;
                            }
                            if lang == TestLang::Php {
                                while b.get(i + 1..i + 3) == Some(b"->".as_slice())
                                    && word_at(i + 3)
                                {
                                    i += 2;
                                    while word_at(i + 1) {
                                        i += 1;
                                    }
                                }
                            }
                        }
                        out.push(HOLE);
                        i += 1;
                        continue;
                    }
                    if interpolates
                        && lang == TestLang::Php
                        && c == b'{'
                        && b.get(i + 1) == Some(&b'$')
                    {
                        i = close_brace(b, i);
                        out.push(HOLE);
                        i += 1;
                        continue;
                    }
                    let ch = s[i..].chars().next().expect("a character at a boundary");
                    out.push(ch);
                    i += ch.len_utf8();
                }
                if i >= b.len() {
                    return None;
                }
                i += 1;
                literal = true;
            }
            _ => {
                if !literal {
                    return None;
                }
                let end = operand_end(b, i, concat);
                if end == i {
                    break;
                }
                out.push(HOLE);
                i = end;
            }
        }
        i = skip_space(s, i);
        if is_concat(i) {
            i += 1;
            continue;
        }
        break;
    }
    if !literal {
        return None;
    }
    Some(if lang == TestLang::Php {
        out
    } else {
        // Spring's URI templates: `get("/owners/{ownerId}", id)`.
        SPRING_TEMPLATE
            .replace_all(&out, HOLE.to_string().as_str())
            .into_owned()
    })
}

/// Index of the `}` closing the `{` at `open`, or the last index of `s`
/// (upstream `closeBrace`).
fn close_brace(s: &[u8], open: usize) -> usize {
    let mut depth = 0i64;
    for (at, &byte) in s.iter().enumerate().skip(open) {
        if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth -= 1;
            if depth == 0 {
                return at;
            }
        }
    }
    s.len().saturating_sub(1)
}

/// End of a concatenated operand starting at `i`: the next top-level
/// `,`, `;`, `)` or concatenation (upstream `operandEnd`), at most the end
/// of `s`.
fn operand_end(s: &[u8], mut i: usize, concat: u8) -> usize {
    let mut depth = 0i64;
    while i < s.len() {
        let c = s[i];
        if c == b'"' || c == b'\'' {
            i += 1;
            while i < s.len() && s[i] != c {
                if s[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == b'(' || c == b'[' {
            depth += 1;
        } else if c == b')' || c == b']' {
            if depth == 0 {
                return i;
            }
            depth -= 1;
        } else if depth == 0
            && (c == b','
                || c == b';'
                || (c == concat
                    && !(concat == b'.' && s.get(i + 1).is_some_and(u8::is_ascii_digit))))
        {
            return i;
        }
        // `$user->id`: an arrow is part of the operand.
        if c == b'-' && s.get(i + 1) == Some(&b'>') {
            i += 1;
        }
        i += 1;
    }
    i.min(s.len())
}

/// A test URL as route segments, app-relative, a hole as `*` (upstream
/// `testRequestPath`); `None` when nothing literal is left. Laravel's
/// `'api/me'` needs no leading `/`.
fn test_request_path(raw: &str) -> Option<(Vec<String>, String)> {
    let p = without_query(raw);
    let p = absolute_path(p).unwrap_or_else(|| p.to_string());
    let (segs, display) = hole_segments(&p);
    if !segs.is_empty() && segs.iter().all(|seg| seg == "*") {
        return None;
    }
    Some((segs, display))
}

/// Whether `name` is a request helper of the project's tests (upstream
/// `isRequestHelper`): a PHP method of a test file whose body, as
/// `bodies` reads them, makes a Laravel request, directly or through one
/// more `$this->helper(…)`. A "no" found below the top level is not
/// remembered.
fn is_request_helper(
    name: &str,
    bodies: &mut dyn FnMut(&str) -> Result<Vec<String>>,
    memo: &mut HashMap<String, bool>,
    depth: usize,
) -> Result<bool> {
    if let Some(&known) = memo.get(name) {
        return Ok(known);
    }
    // A cycle guard.
    memo.insert(name.to_string(), false);
    let mut ok = false;
    for body in bodies(name)? {
        if LARAVEL_REQUEST_IN_BODY.is_match(&body) {
            ok = true;
        } else if depth == 0 {
            for captures in THIS_CALL.captures_iter(&body) {
                let callee = &captures[1];
                if callee != name && is_request_helper(callee, bodies, memo, depth + 1)? {
                    ok = true;
                    break;
                }
            }
        }
        if ok {
            break;
        }
    }
    if ok || depth == 0 {
        memo.insert(name.to_string(), ok);
    } else {
        memo.remove(name);
    }
    Ok(ok)
}

/// Whether the `->` at `arrow` ends a chain rooted at `$this` that only
/// calls methods (upstream `thisRootedCalls`): `$this->actingAs($u)->` is,
/// `$this->app->get(` (a container lookup through a property) is not. Up to
/// four `$this` roots back are tried.
fn this_rooted_calls(s: &str, arrow: usize) -> bool {
    let b = s.as_bytes();
    let mut from = arrow;
    for _ in 0..4 {
        let Some(root) = last_this_at_or_before(b, from as isize - 1) else {
            return false;
        };
        let mut i = root + "$this".len();
        loop {
            i = skip_space(s, i);
            if i == arrow {
                return true;
            }
            if b.get(i..i + 2) != Some(b"->".as_slice()) {
                break;
            }
            i = skip_space(s, i + 2);
            let ident = b[i..]
                .iter()
                .take(64)
                .take_while(|&&byte| is_word_byte(byte))
                .count();
            if ident == 0 {
                break;
            }
            i = skip_space(s, i + ident);
            if b.get(i) != Some(&b'(') {
                break;
            }
            let Some(close) = close_paren(b, i) else {
                break;
            };
            i = close + 1;
        }
        from = root;
    }
    false
}

/// The start of the last `$this` that starts at or before `at` (upstream
/// `lastIndexOf('$this', at)`, a negative `at` read as 0).
fn last_this_at_or_before(s: &[u8], at: isize) -> Option<usize> {
    let pattern = b"$this";
    let at = at.max(0) as usize;
    let end = (at + pattern.len()).min(s.len());
    s[..end]
        .windows(pattern.len())
        .rposition(|window| window == pattern)
}

/// One test file the test pass reads.
struct TestScan<'t> {
    file: &'t str,
    lang: TestLang,
    text: OffsetText<'t>,
    nodes: OnceCell<Vec<Node>>,
}

impl TestScan<'_> {
    /// The site of the request at `index` whose path argument starts at
    /// `arg_at`, when it names a path.
    fn add(
        &self,
        view: &View<'_>,
        sites: &mut Vec<TestSite>,
        index: usize,
        arg_at: usize,
        method: &str,
        callee: String,
    ) -> Result<()> {
        let Some(raw) = read_request_path(&self.text.safe, arg_at, self.lang) else {
            return Ok(());
        };
        let Some((segs, display)) = test_request_path(&raw) else {
            return Ok(());
        };
        let line = self.text.line_at(index);
        let nodes = match self.nodes.get() {
            Some(nodes) => nodes,
            None => {
                let nodes = view.nodes_in_file(self.file)?;
                self.nodes.get_or_init(|| nodes)
            }
        };
        let Some(from) = enclosing_fn(nodes, line)
            .or_else(|| nodes.iter().find(|node| node.kind == NodeKind::File))
            .cloned()
        else {
            return Ok(());
        };
        sites.push(TestSite {
            from,
            line,
            column: column_of(&self.text, index),
            callee,
            method: method.to_ascii_uppercase(),
            segs,
            display,
        });
        Ok(())
    }
}

/// The requests `file`, a Spring or Laravel test, makes (upstream
/// `collectTestRequests`), in upstream's order of patterns.
fn collect_test_requests(
    view: &View<'_>,
    file: &str,
    sites: &mut Vec<TestSite>,
    helpers: &mut HashMap<String, bool>,
    bodies: &mut dyn FnMut(&str) -> Result<Vec<String>>,
) -> Result<()> {
    let Some(content) = view.text(file).filter(|content| !content.is_empty()) else {
        return Ok(());
    };
    if !TEST_REQUEST_GATE.is_match(&content) {
        return Ok(());
    }
    let lang = if file.ends_with(".php") {
        TestLang::Php
    } else if file.ends_with(".kt") || file.ends_with(".kts") {
        TestLang::Kotlin
    } else {
        TestLang::Java
    };
    let text = if lang == TestLang::Php {
        OffsetText::php(&content)
    } else {
        OffsetText::java(&content)
    };
    let scan = TestScan {
        file,
        lang,
        text,
        nodes: OnceCell::new(),
    };
    let safe = &scan.text.safe;
    let at = |captures: &Captures<'_>| {
        let whole = captures.get(0).expect("the whole match");
        (whole.start(), whole.end())
    };
    let suffix = |captures: &Captures<'_>, group: usize| {
        captures
            .get(group)
            .map_or_else(String::new, |found| found.as_str().to_string())
    };

    if lang == TestLang::Php {
        for captures in LARAVEL_REQUEST.captures_iter(safe) {
            let (index, open) = at(&captures);
            if this_rooted_calls(safe, index) {
                let callee = format!("$this->{}{}", &captures[1], suffix(&captures, 2));
                scan.add(view, sites, index, open, &captures[1], callee)?;
            }
        }
        for captures in LARAVEL_JSON_CALL.captures_iter(safe) {
            let (index, open) = at(&captures);
            if this_rooted_calls(safe, index) {
                let verb = captures
                    .get(1)
                    .or_else(|| captures.get(2))
                    .map_or("", |found| found.as_str());
                scan.add(view, sites, index, open, verb, "$this->json".to_string())?;
            }
        }
        for captures in LARAVEL_HELPER.captures_iter(safe) {
            let (index, open) = at(&captures);
            let name = &captures[1];
            // `getJson` / `postJson`, read above.
            if TEST_VERBS
                .split('|')
                .any(|verb| name == format!("{verb}Json"))
            {
                continue;
            }
            if this_rooted_calls(safe, index) && is_request_helper(name, bodies, helpers, 0)? {
                scan.add(
                    view,
                    sites,
                    index,
                    open,
                    &captures[2],
                    format!("$this->{name}"),
                )?;
            }
        }
        if safe.contains("Pest\\Laravel") {
            let excluded =
                |byte: u8| is_word_byte(byte) || matches!(byte, b'$' | b'>' | b':' | b'\\');
            for captures in matches_after(&PEST_REQUEST, safe, excluded) {
                let (index, open) = at(&captures);
                let callee = format!("{}{}", &captures[1], suffix(&captures, 2));
                scan.add(view, sites, index, open, &captures[1], callee)?;
            }
        }
        return Ok(());
    }
    if MOCKMVC_BUILDERS.is_match(safe) {
        let excluded = |byte: u8| is_name_byte(byte) || byte == b'.';
        for captures in matches_after(&MOCKMVC_BUILDER, safe, excluded) {
            let (index, open) = at(&captures);
            let verb = &captures[1];
            let method = if verb == "multipart" { "post" } else { verb };
            scan.add(view, sites, index, open, method, verb.to_string())?;
        }
        for captures in matches_after(&MOCKMVC_REQUEST, safe, excluded) {
            let (index, open) = at(&captures);
            scan.add(
                view,
                sites,
                index,
                open,
                &captures[1],
                "request".to_string(),
            )?;
        }
    }
    for captures in KOTLIN_MOCKMVC.captures_iter(safe) {
        let (index, open) = at(&captures);
        let callee = format!("mockMvc.{}", &captures[1]);
        scan.add(view, sites, index, open, &captures[1], callee)?;
    }
    for captures in WEB_TEST_CLIENT.captures_iter(safe) {
        let (index, open) = at(&captures);
        let callee = format!("{}().uri", &captures[1]);
        scan.add(view, sites, index, open, &captures[1], callee)?;
    }
    for captures in REST_ASSURED.captures_iter(safe) {
        let (index, open) = at(&captures);
        scan.add(
            view,
            sites,
            index,
            open,
            &captures[1],
            captures[1].to_string(),
        )?;
    }
    for captures in REST_TEMPLATE.captures_iter(safe) {
        let (index, open) = at(&captures);
        let name = &captures[1];
        let method = if name == "exchange" {
            arguments_at(safe, open - 1)
                .as_ref()
                .and_then(|args| args.get(1))
                .and_then(|second| HTTP_METHOD_ARGUMENT.captures(second))
                .map(|found| found[1].to_string())
        } else if name.starts_with("get") {
            Some("GET".to_string())
        } else if name.starts_with("post") {
            Some("POST".to_string())
        } else if name == "put" {
            Some("PUT".to_string())
        } else if name == "delete" {
            Some("DELETE".to_string())
        } else if name.starts_with("patch") {
            Some("PATCH".to_string())
        } else {
            None
        };
        if let Some(method) = method {
            let callee = format!("restTemplate.{name}");
            scan.add(view, sites, index, open, &method, callee)?;
        }
    }
    Ok(())
}

/// Test requests (upstream `testRequestEdges`): each Spring or Laravel
/// test's request to the route it reaches, from the test method, or the
/// file for a Pest closure; one edge a pair, matched without a base URL.
pub(super) fn test_request_edges(view: &View<'_>) -> Result<Vec<Edge>> {
    let routes = http_routes(view)?;
    if routes.is_empty() {
        return Ok(Vec::new());
    }
    let mut sites = Vec::new();
    let mut helpers = HashMap::new();
    let mut read: HashMap<String, Vec<String>> = HashMap::new();
    let mut bodies = |name: &str| -> Result<Vec<String>> {
        if let Some(found) = read.get(name) {
            return Ok(found.clone());
        }
        let mut out = Vec::new();
        for node in view.nodes_named(name)? {
            if node.kind != NodeKind::Method
                || node.language != Language::Php
                || !is_test_path(&node.file_path)
            {
                continue;
            }
            let Some(text) = view.text(&node.file_path) else {
                continue;
            };
            if let Some(body) = slice_lines(&text, node.start_line, node.end_line) {
                out.push(body);
            }
        }
        read.insert(name.to_string(), out.clone());
        Ok(out)
    };
    for file in view.files()? {
        if !TEST_REQUEST_FILE.is_match(&file) || !is_test_path(&file) {
            continue;
        }
        collect_test_requests(view, &file, &mut sites, &mut helpers, &mut bodies)?;
    }

    let mut edges = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for site in &sites {
        let Some(route) = match_http(&site.method, &site.segs, false, &routes) else {
            continue;
        };
        if !seen.insert((site.from.id.clone(), route.node.id.clone())) {
            continue;
        }
        let metadata = json!({
            "synthesizedBy": "test-request",
            "channel": "http",
            "callee": site.callee,
            "method": site.method,
            "href": site.display,
            "registeredAt": place(&route.node),
        });
        edges.push(tier_edge(
            &site.from,
            &route.node,
            site.line,
            site.column,
            metadata,
        ));
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    //! The expected values are upstream's: its helpers and regexes at
    //! `e8ddc4e2`, run on the same inputs.

    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use codegraph_core::node_id::hash_content;
    use codegraph_core::types::FileRecord;
    use codegraph_store::Store;

    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// `text` with each `<HOLE>` as [`HOLE`].
    fn holed(text: &str) -> String {
        text.replace("<HOLE>", &HOLE.to_string())
    }

    fn node(
        id: &str,
        kind: NodeKind,
        name: &str,
        file: &str,
        language: Language,
        lines: (i64, i64),
    ) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: file.to_string(),
            language,
            start_line: lines.0,
            end_line: lines.1,
            start_column: 0,
            end_column: 1,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: true,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    /// A project of hand-written files whose index holds exactly the nodes
    /// given; a file `disk_only` is on disk and not in the index.
    struct Fixture {
        dir: PathBuf,
        files: Vec<(String, String, Language)>,
        disk_only: Vec<(String, String)>,
        nodes: Vec<Node>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cg-tier-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self {
                dir,
                files: Vec::new(),
                disk_only: Vec::new(),
                nodes: Vec::new(),
            }
        }

        fn file(mut self, path: &str, language: Language, text: &str) -> Self {
            self.files
                .push((path.to_string(), text.to_string(), language));
            self
        }

        fn disk_only(mut self, path: &str, text: &str) -> Self {
            self.disk_only.push((path.to_string(), text.to_string()));
            self
        }

        fn node(mut self, node: Node) -> Self {
            self.nodes.push(node);
            self
        }

        fn write(&self, path: &str, text: &str) {
            let full = self.dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, text).unwrap();
        }

        /// The index of the fixture's files and nodes, its files written.
        fn store(&self) -> Store {
            let mut store = Store::open(&self.dir.join("index.db")).unwrap();
            store.upsert_nodes(&self.nodes).unwrap();
            for (path, text, language) in &self.files {
                self.write(path, text);
                store
                    .upsert_file(&FileRecord {
                        path: path.clone(),
                        content_hash: hash_content(text),
                        language: *language,
                        size: 0,
                        modified_at: 0,
                        indexed_at: 0,
                        node_count: 0,
                        errors: Vec::new(),
                        generated: false,
                    })
                    .unwrap();
            }
            for (path, text) in &self.disk_only {
                self.write(path, text);
            }
            store
        }

        /// The edges the pass naming itself `synthesized_by` draws, as the
        /// stage merges them.
        fn edges(&self, synthesized_by: &str) -> Vec<Edge> {
            let store = self.store();
            crate::synthesis::synthesize(&store, &self.dir, 1 << 20)
                .unwrap()
                .into_iter()
                .filter(|edge| {
                    edge.metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata["synthesizedBy"] == synthesized_by)
                })
                .collect()
        }

        /// The edges `pass` draws, before the stage merges the passes'.
        fn pass(&self, pass: fn(&View<'_>) -> Result<Vec<Edge>>) -> Vec<Edge> {
            let store = self.store();
            pass(&View::new(&store, &self.dir, 1 << 20)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    // ---- Source reading ----

    #[test]
    fn a_call_closes_past_strings_and_templates() {
        assert_eq!(close_paren(br#"f(a, "x)", 'y)', (b))"#, 1), Some(20));
        assert_eq!(close_paren(br#"f(`t${(1)}`, ")")"#, 1), Some(16));
        assert_eq!(close_paren(br#"f("a\")", x)"#, 1), Some(11));
        assert_eq!(close_paren(b"f(a", 1), None);
        assert_eq!(close_paren(br#"f(`a${`b${")"}`}c`)"#, 1), Some(18));
        // A template nests through its interpolations; an interpolation
        // counts braces and nothing else.
        assert_eq!(template_end(b"`a${`b${c}`}d`", 0), 13);
        assert_eq!(template_end(b"`a\\`b`", 0), 5);
        assert_eq!(template_end(br#"`a${ "}" }b`"#, 0), 11);
        assert_eq!(template_end(b"`unterminated ${x", 0), 17);
        assert_eq!(template_end(b"`a${{b:1}}c`", 0), 11);
    }

    #[test]
    fn arguments_split_at_top_level_commas_keeping_an_empty_first() {
        let args = |s: &str| arguments_at(s, 1);
        assert_eq!(args("f(, b)"), Some(strings(&["", "b"])));
        assert_eq!(args("f(a, , b)"), Some(strings(&["a", "b"])));
        assert_eq!(args("f()"), Some(strings(&[""])));
        assert_eq!(
            args("f(a, {b, c}, [d, e], \"f, g\", `h, ${i, j}`, (k, l))"),
            Some(strings(&[
                "a",
                "{b, c}",
                "[d, e]",
                "\"f, g\"",
                "`h, ${i, j}`",
                "(k, l)"
            ]))
        );
        assert_eq!(args("f( a ,b )"), Some(strings(&["a", "b"])));
        assert_eq!(args("f(a"), None);
    }

    #[test]
    fn a_leading_string_holes_kept_and_new_url_read_through() {
        assert_eq!(leading_string("'/x'").as_deref(), Some("/x"));
        assert_eq!(
            leading_string("new URL('/users', base)").as_deref(),
            Some("/users")
        );
        assert_eq!(leading_string("(  \"/a\")").as_deref(), Some("/a"));
        assert_eq!(leading_string("`/u/${id}/o`"), Some(holed("/u/<HOLE>/o")));
        assert_eq!(leading_string("url"), None);
        assert_eq!(leading_string("new URL(path, base)"), None);
        assert_eq!(
            leading_string("new URL(new URL('/n', b), c)").as_deref(),
            Some("/n")
        );
        assert_eq!(leading_string("  '/p\\'q'  ").as_deref(), Some("/p'q"));
    }

    /// Each literal is a quote, a body without one and the same quote: an
    /// unmatched quote opens no literal.
    #[test]
    fn literals_and_methods_need_matching_quotes() {
        assert_eq!(method_in("{ method: 'post' }").as_deref(), Some("POST"));
        assert_eq!(method_in("{ method: \"GET' }"), None);
        assert_eq!(method_in("{ method: m }"), None);
        assert_eq!(method_in("{ xmethod: 'PUT' }"), None);
        assert_eq!(
            method_in("{ method : `delete` }").as_deref(),
            Some("DELETE")
        );
        assert_eq!(first_literal("'job\""), None);
        assert_eq!(first_literal("{ name: 'x' }").as_deref(), Some("x"));
        assert_eq!(first_literal("['a', \"b\", `c`]").as_deref(), Some("a"));
        assert_eq!(first_literal("`a'b`"), None);
        assert_eq!(first_literal("'a' + \"b\"").as_deref(), Some("a"));
        assert_eq!(all_literals("['a', \"b\", `c`]"), strings(&["a", "b", "c"]));
        assert_eq!(all_literals("'a\" 'b'"), strings(&["b"]));
        assert!(all_literals("x").is_empty());
    }

    #[test]
    fn a_decorated_method_is_named_past_decorators_and_modifiers() {
        let named = |s: &str, from: usize| method_name_after(s, from);
        assert_eq!(
            named("@Process('x')\n  @Other() async handle(job) {", 13),
            Some(("handle".to_string(), 31))
        );
        assert_eq!(
            named("  public static readonly override foo<T>(x) {}", 0),
            Some(("foo".to_string(), 34))
        );
        assert_eq!(named("  @Deco(a)(b) bar(", 0), None);
        assert_eq!(named("  @Bad( x", 0), None);
        assert_eq!(
            named("  readonlyish(", 0),
            Some(("readonlyish".to_string(), 2))
        );
        let found = decorators(
            "@Process('a') m() {}\n@Process ( { name: 'b' } ) n() {}\n@Processor('q')\n@Process(",
            &PROCESS,
        );
        assert_eq!(
            found,
            [
                Decorator {
                    args: "'a'".to_string(),
                    index: 0,
                    end: 13
                },
                Decorator {
                    args: " { name: 'b' } ".to_string(),
                    index: 21,
                    end: 47
                },
            ]
        );
    }

    // ---- HTTP ----

    #[test]
    fn a_route_name_is_a_method_and_a_slash_path() {
        let shape = |name: &str| route_shape(name);
        assert_eq!(
            shape("GET /api/users"),
            Some(("GET".to_string(), strings(&["api", "users"])))
        );
        assert_eq!(shape("get /x"), Some(("GET".to_string(), strings(&["x"]))));
        assert_eq!(
            shape("ANY  /x "),
            Some(("ANY".to_string(), strings(&["x"])))
        );
        assert_eq!(shape("POST /"), Some(("POST".to_string(), Vec::new())));
        assert_eq!(shape("/about"), None);
        assert_eq!(shape(" GET /x"), None);
        assert_eq!(shape("FETCH /x"), None);
        assert_eq!(shape("GET about"), None);
        for verb in [
            "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "ALL", "ANY",
        ] {
            assert_eq!(
                shape(&format!("{verb} /x")),
                Some((verb.to_string(), strings(&["x"]))),
                "{verb}"
            );
        }
    }

    #[test]
    fn a_path_scores_literals_over_parameters_over_catch_alls() {
        let score = |client: &str, route: &str| score_path(&segments(client), &segments(route));
        for (client, route, expected) in [
            ("/api/users", "/api/users", Some(6)),
            ("/api/users", "/api/:id", Some(5)),
            ("/api/*", "/api/:id", Some(5)),
            // A hole never stands for a literal segment.
            ("/api/*", "/api/users", None),
            ("/a/b/c", "/a/*", Some(5)),
            ("/a", "/a/[...slug]", None),
            ("/a/b", "/a/{*rest}", Some(4)),
            ("/a/b", "/a/:rest*", Some(4)),
            ("/a/b", "/a/{rest:*}x", Some(4)),
            ("/a/b", "/a/*name", Some(4)),
            ("/a/b", "/a/b/c", None),
            ("/a/b/c", "/a/b", None),
            ("/", "/", Some(0)),
            ("/a/b", "/a/b?", Some(5)),
            ("/a/x", "/a/<id>", Some(5)),
            ("/a/x", "/a/[id]", Some(5)),
            // A catch-all is any segment starting with `*`, and takes one
            // segment at least.
            ("/a/x.json", "/a/*.json", Some(4)),
            ("/a/b/c", "/a/[...slug]", Some(5)),
        ] {
            assert_eq!(score(client, route), expected, "{client} {route}");
        }
    }

    #[test]
    fn a_call_reaches_its_one_best_route() {
        let found = |method: &str, path: &str, suffix: bool, specs: &[(&str, &str)]| {
            let routes: Vec<HttpRoute> = specs
                .iter()
                .map(|(id, name)| {
                    let (method, segs) = route_shape(name).expect("a route name");
                    HttpRoute {
                        node: node(
                            id,
                            NodeKind::Route,
                            name,
                            "src/server.ts",
                            Language::TypeScript,
                            (1, 1),
                        ),
                        method,
                        segs,
                    }
                })
                .collect();
            match_http(method, &segments(path), suffix, &routes).map(|route| route.node.id.clone())
        };
        let users = [("p", "GET /api/users/:id"), ("l", "GET /api/users/me")];
        assert_eq!(
            found("GET", "/api/users/me", false, &users).as_deref(),
            Some("l")
        );
        assert_eq!(found("GET", "/api/users/*", false, &users[1..]), None);
        assert_eq!(
            found("GET", "/api/users/*", false, &users).as_deref(),
            Some("p")
        );
        assert_eq!(
            found("GET", "/docs/a/b", false, &[("c", "GET /docs/*")]).as_deref(),
            Some("c")
        );
        assert_eq!(
            found("GET", "/docs/a/b", false, &[("s", "GET /docs/:slug")]),
            None
        );
        assert_eq!(
            found("POST", "/api/x", false, &[("a", "ALL /api/x")]).as_deref(),
            Some("a")
        );
        assert_eq!(
            found("DELETE", "/api/x", false, &[("n", "ANY /api/x")]).as_deref(),
            Some("n")
        );
        assert_eq!(found("GET", "/api/x", false, &[("p", "POST /api/x")]), None);
        // A tie draws nothing; a better score after it wins.
        let tied = [("a", "GET /api/:a"), ("b", "GET /api/:b")];
        assert_eq!(found("GET", "/api/x", false, &tied), None);
        let broken = [tied[0], tied[1], ("x", "GET /api/x")];
        assert_eq!(found("GET", "/api/x", false, &broken).as_deref(), Some("x"));
        let first = [("x", "GET /api/x"), tied[0], tied[1]];
        assert_eq!(found("GET", "/api/x", false, &first).as_deref(), Some("x"));
        assert_eq!(
            found(
                "GET",
                "/x/y",
                false,
                &[("g", "GET /x/y"), ("a", "ALL /x/y")]
            ),
            None
        );
        // Behind a base URL: the route's tail, two segments at least, two
        // more at most.
        assert_eq!(
            found("GET", "/users/*", true, &[("r", "GET /api/users/:id")]).as_deref(),
            Some("r")
        );
        assert_eq!(
            found("GET", "/users/*", true, &[("r", "GET /api/v1/users/:id")]).as_deref(),
            Some("r")
        );
        assert_eq!(
            found("GET", "/users/*", true, &[("r", "GET /a/b/c/users/:id")]),
            None
        );
        assert_eq!(
            found("GET", "/users", true, &[("r", "GET /api/users")]),
            None
        );
        assert_eq!(
            found("GET", "/api/users", true, &[("r", "GET /api/users")]).as_deref(),
            Some("r")
        );
        assert_eq!(
            found(
                "GET",
                "/users/*",
                true,
                &[("a", "GET /v1/users/:id"), ("b", "GET /v2/users/:id")]
            ),
            None
        );
        assert_eq!(found("GET", "/a/b/c", true, &[("r", "GET /b/c")]), None);
        assert_eq!(found("GET", "/a", false, &[]), None);
        assert_eq!(
            found("GET", "/", false, &[("r", "GET /"), ("a", "GET /a")]).as_deref(),
            Some("r")
        );
    }

    #[test]
    fn a_client_path_joins_its_base_and_holes_its_template() {
        let path = |raw: &str, base: Option<&str>| {
            let base = base.map(holed);
            client_path(&holed(raw), base.as_deref())
                .map(|path| (path.segs, path.suffix, path.display))
        };
        let shown = |segs: &[&str], suffix: bool, display: &str| {
            Some((strings(segs), suffix, display.to_string()))
        };
        assert_eq!(
            path("/api/users?x=1#h", None),
            shown(&["api", "users"], false, "/api/users")
        );
        assert_eq!(
            path("https://api.example.com/v1/charges", None),
            shown(&["v1", "charges"], false, "/v1/charges")
        );
        assert_eq!(path("//cdn.example.com", None), shown(&[], false, "/"));
        assert_eq!(path("https://host", None), shown(&[], false, "/"));
        assert_eq!(
            path("users", Some("/api")),
            shown(&["api", "users"], false, "/api/users")
        );
        assert_eq!(path("users", None), None);
        assert_eq!(
            path("/users", Some("/api")),
            shown(&["api", "users"], false, "/api/users")
        );
        assert_eq!(
            path("/users", Some("https://x.test/v2")),
            shown(&["v2", "users"], false, "/v2/users")
        );
        assert_eq!(
            path("/users", Some("<HOLE>")),
            shown(&["users"], true, "/users")
        );
        assert_eq!(
            path("<HOLE>/api/users/<HOLE>/orders", None),
            shown(
                &["api", "users", "*", "orders"],
                true,
                "/api/users/${…}/orders"
            )
        );
        assert_eq!(path("<HOLE>x", None), None);
        assert_eq!(
            path("/u/<HOLE>", None),
            shown(&["u", "*"], false, "/u/${…}")
        );
        assert_eq!(path("/<HOLE>/<HOLE>", None), None);
        assert_eq!(
            path("users", Some("<HOLE>/v1")),
            shown(&["v1", "users"], true, "/v1/users")
        );
        assert_eq!(
            path("users", Some("<HOLE>")),
            shown(&["users"], true, "/users")
        );
        assert_eq!(
            path("/users", Some("<HOLE>/base")),
            shown(&["base", "users"], true, "/base/users")
        );
        assert_eq!(
            path("users", Some("https://h.test")),
            shown(&["users"], false, "/users")
        );
        assert_eq!(path("users", Some("rel")), None);
        // JavaScript's `.` stops at a line terminator, `[^/]` does not.
        assert_eq!(path("/a\r/b", None), shown(&["a\r", "b"], false, "/a\r/b"));
        assert_eq!(path("https://h/a\rb", None), None);
        assert_eq!(path("/a b/c", None), shown(&["a b", "c"], false, "/a b/c"));
    }

    /// Upstream's `CHAIN_START` lookbehind dropped: a chain matched from
    /// inside a word would have matched from its start first.
    #[test]
    fn client_calls_are_read_from_a_word_start() {
        let member = |s: &str| -> Vec<(usize, String, String, String)> {
            MEMBER_CLIENT_CALL
                .captures_iter(s)
                .map(|c| {
                    let whole = c.get(0).unwrap();
                    (
                        whole.start(),
                        whole.as_str().to_string(),
                        c[1].to_string(),
                        c[2].to_string(),
                    )
                })
                .collect()
        };
        let row = |at: usize, whole: &str, receiver: &str, verb: &str| {
            (
                at,
                whole.to_string(),
                receiver.to_string(),
                verb.to_string(),
            )
        };
        assert_eq!(
            member("myaxios.get('/a'); api.post('/b')"),
            [
                row(0, "myaxios.get(", "myaxios", "get"),
                row(19, "api.post(", "api", "post")
            ]
        );
        assert_eq!(member("1api.get('/c')"), [row(1, "api.get(", "api", "get")]);
        assert_eq!(
            member("this.http.get<User>('/d')"),
            [row(0, "this.http.get<User>(", "this.http", "get")]
        );
        assert_eq!(
            member("a.b.c.delete('/e')"),
            [row(0, "a.b.c.delete(", "a.b.c", "delete")]
        );
        assert_eq!(member("$x.$get('/f')"), [row(0, "$x.$get(", "$x", "$get")]);
        assert_eq!(
            member("x.y.request({ url: '/g' })"),
            [row(0, "x.y.request(", "x.y", "request")]
        );
        let bare = |s: &str| -> Vec<(usize, String, String)> {
            BARE_CLIENT_CALL
                .captures_iter(s)
                .map(|c| {
                    let whole = c.get(0).unwrap();
                    (whole.start(), whole.as_str().to_string(), c[1].to_string())
                })
                .collect()
        };
        let call =
            |at: usize, whole: &str, callee: &str| (at, whole.to_string(), callee.to_string());
        assert_eq!(
            bare(
                "fetch('/a'); window.fetch('/b'); this.fetch('/c'); $fetch('/d'); x$fetch('/e'); useSWR<T[]>('/f'); ofetch('/g'); myfetch('/h')"
            ),
            [
                call(0, "fetch(", "fetch"),
                call(13, "window.fetch(", "fetch"),
                call(38, "fetch(", "fetch"),
                call(52, "fetch(", "fetch"),
                call(66, "$fetch(", "$fetch"),
                call(80, "useSWR<T[]>(", "useSWR"),
                call(99, "ofetch(", "ofetch"),
            ]
        );
        assert_eq!(
            bare("globalThis . fetch ('/i')"),
            [call(0, "globalThis . fetch (", "fetch")]
        );
        assert_eq!(callee_of("useSWR<T[]>"), "useSWR");
        assert_eq!(callee_of("this . http . get < User >"), "this.http.get");
    }

    #[test]
    fn a_file_binds_clients_and_queues() {
        let factories: Vec<(usize, String, String)> = CLIENT_FACTORY
            .captures_iter(
                "const api = axios.create({ baseURL: '/api' }); export let k: Ky = ky.extend({ prefixUrl: `/v2` }); var w = wretch('/w'); const n = new Axios({ baseURL: base })",
            )
            .map(|c| {
                let whole = c.get(0).unwrap();
                (whole.start(), whole.as_str().to_string(), c[1].to_string())
            })
            .collect();
        let factory =
            |at: usize, whole: &str, name: &str| (at, whole.to_string(), name.to_string());
        assert_eq!(
            factories,
            [
                factory(0, "const api = axios.create(", "api"),
                factory(47, "export let k: Ky = ky.extend(", "k"),
                factory(99, "var w = wretch(", "w"),
                factory(121, "const n = new Axios(", "n"),
            ]
        );
        let bound = |pattern: &Regex,
                     s: &str,
                     name: usize,
                     literal: usize|
         -> Vec<(usize, String, String, String)> {
            pattern
                .captures_iter(s)
                .map(|c| {
                    let whole = c.get(0).unwrap();
                    (
                        whole.start(),
                        whole.as_str().to_string(),
                        c[name].to_string(),
                        literal_group(&c, literal).unwrap().to_string(),
                    )
                })
                .collect()
        };
        let row = |at: usize, whole: &str, name: &str, queue: &str| {
            (at, whole.to_string(), name.to_string(), queue.to_string())
        };
        assert_eq!(
            bound(
                &QUEUE_BINDING,
                "export const q = new Queue('email'); const r = Bull<Job>(\"r\"); let s = new BullQueue('s\"); const t: Queue = Queue(`t`)",
                1,
                2
            ),
            [
                row(0, "export const q = new Queue('email'", "q", "email"),
                row(37, "const r = Bull<Job>(\"r\"", "r", "r"),
                row(91, "const t: Queue = Queue(`t`", "t", "t"),
            ]
        );
        assert_eq!(
            bound(
                &INJECT_QUEUE,
                "@InjectQueue('cats') private readonly catsQueue: Queue, @InjectQueue(\"x') q, @InjectQueue(`y`) public z",
                4,
                1
            ),
            [
                row(
                    0,
                    "@InjectQueue('cats') private readonly catsQueue",
                    "catsQueue",
                    "cats"
                ),
                row(77, "@InjectQueue(`y`) public z", "z", "y"),
            ]
        );
        let bindings = read_bindings(
            "export const api = axios.create({ baseURL: '/api' });\nconst q = new Queue('email');\nexport default ky.create({ prefixUrl: `/v2` });\nconst api = got.extend({});\n",
        );
        // A later binding of a name replaces an earlier one.
        assert_eq!(bindings.clients.get("api"), Some(&None));
        assert_eq!(bindings.default_client, Some(Some("/v2".to_string())));
        assert_eq!(bindings.queues.get("q").map(String::as_str), Some("email"));
        let keys = read_bindings(
            "const a = axios.create({ baseURL: '/a' });\nconst b = axios.create({ baseUrl: '/b' });\nconst c = ky.create({ prefixUrl: '/c' });\nconst d = axios.create({ baseURI: '/d' });\n",
        );
        for (name, base) in [("a", "/a"), ("b", "/b"), ("c", "/c"), ("d", "/d")] {
            assert_eq!(
                keys.clients.get(name),
                Some(&Some(base.to_string())),
                "{name}"
            );
        }
    }

    // ---- Queues ----

    #[test]
    fn queue_calls_are_read_from_a_word_start() {
        let adds: Vec<(usize, String, String, String)> = QUEUE_ADD
            .captures_iter("xq.add('job'); q.add('welcome'); q.add('a\"); this.emailQueue.add(`b`)")
            .map(|c| {
                let whole = c.get(0).unwrap();
                (
                    whole.start(),
                    whole.as_str().to_string(),
                    c[1].to_string(),
                    literal_group(&c, 2).unwrap().to_string(),
                )
            })
            .collect();
        let add = |at: usize, whole: &str, receiver: &str, job: &str| {
            (at, whole.to_string(), receiver.to_string(), job.to_string())
        };
        assert_eq!(
            adds,
            [
                add(0, "xq.add('job'", "xq", "job"),
                add(15, "q.add('welcome'", "q", "welcome"),
                add(45, "this.emailQueue.add(`b`", "this.emailQueue", "b"),
            ]
        );
        let processes: Vec<(usize, String, String, Option<String>)> = QUEUE_PROCESS
            .captures_iter(
                "q.process('job', handle); q.process(3, h); q.process('a\", h); xq.process(h)",
            )
            .map(|c| {
                let whole = c.get(0).unwrap();
                (
                    whole.start(),
                    whole.as_str().to_string(),
                    c[1].to_string(),
                    literal_group(&c, 2).map(str::to_string),
                )
            })
            .collect();
        let process = |at: usize, whole: &str, receiver: &str, job: Option<&str>| {
            (
                at,
                whole.to_string(),
                receiver.to_string(),
                job.map(str::to_string),
            )
        };
        assert_eq!(
            processes,
            [
                process(0, "q.process('job', ", "q", Some("job")),
                process(26, "q.process(3, ", "q", None),
                process(43, "q.process(", "q", None),
                process(62, "xq.process(", "xq", None),
            ]
        );
        let workers: Vec<(usize, String, String)> = NEW_WORKER
            .captures_iter("new Worker('q', handle); new Worker<Job>(\"r\", h); new Worker('s\", h); renew Worker('t', h)")
            .map(|c| {
                let whole = c.get(0).unwrap();
                (whole.start(), whole.as_str().to_string(), literal_group(&c, 1).unwrap().to_string())
            })
            .collect();
        assert_eq!(
            workers,
            [
                (0, "new Worker('q', ".to_string(), "q".to_string()),
                (25, "new Worker<Job>(\"r\", ".to_string(), "r".to_string()),
            ]
        );
        assert_eq!(chain_of("this . emailQueue"), strings(&["emailQueue"]));
        assert_eq!(last_segment("this . emailQueue"), "emailQueue");
        for (last, shaped) in [
            ("emailQueue", true),
            ("jobs", true),
            ("myJob", true),
            ("JOBS", true),
            ("workerPool", true),
            ("BullMQ", true),
            ("taskFlow", true),
            ("producer1", true),
            ("mailer", false),
            ("job", true),
            ("jobHandler", false),
        ] {
            assert_eq!(queue_shaped(last), shaped, "{last}");
        }
    }

    #[test]
    fn a_handler_argument_is_a_name_or_an_inline_function() {
        let handler = |text: &str| {
            HANDLER_ARG.captures(js_trim_start(text)).map(|c| {
                (
                    c[0].to_string(),
                    c.get(1).map(|named| named.as_str().to_string()),
                )
            })
        };
        let named = |whole: &str, name: &str| Some((whole.to_string(), Some(name.to_string())));
        let inline = |whole: &str| Some((whole.to_string(), None));
        assert_eq!(handler("handle, x"), named("handle,", "handle"));
        assert_eq!(handler("async handle)"), named("async handle)", "handle"));
        assert_eq!(handler("this.handle.bind(this)"), None);
        assert_eq!(handler("async (job) => {"), inline("async ("));
        assert_eq!(handler("function (job) {"), inline("function"));
        assert_eq!(handler("job => job"), inline("job =>"));
        assert_eq!(
            handler("handlers.onWelcome"),
            named("handlers.onWelcome", "handlers.onWelcome")
        );
        assert_eq!(
            handler("  async\n  handle"),
            named("async\n  handle", "handle")
        );
        assert_eq!(handler("asyncHandle"), named("asyncHandle", "asyncHandle"));
        assert_eq!(handler("functional(x)"), None);
    }

    fn producer(function: &str, queue: Option<&str>, job: &str) -> QueueProducer {
        QueueProducer {
            function: node(
                function,
                NodeKind::Function,
                function,
                &format!("{function}.ts"),
                Language::TypeScript,
                (1, 1),
            ),
            line: 3,
            column: 2,
            callee: "q.add".to_string(),
            queue: queue.map(str::to_string),
            job: job.to_string(),
        }
    }

    fn consumer(id: &str, queue: Option<&str>, job: Option<&str>) -> QueueConsumer {
        QueueConsumer {
            node: node(
                id,
                NodeKind::Method,
                id,
                &format!("{id}.ts"),
                Language::TypeScript,
                (1, 1),
            ),
            file: "c.ts".to_string(),
            line: 7,
            queue: queue.map(str::to_string),
            job: job.map(str::to_string),
        }
    }

    fn ends(edges: &[Edge]) -> Vec<(String, String)> {
        edges
            .iter()
            .map(|edge| (edge.source.clone(), edge.target.clone()))
            .collect()
    }

    #[test]
    fn a_job_reaches_its_most_specific_consumers() {
        let pair = |producers: Vec<QueueProducer>, consumers: Vec<QueueConsumer>| {
            let mut edges = Vec::new();
            pair_queue(&producers, &consumers, &mut edges, &mut HashSet::new());
            edges
        };
        let edge = |source: &str, target: &str| (source.to_string(), target.to_string());
        let exact = pair(
            vec![producer("p", Some("email"), "welcome")],
            vec![
                consumer("a", Some("email"), Some("welcome")),
                consumer("b", None, Some("welcome")),
                consumer("c", Some("email"), None),
            ],
        );
        assert_eq!(ends(&exact), [edge("p", "a")]);
        assert_eq!(
            exact[0].metadata,
            Some(json!({
                "synthesizedBy": "queue-job",
                "channel": "queue",
                "callee": "q.add",
                "event": "welcome",
                "queue": "email",
                "registeredAt": "c.ts:7",
            }))
        );
        assert_eq!(
            (
                exact[0].line,
                exact[0].col,
                exact[0].kind,
                exact[0].provenance.as_deref()
            ),
            (Some(3), Some(2), EdgeKind::Calls, Some("heuristic"))
        );
        // The job by name; its queue is the consumer's.
        let by_job = pair(
            vec![producer("p", None, "welcome")],
            vec![
                consumer("b", Some("sms"), Some("welcome")),
                consumer("c", Some("email"), None),
            ],
        );
        assert_eq!(ends(&by_job), [edge("p", "b")]);
        assert_eq!(by_job[0].metadata.as_ref().unwrap()["queue"], "sms");
        // The queue's consumers of every job.
        let default = pair(
            vec![producer("p", Some("email"), "monthly")],
            vec![
                consumer("a", Some("email"), Some("welcome")),
                consumer("c", Some("email"), None),
                consumer("d", None, None),
            ],
        );
        assert_eq!(ends(&default), [edge("p", "c")]);
        // An unnamed queue and no consumer naming the job: a guess.
        let guess = pair(
            vec![producer("p", None, "monthly")],
            vec![consumer("c", Some("email"), None)],
        );
        assert!(guess.is_empty());
        // Nor any consumer of every job on an unnamed queue.
        assert!(
            pair(
                vec![producer("p", None, "monthly")],
                vec![consumer("d", None, None)]
            )
            .is_empty()
        );
        // A named queue's job never reaches another queue's consumer.
        assert!(
            pair(
                vec![producer("p", Some("email"), "welcome")],
                vec![consumer("b", Some("sms"), Some("welcome"))]
            )
            .is_empty()
        );
        // An unnamed queue's job reaches its job's consumers on every queue.
        assert_eq!(
            ends(&pair(
                vec![producer("p", None, "welcome")],
                vec![
                    consumer("a", None, Some("welcome")),
                    consumer("b", Some("sms"), Some("welcome")),
                ]
            )),
            [edge("p", "a"), edge("p", "b")]
        );
        let many = |count: usize| {
            pair(
                vec![producer("p", Some("q"), "j")],
                (0..count)
                    .map(|at| consumer(&format!("n{at}"), Some("q"), Some("j")))
                    .collect(),
            )
        };
        assert_eq!(many(EVENT_FANOUT_CAP).len(), EVENT_FANOUT_CAP);
        assert!(many(EVENT_FANOUT_CAP + 1).is_empty());
        let own = pair(
            vec![producer("p", Some("q"), "j")],
            vec![
                consumer("p", Some("q"), Some("j")),
                consumer("o", Some("q"), Some("j")),
            ],
        );
        assert_eq!(ends(&own), [edge("p", "o")]);
    }

    // ---- Events ----

    /// Upstream's `(?<![\w$])` dropped: a word character opens the chain.
    #[test]
    fn event_calls_are_read_from_a_word_start() {
        let emits: Vec<(usize, String, String, String, String)> = EMIT
            .captures_iter(
                "xbus.emit('e'); bus.emit('f'); this.server.to(room).emit(\"g\"); $bus.emit(`h`); a.emit('i\"); bus.emitAsync('j')",
            )
            .map(|c| {
                let whole = c.get(0).unwrap();
                (
                    whole.start(),
                    whole.as_str().to_string(),
                    c[1].to_string(),
                    c[2].to_string(),
                    literal_group(&c, 3).unwrap().to_string(),
                )
            })
            .collect();
        let emit = |at: usize, whole: &str, receiver: &str, verb: &str, event: &str| {
            (
                at,
                whole.to_string(),
                receiver.to_string(),
                verb.to_string(),
                event.to_string(),
            )
        };
        assert_eq!(
            emits,
            [
                emit(0, "xbus.emit('e'", "xbus", "emit", "e"),
                emit(16, "bus.emit('f'", "bus", "emit", "f"),
                emit(63, "$bus.emit(`h`", "$bus", "emit", "h"),
                emit(92, "bus.emitAsync('j'", "bus", "emitAsync", "j"),
            ]
        );
        let ons: Vec<(usize, String, String, String)> = SOCKET_ON
            .captures_iter(
                "xsocket.on('m', h); socket.on('n', (m) => {}); socket.once(\"o\", h); socket.on('p\", h)",
            )
            .map(|c| {
                let whole = c.get(0).unwrap();
                (
                    whole.start(),
                    whole.as_str().to_string(),
                    c[1].to_string(),
                    literal_group(&c, 2).unwrap().to_string(),
                )
            })
            .collect();
        let on = |at: usize, whole: &str, receiver: &str, event: &str| {
            (
                at,
                whole.to_string(),
                receiver.to_string(),
                event.to_string(),
            )
        };
        assert_eq!(
            ons,
            [
                on(0, "xsocket.on('m', ", "xsocket", "m"),
                on(20, "socket.on('n', ", "socket", "n"),
                on(47, "socket.once(\"o\", ", "socket", "o"),
            ]
        );
    }

    #[test]
    fn a_receiver_is_a_socket_a_bus_or_neither() {
        for (receiver, shape) in [
            ("this.server.to(room)", Some(Shape::Socket)),
            ("this.eventEmitter", Some(Shape::Bus)),
            ("this.foo", None),
            ("socket", Some(Shape::Socket)),
            ("bus.socketThing", Some(Shape::Bus)),
            ("this . io . of( \"/ns\" )", Some(Shape::Socket)),
            ("this", None),
            ("emitter.server", Some(Shape::Socket)),
        ] {
            assert_eq!(shape_of(receiver), shape, "{receiver}");
        }
        for word in [
            "socket",
            "io",
            "ws",
            "wss",
            "client",
            "server",
            "namespace",
            "nsp",
            "conn",
            "connection",
            "gateway",
            "broadcast",
            "to",
            "in",
            "of",
            "except",
            "volatile",
            "local",
            "sockets",
            "socketServer",
            "wsServer",
            "room",
            "channel",
            "pusher",
            "ably",
            "ioClient",
            "socketClient",
            "sock",
        ] {
            assert_eq!(
                shape_of(&format!("this.{word}")),
                Some(Shape::Socket),
                "{word}"
            );
        }
        for word in [
            "eventEmitter",
            "emitter",
            "events",
            "eventBus",
            "bus",
            "dispatcher",
            "pubsub",
            "publisher",
            "eventPublisher",
            "ee",
            "hub",
            "mediator",
            "broker",
            "messageBus",
            "appEvents",
            "domainEvents",
            "eventsService",
            "eventService",
        ] {
            assert_eq!(
                shape_of(&format!("this.{word}")),
                Some(Shape::Bus),
                "{word}"
            );
        }
        // A call's arguments are no segments of the receiver.
        assert_eq!(shape_of("bus.get(a.socket.b).x"), Some(Shape::Bus));
    }

    #[test]
    fn an_event_glob_takes_a_segment_or_anything() {
        let mut compiled = HashMap::new();
        for (pattern, event, matches) in [
            ("user.*", "user.created", true),
            ("user.*", "user.a.b", false),
            ("user.**", "user.a.b", true),
            ("**", "anything.at.all", true),
            ("a.*.c", "a.b.c", true),
            ("a.*.c", "a..c", false),
            ("user.created", "user.created", true),
            ("user.(x)", "user.(x)", true),
            ("*", "a\rb", true),
            ("**", "a\rb", false),
            ("a+*", "a+b", true),
            ("user.*", "user.", false),
        ] {
            assert_eq!(
                event_matches(pattern, event, &mut compiled),
                matches,
                "{pattern} {event:?}"
            );
        }
    }

    fn dispatch(function: &str, event: &str, shape: Shape, side: Side) -> Dispatch {
        Dispatch {
            function: node(
                function,
                NodeKind::Function,
                function,
                &format!("{function}.ts"),
                Language::TypeScript,
                (1, 1),
            ),
            line: 4,
            column: 1,
            callee: "x.emit".to_string(),
            event: event.to_string(),
            shape,
            side,
        }
    }

    fn handler(id: &str, pattern: &str, kind: Shape, side: Side) -> Handler {
        Handler {
            node: node(
                id,
                NodeKind::Method,
                id,
                &format!("{id}.ts"),
                Language::TypeScript,
                (1, 1),
            ),
            file: "h.ts".to_string(),
            line: 9,
            pattern: pattern.to_string(),
            kind,
            side,
        }
    }

    #[test]
    fn an_event_reaches_its_handlers_across_the_tier() {
        let pair = |dispatches: Vec<Dispatch>, handlers: Vec<Handler>| {
            let mut edges = Vec::new();
            pair_events(&dispatches, &handlers, &mut edges, &mut HashSet::new());
            edges
        };
        let edge = |source: &str, target: &str| (source.to_string(), target.to_string());
        let bus = pair(
            vec![dispatch("d", "user.created", Shape::Bus, Side::Client)],
            vec![
                handler("a", "user.created", Shape::Bus, Side::Server),
                handler("b", "user.*", Shape::Bus, Side::Client),
                handler("c", "order.paid", Shape::Bus, Side::Client),
                handler("s", "user.created", Shape::Socket, Side::Server),
            ],
        );
        assert_eq!(ends(&bus), [edge("d", "a"), edge("d", "b")]);
        assert_eq!(
            bus[0].metadata,
            Some(json!({
                "synthesizedBy": "event-bus",
                "channel": "event",
                "callee": "x.emit",
                "event": "user.created",
                "registeredAt": "h.ts:9",
            }))
        );
        let client = pair(
            vec![dispatch("d", "msg", Shape::Socket, Side::Client)],
            vec![
                handler("a", "msg", Shape::Socket, Side::Server),
                handler("b", "msg", Shape::Socket, Side::Client),
                handler("c", "msg", Shape::Bus, Side::Server),
            ],
        );
        assert_eq!(ends(&client), [edge("d", "a")]);
        assert_eq!(
            client[0].metadata,
            Some(json!({
                "synthesizedBy": "event-bus",
                "channel": "socket",
                "callee": "x.emit",
                "event": "msg",
                "tier": "client→server",
                "registeredAt": "h.ts:9",
            }))
        );
        let server = pair(
            vec![dispatch("d", "msg", Shape::Socket, Side::Server)],
            vec![
                handler("a", "msg", Shape::Socket, Side::Server),
                handler("b", "msg", Shape::Socket, Side::Client),
            ],
        );
        assert_eq!(ends(&server), [edge("d", "b")]);
        assert_eq!(
            server[0].metadata.as_ref().unwrap()["tier"],
            "server→client"
        );
        // More than the cap of dispatches or of handlers draws nothing; the
        // dispatches are counted by shape and event.
        let dispatched = |count: usize| {
            pair(
                (0..count)
                    .map(|at| dispatch(&format!("d{at}"), "e", Shape::Bus, Side::Client))
                    .collect(),
                vec![handler("h", "e", Shape::Bus, Side::Client)],
            )
        };
        assert_eq!(dispatched(EVENT_FANOUT_CAP).len(), EVENT_FANOUT_CAP);
        assert!(dispatched(EVENT_FANOUT_CAP + 1).is_empty());
        let handled = |count: usize| {
            pair(
                vec![dispatch("d", "e", Shape::Bus, Side::Client)],
                (0..count)
                    .map(|at| handler(&format!("h{at}"), "e", Shape::Bus, Side::Client))
                    .collect(),
            )
        };
        assert_eq!(handled(EVENT_FANOUT_CAP).len(), EVENT_FANOUT_CAP);
        assert!(handled(EVENT_FANOUT_CAP + 1).is_empty());
        let mut crowded: Vec<Dispatch> = (0..=EVENT_FANOUT_CAP)
            .map(|at| dispatch(&format!("b{at}"), "e", Shape::Bus, Side::Client))
            .collect();
        crowded.push(dispatch("s", "e", Shape::Socket, Side::Client));
        let socket = pair(
            crowded,
            vec![handler("h", "e", Shape::Socket, Side::Server)],
        );
        assert_eq!(ends(&socket), [edge("s", "h")]);
        let own = pair(
            vec![dispatch("d", "e", Shape::Bus, Side::Client)],
            vec![
                handler("d", "e", Shape::Bus, Side::Client),
                handler("o", "e", Shape::Bus, Side::Client),
            ],
        );
        assert_eq!(ends(&own), [edge("d", "o")]);
    }

    #[test]
    fn the_gates_read_upstreams_patterns() {
        for (text, open) in [
            ("fetch(", true),
            (" $fetch(", true),
            ("x.get(", true),
            ("x . post <T>(", true),
            ("get(", false),
            ("kyx", false),
            ("got", true),
            (".request(", true),
            (".$post(", true),
            (".$put(", false),
        ] {
            assert_eq!(HTTP_GATE.is_match(text), open, "http {text:?}");
        }
        for (text, open) in [
            ("q.add(", true),
            ("@Processor(", true),
            ("new Worker<J>(", true),
            ("new Worker (", true),
            ("x.process (", true),
            ("renew Worker(", false),
            (".adds(", false),
        ] {
            assert_eq!(QUEUE_GATE.is_match(text), open, "queue {text:?}");
        }
        for (text, open) in [
            ("x.emit(", true),
            ("x.once (", true),
            ("@OnEvent(", true),
            ("@SubscribeMessage (", true),
            ("x.off(", false),
            (".onX(", false),
        ] {
            assert_eq!(EVENT_GATE.is_match(text), open, "event {text:?}");
        }
        for (text, open) in [
            ("mockMvc", true),
            ("WebTestClient x", true),
            ("WebTestClients", false),
            ("restTemplate.", true),
            ("xRestAssured", false),
            ("RestAssured.post(", true),
            (".when()", true),
            ("$this ->", true),
            ("Pest\\Laravel", true),
            ("nothing", false),
        ] {
            assert_eq!(TEST_REQUEST_GATE.is_match(text), open, "test {text:?}");
        }
        for (text, server) in [
            ("@WebSocketGateway(", true),
            ("new Server(", true),
            ("new ServerX(", false),
            ("@SubscribeMessage('chat')", true),
            ("@WebSocketServer() server", true),
            ("io.on('connection', h)", true),
            ("new WebSocket.Server(", true),
            ("x", false),
        ] {
            assert_eq!(SOCKET_SERVER_FILE.is_match(text), server, "socket {text:?}");
        }
        let languages = |list: &[Language]| list.iter().copied().collect::<HashSet<_>>();
        assert!(test_request_gate(&languages(&[Language::Kotlin])));
        assert!(test_request_gate(&languages(&[
            Language::Php,
            Language::TypeScript
        ])));
        assert!(!test_request_gate(&languages(&[
            Language::TypeScript,
            Language::Python
        ])));
    }

    // ---- Test requests ----

    #[test]
    fn a_request_path_holes_its_computed_pieces() {
        for (s, lang, expected) in [
            (
                "'/books/' . $book->slug)",
                TestLang::Php,
                Some("/books/<HOLE>"),
            ),
            ("'/a' . 1)", TestLang::Php, Some("/a<HOLE>")),
            ("'/a'.1 . $b)", TestLang::Php, Some("/a")),
            (
                "\"api/songs/{$song->id}/rating\", [])",
                TestLang::Php,
                Some("api/songs/<HOLE>/rating"),
            ),
            (
                "\"api/songs/$song->public_id/rating\", [])",
                TestLang::Php,
                Some("api/songs/<HOLE>/rating"),
            ),
            (
                "\"/u/${user.id}/x\")",
                TestLang::Kotlin,
                Some("/u/<HOLE>/x"),
            ),
            (
                "\"/owners/{ownerId}/edit\", 1)",
                TestLang::Java,
                Some("/owners/<HOLE>/edit"),
            ),
            ("\"/a\" + id + \"/b\")", TestLang::Java, Some("/a<HOLE>/b")),
            ("\"/a\\\"b\")", TestLang::Java, Some("/a\"b")),
            ("\"/x/$id\")", TestLang::Java, Some("/x/$id")),
            ("route('about'))", TestLang::Php, None),
            ("$url)", TestLang::Php, None),
            ("$base . '/x')", TestLang::Php, None),
            (
                "'/a' . f($x, ')') . '/b')",
                TestLang::Php,
                Some("/a<HOLE>/b"),
            ),
            ("\"/a/{x}/{y/z}\")", TestLang::Java, Some("/a/<HOLE>/{y/z}")),
            ("\"/t/$a->b->c/d\"", TestLang::Php, Some("/t/<HOLE>/d")),
            ("\"/unterminated", TestLang::Java, None),
            ("'/a' . $b['k'] . '/c')", TestLang::Php, Some("/a<HOLE>/c")),
            ("\"/a\\", TestLang::Java, None),
            ("\"/u/{$x}\"", TestLang::Php, Some("/u/<HOLE>")),
            ("\"/e/${a}\"", TestLang::Php, Some("/e/<HOLE>")),
        ] {
            assert_eq!(
                read_request_path(s, 0, lang),
                expected.map(holed),
                "{s} {lang:?}"
            );
        }
        let path = |raw: &str| test_request_path(&holed(raw));
        let shown = |segs: &[&str], display: &str| Some((strings(segs), display.to_string()));
        assert_eq!(path("api/me"), shown(&["api", "me"], "/api/me"));
        assert_eq!(path("/owners?page=1"), shown(&["owners"], "/owners"));
        assert_eq!(
            path("https://localhost:8080/owners/new"),
            shown(&["owners", "new"], "/owners/new")
        );
        assert_eq!(
            path("/owners/<HOLE>/edit"),
            shown(&["owners", "*", "edit"], "/owners/${…}/edit")
        );
        assert_eq!(path("<HOLE>"), None);
        assert_eq!(path(""), shown(&[], "/"));
        assert_eq!(path("/<HOLE>/<HOLE>"), None);
        assert_eq!(path("//host"), shown(&[], "/"));
    }

    #[test]
    fn a_laravel_request_is_a_chain_of_calls_on_this() {
        for (s, at, rooted) in [
            ("$this->get('/x')", 5, true),
            (
                "$this->actingAs($u)->withHeaders(['a' => 'b'])->get('/x')",
                46,
                true,
            ),
            ("$this->app->get('config')", 10, false),
            ("$this->a()->b()->c()->d()->get('/x')", 25, true),
            ("$this->withToken($t)->put(", 20, true),
            ("$x->get(", 2, false),
            ("$this -> a ( ) -> get(", 15, true),
            ("$this->a($this->b())->get(", 20, true),
            ("$this->a(1)->b(2)->c(3)->d(4)->e(5)->get(", 35, true),
        ] {
            assert_eq!(this_rooted_calls(s, at), rooted, "{s} @{at}");
        }
        // Up to four `$this` roots back are tried.
        for (inner, rooted) in [(3, true), (4, false)] {
            let calls: Vec<String> = (0..inner).map(|at| format!("$this->x{at}()")).collect();
            let s = format!("$this->a({})->get('/x')", calls.join(", "));
            assert_eq!(
                this_rooted_calls(&s, s.rfind("->get").unwrap()),
                rooted,
                "{s}"
            );
        }
    }

    #[test]
    fn a_request_helper_makes_a_request_within_one_more_call() {
        let methods: HashMap<&str, &str> = [
            (
                "postAs",
                "protected function postAs($u) { return $this->jsonAs('post', $u); }",
            ),
            (
                "jsonAs",
                "private function jsonAs($m, $u) { return $this->withToken('t')->json($m, $u); }",
            ),
            ("loopA", "function loopA() { $this->loopB(); }"),
            ("loopB", "function loopB() { $this->loopA(); }"),
            ("outer", "function outer() { $this->middle(); }"),
            ("middle", "function middle() { $this->inner(); }"),
            ("inner", "function inner() { return $this->get('/x'); }"),
            ("direct", "function direct() { $this->post('/y'); }"),
        ]
        .into_iter()
        .collect();
        let run = |names: &[&str]| -> (Vec<bool>, Vec<(String, bool)>) {
            let mut bodies = |name: &str| -> Result<Vec<String>> {
                Ok(methods
                    .get(name)
                    .map(|body| vec![body.to_string()])
                    .unwrap_or_default())
            };
            let mut memo = HashMap::new();
            let results = names
                .iter()
                .map(|name| is_request_helper(name, &mut bodies, &mut memo, 0).unwrap())
                .collect();
            let mut memo: Vec<(String, bool)> = memo.into_iter().collect();
            memo.sort();
            (results, memo)
        };
        let memo = |entries: &[(&str, bool)]| -> Vec<(String, bool)> {
            entries
                .iter()
                .map(|(name, known)| (name.to_string(), *known))
                .collect()
        };
        assert_eq!(
            run(&["postAs"]),
            (vec![true], memo(&[("jsonAs", true), ("postAs", true)]))
        );
        assert_eq!(run(&["loopA"]), (vec![false], memo(&[("loopA", false)])));
        // A "no" found below the top level is not remembered.
        assert_eq!(
            run(&["outer", "middle"]),
            (
                vec![false, true],
                memo(&[("inner", true), ("middle", true), ("outer", false)])
            )
        );
        assert_eq!(
            run(&["middle", "outer"]),
            (
                vec![true, true],
                memo(&[("inner", true), ("middle", true), ("outer", true)])
            )
        );
        assert_eq!(run(&["direct"]), (vec![true], memo(&[("direct", true)])));
        assert_eq!(
            run(&["nothing"]),
            (vec![false], memo(&[("nothing", false)]))
        );
        assert_eq!(
            run(&["outer", "inner", "outer"]),
            (
                vec![false, true, false],
                memo(&[("inner", true), ("outer", false)])
            )
        );
    }

    /// Upstream's one-character lookbehinds, checked by hand: a start after
    /// an excluded character is retried one character later.
    #[test]
    fn test_requests_are_read_after_their_excluded_characters() {
        let spring = |byte: u8| is_name_byte(byte) || byte == b'.';
        let rows = |pattern: &Regex,
                    s: &str,
                    excluded: &dyn Fn(u8) -> bool|
         -> Vec<(usize, String, String)> {
            matches_after(pattern, s, excluded)
                .into_iter()
                .map(|c| {
                    let whole = c.get(0).unwrap();
                    (whole.start(), whole.as_str().to_string(), c[1].to_string())
                })
                .collect()
        };
        let row = |at: usize, whole: &str, verb: &str| (at, whole.to_string(), verb.to_string());
        assert_eq!(
            rows(
                &MOCKMVC_BUILDER,
                "get(\"/a\"); x.get(\"/b\"); xget(\"/c\"); $get(\"/d\"); MockMvcRequestBuilders.post(\"/e\"); a.MockMvcRequestBuilders.put(\"/f\"); multipart(\"/g\")",
                &spring
            ),
            [
                row(0, "get(", "get"),
                row(48, "MockMvcRequestBuilders.post(", "post"),
                row(119, "multipart(", "multipart"),
            ]
        );
        assert_eq!(
            rows(
                &MOCKMVC_REQUEST,
                "request(HttpMethod.GET, \"/a\"); x.request(HttpMethod.PUT, \"/b\"); MockMvcRequestBuilders.request(HttpMethod.POST, \"/c\")",
                &spring
            ),
            [
                row(0, "request(HttpMethod.GET,", "GET"),
                row(
                    64,
                    "MockMvcRequestBuilders.request(HttpMethod.POST,",
                    "POST"
                ),
            ]
        );
        let pest = |byte: u8| is_word_byte(byte) || matches!(byte, b'$' | b'>' | b':' | b'\\');
        let pests: Vec<(usize, String, String, Option<String>)> = matches_after(
            &PEST_REQUEST,
            "get('/a'); $this->get('/b'); Http::get('/c'); \\get('/d'); xget('/e'); postJson('/f'); $get('/g')",
            pest,
        )
        .into_iter()
        .map(|c| {
            let whole = c.get(0).unwrap();
            (
                whole.start(),
                whole.as_str().to_string(),
                c[1].to_string(),
                c.get(2).map(|json| json.as_str().to_string()),
            )
        })
        .collect();
        assert_eq!(
            pests,
            [
                (0, "get(".to_string(), "get".to_string(), None),
                (
                    70,
                    "postJson(".to_string(),
                    "post".to_string(),
                    Some("Json".to_string())
                ),
            ]
        );
        // A non-ASCII character before a start is no excluded character.
        assert_eq!(
            rows(&MOCKMVC_BUILDER, "é get(\"/a\") éget(\"/b\")", &spring),
            [row(3, "get(", "get"), row(15, "get(", "get")]
        );
    }

    #[test]
    fn test_request_calls_read_upstreams_patterns() {
        let rows = |pattern: &Regex, s: &str, group: usize| -> Vec<(usize, String, String)> {
            pattern
                .captures_iter(s)
                .map(|c| {
                    let whole = c.get(0).unwrap();
                    (
                        whole.start(),
                        whole.as_str().to_string(),
                        c[group].to_string(),
                    )
                })
                .collect()
        };
        let row = |at: usize, whole: &str, group: &str| (at, whole.to_string(), group.to_string());
        let json_calls: Vec<(usize, String, String)> = LARAVEL_JSON_CALL
            .captures_iter(
                "$this->json('POST', 'api/me', []); $this->call(\"GET\", '/x'); $this->json('PUT\", '/y'); $this->json($method, $uri)",
            )
            .map(|c| {
                let whole = c.get(0).unwrap();
                let verb = c.get(1).or_else(|| c.get(2)).unwrap();
                (whole.start(), whole.as_str().to_string(), verb.as_str().to_string())
            })
            .collect();
        assert_eq!(
            json_calls,
            [
                row(5, "->json('POST',", "POST"),
                row(40, "->call(\"GET\",", "GET")
            ]
        );
        let helpers = "$this->getAs('/a'); $this->postJson('/b'); $this->getFixture('c'); $this->deleteX('/d')";
        assert_eq!(
            rows(&LARAVEL_HELPER, helpers, 1),
            [
                row(5, "->getAs(", "getAs"),
                row(25, "->postJson(", "postJson"),
                row(48, "->getFixture(", "getFixture"),
                row(72, "->deleteX(", "deleteX"),
            ]
        );
        assert_eq!(
            rows(&LARAVEL_HELPER, helpers, 2)
                .into_iter()
                .map(|(_, _, verb)| verb)
                .collect::<Vec<_>>(),
            strings(&["get", "post", "get", "delete"])
        );
        assert_eq!(
            rows(
                &KOTLIN_MOCKMVC,
                "mockMvc.post(\"/a\"); this.mockMvc . get(\"/b\"); myMockMvc.put(\"/c\"); xmockMvc.delete(\"/d\")",
                1
            ),
            [
                row(0, "mockMvc.post(", "post"),
                row(25, "mockMvc . get(", "get"),
                row(46, "myMockMvc.put(", "put"),
                row(67, "xmockMvc.delete(", "delete"),
            ]
        );
        assert_eq!(
            rows(
                &REST_TEMPLATE,
                "restTemplate.getForEntity(\"/a\", X.class); testRestTemplate.exchange(\"/b\", HttpMethod.POST, e, X.class); RestTemplate.put(\"/c\")",
                1
            ),
            [
                row(0, "restTemplate.getForEntity(", "getForEntity"),
                row(42, "testRestTemplate.exchange(", "exchange"),
                row(104, "RestTemplate.put(", "put"),
            ]
        );
        assert_eq!(
            rows(
                &REST_ASSURED,
                "given().when().get(\"/a\"); RestAssured.post(\"/b\"); x.when( ) . put(\"/c\")",
                1
            ),
            [
                row(7, ".when().get(", "get"),
                row(26, "RestAssured.post(", "post"),
                row(51, ".when( ) . put(", "put"),
            ]
        );
        assert_eq!(
            rows(
                &WEB_TEST_CLIENT,
                "webTestClient.get().uri(\"/a\"); client.post( ) . uri(\"/b\")",
                1
            ),
            [
                row(13, ".get().uri(", "get"),
                row(37, ".post( ) . uri(", "post")
            ]
        );
    }

    // ---- The passes over an index ----

    /// The column counts the source's UTF-16 code units from the line's
    /// start, as upstream's blanking leaves them: the emoji in the comment
    /// before the call is two.
    #[test]
    fn a_client_calls_column_counts_the_sources_code_units() {
        let fixture = Fixture::new("js-column")
            .file(
                "src/client.ts",
                Language::TypeScript,
                "// \u{1F600} a comment\nexport function load() { /* \u{1F600} */ return fetch('/api/users'); }\n",
            )
            .node(node(
                "fn:load",
                NodeKind::Function,
                "load",
                "src/client.ts",
                Language::TypeScript,
                (2, 2),
            ))
            .node(node(
                "route:users",
                NodeKind::Route,
                "GET /api/users",
                "src/server.ts",
                Language::TypeScript,
                (4, 4),
            ));
        let edges = fixture.edges("http-client");
        assert_eq!(
            ends(&edges),
            [("fn:load".to_string(), "route:users".to_string())]
        );
        assert_eq!((edges[0].line, edges[0].col), (Some(2), Some(41)));
        assert_eq!(
            edges[0].metadata,
            Some(json!({
                "synthesizedBy": "http-client",
                "channel": "http",
                "callee": "fetch",
                "tier": "client→server",
                "method": "GET",
                "href": "/api/users",
                "registeredAt": "src/server.ts:4",
            }))
        );
    }

    #[test]
    fn a_test_requests_column_counts_the_sources_code_units() {
        let fixture = Fixture::new("test-column")
            .file(
                "src/test/java/OwnerControllerTests.java",
                Language::Java,
                "import static org.springframework.test.web.servlet.request.MockMvcRequestBuilders.get;\nclass OwnerControllerTests {\n    void t() throws Exception { /* \u{1F600} */ mockMvc.perform(get(\"/owners\")); }\n}\n",
            )
            .file(
                "src/test/kotlin/OwnerTests.kt",
                Language::Kotlin,
                "class OwnerTests {\n    fun t() { /* \u{1F600} */ mockMvc.post(\"/owners/new\") { } }\n}\n",
            )
            .file(
                "tests/Feature/AboutTest.php",
                Language::Php,
                "<?php\nclass AboutTest extends TestCase {\n    public function testAbout() { /* \u{1F600} */ $this->get('/about'); }\n}\n",
            )
            .node(node(
                "java:t",
                NodeKind::Method,
                "t",
                "src/test/java/OwnerControllerTests.java",
                Language::Java,
                (3, 3),
            ))
            .node(node(
                "kotlin:t",
                NodeKind::Method,
                "t",
                "src/test/kotlin/OwnerTests.kt",
                Language::Kotlin,
                (2, 2),
            ))
            .node(node(
                "php:testAbout",
                NodeKind::Method,
                "testAbout",
                "tests/Feature/AboutTest.php",
                Language::Php,
                (3, 3),
            ))
            .node(node(
                "route:owners",
                NodeKind::Route,
                "GET /owners",
                "src/main/java/OwnerController.java",
                Language::Java,
                (9, 9),
            ))
            .node(node(
                "route:new",
                NodeKind::Route,
                "POST /owners/new",
                "src/main/java/OwnerController.java",
                Language::Java,
                (5, 5),
            ))
            .node(node(
                "route:about",
                NodeKind::Route,
                "GET /about",
                "routes/web.php",
                Language::Php,
                (3, 3),
            ));
        let mut rows: Vec<_> = fixture
            .edges("test-request")
            .into_iter()
            .map(|edge| {
                let callee = edge.metadata.as_ref().unwrap()["callee"]
                    .as_str()
                    .unwrap()
                    .to_string();
                (edge.source, edge.target, edge.line, edge.col, callee)
            })
            .collect();
        rows.sort();
        let row = |source: &str, target: &str, line: i64, col: i64, callee: &str| {
            (
                source.to_string(),
                target.to_string(),
                Some(line),
                Some(col),
                callee.to_string(),
            )
        };
        assert_eq!(
            rows,
            [
                row("java:t", "route:owners", 3, 57, "get"),
                row("kotlin:t", "route:new", 2, 23, "mockMvc.post"),
                row("php:testAbout", "route:about", 3, 48, "$this->get"),
            ]
        );
    }

    /// An import resolves among the indexed files, in the resolver's
    /// candidate order: a file on disk that the index lacks is skipped for
    /// the next candidate.
    #[test]
    fn an_imported_client_resolves_among_the_indexed_files() {
        let client = "import { api } from './api';\nexport function load(id: string) {\n  return api.get(`/users/${id}`);\n}\n";
        let legacy = "export const api = axios.create({ baseURL: '/legacy' });\n";
        let fixture = |name: &str, indexed_legacy: bool| {
            let fixture = Fixture::new(name)
                .file(
                    "src/api/index.ts",
                    Language::TypeScript,
                    "export const api = axios.create({ baseURL: '/api' });\n",
                )
                .file("src/client.ts", Language::TypeScript, client)
                .node(node(
                    "fn:load",
                    NodeKind::Function,
                    "load",
                    "src/client.ts",
                    Language::TypeScript,
                    (2, 4),
                ))
                .node(node(
                    "route:api",
                    NodeKind::Route,
                    "GET /api/users/:id",
                    "src/server.ts",
                    Language::TypeScript,
                    (3, 3),
                ))
                .node(node(
                    "route:legacy",
                    NodeKind::Route,
                    "GET /legacy/users/:id",
                    "src/server.ts",
                    Language::TypeScript,
                    (4, 4),
                ));
            if indexed_legacy {
                fixture.file("src/api.ts", Language::TypeScript, legacy)
            } else {
                fixture.disk_only("src/api.ts", legacy)
            }
        };
        let reached = |fixture: Fixture| -> Vec<(String, String, String)> {
            fixture
                .edges("http-client")
                .into_iter()
                .map(|edge| {
                    let href = edge.metadata.as_ref().unwrap()["href"]
                        .as_str()
                        .unwrap()
                        .to_string();
                    (edge.source, edge.target, href)
                })
                .collect()
        };
        let edge = |target: &str, href: &str| {
            ("fn:load".to_string(), target.to_string(), href.to_string())
        };
        assert_eq!(
            reached(fixture("import-unindexed", false)),
            [edge("route:api", "/api/users/${…}")]
        );
        assert_eq!(
            reached(fixture("import-indexed", true)),
            [edge("route:legacy", "/legacy/users/${…}")]
        );
    }

    // ---- Red-green additions (W7-20 run 1, batch a) ----

    /// The edges of `synthesized_by` as `source → target` pairs, sorted.
    fn sorted_ends(fixture: &Fixture, synthesized_by: &str) -> Vec<(String, String)> {
        let mut pairs = ends(&fixture.edges(synthesized_by));
        pairs.sort();
        pairs
    }

    fn pair(source: &str, target: &str) -> (String, String) {
        (source.to_string(), target.to_string())
    }

    /// Every JavaScript and TypeScript extension holds client calls; a Vue
    /// component does not (upstream `JS_FILE`).
    #[test]
    fn a_client_call_is_read_in_each_javascript_file() {
        let files = [
            ("a.js", Language::JavaScript),
            ("b.jsx", Language::Jsx),
            ("c.mjs", Language::JavaScript),
            ("d.cjs", Language::JavaScript),
            ("e.ts", Language::TypeScript),
            ("f.tsx", Language::Tsx),
            ("g.mts", Language::TypeScript),
            ("h.cts", Language::TypeScript),
            ("i.vue", Language::Vue),
        ];
        let mut fixture = Fixture::new("js-files");
        for (at, (file, language)) in files.iter().enumerate() {
            let path = format!("src/{file}");
            let line = at as i64 + 1;
            fixture = fixture
                .file(
                    &path,
                    *language,
                    &format!("export function load{at}() {{ return fetch('/x/{at}'); }}\n"),
                )
                .node(node(
                    &format!("fn:{at}"),
                    NodeKind::Function,
                    &format!("load{at}"),
                    &path,
                    *language,
                    (1, 1),
                ))
                .node(node(
                    &format!("route:{at}"),
                    NodeKind::Route,
                    &format!("GET /x/{at}"),
                    "src/server.ts",
                    Language::TypeScript,
                    (line, line),
                ));
        }
        let expected: Vec<(String, String)> = (0..8)
            .map(|at| pair(&format!("fn:{at}"), &format!("route:{at}")))
            .collect();
        assert_eq!(sorted_ends(&fixture, "http-client"), expected);
    }

    /// A `.kt` or `.kts` test reads Kotlin's string templates: `$id` is a
    /// hole there, and literal text in Java.
    #[test]
    fn a_kotlin_test_reads_its_string_templates() {
        let fixture = Fixture::new("kotlin-templates")
            .file(
                "src/test/kotlin/OwnerTests.kt",
                Language::Kotlin,
                "class OwnerTests {\n    fun t() { mockMvc.get(\"/owners/$id\") { } }\n}\n",
            )
            .file(
                "src/test/kotlin/smoke.kts",
                Language::Kotlin,
                "fun s() { mockMvc.get(\"/owners/$id/pets\") { } }\n",
            )
            .node(node(
                "kt:t",
                NodeKind::Method,
                "t",
                "src/test/kotlin/OwnerTests.kt",
                Language::Kotlin,
                (2, 2),
            ))
            .node(node(
                "kts:s",
                NodeKind::Function,
                "s",
                "src/test/kotlin/smoke.kts",
                Language::Kotlin,
                (1, 1),
            ))
            .node(node(
                "route:owner",
                NodeKind::Route,
                "GET /owners/{ownerId}",
                "src/main/kotlin/OwnerController.kt",
                Language::Kotlin,
                (5, 5),
            ))
            .node(node(
                "route:pets",
                NodeKind::Route,
                "GET /owners/{ownerId}/pets",
                "src/main/kotlin/OwnerController.kt",
                Language::Kotlin,
                (9, 9),
            ));
        let mut rows: Vec<(String, String, String)> = fixture
            .edges("test-request")
            .into_iter()
            .map(|edge| {
                let href = edge.metadata.as_ref().unwrap()["href"]
                    .as_str()
                    .unwrap()
                    .to_string();
                (edge.source, edge.target, href)
            })
            .collect();
        rows.sort();
        let row = |source: &str, target: &str, href: &str| {
            (source.to_string(), target.to_string(), href.to_string())
        };
        assert_eq!(
            rows,
            [
                row("kt:t", "route:owner", "/owners/${…}"),
                row("kts:s", "route:pets", "/owners/${…}/pets"),
            ]
        );
    }

    /// A file is read for a channel only when its text passes upstream's
    /// gate: `$put`, `$patch` and `$delete` alone open no HTTP gate, and a
    /// `.get().uri(…)` without `WebTestClient` (or another test word) no
    /// test gate.
    #[test]
    fn a_file_outside_its_channels_gate_is_not_read() {
        let fixture = Fixture::new("gates")
            .file(
                "src/put.ts",
                Language::TypeScript,
                "export function save() { return api.$put('/users'); }\n",
            )
            .file(
                "src/post.ts",
                Language::TypeScript,
                "export function add() { return api.$post('/users'); }\n",
            )
            .file(
                "src/test/java/AnyTests.java",
                Language::Java,
                "class AnyTests {\n    void t() { client.get().uri(\"/users\"); }\n}\n",
            )
            .file(
                "src/test/java/WebTests.java",
                Language::Java,
                "class WebTests {\n    WebTestClient client;\n    void w() { client.get().uri(\"/users\"); }\n}\n",
            )
            .node(node(
                "fn:save",
                NodeKind::Function,
                "save",
                "src/put.ts",
                Language::TypeScript,
                (1, 1),
            ))
            .node(node(
                "fn:add",
                NodeKind::Function,
                "add",
                "src/post.ts",
                Language::TypeScript,
                (1, 1),
            ))
            .node(node(
                "java:t",
                NodeKind::Method,
                "t",
                "src/test/java/AnyTests.java",
                Language::Java,
                (2, 2),
            ))
            .node(node(
                "java:w",
                NodeKind::Method,
                "w",
                "src/test/java/WebTests.java",
                Language::Java,
                (3, 3),
            ))
            .node(node(
                "route:put",
                NodeKind::Route,
                "PUT /users",
                "src/server.ts",
                Language::TypeScript,
                (1, 1),
            ))
            .node(node(
                "route:post",
                NodeKind::Route,
                "POST /users",
                "src/server.ts",
                Language::TypeScript,
                (2, 2),
            ))
            .node(node(
                "route:get",
                NodeKind::Route,
                "GET /users",
                "src/server.ts",
                Language::TypeScript,
                (3, 3),
            ));
        assert_eq!(
            sorted_ends(&fixture, "http-client"),
            [pair("fn:add", "route:post")]
        );
        assert_eq!(
            sorted_ends(&fixture, "test-request"),
            [pair("java:w", "route:get")]
        );
    }

    /// A route a framework resolver read on a line is a registration, even
    /// on a receiver named like a client (Fastify's `server`).
    #[test]
    fn a_route_registration_is_no_client_call() {
        let fixture = Fixture::new("registration")
            .file(
                "src/routes.ts",
                Language::TypeScript,
                "export function register(server) {\n  server.get('/users', list);\n}\nexport function load() {\n  return server.get('/users');\n}\n",
            )
            .node(node(
                "fn:register",
                NodeKind::Function,
                "register",
                "src/routes.ts",
                Language::TypeScript,
                (1, 3),
            ))
            .node(node(
                "fn:load",
                NodeKind::Function,
                "load",
                "src/routes.ts",
                Language::TypeScript,
                (4, 6),
            ))
            .node(node(
                "route:users",
                NodeKind::Route,
                "GET /users",
                "src/routes.ts",
                Language::TypeScript,
                (2, 2),
            ));
        assert_eq!(
            sorted_ends(&fixture, "http-client"),
            [pair("fn:load", "route:users")]
        );
    }

    /// A client call, a job and an event outside every function draw
    /// nothing; the same three inside one do.
    #[test]
    fn a_call_outside_every_function_draws_nothing() {
        let fixture = Fixture::new("top-level")
            .file(
                "src/top.ts",
                Language::TypeScript,
                "fetch('/users');\nemailQueue.add('welcome');\neventBus.emit('user.created');\nexport function run() {\n  fetch('/users');\n  emailQueue.add('welcome');\n  eventBus.emit('user.created');\n}\n",
            )
            .file(
                "src/consumers.ts",
                Language::TypeScript,
                "emailQueue.process('welcome', handleWelcome);\nexport function handleWelcome(job) {\n  return job;\n}\nexport class Listener {\n  @OnEvent('user.created')\n  onCreated(user) {\n    return user;\n  }\n}\n",
            )
            .node(node(
                "file:top",
                NodeKind::File,
                "top.ts",
                "src/top.ts",
                Language::TypeScript,
                (1, 8),
            ))
            .node(node(
                "fn:run",
                NodeKind::Function,
                "run",
                "src/top.ts",
                Language::TypeScript,
                (4, 8),
            ))
            .node(node(
                "file:consumers",
                NodeKind::File,
                "consumers.ts",
                "src/consumers.ts",
                Language::TypeScript,
                (1, 10),
            ))
            .node(node(
                "fn:handleWelcome",
                NodeKind::Function,
                "handleWelcome",
                "src/consumers.ts",
                Language::TypeScript,
                (2, 4),
            ))
            .node(node(
                "method:onCreated",
                NodeKind::Method,
                "onCreated",
                "src/consumers.ts",
                Language::TypeScript,
                (7, 9),
            ))
            .node(node(
                "route:users",
                NodeKind::Route,
                "GET /users",
                "src/server.ts",
                Language::TypeScript,
                (1, 1),
            ));
        assert_eq!(
            sorted_ends(&fixture, "http-client"),
            [pair("fn:run", "route:users")]
        );
        assert_eq!(
            sorted_ends(&fixture, "queue-job"),
            [pair("fn:run", "fn:handleWelcome")]
        );
        assert_eq!(
            sorted_ends(&fixture, "event-bus"),
            [pair("fn:run", "method:onCreated")]
        );
    }

    /// `clientFor`'s tiers: a client the file binds, one an import's file
    /// binds as its default export, a receiver whose last segment names a
    /// client, and not one whose head names a router.
    #[test]
    fn a_receiver_is_a_client_by_its_binding_or_its_name() {
        let fixture = Fixture::new("client-for")
            .file(
                "src/billing.ts",
                Language::TypeScript,
                "export default axios.create({ baseURL: '/billing' });\n",
            )
            .file(
                "src/client.ts",
                Language::TypeScript,
                "import billing from './billing';\nconst shop = axios.create({ baseURL: '/shop' });\nexport function local() { return shop.get('/items'); }\nexport function imported() { return billing.get('/invoices'); }\nexport function named() { return services.api.get('/named'); }\nexport function routed() { return app.http.get('/routed'); }\n",
            );
        let mut fixture = fixture;
        for (at, name) in ["local", "imported", "named", "routed"].iter().enumerate() {
            let line = at as i64 + 3;
            fixture = fixture.node(node(
                &format!("fn:{name}"),
                NodeKind::Function,
                name,
                "src/client.ts",
                Language::TypeScript,
                (line, line),
            ));
        }
        for (at, path) in ["/shop/items", "/billing/invoices", "/named", "/routed"]
            .iter()
            .enumerate()
        {
            let line = at as i64 + 1;
            fixture = fixture.node(node(
                &format!("route:{at}"),
                NodeKind::Route,
                &format!("GET {path}"),
                "src/server.ts",
                Language::TypeScript,
                (line, line),
            ));
        }
        assert_eq!(
            sorted_ends(&fixture, "http-client"),
            [
                pair("fn:imported", "route:1"),
                pair("fn:local", "route:0"),
                pair("fn:named", "route:2"),
            ]
        );
    }

    /// Upstream's two word lists: each client name makes a receiver a client
    /// without a binding, and a router name as its head makes it none.
    #[test]
    fn a_receiver_named_like_a_client_is_one_unless_its_head_is_a_router() {
        let clients = [
            "axios",
            "ky",
            "got",
            "superagent",
            "http",
            "https",
            "httpClient",
            "httpService",
            "api",
            "apiClient",
            "client",
            "restClient",
            "request",
            "agent",
            "fetcher",
            "instance",
            "$api",
            "$http",
            "$axios",
            "axiosInstance",
            "Axios",
            "HttpClient",
            "backend",
            "server",
        ];
        let routers = [
            "app",
            "router",
            "route",
            "routes",
            "express",
            "fastify",
            "koa",
            "hono",
            "elysia",
            "apiRouter",
            "v1",
            "v2",
            "r",
        ];
        let mut text = String::new();
        let mut fixture = Fixture::new("names");
        let mut line = 0;
        for (at, name) in clients.iter().enumerate() {
            line += 1;
            text.push_str(&format!(
                "export function c{at}() {{ return {name}.get('/c/{at}'); }}\n"
            ));
            fixture = fixture
                .node(node(
                    &format!("fn:c{at}"),
                    NodeKind::Function,
                    &format!("c{at}"),
                    "src/client.ts",
                    Language::TypeScript,
                    (line, line),
                ))
                .node(node(
                    &format!("route:c{at}"),
                    NodeKind::Route,
                    &format!("GET /c/{at}"),
                    "src/server.ts",
                    Language::TypeScript,
                    (line, line),
                ));
        }
        for (at, name) in routers.iter().enumerate() {
            line += 1;
            text.push_str(&format!(
                "export function r{at}() {{ return {name}.api.get('/r/{at}'); }}\n"
            ));
            fixture = fixture
                .node(node(
                    &format!("fn:r{at}"),
                    NodeKind::Function,
                    &format!("r{at}"),
                    "src/client.ts",
                    Language::TypeScript,
                    (line, line),
                ))
                .node(node(
                    &format!("route:r{at}"),
                    NodeKind::Route,
                    &format!("GET /r/{at}"),
                    "src/server.ts",
                    Language::TypeScript,
                    (line, line),
                ));
        }
        let fixture = fixture.file("src/client.ts", Language::TypeScript, &text);
        let mut expected: Vec<(String, String)> = (0..clients.len())
            .map(|at| pair(&format!("fn:c{at}"), &format!("route:c{at}")))
            .collect();
        expected.sort();
        assert_eq!(sorted_ends(&fixture, "http-client"), expected);
    }

    /// A member call's verb is its method, `.request(…)` takes the options'
    /// `method`, as a bare call's options object does.
    #[test]
    fn a_calls_method_is_its_verb_or_its_options_method() {
        let fixture = Fixture::new("methods")
            .file(
                "src/client.ts",
                Language::TypeScript,
                "export function add() { return api.post('/users', body); }\nexport function create() { return api.request({ url: '/users', method: 'post' }); }\nexport function make() { return axios({ url: '/users', method: 'post' }); }\nexport function top() { return self.window.fetch('/users', { method: 'POST' }); }\n",
            );
        let mut fixture = fixture;
        for (at, name) in ["add", "create", "make", "top"].iter().enumerate() {
            let line = at as i64 + 1;
            fixture = fixture.node(node(
                &format!("fn:{name}"),
                NodeKind::Function,
                name,
                "src/client.ts",
                Language::TypeScript,
                (line, line),
            ));
        }
        let fixture = fixture
            .node(node(
                "route:get",
                NodeKind::Route,
                "GET /users",
                "src/server.ts",
                Language::TypeScript,
                (1, 1),
            ))
            .node(node(
                "route:post",
                NodeKind::Route,
                "POST /users",
                "src/server.ts",
                Language::TypeScript,
                (2, 2),
            ));
        assert_eq!(
            sorted_ends(&fixture, "http-client"),
            [
                pair("fn:add", "route:post"),
                pair("fn:create", "route:post"),
                pair("fn:make", "route:post"),
                pair("fn:top", "route:post"),
            ]
        );
    }

    // ---- Red-green additions (W7-20 run 1, batch b) ----

    /// A node as `(id, kind, name, file, language, lines)`.
    type NodeRow<'a> = (&'a str, NodeKind, &'a str, &'a str, Language, (i64, i64));

    /// A fixture's nodes from rows.
    fn with_nodes(mut fixture: Fixture, rows: &[NodeRow<'_>]) -> Fixture {
        for (id, kind, name, file, language, lines) in rows {
            fixture = fixture.node(node(id, *kind, name, file, *language, *lines));
        }
        fixture
    }

    /// A producer's queue is its receiver's binding (`new Queue('…')`,
    /// `@InjectQueue('…')`), else the receiver must look like a queue, and
    /// so must a Bull consumer's.
    #[test]
    fn a_queue_is_named_by_its_binding_or_its_receivers_shape() {
        const TS: Language = Language::TypeScript;
        let fixture = Fixture::new("queue-names")
            .file(
                "src/mail.service.ts",
                TS,
                "export class MailService {\n  constructor(@InjectQueue('email') private readonly mail: Queue) {}\n  send() {\n    this.mail.add('welcome');\n    this.tags.add('welcome');\n  }\n  notify() {\n    this.jobs.add('welcome');\n  }\n}\n",
            )
            .file(
                "src/consumers.ts",
                TS,
                "const email = new Queue('email');\nconst sms = new Queue('sms');\nemail.process('welcome', onEmail);\nsms.process('welcome', onSms);\nimage.process('welcome', onImage);\nexport function onEmail(job) { return job; }\nexport function onSms(job) { return job; }\nexport function onImage(job) { return job; }\n",
            );
        let fixture = with_nodes(
            fixture,
            &[
                (
                    "class:MailService",
                    NodeKind::Class,
                    "MailService",
                    "src/mail.service.ts",
                    TS,
                    (1, 10),
                ),
                (
                    "method:send",
                    NodeKind::Method,
                    "send",
                    "src/mail.service.ts",
                    TS,
                    (3, 6),
                ),
                (
                    "method:notify",
                    NodeKind::Method,
                    "notify",
                    "src/mail.service.ts",
                    TS,
                    (7, 9),
                ),
                (
                    "fn:onEmail",
                    NodeKind::Function,
                    "onEmail",
                    "src/consumers.ts",
                    TS,
                    (6, 6),
                ),
                (
                    "fn:onSms",
                    NodeKind::Function,
                    "onSms",
                    "src/consumers.ts",
                    TS,
                    (7, 7),
                ),
                (
                    "fn:onImage",
                    NodeKind::Function,
                    "onImage",
                    "src/consumers.ts",
                    TS,
                    (8, 8),
                ),
            ],
        );
        assert_eq!(
            sorted_ends(&fixture, "queue-job"),
            [
                pair("method:notify", "fn:onEmail"),
                pair("method:notify", "fn:onSms"),
                pair("method:send", "fn:onEmail"),
            ]
        );
    }

    /// A `@Processor` class consumes its queue: each `@Process` method its
    /// job (the method named past the decorator, inside the class, the class
    /// starting at most two lines before its `class` keyword), else its
    /// `process` method every job.
    #[test]
    fn a_processor_consumes_its_queues_jobs() {
        const TS: Language = Language::TypeScript;
        let fixture = Fixture::new("processors")
            .file(
                "src/alpha.processor.ts",
                TS,
                "@Processor('alpha')\nexport class AlphaProcessor {\n  @Process('welcome')\n  onWelcome(job) {\n    return job;\n  }\n  @Process('report')\n  onReport(job) {\n    return job;\n  }\n}\n",
            )
            .file(
                "src/beta.processor.ts",
                TS,
                "export class Shadow { handle(job) { return job; } }\n@Processor('beta') export class BetaProcessor { @Process('report') handle(job) { return job; } }\n",
            )
            .file(
                "src/gamma.processor.ts",
                TS,
                "export class Other { @Process('extra') work(job) { return job; } }\n@Processor('gamma') export class GammaProcessor { work(job) { return job; } }\n",
            )
            .file(
                "src/delta.processor.ts",
                TS,
                "@Processor('delta')\nexport class DeltaProcessor extends WorkerHost {\n  async process(job) {\n    return job;\n  }\n}\n",
            )
            .file(
                "src/producer.ts",
                TS,
                "const alpha = new Queue('alpha');\nconst beta = new Queue('beta');\nconst gamma = new Queue('gamma');\nconst delta = new Queue('delta');\nexport function produce() {\n  alpha.add('welcome');\n  beta.add('report');\n  gamma.add('extra');\n  delta.add('nightly');\n}\n",
            );
        let fixture = with_nodes(
            fixture,
            &[
                (
                    "class:Alpha",
                    NodeKind::Class,
                    "AlphaProcessor",
                    "src/alpha.processor.ts",
                    TS,
                    (1, 11),
                ),
                (
                    "method:onWelcome",
                    NodeKind::Method,
                    "onWelcome",
                    "src/alpha.processor.ts",
                    TS,
                    (3, 6),
                ),
                (
                    "method:onReport",
                    NodeKind::Method,
                    "onReport",
                    "src/alpha.processor.ts",
                    TS,
                    (7, 10),
                ),
                (
                    "class:Shadow",
                    NodeKind::Class,
                    "Shadow",
                    "src/beta.processor.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "method:Shadow.handle",
                    NodeKind::Method,
                    "handle",
                    "src/beta.processor.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "class:Beta",
                    NodeKind::Class,
                    "BetaProcessor",
                    "src/beta.processor.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "method:Beta.handle",
                    NodeKind::Method,
                    "handle",
                    "src/beta.processor.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "class:Other",
                    NodeKind::Class,
                    "Other",
                    "src/gamma.processor.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "method:Other.work",
                    NodeKind::Method,
                    "work",
                    "src/gamma.processor.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "class:Gamma",
                    NodeKind::Class,
                    "GammaProcessor",
                    "src/gamma.processor.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "method:Gamma.work",
                    NodeKind::Method,
                    "work",
                    "src/gamma.processor.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "class:Delta",
                    NodeKind::Class,
                    "DeltaProcessor",
                    "src/delta.processor.ts",
                    TS,
                    (1, 6),
                ),
                (
                    "method:process",
                    NodeKind::Method,
                    "process",
                    "src/delta.processor.ts",
                    TS,
                    (3, 5),
                ),
                (
                    "fn:produce",
                    NodeKind::Function,
                    "produce",
                    "src/producer.ts",
                    TS,
                    (5, 10),
                ),
            ],
        );
        assert_eq!(
            sorted_ends(&fixture, "queue-job"),
            [
                pair("fn:produce", "method:Beta.handle"),
                pair("fn:produce", "method:onWelcome"),
                pair("fn:produce", "method:process"),
            ]
        );
    }

    /// A Bull handler named in a registration is the one function or method
    /// of its name in the registering file (several there: none), else in
    /// the file its head is imported from, else in the project; an inline
    /// one is the function holding the registration, else the value.
    #[test]
    fn a_handler_name_is_its_files_else_its_imports_else_the_projects() {
        const TS: Language = Language::TypeScript;
        let fixture = Fixture::new("handlers")
            .file(
                "src/local.ts",
                TS,
                "import * as handlers from './handlers';\nexport function handleLocal(job) { return job; }\nexport function setup() {\n  jobsQueue.process('local', handleLocal);\n  jobsQueue.process('twice', handleTwice);\n  jobsQueue.process('imported', handlers.onImported);\n  jobsQueue.process('dup', handlers.onDup);\n  jobsQueue.process('project', handleProject);\n  jobsQueue.process('nowhere', handleNowhere);\n  jobsQueue.process('value', handleValue);\n}\nexport function handleTwice(job) { return job; }\nexport class Twice { handleTwice(job) { return job; } }\n",
            )
            .file(
                "src/handlers.ts",
                TS,
                "export function onImported(job) { return job; }\nexport function onDup(job) { return job; }\nexport class Dup { onDup(job) { return job; } }\n",
            )
            .file(
                "src/elsewhere.ts",
                TS,
                "export function handleLocal(job) { return job; }\nexport function onImported(job) { return job; }\nexport function handleNowhere(job) { return job; }\nexport const handleValue = 1;\n",
            )
            .file(
                "src/project.ts",
                TS,
                "export function handleProject(job) { return job; }\nexport function handleNowhere(job) { return job; }\n",
            )
            .file(
                "src/inline.ts",
                TS,
                "export const registry = {\n  setup() {\n    jobsQueue.process('inline', async (job) => job);\n  },\n};\n",
            )
            .file(
                "src/producer.ts",
                TS,
                "export function produce() {\n  jobsQueue.add('local');\n  jobsQueue.add('twice');\n  jobsQueue.add('imported');\n  jobsQueue.add('dup');\n  jobsQueue.add('project');\n  jobsQueue.add('nowhere');\n  jobsQueue.add('value');\n  jobsQueue.add('inline');\n}\n",
            );
        let fixture = with_nodes(
            fixture,
            &[
                (
                    "local:handleLocal",
                    NodeKind::Function,
                    "handleLocal",
                    "src/local.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "local:setup",
                    NodeKind::Function,
                    "setup",
                    "src/local.ts",
                    TS,
                    (3, 11),
                ),
                (
                    "local:handleTwice",
                    NodeKind::Function,
                    "handleTwice",
                    "src/local.ts",
                    TS,
                    (12, 12),
                ),
                (
                    "local:Twice",
                    NodeKind::Class,
                    "Twice",
                    "src/local.ts",
                    TS,
                    (13, 13),
                ),
                (
                    "local:Twice.handleTwice",
                    NodeKind::Method,
                    "handleTwice",
                    "src/local.ts",
                    TS,
                    (13, 13),
                ),
                (
                    "handlers:onImported",
                    NodeKind::Function,
                    "onImported",
                    "src/handlers.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "handlers:onDup",
                    NodeKind::Function,
                    "onDup",
                    "src/handlers.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "handlers:Dup",
                    NodeKind::Class,
                    "Dup",
                    "src/handlers.ts",
                    TS,
                    (3, 3),
                ),
                (
                    "handlers:Dup.onDup",
                    NodeKind::Method,
                    "onDup",
                    "src/handlers.ts",
                    TS,
                    (3, 3),
                ),
                (
                    "elsewhere:handleLocal",
                    NodeKind::Function,
                    "handleLocal",
                    "src/elsewhere.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "elsewhere:onImported",
                    NodeKind::Function,
                    "onImported",
                    "src/elsewhere.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "elsewhere:handleNowhere",
                    NodeKind::Function,
                    "handleNowhere",
                    "src/elsewhere.ts",
                    TS,
                    (3, 3),
                ),
                (
                    "elsewhere:handleValue",
                    NodeKind::Constant,
                    "handleValue",
                    "src/elsewhere.ts",
                    TS,
                    (4, 4),
                ),
                (
                    "project:handleProject",
                    NodeKind::Function,
                    "handleProject",
                    "src/project.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "project:handleNowhere",
                    NodeKind::Function,
                    "handleNowhere",
                    "src/project.ts",
                    TS,
                    (2, 2),
                ),
                (
                    "inline:registry",
                    NodeKind::Constant,
                    "registry",
                    "src/inline.ts",
                    TS,
                    (1, 5),
                ),
                (
                    "inline:setup",
                    NodeKind::Method,
                    "setup",
                    "src/inline.ts",
                    TS,
                    (2, 4),
                ),
                (
                    "fn:produce",
                    NodeKind::Function,
                    "produce",
                    "src/producer.ts",
                    TS,
                    (1, 10),
                ),
            ],
        );
        assert_eq!(
            sorted_ends(&fixture, "queue-job"),
            [
                pair("fn:produce", "handlers:onImported"),
                pair("fn:produce", "inline:setup"),
                pair("fn:produce", "local:handleLocal"),
                pair("fn:produce", "project:handleProject"),
            ]
        );
    }

    /// Events: a socket client's emit reaches the server's handlers of its
    /// event, never a bus's `.on`, the transport's own events reach nothing,
    /// and an `@OnEvent` array names each of its events.
    #[test]
    fn an_event_reaches_its_handlers_by_shape_and_name() {
        const TS: Language = Language::TypeScript;
        let generic = [
            "error",
            "connect",
            "connect_error",
            "connect_failed",
            "connection",
            "disconnect",
            "disconnecting",
            "reconnect",
            "reconnect_attempt",
            "reconnecting",
            "reconnect_error",
            "reconnect_failed",
            "close",
            "open",
            "end",
            "data",
            "ready",
            "drain",
            "finish",
            "pipe",
            "unpipe",
            "listening",
            "timeout",
            "ping",
            "pong",
            "upgrade",
            "newListener",
            "removeListener",
        ];
        let mut client = String::from("export function talk() {\n  socket.emit('chat');\n");
        let mut gateway = String::from(
            "export class ChatGateway {\n  @SubscribeMessage('chat')\n  onChat() {\n    this.server.emit('notice');\n  }\n",
        );
        let mut gateway_nodes = vec![node(
            "gw:onChat",
            NodeKind::Method,
            "onChat",
            "src/chat.gateway.ts",
            TS,
            (3, 5),
        )];
        for (at, event) in generic.iter().enumerate() {
            client.push_str(&format!("  socket.emit('{event}');\n"));
            let line = 6 + 2 * at as i64;
            gateway.push_str(&format!(
                "  @SubscribeMessage('{event}')\n  on{at}() {{ return 1; }}\n"
            ));
            gateway_nodes.push(node(
                &format!("gw:on{at}"),
                NodeKind::Method,
                &format!("on{at}"),
                "src/chat.gateway.ts",
                TS,
                (line + 1, line + 1),
            ));
        }
        client.push_str("}\nemitter.on('notice', handleNotice);\nsocket.on('notice', onNotice);\nexport function handleNotice() { return 1; }\nexport function onNotice() { return 1; }\nexport function publish() {\n  eventBus.emit('user.updated');\n  eventBus.emit('error');\n}\n");
        gateway.push_str("}\nexport class Audit {\n  @OnEvent(['user.created', 'user.updated'])\n  audit() { return 1; }\n  @OnEvent('error')\n  onError() { return 1; }\n}\n");
        let client_lines = client.lines().count() as i64;
        let talk_end = 2 + generic.len() as i64 + 1;
        let gateway_lines = gateway.lines().count() as i64;
        let fixture = Fixture::new("events")
            .file("src/chat.client.ts", TS, &client)
            .file("src/chat.gateway.ts", TS, &gateway);
        let mut fixture = with_nodes(
            fixture,
            &[
                (
                    "client:talk",
                    NodeKind::Function,
                    "talk",
                    "src/chat.client.ts",
                    TS,
                    (1, talk_end),
                ),
                (
                    "client:handleNotice",
                    NodeKind::Function,
                    "handleNotice",
                    "src/chat.client.ts",
                    TS,
                    (talk_end + 3, talk_end + 3),
                ),
                (
                    "client:onNotice",
                    NodeKind::Function,
                    "onNotice",
                    "src/chat.client.ts",
                    TS,
                    (talk_end + 4, talk_end + 4),
                ),
                (
                    "client:publish",
                    NodeKind::Function,
                    "publish",
                    "src/chat.client.ts",
                    TS,
                    (talk_end + 5, client_lines),
                ),
                (
                    "gw:audit",
                    NodeKind::Method,
                    "audit",
                    "src/chat.gateway.ts",
                    TS,
                    (gateway_lines - 3, gateway_lines - 3),
                ),
                (
                    "gw:onError",
                    NodeKind::Method,
                    "onError",
                    "src/chat.gateway.ts",
                    TS,
                    (gateway_lines - 1, gateway_lines - 1),
                ),
            ],
        );
        for gateway_node in gateway_nodes {
            fixture = fixture.node(gateway_node);
        }
        let mut all: Vec<(String, String)> = ends(&fixture.edges("event-bus"));
        all.sort();
        assert_eq!(
            all,
            [
                pair("client:publish", "gw:audit"),
                pair("client:talk", "gw:onChat"),
                pair("gw:onChat", "client:onNotice"),
            ]
        );
    }

    /// A Laravel request is a call on `$this` (`$this->app->get(…)` and
    /// `$client->json(…)` are none), `->json` or `->call` with either quote,
    /// or a test's own helper on `$this`.
    #[test]
    fn a_laravel_test_requests_through_this() {
        const PHP: Language = Language::Php;
        let file = "tests/Feature/AboutTest.php";
        let fixture = Fixture::new("laravel-this").file(
            file,
            PHP,
            "<?php\nclass AboutTest extends TestCase {\n    public function testRooted() { $this->get('/about'); }\n    public function testContainer() { $this->app->get('/about'); }\n    public function testCall() { $this->call(\"POST\", '/contact'); }\n    public function testClient() { $client->json('POST', '/contact'); }\n    public function testHelper() { $this->getAs('/about'); }\n    public function testResponse() { $response->getAs('/about'); }\n    protected function getAs($uri) { return $this->get($uri); }\n}\n",
        );
        let mut fixture = fixture;
        for (at, name) in [
            "testRooted",
            "testContainer",
            "testCall",
            "testClient",
            "testHelper",
            "testResponse",
            "getAs",
        ]
        .iter()
        .enumerate()
        {
            let line = at as i64 + 3;
            fixture = fixture.node(node(
                &format!("php:{name}"),
                NodeKind::Method,
                name,
                file,
                PHP,
                (line, line),
            ));
        }
        let fixture = with_nodes(
            fixture,
            &[
                (
                    "route:about",
                    NodeKind::Route,
                    "GET /about",
                    "routes/web.php",
                    PHP,
                    (3, 3),
                ),
                (
                    "route:contact",
                    NodeKind::Route,
                    "POST /contact",
                    "routes/web.php",
                    PHP,
                    (4, 4),
                ),
            ],
        );
        assert_eq!(
            sorted_ends(&fixture, "test-request"),
            [
                pair("php:testCall", "route:contact"),
                pair("php:testHelper", "route:about"),
                pair("php:testRooted", "route:about"),
            ]
        );
    }

    /// Pest's request functions are read only in a file naming
    /// `Pest\Laravel`, and only where no word, `$`, `>`, `:` or `\` comes
    /// before the name: `xget`, `$get`, `$r->get`, `Http::get` and `\get` are
    /// other calls.
    #[test]
    fn pest_requests_are_bare_calls_in_a_file_naming_pest() {
        const PHP: Language = Language::Php;
        let fixture = Fixture::new("pest")
            .file(
                "tests/Feature/PagesTest.php",
                PHP,
                "<?php\nuse function Pest\\Laravel\\get;\nit('works', function () {\n    get('/home');\n    xget('/p1');\n    $get('/p2');\n    $response->get('/p3');\n    Http::get('/p4');\n    \\get('/p5');\n});\n",
            )
            .file(
                "tests/Feature/PlainTest.php",
                PHP,
                "<?php\nclass PlainTest extends TestCase {\n    public function testPlain() { $this->assertTrue(true); get('/plain'); }\n}\n",
            );
        let mut fixture = with_nodes(
            fixture,
            &[
                (
                    "file:pages",
                    NodeKind::File,
                    "PagesTest.php",
                    "tests/Feature/PagesTest.php",
                    PHP,
                    (1, 10),
                ),
                (
                    "file:plain",
                    NodeKind::File,
                    "PlainTest.php",
                    "tests/Feature/PlainTest.php",
                    PHP,
                    (1, 4),
                ),
                (
                    "php:testPlain",
                    NodeKind::Method,
                    "testPlain",
                    "tests/Feature/PlainTest.php",
                    PHP,
                    (3, 3),
                ),
            ],
        );
        for (at, path) in ["/home", "/p1", "/p2", "/p3", "/p4", "/p5", "/plain"]
            .iter()
            .enumerate()
        {
            let line = at as i64 + 1;
            fixture = fixture.node(node(
                &format!("route:{at}"),
                NodeKind::Route,
                &format!("GET {path}"),
                "routes/web.php",
                PHP,
                (line, line),
            ));
        }
        assert_eq!(
            sorted_ends(&fixture, "test-request"),
            [pair("file:pages", "route:0")]
        );
    }

    /// Spring's request forms: a MockMvc builder (`get`, `multipart` as a
    /// POST) or `request(HttpMethod.X, …)` in a file naming
    /// `MockMvcRequestBuilders`, never after a name or a `.`; and each
    /// `restTemplate` method's verb.
    #[test]
    fn spring_tests_request_through_builders_and_rest_templates() {
        const JAVA: Language = Language::Java;
        let builders = "src/test/java/BuildersTests.java";
        let plain = "src/test/java/PlainTests.java";
        let templates = "src/test/java/TemplatesTests.java";
        let fixture = Fixture::new("spring-forms")
            .file(
                builders,
                JAVA,
                "import org.springframework.test.web.servlet.request.MockMvcRequestBuilders;\nclass BuildersTests {\n    void a() { mockMvc.perform(get(\"/a\")); }\n    void b() { mockMvc.perform(request(HttpMethod.POST, \"/b\")); }\n    void c() { mockMvc.perform(multipart(\"/c\")); }\n    void d() { mockMvc.perform(x.get(\"/d\")); xget(\"/e\"); $get(\"/f\"); }\n}\n",
            )
            .file(
                plain,
                JAVA,
                "class PlainTests {\n    void p() { mockMvc.perform(get(\"/plain\")); }\n}\n",
            )
            .file(
                templates,
                JAVA,
                "class TemplatesTests {\n    void t() {\n        restTemplate.postForEntity(\"/t1\", body, X.class);\n        restTemplate.put(\"/t2\", body);\n        restTemplate.delete(\"/t3\");\n        restTemplate.patchForObject(\"/t4\", body, X.class);\n        restTemplate.getForObject(\"/t5\", X.class);\n    }\n}\n",
            );
        let mut fixture = with_nodes(
            fixture,
            &[
                ("java:a", NodeKind::Method, "a", builders, JAVA, (3, 3)),
                ("java:b", NodeKind::Method, "b", builders, JAVA, (4, 4)),
                ("java:c", NodeKind::Method, "c", builders, JAVA, (5, 5)),
                ("java:d", NodeKind::Method, "d", builders, JAVA, (6, 6)),
                ("java:p", NodeKind::Method, "p", plain, JAVA, (2, 2)),
                ("java:t", NodeKind::Method, "t", templates, JAVA, (2, 8)),
            ],
        );
        for (at, name) in [
            "GET /a",
            "POST /b",
            "POST /c",
            "GET /d",
            "GET /e",
            "GET /f",
            "GET /plain",
            "POST /t1",
            "PUT /t2",
            "DELETE /t3",
            "PATCH /t4",
            "GET /t5",
        ]
        .iter()
        .enumerate()
        {
            let line = at as i64 + 1;
            fixture = fixture.node(node(
                &format!("route:{at}"),
                NodeKind::Route,
                name,
                "src/main/java/Controller.java",
                JAVA,
                (line, line),
            ));
        }
        let mut expected = vec![
            pair("java:a", "route:0"),
            pair("java:b", "route:1"),
            pair("java:c", "route:2"),
            pair("java:t", "route:7"),
            pair("java:t", "route:8"),
            pair("java:t", "route:9"),
            pair("java:t", "route:10"),
            pair("java:t", "route:11"),
        ];
        expected.sort();
        assert_eq!(sorted_ends(&fixture, "test-request"), expected);
    }

    /// A request helper is a PHP method of a test file whose own lines make
    /// a request: a function, another language's method, a method outside
    /// the tests, and a method whose file but not body requests are none.
    #[test]
    fn a_request_helper_is_a_php_test_method_whose_body_requests() {
        const PHP: Language = Language::Php;
        let test = "tests/Feature/HelpersTest.php";
        let fixture = Fixture::new("helpers")
            .file(
                test,
                PHP,
                "<?php\nclass HelpersTest extends TestCase {\n    public function testA() { $this->getKind('/a'); }\n    public function testB() { $this->getLang('/b'); }\n    public function testC() { $this->getPath('/c'); }\n    public function testD() { $this->getSlice('/d'); }\n    public function testE() { $this->getReal('/e'); }\n    protected function getSlice($uri) { return $uri; }\n    protected function other() { return $this->get('/z'); }\n    protected function getReal($uri) { return $this->get($uri); }\n}\n",
            )
            .file(
                "tests/Feature/kinds.php",
                PHP,
                "<?php\nfunction getKind($uri) { return $this->get($uri); }\n",
            )
            .file(
                "tests/js/lang.test.ts",
                Language::TypeScript,
                "class Lang {\n  getLang(uri) { return $this->get(uri); }\n}\n",
            )
            .file(
                "app/Support/Path.php",
                PHP,
                "<?php\nclass Path {\n    public function getPath($uri) { return $this->get($uri); }\n}\n",
            );
        let mut fixture = with_nodes(
            fixture,
            &[
                (
                    "kinds:getKind",
                    NodeKind::Function,
                    "getKind",
                    "tests/Feature/kinds.php",
                    PHP,
                    (2, 2),
                ),
                (
                    "lang:getLang",
                    NodeKind::Method,
                    "getLang",
                    "tests/js/lang.test.ts",
                    Language::TypeScript,
                    (2, 2),
                ),
                (
                    "path:getPath",
                    NodeKind::Method,
                    "getPath",
                    "app/Support/Path.php",
                    PHP,
                    (3, 3),
                ),
                (
                    "php:getSlice",
                    NodeKind::Method,
                    "getSlice",
                    test,
                    PHP,
                    (8, 8),
                ),
                ("php:other", NodeKind::Method, "other", test, PHP, (9, 9)),
                (
                    "php:getReal",
                    NodeKind::Method,
                    "getReal",
                    test,
                    PHP,
                    (10, 10),
                ),
            ],
        );
        for (at, name) in ["testA", "testB", "testC", "testD", "testE"]
            .iter()
            .enumerate()
        {
            let line = at as i64 + 3;
            fixture = fixture
                .node(node(
                    &format!("php:{name}"),
                    NodeKind::Method,
                    name,
                    test,
                    PHP,
                    (line, line),
                ))
                .node(node(
                    &format!("route:{at}"),
                    NodeKind::Route,
                    &format!("GET /{}", ["a", "b", "c", "d", "e"][at]),
                    "routes/web.php",
                    PHP,
                    (line, line),
                ));
        }
        assert_eq!(
            sorted_ends(&fixture, "test-request"),
            [pair("php:testE", "route:4")]
        );
    }

    /// Each pass draws a pair once, the first channel's: a function adding
    /// a job and emitting an event one method consumes and handles is one
    /// `queue-job` edge, and a test requesting a route twice one edge.
    #[test]
    fn a_pass_draws_each_pair_once_with_its_first_channel() {
        const TS: Language = Language::TypeScript;
        const PHP: Language = Language::Php;
        let fixture = Fixture::new("seen")
            .file(
                "src/producer.ts",
                TS,
                "export function run() {\n  emailQueue.add('welcome');\n  eventBus.emit('user.created');\n}\n",
            )
            .file(
                "src/consumer.ts",
                TS,
                "emailQueue.process('welcome', handle);\nexport class Listener {\n  @OnEvent('user.created')\n  handle(payload) {\n    return payload;\n  }\n}\n",
            )
            .file(
                "tests/Feature/UsersTest.php",
                PHP,
                "<?php\nclass UsersTest extends TestCase {\n    public function testUsers() {\n        $this->get('/users');\n        $this->get('/users?page=2');\n    }\n}\n",
            );
        let fixture = with_nodes(
            fixture,
            &[
                (
                    "fn:run",
                    NodeKind::Function,
                    "run",
                    "src/producer.ts",
                    TS,
                    (1, 4),
                ),
                (
                    "class:Listener",
                    NodeKind::Class,
                    "Listener",
                    "src/consumer.ts",
                    TS,
                    (2, 7),
                ),
                (
                    "method:handle",
                    NodeKind::Method,
                    "handle",
                    "src/consumer.ts",
                    TS,
                    (4, 6),
                ),
                (
                    "php:testUsers",
                    NodeKind::Method,
                    "testUsers",
                    "tests/Feature/UsersTest.php",
                    PHP,
                    (3, 6),
                ),
                (
                    "route:users",
                    NodeKind::Route,
                    "GET /users",
                    "routes/web.php",
                    PHP,
                    (3, 3),
                ),
            ],
        );
        let rows = |edges: Vec<Edge>| -> Vec<(String, String, String, Option<i64>)> {
            edges
                .into_iter()
                .map(|edge| {
                    let by = edge.metadata.as_ref().unwrap()["synthesizedBy"]
                        .as_str()
                        .unwrap()
                        .to_string();
                    (edge.source, edge.target, by, edge.line)
                })
                .collect()
        };
        let row = |source: &str, target: &str, by: &str, line: i64| {
            (
                source.to_string(),
                target.to_string(),
                by.to_string(),
                Some(line),
            )
        };
        assert_eq!(
            rows(fixture.pass(cross_tier_edges)),
            [row("fn:run", "method:handle", "queue-job", 2)]
        );
        assert_eq!(
            rows(fixture.pass(test_request_edges)),
            [row("php:testUsers", "route:users", "test-request", 4)]
        );
        // The stage keeps the same edge.
        assert_eq!(
            sorted_ends(&fixture, "queue-job"),
            [pair("fn:run", "method:handle")]
        );
        assert!(fixture.edges("event-bus").is_empty());
    }

    /// An imported receiver takes the binding its file exports under the
    /// imported name, an alias resolved; a file that is no JavaScript binds
    /// nothing.
    #[test]
    fn an_imported_receiver_takes_the_binding_exported_under_its_name() {
        const TS: Language = Language::TypeScript;
        let fixture = Fixture::new("import-names")
            .file(
                "src/shop.ts",
                TS,
                "export const shop = axios.create({ baseURL: '/shop' });\n",
            )
            .file(
                "src/queues.ts",
                TS,
                "export const mailQueue = new Queue('email');\n",
            )
            .file(
                "src/api.vue",
                Language::Vue,
                "export const billing = axios.create({ baseURL: '/vue' });\n",
            )
            .file(
                "src/client.ts",
                TS,
                "import { shop as store } from './shop';\nimport { mailQueue as q } from './queues';\nimport { billing } from './api.vue';\nexport function load() { return store.get('/items'); }\nexport function send() { q.add('welcome'); }\nexport function bill() { return billing.get('/x'); }\n",
            )
            .file(
                "src/consumer.ts",
                TS,
                "const email = new Queue('email');\nemail.process('welcome', onWelcome);\nexport function onWelcome(job) { return job; }\n",
            );
        let fixture = with_nodes(
            fixture,
            &[
                (
                    "fn:load",
                    NodeKind::Function,
                    "load",
                    "src/client.ts",
                    TS,
                    (4, 4),
                ),
                (
                    "fn:send",
                    NodeKind::Function,
                    "send",
                    "src/client.ts",
                    TS,
                    (5, 5),
                ),
                (
                    "fn:bill",
                    NodeKind::Function,
                    "bill",
                    "src/client.ts",
                    TS,
                    (6, 6),
                ),
                (
                    "fn:onWelcome",
                    NodeKind::Function,
                    "onWelcome",
                    "src/consumer.ts",
                    TS,
                    (3, 3),
                ),
                (
                    "route:items",
                    NodeKind::Route,
                    "GET /shop/items",
                    "src/server.ts",
                    TS,
                    (1, 1),
                ),
                (
                    "route:vue",
                    NodeKind::Route,
                    "GET /vue/x",
                    "src/server.ts",
                    TS,
                    (2, 2),
                ),
            ],
        );
        assert_eq!(
            sorted_ends(&fixture, "http-client"),
            [pair("fn:load", "route:items")]
        );
        assert_eq!(
            sorted_ends(&fixture, "queue-job"),
            [pair("fn:send", "fn:onWelcome")]
        );
    }
}
