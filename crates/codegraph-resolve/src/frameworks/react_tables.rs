//! Route tables another file hands React Router (the table pass of upstream
//! `resolution/frameworks/react.ts`: `tableRoutes` and the readers it uses;
//! #2400's table half, #2423's same-file layouts, #2668 and #2672).
//!
//! The ASP.NET Core React template keeps its routes in `AppRoutes.js` —
//! `[{ index: true, element: <Home /> }, { path: '/counter', element:
//! <Counter /> }, …]` — and renders them from `App.js` with
//! `AppRoutes.map(({ element, ...rest }) => <Route {...rest} element={element} />)`;
//! codedthemes' admin templates write one route object per file and pass
//! `[MainRoutes, LoginRoutes]` to `createBrowserRouter`; many apps import
//! `routes` into `useRoutes(routes)`. The table's own file never names the
//! router, so its extraction cannot tell it from a menu's `{ path, element }`
//! list. The evidence is the file that hands it over: a `useRoutes` or
//! `create*Router` argument, or a `.map` whose callback renders a `<Route>`
//! from the item's own fields (the uses [`scan_routes`] reads). From there
//! the import leads to the table, which is read the way a data router's is,
//! and the tables it names in turn (`...ApiAuthorizationRoutes`, `children:
//! adminRoutes`) are read under the path they sit at, and inside the layouts
//! around them when the table is written in the same file. A value holding
//! a call's result (`const routes = getRouteMap({ … })`) stands for the
//! tables the function returns: every `return` of its own, past the
//! functions written inside it, and what a `useMemo` callback returns. A
//! file that makes a router had its route objects read when it was
//! extracted; only the tables it names are new here.
//!
//! The routes are cross-file nodes
//! ([`crate::framework::FrameworkResolver::cross_file_nodes`]): their id says
//! so (`route:{file}:{line}:table:{path}`, [`is_table_route`]), and a pass
//! after `post_extract` reconciles them with the store.
//!
//! Offsets are bytes into the file's text, whose comments are blanked byte
//! for byte so a declaration found there is at the same offset in the
//! source; every byte the readers stop at is ASCII.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::OnceLock;

use codegraph_core::file_class::is_test_path;
use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{JS_SPACE, is_js_space, match_bracket, skip_string};
use super::react_router::{
    RoutePart, ScanOptions, TableAt, TableName, TableScan, allows_jsx, compose_route_path,
    constant_path_value, route_language, route_references, router_call, scan_routes,
};
use crate::import_resolver::resolve_import_path;
use crate::strip_comments::strip_js_comments_keeping_offsets;
use crate::types::{FrameworkResolverExtractionResult, ReExport, ResolutionContext};

/// The packages React Router's components and hooks are imported from
/// (`ROUTER_PACKAGES`).
const ROUTER_PACKAGES: [&str; 3] = ["react-router-dom", "react-router", "react-router-native"];

/// At most this many tables are read from one project's consumers — a bound,
/// not a budget anyone reaches (`MAX_TABLES`).
const MAX_TABLES: usize = 512;

/// How many hops from a name to its table the readers follow: an import, a
/// re-export, a function a value calls (`depth > 3`).
const MAX_HOPS: usize = 3;

/// What a file that hands the router a table has to say: `useRoutes`, a
/// data router, or a `<Route>` to map into (`CONSUMER_HINT`).
fn consumer_hint() -> &'static Regex {
    static HINT: OnceLock<Regex> = OnceLock::new();
    HINT.get_or_init(|| {
        Regex::new(
            r"(?-u:\b)useRoutes(?-u:\b)|(?-u:\b)create(?:Browser|Hash|Memory)Router(?-u:\b)|<Route(?-u:\b)",
        )
        .expect("consumer hint")
    })
}

/// True for a route the table pass made — `route:{file}:{line}:table:{path}`
/// — and for no route extraction makes (`isTableRoute`).
pub(crate) fn is_table_route(node: &Node) -> bool {
    node.kind == NodeKind::Route
        && node
            .id
            .strip_prefix("route:")
            .and_then(|rest| rest.strip_prefix(node.file_path.as_str()))
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.strip_prefix(node.start_line.to_string().as_str()))
            .is_some_and(|rest| rest.starts_with(":table:"))
}

/// A table literal: the file it is written in, and where it opens
/// (`TableLiteral`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TableLiteral {
    file: String,
    at: usize,
}

/// A table to read: where its literal opens in the file at hand, or a
/// function whose returned tables to read (`TableSite`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TableSite {
    Literal(usize),
    Named(TableName),
}

/// A table waiting to be read, with the path it is mounted under and the
/// layouts its routes render inside.
struct Queued {
    file: String,
    at: usize,
    prefix: Vec<String>,
    layouts: Vec<String>,
}

/// The tables to read, each at most once under one prefix, in the order
/// they are found.
#[derive(Default)]
struct Tables {
    queue: VecDeque<Queued>,
    queued: HashSet<String>,
}

