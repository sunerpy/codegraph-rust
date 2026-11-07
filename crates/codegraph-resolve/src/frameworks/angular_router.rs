//! Angular Router (upstream `frameworks/angular-router.ts` at `e8ddc4e2`,
//! which is `b635dd46` with `escapeRegExp` on the names its declaration
//! patterns build): routes declared in `Routes` arrays, the components they
//! render, the path each sits at once its lazily loaded file is mounted, and
//! the router's navigation calls.
//!
//! ```text
//! export const routes: Routes = [
//!   { path: 'login', component: AuthComponent },
//!   { path: 'article/:slug', loadComponent: () => import('./article.component') },
//!   { path: 'editor', children: [{ path: ':slug', loadComponent: … }] },
//!   { path: 'profile', loadChildren: () => import('./profile/profile.routes') },
//! ];
//! ```
//!
//! [`FrameworkResolver::extract`] reads every such array (typed `Routes` or
//! `Route[]`, handed to `RouterModule.forRoot/forChild(…)` or
//! `provideRouter(…)`, a routes file's `export default [...]`, or a `const`
//! that says `as Routes`) into one `route` node per screen. Angular paths
//! are relative: a child's path joins its parent's, and a `loadChildren`
//! file's routes sit under the path that lazy-loads them, which only a
//! cross-file pass knows. So extraction names a file's routes as if it were
//! mounted at `/`, keeping that in-file path in the qualified name, and
//! [`FrameworkResolver::post_extract`] renames each under its mount
//! (following an NgModule's `loadChildren` to the routing module it
//! imports, and an Nx library's barrel to the module it forwards) and reads
//! the constants a path is written with.
//!
//! A route with `children` is a layout, not a screen: its component renders
//! an outlet the children fill, and the `''` child is the screen at its
//! address. It is a screen only when no child claims that address. A
//! `redirectTo` entry and a `**` catch-all are no screens.
//!
//! A route binds to its component with a `references` edge: a class it names
//! is the class its file imports under that name, which the import strategy
//! resolves; a lazy `loadComponent` names a file, and its component is the
//! `@Component` class that file picks or exports (an `import:` reference
//! this resolver answers). Each route also references the components of the
//! layouts it renders inside (`layout:`).
//!
//! A navigation call (`this.router.navigate([...])`, `navigateByUrl`,
//! `createUrlTree`, `parseUrl`) binds to the route of its own app its
//! destination names, a `navigates` edge: a command array, a static string,
//! a route constant or the calling class's property holding one, read
//! through `.concat`. An app is an Angular workspace (`angular.json`,
//! `nx.json`) or a conventional app root, and a redirect makes its path the
//! route it leads to.
//!
//! KEEP-RUST:
//! - `children` and a constant's nested object are read only where they are
//!   written in place. Upstream takes the first `[` (or `{`) after the key,
//!   so `children: CHILDREN` read the array of a later entry, and a
//!   `path: paths.account.path` whose `account` names another constant read
//!   a later object.
//! - A routes file in a cycle of mounts takes the prefixes of its own chain
//!   up to the first file that repeats. Upstream memoizes the prefixes in the
//!   order it asks for them, so in a cycle a file's name depended on which
//!   route was renamed first.
//! - The route table does not depend on node or file order: a path two
//!   routes of an app serve stays unresolved, and a redirect is followed
//!   through the others at most four hops, a path whose redirects lead to two
//!   routes reaching neither. Upstream keeps the first route or redirect of a
//!   path in node and file order.
//! - A destination is read whole where upstream reads its literal head:
//!   `'/editor/' + slug` only when the literal ends with `/` and one operand
//!   follows, a conditional or template whole, a relative or off-site arm
//!   (`//host/…`) dropped before matching; a route or redirect with an
//!   unread constant is in no table.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{
    JS_SPACE, ObjectField, is_js_space, js_trim, match_bracket, read_fields, skip_string,
    top_level_objects,
};
use super::route_href::{
    ExactTable, HOLE, HrefLiteral, LinkRoutes, Literal, RouteEntry, app_root_for,
    destinations_for_href, first_argument_text, nth_argument_text, parse_href, read_string_at,
    to_href,
};
use super::sveltekit_router::leaves_the_site;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver, NavigationCalls};
use crate::import_resolver::resolve_import_path;
use crate::strip_comments::strip_js_comments_keeping_offsets;
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// How deep `children` nest before the walk stops (upstream's `depth > 12`).
const MAX_CHILD_DEPTH: usize = 12;

/// How many barrels a `loadChildren` import is followed through
/// (`routeFilesLoadedBy`'s four hops).
const MAX_BARREL_HOPS: usize = 4;

/// How many re-exports a constant's declaration is followed through
/// (`declarationSource`'s three hops).
const MAX_DECLARATION_HOPS: usize = 3;

/// How many local aliases a constant chain is followed through before the
/// read gives up (KEEP-RUST: upstream follows them without a bound, so
/// `const a = a.b;` recursed until the stack ran out).
const MAX_ALIAS_HOPS: usize = 8;

/// The fields that make an object a route rather than any object in an
/// array (`ROUTE_KEYS`).
const ROUTE_KEYS: [&str; 5] = [
    "component",
    "loadComponent",
    "children",
    "loadChildren",
    "redirectTo",
];

/// A route's reference to a lazily loaded component: `import:<spec>#<member>`
/// (`#default` for the file's default export).
const LAZY_PREFIX: &str = "import:";

/// A route's reference to a layout it renders inside: `layout:<component>`.
const LAYOUT_PREFIX: &str = "layout:";

/// The id suffix every route this resolver emits carries.
const ROUTE_ID_SUFFIX: &str = ":angular";

/// The marker between a route's file and its in-file path in its qualified
/// name.
const QUALIFIED_MARKER: &str = "::route:";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("angular-router pattern")
}

/// A file that could hold a routes array: the cheap gate before scanning
/// (`ROUTER_IMPORT`).
fn router_import() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(r#"['"]@angular/router['"]"#))
}

/// Where a routes array opens (`ARRAY_OPENERS`): typed `Routes` / `Route[]`,
/// handed to `RouterModule.forRoot/forChild` or `provideRouter`, a routes
/// file's default export, or a `const` that says `as Routes` after its
/// close. `children: [` is reached by walking an entry, never here.
fn array_openers() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"(?::{s}*(?:Routes|Route{s}*\[{s}*\]){s}*={s}*|(?-u:\b)RouterModule{s}*\.{s}*for(?:Root|Child){s}*\({s}*|(?-u:\b)provideRouter{s}*\({s}*|(?-u:\b)export{s}+default{s}+(?:<{s}*Routes{s}*>{s}*)?|(?-u:\b)(?:const|let){s}+[A-Za-z_$][A-Za-z0-9_$]*{s}*={s}*)\[",
            s = JS_SPACE
        ))
    })
}

/// `const x = [`, which opens a routes array only when it says so after its
/// close.
fn declares_binding() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(r"^(?-u:\b)(?:const|let)(?-u:\b)"))
}

/// `] as Routes` / `] satisfies Routes`.
fn says_routes() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^\]{s}*(?:as|satisfies){s}+Routes(?-u:\b)",
            s = JS_SPACE
        ))
    })
}

/// A path value the cross-file pass can name: `internalRoutes.account.path`
/// (`CONSTANT_PATH`).
fn constant_path() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^[A-Za-z_$][A-Za-z0-9_$]*(?:{s}*\.{s}*[A-Za-z_$][A-Za-z0-9_$]*)+$",
            s = JS_SPACE
        ))
    })
}

/// The `$localize` tagged template's own text: its body (`localizeDefault`).
fn localize_template() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^\$localize{s}*`((?:[^`\\$]|\\[^\n\r\x{{2028}}\x{{2029}}])*)`$",
            s = JS_SPACE
        ))
    })
}

/// The leading `:meaning|description@@id:` block of a `$localize` text.
fn localize_metadata() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(r"^:[^:]*:"))
}

/// A `${…}` hole of a template path.
fn template_hole() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(r"\$\{([^}]*)\}"))
}

/// A `{expr}` placeholder inside a segment: `:{ProjectConst.IssueId}`.
fn segment_placeholder() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(r"\{([^{}]+)\}"))
}

/// `component: AuthComponent`: a class in scope.
fn component_class_name() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(&format!(r"^{s}*([A-Z][A-Za-z0-9_$]*){s}*$", s = JS_SPACE)))
}

/// `import('./x')`, the specifier in any of the three quotes.
fn dynamic_import() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r#"(?-u:\b)import{s}*\({s}*(?:'([^'"`]+)'|"([^'"`]+)"|`([^'"`]+)`){s}*\)"#,
            s = JS_SPACE
        ))
    })
}

/// `.then(m => m.X)` after an `import(…)`: the parameter and the member the
/// arrow takes off what it is called with.
fn then_member() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^{s}*\.then{s}*\({s}*\(?{s}*([A-Za-z_$][A-Za-z0-9_$]*){s}*\)?{s}*=>{s}*\(?{s}*([A-Za-z_$][A-Za-z0-9_$]*){s}*\.{s}*([A-Za-z_$][A-Za-z0-9_$]*)",
            s = JS_SPACE
        ))
    })
}

/// `(await import('./x')).HomeModule`: the member after the closing paren.
fn awaited_member() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^{s}*\){s}*\.{s}*([A-Za-z_$][A-Za-z0-9_$]*)",
            s = JS_SPACE
        ))
    })
}

/// `@Component(` (`COMPONENT_CLASS`).
fn component_decorator() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(&format!(r"@Component{s}*\(", s = JS_SPACE)))
}

/// The class a `@Component(…)` decorates, right after it.
fn decorated_class() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^{s}*(export{s}+)?(default{s}+)?(?:abstract{s}+)?class{s}+([A-Za-z_$][A-Za-z0-9_$]*)",
            s = JS_SPACE
        ))
    })
}

/// `export default class X`, decorated or not.
fn default_class() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"(?-u:\b)export{s}+default{s}+class{s}+([A-Za-z_$][A-Za-z0-9_$]*)",
            s = JS_SPACE
        ))
    })
}

/// The ` as ` between an imported name and its local one.
fn import_alias() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(&format!(r"{s}+as{s}+", s = JS_SPACE)))
}

/// `import { A, B as C } from '…'`.
fn named_import() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r#"(?-u:\b)import{s}*\{{([^}}]*)\}}{s}*from{s}*(?:'([^'"]+)'|"([^'"]+)")"#,
            s = JS_SPACE
        ))
    })
}

/// `@NgModule(`.
fn ng_module() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(&format!(r"@NgModule{s}*\(", s = JS_SPACE)))
}

/// Any import statement with a `from` specifier.
fn import_from() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r#"(?-u:\b)import{s}+[^'"]*?from{s}+(?:'([^'"]+)'|"([^'"]+)")"#,
            s = JS_SPACE
        ))
    })
}

/// `export * from '…'`, `export { … } from '…'`, `export * as X from '…'`.
fn re_export() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r#"(?-u:\b)export{s}+(?:\*|\{{[^}}]*\}}){s}*(?:as{s}+[A-Za-z0-9_$]+{s}*)?from{s}+(?:'([^'"]+)'|"([^'"]+)")"#,
            s = JS_SPACE
        ))
    })
}

/// `export * from '…'` and `export { … } from '…'`, what they export kept
/// (`declarationSource`'s pattern).
fn re_export_clause() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r#"(?-u:\b)export{s}+(\*|\{{[^}}]*\}}){s}*from{s}+(?:'([^'"]+)'|"([^'"]+)")"#,
            s = JS_SPACE
        ))
    })
}

/// `const { a, b: c } = x.y;`.
fn destructuring() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"(?-u:\b)(?:const|let){s}*\{{([^}}]*)\}}{s}*={s}*([A-Za-z_$][A-Za-z0-9_$]*(?:{s}*\.{s}*[A-Za-z_$][A-Za-z0-9_$]*)*){s}*;?",
            s = JS_SPACE
        ))
    })
}

/// The first capture of a quote alternation (`(?:'(…)'|"(…)"|`(…)`)`) that
/// matched.
fn first_capture<'t>(caps: &regex::Captures<'t>, from: usize) -> Option<&'t str> {
    (from..caps.len()).find_map(|i| caps.get(i).map(|m| m.as_str()))
}

/// `text` without its JavaScript white space (`.replace(/\s+/g, '')`).
fn without_space(text: &str) -> String {
    text.chars().filter(|c| !is_js_space(*c)).collect()
}

/// `s[from..]` up to `chars` characters (a window upstream takes in UTF-16
/// code units; the bytes it compares are ASCII).
fn window(s: &str, from: usize, chars: usize) -> &str {
    let rest = &s[from.min(s.len())..];
    let end = rest
        .char_indices()
        .nth(chars)
        .map_or(rest.len(), |(i, _)| i);
    &rest[..end]
}

/// The 1-based line of byte `at` in `text`.
fn line_of(text: &str, at: usize) -> i64 {
    let newlines = text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count();
    i64::try_from(newlines + 1).unwrap_or(i64::MAX)
}

/// The byte offset of `part`, a slice of `whole`, in `whole`.
fn offset_in(whole: &str, part: &str) -> usize {
    part.as_ptr() as usize - whole.as_ptr() as usize
}

/// The field of `fields` with `key`.
fn field<'f, 's>(fields: &'f [ObjectField<'s>], key: &str) -> Option<&'f ObjectField<'s>> {
    fields.iter().find(|field| field.key == key)
}

/// Where the bracket that opens a field's value sits, when the value is
/// that bracket written in place: `children: [` but not `children: kids`.
fn bracket_written_at(s: &str, field: &ObjectField<'_>, open: u8) -> Option<usize> {
    let value = offset_in(s, field.text);
    let start = value + (field.text.len() - field.text.trim_start_matches(is_js_space).len());
    (s.as_bytes().get(start) == Some(&open)).then_some(start)
}

/// A `$localize` tagged template's default text:
/// `` $localize`:kebab-case@@routes.about:about` `` is `about`
/// (`localizeDefault`).
pub(crate) fn localize_default(text: &str) -> Option<String> {
    let caps = localize_template().captures(js_trim(text))?;
    let body = caps.get(1).map_or("", |m| m.as_str());
    Some(match localize_metadata().find(body) {
        Some(meta) => body[meta.end()..].to_string(),
        None => body.to_string(),
    })
}

/// A string written statically: a literal, a `$localize` default, a
/// template with no hole, or `+` between those (`'/' + $localize`…``, as
/// Ghostfolio builds its links); `None` when any part is computed
/// (`staticString`).
pub(crate) fn static_string(text: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = js_trim(text);
    while !rest.is_empty() {
        let (part, used) = match rest.as_bytes()[0] {
            b'"' | b'\'' => match read_string_at(rest, 0) {
                Some(part) => (Some(part), skip_string(rest, 0).map_or(0, |end| end + 1)),
                None => (None, 0),
            },
            _ if rest.starts_with("$localize") => {
                match rest.find('`').and_then(|tick| skip_string(rest, tick)) {
                    Some(end) if end > 0 => (localize_default(&rest[..=end]), end + 1),
                    _ => (None, 0),
                }
            }
            b'`' => match skip_string(rest, 0) {
                Some(end) if end > 0 && !rest[1..end].contains("${") => {
                    (Some(rest[1..end].to_string()), end + 1)
                }
                _ => (None, 0),
            },
            _ => (None, 0),
        };
        let part = part.filter(|_| used > 0)?;
        parts.push(part);
        rest = js_trim(&rest[used..]);
        if rest.is_empty() {
            break;
        }
        rest = js_trim(rest.strip_prefix('+')?);
    }
    (!parts.is_empty()).then(|| parts.concat())
}

/// The non-empty segments of `path`.
fn segments_of(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|seg| !seg.is_empty())
        .map(str::to_string)
        .collect()
}

