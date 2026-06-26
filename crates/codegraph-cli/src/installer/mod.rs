//! CodeGraph installer — writes/removes the MCP-server config in each supported
//! agent's config files. Ports `upstream installer/`.
//!
//! This is the non-interactive, flag-driven path (`install --target=… --local`,
//! `--print-config`, `uninstall`). The config-writing logic in `targets/` is
//! byte-faithful to the upstream; the interactive `@clack/prompts` multiselect is
//! replaced by a non-interactive default (no `--target` → install to detected
//! agents, claude fallback), since the TUI is a nicety and the file writes are
//! what users depend on.

mod registry;
mod shared;
pub mod skill;
mod targets;
mod types;

use std::path::PathBuf;

use anyhow::{bail, Result};

use registry::{get_target, list_target_ids, resolve_target_flag};
use types::{
    AgentTarget, FileAction, InstallContext, InstallOptions, Location, SkillStatusReport, TargetId,
    WriteResult,
};

/// Build the install context from the process environment, mirroring the upstream's
/// `os.homedir()` / `process.cwd()` reads. `HOME` (POSIX) / `USERPROFILE`
/// (Windows) give the home dir; the rest are optional per-target env inputs.
fn context_from_env() -> Result<InstallContext> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("could not resolve home directory (HOME/USERPROFILE)"))?;
    let cwd = std::env::current_dir()?;
    Ok(InstallContext {
        home,
        cwd,
        app_data: std::env::var_os("APPDATA").map(PathBuf::from),
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        hermes_home: std::env::var_os("HERMES_HOME").map(PathBuf::from),
    })
}

/// Parse a `--location` string. Ports the bin guard (codegraph.ts:1892).
fn parse_location(value: Option<&str>) -> Result<Option<Location>> {
    match value {
        None => Ok(None),
        Some("global") => Ok(Some(Location::Global)),
        Some("local") => Ok(Some(Location::Local)),
        Some(other) => bail!("--location must be \"global\" or \"local\" (got \"{other}\")."),
    }
}

/// Options for `codegraph install`. Mirrors the `install` flag surface
/// (codegraph.ts:1864-1870).
pub struct InstallArgs {
    pub target: Option<String>,
    pub location: Option<String>,
    pub yes: bool,
    /// `--no-permissions` → false; absent → None (default-on, see below).
    pub permissions: Option<bool>,
    /// `--prompt-hook` → true; opt-in front-load hook, off by default.
    pub front_load_hook: bool,
    pub print_config: Option<String>,
}

/// Options for `codegraph uninstall` (codegraph.ts:1931-1935).
pub struct UninstallArgs {
    pub target: Option<String>,
    pub location: Option<String>,
    pub yes: bool,
}

/// `codegraph install`. Ports the bin action (codegraph.ts:1871) + the
/// non-interactive parts of runInstallerWithOptions (index.ts:88).
pub fn run_install(args: InstallArgs) -> Result<()> {
    let ctx = context_from_env()?;
    run_install_with_ctx(ctx, args)
}

