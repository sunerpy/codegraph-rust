//! A bare basename two directories share pins only the file defining what the
//! query names (upstream v1.6.1 #2071).
//!
//! vscode has two `editorOptions.ts`: the editor's option registry, holding
//! `clampedInt`, `cursorStyleToString` and `cursorStyleFromString`, and a small
//! workbench helper holding none of them. `editorOptions.ts clampedInt …`
//! pinned both, and the two pins split the pinned room, so naming the file made
//! the answer worse. The fixture is that shape: the registry is ~60 option
//! classes with the named functions in the middle, the helper a handful of
//! unrelated functions.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const REGISTRY: &str = "src/editor/common/config/editorOptions.ts";
const HELPER: &str = "src/workbench/common/editor/editorOptions.ts";
const NAMED: [&str; 3] = ["clampedInt", "cursorStyleToString", "cursorStyleFromString"];

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-basename-{label}-{}-{}",
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

fn option_class(lines: &mut Vec<String>, i: usize) {
    lines.extend([
        String::new(),
        format!("export class EditorOption{i} extends BaseEditorOption<number, string> {{"),
        format!("  private readonly allowed{i} = ['alpha{i}', 'beta{i}', 'gamma{i}'];"),
        String::new(),
        "  constructor() {".to_string(),
        format!("    super({i}, 'option{i}', 'alpha{i}');"),
        "  }".to_string(),
        String::new(),
        "  public validate(input: unknown): string {".to_string(),
        "    if (typeof input !== 'string') {".to_string(),
        "      return this.defaultValue;".to_string(),
        "    }".to_string(),
        format!("    return this.allowed{i}.includes(input) ? input : this.defaultValue;"),
        "  }".to_string(),
        "}".to_string(),
    ]);
}

fn registry_source() -> String {
    let mut lines: Vec<String> = [
        "export interface IConfigurationPropertySchema {",
        "  type?: string;",
        "  default?: unknown;",
        "  minimum?: number;",
        "  maximum?: number;",
        "}",
        "",
        "export abstract class BaseEditorOption<K extends number, V> {",
        "  constructor(public readonly id: K, public readonly name: string, public readonly defaultValue: V) {}",
        "  public abstract validate(input: unknown): V;",
        "}",
    ]
    .map(str::to_string)
    .to_vec();
    for i in 0..30 {
        option_class(&mut lines, i);
    }
    lines.extend(
        [
            "",
            "export function clampedInt<T>(value: unknown, defaultValue: T, minimum: number, maximum: number): number | T {",
            "  if (typeof value === 'undefined') {",
            "    return defaultValue;",
            "  }",
            "  let r = parseInt(String(value), 10);",
            "  if (isNaN(r)) {",
            "    return defaultValue;",
            "  }",
            "  r = Math.max(minimum, r);",
            "  r = Math.min(maximum, r);",
            "  return r | 0;",
            "}",
            "",
            "export class EditorIntOption<K extends number> extends BaseEditorOption<K, number> {",
            "  public static clampedInt<T>(value: unknown, defaultValue: T, minimum: number, maximum: number): number | T {",
            "    return clampedInt(value, defaultValue, minimum, maximum);",
            "  }",
            "",
            "  constructor(id: K, name: string, defaultValue: number, public readonly minimum: number, public readonly maximum: number, schema?: IConfigurationPropertySchema) {",
            "    if (typeof schema !== 'undefined') {",
            "      schema.type = 'integer';",
            "      schema.default = defaultValue;",
            "      schema.minimum = minimum;",
            "      schema.maximum = maximum;",
            "    }",
            "    super(id, name, defaultValue);",
            "  }",
            "",
            "  public validate(input: unknown): number {",
            "    return EditorIntOption.clampedInt(input, this.defaultValue, this.minimum, this.maximum);",
            "  }",
            "}",
            "",
            "export const enum TextEditorCursorStyle {",
            "  Line = 1,",
            "  Block = 2,",
            "  Underline = 3,",
            "  LineThin = 4,",
            "  BlockOutline = 5,",
            "  UnderlineThin = 6,",
            "}",
            "",
            "export function cursorStyleToString(cursorStyle: TextEditorCursorStyle): string {",
            "  switch (cursorStyle) {",
            "    case TextEditorCursorStyle.Line: return 'line';",
            "    case TextEditorCursorStyle.Block: return 'block';",
            "    case TextEditorCursorStyle.Underline: return 'underline';",
            "    case TextEditorCursorStyle.LineThin: return 'line-thin';",
            "    case TextEditorCursorStyle.BlockOutline: return 'block-outline';",
            "    case TextEditorCursorStyle.UnderlineThin: return 'underline-thin';",
            "  }",
            "}",
            "",
            "export function cursorStyleFromString(cursorStyle: string): TextEditorCursorStyle {",
            "  switch (cursorStyle) {",
            "    case 'line': return TextEditorCursorStyle.Line;",
            "    case 'block': return TextEditorCursorStyle.Block;",
            "    case 'underline': return TextEditorCursorStyle.Underline;",
            "    case 'line-thin': return TextEditorCursorStyle.LineThin;",
            "    case 'block-outline': return TextEditorCursorStyle.BlockOutline;",
            "    case 'underline-thin': return TextEditorCursorStyle.UnderlineThin;",
            "  }",
            "  return TextEditorCursorStyle.Line;",
            "}",
        ]
        .map(str::to_string),
    );
    for i in 30..60 {
        option_class(&mut lines, i);
    }
    lines.join("\n") + "\n"
}