/// A route's own path segments: a string, a `$localize` default, or a
/// constant kept as `{expr}` for the cross-file pass; `None` when it cannot
/// be named or is a `**` catch-all (`pathSegments`).
fn path_segments(text: &str) -> Option<Vec<String>> {
    let trimmed = js_trim(text);
    if let Some(value) = static_string(trimmed) {
        return (!value.contains("**")).then(|| segments_of(&value));
    }
    if constant_path().is_match(trimmed) {
        return Some(vec![format!("{{{}}}", without_space(trimmed))]);
    }
    // `` `issue/:${ProjectConst.IssueId}` ``: a template whose holes are
    // constants.
    if trimmed.starts_with('`') && skip_string(trimmed, 0) == Some(trimmed.len() - 1) {
        let body = &trimmed[1..trimmed.len() - 1];
        let mut constants_only = true;
        let named = template_hole().replace_all(body, |caps: &regex::Captures<'_>| {
            let hole = &caps[1];
            if !constant_path().is_match(js_trim(hole)) {
                constants_only = false;
            }
            format!("{{{}}}", without_space(hole))
        });
        return (constants_only && !named.contains("**")).then(|| segments_of(&named));
    }
    // `internalRoutes.account.subRoutes.access.path + '/:id'`: each part a
    // string or a constant, the constant a whole segment of its own.
    if trimmed.contains('+') {
        let mut joined = String::new();
        for part in split_plus(trimmed) {
            if let Some(literal) = static_string(part) {
                joined.push_str(&literal);
            } else if constant_path().is_match(js_trim(part)) {
                joined.push_str(&format!("{{{}}}", without_space(js_trim(part))));
            } else {
                return None;
            }
        }
        return (!joined.contains("**")).then(|| segments_of(&joined));
    }
    None
}

/// An expression's `+` operands at depth 0, strings and brackets stepped
/// over; the whole text when something never closes (`splitPlus`).
fn split_plus(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => match skip_string(text, i) {
                Some(end) => i = end,
                None => return vec![text],
            },
            b'(' | b'[' | b'{' => match match_bracket(text, i) {
                Some(end) => i = end,
                None => return vec![text],
            },
            b'+' => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&text[start..]);
    parts
}

/// The component a route names (`AngularComponentRef`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComponentRef {
    /// A class in scope: `component: AuthComponent`.
    Class(String),
    /// A lazily imported file's: the member its loader picks, else the
    /// file's default export.
    Lazy {
        spec: String,
        member: Option<String>,
    },
}

impl ComponentRef {
    /// The reference name a route's reference to it carries.
    fn reference_name(&self) -> String {
        match self {
            Self::Class(name) => name.clone(),
            Self::Lazy { spec, member } => {
                format!(
                    "{LAZY_PREFIX}{spec}#{}",
                    member.as_deref().unwrap_or("default")
                )
            }
        }
    }
}

/// `() => import('./x')` → `./x`; `.then(m => m.X)` and
/// `(await import('./x')).X` name the member (`lazyImport`).
fn lazy_import(text: &str) -> Option<(String, Option<String>)> {
    let import = dynamic_import().captures(text)?;
    let spec = first_capture(&import, 1)?.to_string();
    let after = &text[import.get(0)?.end()..];
    let member = then_member()
        .captures(after)
        .filter(|then| then[1] == then[2])
        .map(|then| then[3].to_string())
        .or_else(|| {
            awaited_member()
                .captures(after)
                .map(|awaited| awaited[1].to_string())
        });
    Some((spec, member))
}

/// The component a `component:` or `loadComponent:` value names
/// (`componentRef`).
fn component_ref(field: &str, text: &str) -> Option<ComponentRef> {
    if field == "component" {
        return component_class_name()
            .captures(text)
            .map(|name| ComponentRef::Class(name[1].to_string()));
    }
    lazy_import(text).map(|(spec, member)| ComponentRef::Lazy { spec, member })
}

/// `/` and the segments joined (`joinPath`).
fn join_path<S: AsRef<str>>(segs: &[S]) -> String {
    let mut out = String::from("/");
    for (i, seg) in segs.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(seg.as_ref());
    }
    out
}

/// One screen a routes array declares (`AngularRoute`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AngularRoute {
    /// `/editor/:slug` within its file; a constant path segment kept as
    /// `{expr}` until the cross-file pass reads it.
    pub path: String,
    pub line: i64,
    pub component: Option<ComponentRef>,
    /// The layouts the screen renders inside, outermost first: each
    /// ancestor route's component.
    pub layouts: Vec<ComponentRef>,
}

/// Where a `redirectTo` entry sends the user (`AngularRedirect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AngularRedirect {
    /// `/` within the file.
    pub from: String,
    /// Where it sends the user, within the file, or, when `absolute`, in the
    /// app: `/home`.
    pub to: String,
    pub absolute: bool,
}

/// A `loadChildren` entry: where the routes it loads sit (`AngularMount`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AngularMount {
    /// Where the lazily loaded routes sit within this file: `/profile`.
    pub prefix: String,
    /// The `loadChildren` import: `./profile/profile.routes`.
    pub spec: String,
    pub line: i64,
}

/// Everything a file's routes arrays declare.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ParsedRoutes {
    pub routes: Vec<AngularRoute>,
    pub mounts: Vec<AngularMount>,
    pub redirects: Vec<AngularRedirect>,
}

/// The `/`-joined route of `prefix` and a redirect target, relative to the
/// prefix unless it is absolute.
fn redirect_target(prefix: &[String], target: &str) -> (String, bool) {
    let absolute = target.starts_with('/');
    let mut segs: Vec<String> = if absolute {
        Vec::new()
    } else {
        prefix.to_vec()
    };
    segs.extend(segments_of(target));
    (join_path(&segs), absolute)
}

/// The walk over one file's routes arrays (`parseAngularRoutes`'s `walk`).
struct RouteWalk<'s> {
    safe: &'s str,
    parsed: ParsedRoutes,
    seen_paths: HashSet<String>,
    walked: HashSet<usize>,
}

impl<'s> RouteWalk<'s> {
    fn walk(
        &mut self,
        open: usize,
        close: usize,
        prefix: &[String],
        depth: usize,
        layouts: &[ComponentRef],
    ) {
        if depth > MAX_CHILD_DEPTH || !self.walked.insert(open) {
            return;
        }
        let safe = self.safe;
        for (start, end) in top_level_objects(safe, open + 1, close) {
            let fields = read_fields(safe, start, end);
            if !ROUTE_KEYS.iter().any(|key| field(&fields, key).is_some()) {
                continue;
            }
            // A custom `matcher` decides the address at run time: no path to
            // name.
            if field(&fields, "matcher").is_some() {
                continue;
            }
            let path_field = field(&fields, "path");
            let redirect_to = field(&fields, "redirectTo");
            // `{ path: '**', redirectTo: 'home' }`: where an address nothing
            // else matches lands.
            if let (Some(path), Some(redirect)) = (path_field, redirect_to)
                && static_string(path.text).as_deref() == Some("**")
            {
                if let Some(target) =
                    static_string(redirect.text).filter(|target| !target.contains("**"))
                {
                    let mut from = prefix.to_vec();
                    from.push("**".to_string());
                    let (to, absolute) = redirect_target(prefix, &target);
                    self.parsed.redirects.push(AngularRedirect {
                        from: join_path(&from),
                        to,
                        absolute,
                    });
                }
                continue;
            }
            let own = match path_field {
                Some(path) => match path_segments(path.text) {
                    Some(own) => own,
                    // A computed path, or a `**` catch-all.
                    None => continue,
                },
                None => Vec::new(),
            };
            let mut segs = prefix.to_vec();
            segs.extend(own);
            let line = line_of(safe, path_field.map_or(start, |path| path.at));

            if let Some(redirect) = redirect_to {
                // `{ path: '', redirectTo: 'home' }`: arriving at `/` is
                // arriving at `/home`. A relative target is a sibling, under
                // the same parent.
                if let Some(target) =
                    static_string(redirect.text).filter(|target| !target.contains("**"))
                {
                    let (to, absolute) = redirect_target(prefix, &target);
                    self.parsed.redirects.push(AngularRedirect {
                        from: join_path(&segs),
                        to,
                        absolute,
                    });
                }
                continue;
            }

            if let Some(load_children) = field(&fields, "loadChildren") {
                if let Some((spec, _)) = lazy_import(load_children.text) {
                    self.parsed.mounts.push(AngularMount {
                        prefix: join_path(&segs),
                        spec,
                        line,
                    });
                }
                continue;
            }
            let component_field = ["component", "loadComponent"]
                .into_iter()
                .find(|key| field(&fields, key).is_some());
            let component = component_field.and_then(|key| {
                field(&fields, key).and_then(|value| component_ref(key, value.text))
            });

            let children = field(&fields, "children");
            let mut child_claims_address = false;
            if let Some(children) = children
                && let Some(child_open) = bracket_written_at(safe, children, b'[')
                && let Some(child_close) =
                    match_bracket(safe, child_open).filter(|close| *close > child_open)
            {
                for (child_start, child_end) in top_level_objects(safe, child_open + 1, child_close)
                {
                    let child_fields = read_fields(safe, child_start, child_end);
                    let claims = match field(&child_fields, "path") {
                        None => true,
                        Some(path) => path_segments(path.text).is_some_and(|own| own.is_empty()),
                    };
                    child_claims_address |= claims;
                }
                let mut inner = layouts.to_vec();
                if let Some(component) = &component {
                    inner.push(component.clone());
                }
                self.walk(child_open, child_close, &segs, depth + 1, &inner);
            }
            // A layout is a screen only where no child claims its address; a
            // redirect and a componentless grouping are no screen at all.
            if component_field.is_none() || (children.is_some() && child_claims_address) {
                continue;
            }
            let route = join_path(&segs);
            if !self.seen_paths.insert(route.clone()) {
                continue;
            }
            self.parsed.routes.push(AngularRoute {
                path: route,
                line,
                component,
                layouts: layouts.to_vec(),
            });
        }
    }
}

/// Every screen, lazy mount and redirect a file's routes arrays declare.
/// Entries are walked as objects, never read out of a window of text:
/// `path` may be written after `component` (`parseAngularRoutes`).
pub(crate) fn parse_angular_routes(content: &str) -> ParsedRoutes {
    if !router_import().is_match(content) {
        return ParsedRoutes::default();
    }
    let safe = strip_js_comments_keeping_offsets(content);
    let mut walk = RouteWalk {
        safe: &safe,
        parsed: ParsedRoutes::default(),
        seen_paths: HashSet::new(),
        walked: HashSet::new(),
    };
    for opener in array_openers().find_iter(&safe) {
        let open = opener.end() - 1;
        let Some(close) = match_bracket(&safe, open) else {
            continue;
        };
        // `const x = [` is a routes array only when it says so:
        // `] as Routes` / `] satisfies Routes`.
        if declares_binding().is_match(opener.as_str())
            && !says_routes().is_match(window(&safe, close, 32))
        {
            continue;
        }
        walk.walk(open, close, &[], 0, &[]);
    }
    walk.parsed
}

/// The id a route carries, reconstructed exactly so the resolver recognises
/// its own (`routeId`).
fn route_id(file_path: &str, line: i64, in_file_path: &str) -> String {
    format!("route:{file_path}:{line}:{in_file_path}{ROUTE_ID_SUFFIX}")
}

/// The path a route has within its own file, before any mount, kept in the
/// qualified name so the cross-file pass is idempotent (`inFilePath`).
fn in_file_path(node: &Node) -> &str {
    node.qualified_name
        .find(QUALIFIED_MARKER)
        .map_or(node.name.as_str(), |at| {
            &node.qualified_name[at + QUALIFIED_MARKER.len()..]
        })
}

/// True for a route node this resolver emitted, and no other
/// (`isAngularRoute`).
pub(crate) fn is_angular_route(node: &Node) -> bool {
    node.kind == NodeKind::Route
        && node.id.ends_with(ROUTE_ID_SUFFIX)
        && node.id == route_id(&node.file_path, node.start_line, in_file_path(node))
}

/// Whether `file_path` is a `.ts`, `.mts` or `.cts` file (`/\.[cm]?ts$/`).
fn is_ts_path(file_path: &str) -> bool {
    file_path.ends_with(".ts") || file_path.ends_with(".mts") || file_path.ends_with(".cts")
}

/// Whether `file_path` is a TypeScript module a routes array can be read
/// from: no declaration file.
fn is_routes_source(file_path: &str) -> bool {
    is_ts_path(file_path) && !file_path.ends_with(".d.ts")
}

/// One `@Component` class a file declares (`componentClassesIn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComponentClass {
    pub name: String,
    pub is_default: bool,
}

/// The `@Component` classes a file declares, in order, with whether each is
/// the default export (`componentClassesIn`).
pub(crate) fn component_classes_in(content: &str) -> Vec<ComponentClass> {
    let safe = strip_js_comments_keeping_offsets(content);
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(decorator) = component_decorator().find_at(&safe, from) {
        let open = decorator.end() - 1;
        let Some(close) = match_bracket(&safe, open) else {
            from = decorator.end();
            continue;
        };
        if let Some(class) = decorated_class().captures(window(&safe, close + 1, 399)) {
            out.push(ComponentClass {
                name: class[3].to_string(),
                is_default: class.get(2).is_some(),
            });
        }
        from = close;
    }
    out
}

/// The class node named `name` in `file`.
fn class_in(file: &str, name: &str, context: &dyn ResolutionContext) -> Option<Arc<Node>> {
    context
        .get_nodes_in_file_shared(file)
        .into_iter()
        .find(|node| node.kind == NodeKind::Class && node.name == name)
}

/// The component class a lazily loaded file provides: the member named,
/// else its default-exported component, else its only component, else its
/// default-exported class (`lazyComponent`).
fn lazy_component(
    spec: &str,
    member: Option<&str>,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    let file = resolve_import_path(spec, from_file, Language::TypeScript, context)?;
    let name = match member {
        Some(member) => member.to_string(),
        None => {
            let content = context.read_file_shared(&file)?;
            let classes = component_classes_in(&content);
            let picked = classes
                .iter()
                .find(|class| class.is_default)
                .or(match classes.as_slice() {
                    [only] => Some(only),
                    _ => None,
                })
                .map(|class| class.name.clone());
            match picked {
                Some(name) => name,
                None => default_class().captures(&content)?[1].to_string(),
            }
        }
    };
    class_in(&file, &name, context)
}

/// The file declaring `name` for code in `from_file`: the module a named
/// import of it resolves to (`None` when that does not resolve), else the
/// file itself (`declaringFile`).
fn declaring_file(name: &str, from_file: &str, context: &dyn ResolutionContext) -> Option<String> {
    let content = context.read_file_shared(from_file)?;
    for import in named_import().captures_iter(&content) {
        let named = import[1].split(',').any(|binding| {
            let local = import_alias()
                .split(js_trim(binding))
                .last()
                .map_or("", js_trim);
            local == name
        });
        if named {
            let spec = first_capture(&import, 2)?;
            return resolve_import_path(spec, from_file, Language::TypeScript, context);
        }
    }
    Some(from_file.to_string())
}

/// The class a route's component reference names: a lazy import's, else the
/// class of that name in the file its import names, or its own
/// (`routeComponent`).
fn route_component(
    encoded: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<Arc<Node>> {
    if let Some((spec, member)) = lazy_reference(encoded) {
        return lazy_component(spec, member, from_file, context);
    }
    let file = declaring_file(encoded, from_file, context)?;
    class_in(&file, encoded, context)
}

/// `import:./x#default` → `./x` and no member; `import:./x#X` → `./x` and
/// `X` (`LAZY_COMPONENT_REF`).
fn lazy_reference(name: &str) -> Option<(&str, Option<&str>)> {
    let rest = name.strip_prefix(LAZY_PREFIX)?;
    let (spec, member) = rest.rsplit_once('#')?;
    let well_formed = !spec.is_empty()
        && !spec.contains('#')
        && !member.is_empty()
        && member
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
    well_formed.then_some((spec, (member != "default").then_some(member)))
}

// =============================================================================
// The cross-file pass: mounts and constant paths
// =============================================================================

/// What a [`ConstantReader`] keeps for each pair of strings.
type PairMemo<V> = RefCell<HashMap<(String, String), V>>;

/// What the cross-file pass reads while it names one pass's routes: the
/// constants it looked up, by the file they were read from.
struct ConstantReader<'c> {
    context: &'c dyn ResolutionContext,
    memo: PairMemo<Option<String>>,
    /// The patterns the reads build from a name, by their text, each
    /// compiled once in the reader's life (W7-18d's cost revision: compiled
    /// at every read, they were most of the Angular pass's time).
    patterns: RefCell<HashMap<String, Rc<Regex>>>,
    /// Each (root, declaring file)'s declaring source, comments blanked.
    declared: PairMemo<Option<Rc<str>>>,
    /// Each (name, file)'s [`declaring_file`].
    declaring: PairMemo<Option<String>>,
}

impl<'c> ConstantReader<'c> {
    fn new(context: &'c dyn ResolutionContext) -> Self {
        Self {
            context,
            memo: RefCell::new(HashMap::new()),
            patterns: RefCell::new(HashMap::new()),
            declared: RefCell::new(HashMap::new()),
            declaring: RefCell::new(HashMap::new()),
        }
    }

