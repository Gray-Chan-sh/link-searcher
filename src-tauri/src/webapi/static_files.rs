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

    serve_from_dist(&path, &state).await
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

async fn serve_from_dist(path: &str, state: &ApiState) -> Response {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };

    // ① **内嵌优先**：release 包 / 安装包里前端是**编译期内嵌进 exe** 的（`frontendDist`），
    //    磁盘上并没有 `dist` 目录 —— 只问磁盘必然 404。
    //    2026-10-16 实测：release 下 `GET /` → `404 "Not Found"`，把一份 dist 放到 exe 旁
    //    立刻变 200 —— 即「打开详情」按钮与整个浏览器 Web 版在 release 下都打不开。
    //    顺序不能反过来（详见 docs/LARK_BOT.md §5.11）。
    if let Some(resp) = embedded_asset(rel, state) {
        return resp;
    }

    // ② 磁盘上的 dist：dev / `cargo run`（`frontendDist` 就在仓库里）走这条，行为不变。
    let base = dist_dir();
    if let Some(file) = resolve_static_path(&base, rel)
        && let Ok(content) = tokio::fs::read(&file).await
    {
        let mime = mime_from_extension(&file);
        return ([(header::CONTENT_TYPE, mime)], content).into_response();
    }

    // ③ SPA fallback: unknown paths (and rejected traversals) serve index.html.
    //    同样先内嵌后磁盘。
    if let Some(resp) = embedded_asset("index.html", state) {
        return resp;
    }
    match tokio::fs::read(base.join("index.html")).await {
        Ok(content) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            content,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

/// 从 **exe 内嵌的前端资源**里取一个文件（Tauri 的 asset resolver）。
///
/// - release / 安装包：前端在编译期被打进 exe，磁盘上没有 `dist` ——
///   这是浏览器 Web 版在 release 下**唯一**能用的来源（就是 2026-10-16 那个 404 的根因）。
/// - dev（配了 `devUrl`）：Tauri 不内嵌，resolver 自己会去读 `frontendDist`
///   （按 `config_parent()` 定位，**不依赖进程 CWD** —— 比原磁盘探测更稳）。
///
/// 解析器**自己带 SPA 兜底**（`get_for_scheme`：精确 → `{path}.html` →
/// `{path}/index.html` → `index.html`），所以未知路径它也会返回 index.html 资产；
/// 因此 MIME 必须取**资产自带的**那个，不能按请求路径猜。
fn embedded_asset(rel: &str, state: &ApiState) -> Option<Response> {
    let rel = normalize_asset_rel(rel)?;
    let asset = state.app_handle.asset_resolver().get(rel)?;
    let mime = with_utf8_charset(asset.mime_type());
    Some(([(header::CONTENT_TYPE, mime)], asset.bytes().to_vec()).into_response())
}

/// 查内嵌资源前的路径规范化：**解一次** percent-encoding，再按 [`is_safe_rel_path`] 判断。
/// 返回 `None` = 这条路径不值得去查内嵌资源（交给磁盘兜底 / SPA 兜底）。
///
/// 自己解一次是必需的：Tauri 的解析器**内部也会** percent-decode，只做字面检查会被
/// `%2e%2e%2f`（= `../`）绕过 —— 旧实现因为从不解码而天然安全，换成解析器后要把这一步补回来。
/// 解完若**仍含** `%XX`（= 双重编码：我解一次、解析器再解一次就变味），一律拒。
fn normalize_asset_rel(rel: &str) -> Option<String> {
    let decoded = percent_decode(rel);
    if contains_escape(&decoded) {
        return None;
    }
    if !is_safe_rel_path(&decoded) {
        return None;
    }
    Some(decoded)
}

/// 极简 percent-decoding（只解 `%XX`，解不动的原样保留，结果按 UTF-8 宽容解）。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 是否含**有效**的 `%XX` 转义（孤立 `%` 不算）。
fn contains_escape(s: &str) -> bool {
    let b = s.as_bytes();
    (0..b.len()).any(|i| {
        b[i] == b'%' && i + 2 < b.len() && hex_val(b[i + 1]).is_some() && hex_val(b[i + 2]).is_some()
    })
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 给**文本类** MIME 补上 `charset=utf-8`（资源自带的 MIME 不带 charset）。
///
/// 浏览器对没有 charset 的 `text/html` 会先按本地编码猜，中文界面就可能变乱码
/// （页面里的 `<meta charset>` 通常能救回来，但不该指望它）。
fn with_utf8_charset(mime: &str) -> String {
    if mime.contains("charset") {
        return mime.to_string();
    }
    let textual = mime.starts_with("text/")
        || matches!(
            mime,
            "application/javascript" | "application/json" | "image/svg+xml"
        );
    if textual {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    }
}

/// `rel` 是不是一个可安全查询的相对路径。
///
/// 拒绝：空串、绝对路径（`/x`、`C:/x`、`\\server\x`）、含 `\`、以及任何 `..` 分量。
/// 磁盘那条另有 [`resolve_static_path`] 的 canonicalize 兜底；这里是**共同的第一道闸**
/// （`rel` 直接来自请求 URI，且静态兜底在鉴权中间件之外）。
fn is_safe_rel_path(rel: &str) -> bool {
    if rel.is_empty() || rel.contains('\\') {
        // URI 路径只会用 `/`；出现 `\` 一律当可疑（Windows 会把它当分隔符）。
        return false;
    }
    let p = std::path::Path::new(rel);
    if p.is_absolute() {
        return false;
    }
    !p.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    })
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
    use super::{is_safe_rel_path, resolve_static_path};

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

    /// 内嵌资源那条路的闸门（`embedded_asset` 用的就是它）。
    ///
    /// 这条测试守的是**路径规则**，不是磁盘：内嵌资源是精确 map 查找，
    /// 但它与磁盘兜底共用同一个判断，规则一旦松动，最先被漏掉的会是这里。
    #[test]
    fn safe_rel_path_allows_plain_files_and_rejects_escapes() {
        assert!(is_safe_rel_path("index.html"));
        assert!(is_safe_rel_path("assets/index-CrS2KPuL.js"));
        assert!(is_safe_rel_path("favicon.svg"));

        assert!(!is_safe_rel_path(""), "空串由调用方替成 index.html，不该进查找");
        assert!(!is_safe_rel_path("../secret.txt"));
        assert!(!is_safe_rel_path("sub/../../secret.txt"));
        assert!(!is_safe_rel_path("/etc/passwd"), "以 / 开头 = 根目录");
        assert!(!is_safe_rel_path("C:/Windows/win.ini"));
        assert!(!is_safe_rel_path(r"..\..\secret.txt"), "反斜杠一律拒（Windows 也当分隔符）");
        assert!(!is_safe_rel_path(r"assets\..\..\secret.txt"));
    }

    /// 编码后的穿越：Tauri 的解析器**内部会 percent-decode**，所以字面检查不够。
    ///
    /// 2026-10-16 实测：`get_for_scheme` 里有
    /// `percent_encoding::percent_decode(path)`，且它只过滤 `RootDir` 分量、**不过滤 `..`**
    /// —— 旧实现因为从不解码而天然安全，换成解析器后必须自己解一次再判。
    #[test]
    fn normalize_asset_rel_decodes_once_and_blocks_traversal() {
        use super::{normalize_asset_rel, percent_decode};

        assert_eq!(percent_decode("a%20b.js"), "a b.js");
        assert_eq!(percent_decode("100%.js"), "100%.js", "孤立 % 不是转义");
        assert_eq!(percent_decode("%zz"), "%zz", "非十六进制不解");

        assert_eq!(
            normalize_asset_rel("assets/index-CrS2KPuL.js").as_deref(),
            Some("assets/index-CrS2KPuL.js")
        );
        assert_eq!(normalize_asset_rel("a%20b.js").as_deref(), Some("a b.js"), "正常转义要解出来");
        assert_eq!(normalize_asset_rel("%2e%2e%2fCargo.toml"), None, "解出来是 ../ → 必须拒");
        assert_eq!(
            normalize_asset_rel("%252e%252e%252fCargo.toml"),
            None,
            "双重编码：我解一次、解析器再解一次就变 ../ → 必须拒"
        );
        assert_eq!(normalize_asset_rel("%2e%2e%2f%2e%2e%2fCargo.toml"), None);
        assert_eq!(normalize_asset_rel("../Cargo.toml"), None, "字面量同样拒");
        assert_eq!(normalize_asset_rel("/etc/passwd"), None);
        assert_eq!(normalize_asset_rel(r"..\..\Cargo.toml"), None);
    }

    /// MIME 要取**资产自带**的（解析器带 SPA 兜底：`/chat/xxx` 实际返回的是 index.html），
    /// 只补 `charset`；按请求路径的扩展名猜会把 HTML 标成 `application/javascript`。
    #[test]
    fn utf8_charset_added_only_to_textual_types() {
        use super::with_utf8_charset;

        assert_eq!(with_utf8_charset("text/html"), "text/html; charset=utf-8");
        assert_eq!(with_utf8_charset("text/css"), "text/css; charset=utf-8");
        assert_eq!(
            with_utf8_charset("application/javascript"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(with_utf8_charset("image/svg+xml"), "image/svg+xml; charset=utf-8");
        assert_eq!(
            with_utf8_charset("text/html; charset=utf-8"),
            "text/html; charset=utf-8",
            "已经有了就不重复加"
        );
        assert_eq!(with_utf8_charset("image/png"), "image/png");
        assert_eq!(with_utf8_charset("font/woff2"), "font/woff2");
        assert_eq!(with_utf8_charset("application/octet-stream"), "application/octet-stream");
    }
}
