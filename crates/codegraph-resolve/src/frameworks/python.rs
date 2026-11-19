//! Django, Flask and FastAPI (upstream `resolution/frameworks/python.ts` at
//! `e8ddc4e2`: #2154, #2225, and the FastAPI halves of `bc45e907` and
//! `950686de`).
//!
//! Django's URLconf (`path`, `re_path`, `url`) and DRF router registrations,
//! Flask's `@x.route` decorators, Flask-RESTful resources and `add_url_rule`
//! calls (or a project's wrapper around it), and FastAPI's `@x.get(…)`-style
//! decorators are route nodes, each with a reference to what it serves. A
//! FastAPI route is renamed to the path a request takes through
//! `APIRouter(prefix=…)` and `include_router(…, prefix=…)`
//! ([`FastapiResolver::post_extract`]). Each resolver resolves a few name
//! shapes (`…View`, `…_bp`, `…_router`) through the shared name heuristic
//! ([`super::name_heuristic`]).
//!
//! Offsets are byte offsets. Comments, docstrings and other triple-quoted
//! strings are blanked byte for byte
//! ([`crate::strip_comments::strip_python_comments_keeping_offsets`]), so an
//! offset into the stripped text indexes the source too; whatever upstream
//! measures in UTF-16 code units (a column, a window, a captured string) is
//! measured on the source at the same offsets ([`Text`]).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::{Captures, Regex};

use super::express::qualified_route;
use super::js_scan::{JS_SPACE, js_trim};
use super::name_heuristic::{PickOptions, pick_by_name_and_kind};
use super::offset_text::OffsetText as Text;
use super::{framework_node, now_millis};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::import_resolver::resolve_import_path;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The language the three resolvers read (upstream `languages`).
const LANGUAGES: [Language; 1] = [Language::Python];

/// The root files detection reads by path, indexed or not: the inputs of the
/// sync's `python` control kind (`codegraph_watch::control_inputs`), since
/// `requirements.txt`, `pyproject.toml` and `Pipfile` are never indexed and a
/// configuration may keep the scripts out of the index.
pub const ROOT_DETECTION_FILES: [&str; 8] = [
    "requirements.txt",
    "setup.py",
    "pyproject.toml",
    "Pipfile",
    "manage.py",
    "app.py",
    "main.py",
    "api.py",
];

/// The folders a heuristic prefers, as path substrings (upstream's
/// `includes`).
const MODEL_DIRS: [&str; 3] = ["models", "app/models", "src/models"];
const VIEW_DIRS: [&str; 4] = ["views", "app/views", "src/views", "api/views"];
const FORM_DIRS: [&str; 3] = ["forms", "app/forms", "src/forms"];
const ROUTER_DIRS: [&str; 4] = ["/routers/", "/api/", "/routes/", "/endpoints/"];
const DEP_DIRS: [&str; 3] = ["/dependencies/", "/deps/", "/core/"];

const CLASS_KINDS: [NodeKind; 1] = [NodeKind::Class];
const VIEW_KINDS: [NodeKind; 2] = [NodeKind::Class, NodeKind::Function];
const VARIABLE_KINDS: [NodeKind; 1] = [NodeKind::Variable];
const FUNCTION_KINDS: [NodeKind; 1] = [NodeKind::Function];

/// The methods a FastAPI route's name starts with: its decorator's,
/// upper-cased.
const FASTAPI_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"];

/// Prefix-composition rounds at most (upstream's `round < 8`).
const PREFIX_ROUNDS: usize = 8;

/// How far [`balanced_args`] looks for the closing paren, in UTF-16 code
/// units (upstream's `open + 4000`).
const ARGS_WINDOW: usize = 4000;

/// The Flask entry scripts detection reads at most (upstream's
/// `slice(0, 50)`).
const FLASK_ENTRYPOINTS: usize = 50;

/// The files FastAPI's subdirectory detection reads at most (upstream's
/// `looked >= 40`).
const FASTAPI_LOOKS: usize = 40;

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), and its `\b` is written `(?-u:\b)`.
fn py_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("python framework pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| py_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// `path('url', handler)`, `re_path(r'…', handler)`, `url(r'…', handler)`;
    /// the handler may carry one paren pair (`View.as_view()`,
    /// `include('x.y')`).
    django_route = r#"(?-u:\b)(path|re_path|url){S}*\({S}*r?['"]([^'"]+)['"]{S}*,{S}*([{W}.]+(?:{S}*\([^)]*\))?)"#;
    /// `include('module.path')`.
    django_include = r#"^include{S}*\({S}*['"]([^'"]+)['"]"#;
    /// A trailing `.as_view(…)`.
    as_view = r"\.as_view{S}*\([^)]*\){S}*$";
    /// Any other trailing method call.
    trailing_call = r"\.[{W}]+{S}*\([^)]*\){S}*$";
    /// DRF's `router.register(r'articles', ArticleViewSet)`.
    drf_register = r#"\.register{S}*\({S}*r?['"]([^'"]+)['"]{S}*,{S}*([{W}.]+)"#;
    /// Flask's `@x.route('/path', methods=[…] | (…))`.
    flask_route = r#"@([{W}]+)\.route{S}*\({S}*['"]([^'"]*)['"](?:{S}*,{S}*methods{S}*={S}*[\[(]([^\])]+)[\])])?{S}*\)"#;
    /// The first method a decorator's `methods=` lists (upstream's `/i`,
    /// which folds only ASCII letters).
    flask_method = r#"['"]([A-Za-z]+)['"]"#;
    /// The handler a decorator decorates: the next `def`, past other
    /// decorators.
    handler_def = r"\n{S}*(?:async{S}+)?def{S}+([{W}]+)";
    /// Flask-RESTful's `api.add_resource(Resource, '/path'[, '/path2'])`, and
    /// variants like redash's `add_org_resource`.
    restful = r#"\.add[{W}]*[Rr]esource{S}*\({S}*([{W}]+){S}*,{S}*((?:['"][^'"]+['"]{S}*,?{S}*)+)"#;
    /// One of a resource's quoted paths, with its quotes.
    quoted_path = r#"['"][^'"]+['"]"#;
    /// A call's name and its `(`: `.add_url_rule`, or any other name.
    rule_opener = r"(\.add_url_rule|(?-u:\b)[A-Za-z_][{W}]*){S}*\(";
    /// What makes a call a URL rule: a `view_func=`, or `add_url_rule`.
    rule_check = r"(?-u:\b)view_func{S}*=|\.add_url_rule";
    /// `add_url_rule`'s first argument, a string, perhaps `rule=`
    /// (upstream's `(['"])([^'"]*)\1`, the quote alternation spelled out).
    rule_first = r#"^{S}*(?:rule{S}*={S}*)?(?:"([^'"]*)"|'([^'"]*)')"#;
    /// A wrapper's list of path strings (`routes=["/a", "/b"]`).
    rule_list = r#"(?:^|,){S}*(?:routes|rules|urls|paths)?{S}*=?{S}*\[((?:{S}*(?:"[^'"]*"|'[^'"]*'){S}*,?)+)\]"#;
    /// One string of such a list, its text in group 1 or 2.
    list_item = r#""([^'"]*)"|'([^'"]*)'"#;
    /// `view_func=X` or `view_func=X.as_view`.
    view_func = r"(?-u:\b)view_func{S}*={S}*([A-Za-z_][{W}.]*)";
    /// `add_url_rule`'s third positional argument, after a rule and an
    /// endpoint name or `None`.
    rule_positional = r#"^{S}*(?:"[^'"]*"|'[^'"]*'){S}*,{S}*(?:"[^'"]*"|'[^'"]*'|None){S}*,{S}*([A-Za-z_][{W}.]*)"#;
    /// A `methods=` list or tuple, perhaps empty.
    methods_listed = r"(?-u:\b)methods{S}*={S}*[\[(]([^\])]*)[\])]";
    /// One quoted method of such a list.
    method_quoted = r#"['"]([{W}]+)['"]"#;
    /// FastAPI's `@x.get('/path')`; the path may be empty.
    fastapi_route = r#"@([{W}]+)\.(get|post|put|patch|delete|options|head){S}*\({S}*['"]([^'"]*)['"]"#;
    /// `router = APIRouter(…)`.
    router_decl = r"(?-u:\b)([A-Za-z_][{W}]*){S}*={S}*APIRouter{S}*\(([^)]*)\)";
    /// A literal `prefix="…"`.
    prefix_literal = r#"(?-u:\b)prefix{S}*={S}*['"]([^'"]*)['"]"#;
    /// Any `prefix=`.
    prefix_any = r"(?-u:\b)prefix{S}*=";
    /// The router a FastAPI decorator registers on.
    router_decorator = r"@([A-Za-z_][{W}]*)\.(?:get|post|put|patch|delete|options|head){S}*\(";
    /// `X.include_router(router[, …])`.
    include_router = r"(?-u:\b)([A-Za-z_][{W}]*)\.include_router{S}*\({S}*([A-Za-z_][{W}.]*){S}*((?:,[^)]*)?)\)";
    /// `flask` as a word, in any ASCII case.
    flask_word = r"(?i-u:\bflask\b)";
    /// `fastapi` as a word, in any ASCII case.
    fastapi_word = r"(?i-u:\bfastapi\b)";
    /// A Flask app's entry script, at the root or below.
    flask_entry = r"(?:^|/)(app|application|main|wsgi|__init__)\.py$";
    /// `Flask(`.
    flask_call = r"(?-u:\b)Flask{S}*\(";
    /// `import flask` or `from flask`.
    flask_import = r"(?-u:\b)import{S}+flask(?-u:\b)|(?-u:\b)from{S}+flask(?-u:\b)";
}

pub struct DjangoResolver;

