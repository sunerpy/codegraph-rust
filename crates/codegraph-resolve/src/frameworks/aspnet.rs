//! ASP.NET (upstream `resolution/frameworks/csharp.ts` at `e8ddc4e2`:
//! #2158's FastEndpoints routes and leading slash, `bc45e907`'s endpoint
//! groups, #2415's Razor references, #2225's name heuristic, #89's routes).
//!
//! A project is ASP.NET's when an indexed `.csproj` names ASP.NET, when its
//! root `Program.cs` builds a web host, when its root holds a `Startup.cs`
//! ([`ROOT_DETECTION_FILES`], the sync's `dotnet` control kind), or when an
//! indexed controller, entry point, endpoint or extension file shows
//! ASP.NET in its source.
//!
//! A `.cs` file's comment-stripped text gives four kinds of routes, each
//! with a reference to its handler by name: an action's verb attribute
//! (`[HttpGet("{id}")]`) under the class's `[Route]` prefix, referencing the
//! method it decorates; a minimal API (`app.MapGet("api/todos", GetTodos)`);
//! an endpoint group's route (`group.MapPost(CreateTodoItem)` in a class,
//! the class naming the group); and a FastEndpoints endpoint's
//! (`Get("/Contributors")` in an endpoint class's `Configure()`),
//! referencing the class's `HandleAsync` or `ExecuteAsync`.
//! [`AspnetResolver::post_extract`] names a FastEndpoints route whose path
//! is a constant (`Post(Request.Route)`) by the constant's value, and an
//! endpoint group's routes under the app's `MapGroup($"/api/{groupName}")`
//! head.
//!
//! Resolution guesses the class or interface a C# or Razor name
//! conventionally names, in upstream's order: `…Controller`, `…Service` or
//! `I…`, `…Repository`, a model, `…ViewModel` or `…Dto`, each through the
//! shared name heuristic ([`super::name_heuristic`]).
//!
//! Rust's own (KNOWN_DIFFS, KEEP-RUST): a rule whose tier declines (two
//! candidates, or one in another code family) ends the chain, as Spring's;
//! a constant renames its routes only when every class or struct of its name
//! that declares it gives the same value; and a route whose constant or
//! group head is gone gets back the name its own file gives it, so a sync
//! converges with a full index.
//!
//! Offsets are byte offsets. Comments are blanked byte for byte
//! ([`crate::strip_comments::strip_java_comments_keeping_offsets`]: upstream
//! strips C# as it strips Java), so an offset into the stripped text indexes
//! the source too; whatever upstream measures in UTF-16 code units (a
//! column, a window, a captured string) is measured on the source at the
//! same offsets ([`OffsetText`]).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::js_scan::{JS_SPACE, is_js_space};
use super::name_heuristic::{NamePick, PickOptions, pick_by_name_outcome};
use super::offset_text::OffsetText;
use super::{framework_node, sole};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver extracts (upstream `languages`).
const LANGUAGES: [Language; 1] = [Language::CSharp];

/// The languages whose references it resolves (upstream #2415
/// `resolveLanguages`): a Razor page's `@model CheckoutModel` names the
/// PageModel in the code-behind beside it.
const RESOLVE_LANGUAGES: [Language; 2] = [Language::CSharp, Language::Razor];

/// The root files detection reads by path, indexed or not: inputs of the
/// sync's `dotnet` control kind (`codegraph_watch::control_inputs`), since a
/// configuration may keep either out of the index.
pub const ROOT_DETECTION_FILES: [&str; 2] = ["Program.cs", "Startup.cs"];

/// What an indexed `.csproj` names when the project is ASP.NET's.
const PROJECT_MARKERS: [&str; 3] = [
    "Microsoft.AspNetCore",
    "Microsoft.NET.Sdk.Web",
    "System.Web.Mvc",
];

/// What a root `Program.cs` calls when it builds a web host.
const PROGRAM_MARKERS: [&str; 3] = ["WebApplication", "CreateHostBuilder", "UseStartup"];

/// What a controller, entry point, endpoint or extension file holds when it
/// is ASP.NET's, besides an ASP.NET attribute ([`source_attribute`]).
const SOURCE_MARKERS: [&str; 8] = [
    "ControllerBase",
    ": Controller",
    "MapControllers",
    "WebApplication",
    "Microsoft.AspNetCore",
    "IEndpointGroup",
    "EndpointGroupBase",
    "RouteGroupBuilder",
];

/// What a file holding an ASP.NET attribute ([`source_attribute`]) holds.
const ATTRIBUTE_NEEDLES: [&str; 7] = [
    "[ApiController",
    "[Route",
    "[HttpGet",
    "[HttpPost",
    "[HttpPut",
    "[HttpPatch",
    "[HttpDelete",
];

/// What a file's source holds when any of its four readings finds a route:
/// a verb attribute's `[Http`, a minimal API's or a group's `.Map`, an
/// endpoint's `Configure`. The comment stripper only ever blanks text, so
/// the stripped text holds none of them when the source does not.
const ROUTE_NEEDLES: [&str; 3] = ["[Http", ".Map", "Configure"];

/// The files detection and the group head's search read at once: a batch
/// that decides ends the scan, as upstream's one-by-one read ends at that
/// file.
const READ_BATCH: usize = 256;

/// How far after a verb attribute the method it decorates is looked for, in
/// UTF-16 code units (upstream's `slice(…, … + 600)`).
const ACTION_WINDOW: usize = 600;

/// How far after an endpoint's `Configure()` a verb call starts, in UTF-16
/// code units (upstream's `configure.index + 3000`).
const CONFIGURE_WINDOW: i64 = 3000;

/// How many files holding `MapGroup` the group head's search reads.
const HEAD_FILES: usize = 400;

/// The folders a convention prefers, as path substrings (upstream's
/// `…_DIRS`). Each starts with `/`, so a root folder is no preferred one.
const CONTROLLER_DIRS: [&str; 1] = ["/Controllers/"];
const SERVICE_DIRS: [&str; 3] = ["/Services/", "/Service/", "/Application/"];
const REPO_DIRS: [&str; 4] = [
    "/Repositories/",
    "/Repository/",
    "/Data/",
    "/Infrastructure/",
];
const MODEL_DIRS: [&str; 5] = ["/Models/", "/Model/", "/Entities/", "/Entity/", "/Domain/"];
const VIEWMODEL_DIRS: [&str; 4] = ["/ViewModels/", "/ViewModel/", "/DTOs/", "/Dto/"];

const CLASS_KINDS: [NodeKind; 1] = [NodeKind::Class];
const SERVICE_KINDS: [NodeKind; 2] = [NodeKind::Class, NodeKind::Interface];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), `{D}` its `.` (no line terminator), and its `\b` is
/// written `(?-u:\b)`.
fn aspnet_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_")
            .replace("{D}", r"[^\n\r\x{2028}\x{2029}]"),
    )
    .expect("aspnet pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| aspnet_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A source file detection reads by its name: a controller, an entry
    /// point, an endpoint or an extension.
    detection_file = r"(?:Controller|Program|Startup|Endpoints?|Extensions)\.cs$";
    /// A source file detection reads by its directory.
    detection_endpoints_file = r"(?:^|/)Endpoints/[^/]+\.cs$";
    /// An ASP.NET attribute in a source file.
    source_attribute = r"\[(?:ApiController|Route|Http(?:Get|Post|Put|Patch|Delete))(?-u:\b)";
    /// A model's name (upstream's Pattern 4).
    model_name = r"^[A-Z][a-zA-Z]+$";
    /// The class's `[Route("prefix")]`: one whose attributes and modifiers
    /// lead to `class`.
    class_route = r#"\[Route{S}*\({S}*"([^"]+)"[^)]*\)\]{S}*(?:\[[^\]]*\]{S}*)*(?:public{S}+|sealed{S}+|abstract{S}+|partial{S}+)*class(?-u:\b)"#;
    /// An action's verb attribute, bare or with a path.
    verb_attribute = r#"\[(HttpGet|HttpPost|HttpPut|HttpPatch|HttpDelete)(?:{S}*\({S}*"([^"]+)"[^)]*\))?{S}*\]"#;
    /// The method a verb attribute decorates: its access, return type and
    /// name.
    action_method = r"(?:public|private|protected|internal){S}+[{W}<>,{S}\[\]?.]+?{S}+([{W}]+){S}*\(";
    /// A minimal API, path first: `.MapGet("path", handler`.
    minimal_api = r#"\.Map(Get|Post|Put|Patch|Delete){S}*\({S}*"([^"]+)"{S}*,{S}*([^,)]+)"#;
    /// A handler expression's last identifier, once its spaces are gone
    /// (upstream `extractCSharpTailIdent`).
    tail_identifier = r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$";
    /// An endpoint group's route, handler first: `.MapPost(Handler)` or
    /// `.MapPut(Handler, "{id}")`.
    group_route = r#"\.Map(Get|Post|Put|Patch|Delete){S}*\({S}*([A-Za-z_][{W}]*){S}*(?:,{S}*"([^"]*)")?{S}*\)"#;
    /// A group class's own `RoutePrefix` literal.
    route_prefix_literal = r#"(?-u:\b)RoutePrefix{S}*(?:=>|=){S}*"([^"]+)""#;
    /// A `RoutePrefix` given a literal, in a group route's source.
    route_prefix_given = r#"(?-u:\b)RoutePrefix{S}*(?:=>|=){S}*""#;
    /// A class declaration.
    class_declaration = r"(?-u:\b)class{S}+([A-Za-z_][{W}]*)";
    /// A FastEndpoints endpoint class (`: Endpoint<…>`,
    /// `EndpointWithoutRequest`, `Ep.Req<…>.Res<…>`).
    endpoint_class = r"(?-u:\b)class{S}+([A-Za-z_][{W}]*)[^{;]*?:{S}*(?:FastEndpoints\.)?(?:Endpoint(?:WithoutRequest|WithoutResponse)?(?-u:\b)|Ep\.)";
    /// An endpoint's `Configure()`.
    configure_method = r"(?-u:\b)void{S}+Configure{S}*\({S}*\){S}*\{";
    /// An endpoint's handler.
    endpoint_handler = r"(?-u:\b)(HandleAsync|ExecuteAsync){S}*\(";
    /// A verb call in an endpoint's `Configure()`.
    verb_call = r"(?-u:\b)(Get|Post|Put|Patch|Delete){S}*\({S}*([^;]*?){S}*\){S}*;";
    /// A string literal in a verb call's arguments.
    string_literal = r#""([^"]+)""#;
    /// `{nameof(X)}` in a path: `X`.
    nameof_hole = r"\{{S}*nameof{S}*\({S}*([{W}]+){S}*\){S}*\}";
    /// A verb call's constant path, `X.Y`.
    constant_path = r"^([A-Za-z_][{W}]*)\.([A-Za-z_][{W}]*)$";
    /// A line holding only a closing brace: the end of `Configure()`.
    closing_brace_line = r"\n{S}*\}{S}*\n";
    /// A constant route's qualified name: its verb, class and field.
    constant_route_name = r"::fastendpoint:([A-Z]+):const:([A-Za-z_][{W}]*)\.([A-Za-z_][{W}]*)$";
    /// A group route's qualified name: its group, verb and sub-path.
    group_route_name = r"::group:([A-Za-z_][{W}]*):([A-Z]+):({D}*)$";
    /// The app's group head before a group's name: `$"/api/{groupName}"`.
    group_head_name = r#"\$"([^"{]*)\{{S}*(?:groupName|type\.Name|name|prefix){S}*\}""#;
    /// The app's group head before any hole: `MapGroup($"/api/{`.
    group_head_map = r#"MapGroup\({S}*\$"([^"{]*)\{"#;
}

