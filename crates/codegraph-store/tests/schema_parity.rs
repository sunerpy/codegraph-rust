use std::path::{Path, PathBuf};

use codegraph_store::Store;

const GOLDEN_SCHEMA: &str = include_str!("../../../reference/golden/colby.schema.sql");

#[test]
fn fresh_database_schema_matches_upstream_golden() {
    let tempdir = TestDir::new();
    let db_path = tempdir.path().join("codegraph.db");
    let store = Store::open(&db_path).unwrap();
    drop(store);

    let actual = sqlite_schema(&db_path);
    assert_eq!(normalize_schema(GOLDEN_SCHEMA), normalize_schema(&actual));
}

#[test]
fn reopening_database_does_not_rerun_migrations() {
    let tempdir = TestDir::new();
    let db_path = tempdir.path().join("codegraph.db");

    let first = Store::open(&db_path).unwrap();
    let first_count = schema_version_count(first.connection());
    let first_max = first.schema_version().unwrap();
    drop(first);

    let second = Store::open(&db_path).unwrap();
    let second_count = schema_version_count(second.connection());
    let second_max = second.schema_version().unwrap();

    assert_eq!(first_count, 2);
    assert_eq!(first_count, second_count);
    assert_eq!(first_max, 6);
    assert_eq!(first_max, second_max);
}

#[test]
fn old_v5_database_migrates_to_v6_without_data_loss() {
    // Given a hand-built v5 database (no reference_subkind column) holding a node
    // and an unresolved_ref row,
    let tempdir = TestDir::new();
    let db_path = tempdir.path().join("codegraph.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, description TEXT);
             CREATE TABLE nodes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT NOT NULL, file_path TEXT NOT NULL, language TEXT NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, start_column INTEGER NOT NULL, end_column INTEGER NOT NULL, docstring TEXT, signature TEXT, visibility TEXT, is_exported INTEGER DEFAULT 0, is_async INTEGER DEFAULT 0, is_static INTEGER DEFAULT 0, is_abstract INTEGER DEFAULT 0, decorators TEXT, type_parameters TEXT, return_type TEXT, updated_at INTEGER NOT NULL);
             CREATE TABLE unresolved_refs (id INTEGER PRIMARY KEY AUTOINCREMENT, from_node_id TEXT NOT NULL, reference_name TEXT NOT NULL, reference_kind TEXT NOT NULL, line INTEGER NOT NULL, col INTEGER NOT NULL, candidates TEXT, file_path TEXT NOT NULL DEFAULT '', language TEXT NOT NULL DEFAULT 'unknown', FOREIGN KEY (from_node_id) REFERENCES nodes(id) ON DELETE CASCADE);
             INSERT INTO schema_versions (version, applied_at, description) VALUES (1, 0, 'Initial schema'), (5, 0, 'Initial schema includes all migrations');
             INSERT INTO nodes (id, kind, name, qualified_name, file_path, language, start_line, end_line, start_column, end_column, updated_at) VALUES ('file:a.gd', 'file', 'a.gd', 'a.gd', 'a.gd', 'gdscript', 1, 1, 0, 0, 0);
             INSERT INTO unresolved_refs (from_node_id, reference_name, reference_kind, line, col, file_path, language) VALUES ('file:a.gd', 'player.gd', 'references', 3, 0, 'a.gd', 'godot_scene');",
        )
        .unwrap();
    }

    // When the new binary opens it,
    let store = Store::open(&db_path).unwrap();

    // Then it migrates to v6, gains the reference_subkind column (NULL for the
    // pre-existing row), and the row's data is preserved.
    assert_eq!(store.schema_version().unwrap(), 6);
    let (name, subkind): (String, Option<String>) = store
        .connection()
        .query_row(
            "SELECT reference_name, reference_subkind FROM unresolved_refs WHERE from_node_id = 'file:a.gd'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        name, "player.gd",
        "pre-existing ref row must survive migration"
    );
    assert_eq!(subkind, None, "migrated row's new column defaults to NULL");
}

// Replicates `sqlite3 .schema` in-process (no sqlite3 CLI on the Windows runner):
// dump sqlite_master.sql in rowid order, and reproduce the `/* name(cols) */`
// comment the shell appends after each CREATE VIRTUAL TABLE so the dump is
// byte-identical to the committed golden schema across platforms.
fn sqlite_schema(db_path: &Path) -> String {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name, type, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY rowid")
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut raw = String::new();
    for (name, kind, sql) in rows {
        raw.push_str(&sql);
        if kind == "table" && sql.starts_with("CREATE VIRTUAL TABLE") {
            let mut col_stmt = conn
                .prepare("SELECT name FROM pragma_table_info(?1)")
                .unwrap();
            let cols = col_stmt
                .query_map([&name], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join(",");
            raw.push_str(&format!("\n/* {name}({cols}) */"));
        }
        raw.push_str(";\n");
    }
    raw
}

fn schema_version_count(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM schema_versions", [], |row| row.get(0))
        .unwrap()
}

fn normalize_schema(schema: &str) -> String {
    schema
        .split(';')
        .map(normalize_statement)
        .filter(|statement| !statement.is_empty())
        .collect::<Vec<_>>()
        .join(";\n")
        + ";\n"
}

fn normalize_statement(statement: &str) -> String {
    statement
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .replace("CREATE TABLE IF NOT EXISTS ", "CREATE TABLE ")
        .replace("CREATE INDEX IF NOT EXISTS ", "CREATE INDEX ")
        .replace(
            "CREATE VIRTUAL TABLE IF NOT EXISTS ",
            "CREATE VIRTUAL TABLE ",
        )
        .replace("CREATE TRIGGER IF NOT EXISTS ", "CREATE TRIGGER ")
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let name = format!(
            "codegraph-store-schema-parity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
