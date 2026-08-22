use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
    http::StatusCode,
};
use crate::webapi::state::ApiState;

/// Bearer token auth middleware — reads from ApiState.
pub async fn bearer_auth(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if token.is_empty() || token != state.auth_token.as_str() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}