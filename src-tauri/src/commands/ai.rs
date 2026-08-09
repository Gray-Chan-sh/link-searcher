//! AI gateway commands: per-file summaries, cross-document Q&A (RAG),
//! smart search, and multi-turn conversation.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

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

/// Search + RAG: use BM25 to find the most relevant documents, extract
/// their text, and let the LLM answer the query based on those materials.
/// Returns a textual answer plus the list of source files used.
#[tauri::command]
pub async fn smart_search(
    state: State<'_, AppState>,
    query: String,
) -> Result<SmartSearchResponse, String> {
    use crate::search::searcher::{SearchParams, SortField, SearcherWrap};

    if !crate::ai::llm_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if query.trim().is_empty() {
        return Err("问题不能为空".into());
    }

    // ── BM25 + content extraction (sync, must complete before any await) ──
    let (context, source_ids, source_files) = {
        let mgr = state.index_manager.read().map_err(|e| format!("{e}"))?;
        let reader = mgr.reader().map_err(|e| format!("{e}"))?;
        let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
        drop(mgr);

        let params = SearchParams {
            query: query.to_lowercase(),
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
        return Err("未找到相关文档内容".into());
    }

    let system = "你是严谨的文档分析助手。仅基于提供的材料回答，不臆造事实。回答简洁有条理，引用具体文件时标注来源。如果材料不足以回答，请明确说明。";
    let user_msg = format!("基于以下材料回答问题：\n\n{}\n\n问题：{}", context, query);

    let answer = tokio::task::spawn_blocking(move || crate::ai::chat(system, &user_msg))
        .await
        .unwrap_or(None)
        .ok_or_else(|| "AI 请求失败（检查网关配置或网络）".to_string())?;

    Ok(SmartSearchResponse { answer, source_ids, source_files })
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

    // Reload document context from the previously-identified sources.
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut docs: Vec<String> = Vec::new();
    for fid in &source_ids {
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

    let context = docs.join("\n\n---\n\n");
    let context = truncate_text(&context, 24000);

    // Build the full message array: system + context note + history + current question.
    // The last message in `messages` is the latest user query.
    let system = format!("你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。如果材料不足以回答，请明确说明。\n\n材料：\n{}", context);
    let last_q = messages.last().map(|m| m.content.clone()).unwrap_or_default();
    let last_n = messages.len().saturating_sub(1);

    // The conversation LLM call sends the full history as separate user/assistant
    // turns (mimicking OpenAI chat format).
    let answer = tokio::task::spawn_blocking(move || {
        // Simplified: concatenate history + current question into a single prompt.
        // Full multi-turn chat API support requires switching to /chat/completions
        // with messages array — which we already have in ai::chat. But chat()
        // currently takes system+user only. For true multi-turn we'd add a
        // messages-based variant. For now, include the last question and a short
        // prefix of the history as user context.
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
        crate::ai::chat(&system, &user_msg)
    })
        .await
        .unwrap_or(None)
        .ok_or_else(|| "AI 请求失败（检查网关配置或网络）".to_string())?;

    Ok(answer)
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
        Ok(c) => serde_json::from_str(&c).unwrap_or_default(),
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