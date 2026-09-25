use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::{AppHandle, Emitter, State};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::Connection;
use crate::config::{AppConfig, INDEX_DIR_NAME, load_config, save_config};
use crate::db;
use crate::state::AppState;

#[derive(Serialize, Deserialize, Clone)]
pub struct ConfigInfo {
    pub data_dir: String,
    pub language: String,
    /// Legacy single-gateway fields (kept for backward compat; new UI uses
    /// embedding_*/llm_* pairs).
    pub ai_api_base: String,
    pub ai_api_key: String,
    pub embedding_api_base: String,
    pub embedding_api_key: String,
    pub embedding_model: String,
    pub llm_api_base: String,
    pub llm_api_key: String,
    pub llm_model: String,
    #[serde(default)]
    pub providers: Vec<crate::config::ProviderConfig>,
    #[serde(default)]
    pub active_embedding_model_id: String,
    #[serde(default)]
    pub active_llm_model_id: String,
    #[serde(default)]
    pub semantic_weight: f64,
    #[serde(default)]
    pub active_reranker_model_id: String,
}

#[tauri::command]
pub fn get_config() -> Result<ConfigInfo, String> {
    let config = load_config();
    Ok(ConfigInfo {
        data_dir: config.data_dir.to_string_lossy().to_string(),
        language: config.language,
        ai_api_base: config.ai_api_base,
        ai_api_key: config.ai_api_key,
        embedding_api_base: config.embedding_api_base,
        embedding_api_key: config.embedding_api_key,
        embedding_model: config.embedding_model,
        llm_api_base: config.llm_api_base,
        llm_api_key: config.llm_api_key,
        llm_model: config.llm_model,
        providers: config.providers,
        active_embedding_model_id: config.active_embedding_model_id,
        active_llm_model_id: config.active_llm_model_id,
        semantic_weight: config.semantic_weight,
        active_reranker_model_id: config.active_reranker_model_id,
    })
}

#[tauri::command]
pub fn update_config(
    state: State<'_, AppState>,
    new_config: ConfigInfo,
) -> Result<(), String> {
    // Only validate when the data dir is actually changing.
    let current = load_config();
    if Path::new(&new_config.data_dir) != current.data_dir.as_path() {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let dirs = db::dir_config::list_dirs(&conn).map_err(|e| format!("{e}"))?;
        drop(conn);
        for dir in &dirs {
            crate::commands::helpers::check_data_dir_overlap(
                Path::new(&new_config.data_dir),
                Path::new(&dir.path),
            )?;
        }
    }

    let mut config = AppConfig {
        data_dir: new_config.data_dir.into(),
        language: new_config.language,
        ai_api_base: new_config.ai_api_base,
        ai_api_key: new_config.ai_api_key,
        embedding_api_base: new_config.embedding_api_base,
        embedding_api_key: new_config.embedding_api_key,
        embedding_model: new_config.embedding_model,
        llm_api_base: new_config.llm_api_base,
        llm_api_key: new_config.llm_api_key,
        llm_model: new_config.llm_model,
        providers: new_config.providers,
        active_embedding_model_id: new_config.active_embedding_model_id,
        active_llm_model_id: new_config.active_llm_model_id,
        semantic_weight: new_config.semantic_weight,
        active_reranker_model_id: new_config.active_reranker_model_id,
        pending_cleanup_dir: current.pending_cleanup_dir.clone(),
    };
    // New UI writes the split pairs; mirror into the legacy single-gateway
    // fields for any older consumers that still read ai_api_base/key.
    config.ai_api_base = config.embedding_api_base.clone();
    config.ai_api_key = config.embedding_api_key.clone();
    save_config(&config)
}

/// Add a new AI provider. `models` is pulled from `GET {base}/models` when
/// the request succeeds; on failure the provider is still saved with empty
/// models and the error is returned for the UI to toast.
#[tauri::command]
pub async fn add_provider(
    name: String,
    base_url: String,
    api_key: String,
) -> Result<ProviderOutcome, String> {
    let mut config = load_config();
    let id = uuid::Uuid::new_v4().to_string();
    // Pull models off the UI thread: the request can block for up to 30s.
    let (models, pull_err) = if base_url.trim().is_empty() {
        (Vec::new(), None)
    } else {
        let base_url = base_url.clone();
        let api_key = api_key.clone();
        tokio::task::spawn_blocking(move || {
            crate::ai::list_provider_models(&base_url, &api_key)
        })
        .await
        .unwrap_or_else(|_| (Vec::new(), Some("拉取任务失败".into())))
    };
    config.providers.push(crate::config::ProviderConfig {
        id: id.clone(),
        name,
        base_url: base_url.trim().to_string(),
        api_key,
        models: crate::config::auto_enable_first_per_type(models),
    });
    save_config(&config)?;
    Ok(ProviderOutcome { id, pull_error: pull_err })
}

