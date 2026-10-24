use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};

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
    pub monitor_enabled: bool,
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
    pub monitor_enabled: bool,
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
            let mut slog = crate::logs::session::SessionLogGuard::open(&logs_dir, "scan");
            slog.write_line("[DIRS] 吸收子目录后自动全量扫描");
            let line = match scanner.full_scan(&dir_id, |_| {}) {
                Ok(r) => format!(
                    "[DIRS] 吸收后扫描完成: {} files, {} indexed, {} errors",
                    r.total_files, r.indexed, r.errors
                ),
                Err(e) => format!("[DIRS] 吸收后扫描失败: {e}"),
            };
            log::info!("{line}");
            slog.write_line(&line);
            is_scanning.store(false, std::sync::atomic::Ordering::SeqCst);
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
        monitor_enabled: dir.monitor_enabled,
        created_at: dir.created_at,
        updated_at: dir.updated_at,
    })
}

/// Report returned to the frontend so the delete dialog can tell the user
/// exactly what happened (and stay open until then).
#[derive(Serialize)]
pub struct RemoveDirReport {
    /// Human-readable summary, shown to the user.
    pub message: String,
    /// An in-flight scan was cancelled and waited for before purging.
    pub stopped_scan: bool,
    /// Orphan `dir_id`s left behind by that scan and swept afterwards.
    pub swept_orphan_dirs: u64,
    /// A scan was restarted to finish the remaining directories.
    pub resumed_scan: bool,
}

/// Live stage/status of a directory removal, surfaced to the delete dialog so it
/// can show what is happening instead of a bare spinner.
#[derive(Serialize, Clone)]
pub struct RemoveDirProgress {
    /// Stable machine-readable stage key; the frontend maps it to a label.
    pub stage: String,
    /// Free-form detail (elapsed seconds, counts, scan progress, …).
    pub detail: String,
}

fn emit_progress<R: Runtime>(app: &AppHandle<R>, stage: &str, detail: impl Into<String>) {
    let _ = app.emit(
        "remove-dir-progress",
        RemoveDirProgress { stage: stage.to_string(), detail: detail.into() },
    );
}

