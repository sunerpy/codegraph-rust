//! How a server-rendered endpoint answers, in the Steps picture — a port of
//! upstream `__tests__/steps-server-rendered-replies.test.ts:91-233` and
//! `__tests__/swiftui-viewer-noise.test.ts:139-160` at `e8ddc4e2`: a Spring
//! MVC handler answers by what it returns, a Laravel controller by `view()`
//! and `redirect()`, and a project type shadows a library name.

mod fixtures;
mod steps_support;
mod support;

use codegraph_core::types::Language;
use codegraph_ui::api::effects::{
    CallKind, EffectInput, ProjectKind, classify_effect, implicit_response_status, response_status,
};
use serde_json::Value;
use steps_support::{anchored, items, steps};
use support::{Project, Viewer, encode};

/// Each reply the route's picture draws, as `status · text · when`
/// (upstream's `replies`).
async fn replies(viewer: &Viewer, route: &str) -> Vec<String> {
    let payload = anchored(viewer, route, "").await;
    let reply_ids: Vec<&Value> = items(&payload["steps"])
        .iter()
        .filter(|step| step["effect"]["category"] == "response")
        .map(|step| &step["id"])
        .collect();
    let mut out: Vec<String> = items(&payload["links"])
        .iter()
        .filter(|link| reply_ids.contains(&&link["to"]))
        .flat_map(|link| items(&link["sites"]).iter())
        .map(|site| {
            let status = site["status"]
                .as_u64()
                .map_or_else(|| "—".to_string(), |status| status.to_string());
            let when = site["when"]
                .as_str()
                .filter(|when| !when.is_empty())
                .map(|when| format!(" · WHEN {when}"))
                .unwrap_or_default();
            format!("{status} · {}{when}", site["text"].as_str().unwrap())
        })
        .collect();
    out.sort();
    out
}

fn statuses(replies: &[String]) -> Vec<&str> {
    replies
        .iter()
        .map(|reply| reply.split(" · ").next().unwrap())
        .collect()
}

