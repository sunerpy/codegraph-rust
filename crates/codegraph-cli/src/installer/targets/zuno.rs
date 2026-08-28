//! Zuno target.
//!
//! Writes the MCP entry to `$XDG_CONFIG_HOME/zuno/zuno.json[c]` (global) or
//! `./.zuno/zuno.json[c]` (local), under `mcp.codegraph`. Instructions go to
//! `$XDG_CONFIG_HOME/zuno/AGENTS.md` globally and the project-root `AGENTS.md`
//! locally. Zuno scans the standard `.agents/skills` directories, so this
//! target deliberately reuses them instead of creating a duplicate
//! `zuno/skills/codegraph` discovery path.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::shared::{
    self, CODEGRAPH_SECTION_END, CODEGRAPH_SECTION_START, ConfigRead, parse_json_object,
    read_config_file, remove_nested_key_jsonc, to_upstream_json, upsert_instructions_entry,
    upsert_nested_key_jsonc, write_json_file,
};
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct ZunoTarget;

const ENTRY_KEY: &str = "codegraph";
const LEGACY_ENTRY_KEY: &str = "codegraph-mcp-server";

fn global_config_dir(ctx: &InstallContext) -> PathBuf {
    ctx.xdg_config_home
        .as_ref()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".config"))
        .join("zuno")
}

fn config_dir(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => global_config_dir(ctx),
        Location::Local => ctx.cwd.join(".zuno"),
    }
}

fn config_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    let dir = config_dir(ctx, loc);
    let jsonc = dir.join("zuno.jsonc");
    let json = dir.join("zuno.json");
    if jsonc.exists() {
        return jsonc;
    }
    if json.exists() {
        return json;
    }
    json
}

fn instructions_path(ctx: &InstallContext, loc: Location) -> PathBuf {
    match loc {
        Location::Global => global_config_dir(ctx).join("AGENTS.md"),
        Location::Local => ctx.cwd.join("AGENTS.md"),
    }
}

fn server_entry() -> Value {
    json!({
        "type": "local",
        "command": ["codegraph", "serve", "--mcp"],
        "enabled": true,
    })
}

fn has_codegraph_entry(config: &Map<String, Value>) -> bool {
    config
        .get("mcp")
        .is_some_and(|mcp| mcp.get(ENTRY_KEY).is_some() || mcp.get(LEGACY_ENTRY_KEY).is_some())
}

impl AgentTarget for ZunoTarget {
    fn id(&self) -> TargetId {
        TargetId::Zuno
    }

    fn display_name(&self) -> &'static str {
        "Zuno"
    }

    fn supports_location(&self, _loc: Location) -> bool {
        true
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        let file = config_path(ctx, loc);
        let config =
            parse_json_object(&fs::read_to_string(&file).unwrap_or_default()).unwrap_or_default();
        DetectionResult {
            installed: config_dir(ctx, loc).exists() || file.exists(),
            already_configured: has_codegraph_entry(&config),
        }
    }

    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        WriteResult {
            files: vec![
                write_mcp_entry(ctx, loc),
                upsert_instructions_entry(&instructions_path(ctx, loc)),
            ],
            notes: Vec::new(),
        }
    }

    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        WriteResult {
            files: vec![
                remove_mcp_entry(&config_path(ctx, loc)),
                remove_instructions_entry(ctx, loc),
            ],
            notes: Vec::new(),
        }
    }

    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        let target = config_path(ctx, loc);
        let snippet = to_upstream_json(&json!({
            "mcp": { "codegraph": server_entry() },
        }));
        format!("# Add to {}\n\n{snippet}\n", target.display())
    }

    fn managed_instructions_path(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        Some(instructions_path(ctx, loc))
    }

    fn supports_skills(&self, _loc: Location) -> bool {
        true
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        Some(match loc {
            Location::Global => ctx.home.join(".agents").join("skills"),
            Location::Local => ctx.cwd.join(".agents").join("skills"),
        })
    }
}

