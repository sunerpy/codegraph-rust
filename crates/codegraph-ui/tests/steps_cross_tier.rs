//! The Steps picture across the tiers — a web app, the API it calls, and the
//! jobs, events and socket messages between them (`fixtures::CROSS_TIER`):
//! a port of upstream `__tests__/ui-steps-cross-tier.test.ts:403-474` at
//! `e8ddc4e2`.

mod fixtures;
mod steps_support;
mod support;

use serde_json::{Value, json};
use steps_support::{effect, find_effect, items, link_to, steps};
use support::Project;

fn cross_tier() -> Project {
    Project::indexed(fixtures::CROSS_TIER)
}

fn databases_by(payload: &Value) -> Vec<String> {
    items(&payload["steps"])
        .iter()
        .filter(|step| step["kind"] == "effect" && step["effect"]["category"] == "database")
        .map(|step| step["effect"]["by"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn draws_the_route_as_a_boundary_the_form_crosses_to_and_enters_it_on_request() {
    let project = cross_tier();
    let viewer = project.viewer();
    let boundary = steps(&viewer, "symbol=UsersPage").await;
    assert_eq!(boundary["project"], "web");
    let handler = items(&boundary["steps"])
        .iter()
        .find(|step| step["kind"] == "trigger" && step["node"]["name"] == "handleSubmit")
        .unwrap_or_else(|| panic!("{boundary}"));
    assert_eq!(handler["trigger"]["kind"], "prop");
    assert_eq!(handler["trigger"]["name"], "onSubmit");
    assert_eq!(handler["trigger"]["of"], "form");
    let bridge = items(&boundary["steps"])
        .iter()
        .find(|step| step["kind"] == "bridge" && step["screen"]["path"] == "POST /api/users")
        .unwrap_or_else(|| panic!("{boundary}"));
    assert_eq!(bridge["cut"], "screen");
    assert_eq!(bridge["sub"], "createUser");
    assert_eq!(
        bridge["trigger"],
        json!({ "kind": "request", "name": "POST", "of": "/api/users", "in": "app.ts" })
    );
    let link = items(&boundary["links"])
        .iter()
        .find(|link| link["from"] == handler["id"] && link["to"] == bridge["id"])
        .unwrap_or_else(|| panic!("{boundary}"));
    assert_eq!(link["kind"], "bridge");
    assert_eq!(link["synthesized"], true);
    assert_eq!(link["when"], "email");
    assert_eq!(link["sites"][0]["text"], "fetch");
    assert_eq!(link["sites"][0]["args"], "'/api/users', { method, body }");
    assert_eq!(link["sites"][0]["line"], 9);
    let label = link["label"].as_str().unwrap();
    for part in [
        "POST /api/users",
        "to the server",
        "registered at apps/api/src/app.ts:4",
    ] {
        assert!(label.contains(part), "{part}: {label}");
    }
    assert!(find_effect(&boundary, "network").is_none(), "{boundary}");
    assert_eq!(databases_by(&boundary), ["createUserAction"]);

    let through = steps(&viewer, "symbol=UsersPage&through=1").await;
    let entered = items(&through["steps"])
        .iter()
        .find(|step| step["kind"] == "bridge" && step["screen"]["path"] == "POST /api/users")
        .unwrap();
    assert_eq!(entered["cut"], Value::Null);
    let mut written = databases_by(&through);
    written.sort();
    assert_eq!(written, ["createUser", "createUserAction"]);
    let reply = effect(&through, "response");
    assert_eq!(reply["label"], "201");
    assert_eq!(reply["effect"]["by"]["name"], "createUser");
    let welcome = items(&through["steps"])
        .iter()
        .find(|step| step["kind"] == "event" && step["event"] == "welcome")
        .unwrap_or_else(|| panic!("{through}"));
    assert_eq!(welcome["node"]["name"], "sendWelcome");
    assert_eq!(
        welcome["trigger"],
        json!({ "kind": "decorator", "name": "Process", "of": "'welcome'", "in": "email.processor.ts" })
    );
    let to_welcome = link_to(&through, &welcome["id"]);
    assert_eq!(to_welcome["kind"], "event");
    assert_eq!(to_welcome["sites"][0]["text"], "emailQueue.add");
    assert_eq!(to_welcome["sites"][0]["args"], "'welcome', { userId }");
    assert_eq!(to_welcome["sites"][0]["line"], 5);
    assert_eq!(
        to_welcome["label"],
        "via queue-job · job welcome · queue email · registered at apps/api/src/email.processor.ts:5"
    );
    let queued = find_effect(&through, "queue")
        .map(|queue| queue["effect"]["apis"].clone())
        .unwrap_or_else(|| json!([]));
    assert!(
        !items(&queued).iter().any(|api| api == "emailQueue.add"),
        "{queued}"
    );
    assert_eq!(
        effect(&through, "email")["effect"]["by"]["name"],
        "sendWelcome"
    );
}

#[tokio::test]
async fn a_server_action_called_from_a_client_component_crosses_to_the_server() {
    let project = cross_tier();
    let payload = steps(&project.viewer(), "symbol=NewUserForm").await;
    let action = items(&payload["steps"])
        .iter()
        .find(|step| step["node"]["name"] == "createUserAction")
        .unwrap_or_else(|| panic!("{payload}"));
    assert_eq!(action["kind"], "bridge");
    let link = link_to(&payload, &action["id"]);
    assert_eq!(link["kind"], "bridge");
    assert_eq!(link["when"], "email && res.ok");
    assert!(
        link["label"].as_str().unwrap().contains("server action"),
        "{link}"
    );
    assert_eq!(link["sites"][0]["text"], "calls createUserAction");
    assert_eq!(link["sites"][0]["args"], "{ email }");
    assert_eq!(
        effect(&payload, "database")["effect"]["by"]["name"],
        "createUserAction"
    );
}

#[tokio::test]
async fn a_socket_message_arriving_in_a_component_is_an_event_landing_drawn_as_a_boundary() {
    let project = cross_tier();
    let payload = steps(&project.viewer(), "symbol=handleMessage").await;
    let chat = items(&payload["steps"])
        .iter()
        .find(|step| step["kind"] == "event" && step["node"]["name"] == "Chat")
        .unwrap_or_else(|| panic!("{payload}"));
    assert_eq!(chat["event"], "message");
    assert_eq!(chat["cut"], "component");
    let link = link_to(&payload, &chat["id"]);
    assert_eq!(link["kind"], "event");
    assert!(
        link["label"].as_str().unwrap().contains("from the server"),
        "{link}"
    );
}
