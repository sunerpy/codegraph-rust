//! Workspace-root discovery for clients that launch the MCP server globally.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const ROOTS_LIST_REQUEST_ID: &str = "codegraph-roots-list-1";

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
    let (resolved_str, db_str, db_exists) =
        match resolved.and_then(|p| db_path_for(p).map(|db| (p, db))) {
            Some((p, db)) => {
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

/// The current (v2) index DB path under a project root, resolved fail-closed
/// through the single `codegraph-core::IndexPaths` authority so it agrees with
/// [`crate::CodeGraphEngine::open`]. Returns `None` when the configured root is
/// unsafe/aliased/overlapping (a `resolve` failure): callers treat that as "not
/// an indexed project", which is DISTINCT from a reconstructed default path that
/// could shadow another project. This helper is for path DISPLAY only and
/// deliberately drops the reason; the authoritative fail-closed diagnostic comes
/// from the typed [`probe_root`] / [`resolve_project_arg`] states, which reject
/// an invalid configured root BEFORE any engine is opened.
pub fn db_path_for(project_path: &Path) -> Option<PathBuf> {
    codegraph_core::IndexPaths::resolve(
        project_path,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    )
    .ok()
    .map(|paths| paths.current_db())
}

/// Whether `project_path` resolves to an existing current-namespace index DB.
/// `false` for both an unresolvable configured root and a resolvable-but-absent
/// index — the shared adoption / `tools/list`-schema predicate. Adoption and the
/// schema selector only ask "is there a usable index here?", so collapsing the
/// invalid-config and absent cases to `false` is correct FOR THEM. A tool CALL
/// needs the finer [`probe_root`] to tell an unsafe configured root apart from an
/// absent index and surface the actionable diagnostic instead of a generic miss.
pub fn db_exists_for(project_path: &Path) -> bool {
    matches!(probe_root(project_path), RootStatus::Indexed)
}

/// Fail-closed classification of one project-root candidate under the current
/// `CODEGRAPH_DIR`. Distinguishes an UNSAFE configured root (an
/// [`codegraph_core::IndexPaths::resolve`] failure — the actionable diagnostic
/// is carried verbatim, NEVER re-parsed from a rendered string) from a valid
/// root whose DB is merely ABSENT, so a tool call can fail closed with the real
/// configuration error rather than a generic "not indexed" miss that would let
/// an invalid root masquerade as an un-init'd one.
#[derive(Debug)]
pub enum RootStatus {
    /// Valid configured root AND its current-namespace DB exists on disk.
    Indexed,
    /// Valid configured root, but its current-namespace DB does not exist yet.
    Absent,
    /// The configured `CODEGRAPH_DIR` is unsafe/aliased/overlapping; the string
    /// is the stable `IndexPaths` diagnostic (its `Display`).
    Invalid(String),
}

/// Classify `project_path` via the single `IndexPaths` authority. NEVER
/// reconstructs a default path on a resolve failure — an invalid configured root
/// yields [`RootStatus::Invalid`], not a fabricated `.codegraph-v2` fallback that
/// could open an unrelated project's database.
pub fn probe_root(project_path: &Path) -> RootStatus {
    classify_resolve(codegraph_core::IndexPaths::resolve(
        project_path,
        std::env::var("CODEGRAPH_DIR").ok().as_deref(),
    ))
}

/// Pure classifier for an [`codegraph_core::IndexPaths::resolve`] outcome, split
/// out so the invalid/absent decision is unit-tested WITHOUT mutating the
/// process-global `CODEGRAPH_DIR`.
fn classify_resolve(
    resolved: Result<codegraph_core::IndexPaths, codegraph_core::IndexPathsError>,
) -> RootStatus {
    use codegraph_core::IndexPathsError;
    match resolved {
        Ok(paths) => {
            if paths.current_db().is_file() {
                RootStatus::Indexed
            } else {
                RootStatus::Absent
            }
        }
        // A project directory that cannot be canonicalized is MISSING, not a bad
        // configuration — a bogus `projectPath` must stay a generic "not indexed"
        // miss (the `codegraph init` case), never an "invalid CODEGRAPH_DIR"
        // error. Every OTHER variant is a genuinely unsafe/aliased/overlapping
        // configured root and fails closed with its stable diagnostic.
        Err(IndexPathsError::ProjectInaccessible { .. }) => RootStatus::Absent,
        Err(err) => RootStatus::Invalid(err.to_string()),
    }
}

/// The outcome of resolving a tool call's `projectPath` argument to a project
/// directory, preserving the invalid-config vs absent-index distinction the
/// caller needs to emit an actionable error rather than a generic miss.
#[derive(Debug)]
pub enum ProjectArg {
    /// Resolved to an indexed project directory.
    Resolved(PathBuf),
    /// A candidate's configured `CODEGRAPH_DIR` is unsafe/aliased/overlapping;
    /// carries the stable `IndexPaths` diagnostic. Fail closed with this rather
    /// than masking it as a generic miss.
    InvalidConfig(String),
    /// Nothing resolved to an existing index (valid roots with absent DBs, or no
    /// candidate matched) — the genuine "run `codegraph init`" case.
    NotIndexed,
}

impl ProjectArg {
    /// The resolved indexed path, if any — for the debug trace and the existing
    /// resolution unit tests (which only assert the resolved/none outcome).
    pub fn resolved(&self) -> Option<&Path> {
        match self {
            ProjectArg::Resolved(p) => Some(p.as_path()),
            _ => None,
        }
    }
}

/// Resolve a caller's `projectPath` to an INDEXED project dir, in the SAME
/// candidate order both server front-ends use: absolute raw → cwd-join → bare
/// raw → default-by-basename; `None` raw → the default project. An INDEXED
/// candidate wins immediately (a valid configured root is honored even if an
/// earlier candidate carried a bad config); otherwise the FIRST invalid-config
/// diagnostic is surfaced (fail closed); otherwise [`ProjectArg::NotIndexed`].
pub fn resolve_project_arg(
    raw: Option<&str>,
    cwd: Option<&Path>,
    default_project: Option<&Path>,
) -> ProjectArg {
    resolve_project_arg_with(raw, cwd, default_project, &probe_root)
}

/// [`resolve_project_arg`] with the per-candidate classifier injected, so the
/// candidate ORDER and the indexed/absent/invalid decision are unit-tested
/// without mutating the process-global `CODEGRAPH_DIR`. Production callers use
/// [`resolve_project_arg`], which injects [`probe_root`].
fn resolve_project_arg_with(
    raw: Option<&str>,
    cwd: Option<&Path>,
    default_project: Option<&Path>,
    probe: &dyn Fn(&Path) -> RootStatus,
) -> ProjectArg {
    let Some(raw) = raw else {
        return match default_project {
            Some(p) => match probe(p) {
                RootStatus::Indexed => ProjectArg::Resolved(p.to_path_buf()),
                RootStatus::Invalid(detail) => ProjectArg::InvalidConfig(detail),
                RootStatus::Absent => ProjectArg::NotIndexed,
            },
            None => ProjectArg::NotIndexed,
        };
    };

    let raw_path = PathBuf::from(raw);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if raw_path.is_absolute() {
        candidates.push(raw_path.clone());
    } else {
        if let Some(cwd) = cwd {
            candidates.push(cwd.join(&raw_path));
        }
        candidates.push(raw_path.clone());
    }
    if let Some(default) = default_project
        && raw_path.file_name() == default.file_name()
    {
        candidates.push(default.to_path_buf());
    }

    let mut first_invalid: Option<String> = None;
    for candidate in &candidates {
        match probe(candidate) {
            RootStatus::Indexed => return ProjectArg::Resolved(candidate.clone()),
            RootStatus::Invalid(detail) if first_invalid.is_none() => {
                first_invalid = Some(detail);
            }
            _ => {}
        }
    }
    match first_invalid {
        Some(detail) => ProjectArg::InvalidConfig(detail),
        None => ProjectArg::NotIndexed,
    }
}

/// The actionable tool-error text for an unsafe/aliased configured root, shared
/// by both front-ends so the wording (and the "unsafe configured root" naming
/// the regressions assert) stays identical.
pub fn invalid_config_message(detail: &str) -> String {
    format!(
        "Invalid CODEGRAPH_DIR configuration: {detail}. The configured index root is \
         unsafe — fix or unset CODEGRAPH_DIR (it must not alias the project root, an \
         ancestor, a symlink/reparse component, or overlap the legacy `.codegraph` \
         root), then re-run `codegraph init`."
    )
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
    if db_exists_for(&path) {
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
    !db_exists_for(current) && canonicalize_lenient(current) == canonicalize_lenient(cwd)
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
        // The project dir must exist before `db_path_for` (which resolves the
        // physical identity) can succeed.
        std::fs::create_dir_all(&path).unwrap();
        let db = db_path_for(&path).expect("default project resolves");
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
            db_path_for(project.path()).unwrap().display(),
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

    #[test]
    fn adopt_from_roots_result_uses_path_key_fallback() {
        let project = indexed_project("path-key");
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let mut default_project = Some(home);
        let adopted = WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            None,
            Some(&json!({
                "roots": [{ "path": project.path().display().to_string(), "name": "p" }]
            })),
        );
        assert_eq!(adopted.as_deref(), Some(project.path()));
        assert_eq!(default_project.as_deref(), Some(project.path()));
    }

    #[test]
    fn adopt_from_roots_result_skips_root_without_uri_or_path() {
        let project = indexed_project("skip-first");
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let mut default_project = Some(home);
        let adopted = WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            None,
            Some(&json!({
                "roots": [
                    { "name": "no-locator-here" },
                    { "uri": format!("file://{}", project.path().display()), "name": "p" }
                ]
            })),
        );
        assert_eq!(adopted.as_deref(), Some(project.path()));
    }

    #[test]
    fn adopt_from_roots_result_none_when_missing_roots_array() {
        let mut default_project: Option<PathBuf> = None;
        assert_eq!(
            WorkspaceRoots::new().adopt_from_roots_result(
                &mut default_project,
                None,
                Some(&json!({ "not_roots": [] })),
            ),
            None
        );
        assert_eq!(
            WorkspaceRoots::new().adopt_from_roots_result(&mut default_project, None, None),
            None
        );
    }

    #[test]
    fn adopt_path_returns_none_when_default_already_equals_path() {
        let project = indexed_project("already");
        let mut default_project = Some(project.path().to_path_buf());
        let adopted = WorkspaceRoots::new().adopt_from_roots_result(
            &mut default_project,
            None,
            Some(&json!({
                "roots": [{ "uri": format!("file://{}", project.path().display()), "name": "p" }]
            })),
        );
        assert_eq!(adopted, None, "re-adopting the current path is a no-op");
        assert_eq!(default_project.as_deref(), Some(project.path()));
    }

    #[test]
    fn adopt_from_initialize_root_path_key_adopts() {
        let project = indexed_project("root-path");
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let mut default_project = Some(home);
        WorkspaceRoots::new().adopt_from_initialize(
            &mut default_project,
            None,
            Some(&json!({ "rootPath": project.path().display().to_string() })),
        );
        assert_eq!(default_project.as_deref(), Some(project.path()));
    }

    #[test]
    fn adopt_from_initialize_none_params_is_none() {
        let mut default_project: Option<PathBuf> = None;
        assert_eq!(
            WorkspaceRoots::new().adopt_from_initialize(&mut default_project, None, None),
            None
        );
    }

    #[test]
    fn adopt_from_initialize_empty_root_path_is_ignored() {
        let mut default_project: Option<PathBuf> = None;
        WorkspaceRoots::new().adopt_from_initialize(
            &mut default_project,
            None,
            Some(&json!({ "rootPath": "" })),
        );
        assert_eq!(default_project, None, "empty rootPath yields no locator");
    }

    #[test]
    fn should_request_roots_false_after_marked_requested() {
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let mut roots = WorkspaceRoots::new();
        roots.mark_roots_list_requested();
        assert!(!roots.should_request_roots(
            Some(&home),
            None,
            Some(&json!({ "capabilities": { "roots": {} } }))
        ));
    }

    #[test]
    fn should_request_roots_false_without_roots_capability() {
        let home = home_dir().unwrap_or_else(std::env::temp_dir);
        let roots = WorkspaceRoots::new();
        assert!(!roots.should_request_roots(
            Some(&home),
            None,
            Some(&json!({ "capabilities": { "sampling": {} } }))
        ));
    }

    #[test]
    fn file_uri_to_path_handles_authority_and_percent_encoding() {
        assert_eq!(
            file_uri_to_path("file://localhost/a%20b/c"),
            Some(PathBuf::from("/a b/c"))
        );
        assert_eq!(
            file_uri_to_path("file:///abs/path"),
            Some(PathBuf::from("/abs/path"))
        );
    }

    #[test]
    fn file_uri_to_path_rejects_non_file_and_empty() {
        assert_eq!(file_uri_to_path("http://example.com/x"), None);
        assert_eq!(
            file_uri_to_path("file://"),
            None,
            "empty path decodes to None"
        );
    }

    #[test]
    fn percent_decode_passes_through_invalid_and_trailing_percent() {
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("a%"), "a%");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%41%42"), "AB");
    }

    #[test]
    fn db_path_for_honors_codegraph_dir_default() {
        // A real dir so `resolve` (which canonicalizes) succeeds; the default
        // current DB is `<project>/.codegraph-v2/codegraph.db`.
        let dir = std::env::temp_dir().join(format!(
            "cg-roots-dbpath-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = db_path_for(&dir).expect("resolve default");
        assert!(
            db.ends_with(".codegraph-v2/codegraph.db"),
            "default db is under .codegraph-v2: {}",
            db.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `resolve` failure (here a nonexistent project) must yield `None`, not a
    /// reconstructed `.codegraph-v2` default that could shadow another project.
    /// Race-free: it never mutates the process-global `CODEGRAPH_DIR`, unlike an
    /// env-set test. The invalid-`CODEGRAPH_DIR` path is covered end-to-end by
    /// the real CLI/MCP black-box regressions.
    #[test]
    fn db_path_for_returns_none_on_resolve_failure() {
        let missing = std::env::temp_dir().join(format!(
            "cg-roots-dbpath-missing-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!missing.exists(), "sanity: the probe path must not exist");
        assert!(
            db_path_for(&missing).is_none(),
            "a resolve failure must not resolve to a reconstructed default path"
        );
        assert!(
            !db_exists_for(&missing),
            "db_exists_for must be false when resolution fails"
        );
    }

    /// `classify_resolve` on a VALID configured root whose DB is present →
    /// [`RootStatus::Indexed`]. Race-free: `IndexPaths::resolve` is called with an
    /// explicit `codegraph_dir` argument, never the process-global env.
    #[test]
    fn classify_resolve_valid_present_is_indexed() {
        let dir = std::env::temp_dir().join(format!(
            "cg-roots-classify-present-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = codegraph_core::IndexPaths::resolve(&dir, None).expect("resolve default");
        let db = paths.current_db();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"placeholder").unwrap();
        assert!(matches!(
            classify_resolve(codegraph_core::IndexPaths::resolve(&dir, None)),
            RootStatus::Indexed
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `classify_resolve` on a VALID configured root whose DB is ABSENT →
    /// [`RootStatus::Absent`] (the genuine un-init'd case, not an error).
    #[test]
    fn classify_resolve_valid_absent_is_absent() {
        let dir = std::env::temp_dir().join(format!(
            "cg-roots-classify-absent-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            classify_resolve(codegraph_core::IndexPaths::resolve(&dir, None)),
            RootStatus::Absent
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `classify_resolve` on an UNSAFE configured root (`.` aliases the project
    /// root) → [`RootStatus::Invalid`] carrying the stable diagnostic. Race-free:
    /// the bad value is passed as an argument, not via the global env.
    #[test]
    fn classify_resolve_invalid_config_is_invalid_with_diagnostic() {
        let dir = std::env::temp_dir().join(format!(
            "cg-roots-classify-invalid-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        match classify_resolve(codegraph_core::IndexPaths::resolve(&dir, Some("."))) {
            RootStatus::Invalid(detail) => assert!(
                detail.contains("project root"),
                "invalid diagnostic must name the aliasing reason: {detail}"
            ),
            other => panic!("`.` alias must classify Invalid, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A MISSING project directory is not a bad configuration: it classifies
    /// [`RootStatus::Absent`] (generic "not indexed"), never `Invalid`.
    #[test]
    fn classify_resolve_missing_project_is_absent_not_invalid() {
        let missing = std::env::temp_dir().join(format!(
            "cg-roots-classify-missing-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!missing.exists(), "sanity: probe path must not exist");
        assert!(matches!(
            classify_resolve(codegraph_core::IndexPaths::resolve(&missing, None)),
            RootStatus::Absent
        ));
    }

    /// A stub classifier keyed by path, so candidate ORDER and the three typed
    /// states are asserted without touching the filesystem or the env.
    fn stub_probe(map: Vec<(PathBuf, RootStatus)>) -> impl Fn(&Path) -> RootStatus {
        move |candidate: &Path| {
            for (path, status) in &map {
                if path == candidate {
                    return match status {
                        RootStatus::Indexed => RootStatus::Indexed,
                        RootStatus::Absent => RootStatus::Absent,
                        RootStatus::Invalid(d) => RootStatus::Invalid(d.clone()),
                    };
                }
            }
            RootStatus::Absent
        }
    }

    /// `raw = None` + an INVALID default root ⇒ [`ProjectArg::InvalidConfig`]
    /// carrying the diagnostic — NOT `NotIndexed`. This is the exact Failure-B
    /// masking: a bad `CODEGRAPH_DIR` used to collapse into "no indexed project".
    #[test]
    fn resolve_project_arg_none_invalid_default_is_invalid_config() {
        let default = PathBuf::from("/tmp/cg-stub-default");
        let probe = stub_probe(vec![(
            default.clone(),
            RootStatus::Invalid("unsafe root: project root itself".to_string()),
        )]);
        match resolve_project_arg_with(None, None, Some(&default), &probe) {
            ProjectArg::InvalidConfig(detail) => {
                assert!(detail.contains("project root itself"), "{detail}")
            }
            other => panic!("invalid default must fail closed, got {other:?}"),
        }
    }

    /// An INDEXED candidate wins over an EARLIER invalid one: a valid configured
    /// root must still resolve, so the fail-closed behavior cannot regress into
    /// refusing every call whenever any candidate happens to be misconfigured.
    #[test]
    fn resolve_project_arg_indexed_candidate_wins_over_earlier_invalid() {
        let cwd = PathBuf::from("/tmp/cg-stub-cwd");
        let bare = PathBuf::from("proj");
        let joined = cwd.join(&bare);
        let probe = stub_probe(vec![
            (joined, RootStatus::Invalid("bad".to_string())),
            (bare.clone(), RootStatus::Indexed),
        ]);
        assert_eq!(
            resolve_project_arg_with(Some("proj"), Some(&cwd), None, &probe).resolved(),
            Some(bare.as_path()),
            "an indexed later candidate must win over an earlier invalid one"
        );
    }

    /// With NO indexed candidate, the FIRST invalid diagnostic is surfaced (in
    /// candidate order: cwd-join before bare raw), so the reported reason belongs
    /// to the highest-priority misconfigured candidate.
    #[test]
    fn resolve_project_arg_reports_first_invalid_in_candidate_order() {
        let cwd = PathBuf::from("/tmp/cg-stub-cwd2");
        let bare = PathBuf::from("proj");
        let joined = cwd.join(&bare);
        let probe = stub_probe(vec![
            (joined, RootStatus::Invalid("first-invalid".to_string())),
            (bare, RootStatus::Invalid("second-invalid".to_string())),
        ]);
        match resolve_project_arg_with(Some("proj"), Some(&cwd), None, &probe) {
            ProjectArg::InvalidConfig(detail) => assert_eq!(detail, "first-invalid"),
            other => panic!("expected InvalidConfig, got {other:?}"),
        }
    }

    /// All-absent candidates stay the genuine [`ProjectArg::NotIndexed`] "run
    /// `codegraph init`" case — an absent index is not a bad configuration.
    #[test]
    fn resolve_project_arg_all_absent_is_not_indexed() {
        let probe = stub_probe(vec![]);
        assert!(matches!(
            resolve_project_arg_with(Some("/tmp/cg-stub-absolute"), None, None, &probe),
            ProjectArg::NotIndexed
        ));
    }

    /// The shared invalid-config message embeds the verbatim diagnostic AND the
    /// actionable remedy both front-ends surface.
    #[test]
    fn invalid_config_message_carries_detail_and_remedy() {
        let msg = invalid_config_message("refusing an unsafe index root /p: reason");
        assert!(
            msg.contains("refusing an unsafe index root /p: reason"),
            "{msg}"
        );
        assert!(msg.contains("CODEGRAPH_DIR"), "{msg}");
        assert!(msg.contains("unsafe"), "{msg}");
    }
}
