//! The HTTP side of `codegraph ui` — upstream `src/ui-server/index.ts`.
//!
//! One request, start to finish, in upstream's order: the cheap refusals
//! (method, `Host`, `Origin`, the write check, the raw-path check) run before
//! anything is looked up; `/api/` answers JSON for every outcome; everything else
//! is the embedded bundle, with the app shell as the fallback for client routes.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use codegraph_core::IndexPaths;
use serde_json::Value;

use crate::api;
use crate::assets;
use crate::events::EventHub;
use crate::respond::{ApiError, MAX_BODY_BYTES, Query};
use crate::security;

/// The port `codegraph ui` tries first.
pub const DEFAULT_UI_PORT: u16 = 4747;

/// Ports tried, counting the first, when no port was asked for.
pub const DEFAULT_PORT_ATTEMPTS: u16 = 20;

/// The only address this server binds.
pub const LOOPBACK_ADDRESS: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// What every handler can see.
pub struct AppState {
    /// The project root, canonical.
    pub project_root: PathBuf,
    /// The project's index paths, resolved once through `IndexPaths`.
    pub paths: IndexPaths,
    /// Refuse every write (saved trails), with the reason shown in place of Save.
    pub read_only: bool,
    pub read_only_reason: Option<String>,
    /// The bound port, which the `Host` and `Origin` checks compare against.
    pub port: AtomicU16,
    /// The live channel (`/api/events`).
    pub events: EventHub,
    /// Memoised payloads keyed on the index revision.
    pub caches: crate::caches::Caches,
    /// Who saved trails are signed with, read once per server.
    pub trail_author: std::sync::OnceLock<String>,
}

impl AppState {
    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }
}

/// Upstream's security headers, on every response. Never any `Access-Control-*`.
const SECURITY_HEADERS: [(&str, &str); 4] = [
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "no-referrer"),
    (
        "content-security-policy",
        "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
         font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ),
];

pub fn router(state: Arc<AppState>) -> Router {
    Router::new().fallback(handle).with_state(state)
}

async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let mut response = pipeline(&state, req).await;
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// A header value echoed into a refusal, with anything a terminal or a log could
/// misread replaced.
fn for_echo(value: Option<&str>) -> String {
    match value {
        None => "(none)".to_string(),
        Some(v) => v
            .chars()
            .take(200)
            .map(|c| if c.is_control() { '?' } else { c })
            .collect(),
    }
}

async fn pipeline(state: &Arc<AppState>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let is_head = req.method() == Method::HEAD;

    if !security::ALLOWED_METHODS.contains(&method.as_str()) {
        let mut response = text(
            StatusCode::METHOD_NOT_ALLOWED,
            &format!("codegraph ui does not answer {method}."),
            is_head,
        );
        response.headers_mut().insert(
            header::ALLOW,
            HeaderValue::from_static("GET, HEAD, POST, DELETE"),
        );
        return response;
    }

    let port = state.port();
    let host = header_str(req.headers(), "host");
    if !security::is_allowed_host(host, port) {
        return text(
            StatusCode::FORBIDDEN,
            &format!(
                "Refused: codegraph ui only answers requests addressed to this machine \
                 (localhost, 127.0.0.1 or [::1] on port {port}).\nThis request said Host: {}",
                for_echo(host)
            ),
            is_head,
        );
    }
    if !security::is_allowed_origin(header_str(req.headers(), "origin"), port) {
        return text(
            StatusCode::FORBIDDEN,
            "Refused: cross-origin requests are not served.",
            is_head,
        );
    }

    // Checked on the RAW path, before anything decodes or folds it.
    let raw_path = req.uri().path().to_string();
    let json_namespace = raw_path == "/api" || raw_path.starts_with("/api/");

    if security::is_write_method(&method)
        && let Err(reason) = security::is_write_request(
            &raw_path,
            header_str(req.headers(), security::WRITE_HEADER),
            header_str(req.headers(), "content-type"),
        )
    {
        let body = format!("Refused: {reason}");
        if json_namespace {
            return json(
                StatusCode::FORBIDDEN,
                &serde_json::json!({ "error": body, "code": "refused" }),
                is_head,
            );
        }
        let mut response = text(StatusCode::METHOD_NOT_ALLOWED, &body, is_head);
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }

    if !security::is_safe_request_path(&raw_path) {
        return if json_namespace {
            json(
                StatusCode::NOT_FOUND,
                &serde_json::json!({ "error": "Not found", "code": "not-found" }),
                is_head,
            )
        } else {
            text(StatusCode::NOT_FOUND, "Not found", is_head)
        };
    }

    let Some(pathname) = security::decode_path(&raw_path) else {
        return text(StatusCode::BAD_REQUEST, "Bad request URL.", is_head);
    };

    if json_namespace {
        let query = Query::parse(req.uri().query());
        let body = if security::is_write_method(&method) {
            let too_large = || {
                ApiError::bad_request(format!(
                    "That request body is too large (max {MAX_BODY_BYTES} bytes)."
                ))
            };
            match to_bytes(req.into_body(), MAX_BODY_BYTES).await {
                Ok(bytes) => Ok(bytes.to_vec()),
                // The limit is applied to what actually arrives, not to a
                // `Content-Length` claim.
                Err(err) if is_length_limit(&err) => Err(too_large()),
                Err(_) => Err(ApiError::bad_request(
                    "That request body could not be read.",
                )),
            }
        } else {
            Ok(Vec::new())
        };
        return api::dispatch(state.clone(), &method, &pathname, query, body).await;
    }

    serve_static(&pathname, is_head)
}

