use std::io::Write;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::State;

use crate::db;
use crate::db::tracker::IndexedState;
use crate::scanner::helpers::TempDir;
use crate::state::AppState;

#[derive(Serialize)]
pub struct FileItem {
    pub file_id: String,
    pub file_name: String,
    pub rel_path: String,
    pub file_ext: String,
    pub indexed: i64,
    pub error_msg: Option<String>,
    pub file_size: u64,
    pub mtime: i64,
}

#[derive(Serialize)]
pub struct FileListResponse {
    pub items: Vec<FileItem>,
    pub total: u64,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Serialize)]
pub struct FileDetail {
    pub id: String,
    pub path: String,
    pub dir_id: String,
    pub file_name: String,
    pub file_ext: String,
    pub file_size: u64,
    pub mtime: i64,
    pub size: u64,
    pub md5: Option<String>,
    pub status: String,
    pub indexed: bool,
    pub error_msg: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct FileListResponseAll {
    pub files: Vec<FileDetail>,
    pub total: u64,
}

#[derive(Serialize)]
pub struct DuplicateGroup {
    pub md5: String,
    pub count: u64,
    pub paths: Vec<String>,
    pub file_ids: Vec<String>,
}

#[tauri::command]
pub async fn list_files_db(
    state: State<'_, AppState>,
    filter: Option<String>,
    ext: Option<String>,
    search: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileListResponse, String> {
    if state.is_rebuilding.load(Ordering::SeqCst) {
        return Err("索引重建中，请稍后再试".to_string());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let ps = page_size.unwrap_or(50).clamp(1, 1000);
    let p = page.unwrap_or(1).max(1);
    let offset = (p - 1) * ps;

    let mut wheres: Vec<&str> = vec!["status = 'active'"];
    let mut params: Vec<Box<dyn rusqlite::ToSql + Send>> = Vec::new();

    match filter.as_deref() {
        Some("indexed") => { wheres.push("indexed = 1"); }
        Some("pending") => { wheres.push("indexed IN (0, 3)"); }
        Some("failed") => { wheres.push("indexed = 2"); }
        _ => {}
    }

    if let Some(e) = &ext {
        wheres.push("path LIKE ?");
        params.push(Box::new(format!("%.{e}")));
    }
    if let Some(s) = &search {
        wheres.push("path LIKE ?");
        params.push(Box::new(format!("%{s}%")));
    }

    let where_clause = wheres.join(" AND ");

    let count_sql = format!("SELECT COUNT(*) FROM file_tracking WHERE {where_clause}");
    let total: u64 = conn
        .query_row(&count_sql, rusqlite::params_from_iter(params.iter().map(|p| p as &dyn rusqlite::ToSql)), |row| row.get(0))
        .map_err(|e| format!("count query error: {e}"))?;

    let sort_col = match sort.as_deref().unwrap_or("path") {
        "name" => "path".to_string(),
        "path" => "path".to_string(),
        "ext" => "file_ext".to_string(),
        "size" => "size".to_string(),
        "mtime" => "mtime".to_string(),
        _ => "path".to_string(),
    };
    let order_dir = if order.as_deref() == Some("desc") { "DESC" } else { "ASC" };

    // Build data SQL with named params to avoid positional conflicts
    let data_sql = format!(
        "SELECT id, path, size, mtime, indexed, error_msg \
         FROM file_tracking WHERE {where_clause} \
         ORDER BY {sort_col} {order_dir} \
         LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2,
    );
    let mut data_params: Vec<Box<dyn rusqlite::ToSql + Send>> = params;
    data_params.push(Box::new(ps as i64));
    data_params.push(Box::new(offset as i64));

    let mut stmt = conn
        .prepare(&data_sql)
        .map_err(|e| format!("prepare error: {e}"))?;

    let rows = stmt
        .query_map(rusqlite::params_from_iter(data_params.iter().map(|p| p as &dyn rusqlite::ToSql)), |row| Ok(FileItem {
            file_id: row.get("id")?,
            file_name: std::path::Path::new(&row.get::<_, String>("path")?)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            rel_path: row.get("path")?,
            file_ext: std::path::Path::new(&row.get::<_, String>("path")?)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_string(),
            indexed: row.get("indexed")?,
            error_msg: row.get("error_msg")?,
            file_size: row.get::<_, i64>("size")? as u64,
            mtime: row.get("mtime")?,
        }))
        .map_err(|e| format!("query error: {e}"))?;

    let items: Vec<FileItem> = rows.collect::<rusqlite::Result<_>>()
        .map_err(|e| format!("collect error: {e}"))?;

    Ok(FileListResponse { items, total, page: p, page_size: ps })
}

#[tauri::command]
pub async fn get_file(state: State<'_, AppState>, id: String) -> Result<FileDetail, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let file = db::tracker::get_file_by_id(&conn, &id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))?;
    let file_name = std::path::Path::new(&file.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let file_ext = std::path::Path::new(&file.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    Ok(FileDetail {
        indexed: file.indexed == IndexedState::Indexed as i64,
        id: file.id,
        path: file.path.clone(),
        dir_id: file.dir_id,
        file_name,
        file_ext,
        file_size: file.size,
        mtime: file.mtime,
        size: file.size,
        md5: file.md5,
        status: file.status,
        error_msg: file.error_msg,
        created_at: file.created_at,
        updated_at: file.updated_at,
    })
}

#[tauri::command]
pub async fn list_files(
    state: State<'_, AppState>,
    dir_id: Option<String>,
    _status: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileListResponseAll, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let (files, total) = if let Some(did) = &dir_id {
        let all = db::tracker::get_files_by_dir(&conn, did)
            .map_err(|e| format!("query error: {e}"))?;
        let total = all.len() as u64;
        let page = page.unwrap_or(1).max(1);
        let ps = page_size.unwrap_or(50);
        let start = (page - 1) * ps;
        let slice: Vec<_> = all.into_iter().skip(start).take(ps).collect();
        (slice, total)
    } else {
        (Vec::new(), 0)
    };

    Ok(FileListResponseAll {
        total,
        files: files
            .into_iter()
            .map(|f| {
                let p = std::path::Path::new(&f.path);
                let file_name = p.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
                let file_ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
                FileDetail {
                    indexed: f.indexed == IndexedState::Indexed as i64,
                    id: f.id,
                    path: f.path,
                    dir_id: f.dir_id,
                    file_name,
                    file_ext,
                    file_size: f.size,
                    mtime: f.mtime,
                    size: f.size,
                    md5: f.md5,
                    status: f.status,
                    error_msg: f.error_msg,
                    created_at: f.created_at,
                    updated_at: f.updated_at,
                }
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn get_duplicates(state: State<'_, AppState>) -> Result<Vec<DuplicateGroup>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let groups = db::tracker::get_duplicates(&conn).map_err(|e| format!("query error: {e}"))?;
    Ok(groups
        .into_iter()
        .map(|g| DuplicateGroup {
            md5: g.md5,
            count: g.count,
            paths: g.paths,
            file_ids: g.file_ids,
        })
        .collect())
}

#[tauri::command]
pub async fn preview_file(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let file = db::tracker::get_file_by_id(&conn, &id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))?;

    let md5 = file.md5.ok_or_else(|| "file has no content indexed".to_string())?;
    db::tracker::get_content(&conn, &md5)
        .map_err(|e| format!("content query error: {e}"))?
        .ok_or_else(|| "content not found".to_string())
}

#[tauri::command]
pub async fn open_file(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let file = db::tracker::get_file_by_id(&conn, &id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))?;
    let dir = db::dir_config::get_dir(&conn, &file.dir_id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("dir config not found: {}", file.dir_id))?;
    drop(conn);

    let abs = std::path::Path::new(&dir.path).join(&file.path);
    opener::open(&abs).map_err(|e| format!("failed to open file: {e}"))
}

#[tauri::command]
pub async fn download_files(state: State<'_, AppState>, ids: Vec<String>) -> Result<String, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let tmp_dir = TempDir::new("ls_download").map_err(|e| format!("failed to create temp dir: {e}"))?;
    let zip_path = tmp_dir.path().join("download.zip");
    let file = std::fs::File::create(&zip_path).map_err(|e| format!("failed to create zip: {e}"))?;
    let mut zip_writer = zip::ZipWriter::new(file);
    let options = zip::write::FileOptions::<()>::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for id in &ids {
        let file_record = db::tracker::get_file_by_id(&conn, id)
            .map_err(|e| format!("query error: {e}"))?
            .ok_or_else(|| format!("file not found: {id}"))?;

        // 解析绝对路径：目录根路径 + 相对路径，防止路径穿越
        let dir = db::dir_config::get_dir(&conn, &file_record.dir_id)
            .map_err(|e| format!("query error: {e}"))?
            .ok_or_else(|| format!("dir config not found: {}", file_record.dir_id))?;
        let abs = std::path::Path::new(&dir.path).join(&file_record.path);
        let name = abs.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
        // P1-2: reject files >500MB before reading into memory
        let file_size = match abs.metadata() {
            Ok(m) => m.len(),
            Err(e) => return Err(format!("无法读取文件信息: {e}")),
        };
        if file_size > 500 * 1024 * 1024 {
            return Err(format!("文件过大，无法下载: {}", file_record.path));
        }
        let data = std::fs::read(&abs).map_err(|e| format!("failed to read {}: {e}", file_record.path))?;

        zip_writer
            .start_file(name, options)
            .map_err(|e| format!("zip error: {e}"))?;
        zip_writer
            .write_all(&data)
            .map_err(|e| format!("zip write error: {e}"))?;
    }

    zip_writer
        .finish()
        .map_err(|e| format!("zip finish error: {e}"))?;

    let path_str = zip_path.to_string_lossy().to_string();
    Ok(path_str)
}

#[derive(Serialize)]
pub struct FilePreview {
    pub content: Option<String>,
    pub image_path: Option<String>,
    pub image_base64: Option<String>,
    pub file_type: String,
    pub char_count: usize,
    pub ocr_used: bool,
}

#[tauri::command]
pub async fn get_file_preview(state: State<'_, AppState>, id: String) -> Result<FilePreview, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let file = db::tracker::get_file_by_id(&conn, &id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))?;

    let ext = std::path::Path::new(&file.path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let file_type = crate::extractor::classify_ext(&ext).to_string();
    let (image_path, image_base64) = if file_type == "image" {
        let dir = db::dir_config::get_dir(&conn, &file.dir_id)
            .unwrap_or(None)
            .map(|d| d.path)
            .unwrap_or_default();
        let abs = std::path::Path::new(&dir).join(&file.path).to_string_lossy().into_owned();
        let b64 = std::fs::read(&abs).ok().map(|bytes| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        });
        (Some(abs), b64)
    } else {
        (None, None)
    };

    let (content, char_count, ocr_used) = if let Some(md5) = &file.md5 {
        if let Ok(Some(c)) = db::tracker::get_content(&conn, md5) {
            let cc = c.chars().count();
            let ocr = db::tracker::get_content_ocr_used(&conn, md5).unwrap_or(false);
            (Some(c), cc, ocr)
        } else {
            (None, 0, false)
        }
    } else {
        (None, 0, false)
    };

    Ok(FilePreview { content, image_path, image_base64, file_type, char_count, ocr_used })
}

#[tauri::command]
pub async fn reveal_in_folder(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let file = db::tracker::get_file_by_id(&conn, &id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))?;
    let dir = db::dir_config::get_dir(&conn, &file.dir_id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("dir config not found: {}", file.dir_id))?;
    drop(conn);

    let abs = std::path::Path::new(&dir.path).join(&file.path);
    reveal_in_file_manager(&abs).map_err(|e| format!("failed to reveal: {e}"))
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.to_string_lossy()))
        .spawn()
        .map(|_| ())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(path);
    opener::open(parent).map_err(|e| std::io::Error::other(e.to_string()))
}

#[derive(Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_supported: bool,
    pub file_size: u64,
    pub mtime: i64,
    pub indexed: i64,
    pub error_msg: Option<String>,
}

/// List files and directories at a given filesystem path.
#[tauri::command]
pub async fn list_dir_entries(state: State<'_, AppState>, path: String) -> Result<Vec<DirEntry>, String> {
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err("not a directory".to_string());
    }

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let dir_configs = db::dir_config::list_dirs(&conn)
        .map_err(|e| format!("failed to load dir config: {e}"))?;

    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(dir).map_err(|e| format!("failed to read dir: {e}"))?;

    for entry in read_dir.flatten() {
        let entry_path_raw = entry.path();
        if crate::scanner::helpers::is_excluded(&entry_path_raw, &[]) {
            continue;
        }

        let ft = entry.file_type().ok();
        let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_path = entry_path_raw.to_string_lossy().to_string();

        let meta = entry.metadata().ok();
        let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let is_supported = is_dir || crate::extractor::is_supported(&entry_path_raw);

        let mut indexed = 0;
        let mut error_msg: Option<String> = None;

        if !is_dir {
            for config in &dir_configs {
                if let Ok(rel_path) = crate::scanner::helpers::to_relative(&config.path, &entry_path_raw) {
                    match db::tracker::get_file_by_path(&conn, &rel_path) {
                        Ok(Some(file)) if file.status == "deleted" => {
                            continue;
                        }
                        Ok(Some(file)) => {
                            indexed = file.indexed;
                            error_msg = file.error_msg;
                        }
                        _ => {
                            indexed = 0;
                        }
                    }
                    break;
                }
            }
        }

        entries.push(DirEntry {
            name,
            path: entry_path,
            is_dir,
            is_supported,
            file_size,
            mtime,
            indexed,
            error_msg,
        });
    }

    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            b.is_dir.cmp(&a.is_dir)
        } else {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        }
    });

