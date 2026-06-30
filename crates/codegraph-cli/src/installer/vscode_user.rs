//! Cross-OS VS Code `User/` config-base helpers, shared by the VS Code-fork
//! install targets (Trae desktop, Qoder).
//!
//! The per-OS app-data root resolution is factored into a pure inner fn that
//! takes `os: &str` (NOT `cfg!`) so all three platforms are exercised by tests
//! on any single runner. The public wrappers pass `std::env::consts::OS`.
//!
//! NOTE: unlike `opencode.rs`, which SKIPS when `app_data`/`xdg` is empty, the
//! VS Code-fork layout has no alternate config dir — so here we SYNTHESIZE the
//! conventional fallback (`home/AppData/Roaming` on Windows, `home/.config` on
//! Linux) instead of skipping.

use std::path::{Path, PathBuf};

use super::types::InstallContext;

/// Resolve the per-OS app-data root that hosts a VS Code fork's `<App>/User/`
/// tree. `os` is the platform string (`std::env::consts::OS`: `"macos"`,
/// `"windows"`, or anything else → treated as Linux/Unix).
///
/// - macOS → `home/Library/Application Support`
/// - Windows → `app_data` (ignored when empty) else `home/AppData/Roaming`
/// - else (Linux/Unix) → `xdg` (ignored when empty) else `home/.config`
// `dead_code`: the T1 foundation helper; wired by the T2 Trae target.
#[allow(dead_code)]
pub fn config_base_for(home: &Path, app_data: Option<&Path>, xdg: Option<&Path>, os: &str) -> PathBuf {
    match os {
        "macos" => home.join("Library").join("Application Support"),
        "windows" => app_data
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("AppData").join("Roaming")),
        _ => xdg
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".config")),
    }
}

/// The VS Code DESKTOP `User/mcp.json` path for an app (e.g. Trae desktop):
/// `<config_base>/<app_name>/User/mcp.json`. Resolves the base via
/// [`config_base_for`] using the live platform.
// `dead_code`: the T1 foundation helper; wired by the T2 Trae target.
#[allow(dead_code)]
pub fn vscode_user_mcp_json(ctx: &InstallContext, app_name: &str) -> PathBuf {
    config_base_for(
        &ctx.home,
        ctx.app_data.as_deref(),
        ctx.xdg_config_home.as_deref(),
        std::env::consts::OS,
    )
    .join(app_name)
    .join("User")
    .join("mcp.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_base_for_resolves_per_os() {
        // Given a fixed home and explicit app_data / xdg overrides
        let home = PathBuf::from("/home/u");
        let app_data = PathBuf::from("/win/appdata");
        let xdg = PathBuf::from("/xdg/config");

        // When resolving the base for each OS literal, Then each yields its
        // platform-specific app-data root.
        assert_eq!(
            config_base_for(&home, Some(&app_data), Some(&xdg), "macos"),
            PathBuf::from("/home/u/Library/Application Support"),
        );
        assert_eq!(
            config_base_for(&home, Some(&app_data), Some(&xdg), "windows"),
            PathBuf::from("/win/appdata"),
        );
        assert_eq!(
            config_base_for(&home, Some(&app_data), Some(&xdg), "linux"),
            PathBuf::from("/xdg/config"),
        );
    }

    #[test]
    fn config_base_for_windows_none_falls_back_to_appdata_roaming() {
        // Given Windows with NO app_data
        let home = PathBuf::from("/home/u");

        // When resolving, Then it synthesizes home/AppData/Roaming (NOT a skip).
        assert_eq!(
            config_base_for(&home, None, None, "windows"),
            PathBuf::from("/home/u/AppData/Roaming"),
        );
        // And an empty app_data is treated identically to None.
        let empty = PathBuf::from("");
        assert_eq!(
            config_base_for(&home, Some(&empty), None, "windows"),
            PathBuf::from("/home/u/AppData/Roaming"),
        );
    }

    #[test]
    fn config_base_for_linux_none_falls_back_to_dot_config() {
        // Given Linux with NO xdg, Then it synthesizes home/.config.
        let home = PathBuf::from("/home/u");
        assert_eq!(
            config_base_for(&home, None, None, "linux"),
            PathBuf::from("/home/u/.config"),
        );
    }
}
