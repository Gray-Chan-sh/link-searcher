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

/// 预览接口的字符上限（**纯正文**的长度，加标签前截断）。
pub const PREVIEW_CHAR_LIMIT: usize = 50_000;

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
        .route("/api/files/resolve-path", get(resolve_path_handler))
        .route("/api/files/{id}", get(get_file_handler))
        .route("/api/files/{id}/abs-path", get(abs_path_handler))
        .route("/api/files/{id}/preview", get(file_preview_handler))
        .route("/api/files/{id}/export", get(file_export_handler))
        .route("/api/files/browse", get(files_browse_handler))
        .route("/api/dir-entries", get(dir_entries_handler))
        .route("/api/files/download", post(desktop_only_stub))
        .route("/api/files/open", post(desktop_only_stub))
        .route("/api/files/reveal", post(desktop_only_stub))
        .route("/api/files/restore", post(restore_handler))
}

/// Body for the restore endpoint: `{ "ids": ["<file_id>", ...] }`.
#[derive(Deserialize)]
struct RestoreBody {
    ids: Vec<String>,
}

/// Restore soft-deleted records (Browse「已删除」视图) — reuses the Tauri
/// command so desktop and web modes behave identically.
async fn restore_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<RestoreBody>,
) -> Result<Json<u64>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let restored = crate::commands::index::restore_files(app_state, body.ids)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(restored))
}

fn path_to_ext(path: &str) -> String {
    path.rsplit('.').next().filter(|e| e.len() <= 6).unwrap_or("").to_string()
}

#[derive(Deserialize)]
pub struct PreviewByPathQuery {
    pub path: String,
}

/// 相对路径 → 绝对路径（文件夹视图解析引用深链）。返回 `{ "path": "…" | null }`。
async fn resolve_path_handler(
    State(state): State<ApiState>,
    Query(params): Query<PreviewByPathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let resolved = crate::commands::files::resolve_file_path(app_state, params.path)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "path": resolved })))
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
    // 与桌面预览同源：音频输出带标签版本，其它类型原样；统一的 5 万字上限
    // 作用于**纯正文**（截断发生在加标签之前）。
    let (content, char_count, _ocr, has_timeline) = files::preview_content(
        &conn,
        &rec.path,
        rec.md5.as_deref(),
        Some(PREVIEW_CHAR_LIMIT),
    );
    let file_type = files::file_type_of(&rec.path);
    Ok(Json(serde_json::json!({
        "content": content.unwrap_or_default(),
        "image_path": serde_json::Value::Null,
        "image_base64": serde_json::Value::Null,
        "file_type": file_type,
        "char_count": char_count,
        "ocr_used": false,
        "has_timeline": has_timeline,
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
    // 越界防护：Web 端只能列举已注册索引目录之内的路径（见 mod.rs 说明）。
    {
        let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
        super::ensure_within_registered_roots(&conn, &params.path)?;
    }
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

    // deleted 视图（与 Tauri list_files_db 保持一致）：其余筛选只看 active。
    let mut wheres: Vec<&str> = if params.filter.as_deref() == Some("deleted") {
        vec!["status = 'deleted'"]
    } else {
        vec!["status = 'active'"]
    };
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
        "SELECT id, path, size, mtime, indexed, status FROM file_tracking WHERE {where_clause} ORDER BY {sort_col} {order_dir} LIMIT ?{} OFFSET ?{}",
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
                    "status": row.get::<_, String>(5)?,
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

/// 解析一个受控文件的**绝对路径**（供同机 bot 发送到飞书）。
///
/// 这是 bot 与主程序之间"文件字节从哪来"的唯一接缝：bot 是独立进程，不能直接查 DB；
/// 它拿 `file_id` 换绝对路径，再由自己调用 lark-cli 上传（见 `lark-bot/src/media.rs`）。
///
/// 安全边界：只接受**索引记录里的相对路径**，且 canonicalize 后必须仍在该目录根之内
/// —— `..` 穿越、绝对路径注入、指向外部的符号链接一律拒绝。
async fn abs_path_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let rec = tracker::get_file_by_id(&conn, &id)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "file not found".into() })?;
    let dir = crate::db::dir_config::get_dir(&conn, &rec.dir_id)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "dir not found".into() })?;
    drop(conn);

    let abs = resolve_tracked_file(&dir.path, &rec.path).map_err(|e| ApiError { error: e })?;
    let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(rec.path.as_str())
        .to_string();
    Ok(Json(serde_json::json!({
        "file_id": rec.id,
        "abs_path": abs.to_string_lossy(),
        "file_name": name,
        "file_ext": path_to_ext(&rec.path),
        "file_size": size,
    })))
}

