//! React Router: the routes a file declares, and the navigation written as a
//! string. Ports the route half of `upstream resolution/frameworks/react.ts`
//! (`scanRoutes` and the readers it uses) and `frameworks/react-router.ts`.
//!
//! 1. **A route is declared in markup or in a route object.** `<Route
//!    path="/payment" component={PaymentScreen}/>` (v5), `<Route
//!    path="team" element={<Team/>}/>` (v6), `createBrowserRouter([{ path,
//!    element, children }])` (v6.4+): [`extract_routes`] reads a file's
//!    `<Route>` elements, and in a file that makes a router
//!    ([`ROUTER_CALL`]) its route objects, with a structural scanner that
//!    treats strings, comments, regex literals, JSX and balanced brackets as
//!    units. A nested route's path is relative to the routes around it; an
//!    index route is the page at its parent's address; a route around others
//!    is the layout they render inside (a `layout:` reference); a path built
//!    from a constant (`path: paths.app.root.path`) is named after
//!    extraction by reading the constant's object literal
//!    ([`rename_constant_routes`]).
//! 2. **Navigation is a string.** `history.push('/placeorder')`,
//!    `navigate(\`/cart/${id}\`)`, `router.navigate(…)`, `redirect('/login')`:
//!    [`ReactRouterResolver::resolve`] reads the argument, matches it against
//!    the routes of the calling file's app and returns a `navigates` edge
//!    carrying the href. `<Link to>` is markup, not a call.
//!
//! The lazy loaders' own readers (an export a loader picks, React Router 7's
//! lazy properties), the tables another file hands the router and the sync
//! retries of navigation calls are later changes; a `lazy` loader here names
//! the module its first `import(…)` loads, as upstream did before #2480.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::framework_node;
use super::js_scan::{JS_SPACE, is_js_space, js_trim, match_bracket, skip_string};
use super::lazy_loader;
use super::route_href::{
    ExactTable, RouteEntry, destinations_for_href, first_argument_text, parse_href_expression,
    read_href_via_local,
};
use crate::framework::{FrameworkResolver, NavigationCalls};
use crate::import_resolver::resolve_import_path;
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// A `lazy: () => import('./x')` route's reference: the module, not a symbol
/// (`LAZY_ROUTE_PREFIX`).
pub(crate) const LAZY_ROUTE_PREFIX: &str = "lazy-import:";

/// The signature prefix of a route whose path is built from a constant: its
/// parts, as JSON, named after extraction (`ROUTE_PARTS_PREFIX`).
pub(crate) const ROUTE_PARTS_PREFIX: &str = "route-parts:";

/// A route's reference to a layout it renders inside (`LAYOUT_PREFIX`).
pub(crate) const LAYOUT_PREFIX: &str = "layout:";

/// React Router's `<Outlet />`: what a route with no element of its own
/// renders, the routes inside it (`OUTLET`).
pub(crate) const OUTLET: &str = "Outlet";

/// How many nested units and JSX elements the scanner follows (`unit` and
/// `jsx` count against it together). Upstream recurses without a bound; this
/// keeps a pathological nesting from running the call stack out, and a file
/// nested deeper yields no routes (`scan_route_declarations`).
const MAX_SCAN_DEPTH: usize = 256;

/// The languages routes and navigation calls are written in.
const ROUTE_LANGUAGES: [Language; 4] = [
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];

/// The end of the JavaScript white space starting at `at` in `s`.
fn skip_js_space(s: &str, at: usize) -> usize {
    at + (s[at..].len() - s[at..].trim_start_matches(is_js_space).len())
}

/// A word character of JavaScript's `\w` and `\b`.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A byte that may start an ASCII JavaScript identifier.
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

/// A byte that may continue an ASCII JavaScript identifier.
fn is_ident_byte(b: u8) -> bool {
    is_word_byte(b) || b == b'$'
}

/// The end of the ASCII identifier starting at `at`, or `None`.
fn identifier_end(s: &str, at: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if !bytes.get(at).copied().is_some_and(is_ident_start) {
        return None;
    }
    let mut end = at + 1;
    while bytes.get(end).copied().is_some_and(is_ident_byte) {
        end += 1;
    }
    Some(end)
}

/// The largest char boundary of `s` at or below `at`.
fn floor_boundary(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

// =============================================================================
// Route paths
// =============================================================================

/// One segment of a nested route's path: a literal, or a constant's member
/// expression (`RoutePart`). Serialized as upstream's `JSON.stringify` does:
/// `{"lit":"team"}`, `{"expr":"paths.app.root.path"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoutePart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
}

impl RoutePart {
    fn lit(lit: &str) -> Self {
        Self {
            lit: Some(lit.to_string()),
            expr: None,
        }
    }

    fn expr(expr: &str) -> Self {
        Self {
            lit: None,
            expr: Some(expr.to_string()),
        }
    }

    /// The part as the file shows it: the literal, or `{expr}`.
    fn shown(&self) -> String {
        match (&self.lit, &self.expr) {
            (Some(lit), _) => lit.clone(),
            (None, Some(expr)) => format!("{{{expr}}}"),
            (None, None) => "{undefined}".to_string(),
        }
    }
}

/// React Router's nesting: a child path is relative unless it starts with
/// `/` (`composeRoutePath`).
pub(crate) fn compose_route_path(parts: &[String]) -> String {
    let mut path = String::new();
    for part in parts {
        if part.starts_with('/') {
            path.clone_from(part);
        } else if !part.is_empty() {
            path = format!("{}/{part}", path.trim_end_matches('/'));
        }
    }
    let mut out = String::with_capacity(path.len() + 1);
    out.push('/');
    for ch in path.trim_start_matches('/').chars() {
        // `/{2,}` → `/`.
        if ch == '/' && out.ends_with('/') {
            continue;
        }
        out.push(ch);
    }
    // `(.)\/$` → `$1`: a trailing slash goes unless it is the whole path, or
    // follows a line terminator, which `.` does not match.
    if out.len() > 1 && out.ends_with('/') {
        let before = out[..out.len() - 1].chars().next_back();
        if !matches!(before, Some('\n' | '\r' | '\u{2028}' | '\u{2029}')) {
            out.pop();
        }
    }
    out
}

/// A string literal's content as written (`literal`): `'/team'` or
/// `"/team"`, escapes kept, `None` for anything else.
fn literal(value: &str) -> Option<String> {
    let value = js_trim(value);
    let bytes = value.as_bytes();
    let quote = *bytes.first()?;
    if !matches!(quote, b'"' | b'\'') || bytes.len() < 2 || *bytes.last()? != quote {
        return None;
    }
    let inner = &value[1..value.len() - 1];
    let inner_bytes = inner.as_bytes();
    let mut i = 0;
    while i < inner_bytes.len() {
        match inner_bytes[i] {
            b'\\' => {
                // `\\.` takes the next character, but not a line terminator.
                let next = inner[i + 1..].chars().next()?;
                if matches!(next, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                    return None;
                }
                i += 1 + next.len_utf8();
            }
            b if b == quote => return None,
            _ => i += 1,
        }
    }
    Some(inner.to_string())
}

// =============================================================================
// Constant paths and route-config hrefs
// =============================================================================

/// Where a declaration's text names `name`, as upstream's unescaped
/// `` text.search(new RegExp(`\\b${name}\\b`)) `` finds it, or `None`. A
/// `$` in the name is an anchor there, which never matches.
fn named_at(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() || name.contains('$') {
        return None;
    }
    let bytes = text.as_bytes();
    let name_bytes = name.as_bytes();
    let first_word = is_word_byte(name_bytes[0]);
    let last_word = is_word_byte(name_bytes[name_bytes.len() - 1]);
    text.match_indices(name).map(|(at, _)| at).find(|&at| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + name.len()).copied();
        before.is_some_and(is_word_byte) != first_word
            && after.is_some_and(is_word_byte) != last_word
    })
}

/// The file and binding a member expression's root name reads: the route
/// file's import of it, else the file itself (`constantPathValue`'s and
/// `configHrefExpression`'s lookup). `None` when an import names a module
/// that resolves to no file.
fn constant_source(
    root: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    let mapping = context
        .get_import_mappings(from_file, Language::Tsx)
        .into_iter()
        .find(|m| m.local_name == root)
        .or_else(|| {
            context
                .get_import_mappings(from_file, Language::TypeScript)
                .into_iter()
                .find(|m| m.local_name == root)
        });
    let Some(mapping) = mapping else {
        return Some((from_file.to_string(), root.to_string()));
    };
    let file = resolve_import_path(&mapping.source, from_file, Language::TypeScript, context)?;
    let name = if !mapping.exported_name.is_empty()
        && mapping.exported_name != "default"
        && mapping.exported_name != "*"
    {
        mapping.exported_name
    } else {
        root.to_string()
    };
    Some((file, name))
}

/// The text of the constant or variable `name` declared in `file`, and where
/// its object literal opens, or `None`.
fn constant_object(
    file: &str,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<(String, usize)> {
    let decl = context
        .get_nodes_in_file_shared(file)
        .into_iter()
        .find(|n| n.name == name && matches!(n.kind, NodeKind::Constant | NodeKind::Variable))?;
    let content = context
        .read_file_shared(file)
        .unwrap_or_else(|| Arc::from(""));
    let lines: Vec<&str> = content.split('\n').collect();
    let first = usize::try_from(decl.start_line - 1)
        .unwrap_or(0)
        .min(lines.len());
    let end = usize::try_from(decl.end_line)
        .unwrap_or(0)
        .clamp(first, lines.len());
    let text = lines[first..end].join("\n");
    let from = named_at(&text, name).unwrap_or(0);
    let open = text[from..].find('{')? + from;
    Some((text, open))
}

/// The string a member expression like `paths.app.root.path` reads from a
/// constant object literal: the constant found through the route file's
/// import of its root name, or in the file itself (`constantPathValue`).
pub(crate) fn constant_path_value(
    expr: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let mut keys = expr.split('.');
    let root = keys.next().filter(|root| !root.is_empty())?;
    let keys: Vec<&str> = keys.collect();
    if keys.is_empty() {
        return None;
    }
    let (file, name) = constant_source(root, from_file, context)?;
    let (text, open) = constant_object(&file, &name, context)?;
    read_object_path(&text, open, &keys)
}

/// The href a route-config object names — `paths.app.discussion.getHref(id)`
/// or `paths.app.discussion.path` against `export const paths = { app: {
/// discussion: { path: 'discussions/:discussionId', getHref: (id: string) =>
/// \`/app/discussions/${id}\` } } }` (bulletproof-react's `config/paths.ts`)
/// — as the string or template literal it returns, ready for the href
/// reader. A `${…}` glued to a segment is dropped; one that is a whole
/// segment stays a hole (`configHrefExpression`, upstream #2187).
pub(crate) fn config_href_expression(
    expr: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    static EXPR: OnceLock<Regex> = OnceLock::new();
    static ARROW: OnceLock<Regex> = OnceLock::new();
    static RETURN: OnceLock<Regex> = OnceLock::new();
    let captures = EXPR
        .get_or_init(|| {
            Regex::new(&format!(
                r"^{JS_SPACE}*([A-Za-z_$][A-Za-z0-9_$]*)((?:{JS_SPACE}*\??\.{JS_SPACE}*[A-Za-z_$][A-Za-z0-9_$]*)+){JS_SPACE}*(\((?:[^()]|\([^()]*\))*\))?{JS_SPACE}*$"
            ))
            .expect("route-config expression")
        })
        .captures(expr)?;
    let root = captures.get(1)?.as_str();
    let keys: Vec<String> = captures
        .get(2)?
        .as_str()
        .split('.')
        .map(|key| {
            key.chars()
                .filter(|&c| c != '?' && !is_js_space(c))
                .collect::<String>()
        })
        .filter(|key| !key.is_empty())
        .collect();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let (file, name) = constant_source(root, from_file, context)?;
    let (text, open) = constant_object(&file, &name, context)?;
    let mut value = read_object_value(&text, open, &keys)?;
    // A function's value is what it returns: `(id: string) => \`/app/…\``,
    // `() => { return '/'; }`.
    if captures.get(3).is_some() {
        let arrow = ARROW.get_or_init(|| {
            Regex::new(&format!(
                r"^(?:async{JS_SPACE}+)?(?:\([^()]*(?:\([^()]*\)[^()]*)*\)|[A-Za-z_$][A-Za-z0-9_$]*){JS_SPACE}*(?::{JS_SPACE}*[^=]+?)?=>{JS_SPACE}*"
            ))
            .expect("arrow head")
        });
        let head = arrow.find(&value)?.end();
        value = js_trim(&value[head..]).to_string();
        if value.starts_with('{') {
            value = RETURN
                .get_or_init(|| {
                    Regex::new(&format!(
                        r"(?-u:\b)return{JS_SPACE}+([`'\x22][\s\S]*?[`'\x22]){JS_SPACE}*;?{JS_SPACE}*\}}"
                    ))
                    .expect("block return")
                })
                .captures(&value)
                .and_then(|c| c.get(1))
                .map_or_else(String::new, |m| m.as_str().to_string());
        }
    }
    if !value.starts_with(['`', '\'', '"']) {
        return None;
    }
    Some(if value.starts_with('`') {
        drop_glued_template_holes(&value)
    } else {
        value
    })
}

/// Remove each `${…}` of a template literal that is not a whole path segment
/// (`dropGluedTemplateHoles`).
fn drop_glued_template_holes(template: &str) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    let mut copied = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
            out.push_str(&template[copied..i]);
            let mut depth = 0usize;
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'{' {
                    depth += 1;
                } else if bytes[j] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            let hole_end = (j + 1).min(bytes.len());
            let next = bytes.get(j + 1).copied();
            if out.ends_with('/') && matches!(next, Some(b'/' | b'`' | b'?') | None) {
                out.push_str(&template[i..hole_end]);
            }
            i = j + 1;
            copied = i.min(bytes.len());
            continue;
        }
        i += 1;
    }
    if copied < bytes.len() {
        out.push_str(&template[copied..]);
    }
    out
}

