//! Batch M acceptance item 22 — legacy scan under a runtime extension override.
//!
//! This target executes the REAL published v0.40.4 CLI (tag `v0.40.4` /
//! commit `aba40799ecacb94515f7e1690914d2accc4c8973`), materialized and
//! digest-verified by `scripts/setup-legacy-fixture.sh` from the checked-in
//! manifest at `tests/fixtures/legacy-v0.40.4/manifest.toml`. Nothing here is
//! built from this worktree, and there is no skip path: an unavailable or
//! unverifiable fixture is a fixture-setup FAILURE.
//!
//! # The two distinct claims item 22 separates
//!
//! 1. **Source visibility is configuration-dependent.** The acceptance claim
//!    "the legacy scan produces only the expected graph" holds for the DEFAULT
//!    supported-extension configuration. An unmodified old scanner that is
//!    EXPLICITLY configured (legacy `.codegraph/codegraph.json`) to index
//!    `.json`/`.toml` will legitimately report v2 files such as
//!    `.codegraph-v2/config.toml` and `.codegraph-v2/index-state.0.json` as
//!    SOURCE. That is documented here as accepted behavior, not a defect: the
//!    user asked for those extensions.
//!
//! 2. **Storage authority is configuration-independent.** For EVERY
//!    configuration, including the override above, the old scanner writes only
//!    into its own legacy namespace and leaves every byte of the v2 namespace
//!    unchanged, and the v2 graph stays readable by the v2 reader afterwards.
//!
//! Reading a v2 artifact's TEXT as source is therefore never the same thing as
//! holding authority over v2 storage. Assertion names below keep the two apart.
//!
//! # Explicit non-claims
//!
//! * This target says nothing about deliberately pointing the OLD binary's own
//!   `CODEGRAPH_DIR` at the v2 root. The v0.40.4 binary predates the v2 state
//!   protocol, treats any configured root as a plain directory, and derives no
//!   identity-suffixed sibling; storage separation in that scenario rests on
//!   root naming/identity, which is a different acceptance item. Item 22 is
//!   scoped to the EXTENSION override, so the legacy runs below always use the
//!   legacy binary's own default `.codegraph` root.
//! * The nonmutation oracle is a sequential full-byte snapshot taken while no
//!   other codegraph process is running. It never follows aliases and fails
//!   closed on every I/O or unexpected-type error. Root, nested directories,
//!   and regular files are opened and identity-corroborated before their data
//!   becomes authoritative. Unix uses `(dev, ino)` and Windows uses raw
//!   `GetFileInformationByHandleEx(FileIdInfo)` handle identity — an EXACT
//!   identity on both platforms, never a size/timestamp approximation.
//!   Only a typed `NotFound` root yields an empty snapshot; an existing root
//!   that is an alias, a reparse point, or a non-directory fails closed.
//!   The oracle's own gates are proven by deterministic checkpoint self-tests
//!   below (static root alias, root replacement, nested-directory replacement,
//!   regular-file replacement), and the frozen-binary expectation is read from
//!   the checked-in manifest rather than duplicated here.

use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The release-asset target triple this host would execute natively, or `None`
/// when no v0.40.4 asset is pinned for it. Derived from RUNTIME host facts and
/// cross-checked against the manifest by
/// [`pinned_host_set_matches_the_fixture_manifest`].
fn native_asset_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

fn new_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_codegraph"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("codegraph-cli is under crates/")
        .to_path_buf()
}

fn mini_fixture() -> PathBuf {
    workspace_root().join("crates/codegraph-bench/fixtures/mini")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyExpectation {
    executable_sha256: String,
    version_stdout: String,
}

/// `key = "value"` from a manifest line, or `None` when the line is not that key.
fn manifest_string_value(line: &str, key: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

/// The single `[fixture]` value for `key`. Absent or duplicated is fatal: the
/// manifest is the only authority for what the legacy binary must be.
fn fixture_field(manifest: &str, key: &str) -> String {
    let mut in_fixture = false;
    let mut found: Vec<String> = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_fixture = trimmed == "[fixture]";
            continue;
        }
        if !in_fixture {
            continue;
        }
        if let Some(value) = manifest_string_value(trimmed, key) {
            found.push(value);
        }
    }
    match found.len() {
        1 => found.remove(0),
        0 => panic!("fixture manifest [fixture] is missing key '{key}'"),
        n => panic!("fixture manifest [fixture] declares key '{key}' {n} times"),
    }
}

/// One `[[asset]]` block's parsed fields. Blocks are materialized as RECORDS so
/// block cardinality survives the parse: a streaming "am I inside a matching
/// block" boolean cannot tell one matching block from two.
#[derive(Debug, Default)]
struct AssetBlock {
    targets: Vec<String>,
    executable_sha256s: Vec<String>,
}

/// Every `[[asset]]` block in `manifest`, in file order.
fn asset_blocks(manifest: &str) -> Vec<AssetBlock> {
    let mut blocks: Vec<AssetBlock> = Vec::new();
    let mut current: Option<AssetBlock> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed == "[[asset]]" {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            current = Some(AssetBlock::default());
            continue;
        }
        if trimmed.starts_with('[') {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            continue;
        }
        let Some(block) = current.as_mut() else {
            continue;
        };
        if let Some(value) = manifest_string_value(trimmed, "target") {
            block.targets.push(value);
        } else if let Some(value) = manifest_string_value(trimmed, "executable_sha256") {
            block.executable_sha256s.push(value);
        }
    }
    if let Some(block) = current.take() {
        blocks.push(block);
    }
    blocks
}

/// The `executable_sha256` of the single `[[asset]]` block whose `target` equals
/// `target`.
///
/// Block uniqueness is structural and independent of digest presence: two blocks
/// naming the same target are fatal even when only one of them carries a digest.
fn asset_executable_sha256(manifest: &str, target: &str) -> String {
    let matching = asset_blocks(manifest)
        .into_iter()
        .filter(|block| block.targets.iter().any(|value| value == target))
        .collect::<Vec<_>>();
    let block = match matching.len() {
        1 => matching.into_iter().next().expect("one matching block"),
        0 => panic!("fixture manifest has no [[asset]] block with target='{target}'"),
        n => panic!("fixture manifest declares {n} [[asset]] blocks with target='{target}'"),
    };

    let digest = match block.executable_sha256s.len() {
        1 => block
            .executable_sha256s
            .into_iter()
            .next()
            .expect("one digest"),
        0 => panic!("fixture manifest [[asset]] target='{target}' has no executable_sha256"),
        n => {
            panic!(
                "fixture manifest [[asset]] target='{target}' declares {n} executable_sha256 values"
            )
        }
    };
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "manifest executable_sha256 for {target} is not 64 lowercase hex chars: {digest:?}"
    );
    digest
}

