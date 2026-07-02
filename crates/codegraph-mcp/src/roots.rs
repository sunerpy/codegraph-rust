//! Workspace-root discovery for clients that launch the MCP server globally.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const ROOTS_LIST_REQUEST_ID: &str = "codegraph-roots-list-1";

/// Whether `CODEGRAPH_DEBUG` is truthy (`"1"`/`"true"`), gating the
/// `[codegraph debug]` stderr trace lines. Off ⇒ no new output (stdout stays
/// pure JSON-RPC).
pub fn debug_enabled() -> bool {
    matches!(
        std::env::var("CODEGRAPH_DEBUG").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Pure formatter for the per-tool `projectPath` resolution debug line
/// (unit-tested without touching process state).
pub fn format_tool_debug_line(
    tool_name: &str,
    raw_project: Option<&str>,
    resolved: Option<&Path>,
    cwd: Option<&Path>,
    default_project: Option<&Path>,
) -> String {
    let raw = raw_project.unwrap_or("(none)");
    let (resolved_str, db_str, db_exists) = match resolved {
        Some(p) => {
            let db = db_path_for(p);
            let exists = db.is_file();
            (p.display().to_string(), db.display().to_string(), exists)
        }
        None => ("(unresolved)".to_string(), "(none)".to_string(), false),
    };
    let cwd_str = cwd.map_or_else(|| "(none)".to_string(), |p| p.display().to_string());
    let default_str =
        default_project.map_or_else(|| "(none)".to_string(), |p| p.display().to_string());
    format!(
        "[codegraph debug] tool={tool_name} projectPath_raw={raw} resolved={resolved_str} db={db_str} db_exists={db_exists} cwd={cwd_str} default_project={default_str}"
    )
}

/// The relative `.codegraph/codegraph.db` path under a project root, honoring
/// the `CODEGRAPH_DIR` override.
pub fn db_path_for(project_path: &Path) -> PathBuf {
    let dir = std::env::var("CODEGRAPH_DIR").unwrap_or_else(|_| ".codegraph".to_string());
    project_path.join(dir).join("codegraph.db")
}

pub struct WorkspaceRoots {
    roots_list_requested: bool,
}

impl WorkspaceRoots {
    pub const fn new() -> Self {
        Self {
            roots_list_requested: false,
        }
    }

    pub fn should_request_roots(
        &self,
        default_project: Option<&PathBuf>,
        cwd: Option<&Path>,
        params: Option<&Value>,
    ) -> bool {
        if self.roots_list_requested || !default_is_adoptable(default_project, cwd) {
            return false;
        }
        params
            .and_then(|p| p.get("capabilities"))
            .and_then(|c| c.get("roots"))
            .is_some()
    }

    pub fn mark_roots_list_requested(&mut self) {
        self.roots_list_requested = true;
    }

    pub fn adopt_from_initialize(
        &self,
        default_project: &mut Option<PathBuf>,
        cwd: Option<&Path>,
        params: Option<&Value>,
    ) -> Option<PathBuf> {
        let params = params?;
        let path = params
            .get("rootUri")
            .and_then(Value::as_str)
            .and_then(file_uri_to_path)
            .or_else(|| {
                params
                    .get("rootPath")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(|| {
                params
                    .get("workspaceFolders")
                    .and_then(Value::as_array)
                    .and_then(|folders| folders.first())
                    .and_then(|folder| folder.get("uri"))
                    .and_then(Value::as_str)
                    .and_then(file_uri_to_path)
            });
        let path = path?;
        adopt_path(default_project, cwd, path)
    }

    pub fn adopt_from_roots_result(
        &self,
        default_project: &mut Option<PathBuf>,
        cwd: Option<&Path>,
        result: Option<&Value>,
    ) -> Option<PathBuf> {
        let roots = result
            .and_then(|r| r.get("roots"))
            .and_then(Value::as_array)?;
        for root in roots {
            let Some(path) = root
                .get("uri")
                .and_then(Value::as_str)
                .and_then(file_uri_to_path)
                .or_else(|| root.get("path").and_then(Value::as_str).map(PathBuf::from))
            else {
                continue;
            };
            if let Some(adopted) = adopt_path(default_project, cwd, path) {
                return Some(adopted);
            }
        }
        None
    }
}

pub fn roots_list_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": ROOTS_LIST_REQUEST_ID,
        "method": "roots/list",
    })
}

fn adopt_path(
    default_project: &mut Option<PathBuf>,
    cwd: Option<&Path>,
    path: PathBuf,
) -> Option<PathBuf> {
    if default_project.as_ref() == Some(&path) {
        return None;
    }
    if !default_is_adoptable(default_project.as_ref(), cwd) {
        return None;
    }
    if db_path_for(&path).is_file() {
        let adopted = path.clone();
        *default_project = Some(path);
        return Some(adopted);
    }
    None
}

fn default_is_absent_or_home(default_project: Option<&PathBuf>) -> bool {
    let Some(current) = default_project else {
        return true;
    };
    let Some(home) = home_dir() else {
        return false;
    };
    canonicalize_lenient(current) == canonicalize_lenient(&home)
}

// Displaceable = absent/HOME, OR an unindexed default equal to the process cwd
// (the Zed cwd-derived case). An explicit indexed `--path X` stays protected.
fn default_is_adoptable(default_project: Option<&PathBuf>, cwd: Option<&Path>) -> bool {
    if default_is_absent_or_home(default_project) {
        return true;
    }
    let (Some(current), Some(cwd)) = (default_project, cwd) else {
        return false;
    };
    !db_path_for(current).is_file() && canonicalize_lenient(current) == canonicalize_lenient(cwd)
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path_part = rest.find('/').map(|idx| &rest[idx..]).unwrap_or(rest);
    let decoded = percent_decode(path_part);
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(decoded))
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn canonicalize_lenient(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| path.components().collect::<PathBuf>())
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct TempProject {
        path: PathBuf,
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    impl TempProject {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn indexed_project(tag: &str) -> TempProject {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("cg-mcp-roots-{tag}-{}-{seq}", std::process::id()));
        let db = db_path_for(&path);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"placeholder").unwrap();
        TempProject { path }
    }

    // Real on-disk dir (so canonicalize succeeds for the == cwd compare) with no db.
    fn unindexed_dir(tag: &str) -> TempProject {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cg-mcp-roots-unidx-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempProject { path }
    }

    #[test]
    fn initialize_workspace_folders_adopts_indexed_workspace() {
        let project = indexed_project("wsfolders");
        let uri = format!("file://{}", project.path().display());
        let mut default_project = None;
        WorkspaceRoots::new().adopt_from_initialize(
            &mut default_project,
            None,
            Some(&json!({ "workspaceFolders": [{ "uri": uri, "name": "proj" }] })),
        );
        assert_eq!(default_project.as_deref(), Some(project.path()));
    }

    #[test]
    fn initialize_does_not_override_explicit_non_home_default() {
        let explicit = indexed_project("explicit");
        let hinted = indexed_project("hinted");
        let uri = format!("file://{}", hinted.path().display());
        let mut default_project = Some(explicit.path().to_path_buf());
        WorkspaceRoots::new().adopt_from_initialize(
            &mut default_project,
            None,
            Some(&json!({ "rootUri": uri })),
        );
        assert_eq!(default_project.as_deref(), Some(explicit.path()));
    }

    #[test]
    fn initialize_unindexed_workspace_does_not_displace_default() {
        let unindexed = std::env::temp_dir().join("cg-mcp-roots-unindexed-never");
        let uri = format!("file://{}", unindexed.display());
        let mut default_project = None;
        WorkspaceRoots::new().adopt_from_initialize(
            &mut default_project,
            None,
            Some(&json!({ "rootUri": uri })),
        );
        assert_eq!(default_project, None);
    }

    #[test]
    fn requests_roots_when_client_supports_roots_and_default_is_home() {
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let roots = WorkspaceRoots::new();
        assert!(roots.should_request_roots(
            Some(&home),
            None,
            Some(&json!({ "capabilities": { "roots": { "listChanged": true } } }))
        ));
    }

    #[test]
    fn roots_list_response_adopts_first_indexed_workspace() {
        let project = indexed_project("roots-list");
        let unindexed = std::env::temp_dir().join("cg-mcp-roots-unindexed-never");
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let mut default_project = Some(home);

        WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            None,
            Some(&json!({
                "roots": [
                    { "uri": format!("file://{}", unindexed.display()), "name": "empty" },
                    { "uri": format!("file://{}", project.path().display()), "name": "proj" }
                ]
            })),
        );

        assert_eq!(default_project.as_deref(), Some(project.path()));
    }

    #[test]
    fn roots_list_response_does_not_override_explicit_non_home_default() {
        let explicit = indexed_project("roots-explicit");
        let hinted = indexed_project("roots-hinted");
        let mut default_project = Some(explicit.path().to_path_buf());

        WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            None,
            Some(&json!({
                "roots": [{ "uri": format!("file://{}", hinted.path().display()), "name": "hinted" }]
            })),
        );

        assert_eq!(default_project.as_deref(), Some(explicit.path()));
    }

    #[test]
    fn roots_list_adopts_indexed_root_when_default_is_unindexed_cwd() {
        let cwd = unindexed_dir("zed-cwd");
        let project = indexed_project("zed-proj");
        let mut default_project = Some(cwd.path().to_path_buf());

        WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            Some(cwd.path()),
            Some(&json!({
                "roots": [{ "uri": format!("file://{}", project.path().display()), "name": "proj" }]
            })),
        );

        assert_eq!(default_project.as_deref(), Some(project.path()));
    }

    #[test]
    fn does_not_adopt_when_unindexed_default_differs_from_cwd() {
        let explicit = unindexed_dir("explicit-path");
        let cwd = unindexed_dir("elsewhere");
        let project = indexed_project("hinted-proj");
        let mut default_project = Some(explicit.path().to_path_buf());

        WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            Some(cwd.path()),
            Some(&json!({
                "roots": [{ "uri": format!("file://{}", project.path().display()), "name": "proj" }]
            })),
        );

        assert_eq!(default_project.as_deref(), Some(explicit.path()));
    }

    #[test]
    fn does_not_adopt_when_client_root_is_unindexed() {
        let cwd = unindexed_dir("zed-cwd2");
        let reported = unindexed_dir("reported-empty");
        let mut default_project = Some(cwd.path().to_path_buf());

        WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            Some(cwd.path()),
            Some(&json!({
                "roots": [{ "uri": format!("file://{}", reported.path().display()), "name": "empty" }]
            })),
        );

        assert_eq!(default_project.as_deref(), Some(cwd.path()));
    }

    #[test]
    fn should_request_roots_true_when_default_is_unindexed_cwd() {
        let cwd = unindexed_dir("req-cwd");
        let roots = WorkspaceRoots::new();
        assert!(roots.should_request_roots(
            Some(&cwd.path().to_path_buf()),
            Some(cwd.path()),
            Some(&json!({ "capabilities": { "roots": { "listChanged": true } } }))
        ));
    }

    #[test]
    fn should_not_request_roots_when_indexed_non_home_default() {
        let explicit = indexed_project("req-indexed");
        let roots = WorkspaceRoots::new();
        assert!(!roots.should_request_roots(
            Some(&explicit.path().to_path_buf()),
            Some(explicit.path()),
            Some(&json!({ "capabilities": { "roots": { "listChanged": true } } }))
        ));
    }

    #[test]
    fn debug_enabled_honors_truthy_values_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CODEGRAPH_DEBUG").ok();

        std::env::remove_var("CODEGRAPH_DEBUG");
        assert!(!debug_enabled(), "unset ⇒ off");

        std::env::set_var("CODEGRAPH_DEBUG", "1");
        assert!(debug_enabled(), "\"1\" ⇒ on");

        std::env::set_var("CODEGRAPH_DEBUG", "true");
        assert!(debug_enabled(), "\"true\" ⇒ on");

        std::env::set_var("CODEGRAPH_DEBUG", "0");
        assert!(!debug_enabled(), "\"0\" ⇒ off");

        std::env::set_var("CODEGRAPH_DEBUG", "yes");
        assert!(!debug_enabled(), "any other value ⇒ off");

        match prev {
            Some(v) => std::env::set_var("CODEGRAPH_DEBUG", v),
            None => std::env::remove_var("CODEGRAPH_DEBUG"),
        }
    }

    #[test]
    fn format_tool_debug_line_reports_resolved_project_and_db() {
        let project = indexed_project("dbgline");
        let line = format_tool_debug_line(
            "codegraph_search",
            Some("codegraph-rust"),
            Some(project.path()),
            Some(Path::new("/tmp/cwd")),
            Some(Path::new("/tmp/default")),
        );
        let expected = format!(
            "[codegraph debug] tool=codegraph_search projectPath_raw=codegraph-rust resolved={} db={} db_exists=true cwd=/tmp/cwd default_project=/tmp/default",
            project.path().display(),
            db_path_for(project.path()).display(),
        );
        assert_eq!(line, expected);
    }

    #[test]
    fn format_tool_debug_line_marks_unresolved_and_missing() {
        let line = format_tool_debug_line("codegraph_node", None, None, None, None);
        assert_eq!(
            line,
            "[codegraph debug] tool=codegraph_node projectPath_raw=(none) resolved=(unresolved) db=(none) db_exists=false cwd=(none) default_project=(none)"
        );
    }
}