/// Ask any running scan to stop and wait until it really has.
///
/// Cancellation is cooperative (checked once per file / per batch), so a
/// pathological PDF can delay it by minutes. `remove_dir` **must** wait: purging
/// data while the scan is still writing recreates the very rows we are deleting
/// (orphan `content_index`, ghost Tantivy documents).
///
/// Returns `true` when a scan was actually stopped.
async fn wait_for_scan_to_stop<R: Runtime>(
    app: &AppHandle<R>,
    is_scanning: &std::sync::atomic::AtomicBool,
    cancel_scan: &std::sync::atomic::AtomicBool,
) -> bool {
    if !is_scanning.load(Ordering::SeqCst) {
        return false;
    }
    log::info!("[DIRS] 有扫描在进行：先取消并等待其退出…");
    cancel_scan.store(true, Ordering::SeqCst);
    emit_progress(app, "stopping_scan", "0s");

    let started = std::time::Instant::now();
    let mut warned = false;
    while is_scanning.load(Ordering::SeqCst) {
        emit_progress(app, "stopping_scan", format!("{:.0}s", started.elapsed().as_secs_f64()));
        if !warned && started.elapsed().as_secs() >= 60 {
            log::warn!("[DIRS] 等待扫描退出已超过 60s（可能正在提取大文件/扫描件）…");
            warned = true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    log::info!("[DIRS] 扫描已退出（等待 {:.1}s）", started.elapsed().as_secs_f64());
    true
}

#[tauri::command]
pub async fn remove_dir<R: Runtime>(
    state: State<'_, AppState>,
    app: AppHandle<R>,
    id: String,
    purge: Option<bool>,
) -> Result<RemoveDirReport, String> {
    let purge = purge.unwrap_or(false);

    // ① 停止正在进行的扫描并等它真正退出（否则它会继续往这个目录写数据）。
    let stopped_scan = wait_for_scan_to_stop(&app, &state.is_scanning, &state.cancel_scan).await;

    // ② 数据清理：Tantivy 文档 + file_tracking + 逐文件向量/摘要/错误 + md5 键的
    //    孤儿 content/chunks/chunk 向量。共享内容（其它目录也持有同一 md5）会保留。
    emit_progress(&app, "purging", "");
    let indexer = state.indexer.clone();
    let dir_for_purge = id.clone();
    tokio::task::spawn_blocking(move || indexer.purge_dir_data(&dir_for_purge, purge))
        .await
        .map_err(|e| format!("purge task failed: {e}"))?
        .map_err(|e| format!("failed to clear directory data: {e}"))?;

    // ③ 收尾 sweep：取消是合作式的，扫描可能在被叫停前又写进了一点数据，且会留下
    //    dir_id 已不在 dir_config 的孤儿行。这里统一清掉，确保没有幽灵命中。
    emit_progress(&app, "sweeping", "");
    let indexer_sweep = state.indexer.clone();
    let swept_orphan_dirs = tokio::task::spawn_blocking(move || indexer_sweep.sweep_orphan_dirs())
        .await
        .map_err(|e| format!("sweep task failed: {e}"))?
        .map_err(|e| format!("failed to sweep orphan data: {e}"))?;

    // ④ 移除监控配置，并把刚释放的空间从 WAL 里收回（不用 VACUUM：它会重写整个库
    //    并长时间独占写锁，实测让应用冻结约 40s）。
    emit_progress(&app, "removing", "");
    let remaining_dirs = {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        db::dir_config::remove_dir(&conn, &id)
            .map_err(|e| format!("failed to remove directory: {e}"))?;
        if purge {
            match crate::db::wal_checkpoint_truncate(&conn) {
                Ok((busy, log_pages, done)) => {
                    log::info!("[DIRS] WAL checkpoint: busy={busy} log={log_pages} done={done}");
                }
                Err(e) => log::warn!("[DIRS] WAL checkpoint failed: {e}"),
            }
        }
        db::dir_config::list_dirs(&conn).map(|d| d.len()).unwrap_or(0)
    };
    let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StopWatch {
        dir_id: id.clone(),
    });

    // ⑤ 续跑：被中止的扫描对**其它目录**继续做完，用户不需要再手动点扫描。
    //    走的就是 trigger_scan 用的同一段代码（ScanWorker）；因为 needs_reindex 会
    //    跳过未变更的文件，代价只有一次遍历。进度通过 scan-progress 事件透出。
    let mut resumed_scan = false;
    if stopped_scan
        && remaining_dirs > 0
        && state
            .is_scanning
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        emit_progress(&app, "resuming", "");
        let worker = crate::commands::index::ScanWorker {
            app,
            db: state.db.clone(),
            scanner: state.scanner.clone(),
            indexer: state.indexer.clone(),
            is_scanning: state.is_scanning.clone(),
            cancel_scan: state.cancel_scan.clone(),
            scan_delta: state.scan_delta.clone(),
            logs_dir: state.data_dir.join("logs"),
        };
        resumed_scan = true;
        tokio::task::spawn_blocking(move || worker.run(None))
            .await
            .map_err(|e| format!("resume scan task failed: {e}"))?;
    }

    // ⑥ 收尾（二次）：删除期间嵌入 keeper 可能仍在用 purge 之前的快照写块向量
    //    （实测每次删除会残留 ~150 行无人引用的 chunk_embeddings）。续扫结束后
    //    再清一次孤儿。keeper 可能仍在排空，所以这是尽力而为；启动清理是最终保证。
    let indexer_final = state.indexer.clone();
    match tokio::task::spawn_blocking(move || indexer_final.sweep_orphan_dirs()).await {
        Ok(Ok(n)) if n > 0 => log::info!("[DIRS] 收尾清理了 {n} 个残留目录的数据"),
        Ok(Ok(_)) => {}
        Ok(Err(e)) => log::warn!("[DIRS] 收尾清理失败: {e}"),
        Err(e) => log::warn!("[DIRS] 收尾清理任务失败: {e}"),
    }

    log::info!(
        "[DIRS] 移除目录 {id}（purge={purge}, 停止扫描={stopped_scan}, sweep={swept_orphan_dirs}, 续扫={resumed_scan}）"
    );
    let mut message = format!("已移除目录并清理数据（清理残留目录 {swept_orphan_dirs} 个）");
    if stopped_scan {
        message.push_str("；已停止正在进行的扫描");
    }
    if resumed_scan {
        message.push_str("；已继续索引其它目录");
    }
    Ok(RemoveDirReport {
        message,
        stopped_scan,
        swept_orphan_dirs,
        resumed_scan,
    })
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
            monitor_enabled: dir.monitor_enabled,
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
    monitor_enabled: Option<bool>,
) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let updates = db::dir_config::DirUpdate {
        alias,
        ocr_lang,
        exclude_patterns,
        include_exts,
        recursive,
        monitor_enabled,
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

    // 停用的目录：只停 watcher，不启动监控、也不做增量扫描（数据保留）。
    if !dir.monitor_enabled {
        log::info!("[DIRS] 目录已停用（保留数据，不扫描）: {}", dir.path);
        let _ = app.emit("dirs-changed", serde_json::json!({}));
        return Ok(());
    }

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

/// 磁盘绝对条目路径 → 与 `file_tracking.path`（相对监控根）同一坐标系的键。
/// root 未命中时退化为绝对路径字符串（保持旧行为，避免误报已索引）。
pub(crate) fn indexed_key(dir_root: Option<&str>, abs: &std::path::Path) -> String {
    dir_root
        .and_then(|root| crate::scanner::helpers::to_relative(root, abs).ok())
        .unwrap_or_else(|| abs.to_string_lossy().to_string())
}

/// 懒加载：返回 `parent_path` 目录的单层子项（文件+目录，隐藏已过滤）。
/// 同时返回每文件的 indexed 状态（从 file_tracking 表查询）。
///
/// `dirs_only = true`：只返回子目录，且**跳过**"把全库已索引路径读进内存"那一步
/// —— 资源管理器的导航树只需要目录，没必要为每次展开扫 1.9 万条路径。
#[tauri::command]
pub fn get_dir_children(
    state: State<'_, AppState>,
    parent_path: String,
    dirs_only: Option<bool>,
) -> Result<Vec<DirTreeNode>, String> {
    let dirs_only = dirs_only.unwrap_or(false);
    let mut children = Vec::new();

    // 与文件夹视图同一道硬边界：不允许列举资料库之外的目录
    {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        if !crate::db::dir_config::path_within_registered_roots(&conn, &parent_path)
            .map_err(|e| format!("db error: {e}"))?
        {
            return Err("路径不在已添加的资料库范围内".to_string());
        }
    }

    // file_tracking.path 存的是相对监控根的路径（启动时已 migrate_paths_to_relative），
    // 而 read_dir 给出的是绝对路径——两者必须统一坐标系，否则 indexed 判定
    // 永远为 false，文件树里全部文件显示"未索引"。
    //
    // Windows 下还叠加一层分隔符不一致：read_dir/node.path 是反斜杠（D:\foo），
    // dir_config.path 原样存对话框返回值（也是反斜杠），但子目录展开时前缀
    // 拼接又假设正斜杠——两边必须同时归一化，只归一 parent_path 会让根层
    // 精确匹配也失配。
    let parent_path_norm = parent_path.replace('\\', "/");
    let mut indexed_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut indexed_known = false;
    let mut dir_root: Option<String> = None;
    if !dirs_only && let Ok(conn) = state.db.get() {
        if let Ok(dirs) = crate::db::dir_config::list_dirs(&conn) {
            dir_root = dirs
                .iter()
                .filter(|d| {
                    let dp = d.path.replace('\\', "/").trim_end_matches('/').to_string();
                    parent_path_norm == dp
                        || parent_path_norm.starts_with(&format!("{}/", dp))
                })
                .max_by_key(|d| d.path.len())
                .map(|d| d.path.clone());
        }
        if let Ok(mut stmt) = conn
            .prepare("SELECT path FROM file_tracking WHERE status='active' AND indexed=1")
            && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0))
        {
            indexed_known = true;
            for path in rows.flatten() {
                indexed_set.insert(path);
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(&parent_path) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') { continue; }
                let abs = entry.path();
                let path = abs.to_string_lossy().to_string();
                let is_file = !ft.is_dir();
                if dirs_only && is_file {
                    continue;
                }
                let key = indexed_key(dir_root.as_deref(), &abs);
                let is_indexed = is_file && indexed_known && indexed_set.contains(&key);
                let status = if is_file && indexed_known {
                    Some(if is_indexed { "indexed" } else { "unindexed" }.to_string())
                } else {
                    None
                };

                children.push(DirTreeNode {
                    name,
                    path,
                    is_dir: ft.is_dir(),
                    children: vec![],
                    indexed: Some(is_indexed),
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 磁盘绝对路径必须映射到与 file_tracking.path 一致的相对路径键，
    /// 否则文件树里所有文件都会被误判为"未索引"。
    #[test]
    fn indexed_key_maps_absolute_to_relative_under_root() {
        let root = "/data/工作日志";
        assert_eq!(
            indexed_key(Some(root), Path::new("/data/工作日志/案件/X/a.pdf")),
            "案件/X/a.pdf"
        );
        // root 不匹配（文件不在该 root 下）→ 退化为绝对路径，不误报已索引。
        assert_eq!(
            indexed_key(Some(root), Path::new("/other/案件/X/a.pdf")),
            "/other/案件/X/a.pdf"
        );
        // root 未知 → 绝对路径。
        assert_eq!(
            indexed_key(None, Path::new("/data/工作日志/案件/X/a.pdf")),
            "/data/工作日志/案件/X/a.pdf"
        );
    }
}
