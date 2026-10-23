//! What resolution reads of a project's control files, which are no source
//! files (W5-06): the JS/TS path aliases, workspace and package manifests,
//! the C# project files and the Cargo manifests, and which frameworks the
//! project uses.
//!
//! Each reader reports what it read or probed in the same traversal that
//! reads it ([`codegraph_resolve::path_aliases::alias_config_inputs`],
//! [`codegraph_resolve::workspace_packages::workspace_inputs`],
//! [`codegraph_resolve::package_manifests::manifest_inputs`],
//! [`codegraph_resolve::csharp_types::csharp_project_inputs`],
//! [`codegraph_resolve::rust_scope::rust_manifest_inputs`]); every full index,
//! migration and sync writes one row per kind holding a digest of each input
//! (a file's text, a directory's listing, `null` for a missing one), and one
//! row naming the detected frameworks. A sync re-reads a kind's inputs (the
//! CLI's full sync always, the watcher's when a changed path is one of them,
//! names a control file, is a folder coming into a workspace glob's listing,
//! or changes which files the kind's readers start from) and compares: a kind
//! that moved re-resolves every file of its languages whole; a framework that
//! came or went, or a manifest edit that moves which files a framework that
//! runs only in its own apps extracts, re-extracts every file, since
//! framework nodes are made at extraction. The rows are written last;
//! `control_inputs_version` is withdrawn and restored with the other
//! markers, and `frameworks_version` only by a sync that re-extracts for a
//! framework change.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use codegraph_core::node_id::hash_content;
use codegraph_core::types::{FileRecord, Language};
use codegraph_resolve::module_surface::has_module_surface;
use codegraph_resolve::workspace_packages::{WorkspaceInput, member_listing};
use codegraph_store::Store;

/// Metadata key present when no sync left a control-file change unresolved.
pub const VERSION_KEY: &str = "control_inputs_version";
/// Value of [`VERSION_KEY`].
pub const VERSION: &str = "1";
/// Metadata key prefix of a kind's row.
pub const ROW_KEY_PREFIX: &str = "control_inputs:";
/// Metadata key of the detected frameworks, sorted, as JSON, then the digest
/// of which files the frameworks that run only in their own apps extract
/// ([`codegraph_resolve::ReferenceResolver::framework_signature`]).
pub const FRAMEWORKS_KEY: &str = "frameworks_detected";
/// Metadata key present when no sync left a framework change unextracted.
pub const FRAMEWORKS_VERSION_KEY: &str = "frameworks_version";
/// Value of [`FRAMEWORKS_VERSION_KEY`].
pub const FRAMEWORKS_VERSION: &str = "1";

/// A kind of control input, by the languages whose resolution reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ControlKind {
    Js,
    Csharp,
    Rust,
}

impl ControlKind {
    pub const ALL: [Self; 3] = [Self::Js, Self::Csharp, Self::Rust];

    fn key(self) -> &'static str {
        match self {
            Self::Js => "js",
            Self::Csharp => "csharp",
            Self::Rust => "rust",
        }
    }

    /// The `project_metadata` key of this kind's row.
    pub fn row_key(self) -> String {
        format!("{ROW_KEY_PREFIX}{}", self.key())
    }

    /// Whether a file named `name` is a control file of this kind.
    fn names_file(self, name: &str) -> bool {
        match self {
            Self::Js => matches!(
                name,
                "package.json"
                    | "deno.json"
                    | "deno.jsonc"
                    | "pnpm-workspace.yaml"
                    | "tsconfig.json"
                    | "jsconfig.json"
                    | "tsconfig.base.json"
            ),
            Self::Csharp => {
                let lower = name.to_ascii_lowercase();
                lower.ends_with(".csproj") || lower.ends_with(".props")
            }
            Self::Rust => name == "Cargo.toml",
        }
    }

    /// Whether a file of `language` at `path` is one this kind's readers
    /// start from.
    fn reads_from(self, path: &str, language: Language) -> bool {
        match self {
            Self::Js => has_module_surface(path, language),
            Self::Csharp => language == Language::CSharp,
            Self::Rust => path.ends_with(".rs"),
        }
    }
}

/// The files a Tauri project is detected by, though they are no source
/// files (`frameworks::tauri`'s config markers).
const TAURI_MARKERS: [&str; 4] = [
    "src-tauri/tauri.conf.json",
    "src-tauri/tauri.conf.json5",
    "tauri.conf.json",
    "tauri.conf.json5",
];

/// Whether `relative` is one of [`TAURI_MARKERS`].
pub fn is_tauri_marker(relative: &str) -> bool {
    TAURI_MARKERS.contains(&relative)
}