pub struct AspnetResolver;

impl FrameworkResolver for AspnetResolver {
    fn name(&self) -> &str {
        "aspnet"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&RESOLVE_LANGUAGES)
    }

    /// An indexed `.csproj` naming ASP.NET, a root `Program.cs` building a
    /// web host, a root `Startup.cs`, or an indexed controller, entry point,
    /// endpoint or extension file whose source shows ASP.NET (upstream's
    /// order; `.csproj` files are no source files, so the first holds only
    /// when a configuration indexes them, in upstream too).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let files = context.get_all_files_shared();
        if files
            .iter()
            .filter(|file| file.ends_with(".csproj"))
            .any(|file| {
                context.read_file_shared(file).is_some_and(|content| {
                    PROJECT_MARKERS
                        .iter()
                        .any(|marker| content.contains(marker))
                })
            })
        {
            return true;
        }
        if context
            .read_file_shared(ROOT_DETECTION_FILES[0])
            .is_some_and(|program| {
                PROGRAM_MARKERS
                    .iter()
                    .any(|marker| program.contains(marker))
            })
        {
            return true;
        }
        if context.file_exists(ROOT_DETECTION_FILES[1]) {
            return true;
        }
        let sources: Vec<String> = files
            .iter()
            .filter(|file| {
                detection_file().is_match(file) || detection_endpoints_file().is_match(file)
            })
            .cloned()
            .collect();
        let needles: Vec<&str> = ATTRIBUTE_NEEDLES
            .iter()
            .chain(SOURCE_MARKERS.iter())
            .copied()
            .collect();
        sources.chunks(READ_BATCH).any(|batch| {
            context
                .files_containing(batch, &needles)
                .iter()
                .any(|file| {
                    context
                        .read_file_shared(file)
                        .is_some_and(|content| shows_aspnet(&content))
                })
        })
    }

    /// Upstream's five conventions in order; a rule that finds no candidate
    /// hands over to the next, one that declines ends the chain.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        let rules: [(bool, &[NodeKind], &[&str], f64); 5] = [
            // Pattern 1: a controller.
            (
                name.ends_with("Controller"),
                &CLASS_KINDS,
                &CONTROLLER_DIRS,
                0.85,
            ),
            // Pattern 2: a service, or an interface by its `I` (dependency
            // injection). JavaScript's `a || b && c`: `b && c` first.
            (
                name.ends_with("Service") || (name.starts_with('I') && name.len() > 1),
                &SERVICE_KINDS,
                &SERVICE_DIRS,
                0.85,
            ),
            // Pattern 3: a repository.
            (
                name.ends_with("Repository"),
                &SERVICE_KINDS,
                &REPO_DIRS,
                0.85,
            ),
            // Pattern 4: a model or an entity.
            (model_name().is_match(name), &CLASS_KINDS, &MODEL_DIRS, 0.7),
            // Pattern 5: a view model or a DTO.
            (
                name.ends_with("ViewModel") || name.ends_with("Dto"),
                &CLASS_KINDS,
                &VIEWMODEL_DIRS,
                0.8,
            ),
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
                &PickOptions::default(),
            ) {
                NamePick::Picked(target) => {
                    return Some(framework_ref(reference, target, confidence));
                }
                NamePick::Declined => return None,
                NamePick::NoCandidate => {}
            }
        }
        None
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if !file_path.ends_with(".cs")
            || !ROUTE_NEEDLES.iter().any(|needle| content.contains(needle))
        {
            return Some(FrameworkResolverExtractionResult::default());
        }
        let text = OffsetText::csharp(content);
        let source = Source {
            text: &text,
            file_path,
        };
        let mut result = FrameworkResolverExtractionResult::default();
        source.attribute_routes(&mut result);
        source.minimal_routes(&mut result);
        source.group_routes(&mut result);
        source.endpoint_routes(&mut result);
        Some(result)
    }

    /// Route names only the whole repository can give (upstream
    /// `postExtract`): a FastEndpoints route's constant path, an endpoint
    /// group's head. Every pass recomputes both from the routes' qualified
    /// names; a route whose constant or head is gone gets back its own
    /// file's name (KEEP-RUST: upstream keeps the last one it gave).
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> {
        let routes: Vec<Arc<Node>> = context
            .get_nodes_by_kind_shared(NodeKind::Route)
            .into_iter()
            .filter(|route| route.language == Language::CSharp)
            .collect();
        let mut updates = Vec::new();
        if routes.is_empty() {
            return Some(updates);
        }
        constant_route_names(&routes, context, &mut updates);
        group_route_names(&routes, context, &mut updates);
        Some(updates)
    }
}

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// Whether a source file's text shows ASP.NET (detection's last rule).
fn shows_aspnet(content: &str) -> bool {
    source_attribute().is_match(content)
        || SOURCE_MARKERS.iter().any(|marker| content.contains(marker))
}

/// A class-level prefix and an action's path joined into one `/path`
/// (upstream `joinCsPath`): each without its leading and trailing slashes,
/// the empty ones dropped.
pub(crate) fn join_cs_path(prefix: &str, sub: &str) -> String {
    let parts: Vec<&str> = [prefix, sub]
        .into_iter()
        .map(|part| part.trim_start_matches('/').trim_end_matches('/'))
        .filter(|part| !part.is_empty())
        .collect();
    format!("/{}", parts.join("/"))
}

/// A handler expression's last identifier (upstream
/// `extractCSharpTailIdent`): `MyService.Handler` is `Handler`, a lambda has
/// none.
fn tail_ident(expression: &str) -> Option<String> {
    let cleaned: String = expression.chars().filter(|&ch| !is_js_space(ch)).collect();
    tail_identifier()
        .captures(&cleaned)
        .map(|found| found[1].to_string())
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// The `.cs` file an extraction reads.
struct Source<'s> {
    text: &'s OffsetText<'s>,
    file_path: &'s str,
}