    Ok(entries)
}

/// Preview a file by its filesystem path (looks up DB if indexed, falls back to direct read).
#[tauri::command]
pub async fn preview_file_by_path(state: State<'_, AppState>, path: String) -> Result<FilePreview, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    if let Ok(Some(file)) = db::tracker::get_file_by_path(&conn, &path) {
        drop(conn);

        return get_file_preview_inner(&state, &file).await;
    }

    // 防止路径穿越：校验路径在已监控目录内（conn 还在作用域内）
    let p = std::path::Path::new(&path);
    if !p.exists() {
        drop(conn);
        return Err("file not found".to_string());
    }
    let dirs = db::dir_config::list_dirs(&conn).map_err(|e| format!("db error: {e}"))?;
    let abs = p.canonicalize().map_err(|_| "invalid path".to_string())?;
    let allowed = dirs.iter().any(|d| {
        std::path::Path::new(&d.path).canonicalize().ok()
            .map(|root| abs.starts_with(&root))
            .unwrap_or(false)
    });
    drop(conn);
    if !allowed {
        return Err("file not in monitored directories".to_string());
    }

    let ext = p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let file_type = crate::extractor::classify_ext(&ext).to_string();
    let image_path = if file_type == "image" { Some(path.clone()) } else { None };

    let content = if file_type == "text" {
        std::fs::read_to_string(&path).ok()
    } else {
        None
    };
    let char_count = content.as_ref().map(|c| c.chars().count()).unwrap_or(0);

    Ok(FilePreview {
        content,
        image_path,
        image_base64: None,
        file_type,
        char_count,
        ocr_used: false,
    })
}