impl Tables {
    /// Queue `table` under `prefix`. A table renders inside the layouts
    /// around the place that names it, but a route's `layout:` reference is
    /// resolved in the route's own file, so they go along only when that
    /// place, in `from`, is in the table's file.
    fn enqueue(&mut self, table: TableLiteral, prefix: &[String], from: &str, layouts: &[String]) {
        if self.queued.len() >= MAX_TABLES {
            return;
        }
        let key = format!("{}\0{}\0{}", table.file, table.at, prefix.join("\0"));
        if !self.queued.insert(key) {
            return;
        }
        let layouts = if table.file == from {
            layouts.to_vec()
        } else {
            Vec::new()
        };
        self.queue.push_back(Queued {
            file: table.file,
            at: table.at,
            prefix: prefix.to_vec(),
            layouts,
        });
    }
}

/// The routes of the tables the project's files hand React Router, each with
/// the references that bind it (`tableRoutes`): the nodes the React
/// resolver's cross-file pass wants.
pub(crate) fn table_routes(context: &dyn ResolutionContext) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    let mut consumers = BTreeSet::new();
    for package in ROUTER_PACKAGES {
        for node in context.get_nodes_by_name_shared(package) {
            if node.kind == NodeKind::Import && !is_test_path(&node.file_path) {
                consumers.insert(node.file_path.clone());
            }
        }
    }
    let mut tables = Tables::default();
    for file in &consumers {
        let Some(source) = context.read_file_shared(file) else {
            continue;
        };
        // Most of the files that import React Router only link or navigate.
        if !consumer_hint().is_match(&source) {
            continue;
        }
        let options = ScanOptions {
            objects: router_call().is_match(&source),
            uses: true,
            table: None,
        };
        for table_use in scan_routes(&source, allows_jsx(file), &options).uses {
            let found = match &table_use.table {
                TableAt::Literal(at) => vec![TableLiteral {
                    file: file.clone(),
                    at: *at,
                }],
                TableAt::Named(name) => table_named(name, file, context, 0),
            };
            let prefix = path_values(&table_use.prefix, file, context);
            for table in found {
                tables.enqueue(table, &prefix, file, &table_use.layouts);
            }
        }
    }
    let mut made = HashSet::new();
    while let Some(next) = tables.queue.pop_front() {
        let Some(source) = context.read_file_shared(&next.file) else {
            continue;
        };
        let options = ScanOptions {
            objects: true,
            uses: false,
            table: Some(TableScan {
                at: next.at,
                prefix: next.prefix.iter().map(|lit| RoutePart::lit(lit)).collect(),
                layouts: next.layouts.clone(),
            }),
        };
        let scan = scan_routes(&source, allows_jsx(&next.file), &options);
        for named in &scan.names {
            let prefix = path_values(&named.prefix, &next.file, context);
            for table in table_named(&named.name, &next.file, context, 0) {
                tables.enqueue(table, &prefix, &next.file, &named.layouts);
            }
        }
        if router_call().is_match(&source) {
            continue;
        }
        let language = route_language(&next.file);
        let lines = LineIndex::new(&source);
        for route in scan.routes {
            let line = lines.line_at(route.at);
            let id = format!("route:{}:{line}:table:{}", next.file, route.path);
            if !made.insert(id.clone()) {
                continue;
            }
            let values: Option<Vec<String>> = route
                .parts
                .iter()
                .map(|part| {
                    part.lit.clone().or_else(|| {
                        part.expr
                            .as_deref()
                            .and_then(|expr| constant_path_value(expr, &next.file, context))
                    })
                })
                .collect();
            let name =
                values.map_or_else(|| route.path.clone(), |values| compose_route_path(&values));
            result
                .references
                .extend(route_references(&route, &id, &next.file, line, language));
            result.nodes.push(framework_node(
                id,
                NodeKind::Route,
                name,
                format!("{}::route:{}", next.file, route.path),
                next.file.clone(),
                line,
                line,
                0,
                0,
                language,
                false,
            ));
        }
    }
    result
}

/// 1-based lines of byte offsets into one text (`makeLineAt`).
struct LineIndex {
    breaks: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        Self {
            breaks: text
                .bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(at, _)| at)
                .collect(),
        }
    }

    /// The line of the byte at `at`: one more than the line breaks before it.
    fn line_at(&self, at: usize) -> i64 {
        self.breaks.partition_point(|&brk| brk < at) as i64 + 1
    }
}