fn write_mcp_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = config_path(ctx, loc);
    let entry = server_entry();
    match read_config_file(&file) {
        ConfigRead::Unparseable => FileWrite {
            path: file,
            action: FileAction::Skipped,
        },
        ConfigRead::Missing => {
            let mut mcp = Map::new();
            mcp.insert(ENTRY_KEY.to_string(), entry);
            let mut config = Map::new();
            config.insert("mcp".to_string(), Value::Object(mcp));
            let _ = write_json_file(&file, &config);
            FileWrite {
                path: file,
                action: FileAction::Created,
            }
        }
        ConfigRead::Parsed(config) => {
            let had_legacy = config
                .get("mcp")
                .and_then(|mcp| mcp.get(LEGACY_ENTRY_KEY))
                .is_some();
            let mut action = upsert_nested_key_jsonc(&file, "mcp", ENTRY_KEY, &entry, None)
                .unwrap_or(FileAction::Skipped);
            if action != FileAction::Skipped
                && had_legacy
                && remove_nested_key_jsonc(&file, "mcp", LEGACY_ENTRY_KEY)
                    .is_ok_and(|removed| removed == FileAction::Removed)
            {
                action = FileAction::Updated;
            }
            FileWrite { path: file, action }
        }
    }
}

fn remove_mcp_entry(file: &Path) -> FileWrite {
    let canonical = remove_nested_key_jsonc(file, "mcp", ENTRY_KEY).unwrap_or(FileAction::NotFound);
    let legacy =
        remove_nested_key_jsonc(file, "mcp", LEGACY_ENTRY_KEY).unwrap_or(FileAction::NotFound);
    FileWrite {
        path: file.to_path_buf(),
        action: if canonical == FileAction::Removed || legacy == FileAction::Removed {
            FileAction::Removed
        } else {
            FileAction::NotFound
        },
    }
}

fn remove_instructions_entry(ctx: &InstallContext, loc: Location) -> FileWrite {
    let file = instructions_path(ctx, loc);
    let action =
        shared::remove_marked_section(&file, CODEGRAPH_SECTION_START, CODEGRAPH_SECTION_END);
    FileWrite { path: file, action }
}

