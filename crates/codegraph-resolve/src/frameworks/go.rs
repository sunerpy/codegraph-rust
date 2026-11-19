//! Go server frameworks: Gin, Echo, Fiber, Chi and `net/http` (upstream
//! `resolution/frameworks/go.ts` at `e8ddc4e2`: the framework halves of
//! #2397 and #2408, `e1f339f`'s URL-shaped routes and Go 1.22 method
//! patterns (#1308, fixing #1259), #365's receivers, #89's comment
//! stripping).
//!
//! A project is Go's when its root `go.mod` reads and is not empty, indexed
//! or not (an input of the sync's `go` control kind), or when an indexed
//! file's path ends with `.go`.
//!
//! A `.go` file's comment-stripped text gives a route for each call
//! `x.GET("/path", handler)` on any identifier: Gin's and Echo's upper-case
//! verbs, Chi's and Fiber's `Get` … `Delete`, `net/http`'s `Handle` and
//! `HandleFunc`. Its first argument must be URL-shaped: a path that starts
//! with `/`, or on `Handle` and `HandleFunc` a Go 1.22 pattern such as
//! `"GET /users/{id}"`, which names the method; any other `Handle` or
//! `HandleFunc` route serves `ANY`. A route references its handler by the
//! handler expression's last identifier (`h.Follow` is `Follow`). A group's
//! prefix is not applied, as upstream has no mount pass.
//!
//! Resolution guesses, in upstream's order, a function for a `…Handler` or a
//! `Handle…` (0.8), a struct or an interface for a `…Service`, a
//! `…Repository` or a `…Store` (0.8), a function for a `…Middleware`, an
//! `Auth…` or a `Log…` (0.75), and a type of any kind for a PascalCase name
//! (0.7), each through the shared name heuristic ([`super::name_heuristic`])
//! among the declarations of the reference's own package, its own file's
//! first: Go reads a name written bare from its own package only. It leaves
//! an embedded type (`extends`, `implements`) to name matching and the
//! import resolver (#2397), and a name not written bare (`pkg.Name`,
//! `x[i].Name()`) alone (#2408, [`is_go_bare_name`]).
//!
//! Rust's own (KNOWN_DIFFS, KEEP-RUST): a tier resolves only through its
//! sole candidate, and a rule whose tier declines ends the chain, as
//! Spring's and ASP.NET's do. Valid Go cannot show the second: a package
//! declares no type and function of one name, and build-tag twins share
//! their kind, so a later rule could only decline again or find nothing.
//!
//! Two shortcuts keep every result. The name conditions are tested before
//! the bare check, both pure. A rule with no node of its kinds in the
//! reference's package skips the heuristic, which finds no candidate there
//! either: [`pick_by_name_outcome`] answers [`NamePick::NoCandidate`] when
//! no node of the name and kinds passes `accept`. A file holding none of
//! the verbs' needles ([`ROUTE_NEEDLES`]) is neither stripped nor scanned:
//! stripping only blanks comments, so a match's `.GET` is in the source.
//!
//! Offsets are byte offsets. Comments are blanked byte for byte
//! ([`crate::strip_comments::strip_go_comments_keeping_offsets`]), so an
//! offset into the stripped text indexes the source too; a column, a match
//! length or a captured string is measured as upstream measures it, in
//! UTF-16 code units, on the source at the same offsets ([`OffsetText`]).

use std::sync::OnceLock;

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::{JS_SPACE, is_js_space, js_trim};
use super::name_heuristic::{NamePick, PickOptions, pick_by_name_outcome};
use super::offset_text::OffsetText;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::name_matcher::is_go_bare_name;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The language the resolver extracts and resolves (upstream `languages`).
const LANGUAGES: [Language; 1] = [Language::Go];

/// What a file's source holds when the route pattern can match it: the `.`
/// and the verb of [`route_call`] (`.Handle` also opens `.HandleFunc`).
const ROUTE_NEEDLES: [&str; 13] = [
    ".GET", ".POST", ".PUT", ".PATCH", ".DELETE", ".OPTIONS", ".HEAD", ".Get", ".Post", ".Put",
    ".Patch", ".Delete", ".Handle",
];

const FUNCTION_KINDS: [NodeKind; 1] = [NodeKind::Function];
const SERVICE_KINDS: [NodeKind; 2] = [NodeKind::Struct, NodeKind::Interface];
/// The kinds a Go `type` declaration makes (upstream `GO_TYPE_KINDS`).
const TYPE_KINDS: [NodeKind; 3] = [NodeKind::Struct, NodeKind::Interface, NodeKind::TypeAlias];

/// A pattern ported from upstream: `{S}` is JavaScript's `\s`, `{NS}` its
/// `\S`, `{W}` the body of its `\w` (ASCII), and its `\b` is written
/// `(?-u:\b)`.
fn go_regex(pattern: &str) -> Regex {
    let not_space = format!("[^{}", &JS_SPACE[1..]);
    Regex::new(
        &pattern
            .replace("{S}", JS_SPACE)
            .replace("{NS}", &not_space)
            .replace("{W}", "A-Za-z0-9_"),
    )
    .expect("go pattern")
}

