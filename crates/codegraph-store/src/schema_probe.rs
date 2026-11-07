//! The schema version a database file holds, read without opening it (OD-19).
//!
//! A rebuild must not replace an index a newer codegraph wrote, and it decides
//! that before it changes a byte: before an explicit `init` removes a copied
//! project's state slots, before a lockless root gets its lock, before
//! `phase=building` is published. The version a newer binary recorded may still
//! sit in an un-checkpointed write-ahead log, and opening the database through
//! SQLite to read it would create `-wal`/`-shm` sidecars the newer binary's own
//! read gate refuses. So this reads the main file and the log into memory and
//! applies the log's committed frames the way SQLite's recovery does
//! (<https://www.sqlite.org/fileformat2.html#walformat>): the header's magic,
//! version and checksum; each frame's salts and cumulative checksum, stopping at
//! the first frame that fails them; every frame up to the last commit frame,
//! and the database size that frame records. The result is the database the
//! next opener would see, read through a private read-only in-memory handle.
//! Nothing on disk is created or written.

use std::io;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use crate::connection::{
    SchemaVersionReadError, SqliteImage, database_sidecar_path, deserialize_read_only,
};

/// What a database file holds, as its next opener would read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbedSchema {
    /// There is no database file.
    Absent,
    /// The file is not a SQLite database, so no codegraph wrote it.
    NotSqlite,
    /// A SQLite database recording this schema version (0 without a
    /// `schema_versions` table).
    Version(i64),
}

/// Read the schema version `db_path` holds, its committed write-ahead log
/// included, without creating or writing any file. Either way an error leaves
/// every file as it was.
pub(crate) fn probe_schema_version(db_path: &Path) -> Result<ProbedSchema, SchemaVersionReadError> {
    let read = |path: &Path| match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SchemaVersionReadError::Io {
            path: path.to_path_buf(),
            source,
        }),
    };
    let Some(mut image) = read(db_path)? else {
        return Ok(ProbedSchema::Absent);
    };
    let wal_path = database_sidecar_path(db_path, "-wal");
    if let Some(wal) = read(&wal_path)? {
        apply_committed_frames(&mut image, &wal);
    }
    if image.len() < 100 || !image.starts_with(b"SQLite format 3\0") {
        return Ok(ProbedSchema::NotSqlite);
    }
    let mut conn = Connection::open_in_memory()?;
    let image = SqliteImage::copy_from(&image).map_err(|source| SchemaVersionReadError::Io {
        path: db_path.to_path_buf(),
        source,
    })?;
    deserialize_read_only(&mut conn, image)?;
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_versions'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(ProbedSchema::Version(0));
    }
    let version = conn.query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    Ok(ProbedSchema::Version(version.unwrap_or(0)))
}

const WAL_HEADER_LEN: usize = 32;
const WAL_FRAME_HEADER_LEN: usize = 24;
const WAL_MAGIC_LITTLE_ENDIAN: u32 = 0x377f_0682;
const WAL_MAGIC_BIG_ENDIAN: u32 = 0x377f_0683;
const WAL_FORMAT_VERSION: u32 = 3_007_000;

