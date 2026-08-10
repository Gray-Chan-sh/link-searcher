//! AI gateway commands: per-file summaries, cross-document Q&A (RAG),
//! smart search, and multi-turn conversation.

use serde::Serialize;
use tauri::{Emitter, State};

use crate::state::AppState;

/// Cancel the currently in-flight AI chat request. Marks a one-shot flag;
/// the running request completes in the background but its result is
/// discarded. Safe to call with no request in flight.
#[tauri::command]
pub async fn cancel_ai_request() -> Result<(), String> {
    crate::ai::cancel_ai();
    Ok(())
}

#[derive(Serialize)]
pub struct SummaryResult {
    pub file_id: String,
    pub summary: String,
    pub cached: bool,
}

/// Whether each gateway is usable right now (cached test, 30s TTL). The
/// frontend uses this to enable/disable AI entry points and show guidance.
#[tauri::command]
pub async fn ai_capabilities() -> crate::ai::AiCapabilities {
    // The underlying probe does blocking HTTP; run it off the UI thread so
    // a slow/hanging gateway cannot freeze the command.
    tokio::task::spawn_blocking(|| {
        crate::ai::AiCapabilities::from_gateways(crate::ai::capabilities())
    })
    .await
    .unwrap_or_default()
}

/// Connectivity test for the configured AI gateways. Returns one result per
/// gateway (embedding / llm); the frontend disables the corresponding
/// features when `ok` is false.
#[tauri::command]
pub async fn test_ai_gateway() -> Vec<crate::ai::GatewayTest> {
    tokio::task::spawn_blocking(crate::ai::test_gateways)
        .await
        .unwrap_or_default()
}

/// Generate (or fetch cached) an LLM summary for a file's extracted text.
#[tauri::command]
pub async fn summarize_file(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<SummaryResult, String> {
    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页填写 API Base URL".into());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    // Cached?
    if let Some(saved) = crate::db::tracker::get_summary(&conn, &file_id)
        .map_err(|e| format!("{e}"))?
    {
        return Ok(SummaryResult { file_id, summary: saved, cached: true });
    }

    // Resolve file record → content.
    let rec = crate::db::tracker::get_file_by_id(&conn, &file_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "file not found".to_string())?;
    let md5 = rec.md5.clone().ok_or_else(|| "no content hash".to_string())?;
    let text = crate::db::tracker::get_content(&conn, &md5)
        .map_err(|e| format!("{e}"))?
        .unwrap_or_default();
    drop(conn);

    if text.trim().is_empty() {
        return Err("该文件没有可摘要的文本内容".into());
    }
    let text = truncate_text(text.as_str(), 8000);

    let system = "你是文档摘要助手。用简洁的中文总结以下文档内容，突出主题、关键信息与结论，不超过150字。";
    let summary = tokio::task::spawn_blocking(move || crate::ai::chat(system, &text))
        .await
        .unwrap_or(None)
        .ok_or_else(|| "AI 请求失败（检查 API 配置或网络）".to_string())?;

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let _ = crate::db::tracker::upsert_summary(&conn, &file_id, &summary);
    Ok(SummaryResult { file_id, summary, cached: false })
}

/// Ask a question over one or more documents' extracted text (RAG).
#[tauri::command]
pub async fn ask_documents(
    state: State<'_, AppState>,
    file_ids: Vec<String>,
    question: String,
) -> Result<String, String> {
    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if question.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let mut docs: Vec<String> = Vec::new();
    for fid in &file_ids {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                    }
                }
            }
        }
    }
    drop(conn);

    if docs.is_empty() {
        return Err("所选文件没有可用的文本内容".into());
    }

    let context = docs.join("\n\n---\n\n");
    let user_msg = format!(
        "以下是从用户本地文档中提取的内容，请基于这些内容回答问题。若内容不足以回答，请明确说明。\n\n{context}\n\n问题：{question}",
        context = truncate_text(&context, 24000),
    );

    let system = "你是严谨的文档分析助手。仅基于提供的材料回答，不臆造事实，回答简洁有条理。";
    tokio::task::spawn_blocking(move || crate::ai::chat(system, &user_msg))
        .await
        .unwrap_or(None)
        .ok_or_else(|| "AI 请求失败（检查网关配置或网络）".to_string())
}

