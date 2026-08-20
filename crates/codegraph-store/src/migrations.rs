use rusqlite::{Connection, OptionalExtension, params};

use crate::schema::BASE_SCHEMA;

pub const CURRENT_SCHEMA_VERSION: i64 = 8;
pub const FRESH_SCHEMA_DESCRIPTION: &str = "Initial schema includes all migrations";

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 2,
        description: "Add project metadata, provenance tracking, and unresolved ref context",
        sql: r#"
        CREATE TABLE IF NOT EXISTS project_metadata (
          key TEXT PRIMARY KEY,
          value TEXT NOT NULL,
          updated_at INTEGER NOT NULL
        );
        ALTER TABLE unresolved_refs ADD COLUMN file_path TEXT NOT NULL DEFAULT '';
        ALTER TABLE unresolved_refs ADD COLUMN language TEXT NOT NULL DEFAULT 'unknown';
        ALTER TABLE edges ADD COLUMN provenance TEXT DEFAULT NULL;
        CREATE INDEX IF NOT EXISTS idx_unresolved_file_path ON unresolved_refs(file_path);
        CREATE INDEX IF NOT EXISTS idx_edges_provenance ON edges(provenance);
      "#,
        adds_column: Some(("unresolved_refs", "file_path")),
    },
    Migration {
        version: 3,
        description: "Add lower(name) expression index for memory-efficient case-insensitive lookups",
        sql: r#"
        CREATE INDEX IF NOT EXISTS idx_nodes_lower_name ON nodes(lower(name));
      "#,
        adds_column: None,
    },
    Migration {
        version: 4,
        description: "Drop redundant idx_edges_source / idx_edges_target (covered by source_kind / target_kind composites)",
        sql: r#"
        DROP INDEX IF EXISTS idx_edges_source;
        DROP INDEX IF EXISTS idx_edges_target;
      "#,
        adds_column: None,
    },
    Migration {
        version: 5,
        description: "Add nodes.return_type — normalized return/result type for receiver-type inference (C++ singletons/factories, #645)",
        sql: r#"
        ALTER TABLE nodes ADD COLUMN return_type TEXT;
      "#,
        adds_column: Some(("nodes", "return_type")),
    },
    Migration {
        version: 6,
        description: "Add unresolved_refs.reference_subkind — structural extraction label (Godot edge subkind)",
        sql: r#"
        ALTER TABLE unresolved_refs ADD COLUMN reference_subkind TEXT;
      "#,
        adds_column: Some(("unresolved_refs", "reference_subkind")),
    },
    Migration {
        version: 7,
        description: "Dedup duplicate edge rows and add a UNIQUE identity index so INSERT OR IGNORE actually dedups (#1034)",
        sql: r#"
        DELETE FROM edges
        WHERE id NOT IN (
          SELECT MIN(id) FROM edges
          GROUP BY source, target, kind, IFNULL(line, -1), IFNULL(col, -1)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_identity
          ON edges(source, target, kind, IFNULL(line, -1), IFNULL(col, -1));
      "#,
        adds_column: None,
    },
    Migration {
        version: 8,
        description: "Add files.generated — content-header generated-file detection (#1500)",
        // DDL ONLY, deliberately no backfill: the flag derives from file CONTENT
        // and this table stores `content_hash`, not bytes. Backfilling would mean
        // re-reading every file from disk inside a schema step, on paths that may
        // no longer exist. Existing rows therefore stay 0 until re-extraction,
        // and readers UNION the column with the path check so a pre-#1500 index
        // keeps the demotion it already had.
        sql: r#"
        ALTER TABLE files ADD COLUMN generated INTEGER NOT NULL DEFAULT 0;
        CREATE INDEX IF NOT EXISTS idx_files_generated ON files(path) WHERE generated = 1;
      "#,
        adds_column: Some(("files", "generated")),
    },
];

#[derive(Debug, Clone, Copy)]
struct Migration {
    version: i64,
    description: &'static str,
    sql: &'static str,
    /// A `(table, column)` this migration ADDS. When the column is already
    /// present the `sql` is skipped and only the version row is recorded.
    ///
    /// Needed because `ALTER TABLE` has no `IF NOT EXISTS` and
    /// `run_pending_migrations` can legitimately replay a migration over a
    /// database created from a NEWER `BASE_SCHEMA` that already carries the
    /// column. Measured: without this, replaying migration 8 over a
    /// `BASE_SCHEMA` database fails with `duplicate column name: generated`.
    adds_column: Option<(&'static str, &'static str)>,
}

