use axum::{
    extract::State,
    response::Json,
    routing::get,
    Router,
};
use tauri::Manager;

use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/version", get(version_handler))
        .route("/api/settings", get(settings_handler))
}

async fn version_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "hash": env!("GIT_VERSION"),
        "time": env!("GIT_COMMIT_TIME"),
    }))
}

async fn settings_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM app_settings")
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let mut map = serde_json::Map::new();
    for row in rows {
        let (k, v) = row.map_err(|e| ApiError {
            error: e.to_string(),
        })?;
        map.insert(k, serde_json::Value::String(v));
    }
    Ok(Json(serde_json::Value::Object(map)))
}