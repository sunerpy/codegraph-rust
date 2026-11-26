//! NestJS [`FrameworkResolver`] — ports
//! `upstream resolution/frameworks/nestjs.ts`.
//!
//! Handles decorator-based routing (HTTP / GraphQL / microservice / WebSocket),
//! provider→class resolution, and the cross-file `RouterModule.register([...])`
//! prefixing pass (`postExtract`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use crate::framework::FrameworkResolver;
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

const HTTP_METHODS: [&str; 8] = [
    "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All",
];
const GQL_OPS: [&str; 3] = ["Query", "Mutation", "Subscription"];

/// NestJS resolver (`nestjsResolver`, nestjs.ts:45-267).
pub struct NestjsResolver;

impl FrameworkResolver for NestjsResolver {
    fn name(&self) -> &str {
        "nestjs"
    }

    fn languages(&self) -> Option<&[Language]> {
        // `languages: ['typescript', 'javascript']` (nestjs.ts:47).
        const LANGS: [Language; 2] = [Language::TypeScript, Language::JavaScript];
        Some(&LANGS)
    }

    // Ports nestjsResolver.detect (nestjs.ts:49-89).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Any `@nestjs/*` dependency in a package.json: the root's, or an
        // app's (`apps/api/`, `server/`).
        if context
            .declared_dependencies()
            .iter()
            .any(|name| name.starts_with("@nestjs/"))
        {
            return true;
        }

        for file in context.get_all_files_shared().iter() {
            if file.ends_with(".controller.ts")
                || file.ends_with(".controller.js")
                || file.ends_with(".module.ts")
                || file.ends_with(".resolver.ts")
                || file.ends_with(".gateway.ts")
            {
                if let Some(content) = context.read_file(file) {
                    if content.contains("@nestjs/")
                        || content.contains("@Controller")
                        || content.contains("@Module(")
                        || content.contains("@Resolver(")
                        || content.contains("@WebSocketGateway(")
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    // Ports nestjsResolver.resolve (nestjs.ts:82-115).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        for (suffix, convention) in provider_conventions() {
            if !suffix.is_match(&reference.reference_name) {
                continue;
            }
            let candidates = context
                .get_nodes_by_name_shared(&reference.reference_name)
                .into_iter()
                .filter(|n| n.kind == NodeKind::Class)
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return None;
            }
            let resolved = |target: &Node| {
                Some(ResolvedRef::new(
                    reference.clone(),
                    target.id.clone(),
                    0.85,
                    ResolvedBy::Framework,
                ))
            };
            // A TS/JS module reaches another file's class only by importing
            // it, the import resolver's, so a guess by name stays in the
            // feature: the file itself, or the convention file beside it
            // (`users.controller.ts` -> `users.service.ts`). Every Nest
            // sample app declares its own `CatsService`, and the file-name
            // preference sent specs across apps (upstream #2246).
            if let Some(own) = candidates
                .iter()
                .find(|n| n.file_path == reference.file_path)
            {
                return resolved(own);
            }
            if script_module_regex().is_match(&reference.file_path) {
                let dir = match reference.file_path.rfind('/') {
                    Some(slash) => &reference.file_path[..=slash],
                    None => "",
                };
                return candidates
                    .iter()
                    .find(|n| {
                        n.file_path.contains(convention)
                            && n.file_path
                                .strip_prefix(dir)
                                .is_some_and(|rest| !rest.contains('/'))
                    })
                    .and_then(|beside| resolved(beside));
            }
            let preferred = candidates.iter().find(|n| n.file_path.contains(convention));
            let (target, confidence) = match preferred {
                Some(node) => (node, 0.85),
                None => (&candidates[0], 0.7),
            };
            return Some(ResolvedRef::new(
                reference.clone(),
                target.id.clone(),
                confidence,
                ResolvedBy::Framework,
            ));
        }
        None
    }

    // Ports nestjsResolver.extract (nestjs.ts:113-193).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &crate::framework::FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        Some(extract_routes(file_path, content))
    }

    /// Cross-file finalization (`postExtract`, nestjs.ts:199-297): every
    /// route is renamed by the path a request takes to it. A
    /// `RouterModule.register([...])` prefix applies to the routes of the
    /// controllers its module lists; the app's bootstrap (`NestFactory.create`)
    /// adds its global prefix and URI version to every route whose decorator
    /// sits in a `@Controller` (upstream #2117), in NestJS's order
    /// `/{prefix}/{v}{version}/{RouterModule prefix}/{path}`.
    ///
    /// A route keeps its `id` and `qualified_name`: route->handler edges
    /// reference the id, and the qualified name still reads the in-file
    /// `method:path` every name is computed from, which keeps the pass
    /// idempotent. A route an earlier pass renamed that nothing renames now
    /// (its app's prefix, version or RouterModule entry went away) gets its
    /// in-file name back, so a sync converges with a fresh index.
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let mut module_to_prefix: BTreeMap<String, String> = BTreeMap::new();
        let mut controller_to_module: BTreeMap<String, String> = BTreeMap::new();
        let mut bootstraps: Vec<NestBootstrap> = Vec::new();

        for file_path in context.get_all_files_shared().iter() {
            if !nest_source_regex().is_match(file_path) {
                continue;
            }
            let is_module = module_file_regex().is_match(file_path);
            let Some(content) = context.read_file_shared(file_path) else {
                continue;
            };
            if !is_module && !content.contains("NestFactory") {
                continue;
            }
            let safe = strip_comments_for_regex(&content, comment_lang_of(file_path));
            if is_module {
                collect_router_module_registrations(&safe, &mut module_to_prefix);
                collect_module_controllers(&safe, &mut controller_to_module);
            }
            // Every app counts, even one that adds nothing: its routes must
            // not borrow another app's prefix.
            if nest_factory_create_regex().is_match(&safe) {
                let test = codegraph_core::file_class::is_test_file(file_path);
                bootstraps.push(NestBootstrap {
                    root: bootstrap_root(file_path, test),
                    test,
                    config: read_bootstrap(&safe),
                });
            }
        }

        let mut controller_to_prefix: BTreeMap<String, String> = BTreeMap::new();
        for (controller, module) in &controller_to_module {
            if let Some(prefix) = module_to_prefix.get(module) {
                // `''` and `'/'` are no-op prefixes.
                if !prefix.is_empty() && prefix != "/" {
                    controller_to_prefix.insert(controller.clone(), prefix.clone());
                }
            }
        }

        // Route id -> what the app adds to it. A RouterModule prefix binds
        // through the controller class's line range; the bootstrap's prefix
        // and versioning only to a route its source shows is in a
        // `@Controller`.
        let mut plan: BTreeMap<String, Planned<'_>> = BTreeMap::new();
        for (controller_name, prefix) in &controller_to_prefix {
            for cls in context
                .get_nodes_by_name_shared(controller_name)
                .iter()
                .filter(|n| n.kind == NodeKind::Class)
            {
                for route in context.get_nodes_in_file_shared(&cls.file_path) {
                    if route.kind != NodeKind::Route
                        || route.start_line < cls.start_line
                        || route.start_line > cls.end_line
                    {
                        continue;
                    }
                    plan.insert(
                        route.id.clone(),
                        Planned {
                            route,
                            module_prefix: prefix.clone(),
                            config: None,
                            version: None,
                        },
                    );
                }
            }
        }

        let routes = context.get_nodes_by_kind_shared(NodeKind::Route);
        if bootstraps.iter().any(|b| b.config.adds()) {
            // Upstream walks every file holding `@Controller`; only one that
            // holds an HTTP route has anything to rename.
            let mut http_routes: BTreeMap<&str, Vec<&Arc<Node>>> = BTreeMap::new();
            for route in routes.iter() {
                if http_route_qn_regex().is_match(&route.qualified_name) {
                    http_routes
                        .entry(route.file_path.as_str())
                        .or_default()
                        .push(route);
                }
            }
            for (file_path, file_routes) in http_routes {
                if !nest_source_regex().is_match(file_path) {
                    continue;
                }
                let Some(config) =
                    bootstrap_for(&bootstraps, file_path).filter(|config| config.adds())
                else {
                    continue;
                };
                let Some(content) = context.read_file_shared(file_path) else {
                    continue;
                };
                if !content.contains("@Controller") {
                    continue;
                }
                let safe = strip_comments_for_regex(&content, comment_lang_of(file_path));
                let scopes = build_class_scopes(&safe);
                let hits = find_decorators(&safe, &HTTP_METHODS);
                let hit_lines: Vec<i64> =
                    hits.iter().map(|hit| line_at(&safe, hit.index)).collect();
                for route in file_routes {
                    let Some(hit) = hits
                        .iter()
                        .zip(&hit_lines)
                        .find(|(_, line)| **line == route.start_line)
                        .map(|(hit, _)| hit)
                    else {
                        continue;
                    };
                    let Some(scope) =
                        scope_for(&scopes, hit.index).filter(|s| s.kind == ClassKind::Controller)
                    else {
                        continue;
                    };
                    let module_prefix = plan
                        .get(&route.id)
                        .map(|planned| planned.module_prefix.clone())
                        .unwrap_or_default();
                    plan.insert(
                        route.id.clone(),
                        Planned {
                            route: Arc::clone(route),
                            module_prefix,
                            config: Some(config),
                            version: route_version(&safe, hit, scope),
                        },
                    );
                }
            }
        }

        let mut updates: Vec<Node> = Vec::new();
        for planned in plan.values() {
            if let Some(updated) = apply_app_config(
                &planned.route,
                &planned.module_prefix,
                planned.config,
                planned.version.as_ref(),
            ) && updated.name != planned.route.name
            {
                updates.push(updated);
            }
        }

        // A renamed route nothing renames now. Only a route this resolver's
        // extract makes in its file is ours to reset.
        let mut stale: BTreeMap<&str, Vec<(&Arc<Node>, String)>> = BTreeMap::new();
        for route in routes.iter() {
            if plan.contains_key(&route.id) {
                continue;
            }
            if let Some(in_file) = in_file_name(route)
                && route.name != in_file
            {
                stale
                    .entry(route.file_path.as_str())
                    .or_default()
                    .push((route, in_file));
            }
        }
        for (file_path, file_routes) in stale {
            let Some(content) = context.read_file_shared(file_path) else {
                continue;
            };
            let extracted = extract_routes(file_path, &content);
            for (route, in_file) in file_routes {
                if extracted.nodes.iter().any(|node| node.id == route.id) {
                    let mut updated = Node::clone(route);
                    updated.name = in_file;
                    updated.updated_at = super::now_millis();
                    updates.push(updated);
                }
            }
        }

        Some(updates)
    }
}