/// Edit an existing provider's name/base/key. Models are NOT re-pulled here
/// (use `refresh_provider_models`); changed credentials keep the cached list
/// until the user refreshes.
#[tauri::command]
pub fn update_provider(
    id: String,
    name: String,
    base_url: String,
    api_key: String,
) -> Result<(), String> {
    let mut config = load_config();
    let Some(p) = config.providers.iter_mut().find(|p| p.id == id) else {
        return Err(format!("provider not found: {id}"));
    };
    p.name = name;
    p.base_url = base_url.trim().to_string();
    p.api_key = api_key;
    save_config(&config)
}

/// Delete a provider and its cached models. Refuses when it is the active
/// embedding or LLM endpoint (the UI disables delete in that case too).
#[tauri::command]
pub fn delete_provider(id: String) -> Result<(), String> {
    let mut config = load_config();
    if config.active_embedding_model_id.starts_with(&format!("{id}:"))
        || config.active_llm_model_id.starts_with(&format!("{id}:"))
    {
        return Err("该 Provider 正在使用中，请先切换当前模型".into());
    }
    config.providers.retain(|p| p.id != id);
    save_config(&config)
}

/// Re-pull a provider's model list, merging by model id: existing model types
/// (including user overrides) are kept, new models are auto-classified.
/// Returns the updated list plus any pull error for the UI.
#[tauri::command]
pub async fn refresh_provider_models(id: String) -> Result<Vec<crate::config::ModelConfig>, String> {
    let mut config = load_config();
    let Some(p) = config.providers.iter_mut().find(|p| p.id == id) else {
        return Err(format!("provider not found: {id}"));
    };
    let base_url = p.base_url.clone();
    let api_key = p.api_key.clone();
    let (fresh, pull_err) = tokio::task::spawn_blocking(move || {
        crate::ai::list_provider_models(&base_url, &api_key)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), Some("拉取任务失败".into())));
    if fresh.is_empty() {
        // Pull failed or empty: keep the old list untouched.
        return if let Some(e) = pull_err {
            Err(format!("拉取失败: {e}"))
        } else {
            Ok(p.models.clone())
        };
    }
    // Merge: keep user-overridden types and enabled flags for ids that
    // already exist.
    let old_by_id: std::collections::HashMap<&str, (crate::config::ModelType, bool)> = p
        .models
        .iter()
        .map(|m| (m.id.as_str(), (m.model_type, m.enabled)))
        .collect();
    p.models = fresh
        .into_iter()
        .map(|m| crate::config::ModelConfig {
            // Keep user-overridden types, but never keep an old Unknown —
            // re-classification (e.g. classifier changes) must take effect.
            model_type: old_by_id
                .get(m.id.as_str())
                .map(|(t, _)| *t)
                .filter(|t| *t != crate::config::ModelType::Unknown)
                .unwrap_or(m.model_type),
            enabled: old_by_id
                .get(m.id.as_str())
                .map(|(_, e)| *e)
                .unwrap_or(false),
            ..m
        })
        .collect();
    let result = p.models.clone();
    save_config(&config)?;
    Ok(result)
}

/// Set (or clear, with empty id) which model is used for a role.
#[tauri::command]
pub fn set_active_model(kind: String, model_id: String) -> Result<(), String> {
    let mut config = load_config();
    let field = match kind.as_str() {
        "embedding" => &mut config.active_embedding_model_id,
        "llm" => &mut config.active_llm_model_id,
        "reranker" => &mut config.active_reranker_model_id,
        _ => return Err(format!("unknown kind: {kind}")),
    };
    let previous = field.clone();
    if !model_id.is_empty() && !model_id.starts_with("local:")
        && let Some((pid, mid)) = model_id.split_once(':')
            && let Some(p) = config.providers.iter_mut().find(|p| p.id == pid)
                && let Some(m) = p.models.iter_mut().find(|m| m.id == mid) {
                    m.enabled = true;
                }
    *field = model_id.clone();
    save_config(&config)?;
    if kind == "embedding" {
        // Switching the embedding model invalidates the loaded local embedder
        // (wrong dimension if the new model differs) and any cached query vectors.
        if previous != model_id {
            crate::ai::local_embed::reset_local_embedder();
            crate::ai::clear_query_embed_cache();
        }
        // Local models are a 3-way choice: keep only the selected dimension on
        // disk. Remote selections leave local models untouched. Only prune when
        // the selected model is actually present, so a stale UI selection can't
        // delete the working model.
        if let Some(keep) = crate::ai::local_embed::local_model_dir_name(&model_id) {
            if crate::ai::local_embed::bge_model_ready(&config.data_dir, keep) {
                crate::commands::bge::prune_local_models_except(&config.data_dir, keep);
            }
        }
    } else if kind == "reranker" {
        // Local rerankers are a single choice too: keep only the selected one.
        if previous != model_id {
            crate::ai::local_rerank::reset_local_reranker();
        }
        if let Some(keep) = crate::ai::local_embed::local_model_dir_name(&model_id) {
            if crate::ai::local_rerank::rerank_model_ready(&config.data_dir, keep) {
                crate::ai::local_rerank::prune_local_rerank_models_except(&config.data_dir, keep);
            }
        }
    }
    Ok(())
}

