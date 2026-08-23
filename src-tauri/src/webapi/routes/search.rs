use axum::{
    extract::{Query, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use tauri::Manager;

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