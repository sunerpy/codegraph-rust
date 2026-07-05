//! NestJS [`FrameworkResolver`] — ports
//! `upstream resolution/frameworks/nestjs.ts`.
//!
//! Handles decorator-based routing (HTTP / GraphQL / microservice / WebSocket),
//! provider→class resolution, and the cross-file `RouterModule.register([...])`
//! prefixing pass (`postExtract`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

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
        if let Some(package_json) = context.read_file("package.json") {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&package_json) {
                if any_nestjs_dep(&pkg) {
                    return true;
                }
            }
        }

        for file in context.get_all_files() {
            if file.ends_with(".controller.ts")
                || file.ends_with(".controller.js")
                || file.ends_with(".module.ts")
                || file.ends_with(".resolver.ts")
                || file.ends_with(".gateway.ts")
            {
                if let Some(content) = context.read_file(&file) {
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

    // Ports nestjsResolver.resolve (nestjs.ts:91-111).
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        for (suffix, convention) in provider_conventions() {
            if !suffix.is_match(&reference.reference_name) {
                continue;
            }
            let candidates: Vec<Node> = context
                .get_nodes_by_name(&reference.reference_name)
                .into_iter()
                .filter(|n| n.kind == NodeKind::Class)
                .collect();
            if candidates.is_empty() {
                return None;
            }
            let preferred = candidates.iter().find(|n| n.file_path.contains(convention));
            let (target, confidence) = match preferred {
                Some(node) => (node, 0.85),
                None => (&candidates[0], 0.7),
            };
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: target.id.clone(),
                confidence,
                resolved_by: ResolvedBy::Framework,
            });
        }
        None
    }

    // Ports nestjsResolver.extract (nestjs.ts:113-193).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _project_root: &str,
    ) -> Option<FrameworkResolverExtractionResult> {
        if !nestjs_file_ext_regex().is_match(file_path) {
            return Some(FrameworkResolverExtractionResult::default());
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

        Some(FrameworkResolverExtractionResult { nodes, references })
    }

    // Ports nestjsResolver.postExtract (nestjs.ts:217-266).
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let mut module_to_prefix: BTreeMap<String, String> = BTreeMap::new();
        let mut controller_to_module: BTreeMap<String, String> = BTreeMap::new();

        for file_path in context.get_all_files() {
            if !module_file_regex().is_match(&file_path) {
                continue;
            }
            let Some(content) = context.read_file(&file_path) else {
                continue;
            };
            let comment_lang = match detect_language(&file_path) {
                Language::TypeScript => CommentLang::TypeScript,
                _ => CommentLang::JavaScript,
            };
            let safe = strip_comments_for_regex(&content, comment_lang);
            collect_router_module_registrations(&safe, &mut module_to_prefix);
            collect_module_controllers(&safe, &mut controller_to_module);
        }

        let mut controller_to_prefix: BTreeMap<String, String> = BTreeMap::new();
        for (controller, module) in &controller_to_module {
            if let Some(prefix) = module_to_prefix.get(module) {
                if !prefix.is_empty() && prefix != "/" {
                    controller_to_prefix.insert(controller.clone(), prefix.clone());
                }
            }
        }

        if controller_to_prefix.is_empty() {
            return Some(Vec::new());
        }

        let mut updates: Vec<Node> = Vec::new();
        for (controller_name, prefix) in &controller_to_prefix {
            let classes: Vec<Node> = context
                .get_nodes_by_name(controller_name)
                .into_iter()
                .filter(|n| n.kind == NodeKind::Class)
                .collect();
            for cls in classes {
                let routes: Vec<Node> = context
                    .get_nodes_in_file(&cls.file_path)
                    .into_iter()
                    .filter(|n| n.kind == NodeKind::Route)
                    .collect();
                for route in routes {
                    if route.start_line < cls.start_line || route.start_line > cls.end_line {
                        continue;
                    }
                    if let Some(updated) = apply_module_prefix(&route, prefix) {
                        if updated.name != route.name {
                            updates.push(updated);
                        }
                    }
                }
            }
        }

        Some(updates)
    }
}

