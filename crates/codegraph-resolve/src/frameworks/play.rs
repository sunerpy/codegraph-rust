//! Play (upstream `resolution/frameworks/play.ts` at `e8ddc4e2`: #2156).
//!
//! Play declares its routes in `conf/routes` and `conf/*.routes` files, a
//! line each (`GET /users/:id controllers.Users.show(id: Long)`), which
//! [`codegraph_extract::is_play_routes_file`] indexes as YAML files. Each
//! line is a route node referencing its action as `Controller.action`, the
//! package dropped, which the resolver links to the method of that name in
//! the file of a class of that name: of the classes named so, the one
//! nearest the referencing file (KNOWN_DIFFS; upstream takes its store's
//! first). Only a routes file's references are Play's, and they take no
//! other strategy's answer ([`is_routes_reference`]; KNOWN_DIFFS: upstream
//! also guesses a `Controller.action` call Scala or Java code writes, and
//! lets name matching guess a line Play leaves).

use std::sync::{Arc, OnceLock};

use codegraph_core::types::{EdgeKind, Language, Node, NodeKind};
use codegraph_extract::is_play_routes_file;
use regex::Regex;

use super::framework_node;
use super::js_scan::{JS_SPACE, js_trim};
use crate::framework::{FrameworkExtractionContext, FrameworkResolver};
use crate::name_matcher::crosses_code_boundary;
use crate::types::{
    FrameworkResolverExtractionResult, RefView, ResolutionContext, ResolvedBy, ResolvedRef,
};

/// The languages the resolver reads (upstream `languages`): YAML for the
/// routes files, Scala and Java for a Play project's code.
const LANGUAGES: [Language; 3] = [Language::Scala, Language::Java, Language::Yaml];

/// The root files detection reads by path, indexed or not: inputs of the
/// sync's `jvm` control kind (`codegraph_watch::control_inputs`), since a
/// `build.sbt` and an `application.conf` are never indexed and a
/// configuration may keep `conf/routes` out of the index.
pub const ROOT_DETECTION_FILES: [&str; 3] = ["build.sbt", "conf/routes", "conf/application.conf"];

/// The node kinds an action is.
const ACTION_KINDS: [NodeKind; 2] = [NodeKind::Method, NodeKind::Function];

/// `build.sbt` naming Play (upstream's `/i`, which folds only ASCII
/// letters).
fn build_marker() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"(?i-u)playframework|"play"|sbt-plugin|PlayScala|PlayJava"#)
            .expect("play build marker")
    })
}

/// A routes line: `VERB path action` (upstream `ROUTE_LINE`; its `\S` is
/// JavaScript's non-space, its `.` no line terminator).
fn route_line() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let non_space = format!("[^{}", &JS_SPACE[1..]);
        Regex::new(&format!(
            r"^(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS){JS_SPACE}+({non_space}+){JS_SPACE}+([^\n\r\x{{2028}}\x{{2029}}]+)$"
        ))
        .expect("play route line")
    })
}

pub struct PlayResolver;

