//! The viewer server's security boundary — a port of upstream
//! `src/ui-server/security.ts` (`v1.6.1`).
//!
//! Threat model, as upstream states it: this process serves a browser-readable
//! view of the user's source code from a loopback port. Nothing on the network
//! can reach it, which leaves DNS rebinding — a page the user visits points a
//! name at `127.0.0.1` and has the browser issue same-origin requests. The `Host`
//! header is what tells those apart, so:
//!
//! - `Host` must be a loopback name and, if it carries a port, ours;
//! - `Origin`, when present, must be loopback on our port too;
//! - no CORS header is ever sent;
//! - GET/HEAD answer anywhere, POST/DELETE only under `/api/` and only for a
//!   request a form could not have made (a custom header, JSON or no body type);
//! - every project read resolves through [`resolve_project_file`].

use std::path::{Component, Path, PathBuf};

/// Host names that mean "this machine", stored without IPv6 brackets.
const LOOPBACK_HOSTNAMES: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Methods that answer anywhere: the bundle and every read endpoint.
pub const READ_METHODS: [&str; 2] = ["GET", "HEAD"];

/// Methods that answer under `/api/` only, and only with [`WRITE_HEADER`].
pub const WRITE_METHODS: [&str; 2] = ["POST", "DELETE"];

/// Every method the server answers at all; anything else is a 405.
pub const ALLOWED_METHODS: [&str; 4] = ["GET", "HEAD", "POST", "DELETE"];

/// The header a write must carry. A custom header cannot be sent cross-origin
/// without a CORS preflight, which this server never answers.
pub const WRITE_HEADER: &str = "x-codegraph-ui";

/// The only content type a write body may declare (a form can send none of these).
const WRITE_CONTENT_TYPE: &str = "application/json";

pub fn is_write_method(method: &str) -> bool {
    WRITE_METHODS.contains(&method)
}

/// Whether a mutating request is one the viewer could have made. `Err` carries
/// the reason shown to the caller.
pub fn is_write_request(
    pathname: &str,
    marker: Option<&str>,
    content_type: Option<&str>,
) -> Result<(), String> {
    if pathname != "/api" && !pathname.starts_with("/api/") {
        return Err("Only the /api/ endpoints accept writes.".to_string());
    }
    if marker.is_none_or(|m| m.trim().is_empty()) {
        return Err(format!("A write must carry the {WRITE_HEADER} header."));
    }
    if let Some(content_type) = content_type {
        let kind = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !kind.is_empty() && kind != WRITE_CONTENT_TYPE {
            return Err(format!("A write body must be {WRITE_CONTENT_TYPE}."));
        }
    }
    Ok(())
}

/// `Some(port)` for a `:1234` suffix, `Some(None)` for an empty suffix, `None`
/// when a suffix is present but is not a plain port number.
fn parse_port_suffix(suffix: &str) -> Option<Option<u16>> {
    if suffix.is_empty() {
        return Some(None);
    }
    let digits = suffix.strip_prefix(':')?;
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u32 = digits.parse().ok()?;
    u16::try_from(value).ok().map(Some)
}

/// Split a `Host` header into hostname and optional port; `None` when malformed.
/// An unbracketed IPv6 literal is malformed (RFC 7230) and rejected.
fn split_host_port(host: &str) -> Option<(String, Option<u16>)> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        let end = rest.find(']')?;
        let port = parse_port_suffix(&rest[end + 1..])?;
        return Some((rest[..end].to_string(), port));
    }
    match trimmed.find(':') {
        None => Some((trimmed.to_string(), None)),
        Some(colon) => {
            if trimmed[colon + 1..].contains(':') {
                return None;
            }
            let port = parse_port_suffix(&trimmed[colon..])?;
            Some((trimmed[..colon].to_string(), port))
        }
    }
}

fn is_loopback_name(hostname: &str) -> bool {
    let lower = hostname.to_ascii_lowercase();
    LOOPBACK_HOSTNAMES.contains(&lower.as_str())
}

/// Whether a request's `Host` header names this loopback server. A missing
/// `Host` is rejected: HTTP/1.1 requires it.
pub fn is_allowed_host(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else {
        return false;
    };
    let Some((hostname, host_port)) = split_host_port(host) else {
        return false;
    };
    if !is_loopback_name(&hostname) {
        return false;
    }
    host_port.is_none_or(|p| p == port)
}

/// Whether a request's `Origin` header is acceptable. Absent or empty is fine
/// (same-origin GETs omit it); the literal `null` origin is refused; otherwise it
/// must be http(s) loopback on our port.
pub fn is_allowed_origin(origin: Option<&str>, port: u16) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed == "null" {
        return false;
    }
    let rest = if let Some(rest) = strip_prefix_ignore_case(trimmed, "http://") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_case(trimmed, "https://") {
        rest
    } else {
        return false;
    };
    // An origin is scheme://host[:port] with no path, query or credentials.
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return false;
    }
    let Some((hostname, origin_port)) = split_host_port(authority) else {
        return false;
    };
    if !is_loopback_name(&hostname) {
        return false;
    }
    origin_port.is_none_or(|p| p == port)
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&value[prefix.len()..])
    } else {
        None
    }
}

