//! Kiro CLI / IDE target. Ports `upstream installer/targets/kiro.ts`.
//!
//! Writes the MCP entry to `<dir>/.kiro/settings/mcp.json` under
//! `mcpServers.codegraph`, and (formerly) a steering doc at
//! `<dir>/.kiro/steering/codegraph.md` — now only swept on install for
//! self-heal and deleted on uninstall. No permissions concept.

use std::fs;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::super::shared::{
    ConfigRead, mcp_server_config, read_config_file, read_json_file,
    remove_codegraph_from_mcp_servers, to_upstream_json, upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::claude::upsert_mcp_server;

pub struct KiroTarget;

fn config_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => ctx.home.join(".kiro"),
        Location::Local => ctx.cwd.join(".kiro"),
    }
}
fn mcp_json_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("settings").join("mcp.json")
}
fn steering_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    config_dir(ctx, loc).join("steering").join("codegraph.md")
}

const KIRO_GLOBAL_WHY: &str = "The global Kiro entry is read-only off any existing index — tools list and the agent passes projectPath per call (since v0.21.0).";
const KIRO_GLOBAL_HOWTO: &str = "For LIVE auto-update (watcher) run `codegraph init --target=kiro` in each project (writes the project's absolute --path).";

/// Build the project-local Kiro MCP entry with an explicit `--path = ctx.cwd`.
///
/// Kiro's `initialize` carries no `rootUri`/`workspaceFolders` and does not
/// advertise `capabilities.roots`, so a bare `serve --mcp` cannot discover the
/// project and degrades to home safe mode. Pinning the concrete project root
/// fixes that. The global install instead writes a bare `serve --mcp` entry that
/// is read-only off any existing index (the agent passes projectPath per call);
/// for live auto-update run `codegraph init --target=kiro` per project.
fn build_kiro_local_mcp_config(ctx: &InstallContext) -> Value {
    let mut base = mcp_server_config();
    if let Some(args) = base.get_mut("args").and_then(|a| a.as_array_mut()) {
        args.push(json!("--path"));
        args.push(json!(ctx.cwd.to_string_lossy().to_string()));
    }
    base
}

impl AgentTarget for KiroTarget {
    fn id(&self) -> TargetId {
        TargetId::Kiro
    }
    fn display_name(&self) -> &'static str {
        "Kiro"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let file = mcp_json_path(ctx, loc);
        let config = read_json_file(&file);
        let already_configured = config
            .get("mcpServers")
            .and_then(|s| s.get("codegraph"))
            .is_some();
        let installed = config_dir(ctx, loc).exists() || file.exists();
        DetectionResult {
            installed,
            already_configured,
        }
    }

    // Ports kiroTarget.install (kiro.ts:74).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        let mut files = vec![write_mcp_entry(ctx, loc)];
        let cleanup = remove_steering_entry(ctx, loc);
        if cleanup.action == FileAction::Removed {
            files.push(cleanup);
        }
        let notes = match loc {
            Location::Local => vec![
                format!(
                    "CodeGraph MCP configured for project {}.",
                    ctx.cwd.display()
                ),
                "Restart Kiro for MCP changes to take effect.".to_string(),
                "Kiro IDE: also enable MCP in Settings (search \"MCP\" → \"Enabled\"). Kiro CLI users can skip this step."
                    .to_string(),
                "Each project you open in Kiro needs its own install: cd into it and re-run `codegraph install --target=kiro --local`."
                    .to_string(),
            ],
            Location::Global => vec![KIRO_GLOBAL_WHY.to_string(), KIRO_GLOBAL_HOWTO.to_string()],
        };
        WriteResult { files, notes }
    }

    // Ports kiroTarget.uninstall (kiro.ts:99).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let mut files = Vec::new();
        let file = mcp_json_path(ctx, loc);
        let mut config = read_json_file(&file);
        if remove_codegraph_from_mcp_servers(&mut config) {
            let _ = write_json_file(&file, &config);
            files.push(FileWrite {
                path: file,
                action: FileAction::Removed,
            });
        } else {
            files.push(FileWrite {
                path: file,
                action: FileAction::NotFound,
            });
        }
        files.push(remove_steering_entry(ctx, loc));
        WriteResult {
            files,
            notes: Vec::new(),
        }
    }

    // Ports kiroTarget.printConfig (kiro.ts:120).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        match loc {
            Location::Local => {
                let target = mcp_json_path(ctx, loc);
                let snippet = to_upstream_json(
                    &json!({ "mcpServers": { "codegraph": build_kiro_local_mcp_config(ctx) } }),
                );
                format!("# Add to {}\n\n{snippet}\n", target.display())
            }
            Location::Global => {
                let target = mcp_json_path(ctx, loc);
                let snippet = to_upstream_json(
                    &json!({ "mcpServers": { "codegraph": mcp_server_config() } }),
                );
                format!(
                    "# Add to {}\n# {KIRO_GLOBAL_WHY}\n# {KIRO_GLOBAL_HOWTO}\n\n{snippet}\n",
                    target.display()
                )
            }
        }
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        Some(config_dir(ctx, loc).join("skills"))
    }
}

