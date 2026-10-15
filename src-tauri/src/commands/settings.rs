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