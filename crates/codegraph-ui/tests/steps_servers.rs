//! `GET /api/steps` on servers — an Express API, a NestJS API, a FastAPI
//! service, an ASP.NET group and a Spring controller in one indexed fixture
//! (`fixtures::STEPS_SERVERS`): a port of upstream
//! `__tests__/ui-steps-api-servers.test.ts:312-582` at `e8ddc4e2`.

mod fixtures;
mod steps_support;
mod support;

use serde_json::{Value, json};
use steps_support::{
    anchor_of, anchored, effect, find_effect, in_order, items, link_to, steps, via,
};
use support::{Project, encode};

fn servers() -> Project {
    Project::indexed(fixtures::STEPS_SERVERS)
}

fn replies(payload: &Value) -> Vec<&Value> {
    items(&payload["steps"])
        .iter()
        .filter(|step| step["kind"] == "effect" && step["effect"]["category"] == "response")
        .collect()
}

// ------------------------------------------------------ in the code's order --

#[tokio::test]
async fn reads_an_express_handler_as_it_is_written_helper_and_all() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /users", "").await;
    assert_eq!(payload["defaultView"], "order");
    assert_eq!(
        in_order(&payload),
        [
            "prisma.user.create({ data })",
            "emailQueue.add('welcome', { userId })",
            "if !user.verified",
            "  !user.verified",
            "    inline sendVerification",
            "      transporter.sendMail({ to })",
            "inline signToken",
            "  jwt.sign({ id }, process.env.JWT_SECRET, { expiresIn })",
            "201",
        ],
        "{payload}"
    );
}

#[tokio::test]
async fn reads_a_python_handler_its_raise_ending_the_arm_it_is_in() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /items", "").await;
    assert_eq!(
        in_order(&payload),
        [
            "session.add +1",
            "session.add +1",
            "if item.price < 0",
            "  item.price < 0 → reply",
            "    422",
            "  !(item.price < 0)",
            "    send_welcome.delay(item.id)",
        ],
        "{payload}"
    );
}

#[tokio::test]
async fn reads_a_java_handler_its_early_return_as_the_forks_other_arm() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /owners/new", "").await;
    assert_eq!(
        in_order(&payload),
        [
            "if owner.getName() == null",
            "  owner.getName() == null → reply",
            "    400",
            "  owner.getName() != null → reply",
            "    owners.save(owner)",
            "    201",
        ],
        "{payload}"
    );
}

#[tokio::test]
async fn reads_a_csharp_handlers_two_outcomes() {
    let project = servers();
    let payload = anchored(&project.viewer(), "PUT /api/TodoItems/{id}", "").await;
    assert_eq!(
        in_order(&payload),
        [
            "if id != command.Id",
            "  id != command.Id → reply",
            "    400",
            "  id == command.Id → reply",
            "    204",
        ],
        "{payload}"
    );
}

#[tokio::test]
async fn reads_a_nest_handler_through_the_service_it_delegates_to() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /cats", "").await;
    assert_eq!(
        in_order(&payload),
        [
            "inline create",
            "  this.catsRepository.save(dto)",
            "  handleIndex"
        ],
        "{payload}"
    );
}

#[tokio::test]
async fn has_no_order_to_read_for_a_symbol_nothing_happens_inside() {
    let project = servers();
    let payload = steps(&project.viewer(), "symbol=RolesGuard").await;
    assert_eq!(payload["program"], Value::Null, "{payload}");
    assert_eq!(payload["defaultView"], "tree");
}

// --------------------------------------------------------------- route roots --

/// Upstream `routeRoots` and `projectKind`, read through the routes list and
/// the picture.
#[tokio::test]
async fn names_the_handler_an_api_route_runs_and_calls_the_project_an_api() {
    let project = servers();
    let viewer = project.viewer();
    let routes = viewer.json("/api/routes?limit=500").await;
    let entry = |url: &str| {
        items(&routes["entries"])
            .iter()
            .find(|entry| entry["url"] == url)
            .unwrap_or_else(|| panic!("no route {url}: {routes}"))
            .clone()
    };
    assert_eq!(entry("POST /users")["handler"], "createUser");
    assert_eq!(entry("POST /users")["inline"], false);
    assert_eq!(entry("GET /users/:id")["inline"], true);
    let cats = entry("POST /cats");
    let handler = viewer
        .json(&format!(
            "/api/node/{}",
            encode(cats["handlerId"].as_str().unwrap())
        ))
        .await;
    assert!(
        handler["node"]["qualifiedName"]
            .as_str()
            .unwrap()
            .contains("CatsController"),
        "{handler}"
    );
    assert_eq!(entry("POST /items")["handler"], "create_item");
    assert_eq!(entry("POST /owners/new")["handler"], "create");
    let payload = anchored(&viewer, "POST /users", "").await;
    assert_eq!(payload["project"], "api");
}

