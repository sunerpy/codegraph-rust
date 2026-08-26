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

/// CLI version that owns the embedded skill.
pub const EMBEDDED_VERSION: &str = env!("CARGO_PKG_VERSION");

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
            version: EMBEDDED_VERSION.to_string(),
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
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
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

/// Status plus the version provenance needed by human-facing reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillStatusDetails {
    pub status: SkillStatus,
    /// Version recorded by the sidecar. `None` when absent or unknown.
    pub installed_version: Option<String>,
}

/// Read-only plan for one `codegraph skill update` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillUpdatePreview {
    pub decision: SkillUpdateDecision,
    pub status: SkillStatus,
    pub installed_version: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub unified_diff: Option<String>,
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

/// Report installed-skill status with marker version provenance.
pub fn status_details_for_dir(skill_parent_dir: &Path) -> SkillStatusDetails {
    let (installed, sidecar) = read_installed(skill_parent_dir);
    status_details_from_parts(installed.as_deref(), sidecar.as_ref())
}

fn status_details_from_parts(
    installed: Option<&str>,
    sidecar: Option<&SkillMarker>,
) -> SkillStatusDetails {
    let Some(installed) = installed else {
        return SkillStatusDetails {
            status: SkillStatus::NotInstalled,
            installed_version: None,
        };
    };
    let embedded_hash = git_blob_sha1(SKILL_MD.as_bytes());
    let installed_hash = git_blob_sha1(installed.as_bytes());
    if installed_hash == embedded_hash {
        return SkillStatusDetails {
            status: SkillStatus::UpToDate,
            installed_version: Some(EMBEDDED_VERSION.to_string()),
        };
    }
    SkillStatusDetails {
        status: match sidecar {
            Some(marker) if marker.hash == installed_hash => SkillStatus::Outdated,
            _ => SkillStatus::LocallyModified,
        },
        installed_version: sidecar.map(|marker| marker.version.clone()),
    }
}

