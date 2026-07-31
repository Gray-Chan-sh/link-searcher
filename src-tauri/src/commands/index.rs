use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::db;
use crate::extractor::ocr;
use crate::scanner::{ScanProgress, ScanResult};
use crate::search::IndexManager;
use crate::state::AppState;

#[derive(Serialize)]
pub struct IndexHealth {
    pub healthy: bool,
    pub num_segments: usize,
    pub num_docs: u64,
    pub db_integrity: String,
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
}

#[derive(Clone, Serialize)]
struct ScanEventPayload {
    phase: String,
    current: u64,
    total: u64,
    current_file: String,
    dir_id: String,
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
        is_scanning: state.is_scanning.load(Ordering::Relaxed),
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
    let scanner = state.scanner.clone();
    let db_pool = state.db.clone();
    let app_clone = app.clone();

    tokio::task::spawn_blocking(move || {
        let conn = match db_pool.get() {
            Ok(c) => c,
            Err(e) => {
                log::error!("[SCAN] db connection failed: {e}");
                is_scanning.store(false, Ordering::SeqCst);
                return;
            }
        };
        let dirs = match db::dir_config::list_dirs(&conn) {
            Ok(d) => d,
            Err(e) => {
                log::error!("[SCAN] failed to list dirs: {e}");
                is_scanning.store(false, Ordering::SeqCst);
                return;
            }
        };
        drop(conn);

        let targets: Vec<_> = if let Some(single_id) = &dir_id {
            dirs.into_iter().filter(|d| d.id == *single_id).collect()
        } else {
            dirs
        };

        if targets.is_empty() {
            log::warn!("[SCAN] no directories configured");
            is_scanning.store(false, Ordering::SeqCst);
            return;
        }

        let full = dir_id.is_none();
        for dir in &targets {
            if !is_scanning.load(Ordering::Relaxed) {
                break;
            }
            let result: Result<ScanResult, String> = if full {
                let p = |prog: ScanProgress| {
                    let _ = app_clone.emit("scan-progress", ScanEventPayload {
                        phase: "scanning".into(),
                        current: prog.processed,
                        total: prog.total,
                        current_file: prog.current_file,
                        dir_id: dir.id.clone(),
                    });
                };
                scanner.full_scan(&dir.id, p).map_err(|e| format!("{e}"))
            } else {
                scanner.incremental_scan(&dir.id).map_err(|e| format!("{e}"))
            };
            match result {
                Ok(r) => log::info!("[SCAN] {}: {} files, {} indexed, {} errors in {}ms",
                    dir.id, r.total_files, r.indexed, r.errors, r.duration_ms),
                Err(e) => log::error!("[SCAN] {} failed: {e}", dir.id),
            }
        }

        is_scanning.store(false, Ordering::SeqCst);
        let _ = app_clone.emit("scan-completed", serde_json::json!({}));
    });

    Ok(())
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

    let index_dir = state.index_dir.clone();
    let db_pool = state.db.clone();
    let index_manager = state.index_manager.clone();
    let indexer = state.indexer.clone();
    let scanner = state.scanner.clone();
    let is_scanning = state.is_scanning.clone();

    tokio::task::spawn_blocking(move || {
        // 1. Delete old index directory
        if let Err(e) = std::fs::remove_dir_all(&index_dir) {
            log::error!("[SCAN] failed to remove index dir: {e}");
        }
        std::fs::create_dir_all(&index_dir).ok();

        // 2. Clear file tracking
        if let Ok(conn) = db_pool.get() {
            let _ = conn.execute("DELETE FROM file_tracking", []);
            let _ = conn.execute("DELETE FROM content_index", []);
            drop(conn);
        }

        // 3. Create new IndexManager and swap in the RwLock
        match IndexManager::open_or_create(&index_dir) {
            Ok(new_mgr) => {
                if let Ok(mut mgr) = index_manager.write() {
                    *mgr = new_mgr;
                }
            }
            Err(e) => {
                log::error!("[SCAN] failed to create index: {e}");
                is_scanning.store(false, Ordering::SeqCst);
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
                is_scanning.store(false, Ordering::SeqCst);
                return;
            }
        };
        let dirs = match db::dir_config::list_dirs(&conn) {
            Ok(d) => d,
            Err(e) => {
                log::error!("[SCAN] failed to list dirs: {e}");
                is_scanning.store(false, Ordering::SeqCst);
                return;
            }
        };
        drop(conn);

        for dir in &dirs {
            if !is_scanning.load(Ordering::Relaxed) {
                break;
            }
            let p = |prog: ScanProgress| {
                let _ = app.emit("scan-progress", ScanEventPayload {
                    phase: "rebuild".into(),
                    current: prog.processed,
                    total: prog.total,
                    current_file: prog.current_file,
                    dir_id: dir.id.clone(),
                });
            };
            match scanner.full_scan(&dir.id, p) {
                Ok(r) => log::info!("[SCAN] {}: {} files, {} indexed, {} errors",
                    dir.id, r.total_files, r.indexed, r.errors),
                Err(e) => log::error!("[SCAN] {} failed: {e}", dir.id),
            }
        }

        is_scanning.store(false, Ordering::SeqCst);
        let _ = app.emit("scan-completed", serde_json::json!({}));
    });

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
    drop(reader);

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