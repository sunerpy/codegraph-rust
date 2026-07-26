//! Public behavioral contract for the reusable v2 index lease capability.

use std::cell::Cell;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{
    IndexLease, IndexLeaseError, IndexLeaseValidationError, RebuildKind, Store, begin_full_rebuild,
};

const CHILD_ACTION: &str = "CODEGRAPH_INDEX_LEASE_CHILD_ACTION";
const CHILD_MODE: &str = "CODEGRAPH_INDEX_LEASE_CHILD_MODE";
const CHILD_PROJECT: &str = "CODEGRAPH_INDEX_LEASE_CHILD_PROJECT";
const LOCK_BYTES: &[u8] = b"permanent-lock-sentinel\nnot-a-pid\n";
const CHILD_WAIT: Duration = Duration::from_secs(5);
const SHORT_DEADLINE: Duration = Duration::from_millis(80);
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempProject(PathBuf);

impl TempProject {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codegraph-index-lease-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir(&path)
            .unwrap_or_else(|err| panic!("create temp project {}: {err}", path.display()));
        Self(path.canonicalize().expect("canonical temp project"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn paths(&self) -> IndexPaths {
        IndexPaths::resolve(&self.0, None).expect("resolve test IndexPaths")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|err| panic!("remove temp project {}: {err}", self.0.display()));
    }
}

fn deadline_after(duration: Duration) -> Instant {
    Instant::now()
        .checked_add(duration)
        .expect("test deadline is representable")
}

fn stage_existing_lock(paths: &IndexPaths, bytes: &[u8]) {
    std::fs::create_dir_all(paths.current_root()).expect("create current root fixture");
    let mut lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths.permanent_lock())
        .expect("create permanent lock fixture");
    lock.write_all(bytes).expect("write permanent lock fixture");
    lock.sync_all().expect("sync permanent lock fixture");
}

fn lock_bytes(paths: &IndexPaths) -> Vec<u8> {
    std::fs::read(paths.permanent_lock()).expect("read permanent lock bytes")
}

#[cfg(unix)]
#[test]
fn static_symlink_lock_is_rejected_without_using_the_external_target_as_authority() {
    use std::os::unix::fs::symlink;

    let project = TempProject::new("static-symlink");
    let paths = project.paths();
    std::fs::create_dir_all(paths.current_root()).expect("create current root fixture");
    let external = project.path().join("external.lock");
    std::fs::write(&external, LOCK_BYTES).expect("write external lock target");
    symlink(&external, paths.permanent_lock()).expect("stage malicious lock symlink");

    let result =
        IndexLease::acquire_exclusive_existing(&paths, deadline_after(CHILD_WAIT), || false);
    match result {
        Err(IndexLeaseError::AliasedLock { path }) => {
            assert_eq!(path, paths.permanent_lock());
            let external_handle = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&external)
                .expect("open external target independently");
            external_handle
                .try_lock()
                .expect("rejected alias never became this namespace's lock authority");
            external_handle.unlock().expect("unlock external target");
        }
        Ok(lease) => {
            let external_handle = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&external)
                .expect("open external target independently");
            assert!(
                matches!(
                    external_handle.try_lock(),
                    Err(std::fs::TryLockError::WouldBlock)
                ),
                "the defective lease must demonstrably hold the external target"
            );
            drop(lease);
            panic!("static symlink was accepted as permanent lock authority");
        }
        Err(other) => panic!("wrong typed error for lock alias: {other:?}"),
    }

    assert_eq!(std::fs::read(&external).unwrap(), LOCK_BYTES);
    assert!(
        std::fs::symlink_metadata(paths.permanent_lock())
            .unwrap()
            .file_type()
            .is_symlink(),
        "existing-only rejection must not mutate the alias entry"
    );
}

#[cfg(unix)]
#[test]
fn socket_lock_entry_is_rejected_as_non_regular() {
    use std::os::unix::net::UnixListener;

    let socket_project = TempProject::new("lock-socket");
    let socket_paths = socket_project.paths();
    std::fs::create_dir_all(socket_paths.current_root()).expect("create socket fixture root");
    let _listener = UnixListener::bind(socket_paths.permanent_lock()).expect("stage lock socket");
    let error =
        IndexLease::acquire_shared_existing(&socket_paths, deadline_after(CHILD_WAIT), || false)
            .expect_err("a socket cannot be permanent lock authority");
    assert!(matches!(
        error,
        IndexLeaseError::NonRegularLock {
            path,
            kind: "non-regular filesystem entry"
        } if path == socket_paths.permanent_lock()
    ));
}