/// The end of the string, template or character literal opened at `at`, as
/// `readObjectValue`'s local `skipString` finds it: past the closing quote,
/// escapes skipped, no interpolation followed.
fn skip_quoted(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[at];
    let mut j = at + 1;
    while j < bytes.len() && bytes[j] != quote {
        if bytes[j] == b'\\' {
            j += 1;
        }
        j += 1;
    }
    (j + 1).min(bytes.len())
}

/// Where the value starting at `at` ends: at a `,` or a closer at its own
/// depth (`skipValue`).
fn skip_value(text: &str, mut j: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    while j < bytes.len() {
        match bytes[j] {
            b'"' | b'\'' | b'`' => {
                j = skip_quoted(text, j);
                continue;
            }
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                if depth == 0 {
                    return j;
                }
                depth -= 1;
            }
            b',' if depth == 0 => return j,
            _ => {}
        }
        j += 1;
    }
    j
}

/// An object entry's key at `i` (`^\s*(?:([A-Za-z_$][A-Za-z0-9_$]*)|["']([^"']+)["'])\s*:\s*`):
/// the key and where its value starts.
fn object_entry_key(text: &str, i: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let at = skip_js_space(text, i);
    let (key, after) = if let Some(end) = identifier_end(text, at) {
        (&text[at..end], end)
    } else if matches!(bytes.get(at), Some(b'"' | b'\'')) {
        let close = at
            + 1
            + bytes[at + 1..]
                .iter()
                .position(|b| matches!(b, b'"' | b'\''))?;
        if close == at + 1 {
            return None;
        }
        (&text[at + 1..close], close + 1)
    } else {
        return None;
    };
    let colon = skip_js_space(text, after);
    (bytes.get(colon) == Some(&b':')).then(|| (key, skip_js_space(text, colon + 1)))
}

/// Walk `keys` into the object literal opening at `at`; the final value's
/// source text, or `None` (`readObjectValue`).
fn read_object_value(text: &str, at: usize, keys: &[&str]) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = at + 1;
    while i < bytes.len() {
        let Some((key, value_at)) = object_entry_key(text, i) else {
            let next = skip_value(text, i);
            if bytes.get(next) != Some(&b',') {
                return None;
            }
            i = next + 1;
            continue;
        };
        let end = skip_value(text, value_at);
        if key == keys[0] {
            if keys.len() == 1 {
                return Some(js_trim(&text[value_at..end]).to_string());
            }
            return (bytes.get(value_at) == Some(&b'{'))
                .then(|| read_object_value(text, value_at, &keys[1..]))
                .flatten();
        }
        if bytes.get(end) != Some(&b',') {
            return None;
        }
        i = end + 1;
    }
    None
}

/// Walk `keys` into the object literal opening at `at`; the string literal
/// at the end, or `None` (`readObjectPath`).
fn read_object_path(text: &str, at: usize, keys: &[&str]) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = at + 1;
    while i < bytes.len() {
        let Some((key, value_at)) = object_entry_key(text, i) else {
            let next = skip_value(text, i);
            if bytes.get(next) != Some(&b',') {
                return None;
            }
            i = next + 1;
            continue;
        };
        if key == keys[0] {
            if keys.len() == 1 {
                return leading_string(&text[value_at..]);
            }
            return (bytes.get(value_at) == Some(&b'{'))
                .then(|| read_object_path(text, value_at, &keys[1..]))
                .flatten();
        }
        let end = skip_value(text, value_at);
        if bytes.get(end) != Some(&b',') {
            return None;
        }
        i = end + 1;
    }
    None
}

/// The content of the string literal `s` starts with, as upstream's
/// backtracking `^(["'])((?:\\.|(?!\1).)*)\1` finds it, or `None`. The literal
/// cannot cross a line terminator, and a backslash before the quote may end
/// it when nothing later does: `'a\'` is `a\`.
fn leading_string(s: &str) -> Option<String> {
    let quote = s.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let body = &s[1..];
    let line = &body[..body
        .find(['\n', '\r', '\u{2028}', '\u{2029}'])
        .unwrap_or(body.len())];
    // `close[p]`: where the closing quote is when the content is read from
    // `p`, trying an escape, then any other character, then the quote.
    let mut close: Vec<Option<usize>> = vec![None; line.len() + 1];
    for (at, ch) in line.char_indices().rev() {
        let next = at + ch.len_utf8();
        let mut found = None;
        if ch == '\\' {
            found = line[next..]
                .chars()
                .next()
                .and_then(|escaped| close[next + escaped.len_utf8()]);
        }
        if found.is_none() && ch != quote {
            found = close[next];
        }
        if found.is_none() && ch == quote {
            found = Some(at);
        }
        close[at] = found;
    }
    close[0].map(|end| line[..end].to_string())
}

// =============================================================================
// The route scanner
// =============================================================================

/// A route a file declares (`RouteDeclaration`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteDeclaration {
    /// The route's full path as far as the file tells: nesting composed, a
    /// constant part shown as `{expr}`.
    pub path: String,
    /// Its path parts, outermost first.
    pub parts: Vec<RoutePart>,
    pub component: Option<String>,
    /// What its `lazy` loader renders, as the route's reference names it.
    pub lazy: Option<String>,
    /// The route objects around it that render something, outermost first:
    /// the layouts it renders inside.
    pub layouts: Vec<String>,
    /// Where it is declared: its `path` (or `index`) property, or its
    /// `<Route` tag.
    pub at: usize,
}

/// A path-bearing route object or `<Route>` element, with the extent its
/// children sit in (`RouteScope`).
#[derive(Debug, Clone)]
struct RouteScope {
    part: RoutePart,
    at: usize,
    /// Its `{` or `<`: the scope holds every scope that opens after this and
    /// closes by `end`.
    start: usize,
    end: usize,
    component: Option<String>,
    lazy: Option<String>,
    /// A `<Route>` element: a route even with nothing to render.
    jsx: bool,
    /// A route object that renders React Router's `<Outlet />`: a route at
    /// its address with nothing of its own, and no layout around the routes
    /// inside it (upstream #2670).
    outlet: bool,
    /// A layout at no address of its own.
    pathless: bool,
    /// The page at its parent's address.
    index: bool,
    /// An address the file does not spell out (`path={paths.home}`).
    opaque: bool,
}

impl RouteScope {
    fn new(part: RoutePart, at: usize, start: usize, end: usize) -> Self {
        Self {
            part,
            at,
            start,
            end,
            component: None,
            lazy: None,
            jsx: false,
            outlet: false,
            pathless: false,
            index: false,
            opaque: false,
        }
    }

    fn renders(&self) -> bool {
        self.component.is_some() || self.lazy.is_some()
    }
}

/// The calls that hand React Router its routes; every route object in a file
/// that makes one is a route (`ROUTER_CALL`).
fn router_call() -> &'static Regex {
    static ROUTER_CALL: OnceLock<Regex> = OnceLock::new();
    ROUTER_CALL.get_or_init(|| {
        Regex::new(r"(?-u:\b)(?:createBrowserRouter|createHashRouter|createMemoryRouter|createRoutesFromElements|useRoutes)(?-u:\b)")
            .expect("router call")
    })
}

/// A router written as an element; the `<Route>`s at its top are at `/`
/// (`ROUTER_ELEMENT`).
fn is_router_element(tag: &str) -> bool {
    let name = [
        "Browser",
        "Hash",
        "Memory",
        "Native",
        "Static",
        "History",
        "unstable_History",
    ]
    .iter()
    .find_map(|prefix| tag.strip_prefix(prefix))
    .unwrap_or(tag);
    name == "Router"
}

/// A guard or boundary wrapping the screen — `<ProtectedRoute>`,
/// `<AdminWrapper>`, `<Suspense>` — not what the route renders (`WRAPPER`).
fn is_wrapper(tag: &str) -> bool {
    static WRAPPER: OnceLock<Regex> = OnceLock::new();
    WRAPPER
        .get_or_init(|| {
            Regex::new(
                r"^(?:Suspense|ErrorBoundary|[A-Za-z0-9_]*(?:Guard|Provider|Wrapper|Route)|(?:Protected|Private|Auth|Require[A-Za-z0-9_]*)[A-Za-z0-9_]*)$",
            )
            .expect("wrapper")
        })
        .is_match(tag)
}

/// The component a route's `element` (`jsx`) or `component`/`Component`
/// value shows (`componentName`).
fn component_name(value: Option<&str>, jsx: bool) -> Option<String> {
    let value = value?;
    if !jsx {
        let name = js_trim(value);
        return (name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
            && name.bytes().all(is_word_byte))
        .then(|| name.to_string());
    }
    let mut inner = value.trim_start_matches(is_js_space);
    if let Some(rest) = inner.strip_prefix('(') {
        inner = rest.trim_start_matches(is_js_space);
    }
    let tag = inner.strip_prefix('<')?.trim_start_matches(is_js_space);
    if !tag.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
        return None;
    }
    let (own, props) = element_tags(inner);
    let content = |tag: &&String| !is_wrapper(tag) && tag.as_str() != OUTLET;
    // Then a page handed to a guard as a prop (`<PrivateRoute
    // component={<Profile />} />`), then the innermost wrapper:
    // `<RequireAuth><Outlet /></RequireAuth>` renders the guard.
    own.iter()
        .find(content)
        .or_else(|| props.iter().find(content))
        .or_else(|| own.iter().rev().find(|tag| tag.as_str() != OUTLET))
        .or_else(|| own.first())
        .cloned()
}

/// The component tags of a JSX element and of what it nests, outermost first
/// (`own`), and apart from them a page handed over in a `component`,
/// `element` or `page` attribute (`props`). Any other attribute's elements
/// are neither: `<Suspense fallback={<Loader />}><AdminPanel /></Suspense>`
/// renders AdminPanel (`elementTags`).
fn element_tags(text: &str) -> (Vec<String>, Vec<String>) {
    let bytes = text.as_bytes();
    let mut own = Vec::new();
    let mut props = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let name_at = skip_js_space(text, i + 1);
        let mut name_end = name_at;
        if bytes.get(name_at).is_some_and(u8::is_ascii_alphabetic) {
            name_end += 1;
            while bytes
                .get(name_end)
                .is_some_and(|b| is_word_byte(*b) || matches!(b, b'.' | b':' | b'-'))
            {
                name_end += 1;
            }
        }
        let followed = text[name_end..]
            .chars()
            .next()
            .is_some_and(|c| is_js_space(c) || c == '/' || c == '>');
        if name_end == name_at || !followed {
            i += 1;
            continue;
        }
        let name = &text[name_at..name_end];
        if name.as_bytes()[0].is_ascii_uppercase() && name.bytes().all(is_word_byte) {
            own.push(name.to_string());
        }
        // Skip the tag's attributes, expressions and strings whole.
        let mut j = name_end;
        let mut attr = "";
        let mut broke = false;
        while j < bytes.len() && bytes[j] != b'>' {
            let window_end = floor_boundary(text, j + 64);
            if let Some(end) = attribute_name_end(&text[j..window_end]) {
                attr = &text[j..j + end];
                j += end;
                continue;
            }
            let ch = bytes[j];
            let end = match ch {
                b'{' => match_bracket(text, j),
                b'"' | b'\'' => skip_string(text, j),
                _ => Some(j),
            };
            let Some(end) = end else {
                broke = true;
                break;
            };
            if ch == b'{'
                && ["component", "element", "page"]
                    .iter()
                    .any(|prop| attr.eq_ignore_ascii_case(prop))
            {
                props.extend(prop_tags(&text[j..end]));
            }
            if matches!(ch, b'{' | b'"' | b'\'') {
                attr = "";
            }
            j = end + text[end..].chars().next().map_or(1, char::len_utf8);
        }
        if broke {
            i = j;
            // `matchBracket`/`skipString` found no end: upstream leaves `j`
            // where the construct opened and carries on from there.
            i += 1;
            continue;
        }
        i = j;
    }
    (own, props)
}

