//! Skill-embedding + git-blob-hash update engine.
//!
//! Embeds the canonical `skills/codegraph/SKILL.md` into the binary and writes
//! it into each agent's skill directory under `<parent>/codegraph/SKILL.md`,
//! alongside a `.codegraph-skill.json` sidecar marker. The update decision is
//! driven SOLELY by the git-blob SHA-1 of the installed content versus the
//! embedded content (the sidecar `version`/`installed_at` fields are
//! informational only).
//!
//! This is the shared foundation the per-agent skill writers and the CLI
//! orchestrator build on: [`write_skill_to_dir`], [`uninstall_from_dir`],
//! [`status_for_dir`], and [`read_installed`] take a *skill parent dir* (the
//! directory that will contain the `codegraph/` skill folder) and own all the
//! filesystem + decision logic.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::shared::atomic_write_file;
use super::types::{FileAction, FileWrite, WriteResult};

/// The canonical CodeGraph skill, embedded at compile time.
///
/// Path is relative to THIS file (`crates/codegraph-cli/src/installer/skill.rs`)
/// up to the repo root `skills/codegraph/SKILL.md` — four `../` hops:
/// `installer` → `src` → `codegraph-cli` → `crates` → repo root.
pub const SKILL_MD: &str = include_str!("../../../../skills/codegraph/SKILL.md");

/// The skill folder name (matches the SKILL.md frontmatter `name:`).
pub const SKILL_DIR_NAME: &str = "codegraph";

/// The skill file name (case-sensitive, per the Open Agent Skills standard).
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// The sidecar marker file name written next to the skill.
pub const SIDECAR_FILE_NAME: &str = ".codegraph-skill.json";

/// Compute the git blob object hash of `content`.
///
/// Git hashes a blob as `sha1("blob " + len + "\0" + content)`, returning the
/// lowercase hex digest. This is SHA-1 (NOT SHA-256) so it matches
/// `git hash-object`. The empty blob is the well-known
/// `e69de29bb2d1d6434b8b29ae775ad8c2e48c5391`.
pub fn git_blob_sha1(content: &[u8]) -> String {
    let mut hasher = sha1_smol::Sha1::new();
    let header = format!("blob {}\0", content.len());
    hasher.update(header.as_bytes());
    hasher.update(content);
    hasher.digest().to_string()
}

/// Sidecar marker persisted at `<skill-dir>/.codegraph-skill.json`.
///
/// `hash` is the git-blob SHA-1 of the SKILL.md content we wrote — it is the
/// sole input to the update decision. `version` and `installed_at` are
/// INFORMATIONAL ONLY (human-facing provenance), never decision inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMarker {
    /// git-blob SHA-1 of the SKILL.md content this marker accompanies.
    pub hash: String,
    /// CLI version that wrote the skill (informational).
    pub version: String,
    /// RFC3339 timestamp of when the skill was written (informational).
    pub installed_at: String,
}

impl SkillMarker {
    /// Build a fresh marker for the embedded skill at the current instant.
    fn for_embedded() -> Self {
        Self {
            hash: git_blob_sha1(SKILL_MD.as_bytes()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            installed_at: now_rfc3339(),
        }
    }

    fn to_pretty_json(&self) -> String {
        // `serde_json` cannot fail on this flat all-string struct; fall back to a
        // minimal hand-rolled object rather than panic if it somehow does.
        let mut content = serde_json::to_string_pretty(self).unwrap_or_else(|_| {
            format!(
                "{{\n  \"hash\": \"{}\",\n  \"version\": \"{}\",\n  \"installed_at\": \"{}\"\n}}",
                self.hash, self.version, self.installed_at
            )
        });
        content.push('\n');
        content
    }
}

/// Current UTC time as an RFC3339 string, reusing the workspace `time` dep.
fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// The decision the update engine reaches for a single skill directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillUpdateDecision {
    /// Installed content already equals the embedded skill — no write.
    Unchanged,
    /// We should (over)write: fresh install, forced, or provenance-confirmed.
    Update,
    /// Installed content differs and we did NOT write it — leave it alone.
    LocallyModified,
}

