//! The Swift ↔ Objective-C bridge (upstream
//! `resolution/frameworks/swift-objc.ts` and `resolution/swift-objc-bridge.ts`
//! at `e8ddc4e2`, F12, `4d1a2b3c`).
//!
//! A project is the bridge's when an indexed path ends in `.swift` and
//! another in `.m` or `.mm`. It reads Swift and Objective-C references
//! and extracts nothing.
//!
//! A Swift call `cache.fetchEntry(forKey:)` reaches resolution as
//! `cache.fetchEntry`; the part after the last `.` is looked up among the
//! Swift base names every Objective-C method's selector bridges from
//! ([`swift_base_names_for_objc_selector`]: `fetchEntryForKey:` gives
//! `fetchEntry`), a name equal to a colon-less method's own name and
//! [`GENERIC_NAMES`] left out. An Objective-C message `[chart
//! animateWithXAxisDuration:… yAxisDuration:…]` reaches it as
//! `chart.animateWithXAxisDuration:yAxisDuration:`; each base name its
//! selector gives is tried in turn for a Swift method or function of that
//! name whose declaration is exposed to Objective-C ([`is_objc_exposed`] over
//! [`declaration_window`]: the three lines above its first line and that
//! line, comments included). Both answer `0.6`.
//!
//! Rust's own (KNOWN_DIFFS "The bridge a name means", KEEP-RUST): where
//! upstream takes the first of the sorted candidates (a Swift base name's
//! Objective-C methods, the exposed Swift declarations of the first base
//! name that has one), Rust answers only with the sole candidate and
//! nothing when there are several, without trying the next base name.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use codegraph_core::types::{Language, Node, NodeKind};
use regex::Regex;

use super::sole;
use crate::framework::FrameworkResolver;
use crate::types::{RefView, ResolutionContext, ResolvedBy, ResolvedRef};

/// The resolver's name, which its edges carry in `metadata.framework`.
pub const NAME: &str = "swift-objc-bridge";

/// The languages whose references it reads (upstream `languages`); it
/// extracts no file ([`FrameworkResolver::languages`] is empty).
const RESOLVE_LANGUAGES: [Language; 2] = [Language::Swift, Language::ObjC];

/// Names too generic to bridge (upstream `GENERIC_NAMES`): almost every
/// Objective-C class answers them, and the name matcher handles them.
pub const GENERIC_NAMES: [&str; 37] = [
    "init",
    "description",
    "debugDescription",
    "hash",
    "isEqual",
    "isEqualTo",
    "copy",
    "mutableCopy",
    "class",
    "self",
    "count",
    "length",
    "value",
    "name",
    "data",
    "string",
    "object",
    "add",
    "remove",
    "update",
    "load",
    "save",
    "reload",
    "cancel",
    "start",
    "stop",
    "pause",
    "resume",
    "close",
    "open",
    "show",
    "hide",
    "toString",
    "dealloc",
    "release",
    "retain",
    "autorelease",
];

/// The source lines above a declaration's first line its exposure window
/// holds (upstream `SOURCE_PROBE_LINES`).
const SOURCE_PROBE_LINES: i64 = 3;

/// A first selector keyword ending in a preposition before a capital:
/// `fetchEntryForKey` (upstream's pattern, its lazy prefix the base name).
fn preposition() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^([a-z][a-zA-Z0-9]*?)(?:With|For|By|In|On|At|From|To|Of|As)[A-Z]")
            .expect("preposition pattern")
    })
}