/// The length of an attribute name (`^[A-Za-z_$][\w$:-]*`) at the start of
/// `s`, or `None`.
fn attribute_name_end(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if !bytes.first().copied().is_some_and(is_ident_start) {
        return None;
    }
    Some(
        1 + bytes[1..]
            .iter()
            .take_while(|b| is_ident_byte(**b) || matches!(b, b':' | b'-'))
            .count(),
    )
}

/// The component tags written in a prop's braces (`<\s*([A-Z]\w*)(?=[\s/>])`).
fn prop_tags(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let at = skip_js_space(text, i + 1);
        if bytes.get(at).is_some_and(u8::is_ascii_uppercase) {
            let mut end = at + 1;
            while bytes.get(end).copied().is_some_and(is_word_byte) {
                end += 1;
            }
            if text[end..]
                .chars()
                .next()
                .is_some_and(|c| is_js_space(c) || c == '/' || c == '>')
            {
                out.push(text[at..end].to_string());
            }
        }
        i += 1;
    }
    out
}

/// What a route's `lazy` loader renders, as the route's reference names it
/// ([`lazy_loader::lazy_route_reference`]), the page an `element` it hands
/// back shows read as a route's `element` is.
fn lazy_route_reference(loader: Option<&str>) -> Option<String> {
    lazy_loader::lazy_route_reference(loader, &|jsx| component_name(Some(jsx), true))
}

/// A route object's loader written as a method at `entry`, `async lazy() {
/// … }`: its `async ` prefix and where its parameter list opens
/// (`^(async\s+)?lazy\s*\(` within 32 code units).
fn lazy_method(source: &str, entry: usize) -> Option<(&str, usize)> {
    static METHOD: OnceLock<Regex> = OnceLock::new();
    let window = &source[entry..floor_boundary(source, entry + 32)];
    let method = METHOD
        .get_or_init(|| {
            Regex::new(&format!(r"^(async{JS_SPACE}+)?lazy{JS_SPACE}*\(")).expect("lazy method")
        })
        .captures(window)?;
    let asynchronous = method.get(1).map_or("", |prefix| prefix.as_str());
    Some((asynchronous, entry + method.get(0)?.end() - 1))
}

/// A data-router property's value and where its key starts.
struct Field {
    value: String,
    at: usize,
}

/// The structural scanner over one file's text (`scanRoutes`): strings,
/// comments, regex literals, JSX and balanced expressions are units.
struct Scanner<'s> {
    source: &'s str,
    allow_jsx: bool,
    /// Every route object in the file is read: it makes a router.
    objects: bool,
    depth: usize,
    /// A unit lay past `MAX_SCAN_DEPTH`: the scan stopped there, so what it
    /// read is not the file's whole route tree.
    truncated: bool,
    scopes: Vec<RouteScope>,
    /// How many `<Route>` tags have been read.
    route_tags: usize,
    /// Where the router itself is written (`createRoutesFromElements(…)`,
    /// `<BrowserRouter>…</BrowserRouter>`): its routes start at `/`.
    routers: Vec<(usize, usize)>,
}

impl<'s> Scanner<'s> {
    fn new(source: &'s str, allow_jsx: bool, objects: bool) -> Self {
        Self {
            source,
            allow_jsx,
            objects,
            depth: 0,
            truncated: false,
            scopes: Vec::new(),
            route_tags: 0,
            routers: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.source.len()
    }

    fn byte(&self, at: usize) -> Option<u8> {
        self.source.as_bytes().get(at).copied()
    }

    fn starts_with_at(&self, at: usize, pattern: &str) -> bool {
        self.source
            .as_bytes()
            .get(at..)
            .is_some_and(|rest| rest.starts_with(pattern.as_bytes()))
    }

    /// Past the character at `at`.
    fn next_char(&self, at: usize) -> usize {
        at + self.source[at..].chars().next().map_or(1, char::len_utf8)
    }

    /// Past white space and comments from `at` (`trivia`).
    fn trivia(&self, mut at: usize) -> usize {
        while at < self.len() {
            let rest = &self.source[at..];
            let ch = rest.chars().next().expect("not at the end");
            if is_js_space(ch) {
                at += ch.len_utf8();
            } else if let Some(line) = rest.strip_prefix("//") {
                at = line.find('\n').map_or(self.len(), |end| at + 2 + end);
            } else if let Some(block) = rest.strip_prefix("/*") {
                at = block.find("*/").map_or(self.len(), |end| at + 2 + end + 2);
            } else {
                break;
            }
        }
        at
    }

    /// The last character before `at`, white space skipped, and the text
    /// before `at` up to it (`tokenBefore`).
    fn token_before(&self, at: usize) -> &'s str {
        self.source[..at].trim_end_matches(is_js_space)
    }

    /// Whether `before` ends with the keyword `word` standing alone.
    fn ends_with_keyword(before: &str, word: &str) -> bool {
        before
            .strip_suffix(word)
            .is_some_and(|head| !head.as_bytes().last().copied().is_some_and(is_word_byte))
    }

    /// Whether a `/` at `at` starts a regex literal rather than a division.
    fn regex_may_start(&self, at: usize) -> bool {
        let before = self.token_before(at);
        let Some(&last) = before.as_bytes().last() else {
            return true;
        };
        b"=(:,[!&|?{};".contains(&last)
            || ["return", "throw", "case", "yield"]
                .iter()
                .any(|word| Self::ends_with_keyword(before, word))
    }

