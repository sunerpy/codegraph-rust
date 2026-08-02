//! Read-only index-state authority: the dual fixed state slots and the typed
//! classification derived from them.
//!
//! Frozen plan `upstream-v1.5-portable-fixes.md`, Batch M ("Store ownership,
//! version gate, and final publication", plan lines 452-522). This module owns
//! the SINGLE SOURCE OF TRUTH for the storage protocol / extraction version and
//! the persisted state-slot contract, plus the classifier that turns the two
//! fixed slots into an [`ExtractionStatus`].
//!
//! # Scope of the classifier: READ-ONLY
//!
//! The classifier in this module is deliberately NONMUTATING and self-contained:
//!
//! - it never creates, truncates, renames, or deletes any file or directory
//!   (not the current root, the permanent lock, a state slot, a temp file, the
//!   database, a sidecar, or the tombstone);
//! - it never opens SQLite, so it cannot run schema setup, migrations, WAL
//!   changes, or `ANALYZE`;
//! - publication lives in the separate `index_state_publisher` module and
//!   consumes this classifier without weakening its aggregate ordering. This
//!   classifier itself remains strictly read-only;
//! - `Store::open_for_read` / `open_for_status` / `open_for_write` and
//!   `Store::stamp_extraction_version` consume this classifier without changing
//!   it. Migration mode, uninit lifecycle, and the daemon/watcher lifecycle land
//!   in later Batch M slices.
//!
//! This classifier boundary is narrower than the commit series containing it:
//! existing CLI indexing still writes `project_metadata`, but now takes that
//! metadata key and extraction version (`3`) from this store-owned module. That
//! pre-existing DB stamping is not state-slot publication and is not performed by
//! this classifier.
//!
//! [`checksum_hex`] and [`canonical_checksum_payload`] are pure functions (no
//! I/O); they define the byte representation the publisher reproduces, and the
//! tests use them to author protocol fixtures.
//!
//! # Persisted slot contract
//!
//! The two fixed files [`IndexPaths::state_slots`] returns —
//! `<current_root>/index-state.0.json` and `<current_root>/index-state.1.json` —
//! each hold ONE JSON object with these canonical fields:
//!
//! ```json
//! {
//!   "sequence": 7,
//!   "storageProtocol": 2,
//!   "extractionVersion": 3,
//!   "phase": "current",
//!   "projectIdentity": "<64 lowercase hex>",
//!   "checksum": "<64 lowercase hex>"
//! }
//! ```
//!
//! - `sequence`, `storageProtocol`, `extractionVersion` are JSON unsigned
//!   integers (`u64`); `phase`, `projectIdentity`, `checksum` are JSON strings.
//!   These six names and their JSON types are PROTOCOL-STABLE: a future storage
//!   protocol may add fields and may add `phase` vocabulary, but must keep these
//!   six readable so an older binary can still recognize a future namespace.
//! - Unknown fields are IGNORED (forward compatibility). Missing or wrong-typed
//!   required fields are `Corrupt`.
//! - `phase` is exactly one of `building`, `current`, `uninitialized` for
//!   storage protocol [`CURRENT_STORAGE_PROTOCOL`]; any other value is `Corrupt`.
//! - `checksum` is the lowercase 64-hex SHA-256 of [`canonical_checksum_payload`],
//!   which is a fixed ASCII, LF-terminated, field-ordered byte string. It does
//!   NOT depend on JSON key order, whitespace, pretty-printing, host endianness,
//!   locale, or line-ending conventions.
//!
//! # Classification order (deterministic, typed, never string-matched)
//!
//! 1. **Structural defects dominate.** A present slot that cannot be stat'd or
//!    read, is not a regular file (a directory or a statically observed symlink at
//!    a fixed slot path is an INVALID slot), changes identity while being read, or
//!    whose bytes are not a JSON object with
//!    the six canonical fields at their canonical types ⇒ [`ExtractionStatus::Corrupt`].
//!    A present invalid slot is NEVER ignored because the other slot is valid:
//!    a future writer may have authored it, so it is security-relevant.
//! 2. **Stable fields are validated before protocol trust.** Every parsed slot,
//!    including a future-protocol slot, must carry lowercase 64-hex owner and
//!    checksum strings, a checksum matching the canonical payload (which hashes
//!    the RAW phase text), and the expected owner. A lower/zero storage protocol
//!    is unsupported. The first defect, scanning slot 0 then slot 1, is `Corrupt`.
//!    Current protocol accepts exactly the three known phases; future protocol
//!    may use an unknown phase only after those stable checks pass.
//! 3. No validated record at all ⇒ [`ExtractionStatus::Missing`]. A missing
//!    INACTIVE slot is allowed.
//! 4. Two validated records with the SAME `sequence` ⇒ `Corrupt`, whether they
//!    are current/current, future/future, or mixed, and regardless of raw JSON
//!    formatting.
//! 5. A validated future-protocol record dominates a current-protocol companion
//!    regardless of sequence (the highest future sequence wins). Otherwise the
//!    highest current-protocol record is AUTHORITATIVE. An
//!    authoritative sequence of [`u64::MAX`] is sequence exhaustion ⇒ `Corrupt`
//!    (and nonmutating, like every other outcome).
//! 6. Future storage protocol ⇒ [`ExtractionStatus::Future`]. Otherwise,
//!    `extractionVersion` above [`CURRENT_EXTRACTION_VERSION`] ⇒
//!    [`ExtractionStatus::Future`] for EVERY phase, `uninitialized` included.
//!    Below ⇒ [`ExtractionStatus::Outdated`] preserving the built version. Equal
//!    ⇒ [`ExtractionStatus::Current`], [`ExtractionStatus::Building`], or
//!    [`ExtractionStatus::Uninitialized`] by phase.
//!
//! Callers must branch on the typed variants and on [`CorruptReason`]; the
//! `Display` renderings exist for operator messages only and are never parsed.