/// The Swift base names an Objective-C `selector` may bridge from, in the
/// order they are tried (upstream `swiftBaseNamesForObjcSelector`; the code
/// is authoritative over its doc comment's examples): the first keyword
/// with the trailing colons gone; `init` for an `initWith…` keyword; a
/// keyword's lazy prefix before a preposition and a capital
/// (`fetchEntryForKey` → `fetchEntry`); and the property `x` of a
/// one-keyword `setX:`.
pub fn swift_base_names_for_objc_selector(selector: &str) -> Vec<String> {
    if selector.is_empty() {
        return Vec::new();
    }
    let keywords: Vec<&str> = selector.trim_end_matches(':').split(':').collect();
    let first = keywords[0];
    if first.is_empty() {
        return Vec::new();
    }
    let mut candidates: Vec<String> = Vec::new();
    let mut add = |candidate: &str| {
        if !candidates.iter().any(|seen| seen == candidate) {
            candidates.push(candidate.to_string());
        }
    };
    add(first);
    if first.starts_with("initWith") {
        add("init");
    }
    if let Some(prefix) = preposition().captures(first).and_then(|found| found.get(1)) {
        add(prefix.as_str());
    }
    if keywords.len() == 1 && selector.ends_with(':') && is_setter_keyword(first) {
        let property = lower_first(&first[3..]);
        if !property.is_empty() {
            add(&property);
        }
    }
    candidates
}

/// `/^set[A-Z]/`.
fn is_setter_keyword(keyword: &str) -> bool {
    keyword
        .strip_prefix("set")
        .is_some_and(|rest| rest.starts_with(|ch: char| ch.is_ascii_uppercase()))
}

/// The string with its first character lowered (upstream `lowerFirst`).
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Whether a Swift declaration's source window exposes it to Objective-C
/// (upstream `isObjcExposed`): `@nonobjc` hides it whatever else it holds,
/// else `@objc` or `@objc(name)` exposes it; `@objcMembers` does not, as
/// `\b` reads it.
pub fn is_objc_exposed(window: &str) -> bool {
    if has_attribute(window, "@nonobjc") {
        return false;
    }
    has_attribute(window, "@objc")
}

/// Whether `text` holds `attribute` with no ASCII word character after it
/// (JavaScript's `/@objc\b/`).
fn has_attribute(text: &str, attribute: &str) -> bool {
    text.match_indices(attribute).any(|(at, _)| {
        text.as_bytes()
            .get(at + attribute.len())
            .is_none_or(|next| !(next.is_ascii_alphanumeric() || *next == b'_'))
    })
}

/// The lines of `content` a declaration starting at `start_line` is judged
/// by (upstream `declarationSourceWindow`): the three lines above it and its
/// first line, split as `/\r?\n/` splits them and joined by `\n`.
pub fn declaration_window(content: &str, start_line: i64) -> String {
    let lines = split_lines(content);
    let start = (start_line - 1 - SOURCE_PROBE_LINES).max(0) as usize;
    let end = start_line.clamp(0, lines.len() as i64) as usize;
    if start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

/// `content.split(/\r?\n/)`: a `\r` goes with the `\n` after it, and a
/// last line's own `\r` stays.
pub fn split_lines(content: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = content.split('\n').collect();
    let last = lines.len() - 1;
    for line in &mut lines[..last] {
        *line = line.strip_suffix('\r').unwrap_or(line);
    }
    lines
}

/// Whether a Swift method or function node is exposed to Objective-C, read
/// from its file's text.
pub fn node_is_objc_exposed(content: &str, node: &Node) -> bool {
    is_objc_exposed(&declaration_window(content, node.start_line))
}

/// The upstream lookup order of nodes (`ORDER BY file_path, start_line,
/// id`): the store lists a kind's nodes in no order.
pub(crate) fn node_order(a: &Node, b: &Node) -> std::cmp::Ordering {
    (&a.file_path, a.start_line, &a.id).cmp(&(&b.file_path, b.start_line, &b.id))
}

/// The Objective-C methods each Swift base name bridges to (upstream
/// `buildObjcMap`), each list in [`node_order`]: built once a resolution
/// pass.
#[derive(Default)]
struct ObjcBridgeMap {
    by_base: HashMap<String, Vec<Arc<Node>>>,
}

impl ObjcBridgeMap {
    fn build(context: &dyn ResolutionContext) -> Self {
        let mut methods =
            context.get_nodes_by_kind_in_languages_shared(NodeKind::Method, &[Language::ObjC]);
        methods.sort_by(|a, b| node_order(a, b));
        let mut by_base: HashMap<String, Vec<Arc<Node>>> = HashMap::new();
        for node in methods {
            for candidate in swift_base_names_for_objc_selector(&node.name) {
                // A colon-less method is no bridge of its own name, and a
                // generic name bridges nothing.
                if (candidate == node.name && !node.name.contains(':'))
                    || GENERIC_NAMES.contains(&candidate.as_str())
                {
                    continue;
                }
                by_base
                    .entry(candidate)
                    .or_default()
                    .push(Arc::clone(&node));
            }
        }
        Self { by_base }
    }
}

/// The resolver.
pub struct SwiftObjcBridgeResolver;

impl FrameworkResolver for SwiftObjcBridgeResolver {
    fn name(&self) -> &str {
        NAME
    }

    /// No file is extracted.
    fn languages(&self) -> Option<&[Language]> {
        Some(&[])
    }

    fn resolve_languages(&self) -> Option<&[Language]> {
        Some(&RESOLVE_LANGUAGES)
    }

    /// An indexed `.swift` path and an indexed `.m` or `.mm` one.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let files = context.get_all_files_shared();
        let mut swift = false;
        let mut objc = false;
        for file in files.iter() {
            if file.ends_with(".swift") {
                swift = true;
            } else if file.ends_with(".m") || file.ends_with(".mm") {
                objc = true;
            }
            if swift && objc {
                return true;
            }
        }
        false
    }

    /// A selector (a name with a `:`) names no Swift node, so the name
    /// pre-filter would drop it before [`Self::resolve`] runs.
    fn claims_reference(&self, name: &str) -> bool {
        name.contains(':')
    }

    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        let target = match reference.language {
            Language::Swift => swift_to_objc(reference, context),
            Language::ObjC => objc_to_swift(reference, context),
            _ => None,
        }?;
        Some(ResolvedRef::new(
            reference.clone(),
            target,
            0.6,
            ResolvedBy::Framework,
        ))
    }
}

