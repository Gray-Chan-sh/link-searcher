use axum::{
    extract::{Extension, State},
    routing::{get, post},
    Json, Router,
};
use tauri::Manager;

use crate::state::AppState;
use crate::webapi::session::{self, ClientIp};
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/session", get(status_handler))
        .route("/api/session/ping", post(ping_handler))
        .route("/api/session/logout", post(logout_handler))
}

/// 当前会话状态（谁在线、剩余时间、局域网访问地址）。
async fn status_handler(
    State(state): State<ApiState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> Json<serde_json::Value> {
    let app_state = state.app_handle.state::<AppState>();
    Json(session::status_json(&app_state, Some(&ip)))
}

/// 心跳：续租由 `session_guard` 完成，这里只回状态。
/// 前端每 30s 打一次，保证 SSE 长连接期间会话不被超时回收。
async fn ping_handler(
    State(state): State<ApiState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> Json<serde_json::Value> {
    let app_state = state.app_handle.state::<AppState>();
    Json(session::status_json(&app_state, Some(&ip)))
}

/// 主动退出（web 端「退出登录」）。`session_guard` 已保证只有持有者 IP 能到达这里；
/// 这里仍按 IP 再校验一次，避免与「超时后被他人接管」的窗口期竞争。
async fn logout_handler(
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if session::clear(Some(&ip)) {
        log::info!("[WEBAPI-SESSION] user logged out ({ip})");
        Ok(Json(serde_json::json!({ "status": "logged_out" })))
    } else {
        Err(ApiError { error: "no active session".into() })
    }
}