// ------------------------------------------------------------------- Express --

#[tokio::test]
async fn express_draws_the_write_the_job_the_email_and_the_201_after_the_middleware() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /users", "").await;
    assert_eq!(payload["project"], "api");
    let anchor = anchor_of(&payload);
    assert_eq!(anchor["kind"], "screen");
    assert_eq!(anchor["sub"], "createUser");
    assert_eq!(anchor["screen"]["path"], "POST /users");
    assert_eq!(anchor["screen"]["endpoint"], true);
    assert_eq!(anchor["screen"]["inline"], false);
    assert_eq!(anchor["screen"]["component"]["name"], "createUser");
    assert_eq!(
        anchor["trigger"],
        json!({ "kind": "request", "name": "POST", "of": "/users", "after": ["authenticate", "validate(…)"], "in": "users.routes.ts" })
    );
    let db = effect(&payload, "database");
    assert_eq!(db["label"], "prisma.user.create({ data })");
    assert_eq!(db["effect"]["model"], "user");
    assert_eq!(db["effect"]["access"], "write");
    assert_eq!(db["effect"]["by"]["name"], "createUser");
    assert_eq!(db["sub"], "database · user · write · createUser");
    assert_eq!(
        effect(&payload, "queue")["label"],
        "emailQueue.add('welcome', { userId })"
    );
    let mail = effect(&payload, "email");
    assert_eq!(mail["label"], "transporter.sendMail({ to })");
    let mail_link = link_to(&payload, &mail["id"]);
    assert_eq!(via(mail_link), ["sendVerification"]);
    assert_eq!(mail_link["when"], "!user.verified");
    let reply = effect(&payload, "response");
    assert_eq!(reply["label"], "201");
    assert_eq!(reply["effect"]["statuses"], json!([201]));
    let reply_link = link_to(&payload, &reply["id"]);
    assert_eq!(reply_link["sites"][0]["text"], "res.status(201).json");
    assert_eq!(reply_link["sites"][0]["args"], "{ id, token }");
    assert_eq!(reply_link["sites"][0]["status"], 201);
    let auth = effect(&payload, "auth");
    assert_eq!(
        auth["label"],
        "jwt.sign({ id }, process.env.JWT_SECRET, { expiresIn })"
    );
    let auth_link = link_to(&payload, &auth["id"]);
    assert_eq!(via(auth_link), ["signToken"]);
    assert_eq!(auth_link["within"], "res.status(201).json");
    assert!(reply_link.get("within").is_none(), "{reply_link}");
    let mut row: Vec<&Value> = items(&payload["steps"])
        .iter()
        .filter(|step| step["depth"] == 1)
        .collect();
    row.sort_by_key(|step| step["order"].as_u64().unwrap());
    let row: Vec<&str> = row
        .iter()
        .map(|step| step["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        row,
        [
            "prisma.user.create({ data })",
            "emailQueue.add('welcome', { userId })",
            "transporter.sendMail({ to })",
            "jwt.sign({ id }, process.env.JWT_SECRET, { expiresIn })",
            "201",
        ]
    );
}

