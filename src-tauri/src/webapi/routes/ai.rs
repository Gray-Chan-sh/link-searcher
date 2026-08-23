use axum::{
    extract::{Path, State},
    response::Json,
    routing::{delete, get, post},
    Router,
};
use tauri::Manager;

use crate::db::tracker;
use crate::state::AppState;
use crate::webapi::state::ApiState;

use super::ApiError;

pub fn router(_state: ApiState) -> Router<ApiState> {
    Router::new()
        .route("/api/ai/capabilities", get(ai_capabilities_handler))
        .route("/api/chat/ask", post(chat_ask_handler))
        .route("/api/chat/sessions", get(chat_sessions_handler))
        .route("/api/chat/sessions", post(chat_session_create_handler))
        .route("/api/chat/sessions/{id}", get(chat_session_load_handler))
        .route("/api/chat/sessions/{id}", delete(chat_session_delete_handler))
        .route(
            "/api/chat/sessions/{id}/export",
            post(chat_session_export_handler),
        )
}

async fn ai_capabilities_handler(
    State(_state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caps = crate::ai::AiCapabilities::from_gateways(crate::ai::capabilities());
    Ok(Json(serde_json::json!({
        "embedding": caps.embedding,
        "llm": caps.llm,
    })))
}

async fn chat_sessions_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let h = crate::commands::ai::read_history(&app_state.data_dir);
    let mut sessions: Vec<_> = h.sessions;
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let metas: Vec<serde_json::Value> = sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "title": s.title,
                "updated_at": s.updated_at,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "sessions": metas })))
}

async fn chat_session_create_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let id = crate::commands::ai::create_chat_session_impl(&app_state.data_dir)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn chat_session_load_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let h = crate::commands::ai::read_history(&app_state.data_dir);
    let session = h.sessions.into_iter().find(|s| s.id == id);
    match session {
        Some(s) => Ok(Json(serde_json::to_value(&s).unwrap_or_default())),
        None => Err(ApiError {
            error: "session not found".into(),
        }),
    }
}

async fn chat_session_delete_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let mut h = crate::commands::ai::read_history(&app_state.data_dir);
    h.sessions.retain(|s| s.id != id);
    crate::commands::ai::write_history(&app_state.data_dir, &h)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn chat_session_export_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let md = crate::commands::ai::export_chat_session_impl(&app_state.data_dir, &id)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let json = crate::commands::ai::export_chat_session_json_impl(&app_state.data_dir, &id)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({ "markdown": md, "json": json })))
}

async fn chat_ask_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let question = body
        .get("question")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError {
            error: "missing question".into(),
        })?
        .to_string();
    if !crate::ai::llm_enabled() {
        return Err(ApiError {
            error: "AI service not configured".into(),
        });
    }
    let app_state = state.app_handle.state::<AppState>();
    let hits = crate::commands::ai::bm25_relevant_hits(
        &app_state,
        &question.to_lowercase(),
        3,
        crate::ai::embedding_enabled(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .map_err(|e| ApiError {
        error: e.to_string(),
    })?;

    let conn = app_state
        .db
        .get()
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    let mut docs: Vec<String> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for hit in &hits {
        if let Ok(Some(rec)) = tracker::get_file_by_id(&conn, &hit.file_id) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!(
                            "【{}】\n{}",
                            rec.path,
                            crate::commands::ai::truncate_text(&text, 2000)
                        ));
                        sources.push(rec.path.clone());
                    }
                }
            }
        }
    }
    drop(conn);

    if docs.is_empty() {
        return Err(ApiError {
            error: "no relevant documents found".into(),
        });
    }

    let context = crate::commands::ai::truncate_text(&docs.join("\n\n---\n\n"), 50000);
    let system = format!(
        "你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。\n\n材料：\n{}",
        context
    );
    let answer = tokio::task::spawn_blocking(move || crate::ai::chat(&system, &question))
        .await
        .map_err(|e| ApiError {
            error: format!("task failed: {e}"),
        })?
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "answer": answer,
        "sources": sources,
    })))
}