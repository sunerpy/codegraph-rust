//! Public-surface contract tests for the read-only dual-slot classifier.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use codegraph_core::IndexPaths;
use codegraph_store::{
    CURRENT_EXTRACTION_VERSION, CURRENT_STORAGE_PROTOCOL, CorruptReason, EXTRACTION_VERSION_KEY,
    ExtractionStatus, SlotOutcome, StatePhase, canonical_checksum_payload, checksum_hex, classify,
    classify_slots,
};
use serde_json::{Value, json};

const OWNER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_OWNER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TempTree(PathBuf);

impl TempTree {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codegraph-index-state-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir(&path)
            .unwrap_or_else(|err| panic!("create temp tree {}: {err}", path.display()));
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn slots(&self) -> [PathBuf; 2] {
        [
            self.0.join("index-state.0.json"),
            self.0.join("index-state.1.json"),
        ]
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|err| panic!("remove temp tree {}: {err}", self.0.display()));
    }
}

fn wire_value(
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    owner: &str,
) -> Value {
    json!({
        "sequence": sequence,
        "storageProtocol": storage_protocol,
        "extractionVersion": extraction_version,
        "phase": phase,
        "projectIdentity": owner,
        "checksum": checksum_hex(sequence, storage_protocol, extraction_version, phase, owner),
    })
}

fn wire_bytes(
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    owner: &str,
) -> Vec<u8> {
    serde_json::to_vec(&wire_value(
        sequence,
        storage_protocol,
        extraction_version,
        phase,
        owner,
    ))
    .expect("serialize state fixture")
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes)
        .unwrap_or_else(|err| panic!("write state fixture {}: {err}", path.display()));
}

fn write_wire(
    path: &Path,
    sequence: u64,
    storage_protocol: u64,
    extraction_version: u64,
    phase: &str,
    owner: &str,
) {
    write_bytes(
        path,
        &wire_bytes(sequence, storage_protocol, extraction_version, phase, owner),
    );
}

fn assert_corrupt(
    classification: &codegraph_store::IndexStateClassification,
    expected: impl FnOnce(&CorruptReason) -> bool,
) {
    match classification.status() {
        ExtractionStatus::Corrupt { reason } => {
            assert!(expected(reason), "unexpected corruption reason: {reason:?}");
        }
        status => panic!("expected Corrupt, got {status:?}"),
    }
}

#[test]
fn index_state_constants_and_canonical_payload_are_exact() {
    assert_eq!(CURRENT_STORAGE_PROTOCOL, 2);
    assert_eq!(CURRENT_EXTRACTION_VERSION, 3);
    assert_eq!(EXTRACTION_VERSION_KEY, "indexed_with_extraction_version");
    assert_eq!(
        canonical_checksum_payload(7, 2, 3, "future-phase", OWNER),
        format!(
            "codegraph-index-state-v1\nsequence=7\nstorageProtocol=2\n\
             extractionVersion=3\nphase=future-phase\nprojectIdentity={OWNER}\n"
        )
    );
    let digest = checksum_hex(7, 2, 3, "future-phase", OWNER);
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn index_state_both_missing_is_missing() {
    let tree = TempTree::new("missing");
    let result = classify_slots(&tree.slots(), OWNER);
    assert_eq!(result.status(), &ExtractionStatus::Missing);
    assert_eq!(result.authoritative(), None);
    assert!(matches!(result.slot(0), SlotOutcome::Absent));
    assert!(matches!(result.slot(1), SlotOutcome::Absent));
}

#[test]
fn index_state_current_protocol_maps_each_phase() {
    for (phase, expected) in [
        ("current", ExtractionStatus::Current),
        (
            "building",
            ExtractionStatus::Building {
                built: CURRENT_EXTRACTION_VERSION,
            },
        ),
        ("uninitialized", ExtractionStatus::Uninitialized),
    ] {
        let tree = TempTree::new(phase);
        write_wire(
            &tree.slots()[0],
            1,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            phase,
            OWNER,
        );
        assert_eq!(
            classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER)).status(),
            &expected
        );
    }
}

