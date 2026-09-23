//! Chat session persistence: history file, session CRUD, Markdown/JSON export,
//! and AI event serialization.

use serde::Serialize;
use tauri::State;

use super::{ChatMessage, EvidenceItem, truncate_text};
use crate::state::AppState;
pub fn chat_history_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("chat_history.json")
}

/// One turn's source file references, recorded when a conversation turn
/// completes so the session history can show which documents backed each
/// user/assistant exchange. `items` carries the traceable evidence
/// (scores, rewrite info) for that turn.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PerTurnEvidence {
    pub turn_index: usize,
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub items: Vec<EvidenceItem>,
    /// Unique per-turn trace ID — correlates logs with this turn in the session JSON export.
    #[serde(default)]
    pub trace_id: String,
    /// LLM generation time in ms.
    #[serde(default)]
    pub took_ms: u64,
    /// Active LLM model ID at generation time.
    #[serde(default)]
    pub llm_model: String,
    /// Active embedding model ID at retrieval time.
    #[serde(default)]
    pub embedding_model: String,
    /// Final retrieval query string (after any LLM/rule-based rewrite).
    #[serde(default)]
    pub search_query: String,
    /// Number of BM25 hits returned (before merge with @mention files).
    #[serde(default)]
    pub hits: usize,
}

/// 每轮最终的 @mention 生效集合（含继承解析后），持久化供 `@第N轮` 引用。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PerTurnScope {
    pub turn_index: usize,
    /// 该轮发送时的完整检索范围快照（跨轮累计合并后），用于导出追溯。
    #[serde(default)]
    pub scope: Vec<String>,
}

/// 本轮的 @mention 集合与继承声明，由前端解析输入文本后传入。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TurnScope {
    /// 本轮显式 @ 的文件路径。
    #[serde(default)]
    pub mention_files: Vec<String>,
    /// 本轮显式 @ 的目录路径。
    #[serde(default)]
    pub mention_dirs: Vec<String>,
    /// 显式继承的轮次索引（0‑based，`@第2轮` → `[1]`）。
    #[serde(default)]
    pub inherit_from: Vec<usize>,
    /// 结构化条件（ext/date/模糊）。
    #[serde(default)]
    pub conditions: Vec<ScopeCondition>,
}

/// 一条范围内条件。`parsed` 仅 fuzzy 由 LLM 解析后填充，前端可展示编辑。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScopeCondition {
    pub kind: String, // "ext" | "date" | "fuzzy"
    pub value: String,
    #[serde(default)]
    pub parsed: Option<String>,
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
    /// 每轮问答用到的 source 文件引用（0‑based turn index）。
    #[serde(default)]
    pub per_turn_evidence: Vec<PerTurnEvidence>,
    /// 每轮检索范围快照（0‑based turn index → 该轮发送时的完整范围）。
    #[serde(default)]
    pub per_turn_scopes: Vec<PerTurnScope>,
    /// 会话级统一检索范围：跨轮累计的路径条目（目录/文件统一），直到手动删除。
    /// 每轮发送时以其为基准；父路径自动吞并子路径（合并去冗余）。
    #[serde(default)]
    pub retrieval_scope: Vec<String>,
    /// P2 严格模式：范围内无命中时拒绝回答（会话级，可切换）。
    /// 默认 true：旧会话（该功能加入前）无此字段，缺失时应沿用
    /// 功能初衷"仅依据引用文档"，而非静默退化为全库检索。
    #[serde(default = "default_strict_docs")]
    pub strict_docs: bool,
}

fn default_strict_docs() -> bool { true }


#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatSessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChatHistoryFile {
    pub sessions: Vec<ChatSession>,
}

