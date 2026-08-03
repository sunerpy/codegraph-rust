//! Process-level acceptance for the production full-writer lifecycle.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use codegraph_core::IndexPaths;
use codegraph_store::{
    ExtractionStatus, IndexLeaseError, RebuildError, RebuildKind, Store, begin_full_rebuild,
};

const CHILD_ACTION: &str = "CODEGRAPH_WRITER_LIFECYCLE_CHILD_ACTION";
const CHILD_PROJECT: &str = "CODEGRAPH_WRITER_LIFECYCLE_CHILD_PROJECT";
const CHILD_WAIT: Duration = Duration::from_secs(10);
const LOSER_DEADLINE: Duration = Duration::from_millis(80);
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
enum NamespaceEntry {
    Directory,
    RegularFile(Vec<u8>),
    Symlink(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotCheckpoint {
    DirectoryPathValidated,
    DirectoryHandleAndPathCorroborated,
    DirectoryEnumerationCorroborated,
    RegularPathValidated,
    RegularHandleCorroborated,
}

#[derive(Debug, PartialEq, Eq)]
enum NamespaceSnapshotError {
    PathChangedDuringRead(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

struct TempProject(PathBuf);

impl TempProject {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codegraph-writer-lifecycle-{label}-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("create temp project {}: {error}", path.display()));
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
        fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("remove temp project {}: {error}", self.0.display()));
    }
}

fn deadline_after(duration: Duration) -> Instant {
    Instant::now()
        .checked_add(duration)
        .expect("test deadline is representable")
}

fn snapshot_namespace(root: &Path) -> BTreeMap<PathBuf, NamespaceEntry> {
    snapshot_namespace_with_checkpoint(root, |_, _| {})
        .expect("snapshot namespace without path replacement")
}

/// Windows `ERROR_LOCK_VIOLATION`.
///
/// A byte-range lock taken through `File::try_lock` is ADVISORY on Unix, so an
/// unrelated read of the locked file always succeeds there. On Windows the same
/// lock is MANDATORY and any overlapping read is refused with this code. The
/// contention fixtures below snapshot a namespace whose zero-length `index.lock`
/// is exclusively held by a live HOLDER CHILD PROCESS.
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Read one already-corroborated namespace file's bytes, tolerating exactly one
/// otherwise-fatal case: a mandatory Windows byte-range lock refusing a read of
/// an EMPTY file.
///
/// `len` is the length of the very handle being read, taken from the metadata
/// this walker already corroborated against the path. Metadata queries are not
/// byte-range-locked, so the length stays observable while the lock is held, and
/// a zero-length file has exactly one possible content. Recording it as empty is
/// therefore byte-exact AND independent of whether the lock happened to be held
/// at snapshot time, so a `before`/`after` pair still compares equal across a
/// lock release. Creation, removal and kind changes stay detectable because the
/// entry is still recorded as the regular file it is. Every other read error
/// panics: an unreadable file is a real fault, and a non-empty locked file has
/// content that cannot be observed at all.
fn read_namespace_file_bytes(file: &mut File, path: &Path, len: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    match file.read_to_end(&mut bytes) {
        Ok(_) => bytes,
        Err(error)
            if cfg!(windows) && len == 0 && error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) =>
        {
            Vec::new()
        }
        Err(error) => panic!("read opened namespace file {}: {error}", path.display()),
    }
}

