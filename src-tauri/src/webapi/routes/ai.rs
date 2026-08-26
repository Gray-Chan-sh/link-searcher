use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    response::Json,
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post, put},
    Router,
};
use tauri::Manager;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

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
        .route("/api/ai/summarize", post(summarize_handler))
        .route("/api/ai/smart-search", post(smart_search_handler))
        .route(
            "/api/ai/smart-search/stream",
            post(smart_search_stream_handler),
        )
        .route(
            "/api/ai/conversation/ask",
            post(conversation_ask_handler),
        )
        .route(
            "/api/ai/conversation/ask/stream",
            post(conversation_ask_stream_handler),
        )
        .route("/api/chat/sessions/{id}", put(save_chat_session_handler))
        .route("/api/ai/gateways/test", get(test_gateway_handler))
        .route("/api/ai/cancel", post(cancel_ai_request_handler))
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
        if let Ok(Some(rec)) = tracker::get_file_by_id(&conn, &hit.file_id)
            && let Some(md5) = &rec.md5
                && let Ok(Some(text)) = tracker::get_content(&conn, md5)
                    && !text.trim().is_empty() {
                        docs.push(format!(
                            "【{}】\n{}",
                            rec.path,
                            crate::commands::ai::truncate_text(&text, 2000)
                        ));
                        sources.push(rec.path.clone());
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummarizeBody {
    #[serde(alias = "file_id")]
    file_id: String,
}

async fn summarize_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<SummarizeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let result = crate::commands::ai::summarize_file(app_state.clone(), body.file_id)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::to_value(&result).unwrap_or_default()))
}

fn str_field(body: &serde_json::Value, key: &str) -> Result<String, ApiError> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| ApiError {
            error: format!("missing {key}"),
        })
}

async fn smart_search_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = str_field(&body, "query")?;
    let app_state = state.app_handle.state::<AppState>();
    let result = crate::commands::ai::smart_search(app_state.clone(), query)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::to_value(&result).unwrap_or_default()))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AskBody {
    messages: Vec<crate::commands::ai::ChatMessage>,
    #[serde(default)]
    source_ids: Vec<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    scope: crate::commands::ai::TurnScope,
    #[serde(default)]
    session_retrieval_scope: Vec<String>,
    #[serde(default)]
    strict_docs: bool,
}

async fn conversation_ask_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<AskBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let answer = crate::commands::ai::conversation_ask(
        app_state.clone(),
        body.messages,
        body.source_ids,
        body.scope,
        body.session_retrieval_scope,
        body.strict_docs,
    )
    .await
    .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "answer": answer })))
}

/// SSE frames for one chat session: the AI command emits `ai-chunk`/`ai-done`
/// via Tauri events → webapi bridge → `event_tx`; filter back out by session id.
fn sse_for_session(
    rx: tokio::sync::broadcast::Receiver<(String, String)>,
    session_id: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(rx).filter_map(move |item| match item {
        Ok((name, payload)) => {
            if name != "ai-chunk" && name != "ai-done" {
                return None;
            }
            let mine = serde_json::from_str::<serde_json::Value>(&payload)
                .ok()
                .and_then(|v| {
                    v.get("session_id")?
                        .as_str()
                        .map(|s| s == session_id.as_str())
                })
                .unwrap_or(false);
            mine.then(|| Ok(Event::default().event(name).data(payload)))
        }
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

async fn smart_search_stream_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let query = str_field(&body, "query")?;
    if !crate::ai::llm_enabled() {
        return Err(ApiError {
            error: "AI service not configured".into(),
        });
    }
    if query.trim().is_empty() {
        return Err(ApiError {
            error: "问题不能为空".into(),
        });
    }
    let session_id = body
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Subscribe first: a later subscribe could miss early chunks.
    let rx = state.event_tx.subscribe();
    let handle = state.app_handle.clone();
    let sid = session_id.clone();
    tauri::async_runtime::spawn(async move {
        let app_state = handle.state::<AppState>();
        if let Err(e) =
            crate::commands::ai::smart_search_stream(app_state, handle.clone(), query, sid).await
        {
            log::warn!("[WEBAPI] smart-search stream failed: {e}");
        }
    });

    Ok(sse_for_session(rx, session_id))
}

async fn conversation_ask_stream_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<AskBody>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    if !crate::ai::llm_enabled() {
        return Err(ApiError {
            error: "AI service not configured".into(),
        });
    }
    if body.messages.is_empty() {
        return Err(ApiError {
            error: "对话不能为空".into(),
        });
    }
    let session_id = body
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let rx = state.event_tx.subscribe();
    let handle = state.app_handle.clone();
    let sid = session_id.clone();
    tauri::async_runtime::spawn(async move {
        let app_state = handle.state::<AppState>();
        if let Err(e) = crate::commands::ai::conversation_ask_stream(
            app_state,
            handle.clone(),
            body.messages,
            body.source_ids,
            sid,
            body.scope,
            body.session_retrieval_scope,
            body.strict_docs,
        )
        .await
        {
            log::warn!("[WEBAPI] conversation ask stream failed: {e}");
        }
    });

    Ok(sse_for_session(rx, session_id))
}

async fn save_chat_session_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_val = body.get("session").cloned().ok_or_else(|| ApiError {
        error: "missing session".into(),
    })?;
    let mut session: crate::commands::ai::ChatSession =
        serde_json::from_value(session_val).map_err(|e| ApiError {
            error: format!("invalid session: {e}"),
        })?;
    session.id = id;
    let app_state = state.app_handle.state::<AppState>();
    crate::commands::ai::save_chat_session(app_state.clone(), session)
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "saved": true })))
}

async fn test_gateway_handler() -> Result<Json<serde_json::Value>, ApiError> {
    let tests = crate::commands::ai::test_ai_gateway().await;
    Ok(Json(serde_json::to_value(&tests).unwrap_or_default()))
}

async fn cancel_ai_request_handler() -> Result<Json<serde_json::Value>, ApiError> {
    crate::commands::ai::cancel_ai_request()
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "cancelled": true })))
}