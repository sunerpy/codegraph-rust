use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-impact-edge-count-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cli(args: &[&str]) -> (String, String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .output()
        .expect("run codegraph binary");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn index_project(project: &Path) {
    let p = project.to_str().unwrap();
    let (out, err, ok) = cli(&["init", p]);
    assert!(ok, "init failed: stdout={out} stderr={err}");
    let (out, err, ok) = cli(&["index", "--force", p]);
    assert!(ok, "index --force failed: stdout={out} stderr={err}");
}

fn impact_json(project: &Path, symbol: &str) -> serde_json::Value {
    let p = project.to_str().unwrap();
    let (stdout, err, ok) = cli(&["impact", symbol, "-p", p, "--depth", "4", "--json"]);
    assert!(ok, "impact failed: stdout={stdout} stderr={err}");
    serde_json::from_str(&stdout).expect("impact emits valid JSON on stdout")
}

#[test]
fn godot_resource_referrers_count_as_impact_edges() {
    let dir = TestDir::new("godot");
    let project = dir.path().join("godot");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("project.godot"),
        "[application]\nconfig/name=\"Impact edge count\"\n",
    )
    .unwrap();
    fs::write(
        project.join("weapon_data.gd"),
        "class_name WeaponData\nextends Resource\n",
    )
    .unwrap();
    for index in 0..12 {
        fs::write(
            project.join(format!("weapon_{index}.tres")),
            "[gd_resource type=\"Resource\" load_steps=2 format=3]\n\
             \n\
             [ext_resource type=\"Script\" path=\"res://weapon_data.gd\" id=\"1_script\"]\n\
             \n\
             [resource]\n\
             script = ExtResource(\"1_script\")\n",
        )
        .unwrap();
    }
    index_project(&project);

    let value = impact_json(&project, "weapon_data.gd");
    let resource_referrers = value["affected"]
        .as_array()
        .expect("affected is an array")
        .iter()
        .filter(|node| {
            node["filePath"]
                .as_str()
                .is_some_and(|path| path.ends_with(".tres"))
        })
        .count() as u64;
    let edge_count = value["edgeCount"].as_u64().expect("edgeCount is a number");

    assert_eq!(edge_count, resource_referrers);
    assert!(edge_count > 0, "resource referrers must contribute edges");
    assert_eq!(
        value["resourceEdgeCount"]
            .as_u64()
            .expect("resourceEdgeCount is a number"),
        resource_referrers
    );
}

#[test]
fn godot_referrer_already_reached_by_graph_is_not_counted_twice() {
    let dir = TestDir::new("godot-overlap");
    let project = dir.path().join("godot");
    fs::create_dir_all(project.join("scripts")).unwrap();
    fs::create_dir_all(project.join("data")).unwrap();
    fs::write(
        project.join("project.godot"),
        "[application]\nconfig/name=\"Impact edge overlap\"\n",
    )
    .unwrap();
    fs::write(
        project.join("scripts/target.gd"),
        "class_name Target\nextends Resource\n",
    )
    .unwrap();
    fs::write(
        project.join("scripts/user.gd"),
        "const TargetResource = preload(\"res://scripts/target.gd\")\n",
    )
    .unwrap();
    fs::write(
        project.join("data/one.tres"),
        "[gd_resource type=\"Resource\" load_steps=2 format=3]\n\
         \n\
         [ext_resource type=\"Script\" path=\"res://scripts/target.gd\" id=\"1_script\"]\n\
         \n\
         [resource]\n\
         script = ExtResource(\"1_script\")\n",
    )
    .unwrap();
    index_project(&project);

    let value = impact_json(&project, "target.gd");
    let affected = value["affected"].as_array().expect("affected is an array");
    let file_paths = affected
        .iter()
        .map(|node| {
            node["filePath"]
                .as_str()
                .expect("every affected row has a filePath")
        })
        .collect::<Vec<_>>();
    let distinct_file_paths = file_paths.iter().copied().collect::<HashSet<_>>();

    assert_eq!(
        distinct_file_paths.len(),
        file_paths.len(),
        "a graph-reached preload referrer must not be appended again: {value}"
    );
    assert_eq!(value["edgeCount"].as_u64(), Some(2), "{value}");
    assert_eq!(value["resourceEdgeCount"].as_u64(), Some(1), "{value}");
}

#[test]
fn pure_code_edge_count_is_unchanged() {
    let dir = TestDir::new("pure-code");
    let project = dir.path().join("typescript");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("chain.ts"),
        "export function helper(): number { return 1; }\n\
         export function caller(): number { return helper(); }\n\
         export function outer(): number { return caller(); }\n",
    )
    .unwrap();
    index_project(&project);

    let value = impact_json(&project, "helper");

    assert_eq!(value["edgeCount"].as_u64(), Some(2));
    assert_eq!(value["resourceEdgeCount"].as_u64(), Some(0));
}