/// Build a read-only update preview for one skill directory.
pub fn preview_update_for_dir(skill_parent_dir: &Path, force: bool) -> SkillUpdatePreview {
    let (installed, sidecar) = read_installed(skill_parent_dir);
    let decision = decide(installed.as_deref(), sidecar.as_ref(), force);
    let details = status_details_from_parts(installed.as_deref(), sidecar.as_ref());
    let (added_lines, removed_lines, unified_diff) = match installed.as_deref() {
        Some(content) if content != SKILL_MD => {
            let old_label = match details.installed_version.as_deref() {
                Some(version) => format!("installed CodeGraph skill {version}"),
                None => "installed CodeGraph skill (unknown version)".to_string(),
            };
            let new_label = format!("embedded CodeGraph skill {EMBEDDED_VERSION}");
            let rendered = render_unified_diff(content, SKILL_MD, &old_label, &new_label);
            (
                rendered.added_lines,
                rendered.removed_lines,
                Some(rendered.text),
            )
        }
        None => {
            let new_label = format!("embedded CodeGraph skill {EMBEDDED_VERSION}");
            let rendered = render_unified_diff("", SKILL_MD, "/dev/null", &new_label);
            (
                rendered.added_lines,
                rendered.removed_lines,
                Some(rendered.text),
            )
        }
        Some(_) => (0, 0, None),
    };

    SkillUpdatePreview {
        decision,
        status: details.status,
        installed_version: details.installed_version,
        added_lines,
        removed_lines,
        unified_diff,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Equal,
    Remove,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffLine<'a> {
    kind: DiffKind,
    text: &'a str,
    old_line: usize,
    new_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedDiff {
    text: String,
    added_lines: usize,
    removed_lines: usize,
}

/// Deterministic line-based unified diff. Skill files are small, so the
/// quadratic LCS table is bounded and keeps the shipped binary independent of
/// an external `diff` command on Windows and minimal containers.
fn render_unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> RenderedDiff {
    let old_lines: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.split_inclusive('\n').collect()
    };
    let new_lines: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.split_inclusive('\n').collect()
    };
    let old_len = old_lines.len();
    let new_len = new_lines.len();
    let width = new_len + 1;
    let mut lcs = vec![0_usize; (old_len + 1) * width];
    let at = |i: usize, j: usize| i * width + j;

    for i in (0..old_len).rev() {
        for j in (0..new_len).rev() {
            lcs[at(i, j)] = if old_lines[i] == new_lines[j] {
                1 + lcs[at(i + 1, j + 1)]
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }

    let mut ops = Vec::with_capacity(old_len + new_len);
    let (mut i, mut j, mut old_line, mut new_line) = (0, 0, 1, 1);
    while i < old_len || j < new_len {
        let (kind, text) = if i < old_len && j < new_len && old_lines[i] == new_lines[j] {
            (DiffKind::Equal, old_lines[i])
        } else if i < old_len && (j == new_len || lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            (DiffKind::Remove, old_lines[i])
        } else {
            (DiffKind::Add, new_lines[j])
        };
        ops.push(DiffLine {
            kind,
            text,
            old_line,
            new_line,
        });
        match kind {
            DiffKind::Equal => {
                i += 1;
                j += 1;
                old_line += 1;
                new_line += 1;
            }
            DiffKind::Remove => {
                i += 1;
                old_line += 1;
            }
            DiffKind::Add => {
                j += 1;
                new_line += 1;
            }
        }
    }

    let added_lines = ops.iter().filter(|line| line.kind == DiffKind::Add).count();
    let removed_lines = ops
        .iter()
        .filter(|line| line.kind == DiffKind::Remove)
        .count();
    let mut text = format!("--- {old_label}\n+++ {new_label}\n");
    if added_lines == 0 && removed_lines == 0 {
        return RenderedDiff {
            text,
            added_lines,
            removed_lines,
        };
    }

    const CONTEXT: usize = 3;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for index in ops
        .iter()
        .enumerate()
        .filter_map(|(index, line)| (line.kind != DiffKind::Equal).then_some(index))
    {
        let start = index.saturating_sub(CONTEXT);
        let end = (index + CONTEXT + 1).min(ops.len());
        match ranges.last_mut() {
            Some((_, current_end)) if start <= *current_end => {
                *current_end = (*current_end).max(end);
            }
            _ => ranges.push((start, end)),
        }
    }

    for (start, end) in ranges {
        let hunk = &ops[start..end];
        let old_count = hunk
            .iter()
            .filter(|line| line.kind != DiffKind::Add)
            .count();
        let new_count = hunk
            .iter()
            .filter(|line| line.kind != DiffKind::Remove)
            .count();
        let old_start = if old_count == 0 {
            hunk[0].old_line.saturating_sub(1)
        } else {
            hunk[0].old_line
        };
        let new_start = if new_count == 0 {
            hunk[0].new_line.saturating_sub(1)
        } else {
            hunk[0].new_line
        };
        text.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for line in hunk {
            let prefix = match line.kind {
                DiffKind::Equal => ' ',
                DiffKind::Remove => '-',
                DiffKind::Add => '+',
            };
            text.push(prefix);
            let has_newline = line.text.ends_with('\n');
            let visible = line.text.strip_suffix('\n').unwrap_or(line.text);
            let visible = visible.strip_suffix('\r').unwrap_or(visible);
            text.push_str(visible);
            text.push('\n');
            if !has_newline {
                text.push_str("\\ No newline at end of file\n");
            }
        }
    }

    RenderedDiff {
        text,
        added_lines,
        removed_lines,
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
        let front_matter = SKILL_MD[4..]
            .split_once("\n---")
            .map(|(front_matter, _)| front_matter)
            .expect("must close the YAML front-matter fence");
        assert!(
            front_matter.lines().any(|line| line == "name: codegraph"),
            "must declare the codegraph skill name"
        );
        let description = front_matter
            .lines()
            .position(|line| line == "description: >")
            .expect("must declare a folded description");
        for line in front_matter.lines().skip(description + 1) {
            assert!(
                line.is_empty() || line.starts_with("  "),
                "description continuation must stay indented YAML, got {line:?}"
            );
        }
    }

    // --- Codex MAX_DESCRIPTION_LEN guard --------------------------------------

    /// Codex's `core-skills/src/loader.rs` rejects any skill whose YAML
    /// front-matter `description`, after `sanitize_single_line` (split on
    /// whitespace and rejoin with single spaces), exceeds `MAX_DESCRIPTION_LEN`
    /// (1024 chars). An over-limit description is an `InvalidField` parse
    /// error, which routes the skill into `outcome.errors` (not
    /// `outcome.skills`) with NO warning surfaced to the user — the skill just
    /// silently disappears from the agent's skill list. This test fails
    /// closed: it asserts the front-matter fence, the single folded
    /// `description: >` block, and its extracted content are all present and
    /// non-empty before checking the length, so a change in shape (e.g. losing
    /// the fence or the scalar) fails the test rather than the assertion being
    /// skipped.
    #[test]
    fn skill_description_within_codex_limit() {
        let content = SKILL_MD;
        assert!(
            content.starts_with("---\n"),
            "SKILL.md must open with a YAML front-matter fence"
        );
        let after_open = &content[4..];
        let close_idx = after_open
            .find("\n---")
            .expect("SKILL.md must have a closing front-matter fence");
        let front_matter = &after_open[..close_idx];

        let lines: Vec<&str> = front_matter.lines().collect();
        let desc_start = lines
            .iter()
            .position(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("description:") && trimmed["description:".len()..].trim() == ">"
            })
            .expect("front-matter must have exactly one `description: >` folded scalar");
        assert!(
            lines
                .iter()
                .filter(|line| line.trim_start().starts_with("description:"))
                .count()
                == 1,
            "expected exactly one `description:` key in the front-matter"
        );

        let key_re_is_top_level = |line: &str| -> bool {
            let mut chars = line.chars();
            match chars.next() {
                Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-' => {}
                _ => return false,
            }
            line.contains(':')
                && line
                    .split(':')
                    .next()
                    .unwrap()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };

        let mut block_lines = Vec::new();
        for line in &lines[desc_start + 1..] {
            if key_re_is_top_level(line) {
                break;
            }
            block_lines.push(*line);
        }
        assert!(
            !block_lines.is_empty(),
            "description block must not be empty (extraction failed closed)"
        );

        // Replicates Codex's `sanitize_single_line`: split on whitespace, join
        // with single spaces. For a `description: >` folded scalar this is
        // identical to what YAML parsing + sanitize would produce.
        let joined = block_lines.join(" ");
        let folded = joined.split_whitespace().collect::<Vec<_>>().join(" ");

        let len = folded.chars().count();
        assert!(
            len <= 1024,
            "description folds to {len} chars, exceeding Codex's \
             MAX_DESCRIPTION_LEN=1024 — the skill would be silently dropped \
             from Codex's skill list"
        );
        assert!(
            len <= 950,
            "description folds to {len} chars, exceeding the 950-char \
             maintenance margin kept below Codex's 1024 hard limit"
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
        assert!(
            r1.files
                .iter()
                .any(|f| f.action == FileAction::Created && f.path.ends_with("SKILL.md"))
        );
        assert!(
            r1.files
                .iter()
                .any(|f| f.action == FileAction::Created && f.path.ends_with(SIDECAR_FILE_NAME))
        );
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
        assert!(
            r2.files
                .iter()
                .any(|f| f.action == FileAction::Updated && f.path.ends_with("SKILL.md"))
        );
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
        assert!(
            r.files
                .iter()
                .any(|f| f.action == FileAction::Updated && f.path.ends_with("SKILL.md"))
        );
        assert_eq!(fs::read_to_string(skill_file(&parent)).unwrap(), SKILL_MD);

        let _ = fs::remove_dir_all(&parent);
    }

    // --- update preview + unified diff ---------------------------------------

    #[test]
    fn unified_diff_reports_stable_hunks_and_counts() {
        let diff = render_unified_diff(
            "alpha\nold\nomega\n",
            "alpha\nnew\nomega\n",
            "installed 1.0.0",
            "embedded 2.0.0",
        );
        assert_eq!(diff.added_lines, 1);
        assert_eq!(diff.removed_lines, 1);
        assert!(
            diff.text
                .starts_with("--- installed 1.0.0\n+++ embedded 2.0.0\n@@ -1,3 +1,3 @@\n")
        );
        assert!(diff.text.contains("-old\n"));
        assert!(diff.text.contains("+new\n"));
    }

    #[test]
    fn unified_diff_reports_missing_final_newline() {
        let diff = render_unified_diff("same", "same\n", "installed", "embedded");
        assert_eq!(diff.added_lines, 1);
        assert_eq!(diff.removed_lines, 1);
        assert!(diff.text.contains("\\ No newline at end of file\n"));
    }

    #[test]
    fn outdated_preview_carries_versions_and_never_writes() {
        let parent = temp_parent("preview-outdated");
        let drifted = "previously embedded skill\n";
        fs::create_dir_all(skill_dir(&parent)).unwrap();
        fs::write(skill_file(&parent), drifted).unwrap();
        let marker = SkillMarker {
            hash: git_blob_sha1(drifted.as_bytes()),
            version: "0.40.1".into(),
            installed_at: "t".into(),
        };
        fs::write(sidecar_file(&parent), marker.to_pretty_json()).unwrap();

        let preview = preview_update_for_dir(&parent, false);
        assert_eq!(preview.decision, SkillUpdateDecision::Update);
        assert_eq!(preview.status, SkillStatus::Outdated);
        assert_eq!(preview.installed_version.as_deref(), Some("0.40.1"));
        assert!(preview.added_lines > 0);
        assert_eq!(preview.removed_lines, 1);
        let diff = preview.unified_diff.expect("outdated preview has a diff");
        assert!(diff.contains("--- installed CodeGraph skill 0.40.1"));
        assert!(diff.contains(&format!("+++ embedded CodeGraph skill {EMBEDDED_VERSION}")));
        assert_eq!(fs::read_to_string(skill_file(&parent)).unwrap(), drifted);

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
        assert_eq!(
            status_details_for_dir(&parent).status,
            SkillStatus::NotInstalled
        );

        write_skill_to_dir(&parent, false);
        assert_eq!(
            status_details_for_dir(&parent).status,
            SkillStatus::UpToDate
        );

        // User edit, no provenance match ⇒ LocallyModified.
        fs::write(skill_file(&parent), "edited\n").unwrap();
        assert_eq!(
            status_details_for_dir(&parent).status,
            SkillStatus::LocallyModified
        );

        // Provenance match against drifted content ⇒ Outdated.
        let drifted = "old embedded\n";
        fs::write(skill_file(&parent), drifted).unwrap();
        let marker = SkillMarker {
            hash: git_blob_sha1(drifted.as_bytes()),
            version: "0.0.1".into(),
            installed_at: "t".into(),
        };
        fs::write(sidecar_file(&parent), marker.to_pretty_json()).unwrap();
        let details = status_details_for_dir(&parent);
        assert_eq!(details.status, SkillStatus::Outdated);
        assert_eq!(details.installed_version.as_deref(), Some("0.0.1"));

        let _ = fs::remove_dir_all(&parent);
    }
}
