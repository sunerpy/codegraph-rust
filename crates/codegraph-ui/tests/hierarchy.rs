//! The type hierarchy against a real index — the engine and `/api/node` halves
//! of upstream `__tests__/type-hierarchy.test.ts` (`v1.6.1`). Its layout cases
//! (`buildHierarchyModel`, the fold, `connectorPath`) run in `ui/tests/`; the
//! descendant bound runs against a synthetic store in `codegraph-graph`.
//!
//! Upstream's Go cases rest on the SYNTHESIZED `implements` edge of Go's
//! implicit interface satisfaction, which the synthesis stage writes since
//! W7-15b; they run over a fixture of their own. Not ported:
//! `countImplementers` and the `overrides: false` switch, which only
//! `codegraph_explore` uses.

mod support;

use std::time::{Duration, Instant};

use codegraph_core::types::{Node, NodeKind};
use codegraph_graph::hierarchy::{
    DISPATCH_MIN_IMPLEMENTERS, HierarchyRelation, build_type_hierarchy, can_have_hierarchy,
};
use codegraph_store::Store;
use support::{Project, encode};

const SHAPES_TS: &str = "export interface Drawable {
  draw(): string;
}

export abstract class Shape implements Drawable {
  draw(): string {
    return 'shape';
  }
  area(): number {
    return 0;
  }
}

export class Square extends Shape {
  draw(): string {
    return 'square';
  }
}

export class Tile extends Square {
  label = 'tile';
}
";

fn plugins_ts() -> String {
    let targets = [
        "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India",
    ];
    let classes: Vec<String> = targets
        .iter()
        .map(|name| {
            format!("export class {name}Plugin implements Plugin {{\n  run(): void {{}}\n}}")
        })
        .collect();
    format!(
        "export interface Plugin {{\n  run(): void;\n}}\n\n{}\n",
        classes.join("\n\n")
    )
}

fn fixture() -> Project {
    let plugins = plugins_ts();
    Project::indexed(&[("src/shapes.ts", SHAPES_TS), ("src/plugins.ts", &plugins)])
}

/// Upstream's Go file: `System` satisfies `Clock` without either naming
/// the other, so the `implements` edge is synthesized.
const CLOCK_GO: &str = "package clock

import \"time\"

// Clock is the time seam.
type Clock interface {
\tNow() time.Time
}

// System is the production clock.
type System struct{}

func (System) Now() time.Time { return time.Now().UTC() }

// Fixed is a frozen clock.
type Fixed struct{ At time.Time }

func (f Fixed) Now() time.Time { return f.At }
";

fn go_fixture() -> Project {
    Project::indexed(&[
        ("go.mod", "module fixture\n\ngo 1.22\n"),
        ("src/clock.go", CLOCK_GO),
    ])
}

fn store(project: &Project) -> Store {
    let paths = codegraph_core::IndexPaths::resolve(project.root(), None).unwrap();
    Store::open_for_read(&paths, Instant::now() + Duration::from_secs(30), || false)
        .expect("read the index")
}

fn node_named(store: &Store, name: &str, kind: NodeKind) -> Node {
    store
        .nodes_by_name(name)
        .unwrap()
        .into_iter()
        .find(|n| n.kind == kind)
        .unwrap_or_else(|| panic!("no {} named {name}", kind.as_str()))
}

#[test]
fn is_false_for_a_function_so_the_walk_never_runs_for_one() {
    let project = fixture();
    let store = store(&project);
    let tile = node_named(&store, "Tile", NodeKind::Class);
    for (kind, expected) in [
        (NodeKind::Function, false),
        (NodeKind::Method, false),
        (NodeKind::Class, true),
        (NodeKind::Interface, true),
        (NodeKind::Struct, true),
        (NodeKind::Trait, true),
    ] {
        let probe = Node {
            kind,
            ..tile.clone()
        };
        assert_eq!(can_have_hierarchy(&probe), expected, "{}", kind.as_str());
    }
}

