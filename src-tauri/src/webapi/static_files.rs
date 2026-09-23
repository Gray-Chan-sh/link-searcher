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
            // Trust Vite's own Content-Type (handles tsx/ts/import 等动态模块，
            // 自己按扩展名猜会漏 tsx/ts 等导致浏览器当文件下载)。
            let mime = response
                .header("content-type")
                .map(|v| v.to_string())
                .unwrap_or_else(|| mime_from_path(path).to_string());

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
    let base = dist_dir();

    if let Some(file) = resolve_static_path(&base, rel)
        && let Ok(content) = tokio::fs::read(&file).await
    {
        let mime = mime_from_extension(&file);
        return ([(header::CONTENT_TYPE, mime)], content).into_response();
    }

    // SPA fallback: unknown paths (and rejected traversals) serve index.html.
    match tokio::fs::read(base.join("index.html")).await {
        Ok(content) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            content,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

/// Resolve `rel` against `base`, returning the file only if it exists and,
/// after canonicalization, still lies inside `base`.
///
/// `rel` comes straight from the request URI (the static fallback is outside
/// the auth middleware), so `../`, absolute paths and symlinks must all be
/// rejected. Returns `None` for any escape attempt.
fn resolve_static_path(base: &std::path::Path, rel: &str) -> Option<std::path::PathBuf> {
    let base = base.canonicalize().ok()?;
    let resolved = base.join(rel).canonicalize().ok()?;
    if resolved.starts_with(&base) {
        Some(resolved)
    } else {
        None
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

#[cfg(test)]
mod tests {
    use super::resolve_static_path;

    /// Build an isolated dist-like directory with a sibling "secret" file
    /// just outside it, so traversal attempts have a real target.
    fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "ls-static-test-{}-{}",
            std::process::id(),
            tag
        ));
        let base = root.join("dist");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("index.html"), b"index").unwrap();
        std::fs::write(base.join("app.js"), b"js").unwrap();
        std::fs::write(base.join("sub").join("page.html"), b"page").unwrap();
        let secret = root.join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        (root, base)
    }

    #[test]
    fn resolves_files_inside_base() {
        let (root, base) = fixture("inside");
        assert!(resolve_static_path(&base, "app.js").is_some());
        assert!(resolve_static_path(&base, "sub/page.html").is_some());
        assert!(resolve_static_path(&base, "sub/../app.js").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_parent_traversal() {
        let (root, base) = fixture("traversal");
        assert!(resolve_static_path(&base, "../secret.txt").is_none());
        assert!(resolve_static_path(&base, "sub/../../secret.txt").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_absolute_and_missing_paths() {
        let (root, base) = fixture("absolute");
        let absolute = if cfg!(windows) {
            "C:/Windows/win.ini"
        } else {
            "/etc/passwd"
        };
        assert!(resolve_static_path(&base, absolute).is_none());
        assert!(resolve_static_path(&base, "does-not-exist.js").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
