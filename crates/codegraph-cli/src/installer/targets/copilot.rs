//! Copilot install targets (upstream `490791c` + follow-up fixes): VS Code,
//! Copilot CLI, and JetBrains.
//!
//! All three are GitHub Copilot MCP surfaces but disagree on both the wrapper
//! key and the available locations, which is why they are three targets rather
//! than one parametrized target:
//!
//! | target      | file                                          | wrapper      | locations    |
//! | ----------- | --------------------------------------------- | ------------ | ------------ |
//! | vscode      | `.vscode/mcp.json` (local)                    | `servers`    | local+global |
//! |             | `<config_base>/Code/User/mcp.json` (global)   | `servers`    |              |
//! | copilot-cli | `~/.copilot/mcp-config.json`                  | `mcpServers` | global only  |
//! | jetbrains   | `~/.config/github-copilot/intellij/mcp.json`  | `servers`    | global only  |
//!
//! Two contracts are load-bearing and each has a dedicated test:
//!
//! * The VS Code GLOBAL entry must NOT use `${workspaceFolder}`. VS Code
//!   substitutes that variable in a WORKSPACE `mcp.json` only; in the user-level
//!   file it stays a literal, which would make the server point at a directory
//!   named `${workspaceFolder}`. The global entry is therefore BARE (read-only
//!   off any existing index, like the Kiro/Qoder global entries) and the LOCAL
//!   entry pins an absolute `--path`.
//! * The Copilot CLI entry carries `"tools": ["*"]`. Without it the CLI
//!   registers the server but exposes none of its tools.
//!
//! CLI-only and additive: no extraction, resolution, or golden surface is
//! touched. The upsert is JSONC-surgical and only ever writes the `codegraph`
//! key, preserving sibling servers, comments, and key order.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::shared::{
    ConfigRead, mcp_server_config, read_config_file, read_json_file, remove_nested_key_jsonc,
    to_upstream_json, upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};
use super::super::vscode_user::config_base_for;

/// The VS Code user-level app dir hosting `User/mcp.json`.
const VSCODE_APP_DIR: &str = "Code";

const VSCODE_GLOBAL_WHY: &str = "The global VS Code entry is bare `serve --mcp` (read-only off any existing index); VS Code expands ${workspaceFolder} only in a WORKSPACE mcp.json, never in the user-level one.";
const VSCODE_GLOBAL_HOWTO: &str = "For LIVE auto-update run `codegraph init --target=vscode` in each project (writes .vscode/mcp.json with that project's absolute --path).";

/// `<config_base>/Code/User/mcp.json`.
fn vscode_global_mcp_json(ctx: &InstallContext) -> PathBuf {
    config_base_for(
        &ctx.home,
        ctx.app_data.as_deref(),
        ctx.xdg_config_home.as_deref(),
        std::env::consts::OS,
    )
    .join(VSCODE_APP_DIR)
    .join("User")
    .join("mcp.json")
}

/// `<project>/.vscode/mcp.json`.
fn vscode_local_mcp_json(ctx: &InstallContext) -> PathBuf {
    ctx.cwd.join(".vscode").join("mcp.json")
}

/// `~/.copilot/mcp-config.json`.
fn copilot_cli_mcp_json(ctx: &InstallContext) -> PathBuf {
    ctx.home.join(".copilot").join("mcp-config.json")
}

/// `<xdg_config_home | ~/.config>/github-copilot/intellij/mcp.json`.
///
/// JetBrains' Copilot plugin uses this XDG-style path on every OS, so it does
/// NOT go through [`config_base_for`] (which would resolve to
/// `Library/Application Support` on macOS and `AppData/Roaming` on Windows).
fn jetbrains_mcp_json(ctx: &InstallContext) -> PathBuf {
    ctx.xdg_config_home
        .clone()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| ctx.home.join(".config"))
        .join("github-copilot")
        .join("intellij")
        .join("mcp.json")
}

/// The entry with an absolute `--path`, for a project-scoped config.
fn entry_with_project_path(ctx: &InstallContext) -> Value {
    let mut base = mcp_server_config();
    if let Some(args) = base.get_mut("args").and_then(|a| a.as_array_mut()) {
        args.push(json!("--path"));
        args.push(json!(ctx.cwd.to_string_lossy().to_string()));
    }
    base
}

