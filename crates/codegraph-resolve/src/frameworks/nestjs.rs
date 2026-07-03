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
