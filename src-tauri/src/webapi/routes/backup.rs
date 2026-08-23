use axum::{
    extract::{Path, State},
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::commands::backup::{
    delete_backup, get_backup_status, get_dead_dirs, list_backups, remap_dir,
    remove_dir_with_files, trigger_backup, BackupInfo, BackupSnapshot, DeadDirInfo,
};
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/backup/trigger", post(trigger_backup_handler))
        .route("/api/backup/status", get(backup_status_handler))
        .route("/api/backup/list", get(list_backups_handler))
        .route("/api/backup/dead-dirs", get(dead_dirs_handler))
        .route("/api/backup/remap", post(remap_handler))
        .route("/api/backup/dir/{dirId}", delete(remove_dir_with_files_handler))
        .route("/api/backup/{name}", delete(delete_backup_handler))
}

async fn trigger_backup_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // ponytail: runs on the runtime thread like the Tauri IPC path; move to
    // spawn_blocking if large indexes make this noticeable.
    let app_state = state.app_handle.state::<AppState>();
    trigger_backup(app_state)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn backup_status_handler(
    State(state): State<ApiState>,
) -> Result<Json<BackupInfo>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    get_backup_status(app_state)
        .await
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

async fn list_backups_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<BackupSnapshot>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    list_backups(app_state)
        .await
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

async fn delete_backup_handler(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    delete_backup(app_state, name)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn dead_dirs_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<DeadDirInfo>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    get_dead_dirs(app_state)
        .await
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

#[derive(Deserialize)]
struct RemapBody {
    #[serde(rename = "dirId")]
    dir_id: String,
    #[serde(rename = "newPath")]
    new_path: String,
}

async fn remap_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<RemapBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    remap_dir(app_state, body.dir_id, body.new_path)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(|e| ApiError { error: e })
}

async fn remove_dir_with_files_handler(
    State(state): State<ApiState>,
    Path(dir_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    remove_dir_with_files(app_state, dir_id)
        .await
        .map(|_| Json(serde_json::json!({ "ok": true })))
        .map_err(|e| ApiError { error: e })
}
