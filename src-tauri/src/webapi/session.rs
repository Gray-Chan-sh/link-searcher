//! Single-user Web session lease（单用户登录会话）.
//!
//! 同一时刻全网只允许一个活跃 Web 会话，按 **来源 IP** 识别身份（token 仍是
//! 第一道门槛，见 [`crate::webapi::auth`]):
//!
//! - 无会话 / 会话已超时 → 给该 IP 建立会话（Granted）
//! - 同一 IP 再次请求（多标签页/重连）→ 续租（Renewed）
//! - 其它 IP 且会话未超时 → 拒绝（403，携带在线 IP 与剩余时间）
//!
//! 任何通过鉴权的请求都会续租；浏览器端每 30s 发一次 `POST /api/session/ping`
//! 心跳，保证 SSE 长连接期间会话不过期。会话可由 web 端主动退出
//! （`POST /api/session/logout`）或桌面端强制踢出（`web_session_logout` 命令）。
//!
//! 这把「多用户同时写」的冲突面收敛为单写入者：聊天历史并发保存、扫描重复
//! 触发、AI 取消互杀等在 Web 侧天然不再发生（桌面端与 Web 端仍可能同时操作，
//! 因此聊天历史仍有锁、扫描仍走 `is_scanning` CAS）。

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
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
    /// 其它 IP 持有未过期会话 → 拒绝，携带持有者 IP 与剩余秒数。
    Denied { ip: String, expires_in: i64 },
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
        Some(s) => GuardOutcome::Denied {
            ip: s.ip.clone(),
            expires_in: (timeout - (now - s.last_seen)).max(0),
        },
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

/// axum 中间件：挂在 `bearer_auth` 之后（token 先过，IP 会话后过）。
/// 通过 → 写入 [`ClientIp`] extension 并续租；拒绝 → 403 + 在线 IP / 剩余时间。
pub async fn session_guard(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let app_state = state.app_handle.state::<AppState>();
    let ip = normalize_ip(&addr.ip().to_string());
    let timeout = timeout_from_db(&app_state);
    drop(app_state);

    match guard(&ip, timeout) {
        GuardOutcome::Granted | GuardOutcome::Renewed => {
            let mut req = req;
            req.extensions_mut().insert(ClientIp(ip));
            Ok(next.run(req).await)
        }
        GuardOutcome::Denied { ip: held_ip, expires_in } => {
            log::info!(
                "[WEBAPI-SESSION] denied: {held_ip} holds the session, requester waits {expires_in}s"
            );
            Ok((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": format!("已由 {held_ip} 登录（单用户模式），请等待其退出或超时（约 {expires_in}s），或在桌面端「设置 → Web API」强制踢出",
                    ),
                    "session_ip": held_ip,
                    "expires_in": expires_in,
                })),
            )
                .into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单个测试函数内串行跑完整生命周期：全局 slot 是进程级共享状态，
    /// 拆成多个 #[test] 会与并行执行互相干扰。
    #[test]
    fn session_lifecycle_grant_renew_deny_expire_kick() {
        // 干净起点
        clear(None);

        // 无会话 → Granted
        assert_eq!(guard_at("192.168.1.10", 600, 1_000), GuardOutcome::Granted);

        // 同 IP → Renewed（并刷新 last_seen）
        assert_eq!(guard_at("192.168.1.10", 600, 1_100), GuardOutcome::Renewed);

        // 异 IP、未超时 → Denied，带持有者与剩余时间
        assert_eq!(
            guard_at("192.168.1.20", 600, 1_200),
            GuardOutcome::Denied { ip: "192.168.1.10".into(), expires_in: 500 }
        );

        // 超时后 → 接管
        assert_eq!(guard_at("192.168.1.20", 600, 1_101 + 601), GuardOutcome::Granted);
        assert_eq!(current().map(|s| s.ip).as_deref(), Some("192.168.1.20"));

        // 桌面端强制踢出 → 清空，随后任意 IP Granted
        assert!(clear(None));
        assert!(current().is_none());
        assert_eq!(guard_at("192.168.1.30", 600, 5_000), GuardOutcome::Granted);

        // web 端退出：IP 不匹配不清除
        assert!(!clear(Some("192.168.1.40")));
        assert!(current().is_some());
        assert!(clear(Some("192.168.1.30")));
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
