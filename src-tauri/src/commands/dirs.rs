use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::db;
use crate::state::AppState;

#[derive(Serialize)]
pub struct DirConfigResponse {
    pub id: String,
    pub path: String,
    pub alias: Option<String>,
    pub ocr_lang: String,
    pub exclude_patterns: Option<String>,
    pub include_exts: Option<String>,
    pub recursive: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct DirConfigWithStats {
    pub id: String,
    pub path: String,
    pub alias: Option<String>,
    pub ocr_lang: String,
    pub exclude_patterns: Option<String>,
    pub include_exts: Option<String>,
    pub recursive: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub total_files: u64,
    pub indexed_files: u64,
}

#[tauri::command]
pub async fn add_dir(
    state: State<'_, AppState>,
    path: String,
    alias: Option<String>,
    recursive: Option<bool>,
) -> Result<DirConfigResponse, String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    if !p.is_dir() {
        return Err(format!("path is not a directory: {path}"));
    }

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let existing_dirs = db::dir_config::list_dirs(&conn).map_err(|e| format!("{e}"))?;
    let canonical = std::path::Path::new(&path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&path));
    // Sub-directories already indexed and now contained by the new parent.
    let mut contains: Vec<db::dir_config::DirConfig> = Vec::new();
    for dir in &existing_dirs {
        let existing_path = std::path::Path::new(&dir.path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&dir.path));
        if existing_path.starts_with(&canonical) {
            contains.push(dir.clone());
        }
    }

    // Reject directories that overlap the data dir (contains it or is inside it).
    if crate::commands::helpers::check_data_dir_overlap(&state.data_dir, &canonical).is_err() {
        return Err("此目录与数据目录存在交叠，不允许监控".to_string());
    }

    let ocr_lang = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key='ocr_lang'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "eng".to_string());

    let dir = db::dir_config::add_dir(
        &conn,
        &path,
        alias.as_deref(),
        Some(&ocr_lang),
        None,
        None,
        recursive.unwrap_or(true),
    )
    .map_err(|e| format!("failed to add directory: {e}"))?;

    // Absorb contained sub-directories: re-root their file records under the
    // new parent (path gets a relative sub-dir prefix, dir_id switches to the
    // parent). Their Tantivy documents are deleted; re-scanning the parent
    // re-adds them, reusing extracted content via MD5 dedup.
    for sub in &contains {
        let rel = std::path::Path::new(&sub.path)
            .strip_prefix(&canonical)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        db::tracker::absorb_subdir(&conn, &sub.id, &dir.id, &rel)
            .map_err(|e| format!("failed to absorb '{}': {e}", sub.path))?;
        let _ = db::dir_config::remove_dir(&conn, &sub.id);
        let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StopWatch {
            dir_id: sub.id.clone(),
        });
        let _ = state.indexer.delete_dir(&sub.id);
    }
    if !contains.is_empty() {
        let _ = state.indexer.commit();
        log::info!("[DIRS] 吸收 {} 个子目录到 {}", contains.len(), path);
    }

    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StartWatch {
        dir_id: dir.id.clone(),
        path: std::path::PathBuf::from(&dir.path),
    });

    // Absorbed sub-directories need their Tantivy documents rebuilt with the
    // new paths. Trigger a background full scan of the parent so the change
    // takes effect without a manual scan (content is reused via MD5 dedup).
    if !contains.is_empty() {
        let scanner = state.scanner.clone();
        let dir_id = dir.id.clone();
        let is_scanning = state.is_scanning.clone();
        let cancel_scan = state.cancel_scan.clone();
        let logs_dir = state.data_dir.join("logs");
        tokio::task::spawn_blocking(move || {
            if is_scanning
                .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
                .is_err()
            {
                log::warn!("[DIRS] 扫描已在运行，跳过吸收后自动全量扫描");
                return;
            }
            cancel_scan.store(false, std::sync::atomic::Ordering::Release);
            let mut slog = crate::logs::session::SessionLog::open(&logs_dir, "scan")
                .map_err(|e| log::warn!("[DIRS] 无法创建会话日志: {e}"))
                .ok();
            let mut sess = |line: String| {
                if let Some(ref mut f) = slog {
                    let _ = crate::logs::session::SessionLog::write(f, &line);
                }
            };
            sess("[DIRS] 吸收子目录后自动全量扫描".to_string());
            let line = match scanner.full_scan(&dir_id, |_| {}) {
                Ok(r) => format!(
                    "[DIRS] 吸收后扫描完成: {} files, {} indexed, {} errors",
                    r.total_files, r.indexed, r.errors
                ),
                Err(e) => format!("[DIRS] 吸收后扫描失败: {e}"),
            };
            log::info!("{line}");
            sess(line);
            is_scanning.store(false, std::sync::atomic::Ordering::SeqCst);
            drop(sess);
            if let Some(f) = slog {
                crate::logs::session::SessionLog::close(f);
            }
        });
    }

    Ok(DirConfigResponse {
        id: dir.id,
        path: dir.path,
        alias: dir.alias,
        ocr_lang: dir.ocr_lang,
        exclude_patterns: dir.exclude_patterns,
        include_exts: dir.include_exts,
        recursive: dir.recursive,
        created_at: dir.created_at,
        updated_at: dir.updated_at,
    })
}