/// The Copilot CLI entry: the shared stdio block plus `"tools": ["*"]`, without
/// which the CLI registers the server but exposes none of its tools.
fn copilot_cli_entry() -> Value {
    let mut base = mcp_server_config();
    if let Value::Object(map) = &mut base {
        map.insert("tools".to_string(), json!(["*"]));
    }
    base
}

/// Detect via "the config file's wrapper already holds a codegraph key", plus
/// "the agent's own directory exists" for installed-ness.
fn detect_at(file: &Path, marker_dir: &Path, wrapper: &str) -> DetectionResult {
    let already_configured = read_json_file(file)
        .get(wrapper)
        .and_then(|s| s.get("codegraph"))
        .is_some();
    DetectionResult {
        installed: marker_dir.exists() || file.exists(),
        already_configured,
    }
}

/// JSONC-surgical upsert of `<wrapper>.codegraph = entry`, creating the parent
/// directory and the file when absent.
fn write_entry(file: &Path, wrapper: &str, entry: &Value) -> FileWrite {
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    match read_config_file(file) {
        ConfigRead::Unparseable => FileWrite {
            path: file.to_path_buf(),
            action: FileAction::Skipped,
        },
        ConfigRead::Missing => {
            let mut config = Map::new();
            let mut servers = Map::new();
            servers.insert("codegraph".to_string(), entry.clone());
            config.insert(wrapper.to_string(), Value::Object(servers));
            let _ = write_json_file(file, &config);
            FileWrite {
                path: file.to_path_buf(),
                action: FileAction::Created,
            }
        }
        ConfigRead::Parsed(_) => {
            let action = upsert_nested_key_jsonc(file, wrapper, "codegraph", entry, None)
                .unwrap_or(FileAction::Skipped);
            FileWrite {
                path: file.to_path_buf(),
                action,
            }
        }
    }
}

fn remove_entry(file: &Path, wrapper: &str) -> FileWrite {
    let action =
        remove_nested_key_jsonc(file, wrapper, "codegraph").unwrap_or(FileAction::NotFound);
    FileWrite {
        path: file.to_path_buf(),
        action,
    }
}

fn print_snippet(file: &Path, wrapper: &str, entry: &Value, why: Option<&str>) -> String {
    let snippet = to_upstream_json(&json!({ wrapper: { "codegraph": entry } }));
    match why {
        Some(why) => format!("# Add to {}\n# {why}\n\n{snippet}\n", file.display()),
        None => format!("# Add to {}\n\n{snippet}\n", file.display()),
    }
}

// ============================ VS Code ========================================

pub struct VsCodeTarget;

impl AgentTarget for VsCodeTarget {
    fn id(&self) -> TargetId {
        TargetId::VsCode
    }
    fn display_name(&self) -> &'static str {
        "VS Code (GitHub Copilot)"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        match loc {
            Location::Global => {
                let file = vscode_global_mcp_json(ctx);
                let marker = file.parent().map(Path::to_path_buf).unwrap_or_default();
                detect_at(&file, &marker, "servers")
            }
            Location::Local => {
                let file = vscode_local_mcp_json(ctx);
                detect_at(&file, &ctx.cwd.join(".vscode"), "servers")
            }
        }
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        match loc {
            Location::Global => WriteResult {
                files: vec![write_entry(
                    &vscode_global_mcp_json(ctx),
                    "servers",
                    &mcp_server_config(),
                )],
                notes: vec![
                    VSCODE_GLOBAL_WHY.to_string(),
                    VSCODE_GLOBAL_HOWTO.to_string(),
                    "Reload VS Code for MCP changes to take effect.".to_string(),
                ],
            },
            Location::Local => WriteResult {
                files: vec![write_entry(
                    &vscode_local_mcp_json(ctx),
                    "servers",
                    &entry_with_project_path(ctx),
                )],
                notes: vec![
                    format!(
                        "CodeGraph MCP configured for project {}.",
                        ctx.cwd.display()
                    ),
                    "Reload VS Code for MCP changes to take effect.".to_string(),
                ],
            },
        }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        let file = match loc {
            Location::Global => vscode_global_mcp_json(ctx),
            Location::Local => vscode_local_mcp_json(ctx),
        };
        WriteResult {
            files: vec![remove_entry(&file, "servers")],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        match loc {
            Location::Global => print_snippet(
                &vscode_global_mcp_json(ctx),
                "servers",
                &mcp_server_config(),
                Some(VSCODE_GLOBAL_WHY),
            ),
            Location::Local => print_snippet(
                &vscode_local_mcp_json(ctx),
                "servers",
                &entry_with_project_path(ctx),
                None,
            ),
        }
    }
}