/// The frozen expectation for THIS host, read from the checked-in manifest so
/// the manifest stays the single authority (no duplicated digests in Rust).
fn native_legacy_expectation() -> LegacyExpectation {
    let target = native_asset_target()
        .unwrap_or_else(|| panic!("no pinned legacy asset for this native host"));
    let manifest = manifest_text();
    LegacyExpectation {
        executable_sha256: asset_executable_sha256(&manifest, target),
        version_stdout: fixture_field(&manifest, "expected_version_stdout"),
    }
}

fn verify_legacy_binary(path: &Path, expectation: &LegacyExpectation) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "inspect configured legacy binary {}: {error}",
            path.display()
        )
    })?;
    if !is_regular(&metadata) {
        return Err(format!(
            "configured legacy binary is not a non-alias regular file: {}",
            path.display()
        ));
    }
    let initial_identity = identity_for_validated_path(path, &metadata).map_err(|error| {
        format!(
            "identify configured legacy binary {}: {error}",
            path.display()
        )
    })?;
    let mut file = open_no_follow(path, false)
        .map_err(|error| format!("open configured legacy binary {}: {error}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("inspect opened legacy binary {}: {error}", path.display()))?;
    if !is_regular(&opened) {
        return Err(format!(
            "opened configured legacy binary is not regular: {}",
            path.display()
        ));
    }
    let opened_identity = identity_for_file(&file)
        .map_err(|error| format!("identify opened legacy binary {}: {error}", path.display()))?;
    if opened_identity != initial_identity {
        return Err(format!(
            "configured legacy binary changed between validation and open: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("read configured legacy binary {}: {error}", path.display()))?;
    if identity_for_file(&file).map_err(|error| {
        format!(
            "reidentify legacy binary handle {}: {error}",
            path.display()
        )
    })? != initial_identity
    {
        return Err(format!(
            "configured legacy binary handle changed while reading: {}",
            path.display()
        ));
    }
    corroborate_fixed_path(path, initial_identity, EntryKind::RegularFile)
        .map_err(|error| format!("configured legacy binary changed after reading: {error}"))?;

    let observed_digest = sha256_hex(&bytes);
    if observed_digest != expectation.executable_sha256 {
        return Err(format!(
            "configured legacy binary SHA-256 mismatch for {}: expected {}, observed {}",
            path.display(),
            expectation.executable_sha256,
            observed_digest
        ));
    }

    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|error| {
            format!(
                "run configured legacy binary {} --version: {error}",
                path.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "configured legacy binary {} --version failed with {}: {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let observed_version = String::from_utf8_lossy(&output.stdout)
        .replace('\r', "")
        .trim_end_matches('\n')
        .to_string();
    if observed_version != expectation.version_stdout {
        return Err(format!(
            "configured legacy binary version mismatch for {}: expected {:?}, observed {:?}",
            path.display(),
            expectation.version_stdout,
            observed_version
        ));
    }
    corroborate_fixed_path(path, initial_identity, EntryKind::RegularFile)
        .map_err(|error| format!("configured legacy binary changed after --version: {error}"))?;
    Ok(())
}

/// Absolute path of the verified v0.40.4 executable.
///
/// CI exports `CODEGRAPH_LEGACY_BIN` from an explicit setup step; a local run
/// falls back to invoking the setup script directly. Either way the binary is
/// digest- and `--version`-verified before any test uses it, and any failure
/// panics as a fixture-setup failure rather than skipping the test.
fn legacy_bin() -> PathBuf {
    let expectation = native_legacy_expectation();
    if let Some(configured) = std::env::var_os("CODEGRAPH_LEGACY_BIN") {
        let path = PathBuf::from(configured);
        verify_legacy_binary(&path, &expectation).unwrap_or_else(|error| {
            panic!("CODEGRAPH_LEGACY_BIN failed frozen fixture verification: {error}")
        });
        return path;
    }

    let script = workspace_root().join("scripts/setup-legacy-fixture.sh");
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(workspace_root())
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "fixture setup could not run {}: {error}\n\
                 The legacy-compatibility fixture is mandatory; this is a setup failure.",
                script.display()
            )
        });
    assert!(
        output.status.success(),
        "fixture setup failed ({}). This is a FIXTURE-SETUP FAILURE, never a skipped test.\n\
         stdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let path = stdout
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or_else(|| panic!("fixture setup printed no executable path"));
    let path = PathBuf::from(path);
    verify_legacy_binary(&path, &expectation)
        .unwrap_or_else(|error| panic!("fixture setup returned an unverified binary: {error}"));
    path
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "codegraph-batchm-item22-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create item-22 test directory");
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

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create fixture destination");
    for entry in fs::read_dir(src).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy fixture file");
        }
    }
}

struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

impl Run {
    fn expect_ok(self, what: &str) -> Self {
        assert!(
            self.ok,
            "{what} must succeed.\nstdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
        self
    }
}

/// Runs a codegraph binary with a deliberately clean environment: no inherited
/// `CODEGRAPH_DIR`, no daemon, and an isolated HTTP registry.
fn run(binary: &Path, project: &Path, args: &[&str]) -> Run {
    let output = Command::new(binary)
        .current_dir(project)
        .args(args)
        .env_remove("CODEGRAPH_DIR")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_WATCH", "1")
        .env("CODEGRAPH_HTTP_REGISTRY_DIR", project)
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", binary.display()));
    Run {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        ok: output.status.success(),
    }
}

/// The root-relative paths reported by `codegraph files`, normalized to `/`.
///
/// `files` prints `  <path> (<language>, <n> symbols)`; only those lines carry a
/// path, so the parse is exact rather than a substring search.
fn reported_files(listing: &str) -> Vec<String> {
    let mut out = listing
        .lines()
        .filter_map(|line| {
            let body = line.strip_prefix("  ")?;
            if body.starts_with(' ') {
                return None;
            }
            let (path, rest) = body.rsplit_once(" (")?;
            if !rest.ends_with(')') {
                return None;
            }
            Some(path.trim().replace('\\', "/"))
        })
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Fail-closed nonmutation oracle
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Entry {
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
enum SnapshotError {
    /// A fixed path's identity stopped matching the identity captured for it, so
    /// nothing observed through that path may be trusted.
    PathChangedDuringRead(PathBuf),
    /// The snapshot root exists but is not a non-alias directory (a symlink, a
    /// Windows reparse point, or a non-directory). Never a successful snapshot:
    /// only a typed `NotFound` may yield an empty one.
    RootNotANonAliasDirectory(PathBuf),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathChangedDuringRead(path) => {
                write!(f, "path identity changed during read: {}", path.display())
            }
            Self::RootNotANonAliasDirectory(path) => write!(
                f,
                "snapshot root is not a non-alias directory: {}",
                path.display()
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    RegularFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows {
        volume_serial_number: u64,
        file_id: [u8; 16],
    },
    #[cfg(not(any(unix, windows)))]
    Portable {
        len: u64,
        modified: Option<std::time::SystemTime>,
        created: Option<std::time::SystemTime>,
    },
}

fn is_alias(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

fn is_kind(metadata: &Metadata, kind: EntryKind) -> bool {
    !is_alias(metadata)
        && match kind {
            EntryKind::Directory => metadata.file_type().is_dir(),
            EntryKind::RegularFile => metadata.file_type().is_file(),
        }
}

fn is_regular(metadata: &Metadata) -> bool {
    is_kind(metadata, EntryKind::RegularFile)
}

fn identity_for_file(file: &File) -> std::io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        Ok(FileIdentity::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;

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
        Ok(FileIdentity::Windows {
            volume_serial_number: info.volume_serial_number,
            file_id: info.file_id,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = file.metadata()?;
        Ok(FileIdentity::Portable {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

fn open_no_follow(path: &Path, directory: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const O_NOFOLLOW: i32 = 0x0002_0000;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        const O_NOFOLLOW: i32 = 0x0000_0100;
        let _ = directory;
        options.custom_flags(O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let flags = FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            };
        options.custom_flags(flags);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = directory;
    options.open(path)
}

fn identity_for_validated_path(path: &Path, metadata: &Metadata) -> std::io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = path;
        Ok(FileIdentity::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let directory = metadata.file_type().is_dir();
        let file = open_no_follow(path, directory)?;
        let opened = file.metadata()?;
        if !is_kind(
            &opened,
            if directory {
                EntryKind::Directory
            } else {
                EntryKind::RegularFile
            },
        ) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "validated path changed type before identity capture",
            ));
        }
        identity_for_file(&file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(FileIdentity::Portable {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

fn corroborate_fixed_path(
    path: &Path,
    expected: FileIdentity,
    kind: EntryKind,
) -> Result<(), SnapshotError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
        }
        Err(error) => panic!("snapshot reinspect {}: {error}", path.display()),
    };
    if !is_kind(&metadata, kind) {
        return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
    }
    let reopened = match open_no_follow(path, kind == EntryKind::Directory) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
        }
        Err(error) => panic!("snapshot reopen {}: {error}", path.display()),
    };
    let reopened_metadata = reopened
        .metadata()
        .unwrap_or_else(|error| panic!("snapshot reopened metadata {}: {error}", path.display()));
    let reopened_identity = identity_for_file(&reopened)
        .unwrap_or_else(|error| panic!("snapshot reopened identity {}: {error}", path.display()));
    if !is_kind(&reopened_metadata, kind) || reopened_identity != expected {
        return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
    }
    Ok(())
}

/// Full-byte, alias-free snapshot of `root`.
///
/// Directories are recorded as entries (so an added empty directory is visible),
/// regular files carry their COMPLETE bytes read through an opened handle,
/// and symlinks record their target without ever being followed. Every I/O
/// error and every unsupported entry kind panics, so the helper can never
/// report a false "unchanged".
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    snapshot_with_checkpoint(root, |_, _| {}).expect("snapshot path identities remain stable")
}

fn snapshot_with_checkpoint<F>(
    root: &Path,
    mut checkpoint: F,
) -> Result<BTreeMap<PathBuf, Entry>, SnapshotError>
where
    F: FnMut(&Path, SnapshotCheckpoint),
{
    fn read_regular_bytes<F>(
        path: &Path,
        initial_identity: FileIdentity,
        checkpoint: &mut F,
    ) -> Result<Vec<u8>, SnapshotError>
    where
        F: FnMut(&Path, SnapshotCheckpoint),
    {
        let mut file = match open_no_follow(path, false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
            }
            Err(error) => panic!("snapshot open {}: {error}", path.display()),
        };
        let opened = file
            .metadata()
            .unwrap_or_else(|error| panic!("snapshot opened metadata {}: {error}", path.display()));
        let opened_identity = identity_for_file(&file)
            .unwrap_or_else(|error| panic!("snapshot opened identity {}: {error}", path.display()));
        if !is_regular(&opened) || opened_identity != initial_identity {
            return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
        }
        corroborate_fixed_path(path, initial_identity, EntryKind::RegularFile)?;
        checkpoint(path, SnapshotCheckpoint::RegularHandleCorroborated);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .unwrap_or_else(|error| panic!("snapshot read {}: {error}", path.display()));
        let after_read_identity = identity_for_file(&file).unwrap_or_else(|error| {
            panic!("snapshot post-read identity {}: {error}", path.display())
        });
        if after_read_identity != initial_identity {
            return Err(SnapshotError::PathChangedDuringRead(path.to_path_buf()));
        }
        corroborate_fixed_path(path, initial_identity, EntryKind::RegularFile)?;
        Ok(bytes)
    }

    fn walk<F>(
        root: &Path,
        directory: &Path,
        initial_identity: FileIdentity,
        out: &mut BTreeMap<PathBuf, Entry>,
        checkpoint: &mut F,
    ) -> Result<(), SnapshotError>
    where
        F: FnMut(&Path, SnapshotCheckpoint),
    {
        let opened_directory = match open_no_follow(directory, true) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SnapshotError::PathChangedDuringRead(
                    directory.to_path_buf(),
                ));
            }
            Err(error) => panic!("snapshot open directory {}: {error}", directory.display()),
        };
        let opened_metadata = opened_directory.metadata().unwrap_or_else(|error| {
            panic!(
                "snapshot opened directory metadata {}: {error}",
                directory.display()
            )
        });
        let opened_identity = identity_for_file(&opened_directory).unwrap_or_else(|error| {
            panic!(
                "snapshot opened directory identity {}: {error}",
                directory.display()
            )
        });
        if !is_kind(&opened_metadata, EntryKind::Directory) || opened_identity != initial_identity {
            return Err(SnapshotError::PathChangedDuringRead(
                directory.to_path_buf(),
            ));
        }
        corroborate_fixed_path(directory, initial_identity, EntryKind::Directory)?;
        checkpoint(
            directory,
            SnapshotCheckpoint::DirectoryHandleAndPathCorroborated,
        );

        let entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("snapshot read_dir {}: {error}", directory.display()));
        let mut children = Vec::new();
        for entry in entries {
            children.push(
                entry
                    .unwrap_or_else(|error| {
                        panic!("snapshot entry in {}: {error}", directory.display())
                    })
                    .path(),
            );
        }
        corroborate_fixed_path(directory, initial_identity, EntryKind::Directory)?;
        checkpoint(
            directory,
            SnapshotCheckpoint::DirectoryEnumerationCorroborated,
        );
        children.sort();

        for path in children {
            let relative = path
                .strip_prefix(root)
                .unwrap_or_else(|error| panic!("snapshot strip {}: {error}", path.display()))
                .to_path_buf();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SnapshotError::PathChangedDuringRead(path));
                }
                Err(error) => panic!("snapshot metadata {}: {error}", path.display()),
            };
            let file_type = metadata.file_type();
            let entry = if is_kind(&metadata, EntryKind::Directory) {
                Entry::Directory
            } else if is_regular(&metadata) {
                let identity =
                    identity_for_validated_path(&path, &metadata).unwrap_or_else(|error| {
                        panic!("snapshot initial file identity {}: {error}", path.display())
                    });
                checkpoint(&path, SnapshotCheckpoint::RegularPathValidated);
                Entry::RegularFile(read_regular_bytes(&path, identity, checkpoint)?)
            } else if file_type.is_symlink() || is_alias(&metadata) {
                Entry::Symlink(fs::read_link(&path).unwrap_or_else(|error| {
                    panic!("snapshot read_link {}: {error}", path.display())
                }))
            } else {
                panic!("snapshot unsupported entry kind: {}", path.display());
            };
            assert!(
                out.insert(relative, entry).is_none(),
                "duplicate snapshot path under {}",
                root.display()
            );
            if is_kind(&metadata, EntryKind::Directory) {
                let identity =
                    identity_for_validated_path(&path, &metadata).unwrap_or_else(|error| {
                        panic!(
                            "snapshot initial directory identity {}: {error}",
                            path.display()
                        )
                    });
                checkpoint(&path, SnapshotCheckpoint::DirectoryPathValidated);
                walk(root, &path, identity, out, checkpoint)?;
            }
        }
        corroborate_fixed_path(directory, initial_identity, EntryKind::Directory)?;
        Ok(())
    }

    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => panic!("snapshot root metadata {}: {error}", root.display()),
    };
    if !is_kind(&metadata, EntryKind::Directory) {
        return Err(SnapshotError::RootNotANonAliasDirectory(root.to_path_buf()));
    }
    let identity = identity_for_validated_path(root, &metadata)
        .unwrap_or_else(|error| panic!("snapshot root identity {}: {error}", root.display()));
    checkpoint(root, SnapshotCheckpoint::DirectoryPathValidated);
    let mut out = BTreeMap::new();
    walk(root, root, identity, &mut out, &mut checkpoint)?;
    Ok(out)
}