use std::fmt;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use codegraph_core::IndexPaths;
use serde::Deserialize;

/// The on-disk storage-protocol version this binary defines. A slot above this
/// is [`ExtractionStatus::Future`]; below (or zero) is `Corrupt`.
pub const CURRENT_STORAGE_PROTOCOL: u64 = 2;

/// The extraction-pipeline version this binary produces. This is the single
/// source of truth: no other crate may define its own copy.
pub const CURRENT_EXTRACTION_VERSION: u64 = 3;

/// The `project_metadata` key under which a built index records the extraction
/// version it was produced with. Single source of truth for the key spelling.
pub const EXTRACTION_VERSION_KEY: &str = "indexed_with_extraction_version";

/// Domain-separation prefix of the canonical checksum payload. Bumping it would
/// invalidate every existing checksum, so it is versioned independently of the
/// storage protocol.
const CHECKSUM_DOMAIN: &str = "codegraph-index-state-v1";

/// The lifecycle phase a state slot records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatePhase {
    /// A destructive (re)build is in progress; the DB may be absent or partial.
    Building,
    /// The namespace holds a completely published index.
    Current,
    /// A `uninit --force` was in progress when the namespace last advanced.
    Uninitialized,
}

impl StatePhase {
    /// The exact wire spelling stored in `phase` and hashed into the checksum.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Current => "current",
            Self::Uninitialized => "uninitialized",
        }
    }

    /// Parse a wire `phase` value. `None` for any value outside the exactly
    /// three supported phases (the caller turns that into `Corrupt`).
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "building" => Some(Self::Building),
            "current" => Some(Self::Current),
            "uninitialized" => Some(Self::Uninitialized),
            _ => None,
        }
    }
}

impl fmt::Display for StatePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// The canonical byte representation the [`checksum_hex`] digest covers.
///
/// It is a pure ASCII, LF-separated, LF-terminated, fixed-field-order string:
///
/// ```text
/// codegraph-index-state-v1\n
/// sequence=<decimal u64>\n
/// storageProtocol=<decimal u64>\n
/// extractionVersion=<decimal u64>\n
/// phase=<raw JSON phase string>\n
/// projectIdentity=<64 lowercase hex>\n
/// ```
///
/// The labels, separators, decimal integers, and line endings are exactly the
/// ASCII bytes shown above. `phase` and `projectIdentity` are inserted verbatim
/// as UTF-8 (current phases and valid identities are ASCII); no escaping or
/// normalization is performed. The final byte is one LF. Integer rendering is
/// locale-independent, so the payload is unaffected by JSON key order,
/// whitespace, pretty-printing, endianness, or host line-ending conventions.
#[must_use]
pub fn canonical_checksum_payload(
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    project_identity: &str,
) -> String {
    format!(
        "{CHECKSUM_DOMAIN}\nsequence={sequence}\nstorageProtocol={storage_protocol}\n\
         extractionVersion={extraction_version}\nphase={phase}\nprojectIdentity={project_identity}\n"
    )
}