#[test]
fn existing_open_missing_is_nonmutating_and_never_creates() {
    let missing_root = TempProject::new("missing-root");
    let paths = missing_root.paths();
    assert!(!paths.current_root().exists());

    let err = IndexLease::acquire_shared_existing(&paths, deadline_after(SHORT_DEADLINE), || false)
        .expect_err("ordinary existing open must reject a missing lock");
    assert!(matches!(err, IndexLeaseError::LockNotFound { .. }));
    assert!(
        !paths.current_root().exists(),
        "ordinary probe must not create the current root"
    );

    let missing_lock = TempProject::new("missing-lock");
    let paths = missing_lock.paths();
    std::fs::create_dir(paths.current_root()).expect("stage empty existing root");
    std::fs::write(paths.current_root().join("keep.bin"), b"keep").expect("stage sibling bytes");
    let before = std::fs::read(paths.current_root().join("keep.bin")).unwrap();

    let err =
        IndexLease::acquire_exclusive_existing(&paths, deadline_after(SHORT_DEADLINE), || false)
            .expect_err("ordinary existing open must reject a missing lock");
    assert!(matches!(err, IndexLeaseError::LockNotFound { .. }));
    assert!(!paths.permanent_lock().exists());
    assert_eq!(
        std::fs::read(paths.current_root().join("keep.bin")).unwrap(),
        before
    );
}

#[test]
fn existing_open_rejects_a_directory_lock_without_mutation() {
    let project = TempProject::new("directory-lock");
    let paths = project.paths();
    std::fs::create_dir_all(paths.permanent_lock()).expect("stage directory at lock path");
    let marker = paths.permanent_lock().join("keep.bin");
    std::fs::write(&marker, LOCK_BYTES).expect("stage directory marker");

    let err = IndexLease::acquire_exclusive_existing(&paths, deadline_after(CHILD_WAIT), || false)
        .expect_err("directory cannot be permanent lock authority");
    assert!(matches!(
        err,
        IndexLeaseError::NonRegularLock {
            path,
            kind: "directory"
        } if path == paths.permanent_lock()
    ));
    assert!(paths.permanent_lock().is_dir());
    assert_eq!(std::fs::read(marker).unwrap(), LOCK_BYTES);
}

#[test]
fn explicit_initial_creation_is_separate_and_never_truncates_an_existing_lock() {
    let project = TempProject::new("initial-create");
    let paths = project.paths();

    let lease = IndexLease::create_exclusive(&paths, deadline_after(CHILD_WAIT), || false)
        .expect("explicit initial creation");
    assert!(lease.is_exclusive());
    lease
        .validate_exclusive(&paths)
        .expect("new lease authorizes its exact namespace");
    drop(lease);
    assert!(paths.current_root().is_dir());
    assert!(paths.permanent_lock().is_file());

    std::fs::write(paths.permanent_lock(), LOCK_BYTES).expect("stage permanent bytes");
    let err = IndexLease::create_exclusive(&paths, deadline_after(CHILD_WAIT), || false)
        .expect_err("initial creation must not repair an existing namespace");
    assert!(matches!(
        err,
        IndexLeaseError::NamespaceAlreadyExists { .. }
    ));
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);

    let shared = IndexLease::acquire_shared_existing(&paths, deadline_after(CHILD_WAIT), || false)
        .expect("open existing lock without truncation");
    drop(shared);
    let exclusive =
        IndexLease::acquire_exclusive_existing(&paths, deadline_after(CHILD_WAIT), || false)
            .expect("open existing lock without truncation");
    drop(exclusive);
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);
}

#[test]
fn shared_processes_coexist_but_shared_blocks_exclusive() {
    let project = TempProject::new("shared-matrix");
    let paths = project.paths();
    stage_existing_lock(&paths, LOCK_BYTES);

    let holder = Holder::spawn(project.path(), "shared");
    assert_eq!(run_probe(project.path(), "shared", CHILD_WAIT), "ACQUIRED");
    assert_eq!(
        run_probe(project.path(), "exclusive", SHORT_DEADLINE),
        "TIMED_OUT"
    );
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);
    holder.release();
}

#[test]
fn exclusive_process_blocks_shared_and_exclusive() {
    let project = TempProject::new("exclusive-matrix");
    let paths = project.paths();
    stage_existing_lock(&paths, LOCK_BYTES);

    let holder = Holder::spawn(project.path(), "exclusive");
    assert_eq!(
        run_probe(project.path(), "shared", SHORT_DEADLINE),
        "TIMED_OUT"
    );
    assert_eq!(
        run_probe(project.path(), "exclusive", SHORT_DEADLINE),
        "TIMED_OUT"
    );
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);
    holder.release();

    assert_eq!(run_probe(project.path(), "shared", CHILD_WAIT), "ACQUIRED");
    assert_eq!(
        run_probe(project.path(), "exclusive", CHILD_WAIT),
        "ACQUIRED"
    );
}

