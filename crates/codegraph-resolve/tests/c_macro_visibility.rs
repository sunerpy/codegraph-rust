//! C/C++ macro visibility in translation-unit order (upstream v1.6.1 #1838,
//! #2069, #2070): a call spelled like a function-like macro that is definitely
//! visible at the call site, or that only macros bear, is an expansion and
//! binds nothing; a macro that exists in one build configuration only, sits
//! below the call, or wraps its own name hides nothing; and a macro-named call
//! never binds to a same-named type.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::types::{Edge, EdgeKind, FileRecord, Language, Node, NodeKind};
use codegraph_extract::{detect_language, extract_file};
use codegraph_resolve::{ReferenceResolver, StoreResolutionContext};
use codegraph_store::Store;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct ResolvedGraph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl ResolvedGraph {
    fn function(&self, name: &str) -> &Node {
        self.nodes
            .iter()
            .find(|node| node.kind == NodeKind::Function && node.name == name)
            .unwrap_or_else(|| panic!("no function {name}; nodes={:#?}", self.nodes))
    }

    fn label(&self, id: &str) -> String {
        let node = self
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("missing node {id}"));
        format!(
            "{} {} ({})",
            node.kind.as_str(),
            node.qualified_name,
            node.file_path
        )
    }

    /// `calls` callees of a function, as `kind qualified_name (file)`.
    fn calls(&self, caller: &str) -> Vec<String> {
        let caller = self.function(caller);
        let mut out: Vec<String> = self
            .edges
            .iter()
            .filter(|edge| edge.source == caller.id && edge.kind == EdgeKind::Calls)
            .map(|edge| self.label(&edge.target))
            .collect();
        out.sort();
        out
    }

    /// Every outgoing edge of a function, as `edge_kind kind qualified_name (file)`.
    fn edges_of(&self, caller: &str) -> Vec<String> {
        let caller = self.function(caller);
        let mut out: Vec<String> = self
            .edges
            .iter()
            .filter(|edge| edge.source == caller.id && edge.kind != EdgeKind::Contains)
            .map(|edge| format!("{} {}", edge.kind.as_str(), self.label(&edge.target)))
            .collect();
        out.sort();
        out
    }

    fn callers_of(&self, callee: &Node) -> usize {
        self.edges
            .iter()
            .filter(|edge| edge.target == callee.id && edge.kind == EdgeKind::Calls)
            .count()
    }
}

fn temp_path(slug: &str) -> PathBuf {
    let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "codegraph-c-macro-{slug}-{}-{nonce}",
        std::process::id()
    ))
}

fn collect_files(root: &Path, dir: &Path, output: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read fixture directory") {
        let path = entry.expect("fixture entry").path();
        if path.is_dir() {
            collect_files(root, &path, output);
        } else {
            output.push(
                path.strip_prefix(root)
                    .expect("fixture path under root")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

fn resolve_project(slug: &str, files: &[(String, String)]) -> ResolvedGraph {
    let root = temp_path(slug);
    std::fs::create_dir_all(&root).expect("create fixture root");
    for (relative, source) in files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, source).expect("write fixture source");
    }
    let mut relative = Vec::new();
    collect_files(&root, &root, &mut relative);
    relative.sort();
    let db = temp_path(&format!("{slug}-db")).with_extension("db");
    let mut store = Store::open(&db).expect("open fixture store");
    for path in &relative {
        let result = extract_file(&root, path).expect("extract fixture");
        let language = detect_language(path);
        assert_ne!(language, Language::Unknown, "unknown fixture: {path}");
        store
            .upsert_file(&FileRecord {
                path: path.clone(),
                content_hash: "fixture".to_string(),
                language,
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: result.nodes.len() as i64,
                errors: result.errors,
                generated: false,
            })
            .expect("insert file");
        store.upsert_nodes(&result.nodes).expect("insert nodes");
        store.insert_edges(&result.edges).expect("insert edges");
        store
            .insert_unresolved_refs(&result.unresolved_references)
            .expect("insert references");
    }
    let root_string = root.to_string_lossy().to_string();
    let mut resolver = ReferenceResolver::new(&root_string);
    {
        let context = StoreResolutionContext::new(&store, &root_string);
        resolver.initialize(&context);
    }
    resolver
        .resolve_and_persist(&mut store)
        .expect("resolve fixture");
    let nodes = store.all_nodes().expect("read nodes");
    let edges = store.all_edges().expect("read edges");
    drop(store);
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_dir_all(root);
    ResolvedGraph { nodes, edges }
}

fn files(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(path, source)| (path.to_string(), source.to_string()))
        .collect()
}

