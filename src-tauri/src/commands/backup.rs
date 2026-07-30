use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Serialize)]
pub struct BackupInfo {
    pub last_backup: Option<i64>,
    pub backup_size: u64,
    pub backup_count: u64,
}

#[tauri::command]
pub async fn trigger_backup(state: State<'_, AppState>) -> Result<(), String> {
    let backup_dir = state.data_dir.join("backups");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("time error: {e}"))?
        .as_secs();
    let backup_name = format!("backup_{timestamp}");
    let dest = backup_dir.join(&backup_name);

    std::fs::create_dir_all(&dest).map_err(|e| format!("failed to create backup dir: {e}"))?;

    // Backup Tantivy index
    let index_dest = dest.join("index");
    copy_dir(&state.index_dir, &index_dest)?;

    // Backup SQLite database
    let db_dest = dest.join("data.db");
    std::fs::copy(&state.db_path, &db_dest).map_err(|e| format!("failed to backup db: {e}"))?;

    // Cleanup old backups: keep only the 10 most recent
    cleanup_old_backups(&backup_dir, 10);

    log::info!("backup completed: {backup_name}");
    Ok(())
}

#[tauri::command]
pub async fn get_backup_status(state: State<'_, AppState>) -> Result<BackupInfo, String> {
    let backup_dir = state.data_dir.join("backups");
    if !backup_dir.is_dir() {
        return Ok(BackupInfo {
            last_backup: None,
            backup_size: 0,
            backup_count: 0,
        });
    }

    let mut entries: Vec<_> = std::fs::read_dir(&backup_dir)
        .map_err(|e| format!("failed to read backup dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();

    entries.sort_by_key(|e| e.path());

    let count = entries.len() as u64;
    let last_backup = entries.last().and_then(|e| {
        e.metadata().ok().and_then(|m| m.created().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    });

    let backup_size = entries
        .iter()
        .filter_map(|e| dir_size(&e.path()).ok())
        .sum();

    Ok(BackupInfo {
        last_backup,
        backup_size,
        backup_count: count,
    })
}

#[tauri::command]
pub async fn restore_backup(state: State<'_, AppState>, backup_name: String) -> Result<(), String> {
    let backup_dir = state.data_dir.join("backups").join(&backup_name);
    if !backup_dir.is_dir() {
        return Err(format!("backup not found: {backup_name}"));
    }

    // Restore index
    let index_src = backup_dir.join("index");
    if index_src.is_dir() {
        let _ = std::fs::remove_dir_all(&state.index_dir);
        copy_dir(&index_src, &state.index_dir)?;
    }

    // Restore database
    let db_src = backup_dir.join("data.db");
    if db_src.is_file() {
        std::fs::copy(&db_src, &state.db_path)
            .map_err(|e| format!("failed to restore db: {e}"))?;
    }

    log::info!("restored backup: {backup_name}");
    Ok(())
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("failed to create {dst:?}: {e}"))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("failed to read {src:?}: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry error: {e}"))?;
        let ty = entry.file_type().map_err(|e| format!("file type error: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("failed to copy {src_path:?}: {e}"))?;
        }
    }
    Ok(())
}

fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                total += dir_size(&entry.path())?;
            } else {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

fn cleanup_old_backups(backup_dir: &std::path::Path, keep: usize) {
    let mut entries: Vec<_> = match std::fs::read_dir(backup_dir) {
        Ok(e) => e.filter_map(|e| e.ok()).filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.path());
    while entries.len() > keep {
        if let Some(old) = entries.first() {
            let _ = std::fs::remove_dir_all(old.path());
            entries.remove(0);
        }
    }
}