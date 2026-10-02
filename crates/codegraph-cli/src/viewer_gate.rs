//! The browser viewer (`codegraph ui`, alias `web`) ships in the binary but is
//! not part of a release yet. Until it launches, both spellings — also as
//! `help ui` or `ui --help` — are refused before any startup work unless
//! `CODEGRAPH_UI=1` is set, which keeps it usable for the people testing it. A
//! port of upstream `src/bin/viewer-gate.ts`; `main` runs it before
//! `Cli::parse()`.

/// Setting this to `1` enables the viewer commands.
pub const VIEWER_ENV: &str = "CODEGRAPH_UI";

const VIEWER_COMMANDS: &[&str] = &["ui", "web"];
/// Program-level flags that take no value and may precede the command.
const PROGRAM_FLAGS: &[&str] = &["--color", "--no-color"];

pub fn viewer_enabled() -> bool {
    std::env::var(VIEWER_ENV).is_ok_and(|v| v == "1")
}

/// The viewer command these arguments ask for (`ui` or `web`), whether run
/// directly or through `help`, or `None` when they ask for something else.
pub fn requested_viewer_command(args: &[String]) -> Option<&str> {
    let positional: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !PROGRAM_FLAGS.contains(arg))
        .collect();
    let command = if positional.first() == Some(&"help") {
        positional.get(1)
    } else {
        positional.first()
    };
    command.copied().filter(|c| VIEWER_COMMANDS.contains(c))
}

/// The refusal upstream prints, word for word.
pub fn refusal(command: &str) -> String {
    format!(
        "error: 'codegraph {command}' is not in this release yet. The browser viewer is coming in an upcoming release."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_viewer_is_recognised_however_it_is_asked_for() {
        assert_eq!(requested_viewer_command(&args(&["ui"])), Some("ui"));
        assert_eq!(requested_viewer_command(&args(&["web", "."])), Some("web"));
        assert_eq!(requested_viewer_command(&args(&["help", "ui"])), Some("ui"));
        assert_eq!(
            requested_viewer_command(&args(&["ui", "--help"])),
            Some("ui")
        );
        assert_eq!(
            requested_viewer_command(&args(&["--no-color", "web"])),
            Some("web")
        );
    }

    #[test]
    fn everything_else_passes() {
        assert_eq!(requested_viewer_command(&args(&[])), None);
        assert_eq!(requested_viewer_command(&args(&["status"])), None);
        assert_eq!(requested_viewer_command(&args(&["help"])), None);
        assert_eq!(requested_viewer_command(&args(&["search", "ui"])), None);
        assert_eq!(requested_viewer_command(&args(&["--help", "ui"])), None);
    }
}
