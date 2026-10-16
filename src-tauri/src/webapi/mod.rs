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
    // 设置页「集成」面板靠它刷新（`config_watch.rs` 的热重载事件）。
    // ⚠️ 漏了它 → **Web 模式**下面板永远不刷新（桌面走 Tauri IPC，所以只在浏览器里坏）。
    // 2026-10-16 真机验收就是这样发现的；契约由本文件的
    // `bridged_events_cover_frontend_subscriptions` 测试守住。
    "config-changed",
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
                log::debug!("[WEBAPI-BRIDGE] forwarded event {} payload_chars={}", event_name, event.payload().len());
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::BRIDGED_EVENTS;

    /// **桌面专属**的订阅：只经 `@tauri-apps/api/event` 拿事件，不需要（也不会）桥接。
    ///
    /// 加进来的每个事件都必须在注释里写明"为什么浏览器模式不需要它" ——
    /// 否则这张表会变成"忘了桥接"的垃圾桶。
    const TAURI_ONLY_EVENTS: &[&str] = &[
        // 目录移除进度：`DirManager.tsx` 用 Tauri 的 `listen()` 直接订阅（不在 `client.listen`
        // 的 Web 分支里），桥接了也收不到，属于既有历史；真要修得先改成 `client.listen`。
        "remove-dir-progress",
    ];

    /// 契约：**前端订阅的每个事件都必须在 [`BRIDGED_EVENTS`] 里**（除非明确是桌面专属）。
    ///
    /// 为什么值得一个测试：桌面模式走 Tauri IPC，`emit` 一发就到；Web 模式却要过这张白名单。
    /// 于是"忘了加"的后果**只在浏览器里**出现（页面不报错、只是永远显示旧值）——
    /// 2026-10-16 真机验收发现的 `config-changed` 缺陷正是如此：事件照发，
    /// 浏览器里的设置页却一直不刷新。
    #[test]
    fn bridged_events_cover_frontend_subscriptions() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri 的父目录就是仓库根")
            .join("src");

        let mut files = Vec::new();
        collect_source_files(&src, &mut files);
        assert!(!files.is_empty(), "没扫到前端源码，扫描逻辑失效了");

        let mut subscribed: Vec<String> = Vec::new();
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else {
                continue;
            };
            for name in listen_event_names(&text) {
                if !subscribed.contains(&name) {
                    subscribed.push(name);
                }
            }
        }
        assert!(
            !subscribed.is_empty(),
            "没扫到任何 listen('…') 订阅，扫描逻辑失效了"
        );

        for name in &subscribed {
            if TAURI_ONLY_EVENTS.contains(&name.as_str()) {
                continue;
            }
            assert!(
                BRIDGED_EVENTS.contains(&name.as_str()),
                "前端订阅了 '{name}'，但它不在 BRIDGED_EVENTS 里 —— \
                 Web 模式下这个订阅永远不会触发（桌面模式不受影响，所以最容易漏）。\
                 真要桥接就加进 BRIDGED_EVENTS；确实桌面专属就加进 TAURI_ONLY_EVENTS 并写明理由。"
            );
        }
    }

    /// 递归收集 `src/**/*.{ts,tsx}`。
    fn collect_source_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_source_files(&path, out);
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "ts" || e == "tsx");
            if is_source {
                out.push(path);
            }
        }
    }

    /// 从一份前端源码里抽出 `listen('name'` / `listen<Type>('name'` / `client.listen<…>('name'` 的
    /// 事件名。
    ///
    /// 刻意不引入正则：判据只有"调用者位置 + 紧邻的引号"，同时**要求名字形如
    /// `[a-z][a-z0-9-]*`**，这样 `listeners` / `unlisten.then` / `subscribeToasts(l: Listener)`
    /// 这类同形词都不会被误判（它们要么前一个字符是字母数字，要么后面不是 `(` / `<`）。
    fn listen_event_names(text: &str) -> Vec<String> {
        let bytes = text.as_bytes();
        let mut found = Vec::new();
        let mut i = 0;
        while let Some(pos) = text[i..].find("listen") {
            let start = i + pos;
            i = start + "listen".len();

            // 前一个字符必须是边界：挡住 `unlisten` / `listeners` 等。
            let prev_is_word = start > 0
                && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            if prev_is_word {
                continue;
            }

            // 可选泛型参数 `<…>`（含嵌套），随后必须是 `(`。
            let mut j = i;
            if bytes.get(j).is_some_and(|b| *b == b'<') {
                let mut depth = 0usize;
                while let Some(b) = bytes.get(j) {
                    match b {
                        b'<' => depth += 1,
                        b'>' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
            }
            if bytes.get(j) != Some(&b'(') {
                continue;
            }
            // 跳过 `(` 与空白，取第一个引号串。
            j += 1;
            while bytes.get(j).is_some_and(|b| b.is_ascii_whitespace()) {
                j += 1;
            }
            let Some(quote) = bytes.get(j).copied().filter(|b| *b == b'\'' || *b == b'"') else {
                continue;
            };
            let name_start = j + 1;
            let Some(rel_end) = bytes[name_start..].iter().position(|b| *b == quote) else {
                continue;
            };
            let name = &text[name_start..name_start + rel_end];
            let looks_like_event = !name.is_empty()
                && name.as_bytes()[0].is_ascii_lowercase()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if looks_like_event {
                found.push(name.to_string());
            }
            i = name_start + rel_end;
        }
        found
    }

    /// 扫描器自身的守卫：同形词（`unlisten` / `listeners` / `listenScanProgress`）不得被误判。
    #[test]
    fn listen_event_names_ignores_lookalikes() {
        let sample = r#"
            const listeners = new Map()
            void unlisten.then(f => f())
            subscribeToasts(l: Listener)
            await client.listen<ScanProgress>('scan-progress', h)
            listen('scan-completed', () => {})
            const { listen } = await import('@tauri-apps/api/event')
            listen<{ current: number }>('remove-dir-progress', h)
            listenScanProgress(setScanProgress)
        "#;
        assert_eq!(
            listen_event_names(sample),
            vec![
                "scan-progress".to_string(),
                "scan-completed".to_string(),
                "remove-dir-progress".to_string(),
            ]
        );
    }
}