/// Percent-decode a URL path, rejecting what only ever shows up in an attack:
/// malformed escapes, invalid UTF-8, C0 control bytes, DEL, and backslashes.
pub fn decode_path(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded
        .chars()
        .any(|c| c <= '\u{1f}' || c == '\u{7f}' || c == '\\')
    {
        return None;
    }
    Some(decoded)
}

/// Whether a RAW request path is worth resolving at all: decodable, and free of
/// any `..` segment (checked before anything folds `..` away).
pub fn is_safe_request_path(raw: &str) -> bool {
    match decode_path(raw) {
        Some(decoded) => !decoded.split('/').any(|segment| segment == ".."),
        None => false,
    }
}

/// Why a project read was refused; the API answers these as 403 `refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRefusal(pub String);

impl std::fmt::Display for PathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathRefusal {}

/// Sensitive system directories a viewer must never serve from, as upstream's
/// `validateProjectPath` lists them.
const SENSITIVE_PATHS: [&str; 18] = [
    "/",
    "/etc",
    "/usr",
    "/bin",
    "/sbin",
    "/var",
    "/tmp",
    "/dev",
    "/proc",
    "/sys",
    "/root",
    "/boot",
    "/lib",
    "/lib64",
    "/opt",
    "c:\\",
    "c:\\windows",
    "c:\\windows\\system32",
];

/// Home subdirectories that hold credentials.
const SENSITIVE_HOME_DIRS: [&str; 4] = [".ssh", ".gnupg", ".aws", ".config"];

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Upstream `validateProjectPath`: `Err` when the root is a sensitive system or
/// credential directory, or is not an accessible directory.
pub fn validate_project_path(root: &Path) -> Result<(), PathRefusal> {
    let resolved = absolute_lexical(root);
    let shown = resolved.display().to_string();
    let lower = shown.to_lowercase();
    if SENSITIVE_PATHS.contains(&shown.as_str()) || SENSITIVE_PATHS.contains(&lower.as_str()) {
        return Err(PathRefusal(format!(
            "Refusing to operate on sensitive system directory: {shown}"
        )));
    }
    if let Some(home) = home_dir() {
        for dir in SENSITIVE_HOME_DIRS {
            let sensitive = home.join(dir);
            if resolved == sensitive || resolved.starts_with(&sensitive) {
                return Err(PathRefusal(format!(
                    "Refusing to operate on sensitive directory: {shown}"
                )));
            }
        }
    }
    match std::fs::metadata(&resolved) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(PathRefusal(format!("Path is not a directory: {shown}"))),
        Err(_) => Err(PathRefusal(format!(
            "Path does not exist or is not accessible: {shown}"
        ))),
    }
}

