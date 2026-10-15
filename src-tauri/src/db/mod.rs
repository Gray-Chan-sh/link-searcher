//! Database initialization, connection pool, and schema migrations for Link-Searcher.

pub mod ai_events;
pub mod chunks;
pub mod dir_config;
pub mod search_history;
pub mod tracker;

use anyhow::{Context, Result};
use r2d2::{CustomizeConnection, Pool};
use r2d2_sqlite::{rusqlite::Connection, SqliteConnectionManager};

/// Current schema version. Bump when adding migrations.
const SCHEMA_VERSION: &str = "9";

/// Connection customizer that enables WAL mode and foreign keys on every
/// pooled connection.  r2d2 calls this right after a new connection is created,
/// so all connections start with the correct pragmas.
#[derive(Debug)]
struct PragmaCustomizer;

impl CustomizeConnection<Connection, rusqlite::Error> for PragmaCustomizer {
    fn on_acquire(&self, conn: &mut Connection) -> Result<(), rusqlite::Error> {
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;\
             PRAGMA journal_mode=WAL;\
             PRAGMA synchronous=NORMAL;\
             PRAGMA cache_size=-8000;\
             PRAGMA temp_store=MEMORY;\
             PRAGMA wal_autocheckpoint=1000;\
             PRAGMA journal_size_limit=67108864;\
             PRAGMA busy_timeout=15000;",
        )
    }
}

/// Create a connection pool with WAL mode and foreign keys enabled on every
/// connection via the customizer.
pub fn get_pool(db_path: &str) -> Result<Pool<SqliteConnectionManager>> {
    // Checkpoint WAL before creating pool — prevents startup failure
    // when WAL is large from previous unclean shutdown (pkill/panic).
    if let Ok(conn) = Connection::open(db_path) {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    let manager = SqliteConnectionManager::file(db_path);
    let pool = Pool::builder()
        // 容量按「**同一时刻最多有多少线程想借连接**」推导，**不是**按硬件参数：
        //   · Phase-1 提取 worker（`perf_batch_io_concurrency`，**上限 24** —— 见
        //     `commands/performance.rs::scale_params`：cores × 档位系数 × 磁盘系数，
        //     aggressive 档 ceil=24）——每个文件只在「提取前读 / 提取后写」两个**毫秒级
        //     窗口**各借一次；
        //   · embedding keeper 2 条（doc / chunk）；
        //   · Web API / 扫描 / 文件监听 / 采样 / verify 等若干条。
        // → 24 + 2 + 若干 ≈ 30；48 ≈ 1.6×，给"多个 worker 同时落在短窗口"的突发留余量。
        //
        // ⚠️ **历史教训（不要再走一遍）**：2026-09-26 与 2026-10-14 两次「连接池不够」
        // 的真因**都不是池小**，而是 `indexer::extract_and_index_single` **在整个提取期
        // （含数分钟 OCR）持有连接**。两次都只把池 12→24→48、等待 10s→30s→60s，把症状
        // 吸收掉、根因留了三个月，最终爆成 108 个文件被误判失败。根因已于 2026-10-15
        // 修掉（提取三阶段各借各还，**提取期零连接占用**）。
        // → 见到 `DB conn: timed out` 请**先查连接是否被长期持有**，不要把"扩池"当修复。
        //
        // 等待上限保持宽松（60s）是**安全阀**：修好之后健康的取连接是亚毫秒级，60s 只用于
        // 「极偶发突发时让线程等，而不是失败」—— 因池等待而判文件失败是不可接受的。
        .max_size(48)
        .connection_timeout(std::time::Duration::from_secs(60))
        .connection_customizer(Box::new(PragmaCustomizer))
        .build(manager)
        .context("failed to create connection pool")?;

    Ok(pool)
}

/// Initialize the database: create tables, seed defaults, record schema version.
///
/// Safe to call multiple times — all DDL uses IF NOT EXISTS.
///
/// Callers must ensure the connection's pragmas (WAL, foreign keys) are set,
/// typically via [`get_pool`] with [`PragmaCustomizer`].
pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    run_migrations(conn)?;
    let _ = conn.execute_batch("PRAGMA optimize;");
    Ok(())
}

