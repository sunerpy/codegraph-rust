//! C++ calls through a local function pointer (upstream #1172,
//! `cppLocalFnPtrs`): a local bound from `&fn` in the enclosing symbol is
//! called as every function it was bound to.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// The `calls` references of `source`, as `(caller, name, line)` in
/// extraction order.
fn calls(file: &str, source: &str, language: Option<Language>) -> Vec<(String, String, i64)> {
    let result = extract_source(file, source, language);
    assert!(result.errors.is_empty(), "errors={:?}", result.errors);
    let name_of = |id: &str| {
        result
            .nodes
            .iter()
            .find(|node| node.id == id)
            .map_or_else(|| id.to_string(), |node| node.name.clone())
    };
    result
        .unresolved_references
        .iter()
        .filter(|reference| reference.reference_kind == EdgeKind::Calls)
        .map(|reference| {
            (
                name_of(&reference.from_node_id),
                reference.reference_name.clone(),
                reference.line,
            )
        })
        .collect()
}

/// Upstream's flash-attention launch template: `auto kernel = &fn<…>`, two
/// branch reassignments and a launch through the local. Each distinct
/// target is called once, at the launch; `kernel` is no callee.
#[test]
fn a_launch_through_a_local_calls_each_bound_kernel_once() {
    let code = concat!(
        "template <typename T, bool Flag>\n",
        "__global__ void fwd_kernel(T* data, int n) {\n",
        "    if (blockIdx.x * blockDim.x + threadIdx.x < n) data[0] += T(1);\n",
        "}\n",
        "\n",
        "template <typename T>\n",
        "__global__ void fwd_splitkv_kernel(T* data, int n) {\n",
        "    if (blockIdx.x * blockDim.x + threadIdx.x < n) data[0] += T(2);\n",
        "}\n",
        "\n",
        "template <typename T>\n",
        "void run_fwd(T* data, int n, cudaStream_t stream) {\n",
        "    auto kernel = &fwd_kernel<T, true>;\n",
        "    if (n % 2 == 0) {\n",
        "        kernel = &fwd_kernel<T, false>;\n",
        "    } else if (n % 3 == 0) {\n",
        "        kernel = &fwd_splitkv_kernel<T>;\n",
        "    }\n",
        "    kernel<<<(n + 255) / 256, 256, 0, stream>>>(data, n);\n",
        "}\n",
    );
    let refs = calls("kernels/launch.cu", code, None);
    let from_run: Vec<(&str, i64)> = refs
        .iter()
        .filter(|(caller, _, _)| caller == "run_fwd")
        .map(|(_, name, line)| (name.as_str(), *line))
        .collect();
    assert_eq!(
        from_run,
        [("fwd_kernel", 19), ("fwd_splitkv_kernel", 19)],
        "{refs:?}"
    );
    assert!(
        !refs.iter().any(|(_, name, _)| name == "kernel"),
        "{refs:?}"
    );
}

/// The assignment alone binds a local whose declarator is no identifier
/// (`void (*k)(int);`), and a target bound twice is called once.
#[test]
fn an_assignment_binds_a_declared_function_pointer() {
    let code = concat!(
        "void f(int x) {}\n",
        "void run(int x) {\n",
        "    void (*k)(int);\n",
        "    k = &f;\n",
        "    k = &f;\n",
        "    k(x);\n",
        "}\n",
    );
    assert_eq!(
        calls("run.cpp", code, None),
        [("run".to_string(), "f".to_string(), 6)]
    );
}

/// Only `&fn` binds: a copy (`auto k = f;`) or a dereference (`*table`)
/// leaves the call through `k` named `k`; a qualified, templated target
/// keeps its scope.
#[test]
fn only_an_address_of_binds_a_local() {
    let code = concat!(
        "namespace ns { template <typename T> void g(T) {} }\n",
        "void f(int x) {}\n",
        "void copy(int x) { auto k = f; k(x); }\n",
        "void deref(void (**table)(int), int x) { auto k = *table; k(x); }\n",
        "void scoped(int x) { auto k = &ns::g<int>; k(x); }\n",
    );
    let refs = calls("bind.cpp", code, None);
    let of = |caller: &str| -> Vec<&str> {
        refs.iter()
            .filter(|(from, _, _)| from == caller)
            .map(|(_, name, _)| name.as_str())
            .collect()
    };
    assert_eq!(of("copy"), ["k"], "{refs:?}");
    assert_eq!(of("deref"), ["k"], "{refs:?}");
    assert_eq!(of("scoped"), ["ns::g"], "{refs:?}");
}

/// A binding belongs to the symbol that made it: another function's call
/// of the same local name is untouched, and C is not read at all.
#[test]
fn a_binding_stays_in_its_symbol_and_in_cpp() {
    let cpp = concat!(
        "void f(void) {}\n",
        "void a(void) { auto k = &f; k(); }\n",
        "void b(void (*k)(void)) { k(); }\n",
    );
    assert_eq!(
        calls("ab.cpp", cpp, None),
        [
            ("a".to_string(), "f".to_string(), 2),
            ("b".to_string(), "k".to_string(), 3),
        ]
    );
    let c = concat!(
        "void f(void) {}\n",
        "void a(void) { void (*k)(void) = &f; k(); }\n",
        "void b(void) { void (*k)(void); k = &f; k(); }\n",
    );
    assert_eq!(
        calls("a.c", c, None),
        [
            ("a".to_string(), "k".to_string(), 2),
            ("b".to_string(), "k".to_string(), 3),
        ]
    );
}
