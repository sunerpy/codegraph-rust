//! Qoder IDE target (the rebranded 通义灵码/Lingma). Ports the SharedClientCache
//! MCP install pattern.
//!
//! Qoder's MCP config is NOT the VS Code `User/mcp.json` layout. The working
//! file lives under a dynamic per-install `<machineId>` directory:
//! `<config_base>/{QoderCN|Qoder}/<machineId>/SharedClientCache/mcp.json`, keyed
//! under `mcpServers`. `QoderCN` is the China edition; `Qoder` the international
//! one. The `<machineId>` segment is DYNAMIC — it is DISCOVERED via
//! [`std::fs::read_dir`] (NO glob crate), never hardcoded; when several exist
//! the candidates are sorted lexicographically by the machineId dir name and the
//! first is chosen, so the pick is deterministic regardless of FS read order.
//!
//! The GLOBAL resolution probes IN ORDER:
//!   1. server/remote mode — when `~/.qoder-cn-server` (then `~/.qoder-server`)
//!      exists AND its `data/Machine/mcp.json` exists, that file wins (mirrors
//!      Trae);
//!   2. otherwise discover the SharedClientCache file (QoderCN preferred over
//!      Qoder);
//!   3. otherwise `None` — Qoder is not installed / never launched.
//!
//! The GLOBAL codegraph entry is a BARE `serve --mcp` (NO `--path`, NO
//! `${workspaceFolder}` — substitution is unconfirmed in this layout; the entry
//! is read-only-usable like the Kiro global entry). The LOCAL install writes
//! `<project>/.qoder/mcp.json` with an absolute `--path = cwd` (Qoder
//! project-level path is best-evidence). Skills install to the shared
//! `~/.agents/skills` dir (global) — the SAME dir codex.rs / antigravity.rs use
//! — and `<project>/.qoder/skills` (local).
//!
//! The upsert touches ONLY the `codegraph` key, preserving any sibling servers
//! byte-faithfully. The internal `SharedClientCache/extension/local/mcp.json`
//! mirror is NEVER written.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use super::super::shared::{
    mcp_server_config, read_config_file, read_json_file, remove_codegraph_from_mcp_servers,
    to_upstream_json, upsert_nested_key_jsonc, write_json_file, ConfigRead,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::super::vscode_user::config_base_for;
use super::claude::upsert_mcp_server;

pub struct QoderTarget;

/// Edition dirs under the config base, in preference order (China first, then
/// international) — matched to the `*-server` marker dirs below.
const EDITIONS: [&str; 2] = ["QoderCN", "Qoder"];
/// Server/remote-mode marker dirs in preference order (China first).
const SERVER_DIRS: [&str; 2] = [".qoder-cn-server", ".qoder-server"];

/// The per-OS config base hosting the edition dirs.
fn qoder_config_base(ctx: &InstallContext) -> PathBuf {
    config_base_for(
        &ctx.home,
        ctx.app_data.as_deref(),
        ctx.xdg_config_home.as_deref(),
        std::env::consts::OS,
    )
}