/// Run all pending migrations. Exposed as `pub(crate)` for testing.
pub(crate) fn run_migrations(conn: &Connection) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("failed to start migration transaction")?;

    tx.execute_batch(CREATE_TABLES_SQL)
        .context("failed to create tables")?;

    // Migration to schema v2: add `file_ext` (list_files_db ORDER BY + get_file_type_stats GROUP BY previously referenced a non-existent column).
    ensure_file_ext_column(&tx)?;

    // Schema: add `dead_content` (content-validity marker for empty-content
    // files that already failed verification + retry).
    ensure_dead_content_column(&tx)?;

    // Quality persistence: quality_score, quality_flags, reextract_count.
    ensure_quality_columns(&tx)?;

    // Retry cap for failed extractions: `fail_count` stops a permanently
    // corrupt/encrypted file from being re-extracted on every scan.
    ensure_fail_count_column(&tx)?;

    // P6 removal: migrate existing databases that still carry the legacy
    // `private` column on dir_config.
    drop_dir_config_private_column(&tx)?;

    // Retired feature: the offline QA-pair retrieval channel was removed.
    // Drop its table to reclaim space (idempotent — no-op on fresh DBs).
    tx.execute_batch("DROP TABLE IF EXISTS doc_qa_pairs;")
        .context("failed to drop retired doc_qa_pairs table")?;

    // Watermark decisions must never leak between documents. Older databases
    // may still contain the retired cross-document fragment dictionary.
    tx.execute_batch("DROP TABLE IF EXISTS watermark_tokens;")
        .context("failed to drop retired watermark_tokens table")?;

    // One-time normalization of dir_config.path rows stored before
    // normalize-on-write (Windows backslashes / \\?\ verbatim prefixes);
    // must run BEFORE migrate_paths_to_relative derives prefixes from roots.
    let dirs_normed = tracker::migrate_dir_paths_to_forward_slash(&tx).unwrap_or(0);
    if dirs_normed > 0 {
        log::info!("[DB] dir_config paths normalized: {}", dirs_normed);
    }

    // Migrate any absolute paths stored in file_tracking.path to relative
    // form (relative to each dir_config root), fixing a historic bug where
    // migration was never actually invoked at startup.
    let migrated = tracker::migrate_paths_to_relative(&tx).unwrap_or(0);
    if migrated > 0 {
        log::info!("[DB] path migration: {} records converted from absolute to relative", migrated);
    }

    seed_default_settings(&tx)?;

    // Settings key rename: `auto_backup_enabled`/`auto_backup_interval_days`
    // were seeded but never read (UI + whitelist use `auto_backup`/`backup_interval`).
    migrate_legacy_backup_settings(&tx)?;

    tx.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('schema_version', ?1)",
        [SCHEMA_VERSION],
    )
    .context("failed to set schema version")?;

    tx.commit().context("failed to commit migration")?;
    Ok(())
}