#[test]
fn index_state_extraction_version_maps_outdated_and_future_for_every_phase() {
    let future_extraction = CURRENT_EXTRACTION_VERSION + 1;
    for phase in ["building", "current", "uninitialized"] {
        let old = TempTree::new("outdated");
        write_wire(
            &old.slots()[0],
            1,
            CURRENT_STORAGE_PROTOCOL,
            1,
            phase,
            OWNER,
        );
        assert_eq!(
            classify_slots(&old.slots(), OWNER).status(),
            &ExtractionStatus::Outdated { built: 1 }
        );

        let previous = TempTree::new("previous-extraction");
        write_wire(
            &previous.slots()[0],
            1,
            CURRENT_STORAGE_PROTOCOL,
            2,
            phase,
            OWNER,
        );
        assert_eq!(
            classify_slots(&previous.slots(), OWNER).status(),
            &ExtractionStatus::Outdated { built: 2 }
        );

        let future = TempTree::new("future-extraction");
        write_wire(
            &future.slots()[0],
            1,
            CURRENT_STORAGE_PROTOCOL,
            future_extraction,
            phase,
            OWNER,
        );
        assert_eq!(
            classify_slots(&future.slots(), OWNER).status(),
            &ExtractionStatus::Future {
                built: future_extraction
            },
            "future extraction must dominate phase {phase}"
        );
    }
}

#[test]
fn index_state_unknown_fields_key_order_and_whitespace_are_ignored() {
    let tree = TempTree::new("formatting");
    let checksum = checksum_hex(
        9,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        OWNER,
    );
    let formatted = format!(
        "{{\n  \"unknownFutureField\": {{\"nested\": true}},\n  \
         \"checksum\": \"{checksum}\", \"projectIdentity\": \"{OWNER}\",\n  \
         \"phase\": \"current\", \"extractionVersion\": {CURRENT_EXTRACTION_VERSION},\n  \
         \"storageProtocol\": {CURRENT_STORAGE_PROTOCOL}, \"sequence\": 9\n}}\n"
    );
    write_bytes(&tree.slots()[1], formatted.as_bytes());
    let result = classify_slots(&tree.slots(), OWNER);
    assert_eq!(result.status(), &ExtractionStatus::Current);
    assert_eq!(result.authoritative().expect("authority").index, 1);
}

#[test]
fn index_state_missing_and_wrong_typed_required_fields_are_malformed() {
    for value in [
        json!({
            "storageProtocol": CURRENT_STORAGE_PROTOCOL,
            "extractionVersion": CURRENT_EXTRACTION_VERSION,
            "phase": "current",
            "projectIdentity": OWNER,
            "checksum": "0".repeat(64),
        }),
        json!({
            "sequence": "1",
            "storageProtocol": CURRENT_STORAGE_PROTOCOL,
            "extractionVersion": CURRENT_EXTRACTION_VERSION,
            "phase": "current",
            "projectIdentity": OWNER,
            "checksum": "0".repeat(64),
        }),
        json!([]),
    ] {
        let tree = TempTree::new("malformed-shape");
        write_bytes(&tree.slots()[0], &serde_json::to_vec(&value).unwrap());
        let result = classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER));
        assert_corrupt(&result, |reason| {
            matches!(reason, CorruptReason::MalformedJson { .. })
        });
    }
}

#[test]
fn index_state_bad_owner_and_checksum_shapes_are_typed_corruption() {
    let bad_owners = [
        "abc",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ];
    for owner in bad_owners {
        let tree = TempTree::new("bad-owner-shape");
        write_wire(
            &tree.slots()[0],
            1,
            3,
            CURRENT_EXTRACTION_VERSION,
            "future-phase",
            owner,
        );
        let result = classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER));
        assert_corrupt(&result, |reason| {
            matches!(reason, CorruptReason::InvalidOwnerEncoding { .. })
        });
    }

    for checksum in ["abc".to_string(), "A".repeat(64), "g".repeat(64)] {
        let tree = TempTree::new("bad-checksum-shape");
        let mut value = wire_value(1, 3, CURRENT_EXTRACTION_VERSION, "future-phase", OWNER);
        value["checksum"] = Value::String(checksum);
        write_bytes(&tree.slots()[0], &serde_json::to_vec(&value).unwrap());
        let result = classify_slots(&tree.slots(), OWNER);
        assert_corrupt(&result, |reason| {
            matches!(reason, CorruptReason::InvalidChecksumEncoding { .. })
        });
    }
}