#[test]
fn a_macro_from_an_included_header_does_not_bind_to_the_decoy() {
    let graph = resolve_project(
        "reproduction",
        &files(&[
            ("marker.hpp", "#define TRACE_POINT(value) ((void)(value))\n"),
            (
                "exercise.cpp",
                "#include \"marker.hpp\"\n\nvoid exercise() {\n    TRACE_POINT(1);\n}\n",
            ),
            ("macro_decoy.cpp", "void TRACE_POINT(int value) {}\n"),
        ]),
    );
    assert_eq!(graph.calls("exercise"), Vec::<String>::new());
    assert_eq!(graph.callers_of(graph.function("TRACE_POINT")), 0);
}

#[test]
fn nested_includes_sibling_headers_local_defines_and_undef_decide_visibility() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "nested",
            &[
                (
                    "src/pg/pinio.h".to_string(),
                    "#define HEADER_TRACE(v) ((void)(v))\n".to_string(),
                ),
                (
                    "src/drivers/pinio.h".to_string(),
                    "// unrelated header with the same basename\n".to_string(),
                ),
                (
                    format!("src/pg/pinio.{language}"),
                    "#include \"pinio.h\"\nvoid sibling_header_use() { HEADER_TRACE(1); }\n"
                        .to_string(),
                ),
                (
                    "inner.h".to_string(),
                    "#define TRACE_POINT(value) ((void)(value))\n".to_string(),
                ),
                ("outer.h".to_string(), "#include \"inner.h\"\n".to_string()),
                (
                    format!("exercise.{language}"),
                    [
                        "#include \"outer.h\"",
                        "void macro_use() { TRACE_POINT(1); }",
                        "void local_macro() {",
                        "#define INNER_TRACE(v) ((void)(v))",
                        "INNER_TRACE(1);",
                        "}",
                        "#undef TRACE_POINT",
                        "void after_undef() { TRACE_POINT(1); }",
                        "",
                    ]
                    .join("\n"),
                ),
                (
                    format!("decoy.{language}"),
                    [
                        "void HEADER_TRACE(int value) {}",
                        "void INNER_TRACE(int value) {}",
                        "void TRACE_POINT(int value) {}",
                        "void unrelated_use() { TRACE_POINT(1); }",
                        "",
                    ]
                    .join("\n"),
                ),
            ],
        );
        let real = vec![format!("function TRACE_POINT (decoy.{language})")];
        assert_eq!(graph.calls("macro_use"), Vec::<String>::new(), "{language}");
        assert_eq!(
            graph.calls("sibling_header_use"),
            Vec::<String>::new(),
            "{language}"
        );
        assert_eq!(
            graph.calls("local_macro"),
            Vec::<String>::new(),
            "{language}"
        );
        // After `#undef`, and in a file that never sees the macro, the call is real.
        assert_eq!(graph.calls("after_undef"), real, "{language}");
        assert_eq!(graph.calls("unrelated_use"), real, "{language}");
    }
}

