//! Drupal (upstream `resolution/frameworks/drupal.ts` at `e8ddc4e2`: #271,
//! with `025ebc88`'s claims and composer name/type detection).
//!
//! A project is Drupal's when its root `composer.json` names a `drupal/`
//! package or a `drupal-` type or requires a `drupal/` package (the sync's
//! `php` control kind reads it, [`ROOT_DETECTION_FILES`]), or when it
//! indexes a `*.info.yml` manifest beside a routing or hook file.
//!
//! Every route of a `*.routing.yml` file is a route node referencing its
//! `_controller`, `_form` and entity handlers, which resolve to the class
//! the handler names, and its method. Every procedural hook implementation
//! in a hook file (`.module`, `.install`, `.theme`, `.inc`) or a `.php` file
//! references `hook_X`, by its `Implements hook_X()` docblock or by its
//! module's name prefix, from the function's node; `hook_X` resolves to the
//! one function of a hook file whose name ends with `_X` (a `hook_X`
//! definition, `function hook_X` in an `*.api.php`, is name matching's
//! exact `0.9`, which outranks it). Where upstream takes the first of
//! several same-named candidates in store order, Rust takes a single
//! candidate and gives no result for several (of same-named controller
//! classes, the only one whose file holds the method), and it only considers
//! PHP classes and functions (KNOWN_DIFFS, KEEP-RUST).

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use codegraph_core::node_id::generate_node_id;
use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::Value;

use super::framework_node;
use super::js_scan::{JS_SPACE, js_trim};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver reads (upstream `languages`).
const LANGUAGES: [Language; 2] = [Language::Php, Language::Yaml];

/// The root file detection reads by path, indexed or not: an input of the
/// sync's `php` control kind (`codegraph_watch::control_inputs`), since a
/// `composer.json` is never indexed.
pub const ROOT_DETECTION_FILES: [&str; 1] = ["composer.json"];

