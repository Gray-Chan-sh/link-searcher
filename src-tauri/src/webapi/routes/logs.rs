use axum::{
    extract::{Query, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

#[derive(Deserialize)]
pub struct LogsQuery {
    pub lines: Option<usize>,
    #[serde(rename = "fileId")]
    pub file_id: Option<String>,
    pub session_id: Option<String>,
}

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/logs", get(get_logs_handler).delete(clear_logs_handler))
        .route("/api/logs/sessions", get(list_sessions_handler))
}

async fn get_logs_handler(
    State(state): State<ApiState>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<Vec<String>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    crate::commands::logs::get_logs(app_state, q.lines, q.file_id, q.session_id)
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

async fn clear_logs_handler(State(state): State<ApiState>) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    crate::commands::logs::clear_logs(app_state)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "cleared": true })))
}

async fn list_sessions_handler(State(state): State<ApiState>) -> Result<Json<Vec<String>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    crate::commands::logs::list_session_logs(app_state)
        .map(Json)
        .map_err(|e| ApiError { error: e })
}