    /// `source` compiled, once per reader.
    fn pattern(&self, source: String) -> Rc<Regex> {
        if let Some(found) = self.patterns.borrow().get(&source) {
            return Rc::clone(found);
        }
        let compiled = Rc::new(regex(&source));
        self.patterns
            .borrow_mut()
            .insert(source, Rc::clone(&compiled));
        compiled
    }

    /// [`declaring_file`], once per (name, file).
    fn declaring_file(&self, name: &str, from_file: &str) -> Option<String> {
        let key = (name.to_string(), from_file.to_string());
        if let Some(found) = self.declaring.borrow().get(&key) {
            return found.clone();
        }
        let found = declaring_file(name, from_file, self.context);
        self.declaring.borrow_mut().insert(key, found.clone());
        found
    }

    /// The source declaring `root` as `file` leads to it
    /// ([`Self::declaration_source`]), comments blanked, once per (root,
    /// file).
    fn declared_source(&self, root: &str, file: &str) -> Option<Rc<str>> {
        let key = (root.to_string(), file.to_string());
        if let Some(found) = self.declared.borrow().get(&key) {
            return found.clone();
        }
        let found = self
            .declaration_source(root, file, 0)
            .map(|content| Rc::from(strip_js_comments_keeping_offsets(&content).as_str()));
        self.declared.borrow_mut().insert(key, found.clone());
        found
    }

    /// The source text a constant member chain names, read from the file its
    /// root is imported from or declared in: `internalRoutes.account.path` is
    /// the `path:` field of `account` in `export const internalRoutes = {…}`
    /// (`constantText`).
    fn constant_text(&self, expr: &str, from_file: &str) -> Option<String> {
        self.constant_text_within(expr, from_file, 0)
    }

    fn constant_text_within(&self, expr: &str, from_file: &str, hops: usize) -> Option<String> {
        let key = (from_file.to_string(), expr.to_string());
        if let Some(cached) = self.memo.borrow().get(&key) {
            return cached.clone();
        }
        // A chain is in the memo while it is read, so an alias that leads
        // back to it reads nothing.
        self.memo.borrow_mut().insert(key.clone(), None);
        let value = self.read_constant(expr, from_file, hops);
        self.memo.borrow_mut().insert(key, value.clone());
        value
    }

    fn read_constant(&self, expr: &str, from_file: &str, hops: usize) -> Option<String> {
        // `const { create } = internalRoutes.accounts.subRoutes;` and
        // `const accounts = internalRoutes.accounts;` make `create.path` a
        // chain rooted at the import.
        if let Some(aliased) = self.local_alias(expr, from_file)
            && aliased != expr
        {
            return (hops < MAX_ALIAS_HOPS)
                .then(|| self.constant_text_within(&aliased, from_file, hops + 1))
                .flatten();
        }
        let parts: Vec<&str> = expr.split('.').map(js_trim).collect();
        let (root, chain) = parts.split_first()?;
        if root.is_empty() || chain.is_empty() {
            return None;
        }
        let file = self.declaring_file(root, from_file)?;
        let safe = self.declared_source(root, &file)?;
        let name = regex::escape(root);
        let declared = self.pattern(format!(
            r"(?-u:\b)(?:const|let){s}+{name}{s}*(?::[^=]+)?={s}*\{{",
            s = JS_SPACE
        ));
        if let Some(decl) = declared.find(&safe) {
            return read_chain(&safe, decl.end() - 1, chain);
        }
        let enumerated = self.pattern(format!(r"(?-u:\b)enum{s}+{name}{s}*\{{", s = JS_SPACE));
        if let Some(decl) = enumerated.find(&safe) {
            // `enum Paths { Board = 'board' }`
            let [member] = chain else {
                return None;
            };
            let open = decl.end() - 1;
            let close = match_bracket(&safe, open).unwrap_or(open).max(open);
            let body = &safe[open..close];
            let value = self.pattern(format!(
                r#"(?-u:\b){member}{s}*={s}*(['"`][^'"`]*['"`])"#,
                member = regex::escape(member),
                s = JS_SPACE
            ));
            return value.captures(body).map(|caps| caps[1].to_string());
        }
        let class = self.pattern(format!(
            r"(?-u:\b)class{s}+{name}(?-u:\b)[^{{]*\{{",
            s = JS_SPACE
        ));
        let decl = class.find(&safe)?;
        // `class ProjectConst { static readonly IssueId = 'issueId' }`, and
        // `class RouterUtil { static Configuration = { Visualizer: 'visualizer' } }`.
        let open = decl.end() - 1;
        let close = match_bracket(&safe, open).filter(|close| *close > open)?;
        let body = &safe[open..close];
        let (first, rest) = chain.split_first()?;
        let field = self.pattern(format!(
            r"(?-u:\b)static{s}+(?:readonly{s}+)?{member}{s}*(?::[^=;]+)?={s}*",
            member = regex::escape(first),
            s = JS_SPACE
        ));
        let found = field.find(body)?;
        let at = open + found.end();
        if safe.as_bytes().get(at) == Some(&b'{') {
            return if rest.is_empty() {
                None
            } else {
                read_chain(&safe, at, rest)
            };
        }
        if !rest.is_empty() {
            return None;
        }
        static QUOTED: OnceLock<Regex> = OnceLock::new();
        QUOTED
            .get_or_init(|| regex(r#"^(?:'[^'"`]*'|"[^'"`]*"|`[^'"`]*`)"#))
            .find(&safe[at..])
            .map(|quoted| quoted.as_str().to_string())
    }

    /// The source that declares `root`: `file` itself, or, when `file` is a
    /// barrel (an Nx library's `src/index.ts`), the module it re-exports it
    /// from (`declarationSource`).
    fn declaration_source(&self, root: &str, file: &str, hops: usize) -> Option<Arc<str>> {
        let content = self.context.read_file_shared(file)?;
        let name = regex::escape(root);
        let declares = self.pattern(format!(
            r"(?-u:\b)(?:const|let|class|enum){s}+{name}(?-u:\b)",
            s = JS_SPACE
        ));
        if declares.is_match(&content) {
            return Some(content);
        }
        if hops >= MAX_DECLARATION_HOPS {
            return None;
        }
        let names_root = self.pattern(format!(r"(?-u:\b){name}(?-u:\b)"));
        for clause in re_export_clause().captures_iter(&content) {
            if &clause[1] != "*" && !names_root.is_match(&clause[1]) {
                continue;
            }
            let Some(spec) = first_capture(&clause, 2) else {
                continue;
            };
            let target = resolve_import_path(spec, file, Language::TypeScript, self.context);
            if let Some(found) =
                target.and_then(|target| self.declaration_source(root, &target, hops + 1))
            {
                return Some(found);
            }
        }
        None
    }

    /// `create.path` as the chain its root was destructured from in
    /// `from_file`, or `None` when the root is no local alias (`localAlias`).
    fn local_alias(&self, expr: &str, from_file: &str) -> Option<String> {
        let content = self.context.read_file_shared(from_file)?;
        let mut parts = expr.split('.');
        let root = parts.next().filter(|root| !root.is_empty())?;
        let rest: Vec<&str> = parts.collect();
        let chained = |head: String| {
            let mut out = head;
            for part in &rest {
                out.push('.');
                out.push_str(part);
            }
            out
        };
        for destructure in destructuring().captures_iter(&content) {
            for binding in destructure[1].split(',') {
                let mut sides = binding.split(':').map(js_trim);
                let prop = sides.next().unwrap_or("");
                let local = sides.next().unwrap_or(prop);
                if local == root && !prop.is_empty() {
                    return Some(chained(format!(
                        "{}.{prop}",
                        without_space(&destructure[2])
                    )));
                }
            }
        }
        let simple = self.pattern(format!(
            r"(?-u:\b)(?:const|let){s}+{name}{s}*={s}*([A-Za-z_$][A-Za-z0-9_$]*(?:{s}*\.{s}*[A-Za-z_$][A-Za-z0-9_$]*)+){s}*;",
            name = regex::escape(root),
            s = JS_SPACE
        ));
        simple
            .captures(&content)
            .map(|caps| chained(without_space(&caps[1])))
    }

    /// A constant path's value: `internalRoutes.account.path` → `account`
    /// (`constantValue`).
    fn constant_value(&self, expr: &str, from_file: &str) -> Option<String> {
        self.constant_text(expr, from_file)
            .and_then(|text| static_string(&text))
    }

    /// A path's segments with each `{expr}` read from the constant it names,
    /// as seen from `file`, where the route (or the mount) is written
    /// (`resolvedSegments`).
    fn resolved_segments(&self, path: &str, file: &str) -> Vec<String> {
        let mut out = Vec::new();
        for seg in path.split('/').filter(|seg| !seg.is_empty()) {
            if seg.len() > 1
                && seg.starts_with('{')
                && seg.ends_with('}')
                && !seg[1..].contains('{')
            {
                match self.constant_value(&seg[1..seg.len() - 1], file) {
                    Some(value) => out.extend(segments_of(&value)),
                    None => out.push(seg.to_string()),
                }
                continue;
            }
            // `:{ProjectConst.IssueId}`: a constant inside a segment.
            let named = segment_placeholder().replace_all(seg, |caps: &regex::Captures<'_>| {
                self.constant_value(&caps[1], file)
                    .unwrap_or_else(|| caps[0].to_string())
            });
            out.push(named.into_owned());
        }
        out
    }
}

/// The text of the last link of `links` in the object literal opening at
/// `open`, each link before it an object written in place (`readChain`).
fn read_chain(safe: &str, open: usize, links: &[&str]) -> Option<String> {
    let mut start = open;
    for (i, link) in links.iter().enumerate() {
        let end = match_bracket(safe, start).filter(|end| *end > start)?;
        let fields = read_fields(safe, start, end);
        let found = field(&fields, link)?;
        if i == links.len() - 1 {
            return Some(js_trim(found.text).to_string());
        }
        start = bracket_written_at(safe, found, b'{')?;
    }
    None
}

/// Where every routes file is mounted: the `loadChildren` chain from the app
/// down (`angularMounts`).
struct AngularMounts {
    /// The file that loads each loaded file, and the path it loads it at.
    loaded_by: BTreeMap<String, (String, String)>,
    /// Every file that lazy-loads routes, whether or not it declares a
    /// screen of its own (`mountFiles`).
    mount_files: BTreeSet<String>,
}

impl AngularMounts {
    /// The mounts of the project `context` sees, for the routes files
    /// `route_files` holds.
    fn read(context: &dyn ResolutionContext, route_files: &BTreeSet<String>) -> Self {
        let mut mounts_by_file: BTreeMap<String, Vec<AngularMount>> = BTreeMap::new();
        for file in context.get_all_files_shared().iter() {
            if !is_ts_path(file) {
                continue;
            }
            let Some(content) = context.read_file_shared(file) else {
                continue;
            };
            if !content.contains("loadChildren") {
                continue;
            }
            let mounts = parse_angular_routes(&content).mounts;
            if !mounts.is_empty() {
                mounts_by_file.insert(file.clone(), mounts);
            }
        }
        let mount_files: BTreeSet<String> = mounts_by_file.keys().cloned().collect();
        let mut loaded_by: BTreeMap<String, (String, String)> = BTreeMap::new();
        for (file, mounts) in &mounts_by_file {
            for mount in mounts {
                for target in
                    route_files_loaded_by(&mount.spec, file, context, route_files, &mount_files)
                {
                    // A file mounted from two places keeps its first mount,
                    // in path order.
                    if target != *file && !loaded_by.contains_key(&target) {
                        loaded_by.insert(target, (file.clone(), mount.prefix.clone()));
                    }
                }
            }
        }
        Self {
            loaded_by,
            mount_files,
        }
    }

    /// The path segments `file`'s routes sit under: its parent's prefix plus
    /// the mount's, along the chain of files that load it, stopping at the
    /// first file that repeats (KEEP-RUST, see the module comment).
    fn prefix_of(&self, file: &str, constants: &ConstantReader<'_>) -> Vec<String> {
        let mut chain: Vec<Vec<String>> = Vec::new();
        let mut visited: HashSet<&str> = HashSet::from([file]);
        let mut current = file;
        while let Some((parent, prefix)) = self.loaded_by.get(current) {
            if !visited.insert(parent.as_str()) {
                break;
            }
            chain.push(constants.resolved_segments(prefix, parent));
            current = parent;
        }
        chain.into_iter().rev().flatten().collect()
    }
}

/// The routes files a `loadChildren` import names: the file it resolves to,
/// looked through barrels (an Nx library is imported by its alias, which
/// names the library's `src/index.ts`, whose `export * from './lib/x.module'`
/// names the module), and for an NgModule the routing modules it imports
/// (`routeFilesLoadedBy`).
fn route_files_loaded_by(
    spec: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
    route_files: &BTreeSet<String>,
    mount_files: &BTreeSet<String>,
) -> Vec<String> {
    let routes_or_mounts = |file: &str| route_files.contains(file) || mount_files.contains(file);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    // A barrel can re-export another barrel; a few hops settle it.
    let mut barrels: Vec<String> =
        resolve_import_path(spec, from_file, Language::TypeScript, context)
            .into_iter()
            .collect();
    for _ in 0..MAX_BARREL_HOPS {
        if barrels.is_empty() {
            break;
        }
        let mut next: Vec<String> = Vec::new();
        for file in barrels {
            if !seen.insert(file.clone()) {
                continue;
            }
            if routes_or_mounts(&file) {
                out.push(file);
                continue;
            }
            let Some(content) = context.read_file_shared(&file) else {
                continue;
            };
            // `loadChildren: () => import('./layout/layout.module').then(m =>
            // m.LayoutModule)`: the module holds no routes; the routing module
            // it imports does.
            if ng_module().is_match(&content) {
                for import in import_from().captures_iter(&content) {
                    let Some(spec) = first_capture(&import, 1) else {
                        continue;
                    };
                    if let Some(imported) =
                        resolve_import_path(spec, &file, Language::TypeScript, context)
                        && routes_or_mounts(&imported)
                        && !out.contains(&imported)
                    {
                        out.push(imported);
                    }
                }
                continue;
            }
            for export in re_export().captures_iter(&content) {
                let Some(spec) = first_capture(&export, 1) else {
                    continue;
                };
                if let Some(reexported) =
                    resolve_import_path(spec, &file, Language::TypeScript, context)
                    && !seen.contains(&reexported)
                {
                    next.push(reexported);
                }
            }
        }
        barrels = next;
    }
    out
}

// =============================================================================
// Navigation
// =============================================================================

/// How many redirects a destination is followed through (upstream settles
/// its aliases in four passes).
const MAX_REDIRECT_HOPS: usize = 4;

/// How deep a destination is followed through properties, constants and
/// `.concat` (`destinationOf`'s `depth > 4`).
const MAX_DESTINATION_DEPTH: usize = 4;

/// The verbs of the router's navigation calls (`navigation.tails`).
const NAV_TAILS: [&str; 4] = ["navigate", "navigateByUrl", "createUrlTree", "parseUrl"];

/// `this.router.navigate`, `router.navigateByUrl`, `this._router.createUrlTree`,
/// `router.parseUrl` (`NAV_CALL`).
fn nav_call() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(r"^(?:this\.)?_?[rR]outer\.(navigate|navigateByUrl|createUrlTree|parseUrl)$")
    })
}

/// The verb a navigation call's name ends with, or `None` for any other name.
fn nav_verb(name: &str) -> Option<&str> {
    nav_call()
        .captures(name)
        .and_then(|caps| caps.get(1))
        .map(|verb| verb.as_str())
}

/// `relativeTo` among a navigation's extras.
fn relative_to() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(r"(?-u:\b)relativeTo(?-u:\b)"))
}

/// `x.concat(…)`: the value and its arguments.
fn concat_call() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(&format!(r"(?s)^(.*?)\.concat{s}*\((.*)\)$", s = JS_SPACE)))
}

/// `update.routerLink(dataSource, symbol)`: a member chain, called.
fn chain_call() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"(?s)^((?:this{s}*\.{s}*)?[A-Za-z_$][A-Za-z0-9_$]*(?:{s}*\.{s}*[A-Za-z_$][A-Za-z0-9_$]*)+){s}*\((.*)\)$",
            s = JS_SPACE
        ))
    })
}

/// `this.routerLinkAbout`, `internalRoutes.zen.routerLink`, `link`.
fn member_chain() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex(&format!(
            r"^(?:this{s}*\.{s}*)?([A-Za-z_$][A-Za-z0-9_$]*(?:{s}*\.{s}*[A-Za-z_$][A-Za-z0-9_$]*)*)$",
            s = JS_SPACE
        ))
    })
}