/// Discover the SharedClientCache `mcp.json` for one edition. Reads the edition
/// dir, collects each child `<machineId>` whose
/// `<machineId>/SharedClientCache/mcp.json` is a file, sorts those candidates
/// lexicographically by the machineId dir name (deterministic — `read_dir`
/// order is FS-dependent), and returns the FIRST.
fn discover_shared_client_cache(base: &std::path::Path, edition: &str) -> Option<PathBuf> {
    let edition_dir = base.join(edition);
    if !edition_dir.is_dir() {
        return None;
    }
    let mut candidates: Vec<(std::ffi::OsString, PathBuf)> = Vec::new();
    let Ok(entries) = fs::read_dir(&edition_dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if !child.is_dir() {
            continue;
        }
        let mcp = child.join("SharedClientCache").join("mcp.json");
        if mcp.is_file() {
            candidates.push((entry.file_name(), mcp));
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

/// Resolve the GLOBAL `mcp.json` path, or `None` when Qoder is not present.
/// Probes: (1) server mode (`~/.qoder-cn-server` then `~/.qoder-server` with a
/// `data/Machine/mcp.json`), else (2) SharedClientCache discovery (QoderCN
/// preferred over Qoder).
fn qoder_global_mcp_json(ctx: &InstallContext) -> Option<PathBuf> {
    for server in SERVER_DIRS {
        let marker = ctx.home.join(server);
        if marker.exists() {
            let file = marker.join("data").join("Machine").join("mcp.json");
            if file.is_file() {
                return Some(file);
            }
        }
    }
    let base = qoder_config_base(ctx);
    for edition in EDITIONS {
        if let Some(path) = discover_shared_client_cache(&base, edition) {
            return Some(path);
        }
    }
    None
}

/// A display-only PATTERN path for the GLOBAL config when none is resolved:
/// `<base>/QoderCN/<machineId>/SharedClientCache/mcp.json`.
fn qoder_global_pattern_path(ctx: &InstallContext) -> PathBuf {
    qoder_config_base(ctx)
        .join(EDITIONS[0])
        .join("<machineId>")
        .join("SharedClientCache")
        .join("mcp.json")
}

/// The LOCAL project-level `mcp.json`: `<project>/.qoder/mcp.json`.
fn qoder_local_mcp_json(ctx: &InstallContext) -> PathBuf {
    ctx.cwd.join(".qoder").join("mcp.json")
}

/// Build the codegraph MCP entry. GLOBAL is a BARE `serve --mcp` (read-only,
/// like Kiro global — `${workspaceFolder}` unconfirmed in this layout); LOCAL
/// pins the absolute project path.
fn build_qoder_mcp_config(ctx: &InstallContext, loc: Location) -> Value {
    let mut base = mcp_server_config();
    if loc == Location::Local {
        if let Some(args) = base.get_mut("args").and_then(|a| a.as_array_mut()) {
            args.push(json!("--path"));
            args.push(json!(ctx.cwd.to_string_lossy().to_string()));
        }
    }
    base
}

impl AgentTarget for QoderTarget {
    fn id(&self) -> TargetId {
        TargetId::Qoder
    }
    fn display_name(&self) -> &'static str {
        "Qoder"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        match loc {
            Location::Global => {
                let resolved = qoder_global_mcp_json(ctx);
                let already_configured = resolved
                    .as_ref()
                    .map(|file| {
                        read_json_file(file)
                            .get("mcpServers")
                            .and_then(|s| s.get("codegraph"))
                            .is_some()
                    })
                    .unwrap_or(false);
                DetectionResult {
                    installed: resolved.is_some(),
                    already_configured,
                }
            }
            Location::Local => {
                let file = qoder_local_mcp_json(ctx);
                let already_configured = read_json_file(&file)
                    .get("mcpServers")
                    .and_then(|s| s.get("codegraph"))
                    .is_some();
                DetectionResult {
                    installed: ctx.cwd.join(".qoder").exists() || file.exists(),
                    already_configured,
                }
            }
        }
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        match loc {
            Location::Global => match qoder_global_mcp_json(ctx) {
                Some(file) => WriteResult {
                    files: vec![write_mcp_entry(ctx, loc, &file)],
                    notes: vec![
                        "Qoder global entry is read-only off the existing index (no live watch); the agent passes the project path per call.".to_string(),
                        "For live auto-update run `codegraph init --target=qoder` inside each project.".to_string(),
                        "Restart Qoder for MCP changes to take effect.".to_string(),
                    ],
                },
                None => WriteResult {
                    files: vec![FileWrite {
                        path: qoder_global_pattern_path(ctx),
                        action: FileAction::NotFound,
                    }],
                    notes: vec![
                        "Qoder not detected; launch Qoder once, or use --print-config to copy the entry.".to_string(),
                    ],
                },
            },
            Location::Local => {
                let file = qoder_local_mcp_json(ctx);
                WriteResult {
                    files: vec![write_mcp_entry(ctx, loc, &file)],
                    notes: vec![
                        format!(
                            "CodeGraph MCP configured for project {}.",
                            ctx.cwd.display()
                        ),
                        "Restart Qoder for MCP changes to take effect.".to_string(),
                    ],
                }
            }
        }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let resolved = match loc {
            Location::Global => qoder_global_mcp_json(ctx),
            Location::Local => Some(qoder_local_mcp_json(ctx)),
        };
        let file = match resolved {
            Some(file) => file,
            None => {
                return WriteResult {
                    files: vec![FileWrite {
                        path: qoder_global_pattern_path(ctx),
                        action: FileAction::NotFound,
                    }],
                    notes: Vec::new(),
                };
            }
        };
        let mut config = read_json_file(&file);
        let action = if remove_codegraph_from_mcp_servers(&mut config) {
            let _ = write_json_file(&file, &config);
            FileAction::Removed
        } else {
            FileAction::NotFound
        };
        WriteResult {
            files: vec![FileWrite {
                path: file,
                action,
            }],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let snippet = to_upstream_json(
            &json!({ "mcpServers": { "codegraph": build_qoder_mcp_config(ctx, loc) } }),
        );
        match loc {
            Location::Global => match qoder_global_mcp_json(ctx) {
                Some(file) => format!("# Add to {}\n\n{snippet}\n", file.display()),
                None => format!(
                    "# Add to {} (launch Qoder first to create the <machineId> dir)\n\n{snippet}\n",
                    qoder_global_pattern_path(ctx).display()
                ),
            },
            Location::Local => format!(
                "# Add to {}\n\n{snippet}\n",
                qoder_local_mcp_json(ctx).display()
            ),
        }
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        // Global skills install to the SHARED `~/.agents/skills` dir (the same
        // dir codex.rs / antigravity.rs use); local skills under
        // `<project>/.qoder/skills`.
        let dir = match loc {
            Location::Global => ctx.home.join(".agents").join("skills"),
            Location::Local => ctx.cwd.join(".qoder").join("skills"),
        };
        Some(dir)
    }
}

// Upsert the codegraph entry into the resolved file. ALWAYS `create_dir_all`s the
// parent first (mirror kiro.rs / trae.rs) — the SharedClientCache / `.qoder` dir
// is not guaranteed to pre-exist on the LOCAL path.
fn write_mcp_entry(ctx: &InstallContext, loc: Location, file: &std::path::Path) -> FileWrite {
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let after = build_qoder_mcp_config(ctx, loc);
    match read_config_file(file) {
        ConfigRead::Unparseable => FileWrite {
            path: file.to_path_buf(),
            action: FileAction::Skipped,
        },
        ConfigRead::Missing => {
            let mut config = Map::new();
            upsert_mcp_server(&mut config, "codegraph", after);
            let _ = write_json_file(file, &config);
            FileWrite {
                path: file.to_path_buf(),
                action: FileAction::Created,
            }
        }
        ConfigRead::Parsed(_) => {
            let action = upsert_nested_key_jsonc(file, "mcpServers", "codegraph", &after, None)
                .unwrap_or(FileAction::Skipped);
            FileWrite {
                path: file.to_path_buf(),
                action,
            }
        }
    }
}

pub static QODER_TARGET: QoderTarget = QoderTarget;

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp-rooted context so the probe never hits the real `~/.qoder-*` or
    /// `~/.config/QoderCN`. `xdg_config_home` points into the temp home so
    /// `config_base_for` on Linux resolves there (not the real `~/.config`).
    fn temp_ctx(label: &str) -> (InstallContext, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "cg-qoder-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = base.join("home");
        let ctx = InstallContext {
            home: home.clone(),
            cwd: base.join("cwd"),
            app_data: None,
            xdg_config_home: Some(home.join(".config")),
            hermes_home: None,
        };
        (ctx, base)
    }

    #[test]
    fn discovers_shared_client_cache_under_qoder_cn() {
        // Given a temp `<home>/.config/QoderCN/ABC123/SharedClientCache/mcp.json`
        let (ctx, base) = temp_ctx("discover");
        let mcp = ctx
            .home
            .join(".config")
            .join("QoderCN")
            .join("ABC123")
            .join("SharedClientCache")
            .join("mcp.json");
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        fs::write(&mcp, "{\n  \"mcpServers\": {}\n}\n").unwrap();

        // When resolving the global mcp.json
        let resolved = qoder_global_mcp_json(&ctx).expect("should discover");

        // Then it is that SharedClientCache file
        assert_eq!(resolved, mcp, "got {}", resolved.display());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn discovery_sort_is_deterministic_lexicographic_first() {
        // Given TWO machineId dirs each with a SharedClientCache/mcp.json
        let (ctx, base) = temp_ctx("sort");
        let qoder_cn = ctx.home.join(".config").join("QoderCN");
        for id in ["BBB", "AAA"] {
            let mcp = qoder_cn.join(id).join("SharedClientCache").join("mcp.json");
            fs::create_dir_all(mcp.parent().unwrap()).unwrap();
            fs::write(&mcp, "{\"mcpServers\":{}}").unwrap();
        }

        // When resolving, Then the lexicographically-first ("AAA") is chosen.
        let resolved = qoder_global_mcp_json(&ctx).expect("should discover");
        assert!(
            resolved.ends_with("QoderCN/AAA/SharedClientCache/mcp.json"),
            "expected AAA, got {}",
            resolved.display()
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn qoder_cn_edition_preferred_over_qoder() {
        // Given BOTH QoderCN and Qoder editions present
        let (ctx, base) = temp_ctx("edition");
        for edition in ["QoderCN", "Qoder"] {
            let mcp = ctx
                .home
                .join(".config")
                .join(edition)
                .join("ID")
                .join("SharedClientCache")
                .join("mcp.json");
            fs::create_dir_all(mcp.parent().unwrap()).unwrap();
            fs::write(&mcp, "{\"mcpServers\":{}}").unwrap();
        }

        // When resolving, Then QoderCN wins.
        let resolved = qoder_global_mcp_json(&ctx).expect("should discover");
        assert!(
            resolved.to_string_lossy().contains("QoderCN"),
            "QoderCN must be preferred, got {}",
            resolved.display()
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn server_mode_takes_precedence_over_shared_client_cache() {
        // Given BOTH a server-mode mcp.json AND a SharedClientCache mcp.json
        let (ctx, base) = temp_ctx("servermode");
        let server = ctx
            .home
            .join(".qoder-cn-server")
            .join("data")
            .join("Machine")
            .join("mcp.json");
        fs::create_dir_all(server.parent().unwrap()).unwrap();
        fs::write(&server, "{\"mcpServers\":{}}").unwrap();
        let cache = ctx
            .home
            .join(".config")
            .join("QoderCN")
            .join("ID")
            .join("SharedClientCache")
            .join("mcp.json");
        fs::create_dir_all(cache.parent().unwrap()).unwrap();
        fs::write(&cache, "{\"mcpServers\":{}}").unwrap();

        // When resolving, Then server mode wins.
        let resolved = qoder_global_mcp_json(&ctx).expect("should resolve");
        assert_eq!(resolved, server, "server mode must win, got {}", resolved.display());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn returns_none_when_no_qoder_dir() {
        // Given a temp home with NO qoder dirs at all
        let (ctx, base) = temp_ctx("none");

        // When resolving, Then None.
        assert!(qoder_global_mcp_json(&ctx).is_none());

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn global_build_is_bare_no_path() {
        let (ctx, base) = temp_ctx("globalbuild");
        let entry = build_qoder_mcp_config(&ctx, Location::Global);
        let args = entry["args"].as_array().expect("args array");
        assert_eq!(
            args,
            &vec![json!("serve"), json!("--mcp")],
            "global entry must be bare (no --path)"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn local_build_uses_absolute_cwd() {
        let (ctx, base) = temp_ctx("localbuild");
        let entry = build_qoder_mcp_config(&ctx, Location::Local);
        let args = entry["args"].as_array().expect("args array");
        assert_eq!(
            args,
            &vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!(ctx.cwd.to_string_lossy().to_string()),
            ]
        );
        let path = qoder_local_mcp_json(&ctx);
        assert!(path.ends_with(".qoder/mcp.json"), "got {}", path.display());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn global_install_upserts_preserving_siblings() {
        // Given a discovered SharedClientCache mcp.json pre-seeded with a sibling
        let (ctx, base) = temp_ctx("siblings");
        let mcp = ctx
            .home
            .join(".config")
            .join("QoderCN")
            .join("ID")
            .join("SharedClientCache")
            .join("mcp.json");
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        fs::write(
            &mcp,
            "{\n  \"mcpServers\": {\n    \"github\": { \"command\": \"gh-mcp\", \"args\": [] }\n  }\n}\n",
        )
        .unwrap();

        // When codegraph is upserted via a global install
        let result = QoderTarget.install(
            &ctx,
            Location::Global,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );
        assert_eq!(result.files[0].action, FileAction::Updated);

        // Then the github sibling survives, and codegraph is BARE (no --path).
        let config = read_json_file(&mcp);
        assert_eq!(
            config["mcpServers"]["github"]["command"],
            json!("gh-mcp"),
            "sibling server must survive"
        );
        let cg_args = config["mcpServers"]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args");
        assert_eq!(
            cg_args,
            &vec![json!("serve"), json!("--mcp")],
            "global codegraph entry must be bare"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn local_install_writes_absolute_path() {
        // Given a fresh temp cwd (no .qoder dir)
        let (ctx, base) = temp_ctx("localinstall");
        let file = qoder_local_mcp_json(&ctx);
        assert!(!file.exists(), "precondition: file absent");

        // When a local install runs
        let result = QoderTarget.install(
            &ctx,
            Location::Local,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );
        assert_eq!(result.files[0].action, FileAction::Created);
        assert!(file.exists(), "file must exist: {}", file.display());

        // Then the entry has the absolute --path.
        let config = read_json_file(&file);
        let cg_args = config["mcpServers"]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args");
        assert!(
            cg_args.iter().any(|a| a == &json!("--path")),
            "local entry must have --path"
        );
        assert!(
            cg_args
                .iter()
                .any(|a| a == &json!(ctx.cwd.to_string_lossy().to_string())),
            "local entry must have abs cwd"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn global_install_not_found_when_qoder_absent() {
        // Given a temp home with NO qoder dir
        let (ctx, base) = temp_ctx("notfound");

        // When a global install runs
        let result = QoderTarget.install(
            &ctx,
            Location::Global,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );

        // Then it reports NotFound (no fabricated machineId dir) + a note.
        assert_eq!(result.files[0].action, FileAction::NotFound);
        assert!(
            result.files[0]
                .path
                .to_string_lossy()
                .contains("<machineId>"),
            "NotFound path should be the pattern, got {}",
            result.files[0].path.display()
        );
        assert!(
            result.notes.iter().any(|n| n.contains("launch Qoder")),
            "should note launching Qoder"
        );
        // And no dir was fabricated.
        assert!(
            !ctx.home.join(".config").join("QoderCN").exists(),
            "must NOT fabricate a machineId dir"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_removes_only_codegraph() {
        // Given a discovered mcp.json with github + codegraph
        let (ctx, base) = temp_ctx("uninstall");
        let mcp = ctx
            .home
            .join(".config")
            .join("QoderCN")
            .join("ID")
            .join("SharedClientCache")
            .join("mcp.json");
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        fs::write(
            &mcp,
            "{\n  \"mcpServers\": {\n    \"github\": { \"command\": \"gh-mcp\" },\n    \"codegraph\": { \"command\": \"codegraph\" }\n  }\n}\n",
        )
        .unwrap();

        // When uninstalling
        let result = QoderTarget.uninstall(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::Removed);

        // Then github survives and codegraph is gone.
        let config = read_json_file(&mcp);
        assert!(config["mcpServers"].get("github").is_some(), "github survives");
        assert!(
            config["mcpServers"].get("codegraph").is_none(),
            "codegraph removed"
        );

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_reports_not_found_when_qoder_absent() {
        let (ctx, base) = temp_ctx("uninstall-absent");
        let result = QoderTarget.uninstall(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn skill_dir_global_ends_with_agents_skills() {
        let (ctx, base) = temp_ctx("skillglobal");
        let dir = QoderTarget.skill_dir(&ctx, Location::Global).unwrap();
        assert!(dir.ends_with(".agents/skills"), "got {}", dir.display());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn skill_dir_local_ends_with_qoder_skills() {
        let (ctx, base) = temp_ctx("skilllocal");
        let dir = QoderTarget.skill_dir(&ctx, Location::Local).unwrap();
        assert!(dir.ends_with(".qoder/skills"), "got {}", dir.display());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn supports_skills_both_locations() {
        assert!(QoderTarget.supports_skills(Location::Global));
        assert!(QoderTarget.supports_skills(Location::Local));
    }
}
