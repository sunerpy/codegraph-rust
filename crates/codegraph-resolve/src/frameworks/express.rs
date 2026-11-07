//! Express and plain Node.js routing (upstream
//! `resolution/frameworks/express.ts` at `e8ddc4e2`; #1957, #2200, #2243,
//! #2244, #2261).
//!
//! `app.get('/x', handler)` and `router.route('/x').get(…).put(…)` are route
//! nodes. An inline handler lends its route the calls its body makes and the
//! replies it sends; any other handler is a reference to its name. A mount,
//! `X.use('/prefix', router)`, renames the mounted router file's routes to
//! the paths a request takes ([`ExpressResolver::post_extract`]). Three name
//! patterns resolve a middleware, an `XController.method` or an
//! `XService.method` reference, and only to a declaration of the reference's
//! own file (#2243).
//!
//! Offsets are byte offsets. Comments are blanked byte for byte
//! ([`strip_js_comments_keeping_offsets`]), so an offset into the stripped
//! text indexes the source too. Upstream counts UTF-16 code units and blanks
//! a comment one space per unit, so whatever it measures — a column, a
//! match's length, the text a test reads before or after a position — is
//! measured here on the source at the same offsets ([`Text`]).

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::js_scan::{JS_SPACE, is_js_space, js_trim};
use super::{framework_node, now_millis, sole};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::import_resolver::resolve_import_path;
use crate::strip_comments::strip_js_comments_keeping_offsets;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages Express reads (upstream `languages`): a `.tsx` or `.jsx`
/// file is neither, so its routes are never extracted and its references
/// never resolved here.
const LANGUAGES: [Language; 2] = [Language::JavaScript, Language::TypeScript];

/// Express response and request methods and common JavaScript built-ins
/// (upstream `RESERVED_CALLS`): a call to one in a handler body is framework
/// noise, not the business flow a route edge shows, receiver or not.
const RESERVED_CALLS: &[&str] = &[
    "json",
    "jsonp",
    "send",
    "sendStatus",
    "sendFile",
    "status",
    "end",
    "redirect",
    "render",
    "set",
    "get",
    "header",
    "type",
    "format",
    "attachment",
    "download",
    "cookie",
    "clearCookie",
    "append",
    "location",
    "vary",
    "links",
    "accepts",
    "is",
    "next",
    "then",
    "catch",
    "finally",
    "resolve",
    "reject",
    "all",
    "race",
    "map",
    "filter",
    "forEach",
    "reduce",
    "find",
    "push",
    "pop",
    "slice",
    "splice",
    "includes",
    "keys",
    "values",
    "entries",
    "assign",
    "parse",
    "stringify",
    "log",
    "error",
    "warn",
    "info",
    "String",
    "Number",
    "Boolean",
    "Array",
    "Object",
    "Date",
    "Math",
    "JSON",
    "Promise",
    "require",
    "fail",
];

/// Keywords a `name(` scan over a handler body would read as calls
/// (upstream `JS_KEYWORDS`), unless written as a member.
const JS_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "function", "async", "return", "typeof", "await", "new", "do",
    "else", "with", "yield", "void", "delete", "in", "super", "import",
];

/// The methods an Express route node's name starts with: what a route's
/// in-file name is read back with ([`in_file_name`]).
const ROUTE_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "ALL", "USE"];

/// Mount rounds at most (upstream's `round < 8`).
const MOUNT_ROUNDS: usize = 8;

pub struct ExpressResolver;

impl FrameworkResolver for ExpressResolver {
    fn name(&self) -> &str {
        "express"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// Express (or Fastify, Koa, hapi) in a project manifest, or a file
    /// under a `routes`, `controllers` or `middleware` path that mentions
    /// `express`, `app.get` or `router.get`.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if super::package_deps::depends_on(
            context,
            &["express", "fastify", "koa", "hapi", "@hapi/hapi"],
        ) {
            return true;
        }
        context.get_all_files_shared().iter().any(|file| {
            (file.contains("routes") || file.contains("controllers") || file.contains("middleware"))
                && context.read_file_shared(file).is_some_and(|content| {
                    content.contains("express")
                        || content.contains("app.get")
                        || content.contains("router.get")
                })
        })
    }

    /// A JS/TS module reaches another file's middleware, controller or
    /// service only through an `import` or `require`, the import resolver's,
    /// never by name; the patterns resolve the file's own declarations
    /// (#2243).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        let same_file = |id: Option<String>| {
            id.filter(|id| {
                context
                    .get_node_by_id_shared(id)
                    .is_some_and(|node| node.file_path == reference.file_path)
            })
        };
        let resolved = |target: String, confidence: f64| {
            ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
        };

        if is_middleware_name(name)
            && let Some(target) = same_file(resolve_middleware(name, context, &reference.file_path))
        {
            return Some(resolved(target, 0.8));
        }

        static CONTROLLER: OnceLock<Regex> = OnceLock::new();
        let controller = CONTROLLER.get_or_init(|| {
            Regex::new(r"^([A-Za-z0-9_]+)Controller\.([A-Za-z0-9_]+)$").expect("controller")
        });
        if let Some(captures) = controller.captures(name)
            && let Some(target) = same_file(resolve_controller_method(
                &captures[1],
                &captures[2],
                context,
            ))
        {
            return Some(resolved(target, 0.85));
        }

        static SERVICE: OnceLock<Regex> = OnceLock::new();
        let service = SERVICE.get_or_init(|| {
            Regex::new(r"^([A-Za-z0-9_]+)(Service|Helper|Utils?)\.([A-Za-z0-9_]+)$")
                .expect("service")
        });
        if let Some(captures) = service.captures(name)
            && let Some(target) = same_file(resolve_service_method(
                &format!("{}{}", &captures[1], &captures[2]),
                &captures[3],
                context,
            ))
        {
            return Some(resolved(target, 0.8));
        }

        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(extract_routes(file_path, content))
    }

    /// Mount prefixes (upstream `postExtract`). A router's routes are
    /// written relative to where it is mounted —
    ///
    /// ```text
    /// app.use('/api', routes)            // app.js
    /// router.use('/users', usersRouter)  // routes/index.js
    /// router.post('/', createUser)       // routes/users.js  → POST /api/users
    /// ```
    ///
    /// — so every `X.use('/prefix', …, router)` whose last argument names a
    /// router file (an import, an inline `require('./x')`, or a name bound to
    /// one) composes its prefix down the mount tree, and each mounted file's
    /// routes are renamed to the path a request takes. A file mounted at two
    /// different prefixes is left alone: one name cannot be two paths. A
    /// route keeps its `id` and `qualified_name`, which still reads the
    /// in-file `METHOD:path` every name is computed from.
    ///
    /// Only Express's own routes are renamed, and a route an earlier pass
    /// renamed that nothing renames now gets its in-file name back, so a sync
    /// converges with a fresh index. Upstream renames every route of the
    /// file whose qualified name reads `file::X:/path`, another framework's
    /// too, and never resets one: a name another framework gave its route
    /// could not be read back when the mount went away.
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let files = context.get_all_files_shared();
        let mut mounts: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for file in files.iter().filter(|file| is_route_source(file)) {
            let Some(content) = context.read_file_shared(file) else {
                continue;
            };
            if !content.contains(".use(") {
                continue;
            }
            let language = express_language(file);
            let text = Text::new(&content);
            for mount in find_mounts(&text) {
                let Some((close, structure)) = scan_call(&text, mount.open) else {
                    continue;
                };
                // Split where the blanked structure says, but read the
                // target from the stripped source: a `require('./users')`
                // target needs its string.
                let args = &structure[1..structure.len() - 1];
                let parts = top_level_args(args);
                let Some(last_part) = parts.last() else {
                    continue;
                };
                let last = js_trim(&text.safe[mount.open + 1 + last_part.start..close]);
                let Some(target) = mount_target(last, &text.safe, file, language, context) else {
                    continue;
                };
                if target == *file {
                    continue;
                }
                mounts
                    .entry(file.clone())
                    .or_default()
                    .push((mount.prefix, target));
            }
        }

        let mut prefix_of: BTreeMap<String, String> = BTreeMap::new();
        if !mounts.is_empty() {
            for _ in 0..MOUNT_ROUNDS {
                let mut next: BTreeMap<String, String> = BTreeMap::new();
                let mut ambiguous: BTreeSet<String> = BTreeSet::new();
                for (file, list) in &mounts {
                    let base = prefix_of.get(file).map_or("", String::as_str);
                    for (prefix, target) in list {
                        let full = join_paths(base, prefix);
                        match next.get(target) {
                            Some(seen) if *seen != full => {
                                ambiguous.insert(target.clone());
                            }
                            _ => {
                                next.insert(target.clone(), full);
                            }
                        }
                    }
                }
                for target in &ambiguous {
                    next.remove(target);
                }
                let changed = next != prefix_of;
                prefix_of = next;
                if !changed {
                    break;
                }
            }
        }

        let mut updates: Vec<Node> = Vec::new();
        let mut owned_cache: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut renamed: BTreeSet<String> = BTreeSet::new();
        for (file, prefix) in &prefix_of {
            if prefix.is_empty() || prefix == "/" {
                continue;
            }
            let owned = owned_routes(&mut owned_cache, file, context);
            for route in context.get_nodes_in_file_shared(file) {
                if route.kind != NodeKind::Route || !owned.contains(&route.id) {
                    continue;
                }
                let Some((method, original)) = qualified_route(&route) else {
                    continue;
                };
                if !original.starts_with('/') {
                    continue;
                }
                renamed.insert(route.id.clone());
                let name = format!("{method} {}", join_paths(prefix, original));
                if name != route.name {
                    let mut updated = Node::clone(&route);
                    updated.name = name;
                    updates.push(updated);
                }
            }
        }

        // A route an earlier pass renamed that nothing renames now.
        let mut stale: BTreeMap<String, Vec<(Arc<Node>, String)>> = BTreeMap::new();
        for route in context.get_nodes_by_kind_shared(NodeKind::Route).iter() {
            if renamed.contains(&route.id) {
                continue;
            }
            if let Some(in_file) = in_file_name(route)
                && route.name != in_file
            {
                stale
                    .entry(route.file_path.clone())
                    .or_default()
                    .push((Arc::clone(route), in_file));
            }
        }
        for (file, routes) in stale {
            let owned = owned_routes(&mut owned_cache, &file, context);
            for (route, in_file) in routes {
                if owned.contains(&route.id) {
                    let mut updated = Node::clone(&route);
                    updated.name = in_file;
                    updated.updated_at = now_millis();
                    updates.push(updated);
                }
            }
        }

        Some(updates)
    }
}