pub(super) fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn read_history(data_dir: &std::path::Path) -> ChatHistoryFile {
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
                    per_turn_evidence: vec![],
                    per_turn_scopes: vec![],
                    retrieval_scope: vec![],
        strict_docs: true,
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

pub fn write_history(data_dir: &std::path::Path, h: &ChatHistoryFile) -> Result<(), String> {
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
pub fn list_chat_sessions(state: State<'_, AppState>) -> Result<Vec<ChatSessionMeta>, String> {
    let mut h = read_history(&state.data_dir);
    h.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(h.sessions
        .into_iter()
        .map(|s| ChatSessionMeta { id: s.id, title: s.title, updated_at: s.updated_at })
        .collect())
}

/// Create a new empty session. Returns its id.
pub fn create_chat_session(state: State<'_, AppState>) -> Result<String, String> {
    create_chat_session_impl(&state.data_dir)
}

pub fn create_chat_session_impl(data_dir: &std::path::Path) -> Result<String, String> {
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
        per_turn_evidence: vec![],
        per_turn_scopes: vec![],
        retrieval_scope: vec![],
        strict_docs: false,
    };
    let id = session.id.clone();
    let mut h = read_history(data_dir);
    if h.sessions.len() >= 50 {
        h.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        h.sessions.pop();
    }
    h.sessions.push(session);
    write_history(data_dir, &h)?;
    Ok(id)
}

/// Delete a session by id.
pub fn delete_chat_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut h = read_history(&state.data_dir);
    h.sessions.retain(|s| s.id != id);
    write_history(&state.data_dir, &h)
}

/// Load a full session by id. Returns None if not found.
pub fn load_chat_session(state: State<'_, AppState>, id: String) -> Result<Option<ChatSession>, String> {
    let h = read_history(&state.data_dir);
    Ok(h.sessions.into_iter().find(|s| s.id == id))
}

/// Save (create or update) a session. If the session is new or has no title,
/// derives a title from the first user message.
pub fn save_chat_session(
    state: State<'_, AppState>,
    session: ChatSession,
) -> Result<(), String> {
    save_chat_session_impl(&state.data_dir, session)
}

pub(super) fn save_chat_session_impl(data_dir: &std::path::Path, session: ChatSession) -> Result<(), String> {
    let mut h = read_history(data_dir);
    let now = now_ts();
    let mut session = session;
    session.updated_at = now;
    if session.title.is_empty() || session.title == "新会话" {
        session.title = title_from_first_message(&session);
    }
    let exists = h.sessions.iter_mut().find(|s| s.id == session.id);
    match exists {
        Some(existing) => {
            if session.created_at == 0 {
                session.created_at = existing.created_at;
            }
            *existing = session
        }
        None => {
            if session.created_at == 0 {
                session.created_at = now;
            }
            h.sessions.push(session)
        }
    }
    write_history(data_dir, &h)
}

/// Format a single evidence item as Markdown.
fn fmt_evidence_item(e: &EvidenceItem, index: usize) -> String {
    let scores: Vec<String> = std::iter::empty()
        .chain(e.bm25_score.map(|s| format!("BM25 {s:.2}")))
        .chain(e.semantic_score.map(|s| format!("语义 {s:.2}")))
        .chain(e.rrf_score.map(|s| format!("RRF {s:.2}")))
        .collect();
    let score_str = if scores.is_empty() { String::new() } else { format!("（{}）", scores.join(" · ")) };
    let no = if e.material_no == 0 { index } else { e.material_no };
    let mut out = format!("{no}. 📄 `{}` {score_str}", e.path);
    if e.rewritten
        && let Some(q) = &e.rewritten_query {
            out.push_str(&format!("\n    ↳ 查询改写: `{q}`"));
        }
    if e.from_history {
        out.push_str("\n    ↳ 来自历史来源");
    }
    if !e.snippet.is_empty() {
        out.push_str(&format!("\n    ↳ 片段: {}", truncate_text(&e.snippet, 120)));
    }
    out
}

/// Export a session as Markdown text (chat transcript with full traceability:
/// per-turn references, retrieval evidence, strict/focus mode, timestamps).
/// `selected_turns`（1-based turn_index）为 None 时导出全部轮次。
pub fn export_chat_session(
    state: State<'_, AppState>,
    id: String,
    turns: Option<Vec<usize>>,
) -> Result<String, String> {
    export_chat_session_impl(&state.data_dir, &id, turns.as_deref())
}

