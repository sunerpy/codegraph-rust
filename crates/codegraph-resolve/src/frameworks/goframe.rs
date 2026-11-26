//! GoFrame [`FrameworkResolver`]: the routes a request type's `g.Meta` tag
//! declares. Ports `upstream resolution/frameworks/goframe.ts` at `e8ddc4e2`
//! (#747).
//!
//! GoFrame's standard router binds routes reflectively
//! (`group.Bind(user.NewV1())`): no call names a path or a handler. The route
//! is written in the tag of the request type's embedded `g.Meta`, and the
//! controller method that serves it takes that type as a parameter, under a
//! name of its own (`DeptSearchReq` is served by `List`):
//!
//! ```go
//! type SignInReq struct {
//!     g.Meta `path:"/user/sign-in" method:"post"`
//! }
//! func (c *ControllerV1) SignIn(ctx context.Context, req *v1.SignInReq) (res *v1.SignInRes, err error)
//! ```
//!
//! [`GoFrameResolver::extract`] makes each such type a `route` node named
//! `POST /user/sign-in`, its request type qualified by the file's package
//! (`v1.SignInReq`) after [`GOFRAME_ROUTE_MARKER`] in its qualified name.
//! The synthesis stage's `goframeEdges` pass joins each route to the method
//! that takes it (`crate::synthesis`); this resolver resolves nothing. The
//! route keeps the tag's path as written: the prefix a `s.Group("/api", …)`
//! adds at runtime is not reconstructed.

use std::sync::LazyLock;

use codegraph_core::types::{Language, NodeKind};
use regex::Regex;

use super::framework_node;
use super::js_scan::JS_SPACE;
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::types::{FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedRef};

/// What a GoFrame route's qualified name holds before its request type, the
/// key the synthesis pass joins on: `cash.ListReq` with the file's package,
/// the bare type without one (`GOFRAME_ROUTE_MARKER`).
pub const GOFRAME_ROUTE_MARKER: &str = "::goframe-route:";

/// An upstream pattern with JavaScript's `\s` ([`JS_SPACE`]) for the `regex`
/// crate's Unicode one. Every pattern here spells `\w` as `[A-Za-z0-9_]`
/// and `\b` as `(?-u:\b)`, as JavaScript reads them.
fn js_regex(pattern: &str, what: &str) -> Regex {
    Regex::new(&pattern.replace(r"\s", JS_SPACE)).unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// A request type whose first field is a tagged `g.Meta`, and the tag
/// (`GOFRAME_META_RE`).
static META: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        r"(?-u:\b)type\s+([A-Z][A-Za-z0-9_]*)\s+struct\s*\{\s*g\.Meta\s+`([^`]*)`",
        "g.Meta request type",
    )
});
/// The tag's route path (`META_PATH_RE`); a response type's `mime:` tag has none.
static META_PATH: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r#"(?-u:\b)path:"([^"]+)""#, "g.Meta path"));
/// The tag's HTTP method (`META_METHOD_RE`).
static META_METHOD: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r#"(?-u:\b)method:"([^"]+)""#, "g.Meta method"));
/// The file's package clause (`GO_PACKAGE_RE`).
static PACKAGE: LazyLock<Regex> =
    LazyLock::new(|| js_regex(r"(?m)^\s*package\s+([A-Za-z0-9_]+)", "Go package clause"));

/// GoFrame's route metadata (`goframeResolver`, goframe.ts:57-118).
pub struct GoFrameResolver;

impl FrameworkResolver for GoFrameResolver {
    fn name(&self) -> &str {
        "goframe"
    }

    fn languages(&self) -> Option<&[Language]> {
        const LANGS: [Language; 1] = [Language::Go];
        Some(&LANGS)
    }