// ---------------------------------------------------------------------------
// Detection helpers
// ---------------------------------------------------------------------------

/// Any `@nestjs/*` key in dependencies/devDependencies (nestjs.ts:54-56).
fn any_nestjs_dep(pkg: &serde_json::Value) -> bool {
    let in_section = |section: &str| -> bool {
        pkg.get(section)
            .and_then(|d| d.as_object())
            .is_some_and(|deps| deps.keys().any(|k| k.starts_with("@nestjs/")))
    };
    in_section("dependencies") || in_section("devDependencies")
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
    let pattern = format!(r"@({})\s*\(", names.join("|"));
    let re = Regex::new(&pattern).expect("decorator regex");
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

/// `applyModulePrefix` (nestjs.ts:664-675).
fn apply_module_prefix(route: &Node, prefix: &str) -> Option<Node> {
    let sep = "::";
    let idx = route.qualified_name.find(sep)?;
    let tail = &route.qualified_name[idx + sep.len()..];
    let colon = tail.find(':')?;
    let method = &tail[..colon];
    let original = &tail[colon + 1..];
    let new_name = format!("{method} {}", join_http_path(prefix, original));
    let mut updated = route.clone();
    updated.name = new_name;
    updated.updated_at = super::now_millis();
    Some(updated)
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
    fn resolve_provider_no_convention_dir_lower_confidence() {
        // A *Service ref whose only class is NOT under .service. -> 0.7.
        let svc = mk_node(
            "class:src/foo.ts:AuthService:1",
            NodeKind::Class,
            "AuthService",
            "src/foo.ts",
        );
        let ctx = Ctx::default().node(svc.clone());
        let reference = a_ref("AuthService", "src/auth.controller.ts");
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
            .extract("src/styles.css", "body{}", "")
            .expect("extract");
        assert!(result.nodes.is_empty() && result.references.is_empty());
    }

    #[test]
    fn extract_graphql_query_inside_resolver() {
        let content = "@Resolver()\nclass UsersResolver {\n  @Query()\n  users() {}\n}";
        let result = NestjsResolver
            .extract("src/users.resolver.ts", content, "")
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
            .extract("src/x.resolver.ts", content, "")
            .expect("extract");
        assert!(!result.nodes.iter().any(|n| n.kind == NodeKind::Route));
    }

    #[test]
    fn extract_microservice_message_and_event_patterns() {
        let content = "class H {\n  @MessagePattern('cmd')\n  handle() {}\n  @EventPattern('evt')\n  onEvt() {}\n}";
        let result = NestjsResolver
            .extract("src/h.controller.ts", content, "")
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
            .extract("src/h.controller.ts", content, "")
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
            .extract("src/chat.gateway.ts", content, "")
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
            .extract("src/x.gateway.ts", content, "")
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
            .extract("src/x.controller.ts", content, "")
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
    fn apply_module_prefix_rewrites_name() {
        let mut route = mk_node("route:x:1:GET:/list", NodeKind::Route, "GET /list", "x.ts");
        route.qualified_name = "x.ts::GET:list".to_string();
        let updated = apply_module_prefix(&route, "admin").expect("rewrite");
        assert_eq!(updated.name, "GET /admin/list");
    }

    #[test]
    fn apply_module_prefix_none_without_separator() {
        let route = mk_node("route:x", NodeKind::Route, "GET /", "x.ts");
        // qualified_name has no "::" -> None.
        let bare = Node {
            qualified_name: "no-sep".to_string(),
            ..route
        };
        assert!(apply_module_prefix(&bare, "p").is_none());
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
            .extract("src/items.controller.js", content, "")
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
            .extract("src/admin.controller.ts", content, "")
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
            .extract("src/r.resolver.ts", content, "")
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
            .extract("src/r.resolver.ts", content, "")
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
            .extract("src/h.controller.ts", content, "")
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
            .extract("src/x.gateway.ts", content, "")
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