impl FrameworkResolver for DjangoResolver {
    fn name(&self) -> &str {
        "django"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// `django` in a root `requirements.txt`, `setup.py` or `pyproject.toml`,
    /// or a root `manage.py`.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        ["requirements.txt", "setup.py", "pyproject.toml"]
            .iter()
            .any(|file| {
                context
                    .read_file_shared(file)
                    .is_some_and(|content| content.to_lowercase().contains("django"))
            })
            || context.file_exists("manage.py")
    }

    /// A model (`…Model`, or one capitalized word), a view (`…View`,
    /// `…ViewSet`) or a form (`…Form`), preferring Django's folders.
    /// Upstream's ORM dispatch `_iterable_class` → `ModelIterable.__iter__`
    /// is not ported (KNOWN_DIFFS).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        if (name.ends_with("Model") || is_capitalized_word(name))
            && let Some(resolved) = heuristic(reference, &CLASS_KINDS, &MODEL_DIRS, 0.8, context)
        {
            return Some(resolved);
        }
        if (name.ends_with("View") || name.ends_with("ViewSet"))
            && let Some(resolved) = heuristic(reference, &VIEW_KINDS, &VIEW_DIRS, 0.8, context)
        {
            return Some(resolved);
        }
        if name.ends_with("Form") {
            return heuristic(reference, &CLASS_KINDS, &FORM_DIRS, 0.8, context);
        }
        None
    }

    /// An `include('app.urls')` module name, which no node is named, past
    /// the name pre-filter: import resolution maps it to its `urls.py`.
    fn claims_reference(&self, name: &str) -> bool {
        name.ends_with(".urls")
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(extract_django(file_path, content))
    }
}

pub struct FlaskResolver;

impl FrameworkResolver for FlaskResolver {
    fn name(&self) -> &str {
        "flask"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// `flask` in a root manifest, or an entry script (root or below; the
    /// first 50) that imports flask and calls `Flask(`.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if ["requirements.txt", "pyproject.toml", "Pipfile", "setup.py"]
            .iter()
            .any(|file| {
                context
                    .read_file_shared(file)
                    .is_some_and(|content| flask_word().is_match(&content))
            })
        {
            return true;
        }
        context
            .get_all_files_shared()
            .iter()
            .filter(|file| flask_entry().is_match(file))
            .take(FLASK_ENTRYPOINTS)
            .any(|file| {
                context.read_file_shared(file).is_some_and(|content| {
                    flask_call().is_match(&content) && flask_import().is_match(&content)
                })
            })
    }

    /// A blueprint (`…_bp`, `…_blueprint`).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        if name.ends_with("_bp") || name.ends_with("_blueprint") {
            return heuristic(reference, &VARIABLE_KINDS, &[], 0.8, context);
        }
        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(extract_flask(file_path, content))
    }
}

pub struct FastapiResolver;

impl FrameworkResolver for FastapiResolver {
    fn name(&self) -> &str {
        "fastapi"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// `fastapi` in a root `requirements.txt` or `pyproject.toml`, `FastAPI(`
    /// in a root `app.py`, `main.py` or `api.py`, or either below the root
    /// (`950686de`): a manifest anywhere, or a script at most four path
    /// segments deep, reading at most 40 of them in the file list's order.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let fastapi_manifest = |file: &str| {
            context
                .read_file_shared(file)
                .is_some_and(|content| fastapi_word().is_match(&content))
        };
        let fastapi_script = |file: &str| {
            context
                .read_file_shared(file)
                .is_some_and(|content| content.contains("FastAPI("))
        };
        if ["requirements.txt", "pyproject.toml"]
            .iter()
            .any(|file| fastapi_manifest(file))
            || ["app.py", "main.py", "api.py"]
                .iter()
                .any(|file| fastapi_script(file))
        {
            return true;
        }
        let mut looked = 0;
        for file in context.get_all_files_shared().iter() {
            let normalized = file.replace('\\', "/");
            let base = &normalized[normalized.rfind('/').map_or(0, |at| at + 1)..];
            let found = match base {
                "requirements.txt" | "pyproject.toml" | "requirements-dev.txt" => {
                    fastapi_manifest(file)
                }
                "main.py" | "app.py" | "api.py" if normalized.split('/').count() <= 4 => {
                    fastapi_script(file)
                }
                _ => continue,
            };
            if found {
                return true;
            }
            looked += 1;
            if looked >= FASTAPI_LOOKS {
                break;
            }
        }
        false
    }

    /// A router (`…_router`, `router`), or a dependency (`get_…`,
    /// `Depends…`).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        if (name.ends_with("_router") || name == "router")
            && let Some(resolved) =
                heuristic(reference, &VARIABLE_KINDS, &ROUTER_DIRS, 0.8, context)
        {
            return Some(resolved);
        }
        if name.starts_with("get_") || name.starts_with("Depends") {
            return heuristic(reference, &FUNCTION_KINDS, &DEP_DIRS, 0.75, context);
        }
        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(extract_fastapi(file_path, content))
    }

    /// Router prefixes (upstream `postExtract`, `bc45e907`). A router's
    /// routes are written relative to where it is mounted —
    ///
    /// ```text
    /// router = APIRouter(prefix="/items")                 # items.py
    /// api_router.include_router(items.router)             # api.py
    /// app.include_router(api_router, prefix="/api/v1")    # main.py
    /// @router.get("/{id}")                                # → GET /api/v1/items/{id}
    /// ```
    ///
    /// — so every `APIRouter(prefix=…)` and every literal
    /// `X.include_router(router, prefix=…)` (a computed prefix is unknown,
    /// and that mount is left alone) composes down the mount tree, and the
    /// routes decorated with each router are renamed to the path a request
    /// takes. A router mounted at two different paths is left alone. A route
    /// keeps its `id` and `qualified_name`, which still reads the in-file
    /// `METHOD:path` every name is computed from.
    ///
    /// Only FastAPI's own routes are renamed (a Flask route's id has the same
    /// shape), and a route an earlier pass renamed that nothing renames now
    /// gets its in-file name back, so a sync converges with a fresh index.
    /// Upstream renames any route on a decorator's line and never resets one.
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let sources: Vec<String> = context
            .get_all_files_shared()
            .iter()
            .filter(|file| file.ends_with(".py"))
            .cloned()
            .collect();
        // file → router variable → its own `APIRouter(prefix=)`.
        let mut own: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        // mounting file → its mounts.
        let mut mounts: BTreeMap<String, Vec<Mount>> = BTreeMap::new();
        // file → decorator line → the router it registers on.
        let mut receivers: BTreeMap<String, BTreeMap<i64, String>> = BTreeMap::new();
        for file in context.files_containing(&sources, &["APIRouter", "include_router"]) {
            let Some(content) = context.read_file_shared(&file) else {
                continue;
            };
            let text = Text::python(&content);
            let mut vars: BTreeMap<String, String> = BTreeMap::new();
            for captures in router_decl().captures_iter(&text.safe) {
                let args = captures.get(2).expect("router args");
                let prefix = prefix_literal()
                    .captures(args.as_str())
                    .map(|literal| {
                        let found = literal.get(1).expect("prefix");
                        text.upstream(args.start() + found.start(), args.start() + found.end())
                    })
                    .unwrap_or_default();
                vars.insert(captures[1].to_string(), prefix);
            }
            let mut by_line: BTreeMap<i64, String> = BTreeMap::new();
            for captures in router_decorator().captures_iter(&text.safe) {
                let at = captures.get(0).expect("decorator").start();
                by_line.insert(text.line_at(at), captures[1].to_string());
            }
            for captures in include_router().captures_iter(&text.safe) {
                let rest = captures.get(3).expect("include rest");
                let literal = prefix_literal().captures(rest.as_str()).map(|literal| {
                    let found = literal.get(1).expect("prefix");
                    text.upstream(rest.start() + found.start(), rest.start() + found.end())
                });
                if literal.is_none() && prefix_any().is_match(rest.as_str()) {
                    continue;
                }
                let Some(target) = included_router(&captures[2], &file, &vars, context) else {
                    continue;
                };
                mounts.entry(file.clone()).or_default().push(Mount {
                    from_var: captures[1].to_string(),
                    prefix: literal.unwrap_or_default(),
                    target,
                });
            }
            own.insert(file.clone(), vars);
            receivers.insert(file, by_line);
        }

        let base = compose_bases(&own, &mounts);

        let mut updates: Vec<Node> = Vec::new();
        let mut owned_cache: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut renamed: BTreeSet<String> = BTreeSet::new();
        // Upstream returns here when there is no mount and no router has a
        // prefix of its own; then nothing below renames a route, and the
        // stale ones still get their names back.
        let renames = !mounts.is_empty()
            || own
                .values()
                .any(|vars| vars.values().any(|prefix| !prefix.is_empty()));
        if renames {
            for (file, by_line) in &receivers {
                let vars = own.get(file);
                for route in context.get_nodes_in_file_shared(file) {
                    if route.kind != NodeKind::Route {
                        continue;
                    }
                    let Some(variable) = by_line.get(&route.start_line) else {
                        continue;
                    };
                    let prefix = join_py_paths(
                        base.get(&(file.clone(), variable.clone()))
                            .map_or("", String::as_str),
                        vars.and_then(|vars| vars.get(variable))
                            .map_or("", String::as_str),
                    );
                    if prefix.is_empty() || prefix == "/" {
                        continue;
                    }
                    let Some((method, original)) = qualified_route(&route) else {
                        continue;
                    };
                    if !owned_routes(&mut owned_cache, file, context).contains(&route.id) {
                        continue;
                    }
                    renamed.insert(route.id.clone());
                    let name = js_trim(&format!("{method} {}", join_py_paths(&prefix, original)))
                        .to_string();
                    if name != route.name {
                        let mut updated = Node::clone(&route);
                        updated.name = name;
                        updates.push(updated);
                    }
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

/// One `X.include_router(router, prefix=…)`: the mounting router, the
/// literal prefix (`""` without one), and the file and variable mounted.
struct Mount {
    from_var: String,
    prefix: String,
    target: (String, String),
}

/// The include-derived base path of each (file, router variable): the
/// mounting router's base, plus its own prefix, plus the mount's, to a fixed
/// point (at most [`PREFIX_ROUNDS`] rounds). A router two mounts give
/// different paths in one round has none.
fn compose_bases(
    own: &BTreeMap<String, BTreeMap<String, String>>,
    mounts: &BTreeMap<String, Vec<Mount>>,
) -> BTreeMap<(String, String), String> {
    let mut base: BTreeMap<(String, String), String> = BTreeMap::new();
    for _ in 0..PREFIX_ROUNDS {
        // `None`: two mounts, two paths.
        let mut next: BTreeMap<(String, String), Option<String>> = BTreeMap::new();
        for (file, list) in mounts {
            for mount in list {
                let from_base = base
                    .get(&(file.clone(), mount.from_var.clone()))
                    .map_or("", String::as_str);
                let from_own = own
                    .get(file)
                    .and_then(|vars| vars.get(&mount.from_var))
                    .map_or("", String::as_str);
                let full = join_py_paths(&join_py_paths(from_base, from_own), &mount.prefix);
                match next.get(&mount.target) {
                    Some(Some(seen)) if *seen != full => {
                        next.insert(mount.target.clone(), None);
                    }
                    Some(None) => {}
                    _ => {
                        next.insert(mount.target.clone(), Some(full));
                    }
                }
            }
        }
        let next: BTreeMap<(String, String), String> = next
            .into_iter()
            .filter_map(|(key, path)| path.map(|path| (key, path)))
            .collect();
        let changed = next != base;
        base = next;
        if !changed {
            break;
        }
    }
    base
}

/// `/api/v1` + `/items` → `/api/v1/items`; `/items` + `` → `/items`; `` +
/// `` → `` (upstream `joinPyPaths`).
fn join_py_paths(prefix: &str, path: &str) -> String {
    let a = prefix.trim_end_matches('/');
    let b = path.trim_start_matches('/');
    if a.is_empty() {
        if b.is_empty() {
            String::new()
        } else {
            format!("/{b}")
        }
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{a}/{b}")
    }
}

/// The file and variable an `include_router` argument names (upstream
/// `includedRouter`): a router of the same file, `items.router` through the
/// module's import, or `items_router` through an alias import.
fn included_router(
    expr: &str,
    file: &str,
    local: &BTreeMap<String, String>,
    context: &dyn ResolutionContext,
) -> Option<(String, String)> {
    let segments: Vec<&str> = expr.split('.').collect();
    let head = segments[0];
    if segments.len() == 1 && local.contains_key(head) {
        return Some((file.to_string(), head.to_string()));
    }
    let mapping = context
        .get_import_mappings(file, Language::Python)
        .into_iter()
        .find(|mapping| mapping.local_name == head)?;
    if segments.len() > 1 {
        // `items.router`: `items` is a module — `from app.api.routes import
        // items`.
        let module = resolve_import_path(
            &format!("{}.{}", mapping.source, mapping.exported_name),
            file,
            Language::Python,
            context,
        )
        .or_else(|| resolve_import_path(&mapping.source, file, Language::Python, context))?;
        return Some((module, segments[segments.len() - 1].to_string()));
    }
    // `items_router`: `from .items import router as items_router`.
    let module = resolve_import_path(&mapping.source, file, Language::Python, context)?;
    Some((module, mapping.exported_name))
}

/// The ids of the routes FastAPI's own extraction makes in `file`, when the
/// pipeline extracts it with FastAPI at all (it is Python): a Flask route's
/// id has the same shape.
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
            extract_fastapi(file, &content)
                .nodes
                .into_iter()
                .map(|node| node.id)
                .collect()
        })
    })
}

