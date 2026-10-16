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
    // 飞书机器人策略**不在此**：它已迁到 `config.json` 的 `integrations.lark`
    // （见 [`migrate_legacy_lark_settings`] 与 `docs/LARK_BOT.md` §5.6）。
    // 这里刻意不保留 `lark_*` 直通键 —— bot 只读 config.json，写进 DB 也没人读，
    // 那种"改了没反应"的静默失败正是本文件历史上踩过的坑。
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

/// 遗留的飞书策略键（曾经平铺在 `app_settings` 里，现已迁到 `config.json`）。
const LEGACY_LARK_KEYS: &[&str] = &[
    "lark_bot_enabled",
    "lark_require_p2p",
    "lark_allowed_users",
    "lark_allowed_chats",
    "lark_export_allow_dirs",
    "lark_pairing",
];

/// 纯函数：把遗留键的读取结果套到当前配置上，返回迁移后的值。
/// **一个遗留键都没有**时返回 `None`（表示无需迁移、保持 config.json 现状）。
///
/// 拆成纯函数是为了可单测：真实迁移会写 `config.json`（真实用户目录），
/// 不适合在单测里跑。
fn legacy_lark_into<F>(mut lark: crate::config::LarkIntegration, read: F) -> Option<crate::config::LarkIntegration>
where
    F: Fn(&str) -> Option<String>,
{
    let mut any = false;
    if let Some(v) = read("lark_bot_enabled") {
        lark.enabled = v.eq_ignore_ascii_case("true");
        any = true;
    }
    if let Some(v) = read("lark_require_p2p") {
        // 与 lark-gate 同取向：只有**显式** "false" 才放宽，其余（缺失/拼错）都保持要求私聊。
        lark.require_p2p = !v.eq_ignore_ascii_case("false");
        any = true;
    }
    if let Some(v) = read("lark_allowed_users") {
        lark.allowed_users = lark_gate::parse_allowed_users(&v);
        any = true;
    }
    if let Some(v) = read("lark_allowed_chats") {
        lark.allowed_chats = lark_gate::parse_allowed_users(&v);
        any = true;
    }
    if let Some(v) = read("lark_export_allow_dirs") {
        lark.export_allow_dirs = lark_gate::parse_allowed_users(&v);
        any = true;
    }
    if let Some(v) = read("lark_pairing") {
        lark.pairing = v.eq_ignore_ascii_case("true");
        any = true;
    }
    any.then_some(lark)
}

/// 把 DB `app_settings` 里遗留的飞书策略**一次性**搬进 `config.json` 的
/// `integrations.lark`，然后删掉这些 DB 行并留下 `lark_config_migrated` 标记。
///
/// 为什么要搬：飞书策略原先散在 DB 里，与 `config.json` 的模型/AI 配置割裂，
/// 备份/审阅/回滚都拿不到一起（`docs/LARK_BOT.md` §5.6 存储收口）。
///
/// **幂等**，且顺序刻意是「先写 config → 再置标记 → 最后删行」：
/// 任何一步崩溃后再启动都得到**同样的结果**，不会把用户之后在设置页改的值
/// 用旧的 DB 值覆盖回去。
pub fn migrate_legacy_lark_settings(conn: &rusqlite::Connection) {
    let read = |k: &str| -> Option<String> {
        conn.query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![k],
            |r| r.get::<_, String>(0),
        )
        .ok()
    };

    if read("lark_config_migrated").as_deref() == Some("1") {
        return;
    }

    if let Some(lark) = legacy_lark_into(crate::config::lark_integration(), &read) {
        match crate::config::set_lark_integration(&lark) {
            Ok(()) => log::info!("[CONFIG] 已把遗留飞书策略迁移进 config.json 的 integrations.lark"),
            Err(e) => {
                // 保留 DB 行，下次启动重试；绝不置标记。
                log::error!("[CONFIG] 迁移飞书策略失败（保留 DB 行下次重试）: {e}");
                return;
            }
        }
    }

    // 置标记：即使没有遗留键也置，避免每次启动都做无谓查找。
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES ('lark_config_migrated', '1')",
        [],
    ) {
        log::warn!("[CONFIG] 写入 lark_config_migrated 标记失败: {e}");
        return;
    }
    for k in LEGACY_LARK_KEYS {
        if let Err(e) = conn.execute("DELETE FROM app_settings WHERE key = ?1", rusqlite::params![k]) {
            log::warn!("[CONFIG] 清理遗留飞书键 '{k}' 失败: {e}");
        }
    }
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

    /// 飞书策略键**已退役**：它们不再是 `app_settings` 的可写键，而是
    /// `config.json` 的 `integrations.lark`（存储收口）。
    ///
    /// 留这条测试是为了挡住"顺手加回 `ALLOWED_KEYS`" —— 那会写到**没人读**的键上
    /// （bot 只读 config.json），表现为"改了没反应"，正是本模块踩过的静默失败。
    #[test]
    fn legacy_lark_keys_are_retired_from_allowed_keys() {
        for key in LEGACY_LARK_KEYS {
            assert!(
                !ALLOWED_KEYS.contains(key),
                "'{key}' 已迁到 config.json 的 integrations.lark，不应再出现在 ALLOWED_KEYS"
            );
        }
    }

    /// 迁移映射：显式 "false" 才放宽私聊要求；空串名单解析成空列表（fail-closed）。
    #[test]
    fn legacy_lark_mapping_follows_fail_closed() {
        let from = |pairs: Vec<(&str, &str)>| {
            let owned: Vec<(String, String)> =
                pairs.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            move |k: &str| owned.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone())
        };

        // 无遗留键 → 不迁移。
        assert!(legacy_lark_into(crate::config::LarkIntegration::default(), from(vec![])).is_none());

        // 只有显式 false 才关掉"要求私聊"。
        let l = legacy_lark_into(
            crate::config::LarkIntegration::default(),
            from(vec![("lark_require_p2p", "false")]),
        )
        .expect("有遗留键就应返回");
        assert!(!l.require_p2p, "显式 false 才放宽");

        let l = legacy_lark_into(
            crate::config::LarkIntegration::default(),
            from(vec![("lark_require_p2p", "maybe")]),
        )
        .expect("有遗留键就应返回");
        assert!(l.require_p2p, "拼错/非 false 一律保持要求私聊");

        // 空串名单 → 空列表（拒绝全部），而不是报错。
        let l = legacy_lark_into(
            crate::config::LarkIntegration::default(),
            from(vec![("lark_allowed_users", ""), ("lark_bot_enabled", "true")]),
        )
        .expect("有遗留键就应返回");
        assert!(l.allowed_users.is_empty());
        assert!(l.enabled);

        // 正常 JSON 数组照常解析。
        let l = legacy_lark_into(
            crate::config::LarkIntegration::default(),
            from(vec![("lark_allowed_users", r#"["ou_a","ou_b"]"#)]),
        )
        .expect("有遗留键就应返回");
        assert_eq!(l.allowed_users, vec!["ou_a".to_string(), "ou_b".to_string()]);

        // 只覆盖出现的键：未出现的保持传入的当前配置。
        let current = crate::config::LarkIntegration {
            allowed_users: vec!["ou_keep".into()],
            ..Default::default()
        };
        let l = legacy_lark_into(current, from(vec![("lark_bot_enabled", "true")]))
            .expect("有遗留键就应返回");
        assert_eq!(l.allowed_users, vec!["ou_keep".to_string()]);
    }
}