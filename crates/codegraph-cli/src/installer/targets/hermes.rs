//! Hermes Agent target. Ports `upstream installer/targets/hermes.ts`.
//!
//! Hermes reads MCP servers from `$HERMES_HOME/config.yaml` under `mcp_servers`
//! and exposes them as `mcp-<server>` toolsets. We add `mcp_servers.codegraph`
//! and `platform_toolsets.cli: [hermes-cli, mcp-codegraph]`. Done with the same
//! line-based YAML splicing the upstream uses (no YAML dependency). Global-only.

use std::fs;
use std::path::PathBuf;

use super::super::shared::atomic_write_file;
use super::super::types::{
    AgentTarget, DetectionResult, FileAction, FileWrite, InstallContext, InstallOptions, Location,
    TargetId, WriteResult,
};

pub struct HermesTarget;

struct LineRange {
    start: usize,
    end: usize,
}

fn hermes_home(ctx: &InstallContext) -> PathBuf {
    ctx.hermes_home
        .clone()
        .unwrap_or_else(|| ctx.home.join(".hermes"))
}
fn config_path(ctx: &InstallContext) -> PathBuf {
    hermes_home(ctx).join("config.yaml")
}

fn read_text(file: &PathBuf) -> String {
    fs::read_to_string(file).unwrap_or_default()
}

impl AgentTarget for HermesTarget {
    fn id(&self) -> TargetId {
        TargetId::Hermes
    }
    fn display_name(&self) -> &'static str {
        "Hermes Agent"
    }
    fn supports_location(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult {
        if loc != Location::Global {
            return DetectionResult::default();
        }
        let file = config_path(ctx);
        let content = read_text(&file);
        let installed = hermes_home(ctx).exists() || file.exists();
        DetectionResult {
            installed,
            already_configured: has_codegraph_mcp_server(&content),
        }
    }

    // Ports hermesTarget.install (hermes.ts:54).
    fn install(&self, ctx: &InstallContext, loc: Location, _opts: InstallOptions) -> WriteResult {
        if loc != Location::Global {
            return WriteResult {
                files: Vec::new(),
                notes: vec![
                    "Hermes Agent uses $HERMES_HOME/config.yaml; re-run with --location=global."
                        .to_string(),
                ],
            };
        }
        WriteResult {
            files: vec![write_hermes_config(ctx)],
            notes: vec!["Start a new Hermes session for MCP changes to take effect.".to_string()],
        }
    }

    // Ports hermesTarget.uninstall (hermes.ts:67).
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        if loc != Location::Global {
            return WriteResult::default();
        }
        let file = config_path(ctx);
        if !file.exists() {
            return WriteResult {
                files: vec![FileWrite {
                    path: file,
                    action: FileAction::NotFound,
                }],
                notes: Vec::new(),
            };
        }
        let before = read_text(&file);
        let after = remove_codegraph_toolset(&remove_codegraph_mcp_server(&before));
        if after == before {
            return WriteResult {
                files: vec![FileWrite {
                    path: file,
                    action: FileAction::NotFound,
                }],
                notes: Vec::new(),
            };
        }
        let _ = atomic_write_file(&file, &ensure_trailing_newline(&after));
        WriteResult {
            files: vec![FileWrite {
                path: file,
                action: FileAction::Removed,
            }],
            notes: Vec::new(),
        }
    }

    // Ports hermesTarget.printConfig (hermes.ts:83).
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String {
        if loc != Location::Global {
            return "# Hermes Agent uses $HERMES_HOME/config.yaml; use --location=global.\n"
                .to_string();
        }
        format!(
            "# Add to {}\n\n{}\n\nplatform_toolsets:\n  cli:\n    - hermes-cli\n    - mcp-codegraph\n",
            config_path(ctx).display(),
            render_codegraph_mcp_block().join("\n"),
        )
    }

    fn supports_skills(&self, loc: Location) -> bool {
        loc == Location::Global
    }

    fn skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        match loc {
            Location::Global => Some(hermes_home(ctx).join("skills")),
            Location::Local => None,
        }
    }
}