fn assert_unchanged(
    before: &BTreeMap<PathBuf, Entry>,
    after: &BTreeMap<PathBuf, Entry>,
    what: &str,
) {
    let mut paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let changed = paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .map(|path| {
            let label = |entry: Option<&Entry>| match entry {
                None => "absent".to_string(),
                Some(Entry::Directory) => "directory".to_string(),
                Some(Entry::RegularFile(bytes)) => format!("file[{} bytes]", bytes.len()),
                Some(Entry::Symlink(target)) => format!("symlink -> {}", target.display()),
            };
            format!(
                "{}: {} => {}",
                path.display(),
                label(before.get(&path)),
                label(after.get(&path))
            )
        })
        .collect::<Vec<_>>();
    assert!(changed.is_empty(), "{what} changed entries: {changed:?}");
}

/// The checked-in fixture manifest's text.
fn manifest_text() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy-v0.40.4/manifest.toml"),
    )
    .expect("read the legacy fixture manifest")
}

/// A v2 artifact path, as the legacy scanner would report it.
const V2_CONFIG_TOML: &str = ".codegraph-v2/config.toml";
const V2_EXTENSION_JSON: &str = ".codegraph-v2/codegraph.json";
/// A symbol that exists ONLY inside the v2 database — its source file is deleted
/// before any legacy run, so any legacy graph that surfaced it could only have
/// read v2 storage.
const V2_ONLY_SYMBOL: &str = "v2OnlyTrapSymbol";

