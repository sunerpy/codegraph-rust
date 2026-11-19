//! What a React lazy loader hands over — the loader readers of upstream
//! `react.ts` (#2480): a data router's `lazy` property, React Router 7's
//! object of lazy properties, and a module-level value a lazy wrapper loads
//! (`lazy(() => import('./x'))`). They read the loader's own text, never the
//! modules it loads: what a loader hands over is a module, an export of one,
//! or a name of the file it is written in, which the React resolver finds.
//!
//! Offsets are bytes; every one the readers slice at is an ASCII delimiter,
//! so a slice is always on a character boundary. Upstream counts UTF-16 code
//! units; the characters it compares are ASCII either way.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use super::js_scan::{
    self, JS_SPACE, is_identifier, is_js_space, js_trim, match_bracket, skip_string,
    top_level_items,
};
use super::react_router::{LAZY_ROUTE_PREFIX, OUTLET};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};

/// How deep the readers follow a loader's nesting: arrays inside arrays,
/// destructuring patterns inside patterns, loaders handed to wrappers inside
/// wrappers. KEEP-RUST: upstream recurses without a bound; a loader nested
/// deeper is read as one the readers cannot tell, and links nothing.
const MAX_LOADER_DEPTH: usize = 256;

/// The prefix of the export a loader picks, named the way Vue Router and
/// Angular name a lazily loaded component: `import:./tabs#TestTab`
/// (`PICKED_EXPORT`).
const PICKED_PREFIX: &str = "import:";

/// A value a lazy loader hands over, as far as its source says (`Loaded`):
/// the module an `import('./x')` loads, one export of it, a name of the
/// file the loader is written in, an object literal (read in the scope it is
/// written in), or the array a `Promise.all([…])` resolves to.
#[derive(Clone, Debug, PartialEq)]
enum Loaded {
    Module(String),
    Export { spec: String, name: String },
    Name(String),
    Object { text: String, scope: usize },
    Array(Vec<Option<Loaded>>),
}

/// The names a loader binds, each to what it holds: `None` for a value it
/// does not read (`LoaderScope`).
type Scope = HashMap<String, Option<Loaded>>;

/// What route properties a loader resolves to render (`routeRender`): the
/// reference for its page, nothing, or properties the readers do not read.
enum Rendered {
    Page(String),
    Nothing,
    Unread,
}

/// One reading of a loader. Upstream's scopes are maps an object literal
/// keeps a live reference to; here they are the reader's, an object naming
/// its scope by index.
struct Reader {
    scopes: Vec<Scope>,
    depth: usize,
    /// A value lay past `MAX_LOADER_DEPTH`.
    exhausted: bool,
}

impl Reader {
    fn new() -> Self {
        Self {
            scopes: Vec::new(),
            depth: 0,
            exhausted: false,
        }
    }

    /// A new scope holding `outer`'s bindings (`new Map(outer)`), or none.
    fn scope_from(&mut self, outer: Option<usize>) -> usize {
        let scope = outer
            .map(|outer| self.scopes[outer].clone())
            .unwrap_or_default();
        self.scopes.push(scope);
        self.scopes.len() - 1
    }

    /// One level deeper, unless the bound is reached.
    fn enter(&mut self) -> bool {
        if self.depth >= MAX_LOADER_DEPTH {
            self.exhausted = true;
            return false;
        }
        self.depth += 1;
        true
    }

    /// What the function `text` resolves to when called, `param` handed to
    /// its first parameter: its expression body's value, or what the first
    /// `return` of its body hands back (`loaderValue`). `None` for a function
    /// this does not read.
    fn loader_value(
        &mut self,
        text: &str,
        outer: Option<usize>,
        param: Option<Loaded>,
    ) -> Option<Loaded> {
        if !self.enter() {
            return None;
        }
        let value = self.loader_value_inner(text, outer, param);
        self.depth -= 1;
        value
    }

    fn loader_value_inner(
        &mut self,
        text: &str,
        outer: Option<usize>,
        param: Option<Loaded>,
    ) -> Option<Loaded> {
        let parts = function_parts(text)?;
        let scope = self.scope_from(outer);
        let mut param = Some(param);
        for item in top_level_items(parts.params) {
            let value = param.take().flatten();
            self.bind_pattern(scope, parameter_pattern(item), value);
        }
        if parts.block {
            self.block_value(parts.body, scope)
        } else {
            self.value_of(parts.body, scope)
        }
    }

