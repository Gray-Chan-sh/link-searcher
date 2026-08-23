use std::sync::atomic::Ordering;

use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use tauri::Emitter;
use tauri::Manager;

use crate::db::tracker;
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/index/status", get(index_status_handler))
        .route("/api/index/health", get(index_health_handler))
        .route("/api/scan/trigger", post(trigger_scan_handler))
        .route("/api/scan/cancel", post(cancel_scan_handler))
        .route("/api/reindex", post(reindex_handler))
}

async fn index_status_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let total: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'active'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let indexed: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND indexed = 1",
            [],
            |r| r.get(0),
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let pending: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND indexed IN (0, 3)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let failed: u64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND indexed = 2",
            [],
            |r| r.get(0),
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;

    let mut stmt = conn
        .prepare(
            "SELECT id, path, error_msg FROM file_tracking WHERE status = 'active' AND indexed = 2 ORDER BY updated_at DESC LIMIT 10",
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let errors: Vec<serde_json::Value> = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "path": row.get::<_, String>(1)?,
                "error": row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            }))
        })
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?
        .filter_map(|r| r.ok())
        .collect();

    let is_scanning = app_state.is_scanning.load(Ordering::Relaxed);
    let is_rebuilding = app_state.is_rebuilding.load(Ordering::Relaxed);

    Ok(Json(serde_json::json!({
        "total": total,
        "indexed": indexed,
        "pending": pending,
        "failed": failed,
        "is_scanning": is_scanning,
        "is_rebuilding": is_rebuilding,
        "recent_errors": errors,
    })))
}

async fn index_health_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let db_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'active'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let mgr = app_state
        .index_manager
        .read()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let reader = mgr.reader().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let index_count = reader.searcher().num_docs() as i64;
    Ok(Json(serde_json::json!({
        "db_doc_count": db_count,
        "index_doc_count": index_count,
        "healthy": (db_count - index_count).abs() < 100,
    })))
}

async fn trigger_scan_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let dirs = crate::db::dir_config::list_dirs(&conn)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    drop(conn);
    if dirs.is_empty() {
        return Err(ApiError {
            error: "no directories configured".into(),
        });
    }
    let app_handle = state.app_handle.clone();
    let scanner = app_state.scanner.clone();
    tokio::task::spawn_blocking(move || {
        for dir in &dirs {
            let _ = scanner.full_scan(&dir.id, |_| {});
        }
        let _ = app_handle.emit("scan-completed", serde_json::json!({}));
    });
    Ok(Json(serde_json::json!({ "status": "started" })))
}

async fn cancel_scan_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    app_state.cancel_scan.store(true, Ordering::Relaxed);
    crate::ai::cancel_ai();
    Ok(Json(serde_json::json!({ "status": "cancelled" })))
}

async fn reindex_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let file_id = body
        .get("file_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError {
            error: "missing file_id".into(),
        })?;
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let rec = tracker::get_file_by_id(&conn, file_id)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?
        .ok_or_else(|| ApiError {
            error: "file not found".into(),
        })?;
    let dir = crate::db::dir_config::get_dir(&conn, &rec.dir_id)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?
        .ok_or_else(|| ApiError {
            error: "dir not found".into(),
        })?;
    if let Some(ref md5) = rec.md5 {
        let _ = tracker::delete_content(&conn, md5);
    }
    let full_path = std::path::Path::new(&dir.path).join(&rec.path);
    drop(conn);
    app_state
        .indexer
        .index_file(file_id, &full_path, &rec.dir_id)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({ "status": "reindexed" })))
}