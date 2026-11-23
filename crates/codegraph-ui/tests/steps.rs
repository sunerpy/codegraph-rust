//! The Steps picture (`GET /api/steps`) of a small Expo + React Native app —
//! a port of upstream `__tests__/ui-steps-api.test.ts:170-381` at
//! `e8ddc4e2`, over its fixture (`fixtures::STEPS_MOBILE`) — and the Steps
//! halves of upstream's router tests, then this port's own cases: the
//! errors and a total order that does not depend on how the index was built.

mod fixtures;
mod steps_support;
mod support;

use codegraph_core::types::Language;
use codegraph_ui::api::steps::{crossing, effect_category, is_store_file};
use serde_json::{Map, Value, json};
use steps_support::{anchored, effect, items, kind_of, link, step, steps, via};
use support::Project;

fn mobile() -> Project {
    Project::indexed(fixtures::STEPS_MOBILE)
}

// ------------------------------------------------- classification helpers --

#[test]
fn crossing_js_to_native_is_a_bridge_native_to_js_an_event_anything_else_nothing() {
    let none = Map::new();
    assert_eq!(
        crossing(Language::Tsx, Language::Swift, &none),
        Some("bridge")
    );
    assert_eq!(
        crossing(Language::Swift, Language::Tsx, &none),
        Some("event")
    );
    assert_eq!(
        crossing(Language::TypeScript, Language::JavaScript, &none),
        None
    );
    assert_eq!(crossing(Language::Swift, Language::ObjC, &none), None);
}

#[test]
fn store_files() {
    assert!(is_store_file("src/storage/capture.storage.ts"));
    assert!(is_store_file("src/stores/user.ts"));
    assert!(is_store_file("src/features/cart/cart.slice.ts"));
    assert!(!is_store_file("src/components/button.tsx"));
    assert!(!is_store_file("src/restore/thing.ts"));
}

#[test]
fn effects_a_curated_table_by_reference_text() {
    assert_eq!(effect_category("client.post"), Some("network"));
    assert_eq!(effect_category("fetch"), Some("network"));
    assert_eq!(effect_category("AsyncStorage.setItem"), Some("storage"));
    assert_eq!(effect_category("Linking.openURL"), Some("device"));
    assert_eq!(effect_category("DdRum.addAction"), Some("telemetry"));
    assert_eq!(effect_category("Math.max"), None);
    assert_eq!(effect_category("i18n.t"), None);
}

// ------------------------------------------------------------- buildSteps --