    /// The value a function body hands back from its first top-level
    /// `return`, its declarations bound on the way there (`blockValue`).
    /// Nested blocks and functions are stepped over: what is declared or
    /// returned in there is theirs.
    fn block_value(&mut self, body: &str, scope: usize) -> Option<Loaded> {
        let bytes = body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let ch = bytes[i];
            if matches!(ch, b'"' | b'\'' | b'`' | b'{' | b'(' | b'[') {
                let end = if matches!(ch, b'{' | b'(' | b'[') {
                    match_bracket(body, i)
                } else {
                    skip_string(body, i)
                };
                i = end? + 1;
                continue;
            }
            let Some(after) = identifier_end_at(body, i) else {
                i += 1;
                continue;
            };
            let word = &body[i..after];
            let statement = i == 0 || !is_word_or_dot(bytes[i - 1]);
            i = after;
            if !statement {
                continue;
            }
            match word {
                "return" => {
                    let end = js_scan::statement_end(body, after, false);
                    return self.value_of(&body[after..end], scope);
                }
                "const" | "let" | "var" => i = self.declare(body, after, scope),
                "function" | "class" => {
                    if let Some(name) = declared_name(&body[after..window_end(body, after)]) {
                        self.scopes[scope].insert(name.to_string(), None);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Bind each declarator of a `const`, `let` or `var` whose first pattern
    /// starts at `at`; where the declaration ends (`declare`).
    fn declare(&mut self, body: &str, at: usize, scope: usize) -> usize {
        let bytes = body.as_bytes();
        let mut i = at;
        loop {
            i = skip_js_space(body, i);
            let end = if matches!(bytes.get(i), Some(b'{' | b'[')) {
                match_bracket(body, i).map_or(0, |close| close + 1)
            } else {
                identifier_end_at(&body[..window_end(body, i)], i).unwrap_or(i)
            };
            if end <= i {
                return next_char(body, i);
            }
            let pattern = &body[i..end];
            let mut stop = end;
            match js_scan::initializer_at(body, end) {
                None => self.bind_pattern(scope, pattern, None),
                Some(init) => {
                    stop = js_scan::statement_end(body, init, true);
                    let value = self.value_of(&body[init..stop], scope);
                    self.bind_pattern(scope, pattern, value);
                }
            }
            stop = skip_js_space(body, stop);
            if bytes.get(stop) != Some(&b',') {
                return stop;
            }
            i = stop + 1;
        }
    }

    /// What expression `text` evaluates to in a loader, or `None` when it is
    /// not one this reads (`valueOf`).
    fn value_of(&mut self, text: &str, scope: usize) -> Option<Loaded> {
        if !self.enter() {
            return None;
        }
        let value = self.value_of_inner(text, scope);
        self.depth -= 1;
        value
    }

    fn value_of_inner(&mut self, text: &str, scope: usize) -> Option<Loaded> {
        static AWAIT: OnceLock<Regex> = OnceLock::new();
        let awaited = AWAIT
            .get_or_init(|| Regex::new(&format!(r"^await(?-u:\b){JS_SPACE}*")).expect("await"));
        // `await`, and parentheses around the whole expression, change
        // nothing here.
        let mut s = js_trim(text);
        loop {
            let before = s.len();
            if let Some(m) = awaited.find(s) {
                s = &s[m.end()..];
            }
            if s.starts_with('(') && match_bracket(s, 0) == Some(s.len() - 1) {
                s = js_trim(&s[1..s.len() - 1]);
            }
            if s.len() == before {
                break;
            }
        }
        if s.starts_with('{') || s.starts_with('[') {
            if match_bracket(s, 0) != Some(s.len() - 1) {
                return None;
            }
            let inside = &s[1..s.len() - 1];
            if s.starts_with('{') {
                return Some(Loaded::Object {
                    text: inside.to_string(),
                    scope,
                });
            }
            let items = top_level_items(inside)
                .into_iter()
                .map(|item| {
                    if item.is_empty() {
                        None
                    } else {
                        self.value_of(item, scope)
                    }
                })
                .collect();
            return Some(Loaded::Array(items));
        }
        // The head: `import('./x')`, `Promise.all([…])`, a name, or a
        // parenthesized expression.
        let (mut value, mut i) = match head(s)? {
            Head::Name(name) => (self.name_value(name, scope), name.len()),
            Head::Call { kind, open } => {
                let close = match_bracket(s, open)?;
                let inside = &s[open + 1..close];
                let value = match kind {
                    CallKind::Import => {
                        import_spec(inside).map(|spec| Loaded::Module(spec.to_string()))
                    }
                    CallKind::PromiseAll => self
                        .value_of(inside, scope)
                        .filter(|all| matches!(all, Loaded::Array(_))),
                    CallKind::Paren => self.value_of(inside, scope),
                };
                (value, close + 1)
            }
        };
        // Then each `.name`, `?.name`, `['name']` and `.then(…)` on it.
        while value.is_some() && i < s.len() {
            let rest = &s[i..];
            if let Some(paren) = then_call(rest) {
                let open = i + paren;
                let close = match_bracket(s, open);
                let (Some(close), Some(Loaded::Module(_))) = (close, &value) else {
                    return None;
                };
                let callback = top_level_items(&s[open + 1..close]).first().copied();
                value = match callback {
                    Some(callback) if !callback.is_empty() => {
                        self.loader_value(callback, Some(scope), value)
                    }
                    _ => None,
                };
                i = close + 1;
                continue;
            }
            let (name, len) = member_access(rest)?;
            value = self.member_of(value, name);
            i += len;
        }
        value
    }

    /// What a name holds in a loader: what it binds there, or else a name of
    /// the loader's own file (`nameValue`).
    fn name_value(&self, name: &str, scope: usize) -> Option<Loaded> {
        match self.scopes[scope].get(name) {
            Some(bound) => bound.clone(),
            None => Some(Loaded::Name(name.to_string())),
        }
    }

    /// A property of a value: an export of a module, or what an object
    /// literal holds under `key` (`memberOf`).
    fn member_of(&mut self, value: Option<Loaded>, key: &str) -> Option<Loaded> {
        match value? {
            Loaded::Module(spec) => Some(Loaded::Export {
                spec,
                name: key.to_string(),
            }),
            Loaded::Object { text, scope } => {
                let (props, _) = object_entries(&text);
                let held = props.get(key)?;
                self.value_of(held, scope)
            }
            _ => None,
        }
    }

    /// Bind the names `pattern` declares to the parts of `value` they take:
    /// `{ Page, default: Home }` of a module to its exports, `[{ Page }]` to
    /// the first item of a `Promise.all` (`bindPattern`). A name whose value
    /// this does not read is bound to `None`, so it never counts as a name
    /// of the loader's own file.
    fn bind_pattern(&mut self, scope: usize, pattern: &str, value: Option<Loaded>) {
        if !self.enter() {
            return;
        }
        self.bind_pattern_inner(scope, pattern, value);
        self.depth -= 1;
    }

    fn bind_pattern_inner(&mut self, scope: usize, pattern: &str, value: Option<Loaded>) {
        let p = js_trim(pattern);
        if p.is_empty() {
            return;
        }
        if is_identifier(p) {
            self.scopes[scope].insert(p.to_string(), value);
            return;
        }
        let close = if p.starts_with('{') || p.starts_with('[') {
            match_bracket(p, 0)
        } else {
            None
        };
        if close != Some(p.len() - 1) {
            for name in identifiers(p) {
                self.scopes[scope].insert(name.to_string(), None);
            }
            return;
        }
        let array = p.starts_with('[');
        for (index, item) in top_level_items(&p[1..p.len() - 1]).into_iter().enumerate() {
            if item.is_empty() {
                continue;
            }
            if let Some(rest) = item.strip_prefix("...") {
                self.bind_pattern(scope, rest, None);
                continue;
            }
            if array {
                let element = match &value {
                    Some(Loaded::Array(items)) => items.get(index).cloned().flatten(),
                    _ => None,
                };
                self.bind_pattern(scope, without_default(item), element);
                continue;
            }
            // `Page`, `Page = Fallback`, `default: Home`, `'default': Home`.
            match property_pattern(item) {
                Some((key, target)) => {
                    let member = self.member_of(value.clone(), key);
                    self.bind_pattern(scope, target.unwrap_or(key), member);
                }
                None => {
                    let target = item.find(':').map_or(item, |colon| &item[colon + 1..]);
                    self.bind_pattern(scope, target, None);
                }
            }
        }
    }

    /// The distinct exports `text` uses through the names a loader bound
    /// them to: `Page`, `m.Page` (`loadedExports`).
    fn loaded_exports(&mut self, text: &str, scope: usize) -> Vec<Loaded> {
        let mut found: Vec<Loaded> = Vec::new();
        for (name, member) in used_names(text) {
            let bound = self.scopes[scope].get(name).cloned().flatten();
            let value = match (bound, member) {
                (Some(module @ Loaded::Module(_)), Some(member)) => {
                    self.member_of(Some(module), member)
                }
                (bound, _) => bound,
            };
            if let Some(export @ Loaded::Export { .. }) = value
                && !found.contains(&export)
            {
                found.push(export);
            }
        }
        found
    }

    /// What route properties a loader resolves to render: the reference for
    /// its page, `Outlet` for React Router's, nothing, or properties this
    /// does not read (`routeRender`).
    fn route_render(
        &mut self,
        value: Option<Loaded>,
        element_tag: &dyn Fn(&str) -> Option<String>,
    ) -> Rendered {
        let (text, scope) = match value {
            Some(Loaded::Module(spec)) => {
                return Rendered::Page(format!("{LAZY_ROUTE_PREFIX}{spec}"));
            }
            Some(Loaded::Object { text, scope }) => (text, scope),
            _ => return Rendered::Unread,
        };
        let (props, spreads) = object_entries(&text);
        let component = props.get("Component");
        let element = props.get("element");
        if let Some(shown) = component.or(element) {
            let held = component.and_then(|component| self.value_of(component, scope));
            if let Some(named) = page_reference(held.as_ref()) {
                return Rendered::Page(named);
            }
            // The one export the loader imported that it shows, past a
            // guard of the route file's own: `<AgeGate><FireworksPage
            // /></AgeGate>`, `() => <PrivateRoute component={<Favorites />}
            // />`, `withAuth(Page)`.
            let loaded = self.loaded_exports(shown, scope);
            if let [export] = loaded.as_slice() {
                return page_reference(Some(export)).map_or(Rendered::Unread, Rendered::Page);
            }
            let tag = element
                .filter(|_| component.is_none() && loaded.is_empty())
                .and_then(|element| element_tag(element));
            // `Component: Outlet`, `element: <Outlet />`: the routes inside
            // render in its place.
            if matches!(&held, Some(Loaded::Name(name)) if name == OUTLET)
                || tag.as_deref() == Some(OUTLET)
            {
                return Rendered::Page(OUTLET.to_string());
            }
            return tag
                .and_then(|tag| page_reference(self.name_value(&tag, scope).as_ref()))
                .map_or(Rendered::Unread, Rendered::Page);
        }
        // `{ ...module }`: the module's exports are the route's properties.
        if spreads.is_empty() {
            return Rendered::Nothing;
        }
        let spread = if spreads.len() == 1 {
            self.value_of(spreads[0], scope)
        } else {
            None
        };
        match spread {
            Some(Loaded::Module(spec)) => Rendered::Page(format!("{LAZY_ROUTE_PREFIX}{spec}")),
            _ => Rendered::Unread,
        }
    }

    /// React Router 7's object of lazy properties, `lazy: { loader: …,
    /// Component: async () => (await import('./x')).Page }`: each loads on
    /// its own, and the `Component`'s loader resolves to the page itself
    /// (`lazyPropertiesRender`).
    fn lazy_properties_render(&mut self, code: &str) -> Rendered {
        if match_bracket(code, 0) != Some(code.len() - 1) {
            return Rendered::Unread;
        }
        let (props, _) = object_entries(&code[1..code.len() - 1]);
        let Some(component) = props.get("Component") else {
            return Rendered::Nothing;
        };
        if let Some(page) = page_reference(self.loader_value(component, None, None).as_ref()) {
            return Rendered::Page(page);
        }
        first_import(component).map_or(Rendered::Unread, |spec| {
            Rendered::Page(format!("{LAZY_ROUTE_PREFIX}{spec}"))
        })
    }

    /// The loader function an initializer hands a lazy wrapper (`loaderOf`):
    /// the first function among its call's arguments, through the wrappers
    /// around it (`Loadable(lazy(() => import('./x')))`) and before the
    /// options after it (`dynamic(…, { ssr: false })`), or an options
    /// object's `loader` (`Loadable({ loader: () => import(…) })`). `None`
    /// for an initializer that is no such call.
    fn loader_of(&mut self, text: &str) -> Option<String> {
        if !self.enter() {
            return None;
        }
        let loader = self.loader_of_inner(text);
        self.depth -= 1;
        loader
    }

    fn loader_of_inner(&mut self, text: &str) -> Option<String> {
        let s = js_trim(text);
        let Some(open) = js_scan::callee_end(s) else {
            return self.conditional_loader(s);
        };
        let close = match_bracket(s, open)?;
        for item in top_level_items(&s[open + 1..close]) {
            if item.is_empty() {
                continue;
            }
            if function_parts(item).is_some() {
                return Some(item.to_string());
            }
            if item.starts_with('{') {
                if match_bracket(item, 0) == Some(item.len() - 1) {
                    let (props, _) = object_entries(&item[1..item.len() - 1]);
                    if let Some(loader) = props.get("loader")
                        && function_parts(loader).is_some()
                    {
                        return Some(loader.clone());
                    }
                }
                continue;
            }
            if let Some(inner) = self.loader_of(item) {
                return Some(inner);
            }
        }
        self.conditional_loader(s)
    }

    /// The loader of a conditional's one branch that has one: SigNoz's
    /// `IS_DEV ? React.lazy(() => import(…)) : null`. `None` when neither
    /// branch loads, or both do — the value is then one or the other
    /// (`conditionalLoader`).
    fn conditional_loader(&mut self, text: &str) -> Option<String> {
        let (yes, no) = conditional_branches(text)?;
        match (self.loader_of(yes), self.loader_of(no)) {
            (Some(loader), None) | (None, Some(loader)) => Some(loader),
            _ => None,
        }
    }

    /// The page a lazy wrapper's loader resolves to: a module's default
    /// export, an export it picks, or the `default` of the object
    /// `React.lazy` wants (`{ default: m.Chart }`) (`loadedPage`).
    fn loaded_page(&mut self, value: Option<Loaded>) -> Option<LoadedPage> {
        let value = value?;
        if !self.enter() {
            return None;
        }
        let page = match value {
            Loaded::Module(spec) => Some(LoadedPage {
                spec,
                member: "default".to_string(),
            }),
            Loaded::Export { spec, name } => Some(LoadedPage { spec, member: name }),
            Loaded::Object { text, scope } => {
                let (props, _) = object_entries(&text);
                match props.get("default") {
                    Some(inner) => {
                        let inner = self.value_of(inner, scope);
                        self.loaded_page(inner)
                    }
                    None => None,
                }
            }
            _ => None,
        };
        self.depth -= 1;
        page
    }
}

/// The module a value's lazy wrapper loads and the export its loader picks:
/// `default` unless it picks one (`valueLoader`'s answer).
#[derive(Debug, PartialEq)]
pub(crate) struct LoadedPage {
    pub(crate) spec: String,
    pub(crate) member: String,
}

/// A loader the readers could not read to the end: it nests past
/// `MAX_LOADER_DEPTH`.
#[derive(Debug, PartialEq)]
pub(crate) struct Exhausted;

/// What a data-router `lazy` loader renders, as the route's reference names
/// it (`lazyRouteReference`). The loader resolves to the route's
/// properties, and their `Component`, or the page their `element` shows, is
/// what the route renders:
///
/// - `() => import('./x')` hands over the module's exports:
///   `lazy-import:./x`.
/// - A loader that picks the page names the export it picks,
///   `import:./x#Page`: `const { Page } = await import('./x'); return {
///   Component: Page }`, `import('./x').then((m) => ({ Component: m.Page
///   }))`, `({ Component: (await import('./x')).Page })`, or React Router
///   7's `{ Component: async () => (await import('./x')).Page }`;
///   `import:./x#default` for `const { default: Component } = await
///   import('./x')`.
/// - A component of the route's own file it returns is named as written:
///   `return { loader, Component: SettingsPage }`.
///
/// `Outlet` when it hands back React Router's (`return { Component: Outlet,
/// loader }`): the routes inside it render in its place. `None` when it
/// renders nothing, handing over only a `loader`, and for a loader nested
/// past the readers' bound. A loader this does not read, such as a helper's
/// (`lazyPage(() => import('./x'))`) or `.then(convert)`, loads the module
/// its first import names. `element_tag` reads the page an `element` shows.
pub(crate) fn lazy_route_reference(
    loader: Option<&str>,
    element_tag: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let loader = loader.filter(|loader| !loader.is_empty())?;
    let stripped = strip_comments_for_regex(loader, CommentLang::TypeScript);
    let code = js_trim(&stripped);
    let mut reader = Reader::new();
    let rendered = if code.starts_with('{') {
        reader.lazy_properties_render(code)
    } else {
        let value = reader.loader_value(code, None, None);
        reader.route_render(value, element_tag)
    };
    if reader.exhausted {
        return None;
    }
    match rendered {
        Rendered::Page(page) => Some(page),
        Rendered::Nothing => None,
        Rendered::Unread => first_import(code).map(|spec| format!("{LAZY_ROUTE_PREFIX}{spec}")),
    }
}

/// The module a value's initializer hands a lazy loader, and the export the
/// loader resolves to (`valueLoader`, past the declaration's `=`):
/// `lazy(() => import('./x'))`, wrapped or not (`Loadable(lazy(…))`,
/// `loadable(…)`, `dynamic(…, { ssr: false })`, `Loadable({ loader: () =>
/// import(…) })`), `dynamic(async () => (await import('./x')).default)`, or
/// a named export (`.then((m) => ({ default: m.Chart }))`, `.then((m) =>
/// m.Chart)`), in either branch of a conditional (`IS_DEV ? React.lazy(…) :
/// null`). A loader the readers cannot read loads the module its
/// expression's first import names. A function the value wraps is no loader
/// (`withFallback('Dialog', (props) => { … import('mermaid') … })`): what it
/// returns renders, and what it imports is its own business. `initializer`
/// is comment-stripped text.
pub(crate) fn value_loader(initializer: &str) -> Result<Option<LoadedPage>, Exhausted> {
    let mut reader = Reader::new();
    let Some(loader) = reader.loader_of(initializer) else {
        return if reader.exhausted {
            Err(Exhausted)
        } else {
            Ok(None)
        };
    };
    let value = reader.loader_value(&loader, None, None);
    let page = reader.loaded_page(value);
    if reader.exhausted {
        return Err(Exhausted);
    }
    if page.is_some() {
        return Ok(page);
    }
    Ok(function_parts(&loader)
        .filter(|parts| !parts.block)
        .and_then(|parts| first_import(parts.body))
        .map(|spec| LoadedPage {
            spec: spec.to_string(),
            member: "default".to_string(),
        }))
}

/// The module path and export a picked-export reference names,
/// `import:./x#Page` (`PICKED_EXPORT`: `^import:(.+)#([A-Za-z_$][\w$]*)$`).
/// The module's path may hold a `#` itself.
pub(crate) fn picked_export_name(name: &str) -> Option<(&str, &str)> {
    let (spec, export) = name.strip_prefix(PICKED_PREFIX)?.rsplit_once('#')?;
    (!spec.is_empty()
        && !spec.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
        && is_identifier(export))
    .then_some((spec, export))
}

/// An arrow function's or a `function`'s parameter list and body
/// (`functionParts`), its `async` and return type skipped.
struct FunctionParts<'t> {
    params: &'t str,
    body: &'t str,
    /// The body is a block, not an expression.
    block: bool,
}

/// The parts of the function `text` is, or `None` for any other expression.
fn function_parts(text: &str) -> Option<FunctionParts<'_>> {
    static ASYNC: OnceLock<Regex> = OnceLock::new();
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    let mut s = js_trim(text);
    if let Some(m) = ASYNC
        .get_or_init(|| Regex::new(&format!(r"^async(?-u:\b){JS_SPACE}*")).expect("async"))
        .find(s)
    {
        s = &s[m.end()..];
    }
    let keyword = match KEYWORD
        .get_or_init(|| {
            Regex::new(&format!(
                r"^function(?-u:\b){JS_SPACE}*\*?{JS_SPACE}*(?:[A-Za-z_$][A-Za-z0-9_$]*{JS_SPACE}*)?"
            ))
            .expect("function keyword")
        })
        .find(s)
    {
        Some(m) => {
            s = &s[m.end()..];
            true
        }
        None => false,
    };
    let (params, mut rest) = if s.starts_with('(') {
        let close = match_bracket(s, 0)?;
        (&s[1..close], s[close + 1..].trim_start_matches(is_js_space))
    } else {
        if keyword {
            return None;
        }
        let end = identifier_end_at(s, 0)?;
        (&s[..end], s[end..].trim_start_matches(is_js_space))
    };
    // `async (): Promise<RouteProps> => …`
    if !keyword && rest.starts_with(':') {
        rest = &rest[arrow_at(rest)?..];
    }
    if keyword {
        return (rest.starts_with('{') && match_bracket(rest, 0) == Some(rest.len() - 1)).then(
            || FunctionParts {
                params,
                body: &rest[1..rest.len() - 1],
                block: true,
            },
        );
    }
    let body = js_trim(rest.strip_prefix("=>")?);
    if !body.starts_with('{') {
        return Some(FunctionParts {
            params,
            body,
            block: false,
        });
    }
    (match_bracket(body, 0) == Some(body.len() - 1)).then(|| FunctionParts {
        params,
        body: &body[1..body.len() - 1],
        block: true,
    })
}

/// Where the first `=>` outside brackets and strings is in `text`
/// (`arrowAt`).
fn arrow_at(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(text, i)?,
            b'{' | b'(' | b'[' => i = match_bracket(text, i)?,
            b'=' if bytes.get(i + 1) == Some(&b'>') => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// A parameter's binding pattern, without its type or default: `{ default:
/// Page }` of `{ default: Page }: Module` (`parameterPattern`).
fn parameter_pattern(param: &str) -> &str {
    let param = param
        .strip_prefix("...")
        .map_or(param, |rest| rest.trim_start_matches(is_js_space));
    if param.starts_with('{') || param.starts_with('[') {
        return match_bracket(param, 0).map_or("", |close| &param[..=close]);
    }
    identifier_end_at(param, 0).map_or("", |end| &param[..end])
}

/// The head of an expression the readers evaluate.
enum Head<'t> {
    /// `[A-Za-z_$][\w$]*`.
    Name(&'t str),
    /// `import(`, `Promise.all(` or `(`, with where the paren opens.
    Call { kind: CallKind, open: usize },
}

enum CallKind {
    Import,
    PromiseAll,
    Paren,
}

/// The head of `s` (`^(?:(import)\s*\(|(Promise)\s*\.\s*all\s*\(|([A-Za-z_$][\w$]*)|\()`):
/// the alternatives in order, as the regex tries them.
fn head(s: &str) -> Option<Head<'_>> {
    let bytes = s.as_bytes();
    if let Some(after) = s.strip_prefix("import") {
        let open = skip_js_space(s, s.len() - after.len());
        if bytes.get(open) == Some(&b'(') {
            return Some(Head::Call {
                kind: CallKind::Import,
                open,
            });
        }
    }
    if s.starts_with("Promise") {
        let dot = skip_js_space(s, "Promise".len());
        if bytes.get(dot) == Some(&b'.') {
            let all = skip_js_space(s, dot + 1);
            if s[all..].starts_with("all") {
                let open = skip_js_space(s, all + 3);
                if bytes.get(open) == Some(&b'(') {
                    return Some(Head::Call {
                        kind: CallKind::PromiseAll,
                        open,
                    });
                }
            }
        }
    }
    if let Some(end) = identifier_end_at(s, 0) {
        return Some(Head::Name(&s[..end]));
    }
    s.starts_with('(').then_some(Head::Call {
        kind: CallKind::Paren,
        open: 0,
    })
}

/// The module an `import(…)`'s argument names, a string with no quote in
/// it (`^\s*(["'])([^"'`]+)\1\s*$`).
fn import_spec(argument: &str) -> Option<&str> {
    let s = js_trim(argument);
    let quote = *s.as_bytes().first()?;
    if !matches!(quote, b'"' | b'\'') || s.len() < 3 || s.as_bytes()[s.len() - 1] != quote {
        return None;
    }
    let spec = &s[1..s.len() - 1];
    (!spec.contains(['"', '\'', '`'])).then_some(spec)
}

/// Where the paren of a `.then(` at the start of `rest` opens
/// (`^\s*\??\.\s*then\s*\(`).
fn then_call(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = skip_js_space(rest, 0);
    if bytes.get(i) == Some(&b'?') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'.') {
        return None;
    }
    i = skip_js_space(rest, i + 1);
    if !rest[i..].starts_with("then") {
        return None;
    }
    let open = skip_js_space(rest, i + 4);
    (bytes.get(open) == Some(&b'(')).then_some(open)
}

/// The member a `.name`, `?.name` or `['name']` at the start of `rest`
/// reads, and how long the access is (`^\s*\??\.\s*([A-Za-z_$][\w$]*)`,
/// else `^\s*\[\s*(["'])([A-Za-z_$][\w$]*)\1\s*\]`).
fn member_access(rest: &str) -> Option<(&str, usize)> {
    let bytes = rest.as_bytes();
    let start = skip_js_space(rest, 0);
    let mut i = start;
    if bytes.get(i) == Some(&b'?') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        let name_at = skip_js_space(rest, i + 1);
        if let Some(end) = identifier_end_at(rest, name_at) {
            return Some((&rest[name_at..end], end));
        }
    }
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let quote_at = skip_js_space(rest, start + 1);
    let quote = *bytes.get(quote_at)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    let end = identifier_end_at(rest, quote_at + 1)?;
    if bytes.get(end) != Some(&quote) {
        return None;
    }
    let close = skip_js_space(rest, end + 1);
    (bytes.get(close) == Some(&b']')).then(|| (&rest[quote_at + 1..end], close + 1))
}

