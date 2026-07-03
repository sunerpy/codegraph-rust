use rusqlite::{Connection, OptionalExtension, params};

use crate::schema::BASE_SCHEMA;

pub const CURRENT_SCHEMA_VERSION: i64 = 6;
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
    },
    Migration {
        version: 3,
        description: "Add lower(name) expression index for memory-efficient case-insensitive lookups",
        sql: r#"
        CREATE INDEX IF NOT EXISTS idx_nodes_lower_name ON nodes(lower(name));
      "#,
    },
    Migration {
        version: 4,
        description: "Drop redundant idx_edges_source / idx_edges_target (covered by source_kind / target_kind composites)",
        sql: r#"
        DROP INDEX IF EXISTS idx_edges_source;
        DROP INDEX IF EXISTS idx_edges_target;
      "#,
    },
    Migration {
        version: 5,
        description: "Add nodes.return_type — normalized return/result type for receiver-type inference (C++ singletons/factories, #645)",
        sql: r#"
        ALTER TABLE nodes ADD COLUMN return_type TEXT;
      "#,
    },
    Migration {
        version: 6,
        description: "Add unresolved_refs.reference_subkind — structural extraction label (Godot edge subkind)",
        sql: r#"
        ALTER TABLE unresolved_refs ADD COLUMN reference_subkind TEXT;
      "#,
    },
];

#[derive(Debug, Clone, Copy)]
struct Migration {
    version: i64,
    description: &'static str,
    sql: &'static str,
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
        tx.execute_batch(migration.sql)?;
        record_migration(&tx, migration.version, migration.description)?;
        tx.commit()?;
    }

    Ok(())
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
}