impl FrameworkResolver for PlayResolver {
    fn name(&self) -> &str {
        "play"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&LANGUAGES)
    }

    /// A root `build.sbt` naming Play, a root `conf/routes` or
    /// `conf/application.conf`, or a routes file anywhere (a repository of
    /// Play projects keeps each one's in its own directory, #2156).
    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if context
            .read_file_shared("build.sbt")
            .is_some_and(|content| build_marker().is_match(&content))
        {
            return true;
        }
        if context.file_exists("conf/routes") || context.file_exists("conf/application.conf") {
            return true;
        }
        context
            .get_all_files_shared()
            .iter()
            .any(|file| is_play_routes_file(file))
    }

    /// A `Controller.action` name, which names no node by itself, past the
    /// name pre-filter.
    fn claims_reference(&self, name: &str) -> bool {
        action_name(name).is_some()
    }

    /// `Controller.action`: the first method or function named `action` in
    /// the file of a class named `Controller`, of the classes named so the
    /// one nearest the reference ([`nearest`]), 0.9.
    fn resolve(&self, reference: &RefView, context: &dyn ResolutionContext) -> Option<ResolvedRef> {
        // A `Controller.action` that Scala or Java code writes is a call
        // imports and name matching resolve (KNOWN_DIFFS).
        if !is_routes_reference(reference) {
            return None;
        }
        let (class, action) = action_name(&reference.reference_name)?;
        let mut candidates: Vec<Arc<Node>> = Vec::new();
        for owner in context
            .get_nodes_by_name_shared(class)
            .iter()
            .filter(|node| node.kind == NodeKind::Class)
        {
            // The file's first by line, as upstream's `getNodesInFile`
            // orders them; on one line Rust takes the first by column and id.
            let Some(method) = context
                .get_nodes_in_file_shared(&owner.file_path)
                .into_iter()
                .filter(|node| ACTION_KINDS.contains(&node.kind) && node.name == action)
                .min_by(|a, b| {
                    (a.start_line, a.start_column, &a.id).cmp(&(
                        b.start_line,
                        b.start_column,
                        &b.id,
                    ))
                })
            else {
                continue;
            };
            if !candidates.iter().any(|known| known.id == method.id) {
                candidates.push(method);
            }
        }
        let method = nearest(&candidates, &reference.file_path)?;
        // A pick by name has no bridge into another code family.
        if crosses_code_boundary(method.language, reference.language) {
            return None;
        }
        Some(ResolvedRef::new(
            reference.clone(),
            method.id.clone(),
            0.9,
            ResolvedBy::Framework,
        ))
    }

    fn extract(
        &self,
        file_path: &str,
        content: &str,
        _context: &FrameworkExtractionContext,
    ) -> Option<FrameworkResolverExtractionResult> {
        if !is_play_routes_file(file_path) {
            return Some(FrameworkResolverExtractionResult::default());
        }
        Some(extract_routes(file_path, content))
    }
}

/// Whether `reference` is a routes line's, which only Play links: the
/// resolver takes no other strategy's answer for it, so a line Play leaves
/// (two controllers at one distance, an action in another code family, no
/// candidate) stays unresolved rather than guessed by name (KNOWN_DIFFS).
pub(crate) fn is_routes_reference(reference: &RefView) -> bool {
    is_play_routes_file(&reference.file_path)
}

/// `^([A-Za-z_]\w*)\.([A-Za-z_]\w*)$`: the class and the action.
pub(crate) fn action_name(name: &str) -> Option<(&str, &str)> {
    let (class, action) = name.split_once('.')?;
    (is_action_segment(class) && is_action_segment(action)).then_some((class, action))
}

/// `[A-Za-z_]\w*`: a name a `Controller.action` reference can spell on
/// either side of its dot (the sync's Play lane reads which ones changed).
pub fn is_action_segment(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Of `candidates`, the one whose file shares the most directories with
/// `file_path`'s, when only one does: Play projects side by side each hold a
/// `HomeController` (KNOWN_DIFFS; upstream takes its store's first class,
/// which links every project's routes to one of them).
fn nearest<'n>(candidates: &'n [Arc<Node>], file_path: &str) -> Option<&'n Arc<Node>> {
    let mut best: Option<(usize, &Arc<Node>)> = None;
    let mut tied = false;
    for candidate in candidates {
        let depth = shared_directories(&candidate.file_path, file_path);
        match best {
            Some((top, _)) if depth < top => {}
            Some((top, _)) if depth == top => tied = true,
            _ => {
                best = Some((depth, candidate));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, candidate)| candidate)
}

/// How many leading directories two files' paths share.
pub(super) fn shared_directories(a: &str, b: &str) -> usize {
    fn directory(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }
    let (a, b) = (directory(a), directory(b));
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(left, right)| left == right)
        .count()
}

