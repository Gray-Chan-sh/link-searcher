use std::io::Read;

use axum::{
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};

use crate::webapi::state::ApiState;

const VITE_DEV_SERVER: &str = "http://127.0.0.1:1420";

pub async fn serve_static(uri: Uri, State(state): State<ApiState>) -> Response {
    let path = uri.path().to_string();
    if path.starts_with("/api/") {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    if state.dev_mode {
        return proxy_to_vite(&path).await;
    }

    serve_from_dist(&path).await
}

async fn proxy_to_vite(path: &str) -> Response {
    let url = format!("{VITE_DEV_SERVER}{path}");
    let url_clone = url.clone();

    match tokio::task::spawn_blocking(move || ureq::get(&url_clone).call()).await {
        Ok(Ok(response)) => {
            let status = StatusCode::from_u16(response.status()).unwrap_or(StatusCode::BAD_GATEWAY);
            let mime = mime_from_path(path);

            let mut body = Vec::new();
            if response.into_reader().read_to_end(&mut body).is_err() {
                return (StatusCode::BAD_GATEWAY, "Failed to read upstream body").into_response();
            }

            (status, [(header::CONTENT_TYPE, mime)], body).into_response()
        }
        Ok(Err(e)) => {
            log::warn!("[WEBAPI-DEV] proxy to Vite failed for {path}: {e}");
            (StatusCode::BAD_GATEWAY, format!("Vite dev server unreachable: {e}")).into_response()
        }
        Err(e) => {
            log::error!("[WEBAPI-DEV] spawn_blocking error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Proxy task failed").into_response()
        }
    }
}

async fn serve_from_dist(path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let file = dist_dir().join(rel);

    match tokio::fs::read(&file).await {
        Ok(content) => {
            let mime = mime_from_extension(&file);
            ([(header::CONTENT_TYPE, mime)], content).into_response()
        }
        Err(_) => {
            match tokio::fs::read(dist_dir().join("index.html")).await {
                Ok(content) => (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    content,
                )
                    .into_response(),
                Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
            }
        }
    }
}

fn dist_dir() -> std::path::PathBuf {
    let candidates = [
        std::path::PathBuf::from("../dist"),
        std::path::PathBuf::from("./dist"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("dist")))
            .unwrap_or_default(),
    ];
    for c in &candidates {
        if c.join("index.html").exists() {
            return c.clone();
        }
    }
    std::path::PathBuf::from("../dist")
}

fn mime_from_path(path: &str) -> &'static str {
    if path.is_empty() || path == "/" {
        return "text/html; charset=utf-8";
    }
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn mime_from_extension(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