#[test]
fn walks_past_the_direct_parent_to_the_whole_chain() {
    let project = fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Tile", NodeKind::Class)).unwrap();
    let find = |name: &str| {
        hierarchy
            .ancestors
            .iter()
            .find(|a| a.node.name == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(find("Square").depth, 1);
    assert_eq!(find("Shape").depth, 2);
    // `Shape implements Drawable`, so the interface is three steps up from Tile.
    assert_eq!(find("Drawable").depth, 3);
    assert_eq!(find("Square").relation, HierarchyRelation::Extends);
    assert_eq!(find("Drawable").relation, HierarchyRelation::Implements);
}

#[test]
fn nearest_ancestors_come_first() {
    let project = fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Tile", NodeKind::Class)).unwrap();
    let depths: Vec<usize> = hierarchy.ancestors.iter().map(|a| a.depth).collect();
    let mut sorted = depths.clone();
    sorted.sort_unstable();
    assert_eq!(depths, sorted);
}

#[test]
fn returns_every_direct_subtype_before_any_indirect_one() {
    let project = fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Shape", NodeKind::Class)).unwrap();
    let depths: Vec<usize> = hierarchy.descendants.iter().map(|d| d.depth).collect();
    let mut sorted = depths.clone();
    sorted.sort_unstable();
    assert_eq!(depths, sorted);
    let names: Vec<&str> = hierarchy
        .descendants
        .iter()
        .map(|d| d.node.name.as_str())
        .collect();
    assert!(
        names.contains(&"Square") && names.contains(&"Tile"),
        "{names:?}"
    );
    assert_eq!(hierarchy.direct_subtypes, 1);
}

#[test]
fn hangs_an_indirect_subtype_off_its_own_parent_not_off_the_focus() {
    let project = fixture();
    let store = store(&project);
    let focus = node_named(&store, "Shape", NodeKind::Class);
    let hierarchy = build_type_hierarchy(&store, &focus).unwrap();
    let square = hierarchy
        .descendants
        .iter()
        .find(|d| d.node.name == "Square")
        .unwrap();
    let tile = hierarchy
        .descendants
        .iter()
        .find(|d| d.node.name == "Tile")
        .unwrap();
    assert_eq!(square.parent_id, focus.id);
    assert_eq!(tile.parent_id, square.node.id);
}

#[test]
fn calls_a_nine_implementation_interface_polymorphic() {
    let project = fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Plugin", NodeKind::Interface)).unwrap();
    assert!(hierarchy.direct_implementers >= DISPATCH_MIN_IMPLEMENTERS);
    assert!(hierarchy.polymorphic);
    assert_eq!(
        hierarchy.direct_subtypes,
        hierarchy
            .descendants
            .iter()
            .filter(|d| d.depth == 1)
            .count()
    );
}

#[test]
fn does_not_call_a_one_implementation_interface_polymorphic() {
    // Upstream asks this of a two-implementation Go interface; `Drawable` has
    // one TypeScript implementation and is the same question.
    let project = fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Drawable", NodeKind::Interface)).unwrap();
    assert_eq!(hierarchy.direct_subtypes, 1);
    assert!(!hierarchy.polymorphic);
}

#[test]
fn marks_a_member_that_redeclares_an_ancestors_and_names_the_ancestor() {
    let project = fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Square", NodeKind::Class)).unwrap();
    let draw = hierarchy
        .overrides
        .values()
        .find(|m| m.base_type_name == "Shape")
        .expect("Square.draw should be matched against Shape.draw");
    assert_eq!(draw.relation, HierarchyRelation::Extends);
}

#[test]
fn leaves_a_member_that_declares_something_new_unmarked() {
    let project = fixture();
    let store = store(&project);
    let tile = node_named(&store, "Tile", NodeKind::Class);
    let hierarchy = build_type_hierarchy(&store, &tile).unwrap();
    let label = store
        .nodes_by_name("label")
        .unwrap()
        .into_iter()
        .find(|n| n.file_path == "src/shapes.ts");
    if let Some(label) = label {
        assert!(!hierarchy.overrides.contains_key(&label.id));
    }
}

/* ----------------------------------------------- Go implicit satisfaction */

#[test]
fn finds_the_implementations_of_an_interface_no_file_names() {
    let project = go_fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Clock", NodeKind::Interface)).unwrap();
    let mut names: Vec<&str> = hierarchy
        .descendants
        .iter()
        .map(|entry| entry.node.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["Fixed", "System"]);
    assert!(
        hierarchy
            .descendants
            .iter()
            .all(|entry| entry.relation == HierarchyRelation::Implements)
    );
}

#[test]
fn marks_the_synthesized_edge_and_keeps_where_it_was_wired() {
    let project = go_fixture();
    let store = store(&project);
    let hierarchy =
        build_type_hierarchy(&store, &node_named(&store, "Clock", NodeKind::Interface)).unwrap();
    let system = hierarchy
        .descendants
        .iter()
        .find(|entry| entry.node.name == "System")
        .expect("System below Clock");
    assert!(system.synthesized);
    let meta = system.edge.metadata.as_ref().expect("the edge's metadata");
    assert_eq!(meta["synthesizedBy"], "go-implements");
    assert!(
        meta["registeredAt"]
            .as_str()
            .is_some_and(|at| at.contains("clock.go")),
        "{meta}"
    );
}

/* ------------------------------------------------------- the /api/node block */

#[tokio::test]
async fn is_null_for_a_function() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("run", Some("method")).await;
    assert!(viewer.json(&format!("/api/node/{}", encode(&id))).await["hierarchy"].is_null());
}

#[tokio::test]
async fn carries_a_total_that_equals_the_list_beneath_it() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("Plugin", Some("interface")).await;
    let block = viewer.json(&format!("/api/node/{}", encode(&id))).await["hierarchy"].clone();
    let descendants = &block["descendants"];
    assert_eq!(
        descendants["items"].as_array().unwrap().len() as u64,
        descendants["shown"].as_u64().unwrap()
    );
    assert_eq!(descendants["total"], descendants["shown"]);
    assert_eq!(descendants["truncated"], false);
    assert_eq!(block["direct"], descendants["total"]);
    assert_eq!(block["polymorphic"], true);
}

#[tokio::test]
async fn lifts_the_synthesized_edges_wiring_onto_the_row() {
    let project = go_fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("Clock", Some("interface")).await;
    let block = viewer.json(&format!("/api/node/{}", encode(&id))).await["hierarchy"].clone();
    let system = block["descendants"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == "System")
        .expect("System in the block")
        .clone();
    assert_eq!(system["synthesized"], true);
    assert_eq!(system["via"], "go-implements");
    assert!(
        system["registeredAt"]
            .as_str()
            .is_some_and(|at| at.contains("clock.go")),
        "{system}"
    );
}

#[tokio::test]
async fn hands_the_outline_its_override_marks() {
    let project = fixture();
    let viewer = project.viewer();
    let id = viewer.id_of("Square", Some("class")).await;
    let body = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let draw = body["members"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "draw")
        .expect("Square.draw in the outline")
        .clone();
    assert_eq!(draw["overrides"]["baseTypeName"], "Shape");
    assert_eq!(draw["overrides"]["relation"], "extends");
    // The ancestors ride on the block too, nearest first.
    assert_eq!(body["hierarchy"]["ancestors"]["items"][0]["name"], "Shape");
}