impl Source<'_> {
    /// A capture of the stripped text as upstream's holds it.
    fn capture(&self, found: Option<regex::Match<'_>>) -> String {
        found.map_or_else(String::new, |found| {
            self.text.upstream(found.start(), found.end())
        })
    }

    /// A route node on `line`, from column 0 to `end_column`.
    fn route(
        &self,
        id: String,
        name: String,
        qualified_name: String,
        line: i64,
        end_column: i64,
    ) -> Node {
        framework_node(
            id,
            NodeKind::Route,
            name,
            qualified_name,
            self.file_path.to_string(),
            line,
            line,
            0,
            end_column,
            Language::CSharp,
            false,
        )
    }

    /// A route's reference to its handler `name`.
    fn reference(&self, from: &str, name: String, line: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: from.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::References,
            line,
            column: 0,
            file_path: self.file_path.to_string(),
            language: Language::CSharp,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    /// `[HttpGet]`, `[HttpGet("path")]`, `[HttpPost("path", Name="x")]`,
    /// bare or with a path, under the class's first `[Route]` prefix, each
    /// referencing the next method declared within the window.
    fn attribute_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        let prefix = class_route()
            .captures(&text.safe)
            .map_or_else(String::new, |found| self.capture(found.get(1)));
        for found in verb_attribute().captures_iter(&text.safe) {
            let whole = found.get(0).expect("whole match");
            let method = found[1]
                .strip_prefix("Http")
                .unwrap_or(&found[1])
                .to_ascii_uppercase();
            let route_path = join_cs_path(&prefix, &self.capture(found.get(2)));
            let line = text.line_at(whole.start());
            let id = format!("route:{}:{line}:{method}:{route_path}", self.file_path);
            result.nodes.push(self.route(
                id.clone(),
                format!("{method} {route_path}"),
                format!("{}::route:{route_path}", self.file_path),
                line,
                text.units(whole.start(), whole.end()),
            ));
            // The match ends at its `]`, a whole character.
            let tail = text.units_after(whole.end(), ACTION_WINDOW);
            if let Some(handler) = action_method().captures(&tail) {
                result
                    .references
                    .push(self.reference(&id, handler[1].to_string(), line));
            }
        }
    }

    /// `app.MapGet("path", handler)`: the path from the root
    /// (`"api/todos"` serves `/api/todos`), the handler by its last
    /// identifier.
    fn minimal_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        for found in minimal_api().captures_iter(&text.safe) {
            let whole = found.get(0).expect("whole match");
            let method = found[1].to_ascii_uppercase();
            let route_path = join_cs_path("", &self.capture(found.get(2)));
            let line = text.line_at(whole.start());
            let id = format!("route:{}:{line}:{method}:{route_path}", self.file_path);
            result.nodes.push(self.route(
                id.clone(),
                format!("{method} {route_path}"),
                format!("{}::route:{route_path}", self.file_path),
                line,
                text.units(whole.start(), whole.end()),
            ));
            if let Some(handler) = tail_ident(&self.capture(found.get(3))) {
                result.references.push(self.reference(&id, handler, line));
            }
        }
    }

    /// The endpoint-group idiom, handler first (Jason Taylor's Clean
    /// Architecture template): the class before the call is the group, its
    /// `RoutePrefix` literal or `/{group}` the prefix; the app's head is
    /// read across files ([`group_route_names`]).
    fn group_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        let prefix_literal = route_prefix_literal()
            .captures(&text.safe)
            .map(|found| self.capture(found.get(1)));
        // Upstream takes the last class declared in the text before the
        // call: no declaration runs across the call's `.`, so the last of
        // the whole text's declarations ending before it.
        let classes: Vec<(usize, String)> = class_declaration()
            .captures_iter(&text.safe)
            .map(|found| {
                (
                    found.get(0).expect("whole match").end(),
                    self.capture(found.get(1)),
                )
            })
            .collect();
        for found in group_route().captures_iter(&text.safe) {
            let whole = found.get(0).expect("whole match");
            let declared = classes.partition_point(|(end, _)| *end <= whole.start());
            let Some((_, group)) = declared.checked_sub(1).map(|at| &classes[at]) else {
                continue;
            };
            let method = found[1].to_ascii_uppercase();
            let sub = self.capture(found.get(3));
            let group_prefix = format!("/{group}");
            let route_path = join_cs_path(prefix_literal.as_deref().unwrap_or(&group_prefix), &sub);
            let line = text.line_at(whole.start());
            let id = format!("route:{}:{line}:{method}:{route_path}", self.file_path);
            result.nodes.push(self.route(
                id.clone(),
                format!("{method} {route_path}"),
                format!("{}::group:{group}:{method}:{sub}", self.file_path),
                line,
                text.units(whole.start(), whole.end()),
            ));
            result
                .references
                .push(self.reference(&id, self.capture(found.get(2)), line));
        }
    }

    /// A FastEndpoints endpoint class's routes: each verb call in its
    /// `Configure()`, by its literal paths or a constant's name, each
    /// referencing the class's `HandleAsync` or `ExecuteAsync`.
    fn endpoint_routes(&self, result: &mut FrameworkResolverExtractionResult) {
        let text = self.text;
        for class in endpoint_class().find_iter(&text.safe) {
            let at = class.start();
            // Upstream reads the rest of the file from the class on.
            let body = &text.safe[at..];
            let Some(configure) = configure_method().find(body) else {
                continue;
            };
            let handler = endpoint_handler()
                .captures(body)
                .map(|found| found[1].to_string());
            let mut from = configure.start();
            while let Some(verb) = verb_call().captures_at(body, from) {
                let whole = verb.get(0).expect("whole match");
                if text.units(at + configure.start(), at + whole.start()) >= CONFIGURE_WINDOW {
                    break;
                }
                let method = verb[1].to_ascii_uppercase();
                let args = verb.get(2).expect("arguments");
                let args = text.upstream(at + args.start(), at + args.end());
                let literals: Vec<String> = string_literal()
                    .captures_iter(&args)
                    .map(|literal| {
                        join_cs_path("", &nameof_hole().replace_all(&literal[1], "${1}"))
                    })
                    .collect();
                let targets: Vec<(String, String)> = if !literals.is_empty() {
                    literals
                        .into_iter()
                        .map(|path| (path.clone(), path))
                        .collect()
                } else if let Some(constant) = constant_path().captures(&args) {
                    let name = format!("{}.{}", &constant[1], &constant[2]);
                    vec![(name.clone(), format!("const:{name}"))]
                } else {
                    Vec::new()
                };
                let line = text.line_at(at + whole.start());
                for (name, key) in targets {
                    let id = format!("route:{}:{line}:{method}:{name}", self.file_path);
                    result.nodes.push(self.route(
                        id.clone(),
                        format!("{method} {name}"),
                        format!("{}::fastendpoint:{method}:{key}", self.file_path),
                        line,
                        0,
                    ));
                    if let Some(handler) = &handler {
                        result
                            .references
                            .push(self.reference(&id, handler.clone(), line));
                    }
                }
                // A second verb call is a second route; upstream stops after
                // the first past `Configure()`'s closing brace.
                if closing_brace_line().is_match(&body[configure.start()..whole.start()]) {
                    break;
                }
                from = whole.end();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-file names
// ---------------------------------------------------------------------------

/// A FastEndpoints route named by a constant (`Post(Request.Route)`) gets the
/// constant's value, `const string Route = "/x";` in a C# class or struct of
/// that name wherever it is declared (upstream
/// `fastEndpointConstantRoutes`). Upstream takes the first such class in its
/// store's order; Rust renames only when they all give one value, and gives
/// the route back its file's name (`POST Request.Route`) otherwise.
fn constant_route_names(
    routes: &[Arc<Node>],
    context: &dyn ResolutionContext,
    updates: &mut Vec<Node>,
) {
    let mut values: HashMap<(String, String), Option<String>> = HashMap::new();
    for route in routes {
        let Some(found) = constant_route_name().captures(&route.qualified_name) else {
            continue;
        };
        let (method, owner, field) = (&found[1], &found[2], &found[3]);
        let value = values
            .entry((owner.to_string(), field.to_string()))
            .or_insert_with(|| constant_value(owner, field, context));
        let name = match value {
            Some(value) => format!("{method} {}", join_cs_path("", value)),
            None => format!("{method} {owner}.{field}"),
        };
        if name != route.name {
            let mut updated = Node::clone(route);
            updated.name = name;
            updates.push(updated);
        }
    }
}

/// The one value the C# classes and structs named `owner` give their string
/// constant `field`, read from their files' sources, or `None` when none
/// does or they give several.
fn constant_value(owner: &str, field: &str, context: &dyn ResolutionContext) -> Option<String> {
    let declaration = aspnet_regex(
        &(String::from(
            r"(?-u:\b)(?:const{S}+string|static{S}+readonly{S}+string|static{S}+string){S}+",
        ) + &regex::escape(field)
            + r#"{S}*={S}*"([^"]+)""#),
    );
    let values: BTreeSet<String> = context
        .get_nodes_by_name_shared(owner)
        .iter()
        .filter(|node| {
            node.language == Language::CSharp
                && matches!(node.kind, NodeKind::Class | NodeKind::Struct)
        })
        .filter_map(|node| {
            let content = context.read_file_shared(&node.file_path)?;
            declaration
                .captures(&content)
                .map(|found| found[1].to_string())
        })
        .collect();
    sole(values)
}

/// An endpoint group's routes named under the app's head (upstream
/// `endpointGroupRoutes`): `POST /TodoItems` becomes `POST /api/TodoItems`.
/// A route whose file gives a `RoutePrefix` literal keeps its file's name; a
/// route with no head now gets back its file's, `/{group}` (KEEP-RUST).
fn group_route_names(
    routes: &[Arc<Node>],
    context: &dyn ResolutionContext,
    updates: &mut Vec<Node>,
) {
    let grouped: Vec<(&Arc<Node>, regex::Captures<'_>)> = routes
        .iter()
        .filter_map(|route| {
            group_route_name()
                .captures(&route.qualified_name)
                .map(|found| (route, found))
        })
        .collect();
    if grouped.is_empty() {
        return;
    }
    let head = group_head(context);
    for (route, found) in grouped {
        if context
            .read_file_shared(&route.file_path)
            .is_some_and(|content| route_prefix_given().is_match(&content))
        {
            continue;
        }
        let (group, method, sub) = (&found[1], &found[2], &found[3]);
        let prefix = match &head {
            Some(head) => format!("{}/{group}", head.trim_end_matches('/')),
            None => format!("/{group}"),
        };
        let name = format!("{method} {}", join_cs_path(&prefix, sub));
        if name != route.name {
            let mut updated = Node::clone(route);
            updated.name = name;
            updates.push(updated);
        }
    }
}

/// The app's group head (`/api/` in `MapGroup($"/api/{groupName}")`): the
/// first of the `.cs` files holding `MapGroup`, in path order and at most
/// [`HEAD_FILES`] of them, that writes one decides; `None` when none does
/// or its head is `/` or empty.
fn group_head(context: &dyn ResolutionContext) -> Option<String> {
    let sources: Vec<String> = context
        .get_all_files_shared()
        .iter()
        .filter(|file| file.ends_with(".cs"))
        .cloned()
        .collect();
    let mut looked = 0;
    for batch in sources.chunks(READ_BATCH) {
        for file in context.files_containing(batch, &["MapGroup"]) {
            looked += 1;
            if looked > HEAD_FILES {
                return None;
            }
            let Some(content) = context.read_file_shared(&file) else {
                continue;
            };
            if let Some(found) = group_head_name()
                .captures(&content)
                .or_else(|| group_head_map().captures(&content))
            {
                let head = &found[1];
                return (!head.is_empty() && head != "/").then(|| head.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn aspnet(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        AspnetResolver
            .extract(
                file,
                src,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .unwrap()
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

    /// Each route's name and the handler it references, in node order.
    fn routes_and_handlers(
        result: &FrameworkResolverExtractionResult,
    ) -> Vec<(String, Option<String>)> {
        result
            .nodes
            .iter()
            .map(|node| {
                let handler = result
                    .references
                    .iter()
                    .find(|reference| reference.from_node_id == node.id)
                    .map(|reference| reference.reference_name.clone());
                (node.name.clone(), handler)
            })
            .collect()
    }

    fn csharp(kind: NodeKind, name: &str, file: &str, line: i64) -> Node {
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
            Language::CSharp,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn reference_in(file: &str, name: &str, language: Language) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("file:{file}"),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line: 3,
            column: 4,
            file_path: file.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn resolve(ctx: &RouteCtx, file: &str, name: &str) -> Option<(String, f64)> {
        AspnetResolver
            .resolve(&reference_in(file, name, Language::CSharp), ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    fn ctx_with(nodes: Vec<Node>) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for node in nodes {
            let file = node.file_path.clone();
            ctx = ctx.routes(&file, vec![node]);
        }
        ctx
    }

    // -- Extraction: upstream frameworks.test.ts 1777-1830, 2173-2187 -----

    #[test]
    fn extracts_route_from_http_get_attribute() {
        let src =
            "\n[HttpGet(\"/users\")]\npublic IActionResult ListUsers()\n{\n  return Ok();\n}\n";
        let result = aspnet("UserController.cs", src);
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(targets(&result), ["ListUsers"]);
        let node = &result.nodes[0];
        assert_eq!(node.id, "route:UserController.cs:2:GET:/users");
        assert_eq!(node.qualified_name, "UserController.cs::route:/users");
        assert_eq!((node.start_line, node.end_line), (2, 2));
        assert_eq!((node.start_column, node.end_column), (0, 19));
        assert_eq!(node.language, Language::CSharp);
        let reference = &result.references[0];
        assert_eq!(reference.from_node_id, node.id);
        assert_eq!(reference.reference_kind, EdgeKind::References);
        assert_eq!((reference.line, reference.column), (2, 0));
        assert_eq!(reference.language, Language::CSharp);
    }

    #[test]
    fn extracts_the_handler_first_endpoint_group_form_under_the_class() {
        let src = "
public class TodoItems : IEndpointGroup
{
    public static void Map(RouteGroupBuilder groupBuilder)
    {
        groupBuilder.RequireAuthorization();
        groupBuilder.MapPost(CreateTodoItem);
        groupBuilder.MapPut(UpdateTodoItem, \"{id}\");
        groupBuilder.MapDelete(DeleteTodoItem, \"{id}\");
    }
    public static async Task<Created<int>> CreateTodoItem(ISender sender, CreateTodoItemCommand command) { }
}
";
        let result = aspnet("Web/Endpoints/TodoItems.cs", src);
        assert_eq!(
            names(&result),
            [
                "POST /TodoItems",
                "PUT /TodoItems/{id}",
                "DELETE /TodoItems/{id}"
            ]
        );
        assert_eq!(
            result
                .nodes
                .iter()
                .map(|node| node.start_line)
                .collect::<Vec<_>>(),
            [7, 8, 9]
        );
        assert_eq!(
            targets(&result),
            ["CreateTodoItem", "UpdateTodoItem", "DeleteTodoItem"]
        );
        assert_eq!(
            result.nodes[0].qualified_name,
            "Web/Endpoints/TodoItems.cs::group:TodoItems:POST:"
        );
        assert_eq!(
            result.nodes[1].qualified_name,
            "Web/Endpoints/TodoItems.cs::group:TodoItems:PUT:{id}"
        );
        assert_eq!(
            result.nodes[0].end_column,
            ".MapPost(CreateTodoItem)".len() as i64
        );
    }

    #[test]
    fn a_class_with_its_own_route_prefix_literal_names_its_routes_under_it() {
        let src = "
public class TodoLists : IEndpointGroup
{
    public static string RoutePrefix => \"/api/todo-lists\";
    public static void Map(RouteGroupBuilder group)
    {
        group.MapGet(GetTodoLists);
    }
}
";
        assert_eq!(names(&aspnet("TodoLists.cs", src)), ["GET /api/todo-lists"]);
    }

    #[test]
    fn skips_line_and_block_commented_http_attributes() {
        let src = "
// [HttpGet(\"/fake\")]
// public IActionResult Fake() { return Ok(); }

/* [HttpPost(\"/also-fake\")]
   public IActionResult AlsoFake() { return Ok(); } */

[HttpGet(\"/real\")]
public IActionResult ListUsers() { return Ok(); }
";
        let result = aspnet("UserController.cs", src);
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(targets(&result), ["ListUsers"]);
        assert_eq!(result.nodes[0].start_line, 8);
    }

    // -- Extraction: attribute routes -----------------------------------------

    #[test]
    fn the_class_route_prefixes_bare_and_pathed_attributes() {
        let src = "
[ApiController]
[Route(\"api/[controller]\")]
public sealed class UsersController : ControllerBase
{
    [HttpGet]
    public IActionResult List() => Ok();

    [HttpGet(\"{id}\", Name = \"GetUser\")]
    [ProducesResponseType(200)]
    public async Task<ActionResult<UserDto>> Get(int id) => Ok();

    [HttpPost(\"/absolute/\")]
    public IActionResult Create([FromBody] UserDto user) => Ok();

    [HttpPatch( \"{id}/name\" )]
    protected internal virtual IActionResult Rename(int id) => Ok();

    [HttpDelete]
    internal IActionResult Remove() => Ok();
}
";
        let result = aspnet("Controllers/UsersController.cs", src);
        assert_eq!(
            routes_and_handlers(&result),
            [
                (
                    "GET /api/[controller]".to_string(),
                    Some("List".to_string())
                ),
                (
                    "GET /api/[controller]/{id}".to_string(),
                    Some("Get".to_string())
                ),
                (
                    "POST /api/[controller]/absolute".to_string(),
                    Some("Create".to_string())
                ),
                (
                    "PATCH /api/[controller]/{id}/name".to_string(),
                    Some("Rename".to_string())
                ),
                (
                    "DELETE /api/[controller]".to_string(),
                    Some("Remove".to_string())
                ),
            ]
        );
        assert_eq!(
            result.nodes[0].qualified_name,
            "Controllers/UsersController.cs::route:/api/[controller]"
        );
        assert_eq!(result.nodes[0].end_column, "[HttpGet]".len() as i64);
    }

    #[test]
    fn only_a_route_on_a_class_is_its_prefix() {
        let src = "
public class ReportsController : Controller
{
    [Route(\"reports\")]
    [HttpGet(\"summary\")]
    public IActionResult Summary() => View();
}
";
        assert_eq!(
            names(&aspnet("ReportsController.cs", src)),
            ["GET /summary"]
        );
    }

    #[test]
    fn the_first_class_route_prefixes_every_class_of_the_file() {
        let src = "
[Route(\"a\")]
public class A : Controller { [HttpGet(\"x\")] public IActionResult X() => View(); }
[Route(\"b\")]
public class B : Controller { [HttpGet(\"y\")] public IActionResult Y() => View(); }
";
        assert_eq!(
            names(&aspnet("Controllers.cs", src)),
            ["GET /a/x", "GET /a/y"]
        );
    }

    #[test]
    fn the_class_route_may_be_followed_by_attributes_and_modifiers() {
        let src = "
[Route(\"admin\")]
[ApiController]
public abstract partial class AdminController : Controller
{
    [HttpGet(\"x\")]
    public IActionResult X() => View();
}
";
        assert_eq!(names(&aspnet("AdminController.cs", src)), ["GET /admin/x"]);
    }

    #[test]
    fn every_access_modifier_leads_to_an_action() {
        let src = "
public class ItemsController : ControllerBase
{
    [HttpGet(\"a\")]
    private IActionResult A() => Ok();

    [HttpGet(\"b\")]
    protected IActionResult B() => Ok();
}
";
        assert_eq!(targets(&aspnet("ItemsController.cs", src)), ["A", "B"]);
    }

    #[test]
    fn every_verb_names_a_route_in_each_form() {
        let verbs = ["Get", "Post", "Put", "Patch", "Delete"];
        let each =
            |line: &dyn Fn(&str) -> String| verbs.iter().map(|verb| line(verb)).collect::<String>();
        let attributes =
            each(&|verb| format!("[Http{verb}(\"a\")]\npublic IActionResult {verb}A() => Ok();\n"));
        let minimal = each(&|verb| format!("app.Map{verb}(\"/b\", B);\n"));
        let group = format!(
            "public class C : IEndpointGroup\n{{\n    public static void Map(RouteGroupBuilder g)\n    {{\n{}    }}\n}}\n",
            each(&|verb| format!("        g.Map{verb}(C{verb});\n"))
        );
        let endpoint = format!(
            "public class D : Endpoint<R>\n{{\n  public override void Configure()\n  {{\n{}  }}\n}}\n",
            each(&|verb| format!("    {verb}(\"/d\");\n"))
        );
        for (file, src, path) in [
            ("Controllers/AController.cs", attributes, "/a"),
            ("Program.cs", minimal, "/b"),
            ("Endpoints/C.cs", group, "/C"),
            ("D.cs", endpoint, "/d"),
        ] {
            let expected: Vec<String> = ["GET", "POST", "PUT", "PATCH", "DELETE"]
                .iter()
                .map(|method| format!("{method} {path}"))
                .collect();
            assert_eq!(names(&aspnet(file, &src)), expected, "{file}");
        }
    }

    #[test]
    fn an_action_method_is_looked_for_within_600_utf16_units() {
        let source = |filler: String, method: &str| {
            format!("[HttpGet]\n{filler}\npublic IActionResult {method}() {{}}\n")
        };
        let emoji = |count: usize| "\u{1F600}".repeat(count);
        // An emoji is two code units, blanked in a comment and kept in a
        // string literal: the far method's `(` is past the window in code
        // units (611 and 627 of them), not in characters.
        for (near, far) in [
            (format!("/*{}*/", emoji(280)), format!("/*{}*/", emoji(290))),
            (
                format!("const string S = \"{}\";", emoji(270)),
                format!("const string S = \"{}\";", emoji(290)),
            ),
        ] {
            assert_eq!(
                targets(&aspnet("NearController.cs", &source(near, "Near"))),
                ["Near"]
            );
            let result = aspnet("FarController.cs", &source(far, "Far"));
            assert_eq!(names(&result), ["GET /"]);
            assert!(result.references.is_empty(), "{:?}", targets(&result));
        }
    }

    #[test]
    fn a_non_ascii_method_name_is_no_handler() {
        // JavaScript's `\w` is ASCII: `Listé` is no `(\w+)\s*\(`.
        let src = "[HttpGet]\npublic IActionResult List\u{e9}() => Ok();\n";
        let result = aspnet("UsersController.cs", src);
        assert_eq!(names(&result), ["GET /"]);
        assert!(result.references.is_empty(), "{:?}", targets(&result));
    }

    #[test]
    fn javascript_spaces_separate_an_attribute_and_its_path() {
        // U+FEFF is JavaScript's `\s`, U+0085 is not.
        let bom = aspnet(
            "A.cs",
            "[HttpGet(\u{feff}\"/x\")]\npublic IActionResult X() => Ok();\n",
        );
        assert_eq!(names(&bom), ["GET /x"]);
        let nel = aspnet(
            "B.cs",
            "[HttpGet(\u{85}\"/x\")]\npublic IActionResult X() => Ok();\n",
        );
        assert!(nel.nodes.is_empty(), "{:?}", names(&nel));
    }

    #[test]
    fn an_attribute_spanning_a_comment_measures_its_columns_in_utf16_units() {
        let attribute = "[HttpGet(/*\u{1F600}*/\"/x\")]";
        let result = aspnet(
            "X.cs",
            &format!("{attribute}\npublic IActionResult X() => Ok();\n"),
        );
        assert_eq!(names(&result), ["GET /x"]);
        assert_eq!(
            result.nodes[0].end_column,
            attribute.encode_utf16().count() as i64
        );
        assert_ne!(result.nodes[0].end_column, attribute.len() as i64);
        // A path's supplementary character is kept: two code units, one
        // character.
        let kept = "[HttpGet(\"/\u{1F600}\")]";
        let result = aspnet(
            "Y.cs",
            &format!("{kept}\npublic IActionResult Y() => Ok();\n"),
        );
        assert_eq!(names(&result), ["GET /\u{1F600}"]);
        assert_eq!(
            result.nodes[0].end_column,
            kept.encode_utf16().count() as i64
        );
    }

    // -- Extraction: minimal APIs and endpoint groups -----------------------

    #[test]
    fn a_minimal_api_path_starts_at_the_root_and_names_its_handler_by_its_tail() {
        let src = "
var app = builder.Build();
app.MapGet(\"api/todos\", GetTodos);
app.MapPost(\"/todos/\", TodoHandlers.Create);
app.MapPut(\"/todos/{id}\", TodoHandlers . Update );
app.MapGet(\"/hello\", () => Greeter.Greet());
app.MapDelete(\"/todos/{id}\", (int id) => Results.NoContent());
";
        let result = aspnet("Program.cs", src);
        assert_eq!(
            routes_and_handlers(&result),
            [
                ("GET /api/todos".to_string(), Some("GetTodos".to_string())),
                ("POST /todos".to_string(), Some("Create".to_string())),
                ("PUT /todos/{id}".to_string(), Some("Update".to_string())),
                ("GET /hello".to_string(), None),
                ("DELETE /todos/{id}".to_string(), None),
            ]
        );
        assert_eq!(result.nodes[0].id, "route:Program.cs:3:GET:/api/todos");
        assert_eq!(
            result.nodes[0].qualified_name,
            "Program.cs::route:/api/todos"
        );
        assert_eq!(
            result.nodes[0].end_column,
            ".MapGet(\"api/todos\", GetTodos".len() as i64
        );
    }

    #[test]
    fn a_group_route_belongs_to_the_last_class_before_it() {
        let src = "
app.MapGet(Orphan);
public class First : IEndpointGroup
{
    public static void Map(RouteGroupBuilder g) { g.MapGet(ListFirst); }
}
public class Second : IEndpointGroup
{
    public static void Map(RouteGroupBuilder g) { g.MapGet(ListSecond, \"all\"); }
}
";
        let result = aspnet("Endpoints/Groups.cs", src);
        assert_eq!(
            routes_and_handlers(&result),
            [
                ("GET /First".to_string(), Some("ListFirst".to_string())),
                (
                    "GET /Second/all".to_string(),
                    Some("ListSecond".to_string())
                ),
            ]
        );
    }

    #[test]
    fn minimal_api_and_group_routes_measure_their_columns_in_utf16_units() {
        let minimal = ".MapGet(\"/caf\u{e9}/\u{1F600}\", Cafe";
        let result = aspnet("Program.cs", &format!("app{minimal});\n"));
        assert_eq!(names(&result), ["GET /caf\u{e9}/\u{1F600}"]);
        assert_eq!(
            result.nodes[0].end_column,
            minimal.encode_utf16().count() as i64
        );
        let group = ".MapGet(List, \"caf\u{e9}/\u{1F600}\")";
        let result = aspnet(
            "Endpoints/Menu.cs",
            &format!(
                "public class Menu : IEndpointGroup\n{{\n    public static void Map(RouteGroupBuilder g) {{ g{group}; }}\n}}\n"
            ),
        );
        assert_eq!(names(&result), ["GET /Menu/caf\u{e9}/\u{1F600}"]);
        assert_eq!(
            result.nodes[0].end_column,
            group.encode_utf16().count() as i64
        );
    }

    // -- Extraction: FastEndpoints (fastendpoints-routes.test.ts) ----------

    const CREATE: &str = "namespace Web.Contributors;

public class Create : Endpoint<CreateContributorRequest, CreateContributorResponse>
{
  public override void Configure()
  {
    Post(CreateContributorRequest.Route);
    AllowAnonymous();
  }

  public override async Task ExecuteAsync(CreateContributorRequest request, CancellationToken ct)
  {
  }
}
";

    const LIST: &str = "namespace Web.Contributors;

public class List : EndpointWithoutRequest<ContributorListResponse>
{
  public override void Configure()
  {
    Get(\"/Contributors\");
    AllowAnonymous();
  }

  public override async Task HandleAsync(CancellationToken ct)
  {
  }
}
";

    const PROJECTS: &str = "namespace Web.Projects;

public class List : EndpointWithoutRequest<ProjectListResponse>
{
  public override void Configure()
  {
    Get($\"/{nameof(Project)}s\");
  }

  public override async Task HandleAsync(CancellationToken ct)
  {
  }
}
";

    #[test]
    fn a_fastendpoints_route_is_named_by_its_literal_or_constant_and_references_its_handler() {
        let create = aspnet("src/Web/Contributors/Create.cs", CREATE);
        assert_eq!(
            routes_and_handlers(&create),
            [(
                "POST CreateContributorRequest.Route".to_string(),
                Some("ExecuteAsync".to_string())
            )]
        );
        let node = &create.nodes[0];
        assert_eq!(
            node.id,
            "route:src/Web/Contributors/Create.cs:7:POST:CreateContributorRequest.Route"
        );
        assert_eq!(
            node.qualified_name,
            "src/Web/Contributors/Create.cs::fastendpoint:POST:const:CreateContributorRequest.Route"
        );
        assert_eq!((node.start_column, node.end_column), (0, 0));
        let list = aspnet("src/Web/Contributors/List.cs", LIST);
        assert_eq!(
            routes_and_handlers(&list),
            [(
                "GET /Contributors".to_string(),
                Some("HandleAsync".to_string())
            )]
        );
        assert_eq!(
            list.nodes[0].qualified_name,
            "src/Web/Contributors/List.cs::fastendpoint:GET:/Contributors"
        );
        let projects = aspnet("src/Web/Projects/List.cs", PROJECTS);
        assert_eq!(
            routes_and_handlers(&projects),
            [("GET /Projects".to_string(), Some("HandleAsync".to_string()))]
        );
    }

    #[test]
    fn a_second_verb_is_a_second_route_and_the_first_past_configure_ends_them() {
        let src = "
public class Items : Ep.Req<ItemRequest>.Res<ItemResponse>
{
  public override void Configure()
  {
    Get(\"/items\", \"/items/all\");
    Put(\"/items/{id}\");
  }
  void Helper() { Post(\"/after\"); }
  void Other() { Delete(\"/later\"); }
  public override async Task HandleAsync(ItemRequest r, CancellationToken ct) { }
}
";
        assert_eq!(
            routes_and_handlers(&aspnet("Items.cs", src)),
            [
                ("GET /items".to_string(), Some("HandleAsync".to_string())),
                (
                    "GET /items/all".to_string(),
                    Some("HandleAsync".to_string())
                ),
                (
                    "PUT /items/{id}".to_string(),
                    Some("HandleAsync".to_string())
                ),
                ("POST /after".to_string(), Some("HandleAsync".to_string())),
            ]
        );
    }

    #[test]
    fn a_verb_call_starts_within_3000_utf16_units_of_configure() {
        let body = |filler: String| {
            format!(
                "public class X : Endpoint<R>\n{{\n  public override void Configure()\n  {{\n    {filler} Get(\"/x\");\n  }}\n}}\n"
            )
        };
        let emoji = |count: usize| "\u{1F600}".repeat(count);
        // `void Configure() {\n    ` is 25 units before the filler, whose
        // emoji are two code units each, blanked in a comment and kept in a
        // string literal: `Get` starts 2990 or 2997 units on with 1480 of
        // them, 3010 or 3017 with 1490.
        for (near, far) in [
            (
                format!("/*{}*/", emoji(1480)),
                format!("/*{}*/", emoji(1490)),
            ),
            (
                format!("var s = \"{}\";", emoji(1480)),
                format!("var s = \"{}\";", emoji(1490)),
            ),
        ] {
            assert_eq!(names(&aspnet("X.cs", &body(near))), ["GET /x"]);
            assert!(aspnet("X.cs", &body(far)).nodes.is_empty());
        }
    }

    #[test]
    fn an_endpoint_without_configure_has_no_routes() {
        let src = "public class X : Endpoint<R>\n{\n  public void Setup() { Get(\"/x\"); }\n}\n";
        assert!(aspnet("X.cs", src).nodes.is_empty());
    }

    #[test]
    fn each_endpoint_references_the_first_handler_from_its_own_class_on() {
        let src = "public class A : Endpoint<R>
{
  public override void Configure()
  {
    Get(\"/a\");
  }
  void Helper() { Post(\"/after\"); }
  public override async Task ExecuteAsync(R r, CancellationToken ct) { }
}
public class B : Endpoint<R>
{
  public override void Configure()
  {
    Get(\"/b\");
  }
  public override async Task HandleAsync(R r, CancellationToken ct) { }
}
";
        assert_eq!(
            routes_and_handlers(&aspnet("Both.cs", src)),
            [
                ("GET /a".to_string(), Some("ExecuteAsync".to_string())),
                ("POST /after".to_string(), Some("ExecuteAsync".to_string())),
                ("GET /b".to_string(), Some("HandleAsync".to_string())),
            ]
        );
    }

    #[test]
    fn a_verb_call_before_configure_is_no_route() {
        let src = "public class X : Endpoint<R>\n{\n  void Helper() { Post(\"/early\"); }\n  public override void Configure()\n  {\n    Get(\"/x\");\n  }\n}\n";
        assert_eq!(names(&aspnet("X.cs", src)), ["GET /x"]);
    }

    #[test]
    fn every_fastendpoints_base_makes_an_endpoint() {
        let endpoint = |base: &str| {
            format!(
                "public class X : {base}\n{{\n  public override void Configure()\n  {{\n    Get(\"/x\");\n  }}\n  public override async Task HandleAsync(CancellationToken ct) {{ }}\n}}\n"
            )
        };
        for base in [
            "Endpoint<R, S>",
            "EndpointWithoutRequest<S>",
            "EndpointWithoutResponse<R>",
            "FastEndpoints.Endpoint<R>",
            "Ep.Req<R>.Res<S>",
        ] {
            assert_eq!(
                names(&aspnet("X.cs", &endpoint(base))),
                ["GET /x"],
                "{base}"
            );
        }
        for base in ["Controller", "EndpointGroupBase", "MyEndpoint<R>"] {
            assert!(aspnet("X.cs", &endpoint(base)).nodes.is_empty(), "{base}");
        }
    }

    #[test]
    fn only_a_cs_file_holding_a_route_needle_is_read() {
        assert!(
            aspnet("Pages/Index.cshtml", "[HttpGet(\"/x\")]")
                .nodes
                .is_empty()
        );
        assert!(
            aspnet("Models/User.cs", "public class User { }")
                .nodes
                .is_empty()
        );
    }

    #[test]
    fn join_cs_path_drops_outer_slashes_and_empty_parts() {
        assert_eq!(join_cs_path("", ""), "/");
        assert_eq!(join_cs_path("/api/", "/x/"), "/api/x");
        assert_eq!(join_cs_path("api", ""), "/api");
        assert_eq!(join_cs_path("", "///"), "/");
        assert_eq!(join_cs_path("a//", "//b"), "/a/b");
        assert_eq!(join_cs_path("a/b", "c//d"), "/a/b/c//d");
    }

    // -- Detection ------------------------------------------------------------

    #[test]
    fn an_indexed_csproj_naming_aspnet_is_detected() {
        for marker in [
            "Microsoft.AspNetCore",
            "Microsoft.NET.Sdk.Web",
            "System.Web.Mvc",
        ] {
            let ctx =
                RouteCtx::default().file("App/App.csproj", &format!("<Project>{marker}</Project>"));
            assert!(AspnetResolver.detect(&ctx), "{marker}");
        }
        let plain =
            RouteCtx::default().file("App/App.csproj", "<Project Sdk=\"Microsoft.NET.Sdk\" />");
        assert!(!AspnetResolver.detect(&plain));
        let unindexed = RouteCtx::default().unlisted("App/App.csproj", "Microsoft.NET.Sdk.Web");
        assert!(!AspnetResolver.detect(&unindexed));
    }

    #[test]
    fn a_root_program_building_a_web_host_is_detected_indexed_or_not() {
        for marker in ["WebApplication", "CreateHostBuilder", "UseStartup"] {
            let ctx = RouteCtx::default().unlisted("Program.cs", &format!("var x = {marker}();"));
            assert!(AspnetResolver.detect(&ctx), "{marker}");
        }
        let console = RouteCtx::default().unlisted("Program.cs", "Console.WriteLine(1);");
        assert!(!AspnetResolver.detect(&console));
        let nested =
            RouteCtx::default().unlisted("src/Program.cs", "WebApplication.CreateBuilder();");
        assert!(!AspnetResolver.detect(&nested));
    }

    #[test]
    fn a_root_startup_is_detected() {
        assert!(AspnetResolver.detect(&RouteCtx::default().unlisted("Startup.cs", "")));
        assert!(!AspnetResolver.detect(&RouteCtx::default().unlisted("src/Startup.cs", "")));
    }

    #[test]
    fn an_aspnet_source_file_is_detected_by_its_name_and_source() {
        let detects =
            |file: &str, src: &str| AspnetResolver.detect(&RouteCtx::default().file(file, src));
        for file in [
            "src/Api/UsersController.cs",
            "src/Api/Program.cs",
            "src/Api/Startup.cs",
            "src/Api/TodoEndpoint.cs",
            "src/Api/TodoEndpoints.cs",
            "src/Api/WebApplicationExtensions.cs",
            "src/Web/Endpoints/TodoItems.cs",
            "Endpoints/TodoItems.cs",
        ] {
            assert!(detects(file, "[ApiController]"), "{file}");
        }
        for file in [
            "src/Api/Users.cs",
            "src/Web/MyEndpoints/TodoItems.cs",
            "src/Web/Endpoints/Sub/TodoItems.cs",
            "src/Api/UsersController.cshtml",
        ] {
            assert!(!detects(file, "[ApiController]"), "{file}");
        }
        // Only a file of those names is read, whatever another one uses.
        assert!(!detects(
            "src/Api/Users.cs",
            "using Microsoft.AspNetCore.Mvc; [ApiController]"
        ));
        for src in [
            "[ApiController]",
            "[Route(\"x\")]",
            "[HttpGet]",
            "[HttpPost]",
            "[HttpPut]",
            "[HttpPatch]",
            "[HttpDelete]",
            // JavaScript's `\b` is ASCII: `é` is no word character.
            "[Route\u{e9}]",
        ] {
            assert!(detects("src/HomeController.cs", src), "{src}");
        }
        for marker in [
            "ControllerBase",
            ": Controller",
            "MapControllers",
            "WebApplication",
            "Microsoft.AspNetCore",
            "IEndpointGroup",
            "EndpointGroupBase",
            "RouteGroupBuilder",
        ] {
            assert!(detects("src/HomeController.cs", marker), "{marker}");
        }
        for src in [
            "public class HomeController { }",
            "[RouteData]",
            "[HttpGetter]",
        ] {
            assert!(!detects("src/HomeController.cs", src), "{src}");
        }
    }

    #[test]
    fn detection_reads_the_source_files_a_batch_at_a_time() {
        let mut ctx = RouteCtx::default();
        for at in 0..300 {
            ctx = ctx.file(&format!("src/C{at:03}Controller.cs"), "public class C { }");
        }
        let late = ctx.file("src/Z999Controller.cs", "[ApiController]");
        assert!(AspnetResolver.detect(&late));
        let mut early = RouteCtx::default().file("src/A000Controller.cs", "[ApiController]");
        for at in 0..300 {
            early = early.file(&format!("src/C{at:03}Controller.cs"), "public class C { }");
        }
        assert!(AspnetResolver.detect(&early));
        assert!(
            !early
                .reads()
                .iter()
                .any(|read| read == "src/C299Controller.cs"),
            "the second batch is never read"
        );
    }

    // -- Resolution ---------------------------------------------------------

    #[test]
    fn each_convention_prefers_its_folders_at_its_confidence() {
        let ctx = ctx_with(vec![
            csharp(
                NodeKind::Class,
                "UsersController",
                "src/Web/Controllers/UsersController.cs",
                3,
            ),
            csharp(
                NodeKind::Class,
                "UsersController",
                "src/Admin/UsersController.cs",
                3,
            ),
            csharp(
                NodeKind::Class,
                "OrderService",
                "src/App/Services/OrderService.cs",
                3,
            ),
            csharp(
                NodeKind::Class,
                "OrderService",
                "src/Legacy/OrderService.cs",
                3,
            ),
            csharp(NodeKind::Interface, "IClock", "src/Core/IClock.cs", 3),
            csharp(
                NodeKind::Class,
                "OrderRepository",
                "src/Infrastructure/Data/OrderRepository.cs",
                3,
            ),
            csharp(
                NodeKind::Class,
                "OrderRepository",
                "src/Legacy/OrderRepository.cs",
                3,
            ),
            csharp(NodeKind::Class, "Order", "src/Domain/Entities/Order.cs", 3),
            csharp(NodeKind::Class, "Order", "src/Legacy/Order.cs", 3),
            csharp(
                NodeKind::Class,
                "OrderV2Dto",
                "src/App/DTOs/OrderV2Dto.cs",
                3,
            ),
            csharp(NodeKind::Class, "OrderV2Dto", "src/Legacy/OrderV2Dto.cs", 3),
            csharp(NodeKind::Class, "OrderDto", "src/App/DTOs/OrderDto.cs", 3),
            csharp(
                NodeKind::Class,
                "OrderV2ViewModel",
                "src/Web/ViewModels/OrderV2ViewModel.cs",
                3,
            ),
            csharp(
                NodeKind::Class,
                "OrderV2ViewModel",
                "src/Legacy/OrderV2ViewModel.cs",
                3,
            ),
        ]);
        let from = "src/Web/Program.cs";
        let id =
            |kind: NodeKind, name: &str, file: &str| format!("{}:{file}:{name}:3", kind.as_str());
        assert_eq!(
            resolve(&ctx, from, "UsersController"),
            Some((
                id(
                    NodeKind::Class,
                    "UsersController",
                    "src/Web/Controllers/UsersController.cs"
                ),
                0.85
            ))
        );
        assert_eq!(
            resolve(&ctx, from, "OrderService"),
            Some((
                id(
                    NodeKind::Class,
                    "OrderService",
                    "src/App/Services/OrderService.cs"
                ),
                0.85
            ))
        );
        assert_eq!(
            resolve(&ctx, from, "IClock"),
            Some((
                id(NodeKind::Interface, "IClock", "src/Core/IClock.cs"),
                0.85
            ))
        );
        assert_eq!(
            resolve(&ctx, from, "OrderRepository"),
            Some((
                id(
                    NodeKind::Class,
                    "OrderRepository",
                    "src/Infrastructure/Data/OrderRepository.cs"
                ),
                0.85
            ))
        );
        assert_eq!(
            resolve(&ctx, from, "Order"),
            Some((
                id(NodeKind::Class, "Order", "src/Domain/Entities/Order.cs"),
                0.7
            ))
        );
        // A digit keeps a DTO out of Pattern 4.
        assert_eq!(
            resolve(&ctx, from, "OrderV2Dto"),
            Some((
                id(NodeKind::Class, "OrderV2Dto", "src/App/DTOs/OrderV2Dto.cs"),
                0.8
            ))
        );
        assert_eq!(
            resolve(&ctx, from, "OrderV2ViewModel"),
            Some((
                id(
                    NodeKind::Class,
                    "OrderV2ViewModel",
                    "src/Web/ViewModels/OrderV2ViewModel.cs"
                ),
                0.8
            ))
        );
        // Pattern 4 comes first for a letters-only DTO.
        assert_eq!(
            resolve(&ctx, from, "OrderDto"),
            Some((
                id(NodeKind::Class, "OrderDto", "src/App/DTOs/OrderDto.cs"),
                0.7
            ))
        );
    }

    #[test]
    fn every_folder_a_convention_names_is_preferred() {
        for (name, kind, dirs) in [
            ("UsersController", NodeKind::Class, &["/Controllers/"][..]),
            (
                "OrderService",
                NodeKind::Class,
                &["/Services/", "/Service/", "/Application/"][..],
            ),
            (
                "IClock",
                NodeKind::Interface,
                &["/Services/", "/Service/", "/Application/"][..],
            ),
            (
                "OrderRepository",
                NodeKind::Interface,
                &[
                    "/Repositories/",
                    "/Repository/",
                    "/Data/",
                    "/Infrastructure/",
                ][..],
            ),
            (
                "Order",
                NodeKind::Class,
                &["/Models/", "/Model/", "/Entities/", "/Entity/", "/Domain/"][..],
            ),
            (
                "OrderV2Dto",
                NodeKind::Class,
                &["/ViewModels/", "/ViewModel/", "/DTOs/", "/Dto/"][..],
            ),
        ] {
            for dir in dirs {
                let preferred = format!("src{dir}{name}.cs");
                let ctx = ctx_with(vec![
                    csharp(kind, name, &preferred, 3),
                    csharp(kind, name, &format!("src/Other/{name}.cs"), 3),
                ]);
                assert_eq!(
                    resolve(&ctx, "src/Web/Program.cs", name).map(|(target, _)| target),
                    Some(format!("{}:{preferred}:{name}:3", kind.as_str())),
                    "{name} {dir}"
                );
            }
        }
    }

    #[test]
    fn a_lone_i_is_no_interface_name() {
        let ctx = ctx_with(vec![csharp(NodeKind::Class, "I", "src/Core/I.cs", 3)]);
        assert_eq!(resolve(&ctx, "src/Web/Program.cs", "I"), None);
    }

    #[test]
    fn a_root_folder_is_no_preferred_folder() {
        let ctx = ctx_with(vec![
            csharp(
                NodeKind::Class,
                "UsersController",
                "Controllers/UsersController.cs",
                3,
            ),
            csharp(
                NodeKind::Class,
                "UsersController",
                "src/Admin/UsersController.cs",
                3,
            ),
        ]);
        assert_eq!(resolve(&ctx, "src/Web/Program.cs", "UsersController"), None);
    }

    #[test]
    fn a_convention_with_no_candidate_hands_over_to_the_next() {
        // No class `IHomeController` for Pattern 1; Pattern 2 takes the
        // interface.
        // An interface in `/Controllers/` is no controller.
        let ctx = ctx_with(vec![
            csharp(
                NodeKind::Interface,
                "IHomeController",
                "src/Web/Services/IHomeController.cs",
                3,
            ),
            csharp(
                NodeKind::Interface,
                "IHomeController",
                "src/Web/Controllers/IHomeController.cs",
                3,
            ),
        ]);
        assert_eq!(
            resolve(&ctx, "src/Web/Program.cs", "IHomeController"),
            Some((
                "interface:src/Web/Services/IHomeController.cs:IHomeController:3".to_string(),
                0.85
            ))
        );
    }

    #[test]
    fn a_convention_that_declines_ends_the_chain() {
        // Pattern 2 sees a class and an interface in `/Services/` and
        // declines; Pattern 4, which sees only the class, is not asked.
        let ctx = ctx_with(vec![
            csharp(
                NodeKind::Class,
                "ProductService",
                "src/App/Services/ProductService.cs",
                3,
            ),
            csharp(
                NodeKind::Interface,
                "ProductService",
                "src/App/Services/Contracts.cs",
                3,
            ),
        ]);
        assert_eq!(resolve(&ctx, "src/Web/Program.cs", "ProductService"), None);
    }

    #[test]
    fn a_razor_page_model_resolves_to_the_page_model_beside_it() {
        assert_eq!(
            AspnetResolver.resolve_languages(),
            Some(&[Language::CSharp, Language::Razor][..])
        );
        assert_eq!(AspnetResolver.languages(), Some(&[Language::CSharp][..]));
        let ctx = ctx_with(vec![
            csharp(NodeKind::Class, "IndexModel", "Pages/Index.cshtml.cs", 5),
            csharp(
                NodeKind::Class,
                "IndexModel",
                "Pages/Basket/Index.cshtml.cs",
                5,
            ),
        ]);
        let resolved = AspnetResolver
            .resolve(
                &reference_in("Pages/Basket/Index.cshtml", "IndexModel", Language::Razor),
                &ctx,
            )
            .expect("the page model beside the page");
        assert_eq!(
            resolved.target_node_id,
            "class:Pages/Basket/Index.cshtml.cs:IndexModel:5"
        );
        // `IndexModel` starts with `I`: Pattern 2's, as upstream's.
        assert_eq!(resolved.confidence, 0.85);
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
    }

    // -- post_extract ---------------------------------------------------------

    /// The routes `src` extracts in `file`, its file listed with `src`.
    fn with_routes(ctx: RouteCtx, file: &str, src: &str) -> RouteCtx {
        let mut nodes = aspnet(file, src).nodes;
        for node in &mut nodes {
            node.updated_at = 0;
        }
        ctx.file(file, src).routes(file, nodes)
    }

    fn renamed(ctx: &RouteCtx) -> Vec<(String, String)> {
        AspnetResolver
            .post_extract(ctx)
            .unwrap()
            .into_iter()
            .map(|node| (node.qualified_name, node.name))
            .collect()
    }

    fn renamed_to(ctx: &RouteCtx) -> Vec<String> {
        AspnetResolver
            .post_extract(ctx)
            .unwrap()
            .into_iter()
            .map(|node| node.name)
            .collect()
    }

    /// `ctx` with the route named `from` named `to`, as an earlier pass left
    /// it.
    fn renaming(ctx: &RouteCtx, from: &str, to: &str) -> RouteCtx {
        let mut rebuilt = RouteCtx::default();
        for (path, content) in files_of(ctx) {
            rebuilt = rebuilt.file(&path, &content);
        }
        for node in ctx.nodes() {
            let mut node = node.clone();
            if node.kind == NodeKind::Route && node.name == from {
                node.name = to.to_string();
            }
            let file = node.file_path.clone();
            rebuilt = rebuilt.routes(&file, vec![node]);
        }
        rebuilt
    }

    fn files_of(ctx: &RouteCtx) -> Vec<(String, String)> {
        ctx.get_all_files()
            .into_iter()
            .map(|file| {
                let content = ctx.read_file(&file).unwrap_or_default();
                (file, content)
            })
            .collect()
    }

    const REQUEST: &str = "src/Web/Contributors/Create.CreateContributorRequest.cs";
    const REQUEST_SRC: &str = "public class CreateContributorRequest\n{\n  public const string Route = \"/Contributors\";\n}\n";

    #[test]
    fn a_constant_route_is_named_by_the_constants_value() {
        let ctx = with_routes(
            RouteCtx::default(),
            "src/Web/Contributors/Create.cs",
            CREATE,
        )
        .file(REQUEST, REQUEST_SRC)
        .routes(
            REQUEST,
            vec![csharp(
                NodeKind::Class,
                "CreateContributorRequest",
                REQUEST,
                1,
            )],
        );
        assert_eq!(renamed_to(&ctx), ["POST /Contributors"]);
    }

    #[test]
    fn a_constant_of_a_struct_static_readonly_or_static_field_counts() {
        for (kind, declaration) in [
            (NodeKind::Struct, "public const string Route = \"/a\";"),
            (
                NodeKind::Class,
                "public static readonly string Route = \"a\";",
            ),
            (NodeKind::Class, "internal static string Route = \"/a/\";"),
        ] {
            let ctx = with_routes(
                RouteCtx::default(),
                "src/Web/Contributors/Create.cs",
                CREATE,
            )
            .file(
                REQUEST,
                &format!("x CreateContributorRequest {{ {declaration} }}"),
            )
            .routes(
                REQUEST,
                vec![csharp(kind, "CreateContributorRequest", REQUEST, 1)],
            );
            assert_eq!(renamed_to(&ctx), ["POST /a"], "{declaration}");
        }
        let mut java = csharp(NodeKind::Class, "CreateContributorRequest", REQUEST, 1);
        java.language = Language::Java;
        let other_language = with_routes(
            RouteCtx::default(),
            "src/Web/Contributors/Create.cs",
            CREATE,
        )
        .file(REQUEST, REQUEST_SRC)
        .routes(REQUEST, vec![java]);
        assert!(renamed_to(&other_language).is_empty());
        let interface = with_routes(
            RouteCtx::default(),
            "src/Web/Contributors/Create.cs",
            CREATE,
        )
        .file(REQUEST, REQUEST_SRC)
        .routes(
            REQUEST,
            vec![csharp(
                NodeKind::Interface,
                "CreateContributorRequest",
                REQUEST,
                1,
            )],
        );
        assert!(renamed_to(&interface).is_empty());
    }

    #[test]
    fn a_constant_route_with_no_constant_gets_back_its_files_name() {
        let base = with_routes(
            RouteCtx::default(),
            "src/Web/Contributors/Create.cs",
            CREATE,
        );
        assert!(renamed_to(&base).is_empty());
        let stale = renaming(
            &base,
            "POST CreateContributorRequest.Route",
            "POST /Contributors",
        );
        assert_eq!(renamed_to(&stale), ["POST CreateContributorRequest.Route"]);
    }

    #[test]
    fn a_constant_two_classes_give_different_values_renames_nothing() {
        let other = "src/Api/CreateContributorRequest.cs";
        let two = with_routes(
            RouteCtx::default(),
            "src/Web/Contributors/Create.cs",
            CREATE,
        )
        .file(REQUEST, REQUEST_SRC)
        .routes(
            REQUEST,
            vec![csharp(
                NodeKind::Class,
                "CreateContributorRequest",
                REQUEST,
                1,
            )],
        )
        .file(
            other,
            "class CreateContributorRequest { const string Route = \"/People\"; }",
        )
        .routes(
            other,
            vec![csharp(
                NodeKind::Class,
                "CreateContributorRequest",
                other,
                1,
            )],
        );
        assert!(renamed_to(&two).is_empty());
        let same = with_routes(
            RouteCtx::default(),
            "src/Web/Contributors/Create.cs",
            CREATE,
        )
        .file(REQUEST, REQUEST_SRC)
        .routes(
            REQUEST,
            vec![csharp(
                NodeKind::Class,
                "CreateContributorRequest",
                REQUEST,
                1,
            )],
        )
        .file(
            other,
            "class CreateContributorRequest { const string Route = \"/Contributors\"; }",
        )
        .routes(
            other,
            vec![csharp(
                NodeKind::Class,
                "CreateContributorRequest",
                other,
                1,
            )],
        );
        assert_eq!(renamed_to(&same), ["POST /Contributors"]);
    }

    const TODO_ITEMS: &str = "src/Web/Endpoints/TodoItems.cs";
    const TODO_ITEMS_SRC: &str = "public class TodoItems : IEndpointGroup
{
    public static void Map(RouteGroupBuilder groupBuilder)
    {
        groupBuilder.MapPost(CreateTodoItem);
        groupBuilder.MapPut(UpdateTodoItem, \"{id}\");
    }
}
";
    const EXTENSIONS: &str = "src/Web/Infrastructure/WebApplicationExtensions.cs";
    const EXTENSIONS_SRC: &str = "public static class WebApplicationExtensions
{
    public static WebApplication MapEndpoints(this WebApplication app)
    {
        var groupName = \"x\";
        var group = app.MapGroup($\"/api/{groupName}\").WithTags(groupName);
        return app;
    }
}
";

    #[test]
    fn a_group_route_is_named_under_the_apps_head() {
        let ctx = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC)
            .file(EXTENSIONS, EXTENSIONS_SRC);
        assert_eq!(
            renamed(&ctx),
            [
                (
                    format!("{TODO_ITEMS}::group:TodoItems:POST:"),
                    "POST /api/TodoItems".to_string()
                ),
                (
                    format!("{TODO_ITEMS}::group:TodoItems:PUT:{{id}}"),
                    "PUT /api/TodoItems/{id}".to_string()
                ),
            ]
        );
        let interpolated = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC)
            .file("src/Web/Setup.cs", "app.MapGroup($\"/v1/{group.Key}\");");
        assert_eq!(
            renamed_to(&interpolated),
            ["POST /v1/TodoItems", "PUT /v1/TodoItems/{id}"]
        );
        // A head written before the call, by any of its four hole names.
        for (hole, head) in [
            ("groupName", "/a/"),
            ("type.Name", "/b/"),
            ("name", "/c/"),
            ("prefix", "/d/"),
        ] {
            let ctx = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC).file(
                "src/Web/Setup.cs",
                &format!("var path = $\"{head}{{{hole}}}\";\napp.MapGroup(path);"),
            );
            assert_eq!(
                renamed_to(&ctx),
                [
                    format!("POST {head}TodoItems"),
                    format!("PUT {head}TodoItems/{{id}}")
                ],
                "{hole}"
            );
        }
    }

    #[test]
    fn a_group_route_whose_file_gives_a_route_prefix_keeps_its_name() {
        let src = TODO_ITEMS_SRC.replace(
            "{\n    public static void Map",
            "{\n    public static string RoutePrefix => \"/todo\";\n    public static void Map",
        );
        let ctx =
            with_routes(RouteCtx::default(), TODO_ITEMS, &src).file(EXTENSIONS, EXTENSIONS_SRC);
        assert!(renamed_to(&ctx).is_empty());
        assert_eq!(
            names(&aspnet(TODO_ITEMS, &src)),
            ["POST /todo", "PUT /todo/{id}"]
        );
    }

    #[test]
    fn a_group_route_with_no_head_gets_back_its_files_name() {
        let base = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC);
        assert!(renamed_to(&base).is_empty());
        let stale = renaming(&base, "POST /TodoItems", "POST /api/TodoItems");
        assert_eq!(renamed_to(&stale), ["POST /TodoItems"]);
        let root = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC).file(
            EXTENSIONS,
            &EXTENSIONS_SRC.replace("$\"/api/{groupName}\"", "$\"/{groupName}\""),
        );
        assert!(renamed_to(&root).is_empty());
    }

    #[test]
    fn the_first_file_writing_a_head_decides_and_only_400_files_are_read() {
        let first = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC)
            .file("src/A/Setup.cs", "app.MapGroup($\"/first/{name}\");")
            .file(EXTENSIONS, EXTENSIONS_SRC);
        assert_eq!(
            renamed_to(&first),
            ["POST /first/TodoItems", "PUT /first/TodoItems/{id}"]
        );
        let mut crowded = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC);
        for at in 0..400 {
            crowded = crowded.file(&format!("src/A/G{at:03}.cs"), "app.MapGroup(\"/plain\");");
        }
        let beyond = crowded.file(EXTENSIONS, EXTENSIONS_SRC);
        assert!(renamed_to(&beyond).is_empty());
        let mut edge = with_routes(RouteCtx::default(), TODO_ITEMS, TODO_ITEMS_SRC);
        for at in 0..399 {
            edge = edge.file(&format!("src/A/G{at:03}.cs"), "app.MapGroup(\"/plain\");");
        }
        let last = edge.file(EXTENSIONS, EXTENSIONS_SRC);
        assert_eq!(
            renamed_to(&last),
            ["POST /api/TodoItems", "PUT /api/TodoItems/{id}"]
        );
    }

    #[test]
    fn the_head_is_looked_for_only_when_group_routes_exist() {
        let ctx = with_routes(RouteCtx::default(), "src/Web/Contributors/List.cs", LIST)
            .file(EXTENSIONS, EXTENSIONS_SRC);
        assert!(renamed_to(&ctx).is_empty());
        assert!(
            !ctx.reads().iter().any(|read| read == EXTENSIONS),
            "{:?}",
            ctx.reads()
        );
    }
}
