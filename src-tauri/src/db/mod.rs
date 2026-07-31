//! Database initialization, connection pool, and schema migrations for Link-Searcher.

pub mod dir_config;
pub mod search_history;
pub mod tracker;

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::{rusqlite::Connection, SqliteConnectionManager};

/// Current schema version. Bump when adding migrations.
const SCHEMA_VERSION: &str = "1";

/// Create a connection pool with WAL mode and foreign keys enabled.
pub fn get_pool(db_path: &str) -> Result<Pool<SqliteConnectionManager>> {
    let manager = SqliteConnectionManager::file(db_path);
    let pool = Pool::builder()
        .max_size(8)
        .build(manager)
        .context("failed to create connection pool")?;

    // Enable WAL mode and foreign keys on the initial connection;
    // all pooled connections inherit these pragmas from the same file.
    let conn = pool.get().context("failed to get initial connection")?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .context("failed to set database pragmas")?;

    Ok(pool)
}

/// Initialize the database: create tables, seed defaults, record schema version.
///
/// Safe to call multiple times — all DDL uses IF NOT EXISTS.
pub fn init_db(db_path: &str) -> Result<()> {
    let pool = get_pool(db_path)?;
    let conn = pool.get().context("failed to get connection for init")?;
    run_migrations(&conn)
}

/// Run all pending migrations. Exposed as `pub(crate)` for testing.
pub(crate) fn run_migrations(conn: &Connection) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("failed to start migration transaction")?;

    tx.execute_batch(CREATE_TABLES_SQL)
        .context("failed to create tables")?;

    seed_default_settings(&tx)?;

    tx.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION],
    )
    .context("failed to set schema version")?;

    tx.commit().context("failed to commit migration")?;
    Ok(())
}

const CREATE_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS file_tracking (
        id          TEXT PRIMARY KEY,
        path        TEXT NOT NULL UNIQUE,
        dir_id      TEXT NOT NULL,
        mtime       INTEGER NOT NULL,
        size        INTEGER NOT NULL DEFAULT 0,
        md5         TEXT,
        status      TEXT NOT NULL DEFAULT 'active',
        indexed     INTEGER NOT NULL DEFAULT 0,
        error_msg   TEXT,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_ft_dir_id ON file_tracking(dir_id);
    CREATE INDEX IF NOT EXISTS idx_ft_status ON file_tracking(status);
    CREATE INDEX IF NOT EXISTS idx_ft_md5 ON file_tracking(md5);
    CREATE INDEX IF NOT EXISTS idx_ft_mtime ON file_tracking(mtime);

    CREATE TABLE IF NOT EXISTS content_index (
        md5             TEXT PRIMARY KEY,
        text_content    TEXT NOT NULL,
        indexed_at      INTEGER NOT NULL,
        char_count      INTEGER NOT NULL DEFAULT 0,
        ocr_used        INTEGER NOT NULL DEFAULT 0,
        ocr_duration_ms INTEGER
    );

    CREATE TABLE IF NOT EXISTS dir_config (
        id              TEXT PRIMARY KEY,
        path            TEXT NOT NULL UNIQUE,
        alias           TEXT,
        ocr_lang        TEXT NOT NULL DEFAULT 'eng',
        exclude_patterns TEXT,
        include_exts    TEXT,
        recursive       INTEGER NOT NULL DEFAULT 1,
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS search_history (
        id           TEXT PRIMARY KEY,
        query        TEXT NOT NULL,
        dir_ids      TEXT,
        filters      TEXT,
        result_count INTEGER NOT NULL DEFAULT 0,
        pinned       INTEGER NOT NULL DEFAULT 0,
        created_at   INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS app_settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS index_errors (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        file_id     TEXT NOT NULL,
        file_path   TEXT NOT NULL,
        error_type  TEXT NOT NULL,
        error_msg   TEXT NOT NULL,
        created_at  INTEGER NOT NULL
    );
";

/// Remove content_index rows whose md5 is no longer referenced by any
/// active file_tracking row.
pub fn cleanup_orphan_content(conn: &Connection) -> Result<u64> {
    let deleted = conn.execute(
        "DELETE FROM content_index WHERE md5 NOT IN (SELECT DISTINCT md5 FROM file_tracking WHERE md5 IS NOT NULL)",
        [],
    )?;
    if deleted > 0 {
        log::info!("[DB] cleaned up {deleted} orphan content_index rows");
    }
    Ok(deleted as u64)
}

/// Run VACUUM to reclaim space and defragment the database file.
/// This is a no-op on an in-memory database.
pub fn vacuum(conn: &Connection) -> Result<()> {
    conn.execute_batch("VACUUM;")?;
    log::info!("[DB] VACUUM completed");
    Ok(())
}

fn seed_default_settings(conn: &Connection) -> Result<()> {
    let defaults = [
        ("ocr_lang", "eng"),
        ("ocr_concurrent", "2"),
        ("scheduled_scan_time", "02:00"),
        ("max_results", "1000"),
        ("auto_backup_enabled", "1"),
        ("auto_backup_interval_days", "7"),
        ("theme", "system"),
    ];
    for (key, value) in defaults {
        conn.execute(
            "INSERT OR IGNORE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .context(format!("failed to seed setting '{key}'"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn test_migrations_create_all_tables() {
        let conn = setup_conn();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for table in &["app_settings", "content_index", "dir_config", "file_tracking", "index_errors", "search_history"] {
            assert!(tables.contains(&table.to_string()), "missing table: {table}");
        }
    }

    #[test]
    fn test_migrations_are_idempotent() {
        let conn = setup_conn();
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn test_default_settings_seeded() {
        let conn = setup_conn();
        let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key='schema_version'").unwrap();
        let version: String = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn test_init_db_with_file() {
        let tmp = std::env::temp_dir().join(format!("ls_init_test_{}.db", std::process::id()));
        let path = tmp.to_str().unwrap().to_string();

        init_db(&path).unwrap();

        let pool = get_pool(&path).unwrap();
        let conn = pool.get().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }
}
