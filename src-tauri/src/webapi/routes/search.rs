use axum::{
    extract::{Query, State},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::commands::search;
use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::{default_page, default_page_size, ApiError};

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    pub dir_ids: Option<Vec<String>>,
    pub ext_filter: Option<Vec<String>>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    pub fuzzy: Option<bool>,
    pub semantic: Option<bool>,
}

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/search", get(search_handler))
        .route("/api/suggest", get(suggest_handler))
        .route("/api/search/paths", get(search_paths_handler))
        .route("/api/search/tree-prune", get(tree_prune_handler))
        .route("/api/search/history", get(history_handler).delete(clear_history_handler))
        .route("/api/search/export", post(export_handler))
        .route("/api/stats/file-types", get(file_type_stats_handler))
        .route("/api/stats/browse-types", get(browse_types_handler))
}

async fn search_handler(
    State(state): State<ApiState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let mgr = app_state
        .index_manager
        .read()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let reader = mgr.reader().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);

    let search_params = SearchParams {
        query: crate::search::schema::split_query_terms(&params.q.to_lowercase()),
        dir_ids: params.dir_ids,
        file_ids: None,
        ext_filter: params.ext_filter,
        path_prefixes: None,
        date_from: params.date_from,
        date_to: params.date_to,
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: params.page,
        page_size: params.page_size,
        fuzzy: params.fuzzy.unwrap_or(false),
        semantic: params.semantic.unwrap_or(false),
    };

    let result = searcher
        .search(&search_params)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({
        "hits": result.hits,
        "total": result.total,
        "page": result.page,
        "page_size": result.page_size,
        "took_ms": result.took_ms,
    })))
}

async fn suggest_handler(
    State(state): State<ApiState>,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<Vec<String>>, ApiError> {
    let prefix = params
        .get("prefix")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let app_state = state.app_handle.state::<AppState>();
    let mgr = app_state
        .index_manager
        .read()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let reader = mgr.reader().map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);
    let suggestions = searcher
        .suggest(prefix, 10)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(suggestions))
}

#[derive(Deserialize)]
pub struct PathsQuery {
    pub prefix: String,
    pub limit: usize,
}

async fn search_paths_handler(
    State(state): State<ApiState>,
    Query(params): Query<PathsQuery>,
) -> Result<Json<Vec<String>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let paths = search::search_file_paths_impl(&app_state, params.prefix, params.limit)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(paths))
}

#[derive(Deserialize)]
pub struct TreePruneQuery {
    pub term: String,
}

async fn tree_prune_handler(
    State(state): State<ApiState>,
    Query(params): Query<TreePruneQuery>,
) -> Result<Json<Vec<String>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let nodes = search::search_tree_prune_impl(&app_state, params.term)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(nodes))
}

async fn history_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<search::HistoryEntry>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let entries = search::get_search_history_impl(&app_state)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(entries))
}

async fn clear_history_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    search::clear_search_history_impl(&app_state)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct ExportBody {
    pub query: String,
    pub dir_ids: Option<Vec<String>>,
    pub dir_paths: Option<Vec<String>>,
    pub ext_filter: Option<Vec<String>>,
    pub format: Option<String>,
}

async fn export_handler(
    State(state): State<ApiState>,
    Json(body): Json<ExportBody>,
) -> Result<Json<String>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let content = search::export_search_results_impl(
        &app_state,
        body.query,
        body.dir_ids,
        body.dir_paths,
        body.ext_filter,
        body.format.unwrap_or_else(|| "csv".to_string()),
    )
    .await
    .map_err(|e| ApiError { error: e })?;
    Ok(Json(content))
}

async fn file_type_stats_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<search::FileTypeStat>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let stats = search::get_file_type_stats_impl(&app_state)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(stats))
}

async fn browse_types_handler(
    State(state): State<ApiState>,
) -> Result<Json<Vec<String>>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let types = search::get_browse_file_types_impl(&app_state)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(types))
}