    /// The end of the regex literal at `at` on its line, flags included, or
    /// `None` when it does not close there.
    fn regex_end(&self, at: usize) -> Option<usize> {
        let bytes = self.source.as_bytes();
        let mut in_class = false;
        let mut i = at + 1;
        while i < bytes.len() && bytes[i] != b'\n' {
            match bytes[i] {
                b'\\' => i += 1,
                b'[' => in_class = true,
                b']' => in_class = false,
                b'/' if !in_class => {
                    i += 1;
                    while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
                        i += 1;
                    }
                    return Some(i);
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Past the string or template opened at `at`, a template's `${…}`
    /// scanned as units.
    fn string_end(&mut self, at: usize) -> usize {
        let bytes = self.source.as_bytes();
        let quote = bytes[at];
        let mut i = at + 1;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == quote {
                return i + 1;
            }
            if quote == b'`' && self.starts_with_at(i, "${") {
                i = self.unit(i + 1);
                continue;
            }
            i += 1;
        }
        self.len()
    }

    /// Whether a `(` at `at` follows `createRoutesFromElements` or
    /// `createRoutesFromChildren` (`routesFromElements`).
    fn routes_from_elements(&self, at: usize) -> bool {
        let before = self.token_before(at);
        if !matches!(before.as_bytes().last(), Some(b's' | b'n')) {
            return false;
        }
        let window = &self.source[floor_boundary(self.source, at.saturating_sub(40))..at];
        let window = window.trim_end_matches(is_js_space);
        ["createRoutesFromElements", "createRoutesFromChildren"]
            .iter()
            .any(|name| {
                window.strip_suffix(name).is_some_and(|head| {
                    !head
                        .as_bytes()
                        .last()
                        .is_some_and(|b| is_ident_byte(*b) || *b == b'.')
                })
            })
    }

    /// One unit at `at` and past it (`unit`).
    fn unit(&mut self, at: usize) -> usize {
        if at >= self.len() {
            return self.len();
        }
        if self.depth >= MAX_SCAN_DEPTH {
            self.truncated = true;
            return self.len();
        }
        self.depth += 1;
        let end = self.unit_inner(at);
        self.depth -= 1;
        end.min(self.len())
    }

    fn unit_inner(&mut self, at: usize) -> usize {
        let ch = self.source.as_bytes()[at];
        match ch {
            b'"' | b'\'' | b'`' => return self.string_end(at),
            // A regex can contain braces or JSX-looking text without ending
            // an expression.
            b'/' if self.regex_may_start(at) => {
                if let Some(end) = self.regex_end(at) {
                    return end;
                }
            }
            b'<' if self.allow_jsx
                && self
                    .byte(at + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'>') =>
            {
                let before = self.token_before(at);
                // `count<limit` and `factory<Type>()` are not JSX opening tags.
                let operand = before
                    .as_bytes()
                    .last()
                    .is_some_and(|b| is_ident_byte(*b) || matches!(b, b')' | b']' | b'\'' | b'"'));
                if !operand || Self::ends_with_keyword(before, "return") {
                    return self.jsx(at);
                }
            }
            _ => {}
        }
        let close = match ch {
            b'{' => b'}',
            b'[' => b']',
            b'(' => b')',
            _ => return self.next_char(at),
        };
        let router = ch == b'(' && self.routes_from_elements(at);
        let mut i = at + 1;
        let mut fields: HashMap<String, Field> = HashMap::new();
        loop {
            i = self.trivia(i);
            if i >= self.len() || self.byte(i) == Some(close) {
                break;
            }
            // A property must start at an object entry, never inside its value.
            let key = if ch == b'{' {
                object_key(self.source, i)
            } else {
                None
            };
            if let Some((name, key_end)) = key {
                let start = i;
                let value_at = self.trivia(key_end);
                i = value_at;
                let mut value_end = i;
                loop {
                    i = self.trivia(i);
                    if i >= self.len()
                        || matches!(self.byte(i), Some(b','))
                        || self.byte(i) == Some(close)
                    {
                        break;
                    }
                    i = self.unit(i);
                    value_end = i;
                }
                let value = js_trim(&self.source[value_at..value_end.max(value_at)]).to_string();
                fields.insert(name.to_string(), Field { value, at: start });
            } else {
                // Skip a whole entry (spread, method, shorthand), but still
                // visit nested units.
                let entry = i;
                loop {
                    i = self.trivia(i);
                    if i >= self.len()
                        || matches!(self.byte(i), Some(b','))
                        || self.byte(i) == Some(close)
                    {
                        break;
                    }
                    i = self.unit(i);
                }
                // `async lazy() { … }`: a route's loader written as a method.
                if self.objects
                    && ch == b'{'
                    && let Some((asynchronous, paren)) = lazy_method(self.source, entry)
                {
                    let value = format!(
                        "{asynchronous}function {}",
                        js_trim(&self.source[paren..i.max(paren)])
                    );
                    fields.insert("lazy".to_string(), Field { value, at: entry });
                }
            }
            if self.byte(i) == Some(b',') {
                i += 1;
            }
        }
        if self.objects && ch == b'{' && self.byte(i) == Some(close) {
            self.route_object(&fields, at, i);
        }
        if router {
            self.routers.push((at, i));
        }
        if i < self.len() { i + 1 } else { i }
    }

    /// The route the object literal between `start` and `end` declares, if
    /// any.
    fn route_object(&mut self, fields: &HashMap<String, Field>, start: usize, end: usize) {
        static EXPR: OnceLock<Regex> = OnceLock::new();
        let path_field = fields.get("path");
        let path = path_field.and_then(|field| literal(&field.value));
        let expr = path_field
            .filter(|field| {
                path.is_none()
                    && EXPR
                        .get_or_init(|| {
                            Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)+$")
                                .expect("member path")
                        })
                        .is_match(&field.value)
            })
            .map(|field| field.value.clone());
        let shown = component_name(fields.get("element").map(|f| f.value.as_str()), true)
            .or_else(|| component_name(fields.get("Component").map(|f| f.value.as_str()), false));
        let loaded = lazy_route_reference(fields.get("lazy").map(|f| f.value.as_str()));
        // A route with no element renders an `<Outlet />`, so one written
        // with it renders nothing of its own either: a route at its address,
        // and no layout around the routes inside it. At its parent's address
        // (`path: ''`) it only groups them, the parent's page staying the one
        // there.
        let outlet =
            shown.as_deref().or(loaded.as_deref()) == Some(OUTLET) && path.as_deref() != Some("");
        let component = shown.filter(|shown| shown != OUTLET);
        let lazy = loaded.filter(|loaded| loaded != OUTLET);
        let index = fields.get("index");
        let mut scope =
            if let Some(path_field) = path_field.filter(|_| path.is_some() || expr.is_some()) {
                let part = match &path {
                    Some(path) => RoutePart::lit(path),
                    None => RoutePart::expr(expr.as_deref().expect("expr path")),
                };
                let mut scope = RouteScope::new(part, path_field.at, start, end);
                scope.outlet = outlet;
                scope
            } else if path_field.is_none() && index.is_some_and(|index| index.value == "true") {
                // `{ index: true, element: <Home /> }` is the page at its parent's
                // address.
                RouteScope::new(RoutePart::lit(""), index.expect("index").at, start, end)
            } else if path_field.is_none()
                && (component.is_some() || lazy.is_some())
                && fields.contains_key("children")
            {
                let mut scope = RouteScope::new(RoutePart::lit(""), start, start, end);
                scope.pathless = true;
                scope
            } else {
                return;
            };
        scope.component = component;
        scope.lazy = lazy;
        self.scopes.push(scope);
    }

    /// The JSX element at `at` and past it (`jsx`).
    fn jsx(&mut self, at: usize) -> usize {
        if self.depth >= MAX_SCAN_DEPTH {
            self.truncated = true;
            return self.len();
        }
        self.depth += 1;
        let end = self.jsx_inner(at);
        self.depth -= 1;
        end.min(self.len())
    }

    fn jsx_inner(&mut self, at: usize) -> usize {
        let bytes = self.source.as_bytes();
        let mut i = at + 1;
        while bytes
            .get(i)
            .is_some_and(|b| is_word_byte(*b) || matches!(b, b'.' | b':' | b'-'))
        {
            i += 1;
        }
        let tag = &self.source[at + 1..i];
        let mut attrs: HashMap<&'s str, String> = HashMap::new();
        loop {
            i = self.trivia(i);
            if i >= self.len() || self.byte(i) == Some(b'>') || self.starts_with_at(i, "/>") {
                break;
            }
            let name_len = bytes[i..]
                .iter()
                .take_while(|b| is_word_byte(**b) || matches!(b, b':' | b'-'))
                .count();
            if name_len == 0 {
                i = self.unit(i);
                continue;
            }
            let name = &self.source[i..i + name_len];
            i = self.trivia(i + name_len);
            // A bare attribute is `true`: `<Route index element={<Home/>}/>`.
            if self.byte(i) != Some(b'=') {
                attrs.insert(name, "{true}".to_string());
                continue;
            }
            i = self.trivia(i + 1);
            let start = i;
            i = self.unit(i);
            attrs.insert(name, self.source[start.min(i)..i].to_string());
        }
        let mut scope: Option<usize> = None;
        // A `<Route>` with no address of its own, settled once its children
        // are read: the route, how many `<Route>` tags preceded its
        // children, and whether it is a page at its parent's address.
        let mut addressless: Option<(RouteScope, usize, bool)> = None;
        // `<BrowserRouter>…</BrowserRouter>`: the routes at its top are at `/`.
        let router = is_router_element(tag).then(|| {
            self.routers.push((at, self.len()));
            self.routers.len() - 1
        });
        if tag == "Route" && i < self.len() {
            let path_text = attrs.get("path").map(String::as_str);
            let expression = |name: &str| {
                attrs.get(name).map(|value| {
                    value
                        .strip_prefix('{')
                        .and_then(|inner| inner.strip_suffix('}'))
                        .unwrap_or(value)
                        .to_string()
                })
            };
            // `path="team"`, or the same string in braces: `path={"team"}`.
            let path = literal(path_text.unwrap_or(""))
                .or_else(|| literal(&expression("path").unwrap_or_default()));
            self.route_tags += 1;
            let element =
                component_name(expression("element").as_deref(), true).filter(|tag| tag != OUTLET);
            let component = component_name(expression("component").as_deref(), false)
                .or_else(|| element.clone());
            // A `<Route>` is a route with nothing of its own to render already.
            let lazy = lazy_route_reference(expression("lazy").as_deref())
                .filter(|loaded| loaded != OUTLET);
            let mut route = RouteScope::new(
                RoutePart::lit(path.as_deref().unwrap_or("")),
                at,
                at,
                self.len(),
            );
            route.component = component;
            route.lazy = lazy;
            route.jsx = true;
            let index = attrs.get("index").is_some_and(|value| {
                value
                    .strip_prefix('{')
                    .and_then(|inner| inner.strip_suffix('}'))
                    .is_some_and(|inner| js_trim(inner) == "true")
            });
            let chosen = if path.as_deref().is_some_and(|path| !path.is_empty()) {
                Some(route)
            } else if path_text.is_none() || path.as_deref() == Some("") {
                if index {
                    // `<Route index element={<Home/>}/>` is the page at its
                    // parent's address.
                    route.renders().then(|| {
                        route.index = true;
                        route
                    })
                } else {
                    // Otherwise it is a layout at no address of its own when
                    // routes sit inside it, and `path=""` with an `element`
                    // and nothing inside is the page at its parent's
                    // address. Version 5's `<Route path=""
                    // component={NotFound}/>` is a catch-all, which no
                    // address names.
                    let page =
                        path.as_deref() == Some("") && (element.is_some() || route.lazy.is_some());
                    addressless = Some((route, self.route_tags, page));
                    None
                }
            } else {
                // `path={paths.home}`: an address the file does not spell out.
                route.opaque = true;
                Some(route)
            };
            if let Some(chosen) = chosen {
                self.scopes.push(chosen);
                scope = Some(self.scopes.len() - 1);
            }
        }
        if self.starts_with_at(i, "/>") {
            return self.close_jsx(i + 2, scope, router, addressless);
        }
        i += 1;
        while i < self.len() {
            if self.starts_with_at(i, "</") {
                let end = self.source[i + 2..]
                    .find('>')
                    .map_or(self.len(), |end| i + 2 + end + 1);
                return self.close_jsx(end, scope, router, addressless);
            }
            if self.byte(i) == Some(b'<')
                && self
                    .byte(i + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'>')
            {
                i = self.jsx(i);
            } else if self.byte(i) == Some(b'{') {
                i = self.unit(i);
            } else {
                i = self.next_char(i);
            }
        }
        self.close_jsx(i, scope, router, addressless)
    }

    /// An element closing at `end`: its route and router end there, and a
    /// route with no address of its own is settled (`close`).
    fn close_jsx(
        &mut self,
        end: usize,
        scope: Option<usize>,
        router: Option<usize>,
        addressless: Option<(RouteScope, usize, bool)>,
    ) -> usize {
        let end = end.min(self.len());
        if let Some(scope) = scope {
            self.scopes[scope].end = end;
        }
        if let Some(router) = router {
            self.routers[router].1 = end;
        }
        if let Some((mut route, tags, page)) = addressless {
            route.end = end;
            if self.route_tags > tags {
                if route.renders() {
                    route.pathless = true;
                    self.scopes.push(route);
                }
            } else if page {
                route.index = true;
                self.scopes.push(route);
            }
        }
        end
    }

    /// The routes the scopes declare, nesting composed (the end of
    /// `scanRoutes`).
    fn routes(&self) -> Vec<RouteDeclaration> {
        let scopes = &self.scopes;
        let holds =
            |outer: &RouteScope, start: usize, end: usize| outer.start < start && outer.end >= end;
        let around = |start: usize, end: usize| -> Vec<&RouteScope> {
            let mut outer: Vec<&RouteScope> =
                scopes.iter().filter(|o| holds(o, start, end)).collect();
            outer.sort_by_key(|o| o.start);
            outer
        };
        let parts_of: Vec<Vec<RoutePart>> = scopes
            .iter()
            .map(|scope| {
                around(scope.start, scope.end)
                    .into_iter()
                    .map(|c| c.part.clone())
                    .chain(std::iter::once(scope.part.clone()))
                    .collect()
            })
            .collect();
        let path_of: Vec<String> = parts_of
            .iter()
            .map(|parts| {
                compose_route_path(&parts.iter().map(RoutePart::shown).collect::<Vec<_>>())
            })
            .collect();
        let in_router = |s: &RouteScope| {
            self.routers
                .iter()
                .any(|&(start, end)| start < s.start && end >= s.end)
        };
        // The page at its parent's address needs that address written down:
        // a route around it whose path the file spells out, or the router
        // itself. At the top of a component's own `<Routes>` it is wherever
        // another route mounts the component.
        let addressed = |s: &RouteScope| {
            if !s.index {
                return true;
            }
            let outer = around(s.start, s.end);
            !outer.iter().any(|c| c.opaque)
                && (outer.iter().any(|c| {
                    c.part.lit.as_deref().is_some_and(|lit| !lit.is_empty())
                        || c.part.expr.as_deref().is_some_and(|expr| !expr.is_empty())
                }) || in_router(s))
        };
        let pages: Vec<bool> = scopes
            .iter()
            .map(|s| !s.pathless && !s.opaque && (s.jsx || s.outlet || s.renders()) && addressed(s))
            .collect();
        let layout = |c: &RouteScope| c.renders() && !c.opaque;
        let mut routes = Vec::new();
        for (index, scope) in scopes.iter().enumerate() {
            if !pages[index] {
                continue;
            }
            let path = &path_of[index];
            // A route around others is the layout they render inside; when
            // one of them claims its address, the page there is that one —
            // not the layout, nor a `<Route path>` that only groups them.
            if scopes.iter().enumerate().any(|(other, inner)| {
                pages[other]
                    && inner.jsx == scope.jsx
                    && inner.renders()
                    && holds(scope, inner.start, inner.end)
                    && &path_of[other] == path
            }) {
                continue;
            }
            // A route object renders inside the route objects around it; a
            // `<Route>`, inside the routes around it of either kind.
            let layouts = around(scope.start, scope.end)
                .into_iter()
                .filter(|c| layout(c) && (scope.jsx || !c.jsx))
                .map(|c| {
                    c.component
                        .clone()
                        .or_else(|| c.lazy.clone())
                        .expect("renders")
                })
                .collect();
            routes.push(RouteDeclaration {
                path: path.clone(),
                parts: parts_of[index].clone(),
                component: scope.component.clone(),
                lazy: scope.lazy.clone(),
                layouts,
                at: scope.at,
            });
        }
        routes.sort_by_key(|route| route.at);
        routes
    }
}

/// An object entry's key at `i` and where past its `:` the value's trivia
/// starts (`^(?:([A-Za-z_$][A-Za-z0-9_$]*)|["']([^"']+)["'])\s*:`).
fn object_key(source: &str, i: usize) -> Option<(&str, usize)> {
    let bytes = source.as_bytes();
    let (key, after) = if let Some(end) = identifier_end(source, i) {
        (&source[i..end], end)
    } else if matches!(bytes.get(i), Some(b'"' | b'\'')) {
        let close = i
            + 1
            + bytes[i + 1..]
                .iter()
                .position(|b| matches!(b, b'"' | b'\''))?;
        if close == i + 1 {
            return None;
        }
        (&source[i + 1..close], close + 1)
    } else {
        return None;
    };
    let colon = skip_js_space(source, after);
    (bytes.get(colon) == Some(&b':')).then_some((key, colon + 1))
}

/// The routes a file's own text declares: its `<Route>` elements and, in a
/// file that makes a router, its route objects (`scanRouteDeclarations`).
pub(crate) fn scan_route_declarations(source: &str, allow_jsx: bool) -> Vec<RouteDeclaration> {
    static ROUTE_TAG: OnceLock<Regex> = OnceLock::new();
    let objects = router_call().is_match(source);
    if !objects
        && !ROUTE_TAG
            .get_or_init(|| Regex::new(r"<Route(?-u:\b)").expect("route tag"))
            .is_match(source)
    {
        return Vec::new();
    }
    let mut scanner = Scanner::new(source, allow_jsx, objects);
    let mut at = 0;
    loop {
        at = scanner.trivia(at);
        if at >= scanner.len() {
            break;
        }
        at = scanner.unit(at);
    }
    // KEEP-RUST: a file nested past the bound yields no routes. A route read
    // before the cut can sit in a parent that never closed, and composed
    // without it its path is wrong (`b` under `/a` would be `/b`).
    if scanner.truncated {
        return Vec::new();
    }
    scanner.routes()
}

/// Whether JSX parses in `file_path`: everything but `.ts`, `.mts` and
/// `.cts` (`allowsJsx`).
fn allows_jsx(file_path: &str) -> bool {
    !(file_path.ends_with(".ts") || file_path.ends_with(".mts") || file_path.ends_with(".cts"))
}

/// The language React gives its routes: `tsx` in a `.tsx` file, `jsx`
/// anywhere else, a `.ts` routes file included.
fn route_language(file_path: &str) -> Language {
    if file_path.ends_with(".tsx") {
        Language::Tsx
    } else {
        Language::Jsx
    }
}

/// The route nodes a file declares, each with the references that bind it:
/// what it renders, and the layouts around it (the route half of React's
/// `extract`, with `routeReferences`).
pub(crate) fn extract_routes(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    let language = route_language(file_path);
    for route in scan_route_declarations(content, allows_jsx(file_path)) {
        let line = content[..route.at].matches('\n').count() as i64 + 1;
        let mut node = framework_node(
            format!("route:{file_path}:{line}:{}", route.path),
            NodeKind::Route,
            route.path.clone(),
            format!("{file_path}::route:{}", route.path),
            file_path.to_string(),
            line,
            line,
            0,
            0,
            language,
            false,
        );
        // A path built from a constant (`paths.app.root.path`) is named in
        // `post_extract`.
        if route.parts.iter().any(|part| part.expr.is_some()) {
            node.signature = Some(format!(
                "{ROUTE_PARTS_PREFIX}{}",
                serde_json::to_string(&route.parts).expect("route parts serialize")
            ));
        }
        let reference = |name: String| RefView {
            row_id: None,
            from_node_id: node.id.clone(),
            reference_name: name,
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: file_path.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        };
        if let Some(target) = route.component.clone().or_else(|| route.lazy.clone()) {
            result.references.push(reference(target));
        }
        for layout in &route.layouts {
            result
                .references
                .push(reference(format!("{LAYOUT_PREFIX}{layout}")));
        }
        result.nodes.push(node);
    }
    result
}

/// Name the routes whose path is built from a constant — bulletproof-react's
/// `path: paths.app.discussions.path` under `path: paths.app.root.path` is
/// `/app/discussions` — by reading the constant's object literal where it is
/// declared. Idempotent: the parts ride on the node's signature, and the id
/// keeps the path as the file wrote it (React's `postExtract`).
pub(crate) fn rename_constant_routes(context: &dyn ResolutionContext) -> Vec<Node> {
    let mut updates = Vec::new();
    for route in context.get_nodes_by_kind_shared(NodeKind::Route) {
        let Some(json) = route
            .signature
            .as_deref()
            .and_then(|signature| signature.strip_prefix(ROUTE_PARTS_PREFIX))
        else {
            continue;
        };
        let Ok(parts) = serde_json::from_str::<Vec<RoutePart>>(json) else {
            continue;
        };
        let values: Option<Vec<String>> = parts
            .iter()
            .map(|part| {
                part.lit.clone().or_else(|| {
                    part.expr
                        .as_deref()
                        .and_then(|expr| constant_path_value(expr, &route.file_path, context))
                })
            })
            .collect();
        let Some(values) = values else {
            continue;
        };
        let name = compose_route_path(&values);
        if name != route.name {
            let mut renamed = route.as_ref().clone();
            renamed.name = name;
            updates.push(renamed);
        }
    }
    updates
}

// =============================================================================
// The route table
// =============================================================================

/// The app a route file belongs to: the directory above the first
/// conventional source directory, else the file's own folder (`appRootFor`):
/// `frontend/src/App.js` is `frontend/`, `src/App.tsx` is the repository's.
pub(crate) fn react_router_root(file_path: &str) -> String {
    super::route_href::app_root_for(file_path)
}

/// True for a route node React's extraction emitted, and no other
/// framework's (`isReactRouterRoute`): its id is a verbatim reconstruction
/// of its own fields. A server route carries its METHOD
/// (`route:file:12:POST:/login`), a file-based page carries no line, and a
/// path built from a constant keeps the id it was extracted with.
fn is_react_router_route(node: &Node) -> bool {
    let prefix = format!("route:{}:{}:", node.file_path, node.start_line);
    if !matches!(node.language, Language::Tsx | Language::Jsx) {
        return false;
    }
    let Some(rest) = node.id.strip_prefix(&prefix) else {
        return false;
    };
    rest == node.name
        || (!is_method_prefixed(rest)
            && node
                .signature
                .as_deref()
                .is_some_and(|signature| signature.starts_with(ROUTE_PARTS_PREFIX)))
}

/// `^[A-Z]+:`: a server route's `METHOD:` prefix.
fn is_method_prefixed(rest: &str) -> bool {
    let upper = rest.bytes().take_while(u8::is_ascii_uppercase).count();
    upper > 0 && rest.as_bytes().get(upper) == Some(&b':')
}

/// True for a route the table holds: an absolute path that is no splat
/// (`isDestination`). A nested route's path is relative without its tree, and
/// a splat matches everything, so it answers nothing.
fn is_destination(node: &Node) -> bool {
    is_react_router_route(node) && node.name.starts_with('/') && !node.name.ends_with('*')
}

/// `:id?`: a parameter React Router serves the route with or without.
fn is_optional_param(segment: &str) -> bool {
    segment.starts_with(':') && segment.ends_with('?')
}

/// One app's routes, matched by path.
#[derive(Debug, Default)]
struct AppRoutes {
    exact: ExactTable,
    dynamic: Vec<RouteEntry>,
}

/// The routes of every React Router app, by the directory the app lives in
/// (`reactRouterTable`). A call matches only the routes of its own app.
#[derive(Debug, Default)]
struct ReactRouterTable {
    by_root: BTreeMap<String, AppRoutes>,
}

impl ReactRouterTable {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut table = Self::default();
        let mut literal: HashSet<(String, String)> = HashSet::new();
        let mut shortened: Vec<(String, String, Arc<Node>)> = Vec::new();
        for node in context.get_nodes_by_kind_shared(NodeKind::Route) {
            if !is_destination(&node) {
                continue;
            }
            let root = react_router_root(&node.file_path);
            let path = if node.name.len() > 1 && node.name.ends_with('/') {
                node.name[..node.name.len() - 1].to_string()
            } else {
                node.name.clone()
            };
            let app = table.by_root.entry(root.clone()).or_default();
            if path.contains(':') {
                app.dynamic.push(RouteEntry::at(Arc::clone(&node), &path));
            }
            app.exact
                .insert(path.clone(), root.clone(), Arc::clone(&node));
            literal.insert((root.clone(), path.clone()));
            // React Router's optional parameter: `/cart/:id?` is the screen
            // for `/cart/5` AND for a bare `/cart`. Each shorter form is an
            // entry of its own, registered after every literal path, so a
            // route someone actually wrote always wins.
            let mut segments: Vec<&str> = path.split('/').skip(1).collect();
            while segments.len() > 1 && segments.last().is_some_and(|seg| is_optional_param(seg)) {
                segments.pop();
                shortened.push((
                    root.clone(),
                    format!("/{}", segments.join("/")),
                    Arc::clone(&node),
                ));
            }
        }
        for (root, path, node) in shortened {
            if literal.contains(&(root.clone(), path.clone())) {
                continue;
            }
            let Some(app) = table.by_root.get_mut(&root) else {
                continue;
            };
            if path.contains(':') {
                app.dynamic.push(RouteEntry::at(Arc::clone(&node), &path));
            }
            // Several shorter forms at one path are as ambiguous as several
            // routes at it (KEEP-RUST).
            app.exact.insert(path, root, node);
        }
        table
    }

