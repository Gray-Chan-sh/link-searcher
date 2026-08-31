use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use tauri::Manager;

use crate::commands::files;
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
        .route("/api/files/preview-by-path", get(preview_by_path_handler))
        .route("/api/files/{id}", get(get_file_handler))
        .route("/api/files/{id}/preview", get(file_preview_handler))
        .route("/api/files/browse", get(files_browse_handler))
        .route("/api/dir-entries", get(dir_entries_handler))
        .route("/api/files/download", post(desktop_only_stub))
        .route("/api/files/open", post(desktop_only_stub))
        .route("/api/files/reveal", post(desktop_only_stub))
}

fn path_to_ext(path: &str) -> String {
    path.rsplit('.').next().filter(|e| e.len() <= 6).unwrap_or("").to_string()
}

#[derive(Deserialize)]
pub struct PreviewByPathQuery {
    pub path: String,
}

async fn preview_by_path_handler(
    State(state): State<ApiState>,
    Query(params): Query<PreviewByPathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let rec = tracker::get_file_by_path(&conn, &params.path)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "file not found".into() })?;
    let text = rec.md5.as_ref()
        .and_then(|md5| tracker::get_content(&conn, md5).ok()?)
        .unwrap_or_default();
    let content = text.chars().take(50000).collect::<String>();
    let ext = path_to_ext(&rec.path);
    let file_type = if ext.is_empty() { "unknown" }
        else if ["jpg","jpeg","png","gif","bmp","webp","tiff"].contains(&ext.as_str()) { "image" }
        else if ext == "pdf" { "pdf" }
        else if ["docx","doc","xlsx","xls","pptx","ppt","odt","ods","odp","rtf","epub"].contains(&ext.as_str()) { "office" }
        else { "text" };
    Ok(Json(serde_json::json!({
        "content": content,
        "image_path": serde_json::Value::Null,
        "image_base64": serde_json::Value::Null,
        "file_type": file_type,
        "char_count": content.chars().count(),
        "ocr_used": false,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseQuery {
    pub dir_id: Option<String>,
    pub status: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

async fn files_browse_handler(
    State(state): State<ApiState>,
    Query(params): Query<BrowseQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let resp = files::list_files(
        app_state,
        params.dir_id,
        params.status,
        params.page,
        params.page_size,
    )
    .await
    .map_err(|e| ApiError { error: e })?;
    serde_json::to_value(resp)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

#[derive(Deserialize)]
pub struct DirEntriesQuery {
    pub path: String,
}

async fn dir_entries_handler(
    State(state): State<ApiState>,
    Query(params): Query<DirEntriesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let entries = files::list_dir_entries(app_state, params.path)
        .await
        .map_err(|e| ApiError { error: e })?;
    serde_json::to_value(entries)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

// ponytail: desktop-only (opener/Finder); wire commands::files fns when remote clients need them.
async fn desktop_only_stub() -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "Desktop-only feature")
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

    let ps = params.page_size.unwrap_or(50).clamp(1, 1000);
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
                let path: String = row.get(1)?;
                let file_name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let file_ext = path.rsplit('.').next().filter(|e| e.len() <= 6).unwrap_or("").to_string();
                let rel_path = path.clone();
                Ok(serde_json::json!({
                    "file_id": row.get::<_, String>(0)?,
                    "file_name": file_name,
                    "rel_path": rel_path,
                    "file_ext": file_ext,
                    "file_size": row.get::<_, u64>(2)?,
                    "mtime": row.get::<_, i64>(3)?,
                    "indexed": row.get::<_, i64>(4)?,
                    "error_msg": serde_json::Value::Null,
                }))
            },
        )
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let files: Vec<serde_json::Value> = rows.filter_map(|r| r.ok()).collect();

    Ok(Json(serde_json::json!({
        "items": files,
        "total": total,
        "page": p,
        "page_size": ps,
    })))
}

async fn get_file_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let rec = tracker::get_file_by_id(&conn, &id)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "file not found".into() })?;
    let file_name = rec.path.rsplit('/').next().unwrap_or(&rec.path).to_string();
    let file_ext = rec.path.rsplit('.').next().filter(|e| e.len() <= 6).unwrap_or("").to_string();
    Ok(Json(serde_json::json!({
        "id": rec.id,
        "path": rec.path,
        "file_name": file_name,
        "file_ext": file_ext,
        "mtime": rec.mtime,
        "file_size": rec.size,
        "md5": rec.md5,
        "indexed": rec.indexed == 1,
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
    let content = text.chars().take(50000).collect::<String>();
    let file_type = if path_to_ext(&rec.path).is_empty() { "unknown" }
        else if ["jpg","jpeg","png","gif","bmp","webp","tiff"].contains(&path_to_ext(&rec.path).as_str()) { "image" }
        else if path_to_ext(&rec.path) == "pdf" { "pdf" }
        else if ["docx","doc","xlsx","xls","pptx","ppt","odt","ods","odp","rtf","epub"].contains(&path_to_ext(&rec.path).as_str()) { "office" }
        else { "text" };
    Ok(Json(serde_json::json!({
        "content": content,
        "image_path": null,
        "image_base64": null,
        "file_type": file_type,
        "char_count": content.chars().count(),
        "ocr_used": false,
    })))
}