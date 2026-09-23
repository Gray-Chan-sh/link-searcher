use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
    http::StatusCode,
};
use tauri::Manager;
use crate::webapi::state::ApiState;

/// Length-independent, constant-time string equality.
///
/// Used to compare bearer tokens so that response timing does not leak
/// how many leading bytes of a guess matched the real token.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    // Fold the length difference into the same accumulator so neither the
    // length nor the content comparison short-circuits.
    let mut diff = a.len() ^ b.len();
    let max = a.len().max(b.len());
    for i in 0..max {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// Bearer token auth middleware — reads from ApiState.
pub async fn bearer_auth(
    State(state): State<ApiState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let header_token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if header_token.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let db_token = {
        let app_state = state.app_handle.state::<crate::state::AppState>();
        app_state.db.get().ok().and_then(|conn| {
            conn.query_row::<String, _, _>(
                "SELECT value FROM app_settings WHERE key = 'web_api_token'",
                [],
                |r| r.get(0),
            ).ok()
        })
    };
    if let Some(db_token) = &db_token
        && constant_time_eq(header_token, db_token) {
            return Ok(next.run(req).await);
        }
    Err(StatusCode::UNAUTHORIZED)
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn equal_tokens_match() {
        assert!(constant_time_eq("deadbeef", "deadbeef"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn different_tokens_do_not_match() {
        assert!(!constant_time_eq("deadbeef", "deadbeee"));
        assert!(!constant_time_eq("deadbeef", "deadbee"));
        assert!(!constant_time_eq("deadbeef", "deadbeeff"));
        assert!(!constant_time_eq("", "x"));
        assert!(!constant_time_eq("x", ""));
    }

    #[test]
    fn prefix_padding_does_not_falsely_match() {
        // Length difference that would wrap a u8 accumulator plus a shared
        // prefix followed by NUL padding — must not compare equal.
        let short = "x";
        let long = format!("x{}", "\0".repeat(255));
        assert!(!constant_time_eq(short, &long));
    }
}