/// `path` made absolute against the current directory and lexically normalised
/// (`.` dropped, `..` folded), without touching the filesystem.
fn absolute_lexical(path: &Path) -> PathBuf {
    let base = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let mut out = PathBuf::new();
    for component in base.join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `child` is `parent` or below it; case-insensitive on Windows, where
/// canonicalisation can return a different case than the lexical root.
fn is_within_dir(child: &Path, parent: &Path) -> bool {
    if cfg!(windows) {
        let child = child.to_string_lossy().to_lowercase();
        let parent = parent.to_string_lossy().to_lowercase();
        let parent = parent.trim_end_matches(['\\', '/']);
        child == parent
            || child
                .strip_prefix(parent)
                .is_some_and(|rest| rest.starts_with(['\\', '/']))
    } else {
        child.starts_with(parent)
    }
}

/// Upstream `validatePathWithinRoot` without `allowSymlinkEscape`: lexical
/// containment, then real-path containment on both sides, so an in-tree symlink
/// whose target escapes the root is refused. A path that does not exist yet
/// passes on the lexical check alone; any other resolution failure is unsafe.
fn path_within_root(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = absolute_lexical(root);
    let joined = absolute_lexical(&root.join(relative));
    if !joined.starts_with(&root) {
        return None;
    }
    let real_root = std::fs::canonicalize(&root).ok()?;
    match std::fs::canonicalize(&joined) {
        Ok(real) => is_within_dir(&real, &real_root).then_some(real),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(joined),
        Err(_) => None,
    }
}

/// Resolve a project-relative source path to an absolute path that is safe to
/// read and hand to the browser. The single read chokepoint for anything served
/// out of the user's repository.
pub fn resolve_project_file(project_root: &Path, relative: &str) -> Result<PathBuf, PathRefusal> {
    if relative.trim().is_empty() {
        return Err(PathRefusal("No file path was given.".to_string()));
    }
    let Some(decoded) = decode_path(relative) else {
        return Err(PathRefusal(format!(
            "Refusing to read an unusable path: {relative}"
        )));
    };
    validate_project_path(project_root)?;
    if Path::new(&decoded).is_absolute() || decoded.starts_with('/') {
        return Err(PathRefusal(format!(
            "Refusing to read an absolute path: {decoded}"
        )));
    }
    path_within_root(project_root, &decoded).ok_or_else(|| {
        PathRefusal(format!(
            "Refusing to read a path outside the project: {decoded}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_must_be_loopback_on_our_port() {
        assert!(is_allowed_host(Some("localhost"), 4747));
        assert!(is_allowed_host(Some("127.0.0.1:4747"), 4747));
        assert!(is_allowed_host(Some("[::1]:4747"), 4747));
        assert!(is_allowed_host(Some("LOCALHOST:4747"), 4747));
        assert!(!is_allowed_host(Some("127.0.0.1:4748"), 4747));
        assert!(!is_allowed_host(Some("evil.example"), 4747));
        assert!(!is_allowed_host(Some("evil.example:4747"), 4747));
        assert!(
            !is_allowed_host(Some("::1"), 4747),
            "unbracketed IPv6 is malformed"
        );
        assert!(!is_allowed_host(Some("127.0.0.1:47a7"), 4747));
        assert!(!is_allowed_host(Some(""), 4747));
        assert!(!is_allowed_host(None, 4747));
    }

    #[test]
    fn origin_absent_or_loopback_on_our_port() {
        assert!(is_allowed_origin(None, 4747));
        assert!(is_allowed_origin(Some(""), 4747));
        assert!(is_allowed_origin(Some("http://127.0.0.1:4747"), 4747));
        assert!(is_allowed_origin(Some("http://localhost"), 4747));
        assert!(is_allowed_origin(Some("https://[::1]:4747"), 4747));
        assert!(!is_allowed_origin(Some("null"), 4747));
        assert!(!is_allowed_origin(Some("http://evil.example"), 4747));
        assert!(!is_allowed_origin(Some("http://127.0.0.1:9999"), 4747));
        assert!(!is_allowed_origin(Some("file://"), 4747));
        assert!(!is_allowed_origin(Some("http://user@127.0.0.1:4747"), 4747));
    }

    #[test]
    fn writes_need_the_marker_and_json() {
        assert!(is_write_request("/api/trails", Some("1"), Some("application/json")).is_ok());
        assert!(is_write_request("/api/trails", Some("1"), None).is_ok());
        assert!(
            is_write_request(
                "/api/trails",
                Some("1"),
                Some("application/json; charset=utf-8")
            )
            .is_ok()
        );
        assert!(is_write_request("/api/trails", None, None).is_err());
        assert!(is_write_request("/api/trails", Some("  "), None).is_err());
        assert!(is_write_request("/api/trails", Some("1"), Some("text/plain")).is_err());
        assert!(is_write_request("/index.html", Some("1"), None).is_err());
    }

    #[test]
    fn raw_paths_refuse_traversal_and_control_bytes() {
        assert!(is_safe_request_path("/assets/index.js"));
        assert!(!is_safe_request_path("/../etc/passwd"));
        assert!(!is_safe_request_path("/a/%2e%2e/b"));
        assert!(!is_safe_request_path("/a%00b"));
        assert!(!is_safe_request_path("/a%5cb"));
        assert!(!is_safe_request_path("/a%zz"));
        assert!(!is_safe_request_path("/a%ff"));
    }

    #[test]
    fn project_reads_stay_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "fn a() {}").unwrap();
        assert!(resolve_project_file(root, "a.rs").is_ok());
        assert!(
            resolve_project_file(root, "missing.rs").is_ok(),
            "a not-yet-existing path passes lexically"
        );
        assert!(resolve_project_file(root, "").is_err());
        assert!(resolve_project_file(root, "../outside.rs").is_err());
        assert!(resolve_project_file(root, "/etc/passwd").is_err());
        assert!(resolve_project_file(root, "a%00.rs").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn an_in_tree_symlink_escaping_the_root_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(resolve_project_file(dir.path(), "link/secret.txt").is_err());
    }

    // POSIX only, as upstream gates it: on Windows `/` resolves to the root of
    // the current drive and `/etc` to `<drive>:\etc`, neither of which is on the
    // list. The Windows entries have their own test below.
    #[cfg(not(windows))]
    #[test]
    fn blocks_posix_system_directories_exact_match() {
        for root in ["/", "/etc"] {
            let refusal = validate_project_path(Path::new(root)).unwrap_err();
            assert!(
                refusal.0.contains("sensitive system directory"),
                "{root}: {}",
                refusal.0
            );
        }
    }

    #[test]
    fn allows_a_normal_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_project_path(dir.path()).is_ok());
    }

    // The list stores the Windows entries lower case and the check compares the
    // lower-cased path too, so any spelling is refused.
    #[cfg(windows)]
    #[test]
    fn blocks_windows_system_directories_regardless_of_case() {
        for root in ["C:\\Windows", "c:\\windows", "C:\\WINDOWS\\System32"] {
            let refusal = validate_project_path(Path::new(root)).unwrap_err();
            assert!(
                refusal.0.contains("sensitive system directory"),
                "{root}: {}",
                refusal.0
            );
        }
    }
}
