//! Chat session persistence: history file, session CRUD, Markdown/JSON export,
//! and AI event serialization.

use serde::Serialize;
use tauri::State;

use super::{ChatMessage, EvidenceItem, truncate_text};
use crate::state::AppState;
pub fn chat_history_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("chat_history.json")
}

/// 会话来源端（`docs/LARK_BOT.md` §5.5 阶段二）。
///
/// 三端各自**只见己方**会话：桌面走 IPC（`ui`）、浏览器走 webapi（`web`）、
/// 飞书机器人走 webapi 并以 `lark` 标记（P1 接）。
///
/// ⚠️ 这是**组织隔离**，**不是安全边界**：单用户 + Bearer token 模型下，
/// 三端本就同权；这里只为避免"三端会话互相混杂"。
pub const ORIGIN_UI: &str = "ui";
pub const ORIGIN_WEB: &str = "web";
pub const ORIGIN_LARK: &str = "lark";

/// 解析来源端。只接受三个已知值，未知值**报错而不静默回退**（静默会把别人的
/// 会话认领到自己名下）。`None` ＝ **内部调用 / 统一管理**（不限来源），
/// 三端一律传 `Some(自己)`。
pub fn parse_origin(raw: Option<&str>) -> Result<Option<String>, String> {
    match raw {
        None => Ok(None),
        Some(v) => match v.trim() {
            ORIGIN_UI => Ok(Some(ORIGIN_UI.to_string())),
            ORIGIN_WEB => Ok(Some(ORIGIN_WEB.to_string())),
            ORIGIN_LARK => Ok(Some(ORIGIN_LARK.to_string())),
            other => Err(format!(
                "未知的会话来源端 '{other}'（只接受 {ORIGIN_UI} / {ORIGIN_WEB} / {ORIGIN_LARK}）"
            )),
        },
    }
}

/// 会话的**生效**来源：本功能之前产生的老会话没有 `origin` 字段 → 归 `ui`
/// （它们确实是桌面端产生的）。
pub fn effective_origin(session: &ChatSession) -> &str {
    if session.origin.is_empty() {
        ORIGIN_UI
    } else {
        session.origin.as_str()
    }
}

/// 该会话是否属于 `origin`（`None` ＝ 不限来源，全部通过）。
fn belongs_to(session: &ChatSession, origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => effective_origin(session) == o,
    }
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
    /// 会话来源端（`ui` / `web` / `lark`，见 [`ORIGIN_UI`]）。
    ///
    /// **老会话没有该字段** → 空串，由 [`effective_origin`] 归为 `ui`。
    ///
    /// 刻意**不用** `#[serde(default = "default_origin")]`：前端不知道自己在哪个端，
    /// 保存时不会带这个字段；若给个非空默认值，Web 端的保存就会把桌面端会话
    /// **认领**成 `web`。空串＝"未指定"，由保存逻辑决定（沿用已有值 / 取调用方来源）。
    #[serde(default)]
    pub origin: String,
}

fn default_strict_docs() -> bool { true }


#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatSessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    /// 生效来源端（老会话为 `ui`）。前端据此做分组/标注，也是"统一管理"视图的键。
    pub origin: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChatHistoryFile {
    pub sessions: Vec<ChatSession>,
}

pub(super) fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `chat_history.json` 的进程内互斥锁：桌面端（IPC）与 Web 端（axum）可能
/// 同时读-改-写同一文件，无锁 RMW 会让后写者覆盖先写者（丢整个会话）。
/// 所有跨调用的 RMW 必须走 [`mutate_history`]，单次读/写走本锁的
/// [`read_history`] / [`write_history`]。
fn history_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    // Poisoned: 持锁方 panic 不会留下半写状态（写入是原子 rename），安全恢复。
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 持锁执行一次完整的「读 → 改 → 写」事务。写失败时文件仍保持 rename 前的
/// 完整旧内容（原子替换），不会出现半截 JSON。
pub fn mutate_history<T>(
    data_dir: &std::path::Path,
    f: impl FnOnce(&mut ChatHistoryFile) -> Result<T, String>,
) -> Result<T, String> {
    let _g = history_lock();
    let mut h = read_history_unlocked(data_dir);
    let out = f(&mut h)?;
    write_history_unlocked(data_dir, &h)?;
    Ok(out)
}