/// Path parts as text: a constant read where it is declared, or shown as
/// `{expr}` when it cannot be (`pathValues`).
fn path_values(
    parts: &[RoutePart],
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Vec<String> {
    parts
        .iter()
        .map(|part| {
            part.lit
                .clone()
                .or_else(|| {
                    part.expr
                        .as_deref()
                        .and_then(|expr| constant_path_value(expr, from_file, context))
                })
                .unwrap_or_else(|| part.shown())
        })
        .collect()
}

/// The script language of `file_path` (`scriptLanguage`).
fn script_language(file_path: &str) -> Language {
    if file_path.ends_with(".tsx") {
        Language::Tsx
    } else if file_path.ends_with(".ts")
        || file_path.ends_with(".mts")
        || file_path.ends_with(".cts")
    {
        Language::TypeScript
    } else if file_path.ends_with(".jsx") {
        Language::Jsx
    } else {
        Language::JavaScript
    }
}

/// `file_path`'s text with its comments blanked byte for byte.
fn code_of(file_path: &str, context: &dyn ResolutionContext) -> Option<String> {
    context
        .read_file_shared(file_path)
        .map(|source| strip_js_comments_keeping_offsets(&source))
}

/// The literals a table name stands for in `file`: declared there, or
/// imported (`tableNamed`). A value that holds a call's result — Medusa's
/// `const routes = getRouteMap({ … })` — stands for the tables the function
/// returns.
fn table_named(
    named: &TableName,
    file: &str,
    context: &dyn ResolutionContext,
    depth: usize,
) -> Vec<TableLiteral> {
    let Some(safe) = code_of(file, context) else {
        return Vec::new();
    };
    let visible = Visible::new(Some(named.at), &safe, file, context);
    let local = declared_table(&safe, &named.name, named.call, &visible);
    if !local.is_empty() {
        return tables_at(local, file, context, depth);
    }
    let language = script_language(file);
    let Some(mapping) = context
        .get_import_mappings(file, language)
        .into_iter()
        .find(|mapping| mapping.local_name == named.name)
    else {
        return Vec::new();
    };
    if mapping.is_namespace || depth > MAX_HOPS {
        return Vec::new();
    }
    let Some(target) = resolve_import_path(&mapping.source, file, language, context) else {
        return Vec::new();
    };
    let exported = if mapping.is_default {
        "default"
    } else {
        mapping.exported_name.as_str()
    };
    exported_table(&target, exported, named.call, context, depth + 1)
}

/// The literals `sites` in `file` stand for: each literal written there, and
/// the tables each function named returns (`tablesAt`).
fn tables_at(
    sites: Vec<TableSite>,
    file: &str,
    context: &dyn ResolutionContext,
    depth: usize,
) -> Vec<TableLiteral> {
    sites
        .into_iter()
        .flat_map(|site| match site {
            TableSite::Literal(at) => vec![TableLiteral {
                file: file.to_string(),
                at,
            }],
            TableSite::Named(_) if depth > MAX_HOPS => Vec::new(),
            TableSite::Named(name) => table_named(&name, file, context, depth + 1),
        })
        .collect()
}

/// `export default`, `default` exported by name, and a name exported as
/// another (`\bexport\s+default\s+`, `\bexport\s*\{[^}]*?\bNAME\s+as\s+default\b`).
fn export_default() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"(?-u:\b)export{JS_SPACE}+default{JS_SPACE}+"))
            .expect("export default")
    })
}

/// The local `file` exports as `exported` through an export list,
/// `export { appRoutes as routes }`.
fn exported_alias(safe: &str, exported: &str) -> Option<String> {
    let pattern = format!(
        r"(?-u:\b)export{JS_SPACE}*\{{[^}}]*?(?-u:\b)([A-Za-z_$][A-Za-z0-9_$]*){JS_SPACE}+as{JS_SPACE}+{}(?-u:\b)",
        regex::escape(exported)
    );
    Regex::new(&pattern)
        .ok()?
        .captures(safe)
        .map(|captures| captures[1].to_string())
}