fn run_install_with_ctx(ctx: InstallContext, args: InstallArgs) -> Result<()> {
    // --print-config <id>: dump the snippet and exit, no file writes
    // (codegraph.ts:1878).
    if let Some(id) = &args.print_config {
        let Some(target) = get_target(id) else {
            let known = list_target_ids().join(", ");
            bail!("Unknown target \"{id}\". Known: {known}.");
        };
        let loc = match args.location.as_deref() {
            Some("local") => Location::Local,
            _ => Location::Global,
        };
        print!("{}", target.print_config(&ctx, loc));
        return Ok(());
    }

    let explicit_location = parse_location(args.location.as_deref())?;
    let use_defaults = args.yes;

    // Location: explicit flag wins; --yes ⇒ global; else default to global for
    // the non-interactive port (the upstream prompts here).
    let location = explicit_location.unwrap_or(Location::Global);

    // auto_allow: --no-permissions ⇒ false; --yes ⇒ true; else default false in
    // the non-interactive port (the upstream prompts only when claude is a target).
    let auto_allow = match args.permissions {
        Some(false) => false,
        _ => use_defaults,
    };

    // Resolve targets: explicit --target wins; --yes ⇒ auto; else default to
    // auto-detect (claude fallback) — the no-prompt analog of the multiselect
    // pre-populated with detected agents.
    let target_flag = args.target.clone().unwrap_or_else(|| "auto".to_string());
    let targets = resolve_target_flag(&ctx, &target_flag, location)?;
    if targets.is_empty() {
        println!("No agent targets selected — nothing to do.");
        return Ok(());
    }

    let opts = InstallOptions {
        auto_allow,
        front_load_hook: args.front_load_hook,
    };
    let mut installed_ids: Vec<TargetId> = Vec::new();
    for target in &targets {
        if !target.supports_location(location) {
            println!(
                "{}: skipped — does not support --location={}.",
                target.display_name(),
                location.as_str()
            );
            continue;
        }
        if target.detect(&ctx, location).already_configured {
            println!("{}: already configured — updating.", target.display_name());
        }
        let result = target.install(&ctx, location, opts);
        installed_ids.push(target.id());
        report_write_result(target.display_name(), &ctx, &result);
        if target.supports_skills(location) {
            let skill_result = target.install_skill(&ctx, location, false);
            report_write_result(target.display_name(), &ctx, &skill_result);
        }
    }

    if !installed_ids.is_empty() {
        let names: Vec<&str> = targets.iter().map(|t| t.display_name()).collect();
        println!(
            "\nDone! Restart your agent{} to use CodeGraph: {}",
            if installed_ids.len() > 1 { "s" } else { "" },
            names.join(", ")
        );
    }
    Ok(())
}

/// Install project-level (`Location::Local`) MCP config for the given target
/// flag under an explicit project root, reusing the `run_install` engine. Backs
/// `codegraph init [path] --target=…`, where "init" means "set this project
/// up", so the location is always Local (an absolute project `--path` for
/// editors like Kiro/Cursor that launch the server from a non-project CWD).
/// `project_root` overrides the context CWD so the config and its `--path` land
/// under the project being initialized, not the process CWD. `target_flag`
/// accepts the same values as `install --target` (csv ids, `auto`, `all`,
/// `none`); `none` is a no-op.
pub fn run_install_local_targets(project_root: PathBuf, target_flag: &str) -> Result<()> {
    if target_flag == "none" {
        return Ok(());
    }
    let mut ctx = context_from_env()?;
    ctx.cwd = project_root;
    run_install_with_ctx(
        ctx,
        InstallArgs {
            target: Some(target_flag.to_string()),
            location: Some("local".to_string()),
            yes: true,
            permissions: None,
            front_load_hook: false,
            print_config: None,
        },
    )
}

/// `codegraph uninstall`. Ports runUninstaller (index.ts:346) — sweeps every
/// agent (or the `--target` subset) and reports per-agent outcomes.
pub fn run_uninstall(args: UninstallArgs) -> Result<()> {
    let ctx = context_from_env()?;
    let explicit_location = parse_location(args.location.as_deref())?;
    let _ = args.yes;
    let location = explicit_location.unwrap_or(Location::Global);

    // Default target is every agent (index.ts:385); --target subsets it.
    let targets = match &args.target {
        Some(value) => resolve_target_flag(&ctx, value, location)?,
        None => registry::all_targets(),
    };
    if targets.is_empty() {
        println!("No agent targets selected — nothing to do.");
        return Ok(());
    }

    let reports = uninstall_targets(&ctx, &targets, location);
    let mut removed_names: Vec<&str> = Vec::new();
    for report in &reports {
        match report.status {
            UninstallStatus::Removed => {
                for path in &report.removed_paths {
                    println!("{}: removed {}", report.display_name, tildify(&ctx, path));
                }
                removed_names.push(report.display_name);
            }
            UninstallStatus::NotConfigured => {
                println!(
                    "{}: not configured — nothing to remove",
                    report.display_name
                );
            }
            UninstallStatus::Unsupported => {
                let note = report
                    .notes
                    .first()
                    .map(String::as_str)
                    .unwrap_or("unsupported location");
                println!("{}: skipped — {note}", report.display_name);
            }
        }
    }

    if removed_names.is_empty() {
        println!(
            "\nCodeGraph was not configured in any {} agent — nothing to remove.",
            location.as_str()
        );
    } else {
        println!(
            "\nRemoved CodeGraph from {} agent{}: {}. Restart {} to apply.",
            removed_names.len(),
            if removed_names.len() > 1 { "s" } else { "" },
            removed_names.join(", "),
            if removed_names.len() > 1 {
                "them"
            } else {
                "it"
            }
        );
    }
    Ok(())
}