#[test]
fn unguarded_includes_replay_while_guards_once_and_changed_flags_are_respected() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "replay",
            &[
                ("unguarded.h".to_string(), "#define TRACE(v) ((void)(v))\n".to_string()),
                (
                    "guarded.h".to_string(),
                    "#ifndef GUARDED_H\n#define GUARDED_H\n#define GUARDED(v) ((void)(v))\n#endif\n"
                        .to_string(),
                ),
                ("once.h".to_string(), "#pragma once\n#define ONCE(v) ((void)(v))\n".to_string()),
                (
                    "conditional.h".to_string(),
                    "#if ENABLE_TRACE\n#define CONDITIONAL(v) ((void)(v))\n#endif\n".to_string(),
                ),
                (
                    format!("unit.{language}"),
                    [
                        "#include \"unguarded.h\"",
                        "#undef TRACE",
                        "void between() { TRACE(1); }",
                        "#include \"unguarded.h\"",
                        "void repeated() { TRACE(1); }",
                        "#include \"guarded.h\"",
                        "#undef GUARDED",
                        "#include \"guarded.h\"",
                        "void guarded() { GUARDED(1); }",
                        "#undef GUARDED_H",
                        "#include \"guarded.h\"",
                        "void reset_guard() { GUARDED(1); }",
                        "#include \"once.h\"",
                        "#undef ONCE",
                        "#include \"once.h\"",
                        "void once() { ONCE(1); }",
                        "#define ENABLE_TRACE 0",
                        "#include \"conditional.h\"",
                        "#undef ENABLE_TRACE",
                        "#define ENABLE_TRACE 1",
                        "#include \"conditional.h\"",
                        "void changed_flag() { CONDITIONAL(1); }",
                        "",
                    ]
                    .join("\n"),
                ),
                (
                    format!("decoy.{language}"),
                    "void TRACE(int x) {}\nvoid GUARDED(int x) {}\nvoid ONCE(int x) {}\nvoid CONDITIONAL(int x) {}\n"
                        .to_string(),
                ),
            ],
        );
        let real = |name: &str| vec![format!("function {name} (decoy.{language})")];
        assert_eq!(graph.calls("between"), real("TRACE"), "{language}");
        assert_eq!(graph.calls("repeated"), Vec::<String>::new(), "{language}");
        assert_eq!(graph.calls("guarded"), real("GUARDED"), "{language}");
        assert_eq!(
            graph.calls("reset_guard"),
            Vec::<String>::new(),
            "{language}"
        );
        assert_eq!(graph.calls("once"), real("ONCE"), "{language}");
        assert_eq!(
            graph.calls("changed_flag"),
            Vec::<String>::new(),
            "{language}"
        );
    }
}

#[test]
fn a_diamond_cyclic_include_graph_finds_the_macro_on_an_unconditional_path() {
    // top.h includes left.h and right.h; both include shared.h (no guard), which
    // includes top.h again. The active include stack breaks the cycle while
    // shared.h is replayed on the unconditional path.
    let graph = resolve_project(
        "diamond",
        &files(&[
            ("top.h", "#include \"left.h\"\n#include \"right.h\"\n"),
            ("left.h", "#ifdef USE_LEFT\n#include \"shared.h\"\n#endif\n"),
            ("right.h", "#include \"shared.h\"\n"),
            (
                "shared.h",
                "#include \"top.h\"\n#define SHARED_TRACE(v) ((void)(v))\n",
            ),
            (
                "unit.cpp",
                "#include \"top.h\"\nvoid unit() { SHARED_TRACE(1); }\n",
            ),
            ("decoy.cpp", "void SHARED_TRACE(int value) {}\n"),
        ]),
    );
    assert_eq!(graph.calls("unit"), Vec::<String>::new());
}

#[test]
fn a_define_in_a_comment_string_or_raw_literal_is_no_macro() {
    let graph = resolve_project(
        "masked",
        &files(&[
            (
                "doc.hpp",
                "/*\n * Example:\n * #define helper(x) ((x) + 1)\n */\nconst char *usage = \"/* #define helper(x) */\";\nconst char *raw = R\"doc(\n#define helper(x) ((x) + 9)\n)doc\";\n",
            ),
            (
                "lib.cpp",
                "#include \"doc.hpp\"\nint helper(int x) { return x + 1; }\nint run() { return helper(1); }\n",
            ),
            ("other.cpp", "#define helper(x) ((x) + 2)\n"),
        ]),
    );
    assert_eq!(graph.calls("run"), vec!["function helper (lib.cpp)"]);
}

#[test]
fn a_wrapper_macro_calling_its_own_name_keeps_the_call() {
    let graph = resolve_project(
        "wrapper",
        &files(&[(
            "vec.c",
            "static void vec_splice(char **data, int start, int count) {}\n\
             #define vec_splice(v, start, count)\\\n\
             \x20 ( vec_splice((char **)(v), start, count),\\\n\
             \x20   (v)->length -= (count) )\n\
             struct buf { char *data; int length; };\n\
             void flush(struct buf *b) { vec_splice(b, 0, 1); }\n",
        )]),
    );
    assert_eq!(graph.calls("flush"), vec!["function vec_splice (vec.c)"]);
}