/// The lowercase 64-hex SHA-256 of [`canonical_checksum_payload`].
///
/// The digest comes from `codegraph_core::node_id::hash_content`, the workspace's
/// existing public exact-SHA-256-hex utility. Reusing it keeps this crate free of
/// a direct `sha2` dependency (and therefore leaves `Cargo.lock` byte-identical)
/// without weakening the hash: `hash_content` is a plain SHA-256 over the UTF-8
/// bytes of its argument, and the canonical payload is pure ASCII.
#[must_use]
pub fn checksum_hex(
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    project_identity: &str,
) -> String {
    codegraph_core::node_id::hash_content(&canonical_checksum_payload(
        sequence,
        storage_protocol,
        extraction_version,
        phase,
        project_identity,
    ))
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The canonical fields of one parsed slot.
///
/// `phase` is `None` only for a future-storage-protocol slot, whose phase
/// vocabulary this binary does not define; for a current-protocol slot it is
/// always `Some` (an unknown phase is `Corrupt`, never a valid record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSlotRecord {
    /// Monotonic publication sequence; the highest valid one is authoritative.
    pub sequence: u64,
    /// On-disk storage protocol of this slot.
    pub storage_protocol: u64,
    /// Extraction-pipeline version the namespace was built with.
    pub extraction_version: u64,
    /// Parsed phase, or `None` for a future-protocol slot.
    pub phase: Option<StatePhase>,
    /// The verbatim `phase` string as stored.
    pub phase_raw: String,
    /// Owning project identity (`IndexPaths::project_identity`).
    pub project_identity: String,
    /// The verbatim `checksum` string as stored.
    pub checksum: String,
}

/// Why a namespace is [`ExtractionStatus::Corrupt`]. Typed so callers branch on
/// the variant; the `Display` rendering is for operator messages only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorruptReason {
    /// A present slot could not be stat'd or read.
    UnreadableSlot {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// Stable I/O detail.
        detail: String,
    },
    /// The fixed slot path exists but is not a regular file (for example a
    /// directory or a symlink, which is recorded and refused, never followed).
    NotARegularFile {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// The observed entry kind.
        kind: &'static str,
    },
    /// The fixed slot path stopped naming the same regular filesystem entry
    /// between the initial no-follow metadata check, opening the read handle,
    /// and the post-read no-follow corroboration.
    SlotChangedDuringRead {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// Stable diagnostic without platform-specific identity values.
        detail: String,
    },
    /// The bytes are not a JSON object carrying the six canonical fields at
    /// their canonical JSON types.
    MalformedJson {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// Stable parser detail.
        detail: String,
    },
    /// `projectIdentity` is not exactly 64 lowercase hexadecimal characters.
    InvalidOwnerEncoding {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// The rejected identity.
        found: String,
    },
    /// `checksum` is not exactly 64 lowercase hexadecimal characters.
    InvalidChecksumEncoding {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// The rejected checksum.
        found: String,
    },
    /// `phase` is outside the three supported values.
    UnknownPhase {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// The verbatim rejected `phase` value.
        phase: String,
    },
    /// The stored `checksum` does not equal the recomputed one.
    ChecksumMismatch {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// Recomputed checksum.
        expected: String,
        /// Checksum found in the file.
        found: String,
    },
    /// `storageProtocol` is below [`CURRENT_STORAGE_PROTOCOL`] (zero included);
    /// such a slot is unsupported, not merely old.
    UnsupportedStorageProtocol {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// The rejected protocol value.
        found: u64,
        /// The protocol this binary defines.
        supported: u64,
    },
    /// `projectIdentity` does not match the owner identity of the namespace.
    OwnerMismatch {
        /// Slot index (0 or 1).
        slot: u8,
        /// Slot path.
        path: PathBuf,
        /// The owner identity this binary computed.
        expected: String,
        /// The identity stored in the slot.
        found: String,
    },
    /// Both slots are valid and carry the same `sequence`. This is `Corrupt`
    /// whether the payload bytes are identical or different: the publication
    /// protocol never produces two slots at one sequence, so the namespace's
    /// history is unreconstructible either way.
    EqualSequence {
        /// The duplicated sequence.
        sequence: u64,
        /// Whether the two slots' bytes are identical.
        identical_payload: bool,
    },
    /// The authoritative sequence is [`u64::MAX`], so no successor can be
    /// published. Reported without mutating anything.
    SequenceExhausted {
        /// The exhausted sequence.
        sequence: u64,
    },
}