/// `return [` in an arrow function's body.
fn returns_array() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| regex(&format!(r"(?-u:\b)return{s}*\[", s = JS_SPACE)))
}

/// The comma-separated items of `text`, strings and brackets stepped over;
/// `None` when one never closes.
fn top_level_items(text: &str) -> Option<Vec<&str>> {
    let bytes = text.as_bytes();
    let mut items = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(text, i)?,
            b'[' | b'{' | b'(' => i = match_bracket(text, i)?,
            b',' => {
                items.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    items.push(&text[start..]);
    Some(items)
}

/// The href, when it names at least one segment of its own
/// (`namesSomewhere`): a path of holes alone (`` [`/${a}`, b, c] ``) would
/// match any route of its length.
fn names_somewhere(href: HrefLiteral) -> Option<HrefLiteral> {
    let mut segs = href
        .path
        .split('/')
        .filter(|seg| !seg.is_empty())
        .peekable();
    let names = segs.peek().is_none() || segs.any(|seg| !seg.contains(HOLE));
    names.then_some(href)
}

/// The destination a command array names (`commandsHref`): `['/article',
/// slug]` is `/article/${…}`, each static element's segments in turn, a hole
/// for a computed one, nothing for a matrix-params object. Only an absolute
/// array names a place: its first element a static string starting with
/// `/`, or, for a call that names no `relativeTo` (`from_root`), any that
/// does not start with `.`. In a template, `['../', id]` and `['edit']` are
/// relative to wherever the component is.
pub(crate) fn commands_href(text: &str, from_root: bool) -> Option<HrefLiteral> {
    let arr = js_trim(text);
    if !arr.starts_with('[') {
        return None;
    }
    let close = match_bracket(arr, 0)?;
    let elements = top_level_items(&arr[1..close])?;
    let mut parts: Vec<String> = Vec::new();
    for element in &elements {
        let element = js_trim(element);
        if element.is_empty() {
            continue;
        }
        if let Some(literal) = static_string(element) {
            parts.extend(segments_of(&literal));
        } else if !element.starts_with('{') {
            parts.push(HOLE.to_string());
        }
    }
    let first = static_string(js_trim(elements.first().copied().unwrap_or("")))?;
    if !(first.starts_with('/') || (from_root && !first.starts_with('.'))) {
        return None;
    }
    let path = format!("/{}", parts.join("/"));
    names_somewhere(HrefLiteral {
        display: path.replace(HOLE, "${…}"),
        path,
        alternates: Vec::new(),
    })
}

/// A class property's initializer, read from the class's own lines:
/// `routerLinkAbout = publicRoutes.about.routerLink;` (`propertyInitializer`).
/// The pattern for `name` comes from `constants`, compiled once per reader.
fn property_initializer(
    owner: &Node,
    name: &str,
    content: &str,
    constants: &ConstantReader<'_>,
) -> Option<String> {
    let first = usize::try_from(owner.start_line - 1).ok()?;
    let last = usize::try_from(owner.end_line).ok()?;
    let body = content
        .split('\n')
        .skip(first)
        .take(last.saturating_sub(first))
        .collect::<Vec<_>>()
        .join("\n");
    let decl = constants.pattern(format!(
        r"(?:^|{s}|[;{{])(?:(?:public|private|protected|readonly|static|override){s}+)*{name}{s}*(?::[^=;\n]+)?={s}*",
        name = regex::escape(name),
        s = JS_SPACE
    ));
    let start = decl.find(&body)?.end();
    let bytes = body.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(&body, i)?,
            b'[' | b'{' | b'(' => i = match_bracket(&body, i)?,
            b';' | b'\n' => break,
            _ => {}
        }
        i += 1;
    }
    let value = js_trim(&body[start..i.min(bytes.len())]);
    (!value.is_empty()).then(|| value.to_string())
}

/// The array an arrow function returns: `(id) => ['/x', id]` or `(id) => {
/// return ['/x', id]; }` (`arrowReturn`).
fn arrow_return(fun: &str) -> Option<&str> {
    let arrow = fun.find("=>")?;
    let rest = &fun[arrow + 2..];
    let mut i = fun.len() - rest.trim_start_matches(is_js_space).len();
    let bytes = fun.as_bytes();
    if bytes.get(i) == Some(&b'(') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'[') => {
            let end = match_bracket(fun, i).filter(|end| *end > i)?;
            Some(&fun[i..=end])
        }
        Some(b'{') => {
            let ret = returns_array().find(&fun[i..])?;
            let open = i + ret.end() - 1;
            let end = match_bracket(fun, open).filter(|end| *end > open)?;
            Some(&fun[open..=end])
        }
        _ => None,
    }
}

/// Where an Angular destination expression leads, the argument of
/// `router.navigate(…)` (`angularDestination`):
///
/// - a command array, `['/article', slug]`;
/// - a static string, `'/login'` or `'/' + $localize…`, and `'/editor/' +
///   slug`;
/// - a route constant, `internalRoutes.zen.routerLink`, or one that builds
///   its commands, `access.routerLink('me')`;
/// - a property of the class it is written in, `this.routerLinkAbout`,
///   holding one of those;
/// - any of those `.concat(id)`, one more segment per argument.
///
/// `owner` is the class the expression is written in, for its properties.
fn angular_destination(
    expr: &str,
    file: &str,
    owner: Option<&Node>,
    constants: &ConstantReader<'_>,
    depth: usize,
    from_root: bool,
) -> Option<HrefLiteral> {
    destination_of(expr, file, owner, constants, depth, from_root).and_then(names_somewhere)
}

fn destination_of(
    expr: &str,
    file: &str,
    owner: Option<&Node>,
    constants: &ConstantReader<'_>,
    depth: usize,
    from_root: bool,
) -> Option<HrefLiteral> {
    let text = js_trim(expr);
    if text.is_empty() || depth > MAX_DESTINATION_DEPTH {
        return None;
    }
    if text.starts_with('[') {
        return commands_href(text, from_root);
    }
    if let Some(literal) = static_string(text) {
        return to_href(Some(literal));
    }
    // `'/editor/' + article.slug`: the literal, then a hole for the one
    // segment its other operand fills. KEEP-RUST: only a literal ending
    // with `/` and one operand after it; upstream adds `/` and a hole to any
    // literal head, so `'/redirect' + path` reached `/redirect/:x`.
    if text.starts_with(['\'', '"'])
        && let [head, tail] = split_plus(text).as_slice()
        && static_string(tail).is_none()
        && let Some(head) = static_string(head)
        && head.starts_with('/')
        && head.ends_with('/')
    {
        return to_href(Some(format!("{head}{HOLE}")));
    }
    // `this.routerLinkAdminControlUsers.concat(userId)`: a segment more for
    // each argument.
    if let Some(concat) = concat_call().captures(text) {
        let base = angular_destination(&concat[1], file, owner, constants, depth + 1, from_root);
        let added = top_level_items(&concat[2]).map_or(0, |items| {
            items
                .iter()
                .filter(|item| !js_trim(item).is_empty())
                .count()
        });
        return match base {
            Some(base) if added > 0 => {
                let path = base.path.strip_suffix('/').unwrap_or(&base.path);
                to_href(Some(format!("{path}{}", format!("/{HOLE}").repeat(added))))
            }
            base => base,
        };
    }
    // `update.routerLink(dataSource, symbol)`: a route constant that builds
    // its commands, its returned array read with the arguments as holes.
    if let Some(call) = chain_call().captures(text) {
        let chain = without_space(&call[1]);
        let chain = chain.strip_prefix("this.").unwrap_or(&chain);
        let fun = constants.constant_text(chain, file)?;
        return commands_href(arrow_return(&fun)?, from_root);
    }
    let Some(chain) = member_chain().captures(text) else {
        // KEEP-RUST: a conditional or template is read whole, as Vue Router
        // and SvelteKit read theirs; upstream reads the literal heading it.
        return parse_href(text, Literal::Whole);
    };
    let parts: Vec<String> = without_space(&chain[1])
        .split('.')
        .map(str::to_string)
        .collect();
    if let Some(owner) = owner
        && (text.starts_with("this") || parts.len() == 1)
        && let Some(content) = constants.context.read_file_shared(file)
        && let Some(init) = property_initializer(owner, &parts[0], &content, constants)
    {
        let expr = match parts.as_slice() {
            [_] => init,
            [_, rest @ ..] => format!("{init}.{}", rest.join(".")),
            [] => return None,
        };
        return angular_destination(&expr, file, Some(owner), constants, depth + 1, from_root);
    }
    let constant = constants.constant_text(&parts.join("."), file)?;
    angular_destination(&constant, file, owner, constants, depth + 1, from_root)
}

/// The arms of `href` a navigation can take: absolute, on this site, and
/// naming a segment of their own. Every other arm drops out before the arms
/// are matched, so a relative arm before an absolute one does not hide it,
/// and `//auth.example.com/login` is not this app's `/login` (KEEP-RUST:
/// upstream reads only the first arm's path, and an absolute URL's path).
fn navigable_arms(href: &HrefLiteral) -> Option<HrefLiteral> {
    let mut arms = href
        .arms()
        .filter(|arm| arm.path.starts_with('/') && !leaves_the_site(&arm.path))
        .map(|arm| HrefLiteral {
            path: arm.path.clone(),
            display: arm.display.clone(),
            alternates: Vec::new(),
        })
        .filter_map(names_somewhere);
    let mut first = arms.next()?;
    first.alternates = arms.collect();
    Some(first)
}

/// The app a routes file belongs to (`angularAppRoot`): the nearest
/// directory above it holding an `angular.json` or an Nx `nx.json`, so a
/// workspace whose screens are split across `libs/*` is one app; else the
/// conventional app root.
fn angular_app_root(
    file_path: &str,
    context: &dyn ResolutionContext,
    memo: &mut HashMap<String, Option<String>>,
) -> String {
    let dir = file_path.rfind('/').map_or("", |slash| &file_path[..slash]);
    let found = match memo.get(dir) {
        Some(found) => found.clone(),
        None => {
            let mut found = None;
            let mut current = Some(dir);
            while let Some(d) = current {
                let at = if d.is_empty() {
                    String::new()
                } else {
                    format!("{d}/")
                };
                if context.file_exists(&format!("{at}angular.json"))
                    || context.file_exists(&format!("{at}nx.json"))
                {
                    found = Some(at);
                    break;
                }
                current = (!d.is_empty()).then(|| d.rfind('/').map_or("", |slash| &d[..slash]));
            }
            memo.insert(dir.to_string(), found.clone());
            found
        }
    };
    found.unwrap_or_else(|| app_root_for(file_path))
}

/// True for a route a navigation can reach (`inAngularTable`): this
/// resolver's, at an absolute path. KEEP-RUST: a route whose path still
/// holds a constant the cross-file pass could not read (`/{paths.lost}`)
/// is in no table; upstream matches the placeholder as a literal segment,
/// which any hole of a destination fits.
fn in_angular_table(node: &Node) -> bool {
    is_angular_route(node) && node.name.starts_with('/') && !node.name.contains('{')
}

/// One Angular app's routes, by path (`RouteTable`).
#[derive(Debug, Default)]
struct AngularApp {
    exact: ExactTable,
    /// The routes with a parameter in their path.
    dynamic: Vec<RouteEntry>,
}

/// Every Angular app's routes, by the directory the app lives in, with the
/// paths its redirects serve (`angularRouteTable`).
#[derive(Debug, Default)]
struct AngularTable {
    by_root: BTreeMap<String, AngularApp>,
}

/// The routes every path reaches through `aliases` (redirect `from` → the
/// `to`s it sends the user to) in at most `hops` more redirects: the routes
/// serving it, else those its redirects reach.
fn reached(
    served: &BTreeMap<String, Vec<Arc<Node>>>,
    aliases: &BTreeMap<String, BTreeSet<String>>,
    path: &str,
    hops: usize,
    out: &mut BTreeMap<String, Arc<Node>>,
) {
    if let Some(routes) = served.get(path) {
        for route in routes {
            out.insert(route.id.clone(), Arc::clone(route));
        }
        return;
    }
    if hops == 0 {
        return;
    }
    for to in aliases.get(path).into_iter().flatten() {
        reached(served, aliases, to, hops - 1, out);
    }
}

impl AngularTable {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut memo = HashMap::new();
        let routes: Vec<Arc<Node>> = context
            .get_nodes_by_kind_shared(NodeKind::Route)
            .into_iter()
            .filter(|node| is_angular_route(node))
            .collect();
        // The apps, one per root holding an Angular route (one whose path
        // holds an unread constant too, as upstream's `byRoot`), and each
        // app's routes by the path they serve.
        let mut apps: BTreeSet<String> = BTreeSet::new();
        let mut served: BTreeMap<String, BTreeMap<String, Vec<Arc<Node>>>> = BTreeMap::new();
        for route in routes.iter().filter(|route| route.name.starts_with('/')) {
            let root = angular_app_root(&route.file_path, context, &mut memo);
            if in_angular_table(route) {
                served
                    .entry(root.clone())
                    .or_default()
                    .entry(route.name.clone())
                    .or_default()
                    .push(Arc::clone(route));
            }
            apps.insert(root);
        }
        // Redirects: arriving at `/` is arriving wherever `redirectTo` sends
        // the user, read from every routes file and every file that only
        // mounts others, under the path the file is mounted at.
        let route_files: BTreeSet<String> =
            routes.iter().map(|route| route.file_path.clone()).collect();
        let mounts = AngularMounts::read(context, &route_files);
        let constants = ConstantReader::new(context);
        let only_root = (apps.len() == 1)
            .then(|| apps.iter().next().cloned())
            .flatten();
        let mut aliases: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        for file in route_files.iter().chain(mounts.mount_files.iter()) {
            let Some(content) = context.read_file_shared(file) else {
                continue;
            };
            if !content.contains("redirectTo") {
                continue;
            }
            let own = angular_app_root(file, context, &mut memo);
            let Some(root) = apps
                .contains(&own)
                .then_some(own)
                .or_else(|| only_root.clone())
            else {
                continue;
            };
            let prefix = mounts.prefix_of(file, &constants);
            for redirect in parse_angular_routes(&content).redirects {
                let mut from = prefix.clone();
                from.extend(constants.resolved_segments(&redirect.from, file));
                let mut to = if redirect.absolute {
                    Vec::new()
                } else {
                    prefix.clone()
                };
                to.extend(constants.resolved_segments(&redirect.to, file));
                let (from, to) = (join_path(&from), join_path(&to));
                // KEEP-RUST: a path that still holds a constant the
                // cross-file pass could not read names no real address, so
                // a redirect from or to one is skipped. Upstream keeps the
                // placeholder, which a destination written the same way
                // matches.
                if from.contains('{') || to.contains('{') {
                    continue;
                }
                aliases
                    .entry(root.clone())
                    .or_default()
                    .entry(from)
                    .or_default()
                    .insert(to);
            }
        }
        let mut table = Self::default();
        let no_paths = BTreeMap::new();
        for root in &apps {
            let paths = served.get(root).unwrap_or(&no_paths);
            let app = table.by_root.entry(root.clone()).or_default();
            for (path, nodes) in paths {
                for node in nodes {
                    app.exact
                        .insert(path.clone(), root.clone(), Arc::clone(node));
                    if path.contains(':') {
                        app.dynamic.push(RouteEntry::new(Arc::clone(node)));
                    }
                }
            }
            // KEEP-RUST: each redirect is followed through the routes and
            // the other redirects at most four hops, whatever order they are
            // read in; a path a route serves keeps it (as upstream's
            // `!exact.has(from)`), and a path whose redirects reach two
            // routes reaches neither. Upstream applies them in node and
            // file order, the first to claim a path keeping it.
            let empty = BTreeMap::new();
            let app_aliases = aliases.get(root).unwrap_or(&empty);
            let mut aliased: BTreeSet<&str> = BTreeSet::new();
            for (from, tos) in app_aliases {
                if paths.contains_key(from) {
                    continue;
                }
                let mut targets = BTreeMap::new();
                for to in tos {
                    reached(paths, app_aliases, to, MAX_REDIRECT_HOPS - 1, &mut targets);
                }
                if !targets.is_empty() {
                    aliased.insert(from.as_str());
                }
                for node in targets.into_values() {
                    app.exact.insert(from.clone(), root.clone(), node);
                }
            }
            // `/` that no route or redirect serves falls through to the
            // app's root `**` redirect. Only `/`: letting the wildcard
            // answer any path would draw every destination this reading
            // could not name as a trip home.
            if !paths.contains_key("/")
                && !aliased.contains("/")
                && let Some(tos) = app_aliases.get("/**")
            {
                let mut targets = BTreeMap::new();
                for to in tos {
                    reached(paths, app_aliases, to, MAX_REDIRECT_HOPS - 1, &mut targets);
                }
                for node in targets.into_values() {
                    app.exact.insert("/".to_string(), root.clone(), node);
                }
            }
        }
        table
    }

    /// The routes a file navigates among (`angularRoutesFor`): its own app's
    /// (the longest root above it), or, for a shared library outside any
    /// app, the one app's when there is only one.
    fn routes_for(&self, file_path: &str) -> Option<&AngularApp> {
        let own = self
            .by_root
            .iter()
            .filter(|(root, _)| file_path.starts_with(root.as_str()))
            .max_by_key(|(root, _)| root.len())
            .map(|(_, app)| app);
        if let Some(own) = own
            && !own.exact.is_empty()
        {
            return Some(own);
        }
        match self.by_root.values().collect::<Vec<_>>().as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }
}

