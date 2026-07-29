//! Deterministic acceptance tests for v2 reader/writer lease topology.

use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{IndexLease, IndexLeaseError, Store, StoreError};

const CHILD_ACTION: &str = "CODEGRAPH_LEASE_LIFETIME_CHILD_ACTION";
const CHILD_PROJECT: &str = "CODEGRAPH_LEASE_LIFETIME_CHILD_PROJECT";
const BARRIER_ADDR: &str = "CODEGRAPH_TEST_LEASE_BARRIER_ADDR";
const BARRIER_MODE: &str = "CODEGRAPH_TEST_LEASE_BARRIER_MODE";
const WAIT: Duration = Duration::from_secs(10);
const PROBE_WAIT: Duration = Duration::from_millis(100);

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create fixture target");
    for entry in fs::read_dir(source).expect("read fixture source") {
        let entry = entry.expect("fixture entry");
        let destination = target.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy fixture file");
        }
    }
}

struct TestProject(PathBuf);

impl TestProject {
    fn indexed(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-lease-lifetime-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        copy_tree(
            &workspace_root().join("crates/codegraph-bench/fixtures/mini"),
            &path,
        );
        let output = Command::new(bin())
            .args(["init", path.to_str().expect("utf8 fixture path")])
            .env("CODEGRAPH_NO_DAEMON", "1")
            .output()
            .expect("run fixture init");
        assert!(
            output.status.success(),
            "fixture init failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn paths(&self) -> IndexPaths {
        IndexPaths::resolve(self.path(), None).expect("fixture IndexPaths")
    }

    fn http_registry_dir(&self) -> PathBuf {
        self.path().join("http-registry")
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct LeaseBarrier {
    address: SocketAddr,
    arrived: mpsc::Receiver<(u8, TcpStream)>,
}

impl LeaseBarrier {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind lease barrier");
        let address = listener.local_addr().expect("lease barrier address");
        let (arrived_tx, arrived_rx) = mpsc::channel();
        // Accept EVERY arrival, not just the first: one process can reach the
        // barrier more than once (a daemon takes a startup shared lease, then a
        // separate per-request shared lease), and each arrival must be releasable
        // in order rather than silently dropped.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(WAIT));
                let mut marker = [0_u8; 1];
                if stream.read_exact(&mut marker).is_err() {
                    return;
                }
                if marker[0] == b'C' {
                    return;
                }
                // The receiver can be gone during failure cleanup. That is not a
                // listener-thread failure and must not add a secondary panic.
                if arrived_tx.send((marker[0], stream)).is_err() {
                    return;
                }
            }
        });
        Self {
            address,
            arrived: arrived_rx,
        }
    }

    fn configure(&self, command: &mut Command, mode: &str) {
        command
            .env(BARRIER_ADDR, self.address.to_string())
            .env(BARRIER_MODE, mode);
    }

    fn child_command(&self, mode: &str) -> Command {
        let mut command = Command::new(bin());
        self.configure(&mut command, mode);
        command
    }

    fn wait(&self, expected: u8) -> TcpStream {
        match self.arrived.recv_timeout(WAIT) {
            Ok((actual, stream)) => {
                assert_eq!(actual, expected, "wrong lease mode reached barrier");
                stream
            }
            Err(error) => {
                // Unblock the bounded listener before failing. This cancellation
                // connection is harness cleanup, never lease-order evidence.
                if let Ok(mut cancel) = TcpStream::connect(self.address) {
                    let _ = cancel.write_all(b"C");
                }
                panic!("lease barrier was not reached before its finite deadline: {error}");
            }
        }
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child still owned")
    }

    fn finish(mut self) -> Output {
        wait_child(self.child_mut());
        self.0
            .take()
            .expect("child still owned")
            .wait_with_output()
            .expect("collect child output")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_child(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().expect("poll child status") {
            return status;
        }
        assert!(Instant::now() < deadline, "child exceeded finite deadline");
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

fn run_probe(project: &Path, action: &str) -> String {
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("lease_lifetime_child_process")
        .arg("--nocapture")
        .env(CHILD_ACTION, action)
        .env(CHILD_PROJECT, project)
        .output()
        .expect("run lease probe child");
    assert!(
        output.status.success(),
        "lease probe child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("probe utf8")
        .lines()
        .find(|line| matches!(*line, "ACQUIRED" | "TIMED_OUT"))
        .expect("probe result sentinel")
        .to_string()
}

fn assert_writer_blocked(project: &Path, context: &str) {
    assert_eq!(
        run_probe(project, "probe-exclusive"),
        "TIMED_OUT",
        "{context} must retain its shared lease through final result production"
    );
}

fn mcp_frames(project: &Path) -> Vec<u8> {
    let project = project.to_str().expect("utf8 project path");
    format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"lease-test","version":"0"}}
        }),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"tools/call",
            "params":{"name":"codegraph_search","arguments":{"query":"add","projectPath":project}}
        }),
    )
    .into_bytes()
}