#[test]
fn index_state_checksum_and_owner_mismatch_are_corrupt_even_for_future_protocol() {
    let checksum_tree = TempTree::new("checksum-mismatch");
    let mut value = wire_value(1, 3, CURRENT_EXTRACTION_VERSION, "future-phase", OWNER);
    value["checksum"] = Value::String("0".repeat(64));
    write_bytes(
        &checksum_tree.slots()[0],
        &serde_json::to_vec(&value).unwrap(),
    );
    let result = classify_unchanged(checksum_tree.path(), || {
        classify_slots(&checksum_tree.slots(), OWNER)
    });
    assert_corrupt(&result, |reason| {
        matches!(reason, CorruptReason::ChecksumMismatch { .. })
    });

    let owner_tree = TempTree::new("owner-mismatch");
    write_wire(
        &owner_tree.slots()[0],
        1,
        3,
        CURRENT_EXTRACTION_VERSION,
        "future-phase",
        OTHER_OWNER,
    );
    let result = classify_unchanged(owner_tree.path(), || {
        classify_slots(&owner_tree.slots(), OWNER)
    });
    assert_corrupt(&result, |reason| {
        matches!(reason, CorruptReason::OwnerMismatch { .. })
    });
}

#[test]
fn index_state_current_unknown_phase_is_corrupt_but_future_unknown_phase_is_valid() {
    let current = TempTree::new("unknown-current-phase");
    write_wire(
        &current.slots()[0],
        1,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "future-phase",
        OWNER,
    );
    let result = classify_slots(&current.slots(), OWNER);
    assert_corrupt(
        &result,
        |reason| matches!(reason, CorruptReason::UnknownPhase { phase, .. } if phase == "future-phase"),
    );

    let future = TempTree::new("unknown-future-phase");
    write_wire(&future.slots()[0], 1, 3, 44, "future-phase", OWNER);
    let result = classify_slots(&future.slots(), OWNER);
    assert_eq!(result.status(), &ExtractionStatus::Future { built: 44 });
    let authority = result.authoritative().expect("future authority");
    assert_eq!(authority.record.phase, None);
    assert_eq!(authority.record.phase_raw, "future-phase");
    assert!(matches!(result.slot(0), SlotOutcome::FutureProtocol(_)));
}

#[test]
fn index_state_lower_and_zero_storage_protocol_are_corrupt() {
    for protocol in [0, 1] {
        let tree = TempTree::new("lower-protocol");
        write_wire(
            &tree.slots()[0],
            1,
            protocol,
            CURRENT_EXTRACTION_VERSION,
            "current",
            OWNER,
        );
        let result = classify_slots(&tree.slots(), OWNER);
        assert_corrupt(
            &result,
            |reason| matches!(reason, CorruptReason::UnsupportedStorageProtocol { found, .. } if *found == protocol),
        );
    }
}

#[test]
fn index_state_any_malformed_present_slot_dominates_a_valid_companion() {
    let tree = TempTree::new("invalid-dominance");
    write_bytes(&tree.slots()[0], b"not json");
    write_wire(
        &tree.slots()[1],
        99,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        OWNER,
    );
    let result = classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER));
    assert_corrupt(&result, |reason| {
        matches!(reason, CorruptReason::MalformedJson { slot: 0, .. })
    });
    assert_eq!(result.authoritative(), None);
}

