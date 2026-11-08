//! The shared pick behind the framework resolvers' name heuristics (upstream
//! `resolution/frameworks/name-heuristic.ts` at `e8ddc4e2`, #2225): "a
//! `…View` is a class or a function under `views/`", "`router` is a FastAPI
//! router under `/routers/`".
//!
//! [`pick_by_name_and_kind`] looks only at the declarations of the
//! reference's name that the reference can see: never one inside some
//! function body, never a type nested in another file's type (a nested type
//! is reached by its bare name only from inside its owner), never one the
//! language keeps out of the reference's file ([`is_visible_across_files`]).
//! It starts from the reference's own scope: its file, then (by default) its
//! directory, then a conventional folder, then the rest.
//!
//! Two rules are Rust's own (KNOWN_DIFFS):
//!
//! - a tier resolves only through its sole candidate ([`super::sole`]), and a
//!   tier with two or more proposes nothing, leaving the reference to the
//!   other strategies; upstream takes the first its store returns;
//! - a pick written in another code family than the reference proposes
//!   nothing. A framework's `calls` result passes the resolver's language
//!   gate, for the calls a framework bridges (a Tauri command, a Godot
//!   autoload); a name guess is no such bridge.

use std::sync::Arc;

use codegraph_core::file_class::{is_test_path, is_test_suite_path};
use codegraph_core::types::{EdgeKind, Node, NodeKind};

use crate::name_matcher::{crosses_code_boundary, is_lexically_reachable};
use crate::types::{RefView, ResolutionContext};

/// The node kinds a nested type can be declared in (upstream
/// `TYPE_OWNER_KINDS`; Rust has no `record` kind).
const TYPE_OWNER_KINDS: [NodeKind; 6] = [
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Enum,
    NodeKind::Trait,
    NodeKind::Protocol,
];

/// How a resolver's heuristic narrows its pick (upstream `NamePickOptions`).
pub(crate) struct PickOptions<'a> {
    /// Whether a file in the reference's directory comes before the
    /// conventional folders: its package (JVM, Go, C#, Python). Not for
    /// Rust, where a sibling file is another module.
    pub(crate) same_directory: bool,
    /// A language's own scope rule over the candidates.
    pub(crate) accept: Option<&'a dyn Fn(&Node) -> bool>,
}

impl Default for PickOptions<'_> {
    fn default() -> Self {
        Self {
            same_directory: true,
            accept: None,
        }
    }
}

/// The id of the node named like `reference`, of one of `kinds`, that the
/// reference can see: the sole one in its file, else (with
/// `same_directory`) the sole one in its directory, else the sole one in a
/// folder `in_preferred_dir` accepts, else the sole one at all (upstream
/// `pickByNameAndKind`). A tier with two or more, or a pick in another code
/// family than the reference, proposes nothing.
pub(crate) fn pick_by_name_and_kind(
    reference: &RefView,
    kinds: &[NodeKind],
    in_preferred_dir: &dyn Fn(&str) -> bool,
    context: &dyn ResolutionContext,
    options: &PickOptions<'_>,
) -> Option<String> {
    let inheritance = matches!(
        reference.reference_kind,
        EdgeKind::Extends | EdgeKind::Implements
    );
    let candidates: Vec<Arc<Node>> = context
        .get_nodes_by_name_shared(&reference.reference_name)
        .into_iter()
        .filter(|node| {
            kinds.contains(&node.kind)
                // A type never inherits from itself.
                && !(inheritance && node.id == reference.from_node_id)
                && is_lexically_reachable(node, reference, context)
                && (node.file_path == reference.file_path
                    || (!is_nested_type(node, context)
                        && is_visible_across_files(node, reference, context)))
                && options.accept.is_none_or(|accept| accept(node))
        })
        .collect();
    let picked = pick(&candidates, reference, in_preferred_dir, options)?;
    (!crosses_code_boundary(picked.language, reference.language)).then(|| picked.id.clone())
}

/// What one tier of the pick holds.
enum Tier<'n> {
    Empty,
    Sole(&'n Arc<Node>),
    Several,
}

fn tier<'n>(mut nodes: impl Iterator<Item = &'n Arc<Node>>) -> Tier<'n> {
    match (nodes.next(), nodes.next()) {
        (None, _) => Tier::Empty,
        (Some(node), None) => Tier::Sole(node),
        (Some(_), Some(_)) => Tier::Several,
    }
}