fn snapshot_namespace_with_checkpoint<F>(
    root: &Path,
    mut checkpoint: F,
) -> Result<BTreeMap<PathBuf, NamespaceEntry>, NamespaceSnapshotError>
where
    F: FnMut(&Path, SnapshotCheckpoint),
{
    fn corroborate_directory_path(
        directory: &Path,
        initial_identity: FileIdentity,
        stage: &str,
    ) -> Result<(), NamespaceSnapshotError> {
        let metadata = match fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(NamespaceSnapshotError::PathChangedDuringRead(
                    directory.to_path_buf(),
                ));
            }
            Err(error) => panic!(
                "reinspect namespace directory {stage} {}: {error}",
                directory.display()
            ),
        };
        if !metadata.file_type().is_dir()
            || FileIdentity::from_metadata(&metadata) != initial_identity
        {
            return Err(NamespaceSnapshotError::PathChangedDuringRead(
                directory.to_path_buf(),
            ));
        }
        Ok(())
    }

    fn walk<F>(
        root: &Path,
        directory: &Path,
        initial_identity: FileIdentity,
        snapshot: &mut BTreeMap<PathBuf, NamespaceEntry>,
        checkpoint: &mut F,
    ) -> Result<(), NamespaceSnapshotError>
    where
        F: FnMut(&Path, SnapshotCheckpoint),
    {
        let opened_directory = match File::open(directory) {
            Ok(directory) => directory,
            Err(error) => {
                corroborate_directory_path(directory, initial_identity, "after failed open")?;
                panic!("open namespace directory {}: {error}", directory.display());
            }
        };
        let opened_metadata = opened_directory.metadata().unwrap_or_else(|error| {
            panic!(
                "inspect opened namespace directory {}: {error}",
                directory.display()
            )
        });
        if !opened_metadata.file_type().is_dir()
            || FileIdentity::from_metadata(&opened_metadata) != initial_identity
        {
            return Err(NamespaceSnapshotError::PathChangedDuringRead(
                directory.to_path_buf(),
            ));
        }

        corroborate_directory_path(directory, initial_identity, "before enumeration")?;
        checkpoint(
            directory,
            SnapshotCheckpoint::DirectoryHandleAndPathCorroborated,
        );
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                corroborate_directory_path(
                    directory,
                    initial_identity,
                    "after failed enumeration",
                )?;
                panic!("read namespace directory {}: {error}", directory.display());
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(error) => {
                    corroborate_directory_path(
                        directory,
                        initial_identity,
                        "after failed entry enumeration",
                    )?;
                    panic!(
                        "read namespace entry under {}: {error}",
                        directory.display()
                    );
                }
            }
        }
        corroborate_directory_path(directory, initial_identity, "after enumeration")?;
        checkpoint(
            directory,
            SnapshotCheckpoint::DirectoryEnumerationCorroborated,
        );
        paths.sort();

        for path in paths {
            let relative = path
                .strip_prefix(root)
                .unwrap_or_else(|error| {
                    panic!(
                        "namespace entry {} escaped root {}: {error}",
                        path.display(),
                        root.display()
                    )
                })
                .to_path_buf();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                }
                Err(error) => panic!("inspect namespace entry {}: {error}", path.display()),
            };
            let file_type = metadata.file_type();
            let entry = if file_type.is_dir() {
                NamespaceEntry::Directory
            } else if file_type.is_file() {
                let initial_identity = FileIdentity::from_metadata(&metadata);
                checkpoint(&path, SnapshotCheckpoint::RegularPathValidated);
                let mut file = match File::open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                    }
                    Err(error) => panic!("open namespace file {}: {error}", path.display()),
                };
                let opened = file.metadata().unwrap_or_else(|error| {
                    panic!("inspect opened namespace file {}: {error}", path.display())
                });
                if !opened.file_type().is_file()
                    || FileIdentity::from_metadata(&opened) != initial_identity
                {
                    return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                }
                let before_read = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                    }
                    Err(error) => {
                        panic!(
                            "reinspect namespace file before read {}: {error}",
                            path.display()
                        )
                    }
                };
                if !before_read.file_type().is_file()
                    || FileIdentity::from_metadata(&before_read) != initial_identity
                {
                    return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                }

                checkpoint(&path, SnapshotCheckpoint::RegularHandleCorroborated);
                let bytes = read_namespace_file_bytes(&mut file, &path, before_read.len());
                let after_read = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                    }
                    Err(error) => {
                        panic!(
                            "reinspect namespace file after read {}: {error}",
                            path.display()
                        )
                    }
                };
                if !after_read.file_type().is_file()
                    || FileIdentity::from_metadata(&after_read) != initial_identity
                {
                    return Err(NamespaceSnapshotError::PathChangedDuringRead(path));
                }
                NamespaceEntry::RegularFile(bytes)
            } else if file_type.is_symlink() {
                NamespaceEntry::Symlink(fs::read_link(&path).unwrap_or_else(|error| {
                    panic!("read namespace symlink {}: {error}", path.display())
                }))
            } else {
                panic!("unsupported namespace entry kind at {}", path.display());
            };
            assert!(
                snapshot.insert(relative, entry).is_none(),
                "duplicate native namespace path while snapshotting {}",
                path.display()
            );
            if file_type.is_dir() {
                let identity = FileIdentity::from_metadata(&metadata);
                checkpoint(&path, SnapshotCheckpoint::DirectoryPathValidated);
                walk(root, &path, identity, snapshot, checkpoint)?;
            }
        }
        corroborate_directory_path(directory, initial_identity, "after processing entries")?;
        Ok(())
    }

    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => panic!("inspect namespace root {}: {error}", root.display()),
    };
    assert!(
        metadata.file_type().is_dir(),
        "namespace root is not a directory: {}",
        root.display()
    );
    let identity = FileIdentity::from_metadata(&metadata);
    checkpoint(root, SnapshotCheckpoint::DirectoryPathValidated);
    let mut snapshot = BTreeMap::new();
    snapshot.insert(PathBuf::new(), NamespaceEntry::Directory);
    walk(root, root, identity, &mut snapshot, &mut checkpoint)?;
    Ok(snapshot)
}