fn spawn_stdio_reader(project: &Path, barrier: &LeaseBarrier) -> ChildGuard {
    let mut command = barrier.child_command("shared");
    command
        .args(["serve", "--mcp", "--path"])
        .arg(project)
        .arg("--no-watch")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn stdio MCP reader");
    child
        .stdin
        .take()
        .expect("stdio MCP stdin")
        .write_all(&mcp_frames(project))
        .expect("write stdio MCP frames");
    ChildGuard::new(child)
}

fn assert_successful_mcp_output(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"id\":2") && stdout.contains("add"),
        "{context} did not return the tool result: {stdout}"
    );
}

fn unused_loopback_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve HTTP port");
    let address = listener.local_addr().expect("reserved HTTP address");
    drop(listener);
    address
}

fn connect_http(address: SocketAddr) -> TcpStream {
    let deadline = Instant::now() + WAIT;
    loop {
        match TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            Ok(stream) => return stream,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::park_timeout(Duration::from_millis(5));
            }
            Err(error) => panic!("HTTP server did not bind before deadline: {error}"),
        }
    }
}

fn send_http_tool_call(stream: &mut TcpStream, address: SocketAddr, project: &Path) {
    stream
        .set_read_timeout(Some(WAIT))
        .expect("set HTTP read timeout");
    stream
        .set_write_timeout(Some(WAIT))
        .expect("set HTTP write timeout");
    let body = serde_json::json!({
        "jsonrpc":"2.0", "id":7, "method":"tools/call",
        "params":{"name":"codegraph_search","arguments":{"query":"add","projectPath":project}}
    })
    .to_string();
    write!(
        stream,
        "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("send HTTP MCP request");
    stream.flush().expect("flush HTTP MCP request");
}

fn wait_for_daemon(project: &Path) {
    let deadline = Instant::now() + WAIT;
    loop {
        let socket = codegraph_daemon::recorded_socket_path(project)
            .expect("resolve the recorded v2 rendezvous socket");
        if codegraph_daemon::attach_to_daemon(&socket).is_ok() {
            return;
        }
        assert!(Instant::now() < deadline, "daemon did not become ready");
        std::thread::park_timeout(Duration::from_millis(5));
    }
}

#[derive(Clone)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn test_serial_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct LeaseHookEnv {
    old_addr: Option<std::ffi::OsString>,
    old_mode: Option<std::ffi::OsString>,
}

impl LeaseHookEnv {
    fn exclusive(barrier: &LeaseBarrier) -> Self {
        let guard = Self {
            old_addr: std::env::var_os(BARRIER_ADDR),
            old_mode: std::env::var_os(BARRIER_MODE),
        };
        // SAFETY: both named tests are serialized by `test_serial_guard`; child
        // processes receive explicit env values and no other test mutates them.
        unsafe {
            std::env::set_var(BARRIER_ADDR, barrier.address.to_string());
            std::env::set_var(BARRIER_MODE, "exclusive");
        }
        guard
    }
}

impl Drop for LeaseHookEnv {
    fn drop(&mut self) {
        // SAFETY: serialized by `test_serial_guard`, as above.
        unsafe {
            match self.old_addr.take() {
                Some(value) => std::env::set_var(BARRIER_ADDR, value),
                None => std::env::remove_var(BARRIER_ADDR),
            }
            match self.old_mode.take() {
                Some(value) => std::env::set_var(BARRIER_MODE, value),
                None => std::env::remove_var(BARRIER_MODE),
            }
        }
    }
}

fn existing_metadata(path: &Path, operation: &str) -> fs::Metadata {
    fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("{operation} {} failed: {error}", path.display()))
}

fn is_regular_non_alias(metadata: &fs::Metadata) -> bool {
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    true
}