/// Item 22. Both halves in one target so the two claims cannot drift apart:
/// the DEFAULT-configuration graph claim, and the every-configuration storage
/// claim under an explicit `.json`/`.toml` extension override.
///
/// The proof runs natively on every host that has a pinned v0.40.4 asset
/// (linux/x86_64 and windows/x86_64 — the two CI jobs). A host with no pinned
/// asset cannot execute the real legacy binary at all; it says so LOUDLY and
/// asserts the manifest genuinely has no entry for it, so an unpinned pass can
/// never be mistaken for legacy coverage.
#[test]
fn legacy_scan_with_runtime_extension_override_does_not_weaken_storage_safety() {
    let Some(asset_target) = native_asset_target() else {
        let manifest = manifest_text();
        assert!(
            !manifest.contains(&format!("target_os = \"{}\"", std::env::consts::OS))
                || !manifest.contains(&format!("target_arch = \"{}\"", std::env::consts::ARCH)),
            "the manifest appears to pin {}/{} — the real item-22 proof must run here \
             instead of reporting no coverage",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        eprintln!(
            "item 22 NOT COVERED on this host: no pinned v0.40.4 asset for {}/{}. \
             The legacy proof executes the real published binary natively on \
             linux/x86_64 and windows/x86_64 only; do NOT read this pass as \
             legacy-compatibility coverage.",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return;
    };
    assert!(
        manifest_text().contains(&format!("target = \"{asset_target}\"")),
        "the fixture manifest must pin an asset for this native host ({asset_target})"
    );

    let legacy = legacy_bin();
    let dir = TestDir::new("override");
    let project = dir.path().join("mini");
    copy_tree(&mini_fixture(), &project);

    // A v2-only symbol: indexed by the CURRENT binary, then its source removed.
    // Afterwards the string lives in v2 storage and nowhere on disk as source.
    let trap_source = project.join("src").join("v2_trap.ts");
    fs::write(
        &trap_source,
        format!("export function {V2_ONLY_SYMBOL}(): number {{\n  return 42;\n}}\n"),
    )
    .expect("write v2-only trap source");
    run(&new_bin(), &project, &["init", "."]).expect_ok("v2 init");
    fs::remove_file(&trap_source).expect("remove v2-only trap source");

    let v2_root = project.join(".codegraph-v2");
    // Project-scoped v2 config/extension artifacts: exactly the TOML/JSON files
    // an extension override would claim as source.
    fs::write(v2_root.join("config.toml"), "[app]\nname = \"codegraph\"\n")
        .expect("write v2 project config");
    fs::write(
        v2_root.join("codegraph.json"),
        "{\"extensions\":{\".zz\":\"lua\"}}\n",
    )
    .expect("write v2 extension config");

    let v2_before = snapshot(&v2_root);
    assert!(
        v2_before.contains_key(Path::new("codegraph.db")),
        "setup: the v2 namespace must hold a database before the legacy runs"
    );
    let v2_db_bytes = fs::read(v2_root.join("codegraph.db")).expect("read v2 database bytes");
    assert!(
        contains_bytes(&v2_db_bytes, V2_ONLY_SYMBOL.as_bytes()),
        "setup: the v2-only symbol must be present in v2 storage"
    );
    let sources_before = snapshot(&project.join("src"));
    let tools_before = snapshot(&project.join("tools"));

    // ---------------------------------------------------------------------
    // Claim 1 — DEFAULT supported-extension configuration: only the expected
    // source graph. No legacy extension override exists yet.
    // ---------------------------------------------------------------------
    run(&legacy, &project, &["init", "."]).expect_ok("legacy init with default extensions");
    let default_listing = run(&legacy, &project, &["files"])
        .expect_ok("legacy files with default extensions")
        .stdout;
    assert_eq!(
        reported_files(&default_listing),
        vec![
            "src/app.ts".to_string(),
            "src/math.ts".to_string(),
            "tools/greeter.py".to_string(),
        ],
        "with the DEFAULT supported-extension configuration the legacy scan must \
         report only the expected source graph"
    );

    let legacy_root = project.join(".codegraph");
    assert!(
        legacy_root.join("codegraph.db").is_file(),
        "the legacy binary must write its own legacy-namespace database"
    );
    assert_unchanged(
        &v2_before,
        &snapshot(&v2_root),
        "default-configuration legacy scan",
    );

    // ---------------------------------------------------------------------
    // Claim 2 — EXPLICIT runtime extension override. The old scanner is told to
    // treat `.json`/`.toml` as source, so it MAY now report v2 artifacts as
    // source. Storage authority must not move an inch.
    // ---------------------------------------------------------------------
    fs::write(
        legacy_root.join("codegraph.json"),
        "{\"extensions\":{\".json\":\"javascript\",\".toml\":\"javascript\"}}\n",
    )
    .expect("write legacy runtime extension override");

    run(&legacy, &project, &["index", "--force", "."])
        .expect_ok("legacy index --force under the extension override");
    let override_listing = run(&legacy, &project, &["files"])
        .expect_ok("legacy files under the extension override")
        .stdout;
    let override_reported = reported_files(&override_listing);

    // DOCUMENTED, ACCEPTED source visibility: this is the whole point of item 22.
    // The old scanner was explicitly configured for these extensions, so
    // reporting v2 JSON/TOML as SOURCE is correct behavior, not a leak.
    assert!(
        override_reported.iter().any(|path| path == V2_CONFIG_TOML),
        "an explicitly configured `.toml` extension is expected to make the old \
         scanner report {V2_CONFIG_TOML} as source; reported: {override_reported:?}"
    );
    assert!(
        override_reported
            .iter()
            .any(|path| path == V2_EXTENSION_JSON),
        "an explicitly configured `.json` extension is expected to make the old \
         scanner report {V2_EXTENSION_JSON} as source; reported: {override_reported:?}"
    );
    // The expected source graph is still fully present; the override only ADDS.
    for expected in ["src/app.ts", "src/math.ts", "tools/greeter.py"] {
        assert!(
            override_reported.iter().any(|path| path == expected),
            "the override must not drop expected source {expected}; reported: \
             {override_reported:?}"
        );
    }

    // STORAGE AUTHORITY — unchanged for this configuration too.
    assert_unchanged(
        &v2_before,
        &snapshot(&v2_root),
        "extension-override legacy scan",
    );
    assert_unchanged(
        &sources_before,
        &snapshot(&project.join("src")),
        "extension-override legacy scan (source tree)",
    );
    assert_unchanged(
        &tools_before,
        &snapshot(&project.join("tools")),
        "extension-override legacy scan (tools tree)",
    );

    // Reading a v2 file's TEXT never grants v2 graph authority: the v2-only
    // symbol is absent from the legacy database and from the legacy graph, even
    // though the override let the scanner read v2 config text.
    let legacy_db_bytes =
        fs::read(legacy_root.join("codegraph.db")).expect("read legacy database bytes");
    assert!(
        !contains_bytes(&legacy_db_bytes, V2_ONLY_SYMBOL.as_bytes()),
        "the legacy database must not contain the v2-only symbol"
    );
    let legacy_query = run(&legacy, &project, &["query", V2_ONLY_SYMBOL])
        .expect_ok("legacy query for the v2-only symbol");
    // The query echoes the search term, so match on the RESULT shape instead of
    // the term: a hit prints `Search Results for "…":` followed by rows, a miss
    // prints `No results found for "…"`.
    assert!(
        legacy_query.stdout.contains("No results found"),
        "the legacy graph must not serve the v2-only symbol.\nstdout:\n{}",
        legacy_query.stdout
    );
    assert!(
        !legacy_query.stdout.contains("Search Results for"),
        "the legacy graph must return no result rows for the v2-only symbol.\nstdout:\n{}",
        legacy_query.stdout
    );

    // And the v2 graph is still fully usable by the v2 reader afterwards.
    let v2_query = run(&new_bin(), &project, &["query", V2_ONLY_SYMBOL])
        .expect_ok("v2 query after the legacy override runs");
    assert!(
        v2_query.stdout.contains("Search Results for") && v2_query.stdout.contains("v2_trap.ts"),
        "the v2 graph must still serve its own symbol after the legacy runs.\nstdout:\n{}",
        v2_query.stdout
    );
    assert_unchanged(
        &v2_before,
        &snapshot(&v2_root),
        "v2 read after the extension-override legacy scan",
    );
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// FIPS 180-4 SHA-256 over `bytes`, lowercase hex.
///
/// Test-local on purpose: the crate under test must gain no dependency for a
/// fixture check. Correctness is pinned by [`sha256_hex_matches_known_vectors`]
/// against the published NIST vectors, so the legacy-binary digest gate never
/// rests on an unverified hash.
fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let mut message = bytes.to_vec();
    let bit_length = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = String::with_capacity(64);
    for word in state {
        for byte in word.to_be_bytes() {
            out.push(char::from_digit((byte >> 4) as u32, 16).expect("hex nibble"));
            out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("hex nibble"));
        }
    }
    out
}

/// Harness self-test: the nonmutation oracle must FAIL on an equal-length
/// in-place byte change, so an "unchanged" verdict above is real byte proof and
/// not a size comparison.
#[test]
fn snapshot_detects_equal_length_byte_mutation() {
    let dir = TestDir::new("selftest");
    let root = dir.path().join("ns");
    fs::create_dir_all(root.join("nested")).expect("create nested snapshot directory");
    let file = root.join("nested").join("a.bin");
    fs::write(&file, b"AAAA").expect("write self-test bytes");

    let before = snapshot(&root);
    fs::write(&file, b"AAAB").expect("equal-length in-place mutation");
    let after = snapshot(&root);

    let failed =
        std::panic::catch_unwind(|| assert_unchanged(&before, &after, "self-test")).is_err();
    assert!(
        failed,
        "the nonmutation oracle must reject an equal-length byte mutation"
    );
}

/// Guards the native-host mapping against silent drift from the fixture
/// manifest and the two CI jobs that execute the proof.
#[test]
fn pinned_host_set_matches_the_fixture_manifest() {
    let manifest = manifest_text();
    let targets = manifest
        .lines()
        .filter_map(|line| line.trim().strip_prefix("target = "))
        .map(|value| value.trim().trim_matches('"').to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        vec![
            "x86_64-unknown-linux-musl".to_string(),
            "x86_64-pc-windows-msvc".to_string(),
        ],
        "the manifest must pin exactly the two natively-executed CI hosts"
    );
    // The runtime host mapping must agree with the manifest: a mapped host has
    // a manifest entry, and an unmapped host has none.
    match native_asset_target() {
        Some(target) => assert!(
            targets.iter().any(|pinned| pinned == target),
            "native_asset_target() returned {target}, absent from the manifest: {targets:?}"
        ),
        None => assert!(
            !manifest.contains(&format!("target_os = \"{}\"", std::env::consts::OS))
                || !manifest.contains(&format!("target_arch = \"{}\"", std::env::consts::ARCH)),
            "native_asset_target() reported no pinned asset for {}/{} while the \
             manifest pins it",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    }
}

/// The published NIST SHA-256 vectors. Without this the legacy-binary digest
/// gate would rest on an unverified hand-written hash.
#[test]
fn sha256_hex_matches_known_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    // Spans the 56-byte padding boundary, where a length-encoding slip hides.
    assert_eq!(
        sha256_hex(&[b'a'; 64]),
        "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
    );
}

/// The expectation must come from the checked-in manifest, not from constants
/// duplicated in this file.
#[test]
fn native_legacy_expectation_is_read_from_the_manifest() {
    if native_asset_target().is_none() {
        return;
    }
    let expectation = native_legacy_expectation();
    let manifest = manifest_text();
    assert!(
        manifest.contains(&expectation.executable_sha256),
        "the native executable digest must be the manifest's own value"
    );
    assert!(
        manifest.contains(&format!(
            "expected_version_stdout = \"{}\"",
            expectation.version_stdout
        )),
        "the expected version must be the manifest's own value"
    );
}

#[test]
fn manifest_asset_lookup_rejects_an_unpinned_target() {
    let manifest = manifest_text();
    let missing = std::panic::catch_unwind(|| {
        asset_executable_sha256(&manifest, "aarch64-unknown-linux-gnu")
    });
    assert!(
        missing.is_err(),
        "an unpinned target must fail loudly instead of yielding a digest"
    );
}

/// A synthetic `[[asset]]` block for the parser regressions below.
fn synthetic_asset(target: &str, digests: &[&str]) -> String {
    let mut block = format!("[[asset]]\ntarget = \"{target}\"\n");
    for digest in digests {
        block.push_str(&format!("executable_sha256 = \"{digest}\"\n"));
    }
    block
}

fn lookup_panics(manifest: &str, target: &str) -> bool {
    let manifest = manifest.to_string();
    let target = target.to_string();
    std::panic::catch_unwind(move || asset_executable_sha256(&manifest, &target)).is_err()
}

/// Both carry hex LETTERS, so `to_uppercase()` in the malformed-digest test is a
/// real change rather than a no-op on an all-digit string.
const GOOD_DIGEST: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const OTHER_DIGEST: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

/// A single well-formed block is the ONLY accepting shape, so the rejections
/// below are attributable to the defect under test and not to a broken parser.
#[test]
fn manifest_asset_lookup_accepts_exactly_one_well_formed_block() {
    let manifest = format!(
        "[fixture]\ntag = \"v0.40.4\"\n\n{}{}",
        synthetic_asset("target-a", &[GOOD_DIGEST]),
        synthetic_asset("target-b", &[OTHER_DIGEST])
    );
    assert_eq!(asset_executable_sha256(&manifest, "target-a"), GOOD_DIGEST);
    assert_eq!(asset_executable_sha256(&manifest, "target-b"), OTHER_DIGEST);
}

/// The regression this test exists for: TWO blocks name the same target and only
/// ONE carries a digest. Counting digests alone yields exactly one value and
/// silently accepts; block cardinality must reject it.
#[test]
fn manifest_asset_lookup_rejects_duplicate_target_blocks_when_only_one_has_a_digest() {
    let manifest = format!(
        "{}{}",
        synthetic_asset("target-a", &[GOOD_DIGEST]),
        synthetic_asset("target-a", &[])
    );
    assert!(
        lookup_panics(&manifest, "target-a"),
        "two blocks naming the same target must be fatal even when only one has a digest"
    );

    // Also in the other order, so the rejection is not an artifact of which
    // block happens to come first.
    let reversed = format!(
        "{}{}",
        synthetic_asset("target-a", &[]),
        synthetic_asset("target-a", &[GOOD_DIGEST])
    );
    assert!(
        lookup_panics(&reversed, "target-a"),
        "block-order must not change the duplicate-target verdict"
    );
}

#[test]
fn manifest_asset_lookup_rejects_duplicate_target_blocks_that_both_have_digests() {
    let manifest = format!(
        "{}{}",
        synthetic_asset("target-a", &[GOOD_DIGEST]),
        synthetic_asset("target-a", &[OTHER_DIGEST])
    );
    assert!(
        lookup_panics(&manifest, "target-a"),
        "two blocks naming the same target must be fatal"
    );
}

#[test]
fn manifest_asset_lookup_rejects_duplicate_digest_fields_in_one_block() {
    let manifest = synthetic_asset("target-a", &[GOOD_DIGEST, OTHER_DIGEST]);
    assert!(
        lookup_panics(&manifest, "target-a"),
        "one block declaring two digests must be fatal"
    );
}

#[test]
fn manifest_asset_lookup_rejects_a_block_without_a_digest() {
    let manifest = synthetic_asset("target-a", &[]);
    assert!(
        lookup_panics(&manifest, "target-a"),
        "a matching block with no digest must be fatal"
    );
}

#[test]
fn manifest_asset_lookup_rejects_a_malformed_digest() {
    let uppercase = GOOD_DIGEST.to_uppercase();
    assert_ne!(
        uppercase, GOOD_DIGEST,
        "the uppercase case must actually differ from the accepted digest"
    );
    for bad in [
        uppercase.as_str(),
        &GOOD_DIGEST[..63],
        &format!("{GOOD_DIGEST}a"),
        &format!("{}g", &GOOD_DIGEST[..63]),
    ] {
        let manifest = synthetic_asset("target-a", &[bad]);
        assert!(
            lookup_panics(&manifest, "target-a"),
            "a malformed digest must be fatal: {bad:?}"
        );
    }
}

/// A following `[fixture]` table must CLOSE the preceding asset block, so its
/// keys can never be attributed to that block.
#[test]
fn manifest_asset_blocks_are_closed_by_the_next_table() {
    let manifest = format!(
        "{}\n[fixture]\nexecutable_sha256 = \"{OTHER_DIGEST}\"\n",
        synthetic_asset("target-a", &[GOOD_DIGEST])
    );
    assert_eq!(
        asset_executable_sha256(&manifest, "target-a"),
        GOOD_DIGEST,
        "a key after the next table header must not join the asset block"
    );
}

/// A root that is a symlink to a real directory must be REFUSED, and the alias
/// target must be neither enumerated nor modified.
#[cfg(unix)]
#[test]
fn snapshot_refuses_a_symlinked_root_without_touching_the_target() {
    let dir = TestDir::new("rootalias");
    let target = dir.path().join("target");
    fs::create_dir_all(&target).expect("create alias target directory");
    let secret = target.join("secret.bin");
    fs::write(&secret, b"TARGET-BYTES").expect("write alias target bytes");
    let link = dir.path().join("root-link");
    std::os::unix::fs::symlink(&target, &link).expect("create root symlink");

    let outcome = snapshot_with_checkpoint(&link, |_, _| {});
    assert_eq!(
        outcome,
        Err(SnapshotError::RootNotANonAliasDirectory(link.clone())),
        "an aliased root must fail closed with the typed root error"
    );

    // The alias target is untouched: the same bytes, and the direct snapshot of
    // the real directory still sees exactly one entry.
    assert_eq!(
        fs::read(&secret).expect("reread alias target bytes"),
        b"TARGET-BYTES".to_vec(),
        "refusing an aliased root must not modify the target"
    );
    let direct = snapshot(&target);
    assert_eq!(
        direct.keys().cloned().collect::<Vec<_>>(),
        vec![PathBuf::from("secret.bin")],
        "the alias target must be unchanged in shape"
    );
}

/// A root that exists but is a regular file is refused with the same typed
/// error: only a typed `NotFound` may produce an empty snapshot.
#[test]
fn snapshot_refuses_a_non_directory_root_and_only_notfound_is_empty() {
    let dir = TestDir::new("rootkind");
    let file_root = dir.path().join("not-a-dir");
    fs::write(&file_root, b"x").expect("write non-directory root");
    assert_eq!(
        snapshot_with_checkpoint(&file_root, |_, _| {}),
        Err(SnapshotError::RootNotANonAliasDirectory(file_root)),
        "a non-directory root must fail closed"
    );

    let absent = dir.path().join("absent");
    assert_eq!(
        snapshot_with_checkpoint(&absent, |_, _| {}),
        Ok(BTreeMap::new()),
        "a typed NotFound root is the ONLY empty-snapshot case"
    );
}

/// Replaces `path` (a directory) with a fresh directory of different content,
/// deterministically and without sleeping.
fn replace_directory(path: &Path, marker: &str) {
    let stash = path.with_extension("replaced-away");
    fs::rename(path, &stash).expect("move the original directory aside");
    fs::create_dir_all(path).expect("create the replacement directory");
    fs::write(path.join(marker), b"REPLACEMENT").expect("write replacement marker");
}

/// The post-corroboration gate is real: swapping the fixed ROOT directory after
/// its handle and path were corroborated must reject the run, so children
/// collected from the replacement are never accepted.
#[test]
fn snapshot_rejects_root_replaced_after_handle_corroboration() {
    let dir = TestDir::new("rootswap");
    let root = dir.path().join("ns");
    fs::create_dir_all(&root).expect("create snapshot root");
    fs::write(root.join("original.bin"), b"ORIGINAL").expect("write original bytes");

    let mut trace: Vec<(PathBuf, SnapshotCheckpoint)> = Vec::new();
    let mut swapped = false;
    let outcome = snapshot_with_checkpoint(&root, |path, checkpoint| {
        trace.push((path.to_path_buf(), checkpoint));
        if !swapped
            && path == root
            && checkpoint == SnapshotCheckpoint::DirectoryHandleAndPathCorroborated
        {
            swapped = true;
            replace_directory(&root, "injected.bin");
        }
    });

    assert!(swapped, "the root checkpoint must have been reached");
    assert_eq!(
        outcome,
        Err(SnapshotError::PathChangedDuringRead(root.clone())),
        "a root replaced after corroboration must be rejected"
    );
    // Pins the POST-ENUMERATION gate specifically: enumeration ran against the
    // replacement, so the run must die BEFORE that checkpoint. Without the
    // post-enumeration recheck this checkpoint fires and the test fails.
    assert!(
        !trace.contains(&(
            root.clone(),
            SnapshotCheckpoint::DirectoryEnumerationCorroborated
        )),
        "the post-enumeration recheck must reject the swapped root before its \
         collected children are usable; trace: {trace:?}"
    );
}

/// Same gate one level down: a NESTED directory swapped after its own
/// corroboration must reject the run.
#[test]
fn snapshot_rejects_nested_directory_replaced_after_handle_corroboration() {
    let dir = TestDir::new("nestedswap");
    let root = dir.path().join("ns");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).expect("create nested snapshot directory");
    fs::write(nested.join("original.bin"), b"ORIGINAL").expect("write nested original bytes");

    let mut trace: Vec<(PathBuf, SnapshotCheckpoint)> = Vec::new();
    let mut swapped = false;
    let outcome = snapshot_with_checkpoint(&root, |path, checkpoint| {
        trace.push((path.to_path_buf(), checkpoint));
        if !swapped
            && path == nested
            && checkpoint == SnapshotCheckpoint::DirectoryHandleAndPathCorroborated
        {
            swapped = true;
            replace_directory(&nested, "injected.bin");
        }
    });

    assert!(swapped, "the nested checkpoint must have been reached");
    assert_eq!(
        outcome,
        Err(SnapshotError::PathChangedDuringRead(nested.clone())),
        "a nested directory replaced after corroboration must be rejected"
    );
    assert!(
        !trace.contains(&(
            nested.clone(),
            SnapshotCheckpoint::DirectoryEnumerationCorroborated
        )),
        "the post-enumeration recheck must reject the swapped nested directory \
         before its collected children are usable; trace: {trace:?}"
    );
}