/// Decide what to do for one skill directory.
///
/// `installed_content` is the current on-disk SKILL.md (None ⇒ not installed).
/// `sidecar` is the parsed marker, if any. `force` overrides local-modification
/// protection. The branch table (exhaustive):
///
/// 1. `force && installed.is_some()` → `Update`
/// 2. `installed.is_none()` → `Update` (fresh install)
/// 3. `installed == embedded` (by git-blob SHA-1) → `Unchanged`
/// 4. drift + sidecar.hash == sha(installed) → `Update` (we wrote it; refresh)
/// 5. drift + sidecar.hash != sha(installed) → `LocallyModified`
/// 6. drift + no sidecar → `LocallyModified` (conservative; unknown provenance)
pub fn decide(
    installed_content: Option<&str>,
    sidecar: Option<&SkillMarker>,
    force: bool,
) -> SkillUpdateDecision {
    let Some(installed) = installed_content else {
        // Branch 2: fresh install (also covers force on a missing file).
        return SkillUpdateDecision::Update;
    };

    // Branch 1: forced overwrite of an existing file.
    if force {
        return SkillUpdateDecision::Update;
    }

    let embedded_hash = git_blob_sha1(SKILL_MD.as_bytes());
    let installed_hash = git_blob_sha1(installed.as_bytes());

    // Branch 3: byte-identical to embedded — nothing to do, sidecar irrelevant.
    if installed_hash == embedded_hash {
        return SkillUpdateDecision::Unchanged;
    }

    match sidecar {
        // Branch 4: we recorded this exact installed content ⇒ safe to refresh.
        Some(marker) if marker.hash == installed_hash => SkillUpdateDecision::Update,
        // Branch 5: sidecar exists but disagrees ⇒ user edited the file.
        Some(_) => SkillUpdateDecision::LocallyModified,
        // Branch 6: no provenance ⇒ conservative.
        None => SkillUpdateDecision::LocallyModified,
    }
}

/// The installed-skill status for one directory (consumed by `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillStatus {
    /// No SKILL.md present in the skill directory.
    NotInstalled,
    /// Installed content equals the embedded skill.
    UpToDate,
    /// Installed content differs from embedded and was not written by us.
    LocallyModified,
    /// Installed content differs from embedded but matches our sidecar
    /// (a `codegraph install` would refresh it to the current version).
    Outdated,
}

impl SkillStatus {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            SkillStatus::NotInstalled => "not installed",
            SkillStatus::UpToDate => "up to date",
            SkillStatus::LocallyModified => "locally modified",
            SkillStatus::Outdated => "outdated",
        }
    }
}

/// The skill folder for a given parent dir: `<parent>/codegraph`.
fn skill_dir(skill_parent_dir: &Path) -> PathBuf {
    skill_parent_dir.join(SKILL_DIR_NAME)
}

/// The SKILL.md path for a given parent dir.
fn skill_file(skill_parent_dir: &Path) -> PathBuf {
    skill_dir(skill_parent_dir).join(SKILL_FILE_NAME)
}

/// The sidecar marker path for a given parent dir.
fn sidecar_file(skill_parent_dir: &Path) -> PathBuf {
    skill_dir(skill_parent_dir).join(SIDECAR_FILE_NAME)
}

