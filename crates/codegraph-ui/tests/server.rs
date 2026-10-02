//! The loopback boundary and the embedded viewer — a port of upstream
//! `__tests__/ui-server.test.ts` (`v1.6.1`), against the bundle compiled into
//! this crate rather than a stand-in viewer directory.

mod support;

use axum::http::StatusCode;
use support::{PORT, Project};

fn project() -> Project {
    let project = Project::new(&[("src/auth.ts", "export const token = 1;\n")]);
    // A file OUTSIDE the project that a traversal would be trying to reach.
    std::fs::write(
        project.root().parent().unwrap().join("secret.txt"),
        "SUPER-SECRET-VALUE\n",
    )
    .unwrap();
    project
}

/// A hashed script the shell references, read out of the shell itself.
fn bundled_script() -> String {
    let index = String::from_utf8(
        codegraph_ui::assets::viewer_file("index.html")
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let start = index
        .find("./assets/")
        .expect("the shell loads a hashed asset")
        + 2;
    let end = start + index[start..].find('"').unwrap();
    index[start..end].to_string()
}

/* ----------------------------------------------------- serving the viewer -- */

#[tokio::test]
async fn serves_index_html_at_the_root() {
    let project = project();
    let reply = project.viewer().get("/").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(reply.text().contains(r#"<div id="app">"#));
}

#[tokio::test]
async fn serves_index_html_directly_too() {
    let project = project();
    let reply = project.viewer().get("/index.html").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.text().contains(r#"<div id="app">"#));
}

#[tokio::test]
async fn serves_hashed_assets_with_their_real_content_type() {
    let project = project();
    let viewer = project.viewer();
    let script = bundled_script();
    let js = viewer.get(&format!("/{script}")).await;
    assert_eq!(js.status, StatusCode::OK);
    assert_eq!(
        js.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    let css = codegraph_ui::assets::VIEWER_FILES
        .iter()
        .find(|(path, _)| path.starts_with("assets/") && path.ends_with(".css"))
        .map(|(path, _)| *path)
        .expect("the bundle carries a stylesheet");
    let css = viewer.get(&format!("/{css}")).await;
    assert_eq!(css.status, StatusCode::OK);
    assert_eq!(css.header("content-type"), Some("text/css; charset=utf-8"));
}

#[tokio::test]
async fn caches_hashed_assets_forever_and_index_html_never() {
    let project = project();
    let viewer = project.viewer();
    let asset = viewer.get(&format!("/{}", bundled_script())).await;
    assert_eq!(
        asset.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(
        viewer.get("/").await.header("cache-control"),
        Some("no-store")
    );
}

#[tokio::test]
async fn falls_back_to_index_html_for_an_unknown_route_but_not_for_a_missing_asset() {
    let project = project();
    let viewer = project.viewer();
    let route = viewer.get("/s/some-symbol-id").await;
    assert_eq!(route.status, StatusCode::OK);
    assert!(route.text().contains(r#"<div id="app">"#));
    assert_eq!(
        viewer.get("/assets/index-doesnotexist.js").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn answers_head_with_the_same_headers_and_no_body() {
    let project = project();
    let reply = project.viewer().request("HEAD", "/", &[], None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(reply.header("content-length").is_some());
    assert!(reply.body.is_empty());
}

/* ---------------------------------------------------------------- binding -- */

#[tokio::test]
async fn listens_on_loopback_only() {
    let project = project();
    let server = codegraph_ui::start(codegraph_ui::UiOptions {
        project_root: project.root().to_path_buf(),
        port: Some(0),
        read_only: false,
    })
    .await
    .unwrap();
    assert_eq!(server.url, format!("http://127.0.0.1:{}", server.port));
    server.close().await;
}

#[tokio::test]
async fn falls_back_to_the_next_free_port_when_the_preferred_one_is_taken() {
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken = blocker.local_addr().unwrap().port();
    let listener = codegraph_ui::server::bind_ports(taken, 20, true)
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_ne!(port, taken);
    assert!(port > taken);
}

#[tokio::test]
async fn refuses_to_move_off_a_port_the_caller_pinned() {
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken = blocker.local_addr().unwrap().port();
    let err = codegraph_ui::server::bind(Some(taken)).await.unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("already in use"),
        "{err}"
    );
}

/* -------------------------------------------- Host allowlist (rebinding) -- */

#[tokio::test]
async fn serves_the_loopback_names() {
    let project = project();
    let viewer = project.viewer();
    for host in [
        "127.0.0.1".to_string(),
        "localhost".to_string(),
        "[::1]".to_string(),
        format!("localhost:{PORT}"),
        format!("[::1]:{PORT}"),
    ] {
        let request = axum::http::Request::builder()
            .uri("/")
            .header("host", &host)
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            viewer.send(request).await.status(),
            StatusCode::OK,
            "Host: {host}"
        );
    }
}

async fn status_with_host(
    viewer: &support::Viewer,
    path: &str,
    host: &str,
) -> (StatusCode, String) {
    use http_body_util::BodyExt;
    let request = axum::http::Request::builder()
        .uri(path)
        .header("host", host)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = viewer.send(request).await;
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn refuses_a_foreign_host() {
    let project = project();
    let viewer = project.viewer();
    for host in [
        "evil.example".to_string(),
        format!("evil.example:{PORT}"),
        "attacker.localhost.evil.com".to_string(),
    ] {
        let (status, body) = status_with_host(&viewer, "/", &host).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "Host: {host}");
        assert!(!body.contains(r#"<div id="app">"#));
    }
}

#[tokio::test]
async fn refuses_a_loopback_host_carrying_someone_elses_port() {
    let project = project();
    let (status, _) = status_with_host(&project.viewer(), "/", "127.0.0.1:9").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn refuses_a_malformed_or_missing_host() {
    let project = project();
    let viewer = project.viewer();
    let (malformed, _) = status_with_host(&viewer, "/", "127.0.0.1:notaport").await;
    assert_eq!(malformed, StatusCode::FORBIDDEN);
    let request = axum::http::Request::builder()
        .uri("/")
        .body(axum::body::Body::empty())
        .unwrap();
    assert_eq!(viewer.send(request).await.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn refuses_before_touching_the_filesystem_even_for_an_asset() {
    let project = project();
    let (status, body) = status_with_host(
        &project.viewer(),
        &format!("/{}", bundled_script()),
        "evil.example",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.starts_with("Refused"));
}

/* ----------------------------------------------------------- cross-origin -- */

#[tokio::test]
async fn never_sends_cors_headers() {
    let project = project();
    let reply = project.viewer().get("/").await;
    for header in [
        "access-control-allow-origin",
        "access-control-allow-credentials",
        "access-control-allow-methods",
    ] {
        assert!(reply.header(header).is_none(), "{header}");
    }
}

#[tokio::test]
async fn refuses_a_request_carrying_a_foreign_origin() {
    let project = project();
    let reply = project
        .viewer()
        .request("GET", "/", &[("origin", "https://evil.example")], None)
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn allows_the_viewers_own_origin() {
    let project = project();
    let origin = format!("http://127.0.0.1:{PORT}");
    let reply = project
        .viewer()
        .request("GET", "/", &[("origin", &origin)], None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn sends_the_hardening_headers_on_every_response() {
    let project = project();
    let viewer = project.viewer();
    for path in ["/", "/api/nope", "/assets/missing.js"] {
        let reply = viewer.get(path).await;
        assert_eq!(
            reply.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
        assert_eq!(reply.header("x-frame-options"), Some("DENY"));
        let csp = reply.header("content-security-policy").unwrap();
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("connect-src 'self'"));
    }
}

/* ---------------------------------------------------------------- methods -- */

#[tokio::test]
async fn refuses_every_method_it_has_never_answered() {
    let project = project();
    let viewer = project.viewer();
    for method in ["PUT", "PATCH", "OPTIONS", "TRACE"] {
        let reply = viewer.request(method, "/", &[], None).await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(reply.header("allow"), Some("GET, HEAD, POST, DELETE"));
    }
}

#[tokio::test]
async fn refuses_a_write_outside_api_whatever_it_carries() {
    let project = project();
    let viewer = project.viewer();
    for method in ["POST", "DELETE"] {
        let reply = viewer
            .request(method, "/", &[("x-codegraph-ui", "1")], None)
            .await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(reply.header("allow"), Some("GET, HEAD"));
    }
}

#[tokio::test]
async fn refuses_an_unmarked_write_under_api_as_json() {
    let project = project();
    let reply = project
        .viewer()
        .request("POST", "/api/trails", &[], None)
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert!(
        reply
            .header("content-type")
            .unwrap()
            .contains("application/json")
    );
    assert_eq!(reply.json()["code"], "refused");
}

/* ------------------------------------------------ paths outside the root -- */

#[tokio::test]
async fn never_serves_a_file_outside_the_viewer_directory() {
    let project = project();
    let viewer = project.viewer();
    for traversal in [
        "/../secret.txt",
        "/../../secret.txt",
        "/assets/../../secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e/secret.txt",
        "/%2e%2e%2fsecret.txt",
        "/....//secret.txt",
    ] {
        let reply = viewer.get(traversal).await;
        assert!(!reply.text().contains("SUPER-SECRET-VALUE"), "{traversal}");
        assert_ne!(reply.status, StatusCode::OK, "{traversal}");
    }
}

#[tokio::test]
async fn _404s_an_absolute_system_path_rather_than_reading_it() {
    let project = project();
    let viewer = project.viewer();
    let reply = viewer.get("/etc/passwd").await;
    assert!(!reply.text().contains("root:"));
    // No such file in the bundle; an extension-less path is a route.
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.text().contains(r#"<div id="app">"#));
    assert_eq!(
        viewer.get("/etc/hosts.txt").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn _404s_a_nul_truncation_attempt() {
    let project = project();
    assert_eq!(
        project.viewer().get("/index.html%00.png").await.status,
        StatusCode::NOT_FOUND
    );
}

/* -------------------------------------------------------- /api is reserved -- */

#[tokio::test]
async fn _404s_as_json_never_as_the_app_shell() {
    let project = project();
    let reply = project.viewer().get("/api/no-such-endpoint").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
        reply.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert!(reply.json().get("error").is_some());
    assert!(!reply.text().contains(r#"<div id="app">"#));
}

#[tokio::test]
async fn a_route_with_no_argument_says_what_it_wants() {
    let project = project();
    let viewer = project.viewer();
    for (path, wanted) in [
        ("/api/node", "/api/node/<id>"),
        ("/api/file", "/api/file/<path>"),
        ("/api/filecode", "/api/filecode/<path>"),
    ] {
        let reply = viewer.get(path).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{path}");
        assert!(
            reply.json()["error"].as_str().unwrap().contains(wanted),
            "{path}"
        );
    }
}