/// Whether Express reads `file_path` (upstream `/\.(m?js|tsx?|cjs)$/`).
fn is_route_source(file_path: &str) -> bool {
    [".js", ".mjs", ".ts", ".tsx", ".cjs"]
        .iter()
        .any(|extension| file_path.ends_with(extension))
}

/// Upstream's own `detectLanguage`: `.ts` and `.tsx` are TypeScript, the
/// rest JavaScript.
fn express_language(file_path: &str) -> Language {
    if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
        Language::TypeScript
    } else {
        Language::JavaScript
    }
}

/// The ids of the routes Express's own extraction makes in `file`, when the
/// pipeline extracts it with Express at all (the file is JavaScript or
/// TypeScript): NestJS, Next.js and Expo routes have ids of the same shape.
fn owned_routes<'c>(
    cache: &'c mut BTreeMap<String, BTreeSet<String>>,
    file: &str,
    context: &dyn ResolutionContext,
) -> &'c BTreeSet<String> {
    cache.entry(file.to_string()).or_insert_with(|| {
        let extracted = context
            .get_node_by_id_shared(&format!("file:{file}"))
            .is_some_and(|node| LANGUAGES.contains(&node.language))
            .then(|| context.read_file_shared(file))
            .flatten();
        extracted.map_or_else(BTreeSet::new, |content| {
            extract_routes(file, &content)
                .nodes
                .into_iter()
                .map(|node| node.id)
                .collect()
        })
    })
}

/// A route's in-file `METHOD` and path, read from its qualified name the way
/// upstream's mount pass reads it: after the first `::`, up to the next `:`.
fn qualified_route(route: &Node) -> Option<(&str, &str)> {
    let qualified = route.qualified_name.as_str();
    let sep = qualified.find("::")?;
    let rest = &qualified[sep + 2..];
    let colon = rest.find(':')?;
    Some((&rest[..colon], &rest[colon + 1..]))
}

/// The name an Express route has in its own file, `METHOD path`, when its
/// qualified name says it is one.
fn in_file_name(route: &Node) -> Option<String> {
    let (method, path) = qualified_route(route)?;
    (ROUTE_METHODS.contains(&method) && path.starts_with('/')).then(|| format!("{method} {path}"))
}

/// `/api` + `/users` → `/api/users`; `/api/` + `/` → `/api` (upstream
/// `joinPaths`).
fn join_paths(prefix: &str, path: &str) -> String {
    let a = prefix.trim_end_matches('/');
    let b = path.trim_start_matches('/');
    let joined = if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}/{b}")
    };
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

// ---------------------------------------------------------------------------
// Text: the source and its stripped twin
// ---------------------------------------------------------------------------

/// A file's source and its comment-stripped text, byte for byte aligned.
struct Text<'a> {
    content: &'a str,
    safe: String,
    /// The offsets of the stripped text's `\n`s, built on first use.
    newlines: OnceCell<Vec<usize>>,
}

impl<'a> Text<'a> {
    fn new(content: &'a str) -> Self {
        Self {
            content,
            safe: strip_js_comments_keeping_offsets(content),
            newlines: OnceCell::new(),
        }
    }

    /// `start..end` of the stripped text as upstream's holds it: a blanked
    /// character is a space per UTF-16 code unit there, a byte here.
    fn upstream(&self, start: usize, end: usize) -> String {
        let mut out = String::with_capacity(end - start);
        for (offset, ch) in self.content[start..end].char_indices() {
            match self.piece(start + offset, ch) {
                Some(blank) => out.extend(std::iter::repeat_n(blank, ch.len_utf16())),
                None => out.push(ch),
            }
        }
        out
    }

    /// Upstream's UTF-16 length of the stripped text in `start..end`, which
    /// is the source's there.
    fn units(&self, start: usize, end: usize) -> i64 {
        self.content[start..end].encode_utf16().count() as i64
    }

    /// The 1-based line of `offset` (upstream `split('\n').length`).
    fn line_at(&self, offset: usize) -> i64 {
        let newlines = self.newlines.get_or_init(|| {
            self.safe
                .bytes()
                .enumerate()
                .filter(|&(_, byte)| byte == b'\n')
                .map(|(at, _)| at)
                .collect()
        });
        newlines.partition_point(|&at| at < offset) as i64 + 1
    }

    /// Upstream `safe.slice(Math.max(floor, end - units), end)` — `floor`
    /// is where the text upstream slices starts — as upstream's stripped
    /// text holds it: a blanked character is a space per code unit there.
    /// A unit cut from its pair is U+FFFD, which every pattern here reads as
    /// it reads the lone surrogate: no word character, no space.
    fn units_before(&self, floor: usize, end: usize, units: usize) -> String {
        let mut reversed: Vec<char> = Vec::new();
        let mut taken = 0;
        for (offset, ch) in self.content[floor..end].char_indices().rev() {
            if taken == units {
                break;
            }
            let width = ch.len_utf16();
            let piece = self.piece(floor + offset, ch);
            if taken + width > units {
                reversed.push(piece.map_or('\u{FFFD}', |_| ' '));
                break;
            }
            taken += width;
            match piece {
                Some(blank) => reversed.extend(std::iter::repeat_n(blank, width)),
                None => reversed.push(ch),
            }
        }
        reversed.into_iter().rev().collect()
    }

    /// Upstream `safe.slice(start, start + units)`, as [`Self::units_before`]
    /// reads it.
    fn units_after(&self, start: usize, units: usize) -> String {
        let mut out = String::new();
        let mut taken = 0;
        for (offset, ch) in self.content[start..].char_indices() {
            if taken == units {
                break;
            }
            let width = ch.len_utf16();
            let piece = self.piece(start + offset, ch);
            if taken + width > units {
                out.push(piece.map_or('\u{FFFD}', |_| ' '));
                break;
            }
            taken += width;
            match piece {
                Some(blank) => out.extend(std::iter::repeat_n(blank, width)),
                None => out.push(ch),
            }
        }
        out
    }

    /// What the stripped text holds for the source character `ch` at `at`:
    /// `Some(byte)` for an ASCII character (the comment stripper may have
    /// blanked it) and for a blanked one (a space per code unit), `None`
    /// for one it kept.
    fn piece(&self, at: usize, ch: char) -> Option<char> {
        let byte = self.safe.as_bytes()[at];
        if ch.is_ascii() || byte == b' ' {
            Some(byte as char)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// JavaScript's `\s` inside a pattern.
fn js_regex(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("{S}", JS_SPACE)).expect("express pattern")
}

/// The routes one file declares and what their handlers reference
/// (upstream `extract`).
fn extract_routes(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    if !is_route_source(file_path) {
        return result;
    }
    let language = express_language(file_path);
    let text = Text::new(content);
    let route = |line: i64, method: &str, path: &str, end_column: i64| {
        let method = method.to_ascii_uppercase();
        framework_node(
            format!("route:{file_path}:{line}:{method}:{path}"),
            NodeKind::Route,
            format!("{method} {path}"),
            format!("{file_path}::{method}:{path}"),
            file_path.to_string(),
            line,
            line,
            0,
            end_column,
            language,
            false,
        )
    };

    // The route head up to the first argument, `(app|router).METHOD('/path',`,
    // not the whole call: an inline handler's brackets are scanned from the
    // call's own paren.
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        js_regex(
            r#"(?-u:\b)(app|router){S}*\.{S}*(get|post|put|patch|delete|all|use){S}*\({S}*['"]([^'"]+)['"]{S}*,"#,
        )
    });
    for captures in head.captures_iter(&text.safe) {
        let whole = captures.get(0).expect("whole match");
        let method = &captures[2];
        let found = captures.get(3).expect("route path");
        let path = text.upstream(found.start(), found.end());
        if method == "use" && !path.starts_with('/') {
            continue;
        }
        let line = text.line_at(whole.start());
        let node = route(line, method, &path, text.units(whole.start(), whole.end()));
        let open = whole.start() + text.safe[whole.start()..].find('(').expect("head paren");
        let args = scan_call(&text, open)
            .map(|(_, structure)| structure[1..structure.len() - 1].to_string())
            .unwrap_or_default();
        result.references.extend(handler_refs(
            &text,
            open + 1,
            &args,
            &node.id,
            line,
            file_path,
            language,
        ));
        result.nodes.push(node);
    }

