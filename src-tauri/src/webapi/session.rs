//! Web 会话状态（**仅展示，不再排他**）—— `docs/LARK_BOT.md` D1 / 阶段四。
//!
//! **历史**：这里曾经是"同一时刻全网只允许一个活跃 Web 会话"的**单写入者闸门**，按来源 IP
//! 识别身份、异 IP 直接 **403**。实测发现它与"个人助手 · 多端"的定位**直接冲突**：
//! 机器人从 `127.0.0.1` 出去，而**任何**通过鉴权的请求都会续租 → 机器人会**持续霸占**会话，
//! 把手机浏览器长期挡在 403；反过来手机持有租约时，机器人自己也会被 403（**机器人直接不答**）。
//!
//! **D1（2026-10-15）已定＝放弃 IP 排他**，本模块因此**降级为"仅展示在线状态"**：
//!
//! - 通过 token 的请求**一律放行**（**不再有 403**），并刷新"最近活跃来源"；
//! - 多来源同时在线 → [`GuardOutcome::Shared`]（**只用于日志/展示，不阻断**）；
//! - `GET /api/session`、`/ping`、`/logout`、桌面"强制踢出"**保留**，但语义已从"权限"
//!   变成"查看 / 清空在线记录"。
//!
//! ⚠️ **安全模型变化（R2：必须显式记住，不默默丢失）**：token 现在是**唯一**门槛。
//! 这与既定定位（个人助手、同机 `127.0.0.1` + 局域网站内使用）相符；
//! 若将来要把 webapi 对外暴露，**先重新设计鉴权**，不要指望本模块提供保护。
//!
//! 数据侧的一致性并不依赖它：聊天历史有锁、扫描走 `is_scanning` CAS、
//! AI 取消已按会话隔离（阶段一）。

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use tauri::Manager;

use crate::state::AppState;
use crate::webapi::state::ApiState;

/// 会话空闲超时（秒），超时后其它 IP 可登入。可在设置中覆盖。
pub const KEY_TIMEOUT_SECS: &str = "web_session_timeout_secs";
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, serde::Serialize)]
pub struct WebSession {
    /// 持有会话的来源 IP（已规范化，见 [`normalize_ip`]）。
    pub ip: String,
    /// 会话建立时间（Unix 秒）。
    pub created_at: i64,
    /// 最后一次通过鉴权的请求时间（Unix 秒），任何通过的请求都刷新它。
    pub last_seen: i64,
}

/// 请求方 IP，由 [`session_guard`] 写入 request extensions，供处理器读取。
#[derive(Clone, Debug)]
pub struct ClientIp(pub String);

fn session_slot() -> &'static Mutex<Option<WebSession>> {
    static SLOT: OnceLock<Mutex<Option<WebSession>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn lock_slot() -> MutexGuard<'static, Option<WebSession>> {
    // Poisoned lock: a previous panic while holding it left no partially
    // written state (the value is only swapped under the lock), recover.
    session_slot().lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 规范化来源 IP：去掉 IPv4-mapped 前缀，loopback 统一为 `127.0.0.1`，