impl fmt::Display for CorruptReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnreadableSlot { slot, path, detail } => write!(
                f,
                "index state slot {slot} ({}) could not be read: {detail}",
                path.display()
            ),
            Self::NotARegularFile { slot, path, kind } => write!(
                f,
                "index state slot {slot} ({}) is a {kind}, not a regular file",
                path.display()
            ),
            Self::SlotChangedDuringRead { slot, path, detail } => write!(
                f,
                "index state slot {slot} ({}) changed while being read: {detail}",
                path.display()
            ),
            Self::MalformedJson { slot, path, detail } => write!(
                f,
                "index state slot {slot} ({}) is malformed: {detail}",
                path.display()
            ),
            Self::InvalidOwnerEncoding { slot, path, found } => write!(
                f,
                "index state slot {slot} ({}) has invalid project identity encoding {found:?}",
                path.display()
            ),
            Self::InvalidChecksumEncoding { slot, path, found } => write!(
                f,
                "index state slot {slot} ({}) has invalid checksum encoding {found:?}",
                path.display()
            ),
            Self::UnknownPhase { slot, path, phase } => write!(
                f,
                "index state slot {slot} ({}) has unknown phase {phase:?}",
                path.display()
            ),
            Self::ChecksumMismatch {
                slot,
                path,
                expected,
                found,
            } => write!(
                f,
                "index state slot {slot} ({}) checksum mismatch: expected {expected}, found {found}",
                path.display()
            ),
            Self::UnsupportedStorageProtocol {
                slot,
                path,
                found,
                supported,
            } => write!(
                f,
                "index state slot {slot} ({}) has unsupported storage protocol {found} \
                 (this binary supports {supported})",
                path.display()
            ),
            Self::OwnerMismatch {
                slot,
                path,
                expected,
                found,
            } => write!(
                f,
                "index state slot {slot} ({}) belongs to project identity {found}, not {expected}",
                path.display()
            ),
            Self::EqualSequence {
                sequence,
                identical_payload,
            } => {
                let payload = if *identical_payload {
                    "identical"
                } else {
                    "differing"
                };
                write!(
                    f,
                    "both index state slots are valid at sequence {sequence} with {payload} payloads"
                )
            }
            Self::SequenceExhausted { sequence } => write!(
                f,
                "index state sequence is exhausted at {sequence}; no successor can be published"
            ),
        }
    }
}

/// The typed classification of a v2 namespace, derived from the two fixed slots
/// alone. Equivalent to the frozen plan's `ExtractionStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractionStatus {
    /// A complete index built by this extraction version.
    Current,
    /// A destructive build was in progress at this extraction version.
    Building {
        /// The extraction version the interrupted build was producing.
        built: u64,
    },
    /// A `uninit --force` was in progress when the namespace last advanced.
    Uninitialized,
    /// No state slot exists at all.
    Missing,
    /// Built by an older extraction version; a rebuild is required.
    Outdated {
        /// The older extraction version the namespace was built with.
        built: u64,
    },
    /// Written by a newer extraction version or storage protocol. This binary
    /// must not read, migrate, or delete the namespace.
    Future {
        /// The newer extraction version recorded on disk.
        built: u64,
    },
    /// The namespace cannot be interpreted; manual recovery is required.
    Corrupt {
        /// The typed reason.
        reason: CorruptReason,
    },
}

impl fmt::Display for ExtractionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current => f.write_str("current"),
            Self::Building { built } => write!(f, "building (extraction version {built})"),
            Self::Uninitialized => f.write_str("uninitialized (interrupted uninit)"),
            Self::Missing => f.write_str("missing"),
            Self::Outdated { built } => {
                write!(f, "outdated (built with extraction version {built})")
            }
            Self::Future { built } => write!(f, "future (built with extraction version {built})"),
            Self::Corrupt { reason } => write!(f, "corrupt: {reason}"),
        }
    }
}