pub fn ensure_schema_and_migrations(conn: &mut Connection) -> rusqlite::Result<()> {
    if get_current_version(conn)? == 0 {
        initialize_fresh_schema(conn)?;
    }

    run_pending_migrations(conn)?;

    // The upstream golden `.schema` includes sqlite_stat1 from maintenance/ANALYZE;
    // rusqlite's bundled SQLite may also emit sqlite_stat4, which the upstream lacks.
    // ANALYZE costs ~100ms+ on large DBs and was previously run on EVERY open. The
    // `.schema` oracle compares the sqlite_stat1 TABLE DEFINITION, not its rows, so
    // skipping re-ANALYZE once sqlite_stat1 exists keeps `.schema` byte-identical
    // while removing the per-open floor that every `sync` paid.
    if !has_sqlite_stat1(conn)? {
        conn.execute_batch("ANALYZE")?;
        conn.execute_batch("DROP TABLE IF EXISTS sqlite_stat4")?;
    }
    Ok(())
}

fn has_sqlite_stat1(conn: &Connection) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_stat1'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Sets `auto_vacuum=INCREMENTAL` on a brand-new DB, returning freelist pages to the
/// OS later via `incremental_vacuum`. SQLite only honours `auto_vacuum` on an empty DB
/// before any page is written — it must run BEFORE `journal_mode=WAL` (which writes the
/// header) and before any table DDL; changing it afterwards would require a full VACUUM
/// that reorders `.schema` and breaks golden Tier-1. We therefore gate on "no tables yet"
/// so existing auto_vacuum=NONE DBs are left untouched. INCREMENTAL keeps `.schema` text
/// identical to a NONE DB (only the unread file-header flag differs).
pub fn configure_auto_vacuum_for_fresh_db(conn: &Connection) -> rusqlite::Result<()> {
    let has_any_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_any_table {
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    }
    Ok(())
}

pub fn get_current_version(conn: &Connection) -> rusqlite::Result<i64> {
    let has_schema_versions = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_versions'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();

    if !has_schema_versions {
        return Ok(0);
    }

    let version = conn
        .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .optional()?;

    Ok(version.flatten().unwrap_or(0))
}

fn initialize_fresh_schema(conn: &mut Connection) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(BASE_SCHEMA)?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version, applied_at, description) VALUES (?, ?, ?)",
        params![
            CURRENT_SCHEMA_VERSION,
            now_millis(),
            FRESH_SCHEMA_DESCRIPTION
        ],
    )?;
    tx.commit()
}

fn run_pending_migrations(conn: &mut Connection) -> rusqlite::Result<()> {
    let current = get_current_version(conn)?;
    let mut pending = MIGRATIONS
        .iter()
        .copied()
        .filter(|migration| migration.version > current)
        .collect::<Vec<_>>();
    pending.sort_by_key(|migration| migration.version);

    for migration in pending {
        let tx = conn.transaction()?;
        let already_applied = match migration.adds_column {
            Some((table, column)) => table_has_column(&tx, table, column)?,
            None => false,
        };
        if !already_applied {
            tx.execute_batch(migration.sql)?;
        }
        record_migration(&tx, migration.version, migration.description)?;
        tx.commit()?;
    }

    Ok(())
}

