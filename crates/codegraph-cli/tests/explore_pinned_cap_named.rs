//! A symbol the query names survives a pinned file's node cap (upstream v1.6.1
//! #2064, a guard for #2062).
//!
//! A file named by path is pinned: its first 300 symbols by start line enter the
//! gather. Past that cap the file splits in two: a head cluster of one named
//! method merged with ~300 pinned neighbours and, one cluster down, the named
//! symbols the cap cut off. The head ranked first on density and shrank against
//! its own members only, so pin filler spent the file's room and the named
//! cluster rendered nothing. The cross-cluster hold-back of #2062 reaches this
//! shape without knowing about pinning; this fixture guards the pinned entry
//! path, since a hold-back re-scoped away from it would reopen the bug here and
//! nowhere else.
//!
//! In this port the shape does not go red without #2062 either: the head
//! cluster carries no edge-line members, so it never out-densifies the named
//! cluster, which ranks first. The guard holds the property, not the ranking.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const FILE: &str = "src/rpc/protocol.ts";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-pincap-{label}-{}-{}",
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

/// `receiveOne` → `dispatchRequest` / `MessageCodec.serializeAck` at the top,
/// 300 unrelated `trackMetric*` methods, then the `MessageKind` enum and a
/// `MessageCodec` whose five serializers are followed by 150 `encodeField*`
/// members.
fn protocol_source() -> String {
    let mut lines: Vec<String> = [
        "export class Protocol {",
        "  private handlers = new Map<string, (x: Uint8Array) => unknown>();",
        "  private outbox: Uint8Array[] = [];",
        "",
        "  receiveOne(raw: Uint8Array): void {",
        "    const kind = raw[0] as MessageKind;",
        "    if (kind === MessageKind.Request) {",
        "      this.dispatchRequest(raw);",
        "      this.outbox.push(MessageCodec.serializeAck(raw[1] ?? 0));",
        "    } else if (kind === MessageKind.Cancel) {",
        "      this.outbox.push(MessageCodec.serializeCancel(raw[1] ?? 0));",
        "    }",
        "  }",
        "",
        "  private dispatchRequest(raw: Uint8Array): void {",
        "    const id = raw[1] ?? 0;",
        "    const method = String(raw[2] ?? '');",
        "    const handler = this.handlers.get(method);",
        "    if (!handler) {",
        "      this.outbox.push(MessageCodec.serializeReplyErr(id, new Error(`no handler: ${method}`)));",
        "      return;",
        "    }",
        "    const started = Date.now();",
        "    let result: unknown;",
        "    try {",
        "      result = handler(raw.subarray(3));",
        "    } catch (err) {",
        "      this.outbox.push(MessageCodec.serializeReplyErr(id, err as Error));",
        "      return;",
        "    }",
        "    if (Date.now() - started > 1000) {",
        "      this.outbox.push(MessageCodec.serializeAck(id));",
        "    }",
        "    if (this.outbox.length > 64) {",
        "      this.outbox.splice(0, this.outbox.length - 64);",
        "    }",
        "    this.outbox.push(MessageCodec.serializeReply(id, result ?? null));",
        "  }",
    ]
    .map(str::to_string)
    .to_vec();
    for i in 0..300 {
        lines.extend([
            String::new(),
            format!("  trackMetric{i}(value: number): number {{"),
            format!("    return value * {} + this.outbox.length;", i + 1),
            "  }".to_string(),
        ]);
    }
    lines.extend(
        [
            "}",
            "",
            "export const enum MessageKind {",
            "  Request = 1,",
            "  Reply = 2,",
            "  ReplyErr = 3,",
            "  Cancel = 4,",
            "  Ack = 5,",
            "}",
            "",
            "export class MessageCodec {",
            "  static serializeRequest(id: number, method: string): Uint8Array {",
            "    const body = new TextEncoder().encode(method);",
            "    const out = new Uint8Array(body.length + 2);",
            "    out[0] = MessageKind.Request;",
            "    out[1] = id & 0xff;",
            "    out.set(body, 2);",
            "    return out;",
            "  }",
            "",
            "  static serializeAck(id: number): Uint8Array {",
            "    return Uint8Array.of(MessageKind.Ack, id & 0xff);",
            "  }",
            "",
            "  static serializeCancel(id: number): Uint8Array {",
            "    return Uint8Array.of(MessageKind.Cancel, id & 0xff);",
            "  }",
            "",
            "  static serializeReply(id: number, value: unknown): Uint8Array {",
            "    const body = new TextEncoder().encode(JSON.stringify(value ?? null));",
            "    const out = new Uint8Array(body.length + 2);",
            "    out[0] = MessageKind.Reply;",
            "    out[1] = id & 0xff;",
            "    out.set(body, 2);",
            "    return out;",
            "  }",
            "",
            "  static serializeReplyErr(id: number, err: Error): Uint8Array {",
            "    const body = new TextEncoder().encode(err.message);",
            "    const out = new Uint8Array(body.length + 2);",
            "    out[0] = MessageKind.ReplyErr;",
            "    out[1] = id & 0xff;",
            "    out.set(body, 2);",
            "    return out;",
            "  }",
        ]
        .map(str::to_string),
    );
    for i in 0..150 {
        lines.extend([
            String::new(),
            format!("  static encodeField{i}(id: number): Uint8Array {{"),
            format!(
                "    return Uint8Array.of({}, id & 0xff, {});",
                i % 250,
                (i * 7) % 250
            ),
            "  }".to_string(),
        ]);
    }
    lines.push("}".to_string());
    lines.join("\n") + "\n"
}