    // The chained form, `router.route('/:id').get(getProduct).put(protect,
    // updateProduct)`: one route per method, at the line of its `.method(`.
    static CHAIN: OnceLock<Regex> = OnceLock::new();
    let chain = CHAIN.get_or_init(|| {
        js_regex(r#"(?-u:\b)(?:app|router){S}*\.{S}*route{S}*\({S}*['"]([^'"]+)['"]{S}*\)"#)
    });
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| js_regex(r"^{S}*\.{S}*(get|post|put|patch|delete|all){S}*\("));
    for captures in chain.captures_iter(&text.safe) {
        let found = captures.get(1).expect("route path");
        let path = text.upstream(found.start(), found.end());
        let mut at = captures.get(0).expect("whole match").end();
        // Upstream reads each link in the 64 code units after `at`.
        while let Some(step) = link.captures(&text.safe[at..]) {
            let step_end = at + step.get(0).expect("link").end();
            if text.units(at, step_end) > 64 {
                break;
            }
            let open = step_end - 1;
            let Some((close, structure)) = scan_call(&text, open) else {
                break;
            };
            let line = text.line_at(open);
            let node = route(line, &step[1], &path, text.units(at, step_end));
            let args = &structure[1..structure.len() - 1];
            result.references.extend(handler_refs(
                &text,
                open + 1,
                args,
                &node.id,
                line,
                file_path,
                language,
            ));
            result.nodes.push(node);
            at = close + 1;
        }
    }
    result
}

/// What ends a regex literal: its flags, then a member access, a separator
/// or a closing bracket, an operator, or the end of the line (upstream
/// `REGEX_END_AFTER`).
fn regex_end_after() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        js_regex(
            r"^[dgimsuyv]*[ \t]*(?:\.{S}*(?:test|exec|source|flags|global|lastIndex)(?-u:\b)|[,;)\]}:?]|&&|\|\||\r?\n|$)",
        )
    })
}

/// What a regex literal may follow (upstream `REGEX_START_BEFORE`).
fn regex_start_before() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        js_regex(
            r"(?:^|[=(:,)!&|?;{}\[\]+*%~^<>-]|(?-u:\b)(?:return|throw|case|yield|await|else|do|typeof|void|delete|new|in|of|instanceof)){S}*$",
        )
    })
}

/// The call whose `(` is at `open`, scanned from there to its matching `)`
/// (upstream `scanCall`): its close, and its text with string contents and
/// regex-literal bodies blanked, offsets kept, so `/[^]]/`, `/}/`,
/// `'a, b)'` or a backtick in a regex cannot end an argument or a body
/// early. Scanned from the call's own paren, so nothing earlier in the file
/// can throw it off.
fn scan_call(text: &Text, open: usize) -> Option<(usize, String)> {
    let safe = text.safe.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut depth = 0i64;
    let mut i = open;
    while i < safe.len() {
        let c = safe[i];
        if (c == b'"' || c == b'\'') && !closes_on_line(safe, i) {
            // JSX text (`<p>Don't</p>`), not a string: an apostrophe with no
            // closing quote on its line must not blank the `)` after it.
            out.push(c);
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' || c == b'`' {
            out.push(c);
            i += 1;
            while i < safe.len() && safe[i] != c {
                if c != b'`' && safe[i] == b'\n' {
                    break;
                }
                if safe[i] == b'\\' && i + 1 < safe.len() {
                    out.push(b' ');
                    i += 1;
                }
                out.push(if safe[i] == b'\n' { b'\n' } else { b' ' });
                i += 1;
            }
            if i < safe.len() {
                out.push(safe[i]);
            }
            i += 1;
            continue;
        }
        let next = safe.get(i + 1).copied();
        if c == b'/'
            && next != Some(b'/')
            && next != Some(b'*')
            && let Some(end) = regex_literal_end_at(text, i)
        {
            out.push(b'/');
            out.extend(std::iter::repeat_n(b' ', end - i - 1));
            out.push(b'/');
            i = end + 1;
            continue;
        }
        out.push(c);
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                let structure = String::from_utf8(out).expect("blanking whole characters");
                return Some((i, structure));
            }
        }
        i += 1;
    }
    None
}

/// Whether the quote at `i` has an unescaped closing quote on its line
/// (upstream `closesOnLine`).
fn closes_on_line(s: &[u8], i: usize) -> bool {
    let quote = s[i];
    let mut k = i + 1;
    while k < s.len() && s[k] != b'\n' {
        if s[k] == b'\\' {
            k += 1;
        } else if s[k] == quote {
            return true;
        }
        k += 1;
    }
    false
}

/// The closing `/` of the regex literal the `/` at `i` opens, or `None`
/// when it divides (upstream `regexLiteralEndAt`). A regex opens after
/// punctuation or a keyword but not after `)`, `]`, `}`, a postfix `!`
/// (`done! / total`), `++` or `--`; it closes on its line; and what follows
/// the close must end a regex, since treating a division as one would hide
/// a bracket.
fn regex_literal_end_at(text: &Text, i: usize) -> Option<usize> {
    let safe = text.safe.as_bytes();
    let mut p = i as isize - 1;
    while p >= 0 && (safe[p as usize] == b' ' || safe[p as usize] == b'\t') {
        p -= 1;
    }
    if p >= 0 {
        let before = safe[p as usize];
        if matches!(before, b')' | b']' | b'}') {
            return None;
        }
        if p >= 1 {
            let earlier = safe[p as usize - 1];
            if before == b'!'
                && (earlier.is_ascii_alphanumeric() || matches!(earlier, b'_' | b'$' | b')' | b']'))
            {
                return None;
            }
            if (before == b'+' || before == b'-') && earlier == before {
                return None;
            }
        }
    }
    if !regex_start_before().is_match(&text.units_before(0, i, 32)) {
        return None;
    }
    let mut end = i + 1;
    let mut in_class = false;
    while end < safe.len() && safe[end] != b'\n' {
        match safe[end] {
            b'\\' => end += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => break,
            _ => {}
        }
        end += 1;
    }
    if end >= safe.len() || safe[end] != b'/' {
        return None;
    }
    regex_end_after()
        .is_match(&text.units_after(end + 1, 23))
        .then_some(end)
}

