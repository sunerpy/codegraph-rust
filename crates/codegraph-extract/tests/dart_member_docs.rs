//! A Dart member keeps its dartdoc and annotations (upstream #2382 and
//! #2387). tree-sitter-dart wraps every class, mixin and extension member in a
//! `class_member`, a member with no body also in the `declaration` it opens,
//! and an `external` function in an `external_function_declaration`. The
//! member's annotations sit inside that wrapper or open the member's own
//! node, so the dartdoc written above them is the outermost wrapper's previous
//! sibling. A comment between the annotations and the member joins it.
//!
//! The sources are upstream's own test fixtures. Their `const` constructors
//! (`Counter.zero`) are not nodes until W4-DART-03 (#2380), so they are not
//! asserted here.

use codegraph_core::types::{EdgeKind, ExtractionResult, Language};
use codegraph_extract::extract_source;

/// `__tests__/dart-bodiless-member-docs.test.ts` (upstream `1b752de2`).
const BODILESS: &str = r#"abstract class Repository {
  /// Loads the user with [id].
  Future<User> load(String id);

  @visibleForTesting
  void reset();

  /// Calls into the platform.
  external int platformVersion();
}

class User {
  /// Creates a user from its parts.
  User.parts(this.name) : id = 0;

  @visibleForTesting
  User.test(this.name) : id = 1;

  @Deprecated('use parts')
  @internal
  User.old(this.name) : id = 2;

  /// The user's name.
  @deprecated
  final String name;
  User.afterField() : name = '', id = 3;

  final int id;
}
"#;

/// `__tests__/dart-annotated-member-docs.test.ts` (upstream `99246848`).
const ANNOTATED: &str = r#"// Copyright 2026 the authors.
import 'package:meta/meta.dart';

@visibleForTesting
void afterImport() {}

/// Runs at startup.
@pragma('vm:entry-point')
void main() {}

/// Calls into the platform.
@pragma('vm:external-name', 'version')
external int platformVersion();

/// Watches the counter.
@riverpod
@Deprecated('Use the generated provider')
int counter(Ref ref) => 0;

/// The answer.
final answer = 42;

@visibleForTesting
void afterVariable() {}

abstract class Repository {
  /// Loads the user with [id].
  @protected
  Future<User> load(String id);
}

/// A counter.
@immutable
class Counter {
  /// Starts at zero.
  @literal
  const Counter.zero() : value = 0;

  /// Reads a counter from JSON.
  @visibleForTesting
  factory Counter.fromJson(Map<String, Object?> json) => const Counter.zero();

  /// Builds the widget.
  @override
  Widget build(BuildContext context) => const Text('0');

  /// Releases resources.
  ///
  /// Call it once.
  @protected
  @mustCallSuper
  void dispose() {}

  /// The current count.
  @override
  int get count => value;

  /// Use [build] instead.
  @Deprecated(
    'Use build. '
    'Removed in 2.0.',
  )

  Widget render() => const Text('');

  /// Joined with the comment below the annotation.
  @protected
  // ignore: must_call_super
  void joined() {}

  /// The cached value.
  @JsonKey(name: 'value')
  final int value;

  @override
  void afterField() {}

  void before() {}
  @override
  void afterBody() {}
}

/// A mixin.
@internal
mixin Logging on Counter {}

/// An extension.
@internal
extension CounterX on Counter {}

/// An enum.
@JsonEnum()
enum Mode { light, dark }

/// A typedef.
@internal
typedef Callback = void Function();
@visibleForTesting
void afterTypedef() {}
"#;

fn extract(path: &str, source: &str, crlf: bool) -> ExtractionResult {
    let source = if crlf {
        source.replace('\n', "\r\n")
    } else {
        source.to_string()
    };
    extract_source(path, &source, Some(Language::Dart))
}

fn doc(result: &ExtractionResult, qualified_name: &str) -> Option<String> {
    result
        .nodes
        .iter()
        .find(|node| node.qualified_name == qualified_name)
        .unwrap_or_else(|| panic!("no node {qualified_name}"))
        .docstring
        .clone()
}

