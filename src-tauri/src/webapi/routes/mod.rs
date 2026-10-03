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

pub fn build_router(state: ApiState) -> Router {
    // All `/api/*` routes (including token rotation) sit behind bearer auth.
    // A forgotten token can only be reset from the desktop Settings page —
    // an unauthenticated reset endpoint would let anyone on the network
    // overwrite the token and take over the API.
    //
    // Layer order: `route_layer` added later runs first on the request, so
    // `bearer_auth` (token check) wraps `session_guard` (single-user IP lease):
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