// Ports writeHermesConfig (hermes.ts:123).
fn write_hermes_config(ctx: &InstallContext) -> FileWrite {
    let file = config_path(ctx);
    let existed = file.exists();
    let before = read_text(&file);
    let after_mcp = upsert_codegraph_mcp_server(&before);
    let after = upsert_codegraph_toolset(&after_mcp);
    if after == before {
        return FileWrite {
            path: file,
            action: FileAction::Unchanged,
        };
    }
    let _ = atomic_write_file(&file, &ensure_trailing_newline(&after));
    FileWrite {
        path: file,
        action: if existed {
            FileAction::Updated
        } else {
            FileAction::Created
        },
    }
}

fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

fn split_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_string)
        .collect()
}

fn join_lines(mut lines: Vec<String>) -> String {
    while lines.last().map(|s| s.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    format!("{}\n", lines.join("\n"))
}

// Ports topLevelRange (hermes.ts:150).
fn top_level_range(lines: &[String], key: &str) -> Option<LineRange> {
    let needle = format!("{key}:");
    let start = lines.iter().position(|l| l.trim() == needle)?;
    let mut end = lines.len();
    for (offset, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if is_top_level_key_line(line) {
            end = offset;
            break;
        }
    }
    Some(LineRange { start, end })
}

// Matches `^[A-Za-z_][A-Za-z0-9_-]*:\s*(?:#.*)?$` (hermes.ts:157).
fn is_top_level_key_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    let mut i = 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
    {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return false;
    }
    let rest = line[i + 1..].trim_start();
    rest.is_empty() || rest.starts_with('#')
}

// Ports childRange (hermes.ts:165).
fn child_range(lines: &[String], parent: &LineRange, child: &str) -> Option<LineRange> {
    let prefix = format!("  {child}:");
    let mut start = None;
    for (i, line) in lines
        .iter()
        .enumerate()
        .take(parent.end)
        .skip(parent.start + 1)
    {
        if matches_child_header(line, &prefix) {
            start = Some(i);
            break;
        }
    }
    let start = start?;
    let mut end = parent.end;
    for (i, line) in lines.iter().enumerate().take(parent.end).skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        // `^  \S` — two-space indent then a non-space.
        if line.starts_with("  ")
            && line
                .as_bytes()
                .get(2)
                .is_some_and(|b| !b.is_ascii_whitespace())
        {
            end = i;
            break;
        }
    }
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some(LineRange { start, end })
}

// Matches `^  <child>:\s*(?:#.*)?$`.
fn matches_child_header(line: &str, prefix: &str) -> bool {
    if !line.starts_with(prefix) {
        return false;
    }
    let rest = line[prefix.len()..].trim_start();
    rest.is_empty() || rest.starts_with('#')
}

struct ListChildBlock {
    start: usize,
    end: usize,
    item_indent: String,
}

// Ports listChildBlock (hermes.ts:207).
fn list_child_block(lines: &[String], parent: &LineRange, child: &str) -> Option<ListChildBlock> {
    let prefix = format!("  {child}:");
    let mut start = None;
    for (i, line) in lines
        .iter()
        .enumerate()
        .take(parent.end)
        .skip(parent.start + 1)
    {
        if matches_child_header(line, &prefix) {
            start = Some(i);
            break;
        }
    }
    let start = start?;
    let mut end = parent.end;
    for (i, line) in lines.iter().enumerate().take(parent.end).skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent >= 4 {
            continue;
        }
        if indent == 2 && line.starts_with("  - ") {
            continue;
        }
        end = i;
        break;
    }
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    let mut item_indent = "    ".to_string();
    for line in lines.iter().take(end).skip(start + 1) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") {
            let indent_len = line.len() - trimmed.len();
            if indent_len > 0 {
                item_indent = " ".repeat(indent_len);
                break;
            }
        }
    }
    Some(ListChildBlock {
        start,
        end,
        item_indent,
    })
}

