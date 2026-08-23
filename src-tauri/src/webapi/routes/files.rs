use axum::{
    extract::{Path, Query, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::db::tracker;
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

#[derive(Deserialize)]
pub struct FilesQuery {
    pub filter: Option<String>,
    pub ext: Option<String>,
    pub search: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/files", get(files_handler))
        .route("/api/files/{id}/preview", get(file_preview_handler))
}

async fn files_handler(
    State(state): State<ApiState>,
    Query(params): Query<FilesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;

    let ps = params.page_size.unwrap_or(50).max(1).min(1000);
    let p = params.page.unwrap_or(1).max(1);
    let offset = (p - 1) * ps;

    let mut wheres: Vec<&str> = vec!["status = 'active'"];
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql + Send>> = Vec::new();

    match params.filter.as_deref() {
        Some("indexed") => {
            wheres.push("indexed = 1");
        }
        Some("pending") => {
            wheres.push("indexed IN (0, 3)");
        }
        Some("failed") => {
            wheres.push("indexed = 2");
        }
        _ => {}
    }
    if let Some(e) = &params.ext {
        wheres.push("path LIKE ?");
        sql_params.push(Box::new(format!("%.{e}")));
    }
    if let Some(s) = &params.search {
        wheres.push("path LIKE ?");
        sql_params.push(Box::new(format!("%{s}%")));
    }

    let where_clause = wheres.join(" AND ");
    let total: u64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM file_tracking WHERE {where_clause}"),
            rusqlite::params_from_iter(sql_params.iter().map(|p| p as &dyn rusqlite::ToSql)),
            |row| row.get(0),
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;

    let sort_col = match params.sort.as_deref().unwrap_or("path") {
        "ext" => "file_ext",
        "size" => "size",
        "mtime" => "mtime",
        _ => "path",
    };
    let order_dir = if params.order.as_deref() == Some("desc") {
        "DESC"
    } else {
        "ASC"
    };

    let data_sql = format!(
        "SELECT id, path, size, mtime, indexed FROM file_tracking WHERE {where_clause} ORDER BY {sort_col} {order_dir} LIMIT ?{} OFFSET ?{}",
        sql_params.len() + 1,
        sql_params.len() + 2,
    );
    let mut data_params: Vec<Box<dyn rusqlite::ToSql + Send>> = sql_params;
    data_params.push(Box::new(ps as i64));
    data_params.push(Box::new(offset as i64));

    let mut stmt = conn
        .prepare(&data_sql)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(data_params.iter().map(|p| p as &dyn rusqlite::ToSql)),
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "size": row.get::<_, u64>(2)?,
                    "mtime": row.get::<_, i64>(3)?,
                    "indexed": row.get::<_, i64>(4)?,
                }))
            },
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let files: Vec<serde_json::Value> = rows.filter_map(|r| r.ok()).collect();

    Ok(Json(serde_json::json!({
        "files": files,
        "total": total,
        "page": p,
        "page_size": ps,
    })))
}

async fn file_preview_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let rec = tracker::get_file_by_id(&conn, &id)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?
        .ok_or_else(|| ApiError {
            error: "file not found".into(),
        })?;
    let text = rec
        .md5
        .as_ref()
        .and_then(|md5| tracker::get_content(&conn, md5).ok()?)
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "id": rec.id,
        "path": rec.path,
        "size": rec.size,
        "mtime": rec.mtime,
        "content": text.chars().take(50000).collect::<String>(),
    })))
}