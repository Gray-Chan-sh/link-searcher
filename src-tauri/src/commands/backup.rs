use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};

use crate::config::{config_file_path, INDEX_DIR_NAME};
use crate::search::IndexManager;
use crate::state::AppState;

#[derive(Serialize)]
pub struct BackupInfo {
    pub last_backup: Option<i64>,
    pub backup_size: u64,
    pub backup_count: u64,
}

/// 快照中的单个文件：相对名（索引内用 `/` 分隔）+ 大小 + sha256。
#[derive(Serialize, Deserialize, Clone)]
pub struct SnapshotFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

/// 一次快照的清单，写入 `snapshot.json`；后续增量链 / 导出 zip / 恢复均以此为准。
#[derive(Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub files: Vec<SnapshotFile>,
    pub size: u64,
}

/// 链中的一次快照记录（增量链的持久化状态，跟随 `.chain.json`）。
#[derive(Serialize, Deserialize)]
pub struct ChainSnapshot {
    pub id: String,
    pub ts: i64,
    /// "baseline" 或 "incremental"
    pub kind: String,
    pub files: Vec<SnapshotFile>,
}

/// 增量备份链：`{data_dir}/backups/.chain.json`，记录全部快照 + 已存储的
/// 不可变 segment 文件集合（segment_store，链内去重，硬链接共享一份物理副本）。
#[derive(Serialize, Deserialize, Default)]
pub struct ChainHead {
    /// 首个全量快照 id；被裁剪后可为空串（下次全量快照恢复）
    pub baseline_id: String,
    pub snapshots: Vec<ChainSnapshot>,
    pub segment_store: Vec<String>,
}

fn chain_path(backup_dir: &std::path::Path) -> std::path::PathBuf {
    backup_dir.join(".chain.json")
}

/// 读取链文件；缺失或损坏 → 返回空链。
fn load_chain(backup_dir: &std::path::Path) -> Result<ChainHead, String> {
    match std::fs::read_to_string(chain_path(backup_dir)) {
        Ok(s) => match serde_json::from_str(&s) {
            Ok(chain) => Ok(chain),
            Err(e) => {
                log::warn!("链文件损坏，已重置为全新备份链: {e}");
                Ok(ChainHead::default())
            }
        },
        Err(_) => Ok(ChainHead::default()),
    }
}