pub fn export_chat_session_impl(
    data_dir: &std::path::Path,
    id: &str,
    selected_turns: Option<&[usize]>,
) -> Result<String, String> {
    let h = read_history(data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "会话不存在".to_string())?;

    let config = crate::config::load_config();
    let mut md = String::new();
    let now = now_ts();
    md.push_str(&format!("# {}\n\n", session.title));
    md.push_str("## 追溯信息\n\n");
    md.push_str(&format!("> - 会话 ID: `{}`\n", session.id));
    md.push_str(&format!("> - 创建时间: {}\n", session.created_at));
    md.push_str(&format!("> - 导出时间: {}\n", now));
    md.push_str(&format!("> - LLM 模型: `{}`\n", config.active_llm_model_id));
    md.push_str(&format!("> - Embedding 模型: `{}`\n", config.active_embedding_model_id));
    md.push_str(&format!("> - 语义权重: {:.0}%\n\n", config.semantic_weight * 100.0));

    if session.strict_docs {
        md.push_str("> ⚙️ 严格模式（仅依据文档）：开启\n");
    }
    if !session.retrieval_scope.is_empty() {
        md.push_str("> 📁 检索范围: ");
        for p in &session.retrieval_scope {
            md.push_str(&format!("`{}` ", p));
        }
        md.push('\n');
    }
    md.push('\n');
    // 按轮索引分组：user 消息和它的 evidence/scope
    let mut turn_idx = 0usize;
    for (i, m) in session.messages.iter().enumerate() {
        if m.role == "user" {
            turn_idx += 1;
            // 轮次选择导出：未勾选的轮次整段跳过（含其问题、范围、依据与回答）。
            if let Some(sel) = selected_turns
                && !sel.contains(&turn_idx) {
                    continue;
                }
            md.push_str(&format!("---\n\n## 第 {turn_idx} 轮\n\n### 问\n\n{}\n", m.content));
            // 本轮范围快照（跨轮累计合并后）
            if let Some(sc) = session.per_turn_scopes.iter().find(|s| s.turn_index == turn_idx - 1) {
                md.push_str("**检索范围:**\n");
                if sc.scope.is_empty() {
                    md.push_str("- 未指定（全库）\n");
                } else {
                    for p in &sc.scope {
                        let label = if p.is_empty() { "全库" } else { p.as_str() };
                        md.push_str(&format!("- `{}`\n", label));
                    }
                }
            }
            // 本轮追溯元数据
            let per_turn = session.per_turn_evidence.iter().find(|e| e.turn_index == turn_idx - 1);
            if let Some(ev) = per_turn {
                if !ev.trace_id.is_empty() {
                    md.push_str(&format!("- **Trace ID**: `{}`\n", ev.trace_id));
                }
                if ev.took_ms > 0 {
                    md.push_str(&format!("- **生成耗时**: {}ms\n", ev.took_ms));
                }
                if !ev.llm_model.is_empty() {
                    md.push_str(&format!("- **LLM 模型**: `{}`\n", ev.llm_model));
                }
                if !ev.embedding_model.is_empty() {
                    md.push_str(&format!("- **Embedding 模型**: `{}`\n", ev.embedding_model));
                }
                if !ev.search_query.is_empty() {
                    md.push_str(&format!("- **最终检索查询**: `{}`\n", ev.search_query));
                }
                if ev.hits > 0 {
                    md.push_str(&format!("- **BM25 命中数**: {}\n", ev.hits));
                }
            }
            // 找下一条 assistant 消息
            if let Some(assistant_msg) = session.messages.get(i + 1).filter(|m| m.role == "assistant") {
                md.push_str(&format!("\n### 答\n\n{}\n", assistant_msg.content));
                // 本轮检索依据
                if let Some(ev) = per_turn
                    && !ev.items.is_empty() {
                        md.push_str(&format!("\n**检索依据（{}）:**\n", ev.items.len()));
                        for (j, item) in ev.items.iter().enumerate() {
                            md.push_str(&format!("{}\n", fmt_evidence_item(item, j + 1)));
                        }
                    }
            }
        }
    }
    md.push_str("\n---\n");
    md.push_str(&format!("\n_导出时间: {}\n", now));
    Ok(md)
}

/// JSON 导出结构：每轮含完整范围快照（空=未指定）与依据原始字段，供程序化分析。
#[derive(serde::Serialize)]
struct ChatExportJson {
    schema_version: u32,
    exported_at: i64,
    session: SessionExportMeta,
    turns: Vec<TurnExport>,
}

#[derive(serde::Serialize)]
struct SessionExportMeta {
    id: String,
    title: String,
    created_at: i64,
    strict_docs: bool,
    retrieval_scope: Vec<String>,
    /// 当前语义融合权重（0=纯关键词，1=纯语义）。
    semantic_weight: f64,
    /// 当前激活 LLM 模型 ID。
    llm_model: String,
    /// 当前激活 Embedding 模型 ID。
    embedding_model: String,
}

