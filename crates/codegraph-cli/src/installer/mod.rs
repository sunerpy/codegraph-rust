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
mod vscode_user;

use std::path::PathBuf;

use anyhow::{Result, bail};

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
/// actions; `yes` is consumed by install/uninstall, while update consumes
/// `force`, `show_diff`, and `dry_run`.
pub struct SkillArgs {
    pub target: Option<String>,
    pub location: Option<String>,
    pub yes: bool,
    pub force: bool,
    pub show_diff: bool,
    pub dry_run: bool,
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

/// `codegraph skill update`: preview the version transition + line counts, then
/// refresh safe/outdated skill content and each target's marker-managed
/// instructions block. `--diff` adds a unified skill diff, `--dry-run`
/// suppresses every write, and `--force` overwrites locally modified skill
/// content. User text outside the managed instructions markers is untouched.
pub fn run_skill_update(args: SkillArgs) -> Result<()> {
    let ctx = context_from_env()?;
    let (location, targets) = resolve_skill_targets(&ctx, &args)?;
    if targets.is_empty() {
        println!("No agent targets selected — nothing to do.");
        return Ok(());
    }

    let mut skill_changed_names: Vec<&str> = Vec::new();
    let mut instructions_changed_names: Vec<&str> = Vec::new();
    for target in &targets {
        let Some(skill_dir) = target.resolved_skill_dir(&ctx, location) else {
            println!(
                "{}: skills not supported for --location={}",
                target.display_name(),
                location.as_str()
            );
            continue;
        };
        let preview = skill::preview_update_for_dir(&skill_dir, args.force);
        println!(
            "{}",
            skill_update_preview_line(target.display_name(), &preview, args.dry_run)
        );
        if args.show_diff
            && let Some(diff) = preview.unified_diff.as_deref()
        {
            print!("{diff}");
        }

        let instructions_preview = target.preview_managed_instructions(&ctx, location);
        if let Some(file) = &instructions_preview {
            println!(
                "{}",
                managed_instructions_preview_line(target.display_name(), &ctx, file, args.dry_run)
            );
        }

        if args.dry_run {
            if preview.decision == skill::SkillUpdateDecision::Update {
                skill_changed_names.push(target.display_name());
            }
            if instructions_preview.as_ref().is_some_and(|file| {
                matches!(file.action, FileAction::Created | FileAction::Updated)
            }) {
                instructions_changed_names.push(target.display_name());
            }
            continue;
        }

        let result = target.install_skill(&ctx, location, args.force);
        if result
            .files
            .iter()
            .any(|file| matches!(file.action, FileAction::Created | FileAction::Updated))
        {
            skill_changed_names.push(target.display_name());
        }
        report_write_result(target.display_name(), &ctx, &result);

        if let Some(file) = target.refresh_managed_instructions(&ctx, location) {
            if matches!(file.action, FileAction::Created | FileAction::Updated) {
                instructions_changed_names.push(target.display_name());
            }
            report_write_result(
                target.display_name(),
                &ctx,
                &WriteResult {
                    files: vec![file],
                    notes: Vec::new(),
                },
            );
        }
    }

    if args.dry_run {
        if skill_changed_names.is_empty() && instructions_changed_names.is_empty() {
            println!("\nDry run: no CodeGraph skill assets would change.");
        }
        if !skill_changed_names.is_empty() {
            println!(
                "\nDry run: would update the CodeGraph skill for {} agent{}: {}.",
                skill_changed_names.len(),
                if skill_changed_names.len() > 1 {
                    "s"
                } else {
                    ""
                },
                skill_changed_names.join(", ")
            );
        }
        if !instructions_changed_names.is_empty() {
            println!(
                "\nDry run: would refresh managed CodeGraph instructions for {} agent{}: {}.",
                instructions_changed_names.len(),
                if instructions_changed_names.len() > 1 {
                    "s"
                } else {
                    ""
                },
                instructions_changed_names.join(", ")
            );
        }
    } else {
        if !skill_changed_names.is_empty() {
            println!(
                "\nUpdated the CodeGraph skill for {} agent{}: {}.",
                skill_changed_names.len(),
                if skill_changed_names.len() > 1 {
                    "s"
                } else {
                    ""
                },
                skill_changed_names.join(", ")
            );
        }
        if !instructions_changed_names.is_empty() {
            println!(
                "\nRefreshed managed CodeGraph instructions for {} agent{}: {}.",
                instructions_changed_names.len(),
                if instructions_changed_names.len() > 1 {
                    "s"
                } else {
                    ""
                },
                instructions_changed_names.join(", ")
            );
        }
    }
    Ok(())
}

fn managed_instructions_preview_line(
    display_name: &str,
    ctx: &InstallContext,
    file: &types::FileWrite,
    dry_run: bool,
) -> String {
    let path = tildify(ctx, &file.path);
    match (file.action, dry_run) {
        (FileAction::Created, true) => {
            format!("{display_name}: would create managed instructions {path}")
        }
        (FileAction::Updated, true) => {
            format!("{display_name}: would refresh managed instructions {path}")
        }
        (FileAction::Unchanged, true) => {
            format!("{display_name}: managed instructions up to date {path}")
        }
        (FileAction::Created, false) => {
            format!("{display_name}: creating managed instructions {path}")
        }
        (FileAction::Updated, false) => {
            format!("{display_name}: refreshing managed instructions {path}")
        }
        (FileAction::Unchanged, false) => {
            format!("{display_name}: managed instructions up to date {path}")
        }
        (action, _) => format!(
            "{display_name}: {} managed instructions {path}",
            action.verb()
        ),
    }
}

// Keep the summary logic separate for skills and instructions: a target may
// have an up-to-date or locally modified SKILL.md while its marker-managed
// AGENTS/CLAUDE/GEMINI block still needs a safe refresh.

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
    let _ = (args.yes, args.force, args.show_diff, args.dry_run);
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
    let current = skill::EMBEDDED_VERSION;
    match report.status {
        None => format!("{}: not supported", report.display_name),
        Some(skill::SkillStatus::NotInstalled) => {
            format!(
                "{}: not installed (embedded {current})",
                report.display_name
            )
        }
        Some(skill::SkillStatus::UpToDate) => {
            format!("{}: up to date ({current})", report.display_name)
        }
        Some(skill::SkillStatus::Outdated) => format!(
            "{}: outdated ({} -> {current})",
            report.display_name,
            report.installed_version.as_deref().unwrap_or("unknown")
        ),
        Some(skill::SkillStatus::LocallyModified) => format!(
            "{}: locally modified (base {}; embedded {current})",
            report.display_name,
            report.installed_version.as_deref().unwrap_or("unknown")
        ),
    }
}

