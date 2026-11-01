//! Vue Router [`FrameworkResolver`]: routes declared in a config object,
//! navigation often by NAME. Ports upstream
//! `resolution/frameworks/vue-router.ts` at `e8ddc4e2`, which `b635dd46`
//! already had (F6, and #2130's admin-template tables).
//!
//! The Nuxt resolver in `vue.rs` reads Nuxt's file convention
//! (`pages/about.vue` is `/about`). A plain Vue app declares its routes in one
//! object:
//!
//! ```text
//! const router = createRouter({
//!   history: createWebHistory(),
//!   routes: [
//!     { name: 'login',   path: '/login',             component: () => import('@/views/Login') },
//!     { name: 'profile', path: '/profile/:username', component: Profile },
//!   ],
//! })
//! ```
//!
//! `extract` reads every such table into one route per entry, named by its
//! path, which `calls` the component it names: an identifier, which the
//! router file's import binds, or the module a lazy `() => import(…)` names,
//! whose default export renders. A child route's path is joined onto its
//! parents', and the parents' components are the layouts it renders inside.
//!
//! Navigation is usually a name, not a path:
//!
//! ```text
//! router.push({ name: 'login' })        // by name, the common idiom
//! router.push('/')                      // by path
//! router.push({ path: '/', query })     // by path, with extras
//! navigateTo('/dashboard')              // Nuxt
//! ```
//!
//! So `resolve` reads the argument as a name first, then as a path, each
//! only from a whole value: a string, a template whose holes fill whole
//! segments, a conditional of them, a local initialized to one, an object
//! destination's own field. A route's name is not stored on its
//! node, so the table re-reads the config files its own routes came from with
//! the same parser `extract` used. A Nuxt page (`vue.rs`) is in the table
//! too, by path. `<router-link to>` is markup, not a call (W7-18).
//!
//! Left unresolved rather than guessed: a computed destination, a name or
//! path nothing declares, a name or path two routes of the call's app carry,
//! and a component its import does not lead to (KEEP-RUST).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{self, JS_SPACE, ObjectField};
use super::route_href::{
    ExactTable, HOLE, HrefLiteral, Literal, RouteEntry, app_root_for, destinations_for_href,
    first_argument_text, parse_href, read_href_via_local_with, read_string_at, whole_string,
};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver, NavigationCalls};
use crate::import_resolver::resolve_import_path;
use crate::strip_comments::{
    CommentLang, strip_comments_for_regex, strip_js_comments_keeping_offsets,
};
use crate::types::{
    AlsoTarget, EdgeShape, FrameworkResolverExtractionResult, ReExport, RefView, ResolutionContext,
    ResolvedBy, ResolvedRef,
};

/// The languages routes and navigation calls are written in.
const ROUTE_LANGUAGES: [Language; 3] = [Language::TypeScript, Language::JavaScript, Language::Vue];

/// The packages a Vue Router app declares.
const VUE_ROUTER_APP_DEPENDENCIES: [&str; 3] = ["vue-router", "nuxt", "nuxt3"];

/// What a lazy route component reference starts with: `import:<spec>#<name>`.
const LAZY_PREFIX: &str = "import:";

/// What a layout reference starts with.
const LAYOUT_PREFIX: &str = "layout:";

/// What a route's reference to the component an identifier names starts
/// with: `component:<name>`. No node carries such a name, so the reference
/// is this resolver's alone: where the router file's import of the
/// identifier leads to no component, no other strategy binds a namesake
/// (KEEP-RUST: upstream writes the bare identifier, which the Vue resolver
/// and name matching then bind by name).
const COMPONENT_PREFIX: &str = "component:";

/// Modules re-exporting a component are read this many at most, per
/// component.
const MAX_REEXPORT_MODULES: usize = 8;

/// The identifier pattern upstream writes `[A-Za-z_$][\w$]*`.
const IDENT: &str = "[A-Za-z_$][A-Za-z0-9_$]*";

/// Children nested past this depth are not walked (upstream's bound).
const MAX_CHILD_DEPTH: usize = 12;

// =============================================================================
// Reading the routes tables
// =============================================================================

/// One route a routes table declares (`VueRouteEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VueRouteEntry {
    /// `/profile/:username`: the full path, a child's joined onto its
    /// parents'.
    pub path: String,
    /// `profile`: what `router.push({ name })` names, when the entry has one.
    pub name: Option<String>,
    /// The component the entry names, by identifier or by the tail of its
    /// lazy import.
    pub component: Option<String>,
    /// `@/views/dashboard/index`: the file a lazy `() => import(…)`
    /// component names.
    pub spec: Option<String>,
    /// The components of the parent routes it renders inside, outermost
    /// first.
    pub layouts: Vec<VueLayout>,
    pub line: i64,
}

/// A component a route names, with the file a lazy import names it by
/// (`VueLayout`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VueLayout {
    pub component: String,
    pub spec: Option<String>,
}

fn regex(cell: &'static OnceLock<Regex>, pattern: impl FnOnce() -> String) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&pattern()).expect("vue router pattern"))
}

/// True for a file that may hold a routes table: one that builds a router,
/// writes a `routes` array, imports `vue-router`, or lives in a `router/` or
/// `routes/` directory or is named `router.js` / `routes.ts`.
fn is_route_file(content: &str, file_path: &str) -> bool {
    static ROUTER_FACTORY: OnceLock<Regex> = OnceLock::new();
    static ROUTES_ARRAY: OnceLock<Regex> = OnceLock::new();
    static VUE_ROUTER_IMPORT: OnceLock<Regex> = OnceLock::new();
    static ROUTE_FILE_PATH: OnceLock<Regex> = OnceLock::new();
    regex(&ROUTER_FACTORY, || {
        format!(
            r"(?-u:\b)(?:createRouter|createWebHistory|createWebHashHistory|createMemoryHistory){JS_SPACE}*\(|(?-u:\b)new{JS_SPACE}+(?:VueRouter|Router){JS_SPACE}*\("
        )
    })
    .is_match(content)
        || regex(&ROUTES_ARRAY, || {
            format!(r"(?-u:\b)routes{JS_SPACE}*[:=]{JS_SPACE}*\[")
        })
        .is_match(content)
        || regex(&VUE_ROUTER_IMPORT, || {
            format!(r#"(?-u:\b)from{JS_SPACE}*['"]vue-router['"]"#)
        })
        .is_match(content)
        || regex(&ROUTE_FILE_PATH, || {
            r"(?:^|/)(?:router|routes)(?:/|\.[cm]?[jt]s$)".to_string()
        })
        .is_match(file_path)
}

/// Where a routes table starts: a `routes: [` field, or a declaration named
/// for what it holds (`export const constantRoutes = [`, `const routes:
/// RouteRecordRaw[] = [`, vue-element-admin's per-module `const tableRouter
/// = {`).
fn table_openers() -> &'static Regex {
    static TABLE_OPENERS: OnceLock<Regex> = OnceLock::new();
    regex(&TABLE_OPENERS, || {
        format!(
            r"(?-u:\b)routes{JS_SPACE}*[:=]{JS_SPACE}*\[|(?-u:\b)(?:const|let|var){JS_SPACE}+(?:{IDENT})?(?:[Rr]outes?|[Rr]outers?){JS_SPACE}*(?::[^=;\n]*)?={JS_SPACE}*([\[{{])"
        )
    })
}

/// Every route a file's routes tables declare (`parseVueRoutes`).
///
/// The tables are walked, not pattern-matched: a `name` is written above the
/// `path` it belongs to, so each `{…}` is matched as a unit and only its own
/// fields are read; a `meta: {…}` and a lazy `component: () => import(…)` are
/// stepped over, and `children` are walked in turn with the parent's path in
/// front of theirs, a child listed before its parent.
///
/// A route with children is a layout: its component renders the outlet they
/// fill. It is a screen of its own only when no child claims its address and
/// it does not `redirect` elsewhere. `children` are read only from an array
/// written in place: `children: kids` names an array declared elsewhere,
/// read there as a table of its own, and borrows no array written after it
/// (upstream's `indexOf('[')` reads the next one). Tables do not overlap, so
/// each entry is listed once, where it is written, and two entries that
/// repeat a path and a name are two routes (upstream keeps the first;
/// KEEP-RUST). A top-level entry whose path does not start with `/` (`*`, a
/// catch-all) is no address. A table is read only from a file
/// [`is_route_file`] accepts.
pub fn parse_vue_routes(content: &str, file_path: &str) -> Vec<VueRouteEntry> {
    if !is_route_file(content, file_path) {
        return Vec::new();
    }
    let safe = strip_comments_for_regex(content, CommentLang::TypeScript);
    let mut walk = Walk {
        safe: &safe,
        out: Vec::new(),
    };
    let mut walked: HashSet<usize> = HashSet::new();
    let openers = table_openers();
    let mut at = 0;
    while let Some(found) = openers.find_at(&safe, at) {
        let open = found.end() - 1;
        at = found.end();
        let Some(close) = js_scan::match_bracket(&safe, open) else {
            continue;
        };
        if !walked.insert(open) {
            continue;
        }
        if safe.as_bytes()[open] == b'{' {
            walk.visit(open, close, None, &[], 0);
        } else {
            for (start, end) in js_scan::top_level_objects(&safe, open + 1, close) {
                walk.visit(start, end, None, &[], 0);
            }
        }
        // The next table starts past this one.
        at = at.max(close);
    }
    walk.out
}