pub fn read_history(data_dir: &std::path::Path) -> ChatHistoryFile {
    let _g = history_lock();
    read_history_unlocked(data_dir)
}

fn read_history_unlocked(data_dir: &std::path::Path) -> ChatHistoryFile {
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
                    // 旧单会话结构没有来源端概念 —— 它确实是桌面端产生的。
                    strict_docs: true,
                    origin: ORIGIN_UI.to_string(),
                };
                let migrated = ChatHistoryFile { sessions: vec![session] };
                // 调用方已持锁 → 走 unlocked 版本（std Mutex 不可重入）。
                let _ = write_history_unlocked(data_dir, &migrated);
                return migrated;
            }
            serde_json::from_str(&c).unwrap_or_default()
        }
        Err(_) => ChatHistoryFile::default(),
    }
}

pub fn write_history(data_dir: &std::path::Path, h: &ChatHistoryFile) -> Result<(), String> {
    let _g = history_lock();
    write_history_unlocked(data_dir, h)
}

/// 原子写：先写临时文件再 rename 覆盖（`std::fs::rename` 在 Windows 上同样
/// 替换目标）。读者要么看到旧文件、要么看到新文件，永远不会读到半截 JSON——
/// 半截 JSON 会被 `serde_json::from_str(...).unwrap_or_default()` 静默解析成
/// 空列表，表现为「会话列表瞬间清空」。
fn write_history_unlocked(data_dir: &std::path::Path, h: &ChatHistoryFile) -> Result<(), String> {
    let path = chat_history_path(data_dir);
    let json = serde_json::to_string_pretty(h).map_err(|e| e.to_string())?;
    let tmp = data_dir.join("chat_history.json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("写入聊天记录失败: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("写入聊天记录失败: {e}")
    })
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

/// List chat sessions of **one** origin, newest first.
///
/// `origin = None` ＝ **不限来源**（内部调用 / 将来的"统一管理"视图）；三端一律传
/// `Some(自己)`。
pub fn list_chat_sessions_filtered(
    data_dir: &std::path::Path,
    origin: Option<&str>,
) -> Vec<ChatSessionMeta> {
    let mut h = read_history(data_dir);
    h.sessions.retain(|s| belongs_to(s, origin));
    h.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    h.sessions
        .into_iter()
        .map(|s| {
            let origin = effective_origin(&s).to_string();
            ChatSessionMeta { id: s.id, title: s.title, updated_at: s.updated_at, origin }
        })
        .collect()
}

/// List chat sessions for one end, newest first. Unknown origin values error out
/// instead of silently falling back (a fallback would mislabel someone's sessions).
pub fn list_chat_sessions(
    state: State<'_, AppState>,
    origin: Option<String>,
) -> Result<Vec<ChatSessionMeta>, String> {
    let origin = parse_origin(origin.as_deref())?;
    Ok(list_chat_sessions_filtered(&state.data_dir, origin.as_deref()))
}

/// Create a new empty session **for one end**. Returns its id.
pub fn create_chat_session(
    state: State<'_, AppState>,
    origin: Option<String>,
) -> Result<String, String> {
    let origin = parse_origin(origin.as_deref())?.unwrap_or_else(|| ORIGIN_UI.to_string());
    create_chat_session_impl(&state.data_dir, &origin)
}

pub fn create_chat_session_impl(data_dir: &std::path::Path, origin: &str) -> Result<String, String> {
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
        origin: origin.to_string(),
    };
    let id = session.id.clone();
    // 持锁 RMW：并发创建（桌面 + Web + 飞书）不会互相覆盖。
    mutate_history(data_dir, |h| {
        // 上限**按来源端各自**计算：三端隔离后，某一端刷满不该挤掉另一端的会话。
        let oldest_same_origin = {
            let mut same: Vec<usize> = h
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| effective_origin(s) == origin)
                .map(|(i, _)| i)
                .collect();
            if same.len() >= 50 {
                same.sort_by_key(|&i| h.sessions[i].updated_at);
                Some(same[0])
            } else {
                None
            }
        };
        if let Some(idx) = oldest_same_origin {
            h.sessions.remove(idx);
        }
        h.sessions.push(session);
        Ok(())
    })?;
    Ok(id)
}