/// The slot the classification treated as authoritative, with enough metadata
/// for the later lease / `Store::open_for_*` slices to re-verify it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritativeSlot {
    /// Slot index (0 or 1).
    pub index: u8,
    /// Absolute slot path.
    pub path: PathBuf,
    /// The parsed record.
    pub record: StateSlotRecord,
}

/// The per-slot outcome, kept for inspection independently of the aggregate
/// [`ExtractionStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotOutcome {
    /// The slot file does not exist (allowed for the inactive slot).
    Absent,
    /// The slot parsed and passed every current-protocol check.
    Valid(StateSlotRecord),
    /// The slot parsed but declares a storage protocol above this binary's.
    FutureProtocol(StateSlotRecord),
    /// The slot is present and invalid.
    Invalid(CorruptReason),
}

/// The complete result of one read-only classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStateClassification {
    status: ExtractionStatus,
    authoritative: Option<AuthoritativeSlot>,
    slots: [SlotOutcome; 2],
}

impl IndexStateClassification {
    /// The aggregate status.
    #[must_use]
    pub fn status(&self) -> &ExtractionStatus {
        &self.status
    }

    /// The authoritative slot, when one was selected. `None` for
    /// [`ExtractionStatus::Missing`] and for a `Corrupt` outcome caused by a
    /// defective or duplicated slot.
    #[must_use]
    pub fn authoritative(&self) -> Option<&AuthoritativeSlot> {
        self.authoritative.as_ref()
    }

    /// The per-slot outcome for slot `index` (0 or 1).
    ///
    /// # Panics
    ///
    /// Panics if `index` is not 0 or 1; the slot layout is a fixed pair.
    #[must_use]
    pub fn slot(&self, index: usize) -> &SlotOutcome {
        assert!(index < 2, "index state has exactly two fixed slots");
        &self.slots[index]
    }
}

/// Classify the namespace owned by `paths`, reading ONLY the two fixed slots.
///
/// Consumes [`IndexPaths::state_slots`] and [`IndexPaths::project_identity`]
/// rather than reconstructing paths or owner identity. Nonmutating: see the
/// module docs.
#[must_use]
pub fn classify(paths: &IndexPaths) -> IndexStateClassification {
    classify_slots(&paths.state_slots(), paths.project_identity())
}

/// Classify from exact slot paths and an exact owner identity.
///
/// The unit-test seam for [`classify`], and the form later slices use when they
/// already hold the resolved slot pair. Nonmutating: see the module docs.
#[must_use]
pub fn classify_slots(slots: &[PathBuf; 2], owner_identity: &str) -> IndexStateClassification {
    let raw = [read_slot(0, &slots[0]), read_slot(1, &slots[1])];
    classify_raw(&raw, slots, owner_identity)
}

/// One slot as observed on disk, before semantic validation.
#[derive(Debug, Clone)]
enum RawSlot {
    Absent,
    Invalid(CorruptReason),
    Parsed { wire: WireSlot, bytes: Vec<u8> },
}

/// The wire form. Unknown fields are ignored; every canonical field is required
/// at its canonical JSON type.
#[derive(Debug, Clone, Deserialize)]
struct WireSlot {
    sequence: u64,
    #[serde(rename = "storageProtocol")]
    storage_protocol: u64,
    #[serde(rename = "extractionVersion")]
    extraction_version: u64,
    phase: String,
    #[serde(rename = "projectIdentity")]
    project_identity: String,
    checksum: String,
}

