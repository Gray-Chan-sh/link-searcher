use std::sync::atomic::Ordering;

use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::db;
use crate::db::tracker;
use crate::extractor::ocr;
use crate::scanner::{ScanProgress, ScanResult};
use crate::search::IndexManager;
use crate::state::{AppState, ScanDelta};

#[derive(Serialize)]
pub struct IndexHealth {
    pub healthy: bool,
    pub num_segments: usize,
    pub num_docs: u64,
    pub db_integrity: String,
}

#[derive(Serialize)]
pub struct IndexIntegrityReport {
    /// Files marked indexed=1 in the DB (expected to be searchable).
    pub db_indexed: u64,
    /// Documents actually present in the Tantivy index.
    pub tantivy_docs: u64,
    /// difference = db_indexed - tantivy_docs (positive means orphans).
    pub difference: i64,
    /// Files that were stuck at indexed=3 (extracted, never written) and
    /// were reset to pending so a re-scan rewrites them into Tantivy.
    pub resurrected: u64,
}

#[derive(Serialize)]
pub struct IndexStatus {
    pub total_files: u64,
    pub indexed: u64,
    pub pending: u64,
    pub errors: u64,
    pub ocred: u64,
    pub total_images: u64,
    pub last_scan: Option<i64>,
    pub is_scanning: bool,
    pub scan_delta: Option<ScanDelta>,
}

#[derive(Clone, Serialize)]
pub struct ScanEventPayload {
    pub phase: String,
    pub current: u64,
    pub total: u64,
    pub current_file: String,
    pub dir_id: String,
}

