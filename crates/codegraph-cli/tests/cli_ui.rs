//! `codegraph ui` end to end — a port of upstream `__tests__/cli-ui-command.test.ts`
//! (`v1.6.1`) plus its viewer gate (`src/bin/viewer-gate.ts`): the binary is
//! run for real, the server it starts is asked over a real socket, and every
//! wait is bounded.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-cli-ui-{label}-{}-{}",
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

fn command(args: &[&str], viewer: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_codegraph"));
    command
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("NO_COLOR", "1")
        .env_remove("CODEGRAPH_BROWSER");
    if viewer {
        command.env("CODEGRAPH_UI", "1");
    } else {
        command.env_remove("CODEGRAPH_UI");
    }
    command
}

/// Run to completion: (exit code, stdout + stderr).
fn run(args: &[&str], viewer: bool) -> (i32, String) {
    let output = command(args, viewer).output().expect("run codegraph");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text)
}

fn indexed_project(label: &str) -> TestDir {
    let dir = TestDir::new(label);
    std::fs::create_dir_all(dir.path.join("src")).unwrap();
    std::fs::write(
        dir.path.join("src/auth.ts"),
        "export function parseToken(t: string){ return t.trim(); }\n",
    )
    .unwrap();
    let (code, output) = run(&["init", dir.path.to_str().unwrap()], false);
    assert_eq!(code, 0, "init: {output}");
    dir
}

/// A running viewer and everything it has printed so far.
struct Viewer {
    child: Child,
    port: u16,
    lines: mpsc::Receiver<String>,
    output: String,
}

impl Viewer {
    fn start(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut command = command(&[&["ui"], args].concat(), true);
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn codegraph ui");
        let (tx, lines) = mpsc::channel();
        for stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
        }
        let mut viewer = Self {
            child,
            port: 0,
            lines,
            output: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match viewer.lines.recv_timeout(left) {
                Ok(line) => {
                    viewer.output.push_str(&line);
                    viewer.output.push('\n');
                    if let Some(at) = line.find("http://127.0.0.1:") {
                        let rest = &line[at + "http://127.0.0.1:".len()..];
                        let digits: String =
                            rest.chars().take_while(char::is_ascii_digit).collect();
                        viewer.port = digits.parse().unwrap();
                        break;
                    }
                }
                Err(_) => panic!("codegraph ui never printed a URL:\n{}", viewer.output),
            }
        }
        viewer
    }

    /// Everything printed until `marker` appears, waiting at most `timeout`.
    fn read_until(&mut self, marker: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.output.contains(marker) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.output.push_str(&line);
                    self.output.push('\n');
                }
                Err(_) => return false,
            }
        }
        true
    }

    fn stop(self) {
        drop(self);
    }
}

