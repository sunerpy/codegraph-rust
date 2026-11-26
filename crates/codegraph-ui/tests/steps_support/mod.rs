//! Helpers the Steps tests share: finding a route, asking for a picture,
//! and reading steps, links and the code-order reading back.

#![allow(dead_code)]

use serde_json::Value;

use crate::support::{Viewer, encode};

pub fn items(value: &Value) -> &Vec<Value> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {value}"))
}

/// The id of the route named `name`: from the routes list, the Screens
/// list, or search — a route no handler binding lists and no navigation
/// reaches is found by name.
pub async fn route_id(viewer: &Viewer, name: &str) -> String {
    let routes = viewer.json("/api/routes?limit=500").await;
    if let Some(entry) = items(&routes["entries"])
        .iter()
        .find(|entry| entry["url"] == name)
    {
        return entry["routeId"].as_str().unwrap().to_string();
    }
    let screens = viewer.json("/api/screens").await;
    if let Some(screen) = items(&screens["screens"])
        .iter()
        .find(|screen| screen["path"] == name)
    {
        return screen["id"].as_str().unwrap().to_string();
    }
    let search = viewer
        .json(&format!("/api/search?q={}&limit=50", encode(name)))
        .await;
    items(&search["results"]["items"])
        .iter()
        .find(|item| item["name"] == name && item["kind"] == "route")
        .unwrap_or_else(|| panic!("no route {name}: {routes} {screens} {search}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

pub async fn steps(viewer: &Viewer, query: &str) -> Value {
    viewer.json(&format!("/api/steps?{query}")).await
}

/// The picture anchored at the route named `route`.
pub async fn anchored(viewer: &Viewer, route: &str, extra: &str) -> Value {
    let id = route_id(viewer, route).await;
    steps(viewer, &format!("anchor={}{extra}", encode(&id))).await
}

pub fn anchor_of(payload: &Value) -> &Value {
    items(&payload["steps"])
        .iter()
        .find(|step| step["anchor"] == true)
        .unwrap_or_else(|| panic!("no anchor: {payload}"))
}

pub fn step<'v>(payload: &'v Value, label: &str) -> &'v Value {
    items(&payload["steps"])
        .iter()
        .find(|step| step["label"] == label)
        .unwrap_or_else(|| panic!("no step {label}: {}", payload["steps"]))
}

pub fn kind_of(payload: &Value, label: &str) -> Value {
    items(&payload["steps"])
        .iter()
        .find(|step| step["label"] == label)
        .map_or(Value::Null, |step| step["kind"].clone())
}

pub fn link<'v>(payload: &'v Value, from: &str, to: &str) -> Option<&'v Value> {
    let (from, to) = (
        step(payload, from)["id"].clone(),
        step(payload, to)["id"].clone(),
    );
    items(&payload["links"])
        .iter()
        .find(|link| link["from"] == from && link["to"] == to)
}

/// The first link into step `id`.
pub fn link_to<'v>(payload: &'v Value, id: &Value) -> &'v Value {
    items(&payload["links"])
        .iter()
        .find(|link| link["to"] == *id)
        .unwrap_or_else(|| panic!("no link to {id}: {payload}"))
}

/// The first effect box of `category`, if any.
pub fn find_effect<'v>(payload: &'v Value, category: &str) -> Option<&'v Value> {
    items(&payload["steps"])
        .iter()
        .find(|step| step["kind"] == "effect" && step["effect"]["category"] == category)
}

pub fn effect<'v>(payload: &'v Value, category: &str) -> &'v Value {
    find_effect(payload, category)
        .unwrap_or_else(|| panic!("no {category} effect: {}", payload["steps"]))
}

pub fn via(link: &Value) -> Vec<&str> {
    items(&link["via"])
        .iter()
        .map(|node| node["name"].as_str().unwrap())
        .collect()
}

/// The `program` reading, one line per item, indented (upstream's
/// `inOrder`): a step its label, a fork its condition and each arm's, a
/// bracketed run its words.
pub fn in_order(payload: &Value) -> Vec<String> {
    let label = |id: &Value| {
        items(&payload["steps"])
            .iter()
            .find(|step| step["id"] == *id)
            .and_then(|step| step["label"].as_str())
            .map_or_else(|| id.as_str().unwrap_or("").to_string(), str::to_string)
    };
    fn walk(block: &Value, indent: &str, label: &dyn Fn(&Value) -> String, out: &mut Vec<String>) {
        for item in items(block) {
            match item["kind"].as_str().unwrap() {
                "step" => {
                    let again = if item["again"] == true {
                        " (again)"
                    } else {
                        ""
                    };
                    out.push(format!("{indent}{}{again}", label(&item["step"])));
                    if item["body"].is_array() {
                        walk(&item["body"], &format!("{indent}  "), label, out);
                    }
                }
                "fork" => {
                    out.push(format!(
                        "{indent}{} {}",
                        item["form"].as_str().unwrap(),
                        item["on"].as_str().unwrap()
                    ));
                    for arm in items(&item["arms"]) {
                        let ends = arm["ends"]
                            .as_str()
                            .map(|ends| format!(" → {ends}"))
                            .unwrap_or_default();
                        out.push(format!("{indent}  {}{ends}", arm["when"].as_str().unwrap()));
                        walk(&arm["body"], &format!("{indent}    "), label, out);
                    }
                }
                "block" => {
                    let words = item["via"]["name"]
                        .as_str()
                        .or_else(|| item["by"].as_str())
                        .unwrap_or("");
                    out.push(
                        format!("{indent}{} {words}", item["block"].as_str().unwrap())
                            .trim_end()
                            .to_string(),
                    );
                    walk(&item["body"], &format!("{indent}  "), label, out);
                }
                _ => out.push(format!(
                    "{indent}cut {}",
                    item["why"].as_str().unwrap_or("")
                )),
            }
        }
    }
    let mut out = Vec::new();
    if payload["program"].is_object() {
        walk(&payload["program"]["root"], "", &label, &mut out);
    }
    out
}