/// An object literal's own properties, each name to its value's text —
/// shorthand and methods included — and its spreads (`objectEntries`).
fn object_entries(text: &str) -> (HashMap<&str, String>, Vec<&str>) {
    let mut props: HashMap<&str, String> = HashMap::new();
    let mut spreads = Vec::new();
    for entry in top_level_items(text) {
        if entry.is_empty() {
            continue;
        }
        if let Some(spread) = entry.strip_prefix("...") {
            let spread = spread.trim_start_matches(is_js_space);
            if !spread.is_empty() {
                spreads.push(spread);
            }
            continue;
        }
        if let Some((key, value)) = object_pair(entry) {
            props.insert(key, value.to_string());
        } else if let Some((asynchronous, name, paren)) = object_method(entry) {
            props.insert(name, format!("{asynchronous}function {}", &entry[paren..]));
        } else if is_identifier(entry) {
            props.insert(entry, entry.to_string());
        }
    }
    (props, spreads)
}

/// A `key: value` entry's key and value
/// (`^(?:([A-Za-z_$][\w$]*)|(["'])([A-Za-z_$][\w$]*)\2)\s*:\s*([\s\S]+)$`).
fn object_pair(entry: &str) -> Option<(&str, &str)> {
    let bytes = entry.as_bytes();
    let (key, after) = match identifier_end_at(entry, 0) {
        Some(end) => (&entry[..end], end),
        None => {
            let quote = *bytes.first()?;
            if !matches!(quote, b'"' | b'\'') {
                return None;
            }
            let end = identifier_end_at(entry, 1)?;
            if bytes.get(end) != Some(&quote) {
                return None;
            }
            (&entry[1..end], end + 1)
        }
    };
    let colon = skip_js_space(entry, after);
    if bytes.get(colon) != Some(&b':') {
        return None;
    }
    let value = &entry[colon + 1..];
    // `\s*` then at least one character: past white space when anything
    // else follows, else the last of it.
    let trimmed = value.trim_start_matches(is_js_space);
    if !trimmed.is_empty() {
        Some((key, trimmed))
    } else {
        let last = value.char_indices().next_back()?.0;
        Some((key, &value[last..]))
    }
}

