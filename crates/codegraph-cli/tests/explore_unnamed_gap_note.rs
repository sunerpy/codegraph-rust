//! The trim note never claims a gap marker named what it could not afford to
//! (#1711, #2077).
//!
//! A gap between two rendered windows names the indexed symbols it hides, but
//! only from the budget the file has spare; a name that costs more stays out
//! and the marker is a bare `... (gap) ...`. The note closing the response
//! then said the gap markers named what was elided. Here an index-only helper
//! whose name no budget affords sits between the two windows.

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
            "codegraph-cli-unnamed-gap-{label}-{}-{}",
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

/// What follows the last source fence: the epilogue.
fn epilogue_of(text: &str) -> &str {
    text.rfind("```").map_or(text, |i| &text[i + 3..])
}

/// `src/big.ts`: `alphaEntry` at the top, an index-only helper whose name is
/// longer than any per-file budget, then `betaEntry`, whose body runs `beta_lines`
/// lines. `padding` small files lift the project into the large tiers.
fn project(label: &str, beta_lines: usize, padding: usize) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let root = dir.path.join("app");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let helper = format!("helper{}", "X".repeat(8000));
    let mut body: Vec<String> = (1..10).map(|i| format!("// header {i}")).collect();
    body.push("export function alphaEntry(): number {".to_string());
    body.extend((0..19).map(|i| format!("  const a{i} = {i};")));
    body.push("  return betaEntry(1);".to_string());
    body.push("}".to_string());
    while body.len() < 99 {
        body.push(format!("// filler {}", body.len() + 1));
    }
    body.push(format!("function {helper}(): number {{ return 1; }}"));
    while body.len() < 199 {
        body.push(format!("// filler {}", body.len() + 1));
    }
    body.push("export function betaEntry(v: number): number {".to_string());
    body.extend((0..beta_lines).map(|i| format!("  v = v + {i}; // beta stage {i}")));
    body.push("  return v;".to_string());
    body.push("}".to_string());
    while body.len() < 400 {
        body.push(format!("// filler {}", body.len() + 1));
    }
    std::fs::write(src.join("big.ts"), body.join("\n")).unwrap();
    if padding > 0 {
        let pad = root.join("pad");
        std::fs::create_dir_all(&pad).unwrap();
        for i in 0..padding {
            std::fs::write(
                pad.join(format!("pad{i}.ts")),
                format!("export const pad{i} = {i};\n"),
            )
            .unwrap();
        }
    }
    run_in(&dir.path, &["init", root.to_str().unwrap()]);
    (dir, root)
}

#[test]
fn small_tier_note_does_not_claim_an_unaffordable_gap_name() {
    let (_dir, root) = project("small", 19, 0);
    let text = run_in(&root, &["explore", "alphaEntry betaEntry"]);
    // The fixture does what it is for: both windows render around a bare gap.
    assert!(text.contains("export function alphaEntry"), "{text}");
    assert!(text.contains("export function betaEntry"), "{text}");
    assert!(text.contains("... (gap) ..."), "{text}");
    assert!(!text.contains("XXXXXXXX"), "{text}");
    let epilogue = epilogue_of(&text);
    assert!(
        epilogue.contains("Some file sections were trimmed for size"),
        "{epilogue}"
    );
    assert!(
        !epilogue.contains("Elided symbols are named inside gap markers"),
        "{epilogue}"
    );
    assert!(epilogue.contains("only where room allowed"), "{epilogue}");
}

#[test]
fn large_tier_note_does_not_claim_an_unaffordable_gap_name() {
    let (_dir, root) = project("large", 900, 520);
    let text = run_in(&root, &["explore", "alphaEntry betaEntry"]);
    assert!(text.contains("export function alphaEntry"), "{text}");
    assert!(text.contains("... (gap) ..."), "{text}");
    assert!(!text.contains("XXXXXXXX"), "{text}");
    let epilogue = epilogue_of(&text);
    // `betaEntry` is windowed, so the section is reported trimmed.
    assert!(epilogue.contains("Verbatim source for"), "{epilogue}");
    assert!(!epilogue.contains("name what was elided"), "{epilogue}");
    assert!(epilogue.contains("name what room allowed"), "{epilogue}");
}