#[derive(serde::Serialize)]
struct TurnExport {
    turn_index: usize,
    scope: Vec<String>,
    question: String,
    answer: Option<String>,
    evidence: Vec<EvidenceItem>,
    /// 本轮唯一追溯 ID（日志关联键）。
    trace_id: String,
    /// 本轮生成耗时（毫秒）。
    took_ms: u64,
    /// 本轮使用的 LLM 模型 ID。
    llm_model: String,
    /// 本轮使用的 Embedding 模型 ID。
    embedding_model: String,
    /// 改写后的最终检索查询。
    search_query: String,
    /// BM25 合并前命中数。
    hits: usize,
    /// 本轮 RAG 管线日志（query rewrite/scope/retrieval/context/LLM 调用等
    /// 结构化事件，来自 ai_events 表；旧会话无记录时省略该键）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    log: Vec<AiEventJson>,
}

/// 导出会话为 JSON（分析友好）：空范围轮次 scope=[] ，无回答轮次 answer=null。
/// 每轮附带 `log` 段（ai_events 表中的结构化管线事件，需传 DB pool；为
/// None 时省略 log 段，便于无 DB 的纯历史导出场景）。
pub fn export_chat_session_json(
    state: State<'_, AppState>,
    id: String,
    turns: Option<Vec<usize>>,
) -> Result<String, String> {
    export_chat_session_json_impl(&state.data_dir, &id, Some(&state.db), turns.as_deref())
}

pub fn export_chat_session_json_impl(
    data_dir: &std::path::Path,
    id: &str,
    db: Option<&r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>,
    selected_turns: Option<&[usize]>,
) -> Result<String, String> {
    let h = read_history(data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "会话不存在".to_string())?;

    // Load per-turn structured pipeline events (ai_events, 0-based turn_number).
    let mut events_by_turn: std::collections::HashMap<usize, Vec<AiEventJson>> =
        std::collections::HashMap::new();
    if let Some(pool) = db
        && let Ok(conn) = pool.get()
        && let Ok(events) = crate::db::ai_events::get_session_events(&conn, &session.id)
    {
        for e in events.iter().map(ai_event_to_json) {
            events_by_turn.entry(e.turn_number).or_default().push(e);
        }
    }

    let mut turns = Vec::new();
    let mut turn_idx = 0usize;
    for (i, m) in session.messages.iter().enumerate() {
        if m.role == "user" {
            turn_idx += 1;
            // 轮次选择导出：未勾选的轮次不进入 turns 数组（turn_index 保持原始编号）。
            if let Some(sel) = selected_turns
                && !sel.contains(&turn_idx) {
                    continue;
                }
            let scope = session
                .per_turn_scopes
                .iter()
                .find(|s| s.turn_index == turn_idx - 1)
                .map(|s| s.scope.clone())
                .unwrap_or_default();
            let answer = session
                .messages
                .get(i + 1)
                .filter(|m| m.role == "assistant")
                .map(|m| m.content.clone());
            // Pull per‑turn traceability data (defaults to zero‑valued for legacy sessions).
            let per_turn = session
                .per_turn_evidence
                .iter()
                .find(|e| e.turn_index == turn_idx - 1);
            let trace_id = per_turn.map(|e| e.trace_id.clone()).unwrap_or_default();
            let took_ms = per_turn.map(|e| e.took_ms).unwrap_or(0);
            let llm_model = per_turn.map(|e| e.llm_model.clone()).unwrap_or_default();
            let embedding_model = per_turn.map(|e| e.embedding_model.clone()).unwrap_or_default();
            let search_query = per_turn.map(|e| e.search_query.clone()).unwrap_or_default();
            let hits = per_turn.map(|e| e.hits).unwrap_or(0);
            let evidence = per_turn.map(|e| e.items.clone()).unwrap_or_default();
            // ai_events turn_number 是 0-based（运行时 user 消息计数-1），
            // 与导出的 turn_index-1 对齐。
            let log = events_by_turn
                .get(&(turn_idx - 1))
                .cloned()
                .unwrap_or_default();
            turns.push(TurnExport {
                turn_index: turn_idx,
                scope,
                question: m.content.clone(),
                answer,
                evidence,
                trace_id,
                took_ms,
                llm_model,
                embedding_model,
                search_query,
                hits,
                log,
            });
        }
    }

    let config = crate::config::load_config();
    let export = ChatExportJson {
        schema_version: 3,
        exported_at: now_ts(),
        session: SessionExportMeta {
            id: session.id,
            title: session.title,
            created_at: session.created_at,
            strict_docs: session.strict_docs,
            retrieval_scope: session.retrieval_scope,
            semantic_weight: config.semantic_weight,
            llm_model: config.active_llm_model_id.clone(),
            embedding_model: config.active_embedding_model_id.clone(),
        },
        turns,
    };
    serde_json::to_string_pretty(&export).map_err(|e| format!("JSON 序列化失败: {e}"))
}