fn get_last_scan(conn: &rusqlite::Connection) -> Option<i64> {
    let mut stmt = conn
        .prepare("SELECT MAX(CAST(value AS INTEGER)) FROM app_settings WHERE key LIKE 'last_scan_%'")
        .ok()?;
    stmt.query_row([], |row| row.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
}

#[tauri::command]
pub async fn get_index_status(state: State<'_, AppState>) -> Result<IndexStatus, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let stats = db::tracker::get_stats(&conn, None).map_err(|e| format!("stats error: {e}"))?;
    let ocred = db::tracker::get_ocred_count(&conn).map_err(|e| format!("ocred error: {e}"))?;
    let total_images = db::tracker::get_total_image_files(&conn).map_err(|e| format!("images error: {e}"))?;
    let last_scan = get_last_scan(&conn);
    Ok(IndexStatus {
        total_files: stats.total,
        indexed: stats.indexed,
        pending: stats.pending,
        errors: stats.errors,
        ocred,
        total_images,
        last_scan,
        is_scanning: state.is_scanning.load(Ordering::Acquire),
        scan_delta: Some({ let d = state.scan_delta.lock().unwrap_or_else(|e| e.into_inner()); d.clone() }),
    })
}

#[tauri::command]
pub async fn trigger_scan(
    state: State<'_, AppState>,
    dir_id: Option<String>,
    app: AppHandle,
) -> Result<(), String> {
    if state
        .is_scanning
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a scan is already in progress".to_string());
    }

    // Check OCR engine status before starting scan
    {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let ocr_setting: String = conn
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'ocr_engine'",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "PaddleOCR".to_string());
        // Drop connection before the long-running scan
        drop(conn);

        if ocr_setting != "None"
            && !match ocr_setting.as_str() {
                "PaddleOCR" => true,     // built-in, always available
                "Tesseract" => ocr::is_tesseract_available(),
                _ => !ocr::detect_available_engines().is_empty(),
            }
        {
            state.is_scanning.store(false, Ordering::SeqCst);
            return Err(
                "OCR 引擎不可用，请在设置页面检查 OCR 引擎配置".to_string(),
            );
        }
    }

    let is_scanning = state.is_scanning.clone();
    let cancel_scan = state.cancel_scan.clone();
    let scanner = state.scanner.clone();
    let db_pool = state.db.clone();
    let app_clone = app.clone();
    let scan_delta = state.scan_delta.clone();
    let logs_dir = state.data_dir.join("logs");

    tokio::task::spawn_blocking(move || {
        cancel_scan.store(false, Ordering::Release);
        // Per-scan session log (optional — scan proceeds without it).
        let mut slog = crate::logs::session::SessionLog::open(&logs_dir, "scan")
            .map_err(|e| log::warn!("[SCAN] 无法创建会话日志: {e}"))
            .ok();
        let mut sess = |line: String| {
            if let Some(ref mut f) = slog {
                let _ = crate::logs::session::SessionLog::write(f, &line);
            }
        };
        let conn = match db_pool.get() {
            Ok(c) => c,
            Err(e) => {
                log::error!("[SCAN] db connection failed: {e}");
                is_scanning.store(false, Ordering::SeqCst);
                cancel_scan.store(false, Ordering::SeqCst);
                drop(sess);
                if let Some(f) = slog {
                    crate::logs::session::SessionLog::close(f);
                }
                return;
            }
        };
        let dirs = match db::dir_config::list_dirs(&conn) {
            Ok(d) => d,
            Err(e) => {
                log::error!("[SCAN] failed to list dirs: {e}");
                is_scanning.store(false, Ordering::SeqCst);
                cancel_scan.store(false, Ordering::SeqCst);
                drop(sess);
                if let Some(f) = slog {
                    crate::logs::session::SessionLog::close(f);
                }
                return;
            }
        };
        drop(conn);

        let mut added = 0u64;
        let mut deleted = 0u64;
        let mut modified = 0u64;
        let mut total_errors = 0u64;
        let mut total_duration_ms = 0u64;

        let targets: Vec<_> = if let Some(single_id) = &dir_id {
            dirs.into_iter().filter(|d| d.id == *single_id).collect()
        } else {
            dirs
        };

        if targets.is_empty() {
            log::warn!("[SCAN] no directories configured");
            sess("[SCAN] 无已配置目录".to_string());
            is_scanning.store(false, Ordering::SeqCst);
            cancel_scan.store(false, Ordering::SeqCst);
            drop(sess);
            if let Some(f) = slog {
                crate::logs::session::SessionLog::close(f);
            }
            return;
        }

        let full = dir_id.is_none();
        sess(format!("[SCAN] 开始扫描 {} 个目录", targets.len()));
        for dir in &targets {
            if cancel_scan.load(Ordering::Acquire) {
                log::info!("[SCAN] scan cancelled by user");
                break;
            }
            let result: Result<ScanResult, String> = if full {
                let p = |prog: ScanProgress| {
                    let _ = app_clone.emit("scan-progress", ScanEventPayload {
                        phase: prog.phase.into(),
                        current: prog.processed,
                        total: prog.total,
                        current_file: prog.current_file,
                        dir_id: dir.id.clone(),
                    });
                };
                scanner.full_scan(&dir.id, p).map_err(|e| format!("{e}"))
            } else {
                let p = |prog: ScanProgress| {
                    let _ = app_clone.emit("scan-progress", ScanEventPayload {
                        phase: prog.phase.into(),
                        current: prog.processed,
                        total: prog.total,
                        current_file: prog.current_file,
                        dir_id: dir.id.clone(),
                    });
                };
                scanner.incremental_scan(&dir.id, p).map_err(|e| format!("{e}"))
            };
            match result {
                Ok(r) => {
                    let line = format!("[SCAN] {}: {} files, {} indexed, {} errors in {}ms",
                        dir.id, r.total_files, r.indexed, r.errors, r.duration_ms);
                    log::info!("{line}");
                    sess(line);
                    added += r.added;
                    deleted += r.deleted;
                    modified += r.modified;
                    total_errors += r.errors;
                    total_duration_ms += r.duration_ms;
                }
                Err(e) => {
                    let line = format!("[SCAN] {} failed: {e}", dir.id);
                    log::error!("{line}");
                    sess(line);
                }
            }
        }

        {
            let mut delta = scan_delta.lock().unwrap_or_else(|e| e.into_inner());
            *delta = ScanDelta {
                added,
                deleted,
                modified,
                errors: total_errors,
                duration_ms: total_duration_ms,
            };
        }

        is_scanning.store(false, Ordering::SeqCst);
        cancel_scan.store(false, Ordering::SeqCst);
        sess("[SCAN] 扫描完成".to_string());
        drop(sess);
        if let Some(f) = slog {
            crate::logs::session::SessionLog::close(f);
        }
        let _ = app_clone.emit("scan-completed", serde_json::json!({}));

        // After a scan, backfill any files newly indexed without embeddings
        // (idempotent — only fills gaps). Runs in background so it never
        // delays scan completion.
        if crate::ai::embedding_enabled() {
            let db_pool_bg = db_pool.clone();
            std::thread::spawn(move || {
                let _ = run_backfill_embeddings(&db_pool_bg);
            });
        }
    });

    Ok(())
}

