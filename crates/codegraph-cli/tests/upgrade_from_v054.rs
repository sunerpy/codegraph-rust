//! Upgrading an index the previous release wrote.
//!
//! `tests/fixtures/upgrade_v0_54/codegraph.db` is the schema-8 database the
//! released v0.54.0 binary (extraction version 21) wrote for the mini corpus.
//! Each test plants it in a fresh project namespace, stages the state slot that
//! release publishes (`phase=current`, extraction version 21), and upgrades it
//! through the paths a user hits: `codegraph sync` from the shell, the catch-up
//! sync a daemon or MCP session runs when it first opens the project, and a cold
//! `serve --mcp` session in the default daemon mode, whose daemon must upgrade
//! the index before it starts serving. Each must classify the namespace as
//! outdated, rebuild it from source, and leave an index at the current schema
//! whose canonical graph equals a fresh `init` of the same tree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use codegraph_bench::oracle::{canonicalize_db, diff_canonical};
use codegraph_core::IndexPaths;
use codegraph_store::migrations::CURRENT_SCHEMA_VERSION;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, ExtractionStatus, Store, checksum_hex,
};

/// Extraction version the v0.54.0 release published.
const V0_54_EXTRACTION_VERSION: u64 = 21;

#[test]
fn cli_sync_upgrades_a_v0_54_index_to_the_current_schema_and_extraction() {
    let dir = TestDir::new("cli");
    let project = plant_v0_54_index(&dir);

    let status = cli(&["status", path_str(&project), "--json"]);
    assert!(
        status.ok,
        "status on a v0.54.0 index failed: {}",
        status.stderr
    );
    let json: serde_json::Value = serde_json::from_str(&status.stdout).expect("status JSON");
    assert_eq!(
        json["extractionStatus"], "outdated",
        "a v0.54.0 index must read as outdated: {}",
        status.stdout
    );

    let sync = cli(&["sync", path_str(&project)]);
    assert!(
        sync.ok,
        "sync over a v0.54.0 index failed:\nstdout:\n{}\nstderr:\n{}",
        sync.stdout, sync.stderr
    );

    assert_upgraded_like_a_fresh_init(&project, "cli");
}

#[test]
fn catch_up_sync_upgrades_a_v0_54_index_to_the_current_schema_and_extraction() {
    let dir = TestDir::new("catch-up");
    let project = plant_v0_54_index(&dir);

    // The catch-up a daemon or MCP session runs when it first opens a project.
    codegraph_watch::sync_project_once(&project).expect("catch-up sync over a v0.54.0 index");

    assert_upgraded_like_a_fresh_init(&project, "catch-up");
}

