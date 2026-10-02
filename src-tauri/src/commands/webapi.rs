//! 桌面端 Web API 辅助命令：查看/踢出 Web 端单用户会话。
//!
//! 与 Web 端共用同一份全局会话状态（[`crate::webapi::session`]），
//! 因此桌面端能看到「谁连着」并强制断开——这正是单用户模式下
//! 桌面端保留的管理权：踢掉占用者后新用户即可登入。

use tauri::State;

use crate::state::AppState;
use crate::webapi::session;

/// 当前 Web 会话状态 + 局域网访问地址（无人连接时 `active: false，
/// 但 `lan_ip`/`port`/`url` 始终返回，供设置页展示）。
#[tauri::command]
pub fn web_session_status(state: State<'_, AppState>) -> serde_json::Value {
    session::status_json(&state, None)
}

/// 强制踢出 Web 端当前会话（设置页「强制退出」按钮）。
/// 返回 `true` 表示确实有会话被清除。
#[tauri::command]
pub fn web_session_logout() -> bool {
    let kicked = session::clear(None);
    if kicked {
        log::info!("[WEBAPI-SESSION] session force-logged-out from desktop");
    }
    kicked
}
