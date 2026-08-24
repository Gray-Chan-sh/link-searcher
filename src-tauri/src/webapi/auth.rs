use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
    http::StatusCode,
};
use tauri::Manager;
use crate::webapi::state::ApiState;

/// Bearer token auth middleware — reads from ApiState.
pub async fn bearer_auth(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let header_token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if header_token.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let db_token = {
        let app_state = state.app_handle.state::<crate::state::AppState>();
        app_state.db.get().ok().and_then(|conn| {
            conn.query_row::<String, _, _>(
                "SELECT value FROM app_settings WHERE key = 'web_api_token'",
                [],
                |r| r.get(0),
            ).ok()
        })
    };
    if let Some(db_token) = &db_token {
        if header_token == db_token.as_str() {
            return Ok(next.run(req).await);
        }
    }
    Err(StatusCode::UNAUTHORIZED)
}