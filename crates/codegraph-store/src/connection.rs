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

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codegraph-conn-{label}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

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

    #[test]
    fn open_creates_parent_dir_migrates_and_exposes_accessors() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-open-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_path = base.join("nested").join("graph.db");
        let store = Store::open(&db_path).expect("open creates nested dirs and migrates");

        assert!(db_path.exists(), "db file created");
        assert_eq!(store.path(), db_path.as_path());
        assert_eq!(
            store.schema_version().unwrap(),
            crate::migrations::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            store
                .connection()
                .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap()
                .to_lowercase(),
            "wal"
        );

        drop(store);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reopening_existing_db_keeps_schema_version() {
        let db_path = temp_db_path("reopen");
        let v1 = Store::open(&db_path).unwrap().schema_version().unwrap();
        let v2 = Store::open(&db_path).unwrap().schema_version().unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1, crate::migrations::CURRENT_SCHEMA_VERSION);

        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", db_path.display()));
        }
    }

    #[test]
    #[cfg(unix)]
    fn open_on_unwritable_path_surfaces_open_error() {
        let bogus = Path::new("/proc/definitely-not-writable/graph.db");
        match Store::open(bogus) {
            Ok(_) => panic!("open must fail on an unwritable location"),
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    matches!(err, StoreError::CreateDir { .. } | StoreError::Open { .. }),
                    "unexpected error variant: {msg}"
                );
            }
        }
    }

    #[test]
    fn open_on_a_non_sqlite_file_surfaces_migrate_error() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-garbage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let db_path = base.join("graph.db");
        std::fs::write(&db_path, b"this is not a sqlite database at all").unwrap();

        let Err(err) = Store::open(&db_path) else {
            panic!("a non-sqlite file must fail to open+migrate");
        };
        assert!(
            matches!(
                err,
                StoreError::Configure { .. } | StoreError::Migrate { .. } | StoreError::Open { .. }
            ),
            "a corrupt db must surface a Configure/Migrate/Open error, got: {err}"
        );
        assert!(err.to_string().contains(&db_path.display().to_string()));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_when_db_path_is_a_directory_surfaces_open_error() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-dbdir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db_path = base.join("graph.db");
        std::fs::create_dir_all(&db_path).unwrap();

        let Err(err) = Store::open(&db_path) else {
            panic!("opening a directory as a db file must fail");
        };
        assert!(
            matches!(err, StoreError::Open { .. } | StoreError::Configure { .. }),
            "a directory db path must surface an Open/Configure error, got: {err}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn open_when_parent_is_a_file_surfaces_create_dir_error() {
        let base = std::env::temp_dir().join(format!(
            "codegraph-conn-fileparent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let db_path = blocker.join("nested").join("graph.db");

        let Err(err) = Store::open(&db_path) else {
            panic!("creating a dir under a regular file must fail at CreateDir");
        };
        assert!(
            matches!(err, StoreError::CreateDir { .. }),
            "unexpected error variant: {err}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn store_error_messages_name_the_path_for_each_variant() {
        let p = PathBuf::from("/tmp/x.db");
        let io_err = || std::io::Error::other("boom");
        let sql_err = || rusqlite::Error::InvalidQuery;

        let create = StoreError::CreateDir {
            path: p.clone(),
            source: io_err(),
        };
        assert!(create.to_string().contains("/tmp/x.db"));
        let open = StoreError::Open {
            path: p.clone(),
            source: sql_err(),
        };
        assert!(open.to_string().contains("/tmp/x.db"));
        let configure = StoreError::Configure {
            path: p.clone(),
            source: sql_err(),
        };
        assert!(configure.to_string().contains("/tmp/x.db"));
        let migrate = StoreError::Migrate {
            path: p,
            source: sql_err(),
        };
        assert!(migrate.to_string().contains("/tmp/x.db"));
    }
}
