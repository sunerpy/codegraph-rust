use std::{
    path::{Path, PathBuf},
    process::Command,
};

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
    assert_eq!(first_max, 5);
    assert_eq!(first_max, second_max);
}

fn sqlite_schema(db_path: &Path) -> String {
    let output = Command::new("sqlite3")
        .arg(db_path)
        .arg(".schema")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "sqlite3 .schema failed: {output:?}"
    );
    String::from_utf8(output.stdout).unwrap()
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