/// The literals `file` exports as `exported` (`exportedTable`): `export
/// default [ … ]`, `export default AppRoutes`, `export const routes = [ …
/// ]`, re-exports followed.
fn exported_table(
    file: &str,
    exported: &str,
    call: bool,
    context: &dyn ResolutionContext,
    depth: usize,
) -> Vec<TableLiteral> {
    let Some(safe) = code_of(file, context) else {
        return Vec::new();
    };
    let mut local = exported.to_string();
    if exported == "default" {
        if let Some(found) = export_default().find(&safe) {
            let after = found.end();
            // `export default [ … ]`, `export default buildRoutes()`,
            // `export default function routes() { … }`.
            let inline = if call {
                returned_tables(&safe, after)
            } else {
                held_table(&safe, after)
            };
            if !inline.is_empty() {
                return tables_at(inline, file, context, depth);
            }
            local = default_name(&safe[after..]).unwrap_or_default();
        } else {
            local = exported_alias(&safe, "default").unwrap_or_default();
        }
    } else if let Some(alias) = exported_alias(&safe, exported) {
        local = alias;
    }
    // What a module exports is declared at its top level.
    let declared = if local.is_empty() {
        Vec::new()
    } else {
        let visible = Visible::new(None, &safe, file, context);
        declared_table(&safe, &local, call, &visible)
    };
    if !declared.is_empty() {
        return tables_at(declared, file, context, depth);
    }
    if depth > MAX_HOPS {
        return Vec::new();
    }
    // `export { routes } from './routes'`, `export { default as routes }
    // from './routes'`, `export * from './routes'`.
    let language = script_language(file);
    for re_export in context.get_re_exports(file, language) {
        let (source, name) = match &re_export {
            ReExport::Named {
                exported_name,
                original_name,
                source,
            } => {
                if exported_name != exported {
                    continue;
                }
                (source, original_name.as_str())
            }
            ReExport::Wildcard { source } | ReExport::Namespace { source, .. } => {
                (source, exported)
            }
            // Upstream's re-exports of a module hold no local alias.
            ReExport::LocalAlias { .. } | ReExport::CommonJsAlias { .. } => continue,
        };
        let Some(target) = resolve_import_path(source, file, language, context) else {
            continue;
        };
        let found = exported_table(&target, name, call, context, depth + 1);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// The name `export default NAME` names, its statement ending there
/// (`^([A-Za-z_$][\w$]*)\s*(?:;|\n|$)`).
fn default_name(after: &str) -> Option<String> {
    let end = identifier_end(after, 0)?;
    let spaces = skip_space(after, end);
    let ends = spaces == after.len()
        || after.as_bytes()[spaces] == b';'
        || after[end..spaces].contains('\n');
    ends.then(|| after[..end].to_string())
}

/// Where `name`'s table literal opens, for `const name = [ … ]` (or the
/// tables a function returns when `call`) — or, for `const name =
/// getRouteMap({ … })`, the function whose returned table the value holds
/// (`declaredTable`). Of several declarations of the name, the one `visible`
/// picks.
fn declared_table(safe: &str, name: &str, call: bool, visible: &Visible) -> Vec<TableSite> {
    if !safe.contains(name) {
        return Vec::new();
    }
    let id = regex::escape(name);
    let declaration = Regex::new(&format!(
        r"(?:^|[^A-Za-z0-9_$.])((?:const|let|var){JS_SPACE}+{id}{JS_SPACE}*(?::[^=;]*)?={JS_SPACE}*)"
    ))
    .expect("declaration");
    let declarations: Vec<(usize, usize)> = declaration
        .captures_iter(safe)
        .filter_map(|captures| {
            let keyword = captures.get(1)?;
            Some((keyword.start(), keyword.end()))
        })
        .collect();
    let keywords: Vec<usize> = declarations.iter().map(|(start, _)| *start).collect();
    if let Some(chosen) = visible.pick(&keywords) {
        let after = declarations[chosen].1;
        return if call {
            returned_tables(safe, after)
        } else {
            held_table(safe, after)
        };
    }
    if !call {
        return Vec::new();
    }
    let function = Regex::new(&format!(
        r"(?:^|[^A-Za-z0-9_$.])(function{JS_SPACE}+{id}{JS_SPACE}*\()"
    ))
    .expect("function declaration");
    function
        .captures(safe)
        .and_then(|captures| captures.get(1))
        .map_or_else(Vec::new, |keyword| returned_tables(safe, keyword.start()))
}

/// The tables a value written at `at` gives (`heldTable`): the literal
/// there, or the function a call there names — `getRouteMap({ … })`, `await
/// loadRoutes()` — whose returned table it is, and through `useMemo` what
/// its callback returns: `useMemo(() => getRoutes(user), [user])`,
/// `useMemo(getRoutes, [])`.
fn held_table(safe: &str, mut at: usize) -> Vec<TableSite> {
    // Parentheses around the value change nothing: `return ([ … ])`.
    while safe.as_bytes().get(at) == Some(&b'(') {
        at = skip_space(safe, at + 1);
    }
    if matches!(safe.as_bytes().get(at), Some(b'[' | b'{')) {
        return vec![TableSite::Literal(at)];
    }
    if let Some(callback) = memo_callback(safe, at) {
        // `useMemo(getRoutes, [])`: the function handed over is called.
        if let Some(end) = identifier_end(safe, callback)
            && safe.as_bytes().get(skip_space(safe, end)) == Some(&b',')
        {
            return vec![TableSite::Named(TableName {
                name: safe[callback..end].to_string(),
                call: true,
                at: callback,
            })];
        }
        return returned_tables(safe, callback);
    }
    // `(await\s+)?([A-Za-z_$][\w$]*)\s*\(`
    let called = |start: usize| {
        let end = identifier_end(safe, start)?;
        (safe.as_bytes().get(skip_space(safe, end)) == Some(&b'(')).then(|| {
            TableSite::Named(TableName {
                name: safe[start..end].to_string(),
                call: true,
                at: start,
            })
        })
    };
    let awaited = safe[at..]
        .strip_prefix("await")
        .map(|rest| skip_space(rest, 0))
        .filter(|spaced| *spaced > 0)
        .and_then(|spaced| called(at + "await".len() + spaced));
    awaited.or_else(|| called(at)).into_iter().collect()
}

/// Where the callback of a `useMemo(` written at `at` starts
/// (`(?:React\s*\.\s*)?useMemo\s*(?:<[^()]*>)?\s*\(\s*`).
fn memo_callback(safe: &str, at: usize) -> Option<usize> {
    let mut i = at;
    if let Some(rest) = safe[i..].strip_prefix("React") {
        let dot = skip_space(rest, 0);
        if rest.as_bytes().get(dot) == Some(&b'.') {
            i = skip_space(safe, i + "React".len() + dot + 1);
        }
    }
    i += safe[i..]
        .starts_with("useMemo")
        .then_some("useMemo".len())?;
    i = skip_space(safe, i);
    if safe.as_bytes().get(i) == Some(&b'<') {
        // `<[^()]*>`, greedy: the last `>` before the first parenthesis.
        let span = safe[i..].find(['(', ')']).map_or(safe.len(), |end| i + end);
        let close = i + safe[i..span].rfind('>')?;
        i = skip_space(safe, close + 1);
    }
    (safe.as_bytes().get(i) == Some(&b'(')).then(|| skip_space(safe, i + 1))
}

/// Picks, of a name's declarations in a file (where each keyword sits), the
/// one in scope at a use (`visibleAt`): in the innermost function around the
/// use that declares the name, the last declaration before the use, else the
/// first after it; else one at the module's top level. A declaration inside
/// another function is that function's own. With no use, it picks one at the
/// top level, which is what a module exports. The functions are the
/// graph's — a component, a method, an arrow function held in a `const` —
/// so the blocks inside one function are not told apart.
struct Visible<'c> {
    use_at: Option<usize>,
    safe: &'c str,
    file: &'c str,
    context: &'c dyn ResolutionContext,
}

impl<'c> Visible<'c> {
    fn new(
        use_at: Option<usize>,
        safe: &'c str,
        file: &'c str,
        context: &'c dyn ResolutionContext,
    ) -> Self {
        Self {
            use_at,
            safe,
            file,
            context,
        }
    }

    /// The index of the declaration in scope among `keywords`.
    fn pick(&self, keywords: &[usize]) -> Option<usize> {
        if keywords.is_empty() {
            return None;
        }
        let mut line_starts = vec![0];
        line_starts.extend(
            self.safe
                .bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(at, _)| at + 1),
        );
        let offset = |line: i64, column: i64| {
            let start = usize::try_from(line - 1)
                .ok()
                .and_then(|index| line_starts.get(index).copied())
                .unwrap_or(self.safe.len());
            start + usize::try_from(column).unwrap_or(0)
        };
        let functions: Vec<(usize, usize)> = self
            .context
            .get_nodes_in_file_shared(self.file)
            .iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    NodeKind::Method | NodeKind::Function | NodeKind::Component
                )
            })
            .map(|node| {
                (
                    offset(node.start_line, node.start_column),
                    offset(node.end_line, node.end_column),
                )
            })
            .collect();
        // The function a declaration's own value is (`const routes = (user)
        // => [ … ]`) starts after the keyword, so it is not around it.
        let around = |at: usize| -> Vec<usize> {
            functions
                .iter()
                .enumerate()
                .filter(|(_, (start, end))| *start < at && *end > at)
                .map(|(index, _)| index)
                .collect()
        };
        let scopes: HashSet<usize> = self
            .use_at
            .map(around)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut best = None;
        let mut depth: Option<usize> = None;
        for (index, &at) in keywords.iter().enumerate() {
            let enclosing = around(at);
            if !enclosing.iter().all(|function| scopes.contains(function)) {
                continue;
            }
            let deeper = depth.is_none_or(|depth| enclosing.len() > depth);
            let later =
                depth == Some(enclosing.len()) && self.use_at.is_some_and(|use_at| at < use_at);
            if deeper || later {
                best = Some(index);
                depth = Some(enclosing.len());
            }
        }
        best
    }
}