fn helper_source() -> String {
    let mut lines: Vec<String> = [
        "export interface ITextEditorViewState { scrollTop: number; cursor: number }",
        "",
        "export function applyTextEditorOptions(options: { selection?: number; viewState?: ITextEditorViewState }, editor: { reveal(line: number): void; restore(s: ITextEditorViewState): void }): boolean {",
        "  if (options.viewState) {",
        "    editor.restore(massageEditorViewState(options.viewState));",
        "    return true;",
        "  }",
        "  if (typeof options.selection === \"number\") {",
        "    editor.reveal(options.selection);",
        "    return true;",
        "  }",
        "  return false;",
        "}",
        "",
        "function massageEditorViewState(state: ITextEditorViewState): ITextEditorViewState {",
        "  return { scrollTop: Math.max(0, state.scrollTop), cursor: Math.max(0, state.cursor) };",
        "}",
    ]
    .map(str::to_string)
    .to_vec();
    for i in 0..12 {
        lines.extend([
            String::new(),
            format!(
                "export function restoreEditorGroup{i}(state: ITextEditorViewState): number {{"
            ),
            format!(
                "  const offset = state.scrollTop * {} + state.cursor;",
                i + 2
            ),
            format!(
                "  return offset > {} ? offset - {i} : offset + {i};",
                100 * (i + 1)
            ),
            "}".to_string(),
        ]);
    }
    lines.join("\n") + "\n"
}

struct Fixture {
    _dir: TestDir,
    project: PathBuf,
    registry: String,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = TestDir::new(label);
        let project = dir.path.join("editor");
        let registry = registry_source();
        for (rel, body) in [(REGISTRY, registry.clone()), (HELPER, helper_source())] {
            let path = project.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        run_in(&dir.path, &["init", project.to_str().unwrap()]);
        Self {
            _dir: dir,
            project,
            registry,
        }
    }

    fn explore(&self, query: &str) -> String {
        run_in(&self.project, &["explore", query])
    }

    /// How many of the registry's named definitions the response sent whole:
    /// the two `clampedInt`s (function and static method) and the two
    /// `cursorStyle*` functions.
    fn complete_named_bodies(&self, text: &str) -> usize {
        let header = format!("#### {REGISTRY} ");
        let rendered: BTreeSet<usize> = text
            .find(&header)
            .map(|start| {
                let rest = &text[start + header.len()..];
                rest[..rest.find("\n#### ").unwrap_or(rest.len())]
                    .lines()
                    .filter_map(|line| line.split_once('\t'))
                    .filter_map(|(number, _)| number.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let lines: Vec<&str> = self.registry.lines().collect();
        let mut complete = 0;
        for (index, line) in lines.iter().enumerate() {
            let opens_named = NAMED.iter().any(|name| {
                line.starts_with(&format!("export function {name}"))
                    || line
                        .trim_start()
                        .starts_with(&format!("public static {name}"))
            });
            if !opens_named {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let close = format!("{}}}", " ".repeat(indent));
            let end = index
                + lines[index..]
                    .iter()
                    .position(|candidate| *candidate == close)
                    .unwrap();
            if (index + 1..=end + 1).all(|n| rendered.contains(&n)) {
                complete += 1;
            }
        }
        complete
    }
}

#[test]
fn pins_only_the_file_defining_the_named_symbols_and_names_the_one_set_aside() {
    let fx = Fixture::new("narrow");
    let text = fx.explore(&format!("editorOptions.ts {}", NAMED.join(" ")));
    assert!(text.contains("1 file pinned from the query."), "{text}");
    assert!(
        text.contains(&format!(
            "Not pinned: `{HELPER}`, which defines none of the named symbols."
        )),
        "{text}"
    );
    assert!(!text.contains(&format!("#### {HELPER} ")), "{text}");
}

/// Upstream's two pins split a reserved share; this port funds files in order,
/// so the room was never split and this holds with or without narrowing. It
/// guards that naming the file never costs the named file its bodies.
#[test]
fn the_named_file_keeps_the_room_it_gets_without_the_path() {
    let fx = Fixture::new("room");
    let pinned = fx.explore(&format!("editorOptions.ts {}", NAMED.join(" ")));
    let unpinned = fx.explore(&NAMED.join(" "));
    assert!(
        fx.complete_named_bodies(&pinned) >= fx.complete_named_bodies(&unpinned),
        "pinned:\n{pinned}\nunpinned:\n{unpinned}"
    );
    assert_eq!(fx.complete_named_bodies(&pinned), 4, "{pinned}");
}

#[test]
fn pins_both_when_the_query_names_no_symbol_to_choose_between_them() {
    let fx = Fixture::new("both");
    let text = fx.explore("editorOptions.ts");
    assert!(text.contains("2 files pinned from the query."), "{text}");
    assert!(!text.contains("Not pinned:"), "{text}");
}

#[test]
fn a_path_with_its_directory_pins_that_file_alone_with_no_note() {
    let fx = Fixture::new("dir");
    let text = fx.explore(&format!("{HELPER} {}", NAMED.join(" ")));
    assert!(text.contains("1 file pinned from the query."), "{text}");
    assert!(!text.contains("Not pinned:"), "{text}");
}