/// The routes and handler references one file declares (`extract`,
/// nestjs.ts:117-197).
fn extract_routes(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    if !nestjs_file_ext_regex().is_match(file_path) {
        return FrameworkResolverExtractionResult::default();
    }
    let lang = detect_language(file_path);
    let comment_lang = match lang {
        Language::TypeScript => CommentLang::TypeScript,
        _ => CommentLang::JavaScript,
    };
    let safe = strip_comments_for_regex(content, comment_lang);

    let mut nodes = Vec::new();
    let mut references = Vec::new();

    let scopes = build_class_scopes(&safe);

    // HTTP routes: method decorator path joined onto controller prefix
    // (nestjs.ts:159-164).
    for hit in find_decorators(&safe, &HTTP_METHODS) {
        let scope = scope_for(&scopes, hit.index);
        let prefix = match scope {
            Some(s) if s.kind == ClassKind::Controller => s.prefix.as_str(),
            _ => "",
        };
        let path = join_http_path(prefix, &parse_string_arg(&hit.args));
        add_route(
            &safe,
            file_path,
            lang,
            hit.index,
            &hit.name.to_uppercase(),
            &path,
            hit.length,
            method_name_after(&safe, hit.end),
            &mut nodes,
            &mut references,
        );
    }

    // GraphQL ops: only inside an @Resolver class (nestjs.ts:166-174).
    for hit in find_decorators(&safe, &GQL_OPS) {
        let scope = scope_for(&scopes, hit.index);
        let is_resolver = matches!(scope, Some(s) if s.kind == ClassKind::Resolver);
        if !is_resolver {
            continue;
        }
        let handler = method_name_after(&safe, hit.end);
        let name = parse_graphql_name(&hit.args, handler.as_deref());
        add_route(
            &safe,
            file_path,
            lang,
            hit.index,
            &hit.name.to_uppercase(),
            &name,
            hit.length,
            handler,
            &mut nodes,
            &mut references,
        );
    }

    // Microservice message/event handlers (nestjs.ts:176-181).
    for hit in find_decorators(&safe, &["MessagePattern", "EventPattern"]) {
        let verb = if hit.name == "EventPattern" {
            "EVENT"
        } else {
            "MESSAGE"
        };
        let handler = method_name_after(&safe, hit.end);
        let path = {
            let parsed = parse_string_arg(&hit.args);
            if !parsed.is_empty() {
                parsed
            } else {
                handler.clone().unwrap_or_default()
            }
        };
        add_route(
            &safe,
            file_path,
            lang,
            hit.index,
            verb,
            &path,
            hit.length,
            handler,
            &mut nodes,
            &mut references,
        );
    }

    // WebSocket handlers, prefixed with the gateway namespace (nestjs.ts:183-190).
    for hit in find_decorators(&safe, &["SubscribeMessage"]) {
        let scope = scope_for(&scopes, hit.index);
        let namespace = match scope {
            Some(s) if s.kind == ClassKind::Gateway => s.prefix.clone(),
            _ => String::new(),
        };
        let handler = method_name_after(&safe, hit.end);
        let event = {
            let parsed = parse_string_arg(&hit.args);
            if !parsed.is_empty() {
                parsed
            } else {
                handler.clone().unwrap_or_default()
            }
        };
        let path = if namespace.is_empty() {
            event
        } else {
            format!("{namespace}:{event}")
        };
        add_route(
            &safe,
            file_path,
            lang,
            hit.index,
            "WS",
            &path,
            hit.length,
            handler,
            &mut nodes,
            &mut references,
        );
    }

    FrameworkResolverExtractionResult { nodes, references }
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The app's bootstrap: global prefix and URI versioning (nestjs.ts:300-487)
// ---------------------------------------------------------------------------

/// What [`NestjsResolver::post_extract`] adds to one route.
struct Planned<'a> {
    route: Arc<Node>,
    /// The RouterModule prefix, `""` for none.
    module_prefix: String,
    /// The app's bootstrap config, when it adds something.
    config: Option<&'a NestAppConfig>,
    /// The route's own version: `None` when neither it nor its controller
    /// says (`undefined`), `Some(None)` when the value is not a literal.
    version: Option<Option<Version>>,
}

/// One version value (`versionValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Version {
    /// `VERSION_NEUTRAL`: served without a version segment.
    Neutral,
    Literal(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct NestAppConfig {
    /// `setGlobalPrefix('api')`: `""` when none, or not a literal.
    prefix: String,
    /// Paths the prefix skips (`exclude: ['health', { path: 'metrics', method }]`),
    /// without a leading `/`.
    excludes: Vec<String>,
    /// URI versioning is on (`enableVersioning({ type: VersioningType.URI })`,
    /// the default type).
    uri: bool,
    /// `defaultVersion: '1'`: the version a route without its own is served
    /// at; `None` for none.
    default_version: Option<Version>,
    /// The segment's prefix: `v` unless `prefix:` says otherwise (`false` is
    /// none).
    version_prefix: String,
}

impl NestAppConfig {
    /// Whether the app adds anything to a route's name.
    fn adds(&self) -> bool {
        !self.prefix.is_empty() || self.uri
    }
}

struct NestBootstrap {
    root: String,
    /// Whether a test (an e2e spec) creates this app, not its entry point.
    test: bool,
    config: NestAppConfig,
}

/// The app a file belongs to (`appRootOf`): the directory above its first
/// `src/` segment (`apps/api/`), else the repository (`""`).
fn app_root_of(file_path: &str) -> &str {
    src_root(file_path).unwrap_or("")
}

/// The directory above `file_path`'s first `src/` segment, when it has one.
fn src_root(file_path: &str) -> Option<&str> {
    let mut at = 0;
    for segment in file_path.split('/') {
        let next = at + segment.len() + 1;
        if segment == "src" && next <= file_path.len() {
            return Some(&file_path[..at]);
        }
        at = next;
    }
    None
}

/// The directories that make a file a test (`file_class::is_test_file`'s).
const TEST_DIRECTORIES: [&str; 5] = ["test", "tests", "__tests__", "spec", "e2e"];

/// The app the `NestFactory.create` in `file_path` serves. A test's (an e2e
/// spec's) with no `src/` segment above it serves its own feature: the
/// directory above its test folder (`integration/security/` for
/// `integration/security/e2e/csrf.spec.ts`), else the directory holding it
/// (KEEP-RUST). Upstream's `appRootOf` puts every such spec at the
/// repository, where nestjs/nest's integration apps all took one spec's
/// `setGlobalPrefix('api')`. Any other file's app is [`app_root_of`]'s.
fn bootstrap_root(file_path: &str, test: bool) -> String {
    if !test || src_root(file_path).is_some() {
        return app_root_of(file_path).to_string();
    }
    let mut at = 0;
    let mut segments = file_path.split('/').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            break;
        }
        if TEST_DIRECTORIES.contains(&segment) {
            return file_path[..at].to_string();
        }
        at += segment.len() + 1;
    }
    file_path[..at].to_string()
}

/// The config of the app that holds `file_path` (`bootstrapFor`): the
/// bootstrap with the longest root above it, else, for a file outside every
/// app (an Nx `libs/` controller), the app when there is only one. A seed or
/// migration script creates the same app without serving it, so the
/// bootstrap that configures the app is the one that serves it. A test's
/// bootstrap configures only an app no entry point creates (KEEP-RUST): an
/// e2e spec's `setGlobalPrefix` is not what `src/main.ts` serves.
fn bootstrap_for<'a>(
    bootstraps: &'a [NestBootstrap],
    file_path: &str,
) -> Option<&'a NestAppConfig> {
    let mut root: Option<&str> = None;
    for bootstrap in bootstraps {
        if file_path.starts_with(bootstrap.root.as_str())
            && root.is_none_or(|root| bootstrap.root.len() > root.len())
        {
            root = Some(&bootstrap.root);
        }
    }
    let root = match root {
        Some(root) => root,
        None => {
            let first = bootstraps.first()?;
            if bootstraps.iter().any(|b| b.root != first.root) {
                return None;
            }
            &first.root
        }
    };
    let mut here: Vec<&NestBootstrap> = bootstraps.iter().filter(|b| b.root == root).collect();
    if here.iter().any(|b| !b.test) {
        here.retain(|b| !b.test);
    }
    let first = here.first()?;
    Some(
        &here
            .iter()
            .find(|b| b.config.adds())
            .unwrap_or(first)
            .config,
    )
}

/// One version value (`versionValue`): `'2'`, `['1', '2']` (the first),
/// `VERSION_NEUTRAL`; `None` for anything else.
fn version_value(text: &str) -> Option<Version> {
    static NEUTRAL: OnceLock<Regex> = OnceLock::new();
    static LITERAL: OnceLock<Regex> = OnceLock::new();
    let t = text.trim();
    if NEUTRAL
        .get_or_init(|| Regex::new(r"^VERSION_NEUTRAL\b").expect("neutral"))
        .is_match(t)
    {
        return Some(Version::Neutral);
    }
    LITERAL
        .get_or_init(|| {
            Regex::new(r#"^\[?\s*(?:'([^'"`]*)'|"([^'"`]*)"|`([^'"`]*)`)"#)
                .expect("version literal")
        })
        .captures(t)
        .map(|c| Version::Literal(first_group(&c).to_string()))
}

/// The text of the first participating capture group after group 0: the
/// body of whichever quote an alternation of quoted forms matched.
fn first_group<'h>(captures: &regex::Captures<'h>) -> &'h str {
    captures
        .iter()
        .skip(1)
        .flatten()
        .next()
        .map_or("", |m| m.as_str())
}

/// The arguments of the first `call(...)` the regex finds, or `None`.
fn first_call_args<'s>(safe: &'s str, call: &Regex) -> Option<&'s str> {
    let found = call.find(safe)?;
    let open = found.end() - 1;
    let close = matching_close(safe, open);
    Some(if close > open as i64 {
        &safe[open + 1..close as usize]
    } else {
        ""
    })
}

