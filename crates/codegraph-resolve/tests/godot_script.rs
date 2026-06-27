//! L3 Godot static-analysis tests: `.gd` GDScript dynamic call-site recognition
//! via [`GodotResolver::extract`] (T6 of godot-static-analysis).
//!
//! These exercise the public [`FrameworkResolver::extract`] dispatch — the
//! resolver-pipeline entry point — so the assertions pin the observable
//! extraction shape, not internals. T6 adds the `.gd` branch: each dynamic
//! dispatch call-site emits one reference FROM the enclosing function/file.
//! Literal targets become a normal reference (by NAME); computed/non-literal
//! targets become a dynamic-unresolved sentinel reference (prefix
//! `godot:dynamic:`) so T8 can surface them as "dynamic, unconfirmable".

use codegraph_core::types::EdgeKind;
use codegraph_resolve::framework::FrameworkResolver;
use codegraph_resolve::frameworks::godot::GodotResolver;
use codegraph_resolve::frameworks::godot_script::DYNAMIC_PREFIX;
use codegraph_resolve::types::{FrameworkResolverExtractionResult, RefView};

/// Run `extract()` and unwrap the result (panics if the resolver returned
/// `None`, which for a `.gd` is itself a failure).
fn extract(path: &str, content: &str) -> FrameworkResolverExtractionResult {
    GodotResolver
        .extract(path, content, "")
        .expect(".gd must produce Some(result)")
}

/// Find a reference by exact reference_name.
fn find<'a>(result: &'a FrameworkResolverExtractionResult, name: &str) -> Option<&'a RefView> {
    result.references.iter().find(|r| r.reference_name == name)
}

#[test]
fn signal_connect_emits_reference_to_handler_method() {
    // Given a _ready() that wires a timer's timeout to a handler method,
    let content = "\
func _ready():
\ttimer.timeout.connect(_on_timeout)
";
    // When extracting,
    let result = extract("player.gd", content);

    // Then a reference to the handler method name `_on_timeout` is emitted.
    let r = find(&result, "_on_timeout").expect("ref to _on_timeout handler");
    assert_eq!(
        r.reference_kind,
        EdgeKind::Calls,
        "a connected handler is a deferred call"
    );
}

