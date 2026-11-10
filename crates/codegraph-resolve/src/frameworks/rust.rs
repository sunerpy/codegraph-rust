//! Rust's axum and actix routes and conventions (upstream
//! `resolution/frameworks/rust.ts` at `e8ddc4e2`: #2359's handler sites,
//! #2224's and #2214's scope, #2225's name heuristic, #365's axum and actix
//! builder routes, #151's workspace crates, #89's attribute routes).
//!
//! A project is Rust's when its root holds a `Cargo.toml`.
//!
//! A `.rs` file's comment-stripped text gives three kinds of routes, each
//! with a reference to its handler by name: an attribute (`#[get("/users")]`,
//! actix's and rocket's) referencing the `fn` declared after it; an axum
//! `.route("/users", get(list_users).post(create_user))`, one route per
//! method router at the call's top level; and actix's builder API, a
//! `web::resource("/user/{id}")` chain's `web::get().to(get_user)` or
//! `.to(index)` (`ANY`), and an App-level `.route("/health",
//! web::get().to(health_check))`. An attribute route's reference sits on the
//! attribute's line; a builder route's where its handler's own name is
//! written, which is what the Rust scope rule reads.
//!
//! Resolution guesses what a Rust name conventionally names, in upstream's
//! order: a `…_handler` or `handle_…` function, a `…Service` or
//! `…Repository` struct or trait, a model's struct, each through the shared
//! name heuristic ([`super::name_heuristic`]) under the Rust scope rule
//! ([`crate::rust_scope`]); then a lowercase name's module, the root's
//! `src/{name}.rs` or `src/{name}/mod.rs`, else the `src/lib.rs` or
//! `src/main.rs` of the workspace crate the name gives
//! ([`super::cargo_workspace`]), which answers `0.95`.
//!
//! Rust's own (KNOWN_DIFFS, KEEP-RUST): a rule whose tier declines ends the
//! chain, as Spring's; the module rule links to the candidate's file node,
//! where upstream takes its first node; a builder route's reference column
//! is a byte column, as the scope rule reads it.
//!
//! Offsets are byte offsets. Comments are blanked byte for byte
//! ([`crate::strip_comments::strip_rust_comments_keeping_offsets`]), so an
//! offset into the stripped text indexes the source too; whatever upstream
//! measures in UTF-16 code units (an attribute route's end column, a
//! resource chain's window) is measured on the source at the same offsets
//! ([`OffsetText`]).

use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::JS_SPACE;
use super::name_heuristic::{NamePick, PickOptions, pick_by_name_outcome};
use super::offset_text::OffsetText;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::rust_scope::is_rust_name_in_scope;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver extracts and resolves (upstream `languages`).
const LANGUAGES: [Language; 1] = [Language::Rust];

/// The root file whose presence makes a project Rust's (upstream `detect`).
pub const DETECTION_FILE: &str = "Cargo.toml";

/// What a file's source holds when any of its scans finds a route: each
/// pattern spells one of them. The comment stripper only ever blanks text,
/// so the stripped text holds none of them when the source does not.
const ROUTE_NEEDLES: [&str; 3] = [".route", "#[", "web::resource"];

/// How far a resource's chain reaches past `web::resource(…)`, in UTF-16
/// code units (upstream's `after + 500`).
const RESOURCE_WINDOW: usize = 500;

/// The folders a convention prefers, as path substrings (upstream's
/// `…_DIRS`). Each starts with `/`, so a root folder is no preferred one.
const HANDLER_DIRS: [&str; 5] = [
    "/handlers/",
    "/handler/",
    "/api/",
    "/routes/",
    "/controllers/",
];
const SERVICE_DIRS: [&str; 4] = ["/services/", "/service/", "/repository/", "/domain/"];
const MODEL_DIRS: [&str; 6] = [
    "/models/",
    "/model/",
    "/entities/",
    "/entity/",
    "/domain/",
    "/types/",
];

const FUNCTION_KINDS: [NodeKind; 1] = [NodeKind::Function];
const SERVICE_KINDS: [NodeKind; 2] = [NodeKind::Struct, NodeKind::Trait];
const STRUCT_KINDS: [NodeKind; 1] = [NodeKind::Struct];

/// The module rule's confidence for a module of the root crate and for a
/// workspace crate's root (upstream: a manifest's crate name is trusted
/// above a name match's `0.7`).
const LOCAL_MODULE_CONFIDENCE: f64 = 0.6;
const WORKSPACE_CRATE_CONFIDENCE: f64 = 0.95;

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), and its `\b` is written `(?-u:\b)`.
fn rust_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("rust pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| rust_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// An attribute route, actix's or rocket's: `#[get("/path")]`.
    attribute_route = r#"#\[(get|post|put|patch|delete|head|options){S}*\({S}*["']([^"']+)["'][^\]]*\)\]"#;
    /// The `fn` an attribute route decorates: the next one declared on a
    /// line of its own (`pub(crate) fn` is passed over, as upstream's).
    attribute_fn = r"\n{S}*(?:pub{S}+)?(?:async{S}+)?fn{S}+([{W}]+)";
    /// An axum route's call.
    route_call = r"\.route{S}*\(";
    /// The path an axum route's arguments start with.
    route_path = r#"^{S}*"([^"]+)"{S}*,"#;
    /// A method router and its handler in an axum route's arguments.
    method_router = r"(?-u:\b)(get|post|put|patch|delete|head|options|trace){S}*\({S}*([A-Za-z_][{W}:]*)";
    /// An actix resource.
    resource = r#"web::resource{S}*\({S}*"([^"]+)"{S}*\)"#;
    /// A method's route in a resource's chain.
    resource_method = r"web::(get|post|put|patch|delete|head){S}*\({S}*\){S}*\.to{S}*\({S}*([A-Za-z_][{W}:]*)";
    /// A resource's own handler, every method's.
    resource_to = r"^{S}*\.to{S}*\({S}*([A-Za-z_][{W}:]*)";
    /// An App-level actix route.
    app_route = r#"\.route{S}*\({S}*"([^"]+)"{S}*,{S}*web::(get|post|put|patch|delete|head){S}*\({S}*\){S}*\.to{S}*\({S}*([A-Za-z_][{W}:]*)"#;
    /// A model's name (upstream's Pattern 3).
    model_name = r"^[A-Z][a-zA-Z]+$";
    /// A module's name (upstream's Pattern 4).
    module_name = r"^[a-z_]+$";
}

pub struct RustResolver;

impl FrameworkResolver for RustResolver {
    fn name(&self) -> &str {
        "rust"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `Cargo.toml`, indexed or not: a root control input of the
    /// sync's Rust kind.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context.file_exists(DETECTION_FILE)
    }

