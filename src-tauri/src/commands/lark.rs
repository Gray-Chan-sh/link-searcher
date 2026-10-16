//! 飞书集成的**命令层 + 共享实现**（桌面设置页用；Web API 侧的同名端点见
//! [`crate::webapi::routes::lark`]）。
//!
//! 三条"必须在这里收口"的理由：
//!
//! 1. **策略只有一份真相来源**：`config.json` 的 `integrations.lark` —— 读写都走
//!    [`crate::config::lark_integration`] / [`crate::config::set_lark_integration`]。
//!    （历史教训：键曾散在 DB `app_settings`，bot 与设置页各读一处，于是"改了没反应"。）
//! 2. **改策略必须重启 bot**：bot 在**启动时**把策略读进内存（`GET /api/integrations/lark`），
//!    改完不重启就等于没改。这条最容易漏，所以收在这里，而不是指望每个调用方记得。
//! 3. **配对是安全边界**：是否"配对中"由**服务端**按 `pairing_expires_at` 裁决
//!    （[`crate::config::LarkIntegration::pairing_active`]），不是"字段为 true 就算"。
//!
//! 为什么不是清一色 `#[tauri::command]`：同一套动作**桌面与 Web 两种传输都要用**
//! （`webapi/routes/lark.rs` 直接调这里的 `pub fn`）。实现写成普通函数、命令只做薄包装，
//! 逻辑只写一份 —— 也不会出现"桌面能用、Web 端行为不同"这种最难查的分叉。
//!
//! ⚠️ **写成功后必须广播 `config-changed`**：`config_watch` 只对**外部**改动发事件，
//! 本进程的写会被自写抑制挡掉（见 [`announce_config_changed`]）。

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

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