/// A regular file replaced by a DIFFERENT object after its handle was
/// corroborated must be rejected by the post-read fixed-path check, so bytes
/// read through the old handle are never attributed to the new path.
#[test]
fn snapshot_rejects_regular_file_replaced_after_handle_corroboration() {
    let dir = TestDir::new("fileswap");
    let root = dir.path().join("ns");
    fs::create_dir_all(&root).expect("create snapshot root");
    let file = root.join("a.bin");
    fs::write(&file, b"ORIGINAL").expect("write original file bytes");

    let mut swapped = false;
    let outcome = snapshot_with_checkpoint(&root, |path, checkpoint| {
        if !swapped && path == file && checkpoint == SnapshotCheckpoint::RegularHandleCorroborated {
            swapped = true;
            let replacement = root.join("replacement.tmp");
            fs::write(&replacement, b"REPLACED").expect("write replacement bytes");
            fs::rename(&replacement, &file).expect("atomically replace the snapshot file");
        }
    });

    assert!(
        swapped,
        "the regular-file checkpoint must have been reached"
    );
    assert_eq!(
        outcome,
        Err(SnapshotError::PathChangedDuringRead(file.clone())),
        "a file replaced after handle corroboration must be rejected"
    );
}

/// A temporary executable that prints `line` on stdout, or `None` when this
/// platform has no dependency-free way to make one.
#[cfg(unix)]
fn write_version_stub(path: &Path, line: &str) -> Option<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::write(path, format!("#!/bin/sh\necho '{line}'\n")).expect("write version stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("mark stub executable");
    Some(())
}

