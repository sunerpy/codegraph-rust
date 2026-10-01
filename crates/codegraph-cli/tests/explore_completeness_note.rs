//! The completeness note at the end of a `codegraph explore` response claims
//! "complete" only for sections that are (upstream v1.6.1 #2077).
//!
//! On the tiers with a completeness signal (>= 500 indexed files) every
//! response ended with "Complete source for N files is included above — do NOT
//! re-read them", whatever the render had cut, and the same line said "Reserve
//! Read for a single specific line range", which explore output must never say.
//! The fixture has a function too long for any budget, so its section is
//! windowed, beside a small flow that renders whole.

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
            "codegraph-cli-complete-{label}-{}-{}",
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

/// Anything that offers Read as a way forward. "treat it as already Read" is
/// the guarantee and "do NOT Read" a prohibition; neither offers it.
fn offers_read(text: &str) -> bool {
    ["Reserve Read", "use Read", "Read for ", "fall back to Read"]
        .iter()
        .any(|offer| text.contains(offer))
}

/// What follows the last source fence: the epilogue.
fn epilogue_of(text: &str) -> &str {
    text.rfind("```").map_or(text, |i| &text[i + 3..])
}

/// A project whose `runPipeline` is far past any tier's budget, so its section
/// is always windowed, beside `formatValue → padValue`, which renders whole.
/// `padding` small files lift it into the large tiers.
fn project(label: &str, padding: usize) -> (TestDir, PathBuf) {
    let dir = TestDir::new(label);
    let root = dir.path.join("app");
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut body = vec![
        "import { stepNext } from './step';".to_string(),
        String::new(),
        "export function runPipeline(input: number): number {".to_string(),
        "  let acc = input;".to_string(),
    ];
    for i in 0..900 {
        if i == 450 {
            body.push("  acc = stepNext(acc);".to_string());
        }
        body.push(format!("  acc = acc + {i}; // pipeline stage {i}"));
    }
    body.extend([
        "  const PIPELINE_TAIL_MARKER = acc;".to_string(),
        "  return PIPELINE_TAIL_MARKER;".to_string(),
        "}".to_string(),
        String::new(),
    ]);
    std::fs::write(src.join("pipeline.ts"), body.join("\n")).unwrap();
    std::fs::write(
        src.join("step.ts"),
        "import { finalizeStep } from './finalize';\n\nexport function stepNext(v: number): number {\n  return finalizeStep(v * 2);\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("finalize.ts"),
        "export function finalizeStep(v: number): number {\n  return v + 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("format.ts"),
        "import { padValue } from './pad';\n\nexport function formatValue(v: number): string {\n  return padValue(String(v));\n}\n",
    )
    .unwrap();
    std::fs::write(
        src.join("pad.ts"),
        "export function padValue(s: string): string {\n  return s.padStart(8, ' ');\n}\n",
    )
    .unwrap();
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
fn large_tier_a_windowed_section_is_reported_trimmed_not_complete() {
    let (_dir, root) = project("large-trim", 520);
    let text = run_in(&root, &["explore", "runPipeline stepNext finalizeStep"]);
    // The fixture does what it is for: the function is windowed.
    assert!(text.contains("#### src/pipeline.ts "), "{text}");
    assert!(!text.contains("PIPELINE_TAIL_MARKER"), "{text}");

    assert!(!text.contains("Complete source for"), "{text}");
    assert!(text.contains("Verbatim source for"), "{text}");
    assert!(text.contains("treat it as already Read"), "{text}");
    assert!(
        text.contains("Trimmed for size: `pipeline.ts`")
            || text.contains("Some sections were trimmed for size"),
        "{text}"
    );
    assert!(!offers_read(epilogue_of(&text)), "{}", epilogue_of(&text));
}

#[test]
fn large_tier_complete_sections_are_still_called_complete_without_a_read_escape() {
    let (_dir, root) = project("large-complete", 520);
    let text = run_in(&root, &["explore", "formatValue padValue"]);
    assert!(text.contains("#### src/format.ts "), "{text}");
    assert!(text.contains("#### src/pad.ts "), "{text}");
    assert!(
        text.contains("Complete source for 2 files is included above"),
        "{text}"
    );
    assert!(!text.contains("Verbatim source for"), "{text}");
    assert!(!offers_read(epilogue_of(&text)), "{}", epilogue_of(&text));
}

#[test]
fn small_tier_the_same_cut_gets_the_trimmed_note_and_a_complete_answer_none() {
    let (_dir, root) = project("small", 0);
    let trimmed = run_in(&root, &["explore", "runPipeline stepNext finalizeStep"]);
    assert!(!trimmed.contains("PIPELINE_TAIL_MARKER"), "{trimmed}");
    assert!(
        trimmed.contains("Some file sections were trimmed for size"),
        "{trimmed}"
    );
    let complete = run_in(&root, &["explore", "formatValue padValue"]);
    assert!(!complete.contains("trimmed for size"), "{complete}");
}