/// Add the `file_ext` column to an existing v1 database and backfill it from
/// `path`. Idempotent — safe to call on every startup.
fn ensure_file_ext_column(tx: &Connection) -> Result<()> {
    let has_column: bool = {
        let mut stmt = tx
            .prepare("PRAGMA table_info(file_tracking)")
            .context("failed to inspect file_tracking")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to read columns")?;
        names
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to collect columns")?
            .iter()
            .any(|n| n == "file_ext")
    };
    if has_column {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE file_tracking ADD COLUMN file_ext TEXT")
        .context("failed to add file_ext column")?;
    crate::db::tracker::backfill_file_ext(tx)
        .context("failed to backfill file_ext")?;
    Ok(())
}

/// Add the `dead_content` column to older databases. Idempotent — the
/// verification command relies on it to stop re-testing files whose content
/// was already confirmed empty by a previous attempt.
fn ensure_dead_content_column(tx: &Connection) -> Result<()> {
    let has_column: bool = {
        let mut stmt = tx
            .prepare("PRAGMA table_info(file_tracking)")
            .context("failed to inspect file_tracking")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to read columns")?;
        names
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to collect columns")?
            .iter()
            .any(|n| n == "dead_content")
    };
    if has_column {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE file_tracking ADD COLUMN dead_content INTEGER NOT NULL DEFAULT 0")
        .context("failed to add dead_content column")?;
    Ok(())
}

/// Drop the legacy P6 `private` column from dir_config. Idempotent — safe to
/// call on every startup; no-op when the column was never created.
fn drop_dir_config_private_column(tx: &Connection) -> Result<()> {
    let has_column: bool = {
        let mut stmt = tx
            .prepare("PRAGMA table_info(dir_config)")
            .context("failed to inspect dir_config")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to read columns")?;
        names
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to collect columns")?
            .iter()
            .any(|n| n == "private")
    };
    if !has_column {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE dir_config DROP COLUMN private")
        .context("failed to drop private column")?;
    log::info!("[DB] dropped legacy dir_config.private column (P6 removed)");
    Ok(())
}

/// Add the `fail_count` column used to cap automatic retries of failed
/// extractions. Idempotent — safe to call on every startup.
fn ensure_fail_count_column(tx: &Connection) -> Result<()> {
    let has_column: bool = {
        let mut stmt = tx
            .prepare("PRAGMA table_info(file_tracking)")
            .context("failed to inspect file_tracking")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to read columns")?;
        names
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to collect columns")?
            .iter()
            .any(|n| n == "fail_count")
    };
    if has_column {
        return Ok(());
    }
    tx.execute_batch("ALTER TABLE file_tracking ADD COLUMN fail_count INTEGER NOT NULL DEFAULT 0")
        .context("failed to add fail_count column")?;
    Ok(())
}

fn ensure_quality_columns(tx: &Connection) -> Result<()> {
    let columns: Vec<String> = {
        let mut stmt = tx
            .prepare("PRAGMA table_info(content_index)")
            .context("failed to inspect content_index")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to read columns")?;
        names
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to collect columns")?
    };
    let has = |name: &str| columns.iter().any(|n| n == name);
    if !has("quality_score") {
        tx.execute_batch("ALTER TABLE content_index ADD COLUMN quality_score REAL")
            .context("failed to add quality_score")?;
    }
    if !has("quality_flags") {
        tx.execute_batch("ALTER TABLE content_index ADD COLUMN quality_flags TEXT NOT NULL DEFAULT '[]'")
            .context("failed to add quality_flags")?;
    }
    if !has("reextract_count") {
        tx.execute_batch("ALTER TABLE content_index ADD COLUMN reextract_count INTEGER NOT NULL DEFAULT 0")
            .context("failed to add reextract_count")?;
    }
    if !has("mean_confidence") {
        tx.execute_batch("ALTER TABLE content_index ADD COLUMN mean_confidence REAL")
            .context("failed to add mean_confidence")?;
    }
    Ok(())
}

const CREATE_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS file_tracking (
        id          TEXT PRIMARY KEY,
        path        TEXT NOT NULL UNIQUE,
        file_ext    TEXT,
        dir_id      TEXT NOT NULL,
        mtime       INTEGER NOT NULL,
        size        INTEGER NOT NULL DEFAULT 0,
        md5         TEXT,
        status      TEXT NOT NULL DEFAULT 'active',
        indexed     INTEGER NOT NULL DEFAULT 0,
        error_msg   TEXT,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        dead_content INTEGER NOT NULL DEFAULT 0,
        fail_count  INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS idx_ft_dir_id ON file_tracking(dir_id);
    CREATE INDEX IF NOT EXISTS idx_ft_status ON file_tracking(status);
    CREATE INDEX IF NOT EXISTS idx_ft_md5 ON file_tracking(md5);
    CREATE INDEX IF NOT EXISTS idx_ft_mtime ON file_tracking(mtime);
    CREATE INDEX IF NOT EXISTS idx_ft_pending ON file_tracking(updated_at) WHERE indexed IN (0, 2, 3);

    CREATE TABLE IF NOT EXISTS content_index (
        md5             TEXT PRIMARY KEY,
        text_content    TEXT NOT NULL,
        indexed_at      INTEGER NOT NULL,
        char_count      INTEGER NOT NULL DEFAULT 0,
        ocr_used        INTEGER NOT NULL DEFAULT 0,
        ocr_duration_ms INTEGER,
        quality_score   REAL,
        quality_flags   TEXT NOT NULL DEFAULT '[]',
        reextract_count INTEGER NOT NULL DEFAULT 0,
        mean_confidence REAL
    );
    CREATE INDEX IF NOT EXISTS idx_ci_quality ON content_index(quality_score);

    CREATE TABLE IF NOT EXISTS dir_config (
        id              TEXT PRIMARY KEY,
        path            TEXT NOT NULL UNIQUE,
        alias           TEXT,
        ocr_lang        TEXT NOT NULL DEFAULT 'chi_sim',
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

    CREATE TABLE IF NOT EXISTS hotword_counts (
        word        TEXT PRIMARY KEY,
        count       INTEGER NOT NULL DEFAULT 1
    );

    CREATE TABLE IF NOT EXISTS unsupported_ext_stats (
        ext         TEXT PRIMARY KEY,
        count       INTEGER NOT NULL DEFAULT 0,
        dir_id      TEXT NOT NULL DEFAULT '*',
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS doc_embeddings (
        file_id     TEXT PRIMARY KEY,
        dim         INTEGER NOT NULL,
        vector      BLOB NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS doc_summaries (
        file_id     TEXT PRIMARY KEY,
        summary     TEXT NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS ai_events (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id  TEXT NOT NULL,
        turn_number INTEGER NOT NULL,
        event_seq   INTEGER NOT NULL,
        event_type  TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at  INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_ae_session ON ai_events(session_id, turn_number);
    CREATE INDEX IF NOT EXISTS idx_ae_created_at ON ai_events(created_at);

    CREATE TABLE IF NOT EXISTS doc_chunks (
        md5         TEXT NOT NULL,
        chunk_index INTEGER NOT NULL,
        start_char  INTEGER NOT NULL,
        end_char    INTEGER NOT NULL,
        text        TEXT NOT NULL,
        PRIMARY KEY (md5, chunk_index)
    );

    CREATE TABLE IF NOT EXISTS chunk_embeddings (
        md5         TEXT NOT NULL,
        chunk_index INTEGER NOT NULL,
        dim         INTEGER NOT NULL,
        vector      BLOB NOT NULL,
        updated_at  INTEGER NOT NULL,
        PRIMARY KEY (md5, chunk_index)
    );

    -- 飞书机器人：p2p 会话 ↔ 本库聊天会话 的映射（docs/LARK_BOT.md §5.5 阶段三）。
    -- 存 DB 而非独立文件：事务性 + 跨进程安全，不必再造一套锁与临时文件。
    CREATE TABLE IF NOT EXISTS lark_sessions (
        chat_id      TEXT PRIMARY KEY,
        session_id   TEXT NOT NULL,
        user_open_id TEXT NOT NULL DEFAULT '',
        created_at   INTEGER NOT NULL,
        updated_at   INTEGER NOT NULL
    );

    -- 飞书事件去重环：按 message_id 记住已处理的消息（§8 幂等）。
    -- 官方明言 event_id **不可**用于去重（重投会变），必须用 message_id。
    CREATE TABLE IF NOT EXISTS lark_seen_messages (
        message_id TEXT PRIMARY KEY,
        seen_at    INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_lsm_seen_at ON lark_seen_messages(seen_at);

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
    let deleted_chunks = conn.execute(
        "DELETE FROM doc_chunks WHERE md5 NOT IN (SELECT DISTINCT md5 FROM file_tracking WHERE md5 IS NOT NULL)",
        [],
    )?;
    if deleted_chunks > 0 {
        log::info!("[DB] cleaned up {deleted_chunks} orphan doc_chunks rows");
    }
    Ok(deleted as u64)
}

/// Remove `chunk_embeddings` rows whose md5 is no longer referenced by any
/// `file_tracking` row — the chunk-vector counterpart of
/// [`cleanup_orphan_content`].
///
/// Content is shared by hash across directories, so this only deletes vectors
/// nobody owns any more; a chunk vector survives as long as **any** tracked file
/// still hashes to its md5. Both directory-removal paths call this so a re-added
/// directory starts from a clean slate without disturbing shared content.
pub fn cleanup_orphan_chunk_embeddings(conn: &Connection) -> Result<u64> {
    let deleted = conn.execute(
        "DELETE FROM chunk_embeddings WHERE md5 NOT IN (SELECT DISTINCT md5 FROM file_tracking WHERE md5 IS NOT NULL)",
        [],
    )?;
    if deleted > 0 {
        log::info!("[DB] cleaned up {deleted} orphan chunk_embeddings rows");
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

/// Fold the WAL back into the main database file and truncate it.
///
/// Used after a directory purge **instead of** [`vacuum`]: VACUUM rewrites the
/// whole (multi-GB) database and held the write lock for ~40 s in the field,
/// which made the app look frozen. A WAL checkpoint reclaims the space that was
/// just freed much more cheaply.
///
/// Returns SQLite's `(busy, wal_pages, checkpointed_pages)`; `busy == 1` means
/// another connection blocked the truncation — harmless, it checkpoints later.
pub fn wal_checkpoint_truncate(conn: &Connection) -> Result<(i64, i64, i64)> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .context("wal_checkpoint(TRUNCATE) failed")
}

/// Remove `doc_embeddings` / `doc_summaries` rows whose `file_id` no longer
/// exists in `file_tracking`.
///
/// Both tables are keyed by `file_id`, so any path that drops a tracking row
/// without also dropping its vector/summary leaves an orphan there **forever** —
/// the md5-keyed cleanups above never look at these tables. Move detection is the
/// known source: `startup_scan` hard-deletes the freshly-walked row B and only
/// removes B's Tantivy document.
///
/// Returns the number of rows removed.
pub fn cleanup_orphan_file_rows(conn: &Connection) -> Result<u64> {
    let embeddings = conn.execute(
        "DELETE FROM doc_embeddings WHERE file_id NOT IN (SELECT id FROM file_tracking)",
        [],
    )?;
    let summaries = conn.execute(
        "DELETE FROM doc_summaries WHERE file_id NOT IN (SELECT id FROM file_tracking)",
        [],
    )?;
    let deleted = (embeddings + summaries) as u64;
    if deleted > 0 {
        log::info!("[DB] cleaned up {deleted} orphan doc_embeddings/doc_summaries rows");
    }
    Ok(deleted)
}

/// Rename the dead `auto_backup_enabled`/`auto_backup_interval_days` setting
/// keys to the names the UI and settings whitelist read. Copies the legacy
/// value only when the new key is absent, then drops the dead key.
/// Idempotent — safe on every startup.
fn migrate_legacy_backup_settings(tx: &Connection) -> Result<()> {
    for (legacy, current) in [
        ("auto_backup_enabled", "auto_backup"),
        ("auto_backup_interval_days", "backup_interval"),
    ] {
        tx.execute(
            "INSERT OR REPLACE INTO app_settings (key, value)
             SELECT ?1, value FROM app_settings
             WHERE key = ?2
               AND NOT EXISTS (SELECT 1 FROM app_settings WHERE key = ?1)",
            rusqlite::params![current, legacy],
        )
        .context(format!("failed to migrate setting '{legacy}' to '{current}'"))?;
        tx.execute("DELETE FROM app_settings WHERE key = ?1", [legacy])
            .context(format!("failed to drop legacy setting '{legacy}'"))?;
    }
    Ok(())
}

fn seed_default_settings(conn: &Connection) -> Result<()> {
    // OCR engine default is platform-native: Apple Vision on macOS, Windows
    // OCR on Windows when a usable language pack is installed (checked here
    // at seed time; a user who never opens Settings gets the working default).
    // Otherwise the built-in PaddleOCR. `preferred_engine()` in
    // `extractor/ocr.rs` still guards runtime: if the seeded engine is not
    // actually usable (e.g. no Windows OCR language pack, PaddleOCR models
    // not yet downloaded), it falls back to a usable engine.
    #[cfg(target_os = "macos")]
    let default_ocr_engine = "AppleVision";
    #[cfg(target_os = "windows")]
    let default_ocr_engine = {
        if crate::extractor::windows_ocr::availability().0 {
            "WindowsOcr"
        } else {
            "PaddleOCR"
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let default_ocr_engine = "PaddleOCR";

    let defaults = [
        ("ocr_engine", default_ocr_engine),
        ("ocr_lang", "chi_sim"),
        ("ocr_pdf_dpi", "300"),
        ("ocr_stall_timeout_secs", "120"),
        // 自适应并发（内存压力降级/升级）：阈值与各档并发数
        ("adaptive_load_enabled", "1"),
        ("load_process_soft_pct", "40"),
        ("load_process_hard_pct", "55"),
        ("load_process_pause_pct", "70"),
        ("load_system_soft_pct", "85"),
        ("load_system_hard_pct", "90"),
        ("load_system_pause_pct", "95"),
        ("load_soft_page_window", "2"),
        ("load_soft_ocr_permits", "4"),
        ("load_soft_doc_permits", "6"),
        ("load_hard_page_window", "1"),
        ("load_hard_ocr_permits", "2"),
        ("load_hard_doc_permits", "4"),
        ("load_sample_secs", "5"),
        ("load_upgrade_samples", "24"),
        ("load_recovery_margin_pct", "5"),
        ("embed_parallelism", "0"),
        ("embed_adaptive", "1"),
        ("scheduled_scan_time", "02:00"),
        ("max_results", "1000"),
        ("auto_backup", "1"),
        ("backup_interval", "7"),
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
        for table in &["ai_events", "app_settings", "content_index", "dir_config", "file_tracking", "index_errors", "search_history"] {
            assert!(tables.contains(&table.to_string()), "missing table: {table}");
        }
    }

    #[test]
    fn test_migrations_are_idempotent() {
        let conn = setup_conn();
        run_migrations(&conn).unwrap();
    }

    /// 按 `file_id` 键的表（doc 向量/摘要）没有任何 md5 清理会覆盖：只要 tracking 行
    /// 没了，它们就是永久孤儿（移动检测 `hard_delete_file` 曾是源头）。
    #[test]
    fn orphan_file_rows_are_cleaned() {
        let conn = setup_conn();
        conn.execute(
            "INSERT INTO file_tracking (id,path,dir_id,mtime,size,md5,status,indexed,created_at,updated_at) \
             VALUES ('keep','k','d1',0,0,'m1','active',1,0,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO doc_embeddings (file_id,dim,vector,updated_at) VALUES ('keep',1,X'00000000',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO doc_summaries (file_id,summary,updated_at) VALUES ('keep','s',0)",
            [],
        )
        .unwrap();
        // 没有对应 tracking 行的残留
        conn.execute(
            "INSERT INTO doc_embeddings (file_id,dim,vector,updated_at) VALUES ('ghost',1,X'00000000',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO doc_summaries (file_id,summary,updated_at) VALUES ('ghost2','s',0)",
            [],
        )
        .unwrap();

        let deleted = cleanup_orphan_file_rows(&conn).unwrap();
        assert_eq!(deleted, 2, "两条孤儿（一个向量 + 一个摘要）都应清掉");
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1, "仍被引用的行必须保留");
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM doc_summaries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1);
    }

    /// 按 md5 键的块向量必须"共享内容感知"：删掉一个目录后，只要还有别的
    /// file_tracking 行引用同一 md5，它的 `chunk_embeddings` 就不能被清掉。
    #[test]
    fn orphan_chunk_embeddings_keep_shared_content() {
        let conn = setup_conn();
        conn.execute(
            "INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d1','/a',1,0,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d2','/b',1,0,0)",
            [],
        )
        .unwrap();
        // f1/f3 共享同一内容，f2 是 d1 独占。
        for (id, dir, md5) in [("f1", "d1", "shared"), ("f2", "d1", "only1"), ("f3", "d2", "shared")] {
            conn.execute(
                "INSERT INTO file_tracking (id,path,dir_id,mtime,size,md5,status,indexed,created_at,updated_at) \
                 VALUES (?1,?2,?3,0,0,?4,'active',1,0,0)",
                rusqlite::params![id, id, dir, md5],
            )
            .unwrap();
        }
        // 块向量按 (md5, chunk_index) 唯一：每个内容只插一条。
        for md5 in ["shared", "only1"] {
            conn.execute(
                "INSERT INTO chunk_embeddings (md5,chunk_index,dim,vector,updated_at) VALUES (?1,0,1,X'00000000',0)",
                rusqlite::params![md5],
            )
            .unwrap();
        }

        // 删除 d1：只有 `only1` 变成孤儿，`shared` 仍被 d2 引用。
        conn.execute("DELETE FROM file_tracking WHERE dir_id='d1'", []).unwrap();
        let deleted = cleanup_orphan_chunk_embeddings(&conn).unwrap();
        assert_eq!(deleted, 1, "只应删除 d1 独占内容的块向量");

        let left: Vec<String> = conn
            .prepare("SELECT md5 FROM chunk_embeddings ORDER BY md5")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(left, vec!["shared".to_string()], "共享内容的块向量必须保留");
    }

    #[test]
    fn test_legacy_private_column_dropped() {
        // Simulate a pre-P6-removal database: dir_config still carries `private`.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE dir_config (
                id       TEXT PRIMARY KEY,
                path     TEXT NOT NULL UNIQUE,
                private  INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        run_migrations(&conn).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(dir_config)").unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!names.contains(&"private".to_string()), "private still present: {names:?}");
    }

    #[test]
    fn test_legacy_backup_settings_migrated() {
        // Simulate a database seeded before the auto_backup key rename.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO app_settings (key, value)
             VALUES ('auto_backup_enabled', '1'), ('auto_backup_interval_days', '7');",
        )
        .unwrap();
        run_migrations(&conn).unwrap();

        let get = |conn: &Connection, key: &str| -> String {
            conn.query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(get(&conn, "auto_backup"), "1");
        assert_eq!(get(&conn, "backup_interval"), "7");
        let legacy_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key IN ('auto_backup_enabled','auto_backup_interval_days')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_count, 0, "legacy keys should be removed");

        // Idempotent on re-run.
        run_migrations(&conn).unwrap();
        assert_eq!(get(&conn, "auto_backup"), "1");
        assert_eq!(get(&conn, "backup_interval"), "7");
    }

    #[test]
    fn test_default_settings_seeded() {
        let conn = setup_conn();
        let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key='schema_version'").unwrap();
        let version: String = stmt.query_row([], |row| row.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn test_quality_columns_exist_after_migration() {
        let conn = setup_conn();
        let mut stmt = conn.prepare("PRAGMA table_info(content_index)").unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(names.contains(&"quality_score".to_string()), "missing quality_score: {names:?}");
        assert!(names.contains(&"quality_flags".to_string()), "missing quality_flags: {names:?}");
        assert!(names.contains(&"reextract_count".to_string()), "missing reextract_count: {names:?}");
        assert!(names.contains(&"mean_confidence".to_string()), "missing mean_confidence: {names:?}");
    }

    #[test]
    fn test_schema_version_is_9() {
        let conn = setup_conn();
        let version: String = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
                .unwrap();
        assert_eq!(version, "9");
    }

    #[test]
    fn test_init_db_with_file() {
        let tmp = std::env::temp_dir().join(format!("ls_init_test_{}.db", std::process::id()));
        let path = tmp.to_str().unwrap().to_string();

        let pool = get_pool(&path).unwrap();
        let conn = pool.get().unwrap();
        init_db(&conn).unwrap();

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

    #[test]
    fn test_ocr_pdf_dpi_seeded_with_default_300() {
        let conn = setup_conn();
        let val: String = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'ocr_pdf_dpi'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(val, "300");
    }

    #[test]
    fn test_ocr_pdf_dpi_not_overwritten_on_rerun() {
        let conn = setup_conn();
        conn.execute(
            "UPDATE app_settings SET value = '600' WHERE key = 'ocr_pdf_dpi'",
            [],
        )
        .unwrap();
        run_migrations(&conn).unwrap();
        let val: String = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'ocr_pdf_dpi'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(val, "600");
    }
}