#[derive(Serialize)]
pub struct SmartSearchResponse {
    pub answer: String,
    pub source_ids: Vec<String>,
    pub source_files: Vec<String>,
}

/// Streaming AI payloads emitted over Tauri events (frontend listens and
/// renders incrementally).
#[derive(Clone, Serialize)]
struct AiChunk {
    session_id: String,
    delta: String,
}

#[derive(Clone, Serialize)]
struct AiDone {
    session_id: String,
    full_text: String,
    took_ms: u64,
    cancelled: bool,
    source_ids: Vec<String>,
    source_files: Vec<String>,
}

/// BM25 retrieval + content assembly shared by one-shot and streaming
/// smart_search. Returns the prompt pair plus the source file lists.
struct PreparedSmart {
    system: String,
    user_msg: String,
    source_ids: Vec<String>,
    source_files: Vec<String>,
}

fn prepare_smart_prompt(
    state: &tauri::State<'_, AppState>,
    query: &str,
) -> Result<PreparedSmart, String> {
    use crate::search::searcher::{SearchParams, SortField, SearcherWrap};

    let (context, source_ids, source_files) = {
        let mgr = state.index_manager.read().map_err(|e| format!("{e}"))?;
        let reader = mgr.reader().map_err(|e| format!("{e}"))?;
        let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
        drop(mgr);

        let params = SearchParams {
            // NL questions become an exact PhraseQuery if parsed verbatim
            // (Tantivy default) — re-tokenise as explicit OR so any term hits.
            query: crate::search::schema::split_query_terms(&query.to_lowercase()),
            dir_ids: None, file_ids: None, ext_filter: None,
            date_from: None, date_to: None,
            sort: SortField::Score, sort_order: "desc".to_string(),
            page: 1, page_size: 15, fuzzy: false, semantic: false,
        };
        let result = searcher.search(&params).map_err(|e| format!("{e}"))?;

        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let mut docs: Vec<String> = Vec::new();
        let mut sids: Vec<String> = Vec::new();
        let mut sf: Vec<String> = Vec::new();
        for hit in result.hits.iter().take(10) {
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id) {
                if let Some(md5) = &rec.md5 {
                    if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                        if !text.trim().is_empty() {
                            docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                            sids.push(hit.file_id.clone());
                            sf.push(rec.path.clone());
                        }
                    }
                }
            }
        }
        drop(conn);
        (docs.join("\n\n---\n\n"), sids, sf)
    };

    if context.trim().is_empty() {
        log::warn!("[AI] smart_search: no relevant document content found");
        return Err("未找到相关文档内容".into());
    }

    Ok(PreparedSmart {
        system: "你是严谨的文档分析助手。仅基于提供的材料回答，不臆造事实。回答简洁有条理，引用具体文件时标注来源。如果材料不足以回答，请明确说明。".into(),
        user_msg: format!("基于以下材料回答问题：\n\n{}\n\n问题：{}", context, query),
        source_ids,
        source_files,
    })
}

/// Search + RAG: use BM25 to find the most relevant documents, extract
/// their text, and let the LLM answer the query based on those materials.
/// Returns a textual answer plus the list of source files used.
#[tauri::command]
pub async fn smart_search(
    state: State<'_, AppState>,
    query: String,
) -> Result<SmartSearchResponse, String> {
    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if query.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    log::info!("[AI] smart_search: query={}", query);
    crate::ai::reset_ai_cancel();

    let PreparedSmart { system, user_msg, source_ids, source_files } =
        prepare_smart_prompt(&state, &query)?;

    let answer = tokio::task::spawn_blocking(move || crate::ai::chat(&system, &user_msg))
        .await
        .unwrap_or(None)
        .ok_or_else(|| {
            if crate::ai::ai_cancelled() {
                "请求已取消".to_string()
            } else {
                "AI 请求失败（检查网关配置或网络）".to_string()
            }
        })?;
    if crate::ai::ai_cancelled() {
        return Err("请求已取消".into());
    }
    log::info!(
        "[AI] smart_search: done, answer_chars={} sources={}",
        answer.chars().count(),
        source_ids.len()
    );

    Ok(SmartSearchResponse { answer, source_ids, source_files })
}