#[cfg(not(unix))]
fn write_version_stub(_path: &Path, _line: &str) -> Option<()> {
    None
}

/// A wrong digest must be rejected BEFORE the candidate is ever executed.
#[test]
fn verify_legacy_binary_rejects_a_wrong_executable_digest() {
    let dir = TestDir::new("badsha");
    let candidate = dir.path().join("fake-legacy");
    fs::write(&candidate, b"not the frozen legacy binary").expect("write fake legacy binary");

    let expectation = LegacyExpectation {
        executable_sha256: "0".repeat(64),
        version_stdout: "codegraph 0.40.4".to_string(),
    };
    let error = verify_legacy_binary(&candidate, &expectation)
        .expect_err("a wrong digest must be rejected");
    assert!(
        error.contains("SHA-256 mismatch"),
        "the digest branch must be the rejection reason, got: {error}"
    );
    assert!(
        error.contains(&sha256_hex(b"not the frozen legacy binary")),
        "the observed digest must be reported, got: {error}"
    );
}

/// With the candidate's REAL digest as the expectation, verification reaches the
/// `--version` branch and rejects the wrong version. No collision is needed.
#[test]
fn verify_legacy_binary_rejects_a_wrong_executable_version() {
    let dir = TestDir::new("badversion");
    let candidate = dir.path().join("stub-legacy");
    if write_version_stub(&candidate, "codegraph 9.9.9").is_none() {
        eprintln!(
            "verify_legacy_binary version branch NOT self-tested on {}: no \
             dependency-free executable stub. Production verification still runs \
             the real `--version` natively.",
            std::env::consts::OS
        );
        return;
    }

    let bytes = fs::read(&candidate).expect("read the stub bytes");
    let expectation = LegacyExpectation {
        executable_sha256: sha256_hex(&bytes),
        version_stdout: "codegraph 0.40.4".to_string(),
    };
    let error = verify_legacy_binary(&candidate, &expectation)
        .expect_err("a wrong --version must be rejected");
    assert!(
        error.contains("version mismatch") && error.contains("codegraph 9.9.9"),
        "the version branch must be the rejection reason, got: {error}"
    );
}

/// A matching digest AND matching version is the only accepting path.
#[test]
fn verify_legacy_binary_accepts_a_matching_digest_and_version() {
    let dir = TestDir::new("goodstub");
    let candidate = dir.path().join("stub-legacy");
    if write_version_stub(&candidate, "codegraph 0.40.4").is_none() {
        return;
    }
    let bytes = fs::read(&candidate).expect("read the stub bytes");
    let expectation = LegacyExpectation {
        executable_sha256: sha256_hex(&bytes),
        version_stdout: "codegraph 0.40.4".to_string(),
    };
    verify_legacy_binary(&candidate, &expectation)
        .expect("a matching digest and version must verify");
}