    /// The routes of the app `file_path` belongs to: the longest root above
    /// it (`routesForFile`).
    fn app_of(&self, file_path: &str) -> Option<&AppRoutes> {
        self.by_root
            .iter()
            .filter(|(root, _)| file_path.starts_with(root.as_str()))
            .max_by_key(|(root, _)| root.len())
            .map(|(_, app)| app)
    }
}

/// The pass's React Router table.
fn react_router_table(context: &dyn ResolutionContext) -> Arc<ReactRouterTable> {
    context
        .route_tables()
        .get_or_build("react-router", || ReactRouterTable::build(context))
}

// =============================================================================
// Navigation calls and the resolver
// =============================================================================

/// `history.push` / `.replace` (v5), `navigate(…)` (v6's `useNavigate`),
/// `router.navigate(…)` (a data router), `redirect(…)` (a loader or an
/// action). The receiver is required for `push` / `replace`: an unqualified
/// `push` is an array's (`NAV_CALL`).
fn nav_call() -> &'static Regex {
    static NAV_CALL: OnceLock<Regex> = OnceLock::new();
    NAV_CALL.get_or_init(|| {
        Regex::new(
            r"^(?:history|navigate|router)\.(?:push|replace|navigate)$|^(?:navigate|redirect)$",
        )
        .expect("navigation call")
    })
}

/// The verb a navigation call name stands for, or `None`
/// (`reactRouterNavVerb`).
pub fn react_router_nav_verb(name: &str) -> Option<&str> {
    nav_call()
        .is_match(name)
        .then(|| name.rsplit('.').next().unwrap_or(name))
}

/// React Router's navigation resolver (`reactRouterResolver`).
pub struct ReactRouterResolver;

/// The navigation calls React Router binds to its routes: a route answers the
/// calls of its own app.
struct ReactRouterNavigation;

impl NavigationCalls for ReactRouterNavigation {
    fn tails(&self) -> &[&str] {
        &["push", "replace", "navigate", "redirect"]
    }

    fn scope(&self, route: &Node, _context: &dyn ResolutionContext) -> Option<Vec<String>> {
        is_destination(route).then(|| vec![react_router_root(&route.file_path)])
    }
}

impl FrameworkResolver for ReactRouterResolver {
    fn name(&self) -> &str {
        "react-router"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&ROUTE_LANGUAGES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        super::package_deps::depends_on(
            context,
            &["react-router", "react-router-dom", "react-router-native"],
        )
    }

