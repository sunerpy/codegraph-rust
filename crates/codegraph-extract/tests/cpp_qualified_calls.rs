//! A C++ call keeps the scope it is written with (G13): `ns::f()` calls
//! `ns::f`, a static or base-class call `Base::m()` calls `Base::m`, and a
//! call from the global scope `::f()` calls `::f`. Template arguments are
//! dropped. The resolver binds a qualified name by its qualified-name match.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// The callee names `caller` calls in `source`, sorted.
fn calls_from(source: &str, caller: &str) -> Vec<String> {
    let result = extract_source("src/run.cpp", source, Some(Language::Cpp));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let caller_id = &result
        .nodes
        .iter()
        .find(|node| node.qualified_name == caller)
        .unwrap_or_else(|| panic!("no node {caller}"))
        .id;
    let mut names: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls && &r.from_node_id == caller_id)
        .map(|r| r.reference_name.clone())
        .collect();
    names.sort();
    names
}

const SOURCE: &str = r#"namespace ns {
void compute() {}
namespace inner {
void deep() {}
}
}

void global_fn() {}

struct Base {
  static void make();
  void hook() {}
};

struct Derived : Base {
  void hook() { Base::hook(); }
};

template <typename T> struct Box {
  static void make();
};

void run() {
  ns::compute();
  ns::inner::deep();
  ::global_fn();
  ::ns::compute();
  Base::make();
  Box<int>::make();
  std::move(1);
  compute();
}
"#;

#[test]
fn a_qualified_call_keeps_its_scope() {
    assert_eq!(
        calls_from(SOURCE, "run"),
        [
            "::global_fn",
            "::ns::compute",
            "Base::make",
            "Box::make",
            "compute",
            "ns::compute",
            "ns::inner::deep",
            "std::move",
        ]
    );
    assert_eq!(calls_from(SOURCE, "Derived::hook"), ["Base::hook"]);
}