/// The name a FastAPI route has in its own file, `METHOD path` (`/` for an
/// empty path), when its qualified name says it may be one.
fn in_file_name(route: &Node) -> Option<String> {
    let (method, path) = qualified_route(route)?;
    FASTAPI_METHODS
        .contains(&method)
        .then(|| format!("{method} {}", if path.is_empty() { "/" } else { path }))
}

/// A heuristic's pick (upstream `resolveByNameAndKind`), preferring the
/// files whose path contains one of `preferred_dirs`. A name the file
/// imports is the import's, as a React component's is (#2120): import
/// resolution finds it in the module the import names, and a package's name
/// stays the package's, where a guess linked a `Group` netbox imports from
/// `svgwrite` to a project class (KEEP-RUST, W7-19b).
fn heuristic(
    reference: &RefView,
    kinds: &[NodeKind],
    preferred_dirs: &[&str],
    confidence: f64,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if context
        .get_import_mappings(&reference.file_path, Language::Python)
        .iter()
        .any(|mapping| mapping.local_name == reference.reference_name)
    {
        return None;
    }
    pick_by_name_and_kind(
        reference,
        kinds,
        &|file| preferred_dirs.iter().any(|dir| file.contains(dir)),
        context,
        &PickOptions::default(),
    )
    .map(|target| ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework))
}

