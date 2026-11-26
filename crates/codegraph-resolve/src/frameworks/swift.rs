//! SwiftUI, UIKit and Vapor (upstream `resolution/frameworks/swift.ts` at
//! `e8ddc4e2`: #2106's route handlers on the type the route names, #2157's
//! trailing-closure, `webSocket` and `on(…)` routes, #2273's closure calls,
//! #2242's Swift-only conventions, #2225's name heuristic, #1547's linear
//! route pattern).
//!
//! A project is SwiftUI's when an indexed `.swift` file imports SwiftUI or
//! an indexed path ends in `.xcodeproj` or `.xcworkspace`; UIKit's when an
//! indexed `.swift` file imports UIKit or names `UIViewController` or
//! `UIView`; Vapor's when its root `Package.swift` ([`ROOT_DETECTION_FILES`],
//! the sync's `swift` control kind) names `vapor`, or when an indexed
//! `.swift` file imports Vapor.
//!
//! Each resolves only Swift references, guessing the type a name
//! conventionally names through the shared name heuristic
//! ([`super::name_heuristic`]), in upstream's order: SwiftUI a `…View`
//! struct, a `…ViewModel`, `…Store` or `…Manager` class, then a model;
//! UIKit a `…ViewController`, a `…View` or a `…Cell` class, then a
//! `…Delegate` or `…DataSource` protocol; Vapor a `…Controller`, a Fluent
//! model, then a `…Middleware`.
//!
//! Vapor also reads a `.swift` file's comment-stripped text for three kinds
//! of routes: `app.get("todos", use: index)` (under the receiver's group
//! prefix, `let todos = routes.grouped("todos")` or
//! `routes.group("todos") { todos in`), `routes.on(.POST, "x", use: h)`, and
//! a trailing closure's route (`app.get("hello") { req in … }`,
//! `app.webSocket("chat")`), whose closure's calls are the route's. A
//! handler is referenced by the type the route names: `use:
//! API.PackageController.get` as `API.PackageController@get`, `use:
//! self.index` and `use: index` as `@index`, `use: todos.index` through
//! `let todos = TodoController()` as `TodoController@index`; resolution
//! finds the method on that type ([`resolve_vapor_handler`]).
//!
//! Rust's own (KNOWN_DIFFS, KEEP-RUST): a rule whose tier declines (two
//! candidates, or one in another code family) ends the chain, as Spring's;
//! of one type's overloads of a handler, the one sole overload declared to
//! take a `Request` is picked, and none when no overload or several take
//! one (upstream takes the earliest).
//!
//! Offsets are byte offsets. Comments are blanked byte for byte
//! ([`crate::strip_comments::strip_java_comments_keeping_offsets`]: upstream
//! strips Swift as it strips Java), so an offset into the stripped text
//! indexes the source too; whatever upstream measures in UTF-16 code units
//! (a column, a captured string, a qualified name's owner) is measured on
//! the source at the same offsets ([`OffsetText`]).

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

/// The languages the resolvers read (upstream `languages`): Strategy 1 asks
/// them only for Swift references (#2242: an Objective-C `@interface
/// SDDiskCache : NSObject <SDDiskCache>` is no SwiftUI view or model).
const LANGUAGES: [Language; 1] = [Language::Swift];

/// The root files Vapor's detection reads by path, indexed or not: inputs
/// of the sync's `swift` control kind (`codegraph_watch::control_inputs`),
/// since a configuration may keep it out of the index.
pub const ROOT_DETECTION_FILES: [&str; 1] = ["Package.swift"];

/// What an indexed `.swift` file holds when the project is SwiftUI's.
const SWIFTUI_IMPORT: [&str; 1] = ["import SwiftUI"];

/// What an indexed path ends in when it is an Xcode project or workspace.
const XCODE_SUFFIXES: [&str; 2] = [".xcodeproj", ".xcworkspace"];

/// What an indexed `.swift` file holds when the project is UIKit's.
const UIKIT_MARKERS: [&str; 3] = ["import UIKit", "UIViewController", "UIView"];

/// What the root `Package.swift` holds when it depends on Vapor (upstream's
/// case).
const VAPOR_DEPENDENCY: &str = "vapor";

/// What an indexed `.swift` file holds when the project is Vapor's.
const VAPOR_IMPORT: [&str; 1] = ["import Vapor"];

/// What a file's source holds when any of the three route readings finds a
/// route: each pattern needs one of them. The comment stripper only ever
/// blanks text, so the stripped text holds none of them when the source
/// does not.
const ROUTE_NEEDLES: [&str; 10] = [
    "use:",
    ".get",
    ".post",
    ".put",
    ".patch",
    ".delete",
    ".head",
    ".options",
    ".webSocket",
    ".on",
];

/// The files detection reads at once: a batch that decides ends the scan,
/// as upstream's one-by-one read ends at that file.
const READ_BATCH: usize = 256;

/// The folders a convention prefers, as path substrings (upstream's
/// `…_DIRS`). Each starts with `/`, so a root folder is no preferred one.
const VIEW_DIRS: [&str; 5] = ["/Views/", "/View/", "/Screens/", "/Components/", "/UI/"];
const VIEWMODEL_DIRS: [&str; 5] = [
    "/ViewModels/",
    "/ViewModel/",
    "/Stores/",
    "/Managers/",
    "/Services/",
];
const MODEL_DIRS: [&str; 4] = ["/Models/", "/Model/", "/Entities/", "/Domain/"];
const VC_DIRS: [&str; 4] = [
    "/ViewControllers/",
    "/ViewController/",
    "/Controllers/",
    "/Screens/",
];
const UIVIEW_DIRS: [&str; 4] = ["/Views/", "/View/", "/UI/", "/Components/"];
const CELL_DIRS: [&str; 5] = [
    "/Cells/",
    "/Cell/",
    "/Views/",
    "/TableViewCells/",
    "/CollectionViewCells/",
];
const VAPOR_CONTROLLER_DIRS: [&str; 3] = ["/Controllers/", "/Controller/", "/Routes/"];
const FLUENT_MODEL_DIRS: [&str; 4] = ["/Models/", "/Model/", "/Entities/", "/Database/"];
const VAPOR_MIDDLEWARE_DIRS: [&str; 2] = ["/Middleware/", "/Middlewares/"];
const NO_DIRS: [&str; 0] = [];

const VIEW_KINDS: [NodeKind; 1] = [NodeKind::Struct];
const CLASS_KINDS: [NodeKind; 1] = [NodeKind::Class];
const MODEL_KINDS: [NodeKind; 2] = [NodeKind::Struct, NodeKind::Class];
/// A Swift `protocol` is an `interface` node, so this picks an Objective-C
/// `@protocol` only, in upstream too.
const PROTOCOL_KINDS: [NodeKind; 1] = [NodeKind::Protocol];
const VAPOR_CONTROLLER_KINDS: [NodeKind; 2] = [NodeKind::Class, NodeKind::Struct];

/// The kinds a type owning a handler has (upstream `SWIFT_TYPE_KINDS`); an
/// `extension X` is a `class` node named `X`.
const SWIFT_TYPE_KINDS: [NodeKind; 4] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Enum,
    NodeKind::Protocol,
];

/// Words Swift writes before a `(` that are no calls (upstream
/// `SWIFT_NOT_CALLS`).
const SWIFT_NOT_CALLS: [&str; 14] = [
    "if", "guard", "switch", "while", "for", "return", "catch", "case", "in", "try", "await",
    "throw", "some", "any",
];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{W}` the body
/// of its `\w` (ASCII), and its `\b` is written `(?-u:\b)`.
fn swift_regex(pattern: &str) -> Regex {
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("swift pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| swift_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A model's name (SwiftUI's Pattern 3, Vapor's Pattern 2).
    model_name = r"^[A-Z][a-zA-Z]+$";
    /// A route's handler reference: `Type@method` (the type may be dotted),
    /// or `@method` (upstream `VAPOR_HANDLER`).
    vapor_handler = r"^((?:[A-Za-z_][{W}]*\.)*[A-Za-z_][{W}]*)?@([A-Za-z_][{W}]*)$";
    /// `let todos = routes.grouped("todos")`.
    grouped = r"(?-u:\b)let{S}+([{W}]+){S}*={S}*([{W}]+)\.grouped{S}*\(([^)]*)\)";
    /// `routes.group("todos") { todos in`.
    group_closure = r"(?-u:\b)([{W}]+)\.group{S}*\(([^)]*)\){S}*\{{S}*([{W}]+){S}+in";
    /// A string literal, its text the capture.
    string_literal = r#""([^"]*)""#;
    /// `let todos = TodoController()` or `var x: T = …(`: what a `use:
    /// todos.index` receiver is.
    receiver_type = r"(?-u:\b)(?:let|var){S}+([a-z_][{W}]*){S}*(?::{S}*([A-Z][{W}]*){S}*)?={S}*([A-Z][{W}.]*){S}*\(";
    /// `builder.get([path segments,] use: handler)`: every argument before
    /// `use:` ends at a comma, so the split is unique (#1547).
    use_route = r"(?-u:\b)([{W}]+)\.(get|post|put|patch|delete|head|options){S}*\({S}*((?:[^,()]+,)*{S}*)use:{S}*([A-Za-z_][{W}.]*)";
    /// `routes.on(.POST, "x", body: .collect(maxSize: "1mb"), use: h)`: an
    /// argument may hold one level of parentheses.
    on_route = r"(?-u:\b)([{W}]+)\.on{S}*\({S}*\.([A-Z]+){S}*,{S}*((?:(?:[^,()]|\([^()]*\))+,)*{S}*)use:{S}*([A-Za-z_][{W}.]*)";
    /// An `on(…)` argument that is a path segment: a string literal alone.
    path_argument = r#"^{S}*"[^"]*"{S}*$"#;
    /// A route whose handler is a trailing closure.
    closure_route = r"(?-u:\b)([{W}]+)\.(get|post|put|patch|delete|head|options|webSocket|on){S}*\(([^()]*)\){S}*\{";
    /// A `use:` label in a closure route's arguments.
    use_label = r"(?-u:\b)use:";
    /// An HTTP client's URL as the first argument.
    absolute_url = r#"^{S}*"https?:"#;
    /// What a line holds before a route registration: a statement's start.
    statement_start = r"^{S}*(?:(?:try|await){S}+)*$";
    /// A closure `on(…)`'s arguments: the method, then its path segments.
    on_closure_args = r"(?s)^{S}*\.([A-Z]+){S}*,?(.*)$";
    /// A call in a closure's body, with its receiver.
    closure_call = r"((?:[A-Za-z_][{W}]*{S}*[?!]?\.{S}*)*)([A-Za-z_][{W}]*){S}*\(";
    /// A signature head declaring a `Request` parameter.
    takes_request = r"\({S}*(?:[{W}]+{S}+)?[{W}]+{S}*:{S}*Request(?-u:\b)";
}

/// The method a Vapor route's handler reference names: `index` of
/// `TodoController@index` and of `@index`; `None` for any other name. The
/// sync's Vapor lane re-resolves a route's handler when a callable of this
/// name is declared or goes.
pub fn handler_method(reference_name: &str) -> Option<&str> {
    vapor_handler()
        .captures(reference_name)
        .and_then(|found| found.get(2))
        .map(|method| method.as_str())
}

pub struct SwiftUiResolver;

pub struct UikitResolver;

pub struct VaporResolver;

impl FrameworkResolver for SwiftUiResolver {
    fn name(&self) -> &str {
        "swiftui"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// An indexed `.swift` file importing SwiftUI, else an indexed path
    /// ending in `.xcodeproj` or `.xcworkspace` (upstream's order; Xcode's
    /// are directories, so the second holds only when a configuration
    /// indexes a file so named, in upstream too).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let files = context.get_all_files_shared();
        any_containing(context, &swift_files(&files), &SWIFTUI_IMPORT)
            || files
                .iter()
                .any(|file| XCODE_SUFFIXES.iter().any(|suffix| file.ends_with(suffix)))
    }

    /// Upstream's three conventions in order; a rule that finds no
    /// candidate hands over to the next, one that declines ends the chain.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        let rules: [Rule<'_>; 3] = [
            // Pattern 1: a view, a PascalCase `…View`.
            (
                name.ends_with("View") && starts_upper(name),
                &VIEW_KINDS,
                &VIEW_DIRS,
                0.85,
            ),
            // Pattern 2: a view model or an observable object.
            (
                name.ends_with("ViewModel") || name.ends_with("Store") || name.ends_with("Manager"),
                &CLASS_KINDS,
                &VIEWMODEL_DIRS,
                0.85,
            ),
            // Pattern 3: a model.
            (model_name().is_match(name), &MODEL_KINDS, &MODEL_DIRS, 0.7),
        ];
        first_convention(reference, context, &rules)
    }
}

