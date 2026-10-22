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
    /// `active` | `deleted`（Browse 的"已删除"视图据此显示与提供恢复）。
    pub status: String,
    pub quality_score: Option<f64>,
    pub quality_flags: String,
    pub char_count: i64,
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
    quality: Option<String>,
    max_score: Option<f64>,
) -> Result<FileListResponse, String> {
    if state.is_rebuilding.load(Ordering::SeqCst) {
        return Err("索引重建中，请稍后再试".to_string());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    query_file_list(
        &conn,
        filter.as_deref(),
        ext.as_deref(),
        search.as_deref(),
        sort.as_deref(),
        order.as_deref(),
        page.unwrap_or(1),
        page_size.unwrap_or(50),
        quality.as_deref(),
        max_score,
    )
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

    let abs = crate::scanner::helpers::to_absolute(&dir.path, &file.path);
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
        let abs = crate::scanner::helpers::to_absolute(&dir.path, &file_record.path);
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
    pub quality_score: Option<f64>,
    pub quality_flags: String,
    /// 音频是否带可用时间轴（决定 UI 里「带标签」导出是否可选）。
    pub has_timeline: bool,
}

/// 文件扩展名是否为音频（`classify_ext` 的 "audio" 分类）。
pub(crate) fn is_audio_path(path: &str) -> bool {
    file_type_of(path) == "audio"
}

/// 与 [`crate::extractor::classify_ext`] 同一套分类，直接接受路径。
pub(crate) fn file_type_of(path: &str) -> &'static str {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(crate::extractor::classify_ext)
        .unwrap_or("unknown")
}

/// 读取正文，并把音频的**带标签**版本渲染出来。
///
/// 返回 `(展示用正文, 纯正文字符数, 是否用了 OCR, 是否有时间轴)`。
/// 正文（`text_content`）本身始终是纯文本——标签只在这条输出路径上生成，
/// 因此 tantivy / RAG / 片段预览都不受影响。
pub(crate) fn preview_content(
    conn: &rusqlite::Connection,
    path: &str,
    md5: Option<&str>,
    limit: Option<usize>,
) -> (Option<String>, usize, bool, bool) {
    let Some(md5) = md5 else {
        return (None, 0, false, false);
    };
    let Ok(Some(raw)) = db::tracker::get_content(conn, md5) else {
        return (None, 0, false, false);
    };
    let ocr_used = db::tracker::get_content_ocr_used(conn, md5).unwrap_or(false);
    let audio = is_audio_path(path);
    let timeline = if audio { db::tracker::get_audio_timeline(conn, md5) } else { Vec::new() };
    let has_timeline = crate::transcript::has_timeline(&timeline);

    // 截断发生在**加标签之前**，且按"纯正文字数"计量：否则 5 万字的截断会
    // 吃掉几十个段头，甚至切在半个 `[00:12:` 上。
    let (raw, timeline) = match limit {
        Some(n) if has_timeline => truncate_timeline(&timeline, n),
        Some(n) => (raw.chars().take(n).collect::<String>(), timeline),
        None => (raw, timeline),
    };
    let char_count = raw.chars().count();

    if has_timeline {
        (Some(crate::transcript::render_tagged(&raw, &timeline)), char_count, ocr_used, true)
    } else {
        (Some(raw), char_count, ocr_used, false)
    }
}

/// 按**纯正文字数**截断时间轴，返回 `(截断后的正文, 截断后的时间轴)`。
///
/// 时间轴与正文共用同一份文本（逐段以 `\n` 相连），所以按段累加即可精确定位
/// 截断点；跨界的最后一段按字符裁齐，保证"正文长度"与"带标签输出里的正文"一致。
fn truncate_timeline(
    timeline: &[crate::transcript::AudioSegment],
    limit: usize,
) -> (String, Vec<crate::transcript::AudioSegment>) {
    let mut kept: Vec<crate::transcript::AudioSegment> = Vec::new();
    let mut used = 0usize;
    for seg in timeline {
        let body = seg.text.trim();
        if body.is_empty() {
            continue;
        }
        let len = body.chars().count();
        if used + len > limit {
            let room = limit.saturating_sub(used);
            if room > 0 {
                let mut cut = seg.clone();
                cut.text = body.chars().take(room).collect();
                kept.push(cut);
            }
            break;
        }
        used += len + 1; // +1 for the joining '\n'
        kept.push(seg.clone());
    }
    let text = kept.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
    (text, kept)
}

