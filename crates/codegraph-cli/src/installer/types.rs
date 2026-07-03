//! Agent target abstraction for the installer.
//!
//! Ports `upstream installer/targets/types.ts`. Each MCP-capable
//! agent implements `AgentTarget` so the orchestrator can write the right
//! MCP-server config + instructions + permissions without baking
//! client-specific paths into the dispatch. Adding a new agent = one new
//! module in `targets/` + one entry in `registry.rs`.

use std::path::PathBuf;

use super::skill;

/// Ports the `Location` union (types.ts:15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Global,
    Local,
}

impl Location {
    pub fn as_str(self) -> &'static str {
        match self {
            Location::Global => "global",
            Location::Local => "local",
        }
    }
}

/// Stable id used in `--target` and the registry. Ports `TargetId` (types.ts:22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetId {
    Claude,
    Cursor,
    Codex,
    Opencode,
    Hermes,
    Gemini,
    Antigravity,
    Kiro,
    Trae,
    Qoder,
    Zed,
}

impl TargetId {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetId::Claude => "claude",
            TargetId::Cursor => "cursor",
            TargetId::Codex => "codex",
            TargetId::Opencode => "opencode",
            TargetId::Hermes => "hermes",
            TargetId::Gemini => "gemini",
            TargetId::Antigravity => "antigravity",
            TargetId::Kiro => "kiro",
            TargetId::Trae => "trae",
            TargetId::Qoder => "qoder",
            TargetId::Zed => "zed",
        }
    }
}

/// Ports the `WriteResult.files[].action` union (types.ts:54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
    Kept,
    Skipped,
}

impl FileAction {
    pub fn verb(self) -> &'static str {
        match self {
            FileAction::Created => "Created",
            FileAction::Updated => "Updated",
            FileAction::Unchanged => "Unchanged",
            FileAction::Removed => "Removed",
            FileAction::NotFound => "Not found",
            FileAction::Kept => "Kept",
            FileAction::Skipped => "Skipped (left unchanged)",
        }
    }
}

/// One file the target touched. Ports `WriteResult.files[]` (types.ts:52).
#[derive(Debug, Clone)]
pub struct FileWrite {
    pub path: PathBuf,
    pub action: FileAction,
}

/// What `install`/`uninstall` changed on disk. Ports `WriteResult` (types.ts:51).
#[derive(Debug, Clone, Default)]
pub struct WriteResult {
    pub files: Vec<FileWrite>,
    pub notes: Vec<String>,
}

/// Ports `InstallOptions` (types.ts:64).
#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub auto_allow: bool,
    /// Opt-in: write the Claude `UserPromptSubmit` front-load hook that invokes
    /// `codegraph prompt-hook`. Off by default; never implied by `--yes`.
    pub front_load_hook: bool,
}

/// Ports `DetectionResult` (types.ts:37).
#[derive(Debug, Clone, Default)]
pub struct DetectionResult {
    pub installed: bool,
    pub already_configured: bool,
}