/// Whether `table` already declares `column`, read from `PRAGMA table_info`.
///
/// A missing table reports `false`, so a guarded migration still runs its `sql`
/// and fails loudly there rather than being silently skipped.
fn table_has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn record_migration(
    conn: &Connection,
    version: i64,
    description: &'static str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO schema_versions (version, applied_at, description) VALUES (?, ?, ?)",
        params![version, now_millis(), description],
    )?;
    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_millis()
        .try_into()
        .expect("current epoch milliseconds must fit in i64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_memory_connection_reports_version_zero() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), 0);
    }

    #[test]
    fn fresh_schema_sets_current_version_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        ensure_schema_and_migrations(&mut conn).unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);

        ensure_schema_and_migrations(&mut conn).unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn analyze_runs_once_and_leaves_sqlite_stat1() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert!(!has_sqlite_stat1(&conn).unwrap());
        ensure_schema_and_migrations(&mut conn).unwrap();
        assert!(has_sqlite_stat1(&conn).unwrap());
    }

    #[test]
    fn auto_vacuum_incremental_on_fresh_db_only() {
        let conn = Connection::open_in_memory().unwrap();
        configure_auto_vacuum_for_fresh_db(&conn).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );

        conn.execute_batch("CREATE TABLE t (id INTEGER)").unwrap();
        configure_auto_vacuum_for_fresh_db(&conn).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn pending_migrations_apply_over_a_v1_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER, description TEXT);
             CREATE TABLE unresolved_refs (id INTEGER PRIMARY KEY AUTOINCREMENT, from_node_id TEXT, reference_name TEXT, reference_kind TEXT, line INTEGER, col INTEGER, candidates TEXT);
             CREATE TABLE edges (id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT, target TEXT, kind TEXT, metadata TEXT, line INTEGER, col INTEGER);
             CREATE TABLE nodes (id TEXT PRIMARY KEY, name TEXT);
             CREATE TABLE files (path TEXT PRIMARY KEY, content_hash TEXT NOT NULL, language TEXT NOT NULL, size INTEGER NOT NULL, modified_at INTEGER NOT NULL, indexed_at INTEGER NOT NULL, node_count INTEGER DEFAULT 0, errors TEXT);
             CREATE INDEX idx_edges_source ON edges(source);
             CREATE INDEX idx_edges_target ON edges(target);
             INSERT INTO schema_versions (version, applied_at, description) VALUES (1, 0, 'base');",
        )
        .unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), 1);

        run_pending_migrations(&mut conn).unwrap();

        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(has_column(&conn, "nodes", "return_type"));
        assert!(has_column(&conn, "unresolved_refs", "reference_subkind"));
        assert!(has_column(&conn, "unresolved_refs", "file_path"));
    }

    fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        names.iter().any(|n| n == column)
    }

    fn has_index(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?",
            params![name],
            |_| Ok(()),
        )
        .optional()
        .unwrap()
        .is_some()
    }

    fn edge_ids(conn: &Connection) -> Vec<i64> {
        let mut stmt = conn.prepare("SELECT id FROM edges ORDER BY id").unwrap();
        stmt.query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect()
    }

    // Builds a v7-shaped `files` table (the 8 pre-#1500 columns) with one row, so
    // migration 8's effect on a PRE-EXISTING row is observable.
    fn seed_v7_schema_with_one_file(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT);
             CREATE TABLE files (path TEXT PRIMARY KEY, content_hash TEXT NOT NULL, language TEXT NOT NULL, size INTEGER NOT NULL, modified_at INTEGER NOT NULL, indexed_at INTEGER NOT NULL, node_count INTEGER DEFAULT 0, errors TEXT);
             INSERT INTO schema_versions (version, applied_at, description) VALUES (7, 0, 'v7');
             INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at, node_count, errors)
               VALUES ('payroll.go', 'cafebabe', 'go', 172, 0, 0, 3, NULL);",
        )
        .unwrap();
    }

    #[test]
    fn migration_v8_adds_generated_column_defaulting_to_zero() {
        let mut conn = Connection::open_in_memory().unwrap();
        seed_v7_schema_with_one_file(&conn);
        assert_eq!(get_current_version(&conn).unwrap(), 7);
        assert!(!has_column(&conn, "files", "generated"));

        run_pending_migrations(&mut conn).unwrap();

        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(has_column(&conn, "files", "generated"));
        assert!(has_index(&conn, "idx_files_generated"));

        // DDL only: the pre-existing row reads 0 rather than being backfilled,
        // because the verdict derives from CONTENT this migration cannot see.
        // Readers UNION the column with the path check, which is what keeps a
        // pre-#1500 index ranking the way it already did.
        let generated: i64 = conn
            .query_row(
                "SELECT generated FROM files WHERE path = 'payroll.go'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generated, 0);

        // NOT NULL, so a row inserted without the column still reads 0.
        conn.execute_batch(
            "INSERT INTO files (path, content_hash, language, size, modified_at, indexed_at)
               VALUES ('other.go', 'f00d', 'go', 1, 0, 0);",
        )
        .unwrap();
        let nulls: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE generated IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nulls, 0);
    }

    #[test]
    fn migration_v8_is_idempotent_when_the_column_already_exists() {
        // Settles S4's guard question by MEASUREMENT: `ALTER TABLE` has no
        // `IF NOT EXISTS`, so replaying migration 8 over a database whose
        // BASE_SCHEMA already carries `generated` would raise
        // `duplicate column name: generated` — and if it does, the migration
        // needs a `PRAGMA table_info` guard rather than a plain `&'static str`.
        let mut conn = Connection::open_in_memory().unwrap();
        ensure_schema_and_migrations(&mut conn).unwrap();
        assert!(has_column(&conn, "files", "generated"));

        // Rewind the recorded version to exactly 7 so ONLY migration 8 is
        // pending, over a table that already has the column — the replay the
        // guard must survive. `initialize_fresh_schema` records just two rows,
        // 1 and CURRENT_SCHEMA_VERSION, so a row 7 has to be inserted rather
        // than uncovered by deleting 8: dropping 8 alone would leave MAX = 1 and
        // make migrations 2..8 pending, which tests a different thing.
        conn.execute("DELETE FROM schema_versions WHERE version = 8", [])
            .unwrap();
        conn.execute(
            "INSERT INTO schema_versions (version, applied_at, description) VALUES (7, 0, 'v7')",
            [],
        )
        .unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), 7);

        let replayed = run_pending_migrations(&mut conn);

        // Whichever way this lands, the column and index must still be intact
        // and the version restored, so the assertion is meaningful either way.
        assert!(
            replayed.is_ok(),
            "replaying migration 8 over a BASE_SCHEMA database must not fail: {:?}",
            replayed.err()
        );
        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(has_column(&conn, "files", "generated"));
        assert!(has_index(&conn, "idx_files_generated"));
    }

    #[test]
    fn fresh_schema_dedups_duplicate_edge_identities() {
        let mut conn = Connection::open_in_memory().unwrap();
        ensure_schema_and_migrations(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO nodes (id, kind, name, qualified_name, file_path, language, start_line, end_line, start_column, end_column, updated_at)
             VALUES ('function:a','function','a','a','a.rs','rust',1,1,0,0,0),
                    ('function:b','function','b','b','a.rs','rust',2,2,0,0,0);",
        )
        .unwrap();

        let stmt = "INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance) VALUES ('function:a','function:b','calls',NULL,5,3,NULL)";
        conn.execute_batch(stmt).unwrap();
        conn.execute_batch(stmt).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "INSERT OR IGNORE must dedup identical edge identity"
        );
    }

    #[test]
    fn fresh_schema_dedups_null_coordinate_edge_identities() {
        let mut conn = Connection::open_in_memory().unwrap();
        ensure_schema_and_migrations(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO nodes (id, kind, name, qualified_name, file_path, language, start_line, end_line, start_column, end_column, updated_at)
             VALUES ('file:a.rs','file','a.rs','a.rs','a.rs','rust',1,1,0,0,0),
                    ('function:a','function','a','a','a.rs','rust',2,2,0,0,0);",
        )
        .unwrap();

        let stmt = "INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance) VALUES ('file:a.rs','function:a','contains',NULL,NULL,NULL,NULL)";
        conn.execute_batch(stmt).unwrap();
        conn.execute_batch(stmt).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "IFNULL folding must dedup coordinate-less (NULL line/col) edges"
        );
    }

    #[test]
    fn migration_v7_dedups_existing_edges_keeping_lowest_id() {
        let mut conn = Connection::open_in_memory().unwrap();
        seed_v6_schema_with_duplicate_edges(&conn);
        assert_eq!(get_current_version(&conn).unwrap(), 6);
        assert!(!has_index(&conn, "idx_edges_identity"));

        run_pending_migrations(&mut conn).unwrap();

        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(has_index(&conn, "idx_edges_identity"));

        assert_eq!(
            edge_ids(&conn),
            vec![1, 3, 5],
            "dedup keeps the lowest id per identity group deterministically"
        );
    }

    #[test]
    fn migration_v7_is_idempotent_on_already_unique_edges() {
        let mut conn = Connection::open_in_memory().unwrap();
        ensure_schema_and_migrations(&mut conn).unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(has_index(&conn, "idx_edges_identity"));

        ensure_schema_and_migrations(&mut conn).unwrap();
        assert_eq!(get_current_version(&conn).unwrap(), CURRENT_SCHEMA_VERSION);
    }

    // Builds a v6-shaped DB (edges table with no UNIQUE identity index) holding
    // three identity groups, each with a duplicate at a HIGHER id, so the v7
    // dedup's "keep MIN(id)" behaviour is observable. One group has NULL line/col
    // to exercise the IFNULL folding in the GROUP BY.
    fn seed_v6_schema_with_duplicate_edges(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT);
             CREATE TABLE nodes (id TEXT PRIMARY KEY, name TEXT);
             CREATE TABLE edges (id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL, target TEXT NOT NULL, kind TEXT NOT NULL, metadata TEXT, line INTEGER, col INTEGER, provenance TEXT DEFAULT NULL);
             CREATE TABLE files (path TEXT PRIMARY KEY, content_hash TEXT NOT NULL, language TEXT NOT NULL, size INTEGER NOT NULL, modified_at INTEGER NOT NULL, indexed_at INTEGER NOT NULL, node_count INTEGER DEFAULT 0, errors TEXT);
             INSERT INTO schema_versions (version, applied_at, description) VALUES (6, 0, 'v6');
             INSERT INTO edges (id, source, target, kind, line, col) VALUES
               (1,'a','b','calls',5,3),
               (2,'a','b','calls',5,3),
               (3,'a','b','references',5,3),
               (4,'a','b','references',5,3),
               (5,'f','g','contains',NULL,NULL),
               (6,'f','g','contains',NULL,NULL);",
        )
        .unwrap();
    }
}
