//! 飞书集成的**命令层**（桌面设置页用；Web API 侧的同名端点见
//! [`crate::webapi::routes::lark`]）。
//!
//! 三条"必须在这里收口"的理由：
//!
//! 1. **策略只有一份真相来源**：`config.json` 的 `integrations.lark` —— 读写都走
//!    [`crate::config::lark_integration`] / [`crate::config::set_lark_integration`]。
//!    （历史教训：键曾散在 DB `app_settings`，bot 与设置页各读一处，于是"改了没反应"。）
//! 2. **改策略必须重启 bot**：bot 在**启动时**把策略读进内存（`GET /api/integrations/lark`），
//!    改完不重启就等于没改。这条最容易漏，所以收在命令层，而不是指望每个调用方记得。
//! 3. **配对是安全边界**：是否"配对中"由**服务端**按 `pairing_expires_at` 裁决
//!    （[`crate::config::LarkIntegration::pairing_active`]），不是"字段为 true 就算"。

use serde::Serialize;
use tauri::State;

use crate::commands::sessions::lark as lark_store;
use crate::config::LarkIntegration;
use crate::state::AppState;

/// 配对模式的默认时长（`docs/LARK_BOT.md` §7.1 ⑥：默认 5 分钟）。
pub const DEFAULT_PAIRING_MINUTES: i64 = 5;
/// 配对时长上限 —— 长期开着等于没有白名单，所以给个硬顶。
pub const MAX_PAIRING_MINUTES: i64 = 60;

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 飞书集成的**策略 + 运行态**快照：设置页一次拿全，免得自己拼接多处状态。
#[derive(Serialize)]
pub struct LarkStatus {
    pub config: LarkIntegration,
    /// `enabled` **且** bot 进程真的在跑（不只看配置，避免"配了但没起来"被显示成正常）。
    pub running: bool,
    /// 配对**此刻**是否有效（服务端按 `pairing_expires_at` 裁决，不是"字段为 true"）。
    pub pairing_active: bool,
    /// 配对剩余秒数（无效时为 0）。
    pub pairing_seconds_left: i64,
    /// 已绑定的 p2p 会话数（诊断用）。
    pub bindings: usize,
}

#[tauri::command]
pub fn get_lark_config() -> Result<LarkIntegration, String> {
    Ok(crate::config::lark_integration())
}

/// 写策略并**重启 bot**（不重启＝改了不生效）。
///
/// `async`：重启要 spawn/kill 子进程，别占住主线程（AGENTS「async fn 避免阻塞事件循环」）。
#[tauri::command]
pub async fn set_lark_config(lark: LarkIntegration) -> Result<(), String> {
    crate::config::set_lark_integration(&lark)?;
    crate::lark_bot::restart();
    Ok(())
}

#[tauri::command]
pub fn get_lark_status(state: State<'_, AppState>) -> Result<LarkStatus, String> {
    let config = crate::config::lark_integration();
    let now = now_secs();
    // 绑定数只是诊断信息：拿不到连接不该让整个状态接口失败。
    let bindings = state
        .db
        .get()
        .ok()
        .and_then(|conn| lark_store::list_bindings(&conn).ok())
        .map(|list| list.len())
        .unwrap_or(0);
    Ok(LarkStatus {
        pairing_active: config.pairing_active(now),
        pairing_seconds_left: (config.pairing_expires_at - now).max(0),
        running: crate::lark_bot::is_running(),
        bindings,
        config,
    })
}

/// 手动重启 bot（手改 `config.json` 后由 [`crate::config_watch`] 自动触发；这里是兜底入口）。
#[tauri::command]
pub async fn lark_restart() -> Result<(), String> {
    crate::lark_bot::restart();
    Ok(())
}

/// 进入**配对模式**（§7.1 ⑥）：白名单为空时，用一次私聊把自己配进去。
///
/// 必须**先启用**机器人：没启用就没有 bot 去收那条私聊消息，配对永远不会完成。
/// 这里直接拒绝并给出可操作提示 —— 比"开了配对却等不到人"好排查得多。
#[tauri::command]
pub async fn lark_pairing_start(minutes: Option<i64>) -> Result<(), String> {
    let mut lark = crate::config::lark_integration();
    if !lark.enabled {
        return Err("请先启用飞书机器人（enabled = true）再开始配对".into());
    }
    let mins = minutes
        .unwrap_or(DEFAULT_PAIRING_MINUTES)
        .clamp(1, MAX_PAIRING_MINUTES);
    lark.pairing = true;
    lark.pairing_expires_at = now_secs() + mins * 60;
    crate::config::set_lark_integration(&lark)?;
    // 必须重启：闸门配置是 bot **启动时**读的，不重启就不会放行那条配对消息。
    crate::lark_bot::restart();
    log::info!("[LARK] 已进入配对模式（{mins} 分钟）：请用飞书私聊机器人一次");
    Ok(())
}

/// 取消配对模式（用户点"取消"，或超时后手动收尾）。
#[tauri::command]
pub async fn lark_pairing_cancel() -> Result<(), String> {
    let mut lark = crate::config::lark_integration();
    lark.pairing = false;
    lark.pairing_expires_at = 0;
    crate::config::set_lark_integration(&lark)?;
    crate::lark_bot::restart();
    Ok(())
}

/// 已绑定的 p2p 会话（`chat_id` ↔ 本库会话）。
#[tauri::command]
pub fn get_lark_bindings(state: State<'_, AppState>) -> Result<Vec<lark_store::LarkBinding>, String> {
    let conn = state.db.get().map_err(|e| e.to_string())?;
    lark_store::list_bindings(&conn)
}