/// A method entry's `async ` prefix, its name, and where its parameter list
/// opens (`^(async\s+)?([A-Za-z_$][\w$]*)\s*\(`).
fn object_method(entry: &str) -> Option<(&str, &str, usize)> {
    let named = |from: usize| -> Option<(&str, usize)> {
        let end = identifier_end_at(entry, from)?;
        let paren = skip_js_space(entry, end);
        (entry.as_bytes().get(paren) == Some(&b'(')).then(|| (&entry[from..end], paren))
    };
    if let Some(after) = entry.strip_prefix("async") {
        let name_at = skip_js_space(entry, entry.len() - after.len());
        if name_at > entry.len() - after.len()
            && let Some((name, paren)) = named(name_at)
        {
            return Some((&entry[..name_at], name, paren));
        }
    }
    named(0).map(|(name, paren)| ("", name, paren))
}

/// A property entry of an object pattern, its key and the pattern it binds,
/// when it has one (`default: Home`), past a default value
/// (`^(?:([A-Za-z_$][\w$]*)|(["'])([^"']*)\2)\s*(?::\s*([\s\S]+?))?\s*(?:=(?![=>])[\s\S]*)?$`).
fn property_pattern(item: &str) -> Option<(&str, Option<&str>)> {
    let bytes = item.as_bytes();
    let (key, after) = match identifier_end_at(item, 0) {
        Some(end) => (&item[..end], end),
        None => {
            let quote = *bytes.first()?;
            if !matches!(quote, b'"' | b'\'') {
                return None;
            }
            let close = 1 + bytes[1..].iter().position(|b| matches!(b, b'"' | b'\''))?;
            if bytes[close] != quote {
                return None;
            }
            (&item[1..close], close + 1)
        }
    };
    let colon = skip_js_space(item, after);
    if bytes.get(colon) != Some(&b':') {
        return ends_in_default(item, after).then_some((key, None));
    }
    // The shortest target after which only white space and a default
    // remain.
    let start = skip_js_space(item, colon + 1);
    let end = item[start..]
        .char_indices()
        .map(|(offset, ch)| start + offset + ch.len_utf8())
        .find(|&end| ends_in_default(item, end))?;
    Some((key, Some(&item[start..end])))
}