/// 原子写链文件：先写 `.chain.json.tmp` 再 rename 覆盖，避免写一半损坏。
fn save_chain(backup_dir: &std::path::Path, chain: &ChainHead) -> Result<(), String> {
    let path = chain_path(backup_dir);
    let tmp = backup_dir.join(".chain.json.tmp");
    let json = serde_json::to_string_pretty(chain).map_err(|e| format!("failed to serialize chain: {e}"))?;
    std::fs::write(&tmp, json).map_err(|e| format!("failed to write chain tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("failed to write chain file: {e}"))
}

/// 把 `.ls-index` / `data.db` / `config.json` / `chat_history.json` 写入 `dest`，
/// 返回清单。data.db 用 SQLite 在线备份 API（WAL 安全），Busy/Locked 重试 3 次。
fn snapshot_core(state: &AppState, dest: &std::path::Path) -> Result<SnapshotManifest, String> {
    let mut files: Vec<SnapshotFile> = Vec::new();
    let mut total_size: u64 = 0;

    // Backup Tantivy index
    let index_dest = dest.join(INDEX_DIR_NAME);
    copy_dir(&state.index_dir, &index_dest)?;
    for f in collect_files(&index_dest)? {
        let name = f
            .strip_prefix(&index_dest)
            .map_err(|e| format!("path strip error: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        if name.ends_with("-wal") || name.ends_with("-shm") {
            continue;
        }
        let meta = std::fs::metadata(&f).map_err(|e| format!("failed to stat {f:?}: {e}"))?;
        total_size += meta.len();
        files.push(SnapshotFile {
            name,
            size: meta.len(),
            sha256: file_sha256(&f)?,
        });
    }

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
    drop(backup);
    drop(src_conn);
    drop(dst_conn);
    let db_meta = std::fs::metadata(&db_dest).map_err(|e| format!("failed to stat {db_dest:?}: {e}"))?;
    total_size += db_meta.len();
    files.push(SnapshotFile {
        name: "data.db".to_string(),
        size: db_meta.len(),
        sha256: file_sha256(&db_dest)?,
    });

    // config.json 位于 config_dir（LS_CONFIG_DIR / ~/.config/.link-searcher），非 data_dir
    copy_file_if_exists(
        &config_file_path(),
        &dest.join("config.json"),
        "config.json",
        &mut files,
        &mut total_size,
    )?;

    // chat_history.json 位于 data_dir
    copy_file_if_exists(
        &state.data_dir.join("chat_history.json"),
        &dest.join("chat_history.json"),
        "chat_history.json",
        &mut files,
        &mut total_size,
    )?;

    Ok(SnapshotManifest { files, size: total_size })
}

/// 增量快照：直接读 live 索引目录，不可变 segment 硬链接到链中已存副本
/// （无则复制并登记 segment_store）；meta.json/.managed.json 原子替换、每次复制；
/// 再走与 snapshot_core 相同的在线备份 API 复制 data.db + config.json + chat_history.json。
fn snapshot_incremental(
    state: &AppState,
    dest: &std::path::Path,
    chain: &mut ChainHead,
) -> Result<SnapshotManifest, String> {
    let mut files: Vec<SnapshotFile> = Vec::new();
    let mut total_size: u64 = 0;

    // 遍历 LIVE 索引目录（不复制整棵树——硬链接必须指向 live 的不可变 segment）
    for f in collect_files(&state.index_dir)? {
        let rel = f
            .strip_prefix(&state.index_dir)
            .map_err(|e| format!("path strip error: {e}"))?
            .to_string_lossy()
            .replace('\\', "/");
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        if rel.ends_with("-wal")
            || rel.ends_with("-shm")
            || name == ".tantivy-writer.lock"
            || name == ".tantivy-meta.lock"
        {
            continue;
        }
        let dest_path = dest.join(INDEX_DIR_NAME).join(&rel);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {parent:?}: {e}"))?;
        }
        if name == "meta.json" || name == ".managed.json" {
            // 原子 tmp+rename 写入的元数据：不能硬链接，每次复制新副本
            std::fs::copy(&f, &dest_path).map_err(|e| format!("failed to copy {f:?}: {e}"))?;
        } else if chain.segment_store.iter().any(|s| s == &rel) {
            // 链中已有该 segment：硬链接（跨设备等失败时回退复制）
            if std::fs::hard_link(&f, &dest_path).is_err() {
                std::fs::copy(&f, &dest_path).map_err(|e| format!("failed to copy {f:?}: {e}"))?;
            }
        } else {
            std::fs::copy(&f, &dest_path).map_err(|e| format!("failed to copy {f:?}: {e}"))?;
            chain.segment_store.push(rel.clone());
        }
        let meta = std::fs::metadata(&dest_path).map_err(|e| format!("failed to stat {dest_path:?}: {e}"))?;
        total_size += meta.len();
        files.push(SnapshotFile {
            name: rel,
            size: meta.len(),
            sha256: file_sha256(&dest_path)?,
        });
    }

    // SQLite 在线备份 API（WAL 安全），Busy/Locked 重试 3 次——与 snapshot_core 相同
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
    drop(backup);
    drop(src_conn);
    drop(dst_conn);
    let db_meta = std::fs::metadata(&db_dest).map_err(|e| format!("failed to stat {db_dest:?}: {e}"))?;
    total_size += db_meta.len();
    files.push(SnapshotFile {
        name: "data.db".to_string(),
        size: db_meta.len(),
        sha256: file_sha256(&db_dest)?,
    });

    // config.json 位于 config_dir（LS_CONFIG_DIR / ~/.config/.link-searcher），非 data_dir
    copy_file_if_exists(
        &config_file_path(),
        &dest.join("config.json"),
        "config.json",
        &mut files,
        &mut total_size,
    )?;

    // chat_history.json 位于 data_dir
    copy_file_if_exists(
        &state.data_dir.join("chat_history.json"),
        &dest.join("chat_history.json"),
        "chat_history.json",
        &mut files,
        &mut total_size,
    )?;

    Ok(SnapshotManifest { files, size: total_size })
}

#[tauri::command]
pub async fn trigger_backup(state: State<'_, AppState>) -> Result<(), String> {
    let backup_dir = state.data_dir.join("backups");
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("failed to create backup dir: {e}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("time error: {e}"))?
        .as_secs();
    let backup_name = format!("backup_{timestamp}");
    let dest = backup_dir.join(&backup_name);

    std::fs::create_dir_all(&dest).map_err(|e| format!("failed to create backup dir: {e}"))?;

    let mut chain = load_chain(&backup_dir)?;
    let is_baseline = chain.snapshots.is_empty();
    let manifest = snapshot_incremental(&state, &dest, &mut chain)?;
    if is_baseline {
        chain.baseline_id = backup_name.clone();
    }
    chain.snapshots.push(ChainSnapshot {
        id: backup_name.clone(),
        ts: timestamp as i64,
        kind: if is_baseline { "baseline" } else { "incremental" }.to_string(),
        files: manifest.files.clone(),
    });

    // 只保留最近 10 个快照；被剪掉的是 baseline 也没关系（下次全量快照会重建 baseline）
    while chain.snapshots.len() > 10 {
        let dropped = chain.snapshots.remove(0);
        if dropped.id == chain.baseline_id {
            chain.baseline_id.clear();
        }
    }
    save_chain(&backup_dir, &chain)?;

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("failed to serialize manifest: {e}"))?;
    std::fs::write(dest.join("snapshot.json"), manifest_json)
        .map_err(|e| format!("failed to write snapshot.json: {e}"))?;

    // Cleanup old backups: keep only the 10 most recent
    cleanup_old_backups(&backup_dir, 10);

    log::info!("backup completed: {backup_name}");
    Ok(())
}

#[tauri::command]
pub async fn get_backup_status(state: State<'_, AppState>) -> Result<BackupInfo, String> {
    let backup_dir = state.data_dir.join("backups");
    let chain = load_chain(&backup_dir)?;
    if chain.snapshots.is_empty() {
        return Ok(BackupInfo {
            last_backup: None,
            backup_size: 0,
            backup_count: 0,
        });
    }

    let last_backup = chain.snapshots.last().map(|s| s.ts);
    let count = chain.snapshots.len() as u64;

    // 物理占用：每个快照中“复制的新文件”（非 segment 的 db/config/chat/meta/.managed 等小文件）
    // 各占一份；segment_store 里每个 segment 整链只占一份（其余快照硬链接共享）
    let fresh_bytes: u64 = chain
        .snapshots
        .iter()
        .flat_map(|s| s.files.iter())
        .filter(|f| !chain.segment_store.iter().any(|n| n == &f.name))
        .map(|f| f.size)
        .sum();
    let segment_bytes: u64 = chain
        .segment_store
        .iter()
        .map(|seg| {
            chain
                .snapshots
                .iter()
                .flat_map(|s| s.files.iter())
                .find(|f| &f.name == seg)
                .map(|f| f.size)
                .unwrap_or(0)
        })
        .sum();

    Ok(BackupInfo {
        last_backup,
        backup_size: fresh_bytes + segment_bytes,
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
    copy_dir_depth(src, dst, 0)
}

/// 递归复制目录，`depth` 防止极端深目录导致栈溢出（上限 64 层）。
fn copy_dir_depth(src: &std::path::Path, dst: &std::path::Path, depth: u32) -> Result<(), String> {
    if depth > 64 {
        return Err(format!("目录嵌套过深（>{depth} 层），已停止复制: {src:?}"));
    }
    std::fs::create_dir_all(dst).map_err(|e| format!("failed to create {dst:?}: {e}"))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("failed to read {src:?}: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read entry error: {e}"))?;
        let ty = entry.file_type().map_err(|e| format!("file type error: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_depth(&src_path, &dst_path, depth + 1)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("failed to copy {src_path:?}: {e}"))?;
        }
    }
    Ok(())
}

/// 递归收集目录下所有文件（不包含目录本身）。
fn collect_files(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| format!("failed to read {d:?}: {e}"))? {
            let entry = entry.map_err(|e| format!("read entry error: {e}"))?;
            let path = entry.path();
            if entry.file_type().map_err(|e| format!("file type error: {e}"))?.is_dir() {
                dirs.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut f = std::fs::File::open(path).map_err(|e| format!("failed to open {path:?}: {e}"))?;
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("failed to read {path:?}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 源存在则复制并计入清单，否则跳过。
fn copy_file_if_exists(
    src: &std::path::Path,
    dst: &std::path::Path,
    name: &str,
    files: &mut Vec<SnapshotFile>,
    total_size: &mut u64,
) -> Result<(), String> {
    if !src.is_file() {
        return Ok(());
    }
    std::fs::copy(src, dst).map_err(|e| format!("failed to copy {src:?}: {e}"))?;
    let meta = std::fs::metadata(dst).map_err(|e| format!("failed to stat {dst:?}: {e}"))?;
    *total_size += meta.len();
    files.push(SnapshotFile {
        name: name.to_string(),
        size: meta.len(),
        sha256: file_sha256(dst)?,
    });
    Ok(())
}

fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut dirs = vec![path.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                dirs.push(entry.path());
            } else {
                total += meta.len();
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

    #[test]
    fn test_snapshot_core_copies_all_artifacts() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};

        use crate::db;
        use crate::indexer::IndexerService;
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let base = std::env::temp_dir().join(format!("ls_snap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // config.json 放进 LS_CONFIG_DIR 指向的目录（config.rs 的测试后门）
        let config_dir = base.join("config_dir");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        unsafe { std::env::set_var("LS_CONFIG_DIR", &config_dir) };

        // 小 SQLite 库（WAL 模式）
        let db_path = base.join("data.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42);",
            )
            .unwrap();
        }

        // data_dir 放 chat_history.json；index_dir 放一个文本文件
        let data_dir = base.join("data");
        let index_dir = data_dir.join(INDEX_DIR_NAME);
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("a.txt"), "hello").unwrap();
        std::fs::write(data_dir.join("chat_history.json"), "[]").unwrap();

        // AppState（snapshot_core 只用 db_path/data_dir/index_dir，其余字段占位）
        let db_str = db_path.to_str().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        drop(conn);
        let pool = db::get_pool(db_str).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool,
            im,
            indexer,
            scanner,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            data_dir,
            index_dir,
            db_path,
            dummy_tx,
            None,
        );

        let dest = base.join("backup_1");
        std::fs::create_dir_all(&dest).unwrap();
        let manifest = snapshot_core(&state, &dest).unwrap();

        // 4 类产物全部存在
        assert!(dest.join(".ls-index/a.txt").is_file());
        assert!(dest.join("data.db").is_file());
        assert!(dest.join("config.json").is_file());
        assert!(dest.join("chat_history.json").is_file());

        // 清单：1 索引文件 + data.db + config.json + chat_history.json
        assert_eq!(manifest.files.len(), 4);
        let names: Vec<&str> = manifest.files.iter().map(|f| f.name.as_str()).collect();
        for n in ["a.txt", "data.db", "config.json", "chat_history.json"] {
            assert!(names.contains(&n), "manifest 缺少 {n}");
        }
        let total: u64 = manifest.files.iter().map(|f| f.size).sum();
        assert_eq!(manifest.size, total);

        // sha256 正确性（"hello" 的 sha256 与 size=5）
        let expected_hello: String = Sha256::digest(b"hello")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let a = manifest.files.iter().find(|f| f.name == "a.txt").unwrap();
        assert_eq!(a.sha256, expected_hello);
        assert_eq!(a.size, 5);

        // 备份的 data.db 可读：查询到插入的行
        let check = Connection::open(dest.join("data.db")).unwrap();
        let x: i64 = check.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 42);

        // 清理
        drop(state);
        let _ = std::fs::remove_dir_all(&base);
        unsafe { std::env::remove_var("LS_CONFIG_DIR") };
    }

    #[test]
    fn test_snapshot_incremental_hardlinks_segments() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};

        use crate::db;
        use crate::indexer::IndexerService;
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let base = std::env::temp_dir().join(format!("ls_incr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // config.json 放进 LS_CONFIG_DIR 指向的目录（config.rs 的测试后门）
        let config_dir = base.join("config_dir");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.json"), r#"{"theme":"dark"}"#).unwrap();
        unsafe { std::env::set_var("LS_CONFIG_DIR", &config_dir) };

        // 小 SQLite 库（WAL 模式）
        let db_path = base.join("data.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; CREATE TABLE t(x INTEGER); INSERT INTO t VALUES(42);",
            )
            .unwrap();
        }

        // data_dir 放 chat_history.json；index_dir 模拟 Tantivy：2 个 segment + meta.json
        let data_dir = base.join("data");
        let index_dir = data_dir.join(INDEX_DIR_NAME);
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("s1.idx"), "segment-one").unwrap();
        std::fs::write(index_dir.join("s2.pos"), "segment-two").unwrap();
        std::fs::write(index_dir.join("meta.json"), r#"{"version":1}"#).unwrap();
        std::fs::write(data_dir.join("chat_history.json"), "[]").unwrap();

        let db_str = db_path.to_str().unwrap();
        let conn = Connection::open(&db_path).unwrap();
        db::init_db(&conn).unwrap();
        drop(conn);
        let pool = db::get_pool(db_str).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool,
            im,
            indexer,
            scanner,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            data_dir,
            index_dir.clone(),
            db_path,
            dummy_tx,
            None,
        );

        let mut chain = ChainHead::default();
        let b1 = base.join("backup_1");
        let b2 = base.join("backup_2");
        std::fs::create_dir_all(&b1).unwrap();
        std::fs::create_dir_all(&b2).unwrap();

        // 第一次：2 segments 复制 + meta 复制 + data.db + config.json + chat_history.json
        let m1 = snapshot_incremental(&state, &b1, &mut chain).unwrap();
        assert_eq!(m1.files.len(), 6);

        // 第二次前：新增 s3.idx、修改 meta.json
        std::fs::write(index_dir.join("s3.idx"), "segment-three").unwrap();
        std::fs::write(index_dir.join("meta.json"), r#"{"version":2}"#).unwrap();

        let m2 = snapshot_incremental(&state, &b2, &mut chain).unwrap();

        // backup_2 清单：s1/s2（硬链接）+ s3（复制）+ meta（新副本）+ 3 个小文件
        assert_eq!(m2.files.len(), 7);
        let names1: Vec<&str> = m1.files.iter().map(|f| f.name.as_str()).collect();
        let names2: Vec<&str> = m2.files.iter().map(|f| f.name.as_str()).collect();
        for n in [
            "s1.idx",
            "s2.pos",
            "s3.idx",
            "meta.json",
            "data.db",
            "config.json",
            "chat_history.json",
        ] {
            assert!(names2.contains(&n), "backup_2 清单缺少 {n}");
        }

        // meta.json 每次都是全新副本：backup_1 是 v1、backup_2 是 v2
        assert_eq!(
            std::fs::read_to_string(b1.join(INDEX_DIR_NAME).join("meta.json")).unwrap(),
            r#"{"version":1}"#
        );
        assert_eq!(
            std::fs::read_to_string(b2.join(INDEX_DIR_NAME).join("meta.json")).unwrap(),
            r#"{"version":2}"#
        );

        // s1/s2 与 live 索引是同一 inode（硬链接）→ nlink==2；s3 与 backup_1 的 s1 是独立副本 → nlink==1
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(b2.join(INDEX_DIR_NAME).join("s1.idx")).unwrap().nlink(), 2);
            assert_eq!(std::fs::metadata(b2.join(INDEX_DIR_NAME).join("s2.pos")).unwrap().nlink(), 2);
            assert_eq!(std::fs::metadata(b2.join(INDEX_DIR_NAME).join("s3.idx")).unwrap().nlink(), 1);
            assert_eq!(std::fs::metadata(b1.join(INDEX_DIR_NAME).join("s1.idx")).unwrap().nlink(), 1);
        }
        assert_eq!(
            std::fs::read_to_string(b2.join(INDEX_DIR_NAME).join("s1.idx")).unwrap(),
            "segment-one"
        );

        // segment_store 每个 segment 只登记一次
        assert_eq!(chain.segment_store.len(), 3);
        for n in ["s1.idx", "s2.pos", "s3.idx"] {
            assert!(chain.segment_store.iter().any(|s| s == n), "segment_store 缺少 {n}");
        }

        // data.db / config.json / chat_history.json 两次备份都出现
        for n in ["data.db", "config.json", "chat_history.json"] {
            assert!(names1.contains(&n), "backup_1 清单缺少 {n}");
            assert!(names2.contains(&n), "backup_2 清单缺少 {n}");
        }

        // 清理
        drop(state);
        let _ = std::fs::remove_dir_all(&base);
        unsafe { std::env::remove_var("LS_CONFIG_DIR") };
    }
}