/// The state of one [`parse_vue_routes`] walk.
struct Walk<'s> {
    safe: &'s str,
    out: Vec<VueRouteEntry>,
}

impl Walk<'_> {
    fn line_of(&self, at: usize) -> i64 {
        self.safe[..at].matches('\n').count() as i64 + 1
    }

    fn visit(
        &mut self,
        start: usize,
        end: usize,
        prefix: Option<&str>,
        layouts: &[VueLayout],
        depth: usize,
    ) {
        let safe = self.safe;
        let fields = js_scan::read_fields(safe, start, end);
        let field = |key: &str| fields.iter().find(|field: &&ObjectField| field.key == key);
        let Some(path_field) = field("path") else {
            return;
        };
        let Some(own) = read_string_at(path_field.text.trim_start_matches(js_scan::is_js_space), 0)
        else {
            return;
        };
        let mut path = if own.starts_with('/') {
            own
        } else {
            // A top-level `*` or relative path is no address.
            let Some(prefix) = prefix else {
                return;
            };
            if own.is_empty() {
                prefix.to_string()
            } else {
                format!("{}/{own}", if prefix == "/" { "" } else { prefix })
            }
        };
        if path.len() > 1 {
            path = path.trim_end_matches('/').to_string();
        }

        let component = field("component")
            .or_else(|| field("components"))
            .and_then(|component| component_of(component.text));

        let children = field("children");
        let mut child_claims_address = false;
        if let Some(children) = children
            && depth < MAX_CHILD_DEPTH
            && js_scan::js_trim(children.text).starts_with('[')
            && let Some(open) = safe[children.at..].find('[').map(|i| i + children.at)
            && let Some(close) = js_scan::match_bracket(safe, open)
            && close > open
        {
            let mut inner = layouts.to_vec();
            if let Some(component) = &component {
                inner.push(component.clone());
            }
            for (child_start, child_end) in js_scan::top_level_objects(safe, open + 1, close) {
                let child_own = js_scan::read_fields(safe, child_start, child_end)
                    .into_iter()
                    .find(|field| field.key == "path")
                    .and_then(|path| {
                        read_string_at(path.text.trim_start_matches(js_scan::is_js_space), 0)
                    });
                if child_own
                    .as_deref()
                    .is_some_and(|own| own.is_empty() || own == path)
                {
                    child_claims_address = true;
                }
                self.visit(child_start, child_end, Some(&path), &inner, depth + 1);
            }
        }
        let Some(component) = component else {
            return;
        };
        if children.is_some() && (child_claims_address || field("redirect").is_some()) {
            return;
        }
        let name = field("name")
            .and_then(|name| read_string_at(name.text.trim_start_matches(js_scan::is_js_space), 0));
        let line = self.line_of(path_field.at);
        self.out.push(VueRouteEntry {
            path,
            name,
            component: Some(component.component),
            spec: component.spec,
            layouts: layouts.to_vec(),
            line,
        });
    }
}

/// The component an entry names (`componentOf`): an identifier, or the file
/// a lazy import names, by its tail without the extension.
fn component_of(value: &str) -> Option<VueLayout> {
    static LAZY: OnceLock<Regex> = OnceLock::new();
    static IDENTIFIER: OnceLock<Regex> = OnceLock::new();
    if let Some(spec) = regex(&LAZY, || {
        format!(r#"(?-u:\b)import{JS_SPACE}*\({JS_SPACE}*['"`]([^'"`]+)['"`]"#)
    })
    .captures(value)
    .and_then(|captures| captures.get(1))
    {
        let spec = spec.as_str();
        let tail = spec.rsplit('/').next().unwrap_or_default();
        // `.replace(/\.\w+$/, '')`: the extension, when the tail after the
        // last `.` is all word characters.
        let tail = match tail.rfind('.') {
            Some(dot)
                if dot + 1 < tail.len()
                    && tail[dot + 1..]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                &tail[..dot]
            }
            _ => tail,
        };
        return (!tail.is_empty()).then(|| VueLayout {
            component: tail.to_string(),
            spec: Some(spec.to_string()),
        });
    }
    regex(&IDENTIFIER, || {
        format!(r"^{JS_SPACE}*([A-Z][A-Za-z0-9_]*){JS_SPACE}*$")
    })
    .captures(value)
    .and_then(|captures| captures.get(1))
    .map(|name| VueLayout {
        component: name.as_str().to_string(),
        spec: None,
    })
}

/// The language a file's routes and references carry (`languageForFile`).
fn language_for_file(file_path: &str) -> Language {
    if file_path.ends_with(".vue") {
        Language::Vue
    } else if [".ts", ".mts", ".cts"]
        .iter()
        .any(|ext| file_path.ends_with(ext))
    {
        Language::TypeScript
    } else {
        Language::JavaScript
    }
}

/// The id a config-declared route carries (`routeId`): a verbatim
/// reconstruction, so the table recognises its own.
fn route_id(file_path: &str, line: i64, path: &str) -> String {
    format!("route:{file_path}:{line}:{path}:vue")
}

/// True for a route node this resolver emitted, and no other
/// (`isVueConfigRoute`).
fn is_vue_config_route(node: &Node) -> bool {
    node.id == route_id(&node.file_path, node.start_line, &node.name)
}

/// True for a Nuxt page route `vue.rs` emitted (`isNuxtPage`).
fn is_nuxt_page(node: &Node) -> bool {
    node.language == Language::Vue
        && format!("/{}", node.file_path).contains("/pages/")
        && node.id == format!("route:{}:{}:1", node.file_path, node.name)
}

/// True for a route the table holds: a config route or a Nuxt page, at an
/// absolute path (`inVueTable`).
fn in_vue_table(node: &Node) -> bool {
    (is_vue_config_route(node) || is_nuxt_page(node)) && node.name.starts_with('/')
}

// =============================================================================
// Route table: by path, and by name
// =============================================================================

/// One app's routes, by path and, Vue's own idiom, by name
/// (`VueAppRoutes`).
#[derive(Debug, Default)]
struct VueAppRoutes {
    exact: ExactTable,
    dynamic: Vec<RouteEntry>,
    /// `login` → the routes of that name: `router.push({ name: 'login' })`
    /// reaches the one route, and a name two routes carry reaches nothing
    /// (KEEP-RUST: upstream keeps the first in node order, which a sync
    /// changes).
    by_name: BTreeMap<String, Vec<Arc<Node>>>,
}

/// The routes of every Vue app, by the directory the app lives in
/// (`vueRouteTable`). A call matches only the routes of its own app.
#[derive(Debug, Default)]
struct VueRouteTable {
    by_root: BTreeMap<String, VueAppRoutes>,
}

impl VueRouteTable {
    /// The table, entry by entry: a Nuxt page is its node, and a config
    /// route each entry of its file's tables that names it. A route's name
    /// is not stored on its node, so the config files the routes came from
    /// are read again with the parser `extract` used, and each entry finds
    /// its node by the id it was extracted with. Two entries one id serves
    /// (one line, one path) make that route ambiguous by path and by each
    /// of their names.
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut table = Self::default();
        let mut config_files: BTreeMap<String, (String, HashMap<String, Arc<Node>>)> =
            BTreeMap::new();
        for node in context.get_nodes_by_kind_shared(NodeKind::Route) {
            if !in_vue_table(&node) {
                continue;
            }
            let root = app_root_for(&node.file_path);
            if is_vue_config_route(&node) {
                config_files
                    .entry(node.file_path.clone())
                    .or_insert_with(|| (root, HashMap::new()))
                    .1
                    .insert(node.id.clone(), node);
            } else {
                table.add(&root, &node.name, &node, None, 1);
            }
        }
        for (file_path, (root, nodes)) in config_files {
            let entries = context
                .read_file_shared(&file_path)
                .map(|content| parse_vue_routes(&content, &file_path))
                .unwrap_or_default();
            let mut by_id: BTreeMap<String, Vec<VueRouteEntry>> = BTreeMap::new();
            for entry in entries {
                let id = route_id(&file_path, entry.line, &entry.path);
                if nodes.contains_key(&id) {
                    by_id.entry(id).or_default().push(entry);
                }
            }
            for (id, node) in &nodes {
                match by_id.get(id) {
                    Some(entries) => {
                        for entry in entries {
                            table.add(
                                &root,
                                &entry.path,
                                node,
                                entry.name.as_deref(),
                                entries.len(),
                            );
                        }
                    }
                    // A route its file no longer declares as read: its path.
                    None => table.add(&root, &node.name, node, None, 1),
                }
            }
        }
        table
    }

    /// Register `node` at `path` (and under `name`) in the app at `root`,
    /// `times` over: a route listed more than once there answers nothing.
    fn add(&mut self, root: &str, path: &str, node: &Arc<Node>, name: Option<&str>, times: usize) {
        let app = self.by_root.entry(root.to_string()).or_default();
        for _ in 0..times {
            if path.contains(':') {
                app.dynamic.push(RouteEntry::new(Arc::clone(node)));
            }
            app.exact
                .insert(path.to_string(), root.to_string(), Arc::clone(node));
            if let Some(name) = name {
                app.by_name
                    .entry(name.to_string())
                    .or_default()
                    .push(Arc::clone(node));
            }
        }
    }

    /// The routes of the app `file_path` belongs to: the longest root above
    /// it (`routesForFile`).
    fn app_of(&self, file_path: &str) -> Option<&VueAppRoutes> {
        self.by_root
            .iter()
            .filter(|(root, _)| file_path.starts_with(root.as_str()))
            .max_by_key(|(root, _)| root.len())
            .map(|(_, app)| app)
    }
}