/// `setGlobalPrefix` and `enableVersioning` from the file that calls
/// `NestFactory.create` (`readBootstrap`).
fn read_bootstrap(safe: &str) -> NestAppConfig {
    static PREFIX_CALL: OnceLock<Regex> = OnceLock::new();
    static PREFIX_LITERAL: OnceLock<Regex> = OnceLock::new();
    static EXCLUDE: OnceLock<Regex> = OnceLock::new();
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    static PATH_FIELD: OnceLock<Regex> = OnceLock::new();
    static VERSIONING_CALL: OnceLock<Regex> = OnceLock::new();
    static TYPE: OnceLock<Regex> = OnceLock::new();
    static DEFAULT_VERSION: OnceLock<Regex> = OnceLock::new();
    static VERSION_PREFIX: OnceLock<Regex> = OnceLock::new();

    let mut config = NestAppConfig {
        version_prefix: "v".to_string(),
        ..NestAppConfig::default()
    };
    let prefix_call =
        PREFIX_CALL.get_or_init(|| Regex::new(r"\.setGlobalPrefix\s*\(").expect("setGlobalPrefix"));
    if let Some(args) = first_call_args(safe, prefix_call)
        && let Some(literal) = PREFIX_LITERAL
            .get_or_init(|| {
                Regex::new(r#"^\s*(?:'([^'"`$]*)'|"([^'"`$]*)"|`([^'"`$]*)`)"#)
                    .expect("prefix literal")
            })
            .captures(args)
    {
        config.prefix = trim_slashes(first_group(&literal)).to_string();
        let exclude = EXCLUDE.get_or_init(|| Regex::new(r"\bexclude\s*:\s*\[").expect("exclude"));
        if let Some(found) = exclude.find(args) {
            let open = found.end() - 1;
            let close = matching_close(args, open);
            let list = if close > open as i64 {
                &args[open + 1..close as usize]
            } else {
                ""
            };
            let quoted = QUOTED.get_or_init(|| {
                Regex::new(r#"^(?:'([^'"]*)'|"([^'"]*)")$"#).expect("quoted exclude")
            });
            let path_field = PATH_FIELD.get_or_init(|| {
                Regex::new(r#"\bpath\s*:\s*(?:'([^'"]*)'|"([^'"]*)")"#).expect("exclude path")
            });
            for item in split_top_level(list) {
                let text = item.trim();
                if let Some(path) = quoted
                    .captures(text)
                    .or_else(|| path_field.captures(text))
                    .map(|c| first_group(&c).to_string())
                {
                    config
                        .excludes
                        .push(path.trim_start_matches('/').to_string());
                }
            }
        }
    }
    let versioning_call = VERSIONING_CALL
        .get_or_init(|| Regex::new(r"\.enableVersioning\s*\(").expect("enableVersioning"));
    if let Some(args) = first_call_args(safe, versioning_call) {
        let kind = TYPE
            .get_or_init(|| {
                Regex::new(r"\btype\s*:\s*VersioningType\s*\.\s*([A-Za-z0-9_]+)").expect("type")
            })
            .captures(args)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str());
        config.uri = kind.is_none_or(|kind| kind == "URI");
        if let Some(found) = DEFAULT_VERSION
            .get_or_init(|| Regex::new(r"\bdefaultVersion\s*:\s*").expect("defaultVersion"))
            .find(args)
        {
            config.default_version = version_value(&args[found.end()..]);
        }
        if let Some(prefix) = VERSION_PREFIX
            .get_or_init(|| {
                Regex::new(r#"\bprefix\s*:\s*(?:(false)|'([^'"`]*)'|"([^'"`]*)"|`([^'"`]*)`)"#)
                    .expect("version prefix")
            })
            .captures(args)
        {
            config.version_prefix = if prefix.get(1).is_some() {
                String::new()
            } else {
                first_group(&prefix).to_string()
            };
        }
    }
    config
}

/// A route excluded from the global prefix (`isExcluded`): `health`,
/// `health/(.*)`, `health{/*wildcard}`, `health/*`.
fn is_excluded(path: &str, excludes: &[String]) -> bool {
    static WILD: OnceLock<Regex> = OnceLock::new();
    let wild_re = WILD.get_or_init(|| {
        Regex::new(r"(?:\{/?\*[A-Za-z0-9_]*\}|/?\(\.\*\)|/?\*[A-Za-z0-9_]*)$").expect("wildcard")
    });
    let route = path.trim_start_matches('/');
    excludes.iter().any(|pattern| {
        let wild = wild_re.find(pattern);
        let base = match wild {
            Some(found) => pattern[..found.start()].trim_end_matches('/'),
            None => pattern.trim_end_matches('/'),
        };
        route == base
            || (wild.is_some()
                && route
                    .strip_prefix(base)
                    .is_some_and(|rest| rest.starts_with('/')))
    })
}

/// The version a route is served at (`routeVersion`): `@Version(…)` among
/// its own decorators, else its controller's (`@Version(…)` on the class,
/// `@Controller({ version })`). `None` when neither says.
fn route_version(safe: &str, hit: &DecoratorHit, scope: &ClassScope) -> Option<Option<Version>> {
    static VERSION: OnceLock<Regex> = OnceLock::new();
    static CLASS: OnceLock<Regex> = OnceLock::new();
    static IN_CONTROLLER: OnceLock<Regex> = OnceLock::new();
    let version_re =
        VERSION.get_or_init(|| Regex::new(r"@Version\s*\(([^)]*)\)").expect("@Version"));
    // The method's decorators: back through the ones stacked above this one…
    let from = decorator_chain_start(safe, hit.index, scope.start);
    // …and on to the handler itself, past the decorators between
    // (`@UseGuards(…)`, `@Version('2')`).
    let to = method_name_after(safe, hit.end)
        .and_then(|handler| handler_call_offset(&safe[hit.end..], &handler))
        .map_or(hit.end, |offset| hit.end + offset);
    if let Some(own) = version_re.captures(&safe[from..to]) {
        return Some(version_value(&own[1]));
    }
    // The class's decorators, on either side of `@Controller`.
    let class_at = CLASS
        .get_or_init(|| Regex::new(r"\bclass\b").expect("class"))
        .find_at(safe, scope.start)
        .map(|m| m.start());
    let head_end = class_at.unwrap_or_else(|| floor_char_boundary(safe, scope.start + 400));
    let head_start = decorator_chain_start(safe, scope.start, 0);
    let head = safe.get(head_start..head_end).unwrap_or("");
    if let Some(on_class) = version_re.captures(head) {
        return Some(version_value(&on_class[1]));
    }
    if let Some(in_controller) = IN_CONTROLLER
        .get_or_init(|| {
            Regex::new(r"@Controller\s*\(\s*\{[^)]*\bversion\s*:\s*([^,}]+)")
                .expect("version field")
        })
        .captures(head)
    {
        return Some(version_value(&in_controller[1]));
    }
    None
}

/// The byte offset of `handler(` in `text`, not preceded by `@`, an ASCII
/// word character or `$` (upstream's `(?<![@\w$])handler\s*\(`).
fn handler_call_offset(text: &str, handler: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find(handler) {
        let start = from + found;
        let free = start.checked_sub(1).is_none_or(|i| {
            !matches!(bytes[i], b'@' | b'$' | b'_') && !bytes[i].is_ascii_alphanumeric()
        });
        if free
            && text[start + handler.len()..]
                .trim_start_matches(char::is_whitespace)
                .starts_with('(')
        {
            return Some(start);
        }
        from = start + handler.len();
    }
    None
}

/// `index` clamped to `text`, moved back to a char boundary.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Where the stack of decorators ending at `at` begins (`decoratorChainStart`):
/// back over each `@Name(…)` / `@Name` above it, stepping over balanced
/// parentheses, since a decorator's arguments may hold an object
/// (`@ApiResponse({ status: 200 })`).
fn decorator_chain_start(safe: &str, at: usize, floor: usize) -> usize {
    let bytes = safe.as_bytes();
    // JavaScript's `\s`, for the ASCII characters it covers.
    let space = |b: u8| b.is_ascii_whitespace() || b == 0x0b;
    let name_char = |b: u8| b == b'$' || b == b'.' || b == b'_' || b.is_ascii_alphanumeric();
    let mut start = at;
    loop {
        let mut i = start;
        while i > floor && space(bytes[i - 1]) {
            i -= 1;
        }
        if i > 0 && bytes[i - 1] == b')' {
            let mut depth = 0i64;
            let mut j = i - 1;
            loop {
                if bytes[j] == b')' {
                    depth += 1;
                } else if bytes[j] == b'(' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                if j <= floor {
                    return start;
                }
                j -= 1;
            }
            if j < floor {
                return start;
            }
            i = j;
        }
        let mut name = i;
        while name > floor && name_char(bytes[name - 1]) {
            name -= 1;
        }
        if name == i || name == 0 || bytes[name - 1] != b'@' {
            return start;
        }
        start = name - 1;
    }
}

/// A route's name with everything the app adds to it (`applyAppConfig`), in
/// NestJS's order `/{globalPrefix}/{v}{version}/{RouterModule prefix}/{path}`,
/// recomputed from the in-file `method:path` of its qualified name.
fn apply_app_config(
    route: &Node,
    module_prefix: &str,
    config: Option<&NestAppConfig>,
    version: Option<&Option<Version>>,
) -> Option<Node> {
    let (method, path) = qualified_route(route)?;
    let inner = join_http_path(module_prefix, path);
    let path = match config {
        None => inner,
        Some(config) => {
            let version = match version {
                None => config.default_version.as_ref(),
                Some(own) => own.as_ref(),
            };
            let version_segment = match version {
                Some(Version::Literal(version)) if config.uri => {
                    format!("{}{version}", config.version_prefix)
                }
                _ => String::new(),
            };
            let prefix = if !config.prefix.is_empty() && !is_excluded(&inner, &config.excludes) {
                config.prefix.as_str()
            } else {
                ""
            };
            join_http_path(&join_http_path(prefix, &version_segment), &inner)
        }
    };
    let mut updated = route.clone();
    updated.name = format!("{method} {path}");
    updated.updated_at = super::now_millis();
    Some(updated)
}

/// The in-file `(method, path)` a route's qualified name records
/// (`file::GET:/users`).
fn qualified_route(route: &Node) -> Option<(&str, &str)> {
    let (_, tail) = route.qualified_name.split_once("::")?;
    tail.split_once(':')
}

/// The name this resolver's extract gives a route, read back from its
/// qualified name; `None` for a route of another verb (React's `route:`).
fn in_file_name(route: &Node) -> Option<String> {
    const VERBS: [&str; 14] = [
        "GET",
        "POST",
        "PUT",
        "PATCH",
        "DELETE",
        "HEAD",
        "OPTIONS",
        "ALL",
        "QUERY",
        "MUTATION",
        "SUBSCRIPTION",
        "MESSAGE",
        "EVENT",
        "WS",
    ];
    let (method, path) = qualified_route(route)?;
    VERBS.contains(&method).then(|| format!("{method} {path}"))
}

// ---------------------------------------------------------------------------
// Provider resolution conventions (nestjs.ts:273-283)
// ---------------------------------------------------------------------------

fn provider_conventions() -> &'static [(Regex, &'static str)] {
    static CONV: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CONV.get_or_init(|| {
        vec![
            (Regex::new(r"Service$").expect("service"), ".service."),
            (
                Regex::new(r"Controller$").expect("controller"),
                ".controller.",
            ),
            (Regex::new(r"Resolver$").expect("resolver"), ".resolver."),
            (Regex::new(r"Gateway$").expect("gateway"), ".gateway."),
            (
                Regex::new(r"Repository$").expect("repository"),
                ".repository.",
            ),
            (Regex::new(r"Guard$").expect("guard"), ".guard."),
            (
                Regex::new(r"Interceptor$").expect("interceptor"),
                ".interceptor.",
            ),
            (Regex::new(r"Pipe$").expect("pipe"), ".pipe."),
            (Regex::new(r"Module$").expect("module"), ".module."),
        ]
    })
}

// ---------------------------------------------------------------------------
// Route node construction
// ---------------------------------------------------------------------------

/// Ports the `addRoute` closure (nestjs.ts:121-154).
#[allow(clippy::too_many_arguments)]
fn add_route(
    safe: &str,
    file_path: &str,
    lang: Language,
    index: usize,
    method: &str,
    path: &str,
    length: usize,
    handler: Option<String>,
    nodes: &mut Vec<Node>,
    references: &mut Vec<RefView>,
) {
    let line = line_at(safe, index);
    let node = framework_node(
        format!("route:{file_path}:{line}:{method}:{path}"),
        NodeKind::Route,
        format!("{method} {path}"),
        format!("{file_path}::{method}:{path}"),
        file_path.to_string(),
        line,
        line,
        0,
        length as i64,
        lang,
        false,
    );
    let node_id = node.id.clone();
    nodes.push(node);
    if let Some(handler) = handler {
        references.push(RefView {
            row_id: None,
            from_node_id: node_id,
            reference_name: handler,
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: file_path.to_string(),
            language: lang,
            is_function_ref: false,
            reference_subkind: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Decorator scanning (nestjs.ts:289-407)
// ---------------------------------------------------------------------------

struct DecoratorHit {
    name: String,
    args: String,
    index: usize,
    end: usize,
    length: usize,
}

/// Find every `@Name(...)` decorator whose name is in `names`
/// (`findDecorators`, nestjs.ts:308-326).
fn find_decorators(safe: &str, names: &[&str]) -> Vec<DecoratorHit> {
    let bytes = safe.as_bytes();
    let re = decorator_regex(names);
    let mut hits = Vec::new();
    let mut search_from = 0usize;
    while search_from <= safe.len() {
        let Some(m) = re.find_at(safe, search_from) else {
            break;
        };
        let caps = re
            .captures_at(safe, m.start())
            .expect("captures at match start");
        let name = caps.get(1).expect("decorator name").as_str().to_string();
        // position of '(' (the last char of the match).
        let open_index = m.end() - 1;
        match read_args(bytes, open_index) {
            Some((args, end)) => {
                hits.push(DecoratorHit {
                    name,
                    args,
                    index: m.start(),
                    end,
                    length: end - m.start(),
                });
                search_from = end;
            }
            None => {
                search_from = m.end();
            }
        }
    }
    hits
}

/// `@(names)\s*\(`, compiled once per name list: the post-extract pass
/// scans controller files on every sync.
fn decorator_regex(names: &[&str]) -> Regex {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let pattern = format!(r"@({})\s*\(", names.join("|"));
    let mut cache = CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
        .entry(pattern)
        .or_insert_with_key(|pattern| Regex::new(pattern).expect("decorator regex"))
        .clone()
}

/// Read a balanced, string-aware `(...)` starting at `open_index`
/// (`readArgs`, nestjs.ts:333-358). Operates on bytes (the stripped source is
/// ASCII-safe for the structural characters; non-ASCII bytes pass through).
fn read_args(bytes: &[u8], open_index: usize) -> Option<(String, usize)> {
    if bytes.get(open_index) != Some(&b'(') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None;
    let mut i = open_index;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = Some(ch);
            i += 1;
            continue;
        }
        if ch == b'(' {
            depth += 1;
        } else if ch == b')' {
            depth -= 1;
            if depth == 0 {
                let args = String::from_utf8_lossy(&bytes[open_index + 1..i]).into_owned();
                return Some((args, i + 1));
            }
        }
        i += 1;
    }
    None
}

/// Return the method name that a decorator at `start` decorates
/// (`methodNameAfter`, nestjs.ts:365-407).
fn method_name_after(safe: &str, start: usize) -> Option<String> {
    let bytes = safe.as_bytes();
    let mut i = start;

    // Skip stacked decorators.
    loop {
        i = eat_ws(bytes, i);
        if bytes.get(i) != Some(&b'@') {
            break;
        }
        i = eat_deco_name(bytes, i);
        i = eat_ws(bytes, i);
        if bytes.get(i) == Some(&b'(') {
            match read_args(bytes, i) {
                Some((_, end)) => i = end,
                None => return None,
            }
        }
    }

    // Skip access/async/static modifiers.
    loop {
        i = eat_ws(bytes, i);
        match eat_modifier(bytes, i) {
            Some(next) if next > i => i = next,
            _ => break,
        }
    }

    i = eat_ws(bytes, i);
    read_ident_before_paren(bytes, i)
}

/// Return the class name a class decorator at `start` decorates
/// (`classNameAfter`, nestjs.ts:627-656).
fn class_name_after(safe: &str, start: usize) -> Option<String> {
    let bytes = safe.as_bytes();
    let mut i = start;

    loop {
        i = eat_ws(bytes, i);
        if bytes.get(i) != Some(&b'@') {
            break;
        }
        i = eat_deco_name(bytes, i);
        i = eat_ws(bytes, i);
        if bytes.get(i) == Some(&b'(') {
            match read_args(bytes, i) {
                Some((_, end)) => i = end,
                None => return None,
            }
        }
    }

    i = eat_ws(bytes, i);
    read_class_decl(safe, i)
}

fn eat_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Skip a `@[\w.]+` decorator name.
fn eat_deco_name(bytes: &[u8], mut i: usize) -> usize {
    if bytes.get(i) != Some(&b'@') {
        return i;
    }
    i += 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
    {
        i += 1;
    }
    i
}

/// Match `(?:public|private|protected|async|static)\b` at `i`; return the index
/// past it when present.
fn eat_modifier(bytes: &[u8], i: usize) -> Option<usize> {
    const MODIFIERS: [&[u8]; 5] = [b"public", b"private", b"protected", b"async", b"static"];
    for m in MODIFIERS {
        let end = i + m.len();
        if end <= bytes.len() && &bytes[i..end] == m {
            // `\b`: next char must not be a word char.
            let boundary = bytes
                .get(end)
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'));
            if boundary {
                return Some(end);
            }
        }
    }
    None
}

/// Match `([A-Za-z_$][\w$]*)\s*\(` at `i` and return the identifier.
fn read_ident_before_paren(bytes: &[u8], i: usize) -> Option<String> {
    if i >= bytes.len() || !is_ident_start(bytes[i]) {
        return None;
    }
    let mut j = i + 1;
    while j < bytes.len() && is_ident_part(bytes[j]) {
        j += 1;
    }
    let name_end = j;
    let mut k = j;
    while k < bytes.len() && bytes[k].is_ascii_whitespace() {
        k += 1;
    }
    if bytes.get(k) == Some(&b'(') {
        Some(String::from_utf8_lossy(&bytes[i..name_end]).into_owned())
    } else {
        None
    }
}

/// Match `(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)`
/// at `i`.
fn read_class_decl(safe: &str, i: usize) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)")
            .expect("class decl")
    });
    let tail = safe.get(i..)?;
    re.captures(tail)
        .map(|c| c.get(1).expect("class name").as_str().to_string())
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident_part(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

// ---------------------------------------------------------------------------
// Class scopes (nestjs.ts:413-460)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassKind {
    Controller,
    Resolver,
    Gateway,
    Other,
}

