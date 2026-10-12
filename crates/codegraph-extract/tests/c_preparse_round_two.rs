//! C extraction for upstream's deferral round 2 (#1353, `b9d0f57`): kernel
//! annotations, macro arguments and declaration macros that tree-sitter-c
//! reads as errors are rewritten before parsing, so the declarations around
//! them keep their names and the ones after them are not swallowed.

use codegraph_core::types::{ExtractionResult, Language, NodeKind};
use codegraph_extract::extract_source;

/// `<kind> <qualified name>` for every symbol, file and imports aside.
fn symbols(result: &ExtractionResult) -> Vec<String> {
    result
        .nodes
        .iter()
        .filter(|node| !matches!(node.kind, NodeKind::File | NodeKind::Import))
        .map(|node| format!("{} {}", node.kind.as_str(), node.qualified_name))
        .collect()
}

fn extract(source: &str) -> Vec<String> {
    symbols(&extract_source("src/kernel.c", source, Some(Language::C)))
}

#[track_caller]
fn assert_has(found: &[String], expected: &[&str]) {
    for symbol in expected {
        assert!(
            found.iter().any(|f| f == symbol),
            "{symbol} missing: {found:?}"
        );
    }
}

#[test]
fn a_sandwiched_annotation_keeps_the_function_and_the_next_one() {
    let found = extract(
        "static notrace void tick_do(void) { do_tick(); }\nstatic nokprobe_inline void arm(void) { }\nstatic int after(void) { return 1; }\n",
    );
    assert_has(
        &found,
        &["function tick_do", "function arm", "function after"],
    );
}

#[test]
fn a_c23_auto_local_keeps_its_function() {
    let found =
        extract("void q(void) { auto hb = get_hb(); use(hb); }\nint after(void) { return 2; }\n");
    assert_has(&found, &["function q", "function after"]);
}

#[test]
fn a_parameterized_annotation_is_not_the_function_name() {
    let found = extract(
        "static void __printf(4, 0) log_it(int a, const char *fmt, ...) { }\nint after(void) { return 3; }\n",
    );
    assert_has(&found, &["function log_it", "function after"]);
    assert!(!found.iter().any(|f| f == "function __printf"), "{found:?}");
}

#[test]
fn a_type_keyword_argument_keeps_its_function_and_the_next_one() {
    let found = extract(
        "void j(void *head) {\n\tvoid *p = kzalloc_obj(struct bpf_mount_opts);\n\tuse(p);\n}\nint later(void) { return 0; }\n",
    );
    assert_has(&found, &["function j", "function later"]);
}

#[test]
fn a_prefixed_declaration_macro_swallows_nothing_after_it() {
    let found = extract(
        "static DEFINE_PER_CPU(struct llist_head, rstat_backlog_list);\nint after(void) { return 4; }\n",
    );
    assert_has(&found, &["function after"]);
}

#[test]
fn an_initialized_declaration_macro_declares_its_variable() {
    let found = extract(
        "int bringup_cpu(void) { return 0; }\nstatic DEFINE_PER_CPU(struct cpuhp_cpu_state, cpuhp_state) = {\n\t.startup = bringup_cpu,\n};\nint after(void) { return 5; }\n",
    );
    assert_has(&found, &["function bringup_cpu", "function after"]);
    assert!(
        found.iter().any(|f| f.ends_with(" cpuhp_state")),
        "{found:?}"
    );
}

#[test]
fn a_qualified_va_arg_type_keeps_its_function() {
    let found = extract(
        "void f(va_list ap) {\n\tconst char *s = va_arg(ap, const char *);\n\tuse(s);\n}\nint after(void) { return 6; }\n",
    );
    assert_has(&found, &["function f", "function after"]);
}

#[test]
fn a_named_variadic_define_is_a_macro() {
    let found = extract(
        "#define verbose(env, fmt, args...) log_write(env, fmt, ##args)\nint after(void) { return 7; }\n",
    );
    assert_has(&found, &["constant verbose", "function after"]);
}
