//! Laravel (upstream `resolution/frameworks/laravel.ts` at `e8ddc4e2`:
//! #2099's route paths and mounts, #2271's string controllers).
//!
//! A project is Laravel's when its root holds `artisan` or
//! `app/Http/Kernel.php` ([`ROOT_DETECTION_FILES`], the sync's `php`
//! control kind). Every `Route::get('path', handler)` and
//! `Route::resource('name', Controller)` call in a `.php` file is a route
//! node, named by the path a request takes inside its file (prefix groups
//! included, [`route_groups`]); [`LaravelResolver::post_extract`] then adds
//! the prefix the file is served under (`routes/api.php` under `/api`, a
//! file mounted by a prefixed group, a `require` or Laravel 11's
//! `withRouting`), recomputing every name from the route's qualified name so
//! a mount that goes away is undone.
//!
//! A route references its handler as `Controller@action`
//! (`[C::class, 'm']` drops C's namespace; a string `'Ns\C@m'` keeps the
//! namespace below `App\Http\Controllers\`) and a resource route imports its
//! controller. Resolution, in upstream's order: `Model::method` (0.85; PHP
//! static calls are recorded `Model.method` by both extractors, so this and
//! the facade rule meet no extracted reference and are ported for
//! fidelity), facades and helper names (no result), a resource route's
//! controller class (0.9), then `Controller@action` (0.9). Where upstream
//! takes the first of several same-named candidates in store order, Rust
//! takes a single candidate and gives no result for several, and it only
//! considers PHP classes (KNOWN_DIFFS, KEEP-RUST).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{JS_SPACE, js_trim};
use super::offset_text::OffsetText;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::synthesis::{posix_dirname, posix_normalize};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver reads (upstream `languages`).
const LANGUAGES: [Language; 1] = [Language::Php];

/// The root files detection reads by path, indexed or not: inputs of the
/// sync's `php` control kind (`codegraph_watch::control_inputs`), since
/// `artisan` is never indexed and a configuration may keep
/// `app/Http/Kernel.php` out of the index.
pub const ROOT_DETECTION_FILES: [&str; 2] = ["artisan", "app/Http/Kernel.php"];

/// What a Laravel route's qualified name holds between its file and its
/// in-file path (upstream `ROUTE_QN`).
const ROUTE_QN: &str = "::route:";

/// The routes file Laravel serves under `/api` unless something mounts it
/// (upstream `DEFAULT_API_FILE`).
const DEFAULT_API_FILE: &str = "routes/api.php";

/// What a file outside `routes/` must contain to mount a routes file
/// (upstream `MOUNT_NEEDLES`).
const MOUNT_NEEDLES: [&str; 2] = ["routes/", "withRouting("];

/// The rounds mount prefixes compose down the include tree at most
/// (upstream's `round < 8`).
const MOUNT_ROUNDS: usize = 8;

/// The UTF-16 code units after a call's `)` its chain's next `->x(` must
/// fit in (upstream `slice(close + 1, close + 200)`).
const CHAIN_WINDOW: i64 = 199;

/// The helper functions that resolve to Laravel itself (upstream's Pattern
/// 3).
const HELPERS: [&str; 13] = [
    "route", "view", "config", "env", "app", "abort", "redirect", "response", "request", "session",
    "url", "asset", "mix",
];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), and its `\b` is written `(?-u:\b)`. Upstream's
/// `(['"])…\1` is written as two alternatives, one per quote.
fn php_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("laravel pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| php_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A route's `Controller@action` (upstream `CONTROLLER_ACTION`): a class
    /// name, a namespace path under the controllers root, or a fully
    /// qualified one.
    controller_action = r"^([A-Za-z_][{W}\\]*)@([{W}]+)$";
    /// A class written with its namespace path (upstream `NAMESPACED_CLASS`).
    namespaced_class = r"^[A-Za-z_][{W}]*(?:\\[{W}]+)+$";
    /// `Model::method` (upstream's Pattern 1).
    model_call = r"^([A-Z][a-zA-Z]+)::([{W}]+)$";
    /// A facade call (upstream's Pattern 2).
    facade_call = r"^(?:Auth|Cache|DB|Log|Mail|Queue|Session|Storage|Validator|Route|Request|Response)::[{W}]+$";
    /// `Route::get('path', handler)` and the other verbs.
    verb_route = r#"Route::(get|post|put|patch|delete|options|any){S}*\({S}*['"]([^'"]+)['"]{S}*,{S}*([^)]+)\)"#;
    /// `Route::resource('name', Controller, …)` and `apiResource`.
    resource_route = r#"Route::(?:resource|apiResource){S}*\({S}*['"]([^'"]+)['"]{S}*(?:,{S}*([^)]+))?\)"#;
    /// The start of a `Route::…` chain.
    chain_start = r"(?-u:\b)Route::([{W}]+){S}*\(";
    /// The next call of a chain, right after the last one's `)`.
    chain_next = r"^{S}*->{S}*([{W}]+){S}*\(";
    /// An argument that is an array.
    array_open = r"^{S}*\[";
    /// The `'prefix' => …` entry of a `Route::group` array.
    array_prefix = r#"['"]prefix['"]{S}*=>{S}*([^,\]]+)"#;
    /// A plain string literal.
    literal = r#"^{S}*(?:'([^'"]*)'|"([^'"]*)"){S}*$"#;
    /// `[Class::class, 'method']`.
    tuple_handler = r#"^\[{S}*([A-Za-z_\\][{W}\\]*)::class{S}*,{S}*['"]([^'"]+)['"]{S}*\]"#;
    /// `'Controller@method'`.
    at_handler = r#"^['"]([^'"@]+)@([^'"]+)['"]$"#;
    /// `Class::class`.
    class_handler = r"^([A-Za-z_\\][{W}\\]*)::class";
    /// A controller named by string, `'Ns\Controller'`.
    string_handler = r#"^['"]([A-Za-z_\\][{W}\\]*)['"]$"#;
    /// A group's last argument that is its routes, not a file to mount.
    closure_body = r"^{S}*(?:static{S}+)?(?:function|fn)(?-u:\b)";
    /// `require`, `include` (either `_once`) and `loadRoutesFrom(…);`.
    load_call = r"(?-u:\b)(?:require|include)(?:_once)?(?-u:\b)([^;]*);|(?-u:\b)loadRoutesFrom{S}*\(([^;]*)\){S}*;";
    /// Laravel 11's `withRouting(`.
    with_routing = r"(?-u:\b)withRouting{S}*\(";
    /// A named argument, `api: value`.
    named_arg = r"^{S}*([{W}]+){S}*:{S}*((?s:.*))$";
    /// `base_path('routes/x.php')`.
    base_path_call = r#"^base_path{S}*\({S}*(?:'([^'"]+)'|"([^'"]+)"){S}*\)$"#;
    /// `__DIR__.'/x.php'`.
    dir_path = r#"^__DIR__{S}*\.{S}*(?:'([^'"]+)'|"([^'"]+)")$"#;
    /// `dirname(__DIR__).'/routes/x.php'`.
    parent_dir_path = r#"^dirname{S}*\({S}*__DIR__{S}*\){S}*\.{S}*(?:'([^'"]+)'|"([^'"]+)")$"#;
    /// A bare `'routes/x.php'`.
    php_literal_path = r#"^(?:'([^'"]+\.php)'|"([^'"]+\.php)")$"#;
}

pub struct LaravelResolver;

impl FrameworkResolver for LaravelResolver {
    fn name(&self) -> &str {
        "laravel"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `artisan` or `app/Http/Kernel.php`.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        ROOT_DETECTION_FILES
            .iter()
            .any(|file| context.file_exists(file))
    }

    /// `Controller@action` and namespaced class names, which name no node,
    /// past the name pre-filter.
    fn claims_reference(&self, name: &str) -> bool {
        controller_action().is_match(name) || namespaced_class().is_match(name)
    }