/// The pass's Vue Router table.
fn vue_route_table(context: &dyn ResolutionContext) -> Arc<VueRouteTable> {
    context
        .route_tables()
        .get_or_build("vue-router", || VueRouteTable::build(context))
}

// =============================================================================
// Navigation calls
// =============================================================================

/// `router.push` / `.replace` (the Composition API), `this.$router.push` /
/// `$router.push` (the Options API and templates), and Nuxt's `navigateTo`
/// (`NAV_CALL`). `push` and `replace` need a receiver that names a router:
/// an unqualified `push` is an array's.
fn nav_call() -> &'static Regex {
    static NAV_CALL: OnceLock<Regex> = OnceLock::new();
    regex(&NAV_CALL, || {
        r"^(?:this\.)?\$?router\.(?:push|replace)$|^navigateTo$".to_string()
    })
}

/// The verb a navigation call name stands for, or `None` (`vueNavVerb`).
pub fn vue_nav_verb(name: &str) -> Option<&str> {
    nav_call()
        .is_match(name)
        .then(|| name.rsplit('.').next().unwrap_or(name))
}

/// The value text of the object destination `expr`'s own `key` field, or
/// `None` when `expr` is no object or writes no such field. A nested
/// object's `name` (`params: { name }`) is no field of the destination.
fn own_field<'e>(expr: &'e str, key: &str) -> Option<&'e str> {
    let args = js_scan::js_trim(expr);
    if !args.starts_with('{') {
        return None;
    }
    let end = js_scan::match_bracket(args, 0)?;
    js_scan::read_fields(args, 0, end)
        .into_iter()
        .find(|field| field.key == key)
        .map(|field| field.text)
}

/// The route name in a `{ name: 'login' }` destination
/// (`routeNameInExpression`): the object's own `name`, a whole string
/// literal with no hole; `None` for anything else.
pub fn route_name_in_expression(expr: &str) -> Option<String> {
    whole_string(own_field(expr, "name")?).filter(|name| !name.contains(HOLE))
}

/// `{ path: '/', query }`: Vue's object destination, whose key is `path`
/// (`parseVuePathObject`). The object's own `path`, read as a whole
/// destination: a literal, a template or a conditional of them, never the
/// head of a concatenation.
fn parse_vue_path_object(expr: &str) -> Option<HrefLiteral> {
    parse_href(own_field(expr, "path")?, Literal::Whole)
}

/// True for the reference this resolver's `extract` emitted from a route to
/// a component (`isVueRouteRef`).
fn is_vue_route_ref(reference: &RefView) -> bool {
    reference.from_node_id.starts_with("route:") && reference.from_node_id.ends_with(":vue")
}

/// The component a route names by name (`vueComponentNamed`): a `.vue`
/// file's own component node, or a component declared in a plain script.
/// One candidate is it; among several, the one in the route's app; an
/// ambiguous name is none.
fn vue_component_named(
    name: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Component
                || (node.kind == NodeKind::Function && node.file_path.ends_with(".vue"))
        })
        .collect();
    if let [only] = candidates.as_slice() {
        return Some(only.id.clone());
    }
    let root = app_root_for(from_file);
    match candidates
        .iter()
        .filter(|node| node.file_path.starts_with(&root))
        .collect::<Vec<_>>()
        .as_slice()
    {
        [near] => Some(near.id.clone()),
        _ => None,
    }
}

/// `import:@/views/Login#Login` for a lazy component, the bare name for an
/// identifier (`componentRefName`): what a layout reference names.
fn component_ref_name(component: &str, spec: Option<&str>) -> String {
    match spec {
        Some(spec) => format!("{LAZY_PREFIX}{spec}#{component}"),
        None => component.to_string(),
    }
}

/// The name of a route's `calls` reference to its component: a lazy one's
/// [`component_ref_name`], `component:<name>` for an identifier.
fn route_ref_name(component: &str, spec: Option<&str>) -> String {
    match spec {
        Some(_) => component_ref_name(component, spec),
        None => format!("{COMPONENT_PREFIX}{component}"),
    }
}

/// The component a route's reference names (`routeComponent`). A lazy
/// import names a module, and the component is that module's default
/// export: vue-element-admin's views are all `…/index.vue`, so the name
/// alone (`index`) fits dozens. An identifier is what the router file's
/// import of it takes: a default import the module's default export, a named
/// import the module's node of that name, through the modules that re-export
/// it. Where an import names the module, the answer is that module's or
/// none: a module not found, found twice or holding no such component never
/// falls back to a namesake. Only a name no import reaches is looked up: a
/// declaration of the router file itself, else a component by name
/// (`vueComponentNamed`), `index` never.
fn route_component(
    encoded: &str,
    from_file: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let encoded = encoded.strip_prefix(COMPONENT_PREFIX).unwrap_or(encoded);
    if let Some(rest) = encoded.strip_prefix(LAZY_PREFIX) {
        let (spec, _) = rest
            .rsplit_once('#')
            .filter(|(spec, name)| !spec.is_empty() && !name.is_empty())?;
        let file = module_file(spec, from_file, context)?;
        return export_node(&file, &Export::Default, context, &mut BTreeSet::new());
    }
    if let Some(mapping) = context
        .get_import_mappings(from_file, language_for_file(from_file))
        .into_iter()
        .find(|mapping| mapping.local_name == encoded)
    {
        // A namespace import (`* as L`) takes `*`, which no node is.
        let file = module_file(&mapping.source, from_file, context)?;
        let export = if mapping.is_default || mapping.exported_name == "default" {
            Export::Default
        } else {
            Export::Named(mapping.exported_name)
        };
        return export_node(&file, &export, context, &mut BTreeSet::new());
    }
    if encoded == "index" {
        return None;
    }
    export_in_file(from_file, encoded, Declared::Anywhere, context)
        .or_else(|| vue_component_named(encoded, from_file, context))
}

/// The module `spec` names from `from_file`: as the file resolves the import,
/// or, through an alias the resolver does not know (vben's `#/…`), the one
/// file of its app whose path ends the way the specifier does
/// (`fileBySuffix`). A relative specifier is the file it names or none.
fn module_file(spec: &str, from_file: &str, context: &dyn ResolutionContext) -> Option<String> {
    let resolved = resolve_import_path(spec, from_file, Language::Vue, context);
    if resolved.is_some() || spec.starts_with('.') {
        return resolved;
    }
    file_by_suffix(spec, from_file, context)
}

/// What an import takes from a module.
enum Export {
    /// Its default export.
    Default,
    /// Its export of that name.
    Named(String),
}

/// The node `export` names in `file`, through the modules that re-export it:
/// `export { default as BasicLayout } from './basic.vue'`, `export { X } from
/// './x'`, `export { local as X }`, and for a name, `export * from './x'`
/// when exactly one such module holds it. Re-exports are followed through at
/// most [`MAX_REEXPORT_MODULES`] modules, each read once; a module holding
/// none is none.
fn export_node(
    file: &str,
    export: &Export,
    context: &dyn ResolutionContext,
    visited: &mut BTreeSet<String>,
) -> Option<String> {
    if !visited.insert(file.to_string()) {
        return None;
    }
    let direct = match export {
        Export::Default => component_in_file(file, context),
        Export::Named(name) => export_in_file(file, name, Declared::Exported, context),
    };
    if direct.is_some() || visited.len() > MAX_REEXPORT_MODULES {
        return direct;
    }
    let wanted = match export {
        Export::Default => "default",
        Export::Named(name) => name.as_str(),
    };
    let re_exports = context.get_re_exports(file, language_for_file(file));
    for re_export in &re_exports {
        match re_export {
            ReExport::Named {
                exported_name,
                original_name,
                source,
            } if exported_name == wanted => {
                let next = module_file(source, file, context)?;
                let export = if original_name == "default" {
                    Export::Default
                } else {
                    Export::Named(original_name.clone())
                };
                return export_node(&next, &export, context, visited);
            }
            ReExport::LocalAlias {
                exported_name,
                original_name,
            } if exported_name == wanted => {
                return export_in_file(file, original_name, Declared::TopLevel, context);
            }
            _ => {}
        }
    }
    if matches!(export, Export::Default) {
        return None;
    }
    let mut found: BTreeSet<String> = BTreeSet::new();
    for re_export in &re_exports {
        if let ReExport::Wildcard { source } = re_export
            && let Some(next) = module_file(source, file, context)
            && let Some(node) = export_node(&next, export, context, visited)
        {
            found.insert(node);
        }
    }
    match found.len() {
        1 => found.into_iter().next(),
        _ => None,
    }
}