impl FrameworkResolver for UikitResolver {
    fn name(&self) -> &str {
        "uikit"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// An indexed `.swift` file importing UIKit or naming `UIViewController`
    /// or `UIView` (a comment's and `UIViewRepresentable`'s too, as
    /// upstream's).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let files = context.get_all_files_shared();
        any_containing(context, &swift_files(&files), &UIKIT_MARKERS)
    }

    /// Upstream's four conventions in order; a rule that finds no
    /// candidate hands over to the next, one that declines ends the chain.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        let rules: [Rule<'_>; 4] = [
            // Pattern 1: a view controller.
            (
                name.ends_with("ViewController"),
                &CLASS_KINDS,
                &VC_DIRS,
                0.85,
            ),
            // Pattern 2: a `UIView` subclass (a name ending in `View` never
            // ends in `ViewController`, as upstream's second test asks).
            (
                name.ends_with("View") && !name.ends_with("ViewController"),
                &CLASS_KINDS,
                &UIVIEW_DIRS,
                0.8,
            ),
            // Pattern 3: a cell.
            (name.ends_with("Cell"), &CLASS_KINDS, &CELL_DIRS, 0.85),
            // Pattern 4: a delegate or a data source, in no folder.
            (
                name.ends_with("Delegate") || name.ends_with("DataSource"),
                &PROTOCOL_KINDS,
                &NO_DIRS,
                0.8,
            ),
        ];
        first_convention(reference, context, &rules)
    }
}

impl FrameworkResolver for VaporResolver {
    fn name(&self) -> &str {
        "vapor"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// The root `Package.swift` naming `vapor` (read by path, indexed or
    /// not), else an indexed `.swift` file importing Vapor.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if context
            .read_file_shared(ROOT_DETECTION_FILES[0])
            .is_some_and(|package| package.contains(VAPOR_DEPENDENCY))
        {
            return true;
        }
        let files = context.get_all_files_shared();
        any_containing(context, &swift_files(&files), &VAPOR_IMPORT)
    }

    /// A route's handler reference names no declared symbol, so the name
    /// pre-filter would drop it before [`Self::resolve`] runs.
    fn claims_reference(&self, name: &str) -> bool {
        vapor_handler().is_match(name)
    }

    /// A route's handler on the type the route names (Pattern 0, `0.9`, and
    /// nothing else when it finds none), then upstream's three conventions
    /// in order; a rule that finds no candidate hands over to the next, one
    /// that declines ends the chain.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();
        if let Some(handler) = vapor_handler().captures(name) {
            let method = handler.get(2).expect("a handler's method").as_str();
            return resolve_vapor_handler(
                handler.get(1).map(|path| path.as_str()),
                method,
                reference,
                context,
            )
            .map(|target| framework_ref(reference, target, 0.9));
        }
        let rules: [Rule<'_>; 3] = [
            // Pattern 1: a controller.
            (
                name.ends_with("Controller"),
                &VAPOR_CONTROLLER_KINDS,
                &VAPOR_CONTROLLER_DIRS,
                0.85,
            ),
            // Pattern 2: a Fluent model.
            (
                model_name().is_match(name),
                &CLASS_KINDS,
                &FLUENT_MODEL_DIRS,
                0.75,
            ),
            // Pattern 3: a middleware.
            (
                name.ends_with("Middleware"),
                &VAPOR_CONTROLLER_KINDS,
                &VAPOR_MIDDLEWARE_DIRS,
                0.8,
            ),
        ];
        first_convention(reference, context, &rules)
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if !file_path.ends_with(".swift")
            || !ROUTE_NEEDLES.iter().any(|needle| content.contains(needle))
        {
            return Some(FrameworkResolverExtractionResult::default());
        }
        let text = OffsetText::swift(content);
        let source = Source {
            text: &text,
            file_path,
        };
        let groups = source.group_prefixes();
        let receivers = source.receiver_types();
        let mut result = FrameworkResolverExtractionResult::default();
        source.use_routes(&groups, &receivers, &mut result);
        source.on_routes(&groups, &receivers, &mut result);
        source.closure_routes(&groups, &mut result);
        Some(result)
    }
}

/// One convention: whether it applies to the name, the node kinds and the
/// folders it picks by, and the confidence it answers with.
type Rule<'a> = (bool, &'a [NodeKind], &'a [&'a str], f64);

