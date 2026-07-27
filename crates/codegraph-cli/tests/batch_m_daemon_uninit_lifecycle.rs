//! Batch M item 20 — daemon rendezvous lifecycle under `uninit --force`.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md` lines 590-612 and 787-793:
//!
//! * daemon startup takes a bounded SHARED index lease across state/owner/
//!   tombstone validation AND pid/socket publication, so a concurrent start that
//!   blocks on `uninit`'s exclusive lease observes the authoritative
//!   `uninitialized` state slot and the tombstone (published in that order)
//!   BEFORE it could publish any rendezvous artifact — and therefore publishes
//!   none;
//! * `uninit --force` acquires exclusive first, reclassifies, publishes the
//!   `uninitialized` slot, publishes/ensures the tombstone, and only then sends a
//!   versioned, project-identity-bound shutdown control frame that BYPASSES
//!   data-request lease acquisition. The daemon stops accepting, cancels queued/
//!   running watcher lease loops, drains, removes its owned pid/socket, and ACKs;
//!   only then does uninit remove the remaining v2 runtime children while still
//!   holding the same exclusive lease;
//! * an unresponsive daemon is fail-closed: NO pid is killed, the already
//!   published durable markers stay, and the namespace classifies recoverable
//!   `Uninitialized` (never `Corrupt`), continuable by a repeated
//!   `uninit --force`.
//!
//! Determinism. Ordering evidence never comes from a sleep: the concurrent-start
//! test publishes both durable markers while it still holds the exclusive lease,
//! and the competing daemon start is stopped at the store's test-only
//! post-acquisition SHARED-lease barrier, so the "did it publish anything?"
//! snapshot is taken at an exact checkpoint. The drain tests assert process exit
//! status and on-disk artifacts, with wall-clock only as a finite upper bound.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_daemon::{DaemonLockInfo, encode_lock_info, is_process_alive};
use codegraph_store::{
    ExtractionStatus, IndexLease, StatePhase, Store, classify, publish_index_state,
};

/// Store test-hook barrier envs (compiled into `CARGO_BIN_EXE_codegraph` only
/// for this package's test builds, never the shipped binary).
const BARRIER_ADDR: &str = "CODEGRAPH_TEST_LEASE_BARRIER_ADDR";
const BARRIER_MODE: &str = "CODEGRAPH_TEST_LEASE_BARRIER_MODE";