// Ports writeMcpEntry (kiro.ts:131).
//
// Both locations write `mcpServers.codegraph`. Local pins an absolute `--path`
// so the watcher resolves the project; Global writes a bare `serve --mcp` entry
// (no `--path`) — it serves tools read-only off any existing index and the agent
// passes projectPath per call (v0.21.0). For live auto-update run
// `codegraph init --target=kiro` per project.
fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = mcp_json_path(ctx, loc);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let after = match loc {
        Location::Local => build_kiro_local_mcp_config(ctx),
        Location::Global => mcp_server_config(),
    };
    match read_config_file(&file) {
        ConfigRead::Unparseable => FileWrite {
            path: file,
            action: FileAction::Skipped,
        },
        ConfigRead::Missing => {
            let mut config = Map::new();
            upsert_mcp_server(&mut config, "codegraph", after);
            let _ = write_json_file(&file, &config);
            FileWrite {
                path: file,
                action: FileAction::Created,
            }
        }
        ConfigRead::Parsed(_) => {
            let action = upsert_nested_key_jsonc(&file, "mcpServers", "codegraph", &after, None)
                .unwrap_or(FileAction::Skipped);
            FileWrite { path: file, action }
        }
    }
}

// Ports removeSteeringEntry (kiro.ts:158).
fn remove_steering_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = steering_path(ctx, loc);
    if !file.exists() {
        return FileWrite {
            path: file,
            action: FileAction::NotFound,
        };
    }
    let _ = fs::remove_file(&file);
    FileWrite {
        path: file,
        action: FileAction::Removed,
    }
}