// Ports renderCodeGraphMcpChild (hermes.ts:252).
fn render_codegraph_mcp_child() -> Vec<String> {
    vec![
        "  codegraph:".to_string(),
        "    command: codegraph".to_string(),
        "    args:".to_string(),
        "      - serve".to_string(),
        "      - --mcp".to_string(),
        "    timeout: 120".to_string(),
        "    connect_timeout: 60".to_string(),
        "    enabled: true".to_string(),
    ]
}

// Ports renderCodeGraphMcpBlock (hermes.ts:265).
fn render_codegraph_mcp_block() -> Vec<String> {
    let mut block = vec!["mcp_servers:".to_string()];
    block.extend(render_codegraph_mcp_child());
    block
}

// Ports hasCodeGraphMcpServer (hermes.ts:269).
fn has_codegraph_mcp_server(content: &str) -> bool {
    let lines = split_lines(content);
    top_level_range(&lines, "mcp_servers")
        .map(|parent| child_range(&lines, &parent, "codegraph").is_some())
        .unwrap_or(false)
}

// Ports upsertCodeGraphMcpServer (hermes.ts:275).
fn upsert_codegraph_mcp_server(content: &str) -> String {
    let mut lines = split_lines(content);
    let parent = top_level_range(&lines, "mcp_servers");
    let replacement = render_codegraph_mcp_child();

    let Some(parent) = parent else {
        if lines.last().map(|s| s.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend(render_codegraph_mcp_block());
        return join_lines(lines);
    };

    if let Some(child) = child_range(&lines, &parent, "codegraph") {
        let existing = &lines[child.start..child.end];
        if existing == replacement.as_slice() {
            return join_lines(lines);
        }
        lines.splice(child.start..child.end, replacement);
        return join_lines(lines);
    }

    lines.splice(parent.end..parent.end, replacement);
    join_lines(lines)
}

// Ports removeCodeGraphMcpServer (hermes.ts:299).
fn remove_codegraph_mcp_server(content: &str) -> String {
    let mut lines = split_lines(content);
    let Some(parent) = top_level_range(&lines, "mcp_servers") else {
        return content.to_string();
    };
    let Some(child) = child_range(&lines, &parent, "codegraph") else {
        return content.to_string();
    };
    lines.splice(child.start..child.end, std::iter::empty());
    join_lines(lines)
}

// Ports upsertCodeGraphToolset (hermes.ts:308).
fn upsert_codegraph_toolset(content: &str) -> String {
    let mut lines = split_lines(content);
    let parent = top_level_range(&lines, "platform_toolsets");

    let Some(parent) = parent else {
        if lines.last().map(|s| s.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.extend([
            "platform_toolsets:".to_string(),
            "  cli:".to_string(),
            "    - hermes-cli".to_string(),
            "    - mcp-codegraph".to_string(),
        ]);
        return join_lines(lines);
    };

    let Some(cli) = list_child_block(&lines, &parent, "cli") else {
        lines.splice(
            parent.end..parent.end,
            [
                "  cli:".to_string(),
                "    - hermes-cli".to_string(),
                "    - mcp-codegraph".to_string(),
            ],
        );
        return join_lines(lines);
    };

    let has_entry = lines[(cli.start + 1)..cli.end]
        .iter()
        .any(|l| l.trim() == "- mcp-codegraph");
    if has_entry {
        return join_lines(lines);
    }
    lines.splice(
        cli.end..cli.end,
        [format!("{}- mcp-codegraph", cli.item_indent)],
    );
    join_lines(lines)
}

// Ports removeCodeGraphToolset (hermes.ts:334).
fn remove_codegraph_toolset(content: &str) -> String {
    let lines = split_lines(content);
    let Some(parent) = top_level_range(&lines, "platform_toolsets") else {
        return content.to_string();
    };
    let Some(cli) = list_child_block(&lines, &parent, "cli") else {
        return content.to_string();
    };
    let has_entry = lines[(cli.start + 1)..cli.end]
        .iter()
        .any(|l| l.trim() == "- mcp-codegraph");
    if !has_entry {
        return content.to_string();
    }
    let next: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(idx, line)| {
            if *idx <= cli.start || *idx >= cli.end {
                return true;
            }
            line.trim() != "- mcp-codegraph"
        })
        .map(|(_, line)| line.clone())
        .collect();
    join_lines(next)
}

pub static HERMES_TARGET: HermesTarget = HermesTarget;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_home(home: PathBuf) -> InstallContext {
        InstallContext {
            home,
            cwd: PathBuf::from("/tmp/cwd"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    struct TempHermes {
        base: PathBuf,
        ctx: InstallContext,
    }

    impl TempHermes {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "cg-hermes-{label}-{}-{}",
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
                hermes_home: Some(base.join("hermes")),
            };
            Self { base, ctx }
        }
        fn config(&self) -> PathBuf {
            config_path(&self.ctx)
        }
        fn read(&self) -> String {
            fs::read_to_string(self.config()).unwrap()
        }
    }

    impl Drop for TempHermes {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn install_creates_config_with_mcp_server_and_toolset() {
        let fx = TempHermes::new("install-create");
        let target = HermesTarget;

        let detect = target.detect(&fx.ctx, Location::Global);
        assert!(!detect.installed);
        assert!(!detect.already_configured);

        let result = target.install(
            &fx.ctx,
            Location::Global,
            InstallOptions {
                auto_allow: false,
                front_load_hook: false,
            },
        );
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].action, FileAction::Created);
        assert!(!result.notes.is_empty());

        let content = fx.read();
        assert!(content.contains("mcp_servers:"));
        assert!(content.contains("  codegraph:"));
        assert!(content.contains("    command: codegraph"));
        assert!(content.contains("      - serve"));
        assert!(content.contains("      - --mcp"));
        assert!(content.contains("platform_toolsets:"));
        assert!(content.contains("    - hermes-cli"));
        assert!(content.contains("    - mcp-codegraph"));
        assert!(content.ends_with('\n'));

        let detect = target.detect(&fx.ctx, Location::Global);
        assert!(detect.installed);
        assert!(detect.already_configured);
    }

    #[test]
    fn install_is_idempotent() {
        let fx = TempHermes::new("idempotent");
        let target = HermesTarget;
        let opts = InstallOptions {
            auto_allow: false,
            front_load_hook: false,
        };

        target.install(&fx.ctx, Location::Global, opts);
        let first = fx.read();
        let second_result = target.install(&fx.ctx, Location::Global, opts);
        assert_eq!(second_result.files[0].action, FileAction::Unchanged);
        assert_eq!(fx.read(), first, "re-install must not churn");
        assert_eq!(first.matches("  codegraph:").count(), 1);
        assert_eq!(first.matches("- mcp-codegraph").count(), 1);
    }

    #[test]
    fn install_preserves_existing_config_and_adds_to_existing_sections() {
        let fx = TempHermes::new("preserve");
        let target = HermesTarget;
        fs::create_dir_all(fx.config().parent().unwrap()).unwrap();
        fs::write(
            fx.config(),
            "log_level: info\n\nmcp_servers:\n  other:\n    command: foo\n\nplatform_toolsets:\n  cli:\n    - hermes-cli\n",
        )
        .unwrap();

        target.install(
            &fx.ctx,
            Location::Global,
            InstallOptions {
                auto_allow: false,
                front_load_hook: false,
            },
        );
        let content = fx.read();
        assert!(content.contains("log_level: info"));
        assert!(content.contains("  other:"));
        assert!(content.contains("  codegraph:"));
        assert!(content.contains("- hermes-cli"));
        assert!(content.contains("- mcp-codegraph"));
    }

    #[test]
    fn uninstall_removes_codegraph_preserving_siblings() {
        let fx = TempHermes::new("uninstall");
        let target = HermesTarget;
        fs::create_dir_all(fx.config().parent().unwrap()).unwrap();
        fs::write(fx.config(), "mcp_servers:\n  other:\n    command: foo\n").unwrap();

        target.install(
            &fx.ctx,
            Location::Global,
            InstallOptions {
                auto_allow: false,
                front_load_hook: false,
            },
        );
        assert!(fx.read().contains("  codegraph:"));

        let removed = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(removed.files[0].action, FileAction::Removed);
        let content = fx.read();
        assert!(!content.contains("  codegraph:"));
        assert!(!content.contains("- mcp-codegraph"));
        assert!(content.contains("  other:"), "sibling server preserved");
    }

    #[test]
    fn uninstall_missing_file_is_not_found() {
        let fx = TempHermes::new("uninstall-missing");
        let target = HermesTarget;
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
    }

    #[test]
    fn uninstall_config_without_codegraph_is_not_found() {
        let fx = TempHermes::new("uninstall-absent");
        let target = HermesTarget;
        fs::create_dir_all(fx.config().parent().unwrap()).unwrap();
        fs::write(fx.config(), "log_level: info\n").unwrap();
        let result = target.uninstall(&fx.ctx, Location::Global);
        assert_eq!(result.files[0].action, FileAction::NotFound);
        assert_eq!(fx.read(), "log_level: info\n");
    }

    #[test]
    fn local_location_is_rejected_for_all_ops() {
        let fx = TempHermes::new("local-reject");
        let target = HermesTarget;
        assert!(!target.supports_location(Location::Local));

        let install = target.install(
            &fx.ctx,
            Location::Local,
            InstallOptions {
                auto_allow: false,
                front_load_hook: false,
            },
        );
        assert!(install.files.is_empty());
        assert!(install.notes[0].contains("--location=global"));

        let uninstall = target.uninstall(&fx.ctx, Location::Local);
        assert!(uninstall.files.is_empty());

        let detect = target.detect(&fx.ctx, Location::Local);
        assert!(!detect.installed);

        let printed = target.print_config(&fx.ctx, Location::Local);
        assert!(printed.contains("--location=global"));
    }

    #[test]
    fn print_config_global_shows_block_and_toolset() {
        let fx = TempHermes::new("print");
        let target = HermesTarget;
        let out = target.print_config(&fx.ctx, Location::Global);
        assert!(out.contains("mcp_servers:"));
        assert!(out.contains("codegraph:"));
        assert!(out.contains("command: codegraph"));
        assert!(out.contains("platform_toolsets:"));
        assert!(out.contains("- hermes-cli"));
        assert!(out.contains("- mcp-codegraph"));
        assert!(out.contains(&config_path(&fx.ctx).display().to_string()));
    }

    #[test]
    fn default_hermes_home_when_env_unset() {
        let ctx = ctx_with_home(PathBuf::from("/tmp/h"));
        assert_eq!(hermes_home(&ctx), PathBuf::from("/tmp/h/.hermes"));
        assert_eq!(
            config_path(&ctx),
            PathBuf::from("/tmp/h/.hermes/config.yaml")
        );
    }

    #[test]
    fn crlf_input_is_normalized_to_lf() {
        let out = upsert_codegraph_mcp_server("mcp_servers:\r\n  other:\r\n    command: foo\r\n");
        assert!(!out.contains('\r'));
        assert!(out.contains("  codegraph:"));
        assert!(out.contains("  other:"));
    }

    #[test]
    fn is_top_level_key_line_recognizes_shapes() {
        assert!(is_top_level_key_line("mcp_servers:"));
        assert!(is_top_level_key_line("key: # comment"));
        assert!(is_top_level_key_line("_private:"));
        assert!(is_top_level_key_line("with-dash:"));
        assert!(!is_top_level_key_line("  indented:"));
        assert!(!is_top_level_key_line("key: value"));
        assert!(!is_top_level_key_line("- item"));
        assert!(!is_top_level_key_line(""));
        assert!(!is_top_level_key_line("1nvalid:"));
        assert!(!is_top_level_key_line("nocolon"));
    }

    #[test]
    fn upsert_mcp_server_replaces_divergent_existing() {
        let existing = "mcp_servers:\n  codegraph:\n    command: OLD\n";
        let out = upsert_codegraph_mcp_server(existing);
        assert!(out.contains("    command: codegraph"));
        assert!(!out.contains("OLD"));
        assert_eq!(out.matches("  codegraph:").count(), 1);
    }

    #[test]
    fn upsert_toolset_creates_cli_when_parent_lacks_it() {
        let existing = "platform_toolsets:\n  other:\n    - thing\n";
        let out = upsert_codegraph_toolset(existing);
        assert!(out.contains("  cli:"));
        assert!(out.contains("    - hermes-cli"));
        assert!(out.contains("    - mcp-codegraph"));
        assert!(out.contains("  other:"));
    }

    #[test]
    fn upsert_toolset_appends_to_existing_cli_list() {
        let existing = "platform_toolsets:\n  cli:\n    - hermes-cli\n";
        let out = upsert_codegraph_toolset(existing);
        assert_eq!(out.matches("- mcp-codegraph").count(), 1);
        assert!(out.contains("- hermes-cli"));
    }

    #[test]
    fn upsert_toolset_idempotent_when_entry_present() {
        let existing = "platform_toolsets:\n  cli:\n    - hermes-cli\n    - mcp-codegraph\n";
        let out = upsert_codegraph_toolset(existing);
        assert_eq!(out.matches("- mcp-codegraph").count(), 1);
    }

    #[test]
    fn remove_toolset_absent_entry_returns_unchanged() {
        let existing = "platform_toolsets:\n  cli:\n    - hermes-cli\n";
        assert_eq!(remove_codegraph_toolset(existing), existing);
        assert_eq!(
            remove_codegraph_toolset("log_level: info\n"),
            "log_level: info\n"
        );
    }

    #[test]
    fn remove_mcp_server_absent_returns_unchanged() {
        assert_eq!(
            remove_codegraph_mcp_server("log_level: info\n"),
            "log_level: info\n"
        );
        let no_child = "mcp_servers:\n  other:\n    command: foo\n";
        assert_eq!(remove_codegraph_mcp_server(no_child), no_child);
    }

    #[test]
    fn has_codegraph_mcp_server_detects_presence() {
        assert!(has_codegraph_mcp_server(
            "mcp_servers:\n  codegraph:\n    command: codegraph\n"
        ));
        assert!(!has_codegraph_mcp_server(
            "mcp_servers:\n  other:\n    x: 1\n"
        ));
        assert!(!has_codegraph_mcp_server("log_level: info\n"));
    }

    #[test]
    fn upsert_appends_block_when_no_top_level_key() {
        let out = upsert_codegraph_mcp_server("log_level: info\n");
        assert!(out.contains("log_level: info"));
        assert!(out.contains("mcp_servers:"));
        assert!(out.contains("  codegraph:"));
        let toolset = upsert_codegraph_toolset(&out);
        assert!(toolset.contains("platform_toolsets:"));
        assert!(toolset.contains("- mcp-codegraph"));
    }

    #[test]
    fn hermes_skills_are_global_only() {
        // Given the Hermes target with a default hermes_home (~/.hermes).
        let target = HermesTarget;
        let ctx = ctx_with_home(PathBuf::from("/tmp/home"));

        // Then skills are supported ONLY at Global.
        assert!(target.supports_skills(Location::Global));
        assert!(!target.supports_skills(Location::Local));

        // And the Global skill dir is the PARENT `<hermes_home>/skills`.
        let global = target.skill_dir(&ctx, Location::Global).unwrap();
        assert!(global.ends_with(".hermes/skills"));

        // And there is NO Local skill dir.
        assert!(target.skill_dir(&ctx, Location::Local).is_none());
    }

    #[test]
    fn hermes_local_skill_install_returns_unsupported_note() {
        // Given the Hermes target (global-only for skills).
        let target = HermesTarget;
        let ctx = ctx_with_home(PathBuf::from("/tmp/home"));

        // When installing a skill for --location=local via the T3 default.
        let result = target.install_skill(&ctx, Location::Local, false);

        // Then no files are written and a "not supported" note is emitted.
        assert!(result.files.is_empty());
        assert_eq!(result.notes.len(), 1);
        assert!(
            result.notes[0].contains("skills not supported by Hermes Agent for --location=local")
        );
    }
}
