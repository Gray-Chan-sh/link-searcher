use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::config::INDEX_DIR_NAME;
use crate::search::IndexManager;
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
    let index_dest = dest.join(INDEX_DIR_NAME);
    copy_dir(&state.index_dir, &index_dest)?;

    // Backup SQLite database（在线备份 API，保证 WAL 一致性，不直接 fs::copy 活跃 DB）
    let db_dest = dest.join("data.db");
    let src_conn = Connection::open(&state.db_path)
        .map_err(|e| format!("failed to open source db: {e}"))?;
    let mut dst_conn = Connection::open(&db_dest)
        .map_err(|e| format!("failed to open backup db: {e}"))?;
    let backup = Backup::new(&src_conn, &mut dst_conn)
        .map_err(|e| format!("failed to init backup: {e}"))?;
    let mut r = backup.step(-1).map_err(|e| format!("backup failed: {e}"))?;
    let mut busy = 0;
    while r == StepResult::Busy || r == StepResult::Locked {
        busy += 1;
        if busy >= 3 {
            return Err("数据库繁忙，备份未完成，请重试".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
        r = backup.step(-1).map_err(|e| format!("backup failed: {e}"))?;
    }
    if r != StepResult::Done {
        return Err("数据库繁忙，备份未完成，请重试".to_string());
    }

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
pub async fn restore_backup(
    state: State<'_, AppState>,
    app: AppHandle,
    backup_name: String,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    if state
        .is_restoring
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("正在恢复中，请稍候".to_string());
    }

    let backup_dir = state.data_dir.join("backups").join(&backup_name);
    let index_dir = state.index_dir.clone();
    let index_manager = state.index_manager.clone();
    let indexer = state.indexer.clone();
    let db_pool = state.db.clone();
    let is_restoring = state.is_restoring.clone();
    let backup_name_in = backup_name.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        if !backup_dir.is_dir() {
            return Err(format!("备份不存在: {backup_name_in}"));
        }
        // 1. 恢复索引：复制到临时目录 → 切换 IndexManager → 原子替换（同 rebuild_index 的 tmp→rename 模式）
        let index_src = backup_dir.join(INDEX_DIR_NAME);
        if index_src.is_dir() {
            let tmp_name = format!("index.restore-{}", uuid::Uuid::new_v4().simple());
            let tmp_dir = index_dir.with_file_name(&tmp_name);
            copy_dir(&index_src, &tmp_dir)?;

            match IndexManager::open_or_create(&tmp_dir) {
                Ok(new_mgr) => {
                    if let Ok(mut mgr) = index_manager.write() {
                        *mgr = new_mgr;
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                    return Err(format!("无法打开恢复的索引: {e}"));
                }
            }
            // 后续写入落到恢复后的索引，而不是已删除的旧目录
            indexer.reset_writer();

            let old = index_dir.with_file_name("index.old");
            if index_dir.exists() {
                let _ = std::fs::remove_dir_all(&old);
                let _ = std::fs::rename(&index_dir, &old);
            }
            if let Err(e) = std::fs::rename(&tmp_dir, &index_dir) {
                if old.exists() {
                    let _ = std::fs::rename(&old, &index_dir);
                }
                return Err(format!("切换索引目录失败: {e}"));
            }
            let _ = std::fs::remove_dir_all(&old);
        }

        // 2. 恢复数据库：SQLite 在线备份 API 写入活跃连接，不直接覆盖 data.db（WAL 模式安全）
        let db_src = backup_dir.join("data.db");
        if db_src.is_file() {
            // 备份的 data.db 是 WAL 库的静态副本，先复制到可写临时文件再打开：
            // 只读打开 WAL 库会因缺少 -wal/-shm 失败，直接读备份目录又会污染备份文件
            let staging = std::env::temp_dir().join(format!(
                "ls-restore-{}.db",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::copy(&db_src, &staging).map_err(|e| format!("复制备份数据库失败: {e}"))?;
            let restore_result = (|| -> Result<(), String> {
                let src = Connection::open(&staging)
                    .map_err(|e| format!("打开备份数据库失败: {e}"))?;
                let mut dst = db_pool
                    .get()
                    .map_err(|e| format!("获取数据库连接失败: {e}"))?;
                let backup = Backup::new(&src, &mut dst)
                    .map_err(|e| format!("初始化恢复失败: {e}"))?;
                // step(-1) 一次性备份全部页；Busy/Locked 为瞬时错误，重试（同 rusqlite::Connection::restore）
                let mut r = backup.step(-1).map_err(|e| format!("恢复数据库失败: {e}"))?;
                let mut busy = 0;
                while r == StepResult::Busy || r == StepResult::Locked {
                    busy += 1;
                    if busy >= 3 {
                        return Err("数据库繁忙，恢复未完成，请重试".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                    r = backup.step(-1).map_err(|e| format!("恢复数据库失败: {e}"))?;
                }
                if r != StepResult::Done {
                    return Err("数据库繁忙，恢复未完成，请重试".to_string());
                }
                Ok(())
            })();
            let _ = std::fs::remove_file(&staging);
            restore_result?;
        }

        Ok(())
    })
    .await
    .map_err(|e| format!("恢复任务异常: {e}"))?;
    result?;

    is_restoring.store(false, Ordering::SeqCst);
    log::info!("restored backup: {backup_name}");

    let _ = app.emit("restore-completed", serde_json::json!({ "name": backup_name }));
    app.restart()
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

#[cfg(test)]
mod tests {
    use super::*;

    // 镜像 restore_backup 的核心机制：从 WAL 格式的备份副本在线恢复到活跃连接，
    // 验证数据完整、无需关闭连接池（直接 fs::copy 覆盖活跃 data.db 会损坏库）。
    #[test]
    fn test_backup_api_restore_from_wal_copy() {
        let src_path = std::env::temp_dir().join(format!("ls_bk_src_{}.db", std::process::id()));
        let dst_path = std::env::temp_dir().join(format!("ls_bk_dst_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&dst_path);

        // 备份副本：WAL 模式的 data.db（模拟 trigger_backup 的 fs::copy 产物）
        {
            let src = Connection::open(&src_path).unwrap();
            src.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42);",
            )
            .unwrap();
        }

        // 活跃连接：已打开并持有（模拟连接池连接）
        let mut dst = Connection::open(&dst_path).unwrap();
        dst.execute_batch("CREATE TABLE t(x INTEGER);").unwrap();

        let src = Connection::open(&src_path).unwrap();
        let backup = Backup::new(&src, &mut dst).unwrap();
        assert_eq!(backup.step(-1).unwrap(), StepResult::Done);
        drop(backup);
        drop(src);
        drop(dst);

        let check = Connection::open(&dst_path).unwrap();
        let x: i64 = check.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42);
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&dst_path);
    }
}