fn skill_update_preview_line(
    display_name: &str,
    preview: &skill::SkillUpdatePreview,
    dry_run: bool,
) -> String {
    let current = skill::EMBEDDED_VERSION;
    let change = format!("+{} -{}", preview.added_lines, preview.removed_lines);
    let action = if dry_run { "would update" } else { "updating" };
    match (preview.decision, preview.status) {
        (skill::SkillUpdateDecision::Unchanged, _) => {
            format!("{display_name}: up to date ({current})")
        }
        (skill::SkillUpdateDecision::LocallyModified, _) => format!(
            "{display_name}: locally modified (base {}; embedded {current}; {change}) — skipped; use --force to overwrite",
            preview.installed_version.as_deref().unwrap_or("unknown")
        ),
        (skill::SkillUpdateDecision::Update, skill::SkillStatus::NotInstalled) => {
            let action = if dry_run {
                "would install"
            } else {
                "installing"
            };
            format!("{display_name}: {action} embedded skill {current} ({change})")
        }
        (skill::SkillUpdateDecision::Update, skill::SkillStatus::LocallyModified) => format!(
            "{display_name}: {action} locally modified skill (base {}; embedded {current}; {change})",
            preview.installed_version.as_deref().unwrap_or("unknown")
        ),
        (skill::SkillUpdateDecision::Update, skill::SkillStatus::UpToDate) => {
            format!("{display_name}: {action} {current} -> {current} ({change})")
        }
        (skill::SkillUpdateDecision::Update, skill::SkillStatus::Outdated) => format!(
            "{display_name}: {action} {} -> {current} ({change})",
            preview.installed_version.as_deref().unwrap_or("unknown")
        ),
    }
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
    use crate::installer::types::FileWrite;
    use crate::test_env::env_guard;
    use std::fs;

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
        // Given a supported target reporting each status, Then the line carries
        // the installed/embedded version context; unsupported stays concise.
        let supported = |status, installed_version: Option<&str>| SkillStatusReport {
            display_name: "Claude Code",
            location: Location::Global,
            status: Some(status),
            installed_version: installed_version.map(str::to_string),
        };
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::UpToDate, None)),
            format!("Claude Code: up to date ({})", skill::EMBEDDED_VERSION)
        );
        assert_eq!(
            skill_status_line(&supported(
                skill::SkillStatus::LocallyModified,
                Some("0.40.1")
            )),
            format!(
                "Claude Code: locally modified (base 0.40.1; embedded {})",
                skill::EMBEDDED_VERSION
            )
        );
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::Outdated, Some("0.40.1"))),
            format!(
                "Claude Code: outdated (0.40.1 -> {})",
                skill::EMBEDDED_VERSION
            )
        );
        assert_eq!(
            skill_status_line(&supported(skill::SkillStatus::NotInstalled, None)),
            format!(
                "Claude Code: not installed (embedded {})",
                skill::EMBEDDED_VERSION
            )
        );
        assert_eq!(
            skill_status_line(&SkillStatusReport {
                display_name: "Hermes Agent",
                location: Location::Local,
                status: None,
                installed_version: None,
            }),
            "Hermes Agent: not supported"
        );
    }

    #[test]
    fn init_target_kiro_writes_project_local_mcp_with_concrete_path() {
        // Given a temp HOME and an indexed project root
        let mut env = env_guard();
        let (ctx, base) = temp_ctx("init-kiro");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        env.set(home_key, &ctx.home);

        // When init --target=kiro installs project-local config (run twice = idempotent)
        run_install_local_targets(ctx.cwd.clone(), "kiro").unwrap();
        run_install_local_targets(ctx.cwd.clone(), "kiro").unwrap();

        // Then the project's .kiro/settings/mcp.json pins this project's --path.
        // Parse the JSON (raw-string matching is unreliable on Windows, where the
        // path's backslashes are escaped as `\\` in the serialized output).
        let mcp = ctx.cwd.join(".kiro").join("settings").join("mcp.json");
        let written = fs::read_to_string(&mcp).expect("project mcp.json written");
        let parsed = serde_json::Value::Object(
            crate::installer::shared::parse_json_object(&written).expect("valid jsonc mcp.json"),
        );
        let args = parsed["mcpServers"]["codegraph"]["args"]
            .as_array()
            .expect("codegraph args array");
        let expected_path = serde_json::Value::String(ctx.cwd.to_string_lossy().to_string());
        assert!(args.contains(&serde_json::Value::String("--path".to_string())));
        assert!(
            args.contains(&expected_path),
            "must pin concrete project path, got: {args:?}"
        );

        env.assert_intact();
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn init_target_none_writes_nothing() {
        // Given a temp HOME and a project root
        let mut env = env_guard();
        let (ctx, base) = temp_ctx("init-none");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        env.set(home_key, &ctx.home);

        // When init runs with the default target (none)
        run_install_local_targets(ctx.cwd.clone(), "none").unwrap();

        // Then no agent config dir is created under the project
        assert!(
            !ctx.cwd.join(".kiro").exists() && !ctx.cwd.join(".cursor").exists(),
            "none must be a pure no-op"
        );

        env.assert_intact();
        let _ = fs::remove_dir_all(base);
    }

    fn install_args(target: &str, location: &str) -> InstallArgs {
        InstallArgs {
            target: Some(target.to_string()),
            location: Some(location.to_string()),
            yes: true,
            permissions: None,
            front_load_hook: false,
            print_config: None,
        }
    }

    #[test]
    fn run_install_with_ctx_installs_selected_target() {
        let (ctx, base) = temp_ctx("run-install");
        let cwd = ctx.cwd.clone();
        run_install_with_ctx(ctx, install_args("gemini", "local")).unwrap();
        assert!(cwd.join(".gemini").join("settings.json").exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_install_with_ctx_reports_already_configured_on_reinstall() {
        let (ctx, base) = temp_ctx("run-reinstall");
        run_install_with_ctx(ctx.clone(), install_args("gemini", "local")).unwrap();
        run_install_with_ctx(ctx, install_args("gemini", "local")).unwrap();
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_install_with_ctx_skips_unsupported_location() {
        let (ctx, base) = temp_ctx("run-skip");
        run_install_with_ctx(ctx.clone(), install_args("codex", "local")).unwrap();
        assert!(!ctx.home.join(".codex").exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_install_with_ctx_none_target_is_noop() {
        let (ctx, base) = temp_ctx("run-none");
        run_install_with_ctx(ctx, install_args("none", "global")).unwrap();
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_install_with_ctx_print_config_writes_nothing() {
        let (ctx, base) = temp_ctx("run-print");
        let home = ctx.home.clone();
        let args = InstallArgs {
            target: None,
            location: Some("global".to_string()),
            yes: false,
            permissions: None,
            front_load_hook: false,
            print_config: Some("codex".to_string()),
        };
        run_install_with_ctx(ctx, args).unwrap();
        assert!(!home.join(".codex").exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_install_with_ctx_print_config_unknown_errors() {
        let (ctx, base) = temp_ctx("run-print-unknown");
        let args = InstallArgs {
            target: None,
            location: None,
            yes: false,
            permissions: None,
            front_load_hook: false,
            print_config: Some("bogus".to_string()),
        };
        let result = run_install_with_ctx(ctx, args);
        assert!(result.is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn uninstall_targets_reports_removed_notconfigured_unsupported() {
        let (ctx, base) = temp_ctx("uninstall-sweep");
        let gemini = registry::get_target("gemini").unwrap();
        let codex = registry::get_target("codex").unwrap();

        gemini.install(
            &ctx,
            Location::Local,
            InstallOptions {
                auto_allow: false,
                front_load_hook: false,
            },
        );

        let reports = uninstall_targets(&ctx, &[gemini, codex], Location::Local);
        assert!(matches!(reports[0].status, UninstallStatus::Removed));
        assert!(!reports[0].removed_paths.is_empty());
        assert!(matches!(reports[1].status, UninstallStatus::Unsupported));
        assert!(!reports[1].notes.is_empty());

        let again = uninstall_targets(&ctx, &[gemini], Location::Local);
        assert!(matches!(again[0].status, UninstallStatus::NotConfigured));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn report_write_result_skips_notfound_and_kept() {
        let (ctx, base) = temp_ctx("report");
        let result = WriteResult {
            files: vec![
                FileWrite {
                    path: ctx.home.join("a.json"),
                    action: FileAction::Created,
                },
                FileWrite {
                    path: ctx.home.join("b.json"),
                    action: FileAction::NotFound,
                },
                FileWrite {
                    path: ctx.home.join("c.json"),
                    action: FileAction::Kept,
                },
            ],
            notes: vec!["a note".to_string()],
        };
        report_write_result("Test", &ctx, &result);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_uninstall_with_ctx_via_public_paths() {
        let mut env = env_guard();
        let (ctx, base) = temp_ctx("run-uninstall");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        env.set(home_key, &ctx.home);
        env.set("XDG_CONFIG_HOME", ctx.xdg_config_home.as_ref().unwrap());

        run_uninstall(UninstallArgs {
            target: Some("gemini".to_string()),
            location: Some("global".to_string()),
            yes: true,
        })
        .unwrap();

        env.assert_intact();
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn run_skill_install_and_uninstall_and_status_via_ctx() {
        let mut env = env_guard();
        let (ctx, base) = temp_ctx("run-skill");
        let home_key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        env.set(home_key, &ctx.home);

        run_skill_install(SkillArgs {
            target: Some("claude".to_string()),
            location: Some("global".to_string()),
            yes: true,
            force: false,
            show_diff: false,
            dry_run: false,
        })
        .unwrap();
        run_skill_status(SkillArgs {
            target: Some("claude".to_string()),
            location: Some("global".to_string()),
            yes: false,
            force: false,
            show_diff: false,
            dry_run: false,
        })
        .unwrap();
        run_skill_uninstall(SkillArgs {
            target: Some("claude".to_string()),
            location: Some("global".to_string()),
            yes: true,
            force: false,
            show_diff: false,
            dry_run: false,
        })
        .unwrap();

        env.assert_intact();
        let _ = fs::remove_dir_all(base);
    }
}