/// 否则同一台机器的 `::ffff:192.168.1.5` / `192.168.1.5` / `::1` 会被当成不同用户。
pub fn normalize_ip(raw: &str) -> String {
    let ip = raw.trim();
    let v4mapped = ip
        .strip_prefix("::ffff:")
        .map(|s| s.to_string())
        .unwrap_or_else(|| ip.to_string());
    let is_loopback = v4mapped == "::1"
        || v4mapped == "localhost"
        || v4mapped.starts_with("127.");
    if is_loopback {
        "127.0.0.1".to_string()
    } else {
        v4mapped
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum GuardOutcome {
    /// 无会话或已超时 → 已为该 IP 建立新会话。
    Granted,
    /// 同 IP → 已续租。
    Renewed,
    /// **另一来源 IP 接入**：阶段四起**不再拒绝**，只记录"多来源同时在线"。
    ///
    /// 故意保留成一个独立变体（而不是并入 `Renewed`）：这样"多端同时在用"这件事在
    /// 类型与日志里都**看得见** —— 放弃排他是有意的决策，不该被静默成"什么都没发生"。
    Shared { previous_ip: String },
}

/// 核心判定（纯函数式入口，`now` 可注入便于测试）。
fn guard_at(ip: &str, timeout_secs: u64, now: i64) -> GuardOutcome {
    let mut slot = lock_slot();
    let timeout = timeout_secs.max(1) as i64;
    match slot.as_mut() {
        None => {
            *slot = Some(WebSession { ip: ip.to_string(), created_at: now, last_seen: now });
            GuardOutcome::Granted
        }
        Some(s) if now - s.last_seen > timeout => {
            // 空闲超时 → 原会话作废，登入者接管。
            *s = WebSession { ip: ip.to_string(), created_at: now, last_seen: now };
            GuardOutcome::Granted
        }
        Some(s) if s.ip == ip => {
            s.last_seen = now;
            GuardOutcome::Renewed
        }
        Some(s) => {
            // 阶段四（D1＝放弃 IP 排他）：另一来源接入**不再拒绝**，只把"最近活跃来源"
            // 更新为它并记录一次共享。`created_at` 保持不变（那是"会话起点"）。
            let previous_ip = s.ip.clone();
            s.ip = ip.to_string();
            s.last_seen = now;
            GuardOutcome::Shared { previous_ip }
        }
    }
}

/// 判定 + 续租（真实时钟）。
pub fn guard(ip: &str, timeout_secs: u64) -> GuardOutcome {
    guard_at(ip, timeout_secs, now_ts())
}

/// 当前会话快照。
pub fn current() -> Option<WebSession> {
    lock_slot().clone()
}

/// 清空会话。`Some(ip)` 仅当会话属于该 IP 时清除（web 端主动退出）；
/// `None` 无条件清除（桌面端强制踢出 / token 轮换）。
pub fn clear(only_ip: Option<&str>) -> bool {
    let mut slot = lock_slot();
    match (slot.as_ref(), only_ip) {
        (None, _) => false,
        (Some(_), None) => {
            *slot = None;
            true
        }
        (Some(s), Some(ip)) if s.ip == ip => {
            *slot = None;
            true
        }
        _ => false,
    }
}

/// 读取超时配置（秒）。DB 中无记录或非法时用默认值。
pub fn timeout_from_db(app_state: &AppState) -> u64 {
    app_state
        .db
        .get()
        .ok()
        .and_then(|conn| {
            conn.query_row::<String, _, _>(
                "SELECT value FROM app_settings WHERE key = ?1",
                rusqlite::params![KEY_TIMEOUT_SECS],
                |r| r.get(0),
            )
            .ok()
        })
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

/// 会话状态 JSON（桌面命令与 Web 路由共用）。
/// `client_ip: Some(ip)` 时附带 `is_owner`，供 web 端判断自己是否在线。
pub fn status_json(
    app_state: &AppState,
    client_ip: Option<&str>,
) -> serde_json::Value {
    let timeout = timeout_from_db(app_state);
    let now = now_ts();
    let mut v = serde_json::json!({
        "active": false,
        "timeout_secs": timeout,
        "lan_ip": crate::webapi::lan_ip(),
        "tailscale_ip": crate::webapi::tailscale_ip(),
        "webui_base_url": crate::config::webui_base_url(),
        "port": crate::webapi::settings_port(app_state),
        "bind": crate::webapi::settings_bind(app_state),
    });
    if let Some(s) = current() {
        v["active"] = serde_json::Value::Bool(true);
        v["ip"] = serde_json::Value::String(s.ip.clone());
        v["created_at"] = serde_json::Value::Number(s.created_at.into());
        v["last_seen"] = serde_json::Value::Number(s.last_seen.into());
        v["expires_in"] =
            serde_json::Value::Number((timeout as i64 - (now - s.last_seen)).max(0).into());
        if let Some(ip) = client_ip {
            v["is_owner"] = serde_json::Value::Bool(normalize_ip(ip) == s.ip);
        }
    }
    v
}

/// axum 中间件：挂在 `bearer_auth` 之后。
///
/// **阶段四起不再拒绝任何请求**（D1＝放弃 IP 排他）：只刷新"最近活跃来源"并写入
/// [`ClientIp`] extension。多来源同时在线时打一条日志 —— **看得见，但不阻断**。
pub async fn session_guard(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let app_state = state.app_handle.state::<AppState>();
    let ip = normalize_ip(&addr.ip().to_string());
    let timeout = timeout_from_db(&app_state);
    drop(app_state);

    if let GuardOutcome::Shared { previous_ip } = guard(&ip, timeout) {
        log::info!(
            "[WEBAPI-SESSION] 多来源同时在线（{previous_ip} → {ip}）：阶段四起不再排他，照常放行"
        );
    }
    let mut req = req;
    req.extensions_mut().insert(ClientIp(ip));
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单个测试函数内串行跑完整生命周期：全局 slot 是进程级共享状态，
    /// 拆成多个 #[test] 会与并行执行互相干扰。
    #[test]
    fn session_lifecycle_grant_renew_share_expire_kick() {
        // 干净起点
        clear(None);

        // 无会话 → Granted
        assert_eq!(guard_at("192.168.1.10", 600, 1_000), GuardOutcome::Granted);

        // 同 IP → Renewed（并刷新 last_seen）
        assert_eq!(guard_at("192.168.1.10", 600, 1_100), GuardOutcome::Renewed);

        // 异 IP、未超时 → **阶段四：不再拒绝**，只记录共享（这正是"手机 + bot 可同时用"的依据）
        assert_eq!(
            guard_at("192.168.1.20", 600, 1_200),
            GuardOutcome::Shared { previous_ip: "192.168.1.10".into() },
            "阶段四起异来源必须放行（D1＝放弃 IP 排他）"
        );
        // "最近活跃来源"更新为它（展示用），而 created_at 仍是会话起点
        let s = current().expect("会话仍在");
        assert_eq!(s.ip, "192.168.1.20");
        assert_eq!(s.created_at, 1_000, "created_at 是会话起点，不该被共享改写");
        assert_eq!(s.last_seen, 1_200);

        // 空闲超时后 → 由新来源接管（Granted）
        assert_eq!(guard_at("192.168.1.30", 600, 1_801), GuardOutcome::Granted);
        assert_eq!(current().map(|s| s.ip).as_deref(), Some("192.168.1.30"));

        // 桌面端强制踢出 → 清空，随后任意 IP Granted
        assert!(clear(None));
        assert!(current().is_none());
        assert_eq!(guard_at("192.168.1.40", 600, 5_000), GuardOutcome::Granted);

        // web 端退出：IP 不匹配不清除
        assert!(!clear(Some("192.168.1.50")));
        assert!(current().is_some());
        assert!(clear(Some("192.168.1.40")));
        assert!(current().is_none());
    }

    #[test]
    fn normalize_ip_collapses_v4mapped_and_loopback() {
        assert_eq!(normalize_ip("::ffff:192.168.1.7"), "192.168.1.7");
        assert_eq!(normalize_ip("::1"), "127.0.0.1");
        assert_eq!(normalize_ip("127.0.0.1"), "127.0.0.1");
        assert_eq!(normalize_ip("192.168.1.7"), "192.168.1.7");
    }
}
