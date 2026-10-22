//! Plain output under `NO_COLOR` and pipes, case for case with upstream's
//! `__tests__/cli-no-color.test.ts` (`d6efd43`, #1306 for #1281).
//!
//! Upstream colors its list commands and animates `init` / `index` / `sync` on
//! a terminal, and #1306 made one switch turn both off when stdout is piped or
//! `NO_COLOR` is set. This port prints no color of its own and draws progress
//! only on a terminal stderr, so piped output is plain whatever the environment
//! says. clap's help and errors are the one colored surface: they color for a
//! color terminal or `CLICOLOR_FORCE`, and `NO_COLOR` wins over both.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const ANSI: &[u8] = b"\x1b[";

/// The color signals a run could inherit, removed so each case is explicit.
const COLOR_ENV: &[&str] = &[
    "NO_COLOR",
    "FORCE_COLOR",
    "CI",
    "CLICOLOR",
    "CLICOLOR_FORCE",
];

struct Project {
    root: PathBuf,
    registry: PathBuf,
}

/// Next project directory suffix, unique within this test process.
static NEXT_PROJECT: AtomicU64 = AtomicU64::new(0);

/// A fresh directory name for one project. The clock alone repeats when two
/// tests start within one tick, and the first test's `Drop` then deleted the
/// other's project mid-run; the process-wide sequence keeps every name distinct.
fn project_base(nanos: u128) -> PathBuf {
    std::env::temp_dir().join(format!(
        "codegraph-plain-output-{}-{}-{nanos}",
        std::process::id(),
        NEXT_PROJECT.fetch_add(1, Ordering::Relaxed)
    ))
}

impl Project {
    fn indexed() -> Self {
        let base = project_base(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        // `create_dir` fails on an existing directory, so two projects can
        // never share one.
        std::fs::create_dir(&base).expect("create a fresh project directory");
        let root = base.join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("a.ts"),
            "export function alpha(): number { return beta(); }\n\
             export function beta(): number { return 1; }\n",
        )
        .unwrap();
        let project = Self {
            registry: base.join("registry"),
            root,
        };
        let init = project.run(&["init", "."], &[]);
        assert!(init.status.success(), "{init:?}");
        project
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_codegraph"));
        command
            .args(args)
            .current_dir(&self.root)
            .env("CODEGRAPH_NO_DAEMON", "1")
            .env("CODEGRAPH_NO_WATCH", "1")
            .env("CODEGRAPH_HTTP_REGISTRY_DIR", self.registry.join("http"))
            .env("CODEGRAPH_MCP_REGISTRY_DIR", self.registry.join("mcp"));
        for name in COLOR_ENV {
            command.env_remove(name);
        }
        command.envs(env.iter().copied());
        command.output().expect("run codegraph")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

#[test]
fn projects_created_within_one_clock_tick_get_distinct_directories() {
    assert_ne!(project_base(7), project_base(7));
}

fn has(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn plain(output: &std::process::Output) -> bool {
    !has(&output.stdout, ANSI) && !has(&output.stderr, ANSI)
}

fn show(output: &std::process::Output) -> String {
    format!(
        "exit {:?}\nstdout {:?}\nstderr {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const LIST_COMMANDS: &[&[&str]] = &[
    &["status"],
    &["query", "alpha"],
    &["callers", "beta"],
    &["callees", "alpha"],
    &["impact", "beta"],
    &["files"],
];

#[test]
fn piped_list_commands_carry_no_ansi_codes_whatever_the_environment_says() {
    let project = Project::indexed();
    let environments: &[&[(&str, &str)]] = &[
        &[],
        &[("NO_COLOR", "1"), ("FORCE_COLOR", "1")],
        &[("FORCE_COLOR", "1")],
        &[("CI", "1")],
        &[("CLICOLOR_FORCE", "1")],
    ];
    for env in environments {
        for args in LIST_COMMANDS {
            let output = project.run(args, env);
            assert!(
                output.status.success(),
                "{args:?} {env:?}\n{}",
                show(&output)
            );
            assert!(plain(&output), "{args:?} {env:?}\n{}", show(&output));
        }
    }
}

#[test]
fn piped_index_and_sync_print_plain_lines_with_no_carriage_return_rewrites() {
    let project = Project::indexed();
    for args in [&["index", "--force"][..], &["sync"][..]] {
        let output = project.run(args, &[("FORCE_COLOR", "1")]);
        assert!(output.status.success(), "{args:?}\n{}", show(&output));
        assert!(plain(&output), "{args:?}\n{}", show(&output));
        assert!(
            !has(&output.stdout, b"\r") && !has(&output.stderr, b"\r"),
            "{args:?}\n{}",
            show(&output)
        );
    }
}

#[test]
fn no_color_wins_over_clicolor_force_for_help_and_errors() {
    let project = Project::indexed();
    let help = |env: &[(&str, &str)]| project.run(&["status", "--help"], env);
    let error = |env: &[(&str, &str)]| project.run(&["status", "--bogus"], env);
    // The one surface that colors: clap, when told to even through a pipe.
    assert!(has(&help(&[("CLICOLOR_FORCE", "1")]).stdout, ANSI));
    assert!(has(&error(&[("CLICOLOR_FORCE", "1")]).stderr, ANSI));
    let both = [("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")];
    assert!(plain(&help(&both)), "{}", show(&help(&both)));
    let refused = error(&both);
    assert_eq!(refused.status.code(), Some(2), "{}", show(&refused));
    assert!(plain(&refused), "{}", show(&refused));
    // Piped with no signal at all, it is plain too.
    assert!(plain(&help(&[])));
    assert!(plain(&error(&[])));
}