fn read_slot(index: u8, path: &Path) -> RawSlot {
    read_slot_with(index, path, |_| {})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadCheckpoint {
    InitialMetadataValidated,
    HandleOpened,
    BytesRead,
}

/// Read one fixed slot from one opened handle and corroborate that the path
/// still names the same entry. The callback is private and exists solely so the
/// unit test can replace a slot at an exact checkpoint without a timing race.
fn read_slot_with(index: u8, path: &Path, mut checkpoint: impl FnMut(ReadCheckpoint)) -> RawSlot {
    let initial = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return RawSlot::Absent,
        Err(err) => {
            return RawSlot::Invalid(CorruptReason::UnreadableSlot {
                slot: index,
                path: path.to_path_buf(),
                detail: err.to_string(),
            });
        }
    };
    let file_type = initial.file_type();
    if !file_type.is_file() {
        let kind = if file_type.is_dir() {
            "directory"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "non-regular filesystem entry"
        };
        return RawSlot::Invalid(CorruptReason::NotARegularFile {
            slot: index,
            path: path.to_path_buf(),
            kind,
        });
    }
    checkpoint(ReadCheckpoint::InitialMetadataValidated);

    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) => {
            return RawSlot::Invalid(CorruptReason::UnreadableSlot {
                slot: index,
                path: path.to_path_buf(),
                detail: err.to_string(),
            });
        }
    };
    let opened = match file.metadata() {
        Ok(meta) => meta,
        Err(err) => {
            return RawSlot::Invalid(CorruptReason::UnreadableSlot {
                slot: index,
                path: path.to_path_buf(),
                detail: format!("opened-handle metadata failed: {err}"),
            });
        }
    };
    if !opened.is_file() {
        return slot_changed(index, path, "opened handle is not a regular file");
    }
    if !same_file_identity(&initial, &opened) {
        return slot_changed(index, path, "entry identity changed before open");
    }
    checkpoint(ReadCheckpoint::HandleOpened);

    let mut bytes = Vec::new();
    if let Err(err) = file.read_to_end(&mut bytes) {
        return RawSlot::Invalid(CorruptReason::UnreadableSlot {
            slot: index,
            path: path.to_path_buf(),
            detail: err.to_string(),
        });
    }
    checkpoint(ReadCheckpoint::BytesRead);

    let final_meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => {
            return slot_changed(
                index,
                path,
                &format!("path disappeared or became unreadable after read: {err}"),
            );
        }
    };
    if !final_meta.file_type().is_file() {
        return slot_changed(index, path, "path no longer names a regular file");
    }
    if !same_file_identity(&opened, &final_meta) {
        return slot_changed(index, path, "entry identity changed during read");
    }

    #[cfg(windows)]
    match path_still_names_opened_file(&file, path) {
        Ok(true) => {}
        Ok(false) => {
            return slot_changed(
                index,
                path,
                "entry identity changed during Windows handle corroboration",
            );
        }
        Err(err) => {
            return slot_changed(
                index,
                path,
                &format!("Windows handle corroboration failed: {err}"),
            );
        }
    }

    match serde_json::from_slice::<WireSlot>(&bytes) {
        Ok(wire) => RawSlot::Parsed { wire, bytes },
        Err(err) => RawSlot::Invalid(CorruptReason::MalformedJson {
            slot: index,
            path: path.to_path_buf(),
            detail: err.to_string(),
        }),
    }
}

fn slot_changed(index: u8, path: &Path, detail: &str) -> RawSlot {
    RawSlot::Invalid(CorruptReason::SlotChangedDuringRead {
        slot: index,
        path: path.to_path_buf(),
        detail: detail.to_string(),
    })
}

fn same_file_identity(left: &Metadata, right: &Metadata) -> bool {
    crate::file_identity::metadata_observation_matches(left, right)
}

#[cfg(windows)]
fn path_still_names_opened_file(opened: &File, path: &Path) -> io::Result<bool> {
    crate::file_identity::path_still_names_file(path, opened)
}

