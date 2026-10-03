//! HTTPS API server for remote access.
//!
//! 默认开启（`lib.rs` 启动即 spawn）——`web_api_enabled == "false"` 才关闭。
//! Bearer token auth（来自 config.json，见 [`resolve_token`]）+ 自签名 TLS +
//! 单用户 IP 会话（[`session`]）+ graceful shutdown on exit。

pub mod auth;
pub mod routes;
pub mod session;
pub mod state;
pub mod static_files;
pub mod tls;

use std::net::SocketAddr;
use std::sync::Arc;

use axum_server::tls_rustls::RustlsConfig;
use tauri::{Listener, Manager};
use tokio_util::sync::CancellationToken;

use crate::state::AppState;
use crate::webapi::state::ApiState;

pub const KEY_ENABLED: &str = "web_api_enabled";
pub const KEY_PORT: &str = "web_api_port";
pub const KEY_TOKEN: &str = "web_api_token";
pub const KEY_BIND: &str = "web_api_bind";
pub const KEY_DEV_MODE: &str = "web_api_dev_mode";

const DEFAULT_PORT: u16 = 8443;

/// Tauri events forwarded to web clients via `GET /api/events` (SSE).
const BRIDGED_EVENTS: &[&str] = &[
    "scan-progress",
    "scan-completed",
    "ai-chunk",
    "ai-progress",
    "ai-done",
    "migration-progress",
    "migration-warning",
    "funasr-install-done",
    "bge-install-done",
    "restore-completed",
    "dep-progress",
    "dep-install-done",
];

pub fn spawn_server(app_handle: tauri::AppHandle) {
    let app_state = app_handle.state::<AppState>();
    let token = resolve_token(&app_state);
    let port = settings_port(&app_state);
    let bind = settings_bind(&app_state);
    let dev_mode = load_dev_mode(&app_state);
    drop(app_state);

    let cancel_token = CancellationToken::new();
    let cancel_for_api = cancel_token.clone();
    let cancel_for_shutdown = cancel_token.clone();
    app_handle.manage(cancel_token);

    let (event_tx, _) = tokio::sync::broadcast::channel::<(String, String)>(256);
    for event_name in BRIDGED_EVENTS {
        let tx = event_tx.clone();
        app_handle.listen_any(*event_name, move |event| {
            log::info!("[WEBAPI-BRIDGE] forwarded event {} payload_chars={}", event_name, event.payload().len());
            // No subscribers / lagged receiver: drop silently.
            let _ = tx.send((event_name.to_string(), event.payload().to_string()));
        });
    }

    let data_dir = app_handle.state::<AppState>().data_dir.clone();
    let addr: SocketAddr = match format!("{bind}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            log::error!("[WEBAPI] invalid bind address '{bind}:{port}': {e}");
            return;
        }
    };

    // 方便远程用户连接：把可访问地址与 token 打进日志（日志在本机
    // data_dir/logs，可直接复制分发）。绑 0.0.0.0 时展示探测到的局域网 IP；
    // 绑具体 IP 就展示它本身；仅回环绑定时明确提示本机访问。
    if bind == "127.0.0.1" {
        log::info!("[WEBAPI] 本机访问: https://127.0.0.1:{port} (token: {token})");
    } else {
        let display_host = if bind == "0.0.0.0" { lan_ip() } else { bind.clone() };
        log::info!("[WEBAPI] 远程访问: https://{display_host}:{port} (token: {token})");
    }

    let api_state = ApiState {
        app_handle: app_handle.clone(),
        auth_token: Arc::new(token),
        cancel_token: Arc::new(cancel_for_api),
        event_tx,
        dev_mode,
    };

    let app = routes::build_router(api_state);

    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    // Install rustls CryptoProvider before any TLS operations.
    // Must be done synchronously before spawning the async task.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tauri::async_runtime::spawn(async move {
        let (cert_path, key_path) = match tls::ensure_cert(&data_dir) {
            Ok(p) => p,
            Err(e) => {
                log::error!("[WEBAPI] TLS cert generation failed: {e}");
                return;
            }
        };

        let tls_config = match RustlsConfig::from_pem_file(&cert_path, &key_path).await {
            Ok(c) => c,
            Err(e) => {
                log::error!("[WEBAPI] TLS config load failed: {e}");
                return;
            }
        };

        log::info!("[WEBAPI] HTTPS server starting on https://{addr}");

        if let Err(e) = axum_server::bind_rustls(addr, tls_config)
            .handle(handle)
            // with_connect_info: session_guard 依赖 ConnectInfo<SocketAddr> 取来源 IP
            // （axum-server 的 MakeService 以 SocketAddr 为目标，满足 Connected<SocketAddr>）。
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
        {
            log::error!("[WEBAPI] server error: {e}");
        }
    });

    tauri::async_runtime::spawn(async move {
        cancel_for_shutdown.cancelled().await;
        log::info!("[WEBAPI] server shutting down gracefully");
        handle_clone.shutdown();
    });
}