#[tokio::test]
async fn walks_a_screen_through_its_handler_the_bridge_the_event_the_store_and_the_request() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/capture/review", "").await;

    assert_eq!(kind_of(&payload, "/capture/review"), "screen");
    let anchor = items(&payload["steps"])
        .iter()
        .find(|step| step["anchor"] == true)
        .unwrap();
    assert_eq!(anchor["label"], "/capture/review");
    assert_eq!(kind_of(&payload, "handleApprove"), "trigger", "{payload}");
    assert_eq!(kind_of(&payload, "finalizeCaptureSession"), "bridge");
    assert_eq!(kind_of(&payload, "handleZipComplete"), "event");
    assert_eq!(
        step(&payload, "handleZipComplete")["event"],
        "onZipComplete"
    );
    assert_eq!(
        step(&payload, "handleZipComplete")["events"],
        json!(["onZipComplete"])
    );
    assert_eq!(kind_of(&payload, "setZipUri"), "store");
    let network = effect(&payload, "network");
    assert_eq!(network["label"], "client.post +1");
    assert_eq!(
        network["effect"]["apis"],
        json!(["client.post", "client.get"])
    );
    assert_eq!(network["effect"]["by"]["name"], "uploadARCapture");
    assert_eq!(kind_of(&payload, "/"), "screen");
    // Another screen is a boundary: drawn, marked, not entered.
    assert_eq!(step(&payload, "/")["cut"], "screen");

    let request = link(&payload, "handleZipComplete", "client.post +1").unwrap();
    let tap = link(&payload, "/capture/review", "handleApprove").unwrap();
    assert_eq!(tap["kind"], "handler");
    let on_press =
        json!({ "kind": "prop", "name": "onPress", "of": "Button", "in": "ReviewScreen" });
    assert_eq!(tap["trigger"], on_press);
    assert_eq!(step(&payload, "handleApprove")["trigger"], on_press);
    assert_eq!(kind_of(&payload, "handleSubmit"), "trigger");
    assert_eq!(
        link(&payload, "/capture/review", "handleSubmit").unwrap()["trigger"],
        json!({ "kind": "option", "name": "onSubmit", "of": "useForm", "in": "ReviewScreen" })
    );
    assert_eq!(
        link(&payload, "/capture/review", "handleZipComplete").unwrap()["trigger"],
        json!({ "kind": "callback", "name": "addListener", "of": "'onZipComplete'", "in": "ReviewScreen" })
    );
    assert_eq!(
        link(&payload, "handleApprove", "finalizeCaptureSession").unwrap()["kind"],
        "bridge"
    );
    let event = link(&payload, "finalizeCaptureSession", "handleZipComplete").unwrap();
    assert_eq!(event["kind"], "event");
    assert_eq!(event["synthesized"], true);
    assert_eq!(via(event), ["emitZipComplete"]);
    assert_eq!(event["when"], "result");
    assert!(
        event["label"]
            .as_str()
            .unwrap()
            .contains("event onZipComplete"),
        "{event}"
    );
    let store = link(&payload, "handleZipComplete", "setZipUri").unwrap();
    assert_eq!(store["kind"], "store");
    assert_eq!(store["sites"][0]["args"], "data.uri");
    assert_eq!(
        link(&payload, "handleApprove", "finalizeCaptureSession").unwrap()["sites"][0]["args"],
        ""
    );
    let alert = effect(&payload, "device");
    assert_eq!(alert["label"], "Alert.alert('Uploaded', data.uri, […])");
    assert_eq!(network["label"], "client.post +1");
    let sites: Vec<String> = items(&request["sites"])
        .iter()
        .map(|site| {
            format!(
                "{}({})",
                site["text"].as_str().unwrap(),
                site["args"].as_str().unwrap_or("undefined")
            )
        })
        .collect();
    assert_eq!(
        sites,
        [
            "client.post('/frames', { uri })",
            "client.get('/frames/status')"
        ]
    );
    assert_eq!(request["kind"], "effect");
    assert_eq!(via(request), ["uploadARCapture"]);
    let nav = link(&payload, "handleZipComplete", "/").unwrap();
    assert_eq!(nav["kind"], "navigates");
    assert_eq!(nav["when"], "unlimited");
    assert_eq!(nav["sites"][0]["text"], "replace /");
    assert_eq!(nav["sites"][0]["when"], "unlimited");
    assert_eq!(event["sites"][0]["when"], "result");
    assert_eq!(store["sites"][0]["when"], "");

    assert_eq!(step(&payload, "/capture/review")["depth"], 0);
    assert_eq!(step(&payload, "handleApprove")["depth"], 1);
    assert_eq!(step(&payload, "finalizeCaptureSession")["depth"], 2);
    assert_eq!(step(&payload, "handleZipComplete")["depth"], 1);
    assert_eq!(
        link(&payload, "/capture/review", "handleZipComplete").unwrap()["kind"],
        "handler"
    );
    assert_eq!(network["depth"], 2);
    assert_eq!(payload["through"], false);
    assert_eq!(
        payload["truncated"],
        json!({ "steps": 0, "hubs": 0, "chrome": 0 })
    );
    let cut: Vec<(String, String)> = items(&payload["steps"])
        .iter()
        .filter(|step| !step["cut"].is_null())
        .map(|step| {
            (
                step["label"].as_str().unwrap().to_string(),
                step["cut"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(cut, [("/".to_string(), "screen".to_string())]);
}

#[tokio::test]
async fn walks_through_a_memo_wrapped_component_into_the_screen_body() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/capture", "").await;
    assert_eq!(kind_of(&payload, "handleOpen"), "trigger", "{payload}");
    assert_eq!(kind_of(&payload, "finalizeCaptureSession"), "bridge");
    let handler = step(&payload, "handleOpen")["id"].clone();
    let to_handler = items(&payload["links"])
        .iter()
        .find(|link| link["to"] == handler)
        .unwrap();
    assert_eq!(
        via(to_handler),
        ["MemoizedCaptureComponent", "CaptureComponent"]
    );
    assert_eq!(
        to_handler["trigger"],
        json!({ "kind": "prop", "name": "onPress", "of": "Button", "in": "CaptureComponent" })
    );
    assert!(
        !items(&payload["steps"])
            .iter()
            .any(|step| step["label"] == "CaptureComponent")
    );
}

#[tokio::test]
async fn enters_other_screens_when_asked_to_continue_through_them() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/capture/review", "&through=1").await;
    assert_eq!(payload["through"], true);
    assert_eq!(step(&payload, "/")["cut"], Value::Null);
}

#[tokio::test]
async fn anchors_by_name_prefers_the_screen_and_lists_the_rest_as_ambiguous() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = steps(&viewer, "symbol=handleApprove").await;
    assert_eq!(payload["anchor"]["name"], "handleApprove");
    assert_eq!(payload["steps"][0]["kind"], "anchor");
    assert!(
        items(&payload["steps"])
            .iter()
            .any(|step| step["label"] == "finalizeCaptureSession"),
        "{payload}"
    );
}