    fn claims_reference(&self, name: &str) -> bool {
        nav_call().is_match(name)
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&ReactRouterNavigation)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }
        let verb = react_router_nav_verb(&reference.reference_name)?;
        if !ROUTE_LANGUAGES.contains(&reference.language) {
            return None;
        }
        let table = react_router_table(context);
        let app = table.app_of(&reference.file_path)?;
        if app.exact.is_empty() {
            return None;
        }
        let content = context.read_file_shared(&reference.file_path)?;
        let lines: Vec<&str> = content
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .collect();
        let arg = first_argument_text(&lines, reference.line, reference.column, verb)?;
        let mut href = parse_href_expression(&arg);
        // `navigate(paths.auth.login.getHref())`: a route-config object's
        // href (upstream #2187).
        if href.is_none() {
            href = config_href_expression(&arg, &reference.file_path, context)
                .as_deref()
                .and_then(parse_href_expression);
        }
        if href.is_none() {
            let start = context
                .get_node_by_id_shared(&reference.from_node_id)
                .filter(|enclosing| enclosing.file_path == reference.file_path)
                .map_or((reference.line - 40).max(1), |enclosing| {
                    enclosing.start_line
                });
            href = read_href_via_local(&lines, reference.line, reference.column, verb, start);
        }
        let href = href?;
        // Every arm of a conditional destination is somewhere this call goes:
        // the first is this reference's resolution, the rest ride along.
        let mut targets =
            destinations_for_href(&href, &app.exact, &app.dynamic, &reference.file_path)
                .into_iter();
        let (target, arm) = targets.next()?;
        let metadata = |display: &str| {
            let mut metadata = serde_json::Map::new();
            metadata.insert("href".to_string(), serde_json::json!(display));
            metadata.insert("navMethod".to_string(), serde_json::json!(verb));
            metadata
        };
        Some(
            ResolvedRef::new(
                reference.clone(),
                target.id.clone(),
                0.95,
                ResolvedBy::Framework,
            )
            .with_edge(EdgeShape {
                edge_kind: Some(EdgeKind::Navigates),
                metadata: metadata(&arm.display),
                also_targets: targets
                    .map(|(node, arm)| AlsoTarget {
                        target_node_id: node.id.clone(),
                        metadata: metadata(&arm.display),
                    })
                    .collect(),
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::{RouteCtx, call_in};

    /// Every route `source` declares, sorted, and what each renders (`->`) and
    /// renders inside (`~>`), as upstream's tests print a route's `references`
    /// edges; here the names the references carry.
    fn scanned(source: &str, file: &str) -> (Vec<String>, Vec<String>) {
        let routes = scan_route_declarations(source, allows_jsx(file));
        let mut paths: Vec<String> = routes.iter().map(|route| route.path.clone()).collect();
        paths.sort();
        let mut bindings = Vec::new();
        for route in &routes {
            if let Some(target) = route.component.as_ref().or(route.lazy.as_ref()) {
                bindings.push(format!("{}->{target}", route.path));
            }
            for layout in &route.layouts {
                bindings.push(format!("{}~>{layout}", route.path));
            }
        }
        bindings.sort();
        (paths, bindings)
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn a_nested_route_path_is_relative_to_its_parents() {
        let compose = |parts: &[&str]| compose_route_path(&strings(parts));
        assert_eq!(compose(&["/dashboard", "settings"]), "/dashboard/settings");
        assert_eq!(compose(&["/dashboard", "/absolute"]), "/absolute");
        assert_eq!(compose(&["/", ""]), "/");
        assert_eq!(compose(&[""]), "/");
        assert_eq!(compose(&["/app/", "discussions/"]), "/app/discussions");
        assert_eq!(compose(&["//a//b", "c"]), "/a/b/c");
        assert_eq!(
            compose(&["{paths.app.root.path}", "x"]),
            "/{paths.app.root.path}/x"
        );
    }

    /// Upstream `react-router.test.ts`, "route declaration boundaries (#1348)":
    /// nested and index JSX routes, long attributes and markup that only looks
    /// like a route.
    #[test]
    fn jsx_routes_keep_their_attributes_local() {
        let source = format!(
            r#"
      import {{ Routes, Route }} from 'react-router-dom';
      function DashboardHome() {{ return null; }}
      function Settings() {{ return null; }}
      function Shell() {{ return null; }}
      const comparison = count<limit;
      const fake = '<Route path="/fake" element={{<DashboardHome/>}}/>';
      export function App() {{
        return <Routes>
          <Route path="/dashboard">
            <Route index element={{<DashboardHome/>}}/>
            <Route path="settings" element={{<Settings/>}}/>
          </Route>
          <Route path="/empty"></Route>
          <Route element={{<Settings/>}} path="/sibling"/>
          <Route element={{<Shell title="a > b"><Settings path="/nested"/></Shell>}}
            check={{/}}/.test('}}')}}
            handle={{{{ text: 'path="/borrowed"', nested: {{ element: <DashboardHome/> }} }}}}
            title="{}" path="/long"/>
          <Route path="/no-element" handle={{{{ element: <DashboardHome/> }}}}/>
          <Route component={{Settings}} path="/legacy"/>
        </Routes>;
      }}
    "#,
            "x".repeat(600)
        );
        for file in ["App.tsx", "App.jsx", "App.js"] {
            assert_eq!(
                scanned(&source, file),
                (
                    strings(&[
                        "/dashboard",
                        "/dashboard/settings",
                        "/empty",
                        "/legacy",
                        "/long",
                        "/no-element",
                        "/sibling"
                    ]),
                    strings(&[
                        "/dashboard->DashboardHome",
                        "/dashboard/settings->Settings",
                        "/legacy->Settings",
                        "/long->Shell",
                        "/sibling->Settings"
                    ])
                ),
                "{file}"
            );
        }
    }

    /// Upstream: a data router pairs only an object's own properties, in
    /// either order, quoted or not, past comments.
    #[test]
    fn route_objects_pair_only_their_own_properties() {
        let source = format!(
            r#"
      import {{ createBrowserRouter }} from 'react-router-dom';
      function DataIndex() {{ return null; }}
      function DataSettings() {{ return null; }}
      const routes = createBrowserRouter([
        {{ path: '/data', children: [
          {{ index: true, Component: DataIndex }},
          {{ Component: DataSettings, path: 'prefs' }}
        ] }},
        {{ path: '/empty' }},
        {{ Component: DataSettings, path: '/sibling' }},
        {{ path: '/metadata', handle: {{ Component: DataIndex }} }},
        {{ Component: DataSettings, handle: {{ path: '/not-own' }} }},
        {{ path: '/long', handle: {{ text: '{}' }}, Component: DataSettings }},
        {{ 'Component': DataSettings, /* path: '/fake' */ 'path': '/quoted' /* trailing comment */ }},
        {{ path: '', Component: DataSettings }}
      ]);
    "#,
            "x".repeat(600)
        );
        for file in ["App.tsx", "App.jsx", "App.ts", "App.js"] {
            assert_eq!(
                scanned(&source, file),
                (
                    strings(&["/", "/data", "/data/prefs", "/long", "/quoted", "/sibling"]),
                    strings(&[
                        "/->DataSettings",
                        "/data->DataIndex",
                        "/data/prefs->DataSettings",
                        "/long->DataSettings",
                        "/quoted->DataSettings",
                        "/sibling->DataSettings"
                    ])
                ),
                "{file}"
            );
        }
    }

    /// Upstream: what a route renders, past a `fallback`, a guard, a page
    /// handed to a guard as a prop, and line breaks.
    #[test]
    fn a_route_renders_the_page_inside_its_guards() {
        let source = r#"
      import { Suspense } from 'react';
      import { Routes, Route } from 'react-router-dom';
      export function App() {
        return <Routes>
          <Route path="/admin" element={<Suspense fallback={<Loader />}><AdminPanel /></Suspense>} />
          <Route
            path="/protected"
            element={
              <RequireAuth>
                <ProtectedPage />
              </RequireAuth>
            }
          />
          <Route path="/home" element={<PrivateRoot component={<HomePage />} />} />
          <Route path="/login" element={<Suspense fallback={<Loader />}><AuthPage /></Suspense>} />
          <Route path="/signin" element={<AuthPage type="login" rememberMe={<RememberMe />} />} />
        </Routes>;
      }
    "#;
        for file in ["App.tsx", "App.jsx", "App.js"] {
            assert_eq!(
                scanned(source, file).1,
                strings(&[
                    // A `fallback` is shown while the page loads; it is not the page.
                    "/admin->AdminPanel",
                    // A page handed to a guard as a prop.
                    "/home->HomePage",
                    // `AuthPage` reads like a guard's name; when every tag does,
                    // the innermost is the page.
                    "/login->AuthPage",
                    // A long element on lines of its own.
                    "/protected->ProtectedPage",
                    // Only a `component`, `element` or `page` prop hands over a page.
                    "/signin->AuthPage"
                ]),
                "{file}"
            );
        }
    }

    /// Upstream: an index route is a page only where its parent's address is
    /// written down; a path in braces is still written down.
    #[test]
    fn an_index_route_needs_its_parents_address() {
        let source = r#"
      import { Routes, Route } from 'react-router-dom';
      import { paths } from './paths';
      function Shop() {
        return <Routes>
          <Route index element={<CategoriesPreview />} />
          <Route path=":category" element={<Category />} />
        </Routes>;
      }
      export function App() {
        return <Routes>
          <Route path="/" element={<Navigation />}>
            <Route index element={<Home />} />
            <Route path="shop/*" element={<Shop />} />
            <Route path={paths.agents}>
              <Route index element={<ListAgents />} />
            </Route>
            <Route path={"workspaces"}>
              <Route index element={<ListWorkspaces />} />
            </Route>
          </Route>
        </Routes>;
      }
    "#;
        let (paths, bindings) = scanned(source, "App.jsx");
        assert_eq!(
            paths.iter().filter(|path| *path == "/").count(),
            1,
            "{paths:?}"
        );
        assert!(bindings.contains(&"/->Home".to_string()), "{bindings:?}");
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.contains("CategoriesPreview")
                    || binding.contains("ListAgents")),
            "{bindings:?}"
        );
        assert!(
            bindings.contains(&"/workspaces->ListWorkspaces".to_string()),
            "{bindings:?}"
        );
        assert!(
            bindings.contains(&"/workspaces~>Navigation".to_string()),
            "{bindings:?}"
        );
    }

    /// Upstream: an index route at the top of the router itself is `/`, in a
    /// `<BrowserRouter>` and in `createRoutesFromElements`.
    #[test]
    fn an_index_route_at_the_top_of_a_router_is_root() {
        let source = r#"
      import { BrowserRouter, createBrowserRouter, createRoutesFromElements, Routes, Route } from 'react-router-dom';
      export function App() {
        return <BrowserRouter>
          <Routes>
            <Route index element={<Home />} />
            <Route path="about" element={<About />} />
          </Routes>
        </BrowserRouter>;
      }
      export const router = createBrowserRouter(createRoutesFromElements(<Route index element={<Dashboard />} />));
    "#;
        for file in ["App.tsx", "App.jsx"] {
            assert_eq!(
                scanned(source, file),
                (
                    strings(&["/", "/", "/about"]),
                    strings(&["/->Dashboard", "/->Home", "/about->About"])
                ),
                "{file}"
            );
        }
    }

    /// Upstream: a version 5 catch-all (`path=""` with a `component`, or no
    /// path at all) is no route, and a version 3 parent route is the layout
    /// around its children as well as the page at its own address.
    #[test]
    fn a_catch_all_is_no_route_and_a_parent_route_is_a_layout() {
        let source = r#"
      import { Router, Route, Switch } from 'react-router';
      export const routes = (
        <Router>
          <Route path="/" component={App}>
            <Route path="about" component={About} />
          </Route>
          <Switch>
            <Route path="" component={NotFound} />
            <Route component={NotFound} />
          </Switch>
        </Router>
      );
    "#;
        assert_eq!(
            scanned(source, "App.jsx"),
            (
                strings(&["/", "/about"]),
                strings(&["/->App", "/about->About", "/about~>App"])
            )
        );
    }

    /// Upstream: nested JSX and comma-holding expressions stay inside their
    /// data-router property.
    #[test]
    fn a_route_property_keeps_its_nested_expressions() {
        let source = r#"
      import { createMemoryRouter } from 'react-router-dom';
      const router = createMemoryRouter([
        { element: <Shell title="a > b"><Child path="/fake"/>hello, world</Shell>,
          handle: { text: "}, path: '/fake'", callback: () => ({ path: '/also-fake' }) }, path: '/shell' },
        { path: '/none', handle: { element: <Child/> } },
        { path: '/child', element: <Child/> }
      ]);
    "#;
        assert_eq!(
            scanned(source, "App.tsx"),
            (
                strings(&["/child", "/shell"]),
                strings(&["/child->Child", "/shell->Shell"])
            )
        );
    }

    /// Upstream `react-router-outlet-routes.test.ts`, the scanner's half: a
    /// route that renders `<Outlet />` (`element: <Outlet />`, `Component:
    /// Outlet`) renders nothing of its own and is no layout; at its parent's
    /// address (`path: ''`) it only groups the routes inside it.
    #[test]
    fn an_outlet_route_renders_nothing_of_its_own() {
        let source = r#"
import { createBrowserRouter, Outlet } from 'react-router-dom';
import { Shell } from './Shell';

export const router = createBrowserRouter([
  {
    element: <Shell />,
    children: [
      {
        path: '/regions',
        element: <Outlet />,
        children: [
          { path: '', lazy: () => import('./pages/region-list') },
          { path: 'create', lazy: () => import('./pages/region-create') },
        ],
      },
      {
        path: '/products',
        Component: Outlet,
        children: [{ index: true, lazy: () => import('./pages/product-list') }],
      },
      { path: '/approvals', element: <Outlet /> },
      { path: '/agents/:agentId', Component: Outlet },
    ],
  },
]);
"#;
        assert_eq!(
            scanned(source, "src/router.tsx"),
            (
                strings(&[
                    "/agents/:agentId",
                    "/approvals",
                    "/products",
                    "/regions",
                    "/regions/create"
                ]),
                strings(&[
                    "/agents/:agentId~>Shell",
                    "/approvals~>Shell",
                    "/products->lazy-import:./pages/product-list",
                    "/products~>Shell",
                    "/regions->lazy-import:./pages/region-list",
                    "/regions/create->lazy-import:./pages/region-create",
                    "/regions/create~>Shell",
                    "/regions~>Shell"
                ])
            )
        );
        // React Router's error-boundaries example: a `path: ''` route that
        // renders an <Outlet /> only to hold an errorElement, at its parent's
        // address, where Layout stays the page.
        let errors = r#"
import { createBrowserRouter, Outlet } from 'react-router-dom';
export const router = createBrowserRouter([
  {
    path: '/',
    element: <Layout />,
    children: [
      {
        path: '',
        element: <Outlet />,
        errorElement: <RootErrorBoundary />,
        children: [{ path: 'projects/:projectId', element: <Project /> }],
      },
    ],
  },
]);
"#;
        assert_eq!(
            scanned(errors, "src/errors/router.tsx"),
            (
                strings(&["/", "/projects/:projectId"]),
                strings(&[
                    "/->Layout",
                    "/projects/:projectId->Project",
                    "/projects/:projectId~>Layout"
                ])
            )
        );
    }

    /// KEEP-RUST: the scanner's two recursive entries, `unit` and `jsx`,
    /// share one bound of `MAX_SCAN_DEPTH` nested units. A file read within
    /// it yields its routes; a unit past it stops the scan, and the file
    /// yields none, the routes read before the cut included. Nothing
    /// overflows the stack. (`js_scan`'s stackless helpers have no bound.)
    #[test]
    fn a_file_nested_past_the_scan_bound_yields_no_routes() {
        let nested = |arrays: usize| {
            format!(
                "<Route path=\"/before\" element={{<Before />}} />\ncreateBrowserRouter({}{{ path: '/deep', element: <Deep /> }}{});\n<Route path=\"/after\" element={{<After />}} />\n",
                "[".repeat(arrays),
                "]".repeat(arrays)
            )
        };
        // The call's paren is one unit, each array one, the object one, its
        // `element` value one and the `<Deep />` inside it one: four levels
        // around the arrays, so this file reaches the bound exactly.
        assert_eq!(
            scanned(&nested(MAX_SCAN_DEPTH - 4), "App.tsx").0,
            strings(&["/after", "/before", "/deep"])
        );
        assert!(scanned(&nested(MAX_SCAN_DEPTH - 3), "App.tsx").0.is_empty());
        assert!(scanned(&nested(100_000), "App.tsx").0.is_empty());
        let jsx = |depth: usize| {
            format!(
                "<Route path=\"/before\" element={{<Before />}} />\n<BrowserRouter>{}<Route path=\"/x\" element={{<X />}} />{}</BrowserRouter>",
                "<div>".repeat(depth),
                "</div>".repeat(depth)
            )
        };
        // An element read as a unit is two levels, the unit and the element;
        // a child element is one. So: the router two, each div one, the
        // `<Route>` one, its `element` braces one and the `<X />` in them two.
        assert_eq!(
            scanned(&jsx(MAX_SCAN_DEPTH - 6), "App.tsx").0,
            strings(&["/before", "/x"])
        );
        assert!(scanned(&jsx(MAX_SCAN_DEPTH - 5), "App.tsx").0.is_empty());
        assert!(scanned(&jsx(100_000), "App.tsx").0.is_empty());
    }

    /// A `lazy` loader names the module it hands back (#2480), and one the
    /// loader readers cannot read the module its first `import(…)` loads.
    #[test]
    fn a_lazy_loader_names_the_module_it_hands_back() {
        assert_eq!(
            lazy_route_reference(Some("() => import('./routes/landing')")).as_deref(),
            Some("lazy-import:./routes/landing")
        );
        assert_eq!(
            lazy_route_reference(Some(
                "async () => { const { loader } = await import(\"./a\"); return import('./b'); }"
            ))
            .as_deref(),
            Some("lazy-import:./b")
        );
        assert_eq!(
            lazy_route_reference(Some("() => helper(import('./c'))")).as_deref(),
            Some("lazy-import:./c")
        );
        assert_eq!(lazy_route_reference(Some("lazyPage(load)")), None);
        assert_eq!(lazy_route_reference(None), None);
    }

    /// The nodes and references extraction gives a route, as upstream's
    /// `extract` builds them: tsx in a `.tsx` file and jsx anywhere else, the
    /// path's constant parts on the signature.
    #[test]
    fn a_route_node_is_named_by_its_path_and_references_what_it_renders() {
        let content = "import { createBrowserRouter } from 'react-router';\n\
                       export const router = createBrowserRouter([\n\
                       \x20 { path: paths.app.root.path, element: <AppRoot />,\n\
                       \x20   children: [{ path: 'discussions', lazy: () => import('./routes/d') }] },\n\
                       ]);\n";
        let result = extract_routes("src/router.ts", content);
        let names: Vec<(&str, &str, i64, Language)> = result
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.as_str(),
                    node.name.as_str(),
                    node.start_line,
                    node.language,
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (
                    "route:src/router.ts:3:/{paths.app.root.path}",
                    "/{paths.app.root.path}",
                    3,
                    Language::Jsx
                ),
                (
                    "route:src/router.ts:4:/{paths.app.root.path}/discussions",
                    "/{paths.app.root.path}/discussions",
                    4,
                    Language::Jsx
                ),
            ]
        );
        assert_eq!(
            result.nodes[1].signature.as_deref(),
            Some(r#"route-parts:[{"expr":"paths.app.root.path"},{"lit":"discussions"}]"#)
        );
        assert_eq!(
            result.nodes[1].qualified_name,
            "src/router.ts::route:/{paths.app.root.path}/discussions"
        );
        let references: Vec<(&str, &str, i64, Language)> = result
            .references
            .iter()
            .map(|r| {
                (
                    r.from_node_id.as_str(),
                    r.reference_name.as_str(),
                    r.line,
                    r.language,
                )
            })
            .collect();
        assert_eq!(
            references,
            [
                (
                    "route:src/router.ts:3:/{paths.app.root.path}",
                    "AppRoot",
                    3,
                    Language::Jsx
                ),
                (
                    "route:src/router.ts:4:/{paths.app.root.path}/discussions",
                    "lazy-import:./routes/d",
                    4,
                    Language::Jsx
                ),
                (
                    "route:src/router.ts:4:/{paths.app.root.path}/discussions",
                    "layout:AppRoot",
                    4,
                    Language::Jsx
                ),
            ]
        );
        assert!(
            result
                .references
                .iter()
                .all(|r| r.reference_kind == EdgeKind::References && r.column == 0)
        );
        // A `.ts` file parses no JSX: only a router file's route objects.
        assert!(
            extract_routes("src/App.ts", "<Route path=\"/x\" element={<X/>}/>")
                .nodes
                .is_empty()
        );
        assert!(
            extract_routes("src/App.tsx", "const x = 1;\n")
                .nodes
                .is_empty()
        );
    }

    /// A constant node over `lines` of `file`.
    fn constant(name: &str, file: &str, start_line: i64, end_line: i64) -> Node {
        framework_node(
            format!("constant:{file}:{name}"),
            NodeKind::Constant,
            name.to_string(),
            name.to_string(),
            file.to_string(),
            start_line,
            end_line,
            0,
            0,
            Language::TypeScript,
            true,
        )
    }

    /// Upstream `constantPathValue` and `postExtract`: a path built from a
    /// constant is named by reading the constant's object literal, and the
    /// route keeps the id it was extracted with.
    #[test]
    fn a_constant_path_is_read_from_its_object_literal() {
        let router = "export const paths = {\n\
                      \x20 home: { path: '/', getHref: () => '/' },\n\
                      \x20 app: {\n\
                      \x20   root: { path: '/app', getHref: () => '/app' },\n\
                      \x20   'discussions': { path: 'discussions', getHref: () => '/app/discussions' },\n\
                      \x20 },\n\
                      } as const;\n\
                      export const router = createBrowserRouter([\n\
                      \x20 { path: paths.app.root.path, element: <AppRoot />,\n\
                      \x20   children: [{ path: paths.app.discussions.path, element: <Discussions /> }] },\n\
                      \x20 { path: paths.missing.path, element: <Missing /> },\n\
                      ]);\n";
        let file = "src/router.tsx";
        let extracted = extract_routes(file, router);
        let mut nodes = extracted.nodes.clone();
        nodes.push(constant("paths", file, 1, 7));
        let ctx = RouteCtx::default().file(file, router).routes(file, nodes);
        assert_eq!(
            constant_path_value("paths.app.root.path", file, &ctx).as_deref(),
            Some("/app")
        );
        assert_eq!(
            constant_path_value("paths.app.discussions.path", file, &ctx).as_deref(),
            Some("discussions")
        );
        assert_eq!(constant_path_value("paths.home.getHref", file, &ctx), None);
        assert_eq!(constant_path_value("paths", file, &ctx), None);
        let renamed: Vec<(String, String)> = rename_constant_routes(&ctx)
            .into_iter()
            .map(|node| (node.id, node.name))
            .collect();
        assert_eq!(
            renamed,
            [
                (
                    "route:src/router.tsx:9:/{paths.app.root.path}".to_string(),
                    "/app".to_string()
                ),
                (
                    "route:src/router.tsx:10:/{paths.app.root.path}/{paths.app.discussions.path}"
                        .to_string(),
                    "/app/discussions".to_string()
                ),
            ],
            "a path one of whose constants is missing keeps its extracted name"
        );
    }

    /// The plan's `$` case: upstream finds the declaration's name with an
    /// unescaped pattern, which never matches a name holding `$`, and then
    /// searches the declaration from its start for the object literal.
    #[test]
    fn a_constant_named_with_a_dollar_is_read_from_its_declarations_start() {
        let source = "export const $paths = { app: { path: '/app' } };\n";
        let file = "src/paths.tsx";
        let ctx = RouteCtx::default()
            .file(file, source)
            .routes(file, vec![constant("$paths", file, 1, 1)]);
        assert_eq!(
            constant_path_value("$paths.app.path", file, &ctx).as_deref(),
            Some("/app")
        );
        assert_eq!(named_at("const $paths = {", "$paths"), None);
        assert_eq!(named_at("const paths = {", "paths"), Some(6));
        assert_eq!(named_at("const xpaths = paths", "paths"), Some(15));
    }

    /// Upstream #2187 `configHrefExpression`: the href a route-config object
    /// names, a function's returned literal included, a glued hole dropped.
    #[test]
    fn a_route_config_names_the_href_it_returns() {
        let source = "export const paths = {\n\
                      \x20 auth: {\n\
                      \x20   login: {\n\
                      \x20     path: '/auth/login',\n\
                      \x20     getHref: (redirectTo?: string | null | undefined) =>\n\
                      \x20       `/auth/login${redirectTo ? `?redirectTo=${encodeURIComponent(redirectTo)}` : ''}`,\n\
                      \x20   },\n\
                      \x20 },\n\
                      \x20 app: {\n\
                      \x20   discussion: { path: 'discussions/:discussionId', getHref: (id: string) => `/app/discussions/${id}` },\n\
                      \x20   home: { getHref: () => { return '/app'; } },\n\
                      \x20   count: { getHref: () => 3 },\n\
                      \x20 },\n\
                      } as const;\n";
        let file = "src/config/paths.ts";
        let ctx = RouteCtx::default()
            .file(file, source)
            .routes(file, vec![constant("paths", file, 1, 14)]);
        let href = |expr: &str| config_href_expression(expr, file, &ctx);
        assert_eq!(
            href("paths.auth.login.getHref(location.pathname)").as_deref(),
            Some("`/auth/login`")
        );
        assert_eq!(
            href("paths.auth.login.path").as_deref(),
            Some("'/auth/login'")
        );
        assert_eq!(
            href("paths.app.discussion.getHref(d.id)").as_deref(),
            Some("`/app/discussions/${id}`")
        );
        assert_eq!(href("paths?.app.home.getHref()").as_deref(), Some("'/app'"));
        assert_eq!(href("paths.app.count.getHref()"), None);
        assert_eq!(href("paths.app.missing.getHref()"), None);
        assert_eq!(href("getHref()"), None);
        assert_eq!(
            drop_glued_template_holes("`/a/${id}/b${q}`"),
            "`/a/${id}/b`",
            "a whole-segment hole stays, a glued one goes"
        );
        assert_eq!(drop_glued_template_holes("`/a/${id}`"), "`/a/${id}`");
        assert_eq!(
            drop_glued_template_holes("`/a/${id}?x=1`"),
            "`/a/${id}?x=1`"
        );
    }

    /// Upstream's `^(["'])((?:\\.|(?!\1).)*)\1` backtracks: a backslash before
    /// the closing quote ends the literal when nothing later does.
    #[test]
    fn a_string_literal_is_read_as_upstreams_pattern_reads_it() {
        assert_eq!(leading_string("'/app', x").as_deref(), Some("/app"));
        assert_eq!(leading_string(r#""a\"b" + c"#).as_deref(), Some(r#"a\"b"#));
        assert_eq!(leading_string(r"'a\'").as_deref(), Some(r"a\"));
        assert_eq!(leading_string("'a\\'\nb'"), Some(r"a\".to_string()));
        assert_eq!(leading_string("'a\nb'"), None);
        assert_eq!(leading_string("`a`"), None);
        assert_eq!(literal(" 'team' ").as_deref(), Some("team"));
        assert_eq!(literal(r#""a\"b""#).as_deref(), Some(r#"a\"b"#));
        assert_eq!(literal("'a'b'"), None);
        assert_eq!(literal(r"'a\'"), None);
        assert_eq!(literal("paths.home"), None);
    }

    /// Upstream `react-router.test.ts`, `reactRouterRoot` and
    /// `reactRouterNavVerb`.
    #[test]
    fn a_route_file_names_its_app_and_a_call_its_verb() {
        for (file, root) in [
            ("frontend/src/App.js", "frontend/"),
            ("src/App.tsx", ""),
            ("apps/web/src/routes/index.tsx", "apps/web/"),
            ("client/App.jsx", "client/"),
            ("App.jsx", ""),
            // An empty segment ends the prefix upstream's pattern can match.
            ("a//src/App.jsx", "a//src/"),
        ] {
            assert_eq!(react_router_root(file), root, "{file}");
        }
        for (name, verb) in [
            ("history.push", "push"),
            ("history.replace", "replace"),
            ("navigate", "navigate"),
            ("router.navigate", "navigate"),
            ("redirect", "redirect"),
        ] {
            assert_eq!(react_router_nav_verb(name), Some(verb), "{name}");
            assert!(ReactRouterResolver.claims_reference(name));
        }
        for name in [
            "push",
            "replace",
            "paths.push",
            "list.replace",
            "items.navigate",
            "go",
            "goBack",
        ] {
            assert_eq!(react_router_nav_verb(name), None, "{name}");
            assert!(!ReactRouterResolver.claims_reference(name));
        }
    }

    /// A route node as React's extraction makes it.
    fn route(file: &str, line: i64, path: &str) -> Node {
        framework_node(
            format!("route:{file}:{line}:{path}"),
            NodeKind::Route,
            path.to_string(),
            format!("{file}::route:{path}"),
            file.to_string(),
            line,
            line,
            0,
            0,
            route_language(file),
            false,
        )
    }

    /// proshop's routes (upstream `react-router.test.ts`, end to end).
    fn proshop() -> RouteCtx {
        let app = "frontend/src/App.js";
        RouteCtx::default().routes(
            app,
            vec![
                route(app, 10, "/login"),
                route(app, 11, "/shipping"),
                route(app, 12, "/payment"),
                route(app, 13, "/placeorder"),
                route(app, 14, "/product/:id"),
                route(app, 15, "/cart/:id?"),
            ],
        )
    }

    fn resolve(ctx: &RouteCtx, file: &str, name: &str, line: i64) -> Option<ResolvedRef> {
        let mut reference = call_in(file, name, line, 0);
        reference.language = Language::Jsx;
        ReactRouterResolver.resolve(&reference, ctx)
    }

    fn target(ctx: &RouteCtx, resolved: &ResolvedRef) -> String {
        ctx.nodes()
            .iter()
            .find(|node| node.id == resolved.target_node_id)
            .map(|node| node.name.clone())
            .expect("a route")
    }

    /// Upstream: `history.push` and `.replace` with a literal, v6's
    /// `navigate()` with a template hole reaching the `:id` route, an
    /// optional parameter serving the bare path, each a `navigates` edge
    /// carrying the href and the verb.
    #[test]
    fn a_navigation_call_navigates_to_the_route_its_href_names() {
        let screen = "frontend/src/screens/PaymentScreen.js";
        let ctx = proshop().file(
            screen,
            "const PaymentScreen = ({ history }) => {\n\
             \x20 history.push('/shipping')\n\
             \x20 history.replace('/payment')\n\
             \x20 navigate(`/cart/${match.params.id}`)\n\
             \x20 navigate('/cart')\n\
             \x20 router.navigate('/product/7?tab=a#x')\n\
             \x20 redirect('https://shop.example/login')\n\
             }\n",
        );
        for (name, line, path, href, verb) in [
            ("history.push", 2, "/shipping", "/shipping", "push"),
            ("history.replace", 3, "/payment", "/payment", "replace"),
            ("navigate", 4, "/cart/:id?", "/cart/${…}", "navigate"),
            ("navigate", 5, "/cart/:id?", "/cart", "navigate"),
            (
                "router.navigate",
                6,
                "/product/:id",
                "/product/7?tab=a#x",
                "navigate",
            ),
            (
                "redirect",
                7,
                "/login",
                "https://shop.example/login",
                "redirect",
            ),
        ] {
            let resolved = resolve(&ctx, screen, name, line).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(target(&ctx, &resolved), path, "{name} at {line}");
            let edge = resolved.edge.expect("an edge shape");
            assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
            assert_eq!(edge.metadata["href"], href);
            assert_eq!(edge.metadata["navMethod"], verb);
            assert!(edge.also_targets.is_empty());
            assert_eq!(resolved.confidence, 0.95);
        }
    }

    /// Upstream: a computed destination is left unresolved, a local's
    /// static arm still names where it goes, every arm of a conditional is a
    /// destination of its own, and an array's push is never claimed.
    #[test]
    fn what_names_no_route_stays_unresolved() {
        let screen = "frontend/src/screens/LoginScreen.js";
        let ctx = proshop().file(
            screen,
            "const LoginScreen = ({ location, history }) => {\n\
             \x20 const redirect = location.search ? location.search.split('=')[1] : '/shipping'\n\
             \x20 history.push(redirect)\n\
             \x20 history.push(location.state.from)\n\
             \x20 history.push('/nowhere')\n\
             \x20 history.push(admin ? '/payment' : keyword ? `/product/${keyword}` : '/nowhere')\n\
             \x20 history.push('relative')\n\
             }\n",
        );
        let local = resolve(&ctx, screen, "history.push", 3).expect("the local's static arm");
        assert_eq!(target(&ctx, &local), "/shipping");
        assert_eq!(resolve(&ctx, screen, "history.push", 4), None);
        assert_eq!(resolve(&ctx, screen, "history.push", 5), None);
        let forks = resolve(&ctx, screen, "history.push", 6).expect("a conditional");
        assert_eq!(target(&ctx, &forks), "/payment");
        let edge = forks.edge.expect("an edge shape");
        let also: Vec<(String, String)> = edge
            .also_targets
            .iter()
            .map(|also| {
                (
                    ctx.nodes()
                        .iter()
                        .find(|node| node.id == also.target_node_id)
                        .expect("a route")
                        .name
                        .clone(),
                    also.metadata["href"].as_str().expect("an href").to_string(),
                )
            })
            .collect();
        assert_eq!(
            also,
            [("/product/:id".to_string(), "/product/${…}".to_string())]
        );
        assert_eq!(resolve(&ctx, screen, "history.push", 7), None);
        // `paths.push('/placeorder')` is an array's: never claimed, so never
        // handed to `resolve`; were it handed over, it is no navigation.
        assert_eq!(resolve(&ctx, screen, "paths.push", 2), None);
    }

    /// Upstream #2187: `navigate(paths.auth.login.getHref(…))` reads the href
    /// the route-config helper returns.
    #[test]
    fn a_navigation_through_a_route_config_reads_its_href() {
        let paths = "src/config/paths.ts";
        let auth = "src/lib/auth.tsx";
        let app = "src/app/router.tsx";
        let paths_source = "export const paths = {\n\
                            \x20 auth: { login: { path: '/auth/login', getHref: (to?: string) => `/auth/login${to ? `?to=${to}` : ''}` } },\n\
                            };\n";
        let auth_source = "const paths = {\n\
                           \x20 auth: { login: { getHref: (to?: string) => `/auth/login${to ? `?to=${to}` : ''}` } },\n\
                           };\n\
                           export function Logout() {\n\
                           \x20 return <button onClick={() => navigate(paths.auth.login.getHref(location.pathname))} />;\n\
                           }\n";
        let ctx = RouteCtx::default()
            .file(paths, paths_source)
            .file(auth, auth_source)
            .routes(paths, vec![constant("paths", paths, 1, 3)])
            .routes(auth, vec![constant("paths", auth, 1, 3)])
            .routes(app, vec![route(app, 4, "/auth/login")]);
        let mut call = call_in(auth, "navigate", 5, 0);
        call.language = Language::Tsx;
        let resolved = ReactRouterResolver
            .resolve(&call, &ctx)
            .expect("the login route");
        assert_eq!(target(&ctx, &resolved), "/auth/login");
        assert_eq!(resolved.edge.expect("edge").metadata["href"], "/auth/login");
    }

    /// KEEP-RUST: a path two routes of one app serve, and a bare path two
    /// optional-parameter routes shorten to, stay unresolved; each app of a
    /// monorepo answers its own calls, and a file under no app with routes is
    /// not read.
    #[test]
    fn a_path_two_routes_of_an_app_serve_names_neither() {
        let web = "apps/web/src/App.tsx";
        let admin = "apps/admin/src/App.tsx";
        let nav = "apps/web/src/Nav.tsx";
        let admin_nav = "apps/admin/src/Nav.tsx";
        let source = "navigate('/login')\nnavigate('/dup')\nnavigate('/cart')\nnavigate('/cart/5')\nnavigate('/shop')\n";
        let ctx = RouteCtx::default()
            .file(nav, source)
            .file(admin_nav, source)
            .file("tools/script.tsx", source)
            .routes(
                web,
                vec![
                    route(web, 3, "/login"),
                    route(web, 4, "/dup"),
                    route(web, 5, "/dup"),
                    route(web, 6, "/cart/:id?"),
                    route(web, 7, "/cart/:item?"),
                    route(web, 8, "/shop/:id?"),
                    route(web, 9, "/shop"),
                ],
            )
            .routes(admin, vec![route(admin, 3, "/login")]);
        let resolved = |file: &str, line: i64| {
            let mut call = call_in(file, "navigate", line, 0);
            call.language = Language::Tsx;
            ReactRouterResolver
                .resolve(&call, &ctx)
                .map(|resolved| resolved.target_node_id)
        };
        assert_eq!(resolved(nav, 1), Some(format!("route:{web}:3:/login")));
        assert_eq!(
            resolved(admin_nav, 1),
            Some(format!("route:{admin}:3:/login"))
        );
        assert_eq!(resolved(nav, 2), None, "two routes at /dup");
        assert_eq!(
            resolved(nav, 3),
            None,
            "two optional routes shorten to /cart"
        );
        assert_eq!(resolved(nav, 4), None, "two dynamic routes tie at /cart/5");
        assert_eq!(
            resolved(nav, 5),
            Some(format!("route:{web}:9:/shop")),
            "a route written at a path wins over a shortened one"
        );
        assert_eq!(resolved(admin_nav, 2), None, "the admin app has no /dup");
        assert_eq!(
            resolved("tools/script.tsx", 1),
            None,
            "no app above it has routes"
        );
    }

    /// Only React's own routes are destinations: a server route's id carries
    /// its METHOD, a file-based page carries no line, a splat matches
    /// anything, a relative path has no tree, and a renamed constant path
    /// keeps the id it was extracted with.
    #[test]
    fn only_reacts_absolute_routes_are_destinations() {
        let file = "src/App.tsx";
        let mut server = route(file, 3, "/api");
        server.id = format!("route:{file}:3:GET:/api");
        let mut page = route("app/page.tsx", 1, "/page");
        page.id = "route:app/page.tsx:/page".to_string();
        let mut renamed = route(file, 9, "/app");
        renamed.id = format!("route:{file}:9:/{{paths.app.path}}");
        renamed.signature = Some(r#"route-parts:[{"expr":"paths.app.path"}]"#.to_string());
        let mut vue = route("src/router.ts", 4, "/vue");
        vue.language = Language::TypeScript;
        assert!(is_destination(&route(file, 2, "/home")));
        assert!(!is_destination(&server));
        assert!(!is_destination(&page));
        assert!(!is_destination(&route(file, 4, "/admin/*")));
        assert!(!is_destination(&route(file, 5, "settings")));
        assert!(is_destination(&renamed));
        assert!(!is_destination(&vue));
        let ctx = RouteCtx::default().routes(file, vec![renamed.clone()]);
        assert_eq!(
            ReactRouterNavigation.scope(&renamed, &ctx),
            Some(vec![String::new()])
        );
        assert_eq!(ReactRouterNavigation.scope(&server, &ctx), None);
        assert_eq!(
            ReactRouterNavigation.tails(),
            ["push", "replace", "navigate", "redirect"]
        );
    }
}