/// Shared logic: build a FilePreview from a DB FileRecord.
async fn get_file_preview_inner(state: &State<'_, AppState>, file: &db::tracker::FileRecord) -> Result<FilePreview, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let ext = std::path::Path::new(&file.path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let file_type = crate::extractor::classify_ext(&ext).to_string();
    let (image_path, image_base64) = if file_type == "image" {
        let dir = db::dir_config::get_dir(&conn, &file.dir_id)
            .unwrap_or(None)
            .map(|d| d.path)
            .unwrap_or_default();
        let abs = std::path::Path::new(&dir).join(&file.path).to_string_lossy().into_owned();
        let b64 = std::fs::read(&abs).ok().map(|bytes| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        });
        (Some(abs), b64)
    } else {
        (None, None)
    };

    let (content, char_count, ocr_used) = if let Some(md5) = &file.md5 {
        if let Ok(Some(c)) = db::tracker::get_content(&conn, md5) {
            let cc = c.chars().count();
            let ocr = db::tracker::get_content_ocr_used(&conn, md5).unwrap_or(false);
            (Some(c), cc, ocr)
        } else {
            (None, 0, false)
        }
    } else {
        (None, 0, false)
    };

    Ok(FilePreview { content, image_path, image_base64, file_type, char_count, ocr_used })
}
