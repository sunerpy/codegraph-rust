//! Agent target abstraction for the installer.
//!
//! Ports `upstream installer/targets/types.ts`. Each MCP-capable
//! agent implements `AgentTarget` so the orchestrator can write the right
//! MCP-server config + instructions + permissions without baking
//! client-specific paths into the dispatch. Adding a new agent = one new
//! module in `targets/` + one entry in `registry.rs`.

use std::path::PathBuf;

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
}