fn entry_label(entry: Option<&NamespaceEntry>) -> String {
    match entry {
        None => "missing".to_string(),
        Some(NamespaceEntry::Directory) => "directory".to_string(),
        Some(NamespaceEntry::RegularFile(bytes)) => format!("file[{} bytes]", bytes.len()),
        Some(NamespaceEntry::Symlink(target)) => format!("symlink -> {}", target.display()),
    }
}

fn assert_namespace_unchanged(
    before: &BTreeMap<PathBuf, NamespaceEntry>,
    after: &BTreeMap<PathBuf, NamespaceEntry>,
    label: &str,
) {
    let changed = before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| {
            format!(
                "{}: {} -> {}",
                path.display(),
                entry_label(before.get(path)),
                entry_label(after.get(path))
            )
        })
        .collect::<Vec<_>>();
    assert!(
        changed.is_empty(),
        "{label} changed: {}",
        changed.join(", ")
    );
}

fn stage_unrelated_namespace(project: &Path) -> PathBuf {
    let unrelated = project.join(".codegraph-unrelated");
    fs::create_dir(&unrelated).expect("create unrelated namespace");
    fs::write(
        unrelated.join("codegraph.db"),
        b"unrelated-db-sentinel\0\xff",
    )
    .expect("write unrelated DB sentinel");
    fs::create_dir(unrelated.join("empty-directory")).expect("create unrelated empty directory");
    unrelated
}

fn finish_rebuild(project: &Path) {
    let paths = IndexPaths::resolve(project, None).expect("resolve recovery IndexPaths");
    let rebuild = begin_full_rebuild(
        &paths,
        RebuildKind::ExplicitInit,
        deadline_after(CHILD_WAIT),
        || false,
    )
    .expect("fresh writer acquires released kernel lease");
    rebuild
        .open_store()
        .expect("open recovery writer")
        .finish()
        .expect("complete recovery rebuild");
}

/// Child entry point. READY is emitted only after `begin_full_rebuild` has
/// acquired the production outer exclusive lease and completed its destructive
/// prologue. The parent then controls release or deliberately kills this process.
#[test]
fn writer_lifecycle_child_process() {
    let Ok(action) = std::env::var(CHILD_ACTION) else {
        return;
    };
    let project = PathBuf::from(std::env::var_os(CHILD_PROJECT).expect("child project env"));
    let paths = IndexPaths::resolve(&project, None).expect("child resolve IndexPaths");

    match action.as_str() {
        "hold" => {
            let rebuild = begin_full_rebuild(
                &paths,
                RebuildKind::ExplicitInit,
                deadline_after(CHILD_WAIT),
                || false,
            )
            .expect("holder enters production full-writer lifecycle");
            println!("READY");
            std::io::stdout().flush().expect("flush READY");
            let mut release = [0_u8; 1];
            std::io::stdin()
                .read_exact(&mut release)
                .expect("read release byte");
            drop(rebuild);
            println!("RELEASED");
            std::io::stdout().flush().expect("flush RELEASED");
        }
        "lose" => match begin_full_rebuild(
            &paths,
            RebuildKind::ExplicitInit,
            deadline_after(LOSER_DEADLINE),
            || false,
        ) {
            Err(RebuildError::Lease(IndexLeaseError::TimedOut { .. })) => {
                println!("LEASE_TIMED_OUT")
            }
            Err(error) => panic!("losing writer failed for unrelated reason: {error:?}"),
            Ok(rebuild) => {
                drop(rebuild);
                panic!("losing writer unexpectedly acquired the production writer lease");
            }
        },
        "recover" => {
            finish_rebuild(&project);
            println!("RECOVERED_CURRENT");
        }
        other => panic!("unknown writer lifecycle child action {other}"),
    }
}

