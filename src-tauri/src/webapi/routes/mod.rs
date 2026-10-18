pub mod search;
pub mod files;
pub mod index;
pub mod dirs;
pub mod ai;
pub mod settings;
pub mod session;
pub mod setup;
pub mod events;
pub mod logs;
pub mod tesseract;
pub mod backup;
pub mod config;
pub mod lark;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json},
    Router,
};
use serde::Serialize;

use crate::webapi::auth;
use crate::webapi::state::ApiState;
use crate::webapi::static_files;

#[derive(Serialize)]
pub struct ApiError {
    pub error: String,
}

pub fn default_page() -> usize {
    1
}
pub fn default_page_size() -> usize {
    20
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, Json(self)).into_response()
    }
}

/// 目录枚举的**越界防护**（Web API 专用）。
///
/// 桌面端 IPC 不受此限：添加索引目录本来就要能浏览任意位置（用户得先走到目标
/// 文件夹）。但 Web 端把"任意路径枚举"暴露给持有 token 的任意客户端，等于把整块
/// 磁盘当目录浏览器 —— `~/.ssh`、浏览器 profile、其它项目的文件名全可列举
/// （正文仍受 DB 追踪限制，但文件名/结构本身就是敏感信息）。
///
/// 规则：只允许列举**已注册索引目录之内**的路径；拒绝 `..`（防穿越）。
pub(crate) fn ensure_within_registered_roots(
    conn: &rusqlite::Connection,
    requested: &str,
) -> Result<(), ApiError> {
    if std::path::Path::new(requested)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ApiError { error: "path must not contain '..'".into() });
    }
    let roots = crate::db::dir_config::list_dirs(conn)
        .map_err(|e| ApiError { error: e.to_string() })?;
    let want = canon_path(requested);
    let allowed = !want.is_empty()
        && roots.iter().any(|d| {
            let root = canon_path(&d.path);
            !root.is_empty() && (want == root || want.starts_with(&format!("{root}/")))
        });
    if allowed {
        Ok(())
    } else {
        Err(ApiError {
            error: "path is outside the registered index roots".into(),
        })
    }
}

/// 归一化以便比较：与 DB 存储一致的正斜杠路径；Windows 下大小写不敏感。
fn canon_path(p: &str) -> String {
    let n = crate::scanner::helpers::normalize_os_path(p.trim());
    let n = n.trim_end_matches('/').to_string();
    if cfg!(windows) { n.to_lowercase() } else { n }
}

#[cfg(test)]
mod tests {
    use super::ensure_within_registered_roots;

    fn db_with_root(root: &str) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        crate::db::init_db(&conn).expect("schema");
        conn.execute(
            "INSERT INTO dir_config (id, path, alias, recursive, created_at, updated_at) \
             VALUES ('d1', ?1, NULL, 1, 0, 0)",
            rusqlite::params![root],
        )
        .expect("insert dir");
        conn
    }

    /// Web 端目录枚举必须被限制在已注册根之内，且拒绝 `..` 穿越。
    #[test]
    fn enumeration_guard_rejects_paths_outside_roots() {
        let root = if cfg!(windows) { r"D:\docs" } else { "/docs" };
        let conn = db_with_root(root);
        let inside = if cfg!(windows) { r"D:\docs\sub" } else { "/docs/sub" };
        let outside = if cfg!(windows) { r"D:\secret" } else { "/secret" };
        let sibling_prefix = if cfg!(windows) { r"D:\docs2" } else { "/docs2" };
        let escape = if cfg!(windows) { r"D:\docs\..\secret" } else { "/docs/../secret" };

        assert!(ensure_within_registered_roots(&conn, root).is_ok(), "根本身应放行");
        assert!(ensure_within_registered_roots(&conn, inside).is_ok(), "根之内应放行");
        assert!(ensure_within_registered_roots(&conn, outside).is_err(), "根之外必须拒绝");
        // 前缀相同的兄弟目录不能被当成"根之内"
        assert!(ensure_within_registered_roots(&conn, sibling_prefix).is_err(), "同前缀兄弟目录必须拒绝");
        assert!(ensure_within_registered_roots(&conn, escape).is_err(), "`..` 穿越必须拒绝");
        assert!(ensure_within_registered_roots(&conn, "").is_err(), "空路径必须拒绝");
    }

    /// 没有任何已注册目录时，一律拒绝（不能用"枚举整盘"当作后备）。
    #[test]
    fn enumeration_guard_rejects_when_no_root_registered() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        crate::db::init_db(&conn).expect("schema");
        let any = if cfg!(windows) { r"D:\anything" } else { "/anything" };
        assert!(ensure_within_registered_roots(&conn, any).is_err());
    }
}

pub fn build_router(state: ApiState) -> Router {
    // All `/api/*` routes (including token rotation) sit behind bearer auth.
    // A forgotten token can only be reset from the desktop Settings page —
    // an unauthenticated reset endpoint would let anyone on the network
    // overwrite the token and take over the API.
    //
    // Layer order: `route_layer` added later runs first on the request, so
    // `bearer_auth` (token check) wraps `session_guard` (**在线状态记录**；阶段四起不再排他):
    // a wrong token gets 401 before any session state is touched/created.
    search::router(state.clone())
        .merge(files::router(state.clone()))
        .merge(index::router(state.clone()))
        .merge(dirs::router(state.clone()))
        .merge(ai::router(state.clone()))
        .merge(settings::router(state.clone()))
        .merge(session::router(state.clone()))
        .merge(setup::router(state.clone()))
        .merge(logs::router(state.clone()))
        .merge(tesseract::router(state.clone()))
        .merge(backup::router(state.clone()))
        .merge(config::router(state.clone()))
        .merge(events::router(state.clone()))
        .merge(lark::router(state.clone()))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::webapi::session::session_guard,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::bearer_auth,
        ))
        // 静态资源（SPA 的 HTML/JS/CSS）不走鉴权与会话：浏览器必须先加载页面
        // 才能输入 token 登录；API 一律在上面两层之内。
        .fallback(static_files::serve_static)
        .with_state(state)
}