fn decorators(result: &ExtractionResult, qualified_name: &str) -> Vec<String> {
    let id = &result
        .nodes
        .iter()
        .find(|node| node.qualified_name == qualified_name)
        .unwrap_or_else(|| panic!("no node {qualified_name}"))
        .id;
    let mut names: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Decorates && &r.from_node_id == id)
        .map(|r| r.reference_name.clone())
        .collect();
    names.sort();
    names
}

/// #2382: a member with no body, an abstract or `external` method or a
/// constructor with an initializer list, takes its dartdoc and annotations
/// from before its `declaration`. A field keeps its own: they do not move on
/// to the constructor after it.
#[test]
fn a_member_with_no_body_keeps_its_dartdoc_and_annotations() {
    for crlf in [false, true] {
        let result = extract("lib/user.dart", BODILESS, crlf);
        let doc = |name| doc(&result, name);
        let decorators = |name| decorators(&result, name);
        assert_eq!(
            doc("Repository::load").as_deref(),
            Some("Loads the user with [id].")
        );
        assert_eq!(
            doc("Repository::platformVersion").as_deref(),
            Some("Calls into the platform.")
        );
        assert_eq!(
            doc("User::parts").as_deref(),
            Some("Creates a user from its parts.")
        );
        assert_eq!(decorators("Repository::reset"), ["visibleForTesting"]);
        assert_eq!(decorators("User::test"), ["visibleForTesting"]);
        assert_eq!(decorators("User::old"), ["Deprecated", "internal"]);
        assert_eq!(doc("User::afterField"), None);
        assert!(decorators("User::afterField").is_empty());
    }
}

/// #2387: a member's dartdoc is the one written above its annotations,
/// stacked and multi-line ones included, and a comment between the
/// annotations and the member joins it. Whatever else comes first (an
/// import, a top-level variable, a field, the previous member's body, a
/// typedef) still ends the run. Class-like declarations kept theirs already,
/// and every annotation is still recorded on its member.
#[test]
fn a_member_keeps_the_dartdoc_above_its_annotations() {
    for crlf in [false, true] {
        let result = extract("lib/counter.dart", ANNOTATED, crlf);
        let doc = |name| doc(&result, name);
        let decorators = |name| decorators(&result, name);
        assert_eq!(doc("main").as_deref(), Some("Runs at startup."));
        assert_eq!(
            doc("platformVersion").as_deref(),
            Some("Calls into the platform.")
        );
        assert_eq!(doc("counter").as_deref(), Some("Watches the counter."));

        assert_eq!(doc("Counter::build").as_deref(), Some("Builds the widget."));
        assert_eq!(doc("Counter::count").as_deref(), Some("The current count."));
        assert_eq!(
            doc("Counter::fromJson").as_deref(),
            Some("Reads a counter from JSON.")
        );
        assert_eq!(
            doc("Repository::load").as_deref(),
            Some("Loads the user with [id].")
        );
        assert_eq!(
            doc("Counter::dispose").as_deref(),
            Some("Releases resources.\n\nCall it once.")
        );
        assert_eq!(
            doc("Counter::render").as_deref(),
            Some("Use [build] instead.")
        );
        assert_eq!(
            doc("Counter::joined").as_deref(),
            Some("Joined with the comment below the annotation.\nignore: must_call_super")
        );

        assert_eq!(doc("afterImport"), None);
        assert_eq!(doc("afterVariable"), None);
        assert_eq!(doc("Counter::afterField"), None);
        assert_eq!(doc("Counter::afterBody"), None);
        assert_eq!(doc("afterTypedef"), None);

        assert_eq!(doc("Counter").as_deref(), Some("A counter."));
        assert_eq!(doc("Logging").as_deref(), Some("A mixin."));
        assert_eq!(doc("CounterX").as_deref(), Some("An extension."));
        assert_eq!(doc("Mode").as_deref(), Some("An enum."));
        assert_eq!(doc("Callback").as_deref(), Some("A typedef."));

        assert_eq!(decorators("counter"), ["Deprecated", "riverpod"]);
        assert_eq!(decorators("platformVersion"), ["pragma"]);
        assert_eq!(decorators("Counter::build"), ["override"]);
        assert_eq!(
            decorators("Counter::dispose"),
            ["mustCallSuper", "protected"]
        );
        assert_eq!(decorators("Counter::afterField"), ["override"]);
    }
}