#[test]
fn emit_signal_string_emits_reference_to_signal_name() {
    // Given an emit_signal with a string-literal signal name,
    let content = "\
func hurt():
\temit_signal(\"health_changed\")
";
    // When extracting,
    let result = extract("entity.gd", content);

    // Then a reference to the signal name `health_changed` is emitted.
    let r = find(&result, "health_changed").expect("ref to health_changed signal");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn signal_dot_emit_emits_reference_to_signal_name() {
    // Given the Godot 4 `signal.emit()` object syntax,
    let content = "\
func hurt():
\thealth_changed.emit()
";
    // When extracting,
    let result = extract("entity.gd", content);

    // Then a reference to the signal name is emitted.
    let r = find(&result, "health_changed").expect("ref to health_changed via .emit()");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn get_node_string_emits_reference_to_node_path() {
    // Given get_node with a string-literal path,
    let content = "\
func _ready():
\tvar s = get_node(\"Player/Sprite\")
";
    // When extracting,
    let result = extract("main.gd", content);

    // Then a reference to the node path is emitted.
    let r = find(&result, "Player/Sprite").expect("ref to Player/Sprite path");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn dollar_node_path_emits_reference() {
    // Given the `$NodePath` shorthand,
    let content = "\
func _ready():
\t$Player/Sprite.show()
";
    // When extracting,
    let result = extract("main.gd", content);

    // Then a reference to the dollar path is emitted.
    let r = find(&result, "Player/Sprite").expect("ref to $Player/Sprite");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn dollar_quoted_node_path_emits_reference() {
    // Given the `$"Quoted/Path"` shorthand,
    let content = "\
func _ready():
\tvar n = $\"Player/Sprite\"
";
    // When extracting,
    let result = extract("main.gd", content);

    // Then a reference to the quoted path is emitted.
    let r = find(&result, "Player/Sprite").expect("ref to $\"Player/Sprite\"");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn unique_node_emits_reference_to_unique_name() {
    // Given the `%UniqueName` shorthand,
    let content = "\
func _ready():
\t%Health.set_value(10)
";
    // When extracting,
    let result = extract("ui.gd", content);

    // Then a reference to the unique name `Health` is emitted.
    let r = find(&result, "Health").expect("ref to %Health unique node");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn modulo_operator_is_not_mistaken_for_unique_node() {
    // Given a modulo expression (the `%` operator with a numeric operand),
    let content = "\
func wrap(i):
\treturn i % 4
";
    // When extracting,
    let result = extract("math.gd", content);

    // Then NO unique-node reference is fabricated from the `%`.
    assert!(
        result.references.is_empty(),
        "modulo `%` must not emit a unique-node ref, got {:?}",
        result.references
    );
}

#[test]
fn get_nodes_in_group_emits_reference_to_group_name() {
    // Given get_tree().get_nodes_in_group with a string-literal group,
    let content = "\
func count_enemies():
\treturn get_tree().get_nodes_in_group(\"enemies\").size()
";
    // When extracting,
    let result = extract("game.gd", content);

    // Then a reference to the group name `enemies` is emitted.
    let r = find(&result, "enemies").expect("ref to enemies group");
    assert_eq!(r.reference_kind, EdgeKind::References);
}

#[test]
fn add_to_group_emits_reference_to_group_name() {
    // Given add_to_group with a string-literal group,
    let content = "\
func _ready():
\tadd_to_group(\"hostiles\")
";
    // When extracting,
    let result = extract("enemy.gd", content);

    // Then a reference to the group name is emitted.
    assert!(find(&result, "hostiles").is_some(), "ref to hostiles group");
}

#[test]
fn has_method_emits_reference_to_method_name() {
    // Given has_method with a string-literal method name,
    let content = "\
func apply_to(target):
\tif target.has_method(\"apply\"):
\t\tpass
";
    // When extracting,
    let result = extract("buff.gd", content);

    // Then a reference to the method name `apply` is emitted, as a Call.
    let r = find(&result, "apply").expect("ref to apply method");
    assert_eq!(
        r.reference_kind,
        EdgeKind::Calls,
        "dynamic method dispatch is a Call"
    );
}

#[test]
fn call_string_emits_reference_to_method_name() {
    // Given a dynamic call() with a string-literal method name,
    let content = "\
func run(target):
\ttarget.call(\"apply\")
";
    // When extracting,
    let result = extract("runner.gd", content);

    // Then a reference to the method name is emitted, as a Call.
    let r = find(&result, "apply").expect("ref to apply via call()");
    assert_eq!(r.reference_kind, EdgeKind::Calls);
}

#[test]
fn computed_get_node_emits_dynamic_unresolved_reference_not_a_resolved_edge() {
    // Given get_node with a NON-literal (variable) argument,
    let content = "\
func fetch(var_path):
\treturn get_node(var_path)
";
    // When extracting,
    let result = extract("loader.gd", content);

    // Then NO normal node-path reference is fabricated (the target is unknown):
    // the only reference is a dynamic-unresolved sentinel, flagged so T8 can
    // categorize it as "dynamic, unconfirmable".
    assert_eq!(
        result.references.len(),
        1,
        "exactly one dynamic ref, got {:?}",
        result.references
    );
    let r = &result.references[0];
    assert!(
        r.reference_name.starts_with(DYNAMIC_PREFIX),
        "computed get_node must be flagged dynamic (prefix {DYNAMIC_PREFIX}), got {:?}",
        r.reference_name
    );
    assert_eq!(
        r.reference_name,
        format!("{DYNAMIC_PREFIX}get_node"),
        "sentinel encodes the call kind"
    );
    // And it is NOT mistaken for a literal node path like `var_path`.
    assert!(
        find(&result, "var_path").is_none(),
        "the variable name must not become a resolved node-path ref"
    );
}

#[test]
fn computed_call_emits_dynamic_unresolved_reference() {
    // Given call() with a non-literal (variable) method argument,
    let content = "\
func run(method_var):
\ttarget.call(method_var)
";
    // When extracting,
    let result = extract("runner.gd", content);

    // Then a dynamic-unresolved sentinel reference is emitted (not a resolved
    // method ref), preserving the call kind in the name.
    let r = result
        .references
        .iter()
        .find(|r| r.reference_name.starts_with(DYNAMIC_PREFIX))
        .expect("a dynamic sentinel ref for computed call()");
    assert_eq!(r.reference_name, format!("{DYNAMIC_PREFIX}call"));
    assert_eq!(r.reference_kind, EdgeKind::Calls);
    assert!(
        find(&result, "method_var").is_none(),
        "the variable must not become a resolved method ref"
    );
}

#[test]
fn computed_emit_signal_emits_dynamic_unresolved_reference() {
    // Given emit_signal with a non-literal (variable) signal argument,
    let content = "\
func relay(sig_var):
\temit_signal(sig_var)
";
    // When extracting,
    let result = extract("relay.gd", content);

    // Then a dynamic-unresolved sentinel is emitted, not a resolved signal ref.
    let r = result
        .references
        .iter()
        .find(|r| r.reference_name.starts_with(DYNAMIC_PREFIX))
        .expect("a dynamic sentinel ref for computed emit_signal");
    assert_eq!(r.reference_name, format!("{DYNAMIC_PREFIX}emit_signal"));
}

#[test]
fn reference_originates_from_enclosing_function() {
    // Given two functions each with one dynamic call-site,
    let content = "\
func a():
\temit_signal(\"sig_a\")

func b():
\temit_signal(\"sig_b\")
";
    // When extracting,
    let result = extract("two.gd", content);

    // Then each reference originates from a DIFFERENT (function) source node —
    // they are attributed to their enclosing function, not lumped together.
    let ra = find(&result, "sig_a").expect("ref sig_a");
    let rb = find(&result, "sig_b").expect("ref sig_b");
    assert_ne!(
        ra.from_node_id, rb.from_node_id,
        "refs in different functions must have different source nodes"
    );
    // And neither is the file node (they are inside functions).
    assert_ne!(ra.from_node_id, "file:two.gd");
    assert_ne!(rb.from_node_id, "file:two.gd");
}

#[test]
fn top_level_call_site_attributes_to_file_node() {
    // Given a dynamic call-site before any `func` (a field initializer),
    let content = "extends Node\nvar n = get_node(\"Boot\")\n";
    // When extracting,
    let result = extract("boot.gd", content);

    // Then its reference originates from the file node.
    let r = find(&result, "Boot").expect("ref to Boot");
    assert_eq!(
        r.from_node_id, "file:boot.gd",
        "a top-level call-site attributes to the file node"
    );
}

#[test]
fn gd_with_no_dynamic_patterns_yields_empty_references() {
    // Given a plain `.gd` with no dynamic dispatch call-sites,
    let content = "\
extends Node

func add(a, b):
\treturn a + b
";
    // When extracting (must return Some — the .gd is handled — but empty),
    let result = extract("plain.gd", content);

    // Then no references are emitted, and no nodes (base spec owns symbols).
    assert!(
        result.references.is_empty(),
        "no dynamic patterns → zero refs, got {:?}",
        result.references
    );
    assert!(
        result.nodes.is_empty(),
        "this layer emits no nodes, got {:?}",
        result.nodes
    );
}

#[test]
fn parsing_is_deterministic_across_runs() {
    // Given GDScript with several dynamic call-sites across functions,
    let content = "\
func _ready():
\ttimer.timeout.connect(_on_timeout)
\t$Player/Sprite.show()
\t%Health.set_value(10)

func hurt():
\temit_signal(\"health_changed\")
\tget_tree().get_nodes_in_group(\"enemies\")
\tget_node(some_var)
";
    // When extracting twice,
    let a = extract("e.gd", content);
    let b = extract("e.gd", content);

    // Then the parser-controlled fields (source/target/kind/order) match. (No
    // nodes are emitted by this layer, so there is no `updated_at` clock field
    // to exclude — the full reference vectors compare directly.)
    let proj = |r: &RefView| {
        (
            r.from_node_id.clone(),
            r.reference_name.clone(),
            r.reference_kind,
            r.line,
        )
    };
    let refs_a: Vec<_> = a.references.iter().map(proj).collect();
    let refs_b: Vec<_> = b.references.iter().map(proj).collect();
    assert_eq!(
        refs_a, refs_b,
        "reference source/target/kind/line/order must be deterministic"
    );
}

#[test]
fn malformed_gdscript_does_not_panic() {
    // Given GDScript with unterminated strings and unbalanced parens,
    let content = "\
func broken(:
\temit_signal(\"unterminated
\tget_node(
\t$
\t%
\t.connect(
";
    // When extracting, it must not panic and must return Some.
    let result = extract("broken.gd", content);
    // Then we get a (possibly empty) result without crashing.
    let _ = result.references.len();
}

#[test]
fn extract_routes_gd_to_t6_and_others_correctly() {
    // A .gd dispatches to T6 (Some).
    assert!(GodotResolver
        .extract("player.gd", "extends Node\n", "")
        .is_some());
    // A nested path whose extension is .gd still dispatches.
    assert!(GodotResolver
        .extract("a/b/c/Deep.gd", "extends Node\n", "")
        .is_some());

    // A .tscn routes to T4 (Some, via the scene parser).
    assert!(GodotResolver
        .extract("scenes/Main.tscn", "[gd_scene format=3]\n", "")
        .is_some());
    // A .tres routes to T5 (Some, via the resource parser).
    assert!(GodotResolver
        .extract("data/item.tres", "[gd_resource format=3]\n", "")
        .is_some());
    // project.godot routes to T3 (Some, via the project parser).
    assert!(GodotResolver
        .extract("project.godot", "[autoload]\nX=\"res://x.gd\"\n", "")
        .is_some());
    // A non-Godot file the resolver doesn't claim → None.
    assert!(GodotResolver.extract("README.md", "# hi\n", "").is_none());
}