/// A failing assertion unwinds through here too. `Child` neither kills nor
/// waits when dropped, so without this a test that fails while its server
/// runs leaves that server running after the test binary exits.
impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One HTTP/1.1 GET with the `Host` header given: (status, body).
fn get(port: u16, path: &str, host: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let host = host
        .map(str::to_string)
        .unwrap_or_else(|| format!("127.0.0.1:{port}"));
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/* ------------------------------------------------------------------- gate -- */

#[test]
fn the_viewer_is_refused_unless_codegraph_ui_is_1() {
    for args in [
        &["ui"][..],
        &["web", "."],
        &["help", "ui"],
        &["ui", "--help"],
        &["help", "web"],
    ] {
        let (code, output) = run(args, false);
        assert_eq!(code, 1, "{args:?}: {output}");
        let command = if args.contains(&"web") { "web" } else { "ui" };
        assert!(
            output.contains(&format!(
                "error: 'codegraph {command}' is not in this release yet. The browser viewer is coming in an upcoming release."
            )),
            "{args:?}: {output}"
        );
    }
    let (code, top) = run(&["--help"], false);
    assert_eq!(code, 0);
    assert!(!top.contains("Open the CodeGraph viewer"), "{top}");
}

/* ------------------------------------------------------------------- help -- */

#[test]
fn reads_well_and_documents_the_flags() {
    let (code, output) = run(&["ui", "--help"], true);
    assert_eq!(code, 0, "{output}");
    for needle in [
        "--port",
        "--no-open",
        "4747",
        "127.0.0.1",
        "read-only",
        "Examples:",
        "CODEGRAPH_BROWSER",
    ] {
        assert!(output.contains(needle), "{needle} in {output}");
    }
}

#[test]
fn works_through_codegraph_help_ui() {
    let (code, via_help) = run(&["help", "ui"], true);
    let (_, via_flag) = run(&["ui", "--help"], true);
    assert_eq!(code, 0);
    assert_eq!(via_help, via_flag);
}

#[test]
fn is_listed_in_the_top_level_help_and_web_is_an_alias() {
    let (_, top) = run(&["--help"], true);
    assert!(top.contains("ui") && top.contains("web"), "{top}");
    assert!(top.contains("Open the CodeGraph viewer"));
    let (code, via_alias) = run(&["help", "web"], true);
    assert_eq!(code, 0, "{via_alias}");
    assert!(via_alias.contains("--no-open"));
}

#[test]
fn rejects_a_nonsense_port_with_a_plain_message_not_a_stack_trace() {
    let (code, output) = run(&["ui", "--port", "banana"], true);
    assert_eq!(code, 1);
    assert!(output.contains("--port must be a whole number"), "{output}");
    assert!(!output.contains("panicked"));
}

/* --------------------------------------------------------------- refusals -- */

#[test]
fn gives_friendly_guidance_never_a_stack_trace_when_there_is_no_index() {
    let dir = TestDir::new("unindexed");
    std::fs::write(dir.path.join("a.ts"), "export const a = 1;\n").unwrap();
    let (code, output) = run(&["ui", dir.path.to_str().unwrap()], true);
    assert_eq!(code, 1);
    assert!(output.contains("No CodeGraph index found"), "{output}");
    assert!(output.contains("codegraph init"));
    assert!(!output.contains("Error:"), "{output}");
    assert!(!output.contains("panicked"));
}

#[cfg(unix)]
#[test]
fn refuses_a_sensitive_system_directory() {
    let (code, output) = run(&["ui", "/etc"], true);
    assert_eq!(code, 1);
    assert!(
        output.contains("Refusing to operate on sensitive"),
        "{output}"
    );
}

/* ---------------------------------------------------------------- serving -- */

#[test]
fn serves_the_viewer_and_prints_where_it_is() {
    let project = indexed_project("serve");
    let root = project.path.to_str().unwrap();
    let mut viewer = Viewer::start(&["--no-open", "--port", "0", root], &[]);
    assert!(
        viewer.read_until("Press Ctrl+C to stop", Duration::from_secs(10)),
        "{}",
        viewer.output
    );
    let (status, body) = get(viewer.port, "/", None);
    assert_eq!(status, 200);
    assert!(body.contains(r#"<div id="app">"#));
    assert!(
        viewer.output.contains("CodeGraph viewer"),
        "{}",
        viewer.output
    );
    let canonical = Path::new(root).canonicalize().unwrap();
    assert!(
        viewer.output.contains(&canonical.display().to_string()),
        "{}",
        viewer.output
    );
    assert!(viewer.output.contains("this machine only"));
    viewer.stop();
}

/// A stand-in browser that records the URL it was launched with.
fn opener(dir: &TestDir) -> (PathBuf, PathBuf) {
    let marker = dir.path.join("opened.txt");
    #[cfg(windows)]
    let script = {
        let script = dir.path.join("open.cmd");
        std::fs::write(&script, format!("@echo %1 > \"{}\"\r\n", marker.display())).unwrap();
        script
    };
    #[cfg(not(windows))]
    let script = {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.path.join("open.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s' \"$1\" > \"{}\"\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    };
    (script, marker)
}

fn wait_for_file(path: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return Some(text);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn honours_no_open_no_browser_is_launched() {
    let project = indexed_project("no-open");
    let tools = TestDir::new("opener-a");
    let (script, marker) = opener(&tools);
    let mut viewer = Viewer::start(
        &["--no-open", "--port", "0", project.path.to_str().unwrap()],
        &[("CODEGRAPH_BROWSER", script.to_str().unwrap())],
    );
    assert!(
        viewer.read_until("Press Ctrl+C to stop", Duration::from_secs(10)),
        "{}",
        viewer.output
    );
    assert_eq!(get(viewer.port, "/", None).0, 200);
    assert!(wait_for_file(&marker, Duration::from_millis(1500)).is_none());
    assert!(
        viewer.output.contains("Open that URL in a browser"),
        "{}",
        viewer.output
    );
    assert!(!viewer.output.contains("Opening your browser"));
    viewer.stop();
}

#[test]
fn opens_the_browser_at_the_served_url_when_no_open_is_absent() {
    let project = indexed_project("open");
    let tools = TestDir::new("opener-b");
    let (script, marker) = opener(&tools);
    let mut viewer = Viewer::start(
        &["--port", "0", project.path.to_str().unwrap()],
        &[("CODEGRAPH_BROWSER", script.to_str().unwrap())],
    );
    let opened = wait_for_file(&marker, Duration::from_secs(10)).expect("the opener ran");
    assert!(
        opened
            .trim()
            .contains(&format!("http://127.0.0.1:{}", viewer.port)),
        "{opened}"
    );
    assert!(
        viewer.read_until("Opening your browser", Duration::from_secs(10)),
        "{}",
        viewer.output
    );
    viewer.stop();
}

#[test]
fn codegraph_browser_none_suppresses_the_launch_like_no_open() {
    let project = indexed_project("browser-none");
    let mut viewer = Viewer::start(
        &["--port", "0", project.path.to_str().unwrap()],
        &[("CODEGRAPH_BROWSER", "none")],
    );
    assert!(
        viewer.read_until("Press Ctrl+C to stop", Duration::from_secs(10)),
        "{}",
        viewer.output
    );
    assert_eq!(get(viewer.port, "/", None).0, 200);
    assert!(
        viewer.output.contains("Open that URL in a browser"),
        "{}",
        viewer.output
    );
    viewer.stop();
}

#[test]
fn moves_off_the_default_port_when_it_is_busy() {
    let project = indexed_project("busy");
    // Occupy 4747 so the fallback has something to fall back FROM; if another
    // viewer already holds it the assertion below is still the right one.
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 4747)).ok();
    let viewer = Viewer::start(&["--no-open", project.path.to_str().unwrap()], &[]);
    assert_ne!(viewer.port, 4747);
    assert_eq!(get(viewer.port, "/", None).0, 200);
    viewer.stop();
    drop(blocker);
}

#[test]
fn refuses_to_move_off_a_port_the_user_pinned_with_port() {
    let project = indexed_project("pinned");
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = blocker.local_addr().unwrap().port().to_string();
    let (code, output) = run(
        &[
            "ui",
            "--no-open",
            "--port",
            &taken,
            project.path.to_str().unwrap(),
        ],
        true,
    );
    assert_eq!(code, 1);
    assert!(output.contains("already in use"), "{output}");
    assert!(!output.contains("panicked"));
}

#[test]
fn refuses_a_foreign_host_end_to_end() {
    let project = indexed_project("host");
    let viewer = Viewer::start(
        &["--no-open", "--port", "0", project.path.to_str().unwrap()],
        &[],
    );
    let (status, body) = get(viewer.port, "/", Some("evil.example"));
    assert_eq!(status, 403);
    assert!(!body.contains(r#"<div id="app">"#));
    viewer.stop();
}

#[cfg(unix)]
#[test]
fn shuts_down_cleanly_on_sigterm() {
    let project = indexed_project("sigterm");
    let mut viewer = Viewer::start(
        &["--no-open", "--port", "0", project.path.to_str().unwrap()],
        &[],
    );
    let pid = viewer.child.id().to_string();
    let killed = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(killed.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = viewer.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the viewer ignored SIGTERM");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
}