/// Apply every frame of `wal` up to its last valid commit frame to `image`,
/// resizing it to the page count that frame records. A log whose header is
/// invalid holds no frames, as SQLite's recovery treats it.
fn apply_committed_frames(image: &mut Vec<u8>, wal: &[u8]) {
    let Some(header) = wal.get(..WAL_HEADER_LEN) else {
        return;
    };
    let magic = be32(header, 0);
    let big_endian = match magic {
        WAL_MAGIC_BIG_ENDIAN => true,
        WAL_MAGIC_LITTLE_ENDIAN => false,
        _ => return,
    };
    if be32(header, 4) != WAL_FORMAT_VERSION {
        return;
    }
    let page_size = be32(header, 8) as usize;
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return;
    }
    let salts = (be32(header, 16), be32(header, 20));
    let mut checksum = wal_checksum(&header[..24], (0, 0), big_endian);
    if checksum != (be32(header, 24), be32(header, 28)) {
        return;
    }

    let frame_len = WAL_FRAME_HEADER_LEN + page_size;
    let mut pending: Vec<(usize, &[u8])> = Vec::new();
    let mut committed: Vec<(usize, &[u8])> = Vec::new();
    let mut committed_pages: Option<usize> = None;
    let mut offset = WAL_HEADER_LEN;
    while let Some(frame) = wal.get(offset..offset + frame_len) {
        let page_number = be32(frame, 0) as usize;
        let pages_after_commit = be32(frame, 4) as usize;
        if page_number == 0 || (be32(frame, 8), be32(frame, 12)) != salts {
            break;
        }
        checksum = wal_checksum(&frame[..8], checksum, big_endian);
        checksum = wal_checksum(&frame[WAL_FRAME_HEADER_LEN..], checksum, big_endian);
        if checksum != (be32(frame, 16), be32(frame, 20)) {
            break;
        }
        pending.push((page_number, &frame[WAL_FRAME_HEADER_LEN..]));
        if pages_after_commit != 0 {
            committed.append(&mut pending);
            committed_pages = Some(pages_after_commit);
        }
        offset += frame_len;
    }

    let Some(pages) = committed_pages else {
        return;
    };
    image.resize(pages * page_size, 0);
    for (page_number, page) in committed {
        let start = (page_number - 1) * page_size;
        if let Some(slot) = image.get_mut(start..start + page_size) {
            slot.copy_from_slice(page);
        }
    }
}