/// The pass's Angular table.
fn angular_table(context: &dyn ResolutionContext) -> Arc<AngularTable> {
    context
        .route_tables()
        .get_or_build("angular-router", || AngularTable::build(context))
}

impl LinkRoutes for AngularTable {
    fn is_empty(&self) -> bool {
        self.by_root.values().all(|app| app.exact.is_empty())
    }

    fn serves(&self, file: &str) -> bool {
        self.routes_for(file)
            .is_some_and(|app| !app.exact.is_empty())
    }

    fn destinations<'h>(
        &self,
        file: &str,
        href: &'h HrefLiteral,
    ) -> Vec<(Arc<Node>, &'h HrefLiteral)> {
        match self.routes_for(file) {
            Some(app) if !app.exact.is_empty() => {
                destinations_for_href(href, &app.exact, &app.dynamic, file)
            }
            _ => Vec::new(),
        }
    }
}

/// The Angular routes a component's `routerLink` reaches, in its own app as
/// a navigation call's are (`angularRoutesFor`; W7-18d).
pub(crate) fn angular_link_routes(context: &dyn ResolutionContext) -> Arc<dyn LinkRoutes> {
    angular_table(context)
}

/// The destinations a component's `[routerLink]` bindings and `routerLink:`
/// fields name (W7-18d), read as a navigation call's argument is
/// ([`angular_destination`], the constants each file is read for kept for
/// the pass), but not from the root (upstream's default): a template's
/// `['edit']` is relative to the component. KEEP-RUST, as a call's: only
/// [`navigable_arms`] are matched.
pub(crate) struct LinkDestinations<'c> {
    constants: ConstantReader<'c>,
}

impl<'c> LinkDestinations<'c> {
    pub(crate) fn new(context: &'c dyn ResolutionContext) -> Self {
        Self {
            constants: ConstantReader::new(context),
        }
    }

    /// Where `expr`, written in `file` within the class `owner`, leads.
    pub(crate) fn of(&self, expr: &str, file: &str, owner: &Node) -> Option<HrefLiteral> {
        let href = angular_destination(expr, file, Some(owner), &self.constants, 0, false)?;
        navigable_arms(&href)
    }
}

/// Where a plain `routerLink="…"` leads (`plainHref`): its text, each
/// interpolation a hole, when it starts with `/`: `/profile/{{ name }}` is
/// `/profile/${…}`. KEEP-RUST, as a call's: only [`navigable_arms`] are
/// matched, so `//host/x` is no link of this app.
pub(crate) fn plain_link_destination(value: &str) -> Option<HrefLiteral> {
    let mut with_holes = String::new();
    let mut rest = js_trim(value);
    while let Some(open) = rest.find("{{") {
        let Some(close) = rest[open + 2..].find("}}") else {
            break;
        };
        with_holes.push_str(&rest[..open]);
        with_holes.push(HOLE);
        rest = &rest[open + 2 + close + 2..];
    }
    with_holes.push_str(rest);
    if !with_holes.starts_with('/') {
        return None;
    }
    navigable_arms(&names_somewhere(to_href(Some(with_holes))?)?)
}

/// The navigation calls Angular binds to its routes: a route answers the
/// calls of its own app, or of the whole workspace when it is the only one.
struct AngularNavigation;

impl NavigationCalls for AngularNavigation {
    fn tails(&self) -> &[&str] {
        &NAV_TAILS
    }

    fn scope(&self, route: &Node, context: &dyn ResolutionContext) -> Option<Vec<String>> {
        if !in_angular_table(route) {
            return None;
        }
        Some(if angular_table(context).by_root.len() == 1 {
            vec![String::new()]
        } else {
            vec![angular_app_root(
                &route.file_path,
                context,
                &mut HashMap::new(),
            )]
        })
    }
}

/// Angular Router's routes (`angularRouterResolver`).
pub struct AngularRouterResolver;

impl FrameworkResolver for AngularRouterResolver {
    fn name(&self) -> &str {
        "angular-router"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::TypeScript, Language::JavaScript])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        super::package_deps::depends_on(context, &["@angular/router", "@angular/core"])
    }

    fn claims_reference(&self, name: &str) -> bool {
        nav_verb(name).is_some()
            || lazy_reference(name).is_some()
            || name
                .strip_prefix(LAYOUT_PREFIX)
                .is_some_and(|layout| !layout.is_empty())
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&AngularNavigation)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut result = FrameworkResolverExtractionResult::default();
        if !is_routes_source(file_path) {
            return Some(result);
        }
        for route in parse_angular_routes(content).routes {
            let node = framework_node(
                route_id(file_path, route.line, &route.path),
                NodeKind::Route,
                route.path.clone(),
                format!("{file_path}{QUALIFIED_MARKER}{}", route.path),
                file_path.to_string(),
                route.line,
                route.line,
                0,
                0,
                Language::TypeScript,
                false,
            );
            let reference = |name: String| RefView {
                row_id: None,
                from_node_id: node.id.clone(),
                reference_name: name,
                reference_kind: EdgeKind::References,
                line: route.line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::TypeScript,
                is_function_ref: false,
                reference_subkind: None,
            };
            if let Some(component) = &route.component {
                result
                    .references
                    .push(reference(component.reference_name()));
            }
            // The layouts around it: their links and buttons are on this
            // screen too.
            for layout in &route.layouts {
                result.references.push(reference(format!(
                    "{LAYOUT_PREFIX}{}",
                    layout.reference_name()
                )));
            }
            result.nodes.push(node);
        }
        Some(result)
    }

    /// Put each routes file's routes under the path that lazy-loads it, and
    /// read constant path segments. Recomputed from each route's in-file
    /// path (its qualified name), so a second run changes nothing
    /// (`postExtract`).
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let routes: Vec<Arc<Node>> = context
            .get_nodes_by_kind_shared(NodeKind::Route)
            .into_iter()
            .filter(|node| is_angular_route(node))
            .collect();
        if routes.is_empty() {
            return Some(Vec::new());
        }
        let route_files: BTreeSet<String> =
            routes.iter().map(|route| route.file_path.clone()).collect();
        let mounts = AngularMounts::read(context, &route_files);
        let constants = ConstantReader::new(context);
        let mut prefixes: HashMap<String, Vec<String>> = HashMap::new();
        let mut changed = Vec::new();
        for route in routes {
            let prefix = prefixes
                .entry(route.file_path.clone())
                .or_insert_with(|| mounts.prefix_of(&route.file_path, &constants))
                .clone();
            let mut segs = prefix;
            segs.extend(constants.resolved_segments(in_file_path(&route), &route.file_path));
            let name = join_path(&segs);
            if name != route.name {
                let mut renamed = route.as_ref().clone();
                renamed.name = name;
                changed.push(renamed);
            }
        }
        Some(changed)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if let Some(verb) = nav_verb(&reference.reference_name) {
            return resolve_navigation(reference, verb, context);
        }
        // Only a route this resolver emitted names its component this way.
        if !reference.from_node_id.ends_with(ROUTE_ID_SUFFIX) {
            return None;
        }
        let name = reference.reference_name.as_str();
        if let Some(layout) = name.strip_prefix(LAYOUT_PREFIX) {
            let component = route_component(layout, &reference.file_path, context)?;
            let mut metadata = serde_json::Map::new();
            metadata.insert("layout".to_string(), serde_json::Value::Bool(true));
            return Some(
                ResolvedRef::new(
                    reference.clone(),
                    component.id.clone(),
                    0.95,
                    ResolvedBy::Framework,
                )
                .with_edge(EdgeShape {
                    edge_kind: None,
                    metadata,
                    also_targets: Vec::new(),
                }),
            );
        }
        let (spec, member) = lazy_reference(name)?;
        let component = lazy_component(spec, member, &reference.file_path, context)?;
        Some(ResolvedRef::new(
            reference.clone(),
            component.id.clone(),
            0.95,
            ResolvedBy::Framework,
        ))
    }
}