    /// A route's references only (KEEP-RUST): upstream claims the names
    /// wherever they are written, so every PHP `use App\Models\User;` came
    /// past the pre-filter too.
    fn claims(&self, reference: &RefView) -> bool {
        is_route_reference(reference) && self.claims_reference(&reference.reference_name)
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        // Pattern 1: `Model::method`.
        if let Some(call) = model_call().captures(name)
            && let Some(target) = model_target(&call[1], &call[2], context)
        {
            return Some(framework_ref(reference, target, 0.85));
        }
        // Patterns 2 and 3: Laravel's own code.
        if facade_call().is_match(name) || HELPERS.contains(&name) {
            return None;
        }
        // Pattern 5: a resource route's controller.
        if reference.reference_kind == EdgeKind::Imports
            && reference.from_node_id.contains(":RESOURCE:")
            && let Some(target) = controller_class(name, context)
        {
            return Some(framework_ref(reference, target, 0.9));
        }
        // Pattern 4: `Controller@action`.
        let action = controller_action().captures(name)?;
        let target = controller_method(&action[1], &action[2], context)?;
        Some(framework_ref(reference, target, 0.9))
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        // Every route and group pattern starts with `Route::`, which the
        // comment stripper only ever blanks.
        if !file_path.ends_with(".php") || !content.contains("Route::") {
            return Some(FrameworkResolverExtractionResult::default());
        }
        Some(extract_routes(file_path, content))
    }

    /// Each route named by the path a request takes once its file is mounted
    /// (upstream `postExtract`): mounts compose down the include tree, and a
    /// file reached at two different prefixes keeps its in-file paths.
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let routes: Vec<Arc<Node>> = context
            .get_nodes_by_kind_shared(NodeKind::Route)
            .into_iter()
            .filter(|node| is_laravel_route(node))
            .collect();
        if routes.is_empty() {
            return Some(Vec::new());
        }
        let prefix_of = mount_prefixes(context);
        let mut updates = Vec::new();
        for route in &routes {
            let Some(at) = route.qualified_name.find(ROUTE_QN) else {
                continue;
            };
            let in_file = &route.qualified_name[at + ROUTE_QN.len()..];
            let full = join_route_path([
                prefix_of.get(&route.file_path).map_or("", String::as_str),
                in_file,
            ]);
            let name = if route.name.starts_with("resource:") {
                format!("resource:{}", &full[1..])
            } else {
                format!("{} {full}", verb_of(&route.name))
            };
            if name != route.name {
                let mut updated = Node::clone(route);
                updated.name = name;
                updates.push(updated);
            }
        }
        Some(updates)
    }
}

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// Whether `reference` is a route's: a PHP route's, which only this
/// resolver makes.
pub(crate) fn is_route_reference(reference: &RefView) -> bool {
    reference.language == Language::Php && reference.from_node_id.starts_with("route:")
}

/// A route this resolver made (upstream `isLaravelRoute`): its qualified
/// name holds the in-file path. No other resolver makes a PHP route.
pub(crate) fn is_laravel_route(node: &Node) -> bool {
    node.language == Language::Php
        && node
            .id
            .strip_prefix("route:")
            .and_then(|rest| rest.strip_prefix(node.file_path.as_str()))
            .is_some_and(|rest| rest.starts_with(':'))
        && node
            .qualified_name
            .strip_prefix(node.file_path.as_str())
            .and_then(|rest| rest.strip_prefix(ROUTE_QN))
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A verb route's verb: its name up to the first space (upstream
/// `name.slice(0, name.indexOf(' '))`, which drops the last character when
/// there is none; a Laravel verb route's name always has one).
fn verb_of(name: &str) -> &str {
    match name.split_once(' ') {
        Some((verb, _)) => verb,
        None => {
            let mut chars = name.chars();
            chars.next_back();
            chars.as_str()
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// The first node of `kind` named `name` among one file's nodes: the first
/// by line, as upstream's `getNodesInFile` orders them; on one line the
/// first by column, then id.
fn first_named<'n>(nodes: &'n [Arc<Node>], kind: NodeKind, name: &str) -> Option<&'n Arc<Node>> {
    nodes
        .iter()
        .filter(|node| node.kind == kind && node.name == name)
        .min_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        })
}

/// `Model::method` (upstream `resolveModelCall`): in `app/Models/Model.php`,
/// then in `app/Model.php`, the method, else the class.
fn model_target(class: &str, method: &str, context: &dyn ResolutionContext) -> Option<String> {
    for path in [
        format!("app/Models/{class}.php"),
        format!("app/{class}.php"),
    ] {
        if !context.file_exists(&path) {
            continue;
        }
        let nodes = context.get_nodes_in_file_shared(&path);
        if let Some(found) = first_named(&nodes, NodeKind::Method, method)
            .or_else(|| first_named(&nodes, NodeKind::Class, class))
        {
            return Some(found.id.clone());
        }
    }
    None
}

/// The PHP classes named `name` in a path containing `Controllers`
/// (upstream's `classes`; KEEP-RUST: only PHP's).
fn controller_classes(name: &str, context: &dyn ResolutionContext) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| {
            node.kind == NodeKind::Class
                && node.language == Language::Php
                && node.file_path.contains("Controllers")
        })
        .collect()
}

/// Whether `node`'s file ends with the written namespace path `rel`
/// (upstream ``(`/${filePath}`).toLowerCase().endsWith(`/${rel}.php`)``).
fn ends_with_path(node: &Node, suffix: &str) -> bool {
    format!("/{}", node.file_path.to_lowercase()).ends_with(suffix)
}

/// The resource route's controller class (upstream
/// `resolveControllerClass`): the one whose file ends with the written
/// path, else the only class of that name. Upstream takes its store's first
/// of several files ending with the path; Rust gives no result for them
/// (akaunting's `Common\Companies` is both `Common/Companies.php` and
/// `Api/Common/Companies.php`, and its two routes files mean one each).
fn controller_class(controller: &str, context: &dyn ResolutionContext) -> Option<String> {
    let rel = controller.replace('\\', "/");
    let class_name = rel.rsplit('/').next().unwrap_or(rel.as_str());
    let classes = controller_classes(class_name, context);
    let suffix = format!("/{}.php", rel.to_lowercase());
    let exact: Vec<&Arc<Node>> = classes
        .iter()
        .filter(|node| ends_with_path(node, &suffix))
        .collect();
    match exact.as_slice() {
        [only] => return Some(only.id.clone()),
        [] => {}
        _ => return None,
    }
    match classes.as_slice() {
        [only] => Some(only.id.clone()),
        _ => None,
    }
}

/// The first method named `method` in `file_path` (upstream `methodIn`).
fn method_in(file_path: &str, method: &str, context: &dyn ResolutionContext) -> Option<String> {
    let nodes = context.get_nodes_in_file_shared(file_path);
    first_named(&nodes, NodeKind::Method, method).map(|node| node.id.clone())
}

/// The methods named `method` in the files of `classes`, each once.
fn methods_in<'c>(
    classes: impl Iterator<Item = &'c Arc<Node>>,
    method: &str,
    context: &dyn ResolutionContext,
) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for class in classes {
        if let Some(id) = method_in(&class.file_path, method, context)
            && !found.contains(&id)
        {
            found.push(id);
        }
    }
    found
}