#[tauri::command]
pub async fn remove_dir(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    db::dir_config::remove_dir(&conn, &id).map_err(|e| format!("failed to remove directory: {e}"))?;

    // Clean up the directory's files from Tantivy index, file_tracking, and content_index.
    let files = db::tracker::get_files_by_dir(&conn, &id)
        .map_err(|e| format!("failed to list dir files: {e}"))?;
    for file in &files {
        let _ = state.indexer.delete_file(&file.id);
    }
    conn.execute("DELETE FROM file_tracking WHERE dir_id = ?1", rusqlite::params![id])
        .map_err(|e| format!("failed to delete file records: {e}"))?;
    let _ = db::cleanup_orphan_content(&conn);

    drop(conn);

    let _ = state.indexer.commit();

    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StopWatch {
        dir_id: id.clone(),
    });

    Ok(())
}

#[tauri::command]
pub async fn list_dirs(state: State<'_, AppState>) -> Result<Vec<DirConfigWithStats>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let dirs = db::dir_config::list_dirs(&conn).map_err(|e| format!("failed to list dirs: {e}"))?;

    let mut result = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let stats = db::tracker::get_stats(&conn, Some(&dir.id)).unwrap_or(db::tracker::IndexStats { total: 0, indexed: 0, pending: 0, errors: 0 });
        result.push(DirConfigWithStats {
            id: dir.id,
            path: dir.path,
            alias: dir.alias,
            ocr_lang: dir.ocr_lang,
            exclude_patterns: dir.exclude_patterns,
            include_exts: dir.include_exts,
            recursive: dir.recursive,
            created_at: dir.created_at,
            updated_at: dir.updated_at,
            total_files: stats.total,
            indexed_files: stats.indexed,
        });
    }
    Ok(result)
}

#[tauri::command]
pub async fn update_dir(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
    alias: Option<String>,
    ocr_lang: Option<String>,
    exclude_patterns: Option<String>,
    include_exts: Option<String>,
    recursive: Option<bool>,
) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let updates = db::dir_config::DirUpdate {
        alias,
        ocr_lang,
        exclude_patterns,
        include_exts,
        recursive,
    };
    db::dir_config::update_dir(&conn, &id, updates)
        .map_err(|e| format!("failed to update directory: {e}"))?;

    // Restart watcher to pick up new config (exclude/include patterns)
    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StopWatch {
        dir_id: id.clone(),
    });
    let dir = db::dir_config::get_dir(&conn, &id).map_err(|e| format!("failed to reload dir: {e}"))?
        .ok_or_else(|| format!("dir not found: {id}"))?;
    drop(conn);
    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StartWatch {
        dir_id: id.clone(),
        path: std::path::PathBuf::from(&dir.path),
    });

    // Config changed — trigger an incremental scan so exclude/include/ocr
    // changes take effect immediately instead of waiting for the next scan.
    // Same is_scanning guard as add_dir to avoid concurrent scans.
    let scanner = state.scanner.clone();
    let is_scanning = state.is_scanning.clone();
    let cancel_scan = state.cancel_scan.clone();
    let app_clone = app.clone();
    tokio::task::spawn_blocking(move || {
        if is_scanning
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            log::warn!("[DIRS] 扫描已在运行，跳过更新目录后的自动增量扫描");
            return;
        }
        cancel_scan.store(false, Ordering::Release);
        match scanner.incremental_scan(&id, |_| {}) {
            Ok(r) => {
                log::info!(
                    "[DIRS] 更新目录后增量扫描完成: {} files, {} indexed, {} errors",
                    r.total_files, r.indexed, r.errors
                );
                is_scanning.store(false, Ordering::SeqCst);
                let _ = app_clone.emit("scan-completed", serde_json::json!({}));
            }
            Err(e) => {
                log::error!("[DIRS] 更新目录后增量扫描失败: {e}");
                is_scanning.store(false, Ordering::SeqCst);
            }
        }
    });

    Ok(())
}