/// Streaming variant of [`smart_search`]: emits `ai-chunk` events as the
/// answer is generated and a final `ai-done`. Frontend renders incrementally.
#[tauri::command]
pub async fn smart_search_stream(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    query: String,
    session_id: String,
) -> Result<(), String> {
    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if query.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    log::info!("[AI] smart_search_stream: query={}", query);
    crate::ai::reset_ai_cancel();

    let PreparedSmart { system, user_msg, source_ids, source_files } =
        prepare_smart_prompt(&state, &query)?;
    let session_clone = session_id.clone();
    let app_inner = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut emit = |d: &str| {
            let _ = app_inner.emit("ai-chunk", AiChunk { session_id: session_clone.clone(), delta: d.to_string() });
        };
        crate::ai::chat_stream(&system, &user_msg, &mut emit)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    let _ = app.emit("ai-done", AiDone {
        session_id,
        full_text: result.text.unwrap_or_default(),
        took_ms: result.took_ms,
        cancelled: result.cancelled,
        source_ids,
        source_files,
    });
    Ok(())
}

/// BM25 retrieval returning top relevant `(file_id, path)` hits — tokenised
/// as explicit OR (a raw question would parse as an exact phrase and miss).
fn bm25_relevant_hits(
    state: &tauri::State<'_, AppState>,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, String> {
    use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
    let mgr = state.index_manager.read().map_err(|e| format!("{e}"))?;
    let reader = mgr.reader().map_err(|e| format!("{e}"))?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);

    let params = SearchParams {
        query: crate::search::schema::split_query_terms(&query.to_lowercase()),
        dir_ids: None, file_ids: None, ext_filter: None,
        date_from: None, date_to: None,
        sort: SortField::Score, sort_order: "desc".to_string(),
        page: 1, page_size: limit, fuzzy: false, semantic: false,
    };
    let result = searcher.search(&params).map_err(|e| format!("{e}"))?;

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut out = Vec::new();
    for hit in result.hits {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id) {
            if rec.status == "active" && rec.md5.is_some() {
                out.push((hit.file_id, rec.path));
            }
        }
    }
    drop(conn);
    Ok(out)
}

/// Assembled conversation prompt + the (possibly updated) source file list
/// backing it. Follow-up questions re-retrieve relevant documents so the
/// answer (and the frontend source list) reflects the newest question.
struct PreparedConversation {
    system: String,
    user_msg: String,
    source_ids: Vec<String>,
    source_files: Vec<String>,
}