/// `^[A-Z][a-z]+$`.
fn is_capitalized_word(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_uppercase()
        && bytes[1..].iter().all(u8::is_ascii_lowercase)
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`.
fn is_python_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// A route node as the Python extractors make them.
fn route_node(
    id: String,
    name: String,
    qualified_name: String,
    file_path: &str,
    line: i64,
    end_column: i64,
) -> Node {
    framework_node(
        id,
        NodeKind::Route,
        name,
        qualified_name,
        file_path.to_string(),
        line,
        line,
        0,
        end_column,
        Language::Python,
        false,
    )
}

/// A reference from a route, at its line, column 0.
fn route_reference(
    from: &str,
    name: String,
    kind: EdgeKind,
    line: i64,
    file_path: &str,
) -> RefView {
    RefView {
        row_id: None,
        from_node_id: from.to_string(),
        reference_name: name,
        reference_kind: kind,
        line,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::Python,
        is_function_ref: false,
        reference_subkind: None,
    }
}

/// Django's URLconf routes and DRF router registrations (upstream
/// `djangoResolver.extract`).
fn extract_django(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    if !file_path.ends_with(".py") {
        return result;
    }
    let text = Text::python(content);
    for captures in django_route().captures_iter(&text.safe) {
        let whole = captures.get(0).expect("whole match");
        let found = captures.get(2).expect("url path");
        let handler = captures.get(3).expect("handler");
        let url = text.upstream(found.start(), found.end());
        let line = text.line_at(whole.start());
        let node = route_node(
            format!("route:{file_path}:{line}:{url}"),
            url.clone(),
            format!("{file_path}::route:{url}"),
            file_path,
            line,
            text.units(whole.start(), whole.end()),
        );
        let expr = text.upstream(handler.start(), handler.end());
        if let Some((name, kind)) = handler_target(js_trim(&expr)) {
            result
                .references
                .push(route_reference(&node.id, name, kind, line, file_path));
        }
        result.nodes.push(node);
    }

    // DRF: `router.register(r'articles', ArticleViewSet)`. The string first
    // argument tells it from `admin.site.register(Model, Admin)`; the
    // `View`/`ViewSet` suffix keeps it to viewsets.
    for captures in drf_register().captures_iter(&text.safe) {
        let whole = captures.get(0).expect("whole match");
        let found = captures.get(1).expect("prefix");
        let prefix = strip_route_anchors(&text.upstream(found.start(), found.end()));
        let expr = &captures[2];
        let viewset = expr.rsplit('.').next().unwrap_or(expr);
        if !(viewset.ends_with("View") || viewset.ends_with("ViewSet")) {
            continue;
        }
        let line = text.line_at(whole.start());
        let node = route_node(
            format!("route:{file_path}:{line}:VIEWSET:{prefix}"),
            format!("VIEWSET /{prefix}"),
            format!("{file_path}::route:{prefix}"),
            file_path,
            line,
            text.units(whole.start(), whole.end()),
        );
        result.references.push(route_reference(
            &node.id,
            viewset.to_string(),
            EdgeKind::References,
            line,
            file_path,
        ));
        result.nodes.push(node);
    }
    result
}

/// What a Django URL handler expression links (upstream
/// `resolveHandlerName`): the module an `include('x.y')` names, or the last
/// name of the handler without a trailing `.as_view(…)` and then one more
/// trailing method call. `None` for a shape it cannot read.
fn handler_target(expr: &str) -> Option<(String, EdgeKind)> {
    if let Some(captures) = django_include().captures(expr) {
        return Some((captures[1].to_string(), EdgeKind::Imports));
    }
    let head = replace_first(expr, as_view());
    let head = replace_first(&head, trailing_call());
    let last = head.split('.').rfind(|segment| !segment.is_empty())?;
    is_python_identifier(last).then(|| (last.to_string(), EdgeKind::References))
}

/// `text` without the first match of `pattern` (JavaScript's `replace` with
/// a non-global pattern and `''`).
fn replace_first(text: &str, pattern: &Regex) -> String {
    match pattern.find(text) {
        Some(found) => format!("{}{}", &text[..found.start()], &text[found.end()..]),
        None => text.to_string(),
    }
}

/// A DRF prefix without a leading `^` and a trailing `$` or `/$` (upstream
/// `replace(/^\^|\/?\$$/g, '')`).
fn strip_route_anchors(prefix: &str) -> String {
    let rest = prefix.strip_prefix('^').unwrap_or(prefix);
    let end = if rest.ends_with("/$") {
        rest.len() - 2
    } else if rest.ends_with('$') {
        rest.len() - 1
    } else {
        rest.len()
    };
    rest[..end].to_string()
}

/// Flask's decorator routes, Flask-RESTful resources and URL rules, in that
/// order (upstream `flaskResolver.extract`).
fn extract_flask(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    if !file_path.ends_with(".py") {
        return result;
    }
    let text = Text::python(content);
    // `@x.route('/path', methods=[...] | (...))`: the handler is the next
    // `def`, past other decorators and stacked routes.
    for captures in flask_route().captures_iter(&text.safe) {
        let method = captures
            .get(3)
            .and_then(|listed| flask_method().captures(listed.as_str()))
            .map_or_else(|| "GET".to_string(), |found| found[1].to_ascii_uppercase());
        decorator_route(&text, file_path, &captures, &method, 2, &mut result);
    }
    let restful = flask_restful(&text, file_path);
    result.nodes.extend(restful.nodes);
    result.references.extend(restful.references);
    let rules = flask_url_rules(&text, file_path);
    result.nodes.extend(rules.nodes);
    result.references.extend(rules.references);
    result
}

/// FastAPI's decorator routes (upstream `fastapiResolver.extract`).
fn extract_fastapi(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    if !file_path.ends_with(".py") {
        return result;
    }
    let text = Text::python(content);
    for captures in fastapi_route().captures_iter(&text.safe) {
        let method = captures[2].to_ascii_uppercase();
        decorator_route(&text, file_path, &captures, &method, 3, &mut result);
    }
    result
}

/// One decorator route and its handler (upstream `extractDecoratorRoutes`):
/// `METHOD path`, `/` for an empty path, referencing the next `def`.
fn decorator_route(
    text: &Text,
    file_path: &str,
    captures: &Captures,
    method: &str,
    path_group: usize,
    result: &mut FrameworkResolverExtractionResult,
) {
    let whole = captures.get(0).expect("whole match");
    let found = captures.get(path_group).expect("route path");
    let path = text.upstream(found.start(), found.end());
    let line = text.line_at(whole.start());
    let shown = if path.is_empty() { "/" } else { path.as_str() };
    let name = if method.is_empty() {
        shown.to_string()
    } else {
        format!("{method} {shown}")
    };
    let node = route_node(
        format!("route:{file_path}:{line}:{method}:{path}"),
        name,
        format!("{file_path}::{method}:{path}"),
        file_path,
        line,
        text.units(whole.start(), whole.end()),
    );
    if let Some(def) = handler_def().captures(&text.safe[whole.end()..]) {
        result.references.push(route_reference(
            &node.id,
            def[1].to_string(),
            EdgeKind::References,
            line,
            file_path,
        ));
    }
    result.nodes.push(node);
}

/// Flask-RESTful: `api.add_resource(Resource, '/path'[, '/path2'])`. The
/// resource class holds the verb methods, so each path is an `ANY` route
/// referencing it (upstream `extractFlaskRestful`).
fn flask_restful(text: &Text, file_path: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    for captures in restful().captures_iter(&text.safe) {
        let whole = captures.get(0).expect("whole match");
        let class = &captures[1];
        let list = captures.get(2).expect("paths");
        let line = text.line_at(whole.start());
        for item in quoted_path().find_iter(list.as_str()) {
            let path = text.upstream(
                list.start() + item.start() + 1,
                list.start() + item.end() - 1,
            );
            let node = route_node(
                format!("route:{file_path}:{line}:ANY:{path}"),
                format!("ANY {path}"),
                format!("{file_path}::ANY:{path}"),
                file_path,
                line,
                0,
            );
            result.references.push(route_reference(
                &node.id,
                class.to_string(),
                EdgeKind::References,
                line,
                file_path,
            ));
            result.nodes.push(node);
        }
    }
    result
}

/// Flask's imperative registration (upstream `extractFlaskUrlRules`, #2154):
/// `bp.add_url_rule('/path', 'name', view_func=…, methods=[…])`, and a
/// project's own wrapper handing a list of paths and a `view_func=` over
/// (flaskbb's `register_view(bp, routes=[…], view_func=Login.as_view(…))`).
/// A class-based view serves the verbs it defines, so its route is `ANY`
/// unless `methods=` says; a function defaults to `GET`.
fn flask_url_rules(text: &Text, file_path: &str) -> FrameworkResolverExtractionResult {
    // A call names a path and a target only through `.add_url_rule` or a
    // `view_func=` in its arguments, which are a piece of this text. (A
    // `view(_func=…)` passes upstream's check on name and arguments joined,
    // but has no target.)
    if !text.safe.contains("view_func") && !text.safe.contains(".add_url_rule") {
        return FrameworkResolverExtractionResult::default();
    }
    scan_url_rules(text, file_path)
}

/// [`flask_url_rules`] past its pre-filter: every call upstream reads.
fn scan_url_rules(text: &Text, file_path: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut at = 0;
    while let Some(captures) = rule_opener().captures_at(&text.safe, at) {
        let whole = captures.get(0).expect("opener");
        let callee = captures.get(1).expect("callee").as_str();
        let open = whole.end();
        // Upstream's `lastIndex`: past the `(`, unless a rule is read.
        at = open;
        let Some(close) = balanced_args(text, open) else {
            continue;
        };
        let args = &text.safe[open..close];
        if !rule_check().is_match(&format!("{callee}{args}")) {
            continue;
        }
        let is_rule = callee == ".add_url_rule";
        let mut paths: Vec<String> = Vec::new();
        if is_rule {
            if let Some(first) = rule_first().captures(args) {
                let found = first.get(1).or_else(|| first.get(2)).expect("rule");
                paths.push(text.upstream(open + found.start(), open + found.end()));
            }
        } else if let Some(list) = rule_list().captures(args) {
            let items = list.get(1).expect("list items");
            let base = open + items.start();
            for item in list_item().captures_iter(items.as_str()) {
                let found = item.get(1).or_else(|| item.get(2)).expect("item");
                paths.push(text.upstream(base + found.start(), base + found.end()));
            }
        }
        paths.retain(|path| path.starts_with('/'));
        if paths.is_empty() {
            continue;
        }
        let expr = view_func()
            .captures(args)
            .map(|found| found[1].to_string())
            .or_else(|| {
                is_rule
                    .then(|| rule_positional().captures(args))
                    .flatten()
                    .map(|found| found[1].to_string())
            });
        let Some(expr) = expr else {
            continue;
        };
        let class_view = expr.ends_with(".as_view");
        let target = if class_view {
            &expr[..expr.len() - ".as_view".len()]
        } else {
            expr.as_str()
        };
        let listed = methods_listed()
            .captures(args)
            .map(|found| found.get(1).expect("methods").as_str());
        let methods: Vec<String> = match listed {
            Some(listed) if !listed.is_empty() => method_quoted()
                .captures_iter(listed)
                .map(|found| found[1].to_ascii_uppercase())
                .collect(),
            _ => vec![if class_view { "ANY" } else { "GET" }.to_string()],
        };
        let line = text.line_at(whole.start());
        let name = target.rsplit('.').next().unwrap_or(target);
        for path in &paths {
            for method in &methods {
                let id = format!("route:{file_path}:{line}:{method}:{path}");
                if !seen.insert(id.clone()) {
                    continue;
                }
                result.references.push(route_reference(
                    &id,
                    name.to_string(),
                    EdgeKind::References,
                    line,
                    file_path,
                ));
                result.nodes.push(route_node(
                    id,
                    format!("{method} {path}"),
                    format!("{file_path}::{method}:{path}"),
                    file_path,
                    line,
                    0,
                ));
            }
        }
        at = close;
    }
    result
}

/// The offset of the `)` closing the call whose arguments start at `open`,
/// strings skipped, within [`ARGS_WINDOW`] UTF-16 code units of the source
/// (upstream `balancedArgs`): any closer ends a level, as there.
fn balanced_args(text: &Text, open: usize) -> Option<usize> {
    let mut depth = 1u32;
    let mut quote: Option<char> = None;
    let mut units = 0usize;
    let mut chars = text.content[open..].char_indices();
    while let Some((offset, ch)) = chars.next() {
        if units >= ARGS_WINDOW {
            return None;
        }
        units += ch.len_utf16();
        // A character the stripped text keeps that is not ASCII is none of
        // the ones below.
        let Some(seen) = text.piece(open + offset, ch) else {
            continue;
        };
        if let Some(open_quote) = quote {
            if seen == '\\' {
                if let Some((_, escaped)) = chars.next() {
                    units += escaped.len_utf16();
                }
            } else if seen == open_quote {
                quote = None;
            }
            continue;
        }
        match seen {
            '"' | '\'' => quote = Some(seen),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;
    use crate::types::ImportMapping;

    fn context() -> FrameworkExtractionContext {
        FrameworkExtractionContext::without_config("/project")
    }

    fn django(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        DjangoResolver.extract(file, src, &context()).unwrap()
    }

    fn flask(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        FlaskResolver.extract(file, src, &context()).unwrap()
    }

    fn fastapi(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        FastapiResolver.extract(file, src, &context()).unwrap()
    }

    fn names(result: &FrameworkResolverExtractionResult) -> Vec<String> {
        result.nodes.iter().map(|node| node.name.clone()).collect()
    }

    fn targets(result: &FrameworkResolverExtractionResult) -> Vec<String> {
        result
            .references
            .iter()
            .map(|reference| reference.reference_name.clone())
            .collect()
    }

    // --- Django (upstream frameworks.test.ts 43-98) -------------------------

    #[test]
    fn django_path_with_a_class_based_view() {
        let src = "\nfrom django.urls import path\nfrom users.views import UserListView\n\nurlpatterns = [\n    path('users/', UserListView.as_view(), name='user-list'),\n]\n";
        let result = django("users/urls.py", src);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].kind, NodeKind::Route);
        assert_eq!(result.nodes[0].name, "users/");
        assert_eq!(result.nodes[0].id, "route:users/urls.py:6:users/");
        assert_eq!(
            result.nodes[0].qualified_name,
            "users/urls.py::route:users/"
        );
        assert_eq!(result.references.len(), 1);
        assert_eq!(result.references[0].reference_name, "UserListView");
        assert_eq!(result.references[0].reference_kind, EdgeKind::References);
        assert_eq!(result.references[0].from_node_id, result.nodes[0].id);
        assert_eq!(result.references[0].line, 6);
        assert_eq!(result.references[0].column, 0);
    }

    #[test]
    fn django_path_with_a_dotted_view_and_a_bare_function() {
        let src = "from django.urls import path\nfrom api.v1 import views as api_v1_views\nurlpatterns = [path('api/', api_v1_views.UserListView.as_view())]\n";
        assert_eq!(targets(&django("api/urls.py", src)), ["UserListView"]);
        let src =
            "from django.urls import path\nurlpatterns = [path('home/', home_view, name='home')]\n";
        assert_eq!(targets(&django("home/urls.py", src)), ["home_view"]);
    }

    #[test]
    fn django_include_is_an_imports_reference() {
        let src = "from django.urls import path, include\nurlpatterns = [path('api/', include('api.urls'))]\n";
        let result = django("root/urls.py", src);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.references[0].reference_name, "api.urls");
        assert_eq!(result.references[0].reference_kind, EdgeKind::Imports);
    }

    #[test]
    fn django_re_path_and_url() {
        let src = "from django.urls import re_path, url\nurlpatterns = [re_path(r'^users/$', UserView), url(r'^old/$', OldView)]\n";
        assert_eq!(
            names(&django("legacy/urls.py", src)),
            ["^users/$", "^old/$"]
        );
        let empty = django("views.py", "def foo(): return 1\n");
        assert!(empty.nodes.is_empty() && empty.references.is_empty());
        assert!(django("urls.txt", "path('x/', v)\n").nodes.is_empty());
    }

    #[test]
    fn django_handler_drops_one_trailing_method_call_of_any_name() {
        let src = "urlpatterns = [path(\"x/\", views.handler.decorated())]\n";
        assert_eq!(targets(&django("app/urls.py", src)), ["handler"]);
        let src = "urlpatterns = [path(\"x/\", views.Page.as_view(template_name='a.html'))]\n";
        assert_eq!(targets(&django("app/urls.py", src)), ["Page"]);
        // `.as_view(…)` goes first wherever it starts: inside the call's
        // argument it leaves `views.page(x`, which is no name, where
        // dropping the trailing call alone would leave `views`.
        let src = "urlpatterns = [path(\"x/\", views.page(x.as_view(y)))]\n";
        let result = django("app/urls.py", src);
        assert_eq!(result.nodes.len(), 1);
        assert!(result.references.is_empty());
        // A shape that is no name links nothing.
        let src = "urlpatterns = [path(\"x/\", include(('app.urls', 'app'), namespace='x'))]\n";
        let result = django("app/urls.py", src);
        assert_eq!(result.nodes.len(), 1);
        assert!(result.references.is_empty());
    }

    #[test]
    fn django_drf_register_takes_viewsets_only() {
        let src = "router.register(r'^articles/$', ArticleViewSet)\nrouter.register(r'tags', views.TagView)\nadmin.site.register(Article, ArticleAdmin)\nrouter.register(r'misc', Helper)\n";
        let result = django("app/urls.py", src);
        assert_eq!(names(&result), ["VIEWSET /articles", "VIEWSET /tags"]);
        assert_eq!(result.nodes[0].id, "route:app/urls.py:1:VIEWSET:articles");
        assert_eq!(
            result.nodes[0].qualified_name,
            "app/urls.py::route:articles"
        );
        assert_eq!(targets(&result), ["ArticleViewSet", "TagView"]);
        assert_eq!(strip_route_anchors("^/$"), "");
        assert_eq!(strip_route_anchors("$^"), "$^");
        assert_eq!(strip_route_anchors("^a/b/"), "a/b/");
    }

    #[test]
    fn django_skips_comments_and_docstrings() {
        let src = "\n# urls.py example:\n# path('/admin/', AdminPanel.as_view())\n\"\"\"\nOther routing example:\n    path('/users/', UserListView.as_view())\n\"\"\"\nurlpatterns = [path('/real/', RealView.as_view())]\n";
        assert_eq!(names(&django("app/urls.py", src)), ["/real/"]);
    }

    #[test]
    fn django_end_column_counts_utf16_units_of_the_source() {
        // `é` is two bytes and one unit; `😀` four bytes and two units.
        // The match ends with the handler's name: no paren pair follows it.
        let result = django("app/urls.py", "path('café/😀', views.v)\n");
        assert_eq!(result.nodes[0].name, "café/😀");
        let matched = "path('café/😀', views.v";
        assert_eq!(
            result.nodes[0].end_column,
            matched.encode_utf16().count() as i64
        );
        // A comment of non-ASCII text between the arguments counts as
        // upstream's blanks do, one per code unit.
        let result = django("app/urls.py", "path('a/', # é😀\n    views.v)\n");
        assert_eq!(targets(&result), ["v"]);
        let matched = "path('a/', # é😀\n    views.v";
        assert_eq!(
            result.nodes[0].end_column,
            matched.encode_utf16().count() as i64
        );
        assert_eq!(result.nodes[0].end_column, 28);
        // A DRF registration and the decorator routes count the same way: the
        // match's length in code units (upstream `match[0].length`).
        let result = django("app/urls.py", "router.register(r'café', ArticleViewSet)\n");
        let matched = ".register(r'café', ArticleViewSet";
        assert_eq!(
            result.nodes[0].end_column,
            matched.encode_utf16().count() as i64
        );
        let result = flask("app.py", "@app.route('/café')\ndef c():\n    pass\n");
        assert_eq!(result.nodes[0].end_column, 19);
        let result = fastapi("main.py", "@app.get('/😀')\ndef e():\n    pass\n");
        assert_eq!(result.nodes[0].end_column, 14);
    }

    // --- Flask (upstream frameworks.test.ts 100-170) ------------------------

    #[test]
    fn flask_decorator_routes() {
        let result = flask(
            "app.py",
            "\n@app.route('/users')\ndef list_users():\n    return []\n",
        );
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(result.nodes[0].id, "route:app.py:2:GET:/users");
        assert_eq!(result.nodes[0].qualified_name, "app.py::GET:/users");
        assert_eq!(targets(&result), ["list_users"]);
        let result = flask(
            "routes.py",
            "\n@users_bp.route('/<id>', methods=['POST'])\ndef create_user(id):\n    pass\n",
        );
        assert_eq!(names(&result), ["POST /<id>"]);
        assert_eq!(targets(&result), ["create_user"]);
        let result = flask(
            "routes.py",
            "\n@bp.route('/profile')\n@login_required\ndef profile():\n    return render_template('profile.html')\n",
        );
        assert_eq!(names(&result), ["GET /profile"]);
        assert_eq!(targets(&result), ["profile"]);
        let result = flask(
            "routes.py",
            "\n@bp.route('/', methods=['GET', 'POST'])\n@bp.route('/index', methods=['GET', 'POST'])\n@login_required\ndef index():\n    return render_template('index.html')\n",
        );
        assert_eq!(names(&result), ["GET /", "GET /index"]);
        assert_eq!(targets(&result), ["index", "index"]);
        let result = flask(
            "views.py",
            "\n@blueprint.route('/api/articles', methods=('post',))\ndef make_article():\n    pass\n",
        );
        assert_eq!(names(&result), ["POST /api/articles"]);
        assert_eq!(targets(&result), ["make_article"]);
        // An empty path names `/`; its id keeps the empty path.
        let result = flask("app.py", "@app.route('')\nasync def root():\n    pass\n");
        assert_eq!(names(&result), ["GET /"]);
        assert_eq!(result.nodes[0].id, "route:app.py:1:GET:");
        assert_eq!(targets(&result), ["root"]);
    }

    #[test]
    fn flask_restful_resources() {
        let src = "\napi.add_resource(TodoResource, '/todos/<id>')\napi.add_org_resource(AlertResource, '/api/alerts/<id>', endpoint='alert')\napi.add_resource(Many, '/a', \"/b\")\n";
        let result = flask("api.py", src);
        assert_eq!(
            names(&result),
            [
                "ANY /todos/<id>",
                "ANY /api/alerts/<id>",
                "ANY /a",
                "ANY /b"
            ]
        );
        assert_eq!(
            targets(&result),
            ["TodoResource", "AlertResource", "Many", "Many"]
        );
        assert_eq!(result.nodes[0].end_column, 0);
    }

    #[test]
    fn flask_skips_commented_routes() {
        let src = "\n# @app.route('/fake')\n# def fake_view():\n#     return ''\n\n@app.route('/real')\ndef real_view():\n    return ''\n";
        let result = flask("app.py", src);
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(targets(&result), ["real_view"]);
    }

    /// upstream `flask-url-rules.test.ts`'s `app/auth.py` and its helper.
    #[test]
    fn flask_url_rules_and_a_wrapper() {
        let auth = r#"from flask import Blueprint
from flask.views import MethodView
from app.helpers import register_view

auth = Blueprint("auth", __name__)


class Login(MethodView):
    def get(self):
        return "form"

    def post(self):
        return "ok"


def logout():
    return "bye"


register_view(auth, routes=["/login", "/signin"], view_func=Login.as_view("login"))
auth.add_url_rule("/logout", "logout", logout, methods=["POST"])
auth.add_url_rule("/health", view_func=logout)
"#;
        let result = flask("app/auth.py", auth);
        assert_eq!(
            names(&result),
            ["ANY /login", "ANY /signin", "POST /logout", "GET /health"]
        );
        assert_eq!(targets(&result), ["Login", "Login", "logout", "logout"]);
        assert_eq!(result.nodes[0].id, "route:app/auth.py:20:ANY:/login");
        let helpers = "def register_view(bp_or_app, routes, view_func, **kwargs):\n    for route in routes:\n        bp_or_app.add_url_rule(route, view_func=view_func, **kwargs)\n";
        assert!(flask("app/helpers.py", helpers).nodes.is_empty());
    }

    #[test]
    fn flask_url_rule_shapes() {
        let rules = |src: &str| names(&flask("app/views.py", src));
        assert_eq!(
            rules("bp.add_url_rule(rule='/r', view_func=f)\n"),
            ["GET /r"]
        );
        assert_eq!(rules("bp.add_url_rule('/n', None, handler)\n"), ["GET /n"]);
        assert_eq!(
            rules("bp.add_url_rule('/m', view_func=V.as_view('v'), methods=('get', 'Put'))\n"),
            ["GET /m", "PUT /m"]
        );
        // An empty methods list takes the default; one without a quoted
        // method makes no route.
        assert_eq!(
            rules("bp.add_url_rule('/e', view_func=f, methods=[])\n"),
            ["GET /e"]
        );
        assert!(rules("bp.add_url_rule('/q', view_func=f, methods=[GET])\n").is_empty());
        // Only `/` paths.
        assert_eq!(
            rules("wrap(bp, urls=['relative', '/kept'], view_func=f)\n"),
            ["GET /kept"]
        );
        // The same id once.
        assert_eq!(
            rules("wrap(bp, paths=['/d', '/d'], view_func=f, methods=['GET', 'GET'])\n"),
            ["GET /d"]
        );
        // Mismatched quotes are no list item: the list is read from a later
        // start, here none.
        assert!(rules("wrap(bp, routes=['/a\"], view_func=f)\n").is_empty());
        // Nor a rule or an endpoint name (upstream's backreferences).
        assert!(rules("bp.add_url_rule('/a\", view_func=f)\n").is_empty());
        assert!(rules("bp.add_url_rule('/n', 'x\", handler)\n").is_empty());
        // Strings that balance for Python but pair a quote with the other
        // kind inside them are no rule, list or endpoint name either.
        assert!(rules("bp.add_url_rule(\"/a'\", view_func=f)\n").is_empty());
        assert!(rules("wrap(bp, routes=[\"/a'\", \"'/b\"], view_func=f)\n").is_empty());
        assert!(rules("bp.add_url_rule('/n', \"x', handler\")\n").is_empty());
        // A split `view(_func=…)` passes the check but has no target.
        assert!(rules("view(_func=a, rules=[\"/x\"])\n").is_empty());
    }

    /// The pre-filter drops only texts no call of which names a path and a
    /// target.
    #[test]
    fn flask_url_rule_prefilter_changes_no_result() {
        for src in [
            "view(_func=a, rules=[\"/x\"])\n",
            "add_url_rule('/x', 'x', f)\n",
            "bp.add_url_rule('/x', 'x', f)\n",
            "wrap(bp, routes=['/w'], view_func=f)\n",
            "wrap(bp, routes=['/w'], func=f)\n",
            "# bp.add_url_rule('/x', view_func=f)\nwrap(routes=['/y'])\n",
            "view_func = make()\nwrap(bp, routes=['/z'], view=view_func)\n",
        ] {
            let text = Text::python(src);
            let filtered = flask_url_rules(&text, "app/views.py");
            let scanned = scan_url_rules(&text, "app/views.py");
            assert_eq!(names(&filtered), names(&scanned), "{src}");
            assert_eq!(targets(&filtered), targets(&scanned), "{src}");
        }
    }

    #[test]
    fn flask_url_rule_reading_resumes_after_its_paren() {
        // A rule read resumes after its `)`: the inner `as_view(` call is
        // not read again, while a call that is no rule resumes after its `(`
        // and the rule inside it is read.
        let src = "outer(bp.add_url_rule('/in', view_func=f))\nwrap(bp, routes=['/w'], view_func=Inner.as_view(register(['/no'], view_func=g)))\n";
        let result = flask("app/views.py", src);
        assert_eq!(names(&result), ["GET /in", "ANY /w"]);
    }

    #[test]
    fn flask_url_rule_window_counts_utf16_units() {
        // 3,998 units of `é` (one unit, two bytes each) leave room for the
        // closing paren; 4,000 do not.
        let fits = format!(
            "bp.add_url_rule('/x', view_func=f, doc='{}')\n",
            "é".repeat(3_950)
        );
        assert_eq!(names(&flask("app/views.py", &fits)), ["GET /x"]);
        let long = format!(
            "bp.add_url_rule('/x', view_func=f, doc='{}')\n",
            "é".repeat(4_000)
        );
        assert!(names(&flask("app/views.py", &long)).is_empty());
        // Astral characters count two units each.
        let astral = format!(
            "bp.add_url_rule('/x', view_func=f, doc='{}')\n",
            "😀".repeat(1_975)
        );
        assert_eq!(names(&flask("app/views.py", &astral)), ["GET /x"]);
        let astral = format!(
            "bp.add_url_rule('/x', view_func=f, doc='{}')\n",
            "😀".repeat(2_000)
        );
        assert!(names(&flask("app/views.py", &astral)).is_empty());
        // Blanked comment text counts as the source's units, not its bytes.
        let comment = format!(
            "bp.add_url_rule('/x',  # {}\n view_func=f)\n",
            "é".repeat(3_900)
        );
        assert_eq!(names(&flask("app/views.py", &comment)), ["GET /x"]);
    }

    #[test]
    fn non_ascii_text_in_triple_quotes_counts_as_upstream_blanks() {
        // A blanked triple-quoted string counts one unit per code unit of the
        // source: `😀` two.
        let rule = |n: usize| {
            format!(
                "bp.add_url_rule('/x', view_func=f, doc=\"\"\"{}\"\"\")\n",
                "😀".repeat(n)
            )
        };
        // The arguments are `'/x', view_func=f, doc="""` (26 units), the
        // emoji and `"""` (3): they fit while they take at most 3,999 units.
        assert_eq!(names(&flask("app/views.py", &rule(1_985))), ["GET /x"]);
        assert!(names(&flask("app/views.py", &rule(1_986))).is_empty());
        // A blanked string between `path`'s arguments is no comma.
        let src = "path('a/', \"\"\"é😀\"\"\" , views.v)\n";
        assert!(django("app/urls.py", src).nodes.is_empty());
    }

    #[test]
    fn javascript_space_rules_hold() {
        // U+FEFF is JavaScript space; U+0085 is not.
        let result = flask("app.py", "@app.route(\u{FEFF}'/s')\ndef s():\n    pass\n");
        assert_eq!(names(&result), ["GET /s"]);
        assert!(
            names(&flask(
                "app.py",
                "@app.route(\u{85}'/s')\ndef s():\n    pass\n"
            ))
            .is_empty()
        );
    }

    // --- FastAPI (upstream frameworks.test.ts 172-222) ----------------------

    #[test]
    fn fastapi_decorator_routes() {
        let result = fastapi(
            "main.py",
            "\n@app.get('/users')\nasync def list_users():\n    return []\n",
        );
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(targets(&result), ["list_users"]);
        let result = fastapi(
            "items.py",
            "\n@router.post('/items')\ndef create_item(item: Item):\n    pass\n",
        );
        assert_eq!(names(&result), ["POST /items"]);
        assert_eq!(targets(&result), ["create_item"]);
        let result = fastapi(
            "articles.py",
            "\n@router.get(\"\", response_model=ListOfArticles, name=\"articles:list\")\nasync def list_articles():\n    return []\n",
        );
        assert_eq!(names(&result), ["GET /"]);
        assert_eq!(result.nodes[0].id, "route:articles.py:2:GET:");
        assert_eq!(result.nodes[0].qualified_name, "articles.py::GET:");
        assert_eq!(targets(&result), ["list_articles"]);
        let result = fastapi(
            "articles.py",
            "\n@router.post(\n    \"\",\n    status_code=201,\n    response_model=ArticleInResponse,\n)\nasync def create_article():\n    pass\n",
        );
        assert_eq!(names(&result), ["POST /"]);
        assert_eq!(targets(&result), ["create_article"]);
    }

    #[test]
    fn fastapi_skips_docstring_routes() {
        let src = "\n\"\"\"\nExample:\n    @app.get('/in-docstring')\n    async def doc():\n        pass\n\"\"\"\n@app.get('/real')\nasync def real_handler():\n    return {}\n";
        let result = fastapi("main.py", src);
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(targets(&result), ["real_handler"]);
    }

    // --- Detection -----------------------------------------------------------

    #[test]
    fn django_detection() {
        for file in ["requirements.txt", "setup.py", "pyproject.toml"] {
            let ctx = RouteCtx::default().file(file, "Django>=5\n");
            assert!(DjangoResolver.detect(&ctx), "{file}");
        }
        assert!(DjangoResolver.detect(&RouteCtx::default().file("manage.py", "")));
        assert!(!DjangoResolver.detect(&RouteCtx::default().file("Pipfile", "django\n")));
        assert!(!DjangoResolver.detect(&RouteCtx::default().file("app/manage.py", "")));
    }

    #[test]
    fn flask_detection() {
        for file in ["requirements.txt", "pyproject.toml", "Pipfile", "setup.py"] {
            let ctx = RouteCtx::default().file(file, "Flask==3\n");
            assert!(FlaskResolver.detect(&ctx), "{file}");
        }
        // A word, in ASCII case only: no `flasks`, no Kelvin sign for `k`.
        assert!(!FlaskResolver.detect(&RouteCtx::default().file("requirements.txt", "flasks\n")));
        assert!(
            !FlaskResolver.detect(&RouteCtx::default().file("requirements.txt", "flas\u{212A}\n"))
        );
        let entry = "from flask import Flask\napp = Flask(__name__)\n";
        // Each entry-script name, at the root and below it; a name that only
        // ends like one is none.
        for name in ["app", "application", "main", "wsgi", "__init__"] {
            for file in [format!("{name}.py"), format!("svc/{name}.py")] {
                assert!(
                    FlaskResolver.detect(&RouteCtx::default().file(&file, entry)),
                    "{file}"
                );
            }
        }
        assert!(!FlaskResolver.detect(&RouteCtx::default().file("conduit/server.py", entry)));
        assert!(!FlaskResolver.detect(&RouteCtx::default().file("conduit/myapp.py", entry)));
        // Both the call and the import.
        assert!(
            !FlaskResolver.detect(&RouteCtx::default().file("app.py", "app = Flask(__name__)\n"))
        );
        assert!(
            !FlaskResolver.detect(&RouteCtx::default().file("app.py", "from flask import Flask\n"))
        );
        // At most 50 entry scripts are read, in path order.
        let mut ctx = RouteCtx::default();
        for i in 0..50 {
            ctx = ctx.file(&format!("a{i:02}/main.py"), "x = 1\n");
        }
        let late = ctx.file("b/main.py", entry);
        assert!(!FlaskResolver.detect(&late));
    }

    /// Detection reads its root files by path, whether or not the file list
    /// holds them, and scans below the root only the files it holds.
    #[test]
    fn detection_reads_root_files_by_path_and_scans_listed_ones() {
        let unlisted = |path: &str, content: &str| RouteCtx::default().unlisted(path, content);
        assert!(DjangoResolver.detect(&unlisted("requirements.txt", "django\n")));
        assert!(DjangoResolver.detect(&unlisted("manage.py", "")));
        assert!(FlaskResolver.detect(&unlisted("Pipfile", "flask = \"*\"\n")));
        for file in ["requirements.txt", "pyproject.toml"] {
            assert!(
                FastapiResolver.detect(&unlisted(file, "fastapi\n")),
                "{file}"
            );
        }
        for file in ["app.py", "main.py", "api.py"] {
            assert!(
                FastapiResolver.detect(&unlisted(file, "app = FastAPI()\n")),
                "{file}"
            );
        }
        // An entry script or a manifest below the root is read only when the
        // file list holds it.
        let entry = "from flask import Flask\napp = Flask(__name__)\n";
        assert!(!FlaskResolver.detect(&unlisted("conduit/app.py", entry)));
        assert!(!FastapiResolver.detect(&unlisted("svc/requirements.txt", "fastapi\n")));
        assert!(
            FastapiResolver.detect(&RouteCtx::default().file("svc/requirements.txt", "fastapi\n"))
        );
    }

    #[test]
    fn fastapi_detection() {
        assert!(FastapiResolver.detect(&RouteCtx::default().file("requirements.txt", "FastAPI\n")));
        assert!(
            FastapiResolver
                .detect(&RouteCtx::default().file("pyproject.toml", "fastapi = \"*\"\n"))
        );
        for file in ["app.py", "main.py", "api.py"] {
            let ctx = RouteCtx::default().file(file, "app = FastAPI()\n");
            assert!(FastapiResolver.detect(&ctx), "{file}");
        }
        // Below the root (`950686de`): a script at most four segments deep.
        let app = "from fastapi import FastAPI\napp = FastAPI()\n";
        assert!(FastapiResolver.detect(&RouteCtx::default().file("backend/app/main.py", app)));
        assert!(FastapiResolver.detect(&RouteCtx::default().file("a/b/c/main.py", app)));
        assert!(!FastapiResolver.detect(&RouteCtx::default().file("a/b/c/d/main.py", app)));
        for file in ["svc/app.py", "svc/main.py", "svc/api.py"] {
            assert!(
                FastapiResolver.detect(&RouteCtx::default().file(file, app)),
                "{file}"
            );
        }
        // A manifest at any depth.
        for file in [
            "svc/requirements.txt",
            "svc/pyproject.toml",
            "a/b/c/d/requirements-dev.txt",
        ] {
            assert!(
                FastapiResolver.detect(&RouteCtx::default().file(file, "fastapi\n")),
                "{file}"
            );
        }
        assert!(!FastapiResolver.detect(&RouteCtx::default().file("backend/server.py", app)));
        // At most 40 reads, in path order.
        let mut ctx = RouteCtx::default();
        for i in 0..40 {
            ctx = ctx.file(&format!("a{i:02}/main.py"), "x = 1\n");
        }
        assert!(!FastapiResolver.detect(&ctx.file("b/main.py", app)));
    }

    /// The three resolvers sit where upstream registers them, after Astro and
    /// before GoFrame and Expo Router, and the detected list keeps that
    /// order.
    #[test]
    fn the_python_frameworks_are_detected_in_upstreams_order() {
        let ctx = RouteCtx::default()
            .file(
                "package.json",
                r#"{"dependencies":{"astro":"4","expo-router":"3"}}"#,
            )
            .file(
                "go.mod",
                "module x\n\nrequire github.com/gogf/gf/v2 v2.7.0\n",
            )
            .file("requirements.txt", "django\nflask\nfastapi\n");
        let names: Vec<String> = super::super::detect_frameworks(&ctx)
            .iter()
            .map(|resolver| resolver.name().to_string())
            .collect();
        let at = |name: &str| {
            names
                .iter()
                .position(|found| found == name)
                .unwrap_or_else(|| panic!("{name} in {names:?}"))
        };
        assert!(at("astro") < at("django"), "{names:?}");
        assert!(
            at("django") < at("flask") && at("flask") < at("fastapi"),
            "{names:?}"
        );
        assert!(
            at("fastapi") < at("goframe") && at("fastapi") < at("expo-router"),
            "{names:?}"
        );
    }

    // --- Resolution ------------------------------------------------------------

    fn node(kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
        let mut node = framework_node(
            format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name.to_string(),
            name.to_string(),
            file.to_string(),
            line,
            line + 1,
            0,
            0,
            Language::Python,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn reference(name: &str, kind: EdgeKind, file: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("file:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 30,
            column: 4,
            file_path: file.to_string(),
            language: Language::Python,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn with(nodes: Vec<Node>) -> RouteCtx {
        nodes.into_iter().fold(RouteCtx::default(), |ctx, node| {
            let file = node.file_path.clone();
            ctx.routes(&file, vec![node])
        })
    }

    #[test]
    fn django_resolves_models_views_and_forms_in_their_folders() {
        let model = node(NodeKind::Class, "Article", "blog/models.py", 1);
        let view = node(NodeKind::Function, "ArticleView", "blog/views/list.py", 1);
        let form = node(NodeKind::Class, "ArticleForm", "blog/forms.py", 1);
        let ctx = with(vec![model.clone(), view.clone(), form.clone()]);
        let resolve = |name: &str| {
            DjangoResolver
                .resolve(&reference(name, EdgeKind::Calls, "shop/urls.py"), &ctx)
                .map(|resolved| (resolved.target_node_id, resolved.confidence))
        };
        assert_eq!(resolve("Article"), Some((model.id, 0.8)));
        assert_eq!(resolve("ArticleView"), Some((view.id, 0.8)));
        assert_eq!(resolve("ArticleForm"), Some((form.id, 0.8)));
        assert_eq!(resolve("article"), None);
        assert!(DjangoResolver.claims_reference("shop.urls"));
        assert!(!DjangoResolver.claims_reference("_iterable_class"));
        assert!(is_capitalized_word("Article"));
        assert!(!is_capitalized_word("A"));
        assert!(!is_capitalized_word("ArticleX"));
    }

    #[test]
    fn flask_and_fastapi_resolve_their_name_shapes() {
        let bp = node(NodeKind::Variable, "users_bp", "app/users.py", 3);
        let router = node(NodeKind::Variable, "router", "app/api/items.py", 3);
        let dep = node(NodeKind::Function, "get_db", "app/core/db.py", 3);
        let ctx = with(vec![bp.clone(), router.clone(), dep.clone()]);
        let flask = FlaskResolver.resolve(
            &reference("users_bp", EdgeKind::References, "app/main.py"),
            &ctx,
        );
        assert_eq!(flask.map(|r| r.target_node_id), Some(bp.id));
        let routed = FastapiResolver.resolve(
            &reference("router", EdgeKind::References, "app/main.py"),
            &ctx,
        );
        assert_eq!(
            routed.map(|r| (r.target_node_id, r.confidence)),
            Some((router.id, 0.8))
        );
        let depends =
            FastapiResolver.resolve(&reference("get_db", EdgeKind::Calls, "app/main.py"), &ctx);
        assert_eq!(
            depends.map(|r| (r.target_node_id, r.confidence)),
            Some((dep.id, 0.75))
        );
    }

    /// A name the file imports is the import's (KEEP-RUST, W7-19b): no
    /// Django, Flask or FastAPI name heuristic guesses it, whether the import
    /// names a package (`Group` from `svgwrite`) or a module of the project,
    /// which import resolution finds. A file that imports none of the names
    /// gets the guesses as before.
    #[test]
    fn a_name_the_file_imports_is_left_to_import_resolution() {
        let model = node(NodeKind::Class, "Group", "users/models.py", 1);
        let view = node(NodeKind::Class, "GroupView", "users/views.py", 1);
        let bp = node(NodeKind::Variable, "users_bp", "app/users.py", 3);
        let router = node(NodeKind::Variable, "router", "app/api/items.py", 3);
        let dep = node(NodeKind::Function, "get_db", "app/core/db.py", 3);
        let mut ctx = with(vec![
            model.clone(),
            view.clone(),
            bp.clone(),
            router.clone(),
            dep.clone(),
        ]);
        let imports = "from svgwrite.container import Group\n\
                       from users.views import GroupView\n\
                       from app.users import users_bp\n\
                       from .api.items import router\n\
                       from app.core.db import get_db\n";
        for mapping in crate::import_resolver::extract_import_mappings(imports, Language::Python) {
            ctx = ctx.import("app/main.py", mapping);
        }
        let resolve = |resolver: &dyn FrameworkResolver, name: &str, kind: EdgeKind, file: &str| {
            resolver
                .resolve(&reference(name, kind, file), &ctx)
                .map(|resolved| resolved.target_node_id)
        };
        let cases: [(&dyn FrameworkResolver, &str, EdgeKind, &str); 5] = [
            (&DjangoResolver, "Group", EdgeKind::Instantiates, &model.id),
            (&DjangoResolver, "GroupView", EdgeKind::References, &view.id),
            (&FlaskResolver, "users_bp", EdgeKind::References, &bp.id),
            (&FastapiResolver, "router", EdgeKind::References, &router.id),
            (&FastapiResolver, "get_db", EdgeKind::Calls, &dep.id),
        ];
        for (resolver, name, kind, target) in cases {
            assert_eq!(resolve(resolver, name, kind, "app/main.py"), None, "{name}");
            assert_eq!(
                resolve(resolver, name, kind, "app/other.py").as_deref(),
                Some(target),
                "{name}"
            );
        }
    }

    // --- FastAPI prefixes --------------------------------------------------------

    /// A context of Python files as an index holds them: each file's node,
    /// FastAPI's and Flask's routes, and the files' imports.
    fn indexed(files: &[(&str, &str)]) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for (path, content) in files {
            let mut file_node = framework_node(
                format!("file:{path}"),
                NodeKind::File,
                path.rsplit('/').next().unwrap().to_string(),
                path.to_string(),
                path.to_string(),
                1,
                content.lines().count() as i64,
                0,
                0,
                Language::Python,
                false,
            );
            file_node.updated_at = 0;
            let mut nodes = vec![file_node];
            nodes.extend(extract_fastapi(path, content).nodes);
            nodes.extend(extract_flask(path, content).nodes);
            ctx = ctx.file(path, content).routes(path, nodes);
            for mapping in
                crate::import_resolver::extract_import_mappings(content, Language::Python)
            {
                ctx = ctx.import(path, mapping);
            }
        }
        ctx
    }

    fn renamed(ctx: &RouteCtx) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = FastapiResolver
            .post_extract(ctx)
            .unwrap()
            .into_iter()
            .map(|node| (node.qualified_name, node.name))
            .collect();
        out.sort();
        out
    }

    const ORDERS: &str = "from fastapi import APIRouter\n\nrouter = APIRouter(prefix=\"/orders\", tags=[\"orders\"])\n\n@router.get(\"/\")\ndef list_orders():\n    return []\n\n@router.get(\"/{order_id}\")\ndef get_order(order_id: int):\n    return order_id\n";
    const ITEMS: &str = "from fastapi import APIRouter\n\nrouter = APIRouter()\n\n@router.post(\"/items\")\ndef create_item():\n    return 1\n";
    const V1: &str = "from fastapi import APIRouter\nfrom .orders import router as orders_router\napi_router = APIRouter()\napi_router.include_router(orders_router)\n";
    const MAIN: &str = "from fastapi import FastAPI\nfrom .items import router\nfrom .v1 import api_router\nfrom .config import settings\napp = FastAPI()\napp.include_router(router)\napp.include_router(api_router, prefix=\"/api/v1\")\napp.include_router(api_router, prefix=settings.LEGACY)\n";

    /// upstream `ui-steps-api-servers.test.ts`'s FastAPI service
    /// (`bc45e907`): an alias import, an aggregate router, a literal and a
    /// computed mount.
    #[test]
    fn fastapi_prefixes_compose_down_the_mount_tree() {
        let ctx = indexed(&[
            ("api/orders.py", ORDERS),
            ("api/items.py", ITEMS),
            ("api/v1.py", V1),
            ("api/main.py", MAIN),
            ("api/config.py", "settings = None\n"),
        ]);
        assert_eq!(
            renamed(&ctx),
            [
                (
                    "api/orders.py::GET:/".to_string(),
                    "GET /api/v1/orders".to_string()
                ),
                (
                    "api/orders.py::GET:/{order_id}".to_string(),
                    "GET /api/v1/orders/{order_id}".to_string()
                ),
            ]
        );
    }

    #[test]
    fn fastapi_prefix_targets_and_ambiguity() {
        // A module import: `items.router`, through `{source}.{name}`.
        let main = "from fastapi import FastAPI\nfrom .routes import items\napp = FastAPI()\napp.include_router(items.router, prefix=\"/v2\")\n";
        let module = "from fastapi import APIRouter\nrouter = APIRouter()\n@router.get(\"/x\")\ndef x():\n    pass\n";
        let ctx = indexed(&[("svc/main.py", main), ("svc/routes/items.py", module)]);
        assert_eq!(
            renamed(&ctx),
            [(
                "svc/routes/items.py::GET:/x".to_string(),
                "GET /v2/x".to_string()
            )]
        );
        // `from . import items` reads as upstream's does: `{source}.{name}`
        // is `..items`, a module one package up, and `.` the package's
        // `__init__.py`; neither is `svc/items.py`.
        let dotted = main.replace("from .routes import items", "from . import items");
        let ctx = indexed(&[("svc/main.py", dotted.as_str()), ("svc/items.py", module)]);
        assert!(renamed(&ctx).is_empty());
        // A router of the same file.
        let local = "from fastapi import APIRouter, FastAPI\nrouter = APIRouter()\n@router.get(\"/y\")\ndef y():\n    pass\napp = FastAPI()\napp.include_router(router, prefix=\"/local/\")\n";
        let ctx = indexed(&[("main.py", local)]);
        assert_eq!(
            renamed(&ctx),
            [("main.py::GET:/y".to_string(), "GET /local/y".to_string())]
        );
        // Two literal mounts at two paths: left alone.
        let twice = format!("{local}app.include_router(router, prefix=\"/other\")\n");
        let ctx = indexed(&[("main.py", twice.as_str())]);
        assert!(renamed(&ctx).is_empty());
        // A computed prefix only: left alone.
        let computed = local.replace("prefix=\"/local/\"", "prefix=settings.API");
        assert!(renamed(&indexed(&[("main.py", computed.as_str())])).is_empty());
        // An own prefix without a mount still applies; an empty path is the
        // prefix itself.
        let own = "from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/p\")\n@router.get(\"\")\ndef p():\n    pass\n";
        assert_eq!(
            renamed(&indexed(&[("p.py", own)])),
            [("p.py::GET:".to_string(), "GET /p".to_string())]
        );
    }

    #[test]
    fn fastapi_renames_only_its_own_routes_and_restores_stale_names() {
        // A Flask `add_url_rule` route on a FastAPI decorator's line keeps
        // its name.
        let mixed = "from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/p\")\n@router.get(\"/a\"); bp.add_url_rule(\"/b\", view_func=b)\ndef a():\n    pass\n";
        let ctx = indexed(&[("m.py", mixed)]);
        assert_eq!(
            renamed(&ctx),
            [("m.py::GET:/a".to_string(), "GET /p/a".to_string())]
        );
        // A route renamed earlier whose prefix went away gets its in-file
        // name back; one renamed now, or of another framework, is left.
        let plain = "from fastapi import APIRouter\nrouter = APIRouter()\n@router.get(\"/z\")\ndef z():\n    pass\n";
        let mut ctx = indexed(&[("z.py", plain)]);
        let mut stale = extract_fastapi("z.py", plain).nodes.remove(0);
        stale.name = "GET /old/z".to_string();
        let mut express = node(NodeKind::Route, "GET /api/u", "web/users.js", 2);
        express.qualified_name = "web/users.js::GET:/u".to_string();
        express.language = Language::JavaScript;
        ctx = with_replaced(ctx, stale).routes("web/users.js", vec![express]);
        assert_eq!(
            renamed(&ctx),
            [("z.py::GET:/z".to_string(), "GET /z".to_string())]
        );
    }

    /// `ctx` with `node` in place of the node of the same id.
    fn with_replaced(ctx: RouteCtx, node: Node) -> RouteCtx {
        let mut nodes: Vec<Node> = ctx
            .nodes()
            .iter()
            .filter(|n| n.id != node.id)
            .cloned()
            .collect();
        nodes.push(node);
        let mut out = RouteCtx::default();
        let files: BTreeSet<String> = nodes.iter().map(|n| n.file_path.clone()).collect();
        for file in files {
            if let Some(content) = ctx.read_file(&file) {
                out = out.file(&file, &content);
            }
            for mapping in ctx.get_import_mappings(&file, Language::Python) {
                out = out.import(&file, mapping);
            }
        }
        for node in nodes {
            let file = node.file_path.clone();
            out = out.routes(&file, vec![node]);
        }
        out
    }

    #[test]
    fn joins_python_paths_as_upstream() {
        assert_eq!(join_py_paths("/api/v1", "/items"), "/api/v1/items");
        assert_eq!(join_py_paths("/items", ""), "/items");
        assert_eq!(join_py_paths("", ""), "");
        assert_eq!(join_py_paths("", "/"), "");
        assert_eq!(join_py_paths("/", ""), "");
        assert_eq!(join_py_paths("", "x"), "/x");
        assert_eq!(join_py_paths("/a//", "//b"), "/a/b");
    }

    #[test]
    fn included_router_reads_imports() {
        let ctx = RouteCtx::default().file("svc/orders.py", "").import(
            "svc/v1.py",
            ImportMapping {
                local_name: "orders_router".to_string(),
                exported_name: "router".to_string(),
                source: ".orders".to_string(),
                is_default: false,
                is_namespace: false,
            },
        );
        let local = BTreeMap::new();
        assert_eq!(
            included_router("orders_router", "svc/v1.py", &local, &ctx),
            Some(("svc/orders.py".to_string(), "router".to_string()))
        );
        assert_eq!(included_router("unknown", "svc/v1.py", &local, &ctx), None);
    }
}
