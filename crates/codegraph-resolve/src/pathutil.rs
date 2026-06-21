//! POSIX-style path utilities mirroring the Node `path.posix` operations the upstream
//! relies on across the resolution layer.
//!
//! The upstream runs on Node and uses `path.resolve` / `path.relative` / `path.dirname`
//! / `path.join` with `/` separators (it normalizes `\` to `/` everywhere). The
//! Rust port works entirely in project-relative POSIX strings, so these helpers
//! implement the exact lexical semantics the upstream depends on — WITHOUT touching the
//! filesystem (resolution asks the [`ResolutionContext`] whether a path exists).
//!
//! [`ResolutionContext`]: crate::types::ResolutionContext

/// Lexically normalize a POSIX path, collapsing `.` and `..` segments.
///
/// Equivalent to `path.posix.normalize` for the inputs the resolver produces.
/// Leading `..` segments that can't be collapsed are preserved (so a rewrite
/// escaping the root stays detectable). A leading `/` is preserved.
pub fn normalize(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                        continue;
                    }
                }
                if !is_absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if is_absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Directory portion of a POSIX path (`path.posix.dirname`).
pub fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => "".to_string(),
    }
}

/// Last segment of a POSIX path (`path.posix.basename`).
pub fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Resolve `relative` against `base` and normalize (`path.posix.resolve`-like
/// for the relative inputs the resolver produces). When `relative` is absolute
/// it wins; otherwise it is joined onto `base`.
pub fn resolve(base: &str, relative: &str) -> String {
    if relative.starts_with('/') {
        return normalize(relative);
    }
    let joined = if base.is_empty() {
        relative.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), relative)
    };
    normalize(&joined)
}

/// Compute a relative path from `from` to `to`, both treated as POSIX
/// directories/paths rooted the same way (`path.posix.relative`). Used to turn
/// an absolute-ish base back into a project-relative path.
pub fn relative(from: &str, to: &str) -> String {
    let from_segs: Vec<&str> = from
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let to_segs: Vec<&str> = to
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut i = 0;
    while i < from_segs.len() && i < to_segs.len() && from_segs[i] == to_segs[i] {
        i += 1;
    }
    let mut out: Vec<String> = Vec::new();
    for _ in i..from_segs.len() {
        out.push("..".to_string());
    }
    for seg in &to_segs[i..] {
        out.push((*seg).to_string());
    }
    out.join("/")
}