enum UninstallStatus {
    Removed,
    NotConfigured,
    Unsupported,
}

struct UninstallReport {
    display_name: &'static str,
    status: UninstallStatus,
    removed_paths: Vec<PathBuf>,
    notes: Vec<String>,
}

/// Pure uninstall sweep. Ports uninstallTargets (index.ts:307).
fn uninstall_targets(
    ctx: &InstallContext,
    targets: &[&'static dyn AgentTarget],
    location: Location,
) -> Vec<UninstallReport> {
    targets
        .iter()
        .map(|target| {
            if !target.supports_location(location) {
                let only = match location {
                    Location::Local => "global",
                    Location::Global => "local",
                };
                return UninstallReport {
                    display_name: target.display_name(),
                    status: UninstallStatus::Unsupported,
                    removed_paths: Vec::new(),
                    notes: vec![format!(
                        "no {} config — this agent is {only}-only",
                        location.as_str()
                    )],
                };
            }
            let result = target.uninstall(ctx, location);
            let skill_result = target.uninstall_skill(ctx, location);
            let removed_paths: Vec<PathBuf> = result
                .files
                .iter()
                .chain(skill_result.files.iter())
                .filter(|f| f.action == FileAction::Removed)
                .map(|f| f.path.clone())
                .collect();
            let status = if removed_paths.is_empty() {
                UninstallStatus::NotConfigured
            } else {
                UninstallStatus::Removed
            };
            UninstallReport {
                display_name: target.display_name(),
                status,
                removed_paths,
                notes: result.notes,
            }
        })
        .collect()
}

/// Options for `codegraph skill <action>`. One arg struct serves all four
/// actions; `yes` is consumed by install/uninstall, `force` by update.
pub struct SkillArgs {
    pub target: Option<String>,
    pub location: Option<String>,
    pub yes: bool,
    pub force: bool,
}

fn resolve_skill_targets(
    ctx: &InstallContext,
    args: &SkillArgs,
) -> Result<(Location, Vec<&'static dyn AgentTarget>)> {
    let explicit_location = parse_location(args.location.as_deref())?;
    let location = explicit_location.unwrap_or(Location::Global);
    let target_flag = args.target.clone().unwrap_or_else(|| "auto".to_string());
    let targets = resolve_target_flag(ctx, &target_flag, location)?;
    Ok((location, targets))
}

/// `codegraph skill install`. Writes the embedded skill into each resolved
/// target's skill directory, gating on `supports_skills` (NOT
/// `supports_location` — codex/antigravity support local skills even though
/// their MCP config is global-only).
pub fn run_skill_install(args: SkillArgs) -> Result<()> {
    let _ = args.yes;
    let ctx = context_from_env()?;
    let (location, targets) = resolve_skill_targets(&ctx, &args)?;
    if targets.is_empty() {
        println!("No agent targets selected — nothing to do.");
        return Ok(());
    }

    let mut names: Vec<&str> = Vec::new();
    for target in &targets {
        if !target.supports_skills(location) {
            println!(
                "{}: skills not supported for --location={}",
                target.display_name(),
                location.as_str()
            );
            continue;
        }
        let result = target.install_skill(&ctx, location, args.force);
        if result
            .files
            .iter()
            .any(|f| matches!(f.action, FileAction::Created | FileAction::Updated))
        {
            names.push(target.display_name());
        }
        report_write_result(target.display_name(), &ctx, &result);
    }

    if !names.is_empty() {
        println!(
            "\nInstalled the CodeGraph skill for {} agent{}: {}.",
            names.len(),
            if names.len() > 1 { "s" } else { "" },
            names.join(", ")
        );
    }
    Ok(())
}

/// `codegraph skill update`: identical to install, but `--force` (plumbed
/// through [`SkillArgs::force`]) overwrites a locally modified skill.
pub fn run_skill_update(args: SkillArgs) -> Result<()> {
    run_skill_install(args)
}

/// `codegraph skill uninstall`. Removes the installed skill from each resolved
/// target; an absent skill is reported as "not configured" (success exit).
pub fn run_skill_uninstall(args: SkillArgs) -> Result<()> {
    let _ = args.yes;
    let ctx = context_from_env()?;
    let (location, targets) = resolve_skill_targets(&ctx, &args)?;
    if targets.is_empty() {
        println!("No agent targets selected — nothing to do.");
        return Ok(());
    }

    let mut removed_names: Vec<&str> = Vec::new();
    for target in &targets {
        if !target.supports_skills(location) {
            println!(
                "{}: skills not supported for --location={}",
                target.display_name(),
                location.as_str()
            );
            continue;
        }
        let result = target.uninstall_skill(&ctx, location);
        let removed: Vec<&PathBuf> = result
            .files
            .iter()
            .filter(|f| f.action == FileAction::Removed)
            .map(|f| &f.path)
            .collect();
        if removed.is_empty() {
            println!(
                "{}: not configured — nothing to remove",
                target.display_name()
            );
        } else {
            for path in removed {
                println!("{}: removed {}", target.display_name(), tildify(&ctx, path));
            }
            removed_names.push(target.display_name());
        }
    }

    if removed_names.is_empty() {
        println!(
            "\nThe CodeGraph skill was not installed in any {} agent — nothing to remove.",
            location.as_str()
        );
    } else {
        println!(
            "\nRemoved the CodeGraph skill from {} agent{}: {}.",
            removed_names.len(),
            if removed_names.len() > 1 { "s" } else { "" },
            removed_names.join(", ")
        );
    }
    Ok(())
}

/// `codegraph skill status`. Prints one line per target: "up to date" /
/// "locally modified" / "outdated" / "not installed" / "not supported".
pub fn run_skill_status(args: SkillArgs) -> Result<()> {
    let _ = (args.yes, args.force);
    let ctx = context_from_env()?;
    let (location, targets) = resolve_skill_targets(&ctx, &args)?;
    if targets.is_empty() {
        println!("No agent targets selected — nothing to do.");
        return Ok(());
    }

    for target in &targets {
        let report = target.skill_status(&ctx, location);
        println!("{}", skill_status_line(&report));
    }
    Ok(())
}

/// Map a [`SkillStatusReport`] to its single printed line. Extracted so the
/// label mapping is unit-testable without filesystem state.
fn skill_status_line(report: &SkillStatusReport) -> String {
    format!("{}: {}", report.display_name, report.label())
}

/// Render the per-file log lines for an install result. Ports the loop in
/// runInstallerWithOptions (index.ts:221-233).
fn report_write_result(display_name: &str, ctx: &InstallContext, result: &WriteResult) {
    for file in &result.files {
        // Skip the noise actions the upstream report drops on a fresh install.
        if matches!(file.action, FileAction::NotFound | FileAction::Kept) {
            continue;
        }
        println!(
            "{display_name}: {} {}",
            file.action.verb(),
            tildify(ctx, &file.path)
        );
    }
    for note in &result.notes {
        println!("{display_name}: {note}");
    }
}

/// Replace the home prefix with `~/`. Ports tildify (index.ts:437).
fn tildify(ctx: &InstallContext, path: &std::path::Path) -> String {
    if let Ok(rest) = path.strip_prefix(&ctx.home) {
        // Display the home-relative tail POSIX-style (`~/...`) on every platform,
        // so Windows backslash separators render identically to Unix.
        return format!("~/{}", rest.to_string_lossy().replace('\\', "/"));
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn temp_ctx(label: &str) -> (InstallContext, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "codegraph-installer-{label}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let home = base.join("home");
        let cwd = base.join("project");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        let ctx = InstallContext {
            home,
            cwd,
            app_data: None,
            xdg_config_home: Some(base.join("xdg")),
            hermes_home: Some(base.join("hermes")),
        };
        (ctx, base)
    }

    fn now_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn install_context_parses_locations() {
        assert!(matches!(
            parse_location(Some("global")),
            Ok(Some(Location::Global))
        ));
        assert!(matches!(
            parse_location(Some("local")),
            Ok(Some(Location::Local))
        ));
        assert!(parse_location(Some("nope")).is_err());
        assert!(matches!(parse_location(None), Ok(None)));
    }

    #[test]
    fn tildify_replaces_home() {
        let (ctx, base) = temp_ctx("tildify");
        let p = ctx.home.join("foo.json");
        assert_eq!(tildify(&ctx, &p), "~/foo.json");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn skill_status_line_renders_each_state() {
        // Given a supported target reporting each status, Then the line is
        // "<name>: <label>"; an unsupported report yields "not supported".
        let supported = |status| SkillStatusReport {
            display_name: "Claude Code",
            location: Location::Global,
            status: Some(status),
        };
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::UpToDate)),
            "Claude Code: up to date"
        );
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::LocallyModified)),
            "Claude Code: locally modified"
        );
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::Outdated)),
            "Claude Code: outdated"
        );
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::NotInstalled)),
            "Claude Code: not installed"
        );
        assert_eq!(
            skill_status_line(&SkillStatusReport {
                display_name: "Hermes Agent",
                location: Location::Local,
                status: None,
            }),
            "Hermes Agent: not supported"
        );
    }

    #[test]
    fn init_target_kiro_writes_project_local_mcp_with_concrete_path() {
        // Given a temp HOME and an indexed project root
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, base) = temp_ctx("init-kiro");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let prev_home = std::env::var_os(home_key);
        std::env::set_var(home_key, &ctx.home);

        // When init --target=kiro installs project-local config (run twice = idempotent)
        run_install_local_targets(ctx.cwd.clone(), "kiro").unwrap();
        run_install_local_targets(ctx.cwd.clone(), "kiro").unwrap();

        // Then the project's .kiro/settings/mcp.json pins this project's --path.
        // Parse the JSON (raw-string matching is unreliable on Windows, where the
        // path's backslashes are escaped as `\\` in the serialized output).
        let mcp = ctx.cwd.join(".kiro").join("settings").join("mcp.json");
        let written = fs::read_to_string(&mcp).expect("project mcp.json written");
        let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid mcp.json");
        let args = parsed["mcpServers"]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args array");
        let expected_path = serde_json::Value::String(ctx.cwd.to_string_lossy().to_string());
        assert!(args.contains(&serde_json::Value::String("--path".to_string())));
        assert!(
            args.contains(&expected_path),
            "must pin concrete project path, got: {args:?}"
        );

        match prev_home {
            Some(v) => std::env::set_var(home_key, v),
            None => std::env::remove_var(home_key),
        }
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn init_target_none_writes_nothing() {
        // Given a temp HOME and a project root
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, base) = temp_ctx("init-none");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let prev_home = std::env::var_os(home_key);
        std::env::set_var(home_key, &ctx.home);

        // When init runs with the default target (none)
        run_install_local_targets(ctx.cwd.clone(), "none").unwrap();

        // Then no agent config dir is created under the project
        assert!(
            !ctx.cwd.join(".kiro").exists() && !ctx.cwd.join(".cursor").exists(),
            "none must be a pure no-op"
        );

        match prev_home {
            Some(v) => std::env::set_var(home_key, v),
            None => std::env::remove_var(home_key),
        }
        let _ = fs::remove_dir_all(base);
    }
}