#[tokio::test]
async fn express_walks_an_inline_handler_as_the_route_itself_into_the_read_and_its_404() {
    let project = servers();
    let payload = anchored(&project.viewer(), "GET /users/:id", "").await;
    let anchor = anchor_of(&payload);
    assert_eq!(anchor["sub"], "inline handler · users.routes.ts");
    assert_eq!(anchor["trigger"]["kind"], "request");
    assert_eq!(anchor["trigger"]["name"], "GET");
    assert_eq!(anchor["trigger"]["of"], "/users/:id");
    assert_eq!(anchor["trigger"]["after"], json!(["authenticate"]));
    let db = effect(&payload, "database");
    assert_eq!(db["effect"]["model"], "user");
    assert_eq!(db["effect"]["access"], "read");
    assert_eq!(db["effect"]["by"]["name"], "getUser");
    let replies = replies(&payload);
    let mut outcomes: Vec<(String, String)> = replies
        .iter()
        .map(|step| {
            (
                step["effect"]["by"]["name"].as_str().unwrap().to_string(),
                step["label"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [
            ("GET /users/:id".to_string(), "200".to_string()),
            ("getUser".to_string(), "404".to_string()),
        ]
    );
    let own = replies
        .iter()
        .find(|step| step["effect"]["by"]["name"] == "GET /users/:id")
        .unwrap();
    let own_site = &link_to(&payload, &own["id"])["sites"][0];
    assert_eq!(own_site["text"], "res.json");
    assert_eq!(own_site["args"], "user");
    assert_eq!(own_site["status"], 200);
    let not_found = replies
        .iter()
        .find(|step| step["effect"]["by"]["name"] == "getUser")
        .unwrap();
    let not_found_link = link_to(&payload, &not_found["id"]);
    assert_eq!(not_found_link["sites"][0]["text"], "NotFoundError");
    assert_eq!(not_found_link["sites"][0]["status"], 404);
    assert_eq!(not_found_link["sites"][0]["when"], "!user");
    assert_eq!(via(not_found_link), ["getUser"]);
}

#[tokio::test]
async fn express_a_status_set_by_the_statement_before_the_reply_is_that_replys() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /users/:id/accept", "").await;
    let reply = effect(&payload, "response");
    assert_eq!(reply["label"], "202");
    let sites: Vec<(Value, Value)> = items(&link_to(&payload, &reply["id"])["sites"])
        .iter()
        .map(|site| (site["text"].clone(), site["status"].clone()))
        .collect();
    assert_eq!(
        sites,
        [
            (json!("res.status"), json!(202)),
            (json!("res.json"), json!(202)),
        ]
    );
}

// -------------------------------------------------------------------- NestJS --

#[tokio::test]
async fn nest_reads_the_guards_follows_di_into_the_repository_and_the_queue() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /cats", "").await;
    let anchor = anchor_of(&payload);
    assert_eq!(anchor["sub"], "create");
    assert_eq!(
        anchor["trigger"],
        json!({ "kind": "request", "name": "POST", "of": "/cats", "after": ["UseGuards(AuthGuard('jwt'))", "UseGuards(RolesGuard)"], "in": "cats.controller.ts" })
    );
    let db = effect(&payload, "database");
    assert_eq!(db["label"], "this.catsRepository.save(dto)");
    assert_eq!(db["effect"]["model"], "cats");
    assert_eq!(db["effect"]["access"], "write");
    assert_eq!(db["effect"]["by"]["name"], "create");
    assert_eq!(via(link_to(&payload, &db["id"])), ["create"]);
    assert!(find_effect(&payload, "queue").is_none(), "{payload}");
    let landing = items(&payload["steps"])
        .iter()
        .find(|step| step["kind"] == "event" && step["node"]["name"] == "handleIndex")
        .unwrap_or_else(|| panic!("{payload}"));
    assert_eq!(landing["event"], "index");
    assert_eq!(
        landing["trigger"],
        json!({ "kind": "decorator", "name": "Process", "of": "'index'", "in": "cats.processor.ts" })
    );
    let to_landing = link_to(&payload, &landing["id"]);
    assert_eq!(to_landing["kind"], "event");
    assert_eq!(to_landing["synthesized"], true);
    assert_eq!(to_landing["sites"][0]["text"], "this.catsQueue.add");
    assert_eq!(to_landing["sites"][0]["args"], "'index', { id }");
    assert_eq!(
        to_landing["label"],
        "via queue-job · job index · queue cats · registered at src/nest/cats.processor.ts:4"
    );
}

#[tokio::test]
async fn nest_a_thrown_exception_is_the_404_the_request_gets() {
    let project = servers();
    let payload = anchored(&project.viewer(), "GET /cats/:id", "").await;
    let reply = effect(&payload, "response");
    assert_eq!(reply["label"], "404");
    let site = &link_to(&payload, &reply["id"])["sites"][0];
    assert_eq!(site["text"], "NotFoundException");
    assert_eq!(site["args"], "'no cat'");
    assert_eq!(site["status"], 404);
    assert_eq!(site["when"], "!cat");
    assert_eq!(effect(&payload, "database")["effect"]["access"], "read");
}

#[tokio::test]
async fn nest_a_queue_consumer_says_the_job_that_fires_it() {
    let project = servers();
    let payload = steps(&project.viewer(), "symbol=handleIndex").await;
    assert_eq!(
        anchor_of(&payload)["trigger"],
        json!({ "kind": "decorator", "name": "Process", "of": "'index'", "in": "cats.processor.ts" })
    );
}

// ------------------------------------------------------------------- FastAPI --

#[tokio::test]
async fn fastapi_names_a_mounted_routers_routes_by_the_path_a_request_takes() {
    let project = servers();
    let viewer = project.viewer();
    let routes = viewer.json("/api/routes?limit=500").await;
    let names: Vec<&str> = items(&routes["entries"])
        .iter()
        .map(|entry| entry["url"].as_str().unwrap())
        .collect();
    for name in [
        "GET /api/v1/orders",
        "GET /api/v1/orders/{order_id}",
        "POST /items",
    ] {
        assert!(names.contains(&name), "{name}: {names:?}");
    }
    assert!(!names.contains(&"GET /"), "{names:?}");
}

#[tokio::test]
async fn fastapi_reads_the_dependency_the_session_writes_the_422_and_the_celery_task() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /items", "").await;
    let anchor = anchor_of(&payload);
    assert_eq!(anchor["sub"], "create_item");
    assert_eq!(
        anchor["trigger"],
        json!({ "kind": "request", "name": "POST", "of": "/items", "after": ["Depends(get_current_user)"], "in": "items.py" })
    );
    let db = effect(&payload, "database");
    assert_eq!(
        db["effect"]["apis"],
        json!(["session.add", "session.commit"])
    );
    assert_eq!(db["effect"]["access"], "write");
    let reply = effect(&payload, "response");
    assert_eq!(reply["label"], "422");
    let site = &link_to(&payload, &reply["id"])["sites"][0];
    assert_eq!(site["text"], "HTTPException");
    assert_eq!(site["args"], "status_code=422, detail=\"bad price\"");
    assert_eq!(site["status"], 422);
    assert_eq!(site["when"], "item.price < 0");
    assert_eq!(
        effect(&payload, "queue")["label"],
        "send_welcome.delay(item.id)"
    );
}

