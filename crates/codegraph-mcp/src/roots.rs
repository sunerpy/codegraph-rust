//! Workspace-root discovery for clients that launch the MCP server globally.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const ROOTS_LIST_REQUEST_ID: &str = "codegraph-roots-list-1";

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
        params: Option<&Value>,
    ) -> bool {
        if self.roots_list_requested || !default_is_absent_or_home(default_project) {
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
        adopt_path(default_project, path)
    }

    pub fn adopt_from_roots_result(
        &self,
        default_project: &mut Option<PathBuf>,
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
            if let Some(adopted) = adopt_path(default_project, path) {
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

fn adopt_path(default_project: &mut Option<PathBuf>, path: PathBuf) -> Option<PathBuf> {
    if default_project.as_ref() == Some(&path) {
        return None;
    }
    if !default_is_absent_or_home(default_project.as_ref()) {
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

    static SEQ: AtomicU64 = AtomicU64::new(0);

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

    #[test]
    fn initialize_workspace_folders_adopts_indexed_workspace() {
        let project = indexed_project("wsfolders");
        let uri = format!("file://{}", project.path().display());
        let mut default_project = None;
        WorkspaceRoots::new().adopt_from_initialize(
            &mut default_project,
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
        WorkspaceRoots::new()
            .adopt_from_initialize(&mut default_project, Some(&json!({ "rootUri": uri })));
        assert_eq!(default_project.as_deref(), Some(explicit.path()));
    }

    #[test]
    fn initialize_unindexed_workspace_does_not_displace_default() {
        let unindexed = std::env::temp_dir().join("cg-mcp-roots-unindexed-never");
        let uri = format!("file://{}", unindexed.display());
        let mut default_project = None;
        WorkspaceRoots::new()
            .adopt_from_initialize(&mut default_project, Some(&json!({ "rootUri": uri })));
        assert_eq!(default_project, None);
    }

    #[test]
    fn requests_roots_when_client_supports_roots_and_default_is_home() {
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let roots = WorkspaceRoots::new();
        assert!(roots.should_request_roots(
            Some(&home),
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
            Some(&json!({
                "roots": [{ "uri": format!("file://{}", hinted.path().display()), "name": "hinted" }]
            })),
        );

        assert_eq!(default_project.as_deref(), Some(explicit.path()));
    }
}