/// Read the installed SKILL.md content and parsed sidecar marker, if present.
///
/// A present-but-unparseable sidecar reads as `None` (treated as missing
/// provenance — `decide` is then conservative).
pub fn read_installed(skill_parent_dir: &Path) -> (Option<String>, Option<SkillMarker>) {
    let content = std::fs::read_to_string(skill_file(skill_parent_dir)).ok();
    let sidecar = std::fs::read_to_string(sidecar_file(skill_parent_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<SkillMarker>(&text).ok());
    (content, sidecar)
}

/// Write the embedded skill into `<skill_parent_dir>/codegraph/`.
///
/// Reads any existing installed content + sidecar, runs [`decide`], and:
/// - `Unchanged` → no write; one `FileAction::Unchanged` entry.
/// - `LocallyModified` (and `!force`) → no write; `FileAction::Skipped` + note.
/// - `Update`/fresh/force → atomically writes SKILL.md + a refreshed sidecar;
///   `FileAction::Created` (was absent) or `FileAction::Updated` (existed).
///
/// On an I/O failure the returned [`WriteResult`] carries a `Skipped` action +
/// an explanatory note (the caller's report loop surfaces it) rather than
/// panicking.
pub fn write_skill_to_dir(skill_parent_dir: &Path, force: bool) -> WriteResult {
    let skill_path = skill_file(skill_parent_dir);
    let sidecar_path = sidecar_file(skill_parent_dir);
    let (installed, sidecar) = read_installed(skill_parent_dir);
    let existed = installed.is_some();

    match decide(installed.as_deref(), sidecar.as_ref(), force) {
        SkillUpdateDecision::Unchanged => WriteResult {
            files: vec![FileWrite {
                path: skill_path,
                action: FileAction::Unchanged,
            }],
            notes: Vec::new(),
        },
        SkillUpdateDecision::LocallyModified => WriteResult {
            files: vec![FileWrite {
                path: skill_path,
                action: FileAction::Skipped,
            }],
            notes: vec![
                "skill locally modified — left unchanged (use --force to overwrite)".to_string(),
            ],
        },
        SkillUpdateDecision::Update => {
            let marker = SkillMarker::for_embedded();
            if let Err(err) = atomic_write_file(&skill_path, SKILL_MD) {
                return WriteResult {
                    files: vec![FileWrite {
                        path: skill_path,
                        action: FileAction::Skipped,
                    }],
                    notes: vec![format!("failed to write skill: {err}")],
                };
            }
            if let Err(err) = atomic_write_file(&sidecar_path, &marker.to_pretty_json()) {
                return WriteResult {
                    files: vec![FileWrite {
                        path: skill_path,
                        action: if existed {
                            FileAction::Updated
                        } else {
                            FileAction::Created
                        },
                    }],
                    notes: vec![format!("wrote skill but failed to write marker: {err}")],
                };
            }
            let action = if existed {
                FileAction::Updated
            } else {
                FileAction::Created
            };
            WriteResult {
                files: vec![
                    FileWrite {
                        path: skill_path,
                        action,
                    },
                    FileWrite {
                        path: sidecar_path,
                        action,
                    },
                ],
                notes: Vec::new(),
            }
        }
    }
}

/// Remove the skill from `<skill_parent_dir>/codegraph/`.
///
/// Removes SKILL.md + the sidecar, then removes the now-empty `codegraph/`
/// directory. Reports `FileAction::Removed` for each file actually removed, or
/// a single `FileAction::NotFound` entry when nothing was installed.
pub fn uninstall_from_dir(skill_parent_dir: &Path) -> WriteResult {
    let dir = skill_dir(skill_parent_dir);
    let skill_path = skill_file(skill_parent_dir);
    let sidecar_path = sidecar_file(skill_parent_dir);

    let mut files = Vec::new();
    if skill_path.exists() && std::fs::remove_file(&skill_path).is_ok() {
        files.push(FileWrite {
            path: skill_path.clone(),
            action: FileAction::Removed,
        });
    }
    if sidecar_path.exists() && std::fs::remove_file(&sidecar_path).is_ok() {
        files.push(FileWrite {
            path: sidecar_path,
            action: FileAction::Removed,
        });
    }

    if files.is_empty() {
        return WriteResult {
            files: vec![FileWrite {
                path: skill_path,
                action: FileAction::NotFound,
            }],
            notes: Vec::new(),
        };
    }

    // Best-effort removal of the now-empty skill dir; ignore failures (e.g. the
    // user dropped extra files in there).
    let _ = std::fs::remove_dir(&dir);
    WriteResult {
        files,
        notes: Vec::new(),
    }
}

/// Report the installed-skill status for one directory.
pub fn status_for_dir(skill_parent_dir: &Path) -> SkillStatus {
    let (installed, sidecar) = read_installed(skill_parent_dir);
    let Some(installed) = installed else {
        return SkillStatus::NotInstalled;
    };
    let embedded_hash = git_blob_sha1(SKILL_MD.as_bytes());
    let installed_hash = git_blob_sha1(installed.as_bytes());
    if installed_hash == embedded_hash {
        return SkillStatus::UpToDate;
    }
    match sidecar {
        Some(marker) if marker.hash == installed_hash => SkillStatus::Outdated,
        _ => SkillStatus::LocallyModified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_parent(label: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "codegraph-skill-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    // --- git_blob_sha1 vectors ------------------------------------------------

    #[test]
    fn git_blob_sha1_empty_matches_git() {
        // `printf '' | git hash-object --stdin`
        assert_eq!(
            git_blob_sha1(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn git_blob_sha1_hello_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            git_blob_sha1(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    // --- embedded skill -------------------------------------------------------

    #[test]
    fn skill_md_is_embedded_and_well_formed() {
        assert!(SKILL_MD.starts_with("---\n"), "must start with YAML fence");
        assert!(
            SKILL_MD.contains("name: codegraph"),
            "must declare the codegraph skill name"
        );
    }

    // --- decide(): all six branches ------------------------------------------

    #[test]
    fn decide_branch1_force_with_installed_is_update() {
        // force == true AND installed.is_some() → Update
        assert_eq!(
            decide(Some("anything at all"), None, true),
            SkillUpdateDecision::Update
        );
        // also with a matching sidecar present
        let marker = SkillMarker {
            hash: git_blob_sha1(b"anything at all"),
            version: "x".into(),
            installed_at: "x".into(),
        };
        assert_eq!(
            decide(Some("anything at all"), Some(&marker), true),
            SkillUpdateDecision::Update
        );
    }

    #[test]
    fn decide_branch2_fresh_install_is_update() {
        // installed.is_none() → Update
        assert_eq!(decide(None, None, false), SkillUpdateDecision::Update);
        assert_eq!(decide(None, None, true), SkillUpdateDecision::Update);
    }

    #[test]
    fn decide_branch3_identical_is_unchanged() {
        // installed == embedded → Unchanged (regardless of sidecar)
        assert_eq!(
            decide(Some(SKILL_MD), None, false),
            SkillUpdateDecision::Unchanged
        );
        let stale = SkillMarker {
            hash: "deadbeef".into(),
            version: "old".into(),
            installed_at: "old".into(),
        };
        assert_eq!(
            decide(Some(SKILL_MD), Some(&stale), false),
            SkillUpdateDecision::Unchanged
        );
    }

    #[test]
    fn decide_branch4_drift_matching_sidecar_is_update() {
        // installed != embedded AND sidecar.hash == sha(installed) → Update
        let installed = "we wrote this once\n";
        let marker = SkillMarker {
            hash: git_blob_sha1(installed.as_bytes()),
            version: "0.1.0".into(),
            installed_at: "t".into(),
        };
        assert_eq!(
            decide(Some(installed), Some(&marker), false),
            SkillUpdateDecision::Update
        );
    }

    #[test]
    fn decide_branch5_drift_mismatching_sidecar_is_locally_modified() {
        // installed != embedded AND sidecar.hash != sha(installed) → LocallyModified
        let installed = "user edited this\n";
        let marker = SkillMarker {
            hash: git_blob_sha1(b"some other content"),
            version: "0.1.0".into(),
            installed_at: "t".into(),
        };
        assert_eq!(
            decide(Some(installed), Some(&marker), false),
            SkillUpdateDecision::LocallyModified
        );
    }

    #[test]
    fn decide_branch6_drift_no_sidecar_is_locally_modified() {
        // installed != embedded AND sidecar None → LocallyModified
        assert_eq!(
            decide(Some("mystery content\n"), None, false),
            SkillUpdateDecision::LocallyModified
        );
    }

    // --- write_skill_to_dir lifecycle ----------------------------------------

    #[test]
    fn write_create_then_unchanged_then_force() {
        let parent = temp_parent("write-cycle");

        // Fresh install → Created (skill + sidecar both Created).
        let r1 = write_skill_to_dir(&parent, false);
        assert!(r1
            .files
            .iter()
            .any(|f| f.action == FileAction::Created && f.path.ends_with("SKILL.md")));
        assert!(r1
            .files
            .iter()
            .any(|f| f.action == FileAction::Created && f.path.ends_with(SIDECAR_FILE_NAME)));
        assert_eq!(fs::read_to_string(skill_file(&parent)).unwrap(), SKILL_MD);
        // Sidecar round-trips and records the embedded hash.
        let (_, marker) = read_installed(&parent);
        let marker = marker.expect("sidecar written");
        assert_eq!(marker.hash, git_blob_sha1(SKILL_MD.as_bytes()));
        assert_eq!(marker.version, env!("CARGO_PKG_VERSION"));

        // Re-run with identical content → Unchanged, no write.
        let r2 = write_skill_to_dir(&parent, false);
        assert_eq!(r2.files.len(), 1);
        assert_eq!(r2.files[0].action, FileAction::Unchanged);

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn write_skips_locally_modified_without_force() {
        let parent = temp_parent("write-skip");
        // Install, then mutate the file to simulate a user edit, then drop the
        // sidecar provenance match by overwriting SKILL.md directly.
        write_skill_to_dir(&parent, false);
        fs::write(skill_file(&parent), "user hacked this\n").unwrap();

        // Without force → Skipped + note, file untouched.
        let r = write_skill_to_dir(&parent, false);
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].action, FileAction::Skipped);
        assert!(!r.notes.is_empty());
        assert_eq!(
            fs::read_to_string(skill_file(&parent)).unwrap(),
            "user hacked this\n"
        );

        // With force → Updated, embedded content restored.
        let r2 = write_skill_to_dir(&parent, true);
        assert!(r2
            .files
            .iter()
            .any(|f| f.action == FileAction::Updated && f.path.ends_with("SKILL.md")));
        assert_eq!(fs::read_to_string(skill_file(&parent)).unwrap(), SKILL_MD);

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn write_then_drift_matching_sidecar_updates() {
        let parent = temp_parent("write-drift");
        write_skill_to_dir(&parent, false);
        // Simulate "we wrote it but the embedded content changed": rewrite the
        // file to non-embedded content AND record that content's hash in the
        // sidecar, so provenance matches.
        let drifted = "previously-embedded skill\n";
        fs::write(skill_file(&parent), drifted).unwrap();
        let marker = SkillMarker {
            hash: git_blob_sha1(drifted.as_bytes()),
            version: "0.0.1".into(),
            installed_at: "t".into(),
        };
        fs::write(sidecar_file(&parent), marker.to_pretty_json()).unwrap();

        // Provenance match ⇒ Update (no force) ⇒ embedded restored.
        let r = write_skill_to_dir(&parent, false);
        assert!(r
            .files
            .iter()
            .any(|f| f.action == FileAction::Updated && f.path.ends_with("SKILL.md")));
        assert_eq!(fs::read_to_string(skill_file(&parent)).unwrap(), SKILL_MD);

        let _ = fs::remove_dir_all(&parent);
    }

    // --- uninstall_from_dir ---------------------------------------------------

    #[test]
    fn uninstall_removes_files_and_dir() {
        let parent = temp_parent("uninstall");
        write_skill_to_dir(&parent, false);
        assert!(skill_file(&parent).exists());

        let r = uninstall_from_dir(&parent);
        let removed = r
            .files
            .iter()
            .filter(|f| f.action == FileAction::Removed)
            .count();
        assert_eq!(removed, 2, "SKILL.md + sidecar removed");
        assert!(!skill_dir(&parent).exists(), "empty skill dir removed");

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn uninstall_absent_is_not_found() {
        let parent = temp_parent("uninstall-absent");
        let r = uninstall_from_dir(&parent);
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].action, FileAction::NotFound);
        let _ = fs::remove_dir_all(&parent);
    }

    // --- status_for_dir -------------------------------------------------------

    #[test]
    fn status_reports_lifecycle_states() {
        let parent = temp_parent("status");
        assert_eq!(status_for_dir(&parent), SkillStatus::NotInstalled);

        write_skill_to_dir(&parent, false);
        assert_eq!(status_for_dir(&parent), SkillStatus::UpToDate);

        // User edit, no provenance match ⇒ LocallyModified.
        fs::write(skill_file(&parent), "edited\n").unwrap();
        assert_eq!(status_for_dir(&parent), SkillStatus::LocallyModified);

        // Provenance match against drifted content ⇒ Outdated.
        let drifted = "old embedded\n";
        fs::write(skill_file(&parent), drifted).unwrap();
        let marker = SkillMarker {
            hash: git_blob_sha1(drifted.as_bytes()),
            version: "0.0.1".into(),
            installed_at: "t".into(),
        };
        fs::write(sidecar_file(&parent), marker.to_pretty_json()).unwrap();
        assert_eq!(status_for_dir(&parent), SkillStatus::Outdated);

        let _ = fs::remove_dir_all(&parent);
    }
}