pub static VSCODE_TARGET: VsCodeTarget = VsCodeTarget;

// ========================== Copilot CLI ======================================

pub struct CopilotCliTarget;

impl AgentTarget for CopilotCliTarget {
    fn id(&self) -> TargetId {
        TargetId::CopilotCli
    }
    fn display_name(&self) -> &'static str {
        "GitHub Copilot CLI"
    }
    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult {
                installed: false,
                already_configured: false,
            };
        }
        detect_at(
            &copilot_cli_mcp_json(ctx),
            &ctx.home.join(".copilot"),
            "mcpServers",
        )
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return unsupported_local(self.display_name());
        }
        WriteResult {
            files: vec![write_entry(
                &copilot_cli_mcp_json(ctx),
                "mcpServers",
                &copilot_cli_entry(),
            )],
            notes: vec![
                "GitHub Copilot CLI supports GLOBAL config only.".to_string(),
                "The entry sets \"tools\": [\"*\"] — without it the CLI registers the server but exposes no tools.".to_string(),
            ],
        }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return unsupported_local(self.display_name());
        }
        WriteResult {
            files: vec![remove_entry(&copilot_cli_mcp_json(ctx), "mcpServers")],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, _loc: Location) -> String {
        print_snippet(
            &copilot_cli_mcp_json(ctx),
            "mcpServers",
            &copilot_cli_entry(),
            None,
        )
    }
}

pub static COPILOT_CLI_TARGET: CopilotCliTarget = CopilotCliTarget;

// =========================== JetBrains =======================================

pub struct JetBrainsTarget;

impl AgentTarget for JetBrainsTarget {
    fn id(&self) -> TargetId {
        TargetId::JetBrains
    }
    fn display_name(&self) -> &'static str {
        "JetBrains IDEs (GitHub Copilot)"
    }
    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult {
                installed: false,
                already_configured: false,
            };
        }
        let file = jetbrains_mcp_json(ctx);
        let marker = file.parent().map(Path::to_path_buf).unwrap_or_default();
        detect_at(&file, &marker, "servers")
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return unsupported_local(self.display_name());
        }
        WriteResult {
            files: vec![write_entry(
                &jetbrains_mcp_json(ctx),
                "servers",
                &mcp_server_config(),
            )],
            notes: vec![
                "JetBrains Copilot MCP config is GLOBAL only.".to_string(),
                "Restart the IDE for MCP changes to take effect.".to_string(),
            ],
        }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return unsupported_local(self.display_name());
        }
        WriteResult {
            files: vec![remove_entry(&jetbrains_mcp_json(ctx), "servers")],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, _loc: Location) -> String {
        print_snippet(
            &jetbrains_mcp_json(ctx),
            "servers",
            &mcp_server_config(),
            None,
        )
    }
}

pub static JETBRAINS_TARGET: JetBrainsTarget = JetBrainsTarget;

