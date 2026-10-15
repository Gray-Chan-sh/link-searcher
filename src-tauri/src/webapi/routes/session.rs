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

/// 心跳：刷新"最近活跃来源"由 `session_guard` 完成，这里只回状态。
///
/// 阶段四（D1＝放弃 IP 排他）之后**心跳已不再是"保命"动作** —— 不再有超时被 403 的风险，
/// 前端仍打它只是为了状态展示；保留端点以免破坏既有前端。
async fn ping_handler(
    State(state): State<ApiState>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> Json<serde_json::Value> {
    let app_state = state.app_handle.state::<AppState>();
    Json(session::status_json(&app_state, Some(&ip)))
}

/// 主动退出（web 端「退出登录」）—— 语义已降级为"清空在线记录"，**不再是权限**：
/// 阶段四起别的来源随时可用，所以这里按 IP 校验只是"谁先记录谁清"，避免误清他人记录。
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