/// The component a module renders as its default (`componentInFile`): an
/// SFC's own component node, else the node the module's `export default`
/// names. A module that says neither is none.
fn component_in_file(file: &str, context: &dyn ResolutionContext) -> Option<String> {
    if file.ends_with(".vue") {
        let nodes = context.get_nodes_in_file_shared(file);
        let components: Vec<&Arc<Node>> = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Component)
            .collect();
        return match components.as_slice() {
            [only] => Some(only.id.clone()),
            _ => None,
        };
    }
    let content = context.read_file_shared(file)?;
    let stripped = strip_comments_for_regex(&content, CommentLang::TypeScript);
    let (name, _) = super::route_href::default_export_name(&stripped)?;
    export_in_file(file, &name, Declared::TopLevel, context)
}

/// Which declarations of a file a lookup by name may take.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Declared {
    /// A top-level one a named export declares (`export const X`, not
    /// `export default function X`); an export list is a re-export.
    Exported,
    /// A top-level one, the binding an export list or `export default X`
    /// names.
    TopLevel,
    /// Any, nested ones too: the routes file's own identifier.
    Anywhere,
}

/// The one node of `file` named `export` that a route can render: a
/// component, a function, a class or the value holding one
/// (`IFrameView = () => import(…)`), declared as `declared` says. Whether a
/// node is the module's own is read from the module's text where the node
/// starts ([`js_scan::at_top_level`]): the extractor marks a declaration
/// inside an exported function exported too, and names one inside an
/// anonymous `export default function () {…}` as if it were the module's.
/// Only a script module's text is read so: a component file's (`.vue`)
/// template is no script. Several, or none, is none.
fn export_in_file(
    file: &str,
    export: &str,
    declared: Declared,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let nodes = context.get_nodes_in_file_shared(file);
    let mut named: Vec<&Arc<Node>> = nodes
        .iter()
        .filter(|node| {
            node.name == export
                && matches!(
                    node.kind,
                    NodeKind::Component
                        | NodeKind::Function
                        | NodeKind::Class
                        | NodeKind::Constant
                        | NodeKind::Variable
                )
        })
        .collect();
    if declared != Declared::Anywhere {
        if !crate::context::is_js_family_path_pub(file) {
            return None;
        }
        let content = context.read_file_shared(file)?;
        let code = strip_js_comments_keeping_offsets(&content);
        named.retain(|node| {
            byte_offset(&code, node.start_line, node.start_column).is_some_and(|at| {
                js_scan::at_top_level(&code, at)
                    && (declared == Declared::TopLevel
                        || (node.is_exported && !js_scan::follows_export_default(&code, at)))
            })
        });
    }
    match named.as_slice() {
        [only] => Some(only.id.clone()),
        _ => None,
    }
}

/// The byte offset of 1-based `line`, byte `column` (a node's start) in
/// `text`, if the line is that long.
fn byte_offset(text: &str, line: i64, column: i64) -> Option<usize> {
    let skip = usize::try_from(line.checked_sub(1)?).ok()?;
    let start = match skip {
        0 => 0,
        _ => text.match_indices('\n').nth(skip - 1)?.0 + 1,
    };
    let at = start.checked_add(usize::try_from(column).ok()?)?;
    (!text.get(start..at)?.contains('\n')).then_some(at)
}

/// The suffixes a component file may add to its specifier.
const COMPONENT_EXTENSIONS: [&str; 10] = [
    "",
    ".vue",
    ".tsx",
    ".jsx",
    ".ts",
    ".js",
    "/index.vue",
    "/index.tsx",
    "/index.ts",
    "/index.js",
];

/// The one file of `from_file`'s app whose path ends like `spec` without its
/// alias (`#/`, `@/`, `~/`) or relative prefix (`fileBySuffix`).
fn file_by_suffix(spec: &str, from_file: &str, context: &dyn ResolutionContext) -> Option<String> {
    static ALIAS: OnceLock<Regex> = OnceLock::new();
    let rest = regex(&ALIAS, || {
        r"^(?:[@#~$][A-Za-z0-9_\-]*/|\.{1,2}/)+".to_string()
    })
    .replace(spec, "");
    if rest == spec || rest.is_empty() {
        return None;
    }
    let root = app_root_for(from_file);
    let tails: Vec<(String, String)> = COMPONENT_EXTENSIONS
        .iter()
        .map(|ext| (format!("{rest}{ext}"), format!("/{rest}{ext}")))
        .collect();
    let mut hits: BTreeSet<&str> = BTreeSet::new();
    let files = context.get_all_files_shared();
    for file in files.iter().filter(|file| file.starts_with(&root)) {
        if tails
            .iter()
            .any(|(tail, nested)| file == tail || file.ends_with(nested.as_str()))
        {
            hits.insert(file.as_str());
        }
    }
    match hits.len() {
        1 => hits.into_iter().next().map(str::to_string),
        _ => None,
    }
}

// =============================================================================
// The resolver
// =============================================================================

/// Vue Router resolver (`vueRouterResolver`).
pub struct VueRouterResolver;

/// The navigation calls Vue Router binds to its routes: a route answers the
/// calls of its own app.
struct VueNavigation;

impl NavigationCalls for VueNavigation {
    fn tails(&self) -> &[&str] {
        &["push", "replace", "navigateTo"]
    }

    fn scope(&self, route: &Node, _context: &dyn ResolutionContext) -> Option<Vec<String>> {
        in_vue_table(route).then(|| vec![app_root_for(&route.file_path)])
    }
}

