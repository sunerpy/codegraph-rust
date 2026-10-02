//! Shared harness for the viewer's integration tests: a fixture project
//! indexed with the real pipeline into a temp dir, and the viewer's router
//! driven without a socket (`tower::ServiceExt::oneshot`), with the loopback
//! `Host` the boundary wants.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use codegraph_ui::UiOptions;
use codegraph_ui::server::AppState;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// The port the boundary checks compare against. No socket is bound.
pub const PORT: u16 = 47_470;

/// A project on disk.
pub struct Project {
    dir: tempfile::TempDir,
    root: PathBuf,
}

impl Project {
    pub fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("codegraph-ui-test-")
            .tempdir()
            .expect("temp dir");
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = Self { dir, root };
        for (path, text) in files {
            project.write(path, text);
        }
        project
    }

    /// A project indexed with the real pipeline (a full sync of a fresh
    /// project builds the index, byte-identical to `index --force`).
    pub fn indexed(files: &[(&str, &str)]) -> Self {
        let project = Self::new(files);
        project.index();
        project
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub fn index(&self) {
        codegraph_watch::sync_project_once(&self.root).expect("index the fixture");
    }

    pub fn viewer(&self) -> Viewer {
        Viewer::open(&self.root, false)
    }

    pub fn read_only_viewer(&self) -> Viewer {
        Viewer::open(&self.root, true)
    }
}

/// The viewer's router over one project.
pub struct Viewer {
    pub state: Arc<AppState>,
    router: axum::Router,
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

impl Viewer {
    pub fn open(root: &Path, read_only: bool) -> Self {
        let options = UiOptions {
            project_root: root.to_path_buf(),
            port: Some(PORT),
            read_only,
        };
        let state = codegraph_ui::app_state(&options, PORT).expect("viewer state");
        let router = codegraph_ui::server::router(state.clone());
        Self { state, router }
    }

    /// The raw response, for a streaming body.
    pub async fn send(&self, request: Request<Body>) -> Response {
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("router answers")
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Reply {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", format!("127.0.0.1:{PORT}"));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let body = body
            .map(|b| Body::from(b.to_vec()))
            .unwrap_or_else(Body::empty);
        let response = self.send(builder.body(body).unwrap()).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = tokio::time::timeout(Duration::from_secs(60), response.into_body().collect())
            .await
            .expect("a body within a minute")
            .expect("body")
            .to_bytes()
            .to_vec();
        Reply {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, path: &str) -> Reply {
        self.request("GET", path, &[], None).await
    }

    /// A 200 JSON answer.
    pub async fn json(&self, path: &str) -> Value {
        let reply = self.get(path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text());
        assert_eq!(
            reply.header("content-type"),
            Some("application/json; charset=utf-8")
        );
        reply.json()
    }

    /// A marked JSON write.
    pub async fn write(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let text = body.map(|b| b.to_string());
        let mut headers = vec![("x-codegraph-ui", "1")];
        if text.is_some() {
            headers.push(("content-type", "application/json"));
        }
        self.request(method, path, &headers, text.as_deref().map(str::as_bytes))
            .await
    }

    /// The id of the symbol called `name` (of `kind`), found through search.
    pub async fn id_of(&self, name: &str, kind: Option<&str>) -> String {
        let search = self.json(&format!("/api/search?q={}", encode(name))).await;
        search["results"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name && kind.is_none_or(|k| r["kind"] == k))
            .unwrap_or_else(|| panic!("no {} named {name}", kind.unwrap_or("symbol")))["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

/// `encodeURIComponent`.
pub fn encode(text: &str) -> String {
    const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'!')
        .remove(b'~')
        .remove(b'*')
        .remove(b'\'')
        .remove(b'(')
        .remove(b')');
    percent_encoding::utf8_percent_encode(text, SET).to_string()
}
