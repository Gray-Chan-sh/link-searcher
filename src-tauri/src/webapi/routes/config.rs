use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::commands::config;
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/config", get(get_config_handler).put(update_config_handler))
        .route("/api/config/providers", post(add_provider_handler))
        .route(
            "/api/config/providers/{id}",
            put(update_provider_handler).delete(delete_provider_handler),
        )
        .route(
            "/api/config/providers/{id}/refresh",
            post(refresh_provider_models_handler),
        )
        .route("/api/config/active-model", put(set_active_model_handler))
        .route(
            "/api/config/providers/{id}/test",
            post(test_provider_handler),
        )
        .route("/api/config/migrate", post(migrate_data_handler))
        .route("/api/system/restart", post(restart_app_handler))
}

async fn get_config_handler() -> Result<Json<config::ConfigInfo>, ApiError> {
    config::get_config().map(Json).map_err(|e| ApiError { error: e })
}

async fn update_config_handler(
    State(state): State<ApiState>,
    Json(body): Json<config::ConfigInfo>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    config::update_config(app_state, body)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProviderBody {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
}

async fn add_provider_handler(
    Json(body): Json<AddProviderBody>,
) -> Result<Json<config::ProviderOutcome>, ApiError> {
    let outcome = config::add_provider(body.name, body.base_url, body.api_key)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(outcome))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProviderBody {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
}

async fn update_provider_handler(
    Path(id): Path<String>,
    Json(body): Json<UpdateProviderBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    config::update_provider(id, body.name, body.base_url, body.api_key)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_provider_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    config::delete_provider(id).map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn refresh_provider_models_handler(
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::config::ModelConfig>>, ApiError> {
    let models = config::refresh_provider_models(id)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(models))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetActiveModelBody {
    pub kind: String,
    pub model_id: String,
}

async fn set_active_model_handler(
    Json(body): Json<SetActiveModelBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    config::set_active_model(body.kind, body.model_id)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProviderBody {
    pub base_url: String,
    pub api_key: String,
}

async fn test_provider_handler(
    Path(_id): Path<String>,
    Json(body): Json<TestProviderBody>,
) -> Result<Json<config::ProviderTest>, ApiError> {
    let result = config::test_provider(body.base_url, body.api_key)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(result))
}

fn not_implemented(feature: &str) -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ApiError {
            error: format!("{feature} 仅在桌面端可用"),
        }),
    )
}

/// Desktop-only: needs AppHandle events + full AppState lifecycle.
async fn migrate_data_handler() -> impl IntoResponse {
    not_implemented("数据迁移")
}

/// Desktop-only: restarts the Tauri app process.
async fn restart_app_handler() -> impl IntoResponse {
    not_implemented("重启应用")
}