/// The first convention that picks (KEEP-RUST, as Spring's and ASP.NET's):
/// a rule that finds no candidate hands over to the next, one whose tier
/// declines ends the chain (upstream takes the first candidate whenever
/// there is one).
fn first_convention(
    reference: &RefView,
    context: &dyn ResolutionContext,
    rules: &[Rule<'_>],
) -> Option<ResolvedRef> {
    for &(applies, kinds, dirs, confidence) in rules {
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

fn framework_ref(reference: &RefView, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef::new(reference.clone(), target, confidence, ResolvedBy::Framework)
}

/// Whether `name` starts with an ASCII capital (upstream `/^[A-Z]/`).
fn starts_upper(name: &str) -> bool {
    name.starts_with(|ch: char| ch.is_ascii_uppercase())
}

/// The indexed `.swift` files, in the file list's order.
fn swift_files(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|file| file.ends_with(".swift"))
        .cloned()
        .collect()
}

/// Whether one of `files` holds one of `needles`, read [`READ_BATCH`] at a
/// time: a batch that holds one ends the read.
fn any_containing(context: &dyn ResolutionContext, files: &[String], needles: &[&str]) -> bool {
    files
        .chunks(READ_BATCH)
        .any(|batch| !context.files_containing(batch, needles).is_empty())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// The method a route's handler reference names, on the type it names
/// (upstream `resolveVaporHandler`; never by the method's name alone, every
/// controller has a `show`). `type_path` is `None` for `@method`: the type
/// whose body holds the route, its own method in the route's file, else one
/// declared in another extension of it; a route outside any type names a
/// function in scope.
fn resolve_vapor_handler(
    type_path: Option<&str>,
    method: &str,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let callables: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(method)
        .into_iter()
        .filter(|node| {
            matches!(node.kind, NodeKind::Method | NodeKind::Function)
                && node.language == Language::Swift
        })
        .collect();
    if callables.is_empty() {
        return None;
    }
    // Upstream sorts the candidates (`getNodesByName`'s order); nothing
    // below depends on it, as no step takes a first candidate.
    let handlers = Handlers { method, context };
    let type_path = match type_path {
        Some(path) => path.to_string(),
        None => {
            let owner = innermost_type(context, &reference.file_path, |node| {
                node.start_line <= reference.line && node.end_line >= reference.line
            });
            let Some(owner) = owner else {
                // A route in a top-level `func routes(_ app:)` names a
                // function in scope.
                let functions: Vec<&Arc<Node>> = callables
                    .iter()
                    .filter(|node| node.kind == NodeKind::Function)
                    .collect();
                let same_file: Vec<&Arc<Node>> = functions
                    .iter()
                    .copied()
                    .filter(|node| node.file_path == reference.file_path)
                    .collect();
                if !same_file.is_empty() {
                    return handlers.one_owners(&same_file);
                }
                return sole(functions).map(|node| node.id.clone());
            };
            let own: Vec<&Arc<Node>> = callables
                .iter()
                .filter(|node| {
                    node.file_path == reference.file_path
                        && node.start_line >= owner.start_line
                        && node.end_line <= owner.end_line
                })
                .collect();
            if !own.is_empty() {
                return handlers.one_owners(&own);
            }
            owner.qualified_name.replace("::", ".")
        }
    };
    let segments: Vec<&str> = type_path.split('.').collect();
    let last = segments.last().copied().unwrap_or_default();
    let full = segments.join("::");
    let exact: Vec<&Arc<Node>> = callables
        .iter()
        .filter(|node| handlers.owner_of(node) == full)
        .collect();
    if !exact.is_empty() {
        return handlers.one_owners(&exact);
    }
    let nested_suffix = format!("::{full}");
    let nested: Vec<&Arc<Node>> = callables
        .iter()
        .filter(|node| handlers.owner_of(node).ends_with(&nested_suffix))
        .collect();
    if !nested.is_empty() {
        return handlers.one_owners(&nested);
    }
    if segments.len() > 1 {
        // `extension API.PackageController { static func get }` names its
        // method by the last segment (`PackageController::get`), as a
        // top-level `PackageController`'s, so the extension's own line
        // decides.
        let mut pattern = String::from(r"(?-u:\b)extension{S}+");
        pattern.push_str(
            &segments
                .iter()
                .map(|segment| regex::escape(segment))
                .collect::<Vec<_>>()
                .join(r"{S}*\.{S}*"),
        );
        pattern.push_str(r"(?-u:\b)");
        let declared = swift_regex(&pattern);
        let in_extension: Vec<&Arc<Node>> = callables
            .iter()
            .filter(|node| {
                handlers.owner_of(node) == last && handlers.declared_in(node, last, &declared)
            })
            .collect();
        return handlers.one_owners(&in_extension);
    }
    // Upstream's last step, a type whose name ends in `::{last}`, asks
    // with one segment what the nested step asked, which found none.
    None
}

/// The innermost type in `file` that `encloses` accepts: of the type nodes,
/// sorted by start line then id, the last (upstream's `reduce` over
/// `getNodesInFile`, a later start winning and a tie going to the later).
fn innermost_type(
    context: &dyn ResolutionContext,
    file: &str,
    encloses: impl Fn(&Node) -> bool,
) -> Option<Arc<Node>> {
    let mut types: Vec<Arc<Node>> = context
        .get_nodes_in_file_shared(file)
        .into_iter()
        .filter(|node| SWIFT_TYPE_KINDS.contains(&node.kind) && encloses(node))
        .collect();
    types.sort_by(|a, b| (a.start_line, a.id.as_str()).cmp(&(b.start_line, b.id.as_str())));
    types.pop()
}

/// The handler candidates of one method name.
struct Handlers<'a> {
    method: &'a str,
    context: &'a dyn ResolutionContext,
}

impl Handlers<'_> {
    /// The type a candidate is declared on: its qualified name without the
    /// method and the `::` before it, in UTF-16 code units (upstream's
    /// `slice`; a top-level function's is `""`).
    fn owner_of(&self, node: &Node) -> String {
        let keep = utf16_len(&node.qualified_name).saturating_sub(utf16_len(self.method) + 2);
        utf16_prefix(&node.qualified_name, keep)
    }

    /// One type's method among `candidates` (upstream `oneOwnersMethod`):
    /// none when they are on different types; the sole candidate; else
    /// (KEEP-RUST) the sole overload whose head declares a `Request`, what
    /// Vapor calls a handler with, and none when no overload or several do
    /// (upstream takes the earliest of those, else the earliest of all).
    fn one_owners(&self, candidates: &[&Arc<Node>]) -> Option<String> {
        let first = candidates.first()?;
        let owner = self.owner_of(first);
        if candidates
            .iter()
            .any(|candidate| self.owner_of(candidate) != owner)
        {
            return None;
        }
        if candidates.len() == 1 {
            return Some(first.id.clone());
        }
        sole(
            candidates
                .iter()
                .filter(|candidate| self.takes_request(candidate)),
        )
        .map(|candidate| candidate.id.clone())
    }

    /// Whether `node`'s head (its source from its start line, at most three
    /// lines and none past its end line) declares a `Request` parameter.
    fn takes_request(&self, node: &Node) -> bool {
        let start = slice_bound(node.start_line - 1);
        let end = slice_bound(node.end_line.min(node.start_line + 2));
        self.context
            .read_file_shared(&node.file_path)
            .is_some_and(|content| takes_request().is_match(&joined_lines(&content, start, end)))
    }

    /// Whether the innermost type named `name` around `node` is declared
    /// as `declared` asks: its source's first three lines, read as written
    /// (a commented-out `// extension A.B` counts, in upstream too). The
    /// node starts at its attributes, the keyword a line or two on.
    fn declared_in(&self, node: &Node, name: &str, declared: &Regex) -> bool {
        let Some(owner) = innermost_type(self.context, &node.file_path, |owner| {
            owner.name == name
                && owner.start_line <= node.start_line
                && owner.end_line >= node.end_line
        }) else {
            return false;
        };
        let start = slice_bound(owner.start_line - 1);
        let end = slice_bound(owner.start_line + 2);
        self.context
            .read_file_shared(&node.file_path)
            .is_some_and(|content| declared.is_match(&joined_lines(&content, start, end)))
    }
}

/// A slice bound of upstream's line arithmetic, which a node's 1-based
/// lines never make negative.
fn slice_bound(bound: i64) -> usize {
    usize::try_from(bound).unwrap_or(0)
}

/// Lines `start..end` (0-based) of `content` joined by a space, as
/// upstream's `split(/\r?\n/).slice(start, end).join(' ')` holds them: a
/// `\r` before a `\n` goes, a last line's stays.
fn joined_lines(content: &str, start: usize, end: usize) -> String {
    let mut pieces = content.split('\n').peekable();
    let mut lines: Vec<&str> = Vec::new();
    let mut index = 0;
    while let Some(piece) = pieces.next() {
        if index >= end {
            break;
        }
        if index >= start {
            let line = if pieces.peek().is_some() {
                piece.strip_suffix('\r').unwrap_or(piece)
            } else {
                piece
            };
            lines.push(line);
        }
        index += 1;
    }
    lines.join(" ")
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `units` UTF-16 code units of `text`; a unit cut from its pair
/// is U+FFFD, which equals no identifier, as the lone surrogate upstream
/// holds.
fn utf16_prefix(text: &str, units: usize) -> String {
    let mut out = String::new();
    let mut taken = 0;
    for ch in text.chars() {
        let width = ch.len_utf16();
        if taken + width > units {
            if taken < units {
                out.push('\u{FFFD}');
            }
            break;
        }
        taken += width;
        out.push(ch);
    }
    out
}

/// The handler reference for `use: <expression>` (upstream
/// `vaporHandlerRef`), keeping the type the method is on:
/// `API.PackageController.get` is `API.PackageController@get`; `self.index`,
/// `Self.index` and a bare `index` are `@index` (the type the route is
/// written in); `todos.index` is `TodoController@index` when the file made
/// `todos` from `TodoController`, else `@index`.
fn vapor_handler_ref(expression: &str, receivers: &HashMap<String, String>) -> Option<String> {
    let mut segments: Vec<&str> = expression
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    let method = segments.pop()?;
    if matches!(segments.first(), Some(&("self" | "Self"))) {
        segments.remove(0);
    }
    if segments.is_empty() {
        return Some(format!("@{method}"));
    }
    if starts_upper(segments[0]) {
        return Some(format!("{}@{method}", segments.join(".")));
    }
    let receiver = if segments.len() == 1 {
        receivers.get(segments[0])
    } else {
        None
    };
    Some(match receiver {
        Some(receiver) => format!("{receiver}@{method}"),
        None => format!("@{method}"),
    })
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// The `.swift` file an extraction reads.
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

    /// Each group variable's path prefix: every `let x = y.grouped("a")`,
    /// then every `y.group("a") { x in`, each under its source variable's
    /// prefix, a later one replacing an earlier. The roots (`app`,
    /// `routes`) have none.
    fn group_prefixes(&self) -> HashMap<String, String> {
        let safe = &self.text.safe;
        let mut prefixes: HashMap<String, String> = HashMap::new();
        for found in grouped().captures_iter(safe) {
            let base = prefixes.get(&found[2]).cloned().unwrap_or_default();
            let prefix = seg_join(&base, &self.capture(found.get(3)));
            prefixes.insert(found[1].to_string(), prefix);
        }
        for found in group_closure().captures_iter(safe) {
            let base = prefixes.get(&found[1]).cloned().unwrap_or_default();
            let prefix = seg_join(&base, &self.capture(found.get(2)));
            prefixes.insert(found[3].to_string(), prefix);
        }
        prefixes
    }

    /// Each variable's type, by `let x = T(` or `var x: T = …(`: the written
    /// type, else the constructed one; a later one replacing an earlier.
    fn receiver_types(&self) -> HashMap<String, String> {
        let mut receivers: HashMap<String, String> = HashMap::new();
        for found in receiver_type().captures_iter(&self.text.safe) {
            let receiver = found
                .get(2)
                .or_else(|| found.get(3))
                .expect("a constructed type");
            receivers.insert(found[1].to_string(), receiver.as_str().to_string());
        }
        receivers
    }

    /// A route node for `whole`, on its line from column 0 to its UTF-16
    /// length; its id and line.
    fn route(
        &self,
        method: &str,
        route_path: &str,
        whole: regex::Match<'_>,
        result: &mut FrameworkResolverExtractionResult,
    ) -> (String, i64) {
        let line = self.text.line_at(whole.start());
        let id = format!("route:{}:{line}:{method}:{route_path}", self.file_path);
        result.nodes.push(framework_node(
            id.clone(),
            NodeKind::Route,
            format!("{method} {route_path}"),
            format!("{}::route:{route_path}", self.file_path),
            self.file_path.to_string(),
            line,
            line,
            0,
            self.text.units(whole.start(), whole.end()),
            Language::Swift,
            false,
        ));
        (id, line)
    }

    /// A route's reference `name`, on the route's line at column 0.
    fn reference(&self, from: &str, name: String, kind: EdgeKind, line: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: from.to_string(),
            reference_name: name,
            reference_kind: kind,
            line,
            column: 0,
            file_path: self.file_path.to_string(),
            language: Language::Swift,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    /// `builder.get([path segments,] use: handler)`: the path is the
    /// receiver's group prefix and the arguments' string literals, `/`
    /// when both are empty; a segment that is no literal (`User.parameter`,
    /// a constant) adds nothing.
    fn use_routes(
        &self,
        groups: &HashMap<String, String>,
        receivers: &HashMap<String, String>,
        result: &mut FrameworkResolverExtractionResult,
    ) {
        for found in use_route().captures_iter(&self.text.safe) {
            let whole = found.get(0).expect("whole match");
            let method = found[2].to_ascii_uppercase();
            let route_path = route_path(groups.get(&found[1]), &self.capture(found.get(3)));
            let (id, line) = self.route(&method, &route_path, whole, result);
            if let Some(handler) = vapor_handler_ref(&self.capture(found.get(4)), receivers) {
                result
                    .references
                    .push(self.reference(&id, handler, EdgeKind::References, line));
            }
        }
    }

    /// `builder.on(.POST, "x", use: handler)`: the method is the first
    /// argument; only an argument that is a string literal alone is a path
    /// segment.
    fn on_routes(
        &self,
        groups: &HashMap<String, String>,
        receivers: &HashMap<String, String>,
        result: &mut FrameworkResolverExtractionResult,
    ) {
        for found in on_route().captures_iter(&self.text.safe) {
            let whole = found.get(0).expect("whole match");
            let arguments = path_arguments(&self.capture(found.get(3)));
            let route_path = route_path(groups.get(&found[1]), &arguments);
            let (id, line) = self.route(&found[2], &route_path, whole, result);
            if let Some(handler) = vapor_handler_ref(&self.capture(found.get(4)), receivers) {
                result
                    .references
                    .push(self.reference(&id, handler, EdgeKind::References, line));
            }
        }
    }

    /// A route whose handler is a trailing closure: `app.get("hello") { req
    /// in … }`, `app.webSocket("chat") { … }` (method `WS`),
    /// `routes.on(.GET, "x") { … }`. The closure is the handler, so its
    /// calls are the route's. No route: a `use:` one (read above), an HTTP
    /// client's `req.client.get("https://…") { … }`, and a `{` that opens
    /// no statement's closure (`if let v = req.parameters.get("x") {`).
    fn closure_routes(
        &self,
        groups: &HashMap<String, String>,
        result: &mut FrameworkResolverExtractionResult,
    ) {
        let safe = &self.text.safe;
        for found in closure_route().captures_iter(safe) {
            let whole = found.get(0).expect("whole match");
            let receiver = &found[1];
            let verb = &found[2];
            let arguments = self.capture(found.get(3));
            if use_label().is_match(&arguments)
                || receiver == "client"
                || absolute_url().is_match(&arguments)
            {
                continue;
            }
            let line_start = safe[..whole.start()].rfind('\n').map_or(0, |at| at + 1);
            if !statement_start().is_match(&safe[line_start..whole.start()]) {
                continue;
            }
            let (method, segments) = match verb {
                "on" => {
                    let Some(on) = on_closure_args().captures(&arguments) else {
                        continue;
                    };
                    (on[1].to_string(), on[2].to_string())
                }
                "webSocket" => ("WS".to_string(), arguments),
                _ => (verb.to_ascii_uppercase(), arguments),
            };
            let route_path = route_path(groups.get(receiver), &segments);
            let (id, line) = self.route(&method, &route_path, whole, result);
            // The match ends at the closure's `{`.
            let open = whole.end() - 1;
            if let Some(close) = closing_brace(safe, open) {
                for name in closure_call_names(&self.text.upstream(open + 1, close)) {
                    result
                        .references
                        .push(self.reference(&id, name, EdgeKind::Calls, line));
                }
            }
        }
    }
}

/// `existing` and a `/{segment}` for each string literal in `segments`
/// (upstream `segJoin`).
fn seg_join(existing: &str, segments: &str) -> String {
    let mut out = existing.to_string();
    for found in string_literal().captures_iter(segments) {
        out.push('/');
        out.push_str(&found[1]);
    }
    out
}

/// A route's path: the receiver's group prefix and the segments' string
/// literals, `/` when both are empty.
fn route_path(prefix: Option<&String>, segments: &str) -> String {
    let path = format!(
        "{}{}",
        prefix.map_or("", String::as_str),
        seg_join("", segments)
    );
    if path.is_empty() {
        "/".to_string()
    } else {
        path
    }
}

/// The `on(…)` arguments that are path segments: each that is a string
/// literal alone (upstream `pathArgs`).
fn path_arguments(arguments: &str) -> String {
    arguments
        .split(',')
        .filter(|argument| path_argument().is_match(argument))
        .collect::<Vec<_>>()
        .join(",")
}

/// The `}` matching the `{` at `open`, string literals skipped (a `\`
/// escaping the next character); `None` when unbalanced (upstream
/// `closingBrace`). Every byte it looks for is ASCII, which no byte of a
/// multi-byte character equals, so reading bytes decides what reading code
/// units decides.
fn closing_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth: i64 = 0;
    let mut at = open;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// The calls a route closure's body makes, each once, in order, keeping the
/// receiver (`Todo.query`, `req.auth.require`) so they resolve as calls on
/// it (upstream `closureCallNames`). A keyword before a `(` is no call, and
/// a member of an expression (`a.b().c(`) names nothing to follow.
fn closure_call_names(body: &str) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut names = Vec::new();
    for found in closure_call().captures_iter(body) {
        let whole = found.get(0).expect("whole match");
        let callee = &found[2];
        let receiver: String = found[1]
            .chars()
            .filter(|&ch| !is_js_space(ch) && ch != '?' && ch != '!')
            .collect();
        let receiver = receiver.strip_suffix('.').unwrap_or(&receiver);
        if receiver.is_empty()
            && (SWIFT_NOT_CALLS.contains(&callee)
                || body[..whole.start()]
                    .trim_end_matches(is_js_space)
                    .ends_with(['.', ')', ']']))
        {
            continue;
        }
        let name = if receiver.is_empty() {
            callee.to_string()
        } else {
            format!("{receiver}.{callee}")
        };
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn vapor(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        VaporResolver
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
            .filter(|reference| reference.reference_kind == EdgeKind::References)
            .map(|reference| reference.reference_name.clone())
            .collect()
    }

    /// The calls of the route named `route`, in order.
    fn calls_of(result: &FrameworkResolverExtractionResult, route: &str) -> Vec<String> {
        let id = &result
            .nodes
            .iter()
            .find(|node| node.name == route)
            .unwrap_or_else(|| panic!("no route {route}: {:?}", names(result)))
            .id;
        result
            .references
            .iter()
            .filter(|reference| {
                &reference.from_node_id == id && reference.reference_kind == EdgeKind::Calls
            })
            .map(|reference| reference.reference_name.clone())
            .collect()
    }

    fn sorted(mut items: Vec<String>) -> Vec<String> {
        items.sort();
        items
    }

    fn node(kind: NodeKind, name: &str, qualified: &str, file: &str, lines: (i64, i64)) -> Node {
        let mut node = framework_node(
            format!("{}:{file}:{name}:{}", kind.as_str(), lines.0),
            kind,
            name.to_string(),
            qualified.to_string(),
            file.to_string(),
            lines.0,
            lines.1,
            0,
            0,
            Language::Swift,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn method(qualified: &str, file: &str, lines: (i64, i64)) -> Node {
        let name = qualified.rsplit("::").next().unwrap();
        node(NodeKind::Method, name, qualified, file, lines)
    }

    fn function(name: &str, file: &str, lines: (i64, i64)) -> Node {
        node(NodeKind::Function, name, name, file, lines)
    }

    fn ctx_of(files: &[(&str, &str)], nodes: Vec<Node>) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for (path, content) in files {
            ctx = ctx.file(path, content);
        }
        for node in nodes {
            let file = node.file_path.clone();
            ctx = ctx.routes(&file, vec![node]);
        }
        ctx
    }

    fn reference(file: &str, name: &str, line: i64) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("file:{file}"),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line,
            column: 4,
            file_path: file.to_string(),
            language: Language::Swift,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn resolve(
        resolver: &dyn FrameworkResolver,
        ctx: &RouteCtx,
        file: &str,
        name: &str,
        line: i64,
    ) -> Option<(String, f64)> {
        resolver
            .resolve(&reference(file, name, line), ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    /// The node a route's handler reference resolves to, at Pattern 0's
    /// `0.9`.
    fn handler(ctx: &RouteCtx, file: &str, name: &str, line: i64) -> Option<String> {
        resolve(&VaporResolver, ctx, file, name, line).map(|(target, confidence)| {
            assert_eq!(confidence, 0.9, "{name}");
            target
        })
    }

    fn id_of(kind: NodeKind, name: &str, file: &str, line: i64) -> String {
        format!("{}:{file}:{name}:{line}", kind.as_str())
    }

    // -- Extraction: upstream frameworks.test.ts 1830-1915, 2189-2199 ------

    #[test]
    fn extracts_route_from_app_get_with_use() {
        let result = vapor("routes.swift", "app.get(\"users\", use: listUsers)\n");
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(targets(&result), ["@listUsers"]);
        let route = &result.nodes[0];
        assert_eq!(route.id, "route:routes.swift:1:GET:/users");
        assert_eq!(route.kind, NodeKind::Route);
        assert_eq!(route.qualified_name, "routes.swift::route:/users");
        assert_eq!((route.start_line, route.end_line), (1, 1));
        assert_eq!(
            (route.start_column, route.end_column),
            (0, "app.get(\"users\", use: listUsers".len() as i64)
        );
        assert_eq!(route.language, Language::Swift);
        let handler = &result.references[0];
        assert_eq!(handler.from_node_id, route.id);
        assert_eq!(handler.reference_kind, EdgeKind::References);
        assert_eq!((handler.line, handler.column), (1, 0));
        assert_eq!(handler.file_path, "routes.swift");
        assert_eq!(handler.language, Language::Swift);
    }

    #[test]
    fn extracts_grouped_routes_with_the_group_prefix_and_no_path_argument() {
        let src = "
func boot(routes: RoutesBuilder) throws {
    let todos = routes.grouped(\"todos\")
    todos.get(use: index)
    todos.post(use: create)
    todos.group(\":todoID\") { todo in
        todo.delete(use: delete)
    }
}
";
        let result = vapor("TodoController.swift", src);
        assert_eq!(
            sorted(names(&result)),
            ["DELETE /todos/:todoID", "GET /todos", "POST /todos"]
        );
        assert_eq!(sorted(targets(&result)), ["@create", "@delete", "@index"]);
    }

    #[test]
    fn reads_a_self_handler_and_skips_a_segment_that_is_no_string() {
        let result = vapor(
            "UserController.swift",
            "router.get(\"users\", User.parameter, \"edit\", use: self.editUserHandler)\n",
        );
        assert_eq!(names(&result), ["GET /users/edit"]);
        assert_eq!(targets(&result), ["@editUserHandler"]);
    }

    #[test]
    fn a_get_without_use_is_no_route() {
        let result = vapor(
            "configure.swift",
            "let host = Environment.get(\"DATABASE_HOST\") ?? \"localhost\"\n",
        );
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn a_long_argument_list_without_use_is_no_route() {
        let args: Vec<String> = (0..60).map(|i| format!("arg{i}: value{i}")).collect();
        let result = vapor("routes.swift", &format!("app.get({})\n", args.join(", ")));
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn reads_every_route_shape_after_the_linear_rewrite() {
        let src = "
admin.get(use: self.list)
app.get(\"users\", use: listUsers)
router.post(\"users\", User.parameter, \"edit\", use: UserController.edit)
app.patch(\":id\" ,  \"meta\" ,  use: update)
app.get(
  \"multi\",
  \"line\",
  use: multiLine
)
";
        let result = vapor("routes.swift", src);
        assert_eq!(
            names(&result),
            [
                "GET /",
                "GET /users",
                "POST /users/edit",
                "PATCH /:id/meta",
                "GET /multi/line"
            ]
        );
        assert_eq!(
            targets(&result),
            [
                "@list",
                "@listUsers",
                "UserController@edit",
                "@update",
                "@multiLine"
            ]
        );
        assert_eq!(result.nodes[4].start_line, 6);
    }

    #[test]
    fn skips_line_and_block_commented_routes() {
        let src = "
// app.get(\"fake\", use: fakeHandler)
/* app.post(\"also-fake\", use: anotherHandler) */
app.get(\"real\", use: listUsers)
";
        let result = vapor("routes.swift", src);
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(targets(&result), ["@listUsers"]);
        assert_eq!(result.nodes[0].start_line, 4);
    }

    // -- Extraction: the readings -------------------------------------------

    #[test]
    fn an_on_route_takes_its_method_and_only_its_literal_arguments() {
        let src = "
let todos = app.grouped(\"todos\")
todos.on(.POST, \"import\", body: .collect(maxSize: \"1mb\"), use: importTodos)
routes.on(.GET, use: list)
";
        let result = vapor("routes.swift", src);
        assert_eq!(names(&result), ["POST /todos/import", "GET /"]);
        assert_eq!(targets(&result), ["@importTodos", "@list"]);
        assert_eq!(
            result.nodes[0].id,
            "route:routes.swift:3:POST:/todos/import"
        );
    }

    #[test]
    fn a_closure_route_takes_its_verb_and_its_closures_calls() {
        let src = "
app.get(\"hello\", \":name\") { req async throws -> String in
    return greeting(req)
}
app.webSocket(\"chat\") { req, ws in
    ws.send(\"hi\")
}
app.on(.PATCH, \"items\", \":id\") { req in
    update(req)
}
app.delete(\"items\") { req in
    remove()
}
";
        let result = vapor("routes.swift", src);
        assert_eq!(
            names(&result),
            [
                "GET /hello/:name",
                "WS /chat",
                "PATCH /items/:id",
                "DELETE /items"
            ]
        );
        assert_eq!(calls_of(&result, "GET /hello/:name"), ["greeting"]);
        assert_eq!(calls_of(&result, "WS /chat"), ["ws.send"]);
        assert_eq!(calls_of(&result, "PATCH /items/:id"), ["update"]);
        assert_eq!(calls_of(&result, "DELETE /items"), ["remove"]);
        let route = &result.nodes[0];
        assert_eq!(route.id, "route:routes.swift:2:GET:/hello/:name");
        assert_eq!(
            route.end_column,
            "app.get(\"hello\", \":name\") {".len() as i64
        );
        let call = result
            .references
            .iter()
            .find(|reference| reference.reference_kind == EdgeKind::Calls)
            .unwrap();
        assert_eq!((call.line, call.column), (2, 0));
        assert_eq!(call.from_node_id, route.id);
    }

    #[test]
    fn a_closure_route_is_no_use_route_client_call_url_or_open_statement() {
        let src = "
func lookup(req: Request) async throws -> String {
    if let owner = req.parameters.get(\"owner\") {
        return owner
    }
    let res = try await req.client.get(\"https://example.com\") { out in
        try out.query.encode([\"q\": \"x\"])
    }
    client.get(\"/x\") { out in }
    req.get(\"https://example.com\") { out in }
    app.get(\"a\", use: handler) { }
    try await app.get(\"b\") { req in
        ok()
    }
    app.on(\"c\") { req in }
    return \"\"
}
";
        let result = vapor("routes.swift", src);
        assert_eq!(names(&result), ["GET /a", "GET /b"]);
        assert_eq!(calls_of(&result, "GET /b"), ["ok"]);
    }

    #[test]
    fn group_prefixes_chain_and_a_closure_group_takes_its_parents() {
        let src = "
let api = app.grouped(\"api\")
let v1 = api.grouped(\"v1\", \"x\")
v1.get(\"users\", use: list)
api.group(\"admin\") { admin in
    admin.get(use: dashboard)
}
app.get(use: root)
";
        let result = vapor("routes.swift", src);
        assert_eq!(
            names(&result),
            ["GET /api/v1/x/users", "GET /api/admin", "GET /"]
        );
    }

    #[test]
    fn a_handler_reference_keeps_the_type_the_route_names() {
        let src = "
let todos = TodoController()
var pages: PageController = Pages.make()
app.get(\"a\", use: API.PackageController.get)
app.get(\"b\", use: self.index)
app.get(\"c\", use: Self.index)
app.get(\"d\", use: index)
app.get(\"e\", use: todos.index)
app.get(\"f\", use: pages.show)
app.get(\"g\", use: other.index)
app.get(\"h\", use: todos.nested.index)
app.get(\"i\", use: self.todos.index)
";
        assert_eq!(
            targets(&vapor("routes.swift", src)),
            [
                "API.PackageController@get",
                "@index",
                "@index",
                "@index",
                "TodoController@index",
                "PageController@show",
                "@index",
                "@index",
                "TodoController@index"
            ]
        );
    }

    /// `\w` and `\b` are ASCII's, as upstream's: a handler name stops at its
    /// first other character, and a non-ASCII letter before `let` leaves a
    /// word boundary.
    #[test]
    fn the_patterns_read_ascii_words() {
        let result = vapor("routes.swift", "app.get(\"x\", use: café)\n");
        assert_eq!(targets(&result), ["@caf"]);
        let grouped = vapor(
            "routes.swift",
            "élet todos = app.grouped(\"todos\")\ntodos.get(use: index)\n",
        );
        assert_eq!(names(&grouped), ["GET /todos"]);
    }

    /// The end column counts UTF-16 code units, as upstream's: a match
    /// across a comment holding an astral character.
    #[test]
    fn the_end_column_counts_utf16_units_across_a_comment() {
        let route = "app.get(\"a\", /* 😀 */ use: h";
        let result = vapor("routes.swift", &format!("{route})\n"));
        assert_eq!(names(&result), ["GET /a"]);
        assert_eq!(
            result.nodes[0].end_column,
            route.encode_utf16().count() as i64
        );
        assert_ne!(route.encode_utf16().count(), route.chars().count());
        // One a string keeps counts its two units as well.
        let kept = "app.get(\"😀\", use: h";
        let result = vapor("routes.swift", &format!("{kept})\n"));
        assert_eq!(names(&result), ["GET /😀"]);
        assert_eq!(
            result.nodes[0].end_column,
            kept.encode_utf16().count() as i64
        );
    }

    #[test]
    fn a_closure_pairs_its_brace_past_strings_and_an_unbalanced_one_calls_nothing() {
        let src = "app.get(\"a\") { req in
    log(\"}\")
    log(\"\\\"}\")
    Greeter.greeting()
}
app.get(\"b\") { req in
    pending()
";
        let result = vapor("routes.swift", src);
        assert_eq!(names(&result), ["GET /a", "GET /b"]);
        assert_eq!(calls_of(&result, "GET /a"), ["log", "Greeter.greeting"]);
        assert!(calls_of(&result, "GET /b").is_empty());
    }

    #[test]
    fn closure_calls_skip_keywords_and_expression_members_each_once() {
        let src = "app.get(\"a\") { req in
    if (ready) { }
    guard (ok) else { }
    switch(x) { }
    let user = try User.find(req.parameters.get(\"id\"), on: req.db)
    let v = a.b().c()
    let w = (x).y()
    let z = items[0].first()
    let y = items[0]
    refresh()
    let p = greeting()
    let q = greeting()
    Todo.query(on: req.db)
    req.auth?.require(User.self)
    after()
    return render()
}
";
        // `after()` follows a `)` and `refresh()` a `]`, each a line before:
        // no call, in upstream too.
        assert_eq!(
            calls_of(&vapor("routes.swift", src), "GET /a"),
            [
                "User.find",
                "req.parameters.get",
                "a.b",
                "greeting",
                "Todo.query",
                "req.auth.require",
                "render"
            ]
        );
    }

    #[test]
    fn only_a_swift_file_naming_a_route_is_read() {
        assert!(
            vapor("routes.ts", "app.get(\"users\", use: listUsers)\n")
                .nodes
                .is_empty()
        );
        let quiet = vapor("Model.swift", "struct Todo { let title: String }\n");
        assert!(quiet.nodes.is_empty() && quiet.references.is_empty());
        assert_eq!(
            SwiftUiResolver.extract(
                "View.swift",
                "app.get(\"a\", use: b)",
                &FrameworkExtractionContext::without_config("/project"),
            ),
            None
        );
    }

    // -- Detection ------------------------------------------------------------

    #[test]
    fn swiftui_is_detected_by_its_import_in_a_swift_file_or_an_xcode_path() {
        let detect = |ctx: RouteCtx| SwiftUiResolver.detect(&ctx);
        assert!(detect(
            RouteCtx::default().file("App/Home.swift", "import SwiftUI\n")
        ));
        assert!(!detect(
            RouteCtx::default().file("notes.md", "import SwiftUI\n")
        ));
        assert!(!detect(
            RouteCtx::default().unlisted("App/Home.swift", "import SwiftUI\n")
        ));
        assert!(!detect(
            RouteCtx::default().file("App/Home.swift", "import UIKit\n")
        ));
        assert!(detect(RouteCtx::default().file("App.xcodeproj", "")));
        assert!(detect(RouteCtx::default().file("App.xcworkspace", "")));
        // A file inside an Xcode project is no Xcode path: upstream reads
        // the path's end.
        assert!(!detect(
            RouteCtx::default().file("App.xcodeproj/project.pbxproj", "")
        ));
        assert!(!detect(RouteCtx::default().unlisted("App.xcodeproj", "")));
    }

    #[test]
    fn uikit_is_detected_by_its_import_or_a_view_in_a_swift_file() {
        let detect =
            |file: &str, src: &str| UikitResolver.detect(&RouteCtx::default().file(file, src));
        assert!(detect("App/A.swift", "import UIKit\n"));
        assert!(detect("App/A.swift", "class A: UIViewController {}\n"));
        assert!(detect("App/A.swift", "// Hosts a UIView.\n"));
        assert!(detect("App/A.swift", "struct A: UIViewRepresentable {}\n"));
        assert!(!detect("App/A.swift", "import Foundation\n"));
        assert!(!detect("App/A.m", "#import <UIKit/UIKit.h> UIView\n"));
    }

    #[test]
    fn vapor_is_detected_by_its_root_package_or_its_import() {
        let detect = |ctx: RouteCtx| VaporResolver.detect(&ctx);
        let dependency = ".package(url: \"https://github.com/vapor/vapor.git\", from: \"4.0.0\")";
        assert!(detect(
            RouteCtx::default().unlisted("Package.swift", dependency)
        ));
        assert!(detect(
            RouteCtx::default().file("Package.swift", dependency)
        ));
        assert!(!detect(
            RouteCtx::default().file("Sources/App/Package.swift", dependency)
        ));
        assert!(!detect(
            RouteCtx::default().unlisted("Package.swift", "// A Vapor app\n")
        ));
        assert!(detect(
            RouteCtx::default().file("Sources/App/routes.swift", "import Vapor\n")
        ));
        assert!(!detect(
            RouteCtx::default().file("Sources/App/routes.swift", "import Fluent\n")
        ));
    }

    /// Detection reads the `.swift` files a batch at a time: a batch that
    /// decides ends the read, and a later batch still decides.
    #[test]
    fn detection_reads_the_swift_files_a_batch_at_a_time() {
        let files: Vec<String> = (0..300).map(|i| format!("Sources/F{i:03}.swift")).collect();
        let with = |hit: usize, src: &str| {
            let mut ctx = RouteCtx::default();
            for (at, file) in files.iter().enumerate() {
                ctx = ctx.file(file, if at == hit { src } else { "" });
            }
            ctx
        };
        let early = with(0, "import Vapor\n");
        assert!(VaporResolver.detect(&early));
        let reads = early.reads();
        assert_eq!(reads.len(), 1 + READ_BATCH, "{reads:?}");
        assert!(!reads.contains(&files[299]));
        for (resolver, src) in [
            (
                &SwiftUiResolver as &dyn FrameworkResolver,
                "import SwiftUI\n",
            ),
            (&UikitResolver, "import UIKit\n"),
            (&VaporResolver, "import Vapor\n"),
        ] {
            assert!(resolver.detect(&with(299, src)), "{}", resolver.name());
            assert!(!resolver.detect(&with(299, "")), "{}", resolver.name());
        }
    }

    // -- Resolution: the conventions -----------------------------------------

    #[test]
    fn each_convention_picks_its_kind_in_its_folders_with_its_confidence() {
        use NodeKind::{Class, Enum, Struct};
        let cases: [(&dyn FrameworkResolver, &str, NodeKind, NodeKind, &str, f64); 9] = [
            (
                &SwiftUiResolver,
                "ProfileView",
                Struct,
                Class,
                "App/Views",
                0.85,
            ),
            (
                &SwiftUiResolver,
                "SessionStore",
                Class,
                Struct,
                "App/Stores",
                0.85,
            ),
            (&SwiftUiResolver, "Profile", Struct, Enum, "App/Models", 0.7),
            (
                &UikitResolver,
                "ReportViewController",
                Class,
                Struct,
                "App/ViewControllers",
                0.85,
            ),
            (
                &UikitResolver,
                "HeaderView",
                Class,
                Struct,
                "App/Views",
                0.8,
            ),
            (
                &UikitResolver,
                "ProfileCell",
                Class,
                Struct,
                "App/Cells",
                0.85,
            ),
            (
                &VaporResolver,
                "TodoController",
                Struct,
                Enum,
                "Sources/App/Controllers",
                0.85,
            ),
            (
                &VaporResolver,
                "Todo",
                Class,
                Struct,
                "Sources/App/Models",
                0.75,
            ),
            (
                &VaporResolver,
                "AuthMiddleware",
                Struct,
                Enum,
                "Sources/App/Middleware",
                0.8,
            ),
        ];
        for (resolver, name, kind, other_kind, dir, confidence) in cases {
            let preferred = format!("{dir}/{name}.swift");
            let ctx = ctx_of(
                &[],
                vec![
                    node(kind, name, name, &preferred, (1, 3)),
                    node(
                        kind,
                        name,
                        name,
                        &format!("Other/Legacy/{name}.swift"),
                        (1, 3),
                    ),
                    node(
                        other_kind,
                        name,
                        name,
                        &format!("{dir}/{name}+Other.swift"),
                        (1, 3),
                    ),
                ],
            );
            assert_eq!(
                resolve(resolver, &ctx, "Main/Root.swift", name, 3),
                Some((id_of(kind, name, &preferred, 1), confidence)),
                "{} {name}",
                resolver.name()
            );
        }
    }

    /// Every folder a convention names is preferred, not only its first:
    /// of a candidate there and one elsewhere, the convention takes the one
    /// there.
    #[test]
    fn every_folder_a_convention_names_is_preferred() {
        use NodeKind::{Class, Struct};
        let conventions: [(&dyn FrameworkResolver, &str, NodeKind, &[&str]); 9] = [
            (
                &SwiftUiResolver,
                "ProfileView",
                Struct,
                &["/Views/", "/View/", "/Screens/", "/Components/", "/UI/"],
            ),
            (
                &SwiftUiResolver,
                "SessionStore",
                Class,
                &[
                    "/ViewModels/",
                    "/ViewModel/",
                    "/Stores/",
                    "/Managers/",
                    "/Services/",
                ],
            ),
            (
                &SwiftUiResolver,
                "Profile",
                Struct,
                &["/Models/", "/Model/", "/Entities/", "/Domain/"],
            ),
            (
                &UikitResolver,
                "ReportViewController",
                Class,
                &[
                    "/ViewControllers/",
                    "/ViewController/",
                    "/Controllers/",
                    "/Screens/",
                ],
            ),
            (
                &UikitResolver,
                "HeaderView",
                Class,
                &["/Views/", "/View/", "/UI/", "/Components/"],
            ),
            (
                &UikitResolver,
                "ProfileCell",
                Class,
                &[
                    "/Cells/",
                    "/Cell/",
                    "/Views/",
                    "/TableViewCells/",
                    "/CollectionViewCells/",
                ],
            ),
            (
                &VaporResolver,
                "TodoController",
                Struct,
                &["/Controllers/", "/Controller/", "/Routes/"],
            ),
            (
                &VaporResolver,
                "Todo",
                Class,
                &["/Models/", "/Model/", "/Entities/", "/Database/"],
            ),
            (
                &VaporResolver,
                "AuthMiddleware",
                Struct,
                &["/Middleware/", "/Middlewares/"],
            ),
        ];
        for (resolver, name, kind, dirs) in conventions {
            for dir in dirs {
                let preferred = format!("App{dir}{name}.swift");
                let ctx = ctx_of(
                    &[],
                    vec![
                        node(kind, name, name, &preferred, (1, 3)),
                        node(
                            kind,
                            name,
                            name,
                            &format!("Other/Legacy/{name}.swift"),
                            (1, 3),
                        ),
                    ],
                );
                assert_eq!(
                    resolve(resolver, &ctx, "Main/Root.swift", name, 3).map(|(target, _)| target),
                    Some(id_of(kind, name, &preferred, 1)),
                    "{} {name} {dir}",
                    resolver.name()
                );
            }
        }
    }

    /// The names each convention reads, as upstream's patterns read them: a
    /// view is capitalized, a view model may end in `ViewModel`, `Store` or
    /// `Manager`, and a model is a capital and at least one more letter,
    /// letters only.
    #[test]
    fn a_convention_reads_the_names_upstream_reads() {
        use NodeKind::{Class, Struct};
        let one = |kind: NodeKind, name: &str, dir: &str| {
            let file = format!("App/{dir}/{name}.swift");
            let ctx = ctx_of(
                &[],
                vec![
                    node(kind, name, name, &file, (1, 3)),
                    node(
                        kind,
                        name,
                        name,
                        &format!("Other/Legacy/{name}.swift"),
                        (1, 3),
                    ),
                ],
            );
            (ctx, id_of(kind, name, &file, 1))
        };
        for (name, dir) in [
            ("ProfileViewModel", "ViewModels"),
            ("SessionManager", "Managers"),
        ] {
            let (ctx, id) = one(Class, name, dir);
            assert_eq!(
                resolve(&SwiftUiResolver, &ctx, "Main/Root.swift", name, 3),
                Some((id, 0.85)),
                "{name}"
            );
        }
        // A view starts with a capital.
        let (ctx, _) = one(Struct, "rowView", "Views");
        assert_eq!(
            resolve(&SwiftUiResolver, &ctx, "Main/Root.swift", "rowView", 3),
            None
        );
        // A model is letters only, a capital first, two at least.
        for name in ["Profile2", "profile", "P"] {
            let (ctx, _) = one(Struct, name, "Models");
            assert_eq!(
                resolve(&SwiftUiResolver, &ctx, "Main/Root.swift", name, 3),
                None,
                "{name}"
            );
        }
    }

    /// A candidate in the reference's own folder comes before one in a
    /// preferred folder: the shared pick's directory tier (upstream's
    /// `sameDirectory` default).
    #[test]
    fn a_candidate_beside_the_reference_comes_before_a_preferred_folder() {
        let beside = "Feature/Home/ProfileView.swift";
        let ctx = ctx_of(
            &[],
            vec![
                node(
                    NodeKind::Struct,
                    "ProfileView",
                    "ProfileView",
                    beside,
                    (1, 3),
                ),
                node(
                    NodeKind::Struct,
                    "ProfileView",
                    "ProfileView",
                    "App/Views/ProfileView.swift",
                    (1, 3),
                ),
            ],
        );
        assert_eq!(
            resolve(
                &SwiftUiResolver,
                &ctx,
                "Feature/Home/Root.swift",
                "ProfileView",
                3
            ),
            Some((id_of(NodeKind::Struct, "ProfileView", beside, 1), 0.85))
        );
    }

    #[test]
    fn uikit_takes_a_delegate_or_data_source_protocol_from_any_folder() {
        for name in ["ProfileDelegate", "ListDataSource"] {
            let protocol = format!("App/Protocols/{name}.swift");
            let ctx = ctx_of(
                &[],
                vec![
                    node(NodeKind::Protocol, name, name, &protocol, (1, 3)),
                    node(
                        NodeKind::Interface,
                        name,
                        name,
                        "App/Other/Swift.swift",
                        (1, 3),
                    ),
                ],
            );
            assert_eq!(
                resolve(&UikitResolver, &ctx, "Main/Root.swift", name, 3),
                Some((id_of(NodeKind::Protocol, name, &protocol, 1), 0.8))
            );
        }
        let ctx = ctx_of(
            &[],
            vec![
                node(
                    NodeKind::Protocol,
                    "ProfileDelegate",
                    "ProfileDelegate",
                    "App/A/P.swift",
                    (1, 3),
                ),
                node(
                    NodeKind::Protocol,
                    "ProfileDelegate",
                    "ProfileDelegate",
                    "App/B/P.swift",
                    (1, 3),
                ),
            ],
        );
        assert_eq!(
            resolve(
                &UikitResolver,
                &ctx,
                "Main/Root.swift",
                "ProfileDelegate",
                3
            ),
            None
        );
    }

    /// A rule that finds no candidate hands over to the next; one whose tier
    /// declines ends the chain (KEEP-RUST).
    #[test]
    fn no_candidate_hands_over_and_a_decline_ends_the_chain() {
        let model = "App/Models/ProfileView.swift";
        let ctx = ctx_of(
            &[],
            vec![node(
                NodeKind::Class,
                "ProfileView",
                "ProfileView",
                model,
                (1, 3),
            )],
        );
        assert_eq!(
            resolve(&SwiftUiResolver, &ctx, "Main/Root.swift", "ProfileView", 3),
            Some((id_of(NodeKind::Class, "ProfileView", model, 1), 0.7))
        );
        let ctx = ctx_of(
            &[],
            vec![
                node(
                    NodeKind::Struct,
                    "ProfileView",
                    "ProfileView",
                    "App/Views/A/ProfileView.swift",
                    (1, 3),
                ),
                node(
                    NodeKind::Struct,
                    "ProfileView",
                    "ProfileView",
                    "App/Views/B/ProfileView.swift",
                    (1, 3),
                ),
                node(NodeKind::Class, "ProfileView", "ProfileView", model, (1, 3)),
            ],
        );
        assert_eq!(
            resolve(&SwiftUiResolver, &ctx, "Main/Root.swift", "ProfileView", 3),
            None
        );
        let ctx = ctx_of(
            &[],
            vec![
                node(
                    NodeKind::Class,
                    "TodoController",
                    "TodoController",
                    "Sources/App/Controllers/A/TodoController.swift",
                    (1, 3),
                ),
                node(
                    NodeKind::Class,
                    "TodoController",
                    "TodoController",
                    "Sources/App/Controllers/B/TodoController.swift",
                    (1, 3),
                ),
                node(
                    NodeKind::Class,
                    "TodoController",
                    "TodoController",
                    "Sources/App/Models/TodoController.swift",
                    (1, 3),
                ),
            ],
        );
        assert_eq!(
            resolve(&VaporResolver, &ctx, "Main/Root.swift", "TodoController", 3),
            None
        );
    }

    /// Strategy 1 asks the three only for Swift references (#2242).
    #[test]
    fn only_swift_references_reach_the_swift_conventions() {
        for resolver in [
            &SwiftUiResolver as &dyn FrameworkResolver,
            &UikitResolver,
            &VaporResolver,
        ] {
            assert_eq!(resolver.languages(), Some(&[Language::Swift][..]));
            assert_eq!(resolver.resolve_languages(), Some(&[Language::Swift][..]));
        }
        assert_eq!(
            [
                SwiftUiResolver.name(),
                UikitResolver.name(),
                VaporResolver.name()
            ],
            ["swiftui", "uikit", "vapor"]
        );
    }

    #[test]
    fn vapor_claims_a_handlers_name_only() {
        for name in [
            "TodoController@index",
            "@index",
            "API.PackageController@get",
        ] {
            assert!(VaporResolver.claims_reference(name), "{name}");
            assert!(!SwiftUiResolver.claims_reference(name), "{name}");
            assert!(!UikitResolver.claims_reference(name), "{name}");
        }
        for name in ["index", "a@b@c", "@", "Todo.@index", "@1x", "Greeter.audit"] {
            assert!(!VaporResolver.claims_reference(name), "{name}");
        }
        assert_eq!(handler_method("TodoController@index"), Some("index"));
        assert_eq!(handler_method("@index"), Some("index"));
        assert_eq!(handler_method("API.PackageController@get"), Some("get"));
        assert_eq!(handler_method("Greeter.audit"), None);
        assert_eq!(handler_method("index"), None);
    }

    // -- Resolution: a route's handler ------------------------------------------

    const ROUTES: &str = "Sources/App/routes.swift";

    #[test]
    fn a_route_outside_any_type_names_a_function_in_scope() {
        let same_file = function("hello", ROUTES, (5, 5));
        let elsewhere = function("hello", "Sources/Tools/Hello.swift", (1, 1));
        let ctx = ctx_of(
            &[],
            vec![
                function("routes", ROUTES, (2, 4)),
                same_file.clone(),
                elsewhere.clone(),
                method("Greeter::hello", "Sources/App/Greeter.swift", (2, 2)),
            ],
        );
        assert_eq!(handler(&ctx, ROUTES, "@hello", 3), Some(same_file.id));
        let ctx = ctx_of(
            &[],
            vec![function("routes", ROUTES, (2, 4)), elsewhere.clone()],
        );
        assert_eq!(
            handler(&ctx, ROUTES, "@hello", 3),
            Some(elsewhere.id.clone())
        );
        let ctx = ctx_of(
            &[],
            vec![
                elsewhere,
                function("hello", "Sources/Other/Hello.swift", (1, 1)),
            ],
        );
        assert_eq!(handler(&ctx, ROUTES, "@hello", 3), None);
        let ctx = ctx_of(
            &[],
            vec![method(
                "Greeter::hello",
                "Sources/App/Greeter.swift",
                (2, 2),
            )],
        );
        assert_eq!(handler(&ctx, ROUTES, "@hello", 3), None);
    }

    #[test]
    fn a_route_in_a_type_takes_its_own_method_then_another_extensions() {
        let file = "Sources/App/Controllers/TodoController.swift";
        let own = method("TodoController::index", file, (8, 8));
        let ctx = ctx_of(
            &[],
            vec![
                node(
                    NodeKind::Struct,
                    "TodoController",
                    "TodoController",
                    file,
                    (2, 12),
                ),
                own.clone(),
                method(
                    "OtherController::index",
                    "Sources/App/OtherController.swift",
                    (2, 2),
                ),
            ],
        );
        assert_eq!(handler(&ctx, file, "@index", 4), Some(own.id));
        let routes = "Sources/App/TagController+Routes.swift";
        let declared = method(
            "TagController::index",
            "Sources/App/TagController.swift",
            (2, 2),
        );
        let ctx = ctx_of(
            &[],
            vec![
                node(
                    NodeKind::Class,
                    "TagController",
                    "TagController",
                    routes,
                    (1, 5),
                ),
                node(
                    NodeKind::Class,
                    "TagController",
                    "TagController",
                    "Sources/App/TagController.swift",
                    (1, 4),
                ),
                declared.clone(),
                method(
                    "OtherController::index",
                    "Sources/App/OtherController.swift",
                    (2, 2),
                ),
            ],
        );
        assert_eq!(handler(&ctx, routes, "@index", 3), Some(declared.id));
    }

    #[test]
    fn a_typed_handler_takes_the_exact_type_then_a_nested_one() {
        let exact = method("TodoController::index", "Sources/App/A.swift", (2, 2));
        let ctx = ctx_of(
            &[],
            vec![
                exact.clone(),
                method(
                    "Admin::TodoController::index",
                    "Sources/App/B.swift",
                    (3, 3),
                ),
            ],
        );
        assert_eq!(
            handler(&ctx, ROUTES, "TodoController@index", 3),
            Some(exact.id)
        );
        let nested = method(
            "API::PackageController::get",
            "Sources/App/API.swift",
            (3, 3),
        );
        let ctx = ctx_of(
            &[],
            vec![
                nested.clone(),
                method("Other::get", "Sources/App/Other.swift", (2, 2)),
            ],
        );
        assert_eq!(
            handler(&ctx, ROUTES, "PackageController@get", 3),
            Some(nested.id)
        );
        let deeper = method(
            "V1::API::PackageController::get",
            "Sources/App/V1.swift",
            (4, 4),
        );
        let ctx = ctx_of(&[], vec![deeper.clone()]);
        assert_eq!(
            handler(&ctx, ROUTES, "API.PackageController@get", 3),
            Some(deeper.id)
        );
    }

    /// Candidates on two types give none, even when one of them takes a
    /// `Request`.
    #[test]
    fn handlers_on_two_types_resolve_to_none() {
        let ctx = ctx_of(
            &[
                (
                    "Sources/App/API.swift",
                    "enum API {\n  struct PackageController {\n    func get(req: Request) -> String { \"\" }\n  }\n}\n",
                ),
                (
                    "Sources/App/V2.swift",
                    "enum V2 {\n  struct PackageController {\n    func get() -> String { \"\" }\n  }\n}\n",
                ),
            ],
            vec![
                method(
                    "API::PackageController::get",
                    "Sources/App/API.swift",
                    (3, 3),
                ),
                method("V2::PackageController::get", "Sources/App/V2.swift", (3, 3)),
            ],
        );
        assert_eq!(handler(&ctx, ROUTES, "PackageController@get", 3), None);
    }

    #[test]
    fn a_dotted_handler_takes_the_extension_its_type_path_names() {
        let extension = "Sources/App/API/SearchController+API.swift";
        let top = "Sources/App/SearchController.swift";
        let commented = "Sources/App/Legacy/SearchController.swift";
        let show = "    func show(req: Request) -> String { \"\" }\n";
        let files = [
            (
                extension,
                format!("extension API.SearchController {{\n{show}}}\n"),
            ),
            (top, format!("struct SearchController {{\n{show}}}\n")),
            (
                commented,
                format!(
                    "struct SearchController {{\n    // extension API.SearchController\n{show}}}\n"
                ),
            ),
        ];
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, src)| (*path, src.as_str()))
            .collect();
        let in_extension = method("SearchController::show", extension, (2, 2));
        let ctx = ctx_of(
            &files,
            vec![
                node(
                    NodeKind::Class,
                    "SearchController",
                    "SearchController",
                    extension,
                    (1, 3),
                ),
                in_extension.clone(),
                node(
                    NodeKind::Struct,
                    "SearchController",
                    "SearchController",
                    top,
                    (1, 3),
                ),
                method("SearchController::show", top, (2, 2)),
            ],
        );
        assert_eq!(
            handler(&ctx, ROUTES, "API.SearchController@show", 3),
            Some(in_extension.id)
        );
        let in_comment = method("SearchController::show", commented, (3, 3));
        let ctx = ctx_of(
            &files,
            vec![
                node(
                    NodeKind::Struct,
                    "SearchController",
                    "SearchController",
                    top,
                    (1, 3),
                ),
                method("SearchController::show", top, (2, 2)),
                node(
                    NodeKind::Struct,
                    "SearchController",
                    "SearchController",
                    commented,
                    (1, 4),
                ),
                in_comment.clone(),
            ],
        );
        assert_eq!(
            handler(&ctx, ROUTES, "API.SearchController@show", 3),
            Some(in_comment.id)
        );
        let ctx = ctx_of(
            &files,
            vec![
                node(
                    NodeKind::Struct,
                    "SearchController",
                    "SearchController",
                    top,
                    (1, 3),
                ),
                method("SearchController::show", top, (2, 2)),
            ],
        );
        assert_eq!(handler(&ctx, ROUTES, "API.SearchController@show", 3), None);
    }

    const SITE_MAP: &str = "Sources/App/Controllers/SiteMapController.swift";

    fn site_map(src: &str, overloads: [(i64, i64); 2]) -> (RouteCtx, [String; 2]) {
        let first = method("SiteMapController::index", SITE_MAP, overloads[0]);
        let second = method("SiteMapController::index", SITE_MAP, overloads[1]);
        let ids = [first.id.clone(), second.id.clone()];
        let ctx = ctx_of(
            &[(SITE_MAP, src)],
            vec![
                node(
                    NodeKind::Enum,
                    "SiteMapController",
                    "SiteMapController",
                    SITE_MAP,
                    (1, 6),
                ),
                first,
                second,
            ],
        );
        (ctx, ids)
    }

    /// Of one type's overloads, the sole one whose head declares a
    /// `Request` (KEEP-RUST: none when no overload or several do).
    #[test]
    fn of_one_types_overloads_the_sole_one_taking_a_request_handles() {
        let resolved = |src: &str, overloads: [(i64, i64); 2]| {
            let (ctx, ids) = site_map(src, overloads);
            let found = handler(&ctx, ROUTES, "SiteMapController@index", 3);
            found.map(|found| ids.iter().position(|id| *id == found).unwrap())
        };
        let takes_second = "enum SiteMapController {\n    static func index(packages: [String]) -> String { packages.joined() }\n    @Sendable\n    static func index(req: Request) async throws -> String { \"\" }\n}\n";
        assert_eq!(resolved(takes_second, [(2, 2), (3, 4)]), Some(1));
        let takes_both = "enum SiteMapController {\n    static func index(req: Request, page: Int) -> String { \"\" }\n    @Sendable\n    static func index(req: Request) async throws -> String { \"\" }\n}\n";
        assert_eq!(resolved(takes_both, [(2, 2), (3, 4)]), None);
        let takes_none = "enum SiteMapController {\n    static func index(packages: [String]) -> String { packages.joined() }\n    @Sendable\n    static func index(names: [String]) async throws -> String { \"\" }\n}\n";
        assert_eq!(resolved(takes_none, [(2, 2), (3, 4)]), None);
        // A head reads at most to its node's last line: the next
        // declaration's `Request` is not the first overload's.
        let one_liner = "enum SiteMapController {\n    static func index() -> String { \"\" }\n    static func index(req: Request) async throws -> String { \"\" }\n}\n";
        assert_eq!(resolved(one_liner, [(2, 2), (3, 3)]), Some(1));
        // A head of CRLF lines, its parameter on a line of its own.
        // `Request` is a whole word: a `RequestContext` is no request.
        let context_first = "enum SiteMapController {\n    static func index(ctx: RequestContext) -> String { \"\" }\n    @Sendable\n    static func index(req: Request) async throws -> String { \"\" }\n}\n";
        assert_eq!(resolved(context_first, [(2, 2), (3, 4)]), Some(1));
        let crlf = "enum SiteMapController {\r\n    static func index(packages: [String]) -> String { \"\" }\r\n    static func index(\r\n        req: Request\r\n    ) async throws -> String { \"\" }\r\n}\r\n";
        assert_eq!(resolved(crlf, [(2, 2), (3, 5)]), Some(1));
    }

    /// The route's own type's method in the route's file comes before an
    /// overload another extension of the type declares: the two are never
    /// weighed against each other.
    #[test]
    fn the_routes_own_method_comes_before_another_extensions_overload() {
        let file = "Sources/App/Controllers/TodoController.swift";
        let other = "Sources/App/Controllers/TodoController+Paging.swift";
        let src = "import Vapor\nstruct TodoController: RouteCollection {\n    func boot(routes: RoutesBuilder) throws {\n        routes.get(use: index)\n    }\n\n\n    func index(req: Request) -> String { \"\" }\n\n\n\n}\n";
        let paging = "extension TodoController {\n    func index(req: Request, page: Int) -> String { \"\" }\n}\n";
        let own = method("TodoController::index", file, (8, 8));
        let ctx = ctx_of(
            &[(file, src), (other, paging)],
            vec![
                node(
                    NodeKind::Struct,
                    "TodoController",
                    "TodoController",
                    file,
                    (2, 12),
                ),
                own.clone(),
                node(
                    NodeKind::Class,
                    "TodoController",
                    "TodoController",
                    other,
                    (1, 3),
                ),
                method("TodoController::index", other, (2, 2)),
            ],
        );
        assert_eq!(handler(&ctx, file, "@index", 4), Some(own.id));
    }

    /// A handler is a Swift method or function: a Ruby controller's action
    /// on a type of the same name is none.
    #[test]
    fn a_handler_is_a_swift_callable() {
        let mut action = method(
            "TodoController::index",
            "app/controllers/todo_controller.rb",
            (2, 2),
        );
        action.language = Language::Ruby;
        let ctx = ctx_of(&[], vec![action]);
        assert_eq!(handler(&ctx, ROUTES, "TodoController@index", 3), None);
    }

    /// A dotted type path that no type and no extension head names finds
    /// nothing: a namesake nested elsewhere (`Legacy::SearchController`) is
    /// no `API.SearchController`.
    #[test]
    fn a_dotted_handler_no_type_or_extension_names_finds_nothing() {
        let file = "Sources/App/Legacy/SearchController.swift";
        let src = "enum Legacy {\n    struct SearchController {\n        func show(req: Request) -> String { \"\" }\n    }\n}\n";
        let ctx = ctx_of(
            &[(file, src)],
            vec![
                node(NodeKind::Enum, "Legacy", "Legacy", file, (1, 5)),
                node(
                    NodeKind::Struct,
                    "SearchController",
                    "Legacy::SearchController",
                    file,
                    (2, 4),
                ),
                method("Legacy::SearchController::show", file, (3, 3)),
            ],
        );
        assert_eq!(handler(&ctx, ROUTES, "API.SearchController@show", 3), None);
    }

    /// An extension's head is its first three lines, read with ASCII word
    /// boundaries as upstream's `\b` reads them: two attribute lines may
    /// stand before the keyword, and a letter outside ASCII next to the head
    /// is no word character.
    #[test]
    fn an_extension_head_is_read_as_upstream_reads_it() {
        let extension = "Sources/App/API/SearchController+API.swift";
        let cases = [
            (
                "@available(macOS 13, *)\n@MainActor\nextension API.SearchController {\n    func show(req: Request) -> String { \"\" }\n}\n",
                NodeKind::Class,
            ),
            (
                "// Üextension API.SearchControllerÉ\nstruct SearchController {\n    func show(req: Request) -> String { \"\" }\n}\n",
                NodeKind::Struct,
            ),
        ];
        for (src, kind) in cases {
            let show = method("SearchController::show", extension, (4, 4));
            let ctx = ctx_of(
                &[(extension, src)],
                vec![
                    node(
                        kind,
                        "SearchController",
                        "SearchController",
                        extension,
                        (1, 5),
                    ),
                    show.clone(),
                ],
            );
            assert_eq!(
                handler(&ctx, ROUTES, "API.SearchController@show", 3),
                Some(show.id),
                "{src}"
            );
        }
    }

    #[test]
    fn the_innermost_type_owns_the_route() {
        let file = "Sources/App/API.swift";
        let inner_index = method("API::Inner::index", file, (4, 4));
        let ctx = ctx_of(
            &[],
            vec![
                node(NodeKind::Enum, "API", "API", file, (1, 7)),
                node(NodeKind::Struct, "Inner", "API::Inner", file, (2, 5)),
                inner_index.clone(),
                method("API::index", file, (6, 6)),
            ],
        );
        assert_eq!(
            handler(&ctx, file, "@index", 3),
            Some(inner_index.id.clone())
        );
        // Two types on one line: the later by id, whatever order the file's
        // nodes come in.
        let ctx = ctx_of(
            &[],
            vec![
                node(NodeKind::Struct, "Inner", "API::Inner", file, (2, 5)),
                node(NodeKind::Enum, "API", "API", file, (2, 7)),
                inner_index.clone(),
                method("API::index", file, (6, 6)),
            ],
        );
        assert_eq!(handler(&ctx, file, "@index", 3), Some(inner_index.id));
    }

    #[test]
    fn a_handler_found_nowhere_resolves_to_none() {
        let ctx = ctx_of(
            &[],
            vec![node(
                NodeKind::Class,
                "TodoController",
                "TodoController",
                "Sources/App/Controllers/TodoController.swift",
                (1, 3),
            )],
        );
        assert_eq!(handler(&ctx, ROUTES, "TodoController@index", 3), None);
        assert_eq!(handler(&ctx, ROUTES, "@index", 3), None);
    }

    #[test]
    fn joined_lines_split_as_upstream_does() {
        assert_eq!(joined_lines("a\r\nb\r\nc\r", 0, 3), "a b c\r");
        assert_eq!(joined_lines("a\nb\n", 1, 5), "b ");
        assert_eq!(joined_lines("a\nb", 2, 1), "");
        assert_eq!(utf16_prefix("A😀B", 2), "A\u{FFFD}");
        assert_eq!(utf16_prefix("A😀B", 3), "A😀");
    }
}