/// Truncate the index-facing tables before a rebuild. Embeddings/summaries
/// must go too, or stale rows would rank ghost docs in semantic search.
fn clear_index_tables(conn: &rusqlite::Connection) {
    for table in ["file_tracking", "content_index", "doc_embeddings", "doc_summaries"] {
        if let Err(e) = conn.execute(&format!("DELETE FROM {table}"), []) {
            log::warn!("[SCAN] failed to clear {table}: {e}");
        }
    }
}

#[tauri::command]
pub async fn rebuild_index(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    if state
        .is_scanning
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a scan is already in progress".to_string());
    }
    state.is_rebuilding.store(true, Ordering::SeqCst);

    let index_dir = state.index_dir.clone();
    let db_pool = state.db.clone();
    let index_manager = state.index_manager.clone();
    let indexer = state.indexer.clone();
    let scanner = state.scanner.clone();
    let is_scanning = state.is_scanning.clone();
    let is_rebuilding = state.is_rebuilding.clone();
    let cancel_scan = state.cancel_scan.clone();
    let scan_delta = state.scan_delta.clone();
    let logs_dir = state.data_dir.join("logs");

        tokio::task::spawn_blocking(move || {
            cancel_scan.store(false, Ordering::Release);
            let mut added = 0u64;
            let mut deleted = 0u64;
            let mut modified = 0u64;
            let mut total_errors = 0u64;
            let mut total_duration_ms = 0u64;

        // 1. 建临时索引目录（不删旧索引；扫描完成后再原子替换）
        let tmp_name = format!("index.tmp-{}", uuid::Uuid::new_v4().simple());
        let tmp_dir = index_dir.with_file_name(&tmp_name);
        if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
            log::error!("[SCAN] failed to create tmp index dir: {e}");
            is_scanning.store(false, Ordering::SeqCst);
            is_rebuilding.store(false, Ordering::SeqCst);
            cancel_scan.store(false, Ordering::SeqCst);
            return;
        }

        // 2. Clear file tracking
        if let Ok(conn) = db_pool.get() {
            clear_index_tables(&conn);
            drop(conn);
        }

        // 3. Create new IndexManager on tmp dir and swap in the RwLock
        match IndexManager::open_or_create(&tmp_dir) {
            Ok(new_mgr) => {
                if let Ok(mut mgr) = index_manager.write() {
                    *mgr = new_mgr;
                }
            }
            Err(e) => {
                log::error!("[SCAN] failed to create index: {e}");
                let _ = std::fs::remove_dir_all(&tmp_dir);
                is_scanning.store(false, Ordering::SeqCst);
                is_rebuilding.store(false, Ordering::SeqCst);
                cancel_scan.store(false, Ordering::SeqCst);
                return;
            }
        }

        // 4. Reset indexer writer (next write will create from new manager)
        indexer.reset_writer();

        // 5. Run full scan on all dirs
        let conn = match db_pool.get() {
            Ok(c) => c,
            Err(e) => {
                log::error!("[SCAN] db connection failed: {e}");
                let _ = std::fs::remove_dir_all(&tmp_dir);
                is_scanning.store(false, Ordering::SeqCst);
                is_rebuilding.store(false, Ordering::SeqCst);
                cancel_scan.store(false, Ordering::SeqCst);
                return;
            }
        };
        let dirs = match db::dir_config::list_dirs(&conn) {
            Ok(d) => d,
            Err(e) => {
                log::error!("[SCAN] failed to list dirs: {e}");
                let _ = std::fs::remove_dir_all(&tmp_dir);
                is_scanning.store(false, Ordering::SeqCst);
                is_rebuilding.store(false, Ordering::SeqCst);
                cancel_scan.store(false, Ordering::SeqCst);
                return;
            }
        };
        drop(conn);

        // Per-scan session log (optional — rebuild proceeds without it).
        let mut slog = crate::logs::session::SessionLog::open(&logs_dir, "scan")
            .map_err(|e| log::warn!("[SCAN] 无法创建会话日志: {e}"))
            .ok();
        let mut sess = |line: String| {
            if let Some(ref mut f) = slog {
                let _ = crate::logs::session::SessionLog::write(f, &line);
            }
        };
        sess(format!("[SCAN] 开始重建索引，扫描 {} 个目录", dirs.len()));

        for dir in &dirs {
            if cancel_scan.load(Ordering::Acquire) {
                log::info!("[SCAN] rebuild cancelled by user");
                break;
            }
            let p = |prog: ScanProgress| {
                let _ = app.emit("scan-progress", ScanEventPayload {
                    phase: prog.phase.into(),
                    current: prog.processed,
                    total: prog.total,
                    current_file: prog.current_file,
                    dir_id: dir.id.clone(),
                });
            };
            match scanner.full_scan(&dir.id, p) {
                Ok(r) => {
                    let line = format!("[SCAN] {}: {} files, {} indexed, {} errors, {}ms",
                        dir.id, r.total_files, r.indexed, r.errors, r.duration_ms);
                    log::info!("{line}");
                    sess(line);
                    added += r.added;
                    deleted += r.deleted;
                    modified += r.modified;
                    total_errors += r.errors;
                    total_duration_ms += r.duration_ms;
                }
                Err(e) => {
                    let line = format!("[SCAN] {} failed: {e}", dir.id);
                    log::error!("{line}");
                    sess(line);
                }
            }
        }

        // 6. 提交，确保 tmp 索引全部落盘
        if let Err(e) = indexer.commit() {
            log::error!("[SCAN] failed to commit tmp index: {e}");
        }

        // 7. 原子替换磁盘目录：旧索引 → backup，tmp → index_dir
        let backup = index_dir.with_file_name("index.old");
        if index_dir.exists() {
            let _ = std::fs::remove_dir_all(&backup);
            let _ = std::fs::rename(&index_dir, &backup);
        }
        match std::fs::rename(&tmp_dir, &index_dir) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&backup);
            }
            Err(e) => {
                log::error!("[SCAN] failed to swap index dir: {e}");
                if backup.exists() {
                    let _ = std::fs::rename(&backup, &index_dir);
                }
            }
        }

        {
            let mut delta = scan_delta.lock().unwrap_or_else(|e| e.into_inner());
            *delta = ScanDelta {
                added,
                deleted,
                modified,
                errors: total_errors,
                duration_ms: total_duration_ms,
            };
        }

        is_scanning.store(false, Ordering::SeqCst);
        is_rebuilding.store(false, Ordering::SeqCst);
        cancel_scan.store(false, Ordering::SeqCst);
        sess("[SCAN] 索引重建完成".to_string());
        drop(sess);
        if let Some(f) = slog {
            crate::logs::session::SessionLog::close(f);
        }
        let _ = app.emit("scan-completed", serde_json::json!({}));
    });

    Ok(())
}