/// The no-file result for a `--local` request on a global-only target: a note,
/// no error, and nothing written.
fn unsupported_local(display_name: &str) -> WriteResult {
    WriteResult {
        files: Vec::new(),
        notes: vec![format!(
            "{display_name} supports --global only; nothing written."
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp-rooted context. Sets BOTH `app_data` and `xdg_config_home` under
    /// the temp home so `config_base_for` resolves inside it on every OS.
    fn temp_ctx(label: &str) -> (InstallContext, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "cg-copilot-{label}-{}-{}",
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
            app_data: Some(home.join("AppData").join("Roaming")),
            xdg_config_home: Some(home.join(".config")),
            hermes_home: None,
        };
        (ctx, base)
    }

    fn opts() -> InstallOptions {
        InstallOptions {
            auto_allow: true,
            front_load_hook: false,
        }
    }

    fn args_of(config: &Map<String, Value>, wrapper: &str) -> Vec<Value> {
        config[wrapper]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args")
            .clone()
    }

    // ---------------- VS Code -------------------------------------------------

    #[test]
    fn vscode_global_entry_is_bare_and_never_uses_workspace_folder() {
        // The load-bearing contract: VS Code expands ${workspaceFolder} only in a
        // WORKSPACE mcp.json, so a global entry using it would point the server at
        // a directory literally named "${workspaceFolder}".
        let (ctx, base) = temp_ctx("vscode-global-bare");
        let result = VsCodeTarget.install(&ctx, Location::Global, opts());
        assert_eq!(result.files[0].action, FileAction::Created);
        let file = vscode_global_mcp_json(&ctx);
        let config = read_json_file(&file);
        assert_eq!(
            args_of(&config, "servers"),
            vec![json!("serve"), json!("--mcp")],
            "global entry must be bare"
        );
        let raw = fs::read_to_string(&file).unwrap();
        assert!(
            !raw.contains("${workspaceFolder}"),
            "global config must not contain ${{workspaceFolder}}: {raw}"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_global_path_is_code_user_mcp_json() {
        let (ctx, base) = temp_ctx("vscode-global-path");
        let file = vscode_global_mcp_json(&ctx);
        assert!(
            file.ends_with(PathBuf::from("Code").join("User").join("mcp.json")),
            "got {}",
            file.display()
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_local_writes_dot_vscode_with_absolute_path() {
        let (ctx, base) = temp_ctx("vscode-local");
        let result = VsCodeTarget.install(&ctx, Location::Local, opts());
        assert_eq!(result.files[0].action, FileAction::Created);
        let file = vscode_local_mcp_json(&ctx);
        assert!(
            file.ends_with(PathBuf::from(".vscode").join("mcp.json")),
            "got {}",
            file.display()
        );
        let args = args_of(&read_json_file(&file), "servers");
        assert_eq!(
            args,
            vec![
                json!("serve"),
                json!("--mcp"),
                json!("--path"),
                json!(ctx.cwd.to_string_lossy().to_string()),
            ]
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_uses_the_servers_wrapper_not_mcp_servers() {
        let (ctx, base) = temp_ctx("vscode-wrapper");
        VsCodeTarget.install(&ctx, Location::Local, opts());
        let config = read_json_file(&vscode_local_mcp_json(&ctx));
        assert!(config.contains_key("servers"), "must use `servers`");
        assert!(
            !config.contains_key("mcpServers"),
            "must NOT use `mcpServers`: {config:?}"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_install_preserves_sibling_servers() {
        let (ctx, base) = temp_ctx("vscode-siblings");
        let file = vscode_local_mcp_json(&ctx);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  \"servers\": {\n    \"github\": { \"command\": \"gh-mcp\" }\n  }\n}\n",
        )
        .unwrap();
        let result = VsCodeTarget.install(&ctx, Location::Local, opts());
        assert_eq!(result.files[0].action, FileAction::Updated);
        let config = read_json_file(&file);
        assert_eq!(config["servers"]["github"]["command"], json!("gh-mcp"));
        assert!(config["servers"].get("codegraph").is_some());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_install_is_idempotent() {
        let (ctx, base) = temp_ctx("vscode-idempotent");
        let file = vscode_local_mcp_json(&ctx);
        VsCodeTarget.install(&ctx, Location::Local, opts());
        let first = fs::read_to_string(&file).unwrap();
        let second = VsCodeTarget.install(&ctx, Location::Local, opts());
        assert_eq!(second.files[0].action, FileAction::Unchanged);
        assert_eq!(fs::read_to_string(&file).unwrap(), first);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_uninstall_removes_only_codegraph() {
        let (ctx, base) = temp_ctx("vscode-uninstall");
        let file = vscode_local_mcp_json(&ctx);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  \"servers\": {\n    \"github\": { \"command\": \"gh-mcp\" },\n    \"codegraph\": { \"command\": \"codegraph\" }\n  }\n}\n",
        )
        .unwrap();
        let result = VsCodeTarget.uninstall(&ctx, Location::Local);
        assert_eq!(result.files[0].action, FileAction::Removed);
        let config = read_json_file(&file);
        assert!(config["servers"].get("github").is_some());
        assert!(config["servers"].get("codegraph").is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_uninstall_reports_not_found_when_absent() {
        let (ctx, base) = temp_ctx("vscode-uninstall-absent");
        let result = VsCodeTarget.uninstall(&ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_skips_unparseable_config() {
        let (ctx, base) = temp_ctx("vscode-corrupt");
        let file = vscode_local_mcp_json(&ctx);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let corrupt = "{ not json";
        fs::write(&file, corrupt).unwrap();
        let result = VsCodeTarget.install(&ctx, Location::Local, opts());
        assert_eq!(result.files[0].action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&file).unwrap(), corrupt);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_detect_reflects_configuration() {
        let (ctx, base) = temp_ctx("vscode-detect");
        assert!(!VsCodeTarget.detect(&ctx, Location::Local).installed);
        VsCodeTarget.install(&ctx, Location::Local, opts());
        let after = VsCodeTarget.detect(&ctx, Location::Local);
        assert!(after.installed);
        assert!(after.already_configured);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn vscode_print_config_global_warns_about_workspace_folder() {
        let (ctx, base) = temp_ctx("vscode-print");
        let out = VsCodeTarget.print_config(&ctx, Location::Global);
        assert!(out.contains("servers"));
        assert!(
            out.contains("${workspaceFolder}"),
            "the note must EXPLAIN the omission: {out}"
        );
        let entry_line = out
            .lines()
            .find(|l| l.contains("\"args\""))
            .unwrap_or_default();
        assert!(
            !entry_line.contains("${workspaceFolder}"),
            "the ENTRY itself must not use it: {entry_line}"
        );
        let local = VsCodeTarget.print_config(&ctx, Location::Local);
        assert!(local.replace('\\', "/").contains(".vscode/mcp.json"));
        assert!(local.contains("--path"));
        let _ = fs::remove_dir_all(base);
    }

    // ---------------- Copilot CLI --------------------------------------------

    #[test]
    fn copilot_cli_entry_declares_all_tools() {
        // Without `"tools": ["*"]` the CLI registers the server but exposes none
        // of its tools.
        let (ctx, base) = temp_ctx("cli-tools");
        CopilotCliTarget.install(&ctx, Location::Global, opts());
        let config = read_json_file(&copilot_cli_mcp_json(&ctx));
        assert_eq!(
            config["mcpServers"]["codegraph"]["tools"],
            json!(["*"]),
            "the CLI entry must declare tools: [\"*\"]"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn copilot_cli_path_and_wrapper() {
        let (ctx, base) = temp_ctx("cli-path");
        let file = copilot_cli_mcp_json(&ctx);
        assert!(
            file.ends_with(PathBuf::from(".copilot").join("mcp-config.json")),
            "got {}",
            file.display()
        );
        CopilotCliTarget.install(&ctx, Location::Global, opts());
        let config = read_json_file(&file);
        assert!(config.contains_key("mcpServers"), "must use `mcpServers`");
        assert!(!config.contains_key("servers"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn copilot_cli_is_global_only() {
        assert!(CopilotCliTarget.supports_location(Location::Global));
        assert!(!CopilotCliTarget.supports_location(Location::Local));
    }

    #[test]
    fn copilot_cli_local_install_writes_nothing() {
        let (ctx, base) = temp_ctx("cli-local");
        let result = CopilotCliTarget.install(&ctx, Location::Local, opts());
        assert!(result.files.is_empty(), "no file may be written");
        assert!(
            result.notes.iter().any(|n| n.contains("--global only")),
            "must explain: {:?}",
            result.notes
        );
        assert!(
            !copilot_cli_mcp_json(&ctx).exists(),
            "must not create the global config from a --local request"
        );
        let local_result = CopilotCliTarget.uninstall(&ctx, Location::Local);
        assert!(local_result.files.is_empty());
        let detect = CopilotCliTarget.detect(&ctx, Location::Local);
        assert!(!detect.installed && !detect.already_configured);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn copilot_cli_uninstall_removes_only_codegraph() {
        let (ctx, base) = temp_ctx("cli-uninstall");
        let file = copilot_cli_mcp_json(&ctx);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" },\n    \"codegraph\": { \"command\": \"codegraph\" }\n  }\n}\n",
        )
        .unwrap();
        assert_eq!(
            CopilotCliTarget.uninstall(&ctx, Location::Global).files[0].action,
            FileAction::Removed
        );
        let config = read_json_file(&file);
        assert!(config["mcpServers"].get("other").is_some());
        assert!(config["mcpServers"].get("codegraph").is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn copilot_cli_print_config_includes_tools() {
        let (ctx, base) = temp_ctx("cli-print");
        let out = CopilotCliTarget.print_config(&ctx, Location::Global);
        assert!(out.contains("mcpServers"));
        assert!(out.contains("\"tools\""));
        let _ = fs::remove_dir_all(base);
    }

    // ---------------- JetBrains ----------------------------------------------

    #[test]
    fn jetbrains_path_is_xdg_github_copilot_intellij() {
        let (ctx, base) = temp_ctx("jb-path");
        let file = jetbrains_mcp_json(&ctx);
        assert!(
            file.ends_with(
                PathBuf::from("github-copilot")
                    .join("intellij")
                    .join("mcp.json")
            ),
            "got {}",
            file.display()
        );
        assert!(
            file.starts_with(ctx.xdg_config_home.as_ref().unwrap()),
            "must sit under xdg_config_home on every OS, got {}",
            file.display()
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn jetbrains_falls_back_to_dot_config_when_xdg_unset() {
        let (mut ctx, base) = temp_ctx("jb-noxdg");
        ctx.xdg_config_home = None;
        let file = jetbrains_mcp_json(&ctx);
        assert!(
            file.starts_with(ctx.home.join(".config")),
            "got {}",
            file.display()
        );
        ctx.xdg_config_home = Some(PathBuf::new());
        assert!(
            jetbrains_mcp_json(&ctx).starts_with(ctx.home.join(".config")),
            "an EMPTY xdg value must fall back too"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn jetbrains_uses_servers_wrapper_and_is_global_only() {
        let (ctx, base) = temp_ctx("jb-wrapper");
        assert!(JetBrainsTarget.supports_location(Location::Global));
        assert!(!JetBrainsTarget.supports_location(Location::Local));
        JetBrainsTarget.install(&ctx, Location::Global, opts());
        let config = read_json_file(&jetbrains_mcp_json(&ctx));
        assert!(config.contains_key("servers"), "must use `servers`");
        assert!(!config.contains_key("mcpServers"));
        assert_eq!(
            args_of(&config, "servers"),
            vec![json!("serve"), json!("--mcp")],
            "global-only target writes a bare entry"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn jetbrains_local_install_writes_nothing() {
        let (ctx, base) = temp_ctx("jb-local");
        let result = JetBrainsTarget.install(&ctx, Location::Local, opts());
        assert!(result.files.is_empty());
        assert!(!jetbrains_mcp_json(&ctx).exists());
        assert!(
            JetBrainsTarget
                .uninstall(&ctx, Location::Local)
                .files
                .is_empty()
        );
        let detect = JetBrainsTarget.detect(&ctx, Location::Local);
        assert!(!detect.installed);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn jetbrains_uninstall_and_detect_roundtrip() {
        let (ctx, base) = temp_ctx("jb-roundtrip");
        assert!(!JetBrainsTarget.detect(&ctx, Location::Global).installed);
        JetBrainsTarget.install(&ctx, Location::Global, opts());
        let after = JetBrainsTarget.detect(&ctx, Location::Global);
        assert!(after.installed && after.already_configured);
        assert_eq!(
            JetBrainsTarget.uninstall(&ctx, Location::Global).files[0].action,
            FileAction::Removed
        );
        assert!(
            !JetBrainsTarget
                .detect(&ctx, Location::Global)
                .already_configured
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn jetbrains_print_config_names_the_file() {
        let (ctx, base) = temp_ctx("jb-print");
        let out = JetBrainsTarget.print_config(&ctx, Location::Global);
        assert!(
            out.replace('\\', "/")
                .contains("github-copilot/intellij/mcp.json")
        );
        assert!(out.contains("servers"));
        let _ = fs::remove_dir_all(base);
    }

    // ---------------- Cross-target -------------------------------------------

    #[test]
    fn no_copilot_target_declares_skill_support() {
        // None of the three has a documented skill directory, so all three keep
        // the trait's unsupported default rather than guessing a path.
        for (name, supports) in [
            ("vscode", VsCodeTarget.supports_skills(Location::Global)),
            (
                "copilot-cli",
                CopilotCliTarget.supports_skills(Location::Global),
            ),
            (
                "jetbrains",
                JetBrainsTarget.supports_skills(Location::Global),
            ),
        ] {
            assert!(!supports, "{name} must not claim skill support");
        }
    }

    #[test]
    fn the_three_targets_write_three_distinct_files() {
        let (ctx, base) = temp_ctx("distinct");
        VsCodeTarget.install(&ctx, Location::Global, opts());
        CopilotCliTarget.install(&ctx, Location::Global, opts());
        JetBrainsTarget.install(&ctx, Location::Global, opts());
        let files = [
            vscode_global_mcp_json(&ctx),
            copilot_cli_mcp_json(&ctx),
            jetbrains_mcp_json(&ctx),
        ];
        for file in &files {
            assert!(file.is_file(), "missing {}", file.display());
        }
        let unique: std::collections::BTreeSet<_> = files.iter().collect();
        assert_eq!(unique.len(), 3, "the three targets must not share a file");
        let _ = fs::remove_dir_all(base);
    }
}