#[derive(Serialize, Clone)]
pub struct DirTreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub children: Vec<DirTreeNode>,
    pub indexed: Option<bool>,
    pub status: Option<String>,
}

#[tauri::command]
pub fn get_dir_tree(state: State<'_, AppState>, dir_id: String, include_files: Option<bool>) -> Result<DirTreeNode, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let dir = db::dir_config::get_dir(&conn, &dir_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "dir not found".to_string())?;
    drop(conn);
    let mut budget = TREE_NODE_BUDGET;
    build_dir_tree(&dir.path, include_files.unwrap_or(false), &mut budget)
}

/// 懒加载：返回 `parent_path` 目录的单层子项（文件+目录，隐藏已过滤）。
/// 同时返回每文件的 indexed 状态（从 file_tracking 表查询）。
#[tauri::command]
pub fn get_dir_children(state: State<'_, AppState>, parent_path: String) -> Result<Vec<DirTreeNode>, String> {
    let mut children = Vec::new();

    let indexed_set: Result<std::collections::HashSet<String>, String> = (|| {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let mut stmt = conn.prepare(
            "SELECT path FROM file_tracking WHERE status='active' AND indexed=1"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            if let Ok(path) = row { set.insert(path); }
        }
        Ok(set)
    })();

    if let Ok(entries) = std::fs::read_dir(&parent_path) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') { continue; }
                let path = entry.path().to_string_lossy().to_string();
                let is_file = !ft.is_dir();
                let (_, status) = if is_file {
                    if let Ok(set) = &indexed_set {
                        if set.contains(&path) { (true, Some("indexed".to_string())) }
                        else { (false, Some("unindexed".to_string())) }
                    } else { (false, None) }
                } else { (false, None) };

                children.push(DirTreeNode {
                    name,
                    path: path.clone(),
                    is_dir: ft.is_dir(),
                    children: vec![],
                    indexed: Some(is_file && matches!(indexed_set, Ok(ref s) if s.contains(&path))),
                    status,
                });
            }
        }
    }
    children.sort_by(|a, b| {
        use std::cmp::Ordering;
        if a.is_dir != b.is_dir {
            return if a.is_dir { Ordering::Less } else { Ordering::Greater };
        }
        a.name.cmp(&b.name)
    });
    Ok(children)
}

/// 树节点总数预算：超限即停止收拢（防超大目录返回上万节点卡爆前端）。
const TREE_NODE_BUDGET: usize = 2000;

fn build_dir_tree(root_path: &str, include_files: bool, budget: &mut usize) -> Result<DirTreeNode, String> {
    let mut children = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root_path) {
        for entry in entries.flatten() {
            if *budget == 0 {
                break;
            }
            if let Ok(ft) = entry.file_type() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue; // 隐藏文件/目录（.DS_Store 等）
                }
                let path = entry.path().to_string_lossy().to_string();
                if ft.is_dir() {
                    let sub = build_dir_tree(&path, include_files, budget)?;
                    if *budget > 0 {
                        *budget -= 1;
                        children.push(DirTreeNode { name, path, is_dir: true, children: sub.children, indexed: None, status: None });
                    }
                } else if include_files && ft.is_file() {
                    *budget -= 1;
                    children.push(DirTreeNode { name, path, is_dir: false, children: vec![], indexed: None, status: None });
                }
            }
        }
    }
    children.sort_by(|a, b| a.name.cmp(&b.name));
    let name = std::path::Path::new(root_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(DirTreeNode { name, path: root_path.to_string(), is_dir: true, children, indexed: None, status: None })
}