/// 解析生效的 Bearer token，优先级：**环境变量 > config.json > DB（旧数据迁移）> 首次生成**。
///
/// 解析结果始终同步回写 `config.json` 与 `app_settings`（鉴权中间件
/// [`crate::webapi::auth`] 从 DB 读取，设置页也读 DB），保证三处一致。
fn resolve_token(app_state: &AppState) -> String {
    let env_token = std::env::var("LS_WEB_API_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let db_token = app_state
        .db
        .get()
        .ok()
        .and_then(|conn| {
            conn.query_row::<String, _, _>(
                "SELECT value FROM app_settings WHERE key = ?1",
                rusqlite::params![KEY_TOKEN],
                |r| r.get(0),
            )
            .ok()
        })
        .filter(|s| !s.is_empty());

    let cfg_token = {
        let t = crate::config::web_api_token();
        if t.is_empty() { None } else { Some(t) }
    };

    // 决定生效 token 并标记是否需要回写 config.json。
    let (token, cfg_dirty) = if let Some(t) = env_token {
        (t, true) // 环境变量注入 → 以它为准并固化
    } else if let Some(t) = cfg_token {
        (t, false)
    } else if let Some(t) = db_token.clone() {
        (t, true) // 旧版本只存 DB → 迁移进 config.json
    } else {
        (uuid::Uuid::new_v4().simple().to_string(), true)
    };

    if cfg_dirty {
        crate::config::set_web_api_token(&token);
        log::info!("[WEBAPI] token 已写入 config.json");
    }
    if db_token.as_deref() != Some(token.as_str())
        && let Ok(conn) = app_state.db.get()
    {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![KEY_TOKEN, &token],
        );
    }
    token
}

/// 本机局域网 IP：UDP connect 只做路由选择、不发包，用于选出对外网卡地址。
/// 无网络时回退 127.0.0.1。
pub fn lan_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

pub fn settings_port(app_state: &AppState) -> u16 {
    if let Ok(conn) = app_state.db.get()
        && let Ok(port_str) = conn.query_row::<String, _, _>(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![KEY_PORT],
            |r| r.get::<_, String>(0),
        )
            && let Ok(port) = port_str.parse::<u16>() {
                return port;
            }
    DEFAULT_PORT
}

pub fn settings_bind(app_state: &AppState) -> String {
    if let Ok(conn) = app_state.db.get()
        && let Ok(bind) = conn.query_row::<String, _, _>(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![KEY_BIND],
            |r| r.get::<_, String>(0),
        )
            && !bind.is_empty() {
                return bind;
            }
    "0.0.0.0".to_string()
}

fn load_dev_mode(app_state: &AppState) -> bool {
    if let Ok(conn) = app_state.db.get()
        && let Ok(val) = conn.query_row::<String, _, _>(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![KEY_DEV_MODE],
            |r| r.get::<_, String>(0),
        )
    {
        return val == "true";
    }
    false
}