#[cfg(windows)]
fn open_sqlite_artifact(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(windows))]
fn open_sqlite_artifact(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(unix)]
fn assert_same_file_identity(
    path: &Path,
    expected: &fs::Metadata,
    actual: &fs::Metadata,
    operation: &str,
) {
    use std::os::unix::fs::MetadataExt;

    assert_eq!(
        (actual.dev(), actual.ino()),
        (expected.dev(), expected.ino()),
        "{operation} changed the fixed SQLite artifact {}",
        path.display()
    );
}

#[cfg(windows)]
fn assert_same_file_identity(
    path: &Path,
    expected: &fs::Metadata,
    actual: &fs::Metadata,
    operation: &str,
) {
    use std::os::windows::fs::MetadataExt;

    assert!(
        expected.file_attributes() == actual.file_attributes()
            && expected.creation_time() == actual.creation_time()
            && expected.last_write_time() == actual.last_write_time()
            && expected.file_size() == actual.file_size(),
        "{operation} changed the fixed SQLite artifact {}",
        path.display()
    );
}

#[cfg(not(any(unix, windows)))]
fn assert_same_file_identity(
    path: &Path,
    expected: &fs::Metadata,
    actual: &fs::Metadata,
    operation: &str,
) {
    assert_eq!(
        expected.len(),
        actual.len(),
        "{operation} changed the fixed SQLite artifact {}",
        path.display()
    );
}

#[cfg(windows)]
fn windows_file_identity(file: &fs::File) -> std::io::Result<(u64, [u8; 16])> {
    use std::os::windows::io::AsRawHandle;

    const FILE_ID_INFO_CLASS: i32 = 18;
    #[repr(C)]
    struct FileIdInfo {
        volume_serial_number: u64,
        file_id: [u8; 16],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            h_file: isize,
            file_information_class: i32,
            lp_file_information: *mut core::ffi::c_void,
            dw_buffer_size: u32,
        ) -> i32;
    }

    let mut info = FileIdInfo {
        volume_serial_number: 0,
        file_id: [0; 16],
    };
    // SAFETY: `file` owns a live handle and `info` is the exact writable
    // FILE_ID_INFO layout required by FileIdInfo.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as isize,
            FILE_ID_INFO_CLASS,
            (&mut info as *mut FileIdInfo).cast(),
            core::mem::size_of::<FileIdInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((info.volume_serial_number, info.file_id))
}