fn prepare_conversation_prompt(
    state: &tauri::State<'_, AppState>,
    messages: &[ChatMessage],
    source_ids: &[String],
) -> Result<PreparedConversation, String> {
    let last_q = messages.last().map(|m| m.content.clone()).unwrap_or_default();
    // 动态依据：保留仍有效的旧来源，并按追问问题 BM25 命中补齐（去重, ≤15）。
    let new_hits = if last_q.trim().is_empty() {
        Vec::new()
    } else {
        bm25_relevant_hits(state, &last_q, 10)?
    };

    const MAX_SOURCES: usize = 15;
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 新检索优先：追问要主导依据更新（否则旧来源累积顶满上限,
    // 新文档永远挤不进来, 退化为"只围绕第一轮文档回答"）。
    for (fid, path) in new_hits {
        if seen.insert(fid.clone()) {
            merged.push((fid, path));
        }
    }
    // 旧来源的保留信号：*对话中提到过的文件*最值得留（用户/助手引用过 =
    // 实际在使用）；其次按最近加入倒序补槽。上限以内仅供上下文底垫。
    let message_text: String = messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ");
    let mentioned = |rec_path: &str| -> bool {
        let stem = std::path::Path::new(rec_path).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let name = std::path::Path::new(rec_path).file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        (!stem.is_empty() && message_text.contains(stem.as_str())) || (!name.is_empty() && message_text.contains(name.as_str()))
    };
    let keep_old = |merged: &mut Vec<(String, String)>, seen: &mut std::collections::HashSet<String>, skip_mentioned: bool| {
        for fid in source_ids.iter().rev() {
            if merged.len() >= MAX_SOURCES {
                break;
            }
            if seen.contains(fid) {
                continue;
            }
            let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) else { continue };
            if rec.status != "active" || rec.md5.is_none() {
                continue;
            }
            if mentioned(&rec.path) == skip_mentioned {
                continue;
            }
            seen.insert(fid.clone());
            merged.push((fid.clone(), rec.path.clone()));
        }
    };
    keep_old(&mut merged, &mut seen, false); // 第一遍: 对话中提到过的
    keep_old(&mut merged, &mut seen, true);  // 第二遍: 其余按最近补槽

    let mut docs: Vec<String> = Vec::new();
    for (fid, _) in &merged {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                    }
                }
            }
        }
    }
    drop(conn);

    let source_ids_final = merged.iter().map(|(id, _)| id.clone()).collect();
    let source_files_final = merged.iter().map(|(_, p)| p.clone()).collect();

    let context = truncate_text(&docs.join("\n\n---\n\n"), 24000);
    let system = format!("你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。如果材料不足以回答，请明确说明。\n\n材料：\n{}", context);
    let last_n = messages.len().saturating_sub(1);
    let user_msg = if messages.len() > 1 {
        let mut history_str = String::from("对话历史：\n");
        for m in messages.iter().take(last_n) {
            history_str.push_str(&format!("[{}] {}\n",
                if m.role == "user" { "用户" } else { "助手" },
                truncate_text(&m.content, 500)));
        }
        format!("{}\n当前问题：{}", history_str, last_q)
    } else {
        last_q
    };
    Ok(PreparedConversation { system, user_msg, source_ids: source_ids_final, source_files: source_files_final })
}

/// Multi-turn conversation: continue a chat using previously-selected
/// source documents as the knowledge base. `messages` includes the full
/// conversation history (alternating user/assistant roles).
#[tauri::command]
pub async fn conversation_ask(
    state: State<'_, AppState>,
    messages: Vec<ChatMessage>,
    source_ids: Vec<String>,
) -> Result<String, String> {
    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if messages.is_empty() {
        return Err("对话不能为空".into());
    }
    log::info!(
        "[AI] conversation_ask: messages={} source_ids={}",
        messages.len(),
        source_ids.len()
    );
    crate::ai::reset_ai_cancel();

    let PreparedConversation { system, user_msg, .. } =
        prepare_conversation_prompt(&state, &messages, &source_ids)?;
    let answer = tokio::task::spawn_blocking(move || crate::ai::chat(&system, &user_msg))
        .await
        .unwrap_or(None)
        .ok_or_else(|| {
            if crate::ai::ai_cancelled() {
                "请求已取消".to_string()
            } else {
                "AI 请求失败（检查网关配置或网络）".to_string()
            }
        })?;
    if crate::ai::ai_cancelled() {
        return Err("请求已取消".into());
    }
    log::info!("[AI] conversation_ask: done, answer_chars={}", answer.chars().count());

    Ok(answer)
}