/// The tables the function written at `at` returns (`returnedTables`): its
/// expression body's (`(user) => [ … ]`, `() => buildRoutes()`), or what
/// each `return` of its own hands back, an early guard (`if (!user) return
/// [];`) and every branch alike. A `return` in a function written inside it
/// (`.map((p) => { return [ … ]; })`, a method's) is that function's.
fn returned_tables(safe: &str, at: usize) -> Vec<TableSite> {
    let Some(body) = function_body(safe, at) else {
        return Vec::new();
    };
    if !body.block {
        return held_table(safe, body.start);
    }
    own_returns(safe, body.start, body.end)
        .into_iter()
        .flat_map(|value| held_table(safe, value))
        .collect()
}

/// Where a function keeps its body: a block from its `{` to its `}`, or an
/// arrow's expression at `start`.
struct FunctionBody {
    block: bool,
    start: usize,
    end: usize,
}

/// The body of the function written at `at`, or `None` for no function
/// (`functionBody`).
fn function_body(safe: &str, at: usize) -> Option<FunctionBody> {
    let bytes = safe.as_bytes();
    let mut i = skip_space(safe, at);
    // `word\b`: the keyword standing alone, then its white space.
    let keyword = |i: &mut usize, word: &str| {
        let rest = &safe[*i..];
        if rest.starts_with(word)
            && !rest
                .as_bytes()
                .get(word.len())
                .copied()
                .is_some_and(is_word_byte)
        {
            *i = skip_space(safe, *i + word.len());
            true
        } else {
            false
        }
    };
    keyword(&mut i, "async");
    let arrow = !keyword(&mut i, "function");
    if !arrow {
        // `\*?\s*[A-Za-z_$][\w$]*`: a generator's star and the name.
        let mut name = i;
        if bytes.get(name) == Some(&b'*') {
            name = skip_space(safe, name + 1);
        }
        if let Some(end) = identifier_end(safe, name) {
            i = skip_space(safe, end);
        }
    }
    if bytes.get(i) == Some(&b'(') {
        let close = match_bracket(safe, i)?;
        i = skip_space(safe, close + 1);
    } else {
        // `user => [ … ]`
        if !arrow {
            return None;
        }
        let end = identifier_end(safe, i)?;
        i = skip_space(safe, end);
    }
    // A return type, up to the arrow or the body: `(): RouteObject[] => [ … ]`.
    if bytes.get(i) == Some(&b':') {
        let stop = if arrow { "=>" } else { "{" };
        i += safe[i..].find(stop)?;
    }
    if arrow {
        if !safe[i..].starts_with("=>") {
            return None;
        }
        i = skip_space(safe, i + 2);
        if bytes.get(i) != Some(&b'{') {
            return Some(FunctionBody {
                block: false,
                start: i,
                end: i,
            });
        }
    }
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    let end = block_end(safe, i)?;
    Some(FunctionBody {
        block: true,
        start: i,
        end,
    })
}