#[test]
fn index_state_non_regular_and_unreadable_slots_are_corrupt() {
    let directory = TempTree::new("slot-directory");
    std::fs::create_dir(&directory.slots()[0]).unwrap();
    let result = classify_unchanged(directory.path(), || {
        classify_slots(&directory.slots(), OWNER)
    });
    assert_corrupt(&result, |reason| {
        matches!(
            reason,
            CorruptReason::NotARegularFile {
                kind: "directory",
                ..
            }
        )
    });

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = TempTree::new("slot-symlink");
        let target = link.path().join("target.json");
        write_wire(
            &target,
            1,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            "current",
            OWNER,
        );
        symlink(&target, &link.slots()[0]).unwrap();
        let result = classify_unchanged(link.path(), || classify_slots(&link.slots(), OWNER));
        assert_corrupt(&result, |reason| {
            matches!(
                reason,
                CorruptReason::NotARegularFile {
                    kind: "symlink",
                    ..
                }
            )
        });
    }

    let unreadable = TempTree::new("slot-unreadable");
    let too_long = unreadable.path().join(OsString::from("x".repeat(4096)));
    let result = classify_slots(&[too_long, unreadable.slots()[1].clone()], OWNER);
    assert_corrupt(&result, |reason| {
        matches!(reason, CorruptReason::UnreadableSlot { slot: 0, .. })
    });
}

#[test]
fn index_state_future_protocol_dominates_current_in_both_sequence_directions() {
    for (future_sequence, current_sequence) in [(1, 100), (100, 1)] {
        let tree = TempTree::new("future-dominance");
        write_wire(
            &tree.slots()[0],
            current_sequence,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            "current",
            OWNER,
        );
        write_wire(
            &tree.slots()[1],
            future_sequence,
            3,
            77,
            "future-phase",
            OWNER,
        );
        let result = classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER));
        assert_eq!(result.status(), &ExtractionStatus::Future { built: 77 });
        assert_eq!(result.authoritative().expect("future authority").index, 1);
    }
}

#[test]
fn index_state_equal_current_sequences_are_corrupt_for_identical_and_reformatted_json() {
    let identical = TempTree::new("equal-identical");
    let bytes = wire_bytes(
        7,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        OWNER,
    );
    write_bytes(&identical.slots()[0], &bytes);
    write_bytes(&identical.slots()[1], &bytes);
    let result = classify_unchanged(identical.path(), || {
        classify_slots(&identical.slots(), OWNER)
    });
    assert_corrupt(&result, |reason| {
        matches!(
            reason,
            CorruptReason::EqualSequence {
                sequence: 7,
                identical_payload: true
            }
        )
    });

    let reformatted = TempTree::new("equal-reformatted");
    write_bytes(&reformatted.slots()[0], &bytes);
    let checksum = checksum_hex(
        7,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        OWNER,
    );
    let other = format!(
        "{{ \"checksum\":\"{checksum}\", \"phase\":\"current\", \
         \"projectIdentity\":\"{OWNER}\", \
         \"extractionVersion\":{CURRENT_EXTRACTION_VERSION}, \
         \"storageProtocol\":{CURRENT_STORAGE_PROTOCOL}, \"sequence\":7 }}"
    );
    write_bytes(&reformatted.slots()[1], other.as_bytes());
    let result = classify_unchanged(reformatted.path(), || {
        classify_slots(&reformatted.slots(), OWNER)
    });
    assert_corrupt(&result, |reason| {
        matches!(
            reason,
            CorruptReason::EqualSequence {
                sequence: 7,
                identical_payload: false
            }
        )
    });
}

