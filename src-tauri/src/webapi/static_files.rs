use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};

pub async fn serve_static(uri: Uri) -> Response {
    let path = uri.path().to_string();
    if path.starts_with("/api/") {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

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
                ).into_response(),
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
