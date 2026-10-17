//! A Dart annotation's name (upstream #2390): a dotted one (`@meta.immutable`,
//! `@Riverpod.scoped`, `@p.Foo.named(…)`) is recorded by its last name,
//! `@Foo<int>.named(…)` by the name its type arguments follow. KEEP-RUST:
//! this grammar writes a dotted name as a `qualified` node. An annotation
//! belongs to the declaration that follows it, whatever comments come
//! between them (upstream's comment fixture): ALREADY-HAVE, as this grammar
//! writes a declaration's annotations and comments inside it.

use codegraph_core::types::{EdgeKind, Language};
use codegraph_extract::extract_source;

/// The annotation names recorded on the node `qualified`, sorted.
fn decorators(source: &str, qualified: &str) -> Vec<String> {
    let result = extract_source("lib/counter.dart", source, Some(Language::Dart));
    let node = result
        .nodes
        .iter()
        .find(|n| n.qualified_name == qualified)
        .unwrap_or_else(|| panic!("{qualified}"));
    let mut names: Vec<String> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Decorates && r.from_node_id == node.id)
        .map(|r| r.reference_name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn a_dotted_annotation_is_recorded_by_its_last_name() {
    let source = "import 'package:meta/meta.dart' as meta;\n\n@meta.immutable\nclass C {}\n\n@Riverpod.scoped\nint s() => 0;\n\n@Riverpod.forever()\nint k() => 0;\n\n@meta.Foo.named(1)\nint p() => 0;\n\n@Foo<int>.named(1)\nint e() => 0;\n\nclass M {\n  @Riverpod.scoped\n  void g() {}\n}\n";
    assert_eq!(decorators(source, "C"), ["immutable"]);
    assert_eq!(decorators(source, "s"), ["scoped"]);
    assert_eq!(decorators(source, "k"), ["forever"]);
    assert_eq!(decorators(source, "p"), ["named"]);
    assert_eq!(decorators(source, "e"), ["Foo"]);
    assert_eq!(decorators(source, "M::g"), ["scoped"]);
}

/// Upstream's `dart-annotation-comments.test.ts` source. KEEP-RUST: stacked
/// annotations are recorded in source order (upstream's in reverse); the
/// graph keeps no order among a node's references, so they are compared
/// sorted.
const SOURCE: &str = "import 'package:meta/meta.dart';\n// After the import.\n@visibleForTesting\n// Below the annotation.\nvoid afterImport() {}\n\n@override // On the annotation's line.\nvoid trailing() {}\n\n@riverpod\n// expect_lint: functional_ref\nint counter(Ref ref) => 0;\n\n@Deprecated('x')\n/// Doc below the annotation.\n/* And a block comment. */\nvoid docBelow() {}\n\n@a\n// one\n@b\n/** two */\n@c\nvoid stacked() {}\n\n@x\nfinal answer = 42;\n// After a variable.\nvoid afterVariable() {}\n\nclass Counter {\n  @override\n  // ignore: must_call_super\n  void dispose() {}\n\n  @protected\n  // A comment.\n  void bodiless();\n\n  @literal\n  // A comment.\n  const Counter.zero();\n\n  @x /* inline */ void inline() {}\n\n  @JsonKey(name: 'value')\n  final int value = 0;\n  // After a field.\n  void afterField() {}\n\n  void before() {} // Trailing the previous member.\n  @override\n  void afterBody() {}\n}\n";

#[test]
fn an_annotation_behind_a_comment_still_decorates_its_declaration() {
    for source in [SOURCE.to_string(), SOURCE.replace('\n', "\r\n")] {
        assert_eq!(decorators(&source, "afterImport"), ["visibleForTesting"]);
        assert_eq!(decorators(&source, "trailing"), ["override"]);
        assert_eq!(decorators(&source, "counter"), ["riverpod"]);
        assert_eq!(decorators(&source, "docBelow"), ["Deprecated"]);
        assert_eq!(decorators(&source, "stacked"), ["a", "b", "c"]);
        assert_eq!(decorators(&source, "Counter::dispose"), ["override"]);
        assert_eq!(decorators(&source, "Counter::bodiless"), ["protected"]);
        assert_eq!(decorators(&source, "Counter::zero"), ["literal"]);
        assert_eq!(decorators(&source, "Counter::inline"), ["x"]);
        assert_eq!(decorators(&source, "Counter::afterBody"), ["override"]);
        assert_eq!(decorators(&source, "afterVariable"), Vec::<String>::new());
        assert_eq!(
            decorators(&source, "Counter::afterField"),
            Vec::<String>::new()
        );
    }
}