#[test]
fn index_state_equal_future_and_mixed_sequences_are_corrupt_before_future_dominance() {
    let future = TempTree::new("equal-future");
    write_wire(&future.slots()[0], 8, 3, 9, "alpha", OWNER);
    write_wire(&future.slots()[1], 8, 4, 10, "beta", OWNER);
    let result = classify_unchanged(future.path(), || classify_slots(&future.slots(), OWNER));
    assert_corrupt(&result, |reason| {
        matches!(reason, CorruptReason::EqualSequence { sequence: 8, .. })
    });

    let mixed = TempTree::new("equal-mixed");
    write_wire(
        &mixed.slots()[0],
        8,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        OWNER,
    );
    write_wire(&mixed.slots()[1], 8, 3, 10, "future-phase", OWNER);
    let result = classify_unchanged(mixed.path(), || classify_slots(&mixed.slots(), OWNER));
    assert_corrupt(&result, |reason| {
        matches!(reason, CorruptReason::EqualSequence { sequence: 8, .. })
    });
}

#[test]
fn index_state_selected_max_sequence_is_corrupt_before_status_mapping() {
    for (protocol, extraction, phase) in [
        (
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            "current",
        ),
        (3, 99, "future-phase"),
    ] {
        let tree = TempTree::new("max-sequence");
        write_wire(
            &tree.slots()[0],
            u64::MAX,
            protocol,
            extraction,
            phase,
            OWNER,
        );
        let result = classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER));
        assert_corrupt(
            &result,
            |reason| matches!(reason, CorruptReason::SequenceExhausted { sequence } if *sequence == u64::MAX),
        );
        assert_eq!(
            result
                .authoritative()
                .expect("selected authority")
                .record
                .sequence,
            u64::MAX
        );
    }
}