/// A navigation call's edge to the route its destination names in the
/// caller's app (`resolve`'s navigation half).
fn resolve_navigation(
    reference: &RefView,
    verb: &str,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Only a call carries a destination.
    if reference.reference_kind != EdgeKind::Calls {
        return None;
    }
    let table = angular_table(context);
    let app = table.routes_for(&reference.file_path)?;
    if app.exact.is_empty() {
        return None;
    }
    let content = context.read_file_shared(&reference.file_path)?;
    let lines: Vec<&str> = content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let arg = first_argument_text(&lines, reference.line, reference.column, verb)?;
    // `navigate(['..', id], { relativeTo: this.route })` goes somewhere
    // relative to here.
    if nth_argument_text(&lines, reference.line, reference.column, verb, 1)
        .is_some_and(|extras| relative_to().is_match(&extras))
    {
        return None;
    }
    // The innermost class the call is written in, for its properties.
    let owner = context
        .get_nodes_in_file_shared(&reference.file_path)
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Class
                && node.start_line <= reference.line
                && node.end_line >= reference.line
        })
        .max_by_key(|node| node.start_line);
    let constants = ConstantReader::new(context);
    let href = angular_destination(
        &arg,
        &reference.file_path,
        owner.as_deref(),
        &constants,
        0,
        true,
    )?;
    let href = navigable_arms(&href)?;
    // Every arm of a conditional destination is somewhere this call goes:
    // the first is this reference's resolution, the rest ride along.
    let mut targets =
        destinations_for_href(&href, &app.exact, &app.dynamic, &reference.file_path).into_iter();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;
    use crate::types::ImportMapping;

    /// A file that imports the router, then `body`.
    fn routes_file(body: &str) -> String {
        format!("import {{ Routes }} from '@angular/router';\n{body}")
    }

    fn class(name: &str) -> ComponentRef {
        ComponentRef::Class(name.to_string())
    }

    fn lazy(spec: &str, member: Option<&str>) -> ComponentRef {
        ComponentRef::Lazy {
            spec: spec.to_string(),
            member: member.map(str::to_string),
        }
    }

    /// Upstream `parseAngularRoutes`, "reads screens, joining children to
    /// their parents, and skips what is not a screen".
    #[test]
    fn a_routes_array_names_its_screens_and_skips_what_is_no_screen() {
        let parsed = parse_angular_routes(&routes_file(
            "export const routes: Routes = [\n  { path: '', loadComponent: () => import('./home/home.component') },\n  { path: 'login', component: AuthComponent, canActivate: [guard] },\n  { path: 'editor', children: [\n      { path: '', loadComponent: () => import('./editor.component').then((m) => m.EditorComponent) },\n      { path: ':slug', loadComponent: () => import('./editor.component').then((m) => m.EditorComponent) },\n  ] },\n  { path: 'profile', loadChildren: () => import('./profile/profile.routes') },\n  { path: 'users', matcher: usersMatcher, component: UsersComponent },\n  { path: 'old', redirectTo: 'login' },\n  { path: '**', component: NotFoundComponent },\n];",
        ));
        let routes: Vec<(&str, Option<ComponentRef>)> = parsed
            .routes
            .iter()
            .map(|route| (route.path.as_str(), route.component.clone()))
            .collect();
        assert_eq!(
            routes,
            [
                ("/", Some(lazy("./home/home.component", None))),
                ("/login", Some(class("AuthComponent"))),
                (
                    "/editor",
                    Some(lazy("./editor.component", Some("EditorComponent")))
                ),
                (
                    "/editor/:slug",
                    Some(lazy("./editor.component", Some("EditorComponent")))
                ),
            ]
        );
        let mounts: Vec<(&str, &str)> = parsed
            .mounts
            .iter()
            .map(|mount| (mount.prefix.as_str(), mount.spec.as_str()))
            .collect();
        assert_eq!(mounts, [("/profile", "./profile/profile.routes")]);
        assert_eq!(
            parsed.redirects,
            [AngularRedirect {
                from: "/old".to_string(),
                to: "/login".to_string(),
                absolute: false,
            }]
        );
    }

    /// A layout whose `''` child redirects is no screen either: the child
    /// claims the address and sends the user on, as upstream's
    /// `childClaimsAddress` reads it. Of two entries of a file at one path,
    /// the first is the route (`seenPaths`).
    #[test]
    fn a_layout_whose_child_redirects_is_no_screen_and_a_path_is_its_first_entry() {
        let parsed = parse_angular_routes(&routes_file(
            "export const routes: Routes = [\n  { path: 'settings', component: SettingsComponent, children: [\n      { path: '', redirectTo: 'profile', pathMatch: 'full' },\n      { path: 'profile', component: ProfileComponent },\n  ] },\n  { path: 'help', component: HelpComponent },\n  { path: 'help', component: OtherHelpComponent },\n];",
        ));
        let routes: Vec<(&str, Option<ComponentRef>, Vec<ComponentRef>)> = parsed
            .routes
            .iter()
            .map(|route| {
                (
                    route.path.as_str(),
                    route.component.clone(),
                    route.layouts.clone(),
                )
            })
            .collect();
        assert_eq!(
            routes,
            [
                (
                    "/settings/profile",
                    Some(class("ProfileComponent")),
                    vec![class("SettingsComponent")]
                ),
                ("/help", Some(class("HelpComponent")), vec![]),
            ]
        );
        assert_eq!(
            parsed.redirects,
            [AngularRedirect {
                from: "/settings".to_string(),
                to: "/settings/profile".to_string(),
                absolute: false,
            }]
        );
    }

    /// A loader picks a member only off the module it is handed: `.then`'s
    /// parameter must be what the arrow reads the member from, as upstream's
    /// back-reference requires; `(await import(…)).X` picks `X`.
    #[test]
    fn a_loader_picks_a_member_only_off_what_it_is_handed() {
        let picked = lazy_import;
        assert_eq!(
            picked("() => import('./a').then((m) => m.A)"),
            Some(("./a".to_string(), Some("A".to_string())))
        );
        assert_eq!(
            picked("() => import(\"./a\").then(m => other.A)"),
            Some(("./a".to_string(), None))
        );
        assert_eq!(
            picked("async () => (await import(`./a`)).A"),
            Some(("./a".to_string(), Some("A".to_string())))
        );
        assert_eq!(picked("() => import('./a\")"), None);
    }

    /// Upstream: a route with children is a layout, a screen only where no
    /// child claims its address.
    #[test]
    fn a_route_with_children_is_a_layout() {
        let parsed = parse_angular_routes(&routes_file(
            "const routes: Routes = [\n  { path: ':username', component: ProfileComponent, children: [\n      { path: '', loadComponent: () => import('./articles.component') },\n      { path: 'favorites', loadComponent: () => import('./favorites.component') },\n  ] },\n  { path: 'pages', component: PagesComponent, children: [{ path: 'dashboard', component: DashboardComponent }] },\n];\nexport default routes;",
        ));
        let routes: Vec<(&str, Option<ComponentRef>, Vec<ComponentRef>)> = parsed
            .routes
            .iter()
            .map(|route| {
                (
                    route.path.as_str(),
                    route.component.clone(),
                    route.layouts.clone(),
                )
            })
            .collect();
        assert_eq!(
            routes,
            [
                (
                    "/:username",
                    Some(lazy("./articles.component", None)),
                    vec![class("ProfileComponent")]
                ),
                (
                    "/:username/favorites",
                    Some(lazy("./favorites.component", None)),
                    vec![class("ProfileComponent")]
                ),
                (
                    "/pages/dashboard",
                    Some(class("DashboardComponent")),
                    vec![class("PagesComponent")]
                ),
                ("/pages", Some(class("PagesComponent")), vec![]),
            ]
        );
    }

    /// Upstream: the arrays `forRoot`, `forChild`, `provideRouter` and a
    /// typed default export hold; an array that is no routes array, and a
    /// file that does not import the router, hold none.
    #[test]
    fn the_arrays_the_router_is_handed_hold_routes() {
        for body in [
            "RouterModule.forRoot([{ path: 'a', component: A }])",
            "RouterModule.forChild([{ path: 'a', component: A }])",
            "bootstrapApplication(App, { providers: [provideRouter([{ path: 'a', component: A }])] })",
            "export default [{ path: 'a', component: A }] satisfies Routes;",
            "const table = [{ path: 'a', component: A }] as Routes;",
        ] {
            let paths: Vec<String> = parse_angular_routes(&routes_file(body))
                .routes
                .into_iter()
                .map(|route| route.path)
                .collect();
            assert_eq!(paths, ["/a"], "{body}");
        }
        assert!(
            parse_angular_routes(&routes_file("const tabs = [{ path: 'a', component: A }];"))
                .routes
                .is_empty()
        );
        assert!(
            parse_angular_routes("export const routes = [{ path: 'a', component: A }];")
                .routes
                .is_empty()
        );
    }

    /// Upstream: a constant path is a placeholder for the cross-file pass,
    /// and a `$localize` default is read; a `**` redirect is where an
    /// unmatched address lands.
    #[test]
    fn a_constant_path_waits_for_the_cross_file_pass() {
        let parsed = parse_angular_routes(&routes_file(
            "export const routes: Routes = [\n  { path: internalRoutes.account.path, component: AccountComponent },\n  { path: $localize`:kebab-case@@routes.about:about`, component: AboutComponent },\n  { path: `issue/:${ProjectConst.IssueId}`, component: IssueComponent },\n  { path: internalRoutes.account.subRoutes.access.path + '/:id', component: AccessComponent },\n  { path: `${computed()}`, component: Lost },\n  { path: '**', redirectTo: 'home', pathMatch: 'full' },\n];",
        ));
        let paths: Vec<&str> = parsed
            .routes
            .iter()
            .map(|route| route.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "/{internalRoutes.account.path}",
                "/about",
                "/issue/:{ProjectConst.IssueId}",
                "/{internalRoutes.account.subRoutes.access.path}/:id",
            ]
        );
        assert_eq!(
            parsed.redirects,
            [AngularRedirect {
                from: "/**".to_string(),
                to: "/home".to_string(),
                absolute: false,
            }]
        );
    }

    /// Upstream "Angular destinations": a static string is literals,
    /// `$localize` defaults and `+` between them.
    #[test]
    fn a_static_string_is_literals_localize_defaults_and_plus() {
        assert_eq!(
            static_string("'/' + $localize`:kebab-case@@routes.about:about`").as_deref(),
            Some("/about")
        );
        assert_eq!(static_string("'/editor/' + slug"), None);
        assert_eq!(
            localize_default("$localize`Access`").as_deref(),
            Some("Access")
        );
        assert_eq!(static_string("`/a/b`").as_deref(), Some("/a/b"));
        assert_eq!(static_string("`/a/${b}`"), None);
    }

    /// KEEP-RUST: `children` is read only as an array written in place.
    /// Upstream takes the first `[` after the key, so `children: KIDS` read
    /// the next entry's children under `/a`, and that array was then never
    /// walked as `/b`'s.
    #[test]
    fn children_are_read_only_where_they_are_written() {
        let parsed = parse_angular_routes(&routes_file(
            "export const routes: Routes = [\n  { path: 'a', component: A, children: KIDS },\n  { path: 'b', component: B, children: [{ path: 'c', component: C }] },\n];",
        ));
        let routes: Vec<(&str, Vec<ComponentRef>)> = parsed
            .routes
            .iter()
            .map(|route| (route.path.as_str(), route.layouts.clone()))
            .collect();
        assert_eq!(
            routes,
            [("/a", vec![]), ("/b/c", vec![class("B")]), ("/b", vec![])]
        );
    }

    /// A route node per screen, its in-file path in the qualified name, and
    /// references to what it renders and the layouts around it.
    #[test]
    fn each_screen_references_its_component_and_layouts() {
        let file = "src/app/app.routes.ts";
        let extracted = AngularRouterResolver
            .extract(
                file,
                &routes_file(
                    "export const routes: Routes = [\n  { path: '', loadComponent: () => import('./home.component') },\n  { path: 'account', component: AccountComponent, children: [\n    { path: 'access', loadComponent: () => import('./access.component').then((m) => m.AccessComponent) },\n  ] },\n];",
                ),
                &FrameworkExtractionContext::without_config("/project"),
            )
            .expect("an extraction");
        let nodes: Vec<(&str, &str, &str, i64)> = extracted
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.as_str(),
                    node.name.as_str(),
                    node.qualified_name.as_str(),
                    node.start_line,
                )
            })
            .collect();
        assert_eq!(
            nodes,
            [
                (
                    "route:src/app/app.routes.ts:3:/:angular",
                    "/",
                    "src/app/app.routes.ts::route:/",
                    3
                ),
                (
                    "route:src/app/app.routes.ts:5:/account/access:angular",
                    "/account/access",
                    "src/app/app.routes.ts::route:/account/access",
                    5
                ),
                (
                    "route:src/app/app.routes.ts:4:/account:angular",
                    "/account",
                    "src/app/app.routes.ts::route:/account",
                    4
                ),
            ]
        );
        assert!(extracted.nodes.iter().all(is_angular_route));
        let references: Vec<(&str, &str, EdgeKind)> = extracted
            .references
            .iter()
            .map(|reference| {
                (
                    reference.from_node_id.as_str(),
                    reference.reference_name.as_str(),
                    reference.reference_kind,
                )
            })
            .collect();
        assert_eq!(
            references,
            [
                (
                    "route:src/app/app.routes.ts:3:/:angular",
                    "import:./home.component#default",
                    EdgeKind::References
                ),
                (
                    "route:src/app/app.routes.ts:5:/account/access:angular",
                    "import:./access.component#AccessComponent",
                    EdgeKind::References
                ),
                (
                    "route:src/app/app.routes.ts:5:/account/access:angular",
                    "layout:AccountComponent",
                    EdgeKind::References
                ),
                (
                    "route:src/app/app.routes.ts:4:/account:angular",
                    "AccountComponent",
                    EdgeKind::References
                ),
            ]
        );
        // A declaration file and a JavaScript file hold no routes.
        for other in ["src/app/app.routes.d.ts", "src/app/app.routes.js"] {
            let none = AngularRouterResolver
                .extract(
                    other,
                    &routes_file("export const routes: Routes = [{ path: 'a', component: A }];"),
                    &FrameworkExtractionContext::without_config("/project"),
                )
                .expect("an extraction");
            assert!(none.nodes.is_empty(), "{other}");
        }
    }

    /// A class node `name` in `file`.
    fn class_node(name: &str, file: &str, line: i64) -> Node {
        framework_node(
            format!("class:{file}:{name}"),
            NodeKind::Class,
            name.to_string(),
            name.to_string(),
            file.to_string(),
            line,
            line + 2,
            0,
            0,
            Language::TypeScript,
            true,
        )
    }

    /// The route nodes `file`'s routes arrays make.
    fn routes_of(file: &str, content: &str) -> Vec<Node> {
        AngularRouterResolver
            .extract(
                file,
                content,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .expect("an extraction")
            .nodes
    }

    /// `files` with the route nodes their routes arrays make, and a class
    /// node per `(name, file)` of `classes`.
    fn project(files: &[(&str, &str)], classes: &[(&str, &str)]) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for (file, content) in files {
            ctx = ctx.file(file, content);
        }
        for (file, content) in files {
            ctx = ctx.routes(file, routes_of(file, content));
        }
        for (name, file) in classes {
            ctx = ctx.routes(file, vec![class_node(name, file, 3)]);
        }
        ctx
    }

    /// A reference from the route `route_id` of `file`.
    fn route_ref(route_id: &str, file: &str, name: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: route_id.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language: Language::TypeScript,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    const COMPONENT: &str = "import { Component } from '@angular/core';\n";

    /// A lazy component is the member its loader picks, else the file's
    /// default-exported component, else its only component, else its
    /// default-exported class; only a route of this resolver's asks.
    #[test]
    fn a_lazy_component_is_the_member_its_loader_picks() {
        let routes = "src/app/app.routes.ts";
        let picked = format!(
            "{COMPONENT}@Component({{ template: '' }})\nexport class AComponent {{}}\n@Component({{ template: '' }})\nexport class BComponent {{}}\n"
        );
        let defaulted = format!(
            "{COMPONENT}@Component({{ template: '' }})\nexport class Helper {{}}\n@Component({{ template: '' }})\nexport default class Main {{}}\n"
        );
        let only = format!("{COMPONENT}@Component({{ template: '' }})\nexport class Only {{}}\n");
        let plain = "export default class Plain {}\n";
        let several = format!(
            "{COMPONENT}@Component({{ template: '' }})\nexport class One {{}}\n@Component({{ template: '' }})\nexport class Two {{}}\n"
        );
        let ctx = project(
            &[
                (routes, &routes_file("export const routes: Routes = [];")),
                ("src/app/picked.ts", &picked),
                ("src/app/defaulted.ts", &defaulted),
                ("src/app/only.ts", &only),
                ("src/app/plain.ts", plain),
                ("src/app/several.ts", &several),
            ],
            &[
                ("AComponent", "src/app/picked.ts"),
                ("BComponent", "src/app/picked.ts"),
                ("Helper", "src/app/defaulted.ts"),
                ("Main", "src/app/defaulted.ts"),
                ("Only", "src/app/only.ts"),
                ("Plain", "src/app/plain.ts"),
                ("One", "src/app/several.ts"),
                ("Two", "src/app/several.ts"),
            ],
        );
        let route = "route:src/app/app.routes.ts:2:/a:angular";
        let target = |name: &str| {
            AngularRouterResolver
                .resolve(&route_ref(route, routes, name), &ctx)
                .map(|resolved| resolved.target_node_id)
        };
        assert_eq!(
            target("import:./picked#BComponent").as_deref(),
            Some("class:src/app/picked.ts:BComponent")
        );
        assert_eq!(
            target("import:./defaulted#default").as_deref(),
            Some("class:src/app/defaulted.ts:Main")
        );
        assert_eq!(
            target("import:./only#default").as_deref(),
            Some("class:src/app/only.ts:Only")
        );
        assert_eq!(
            target("import:./plain#default").as_deref(),
            Some("class:src/app/plain.ts:Plain")
        );
        assert_eq!(target("import:./several#default"), None);
        assert_eq!(target("import:./missing#default"), None);
        assert_eq!(target("import:./picked#Absent"), None);
        // Another node's `import:` reference is no route's.
        let other = route_ref("function:f", routes, "import:./picked#BComponent");
        assert!(AngularRouterResolver.resolve(&other, &ctx).is_none());
        assert!(AngularRouterResolver.claims_reference("import:./picked#BComponent"));
        assert!(AngularRouterResolver.claims_reference("layout:AccountComponent"));
        assert!(!AngularRouterResolver.claims_reference("layout:"));
        assert!(!AngularRouterResolver.claims_reference("AccountComponent"));
    }

    /// A layout is the class its route file's named import of it names, else
    /// the file's own, or a lazy import's; an import that resolves nowhere
    /// names nothing.
    #[test]
    fn a_layout_is_the_class_its_import_names() {
        let routes = "src/app/app.routes.ts";
        let content = routes_file(
            "import { AccountComponent as Account } from './account.component';\nimport { Lost } from './lost.component';\nexport class LocalShell {}\nexport const routes: Routes = [];",
        );
        let ctx = project(
            &[
                (routes, &content),
                (
                    "src/app/account.component.ts",
                    "export class AccountComponent {}\n",
                ),
                ("src/app/shell.ts", "export default class Shell {}\n"),
            ],
            &[
                ("AccountComponent", "src/app/account.component.ts"),
                ("Account", "src/app/elsewhere.ts"),
                ("LocalShell", routes),
                ("Lost", "src/app/somewhere.ts"),
                ("Shell", "src/app/shell.ts"),
            ],
        );
        let route = "route:src/app/app.routes.ts:9:/x:angular";
        let layout = |name: &str| {
            AngularRouterResolver
                .resolve(&route_ref(route, routes, name), &ctx)
                .map(|resolved| {
                    let edge = resolved.edge.expect("a layout edge");
                    assert_eq!(edge.metadata.get("layout"), Some(&serde_json::json!(true)));
                    resolved.target_node_id
                })
        };
        // The local name of the import is what the route writes.
        assert_eq!(layout("layout:AccountComponent"), None);
        assert_eq!(layout("layout:Account"), None);
        assert_eq!(
            layout("layout:LocalShell").as_deref(),
            Some("class:src/app/app.routes.ts:LocalShell")
        );
        assert_eq!(layout("layout:Lost"), None);
        assert_eq!(
            layout("layout:import:./shell#default").as_deref(),
            Some("class:src/app/shell.ts:Shell")
        );
    }

    /// The renames the cross-file pass makes, by route id.
    fn renames(ctx: &RouteCtx) -> BTreeMap<String, String> {
        AngularRouterResolver
            .post_extract(ctx)
            .expect("a pass")
            .into_iter()
            .map(|node| (node.id, node.name))
            .collect()
    }

    /// The name each route of `file` has once the pass ran.
    fn named(ctx: &RouteCtx, file: &str) -> Vec<String> {
        let renamed = renames(ctx);
        let mut names: Vec<String> = ctx
            .nodes()
            .iter()
            .filter(|node| node.file_path == file && is_angular_route(node))
            .map(|node| renamed.get(&node.id).unwrap_or(&node.name).clone())
            .collect();
        names.sort();
        names
    }

    /// A routes file sits under the path that lazy-loads it: directly, by
    /// the routing module an NgModule imports, through a barrel, and under
    /// its loader's own mount. A file loaded from two places keeps the first
    /// by path.
    #[test]
    fn a_lazily_loaded_routes_file_sits_under_its_mount() {
        let app = routes_file(
            "export const routes: Routes = [\n  { path: 'home', component: Home },\n  { path: 'account', loadChildren: () => import('./account/account.routes') },\n  { path: 'admin', loadChildren: () => import('./admin/admin.module').then((m) => m.AdminModule) },\n  { path: 'lib', loadChildren: async () => (await import('./lib')).LibModule },\n];",
        );
        let account = routes_file(
            "export default [\n  { path: '', component: AccountComponent },\n  { path: 'deep', loadChildren: () => import('./deep.routes') },\n] satisfies Routes;",
        );
        let deep = routes_file("export const routes: Routes = [{ path: 'end', component: End }];");
        let admin_module = "import { NgModule } from '@angular/core';\nimport { AdminRoutingModule } from './admin-routing.module';\n@NgModule({ imports: [AdminRoutingModule] })\nexport class AdminModule {}\n";
        let admin_routing = routes_file(
            "const routes: Routes = [{ path: 'users', component: Users }];\nRouterModule.forChild(routes);",
        );
        let barrel = "export * from './lib.module';\n";
        let lib_module = "import { NgModule } from '@angular/core';\nimport { RouterModule } from '@angular/router';\n@NgModule({ imports: [RouterModule.forChild([{ path: 'x', component: X }])] })\nexport class LibModule {}\n";
        let other = routes_file(
            "export const routes: Routes = [{ path: 'zz', loadChildren: () => import('./account/account.routes') }];",
        );
        let ctx = project(
            &[
                ("src/app/app.routes.ts", &app),
                ("src/app/account/account.routes.ts", &account),
                ("src/app/account/deep.routes.ts", &deep),
                ("src/app/admin/admin.module.ts", admin_module),
                ("src/app/admin/admin-routing.module.ts", &admin_routing),
                ("src/app/lib/index.ts", barrel),
                ("src/app/lib/lib.module.ts", lib_module),
                ("src/app/zz.routes.ts", &other),
            ],
            &[],
        );
        assert_eq!(named(&ctx, "src/app/app.routes.ts"), ["/home"]);
        assert_eq!(
            named(&ctx, "src/app/account/account.routes.ts"),
            ["/account"]
        );
        assert_eq!(
            named(&ctx, "src/app/account/deep.routes.ts"),
            ["/account/deep/end"]
        );
        assert_eq!(
            named(&ctx, "src/app/admin/admin-routing.module.ts"),
            ["/admin/users"]
        );
        assert_eq!(named(&ctx, "src/app/lib/lib.module.ts"), ["/lib/x"]);
        // A second run renames nothing more.
        let renamed = renames(&ctx);
        let mut once = RouteCtx::default();
        for (file, content) in [
            ("src/app/app.routes.ts", app.as_str()),
            ("src/app/account/account.routes.ts", account.as_str()),
            ("src/app/account/deep.routes.ts", deep.as_str()),
            ("src/app/admin/admin.module.ts", admin_module),
            (
                "src/app/admin/admin-routing.module.ts",
                admin_routing.as_str(),
            ),
            ("src/app/lib/index.ts", barrel),
            ("src/app/lib/lib.module.ts", lib_module),
            ("src/app/zz.routes.ts", other.as_str()),
        ] {
            once = once.file(file, content);
        }
        for node in ctx.nodes() {
            let mut node = node.clone();
            if let Some(name) = renamed.get(&node.id) {
                node.name = name.clone();
            }
            let file = node.file_path.clone();
            once = once.routes(&file, vec![node]);
        }
        assert!(renames(&once).is_empty());
    }

    /// `ctx` with its files listed in `order`, and its routes in `routes`.
    struct Listed<'c> {
        ctx: &'c RouteCtx,
        order: Vec<String>,
        routes: Vec<Node>,
    }

    impl ResolutionContext for Listed<'_> {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.ctx.get_nodes_in_file(file_path)
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.ctx.get_nodes_by_name(name)
        }
        fn get_nodes_by_qualified_name(&self, q: &str) -> Vec<Node> {
            self.ctx.get_nodes_by_qualified_name(q)
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            if kind == NodeKind::Route {
                return self.routes.clone();
            }
            self.ctx.get_nodes_by_kind(kind)
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.ctx.file_exists(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.ctx.read_file(file_path)
        }
        fn get_project_root(&self) -> &str {
            self.ctx.get_project_root()
        }
        fn get_all_files(&self) -> Vec<String> {
            self.order.clone()
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.ctx.get_nodes_by_lower_name(lower)
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.ctx.get_node_by_id(id)
        }
        fn get_import_mappings(&self, file: &str, language: Language) -> Vec<ImportMapping> {
            self.ctx.get_import_mappings(file, language)
        }
    }

    /// A routes file two files mount sits under the mount of the first in
    /// path order, whatever order the files and the routes are listed in:
    /// the first mount a file gets is kept, so the walk over the mounts
    /// must not follow the listing.
    #[test]
    fn a_routes_file_mounted_twice_keeps_the_first_mount_by_path() {
        let app = routes_file(
            "export const routes: Routes = [{ path: 'profile', loadChildren: () => import('./profile/profile.routes') }];",
        );
        let admin = routes_file(
            "export const routes: Routes = [{ path: 'people', loadChildren: () => import('./profile/profile.routes') }];",
        );
        let profile =
            routes_file("export default [{ path: ':username', component: P }] satisfies Routes;");
        let files = [
            ("src/app/app.routes.ts", app.as_str()),
            ("src/app/admin.routes.ts", admin.as_str()),
            ("src/app/profile/profile.routes.ts", profile.as_str()),
        ];
        let ctx = project(&files, &[]);
        let routes: Vec<Node> = ctx
            .nodes()
            .iter()
            .filter(|node| is_angular_route(node))
            .cloned()
            .collect();
        let forward: Vec<String> = files.iter().map(|(file, _)| file.to_string()).collect();
        let backward: Vec<String> = forward.iter().rev().cloned().collect();
        let mut reversed = routes.clone();
        reversed.reverse();
        for (order, routes) in [(forward, routes.clone()), (backward, reversed)] {
            let listed = Listed {
                ctx: &ctx,
                order,
                routes,
            };
            let names: Vec<String> = AngularRouterResolver
                .post_extract(&listed)
                .expect("a pass")
                .into_iter()
                .map(|node| node.name)
                .collect();
            assert_eq!(names, ["/people/:username"]);
        }
    }

    /// A constant segment is read from its declaration: an object's field
    /// chain, a destructured or plain alias of it, an enum member, a class's
    /// static field or object, a `$localize` value, the module a barrel
    /// forwards it from, and a constant inside a segment. One that reads no
    /// static string stays a placeholder.
    #[test]
    fn a_constant_segment_is_read_from_its_declaration() {
        let constants = "export const internalRoutes = {\n  account: { path: 'account', subRoutes: { access: { path: 'access' } } },\n  about: { path: $localize`:kebab-case@@routes.about:about` },\n  computed: { path: build() },\n};\nexport enum Paths { Board = 'board' }\nexport class ProjectConst {\n  static readonly IssueId = 'issueId';\n  static Configuration = { Lyrics: 'lyrics' };\n}\n";
        let routes = routes_file(
            "import { internalRoutes, Paths, ProjectConst } from './constants';\nimport { Shared } from './barrel';\nconst { about } = internalRoutes;\nconst access = internalRoutes.account.subRoutes.access;\nexport const routes: Routes = [\n  { path: internalRoutes.account.path, component: A },\n  { path: about.path, component: B },\n  { path: access.path + '/:id', component: C },\n  { path: Paths.Board, component: D },\n  { path: `issue/:${ProjectConst.IssueId}`, component: E },\n  { path: ProjectConst.Configuration.Lyrics, component: F },\n  { path: Shared.home.path, component: G },\n  { path: internalRoutes.computed.path, component: H },\n  { path: Unknown.thing, component: I },\n];",
        );
        let ctx = project(
            &[
                ("src/app/app.routes.ts", &routes),
                ("src/app/constants.ts", constants),
                ("src/app/barrel.ts", "export { Shared } from './shared';\n"),
                (
                    "src/app/shared.ts",
                    "export const Shared = { home: { path: 'start' } };\n",
                ),
            ],
            &[],
        );
        assert_eq!(
            named(&ctx, "src/app/app.routes.ts"),
            [
                "/about",
                "/access/:id",
                "/account",
                "/board",
                "/issue/:issueId",
                "/lyrics",
                "/start",
                "/{Unknown.thing}",
                "/{internalRoutes.computed.path}",
            ]
        );
    }

    /// KEEP-RUST: a constant's nested object is read only where it is written
    /// in place. Upstream takes the first `{` after the key, so
    /// `paths.account.path` read the next field's object (`'other'`).
    #[test]
    fn a_nested_constant_object_is_read_only_where_it_is_written() {
        let routes = routes_file(
            "import { paths } from './paths';\nexport const routes: Routes = [{ path: paths.account.path, component: A }];",
        );
        let ctx = project(
            &[
                ("src/app/app.routes.ts", &routes),
                (
                    "src/app/paths.ts",
                    "export const paths = {\n  account: ACCOUNT,\n  other: { path: 'other' },\n};\n",
                ),
            ],
            &[],
        );
        assert_eq!(
            named(&ctx, "src/app/app.routes.ts"),
            ["/{paths.account.path}"]
        );
    }

    /// KEEP-RUST: in a cycle of mounts each file takes the prefixes of its
    /// own chain up to the file that repeats, whatever order the routes are
    /// renamed in; an alias that leads back to itself reads nothing.
    #[test]
    fn a_cycle_of_mounts_or_aliases_settles() {
        let a = routes_file(
            "export const routes: Routes = [\n  { path: 'a', component: A },\n  { path: 'to-b', loadChildren: () => import('./b.routes') },\n];",
        );
        let b = routes_file(
            "export const routes: Routes = [\n  { path: 'b', component: B },\n  { path: 'to-a', loadChildren: () => import('./a.routes') },\n];",
        );
        let forward = project(&[("src/a.routes.ts", &a), ("src/b.routes.ts", &b)], &[]);
        let backward = project(&[("src/b.routes.ts", &b), ("src/a.routes.ts", &a)], &[]);
        for ctx in [&forward, &backward] {
            assert_eq!(named(ctx, "src/a.routes.ts"), ["/to-a/a"]);
            assert_eq!(named(ctx, "src/b.routes.ts"), ["/to-b/b"]);
        }
        let looped = routes_file(
            "const a = a.b;\nexport const routes: Routes = [{ path: a.x, component: A }];",
        );
        let ctx = project(&[("src/loop.routes.ts", &looped)], &[]);
        assert_eq!(named(&ctx, "src/loop.routes.ts"), ["/{a.x}"]);
    }

    /// One reader reads constant after constant (W7-18d's cost revision
    /// keeps its compiled patterns, declaring sources and declaring files
    /// for its life): two members of one enum, a root two files each
    /// declare, and a root two files import from different modules are each
    /// read for themselves, in either order, as a fresh reader reads them.
    #[test]
    fn one_reader_reads_each_constant_for_itself() {
        let ctx = RouteCtx::default()
            .file(
                "src/a/paths.ts",
                "export enum Paths { Board = 'board', Lyrics = 'lyrics' }\nexport const shared = { home: { path: 'a-home' } };\n",
            )
            .file(
                "src/b/paths.ts",
                "export const shared = { home: { path: 'b-home' } };\n",
            )
            .file(
                "src/a/app.routes.ts",
                "import { Paths, shared } from './paths';\nconst local = { path: 'a-local' };\n",
            )
            .file(
                "src/b/app.routes.ts",
                "import { shared } from './paths';\nconst local = { path: 'b-local' };\n",
            );
        let reads = [
            ("Paths.Board", "src/a/app.routes.ts", "board"),
            ("Paths.Lyrics", "src/a/app.routes.ts", "lyrics"),
            ("local.path", "src/a/app.routes.ts", "a-local"),
            ("local.path", "src/b/app.routes.ts", "b-local"),
            ("shared.home.path", "src/a/app.routes.ts", "a-home"),
            ("shared.home.path", "src/b/app.routes.ts", "b-home"),
        ];
        for (expr, file, value) in reads {
            let fresh = ConstantReader::new(&ctx).constant_value(expr, file);
            assert_eq!(fresh.as_deref(), Some(value), "{expr} in {file}");
        }
        let forward: Vec<_> = reads.to_vec();
        let backward: Vec<_> = reads.iter().rev().copied().collect();
        for order in [forward, backward] {
            let reader = ConstantReader::new(&ctx);
            for (expr, file, value) in order {
                assert_eq!(
                    reader.constant_value(expr, file).as_deref(),
                    Some(value),
                    "{expr} in {file}"
                );
            }
        }
    }

    /// Two properties of one class lead where their own initializers say,
    /// read through one reader, again and in either spelling (W7-18d's cost
    /// revision compiles the pattern for a property's name once per reader).
    #[test]
    fn each_property_of_a_class_leads_where_its_initializer_says() {
        let file = "src/app/nav.component.ts";
        let ctx = RouteCtx::default().file(
            file,
            "export class NavComponent {\n  routerLinkAbout = ['/about'];\n  public routerLinkHome = ['/home'];\n}\n",
        );
        let mut owner = class_node("NavComponent", file, 1);
        owner.end_line = 4;
        let destinations = LinkDestinations::new(&ctx);
        let path = |expr: &str| destinations.of(expr, file, &owner).map(|href| href.path);
        assert_eq!(path("routerLinkAbout").as_deref(), Some("/about"));
        assert_eq!(path("this.routerLinkHome").as_deref(), Some("/home"));
        assert_eq!(path("this.routerLinkAbout").as_deref(), Some("/about"));
        assert_eq!(path("routerLinkHome").as_deref(), Some("/home"));
    }

    /// `files` indexed as a sync leaves them: each file's routes renamed
    /// under its mount, and a class node per `(name, file, start, end)`.
    fn indexed(files: &[(&str, &str)], classes: &[(&str, &str, i64, i64)]) -> RouteCtx {
        let extracted = project(files, &[]);
        let renamed = renames(&extracted);
        let mut ctx = RouteCtx::default();
        for (file, content) in files {
            ctx = ctx.file(file, content);
        }
        for node in extracted.nodes() {
            let mut node = node.clone();
            if let Some(name) = renamed.get(&node.id) {
                node.name = name.clone();
            }
            let file = node.file_path.clone();
            ctx = ctx.routes(&file, vec![node]);
        }
        for (name, file, start, end) in classes {
            let mut class = class_node(name, file, *start);
            class.end_line = *end;
            ctx = ctx.routes(file, vec![class]);
        }
        ctx
    }

    /// Where the navigation call `name` on `line` of `file` goes: each
    /// target route's name with the `href` its edge carries.
    fn navigation(
        ctx: &dyn ResolutionContext,
        file: &str,
        name: &str,
        line: i64,
    ) -> Vec<(String, String)> {
        let reference = crate::frameworks::route_href::test_ctx::call_in(file, name, line, 0);
        let Some(resolved) = AngularRouterResolver.resolve(&reference, ctx) else {
            return Vec::new();
        };
        let edge = resolved.edge.expect("a navigation edge");
        assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
        assert_eq!(resolved.confidence, 0.95);
        let verb = nav_verb(name).expect("a navigation call");
        let mut out = Vec::new();
        let mut push = |target: &str, metadata: &serde_json::Map<String, serde_json::Value>| {
            assert_eq!(metadata.get("navMethod"), Some(&serde_json::json!(verb)));
            let route = ctx
                .get_node_by_id(target)
                .map_or_else(|| target.to_string(), |node| node.name);
            let href = metadata
                .get("href")
                .and_then(|href| href.as_str())
                .unwrap_or_default()
                .to_string();
            out.push((route, href));
        };
        push(&resolved.target_node_id, &edge.metadata);
        for also in &edge.also_targets {
            push(&also.target_node_id, &also.metadata);
        }
        out
    }

    /// The route each call of `calls` (a line of `file`) reaches, or `-`.
    fn reached_by(ctx: &dyn ResolutionContext, file: &str, calls: &[(i64, &str)]) -> Vec<String> {
        calls
            .iter()
            .map(|(line, name)| {
                let targets = navigation(ctx, file, name, *line);
                if targets.is_empty() {
                    "-".to_string()
                } else {
                    targets
                        .iter()
                        .map(|(route, _)| route.as_str())
                        .collect::<Vec<_>>()
                        .join(" | ")
                }
            })
            .collect()
    }

    /// Upstream "Angular destinations": an absolute command array, holes for
    /// what is computed, nothing for a relative or all-hole one; from the
    /// root, a call's bare first segment counts too (#2133).
    #[test]
    fn a_command_array_names_a_place_only_when_it_is_absolute() {
        let display =
            |text: &str, from_root: bool| commands_href(text, from_root).map(|href| href.display);
        assert_eq!(
            display("['/article', article.slug]", false).as_deref(),
            Some("/article/${…}")
        );
        assert_eq!(
            display("['/profile', p.username, 'favorites']", false).as_deref(),
            Some("/profile/${…}/favorites")
        );
        assert_eq!(display("['/']", false).as_deref(), Some("/"));
        assert_eq!(
            display("['/users', { tab: 'x' }, id]", false).as_deref(),
            Some("/users/${…}")
        );
        assert_eq!(display("['../', id]", false), None);
        assert_eq!(display("['edit']", false), None);
        assert_eq!(display("[`/${a}`, b, c]", false), None);
        assert_eq!(display("['/', id]", false), None);
        assert_eq!(display("[]", true), None);
        assert_eq!(
            display("['project', 'issue', id]", true).as_deref(),
            Some("/project/issue/${…}")
        );
        assert_eq!(display("['./edit']", true), None);
        assert_eq!(display("['/a', 'b'", true), None);
    }

    /// A destination is read through a property of the calling class, a
    /// route constant (or one that builds its commands), `.concat` and a
    /// literal followed by one segment; a relative `relativeTo` call, a
    /// computed template and a concatenation that continues a segment reach
    /// nothing, and a relative arm of a conditional drops out first.
    #[test]
    fn a_destination_is_read_through_properties_constants_and_concat() {
        let routes = routes_file(
            "export const routes: Routes = [\n  { path: 'home', component: Home },\n  { path: 'home/zen', component: Zen },\n  { path: 'home/zen/:id', component: ZenItem },\n  { path: 'account/access/:id', component: Access },\n  { path: 'editor/:slug', component: Editor },\n  { path: 'article/:slug', component: Article },\n  { path: 'project/board', component: Board },\n  { path: 'redirect/:path', component: Redirect },\n  { path: ':page', component: Page },\n];",
        );
        let constants = "export const internalRoutes = {\n  zen: { routerLink: ['/home', 'zen'] },\n  access: { routerLink: (id: string) => ['/account', 'access', id] },\n};\n";
        let page = [
            "import { Router } from '@angular/router';",
            "import { internalRoutes } from './routes.constants';",
            "export class PageComponent {",
            "  protected readonly routerLinkZen = internalRoutes.zen.routerLink;",
            "  constructor(private router: Router) {}",
            "  a() { this.router.navigate(this.routerLinkZen); }",
            "  b() { this.router.navigate(internalRoutes.access.routerLink('me')); }",
            "  c(id) { this.router.navigate(this.routerLinkZen.concat(id)); }",
            "  d(slug) { this.router.navigateByUrl('/editor/' + slug); }",
            "  e(path) { this.router.navigateByUrl('/redirect' + path); }",
            "  f() { this.router.navigate(['project', 'board']); }",
            "  g(id) { this.router.navigate(['..', id], { relativeTo: this.route }); }",
            "  h() { this.router.navigate([], { queryParams: { page: 2 } }); }",
            "  i(ok) { this.router.navigateByUrl(ok ? 'relative' : '/home'); }",
            "  j(slug) { this.router.navigateByUrl(`/article/${slug}`); }",
            "  k(slug) { this.router.navigateByUrl(`/article${slug}`); }",
            "  l() { this.router.navigate(internalRoutes.zen.routerLink); }",
            "  m() { this.router.navigate(['project', 'board'], { relativeTo: this.route }); }",
            "  n(slug) { this.router.navigateByUrl(`/${slug}`); }",
            "}",
        ]
        .join("\n");
        let file = "src/app/page.component.ts";
        let ctx = indexed(
            &[
                ("src/app/app.routes.ts", &routes),
                ("src/app/routes.constants.ts", constants),
                (file, &page),
            ],
            &[("PageComponent", file, 3, 20)],
        );
        assert_eq!(
            reached_by(
                &ctx,
                file,
                &[
                    (6, "this.router.navigate"),
                    (7, "this.router.navigate"),
                    (8, "this.router.navigate"),
                    (9, "this.router.navigateByUrl"),
                    (10, "this.router.navigateByUrl"),
                    (11, "this.router.navigate"),
                    (12, "this.router.navigate"),
                    (13, "this.router.navigate"),
                    (14, "this.router.navigateByUrl"),
                    (15, "this.router.navigateByUrl"),
                    (16, "this.router.navigateByUrl"),
                    (17, "this.router.navigate"),
                    (18, "this.router.navigate"),
                    (19, "this.router.navigateByUrl"),
                ]
            ),
            [
                "/home/zen",
                "/account/access/:id",
                "/home/zen/:id",
                "/editor/:slug",
                "-",
                "/project/board",
                "-",
                "-",
                "/home",
                "/article/:slug",
                "-",
                "/home/zen",
                "-",
                "-",
            ]
        );
        assert_eq!(
            navigation(&ctx, file, "this.router.navigateByUrl", 15),
            [("/article/:slug".to_string(), "/article/${…}".to_string())]
        );
        // Outside the class, a property is no destination.
        let outside = indexed(
            &[
                ("src/app/app.routes.ts", &routes),
                ("src/app/routes.constants.ts", constants),
                (file, &page),
            ],
            &[],
        );
        assert!(navigation(&outside, file, "this.router.navigate", 6).is_empty());
    }

    /// Only a call of the router navigates, with its own verb.
    #[test]
    fn a_navigation_is_a_call_of_the_router() {
        let routes =
            routes_file("export const routes: Routes = [{ path: 'login', component: Login }];");
        let nav = [
            "export class NavComponent {",
            "  a() { this._router.navigateByUrl('/login'); }",
            "  b() { router.parseUrl('/login'); }",
            "  c() { this.router.createUrlTree(['/login']); }",
            "}",
        ]
        .join("\n");
        let file = "src/app/nav.component.ts";
        let ctx = indexed(&[("src/app/app.routes.ts", &routes), (file, &nav)], &[]);
        assert_eq!(
            reached_by(
                &ctx,
                file,
                &[
                    (2, "this._router.navigateByUrl"),
                    (3, "router.parseUrl"),
                    (4, "this.router.createUrlTree"),
                ]
            ),
            ["/login", "/login", "/login"]
        );
        for name in [
            "this.router.navigate",
            "router.navigateByUrl",
            "this._router.createUrlTree",
            "Router.parseUrl",
        ] {
            assert!(AngularRouterResolver.claims_reference(name), "{name}");
        }
        for name in ["this.location.go", "this.myRouter.navigate", "navigate"] {
            assert!(!AngularRouterResolver.claims_reference(name), "{name}");
        }
        let mut reference =
            crate::frameworks::route_href::test_ctx::call_in(file, "router.parseUrl", 3, 0);
        reference.reference_kind = EdgeKind::References;
        assert!(AngularRouterResolver.resolve(&reference, &ctx).is_none());
        assert_eq!(
            AngularNavigation.tails(),
            ["navigate", "navigateByUrl", "createUrlTree", "parseUrl"]
        );
    }

    /// KEEP-RUST: whatever order the routes and files are listed in, a path
    /// two routes of an app serve reaches neither; a redirect is followed
    /// through other redirects at most four hops; a route serving a path
    /// keeps it from a redirect; two redirects of one path to different
    /// routes reach neither, to the same route reach it; and `/` that no
    /// route or redirect serves takes the root `**` redirect's route, unless
    /// two disagree.
    #[test]
    fn the_route_table_settles_whatever_the_order() {
        let one = routes_file(
            "export const routes: Routes = [\n  { path: 'dup', component: A },\n  { path: 'home', component: H },\n  { path: 'end', component: E },\n  { path: 'x1', component: X1 },\n  { path: 'x2', component: X2 },\n  { path: 'a', redirectTo: 'b' },\n  { path: 'b', redirectTo: 'c' },\n  { path: 'c', redirectTo: 'd' },\n  { path: 'd', redirectTo: 'end' },\n  { path: 'v', redirectTo: 'w' },\n  { path: 'w', redirectTo: 'x' },\n  { path: 'x', redirectTo: 'y' },\n  { path: 'y', redirectTo: 'z' },\n  { path: 'z', redirectTo: 'end' },\n  { path: 'go', redirectTo: 'x1' },\n  { path: 'same', redirectTo: 'x1' },\n  { path: '**', redirectTo: 'home' },\n];",
        );
        let two = routes_file(
            "export const routes: Routes = [\n  { path: 'dup', component: B },\n  { path: 'home', redirectTo: 'x2' },\n  { path: 'go', redirectTo: 'x2' },\n  { path: 'same', redirectTo: '/x1' },\n];",
        );
        let nav = [
            "export class NavComponent {",
            "  a() { this.router.navigate(['/dup']); }",
            "  b() { this.router.navigate(['/a']); }",
            "  c() { this.router.navigate(['/v']); }",
            "  d() { this.router.navigate(['/home']); }",
            "  e() { this.router.navigate(['/go']); }",
            "  f() { this.router.navigate(['/same']); }",
            "  g() { this.router.navigate(['/']); }",
            "}",
        ]
        .join("\n");
        let calls: Vec<(i64, &str)> = (2..=8).map(|line| (line, "this.router.navigate")).collect();
        let expected = ["-", "/end", "-", "/home", "-", "/x1", "/home"];
        let file = "src/app/nav.component.ts";
        let files = [
            ("src/app/one.routes.ts", one.as_str()),
            ("src/app/two.routes.ts", two.as_str()),
            (file, nav.as_str()),
        ];
        let ctx = indexed(&files, &[]);
        let routes: Vec<Node> = ctx.nodes().to_vec();
        let forward: Vec<String> = files.iter().map(|(file, _)| file.to_string()).collect();
        let backward: Vec<String> = forward.iter().rev().cloned().collect();
        let mut reversed = routes.clone();
        reversed.reverse();
        for (order, routes) in [(forward.clone(), routes.clone()), (backward, reversed)] {
            let listed = Listed {
                ctx: &ctx,
                order,
                routes: routes
                    .into_iter()
                    .filter(|node| node.kind == NodeKind::Route)
                    .collect(),
            };
            assert_eq!(reached_by(&listed, file, &calls), expected);
        }
        // Two root `**` redirects to different routes leave `/` unserved (a
        // file's redirects are read where it declares a screen or a mount).
        let three = routes_file(
            "export const routes: Routes = [{ path: 'other', component: O }, { path: '**', redirectTo: 'end' }];",
        );
        let ctx = indexed(
            &[
                ("src/app/one.routes.ts", one.as_str()),
                ("src/app/three.routes.ts", three.as_str()),
                (file, nav.as_str()),
            ],
            &[],
        );
        assert_eq!(
            reached_by(&ctx, file, &[(8, "this.router.navigate")]),
            ["-"]
        );
    }

    /// A redirect in a file that only mounts others sits under that file's
    /// mount, and a mounted file's relative redirect under its own (#2133,
    /// jira-clone); a route with a constant the pass could not read is in no
    /// table (KEEP-RUST), so a hole does not reach it.
    /// A destination that leaves the site (`//host/…`, a scheme) names no
    /// route of this app, written alone or as either arm of a conditional
    /// (KEEP-RUST: upstream matches an absolute URL's path).
    #[test]
    fn an_off_site_destination_names_nothing() {
        let routes = routes_file(
            "export const routes: Routes = [\n  { path: 'login', component: Login },\n  { path: 'home', component: Home },\n];",
        );
        let nav = [
            "export class NavComponent {",
            "  a() { this.router.navigateByUrl('//auth.example.com/login'); }",
            "  b(c) { this.router.navigateByUrl(c ? '//auth.example.com/login' : '/home'); }",
            "  d(c) { this.router.navigateByUrl(c ? '/home' : '//auth.example.com/login'); }",
            "  e() { this.router.navigateByUrl('https://auth.example.com/login'); }",
            "  f() { this.router.navigateByUrl('/login'); }",
            "}",
        ]
        .join("\n");
        let file = "src/app/nav.component.ts";
        let ctx = indexed(&[("src/app/app.routes.ts", &routes), (file, &nav)], &[]);
        assert_eq!(
            reached_by(
                &ctx,
                file,
                &[
                    (2, "this.router.navigateByUrl"),
                    (3, "this.router.navigateByUrl"),
                    (4, "this.router.navigateByUrl"),
                    (5, "this.router.navigateByUrl"),
                    (6, "this.router.navigateByUrl"),
                ]
            ),
            ["-", "/home", "/home", "-", "/login"]
        );
    }

    #[test]
    fn redirects_sit_under_their_mounts_and_unread_constants_name_nothing() {
        let app = routes_file(
            "export const routes: Routes = [\n  { path: 'project', loadChildren: () => import('./project/project.routes').then((m) => m.ROUTES) },\n  { path: '', redirectTo: 'project', pathMatch: 'full' },\n  { path: lost.path, redirectTo: '/project' },\n  { path: lost.where, loadChildren: () => import('./lost/lost.routes').then((m) => m.ROUTES) },\n];",
        );
        let lost_routes = routes_file(
            "export const ROUTES: Routes = [\n  { path: 'screen', component: LostScreen },\n  { path: 'old', redirectTo: '/project/board' },\n];",
        );
        let project_routes = routes_file(
            "export const ROUTES: Routes = [\n  { path: '', component: Shell, children: [\n      { path: 'board', component: Board },\n      { path: '', redirectTo: 'board', pathMatch: 'full' },\n  ] },\n  { path: 'users/' + lost.path, component: Lost },\n  { path: lost.path + '/:id', component: AlsoLost },\n];",
        );
        let nav = [
            "export class NavComponent {",
            "  a() { this.router.navigate(['/']); }",
            "  b() { this.router.navigate(['/project']); }",
            "  c(id) { this.router.navigate(['/project/users', id]); }",
            "  d(a, b) { this.router.navigate(['/project', a, b]); }",
            "  e() { this.router.navigateByUrl('/{lost.path}'); }",
            "  f() { this.router.navigateByUrl('/{lost.where}/old'); }",
            "}",
        ]
        .join("\n");
        let file = "src/app/nav.component.ts";
        let ctx = indexed(
            &[
                ("src/app/app.routes.ts", &app),
                ("src/app/project/project.routes.ts", &project_routes),
                ("src/app/lost/lost.routes.ts", &lost_routes),
                (file, &nav),
            ],
            &[],
        );
        assert_eq!(
            reached_by(
                &ctx,
                file,
                &[
                    (2, "this.router.navigate"),
                    (3, "this.router.navigate"),
                    (4, "this.router.navigate"),
                    (5, "this.router.navigate"),
                    (6, "this.router.navigateByUrl"),
                    (7, "this.router.navigateByUrl"),
                ]
            ),
            ["/project/board", "/project/board", "-", "-", "-", "-"]
        );
    }

    /// An Angular workspace (the directory of an `angular.json` or an Nx
    /// `nx.json`) is one app, so a library's call reaches the routes another
    /// library declares; without one, a file under no app's root uses the
    /// only app's routes, and none when there are two.
    #[test]
    fn an_angular_workspace_is_one_app() {
        let x = routes_file("export const routes: Routes = [{ path: 'x', component: X }];");
        let y = routes_file("export const routes: Routes = [{ path: 'y', component: Y }];");
        let nav = "export function go(router) {\n  router.navigate(['/x']);\n}\n";
        let call = [(2, "router.navigate")];
        let files = vec![
            ("apps/x/src/app/x.routes.ts", x.as_str()),
            ("apps/y/src/app/y.routes.ts", y.as_str()),
            ("libs/shared/src/nav.ts", nav),
        ];
        let two_apps = indexed(&files, &[]);
        assert_eq!(
            reached_by(&two_apps, "libs/shared/src/nav.ts", &call),
            ["-"]
        );
        let mut with_nx = files.clone();
        with_nx.push(("nx.json", "{}"));
        let workspace = indexed(&with_nx, &[]);
        assert_eq!(
            reached_by(&workspace, "libs/shared/src/nav.ts", &call),
            ["/x"]
        );
        let route = workspace
            .nodes()
            .iter()
            .find(|node| node.name == "/x")
            .expect("the /x route");
        assert_eq!(
            AngularNavigation.scope(route, &workspace),
            Some(vec![String::new()])
        );
        assert_eq!(
            AngularNavigation.scope(route, &two_apps),
            Some(vec!["apps/x/".to_string()])
        );
        let mut one_workspace = files.clone();
        one_workspace.push(("apps/x/angular.json", "{}"));
        let one = indexed(&one_workspace, &[]);
        assert_eq!(reached_by(&one, "libs/shared/src/nav.ts", &call), ["-"]);
        let single = indexed(
            &files[..1]
                .iter()
                .copied()
                .chain([files[2]])
                .collect::<Vec<_>>(),
            &[],
        );
        assert_eq!(reached_by(&single, "libs/shared/src/nav.ts", &call), ["/x"]);
        assert_eq!(
            AngularNavigation.scope(&class_node("X", "x.ts", 1), &single),
            None
        );
    }

    /// An app whose routes all hold a constant the cross-file pass could not
    /// read is still an app: neither its own calls nor a shared library's
    /// fall back to the other app's routes, which answer only their app.
    #[test]
    fn an_app_of_unread_routes_is_still_an_app() {
        let x = routes_file("export const routes: Routes = [{ path: 'x', component: X }];");
        let y = routes_file("export const routes: Routes = [{ path: lost.path, component: Y }];");
        let nav = "export function go(router) {\n  router.navigate(['/x']);\n}\n";
        let call = [(2, "router.navigate")];
        let ctx = indexed(
            &[
                ("apps/x/src/app/x.routes.ts", &x),
                ("apps/y/src/app/y.routes.ts", &y),
                ("apps/x/src/app/nav.ts", nav),
                ("apps/y/src/app/nav.ts", nav),
                ("libs/shared/src/nav.ts", nav),
            ],
            &[],
        );
        assert_eq!(reached_by(&ctx, "apps/x/src/app/nav.ts", &call), ["/x"]);
        assert_eq!(reached_by(&ctx, "apps/y/src/app/nav.ts", &call), ["-"]);
        assert_eq!(reached_by(&ctx, "libs/shared/src/nav.ts", &call), ["-"]);
        let route = ctx
            .nodes()
            .iter()
            .find(|node| node.name == "/x")
            .expect("the /x route");
        assert_eq!(
            AngularNavigation.scope(route, &ctx),
            Some(vec!["apps/x/".to_string()])
        );
    }

    #[test]
    fn angular_is_detected_by_its_dependency() {
        let with = |manifest: &str| RouteCtx::default().file("package.json", manifest);
        assert!(
            AngularRouterResolver.detect(&with("{\"dependencies\":{\"@angular/router\":\"19\"}}"))
        );
        assert!(
            AngularRouterResolver.detect(&with("{\"dependencies\":{\"@angular/core\":\"19\"}}"))
        );
        assert!(!AngularRouterResolver.detect(&with("{\"dependencies\":{\"react\":\"19\"}}")));
    }
}