    /// Upstream's four conventions in order. The three name heuristics take
    /// only what the reference can name; one that finds no candidate hands
    /// over to the next, one that declines ends the chain.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        let in_scope = |node: &Node| is_rust_name_in_scope(node, reference, context);
        // A file is a module: a sibling file is another one, reached only
        // through a `use`.
        let options = PickOptions {
            same_directory: false,
            accept: Some(&in_scope),
        };
        let rules: [(bool, &[NodeKind], &[&str], f64); 3] = [
            // Pattern 1: a handler.
            (
                name.ends_with("_handler") || name.starts_with("handle_"),
                &FUNCTION_KINDS,
                &HANDLER_DIRS,
                0.8,
            ),
            // Pattern 2: a service or a repository.
            (
                name.ends_with("Service") || name.ends_with("Repository"),
                &SERVICE_KINDS,
                &SERVICE_DIRS,
                0.8,
            ),
            // Pattern 3: a model, whatever the reference's kind.
            (model_name().is_match(name), &STRUCT_KINDS, &MODEL_DIRS, 0.7),
        ];
        for (applies, kinds, dirs, confidence) in rules {
            if !applies {
                continue;
            }
            match pick_by_name_outcome(
                reference,
                kinds,
                &|file| dirs.iter().any(|dir| file.contains(dir)),
                context,
                &options,
            ) {
                NamePick::Picked(target) => {
                    return Some(framework_ref(reference, target, confidence));
                }
                NamePick::Declined => return None,
                NamePick::NoCandidate => {}
            }
        }
        // Pattern 4: a module.
        if module_name().is_match(name) {
            let (target, from_workspace) = module_target(name, context)?;
            let confidence = if from_workspace {
                WORKSPACE_CRATE_CONFIDENCE
            } else {
                LOCAL_MODULE_CONFIDENCE
            };
            return Some(framework_ref(reference, target, confidence));
        }
        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if !file_path.ends_with(".rs")
            || !ROUTE_NEEDLES.iter().any(|needle| content.contains(needle))
        {
            return Some(FrameworkResolverExtractionResult::default());
        }
        let text = OffsetText::rust(content);
        let source = Source {
            text: &text,
            file_path,
        };
        let mut result = FrameworkResolverExtractionResult::default();
        source.attribute_routes(&mut result);
        source.axum_routes(&mut result);
        source.resource_routes(&mut result);
        source.app_routes(&mut result);
        Some(result)
    }
}

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// The file node of the module a lowercase name names (upstream
/// `resolveModule`), with whether it is a workspace crate's: the first
/// indexed of the root's `src/{name}.rs` and `src/{name}/mod.rs`, then of
/// the `src/lib.rs` and `src/main.rs` of the crate the workspace names
/// `name`. Upstream takes the candidate's first node, a module node first;
/// no Rust extraction writes module nodes, so it is the file node but where
/// a declaration's id sorts before it on the file's first line
/// (KEEP-RUST).
fn module_target(name: &str, context: &dyn ResolutionContext) -> Option<(String, bool)> {
    let file_node = |path: &String| {
        context
            .get_nodes_in_file_shared(path)
            .iter()
            .find(|node| node.kind == NodeKind::File)
            .map(|file| file.id.clone())
    };
    if let Some(target) = [format!("src/{name}.rs"), format!("src/{name}/mod.rs")]
        .iter()
        .find_map(file_node)
    {
        return Some((target, false));
    }
    let crates = context.cargo_workspace_crates();
    let dir = crates.get(name)?;
    [format!("{dir}/src/lib.rs"), format!("{dir}/src/main.rs")]
        .iter()
        .find_map(file_node)
        .map(|target| (target, true))
}

/// A handler expression's last `::` segment that is not empty (upstream
/// `split('::').filter(Boolean).pop()`).
fn handler_name(expression: &str) -> Option<&str> {
    expression.rsplit("::").find(|segment| !segment.is_empty())
}

/// The `)` matching the `(` at `open`, counting every parenthesis
/// (upstream `findMatchingParen`, which reads no strings).
fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i64;
    for (at, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// The `.rs` file an extraction reads.
struct Source<'s> {
    text: &'s OffsetText<'s>,
    file_path: &'s str,
}