/// `Controller@action` (upstream `resolveControllerMethod`): the method in
/// `app/Http/Controllers/{path}.php`, else in the file of a controller class
/// of that name, the ones whose file ends with a written namespace path
/// first. Upstream takes the first such file holding the method; Rust takes
/// the only one and gives no result for several.
fn controller_method(
    controller: &str,
    method: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let rel = controller.replace('\\', "/");
    let path = format!("app/Http/Controllers/{rel}.php");
    if context.file_exists(&path)
        && let Some(found) = method_in(&path, method, context)
    {
        return Some(found);
    }
    let class_name = rel.rsplit('/').next().unwrap_or(rel.as_str());
    let classes = controller_classes(class_name, context);
    if rel.contains('/') {
        let suffix = format!("/{}.php", rel.to_lowercase());
        let found = methods_in(
            classes.iter().filter(|node| ends_with_path(node, &suffix)),
            method,
            context,
        );
        match found.as_slice() {
            [only] => return Some(only.clone()),
            [] => {}
            _ => return None,
        }
        // A namespace path that names no file decides nothing by name alone.
        if classes.len() > 1 {
            return None;
        }
    }
    match methods_in(classes.iter(), method, context).as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// A file's routes (upstream `laravelResolver.extract`): the verb routes,
/// then the resource routes, on the comment-stripped text.
fn extract_routes(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    let text = OffsetText::php(content);
    let groups = route_groups(&text);
    for found in verb_route().captures_iter(&text.safe) {
        let whole = found.get(0).expect("whole match");
        let path = capture(&text, found.get(2));
        let handler = capture(&text, found.get(3));
        let line = text.line_at(whole.start());
        let upper = found[1].to_ascii_uppercase();
        let in_file = join_route_path([prefix_at(&groups, whole.start()).as_str(), path.as_str()]);
        let node = framework_node(
            format!("route:{file_path}:{line}:{upper}:{path}"),
            NodeKind::Route,
            format!("{upper} {in_file}"),
            format!("{file_path}{ROUTE_QN}{in_file}"),
            file_path.to_string(),
            line,
            line,
            0,
            text.units(whole.start(), whole.end()),
            Language::Php,
            false,
        );
        if let Some(name) = laravel_handler(&handler) {
            result.references.push(route_reference(
                &node,
                name,
                EdgeKind::References,
                file_path,
            ));
        }
        result.nodes.push(node);
    }
    for found in resource_route().captures_iter(&text.safe) {
        let whole = found.get(0).expect("whole match");
        let name = capture(&text, found.get(1));
        let line = text.line_at(whole.start());
        let in_file = join_route_path([prefix_at(&groups, whole.start()).as_str(), name.as_str()]);
        let node = framework_node(
            format!("route:{file_path}:{line}:RESOURCE:{name}"),
            NodeKind::Route,
            format!("resource:{}", &in_file[1..]),
            format!("{file_path}{ROUTE_QN}{in_file}"),
            file_path.to_string(),
            line,
            line,
            0,
            text.units(whole.start(), whole.end()),
            Language::Php,
            false,
        );
        if let Some(handler) = found.get(2) {
            // The controller is the first argument; an options array may
            // follow.
            let handler = text.upstream(handler.start(), handler.end());
            let first = split_args(&handler)
                .first()
                .map_or(handler.as_str(), |range| &handler[range.clone()]);
            if let Some(controller) = laravel_handler(first) {
                result.references.push(route_reference(
                    &node,
                    controller,
                    EdgeKind::Imports,
                    file_path,
                ));
            }
        }
        result.nodes.push(node);
    }
    result
}

/// A capture of the stripped text as upstream's holds it.
fn capture(text: &OffsetText, found: Option<regex::Match>) -> String {
    found.map_or(String::new(), |found| {
        text.upstream(found.start(), found.end())
    })
}

fn route_reference(node: &Node, name: String, kind: EdgeKind, file_path: &str) -> RefView {
    RefView {
        row_id: None,
        from_node_id: node.id.clone(),
        reference_name: name,
        reference_kind: kind,
        line: node.start_line,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::Php,
        is_function_ref: false,
        reference_subkind: None,
    }
}

/// The symbol a route handler names (upstream `extractLaravelHandler`):
/// `[C::class, 'm']` is `C@m` (C without its namespace), `'X@m'` keeps the
/// namespace below the controllers root, `C::class` is C, a string `'Ns\C'`
/// is its path below the controllers root; anything else (a closure) none.
fn laravel_handler(expr: &str) -> Option<String> {
    let trimmed = js_trim(expr);
    let short = |name: &str| name.rsplit('\\').next().unwrap_or(name).to_string();
    if let Some(tuple) = tuple_handler().captures(trimmed) {
        return Some(format!("{}@{}", short(&tuple[1]), &tuple[2]));
    }
    if let Some(at) = at_handler().captures(trimmed) {
        return Some(format!("{}@{}", controller_path(&at[1]), &at[2]));
    }
    if let Some(class) = class_handler().captures(trimmed) {
        return Some(short(&class[1]));
    }
    string_handler()
        .captures(trimmed)
        .map(|class| controller_path(&class[1]))
}

/// A string handler's class as a path under the controllers root (upstream
/// `controllerPath`): leading `\`s and `App\Http\Controllers\` (ASCII
/// letters in any case) off.
fn controller_path(written: &str) -> String {
    const ROOT: &str = r"App\Http\Controllers\";
    let rest = written.trim_start_matches('\\');
    match rest.get(..ROOT.len()) {
        Some(head) if head.eq_ignore_ascii_case(ROOT) => rest[ROOT.len()..].to_string(),
        _ => rest.to_string(),
    }
}

/// A route group: where its arguments open and close, the prefix it adds
/// (`{prefix}` when not a literal), and its top-level arguments as byte
/// ranges of the text (upstream `RouteGroup`).
struct RouteGroup {
    open: usize,
    close: usize,
    prefix: String,
    args: Vec<Range<usize>>,
}

/// Every route group in a file (upstream `routeGroups`): `Route::group([…],
/// …)` and the `->group(…)` that ends a `Route::…` chain, with the prefix
/// the chain or the array names.
fn route_groups(text: &OffsetText) -> Vec<RouteGroup> {
    let safe = text.safe.as_str();
    let mut groups = Vec::new();
    for start in chain_start().captures_iter(safe) {
        let mut name = start[1].to_string();
        let mut open = start.get(0).expect("whole match").end() - 1;
        let mut prefixes: Vec<String> = Vec::new();
        while let Some(close) = match_paren(safe, open) {
            let args: Vec<Range<usize>> = split_args(&safe[open + 1..close])
                .into_iter()
                .map(|range| open + 1 + range.start..open + 1 + range.end)
                .collect();
            let first = args
                .first()
                .map_or(String::new(), |range| text.upstream(range.start, range.end));
            if name == "prefix" {
                prefixes.push(literal_or_placeholder(&first));
            }
            if name == "group" {
                // `Route::group(['prefix' => 'admin', …], …)` carries it in the
                // array.
                if prefixes.is_empty()
                    && array_open().is_match(&first)
                    && let Some(entry) = array_prefix().captures(&first)
                {
                    prefixes.push(literal_or_placeholder(&entry[1]));
                }
                groups.push(RouteGroup {
                    open,
                    close,
                    prefix: join_route_path(prefixes.iter().map(String::as_str)),
                    args,
                });
                break;
            }
            let Some(next) = chain_next().captures(&safe[close + 1..]) else {
                break;
            };
            let end = close + 1 + next.get(0).expect("whole match").end();
            if text.units(close + 1, end) > CHAIN_WINDOW {
                break;
            }
            name = next[1].to_string();
            open = end - 1;
        }
    }
    groups
}

/// The composed prefix of every group whose arguments contain `at`,
/// outermost first (upstream `prefixAt`).
fn prefix_at(groups: &[RouteGroup], at: usize) -> String {
    let mut enclosing: Vec<&RouteGroup> = groups
        .iter()
        .filter(|group| group.open < at && at < group.close)
        .collect();
    enclosing.sort_by_key(|group| group.open);
    join_route_path(enclosing.iter().map(|group| group.prefix.as_str()))
}

/// `'admin'` is `admin`; anything that is not a plain string literal is
/// `{prefix}` (upstream `literalOrPlaceholder`).
fn literal_or_placeholder(expr: &str) -> String {
    match literal().captures(expr) {
        Some(found) => found
            .get(1)
            .or_else(|| found.get(2))
            .map_or(String::new(), |piece| piece.as_str().to_string()),
        None => "{prefix}".to_string(),
    }
}

/// Route path segments joined the way Laravel does (upstream
/// `joinRoutePath`): `("api", "v1/", "/users")` is `/api/v1/users`, the
/// root `/`.
fn join_route_path<'p>(parts: impl IntoIterator<Item = &'p str>) -> String {
    let segments: Vec<&str> = parts
        .into_iter()
        .flat_map(|part| part.split('/'))
        .filter(|segment| !segment.is_empty())
        .collect();
    format!("/{}", segments.join("/"))
}