/// SQLite's write-ahead-log checksum of `bytes` (a multiple of 8 bytes),
/// continuing from `seed`.
fn wal_checksum(bytes: &[u8], seed: (u32, u32), big_endian: bool) -> (u32, u32) {
    let word = |raw: [u8; 4]| {
        if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        }
    };
    let (mut s0, mut s1) = seed;
    let (pairs, _) = bytes.as_chunks::<8>();
    for [a, b, c, d, e, f, g, h] in pairs.iter().copied() {
        s0 = s0.wrapping_add(word([a, b, c, d])).wrapping_add(s1);
        s1 = s1.wrapping_add(word([e, f, g, h])).wrapping_add(s0);
    }
    (s0, s1)
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Dir(PathBuf);

    impl Dir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "codegraph-schema-probe-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A database at `dir/codegraph.db` whose main file records schema 10 and
    /// whose write-ahead log alone adds 11 (and one more row, so the log has
    /// more than one transaction). Returns copies of both files taken while
    /// the writer is still open, as a killed process leaves them.
    fn wal_only_newer(dir: &Path) -> (Vec<u8>, Vec<u8>) {
        let db = dir.join("source.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER, description TEXT);
             INSERT INTO schema_versions VALUES (10, 0, 'ten');
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO schema_versions VALUES (11, 0, 'eleven');
             CREATE TABLE later (x);
             INSERT INTO later VALUES (1);",
        )
        .unwrap();
        let main = std::fs::read(&db).unwrap();
        let wal = std::fs::read(database_sidecar_path(&db, "-wal")).unwrap();
        drop(conn);
        (main, wal)
    }

    fn plant(dir: &Path, name: &str, main: &[u8], wal: Option<&[u8]>) -> PathBuf {
        let db = dir.join(name);
        std::fs::write(&db, main).unwrap();
        if let Some(wal) = wal {
            std::fs::write(database_sidecar_path(&db, "-wal"), wal).unwrap();
        }
        db
    }

    /// What SQLite itself reads after recovering a copy of `main` + `wal`.
    fn sqlite_recovers(dir: &Path, main: &[u8], wal: Option<&[u8]>) -> i64 {
        let copy = plant(
            dir,
            &format!("oracle-{}.db", NEXT.fetch_add(1, Ordering::Relaxed)),
            main,
            wal,
        );
        let conn = Connection::open(&copy).unwrap();
        conn.query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn frame_len(wal: &[u8]) -> usize {
        WAL_FRAME_HEADER_LEN + be32(wal, 8) as usize
    }

    #[test]
    fn a_version_only_in_the_write_ahead_log_is_read_without_touching_a_file() {
        let work = Dir::new("wal-only");
        let (main, wal) = wal_only_newer(&work.0);
        let probe_dir = Dir::new("wal-only-probe");
        let db = plant(&probe_dir.0, "codegraph.db", &main, Some(&wal));
        let before = snapshot(&probe_dir.0);

        assert_eq!(
            probe_schema_version(&db).unwrap(),
            ProbedSchema::Version(11)
        );
        assert_eq!(snapshot(&probe_dir.0), before, "the probe changed a file");

        let main_only = Dir::new("wal-only-main");
        let db = plant(&main_only.0, "codegraph.db", &main, None);
        assert_eq!(
            probe_schema_version(&db).unwrap(),
            ProbedSchema::Version(10)
        );
        assert_eq!(sqlite_recovers(&work.0, &main, Some(&wal)), 11);
    }

    #[test]
    fn a_damaged_log_reads_as_sqlite_recovers_it() {
        let work = Dir::new("damaged");
        let (main, wal) = wal_only_newer(&work.0);
        let frame = frame_len(&wal);
        let frames = (wal.len() - WAL_HEADER_LEN) / frame;
        assert!(frames >= 2, "the fixture log has several frames");

        // Each damage, and the version SQLite's recovery is expected to read:
        // the log's later transactions go, the one recording 11 stays, unless
        // the damage reaches its first frame or the header.
        let mut cases: Vec<(&str, Vec<u8>, i64)> = Vec::new();
        // A torn tail: the last frame is half written.
        cases.push(("torn", wal[..wal.len() - frame / 2].to_vec(), 11));
        // The last frame's page is damaged, so its checksum fails.
        let mut bad_page = wal.clone();
        let last_page = wal.len() - 10;
        bad_page[last_page] ^= 0xff;
        cases.push(("checksum", bad_page, 11));
        // The first frame carries another log's salt.
        let mut bad_salt = wal.clone();
        bad_salt[WAL_HEADER_LEN + 8] ^= 0x01;
        cases.push(("salt", bad_salt, 10));
        // The header's checksum fails: the whole log is ignored.
        let mut bad_header = wal.clone();
        bad_header[12] ^= 0x01;
        cases.push(("header", bad_header, 10));

        for (label, damaged, expected) in cases {
            let dir = Dir::new(&format!("damaged-{label}"));
            let db = plant(&dir.0, "codegraph.db", &main, Some(&damaged));
            let probed = probe_schema_version(&db).unwrap();
            let recovered = sqlite_recovers(&work.0, &main, Some(&damaged));
            assert_eq!(recovered, expected, "{label}: SQLite's own recovery");
            assert_eq!(probed, ProbedSchema::Version(recovered), "{label}");
        }
    }

    #[test]
    fn what_is_not_a_sqlite_database_reads_as_such() {
        let dir = Dir::new("not-sqlite");
        assert_eq!(
            probe_schema_version(&dir.0.join("absent.db")).unwrap(),
            ProbedSchema::Absent
        );
        let empty = plant(&dir.0, "empty.db", b"", None);
        assert_eq!(
            probe_schema_version(&empty).unwrap(),
            ProbedSchema::NotSqlite
        );
        let garbage = plant(&dir.0, "garbage.db", &[7u8; 4096], None);
        assert_eq!(
            probe_schema_version(&garbage).unwrap(),
            ProbedSchema::NotSqlite
        );
    }

    #[test]
    fn a_database_without_schema_versions_is_version_zero() {
        let dir = Dir::new("no-table");
        let db = dir.0.join("plain.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE other (x); INSERT INTO other VALUES (1);")
            .unwrap();
        drop(conn);
        assert_eq!(probe_schema_version(&db).unwrap(), ProbedSchema::Version(0));
    }

    #[test]
    fn a_damaged_sqlite_database_is_an_error() {
        let dir = Dir::new("damaged-db");
        let db = dir.0.join("damaged.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER, description TEXT);
             INSERT INTO schema_versions VALUES (10, 0, 'ten');",
        )
        .unwrap();
        drop(conn);
        let mut bytes = std::fs::read(&db).unwrap();
        // Page 2 holds `schema_versions`; overwrite its b-tree header.
        let page_size = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
        bytes[page_size..page_size + 8].fill(0xff);
        std::fs::write(&db, &bytes).unwrap();
        assert!(matches!(
            probe_schema_version(&db),
            Err(SchemaVersionReadError::Sqlite(_))
        ));
    }
}
