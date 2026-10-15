use std::collections::HashMap;

use tauri::State;

use crate::state::AppState;

/// Whitelisted keys that may be modified through [`update_settings`].
const ALLOWED_KEYS: &[&str] = &[
    "ocr_engine",
    "ocr_lang",
    "ocr_pdf_dpi",
    "ocr_stall_timeout_secs",
    // 自适应并发（内存压力降级/升级）
    "adaptive_load_enabled",
    "load_process_soft_pct",
    "load_process_hard_pct",
    "load_process_pause_pct",
    "load_system_soft_pct",
    "load_system_hard_pct",
    "load_system_pause_pct",
    "load_soft_page_window",
    "load_soft_ocr_permits",
    "load_soft_doc_permits",
    "load_hard_page_window",
    "load_hard_ocr_permits",
    "load_hard_doc_permits",
    "load_sample_secs",
    "load_upgrade_samples",
    "load_recovery_margin_pct",
    "max_results",
    "exclude_patterns",
    "scan_time",
    "auto_backup",
    "backup_interval",
    "auto_start",
    "web_api_enabled",
    "web_api_port",
    "web_api_token",
    "web_api_bind",
    "web_api_dev_mode",
    "web_session_timeout_secs",
    "perf_tier",
    "perf_batch_io_concurrency",
    "perf_commit_interval",
    "perf_writer_buffer_mb",
    "embed_parallelism",
    "embed_adaptive",
    // 飞书机器人（`docs/LARK_BOT.md` §7.1 ⑤）。
    // ⚠️ `lark_allowed_users` / `lark_allowed_chats` 的**空值语义是"拒绝全部"**（fail-closed），
    //    而 `lark_export_allow_dirs` 的空值是"不限制"（fail-open）—— 方向相反，别"顺手统一"。
    "lark_bot_enabled",
    "lark_require_p2p",
    "lark_allowed_users",
    "lark_allowed_chats",
    "lark_export_allow_dirs",
    "lark_pairing",
];

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM app_settings")
        .map_err(|e| format!("query error: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("query error: {e}"))?;
    let mut map = HashMap::new();
    for row in rows {
        let (k, v) = row.map_err(|e| format!("read error: {e}"))?;
        map.insert(k, v);
    }
    Ok(map)
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    settings: HashMap<String, String>,
) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    for (key, value) in &settings {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown setting key: {key}"));
        }
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .map_err(|e| format!("failed to update setting '{key}': {e}"))?;
    }
    drop(conn);
    // token 双端（设置页 / Web 端走同一实现）可改：改了就同步进 config.json
    // （token 的持久化来源）并作废当前 Web 会话——持旧 token 的客户端会在下
    // 一次请求收到 401，本机（同 IP）下一次请求则重新建立会话。
    if let Some(token) = settings.get("web_api_token")
        && !token.trim().is_empty()
    {
        crate::config::set_web_api_token(token.trim());
        if crate::webapi::session::clear(None) {
            log::info!("[WEBAPI-SESSION] token changed → session reset");
        }
    }
    Ok(())
}

#[tauri::command]
pub fn get_version() -> serde_json::Value {
    serde_json::json!({
        "hash": env!("GIT_VERSION"),
        "time": env!("GIT_COMMIT_TIME"),
    })
}

/// 开机自启的真实状态（**来自操作系统**，不是我们的设置项）。
#[derive(serde::Serialize)]
pub struct AutostartState {
    /// OS 里当前是否已登记自启。
    pub enabled: bool,
    /// 本进程实际使用的数据目录。
    pub data_dir: String,
    /// 非 `None` 表示"自启**无法**复现当前数据目录"的原因，供 UI 提示。
    pub warning: Option<String>,
}

/// 组装自启状态。`effective` 由调用方给出（避免这里再取一次 state）。
fn autostart_state_with(app: &tauri::AppHandle, effective: std::path::PathBuf) -> Result<AutostartState, String> {
    use tauri_plugin_autostart::ManagerExt as _;

    let enabled = app.autolaunch().is_enabled().map_err(|e| e.to_string())?;

    // 自启是**无参数**启动应用的（插件 API 不接受参数），因此它读的是 `config.json` 里的
    // data_dir。若当前进程是用 `--data-dir` 启动的，两者会不同 —— 那时自启会指向**另一个库**，
    // 必须显式告知用户，而不是默默失败。
    let from_config = crate::config::load_config().data_dir;
    let warning = if from_config == effective {
        None
    } else {
        Some(format!(
            "自启以无参数启动应用，会使用 config.json 里的数据目录（{}），而当前进程用的是 {}。\
             若你要自启到当前这个库，请把 config.json 的 data_dir 改成它。",
            from_config.display(),
            effective.display()
        ))
    };
    Ok(AutostartState {
        enabled,
        data_dir: effective.display().to_string(),
        warning,
    })
}

/// 查询自启状态（含"能否复现当前 data_dir"的提示）。
#[tauri::command]
pub fn autostart_state(app: tauri::AppHandle) -> Result<AutostartState, String> {
    use tauri::Manager as _;
    let data_dir = app.state::<AppState>().data_dir.clone();
    autostart_state_with(&app, data_dir)
}

/// 打开/关闭开机自启。
#[tauri::command]
pub fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<AutostartState, String> {
    use tauri::Manager as _;
    use tauri_plugin_autostart::ManagerExt as _;

    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())?;
    } else {
        manager.disable().map_err(|e| e.to_string())?;
    }
    let data_dir = app.state::<AppState>().data_dir.clone();
    let state = autostart_state_with(&app, data_dir)?;
    log::info!("[AUTOSTART] set enabled={enabled} -> 实际 {}", state.enabled);
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_keys_contains_ocr_pdf_dpi() {
        assert!(
            ALLOWED_KEYS.contains(&"ocr_pdf_dpi"),
            "ALLOWED_KEYS must contain 'ocr_pdf_dpi'"
        );
    }

    /// 飞书策略键必须可通过设置接口写入 —— 否则 bot 侧读到的永远是默认值，
    /// 表现为"机器人不响应任何消息"（2026-10-15 E2E 实测：`unknown setting key`）。
    #[test]
    fn allowed_keys_contain_lark_policy() {
        for key in [
            "lark_bot_enabled",
            "lark_require_p2p",
            "lark_allowed_users",
            "lark_allowed_chats",
            "lark_export_allow_dirs",
            "lark_pairing",
        ] {
            assert!(ALLOWED_KEYS.contains(&key), "ALLOWED_KEYS 缺少 '{key}'");
        }
    }
}