#[test]
fn a_macro_only_in_an_unrelated_file_neither_suppresses_nor_receives_a_call() {
    let graph = resolve_project(
        "unrelated",
        &files(&[
            ("unrelated.hpp", "#define helper(x) ((x) + 1)\n"),
            ("lib.cpp", "int helper(int x) { return x + 1; }\n"),
            (
                "main.cpp",
                "int helper(int x);\nint run() { return helper(1); }\n",
            ),
        ]),
    );
    assert_eq!(graph.calls("run"), vec!["function helper (lib.cpp)"]);
}

#[test]
fn a_call_above_the_define_or_under_a_false_branch_is_still_a_call() {
    let graph = resolve_project(
        "ordered",
        &files(&[
            (
                "maths.h",
                "#define FAST_MATH\n#if defined(FAST_MATH)\nfloat sin_approx(float x);\n#else\n#define sin_approx(x) external_sin(x)\n#endif\n",
            ),
            (
                "maths.cpp",
                "#include \"maths.h\"\nfloat sin_approx(float x) { return x; }\nvoid real_fn(int value) {}\nfloat invoke_real(float value) { real_fn(1); return sin_approx(value); }\n#define real_fn(x) ((void)(x))\n",
            ),
        ]),
    );
    assert_eq!(
        graph.calls("invoke_real"),
        vec![
            "function real_fn (maths.cpp)",
            "function sin_approx (maths.cpp)"
        ]
    );
}

#[test]
fn a_name_only_macros_bear_binds_nothing_even_by_a_loose_match() {
    // The macro is not visible from `run` at all, but no function bears its
    // exact name: a case-insensitive match (`SWAP` -> `swap`) must not invent one.
    let graph = resolve_project(
        "only-macros",
        &files(&[
            (
                "swap.h",
                "#define SWAP(a, b) do { int t = a; a = b; b = t; } while (0)\n",
            ),
            ("lib.c", "void swap(int *a, int *b) {}\n"),
            ("main.c", "void run(int a, int b) { SWAP(a, b); }\n"),
        ]),
    );
    assert_eq!(graph.calls("run"), Vec::<String>::new());
}

#[test]
fn an_undef_under_an_undecidable_if_leaves_a_never_seen_flag_unknown() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "undef-unknown",
            &[
                (
                    "config.h".to_string(),
                    "#if FREE_THREADED == 0\n#undef FREE_THREADED\n#endif\n".to_string(),
                ),
                (
                    format!("unit.{language}"),
                    [
                        "#include \"config.h\"",
                        "#ifdef FREE_THREADED",
                        "static int world_stopped(void) { return 0; }",
                        "#else",
                        "#define world_stopped() 1",
                        "#endif",
                        "int check(void) { return world_stopped(); }",
                        "",
                    ]
                    .join("\n"),
                ),
            ],
        );
        assert_eq!(
            graph.calls("check"),
            vec![format!("function world_stopped (unit.{language})")],
            "{language}"
        );
    }
}

#[test]
fn an_undef_on_a_certain_line_still_makes_the_flag_undefined() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "undef-certain",
            &[
                (
                    "config.h".to_string(),
                    "#undef TOP_LEVEL\n#if 1\n#undef TRUE_BRANCH\n#endif\n".to_string(),
                ),
                (
                    format!("unit.{language}"),
                    [
                        "#include \"config.h\"",
                        "#ifdef TOP_LEVEL",
                        "#else",
                        "#define top_hook() 1",
                        "#endif",
                        "#ifdef TRUE_BRANCH",
                        "#else",
                        "#define branch_hook() 1",
                        "#endif",
                        "int use_top(void) { return top_hook(); }",
                        "int use_branch(void) { return branch_hook(); }",
                        "",
                    ]
                    .join("\n"),
                ),
                (
                    format!("decoy.{language}"),
                    "int top_hook(void) { return 0; }\nint branch_hook(void) { return 0; }\n"
                        .to_string(),
                ),
            ],
        );
        assert_eq!(graph.calls("use_top"), Vec::<String>::new(), "{language}");
        assert_eq!(
            graph.calls("use_branch"),
            Vec::<String>::new(),
            "{language}"
        );
    }
}