/// The statements that start a block of their own after a parenthesized
/// head: never a method's (`BLOCK_HEADS`).
const BLOCK_HEADS: [&str; 6] = ["if", "for", "while", "switch", "catch", "with"];

/// Where each `return` of the block opening at `open` hands back its value
/// (`ownReturns`): the block's own, past the functions written inside it —
/// an arrow's block, a `function`, a method like `lazy() { … }`.
fn own_returns(safe: &str, open: usize, close: usize) -> Vec<usize> {
    let bytes = safe.as_bytes();
    let mut values = Vec::new();
    // Past the body of a function whose parameters open at `paren`.
    let past_body = |paren: Option<usize>| -> Option<usize> {
        let params = match_bracket(safe, paren?)?;
        let mut i = skip_space(safe, params + 1);
        if bytes.get(i) == Some(&b':') {
            i += safe[i..].find('{')?;
        }
        (bytes.get(i) == Some(&b'{'))
            .then(|| block_end(safe, i))
            .flatten()
    };
    let mut i = open + 1;
    while i < close {
        let ch = bytes[i];
        if opens_string(safe, i) {
            let Some(end) = skip_string(safe, i) else {
                break;
            };
            i = end + 1;
            continue;
        }
        if ch == b'=' && bytes.get(i + 1) == Some(&b'>') {
            // An arrow's block is its own; past an expression body's `=>`
            // the scan goes on.
            let body = skip_space(safe, i + 2);
            i = (bytes.get(body) == Some(&b'{'))
                .then(|| block_end(safe, body))
                .flatten()
                .map_or(i + 2, |end| end + 1);
            continue;
        }
        if is_ident_start(ch) && !(i > 0 && (is_ident_byte(bytes[i - 1]) || bytes[i - 1] == b'.')) {
            let end = identifier_end(safe, i).expect("an identifier starts here");
            let name = &safe[i..end];
            let next = skip_space(safe, end);
            if name == "return" {
                values.push(next);
            } else if name == "function" {
                if let Some(past) = past_body(safe[next..].find('(').map(|at| next + at))
                    && past > 0
                {
                    i = past + 1;
                    continue;
                }
            } else if bytes.get(next) == Some(&b'(')
                && !BLOCK_HEADS.contains(&name)
                && let Some(past) = past_body(Some(next))
                && past > 0
            {
                i = past + 1;
                continue;
            }
            i = end;
            continue;
        }
        i += 1;
    }
    values
}

