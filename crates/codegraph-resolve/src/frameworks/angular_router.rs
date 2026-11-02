//! Angular Router (upstream `frameworks/angular-router.ts` at `e8ddc4e2`,
//! which is `b635dd46` with `escapeRegExp` on the names its declaration
//! patterns build): routes declared in `Routes` arrays, the components they
//! render, and the path each sits at once its lazily loaded file is mounted.
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

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{
    JS_SPACE, ObjectField, is_js_space, js_trim, match_bracket, read_fields, skip_string,
    top_level_objects,
};
use super::route_href::read_string_at;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::import_resolver::resolve_import_path;
use crate::strip_comments::strip_js_comments_keeping_offsets;
use crate::types::{
    EdgeShape, FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy,
    ResolvedRef,
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

/// What the cross-file pass reads while it names one pass's routes: the
/// constants it looked up, by the file they were read from.
struct ConstantReader<'c> {
    context: &'c dyn ResolutionContext,
    memo: RefCell<HashMap<(String, String), Option<String>>>,
}

impl<'c> ConstantReader<'c> {
    fn new(context: &'c dyn ResolutionContext) -> Self {
        Self {
            context,
            memo: RefCell::new(HashMap::new()),
        }
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
        let file = declaring_file(root, from_file, self.context)?;
        let content = self.declaration_source(root, &file, 0)?;
        let safe = strip_js_comments_keeping_offsets(&content);
        let name = regex::escape(root);
        let declared = regex(&format!(
            r"(?-u:\b)(?:const|let){s}+{name}{s}*(?::[^=]+)?={s}*\{{",
            s = JS_SPACE
        ));
        if let Some(decl) = declared.find(&safe) {
            return read_chain(&safe, decl.end() - 1, chain);
        }
        let enumerated = regex(&format!(r"(?-u:\b)enum{s}+{name}{s}*\{{", s = JS_SPACE));
        if let Some(decl) = enumerated.find(&safe) {
            // `enum Paths { Board = 'board' }`
            let [member] = chain else {
                return None;
            };
            let open = decl.end() - 1;
            let close = match_bracket(&safe, open).unwrap_or(open).max(open);
            let body = &safe[open..close];
            let value = regex(&format!(
                r#"(?-u:\b){member}{s}*={s}*(['"`][^'"`]*['"`])"#,
                member = regex::escape(member),
                s = JS_SPACE
            ));
            return value.captures(body).map(|caps| caps[1].to_string());
        }
        let class = regex(&format!(
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
        let field = regex(&format!(
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
        let declares = regex(&format!(
            r"(?-u:\b)(?:const|let|class|enum){s}+{name}(?-u:\b)",
            s = JS_SPACE
        ));
        if declares.is_match(&content) {
            return Some(content);
        }
        if hops >= MAX_DECLARATION_HOPS {
            return None;
        }
        let names_root = regex(&format!(r"(?-u:\b){name}(?-u:\b)"));
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
        let simple = regex(&format!(
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
        Self { loaded_by }
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
        lazy_reference(name).is_some()
            || name
                .strip_prefix(LAYOUT_PREFIX)
                .is_some_and(|layout| !layout.is_empty())
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