struct ClassScope {
    kind: ClassKind,
    prefix: String,
    start: usize,
    end: usize,
}

/// Build class-level decorator scopes, sorted by position
/// (`buildClassScopes`, nestjs.ts:429-453).
fn build_class_scopes(safe: &str) -> Vec<ClassScope> {
    struct Def {
        kind: ClassKind,
        name: &'static str,
        prefix_of: fn(&str) -> String,
    }
    let defs: [Def; 6] = [
        Def {
            kind: ClassKind::Controller,
            name: "Controller",
            prefix_of: parse_controller_prefix,
        },
        Def {
            kind: ClassKind::Resolver,
            name: "Resolver",
            prefix_of: |_| String::new(),
        },
        Def {
            kind: ClassKind::Gateway,
            name: "WebSocketGateway",
            prefix_of: parse_gateway_namespace,
        },
        Def {
            kind: ClassKind::Other,
            name: "Injectable",
            prefix_of: |_| String::new(),
        },
        Def {
            kind: ClassKind::Other,
            name: "Module",
            prefix_of: |_| String::new(),
        },
        Def {
            kind: ClassKind::Other,
            name: "Catch",
            prefix_of: |_| String::new(),
        },
    ];

    let mut raw: Vec<(ClassKind, String, usize)> = Vec::new();
    for def in &defs {
        for hit in find_decorators(safe, &[def.name]) {
            raw.push((def.kind, (def.prefix_of)(&hit.args), hit.index));
        }
    }
    raw.sort_by_key(|r| r.2);

    let len = raw.len();
    raw.iter()
        .enumerate()
        .map(|(i, (kind, prefix, index))| ClassScope {
            kind: *kind,
            prefix: prefix.clone(),
            start: *index,
            end: if i + 1 < len {
                raw[i + 1].2
            } else {
                safe.len()
            },
        })
        .collect()
}

fn scope_for(scopes: &[ClassScope], index: usize) -> Option<&ClassScope> {
    scopes.iter().find(|s| index >= s.start && index < s.end)
}

// ---------------------------------------------------------------------------
// Argument parsing (nestjs.ts:466-496)
// ---------------------------------------------------------------------------

/// First string literal in `args`, or `""` (`parseStringArg`, nestjs.ts:467-470).
fn parse_string_arg(args: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"['"`]([^'"`]*)['"`]"#).expect("string arg"))
        .captures(args)
        .map(|c| c.get(1).expect("string group").as_str().to_string())
        .unwrap_or_default()
}

/// `parseControllerPrefix` (nestjs.ts:473-477).
fn parse_controller_prefix(args: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"path\s*:\s*['"`]([^'"`]*)['"`]"#).expect("ctrl path"));
    if let Some(c) = re.captures(args) {
        return c.get(1).expect("path group").as_str().to_string();
    }
    parse_string_arg(args)
}

/// `parseGatewayNamespace` (nestjs.ts:480-483).
fn parse_gateway_namespace(args: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"namespace\s*:\s*['"`]([^'"`]*)['"`]"#).expect("ns"))
        .captures(args)
        .map(|c| c.get(1).expect("ns group").as_str().to_string())
        .unwrap_or_default()
}

/// `parseGraphqlName` (nestjs.ts:490-496).
fn parse_graphql_name(args: &str, handler: Option<&str>) -> String {
    static NAMED: OnceLock<Regex> = OnceLock::new();
    static LEAD: OnceLock<Regex> = OnceLock::new();
    let named =
        NAMED.get_or_init(|| Regex::new(r#"name\s*:\s*['"`]([^'"`]*)['"`]"#).expect("named"));
    if let Some(c) = named.captures(args) {
        return c.get(1).expect("name group").as_str().to_string();
    }
    let lead = LEAD.get_or_init(|| Regex::new(r#"^\s*['"`]([^'"`]*)['"`]"#).expect("lead"));
    if let Some(c) = lead.captures(args) {
        return c.get(1).expect("lead group").as_str().to_string();
    }
    handler.unwrap_or("").to_string()
}

// ---------------------------------------------------------------------------
// Path helpers (nestjs.ts:503-517)
// ---------------------------------------------------------------------------

/// `joinHttpPath` (nestjs.ts:503-508).
fn join_http_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<String> = [prefix, sub]
        .iter()
        .map(|p| trim_slashes(p.trim()).to_string())
        .filter(|p| !p.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// `p.replace(/^\/+|\/+$/g, '')`.
fn trim_slashes(s: &str) -> &str {
    s.trim_matches('/')
}

fn line_at(safe: &str, index: usize) -> i64 {
    safe[..index].matches('\n').count() as i64 + 1
}

fn detect_language(file_path: &str) -> Language {
    if file_path.ends_with(".ts") || file_path.ends_with(".tsx") {
        Language::TypeScript
    } else {
        Language::JavaScript
    }
}

// ---------------------------------------------------------------------------
// RouterModule + @Module walkers (nestjs.ts:532-620)
// ---------------------------------------------------------------------------

/// `collectRouterModuleRegistrations` (nestjs.ts:532-543).
fn collect_router_module_registrations(safe: &str, out: &mut BTreeMap<String, String>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\bRouterModule\s*\.\s*(?:register|forRoot|forChild)\s*\(").expect("router mod")
    });
    let bytes = safe.as_bytes();
    let mut search_from = 0usize;
    while search_from <= safe.len() {
        let Some(m) = re.find_at(safe, search_from) else {
            break;
        };
        let open_index = m.end() - 1;
        match read_args(bytes, open_index) {
            Some((args, end)) => {
                let items = parse_routes_array(&args);
                walk_routes_tree(&items, "", out);
                search_from = end;
            }
            None => search_from = m.end(),
        }
    }
}

struct RouteItem {
    path: String,
    module_name: Option<String>,
    children: Vec<RouteItem>,
}

/// `parseRoutesArray` (nestjs.ts:557-564).
fn parse_routes_array(args: &str) -> Vec<RouteItem> {
    let trimmed = args.trim();
    if !trimmed.starts_with('[') {
        return Vec::new();
    }
    let close = matching_close(trimmed, 0);
    if close < 0 {
        return Vec::new();
    }
    parse_route_objects(&trimmed[1..close as usize])
}

/// `parseRouteObjects` (nestjs.ts:566-576).
fn parse_route_objects(s: &str) -> Vec<RouteItem> {
    let mut items = Vec::new();
    for obj in split_top_level_objects(s) {
        let path = parse_string_field(&obj, "path");
        let module_name = parse_ident_field(&obj, "module");
        let children = match parse_array_field(&obj, "children") {
            Some(children_str) => parse_route_objects(&children_str),
            None => Vec::new(),
        };
        items.push(RouteItem {
            path,
            module_name,
            children,
        });
    }
    items
}

/// `walkRoutesTree` (nestjs.ts:578-592). First-write-wins.
fn walk_routes_tree(items: &[RouteItem], parent_prefix: &str, out: &mut BTreeMap<String, String>) {
    for item in items {
        let my_prefix = join_http_path(parent_prefix, &item.path);
        if let Some(module_name) = &item.module_name {
            out.entry(module_name.clone())
                .or_insert_with(|| my_prefix.clone());
        }
        if !item.children.is_empty() {
            walk_routes_tree(&item.children, &my_prefix, out);
        }
    }
}

/// `collectModuleControllers` (nestjs.ts:601-611). First-write-wins.
fn collect_module_controllers(safe: &str, out: &mut BTreeMap<String, String>) {
    for hit in find_decorators(safe, &["Module"]) {
        let Some(class_name) = class_name_after(safe, hit.end) else {
            continue;
        };
        for controller in parse_controllers_field(&hit.args) {
            out.entry(controller).or_insert_with(|| class_name.clone());
        }
    }
}

/// `parseControllersField` (nestjs.ts:613-620).
fn parse_controllers_field(args: &str) -> Vec<String> {
    static IDENT: OnceLock<Regex> = OnceLock::new();
    let ident = IDENT.get_or_init(|| Regex::new(r"^[A-Za-z_$][\w$]*$").expect("ident"));
    match parse_array_field(args, "controllers") {
        None => Vec::new(),
        Some(inner) => inner
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| ident.is_match(s))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// String utilities (nestjs.ts:682-765)
// ---------------------------------------------------------------------------

/// `matchingClose` (nestjs.ts:682-702). Returns a byte index or -1.
fn matching_close(s: &str, open: usize) -> i64 {
    let bytes = s.as_bytes();
    let opener = bytes.get(open).copied();
    if opener != Some(b'[') && opener != Some(b'{') && opener != Some(b'(') {
        return -1;
    }
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None;
    let mut i = open;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = Some(ch);
            i += 1;
            continue;
        }
        if ch == b'{' || ch == b'[' || ch == b'(' {
            depth += 1;
        } else if ch == b'}' || ch == b']' || ch == b')' {
            depth -= 1;
            if depth == 0 {
                return i as i64;
            }
        }
        i += 1;
    }
    -1
}

/// `splitTopLevelObjects` (nestjs.ts:709-737).
fn split_top_level_objects(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut obj_start: i64 = -1;
    let mut in_str: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            in_str = Some(ch);
            i += 1;
            continue;
        }
        if depth == 0 && ch == b'{' {
            depth = 1;
            obj_start = i as i64;
            i += 1;
            continue;
        }
        if ch == b'{' || ch == b'[' || ch == b'(' {
            depth += 1;
        } else if ch == b'}' || ch == b']' || ch == b')' {
            depth -= 1;
            if depth == 0 && obj_start >= 0 && ch == b'}' {
                let start = obj_start as usize + 1;
                out.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
                obj_start = -1;
            }
        }
        i += 1;
    }
    out
}

/// `parseStringField` (nestjs.ts:744-748).
fn parse_string_field(obj: &str, name: &str) -> String {
    let pattern = format!(r#"(?:^|[,{{\s]){name}\s*:\s*['"`]([^'"`]*)['"`]"#);
    let re = Regex::new(&pattern).expect("string field");
    re.captures(obj)
        .map(|c| c.get(1).expect("value group").as_str().to_string())
        .unwrap_or_default()
}

/// `parseIdentField` (nestjs.ts:751-755).
fn parse_ident_field(obj: &str, name: &str) -> Option<String> {
    let pattern = format!(r"(?:^|[,{{\s]){name}\s*:\s*([A-Za-z_$][\w$]*)");
    let re = Regex::new(&pattern).expect("ident field");
    re.captures(obj)
        .map(|c| c.get(1).expect("ident group").as_str().to_string())
}

/// `parseArrayField` (nestjs.ts:758-765).
fn parse_array_field(obj: &str, name: &str) -> Option<String> {
    let pattern = format!(r"(?:^|[,{{\s]){name}\s*:\s*\[");
    let re = Regex::new(&pattern).expect("array field");
    let m = re.find(obj)?;
    let open = m.end() - 1;
    let close = matching_close(obj, open);
    if close < 0 {
        return None;
    }
    Some(obj[open + 1..close as usize].to_string())
}

/// A list's comma-separated items at depth 0, strings and brackets stepped
/// over (`splitTopLevel`).
fn split_top_level(list: &str) -> Vec<&str> {
    let bytes = list.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let ch = bytes[i];
        if let Some(q) = in_str {
            if ch == b'\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match ch {
            b'"' | b'\'' | b'`' => in_str = Some(ch),
            b'[' | b'{' | b'(' => depth += 1,
            b']' | b'}' | b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&list[start..]);
    out.retain(|item| !item.trim().is_empty());
    out
}

fn comment_lang_of(file_path: &str) -> CommentLang {
    match detect_language(file_path) {
        Language::TypeScript => CommentLang::TypeScript,
        _ => CommentLang::JavaScript,
    }
}

/// The files the bootstrap and RouterModule passes read (`/\.(m?[jt]s|cjs)$/`).
fn nest_source_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(m?[jt]s|cjs)$").expect("nest source"))
}

/// A TS/JS module, where another file's class needs an import.
fn script_module_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(?:m?[jt]sx?|cjs|cts|mts)$").expect("script module"))
}