#[cfg(unix)]
fn assert_path_still_names_opened_file(path: &Path, opened: &fs::File) {
    let opened_metadata = opened.metadata().unwrap_or_else(|error| {
        panic!(
            "inspect opened SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    let path_metadata = existing_metadata(path, "reinspect SQLite artifact");
    assert!(
        is_regular_non_alias(&path_metadata),
        "SQLite artifact {} changed to an alias or other entry type during snapshot",
        path.display()
    );
    assert_same_file_identity(path, &opened_metadata, &path_metadata, "reading artifact");
}

#[cfg(windows)]
fn assert_path_still_names_opened_file(path: &Path, opened: &fs::File) {
    let path_file = open_sqlite_artifact(path).unwrap_or_else(|error| {
        panic!(
            "reopen fixed SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    let path_metadata = path_file.metadata().unwrap_or_else(|error| {
        panic!(
            "reinspect fixed SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    assert!(
        is_regular_non_alias(&path_metadata),
        "SQLite artifact {} changed to an alias or other entry type during snapshot",
        path.display()
    );
    let opened_identity = windows_file_identity(opened).unwrap_or_else(|error| {
        panic!(
            "identify opened SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    let path_identity = windows_file_identity(&path_file).unwrap_or_else(|error| {
        panic!(
            "identify fixed SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    assert_eq!(
        opened_identity,
        path_identity,
        "fixed SQLite artifact {} changed identity during snapshot",
        path.display()
    );
}

#[cfg(not(any(unix, windows)))]
fn assert_path_still_names_opened_file(path: &Path, opened: &fs::File) {
    let opened_metadata = opened.metadata().unwrap_or_else(|error| {
        panic!(
            "inspect opened SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    let path_metadata = existing_metadata(path, "reinspect SQLite artifact");
    assert!(
        is_regular_non_alias(&path_metadata),
        "SQLite artifact {} changed to an alias or other entry type during snapshot",
        path.display()
    );
    assert_same_file_identity(path, &opened_metadata, &path_metadata, "reading artifact");
}

fn sqlite_artifact_bytes(path: &Path) -> Option<Vec<u8>> {
    let initial = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("inspect SQLite artifact {} failed: {error}", path.display()),
    };
    assert!(
        is_regular_non_alias(&initial),
        "SQLite artifact {} must be a regular file, not an alias or other entry type",
        path.display()
    );

    let mut file = open_sqlite_artifact(path)
        .unwrap_or_else(|error| panic!("open SQLite artifact {} failed: {error}", path.display()));
    let opened = file.metadata().unwrap_or_else(|error| {
        panic!(
            "inspect opened SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    assert!(
        is_regular_non_alias(&opened),
        "opened SQLite artifact {} is not a regular file",
        path.display()
    );
    assert_same_file_identity(path, &initial, &opened, "opening artifact");

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("read SQLite artifact {} failed: {error}", path.display()));
    let opened_after = file.metadata().unwrap_or_else(|error| {
        panic!(
            "reinspect opened SQLite artifact {} failed: {error}",
            path.display()
        )
    });
    assert_same_file_identity(path, &opened, &opened_after, "reading artifact");

    assert_path_still_names_opened_file(path, &file);
    let bytes_len = u64::try_from(bytes.len()).expect("SQLite artifact length fits u64");
    assert_eq!(
        opened_after.len(),
        bytes_len,
        "SQLite artifact {} changed length during snapshot",
        path.display()
    );
    assert_eq!(
        opened.len(),
        bytes_len,
        "opened SQLite artifact {} changed length during snapshot",
        path.display()
    );
    Some(bytes)
}

fn sqlite_snapshot(paths: &IndexPaths) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let db = paths.current_db();
    let sidecar = |suffix: &str| {
        let mut path = db.as_os_str().to_os_string();
        path.push(suffix);
        PathBuf::from(path)
    };
    [db.clone(), sidecar("-wal"), sidecar("-shm")]
        .into_iter()
        .map(|path| (path.clone(), sqlite_artifact_bytes(&path)))
        .collect()
}

fn assert_sqlite_snapshot_unchanged(
    before: &[(PathBuf, Option<Vec<u8>>)],
    after: &[(PathBuf, Option<Vec<u8>>)],
    context: &str,
) {
    assert_eq!(before.len(), after.len(), "{context}: artifact set changed");
    for ((before_path, before_bytes), (after_path, after_bytes)) in before.iter().zip(after) {
        assert_eq!(before_path, after_path, "{context}: artifact path changed");
        assert_eq!(
            before_bytes.is_some(),
            after_bytes.is_some(),
            "{context}: artifact presence changed at {}",
            before_path.display()
        );
        assert!(
            before_bytes == after_bytes,
            "{context}: artifact bytes changed at {}",
            before_path.display()
        );
    }
}

#[test]
fn sqlite_snapshot_rejects_non_regular_artifact() {
    let project = TestProject::indexed("snapshot-non-regular");
    let paths = project.paths();
    let mut wal = paths.current_db().as_os_str().to_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    fs::create_dir(&wal).expect("stage non-regular SQLite artifact");

    let result = std::panic::catch_unwind(|| sqlite_snapshot(&paths));
    assert!(
        result.is_err(),
        "the nonmutation oracle must fail closed on a non-regular artifact"
    );
}

#[cfg(unix)]
#[test]
fn sqlite_snapshot_rejects_alias_artifact_without_following_it() {
    use std::os::unix::fs::symlink;

    let project = TestProject::indexed("snapshot-alias");
    let paths = project.paths();
    let mut wal = paths.current_db().as_os_str().to_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    let outside = project.path().join("outside-sentinel");
    fs::write(&outside, b"outside").expect("stage alias target");
    symlink(&outside, &wal).expect("stage SQLite artifact alias");

    let result = std::panic::catch_unwind(|| sqlite_snapshot(&paths));
    assert!(
        result.is_err(),
        "the nonmutation oracle must reject aliases without reading their targets"
    );
    assert_eq!(
        fs::read(&outside).expect("read untouched alias target"),
        b"outside"
    );
}

#[test]
fn reader_lease_spans_stamp_check_through_last_row() {
    let _serial = test_serial_guard();
    let project = TestProject::indexed("reader-topologies");

    // CLI query.
    {
        let barrier = LeaseBarrier::new();
        let mut command = barrier.child_command("shared");
        command
            .args(["query", "add", "--path"])
            .arg(project.path())
            .arg("--json")
            .env("CODEGRAPH_NO_DAEMON", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let reader = ChildGuard::new(command.spawn().expect("spawn CLI reader"));
        let mut release = barrier.wait(b'S');
        assert_writer_blocked(project.path(), "CLI query");
        release.write_all(b"R").expect("release CLI reader");
        let output = reader.finish();
        assert!(output.status.success(), "CLI reader failed after release");
    }

    // Direct stdio MCP.
    {
        let barrier = LeaseBarrier::new();
        let reader = spawn_stdio_reader(project.path(), &barrier);
        let mut release = barrier.wait(b'S');
        assert_writer_blocked(project.path(), "stdio MCP");
        release.write_all(b"R").expect("release stdio MCP reader");
        assert_successful_mcp_output(&reader.finish(), "stdio MCP reader");
    }

    // Daemon session reached through the real local-handshake proxy.
    {
        let barrier = LeaseBarrier::new();
        let mut daemon_command = Command::new(bin());
        daemon_command
            .args(["serve", "--mcp", "--path"])
            .arg(project.path())
            .arg("--no-watch")
            .env(codegraph_daemon::CODEGRAPH_DAEMON_INTERNAL, "1")
            .env("CODEGRAPH_NO_WATCH", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        barrier.configure(&mut daemon_command, "shared");
        let mut daemon = ChildGuard::new(daemon_command.spawn().expect("spawn daemon reader"));
        // The daemon's STARTUP shared lease (state/owner/tombstone validation held
        // across pid/socket publication) reaches the same barrier first. Release it
        // so the rendezvous can be published; the next arrival is the per-request
        // lease this test is about, which proves the two acquisitions are distinct.
        let mut startup_release = barrier.wait(b'S');
        startup_release
            .write_all(b"R")
            .expect("release the daemon startup lease");
        wait_for_daemon(project.path());

        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = SharedSink(Arc::clone(&output));
        let socket = codegraph_daemon::recorded_socket_path(project.path())
            .expect("resolve the recorded v2 rendezvous socket");
        let frames = mcp_frames(project.path());
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = codegraph_daemon::run_proxy(
                &socket,
                Some(codegraph_daemon::current_ppid()),
                Cursor::new(frames),
                sink,
            );
            let _ = done_tx.send(result);
        });

        let mut release = barrier.wait(b'S');
        assert_writer_blocked(project.path(), "daemon proxy MCP");
        release
            .write_all(b"R")
            .expect("release daemon proxy reader");
        let proxy = done_rx
            .recv_timeout(WAIT)
            .expect("proxy completed before deadline")
            .expect("proxy completed successfully");
        assert_eq!(proxy, codegraph_daemon::ProxyOutcome::Proxied);
        let proxy_output = String::from_utf8(
            output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
        .expect("proxy output utf8");
        assert!(
            proxy_output.contains("\"id\":2"),
            "proxy output: {proxy_output}"
        );
        let _ = daemon.child_mut().kill();
        let _ = daemon.child_mut().wait();
    }

    // Streamable HTTP MCP.
    {
        let barrier = LeaseBarrier::new();
        let address = unused_loopback_addr();
        let registry_dir = project.http_registry_dir();
        assert!(
            registry_dir.starts_with(project.path()),
            "HTTP registry must be owned by the fixture"
        );
        let mut command = barrier.child_command("shared");
        command
            .args(["serve", "--http", "--path"])
            .arg(project.path())
            .arg("--http-addr")
            .arg(address.to_string())
            .env("CODEGRAPH_HTTP_REGISTRY_DIR", &registry_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut reader = ChildGuard::new(command.spawn().expect("spawn HTTP MCP reader"));
        let mut stream = connect_http(address);
        send_http_tool_call(&mut stream, address, project.path());
        let mut release = barrier.wait(b'S');
        assert_writer_blocked(project.path(), "HTTP MCP");
        release.write_all(b"R").expect("release HTTP MCP reader");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read HTTP MCP response");
        assert!(response.contains("200 OK"), "HTTP response: {response}");
        assert!(response.contains("add"), "HTTP tool response: {response}");
        let _ = reader.child_mut().kill();
        let _ = reader.child_mut().wait();
        fs::remove_dir_all(&registry_dir).expect("remove fixture-owned HTTP registry");
        match fs::symlink_metadata(&registry_dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => panic!("fixture-owned HTTP registry survived explicit cleanup"),
            Err(error) => panic!("inspect cleaned HTTP registry failed: {error}"),
        }
    }

    // Status uses the same retained shared lease as ordinary reads.
    {
        let barrier = LeaseBarrier::new();
        let mut command = barrier.child_command("shared");
        command
            .args(["status", "--json"])
            .arg(project.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let reader = ChildGuard::new(command.spawn().expect("spawn status reader"));
        let mut release = barrier.wait(b'S');
        assert_writer_blocked(project.path(), "status");
        release.write_all(b"R").expect("release status reader");
        let output = reader.finish();
        assert!(output.status.success(), "status failed after release");
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("status JSON response");
        assert_eq!(value["initialized"], serde_json::Value::Bool(true));
    }
}

#[test]
fn writer_blocks_new_reads_without_sqlite_open() {
    let _serial = test_serial_guard();
    let project = TestProject::indexed("writer-watcher");
    let paths = project.paths();
    let source = project.path().join("src/math.ts");
    fs::write(
        &source,
        "export function add(a: number, b: number) { return a + b + 1; }\n",
    )
    .expect("change watched source");

    let barrier = LeaseBarrier::new();
    let _env = LeaseHookEnv::exclusive(&barrier);
    let root = project.path().to_path_buf();
    let db = paths.current_db();
    let changed = source.clone();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = codegraph_watch::sync_changed_paths(&root, &db, [changed]);
        let _ = done_tx.send(result);
    });

    // The watcher acknowledged exclusive ownership immediately after the kernel
    // lock and before Store::open_for_write can open SQLite.
    let mut release = barrier.wait(b'X');
    let before = sqlite_snapshot(&paths);
    assert_eq!(
        run_probe(project.path(), "probe-read"),
        "TIMED_OUT",
        "a new reader must not pass a held watcher writer"
    );
    let after_blocked_read = sqlite_snapshot(&paths);
    assert_sqlite_snapshot_unchanged(
        &before,
        &after_blocked_read,
        "the blocked reader must not open or mutate SQLite artifacts",
    );

    // Status contention is data, not an SQLite open or an error.
    let output = Command::new(bin())
        .args(["status", "--json"])
        .arg(project.path())
        .output()
        .expect("run busy status");
    assert!(output.status.success(), "busy status must succeed");
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("busy status JSON");
    assert_eq!(status["rebuilding"], serde_json::Value::Bool(true));
    assert_eq!(
        status["initialized"],
        serde_json::Value::Bool(false),
        "busy status cannot corroborate a readable Current index"
    );
    let after_busy_status = sqlite_snapshot(&paths);
    assert_sqlite_snapshot_unchanged(
        &before,
        &after_busy_status,
        "busy status must not open or mutate SQLite artifacts",
    );

    release.write_all(b"R").expect("release watcher writer");
    done_rx
        .recv_timeout(WAIT)
        .expect("watcher completed before deadline")
        .expect("watcher sync succeeded after release");
}

#[test]
fn lease_lifetime_child_process() {
    let Ok(action) = std::env::var(CHILD_ACTION) else {
        return;
    };
    let project = PathBuf::from(std::env::var_os(CHILD_PROJECT).expect("child project env"));
    let paths = IndexPaths::resolve(&project, None).expect("child IndexPaths");
    match action.as_str() {
        "probe-exclusive" => match IndexLease::acquire_exclusive_existing(
            &paths,
            Instant::now() + PROBE_WAIT,
            || false,
        ) {
            Ok(lease) => {
                println!("ACQUIRED");
                drop(lease);
            }
            Err(IndexLeaseError::TimedOut { .. }) => println!("TIMED_OUT"),
            Err(error) => panic!("unexpected exclusive probe error: {error}"),
        },
        "probe-read" => match Store::open_for_read(&paths, Instant::now() + PROBE_WAIT, || false) {
            Ok(store) => {
                println!("ACQUIRED");
                drop(store);
            }
            Err(StoreError::Lease(IndexLeaseError::TimedOut { .. })) => println!("TIMED_OUT"),
            Err(error) => panic!("unexpected read probe error: {error}"),
        },
        other => panic!("unknown lease-lifetime child action {other}"),
    }
}