#[tokio::test]
async fn a_depth_cap_is_announced_on_the_step_it_stopped_at() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/capture/review", "&depth=2").await;
    assert_eq!(step(&payload, "finalizeCaptureSession")["cut"], "depth");
    assert!(
        !items(&payload["links"])
            .iter()
            .any(|link| link["kind"] == "event")
    );
    assert_eq!(kind_of(&payload, "handleZipComplete"), "trigger");
}

#[tokio::test]
async fn refuses_a_missing_anchor_and_an_unknown_id() {
    let project = mobile();
    let viewer = project.viewer();
    let missing = viewer.get("/api/steps").await;
    assert_eq!(missing.status, 400);
    assert!(missing.text().contains("anchor"), "{}", missing.text());
    let unknown = viewer.get("/api/steps?anchor=function%3Anope").await;
    assert_eq!(unknown.status, 404);
    assert!(unknown.text().contains("No symbol"), "{}", unknown.text());
    let unnamed = viewer.get("/api/steps?symbol=nothingNamedThis").await;
    assert_eq!(unnamed.status, 404);
    assert!(unnamed.text().contains("Nothing"), "{}", unnamed.text());
}

// --------------------------------------------------------- screen regions --

#[tokio::test]
async fn a_screen_names_every_steps_region_inherited_down_the_walk() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/capture/review", "").await;
    for step in items(&payload["steps"]) {
        if step["anchor"] != true {
            assert!(step["region"].is_object(), "{}: {step}", step["label"]);
        }
    }
    assert_eq!(
        step(&payload, "handleApprove")["region"]["label"],
        "ReviewScreen"
    );
    assert_eq!(
        step(&payload, "finalizeCaptureSession")["region"]["id"],
        step(&payload, "handleApprove")["region"]["id"]
    );
    assert_eq!(
        step(&payload, "setZipUri")["region"]["label"],
        "ReviewScreen"
    );
}

#[tokio::test]
async fn a_step_reached_through_a_folded_component_belongs_to_that_component() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/capture", "").await;
    let handler = step(&payload, "handleOpen");
    let to_handler = items(&payload["links"])
        .iter()
        .find(|link| link["to"] == handler["id"])
        .unwrap();
    assert_eq!(handler["region"]["label"], to_handler["via"][0]["name"]);
}

#[tokio::test]
async fn an_anchor_with_a_body_carries_no_regions() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = steps(&viewer, "symbol=handleApprove").await;
    for step in items(&payload["steps"]) {
        assert!(step.get("region").is_none(), "{step}");
    }
}

// ------------------------------------------------------ fired from a dialog --

#[tokio::test]
async fn a_handler_bound_inside_a_dialogs_buttons_arrives_from_the_dialog() {
    let project = mobile();
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/confirm", "").await;
    let prompt = items(&payload["steps"])
        .iter()
        .find(|step| {
            step["kind"] == "effect" && step["label"].as_str().unwrap().starts_with("Alert.prompt")
        })
        .unwrap_or_else(|| panic!("{payload}"));
    let handler = step(&payload, "removeThing");
    let into: Vec<&Value> = items(&payload["links"])
        .iter()
        .filter(|link| link["to"] == handler["id"])
        .collect();
    assert_eq!(into.len(), 1, "{payload}");
    assert_eq!(into[0]["from"], prompt["id"]);
    assert_eq!(into[0]["trigger"]["of"], "Alert.prompt");
    assert_eq!(into[0]["sites"][0]["args"], "name");
    assert_eq!(
        handler["depth"].as_u64().unwrap(),
        prompt["depth"].as_u64().unwrap() + 1
    );
    assert_eq!(handler["region"]["id"], prompt["region"]["id"]);
    let post = effect(&payload, "network");
    assert!(
        items(&payload["links"])
            .iter()
            .any(|link| link["from"] == handler["id"] && link["to"] == post["id"])
    );
}