/// The tiers in order, the first that holds a candidate deciding.
fn pick<'n>(
    candidates: &'n [Arc<Node>],
    reference: &RefView,
    in_preferred_dir: &dyn Fn(&str) -> bool,
    options: &PickOptions<'_>,
) -> Option<&'n Arc<Node>> {
    // The reference's directory, `""` at the root (upstream
    // `slice(0, lastIndexOf('/') + 1)`): its direct files only.
    let dir = &reference.file_path[..reference.file_path.rfind('/').map_or(0, |at| at + 1)];
    let in_dir = |node: &&Arc<Node>| {
        node.file_path
            .strip_prefix(dir)
            .is_some_and(|rest| !rest.contains('/'))
    };
    let tiers: [Option<Tier<'n>>; 4] = [
        Some(tier(
            candidates
                .iter()
                .filter(|node| node.file_path == reference.file_path),
        )),
        options
            .same_directory
            .then(|| tier(candidates.iter().filter(in_dir))),
        Some(tier(
            candidates
                .iter()
                .filter(|node| in_preferred_dir(&node.file_path)),
        )),
        Some(tier(candidates.iter())),
    ];
    for tier in tiers.into_iter().flatten() {
        match tier {
            Tier::Empty => continue,
            Tier::Sole(node) => return Some(node),
            Tier::Several => return None,
        }
    }
    None
}

/// Whether `node` is declared inside a type of its own file (upstream
/// `isNestedType`): its qualified name's owner is a type node there.
fn is_nested_type(node: &Node, context: &dyn ResolutionContext) -> bool {
    let Some(cut) = node.qualified_name.rfind("::") else {
        return false;
    };
    let owner = &node.qualified_name[..cut];
    context
        .get_nodes_in_file_shared(&node.file_path)
        .iter()
        .any(|parent| parent.qualified_name == owner && TYPE_OWNER_KINDS.contains(&parent.kind))
}