#[tauri::command]
pub async fn cancel_scan(state: State<'_, AppState>) -> Result<(), String> {
    let was_scanning = state.is_scanning.load(Ordering::Acquire);
    state.cancel_scan.store(true, Ordering::Release);
    log::info!("[SCAN] cancel requested (was_scanning={was_scanning})");
    Ok(())
}

#[tauri::command]
pub async fn check_index_health(state: State<'_, AppState>) -> Result<IndexHealth, String> {
    let index = state
        .index_manager
        .read()
        .map_err(|e| format!("{e}"))?;
    let reader = index.reader().map_err(|e| format!("{e}"))?;
    let searcher = reader.searcher();
    let num_segments = searcher.segment_readers().len();
    let num_docs = searcher.num_docs();

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let db_integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap_or_else(|_| "error".to_string());
    drop(conn);

    Ok(IndexHealth {
        healthy: db_integrity == "ok" && num_segments > 0,
        num_segments,
        num_docs,
        db_integrity,
    })
}

/// Index integrity audit: compares DB marked-indexed count vs documents
/// actually present in Tantivy, and resurrects files stuck at indexed=3
/// (extracted but never written — e.g. after a crash) back to pending so a
/// re-scan rewrites them. Run after a crash or when search seems incomplete.
#[tauri::command]
pub fn check_index_integrity(state: State<'_, AppState>) -> Result<IndexIntegrityReport, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let db_indexed: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE indexed = 1",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("{e}"))?;
    // Files at indexed=3 never made it into Tantivy (crash between Phase-1
    // extraction and Phase-2 write). Reset them to pending (indexed=0) so
    // needs_reindex re-queues them on the next scan.
    let resurrected = conn
        .execute(
            "UPDATE file_tracking SET indexed = 0, updated_at = ?1 WHERE indexed = 3",
            rusqlite::params![chrono::Utc::now().timestamp()],
        )
        .map_err(|e| format!("{e}"))? as u64;
    drop(conn);

    let index = state.index_manager.read().map_err(|e| format!("{e}"))?;
    let reader = index.reader().map_err(|e| format!("{e}"))?;
    let tantivy_docs = reader.searcher().num_docs();
    drop(index);

    Ok(IndexIntegrityReport {
        db_indexed,
        tantivy_docs,
        difference: db_indexed as i64 - tantivy_docs as i64,
        resurrected,
    })
}

