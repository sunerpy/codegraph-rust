//! A session replaces a daemon left running by an older install (upstream #2343).
//!
//! Each test stands up a daemon for a project and then starts a stdio session
//! from this build. The daemon is this build too, but its `test-hooks` seams
//! make it announce another version in its hello and, in one test, leave
//! control frames unanswered:
//!
//! - an older plain `X.Y.Z` release is asked to drain, exits, and is replaced
//!   by a daemon from this install, which then keeps the session's index live;
//! - a newer release is never touched;
//! - an older release that does not drain keeps the project;
//!
//! In the last two cases the session serves reads without auto-sync and says
//! so on stderr. In every case exactly one daemon holds the writer slot.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const READY_WITHIN: Duration = Duration::from_secs(20);
const EDIT_VISIBLE_WITHIN: Duration = Duration::from_secs(15);
const READ_ONLY_NOTICE: &str = "Serving reads in-process without auto-sync";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-handover-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_live_source(project: &Path, symbol: &str) {
    fs::write(
        project.join("src/live.ts"),
        format!("export function {symbol}(): number {{ return 1; }}\n"),
    )
    .unwrap();
}

fn indexed_project(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    write_live_source(&project, "handoverInitialSymbol");
    let output = Command::new(bin())
        .args(["init"])
        .arg(&project)
        .env_remove("CODEGRAPH_DIR")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .output()
        .expect("run codegraph init");
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    project.canonicalize().unwrap()
}

/// The pid `daemon.pid` names, when that process is alive.
fn daemon_pid(project: &Path) -> Option<u32> {
    let raw = fs::read_to_string(codegraph_daemon::daemon_pid_path(project).ok()?).ok()?;
    codegraph_daemon::decode_lock_info(&raw)
        .map(|info| info.pid)
        .filter(|&pid| pid > 0 && codegraph_daemon::is_process_alive(pid))
}

/// The live process holding the project's writer slot.
fn writer_pid(project: &Path) -> Option<u32> {
    codegraph_daemon::read_writer_lock(project).map(|info| info.pid)
}

/// A daemon of this build standing in for another install's: `hello_version`
/// is what it announces, and `ignore_control` makes it close every control
/// frame's connection unanswered.
struct StandInDaemon {
    child: Child,
}