#[test]
fn a_macro_named_call_never_binds_to_a_same_named_type() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "macro-type",
            &[
                (
                    format!("unit.{language}"),
                    "#ifdef TOK_IMPL\n#define PREFIX(ident) ident\n#endif\nint tok(int x) { return PREFIX(scan)(x); }\n"
                        .to_string(),
                ),
                (format!("parser.{language}"), "struct PREFIX { int x; };\n".to_string()),
                ("types.h".to_string(), "struct WRAP { int x; };\n".to_string()),
                (
                    format!("wrapped.{language}"),
                    "#include \"types.h\"\n#ifdef WRAP_IMPL\n#define WRAP(ident) ident\n#endif\nint wrapped(int x) { return WRAP(scan)(x); }\n"
                        .to_string(),
                ),
            ],
        );
        assert!(
            !graph
                .edges_of("tok")
                .iter()
                .any(|edge| edge.contains("struct PREFIX")),
            "{language}: {:?}",
            graph.edges_of("tok")
        );
        assert!(
            !graph
                .edges_of("wrapped")
                .iter()
                .any(|edge| edge.contains("struct WRAP")),
            "{language}: {:?}",
            graph.edges_of("wrapped")
        );
    }
}

#[test]
fn a_same_named_function_is_still_the_callee_when_the_macro_may_be_off() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "macro-off",
            &[
                (
                    format!("unit.{language}"),
                    "#ifdef FAST\n#define helper(x) (x)\n#endif\nint run(int x) { return helper(x); }\n"
                        .to_string(),
                ),
                (format!("lib.{language}"), "int helper(int x) { return x; }\n".to_string()),
                (format!("types.{language}"), "struct helper { int x; };\n".to_string()),
            ],
        );
        assert_eq!(
            graph.edges_of("run"),
            vec![format!("calls function helper (lib.{language})")],
            "{language}"
        );
    }
}

#[test]
fn a_cpp_type_whose_name_is_no_macro_is_still_constructed() {
    let graph = resolve_project(
        "type-not-macro",
        &files(&[
            ("widget.hpp", "struct Widget { int x; };\n"),
            (
                "use.cpp",
                "#include \"widget.hpp\"\nvoid make() { auto w = Widget(1); }\n",
            ),
            ("other.cpp", "#define OTHER(x) (x)\n"),
        ]),
    );
    assert_eq!(
        graph.edges_of("make"),
        vec!["instantiates struct Widget (widget.hpp)"]
    );
}

#[test]
fn compound_conditions_decide_macro_visibility() {
    for language in ["c", "cpp"] {
        let graph = resolve_project(
            "compound",
            &[
                (
                    format!("unit.{language}"),
                    [
                        "#if 1 || FLAG",
                        "#define HOOK_A(x) ((void)(x))",
                        "#endif",
                        "#if 0 && FLAG",
                        "#define HOOK_B(x) ((void)(x))",
                        "#endif",
                        "#define A",
                        "#if defined(A) || defined(B)",
                        "#define HOOK_C(x) ((void)(x))",
                        "#endif",
                        "#if FLAG == 0",
                        "#define HOOK_D(x) ((void)(x))",
                        "#endif",
                        "#if 1 || \\",
                        "    FLAG /* continued */",
                        "#define HOOK_E(x) ((void)(x))",
                        "#endif",
                        "#if 0",
                        "#elif 1 || \\",
                        "      FLAG",
                        "#define HOOK_F(x) ((void)(x))",
                        "#endif",
                        "void use_a(void) { HOOK_A(1); }",
                        "void use_b(void) { HOOK_B(1); }",
                        "void use_c(void) { HOOK_C(1); }",
                        "void use_d(void) { HOOK_D(1); }",
                        "void use_e(void) { HOOK_E(1); }",
                        "void use_f(void) { HOOK_F(1); }",
                        "",
                    ]
                    .join("\n"),
                ),
                (
                    format!("decoy.{language}"),
                    ["A", "B", "C", "D", "E", "F"]
                        .map(|hook| format!("void HOOK_{hook}(int x) {{}}\n"))
                        .concat(),
                ),
            ],
        );
        let calls = ["a", "b", "c", "d", "e", "f"]
            .map(|user| (user, graph.calls(&format!("use_{user}"))))
            .to_vec();
        let real = |hook: &str| vec![format!("function HOOK_{hook} (decoy.{language})")];
        // Definitely true, through `||`, `defined` or a continued line: the
        // macro expands. Definitely false: the function is called. Unknown: the
        // call keeps its function.
        assert_eq!(
            calls,
            vec![
                ("a", Vec::<String>::new()),
                ("b", real("B")),
                ("c", Vec::new()),
                ("d", real("D")),
                ("e", Vec::new()),
                ("f", Vec::new()),
            ],
            "{language}"
        );
    }
}