#[derive(Serialize, Clone)]
pub struct AiEventJson {
    pub id: i64,
    pub session_id: String,
    pub turn_number: usize,
    pub event_seq: u32,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
    /// 事件时间（本地时区可读格式，如 `2026-09-10 13:15:08`）。
    pub created_at_readable: String,
}

/// Convert a stored [`crate::db::ai_events::AiEvent`] row to its JSON shape.
pub(crate) fn ai_event_to_json(e: &crate::db::ai_events::AiEvent) -> AiEventJson {
    AiEventJson {
        id: e.id,
        session_id: e.session_id.clone(),
        turn_number: e.turn_number,
        event_seq: e.event_seq,
        event_type: e.event_type.clone(),
        payload: e.payload.clone(),
        created_at: e.created_at,
        created_at_readable: fmt_event_time(e.created_at),
    }
}

/// Format a Unix timestamp (seconds) as local-time `YYYY-MM-DD HH:MM:SS`.
fn fmt_event_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// 从（可能被 50k 截断的）context 反推实际可见的最大材料编号。
/// 材料块格式为 `[N]（路径）…`；无匹配返回 0。
pub(super) fn visible_material_numbers(context: &str) -> std::collections::BTreeSet<usize> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        // nosemgrep: rust-expect-panic — compile-time verified regex literal
        regex::Regex::new(r"\[(\d+)\]（").expect("material-number regex is valid")
    });
    re.captures_iter(context)
        .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse::<usize>().ok()))
        .collect()
}

/// Session-stable material numbers: distinct file_ids in first-appearance order
/// across the session's persisted per-turn evidence; index + 1 = stable `[N]`.
pub(super) fn prior_material_order(state: &AppState, session_id: &str) -> Vec<String> {
    if session_id.is_empty() { return Vec::new(); }
    let h = read_history(&state.data_dir);
    let Some(session) = h.sessions.into_iter().find(|s| s.id == session_id) else { return Vec::new() };
    let mut order: Vec<String> = Vec::new();
    for turn in &session.per_turn_evidence {
        for item in &turn.items {
            if !item.file_id.is_empty() && !order.iter().any(|x| x == &item.file_id) {
                order.push(item.file_id.clone());
            }
        }
    }
    order
}

/// File paths the session has already surfaced (first-appearance order). Fed to
/// the rewrite model as entity context so pronouns can bind to case/person names.
pub(super) fn prior_evidence_paths(state: &AppState, session_id: &str) -> Vec<String> {
    if session_id.is_empty() { return Vec::new(); }
    let h = read_history(&state.data_dir);
    let Some(session) = h.sessions.into_iter().find(|s| s.id == session_id) else { return Vec::new() };
    let mut paths: Vec<String> = Vec::new();
    for turn in &session.per_turn_evidence {
        for item in &turn.items {
            if !item.path.is_empty() && !paths.iter().any(|p| p == &item.path) {
                paths.push(item.path.clone());
            }
        }
    }
    paths
}

/// Assign the stable number for a material id, appending new ids.
pub(super) fn assign_material_no(order: &mut Vec<String>, id: &str) -> usize {
    if let Some(pos) = order.iter().position(|x| x == id) {
        return pos + 1;
    }
    order.push(id.to_string());
    order.len()
}

pub fn get_ai_events(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<AiEventJson>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let events = crate::db::ai_events::get_session_events(&conn, &session_id)
        .map_err(|e| format!("{e}"))?;
    Ok(events.iter().map(ai_event_to_json).collect())
}

pub fn get_turn_ai_events(
    state: State<'_, AppState>,
    session_id: String,
    turn_number: usize,
) -> Result<Vec<AiEventJson>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let events = crate::db::ai_events::get_turn_events(&conn, &session_id, turn_number)
        .map_err(|e| format!("{e}"))?;
    Ok(events.iter().map(ai_event_to_json).collect())
}