/// Whether `candidate`, written in another file than `reference`, is one the
/// reference can name by itself (upstream `isVisibleAcrossFiles`). Upstream
/// keeps these rules in one function; Rust keeps them where its name match
/// applies them, so this composes those:
/// [`crate::resolver::target_is_visible_across_files`] (the language rules),
/// then `gate_test_suite`'s (a minified script, a test suite from code that
/// is no test, an ES module's unexported binding, a Svelte or Vue
/// component's private script, a Java type out of reach) and
/// [`crate::name_matcher::is_js_name_target_visible`] (a JSON `require`, a
/// sealed ES module). Unlike `gate_test_suite`, a C or C++ test suite the
/// reference's translation unit includes is no exception here: that
/// exception holds only for a C or C++ reference, which no resolver with a
/// name heuristic resolves.
pub(crate) fn is_visible_across_files(
    candidate: &Node,
    reference: &RefView,
    context: &dyn ResolutionContext,
) -> bool {
    if !crate::resolver::target_is_visible_across_files(candidate, reference, context) {
        return false;
    }
    if candidate.file_path == reference.file_path {
        return true;
    }
    !crate::resolver::is_minified_script(&candidate.file_path, context)
        && !(is_test_suite_path(&candidate.file_path) && !is_test_path(&reference.file_path))
        && crate::name_matcher::is_name_match_reachable(candidate, reference, context)
        && !crate::sfc_scripts::is_sfc_private(candidate, context)
        && crate::java_scope::is_java_type_visible(candidate, reference, context)
        && crate::name_matcher::is_js_name_target_visible(candidate, reference, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frameworks::route_href::test_ctx::RouteCtx;
    use codegraph_core::types::Language;

    fn node(kind: NodeKind, name: &str, qualified: &str, file: &str, line: i64) -> Node {
        let mut node = super::super::framework_node(
            format!("{}:{file}:{name}:{line}", kind.as_str()),
            kind,
            name.to_string(),
            qualified.to_string(),
            file.to_string(),
            line,
            line + 2,
            0,
            0,
            Language::Python,
            false,
        );
        node.updated_at = 0;
        node
    }

    fn class(name: &str, file: &str) -> Node {
        node(NodeKind::Class, name, name, file, 1)
    }

    fn reference(name: &str, kind: EdgeKind, file: &str) -> RefView {
        RefView {
            row_id: None,
            from_node_id: format!("file:{file}"),
            reference_name: name.to_string(),
            reference_kind: kind,
            line: 40,
            column: 4,
            file_path: file.to_string(),
            language: Language::Python,
            is_function_ref: false,
            reference_subkind: None,
        }
    }

    fn ctx(nodes: Vec<Node>) -> RouteCtx {
        let mut ctx = RouteCtx::default();
        for node in nodes {
            let file = node.file_path.clone();
            ctx = ctx.routes(&file, vec![node]);
        }
        ctx
    }

    fn pick_class(ctx: &RouteCtx, name: &str, file: &str, dirs: &[&str]) -> Option<String> {
        pick_by_name_and_kind(
            &reference(name, EdgeKind::Calls, file),
            &[NodeKind::Class],
            &|path| dirs.iter().any(|dir| path.contains(dir)),
            ctx,
            &PickOptions::default(),
        )
    }

    #[test]
    fn the_own_file_comes_first_then_the_directory_then_a_preferred_folder() {
        let own = class("Article", "app/views.py");
        let sibling = class("Article", "app/forms.py");
        let deeper = class("Article", "app/sub/models.py");
        let models = class("Article", "src/models/article.py");
        let elsewhere = class("Article", "lib/article.py");
        let all = vec![
            own.clone(),
            sibling.clone(),
            deeper.clone(),
            models.clone(),
            elsewhere.clone(),
        ];
        assert_eq!(
            pick_class(&ctx(all.clone()), "Article", "app/views.py", &["models"]),
            Some(own.id.clone())
        );
        let without_own: Vec<Node> = all.iter().filter(|n| n.id != own.id).cloned().collect();
        assert_eq!(
            pick_class(&ctx(without_own), "Article", "app/views.py", &["models"]),
            Some(sibling.id.clone()),
            "the reference's directory, its direct files only"
        );
        let preferred = vec![deeper.clone(), models.clone(), elsewhere.clone()];
        assert_eq!(
            pick_class(&ctx(preferred), "Article", "app/views.py", &["/models/"]),
            Some(models.id.clone())
        );
        assert_eq!(
            pick_class(
                &ctx(vec![elsewhere.clone()]),
                "Article",
                "app/views.py",
                &["models"]
            ),
            Some(elsewhere.id.clone()),
            "the sole candidate at all"
        );
    }

    #[test]
    fn a_tier_with_two_candidates_proposes_nothing_and_stops() {
        let a = class("Article", "app/a.py");
        let b = class("Article", "app/b.py");
        let models = class("Article", "models/article.py");
        // Two in the reference's directory: no pick, though the preferred
        // folder holds one.
        assert_eq!(
            pick_class(
                &ctx(vec![a, b, models.clone()]),
                "Article",
                "app/views.py",
                &["models"]
            ),
            None
        );
        let m1 = class("Article", "models/a.py");
        let m2 = class("Article", "models/b.py");
        assert_eq!(
            pick_class(&ctx(vec![m1, m2]), "Article", "app/views.py", &["models"]),
            None
        );
        let other = class("Article", "lib/x.py");
        let other2 = class("Article", "lib/y.py");
        assert_eq!(
            pick_class(
                &ctx(vec![other, other2]),
                "Article",
                "app/views.py",
                &["models"]
            ),
            None
        );
    }

    #[test]
    fn same_directory_off_skips_the_directory_tier() {
        let sibling = class("Uri", "src/a.py");
        let preferred = class("Uri", "src/models/uri.py");
        let ctx = ctx(vec![sibling, preferred.clone()]);
        let options = PickOptions {
            same_directory: false,
            accept: None,
        };
        assert_eq!(
            pick_by_name_and_kind(
                &reference("Uri", EdgeKind::Calls, "src/main.py"),
                &[NodeKind::Class],
                &|path| path.contains("/models/"),
                &ctx,
                &options,
            ),
            Some(preferred.id)
        );
    }

    #[test]
    fn accept_and_kind_filter_the_candidates() {
        let class_node = class("router", "api/routes.py");
        let variable = node(NodeKind::Variable, "router", "router", "api/items.py", 3);
        let ctx = ctx(vec![class_node, variable.clone()]);
        let pick = |kinds: &[NodeKind], accept: Option<&dyn Fn(&Node) -> bool>| {
            pick_by_name_and_kind(
                &reference("router", EdgeKind::References, "main.py"),
                kinds,
                &|_| false,
                &ctx,
                &PickOptions {
                    same_directory: true,
                    accept,
                },
            )
        };
        assert_eq!(pick(&[NodeKind::Variable], None), Some(variable.id.clone()));
        assert_eq!(pick(&[NodeKind::Variable], Some(&|_| false)), None);
        assert_eq!(pick(&[NodeKind::Function], None), None);
    }

    #[test]
    fn a_type_nested_in_another_files_type_is_out_of_reach() {
        let owner = class("Outer", "lib/outer.py");
        let nested = node(NodeKind::Class, "Inner", "Outer::Inner", "lib/outer.py", 5);
        let ctx = ctx(vec![owner, nested.clone()]);
        assert_eq!(pick_class(&ctx, "Inner", "app/views.py", &[]), None);
        assert_eq!(
            pick_class(&ctx, "Inner", "lib/outer.py", &[]),
            Some(nested.id),
            "its own file reaches it"
        );
    }

    #[test]
    fn a_value_in_a_function_body_is_out_of_reach() {
        let function = node(NodeKind::Function, "build", "build", "lib/make.py", 1);
        let mut local = node(
            NodeKind::Variable,
            "Local",
            "build::Local",
            "lib/make.py",
            2,
        );
        local.start_line = 2;
        local.end_line = 2;
        let ctx = ctx(vec![function, local]);
        assert_eq!(
            pick_by_name_and_kind(
                &reference("Local", EdgeKind::References, "app/views.py"),
                &[NodeKind::Variable],
                &|_| false,
                &ctx,
                &PickOptions::default(),
            ),
            None
        );
    }

    #[test]
    fn a_test_suite_is_out_of_reach_of_code_that_is_no_test() {
        let suite = class("TestForm", "tests/test_forms.py");
        let ctx = ctx(vec![suite.clone()]);
        assert_eq!(pick_class(&ctx, "TestForm", "app/views.py", &[]), None);
        assert_eq!(
            pick_class(&ctx, "TestForm", "tests/test_views.py", &[]),
            Some(suite.id)
        );
    }

    #[test]
    fn a_pick_in_another_code_family_proposes_nothing_for_calls_too() {
        let mut ts = class("Article", "web/article.ts");
        ts.language = Language::TypeScript;
        let only_ts = ctx(vec![ts]);
        for kind in [EdgeKind::Calls, EdgeKind::References] {
            assert_eq!(
                pick_by_name_and_kind(
                    &reference("Article", kind, "app/views.py"),
                    &[NodeKind::Class],
                    &|_| false,
                    &only_ts,
                    &PickOptions::default(),
                ),
                None,
                "{kind:?}"
            );
        }
        // It still counts in its tier: with a Python twin in the same
        // directory the tier holds two.
        let mut ts_sibling = class("Article", "app/article.ts");
        ts_sibling.language = Language::TypeScript;
        let py_sibling = class("Article", "app/article.py");
        assert_eq!(
            pick_class(
                &ctx(vec![ts_sibling, py_sibling]),
                "Article",
                "app/views.py",
                &[]
            ),
            None
        );
    }

    #[test]
    fn a_cross_family_candidate_holds_its_tier_over_a_lower_one() {
        // A TypeScript `Article` in the reference's directory decides that
        // tier, and is in another family: no pick, though a preferred
        // folder holds a Python one.
        let mut ts = class("Article", "app/article.ts");
        ts.language = Language::TypeScript;
        let models = class("Article", "models/article.py");
        assert_eq!(
            pick_class(
                &ctx(vec![ts, models]),
                "Article",
                "app/views.py",
                &["models"]
            ),
            None
        );
    }

    fn function_in(language: Language, file: &str) -> Node {
        let mut function = node(NodeKind::Function, "get_user", "get_user", file, 1);
        function.language = language;
        function
    }

    fn pick_function(ctx: &RouteCtx) -> Option<String> {
        pick_by_name_and_kind(
            &reference("get_user", EdgeKind::Calls, "app/main.py"),
            &[NodeKind::Function],
            &|path| path.contains("core/"),
            ctx,
            &PickOptions::default(),
        )
    }

    #[test]
    fn a_static_c_function_is_out_of_reach_and_a_plain_one_holds_its_tier() {
        let python = node(
            NodeKind::Function,
            "get_user",
            "get_user",
            "core/users.py",
            1,
        );
        let statics = "static int get_user(void) {\n  return 1;\n}\n";
        let plain = "int get_user(void) {\n  return 1;\n}\n";
        for (language, file) in [
            (Language::C, "app/native.c"),
            (Language::ObjC, "app/native.m"),
        ] {
            let candidates = vec![function_in(language, file), python.clone()];
            // A `static` function is its translation unit's own: the
            // directory tier holds nothing, and the preferred folder's
            // Python one is the pick.
            assert_eq!(
                pick_function(&ctx(candidates.clone()).file(file, statics)),
                Some(python.id.clone()),
                "{file}"
            );
            // A plain one is in reach and decides the directory tier, in
            // another family.
            assert_eq!(
                pick_function(&ctx(candidates).file(file, plain)),
                None,
                "{file}"
            );
        }
    }

    #[test]
    fn a_type_never_inherits_from_itself() {
        let own = class("Base", "app/models.py");
        let other = class("Base", "lib/base.py");
        let ctx = ctx(vec![own.clone(), other.clone()]);
        let mut inherits = reference("Base", EdgeKind::Extends, "app/models.py");
        inherits.from_node_id = own.id.clone();
        assert_eq!(
            pick_by_name_and_kind(
                &inherits,
                &[NodeKind::Class],
                &|_| false,
                &ctx,
                &PickOptions::default(),
            ),
            Some(other.id)
        );
    }
}