/// Whether `relative` names a control file of any kind, a Tauri marker, or
/// a Dart pubspec manifest (the Dart library lane's): the watcher hands its
/// events to the sync, though it is no source file.
pub fn is_control_file(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    is_tauri_marker(relative)
        || ControlKind::ALL.iter().any(|kind| kind.names_file(name))
        || crate::dart_libraries::is_pubspec_path(relative)
}

/// A kind's inputs and their digests, by key: a project path for an input
/// inside the project, the absolute path a reader used for one outside it,
/// and `dir:` before a directory listing's.
pub type Digests = BTreeMap<String, Option<String>>;

/// `path` with `.` dropped and each `..` taking the component before it,
/// lexically (no link is followed).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path`'s key: its project path when it is lexically inside
/// `project_root`, else the absolute path the reader used.
fn key_of(project_root: &Path, path: &Path) -> String {
    let path = lexical(path);
    match path.strip_prefix(lexical(project_root)) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

/// A file's digest: its whole text's sha256, read whatever its size, as its
/// readers read it whole; `None` when it is no readable regular file.
fn file_digest(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .read_to_end(&mut bytes)
        .ok()?;
    Some(hash_content(&String::from_utf8_lossy(&bytes)))
}

/// A listing's digest: the names, one per line; `None` for no directory.
fn listing_digest(dir: &Path, names: &[String]) -> Option<String> {
    dir.is_dir().then(|| hash_content(&names.join("\n")))
}

/// What `kind`'s readers read for the project at `project_root` whose
/// indexed files are `files`, with each input's digest, read now.
pub fn digests_of(kind: ControlKind, project_root: &Path, files: &[FileRecord]) -> Digests {
    let root = project_root.to_string_lossy().to_string();
    let mut out = Digests::new();
    match kind {
        ControlKind::Js => {
            for path in codegraph_resolve::path_aliases::alias_config_inputs(&root) {
                out.insert(key_of(project_root, &path), file_digest(&path));
            }
            for input in codegraph_resolve::workspace_packages::workspace_inputs(&root) {
                match input {
                    WorkspaceInput::File(path) => {
                        out.insert(key_of(project_root, &path), file_digest(&path));
                    }
                    WorkspaceInput::MemberListing(dir) => {
                        let names = member_listing(&dir);
                        out.insert(
                            format!("dir:{}", key_of(project_root, &dir)),
                            listing_digest(&dir, &names),
                        );
                    }
                }
            }
            // The Tauri markers a framework is detected by.
            for marker in TAURI_MARKERS {
                out.insert(marker.to_string(), file_digest(&project_root.join(marker)));
            }
            let js: Vec<String> = files
                .iter()
                .filter(|file| kind.reads_from(&file.path, file.language))
                .map(|file| file.path.clone())
                .collect();
            for relative in codegraph_resolve::package_manifests::manifest_inputs(&js) {
                let digest = file_digest(&project_root.join(&relative));
                out.insert(relative, digest);
            }
            // The manifests framework detection reads (upstream #2395): the
            // root's, the ones one or two directories down from any indexed
            // file, and the ones deeper above JavaScript-family code.
            let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
            for relative in codegraph_resolve::frameworks::package_deps::manifest_candidates(&paths)
            {
                let digest = file_digest(&project_root.join(&relative));
                out.insert(relative, digest);
            }
        }
        ControlKind::Csharp => {
            let cs: Vec<String> = files
                .iter()
                .filter(|file| kind.reads_from(&file.path, file.language))
                .map(|file| file.path.clone())
                .collect();
            let (listings, project_files) =
                codegraph_resolve::csharp_types::csharp_project_inputs(project_root, &cs);
            for (dir, names) in listings {
                let full = if dir == "." {
                    project_root.to_path_buf()
                } else {
                    project_root.join(&dir)
                };
                out.insert(format!("dir:{dir}"), listing_digest(&full, &names));
            }
            for relative in project_files {
                let digest = file_digest(&project_root.join(&relative));
                out.insert(relative, digest);
            }
        }
        ControlKind::Rust => {
            let paths: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
            for relative in codegraph_resolve::rust_scope::rust_manifest_inputs(&paths) {
                let digest = file_digest(&project_root.join(&relative));
                out.insert(relative, digest);
            }
        }
    }
    out
}

fn read_row(store: &Store, kind: ControlKind) -> Result<Option<Digests>> {
    Ok(store
        .get_project_metadata(&kind.row_key())?
        .and_then(|value| serde_json::from_str(&value).ok()))
}

fn write_row(store: &Store, kind: ControlKind, digests: &Digests) -> Result<()> {
    store.set_project_metadata(&kind.row_key(), &serde_json::to_string(digests)?)?;
    Ok(())
}

/// The detected framework names as the row holds them.
fn read_frameworks(store: &Store) -> Result<Option<Vec<String>>> {
    Ok(store
        .get_project_metadata(FRAMEWORKS_KEY)?
        .and_then(|value| serde_json::from_str(&value).ok()))
}

/// Write every kind's row, the frameworks row and both markers for a full
/// index or a migration, once every file row is stored.
pub fn write_index_rows(store: &Store, project_root: &Path, frameworks: &[String]) -> Result<()> {
    let files = store.all_files()?;
    for kind in ControlKind::ALL {
        write_row(store, kind, &digests_of(kind, project_root, &files))?;
    }
    store.set_project_metadata(FRAMEWORKS_KEY, &serde_json::to_string(frameworks)?)?;
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    store.set_project_metadata(FRAMEWORKS_VERSION_KEY, FRAMEWORKS_VERSION)?;
    Ok(())
}

/// Whether this index holds no trustworthy control rows.
pub(crate) fn needs_bootstrap(store: &Store) -> Result<bool> {
    Ok(store.get_project_metadata(VERSION_KEY)?.as_deref() != Some(VERSION))
}

/// Withdraw the marker before a sync's first change.
pub(crate) fn withdraw_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(VERSION_KEY)?;
    Ok(())
}

