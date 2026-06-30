//! Trae IDE target. Ports the cross-OS VS Code-fork MCP install pattern.
//!
// STUB: filled by T2/T3 — this is the minimal compiling skeleton T1 registers
// so the crate builds; T2 replaces it with the full server/desktop layout probe
// (`~/.trae-server/data/Machine/mcp.json` else desktop `Trae/User/mcp.json`),
// `${workspaceFolder}` global / absolute-`--path` local, sibling-preserving
// upsert, and skill install. Do NOT rely on this stub's behavior.

use super::super::types::{
    AgentTarget, DetectionResult, InstallContext, InstallOptions, Location, TargetId, WriteResult,
};

pub struct TraeTarget;

impl AgentTarget for TraeTarget {
    fn id(&self) -> TargetId {
        TargetId::Trae
    }
    fn display_name(&self) -> &'static str {
        "Trae"
    }
    fn supports_location(&self, _loc: Location) -> bool {
        true
    }
    fn detect(&self, _ctx: &InstallContext, _loc: Location) -> DetectionResult {
        DetectionResult::default()
    }
    fn install(&self, _ctx: &InstallContext, _loc: Location, _opts: InstallOptions) -> WriteResult {
        WriteResult::default()
    }
    fn uninstall(&self, _ctx: &InstallContext, _loc: Location) -> WriteResult {
        WriteResult::default()
    }
    fn print_config(&self, _ctx: &InstallContext, _loc: Location) -> String {
        String::new()
    }
}

pub static TRAE_TARGET: TraeTarget = TraeTarget;
