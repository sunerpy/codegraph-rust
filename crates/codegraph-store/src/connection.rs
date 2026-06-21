use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::migrations;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to create database directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open SQLite database {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to configure SQLite pragmas for {path}: {source}")]
    Configure {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("failed to initialize or migrate SQLite schema for {path}: {source}")]
    Migrate {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    pub(crate) conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut conn = Connection::open(db_path).map_err(|source| StoreError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;

        migrations::configure_auto_vacuum_for_fresh_db(&conn).map_err(|source| {
            StoreError::Configure {
                path: db_path.to_path_buf(),
                source,
            }
        })?;

        configure_connection(&conn).map_err(|source| StoreError::Configure {
            path: db_path.to_path_buf(),
            source,
        })?;

        migrations::ensure_schema_and_migrations(&mut conn).map_err(|source| {
            StoreError::Migrate {
                path: db_path.to_path_buf(),
                source,
            }
        })?;

        Ok(Self {
            conn,
            path: db_path.to_path_buf(),
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> rusqlite::Result<i64> {
        migrations::get_current_version(&self.conn)
    }
}

fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    // Order mirrors the upstream configureConnection exactly. busy_timeout must be
    // first so later file-touching pragmas wait instead of immediately failing.
    conn.busy_timeout(std::time::Duration::from_millis(5_000))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", -64_000)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 268_435_456_i64)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pragmas_match_upstream_connection_settings() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();

        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            -64_000
        );
        assert_eq!(
            conn.query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
