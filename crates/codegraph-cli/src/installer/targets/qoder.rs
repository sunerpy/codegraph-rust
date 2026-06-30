//! Qoder IDE target (the rebranded 通义灵码/Lingma). Ports the SharedClientCache
//! MCP install pattern.
//!
// STUB: filled by T2/T3 — this is the minimal compiling skeleton T1 registers
// so the crate builds; T3 replaces it with the server-mode probe
// (`~/.qoder-cn-server/data/Machine/mcp.json`) else the dynamic
// `<base>/{QoderCN|Qoder}/<machineId>/SharedClientCache/mcp.json` discovery via
// `std::fs::read_dir` (deterministic sort), a bare `serve --mcp` global entry,
// absolute-`--path` local, and skills via `~/.agents/skills`. Do NOT rely on
// this stub's behavior.

use super::super::types::{
    AgentTarget, DetectionResult, InstallContext, InstallOptions, Location, TargetId, WriteResult,
};

pub struct QoderTarget;

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

pub static QODER_TARGET: QoderTarget = QoderTarget;