/// `kind name` of each symbol a method calls, from its Symbol view's right
/// rail (upstream's `getCallees`).
async fn callees(viewer: &Viewer, method: &str) -> Vec<String> {
    let id = viewer.id_of(method, Some("method")).await;
    let node = viewer.json(&format!("/api/node/{}", encode(&id))).await;
    let mut out: Vec<String> = items(&node["outgoing"]["items"])
        .iter()
        .filter(|row| {
            items(&row["edges"])
                .iter()
                .any(|edge| edge["kind"] == "calls")
        })
        .map(|row| {
            format!(
                "{} {}",
                row["node"]["kind"].as_str().unwrap(),
                row["node"]["name"].as_str().unwrap()
            )
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn spring_mvc_draws_the_view_it_renders_and_the_redirect_it_sends_each_under_its_condition() {
    let project = Project::indexed(fixtures::REPLIES_SPRING);
    let viewer = project.viewer();
    assert_eq!(
        replies(&viewer, "POST /owners/new").await,
        [
            "200 · view owners/createOrUpdateOwnerForm · WHEN result.hasErrors()",
            "302 · redirect:/owners/${…} · WHEN !result.hasErrors()",
        ]
    );
    assert_eq!(
        statuses(&replies(&viewer, "GET /owners/{ownerId}").await),
        ["200"]
    );
    assert_eq!(
        statuses(&replies(&viewer, "GET /owners/legacy").await),
        ["302"]
    );
    // A @RestController's String is its body, not a view name.
    assert!(replies(&viewer, "GET /status").await.is_empty());
}

#[tokio::test]
async fn laravel_draws_the_page_a_controller_renders_and_the_redirect_and_binds_neither_to_a_same_named_member()
 {
    let project = Project::indexed(fixtures::REPLIES_LARAVEL);
    let viewer = project.viewer();
    // PHP has no condition rules yet, so the replies carry no WHEN.
    assert_eq!(
        replies(&viewer, "GET /books/{slug}").await,
        ["200 · view", "302 · redirect"]
    );
    assert_eq!(statuses(&replies(&viewer, "POST /books").await), ["302"]);
    let bound = callees(&viewer, "show").await;
    for wrong in ["method redirect", "field view", "class View"] {
        assert!(
            !bound.iter().any(|callee| callee == wrong),
            "{wrong}: {bound:?}"
        );
    }
}

#[tokio::test]
async fn laravel_still_binds_this_method_and_a_project_helper_function() {
    let project = Project::indexed(fixtures::REPLIES_LARAVEL_BINDS);
    let viewer = project.viewer();
    assert_eq!(
        callees(&viewer, "show").await,
        ["function format_title", "method setTitle"]
    );
}

// ------------------------------------------------------- reply classification --

fn php(text: &str) -> Option<&'static str> {
    classify_effect(&EffectInput {
        text,
        kind: CallKind::Calls,
        language: Some(Language::Php),
        project: ProjectKind::Api,
        receiver_type: None,
        args: None,
        project_type: false,
    })
    .map(|effect| effect.category.as_str())
}

fn status(text: &str, args: Option<&str>) -> Option<u16> {
    response_status(text, args).or_else(|| implicit_response_status(text))
}

#[test]
fn reads_laravels_reply_chains_with_their_argument_lists_stripped_and_their_statuses() {
    for text in [
        "view",
        "redirect",
        "redirect()->route",
        "redirect()->back()->withErrors",
        "back",
        "to_route",
        "response()->json",
        "response()->noContent",
    ] {
        assert_eq!(php(text), Some("response"), "{text}");
    }
    let redirects: Vec<Option<u16>> = [
        "redirect()->route",
        "redirect()->back()->withErrors",
        "back()->with",
        "to_route",
    ]
    .into_iter()
    .map(|text| status(text, None))
    .collect();
    assert_eq!(redirects, [Some(302); 4]);
    assert_eq!(status("view", None), Some(200));
    assert_eq!(status("response()->json", None), Some(200));
    assert_eq!(php("$this->view"), None);
}

#[test]
fn keeps_phps_programming_error_exceptions_out_of_the_replies_and_namespaced_http_ones_in() {
    let thrown = |text: &str| {
        classify_effect(&EffectInput {
            text,
            kind: CallKind::Instantiates,
            language: Some(Language::Php),
            project: ProjectKind::Api,
            receiver_type: None,
            args: None,
            project_type: false,
        })
        .map(|effect| effect.category.as_str())
    };
    assert_eq!(thrown("InvalidArgumentException"), None);
    assert_eq!(thrown("\\InvalidArgumentException"), None);
    assert_eq!(thrown("\\LogicException"), None);
    assert_eq!(thrown("NotFoundException"), Some("response"));
    assert_eq!(
        thrown("\\Symfony\\Component\\HttpKernel\\Exception\\NotFoundHttpException"),
        Some("response")
    );
}

#[test]
fn reads_springs_model_and_view_and_redirect_view() {
    let jvm = |text: &str| {
        classify_effect(&EffectInput {
            text,
            kind: CallKind::Instantiates,
            language: Some(Language::Java),
            project: ProjectKind::Api,
            receiver_type: None,
            args: None,
            project_type: false,
        })
        .map(|effect| effect.category.as_str())
    };
    assert_eq!(jvm("ModelAndView"), Some("response"));
    assert_eq!(jvm("RedirectView"), Some("response"));
    assert_eq!(
        status("ModelAndView", Some("\"owners/ownerDetails\"")),
        Some(200)
    );
    assert_eq!(
        status("ModelAndView", Some("\"redirect:/owners\"")),
        Some(302)
    );
    assert_eq!(status("RedirectView", Some("\"/owners\"")), Some(302));
}

// ------------------------------------------------------------------ SwiftUI --

/// `swiftui-viewer-noise.test.ts:139-160`: a call through the project's own
/// `Notifications` type is no device call.
#[tokio::test]
async fn a_call_through_the_projects_own_notifications_type_is_no_device_call() {
    let project = Project::indexed(fixtures::SWIFTUI_NOTIFICATIONS);
    let viewer = project.viewer();
    let id = viewer.id_of("refresh", None).await;
    let payload = steps(&viewer, &format!("anchor={}", encode(&id))).await;
    assert!(
        !items(&payload["steps"])
            .iter()
            .any(|step| step["effect"]["category"] == "device"),
        "{payload}"
    );
}