/// Whether only white space, then nothing or a default value
/// (`=(?![=>])…`), follows `at` in `item`.
fn ends_in_default(item: &str, at: usize) -> bool {
    let at = skip_js_space(item, at);
    match item.as_bytes().get(at) {
        None => true,
        Some(b'=') => !matches!(item.as_bytes().get(at + 1), Some(b'=' | b'>')),
        Some(_) => false,
    }
}

/// An array pattern's item without its default value
/// (`item.replace(/\s*=(?![=>])[\s\S]*$/, '')`).
fn without_default(item: &str) -> &str {
    let bytes = item.as_bytes();
    let Some(equals) = (0..bytes.len())
        .find(|&i| bytes[i] == b'=' && !matches!(bytes.get(i + 1), Some(b'=' | b'>')))
    else {
        return item;
    };
    item[..equals].trim_end_matches(is_js_space)
}

/// The identifiers written in `text` (`/[A-Za-z_$][\w$]*/g`).
fn identifiers(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        match identifier_end_at(text, i) {
            Some(end) => {
                out.push(&text[i..end]);
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// The names `text` uses, each with the member it reads off it, if any
/// (`/(?<![\w$.])([A-Za-z_$][\w$]*)(?:\s*\??\.\s*([A-Za-z_$][\w$]*))?/g`).
fn used_names(text: &str) -> Vec<(&str, Option<&str>)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let after_word = i > 0 && is_word_or_dot(bytes[i - 1]);
        let Some(end) = identifier_end_at(text, i).filter(|_| !after_word) else {
            i += 1;
            continue;
        };
        let name = &text[i..end];
        let mut next = end;
        let mut member = None;
        let mut at = skip_js_space(text, end);
        if bytes.get(at) == Some(&b'?') {
            at += 1;
        }
        if bytes.get(at) == Some(&b'.') {
            let member_at = skip_js_space(text, at + 1);
            if let Some(member_end) = identifier_end_at(text, member_at) {
                member = Some(&text[member_at..member_end]);
                next = member_end;
            }
        }
        out.push((name, member));
        i = next;
    }
    out
}

/// The reference for the page a value is: an export a loader picks, or a
/// component name of the route's file (`pageReference`).
fn page_reference(value: Option<&Loaded>) -> Option<String> {
    match value? {
        Loaded::Export { spec, name } => Some(format!("{PICKED_PREFIX}{spec}#{name}")),
        Loaded::Name(name)
            if name.starts_with(|c: char| c.is_ascii_uppercase()) && name != OUTLET =>
        {
            Some(name.clone())
        }
        _ => None,
    }
}

/// The module the first `import('…')` in `code` names (`FIRST_IMPORT`).
fn first_import(code: &str) -> Option<&str> {
    static FIRST_IMPORT: OnceLock<Regex> = OnceLock::new();
    FIRST_IMPORT
        .get_or_init(|| {
            Regex::new(&format!(
                r#"(?-u:\b)import{JS_SPACE}*\({JS_SPACE}*["']([^"']+)["']{JS_SPACE}*\)"#
            ))
            .expect("first import")
        })
        .captures(code)
        .and_then(|captures| captures.get(1))
        .map(|spec| spec.as_str())
}

/// The two branches of a conditional expression, `test ? yes : no`; `None`
/// for any other expression (`conditionalBranches`).
fn conditional_branches(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut question = 0;
    let mut open = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(text, i)?,
            b'{' | b'(' | b'[' => i = match_bracket(text, i)?,
            b'?' => {
                // `??` and `?.` are no conditional.
                if bytes.get(i + 1) == Some(&b'?')
                    || (bytes.get(i + 1) == Some(&b'.')
                        && !bytes.get(i + 2).is_some_and(u8::is_ascii_digit))
                {
                    i += 1;
                } else {
                    if open == 0 {
                        question = i;
                    }
                    open += 1;
                }
            }
            b':' if open > 0 => {
                open -= 1;
                if open == 0 {
                    return Some((&text[question + 1..i], &text[i + 1..]));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The name a nested `function` or `class` declares, read from the window
/// after its keyword (`^\s*\*?\s*([A-Za-z_$][\w$]*)`).
fn declared_name(window: &str) -> Option<&str> {
    let mut at = skip_js_space(window, 0);
    if window.as_bytes().get(at) == Some(&b'*') {
        at = skip_js_space(window, at + 1);
    }
    identifier_end_at(window, at).map(|end| &window[at..end])
}

/// The end of the 128 code units upstream reads a name from, after `at`.
fn window_end(text: &str, at: usize) -> usize {
    let mut end = (at + 128).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The end of the identifier (`[A-Za-z_$][\w$]*`) starting at `at`, or
/// `None` when none starts there.
fn identifier_end_at(text: &str, at: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes
        .get(at)
        .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
    {
        return None;
    }
    let mut end = at + 1;
    while bytes
        .get(end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
    {
        end += 1;
    }
    Some(end)
}

/// `[\w$.]`.
fn is_word_or_dot(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$' | b'.')
}

/// The end of the JavaScript white space starting at `at` in `s`.
fn skip_js_space(s: &str, at: usize) -> usize {
    let Some(rest) = s.get(at..) else {
        return at;
    };
    at + (rest.len() - rest.trim_start_matches(is_js_space).len())
}

/// Past the character at `at`, or one past the end.
fn next_char(s: &str, at: usize) -> usize {
    at + s
        .get(at..)
        .and_then(|rest| rest.chars().next())
        .map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The route reference a loader gives, its `element` read the way the
    /// route scanner reads one: the first capitalized tag.
    fn reference(loader: &str) -> Option<String> {
        let tag = |jsx: &str| -> Option<String> {
            let at = jsx.find('<')? + 1;
            let end = identifier_end_at(jsx, at)?;
            Some(jsx[at..end].to_string())
                .filter(|tag| tag.starts_with(|c: char| c.is_ascii_uppercase()))
        };
        lazy_route_reference(Some(loader), &tag)
    }

    /// Upstream `react-router-lazy-member.test.ts`: a loader that picks the
    /// export it renders, however it picks it.
    #[test]
    fn a_loader_names_the_export_it_picks() {
        let cases = [
            (
                "async () => {\n  const { TestTab } = await import(\n    '@/invocation_page/tabs'\n  );\n  return { Component: TestTab };\n}",
                "import:@/invocation_page/tabs#TestTab",
            ),
            (
                "async () => {\n  const { default: Component } = await import(/* webpackChunkName: \"login\" */ './pages/login');\n  return { Component };\n}",
                "import:./pages/login#default",
            ),
            (
                "() => import('./pages/reports').then((m) => ({ Component: m.ReportList }))",
                "import:./pages/reports#ReportList",
            ),
            (
                "async () => ({ Component: (await import('./pages/reports')).ReportPage })",
                "import:./pages/reports#ReportPage",
            ),
            (
                "async () => {\n  const page = await import('./pages/profile');\n  return { Component: page.ProfilePage };\n}",
                "import:./pages/profile#ProfilePage",
            ),
            (
                "async function () {\n  const { ProjectsPage } = await import('./pages/projects');\n  return { Component: ProjectsPage };\n}",
                "import:./pages/projects#ProjectsPage",
            ),
            (
                "async () => {\n  const { teamLoader } = await import('./loaders/team');\n  const { TeamPage } = await import('./pages/team');\n  return { loader: teamLoader, Component: TeamPage };\n}",
                "import:./pages/team#TeamPage",
            ),
            (
                "{\n  loader: async () => (await import('./shows/loader')).loader,\n  Component: async () => (await import('./shows/show')).Show,\n}",
                "import:./shows/show#Show",
            ),
            (
                "async () => {\n  const { AdminPage } = await import('./pages/admin');\n  return {\n    element: (\n      <RequireAuth>\n        <AdminPage />\n      </RequireAuth>\n    ),\n  };\n}",
                "import:./pages/admin#AdminPage",
            ),
            (
                "async () => {\n  const { default: FireworksPage } = await import('./pages/fireworks');\n  return { element: <AgeGate minAge={18}><FireworksPage /></AgeGate> };\n}",
                "import:./pages/fireworks#default",
            ),
            (
                "async function () {\n  let { Favorites } = await import('./pages/favorites');\n  return { Component: () => <PrivateRoute redirectTo=\"/\" component={<Favorites />} /> };\n}",
                "import:./pages/favorites#Favorites",
            ),
            (
                "async () => {\n  const [{ Feed }] = await Promise.all([import('./pages/feed'), loadMessages()]);\n  return { Component: Feed };\n}",
                "import:./pages/feed#Feed",
            ),
            (
                "async () => {\n  const { settingsLoader } = await import('./loaders/settings');\n  return { loader: settingsLoader, Component: SettingsPage };\n}",
                "SettingsPage",
            ),
            (
                "async () => {\n  const { Missing } = await import('./pages/reports');\n  return { Component: Missing };\n}",
                "import:./pages/reports#Missing",
            ),
            ("() => import('./pages/x')", "lazy-import:./pages/x"),
            (
                "() => import('./routes/x').then(convert(queryClient))",
                "lazy-import:./routes/x",
            ),
            ("() => lazyPage(() => import('./x'))", "lazy-import:./x"),
            (
                "async () => ({ ...(await import('./pages/spread')) })",
                "lazy-import:./pages/spread",
            ),
        ];
        for (loader, expected) in cases {
            assert_eq!(reference(loader).as_deref(), Some(expected), "{loader}");
        }
    }

    /// A loader that hands over only a `loader` renders nothing; one that
    /// hands back React Router's `Outlet` renders the routes inside it.
    #[test]
    fn a_loader_renders_nothing_or_the_outlet() {
        assert_eq!(
            reference(
                "async () => {\n  const { loader } = await import('./pages/logout');\n  return { loader };\n}"
            ),
            None
        );
        assert_eq!(reference("{ loader: async () => null }"), None);
        assert_eq!(reference(""), None);
        assert_eq!(
            reference("async () => {\n  const { loader } = await import('./pages/product-detail');\n  return { Component: Outlet, loader };\n}")
                .as_deref(),
            Some(OUTLET)
        );
        assert_eq!(
            reference("async () => {\n  const { loader } = await import('./pages/order-list');\n  return { element: <Outlet />, loader };\n}")
                .as_deref(),
            Some(OUTLET)
        );
    }

    /// A value's lazy wrapper hands its loader over, wrapped, with options,
    /// in an options object or in one branch of a conditional; the loader
    /// picks the default or a named export.
    #[test]
    fn a_value_names_the_module_its_lazy_wrapper_loads() {
        let page = |spec: &str, member: &str| {
            Ok(Some(LoadedPage {
                spec: spec.to_string(),
                member: member.to_string(),
            }))
        };
        assert_eq!(
            value_loader(" Loadable(lazy(() => import('../pages/auth/Register')))"),
            page("../pages/auth/Register", "default")
        );
        assert_eq!(
            value_loader(
                " dynamic(\n  async () => (await import('../excalidrawWrapper')).default,\n  {\n    ssr: false,\n  },\n)"
            ),
            page("../excalidrawWrapper", "default")
        );
        assert_eq!(
            value_loader(" lazy(() => import('./charts').then((m) => ({ default: m.LineChart })))"),
            page("./charts", "LineChart")
        );
        assert_eq!(
            value_loader(" lazy(() => import('./charts').then((m) => m.LineChart))"),
            page("./charts", "LineChart")
        );
        assert_eq!(
            value_loader(" Loadable({ loader: () => import('./pages/x') })"),
            page("./pages/x", "default")
        );
        assert_eq!(
            value_loader(
                " IS_DEV\n  ? React.lazy(() =>\n      import('./lib/devtools/AuthZDevModal').then((m) => ({\n        default: m.AuthZDevModal,\n      })),\n    )\n  : null"
            ),
            page("./lib/devtools/AuthZDevModal", "AuthZDevModal")
        );
        // A loader the readers cannot read loads its first import.
        assert_eq!(
            value_loader(" lazy(() => import('./x').then(pick))"),
            page("./x", "default")
        );
        // A function the value wraps is no loader.
        assert_eq!(
            value_loader(
                " withFallback('Dialog', (props) => {\n  const load = () => import('./heavy');\n  return <div onClick={load} />;\n})"
            ),
            Ok(None)
        );
        assert_eq!(value_loader(" observer(Login)"), Ok(None));
        assert_eq!(value_loader(" makeConfig()"), Ok(None));
        // Both branches load: the value is one or the other.
        assert_eq!(
            value_loader(" a ? lazy(() => import('./a')) : lazy(() => import('./b'))"),
            Ok(None)
        );
    }

    #[test]
    fn a_picked_export_reference_names_its_module_and_export() {
        assert_eq!(
            picked_export_name("import:./tabs#TestTab"),
            Some(("./tabs", "TestTab"))
        );
        assert_eq!(
            picked_export_name("import:./a#b#default"),
            Some(("./a#b", "default"))
        );
        for name in [
            "import:#X",
            "import:./x#",
            "import:./x#1a",
            "lazy-import:./x",
            "import:./x",
        ] {
            assert_eq!(picked_export_name(name), None, "{name}");
        }
    }

    /// KEEP-RUST: a loader nested past the readers' bound links nothing,
    /// and nothing overflows the stack.
    #[test]
    fn a_loader_nested_past_the_bound_links_nothing() {
        let nested = |depth: usize| {
            format!(
                "async () => {{ const {{ Page }} = await import('./p'); return {{ Component: {}Page{} }}; }}",
                "(".repeat(depth),
                ")".repeat(depth)
            )
        };
        assert_eq!(reference(&nested(10)).as_deref(), Some("import:./p#Page"));
        let arrays = |depth: usize| {
            format!(
                "async () => {{ const {{ Page }} = await import('./p'); const x = {}1{}; return {{ Component: Page }}; }}",
                "[".repeat(depth),
                "]".repeat(depth)
            )
        };
        // The loader is one level and the array around the rest another;
        // each array is one more, and the `1` inside the last one too.
        assert_eq!(
            reference(&arrays(MAX_LOADER_DEPTH - 2)).as_deref(),
            Some("import:./p#Page")
        );
        assert_eq!(reference(&arrays(MAX_LOADER_DEPTH - 1)), None);
        assert_eq!(reference(&arrays(100_000)), None);
        let wrapped = |depth: usize| {
            format!(
                " {}lazy(() => import('./x')){}",
                "w(".repeat(depth),
                ")".repeat(depth)
            )
        };
        // Each wrapper call is one level, and the `lazy(…)` it hands over one
        // more.
        assert_eq!(
            value_loader(&wrapped(MAX_LOADER_DEPTH - 1)),
            Ok(Some(LoadedPage {
                spec: "./x".to_string(),
                member: "default".to_string()
            }))
        );
        assert_eq!(value_loader(&wrapped(MAX_LOADER_DEPTH)), Err(Exhausted));
        assert_eq!(value_loader(&wrapped(100_000)), Err(Exhausted));
    }

    #[test]
    fn a_pattern_binds_each_name_to_the_part_it_takes() {
        let mut reader = Reader::new();
        let scope = reader.scope_from(None);
        let module = Some(Loaded::Module("./x".to_string()));
        reader.bind_pattern(
            scope,
            "{ Page, default: Home, 'Other': O = Fallback, ...rest }",
            module.clone(),
        );
        reader.bind_pattern(
            scope,
            "[{ First }, , Third = 1]",
            Some(Loaded::Array(vec![module.clone(), None, module])),
        );
        let bound = |name: &str| reader.scopes[scope].get(name).cloned();
        let export = |name: &str| {
            Some(Some(Loaded::Export {
                spec: "./x".to_string(),
                name: name.to_string(),
            }))
        };
        assert_eq!(bound("Page"), export("Page"));
        assert_eq!(bound("Home"), export("default"));
        assert_eq!(bound("O"), export("Other"));
        assert_eq!(bound("rest"), Some(None));
        assert_eq!(bound("First"), export("First"));
        assert_eq!(
            bound("Third"),
            Some(Some(Loaded::Module("./x".to_string())))
        );
        assert_eq!(bound("Fallback"), None);
    }

    #[test]
    fn a_conditional_splits_at_its_own_colon() {
        assert_eq!(conditional_branches("a ? b : c"), Some((" b ", " c")));
        assert_eq!(
            conditional_branches("a ? (x ? y : z) : c"),
            Some((" (x ? y : z) ", " c"))
        );
        assert_eq!(
            conditional_branches("a ? x ? y : z : c"),
            Some((" x ? y : z ", " c"))
        );
        assert_eq!(conditional_branches("a ?? b"), None);
        assert_eq!(conditional_branches("a?.b"), None);
        assert_eq!(conditional_branches("a ?.5 : 1"), Some((".5 ", " 1")));
    }
}