struct Holder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<Result<String, String>>>,
}

impl Holder {
    fn spawn(project: &Path) -> Self {
        let mut child = child_command(project, "hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn production writer holder");
        let stdin = child.stdin.take().expect("holder stdin");
        let stdout = child.stdout.take().expect("holder stdout");
        let (ready_tx, ready_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let read = reader
                    .read_line(&mut line)
                    .map_err(|error| format!("read holder output: {error}"))?;
                if read == 0 {
                    return Err("holder exited before READY".to_string());
                }
                if line.trim() == "READY" {
                    ready_tx
                        .send(line)
                        .map_err(|_| "holder READY receiver was dropped".to_string())?;
                    break;
                }
            }
            let mut tail = String::new();
            reader
                .read_to_string(&mut tail)
                .map_err(|error| format!("read holder tail: {error}"))?;
            Ok(tail)
        });
        let ready = match ready_rx.recv_timeout(CHILD_WAIT) {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let reader_result = reader.join();
                panic!("holder READY before finite deadline: {error}; reader={reader_result:?}");
            }
        };
        assert_eq!(ready.trim(), "READY");
        Self {
            child: Some(child),
            stdin: Some(stdin),
            reader: Some(reader),
        }
    }

    fn join_reader(&mut self) -> String {
        self.reader
            .take()
            .expect("holder reader")
            .join()
            .expect("join holder reader")
            .expect("holder reader completed")
    }

    fn release(mut self) {
        let mut stdin = self.stdin.take().expect("holder release stdin");
        stdin.write_all(b"x").expect("signal holder release");
        drop(stdin);
        let status = wait_bounded(self.child.as_mut().expect("holder child"), CHILD_WAIT);
        assert!(status.success(), "holder child failed: {status}");
        let tail = self.join_reader();
        assert!(
            tail.lines().any(|line| line == "RELEASED"),
            "holder emitted no RELEASED sentinel: {tail:?}"
        );
        self.child.take();
    }

    fn crash(mut self) -> ExitStatus {
        let child = self.child.as_mut().expect("holder child");
        child.kill().expect("send OS kill to holder");
        let status = wait_bounded(child, CHILD_WAIT);
        self.join_reader();
        self.child.take();
        self.stdin.take();
        status
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.stdin.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn child_command(project: &Path, action: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg("writer_lifecycle_child_process")
        .arg("--nocapture")
        .env(CHILD_ACTION, action)
        .env(CHILD_PROJECT, project);
    command
}

fn run_child(project: &Path, action: &str) -> (ExitStatus, String, String) {
    let mut child = child_command(project, action)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn writer child {action}: {error}"));
    let stdout = child.stdout.take().expect("child stdout");
    let stderr = child.stderr.take().expect("child stderr");
    let (output_tx, output_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout_reader = BufReader::new(stdout);
        let mut stderr_reader = BufReader::new(stderr);
        let mut stdout_text = String::new();
        let mut stderr_text = String::new();
        stdout_reader
            .read_to_string(&mut stdout_text)
            .expect("read child stdout");
        stderr_reader
            .read_to_string(&mut stderr_text)
            .expect("read child stderr");
        output_tx
            .send((stdout_text, stderr_text))
            .expect("send child output");
    });
    let status = wait_bounded(&mut child, CHILD_WAIT);
    let (stdout, stderr) = output_rx
        .recv_timeout(CHILD_WAIT)
        .expect("child output before finite deadline");
    (status, stdout, stderr)
}

