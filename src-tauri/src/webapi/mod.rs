//! Optional HTTPS API server for remote access.
//!
//! Default OFF — enabled via `web_api_enabled` in app_settings.
//! Bearer token auth + self-signed TLS cert + graceful shutdown on exit.

pub mod auth;
pub mod routes;
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

const DEFAULT_PORT: u16 = 8443;

/// Tauri events forwarded to web clients via `GET /api/events` (SSE).
const BRIDGED_EVENTS: &[&str] = &[
    "scan-progress",
    "scan-completed",
    "ai-chunk",
    "ai-done",
    "migration-progress",
    "migration-warning",
    "funasr-install-done",
    "bge-install-done",
    "restore-completed",
];

pub fn spawn_server(app_handle: tauri::AppHandle) {
    let token = generate_or_load_token(&app_handle);
    let port = load_port(&app_handle);
    let bind = load_bind(&app_handle);

    let cancel_token = CancellationToken::new();
    let cancel_for_api = cancel_token.clone();
    let cancel_for_shutdown = cancel_token.clone();
    app_handle.manage(cancel_token);

    let (event_tx, _) = tokio::sync::broadcast::channel::<(String, String)>(256);
    for event_name in BRIDGED_EVENTS {
        let tx = event_tx.clone();
        app_handle.listen_any(*event_name, move |event| {
            // No subscribers / lagged receiver: drop silently.
            let _ = tx.send((event_name.to_string(), event.payload().to_string()));
        });
    }

    let api_state = ApiState {
        app_handle: app_handle.clone(),
        auth_token: Arc::new(token),
        cancel_token: Arc::new(cancel_for_api),
        event_tx,
    };

    let app = routes::build_router(api_state);

    let data_dir = app_handle.state::<AppState>().data_dir.clone();
    let addr: SocketAddr = match format!("{bind}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            log::error!("[WEBAPI] invalid bind address '{bind}:{port}': {e}");
            return;
        }
    };

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
            .serve(app.into_make_service())
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

fn generate_or_load_token(app_handle: &tauri::AppHandle) -> String {
    let app_state = app_handle.state::<AppState>();
    if let Ok(conn) = app_state.db.get() {
        let existing: Result<String, _> = conn.query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            rusqlite::params![KEY_TOKEN],
            |r| r.get::<_, String>(0),
        );
        if let Ok(token) = existing
            && !token.is_empty() {
                return token;
            }
        let token = uuid::Uuid::new_v4().simple().to_string();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![KEY_TOKEN, &token],
        );
        log::info!("[WEBAPI] generated new bearer token");
        token
    } else {
        uuid::Uuid::new_v4().simple().to_string()
    }
}

fn load_port(app_handle: &tauri::AppHandle) -> u16 {
    let app_state = app_handle.state::<AppState>();
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

fn load_bind(app_handle: &tauri::AppHandle) -> String {
    let app_state = app_handle.state::<AppState>();
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