fn serve_static(pathname: &str, is_head: bool) -> Response {
    let requested = if pathname == "/" {
        "index.html"
    } else {
        pathname.trim_start_matches('/')
    };
    if let Some(bytes) = assets::viewer_file(requested) {
        return file(requested, bytes, is_head);
    }
    if assets::should_fall_back_to_index(pathname)
        && let Some(bytes) = assets::viewer_file("index.html")
    {
        return file("index.html", bytes, is_head);
    }
    text(StatusCode::NOT_FOUND, "Not found", is_head)
}

fn file(path: &str, bytes: &'static [u8], is_head: bool) -> Response {
    let mut response = Response::new(if is_head {
        Body::empty()
    } else {
        Body::from(bytes)
    });
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(assets::content_type_for(path)),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(assets::cache_control_for(path)),
    );
    response
}

/// A plain-text answer, newline-terminated, never cached.
pub fn text(status: StatusCode, message: &str, is_head: bool) -> Response {
    let body = if message.ends_with('\n') {
        message.to_string()
    } else {
        format!("{message}\n")
    };
    let len = body.len();
    let mut response = Response::new(if is_head {
        Body::empty()
    } else {
        Body::from(body)
    });
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A JSON answer, never cached.
pub fn json(status: StatusCode, payload: &Value, is_head: bool) -> Response {
    let body = serde_json::to_vec(payload).unwrap_or_else(|_| b"{}".to_vec());
    let len = body.len();
    let mut response = Response::new(if is_head {
        Body::empty()
    } else {
        Body::from(body)
    });
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Whether a body read failed on the size cap rather than on the connection.
fn is_length_limit(err: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = source {
        if current.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = current.source();
    }
    false
}

/// Bind the loopback listener: the asked-for port exactly, or — when none was
/// asked for — the default and up to [`DEFAULT_PORT_ATTEMPTS`] after it.
pub async fn bind(port: Option<u16>) -> anyhow::Result<tokio::net::TcpListener> {
    match port {
        Some(port) => bind_ports(port, 1, false).await,
        None => bind_ports(DEFAULT_UI_PORT, DEFAULT_PORT_ATTEMPTS, true).await,
    }
}

/// Bind `first`, or — with `fallback` — the next free port of the `attempts`
/// starting there. Without it a taken port is an error, never a quiet move.
pub async fn bind_ports(
    first: u16,
    attempts: u16,
    fallback: bool,
) -> anyhow::Result<tokio::net::TcpListener> {
    let mut last: Option<std::io::Error> = None;
    for offset in 0..attempts.max(1) {
        let candidate = first.saturating_add(offset);
        match tokio::net::TcpListener::bind(SocketAddr::from((LOOPBACK_ADDRESS, candidate))).await {
            Ok(listener) => return Ok(listener),
            Err(err) if fallback && err.kind() == std::io::ErrorKind::AddrInUse => {
                last = Some(err);
            }
            Err(err) => {
                return Err(match err.kind() {
                    std::io::ErrorKind::AddrInUse => anyhow::anyhow!(
                        "Port {candidate} is already in use. Pick another with --port, or omit --port to let codegraph ui find a free one."
                    ),
                    std::io::ErrorKind::PermissionDenied => anyhow::anyhow!(
                        "Not allowed to listen on port {candidate}. Ports below 1024 usually need elevated rights; pick another with --port."
                    ),
                    _ => anyhow::anyhow!("Could not listen on 127.0.0.1:{candidate}: {err}"),
                });
            }
        }
    }
    Err(anyhow::anyhow!(
        "Ports {first}-{} are all in use. Pick a free one with --port.{}",
        first.saturating_add(attempts.max(1) - 1),
        last.map(|e| format!(" ({e})")).unwrap_or_default()
    ))
}
