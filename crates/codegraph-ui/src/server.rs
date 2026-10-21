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
    bind_ports_with(first, attempts, fallback, BindPlatform::CURRENT, |port| {
        tokio::net::TcpListener::bind(SocketAddr::from((LOOPBACK_ADDRESS, port)))
    })
    .await
}

/// [`bind_ports`] through `bind`, its failures read as on `platform`. Only a
/// failure the next port may not share moves on ([`should_try_next_port`]):
/// a permission rule every port is under, or a bad address, would only be
/// buried by twenty more tries.
async fn bind_ports_with<F, Fut>(
    first: u16,
    attempts: u16,
    fallback: bool,
    platform: BindPlatform,
    mut bind: F,
) -> anyhow::Result<tokio::net::TcpListener>
where
    F: FnMut(u16) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<tokio::net::TcpListener>>,
{
    let attempts = if fallback { attempts.max(1) } else { 1 };
    // Whether any port was refused rather than in use: the message says so.
    let mut refused = false;
    for offset in 0..attempts {
        let candidate = first.saturating_add(offset);
        match bind(candidate).await {
            Ok(listener) => return Ok(listener),
            Err(err) => {
                refused |= err.kind() == std::io::ErrorKind::PermissionDenied;
                if !(fallback && should_try_next_port(err.kind(), candidate, platform))
                    || offset + 1 == attempts
                {
                    return Err(describe_bind_failure(
                        &err, first, candidate, fallback, platform, refused,
                    ));
                }
            }
        }
    }
    unreachable!("the last attempt returns")
}

/// Whose rules a bind failure is read by: Windows refuses ports for reasons
/// POSIX does not have, and has no privileged ports (upstream #2353).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindPlatform {
    Windows,
    Posix,
}

impl BindPlatform {
    const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Posix
    };
}

/// The command that lists the port ranges Windows reserves.
const LIST_RESERVED_PORTS: &str = "`netsh int ipv4 show excludedportrange protocol=tcp`";

/// Whether only an administrator may bind `port`: below 1024, on every
/// platform but Windows, which has no such rule.
fn is_privileged_port(port: u16, platform: BindPlatform) -> bool {
    platform == BindPlatform::Posix && port > 0 && port < 1024
}

/// Whether a bind of `port` that failed with `kind` leaves the next port
/// worth trying. A port in use does. So does one refused on Windows, where a
/// port another program holds exclusively (`SO_EXCLUSIVEADDRUSE`) or one in
/// a reserved range fails with a permission error rather than as in use;
/// elsewhere a refused port below 1024 is the privileged-port rule, which the
/// next port is under too, and from 1024 up something refusing that one port.
fn should_try_next_port(kind: std::io::ErrorKind, port: u16, platform: BindPlatform) -> bool {
    match kind {
        std::io::ErrorKind::AddrInUse => true,
        std::io::ErrorKind::PermissionDenied => !is_privileged_port(port, platform),
        _ => false,
    }
}