/// A routes file's routes (upstream `playResolver.extract`): a `scala`
/// route per `VERB path action` line, referencing `Controller.action`.
/// Comments, `->` includes and lines naming no `Controller.action` are none.
fn extract_routes(file_path: &str, content: &str) -> FrameworkResolverExtractionResult {
    let mut result = FrameworkResolverExtractionResult::default();
    for (index, raw) in content.split('\n').enumerate() {
        let line = js_trim(raw);
        if line.is_empty() || line.starts_with('#') || line.starts_with("->") {
            continue;
        }
        let Some(captures) = route_line().captures(line) else {
            continue;
        };
        let (method, path, action) = (&captures[1], &captures[2], &captures[3]);
        let call = js_trim(action.split('(').next().unwrap_or_default());
        let parts: Vec<&str> = call.split('.').filter(|part| !part.is_empty()).collect();
        if parts.len() < 2 {
            continue;
        }
        let handler = parts[parts.len() - 2..].join(".");
        let number = index as i64 + 1;
        let node = framework_node(
            format!("route:{file_path}:{number}:{method}:{path}"),
            NodeKind::Route,
            format!("{method} {path}"),
            format!("{file_path}::{method}:{path}"),
            file_path.to_string(),
            number,
            number,
            0,
            0,
            Language::Scala,
            false,
        );
        result.references.push(RefView {
            row_id: None,
            from_node_id: node.id.clone(),
            reference_name: handler,
            reference_kind: EdgeKind::References,
            line: number,
            column: 0,
            file_path: file_path.to_string(),
            language: Language::Scala,
            is_function_ref: false,
            reference_subkind: None,
        });
        result.nodes.push(node);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;

    fn play(file: &str, src: &str) -> FrameworkResolverExtractionResult {
        PlayResolver
            .extract(
                file,
                src,
                &FrameworkExtractionContext::without_config("/project"),
            )
            .unwrap()
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

    fn controller(file: &str, class: &str, methods: &[&str], language: Language) -> Vec<Node> {
        let mut nodes = vec![node(NodeKind::Class, class, file, 1, language)];
        for (offset, method) in methods.iter().enumerate() {
            nodes.push(node(
                NodeKind::Method,
                method,
                file,
                3 + 3 * offset as i64,
                language,
            ));
        }
        nodes
    }

    fn route_ref(name: &str, file: &str, kind: EdgeKind, language: Language) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("route:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 1,
            column: 0,
            file_path: file.to_string(),
            language,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn resolve(ctx: &RouteCtx, reference: &RefView) -> Option<String> {
        PlayResolver.resolve(reference, ctx).map(|resolved| {
            assert_eq!(resolved.confidence, 0.9);
            resolved.target_node_id
        })
    }

    // --- Extraction (upstream frameworks.test.ts 1246-1285) ------------------

    #[test]
    fn routes_lines_reference_the_last_two_segments_of_their_action() {
        let src = "# Routes\nGET     /                       controllers.HomeController.index\nGET   /computers   controllers.Application.list(p: Int ?= 0)\nPOST  /computers   controllers.Application.save\n->    /admin       admin.Routes\nGET   /assets/*file   controllers.Assets.versioned(path = \"/public\", file: Asset)\nGET   /bare    index\nGET   /broken\r\n";
        let result = play("conf/routes", src);
        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "GET /",
                "GET /computers",
                "POST /computers",
                "GET /assets/*file"
            ]
        );
        let targets: Vec<&str> = result
            .references
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert_eq!(
            targets,
            [
                "HomeController.index",
                "Application.list",
                "Application.save",
                "Assets.versioned"
            ]
        );
        let first = &result.nodes[0];
        assert_eq!(first.id, "route:conf/routes:2:GET:/");
        assert_eq!(first.qualified_name, "conf/routes::GET:/");
        assert_eq!(
            (first.start_line, first.start_column, first.end_column),
            (2, 0, 0)
        );
        assert_eq!(first.language, Language::Scala);
        assert_eq!(result.references[1].line, 3);
        assert!(
            result
                .references
                .iter()
                .all(|r| r.language == Language::Scala
                    && r.reference_kind == EdgeKind::References
                    && r.column == 0)
        );
    }

    #[test]
    fn only_routes_files_are_read_and_a_line_separator_is_no_action_text() {
        assert!(
            play(
                "app/controllers/Home.scala",
                "GET / controllers.Home.index\n"
            )
            .nodes
            .is_empty()
        );
        assert_eq!(play("conf/api.routes", "GET /a a.B.c\n").nodes.len(), 1);
        assert_eq!(play("svc/conf/routes", "GET /a B.c\n").nodes.len(), 1);
        // JavaScript's `.` stops at U+2028: no route.
        assert!(
            play("conf/routes", "GET /a B.c\u{2028}x\n")
                .nodes
                .is_empty()
        );
        // Verbs are upper case, as upstream's pattern writes them.
        assert!(play("conf/routes", "get /a B.c\n").nodes.is_empty());
    }

    #[test]
    fn action_names_and_claims() {
        assert_eq!(action_name("Home.index"), Some(("Home", "index")));
        assert_eq!(action_name("_a1._b2"), Some(("_a1", "_b2")));
        for name in ["a.b.c", "1a.b", "a.", ".b", "a b.c", "ab", "a.b-c"] {
            assert_eq!(action_name(name), None, "{name}");
            assert!(!PlayResolver.claims_reference(name));
        }
        assert!(PlayResolver.claims_reference("Home.index"));
    }

    // --- Resolution ------------------------------------------------------------

    #[test]
    fn the_nearest_project_s_controller_wins() {
        let ctx = RouteCtx::default()
            .routes(
                "a/app/controllers/HomeController.scala",
                controller(
                    "a/app/controllers/HomeController.scala",
                    "HomeController",
                    &["index"],
                    Language::Scala,
                ),
            )
            .routes(
                "b/app/controllers/HomeController.java",
                controller(
                    "b/app/controllers/HomeController.java",
                    "HomeController",
                    &["index"],
                    Language::Java,
                ),
            );
        let b = route_ref(
            "HomeController.index",
            "b/conf/routes",
            EdgeKind::References,
            Language::Scala,
        );
        assert_eq!(
            resolve(&ctx, &b),
            Some("method:b/app/controllers/HomeController.java:index:3".to_string())
        );
        let a = route_ref(
            "HomeController.index",
            "a/conf/routes",
            EdgeKind::References,
            Language::Scala,
        );
        assert_eq!(
            resolve(&ctx, &a),
            Some("method:a/app/controllers/HomeController.scala:index:3".to_string())
        );
        // A tie at the best depth resolves nothing.
        let root = route_ref(
            "HomeController.index",
            "conf/routes",
            EdgeKind::References,
            Language::Scala,
        );
        assert_eq!(resolve(&ctx, &root), None);
        // One candidate needs no proximity.
        let ctx = RouteCtx::default().routes(
            "x/C.scala",
            controller("x/C.scala", "C", &["m"], Language::Scala),
        );
        assert_eq!(
            resolve(&ctx, &root_named("C.m")),
            Some("method:x/C.scala:m:3".to_string())
        );
    }

    fn root_named(name: &str) -> RefView {
        route_ref(name, "conf/routes", EdgeKind::References, Language::Scala)
    }

    #[test]
    fn the_file_s_first_action_and_one_candidate_per_method() {
        let mut nodes = controller("p/C.scala", "C", &["m", "m"], Language::Scala);
        // A second class of the name in the same file adds no candidate.
        nodes.push(node(NodeKind::Class, "C", "p/C.scala", 20, Language::Scala));
        let ctx = RouteCtx::default().routes("p/C.scala", nodes);
        assert_eq!(
            resolve(&ctx, &root_named("C.m")),
            Some("method:p/C.scala:m:3".to_string())
        );
        // A function counts; a class without the action does not.
        let ctx = RouteCtx::default()
            .routes(
                "q/D.scala",
                vec![
                    node(NodeKind::Class, "D", "q/D.scala", 1, Language::Scala),
                    node(NodeKind::Function, "run", "q/D.scala", 4, Language::Scala),
                ],
            )
            .routes(
                "r/D.scala",
                vec![node(NodeKind::Class, "D", "r/D.scala", 1, Language::Scala)],
            );
        assert_eq!(
            resolve(&ctx, &root_named("D.run")),
            Some("function:q/D.scala:run:4".to_string())
        );
        assert_eq!(resolve(&ctx, &root_named("D.missing")), None);
        assert_eq!(resolve(&ctx, &root_named("Nobody.run")), None);
    }

    #[test]
    fn only_a_routes_file_s_reference_is_play_s() {
        let ctx = RouteCtx::default().routes(
            "app/controllers/C.scala",
            controller("app/controllers/C.scala", "C", &["m"], Language::Scala),
        );
        assert_eq!(
            resolve(&ctx, &root_named("C.m")),
            Some("method:app/controllers/C.scala:m:3".to_string())
        );
        assert_eq!(
            resolve(
                &ctx,
                &route_ref(
                    "C.m",
                    "sub/conf/app.routes",
                    EdgeKind::References,
                    Language::Scala
                )
            ),
            Some("method:app/controllers/C.scala:m:3".to_string())
        );
        // The `Controller.action` that Scala or Java code writes is no
        // routes line: Play leaves it to imports and name matching.
        for (file, kind, language) in [
            ("app/Boot.scala", EdgeKind::Calls, Language::Scala),
            ("app/Boot.scala", EdgeKind::References, Language::Scala),
            ("app/Boot.java", EdgeKind::Calls, Language::Java),
        ] {
            let code = route_ref("C.m", file, kind, language);
            assert_eq!(resolve(&ctx, &code), None, "{file} {kind:?}");
            assert!(!is_routes_reference(&code));
        }
        assert!(is_routes_reference(&root_named("C.m")));
    }

    #[test]
    fn a_pick_in_another_code_family_resolves_nothing() {
        let ctx = RouteCtx::default().routes(
            "py/C.py",
            controller("py/C.py", "C", &["m"], Language::Python),
        );
        assert_eq!(resolve(&ctx, &root_named("C.m")), None);
        // Kotlin is the JVM's too.
        let ctx = RouteCtx::default().routes(
            "kt/C.kt",
            controller("kt/C.kt", "C", &["m"], Language::Kotlin),
        );
        assert_eq!(
            resolve(&ctx, &root_named("C.m")),
            Some("method:kt/C.kt:m:3".to_string())
        );
    }

    #[test]
    fn detection() {
        let ctx = RouteCtx::default().unlisted(
            "build.sbt",
            "addSbtPlugin(\"com.typesafe.play\" % \"sbt-plugin\" % \"2.8\")",
        );
        assert!(PlayResolver.detect(&ctx));
        let ctx = RouteCtx::default().unlisted(
            "build.sbt",
            "lazy val root = (project in file(\".\")).enablePlugins(playscala)",
        );
        assert!(PlayResolver.detect(&ctx));
        // Each `build.sbt` marker alone.
        for build in [
            "libraryDependencies += \"org.playframework\" %% \"play-json\" % \"3.0\"",
            "libraryDependencies += \"com.typesafe.play\" %% \"play\" % \"2.3\"",
            "lazy val root = (project in file(\".\")).enablePlugins(PlayJava)",
        ] {
            let ctx = RouteCtx::default().unlisted("build.sbt", build);
            assert!(PlayResolver.detect(&ctx), "{build}");
        }
        let ctx = RouteCtx::default().unlisted("conf/application.conf", "");
        assert!(PlayResolver.detect(&ctx));
        // A root `conf/routes` the index leaves out (a configuration
        // excludes it) still shows Play.
        let ctx = RouteCtx::default().unlisted("conf/routes", "");
        assert!(PlayResolver.detect(&ctx));
        let ctx = RouteCtx::default().file(
            "play-java-hello/conf/routes",
            "GET / controllers.HomeController.index\n",
        );
        assert!(PlayResolver.detect(&ctx));
        let ctx = RouteCtx::default()
            .unlisted("build.sbt", "name := \"x\"")
            .file("config/routes", "")
            .file("app/X.scala", "");
        assert!(!PlayResolver.detect(&ctx));
    }

    /// Spring and Play sit where upstream registers them, Spring first,
    /// after FastAPI and before Expo Router and GoFrame, and the detected
    /// list keeps that order.
    #[test]
    fn the_jvm_frameworks_are_detected_in_upstreams_order() {
        let ctx = RouteCtx::default()
            .file("package.json", r#"{"dependencies":{"expo-router":"3"}}"#)
            .file(
                "go.mod",
                "module x\n\nrequire github.com/gogf/gf/v2 v2.7.0\n",
            )
            .file("requirements.txt", "fastapi\n")
            .file("pom.xml", "<project>spring-boot</project>\n")
            .file("conf/routes", "GET / controllers.Home.index\n");
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
            at("fastapi") < at("spring") && at("spring") < at("play"),
            "{names:?}"
        );
        assert!(
            at("play") < at("expo-router") && at("play") < at("goframe"),
            "{names:?}"
        );
    }

    #[test]
    fn shared_directories_counts_leading_directories() {
        assert_eq!(shared_directories("a/b/c.x", "a/b/d.x"), 2);
        assert_eq!(shared_directories("a/b/c.x", "a/c/d.x"), 1);
        assert_eq!(shared_directories("c.x", "c.y"), 0);
        assert_eq!(shared_directories("a/c.x", "b/c.y"), 0);
        assert_eq!(shared_directories("ab/c.x", "a/c.y"), 0);
    }
}