fn wait_bounded(child: &mut Child, timeout: Duration) -> ExitStatus {
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

#[test]
fn namespace_oracle_detects_equal_length_content_mutation() {
    let project = TempProject::new("oracle-equal-length");
    let root = project.path().join("namespace");
    fs::create_dir(&root).expect("create oracle namespace");
    let file = root.join("payload.bin");
    fs::write(&file, b"AAAA").expect("write first equal-length payload");
    let before = snapshot_namespace(&root);
    fs::write(&file, b"BBBB").expect("write second equal-length payload");
    let after = snapshot_namespace(&root);
    let detection = std::panic::catch_unwind(|| {
        assert_namespace_unchanged(&before, &after, "oracle self-test")
    });
    assert!(
        detection.is_err(),
        "namespace oracle missed an equal-length byte mutation"
    );
}

#[test]
fn namespace_oracle_accepts_only_typed_root_absence_and_rejects_aliases() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let project = TempProject::new("oracle-root-validation");
    let missing = project.path().join("missing");
    assert!(snapshot_namespace(&missing).is_empty());

    let external = project.path().join("external");
    fs::create_dir(&external).expect("create external root target");
    fs::write(external.join("must-not-be-traversed"), b"outside")
        .expect("write external root target payload");
    let alias = project.path().join("alias");
    std::os::unix::fs::symlink(&external, &alias).expect("create root symlink");
    assert!(
        std::panic::catch_unwind(|| snapshot_namespace(&alias)).is_err(),
        "a root symlink must be rejected before traversal"
    );

    let regular = project.path().join("regular");
    fs::write(&regular, b"not-a-directory").expect("write regular root path");
    assert!(
        std::panic::catch_unwind(|| snapshot_namespace(&regular)).is_err(),
        "a regular root path must be rejected before traversal"
    );

    let invalid = project
        .path()
        .join(OsString::from_vec(b"invalid\0root".to_vec()));
    assert!(
        std::panic::catch_unwind(|| snapshot_namespace(&invalid)).is_err(),
        "a non-NotFound root metadata error must fail closed"
    );
}

#[test]
fn namespace_oracle_detects_regular_path_replaced_by_symlink_before_read() {
    let project = TempProject::new("oracle-symlink-replacement");
    let root = project.path().join("namespace");
    fs::create_dir(&root).expect("create replacement-test namespace");
    let file = root.join("payload.bin");
    fs::write(&file, b"original-namespace-bytes").expect("write original namespace file");
    let external = project.path().join("external.bin");
    fs::write(&external, b"external-target-must-not-be-authoritative")
        .expect("write external symlink target");

    let mut read_boundary_reached = false;
    let result = snapshot_namespace_with_checkpoint(&root, |path, checkpoint| {
        if path == file && checkpoint == SnapshotCheckpoint::RegularPathValidated {
            fs::remove_file(path).expect("remove validated regular path");
            std::os::unix::fs::symlink(&external, path)
                .expect("replace regular path with external symlink");
        } else if path == file && checkpoint == SnapshotCheckpoint::RegularHandleCorroborated {
            read_boundary_reached = true;
        }
    });

    assert_eq!(
        result,
        Err(NamespaceSnapshotError::PathChangedDuringRead(file)),
        "a replacement symlink must be detected explicitly rather than accepting its external target bytes"
    );
    assert!(
        !read_boundary_reached,
        "the external symlink target must be rejected before any authoritative byte read"
    );
}

fn assert_directory_replacement_rejected(
    result: Result<BTreeMap<PathBuf, NamespaceEntry>, NamespaceSnapshotError>,
    changed_path: &Path,
    external_sentinel: &Path,
) {
    match result {
        Err(error) => assert_eq!(
            error,
            NamespaceSnapshotError::PathChangedDuringRead(changed_path.to_path_buf())
        ),
        Ok(snapshot) => {
            assert!(
                !snapshot.contains_key(external_sentinel),
                "external-tree entry escaped into a successful authoritative snapshot: {}",
                external_sentinel.display()
            );
            panic!(
                "directory replacement unexpectedly produced a successful authoritative snapshot: {}",
                changed_path.display()
            );
        }
    }
}

#[test]
fn namespace_oracle_detects_root_directory_replaced_by_external_symlink() {
    let project = TempProject::new("oracle-root-directory-replacement");
    let root = project.path().join("namespace");
    fs::create_dir(&root).expect("create root replacement-test namespace");
    let external = project.path().join("external-root-directory");
    fs::create_dir(&external).expect("create external root directory");
    let sentinel_name = PathBuf::from("external-root-sentinel-must-never-be-authoritative");
    fs::write(external.join(&sentinel_name), b"outside-root-tree")
        .expect("write external root sentinel");

    let mut replaced = false;
    let result = snapshot_namespace_with_checkpoint(&root, |path, checkpoint| {
        if path == root && checkpoint == SnapshotCheckpoint::DirectoryPathValidated {
            fs::remove_dir(path).expect("remove validated root directory");
            std::os::unix::fs::symlink(&external, path)
                .expect("replace root directory with external symlink");
            replaced = true;
        }
    });

    assert!(replaced, "root replacement checkpoint was not reached");
    assert_directory_replacement_rejected(result, &root, &sentinel_name);
}