#[test]
fn timeout_and_post_contention_cancellation_preserve_lock_bytes() {
    let project = TempProject::new("bounded");
    let paths = project.paths();
    stage_existing_lock(&paths, LOCK_BYTES);
    let holder = Holder::spawn(project.path(), "exclusive");

    let timeout =
        IndexLease::acquire_shared_existing(&paths, deadline_after(SHORT_DEADLINE), || false)
            .expect_err("exclusive holder must force bounded timeout");
    assert!(matches!(timeout, IndexLeaseError::TimedOut { .. }));
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);

    let checks = Cell::new(0_u8);
    let cancelled = IndexLease::acquire_shared_existing(&paths, deadline_after(CHILD_WAIT), || {
        let next = checks.get().saturating_add(1);
        checks.set(next);
        next >= 3
    })
    .expect_err("cancellation must be checked again after observing contention");
    assert!(matches!(cancelled, IndexLeaseError::Cancelled { .. }));
    assert!(
        checks.get() >= 3,
        "the cancellation check must run after a busy try_lock"
    );
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);
    holder.release();
}

#[test]
fn an_already_expired_deadline_is_nonmutating_even_when_the_lock_is_free() {
    let project = TempProject::new("expired");
    let paths = project.paths();
    stage_existing_lock(&paths, LOCK_BYTES);

    let expired = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("expired deadline is representable");
    let err = IndexLease::acquire_exclusive_existing(&paths, expired, || false)
        .expect_err("an expired deadline cannot acquire a free lock");
    assert!(matches!(err, IndexLeaseError::TimedOut { .. }));
    assert_eq!(lock_bytes(&paths), LOCK_BYTES);
}

#[test]
fn a_clone_keeps_the_single_lock_alive_until_the_final_drop() {
    let project = TempProject::new("clone");
    let paths = project.paths();
    stage_existing_lock(&paths, LOCK_BYTES);

    let lease =
        IndexLease::acquire_exclusive_existing(&paths, deadline_after(CHILD_WAIT), || false)
            .expect("acquire exclusive lease");
    let clone = lease.clone();
    drop(lease);

    assert_eq!(
        run_probe(project.path(), "shared", SHORT_DEADLINE),
        "TIMED_OUT",
        "dropping a non-final clone must not unlock"
    );
    drop(clone);
    assert_eq!(
        run_probe(project.path(), "shared", CHILD_WAIT),
        "ACQUIRED",
        "the final owner must release the kernel lock"
    );
}