const WAIT: Duration = Duration::from_secs(20);
const LEASE_WAIT: Duration = Duration::from_secs(30);
const TOMBSTONE_BYTES: &[u8] = b"uninitialized\n";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli lives under crates/")
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
    /// A real `codegraph init` of the mini fixture: a genuinely `Current` v2
    /// namespace with its permanent lock, both state slots, and a database.
    fn indexed(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "codegraph-m20-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        let project = root.join("mini");
        copy_tree(
            &workspace_root().join("crates/codegraph-bench/fixtures/mini"),
            &project,
        );
        let output = Command::new(bin())
            .args(["init", project.to_str().expect("utf-8 project path")])
            .output()
            .expect("run codegraph init");
        assert!(
            output.status.success(),
            "init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self(project)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn paths(&self) -> IndexPaths {
        IndexPaths::resolve(&self.0, None).expect("resolve v2 index paths")
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        if let Some(parent) = self.0.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

/// Every rendezvous artifact path a daemon could publish: the authoritative v2
/// identities plus the LEGACY `.codegraph` spellings, so "published nothing"
/// cannot pass merely because the daemon wrote somewhere else.
fn rendezvous_candidates(project: &Path, paths: &IndexPaths) -> Vec<PathBuf> {
    let legacy = project.join(".codegraph");
    vec![
        paths.daemon_pid(),
        paths.daemon_socket(),
        paths.daemon_log(),
        legacy.join("daemon.pid"),
        legacy.join("daemon.sock"),
        legacy.join("daemon.log"),
    ]
}

fn assert_no_rendezvous(project: &Path, paths: &IndexPaths, when: &str) {
    for candidate in rendezvous_candidates(project, paths) {
        assert!(
            fs::symlink_metadata(&candidate).is_err(),
            "{when}: no daemon rendezvous artifact may exist at {}",
            candidate.display()
        );
    }
}

/// Deterministic post-acquisition lease barrier: the child stops immediately
/// after its lease is granted and corroborated, acknowledges arrival on this
/// listener, and resumes only when released.
struct LeaseBarrier {
    address: SocketAddr,
    arrived: Receiver<(u8, TcpStream)>,
}

impl LeaseBarrier {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind lease barrier listener");
        let address = listener.local_addr().expect("lease barrier address");
        let (tx, arrived) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut marker = [0_u8; 1];
                if stream.read_exact(&mut marker).is_err() {
                    return;
                }
                if marker[0] == b'C' {
                    return;
                }
                if tx.send((marker[0], stream)).is_err() {
                    return;
                }
            }
        });
        Self { address, arrived }
    }

    fn configure(&self, command: &mut Command, mode: &str) {
        command
            .env(BARRIER_ADDR, self.address.to_string())
            .env(BARRIER_MODE, mode);
    }

    fn wait(&self, expected: u8) -> TcpStream {
        match self.arrived.recv_timeout(WAIT) {
            Ok((actual, stream)) => {
                assert_eq!(actual, expected, "wrong lease mode reached the barrier");
                stream
            }
            Err(error) => {
                if let Ok(mut cancel) = TcpStream::connect(self.address) {
                    let _ = cancel.write_all(b"C");
                }
                panic!("lease barrier was not reached before its finite deadline: {error}");
            }
        }
    }

    fn release(mut stream: TcpStream) {
        stream.write_all(b"R").expect("release the barriered child");
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

    /// Wait for exit within a finite bound. The bound is an upper limit only —
    /// no assertion depends on how long the child took.
    fn wait_bounded(&mut self, label: &str) -> std::process::ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            match self.child_mut().try_wait().expect("poll child status") {
                Some(status) => return status,
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                None => {
                    let _ = self.child_mut().kill();
                    panic!("{label} did not exit within {WAIT:?}");
                }
            }
        }
    }

    fn finish(mut self, label: &str) -> Output {
        let status = self.wait_bounded(label);
        let mut child = self.0.take().expect("child still owned");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(pipe) = child.stdout.as_mut() {
            let _ = pipe.read_to_end(&mut stdout);
        }
        if let Some(pipe) = child.stderr.as_mut() {
            let _ = pipe.read_to_end(&mut stderr);
        }
        // `wait_bounded` already reaped the child through `try_wait`; std caches
        // that status, so this second wait cannot block and simply proves to the
        // reader (and to clippy) that no child is left unreaped.
        let _ = child.wait();
        Output {
            status,
            stdout,
            stderr,
        }
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

/// Spawn the daemon process the way production does when it IS the daemon:
/// `CODEGRAPH_DAEMON_INTERNAL=1 codegraph serve --mcp --path <project>`.
fn daemon_command(project: &Path) -> Command {
    let mut command = Command::new(bin());
    command
        .args(["serve", "--mcp", "--path"])
        .arg(project)
        .env(codegraph_daemon::CODEGRAPH_DAEMON_INTERNAL, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn uninit_force(project: &Path) -> Output {
    Command::new(bin())
        .args(["uninit", "--force"])
        .arg(project)
        .output()
        .expect("run codegraph uninit --force")
}

/// A live process this test owns, used ONLY as the pid recorded in an
/// unresponsive daemon's rendezvous record: the "no PID kill" assertion needs a
/// real live pid that production must leave running.
fn spawn_live_placeholder() -> Child {
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sleep");
        command.arg("120");
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "ping -n 120 127.0.0.1 > NUL"]);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the live placeholder owner process")
}

fn write_owner_record(paths: &IndexPaths, pid: u32) {
    let info = DaemonLockInfo {
        pid,
        version: env!("CARGO_PKG_VERSION").to_string(),
        socket_path: paths.daemon_socket(),
        started_at: 1,
    };
    fs::write(
        paths.daemon_pid(),
        encode_lock_info(&info).expect("encode owner record"),
    )
    .expect("write daemon owner record");
}

fn assert_recoverable_uninitialized(paths: &IndexPaths, when: &str) {
    let classification = classify(paths);
    assert_eq!(
        classification.status(),
        &ExtractionStatus::Uninitialized,
        "{when}: the namespace must classify recoverable Uninitialized, not Corrupt"
    );
    assert_eq!(
        Store::extraction_status(paths),
        ExtractionStatus::Uninitialized,
        "{when}: the visible status must agree"
    );
    assert!(
        paths.tombstone().is_file(),
        "{when}: the tombstone must remain published"
    );
    assert!(
        paths.permanent_lock().is_file(),
        "{when}: the permanent index.lock is never removed"
    );
    assert!(
        paths.state_slots().iter().all(|slot| slot.is_file()),
        "{when}: both state slots survive"
    );
}

/// Whether a recorded rendezvous name is bound. On Unix the socket IS a
/// filesystem path; on Windows it is a bare namespaced pipe name (the daemon's
/// `GenericNamespaced` identity), which has no filesystem existence, so the
/// published owner record is the only observable there.
fn recorded_rendezvous_is_bound(socket: &Path) -> bool {
    #[cfg(unix)]
    {
        socket.exists()
    }
    #[cfg(not(unix))]
    {
        !socket.as_os_str().is_empty()
    }
}

fn wait_for_owner_record(paths: &IndexPaths) -> u32 {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if let Ok(raw) = fs::read_to_string(paths.daemon_pid())
            && let Some(info) = codegraph_daemon::decode_lock_info(&raw)
            && info.pid > 0
            && recorded_rendezvous_is_bound(&info.socket_path)
        {
            return info.pid;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "the daemon never published its v2 rendezvous at {}",
        paths.daemon_pid().display()
    );
}

#[test]
fn daemon_start_during_uninit_observes_uninitialized_and_tombstone_before_publish() {
    let project = TestProject::indexed("concurrent-start");
    let paths = project.paths();
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    assert_no_rendezvous(project.path(), &paths, "before any daemon start");

    let barrier = LeaseBarrier::start();

    // The competing daemon start can only observe the namespace AFTER this
    // exclusive lease is released, so everything published below is ordered
    // before its first observation without any timing assumption.
    let uninit_lease =
        IndexLease::acquire_exclusive_existing(&paths, Instant::now() + LEASE_WAIT, || false)
            .expect("uninit acquires the exclusive lease first");

    let mut command = daemon_command(project.path());
    barrier.configure(&mut command, "shared");
    let daemon = ChildGuard::new(command.spawn().expect("spawn the competing daemon start"));

    // The authoritative durable markers, in the protocol order: the
    // `uninitialized` state slot first, then the tombstone.
    publish_index_state(&paths, &uninit_lease, StatePhase::Uninitialized)
        .expect("publish the authoritative uninitialized slot");
    assert_eq!(
        Store::extraction_status(&paths),
        ExtractionStatus::Uninitialized
    );
    fs::write(paths.tombstone(), TOMBSTONE_BYTES).expect("publish the tombstone");
    assert_no_rendezvous(
        project.path(),
        &paths,
        "while uninit still holds the exclusive lease",
    );

    drop(uninit_lease);

    // Deterministic checkpoint: the child has just been granted its startup
    // SHARED lease. Nothing may have been published at or before this point.
    let released = barrier.wait(b'S');
    assert_no_rendezvous(
        project.path(),
        &paths,
        "at the daemon's own startup shared-lease checkpoint",
    );
    LeaseBarrier::release(released);

    let output = daemon.finish("the competing daemon start");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "a daemon start must refuse an uninitialized namespace; stderr: {stderr}"
    );
    assert!(
        stderr.contains("uninitialized"),
        "the refusal must name the observed uninitialized state: {stderr}"
    );
    assert!(
        stderr.contains("tombstone"),
        "the refusal must name the observed tombstone: {stderr}"
    );
    assert_no_rendezvous(
        project.path(),
        &paths,
        "after the refused daemon start exited",
    );
    assert_recoverable_uninitialized(&paths, "after the refused daemon start");
}

