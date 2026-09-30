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

use std::path::{Path, PathBuf};

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

/// Resolve a project-relative candidate without allowing it to escape the
/// project root lexically.
///
/// Resolution's filesystem fallback receives paths containing `..` from
/// relative-import candidates. A plain [`Path::join`] would let repository
/// content probe arbitrary paths outside the project. This intentionally does
/// not canonicalize or reject symlinks: indexing permits an in-project symlink
/// whose target is outside the root, so this guard covers lexical traversal
/// only (#1631).
pub(crate) fn lexical_path_within_root(project_root: &str, file_path: &str) -> Option<PathBuf> {
    let normalized_separators = file_path.replace('\\', "/");
    let bytes = normalized_separators.as_bytes();
    let has_windows_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    // After separators are normalized, a leading `/` covers POSIX absolute
    // paths plus Windows root-relative, UNC, and device forms. Check it directly
    // rather than using `Path::is_absolute`: on Windows `/outside` is rooted but
    // not "absolute" (it has no drive prefix), yet joining it to `C:\repo`
    // still escapes to `C:\outside`. The prefix check covers both `C:/outside`
    // and drive-relative `C:outside` forms on every host.
    if normalized_separators.starts_with('/') || has_windows_prefix {
        return None;
    }

    let relative = normalize(&normalized_separators);
    if relative == ".." || relative.starts_with("../") {
        return None;
    }

    Some(Path::new(project_root).join(relative))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_dot_and_dotdot() {
        assert_eq!(normalize("a/./b/../c"), "a/c");
        assert_eq!(normalize("a/b/../../c"), "c");
        assert_eq!(normalize("./a/b"), "a/b");
    }

    #[test]
    fn normalize_preserves_leading_dotdot_when_relative() {
        assert_eq!(normalize("../a"), "../a");
        assert_eq!(normalize("../../a/b"), "../../a/b");
        assert_eq!(normalize("a/../../b"), "../b");
    }

    #[test]
    fn normalize_absolute_drops_escaping_dotdot() {
        assert_eq!(normalize("/a/b/../c"), "/a/c");
        assert_eq!(normalize("/../a"), "/a");
        assert_eq!(normalize("/a/../.."), "/");
    }

    #[test]
    fn normalize_empty_and_dot_yield_dot() {
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("."), ".");
        assert_eq!(normalize("a/.."), ".");
    }

    #[test]
    fn normalize_root_only() {
        assert_eq!(normalize("/"), "/");
    }

    #[test]
    fn dirname_variants() {
        assert_eq!(dirname("a/b/c"), "a/b");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("noslash"), "");
        assert_eq!(dirname("/a/b"), "/a");
    }

    #[test]
    fn basename_variants() {
        assert_eq!(basename("a/b/c"), "c");
        assert_eq!(basename("/a"), "a");
        assert_eq!(basename("noslash"), "noslash");
        assert_eq!(basename("a/b/"), "");
    }

    #[test]
    fn resolve_absolute_relative_wins() {
        assert_eq!(resolve("/base/dir", "/abs/path"), "/abs/path");
        assert_eq!(resolve("/base", "/abs/../x"), "/x");
    }

    #[test]
    fn resolve_joins_and_normalizes() {
        assert_eq!(resolve("base", "sub/file"), "base/sub/file");
        assert_eq!(resolve("base/", "sub"), "base/sub");
        assert_eq!(resolve("base/dir", "../other"), "base/other");
    }

    #[test]
    fn resolve_empty_base_uses_relative() {
        assert_eq!(resolve("", "a/b"), "a/b");
        assert_eq!(resolve("", "./a"), "a");
    }

    #[test]
    fn lexical_path_within_root_rejects_relative_and_absolute_escapes() {
        let root = if cfg!(windows) { r"C:\repo" } else { "/repo" };
        assert_eq!(
            lexical_path_within_root(root, "src/../lib/a.ts"),
            Some(Path::new(root).join("lib/a.ts"))
        );
        assert!(lexical_path_within_root(root, "../outside/a.ts").is_none());
        assert!(lexical_path_within_root(root, "a/../../outside.ts").is_none());
        assert!(lexical_path_within_root(root, "/outside/a.ts").is_none());
        assert!(lexical_path_within_root(root, r"\outside\a.ts").is_none());
        assert!(lexical_path_within_root(root, r"C:\outside\a.ts").is_none());
        assert!(lexical_path_within_root(root, r"C:outside\a.ts").is_none());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn lexical_path_within_root_preserves_in_root_symlink_targets() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox = std::env::temp_dir().join(format!(
            "cg-pathutil-symlink-{}-{nanos}",
            std::process::id()
        ));
        let root = sandbox.join("repo");
        let outside = sandbox.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.ts"), "export const secret = 42;\n").unwrap();

        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&outside, root.join("vendor"));
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_dir(&outside, root.join("vendor"));

        if link_result.is_ok() {
            let candidate = lexical_path_within_root(
                root.to_str().expect("UTF-8 temp path"),
                "vendor/secret.ts",
            )
            .expect("the symlink path is lexically inside the project");
            assert_eq!(candidate, root.join("vendor/secret.ts"));
            assert!(
                candidate.exists(),
                "the filesystem probe still follows symlinks"
            );
        }

        std::fs::remove_dir_all(&sandbox).unwrap();
    }

    #[test]
    fn relative_computes_updots_and_forward() {
        assert_eq!(relative("a/b", "a/c"), "../c");
        assert_eq!(relative("a/b/c", "a/b"), "..");
        assert_eq!(relative("a", "a/b/c"), "b/c");
        assert_eq!(relative("a/b", "a/b"), "");
    }

    #[test]
    fn relative_ignores_dot_and_empty_segments() {
        assert_eq!(relative("./a/b", "a/c"), "../c");
        assert_eq!(relative("a//b", "a/b"), "");
    }
}