/// Backfill missing semantic embeddings for already-indexed files.
///
/// Reads each indexed file's extracted text from `content_index` (via md5 —
/// no re-extraction / no OCR), embeds it in batches, and upserts into
/// `doc_embeddings`. Idempotent: only files without an embedding are picked
/// up, so it is safe to run repeatedly and after every scan.
///
/// Exposed both as a Tauri command (manual trigger) and called by the scan
/// completion hook in the background.
#[tauri::command]
pub async fn backfill_embeddings(state: State<'_, AppState>) -> Result<BackfillReport, String> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || run_backfill_embeddings(&db))
        .await
        .map_err(|e| format!("backfill task failed: {e}"))?
}

fn run_backfill_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
    const BATCH: usize = 64;

    let conn = db.get().map_err(|e| format!("db error: {e}"))?;
    let pending = missing_embedding_rows(&conn).map_err(|e| e.to_string())?;
    let total = pending.len();
    if total == 0 {
        return Ok(BackfillReport { processed: 0, pending: 0, failed: 0 });
    }

    log::info!("[AI] 向量回填开始: {total} 个文件缺向量");
    let mut processed = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
        let vecs = crate::ai::embed_batched(&texts, BATCH);
        for ((id, _), v) in chunk.iter().zip(vecs) {
            match v {
                Some(vec) => {
                    if let Err(e) = tracker::upsert_embedding(&conn, id, &vec) {
                        log::warn!("[AI] upsert_embedding failed {}: {e}", id);
                        failed += 1;
                    }
                }
                None => failed += 1,
            }
        }
        processed += chunk.len();
        log::info!("[AI] 回填进度: {processed}/{total} (失败 {failed})");
    }
    log::info!("[AI] 回填完成: {processed} 处理, {failed} 失败, 剩余 {}", total - processed);
    Ok(BackfillReport {
        processed: processed as u64,
        pending: (total - processed) as u64,
        failed: failed as u64,
    })
}

/// Files that are indexed (indexed=1) but lack an embedding, with their
/// cached extracted text. The join goes through `content_index` by md5, so
/// no re-extraction / re-OCR is needed.
fn missing_embedding_rows(
    conn: &rusqlite::Connection,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT ft.id, ci.text_content
             FROM file_tracking ft
             JOIN content_index ci ON ft.md5 = ci.md5
             WHERE ft.indexed = 1 AND ft.status = 'active'
               AND NOT EXISTS (SELECT 1 FROM doc_embeddings e WHERE e.file_id = ft.id)",
        )
        .context("prepare missing-embedding query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("query missing-embedding rows")?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect missing-embedding rows")
}

