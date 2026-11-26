//! Branch conditions (`when`) on the Symbol view's rails and the Flow's
//! connectors, against a real index — a port of upstream
//! `__tests__/branch-guards.test.ts` "branch guards: on the wire" (`e8ddc4e2`,
//! 340-376), plus this port's own cases for what must stay unlabelled. The
//! file and site caps are pinned without the wall-clock budget beside
//! `src/api/when.rs`.

mod support;

use serde_json::Value;
use support::{Project, Viewer};

/// Upstream's fixture, line for line (`branch-guards.test.ts:342-353`).
const APP_TS: &str = "export function helper() { return 1 }
export function other() { return 2 }
export function run(ready: boolean, busy: boolean) {
  if (busy) return
  if (ready) {
    helper()
  } else {
    other()
  }
}
";

/// The edges of the rail row whose symbol is `name`.
fn rail_edges<'v>(rail: &'v Value, name: &str) -> &'v Vec<Value> {
    rail["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["node"]["name"] == name)
        .unwrap_or_else(|| panic!("no row for {name} in {rail}"))["edges"]
        .as_array()
        .unwrap()
}

fn when_of(edge: &Value) -> Option<&str> {
    edge.get("when").and_then(Value::as_str)
}

async fn node(viewer: &Viewer, name: &str) -> Value {
    let id = viewer.id_of(name, Some("function")).await;
    viewer
        .json(&format!("/api/node/{}", support::encode(&id)))
        .await
}

#[tokio::test]
async fn labels_symbol_view_rails_and_flow_connectors_with_the_call_sites_conditions() {
    let project = Project::indexed(&[("src/app.ts", APP_TS)]);
    let viewer = project.viewer();

    let run = node(&viewer, "run").await;
    assert_eq!(
        when_of(&rail_edges(&run["outgoing"], "helper")[0]),
        Some("!busy && ready")
    );
    assert_eq!(
        when_of(&rail_edges(&run["outgoing"], "other")[0]),
        Some("!busy && !ready")
    );

    let helper = node(&viewer, "helper").await;
    assert_eq!(
        when_of(&rail_edges(&helper["incoming"], "run")[0]),
        Some("!busy && ready")
    );

    let flow = viewer.json("/api/flow?from=run&to=helper").await;
    let hop = &flow["flows"][0]["hops"][1];
    assert_eq!(when_of(&hop["edge"]), Some("!busy && ready"), "{flow}");
    assert_eq!(hop["edge"]["label"], "calls · when !busy && ready");
}

#[tokio::test]
async fn a_file_that_drifted_since_the_index_gives_no_condition() {
    let project = Project::indexed(&[("src/app.ts", APP_TS)]);
    // A line added on top: every recorded call site now points one line too
    // high, and a condition read there would be another line's.
    project.write("src/app.ts", &format!("// moved\n{APP_TS}"));
    let viewer = project.viewer();

    let run = node(&viewer, "run").await;
    for name in ["helper", "other"] {
        for edge in rail_edges(&run["outgoing"], name) {
            assert_eq!(when_of(edge), None, "{name}: {edge}");
        }
    }
    let flow = viewer.json("/api/flow?from=run&to=helper").await;
    let hop = &flow["flows"][0]["hops"][1];
    assert_eq!(when_of(&hop["edge"]), None, "{flow}");
    assert_eq!(hop["edge"]["label"], "calls");
}

#[tokio::test]
async fn a_language_without_rules_gives_no_condition() {
    let project = Project::indexed(&[(
        "lib/app.rb",
        "def helper()
  1
end

def run(ready)
  if ready
    helper()
  end
end
",
    )]);
    let viewer = project.viewer();
    let id = viewer.id_of("run", None).await;
    let run = viewer
        .json(&format!("/api/node/{}", support::encode(&id)))
        .await;
    let edges = rail_edges(&run["outgoing"], "helper");
    assert!(!edges.is_empty());
    for edge in edges {
        assert_eq!(when_of(edge), None, "{edge}");
    }
}

/// Twenty-five callers in twenty-five files, each under a condition, and a
/// conditional call of the focal symbol's own. The focal file is read first,
/// so its own call is labelled whatever the budget; the last caller in rail
/// order is past the twenty-four files a request reads, so it never is.
#[tokio::test]
async fn reads_the_focal_file_first_and_stops_at_the_file_cap() {
    let mut files: Vec<(String, String)> = vec![(
        "src/hub.ts".to_string(),
        "export function leaf() { return 0 }
export function hub(on: boolean) {
  if (on) {
    leaf()
  }
}
"
        .to_string(),
    )];
    for i in 0..25 {
        files.push((
            format!("src/callers/c{i:02}.ts"),
            format!(
                "import {{ hub }} from '../hub'
export function caller{i:02}(flag: boolean) {{
  if (flag) {{
    hub(true)
  }}
}}
"
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let project = Project::indexed(&refs);
    let viewer = project.viewer();
    let hub = node(&viewer, "hub").await;

    assert_eq!(
        when_of(&rail_edges(&hub["outgoing"], "leaf")[0]),
        Some("on")
    );
    // Each caller file is two rows, its import and its function; the
    // functions are what carry the conditions.
    let callers: Vec<&Value> = hub["incoming"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["node"]["kind"] == "function")
        .collect();
    assert_eq!(callers.len(), 25, "{}", hub["incoming"]);
    let labelled: Vec<&str> = callers
        .iter()
        .filter(|r| {
            r["edges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| when_of(e).is_some())
        })
        .map(|r| r["node"]["name"].as_str().unwrap())
        .collect();
    // The focal file is one of the twenty-four; at most twenty-three caller
    // files follow it, and the budget may stop the pass sooner.
    assert!(labelled.len() <= 23, "{labelled:?}");
    let last = callers.last().unwrap();
    assert_eq!(last["node"]["name"], "caller24");
    assert!(
        last["edges"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| when_of(e).is_none()),
        "{last}"
    );
    for row in callers.iter().filter(|r| {
        r["edges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| when_of(e).is_some())
    }) {
        for edge in row["edges"].as_array().unwrap() {
            assert_eq!(when_of(edge), Some("flag"), "{row}");
        }
    }
}
