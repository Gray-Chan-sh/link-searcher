use serde::Serialize;
use tauri::State;

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

    // Check for overlapping directories
    let existing_dirs = db::dir_config::list_dirs(&conn).map_err(|e| format!("{e}"))?;
    let canonical = std::path::Path::new(&path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&path));
    for dir in &existing_dirs {
        let existing_path = std::path::Path::new(&dir.path)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(&dir.path));
        if canonical.starts_with(&existing_path) {
            return Err(format!("此目录是已索引目录 '{}' 的子目录，请索引上级目录", dir.path));
        }
        if existing_path.starts_with(&canonical) {
            return Err(format!("此目录包含已索引目录 '{}'，请直接索引该目录", dir.path));
        }
    }

    let dir = db::dir_config::add_dir(
        &conn,
        &path,
        alias.as_deref(),
        None,
        None,
        None,
        recursive.unwrap_or(true),
    )
    .map_err(|e| format!("failed to add directory: {e}"))?;

    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StartWatch {
        dir_id: dir.id.clone(),
        path: std::path::PathBuf::from(&dir.path),
    });

    // Trigger initial scan for the newly added directory
    let scanner = state.scanner.clone();
    let dir_id = dir.id.clone();
    tokio::task::spawn_blocking(move || {
        let _ = scanner.incremental_scan(&dir_id, |_| {});
    });

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
        dir_id: id,
        path: std::path::PathBuf::from(&dir.path),
    });

    Ok(())
}

#[derive(Serialize, Clone)]
pub struct DirTreeNode {
    pub name: String,
    pub path: String,
    pub children: Vec<DirTreeNode>,
}

#[tauri::command]
pub fn get_dir_tree(state: State<'_, AppState>, dir_id: String) -> Result<DirTreeNode, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let dir = db::dir_config::get_dir(&conn, &dir_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "dir not found".to_string())?;
    drop(conn);
    build_dir_tree(&dir.path)
}

fn build_dir_tree(root_path: &str) -> Result<DirTreeNode, String> {
    let mut children = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root_path) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let path = entry.path().to_string_lossy().to_string();
                    let sub = build_dir_tree(&path)?;
                    children.push(DirTreeNode { name, path, children: sub.children });
                }
            }
        }
    }
    children.sort_by(|a, b| a.name.cmp(&b.name));
    let name = std::path::Path::new(root_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(DirTreeNode { name, path: root_path.to_string(), children })
}