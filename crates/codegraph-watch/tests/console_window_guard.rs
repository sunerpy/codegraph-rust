//! Static source guard (all platforms): every process the daemon can start
//! keeps its console window hidden on Windows (upstream #2280).
//!
//! The daemon runs detached, with no console of its own. On Windows, a console
//! program it starts without `CREATE_NO_WINDOW` gets a new visible console, so a
//! window flashes once per call: on daemon start and on every auto-sync. The
//! flag is Windows-only and `std::process::Command` cannot report it back, so,
//! like upstream's AST guard, this test reads the source: every `Command::new(`
//! in the crates the daemon runs must reach `hide_console_window(` or
//! `creation_flags(` in the same statement, or carry a
//! `console-window-guard:` comment saying why it needs neither.

use std::fs;
use std::path::{Path, PathBuf};

/// The crates whose code runs inside the daemon or the MCP server.
const GUARDED_CRATES: &[&str] = &[
    "codegraph-watch",
    "codegraph-daemon",
    "codegraph-mcp",
    "codegraph-ui",
    "codegraph-store",
    "codegraph-resolve",
    "codegraph-graph",
    "codegraph-extract",
];

/// How far past `Command::new(` the builder may set the flag.
const STATEMENT_LINES: usize = 12;

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Each `Command::new(` outside a `#[cfg(test)]` module that neither hides its
/// console nor says why not, as `path:line`.
fn unguarded_spawns(source: &str, label: &str) -> Vec<String> {
    let lines = source.lines().collect::<Vec<_>>();
    let test_module_start = lines
        .iter()
        .position(|line| line.trim() == "#[cfg(test)]")
        .unwrap_or(lines.len());
    let mut unguarded = Vec::new();
    for (index, line) in lines.iter().enumerate().take(test_module_start) {
        let code = line.split("//").next().unwrap_or_default();
        if !code.contains("Command::new(") {
            continue;
        }
        let justified = lines[index.saturating_sub(2)..index]
            .iter()
            .any(|above| above.contains("console-window-guard:"));
        let hidden = lines[index..(index + STATEMENT_LINES).min(lines.len())]
            .iter()
            .any(|below| {
                below.contains("hide_console_window(") || below.contains("creation_flags(")
            });
        if !justified && !hidden {
            unguarded.push(format!("{label}:{}", index + 1));
        }
    }
    unguarded
}

#[test]
fn every_daemon_reachable_spawn_hides_its_console_window() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ directory")
        .to_path_buf();
    let mut checked = 0;
    let mut unguarded = Vec::new();
    for name in GUARDED_CRATES {
        let mut sources = Vec::new();
        rust_sources(&crates.join(name).join("src"), &mut sources);
        sources.sort();
        for path in sources {
            let source = fs::read_to_string(&path).unwrap();
            checked += source.matches("Command::new(").count();
            let label = path.strip_prefix(&crates).unwrap().display().to_string();
            unguarded.extend(unguarded_spawns(&source, &label));
        }
    }
    assert!(checked > 0, "the guard found no process spawns to check");
    assert!(
        unguarded.is_empty(),
        "these spawns can flash a console window on Windows; call \
         `codegraph_watch::hide_console_window` on the command, or add a \
         `console-window-guard:` comment saying why not:\n{}",
        unguarded.join("\n")
    );
}

#[test]
fn the_guard_flags_a_spawn_without_the_flag() {
    let bare = "fn run() {\n    let mut command = Command::new(\"git\");\n    command.arg(\"status\");\n}\n";
    assert_eq!(unguarded_spawns(bare, "x.rs"), ["x.rs:2"]);
    let hidden = "fn run() {\n    let mut command = Command::new(\"git\");\n    hide_console_window(&mut command);\n}\n";
    assert!(unguarded_spawns(hidden, "x.rs").is_empty());
    let justified = "fn run() {\n    // console-window-guard: a test's own script.\n    let _ = Command::new(script);\n}\n";
    assert!(unguarded_spawns(justified, "x.rs").is_empty());
    let in_tests = "fn run() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let _ = Command::new(\"git\"); }\n}\n";
    assert!(unguarded_spawns(in_tests, "x.rs").is_empty());
}