#[test]
fn lease_mode_parent_and_clone_drop_order_are_enforced() {
    // Use two clones so both the original parent and a clone are observed as
    // non-final owners. Every contender is a separate process synchronized by
    // its result sentinel; no same-process lock semantics or sleeps are evidence.
    let shared_project = TempProject::new("shared-parent-clones");
    let shared_paths = shared_project.paths();
    stage_existing_lock(&shared_paths, LOCK_BYTES);
    let shared_parent =
        IndexLease::acquire_shared_existing(&shared_paths, deadline_after(CHILD_WAIT), || false)
            .expect("acquire shared parent lease");
    let shared_clone_a = shared_parent.clone();
    let shared_clone_b = shared_parent.clone();

    drop(shared_parent);
    assert_eq!(
        run_probe(shared_project.path(), "exclusive", SHORT_DEADLINE),
        "TIMED_OUT",
        "dropping a non-final shared parent must keep exclusives blocked"
    );
    drop(shared_clone_a);
    assert_eq!(
        run_probe(shared_project.path(), "exclusive", SHORT_DEADLINE),
        "TIMED_OUT",
        "dropping a non-final shared clone must keep exclusives blocked"
    );
    drop(shared_clone_b);
    assert_eq!(
        run_probe(shared_project.path(), "exclusive", SHORT_DEADLINE),
        "ACQUIRED",
        "the final shared owner must release immediately"
    );

    let exclusive_project = TempProject::new("exclusive-parent-clones");
    let exclusive_paths = exclusive_project.paths();
    stage_existing_lock(&exclusive_paths, LOCK_BYTES);
    let exclusive_parent = IndexLease::acquire_exclusive_existing(
        &exclusive_paths,
        deadline_after(CHILD_WAIT),
        || false,
    )
    .expect("acquire exclusive parent lease");
    let exclusive_clone_a = exclusive_parent.clone();
    let exclusive_clone_b = exclusive_parent.clone();

    drop(exclusive_parent);
    for mode in ["shared", "exclusive"] {
        assert_eq!(
            run_probe(exclusive_project.path(), mode, SHORT_DEADLINE),
            "TIMED_OUT",
            "dropping a non-final exclusive parent must keep {mode} contenders blocked"
        );
    }
    drop(exclusive_clone_a);
    for mode in ["shared", "exclusive"] {
        assert_eq!(
            run_probe(exclusive_project.path(), mode, SHORT_DEADLINE),
            "TIMED_OUT",
            "dropping a non-final exclusive clone must keep {mode} contenders blocked"
        );
    }
    drop(exclusive_clone_b);
    assert_eq!(
        run_probe(exclusive_project.path(), "shared", SHORT_DEADLINE),
        "ACQUIRED",
        "the final exclusive owner must release immediately"
    );

    // Build a real Current namespace exclusively through public production APIs,
    // then let Store own the final shared lease capability and both SQLite
    // handles. Store's declared field order must close those handles before its
    // retained lease drops and admits the next exclusive contender.
    let store_project = TempProject::new("current-store-final-owner");
    let store_paths = store_project.paths();
    let rebuild = begin_full_rebuild(
        &store_paths,
        RebuildKind::ExplicitInit,
        deadline_after(CHILD_WAIT),
        || false,
    )
    .expect("begin Current Store fixture rebuild");
    rebuild
        .open_store()
        .expect("open Current Store fixture writer")
        .finish()
        .expect("finish Current Store fixture");

    #[cfg(windows)]
    let replacement = {
        let replacement = store_paths.current_root().join("replacement.db");
        std::fs::copy(store_paths.current_db(), &replacement)
            .expect("stage Windows database replacement");
        replacement
    };

    let store = Store::open_for_read(&store_paths, deadline_after(CHILD_WAIT), || false)
        .expect("open Current Store with retained shared lease");
    assert_eq!(
        run_probe(store_project.path(), "exclusive", SHORT_DEADLINE),
        "TIMED_OUT",
        "a live Current Store must retain its lease through its SQLite handles"
    );
    drop(store);

    #[cfg(windows)]
    {
        // Windows refuses renaming an open SQLite database. Performing the
        // replacement synchronously after Store::drop therefore proves its two
        // connections closed before the final retained lease capability dropped.
        let retired = store_paths.current_root().join("retired.db");
        std::fs::rename(store_paths.current_db(), &retired)
            .expect("final Current Store drop closes Windows database handles");
        std::fs::rename(replacement, store_paths.current_db())
            .expect("install Windows database replacement immediately after Store drop");
    }

    assert_eq!(
        run_probe(store_project.path(), "exclusive", SHORT_DEADLINE),
        "ACQUIRED",
        "dropping the final Store owner must admit a fresh contender immediately"
    );
}

#[test]
fn writer_validation_rejects_shared_and_wrong_parent_capabilities() {
    let project_a = TempProject::new("capability-a");
    let project_b = TempProject::new("capability-b");
    let paths_a = project_a.paths();
    let paths_b = project_b.paths();
    stage_existing_lock(&paths_a, LOCK_BYTES);

    let shared =
        IndexLease::acquire_shared_existing(&paths_a, deadline_after(CHILD_WAIT), || false)
            .expect("shared lease");
    assert!(shared.is_shared());
    assert!(!shared.is_exclusive());
    assert!(shared.matches_db_parent(&paths_a));
    assert_eq!(
        shared.validate_exclusive(&paths_a),
        Err(IndexLeaseValidationError::SharedLease)
    );
    drop(shared);

    let exclusive =
        IndexLease::acquire_exclusive_existing(&paths_a, deadline_after(CHILD_WAIT), || false)
            .expect("exclusive lease");
    assert!(exclusive.is_exclusive());
    assert!(exclusive.validate_exclusive(&paths_a).is_ok());
    assert!(!exclusive.matches_db_parent(&paths_b));
    assert_eq!(
        exclusive.validate_exclusive(&paths_b),
        Err(IndexLeaseValidationError::WrongDbParent)
    );
}

