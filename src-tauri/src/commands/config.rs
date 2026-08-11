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
    pub lo_binary_path: String,
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
}

#[tauri::command]
pub fn get_config() -> Result<ConfigInfo, String> {
    let config = load_config();
    Ok(ConfigInfo {
        data_dir: config.data_dir.to_string_lossy().to_string(),
        language: config.language,
        lo_binary_path: config.lo_binary_path,
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
        lo_binary_path: new_config.lo_binary_path,
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
        models,
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
    // Merge: keep user-overridden types for ids that already exist.
    let old_by_id: std::collections::HashMap<&str, crate::config::ModelType> = p
        .models
        .iter()
        .map(|m| (m.id.as_str(), m.model_type))
        .collect();
    p.models = fresh
        .into_iter()
        .map(|m| crate::config::ModelConfig {
            // Keep user-overridden types, but never keep an old Unknown —
            // re-classification (e.g. classifier changes) must take effect.
            model_type: old_by_id
                .get(m.id.as_str())
                .copied()
                .filter(|t| *t != crate::config::ModelType::Unknown)
                .unwrap_or(m.model_type),
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
        _ => return Err(format!("unknown kind: {kind}")),
    };
    *field = model_id;
    save_config(&config)
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
        emit("db", 15);
        let old_db = old.join("data.db");
        if old_db.exists() {
            if let Err(e) = backup_db(&old_db, &tmp.join("data.db")) {
                cleanup_tmp();
                return Err(e);
            }
        }
        emit("db", 30);

        let old_index = old.join(INDEX_DIR_NAME);
        if old_index.exists() {
            if let Err(e) = copy_dir_recursive(&old_index, &tmp.join(INDEX_DIR_NAME)) {
                cleanup_tmp();
                return Err(e);
            }
        }
        emit("index", 55);

        let old_log = old.join("app.log");
        if old_log.exists() {
            let _ = std::fs::copy(&old_log, &tmp.join("app.log"));
        }
        emit("log", 75);

        // fsync everything so the rename publishes durable data.
        if let Err(e) = fsync_tree(&tmp) {
            cleanup_tmp();
            return Err(format!("数据落盘失败: {e}"));
        }

        // Atomic rename — tmp lives inside the target dir, so same filesystem.
        std::fs::create_dir_all(new).map_err(|e| format!("无法创建目标目录: {e}"))?;
        for name in ["data.db", INDEX_DIR_NAME, "app.log"] {
            let src = tmp.join(name);
            if src.exists() {
                if let Err(e) = std::fs::rename(&src, new.join(name)) {
                    cleanup_tmp();
                    return Err(format!("移动 {name} 失败: {e}"));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        emit("cleanup", 90);

        // Persist the new data dir before removing the old one.
        let mut loaded = load_config();
        loaded.data_dir = new_path.clone().into();
        if let Err(e) = save_config(&loaded) {
            return Err(format!("保存配置失败: {e}"));
        }

        // Best-effort removal of the old data dir — failure is a warning only.
        if let Err(e) = std::fs::remove_dir_all(old) {
            let _ = app_clone.emit(
                "migration-warning",
                serde_json::json!({
                    "message": format!("旧数据目录删除失败，请手动清理: {}", old.display())
                }),
            );
            log::warn!("[MIGRATE] failed to remove old data dir {:?}: {e}", old);
        } else {
            log::info!("[MIGRATE] removed old data dir {:?}", old);
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
            std::fs::File::open(entry.path())?.sync_all()?;
        }
    }
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("{e}"))? {
        let entry = entry.map_err(|e| format!("{e}"))?;
        let file_type = entry.file_type().map_err(|e| format!("{e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| format!("{e}"))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}