struct Fixture {
    _dir: TestDir,
    project: PathBuf,
    source: String,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = TestDir::new(label);
        let project = dir.path.join("rpc");
        let source = protocol_source();
        let path = project.join(FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &source).unwrap();
        run_in(&dir.path, &["init", project.to_str().unwrap()]);
        Self {
            _dir: dir,
            project,
            source,
        }
    }

    fn explore(&self, query: &str) -> String {
        run_in(&self.project, &["explore", query])
    }

    /// 1-based `(first, last)` line of the method `name`.
    fn span_of(&self, name: &str) -> (usize, usize) {
        let lines: Vec<&str> = self.source.lines().collect();
        let start = lines
            .iter()
            .position(|line| {
                let trimmed = line.trim_start();
                [
                    format!("{name}("),
                    format!("private {name}("),
                    format!("static {name}("),
                ]
                .iter()
                .any(|opening| trimmed.starts_with(opening))
            })
            .unwrap_or_else(|| panic!("{name} not in fixture"));
        let end = start
            + lines[start..]
                .iter()
                .position(|line| *line == "  }")
                .unwrap();
        (start + 1, end + 1)
    }

    /// Names whose whole definition did NOT reach the response.
    fn incomplete_bodies<'a>(&self, text: &str, names: &[&'a str]) -> Vec<&'a str> {
        let header = format!("#### {FILE} ");
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
        names
            .iter()
            .copied()
            .filter(|name| {
                let (start, end) = self.span_of(name);
                (start..=end).any(|line| !rendered.contains(&line))
            })
            .collect()
    }
}

#[test]
fn the_named_symbols_render_whole_when_the_file_is_not_pinned() {
    // The parity the pinned queries below are held to.
    let fx = Fixture::new("unpinned");
    let text = fx.explore("receiveOne serializeAck");
    assert_eq!(
        fx.incomplete_bodies(&text, &["receiveOne", "serializeAck"]),
        Vec::<&str>::new(),
        "{text}"
    );
}

#[test]
fn a_pinned_file_returns_a_named_symbol_past_the_node_cap() {
    let fx = Fixture::new("pinned");
    let text = fx.explore("protocol.ts receiveOne serializeAck");
    assert!(text.contains("1 file pinned from the query."), "{text}");
    assert_eq!(
        fx.incomplete_bodies(&text, &["receiveOne", "serializeAck"]),
        Vec::<&str>::new(),
        "{text}"
    );
    let lone = fx.explore("protocol.ts serializeAck");
    assert_eq!(
        fx.incomplete_bodies(&lone, &["serializeAck"]),
        Vec::<&str>::new(),
        "{lone}"
    );
}