#[test]
fn uninit_shutdown_control_drains_without_pid_kill() {
    let project = TestProject::indexed("shutdown-drain");
    let paths = project.paths();

    let mut daemon = ChildGuard::new(
        daemon_command(project.path())
            .spawn()
            .expect("spawn daemon"),
    );
    let daemon_pid = wait_for_owner_record(&paths);
    assert!(
        is_process_alive(daemon_pid),
        "the published owner pid must be live before uninit runs"
    );

    // `uninit --force` holds the ONE exclusive lease for its whole lifecycle, so
    // a shutdown control frame that took a data-request shared lease could never
    // complete: this command succeeding IS the bypass evidence.
    let output = uninit_force(project.path());
    assert!(
        output.status.success(),
        "uninit --force must drain the live daemon and complete; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let status = daemon.wait_bounded("the drained daemon");
    assert_eq!(
        status.code(),
        Some(0),
        "the daemon must exit gracefully after ACKing its drain"
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(
            status.signal(),
            None,
            "no PID may be signalled: the daemon exits on its own control frame"
        );
    }

    assert!(
        fs::symlink_metadata(paths.daemon_pid()).is_err(),
        "the daemon removes its own pid record before ACKing"
    );
    assert!(
        fs::symlink_metadata(paths.daemon_socket()).is_err(),
        "the daemon removes its own socket before ACKing"
    );
    assert!(
        !paths.current_db().is_file(),
        "uninit removes the v2 database after the ACK, under the same lease"
    );
    assert_recoverable_uninitialized(&paths, "after a drained uninit");
}

/// A daemon that was KILLED rather than closed leaves its SQLite `-wal`/`-shm`
/// sidecars behind on an otherwise untouched `Current` namespace. The startup gate
/// must still admit a replacement daemon, because refusing those sidecars would
/// make the namespace unstartable after any hard kill — while every other part of
/// the `Current` contract stays enforced (proved by the tombstone case below, which
/// is refused with the very same sidecars present).
#[test]
fn a_killed_daemons_live_sidecars_do_not_block_a_replacement_start() {
    let project = TestProject::indexed("live-sidecars");
    let paths = project.paths();

    let sidecars: Vec<PathBuf> = ["-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut path = paths.current_db().into_os_string();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect();
    for sidecar in &sidecars {
        fs::write(sidecar, b"").expect("plant a killed daemon's live sidecar");
    }

    let mut daemon = ChildGuard::new(
        daemon_command(project.path())
            .spawn()
            .expect("spawn daemon"),
    );
    let pid = wait_for_owner_record(&paths);
    assert!(
        is_process_alive(pid),
        "a replacement daemon must start despite a killed predecessor's sidecars"
    );

    // The same namespace WITH those sidecars is still refused once it carries a
    // tombstone, so this relaxation did not weaken the state contract.
    let output = uninit_force(project.path());
    assert!(
        output.status.success(),
        "uninit --force must drain the replacement daemon; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(daemon.wait_bounded("the drained daemon").code(), Some(0));
    assert_recoverable_uninitialized(&paths, "after draining the replacement daemon");

    let refused = ChildGuard::new(
        daemon_command(project.path())
            .spawn()
            .expect("spawn daemon"),
    )
    .finish("the refused daemon start");
    let stderr = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(
        !refused.status.success() && stderr.contains("tombstone"),
        "a tombstoned namespace must still refuse a daemon start: {stderr}"
    );
    assert_no_rendezvous(
        project.path(),
        &paths,
        "after the tombstoned start was refused",
    );
}

#[test]
fn unresponsive_daemon_leaves_recoverable_uninitialized_without_kill() {
    let project = TestProject::indexed("unresponsive");
    let paths = project.paths();

    // A live owner pid with an unreachable rendezvous socket: the daemon can
    // never ACK, so uninit must fail closed.
    let mut placeholder = spawn_live_placeholder();
    let owner_pid = placeholder.id();
    write_owner_record(&paths, owner_pid);
    assert!(fs::symlink_metadata(paths.daemon_socket()).is_err());

    let output = uninit_force(project.path());
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "an unresponsive daemon must make uninit fail closed; stderr: {stderr}"
    );
    assert!(
        stderr.contains("daemon"),
        "the incomplete-uninit error must name the undrained daemon: {stderr}"
    );

    assert!(
        is_process_alive(owner_pid),
        "no PID is killed: the recorded owner process must still be running"
    );
    assert_recoverable_uninitialized(&paths, "after a fail-closed uninit");
    assert!(
        paths.current_db().is_file(),
        "the fail-closed pass must NOT remove runtime children"
    );
    assert!(
        paths.daemon_pid().is_file(),
        "the undrained owner record is preserved for the continuation"
    );

    // Continuation: once the owner is gone, a repeated `uninit --force` resumes
    // the same cleanup idempotently under a fresh lease and reclassification.
    let _ = placeholder.kill();
    let _ = placeholder.wait();
    let deadline = Instant::now() + WAIT;
    while is_process_alive(owner_pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }

    let retry = uninit_force(project.path());
    assert!(
        retry.status.success(),
        "a repeated uninit --force must resume cleanup; stderr: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(!paths.current_db().is_file());
    assert!(fs::symlink_metadata(paths.daemon_pid()).is_err());
    assert_recoverable_uninitialized(&paths, "after the uninit continuation");
}