/// Withdraw the frameworks marker before re-extracting for a framework
/// change.
pub(crate) fn withdraw_frameworks_marker(store: &Store) -> Result<()> {
    store.delete_project_metadata(FRAMEWORKS_VERSION_KEY)?;
    Ok(())
}

/// The directory a project path is in: `""` for one at the root.
fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The recorded inputs an event can name, by project path, as the rows hold
/// them once a sync is done (W5-06): the watcher hands their events to the
/// next sync, though they are no source files.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecordedInputs {
    /// Every input inside the project, a listed directory by its own path.
    pub(crate) paths: BTreeSet<String>,
    /// The directories a workspace glob lists for its members: a directory
    /// coming into one moves the listing, though no event names a control
    /// file in it.
    pub(crate) member_listings: BTreeSet<String>,
}

impl RecordedInputs {
    /// Whether an event for `relative`, a directory when `is_dir`, may move
    /// a recorded input.
    pub fn may_move(&self, relative: &str, is_dir: bool) -> bool {
        self.paths.contains(relative)
            || (is_dir && self.member_listings.contains(parent_of(relative)))
    }
}

/// The recorded inputs of the rows this index holds.
pub fn recorded_paths(store: &Store) -> Result<RecordedInputs> {
    let mut out = RecordedInputs::default();
    for kind in ControlKind::ALL {
        for key in read_row(store, kind)?.unwrap_or_default().into_keys() {
            let (listing, path) = match key.strip_prefix("dir:") {
                Some(dir) => (true, dir),
                None => (false, key.as_str()),
            };
            if Path::new(path).is_absolute() {
                continue;
            }
            // A C# listing holds only project file names, whose events a
            // control file's name already hands on.
            if listing && kind == ControlKind::Js {
                out.member_listings.insert(path.to_string());
            }
            out.paths.insert(path.to_string());
        }
    }
    Ok(out)
}

/// A file a sync added (`None` before), removed (`None` after) or gave
/// another language.
pub(crate) type MembershipChange = (String, Option<Language>, Option<Language>);

/// What a sync's control lane found.
#[derive(Debug, Default)]
pub(crate) struct ControlCheck {
    /// The kinds whose inputs moved, or every kind on a bootstrap.
    pub(crate) changed: BTreeSet<ControlKind>,
    /// The rows to write once the sync's resolution is persisted.
    rows: BTreeMap<ControlKind, Digests>,
}

impl ControlCheck {
    /// Whether any kind moved.
    pub(crate) fn any(&self) -> bool {
        !self.changed.is_empty()
    }

    /// Write the rows. Called after the sync's resolution is persisted.
    pub(crate) fn write_rows(&self, store: &Store) -> Result<()> {
        for (kind, digests) in &self.rows {
            write_row(store, *kind, digests)?;
        }
        Ok(())
    }
}