/// 把请求的配对时长夹到合法区间（**纯函数**，便于单测）。
pub fn pairing_minutes(requested: Option<i64>) -> i64 {
    requested
        .unwrap_or(DEFAULT_PAIRING_MINUTES)
        .clamp(1, MAX_PAIRING_MINUTES)
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

/// 配对消费的**核心规则**（纯函数 —— 这条安全边界只有这一份实现）。
///
/// - 校验 `pairing_active(now)`；**不通过就 `Err`，调用方绝不写盘**
///   （否则这里就成了"凭一个 token 往白名单里塞任意 `open_id`"的口子，等于绕过 L1）
/// - 通过：去重追加 `open_id`，并**关闭配对**（配对是一次性的，留着等于没有白名单）
pub fn consume_pairing_into(
    lark: &mut LarkIntegration,
    open_id: &str,
    now: i64,
) -> Result<(), String> {
    let open_id = open_id.trim();
    if open_id.is_empty() {
        return Err("open_id is required".into());
    }
    if !lark.pairing_active(now) {
        return Err("pairing is not active（未开启或已过期）".into());
    }
    if !lark.allowed_users.iter().any(|u| u == open_id) {
        lark.allowed_users.push(open_id.to_string());
    }
    lark.pairing = false;
    lark.pairing_expires_at = 0;
    Ok(())
}

// ───────────────────────── 共享实现（命令与端点都调它） ─────────────────────────

/// 本进程写成功且**内容真的变了**之后广播 `config-changed`（`source = "local"`）。
///
/// 为什么必须补这一发（2026-10-16 真机验收发现）：
/// `config_watch` 的 `config-changed` **只在外部改动时**发出，而本进程的写会被
/// "自写抑制"挡掉（`config.rs::write_config_file` → `note_known`）。于是
/// 「bot 配对消费」「远程/CLI 改策略」这些**经本进程**的改动谁都不通知界面 ——
/// 设置页会一直显示旧值（配对还显示"配对中"、白名单缺新配的用户）。
///
/// 事件名与 payload 形状与 [`crate::config_watch`] 保持一致，只有 `source` 不同。
fn announce_config_changed(app: &AppHandle) {
    if let Err(e) = app.emit(
        crate::config_watch::CONFIG_CHANGED_EVENT,
        serde_json::json!({ "source": "local" }),
    ) {
        log::debug!("[LARK] 广播 {} 失败: {e}", crate::config_watch::CONFIG_CHANGED_EVENT);
    }
}

/// 读策略 + 运行态快照。
pub fn status_of(state: &AppState) -> Result<LarkStatus, String> {
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

/// 已绑定的 p2p 会话（`chat_id` ↔ 本库会话）。
pub fn bindings_of(state: &AppState) -> Result<Vec<lark_store::LarkBinding>, String> {
    let conn = state.db.get().map_err(|e| e.to_string())?;
    lark_store::list_bindings(&conn)
}

/// 写策略并**重启 bot**（不重启＝改了不生效）。
pub fn save_config(app: &AppHandle, lark: &LarkIntegration) -> Result<(), String> {
    if crate::config::set_lark_integration(lark)? {
        announce_config_changed(app);
    }
    crate::lark_bot::restart();
    Ok(())
}

/// **配对消费**：把发送者写入白名单、自动关闭配对、重启 bot 并广播。
///
/// 校验只有一份实现（[`consume_pairing_into`]）：调用方**不得**自己判一遍
/// （漏判＝白名单后门，重复判＝两套规则漂移）。返回写入后的白名单，供端点回显。
pub fn pairing_consume(app: &AppHandle, open_id: &str) -> Result<Vec<String>, String> {
    let now = now_secs();
    let mut lark = crate::config::lark_integration();
    consume_pairing_into(&mut lark, open_id, now)?;
    crate::config::set_lark_integration(&lark)?;
    // 白名单变了 → 让 bot 重读，否则新配的用户仍会被拒。
    crate::lark_bot::restart();
    announce_config_changed(app);
    log::info!(
        "[LARK] 配对成功：{} 已加入白名单，配对模式自动关闭",
        open_id.trim()
    );
    Ok(crate::config::lark_integration().allowed_users)
}

/// 手动重启 bot（手改 `config.json` 后由 [`crate::config_watch`] 自动触发；这里是兜底入口）。
pub fn restart() {
    crate::lark_bot::restart();
}

/// 进入**配对模式**（§7.1 ⑥）：白名单为空时，用一次私聊把自己配进去。
///
/// 必须**先启用**机器人：没启用就没有 bot 去收那条私聊消息，配对永远不会完成。
/// 这里直接拒绝并给出可操作提示 —— 比"开了配对却等不到人"好排查得多。
pub fn pairing_start(app: &AppHandle, minutes: Option<i64>) -> Result<(), String> {
    let mut lark = crate::config::lark_integration();
    if !lark.enabled {
        return Err("请先启用飞书机器人（enabled = true）再开始配对".into());
    }
    let mins = pairing_minutes(minutes);
    lark.pairing = true;
    lark.pairing_expires_at = now_secs() + mins * 60;
    if crate::config::set_lark_integration(&lark)? {
        announce_config_changed(app);
    }
    // 必须重启：闸门配置是 bot **启动时**读的，不重启就不会放行那条配对消息。
    crate::lark_bot::restart();
    log::info!("[LARK] 已进入配对模式（{mins} 分钟）：请用飞书私聊机器人一次");
    Ok(())
}

/// 取消配对模式（用户点"取消"，或超时后手动收尾）。
pub fn pairing_cancel(app: &AppHandle) -> Result<(), String> {
    let mut lark = crate::config::lark_integration();
    lark.pairing = false;
    lark.pairing_expires_at = 0;
    if crate::config::set_lark_integration(&lark)? {
        announce_config_changed(app);
    }
    crate::lark_bot::restart();
    Ok(())
}

// ───────────────────────────── Tauri 命令（薄包装） ─────────────────────────────

#[tauri::command]
pub fn get_lark_config() -> Result<LarkIntegration, String> {
    Ok(crate::config::lark_integration())
}

/// 写策略并**重启 bot**（不重启＝改了不生效）。
///
/// `async`：重启要 spawn/kill 子进程，别占住主线程（AGENTS「async fn 避免阻塞事件循环」）。
/// `app` 由 Tauri 注入 —— 写成功后要靠它广播 `config-changed`（见 [`announce_config_changed`]）。
#[tauri::command]
pub async fn set_lark_config(app: AppHandle, lark: LarkIntegration) -> Result<(), String> {
    save_config(&app, &lark)
}

#[tauri::command]
pub fn get_lark_status(state: State<'_, AppState>) -> Result<LarkStatus, String> {
    status_of(state.inner())
}

#[tauri::command]
pub async fn lark_restart() -> Result<(), String> {
    restart();
    Ok(())
}

#[tauri::command]
pub async fn lark_pairing_start(app: AppHandle, minutes: Option<i64>) -> Result<(), String> {
    pairing_start(&app, minutes)
}

#[tauri::command]
pub async fn lark_pairing_cancel(app: AppHandle) -> Result<(), String> {
    pairing_cancel(&app)
}

#[tauri::command]
pub fn get_lark_bindings(
    state: State<'_, AppState>,
) -> Result<Vec<lark_store::LarkBinding>, String> {
    bindings_of(state.inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lark_pairing_active() -> LarkIntegration {
        LarkIntegration {
            enabled: true,
            pairing: true,
            pairing_expires_at: 1_000,
            ..LarkIntegration::default()
        }
    }

    /// 配对时长默认 5 分钟，且被夹在 `1..=60`（长期开着等于没有白名单）。
    #[test]
    fn pairing_minutes_is_clamped() {
        assert_eq!(pairing_minutes(None), DEFAULT_PAIRING_MINUTES);
        assert_eq!(pairing_minutes(Some(0)), 1);
        assert_eq!(pairing_minutes(Some(-9)), 1);
        assert_eq!(pairing_minutes(Some(10)), 10);
        assert_eq!(pairing_minutes(Some(9_999)), MAX_PAIRING_MINUTES);
    }

    /// **安全边界**：配对没开 / 已过期 → 一律 `Err`，且**不得改动配置**。
    #[test]
    fn consume_pairing_refuses_when_pairing_is_not_active() {
        // 没开配对
        let mut off = LarkIntegration::default();
        let before = off.clone();
        assert!(consume_pairing_into(&mut off, "ou_x", 500).is_err());
        assert_eq!(off, before, "拒绝时不得改动任何字段");

        // 开着但已过期
        let mut expired = lark_pairing_active();
        expired.pairing_expires_at = 999;
        let before = expired.clone();
        assert!(consume_pairing_into(&mut expired, "ou_x", 1_000).is_err());
        assert_eq!(expired, before);

        // 开着但截止时刻为 0（fail-closed：漏设＝没开）
        let mut zero = lark_pairing_active();
        zero.pairing_expires_at = 0;
        assert!(consume_pairing_into(&mut zero, "ou_x", 1_000).is_err());

        // 空白 open_id
        let mut active = lark_pairing_active();
        assert!(consume_pairing_into(&mut active, "   ", 500).is_err());
    }

    /// 配对有效时：写白名单（去重、去空白）+ **自动关闭配对**（一次性）。
    #[test]
    fn consume_pairing_adds_sender_and_closes_pairing() {
        let mut lark = lark_pairing_active();
        lark.allowed_users = vec!["ou_me".into()];

        consume_pairing_into(&mut lark, "  ou_new  ", 500).expect("active pairing must be consumed");
        assert_eq!(lark.allowed_users, vec!["ou_me".to_string(), "ou_new".to_string()]);
        assert!(!lark.pairing, "消费即关闭：留着等于没有白名单");
        assert_eq!(lark.pairing_expires_at, 0);

        // 第二次消费（配对已关）必须被拒 —— 这条路径不能被重复利用。
        assert!(consume_pairing_into(&mut lark, "ou_other", 500).is_err());
        assert_eq!(lark.allowed_users.len(), 2, "被拒时不得再写白名单");

        // 已在白名单里的人再配对一次不会产生重复项。
        let mut again = lark_pairing_active();
        again.allowed_users = vec!["ou_me".into()];
        consume_pairing_into(&mut again, "ou_me", 500).expect("consume");
        assert_eq!(again.allowed_users, vec!["ou_me".to_string()]);
    }
}