#[test]
fn index_state_highest_ordinary_current_slot_is_authoritative_with_full_metadata() {
    let tree = TempTree::new("authority");
    write_wire(
        &tree.slots()[0],
        4,
        CURRENT_STORAGE_PROTOCOL,
        1,
        "building",
        OWNER,
    );
    write_wire(
        &tree.slots()[1],
        5,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        OWNER,
    );
    let result = classify_unchanged(tree.path(), || classify_slots(&tree.slots(), OWNER));
    assert_eq!(result.status(), &ExtractionStatus::Current);
    let authority = result.authoritative().expect("authority");
    assert_eq!(authority.index, 1);
    assert_eq!(authority.path, tree.slots()[1]);
    assert_eq!(authority.record.sequence, 5);
    assert_eq!(authority.record.storage_protocol, 2);
    assert_eq!(
        authority.record.extraction_version,
        CURRENT_EXTRACTION_VERSION
    );
    assert_eq!(authority.record.phase, Some(StatePhase::Current));
    assert_eq!(authority.record.phase_raw, "current");
    assert_eq!(authority.record.project_identity, OWNER);
    assert_eq!(
        authority.record.checksum,
        checksum_hex(
            5,
            CURRENT_STORAGE_PROTOCOL,
            CURRENT_EXTRACTION_VERSION,
            "current",
            OWNER
        )
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotKind {
    Directory,
    File(Vec<u8>),
    Symlink(PathBuf),
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, SnapshotKind> {
    fn walk(root: &Path, directory: &Path, out: &mut BTreeMap<PathBuf, SnapshotKind>) {
        let entries = std::fs::read_dir(directory)
            .unwrap_or_else(|err| panic!("snapshot read_dir {}: {err}", directory.display()));
        let mut paths = entries
            .map(|entry| {
                entry
                    .unwrap_or_else(|err| {
                        panic!("snapshot entry in {}: {err}", directory.display())
                    })
                    .path()
            })
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let relative = path
                .strip_prefix(root)
                .unwrap_or_else(|err| panic!("snapshot strip {}: {err}", path.display()))
                .to_path_buf();
            let metadata = std::fs::symlink_metadata(&path)
                .unwrap_or_else(|err| panic!("snapshot metadata {}: {err}", path.display()));
            let file_type = metadata.file_type();
            let kind = if file_type.is_dir() {
                SnapshotKind::Directory
            } else if file_type.is_file() {
                SnapshotKind::File(
                    std::fs::read(&path)
                        .unwrap_or_else(|err| panic!("snapshot read {}: {err}", path.display())),
                )
            } else if file_type.is_symlink() {
                SnapshotKind::Symlink(
                    std::fs::read_link(&path).unwrap_or_else(|err| {
                        panic!("snapshot read_link {}: {err}", path.display())
                    }),
                )
            } else {
                panic!("snapshot unsupported entry kind: {}", path.display());
            };
            assert!(
                out.insert(relative, kind).is_none(),
                "duplicate snapshot path"
            );
            if file_type.is_dir() {
                walk(root, &path, out);
            }
        }
    }

    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn assert_snapshot_unchanged(
    before: &BTreeMap<PathBuf, SnapshotKind>,
    after: &BTreeMap<PathBuf, SnapshotKind>,
) {
    let changed = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<BTreeSet<_>>();
    let total = changed.len();
    let bounded = changed.iter().take(16).cloned().collect::<Vec<_>>();
    assert!(
        changed.is_empty(),
        "filesystem snapshot changed at {bounded:?}{}",
        if total > bounded.len() {
            format!(" (and {} more paths)", total - bounded.len())
        } else {
            String::new()
        }
    );
}

fn classify_unchanged(
    root: &Path,
    classify_once: impl FnOnce() -> codegraph_store::IndexStateClassification,
) -> codegraph_store::IndexStateClassification {
    let before = snapshot_tree(root);
    let classification = classify_once();
    let after = snapshot_tree(root);
    assert_snapshot_unchanged(&before, &after);
    classification
}

#[test]
fn index_state_classifier_is_exactly_byte_nonmutating_and_never_opens_sqlite() {
    let project = TempTree::new("nonmutation");
    let paths = IndexPaths::resolve(project.path(), None).expect("resolve paths");
    std::fs::create_dir_all(paths.current_root().join("empty-directory")).unwrap();
    write_bytes(&paths.current_db(), b"not sqlite; immutable trap bytes");
    write_bytes(&paths.permanent_lock(), b"lock trap");
    write_wire(
        &paths.state_slots()[0],
        1,
        CURRENT_STORAGE_PROTOCOL,
        CURRENT_EXTRACTION_VERSION,
        "current",
        paths.project_identity(),
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink("codegraph.db", paths.current_root().join("db-link")).unwrap();

    let result = classify_unchanged(project.path(), || classify(&paths));
    assert_eq!(result.status(), &ExtractionStatus::Current);
}

#[test]
fn index_state_snapshot_detects_equal_length_byte_mutation() {
    let tree = TempTree::new("snapshot-bytes");
    let file = tree.path().join("same-length.bin");
    write_bytes(&file, b"AAAA");
    let before = snapshot_tree(tree.path());
    write_bytes(&file, b"BBBB");
    let after = snapshot_tree(tree.path());
    let caught = std::panic::catch_unwind(|| assert_snapshot_unchanged(&before, &after));
    assert!(
        caught.is_err(),
        "equal-length byte mutation must be detected"
    );
}

#[cfg(unix)]
#[test]
fn index_state_snapshot_records_symlink_target_without_following_it() {
    use std::os::unix::fs::symlink;

    let tree = TempTree::new("snapshot-symlink");
    let outside = TempTree::new("snapshot-outside");
    let target = outside.path().join("target.bin");
    write_bytes(&target, b"AAAA");
    symlink(&target, tree.path().join("link")).unwrap();
    let before = snapshot_tree(tree.path());
    assert!(
        matches!(before.get(Path::new("link")), Some(SnapshotKind::Symlink(found)) if found == &target)
    );

    write_bytes(&target, b"BBBB");
    let after_target_write = snapshot_tree(tree.path());
    assert_snapshot_unchanged(&before, &after_target_write);

    std::fs::remove_file(tree.path().join("link")).unwrap();
    let second_target = outside.path().join("second.bin");
    write_bytes(&second_target, b"BBBB");
    symlink(&second_target, tree.path().join("link")).unwrap();
    let after_retarget = snapshot_tree(tree.path());
    let caught = std::panic::catch_unwind(|| assert_snapshot_unchanged(&before, &after_retarget));
    assert!(caught.is_err(), "symlink retarget must be detected");
}
