use std::collections::HashMap;

use axum::{
    extract::{State, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/version", get(version_handler))
        .route(
            "/api/settings",
            get(settings_handler).put(update_settings_handler),
        )
        .route("/api/auth/token", post(update_token_handler))
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
        // 密钥（`web_api_token` 等）**不回显明文**：Web 端只拿到哨兵，
        // 写回时哨兵按"保持原值"处理（桌面端 IPC 不受影响，仍可查看 token）。
        let v = if crate::commands::settings::is_secret_key(&k) {
            crate::commands::settings::mask_secret(&v)
        } else {
            v
        };
        map.insert(k, serde_json::Value::String(v));
    }
    Ok(Json(serde_json::Value::Object(map)))
}

#[derive(Deserialize)]
struct UpdateSettingsBody {
    #[serde(default)]
    settings: Option<HashMap<String, String>>,
}

async fn update_settings_handler(
    State(state): State<ApiState>,
    Json(body): Json<UpdateSettingsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let settings = body.settings.ok_or_else(|| ApiError {
        error: "missing 'settings' object".into(),
    })?;
    // 掩码哨兵 = "保持原值"：Web 端表单会把 GET 回来的哨兵原样回传，
    // 直接进 `update_settings` 就会把密钥写成哨兵（token 就是这样被写坏过的）。
    let settings: HashMap<String, String> = settings
        .into_iter()
        .filter(|(_, v)| !crate::commands::settings::is_masked(v))
        .collect();
    let app_state = state.app_handle.state::<AppState>();
    crate::commands::settings::update_settings(app_state, settings)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

#[derive(Deserialize)]
pub(crate) struct UpdateTokenBody {
    pub token: String,
}

/// Update the server-side Bearer token in the database.
///
/// Protected by the bearer-auth middleware, so the caller must already hold
/// the current valid token. A forgotten token can only be reset from the
/// desktop Settings page (which writes through Tauri IPC, not this endpoint).
pub(crate) async fn update_token_handler(
    State(mut state): State<ApiState>,
    body: Json<UpdateTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let new_token = body.token.trim().to_string();
    if new_token.is_empty() {
        return Err(ApiError { error: "token cannot be empty".into() });
    }
    let app_state = state.app_handle.state::<AppState>();
    // 单一收口：`config.json`（权威，bot 是另一个进程、只能读它）+ `app_settings`
    // （鉴权中间件读它）**一次写完**，并作废当前会话 —— 持旧 token 的其它客户端
    // 下一次请求即 401 掉线。
    crate::webapi::apply_token(&app_state, &new_token).map_err(|e| ApiError { error: e })?;
    state.auth_token = std::sync::Arc::new(new_token.clone());
    log::info!("[WEBAPI] token updated via web UI");
    Ok(Json(serde_json::json!({ "status": "ok", "token": new_token })))
}