/// Delete a session by id. `origin = Some(...)` restricts deletion to that end
/// (跨来源端删除会被拒绝)；`None` ＝ 内部/统一管理，不受限。
pub fn delete_chat_session(
    state: State<'_, AppState>,
    id: String,
    origin: Option<String>,
) -> Result<(), String> {
    let origin = parse_origin(origin.as_deref())?;
    delete_chat_session_impl(&state.data_dir, id, origin.as_deref())
}

pub fn delete_chat_session_impl(
    data_dir: &std::path::Path,
    id: String,
    origin: Option<&str>,
) -> Result<(), String> {
    mutate_history(data_dir, |h| {
        if let Some(o) = origin
            && let Some(s) = h.sessions.iter().find(|s| s.id == id)
            && effective_origin(s) != o
        {
            return Err(format!(
                "跨来源端删除被拒绝：会话属于 '{}'，请求来自 '{o}'",
                effective_origin(s)
            ));
        }
        h.sessions.retain(|s| s.id != id);
        Ok(())
    })
}

/// 按 id（+可选来源端）取一个完整会话。`origin = None` ＝ 不限来源。
/// 三端与 webapi 路由共用它，避免各自重复实现过滤。
pub fn find_chat_session(
    data_dir: &std::path::Path,
    id: &str,
    origin: Option<&str>,
) -> Option<ChatSession> {
    read_history(data_dir)
        .sessions
        .into_iter()
        .find(|s| s.id == id && belongs_to(s, origin))
}

/// 显式改标题的**长度上限**（按字符；UI 侧栏与飞书卡片列表都只有一行空间）。
pub const MAX_SESSION_TITLE_CHARS: usize = 30;

/// 校验并按上限截断一个用户输入的会话标题。
///
/// 空/纯空白是**错误**（不是静默忽略）：调用方（`/rename`）必须能把这个反馈给用户。
pub fn sanitize_session_title(title: &str) -> Result<String, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("标题不能为空".to_string());
    }
    Ok(trimmed.chars().take(MAX_SESSION_TITLE_CHARS).collect())
}

/// **只改标题**的窄写入口（`/rename` 用；前端没有改名 UI，整会话保存走 `save_chat_session_impl`）。
///
/// 与其他写路径一样受**来源端**约束（跨端拒绝）。刻意不写 `updated_at`：
/// 改名是元数据整理，不该把它顶到"最近使用"的第一位（列表排序按它）。
pub fn rename_chat_session_impl(
    data_dir: &std::path::Path,
    id: &str,
    title: &str,
    origin: Option<&str>,
) -> Result<String, String> {
    let title = sanitize_session_title(title)?;
    let saved = title.clone();
    mutate_history(data_dir, |h| {
        let Some(s) = h.sessions.iter_mut().find(|s| s.id == id) else {
            return Err("会话不存在".to_string());
        };
        if let Some(o) = origin
            && effective_origin(s) != o
        {
            return Err(format!(
                "跨来源端改名被拒绝：会话属于 '{}'，请求来自 '{o}'",
                effective_origin(s)
            ));
        }
        s.title = title;
        Ok(())
    })?;
    Ok(saved)
}

/// Load a full session by id. Returns None if not found **or not owned by `origin`**.
pub fn load_chat_session(
    state: State<'_, AppState>,
    id: String,
    origin: Option<String>,
) -> Result<Option<ChatSession>, String> {
    let origin = parse_origin(origin.as_deref())?;
    Ok(find_chat_session(&state.data_dir, &id, origin.as_deref()))
}

/// Save (create or update) a session. If the session is new or has no title,
/// derives a title from the first user message.
pub fn save_chat_session(
    state: State<'_, AppState>,
    session: ChatSession,
    origin: Option<String>,
) -> Result<(), String> {
    let origin = parse_origin(origin.as_deref())?;
    save_chat_session_impl(&state.data_dir, session, origin.as_deref())
}