/// Summary of a [`backfill_embeddings`] run.
#[derive(Serialize)]
pub struct BackfillReport {
    pub processed: u64,
    pub pending: u64,
    pub failed: u64,
}

/// Summary of a [`reextract_missing_content`] run.
#[derive(Serialize)]
pub struct ReextractReport {
    pub processed: u64,
    pub ok: u64,
    pub failed: u64,
}

/// Re-extract content for tracked files whose md5 has no `content_index`
/// row (historical extraction failures: stale .doc, scans OCR'd later, …).
/// Reuses `reindex_file`'s per-file path (clear dedup cache first) but
/// removes any stale Tantivy doc before re-adding, so no duplicates.
#[tauri::command]
pub async fn reextract_missing_content(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<ReextractReport, String> {
    let limit = limit.unwrap_or(500);
    let indexer = state.indexer.clone();
    let db_pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db_pool.get().map_err(|e| format!("db error: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT ft.id, ft.dir_id, ft.path, ft.md5, ft.indexed
                 FROM file_tracking ft
                 WHERE ft.status = 'active' AND ft.indexed IN (1, 2) AND ft.md5 IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM content_index c WHERE c.md5 = ft.md5)
                 LIMIT ?1",
            )
            .map_err(|e| format!("db prepare error: {e}"))?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("db query error: {e}"))?;
        let targets: Vec<_> = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| format!("{e}"))?;

        let (mut ok, mut failed) = (0u64, 0u64);
        for (file_id, dir_id, rel_path, md5) in targets {
            let conn = match db_pool.get() {
                Ok(c) => c,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let full_path = match crate::db::dir_config::get_dir(&conn, &dir_id)
                .map_err(|e| format!("{e}"))
                .and_then(|d| d.ok_or_else(|| "dir config missing".to_string()))
            {
                Ok(d) => std::path::Path::new(&d.path).join(&rel_path),
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let _ = tracker::delete_content(&conn, &md5);
            drop(conn);
            // Must delete the stale Tantivy doc first — re-adding without it
            // leaves a duplicate document for the same file.
            if let Err(e) = indexer.delete_document_only(&file_id) {
                log::warn!("[REEXTRACT] delete stale doc failed {file_id}: {e}");
            }
            match indexer.index_file(&file_id, &full_path, &dir_id) {
                Ok(()) => ok += 1,
                Err(e) => {
                    log::warn!("[REEXTRACT] {rel_path}: {e}");
                    failed += 1;
                }
            }
        }
        Ok(ReextractReport { processed: ok + failed, ok, failed })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?
}

/// Batch re-index of user-selected files. Reuses `reindex_file`'s per-file
/// path (clear dedup cache → delete stale Tantivy doc → index_file), run in
/// `spawn_blocking` so a large selection doesn't block the event loop.
/// `reindex_file` itself stays for single-file/restore paths.
#[tauri::command]
pub async fn reindex_files(
    state: State<'_, AppState>,
    file_ids: Vec<String>,
) -> Result<ReextractReport, String> {
    let indexer = state.indexer.clone();
    let db_pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let (mut ok, mut failed) = (0u64, 0u64);
        for file_id in file_ids {
            let conn = match db_pool.get() {
                Ok(c) => c,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            };
            let rec = match tracker::get_file_by_id(&conn, &file_id)
                .map_err(|e| format!("{e}"))
                .and_then(|r| r.ok_or_else(|| "file not found".to_string()))
            {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("[REINDEX_FILES] {file_id}: {e}");
                    failed += 1;
                    continue;
                }
            };
            let dir = match db::dir_config::get_dir(&conn, &rec.dir_id)
                .map_err(|e| format!("{e}"))
                .and_then(|d| d.ok_or_else(|| "dir config not found".to_string()))
            {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("[REINDEX_FILES] {file_id}: {e}");
                    failed += 1;
                    continue;
                }
            };
            // Clear the dedup cache for this file's hash so re-extraction
            // actually runs (important when OCR language changed).
            if let Some(ref md5) = rec.md5 {
                let _ = tracker::delete_content(&conn, md5);
            }
            let full_path = std::path::Path::new(&dir.path).join(&rec.path);
            drop(conn);
            // Must delete the stale Tantivy doc first — re-adding without it
            // leaves a duplicate document for the same file.
            if let Err(e) = indexer.delete_document_only(&file_id) {
                log::warn!("[REINDEX_FILES] delete stale doc failed {file_id}: {e}");
            }
            match indexer.index_file(&file_id, &full_path, &rec.dir_id) {
                Ok(()) => ok += 1,
                Err(e) => {
                    log::warn!("[REINDEX_FILES] {}: {e}", rec.path);
                    failed += 1;
                }
            }
        }
        Ok(ReextractReport { processed: ok + failed, ok, failed })
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?
}