// ------------------------------------------------------ ASP.NET endpoint groups --

#[tokio::test]
async fn aspnet_names_the_groups_routes_and_starts_the_walk_at_the_handler() {
    let project = servers();
    let viewer = project.viewer();
    let routes = viewer.json("/api/routes?limit=500").await;
    let names: Vec<&str> = items(&routes["entries"])
        .iter()
        .map(|entry| entry["url"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"POST /api/TodoItems"), "{names:?}");
    assert!(names.contains(&"PUT /api/TodoItems/{id}"), "{names:?}");
    let payload = anchored(&viewer, "PUT /api/TodoItems/{id}", "").await;
    let anchor = anchor_of(&payload);
    assert_eq!(anchor["sub"], "UpdateTodoItem");
    assert_eq!(anchor["trigger"]["kind"], "request");
    assert_eq!(anchor["trigger"]["name"], "PUT");
    assert_eq!(anchor["trigger"]["of"], "/api/TodoItems/{id}");
    let mut outcomes: Vec<(String, String)> = replies(&payload)
        .iter()
        .map(|step| {
            (
                step["label"].as_str().unwrap().to_string(),
                link_to(&payload, &step["id"])["when"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .collect();
    outcomes.sort();
    assert_eq!(
        outcomes,
        [
            ("204".to_string(), "id == command.Id".to_string()),
            ("400".to_string(), "id != command.Id".to_string()),
        ]
    );
}

// -------------------------------------------------------------------- Spring --

#[tokio::test]
async fn spring_types_the_repository_off_the_field_reads_the_guard_and_both_replies() {
    let project = servers();
    let payload = anchored(&project.viewer(), "POST /owners/new", "").await;
    let anchor = anchor_of(&payload);
    assert_eq!(anchor["sub"], "create");
    assert_eq!(
        anchor["trigger"],
        json!({ "kind": "request", "name": "POST", "of": "/owners/new", "after": ["PreAuthorize(\"hasRole('ADMIN')\")"], "in": "OwnerController.java" })
    );
    let db = effect(&payload, "database");
    assert_eq!(db["label"], "owners.save(owner)");
    assert_eq!(db["effect"]["model"], "Owner");
    assert_eq!(db["effect"]["access"], "write");
    assert_eq!(
        link_to(&payload, &db["id"])["when"],
        "owner.getName() != null"
    );
    let mut outcomes: Vec<(String, Value, String)> = replies(&payload)
        .iter()
        .map(|step| {
            (
                step["label"].as_str().unwrap().to_string(),
                step["effect"]["statuses"].clone(),
                link_to(&payload, &step["id"])["when"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .collect();
    outcomes.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        outcomes,
        [
            (
                "201".to_string(),
                json!([201]),
                "owner.getName() != null".to_string()
            ),
            (
                "400".to_string(),
                json!([400]),
                "owner.getName() == null".to_string()
            ),
        ]
    );
}

/// The same server fixture indexed afresh, and indexed then edited and put
/// back, draws the same endpoint picture byte for byte.
#[tokio::test]
async fn an_endpoints_picture_does_not_depend_on_how_the_index_was_built() {
    let fresh = servers();
    let synced = servers();
    let service = fixtures::STEPS_SERVERS
        .iter()
        .find(|(path, _)| *path == "src/server/users.service.ts")
        .unwrap()
        .1;
    synced.write(
        "src/server/users.service.ts",
        &format!("{service}\nexport const moved = 1\n"),
    );
    synced.index();
    synced.write("src/server/users.service.ts", service);
    synced.index();
    let stable = |mut payload: Value| {
        payload.as_object_mut().unwrap().remove("timing");
        payload["index"]
            .as_object_mut()
            .unwrap()
            .remove("lastIndexedAt");
        serde_json::to_string_pretty(&payload).unwrap()
    };
    let a = anchored(&fresh.viewer(), "POST /users", "").await;
    let b = anchored(&synced.viewer(), "POST /users", "").await;
    assert_eq!(stable(a), stable(b));
}