/// Semantically validate ONE parsed slot into a [`SlotOutcome`].
///
/// Order is fixed and typed: owner encoding → checksum encoding → checksum over
/// the raw phase text → owner equality → supported protocol → current-protocol
/// phase vocabulary. A future protocol is trusted only after every stable field
/// check; its phase vocabulary may be unknown.
fn validate_parsed(index: u8, path: &Path, wire: &WireSlot, owner_identity: &str) -> SlotOutcome {
    if !is_lowercase_sha256(&wire.project_identity) {
        return SlotOutcome::Invalid(CorruptReason::InvalidOwnerEncoding {
            slot: index,
            path: path.to_path_buf(),
            found: wire.project_identity.clone(),
        });
    }
    if !is_lowercase_sha256(&wire.checksum) {
        return SlotOutcome::Invalid(CorruptReason::InvalidChecksumEncoding {
            slot: index,
            path: path.to_path_buf(),
            found: wire.checksum.clone(),
        });
    }

    let expected = checksum_hex(
        wire.sequence,
        wire.storage_protocol,
        wire.extraction_version,
        &wire.phase,
        &wire.project_identity,
    );
    if expected != wire.checksum {
        return SlotOutcome::Invalid(CorruptReason::ChecksumMismatch {
            slot: index,
            path: path.to_path_buf(),
            expected,
            found: wire.checksum.clone(),
        });
    }

    if wire.project_identity != owner_identity {
        return SlotOutcome::Invalid(CorruptReason::OwnerMismatch {
            slot: index,
            path: path.to_path_buf(),
            expected: owner_identity.to_string(),
            found: wire.project_identity.clone(),
        });
    }

    if wire.storage_protocol < CURRENT_STORAGE_PROTOCOL {
        return SlotOutcome::Invalid(CorruptReason::UnsupportedStorageProtocol {
            slot: index,
            path: path.to_path_buf(),
            found: wire.storage_protocol,
            supported: CURRENT_STORAGE_PROTOCOL,
        });
    }

    let phase = StatePhase::from_wire(&wire.phase);
    let record = StateSlotRecord {
        sequence: wire.sequence,
        storage_protocol: wire.storage_protocol,
        extraction_version: wire.extraction_version,
        phase,
        phase_raw: wire.phase.clone(),
        project_identity: wire.project_identity.clone(),
        checksum: wire.checksum.clone(),
    };

    if wire.storage_protocol > CURRENT_STORAGE_PROTOCOL {
        return SlotOutcome::FutureProtocol(record);
    }

    if phase.is_none() {
        return SlotOutcome::Invalid(CorruptReason::UnknownPhase {
            slot: index,
            path: path.to_path_buf(),
            phase: wire.phase.clone(),
        });
    }

    SlotOutcome::Valid(record)
}

/// Aggregate two observed slots into one classification.
///
/// Dominance order, applied before any authority selection:
///
/// 1. A present INVALID slot (unreadable, non-regular, malformed, unknown phase,
///    checksum mismatch, unsupported lower protocol, owner mismatch) is
///    `Corrupt`. It dominates even a future-protocol companion: a defective
///    present fixed slot always requires manual recovery, and both outcomes are
///    equally nonmutating, so the stricter one is reported. Slot 0 is inspected
///    before slot 1 so the reason is deterministic.
/// 2. Equal sequence across ANY two validated records is `Corrupt`, before
///    future dominance and without relying on raw JSON byte equality.
/// 3. A future-STORAGE-PROTOCOL slot dominates a valid current-protocol
///    companion regardless of sequence; otherwise current records decide.
/// 4. The selected authority is rejected at `u64::MAX` before Future/status
///    mapping; then protocol/extraction version/phase maps the status.
fn classify_raw(
    raw: &[RawSlot; 2],
    slots: &[PathBuf; 2],
    owner_identity: &str,
) -> IndexStateClassification {
    let outcomes: [SlotOutcome; 2] = [
        observe(0, &slots[0], &raw[0], owner_identity),
        observe(1, &slots[1], &raw[1], owner_identity),
    ];

    for outcome in &outcomes {
        if let SlotOutcome::Invalid(reason) = outcome {
            return IndexStateClassification {
                status: ExtractionStatus::Corrupt {
                    reason: reason.clone(),
                },
                authoritative: None,
                slots: outcomes,
            };
        }
    }

    let validated: Vec<(u8, &StateSlotRecord)> = outcomes
        .iter()
        .enumerate()
        .filter_map(|(index, outcome)| match outcome {
            SlotOutcome::Valid(record) | SlotOutcome::FutureProtocol(record) => {
                Some((u8::try_from(index).unwrap_or(u8::MAX), record))
            }
            _ => None,
        })
        .collect();

    if validated.is_empty() {
        return IndexStateClassification {
            status: ExtractionStatus::Missing,
            authoritative: None,
            slots: outcomes,
        };
    }

    if let [(_, first), (_, second)] = validated.as_slice()
        && first.sequence == second.sequence
    {
        let identical_payload = match (&raw[0], &raw[1]) {
            (RawSlot::Parsed { bytes: left, .. }, RawSlot::Parsed { bytes: right, .. }) => {
                left == right
            }
            _ => false,
        };
        return IndexStateClassification {
            status: ExtractionStatus::Corrupt {
                reason: CorruptReason::EqualSequence {
                    sequence: first.sequence,
                    identical_payload,
                },
            },
            authoritative: None,
            slots: outcomes,
        };
    }

    let future = pick_highest(&outcomes, slots, |outcome| match outcome {
        SlotOutcome::FutureProtocol(record) => Some(record),
        _ => None,
    });
    let authoritative = future.unwrap_or_else(|| {
        pick_highest(&outcomes, slots, |outcome| match outcome {
            SlotOutcome::Valid(record) => Some(record),
            _ => None,
        })
        .expect("at least one validated slot exists here")
    });

    if authoritative.record.sequence == u64::MAX {
        return IndexStateClassification {
            status: ExtractionStatus::Corrupt {
                reason: CorruptReason::SequenceExhausted {
                    sequence: authoritative.record.sequence,
                },
            },
            authoritative: Some(authoritative),
            slots: outcomes,
        };
    }

    let built = authoritative.record.extraction_version;
    let status = if authoritative.record.storage_protocol > CURRENT_STORAGE_PROTOCOL
        || built > CURRENT_EXTRACTION_VERSION
    {
        // Future dominance applies to EVERY phase, `uninitialized` included: a
        // future-extraction `uninitialized` slot is never treated as a
        // recoverable interrupted uninit.
        ExtractionStatus::Future { built }
    } else if built < CURRENT_EXTRACTION_VERSION {
        ExtractionStatus::Outdated { built }
    } else {
        match authoritative.record.phase {
            Some(StatePhase::Current) => ExtractionStatus::Current,
            Some(StatePhase::Building) => ExtractionStatus::Building { built },
            Some(StatePhase::Uninitialized) => ExtractionStatus::Uninitialized,
            // Unreachable: a current-protocol valid slot always carries a phase.
            None => ExtractionStatus::Corrupt {
                reason: CorruptReason::UnknownPhase {
                    slot: authoritative.index,
                    path: authoritative.path.clone(),
                    phase: authoritative.record.phase_raw.clone(),
                },
            },
        }
    };

    IndexStateClassification {
        status,
        authoritative: Some(authoritative),
        slots: outcomes,
    }
}