/// Compare the control inputs after a sync's paths are processed (the store
/// holds the new file set). `full` (the CLI's full sync) re-reads every
/// kind; otherwise a kind is re-read when a `requested` path is one of its
/// recorded inputs, is a listed directory or one directly in a workspace
/// glob's listing, or names one of its control files, or a `membership`
/// change touches a file its readers start from. A `bootstrap` (the marker
/// was missing when the sync began) re-reads every kind and counts each as
/// moved.
pub(crate) fn check(
    store: &Store,
    project_root: &Path,
    full: bool,
    bootstrap: bool,
    requested: &[String],
    membership: &[MembershipChange],
) -> Result<ControlCheck> {
    let mut recorded: BTreeMap<ControlKind, Option<Digests>> = BTreeMap::new();
    for kind in ControlKind::ALL {
        recorded.insert(kind, read_row(store, kind)?);
    }
    let touched = |kind: ControlKind| -> bool {
        if full || bootstrap {
            return true;
        }
        let Some(Some(row)) = recorded.get(&kind) else {
            return true;
        };
        // A recorded input, a listed directory, a directory a workspace glob
        // lists for its members, or a file named as a control file of the
        // kind (a C# listing holds only those).
        requested.iter().any(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            kind.names_file(name)
                || row.contains_key(path)
                || row.contains_key(&format!("dir:{path}"))
                || (kind == ControlKind::Js
                    && row.contains_key(&format!("dir:{}", parent_of(path))))
        }) || membership.iter().any(|(path, before, after)| {
            before
                .iter()
                .chain(after)
                .any(|language| kind.reads_from(path, *language))
        })
    };
    let mut out = ControlCheck::default();
    let mut files: Option<Vec<FileRecord>> = None;
    for kind in ControlKind::ALL {
        if !touched(kind) {
            continue;
        }
        let files = match &files {
            Some(files) => files,
            None => files.insert(store.all_files()?),
        };
        let now = digests_of(kind, project_root, files);
        if bootstrap || recorded.get(&kind).and_then(Option::as_ref) != Some(&now) {
            out.changed.insert(kind);
        }
        out.rows.insert(kind, now);
    }
    Ok(out)
}

/// Whether no finished sync recorded the frameworks: the row or the
/// frameworks marker is missing, so this sync must detect them.
pub(crate) fn frameworks_unchecked(store: &Store) -> Result<bool> {
    Ok(store
        .get_project_metadata(FRAMEWORKS_VERSION_KEY)?
        .as_deref()
        != Some(FRAMEWORKS_VERSION)
        || read_frameworks(store)?.is_none())
}

/// Whether the frameworks a sync detects differ from the row's, or a sync
/// that re-extracted for a framework change did not finish.
pub(crate) fn frameworks_changed(store: &Store, detected: &[String]) -> Result<bool> {
    Ok(store
        .get_project_metadata(FRAMEWORKS_VERSION_KEY)?
        .as_deref()
        != Some(FRAMEWORKS_VERSION)
        || read_frameworks(store)?.as_deref() != Some(detected))
}

/// Record the detected frameworks and the frameworks marker, once the sync's
/// resolution is persisted.
pub(crate) fn write_frameworks(store: &Store, detected: &[String]) -> Result<()> {
    store.set_project_metadata(FRAMEWORKS_KEY, &serde_json::to_string(detected)?)?;
    store.set_project_metadata(FRAMEWORKS_VERSION_KEY, FRAMEWORKS_VERSION)?;
    Ok(())
}

/// Record that every control-file change is resolved.
pub(crate) fn write_marker(store: &Store) -> Result<()> {
    store.set_project_metadata(VERSION_KEY, VERSION)?;
    Ok(())
}

/// The indexed files of the kinds in `changed`: what re-resolves whole.
pub(crate) fn files_of(store: &Store, changed: &BTreeSet<ControlKind>) -> Result<Vec<String>> {
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(store
        .all_files()?
        .into_iter()
        .filter(|file| {
            changed
                .iter()
                .any(|kind| kind.reads_from(&file.path, file.language))
        })
        .map(|file| file.path)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_files_are_named_by_kind() {
        assert!(is_control_file("package.json"));
        assert!(is_control_file("apps/web/tsconfig.json"));
        assert!(is_control_file("src/App.CSPROJ"));
        assert!(is_control_file("Directory.Build.props"));
        assert!(is_control_file("crates/a/Cargo.toml"));
        assert!(is_control_file("src-tauri/tauri.conf.json"));
        assert!(is_control_file("packages/kit/pubspec.yaml"));
        assert!(is_control_file("pubspec_overrides.yaml"));
        assert!(!is_control_file("pubspec.lock"));
        assert!(!is_control_file("apps/tauri.conf.json"));
        assert!(!is_control_file("src/main.ts"));
        assert!(!is_control_file("cargo.toml"));
    }
}