impl Source<'_> {
    /// A capture of the stripped text as upstream's holds it.
    fn capture(&self, found: Option<regex::Match<'_>>, base: usize) -> String {
        found.map_or_else(String::new, |found| {
            self.text.upstream(base + found.start(), base + found.end())
        })
    }

    /// A route node for `method` and `path` on `line`, from column 0 to
    /// `end_column`, and its id.
    fn route(
        &self,
        result: &mut FrameworkResolverExtractionResult,
        method: &str,
        path: &str,
        line: i64,
        end_column: i64,
    ) -> String {
        let id = format!("route:{}:{line}:{method}:{path}", self.file_path);
        result.nodes.push(framework_node(
            id.clone(),
            NodeKind::Route,
            format!("{method} {path}"),
            format!("{}::route:{path}", self.file_path),
            self.file_path.to_string(),
            line,
            line,
            0,
            end_column,
            Language::Rust,
            false,
        ));
        id
    }

    /// A route's reference to its handler `name` at `line`:`column`.
    fn reference(
        &self,
        result: &mut FrameworkResolverExtractionResult,
        from: String,
        name: &str,
        line: i64,
        column: i64,
    ) {
        result.references.push(RefView {
            row_id: None,
            from_node_id: from,
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line,
            column,
            file_path: self.file_path.to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        });
    }

    /// Where a handler's own name is written (upstream `handlerSite`): the
    /// handler `name`, the last segment of the expression `expression`
    /// ending at `end`, on its line and at its byte column there.
    fn handler_site(&self, end: usize, expression: &str, name: &str) -> (i64, i64) {
        let at = end - expression.len() + expression.rfind(name).unwrap_or(0);
        let line_start = self.text.safe[..at].rfind('\n').map_or(0, |cut| cut + 1);
        (self.text.line_at(at), (at - line_start) as i64)
    }

    /// `#[get("/path")]`: a route on the attribute's line, from column 0 to
    /// the attribute's end, referencing the next `fn` declared after it.
    fn attribute_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        for found in attribute_route().captures_iter(&text.safe) {
            let whole = found.get(0).expect("whole match");
            let method = found[1].to_ascii_uppercase();
            let path = self.capture(found.get(2), 0);
            let line = text.line_at(whole.start());
            let id = self.route(
                result,
                &method,
                &path,
                line,
                text.units(whole.start(), whole.end()),
            );
            if let Some(handler) = attribute_fn().captures(&text.safe[whole.end()..]) {
                self.reference(result, id, &handler[1], line, 0);
            }
        }
    }

    /// `.route("/path", get(a).post(b))`: a route on the `.route` line for
    /// each method router at the arguments' top level, a call inside a
    /// closure handler (`get(|| async { cache.get(key) })`) being none.
    fn axum_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let safe = &self.text.safe;
        for call in route_call().find_iter(safe) {
            let Some(open) = safe[call.start()..].find('(').map(|at| call.start() + at) else {
                continue;
            };
            let Some(close) = matching_paren(safe, open) else {
                continue;
            };
            let arguments = &safe[open + 1..close];
            let Some(path) = route_path().captures(arguments) else {
                continue;
            };
            let route_path = self.capture(path.get(1), open + 1);
            let line = self.text.line_at(call.start());
            let body_at = open + 1 + path.get(0).expect("whole match").end();
            let body = &safe[body_at..close];
            let mut depth = 0i64;
            let mut scanned = 0;
            for router in method_router().captures_iter(body) {
                let whole = router.get(0).expect("whole match");
                for byte in &body.as_bytes()[scanned..whole.start()] {
                    match byte {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                }
                scanned = whole.start();
                if depth > 0 {
                    continue;
                }
                let method = router[1].to_ascii_uppercase();
                let expression = router.get(2).expect("handler");
                let Some(handler) = handler_name(expression.as_str()) else {
                    continue;
                };
                let (handler_line, column) =
                    self.handler_site(body_at + expression.end(), expression.as_str(), handler);
                let id = self.route(result, &method, &route_path, line, 0);
                self.reference(result, id, handler, handler_line, column);
            }
        }
    }

    /// `web::resource("/path")`'s chain, up to [`RESOURCE_WINDOW`] units on,
    /// the next resource, or the `)` of the call it is an argument of:
    /// each `web::get().to(handler)` a route on its own line; with none, a
    /// leading `.to(handler)` the `ANY` route on the resource's line.
    fn resource_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        let safe = &text.safe;
        for found in resource().captures_iter(safe) {
            let whole = found.get(0).expect("whole match");
            let route_path = self.capture(found.get(1), 0);
            let start_line = text.line_at(whole.start());
            let after = whole.end();
            let next = safe[after..]
                .find("web::resource")
                .map_or(safe.len(), |at| after + at);
            let mut end = text.offset_after_units(after, RESOURCE_WINDOW).min(next);
            let mut depth = 0i64;
            for (at, byte) in safe.as_bytes()[after..end].iter().enumerate() {
                match byte {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth < 0 {
                            end = after + at;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let chain = &safe[after..end];
            let mut any_method = false;
            for route in resource_method().captures_iter(chain) {
                let at = route.get(0).expect("whole match");
                let line =
                    start_line + chain[..at.start()].bytes().filter(|&b| b == b'\n').count() as i64;
                self.builder_route(
                    result,
                    &route_path,
                    &route[1].to_ascii_uppercase(),
                    route.get(2).expect("handler").as_str(),
                    line,
                    after + at.end(),
                );
                any_method = true;
            }
            if !any_method && let Some(direct) = resource_to().captures(chain) {
                self.builder_route(
                    result,
                    &route_path,
                    "ANY",
                    direct.get(1).expect("handler").as_str(),
                    start_line,
                    after + direct.get(0).expect("whole match").end(),
                );
            }
        }
    }

    /// `.route("/path", web::get().to(handler))`: a route on the `.route`
    /// line.
    fn app_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        for found in app_route().captures_iter(&text.safe) {
            let whole = found.get(0).expect("whole match");
            let route_path = self.capture(found.get(1), 0);
            self.builder_route(
                result,
                &route_path,
                &found[2].to_ascii_uppercase(),
                found.get(3).expect("handler").as_str(),
                text.line_at(whole.start()),
                whole.end(),
            );
        }
    }

    /// An actix builder route (upstream `pushActixRoute`): the handler is
    /// the expression's last segment, referenced where it is written; the
    /// expression ends at `end`.
    fn builder_route(
        &self,
        result: &mut FrameworkResolverExtractionResult,
        path: &str,
        method: &str,
        expression: &str,
        line: i64,
        end: usize,
    ) {
        let Some(handler) = handler_name(expression) else {
            return;
        };
        let (handler_line, column) = self.handler_site(end, expression, handler);
        let id = self.route(result, method, path, line, 0);
        self.reference(result, id, handler, handler_line, column);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn extract(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        RustResolver
            .extract(
                file,
                src,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .expect("an extraction result")
    }

    fn names(result: &FrameworkResolverExtractionResult) -> Vec<&str> {
        result.nodes.iter().map(|node| node.name.as_str()).collect()
    }

    fn handlers(result: &FrameworkResolverExtractionResult) -> Vec<&str> {
        result
            .references
            .iter()
            .map(|reference| reference.reference_name.as_str())
            .collect()
    }

    /// Each reference's route name and handler name, in order.
    fn routes(result: &FrameworkResolverExtractionResult) -> Vec<(String, String)> {
        result
            .references
            .iter()
            .map(|reference| {
                let route = result
                    .nodes
                    .iter()
                    .find(|node| node.id == reference.from_node_id)
                    .expect("the reference's route");
                (route.name.clone(), reference.reference_name.clone())
            })
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(route, handler)| (route.to_string(), handler.to_string()))
            .collect()
    }

    /// Upstream `rust-route-handler-line.test.ts`.
    const MAIN_RS: &str = "mod actix_app;
mod handlers;
mod keys;
use axum::{routing::{delete, get, post}, Router};
use crate::handlers::used_handler;

pub fn app() -> Router {
    Router::new()
        .route(\"/single\", get(handlers::single_line_handler))
        .route(
            \"/multi\",
            get(handlers::multi_line_handler),
        )
        .route(
            \"/deep\",
            get(
                handlers::deep_line_handler,
            ),
        )
        .route(
            \"/items\",
            get(handlers::list_items)
                .post(handlers::create_item)
                .delete(handlers::delete_item),
        )
        .route(
            \"/used\",
            post(used_handler),
        )
        .route(
            \"/other\",
            get(metrics::render_handler),
        )
        .route(\"/login\", post(handlers::login))
        .route(\"/account\", delete(handlers::delete))
        .route(\"/closure-inline\", get(|| async { cache().get(keys::session_key) }))
        .route(
            \"/closure\",
            get(|| async move {
                cache().get(keys::session_key)
            }),
        )
}
";

    const ACTIX_RS: &str = "use actix_web::{web, App};
use crate::handlers;

pub fn actix() {
    App::new()
        .service(
            web::resource(\"/index/with/a/rather/long/path/{id}\")
                .to(handlers::actix_index),
        )
        .service(
            web::resource(\"/user/{id}\")
                .route(
                    web::get()
                        .to(handlers::actix_get_user),
                ),
        )
        .route(
            \"/hey\",
            web::get().to(handlers::actix_hello),
        );
}
";

    // -- Upstream frameworks.test.ts 1415-1466 and 2162-2171 -------------

    #[test]
    fn extracts_an_axum_route_with_get() {
        let result = extract(
            "main.rs",
            "let app = Router::new().route(\"/users\", get(list_users));\n",
        );
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(handlers(&result), ["list_users"]);
    }

    #[test]
    fn extracts_every_method_of_a_chained_axum_route() {
        let result = extract(
            "main.rs",
            "let app = Router::new().route(\"/user\", get(get_current_user).put(update_user));\n",
        );
        assert_eq!(names(&result), ["GET /user", "PUT /user"]);
        assert_eq!(handlers(&result), ["get_current_user", "update_user"]);
    }

    #[test]
    fn extracts_a_multi_line_axum_route_with_a_namespaced_handler() {
        let result = extract(
            "main.rs",
            "\nlet app = Router::new()\n    .route(\n        \"/articles/feed\",\n        get(listing::feed_articles),\n    );\n",
        );
        assert_eq!(names(&result), ["GET /articles/feed"]);
        assert_eq!(handlers(&result), ["feed_articles"]);
    }

    #[test]
    fn extracts_an_actix_resource_route() {
        let result = extract(
            "main.rs",
            "App::new().service(web::resource(\"/user/{id}\").route(web::get().to(get_user)))\n",
        );
        assert_eq!(names(&result), ["GET /user/{id}"]);
        assert_eq!(handlers(&result), ["get_user"]);
    }

    #[test]
    fn extracts_an_actix_resource_to_for_every_method() {
        let result = extract(
            "main.rs",
            "App::new().service(web::resource(\"/\").to(index))\n",
        );
        assert_eq!(names(&result), ["ANY /"]);
        assert_eq!(handlers(&result), ["index"]);
    }

    #[test]
    fn extracts_an_actix_app_level_route() {
        let result = extract(
            "main.rs",
            "App::new().route(\"/health\", web::get().to(health_check))\n",
        );
        assert_eq!(names(&result), ["GET /health"]);
        assert_eq!(handlers(&result), ["health_check"]);
    }

    #[test]
    fn skips_routes_in_line_and_nested_block_comments() {
        let result = extract(
            "main.rs",
            "\n// .route(\"/fake\", get(fake_handler))\n/* outer /* inner .route(\"/inner-fake\", get(x)) */ still .route(\"/outer-fake\", get(y)) */\nlet app = Router::new().route(\"/real\", get(list_users));\n",
        );
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(handlers(&result), ["list_users"]);
    }

    // -- Upstream rust-route-handler-line.test.ts (#2359) ----------------

    #[test]
    fn the_route_stays_on_its_line_and_the_reference_moves_to_the_handler() {
        let result = extract("src/main.rs", MAIN_RS);
        let lines: Vec<&str> = MAIN_RS.split('\n').collect();
        let line_of = |text: &str| {
            lines
                .iter()
                .position(|line| line.contains(text))
                .expect("the line") as i64
                + 1
        };
        let route = |name: &str| {
            result
                .nodes
                .iter()
                .find(|node| node.name == name)
                .expect("the route")
        };
        let reference = |name: &str| {
            result
                .references
                .iter()
                .find(|reference| reference.reference_name == name)
                .expect("the reference")
        };
        assert_eq!(route("GET /multi").start_line, line_of("\"/multi\"") - 1);
        let multi = reference("multi_line_handler");
        assert_eq!(multi.line, line_of("handlers::multi_line_handler"));
        let column = usize::try_from(multi.column).expect("a column");
        assert!(lines[multi.line as usize - 1][column..].starts_with("multi_line_handler"));
        assert_eq!(
            reference("single_line_handler").line,
            route("GET /single").start_line
        );
        let deep = reference("deep_line_handler");
        assert_eq!(deep.line, line_of("handlers::deep_line_handler,"));
        assert_eq!(route("GET /deep").start_line, line_of("\"/deep\"") - 1);
    }

    #[test]
    fn axum_routes_are_the_top_level_method_routers() {
        let result = extract("src/main.rs", MAIN_RS);
        assert_eq!(
            routes(&result),
            pairs(&[
                ("GET /single", "single_line_handler"),
                ("GET /multi", "multi_line_handler"),
                ("GET /deep", "deep_line_handler"),
                ("GET /items", "list_items"),
                ("POST /items", "create_item"),
                ("DELETE /items", "delete_item"),
                ("POST /used", "used_handler"),
                ("GET /other", "render_handler"),
                ("POST /login", "login"),
                ("DELETE /account", "delete"),
            ])
        );
        assert_eq!(result.nodes.len(), result.references.len());
    }

    /// A method router is a whole word (upstream's `\b`): `widget(` holds
    /// no `get(`.
    #[test]
    fn a_method_router_is_a_whole_word() {
        let result = extract(
            "main.rs",
            "let app = Router::new().route(\"/a\", widget(x).get(y));\n",
        );
        assert_eq!(routes(&result), pairs(&[("GET /a", "y")]));
    }

    #[test]
    fn a_call_inside_a_closure_handler_is_no_route() {
        let result = extract(
            "main.rs",
            "let app = Router::new().route(\"/c\", get(|| async { cache().get(keys::session_key) })).route(\"/d\", get(list_d));\n",
        );
        // Only the route a handler name gives.
        assert_eq!(names(&result), ["GET /d"]);
        assert_eq!(handlers(&result), ["list_d"]);
    }

    #[test]
    fn actix_routes_reference_their_handlers_where_they_are_written() {
        let result = extract("src/actix_app.rs", ACTIX_RS);
        assert_eq!(
            routes(&result),
            pairs(&[
                ("ANY /index/with/a/rather/long/path/{id}", "actix_index"),
                ("GET /user/{id}", "actix_get_user"),
                ("GET /hey", "actix_hello"),
            ])
        );
        let lines: Vec<&str> = ACTIX_RS.split('\n').collect();
        let line_of = |text: &str| {
            lines
                .iter()
                .position(|line| line.contains(text))
                .expect("the line") as i64
                + 1
        };
        let at = |route: &str| {
            let node = result
                .nodes
                .iter()
                .find(|node| node.name == route)
                .expect("the route");
            let reference = result
                .references
                .iter()
                .find(|reference| reference.from_node_id == node.id)
                .expect("the reference");
            (node.start_line, reference.line, reference.column)
        };
        let any = at("ANY /index/with/a/rather/long/path/{id}");
        assert_eq!(any.0, line_of("web::resource(\"/index"));
        assert_eq!(any.1, line_of(".to(handlers::actix_index)"));
        let user = at("GET /user/{id}");
        assert_eq!(user.0, line_of("web::get()"));
        assert_eq!(user.1, line_of("handlers::actix_get_user"));
        let hey = at("GET /hey");
        assert_eq!(hey.0, line_of("\"/hey\"") - 1);
        assert_eq!(hey.1, line_of("handlers::actix_hello"));
        for (line, column, name) in [
            (any.1, any.2, "actix_index"),
            (user.1, user.2, "actix_get_user"),
            (hey.1, hey.2, "actix_hello"),
        ] {
            let column = usize::try_from(column).expect("a column");
            assert!(
                lines[line as usize - 1][column..].starts_with(name),
                "{name}"
            );
        }
    }

    /// A resource's line counts from the resource's start, whatever lines
    /// its own call spans (upstream's `startLine` plus the chain's lines).
    #[test]
    fn a_resource_method_route_counts_its_line_from_the_resource() {
        let result = extract(
            "main.rs",
            "web::resource(\n    \"/a\"\n).route(web::post().to(post_a))\n",
        );
        assert_eq!(routes(&result), pairs(&[("POST /a", "post_a")]));
        assert_eq!(result.nodes[0].start_line, 1);
        assert_eq!(result.references[0].line, 3);
    }

    /// A resource's own handler leads its chain (upstream's `^\s*\.to`):
    /// one after a guard is none.
    #[test]
    fn a_resources_own_handler_leads_its_chain() {
        assert_eq!(
            routes(&extract(
                "main.rs",
                "web::resource(\"/x\").guard(g).to(h)\n"
            )),
            pairs(&[])
        );
        assert_eq!(
            routes(&extract("main.rs", "web::resource(\"/x\")\n    .to(h)\n")),
            pairs(&[("ANY /x", "h")])
        );
    }

    #[test]
    fn a_resource_with_a_method_route_has_no_any_route() {
        let result = extract(
            "main.rs",
            "App::new().service(web::resource(\"/a\").to(any_a).route(web::post().to(post_a)))\n",
        );
        assert_eq!(routes(&result), pairs(&[("POST /a", "post_a")]));
    }

    #[test]
    fn the_app_level_route_after_a_service_is_not_the_resources() {
        let result = extract(
            "main.rs",
            "App::new().service(web::resource(\"/a\").to(a)).route(\"/b\", web::get().to(b))\n",
        );
        assert_eq!(routes(&result), pairs(&[("ANY /a", "a"), ("GET /b", "b")]));
    }

    #[test]
    fn a_resource_chain_ends_at_the_next_resource() {
        let result = extract(
            "main.rs",
            "web::resource(\"/a\").route(web::get().to(a)) web::resource(\"/b\").route(web::post().to(b))\n",
        );
        assert_eq!(routes(&result), pairs(&[("GET /a", "a"), ("POST /b", "b")]));
    }

    /// The chain reaches 500 UTF-16 code units past the resource: an emoji
    /// is two of them and four bytes.
    #[test]
    fn a_resource_chain_reaches_500_utf16_units() {
        let source = |emoji: usize| {
            format!(
                "web::resource(\"/x\") /* {} */.to(late)\n",
                "\u{1F600}".repeat(emoji)
            )
        };
        assert_eq!(
            routes(&extract("main.rs", &source(240))),
            pairs(&[("ANY /x", "late")])
        );
        assert_eq!(routes(&extract("main.rs", &source(300))), pairs(&[]));
    }

    /// Upstream #89: `#[get("/path")]` and the `fn` after it.
    #[test]
    fn an_attribute_route_references_the_next_fn() {
        let src = "#[get(\"/caf\u{e9}\")]\npub(crate) fn skipped() {}\nasync fn next_one() {}\n";
        let result = extract("src/main.rs", src);
        assert_eq!(result.nodes.len(), 1);
        let node = &result.nodes[0];
        assert_eq!(node.id, "route:src/main.rs:1:GET:/caf\u{e9}");
        assert_eq!(node.name, "GET /caf\u{e9}");
        assert_eq!(node.qualified_name, "src/main.rs::route:/caf\u{e9}");
        assert_eq!(node.kind, NodeKind::Route);
        assert_eq!(node.language, Language::Rust);
        assert_eq!((node.start_line, node.end_line), (1, 1));
        // `#[get("/café")]` is fifteen UTF-16 code units and sixteen bytes.
        assert_eq!((node.start_column, node.end_column), (0, 15));
        assert_eq!(routes(&result), pairs(&[("GET /caf\u{e9}", "next_one")]));
        let reference = &result.references[0];
        assert_eq!(
            (reference.line, reference.column, reference.reference_kind),
            (1, 0, EdgeKind::References)
        );
        assert_eq!(reference.language, Language::Rust);
        // A path-form attribute is none.
        assert_eq!(
            names(&extract(
                "main.rs",
                "#[actix_web::get(\"/x\")]\nfn x() {}\n"
            )),
            Vec::<&str>::new()
        );
        // Every method upstream reads, either quote.
        assert_eq!(
            names(&extract(
                "main.rs",
                "#[post('/p')]\nfn p() {}\n#[options(\"/o\", guard = \"g\")]\npub async fn o() {}\n"
            )),
            ["POST /p", "OPTIONS /o"]
        );
    }

    /// Every method each route form reads (upstream's alternations), with
    /// the spacing each pattern allows around its parentheses, and an
    /// attribute's `pub fn`.
    #[test]
    fn every_method_each_route_form_reads() {
        let one = |route: String| vec![(route, "handler".to_string())];
        for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
            let src = format!("#[{method} ( '/a' )]\npub fn handler() {{}}\n");
            assert_eq!(
                routes(&extract("main.rs", &src)),
                one(format!("{} /a", method.to_ascii_uppercase())),
                "{src}"
            );
        }
        for method in [
            "get", "post", "put", "patch", "delete", "head", "options", "trace",
        ] {
            let src = format!("let app = Router::new().route ( \"/a\", {method} ( handler ) );\n");
            assert_eq!(
                routes(&extract("main.rs", &src)),
                one(format!("{} /a", method.to_ascii_uppercase())),
                "{src}"
            );
        }
        for method in ["get", "post", "put", "patch", "delete", "head"] {
            let upper = method.to_ascii_uppercase();
            let src = format!(
                "App::new().service(web::resource ( \"/a\" ).route(web::{method} ( ) .to ( handler )))\n"
            );
            assert_eq!(
                routes(&extract("main.rs", &src)),
                one(format!("{upper} /a")),
                "{src}"
            );
            let src = format!("App::new().route ( \"/b\", web::{method} ( ) .to ( handler ) )\n");
            assert_eq!(
                routes(&extract("main.rs", &src)),
                one(format!("{upper} /b")),
                "{src}"
            );
        }
        // A resource's own handler, spaced too.
        assert_eq!(
            routes(&extract(
                "main.rs",
                "web::resource ( \"/c\" ) .to ( handler )\n"
            )),
            one("ANY /c".to_string())
        );
        // No other method is read.
        for src in [
            "#[connect(\"/x\")]\nfn x() {}\n",
            "let app = Router::new().route(\"/x\", connect(x));\n",
            "web::resource(\"/x\").route(web::options().to(x))\n",
            "App::new().route(\"/x\", web::trace().to(x))\n",
        ] {
            assert_eq!(names(&extract("main.rs", src)), Vec::<&str>::new(), "{src}");
        }
    }

    /// A builder route's reference column is a byte column: the scope rule
    /// reads the reference's line at it (KEEP-RUST).
    #[test]
    fn a_builder_route_reference_column_is_a_byte_column() {
        for (src, handler) in [
            (
                "let app = Router::new().route(\"/caf\u{e9}\", get(handlers::list));\n",
                "list",
            ),
            (
                "App::new().route(\"/\u{e9}\", web::get().to(handlers::hey))\n",
                "hey",
            ),
            ("web::resource(\"/\u{e9}\").to(handlers::all)\n", "all"),
        ] {
            let result = extract("main.rs", src);
            let reference = &result.references[0];
            let bytes = src.find(&format!("::{handler})")).expect("the handler") + 2;
            let units = src[..bytes].encode_utf16().count();
            assert_ne!(bytes, units, "{src}");
            assert_eq!(reference.column, bytes as i64, "{src}");
            assert_eq!(reference.line, 1, "{src}");
        }
    }

    /// A handler's site is where its last segment is written (upstream
    /// `handlerSite`'s `lastIndexOf`): in `users::users`, the second.
    #[test]
    fn a_handler_site_is_its_last_segments() {
        for src in [
            "let app = Router::new().route(\"/u\", get(users::users));\n",
            "App::new().route(\"/u\", web::get().to(users::users))\n",
            "web::resource(\"/u\").to(users::users)\n",
        ] {
            let result = extract("main.rs", src);
            let reference = &result.references[0];
            assert_eq!(reference.reference_name, "users", "{src}");
            let last = src.rfind("users)").expect("the last segment");
            assert_eq!(reference.column, last as i64, "{src}");
        }
    }

    #[test]
    fn only_a_rust_file_with_a_route_needle_is_read() {
        let route = "let app = Router::new().route(\"/users\", get(list_users));\n";
        assert_eq!(names(&extract("main.rs", route)), ["GET /users"]);
        assert_eq!(names(&extract("main.go", route)), Vec::<&str>::new());
        assert_eq!(names(&extract("main.ts", route)), Vec::<&str>::new());
        assert_eq!(
            names(&extract("main.rs", "fn route() { get(list_users); }\n")),
            Vec::<&str>::new()
        );
    }

    /// Two routes of one id are both written, and both references kept: the
    /// store keeps the later node, as upstream's upsert does.
    #[test]
    fn routes_of_one_id_keep_every_reference() {
        let result = extract(
            "main.rs",
            "let app = Router::new().route(\"/a\", get(x)).route(\"/a\", get(y));\n",
        );
        let ids: Vec<&str> = result.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(ids, ["route:main.rs:1:GET:/a", "route:main.rs:1:GET:/a"]);
        assert_eq!(handlers(&result), ["x", "y"]);
    }

    // -- Resolution -------------------------------------------------------

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
            Language::Rust,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn file_node(file: &str) -> Node {
        let mut node = framework_node(
            format!("file:{file}"),
            NodeKind::File,
            file.rsplit('/').next().unwrap_or(file).to_string(),
            file.to_string(),
            file.to_string(),
            1,
            2,
            0,
            0,
            Language::Rust,
            false,
        );
        node.updated_at = 0;
        node
    }

    /// The source of the reference's file: its `use`s, then the line the
    /// reference is on, `name` written bare at column 4.
    fn referencing_source(uses: &[&str], name: &str) -> String {
        let mut source: String = uses.iter().map(|line| format!("{line}\n")).collect();
        source.push_str(&format!("    {name}();\n"));
        source
    }

    /// A reference named `name` of `kind` on the last line of
    /// [`referencing_source`] with `uses` before it.
    fn reference_in(file: &str, name: &str, kind: EdgeKind, uses: usize) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("file:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: uses as i64 + 1,
            column: 4,
            file_path: file.to_string(),
            language: Language::Rust,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    /// What `RustResolver` resolves `name` (written in `src/main.rs` under
    /// `uses`) to in a project of `nodes`, with its confidence.
    fn resolve_with(
        ctx: RouteCtx,
        uses: &[&str],
        name: &str,
        kind: EdgeKind,
    ) -> Option<(String, f64)> {
        let ctx = ctx.file("src/main.rs", &referencing_source(uses, name));
        RustResolver
            .resolve(&reference_in("src/main.rs", name, kind, uses.len()), &ctx)
            .map(|resolved| {
                assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
                (resolved.target_node_id, resolved.confidence)
            })
    }

    fn ctx_of(nodes: Vec<Node>) -> RouteCtx {
        nodes.into_iter().fold(RouteCtx::default(), |ctx, node| {
            let file = node.file_path.clone();
            ctx.routes(&file, vec![node])
        })
    }

    #[test]
    fn a_handler_is_a_function_the_handler_folders_prefer() {
        for name in ["list_handler", "handle_login"] {
            let preferred = node(NodeKind::Function, name, "src/handlers/users.rs", 3);
            let other = node(NodeKind::Function, name, "src/other/users.rs", 3);
            let ctx = ctx_of(vec![other, preferred.clone()]);
            let uses = [
                &*format!("use crate::handlers::users::{name};"),
                &*format!("use crate::other::users::{name};"),
            ];
            assert_eq!(
                resolve_with(ctx, &uses, name, EdgeKind::Calls),
                Some((preferred.id, 0.8)),
                "{name}"
            );
        }
        // Every folder upstream prefers.
        for dir in ["handlers", "handler", "api", "routes", "controllers"] {
            let preferred = node(
                NodeKind::Function,
                "list_handler",
                &format!("src/{dir}/a.rs"),
                3,
            );
            let other = node(NodeKind::Function, "list_handler", "src/other/a.rs", 3);
            let ctx = ctx_of(vec![other, preferred.clone()]);
            assert_eq!(
                resolve_with(
                    ctx,
                    &["use crate::x::list_handler;"],
                    "list_handler",
                    EdgeKind::Calls
                ),
                Some((preferred.id, 0.8)),
                "{dir}"
            );
        }
    }

    #[test]
    fn a_handler_is_no_method() {
        let method = node(NodeKind::Method, "list_handler", "src/handlers/users.rs", 3);
        let uses = ["use crate::handlers::users::list_handler;"];
        let ctx = ctx_of(vec![method.clone()]);
        assert_eq!(
            resolve_with(ctx, &uses, "list_handler", EdgeKind::Calls),
            None
        );
        // Beside the method, a function of the name is the sole candidate.
        let function = node(
            NodeKind::Function,
            "list_handler",
            "src/handlers/admin.rs",
            3,
        );
        let ctx = ctx_of(vec![method, function.clone()]);
        assert_eq!(
            resolve_with(ctx, &uses, "list_handler", EdgeKind::Calls),
            Some((function.id, 0.8))
        );
    }

    #[test]
    fn a_service_or_repository_is_a_struct_or_trait_the_service_folders_prefer() {
        for (name, kind) in [
            ("UserService", NodeKind::Struct),
            ("UserService", NodeKind::Trait),
            ("UserRepository", NodeKind::Struct),
            ("UserRepository", NodeKind::Trait),
        ] {
            for dir in ["services", "service", "repository", "domain"] {
                let preferred = node(kind, name, &format!("src/{dir}/user.rs"), 3);
                let other = node(kind, name, "src/legacy/user.rs", 3);
                let ctx = ctx_of(vec![other, preferred.clone()]);
                assert_eq!(
                    resolve_with(
                        ctx,
                        &[&*format!("use crate::x::{name};")],
                        name,
                        EdgeKind::References
                    ),
                    Some((preferred.id, 0.8)),
                    "{name} {kind:?} {dir}"
                );
            }
        }
        // An enum is neither.
        let ctx = ctx_of(vec![node(
            NodeKind::Enum,
            "UserService",
            "src/services/user.rs",
            3,
        )]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use crate::x::UserService;"],
                "UserService",
                EdgeKind::References
            ),
            None
        );
    }

    #[test]
    fn a_model_is_a_struct_the_model_folders_prefer_whatever_the_reference() {
        for dir in ["models", "model", "entities", "entity", "domain", "types"] {
            for kind in [
                EdgeKind::References,
                EdgeKind::Calls,
                EdgeKind::Instantiates,
            ] {
                let preferred = node(NodeKind::Struct, "User", &format!("src/{dir}/user.rs"), 3);
                let other = node(NodeKind::Struct, "User", "src/legacy/user.rs", 3);
                let ctx = ctx_of(vec![other, preferred.clone()]);
                assert_eq!(
                    resolve_with(ctx, &["use crate::x::User;"], "User", kind),
                    Some((preferred.id, 0.7)),
                    "{dir} {kind:?}"
                );
            }
        }
        // A trait is no model.
        let ctx = ctx_of(vec![node(NodeKind::Trait, "User", "src/models/user.rs", 3)]);
        assert_eq!(
            resolve_with(ctx, &["use crate::x::User;"], "User", EdgeKind::References),
            None
        );
    }

    #[test]
    fn a_name_with_a_digit_is_neither_a_model_nor_a_module() {
        let ctx = ctx_of(vec![
            node(NodeKind::Struct, "User2", "src/models/user.rs", 3),
            file_node("src/v2.rs"),
        ]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use crate::x::User2;"],
                "User2",
                EdgeKind::References
            ),
            None
        );
        let ctx = ctx_of(vec![file_node("src/v2.rs")]);
        assert_eq!(resolve_with(ctx, &[], "v2", EdgeKind::References), None);
        // Without the digit, each rule answers.
        let user = node(NodeKind::Struct, "User", "src/models/user.rs", 3);
        let ctx = ctx_of(vec![user.clone()]);
        assert_eq!(
            resolve_with(ctx, &["use crate::x::User;"], "User", EdgeKind::References),
            Some((user.id, 0.7))
        );
        let ctx = ctx_of(vec![file_node("src/v.rs")]);
        assert_eq!(
            resolve_with(ctx, &[], "v", EdgeKind::References),
            Some(("file:src/v.rs".to_string(), 0.6))
        );
    }

    /// A tier that declines ends the chain: two handlers in no preferred
    /// folder, though `src/list_handler.rs` would answer the module rule.
    #[test]
    fn a_declined_rule_ends_the_chain() {
        let ctx = ctx_of(vec![
            node(NodeKind::Function, "list_handler", "src/a/x.rs", 3),
            node(NodeKind::Function, "list_handler", "src/b/x.rs", 3),
            file_node("src/list_handler.rs"),
        ]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use crate::a::x::list_handler;"],
                "list_handler",
                EdgeKind::Calls
            ),
            None
        );
        // One of them alone is the pick.
        let sole = node(NodeKind::Function, "list_handler", "src/a/x.rs", 3);
        let ctx = ctx_of(vec![sole.clone(), file_node("src/list_handler.rs")]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use crate::a::x::list_handler;"],
                "list_handler",
                EdgeKind::Calls
            ),
            Some((sole.id, 0.8))
        );
    }

    /// A tier without a candidate hands over to the next rule.
    #[test]
    fn no_candidate_hands_over_to_the_module_rule() {
        let ctx = ctx_of(vec![file_node("src/list_handler.rs")]);
        assert_eq!(
            resolve_with(ctx, &[], "list_handler", EdgeKind::Calls),
            Some(("file:src/list_handler.rs".to_string(), 0.6))
        );
    }

    /// Upstream #2214: what the name heuristics pick is in the reference's
    /// scope. `use std::task::Context` makes `Context` std's.
    #[test]
    fn a_heuristic_takes_only_a_name_in_scope() {
        let ctx = ctx_of(vec![node(
            NodeKind::Struct,
            "Context",
            "src/models/context.rs",
            3,
        )]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use std::task::Context;"],
                "Context",
                EdgeKind::References
            ),
            None
        );
        let ctx = ctx_of(vec![node(
            NodeKind::Struct,
            "Context",
            "src/models/context.rs",
            3,
        )]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use crate::models::context::Context;"],
                "Context",
                EdgeKind::References
            ),
            Some(("struct:src/models/context.rs:Context:3".to_string(), 0.7))
        );
    }

    /// A sibling file is another module (upstream `sameDirectory: false`):
    /// no tier of the reference's directory comes before the preferred
    /// folders.
    #[test]
    fn a_sibling_file_is_no_tier_of_its_own() {
        let preferred = node(NodeKind::Struct, "UserService", "src/services/user.rs", 3);
        let sibling = node(NodeKind::Struct, "UserService", "src/user.rs", 3);
        let ctx = ctx_of(vec![sibling, preferred.clone()]);
        assert_eq!(
            resolve_with(
                ctx,
                &["use crate::x::UserService;"],
                "UserService",
                EdgeKind::References
            ),
            Some((preferred.id, 0.8))
        );
    }

    fn workspace_ctx(nodes: Vec<Node>) -> RouteCtx {
        ctx_of(nodes)
            .unlisted("Cargo.toml", "[workspace]\nmembers = [\"crates/beta\"]\n")
            .unlisted("crates/beta/Cargo.toml", "[package]\nname = \"beta\"\n")
    }

    #[test]
    fn a_module_of_the_root_comes_before_a_workspace_crate() {
        let ctx = workspace_ctx(vec![
            file_node("crates/beta/src/lib.rs"),
            file_node("src/beta.rs"),
        ]);
        assert_eq!(
            resolve_with(ctx, &[], "beta", EdgeKind::Imports),
            Some(("file:src/beta.rs".to_string(), 0.6))
        );
        let ctx = workspace_ctx(vec![
            file_node("crates/beta/src/lib.rs"),
            file_node("src/beta/mod.rs"),
        ]);
        assert_eq!(
            resolve_with(ctx, &[], "beta", EdgeKind::Imports),
            Some(("file:src/beta/mod.rs".to_string(), 0.6))
        );
        let ctx = workspace_ctx(vec![file_node("crates/beta/src/lib.rs")]);
        assert_eq!(
            resolve_with(ctx, &[], "beta", EdgeKind::Imports),
            Some(("file:crates/beta/src/lib.rs".to_string(), 0.95))
        );
        // `src/beta.rs` comes before `src/beta/mod.rs`.
        let ctx = workspace_ctx(vec![file_node("src/beta/mod.rs"), file_node("src/beta.rs")]);
        assert_eq!(
            resolve_with(ctx, &[], "beta", EdgeKind::Imports),
            Some(("file:src/beta.rs".to_string(), 0.6))
        );
    }

    /// The module rule's target is the candidate's file node, whichever node
    /// its file lists first (KEEP-RUST).
    #[test]
    fn a_module_is_its_file_node() {
        let ctx = ctx_of(vec![
            node(NodeKind::Constant, "LIMIT", "src/beta.rs", 1),
            file_node("src/beta.rs"),
        ]);
        assert_eq!(
            resolve_with(ctx, &[], "beta", EdgeKind::Imports),
            Some(("file:src/beta.rs".to_string(), 0.6))
        );
        // A file the index holds no file node of is no candidate.
        let ctx = ctx_of(vec![node(NodeKind::Constant, "LIMIT", "src/beta.rs", 1)]);
        assert_eq!(resolve_with(ctx, &[], "beta", EdgeKind::Imports), None);
    }

    /// Upstream `frameworks.test.ts` 1468-1772: a workspace crate's
    /// `src/lib.rs`, else its `src/main.rs`, by its name or its `_` alias.
    #[test]
    fn a_workspace_crate_is_its_lib_or_its_main() {
        let ctx = ctx_of(vec![file_node("crates/mytool-core/src/lib.rs")])
            .unlisted(
                "Cargo.toml",
                "\n[workspace]\nmembers = [\"crates/mytool-core\", \"crates/mytool-fetcher\"]\n",
            )
            .unlisted(
                "crates/mytool-core/Cargo.toml",
                "\n[package]\nname = \"mytool-core\"\nversion = \"0.1.0\"\n",
            );
        assert_eq!(
            resolve_with(ctx, &[], "mytool_core", EdgeKind::References),
            Some(("file:crates/mytool-core/src/lib.rs".to_string(), 0.95))
        );
        let ctx = ctx_of(vec![file_node("crates/mytool-runner/src/main.rs")])
            .unlisted(
                "Cargo.toml",
                "\n[workspace]\nmembers = [\n  \"crates/mytool-runner\",\n]\n",
            )
            .unlisted(
                "crates/mytool-runner/Cargo.toml",
                "\n[package]\nname = \"mytool-runner\"\nversion = \"0.1.0\"\n",
            );
        assert_eq!(
            resolve_with(ctx, &[], "mytool_runner", EdgeKind::References),
            Some(("file:crates/mytool-runner/src/main.rs".to_string(), 0.95))
        );
        // Both present: the library.
        let ctx = workspace_ctx(vec![
            file_node("crates/beta/src/main.rs"),
            file_node("crates/beta/src/lib.rs"),
        ]);
        assert_eq!(
            resolve_with(ctx, &[], "beta", EdgeKind::Imports),
            Some(("file:crates/beta/src/lib.rs".to_string(), 0.95))
        );
    }

    #[test]
    fn a_workspace_glob_member_is_a_crate() {
        let ctx = ctx_of(vec![
            file_node("crates/mytool-foo/src/lib.rs"),
            file_node("crates/mytool-bar/src/lib.rs"),
        ])
        .unlisted("Cargo.toml", "\n[workspace]\nmembers = [\"crates/*\"]\n")
        .unlisted(
            "crates/mytool-foo/Cargo.toml",
            "\n[package]\nname = \"mytool-foo\"\nversion = \"0.1.0\"\n",
        )
        .unlisted(
            "crates/mytool-bar/Cargo.toml",
            "\n[package]\nname = \"mytool-bar\"\nversion = \"0.1.0\"\n",
        )
        .dirs(".", &["crates"])
        .dirs("crates", &["mytool-foo", "mytool-bar"])
        .dirs("crates/mytool-foo", &["src"])
        .dirs("crates/mytool-bar", &["src"]);
        let ctx = ctx.file("src/main.rs", &referencing_source(&[], "mytool_foo"));
        for (name, target) in [
            ("mytool_foo", "file:crates/mytool-foo/src/lib.rs"),
            ("mytool_bar", "file:crates/mytool-bar/src/lib.rs"),
        ] {
            let resolved = RustResolver
                .resolve(
                    &reference_in("src/main.rs", name, EdgeKind::References, 0),
                    &ctx,
                )
                .expect("the crate");
            assert_eq!(resolved.target_node_id, target);
        }
        let ctx = ctx_of(vec![file_node("helix-core/src/lib.rs")])
            .unlisted("Cargo.toml", "\n[workspace]\nmembers = [\"helix-*\"]\n")
            .unlisted(
                "helix-core/Cargo.toml",
                "\n[package]\nname = \"helix-core\"\nversion = \"0.1.0\"\n",
            )
            .dirs(".", &["helix-core", "docs", "target"])
            .dirs("helix-core", &["src"]);
        assert_eq!(
            resolve_with(ctx, &[], "helix_core", EdgeKind::References),
            Some(("file:helix-core/src/lib.rs".to_string(), 0.95))
        );
    }

    #[test]
    fn a_project_is_rusts_by_its_root_manifest() {
        let ctx = RouteCtx::default().unlisted("Cargo.toml", "[package]\nname = \"x\"\n");
        assert!(RustResolver.detect(&ctx));
        let ctx = RouteCtx::default()
            .unlisted("crates/x/Cargo.toml", "[package]\nname = \"x\"\n")
            .file("crates/x/src/lib.rs", "pub fn f() {}\n");
        assert!(!RustResolver.detect(&ctx));
        assert_eq!(RustResolver.name(), "rust");
        assert_eq!(RustResolver.languages(), Some(&[Language::Rust][..]));
    }
}