pub fn save_chat_session_impl(
    data_dir: &std::path::Path,
    session: ChatSession,
    caller_origin: Option<&str>,
) -> Result<(), String> {
    let now = now_ts();
    let mut session = session;
    session.updated_at = now;
    if session.title.is_empty() || session.title == "新会话" {
        session.title = title_from_first_message(&session);
    }
    // 持锁 RMW：多个客户端同时保存不同会话时不再互相覆盖。
    mutate_history(data_dir, |h| {
        let found = h.sessions.iter().position(|s| s.id == session.id);
        match found {
            Some(idx) => {
                let existing_origin = effective_origin(&h.sessions[idx]).to_string();
                // 写入限同来源端：别端的会话不允许被改写。
                if let Some(o) = caller_origin
                    && existing_origin != o
                {
                    return Err(format!(
                        "跨来源端写入被拒绝：会话属于 '{existing_origin}'，请求来自 '{o}'"
                    ));
                }
                let existing_created_at = h.sessions[idx].created_at;
                if session.created_at == 0 {
                    session.created_at = existing_created_at;
                }
                // 载荷没带 origin（前端不知道自己在哪个端）→ 沿用已有值，
                // **绝不**因为缺字段就把来源改掉。
                if session.origin.is_empty() {
                    session.origin = existing_origin;
                }
                h.sessions[idx] = session;
            }
            None => {
                if session.created_at == 0 {
                    session.created_at = now;
                }
                if session.origin.is_empty() {
                    session.origin = caller_origin.unwrap_or(ORIGIN_UI).to_string();
                }
                h.sessions.push(session);
            }
        }
        Ok(())
    })
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
    origin: Option<String>,
) -> Result<String, String> {
    let origin = parse_origin(origin.as_deref())?;
    export_chat_session_impl(&state.data_dir, &id, turns.as_deref(), origin.as_deref())
}

pub fn export_chat_session_impl(
    data_dir: &std::path::Path,
    id: &str,
    selected_turns: Option<&[usize]>,
    origin: Option<&str>,
) -> Result<String, String> {
    let h = read_history(data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id && belongs_to(s, origin))
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
    origin: Option<String>,
) -> Result<String, String> {
    let origin = parse_origin(origin.as_deref())?;
    export_chat_session_json_impl(
        &state.data_dir,
        &id,
        Some(&state.db),
        turns.as_deref(),
        origin.as_deref(),
    )
}