fn observe(index: u8, path: &Path, raw: &RawSlot, owner_identity: &str) -> SlotOutcome {
    match raw {
        RawSlot::Absent => SlotOutcome::Absent,
        RawSlot::Invalid(reason) => SlotOutcome::Invalid(reason.clone()),
        RawSlot::Parsed { wire, .. } => validate_parsed(index, path, wire, owner_identity),
    }
}

/// Select the highest-sequence slot among the outcomes `select` accepts; ties
/// cannot occur here because equal sequences are rejected earlier, so slot 0
/// wins a hypothetical tie deterministically.
fn pick_highest<'a>(
    outcomes: &'a [SlotOutcome; 2],
    slots: &[PathBuf; 2],
    select: impl Fn(&'a SlotOutcome) -> Option<&'a StateSlotRecord>,
) -> Option<AuthoritativeSlot> {
    let mut best: Option<AuthoritativeSlot> = None;
    for (index, outcome) in outcomes.iter().enumerate() {
        let Some(record) = select(outcome) else {
            continue;
        };
        let replace = best
            .as_ref()
            .is_none_or(|current| record.sequence > current.record.sequence);
        if replace {
            best = Some(AuthoritativeSlot {
                index: u8::try_from(index).unwrap_or(u8::MAX),
                path: slots[index].clone(),
                record: record.clone(),
            });
        }
    }
    best
}

#[cfg(test)]
mod read_tests {
    use super::*;

    #[test]
    fn regular_slot_replaced_after_validation_is_typed_corruption() {
        let root = std::env::temp_dir().join(format!(
            "codegraph-slot-replacement-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).expect("create replacement test root");
        let slot = root.join("index-state.0.json");
        let replacement = root.join("replacement.json");
        std::fs::write(&slot, b"original").expect("write original slot");
        std::fs::write(&replacement, b"replacement").expect("write replacement slot");

        let mut replaced = false;
        let observed = read_slot_with(0, &slot, |point| {
            if point == ReadCheckpoint::InitialMetadataValidated && !replaced {
                std::fs::remove_file(&slot).expect("remove validated slot");
                std::fs::rename(&replacement, &slot).expect("install replacement slot");
                replaced = true;
            }
        });

        assert!(matches!(
            observed,
            RawSlot::Invalid(CorruptReason::SlotChangedDuringRead { slot: 0, .. })
        ));
        std::fs::remove_dir_all(&root).expect("remove replacement test root");
    }
}
