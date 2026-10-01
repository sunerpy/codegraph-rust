//! A file the query NAMED may use the budget the rest of the response left
//! unspent (upstream v1.6.1 #2068), and only that budget.
//!
//! Upstream's express case: "response.js res.send res.json res.render …"
//! admits one file, and the per-file valve cut the named `send` body at 55 of
//! 97 lines in a response far under its budget. This port's valve is the
//! per-file ceiling, `1.5 × max_chars_per_file`.
//!
//! 1. **Far from spent.** One file, five named functions whose bodies together
//!    overrun the per-file ceiling but not the response budget: every body comes
//!    back.
//! 2. **Shared.** The named file again, plus a lower file that renders whole:
//!    the named file still grows past its ceiling, out of what the lower file
//!    left, and the lower file keeps its whole section.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-named-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn run_in(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph binary");
    assert!(
        output.status.success(),
        "{args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const NAMED: [&str; 5] = [
    "sendBody",
    "sendJson",
    "renderView",
    "redirectTo",
    "sendFileStream",
];
const RESPONSE: &str = "lib/response.ts";

/// Five independent named functions, then unrelated header helpers, so the
/// file is far too big to come back whole.
fn response_source() -> String {
    let mut lines = vec![
        "export interface Res { headers: Record<string, string>; body: string[]; status: number }"
            .to_string(),
        String::new(),
    ];
    for name in NAMED {
        lines.push(format!(
            "export function {name}(res: Res, payload: unknown): Res {{"
        ));
        // 28 statements rather than upstream's 33: this port numbers every line
        // and pays its section frame inside the same 13K budget, so five
        // 36-line bodies no longer fit it at all.
        for i in 0..28 {
            lines.push(format!(
                "  res.body.push(String(payload ?? '').slice({i}, {}) + '{name}:{i}');",
                i + 7
            ));
        }
        lines.extend(["  return res;".to_string(), "}".to_string(), String::new()]);
    }
    for i in 0..60 {
        lines.push(format!(
            "export function headerSlot{i}(res: Res, value: string): Res {{"
        ));
        for k in 0..6 {
            lines.push(format!(
                "  res.headers['x-slot-{i}-{k}'] = value.slice({k}, {});",
                k + 3
            ));
        }
        lines.extend(["  return res;".to_string(), "}".to_string(), String::new()]);
    }
    lines.join("\n")
}

/// A small file that calls one of the named functions, so it ranks into the
/// response and renders whole.
fn handler_source() -> String {
    let mut lines = vec![
        "import { sendBody, Res } from '../lib/response';".to_string(),
        String::new(),
        "export function handleUpload(res: Res, chunks: string[]): Res {".to_string(),
    ];
    for i in 0..12 {
        lines.push(format!("  res = sendBody(res, chunks[{i}]);"));
    }
    lines.extend(["  return res;".to_string(), "}".to_string()]);
    lines.join("\n") + "\n"
}

fn indexed(label: &str, files: &[(&str, String)]) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let project = dir.path.join("app");
    for (rel, body) in files {
        let path = project.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    run_in(&dir.path, &["init", project.to_str().unwrap()]);
    (dir, project)
}

/// Line numbers rendered in `file`'s section.
fn rendered_lines(text: &str, file: &str) -> BTreeSet<usize> {
    let header = format!("#### {file} ");
    let Some(start) = text.find(&header) else {
        return BTreeSet::new();
    };
    let rest = &text[start + header.len()..];
    let section = &rest[..rest.find("\n#### ").unwrap_or(rest.len())];
    section
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter_map(|(number, _)| number.parse().ok())
        .collect()
}

/// 1-based `(first, last)` line of each named function in `source`.
fn body_spans(source: &str) -> Vec<(&'static str, usize, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    NAMED
        .iter()
        .map(|name| {
            let start = lines
                .iter()
                .position(|line| line.starts_with(&format!("export function {name}(")))
                .unwrap()
                + 1;
            let end = start + lines[start..].iter().position(|line| *line == "}").unwrap() + 1;
            (*name, start, end)
        })
        .collect()
}

fn incomplete_bodies(text: &str, source: &str) -> Vec<&'static str> {
    let rendered = rendered_lines(text, RESPONSE);
    body_spans(source)
        .into_iter()
        .filter(|(_, start, end)| (*start..=*end).any(|line| !rendered.contains(&line)))
        .map(|(name, _, _)| name)
        .collect()
}

#[test]
fn a_named_file_far_from_its_budget_returns_every_named_body() {
    let source = response_source();
    let (_dir, project) = indexed("far", &[(RESPONSE, source.clone())]);
    let text = run_in(&project, &["explore", &NAMED.join(" ")]);
    // Fixture shape: the bodies overrun the 5,700-char per-file ceiling.
    let bodies: usize = body_spans(&source)
        .iter()
        .map(|(_, start, end)| {
            source.lines().collect::<Vec<_>>()[start - 1..*end]
                .join("\n")
                .len()
        })
        .sum();
    assert!(bodies > 5700, "fixture too small: {bodies}");
    assert_eq!(
        incomplete_bodies(&text, &source),
        Vec::<&str>::new(),
        "{text}"
    );
    assert!(
        text.len() <= 13000,
        "response {} past its budget",
        text.len()
    );
}

#[test]
fn a_named_file_takes_only_what_the_other_files_left() {
    let source = response_source();
    let handler = handler_source();
    let (_dir, project) = indexed(
        "shared",
        &[
            (RESPONSE, source.clone()),
            ("routes/upload.ts", handler.clone()),
        ],
    );
    let text = run_in(
        &project,
        &["explore", &format!("{} handleUpload", NAMED.join(" "))],
    );
    let lower = rendered_lines(&text, "routes/upload.ts");
    let want: BTreeSet<usize> = (1..=handler.lines().count()).collect();
    assert_eq!(
        lower, want,
        "the lower file must keep its whole section:\n{text}"
    );
    let named_source: usize = text
        .split("#### ")
        .find(|section| section.starts_with(&format!("{RESPONSE} ")))
        .map_or(0, str::len);
    assert!(
        named_source > 5700,
        "the named file did not grow past its per-file ceiling ({named_source}):\n{text}"
    );
    assert!(
        text.len() <= 13000,
        "response {} past its budget",
        text.len()
    );
}