/// Streaming variant of [`conversation_ask`]: emits `ai-chunk`/`ai-done`.
#[tauri::command]
pub async fn conversation_ask_stream(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    messages: Vec<ChatMessage>,
    source_ids: Vec<String>,
    session_id: String,
) -> Result<(), String> {
    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if messages.is_empty() {
        return Err("对话不能为空".into());
    }
    log::info!(
        "[AI] conversation_ask_stream: messages={} source_ids={}",
        messages.len(),
        source_ids.len()
    );
    crate::ai::reset_ai_cancel();

    let PreparedConversation { system, user_msg, source_ids, source_files } =
        prepare_conversation_prompt(&state, &messages, &source_ids)?;
    let session_clone = session_id.clone();
    let app_inner = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut emit = |d: &str| {
            let _ = app_inner.emit("ai-chunk", AiChunk { session_id: session_clone.clone(), delta: d.to_string() });
        };
        crate::ai::chat_stream(&system, &user_msg, &mut emit)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    let _ = app.emit("ai-done", AiDone {
        session_id,
        full_text: result.text.unwrap_or_default(),
        took_ms: result.took_ms,
        cancelled: result.cancelled,
        source_ids,
        source_files,
    });
    Ok(())
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

fn truncate_text(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars].iter().collect()
    }
}

fn chat_history_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("chat_history.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<ChatMessage>,
    pub source_ids: Vec<String>,
    pub source_files: Vec<String>,
    /// 进行中的 AI 请求（前端据此恢复"思考中"状态）。
    #[serde(default)]
    pub pending_query: Option<String>,
    #[serde(default)]
    pub pending_started_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatSessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct ChatHistoryFile {
    sessions: Vec<ChatSession>,
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn read_history(data_dir: &std::path::Path) -> ChatHistoryFile {
    let path = chat_history_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(c) => {
            // Migrate the legacy single-session structure ({messages,...} at
            // top level) into the multi-session layout: wrap it as one session.
            // The result is persisted immediately — otherwise every read would
            // mint a fresh random id and list/load would never agree.
            #[derive(serde::Deserialize)]
            struct Legacy {
                messages: Vec<ChatMessage>,
                source_ids: Vec<String>,
                source_files: Vec<String>,
            }
            if let Ok(legacy) = serde_json::from_str::<Legacy>(&c) {
                let now = now_ts();
                let title = legacy
                    .messages
                    .iter()
                    .find(|m| m.role == "user")
                    .map(|m| m.content.chars().take(20).collect::<String>())
                    .unwrap_or_else(|| "历史对话".to_string());
                let session = ChatSession {
                    id: uuid::Uuid::new_v4().to_string(),
                    title: if title.is_empty() { "历史对话".to_string() } else { title },
                    created_at: now,
                    updated_at: now,
                    messages: legacy.messages,
                    source_ids: legacy.source_ids,
                    source_files: legacy.source_files,
                    pending_query: None,
                    pending_started_at: None,
                };
                let migrated = ChatHistoryFile { sessions: vec![session] };
                let _ = write_history(data_dir, &migrated);
                return migrated;
            }
            serde_json::from_str(&c).unwrap_or_default()
        }
        Err(_) => ChatHistoryFile::default(),
    }
}

fn write_history(data_dir: &std::path::Path, h: &ChatHistoryFile) -> Result<(), String> {
    let path = chat_history_path(data_dir);
    let json = serde_json::to_string_pretty(h).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写入聊天记录失败: {e}"))
}

/// Generate a short title from the first user message (first ~20 chars).
fn title_from_first_message(session: &ChatSession) -> String {
    session
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| {
            let trimmed: String = m.content.chars().take(20).collect();
            if trimmed.is_empty() { "新会话".to_string() } else { trimmed }
        })
        .unwrap_or_else(|| "新会话".to_string())
}

/// List all chat sessions, newest first.
#[tauri::command]
pub fn list_chat_sessions(state: State<'_, AppState>) -> Result<Vec<ChatSessionMeta>, String> {
    let mut h = read_history(&state.data_dir);
    h.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(h.sessions
        .into_iter()
        .map(|s| ChatSessionMeta { id: s.id, title: s.title, updated_at: s.updated_at })
        .collect())
}