// ------------------------------------------------- the routers' Steps halves --

/// Upstream `react-router.test.ts:312-330`: the payment screen's picture
/// draws the pages it leads to, not just its store write.
#[tokio::test]
async fn react_router_the_payment_screens_steps_draw_the_pages_it_leads_to() {
    let project = Project::indexed(fixtures::RR_APP);
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/payment", "").await;
    let anchor = items(&payload["steps"])
        .iter()
        .find(|step| step["anchor"] == true)
        .unwrap();
    assert_eq!(anchor["sub"], "PaymentScreen");
    let store = effect(&payload, "storage");
    assert!(
        store["label"]
            .as_str()
            .unwrap()
            .contains("localStorage.setItem('paymentMethod'"),
        "{store}"
    );
    // Its own two pushes, plus the link back to sign-in its checkout nav
    // renders.
    let mut to: Vec<&str> = items(&payload["steps"])
        .iter()
        .filter(|step| step["kind"] == "screen" && step["anchor"] != true)
        .map(|step| step["screen"]["path"].as_str().unwrap())
        .collect();
    to.sort();
    assert_eq!(to, ["/cart/:id?", "/login", "/placeorder", "/shipping"]);
    let placeorder = items(&payload["steps"])
        .iter()
        .find(|step| step["screen"]["path"] == "/placeorder")
        .unwrap();
    assert_eq!(placeorder["cut"], "screen");
    let push = items(&payload["links"])
        .iter()
        .find(|link| link["to"] == placeorder["id"])
        .unwrap();
    assert_eq!(push["kind"], "navigates");
    assert!(
        items(&push["sites"])
            .iter()
            .any(|site| site["text"] == "push /placeorder")
    );
    let shipping = items(&payload["steps"])
        .iter()
        .find(|step| step["screen"]["path"] == "/shipping")
        .unwrap();
    let bounce = items(&payload["links"])
        .iter()
        .find(|link| link["to"] == shipping["id"])
        .unwrap();
    assert_eq!(bounce["sites"][0]["text"], "push /shipping");
    assert_eq!(bounce["sites"][0]["when"], "!shippingAddress.address");
}

/// Upstream `nextjs.test.ts:287-312`: a page's picture fires from its load,
/// crosses to the server action, and draws the pages it leads to as
/// boundaries.
#[tokio::test]
async fn nextjs_a_pages_steps_fire_from_its_load_and_cross_to_the_server_action() {
    let project = Project::indexed(fixtures::NEXT_APP);
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/users", "").await;
    assert_eq!(payload["project"], "web");
    let anchor = items(&payload["steps"])
        .iter()
        .find(|step| step["anchor"] == true)
        .unwrap();
    assert_eq!(anchor["kind"], "screen");
    assert_eq!(anchor["sub"], "UsersPage");
    assert_eq!(
        anchor["trigger"],
        json!({ "kind": "load", "name": "GET", "of": "/users", "in": "page.tsx" })
    );
    let by = |category: &str, name: &str| {
        items(&payload["steps"])
            .iter()
            .find(|step| {
                step["kind"] == "effect"
                    && step["effect"]["category"] == category
                    && step["effect"]["by"]["name"] == name
            })
            .unwrap_or_else(|| panic!("no {category} effect by {name}: {payload}"))
            .clone()
    };
    assert_eq!(
        by("database", "UsersPage")["label"],
        "prisma.user.findMany()"
    );
    let handler = items(&payload["steps"])
        .iter()
        .find(|step| step["kind"] == "trigger" && step["node"]["name"] == "handleSubmit")
        .unwrap_or_else(|| panic!("{payload}"));
    assert_eq!(handler["trigger"]["kind"], "prop");
    assert_eq!(handler["trigger"]["name"], "onSubmit");
    assert_eq!(handler["trigger"]["of"], "form");
    let action = items(&payload["steps"])
        .iter()
        .find(|step| step["node"]["name"] == "createUserAction")
        .unwrap();
    assert_eq!(action["kind"], "bridge");
    let to_action = items(&payload["links"])
        .iter()
        .find(|link| link["to"] == action["id"])
        .unwrap();
    assert!(
        to_action["label"]
            .as_str()
            .unwrap()
            .contains("server action"),
        "{to_action}"
    );
    assert_eq!(
        by("database", "createUserAction")["label"],
        "prisma.user.create({ data })"
    );
    let detail = items(&payload["steps"])
        .iter()
        .find(|step| step["kind"] == "screen" && step["screen"]["path"] == "/users/:id")
        .unwrap();
    assert_eq!(detail["cut"], "screen");
    let to_detail = items(&payload["links"])
        .iter()
        .find(|link| link["to"] == detail["id"])
        .unwrap();
    assert_eq!(to_detail["kind"], "navigates");
    assert_eq!(to_detail["when"], "user.ok");
    assert_eq!(to_detail["sites"][0]["text"], "push /users/${…}");
    let back = items(&payload["links"])
        .iter()
        .find(|link| link["from"] == action["id"] && link["to"] == anchor["id"])
        .unwrap_or_else(|| panic!("{payload}"));
    assert_eq!(back["sites"][0]["text"], "redirect /users");
    assert_eq!(back["sites"][0]["when"], "!user.verified");
}

