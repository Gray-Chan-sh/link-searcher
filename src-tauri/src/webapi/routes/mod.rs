pub mod search;
pub mod files;
pub mod index;
pub mod dirs;
pub mod ai;
pub mod settings;

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
    search::router(state.clone())
        .merge(files::router(state.clone()))
        .merge(index::router(state.clone()))
        .merge(dirs::router(state.clone()))
        .merge(ai::router(state.clone()))
        .merge(settings::router(state.clone()))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::bearer_auth,
        ))
        .fallback(static_files::serve_static)
        .with_state(state)
}