/// Create a new empty session. Returns its id.
#[tauri::command]
pub fn create_chat_session(state: State<'_, AppState>) -> Result<String, String> {
    let now = now_ts();
    let session = ChatSession {
        id: uuid::Uuid::new_v4().to_string(),
        title: "新会话".to_string(),
        created_at: now,
        updated_at: now,
        messages: vec![],
        source_ids: vec![],
        source_files: vec![],
        pending_query: None,
        pending_started_at: None,
    };
    let id = session.id.clone();
    let mut h = read_history(&state.data_dir);
    if h.sessions.len() >= 50 {
        h.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        h.sessions.pop();
    }
    h.sessions.push(session);
    write_history(&state.data_dir, &h)?;
    Ok(id)
}

/// Delete a session by id.
#[tauri::command]
pub fn delete_chat_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut h = read_history(&state.data_dir);
    h.sessions.retain(|s| s.id != id);
    write_history(&state.data_dir, &h)
}

/// Load a full session by id. Returns None if not found.
#[tauri::command]
pub fn load_chat_session(state: State<'_, AppState>, id: String) -> Result<Option<ChatSession>, String> {
    let h = read_history(&state.data_dir);
    Ok(h.sessions.into_iter().find(|s| s.id == id))
}

/// Save (create or update) a session. If the session is new or has no title,
/// derives a title from the first user message.
#[tauri::command]
pub fn save_chat_session(
    state: State<'_, AppState>,
    session: ChatSession,
) -> Result<(), String> {
    let mut h = read_history(&state.data_dir);
    let now = now_ts();
    let mut session = session;
    session.updated_at = now;
    if session.title.is_empty() || session.title == "新会话" {
        session.title = title_from_first_message(&session);
    }
    let exists = h.sessions.iter_mut().find(|s| s.id == session.id);
    match exists {
        Some(existing) => *existing = session,
        None => h.sessions.push(session),
    }
    write_history(&state.data_dir, &h)
}

/// Export a session as Markdown text (chat transcript with file refs).
#[tauri::command]
pub fn export_chat_session(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let h = read_history(&state.data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "会话不存在".to_string())?;

    let mut md = String::new();
    md.push_str(&format!("# {}\n\n", session.title));
    if let Some(first) = session.messages.first() {
        md.push_str(&format!("> 开始于 {}\n\n", first.content.chars().take(30).collect::<String>()));
    } else {
        md.push_str(&format!("> 空会话\n\n"));
    }
    for m in &session.messages {
        if m.role == "user" {
            md.push_str(&format!("## 问\n\n{}\n\n", m.content));
        } else {
            md.push_str(&format!("## 答\n\n{}\n\n", m.content));
        }
    }
    md.push_str("\n---\n**引用文件:**\n");
    for f in &session.source_files {
        md.push_str(&format!("- {}\n", f));
    }
    md.push_str(&format!("\n_导出时间: {}\n", now_ts()));
    Ok(md)
}
#[cfg(test)]
mod history_tests {
    use super::*;

    #[test]
    fn legacy_history_migrates_once_and_keeps_stable_id() {
        let dir = std::env::temp_dir().join(format!("ls_ai_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Legacy single-session top-level layout.
        let legacy = r#"{"messages":[{"role":"user","content":"hello"}],"source_files":["a.pdf"],"source_ids":["f1"]}"#;
        std::fs::write(chat_history_path(&dir), legacy).unwrap();

        // First read migrates (and persists) it as one session.
        let first = read_history(&dir);
        assert_eq!(first.sessions.len(), 1);
        let id = first.sessions[0].id.clone();

        // A second read must return the SAME id — list_chat_sessions and
        // load_chat_session are separate read_history calls.
        let second = read_history(&dir);
        assert_eq!(second.sessions.len(), 1);
        assert_eq!(second.sessions[0].id, id);

        // File is now the multi-session layout.
        let content = std::fs::read_to_string(chat_history_path(&dir)).unwrap();
        assert!(content.contains("\"sessions\""));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