/// Upstream `nextjs.test.ts:314-323`: an endpoint anchors as any server
/// route does.
#[tokio::test]
async fn nextjs_an_endpoint_anchors_as_any_server_route_does() {
    let project = Project::indexed(fixtures::NEXT_APP);
    let viewer = project.viewer();
    let payload = anchored(&viewer, "POST /api/users", "").await;
    let anchor = items(&payload["steps"])
        .iter()
        .find(|step| step["anchor"] == true)
        .unwrap();
    assert_eq!(anchor["sub"], "POST");
    assert_eq!(
        anchor["trigger"],
        json!({ "kind": "request", "name": "POST", "of": "/api/users", "in": "route.ts" })
    );
    let db = effect(&payload, "database");
    assert_eq!(db["effect"]["model"], "user");
    assert_eq!(db["effect"]["access"], "write");
    assert_eq!(db["effect"]["by"]["name"], "POST");
    assert_eq!(effect(&payload, "response")["label"], "201");
}

/// Upstream `expo-router.test.ts:432-600`, its Steps assertions: where the
/// app goes after login is a fork, each arm read off the literal's column.
#[tokio::test]
async fn expo_router_reads_each_arm_of_a_returned_destination_as_one_decision() {
    let project = Project::indexed(fixtures::EXPO_APP);
    let viewer = project.viewer();
    let payload = steps(&viewer, "symbol=finishLogin").await;
    let to = |label: &str| {
        let target = step(&payload, label)["id"].clone();
        items(&payload["links"])
            .iter()
            .find(|link| link["to"] == target)
            .unwrap_or_else(|| panic!("no link to {label}: {payload}"))
            .clone()
    };
    let to_root = to("/");
    let to_welcome = to("/welcome");
    let root_when = to_root["when"].as_str().unwrap();
    let welcome_when = to_welcome["when"].as_str().unwrap();
    assert!(root_when.contains("await seen()"), "{root_when}");
    assert!(!root_when.contains('!'), "{root_when}");
    let negated = regex::Regex::new(r"!\s*\(?\s*await seen\(\)").unwrap();
    assert!(negated.is_match(welcome_when), "{welcome_when}");
    let root_arm = &to_root["sites"][0]["decision"];
    let welcome_arm = &to_welcome["sites"][0]["decision"];
    assert_eq!(root_arm["branch"], welcome_arm["branch"]);
    assert_ne!(root_arm["branch"], "");
    assert_eq!(root_arm["form"], "ternary");
    assert_eq!(root_arm["on"], welcome_arm["on"]);
    assert!(root_arm["on"].as_str().unwrap().contains("await seen()"));
    assert!(!root_arm["on"].as_str().unwrap().starts_with('!'));
    assert!(root_arm.get("not").is_none());
    assert_eq!(welcome_arm["not"], true);
    assert_ne!(root_arm["arm"], welcome_arm["arm"]);
}

/// Upstream `angular-router.test.ts:359-378`, its Steps assertion: a
/// template's event binding is the trigger of the method it calls.
#[tokio::test]
async fn angular_a_template_binding_is_the_trigger_of_the_method_it_calls() {
    let files = fixtures::angular_app();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let project = Project::indexed(&refs);
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/home", "").await;
    let save = step(&payload, "save");
    assert_eq!(save["trigger"]["kind"], "prop", "{save}");
    assert_eq!(save["trigger"]["name"], "(click)");
    assert_eq!(save["trigger"]["of"], "button");
    assert_eq!(save["trigger"]["in"], "CardComponent");
}