/// 导出结果（桌面与 Web 同一形状，前端只需读 `content`）。
#[derive(Serialize)]
pub struct ExportedText {
    pub content: String,
    pub char_count: usize,
    pub format: String,
}

/// 单文件导出的实际实现（桌面命令与 Web 路由共用）。
///
/// * `raw`    —— 纯正文（与 `content_index.text_content` 逐字节相同）
/// * `tagged` —— 带 `[时间 · 说话人]` 标签的可读稿（仅对**有时间轴的音频**有效）
///
/// 不落盘：由前端 `exportFile()` 负责弹保存框/下载。
pub(crate) fn export_file_text_impl(
    state: &AppState,
    id: &str,
    format: &str,
) -> Result<ExportedText, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let file = db::tracker::get_file_by_id(&conn, id)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| format!("file not found: {id}"))?;
    let md5 = file.md5.as_deref().ok_or_else(|| "文件尚未索引".to_string())?;
    let raw = db::tracker::get_content(&conn, md5)
        .map_err(|e| format!("query error: {e}"))?
        .ok_or_else(|| "文件尚未索引".to_string())?;

    let content = match format {
        crate::transcript::FORMAT_RAW => raw,
        crate::transcript::FORMAT_TAGGED => {
            let timeline = db::tracker::get_audio_timeline(&conn, md5);
            if !crate::transcript::has_timeline(&timeline) {
                return Err("该文件没有时间轴（仅分段识别/说话人分离完成的音频可导出带标签版本）".into());
            }
            crate::transcript::render_tagged(&raw, &timeline)
        }
        other => return Err(format!("不支持的导出格式: {other}")),
    };
    Ok(ExportedText { char_count: content.chars().count(), content, format: format.to_string() })
}

#[tauri::command]
pub async fn export_file_text(
    state: State<'_, AppState>,
    id: String,
    format: String,
) -> Result<ExportedText, String> {
    export_file_text_impl(&state, &id, &format)
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
        let abs = crate::scanner::helpers::to_absolute(&dir, &file.path).to_string_lossy().into_owned();
        let b64 = std::fs::read(&abs).ok().map(|bytes| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        });
        (Some(abs), b64)
    } else {
        (None, None)
    };

    let (content, char_count, ocr_used, has_timeline) =
        preview_content(&conn, &file.path, file.md5.as_deref(), None);

    let (quality_score, quality_flags) = if let Some(md5) = &file.md5 {
        conn.query_row(
            "SELECT quality_score, quality_flags FROM content_index WHERE md5 = ?1",
            rusqlite::params![md5],
            |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap_or((None, "[]".to_string()))
    } else {
        (None, "[]".to_string())
    };

    Ok(FilePreview {
        content,
        image_path,
        image_base64,
        file_type,
        char_count,
        ocr_used,
        quality_score,
        quality_flags,
        has_timeline,
    })
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

    let abs = crate::scanner::helpers::to_absolute(&dir.path, &file.path);
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

/// Build the single `explorer.exe` argument that reveals `path` via `/select,`.
///
/// Explorer 对 `/select,` 有两条硬性要求，违反任一条都会**静默回退**去打开
/// 「文档」/桌面（用户看到的正是这个现象）：
///   ① **只给路径加引号** —— `/select,"<path>"`。`Command::arg` 在参数含空格时
///      会把整串加引号（`"/select,<path>"`），explorer 便认不出 `/select,`；
///   ② 路径用**反斜杠** —— `file_tracking.path` / `dir_config.path` 经
///      [`crate::scanner::helpers::normalize_os_path`] 存的是正斜杠，`/select,` 不认。
/// 故这里手工拼串，交由 [`std::os::windows::process::CommandExt::raw_arg`] 原样传递，
/// 绕过 std 的自动引号。
#[cfg(target_os = "windows")]
fn reveal_arg(path: &std::path::Path) -> String {
    let win = path.to_string_lossy().replace('/', "\\");
    format!("/select,\"{win}\"")
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // process::new adds CREATE_NO_WINDOW — bare std spawn flashes a console.
    // raw_arg 原样写入命令行：只对**路径**加引号，分隔符改反斜杠（见 reveal_arg）。
    crate::process::new("explorer")
        .raw_arg(reveal_arg(path))
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
        quality_score: None,
        quality_flags: "[]".to_string(),
        has_timeline: false,
    })
}