pub static ZUNO_TARGET: ZunoTarget = ZunoTarget;

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-zuno-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let ctx = InstallContext {
                home: base.join("home"),
                cwd: base.join("project"),
                app_data: None,
                xdg_config_home: Some(base.join("xdg")),
                hermes_home: None,
            };
            fs::create_dir_all(&ctx.home).unwrap();
            fs::create_dir_all(&ctx.cwd).unwrap();
            Self { base, ctx }
        }
    }

    impl Drop for Fixture {
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
    fn paths_and_shared_skill_dirs_match_zuno_discovery() {
        let fx = Fixture::new("paths");
        assert_eq!(
            config_path(&fx.ctx, Location::Global),
            fx.base.join("xdg/zuno/zuno.json")
        );
        assert_eq!(
            instructions_path(&fx.ctx, Location::Global),
            fx.base.join("xdg/zuno/AGENTS.md")
        );
        assert_eq!(
            config_path(&fx.ctx, Location::Local),
            fx.ctx.cwd.join(".zuno/zuno.json")
        );
        assert_eq!(
            instructions_path(&fx.ctx, Location::Local),
            fx.ctx.cwd.join("AGENTS.md")
        );
        assert_eq!(
            ZunoTarget.skill_dir(&fx.ctx, Location::Global),
            Some(fx.ctx.home.join(".agents/skills"))
        );
        assert_eq!(
            ZunoTarget.skill_dir(&fx.ctx, Location::Local),
            Some(fx.ctx.cwd.join(".agents/skills"))
        );
    }

    #[test]
    fn existing_jsonc_is_preferred_over_json() {
        let fx = Fixture::new("jsonc");
        let dir = config_dir(&fx.ctx, Location::Global);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("zuno.json"), "{}\n").unwrap();
        fs::write(dir.join("zuno.jsonc"), "{ // preferred\n}\n").unwrap();
        assert!(config_path(&fx.ctx, Location::Global).ends_with("zuno.jsonc"));
    }

    #[test]
    fn global_install_preserves_jsonc_siblings_and_user_instructions() {
        let fx = Fixture::new("global");
        let config = config_path(&fx.ctx, Location::Global);
        let instructions = instructions_path(&fx.ctx, Location::Global);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(
            &config,
            "{\n  // keep this comment\n  \"mcp\": {\n    \"other\": { \"command\": [\"other\"] }\n  }\n}\n",
        )
        .unwrap();
        fs::write(
            &instructions,
            format!(
                "user before\n\n{CODEGRAPH_SECTION_START}\nold\n{CODEGRAPH_SECTION_END}\n\nuser after\n"
            ),
        )
        .unwrap();

        let first = ZunoTarget.install(&fx.ctx, Location::Global, opts());
        assert!(
            first
                .files
                .iter()
                .all(|file| { matches!(file.action, FileAction::Created | FileAction::Updated) })
        );
        let written = fs::read_to_string(&config).unwrap();
        assert!(written.contains("// keep this comment"));
        let parsed = parse_json_object(&written).unwrap();
        assert_eq!(
            parsed["mcp"][ENTRY_KEY]["command"],
            json!(["codegraph", "serve", "--mcp"])
        );
        assert_eq!(parsed["mcp"][ENTRY_KEY]["type"], "local");
        assert_eq!(parsed["mcp"][ENTRY_KEY]["enabled"], true);
        assert!(parsed["mcp"]["other"].is_object());

        let managed = fs::read_to_string(&instructions).unwrap();
        assert!(managed.contains("user before"));
        assert!(managed.contains("user after"));
        assert!(managed.contains("`codegraph_status`"));
        assert!(!managed.contains("\nold\n"));

        let before = fs::read_to_string(&config).unwrap();
        let second = ZunoTarget.install(&fx.ctx, Location::Global, opts());
        assert!(
            second
                .files
                .iter()
                .all(|file| file.action == FileAction::Unchanged)
        );
        assert_eq!(fs::read_to_string(&config).unwrap(), before);
    }

    #[test]
    fn local_install_and_uninstall_use_project_paths() {
        let fx = Fixture::new("local");
        let config = config_path(&fx.ctx, Location::Local);
        let instructions = instructions_path(&fx.ctx, Location::Local);
        fs::write(&instructions, "project rules\n").unwrap();

        ZunoTarget.install(&fx.ctx, Location::Local, opts());
        assert!(config.exists());
        assert!(instructions.starts_with(&fx.ctx.cwd));
        assert!(
            parse_json_object(&fs::read_to_string(&config).unwrap()).unwrap()["mcp"][ENTRY_KEY]
                .is_object()
        );

        ZunoTarget.uninstall(&fx.ctx, Location::Local);
        let parsed = parse_json_object(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(parsed.get("mcp").is_none());
        assert_eq!(
            fs::read_to_string(&instructions).unwrap(),
            "project rules\n"
        );
    }

    #[test]
    fn install_migrates_legacy_entry_without_duplication() {
        let fx = Fixture::new("legacy");
        let config = config_path(&fx.ctx, Location::Global);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(
            &config,
            format!(
                "{{\n  \"mcp\": {{\n    \"{LEGACY_ENTRY_KEY}\": {{ \"type\": \"local\", \"command\": [\"codegraph\", \"serve\", \"--mcp\"] }}\n  }}\n}}\n"
            ),
        )
        .unwrap();
        assert!(
            ZunoTarget
                .detect(&fx.ctx, Location::Global)
                .already_configured
        );

        let result = ZunoTarget.install(&fx.ctx, Location::Global, opts());
        assert_eq!(result.files[0].action, FileAction::Updated);
        let parsed = parse_json_object(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(parsed["mcp"][ENTRY_KEY].is_object());
        assert!(parsed["mcp"].get(LEGACY_ENTRY_KEY).is_none());
    }

    #[test]
    fn unparseable_config_is_backed_up_and_not_clobbered() {
        let fx = Fixture::new("unparseable");
        let config = config_path(&fx.ctx, Location::Global);
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "{ invalid").unwrap();

        let result = ZunoTarget.install(&fx.ctx, Location::Global, opts());
        assert_eq!(result.files[0].action, FileAction::Skipped);
        assert_eq!(fs::read_to_string(&config).unwrap(), "{ invalid");
        assert!(config.with_extension("backup").exists());
    }

    #[test]
    fn print_config_uses_zuno_local_command_shape() {
        let fx = Fixture::new("print");
        let output = ZunoTarget.print_config(&fx.ctx, Location::Global);
        assert!(output.contains("\"mcp\""));
        assert!(output.contains("\"type\": \"local\""));
        assert!(output.contains("\"command\": ["));
        assert!(output.contains("\"codegraph\""));
    }
}
