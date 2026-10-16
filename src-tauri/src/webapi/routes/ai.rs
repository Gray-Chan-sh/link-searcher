use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    response::Json,
    response::sse::{Event, KeepAlive, Sse},
    routing::{delete, get, post, put},
    Router,
};
use serde::Deserialize;
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
        .route(
            "/api/chat/sessions/{id}/rename",
            put(rename_chat_session_handler),
        )
        .route("/api/ai/gateways/test", get(test_gateway_handler))
        .route("/api/ai/cancel", post(cancel_ai_request_handler))
        .route("/api/ai/events", get(ai_events_handler))
        .route("/api/ai/events/turn", get(turn_ai_events_handler))
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

#[derive(serde::Deserialize)]
struct AiEventsQuery {
    session_id: String,
}

async fn ai_events_handler(
    State(state): State<ApiState>,
    Query(q): Query<AiEventsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let events = crate::db::ai_events::get_session_events(&conn, &q.session_id)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::to_value(events).map_err(|e| ApiError { error: e.to_string() })?))
}

#[derive(serde::Deserialize)]
struct TurnAiEventsQuery {
    session_id: String,
    turn_number: usize,
}

async fn turn_ai_events_handler(
    State(state): State<ApiState>,
    Query(q): Query<TurnAiEventsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let events = crate::db::ai_events::get_turn_events(&conn, &q.session_id, q.turn_number)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::to_value(events).map_err(|e| ApiError { error: e.to_string() })?))
}

/// 会话来源端：默认 `web`（浏览器），可用 `?origin=lark` 覆盖，供飞书 bot 使用。
///
/// 单用户 + Bearer token 模型下这**不构成权限边界**（§5.5 阶段二：隔离是**组织**需要，
/// 不是安全边界）；校验只为挡住拼写错误 —— 非法值**报错**而不静默回退。
#[derive(Deserialize, Default)]
struct OriginQuery {
    #[serde(default)]
    origin: Option<String>,
}

fn session_origin(q: &OriginQuery) -> Result<String, ApiError> {
    match q.origin.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(crate::commands::ai::ORIGIN_WEB.to_string()),
        Some(v) => {
            match crate::commands::ai::parse_origin(Some(v)).map_err(|e| ApiError { error: e })? {
                Some(o) => Ok(o),
                None => Ok(crate::commands::ai::ORIGIN_WEB.to_string()),
            }
        }
    }
}

async fn chat_sessions_handler(
    State(state): State<ApiState>,
    Query(q): Query<OriginQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    // §5.5 阶段二：默认只见本端会话；`?origin=lark` 让飞书 bot 也能用同一族路由。
    let origin = session_origin(&q)?;
    let sessions =
        crate::commands::ai::list_chat_sessions_filtered(&app_state.data_dir, Some(&origin));
    let metas: Vec<serde_json::Value> = sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "title": s.title,
                "updated_at": s.updated_at,
                "origin": s.origin,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "sessions": metas })))
}

async fn chat_session_create_handler(
    State(state): State<ApiState>,
    Query(q): Query<OriginQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let origin = session_origin(&q)?;
    let id = crate::commands::ai::create_chat_session_impl(&app_state.data_dir, &origin)
        .map_err(|e| ApiError {
            error: e.to_string(),
        })?;
    Ok(Json(serde_json::json!({ "id": id, "origin": origin })))
}

async fn chat_session_load_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<OriginQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let origin = session_origin(&q)?;
    // 跨端的会话按"不存在"处理 —— 不向别的端泄漏"这个 id 是否存在"。
    let session = crate::commands::ai::find_chat_session(&app_state.data_dir, &id, Some(&origin));
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
    Query(q): Query<OriginQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let origin = session_origin(&q)?;
    crate::commands::ai::delete_chat_session_impl(&app_state.data_dir, id, Some(&origin))
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// 导出请求体：`{"turns":[1,3]}`；缺省/为空表示导出全部轮次。
#[derive(Deserialize, Default)]
#[serde(default)]
struct ExportBody {
    turns: Option<Vec<usize>>,
}

async fn chat_session_export_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<OriginQuery>,
    axum::Json(body): axum::Json<ExportBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let origin = session_origin(&q)?;
    let md = crate::commands::ai::export_chat_session_impl(
        &app_state.data_dir,
        &id,
        body.turns.as_deref(),
        Some(&origin),
    )
    .map_err(|e| ApiError {
        error: e.to_string(),
    })?;
    let json = crate::commands::ai::export_chat_session_json_impl(
        &app_state.data_dir,
        &id,
        Some(&app_state.db),
        body.turns.as_deref(),
        Some(&origin),
    )
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
    #[serde(default)]
    full_recall: bool,
    /// 重跑（飞书卡片「换一批证据」）：上一轮回答正文。`Some` ⇒ 服务端**换一批检索词**
    /// 重新检索（见 `commands::ai::prompt` 的 retry 分支），而不是原词再搜一遍。
    #[serde(default)]
    retry_prior_answer: Option<String>,
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
        Some(body.full_recall),
        None,
        body.retry_prior_answer,
    )
    .await
    .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "answer": answer })))
}

/// SSE frames for one chat session: the AI command emits
/// `ai-chunk`/`ai-progress`/`ai-done` via Tauri events → webapi bridge →
/// `event_tx`; filter back out by session id.
fn sse_for_session(
    rx: tokio::sync::broadcast::Receiver<(String, String)>,
    session_id: String,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(rx).filter_map(move |item| match item {
        Ok((name, payload)) => {
            if name != "ai-chunk" && name != "ai-progress" && name != "ai-done" {
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
            Some(body.full_recall),
            None,
            body.retry_prior_answer,
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
    Query(q): Query<OriginQuery>,
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
    let origin = session_origin(&q)?;
    crate::commands::ai::save_chat_session_impl(&app_state.data_dir, session, Some(&origin))
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "saved": true })))
}

#[derive(serde::Deserialize)]
struct RenameChatSessionBody {
    title: String,
}

/// **只改标题**（`/rename` 的落点；§11.6）。
///
/// 为什么不复用 `PUT /api/chat/sessions/{id}`：那条路要求把**整个会话载荷**送回来
/// （含全部消息），对"只想改个名字"的调用方既笨重又有"旧载荷覆盖新消息"的竞态风险。
/// 这里只碰标题字段，且与其他写路径一样受 `origin` 约束（跨端拒绝）。
async fn rename_chat_session_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<OriginQuery>,
    Json(body): Json<RenameChatSessionBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let origin = session_origin(&q)?;
    let saved = crate::commands::ai::rename_chat_session_impl(
        &app_state.data_dir,
        &id,
        &body.title,
        Some(&origin),
    )
    .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "ok": true, "title": saved })))
}

async fn test_gateway_handler() -> Result<Json<serde_json::Value>, ApiError> {
    let tests = crate::commands::ai::test_ai_gateway().await;
    Ok(Json(serde_json::to_value(&tests).unwrap_or_default()))
}

/// Optional body: `{ "session_id": "..." }` (the `sessionId` spelling is also
/// accepted). Omitting it cancels every in-flight generation, which is what
/// callers predating per-session cancellation send (`docs/LARK_BOT.md` §5.5
/// decision D3).
#[derive(serde::Deserialize, Default)]
struct CancelAiBody {
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
}

async fn cancel_ai_request_handler(
    body: Option<Json<CancelAiBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = body.and_then(|Json(b)| b.session_id);
    crate::commands::ai::cancel_ai_request(session_id)
        .await
        .map_err(|e| ApiError { error: e })?;
    Ok(Json(serde_json::json!({ "cancelled": true })))
}