pub static KIRO_TARGET: KiroTarget = KiroTarget;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> InstallContext {
        InstallContext {
            home: PathBuf::from("/home/user"),
            cwd: PathBuf::from("/work/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    #[test]
    fn kiro_supports_and_locates_skills_at_both_locations() {
        // Given the Kiro target
        let target = KiroTarget;
        let ctx = ctx();

        // Then it supports skills at both locations
        assert!(target.supports_skills(Location::Global));
        assert!(target.supports_skills(Location::Local));

        // And the PARENT skill dir is `<root>/.kiro/skills` (engine appends codegraph/SKILL.md)
        let global = target.skill_dir(&ctx, Location::Global).unwrap();
        assert!(global.ends_with(".kiro/skills"));
        assert_eq!(global, PathBuf::from("/home/user/.kiro/skills"));

        let local = target.skill_dir(&ctx, Location::Local).unwrap();
        assert!(local.ends_with(".kiro/skills"));
        assert_eq!(local, PathBuf::from("/work/proj/.kiro/skills"));
    }

    #[test]
    fn kiro_local_mcp_entry_pins_concrete_project_path() {
        // Given a local Kiro install context
        let ctx = ctx();

        // When the MCP entry is built for the local location
        let entry = build_kiro_local_mcp_config(&ctx);

        // Then args end with --path pinned to the concrete cwd
        let args = entry["args"].as_array().expect("args array");
        assert_eq!(
            args,
            &vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!("/work/proj"),
            ]
        );
    }

    #[test]
    fn kiro_global_install_writes_bare_mcp_entry_without_path() {
        // Given a global Kiro install into a temp HOME with no prior config
        let home = std::env::temp_dir().join(format!("cg-kiro-global-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let ctx = InstallContext {
            home: home.clone(),
            cwd: PathBuf::from("/work/proj"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        };

        // When a global install runs
        let result = KiroTarget.install(
            &ctx,
            Location::Global,
            InstallOptions {
                auto_allow: true,
                front_load_hook: false,
            },
        );

        // Then the global mcp.json now carries a BARE codegraph entry (no --path)
        let mcp = home.join(".kiro").join("settings").join("mcp.json");
        let config = read_json_file(&mcp);
        let args = config["mcpServers"]["codegraph"]["args"]
            .as_array()
            .expect("global codegraph args array");
        assert_eq!(
            args,
            &vec![json!("serve"), json!("--mcp")],
            "global install must write a bare `serve --mcp` entry with no --path"
        );
        assert!(
            !args.iter().any(|a| a == &json!("--path")),
            "global entry must NOT contain --path"
        );
        // And the notes describe the read-only reality + the per-project live-watch path
        assert!(
            result.notes.iter().any(|n| n.contains("read-only")),
            "global install must explain the entry is read-only"
        );
        assert!(
            result
                .notes
                .iter()
                .any(|n| n.contains("codegraph init --target=kiro")),
            "global install must point at `codegraph init --target=kiro` for live auto-update"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    struct TempKiro {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempKiro {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-kiro-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&base).unwrap();
            let ctx = InstallContext {
                home: base.join("home"),
                cwd: base.join("cwd"),
                app_data: None,
                xdg_config_home: None,
                hermes_home: None,
            };
            fs::create_dir_all(&ctx.cwd).unwrap();
            Self { base, ctx }
        }
        fn read(&self, p: &PathBuf) -> Value {
            serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
        }
    }

    impl Drop for TempKiro {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn opts() -> InstallOptions {
        InstallOptions {
            auto_allow: false,
            front_load_hook: false,
        }
    }

    #[test]
    fn local_install_detect_idempotent_then_uninstall() {
        let fx = TempKiro::new("lifecycle");
        let target = KiroTarget;
        let mcp = mcp_json_path(&fx.ctx, Location::Local);

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(!detect.installed);

        let result = target.install(&fx.ctx, Location::Local, opts());
        assert_eq!(result.files[0].action, FileAction::Created);
        assert!(
            result
                .notes
                .iter()
                .any(|n| n.contains("CodeGraph MCP configured"))
        );
        let args = fx.read(&mcp)["mcpServers"]["codegraph"]["args"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(args[2], "--path");
        assert_eq!(args[3], fx.ctx.cwd.to_string_lossy().as_ref());

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(detect.installed);
        assert!(detect.already_configured);

        let first = fs::read_to_string(&mcp).unwrap();
        target.install(&fx.ctx, Location::Local, opts());
        assert_eq!(fs::read_to_string(&mcp).unwrap(), first, "idempotent");

        let removed = target.uninstall(&fx.ctx, Location::Local);
        assert_eq!(removed.files[0].action, FileAction::Removed);
        let json = fx.read(&mcp);
        assert!(json.get("mcpServers").is_none());
    }

    #[test]
    fn uninstall_missing_is_not_found() {
        let fx = TempKiro::new("uninstall-missing");
        let target = KiroTarget;
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
    }

    #[test]
    fn install_sweeps_stale_steering_doc() {
        let fx = TempKiro::new("steering");
        let target = KiroTarget;
        let steering = steering_path(&fx.ctx, Location::Local);
        fs::create_dir_all(steering.parent().unwrap()).unwrap();
        fs::write(&steering, "stale steering doc\n").unwrap();

        let result = target.install(&fx.ctx, Location::Local, opts());
        assert!(
            result.files.iter().any(|f| f.action == FileAction::Removed),
            "stale steering doc swept on install"
        );
        assert!(!steering.exists());
    }

    #[test]
    fn install_skips_unparseable_config() {
        let fx = TempKiro::new("unparseable");
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        let corrupt = "{ not json";
        fs::write(&mcp, corrupt).unwrap();
        let entry = write_mcp_entry(&fx.ctx, Location::Local);
        assert_eq!(entry.action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&mcp).unwrap(), corrupt);
    }

    #[test]
    fn preserves_sibling_server_on_install() {
        let fx = TempKiro::new("preserve");
        let target = KiroTarget;
        let mcp = mcp_json_path(&fx.ctx, Location::Local);
        fs::create_dir_all(mcp.parent().unwrap()).unwrap();
        fs::write(
            &mcp,
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"foo\" } }\n}\n",
        )
        .unwrap();
        target.install(&fx.ctx, Location::Local, opts());
        let json = fx.read(&mcp);
        assert!(json["mcpServers"]["other"].is_object());
        assert!(json["mcpServers"]["codegraph"].is_object());
    }

    #[test]
    fn print_config_local_and_global_differ() {
        let target = KiroTarget;
        let ctx = ctx();
        let local = target.print_config(&ctx, Location::Local);
        assert!(local.contains("--path"));
        assert!(local.contains("mcp.json"));

        let global = target.print_config(&ctx, Location::Global);
        assert!(global.contains(KIRO_GLOBAL_WHY));
        assert!(global.contains(KIRO_GLOBAL_HOWTO));
        let global_args = global.split("\"args\"").nth(1).expect("args block present");
        assert!(
            !global_args.contains("--path"),
            "global entry args must not pin --path"
        );
    }
}