/// The end of the PHP string opening at `at` (its quote): past the closing
/// quote, a `\` skipping the byte after it (upstream's inner loops, which
/// compare only ASCII quotes and backslashes, so bytes and code units agree).
fn skip_string(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != quote {
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    i + 1
}

/// The `)` closing the `(` at `open`, PHP strings skipped (upstream
/// `matchParen`).
fn match_paren(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
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

/// The top-level comma split of an argument list, strings and brackets
/// respected, as byte ranges of it (upstream `splitArgs`).
fn split_args(args: &str) -> Vec<Range<usize>> {
    let bytes = args.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i64;
    let mut from = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(from..i);
                from = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if !js_trim(&args[from..]).is_empty() || !out.is_empty() {
        out.push(from..args.len());
    }
    out
}

// ---------------------------------------------------------------------------
// Mounts
// ---------------------------------------------------------------------------

/// A routes file one file mounts, with the prefix a request carries into it.
struct Mount {
    source: String,
    target: String,
    prefix: String,
}

/// The prefix each routes file is served under (upstream `postExtract`'s
/// mounts and rounds): the mounts of every `.php` file in `routes/` and of
/// every other one naming `routes/` or `withRouting(`, `routes/api.php`'s
/// default `/api`, composed down the include tree.
fn mount_prefixes(context: &dyn ResolutionContext) -> BTreeMap<String, String> {
    let files = context.get_all_files_shared();
    let outside: Vec<String> = files
        .iter()
        .filter(|file| file.ends_with(".php") && !file.starts_with("routes/"))
        .cloned()
        .collect();
    // Every sync searches each PHP file outside `routes/` for a mount, so
    // the context may read them in parallel.
    let mounting: HashSet<String> = context
        .files_containing(&outside, &MOUNT_NEEDLES)
        .into_iter()
        .collect();
    let mut mounts: Vec<Mount> = Vec::new();
    let mut custom_loader = false;
    for file in files.iter() {
        if !file.ends_with(".php") || (!file.starts_with("routes/") && !mounting.contains(file)) {
            continue;
        }
        let Some(content) = context.read_file_shared(file) else {
            continue;
        };
        if content.is_empty() {
            continue;
        }
        let text = OffsetText::php(&content);
        custom_loader |= route_file_mounts(&text, file, context, &mut mounts);
    }

    // Nothing mounts `routes/api.php` and no custom loader might: Laravel's
    // own default is `/api`, seeded before composing so a file `api.php`
    // includes inherits it.
    let mut defaults: BTreeMap<String, String> = BTreeMap::new();
    if !custom_loader
        && !mounts.iter().any(|mount| mount.target == DEFAULT_API_FILE)
        && context.file_exists(DEFAULT_API_FILE)
    {
        defaults.insert(DEFAULT_API_FILE.to_string(), "/api".to_string());
    }
    let mut prefix_of = defaults.clone();
    for _ in 0..MOUNT_ROUNDS {
        let mut next = defaults.clone();
        let mut ambiguous: BTreeSet<&str> = BTreeSet::new();
        for mount in &mounts {
            if mount.target == mount.source {
                continue;
            }
            let full = join_route_path([
                prefix_of.get(&mount.source).map_or("", String::as_str),
                mount.prefix.as_str(),
            ]);
            match next.get(&mount.target) {
                Some(seen) if *seen != full => {
                    ambiguous.insert(&mount.target);
                }
                _ => {
                    next.insert(mount.target.clone(), full);
                }
            }
        }
        for target in ambiguous {
            next.remove(target);
        }
        let changed = next != prefix_of;
        prefix_of = next;
        if !changed {
            break;
        }
    }
    prefix_of
}

/// The routes files `file` mounts (upstream `routeFileMounts`), pushed onto
/// `mounts`: a group whose last argument is a file, a `require`, `include`
/// or `loadRoutesFrom` (inside groups or not), and Laravel 11's
/// `withRouting(api:, web:)`. True when `withRouting(using:)` loads routes
/// by code this pass cannot read.
fn route_file_mounts(
    text: &OffsetText,
    file: &str,
    context: &dyn ResolutionContext,
    mounts: &mut Vec<Mount>,
) -> bool {
    let safe = text.safe.as_str();
    let groups = route_groups(text);
    let mut add = |expr: &str, prefix: String| {
        if let Some(target) = resolve_php_path(expr, file, context) {
            mounts.push(Mount {
                source: file.to_string(),
                target,
                prefix,
            });
        }
    };
    for group in &groups {
        // `->group(base_path('routes/v1.php'))`, `Route::group([...],
        // base_path(…))`.
        let body = group
            .args
            .last()
            .map_or(String::new(), |range| text.upstream(range.start, range.end));
        if !closure_body().is_match(&body) {
            add(&body, prefix_at(&groups, group.open + 1));
        }
    }
    for found in load_call().captures_iter(safe) {
        let raw = capture(text, found.get(1).or_else(|| found.get(2)));
        let mut expr = js_trim(&raw);
        // `require(__DIR__.'/x.php')`: the parens off only when they wrap the
        // whole expression.
        if expr.starts_with('(') && match_paren(expr, 0) == Some(expr.len() - 1) {
            expr = &expr[1..expr.len() - 1];
        }
        let at = found.get(0).expect("whole match").start();
        add(expr, prefix_at(&groups, at));
    }

    let mut custom_loader = false;
    for found in with_routing().find_iter(safe) {
        let open = found.end() - 1;
        let Some(close) = match_paren(safe, open) else {
            continue;
        };
        let mut named: HashMap<String, String> = HashMap::new();
        for range in split_args(&safe[open + 1..close]) {
            let arg = text.upstream(open + 1 + range.start, open + 1 + range.end);
            if let Some(pair) = named_arg().captures(&arg) {
                named.insert(pair[1].to_string(), pair[2].to_string());
            }
        }
        if named.contains_key("using") {
            custom_loader = true;
        }
        let api_prefix = named
            .get("apiPrefix")
            .map_or_else(|| "api".to_string(), |value| literal_or_placeholder(value));
        for (key, prefix) in [("api", api_prefix), ("web", String::new())] {
            let Some(value) = named.get(key).filter(|value| !value.is_empty()) else {
                continue;
            };
            if array_open().is_match(value) {
                let inner = array_items(js_trim(value));
                for range in split_args(&inner) {
                    add(&inner[range], prefix.clone());
                }
            } else {
                add(value, prefix.clone());
            }
        }
    }
    custom_loader
}

/// `trimmed.slice(1, -1)`: an array's text inside its first and last code
/// units. A last character outside the BMP leaves its first unit upstream,
/// which no pattern reads as anything but a lone surrogate: U+FFFD here.
fn array_items(trimmed: &str) -> String {
    let mut chars = trimmed.chars();
    chars.next();
    let Some(last) = chars.next_back() else {
        return String::new();
    };
    let mut items = chars.as_str().to_string();
    if last.len_utf16() == 2 {
        items.push('\u{FFFD}');
    }
    items
}

/// The project file a PHP path expression names (upstream
/// `resolvePhpPath`): `base_path('routes/x.php')`, `__DIR__.'/x.php'`,
/// `dirname(__DIR__).'/routes/x.php'` or a bare `'routes/x.php'`, when it
/// is a `.php` file inside the project that exists.
fn resolve_php_path(expr: &str, file: &str, context: &dyn ResolutionContext) -> Option<String> {
    let expr = js_trim(expr);
    let quoted = |found: regex::Captures| {
        found
            .get(1)
            .or_else(|| found.get(2))
            .map(|piece| piece.as_str().to_string())
    };
    // `path.posix.join` of a directory and a non-empty path.
    let join = |dir: &str, rel: String| posix_normalize(&format!("{dir}/{rel}"));
    let rel = if let Some(found) = base_path_call().captures(expr) {
        quoted(found)?
    } else if let Some(found) = dir_path().captures(expr) {
        join(posix_dirname(file), quoted(found)?)
    } else if let Some(found) = parent_dir_path().captures(expr) {
        join(posix_dirname(posix_dirname(file)), quoted(found)?)
    } else {
        quoted(php_literal_path().captures(expr)?)?
    };
    let normalized = posix_normalize(&rel);
    let mut target = normalized.as_str();
    while let Some(rest) = target.strip_prefix("./") {
        target = rest;
    }
    let target = target.trim_start_matches('/');
    if target.starts_with("..") || !target.ends_with(".php") {
        return None;
    }
    context.file_exists(target).then(|| target.to_string())
}

// ---------------------------------------------------------------------------
// What a sync's Laravel lane reads (`codegraph_watch::laravel_routes`)
// ---------------------------------------------------------------------------

/// The controller paths a Laravel reference's resolution reads, `\`
/// written `/`: a resource route's controller import (Pattern 5) and a
/// `Controller@action`'s controller (Pattern 4). Each one's last segment is
/// the class name it reads ([`controller_class_name`]), and Pattern 4 reads
/// [`controller_file`] first.
pub fn controller_paths(name: &str, kind: EdgeKind, from_node_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if kind == EdgeKind::Imports && from_node_id.contains(":RESOURCE:") {
        out.push(name.replace('\\', "/"));
    }
    if let Some(action) = controller_action().captures(name) {
        out.push(action[1].replace('\\', "/"));
    }
    out
}

/// The class name a controller path reads: its last segment.
pub fn controller_class_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The file Pattern 4 reads first for a controller path.
pub fn controller_file(path: &str) -> String {
    format!("app/Http/Controllers/{path}.php")
}

/// The class a `Model::method` reference reads (Pattern 1).
pub fn model_class(name: &str) -> Option<&str> {
    model_call()
        .captures(name)
        .and_then(|call| call.get(1))
        .map(|class| class.as_str())
}

/// The class whose `Model::method` references read `path`: Pattern 1 reads
/// `app/Models/{class}.php`, then `app/{class}.php`.
pub fn model_file_class(path: &str) -> Option<&str> {
    let file = path
        .strip_prefix("app/Models/")
        .filter(|rest| !rest.contains('/'))
        .or_else(|| path.strip_prefix("app/"))?;
    let class = file.strip_suffix(".php")?;
    let mut chars = class.chars();
    (chars.next().is_some_and(|first| first.is_ascii_uppercase())
        && class.len() >= 2
        && chars.all(|c| c.is_ascii_alphabetic()))
    .then_some(class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn laravel(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        LaravelResolver
            .extract(
                file,
                src,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .unwrap()
    }

    /// The names of the routes `src` declares in `routes/web.php`, after
    /// `<?php\n` (upstream `extractNames`).
    fn names(src: &str) -> Vec<String> {
        laravel("routes/web.php", &format!("<?php\n{src}"))
            .nodes
            .into_iter()
            .map(|node| node.name)
            .collect()
    }

    fn node(kind: NodeKind, name: &str, file: &str, line: i64, language: Language) -> Node {
        let mut node = framework_node(
            format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name.to_string(),
            name.to_string(),
            file.to_string(),
            line,
            line + 2,
            0,
            0,
            language,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn controller(file: &str, class: &str, methods: &[&str]) -> Vec<Node> {
        let mut nodes = vec![node(NodeKind::Class, class, file, 1, Language::Php)];
        for (offset, method) in methods.iter().enumerate() {
            nodes.push(node(
                NodeKind::Method,
                method,
                file,
                3 + 3 * offset as i64,
                Language::Php,
            ));
        }
        nodes
    }

    fn reference(name: &str, from: &str, kind: EdgeKind) -> RefView {
        RefView {
            row_id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: "routes/web.php".to_string(),
            language: Language::Php,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn action(name: &str) -> RefView {
        reference(name, "route:routes/web.php:1:GET:/x", EdgeKind::References)
    }

    fn resource(name: &str) -> RefView {
        reference(
            name,
            "route:routes/web.php:1:RESOURCE:photos",
            EdgeKind::Imports,
        )
    }

    fn target(ctx: &RouteCtx, reference: &RefView) -> Option<(String, f64)> {
        LaravelResolver
            .resolve(reference, ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    fn id(kind: NodeKind, name: &str, file: &str, line: i64) -> String {
        format!("{}:{file}:{name}:{line}", kind.as_str())
    }

    // -- frameworks.test.ts 976-1000 ------------------------------------

    #[test]
    fn extracts_a_route_with_the_controller_tuple_syntax() {
        let result = laravel(
            "routes/web.php",
            "Route::get('/users', [UserController::class, 'index']);\n",
        );
        assert_eq!(result.nodes[0].name, "GET /users");
        assert_eq!(result.references[0].reference_name, "UserController@index");
    }

    #[test]
    fn extracts_a_route_with_the_controller_at_action_syntax() {
        let result = laravel(
            "routes/web.php",
            "Route::post('/users', 'UserController@store');\n",
        );
        assert_eq!(result.references[0].reference_name, "UserController@store");
    }

    #[test]
    fn extracts_a_resource_route() {
        let result = laravel(
            "routes/web.php",
            "Route::resource('users', UserController::class);\n",
        );
        assert_eq!(result.nodes[0].kind, NodeKind::Route);
        assert_eq!(result.references[0].reference_name, "UserController");
        assert_eq!(result.references[0].reference_kind, EdgeKind::Imports);
    }

    // -- frameworks.test.ts 2110-2120 -----------------------------------

    #[test]
    fn skips_commented_route_calls() {
        let src = "<?php
// Route::get('/fake', [FakeController::class, 'index']);
# Route::get('/also-fake', 'FakeController@show');
/* Route::post('/another-fake', [X::class, 'y']); */
Route::get('/real', [RealController::class, 'index']);
";
        let result = laravel("routes/web.php", src);
        let names: Vec<&str> = result.nodes.iter().map(|node| node.name.as_str()).collect();
        let refs: Vec<&str> = result
            .references
            .iter()
            .map(|reference| reference.reference_name.as_str())
            .collect();
        assert_eq!(names, ["GET /real"]);
        assert_eq!(refs, ["RealController@index"]);
    }

    // -- laravel-route-paths.test.ts, inside a routes file ---------------

    #[test]
    fn names_a_path_written_without_a_leading_slash_from_the_root() {
        assert_eq!(
            names("Route::get('pages', [PageController::class, 'index']);"),
            ["GET /pages"]
        );
        assert_eq!(
            names("Route::get('/', [HomeController::class, 'index']);"),
            ["GET /"]
        );
    }

    #[test]
    fn applies_prefix_groups_nested_ones_in_order_and_only_inside_the_group() {
        let src = "
Route::prefix('api')->middleware('auth:sanctum')->group(static function (): void {
    Route::get('ping', static fn () => null);
    Route::prefix('v1/')->group(function () {
        Route::post('/songs', [SongController::class, 'store']);
    });
    Route::get('me', [ProfileController::class, 'show']);
});
Route::get('/after', [HomeController::class, 'after']);
";
        assert_eq!(
            names(src),
            [
                "GET /api/ping",
                "POST /api/v1/songs",
                "GET /api/me",
                "GET /after"
            ]
        );
    }

    #[test]
    fn reads_a_prefix_from_a_group_array_and_adds_nothing_for_a_middleware_only_group() {
        let src = "
Route::group(['middleware' => 'auth', 'prefix' => 'admin'], function () {
    Route::get('/', [AdminController::class, 'index']);
    Route::middleware('can:manage')->group(fn () => Route::get('users', [UserController::class, 'index']));
});
";
        assert_eq!(names(src), ["GET /admin", "GET /admin/users"]);
    }

    #[test]
    fn keeps_a_computed_prefix_as_a_placeholder_segment() {
        let src = "
Route::group(['prefix' => LaravelLocalization::setLocale()], function () {
    Route::get('about', [PageController::class, 'about']);
});
";
        assert_eq!(names(src), ["GET /{prefix}/about"]);
    }

    #[test]
    fn prefixes_a_resource_route_and_records_the_in_file_path() {
        let result = laravel(
            "routes/web.php",
            "<?php
Route::prefix('admin')->group(function () {
    Route::resource('photos', PhotoController::class);
    Route::get('stats', [StatsController::class, 'index']);
});
",
        );
        let names: Vec<&str> = result.nodes.iter().map(|node| node.name.as_str()).collect();
        let qualified: Vec<&str> = result
            .nodes
            .iter()
            .map(|node| node.qualified_name.as_str())
            .collect();
        assert_eq!(names, ["GET /admin/stats", "resource:admin/photos"]);
        assert_eq!(
            qualified,
            [
                "routes/web.php::route:/admin/stats",
                "routes/web.php::route:/admin/photos"
            ]
        );
        assert_eq!(result.nodes[1].id, "route:routes/web.php:3:RESOURCE:photos");
    }

    #[test]
    fn ignores_a_group_opened_inside_a_comment() {
        let src = "
// Route::prefix('old')->group(function () {
Route::get('kept', [X::class, 'y']);
";
        assert_eq!(names(src), ["GET /kept"]);
    }

    // -- laravel-controller-strings.test.ts, extraction ------------------

    #[test]
    fn string_controllers_keep_their_namespace_below_the_controllers_root() {
        let src = r"<?php
Route::get('uploads/{id}/inline', 'Common\Uploads@inline')->name('inline');
Route::get('portal/uploads/{id}', '\App\Http\Controllers\Portal\Uploads@inline');
Route::get('lower', 'app\http\controllers\Lower@show');
Route::resource('companies', 'Common\Companies', ['middleware' => ['dropzone']]);
Route::resource('photos', \App\Models\Photo::class, ['only' => ['index']]);
";
        let result = laravel("routes/admin.php", src);
        let refs: Vec<(&str, EdgeKind)> = result
            .references
            .iter()
            .map(|reference| (reference.reference_name.as_str(), reference.reference_kind))
            .collect();
        assert_eq!(
            refs,
            [
                (r"Common\Uploads@inline", EdgeKind::References),
                (r"Portal\Uploads@inline", EdgeKind::References),
                ("Lower@show", EdgeKind::References),
                (r"Common\Companies", EdgeKind::Imports),
                ("Photo", EdgeKind::Imports),
            ]
        );
    }

    // -- node shape ---------------------------------------------------------

    #[test]
    fn a_route_node_has_upstreams_id_name_qualified_name_and_columns() {
        let result = laravel(
            "routes/web.php",
            "<?php\n\nRoute::get('ping', fn () => 'pong');\n",
        );
        let route = &result.nodes[0];
        assert_eq!(route.id, "route:routes/web.php:3:GET:ping");
        assert_eq!(route.name, "GET /ping");
        assert_eq!(route.qualified_name, "routes/web.php::route:/ping");
        assert_eq!((route.start_line, route.end_line), (3, 3));
        // The match ends at the first `)`: `Route::get('ping', fn ()`.
        assert_eq!((route.start_column, route.end_column), (0, 24));
        assert_eq!(route.language, Language::Php);
        // A closure names nothing.
        assert!(result.references.is_empty());
    }

    #[test]
    fn end_column_and_the_chain_window_count_utf16_units_of_the_source() {
        // `é` is one code unit and two bytes.
        let src = "<?php\nRoute::get('é', [A::class, 'b']);\n";
        let result = laravel("routes/web.php", src);
        assert_eq!(result.nodes[0].end_column, 32);
        assert_eq!(result.nodes[0].name, "GET /é");

        // The next call of a chain must end within 199 units of the `)`:
        // the blanked `/*…*/`, `units` code units inside its four (a `😀` is
        // two units and four bytes), and `->group(`, eight more.
        let chain = |units: usize| {
            names(&format!(
                "Route::prefix('a')/*{}{}*/->group(function () {{ Route::get('x', [A::class, 'b']); }});\n",
                "😀".repeat(units / 2),
                "é".repeat(units % 2)
            ))
        };
        assert_eq!(chain(187), ["GET /a/x"]);
        assert_eq!(chain(188), ["GET /x"]);
    }

    #[test]
    fn a_file_without_route_calls_or_another_extension_extracts_nothing() {
        assert!(
            laravel("app/Models/User.php", "<?php\nclass User {}\n")
                .nodes
                .is_empty()
        );
        assert!(
            laravel("routes/web.inc", "Route::get('/x', 'A@b');\n")
                .nodes
                .is_empty()
        );
    }

    // -- helpers ------------------------------------------------------------

    #[test]
    fn joins_route_paths_the_way_laravel_does() {
        assert_eq!(join_route_path(["api", "v1/", "/users"]), "/api/v1/users");
        assert_eq!(join_route_path(["", "/"]), "/");
        assert_eq!(join_route_path(Vec::<&str>::new()), "/");
    }

    #[test]
    fn splits_arguments_at_the_top_level_and_matches_parens_past_strings() {
        let args = "'a', ['b' => 'c,d'], fn ($x) => f($x, 1), ";
        let pieces: Vec<&str> = split_args(args)
            .into_iter()
            .map(|range| &args[range])
            .collect();
        assert_eq!(
            pieces,
            ["'a'", " ['b' => 'c,d']", " fn ($x) => f($x, 1)", " "]
        );
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args(" x ").len(), 1);
        assert_eq!(match_paren(r"(')', '\'', (x))", 0), Some(15));
        assert_eq!(match_paren("(unbalanced", 0), None);
    }

    #[test]
    fn literal_or_placeholder_reads_only_a_plain_string() {
        assert_eq!(literal_or_placeholder(" 'admin' "), "admin");
        assert_eq!(literal_or_placeholder("\"v1\""), "v1");
        assert_eq!(literal_or_placeholder("''"), "");
        assert_eq!(literal_or_placeholder("'a\"'"), "{prefix}");
        assert_eq!(literal_or_placeholder("config('x')"), "{prefix}");
    }

    #[test]
    fn array_items_drop_the_brackets_by_code_unit() {
        assert_eq!(array_items("['a.php', 'b.php']"), "'a.php', 'b.php'");
        assert_eq!(array_items("["), "");
        assert_eq!(array_items("['a.php' 😀"), "'a.php' \u{FFFD}");
    }

    #[test]
    fn resolves_php_path_expressions_to_existing_project_files() {
        let ctx = RouteCtx::default()
            .file("routes/api.php", "")
            .file("routes/auth.php", "")
            .file("routes/api/v1.php", "")
            .unlisted("routes/unlisted.php", "");
        let resolve = |expr: &str, file: &str| resolve_php_path(expr, file, &ctx);
        assert_eq!(
            resolve(" base_path('routes/api.php') ", "x.php").as_deref(),
            Some("routes/api.php")
        );
        assert_eq!(
            resolve("__DIR__.'/auth.php'", "routes/web.php").as_deref(),
            Some("routes/auth.php")
        );
        assert_eq!(
            resolve("__DIR__ . '/../routes/api.php'", "bootstrap/app.php").as_deref(),
            Some("routes/api.php")
        );
        assert_eq!(
            resolve(
                "dirname(__DIR__).'/routes/api/v1.php'",
                "app/Providers/RouteServiceProvider.php"
            )
            .as_deref(),
            None,
            "dirname(__DIR__) of app/Providers is app"
        );
        assert_eq!(
            resolve("dirname(__DIR__).'/routes/api/v1.php'", "bootstrap/app.php").as_deref(),
            Some("routes/api/v1.php")
        );
        assert_eq!(
            resolve("'./routes/api.php'", "x.php").as_deref(),
            Some("routes/api.php")
        );
        assert_eq!(
            resolve("'/routes/api.php'", "x.php").as_deref(),
            Some("routes/api.php")
        );
        // Indexed or not, it has to exist; outside the project, computed or
        // not PHP, it names nothing.
        assert_eq!(
            resolve("base_path('routes/unlisted.php')", "x.php").as_deref(),
            Some("routes/unlisted.php")
        );
        assert_eq!(resolve("base_path('routes/gone.php')", "x.php"), None);
        assert_eq!(resolve("base_path('../routes/api.php')", "x.php"), None);
        assert_eq!(resolve("base_path('routes/api')", "x.php"), None);
        assert_eq!(resolve("base_path($file)", "x.php"), None);
        assert_eq!(resolve("base_path('routes/api.php\")", "x.php"), None);
    }

    // -- resolution -----------------------------------------------------------

    #[test]
    fn claims_controller_actions_and_namespaced_classes() {
        for name in [
            "UserController@index",
            r"Common\Uploads@inline",
            "_A@b",
            r"Common\Companies",
            r"App\Http\Controllers\UserController",
        ] {
            assert!(LaravelResolver.claims_reference(name), "{name}");
        }
        for name in [
            "UserController",
            "a@b@c",
            r"\Lead\X",
            "A@",
            r"A\",
            "Route.get",
        ] {
            assert!(!LaravelResolver.claims_reference(name), "{name}");
        }
    }

    #[test]
    fn a_controller_action_resolves_in_the_controllers_root_first() {
        let file = "app/Http/Controllers/UserController.php";
        let ctx = RouteCtx::default()
            .routes(file, controller(file, "UserController", &["index"]))
            .routes(
                "modules/Http/Controllers/UserController.php",
                controller(
                    "modules/Http/Controllers/UserController.php",
                    "UserController",
                    &["index"],
                ),
            );
        assert_eq!(
            target(&ctx, &action("UserController@index")),
            Some((id(NodeKind::Method, "index", file, 3), 0.9))
        );
    }

    #[test]
    fn a_namespace_path_picks_the_file_ending_with_it() {
        let common = "app/Http/Controllers/Common/Uploads.php";
        let portal = "app/Http/Controllers/Portal/Uploads.php";
        let module = "modules/Offline/Http/Controllers/Common/Uploads.php";
        let ctx = RouteCtx::default()
            .routes(
                common,
                controller(common, "Uploads", &["inline", "destroy"]),
            )
            .routes(portal, controller(portal, "Uploads", &["inline"]))
            .routes(module, controller(module, "Uploads", &["inline", "extra"]));
        // The root's file.
        assert_eq!(
            target(&ctx, &action(r"Portal\Uploads@inline")),
            Some((id(NodeKind::Method, "inline", portal, 3), 0.9))
        );
        // A fully qualified one: the only file ending with its path.
        assert_eq!(
            target(
                &ctx,
                &action(r"Offline\Http\Controllers\Common\Uploads@extra")
            ),
            Some((id(NodeKind::Method, "extra", module, 6), 0.9))
        );
        // Two files end with `Common/Uploads.php` and hold `inline`: upstream
        // takes its store's first, Rust neither (KEEP-RUST).
        assert_eq!(
            target(&ctx, &action(r"Http\Controllers\Common\Uploads@inline")),
            None
        );
        // One of them holds `extra`.
        assert_eq!(
            target(&ctx, &action(r"Http\Controllers\Common\Uploads@extra")),
            Some((id(NodeKind::Method, "extra", module, 6), 0.9))
        );
        // A path naming no file decides nothing among several same-named
        // classes.
        assert_eq!(target(&ctx, &action(r"Admin\Uploads@extra")), None);
    }

    #[test]
    fn a_bare_controller_needs_one_class_holding_the_method() {
        let a = "app/Http/Controllers/Admin/UserController.php";
        let b = "app/Http/Controllers/Api/UserController.php";
        let ctx = RouteCtx::default()
            .routes(a, controller(a, "UserController", &["index", "edit"]))
            .routes(b, controller(b, "UserController", &["index"]));
        // Both hold `index`: upstream takes its store's first, Rust neither.
        assert_eq!(target(&ctx, &action("UserController@index")), None);
        // Only one holds `edit`.
        assert_eq!(
            target(&ctx, &action("UserController@edit")),
            Some((id(NodeKind::Method, "edit", a, 6), 0.9))
        );
        assert_eq!(target(&ctx, &action("UserController@missing")), None);
    }

    #[test]
    fn controller_candidates_are_php_classes_in_controllers_paths() {
        let java = "src/main/java/Controllers/UserController.java";
        let outside = "app/Services/UserController.php";
        let mut nodes = controller(java, "UserController", &["index"]);
        for node in &mut nodes {
            node.language = Language::Java;
        }
        let ctx = RouteCtx::default()
            .routes(java, nodes)
            .routes(outside, controller(outside, "UserController", &["index"]));
        assert_eq!(target(&ctx, &action("UserController@index")), None);
    }

    #[test]
    fn two_class_nodes_in_one_file_find_one_method() {
        let file = "app/Http/Controllers/Legacy/UserController.php";
        let mut nodes = controller(file, "UserController", &["index"]);
        nodes.push(node(
            NodeKind::Class,
            "UserController",
            file,
            20,
            Language::Php,
        ));
        let ctx = RouteCtx::default().routes(file, nodes);
        assert_eq!(
            target(&ctx, &action("UserController@index")),
            Some((id(NodeKind::Method, "index", file, 3), 0.9))
        );
    }

    #[test]
    fn a_resource_route_imports_its_controller_class() {
        let companies = "app/Http/Controllers/Common/Companies.php";
        let api = "app/Http/Controllers/Api/Common/Companies.php";
        let photos = "app/Http/Controllers/PhotoController.php";
        let ctx = RouteCtx::default()
            .routes(companies, controller(companies, "Companies", &["index"]))
            .routes(photos, controller(photos, "PhotoController", &["index"]));
        assert_eq!(
            target(&ctx, &resource(r"Common\Companies")),
            Some((id(NodeKind::Class, "Companies", companies, 1), 0.9))
        );
        assert_eq!(
            target(&ctx, &resource("PhotoController")),
            Some((id(NodeKind::Class, "PhotoController", photos, 1), 0.9))
        );
        // Not from a resource route, or not an import: no controller class.
        assert_eq!(
            target(
                &ctx,
                &reference(
                    "PhotoController",
                    "route:routes/web.php:1:GET:/x",
                    EdgeKind::Imports
                )
            ),
            None
        );
        assert_eq!(
            target(
                &ctx,
                &reference(
                    "PhotoController",
                    "route:routes/web.php:1:RESOURCE:photos",
                    EdgeKind::References
                )
            ),
            None
        );

        // akaunting: the path ends both files; upstream takes its store's
        // first, Rust neither (KEEP-RUST).
        let ctx = RouteCtx::default()
            .routes(companies, controller(companies, "Companies", &["index"]))
            .routes(api, controller(api, "Companies", &["index"]));
        assert_eq!(target(&ctx, &resource(r"Common\Companies")), None);
        // Both files end with `Companies` too.
        assert_eq!(target(&ctx, &resource("Companies")), None);

        // No file ends with the path: the only class of the name, and of
        // two, neither (KEEP-RUST).
        let photos = "app/Http/Controllers/Photos.php";
        let gallery = "app/Http/Controllers/Admin/Gallery.php";
        let one =
            RouteCtx::default().routes(photos, controller(photos, "PhotoController", &["index"]));
        assert_eq!(
            target(&one, &resource("PhotoController")),
            Some((id(NodeKind::Class, "PhotoController", photos, 1), 0.9))
        );
        let two = one.routes(gallery, controller(gallery, "PhotoController", &["index"]));
        assert_eq!(target(&two, &resource("PhotoController")), None);
    }

    #[test]
    fn helper_names_resolve_to_nothing_before_a_resource_controller() {
        let file = "app/Http/Controllers/Route/View.php";
        let ctx = RouteCtx::default().routes(file, controller(file, "view", &[]));
        assert_eq!(target(&ctx, &resource("view")), None);
    }

    #[test]
    fn a_model_call_resolves_in_app_models_then_app() {
        let model = "app/Models/User.php";
        let legacy = "app/Post.php";
        let ctx = RouteCtx::default()
            .routes(model, controller(model, "User", &["find"]))
            .routes(legacy, controller(legacy, "Post", &["latest"]));
        assert_eq!(
            target(&ctx, &action("User::find")),
            Some((id(NodeKind::Method, "find", model, 3), 0.85))
        );
        assert_eq!(
            target(&ctx, &action("User::where")),
            Some((id(NodeKind::Class, "User", model, 1), 0.85))
        );
        assert_eq!(
            target(&ctx, &action("Post::latest")),
            Some((id(NodeKind::Method, "latest", legacy, 3), 0.85))
        );
        assert_eq!(target(&ctx, &action("Comment::find")), None);
        // Facades are Laravel's.
        assert_eq!(target(&ctx, &action("Auth::user")), None);
    }

    #[test]
    fn claims_only_a_route_s_references() {
        let reference = |name: &str, from: &str| RefView {
            row_id: None,
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 2,
            column: 0,
            file_path: "routes/web.php".to_string(),
            language: Language::Php,
            is_function_ref: false,
            reference_subkind: None,
        };
        let route = "route:routes/web.php:2:GET:/users";
        assert!(LaravelResolver.claims(&reference("UserController@index", route)));
        assert!(LaravelResolver.claims(&reference(r"Admin\UserController", route)));
        assert!(!LaravelResolver.claims(&reference("UserController", route)));
        // A PHP `use` import, or a call, of the same shapes.
        let namespace = "namespace:app/Models/User.php:App:2";
        assert!(!LaravelResolver.claims(&reference(r"App\Models\User", namespace)));
        assert!(!LaravelResolver.claims(&reference("UserController@index", namespace)));
        let mut yaml = reference("UserController@index", route);
        yaml.language = Language::Yaml;
        assert!(!LaravelResolver.claims(&yaml));
    }

    #[test]
    fn detects_by_a_root_artisan_or_http_kernel() {
        assert!(
            LaravelResolver.detect(&RouteCtx::default().unlisted("artisan", "#!/usr/bin/env php"))
        );
        assert!(LaravelResolver.detect(&RouteCtx::default().file("app/Http/Kernel.php", "<?php")));
        assert!(!LaravelResolver.detect(&RouteCtx::default().file("sub/artisan", "")));
        assert!(!LaravelResolver.detect(&RouteCtx::default().file("composer.json", "{}")));
    }

    // -- mounts ---------------------------------------------------------------

    /// The route names once `files` are indexed and mounted.
    fn mounted(files: &[(&str, &str)]) -> Vec<String> {
        let mut ctx = RouteCtx::default();
        for (file, content) in files {
            ctx = ctx.file(file, content);
            if file.ends_with(".php") {
                ctx = ctx.routes(file, laravel(file, content).nodes);
            }
        }
        let updates = LaravelResolver.post_extract(&ctx).unwrap();
        let mut names: Vec<String> = ctx
            .nodes()
            .iter()
            .filter(|node| node.kind == NodeKind::Route)
            .map(|node| {
                updates
                    .iter()
                    .find(|update| update.id == node.id)
                    .map_or(node.name.clone(), |update| update.name.clone())
            })
            .collect();
        names.sort();
        names
    }

    const API: &str = "<?php\nRoute::get('pages', [PageController::class, 'list']);\n";
    const WEB: &str = "<?php\nRoute::get('/login', [PageController::class, 'login']);\n";

    #[test]
    fn serves_routes_api_under_api_by_default() {
        assert_eq!(
            mounted(&[("routes/api.php", API), ("routes/web.php", WEB)]),
            ["GET /api/pages", "GET /login"]
        );
    }

    #[test]
    fn reads_a_route_service_provider_prefix() {
        let provider = "<?php
class RouteServiceProvider {
    public function boot() {
        Route::middleware('api')->prefix('api/v1')->group(base_path('routes/api.php'));
        Route::middleware('web')->group(base_path('routes/web.php'));
    }
}
";
        assert_eq!(
            mounted(&[
                ("routes/api.php", API),
                ("routes/web.php", WEB),
                ("app/Providers/RouteServiceProvider.php", provider),
            ]),
            ["GET /api/v1/pages", "GET /login"]
        );
    }

    #[test]
    fn reads_a_group_that_requires_the_file() {
        let provider = "<?php
class RouteServiceProvider {
    protected function mapApiRoutes() {
        Route::group(['middleware' => 'api', 'prefix' => 'api'], function ($router) {
            require base_path('routes/api.php');
        });
    }
}
";
        assert_eq!(
            mounted(&[
                ("routes/api.php", API),
                ("app/App/Providers/RouteServiceProvider.php", provider),
            ]),
            ["GET /api/pages"]
        );
    }

    #[test]
    fn reads_with_routing_api_and_api_prefix() {
        let app = "<?php
return Application::configure(basePath: dirname(__DIR__))
    ->withRouting(
        web: __DIR__.'/../routes/web.php',
        api: [__DIR__.'/../routes/api.php', base_path('routes/extra.php')],
        apiPrefix: 'api/v2',
        health: '/up',
    )
    ->create();
";
        assert_eq!(
            mounted(&[
                ("routes/api.php", API),
                (
                    "routes/extra.php",
                    "<?php\nRoute::get('extra', fn () => 1);\n"
                ),
                ("routes/web.php", WEB),
                ("bootstrap/app.php", app),
            ]),
            ["GET /api/v2/extra", "GET /api/v2/pages", "GET /login"]
        );
    }

    #[test]
    fn composes_prefixes_down_an_include_tree() {
        assert_eq!(
            mounted(&[
                (
                    "routes/api.php",
                    "<?php\nRoute::get('pages', [PageController::class, 'list']);\nRoute::prefix('v1')->group(base_path('routes/api/v1.php'));\n"
                ),
                (
                    "routes/api/v1.php",
                    "<?php\nRoute::get('songs', [PageController::class, 'list']);\n"
                ),
                (
                    "routes/web.php",
                    "<?php\nRoute::get('/', [PageController::class, 'list']);\nrequire(__DIR__.'/auth.php');\n"
                ),
                (
                    "routes/auth.php",
                    "<?php\nRoute::get('login', [PageController::class, 'login']);\n"
                ),
            ]),
            ["GET /", "GET /api/pages", "GET /api/v1/songs", "GET /login"]
        );
    }

    #[test]
    fn composes_prefixes_down_a_chain_of_three_mounts() {
        // Each round carries a prefix one mount further down.
        assert_eq!(
            mounted(&[
                (
                    "app/Providers/RouteServiceProvider.php",
                    "<?php\nRoute::prefix('admin')->group(base_path('routes/admin.php'));\n"
                ),
                (
                    "routes/admin.php",
                    "<?php\nRoute::prefix('users')->group(base_path('routes/admin/users.php'));\n"
                ),
                (
                    "routes/admin/users.php",
                    "<?php\nRoute::get('list', [PageController::class, 'list']);\nRoute::prefix('roles')->group(base_path('routes/admin/roles.php'));\n"
                ),
                (
                    "routes/admin/roles.php",
                    "<?php\nRoute::get('edit', [PageController::class, 'list']);\n"
                ),
            ]),
            ["GET /admin/users/list", "GET /admin/users/roles/edit"]
        );
    }

    #[test]
    fn a_custom_loader_leaves_routes_api_alone() {
        let app = "<?php
return Application::configure(basePath: dirname(__DIR__))
    ->withRouting(using: static function (): void { RouteLoader::load('api'); })
    ->create();
";
        assert_eq!(
            mounted(&[("routes/api.php", API), ("bootstrap/app.php", app)]),
            ["GET /pages"]
        );
    }

    #[test]
    fn a_file_mounted_at_two_prefixes_keeps_its_in_file_path() {
        let provider = "<?php
Route::prefix('a')->group(base_path('routes/shared.php'));
Route::prefix('b')->group(base_path('routes/shared.php'));
";
        assert_eq!(
            mounted(&[
                (
                    "routes/shared.php",
                    "<?php\nRoute::get('status', [PageController::class, 'list']);\n"
                ),
                ("app/Providers/RouteServiceProvider.php", provider),
            ]),
            ["GET /status"]
        );
    }

    #[test]
    fn a_mount_outside_routes_needs_a_needle_and_a_commented_one_is_none() {
        // A file outside `routes/` naming neither needle is never read; the
        // needles are looked for in its source, comments included.
        let provider = |comment: &str| {
            format!(
                "<?php\n{comment}\nRoute::prefix('p')->group(function () {{ $this->loadRoutesFrom(__DIR__.'/../web.php'); }});\n"
            )
        };
        let web = ("web.php", "<?php\nRoute::get('x', fn () => 1);\n");
        assert_eq!(
            mounted(&[web, ("app/Provider.php", provider("// none").as_str())]),
            ["GET /x"]
        );
        assert_eq!(
            mounted(&[
                web,
                ("app/Provider.php", provider("// see routes/").as_str())
            ]),
            ["GET /p/x"]
        );
        let commented = "<?php\n// Route::prefix('old')->group(base_path('routes/admin.php'));\n";
        assert_eq!(
            mounted(&[
                (
                    "routes/admin.php",
                    "<?php\nRoute::get('users', fn () => 1);\n"
                ),
                ("app/Providers/RouteServiceProvider.php", commented),
            ]),
            ["GET /users"]
        );
    }

    #[test]
    fn a_mount_that_goes_away_is_undone() {
        let file = "routes/admin.php";
        let route = laravel(file, "<?php\nRoute::get('users', fn () => 1);\n")
            .nodes
            .remove(0);
        let mut renamed = route.clone();
        renamed.name = "GET /admin/users".to_string();
        // The provider no longer prefixes it.
        let ctx = RouteCtx::default()
            .file(file, "<?php\nRoute::get('users', fn () => 1);\n")
            .file(
                "app/Providers/RouteServiceProvider.php",
                "<?php\nRoute::group([], base_path('routes/admin.php'));\n",
            )
            .routes(file, vec![renamed]);
        let updates = LaravelResolver.post_extract(&ctx).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].name, "GET /users");
        assert_eq!(updates[0].id, route.id);
        assert_eq!(updates[0].qualified_name, route.qualified_name);
        // Without Laravel routes nothing is read.
        let empty = RouteCtx::default().file("routes/api.php", API);
        assert!(LaravelResolver.post_extract(&empty).unwrap().is_empty());
        assert!(empty.reads().is_empty());
    }

    #[test]
    fn only_laravel_routes_are_renamed() {
        let mut other = node(
            NodeKind::Route,
            "GET /x",
            "routes/web.yml",
            1,
            Language::Yaml,
        );
        other.qualified_name = "routes/web.yml::route:/x".to_string();
        other.id = "route:routes/web.yml:1:GET:/x".to_string();
        assert!(!is_laravel_route(&other));
        let mut php = other.clone();
        php.language = Language::Php;
        php.file_path = "routes/web.yml".to_string();
        assert!(is_laravel_route(&php));
        php.qualified_name = "routes/web.yml::GET:/x".to_string();
        assert!(!is_laravel_route(&php));
    }

    #[test]
    fn verb_of_reads_the_name_up_to_its_first_space() {
        assert_eq!(verb_of("GET /x y"), "GET");
        assert_eq!(verb_of("GETX"), "GET");
    }
}