pub fn export_chat_session_json_impl(
    data_dir: &std::path::Path,
    id: &str,
    db: Option<&r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>,
    selected_turns: Option<&[usize]>,
    origin: Option<&str>,
) -> Result<String, String> {
    let h = read_history(data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id && belongs_to(s, origin))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ls_history_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mk_session(id: &str, title: &str) -> ChatSession {
        let now = now_ts();
        ChatSession {
            id: id.to_string(),
            title: title.to_string(),
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
            origin: ORIGIN_UI.to_string(),
        }
    }

    /// 两个线程同时保存不同会话 → 都必须存活（锁 + 原子写之前的实现会
    /// 「后写覆盖先写」丢掉一个）。同时覆盖 50 次重复 RMW 循环。
    #[test]
    fn concurrent_saves_do_not_lose_sessions() {
        let dir = temp_dir();
        let d1 = dir.clone();
        let d2 = dir.clone();
        let t1 = std::thread::spawn(move || {
            for i in 0..50 {
                save_chat_session_impl(&d1, mk_session("s-a", &format!("a{i}")), Some("ui")).unwrap();
            }
        });
        let t2 = std::thread::spawn(move || {
            for i in 0..50 {
                save_chat_session_impl(&d2, mk_session("s-b", &format!("b{i}")), Some("ui")).unwrap();
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();

        let h = read_history(&dir);
        let ids: Vec<&str> = h.sessions.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"s-a"), "session A must survive, got {ids:?}");
        assert!(ids.contains(&"s-b"), "session B must survive, got {ids:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写入必须是原子替换：文件要么是完整的旧 JSON、要么是完整的新 JSON。
    #[test]
    fn write_history_leaves_no_temp_file() {
        let dir = temp_dir();
        save_chat_session_impl(&dir, mk_session("s-1", "t"), Some("ui")).unwrap();
        assert!(chat_history_path(&dir).is_file());
        assert!(
            !dir.join("chat_history.json.tmp").exists(),
            "tmp file must be renamed away"
        );
        // 读回仍是合法 JSON（未被半截写破坏）
        let h = read_history(&dir);
        assert_eq!(h.sessions.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// create / delete 走 mutate_history，同样不得丢已存在的会话。
    #[test]
    fn create_and_delete_preserve_other_sessions() {
        let dir = temp_dir();
        let a = create_chat_session_impl(&dir, "ui").unwrap();
        save_chat_session_impl(&dir, mk_session("s-x", "x"), Some("ui")).unwrap();
        delete_chat_session_impl(&dir, a, Some("ui")).unwrap();
        let h = read_history(&dir);
        assert_eq!(h.sessions.len(), 1);
        assert_eq!(h.sessions[0].id, "s-x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── §5.5 阶段二：来源端隔离 ──────────────────────────────────────────────

    /// 三端各自**只见己方**会话；`None` 表示不限来源（统一管理视图用）。
    #[test]
    fn sessions_are_isolated_per_origin() {
        let dir = temp_dir();
        let ui_id = create_chat_session_impl(&dir, ORIGIN_UI).unwrap();
        let web_id = create_chat_session_impl(&dir, ORIGIN_WEB).unwrap();
        let lark_id = create_chat_session_impl(&dir, ORIGIN_LARK).unwrap();

        let ui = list_chat_sessions_filtered(&dir, Some(ORIGIN_UI));
        assert_eq!(ui.len(), 1, "桌面端只应看到自己那 1 条");
        assert_eq!(ui[0].id, ui_id);
        assert_eq!(ui[0].origin, ORIGIN_UI);

        let web = list_chat_sessions_filtered(&dir, Some(ORIGIN_WEB));
        assert_eq!(web.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![web_id.as_str()]);
        let lark = list_chat_sessions_filtered(&dir, Some(ORIGIN_LARK));
        assert_eq!(lark.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec![lark_id.as_str()]);

        assert_eq!(list_chat_sessions_filtered(&dir, None).len(), 3, "None = 不限来源");

        // load：跨端按"不存在"处理，不泄漏别的端有没有这个 id
        assert!(find_chat_session(&dir, &ui_id, Some(ORIGIN_WEB)).is_none());
        assert!(find_chat_session(&dir, &ui_id, Some(ORIGIN_UI)).is_some());
        assert!(find_chat_session(&dir, &ui_id, None).is_some());

        // delete：跨端必须被拒，且被拒后会话仍在
        let err = delete_chat_session_impl(&dir, ui_id.clone(), Some(ORIGIN_WEB)).unwrap_err();
        assert!(err.contains("跨来源端删除被拒绝"), "unexpected: {err}");
        assert!(find_chat_session(&dir, &ui_id, Some(ORIGIN_UI)).is_some(), "被拒后会话必须仍在");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 老会话（没有 `origin` 字段）归 `ui`；**Web 端看不到它** —— 这是本阶段的行为变化，
    /// 用测试钉住，避免将来有人"顺手"把老会话改成对三端都可见。
    #[test]
    fn legacy_sessions_without_origin_are_ui_only() {
        let dir = temp_dir();
        std::fs::write(
            chat_history_path(&dir),
            r#"{"sessions":[{"id":"legacy-1","title":"旧会话","created_at":1,"updated_at":2,
                "messages":[],"source_ids":[],"source_files":[]}]}"#,
        )
        .unwrap();

        let ui = list_chat_sessions_filtered(&dir, Some(ORIGIN_UI));
        assert_eq!(ui.len(), 1, "老会话必须归 ui");
        assert_eq!(ui[0].origin, ORIGIN_UI);
        assert!(
            list_chat_sessions_filtered(&dir, Some(ORIGIN_WEB)).is_empty(),
            "老会话不应出现在 Web 端"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 保存时**不带** origin（前端的真实情况：它不知道自己在哪个端）不得改写来源端。
    #[test]
    fn save_without_origin_keeps_existing_origin() {
        let dir = temp_dir();
        let id = create_chat_session_impl(&dir, ORIGIN_WEB).unwrap();
        let mut s = find_chat_session(&dir, &id, None).unwrap();
        assert_eq!(s.origin, ORIGIN_WEB);

        s.title = "改个标题".into();
        s.origin = String::new(); // 模拟前端：完全不带该字段
        save_chat_session_impl(&dir, s, Some(ORIGIN_WEB)).unwrap();

        let after = find_chat_session(&dir, &id, None).unwrap();
        assert_eq!(after.origin, ORIGIN_WEB, "缺字段绝不能把来源端改掉");
        assert_eq!(after.title, "改个标题", "其余字段照常更新");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 跨来源端写入必须被拒（写入限同 origin），且被拒后原会话不被改动。
    #[test]
    fn cross_origin_write_is_rejected() {
        let dir = temp_dir();
        let id = create_chat_session_impl(&dir, ORIGIN_UI).unwrap();
        let mut s = find_chat_session(&dir, &id, None).unwrap();
        s.title = "被别端偷改".into();

        let err = save_chat_session_impl(&dir, s, Some(ORIGIN_WEB)).unwrap_err();
        assert!(err.contains("跨来源端写入被拒绝"), "unexpected: {err}");
        assert_eq!(
            find_chat_session(&dir, &id, None).unwrap().title,
            "新会话",
            "被拒的写入不得留下任何改动"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 50 上限**按来源端各自**计算：一端刷满不该挤掉另一端的会话。
    #[test]
    fn session_cap_is_per_origin() {
        let dir = temp_dir();
        for _ in 0..51 {
            create_chat_session_impl(&dir, ORIGIN_WEB).unwrap();
        }
        create_chat_session_impl(&dir, ORIGIN_UI).unwrap();

        assert_eq!(list_chat_sessions_filtered(&dir, Some(ORIGIN_WEB)).len(), 50, "web 端封顶 50");
        assert_eq!(
            list_chat_sessions_filtered(&dir, Some(ORIGIN_UI)).len(),
            1,
            "另一端不该被 web 端挤掉"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未知来源端**报错**而不是静默回退 —— 静默回退会把别端会话认领过来。
    #[test]
    fn unknown_origin_is_an_error() {
        assert!(parse_origin(Some("desktop")).is_err());
        assert!(parse_origin(Some("")).is_err());
        assert_eq!(parse_origin(None).unwrap(), None, "None = 不限来源");
        assert_eq!(parse_origin(Some("lark")).unwrap().as_deref(), Some(ORIGIN_LARK));
        assert_eq!(parse_origin(Some(" web ")).unwrap().as_deref(), Some(ORIGIN_WEB), "两侧空白容忍");
    }

    /// 标题清洗：去空白、按 30 字截断；空/纯空白**报错**（不能把会话改成没名字）。
    #[test]
    fn sanitize_session_title_trims_limits_and_rejects_blank() {
        assert_eq!(sanitize_session_title("  合同审查  ").unwrap(), "合同审查");
        assert_eq!(
            sanitize_session_title(&"字".repeat(50)).unwrap(),
            "字".repeat(MAX_SESSION_TITLE_CHARS),
            "超长按字符截到上限（不切碎 UTF-8）"
        );
        assert!(sanitize_session_title("").is_err());
        assert!(sanitize_session_title("   ").is_err());
    }

    /// 改名：只动 `title`（`updated_at` 不刷新——改名不该把它顶到"最近使用"首位）；
    /// 空标题与跨端改名都被拒。
    #[test]
    fn rename_only_touches_title_and_honours_origin() {
        let dir = temp_dir();
        let id = create_chat_session_impl(&dir, ORIGIN_LARK).unwrap();
        // 让 updated_at 与改名时刻拉开差距（now_ts 是秒级）
        let before = find_chat_session(&dir, &id, None).unwrap();

        let saved = rename_chat_session_impl(&dir, &id, "  合同审查  ", Some(ORIGIN_LARK)).unwrap();
        assert_eq!(saved, "合同审查", "返回值是清洗后的标题");

        let after = find_chat_session(&dir, &id, None).unwrap();
        assert_eq!(after.title, "合同审查");
        assert_eq!(after.updated_at, before.updated_at, "改名不得刷新 updated_at");
        assert_eq!(after.origin, ORIGIN_LARK, "来源端不受影响");
        assert_eq!(after.messages.len(), 0, "不改动消息");

        // 空标题被拒（且不得发生任何改动）
        assert!(rename_chat_session_impl(&dir, &id, "   ", Some(ORIGIN_LARK)).is_err());
        assert_eq!(find_chat_session(&dir, &id, None).unwrap().title, "合同审查");

        // 跨端改名被拒
        assert!(rename_chat_session_impl(&dir, &id, "偷改", Some(ORIGIN_WEB)).is_err());
        assert_eq!(find_chat_session(&dir, &id, None).unwrap().title, "合同审查");

        // 不存在的会话报错
        assert!(rename_chat_session_impl(&dir, "nope", "x", Some(ORIGIN_LARK)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