impl StandInDaemon {
    fn start(project: &Path, scratch: &Path, hello_version: &str, ignore_control: bool) -> Self {
        let mut command = Command::new(bin());
        command
            .args(["serve", "--mcp", "--path"])
            .arg(project)
            .env("CODEGRAPH_DAEMON_INTERNAL", "1")
            .env("CODEGRAPH_TEST_DAEMON_HELLO_VERSION", hello_version)
            .env("CODEGRAPH_NO_WATCH", "1")
            .env("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "120000")
            .env("CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS", "120000")
            .env_remove("CODEGRAPH_DIR")
            .env_remove("CODEGRAPH_NO_DAEMON")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(scratch.join("stand-in-daemon.log")).unwrap());
        if ignore_control {
            command.env("CODEGRAPH_TEST_DAEMON_IGNORE_CONTROL", "1");
        }
        let child = command.spawn().expect("spawn the stand-in daemon");
        let daemon = Self { child };
        let deadline = Instant::now() + READY_WITHIN;
        loop {
            let attached = codegraph_daemon::recorded_socket_path(project)
                .ok()
                .and_then(|socket| codegraph_daemon::attach_to_daemon(&socket).ok());
            if daemon_pid(project) == Some(daemon.pid())
                && attached.is_some_and(|client| client.hello["codegraph"] == hello_version)
            {
                return daemon;
            }
            assert!(
                Instant::now() < deadline,
                "the stand-in daemon did not come up within {READY_WITHIN:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn exited_within(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.child.try_wait().unwrap().is_some()
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for StandInDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A stdio `serve --mcp --path <project>` from this build.
struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_log: PathBuf,
    next_id: i64,
}

impl Session {
    fn start(project: &Path, scratch: &Path) -> Self {
        let stderr_log = scratch.join("session.stderr.log");
        let mut child = Command::new(bin())
            .args(["serve", "--mcp", "--path"])
            .arg(project)
            .env_remove("CODEGRAPH_NO_DAEMON")
            .env_remove("CODEGRAPH_NO_WATCH")
            .env_remove("CODEGRAPH_DIR")
            .env("CODEGRAPH_MCP_REGISTRY_DIR", scratch.join("mcp-registry"))
            .env("CODEGRAPH_WATCH_DEBOUNCE_MS", "100")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(&stderr_log).unwrap())
            .spawn()
            .expect("spawn serve --mcp");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut session = Self {
            child,
            stdin,
            stdout,
            stderr_log,
            next_id: 1,
        };
        let id = session.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "daemon-handover", "version": "0" }
            }),
        );
        let response = session.read_id(id);
        assert_eq!(
            response["result"]["serverInfo"]["name"], "codegraph",
            "{response}"
        );
        session.notify("notifications/initialized");
        session
    }

    fn request(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    fn notify(&mut self, method: &str) {
        self.write(json!({ "jsonrpc": "2.0", "method": method }));
    }

    fn write(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    }

    fn read_id(&mut self, want: i64) -> Value {
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(
                read > 0,
                "the session closed stdout; stderr:\n{}",
                self.stderr()
            );
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                panic!("non-JSON line on the session's stdout: {line:?}");
            };
            if value.get("id").and_then(Value::as_i64) == Some(want) {
                return value;
            }
        }
    }

    fn search(&mut self, query: &str) -> String {
        let id = self.request(
            "tools/call",
            json!({ "name": "codegraph_search", "arguments": { "query": query } }),
        );
        let response = self.read_id(id);
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn wait_for_symbol(&mut self, symbol: &str, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            let text = self.search(symbol);
            if text.contains(symbol) && text.contains("src/live.ts") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "`{symbol}` was not searchable within {within:?}: {text}\nstderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr_log).unwrap_or_default()
    }

    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the session did not exit after stdin EOF");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_new_daemon(project: &Path, old: u32) -> u32 {
    let deadline = Instant::now() + READY_WITHIN;
    loop {
        if let Some(pid) = daemon_pid(project)
            && pid != old
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "no daemon replaced pid {old} within {READY_WITHIN:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn shutdown_daemon(project: &Path) {
    let paths = codegraph_core::IndexPaths::resolve(project, None).unwrap();
    match codegraph_daemon::request_daemon_shutdown(project, paths.project_identity()) {
        Ok(codegraph_daemon::ShutdownOutcome::NoDaemon)
        | Ok(codegraph_daemon::ShutdownOutcome::Drained { .. }) => {}
        other => panic!("failed to stop the daemon: {other:?}"),
    }
}

#[test]
fn an_older_release_daemon_is_replaced_and_the_session_stays_live() {
    let dir = TestDir::new("older");
    let project = indexed_project(&dir);
    let mut old = StandInDaemon::start(&project, dir.path(), "0.0.1", false);
    let old_pid = old.pid();
    assert_eq!(writer_pid(&project), Some(old_pid));

    let mut session = Session::start(&project, dir.path());
    assert!(
        old.exited_within(READY_WITHIN),
        "the older daemon did not drain and exit; session stderr:\n{}",
        session.stderr()
    );
    let new_pid = wait_for_new_daemon(&project, old_pid);
    session.wait_for_symbol("handoverInitialSymbol", EDIT_VISIBLE_WITHIN);

    let edited = "handoverEditAfterReplacement";
    write_live_source(&project, edited);
    session.wait_for_symbol(edited, EDIT_VISIBLE_WITHIN);
    assert_eq!(
        writer_pid(&project),
        Some(new_pid),
        "the replacement daemon alone holds the writer slot"
    );
    assert!(
        !session.stderr().contains(READ_ONLY_NOTICE),
        "a replaced daemon leaves nothing to serve read-only: {}",
        session.stderr()
    );

    session.close();
    shutdown_daemon(&project);
}

#[test]
fn a_newer_release_daemon_is_left_alone_and_the_session_serves_reads() {
    let dir = TestDir::new("newer");
    let project = indexed_project(&dir);
    let mut newer = StandInDaemon::start(&project, dir.path(), "999.0.0", false);

    let mut session = Session::start(&project, dir.path());
    session.wait_for_symbol("handoverInitialSymbol", EDIT_VISIBLE_WITHIN);
    let stderr = session.stderr();
    assert!(
        stderr.contains(READ_ONLY_NOTICE) && stderr.contains("999.0.0"),
        "the session says it serves reads only, and why: {stderr}"
    );
    assert!(newer.running(), "a newer daemon is never stopped");
    assert_eq!(writer_pid(&project), Some(newer.pid()));
    session.close();
}

#[test]
fn an_older_release_daemon_that_does_not_drain_keeps_the_project() {
    let dir = TestDir::new("undrained");
    let project = indexed_project(&dir);
    let mut stuck = StandInDaemon::start(&project, dir.path(), "0.0.1", true);

    let mut session = Session::start(&project, dir.path());
    session.wait_for_symbol("handoverInitialSymbol", EDIT_VISIBLE_WITHIN);
    let stderr = session.stderr();
    assert!(
        stderr.contains(READ_ONLY_NOTICE) && stderr.contains("did not drain"),
        "the session says it serves reads only, and why: {stderr}"
    );
    assert!(stuck.running(), "a daemon that did not drain is not killed");
    assert_eq!(writer_pid(&project), Some(stuck.pid()));
    session.close();
}

/// A session classified an older daemon, and a newer one took the project
/// before the shutdown frame went out: the frame is sent only on a connection
/// whose own hello names the classified peer, so the newer daemon never sees
/// it.
#[test]
fn a_shutdown_bound_to_an_older_peer_never_drains_a_newer_daemon() {
    let dir = TestDir::new("race");
    let project = indexed_project(&dir);
    let mut newer = StandInDaemon::start(&project, dir.path(), "999.0.0", false);
    let paths = codegraph_core::IndexPaths::resolve(&project, None).unwrap();

    let outcome =
        codegraph_daemon::request_daemon_shutdown_of(&project, paths.project_identity(), |hello| {
            hello["codegraph"] == "0.0.1"
        });
    assert!(
        matches!(
            outcome,
            Ok(codegraph_daemon::ShutdownOutcome::Declined { pid, .. }) if pid == newer.pid()
        ),
        "{outcome:?}"
    );
    assert!(newer.running(), "the newer daemon never received the frame");
    assert_eq!(writer_pid(&project), Some(newer.pid()));
}

/// The daemon a copied install starts exits once that copy is replaced: the
/// copy is renamed away, the way updaters move a running executable aside, and
/// a new file takes its path.
#[test]
fn a_daemon_exits_once_its_copied_install_is_replaced() {
    let dir = TestDir::new("install");
    let project = indexed_project(&dir);
    let install_dir = dir.path().join("install");
    fs::create_dir_all(&install_dir).unwrap();
    let copy = install_dir.join(bin().file_name().unwrap());
    fs::copy(bin(), &copy).expect("copy the codegraph binary");

    let mut daemon = Command::new(&copy)
        .args(["serve", "--mcp", "--path"])
        .arg(&project)
        .env("CODEGRAPH_DAEMON_INTERNAL", "1")
        .env("CODEGRAPH_DAEMON_INSTALL_CHECK_MS", "200")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "120000")
        .env("CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS", "120000")
        .env_remove("CODEGRAPH_DIR")
        .env_remove("CODEGRAPH_NO_DAEMON")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(dir.path().join("copied-daemon.log")).unwrap())
        .spawn()
        .expect("start the copied daemon");
    let deadline = Instant::now() + READY_WITHIN;
    while daemon_pid(&project) != Some(daemon.id()) {
        assert!(
            Instant::now() < deadline,
            "the copied daemon did not come up"
        );
        assert!(
            daemon.try_wait().unwrap().is_none(),
            "the copied daemon exited early"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    // Two check intervals pass without a change: the daemon stays up.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        daemon.try_wait().unwrap().is_none(),
        "an untouched install keeps its daemon"
    );

    fs::rename(&copy, install_dir.join("codegraph.old")).expect("move the running copy aside");
    fs::write(&copy, b"an upgrade put a new build here").expect("install the upgrade");

    let deadline = Instant::now() + Duration::from_secs(10);
    let exited = loop {
        if let Some(status) = daemon.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if exited.is_none() {
        let _ = daemon.kill();
        let _ = daemon.wait();
    }
    assert!(
        exited.is_some(),
        "the daemon kept running after its install was replaced; log:\n{}",
        fs::read_to_string(dir.path().join("copied-daemon.log")).unwrap_or_default()
    );
    assert_eq!(
        daemon_pid(&project),
        None,
        "the exiting daemon removed its own pid record"
    );
}