/// Shared query logic extracted from `list_files_db` so tests can call it
/// with a bare `&Connection` without constructing Tauri `State`.
pub(crate) fn query_file_list(
    conn: &rusqlite::Connection,
    filter: Option<&str>,
    ext: Option<&str>,
    search: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
    page: usize,
    page_size: usize,
    quality: Option<&str>,
    quality_max_score: Option<f64>,
) -> Result<FileListResponse, String> {
    let ps = page_size.clamp(1, 1000);
    let p = page.max(1);
    let offset = (p - 1) * ps;

    // deleted 视图查看被扫描标记删除的记录（如目录瞬断误删），可据此恢复；
    // 其余筛选一律只看 active 文件。
    let mut wheres: Vec<String> = if filter == Some("deleted") {
        vec!["status = 'deleted'".into()]
    } else {
        vec!["status = 'active'".into()]
    };
    let mut params: Vec<Box<dyn rusqlite::ToSql + Send>> = Vec::new();

    match filter {
        Some("indexed") => { wheres.push("indexed = 1".into()); }
        Some("pending") => { wheres.push("indexed IN (0, 3)".into()); }
        Some("failed") => { wheres.push("indexed = 2".into()); }
        _ => {}
    }

    if let Some(e) = ext {
        wheres.push("path LIKE ?".into());
        params.push(Box::new(format!("%.{e}")));
    }
    if let Some(s) = search {
        wheres.push("path LIKE ?".into());
        params.push(Box::new(format!("%{s}%")));
    }

    let quality_join = match quality {
        Some("low") | Some("red") => {
            // 阈值可调：默认 0.5（旧行为），前端「质量」页可放到 0.8 / 1.01（不限）
            let max = quality_max_score.unwrap_or(0.5).clamp(1e-6, 1.01);
            wheres.push("ci.quality_score < ?".into());
            params.push(Box::new(max));
            "LEFT JOIN content_index ci ON ci.md5 = file_tracking.md5"
        }
        Some("yellow") => {
            wheres.push("ci.quality_score >= 0.5 AND ci.quality_score <= 0.75".into());
            "LEFT JOIN content_index ci ON ci.md5 = file_tracking.md5"
        }
        Some("green") => {
            wheres.push("ci.quality_score > 0.75".into());
            "LEFT JOIN content_index ci ON ci.md5 = file_tracking.md5"
        }
        Some("flagged") => {
            wheres.push("ci.quality_flags IS NOT NULL AND ci.quality_flags != '[]' AND file_tracking.size > 0".into());
            "LEFT JOIN content_index ci ON ci.md5 = file_tracking.md5"
        }
        _ => "LEFT JOIN content_index ci ON ci.md5 = file_tracking.md5",
    };

    let where_clause = wheres.join(" AND ");
    log::info!("[FILES] query_file_list filter={:?} where={where_clause}", filter);

    let count_sql = format!("SELECT COUNT(*) FROM file_tracking {quality_join} WHERE {where_clause}");
    let total: u64 = conn
        .query_row(&count_sql, rusqlite::params_from_iter(params.iter().map(|p| p as &dyn rusqlite::ToSql)), |row| row.get(0))
        .map_err(|e| format!("count query error: {e}"))?;

    let (sort_col, nulls_last) = match sort {
        Some("quality") => ("ci.quality_score ASC NULLS LAST".to_string(), true),
        Some("name") | Some("path") | None => ("file_tracking.path".to_string(), false),
        Some("ext") => ("file_tracking.file_ext".to_string(), false),
        Some("size") => ("file_tracking.size".to_string(), false),
        Some("mtime") => ("file_tracking.mtime".to_string(), false),
        _ => ("file_tracking.path".to_string(), false),
    };
    let order_dir = if order == Some("desc") && !nulls_last { "DESC" } else { "ASC" };
    let order_clause = if nulls_last {
        sort_col
    } else {
        format!("{sort_col} {order_dir}")
    };

    let data_sql = format!(
        "SELECT file_tracking.id, file_tracking.path, file_tracking.size, file_tracking.mtime, \
         file_tracking.indexed, file_tracking.error_msg, file_tracking.status, \
         ci.quality_score, ci.quality_flags, ci.char_count \
         FROM file_tracking {quality_join} WHERE {where_clause} \
         ORDER BY {order_clause} \
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
        .query_map(rusqlite::params_from_iter(data_params.iter().map(|p| p as &dyn rusqlite::ToSql)), |row| {
            let path: String = row.get("path")?;
            Ok(FileItem {
                file_id: row.get("id")?,
                file_name: std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                rel_path: path.clone(),
                file_ext: std::path::Path::new(&path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string(),
                indexed: row.get("indexed")?,
                error_msg: row.get("error_msg")?,
                file_size: row.get::<_, i64>("size")? as u64,
                mtime: row.get("mtime")?,
                status: row.get("status")?,
                quality_score: row.get("quality_score")?,
                quality_flags: row.get::<_, Option<String>>("quality_flags")?.unwrap_or_else(|| "[]".into()),
                char_count: row.get::<_, Option<i64>>("char_count")?.unwrap_or(0),
            })
        })
        .map_err(|e| format!("query error: {e}"))?;

    let items: Vec<FileItem> = rows.collect::<rusqlite::Result<_>>()
        .map_err(|e| format!("collect error: {e}"))?;

    Ok(FileListResponse { items, total, page: p, page_size: ps })
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
        let abs = crate::scanner::helpers::to_absolute(&dir, &file.path).to_string_lossy().into_owned();
        let b64 = std::fs::read(&abs).ok().map(|bytes| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        });
        (Some(abs), b64)
    } else {
        (None, None)
    };

    let (content, char_count, ocr_used, has_timeline) =
        preview_content(&conn, &file.path, file.md5.as_deref(), None);

    let (quality_score, quality_flags) = if let Some(md5) = &file.md5 {
        conn.query_row(
            "SELECT quality_score, quality_flags FROM content_index WHERE md5 = ?1",
            rusqlite::params![md5],
            |row| Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap_or((None, "[]".to_string()))
    } else {
        (None, "[]".to_string())
    };

    Ok(FilePreview {
        content,
        image_path,
        image_base64,
        file_type,
        char_count,
        ocr_used,
        quality_score,
        quality_flags,
        has_timeline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    fn insert_file_with_md5(conn: &rusqlite::Connection, path: &str, md5: &str) {
        db::tracker::upsert_file(conn, path, "d1", 1000, 10, Some(md5)).unwrap();
        db::tracker::update_indexed(conn, &db::tracker::get_file_by_path(conn, path).unwrap().unwrap().id, Some(md5)).unwrap();
    }

    /// `/select,` 的两条硬性要求：**只给路径加引号** + **反斜杠分隔**。
    /// 违反任一条都会让 explorer 静默回退打开「文档」目录（见 `reveal_arg` 文档）。
    #[cfg(target_os = "windows")]
    #[test]
    fn reveal_arg_quotes_only_path_and_uses_backslashes() {
        // DB 里存的是归一化正斜杠路径（normalize_os_path）；这里刻意带空格与中文。
        let arg = reveal_arg(std::path::Path::new("D:/Syncthing/工作日志/我的 文件.pdf"));
        assert_eq!(arg, "/select,\"D:\\Syncthing\\工作日志\\我的 文件.pdf\"");
        // `"/select,…"`（整串被引号包住）是会导致回退的错误形式，必须避免。
        assert_eq!(arg.matches('"').count(), 2, "只应给路径加一对引号: {arg}");
        assert!(!arg.starts_with('"'));
        assert!(arg.starts_with("/select,\""));
        assert!(arg.ends_with('"'));
        let path_part = arg.strip_prefix("/select,").unwrap();
        assert!(!path_part.contains('/'), "explorer /select 不认正斜杠: {arg}");
    }

    fn insert_content(conn: &rusqlite::Connection, md5: &str, score: Option<f64>, flags: &str) {
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT OR REPLACE INTO content_index (md5,text_content,indexed_at,char_count,ocr_used,ocr_duration_ms,quality_score,quality_flags,reextract_count) \
             VALUES (?1,'x',?2,0,0,NULL,?3,?4,0)",
            rusqlite::params![md5, now, score, flags],
        ).unwrap();
    }

    fn store_transcript(conn: &rusqlite::Connection, md5: &str, text: &str, timeline: &[crate::transcript::AudioSegment]) {
        db::tracker::store_content(conn, md5, text, false, None).unwrap();
        db::tracker::store_audio_timeline(conn, md5, timeline).unwrap();
    }

    fn seg(start_ms: i64, end_ms: i64, speaker: Option<i32>, text: &str) -> crate::transcript::AudioSegment {
        crate::transcript::AudioSegment { start_ms, end_ms, speaker, text: text.to_string() }
    }

    /// 音频预览/导出：正文纯文本 + 时间轴 → 输出带标签；纯正文与库里逐字节相同。
    #[test]
    fn audio_preview_and_export_split_plain_from_tagged() {
        let conn = setup();
        let text = "你好，能听到吗？\n听得到。";
        store_transcript(
            &conn,
            "md5_audio",
            text,
            &[seg(0, 7_000, Some(0), "你好，能听到吗？"), seg(7_000, 15_000, Some(1), "听得到。")],
        );

        let (preview, chars, _ocr, has_timeline) =
            preview_content(&conn, "dir/meeting.mp3", Some("md5_audio"), None);
        let preview = preview.expect("音频预览应有正文");
        assert!(has_timeline, "有时间轴时 has_timeline 必须为 true");
        assert_eq!(chars, text.chars().count(), "char_count 仍按纯正文计");
        assert_eq!(
            preview,
            "[00:00:00 · 说话人0] 你好，能听到吗？\n[00:00:07 · 说话人1] 听得到。"
        );
        // 库里正文保持纯净（标签只存在于输出层）
        assert_eq!(db::tracker::get_content(&conn, "md5_audio").unwrap().unwrap(), text);
    }

    #[test]
    fn audio_without_timeline_stays_plain_and_reports_no_timeline() {
        let conn = setup();
        db::tracker::store_content(&conn, "md5_old", "老音频只有正文", false, None).unwrap();
        let (preview, _, _, has_timeline) =
            preview_content(&conn, "dir/old.mp3", Some("md5_old"), None);
        assert_eq!(preview.unwrap(), "老音频只有正文");
        assert!(!has_timeline);
    }

    #[test]
    fn non_audio_preview_never_tags_even_with_timeline_rows() {
        let conn = setup();
        store_transcript(&conn, "md5_pdf", "合同正文", &[seg(0, 1_000, Some(0), "合同正文")]);
        let (preview, _, _, has_timeline) =
            preview_content(&conn, "dir/a.pdf", Some("md5_pdf"), None);
        assert_eq!(preview.unwrap(), "合同正文");
        assert!(!has_timeline, "非音频不该报时间轴");
    }

    #[test]
    fn preview_limit_is_applied_before_tagging() {
        let conn = setup();
        let text = "一".repeat(10) + "二";
        store_transcript(&conn, "md5_long", &text, &[seg(0, 1_000, None, &text)]);
        let (preview, chars, _, _) =
            preview_content(&conn, "dir/long.mp3", Some("md5_long"), Some(10));
        // 截断按纯正文算：10 个字 + 段头，不会把标签切一半
        assert_eq!(chars, 10);
        assert_eq!(preview.unwrap(), "[00:00:00] 一一一一一一一一一一");
    }

    #[test]
    fn file_type_of_distinguishes_audio() {
        assert_eq!(file_type_of("a/b/c.mp3"), "audio");
        assert_eq!(file_type_of("a/b/c.PDF"), "pdf");
        assert_eq!(file_type_of("a/b/c.docx"), "office");
        assert_eq!(file_type_of("no_ext"), "unknown");
    }

    fn seed_quality_data(conn: &rusqlite::Connection) {
        db::dir_config::add_dir(conn, "/tmp/test", None, None, None, None, true).unwrap();
        insert_file_with_md5(conn, "a.pdf", "md5_a");
        insert_file_with_md5(conn, "b.pdf", "md5_b");
        insert_file_with_md5(conn, "c.pdf", "md5_c");
        insert_file_with_md5(conn, "d.pdf", "md5_d");
        insert_content(conn, "md5_a", Some(0.2), r#"["short"]"#);
        insert_content(conn, "md5_b", Some(0.6), r#"["medium"]"#);
        insert_content(conn, "md5_c", Some(0.9), r#"["ok"]"#);
        insert_content(conn, "md5_d", None, "[]");
    }

    #[test]
    fn quality_low_honours_custom_max_score() {
        // 阈值可调：0.5 只出 0.2；放到 0.95 应把 0.2/0.6/0.9 都列出来
        // （修复"旋转扫描件乱码分在 0.65~0.79、被硬阈值 0.5 漏掉"的问题）。
        let conn = setup();
        seed_quality_data(&conn);
        let narrow =
            query_file_list(&conn, None, None, None, None, None, 1, 50, Some("low"), Some(0.5)).unwrap();
        assert_eq!(narrow.items.len(), 1);

        let wide =
            query_file_list(&conn, None, None, None, None, None, 1, 50, Some("low"), Some(0.95)).unwrap();
        assert_eq!(wide.items.len(), 3, "阈值 0.95 应列出 0.2/0.6/0.9");

        // 1.01 = 不限（只看有评分的；无评分的 d.pdf 仍排除）
        let all =
            query_file_list(&conn, None, None, None, None, None, 1, 50, Some("low"), Some(1.01)).unwrap();
        assert_eq!(all.items.len(), 3);
    }

    #[test]
    fn quality_low_returns_only_red() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, None, None, 1, 50, Some("low"), None).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].rel_path, "a.pdf");
    }

    #[test]
    fn quality_red_same_as_low() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, None, None, 1, 50, Some("red"), None).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].rel_path, "a.pdf");
    }

    #[test]
    fn quality_yellow_returns_mid_range() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, None, None, 1, 50, Some("yellow"), None).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].rel_path, "b.pdf");
    }

    #[test]
    fn quality_green_returns_high() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, None, None, 1, 50, Some("green"), None).unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].rel_path, "c.pdf");
    }

    #[test]
    fn quality_none_returns_all_rows() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, None, None, 1, 50, None, None).unwrap();
        assert_eq!(resp.items.len(), 4);
        assert_eq!(resp.total, 4);
    }

    #[test]
    fn sort_quality_ascending_nulls_last() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, Some("quality"), None, 1, 50, None, None).unwrap();
        let paths: Vec<&str> = resp.items.iter().map(|i| i.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["a.pdf", "b.pdf", "c.pdf", "d.pdf"]);
    }

    #[test]
    fn quality_filter_populates_score_and_flags() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, None, None, None, None, None, 1, 50, Some("green"), None).unwrap();
        let item = &resp.items[0];
        assert!((item.quality_score.unwrap() - 0.9).abs() < 0.01);
        assert_eq!(item.quality_flags, r#"["ok"]"#);
    }

    #[test]
    fn existing_filters_unaffected_by_quality_param() {
        let conn = setup();
        seed_quality_data(&conn);
        let resp = query_file_list(&conn, Some("indexed"), None, None, None, None, 1, 50, None, None).unwrap();
        assert_eq!(resp.items.len(), 4);
        let resp_ext = query_file_list(&conn, None, Some("pdf"), None, None, None, 1, 50, None, None).unwrap();
        assert_eq!(resp_ext.items.len(), 4);
    }
}