/// The default daemon mode: a cold `serve --mcp` session answers direct and
/// spawns the project's daemon, which used to refuse an `outdated` index at its
/// startup gate, so nothing ever upgraded it and every tool call failed until a
/// manual `codegraph sync`. The daemon must upgrade the index first, then serve
/// it and keep it live.
#[cfg(unix)]
#[test]
fn a_cold_mcp_session_upgrades_a_v0_54_index_through_its_daemon() {
    let dir = TestDir::new("daemon");
    let project = plant_v0_54_index(&dir);
    let paths = index_paths(&project);
    let mut session = cold::Session::start(&project, dir.path());

    let deadline = std::time::Instant::now() + cold::UPGRADED_WITHIN;
    while Store::extraction_status(&paths) != ExtractionStatus::Current {
        assert!(
            std::time::Instant::now() < deadline,
            "the v0.54.0 index was still {:?} {:?} after the cold session started;\n\
             daemon.log:\n{}\nsession stderr:\n{}",
            Store::extraction_status(&paths),
            cold::UPGRADED_WITHIN,
            fs::read_to_string(paths.daemon_log()).unwrap_or_default(),
            session.stderr()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    cold::wait_for_daemon(&project, &session);
    assert_upgraded_like_a_fresh_init(&project, "daemon");

    let marker = "upgradedDaemonSessionMarker";
    fs::write(
        project.join("src/upgraded.ts"),
        format!("export function {marker}(): number {{\n  return 1;\n}}\n"),
    )
    .unwrap();
    session.wait_for_symbol(marker, "src/upgraded.ts");
    session.close();
    cold::shutdown_daemon(&project);
}

/// A daemon never undoes an older `uninit --force`. That leaves an
/// `uninitialized` slot at the older extraction version, which the state gate
/// classifies as `outdated`, a tombstone, and no database; the daemon must
/// leave all of it alone and refuse to start, as it did before.
#[cfg(unix)]
#[test]
fn a_cold_mcp_session_leaves_an_older_uninit_alone() {
    let dir = TestDir::new("daemon-uninit");
    let project = dir.path().join("project");
    copy_tree(&mini_fixture(), &project);
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init failed: {}", init.stderr);
    let uninit = cli(&["uninit", "--force", path_str(&project)]);
    assert!(uninit.ok, "uninit failed: {}", uninit.stderr);
    let paths = index_paths(&project);
    stage_state_slot(
        &paths,
        3,
        CURRENT_STORAGE_PROTOCOL,
        V0_54_EXTRACTION_VERSION,
        "uninitialized",
    );
    let outdated = ExtractionStatus::Outdated {
        built: V0_54_EXTRACTION_VERSION,
    };
    assert_eq!(Store::extraction_status(&paths), outdated);
    assert!(paths.tombstone().is_file(), "uninit leaves its tombstone");
    assert!(!paths.current_db().exists(), "uninit removes the database");
    let slots = state_slot_bytes(&paths);

    let session = cold::Session::start(&project, dir.path());
    let log = cold::wait_for_daemon_log(&paths, "refusing to start a daemon", &session);
    assert!(
        !paths.current_db().exists(),
        "the daemon recreated the database an uninit removed:\n{log}"
    );
    assert!(paths.tombstone().is_file(), "the tombstone must stay");
    assert_eq!(Store::extraction_status(&paths), outdated);
    assert_eq!(
        state_slot_bytes(&paths),
        slots,
        "the state slots must not move"
    );
    assert!(
        !cold::daemon_live(&project),
        "no daemon may serve this index"
    );
    assert!(
        log.contains("left the outdated index to the startup gate"),
        "the daemon must say why it left the index alone:\n{log}"
    );
    session.close();
}

/// A failed upgrade changes nothing, and the startup gate still refuses the
/// index with its own guidance. Here the project config cannot be parsed.
#[cfg(unix)]
#[test]
fn a_daemon_whose_upgrade_fails_still_refuses_the_outdated_index() {
    let dir = TestDir::new("daemon-failed-upgrade");
    let project = plant_v0_54_index(&dir);
    let paths = index_paths(&project);
    fs::write(paths.config_toml(), "[indexing\n").unwrap();
    let database = fs::read(paths.current_db()).unwrap();
    let slots = state_slot_bytes(&paths);

    let session = cold::Session::start(&project, dir.path());
    let log = cold::wait_for_daemon_log(&paths, "refusing to start a daemon", &session);
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Outdated {
            built: V0_54_EXTRACTION_VERSION
        }
    );
    assert_eq!(
        fs::read(paths.current_db()).unwrap(),
        database,
        "a failed upgrade must leave the database as it was"
    );
    assert_eq!(
        state_slot_bytes(&paths),
        slots,
        "the state slots must not move"
    );
    assert!(
        !cold::daemon_live(&project),
        "no daemon may serve this index"
    );
    assert!(
        log.contains("could not upgrade the outdated index"),
        "the daemon must log the failed upgrade:\n{log}"
    );
    session.close();
}

/// The upgrade a daemon runs rebuilds only a complete older index. Each
/// namespace below is one `codegraph sync` would still migrate.
#[test]
fn the_daemon_upgrade_skips_an_interrupted_older_build() {
    let dir = TestDir::new("skip-building");
    let project = plant_v0_54_index(&dir);
    stage_state_slot(
        &index_paths(&project),
        3,
        CURRENT_STORAGE_PROTOCOL,
        V0_54_EXTRACTION_VERSION,
        "building",
    );
    assert_upgrade_skipped_untouched(&project, "records phase building");
}

#[test]
fn the_daemon_upgrade_skips_an_older_index_with_a_tombstone() {
    let dir = TestDir::new("skip-tombstone");
    let project = plant_v0_54_index(&dir);
    fs::write(index_paths(&project).tombstone(), b"uninitialized\n").unwrap();
    assert_upgrade_skipped_untouched(&project, "tombstone is present");
}

#[test]
fn the_daemon_upgrade_skips_an_older_index_without_its_database() {
    let dir = TestDir::new("skip-missing-db");
    let project = plant_v0_54_index(&dir);
    fs::remove_file(index_paths(&project).current_db()).unwrap();
    assert_upgrade_skipped_untouched(&project, "database is missing");
}

fn assert_upgrade_skipped_untouched(project: &Path, reason: &str) {
    let paths = index_paths(project);
    let status = Store::extraction_status(&paths);
    assert!(
        matches!(status, ExtractionStatus::Outdated { .. }),
        "each case starts outdated: {status:?}"
    );
    let database = fs::read(paths.current_db()).ok();
    let tombstone = fs::read(paths.tombstone()).ok();
    let slots = state_slot_bytes(&paths);
    match codegraph_watch::upgrade_outdated_index_once(project).expect("daemon upgrade entry") {
        codegraph_watch::OutdatedUpgrade::Skipped(text) => {
            assert!(text.contains(reason), "skipped for another reason: {text}");
        }
        other => panic!("expected a skip ({reason}), got {other:?}"),
    }
    assert_eq!(Store::extraction_status(&paths), status);
    assert_eq!(fs::read(paths.current_db()).ok(), database);
    assert_eq!(fs::read(paths.tombstone()).ok(), tombstone);
    assert_eq!(state_slot_bytes(&paths), slots);
}

/// Both state slots, byte for byte (`None` for an absent slot).
fn state_slot_bytes(paths: &IndexPaths) -> [Option<Vec<u8>>; 2] {
    paths.state_slots().map(|slot| fs::read(slot).ok())
}

/// A cold `serve --mcp --path <project>` over line-delimited JSON-RPC, the
/// harness of `cold_start_live_sync.rs` reduced to what the upgrade case needs.
#[cfg(unix)]
mod cold {
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    /// Upper bound for the daemon to upgrade the mini corpus and publish.
    pub const UPGRADED_WITHIN: Duration = Duration::from_secs(60);
    /// Upper bound for an edit to reach `codegraph_search` once it serves.
    const EDIT_VISIBLE_WITHIN: Duration = Duration::from_secs(10);

    pub struct Session {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: BufReader<ChildStdout>,
        stderr_log: PathBuf,
        next_id: i64,
    }

    impl Session {
        pub fn start(project: &Path, scratch: &Path) -> Self {
            let stderr_log = scratch.join("serve.stderr.log");
            let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
                .args(["serve", "--mcp", "--path"])
                .arg(project)
                .env_remove("CODEGRAPH_NO_DAEMON")
                .env_remove("CODEGRAPH_NO_WATCH")
                .env_remove("CODEGRAPH_DIR")
                .env("CODEGRAPH_MCP_REGISTRY_DIR", scratch.join("mcp-registry"))
                .env("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS", "1000")
                .env("CODEGRAPH_DAEMON_RETRY_MS", "200")
                .env("CODEGRAPH_DAEMON_RETRY_MAX_MS", "1000")
                .env("CODEGRAPH_WATCH_DEBOUNCE_MS", "100")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(fs::File::create(&stderr_log).expect("create serve stderr log"))
                .spawn()
                .expect("spawn cold serve --mcp");
            let stdin = child.stdin.take().expect("serve stdin");
            let stdout = BufReader::new(child.stdout.take().expect("serve stdout"));
            let mut session = Self {
                child,
                stdin: Some(stdin),
                stdout,
                stderr_log,
                next_id: 1,
            };
            let id = session.next_id();
            session.write(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "upgrade-from-v054", "version": "0" }
                }
            }));
            let response = session.read_id(id);
            assert_eq!(
                response["result"]["serverInfo"]["name"], "codegraph",
                "{response}"
            );
            session.write(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
            session
        }

        fn next_id(&mut self) -> i64 {
            let id = self.next_id;
            self.next_id += 1;
            id
        }

        fn write(&mut self, value: Value) {
            let stdin = self.stdin.as_mut().expect("serve stdin stays open");
            writeln!(stdin, "{value}").unwrap();
            stdin.flush().unwrap();
        }

        fn read_id(&mut self, want: i64) -> Value {
            loop {
                let mut line = String::new();
                let read = self.stdout.read_line(&mut line).expect("read serve stdout");
                assert!(
                    read > 0,
                    "serve closed stdout before response {want}; stderr:\n{}",
                    self.stderr()
                );
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(trimmed).unwrap_or_else(|error| {
                    panic!("non-JSON line on serve stdout ({error}): {trimmed:?}")
                });
                if value.get("id").and_then(Value::as_i64) == Some(want) {
                    return value;
                }
            }
        }

        fn search(&mut self, query: &str) -> String {
            let id = self.next_id();
            self.write(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": "codegraph_search", "arguments": { "query": query } }
            }));
            let response = self.read_id(id);
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        }

        pub fn wait_for_symbol(&mut self, symbol: &str, file: &str) {
            let deadline = Instant::now() + EDIT_VISIBLE_WITHIN;
            loop {
                let text = self.search(symbol);
                if text.contains(symbol) && text.contains(file) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "`{symbol}` was not searchable within {EDIT_VISIBLE_WITHIN:?}; last result:\n{text}\nstderr:\n{}",
                    self.stderr()
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        pub fn stderr(&self) -> String {
            fs::read_to_string(&self.stderr_log).unwrap_or_default()
        }

        pub fn close(mut self) {
            drop(self.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if self.child.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("cold serve did not exit after stdin EOF");
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

    /// Upper bound for a refusing daemon to write its log lines.
    const REFUSED_WITHIN: Duration = Duration::from_secs(30);

    /// Whether a live daemon owns `project`.
    pub fn daemon_live(project: &Path) -> bool {
        codegraph_daemon::daemon_pid_path(project)
            .ok()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|raw| codegraph_daemon::decode_lock_info(&raw))
            .is_some_and(|info| info.pid > 0 && codegraph_daemon::is_process_alive(info.pid))
    }

    /// Wait until the project's `daemon.log` contains `needle`; returns the log.
    pub fn wait_for_daemon_log(
        paths: &codegraph_core::IndexPaths,
        needle: &str,
        session: &Session,
    ) -> String {
        let deadline = Instant::now() + REFUSED_WITHIN;
        loop {
            let log = fs::read_to_string(paths.daemon_log()).unwrap_or_default();
            if log.contains(needle) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "daemon.log did not show {needle:?} within {REFUSED_WITHIN:?}:\n{log}\nsession stderr:\n{}",
                session.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until a live daemon owns `project`: it publishes only after the
    /// upgrade and its startup gate.
    pub fn wait_for_daemon(project: &Path, session: &Session) {
        let deadline = Instant::now() + UPGRADED_WITHIN;
        loop {
            if daemon_live(project) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no live daemon within {UPGRADED_WITHIN:?}; stderr:\n{}",
                session.stderr()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn shutdown_daemon(project: &Path) {
        let paths = codegraph_core::IndexPaths::resolve(project, None).unwrap();
        match codegraph_daemon::request_daemon_shutdown(project, paths.project_identity()) {
            Ok(codegraph_daemon::ShutdownOutcome::NoDaemon)
            | Ok(codegraph_daemon::ShutdownOutcome::Drained { .. }) => {}
            other => panic!("failed to stop the daemon: {other:?}"),
        }
    }
}

/// Copy the mini corpus into `dir`, create a namespace with the current binary,
/// then replace its database and state slot with what v0.54.0 left behind.
fn plant_v0_54_index(dir: &TestDir) -> PathBuf {
    let project = dir.path().join("project");
    copy_tree(&mini_fixture(), &project);
    let init = cli(&["init", path_str(&project)]);
    assert!(init.ok, "init failed: {}", init.stderr);

    let paths = index_paths(&project);
    let db = paths.current_db();
    for sidecar in ["-wal", "-shm"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{sidecar}", db.display())));
    }
    fs::copy(v0_54_database(), &db).expect("plant the v0.54.0 database");
    stage_state_slot(
        &paths,
        2,
        CURRENT_STORAGE_PROTOCOL,
        V0_54_EXTRACTION_VERSION,
        "current",
    );
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Outdated {
            built: V0_54_EXTRACTION_VERSION
        },
        "the planted namespace must classify as outdated before the upgrade"
    );
    project
}

fn assert_upgraded_like_a_fresh_init(project: &Path, label: &str) {
    let paths = index_paths(project);
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Current,
        "the upgraded namespace must be current"
    );
    // `immutable=1` takes no lock and leaves no `-wal`/`-shm` beside the index.
    let conn = rusqlite::Connection::open_with_flags(
        format!("{}?immutable=1", sqlite_file_uri(&paths.current_db())),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open upgraded database");
    let schema: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get(0)
        })
        .expect("schema version");
    assert_eq!(schema, CURRENT_SCHEMA_VERSION, "schema must be current");
    let stamp: String = conn
        .query_row(
            "SELECT value FROM project_metadata WHERE key = 'indexed_with_extraction_version'",
            [],
            |row| row.get(0),
        )
        .expect("extraction stamp");
    assert_eq!(stamp, CURRENT_EXTRACTION_VERSION.to_string());
    drop(conn);

    let peer_dir = TestDir::new(&format!("{label}-fresh"));
    let peer = peer_dir.path().join("project");
    copy_tree(&mini_fixture(), &peer);
    let init = cli(&["init", path_str(&peer)]);
    assert!(init.ok, "fresh init failed: {}", init.stderr);

    let upgraded = canonicalize_db(&paths.current_db()).expect("canonicalize upgraded index");
    let fresh =
        canonicalize_db(&index_paths(&peer).current_db()).expect("canonicalize fresh index");
    if let Err(report) = diff_canonical(&fresh, &upgraded, None) {
        panic!("the upgraded index differs from a fresh init:\n{report}");
    }
}

/// `path` as an absolute SQLite `file:` URI: a Windows verbatim prefix (`\\?\`)
/// is dropped and every byte outside the unreserved set is percent-encoded.
fn sqlite_file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

fn v0_54_database() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade_v0_54/codegraph.db")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn index_paths(project: &Path) -> IndexPaths {
    IndexPaths::resolve(project, None).expect("resolve index paths")
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 test path")
}

/// Author one authoritative state slot and remove its companion, the same way
/// `batch_m_outdated_migration.rs` stages protocol records.
fn stage_state_slot(
    paths: &IndexPaths,
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
) {
    let identity = paths.project_identity();
    let checksum = checksum_hex(
        sequence,
        storage_protocol,
        extraction_version,
        phase,
        identity,
    );
    let body = format!(
        "{{\"sequence\":{sequence},\"storageProtocol\":{storage_protocol},\
         \"extractionVersion\":{extraction_version},\"phase\":\"{phase}\",\
         \"projectIdentity\":\"{identity}\",\"checksum\":\"{checksum}\"}}\n"
    );
    let [slot0, slot1] = paths.state_slots();
    fs::write(&slot0, body).expect("stage authoritative state slot");
    let _ = fs::remove_file(&slot1);
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn cli(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env_remove("CODEGRAPH_DIR")
        .output()
        .expect("run codegraph binary");
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-upgrade-v054-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
