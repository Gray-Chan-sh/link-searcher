use std::sync::Arc;

/// Shared state injected into every axum handler.
#[derive(Clone)]
pub struct ApiState {
    pub app_handle: tauri::AppHandle,
    pub auth_token: Arc<String>,
    pub cancel_token: Arc<tokio_util::sync::CancellationToken>,
    /// Bridged Tauri events: `(event_name, payload_json)`.
    pub event_tx: tokio::sync::broadcast::Sender<(String, String)>,
}