//! Dependency center over HTTP（依赖中心）。
//!
//! 桌面端依赖面板靠 4 个 Tauri 命令驱动，这里把它们原样暴露到 Web，
//! **复用 `deps::commands` 的实现**（不复制逻辑），于是 Web 端与桌面端
//! 行为完全一致：
//!
//! - `GET  /api/setup/status`          → `get_setup_status`（每个依赖的
//!   ready/available/size/hint，依赖面板主数据）
//! - `POST /api/setup/install`         → `install_dep`（body `{ "dep": "bge-large" }`）
//! - `POST /api/setup/cancel`          → `cancel_dep_install`
//! - `GET  /api/setup/install-status`  → `dep_install_status`（刷新后恢复进度条）
//!
//! 进度靠事件桥：`dep-progress` / `dep-install-done` 已加入
//! [`crate::webapi::BRIDGED_EVENTS`]，浏览器经 `GET /api/events` (SSE) 收到。
//!
//! 并发安全：这些路由在 `bearer_auth` + `session_guard` 之内，且单用户
//! 会话制保证同一时刻只有一个 Web 客户端——不会出现两个浏览器同时点安装
//! 触发 `INSTALLING_DEP` 竞争（后端本身也有 `INSTALLING_DEP` 单飞守卫）。

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/setup/status", get(setup_status_handler))
        .route("/api/setup/install", post(install_dep_handler))
        .route("/api/setup/cancel", post(cancel_dep_install_handler))
        .route("/api/setup/install-status", get(dep_install_status_handler))
}

async fn setup_status_handler(
    State(state): State<ApiState>,
) -> Result<Json<crate::deps::SetupStatus>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    crate::deps::commands::get_setup_status(app_state)
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

#[derive(Deserialize)]
struct InstallDepBody {
    dep: String,
}

async fn install_dep_handler(
    State(state): State<ApiState>,
    Json(body): Json<InstallDepBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // 下载在后台线程里跑（install_dep 自身即 spawn + 立即返回），
    // 这里只负责校验与启动，进度走事件桥。
    let dep = body.dep.trim().to_string();
    if dep.is_empty() {
        return Err(ApiError {
            error: "missing 'dep'".into(),
        });
    }
    let app_state = state.app_handle.state::<AppState>();
    crate::deps::commands::install_dep(app_state, state.app_handle.clone(), dep)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "status": "started" })))
}

async fn cancel_dep_install_handler() -> Result<Json<serde_json::Value>, ApiError> {
    crate::deps::commands::cancel_dep_install()
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "status": "cancelling" })))
}

async fn dep_install_status_handler() -> Result<Json<serde_json::Value>, ApiError> {
    crate::deps::commands::dep_install_status()
        .map(Json)
        .map_err(|e| ApiError { error: e })
}
