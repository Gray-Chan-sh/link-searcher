use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::commands::tesseract::{
    check_dependencies, check_tesseract, get_file_type_support, get_unsupported_ext_stats,
    list_ocr_engines, test_ocr_engine, DependencyStatus, FileTypeInfo, OcrEngineStatus,
    OcrTestResult, UnsupportedExtInfo,
};
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/ocr/tesseract", get(tesseract_available_handler))
        .route("/api/ocr/engines", get(list_engines_handler))
        .route("/api/ocr/engines/test", post(test_engine_handler))
        .route("/api/ocr/dependencies", get(dependencies_handler))
        .route("/api/ocr/file-type-support", get(file_type_support_handler))
        .route("/api/ocr/unsupported-exts", get(unsupported_exts_handler))
}

async fn tesseract_available_handler() -> Result<Json<bool>, ApiError> {
    check_tesseract()
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

async fn list_engines_handler() -> Result<Json<Vec<OcrEngineStatus>>, ApiError> {
    list_ocr_engines().map(Json).map_err(|e| ApiError { error: e })
}

#[derive(Deserialize)]
struct TestEngineBody {
    #[serde(rename = "engineType")]
    engine_type: String,
}

async fn test_engine_handler(
    axum::Json(body): axum::Json<TestEngineBody>,
) -> Result<Json<OcrTestResult>, ApiError> {
    // OCR inference can take seconds — keep it off the async runtime threads.
    let result = tokio::task::spawn_blocking(move || test_ocr_engine(body.engine_type))
        .await
        .map_err(|e| ApiError {
            error: format!("task failed: {e}"),
        })?
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(result))
}

async fn dependencies_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<DependencyStatus>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    check_dependencies(app_state)
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

async fn file_type_support_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<FileTypeInfo>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    get_file_type_support(app_state)
        .map(Json)
        .map_err(|e| ApiError { error: e })
}

async fn unsupported_exts_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<UnsupportedExtInfo>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    get_unsupported_ext_stats(app_state)
        .map(Json)
        .map_err(|e| ApiError { error: e })
}