/// The files that hold procedural hook implementations (upstream
/// `HOOK_FILE_EXTENSIONS`).
const HOOK_FILE_EXTENSIONS: [&str; 4] = [".module", ".install", ".theme", ".inc"];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{N}` its
/// `\S`, `{D}` its `.` (no line terminator), `{W}` the body of its `\w`
/// (ASCII).
fn drupal_regex(pattern: &str) -> Regex {
    let non_space = format!("[^{}", &JS_SPACE[1..]);
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{N}", &non_space)
            .replace("{D}", r"[^\n\r\x{2028}\x{2029}]")
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("drupal pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| drupal_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A `Class::method` or `Class:method` name (the claim's third shape).
    method_shape = r"^[A-Za-z_][{W}]*::?[{W}]+$";
    /// A controller (`\Drupal\…\Class::method`, the single-colon service
    /// form `Class:method`): the class and the method.
    controller = r"^\\?(?:Drupal\\[^:]+\\)?([^\\:]+):{1,2}([{W}]+)$";
    /// A route's name line: no indentation, ending with a colon.
    route_start = r"^{N}{D}*:{S}*$";
    /// `path: '/some/path'`.
    path_line = r##"^path:{S}*['"]?([^'"#\n]+?)['"]?{S}*(?:#{D}*)?$"##;
    /// `_controller: '\Drupal\…\Class::method'`.
    controller_line = r##"^_controller:{S}*['"]?([^'"#\n]+?)['"]?{S}*(?:#{D}*)?$"##;
    /// `_form: '\Drupal\…\Form\MyForm'`.
    form_line = r##"^_form:{S}*['"]?([^'"#\n]+?)['"]?{S}*(?:#{D}*)?$"##;
    /// `_entity_form`, `_entity_list` and `_entity_view: entity.type`.
    entity_line = r##"^_(?:entity_form|entity_list|entity_view):{S}*['"]?([^'"#\n]+?)['"]?{S}*(?:#{D}*)?$"##;
    /// `methods: [GET, POST]`.
    methods_line = r"^methods:{S}*\[([^\]]+)\]";
    /// A function declared at a line's start.
    function_def = r"^function{S}+([{W}]+){S}*\(";
    /// A docblock reading `Implements hook_X()` and the function after it.
    docblock = r"/\*\*(?s:.*?)(?:@|\*{S}+)Implements{S}+(hook_[{W}]+){S}*\(\)(?s:.*?)\*/{S}*\n(?:{S}*\n)*function{S}+([{W}]+){S}*\(";
}

pub struct DrupalResolver;

impl FrameworkResolver for DrupalResolver {
    fn name(&self) -> &str {
        "drupal"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `composer.json` naming Drupal (upstream: a `drupal/` name, a
    /// `drupal-` type, or a `drupal/` key in `require` or `require-dev`),
    /// else an indexed `*.info.yml` beside a routing, module, install or
    /// theme file.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if context
            .read_file_shared("composer.json")
            .is_some_and(|composer| composer_names_drupal(&composer))
        {
            return true;
        }
        let files = context.get_all_files_shared();
        files.iter().any(|file| file.ends_with(".info.yml"))
            && files.iter().any(|file| {
                file.ends_with(".routing.yml")
                    || file.ends_with(".module")
                    || file.ends_with(".install")
                    || file.ends_with(".theme")
            })
    }

    /// Handler class names and hook names, which name no node, past the
    /// name pre-filter.
    fn claims_reference(&self, name: &str) -> bool {
        name.starts_with("hook_") || name.contains('\\') || method_shape().is_match(name)
    }

    /// A routing file's references and the hook names only (KEEP-RUST):
    /// upstream claims the names wherever they are written, so every PHP
    /// `use Foo\Bar;` came past the pre-filter too.
    fn claims(&self, reference: &RefView) -> bool {
        reference.reference_name.starts_with("hook_")
            || (is_routing_reference(reference) && self.claims_reference(&reference.reference_name))
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        // A handler is a routing file's (KEEP-RUST): upstream answers any
        // reference by its name's last segment, linking a PHP `use Foo\Bar;`
        // to any project class `Bar`, which no sync then revisits.
        let handler = is_routing_reference(reference);
        if handler && let Some(found) = controller().captures(name) {
            let (class, method) = (&found[1], &found[2]);
            match php_classes(class, context).as_slice() {
                [] => {}
                // Upstream: the class's file's method, else the class.
                [only] => {
                    let nodes = context.get_nodes_in_file_shared(&only.file_path);
                    return Some(match first_method(&nodes, method) {
                        Some(found) => framework_ref(reference, found.id.clone(), 0.9),
                        None => framework_ref(reference, only.id.clone(), 0.7),
                    });
                }
                // Upstream takes its store's first class, which may lack the
                // method; Rust the one whose file holds it, when only one does.
                several => {
                    let mut methods: Vec<String> = Vec::new();
                    for class in several {
                        let nodes = context.get_nodes_in_file_shared(&class.file_path);
                        if let Some(found) = first_method(&nodes, method)
                            && !methods.contains(&found.id)
                        {
                            methods.push(found.id.clone());
                        }
                    }
                    return match methods.as_slice() {
                        [only] => Some(framework_ref(reference, only.clone(), 0.9)),
                        _ => None,
                    };
                }
            }
        }

        // `_form` and `_entity_form`'s bare class name.
        if handler
            && name.contains('\\')
            && !name.contains(':')
            && let Some(class) = last_segment(name)
        {
            match php_classes(class, context).as_slice() {
                [] => {}
                [only] => return Some(framework_ref(reference, only.id.clone(), 0.85)),
                _ => return None,
            }
        }

        // `hook_X`: the function of a hook file named `*_X`.
        let suffix = name.strip_prefix("hook_")?;
        let wanted = format!("_{suffix}");
        let tables = context.drupal_tables();
        let mut candidates = tables
            .hook_functions(context)
            .iter()
            .filter(|function| function.name.ends_with(&wanted));
        let first = candidates.next()?;
        candidates
            .next()
            .is_none()
            .then(|| framework_ref(reference, first.id.clone(), 0.75))
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if file_path.ends_with(".routing.yml") {
            return Some(extract_routes(file_path, content));
        }
        if is_hook_file(file_path) || file_path.ends_with(".php") {
            return Some(extract_hooks(file_path, content));
        }
        Some(FrameworkResolverExtractionResult::default())
    }
}

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// Whether `reference` is a routing file's: a YAML route's in a
/// `*.routing.yml`.
pub(crate) fn is_routing_reference(reference: &RefView) -> bool {
    reference.language == Language::Yaml
        && reference.file_path.ends_with(".routing.yml")
        && reference.from_node_id.starts_with("route:")
}

/// Whether a root `composer.json`'s text names Drupal. Text `JSON.parse`
/// rejects (a BOM, a trailing comma), a root that is not an object, and
/// values that are not strings or objects name nothing, as upstream reads
/// them.
fn composer_names_drupal(text: &str) -> bool {
    let Ok(Value::Object(json)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let starts = |key: &str, prefix: &str| {
        json.get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with(prefix))
    };
    if starts("name", "drupal/") || starts("type", "drupal-") {
        return true;
    }
    ["require", "require-dev"].iter().any(|key| {
        json.get(*key)
            .and_then(Value::as_object)
            .is_some_and(|deps| deps.keys().any(|dep| dep.starts_with("drupal/")))
    })
}

/// Whether `file_path` is a hook file (upstream `isDrupalHookFile`).
pub fn is_hook_file(file_path: &str) -> bool {
    HOOK_FILE_EXTENSIONS
        .iter()
        .any(|extension| file_path.ends_with(extension))
}

/// The PHP classes named `name` (KEEP-RUST: upstream takes any language's).
fn php_classes(name: &str, context: &dyn ResolutionContext) -> Vec<Arc<Node>> {
    context
        .get_nodes_by_name_shared(name)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Class && node.language == Language::Php)
        .collect()
}

/// The first method named `name` among one file's nodes, as upstream's
/// `getNodesInFile` orders them: by line; on one line by column, then id.
fn first_method<'n>(nodes: &'n [Arc<Node>], name: &str) -> Option<&'n Arc<Node>> {
    nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method && node.name == name)
        .min_by(|a, b| {
            (a.start_line, a.start_column, &a.id).cmp(&(b.start_line, b.start_column, &b.id))
        })
}

/// The last segment of a namespaced class name (upstream `lastSegment`),
/// when it has one.
fn last_segment(fqcn: &str) -> Option<&str> {
    let clean = js_trim(fqcn.trim_start_matches('\\'));
    if !clean.contains('\\') {
        return None;
    }
    clean.rsplit('\\').next().filter(|last| !last.is_empty())
}

/// The project's procedural hook implementations, read once per resolution
/// pass: every `hook_X` reference reads them all (upstream scans
/// `getNodesByKind('function')` per reference).
#[derive(Default)]
pub struct DrupalTables {
    hook_functions: OnceLock<Vec<Arc<Node>>>,
}

impl DrupalTables {
    pub fn new() -> Self {
        Self::default()
    }

    /// The PHP functions of hook files (KEEP-RUST: upstream takes a `.inc`
    /// file's functions in any language).
    fn hook_functions(&self, context: &dyn ResolutionContext) -> &[Arc<Node>] {
        self.hook_functions.get_or_init(|| {
            context
                .get_nodes_by_kind_shared(NodeKind::Function)
                .into_iter()
                .filter(|node| is_hook_function(node))
                .collect()
        })
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// The route a name line opened.
struct Pending {
    name: String,
    line: i64,
}

/// What a routing file's current route has read so far.
#[derive(Default)]
struct RouteState {
    pending: Option<Pending>,
    path: Option<String>,
    handlers: Vec<String>,
    methods: Vec<String>,
}

impl RouteState {
    /// Emit the pending route when it has a path (upstream `flushRoute`).
    fn flush(&mut self, file_path: &str, result: &mut FrameworkResolverExtractionResult) {
        let (Some(pending), Some(path)) = (self.pending.take(), self.path.take()) else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let tag = if self.methods.is_empty() {
            String::new()
        } else {
            format!(" [{}]", self.methods.join(","))
        };
        let node = framework_node(
            format!("route:{file_path}:{}:{path}", pending.line),
            NodeKind::Route,
            format!("{path}{tag}"),
            format!("{file_path}::{}", pending.name),
            file_path.to_string(),
            pending.line,
            pending.line,
            0,
            0,
            Language::Yaml,
            false,
        );
        for handler in self.handlers.drain(..) {
            result.references.push(RefView {
                row_id: None,
                from_node_id: node.id.clone(),
                reference_name: handler,
                reference_kind: EdgeKind::References,
                line: pending.line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Yaml,
                is_function_ref: false,
                reference_subkind: None,
            });
        }
        result.nodes.push(node);
    }
}

/// A `*.routing.yml` file's routes (upstream `extractDrupalRoutes`), read
/// line by line: a route per name line that a `path:` follows, referencing
/// its `_controller`, `_form` and entity handlers.
fn extract_routes(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    let mut state = RouteState::default();
    for (index, line) in content.split('\n').enumerate() {
        let trimmed = js_trim(line);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if route_start().is_match(line) {
            state.flush(file_path, &mut result);
            state = RouteState {
                pending: Some(Pending {
                    // The trimmed line ends with the colon.
                    name: js_trim(&trimmed[..trimmed.len() - 1]).to_string(),
                    line: index as i64 + 1,
                }),
                ..RouteState::default()
            };
            continue;
        }
        if let Some(found) = path_line().captures(trimmed) {
            state.path = Some(js_trim(&found[1]).to_string());
            continue;
        }
        if let Some(found) = controller_line()
            .captures(trimmed)
            .or_else(|| form_line().captures(trimmed))
            .or_else(|| entity_line().captures(trimmed))
        {
            state.handlers.push(js_trim(&found[1]).to_string());
            continue;
        }
        if let Some(found) = methods_line().captures(trimmed) {
            state.methods = found[1]
                .split(',')
                .map(|method| js_trim(method).to_uppercase())
                .filter(|method| !method.is_empty())
                .collect();
        }
    }
    state.flush(file_path, &mut result);
    result
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/// The functions declared at a line's start (upstream's
/// `/^function\s+(\w+)\s*\(/gm`, a line starting after `\n`, `\r`, U+2028
/// or U+2029), each name with its first one's 1-based line (`\n`s counted),
/// in the order they first appear.
fn line_start_functions(content: &str) -> Vec<(String, i64)> {
    let starts = std::iter::once(0).chain(
        content
            .char_indices()
            .filter(|&(_, ch)| matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            .map(|(at, ch)| at + ch.len_utf8()),
    );
    let mut functions: Vec<(String, i64)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut resume = 0;
    let mut counted = 0;
    let mut line = 1;
    for start in starts {
        if start < resume || !content[start..].starts_with("function") {
            continue;
        }
        let Some(found) = function_def().captures(&content[start..]) else {
            continue;
        };
        resume = start + found.get(0).expect("whole match").end();
        line += content.as_bytes()[counted..start]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as i64;
        counted = start;
        let name = found[1].to_string();
        if seen.insert(name.clone()) {
            functions.push((name, line));
        }
    }
    functions
}

/// The module a file belongs to (upstream `moduleNameFromPath`): its name
/// past the last `/` and before its last `.`; none for a file at the root.
fn module_name(file_path: &str) -> Option<&str> {
    let (_, base) = file_path.rsplit_once('/')?;
    let (stem, extension) = base.rsplit_once('.')?;
    (!stem.is_empty() && !extension.is_empty()).then_some(stem)
}

/// A hook file's or PHP file's hook implementations (upstream
/// `extractDrupalHooks`): a reference from each implementing function's node
/// to `hook_X`, by an `Implements hook_X()` docblock, else by the module's
/// name prefix.
fn extract_hooks(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    // Both strategies need a `function` declaration.
    if !content.contains("function") {
        return result;
    }
    let functions = line_start_functions(content);
    let line_of = |name: &str| {
        functions
            .iter()
            .find(|(function, _)| function == name)
            .map(|&(_, line)| line)
    };
    let mut emit = |hook: String, function: &str| {
        let Some(line) = line_of(function) else {
            return;
        };
        // The node the PHP extractor gives the declaration: the first on its
        // line, so it carries no `:{column}` suffix.
        let from = generate_node_id(
            file_path,
            NodeKind::Function,
            function,
            u32::try_from(line).unwrap_or(u32::MAX),
        );
        result.references.push(RefView {
            row_id: None,
            from_node_id: from,
            reference_name: hook,
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: file_path.to_string(),
            language: Language::Php,
            is_function_ref: false,
            reference_subkind: None,
        });
    };

    let mut documented: HashSet<String> = HashSet::new();
    if content.contains("Implements") {
        for found in docblock().captures_iter(content) {
            emit(found[1].to_string(), &found[2]);
            documented.insert(found[2].to_string());
        }
    }
    if let Some(module) = module_name(file_path) {
        let prefix = format!("{module}_");
        for (function, _) in &functions {
            if documented.contains(function) {
                continue;
            }
            let Some(suffix) = function.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if !suffix.is_empty() {
                emit(format!("hook_{suffix}"), function);
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// What a sync's Drupal lane reads (`codegraph_watch::drupal_refs`)
// ---------------------------------------------------------------------------

/// The class a Drupal reference's resolution reads by name: a controller's
/// class, else a bare namespaced class's last segment.
pub fn handler_class(name: &str) -> Option<&str> {
    if let Some(found) = controller().captures(name) {
        return found.get(1).map(|class| class.as_str());
    }
    if name.contains('\\') && !name.contains(':') {
        return last_segment(name);
    }
    None
}

/// Whether a function is a hook implementation `hook_X` references read:
/// a PHP function of a hook file.
pub fn is_hook_function(node: &Node) -> bool {
    node.kind == NodeKind::Function
        && node.language == Language::Php
        && is_hook_file(&node.file_path)
}

/// The `hook_X` names whose resolution reads a hook function named
/// `function`: one per suffix after each of its `_`s, as `hook_X` takes the
/// functions ending with `_X`.
pub fn hooks_reading(function: &str) -> impl Iterator<Item = String> + '_ {
    function
        .match_indices('_')
        .map(move |(at, _)| format!("hook_{}", &function[at + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn drupal(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        DrupalResolver
            .extract(
                file,
                src,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .unwrap()
    }

    fn names(result: &FrameworkResolverExtractionResult) -> Vec<&str> {
        result
            .references
            .iter()
            .map(|reference| reference.reference_name.as_str())
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

    fn class(file: &str, name: &str, methods: &[&str]) -> Vec<Node> {
        let mut nodes = vec![node(NodeKind::Class, name, file, 1, Language::Php)];
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

    fn routing_ref(name: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "route:x".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 1,
            column: 0,
            file_path: "mymodule.routing.yml".to_string(),
            language: Language::Yaml,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn target(ctx: &RouteCtx, name: &str) -> Option<(String, f64)> {
        DrupalResolver
            .resolve(&routing_ref(name), ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    fn id(kind: NodeKind, name: &str, file: &str, line: i64) -> String {
        format!("{}:{file}:{name}:{line}", kind.as_str())
    }

    fn composer(json: &str) -> RouteCtx {
        RouteCtx::default().unlisted("composer.json", json)
    }

    // -- detect ---------------------------------------------------------------

    #[test]
    fn detects_a_drupal_dependency_in_require_or_require_dev() {
        assert!(DrupalResolver.detect(&composer(
            r#"{"require": {"drupal/core-recommended": "~10.5", "drush/drush": "^13"}}"#
        )));
        assert!(DrupalResolver.detect(&composer(r#"{"require-dev": {"drupal/core": "^10"}}"#)));
        assert!(!DrupalResolver.detect(&composer(
            r#"{"require": {"laravel/framework": "^10", "php": ">=8.1"}}"#
        )));
        // A `require` that is not an object contributes no `drupal/` key.
        assert!(!DrupalResolver.detect(&composer(r#"{"require": "drupal/core"}"#)));
        assert!(!DrupalResolver.detect(&composer(r#"{"require": ["drupal/core"]}"#)));
    }

    #[test]
    fn detects_a_contrib_module_by_composer_name_or_type() {
        assert!(DrupalResolver.detect(&composer(
            r#"{"name": "drupal/admin_toolbar", "type": "drupal-module", "require": {}}"#
        )));
        assert!(DrupalResolver.detect(&composer(r#"{"name": "drupal/admin_toolbar"}"#)));
        assert!(DrupalResolver.detect(&composer(r#"{"type": "drupal-theme"}"#)));
        assert!(!DrupalResolver.detect(&composer(r#"{"name": ["drupal/x"]}"#)));
    }

    #[test]
    fn unreadable_composer_json_falls_through_to_the_files() {
        assert!(!DrupalResolver.detect(&RouteCtx::default()));
        for broken in [
            "{ bad json",
            "\u{FEFF}{\"name\": \"drupal/x\"}",
            r#"{"name": "drupal/x",}"#,
            "null",
            r#""drupal/x""#,
            "",
        ] {
            assert!(!DrupalResolver.detect(&composer(broken)), "{broken:?}");
            assert!(
                DrupalResolver.detect(
                    &composer(broken)
                        .file("mymodule/mymodule.info.yml", "")
                        .file("mymodule/mymodule.module", "")
                ),
                "{broken:?}"
            );
        }
    }

    #[test]
    fn detects_an_info_yml_beside_a_drupal_file_but_not_alone() {
        let ctx = RouteCtx::default()
            .file("mymodule/mymodule.info.yml", "")
            .file("mymodule/mymodule.routing.yml", "");
        assert!(DrupalResolver.detect(&ctx));
        for other in ["m/m.install", "t/t.theme"] {
            assert!(
                DrupalResolver
                    .detect(&RouteCtx::default().file("m/m.info.yml", "").file(other, ""))
            );
        }
        assert!(!DrupalResolver.detect(&RouteCtx::default().file("some/unrelated.info.yml", "")));
        // `.inc` alone is not a Drupal signature.
        assert!(
            !DrupalResolver.detect(
                &RouteCtx::default()
                    .file("m/m.info.yml", "")
                    .file("m/m.inc", "")
            )
        );
    }

    // -- claims ---------------------------------------------------------------

    #[test]
    fn claims_handler_class_names_and_hook_names() {
        for name in [
            r"\Drupal\m\Form\SettingsForm",
            r"\Drupal\m\Controller\C:setNoJsCookie",
            "hook_form_alter",
            "C::build",
            "service:method",
        ] {
            assert!(DrupalResolver.claims_reference(name), "{name}");
        }
        for name in ["someHelperFunction", "comment.default", "a:::b", "C::"] {
            assert!(!DrupalResolver.claims_reference(name), "{name}");
        }
    }

    /// A PHP import, or any reference outside a routing file, under one of
    /// the handler shapes (KEEP-RUST).
    fn php_ref(name: &str, kind: EdgeKind) -> RefView {
        RefView {
            row_id: None,
            from_node_id: "namespace:src/A.php:App:2".to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 3,
            column: 0,
            file_path: "src/A.php".to_string(),
            language: Language::Php,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    #[test]
    fn claims_only_a_routing_file_s_handlers_and_the_hook_names() {
        let handler = r"\Drupal\m\Controller\C::build";
        assert!(DrupalResolver.claims(&routing_ref(handler)));
        assert!(DrupalResolver.claims(&routing_ref(r"\Drupal\m\Form\SettingsForm")));
        assert!(!DrupalResolver.claims(&routing_ref("comment.default")));
        assert!(!DrupalResolver.claims(&php_ref(r"Foo\Bar", EdgeKind::Imports)));
        assert!(!DrupalResolver.claims(&php_ref(handler, EdgeKind::References)));
        // A YAML route of another file is no routing file's.
        let mut other = routing_ref(handler);
        other.file_path = "config/services.yml".to_string();
        assert!(!DrupalResolver.claims(&other));
        assert!(DrupalResolver.claims(&php_ref("hook_help", EdgeKind::References)));
    }

    #[test]
    fn a_handler_rule_answers_only_a_routing_file_s_reference() {
        let bar = "src/Baz/Bar.php";
        let ctx = RouteCtx::default().routes(bar, class(bar, "Bar", &["build"]));
        let resolve = |reference: RefView| {
            DrupalResolver
                .resolve(&reference, &ctx)
                .map(|resolved| (resolved.target_node_id, resolved.confidence))
        };
        assert_eq!(
            resolve(routing_ref(r"\Foo\Bar")),
            Some((id(NodeKind::Class, "Bar", bar, 1), 0.85))
        );
        assert_eq!(resolve(php_ref(r"Foo\Bar", EdgeKind::Imports)), None);
        assert_eq!(
            resolve(php_ref(r"\Foo\Bar::build", EdgeKind::References)),
            None
        );
    }

    // -- routing.yml ------------------------------------------------------------

    const ROUTING: &str = r"
mymodule.example:
  path: '/mymodule/example'
  defaults:
    _controller: '\Drupal\mymodule\Controller\MyController::build'
    _title: 'Example page'
  requirements:
    _permission: 'access content'
";

    #[test]
    fn a_route_node_per_route_with_upstreams_id_name_and_qualified_name() {
        let result = drupal("mymodule/mymodule.routing.yml", ROUTING);
        assert_eq!(result.nodes.len(), 1);
        let route = &result.nodes[0];
        assert_eq!(route.kind, NodeKind::Route);
        assert_eq!(route.name, "/mymodule/example");
        assert_eq!(
            route.qualified_name,
            "mymodule/mymodule.routing.yml::mymodule.example"
        );
        assert_eq!(
            route.id,
            "route:mymodule/mymodule.routing.yml:2:/mymodule/example"
        );
        assert_eq!((route.start_line, route.end_line), (2, 2));
        assert_eq!((route.start_column, route.end_column), (0, 0));
        assert_eq!(route.language, Language::Yaml);
        assert_eq!(
            names(&result),
            [r"\Drupal\mymodule\Controller\MyController::build"]
        );
        let reference = &result.references[0];
        assert_eq!(reference.reference_kind, EdgeKind::References);
        assert_eq!(reference.from_node_id, route.id);
        assert_eq!((reference.line, reference.column), (2, 0));
        assert_eq!(reference.language, Language::Yaml);
    }

    #[test]
    fn reads_form_and_entity_handlers() {
        let src = r"
mymodule.settings_form:
  path: '/admin/config/mymodule'
  defaults:
    _form: '\Drupal\mymodule\Form\SettingsForm'
    _entity_form: node.edit  # the node form
    _entity_list: 'node'
";
        let result = drupal("mymodule/mymodule.routing.yml", src);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(
            names(&result),
            [r"\Drupal\mymodule\Form\SettingsForm", "node.edit", "node"]
        );
    }

    #[test]
    fn handles_several_routes_comments_and_methods() {
        let src = r"
mod.page_one:
  path: '/page-one'
  defaults:
    #_controller: '\Drupal\mod\Controller\Old::build'
    _controller: '\Drupal\mod\Controller\PageController::one'
  methods: [get, Post ]

mod.page_two:
  path: /page-two # trailing comment
  defaults:
    _controller: '\Drupal\mod\Controller\PageController::two'
mod.no_path:
  defaults:
    _controller: '\Drupal\mod\Controller\PageController::three'
";
        let result = drupal("mod/mod.routing.yml", src);
        let routes: Vec<&str> = result.nodes.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(routes, ["/page-one [GET,POST]", "/page-two"]);
        assert_eq!(
            names(&result),
            [
                r"\Drupal\mod\Controller\PageController::one",
                r"\Drupal\mod\Controller\PageController::two"
            ]
        );
    }

    #[test]
    fn crlf_route_lines_and_files_with_no_routes() {
        let src = "mod.page:\r\n  path: '/page'\r\n";
        let result = drupal("mod/mod.routing.yml", src);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(
            result.nodes[0].qualified_name,
            "mod/mod.routing.yml::mod.page"
        );
        assert_eq!(result.nodes[0].name, "/page");
        let empty = drupal("some.routing.yml", "# empty\n");
        assert!(empty.nodes.is_empty() && empty.references.is_empty());
        assert!(drupal("mymodule.module", "<?php\n").nodes.is_empty());
        assert!(drupal("config/services.yml", ROUTING).nodes.is_empty());
    }

    // -- hooks --------------------------------------------------------------------

    const MODULE: &str = "web/modules/custom/mymodule/mymodule.module";

    #[test]
    fn a_docblock_names_the_hook_strategy_a() {
        let src = "<?php

/**
 * Implements hook_form_alter().
 */
function mymodule_form_alter(&$form, $form_state, $form_id) {
  // ...
}
";
        let result = drupal(MODULE, src);
        assert_eq!(names(&result), ["hook_form_alter"]);
        let reference = &result.references[0];
        assert_eq!(reference.reference_kind, EdgeKind::References);
        assert_eq!(reference.line, 6);
        assert_eq!(reference.column, 0);
        assert_eq!(reference.language, Language::Php);
        assert_eq!(
            reference.from_node_id,
            generate_node_id(MODULE, NodeKind::Function, "mymodule_form_alter", 6)
        );
    }

    #[test]
    fn the_module_prefix_names_the_hook_strategy_b() {
        let src = "<?php

function mymodule_views_data() {
  return [];
}
function other_module_helper() {}
  function mymodule_indented() {}
function mymodule_() {}
";
        assert_eq!(names(&drupal(MODULE, src)), ["hook_views_data"]);
    }

    #[test]
    fn install_and_theme_files_and_no_duplicate_for_both_strategies() {
        let install = "<?php
/**
 * Implements hook_schema().
 */
function mymodule_schema() {
  return [];
}
";
        assert_eq!(
            names(&drupal(
                "web/modules/custom/mymodule/mymodule.install",
                install
            )),
            ["hook_schema"]
        );
        let theme = "<?php
/**
 * Implements hook_preprocess_node().
 */
function mytheme_preprocess_node(&$variables) {}
";
        assert_eq!(
            names(&drupal("web/themes/custom/mytheme/mytheme.theme", theme)),
            ["hook_preprocess_node"]
        );
    }

    #[test]
    fn a_docblock_may_name_another_hook_and_lines_start_after_any_terminator() {
        // Strategy A names `hook_help` for `mymodule_info`, which Strategy B
        // then skips; a `\r` and U+2028 start lines, only `\n` counts them,
        // and a name's first declaration gives its line.
        let src = "<?php\n/**\n * Implements hook_help().\n */\nfunction mymodule_info() {}\rfunction mymodule_cron() {}\u{2028}function mymodule_init() {}\nfunction mymodule_cron() {}\n";
        let result = drupal(MODULE, src);
        assert_eq!(names(&result), ["hook_help", "hook_cron", "hook_init"]);
        let lines: Vec<i64> = result
            .references
            .iter()
            .map(|reference| reference.line)
            .collect();
        assert_eq!(lines, [5, 5, 5]);
        assert_eq!(
            result.references[0].from_node_id,
            generate_node_id(MODULE, NodeKind::Function, "mymodule_info", 5)
        );
    }

    #[test]
    fn a_one_line_docblock_is_not_strategy_a() {
        // Upstream's docblock wants `@` or `* ` before `Implements`.
        let src = "<?php\n/** Implements hook_help(). */\nfunction mymodule_info() {}\n";
        assert_eq!(names(&drupal(MODULE, src)), ["hook_info"]);
    }

    #[test]
    fn module_names_follow_the_path() {
        assert_eq!(module_name("a/b/mymodule.module"), Some("mymodule"));
        assert_eq!(module_name("a/foo.api.php"), Some("foo.api"));
        assert_eq!(module_name("mymodule.module"), None);
        assert_eq!(module_name("a/.module"), None);
        assert_eq!(module_name("a/README"), None);
        assert_eq!(module_name("a/x."), None);
    }

    // -- resolve ---------------------------------------------------------------------

    #[test]
    fn a_controller_resolves_to_the_method_else_the_class() {
        let file = "web/modules/custom/mymodule/src/Controller/MyController.php";
        let ctx = RouteCtx::default().routes(file, class(file, "MyController", &["build"]));
        assert_eq!(
            target(&ctx, r"\Drupal\mymodule\Controller\MyController::build"),
            Some((id(NodeKind::Method, "build", file, 3), 0.9))
        );
        // The single-colon service form, and a bare `Class::method`.
        assert_eq!(
            target(&ctx, r"\Drupal\mymodule\Controller\MyController:build"),
            Some((id(NodeKind::Method, "build", file, 3), 0.9))
        );
        assert_eq!(
            target(&ctx, "MyController::build"),
            Some((id(NodeKind::Method, "build", file, 3), 0.9))
        );
        assert_eq!(
            target(&ctx, r"\Drupal\mymodule\Controller\MyController::missing"),
            Some((id(NodeKind::Class, "MyController", file, 1), 0.7))
        );
        assert_eq!(target(&ctx, r"\Drupal\m\Controller\Missing::method"), None);
        // A namespace outside `Drupal\` is not a controller.
        assert_eq!(target(&ctx, r"\Other\Controller\MyController::build"), None);
    }

    #[test]
    fn same_named_controller_classes_take_the_one_whose_file_has_the_method() {
        let link = "core/modules/menu_link_content/src/Controller/MenuController.php";
        let ui = "core/modules/menu_ui/src/Controller/MenuController.php";
        let ctx = RouteCtx::default()
            .routes(link, class(link, "MenuController", &["addLink"]))
            .routes(
                ui,
                class(ui, "MenuController", &["addLink", "getParentOptions"]),
            );
        assert_eq!(
            target(
                &ctx,
                r"\Drupal\menu_ui\Controller\MenuController::getParentOptions"
            ),
            Some((id(NodeKind::Method, "getParentOptions", ui, 6), 0.9))
        );
        // Both hold it, or neither: upstream takes its store's first class,
        // Rust neither (KEEP-RUST).
        assert_eq!(target(&ctx, "MenuController::addLink"), None);
        assert_eq!(target(&ctx, "MenuController::missing"), None);
    }

    #[test]
    fn a_bare_class_name_resolves_to_the_only_php_class_of_that_name() {
        let form = "web/modules/custom/mymodule/src/Form/SettingsForm.php";
        let other = "web/modules/custom/other/src/Form/SettingsForm.php";
        let java = "src/main/java/SettingsForm.java";
        let mut java_class = class(java, "SettingsForm", &[]);
        java_class[0].language = Language::Java;
        let ctx = RouteCtx::default()
            .routes(form, class(form, "SettingsForm", &[]))
            .routes(java, java_class);
        assert_eq!(
            target(&ctx, r"\Drupal\mymodule\Form\SettingsForm"),
            Some((id(NodeKind::Class, "SettingsForm", form, 1), 0.85))
        );
        assert_eq!(target(&ctx, "SettingsForm"), None, "no namespace");
        assert_eq!(target(&ctx, r"\Drupal\mymodule\Form\"), None);
        let ctx = ctx.routes(other, class(other, "SettingsForm", &[]));
        assert_eq!(target(&ctx, r"\Drupal\mymodule\Form\SettingsForm"), None);
    }

    #[test]
    fn a_hook_resolves_to_the_one_php_implementation_in_a_hook_file() {
        let module = "modules/a/a.module";
        let other = "modules/b/b.module";
        let php = "modules/a/src/Helper.php";
        let pascal = "modules/a/a.inc";
        let pascal_function = node(NodeKind::Function, "a_cron", pascal, 1, Language::Pascal);
        let ctx = RouteCtx::default()
            .routes(
                module,
                vec![
                    node(NodeKind::Function, "a_help", module, 1, Language::Php),
                    node(NodeKind::Function, "a_form_alter", module, 5, Language::Php),
                ],
            )
            .routes(
                php,
                vec![node(
                    NodeKind::Function,
                    "helper_cron",
                    php,
                    1,
                    Language::Php,
                )],
            )
            .routes(pascal, vec![pascal_function])
            .routes(
                other,
                vec![node(
                    NodeKind::Function,
                    "b_form_alter",
                    other,
                    1,
                    Language::Php,
                )],
            );
        assert_eq!(
            target(&ctx, "hook_help"),
            Some((id(NodeKind::Function, "a_help", module, 1), 0.75))
        );
        // Two modules implement it: upstream takes its store's first, Rust
        // neither (KEEP-RUST).
        assert_eq!(target(&ctx, "hook_form_alter"), None);
        // Only a hook file's PHP functions count.
        assert_eq!(target(&ctx, "hook_cron"), None);
        assert_eq!(target(&ctx, "hook_missing"), None);
    }
}