/// Upstream `angular-router.test.ts:570-576`: a binding is render-time work
/// in Steps, not something the user triggers.
#[tokio::test]
async fn angular_draws_a_binding_as_render_time_work() {
    let project = Project::indexed(fixtures::NG_BINDINGS);
    let viewer = project.viewer();
    let payload = anchored(&viewer, "/tasks", "").await;
    let save = step(&payload, "save");
    assert_eq!(save["trigger"]["kind"], "prop", "{save}");
    assert_eq!(save["trigger"]["name"], "(blur)");
    assert_eq!(save["trigger"]["of"], "input");
    let labels: Vec<&str> = items(&payload["steps"])
        .iter()
        .map(|step| step["label"].as_str().unwrap())
        .collect();
    for name in ["remainingItems", "canSave", "trackById", "lists"] {
        assert!(!labels.contains(&name), "{name}: {labels:?}");
    }
}

// ------------------------------------------------------- This port's own --

/// The payload without what changes from one request to the next.
fn stable(mut payload: Value) -> String {
    payload.as_object_mut().unwrap().remove("timing");
    payload["index"]
        .as_object_mut()
        .unwrap()
        .remove("lastIndexedAt");
    serde_json::to_string_pretty(&payload).unwrap()
}

/// The same files indexed afresh, and indexed then edited and put back,
/// draw the same Steps picture byte for byte, for a screen and a handler.
#[tokio::test]
async fn the_steps_picture_does_not_depend_on_how_the_index_was_built() {
    let fresh = mobile();
    let synced = mobile();
    let review = fixtures::STEPS_MOBILE
        .iter()
        .find(|(path, _)| *path == "src/app/capture/review.tsx")
        .unwrap()
        .1;
    synced.write(
        "src/app/capture/review.tsx",
        &review.replace("return <Button", "return <Button key='x'"),
    );
    synced.index();
    synced.write("src/app/capture/review.tsx", review);
    synced.index();
    for query in ["symbol=handleZipComplete", "symbol=ReviewScreen"] {
        let a = steps(&fresh.viewer(), query).await;
        let b = steps(&synced.viewer(), query).await;
        assert!(!items(&a["steps"]).is_empty(), "{query}: {a}");
        assert_eq!(stable(a), stable(b), "{query}");
    }
    let a = anchored(&fresh.viewer(), "/capture/review", "").await;
    let b = anchored(&synced.viewer(), "/capture/review", "").await;
    assert_eq!(stable(a), stable(b));
}

/// The query's bounds: a depth past the cap is refused, never clamped.
#[tokio::test]
async fn a_depth_or_a_limit_out_of_bounds_is_refused() {
    let project = mobile();
    let viewer = project.viewer();
    for query in [
        "symbol=handleApprove&depth=15",
        "symbol=handleApprove&depth=0",
        "symbol=handleApprove&limit=19",
        "symbol=handleApprove&limit=401",
    ] {
        let reply = viewer.get(&format!("/api/steps?{query}")).await;
        assert_eq!(reply.status, 400, "{query}: {}", reply.text());
    }
    let ok = steps(&viewer, "symbol=handleApprove&depth=14&limit=400").await;
    assert_eq!(ok["depth"], 14);
    assert_eq!(ok["limit"], 400);
}

// ------------------------------------------------- Rust's own: dead ends --

/// The functions behind each `category` effect box, sorted.
fn effects_by(payload: &Value, category: &str) -> Vec<String> {
    let mut by: Vec<String> = items(&payload["steps"])
        .iter()
        .filter(|step| step["kind"] == "effect" && step["effect"]["category"] == category)
        .map(|step| step["effect"]["by"]["name"].as_str().unwrap().to_string())
        .collect();
    by.sort();
    by
}