macro_rules! patterns {
    ($($(#[$doc:meta])* $name:ident = $pattern:expr;)*) => {
        $(
            $(#[$doc])*
            fn $name() -> &'static Regex {
                static PATTERN: OnceLock<Regex> = OnceLock::new();
                PATTERN.get_or_init(|| go_regex($pattern))
            }
        )*
    };
}

patterns! {
    /// A route registration: a verb called on any identifier with a string
    /// first argument and a handler.
    route_call = r#"(?-u:\b)[{W}]+\.(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Get|Post|Put|Patch|Delete|Handle|HandleFunc){S}*\({S}*"([^"]+)"{S}*,{S}*([^)]+)\)"#;
    /// A Go 1.22 mux pattern's method (upstream `matchGo122MethodPattern`).
    go122_method_pattern = r"^(GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|CONNECT|TRACE){S}+{NS}";
    /// A model's name (upstream's Pattern 4).
    model_name = r"^[A-Z][a-zA-Z]+$";
    /// A handler expression's last identifier, once its spaces are gone
    /// (upstream `extractGoTailIdent`).
    tail_identifier = r"(?:\.|^)([A-Za-z_][A-Za-z0-9_]*)$";
}

pub struct GoResolver;

impl FrameworkResolver for GoResolver {
    fn name(&self) -> &str {
        "go"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `go.mod` that reads and is not empty (JavaScript's truthy
    /// string: one holding only spaces counts), else an indexed `.go` file.
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if context
            .read_file_shared("go.mod")
            .is_some_and(|go_mod| !go_mod.is_empty())
        {
            return true;
        }
        context
            .get_all_files_shared()
            .iter()
            .any(|file| file.ends_with(".go"))
    }

    /// Upstream's four conventions in order, each among the declarations of
    /// the reference's own package; a rule that finds no candidate hands
    /// over to the next, one that declines ends the chain.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        // An embedded type is name matching's and the import resolver's
        // (#2397).
        if matches!(
            reference.reference_kind,
            EdgeKind::Extends | EdgeKind::Implements
        ) {
            return None;
        }
        let name = reference.reference_name.as_str();
        let rules: [(bool, &[NodeKind], f64); 4] = [
            // Pattern 1: a handler.
            (
                name.ends_with("Handler") || name.starts_with("Handle"),
                &FUNCTION_KINDS,
                0.8,
            ),
            // Pattern 2: a service, a repository or a store.
            (
                name.ends_with("Service")
                    || name.ends_with("Repository")
                    || name.ends_with("Store"),
                &SERVICE_KINDS,
                0.8,
            ),
            // Pattern 3: a middleware.
            (
                name.ends_with("Middleware") || name.starts_with("Auth") || name.starts_with("Log"),
                &FUNCTION_KINDS,
                0.75,
            ),
            // Pattern 4: a model, a type of any kind.
            (model_name().is_match(name), &TYPE_KINDS, 0.7),
        ];
        // Upstream checks that the name is written bare first; both checks
        // are pure, so the cheaper one goes first.
        if !rules.iter().any(|&(applies, _, _)| applies) || !is_go_bare_name(reference, context) {
            return None;
        }
        // The reference's package: its directory, `""` at the root (upstream
        // `slice(0, lastIndexOf('/') + 1)`), its direct files only.
        let dir = &reference.file_path[..reference.file_path.rfind('/').map_or(0, |at| at + 1)];
        let in_package = |node: &Node| {
            node.file_path
                .strip_prefix(dir)
                .is_some_and(|rest| !rest.contains('/'))
        };
        let options = PickOptions {
            same_directory: true,
            accept: Some(&in_package),
        };
        let named = context.get_nodes_by_name_shared(name);
        for (applies, kinds, confidence) in rules {
            // Without a node of its kinds in the package, the heuristic
            // finds no candidate: the next rule's turn.
            if !applies
                || !named
                    .iter()
                    .any(|node| kinds.contains(&node.kind) && in_package(node))
            {
                continue;
            }
            match pick_by_name_outcome(reference, kinds, &|_| false, context, &options) {
                NamePick::Picked(target) => {
                    return Some(ResolvedRef::new(
                        reference.clone(),
                        target,
                        confidence,
                        ResolvedBy::Framework,
                    ));
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
        let mut result = FrameworkResolverExtractionResult::default();
        if !file_path.ends_with(".go")
            || !ROUTE_NEEDLES.iter().any(|needle| content.contains(needle))
        {
            return Some(result);
        }
        let text = OffsetText::go(content);
        for found in route_call().captures_iter(&text.safe) {
            let whole = found.get(0).expect("whole match");
            let verb = found.get(1).expect("verb").as_str();
            let written = capture(&text, found.get(2));
            let prefix = go122_method(&written, verb);
            // The first argument is URL-shaped, or the call is just a method
            // sharing a verb's name: `cache.Put("key", v)` (#1259).
            if !written.starts_with('/') && prefix.is_none() {
                continue;
            }
            let line = text.line_at(whole.start());
            let (method, path) = match prefix {
                Some(method) => {
                    let path = written[method.len()..].trim_start_matches(is_js_space);
                    (method, path.to_string())
                }
                None if verb == "Handle" || verb == "HandleFunc" => ("ANY".to_string(), written),
                None => (verb.to_ascii_uppercase(), written),
            };
            let id = format!("route:{file_path}:{line}:{method}:{path}");
            result.nodes.push(framework_node(
                id.clone(),
                NodeKind::Route,
                format!("{method} {path}"),
                format!("{file_path}::route:{path}"),
                file_path.to_string(),
                line,
                line,
                0,
                text.units(whole.start(), whole.end()),
                Language::Go,
                false,
            ));
            if let Some(handler) = tail_ident(&capture(&text, found.get(3))) {
                result.references.push(RefView {
                    row_id: None,
                    from_node_id: id,
                    reference_name: handler,
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Go,
                    is_function_ref: false,
                    reference_subkind: None,
                });
            }
        }
        Some(result)
    }
}

/// A capture of the stripped text as upstream's holds it.
fn capture(text: &OffsetText<'_>, found: Option<regex::Match<'_>>) -> String {
    found.map_or_else(String::new, |found| {
        text.upstream(found.start(), found.end())
    })
}

/// The method a Go 1.22 mux pattern starts with (`"GET /users/{id}"`), on
/// `Handle` and `HandleFunc` only: the other verbs take a bare path
/// (upstream `matchGo122MethodPattern`).
fn go122_method(path: &str, verb: &str) -> Option<String> {
    if verb != "Handle" && verb != "HandleFunc" {
        return None;
    }
    go122_method_pattern()
        .captures(path)
        .map(|found| found[1].to_string())
}

/// A handler expression's last identifier (upstream `extractGoTailIdent`):
/// `pkg.Sub.handler` is `handler`, a function literal's last parameter type
/// (`func(c *gin.Context`, where the pattern's `[^)]+` stops) is `Context`.
/// Upstream trims the expression, removes every space and drops one
/// trailing `()`; the expression never ends with one, as it ends before the
/// call's first `)`.
fn tail_ident(expression: &str) -> Option<String> {
    let cleaned: String = js_trim(expression)
        .chars()
        .filter(|&ch| !is_js_space(ch))
        .collect();
    let cleaned = cleaned.strip_suffix("()").unwrap_or(&cleaned);
    tail_identifier()
        .captures(cleaned)
        .map(|found| found[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn go(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        GoResolver
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

    /// The name of the route `src`'s one line makes, if any.
    fn route_of(src: &str) -> Option<String> {
        let result = go("main.go", &format!("{src}\n"));
        assert!(result.nodes.len() <= 1, "{:?}", names(&result));
        names(&result).into_iter().next()
    }

    #[test]
    fn the_resolver_is_named_go_and_reads_go() {
        assert_eq!(GoResolver.name(), "go");
        assert_eq!(GoResolver.languages(), Some(&[Language::Go][..]));
        assert_eq!(GoResolver.resolve_languages(), Some(&[Language::Go][..]));
    }

    // -- Extraction: upstream frameworks.test.ts 1285-1339, 2151-2160 -----

    #[test]
    fn extracts_route_from_r_get() {
        let call = "r.GET(\"/users\", listUsers)";
        let result = go("main.go", &format!("{call}\n"));
        assert_eq!(names(&result), ["GET /users"]);
        assert_eq!(targets(&result), ["listUsers"]);
        let node = &result.nodes[0];
        assert_eq!(node.id, "route:main.go:1:GET:/users");
        assert_eq!(node.kind, NodeKind::Route);
        assert_eq!(node.qualified_name, "main.go::route:/users");
        assert_eq!(node.file_path, "main.go");
        assert_eq!((node.start_line, node.end_line), (1, 1));
        assert_eq!((node.start_column, node.end_column), (0, call.len() as i64));
        assert_eq!(node.language, Language::Go);
        let reference = &result.references[0];
        assert_eq!(reference.from_node_id, node.id);
        assert_eq!(reference.reference_kind, EdgeKind::References);
        assert_eq!((reference.line, reference.column), (1, 0));
        assert_eq!(reference.file_path, "main.go");
        assert_eq!(reference.language, Language::Go);
    }

    #[test]
    fn extracts_route_from_router_handlefunc() {
        let result = go("main.go", "router.HandleFunc(\"/items\", createItem)\n");
        assert_eq!(names(&result), ["ANY /items"]);
        assert_eq!(targets(&result), ["createItem"]);
    }

    #[test]
    fn extracts_gorilla_mux_handlefunc_on_a_subrouter_ignoring_chained_methods() {
        let result = go(
            "routes.go",
            "s.HandleFunc(\"/users/{id}\", listUsers).Methods(\"GET\")\n",
        );
        assert_eq!(names(&result), ["ANY /users/{id}"]);
        assert_eq!(targets(&result), ["listUsers"]);
    }

    /// #1259: a cache's `Put` and `Get`, an event bus's `Handle` share a
    /// router verb's name; their first arguments are keys, not paths.
    #[test]
    fn verb_named_calls_with_non_path_arguments_are_no_routes() {
        let src = [
            "c.Put(\"a\", 1)",
            "c.Put(\"user:123\", value)",
            "store.Get(\"config\", out)",
            "bus.Handle(\"user.created\", onUserCreated)",
            "m.HandleFunc(\"shutdown\", hook)",
        ]
        .join("\n");
        let result = go("cache.go", &src);
        assert!(result.nodes.is_empty(), "{:?}", names(&result));
        assert!(result.references.is_empty());
    }

    #[test]
    fn keeps_registrations_whose_paths_start_with_a_slash_for_every_router_style() {
        let src = [
            "r.Put(\"/users/{id}\", updateUser)",
            "v1.GET(\"/ping\", ping)",
            "mux.HandleFunc(\"/healthz\", health)",
        ]
        .join("\n");
        assert_eq!(
            names(&go("routes.go", &src)),
            ["PUT /users/{id}", "GET /ping", "ANY /healthz"]
        );
    }

    #[test]
    fn recognizes_go_122_method_patterns_on_handlefunc_and_extracts_the_method() {
        let result = go(
            "main.go",
            "mux.HandleFunc(\"GET /api/users/{id}\", getUser)\n",
        );
        assert_eq!(names(&result), ["GET /api/users/{id}"]);
        assert_eq!(targets(&result), ["getUser"]);
        let node = &result.nodes[0];
        assert_eq!(node.id, "route:main.go:1:GET:/api/users/{id}");
        assert_eq!(node.qualified_name, "main.go::route:/api/users/{id}");
    }

    #[test]
    fn skips_line_and_block_commented_calls() {
        let src = "\n// r.GET(\"/fake\", fakeHandler)\n/* r.POST(\"/also-fake\", anotherHandler) */\nr.GET(\"/real\", listUsers)\n";
        let result = go("main.go", src);
        assert_eq!(names(&result), ["GET /real"]);
        assert_eq!(targets(&result), ["listUsers"]);
        assert_eq!(result.nodes[0].start_line, 4);
    }

    // -- Extraction: the pattern ---------------------------------------------

    #[test]
    fn each_verb_names_its_method() {
        for (verb, method) in [
            ("GET", "GET"),
            ("POST", "POST"),
            ("PUT", "PUT"),
            ("PATCH", "PATCH"),
            ("DELETE", "DELETE"),
            ("OPTIONS", "OPTIONS"),
            ("HEAD", "HEAD"),
            ("Get", "GET"),
            ("Post", "POST"),
            ("Put", "PUT"),
            ("Patch", "PATCH"),
            ("Delete", "DELETE"),
            ("Handle", "ANY"),
            ("HandleFunc", "ANY"),
        ] {
            let result = go("main.go", &format!("r.{verb}(\"/x\", h)\n"));
            assert_eq!(names(&result), [format!("{method} /x")], "{verb}");
            assert_eq!(result.nodes[0].id, format!("route:main.go:1:{method}:/x"));
            assert_eq!(targets(&result), ["h"], "{verb}");
        }
    }

    #[test]
    fn other_calls_are_no_routes() {
        for call in [
            "r.Options(\"/x\", h)",
            "r.Head(\"/x\", h)",
            "r.Any(\"/x\", h)",
            "r.Connect(\"/x\", h)",
            "r.Trace(\"/x\", h)",
            "r.CONNECT(\"/x\", h)",
            "r.TRACE(\"/x\", h)",
            "r.ANY(\"/x\", h)",
            "r.Mount(\"/x\", h)",
            "r.Route(\"/x\", h)",
            "r.Group(\"/x\", h)",
            "r.Static(\"/x\", h)",
            "r.Use(\"/x\", h)",
            "r.get(\"/x\", h)",
            "r.GETS(\"/x\", h)",
            "r.HandleFn(\"/x\", h)",
            // The verb follows an identifier and its `.`.
            "e.Group(\"/a\").GET(\"/b\", h)",
            "(r).GET(\"/x\", h)",
            "r[0].GET(\"/x\", h)",
            "GET(\"/x\", h)",
            // A double-quoted first argument and a handler.
            "r.GET(`/x`, h)",
            "r.GET(\"/x\")",
            "r.GET(\"/x\" h)",
        ] {
            assert_eq!(route_of(call), None, "{call}");
            // Also in a file whose other route lets it past the verbs'
            // prefilter.
            let result = go("main.go", &format!("r.GET(\"/a\", h)\n{call}\n"));
            assert_eq!(names(&result), ["GET /a"], "{call}");
        }
    }

    /// #365: a route is registered on any identifier, a group's variable
    /// included; JavaScript's `\w` and `\b` are ASCII, so `über`'s receiver
    /// is `ber`.
    #[test]
    fn any_identifier_receives_a_route() {
        for receiver in [
            "r",
            "router",
            "PublicGroup",
            "userRouter",
            "v1",
            "_r",
            "r_2",
        ] {
            assert_eq!(
                route_of(&format!("{receiver}.GET(\"/x\", h)")).as_deref(),
                Some("GET /x"),
                "{receiver}"
            );
        }
        let result = go("main.go", "über.GET(\"/x\", h)\n");
        assert_eq!(names(&result), ["GET /x"]);
        assert_eq!(
            result.nodes[0].end_column,
            "ber.GET(\"/x\", h)".len() as i64
        );
        // A dotted receiver: the route is its last identifier's call.
        let result = go("main.go", "api.v1.GET(\"/x\", h)\n");
        assert_eq!(names(&result), ["GET /x"]);
        assert_eq!(result.nodes[0].end_column, "v1.GET(\"/x\", h)".len() as i64);
    }

    /// `e1f339f` (#1259): the first argument is a path from the root, or on
    /// `Handle` and `HandleFunc` a method pattern; an empty one is none.
    #[test]
    fn the_first_argument_must_be_url_shaped() {
        for (call, route) in [
            ("r.GET(\"/\", h)", Some("GET /")),
            ("r.GET(\"users\", h)", None),
            ("r.GET(\" /users\", h)", None),
            ("r.GET(\"\", h)", None),
            ("g.POST(\"\", h.SignUp)", None),
            ("r.GET(\"GET /x\", h)", None),
            ("mux.HandleFunc(\"x\", h)", None),
            ("mux.HandleFunc(\"GET x\", h)", Some("GET x")),
            ("mux.Handle(\"GET /x\", h)", Some("GET /x")),
        ] {
            assert_eq!(route_of(call).as_deref(), route, "{call}");
        }
    }

    /// `e1f339f`: a Go 1.22 mux pattern names its method on `Handle` and
    /// `HandleFunc`; the path is what follows it, without its leading spaces.
    /// The spaces are JavaScript's.
    #[test]
    fn a_go_122_pattern_names_the_method_on_handle_and_handlefunc() {
        for verb in ["Handle", "HandleFunc"] {
            for method in [
                "GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "CONNECT", "TRACE",
            ] {
                let result = go("main.go", &format!("mux.{verb}(\"{method} /x\", h)\n"));
                assert_eq!(names(&result), [format!("{method} /x")], "{verb} {method}");
                let node = &result.nodes[0];
                assert_eq!(node.id, format!("route:main.go:1:{method}:/x"));
                assert_eq!(node.qualified_name, "main.go::route:/x");
                assert_eq!(targets(&result), ["h"]);
            }
        }
        for (pattern, route) in [
            ("GET /x", Some("GET /x")),
            ("GET  /x", Some("GET /x")),
            ("GET\t/x", Some("GET /x")),
            ("GET /x ", Some("GET /x ")),
            ("GET\u{3000}/x", Some("GET /x")),
            // JavaScript's `\s` holds U+FEFF, and its `trimStart` drops it.
            ("GET\u{FEFF}/x", Some("GET /x")),
            ("GET \u{FEFF}/x", Some("GET /x")),
            // U+0085 is none of JavaScript's spaces.
            ("GET \u{85}", Some("GET \u{85}")),
            ("GET\u{85}/x", None),
            ("get /x", None),
            ("GET/x", None),
            ("GET ", None),
            ("ANY /x", None),
            ("GETS /x", None),
            (" GET /x", None),
        ] {
            assert_eq!(
                route_of(&format!("mux.HandleFunc(\"{pattern}\", h)")).as_deref(),
                route,
                "{pattern:?}"
            );
        }
    }

    #[test]
    fn a_route_is_on_the_line_its_call_starts_and_spans_the_whole_match() {
        let src = "package main\n\n/* a\nb */\nfunc init() {\n\tr.GET(\n\t\t\"/x\",\n\t\th,\n\t)\n\tr.POST(\"/y\", y)\n}\n";
        let result = go("main.go", src);
        // A handler followed by a trailing comma has no last identifier.
        assert_eq!(
            routes_and_handlers(&result),
            [
                ("GET /x".to_string(), None),
                ("POST /y".to_string(), Some("y".to_string()))
            ]
        );
        let get = &result.nodes[0];
        assert_eq!(get.id, "route:main.go:6:GET:/x");
        assert_eq!((get.start_line, get.end_line), (6, 6));
        assert_eq!(
            get.end_column,
            "r.GET(\n\t\t\"/x\",\n\t\th,\n\t)".len() as i64
        );
        assert_eq!(
            (result.nodes[1].start_line, result.references[0].line),
            (10, 10)
        );
    }

    /// Upstream measures a match in UTF-16 code units: a comment inside the
    /// call holding a two-byte and a four-byte character ends the route at
    /// neither its byte nor its character count.
    #[test]
    fn a_routes_end_column_counts_utf16_code_units() {
        let call = "r.GET(\"/é\", /* ü 😀 */ h)";
        let result = go("main.go", &format!("{call}\n"));
        assert_eq!(names(&result), ["GET /é"]);
        assert_eq!(targets(&result), ["h"]);
        let units = call.encode_utf16().count() as i64;
        assert_ne!(units, call.len() as i64);
        assert_ne!(units, call.chars().count() as i64);
        assert_eq!(result.nodes[0].end_column, units);
    }

    #[test]
    fn two_identical_routes_on_one_line_share_an_id_and_keep_both_references() {
        let result = go("main.go", "r.GET(\"/x\", a); r.GET(\"/x\", b)\n");
        let ids: Vec<&str> = result.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(ids, ["route:main.go:1:GET:/x", "route:main.go:1:GET:/x"]);
        assert_eq!(targets(&result), ["a", "b"]);
        assert!(
            result
                .references
                .iter()
                .all(|reference| reference.from_node_id == "route:main.go:1:GET:/x")
        );
    }

    /// Upstream `extractGoTailIdent`: the handler expression without its
    /// spaces, up to the call's first `)`, ends in the identifier it names.
    #[test]
    fn the_handler_is_the_expressions_last_identifier() {
        for (expression, handler) in [
            ("listUsers ", Some("listUsers")),
            (" api . query ", Some("query")),
            ("pkg.Sub.handler", Some("handler")),
            ("wrap(api.query)", Some("query")),
            ("func(c *gin.Context) {}", Some("Context")),
            ("func(c *Context) {}", None),
            ("mw(), h", None),
            ("static.New(\"./x\")", None),
            ("h.héllo", None),
            ("_h", Some("_h")),
            ("h2", Some("h2")),
            ("2h", None),
            ("h\n\t", Some("h")),
            // JavaScript's `\s` holds U+FEFF, not U+0085.
            ("h\u{FEFF}", Some("h")),
            ("h\u{85}", None),
        ] {
            let result = go("main.go", &format!("r.GET(\"/x\", {expression})\n"));
            assert_eq!(
                routes_and_handlers(&result),
                [("GET /x".to_string(), handler.map(str::to_string))],
                "{expression:?}"
            );
        }
    }

    #[test]
    fn a_groups_prefix_is_not_applied() {
        let result = go("main.go", "g := r.Group(\"/api\")\ng.GET(\"/users\", h)\n");
        assert_eq!(names(&result), ["GET /users"]);
    }

    #[test]
    fn routes_are_read_from_a_go_files_comment_stripped_text() {
        assert!(go("main.ts", "r.GET(\"/x\", h)\n").nodes.is_empty());
        assert!(go("main.go.txt", "r.GET(\"/x\", h)\n").nodes.is_empty());
        assert!(go("main.go", "package main\n").nodes.is_empty());
        assert_eq!(
            names(&go("cmd/api/main.go", "r.GET(\"/x\", h)\n")),
            ["GET /x"]
        );
        // A comment between the call's parts is blanked before the pattern
        // runs.
        assert_eq!(route_of("r.GET(/*x*/\"/a\", h)").as_deref(), Some("GET /a"));
    }

    // -- Detection ---------------------------------------------------------

    #[test]
    fn a_root_go_mod_that_is_not_empty_is_detected_indexed_or_not() {
        assert!(GoResolver.detect(&RouteCtx::default().file("go.mod", "module x\n\ngo 1.22\n")));
        assert!(GoResolver.detect(&RouteCtx::default().unlisted("go.mod", "module x\n")));
        // JavaScript's truthy string: one holding only spaces counts, an
        // empty one does not.
        assert!(GoResolver.detect(&RouteCtx::default().file("go.mod", " \n")));
        assert!(!GoResolver.detect(&RouteCtx::default().file("go.mod", "")));
        assert!(!GoResolver.detect(&RouteCtx::default().file("svc/go.mod", "module x\n")));
        assert!(!GoResolver.detect(&RouteCtx::default().file("package.json", "{}")));
    }

    #[test]
    fn an_indexed_go_file_is_detected() {
        assert!(GoResolver.detect(&RouteCtx::default().file("main.go", "package main\n")));
        assert!(
            GoResolver.detect(
                &RouteCtx::default()
                    .file("go.mod", "")
                    .file("cmd/tool/main.go", "package main\n")
            )
        );
        assert!(!GoResolver.detect(&RouteCtx::default().file("docs/main.go.md", "")));
        assert!(!GoResolver.detect(&RouteCtx::default().unlisted("main.go", "package main\n")));
    }

    /// Go sits where upstream registers it after Play: between Expo Router
    /// (Rust's slot for it, before GoFrame) and GoFrame.
    #[test]
    fn go_is_detected_between_expo_router_and_goframe() {
        let ctx = RouteCtx::default()
            .file("package.json", r#"{"dependencies":{"expo-router":"3"}}"#)
            .file(
                "go.mod",
                "module x\n\nrequire github.com/gogf/gf/v2 v2.7.0\n",
            );
        let names: Vec<String> = crate::frameworks::detect_frameworks(&ctx)
            .iter()
            .map(|resolver| resolver.name().to_string())
            .collect();
        let at = |name: &str| {
            names
                .iter()
                .position(|found| found == name)
                .unwrap_or_else(|| panic!("{name} in {names:?}"))
        };
        assert!(
            at("expo-router") < at("go") && at("go") < at("goframe"),
            "{names:?}"
        );
    }

    // -- Resolution ----------------------------------------------------------

    fn go_node(kind: NodeKind, name: &str, qualified: &str, file: &str) -> Node {
        let mut node = framework_node(
            format!("{}:{file}:{qualified}", kind.as_str()),
            kind,
            name.to_string(),
            qualified.to_string(),
            file.to_string(),
            5,
            6,
            0,
            0,
            Language::Go,
            true,
        );
        node.updated_at = 0;
        node
    }

    fn function(name: &str, file: &str) -> Node {
        go_node(NodeKind::Function, name, name, file)
    }

    fn structure(name: &str, file: &str) -> Node {
        go_node(NodeKind::Struct, name, name, file)
    }

    fn interface(name: &str, file: &str) -> Node {
        go_node(NodeKind::Interface, name, name, file)
    }

    fn alias(name: &str, file: &str) -> Node {
        go_node(NodeKind::TypeAlias, name, name, file)
    }

    fn method(owner: &str, name: &str, file: &str) -> Node {
        go_node(NodeKind::Method, name, &format!("{owner}::{name}"), file)
    }

    /// How a `kind` reference to `name` is written bare.
    fn spelled(kind: EdgeKind, name: &str) -> String {
        match kind {
            EdgeKind::Calls => format!("\t{name}(w, r)"),
            EdgeKind::Instantiates => format!("\tx := {name}{{}}"),
            EdgeKind::Extends | EdgeKind::Implements => format!("\t{name}"),
            _ => format!("func Ship(o {name}) int {{"),
        }
    }

    /// The resolver's target and confidence for a `kind` reference to `name`
    /// written bare in `file`, among `nodes`.
    fn resolve(nodes: &[Node], file: &str, name: &str, kind: EdgeKind) -> Option<(String, f64)> {
        let line = spelled(kind, name);
        let column = line.find(name).expect("spelled");
        resolve_written(nodes, file, &line, column, name, kind)
    }

    /// The resolver's target and confidence for a `kind` reference to `name`
    /// recorded at `column` of `line`, line 3 of `file`, among `nodes`.
    fn resolve_written(
        nodes: &[Node],
        file: &str,
        line: &str,
        column: usize,
        name: &str,
        kind: EdgeKind,
    ) -> Option<(String, f64)> {
        let mut ctx = RouteCtx::default().file(file, &format!("package x\n\n{line}\n"));
        for node in nodes {
            ctx = ctx.routes(&node.file_path, vec![node.clone()]);
        }
        let reference = RefView {
            row_id: None,
            from_node_id: format!("function:{file}:caller"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 3,
            column: column as i64,
            file_path: file.to_string(),
            language: Language::Go,
            is_function_ref: false,
            reference_subkind: None,
        };
        GoResolver
            .resolve(&reference, &ctx)
            .map(|resolved| (resolved.target_node_id, resolved.confidence))
    }

    #[test]
    fn each_convention_guesses_its_kinds_in_the_package_at_its_confidence() {
        for (target, kind, confidence) in [
            (
                function("OrderHandler", "api/handlers.go"),
                EdgeKind::Calls,
                0.8,
            ),
            (
                function("HandleOrder", "api/handlers.go"),
                EdgeKind::Calls,
                0.8,
            ),
            (
                structure("OrderService", "api/service.go"),
                EdgeKind::Instantiates,
                0.8,
            ),
            (
                interface("OrderService", "api/service.go"),
                EdgeKind::References,
                0.8,
            ),
            (
                interface("OrderRepository", "api/repo.go"),
                EdgeKind::References,
                0.8,
            ),
            (
                structure("OrderStore", "api/store.go"),
                EdgeKind::Instantiates,
                0.8,
            ),
            (
                function("CORSMiddleware", "api/mw.go"),
                EdgeKind::Calls,
                0.75,
            ),
            (function("AuthRequired", "api/mw.go"), EdgeKind::Calls, 0.75),
            (function("LogRequests", "api/mw.go"), EdgeKind::Calls, 0.75),
            (
                structure("Order", "api/order.go"),
                EdgeKind::References,
                0.7,
            ),
            (
                interface("Reader", "api/reader.go"),
                EdgeKind::References,
                0.7,
            ),
            (alias("Celsius", "api/temp.go"), EdgeKind::References, 0.7),
            // A handler's name comes before a middleware's.
            (function("AuthHandler", "api/mw.go"), EdgeKind::Calls, 0.8),
        ] {
            assert_eq!(
                resolve(
                    std::slice::from_ref(&target),
                    "api/routes.go",
                    &target.name,
                    kind
                ),
                Some((target.id.clone(), confidence)),
                "{}",
                target.name
            );
        }
    }

    #[test]
    fn each_convention_takes_only_its_kinds() {
        let file = "api/routes.go";
        // A struct named like a handler is no handler: it is a model.
        let handler = structure("OrderHandler", "api/types.go");
        assert_eq!(
            resolve(
                std::slice::from_ref(&handler),
                file,
                "OrderHandler",
                EdgeKind::Instantiates
            ),
            Some((handler.id.clone(), 0.7))
        );
        // A method is reached through a value, never by a bare name.
        assert_eq!(
            resolve(
                &[method("Server", "HandleEvent", "api/server.go")],
                file,
                "HandleEvent",
                EdgeKind::Calls
            ),
            None
        );
        assert_eq!(
            resolve(
                &[method("Logger", "LogRequest", "api/log.go")],
                file,
                "LogRequest",
                EdgeKind::Calls
            ),
            None
        );
        // A function named like a service, a struct named like a middleware.
        assert_eq!(
            resolve(
                &[function("OrderService", "api/service.go")],
                file,
                "OrderService",
                EdgeKind::Calls
            ),
            None
        );
        let config = structure("AuthConfig", "api/config.go");
        assert_eq!(
            resolve(
                std::slice::from_ref(&config),
                file,
                "AuthConfig",
                EdgeKind::References
            ),
            Some((config.id.clone(), 0.7))
        );
        // A model is a type, not a function.
        assert_eq!(
            resolve(
                &[function("Order", "api/order.go")],
                file,
                "Order",
                EdgeKind::Calls
            ),
            None
        );
    }

    #[test]
    fn the_model_convention_reads_an_ascii_pascal_case_name_of_two_letters_or_more() {
        let file = "api/routes.go";
        for name in ["ID", "ORDER", "Order"] {
            let target = structure(name, "api/types.go");
            assert_eq!(
                resolve(
                    std::slice::from_ref(&target),
                    file,
                    name,
                    EdgeKind::References
                ),
                Some((target.id.clone(), 0.7)),
                "{name}"
            );
        }
        for name in ["Order2", "order", "T", "Order_V1", "Order_Item", "_Order"] {
            assert_eq!(
                resolve(
                    &[structure(name, "api/types.go")],
                    file,
                    name,
                    EdgeKind::References
                ),
                None,
                "{name}"
            );
        }
    }

    /// #2408: Go reads a bare name from the reference's own package, the
    /// files of its directory; the root's is the root directory's.
    #[test]
    fn a_convention_guesses_only_in_the_references_own_package() {
        let file = "api/routes.go";
        for elsewhere in [
            "model/order.go",
            "api/v2/order.go",
            "api2/order.go",
            "order.go",
        ] {
            assert_eq!(
                resolve(
                    &[structure("Order", elsewhere)],
                    file,
                    "Order",
                    EdgeKind::References
                ),
                None,
                "{elsewhere}"
            );
        }
        assert_eq!(
            resolve(
                &[function("OrderHandler", "handlers/order.go")],
                file,
                "OrderHandler",
                EdgeKind::Calls
            ),
            None
        );
        let root = structure("Order", "order.go");
        assert_eq!(
            resolve(
                &[root.clone(), structure("Order", "model/order.go")],
                "main.go",
                "Order",
                EdgeKind::References
            ),
            Some((root.id.clone(), 0.7))
        );
        assert_eq!(
            resolve(
                &[structure("Order", "model/order.go")],
                "main.go",
                "Order",
                EdgeKind::References
            ),
            None
        );
    }

    /// Two build-tag twins of a type: the reference's own file comes first,
    /// and elsewhere in the package the tier holds both, so the convention
    /// declines (KEEP-RUST: upstream takes the first its store returns).
    #[test]
    fn the_references_own_file_comes_before_its_package() {
        let own = structure("Order", "api/order.go");
        let twin = structure("Order", "api/order_windows.go");
        let nodes = [own.clone(), twin.clone()];
        assert_eq!(
            resolve(&nodes, "api/order.go", "Order", EdgeKind::References),
            Some((own.id.clone(), 0.7))
        );
        assert_eq!(
            resolve(
                &nodes,
                "api/order_windows.go",
                "Order",
                EdgeKind::References
            ),
            Some((twin.id.clone(), 0.7))
        );
        assert_eq!(
            resolve(&nodes, "api/routes.go", "Order", EdgeKind::References),
            None
        );
    }

    /// Pattern 2's tier holds a struct and an interface; Pattern 4 would take
    /// the own file's type alias, but a convention that declines ends the
    /// chain (KEEP-RUST).
    #[test]
    fn a_convention_that_declines_ends_the_chain() {
        let nodes = [
            alias("OrderService", "api/routes.go"),
            structure("OrderService", "api/a.go"),
            interface("OrderService", "api/b.go"),
        ];
        assert_eq!(
            resolve(
                &nodes,
                "api/routes.go",
                "OrderService",
                EdgeKind::References
            ),
            None
        );
    }

    #[test]
    fn a_convention_without_a_candidate_hands_over_to_the_next() {
        let file = "api/routes.go";
        // No struct or interface for Pattern 2, a function for Pattern 3.
        let auth = function("AuthService", "api/auth.go");
        assert_eq!(
            resolve(
                std::slice::from_ref(&auth),
                file,
                "AuthService",
                EdgeKind::Calls
            ),
            Some((auth.id.clone(), 0.75))
        );
        // No struct or interface for Pattern 2, a type alias for Pattern 4.
        let service = alias("OrderService", "api/types.go");
        assert_eq!(
            resolve(
                std::slice::from_ref(&service),
                file,
                "OrderService",
                EdgeKind::References
            ),
            Some((service.id.clone(), 0.7))
        );
    }

    /// #2397: an embedded type is name matching's and the import resolver's.
    #[test]
    fn an_embedded_type_is_left_to_name_matching_and_the_import_resolver() {
        let reader = interface("Reader", "api/reader.go");
        for kind in [EdgeKind::Extends, EdgeKind::Implements] {
            assert_eq!(
                resolve(
                    std::slice::from_ref(&reader),
                    "api/store.go",
                    "Reader",
                    kind
                ),
                None,
                "{kind:?}"
            );
        }
        assert_eq!(
            resolve(
                std::slice::from_ref(&reader),
                "api/store.go",
                "Reader",
                EdgeKind::References
            ),
            Some((reader.id.clone(), 0.7))
        );
    }

    /// #2408: a name written through an import or a value is not read from
    /// the reference's own package.
    #[test]
    fn a_name_not_written_bare_is_left_alone() {
        let file = "api/routes.go";
        let order = structure("Order", "api/order.go");
        let line = "func Ship(o model.Order) int {";
        assert_eq!(
            resolve_written(
                std::slice::from_ref(&order),
                file,
                line,
                line.find("Order").expect("spelled"),
                "Order",
                EdgeKind::References
            ),
            None
        );
        // A call through a value of unknown type, recorded at its expression.
        let handle = function("HandleEvent", "api/events.go");
        assert_eq!(
            resolve_written(
                std::slice::from_ref(&handle),
                file,
                "\thandlers[0].HandleEvent(e)",
                1,
                "HandleEvent",
                EdgeKind::Calls
            ),
            None
        );
        assert_eq!(
            resolve(
                std::slice::from_ref(&order),
                file,
                "Order",
                EdgeKind::References
            ),
            Some((order.id.clone(), 0.7))
        );
        assert_eq!(
            resolve(
                std::slice::from_ref(&handle),
                file,
                "HandleEvent",
                EdgeKind::Calls
            ),
            Some((handle.id.clone(), 0.8))
        );
    }

    /// A route's handler is recorded at the start of its line: it is bare as
    /// the line writes it (`UsersHandler`, not `h.UsersHandler`).
    #[test]
    fn a_routes_handler_is_bare_as_its_line_writes_it() {
        let file = "api/routes.go";
        let handler = function("UsersHandler", "api/users.go");
        assert_eq!(
            resolve_written(
                std::slice::from_ref(&handler),
                file,
                "\tr.GET(\"/users\", UsersHandler)",
                0,
                "UsersHandler",
                EdgeKind::References
            ),
            Some((handler.id.clone(), 0.8))
        );
        assert_eq!(
            resolve_written(
                std::slice::from_ref(&handler),
                file,
                "\tr.GET(\"/users\", h.UsersHandler)",
                0,
                "UsersHandler",
                EdgeKind::References
            ),
            None
        );
    }
}