/// The index of the delimiter matching the one at `open`, skipping string
/// and template literals (upstream `matchDelim`).
fn match_delim(s: &str, open: usize, oc: u8, cc: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i64;
    let mut i = open;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            i += 1;
            while i < bytes.len() && bytes[i] != ch {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if ch == oc {
            depth += 1;
        } else if ch == cc {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Where a `function`'s body closes: the `}` matching the first `{` after its
/// parameter list (upstream `functionBodyEnd`).
fn function_body_end(text: &str, fn_at: usize) -> Option<usize> {
    let params_open = fn_at + text[fn_at..].find('(')?;
    let params_close = match_delim(text, params_open, b'(', b')')?;
    let brace_at = params_close + text[params_close..].find('{')?;
    match_delim(text, brace_at, b'{', b'}')
}

/// One top-level argument: its text and its offset into the argument list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arg<'a> {
    text: &'a str,
    start: usize,
}

/// `args` split at its top-level commas (upstream `topLevelArgs`). A
/// type-argument list, `asyncHandler<Req, Res>(`, splits nothing.
fn top_level_args(args: &str) -> Vec<Arg<'_>> {
    static GENERIC: OnceLock<Regex> = OnceLock::new();
    let generic = GENERIC.get_or_init(|| js_regex(r"^<[^()<>]*(?:<[^()<>]*>[^()<>]*)*>{S}*\("));
    let bytes = args.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            i += 1;
            while i < bytes.len() && bytes[i] != ch {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if ch == b'<'
            && i > 0
            && (bytes[i - 1].is_ascii_alphanumeric() || matches!(bytes[i - 1], b'_' | b'$'))
            && let Some(found) = generic.find(&args[i..])
        {
            // Land on the `(`, which the next step counts.
            i += found.end() - 1;
            continue;
        }
        match ch {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(Arg {
                    text: &args[start..i],
                    start,
                });
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(Arg {
        text: &args[start..],
        start,
    });
    out
}

/// The last identifier of an expression, whitespace and a trailing `()`
/// aside (upstream `extractTailIdent`).
fn extract_tail_ident(expr: &str) -> Option<String> {
    static TAIL: OnceLock<Regex> = OnceLock::new();
    let tail =
        TAIL.get_or_init(|| Regex::new(r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$").expect("tail ident"));
    let cleaned: String = expr.chars().filter(|&c| !is_js_space(c)).collect();
    let cleaned = cleaned.strip_suffix("()").unwrap_or(&cleaned);
    tail.captures(cleaned)
        .map(|captures| captures[1].to_string())
}

/// What a route call's handler contributes (upstream `handlerRefs`). The
/// handler is the LAST argument; the ones before it are middleware, and an
/// arrow inside one of those says nothing about the handler. A handler that
/// is a function — an arrow, a `function` expression, or one wrapped in a
/// call — is anonymous, so its body's calls are the route's `calls` and its
/// replies are references at their own place; anything else is a named
/// handler, a `references` reference to its last name.
///
/// `args_start` is where `args` begins in the file.
fn handler_refs(
    text: &Text,
    args_start: usize,
    args: &str,
    from: &str,
    line: i64,
    file_path: &str,
    language: Language,
) -> Vec<RefView> {
    let reference = |name: String, kind: EdgeKind, line: i64, column: i64| RefView {
        row_id: None,
        from_node_id: from.to_string(),
        reference_name: name,
        reference_kind: kind,
        line,
        column,
        file_path: file_path.to_string(),
        language,
        is_function_ref: false,
        reference_subkind: None,
    };

    // A trailing options object is not the handler; a trailing array of
    // handlers is read as its last element.
    let mut parts: Vec<Arg<'_>> = top_level_args(args)
        .into_iter()
        .filter(|arg| !js_trim(arg.text).is_empty())
        .collect();
    while parts.len() > 1
        && parts
            .last()
            .is_some_and(|arg| arg.text.trim_start_matches(is_js_space).starts_with('{'))
    {
        parts.pop();
    }
    if let Some(last) = parts.last().cloned() {
        let array_at = last.text.find(|c: char| !is_js_space(c));
        if let Some(array_at) = array_at.filter(|&at| last.text.as_bytes()[at] == b'[') {
            let inner = match_delim(last.text, array_at, b'[', b']')
                .filter(|&close| close > array_at)
                .and_then(|close| {
                    top_level_args(&last.text[array_at + 1..close])
                        .into_iter()
                        .rfind(|arg| !js_trim(arg.text).is_empty())
                });
            parts.pop();
            if let Some(inner) = inner {
                parts.push(Arg {
                    text: inner.text,
                    start: last.start + array_at + 1 + inner.start,
                });
            }
        }
    }
    let Some(handler) = parts.last() else {
        return Vec::new();
    };
    let handler_text = handler.text;

    // A `function` is the handler's own only when it opens the argument,
    // directly or inside wrapper calls; then its body starts after its
    // parameter list, not after an arrow inside it.
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let opening = OPENING.get_or_init(|| {
        js_regex(
            r"^{S}*(?:[A-Za-z_$][A-Za-z0-9_$.]*{S}*\({S}*)*(?:async{S}+)?function(?-u:\b)[^(]*\(",
        )
    });
    let mut arrow_at = handler_text.find("=>");
    let opened = opening.find(handler_text);
    let mut fn_at = opened.and_then(|m| m.as_str().rfind("function"));
    if let (Some(at), Some(opened)) = (fn_at, opened) {
        // Inside a wrapper, a `function` followed by an arrow after its body
        // is a callback argument: the arrow is the handler.
        let body_end = function_body_end(handler_text, at);
        let inside_wrapper = opened.as_str()[..at].contains('(');
        let later_arrow = body_end
            .filter(|_| inside_wrapper)
            .and_then(|end| handler_text[end..].find("=>").map(|offset| end + offset));
        if let Some(later) = later_arrow {
            arrow_at = Some(later);
            fn_at = None;
        } else if inside_wrapper
            && body_end.is_some_and(|end| {
                !handler_text[end + 1..]
                    .chars()
                    .all(|c| is_js_space(c) || c == ')')
            })
        {
            // A callback followed by more arguments is not the handler.
            arrow_at = None;
            fn_at = None;
        } else {
            arrow_at = None;
        }
    }

    if arrow_at.is_none() && fn_at.is_none() {
        let after_comma = handler_text.rfind(',').map_or(0, |comma| comma + 1);
        let name = extract_tail_ident(js_trim(handler_text))
            .or_else(|| extract_tail_ident(js_trim(&handler_text[after_comma..])));
        return name
            .map(|name| vec![reference(name, EdgeKind::References, line, 0)])
            .unwrap_or_default();
    }

    let body_from = match (arrow_at, fn_at) {
        (Some(arrow), _) => arrow + 2,
        (None, Some(at)) => {
            let params_open = at + handler_text[at..].find('(').expect("function params");
            match match_delim(handler_text, params_open, b'(', b')') {
                Some(close) if close > params_open => close + 1,
                _ => handler_text.len(),
            }
        }
        (None, None) => unreachable!("a named handler returned above"),
    };
    let after_params = &handler_text[body_from..];
    let mut body = after_params;
    let mut body_start = args_start + handler.start + body_from;
    if let Some(brace_at) = after_params.find('{')
        && js_trim(&after_params[..brace_at]).is_empty()
        && let Some(end) = match_delim(after_params, brace_at, b'{', b'}').filter(|&e| e > brace_at)
    {
        body = &after_params[brace_at + 1..end];
        body_start += brace_at + 1;
    }

    // Positions came from the blanked structure; the calls are read from the
    // stripped source at the same offsets, so a call inside a template
    // interpolation counts.
    let body_end = body_start + body.len();
    let mut refs: Vec<RefView> = handler_call_names(text, body_start, body_end)
        .into_iter()
        .map(|name| reference(name, EdgeKind::Calls, line, 0))
        .collect();
    for (name, at) in reply_calls(&text.safe[body_start..body_end]) {
        let at = body_start + at;
        let line_start = text.safe[..at].rfind('\n').map_or(0, |newline| newline + 1);
        refs.push(reference(
            name,
            EdgeKind::Calls,
            text.line_at(at),
            text.units(line_start, at),
        ));
    }
    refs
}

/// The calls an inline handler's body makes, each once, framework noise
/// aside (upstream `handlerCallNames`). A member call keeps its receiver
/// (`userService.lookup`, `c.text`) so it resolves as one; a member of an
/// expression (`a.b().c(`) names nothing this can follow.
fn handler_call_names(text: &Text, start: usize, end: usize) -> Vec<String> {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let call = CALL.get_or_init(|| {
        js_regex(r"((?:[A-Za-z_$][A-Za-z0-9_$]*{S}*\??\.{S}*)*)([A-Za-z_$][A-Za-z0-9_$]*){S}*\(")
    });
    static DECLARED: OnceLock<Regex> = OnceLock::new();
    let declared = DECLARED.get_or_init(|| js_regex(r"(?-u:\b)function{S}*\*?{S}*$"));
    let body = &text.safe[start..end];
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for captures in call.captures_iter(body) {
        let at = captures.get(0).expect("whole match").start();
        let method = &captures[2];
        let receiver: String = captures[1]
            .chars()
            .filter(|&c| !is_js_space(c) && c != '?')
            .collect();
        let receiver = receiver.strip_suffix('.').unwrap_or(&receiver);
        if receiver.is_empty() {
            if body[..at].trim_end_matches(is_js_space).ends_with('.') {
                continue;
            }
            // A keyword is only a keyword when it isn't a member: `if (` is
            // no call, `userService.delete(` is. `function helper(`
            // declares helper.
            if JS_KEYWORDS.contains(&method) {
                continue;
            }
            if declared.is_match(&text.units_before(start, start + at, 12)) {
                continue;
            }
        }
        let name = if receiver.is_empty() {
            method.to_string()
        } else {
            format!("{receiver}.{method}")
        };
        if seen.contains(&name) || RESERVED_CALLS.contains(&method) {
            continue;
        }
        seen.insert(name.clone());
        out.push(name);
    }
    out
}

/// The replies an inline handler makes — `res.status(404).json({…})`,
/// `res.json(user)`, `reply.send(…)` — with their offsets in `body`
/// (upstream `replyRefs`): references the Steps view reads at their own
/// line and column.
fn reply_calls(body: &str) -> Vec<(String, usize)> {
    static REPLY: OnceLock<Regex> = OnceLock::new();
    let reply = REPLY.get_or_init(|| {
        js_regex(
            r"(?-u:\b)(res|response|reply|rep|ctx){S}*\.{S}*(?:[A-Za-z_$][A-Za-z0-9_$]*{S}*\([^()]*\){S}*\.{S}*)*([A-Za-z_$][A-Za-z0-9_$]*){S}*\(",
        )
    });
    reply
        .captures_iter(body)
        .map(|captures| {
            (
                format!("{}.{}", &captures[1], &captures[2]),
                captures.get(0).expect("whole match").start(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Mounts
// ---------------------------------------------------------------------------

/// One `X.use('/prefix', …` mount: the call's `(` and its prefix.
struct Mount {
    open: usize,
    prefix: String,
}

/// Every `X.use('/prefix', …` in the stripped text (upstream's
/// `/\b[A-Za-z_$][\w$]*\.use\s*\(\s*(['"`])(\/[^'"`]*)\1\s*,/g`). The
/// `regex` crate has no backreference: the pattern takes any closing quote,
/// and a match whose quotes differ is no match there. Nothing between the
/// quotes is a quote, so a start has one match at most, and the search goes
/// on from the next character, as JavaScript's does after a failed start.
fn find_mounts(text: &Text) -> Vec<Mount> {
    static MOUNT: OnceLock<Regex> = OnceLock::new();
    let mount = MOUNT.get_or_init(|| {
        js_regex(r#"(?-u:\b)[A-Za-z_$][A-Za-z0-9_$]*\.use{S}*\({S}*(['"`])(/[^'"`]*)(['"`]){S}*,"#)
    });
    let safe = text.safe.as_str();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(captures) = mount.captures_at(safe, from) {
        let whole = captures.get(0).expect("whole match");
        if captures[1] != captures[3] {
            from = whole.start() + 1;
            continue;
        }
        let prefix = captures.get(2).expect("mount prefix");
        out.push(Mount {
            open: whole.start() + safe[whole.start()..].find('(').expect("mount paren"),
            prefix: text.upstream(prefix.start(), prefix.end()),
        });
        from = whole.end();
    }
    out
}

/// The file a mount's last argument names (upstream `mountTarget`): an
/// inline `require('./x')`, an identifier imported from a project file, or
/// one bound to `require('./x')` in the same file. `x.default`, `x.router`
/// and `x.routes` count as `x`.
fn mount_target(
    expr: &str,
    safe: &str,
    file: &str,
    language: Language,
    context: &dyn ResolutionContext,
) -> Option<String> {
    static INLINE: OnceLock<Regex> = OnceLock::new();
    let inline = INLINE.get_or_init(|| {
        js_regex(r#"^require{S}*\({S}*(['"])([^'"]+)(['"]){S}*\)(?:\.[A-Za-z0-9_]+)?$"#)
    });
    // Anchored at both ends: differing quotes are no match at all.
    if let Some(captures) = inline.captures(expr)
        && captures[1] == captures[3]
    {
        return resolve_import_path(&captures[2], file, language, context);
    }
    static IDENT: OnceLock<Regex> = OnceLock::new();
    let ident = IDENT.get_or_init(|| {
        Regex::new(r"^([A-Za-z_$][A-Za-z0-9_$]*)(?:\.(?:default|router|routes))?$")
            .expect("mount ident")
    });
    let name = ident.captures(expr)?.get(1)?.as_str().to_string();
    if let Some(mapping) = context
        .get_import_mappings(file, language)
        .into_iter()
        .find(|mapping| mapping.local_name == name)
    {
        return resolve_import_path(&mapping.source, file, language, context);
    }
    let required = js_regex(&format!(
        r#"(?-u:\b)(?:const|let|var){{S}}+{}{{S}}*={{S}}*require{{S}}*\({{S}}*(['"])([^'"]+)(['"]){{S}}*\)"#,
        regex::escape(&name)
    ));
    let mut from = 0;
    while let Some(captures) = required.captures_at(safe, from) {
        let whole = captures.get(0).expect("whole match");
        if captures[1] == captures[3] {
            return resolve_import_path(&captures[2], file, language, context);
        }
        from = whole.start() + 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Name patterns
// ---------------------------------------------------------------------------

/// Whether a name looks like middleware (upstream `isMiddlewareName`; the
/// patterns' `i` is ASCII-only, as JavaScript's without `u`).
fn is_middleware_name(name: &str) -> bool {
    const EXACT: [&str; 8] = [
        "auth",
        "authenticate",
        "authorization",
        "cors",
        "helmet",
        "logger",
        "errorHandler",
        "notFound",
    ];
    const PREFIXES: [&str; 3] = ["validate", "sanitize", "rateLimit"];
    EXACT.iter().any(|exact| name.eq_ignore_ascii_case(exact))
        || PREFIXES
            .iter()
            .any(|prefix| starts_with_ignore_ascii_case(name, prefix))
        || ends_with_ignore_ascii_case(name, "Middleware")
}

fn starts_with_ignore_ascii_case(name: &str, prefix: &str) -> bool {
    name.len() >= prefix.len()
        && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn ends_with_ignore_ascii_case(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len()
        && name.as_bytes()[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}

/// Node kinds a middleware can be declared as (upstream `MIDDLEWARE_KINDS`):
/// never the file's import of a package (`import cors from 'cors'`, #2244).
fn is_middleware_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Variable
            | NodeKind::Constant
            | NodeKind::Class
    )
}

/// Upstream `resolveMiddleware`'s first tier, resolved only through its sole
/// candidate: the name's own declarations. Upstream takes the calling file's
/// first, and another file's never passes the same-file rule, so only a sole
/// one in the calling file counts. Upstream's two lowercase comparisons hold
/// for every node found by the name.
///
/// Upstream's second tier, the name without a `Middleware` suffix when no
/// declaration carries the full name, is not ported. A name only reaches it
/// when some node that declares nothing carries it — an import
/// (`import { corsMiddleware } from 'cors-kit'`), a parameter — and with the
/// same-file rule it then links that import's or parameter's call to the
/// file's own `cors`, which it is not. Its outcome also reads whether any
/// file declares the full name, which the edge to `cors` gives a sync no way
/// to re-read.
fn resolve_middleware(
    name: &str,
    context: &dyn ResolutionContext,
    from_file: &str,
) -> Option<String> {
    sole(
        context
            .get_nodes_by_name_shared(name)
            .iter()
            .filter(|node| is_middleware_kind(node.kind) && node.file_path == from_file),
    )
    .map(|node| node.id.clone())
}

/// Upstream `resolveControllerMethod`'s first tier, resolved only through its
/// sole candidate: the methods in files whose path names the controller.
///
/// Upstream's second tier, the methods of that name in the files of nodes
/// named `XController`, is not ported: with the same-file rule it only ever
/// finds a method of an `XController` object the calling file declares,
/// which the instance-method strategy links to the same method already, and
/// its outcome reads which files declare `XController`, which the edge to
/// the method gives a sync no way to re-read.
fn resolve_controller_method(
    controller: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let controller_lower = controller.to_lowercase();
    sole(
        context
            .get_nodes_by_name_shared(method)
            .iter()
            .filter(|node| {
                is_callable(node.kind) && node.file_path.to_lowercase().contains(&controller_lower)
            }),
    )
    .map(|node| node.id.clone())
}

/// Upstream `resolveServiceMethod`, resolved only through its sole
/// candidate: the method in a file whose path names the service without
/// its suffix.
fn resolve_service_method(
    service: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut stripped = service;
    for suffix in ["Service", "Helper", "Utils", "Util"] {
        if ends_with_ignore_ascii_case(stripped, suffix) {
            stripped = &stripped[..stripped.len() - suffix.len()];
            break;
        }
    }
    let stripped = stripped.to_lowercase();
    sole(
        context
            .get_nodes_by_name_shared(method)
            .iter()
            .filter(|node| {
                is_callable(node.kind) && node.file_path.to_lowercase().contains(&stripped)
            }),
    )
    .map(|node| node.id.clone())
}

fn is_callable(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::Method | NodeKind::Function)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::{RouteCtx, call_in};
    use crate::types::ImportMapping;

    fn names(file: &str, src: &str) -> Vec<String> {
        extract_routes(file, src)
            .nodes
            .into_iter()
            .map(|node| node.name)
            .collect()
    }

    fn refs_in(file: &str, src: &str) -> Vec<String> {
        extract_routes(file, src)
            .references
            .iter()
            .map(|r| format!("{}:{}", r.reference_kind.as_str(), r.reference_name))
            .collect()
    }

    /// Upstream's `refsOf`: a `routes.ts` file's references as `kind:name`.
    fn refs_of(src: &str) -> Vec<String> {
        refs_in("routes.ts", src)
    }

    #[track_caller]
    fn contains(refs: &[String], wanted: &str) {
        assert!(refs.iter().any(|r| r == wanted), "{wanted} in {refs:?}");
    }

    #[track_caller]
    fn lacks(refs: &[String], unwanted: &str) {
        assert!(
            !refs.iter().any(|r| r == unwanted),
            "{unwanted} not in {refs:?}"
        );
    }

    // -- upstream `frameworks.test.ts` `expressResolver.extract` ---------------

    #[test]
    fn extracts_route_with_inline_handler_reference() {
        let result = extract_routes("routes.ts", "app.get('/users', listUsers);\n");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(result.references[0].reference_name, "listUsers");
    }

    #[test]
    fn extracts_route_with_router_post_and_middleware_chain() {
        let result = extract_routes("items.ts", "router.post('/items', auth, createItem);\n");
        assert_eq!(result.nodes[0].name, "POST /items");
        // The LAST handler: middleware first, handler last.
        assert_eq!(result.references[0].reference_name, "createItem");
    }

    #[test]
    fn extracts_route_with_controller_method_reference() {
        let result = extract_routes("routes.ts", "app.get('/x', userController.list);\n");
        assert_eq!(result.references[0].reference_name, "list");
    }

    #[test]
    fn keeps_the_named_handler_when_a_middleware_argument_holds_an_arrow() {
        let refs = refs_of(
            "router.get('/users', rateLimit({ keyGenerator: (req) => lookup(req.ip) }), getUsers);\n",
        );
        contains(&refs, "references:getUsers");
        lacks(&refs, "calls:lookup");
    }

    #[test]
    fn keeps_the_named_handler_after_an_inline_arrow_middleware() {
        let refs = refs_of("router.get('/x', (req, res, next) => audit(req, next), getUsers);\n");
        contains(&refs, "references:getUsers");
        lacks(&refs, "calls:audit");
    }

    #[test]
    fn still_reads_an_inline_handler_wrapped_or_after_named_middleware() {
        contains(
            &refs_of(
                "router.post('/x', asyncHandler(async (req, res) => { createItem(req.body); }));\n",
            ),
            "calls:createItem",
        );
        let after_auth =
            refs_of("router.post('/x', auth, async (req, res) => { createItem(req.body); });\n");
        contains(&after_auth, "calls:createItem");
        lacks(&after_auth, "references:auth");
    }

    #[test]
    fn reads_a_function_expression_handler() {
        contains(
            &refs_of("router.get('/x', function (req, res) { listUsers(); });\n"),
            "calls:listUsers",
        );
    }

    #[test]
    fn a_function_handler_whose_body_holds_an_arrow_keeps_every_call() {
        let plain = refs_of(
            "router.get('/x', function (req, res) { const u = findUser(req.id); res.json(u.items.map((i) => shape(i))); });\n",
        );
        contains(&plain, "calls:findUser");
        contains(&plain, "calls:shape");
        let chained = refs_of(
            "router.route('/x').get(function (req, res) { const a = load(); a.forEach((x) => shape(x)); });\n",
        );
        contains(&chained, "calls:load");
        contains(&chained, "calls:shape");
    }

    #[test]
    fn a_function_elsewhere_in_the_argument_is_not_the_handler() {
        contains(
            &refs_of("router.get('/x', (req, res, cb = function () {}) => { svc(); });\n"),
            "calls:svc",
        );
        lacks(
            &refs_of(
                "router.get('/x', wrap(ctrl.list, { onError: function (e) { report(e); } }));\n",
            ),
            "calls:report",
        );
        contains(
            &refs_of("router.get('/x', asyncHandler(function (req, res) { svc(); }));\n"),
            "calls:svc",
        );
        // After a wrapper's other argument a `function` opens no handler:
        // the argument is a named handler, and it names nothing.
        assert!(
            refs_of(
                "router.get('/x', withTimeout(5000, function (req, res) { res.json(list()); }));\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_function_callback_before_the_handler_inside_a_wrapper_is_not_the_handler() {
        let refs = refs_of(
            "router.get('/x', withErrors(function onErr(e) { report(e); }, async (req, res) => { svc(); }));\n",
        );
        contains(&refs, "calls:svc");
        lacks(&refs, "calls:report");
    }

    #[test]
    fn a_regex_literal_in_an_argument_does_not_throw_off_the_split() {
        contains(
            &refs_of("router.get('/x', body('tags').matches(/^[^\\]]+$/), getUsers);\n"),
            "references:getUsers",
        );
        contains(
            &refs_of("router.get('/x', check('q').matches(/\\[/), getUsers);\n"),
            "references:getUsers",
        );
        let inline = refs_of(
            "router.get('/x', async (req, res) => { const re = /[^]]/, out = compute(req); save(out); });\n",
        );
        contains(&inline, "calls:compute");
        contains(&inline, "calls:save");
    }

    #[test]
    fn keywords_in_a_handler_body_are_not_calls() {
        let refs = refs_of(
            "router.get('/x', function (req, res) { if (ok(req)) { for (const a of b) run(a); } items.forEach(function (i) { shape(i); }); });\n",
        );
        for wanted in ["calls:ok", "calls:run", "calls:shape"] {
            contains(&refs, wanted);
        }
        for keyword in [
            "if", "for", "function", "while", "switch", "catch", "return",
        ] {
            lacks(&refs, &format!("calls:{keyword}"));
        }
    }

    #[test]
    fn a_jsx_closing_tag_is_not_a_regex() {
        let refs = refs_in(
            "server.tsx",
            "app.get('/a', (req, res) => { res.send(renderToString(<div>{a()}</div>)); });\napp.get('/b', (req, res) => { res.send(renderPage(<>{b()}</>)); });\n",
        );
        for wanted in [
            "calls:renderToString",
            "calls:a",
            "calls:renderPage",
            "calls:b",
        ] {
            contains(&refs, wanted);
        }
    }

    #[test]
    fn a_self_closing_jsx_tag_after_an_attribute_and_division_after_a_brace_are_not_regexes() {
        let tsx = refs_in(
            "server.tsx",
            "app.get('*', (req, res) => {\n  const html = renderToString(<App store={store} />);\n  sendPage(res, html);\n});\n",
        );
        contains(&tsx, "calls:renderToString");
        contains(&tsx, "calls:sendPage");
        contains(
            &refs_of("router.get('/x', (req, res) => { const v = { a: 1 }\n/ 2; svc(v); });\n"),
            "calls:svc",
        );
    }

    #[test]
    fn division_after_a_bang_or_increment_is_not_a_regex() {
        contains(
            &refs_of("router.get('/x', rateLimit({ max: cfg.max! / 2 }), getUsers);\n"),
            "references:getUsers",
        );
        contains(
            &refs_of("router.get('/x', limiter(hits++ / 2), getUsers);\n"),
            "references:getUsers",
        );
    }

    #[test]
    fn a_slash_after_an_operand_divides_where_a_regex_could_also_close() {
        // `)`, `]`, `}`, `++` and `--` end an operand, so the `/` after each
        // divides, though the punctuation before it may open a regex and
        // `/\)/` closes as one. Read as divisions, the `\)` closes `f(` and
        // the next `)` the route call, which leaves `getUsers` outside.
        // After a `,` the slashes are a regex and `getUsers` the handler.
        for operand in ["(a)", "a[0]", "{}", "i++", "i--"] {
            let src = format!("router.get('/x', f({operand} /\\)/), getUsers);\n");
            lacks(&refs_of(&src), "references:getUsers");
        }
        contains(
            &refs_of("router.get('/x', f(a, /\\)/), getUsers);\n"),
            "references:getUsers",
        );
    }

    #[test]
    fn an_async_function_handler_with_a_brace_regex_and_an_arrow_keeps_every_call() {
        let refs = refs_of(
            "router.route('/x').get(async function (req, res) { const s = t.replace(/}/g, ''); load(); items.map((i) => shape(i)); });\n",
        );
        contains(&refs, "calls:load");
        contains(&refs, "calls:shape");
    }

    #[test]
    fn async_and_a_nested_function_declaration_name_are_not_calls() {
        let refs = refs_of(
            "router.get('/x', function (req, res) { function helper(a) { return a; } Promise.all(items.map(async (i) => save(i))); });\n",
        );
        contains(&refs, "calls:save");
        lacks(&refs, "calls:async");
        lacks(&refs, "calls:helper");
    }

    #[test]
    fn regex_literals_in_handlers_do_not_cost_the_route_its_edges() {
        contains(
            &refs_of(
                "router.get('/u/:id', async (req, res) => { if (!/^\\d+/.test(req.params.id)) return res.sendStatus(400); const user = await userService.findById(req.params.id); res.json(user); });\n",
            ),
            "calls:userService.findById",
        );
        let src = "router.route('/:id').get((req, res) => { if (!/^\\w+/.test(req.params.id)) return bad(res); load(req); }).put(protect, updateThing);\n";
        assert_eq!(names("routes.ts", src), ["GET /:id", "PUT /:id"]);
        let chained = refs_of(src);
        contains(&chained, "calls:load");
        contains(&chained, "references:updateThing");
        contains(
            &refs_of(
                "app.get('/echo', (req, res) => { res.send(String(req.query.q).replace(/</g, '&lt;').replace(/>/g, '&gt;')); audit(req); });\n",
            ),
            "calls:audit",
        );
    }

    #[test]
    fn a_call_inside_a_template_interpolation_in_the_body_still_counts() {
        contains(
            &refs_of(
                "router.get('/x', (req, res) => { res.send(`<p>${fmtUser(req.user)}</p>`); });\n",
            ),
            "calls:fmtUser",
        );
    }

    #[test]
    fn a_division_that_looks_like_a_regex_opener_does_not_cost_the_route() {
        contains(
            &refs_of(
                "router.get('/stats', async (req, res) => { const pct = Math.round((a - b) / total * 100) / 100; res.json({ pct: summarize(pct) }); });\n",
            ),
            "calls:summarize",
        );
        let src = "router.route('/s').get((req, res) => { const m = Math.floor((t1 - t0) / 1000) / 60; res.json(fmt(m)); }).post(protect, createS);\n";
        assert_eq!(names("routes.ts", src), ["GET /s", "POST /s"]);
        let chained = refs_of(src);
        contains(&chained, "calls:fmt");
        contains(&chained, "references:createS");
        let tsx = refs_in(
            "server.tsx",
            "app.get('/list', (req, res) => { res.send(renderToString(<ul>{items.map((x) => (<li key={x}>{x}</li>))}</ul>)); audit(req); });\n",
        );
        contains(&tsx, "calls:renderToString");
        contains(&tsx, "calls:audit");
    }

    #[test]
    fn a_division_after_a_paren_with_a_slash_in_a_later_string_does_not_cost_the_route() {
        let refs = refs_of(
            "router.get('/s', (req, res) => { res.json({ pct: Math.round(s.done) / s.total, next: '/stats/2' }); audit(req); });\n",
        );
        contains(&refs, "calls:Math.round");
        contains(&refs, "calls:audit");
        contains(
            &refs_of(
                "router.get('/p', (req, res) => { const n = Math.ceil(count(req)) / size; res.redirect('/p/' + n); after(req); });\n",
            ),
            "calls:after",
        );
        contains(
            &refs_of("router.get('/c', cfg(f(a) / 2, 'x/y'), handler);\n"),
            "references:handler",
        );
    }

    #[test]
    fn a_callback_inside_middleware_or_a_middleware_that_calls_next_is_not_the_handler() {
        contains(
            &refs_of(
                "router.get('/admin', authorize(function (user, req) { return user.isAdmin; }), adminCtrl.index);\n",
            ),
            "references:index",
        );
        contains(
            &refs_of(
                "router.get('/referrals', function (req, res, next) { check(); next(); }, getReferrals);\n",
            ),
            "references:getReferrals",
        );
        contains(
            &refs_of(
                "router.get('/cfg', (req, res, next) => { load(); next(); }, overrideConfig);\n",
            ),
            "references:overrideConfig",
        );
    }

    #[test]
    fn the_last_argument_decides() {
        for (src, wanted) in [
            (
                "router.get('/admin', authorize(function (user, req) { return user.isAdmin; }), adminCtrl.index);\n",
                "references:index",
            ),
            (
                "router.get('/u/:id', function (req, res, next) { User.findById(id, function (err, user) { req.user = user; next(err); }); }, userCtrl.show);\n",
                "references:show",
            ),
            (
                "router.get('/x', responseTime(function (req, res, time) { stats.timing(req.path, time); }), ctrl.list);\n",
                "references:list",
            ),
            (
                "router.get('/errors', function (req, res, next) { auth(req); next(); }, getErrors);\n",
                "references:getErrors",
            ),
            (
                "app.use('/api', function (req, res, next) { if (!ok) return res.sendStatus(403); authorize(req, next); }, apiRouter);\n",
                "references:apiRouter",
            ),
            (
                "router.post('/g', auth, asyncHandler<Req, Res>(async (req, res) => { doIt(); }));\n",
                "calls:doIt",
            ),
            (
                "router.post('/d', asyncHandler<Req>(async (req, res) => { saveFile(req.file); }));\n",
                "calls:saveFile",
            ),
        ] {
            contains(&refs_of(src), wanted);
        }
    }

    #[test]
    fn a_type_argument_list_splits_no_argument() {
        let texts = |args: &str| {
            top_level_args(args)
                .into_iter()
                .map(|arg| arg.text.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            texts("auth, asyncHandler<Req, Res>(h)"),
            ["auth", " asyncHandler<Req, Res>(h)"]
        );
        assert_eq!(
            texts("typed<Map<K, V>, Opts>(h), x"),
            ["typed<Map<K, V>, Opts>(h)", " x"]
        );
        // A comparison is no list: only a `<` right after a name, closed
        // before a `(`.
        assert_eq!(texts("a < b, c > (d)"), ["a < b", " c > (d)"]);
        // A tuple type in the list is no array of handlers.
        assert!(refs_of("router.get('/x', typed<Req, [Res]>(handle));\n").is_empty());
    }

    #[test]
    fn an_arrow_or_a_call_inside_a_string_is_no_handler() {
        // String contents are blanked before the handler is read: the
        // `=> run()` in a named handler's argument opens no inline body.
        assert!(refs_of("router.get('/x', sendText('usage: x => run()'));\n").is_empty());
    }

    #[test]
    fn jsx_apostrophes_a_trailing_options_object_and_a_callback_before_a_named_handler() {
        let tsx = refs_in(
            "server.tsx",
            "app.get('/x', (req, res) => {\n  res.send(render(<p>Don't</p>));\n  res.send(render(<p>won't</p>));\n  load();\n});\n",
        );
        contains(&tsx, "calls:load");
        contains(
            &refs_of(
                "router.get('/o', (req, res) => { svc.list(); res.json(1) }, { cache: true });\n",
            ),
            "calls:svc.list",
        );
        lacks(
            &refs_of(
                "router.get('/w', withErrors(function onErr(e) { report(e); }, ctrl.list));\n",
            ),
            "calls:report",
        );
        // An array of handlers: its last element is the handler.
        assert_eq!(
            refs_of("router.post('/x', auth, [validate, ctrl.create]);\n"),
            ["references:create"]
        );
    }

    #[test]
    fn a_regex_that_ends_its_line_without_a_semicolon_is_still_a_regex() {
        contains(
            &refs_of("router.get('/re', (req, res) => {\n  const re = /[}]/\n  run(re)\n})\n"),
            "calls:run",
        );
    }

    #[test]
    fn a_backtick_in_an_earlier_regex_does_not_cost_later_routes() {
        let refs = refs_of(
            "const esc = (s) => s.replace(/`/g, 'x');\nrouter.get('/d', async (req, res) => { await svc.doThing(); res.end(); });\nrouter.get('/e', ctrl.list);\n",
        );
        contains(&refs, "calls:svc.doThing");
        contains(&refs, "references:list");
    }

    #[test]
    fn a_division_after_a_non_null_bang_is_not_a_regex_even_with_a_path_string_later() {
        contains(
            &refs_of(
                "app.get('/x', (req, res) => {\n const r = done! / total!; res.redirect(r ? '/:id' : '/');\n svc.run(r);\n});\n",
            ),
            "calls:svc.run",
        );
        contains(
            &refs_of(
                "app.get('/y', (req, res) => {\n const r = sum! / count!; const parts = csv.split(/,/);\n svc.run(parts);\n});\n",
            ),
            "calls:svc.run",
        );
    }

    #[test]
    fn a_method_named_like_a_keyword_is_still_a_call() {
        let refs = refs_of(
            "router.delete('/users/:id', async (req, res) => { await userService.delete(req.params.id); cache .with(k); });\n",
        );
        contains(&refs, "calls:userService.delete");
        contains(&refs, "calls:cache.with");
        contains(
            &refs_of("router.get('/x', (req, res) => { return of(load()); });\n"),
            "calls:of",
        );
    }

    #[test]
    fn chained_routes_the_handler_decides_too() {
        let refs = refs_of(
            "router.route('/items').get(rateLimit({ keyGenerator: (r) => lookup(r.ip) }), getItem).post(async (req, res) => { createItem(); });\n",
        );
        contains(&refs, "references:getItem");
        lacks(&refs, "calls:lookup");
        contains(&refs, "calls:createItem");
    }

    /// Upstream `frameworks.test.ts`, "express: skips // and /* */ commented routes".
    #[test]
    fn commented_routes_are_skipped() {
        let src = "\n// app.get('/fake', fakeHandler);\n/* router.post('/also-fake', otherHandler); */\napp.get('/real', realHandler);\n";
        let result = extract_routes("routes.ts", src);
        assert_eq!(
            result
                .nodes
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            ["GET /real"]
        );
        assert_eq!(
            result
                .references
                .iter()
                .map(|r| r.reference_name.as_str())
                .collect::<Vec<_>>(),
            ["realHandler"]
        );
    }

    // -- resolution: the three patterns, own file only, sole candidates -------

    fn decl(file: &str, kind: NodeKind, name: &str, line: i64) -> Node {
        framework_node(
            format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name.to_string(),
            format!("{file}::{name}"),
            file.to_string(),
            line,
            line,
            0,
            0,
            express_language(file),
            false,
        )
    }

    fn ctx_of(nodes: Vec<Node>) -> RouteCtx {
        nodes.into_iter().fold(RouteCtx::default(), |ctx, node| {
            let file = node.file_path.clone();
            ctx.routes(&file, vec![node])
        })
    }

    fn target(ctx: &RouteCtx, file: &str, name: &str) -> Option<(String, f64)> {
        ExpressResolver
            .resolve(&call_in(file, name, 3, 0), ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    fn id(node: &Node) -> String {
        node.id.clone()
    }

    #[test]
    fn a_middleware_resolves_only_to_its_own_files_declaration() {
        let own = decl("routes.ts", NodeKind::Function, "authenticate", 2);
        assert_eq!(
            target(&ctx_of(vec![own.clone()]), "routes.ts", "authenticate"),
            Some((id(&own), 0.8))
        );
        // #2243: another file's middleware is the import resolver's.
        let other = decl("lib/auth.ts", NodeKind::Function, "authenticate", 2);
        assert_eq!(
            target(&ctx_of(vec![other]), "routes.ts", "authenticate"),
            None
        );
        // The name patterns are ASCII-case-insensitive.
        let upper = decl("routes.ts", NodeKind::Function, "RateLimitByIp", 2);
        assert_eq!(
            target(&ctx_of(vec![upper.clone()]), "routes.ts", "RateLimitByIp"),
            Some((id(&upper), 0.8))
        );
        assert_eq!(
            target(
                &ctx_of(vec![decl("routes.ts", NodeKind::Function, "auth2", 2)]),
                "routes.ts",
                "auth2"
            ),
            None,
            "not a middleware name"
        );
    }

    #[test]
    fn a_middleware_is_never_the_files_import_of_a_package() {
        // #2244: `import cors from 'cors'` is an import node named `cors`.
        let import = decl("routes.ts", NodeKind::Import, "cors", 1);
        assert_eq!(
            target(&ctx_of(vec![import.clone()]), "routes.ts", "cors"),
            None
        );
        let function = decl("routes.ts", NodeKind::Function, "cors", 4);
        assert_eq!(
            target(&ctx_of(vec![import, function.clone()]), "routes.ts", "cors"),
            Some((id(&function), 0.8))
        );
    }

    #[test]
    fn two_middleware_declarations_in_the_file_are_ambiguous() {
        let first = decl("routes.ts", NodeKind::Function, "auth", 2);
        let second = decl("routes.ts", NodeKind::Constant, "auth", 7);
        assert_eq!(
            target(&ctx_of(vec![first, second]), "routes.ts", "auth"),
            None
        );
    }

    #[test]
    fn a_middleware_suffix_never_falls_back_to_the_base_name() {
        // Upstream's second tier linked an imported `corsMiddleware()` to
        // the file's own `cors`.
        let import = decl("routes.ts", NodeKind::Import, "corsMiddleware", 1);
        let base = decl("routes.ts", NodeKind::Function, "cors", 3);
        assert_eq!(
            target(&ctx_of(vec![import, base]), "routes.ts", "corsMiddleware"),
            None
        );
        let from = "src/middleware/index.ts";
        let declared = decl(from, NodeKind::Function, "authMiddleware", 2);
        let base = decl(from, NodeKind::Function, "auth", 6);
        assert_eq!(
            target(
                &ctx_of(vec![declared.clone(), base]),
                from,
                "authMiddleware"
            ),
            Some((id(&declared), 0.8))
        );
    }

    #[test]
    fn a_controller_method_resolves_only_in_its_own_file() {
        // Tier A: a method in a file whose path names the controller.
        let from = "src/user.routes.ts";
        let own = decl(from, NodeKind::Function, "list", 2);
        assert_eq!(
            target(&ctx_of(vec![own.clone()]), from, "userController.list"),
            Some((id(&own), 0.85))
        );
        let other = decl("src/controllers/user.ts", NodeKind::Method, "list", 2);
        assert_eq!(
            target(
                &ctx_of(vec![other.clone()]),
                "src/routes.ts",
                "userController.list"
            ),
            None
        );
        assert_eq!(
            target(&ctx_of(vec![own, other]), from, "userController.list"),
            None,
            "two files name the controller"
        );
    }

    #[test]
    fn a_controller_object_lends_no_method_by_its_name() {
        // Upstream's second tier: no path names `user`, and the file
        // declaring `userController` holds a `list`. The instance-method
        // strategy links that call; this resolver does not.
        let from = "src/routes.ts";
        let controller = decl(from, NodeKind::Variable, "userController", 1);
        let list = decl(from, NodeKind::Method, "list", 2);
        assert_eq!(
            target(&ctx_of(vec![controller, list]), from, "userController.list"),
            None
        );
    }

    #[test]
    fn a_service_method_resolves_only_in_its_own_file() {
        let from = "src/user-api.ts";
        let own = decl(from, NodeKind::Function, "lookup", 2);
        assert_eq!(
            target(&ctx_of(vec![own.clone()]), from, "userService.lookup"),
            Some((id(&own), 0.8))
        );
        assert_eq!(
            target(&ctx_of(vec![own.clone()]), from, "userUtils.lookup"),
            Some((id(&own), 0.8))
        );
        let other = decl("src/services/user.ts", NodeKind::Method, "lookup", 2);
        assert_eq!(
            target(&ctx_of(vec![other]), "src/routes.ts", "userService.lookup"),
            None
        );
        let twin = decl("src/user-db.ts", NodeKind::Function, "lookup", 2);
        assert_eq!(
            target(&ctx_of(vec![own, twin]), from, "userService.lookup"),
            None
        );
    }

    // -- UTF-16 measurements and JavaScript whitespace ------------------------

    #[test]
    fn a_reply_column_counts_utf16_code_units() {
        // Before `res.json(`: 2 + 7 (`/* é */`) + 11 + 4 (`'😀'`) + 2 units;
        // 29 bytes.
        let src = "router.get('/x', (req, res) => {\n  /* é */ const s = '😀'; res.json(1);\n});\n";
        let replies: Vec<(i64, i64)> = extract_routes("routes.ts", src)
            .references
            .iter()
            .filter(|r| r.reference_name == "res.json")
            .map(|r| (r.line, r.column))
            .collect();
        assert_eq!(replies, [(2, 26)]);
    }

    #[test]
    fn the_regex_window_counts_utf16_code_units() {
        // 25 units of blanks — a comment of 20 `é`s and a space, 45 bytes —
        // leave `x` in the 32-unit window: a division, so the `)` a regex
        // would have hidden closes the call early.
        let route =
            |comment: &str| format!("router.get('/x', f(x/*{comment}*/ /\\)/), getUsers);\n");
        lacks(&refs_of(&route(&"é".repeat(20))), "references:getUsers");
        contains(&refs_of(&route(&"a".repeat(30))), "references:getUsers");
    }

    #[test]
    fn a_window_that_cuts_a_surrogate_pair_reads_it_as_no_space() {
        let before = format!("😀{}/", " ".repeat(31));
        let text = Text::new(&before);
        let window = text.units_before(0, before.len() - 1, 32);
        assert_eq!(window, format!("\u{FFFD}{}", " ".repeat(31)));
        assert!(!regex_start_before().is_match(&window));
        let after = format!("{}😀", " ".repeat(22));
        let text = Text::new(&after);
        let window = text.units_after(0, 23);
        assert_eq!(window, format!("{}\u{FFFD}", " ".repeat(22)));
        assert!(!regex_end_after().is_match(&window));
        // Blanked by the comment stripper, a cut character is a space.
        let commented = format!("/*😀*/{}", " ".repeat(10));
        let text = Text::new(&commented);
        assert_eq!(text.units_before(0, commented.len(), 15), " ".repeat(15));
    }

    #[test]
    fn javascript_whitespace_is_what_separates() {
        // `\s` holds U+FEFF; U+0085 is no JavaScript whitespace.
        let result = extract_routes("routes.ts", "router.get(\u{FEFF}'/x', h);\n");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /x");
        assert_eq!(result.nodes[0].end_column, 17, "UTF-16 units, not bytes");
        assert!(names("routes.ts", "router.get(\u{85}'/x', h);\n").is_empty());
    }

    // -- mounts --------------------------------------------------------------

    fn prefixes(src: &str) -> Vec<String> {
        find_mounts(&Text::new(src))
            .into_iter()
            .map(|mount| mount.prefix)
            .collect()
    }

    #[test]
    fn a_mount_needs_matching_quotes_and_later_ones_still_count() {
        assert_eq!(
            prefixes("app.use('/api\", x); app.use('/v1', users);\n"),
            ["/v1"]
        );
        assert_eq!(
            prefixes("app.use(`/t`, r); app.use(\"/u\", s);\n"),
            ["/t", "/u"]
        );
        assert!(prefixes("app.use('/a\"b', r);\n").is_empty());
    }

    /// A project of `files`, each with its file node and Express's routes.
    fn indexed(files: &[(&str, &str)]) -> RouteCtx {
        files
            .iter()
            .fold(RouteCtx::default(), |ctx, (path, content)| {
                let mut nodes = vec![framework_node(
                    format!("file:{path}"),
                    NodeKind::File,
                    path.rsplit('/').next().unwrap_or(path).to_string(),
                    path.to_string(),
                    path.to_string(),
                    1,
                    1,
                    0,
                    0,
                    express_language(path),
                    false,
                )];
                nodes.extend(extract_routes(path, content).nodes);
                ctx.routes(path, nodes).file(path, content)
            })
    }

    /// The renames `post_extract` makes, by route id.
    fn renames(ctx: &RouteCtx) -> BTreeMap<String, String> {
        ExpressResolver
            .post_extract(ctx)
            .unwrap_or_default()
            .into_iter()
            .map(|node| (node.id, node.name))
            .collect()
    }

    #[test]
    fn a_mount_renames_the_mounted_routers_routes() {
        let ctx = indexed(&[
            (
                "app.js",
                "const users = require('./routes/users');\napp.use('/api', users);\n",
            ),
            ("routes/users.js", "router.get('/users/:id', loadUser);\n"),
        ]);
        let route = ctx.route("GET /users/:id", "routes/users.js");
        assert_eq!(
            renames(&ctx),
            BTreeMap::from([(route, "GET /api/users/:id".to_string())])
        );
    }

    #[test]
    fn prefixes_compose_down_the_mount_tree() {
        let ctx = indexed(&[
            ("app.js", "app.use('/api', require('./routes'));\n"),
            (
                "routes/index.js",
                "router.use('/users', require('./users'));\n",
            ),
            ("routes/users.js", "router.post('/', createUser);\n"),
        ]);
        // `router.use('/users', …)` is a route of its own, renamed too.
        let mount = ctx.route("USE /users", "routes/index.js");
        let route = ctx.route("POST /", "routes/users.js");
        assert_eq!(
            renames(&ctx),
            BTreeMap::from([
                (mount, "USE /api/users".to_string()),
                (route, "POST /api/users".to_string()),
            ])
        );
    }

    #[test]
    fn an_imported_router_is_a_mount_target() {
        let ctx = indexed(&[
            (
                "app.ts",
                "import users from './routes/users';\napp.use('/api', users);\n",
            ),
            ("routes/users.ts", "router.get('/me', me);\n"),
        ])
        .import(
            "app.ts",
            ImportMapping {
                local_name: "users".to_string(),
                exported_name: "default".to_string(),
                source: "./routes/users".to_string(),
                is_default: true,
                is_namespace: false,
            },
        );
        let route = ctx.route("GET /me", "routes/users.ts");
        assert_eq!(
            renames(&ctx),
            BTreeMap::from([(route, "GET /api/me".to_string())])
        );
    }

    #[test]
    fn a_router_mounted_at_two_paths_keeps_its_in_file_names() {
        let ctx = indexed(&[
            ("app.js", "app.use('/api', require('./routes/users'));\n"),
            (
                "admin.js",
                "app.use('/admin', require('./routes/users'));\n",
            ),
            ("routes/users.js", "router.get('/users', list);\n"),
        ]);
        assert!(renames(&ctx).is_empty());
    }

    #[test]
    fn a_file_mounting_itself_is_no_mount() {
        let ctx = indexed(&[(
            "app.js",
            "app.use('/api', require('./app'));\napp.get('/health', health);\n",
        )]);
        assert!(renames(&ctx).is_empty());
    }

    #[test]
    fn a_route_no_mount_renames_gets_its_in_file_name_back() {
        let ctx = indexed(&[("routes/users.js", "router.get('/users', list);\n")]);
        let id = ctx.route("GET /users", "routes/users.js");
        // An earlier pass mounted it at `/api`; the mount is gone.
        let mut stale = ctx
            .nodes()
            .iter()
            .find(|node| node.id == id)
            .cloned()
            .expect("the route");
        stale.name = "GET /api/users".to_string();
        let ctx = RouteCtx::default()
            .routes(
                "routes/users.js",
                ctx.nodes()
                    .iter()
                    .filter(|node| node.id != id)
                    .cloned()
                    .chain([stale])
                    .collect(),
            )
            .file("routes/users.js", "router.get('/users', list);\n");
        assert_eq!(
            renames(&ctx),
            BTreeMap::from([(id, "GET /users".to_string())])
        );
    }

    #[test]
    fn another_frameworks_route_in_a_mounted_file_is_left_alone() {
        // An Astro endpoint's route reads `file::route:/api/hello`: upstream
        // would rename it `route /x/api/hello`.
        let file = "src/pages/api/hello.ts";
        let endpoint = framework_node(
            format!("route:{file}:/api/hello:1"),
            NodeKind::Route,
            "/api/hello".to_string(),
            format!("{file}::route:/api/hello"),
            file.to_string(),
            1,
            1,
            0,
            0,
            Language::TypeScript,
            false,
        );
        let ctx = indexed(&[
            (
                "app.ts",
                "app.use('/x', require('./src/pages/api/hello'));\n",
            ),
            (file, "export const GET = () => new Response('hi');\n"),
        ]);
        let ctx = ctx.routes(file, vec![endpoint]);
        assert!(renames(&ctx).is_empty());
    }
}