/// The part of a reference name after its last `.`.
fn last_segment(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(_, tail)| tail)
}

/// A Swift call's Objective-C method: the sole one whose selector bridges
/// from the call's name (upstream takes the first).
fn swift_to_objc(reference: &RefView, context: &dyn ResolutionContext) -> Option<String> {
    let name = last_segment(&reference.reference_name);
    let map = context
        .route_tables()
        .get_or_build(NAME, || ObjcBridgeMap::build(context));
    let candidates = map.by_base.get(name)?;
    sole(candidates.iter()).map(|node| node.id.clone())
}

/// An Objective-C message's Swift method or function: of the first base
/// name its selector gives that names an exposed one, the sole exposed one
/// (upstream takes the first).
fn objc_to_swift(reference: &RefView, context: &dyn ResolutionContext) -> Option<String> {
    let selector = last_segment(&reference.reference_name);
    if !selector.contains(':') {
        return None;
    }
    for candidate in swift_base_names_for_objc_selector(selector) {
        let mut matches: Vec<Arc<Node>> = context
            .get_nodes_by_name_shared(&candidate)
            .into_iter()
            .filter(|node| {
                node.language == Language::Swift
                    && matches!(node.kind, NodeKind::Method | NodeKind::Function)
            })
            .collect();
        matches.sort_by(|a, b| node_order(a, b));
        let exposed: Vec<Arc<Node>> = matches
            .into_iter()
            .filter(|node| {
                context
                    .read_file_shared(&node.file_path)
                    .is_some_and(|content| node_is_objc_exposed(&content, node))
            })
            .collect();
        if !exposed.is_empty() {
            return sole(exposed).map(|node| node.id.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;
    use codegraph_core::types::EdgeKind;

    fn method(name: &str, language: Language, file: &str, line: i64) -> Node {
        Node {
            id: format!("{}:{file}:{name}:{line}", language.as_str()),
            kind: NodeKind::Method,
            name: name.to_string(),
            qualified_name: format!("{file}::{name}"),
            file_path: file.to_string(),
            language,
            start_line: line,
            end_line: line + 5,
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

    fn reference(name: &str, language: Language, file: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("caller:{file}"),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn ctx(nodes: Vec<Node>, files: &[(&str, &str)]) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for (path, content) in files {
            ctx = ctx.file(path, content);
        }
        let mut by_file: HashMap<String, Vec<Node>> = HashMap::new();
        for node in nodes {
            by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node);
        }
        let mut paths: Vec<String> = by_file.keys().cloned().collect();
        paths.sort();
        for path in paths {
            ctx = ctx.routes(&path, by_file.remove(&path).unwrap());
        }
        ctx
    }

    fn resolved(ctx: &RouteCtx, name: &str, language: Language) -> Option<(String, f64)> {
        SwiftObjcBridgeResolver
            .resolve(&reference(name, language, "Caller.x"), ctx)
            .map(|result| (result.target_node_id, result.confidence))
    }

    /// Upstream `swift-objc-bridge.test.ts` 100-156, in the order the
    /// candidates are tried.
    #[test]
    fn selector_base_names_follow_the_code() {
        let names = |selector: &str| swift_base_names_for_objc_selector(selector);
        assert_eq!(names("play"), ["play"]);
        assert_eq!(names("play:"), ["play"]);
        assert_eq!(names("playWithSong:"), ["playWithSong", "play"]);
        assert!(names("objectForKey:").contains(&"object".to_string()));
        assert!(names("stringWithFormat:").contains(&"string".to_string()));
        assert_eq!(names("imageNamed:inBundle:"), ["imageNamed"]);
        assert_eq!(names("play:by:"), ["play"]);
        assert_eq!(names("playWithSong:by:"), ["playWithSong", "play"]);
        assert_eq!(names("initWithName:"), ["initWithName", "init"]);
        assert_eq!(names("initWithName:age:"), ["initWithName", "init"]);
        assert_eq!(names("setName:"), ["setName", "name"]);
        assert_eq!(names("tableView:didSelectRowAtIndexPath:"), ["tableView"]);
        // The lazy prefix is the shortest before a preposition and a
        // capital; `With`'s own `init` comes first.
        assert_eq!(
            names("animateWithXAxisDuration:yAxisDuration:"),
            ["animateWithXAxisDuration", "animate"]
        );
        assert_eq!(
            names("fetchEntryForKey:"),
            ["fetchEntryForKey", "fetchEntry"]
        );
        assert_eq!(
            names("initWithFrame:"),
            ["initWithFrame", "init"],
            "init is added once, before the preposition's prefix"
        );
        // A setter takes one keyword and ends in a colon.
        assert_eq!(names("setName"), ["setName"]);
        assert_eq!(names("setName:value:"), ["setName"]);
        assert_eq!(names("settle:"), ["settle"]);
        assert_eq!(names("setX:"), ["setX", "x"]);
        // Trailing colons go; an empty first keyword names nothing.
        assert_eq!(names("play::"), ["play"]);
        assert!(names(":foo:").is_empty());
        assert!(names("").is_empty());
    }

    /// Upstream `swift-objc-bridge.test.ts` 174-188, and `\b`'s reading of
    /// `@objcMembers`.
    #[test]
    fn exposure_reads_objc_and_nonobjc() {
        assert!(is_objc_exposed("@objc func foo() {}"));
        assert!(is_objc_exposed("@objc(custom:) func foo() {}"));
        assert!(!is_objc_exposed("public func foo() {}"));
        assert!(!is_objc_exposed("@nonobjc @objc func foo() {}"));
        assert!(!is_objc_exposed("@objcMembers class Foo {}"));
        assert!(is_objc_exposed("@objcMembers class Foo {\n  @objc"));
        assert!(!is_objc_exposed("@nonobjcX func foo() {}\n@objcX"));
        assert!(is_objc_exposed("// @objc in a comment"));
    }

    /// The window holds the three lines above the declaration's first line
    /// and that line, `\r\n` read as `\n`.
    #[test]
    fn the_window_is_three_lines_above_and_the_first() {
        let content = "a\nb\r\nc\nd\ne\nf\n";
        assert_eq!(declaration_window(content, 5), "b\nc\nd\ne");
        assert_eq!(split_lines("a\r\nb\r"), ["a", "b\r"]);
        assert_eq!(declaration_window(content, 2), "a\nb");
        assert_eq!(declaration_window(content, 1), "a");
        assert_eq!(declaration_window(content, 0), "");
        assert_eq!(declaration_window(content, 20), "");
    }

    #[test]
    fn detects_swift_beside_objc_or_objcpp() {
        let both = ctx(
            vec![
                method("foo", Language::Swift, "A.swift", 1),
                method("bar", Language::ObjC, "B.m", 1),
            ],
            &[],
        );
        assert!(SwiftObjcBridgeResolver.detect(&both));
        let mm = ctx(
            vec![
                method("foo", Language::Swift, "A.swift", 1),
                method("bar", Language::ObjC, "B.mm", 1),
            ],
            &[],
        );
        assert!(SwiftObjcBridgeResolver.detect(&mm));
        let swift_only = ctx(vec![method("foo", Language::Swift, "A.swift", 1)], &[]);
        assert!(!SwiftObjcBridgeResolver.detect(&swift_only));
        let objc_only = ctx(vec![method("bar", Language::ObjC, "B.m", 1)], &[]);
        assert!(!SwiftObjcBridgeResolver.detect(&objc_only));
    }

    #[test]
    fn claims_selectors_reads_swift_and_objc_and_extracts_nothing() {
        let resolver = SwiftObjcBridgeResolver;
        assert!(resolver.claims_reference("fooWithBar:"));
        assert!(resolver.claims_reference("tableView:didSelectRowAtIndexPath:"));
        assert!(!resolver.claims_reference("foo"));
        assert_eq!(resolver.name(), "swift-objc-bridge");
        assert_eq!(resolver.languages(), Some(&[][..]));
        assert_eq!(
            resolver.resolve_languages(),
            Some(&[Language::Swift, Language::ObjC][..])
        );
    }

    /// Upstream `swift-objc-bridge-resolver.test.ts` 99-146.
    #[test]
    fn a_swift_call_reaches_the_objc_method_its_name_bridges_to() {
        let fetch = method("fetchEntryForKey:", Language::ObjC, "Cache.m", 10);
        let play = method("playWithSong:", Language::ObjC, "Player.m", 10);
        let init = method("initWithFrame:", Language::ObjC, "View.m", 10);
        let unrelated = method("unrelated:thing:", Language::ObjC, "X.m", 10);
        let colonless = method("reloadTable", Language::ObjC, "Y.m", 10);
        let ctx = ctx(
            vec![fetch.clone(), play.clone(), init, unrelated, colonless],
            &[],
        );
        assert_eq!(
            resolved(&ctx, "cache.fetchEntry", Language::Swift),
            Some((fetch.id.clone(), 0.6))
        );
        assert_eq!(
            resolved(&ctx, "fetchEntry", Language::Swift),
            Some((fetch.id, 0.6))
        );
        assert_eq!(
            resolved(&ctx, "play", Language::Swift),
            Some((play.id, 0.6))
        );
        // A generic name bridges nothing, a colon-less method is no bridge
        // of its own name, and an unknown name finds nothing.
        assert_eq!(resolved(&ctx, "init", Language::Swift), None);
        assert_eq!(resolved(&ctx, "reloadTable", Language::Swift), None);
        assert_eq!(resolved(&ctx, "completelyDifferent", Language::Swift), None);
        // Other languages are not the bridge's.
        assert_eq!(resolved(&ctx, "fetchEntry", Language::Kotlin), None);
    }

    /// KEEP-RUST: two Objective-C methods a Swift name bridges to leave the
    /// call unresolved, where upstream takes the first by path.
    #[test]
    fn two_bridged_objc_methods_leave_a_swift_call_alone() {
        let ctx = ctx(
            vec![
                method("fetchEntryForKey:", Language::ObjC, "A.m", 10),
                method("fetchEntryForKey:", Language::ObjC, "B.m", 10),
            ],
            &[],
        );
        assert_eq!(resolved(&ctx, "fetchEntry", Language::Swift), None);
    }

    /// Upstream `swift-objc-bridge-resolver.test.ts` 150-202.
    #[test]
    fn an_objc_message_reaches_the_exposed_swift_declaration() {
        let animate = method("animate", Language::Swift, "Chart.swift", 10);
        let init = method("init", Language::Swift, "MyClass.swift", 10);
        let pad = "\n".repeat(8);
        let exposed = format!(
            "{pad}@objc open func animate(xAxisDuration: Double, yAxisDuration: Double) {{}}\n"
        );
        let plain =
            format!("{pad}func animate(xAxisDuration: Double, yAxisDuration: Double) {{}}\n");
        let init_text = format!("{pad}@objc init(name: String, age: Int) {{}}\n");
        let exposed_ctx = ctx(
            vec![animate.clone(), init.clone()],
            &[("Chart.swift", &exposed), ("MyClass.swift", &init_text)],
        );
        assert_eq!(
            resolved(
                &exposed_ctx,
                "chart.animateWithXAxisDuration:yAxisDuration:",
                Language::ObjC
            ),
            Some((animate.id.clone(), 0.6))
        );
        assert_eq!(
            resolved(&exposed_ctx, "initWithName:age:", Language::ObjC),
            Some((init.id, 0.6))
        );
        let plain_ctx = ctx(vec![animate], &[("Chart.swift", &plain)]);
        assert_eq!(
            resolved(
                &plain_ctx,
                "animateWithXAxisDuration:yAxisDuration:",
                Language::ObjC
            ),
            None
        );
        assert_eq!(
            resolved(&plain_ctx, "someUnknownThing:", Language::ObjC),
            None
        );
        // A message without a colon is no selector the bridge reads, even
        // where an exposed declaration has its name.
        assert_eq!(resolved(&plain_ctx, "chart.animate", Language::ObjC), None);
        assert_eq!(
            resolved(&exposed_ctx, "chart.animate", Language::ObjC),
            None
        );
    }

    /// The base names are tried in order: the first that names an exposed
    /// Swift declaration decides, and two exposed ones there leave the
    /// message unresolved (KEEP-RUST), without trying the next base name.
    #[test]
    fn the_first_base_name_with_an_exposed_declaration_decides() {
        let exposed = "@objc func playWithSong(_ s: String) {}\n";
        let first = method("playWithSong", Language::Swift, "A.swift", 1);
        let second = method("playWithSong", Language::Swift, "B.swift", 1);
        let base = method("play", Language::Swift, "C.swift", 1);
        let files = [
            ("A.swift", exposed),
            ("B.swift", exposed),
            ("C.swift", "@objc func play(song: String) {}\n"),
        ];
        let one = ctx(vec![first.clone(), base.clone()], &files);
        assert_eq!(
            resolved(&one, "playWithSong:", Language::ObjC),
            Some((first.id.clone(), 0.6))
        );
        let two = ctx(vec![first, second, base.clone()], &files);
        assert_eq!(resolved(&two, "playWithSong:", Language::ObjC), None);
        // An unexposed declaration of the first base name hands over to
        // the next.
        let hidden = method("playWithSong", Language::Swift, "D.swift", 1);
        let next = ctx(
            vec![hidden, base.clone()],
            &[
                ("D.swift", "func playWithSong(_ s: String) {}\n"),
                ("C.swift", "@objc func play(song: String) {}\n"),
            ],
        );
        assert_eq!(
            resolved(&next, "playWithSong:", Language::ObjC),
            Some((base.id, 0.6))
        );
    }

    /// Each preposition upstream's pattern names gives the keyword's lazy
    /// prefix, the shortest one; `initWith` gives `init` with no capital
    /// after it.
    #[test]
    fn each_preposition_gives_the_lazy_prefix() {
        let names = |selector: &str| swift_base_names_for_objc_selector(selector);
        for (selector, prefix) in [
            ("playWithSong:", "play"),
            ("objectForKey:", "object"),
            ("sortByName:", "sort"),
            ("drawInRect:", "draw"),
            ("tapOnButton:", "tap"),
            ("insertAtIndex:", "insert"),
            ("loadFromURL:", "load"),
            ("moveToPoint:", "move"),
            ("sizeOfImage:", "size"),
            ("saveAsPNG:", "save"),
        ] {
            assert_eq!(
                names(selector),
                [selector.trim_end_matches(':'), prefix],
                "{selector}"
            );
        }
        assert_eq!(
            names("fetchEntryForKeyAtIndex:"),
            ["fetchEntryForKeyAtIndex", "fetchEntry"]
        );
        // A preposition before a lowercase letter is none.
        assert_eq!(names("drawInside:"), ["drawInside"]);
        assert_eq!(names("initWith:"), ["initWith", "init"]);
        assert_eq!(names("initWithframe:"), ["initWithframe", "init"]);
        assert_eq!(names("initialize:"), ["initialize"]);
    }

    /// Upstream's `GENERIC_NAMES`, written out: a Swift call by any of them
    /// bridges to no Objective-C method, which a name outside them does.
    #[test]
    fn a_generic_name_bridges_nothing() {
        let generic = [
            "init",
            "description",
            "debugDescription",
            "hash",
            "isEqual",
            "isEqualTo",
            "copy",
            "mutableCopy",
            "class",
            "self",
            "count",
            "length",
            "value",
            "name",
            "data",
            "string",
            "object",
            "add",
            "remove",
            "update",
            "load",
            "save",
            "reload",
            "cancel",
            "start",
            "stop",
            "pause",
            "resume",
            "close",
            "open",
            "show",
            "hide",
            "toString",
            "dealloc",
            "release",
            "retain",
            "autorelease",
        ];
        assert_eq!(generic.len(), GENERIC_NAMES.len());
        for name in generic.into_iter().chain(["refresh"]) {
            let objc = method(&format!("{name}:"), Language::ObjC, "Lib.m", 3);
            let ctx = ctx(vec![objc.clone()], &[]);
            let expected = (name == "refresh").then(|| (objc.id.clone(), 0.6));
            assert_eq!(
                resolved(&ctx, &format!("x.{name}"), Language::Swift),
                expected,
                "{name}"
            );
        }
    }

    /// The name a call bridges by is its last segment, however long its
    /// receiver.
    #[test]
    fn the_last_segment_of_a_long_call_is_its_name() {
        let fetch = method("fetchEntryForKey:", Language::ObjC, "Cache.m", 10);
        let ctx = ctx(vec![fetch.clone()], &[]);
        assert_eq!(
            resolved(&ctx, "self.cache.fetchEntry", Language::Swift),
            Some((fetch.id, 0.6))
        );
    }

    /// An exposed Swift function answers as a method does; a declaration
    /// of another language with the attribute in its window does not.
    #[test]
    fn an_exposed_swift_function_answers_and_another_language_does_not() {
        let mut function = method("animate", Language::Swift, "Util.swift", 2);
        function.kind = NodeKind::Function;
        let swift = ctx(
            vec![function.clone()],
            &[(
                "Util.swift",
                "@objc\nfunc animate(withDuration d: Double) {}\n",
            )],
        );
        assert_eq!(
            resolved(&swift, "x.animateWithDuration:", Language::ObjC),
            Some((function.id, 0.6))
        );
        let kotlin = method("animate", Language::Kotlin, "Util.kt", 2);
        let other = ctx(
            vec![kotlin],
            &[(
                "Util.kt",
                "// @objc\nfun animate(withDuration: Double) {}\n",
            )],
        );
        assert_eq!(
            resolved(&other, "x.animateWithDuration:", Language::ObjC),
            None
        );
        // An underscore continues the attribute's word.
        assert!(!is_objc_exposed("@objc_attr func foo() {}"));
    }
}