    /// GoFrame is `github.com/gogf/gf` (v1) or `github.com/gogf/gf/v2`, in
    /// the root `go.mod` (goframe.ts:61-65).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context
            .read_file("go.mod")
            .is_some_and(|go_mod| go_mod.contains("github.com/gogf/gf"))
    }

    /// The route → handler join is the synthesis stage's: no reference is
    /// this resolver's (goframe.ts:112-117).
    fn resolve(
        &self,
        _reference: &RefView,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        None
    }

    /// Each request type of the file whose `g.Meta` tag has a `path:`, as a
    /// route (goframe.ts:67-110).
    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        let mut result = FrameworkResolverExtractionResult {
            nodes: Vec::new(),
            references: Vec::new(),
        };
        if !file_path.ends_with(".go") || !content.contains("g.Meta") {
            return Some(result);
        }
        // Comments blanked char for char, strings and raw strings kept, so a
        // char offset of `safe` is one of `content`.
        let safe = strip_comments_for_regex(content, CommentLang::Go);
        let package = PACKAGE
            .captures(&safe)
            .map(|captures| captures[1].to_string());
        for captures in META.captures_iter(&safe) {
            let whole = captures.get(0).expect("the match");
            let tag = &captures[2];
            // A response type's `g.Meta` (`mime:"…"`) and other metadata
            // declare no route.
            let Some(path) = META_PATH.captures(tag) else {
                continue;
            };
            let path = &path[1];
            // GoFrame serves every method when the tag names none.
            let method = META_METHOD
                .captures(tag)
                .map_or_else(|| "ANY".to_string(), |method| method[1].to_uppercase());
            let line = 1 + safe[..whole.start()].matches('\n').count() as i64;
            let request_type = &captures[1];
            let join_key = match &package {
                Some(package) => format!("{package}.{request_type}"),
                None => request_type.to_string(),
            };
            // `match[0].length`: the match's UTF-16 code units, which the
            // char-for-char blanking keeps from the source.
            let start = safe[..whole.start()].chars().count();
            let width: usize = content
                .chars()
                .skip(start)
                .take(whole.as_str().chars().count())
                .map(char::len_utf16)
                .sum();
            result.nodes.push(framework_node(
                format!("route:{file_path}:{line}:{method}:{path}"),
                NodeKind::Route,
                format!("{method} {path}"),
                format!("{file_path}{GOFRAME_ROUTE_MARKER}{join_key}"),
                file_path.to_string(),
                line,
                line,
                0,
                width as i64,
                Language::Go,
                false,
            ));
        }
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::types::Node;

    fn routes(path: &str, source: &str) -> Vec<Node> {
        GoFrameResolver
            .extract(
                path,
                source,
                &FrameworkExtractionContext::without_config(""),
            )
            .expect("an extraction")
            .nodes
    }

    /// A request type is a route named `METHOD /path` whose qualified name
    /// carries its package-qualified type, on the line its declaration
    /// starts, as wide as the match in UTF-16 code units.
    #[test]
    fn a_request_type_is_a_route_named_by_its_method_and_path() {
        let declaration = "type DeptAddReq struct {\n\tg.Meta `path:\"/dept/add\" method:\"post\" summary:\"新增部门\"`";
        let source = format!(
            "package system\n\nimport \"github.com/gogf/gf/v2/frame/g\"\n\n{declaration}\n\tName string\n}}\n"
        );
        let nodes = routes("api/system/dept.go", &source);
        let [route] = nodes.as_slice() else {
            panic!("one route: {nodes:#?}");
        };
        assert_eq!(route.id, "route:api/system/dept.go:5:POST:/dept/add");
        assert_eq!(route.kind, NodeKind::Route);
        assert_eq!(route.name, "POST /dept/add");
        assert_eq!(
            route.qualified_name,
            "api/system/dept.go::goframe-route:system.DeptAddReq"
        );
        assert_eq!((route.start_line, route.end_line), (5, 5));
        assert_eq!(route.start_column, 0);
        assert_eq!(route.end_column, declaration.encode_utf16().count() as i64);
        assert_eq!(route.language, Language::Go);
        assert!(!route.is_exported);
    }

    /// A comment inside the match is blanked char for char; the width still
    /// counts the source's UTF-16 code units, an astral char as two, as
    /// upstream's `match[0].length` counts its blanked text.
    #[test]
    fn the_width_counts_a_blanked_comment_in_utf16() {
        let declaration = "type PingReq struct { /* 😀 */ g.Meta `path:\"/ping\"`";
        let nodes = routes("api/ping.go", &format!("package api\n{declaration}\n}}\n"));
        let [route] = nodes.as_slice() else {
            panic!("one route: {nodes:#?}");
        };
        assert_eq!(route.name, "ANY /ping");
        assert_eq!(route.end_column, declaration.encode_utf16().count() as i64);
    }

    /// No `method:` serves every method; a method is upper-cased; a tag
    /// without `path:` and a type whose `g.Meta` is not its first field are
    /// no routes; without a package clause the type is the bare key.
    #[test]
    fn the_tag_decides_the_route() {
        let source = "type AReq struct {\n\tg.Meta `path:\"/a\"`\n}\n\
                      type BReq struct {\n\tg.Meta `path:\"/b\" method:\"delete\"`\n}\n\
                      type BRes struct {\n\tg.Meta `mime:\"application/json\"`\n}\n\
                      type CReq struct {\n\tName string\n\tg.Meta `path:\"/c\"`\n}\n\
                      type dReq struct {\n\tg.Meta `path:\"/d\"`\n}\n";
        let names: Vec<(String, String)> = routes("api/x.go", source)
            .into_iter()
            .map(|route| (route.name, route.qualified_name))
            .collect();
        assert_eq!(
            names,
            [
                (
                    "ANY /a".to_string(),
                    "api/x.go::goframe-route:AReq".to_string()
                ),
                (
                    "DELETE /b".to_string(),
                    "api/x.go::goframe-route:BReq".to_string()
                ),
            ]
        );
    }

    /// A file that is not Go, or names no `g.Meta`, gives nothing.
    #[test]
    fn only_a_go_file_naming_g_meta_is_read() {
        let source = "package api\ntype AReq struct {\n\tg.Meta `path:\"/a\"`\n}\n";
        assert!(routes("api/x.txt", source).is_empty());
        assert!(routes("api/x.go", "package api\ntype AReq struct{}\n").is_empty());
        assert_eq!(routes("api/x.go", source).len(), 1);
    }
}