impl FrameworkResolver for VueRouterResolver {
    fn name(&self) -> &str {
        "vue-router"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&ROUTE_LANGUAGES)
    }

    fn app_dependencies(&self) -> Option<&'static [&'static str]> {
        Some(&VUE_ROUTER_APP_DEPENDENCIES)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        super::package_deps::depends_on(context, &VUE_ROUTER_APP_DEPENDENCIES)
    }

    fn claims_reference(&self, name: &str) -> bool {
        nav_call().is_match(name)
            || name.starts_with(LAZY_PREFIX)
            || name.starts_with(LAYOUT_PREFIX)
            || name.starts_with(COMPONENT_PREFIX)
    }

    fn navigation(&self) -> Option<&dyn NavigationCalls> {
        Some(&VueNavigation)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let entries = parse_vue_routes(content, file_path);
        let mut result = FrameworkResolverExtractionResult::default();
        if entries.is_empty() {
            return Some(result);
        }
        let language = language_for_file(file_path);
        let mut ids: HashSet<String> = HashSet::new();
        for entry in entries {
            let id = route_id(file_path, entry.line, &entry.path);
            // Two entries of one line at one path would share an id: the
            // first keeps it.
            if !ids.insert(id.clone()) {
                continue;
            }
            let node = framework_node(
                id,
                NodeKind::Route,
                entry.path.clone(),
                format!("{file_path}::route:{}", entry.path),
                file_path.to_string(),
                entry.line,
                entry.line,
                0,
                0,
                language,
                false,
            );
            let reference = |name: String, kind: EdgeKind| RefView {
                row_id: None,
                from_node_id: node.id.clone(),
                reference_name: name,
                reference_kind: kind,
                line: entry.line,
                column: 0,
                file_path: file_path.to_string(),
                language,
                is_function_ref: false,
                reference_subkind: None,
            };
            if let Some(component) = &entry.component {
                // `calls`, as every component-backed screen binds: a
                // `references` candidate list keeps the reference's own
                // language family, and a `.js` router names a `.vue` view.
                result.references.push(reference(
                    route_ref_name(component, entry.spec.as_deref()),
                    EdgeKind::Calls,
                ));
            }
            // The layouts around it: what they render, a sidebar or a
            // navbar, is on this screen too.
            for layout in &entry.layouts {
                result.references.push(reference(
                    format!(
                        "{LAYOUT_PREFIX}{}",
                        component_ref_name(&layout.component, layout.spec.as_deref())
                    ),
                    EdgeKind::References,
                ));
            }
            result.nodes.push(node);
        }
        Some(result)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        if reference.reference_kind == EdgeKind::References && is_vue_route_ref(reference) {
            let layout = reference.reference_name.strip_prefix(LAYOUT_PREFIX)?;
            let target = route_component(layout, &reference.file_path, context)?;
            let mut metadata = serde_json::Map::new();
            metadata.insert("layout".to_string(), serde_json::Value::Bool(true));
            return Some(
                ResolvedRef::new(reference.clone(), target, 0.95, ResolvedBy::Framework).with_edge(
                    EdgeShape {
                        edge_kind: None,
                        metadata,
                        also_targets: Vec::new(),
                    },
                ),
            );
        }
        if reference.reference_kind != EdgeKind::Calls {
            return None;
        }

        // A route naming the component it renders: this resolver's own
        // reference, bound here rather than by name alone. A Vue app usually
        // has a `.vue` `Login` view and a `login` action in a store, and only
        // one of them is the screen.
        if is_vue_route_ref(reference) {
            return route_component(&reference.reference_name, &reference.file_path, context).map(
                |target| ResolvedRef::new(reference.clone(), target, 0.95, ResolvedBy::Framework),
            );
        }

        let verb = vue_nav_verb(&reference.reference_name)?;
        if !ROUTE_LANGUAGES.contains(&reference.language) {
            return None;
        }
        let table = vue_route_table(context);
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

        // By name first: `{ name: 'login' }` is the idiom Vue apps are
        // written in.
        if let Some(named) = route_name_in_expression(&arg) {
            let [target] = app.by_name.get(&named)?.as_slice() else {
                return None;
            };
            let mut metadata = serde_json::Map::new();
            metadata.insert("href".to_string(), serde_json::json!(named));
            metadata.insert("navMethod".to_string(), serde_json::json!(verb));
            metadata.insert("by".to_string(), serde_json::json!("name"));
            return Some(
                ResolvedRef::new(
                    reference.clone(),
                    target.id.clone(),
                    0.95,
                    ResolvedBy::Framework,
                )
                .with_edge(EdgeShape {
                    edge_kind: Some(EdgeKind::Navigates),
                    metadata,
                    also_targets: Vec::new(),
                }),
            );
        }

        // Otherwise a path, read as the other routers read one, but only as a
        // whole literal: `'/redirect' + fullPath` goes past `/redirect`.
        let mut href = parse_href(&arg, Literal::Whole).or_else(|| parse_vue_path_object(&arg));
        if href.is_none() {
            let start = context
                .get_node_by_id_shared(&reference.from_node_id)
                .filter(|enclosing| enclosing.file_path == reference.file_path)
                .map_or((reference.line - 40).max(1), |enclosing| {
                    enclosing.start_line
                });
            href = read_href_via_local_with(
                &lines,
                reference.line,
                reference.column,
                verb,
                start,
                Literal::Whole,
            );
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
    use crate::types::ImportMapping;

    /// Upstream `vue-router.test.ts`'s `ROUTER_SOURCE`.
    const ROUTER_SOURCE: &str = "import { createRouter, createWebHistory } from \"vue-router\"\nconst router = createRouter({\n  history: createWebHistory(),\n  routes: [\n    {\n      name: \"home\",\n      path: \"/\",\n      component: () => import(\"@/views/Home\")\n    },\n    {\n      name: \"login\",\n      path: \"/login\",\n      component: () => import(\"@/views/Login\")\n    },\n    {\n      name: \"settings\",\n      path: \"/settings\",\n      component: () => import(\"@/views/Settings\"),\n      meta: { requiresAuth: true }\n    },\n    {\n      name: \"profile\",\n      path: \"/profile/:username\",\n      component: Profile,\n      children: [\n        { path: \"favorites\", component: Favorites }\n      ]\n    }\n  ]\n})\nexport default router\n";

    /// Upstream `vue-router: parseVueRoutes`.
    #[test]
    fn every_entry_takes_its_own_name_and_component() {
        let entries = parse_vue_routes(ROUTER_SOURCE, "");
        // The name is written above the path it belongs to; a child is
        // listed before its parent.
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.name.as_deref(), entry.path.as_str()))
                .collect::<Vec<_>>(),
            [
                (Some("home"), "/"),
                (Some("login"), "/login"),
                (Some("settings"), "/settings"),
                (None, "/profile/:username/favorites"),
                (Some("profile"), "/profile/:username"),
            ]
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.component.as_deref())
                .collect::<Vec<_>>(),
            [
                Some("Home"),
                Some("Login"),
                Some("Settings"),
                Some("Favorites"),
                Some("Profile")
            ]
        );
        assert_eq!(entries[0].spec.as_deref(), Some("@/views/Home"));
        assert_eq!(entries[4].spec, None);
        // A child is joined onto its parent, which is the layout around it.
        assert_eq!(
            entries[3]
                .layouts
                .iter()
                .map(|layout| layout.component.as_str())
                .collect::<Vec<_>>(),
            ["Profile"]
        );
        assert_eq!(
            entries.iter().map(|entry| entry.line).collect::<Vec<_>>(),
            [7, 12, 17, 26, 23]
        );
        // Nothing on a file that declares no routes.
        assert_eq!(
            parse_vue_routes("export const paths = [{ path: \"/x\" }]\n", ""),
            []
        );
        assert_eq!(parse_vue_routes("const x = 1\n", ""), []);
    }

    /// Upstream `vue-element-admin`: named tables, `new Router`, a module
    /// file's table object, children, and frames that redirect into them.
    #[test]
    fn an_admin_template_names_its_tables_and_frames_its_children() {
        let index = "import Vue from 'vue'\nimport Router from 'vue-router'\nimport Layout from '@/layout'\nimport tableRouter from './modules/table'\nVue.use(Router)\n\nexport const constantRoutes = [\n  { path: '/login', component: () => import('@/views/login/index'), hidden: true },\n  {\n    path: '/',\n    component: Layout,\n    redirect: '/dashboard',\n    children: [\n      { path: 'dashboard', component: () => import('@/views/dashboard/index'), name: 'Dashboard' }\n    ]\n  }\n]\n\nexport const asyncRoutes = [tableRouter, { path: '*', redirect: '/404', hidden: true }]\n\nconst createRouter = () => new Router({ routes: constantRoutes })\nexport default createRouter()\n";
        let entries = parse_vue_routes(index, "src/router/index.js");
        assert_eq!(
            entries
                .iter()
                .map(|entry| (
                    entry.path.as_str(),
                    entry.name.as_deref(),
                    entry.component.as_deref(),
                    entry.spec.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                ("/login", None, Some("index"), Some("@/views/login/index")),
                (
                    "/dashboard",
                    Some("Dashboard"),
                    Some("index"),
                    Some("@/views/dashboard/index")
                ),
            ]
        );
        assert_eq!(
            entries[1].layouts,
            [VueLayout {
                component: "Layout".to_string(),
                spec: None
            }]
        );
        let table = "import Layout from '@/layout'\n\nconst tableRouter = {\n  path: '/table',\n  component: Layout,\n  redirect: '/table/complex-table',\n  name: 'Table',\n  children: [\n    { path: 'complex-table', component: () => import('@/views/table/complex-table'), name: 'ComplexTable' }\n  ]\n}\nexport default tableRouter\n";
        // In a `router/` directory, a table with no import of the router.
        let entries = parse_vue_routes(table, "src/router/modules/table.js");
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.path.as_str(), entry.name.as_deref()))
                .collect::<Vec<_>>(),
            [("/table/complex-table", Some("ComplexTable"))]
        );
        assert!(parse_vue_routes(table, "src/views/table.js").is_empty());
        // A parent with children is its own page only when none claims its
        // address and it redirects nowhere.
        let pages = "import { createRouter } from 'vue-router'\nconst routes = [\n  { path: '/a', component: A, children: [{ path: 'x', component: X }] },\n  { path: '/b', component: B, children: [{ path: '', component: BIndex }] },\n]\n";
        assert_eq!(
            parse_vue_routes(pages, "")
                .iter()
                .map(|entry| (entry.path.as_str(), entry.component.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("/a/x", Some("X")),
                ("/a", Some("A")),
                ("/b", Some("BIndex"))
            ]
        );
    }

    /// `children: kids` names an array written elsewhere: the next entry's
    /// array is not its children, and that entry's child is listed once,
    /// under its own parent.
    #[test]
    fn a_children_identifier_borrows_no_later_array() {
        let table = "import { createRouter } from 'vue-router'\nconst kids = [{ path: 'k', component: K }]\nexport default createRouter({\n  routes: [\n    { path: '/a', component: A, redirect: '/a/k', children: kids },\n    { path: '/b', component: B, children: [{ path: 'c', component: C }] },\n  ],\n})\n";
        assert_eq!(
            parse_vue_routes(table, "")
                .iter()
                .map(|entry| (entry.path.as_str(), entry.component.as_deref()))
                .collect::<Vec<_>>(),
            [("/b/c", Some("C")), ("/b", Some("B"))]
        );
    }

    /// Upstream `vben`: a typed routes module whose parent names no
    /// component, and a lazy view through an alias.
    #[test]
    fn a_typed_routes_module_lists_its_children() {
        let module = "import type { RouteRecordRaw } from 'vue-router';\n\nconst routes: RouteRecordRaw[] = [\n  {\n    name: 'Dashboard',\n    path: '/dashboard',\n    children: [\n      { name: 'Analytics', path: 'analytics', component: () => import('#/views/dashboard/analytics/index.vue') },\n    ],\n  },\n];\n\nexport default routes;\n";
        let entries = parse_vue_routes(module, "apps/web/src/router/routes/modules/dashboard.ts");
        assert_eq!(
            entries
                .iter()
                .map(|entry| (
                    entry.path.as_str(),
                    entry.component.as_deref(),
                    entry.spec.as_deref()
                ))
                .collect::<Vec<_>>(),
            [(
                "/dashboard/analytics",
                Some("index"),
                Some("#/views/dashboard/analytics/index.vue")
            )]
        );
        assert!(entries[0].layouts.is_empty());
    }

    /// Upstream `vue-router: navigation call names`.
    #[test]
    fn a_navigation_call_is_a_routers() {
        for (name, verb) in [
            ("router.push", "push"),
            ("router.replace", "replace"),
            ("$router.push", "push"),
            ("this.$router.push", "push"),
            ("navigateTo", "navigateTo"),
        ] {
            assert_eq!(vue_nav_verb(name), Some(verb), "{name}");
        }
        for name in [
            "push",
            "replace",
            "paths.push",
            "list.replace",
            "go",
            "back",
        ] {
            assert_eq!(vue_nav_verb(name), None, "{name}");
        }
        assert_eq!(
            route_name_in_expression("{ name: \"login\" }").as_deref(),
            Some("login")
        );
        assert_eq!(
            route_name_in_expression("{ name: 'profile', params: { username } }").as_deref(),
            Some("profile")
        );
        assert_eq!(route_name_in_expression("{ path: \"/\", query }"), None);
        assert_eq!(route_name_in_expression("'/login'"), None);
        assert_eq!(
            parse_vue_path_object("{ path: '/', query: { tag } }").map(|href| href.path),
            Some("/".to_string())
        );
    }

    /// A context holding `files`, with the routes Vue Router extracts from
    /// each.
    fn app(files: &[(&str, &str)]) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for (path, content) in files {
            let nodes = VueRouterResolver
                .extract(
                    path,
                    content,
                    &FrameworkExtractionContext::without_config(""),
                )
                .map(|result| result.nodes)
                .unwrap_or_default();
            ctx = ctx.file(path, content).routes(path, nodes);
        }
        ctx
    }

    fn resolve(ctx: &RouteCtx, file: &str, name: &str, line: i64) -> Option<ResolvedRef> {
        let mut reference = call_in(file, name, line, 0);
        reference.language = language_for_file(file);
        VueRouterResolver.resolve(&reference, ctx)
    }

    const WEB_ROUTER: &str = "import { createRouter } from 'vue-router'\nexport default createRouter({\n  routes: [\n    { name: 'home', path: '/', component: Home },\n    { name: 'post', path: '/posts/:id', component: Post },\n    { name: 'twice', path: '/a', component: A },\n    { name: 'twice', path: '/b', component: B },\n  ],\n})\n";

    /// By name first, then by path; a name two routes carry, a name or a
    /// path nothing declares, another app's route and a computed
    /// destination stay unresolved.
    #[test]
    fn a_navigation_reaches_the_route_its_name_or_path_names() {
        let ctx = app(&[
            ("web/src/router/index.js", WEB_ROUTER),
            (
                "admin/src/router/index.js",
                "import { createRouter } from 'vue-router'\nexport default createRouter({ routes: [{ name: 'login', path: '/login', component: Login }] })\n",
            ),
            (
                "web/src/views/Nav.vue",
                "router.push({ name: 'post', params: { id: 1 } })\nrouter.push('/posts/7')\nrouter.push({ path: '/', query: { tag } })\nrouter.push({ name: 'twice' })\nrouter.push({ name: 'login' })\nrouter.push('/login')\nrouter.push(target)\n",
            ),
        ]);
        let post = resolve(&ctx, "web/src/views/Nav.vue", "router.push", 1).expect("by name");
        assert_eq!(
            post.target_node_id,
            ctx.route("/posts/:id", "web/src/router/index.js")
        );
        let edge = post.edge.expect("a navigates edge");
        assert_eq!(edge.edge_kind, Some(EdgeKind::Navigates));
        assert_eq!(edge.metadata["href"], "post");
        assert_eq!(edge.metadata["navMethod"], "push");
        assert_eq!(edge.metadata["by"], "name");
        let by_path = resolve(&ctx, "web/src/views/Nav.vue", "router.push", 2).expect("by path");
        assert_eq!(
            by_path.target_node_id,
            ctx.route("/posts/:id", "web/src/router/index.js")
        );
        let by_path = by_path.edge.expect("an edge");
        assert_eq!(by_path.metadata["href"], "/posts/7");
        assert!(by_path.metadata.get("by").is_none());
        let object = resolve(&ctx, "web/src/views/Nav.vue", "router.push", 3).expect("object");
        assert_eq!(
            object.target_node_id,
            ctx.route("/", "web/src/router/index.js")
        );
        for line in 4..=7 {
            assert!(
                resolve(&ctx, "web/src/views/Nav.vue", "router.push", line).is_none(),
                "line {line}"
            );
        }
        // An array's push is never a navigation.
        let ctx = ctx.file("web/src/trail.js", "paths.push('/')\n");
        assert!(resolve(&ctx, "web/src/trail.js", "paths.push", 1).is_none());
    }

    /// A destination is a whole literal: a concatenation's head (`'/'` of
    /// `'/' + next`), here or in the local the call names, a comment between
    /// them or not, is no address, nor is a template whose hole shares a
    /// segment. A template's holes match a
    /// parameter. An object's own `name` and `path` are read, never a nested
    /// object's.
    #[test]
    fn a_destination_is_read_whole_from_its_own_fields() {
        let ctx = app(&[
            ("src/router/index.js", WEB_ROUTER),
            (
                "src/views/Nav.vue",
                "router.push('/' + next)\nrouter.replace({ path: '/' + next })\nrouter.push({ path: `/posts/${id}` })\nrouter.push({ path: '/', query: { name: 'post' } })\nrouter.push({ name: 'home', params: { path: '/posts/1' } })\nrouter.push({ query: { path: '/posts/1' } })\nconst sum = '/' + next\nrouter.push(sum)\nconst home = '/';\nrouter.push(home)\nrouter.push(`/posts/p-${id}`)\nrouter.push(`/posts${rest}`)\nrouter.push('/' /* next */ + next)\nrouter.replace({ path: '/' // the root\n  + next })\nconst later = '/' /* then */ + next\nrouter.push(later)\nrouter.push('/' /* home */)\nrouter.push({ path: '/' /* home */, query })\n",
            ),
        ]);
        let file = "src/views/Nav.vue";
        assert!(resolve(&ctx, file, "router.push", 1).is_none());
        assert!(resolve(&ctx, file, "router.replace", 2).is_none());
        let post = ctx.route("/posts/:id", "src/router/index.js");
        let root = ctx.route("/", "src/router/index.js");
        assert_eq!(
            resolve(&ctx, file, "router.push", 3).map(|resolved| resolved.target_node_id),
            Some(post.clone())
        );
        assert_eq!(
            resolve(&ctx, file, "router.push", 4).map(|resolved| resolved.target_node_id),
            Some(root.clone())
        );
        let named = resolve(&ctx, file, "router.push", 5).expect("by its own name");
        assert_eq!(named.target_node_id, root);
        assert_eq!(named.edge.expect("an edge").metadata["by"], "name");
        assert!(resolve(&ctx, file, "router.push", 6).is_none());
        assert!(resolve(&ctx, file, "router.push", 8).is_none());
        assert_eq!(
            resolve(&ctx, file, "router.push", 10).map(|resolved| resolved.target_node_id),
            Some(root)
        );
        // A hole inside a segment is no whole segment: its value may add
        // segments of its own.
        assert!(resolve(&ctx, file, "router.push", 11).is_none());
        assert!(resolve(&ctx, file, "router.push", 12).is_none());
        // A comment does not end the value: the concatenation past it goes
        // on, in the call, the object and the local alike.
        assert!(resolve(&ctx, file, "router.push", 13).is_none());
        assert!(resolve(&ctx, file, "router.replace", 14).is_none());
        assert!(resolve(&ctx, file, "router.push", 17).is_none());
        let root = ctx.route("/", "src/router/index.js");
        for line in [18, 19] {
            assert_eq!(
                resolve(&ctx, file, "router.push", line).map(|resolved| resolved.target_node_id),
                Some(root.clone()),
                "line {line}"
            );
        }
    }

    /// Two entries that carry one path and one name are two routes, so
    /// neither answers by that name or that path; two entries on one line at
    /// one path share a route id, and that route answers neither name.
    #[test]
    fn a_repeated_entry_makes_its_name_and_path_ambiguous() {
        let ctx = app(&[
            (
                "src/router/index.js",
                "import { createRouter } from 'vue-router'\nexport default createRouter({\n  routes: [\n    { name: 'same', path: '/same', component: A },\n    { name: 'same', path: '/same', component: B },\n    { name: 'x', path: '/one', component: X }, { name: 'y', path: '/one', component: Y },\n    { name: 'z', path: '/z', component: Z },\n  ],\n})\n",
            ),
            (
                "src/views/Nav.vue",
                "router.push({ name: 'same' })\nrouter.push('/same')\nrouter.push({ name: 'x' })\nrouter.push({ name: 'y' })\nrouter.push('/one')\nrouter.push({ name: 'z' })\n",
            ),
        ]);
        assert_eq!(
            ctx.nodes()
                .iter()
                .filter(|node| node.name == "/same")
                .count(),
            2
        );
        for line in 1..=5 {
            assert!(
                resolve(&ctx, "src/views/Nav.vue", "router.push", line).is_none(),
                "line {line}"
            );
        }
        assert!(resolve(&ctx, "src/views/Nav.vue", "router.push", 6).is_some());
    }

    /// An identifier's component is what its import takes: a named import's
    /// export (not the module's first component-like function), through
    /// the modules that re-export it, a default import's module default. An
    /// import whose module is not found, or holds no such component, is
    /// none, never a same-named component elsewhere; only a name no import
    /// reaches is looked up by name. A lazy import's module likewise.
    #[test]
    fn a_route_component_is_the_export_its_import_takes() {
        let node = |file: &str, kind: NodeKind, name: &str, line: i64| {
            framework_node(
                format!("{}:{file}:{name}", kind.as_str()),
                kind,
                name.to_string(),
                format!("{file}::{name}"),
                file.to_string(),
                line,
                line,
                0,
                0,
                language_for_file(file),
                false,
            )
        };
        // Where the extractor starts it: a function at `function`, an
        // arrow-held one at its arrow, a constant at its name.
        let node_at = |file: &str, kind: NodeKind, name: &str, line: i64, column: i64| Node {
            start_column: column,
            ..node(file, kind, name, line)
        };
        let import = |local: &str, exported: &str, source: &str, is_default: bool| ImportMapping {
            local_name: local.to_string(),
            exported_name: exported.to_string(),
            source: source.to_string(),
            is_default,
            is_namespace: false,
        };
        let router = "src/router/index.ts";
        let layouts = "src/layouts/index.ts";
        let multi = "src/layouts/multi.ts";
        let multi_source = "export const Other = 1, Second = defineComponent({})\nconst First = 1,\n  Listed = () => 3\nexport { Listed }\nexport default function Main() {}\n";
        // A string of characters wider than a byte before the node: the
        // extractor's column counts bytes.
        let wide = "src/layouts/wide.ts";
        let wide_source =
            "const s = '\u{1F600}\u{1F600}\u{1F600}'; export const Wide = () => { return 1 }\n";
        // The byte column of `needle` on 1-based `line`.
        let column = |source: &str, line: usize, needle: &str| -> i64 {
            let text = source.split('\n').nth(line - 1).expect("the line");
            i64::try_from(text.find(needle).expect("the needle")).expect("a column")
        };
        let ctx = RouteCtx::default()
            .file(
                layouts,
                "const BasicLayout = () => import('./basic.vue');\nconst IFrameView = () => import('@vben/layouts').then((m) => m.IFrameView);\nfunction Real() {}\nexport { BasicLayout, IFrameView };\nexport { default as FrameLayout } from './frame.vue';\nexport { Real as Aliased };\nexport * from './more';\n",
            )
            .routes(
                layouts,
                vec![
                    node(layouts, NodeKind::Function, "BasicLayout", 1),
                    node(layouts, NodeKind::Function, "IFrameView", 2),
                    node(layouts, NodeKind::Function, "Real", 3),
                ],
            )
            .file("src/layouts/frame.vue", "<template><iframe /></template>\n")
            .routes(
                "src/layouts/frame.vue",
                vec![node("src/layouts/frame.vue", NodeKind::Component, "frame", 1)],
            )
            .file(
                "src/layouts/anon.ts",
                "export default function () {\n  function Hidden2() {}\n  return Hidden2\n}\n",
            )
            .routes(
                "src/layouts/anon.ts",
                // As the extractor gives it: no outer node, so qualified as
                // the module's own, and marked exported.
                vec![Node {
                    is_exported: true,
                    qualified_name: "Hidden2".to_string(),
                    ..node_at("src/layouts/anon.ts", NodeKind::Function, "Hidden2", 2, 2)
                }],
            )
            .file("src/layouts/more.ts", "export function MoreLayout() {}\n")
            .routes(
                "src/layouts/more.ts",
                vec![Node {
                    is_exported: true,
                    ..node_at("src/layouts/more.ts", NodeKind::Function, "MoreLayout", 1, 7)
                }],
            )
            .file(
                "src/layouts/private.ts",
                "function Hidden() {}\nfunction Shown() {}\nexport { Shown as Swapped };\nfunction Swapped() {}\nexport function Outer() {\n  function Nested() {}\n  return Nested\n}\nexport default Nested\n",
            )
            .routes(
                "src/layouts/private.ts",
                vec![
                    node("src/layouts/private.ts", NodeKind::Function, "Hidden", 1),
                    node("src/layouts/private.ts", NodeKind::Function, "Shown", 2),
                    node("src/layouts/private.ts", NodeKind::Function, "Swapped", 4),
                    Node {
                        is_exported: true,
                        qualified_name: "Outer".to_string(),
                        ..node_at("src/layouts/private.ts", NodeKind::Function, "Outer", 5, 7)
                    },
                    // Marked exported as the extractor marks a declaration
                    // inside an exported function.
                    Node {
                        is_exported: true,
                        qualified_name: "Outer::Nested".to_string(),
                        ..node_at("src/layouts/private.ts", NodeKind::Function, "Nested", 6, 2)
                    },
                ],
            )
            .file(
                "src/views/Home.ts",
                "function helper() {}\nexport default function HomeView() {}\n",
            )
            .routes(
                "src/views/Home.ts",
                vec![
                    node("src/views/Home.ts", NodeKind::Function, "Helper", 1),
                    Node {
                        is_exported: true,
                        ..node_at("src/views/Home.ts", NodeKind::Function, "HomeView", 2, 15)
                    },
                ],
            )
            .file(multi, multi_source)
            .routes(
                multi,
                vec![
                    Node {
                        is_exported: true,
                        ..node_at(multi, NodeKind::Constant, "Other", 1, column(multi_source, 1, "Other"))
                    },
                    Node {
                        is_exported: true,
                        ..node_at(multi, NodeKind::Constant, "Second", 1, column(multi_source, 1, "Second"))
                    },
                    node_at(multi, NodeKind::Constant, "First", 2, column(multi_source, 2, "First")),
                    node_at(multi, NodeKind::Function, "Listed", 3, column(multi_source, 3, "() =>")),
                    Node {
                        is_exported: true,
                        ..node_at(multi, NodeKind::Function, "Main", 5, column(multi_source, 5, "function"))
                    },
                ],
            )
            .file(wide, wide_source)
            .routes(
                wide,
                vec![Node {
                    is_exported: true,
                    ..node_at(wide, NodeKind::Function, "Wide", 1, column(wide_source, 1, "() =>"))
                }],
            )
            .file(
                "src/views/Sfc.vue",
                "<template><p>Hi</p></template>\n<script lang=\"ts\">\nexport const Named = () => 1\n</script>\n",
            )
            .routes(
                "src/views/Sfc.vue",
                vec![
                    node("src/views/Sfc.vue", NodeKind::Component, "Sfc", 1),
                    Node {
                        is_exported: true,
                        ..node_at("src/views/Sfc.vue", NodeKind::Function, "Named", 3, 21)
                    },
                ],
            )
            .routes(
                "src/views/Missing.vue",
                vec![node("src/views/Missing.vue", NodeKind::Component, "Missing", 1)],
            )
            .routes(
                "src/views/Lost.vue",
                vec![node("src/views/Lost.vue", NodeKind::Component, "Lost", 1)],
            )
            .routes(
                "src/views/Profile.vue",
                vec![node("src/views/Profile.vue", NodeKind::Component, "Profile", 1)],
            )
            .routes(
                "src/legacy/views/Ghost.vue",
                vec![node("src/legacy/views/Ghost.vue", NodeKind::Component, "Ghost", 1)],
            )
            .file(router, "const Inline = defineComponent({})\n")
            .routes(router, vec![node(router, NodeKind::Constant, "Inline", 1)])
            .import(router, import("IFrameView", "IFrameView", "@/layouts", false))
            .import(router, import("FrameLayout", "FrameLayout", "@/layouts", false))
            .import(router, import("Aliased", "Aliased", "@/layouts", false))
            .import(router, import("MoreLayout", "MoreLayout", "@/layouts", false))
            .import(router, import("Home", "default", "@/views/Home", true))
            .import(router, import("Gone", "Gone", "@/layouts", false))
            .import(router, import("Missing", "Missing", "@/layouts", false))
            .import(router, import("Lost", "Lost", "#/missing-module", false))
            .import(router, import("Hidden", "Hidden", "@/layouts/private", false))
            .import(router, import("Swapped", "Swapped", "@/layouts/private", false))
            .import(router, import("Nested", "Nested", "@/layouts/private", false))
            .import(router, import("Hidden2", "Hidden2", "@/layouts/anon", false))
            .import(router, import("PrivateDefault", "default", "@/layouts/private", true))
            .import(router, import("Second", "Second", "@/layouts/multi", false))
            .import(router, import("Listed", "Listed", "@/layouts/multi", false))
            .import(router, import("Main", "Main", "@/layouts/multi", false))
            .import(router, import("MultiDefault", "default", "@/layouts/multi", true))
            .import(router, import("Wide", "Wide", "@/layouts/wide", false))
            .import(router, import("Named", "Named", "@/views/Sfc.vue", false))
            .import(
                router,
                ImportMapping {
                    is_namespace: true,
                    ..import("Layouts", "*", "@/layouts", false)
                },
            );
        let route = |name: &str| RefView {
            from_node_id: format!("route:{router}:3:/x:vue"),
            language: Language::TypeScript,
            ..call_in(router, name, 3, 0)
        };
        let target = |name: &str| {
            VueRouterResolver
                .resolve(&route(name), &ctx)
                .map(|r| r.target_node_id)
        };
        assert_eq!(
            target("component:IFrameView").as_deref(),
            Some("function:src/layouts/index.ts:IFrameView")
        );
        assert_eq!(
            target("component:Home").as_deref(),
            Some("function:src/views/Home.ts:HomeView")
        );
        // Through `export { default as FrameLayout } from './frame.vue'`,
        // `export { Real as Aliased }` and `export * from './more'`.
        assert_eq!(
            target("component:FrameLayout").as_deref(),
            Some("component:src/layouts/frame.vue:frame")
        );
        assert_eq!(
            target("component:Aliased").as_deref(),
            Some("function:src/layouts/index.ts:Real")
        );
        assert_eq!(
            target("component:MoreLayout").as_deref(),
            Some("function:src/layouts/more.ts:MoreLayout")
        );
        // The module is found and declares no `Gone`; `Missing` is a
        // component elsewhere, which the named import does not reach; `Lost`
        // names a module nothing finds, and `Layouts` is a namespace.
        assert_eq!(target("component:Gone"), None);
        assert_eq!(target("component:Missing"), None);
        assert_eq!(target("component:Lost"), None);
        assert_eq!(target("component:Layouts"), None);
        // A named import takes what the module exports: not a private
        // declaration of the name, but what an export list names so.
        assert_eq!(target("component:Hidden"), None);
        assert_eq!(
            target("component:Swapped").as_deref(),
            Some("function:src/layouts/private.ts:Shown")
        );
        // A function declared inside an exported one is no export of the
        // module, by name or as the binding `export default` names.
        assert_eq!(target("component:Nested"), None);
        assert_eq!(target("component:PrivateDefault"), None);
        // Nor one inside an anonymous `export default function () {…}`.
        assert_eq!(target("component:Hidden2"), None);
        // The second declarator of a statement, exported there or through
        // an export list; a named import never takes what `export default`
        // declares, which a default import does.
        assert_eq!(
            target("component:Second").as_deref(),
            Some("constant:src/layouts/multi.ts:Second")
        );
        assert_eq!(
            target("component:Listed").as_deref(),
            Some("function:src/layouts/multi.ts:Listed")
        );
        assert_eq!(target("component:Main"), None);
        assert_eq!(
            target("component:MultiDefault").as_deref(),
            Some("function:src/layouts/multi.ts:Main")
        );
        assert_eq!(
            target("component:Wide").as_deref(),
            Some("function:src/layouts/wide.ts:Wide")
        );
        // A component file's template is no script: its own exports are
        // not read, though this one's would read as a script's.
        assert_eq!(target("component:Named"), None);
        // No import: the router file's own declaration, else a component by
        // name, never an `index`.
        assert_eq!(
            target("component:Inline").as_deref(),
            Some("constant:src/router/index.ts:Inline")
        );
        assert_eq!(
            target("component:Profile").as_deref(),
            Some("component:src/views/Profile.vue:Profile")
        );
        assert_eq!(target("component:index"), None);
        // A lazy module that is no SFC renders its default export; a lazy
        // module nothing finds renders nothing, though `Lost.vue` exists.
        assert_eq!(
            target("import:@/views/Home#Home").as_deref(),
            Some("function:src/views/Home.ts:HomeView")
        );
        assert_eq!(target("import:@/views/nowhere/Lost.vue#Lost"), None);
        // A relative specifier is the file it names: `../views/Ghost.vue`
        // is not `src/legacy/views/Ghost.vue`, whose path ends the same way.
        assert_eq!(target("import:../views/Ghost.vue#Ghost"), None);
        // The references are this resolver's alone.
        assert!(VueRouterResolver.claims_reference("component:Lost"));
    }

    /// A Nuxt page is in its app's table by path: `navigateTo` reaches it.
    #[test]
    fn navigate_to_reaches_a_nuxt_page() {
        let page = framework_node(
            "route:pages/users/[id].vue:/users/:id:1".to_string(),
            NodeKind::Route,
            "/users/:id".to_string(),
            "pages/users/[id].vue::route:/users/:id".to_string(),
            "pages/users/[id].vue".to_string(),
            1,
            1,
            0,
            0,
            Language::Vue,
            false,
        );
        let ctx = RouteCtx::default()
            .routes("pages/users/[id].vue", vec![page])
            .file(
                "components/Go.vue",
                "navigateTo(`/users/${id}`)\nnavigateTo('/nowhere')\n",
            );
        let user = resolve(&ctx, "components/Go.vue", "navigateTo", 1).expect("the page");
        assert_eq!(
            user.target_node_id,
            ctx.route("/users/:id", "pages/users/[id].vue")
        );
        assert_eq!(
            user.edge.expect("an edge").metadata["navMethod"],
            "navigateTo"
        );
        assert!(resolve(&ctx, "components/Go.vue", "navigateTo", 2).is_none());
        // Another router's route at the same path is none of Vue's.
        let mut other = framework_node(
            "route:src/App.tsx:3:/settings".to_string(),
            NodeKind::Route,
            "/settings".to_string(),
            "src/App.tsx::route:/settings".to_string(),
            "src/App.tsx".to_string(),
            3,
            3,
            0,
            0,
            Language::Tsx,
            false,
        );
        other.language = Language::Tsx;
        let ctx = RouteCtx::default()
            .routes("src/App.tsx", vec![other])
            .file("src/go.js", "router.push('/settings')\n");
        assert!(resolve(&ctx, "src/go.js", "router.push", 1).is_none());
    }

    /// A route's component: a lazy view by the file its import names (an
    /// alias read by the path's suffix), an identifier by name; an `index`
    /// with no file to read is none.
    #[test]
    fn a_route_calls_the_view_its_entry_names() {
        let component = |file: &str, name: &str| {
            framework_node(
                format!("component:{file}"),
                NodeKind::Component,
                name.to_string(),
                format!("{file}::{name}"),
                file.to_string(),
                1,
                1,
                0,
                0,
                Language::Vue,
                false,
            )
        };
        let ctx = RouteCtx::default()
            .routes(
                "apps/web/src/views/dashboard/analytics/index.vue",
                vec![component(
                    "apps/web/src/views/dashboard/analytics/index.vue",
                    "index",
                )],
            )
            .routes(
                "apps/web/src/views/Profile.vue",
                vec![component("apps/web/src/views/Profile.vue", "Profile")],
            );
        let route = |name: &str| RefView {
            from_node_id:
                "route:apps/web/src/router/routes/modules/dashboard.ts:8:/dashboard/analytics:vue"
                    .to_string(),
            language: Language::TypeScript,
            ..call_in(
                "apps/web/src/router/routes/modules/dashboard.ts",
                name,
                8,
                0,
            )
        };
        let lazy = VueRouterResolver
            .resolve(
                &route("import:#/views/dashboard/analytics/index.vue#index"),
                &ctx,
            )
            .expect("the lazy view");
        assert_eq!(
            lazy.target_node_id,
            "component:apps/web/src/views/dashboard/analytics/index.vue"
        );
        assert_eq!(lazy.confidence, 0.95);
        assert_eq!(
            VueRouterResolver
                .resolve(&route("component:Profile"), &ctx)
                .map(|resolved| resolved.target_node_id),
            Some("component:apps/web/src/views/Profile.vue".to_string())
        );
        // What `extract` names them: a lazy view by its import, an
        // identifier as this resolver's own, a layout behind `layout:`.
        let extracted = VueRouterResolver
            .extract(
                "src/router/index.js",
                ROUTER_SOURCE,
                &FrameworkExtractionContext::without_config(""),
            )
            .expect("routes");
        let names: Vec<&str> = extracted
            .references
            .iter()
            .map(|reference| reference.reference_name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "import:@/views/Home#Home",
                "import:@/views/Login#Login",
                "import:@/views/Settings#Settings",
                "component:Favorites",
                "layout:Profile",
                "component:Profile"
            ]
        );
        assert!(
            VueRouterResolver
                .resolve(&route("import:#/views/missing/index#index"), &ctx)
                .is_none()
        );
        // `index` fits every folder's view: never looked up by name.
        assert!(
            VueRouterResolver
                .resolve(&route("component:index"), &ctx)
                .is_none()
        );
        // A layout reference binds with `layout: true`.
        let layout = RefView {
            reference_kind: EdgeKind::References,
            ..route("layout:Profile")
        };
        let resolved = VueRouterResolver
            .resolve(&layout, &ctx)
            .expect("the layout");
        assert_eq!(
            resolved.edge.expect("an edge").metadata["layout"],
            serde_json::Value::Bool(true)
        );
    }

    /// A sync finds the navigation calls a changed route may answer in its
    /// own app's files, and none for another router's route.
    #[test]
    fn a_route_is_answered_by_the_navigation_calls_of_its_app() {
        let ctx = app(&[("web/src/router/index.js", WEB_ROUTER)]);
        let home = ctx
            .nodes()
            .iter()
            .find(|node| node.name == "/")
            .cloned()
            .expect("the home route");
        assert_eq!(
            VueNavigation.scope(&home, &ctx),
            Some(vec!["web/".to_string()])
        );
        let mut other = home.clone();
        other.id = "route:web/src/router/index.js:4:/".to_string();
        assert_eq!(VueNavigation.scope(&other, &ctx), None);
        assert_eq!(VueNavigation.tails(), ["push", "replace", "navigateTo"]);
    }
}
