use std::collections::BTreeSet;

use codegraph_core::types::{EdgeKind, Language, NodeKind};
use codegraph_extract::extract_source;

#[test]
fn scala_companion_object_is_a_module() {
    let source = r#"
object ExtAgreement {
  val Kind = "agreement"
}
trait ExtAgreement {
  def extId: String = "x"
}
"#;

    let result = extract_source("ExtAgreement.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    let companion = result
        .nodes
        .iter()
        .find(|node| node.name == "ExtAgreement" && node.kind == NodeKind::Module);
    assert!(
        companion.is_some(),
        "Scala object must be a module distinct from its companion type; nodes={:#?}",
        result.nodes
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|node| node.name == "ExtAgreement" && node.kind == NodeKind::Trait),
        "companion trait must remain a trait; nodes={:#?}",
        result.nodes
    );
}

#[test]
fn scala_multi_parameter_class_keeps_all_inheritance_targets() {
    let source = r#"
trait ExtAgreement
trait Other
class MAgreement(agreement: String)(implicit qs: Int)
class MExtAgreement(agreement: String, ext: String)(implicit qs: Int)
  extends MAgreement(agreement)(qs) with ExtAgreement with Other {
  def foo: Int = 1
}
"#;

    let result = extract_source("MExtAgreement.scala", source, Some(Language::Scala));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let class = result
        .nodes
        .iter()
        .find(|node| node.name == "MExtAgreement" && node.kind == NodeKind::Class)
        .expect("MExtAgreement class");
    let targets: BTreeSet<_> = result
        .unresolved_references
        .iter()
        .filter(|reference| {
            reference.from_node_id == class.id && reference.reference_kind == EdgeKind::Extends
        })
        .map(|reference| reference.reference_name.as_str())
        .collect();

    assert_eq!(
        targets,
        BTreeSet::from(["ExtAgreement", "MAgreement", "Other"]),
        "all Scala extends/with targets must survive parsing and extraction; refs={:#?}",
        result.unresolved_references
    );
}

#[test]
fn dart_extension_type_owns_its_members() {
    let source = r#"
extension type MetersT(double value) {
  double get km => value / 1000;
  void report() {
    print(km);
  }
}

class Widget {
  double get half => 1.0;
}
"#;

    let result = extract_source("probe.dart", source, Some(Language::Dart));
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(
        result
            .nodes
            .iter()
            .any(|node| node.name == "MetersT" && node.kind == NodeKind::Class),
        "extension type must be indexed as a class-like type; nodes={:#?}",
        result.nodes
    );

    let km = result
        .nodes
        .iter()
        .find(|node| node.name == "km")
        .expect("extension-type getter must be indexed");
    assert_eq!(km.kind, NodeKind::Method);
    assert_eq!(km.qualified_name, "MetersT::km");

    let report = result
        .nodes
        .iter()
        .find(|node| node.name == "report")
        .expect("extension-type method must be indexed");
    assert_eq!(report.kind, NodeKind::Method);
    assert_eq!(report.qualified_name, "MetersT::report");
    assert!(
        report.end_line > report.start_line,
        "method body span was cut"
    );

    assert!(
        result
            .nodes
            .iter()
            .any(|node| node.name == "Widget" && node.kind == NodeKind::Class)
    );
    assert!(
        result
            .nodes
            .iter()
            .any(|node| node.name == "half" && node.kind == NodeKind::Method)
    );
}