/// The index of the brace closing the block at `open`, or `None`
/// (`blockEnd`). A quote right after a word is an apostrophe in JSX text
/// (`<p>Don't</p>`), not a string.
fn block_end(safe: &str, open: usize) -> Option<usize> {
    let bytes = safe.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        if opens_string(safe, i) {
            i = skip_string(safe, i)? + 1;
            continue;
        }
        match bytes[i] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// True for a quote that opens a string or template, not an apostrophe in
/// JSX text (`opensString`).
fn opens_string(safe: &str, i: usize) -> bool {
    let bytes = safe.as_bytes();
    match bytes.get(i) {
        Some(b'`') => true,
        Some(b'"' | b'\'') => !(i > 0 && (is_word_byte(bytes[i - 1]) || bytes[i - 1] == b'$')),
        _ => false,
    }
}

/// Past JavaScript white space from `at` (`skipSpace`).
fn skip_space(s: &str, at: usize) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::framework_node;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    /// What `sites` stand for in `safe`: a literal's first 12 bytes, or a
    /// function's name and `()` when the table is what it returns.
    fn shown(sites: &[TableSite], safe: &str) -> Vec<String> {
        sites
            .iter()
            .map(|site| match site {
                TableSite::Literal(at) => safe[*at..].chars().take(12).collect(),
                TableSite::Named(name) => {
                    format!("{}{}", name.name, if name.call { "()" } else { "" })
                }
            })
            .collect()
    }

    fn held(safe: &str) -> Vec<String> {
        shown(&held_table(safe, 0), safe)
    }

    fn returned(safe: &str) -> Vec<String> {
        shown(&returned_tables(safe, 0), safe)
    }

    /// Upstream `react-router-call-tables` and `react-router-returned-tables`
    /// (#2668, #2672): a value holds the literal written there, or the table
    /// a function it calls returns, past `await`, and through `useMemo` what
    /// its callback returns, or the function it is handed.
    #[test]
    fn a_value_holds_the_table_a_literal_or_a_call_gives() {
        assert_eq!(held("([ { a: 1 } ])"), ["[ { a: 1 } ]"]);
        assert_eq!(held("getRouteMap({ coreRoutes })"), ["getRouteMap()"]);
        assert_eq!(held("await loadRoutes()"), ["loadRoutes()"]);
        assert_eq!(
            held("useMemo(() => getRoutes(user), [user])"),
            ["getRoutes()"]
        );
        assert_eq!(
            held("React.useMemo<RouteObject[]>(getRoutes, [])"),
            ["getRoutes()"]
        );
        assert_eq!(
            held("useMemo(function () { return [1]; }, [])"),
            ["[1]; }, [])"]
        );
        assert!(held("pages.map((p) => p)").is_empty());
        assert!(held("42").is_empty());
        // Not `useMemo`: a call like any other.
        assert_eq!(held("useMemoize(getRoutes, [])"), ["useMemoize()"]);
    }

    /// Every `return` of a function's own, an expression body, branches and
    /// an early guard alike; never one in a function written inside it, a
    /// method's, or an arrow callback's.
    #[test]
    fn a_function_returns_the_tables_of_its_own_returns() {
        assert_eq!(returned("(user) => [{ a: 1 }]"), ["[{ a: 1 }]"]);
        assert_eq!(returned("user => [2]"), ["[2]"]);
        assert_eq!(returned("() => buildRoutes()"), ["buildRoutes()"]);
        assert_eq!(
            returned(
                "function getRoutes(user) {\n  if (!user) return [];\n  if (user.admin) {\n    return [{ admin: 1 }];\n  }\n  return [{ rest: 1 }];\n}"
            ),
            ["[];\n  if (us", "[{ admin: 1 ", "[{ rest: 1 }"]
        );
        assert_eq!(
            returned(
                "function build() {\n  const menu = () => {\n    return [1];\n  };\n  function faq() {\n    return [2];\n  }\n  list.forEach((x) => { return [3]; });\n  return [{ loader() { return [4]; } }];\n}"
            ),
            ["[{ loader() "]
        );
        assert_eq!(
            returned("function f() {\n  const note = <p>Don't panic</p>;\n  return [5];\n}"),
            ["[5];\n}"]
        );
        assert_eq!(returned("(): RouteObject[] => [6]"), ["[6]"]);
        assert_eq!(
            returned("function f(): RouteObject[] { return [7]; }"),
            ["[7]; }"]
        );
        assert_eq!(
            returned("async function* gen() { return [8]; }"),
            ["[8]; }"]
        );
        assert_eq!(
            returned("(user = current(), leave = () => null) => [9]"),
            ["[9]"]
        );
        assert!(returned("routes").is_empty());
        assert!(
            returned("function f() { return pages.map((p) => ({ path: p.path })); }").is_empty()
        );
    }

    fn function(name: &str, file: &str, lines: (i64, i64), columns: (i64, i64)) -> Node {
        framework_node(
            format!("function:{name}"),
            NodeKind::Function,
            name.to_string(),
            name.to_string(),
            file.to_string(),
            lines.0,
            lines.1,
            columns.0,
            columns.1,
            Language::Jsx,
            false,
        )
    }

    /// `visibleAt`: of a name's declarations, the one in scope at the use —
    /// the innermost function's, else the module's — and with no use the
    /// module's own, which is what it exports.
    #[test]
    fn a_declaration_in_scope_holds_the_table() {
        let source = "function Sidebar() {\n  const routes = getMenu();\n  return null;\n}\nexport default function App() {\n  const routes = getRoutes(user);\n  return useRoutes(routes);\n}\nconst routes = topLevel();\n";
        let file = "src/App.jsx";
        let context = RouteCtx::default().file(file, source).routes(
            file,
            vec![
                function("Sidebar", file, (1, 4), (0, 1)),
                function("App", file, (5, 8), (15, 1)),
            ],
        );
        let pick = |use_at: Option<usize>| {
            let visible = Visible::new(use_at, source, file, &context);
            shown(&declared_table(source, "routes", false, &visible), source)
        };
        let in_app = source.find("useRoutes(routes)").unwrap() + "useRoutes(".len();
        let in_sidebar = source.find("return null").unwrap();
        assert_eq!(pick(Some(in_app)), ["getRoutes()"]);
        assert_eq!(pick(Some(in_sidebar)), ["getMenu()"]);
        assert_eq!(pick(None), ["topLevel()"]);
        // A function declaration answers a call, and a typed declaration its
        // literal.
        let visible = Visible::new(None, source, file, &context);
        let builder = "function buildRoutes() {\n  return [1];\n}\n";
        assert_eq!(
            shown(
                &declared_table(builder, "buildRoutes", true, &visible),
                builder
            ),
            ["[1];\n}\n"]
        );
        assert!(declared_table(builder, "buildRoutes", false, &visible).is_empty());
        let typed = "const routes: RouteObject[] = [{ a: 1 }];";
        assert_eq!(
            shown(&declared_table(typed, "routes", false, &visible), typed),
            ["[{ a: 1 }];"]
        );
        // `routesX` and `a.routes` declare no `routes`.
        assert!(
            declared_table(
                "const routesX = [1]; a.routes = [2];",
                "routes",
                false,
                &visible
            )
            .is_empty()
        );
    }

    /// `exportedTable`: `export default [ … ]`, a default-exported local, a
    /// name exported under another, and a default export that is a call.
    #[test]
    fn an_export_names_its_table() {
        let at = |file: &str, content: &str, exported: &str| {
            let context = RouteCtx::default().file(file, content);
            exported_table(file, exported, false, &context, 0)
                .into_iter()
                .map(|table| content[table.at..].chars().take(8).collect::<String>())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            at("a.js", "export default [{ a: 1 }];\n", "default"),
            ["[{ a: 1 "]
        );
        assert_eq!(
            at(
                "b.js",
                "const AppRoutes = [{ b: 1 }];\n\nexport default AppRoutes;\n",
                "default"
            ),
            ["[{ b: 1 "]
        );
        assert_eq!(
            at(
                "c.js",
                "const appRoutes = [{ c: 1 }];\nexport { appRoutes as routes };\n",
                "routes"
            ),
            ["[{ c: 1 "]
        );
        assert_eq!(
            at(
                "d.js",
                "const table = [{ d: 1 }];\nexport { table as default };\n",
                "default"
            ),
            ["[{ d: 1 "]
        );
        assert_eq!(
            at(
                "e.js",
                "function buildRoutes() {\n  return [{ e: 1 }];\n}\nexport default buildRoutes();\n",
                "default"
            ),
            ["[{ e: 1 "]
        );
        // A comment's non-ASCII text moves no offset into the file.
        assert_eq!(
            at(
                "g.js",
                "// The app’s routes — every page\nexport default [{ g: 1 }];\n",
                "default"
            ),
            ["[{ g: 1 "]
        );
        // `export default AppRoutes` read only to its statement's end.
        assert!(
            at(
                "f.js",
                "const AppRoutes = [1];\nexport default AppRoutes.filter(Boolean);\n",
                "default"
            )
            .is_empty()
        );
        assert_eq!(default_name("routes;\n"), Some("routes".to_string()));
        assert_eq!(default_name("routes  \nnext"), Some("routes".to_string()));
        assert_eq!(default_name("routes"), Some("routes".to_string()));
        assert_eq!(default_name("routes.x"), None);
    }

    fn import_of(file: &str, package: &str) -> Node {
        framework_node(
            format!("import:{file}:{package}"),
            NodeKind::Import,
            package.to_string(),
            package.to_string(),
            file.to_string(),
            1,
            1,
            0,
            0,
            Language::Jsx,
            false,
        )
    }

    /// Upstream `react-router.test.ts`, "a table mapped inside <Route path>
    /// in the same file": the table's routes sit under the `<Route path>` it
    /// is mapped inside and render inside its element. A file that makes a
    /// router had its route objects read when it was extracted, and a test
    /// file is no consumer.
    #[test]
    fn a_table_a_route_maps_is_read_where_it_is_mounted() {
        let routes = "import { Routes, Route } from 'react-router-dom';\n\
                      const DashboardRoutes = [\n\
                      \x20 { path: 'app', element: <DashboardApp/> },\n\
                      ];\n\
                      export default function Router() {\n\
                      \x20 return (\n\
                      \x20   <Routes>\n\
                      \x20     <Route path=\"/dashboard\" element={<DashboardLayout/>}>\n\
                      \x20       {DashboardRoutes.map((route) => <Route key={route.path} {...route}/>)}\n\
                      \x20     </Route>\n\
                      \x20   </Routes>\n\
                      \x20 );\n\
                      }\n";
        let router = "import { createBrowserRouter } from 'react-router-dom';\nconst routes = [{ path: '/own', element: <Own/> }];\nexport const router = createBrowserRouter(routes);\n";
        let context = RouteCtx::default()
            .file("src/routes.js", routes)
            .file("src/router.js", router)
            .file("src/routes.test.js", routes)
            .routes(
                "src/routes.js",
                vec![
                    import_of("src/routes.js", "react-router-dom"),
                    import_of("src/router.js", "react-router-dom"),
                    import_of("src/routes.test.js", "react-router-dom"),
                ],
            );
        let result = table_routes(&context);
        let nodes: Vec<(String, String)> = result
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.name.clone()))
            .collect();
        assert_eq!(
            nodes,
            [(
                "route:src/routes.js:3:table:/dashboard/app".to_string(),
                "/dashboard/app".to_string()
            )]
        );
        assert!(is_table_route(&result.nodes[0]));
        let references: Vec<&str> = result
            .references
            .iter()
            .map(|reference| reference.reference_name.as_str())
            .collect();
        assert_eq!(references, ["DashboardApp", "layout:DashboardLayout"]);
        assert!(result.references.iter().all(|reference| reference.line == 3
            && reference.from_node_id == result.nodes[0].id
            && reference.language == Language::Jsx));
    }

    #[test]
    fn a_table_route_is_known_by_its_id() {
        let route = |id: &str, file: &str, line: i64| {
            framework_node(
                id.to_string(),
                NodeKind::Route,
                "/x".to_string(),
                format!("{file}::route:/x"),
                file.to_string(),
                line,
                line,
                0,
                0,
                Language::Jsx,
                false,
            )
        };
        assert!(is_table_route(&route(
            "route:src/a.js:3:table:/x",
            "src/a.js",
            3
        )));
        assert!(!is_table_route(&route(
            "route:src/a.js:3:/x",
            "src/a.js",
            3
        )));
        assert!(!is_table_route(&route(
            "route:src/a.js:3:table:/x",
            "src/a.js",
            4
        )));
        assert!(!is_table_route(&route(
            "route:src/a.js:3:table:/x",
            "src/b.js",
            3
        )));
        let mut function = route("route:src/a.js:3:table:/x", "src/a.js", 3);
        function.kind = NodeKind::Function;
        assert!(!is_table_route(&function));
        let lines = LineIndex::new("a\nb\n\nc");
        assert_eq!(
            [0, 1, 2, 3, 4, 5].map(|at| lines.line_at(at)),
            [1, 1, 2, 2, 3, 4]
        );
    }
}