/// Child-process entry point. Parent tests coordinate holder readiness through a
/// pipe before launching a contender, so lock ordering never depends on sleeps.
#[test]
fn lease_child_process() {
    let Ok(action) = std::env::var(CHILD_ACTION) else {
        return;
    };
    let project = PathBuf::from(std::env::var_os(CHILD_PROJECT).expect("child project env"));
    let mode = std::env::var(CHILD_MODE).expect("child mode env");
    let paths = IndexPaths::resolve(&project, None).expect("child resolve IndexPaths");

    let acquire = || match mode.as_str() {
        "shared" => {
            IndexLease::acquire_shared_existing(&paths, deadline_after(SHORT_DEADLINE), || false)
        }
        "exclusive" => {
            IndexLease::acquire_exclusive_existing(&paths, deadline_after(SHORT_DEADLINE), || false)
        }
        other => panic!("unknown child mode {other}"),
    };

    match action.as_str() {
        "hold" => {
            let lease = acquire().expect("holder acquires lock");
            println!("READY");
            std::io::stdout().flush().expect("flush READY");
            let mut release = [0_u8; 1];
            std::io::stdin()
                .read_exact(&mut release)
                .expect("read release byte");
            drop(lease);
            println!("RELEASED");
            std::io::stdout().flush().expect("flush RELEASED");
        }
        "probe" => match acquire() {
            Ok(lease) => {
                println!("ACQUIRED");
                drop(lease);
            }
            Err(IndexLeaseError::TimedOut { .. }) => println!("TIMED_OUT"),
            Err(err) => panic!("unexpected probe error: {err}"),
        },
        other => panic!("unknown child action {other}"),
    }
}

struct Holder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    tail: Receiver<String>,
}

impl Holder {
    fn spawn(project: &Path, mode: &str) -> Self {
        let mut child = child_command(project, mode, "hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn lock holder");
        let stdin = child.stdin.take().expect("holder stdin");
        let stdout = child.stdout.take().expect("holder stdout");
        let (ready_tx, ready_rx) = mpsc::channel();
        let (tail_tx, tail_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line).expect("read holder output");
                assert_ne!(read, 0, "holder exited before READY");
                if line.trim() == "READY" {
                    ready_tx.send(line).expect("send holder READY");
                    break;
                }
            }
            let mut tail = String::new();
            reader.read_to_string(&mut tail).expect("read holder tail");
            tail_tx.send(tail).expect("send holder tail");
        });
        let ready = ready_rx
            .recv_timeout(CHILD_WAIT)
            .expect("holder READY before finite deadline");
        assert_eq!(ready.trim(), "READY");
        Self {
            child: Some(child),
            stdin: Some(stdin),
            tail: tail_rx,
        }
    }

    fn release(mut self) {
        let mut stdin = self.stdin.take().expect("holder release stdin");
        stdin.write_all(b"x").expect("signal holder release");
        drop(stdin);
        let child = self.child.as_mut().expect("holder child");
        let status = wait_bounded(child, CHILD_WAIT);
        assert!(status.success(), "holder child failed: {status}");
        let tail = self
            .tail
            .recv_timeout(CHILD_WAIT)
            .expect("holder tail before finite deadline");
        assert!(
            tail.lines().any(|line| line == "RELEASED"),
            "holder emitted no RELEASED sentinel: {tail:?}"
        );
        self.child.take();
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn child_command(project: &Path, mode: &str, action: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg("lease_child_process")
        .arg("--nocapture")
        .env(CHILD_ACTION, action)
        .env(CHILD_MODE, mode)
        .env(CHILD_PROJECT, project);
    command
}

fn run_probe(project: &Path, mode: &str, acquisition_bound: Duration) -> String {
    let mut child = child_command(project, mode, "probe")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lock probe");
    let mut stdout = child.stdout.take().expect("probe stdout");
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = String::new();
        stdout
            .read_to_string(&mut output)
            .expect("read probe stdout");
        output_tx.send(output).expect("send probe output");
    });
    let process_bound = acquisition_bound
        .checked_add(CHILD_WAIT)
        .expect("probe process bound");
    let status = wait_bounded(&mut child, process_bound);
    assert!(status.success(), "probe child failed: {status}");
    output_rx
        .recv_timeout(CHILD_WAIT)
        .expect("probe output before finite deadline")
        .lines()
        .find(|line| matches!(*line, "ACQUIRED" | "TIMED_OUT"))
        .unwrap_or_else(|| panic!("probe emitted no result sentinel"))
        .to_string()
}

fn wait_bounded(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = deadline_after(timeout);
    loop {
        if let Some(status) = child.try_wait().expect("poll child status") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child process exceeded finite {timeout:?} bound");
        }
        std::thread::park_timeout(Duration::from_millis(5));
    }
}