/// A helper forty places call is a hub: the walk does not go through it and
/// counts it in `truncated.hubs`; a helper called once is walked through to
/// the request it sends.
#[tokio::test]
async fn a_hub_is_a_dead_end_the_picture_counts() {
    let callers: String = (0..40)
        .map(|i| format!("export function c{i}() {{\n  track('c{i}');\n}}\n"))
        .collect();
    let callers = format!("import {{ track }} from './hub';\n\n{callers}");
    let project = Project::indexed(&[
        (
            "src/hub.ts",
            "export function track(event: string) {\n  fetch('/api/track', { method: 'POST', body: event });\n}\n",
        ),
        (
            "src/ping.ts",
            "export function ping() {\n  fetch('/api/ping');\n}\n",
        ),
        (
            "src/run.ts",
            "import { track } from './hub';\nimport { ping } from './ping';\n\nexport function run() {\n  track('run');\n  ping();\n}\n",
        ),
        ("src/callers.ts", callers.as_str()),
    ]);
    let payload = steps(&project.viewer(), "symbol=run").await;
    assert_eq!(effects_by(&payload, "network"), ["ping"], "{payload}");
    assert_eq!(payload["truncated"]["hubs"], 1, "{payload}");
}

const BANNER: (&str, &str) = (
    "src/Banner.tsx",
    "export function Banner() {\n  fetch('/api/banner');\n  return <div>banner</div>;\n}\n",
);

fn page(name: &str) -> (String, String) {
    (
        format!("src/{name}.tsx"),
        format!(
            "import {{ Banner }} from './Banner';\n\nexport function {name}() {{\n  return <Banner />;\n}}\n"
        ),
    )
}

/// A component five others render is shared chrome: the walk stops at it
/// and counts it in `truncated.chrome`, instead of reading the banner's
/// request into every page's picture.
#[tokio::test]
async fn a_component_five_others_render_is_shared_chrome_the_walk_stops_at() {
    let pages: Vec<(String, String)> = ["Home", "PageA", "PageB", "PageC", "PageD"]
        .into_iter()
        .map(page)
        .collect();
    let mut files: Vec<(&str, &str)> = vec![BANNER];
    files.extend(
        pages
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    );
    let project = Project::indexed(&files);
    let payload = steps(&project.viewer(), "symbol=Home").await;
    assert!(effects_by(&payload, "network").is_empty(), "{payload}");
    assert_eq!(payload["truncated"]["chrome"], 1, "{payload}");
}

/// A file that renders a component at its top level is no render parent
/// (upstream #2611): four entry files and one page render the banner, so
/// it is no chrome, and the page's picture reads its request.
#[tokio::test]
async fn a_file_rendering_at_its_top_level_is_no_render_parent() {
    let entries: Vec<(String, String)> = (1..=4)
        .map(|i| {
            (
                format!("src/entry{i}.tsx"),
                format!(
                    "import {{ createRoot }} from 'react-dom/client';\nimport {{ Banner }} from './Banner';\n\ncreateRoot(document.getElementById('banner-{i}')!).render(<Banner />);\n"
                ),
            )
        })
        .collect();
    let home = page("Home");
    let mut files: Vec<(&str, &str)> = vec![BANNER, (home.0.as_str(), home.1.as_str())];
    files.extend(
        entries
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    );
    let project = Project::indexed(&files);
    let payload = steps(&project.viewer(), "symbol=Home").await;
    assert_eq!(effects_by(&payload, "network"), ["Banner"], "{payload}");
    assert_eq!(payload["truncated"]["chrome"], 0, "{payload}");
}

/// A production walk never enters a test double; a test's own walk does.
#[tokio::test]
async fn a_production_walk_never_enters_a_test_double() {
    let project = Project::indexed(&[
        (
            "src/api.ts",
            "export function loadUser() {\n  return fetch('/api/user');\n}\n",
        ),
        (
            "test/fakes.ts",
            "export function fakeUser() {\n  return fetch('/fake/user');\n}\n",
        ),
        (
            "src/run.ts",
            "import { loadUser } from './api';\nimport { fakeUser } from '../test/fakes';\n\nexport function run() {\n  loadUser();\n  fakeUser();\n}\n",
        ),
        (
            "test/run.test.ts",
            "import { fakeUser } from './fakes';\n\nexport function checkRun() {\n  fakeUser();\n}\n",
        ),
    ]);
    let viewer = project.viewer();
    let production = steps(&viewer, "symbol=run").await;
    assert_eq!(
        effects_by(&production, "network"),
        ["loadUser"],
        "{production}"
    );
    let test = steps(&viewer, "symbol=checkRun").await;
    assert_eq!(effects_by(&test, "network"), ["fakeUser"], "{test}");
}