/// Filesystem roots a target resolves paths against.
///
/// The upstream reads `os.homedir()` / `process.cwd()` directly inside each target.
/// Threading them here is the one structural divergence from the TS source —
/// it keeps the per-target path logic byte-faithful while making the writers
/// testable against temp dirs without `chdir`/`setenv` races.
#[derive(Debug, Clone)]
pub struct InstallContext {
    pub home: PathBuf,
    pub cwd: PathBuf,
    /// `%APPDATA%` equivalent, used only by the opencode legacy-Windows sweep.
    pub app_data: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`, used only by the opencode global config dir.
    pub xdg_config_home: Option<PathBuf>,
    /// `$HERMES_HOME`, used only by the Hermes target.
    pub hermes_home: Option<PathBuf>,
}

/// Ports `AgentTarget` (types.ts:73). `&self` is the frozen registry singleton.
pub trait AgentTarget {
    fn id(&self) -> TargetId;
    fn display_name(&self) -> &'static str;
    fn supports_location(&self, loc: Location) -> bool;
    fn detect(&self, ctx: &InstallContext, loc: Location) -> DetectionResult;
    fn install(&self, ctx: &InstallContext, loc: Location, opts: InstallOptions) -> WriteResult;
    fn uninstall(&self, ctx: &InstallContext, loc: Location) -> WriteResult;
    fn print_config(&self, ctx: &InstallContext, loc: Location) -> String;

    // --- Skill-capability surface (all default; targets opt in) --------------
    //
    // A target enables skill install/uninstall/status by overriding ONLY
    // [`supports_skills`] (→ `true`) and [`skill_dir`] (→ `Some(parent)`); the
    // install/uninstall/status methods below then delegate to the shared
    // `skill::*` engine for free. Targets that do not override stay unsupported
    // and the default `install_skill`/`uninstall_skill` emit a "not supported"
    // note (no files, no error) while `skill_status` reports an unsupported
    // report.

    /// Whether this target supports skill embedding at `loc`. Default: `false`.
    fn supports_skills(&self, _loc: Location) -> bool {
        false
    }

    /// The PARENT skills directory for `loc` (e.g. `~/.claude/skills`); the
    /// engine appends `codegraph/SKILL.md` itself. Default: `None`.
    fn skill_dir(&self, _ctx: &InstallContext, _loc: Location) -> Option<PathBuf> {
        None
    }

    /// Install the embedded skill. When unsupported (or no `skill_dir`), returns
    /// a no-file [`WriteResult`] whose `notes` explains the skip; otherwise
    /// delegates to [`skill::write_skill_to_dir`].
    fn install_skill(&self, ctx: &InstallContext, loc: Location, force: bool) -> WriteResult {
        match self.resolved_skill_dir(ctx, loc) {
            Some(dir) => skill::write_skill_to_dir(&dir, force),
            None => self.unsupported_skill_write(loc),
        }
    }

    /// Uninstall the embedded skill. When unsupported, returns a no-file
    /// [`WriteResult`] with an explanatory note; otherwise delegates to
    /// [`skill::uninstall_from_dir`].
    fn uninstall_skill(&self, ctx: &InstallContext, loc: Location) -> WriteResult {
        match self.resolved_skill_dir(ctx, loc) {
            Some(dir) => skill::uninstall_from_dir(&dir),
            None => self.unsupported_skill_write(loc),
        }
    }

    /// Report the installed-skill status. When unsupported, the report's
    /// `unsupported` flag is set; otherwise delegates to
    /// [`skill::status_for_dir`].
    fn skill_status(&self, ctx: &InstallContext, loc: Location) -> SkillStatusReport {
        match self.resolved_skill_dir(ctx, loc) {
            Some(dir) => SkillStatusReport {
                display_name: self.display_name(),
                location: loc,
                status: Some(skill::status_for_dir(&dir)),
            },
            None => SkillStatusReport {
                display_name: self.display_name(),
                location: loc,
                status: None,
            },
        }
    }

    /// Resolve the effective skill parent dir: `Some` only when the target both
    /// claims support at `loc` AND yields a `skill_dir`. Not part of the public
    /// override surface — it funnels the two opt-in hooks into one decision so
    /// the three engine-delegating methods share identical gating.
    fn resolved_skill_dir(&self, ctx: &InstallContext, loc: Location) -> Option<PathBuf> {
        if !self.supports_skills(loc) {
            return None;
        }
        self.skill_dir(ctx, loc)
    }

    /// The shared "skills not supported" [`WriteResult`] (no files, one note,
    /// success exit). Used by both `install_skill` and `uninstall_skill`.
    fn unsupported_skill_write(&self, loc: Location) -> WriteResult {
        WriteResult {
            files: Vec::new(),
            notes: vec![format!(
                "skills not supported by {} for --location={}",
                self.display_name(),
                loc.as_str()
            )],
        }
    }
}

/// A target's installed-skill status, ready for the `status` command to
/// render. `status` is `None` when the target does not support skills at the
/// queried `location` (an "unsupported"/"not supported" state, not an error).
#[derive(Debug, Clone)]
pub struct SkillStatusReport {
    pub display_name: &'static str,
    /// Queried location; carried for callers/tests, not read by the status line.
    #[allow(dead_code)]
    pub location: Location,
    pub status: Option<skill::SkillStatus>,
}

impl SkillStatusReport {
    /// `true` when the target does not support skills at this location.
    #[allow(dead_code)]
    pub fn is_unsupported(&self) -> bool {
        self.status.is_none()
    }

    /// Human-readable label: the underlying [`skill::SkillStatus::label`] when
    /// supported, else `"not supported"`.
    pub fn label(&self) -> &'static str {
        match self.status {
            Some(status) => status.label(),
            None => "not supported",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_ctx(label: &str) -> (InstallContext, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "codegraph-types-{label}-{}-{}",
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
        (ctx, base)
    }

    struct UnsupportedTarget;

    impl AgentTarget for UnsupportedTarget {
        fn id(&self) -> TargetId {
            TargetId::Claude
        }
        fn display_name(&self) -> &'static str {
            "Dummy Unsupported"
        }
        fn supports_location(&self, _loc: Location) -> bool {
            true
        }
        fn detect(&self, _ctx: &InstallContext, _loc: Location) -> DetectionResult {
            DetectionResult::default()
        }
        fn install(
            &self,
            _ctx: &InstallContext,
            _loc: Location,
            _opts: InstallOptions,
        ) -> WriteResult {
            WriteResult::default()
        }
        fn uninstall(&self, _ctx: &InstallContext, _loc: Location) -> WriteResult {
            WriteResult::default()
        }
        fn print_config(&self, _ctx: &InstallContext, _loc: Location) -> String {
            String::new()
        }
    }

    struct SupportingTarget {
        skills_parent: PathBuf,
    }

    impl AgentTarget for SupportingTarget {
        fn id(&self) -> TargetId {
            TargetId::Claude
        }
        fn display_name(&self) -> &'static str {
            "Dummy Supporting"
        }
        fn supports_location(&self, _loc: Location) -> bool {
            true
        }
        fn detect(&self, _ctx: &InstallContext, _loc: Location) -> DetectionResult {
            DetectionResult::default()
        }
        fn install(
            &self,
            _ctx: &InstallContext,
            _loc: Location,
            _opts: InstallOptions,
        ) -> WriteResult {
            WriteResult::default()
        }
        fn uninstall(&self, _ctx: &InstallContext, _loc: Location) -> WriteResult {
            WriteResult::default()
        }
        fn print_config(&self, _ctx: &InstallContext, _loc: Location) -> String {
            String::new()
        }

        // The whole point of T3: a supporting target overrides ONLY these two.
        fn supports_skills(&self, _loc: Location) -> bool {
            true
        }
        fn skill_dir(&self, _ctx: &InstallContext, _loc: Location) -> Option<PathBuf> {
            Some(self.skills_parent.clone())
        }
    }

    #[test]
    fn default_target_does_not_support_skills() {
        // Given a target that does not override the skill hooks
        let (ctx, base) = temp_ctx("unsupported");
        let target = UnsupportedTarget;

        // Then it reports no skill support and emits a "not supported" note,
        // writing no files and never panicking.
        assert!(!target.supports_skills(Location::Global));
        assert!(target.skill_dir(&ctx, Location::Global).is_none());

        let install = target.install_skill(&ctx, Location::Global, false);
        assert!(install.files.is_empty());
        assert_eq!(install.notes.len(), 1);
        assert!(
            install.notes[0]
                .contains("skills not supported by Dummy Unsupported for --location=global")
        );

        let uninstall = target.uninstall_skill(&ctx, Location::Global);
        assert!(uninstall.files.is_empty());
        assert!(
            uninstall.notes[0]
                .contains("skills not supported by Dummy Unsupported for --location=global")
        );

        let status = target.skill_status(&ctx, Location::Global);
        assert!(status.is_unsupported());
        assert_eq!(status.label(), "not supported");

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn supporting_target_gets_full_lifecycle_for_free() {
        // Given a target overriding only supports_skills + skill_dir
        let (ctx, base) = temp_ctx("supporting");
        let skills_parent = base.join("skills");
        fs::create_dir_all(&skills_parent).unwrap();
        let target = SupportingTarget {
            skills_parent: skills_parent.clone(),
        };

        // When installing for the first time → Created (via default delegation).
        let r1 = target.install_skill(&ctx, Location::Global, false);
        assert!(
            r1.files
                .iter()
                .any(|f| f.action == FileAction::Created && f.path.ends_with("SKILL.md"))
        );
        assert_eq!(
            target.skill_status(&ctx, Location::Global).status,
            Some(skill::SkillStatus::UpToDate)
        );

        // When re-installing identical content → Unchanged.
        let r2 = target.install_skill(&ctx, Location::Global, false);
        assert_eq!(r2.files.len(), 1);
        assert_eq!(r2.files[0].action, FileAction::Unchanged);

        // When the user edits the skill → install is Skipped (LocallyModified).
        let skill_md = skills_parent
            .join(skill::SKILL_DIR_NAME)
            .join(skill::SKILL_FILE_NAME);
        fs::write(&skill_md, "user edited\n").unwrap();
        let r3 = target.install_skill(&ctx, Location::Global, false);
        assert_eq!(r3.files.len(), 1);
        assert_eq!(r3.files[0].action, FileAction::Skipped);
        assert!(!r3.notes.is_empty());
        assert_eq!(
            target.skill_status(&ctx, Location::Global).status,
            Some(skill::SkillStatus::LocallyModified)
        );

        // When forcing → Updated, embedded content restored.
        let r4 = target.install_skill(&ctx, Location::Global, true);
        assert!(
            r4.files
                .iter()
                .any(|f| f.action == FileAction::Updated && f.path.ends_with("SKILL.md"))
        );
        assert_eq!(fs::read_to_string(&skill_md).unwrap(), skill::SKILL_MD);

        // When uninstalling → files removed.
        let r5 = target.uninstall_skill(&ctx, Location::Global);
        assert!(r5.files.iter().any(|f| f.action == FileAction::Removed));
        assert_eq!(
            target.skill_status(&ctx, Location::Global).status,
            Some(skill::SkillStatus::NotInstalled)
        );

        let _ = fs::remove_dir_all(base);
    }
}