fn nest_factory_create_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bNestFactory\s*\.\s*create").expect("NestFactory.create"))
}

/// A route's in-file shape in its qualified name: `file::GET:/users`.
fn http_route_qn_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"::(?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|ALL):").expect("http route qn")
    })
}

fn nestjs_file_ext_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(m?js|tsx?|cjs)$").expect("nestjs ext"))
}

fn module_file_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.module\.(m?[jt]s|cjs)$").expect("module file"))
}

#[cfg(test)]
mod tests {
    /// Extraction context for a resolver that needs no project config.
    fn test_extraction_context() -> crate::framework::FrameworkExtractionContext {
        crate::framework::FrameworkExtractionContext::without_config("")
    }

    use super::*;
    use crate::types::ImportMapping;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Ctx {
        files: HashMap<String, String>,
        nodes: Vec<Node>,
    }

    impl Ctx {
        fn file(mut self, path: &str, content: &str) -> Self {
            self.files.insert(path.to_string(), content.to_string());
            self
        }
        fn node(mut self, n: Node) -> Self {
            self.nodes.push(n);
            self
        }
    }

    impl ResolutionContext for Ctx {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, q: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == q)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains_key(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.files.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            let mut files: Vec<String> = self.files.keys().cloned().collect();
            files.sort();
            files
        }
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn get_node_by_id(&self, id: &str) -> Option<Node> {
            self.nodes.iter().find(|n| n.id == id).cloned()
        }
        fn get_import_mappings(&self, _f: &str, _l: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn mk_node(id: &str, kind: NodeKind, name: &str, file: &str) -> Node {
        Node {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language: Language::TypeScript,
            start_line: 1,
            end_line: 1,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }

    fn a_ref(name: &str, file: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("from:{file}"),
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

    // -- pure helper units --------------------------------------------------

    #[test]
    fn detect_language_by_extension() {
        assert_eq!(detect_language("a.ts"), Language::TypeScript);
        assert_eq!(detect_language("a.tsx"), Language::TypeScript);
        assert_eq!(detect_language("a.js"), Language::JavaScript);
    }

    #[test]
    fn line_at_counts_newlines() {
        assert_eq!(line_at("a\nb\nc", 4), 3);
        assert_eq!(line_at("abc", 0), 1);
    }

    #[test]
    fn trim_slashes_and_join_http_path() {
        assert_eq!(trim_slashes("/a/"), "a");
        assert_eq!(join_http_path("users", "/:id"), "/users/:id");
        assert_eq!(join_http_path("", ""), "/");
        assert_eq!(join_http_path("/api/", "list"), "/api/list");
    }

    #[test]
    fn parse_string_arg_variants() {
        assert_eq!(parse_string_arg("'users'"), "users");
        assert_eq!(parse_string_arg(r#""posts""#), "posts");
        assert_eq!(parse_string_arg("noStringHere"), "");
    }

    #[test]
    fn parse_controller_prefix_path_field_and_bare() {
        assert_eq!(parse_controller_prefix("{ path: 'admin' }"), "admin");
        assert_eq!(parse_controller_prefix("'users'"), "users");
    }

    #[test]
    fn parse_gateway_namespace_field() {
        assert_eq!(parse_gateway_namespace("{ namespace: 'chat' }"), "chat");
        assert_eq!(parse_gateway_namespace("{}"), "");
    }

    #[test]
    fn parse_graphql_name_named_lead_and_handler_fallback() {
        assert_eq!(parse_graphql_name("{ name: 'getUser' }", None), "getUser");
        assert_eq!(parse_graphql_name("'listUsers'", None), "listUsers");
        assert_eq!(parse_graphql_name("", Some("handlerFn")), "handlerFn");
        assert_eq!(parse_graphql_name("", None), "");
    }

    #[test]
    fn matching_close_balanced_and_unbalanced_and_strings() {
        // A string literal containing a bracket does not confuse the matcher.
        assert_eq!(matching_close("[a, ']']", 0), 7);
        // No opener at position -> -1.
        assert_eq!(matching_close("abc", 0), -1);
        // Unbalanced -> -1.
        assert_eq!(matching_close("[a, b", 0), -1);
    }

    #[test]
    fn read_args_rejects_non_paren_and_reads_nested() {
        assert_eq!(read_args(b"abc", 0), None);
        let (args, end) = read_args(b"(a, (b))x", 0).expect("balanced");
        assert_eq!(args, "a, (b)");
        assert_eq!(end, 8);
    }

    #[test]
    fn read_args_handles_string_with_escape_and_paren() {
        let src = b"('a\\')b')x";
        let (args, _) = read_args(src, 0).expect("balanced");
        assert_eq!(args, "'a\\')b'");
    }

    #[test]
    fn split_top_level_objects_units() {
        let objs = split_top_level_objects("{a:1},{b:2}");
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0], "a:1");
        assert_eq!(objs[1], "b:2");
    }

    #[test]
    fn parse_field_helpers() {
        assert_eq!(parse_string_field("{ path: 'admin' }", "path"), "admin");
        assert_eq!(
            parse_ident_field("{ module: UsersModule }", "module").as_deref(),
            Some("UsersModule")
        );
        assert_eq!(
            parse_array_field("{ controllers: [A, B] }", "controllers").as_deref(),
            Some("A, B")
        );
        assert_eq!(parse_array_field("{ x: 1 }", "controllers"), None);
    }

    #[test]
    fn parse_controllers_field_filters_non_idents() {
        let got = parse_controllers_field("{ controllers: [UsersController, 123bad, OK] }");
        assert_eq!(got, vec!["UsersController".to_string(), "OK".to_string()]);
    }

    #[test]
    fn method_name_after_skips_modifiers_and_stacked_decorators() {
        // @Get() @UseGuards(x) async findAll() -> findAll.
        let safe = "@UseGuards(AuthGuard) async findAll() {}";
        assert_eq!(method_name_after(safe, 0).as_deref(), Some("findAll"));
    }

    #[test]
    fn class_name_after_reads_exported_class() {
        let safe = "export class UsersController {}";
        assert_eq!(
            class_name_after(safe, 0).as_deref(),
            Some("UsersController")
        );
    }

    #[test]
    fn eat_modifier_boundary_check() {
        // "asyncfoo" must NOT match "async" (word boundary fails).
        assert_eq!(eat_modifier(b"asyncfoo", 0), None);
        assert_eq!(eat_modifier(b"async foo", 0), Some(5));
    }

    // -- detect -------------------------------------------------------------

    #[test]
    fn detect_via_controller_file_content() {
        let ctx = Ctx::default()
            .file("package.json", r#"{"dependencies":{"lodash":"4"}}"#)
            .file("src/x.controller.ts", "@Controller('x')\nclass X {}");
        assert!(NestjsResolver.detect(&ctx));
    }

    #[test]
    fn detect_via_gateway_file() {
        let ctx = Ctx::default().file("src/x.gateway.ts", "@WebSocketGateway()\nclass X {}");
        assert!(NestjsResolver.detect(&ctx));
    }

    #[test]
    fn detect_controller_file_without_marker_false() {
        let ctx = Ctx::default().file("src/x.controller.ts", "class Plain {}");
        assert!(!NestjsResolver.detect(&ctx));
    }

    // -- resolve: provider conventions --------------------------------------

    #[test]
    fn resolve_provider_in_a_script_module_stays_in_its_feature() {
        // In a TS/JS module, a *Service class in another directory is the
        // import resolver's or name matching's, convention file or not
        // (upstream #2246).
        let svc = mk_node(
            "class:src/foo.ts:AuthService:1",
            NodeKind::Class,
            "AuthService",
            "src/foo.ts",
        );
        let named = mk_node(
            "class:lib/auth.service.ts:AuthService:1",
            NodeKind::Class,
            "AuthService",
            "lib/auth.service.ts",
        );
        let ctx = Ctx::default().node(svc).node(named);
        let reference = a_ref("AuthService", "src/auth/auth.controller.ts");
        assert!(NestjsResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_provider_outside_a_script_module_lower_confidence() {
        // Outside a TS/JS module the convention still guesses: a *Service
        // ref whose only class is NOT under .service. -> 0.7.
        let svc = mk_node(
            "class:src/foo.ts:AuthService:1",
            NodeKind::Class,
            "AuthService",
            "src/foo.ts",
        );
        let ctx = Ctx::default().node(svc.clone());
        let reference = a_ref("AuthService", "src/auth.es6");
        let r = NestjsResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(r.target_node_id, svc.id);
        assert_eq!(r.confidence, 0.7);
    }

    #[test]
    fn resolve_provider_no_class_candidate_none() {
        // *Repository ref but no matching class -> None (early return in loop).
        let iface = mk_node(
            "iface:src/x.ts:UserRepository:1",
            NodeKind::Interface,
            "UserRepository",
            "src/x.ts",
        );
        let ctx = Ctx::default().node(iface);
        let reference = a_ref("UserRepository", "src/x.ts");
        assert!(NestjsResolver.resolve(&reference, &ctx).is_none());
    }

    #[test]
    fn resolve_non_convention_name_none() {
        let ctx = Ctx::default();
        let reference = a_ref("PlainName", "src/x.ts");
        assert!(NestjsResolver.resolve(&reference, &ctx).is_none());
    }

    // -- extract: non-nest ext / GraphQL / microservice / WS ----------------

    #[test]
    fn extract_non_nest_extension_returns_default() {
        let result = NestjsResolver
            .extract("src/styles.css", "body{}", &test_extraction_context())
            .expect("extract");
        assert!(result.nodes.is_empty() && result.references.is_empty());
    }

    #[test]
    fn extract_graphql_query_inside_resolver() {
        let content = "@Resolver()\nclass UsersResolver {\n  @Query()\n  users() {}\n}";
        let result = NestjsResolver
            .extract("src/users.resolver.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "QUERY users");
    }

    #[test]
    fn extract_graphql_op_outside_resolver_skipped() {
        // @Query not inside an @Resolver class -> ignored.
        let content = "class Plain {\n  @Query()\n  q() {}\n}";
        let result = NestjsResolver
            .extract("src/x.resolver.ts", content, &test_extraction_context())
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn extract_microservice_message_and_event_patterns() {
        let content = "class H {\n  @MessagePattern('cmd')\n  handle() {}\n  @EventPattern('evt')\n  onEvt() {}\n}";
        let result = NestjsResolver
            .extract("src/h.controller.ts", content, &test_extraction_context())
            .expect("extract");
        let names: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            names.contains(&"MESSAGE /cmd") || names.contains(&"MESSAGE cmd"),
            "got {names:?}"
        );
        assert!(
            names.iter().any(|n| n.starts_with("EVENT")),
            "got {names:?}"
        );
    }

    #[test]
    fn extract_message_pattern_falls_back_to_handler_name() {
        // No string arg -> the handler name is used as the path.
        let content = "class H {\n  @MessagePattern()\n  doThing() {}\n}";
        let result = NestjsResolver
            .extract("src/h.controller.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert!(route.name.contains("doThing"));
    }

    #[test]
    fn extract_websocket_handler_with_gateway_namespace() {
        let content = "@WebSocketGateway({ namespace: 'chat' })\nclass ChatGw {\n  @SubscribeMessage('msg')\n  onMsg() {}\n}";
        let result = NestjsResolver
            .extract("src/chat.gateway.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "WS chat:msg");
        assert!(
            result
                .references
                .iter()
                .any(|r| r.reference_name == "onMsg")
        );
    }

    #[test]
    fn extract_websocket_without_namespace_uses_event() {
        let content = "class Gw {\n  @SubscribeMessage('ping')\n  onPing() {}\n}";
        let result = NestjsResolver
            .extract("src/x.gateway.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "WS ping");
    }

    #[test]
    fn extract_http_route_no_controller_scope_uses_empty_prefix() {
        // @Get outside any controller class -> prefix is empty.
        let content = "@Get('bare')\nfunction f() {}";
        let result = NestjsResolver
            .extract("src/x.controller.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "GET /bare");
    }

    // -- post_extract -------------------------------------------------------

    #[test]
    fn post_extract_no_registrations_returns_empty() {
        // A module file with no RouterModule.register -> no prefix updates.
        let ctx = Ctx::default().file(
            "src/app.module.ts",
            "@Module({ controllers: [UsersController] })\nexport class AppModule {}",
        );
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        assert!(updates.is_empty());
    }

    #[test]
    fn post_extract_nested_children_prefix() {
        // RouterModule with nested children: child 'admin' module under parent 'v1'.
        // The child module resolves to the joined parent+child prefix.
        let module_content = "@Module({ controllers: [AdminController] })\nexport class AdminModule {}\nRouterModule.register([{ path: 'v1', children: [{ path: 'admin', module: AdminModule }] }]);";
        let mut controller = mk_node(
            "class:src/admin/admin.controller.ts:AdminController:1",
            NodeKind::Class,
            "AdminController",
            "src/admin/admin.controller.ts",
        );
        controller.start_line = 1;
        controller.end_line = 10;
        let mut route = mk_node(
            "route:src/admin/admin.controller.ts:3:GET:/",
            NodeKind::Route,
            "GET /",
            "src/admin/admin.controller.ts",
        );
        route.qualified_name = "src/admin/admin.controller.ts::GET:".to_string();
        route.start_line = 3;
        route.end_line = 3;
        let ctx = Ctx::default()
            .file("src/app.module.ts", module_content)
            .node(controller)
            .node(route);
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        // The nested child registration is walked; AdminModule's controller route
        // gets the joined prefix.
        let updated = updates
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("updated route");
        assert!(
            updated.name.starts_with("GET /v1"),
            "expected a /v1-prefixed route, got {}",
            updated.name
        );
    }

    #[test]
    fn post_extract_route_outside_controller_range_unchanged() {
        // A route whose line is outside the controller's [start,end] is skipped.
        let module_content = "@Module({ controllers: [C] })\nexport class M {}\nRouterModule.register([{ path: 'p', module: M }]);";
        let mut controller = mk_node(
            "class:src/c.controller.ts:C:1",
            NodeKind::Class,
            "C",
            "src/c.controller.ts",
        );
        controller.start_line = 1;
        controller.end_line = 2;
        let mut route = mk_node(
            "route:src/c.controller.ts:9:GET:/",
            NodeKind::Route,
            "GET /",
            "src/c.controller.ts",
        );
        route.qualified_name = "src/c.controller.ts::GET:".to_string();
        route.start_line = 9;
        route.end_line = 9;
        let ctx = Ctx::default()
            .file("src/app.module.ts", module_content)
            .node(controller)
            .node(route);
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        assert!(updates.is_empty());
    }

    #[test]
    fn apply_app_config_rewrites_name_with_a_module_prefix() {
        let mut route = mk_node("route:x:1:GET:/list", NodeKind::Route, "GET /list", "x.ts");
        route.qualified_name = "x.ts::GET:list".to_string();
        let updated = apply_app_config(&route, "admin", None, None).expect("rewrite");
        assert_eq!(updated.name, "GET /admin/list");
    }

    #[test]
    fn apply_app_config_none_without_separator() {
        let route = mk_node("route:x", NodeKind::Route, "GET /", "x.ts");
        // qualified_name has no "::" -> None.
        let bare = Node {
            qualified_name: "no-sep".to_string(),
            ..route
        };
        assert!(apply_app_config(&bare, "p", None, None).is_none());
    }

    #[test]
    fn resolver_name_and_languages() {
        assert_eq!(NestjsResolver.name(), "nestjs");
        let langs = NestjsResolver.languages().expect("langs");
        assert!(langs.contains(&Language::TypeScript));
        assert!(langs.contains(&Language::JavaScript));
    }

    #[test]
    fn detect_via_package_json_dependency() {
        let ctx = Ctx::default().file(
            "package.json",
            r#"{"dependencies":{"@nestjs/core":"10","lodash":"4"}}"#,
        );
        assert!(NestjsResolver.detect(&ctx));
    }

    #[test]
    fn detect_via_package_json_dev_dependency() {
        let ctx = Ctx::default().file(
            "package.json",
            r#"{"devDependencies":{"@nestjs/cli":"10"}}"#,
        );
        assert!(NestjsResolver.detect(&ctx));
    }

    #[test]
    fn detect_false_no_dep_no_file_marker() {
        let ctx = Ctx::default()
            .file("package.json", r#"{"dependencies":{"express":"4"}}"#)
            .file("src/plain.ts", "export const x = 1;");
        assert!(!NestjsResolver.detect(&ctx));
    }

    #[test]
    fn resolve_prefers_class_in_convention_dir_higher_confidence() {
        // Two AuthService classes: one under `.service.`, one not. The
        // convention-dir one wins at 0.85.
        let elsewhere = mk_node(
            "class:src/foo.ts:AuthService:1",
            NodeKind::Class,
            "AuthService",
            "src/foo.ts",
        );
        let in_convention = mk_node(
            "class:src/auth.service.ts:AuthService:1",
            NodeKind::Class,
            "AuthService",
            "src/auth.service.ts",
        );
        let ctx = Ctx::default().node(elsewhere).node(in_convention.clone());
        let reference = a_ref("AuthService", "src/auth.controller.ts");
        let r = NestjsResolver.resolve(&reference, &ctx).expect("resolves");
        assert_eq!(
            r.target_node_id, in_convention.id,
            "convention-dir class wins"
        );
        assert_eq!(r.confidence, 0.85);
    }

    #[test]
    fn extract_javascript_controller_uses_js_comment_lang() {
        // A `.controller.js` routes through the JavaScript comment-strip branch.
        let content = "@Controller('items')\nclass ItemsController {\n  @Get()\n  list() {}\n}";
        let result = NestjsResolver
            .extract(
                "src/items.controller.js",
                content,
                &test_extraction_context(),
            )
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "GET /items");
        assert_eq!(route.language, Language::JavaScript);
    }

    #[test]
    fn extract_http_route_with_controller_prefix_object_path() {
        // `@Controller({ path: 'admin' })` prefix joins onto the method path.
        let content = "@Controller({ path: 'admin' })\nclass AdminController {\n  @Get('users')\n  list() {}\n}";
        let result = NestjsResolver
            .extract(
                "src/admin.controller.ts",
                content,
                &test_extraction_context(),
            )
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "GET /admin/users");
    }

    #[test]
    fn extract_graphql_named_query_uses_name_over_handler() {
        // `@Query({ name: 'allUsers' })` inside a resolver names the route by
        // the explicit `name` field, not the handler.
        let content = "@Resolver()\nclass R {\n  @Query({ name: 'allUsers' })\n  fetch() {}\n}";
        let result = NestjsResolver
            .extract("src/r.resolver.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "QUERY allUsers");
    }

    #[test]
    fn extract_graphql_leading_string_name() {
        // `@Mutation('doThing')` inside a resolver names by the leading string.
        let content = "@Resolver()\nclass R {\n  @Mutation('doThing')\n  run() {}\n}";
        let result = NestjsResolver
            .extract("src/r.resolver.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "MUTATION doThing");
    }

    #[test]
    fn extract_event_pattern_falls_back_to_handler_name() {
        // `@EventPattern()` with no string arg → path is the handler name.
        let content = "class H {\n  @EventPattern()\n  onEvt() {}\n}";
        let result = NestjsResolver
            .extract("src/h.controller.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "EVENT onEvt");
    }

    #[test]
    fn extract_websocket_no_arg_no_handler_uses_empty_event() {
        // `@SubscribeMessage()` with neither a string arg nor a following method
        // name → empty event, empty namespace → "WS ".
        let content = "class Gw {\n  @SubscribeMessage()\n}";
        let result = NestjsResolver
            .extract("src/x.gateway.ts", content, &test_extraction_context())
            .expect("extract");
        let route = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Route)
            .expect("route");
        assert_eq!(route.name, "WS ");
    }

    #[test]
    fn method_name_after_returns_none_on_unterminated_decorator_args() {
        // A stacked decorator whose `(` never closes → read_args None → None.
        let safe = "@UseGuards(unterminated";
        assert!(method_name_after(safe, 0).is_none());
    }

    #[test]
    fn method_name_after_none_when_no_ident_before_paren() {
        // After modifiers there is no `ident(` → None.
        let safe = "async   ";
        assert!(method_name_after(safe, 0).is_none());
    }

    #[test]
    fn class_name_after_returns_none_on_unterminated_decorator_args() {
        let safe = "@Injectable(unterminated";
        assert!(class_name_after(safe, 0).is_none());
    }

    #[test]
    fn class_name_after_none_when_no_class_decl() {
        // No `class X` follows → None.
        let safe = "const notAClass = 1;";
        assert!(class_name_after(safe, 0).is_none());
    }

    #[test]
    fn read_ident_before_paren_rejects_non_ident_start() {
        assert!(read_ident_before_paren(b"123(", 0).is_none());
        assert!(read_ident_before_paren(b"", 0).is_none());
        // ident not followed by `(` → None.
        assert!(read_ident_before_paren(b"name;", 0).is_none());
    }

    #[test]
    fn read_class_decl_variants() {
        assert_eq!(
            read_class_decl("export default abstract class Foo {}", 0).as_deref(),
            Some("Foo")
        );
        assert_eq!(read_class_decl("not a class", 0), None);
    }

    #[test]
    fn parse_string_field_no_match_is_empty() {
        assert_eq!(parse_string_field("{ other: 'x' }", "path"), "");
    }

    #[test]
    fn parse_array_field_unterminated_is_none() {
        // `controllers: [` with no closing bracket → matching_close < 0 → None.
        assert_eq!(
            parse_array_field("{ controllers: [A, B", "controllers"),
            None
        );
    }

    #[test]
    fn parse_routes_array_non_array_and_unterminated() {
        assert!(parse_routes_array("nope").is_empty());
        assert!(parse_routes_array("[ { path: 'a' }").is_empty());
    }

    #[test]
    fn find_decorators_skips_unterminated_open_paren() {
        // `@Get(` never closes → read_args None → the hit is skipped, search
        // advances past the match, no panic and no hits.
        let hits = find_decorators("@Get(", &HTTP_METHODS);
        assert!(hits.is_empty());
    }

    #[test]
    fn matching_close_object_and_paren_openers() {
        assert_eq!(matching_close("{a}", 0), 2);
        assert_eq!(matching_close("(a)", 0), 2);
    }

    #[test]
    fn detect_via_controller_content_marker_at_nestjs_string() {
        // A `.module.ts` file whose content carries `@nestjs/` (not a decorator)
        // triggers the content-scan detect branch.
        let ctx = Ctx::default().file(
            "src/app.module.ts",
            "import { Module } from '@nestjs/common';\nclass App {}",
        );
        assert!(NestjsResolver.detect(&ctx));
    }

    #[test]
    fn build_class_scopes_recognizes_all_decorator_kinds() {
        // A source with Controller / Resolver / WebSocketGateway / Injectable /
        // Module / Catch decorators exercises every `Def` in build_class_scopes,
        // and the Controller prefix + Gateway namespace prefix_of closures.
        let safe = "\
@Controller({ path: 'api' })
class C {}
@Resolver()
class R {}
@WebSocketGateway({ namespace: 'ws' })
class G {}
@Injectable()
class S {}
@Module({})
class M {}
@Catch()
class F {}
";
        let scopes = build_class_scopes(safe);
        let kinds: Vec<ClassKind> = scopes.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&ClassKind::Controller));
        assert!(kinds.contains(&ClassKind::Resolver));
        assert!(kinds.contains(&ClassKind::Gateway));
        assert!(kinds.contains(&ClassKind::Other));
        let controller = scopes
            .iter()
            .find(|s| s.kind == ClassKind::Controller)
            .expect("controller scope");
        assert_eq!(controller.prefix, "api");
        let gateway = scopes
            .iter()
            .find(|s| s.kind == ClassKind::Gateway)
            .expect("gateway scope");
        assert_eq!(gateway.prefix, "ws");
    }

    #[test]
    fn class_name_after_reads_bare_and_default_decls() {
        // No modifiers, bare `class X` after the decorator.
        assert_eq!(
            class_name_after("@Injectable()\nclass Plain {}", 0).as_deref(),
            Some("Plain")
        );
    }

    #[test]
    fn collect_module_controllers_skips_module_without_class() {
        // A `@Module({...})` decorator not followed by a class declaration → the
        // class_name_after None arm is taken and the entry is skipped.
        let mut out = BTreeMap::new();
        collect_module_controllers("@Module({ controllers: [C] })\nconst x = 1;", &mut out);
        assert!(out.is_empty(), "no class after @Module → skipped: {out:?}");
    }

    #[test]
    fn collect_module_controllers_maps_controllers_to_class() {
        let mut out = BTreeMap::new();
        collect_module_controllers(
            "@Module({ controllers: [UsersController] })\nexport class UsersModule {}",
            &mut out,
        );
        assert_eq!(
            out.get("UsersController").map(String::as_str),
            Some("UsersModule")
        );
    }

    #[test]
    fn walk_routes_tree_recurses_children_first_write_wins() {
        let items = vec![RouteItem {
            path: "v1".to_string(),
            module_name: Some("V1Module".to_string()),
            children: vec![RouteItem {
                path: "admin".to_string(),
                module_name: Some("AdminModule".to_string()),
                children: Vec::new(),
            }],
        }];
        let mut out = BTreeMap::new();
        walk_routes_tree(&items, "", &mut out);
        assert_eq!(out.get("V1Module").map(String::as_str), Some("/v1"));
        assert_eq!(
            out.get("AdminModule").map(String::as_str),
            Some("/v1/admin")
        );
    }

    #[test]
    fn collect_router_module_registrations_skips_unterminated_call() {
        // `RouterModule.register(` with no closing paren → read_args None arm,
        // search advances, no registrations collected.
        let mut out = BTreeMap::new();
        collect_router_module_registrations("RouterModule.register(", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn post_extract_javascript_module_file_uses_js_comment_lang() {
        // A `.module.js` module file drives the JavaScript comment-lang arm of
        // post_extract's per-file loop.
        let ctx = Ctx::default().file(
            "src/app.module.js",
            "@Module({ controllers: [C] })\nclass M {}",
        );
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        assert!(updates.is_empty(), "no RouterModule reg → no updates");
    }

    // -- the app's bootstrap: global prefix and URI versioning (#2117) -----

    fn bootstrap(root: &str, safe: &str) -> NestBootstrap {
        NestBootstrap {
            root: root.to_string(),
            test: false,
            config: read_bootstrap(safe),
        }
    }

    fn test_bootstrap(root: &str, safe: &str) -> NestBootstrap {
        NestBootstrap {
            test: true,
            ..bootstrap(root, safe)
        }
    }

    fn http_route(file: &str, line: i64, method: &str, path: &str, name: &str) -> Node {
        let mut route = mk_node(
            &format!("route:{file}:{line}:{method}:{path}"),
            NodeKind::Route,
            name,
            file,
        );
        route.qualified_name = format!("{file}::{method}:{path}");
        route.start_line = line;
        route.end_line = line;
        route
    }

    #[test]
    fn app_root_is_the_directory_above_the_first_src_segment() {
        assert_eq!(app_root_of("apps/api/src/main.ts"), "apps/api/");
        assert_eq!(app_root_of("src/main.ts"), "");
        assert_eq!(app_root_of("apps/api/src/app/src/x.ts"), "apps/api/");
        assert_eq!(app_root_of("libs/shared/x.ts"), "");
        assert_eq!(app_root_of("apps/mysrc/main.ts"), "");
        assert_eq!(app_root_of("apps/src"), "");
    }

    /// W7-02b (KEEP-RUST): a test's bootstrap with no `src/` above it serves
    /// the feature above its test folder; every other file's app is the one
    /// above its `src/`, else the repository.
    #[test]
    fn a_tests_bootstrap_belongs_to_its_own_feature() {
        for (file, test, root) in [
            (
                "integration/security/e2e/csrf-protection.spec.ts",
                true,
                "integration/security/",
            ),
            (
                "integration/cookies/e2e/cookies.spec.ts",
                true,
                "integration/cookies/",
            ),
            ("test/app.e2e-spec.ts", true, ""),
            ("apps/api/test/app.e2e-spec.ts", true, "apps/api/"),
            (
                "packages/core/test/scope/transient.spec.ts",
                true,
                "packages/core/",
            ),
            ("integration/hooks/main.spec.ts", true, "integration/hooks/"),
            ("app.spec.ts", true, ""),
            ("apps/api/src/app.e2e.spec.ts", true, "apps/api/"),
            ("apps/api/src/test/app.e2e-spec.ts", true, "apps/api/"),
            ("server/main.ts", false, ""),
            ("apps/api/src/main.ts", false, "apps/api/"),
        ] {
            assert_eq!(bootstrap_root(file, test), root, "{file}");
        }
    }

    /// W7-02b (KEEP-RUST): within an app, its entry point's bootstrap decides
    /// over a test's; a test's configures an app nothing else creates.
    #[test]
    fn an_entry_points_bootstrap_decides_over_a_tests() {
        let main = bootstrap("", "NestFactory.create(AppModule);");
        let spec = test_bootstrap(
            "",
            "NestFactory.create(AppModule);\napp.setGlobalPrefix('api');",
        );
        assert_eq!(
            bootstrap_for(&[spec, main], "src/users/users.controller.ts")
                .map(|config| config.adds()),
            Some(false)
        );
        let main = bootstrap(
            "",
            "NestFactory.create(AppModule);\napp.setGlobalPrefix('api');",
        );
        let spec = test_bootstrap("", "NestFactory.create(AppModule);");
        assert_eq!(
            bootstrap_for(&[spec, main], "src/users/users.controller.ts")
                .map(|config| config.prefix.as_str()),
            Some("api")
        );
        let security = test_bootstrap(
            "integration/security/",
            "NestFactory.create(AppModule);\napp.setGlobalPrefix('api');",
        );
        let cookies = test_bootstrap("integration/cookies/", "NestFactory.create(AppModule);");
        let bootstraps = [security, cookies];
        assert_eq!(
            bootstrap_for(&bootstraps, "integration/security/src/app.controller.ts")
                .map(|config| config.prefix.as_str()),
            Some("api")
        );
        assert_eq!(
            bootstrap_for(&bootstraps, "integration/cookies/src/app.controller.ts")
                .map(|config| config.prefix.as_str()),
            Some("")
        );
        // A feature no bootstrap creates, beside two apps: none of theirs.
        assert!(bootstrap_for(&bootstraps, "integration/hello/src/hello.controller.ts").is_none());
    }

    #[test]
    fn version_values() {
        assert_eq!(version_value(" '2' "), Some(Version::Literal("2".into())));
        assert_eq!(
            version_value("['1', '2']"),
            Some(Version::Literal("1".into()))
        );
        assert_eq!(version_value("VERSION_NEUTRAL"), Some(Version::Neutral));
        assert_eq!(version_value("VERSION_NEUTRALS"), None);
        assert_eq!(version_value("config.version"), None);
    }

    #[test]
    fn bootstrap_reads_prefix_excludes_and_versioning() {
        let config = read_bootstrap(
            "app.setGlobalPrefix('/api/', { exclude: ['/health', { path: 'metrics', method: RequestMethod.GET }, 'a{/*w}'] });\napp.enableVersioning({ type: VersioningType.URI, defaultVersion: ['3', '4'], prefix: 'ver' });",
        );
        assert_eq!(config.prefix, "api");
        assert_eq!(config.excludes, ["health", "metrics", "a{/*w}"]);
        assert!(config.uri);
        assert_eq!(config.default_version, Some(Version::Literal("3".into())));
        assert_eq!(config.version_prefix, "ver");
        assert!(config.adds());
    }

    #[test]
    fn bootstrap_without_a_literal_prefix_or_uri_versioning_adds_nothing() {
        let config = read_bootstrap(
            "app.setGlobalPrefix(config.get('prefix'));\napp.enableVersioning({ type: VersioningType.HEADER, header: 'X-V' });",
        );
        assert_eq!(
            config,
            NestAppConfig {
                version_prefix: "v".into(),
                ..NestAppConfig::default()
            }
        );
        assert!(!config.adds());
        // A template literal with a hole is not a literal either.
        assert_eq!(read_bootstrap("app.setGlobalPrefix(`${base}`);").prefix, "");
        // Versioning without a type is URI versioning; `prefix: false` drops
        // the segment's prefix.
        let config = read_bootstrap("app.enableVersioning({ prefix: false });");
        assert!(config.uri);
        assert_eq!(config.version_prefix, "");
        assert_eq!(config.default_version, None);
    }

    #[test]
    fn excluded_paths_and_wildcards() {
        let excludes = ["health{/*wildcard}", "metrics", "admin/(.*)", "docs/*"]
            .map(String::from)
            .to_vec();
        for path in [
            "/health",
            "/health/db",
            "/metrics",
            "/admin/x",
            "/docs",
            "/docs/a/b",
        ] {
            assert!(is_excluded(path, &excludes), "{path}");
        }
        for path in ["/healthz", "/metrics/x", "/administrator", "/user"] {
            assert!(!is_excluded(path, &excludes), "{path}");
        }
    }

    #[test]
    fn each_file_takes_its_apps_bootstrap() {
        let seed = bootstrap("apps/api/", "NestFactory.create(SeedModule);");
        let main = bootstrap("apps/api/", "app.setGlobalPrefix('api');");
        let jobs = bootstrap("apps/jobs/", "NestFactory.create(JobsModule);");
        let bootstraps = [seed, main, jobs];
        // The bootstrap that configures the app, though a seed script came
        // first.
        assert_eq!(
            bootstrap_for(&bootstraps, "apps/api/src/user.controller.ts")
                .map(|c| c.prefix.as_str()),
            Some("api")
        );
        // An app that sets nothing keeps nothing, never another app's.
        assert_eq!(
            bootstrap_for(&bootstraps, "apps/jobs/src/jobs.controller.ts").map(|c| c.adds()),
            Some(false)
        );
        // A file outside every app: only when there is one app.
        assert!(bootstrap_for(&bootstraps, "libs/shared/x.controller.ts").is_none());
        let one = [bootstrap("apps/api/", "app.setGlobalPrefix('api');")];
        assert_eq!(
            bootstrap_for(&one, "libs/shared/x.controller.ts").map(|c| c.prefix.as_str()),
            Some("api")
        );
        assert!(bootstrap_for(&[], "src/x.ts").is_none());
        // The longest root above the file wins over the repository's.
        let nested = [
            bootstrap("", "app.setGlobalPrefix('root');"),
            bootstrap("apps/api/", "app.setGlobalPrefix('api');"),
        ];
        assert_eq!(
            bootstrap_for(&nested, "apps/api/src/x.ts").map(|c| c.prefix.as_str()),
            Some("api")
        );
        assert_eq!(
            bootstrap_for(&nested, "src/x.ts").map(|c| c.prefix.as_str()),
            Some("root")
        );
    }

    #[test]
    fn a_routes_version_past_its_stacked_decorators() {
        let safe = "@Controller('stats')\nexport class Stats {\n  @Version('2')\n  @ApiResponse({ status: 200, description: 'x (y)' })\n  @Get('daily')\n  @Version('9')\n  public daily() {}\n\n  @Get('weekly')\n  public weekly() { getVersion('5'); }\n}\n";
        let scopes = build_class_scopes(safe);
        let hits = find_decorators(safe, &HTTP_METHODS);
        let scope = scope_for(&scopes, hits[0].index).expect("controller scope");
        // The first @Version of the stack, above or below the route's own
        // decorator, before the handler.
        assert_eq!(
            route_version(safe, &hits[0], scope),
            Some(Some(Version::Literal("2".into())))
        );
        // Nothing of its own, nothing on the class: unspecified.
        assert_eq!(route_version(safe, &hits[1], scope), None);
    }

    #[test]
    fn a_controllers_version_on_the_class_or_in_its_options() {
        for (safe, expected) in [
            (
                "@Version('4')\n@ApiTags({ name: 'r' })\n@Controller('r')\nexport class R {\n  @Get()\n  list() {}\n}\n",
                Some(Version::Literal("4".into())),
            ),
            (
                "@Controller({ path: 'h', version: '3' })\nexport class H {\n  @Get()\n  list() {}\n}\n",
                Some(Version::Literal("3".into())),
            ),
            (
                "@Controller()\n@Version(VERSION_NEUTRAL)\nexport class S {\n  @Get()\n  list() {}\n}\n",
                Some(Version::Neutral),
            ),
        ] {
            let scopes = build_class_scopes(safe);
            let hits = find_decorators(safe, &HTTP_METHODS);
            let scope = scope_for(&scopes, hits[0].index).expect("controller scope");
            assert_eq!(
                route_version(safe, &hits[0], scope),
                Some(expected),
                "{safe}"
            );
        }
    }

    #[test]
    fn a_handler_call_is_not_a_decorator_or_a_longer_name() {
        assert_eq!(
            handler_call_offset(" @getUser() xgetUser( getUser (", "getUser"),
            Some(22)
        );
        assert_eq!(handler_call_offset("getUser()", "getUser"), Some(0));
        assert_eq!(
            handler_call_offset("$getUser() _getUser()", "getUser"),
            None
        );
        assert_eq!(handler_call_offset("getUsers()", "getUser"), None);
    }

    #[test]
    fn decorator_chain_start_steps_over_object_arguments() {
        let safe = "class A {\n  @UseGuards(Auth('jwt'))\n  @ApiResponse({ a: (1) })\n  @Get()\n  m() {}\n}";
        let get = safe.find("@Get").expect("@Get");
        let guards = safe.find("@UseGuards").expect("@UseGuards");
        assert_eq!(decorator_chain_start(safe, get, 0), guards);
        // Never above the floor.
        let response = safe.find("@ApiResponse").expect("@ApiResponse");
        assert_eq!(decorator_chain_start(safe, get, response), response);
    }

    #[test]
    fn app_config_order_prefix_version_module_prefix_path() {
        let route = http_route("a.ts", 3, "GET", "/user", "GET /user");
        let config = read_bootstrap(
            "app.setGlobalPrefix('api', { exclude: ['admin/user'] });\napp.enableVersioning({ defaultVersion: '1' });",
        );
        let name = |module_prefix: &str, version: Option<Option<Version>>| {
            apply_app_config(&route, module_prefix, Some(&config), version.as_ref())
                .map(|node| node.name)
        };
        assert_eq!(name("", None).as_deref(), Some("GET /api/v1/user"));
        assert_eq!(
            name("staff", None).as_deref(),
            Some("GET /api/v1/staff/user")
        );
        // Excluded from the prefix, still versioned.
        assert_eq!(name("admin", None).as_deref(), Some("GET /v1/admin/user"));
        assert_eq!(
            name("", Some(Some(Version::Literal("2".into())))).as_deref(),
            Some("GET /api/v2/user")
        );
        assert_eq!(
            name("", Some(Some(Version::Neutral))).as_deref(),
            Some("GET /api/user")
        );
        // A version that is not a literal adds no segment.
        assert_eq!(name("", Some(None)).as_deref(), Some("GET /api/user"));
    }

    #[test]
    fn only_this_resolvers_verbs_have_an_in_file_name() {
        let route = http_route("a.ts", 1, "POST", "/x", "POST /api/x");
        assert_eq!(in_file_name(&route).as_deref(), Some("POST /x"));
        let mut react = route.clone();
        react.qualified_name = "a.tsx::route:/x".into();
        assert_eq!(in_file_name(&react), None);
    }

    #[test]
    fn post_extract_names_routes_by_their_apps_bootstrap() {
        let controller = "import { Controller, Get } from '@nestjs/common';\n@Controller('user')\nexport class UserController {\n  @Get()\n  getUser() {}\n}\n";
        let ctx = Ctx::default()
            .file(
                "apps/api/src/main.ts",
                "const app = await NestFactory.create(AppModule);\napp.setGlobalPrefix('api');\napp.enableVersioning({ defaultVersion: '1' });\n",
            )
            .file("apps/api/src/user.controller.ts", controller)
            .file("tools/server.ts", "app.get('/user', handler);\n")
            .node(http_route("apps/api/src/user.controller.ts", 4, "GET", "/user", "GET /user"))
            // Another resolver's route of the same shape, outside a
            // controller: never renamed.
            .node(http_route("tools/server.ts", 1, "GET", "/user", "GET /user"));
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        let names: Vec<(&str, &str)> = updates
            .iter()
            .map(|node| (node.file_path.as_str(), node.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [("apps/api/src/user.controller.ts", "GET /api/v1/user")]
        );
        // Idempotent: the renamed route is renamed to the same name.
        let ctx = Ctx {
            nodes: vec![
                updates[0].clone(),
                http_route("tools/server.ts", 1, "GET", "/user", "GET /user"),
            ],
            ..ctx
        };
        assert!(NestjsResolver.post_extract(&ctx).expect("runs").is_empty());
    }

    #[test]
    fn post_extract_gives_a_route_nothing_renames_its_in_file_name_back() {
        let controller =
            "@Controller('user')\nexport class UserController {\n  @Get()\n  getUser() {}\n}\n";
        let ctx = Ctx::default()
            .file(
                "src/main.ts",
                "NestFactory.create(AppModule).then((app) => app.listen(3000));\n",
            )
            .file("src/user.controller.ts", controller)
            .file("src/server.ts", "app.get('/x', handler);\n")
            .node(http_route(
                "src/user.controller.ts",
                3,
                "GET",
                "/user",
                "GET /api/v1/user",
            ))
            // Not a route this resolver's extract makes in its file.
            .node(http_route(
                "src/server.ts",
                1,
                "GET",
                "/x",
                "GET /mounted/x",
            ));
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        let names: Vec<(&str, &str)> = updates
            .iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [("route:src/user.controller.ts:3:GET:/user", "GET /user")]
        );
    }

    fn route_names_of(updates: &[Node]) -> Vec<(&str, &str)> {
        updates
            .iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect()
    }

    #[test]
    fn post_extract_renames_only_a_route_in_a_controller() {
        // A route whose decorator sits outside every `@Controller`, in a file
        // that has one, keeps its name.
        let mixed = "@Controller('a')\nclass A {\n  @Get()\n  a() {}\n}\n@Injectable()\nclass B {\n  @Get('b')\n  b() {}\n}\n";
        let ctx = Ctx::default()
            .file(
                "src/main.ts",
                "NestFactory.create(M);\napp.setGlobalPrefix('api');\n",
            )
            .file("src/mixed.ts", mixed)
            .node(http_route("src/mixed.ts", 3, "GET", "/a", "GET /a"))
            .node(http_route("src/mixed.ts", 8, "GET", "/b", "GET /b"));
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        assert_eq!(
            route_names_of(&updates),
            [("route:src/mixed.ts:3:GET:/a", "GET /api/a")]
        );
    }

    #[test]
    fn post_extract_puts_the_router_module_prefix_under_the_apps() {
        let mut controller = mk_node(
            "class:src/users.controller.ts:UsersController:1",
            NodeKind::Class,
            "UsersController",
            "src/users.controller.ts",
        );
        controller.end_line = 5;
        let ctx = Ctx::default()
            .file(
                "src/main.ts",
                "NestFactory.create(AppModule);\napp.setGlobalPrefix('api');\napp.enableVersioning({ defaultVersion: '1' });\n",
            )
            .file(
                "src/app.module.ts",
                "@Module({ imports: [RouterModule.register([{ path: 'admin', module: AppModule }])], controllers: [UsersController] })\nexport class AppModule {}\n",
            )
            .file(
                "src/users.controller.ts",
                "@Controller('users')\nexport class UsersController {\n  @Get()\n  list() {}\n}\n",
            )
            .node(controller)
            .node(http_route("src/users.controller.ts", 3, "GET", "/users", "GET /users"));
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        assert_eq!(
            route_names_of(&updates),
            [(
                "route:src/users.controller.ts:3:GET:/users",
                "GET /api/v1/admin/users"
            )]
        );
    }

    #[test]
    fn post_extract_reads_a_bootstrap_only_where_nest_factory_create_is_code() {
        // A comment naming `NestFactory.create` is no app: the library
        // controller outside every app takes the one app's config.
        let ctx = Ctx::default()
            .file(
                "apps/api/src/main.ts",
                "NestFactory.create(AppModule);\napp.setGlobalPrefix('api');\n",
            )
            .file(
                "libs/shared/src/doc.ts",
                "// NestFactory.create is called by apps/api\nexport const doc = 1;\n",
            )
            .file(
                "libs/shared/src/x.controller.ts",
                "@Controller('x')\nexport class X {\n  @Get()\n  x() {}\n}\n",
            )
            .node(http_route(
                "libs/shared/src/x.controller.ts",
                3,
                "GET",
                "/x",
                "GET /x",
            ));
        let updates = NestjsResolver.post_extract(&ctx).expect("runs");
        assert_eq!(
            route_names_of(&updates),
            [(
                "route:libs/shared/src/x.controller.ts:3:GET:/x",
                "GET /api/x"
            )]
        );
    }

    #[test]
    fn split_top_level_items() {
        assert_eq!(
            split_top_level(" 'a', { path: 'b,c', m: [1, 2] }, f(x, y) ,, "),
            [" 'a'", " { path: 'b,c', m: [1, 2] }", " f(x, y) "]
        );
        assert!(split_top_level("").is_empty());
    }

    #[test]
    fn ctx_trait_accessors_exercise_all_lookup_paths() {
        // Drive the MockContext lookup surface so its trait methods are covered.
        let n = mk_node("class:a.ts:Svc:1", NodeKind::Class, "Svc", "a.ts");
        let ctx = Ctx::default().file("a.ts", "class Svc {}").node(n.clone());
        assert_eq!(ctx.get_nodes_in_file("a.ts").len(), 1);
        assert_eq!(ctx.get_nodes_by_name("Svc").len(), 1);
        assert_eq!(ctx.get_nodes_by_qualified_name("a.ts::Svc").len(), 1);
        assert_eq!(ctx.get_nodes_by_kind(NodeKind::Class).len(), 1);
        assert_eq!(ctx.get_nodes_by_lower_name("svc").len(), 1);
        assert_eq!(ctx.get_node_by_id(&n.id).as_ref(), Some(&n));
        assert!(ctx.file_exists("a.ts"));
        assert_eq!(ctx.read_file("a.ts").as_deref(), Some("class Svc {}"));
        assert_eq!(ctx.get_project_root(), "/project");
        assert_eq!(ctx.get_all_files(), vec!["a.ts".to_string()]);
        assert!(
            ctx.get_import_mappings("a.ts", Language::TypeScript)
                .is_empty()
        );
    }
}
