//! Registry of all known agent targets.
//! Ports `upstream installer/targets/registry.ts`.
//!
//! Order is load-bearing: it is the order in `--target=all`, in detection, and
//! in `--print-config`'s help listing. Matches `ALL_TARGETS` (registry.ts:20):
//! claude, cursor, codex, opencode, hermes, gemini, antigravity, kiro, trae,
//! qoder.

use anyhow::{Result, bail};

use super::targets::{
    antigravity::ANTIGRAVITY_TARGET, claude::CLAUDE_TARGET, codex::CODEX_TARGET,
    cursor::CURSOR_TARGET, gemini::GEMINI_TARGET, hermes::HERMES_TARGET, kiro::KIRO_TARGET,
    opencode::OPENCODE_TARGET, qoder::QODER_TARGET, trae::TRAE_TARGET, zed::ZED_TARGET,
};
use super::types::{AgentTarget, DetectionResult, InstallContext, Location};

pub fn all_targets() -> Vec<&'static dyn AgentTarget> {
    vec![
        &CLAUDE_TARGET,
        &CURSOR_TARGET,
        &CODEX_TARGET,
        &OPENCODE_TARGET,
        &HERMES_TARGET,
        &GEMINI_TARGET,
        &ANTIGRAVITY_TARGET,
        &KIRO_TARGET,
        &TRAE_TARGET,
        &QODER_TARGET,
        &ZED_TARGET,
    ]
}

pub fn get_target(id: &str) -> Option<&'static dyn AgentTarget> {
    all_targets().into_iter().find(|t| t.id().as_str() == id)
}

pub fn list_target_ids() -> Vec<&'static str> {
    all_targets().into_iter().map(|t| t.id().as_str()).collect()
}

/// Ports detectAll (registry.ts:45).
pub fn detect_all(
    ctx: &InstallContext,
    loc: Location,
) -> Vec<(&'static dyn AgentTarget, DetectionResult)> {
    all_targets()
        .into_iter()
        .map(|t| {
            let detection = t.detect(ctx, loc);
            (t, detection)
        })
        .collect()
}

/// Resolve a `--target=` flag value. Ports resolveTargetFlag (registry.ts:66):
/// `auto` | `all` | `none` | csv. `auto` falls back to claude when none detected.
pub fn resolve_target_flag(
    ctx: &InstallContext,
    value: &str,
    loc: Location,
) -> Result<Vec<&'static dyn AgentTarget>> {
    if value == "none" {
        return Ok(Vec::new());
    }
    if value == "all" {
        return Ok(all_targets());
    }
    if value == "auto" {
        let detected: Vec<_> = detect_all(ctx, loc)
            .into_iter()
            .filter(|(_, d)| d.installed)
            .map(|(t, _)| t)
            .collect();
        if !detected.is_empty() {
            return Ok(detected);
        }
        return Ok(get_target("claude").into_iter().collect());
    }

    let ids: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let mut resolved = Vec::new();
    let mut unknown = Vec::new();
    for id in ids {
        match get_target(id) {
            Some(t) => resolved.push(t),
            None => unknown.push(id.to_string()),
        }
    }
    if !unknown.is_empty() {
        let known = list_target_ids().join(", ");
        bail!(
            "Unknown --target id(s): {}. Known: {known}, plus 'auto' / 'all' / 'none'.",
            unknown.join(", ")
        );
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full registry id set, in load-bearing order (registry.rs:5-7).
    const ALL_IDS: &[&str] = &[
        "claude",
        "cursor",
        "codex",
        "opencode",
        "hermes",
        "gemini",
        "antigravity",
        "kiro",
        "trae",
        "qoder",
        "zed",
    ];

    #[test]
    fn list_target_ids_matches_expected_order_and_count() {
        assert_eq!(list_target_ids(), ALL_IDS);
        assert_eq!(all_targets().len(), ALL_IDS.len());
    }

    #[test]
    fn every_known_id_resolves_via_get_target() {
        for id in ALL_IDS {
            let target = get_target(id);
            assert!(target.is_some(), "get_target({id:?}) should resolve");
            assert_eq!(target.unwrap().id().as_str(), *id);
        }
    }

    #[test]
    fn rebranded_lingma_id_does_not_resolve() {
        // Lingma was folded into Qoder; the stale id must not resolve.
        assert!(get_target("lingma").is_none());
    }

    fn temp_ctx() -> InstallContext {
        let base = std::env::temp_dir().join(format!(
            "cg-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        InstallContext {
            home: base.join("home"),
            cwd: base.join("cwd"),
            app_data: None,
            xdg_config_home: None,
            hermes_home: None,
        }
    }

    #[test]
    fn resolve_none_yields_empty() {
        let ctx = temp_ctx();
        let resolved = resolve_target_flag(&ctx, "none", Location::Global).unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_all_yields_full_registry() {
        let ctx = temp_ctx();
        let resolved = resolve_target_flag(&ctx, "all", Location::Global).unwrap();
        assert_eq!(resolved.len(), ALL_IDS.len());
    }

    #[test]
    fn resolve_auto_falls_back_to_claude_when_none_detected() {
        let ctx = temp_ctx();
        let resolved = resolve_target_flag(&ctx, "auto", Location::Global).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id().as_str(), "claude");
    }

    #[test]
    fn resolve_csv_list_resolves_each() {
        let ctx = temp_ctx();
        let resolved =
            resolve_target_flag(&ctx, "claude, cursor ,codex", Location::Global).unwrap();
        let ids: Vec<&str> = resolved.iter().map(|t| t.id().as_str()).collect();
        assert_eq!(ids, vec!["claude", "cursor", "codex"]);
    }

    #[test]
    fn resolve_csv_with_unknown_id_errors() {
        let ctx = temp_ctx();
        let result = resolve_target_flag(&ctx, "claude,bogus", Location::Global);
        let err = match result {
            Ok(_) => panic!("unknown id must error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("Unknown --target id(s): bogus"));
    }

    #[test]
    fn detect_all_returns_one_entry_per_target() {
        let ctx = temp_ctx();
        let detections = detect_all(&ctx, Location::Global);
        assert_eq!(detections.len(), ALL_IDS.len());
        assert!(detections.iter().all(|(_, d)| !d.installed));
    }
}
