//! Opening the user's browser at the viewer URL — upstream
//! `src/ui-server/open-browser.ts`. Best effort: the URL is already printed,
//! which is the part that matters.

use std::process::{Command, Stdio};

/// Names the program that opens the URL; `none`, `0`, `false`, `off` or empty
/// suppress opening.
pub const BROWSER_ENV: &str = "CODEGRAPH_BROWSER";

const SUPPRESS_VALUES: &[&str] = &["", "none", "0", "false", "off"];

/// The command that would open `url` on `platform` (`"macos"`, `"windows"`,
/// anything else), or `None` when opening is suppressed.
pub fn browser_open_command(
    url: &str,
    platform: &str,
    browser_override: Option<&str>,
) -> Option<(String, Vec<String>)> {
    if let Some(raw) = browser_override {
        let trimmed = raw.trim();
        if SUPPRESS_VALUES.contains(&trimmed.to_lowercase().as_str()) {
            return None;
        }
        // Windows: through `cmd /c`, so a `.cmd` / `.bat` browser shim works too.
        if platform == "windows" {
            return Some(("cmd".into(), vec!["/c".into(), trimmed.into(), url.into()]));
        }
        return Some((trimmed.into(), vec![url.into()]));
    }
    match platform {
        "macos" => Some(("open".into(), vec![url.into()])),
        // `start` is a cmd builtin; the empty string is the window title.
        "windows" => Some((
            "cmd".into(),
            vec!["/c".into(), "start".into(), String::new(), url.into()],
        )),
        _ => Some(("xdg-open".into(), vec![url.into()])),
    }
}

/// Open `url` in the default browser. Never fails the caller and never keeps
/// it waiting: the opener runs detached with its output discarded. `true` when
/// a launch was attempted.
pub fn open_browser(url: &str) -> bool {
    let browser_override = std::env::var(BROWSER_ENV).ok();
    let Some((command, args)) =
        browser_open_command(url, std::env::consts::OS, browser_override.as_deref())
    else {
        return false;
    };
    let spawned = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => {
            // Reap it off the caller's thread so it never lingers as a zombie.
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_has_its_opener() {
        let url = "http://127.0.0.1:4747/";
        assert_eq!(
            browser_open_command(url, "macos", None),
            Some(("open".into(), vec![url.into()]))
        );
        assert_eq!(
            browser_open_command(url, "linux", None),
            Some(("xdg-open".into(), vec![url.into()]))
        );
        assert_eq!(
            browser_open_command(url, "windows", None),
            Some((
                "cmd".into(),
                vec!["/c".into(), "start".into(), String::new(), url.into()]
            ))
        );
    }

    #[test]
    fn the_override_names_the_browser_or_suppresses_it() {
        let url = "http://127.0.0.1:4747/";
        for off in ["", " none ", "0", "FALSE", "off"] {
            assert_eq!(
                browser_open_command(url, "linux", Some(off)),
                None,
                "{off:?}"
            );
        }
        assert_eq!(
            browser_open_command(url, "linux", Some(" firefox ")),
            Some(("firefox".into(), vec![url.into()]))
        );
        assert_eq!(
            browser_open_command(url, "windows", Some("my browser.cmd")),
            Some((
                "cmd".into(),
                vec!["/c".into(), "my browser.cmd".into(), url.into()]
            ))
        );
    }
}