#[tauri::command]
pub fn get_index_errors(state: State<'_, AppState>, limit: Option<usize>) -> Result<Vec<tracker::IndexError>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    tracker::get_index_errors(&conn, limit.unwrap_or(50)).map_err(|e| format!("{e}"))
}

/// Manual re-index of a single file. Looks up the DB record, resolves the
/// full disk path from dir_config, and re-extracts + re-indexes.
#[tauri::command]
pub async fn reindex_file(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let rec = tracker::get_file_by_id(&conn, &file_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "file not found".to_string())?;
    let dir = db::dir_config::get_dir(&conn, &rec.dir_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "dir config not found".to_string())?;
    // Clear the dedup cache for this file's hash so re-extraction actually
    // runs (important when OCR language changed).
    if let Some(ref md5) = rec.md5 {
        let _ = tracker::delete_content(&conn, md5);
    }
    let full_path = std::path::Path::new(&dir.path).join(&rec.path);
    drop(conn);
    state.indexer.index_file(&file_id, &full_path, &rec.dir_id)
        .map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn setup_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn backfill_picks_only_indexed_files_without_embedding() {
        let conn = setup_conn();
        conn.execute("INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d1','/tmp',1,0,0)", [])
            .unwrap();
        let id_a = tracker::upsert_file(&conn, "/a.txt", "d1", 1000, 10, Some("md5a")).unwrap();
        let id_b = tracker::upsert_file(&conn, "/b.txt", "d1", 1000, 10, Some("md5b")).unwrap();
        let id_c = tracker::upsert_file(&conn, "/c.txt", "d1", 1000, 10, Some("md5c")).unwrap();

        // a: indexed + content + NO embedding → should be picked
        tracker::store_content(&conn, "md5a", "content a", false, None).unwrap();
        tracker::update_indexed(&conn, &id_a, Some("md5a")).unwrap();
        // b: indexed but content missing (no md5 row) → skip
        tracker::update_indexed(&conn, &id_b, Some("md5b")).unwrap();
        // c: indexed + content + embedding ALREADY present → skip
        tracker::store_content(&conn, "md5c", "content c", false, None).unwrap();
        tracker::update_indexed(&conn, &id_c, Some("md5c")).unwrap();
        tracker::upsert_embedding(&conn, &id_c, &[1.0, 2.0, 3.0]).unwrap();

        let rows = missing_embedding_rows(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, id_a);
        assert_eq!(rows[0].1, "content a");
    }
}
#[cfg(test)]
mod clear_index_tables_tests {
    use super::clear_index_tables;

    fn setup_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn clears_tracking_embeddings_and_summaries() {
        let conn = setup_conn();
        crate::db::dir_config::add_dir(&conn, "/d", None, None, None, None, true).unwrap();
        let d = crate::db::dir_config::list_dirs(&conn).unwrap().remove(0);
        let id = crate::db::tracker::upsert_file(&conn, "a.txt", &d.id, 1, 10, None).unwrap();
        crate::db::tracker::upsert_embedding(&conn, &id, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        crate::db::tracker::upsert_summary(&conn, &id, "summary").unwrap();

        clear_index_tables(&conn);

        assert_eq!(crate::db::tracker::get_files_by_dir(&conn, &d.id).unwrap().len(), 0);
        assert_eq!(crate::db::tracker::count_embeddings(&conn).unwrap(), 0);
        assert!(crate::db::tracker::get_summary(&conn, &id).unwrap().is_none());
    }
}