/// A bind failure of `port` as something a user can act on, and true where
/// it is read. `refused` says whether a walk from `first` met a refused port
/// rather than only ports in use; it only words a walk that ran out.
fn describe_bind_failure(
    err: &std::io::Error,
    first: u16,
    port: u16,
    fallback: bool,
    platform: BindPlatform,
    refused: bool,
) -> anyhow::Error {
    let kind = err.kind();
    let windows = platform == BindPlatform::Windows;
    if fallback && should_try_next_port(kind, port, platform) {
        let taken = if !refused && kind == std::io::ErrorKind::AddrInUse {
            "in use".to_string()
        } else if windows {
            format!("in use or reserved ({LIST_RESERVED_PORTS} lists the reserved ranges)")
        } else {
            "in use or not allowed".to_string()
        };
        return anyhow::anyhow!(
            "Ports {first}-{port} are all {taken}. Free one, or pick another with --port. ({err})"
        );
    }
    match kind {
        std::io::ErrorKind::AddrInUse => anyhow::anyhow!(
            "Port {port} is already in use. Pick another with --port, or omit --port to let codegraph ui find a free one."
        ),
        std::io::ErrorKind::PermissionDenied if is_privileged_port(port, platform) => {
            anyhow::anyhow!(
                "Not allowed to listen on port {port}. Ports below 1024 usually need elevated rights; pick another with --port."
            )
        }
        std::io::ErrorKind::PermissionDenied if windows => anyhow::anyhow!(
            "Windows refused port {port}: another program holds it, or it is in a reserved range ({LIST_RESERVED_PORTS} lists them). Pick another with --port, or omit --port to let codegraph ui find a free one."
        ),
        std::io::ErrorKind::PermissionDenied => anyhow::anyhow!(
            "Not allowed to listen on port {port}: the system refused permission. Pick another with --port, or omit --port to let codegraph ui find a free one."
        ),
        _ => anyhow::anyhow!("Could not listen on 127.0.0.1:{port}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    const PLATFORMS: [BindPlatform; 2] = [BindPlatform::Windows, BindPlatform::Posix];

    /// A binder that fails the ports `failures` names with their kind and binds
    /// any free loopback port for the rest, recording what it was asked.
    fn binder(
        failures: Vec<(u16, ErrorKind)>,
        tried: std::sync::Arc<std::sync::Mutex<Vec<u16>>>,
    ) -> impl FnMut(
        u16,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<tokio::net::TcpListener>>>,
    > {
        move |port| {
            tried.lock().unwrap().push(port);
            let failure = failures.iter().find(|(p, _)| *p == port).map(|(_, k)| *k);
            Box::pin(async move {
                match failure {
                    Some(kind) => Err(Error::from(kind)),
                    None => tokio::net::TcpListener::bind((LOOPBACK_ADDRESS, 0)).await,
                }
            })
        }
    }

    /// #2353: a port in use and the next one refused (another program holds
    /// it, or a reserved range) are both walked past, on every platform from
    /// 1024 up.
    #[tokio::test]
    async fn a_walk_moves_past_a_port_in_use_and_one_refused() {
        for platform in PLATFORMS {
            let tried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let failures = vec![
                (4747, ErrorKind::AddrInUse),
                (4748, ErrorKind::PermissionDenied),
            ];
            let listener =
                bind_ports_with(4747, 20, true, platform, binder(failures, tried.clone()))
                    .await
                    .expect("the third port binds");
            drop(listener);
            assert_eq!(*tried.lock().unwrap(), [4747, 4748, 4749], "{platform:?}");
        }
    }

    /// Below 1024 a refusal is POSIX's privileged-port rule, which every next
    /// port is under too, so the walk stops with that message; Windows has no
    /// such rule and walks on.
    #[tokio::test]
    async fn a_privileged_refusal_stops_the_walk_off_windows_only() {
        let tried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let err = bind_ports_with(
            80,
            20,
            true,
            BindPlatform::Posix,
            binder(vec![(80, ErrorKind::PermissionDenied)], tried.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(*tried.lock().unwrap(), [80]);
        assert!(err.to_string().contains("Ports below 1024"), "{err}");

        let tried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        bind_ports_with(
            80,
            20,
            true,
            BindPlatform::Windows,
            binder(vec![(80, ErrorKind::PermissionDenied)], tried.clone()),
        )
        .await
        .expect("Windows walks on");
        assert_eq!(*tried.lock().unwrap(), [80, 81]);
    }

    /// A pinned port never moves, and its refusal says why where it is read.
    #[tokio::test]
    async fn a_pinned_refused_port_names_the_platforms_reason() {
        for (platform, expected) in [
            (BindPlatform::Windows, "Windows refused port 4747"),
            (BindPlatform::Posix, "the system refused permission"),
        ] {
            let tried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let err = bind_ports_with(
                4747,
                1,
                false,
                platform,
                binder(vec![(4747, ErrorKind::PermissionDenied)], tried.clone()),
            )
            .await
            .unwrap_err();
            assert_eq!(*tried.lock().unwrap(), [4747]);
            assert!(err.to_string().contains(expected), "{platform:?}: {err}");
        }
    }

    #[test]
    fn the_next_port_is_tried_after_a_port_in_use_or_a_refusal_it_may_not_share() {
        for platform in PLATFORMS {
            assert!(should_try_next_port(ErrorKind::AddrInUse, 80, platform));
            assert!(should_try_next_port(ErrorKind::AddrInUse, 4747, platform));
            assert!(should_try_next_port(
                ErrorKind::PermissionDenied,
                4747,
                platform
            ));
            assert!(!should_try_next_port(
                ErrorKind::AddrNotAvailable,
                4747,
                platform
            ));
        }
        assert!(should_try_next_port(
            ErrorKind::PermissionDenied,
            80,
            BindPlatform::Windows
        ));
        assert!(!should_try_next_port(
            ErrorKind::PermissionDenied,
            80,
            BindPlatform::Posix
        ));
        assert!(!should_try_next_port(
            ErrorKind::PermissionDenied,
            1023,
            BindPlatform::Posix
        ));
        assert!(should_try_next_port(
            ErrorKind::PermissionDenied,
            1024,
            BindPlatform::Posix
        ));
    }

    #[test]
    fn a_walk_that_ran_out_says_whether_a_port_was_refused() {
        let in_use = Error::from(ErrorKind::AddrInUse);
        let message = |platform, refused| {
            describe_bind_failure(&in_use, 4747, 4766, true, platform, refused).to_string()
        };
        for platform in PLATFORMS {
            assert!(message(platform, false).starts_with("Ports 4747-4766 are all in use."));
        }
        assert!(message(BindPlatform::Windows, true).contains("in use or reserved"));
        assert!(
            message(BindPlatform::Windows, true).contains("netsh int ipv4 show excludedportrange")
        );
        assert!(message(BindPlatform::Posix, true).contains("in use or not allowed"));
        assert!(!message(BindPlatform::Posix, true).contains("netsh"));
        let refused = Error::from(ErrorKind::PermissionDenied);
        assert!(
            describe_bind_failure(&refused, 4747, 4766, true, BindPlatform::Posix, true)
                .to_string()
                .contains("in use or not allowed")
        );
    }

    #[test]
    fn a_pinned_port_in_use_or_privileged_keeps_its_message() {
        for platform in PLATFORMS {
            let err = describe_bind_failure(
                &Error::from(ErrorKind::AddrInUse),
                4747,
                4747,
                false,
                platform,
                false,
            );
            assert!(
                err.to_string().contains("Port 4747 is already in use"),
                "{err}"
            );
        }
        let err = describe_bind_failure(
            &Error::from(ErrorKind::PermissionDenied),
            80,
            80,
            false,
            BindPlatform::Posix,
            true,
        );
        assert!(err.to_string().contains("Ports below 1024"), "{err}");
        let err = describe_bind_failure(
            &Error::from(ErrorKind::PermissionDenied),
            80,
            80,
            false,
            BindPlatform::Windows,
            true,
        );
        assert!(err.to_string().contains("Windows refused port 80"), "{err}");
    }
}
