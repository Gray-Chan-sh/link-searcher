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

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route(
            "/api/dirs",
            get(dirs_handler)
                .post(add_dir_handler)
                .put(update_dir_handler)
                .delete(remove_dir_handler),
        )
        .route("/api/dirs/tree", get(dir_tree_handler))
        .route("/api/dirs/children", get(dir_children_handler))
}

async fn dirs_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let dirs = crate::db::dir_config::list_dirs(&conn)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({ "dirs": dirs })))
}

#[derive(Deserialize)]
struct AddDirBody {
    path: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    recursive: Option<bool>,
}

async fn add_dir_handler(
    State(state): State<ApiState>,
    Json(body): Json<AddDirBody>,
) -> Result<Json<crate::commands::dirs::DirConfigResponse>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let dir = crate::commands::dirs::add_dir(app_state, body.path, body.alias, body.recursive)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(dir))
}

#[derive(Deserialize)]
struct RemoveDirBody {
    id: String,
}

async fn remove_dir_handler(
    State(state): State<ApiState>,
    Json(body): Json<RemoveDirBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    crate::commands::dirs::remove_dir(app_state, body.id)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "status": "removed" })))
}

#[derive(Deserialize)]
struct UpdateDirBody {
    id: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default, rename = "ocrLang")]
    ocr_lang: Option<String>,
    #[serde(default, rename = "excludePatterns")]
    exclude_patterns: Option<String>,
    #[serde(default, rename = "includeExts")]
    include_exts: Option<String>,
    #[serde(default)]
    recursive: Option<bool>,
}

async fn update_dir_handler(
    State(state): State<ApiState>,
    Json(body): Json<UpdateDirBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_handle = state.app_handle.clone();
    let app_state = app_handle.state::<AppState>();
    crate::commands::dirs::update_dir(
        app_state,
        app_handle.clone(),
        body.id,
        body.alias,
        body.ocr_lang,
        body.exclude_patterns,
        body.include_exts,
        body.recursive,
    )
    .await
    .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "status": "updated" })))
}

#[derive(Deserialize)]
struct DirTreeQuery {
    #[serde(rename = "dirId")]
    dir_id: String,
    #[serde(rename = "includeFiles", default)]
    include_files: Option<bool>,
}

async fn dir_tree_handler(
    State(state): State<ApiState>,
    Query(query): Query<DirTreeQuery>,
) -> Result<Json<crate::commands::dirs::DirTreeNode>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let tree =
        crate::commands::dirs::get_dir_tree(app_state, query.dir_id, query.include_files)
            .map_err(|e| ApiError { error: e })?;
    Ok(Json(tree))
}

#[derive(Deserialize)]
struct DirChildrenQuery {
    #[serde(rename = "parentPath")]
    parent_path: String,
}

async fn dir_children_handler(
    State(state): State<ApiState>,
    Query(query): Query<DirChildrenQuery>,
) -> Result<Json<Vec<crate::commands::dirs::DirTreeNode>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let children = crate::commands::dirs::get_dir_children(app_state, query.parent_path)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(children))
}