/// 把「目录根 + 相对路径」解析成受控的绝对路径。
///
/// 先做**纯词法**校验（[`rel_within_root`] 挡 `..` / 绝对路径），再 `canonicalize`
/// 校验真实路径仍在目录根内（挡符号链接逃逸）；不存在、不是普通文件都报错。
pub(crate) fn resolve_tracked_file(dir_root: &str, rel: &str) -> Result<std::path::PathBuf, String> {
    if !rel_within_root(rel) {
        return Err("file path escapes the indexed directory".into());
    }
    let root = std::fs::canonicalize(dir_root)
        .map_err(|e| format!("indexed directory is unavailable: {e}"))?;
    let abs = std::fs::canonicalize(std::path::Path::new(dir_root).join(rel))
        .map_err(|e| format!("file is unavailable: {e}"))?;
    if !abs.starts_with(&root) {
        return Err("file path escapes the indexed directory".into());
    }
    if !abs.is_file() {
        return Err("not a regular file".into());
    }
    Ok(abs)
}

/// 相对路径的词法校验：非空、非绝对、不含 `..`。纯函数，便于单测（不碰磁盘）。
pub(crate) fn rel_within_root(rel: &str) -> bool {
    use std::path::Component;
    let rel = rel.trim();
    !rel.is_empty()
        && !std::path::Path::new(rel).is_absolute()
        && std::path::Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
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
    let (content, char_count, _ocr, has_timeline) = files::preview_content(
        &conn,
        &rec.path,
        rec.md5.as_deref(),
        Some(PREVIEW_CHAR_LIMIT),
    );
    let file_type = files::file_type_of(&rec.path);
    Ok(Json(serde_json::json!({
        "content": content.unwrap_or_default(),
        "image_path": null,
        "image_base64": null,
        "file_type": file_type,
        "char_count": char_count,
        "ocr_used": false,
        "has_timeline": has_timeline,
    })))
}

/// `GET /api/files/{id}/export?format=raw|tagged`
///
/// 与桌面命令 `export_file_text` 同义：返回**全文**（不截断），由前端
/// `exportFile()` 落盘或直接下载。
#[derive(Deserialize)]
pub struct ExportQuery {
    /// `raw`（默认）或 `tagged`
    pub format: Option<String>,
}

async fn file_export_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let format = q.format.unwrap_or_else(|| crate::transcript::FORMAT_RAW.to_string());
    let exported = files::export_file_text_impl(&app_state, &id, &format)
        .map_err(|error| ApiError { error })?;
    serde_json::to_value(exported)
        .map(Json)
        .map_err(|e| ApiError { error: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::{rel_within_root, resolve_tracked_file};

    /// 词法防线：拒绝 `..` 与绝对路径；正常相对路径放行。
    /// 真实逃逸（符号链接）由 `resolve_tracked_file` 的 canonicalize 兜住。
    #[test]
    fn tracked_path_must_stay_relative_and_parent_free() {
        for ok in ["a.pdf", "sub/a.pdf", "./a.pdf", "a/./b.pdf", "带空格 的文件.docx"] {
            assert!(rel_within_root(ok), "{ok:?} 应被接受");
        }
        for bad in [
            "",
            "   ",
            "..",
            "../a.pdf",
            "sub/../../a.pdf",
            "/etc/passwd",
            "/tmp/a.pdf",
        ] {
            assert!(!rel_within_root(bad), "{bad:?} 应被拒绝");
        }
        #[cfg(windows)]
        {
            assert!(
                !rel_within_root("C:\\Windows\\win.ini"),
                "Windows 盘符绝对路径必须拒绝"
            );
            assert!(
                !rel_within_root("sub\\..\\a.pdf"),
                "Windows 反斜杠 .. 必须拒绝"
            );
        }
    }

    /// 真实路径解析：目录内的文件可解析；不存在、`..` 越界都报错。
    /// 用 uuid 独占临时目录（避免与并行测试互相看见，见 AGENTS.md 测试门禁 ④′）。
    #[test]
    fn resolve_tracked_file_stays_inside_the_root() {
        let root = std::env::temp_dir().join(format!("ls_abs_path_{}", uuid::Uuid::new_v4()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).expect("建临时目录");
        let file = sub.join("a.txt");
        std::fs::write(&file, b"hi").expect("写临时文件");

        let root_s = root.to_str().expect("临时路径应为 UTF-8");
        let got = resolve_tracked_file(root_s, "sub/a.txt").expect("目录内文件应可解析");
        assert!(got.ends_with("a.txt"), "解析结果：{got:?}");

        assert!(resolve_tracked_file(root_s, "sub/missing.txt").is_err(), "不存在要报错");
        assert!(resolve_tracked_file(root_s, "../a.txt").is_err(), "越界要报错");

        let _ = std::fs::remove_dir_all(&root);
    }
}