#[test]
fn namespace_oracle_detects_nested_directory_replaced_by_external_symlink() {
    let project = TempProject::new("oracle-nested-directory-replacement");
    let root = project.path().join("namespace");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).expect("create nested replacement-test namespace");
    let external = project.path().join("external-nested-directory");
    fs::create_dir(&external).expect("create external nested directory");
    let sentinel_name = PathBuf::from("external-nested-sentinel-must-never-be-authoritative");
    fs::write(external.join(&sentinel_name), b"outside-nested-tree")
        .expect("write external nested sentinel");
    let sentinel_relative = Path::new("nested").join(&sentinel_name);

    let mut replaced = false;
    let result = snapshot_namespace_with_checkpoint(&root, |path, checkpoint| {
        if path == nested && checkpoint == SnapshotCheckpoint::DirectoryHandleAndPathCorroborated {
            fs::remove_dir(path).expect("remove validated nested directory");
            std::os::unix::fs::symlink(&external, path)
                .expect("replace nested directory with external symlink");
            replaced = true;
        }
    });

    assert!(replaced, "nested replacement checkpoint was not reached");
    assert_directory_replacement_rejected(result, &nested, &sentinel_relative);
}

#[test]
fn concurrent_writers_serialize_and_loser_is_nonmutating() {
    let project = TempProject::new("concurrent-writers");
    let paths = project.paths();
    let unrelated = stage_unrelated_namespace(project.path());
    let unrelated_before = snapshot_namespace(&unrelated);

    let holder = Holder::spawn(project.path());
    assert!(matches!(
        Store::extraction_status(&paths),
        ExtractionStatus::Building { .. }
    ));
    let current_before_loser = snapshot_namespace(paths.current_root());

    let (status, stdout, stderr) = run_child(project.path(), "lose");
    assert!(
        status.success(),
        "losing writer child did not report its typed contention result: status={status}, stdout={stdout:?}, stderr={stderr:?}"
    );
    assert_eq!(
        stdout.lines().find(|line| *line == "LEASE_TIMED_OUT"),
        Some("LEASE_TIMED_OUT"),
        "losing writer must fail for exact bounded lease contention: stdout={stdout:?}, stderr={stderr:?}"
    );

    assert_namespace_unchanged(
        &current_before_loser,
        &snapshot_namespace(paths.current_root()),
        "index namespace while winner retains the production lease",
    );
    assert_namespace_unchanged(
        &unrelated_before,
        &snapshot_namespace(&unrelated),
        "unrelated namespace during writer contention",
    );
    holder.release();
    assert_namespace_unchanged(
        &unrelated_before,
        &snapshot_namespace(&unrelated),
        "unrelated namespace after winner release",
    );
}

#[test]
fn crashed_writer_releases_permanent_kernel_lease() {
    let project = TempProject::new("crashed-writer");
    let paths = project.paths();
    let unrelated = stage_unrelated_namespace(project.path());
    let unrelated_before = snapshot_namespace(&unrelated);

    let holder = Holder::spawn(project.path());
    assert!(matches!(
        Store::extraction_status(&paths),
        ExtractionStatus::Building { .. }
    ));
    let crash_status = holder.crash();
    assert_eq!(
        crash_status.signal(),
        Some(9),
        "holder must terminate abnormally by SIGKILL, bypassing Rust cleanup: {crash_status:?}"
    );
    assert_namespace_unchanged(
        &unrelated_before,
        &snapshot_namespace(&unrelated),
        "unrelated namespace immediately after holder crash",
    );

    let (status, stdout, stderr) = run_child(project.path(), "recover");
    assert!(
        status.success(),
        "fresh writer failed to recover after holder crash: status={status}, stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.lines().any(|line| line == "RECOVERED_CURRENT"),
        "fresh writer emitted no recovery sentinel: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert_eq!(Store::extraction_status(&paths), ExtractionStatus::Current);
    let readable = Store::open_for_read(&paths, deadline_after(CHILD_WAIT), || false)
        .expect("public read gate corroborates recovered Current");
    drop(readable);
    assert_namespace_unchanged(
        &unrelated_before,
        &snapshot_namespace(&unrelated),
        "unrelated namespace across crash recovery",
    );
}