/// Test a provider's connectivity (GET /models). Returns ok + detail.
#[tauri::command]
pub async fn test_provider(base_url: String, api_key: String) -> Result<ProviderTest, String> {
    if base_url.trim().is_empty() {
        return Err("base_url 不能为空".into());
    }
    let (models, pull_err) = tokio::task::spawn_blocking(move || {
        crate::ai::list_provider_models(&base_url, &api_key)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), Some("测试任务失败".into())));
    match pull_err {
        None => Ok(ProviderTest { ok: true, detail: format!("连通成功，发现 {} 个模型", models.len()) }),
        Some(e) => Ok(ProviderTest { ok: false, detail: e }),
    }
}

#[derive(serde::Serialize)]
pub struct ProviderOutcome {
    pub id: String,
    pub pull_error: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ProviderTest {
    pub ok: bool,
    pub detail: String,
}

#[tauri::command]
pub async fn migrate_data(
    app: AppHandle,
    state: State<'_, AppState>,
    old_path: String,
    new_path: String,
) -> Result<String, String> {
    use std::sync::atomic::Ordering;

    let old = std::path::Path::new(&old_path);
    let new = std::path::Path::new(&new_path);

    if !old.exists() {
        return Err("当前数据目录不存在".to_string());
    }
    // Allow migration to existing directory, but refuse if it already has index data
    if new.join("data.db").exists() {
        return Err("目标目录已包含 data.db，请选择空目录或新目录".to_string());
    }
    if new.join(INDEX_DIR_NAME).exists() {
        return Err("目标目录已包含索引文件夹，请选择空目录或新目录".to_string());
    }
    // Migrating into the old data dir would delete the fresh copy afterwards.
    if crate::commands::helpers::is_within(old, new) {
        return Err("目标目录不能是当前数据目录或其子目录".to_string());
    }

    // Reject when the new data dir would overlap any monitored dir.
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let dirs = db::dir_config::list_dirs(&conn).map_err(|e| format!("{e}"))?;
    drop(conn);
    for dir in &dirs {
        crate::commands::helpers::check_data_dir_overlap(new, Path::new(&dir.path))?;
    }

    // Pause background work so the copy is not raced by a scan or watcher event.
    state.cancel_scan.store(true, Ordering::SeqCst);
    state.is_scanning.store(false, Ordering::SeqCst);
    for dir in &dirs {
        let _ = state.watcher_tx.send(crate::scanner::watcher::WatcherCommand::StopWatch {
            dir_id: dir.id.clone(),
        });
    }

    let app_clone = app.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let old = std::path::Path::new(&old_path);
        let new = std::path::Path::new(&new_path);
        let tmp = new.join(format!(".migrate-tmp-{}", uuid::Uuid::new_v4().simple()));

        let emit = |stage: &str, progress: u32| {
            let _ = app_clone.emit(
                "migration-progress",
                serde_json::json!({ "stage": stage, "progress": progress }),
            );
        };
        // On any failure the tmp dir is removed; the old dir is never touched.
        let cleanup_tmp = || {
            let _ = std::fs::remove_dir_all(&tmp);
        };

        emit("preparing", 0);
        std::fs::create_dir_all(&tmp).map_err(|e| format!("无法创建临时目录: {e}"))?;

        // SQLite via online Backup API — WAL-safe, unlike fs::copy of a live DB.
        // `data.db-wal`/`-shm` are skipped (the snapshot already holds their
        // committed contents).
        emit("db", 5);
        let old_db = old.join("data.db");
        if old_db.exists()
            && let Err(e) = backup_db(&old_db, &tmp.join("data.db")) {
                cleanup_tmp();
                return Err(e);
            }
        emit("db", 15);

        // Copy everything else in the data dir verbatim — local models
        // (`models/`, often >1 GB), chat history, backups, TLS certs, the
        // Tantivy index, app.log. Byte-based progress because of the models.
        let total = tree_bytes(old, true);
        let mut copied = 0u64;
        emit("files", 20);
        if let Err(e) = copy_tree(old, &tmp, true, total, &mut copied, &emit) {
            cleanup_tmp();
            return Err(e);
        }
        emit("files", 80);

        // fsync everything so the rename publishes durable data.
        if let Err(e) = fsync_tree(&tmp) {
            cleanup_tmp();
            return Err(format!("数据落盘失败: {e}"));
        }
        emit("fsync", 85);

        // Atomic rename — tmp lives inside the target dir, so same filesystem.
        std::fs::create_dir_all(new).map_err(|e| format!("无法创建目标目录: {e}"))?;
        let entries = std::fs::read_dir(&tmp).map_err(|e| format!("读取临时目录失败: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("遍历临时目录失败: {e}"))?;
            let name = entry.file_name();
            if let Err(e) = std::fs::rename(entry.path(), new.join(&name)) {
                cleanup_tmp();
                return Err(format!("移动 {name:?} 失败: {e}"));
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        emit("cleanup", 90);

        // Persist the new data dir before touching the old one. Record the old
        // dir as pending cleanup: this process still holds its log/DB handles,
        // so on Windows the delete below is expected to fail. Startup retries
        // once those handles are released.
        let mut loaded = load_config();
        loaded.data_dir = new_path.clone().into();
        loaded.pending_cleanup_dir = Some(old.to_path_buf());
        if let Err(e) = save_config(&loaded) {
            return Err(format!("保存配置失败: {e}"));
        }

        // Best-effort removal now. If it succeeds, clear the pending marker;
        // otherwise leave it for the next startup (no user-facing warning).
        match std::fs::remove_dir_all(old) {
            Ok(()) => {
                log::info!("[MIGRATE] removed old data dir {:?}", old);
                loaded.pending_cleanup_dir = None;
                let _ = save_config(&loaded);
            }
            Err(e) => {
                log::warn!(
                    "[MIGRATE] old data dir {:?} still in use ({e}); will retry at next startup",
                    old
                );
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("迁移任务异常: {e}"))?;
    result?;

    let _ = app.emit("migration-completed", serde_json::json!({ "message": "数据已迁移到新目录" }));
    Ok("数据已迁移到新目录，即将自动重启".to_string())
}

fn backup_db(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    let src_conn = Connection::open(src).map_err(|e| format!("无法打开源数据库: {e}"))?;
    let mut dst_conn = Connection::open(dst).map_err(|e| format!("无法打开目标数据库: {e}"))?;
    let backup = Backup::new(&src_conn, &mut dst_conn)
        .map_err(|e| format!("初始化备份失败: {e}"))?;
    let mut r = backup.step(-1).map_err(|e| format!("备份数据库失败: {e}"))?;
    let mut busy = 0;
    while r == StepResult::Busy || r == StepResult::Locked {
        busy += 1;
        if busy >= 3 {
            return Err("数据库繁忙，迁移未完成，请重试".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        r = backup.step(-1).map_err(|e| format!("备份数据库失败: {e}"))?;
    }
    if r != StepResult::Done {
        return Err("数据库繁忙，迁移未完成，请重试".to_string());
    }
    Ok(())
}

fn fsync_tree(root: &std::path::Path) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            // Windows: FlushFileBuffers 要求句柄带 GENERIC_WRITE，而 File::open
            // 只请求 GENERIC_READ → 用 OpenOptions 显式加写；打不开则跳过
            // （fsync 是持久化优化，非正确性要求）。
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(entry.path()) {
                let _ = f.sync_all();
            }
        }
    }
    Ok(())
}

/// 迁移时跳过的顶层条目：DB 旁路文件（备份 API 已含其内容）与纯可再生的
/// 会话日志、Vision 预热缓存。
fn is_migration_skipped(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_string_lossy().as_ref(),
        "data.db" | "data.db-wal" | "data.db-shm" | "logs" | ".vision_warmup.png"
    )
}

/// 统计迁移需要复制的总字节数（用于进度上报），跳过 [`is_migration_skipped`]。
fn tree_bytes(dir: &std::path::Path, top: bool) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        if top && is_migration_skipped(&entry.file_name()) {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => total += tree_bytes(&entry.path(), false),
            Ok(ft) if ft.is_file() => {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
            _ => {}
        }
    }
    total
}

/// 递归复制 `src` 到 `dst`，跳过顶层 [`is_migration_skipped`] 条目，并按已复制
/// 字节数在 20..80 区间上报进度。
fn copy_tree(
    src: &std::path::Path,
    dst: &std::path::Path,
    top: bool,
    total: u64,
    copied: &mut u64,
    emit: &dyn Fn(&str, u32),
) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("创建目录 {dst:?} 失败: {e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("读取目录 {src:?} 失败: {e}"))? {
        let entry = entry.map_err(|e| format!("遍历 {src:?} 失败: {e}"))?;
        let name = entry.file_name();
        if top && is_migration_skipped(&name) {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| format!("获取类型 {src:?} 失败: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if file_type.is_dir() {
            copy_tree(&src_path, &dst_path, false, total, copied, emit)?;
        } else {
            // Windows 上 Tantivy 索引文件被 reader mmap 锁定、
            // meta.lock 被独占锁定，fs::copy 返回 PermissionDenied。
            // 这些文件是临时性的（锁文件/可重建的段文件），
            // 新位置重新打开索引时自动重建，跳过即可。
            match std::fs::copy(&src_path, &dst_path) {
                Ok(bytes) => {
                    *copied += bytes;
                    if total > 0 {
                        let p = 20 + ((*copied).min(total) * 60 / total) as u32;
                        emit("files", p);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    log::warn!("[MIGRATE] 跳过被锁定的文件 {src_path:?}: {e}");
                }
                Err(e) => {
                    return Err(format!("复制 {src_path:?} -> {dst_path:?} 失败: {e}"));
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_base(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ls_migrate_{tag}_{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn copy_tree_migrates_models_and_chat_but_skips_db_sidecars() {
        let base = tmp_base("copy");
        let src = base.join("old");
        let dst = base.join("tmp");

        // 本地模型（BGE / FunASR）与聊天历史都必须迁移。
        std::fs::create_dir_all(src.join("models").join("bge-small-zh-v1.5")).unwrap();
        std::fs::write(src.join("models").join("bge-small-zh-v1.5").join("model.onnx"), b"onnx").unwrap();
        std::fs::create_dir_all(src.join("models").join("funasr")).unwrap();
        std::fs::write(src.join("models").join("funasr").join("llm.int8.onnx"), b"asr").unwrap();
        std::fs::write(src.join("chat_history.json"), b"[]").unwrap();
        std::fs::write(src.join("app.log"), b"log").unwrap();
        // DB 与旁路文件、可再生目录应被跳过。
        std::fs::write(src.join("data.db"), b"db").unwrap();
        std::fs::write(src.join("data.db-wal"), b"wal").unwrap();
        std::fs::write(src.join("data.db-shm"), b"shm").unwrap();
        std::fs::create_dir_all(src.join("logs")).unwrap();
        std::fs::write(src.join("logs").join("scan.log"), b"x").unwrap();

        let total = tree_bytes(&src, true);
        let mut copied = 0u64;
        let emit = |_: &str, _: u32| {};
        copy_tree(&src, &dst, true, total, &mut copied, &emit).unwrap();

        assert!(dst.join("models/bge-small-zh-v1.5/model.onnx").is_file(), "BGE model must migrate");
        assert!(dst.join("models/funasr/llm.int8.onnx").is_file(), "FunASR model must migrate");
        assert!(dst.join("chat_history.json").is_file(), "chat history must migrate");
        assert!(dst.join("app.log").is_file(), "app.log must migrate");
        assert!(!dst.join("data.db-wal").exists(), "WAL sidecar must be skipped");
        assert!(!dst.join("data.db-shm").exists(), "SHM sidecar must be skipped");
        assert!(!dst.join("data.db").exists(), "DB is copied via backup API, not copy_tree");
        assert!(!dst.join("logs").exists(), "regenerable logs dir must be skipped");
        assert_eq!(copied, total, "progress bytes must match counted total");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tree_bytes_ignores_skipped_entries() {
        let base = tmp_base("bytes");
        let src = base.join("old");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("models.onnx"), vec![0u8; 100]).unwrap();
        std::fs::write(src.join("data.db"), vec![0u8; 999]).unwrap();
        std::fs::write(src.join("data.db-wal"), vec![0u8; 999]).unwrap();

        assert_eq!(tree_bytes(&src, true), 100, "only non-skipped files counted");
        let _ = std::fs::remove_dir_all(&base);
    }
}