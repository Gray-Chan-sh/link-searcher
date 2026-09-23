//! AI gateway commands: per-file summaries, cross-document Q&A (RAG),
//! smart search, and multi-turn conversation.

use serde::Serialize;
use tauri::{Emitter, State};

use crate::state::AppState;

mod cite;
pub use cite::{auto_cite, sanitize_citations, sanitize_citations_set};

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
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页填写 API Base URL")
            .into());
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
    if let Err(e) = crate::db::tracker::upsert_summary(&conn, &file_id, &summary) {
        log::warn!("[AI] upsert_summary failed for {file_id}: {e}");
    }
    Ok(SummaryResult { file_id, summary, cached: false })
}

/// A group of documents sharing one content theme.
#[derive(Debug, Clone, Serialize)]
pub struct TopicCluster {
    pub topic: String,
    pub files: Vec<String>,
}

/// Cluster indexed documents into topics via LLM over summaries/snippets.
#[tauri::command]
pub async fn ai_topic_clusters(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<TopicCluster>, String> {
    if !crate::ai::llm_enabled() {
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页填写 API Base URL")
            .into());
    }
    let limit = limit.unwrap_or(150).min(400);
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let brief = "COALESCE(ds.summary, substr(ci.text_content, 1, 200))";
    let sql = format!(
        "SELECT f.path, {brief} FROM file_tracking f \
         LEFT JOIN doc_summaries ds ON ds.file_id = f.id \
         LEFT JOIN content_index ci ON ci.md5 = f.md5 \
         WHERE f.status = 'active' AND {brief} IS NOT NULL AND trim({brief}) != '' \
         ORDER BY f.updated_at DESC LIMIT ?1"
    );
    let items: Vec<(String, String)> = {
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(
            [limit as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ).map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())?
    };
    drop(conn);

    if items.is_empty() {
        return Err("没有可分析的文档内容".into());
    }

    let mut listing = String::new();
    for (i, (path, text)) in items.iter().enumerate() {
        listing.push_str(&format!("[{}] {}: {}\n", i + 1, path, text.replace('\n', " ")));
    }
    listing.push_str("\n请把以上文档按内容主题分成 3-8 组，只输出 JSON 数组，不要输出任何其他文字：\n[{\"topic\":\"组名\",\"ids\":[\"编号\"]}]");
    let system = "你是文档主题聚类助手。根据每个文档的路径与内容摘要将其分组。";
    let raw = tokio::task::spawn_blocking(move || crate::ai::chat(system, &listing))
        .await
        .unwrap_or(None)
        .ok_or_else(|| "AI 请求失败（检查 API 配置或网络）".to_string())?;

    let Some(start) = raw.find('[') else {
        return Err("AI 返回格式无法解析".into());
    };
    let Some(end) = raw.rfind(']') else {
        return Err("AI 返回格式无法解析".into());
    };
    if end <= start {
        return Err("AI 返回格式无法解析".into());
    }
    #[derive(serde::Deserialize)]
    struct RawCluster {
        topic: String,
        ids: Vec<String>,
    }
    let clusters: Vec<RawCluster> = serde_json::from_str(&raw[start..=end])
        .map_err(|_| "AI 返回的 JSON 无法解析".to_string())?;

    let mut out = Vec::new();
    for c in clusters {
        let files = c
            .ids
            .iter()
            .filter_map(|id| id.trim().parse::<usize>().ok())
            .filter(|&n| n >= 1 && n <= items.len())
            .map(|n| items[n - 1].0.clone())
            .collect::<Vec<_>>();
        if !files.is_empty() && !c.topic.trim().is_empty() {
            out.push(TopicCluster { topic: c.topic.trim().to_string(), files });
        }
    }
    if out.is_empty() {
        return Err("未能从 AI 返回中解析出有效分组".into());
    }
    Ok(out)
}

/// Ask a question over one or more documents' extracted text (RAG).
#[tauri::command]
pub async fn ask_documents(
    state: State<'_, AppState>,
    file_ids: Vec<String>,
    question: String,
) -> Result<String, String> {
    if !crate::ai::llm_enabled() {
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页配置 API Base URL")
            .into());
    }
    if question.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let mut docs: Vec<String> = Vec::new();
    for fid in &file_ids {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid)
            && let Some(md5) = &rec.md5
                && let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5)
                    && !text.trim().is_empty() {
                        docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
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

/// Structured citation backing an AI answer: which file, its path, and a
/// short snippet of the supporting passage (first ~200 chars).
///
/// Traceability: score fields explain *why* this document was picked —
/// BM25 score, embedding cosine similarity, and the RRF fused score (the
/// latter two present only when semantic fusion ran). `rewritten` /
/// `rewritten_query` record whether this turn's query was rewritten before
/// retrieval; `from_history` marks documents carried over from earlier
/// turns instead of being retrieved by the current query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvidenceItem {
    pub file_id: String,
    pub path: String,
    pub snippet: String,
    #[serde(default)]
    pub bm25_score: Option<f64>,
    #[serde(default)]
    pub semantic_score: Option<f64>,
    #[serde(default)]
    pub rrf_score: Option<f64>,
    #[serde(default)]
    pub rewritten: bool,
    #[serde(default)]
    pub rewritten_query: Option<String>,
    #[serde(default)]
    pub from_history: bool,
    /// Session-stable material number (`[N]`). 0 = unset (legacy / smart path);
    /// callers fall back to positional index in that case.
    #[serde(default)]
    pub material_no: usize,
    /// Source-document char ranges that actually reached the prompt.
    #[serde(default)]
    pub injected_spans: Vec<(usize, usize)>,
}

/// Retrieval hit carrying its scores; semantic fields are `None` unless
/// the RRF fusion path ran.
#[derive(Debug, Clone)]
pub struct ScoredHit {
    pub file_id: String,
    pub path: String,
    pub bm25_score: Option<f64>,
    pub semantic_score: Option<f64>,
    pub rrf_score: Option<f64>,
    pub from_history: bool,
    /// Set when this hit came from the chunk-vector channel — the chunk
    /// indices (with similarities) that matched. Used by the injection layer
    /// to prioritize those exact chunks over lexical overlap.
    pub from_chunk: bool,
    pub hit_chunks: Vec<(usize, f32)>,
}

#[derive(Serialize)]
pub struct SmartSearchResponse {
    pub answer: String,
    pub source_ids: Vec<String>,
    pub source_files: Vec<String>,
    pub evidence: Vec<EvidenceItem>,
}

/// Streaming AI payloads emitted over Tauri events (frontend listens and
/// renders incrementally).
#[derive(Clone, Serialize)]
struct AiChunk {
    session_id: String,
    delta: String,
    #[serde(default)]
    reasoning: bool,
}

#[derive(Clone, Serialize)]
struct AiDone {
    session_id: String,
    full_text: String,
    took_ms: u64,
    cancelled: bool,
    source_ids: Vec<String>,
    source_files: Vec<String>,
    evidence: Vec<EvidenceItem>,
    #[serde(default)]
    trace_id: String,
    #[serde(default)]
    search_query: String,
    #[serde(default)]
    search_terms: Vec<String>,
    #[serde(default)]
    clarify_candidates: Vec<String>,
    #[serde(default)]
    clarify_slots: Vec<ClarifySlot>,
    #[serde(default)]
    clarify_blocking: bool,
    #[serde(default)]
    hits: usize,
    #[serde(default)]
    total_match_count: usize,
    #[serde(default)]
    llm_model: String,
    #[serde(default)]
    embedding_model: String,
}

#[derive(Clone, Serialize)]
struct AiProgress {
    session_id: String,
    phase: String,
    message: String,
    #[serde(default)]
    current: usize,
    #[serde(default)]
    total: usize,
}

mod prompt;
pub use prompt::truncate_text;
pub(crate) use prompt::{prepare_conversation_prompt, PreparedConversation, DOC_TYPE_NOUNS};
use prompt::{compose_answer_text, prepare_smart_prompt, PreparedSmart};
#[cfg(test)]
use prompt::{
    chunked_or_truncated_with_budget, cited_excerpt, clarify_is_blocking, query_terms,
    resolve_mention_file_ids,
};

/// Search + RAG: use BM25 to find the most relevant documents, extract
/// their text, and let the LLM answer the query based on those materials.
/// Returns a textual answer plus the list of source files used.
#[tauri::command]
pub async fn smart_search(
    state: State<'_, AppState>,
    query: String,
) -> Result<SmartSearchResponse, String> {
    if !crate::ai::llm_enabled() {
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页配置 API Base URL")
            .into());
    }
    if query.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    log::info!("[AI] smart_search: query={}", query);
    crate::ai::reset_ai_cancel();

    let PreparedSmart { system, user_msg, source_ids, source_files, evidence } =
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

    Ok(SmartSearchResponse { answer, source_ids, source_files, evidence })
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
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页配置 API Base URL")
            .into());
    }
    if query.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    log::info!("[AI] smart_search_stream: query={}", query);
    crate::ai::reset_ai_cancel();

    let PreparedSmart { system, user_msg, source_ids, source_files, evidence } =
        prepare_smart_prompt(&state, &query)?;
    let hits = evidence.len();
    let session_clone = session_id.clone();
    let app_inner = app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut emit = |d: &str, _is_reasoning: bool| {
            let _ = app_inner.emit("ai-chunk", AiChunk { session_id: session_clone.clone(), delta: d.to_string(), reasoning: false });
        };
        crate::ai::chat_stream(&system, &user_msg, &mut emit)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?;

    let raw_text = result.text.unwrap_or_default();
    log::info!("[AI]   raw answer bytes={:?} chars={} trimmed_empty={}", raw_text.as_bytes(), raw_text.chars().count(), raw_text.trim().is_empty());
    let cited_text = auto_cite(&sanitize_citations(&raw_text, evidence.len()), &evidence);
    let _ = app.emit("ai-done", AiDone {
        session_id,
        full_text: cited_text,
        took_ms: result.took_ms,
        cancelled: result.cancelled,
        source_ids,
        source_files,
        evidence,
        trace_id: String::new(),
        search_query: query,
        search_terms: vec![],
        clarify_candidates: vec![],
        clarify_slots: vec![],
        clarify_blocking: false,
        hits,
        total_match_count: 0,
        llm_model: String::new(),
        embedding_model: String::new(),
    });
    Ok(())
}

mod rewrite;
pub use rewrite::{RewriteOutcome, llm_rewrite_query, rewrite_query};
use rewrite::{carry_forward_source, ensure_parent_entities, extract_retrieval_keywords};
#[cfg(test)]
use rewrite::{rewrite_history, valid_rewrite_output};

mod retrieval;
pub use retrieval::{merge_scope_prefixes, weighted_mix};
pub(crate) use retrieval::{bm25_relevant_hits, rrf_add};
use retrieval::{apply_rerank_order, hit_in_scope, trace_rank, trace_target_ids};
#[cfg(test)]
use retrieval::rrf_fuse;

/// 前端回填的槽位绑定：用户在澄清追问里给出的答案。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClarifyBinding {
    pub surface: String,
    pub value: String,
}

/// 一次澄清回复：被回答的原问题 + 该问题各槽位的答案。
/// `base_question` 必须回传——前端只把答案本身作为可见消息，否则检索会退化成
/// 对答案（如"汪均益"）的单词查询。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClarifyReply {
    pub base_question: String,
    pub bindings: Vec<ClarifyBinding>,
}

/// 结构化的追问槽位（回传前端渲染填空控件）。比 `clarify_candidates` 多带
/// `stype`，前端靠它把答案回填到正确的槽位。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClarifySlot {
    pub surface: String,
    pub stype: String,
    pub options: Vec<String>,
}


/// Multi-turn conversation: continue a chat using previously-selected
/// source documents as the knowledge base. `messages` includes the full
/// conversation history (alternating user/assistant roles).
#[tauri::command]
pub async fn conversation_ask(
    state: State<'_, AppState>,
    messages: Vec<ChatMessage>,
    source_ids: Vec<String>,
    scope: TurnScope,
    session_retrieval_scope: Vec<String>,
    strict_docs: bool,
    full_recall: Option<bool>,
    clarify_reply: Option<ClarifyReply>,
) -> Result<String, String> {
    if !crate::ai::llm_enabled() {
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页配置 API Base URL")
            .into());
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

    let PreparedConversation { system, user_msg, evidence, has_evidence, visible_nums, clarify_note, clarify_blocking, .. } =
        prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_retrieval_scope, strict_docs, full_recall.unwrap_or(false), false, None, "", clarify_reply.as_ref()).await?;
    if clarify_blocking {
        log::info!("[AI]   clarify blocking: 指代未绑定且提问无锚点 → 只问不答（跳过 LLM）");
        return Ok(clarify_note.unwrap_or_default());
    }
    // 非严格模式：无材料注入时拒绝硬答（避免 LLM 无据发挥）。
    if !has_evidence {
        log::warn!("[AI]   no evidence injected, refusing to answer (non-stream)");
        return Ok("未在与当前范围匹配的文档中找到依据，因此无法回答。\n建议：换用更具体的关键词提问、通过 @ 引用相关文件，或使用全文搜索直接检索。".to_string());
    }
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
    let visible_evidence: Vec<EvidenceItem> = evidence.iter().filter(|e| visible_nums.contains(&e.material_no)).cloned().collect();
    let cited = auto_cite(&sanitize_citations_set(&answer, &visible_nums), &visible_evidence);
    log::info!("[AI]   done: {} chars", answer.chars().count());
    let cited = match clarify_note {
        Some(n) => format!("{n}\n\n{cited}"),
        None => cited,
    };

    Ok(cited)
}

/// Streaming variant of [`conversation_ask`]: emits `ai-chunk`/`ai-done`.
#[tauri::command]
pub async fn conversation_ask_stream(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    messages: Vec<ChatMessage>,
    source_ids: Vec<String>,
    session_id: String,
    scope: TurnScope,
    session_retrieval_scope: Vec<String>,
    strict_docs: bool,
    full_recall: Option<bool>,
    clarify_reply: Option<ClarifyReply>,
) -> Result<(), String> {
    if !crate::ai::llm_enabled() {
        return Err(crate::ai::llm_unavailable_reason()
            .unwrap_or("AI 服务未配置，请在设置页配置 API Base URL")
            .into());
    }
    if messages.is_empty() {
        return Err("对话不能为空".into());
    }
    log::info!(
        "[AI] ▶ stream: msgs={} sources={}",
        messages.len(),
        source_ids.len()
    );
    crate::ai::reset_ai_cancel();

    log::info!("[AI] conversation_ask_stream: scope={:?}", scope);

    let prepared = prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_retrieval_scope, strict_docs, full_recall.unwrap_or(false), false, Some(&app), &session_id, clarify_reply.as_ref()).await;
    match &prepared {
        Ok(p) => log::info!("[AI]   prepare ok: system_chars={} user_chars={} sources={} evidence={}", p.system.chars().count(), p.user_msg.chars().count(), p.source_ids.len(), p.evidence.len()),
        Err(e) => log::warn!("[AI]   prepare failed: {}", e),
    }
    let PreparedConversation { system, user_msg, source_ids, source_files, evidence, search_query, search_terms, clarify_note, clarify_candidates, clarify_slots, clarify_blocking, hits, total_match_count, has_evidence, visible_nums, mut events } =
        prepared?;
    let visible_evidence: Vec<EvidenceItem> = evidence.iter().filter(|e| visible_nums.contains(&e.material_no)).cloned().collect();
    let trace_id = format!("{session_id}#t{}", messages.iter().filter(|m| m.role == "user").count());
    let cfg = crate::config::load_config();
    let turn_number = messages.iter().filter(|m| m.role == "user").count().saturating_sub(1);
    log::info!(
        "[AI_TRACE] turn_begin trace_id={trace_id} llm={} embedding={} strict={} hits={} search_q={} has_evidence={}",
        cfg.active_llm_model_id, cfg.active_embedding_model_id, strict_docs, hits, search_query, has_evidence
    );
    // 非严格模式：检索 0 命中 / 无材料注入时，拒绝硬答（避免 LLM 无据发挥），
    // 直接返回"未找到依据"提示 + 建议。strict 模式的空 context 已在 prepare 内 Err。
    if !has_evidence {
        log::warn!("[AI]   no evidence injected (total_match_count={}), refusing to answer", total_match_count);
        events.push(("turn_complete".into(), serde_json::json!({
            "refused_no_evidence": true,
            "total_match_count": total_match_count,
            "answer_chars": 0,
        })));
        if let Ok(conn) = state.db.get() {
            for (i, (event_type, payload)) in events.iter().enumerate() {
                let _ = crate::db::ai_events::record_event(
                    &conn, &session_id, turn_number, (i + 1) as u32, event_type, payload,
                );
            }
        }
        let refuse = "未在与当前范围匹配的文档中找到依据，因此无法回答。\n建议：换用更具体的关键词提问、通过 @ 引用相关文件，或点击右上角「全文搜索」直接检索。";
        let _ = app.emit("ai-done", AiDone {
            session_id,
            full_text: refuse.to_string(),
            took_ms: 0,
            cancelled: false,
            source_ids,
            source_files,
            evidence,
            trace_id,
            search_query,
            search_terms: search_terms.clone(),
            clarify_candidates: clarify_candidates.clone(),
            clarify_slots: clarify_slots.clone(),
            clarify_blocking,
            hits,
            total_match_count,
            llm_model: cfg.active_llm_model_id,
            embedding_model: cfg.active_embedding_model_id,
        });
        return Ok(());
    }
    events.push(("llm_call".into(), serde_json::json!({
        "model_id": cfg.active_llm_model_id,
        "system_prompt_chars": system.chars().count(),
        "user_msg_chars": user_msg.chars().count(),
        "streaming": true,
    })));
    let _ = app.emit("ai-progress", AiProgress {
        session_id: session_id.clone(),
        phase: "llm_call".to_string(),
        message: "等待 AI 回答中...".to_string(),
        current: 0,
        total: 0,
    });
    if let Some(n) = &clarify_note {
        let _ = app.emit("ai-chunk", AiChunk { session_id: session_id.clone(), delta: format!("{n}\n\n"), reasoning: false });
    }
    let session_clone = session_id.clone();
    let app_inner = app.clone();
    log::info!("[AI]   invoking chat_stream: system_chars={} user_chars={} model={}", system.chars().count(), user_msg.chars().count(), cfg.active_llm_model_id);
    let result = if clarify_blocking {
        log::info!("[AI]   clarify blocking: 指代未绑定且提问无锚点 → 只问不答（跳过 LLM）");
        crate::ai::ChatStreamOutcome { text: clarify_note.clone(), took_ms: 0, cancelled: false }
    } else {
        tokio::task::spawn_blocking(move || {
            let mut emit = |d: &str, is_reasoning: bool| {
                let _ = app_inner.emit("ai-chunk", AiChunk { session_id: session_clone.clone(), delta: d.to_string(), reasoning: is_reasoning });
            };
            crate::ai::chat_stream(&system, &user_msg, &mut emit)
        })
        .await
        .map_err(|e| format!("task panicked: {e}"))?
    };
    log::info!("[AI]   chat_stream returned: chars={} cancelled={} took_ms={}", result.text.as_ref().map(|t| t.chars().count()).unwrap_or(0), result.cancelled, result.took_ms);

    log::info!(
        "[AI_TRACE] turn_end trace_id={trace_id} took_ms={} cancelled={} answer_chars={} sources={}",
        result.took_ms, result.cancelled,
        result.text.as_ref().map(|t| t.chars().count()).unwrap_or(0),
        source_ids.len()
    );
    events.push(("turn_complete".into(), serde_json::json!({
        "took_ms": result.took_ms,
        "cancelled": result.cancelled,
        "answer_chars": result.text.as_ref().map(|t| t.chars().count()).unwrap_or(0),
        "source_count": source_ids.len(),
        "evidence_count": evidence.len(),
    })));
    if let Ok(conn) = state.db.get() {
        for (i, (event_type, payload)) in events.iter().enumerate() {
            let _ = crate::db::ai_events::record_event(
                &conn, &session_id, turn_number, (i + 1) as u32, event_type, payload,
            );
        }
     }
     let raw_text = result.text.unwrap_or_default();
     log::info!("[AI]   raw answer bytes={:?} chars={} trimmed_empty={}", raw_text.as_bytes(), raw_text.chars().count(), raw_text.trim().is_empty());
     let cited_text = compose_answer_text(clarify_blocking, raw_text, clarify_note, |raw| {
         auto_cite(&sanitize_citations_set(raw, &visible_nums), &visible_evidence)
     });
     let _ = app.emit("ai-done", AiDone {
         session_id,
        full_text: cited_text,
        took_ms: result.took_ms,
        cancelled: result.cancelled,
        source_ids,
        source_files,
        evidence,
        trace_id,
        search_query,
        search_terms,
        clarify_candidates,
        clarify_slots,
        clarify_blocking,
        hits,
        total_match_count,
        llm_model: cfg.active_llm_model_id,
        embedding_model: cfg.active_embedding_model_id,
    });
    Ok(())
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}


mod session;
pub use session::{
    AiEventJson, ChatHistoryFile, ChatSession, ChatSessionMeta, PerTurnEvidence, PerTurnScope,
    ScopeCondition, TurnScope, chat_history_path, create_chat_session_impl,
    export_chat_session_impl, export_chat_session_json_impl, read_history, write_history,
};
use session::{
    assign_material_no, prior_evidence_paths, prior_material_order, visible_material_numbers,
};
#[cfg(test)]
use session::{now_ts, save_chat_session_impl};

// ── Session commands (thin wrappers; implementation lives in `session`) ──

/// List all chat sessions, newest first.
#[tauri::command]
pub fn list_chat_sessions(state: State<'_, AppState>) -> Result<Vec<ChatSessionMeta>, String> {
    session::list_chat_sessions(state)
}

/// Create a new empty session. Returns its id.
#[tauri::command]
pub fn create_chat_session(state: State<'_, AppState>) -> Result<String, String> {
    session::create_chat_session(state)
}

/// Delete a session by id.
#[tauri::command]
pub fn delete_chat_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    session::delete_chat_session(state, id)
}

/// Load a full session by id. Returns None if not found.
#[tauri::command]
pub fn load_chat_session(state: State<'_, AppState>, id: String) -> Result<Option<ChatSession>, String> {
    session::load_chat_session(state, id)
}

/// Save (create or update) a session. If the session is new or has no title,
/// derives a title from the first user message.
#[tauri::command]
pub fn save_chat_session(
    state: State<'_, AppState>,
    sess: ChatSession,
) -> Result<(), String> {
    session::save_chat_session(state, sess)
}

/// Export a session as Markdown (chat transcript with full traceability).
#[tauri::command]
pub fn export_chat_session(
    state: State<'_, AppState>,
    id: String,
    turns: Option<Vec<usize>>,
) -> Result<String, String> {
    session::export_chat_session(state, id, turns)
}

/// Export a session as analysis-friendly JSON.
#[tauri::command]
pub fn export_chat_session_json(
    state: State<'_, AppState>,
    id: String,
    turns: Option<Vec<usize>>,
) -> Result<String, String> {
    session::export_chat_session_json(state, id, turns)
}

/// Structured RAG pipeline events for a session.
#[tauri::command]
pub fn get_ai_events(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<AiEventJson>, String> {
    session::get_ai_events(state, session_id)
}

/// Structured RAG pipeline events for one turn of a session.
#[tauri::command]
pub fn get_turn_ai_events(
    state: State<'_, AppState>,
    session_id: String,
    turn_number: usize,
) -> Result<Vec<AiEventJson>, String> {
    session::get_turn_ai_events(state, session_id, turn_number)
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

    #[test]
    fn session_store_evicts_oldest_at_50_cap() {
        let dir = std::env::temp_dir().join(format!("ls_ai_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // create 时按 updated_at 驱逐最旧，总量锁 50。
        let created: Vec<String> = (0..51)
            .map(|i| create_chat_session_impl(&dir).unwrap())
            .collect();
        let h = read_history(&dir);
        assert_eq!(h.sessions.len(), 50, "超过 50 应驱逐最旧");
        // 最新创建的必须存活（同秒驱逐顺序未定义，不断言具体哪条被逐）
        assert!(h.sessions.iter().any(|s| s.id == created[50]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_chat_session_updates_existing_not_duplicates() {
        let dir = std::env::temp_dir().join(format!("ls_ai_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut s = ChatSession {
            id: "s1".into(),
            title: String::new(),
            created_at: now_ts(),
            updated_at: now_ts(),
            messages: vec![ChatMessage { role: "user".into(), content: "第一问".into() }],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![],
            per_turn_scopes: vec![],
            retrieval_scope: vec![],
            strict_docs: false,
        };
        save_chat_session_impl(&dir, s.clone()).unwrap();
        // 无标题 → 从首条 user 消息推导
        let h = read_history(&dir);
        assert_eq!(h.sessions.len(), 1);
        assert_eq!(h.sessions[0].title, "第一问");

        // 再次保存（新消息）→ 原地更新，不新增
        s.messages.push(ChatMessage { role: "assistant".into(), content: "答1".into() });
        save_chat_session_impl(&dir, s.clone()).unwrap();
        let h = read_history(&dir);
        assert_eq!(h.sessions.len(), 1, "同 id 保存应原地更新");
        assert_eq!(h.sessions[0].messages.len(), 2);
        assert_eq!(h.sessions[0].title, "第一问", "标题不被第二存盘覆盖");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_chat_session_includes_turns_evidence_and_modes() {
        let dir = std::env::temp_dir().join(format!("ls_ai_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let session = ChatSession {
            id: "s1".into(),
            title: "克虏伯项目".into(),
            created_at: 1000,
            updated_at: 2000,
            retrieval_scope: vec!["a.pdf".into()],
            messages: vec![
                ChatMessage { role: "user".into(), content: "项目背景".into() },
                ChatMessage { role: "assistant".into(), content: "2015年启动。".into() },
                ChatMessage { role: "user".into(), content: "股权比例呢".into() },
                ChatMessage { role: "assistant".into(), content: "最终95%。".into() },
            ],
            source_ids: vec!["f1".into()],
            source_files: vec!["a.pdf".into()],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![PerTurnEvidence {
                turn_index: 0,
                file_ids: vec!["f1".into()],
                items: vec![EvidenceItem {
                    file_id: "f1".into(),
                    path: "a.pdf".into(),
                    snippet: "2015年初出让股权".into(),
                    bm25_score: Some(3.5),
                    semantic_score: None,
                    rrf_score: None,
                    rewritten: true,
                    rewritten_query: Some("项目 背景".into()),
                    from_history: false,
                    material_no: 0,
                    injected_spans: Vec::new(),
                }],
                trace_id: "s1#t1".into(),
                took_ms: 5200,
                llm_model: "p1:qwen2.5-7b-instruct".into(),
                embedding_model: "p1:bge-m3".into(),
                search_query: "克虏伯 项目背景".into(),
                hits: 3,
            }],
            per_turn_scopes: vec![PerTurnScope {
                turn_index: 0,
                scope: vec!["a.pdf".into()],
            }],
            strict_docs: true,
        };
        save_chat_session_impl(&dir, session).unwrap();
        let md = export_chat_session_impl(&dir, "s1", None).unwrap();
        assert!(md.contains("# 克虏伯项目"));
        assert!(md.contains("## 追溯信息"), "头部应有追溯信息块: {md}");
        assert!(md.contains("LLM 模型"), "追溯块应含模型: {md}");
        assert!(md.contains("## 第 1 轮"), "第一轮问题应导出: {md}");
        assert!(md.contains("项目背景"));
        assert!(md.contains("### 答"));
        assert!(md.contains("2015年启动"));
        assert!(md.contains("第 2 轮"), "第二轮问题应导出");
        assert!(md.contains("检索依据（1）"));
        assert!(md.contains("a.pdf"));
        assert!(md.contains("查询改写"));
        assert!(md.contains("严格模式"));
        assert!(md.contains("📁 检索范围: `a.pdf`"), "导出应含统一检索范围: {md}");
        assert!(md.contains("**检索范围:**"), "每轮应有范围快照: {md}");
        assert!(md.contains("`s1#t1`"), "应含 Trace ID: {md}");
        assert!(md.contains("**生成耗时**: 5200ms"), "应含耗时: {md}");
        assert!(md.contains("**最终检索查询**: `克虏伯 项目背景`"), "应含最终查询: {md}");
        assert!(md.contains("**BM25 命中数**: 3"), "应含命中数: {md}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_markdown_shows_unset_scope() {
        let dir = std::env::temp_dir().join(format!("ls_ai_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let session = ChatSession {
            id: "s3".into(),
            title: "无范围会话".into(),
            created_at: 0,
            updated_at: 0,
            retrieval_scope: vec![],
            messages: vec![
                ChatMessage { role: "user".into(), content: "外联发股权".into() },
                ChatMessage { role: "assistant".into(), content: "无法回答".into() },
            ],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![],
            per_turn_scopes: vec![PerTurnScope { turn_index: 0, scope: vec![] }],
            strict_docs: false,
        };
        save_chat_session_impl(&dir, session).unwrap();
        let md = export_chat_session_impl(&dir, "s3", None).unwrap();
        assert!(!md.contains("> 📁 检索范围"), "顶部空会话范围不显示: {md}");
        assert!(md.contains("**检索范围:**\n- 未指定（全库）"), "每轮空范围应标注: {md}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_json_marks_unset_scope_and_full_evidence() {
        let dir = std::env::temp_dir().join(format!("ls_ai_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let scope = "案件/CH 常宏案/05 工商内档/上海万联发实业发展有限公司".to_string();
        let session = ChatSession {
            id: "s2".into(),
            title: "外联发股权".into(),
            created_at: 0,
            updated_at: 0,
            retrieval_scope: vec![scope.clone()],
            messages: vec![
                ChatMessage { role: "user".into(), content: "关于外联发的股权是怎么转让的".into() },
                ChatMessage { role: "user".into(), content: "再找一下".into() },
                ChatMessage { role: "assistant".into(), content: "2010年转让给圣金".into() },
            ],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![PerTurnEvidence {
                turn_index: 1,
                file_ids: vec!["f1".into()],
                items: vec![EvidenceItem {
                    file_id: "f1".into(),
                    path: "内档变更.pdf".into(),
                    snippet: "外高桥转让给和兆".into(),
                    bm25_score: Some(20.83),
                    semantic_score: None,
                    rrf_score: Some(0.86),
                    rewritten: true,
                    rewritten_query: Some("万联发股权转让".into()),
                    from_history: false,
                    material_no: 0,
                    injected_spans: Vec::new(),
                }],
                ..Default::default()
            }],
            per_turn_scopes: vec![
                PerTurnScope { turn_index: 0, scope: vec![] },
                PerTurnScope { turn_index: 1, scope: vec![scope.clone()] },
            ],
            strict_docs: false,
        };
        save_chat_session_impl(&dir, session).unwrap();
        let json_str = export_chat_session_json_impl(&dir, "s2", None, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        // schema v3：无 ai_events 记录时省略 log 段（向后兼容）。
        assert_eq!(v["schema_version"], serde_json::json!(3));

        let t0 = &v["turns"][0];
        assert_eq!(t0["scope"], serde_json::json!([]), "未指定范围轮次 scope 应为空数组");
        assert_eq!(t0["answer"], serde_json::Value::Null, "无回答轮次 answer 应为 null");
        assert_eq!(t0["evidence"].as_array().map(Vec::len), Some(0), "无依据轮次 evidence 应为空数组");
        assert!(t0.get("log").is_none(), "无事件记录时不应输出 log 段");

        let t1 = &v["turns"][1];
        assert_eq!(t1["scope"][0], serde_json::json!(scope));
        assert_eq!(t1["answer"], serde_json::json!("2010年转让给圣金"));
        let ev = &t1["evidence"][0];
        assert_eq!(ev["bm25_score"], serde_json::json!(20.83));
        assert_eq!(ev["rrf_score"], serde_json::json!(0.86));
        assert_eq!(ev["rewritten_query"], serde_json::json!("万联发股权转让"));
        assert_eq!(ev["from_history"], serde_json::json!(false));
        assert_eq!(v["session"]["retrieval_scope"][0], serde_json::json!(scope));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: content.into() }
    }

    /// 轮次选择导出：仅导出勾选的轮次（json 的 turn_index 保持原始编号）。
    #[test]
    fn export_selected_turns_only() {
        let dir = std::env::temp_dir().join(format!("ls_export_turns_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = ChatSession {
            id: "s-turns".into(),
            title: "轮次选择".into(),
            created_at: 0,
            updated_at: 0,
            messages: vec![
                msg("user", "第一轮问题"),
                msg("assistant", "第一轮回答"),
                msg("user", "第二轮问题"),
                msg("assistant", "第二轮回答"),
            ],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![],
            per_turn_scopes: vec![],
            retrieval_scope: vec![],
            strict_docs: false,
        };
        save_chat_session_impl(&dir, session).unwrap();

        // 全部轮次（turns=None）。
        let all = export_chat_session_json_impl(&dir, "s-turns", None, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&all).unwrap();
        assert_eq!(v["turns"].as_array().map(Vec::len), Some(2));

        // 仅第 2 轮：turns 数组只含 turn_index=2。
        let only2 = export_chat_session_json_impl(&dir, "s-turns", None, Some(&[2])).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&only2).unwrap();
        let ts = v2["turns"].as_array().unwrap();
        assert_eq!(ts.len(), 1, "应只导出第 2 轮: {ts:?}");
        assert_eq!(ts[0]["turn_index"], serde_json::json!(2));

        // Markdown 同样支持轮次选择：只含第 1 轮标题，不含第 2 轮。
        let md = export_chat_session_impl(&dir, "s-turns", Some(&[1])).unwrap();
        assert!(md.contains("## 第 1 轮"), "md 应含第 1 轮");
        assert!(!md.contains("## 第 2 轮"), "md 不应含未选轮次");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// export JSON 每轮 log 段：ai_events 的结构化管线事件按轮次写入导出。
    #[test]
    fn export_json_includes_per_turn_events() {
        let dir = std::env::temp_dir().join(format!("ls_export_ev_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("ev.db");
        let pool = crate::db::get_pool(db_path.to_str().unwrap()).unwrap();
        {
            let c = pool.get().unwrap();
            crate::db::init_db(&c).unwrap();
            drop(c);
        }

        let session = ChatSession {
            id: "s-evt".into(),
            title: "带日志导出".into(),
            created_at: 0,
            updated_at: 0,
            messages: vec![msg("user", "毛弟被辱骂了么"), msg("assistant", "是的")],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![],
            per_turn_scopes: vec![],
            retrieval_scope: vec!["案件/和嘉案/聊天记录".into()],
            strict_docs: true,
        };
        save_chat_session_impl(&dir, session).unwrap();

        // 记录第 0 轮（ai_events 的 turn_number 是 0-based）的两个事件。
        {
            let c = pool.get().unwrap();
            crate::db::ai_events::record_event(
                &c, "s-evt", 0, 1, "query_rewrite",
                &serde_json::json!({"original": "q", "rewritten": "毛弟 辱骂", "was_rewritten": true, "rewrite_method": "rule"}),
            )
            .unwrap();
            crate::db::ai_events::record_event(
                &c, "s-evt", 0, 2, "retrieval",
                &serde_json::json!({"search_query": "毛弟 OR 辱骂", "bm25_hits": 132, "semantic_fused": false, "merged_hits": 6, "from_history_count": 0}),
            )
            .unwrap();
            drop(c);
        }

        let json_str = export_chat_session_json_impl(&dir, "s-evt", Some(&pool), None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["schema_version"], serde_json::json!(3));
        let log = &v["turns"][0]["log"];
        assert_eq!(log.as_array().map(Vec::len), Some(2), "应导出 2 条事件: {log}");
        assert_eq!(log[0]["event_type"], serde_json::json!("query_rewrite"));
        assert_eq!(log[0]["payload"]["rewritten"], serde_json::json!("毛弟 辱骂"));
        assert_eq!(log[1]["event_type"], serde_json::json!("retrieval"));
        assert_eq!(log[1]["payload"]["bm25_hits"], serde_json::json!(132));
        // 事件可读时间：本地时区 YYYY-MM-DD HH:MM:SS
        let ts_str = log[0]["created_at_readable"].as_str().unwrap_or_default().to_string();
        assert_eq!(ts_str.len(), 19, "created_at_readable 应为可读时间: {ts_str}");
        assert_eq!(log[0]["created_at_readable"].as_str().unwrap_or_default().as_bytes()[4], b'-');
        assert_eq!(log[0]["created_at_readable"].as_str().unwrap_or_default().as_bytes()[10], b' ');

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 材料可见编号：从完整/截断 context 反推实际可见的编号集合。
    #[test]
    fn visible_material_numbers_from_context() {
        let ctx = "[1]（a/1.pdf）\n内容一\n\n---\n\n[3]（a/3.pdf）\n内容三\n\n---\n\n[7]（a/7.pdf）\n内容七";
        assert_eq!(visible_material_numbers(ctx).iter().copied().collect::<Vec<_>>(), vec![1, 3, 7]);
        // 截断（第 3 份只剩开头，正则要求 [N]（ 完整出现才算可见）。
        let truncated = "[1]（a/1.pdf）\n内容一\n\n---\n\n[3]（a/3.pdf）\n内容三\n\n---\n\n[7";
        assert_eq!(visible_material_numbers(truncated).iter().copied().collect::<Vec<_>>(), vec![1, 3]);
        assert!(visible_material_numbers("").is_empty());
    }

    #[test]
    fn assign_material_no_is_stable_and_appends() {
        let mut order: Vec<String> = vec!["a".into(), "b".into()];
        assert_eq!(assign_material_no(&mut order, "a"), 1);
        assert_eq!(assign_material_no(&mut order, "b"), 2);
        assert_eq!(assign_material_no(&mut order, "c"), 3);
        assert_eq!(assign_material_no(&mut order, "a"), 1);
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn sanitize_citations_set_strips_numbers_not_in_set() {
        let valid: std::collections::BTreeSet<usize> = [3usize, 7].into_iter().collect();
        assert_eq!(sanitize_citations_set("依据[3]与[5]，另见[7][9]。", &valid), "依据[3]与5，另见[7]9。");
        assert!(sanitize_citations_set("见[1]。", &std::collections::BTreeSet::new()).contains("[1]"));
    }

    #[test]
    fn cited_excerpt_prefers_query_matching_passage() {
        let filler = "无关内容".repeat(40);
        let text = format!("{filler}目标条款：违约金为每日千分之五。{filler}");
        let s = cited_excerpt(&text, &query_terms("违约金 千分之五"), 60);
        assert!(s.contains("违约金"), "got: {s}");
        let head: String = text.chars().take(60).collect();
        assert_ne!(s, head, "should not fall back to head");
        let compound = cited_excerpt(&text, &query_terms("违约金额度条款"), 60);
        assert!(compound.contains("违约金"), "compound query: {compound}");
        assert_eq!(cited_excerpt("abcdef", &[], 3), "abc");
    }

    /// strict + 目录引用 + 检索零命中：回退注入范围内已索引文件，而非拒答。
    #[tokio::test]
    async fn e2e_strict_dir_scope_falls_back_to_scoped_files() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let dir = std::env::temp_dir().join(format!("ls_scope_fb_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("data.db");
        let pool = crate::db::get_pool(db_path.to_str().unwrap()).unwrap();
        {
            let c = pool.get().unwrap();
            crate::db::init_db(&c).unwrap();
            let d = crate::db::dir_config::add_dir(&c, dir.to_str().unwrap(), None, None, None, None, true).unwrap();
            // 范围内两个已索引文件（内容与问句"这是什么文档"无词重叠）。
            for (rel, md5, text) in [
                ("案件/X/庭审录音/0001 录音.txt", "md5-f1", "审判长：现在开庭。原告陈述诉讼请求。"),
                ("案件/X/庭审录音/0002 录音.txt", "md5-f2", "被告：对证据三的真实性没有异议。"),
            ] {
                let id = crate::db::tracker::upsert_file(&c, rel, &d.id, 100, 10, Some(md5)).unwrap();
                crate::db::tracker::store_content(&c, md5, text, false, None).unwrap();
                crate::db::tracker::update_indexed(&c, &id, Some(md5)).unwrap();
            }
            drop(c);
        }

        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(crate::indexer::IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let state = AppState::new(
            pool, im, indexer, scanner,
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            dir.clone(), index_dir, db_path, dummy_tx, None,
        );

        let messages = vec![ChatMessage { role: "user".into(), content: "这是什么文档".into() }];
        let scope = TurnScope { mention_files: vec![], mention_dirs: vec![], inherit_from: vec![], conditions: vec![] };
        let session_scope = vec!["案件/X/庭审录音".to_string()];

        let prep = prepare_conversation_prompt(
            &state, &messages, &[], &scope, &session_scope, true, false, true, None, "", None,
        )
        .await
        .expect("strict 目录引用在检索零命中时应回退注入范围内文件，而非报错");

        assert!(!prep.evidence.is_empty(), "应注入范围内已索引文件: {:?}", prep.evidence);
        assert!(prep.evidence.iter().all(|e| e.path.starts_with("案件/X/庭审录音")), "材料应在范围目录内");
        assert!(!prep.system.contains("本轮共提供 0 份材料"), "system 材料数应包含兜底文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 改写结果必须携带可检索词，否则视为无效（回退规则链 + 范围兜底），
    /// 避免"LLM 改成泛词 → 过滤后仍为空"。
    #[test]
    fn valid_rewrite_output_requires_retrievable_term() {
        // 带实体：有效
        assert_eq!(
            valid_rewrite_output("毛弟 辱骂 聊天记录", "列出表格").as_deref(),
            Some("毛弟 辱骂 聊天记录")
        );
        // 全是停用词/泛词：无效
        assert!(valid_rewrite_output("文档 内容 材料", "这是什么文档").is_none());
        // 空 / 回显原句：无效
        assert!(valid_rewrite_output("", "这是什么文档").is_none());
        assert!(valid_rewrite_output("这是什么文档", "这是什么文档").is_none());
    }

    #[test]
    fn rewrite_query_expands_demonstrative_followup_with_parent_keywords() {
        let history = vec![
            msg("user", "季度报告的主要结论是什么"),
            msg("assistant", "结论是营收增长 20%。"),
            msg("user", "它的风险有哪些"),
        ];
        let out = rewrite_query("它的风险有哪些", &history);
        assert_ne!(out.query, "它的风险有哪些", "deictic follow-up must be rewritten");
        let rewritten = &out.query;
        assert!(!rewritten.starts_with("它"), "deictic head must be replaced: {rewritten}");
        assert!(rewritten.contains("季度"), "must carry parent keywords: {rewritten}");
        assert!(rewritten.contains("报告"), "must carry parent keywords: {rewritten}");
    }

    #[test]
    fn rewrite_query_returns_original_for_self_contained_question() {
        let history = vec![msg("user", "季度报告")];
        let rewritten = rewrite_query("为什么营收下降", &history);
        assert_eq!(rewritten.query, "为什么营收下降");
    }

    #[test]
    fn rewrite_query_short_question_borrows_parent_keywords() {
        let history = vec![
            msg("user", "如何配置索引目录"),
            msg("assistant", "在设置页添加目录即可。"),
            msg("user", "增量呢"),
        ];
        let out = rewrite_query("增量呢", &history);
        assert_ne!(out.query, "增量呢");
        assert!(out.query.contains("索引"), "short follow-up must borrow parent keywords: {}", out.query);
    }

    #[test]
    fn rewrite_query_triggers_on_referential_time_preface() {
        let history = vec![
            msg("user", "请总结这份年度财务报告"),
            msg("assistant", "报告显示营收增长 20%。"),
            msg("user", "刚才提到的那份报告呢"),
        ];
        let out = rewrite_query("刚才提到的那份报告呢", &history);
        assert_ne!(out.query, "刚才提到的那份报告呢", "referential preface must trigger rewrite");
        assert!(out.query.contains("年度"), "must carry parent keywords: {}", out.query);
        assert!(out.query.contains("报告"), "must carry parent keywords: {}", out.query);
    }

    #[test]
    fn valid_rewrite_output_rejects_garbage_and_echoes() {
        assert!(valid_rewrite_output("", "原问题").is_none());
        assert!(valid_rewrite_output("  原问题  ", "原问题").is_none());
        // 无检索词（单字符）同样无效：不会比原句更有用。
        assert!(valid_rewrite_output("x", "原问题").is_none());
        assert!(valid_rewrite_output("营收", "原问题").is_some());
        let good = valid_rewrite_output("季度报告的风险有哪些", "它的风险有哪些");
        assert_eq!(good.as_deref(), Some("季度报告的风险有哪些"));
    }

    #[test]
    fn rrf_fuse_ranks_common_docs_first() {
        let bm25 = vec![
            ("a".to_string(), 5.0),
            ("b".to_string(), 4.0),
            ("c".to_string(), 3.0),
        ];
        let sem = vec![
            ("a".to_string(), 0.9),
            ("d".to_string(), 0.8),
            ("b".to_string(), 0.2),
        ];
        let fused = rrf_fuse(&bm25, &sem, 60.0);
        // "a" in both lists at rank 0 → highest fused score.
        assert_eq!(fused[0].0, "a");
        let pos = |id: &str| fused.iter().position(|(f, _)| f == id).unwrap();
        assert!(pos("b") < pos("c"), "b (in both) must outrank c (BM25-only)");
        assert!(pos("b") < pos("d"), "b (in both) must outrank d (semantic-only)");
    }

    #[test]
    fn weighted_mix_prefers_keyword_when_weight_low() {
        // 权重 0.1（偏关键词）：BM25 高的文档排前，即使 cosine 低。
        let hits = vec![
            ("kw".to_string(), 10.0, 0.1), // BM25 高、语义低
            ("sem".to_string(), 2.0, 0.9), // BM25 低、语义高
        ];
        let r = weighted_mix(hits, 0.1);
        assert_eq!(r[0].0, "kw", "低权重应偏向关键词命中");
        // normalize: kw bm25=10→1.0, sem bm25=2→0.2
        // kw mix = 0.1×0.1 + 0.9×1.0 = 0.91；sem mix = 0.1×0.9 + 0.9×0.2 = 0.27
        assert!((r[0].3 - 0.91).abs() < 1e-9, "kw mix must be 0.91, got {}", r[0].3);
    }

    #[test]
    fn weighted_mix_prefers_semantic_when_weight_high() {
        let hits = vec![
            ("kw".to_string(), 10.0, 0.1),
            ("sem".to_string(), 2.0, 0.9),
        ];
        let r = weighted_mix(hits, 0.9);
        assert_eq!(r[0].0, "sem", "高权重应偏向语义命中");
    }

    #[test]
    fn weighted_mix_empty_or_zero_bm25_handled() {
        // 空输入
        assert!(weighted_mix(vec![], 0.3).is_empty());
        // BM25 全 0 → 归一化 0，仅 cosine 主导
        let r = weighted_mix(vec![("a".to_string(), 0.0, 0.8), ("b".to_string(), 0.0, 0.2)], 0.5);
        assert_eq!(r[0].0, "a");
        assert_eq!(r[0].1, 0.0, "zero BM25 normalizes to 0");
    }

    #[test]
    fn evidence_item_json_round_trip_preserves_all_fields() {
        let e = EvidenceItem {
            file_id: "f1".into(),
            path: "a.pdf".into(),
            snippet: "摘要内容".into(),
            bm25_score: Some(3.5),
            semantic_score: Some(0.89),
            rrf_score: Some(0.033),
            rewritten: true,
            rewritten_query: Some("季度报告 它的风险".into()),
            from_history: false,
            material_no: 0,
            injected_spans: Vec::new(),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: EvidenceItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file_id, "f1");
        assert_eq!(back.path, "a.pdf");
        assert_eq!(back.snippet, "摘要内容");
        assert_eq!(back.bm25_score, Some(3.5));
        assert_eq!(back.semantic_score, Some(0.89));
        assert_eq!(back.rrf_score, Some(0.033));
        assert!(back.rewritten);
        assert_eq!(back.rewritten_query.as_deref(), Some("季度报告 它的风险"));
        assert!(!back.from_history);
    }

    #[test]
    fn evidence_item_deserializes_legacy_json_without_scores() {
        let legacy = r#"{"file_id":"f1","path":"a.pdf","snippet":"x"}"#;
        let e: EvidenceItem = serde_json::from_str(legacy).unwrap();
        assert_eq!(e.bm25_score, None);
        assert_eq!(e.semantic_score, None);
        assert_eq!(e.rrf_score, None);
        assert!(!e.rewritten);
        assert_eq!(e.rewritten_query, None);
        assert!(!e.from_history);
    }

    #[test]
    fn chat_session_deserializes_legacy_json_without_per_turn_evidence() {
        let legacy = r#"{"id":"s1","title":"t","created_at":1,"updated_at":2,"messages":[],"source_ids":[],"source_files":[]}"#;
        let s: ChatSession = serde_json::from_str(legacy).unwrap();
        assert!(s.per_turn_evidence.is_empty(), "legacy records must default to empty");
    }

    #[test]
    fn session_json_round_trip_preserves_items() {
        let s = ChatSession {
            id: "s1".into(),
            title: "t".into(),
            created_at: 1,
            updated_at: 2,
            messages: vec![],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_scopes: vec![],
            retrieval_scope: vec![],
            strict_docs: false,
            per_turn_evidence: vec![PerTurnEvidence {
                turn_index: 0,
                file_ids: vec!["f1".into()],
                items: vec![EvidenceItem {
                    file_id: "f1".into(),
                    path: "a.pdf".into(),
                    snippet: "x".into(),
                    bm25_score: Some(1.0),
                    semantic_score: None,
                    rrf_score: None,
                    rewritten: true,
                    rewritten_query: Some("q".into()),
                    from_history: false,
                    material_no: 0,
                    injected_spans: Vec::new(),
                }],
                ..Default::default()
            }],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ChatSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back.per_turn_evidence[0].items.len(), 1);
        assert_eq!(back.per_turn_evidence[0].items[0].rewritten_query.as_deref(), Some("q"));
    }
}

#[cfg(test)]
mod mention_resolve_tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(prefix: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ls_mention_{prefix}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &std::path::Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn setup_db(tmp: &TempDir) -> rusqlite::Connection {
        let db_path = tmp.path().join("test.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        crate::db::init_db(&conn).unwrap();
        // Insert a test file with content
        let _id = crate::db::tracker::upsert_file(&conn, "report.pdf", "dir-1", 0, 1024, Some("md5-report")).unwrap();
        crate::db::tracker::store_content(&conn, "md5-report", "Quarterly report content.", false, None).unwrap();
        // Insert another file with similar name for ambiguity test
        let _id2 = crate::db::tracker::upsert_file(&conn, "docs/report_2024.pdf", "dir-1", 0, 2048, Some("md5-report2")).unwrap();
        crate::db::tracker::store_content(&conn, "md5-report2", "2024 report content.", false, None).unwrap();
        conn
    }

    #[test]
    fn strict_zero_overlap_mention_in_evidence() {
        let tmp = TempDir::new("zero_overlap");
        let conn = setup_db(&tmp);
        let (resolved, missing) = resolve_mention_file_ids(&conn, &["report.pdf".to_string()]);
        assert_eq!(resolved.len(), 1, "exact path should resolve");
        assert_eq!(resolved[0].1, "report.pdf", "resolved path should match");
        assert!(missing.is_empty(), "no missing files");
    }

    #[test]
    fn strict_missing_mention_errors() {
        let tmp = TempDir::new("missing");
        let conn = setup_db(&tmp);
        let (resolved, missing) = resolve_mention_file_ids(&conn, &["nonexistent.pdf".to_string()]);
        assert!(resolved.is_empty(), "no files should resolve");
        assert_eq!(missing.len(), 1, "one missing file");
        assert_eq!(missing[0], "nonexistent.pdf");
    }

    #[test]
    fn strict_ambiguous_mention_errors() {
        let tmp = TempDir::new("ambiguous");
        let conn = setup_db(&tmp);
        // "report" matches both report.pdf and docs/report_2024.pdf via LIKE
        let (resolved, missing) = resolve_mention_file_ids(&conn, &["report".to_string()]);
        assert!(resolved.is_empty(), "ambiguous match should not resolve — 2 LIKE hits");
        assert_eq!(missing.len(), 1, "ambiguous path should be missing");
    }

    #[test]
    fn strict_excludes_history_sources() {
        let tmp = TempDir::new("excludes_history");
        let conn = setup_db(&tmp);
        let (resolved, missing) = resolve_mention_file_ids(&conn, &["report.pdf".to_string()]);
        assert_eq!(resolved.len(), 1, "exact match should resolve");
        assert!(missing.is_empty());
        assert_eq!(resolved[0].1, "report.pdf");
    }

    #[test]
    fn mention_dir_no_strict_false_error() {
        let tmp = TempDir::new("no_strict");
        let conn = setup_db(&tmp);
        let (resolved, missing) = resolve_mention_file_ids(
            &conn,
            &["report.pdf".to_string(), "missing.pdf".to_string()],
        );
        assert_eq!(resolved.len(), 1, "report.pdf should resolve");
        assert_eq!(resolved[0].1, "report.pdf");
        assert_eq!(missing.len(), 1, "missing.pdf should be missing");
        assert_eq!(missing[0], "missing.pdf");
    }

    #[test]
    fn scope_empty_is_all_library() {
        let dir = std::env::temp_dir().join(format!("ls_ai_export_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let session = ChatSession {
            id: "s1".into(),
            title: "Test".into(),
            created_at: 1,
            updated_at: 2,
            messages: vec![
                ChatMessage { role: "user".into(), content: "问".into() },
                ChatMessage { role: "assistant".into(), content: "答".into() },
            ],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![],
            per_turn_scopes: vec![PerTurnScope {
                turn_index: 0,
                scope: vec!["".into()],
            }],
            retrieval_scope: vec![],
            strict_docs: false,
        };
        save_chat_session_impl(&dir, session).unwrap();
        let md = export_chat_session_impl(&dir, "s1", None).unwrap();
        assert!(md.contains("全库"), "empty scope should render as 全库: {md}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_unset_is_all_library() {
        let dir = std::env::temp_dir().join(format!("ls_ai_export_test2_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let session = ChatSession {
            id: "s2".into(),
            title: "Test".into(),
            created_at: 1,
            updated_at: 2,
            messages: vec![
                ChatMessage { role: "user".into(), content: "问".into() },
                ChatMessage { role: "assistant".into(), content: "答".into() },
            ],
            source_ids: vec![],
            source_files: vec![],
            pending_query: None,
            pending_started_at: None,
            per_turn_evidence: vec![],
            per_turn_scopes: vec![],
            retrieval_scope: vec![],
            strict_docs: false,
        };
        save_chat_session_impl(&dir, session).unwrap();
        let md = export_chat_session_impl(&dir, "s2", None).unwrap();
        assert!(!md.contains("检索范围"), "no per_turn_scopes should not render scope: {md}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod auto_cite_tests {
    use super::*;

    fn ev(path: &str, snippet: &str) -> EvidenceItem {
        EvidenceItem {
            file_id: "f1".into(), path: path.into(), snippet: snippet.into(),
            bm25_score: None, semantic_score: None, rrf_score: None,
            rewritten: false, rewritten_query: None, from_history: false,
            material_no: 0,
            injected_spans: Vec::new(),
        }
    }

    #[test]
    fn no_evidence_returns_original() {
        let result = auto_cite("这是一段回答。", &[]);
        assert_eq!(result, "这是一段回答。");
    }

    #[test]
    fn already_tagged_kept_as_is() {
        let evidence = vec![ev("a.pdf", "违约金千分之五")];
        let result = auto_cite("违约金为千分之五[1]。", &evidence);
        assert!(result.contains("[1]"));
    }

    #[test]
    fn untagged_sentence_gets_citation() {
        let evidence = vec![ev("a.pdf", "合同约定违约金为每日千分之五")];
        let result = auto_cite("合同约定违约金为每日千分之五。这是行业惯例。", &evidence);
        assert!(result.contains("[1]"), "first sentence should be cited: {}", result);
    }

    #[test]
    fn unmatched_sentence_no_citation() {
        let evidence = vec![ev("a.pdf", "违约金千分之五")];
        let result = auto_cite("今天是星期三。", &evidence);
        assert!(!result.contains("[1]"), "unmatched sentence should not be cited: {}", result);
    }

    #[test]
    fn consecutive_same_source_merged() {
        let evidence = vec![ev("a.pdf", "违约金 千分之五 每日")];
        let result = auto_cite("违约金千分之五。每日计算。", &evidence);
        let count = result.matches("[1]").count();
        assert_eq!(count, 1, "consecutive same source should merge into one [1]: {}", result);
    }

    #[test]
    fn citation_beyond_visible_bound_is_dropped() {
        let evidence = vec![ev("a.pdf", "违约金千分之五"), ev("b.pdf", "租赁期限三年")];
        let result = auto_cite(&sanitize_citations("租赁期限为三年[2]。", 1), &evidence[..1]);
        assert!(!result.contains("[2]"), "citation beyond visible bound leaked: {}", result);
    }

    #[test]
    fn generic_sentence_not_cited_by_incidental_overlap() {
        let evidence = vec![ev(
            "a.pdf",
            "委托因歙县渔梁路129号房屋的不动产登记簿记载事项有错误，需前往歙县不动产登记中心办理更正登记手续",
        )];
        let result = auto_cite("注：以上回答仅基于提供的材料，具体操作请以当地不动产登记中心最新要求为准。", &evidence);
        assert!(!result.contains("[1]"), "generic disclaimer should not be cited: {}", result);
    }
}

#[cfg(test)]
mod auto_cite_md_tests {
    use super::*;

    fn ev(snippet: &str) -> EvidenceItem {
        EvidenceItem {
            file_id: "f1".into(), path: "合同.pdf".into(), snippet: snippet.into(),
            bm25_score: None, semantic_score: None, rrf_score: None,
            rewritten: false, rewritten_query: None, from_history: false,
            material_no: 0,
            injected_spans: Vec::new(),
        }
    }

    #[test]
    fn markdown_bold_preserved() {
        let e = vec![ev("违约金千分之五每日")];
        let input = "**合同分析**\n\n根据合同约定，违约金为每日千分之五。\n\n**结论**：略高。";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(result.contains("**合同分析**"), "bold not preserved: {}", result);
        assert!(result.contains("**结论**"), "bold not preserved: {}", result);
    }

    #[test]
    fn markdown_table_preserved() {
        let e = vec![ev("违约金千分之五")];
        let input = "| 条款 | 内容 |\n|------|------|\n| 违约金 | 千分之五 |";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(result.contains("|------|"), "table separator broken: {}", result);
        assert!(result.contains("| 条款 |"), "table header broken: {}", result);
    }

    #[test]
    fn markdown_code_block_preserved() {
        let e = vec![ev("违约金千分之五")];
        let input = "代码如下：\n```rust\nlet x = 1;\n```\n结束。";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(result.contains("```rust"), "code block broken: {}", result);
        assert!(result.contains("let x = 1;"), "code content broken: {}", result);
    }

    #[test]
    fn markdown_list_preserved() {
        let e = vec![ev("违约金千分之五")];
        let input = "主要条款：\n1. 违约金按日计算\n2. 不影响继续履行";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(result.contains("1. 违约金按日计算"), "list broken: {}", result);
        assert!(result.contains("2. 不影响继续履行"), "list broken: {}", result);
    }

    #[test]
    fn markdown_heading_preserved() {
        let e = vec![ev("违约金千分之五")];
        let input = "## 主要条款\n\n违约金为千分之五。";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(result.contains("## 主要条款"), "heading broken: {}", result);
    }

    // ---- sanitize_citations ----

    #[test]
    fn sanitize_keeps_in_range_citations() {
        let s = sanitize_citations("违约金是千分之五[1]，每日计算。", 3);
        assert_eq!(s, "违约金是千分之五[1]，每日计算。");
    }

    #[test]
    fn sanitize_strips_out_of_range_citation() {
        // evidence 只有 1 条，LLM 却引用了 [99] → 剥括号留文字
        let s = sanitize_citations("相关内容见判决书[99]的记载。", 1);
        assert_eq!(s, "相关内容见判决书99的记载。", "got: {s}");
        assert!(!s.contains("[99]"));
    }

    #[test]
    fn sanitize_strips_zero_and_handles_multi() {
        let s = sanitize_citations("依据[1]与[0]，另见[2][5]。", 2);
        // [1][2] 合法保留；[0][5] 越界剥离
        assert_eq!(s, "依据[1]与0，另见[2]5。", "got: {s}");
    }

    #[test]
    fn sanitize_empty_evidence_returns_original() {
        let s = sanitize_citations("没有任何引用[7]的痕迹。", 0);
        assert_eq!(s, "没有任何引用[7]的痕迹。");
    }

    #[test]
    fn sanitize_plain_text_untouched() {
        let s = sanitize_citations("这是没有方括号引用的普通文本。", 5);
        assert_eq!(s, "这是没有方括号引用的普通文本。");
    }

    // ---- 小数/版本号等含 `.` 内容不被断句切开（0.[1]3 回归）----

    #[test]
    fn decimal_number_not_split_by_citation() {
        // 回归：物业服务费"下调为0.3元/天/平方米"曾被 `.` 断句切开，
        // 前半句"下调为0."被补引成"0.[1]3"。`.` 不再是句终符后应整体保留。
        let e = vec![ev("违约金千分之五"), ev("物业服务费标准为10元每平方米每月")];
        let input = "物业服务费标准原为10元/月·平方米，后下调为0.3元/天/平方米。";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(
            !result.contains("0.[1]") && !result.contains(".[1]"),
            "citation inserted inside decimal: {result}"
        );
        assert!(result.contains("0.3"), "decimal mangled: {result}");
    }

    #[test]
    fn version_like_number_not_split() {
        let e = vec![ev("软件版本")];
        let input = "系统升级到 v2.1.3 版本后恢复正常。";
        let result = auto_cite(input, &e);
        eprintln!("OUTPUT: {}", result);
        assert!(result.contains("v2.1.3"), "version mangled: {result}");
        assert!(!result.contains("2.["), "citation inside version: {result}");
    }
}

#[cfg(test)]
mod retrieval_keyword_tests {
    use super::*;

    #[test]
    fn extracts_person_name_from_full_question() {
        // 完整问句中"常宏"是核心实体，泛词（民事/案件/多少）必须被过滤
        let kws = extract_retrieval_keywords("涉及常宏的民事案件一共有多少，请列表");
        assert_eq!(kws, vec!["常宏"], "got: {kws:?}");
    }

    #[test]
    fn extracts_company_and_topic() {
        let kws = extract_retrieval_keywords("万城的股东资格确认纠纷");
        assert!(kws.contains(&"万城".to_string()), "got: {kws:?}");
        assert!(kws.contains(&"股东".to_string()), "got: {kws:?}");
    }

    #[test]
    fn keeps_topic_word_for_generic_question() {
        // 无专有实体时保留主题词（"纪要"），调用方用该词检索
        let kws = extract_retrieval_keywords("上周会议纪要");
        assert!(kws.contains(&"纪要".to_string()), "got: {kws:?}");
    }

    #[test]
    fn single_person_name_kept_whole() {
        assert_eq!(extract_retrieval_keywords("常宏"), vec!["常宏"]);
    }

    #[test]
    fn full_word_survives_positional_cutoff() {
        // 回归：MAX_KEYWORDS=3 时"不动产"（排在子词"不动"/"动产"之后）被截断丢弃
        let kws = extract_retrieval_keywords("律师委托查询不动产手续材料");
        assert!(kws.contains(&"不动产".to_string()), "got: {kws:?}");
        assert!(!kws.contains(&"不动".to_string()), "sub-word leaked: {kws:?}");
        assert!(!kws.contains(&"动产".to_string()), "sub-word leaked: {kws:?}");
    }

    #[test]
    fn sub_word_fragments_deduped_to_full_word() {
        let kws = extract_retrieval_keywords("不动产权证书遗失声明");
        assert!(kws.contains(&"不动产".to_string()), "got: {kws:?}");
        assert!(!kws.contains(&"不动".to_string()), "sub-word leaked: {kws:?}");
        assert!(!kws.contains(&"动产".to_string()), "sub-word leaked: {kws:?}");
    }

    #[test]
    fn proper_nouns_kept_whole_by_search_mode() {
        // 守护：切词模式切回 Default 会把人名拆成单字并被 <2 chars 过滤丢弃
        let kws = extract_retrieval_keywords("涉及常宏的民事案件一共有多少，请列表");
        assert!(kws.contains(&"常宏".to_string()), "got: {kws:?}");
        let kws = extract_retrieval_keywords("万城的股东资格确认纠纷");
        assert!(kws.contains(&"万城".to_string()), "got: {kws:?}");
        let kws = extract_retrieval_keywords("联嵘公司的工商变更沿革");
        assert!(kws.contains(&"联嵘".to_string()), "got: {kws:?}");
    }

    #[test]
    fn new_function_word_stopwords_filtered() {
        let kws = extract_retrieval_keywords("请根据工商登记材料说明情况");
        assert!(!kws.contains(&"根据".to_string()), "got: {kws:?}");
    }

    #[test]
    fn no_positional_keyword_cap() {
        let kws = extract_retrieval_keywords("常宏 郑坚敏 万城 违约金 比例 审计");
        for want in ["常宏", "郑坚敏", "万城", "违约金", "审计"] {
            assert!(kws.contains(&want.to_string()), "missing {want}, got: {kws:?}");
        }
        assert!(!kws.contains(&"违约".to_string()), "sub-word leaked: {kws:?}");
        assert!(!kws.contains(&"约金".to_string()), "sub-word leaked: {kws:?}");
    }

    #[test]
    fn rewrite_history_ignores_assistant_turns() {
        let msgs = vec![
            ChatMessage { role: "user".into(), content: "关于外联发的股权转让".into() },
            ChatMessage { role: "assistant".into(), content: "根据材料，没有相关记录".into() },
        ];
        let h = rewrite_history(&msgs, &[]);
        assert!(h.contains("股权转让"), "user content missing: {h}");
        assert!(!h.contains("没有相关记录"), "assistant content leaked: {h}");
    }

    #[test]
    fn rewrite_history_includes_context_file_names() {
        let msgs = vec![ChatMessage { role: "user".into(), content: "为什么认定他是利害关系人".into() }];
        let paths = vec!["案件/WJY 汪均益/行政诉讼/汪均益行政上诉案.docx".to_string()];
        let h = rewrite_history(&msgs, &paths);
        assert!(h.contains("汪均益行政上诉案.docx"), "context file missing: {h}");
    }

    #[test]
    fn keeps_case_numbers_but_drops_short_numbers() {
        let kws = extract_retrieval_keywords("41833号案件 常宏");
        assert!(kws.iter().any(|k| k == "41833"), "案号是文件名锚点，不应丢弃: {kws:?}");
        assert!(kws.contains(&"常宏".to_string()), "got: {kws:?}");
        let short = extract_retrieval_keywords("第 12 号");
        assert!(short.iter().all(|k| k != "12"), "短数字无区分度，应丢弃: {short:?}");
    }
}

#[cfg(test)]
mod clarify_blocking_tests {
    use super::*;
    use crate::commands::clarify::{Resolution, SlotOutcome, Verdict};

    fn res(v: Verdict, stype: &str) -> Resolution {
        Resolution {
            verdict: v,
            outcomes: vec![SlotOutcome::Ask {
                surface: "他".into(),
                stype: stype.into(),
                options: vec!["甲".into(), "乙".into()],
            }],
        }
    }

    #[test]
    fn blocking_only_when_unanchored_and_unbound() {
        let q = "判决书里为什么认定他是利害关系人？";
        assert!(clarify_is_blocking(q, &res(Verdict::Ambiguous, "person")), "无锚点+未绑定 → 应只问不答");
        assert!(clarify_is_blocking(q, &res(Verdict::Ambiguous, "case")));
        // 裁决已绑定 → 不问
        assert!(!clarify_is_blocking(q, &res(Verdict::Answerable, "person")));
        // 槽类型不是 case/person（如 doc/org）→ 不阻断
        assert!(!clarify_is_blocking(q, &res(Verdict::Ambiguous, "doc")));
    }

    #[test]
    fn anchored_question_is_never_blocking() {
        // 已具名 → 有锚点可依，不该拒答
        let q = "汪均益案中，被告歙县自然资源和规划局在答辩状里是怎么抗辩的？";
        assert!(!clarify_is_blocking(q, &res(Verdict::Ambiguous, "person")));
    }

    /// 阻塞轮 `raw_text` 就是提示本身 → 必须只输出一份，且绝不能跑 auto_cite
    /// （提示列出的实体名会误挂材料引用，如 [9]）。
    #[test]
    fn blocked_turn_emits_note_once_and_skips_citations() {
        let note = "（提示：他 → 汪均益 / 汪少荣。如需精确，请用 @ 指定文件或目录。）";
        let mut cite_called = false;
        let out = compose_answer_text(true, note.to_string(), Some(note.to_string()), |_| {
            cite_called = true;
            "提示：他 → 汪均益 [9]".to_string()
        });
        assert!(!cite_called, "阻塞轮不应调用 auto_cite");
        assert_eq!(out, note, "提示必须只出现一次");
        assert_eq!(out.matches("提示：").count(), 1, "提示重复输出");
        assert!(!out.contains('['), "提示不该带引用编号");
    }

    /// 非阻塞轮：提示前置一次，答案正文保留 auto_cite 结果。
    #[test]
    fn non_blocking_turn_prepends_note_once() {
        let out = compose_answer_text(
            false,
            "答案正文".to_string(),
            Some("（提示：他 → 甲）".to_string()),
            |raw| format!("{raw}[1]"),
        );
        assert_eq!(out, "（提示：他 → 甲）\n\n答案正文[1]");
    }

    /// 无提示时原样返回带引用的正文。
    #[test]
    fn no_note_returns_cited_text() {
        let out = compose_answer_text(false, "答案正文".to_string(), None, |raw| format!("{raw}[2]"));
        assert_eq!(out, "答案正文[2]");
    }
}

#[cfg(test)]
mod chunk_budget_tests {
    use super::*;

    /// 长文本 + 预算 → 内容不超过预算（含 20 字符块头开销），但允许追加截断提示元信息
    #[test]
    fn respects_char_budget() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let long = "违约".repeat(2000); // 4000 字符
        let (out, _) = chunked_or_truncated_with_budget(&conn, "md5-x", &long, "违约", 500, &[]);
        // 截断提示元信息不计入内容预算；正文部分仍应受限。
        let body = out.split("〔注：").next().unwrap_or(&out);
        assert!(body.chars().count() <= 520, "budget exceeded: {}", body.chars().count());
        assert!(!out.trim().is_empty());
    }

    /// 长文本超出预算 → 返回内容带显式"材料被截断"提示（可感知截断）
    #[test]
    fn truncated_text_gets_notice() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let long = "违约".repeat(2000); // 4000 字符
        let (out, _) = chunked_or_truncated_with_budget(&conn, "md5-note", &long, "违约", 500, &[]);
        assert!(out.contains("材料过长"), "missing truncation notice: {}", &out[out.len().saturating_sub(80)..]);
        assert!(out.contains("未全部展示"), "missing truncation notice: {}", &out[out.len().saturating_sub(80)..]);
    }

    /// 短文本 ≤ 预算 → 全文返回，无截断提示
    #[test]
    fn short_text_returned_whole() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let short = "常宏诉万城公司股东资格确认纠纷案";
        let (out, _) = chunked_or_truncated_with_budget(&conn, "md5-y", short, "常宏", 10_000, &[]);
        assert_eq!(out, short);
    }

    /// 预算 0 → 返回空
    #[test]
    fn zero_budget_returns_empty() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let (out, _) = chunked_or_truncated_with_budget(&conn, "md5-z", "任意内容", "q", 0, &[]);
        assert!(out.is_empty());
    }

    /// 语义命中块优先：命中块即便词重叠低也排前面
    #[test]
    fn hit_chunks_prioritized() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        // 构造一个长文档（>1 万字符触发分块），写入 doc_chunks
        let md5 = "md5-hit";
        let long = format!("{}。{}。", "A".repeat(6000), "B".repeat(6000)); // 12000 字符
        crate::db::tracker::store_content(&conn, md5, &long, false, None).unwrap();
        let windows = crate::db::chunks::chunk_text(&long);
        assert!(!windows.is_empty(), "chunk_text should split long text");
        crate::db::chunks::replace_chunks(&conn, md5, &windows).unwrap();
        let chunks = crate::db::chunks::get_chunks(&conn, md5).unwrap();
        assert!(!chunks.is_empty(), "chunks should exist for long text");
        // 预算只够 1 块，命中块是最后一块 → 应返回它而非第一块
        let hit_idx = chunks.last().unwrap().chunk_index as usize;
        let (out, _) = chunked_or_truncated_with_budget(&conn, md5, &long, "无关词", 700, &[(hit_idx, 0.9)]);
        assert!(out.contains(&format!("第{}", chunks.last().unwrap().start_char)), "hit chunk not prioritized: {out}");
    }

    /// 返回值第二项是**源文档坐标**的注入区间——评测据此判定答案段是否进入上下文
    #[test]
    fn returns_source_document_spans() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        let md5 = "md5-spans";
        let long = format!("{}。{}。", "A".repeat(6000), "B".repeat(6000));
        crate::db::tracker::store_content(&conn, md5, &long, false, None).unwrap();
        let windows = crate::db::chunks::chunk_text(&long);
        crate::db::chunks::replace_chunks(&conn, md5, &windows).unwrap();
        let chunks = crate::db::chunks::get_chunks(&conn, md5).unwrap();
        let last = chunks.last().unwrap();
        let (_, spans) = chunked_or_truncated_with_budget(
            &conn, md5, &long, "无关词", 700, &[(last.chunk_index as usize, 0.9)],
        );
        assert_eq!(spans, vec![(last.start_char as usize, last.end_char as usize)]);

        let short = "短文本";        let (_, whole) = chunked_or_truncated_with_budget(&conn, "md5-w", short, "q", 10_000, &[]);
        assert_eq!(whole, vec![(0, short.chars().count())]);
    }

    /// 追问泛词（检索关键词为空）时，LLM 重写丢失父问句核心实体 → 硬性兜底补全。
    /// 对应缺陷 1：首问"毛弟是否被人辱骂？"，追问"列出清单，把时间、辱骂人以及完整的辱骂内容列出"
    /// 改写后 query 若不包含"毛弟"，必须把父问句实体合并进去。
    #[test]
    fn ensure_parent_entities_reinjects_missing_core_entity() {
        let parent = "毛弟是否被人辱骂？";
        let rewritten = "辱骂清单 时间 辱骂人 完整辱骂内容"; // 假设 LLM 丢了"毛弟"
        let merged = ensure_parent_entities(rewritten, parent);
        assert!(merged.contains("毛弟"), "父问句核心实体必须补回: {merged}");
        assert!(merged.contains("辱骂清单"), "原改写 query 内容应保留: {merged}");
        // 顺序：父实体在前
        assert!(merged.starts_with("毛弟"), "父实体应前置: {merged}");
    }

    /// 改写 query 已包含父实体 → 兜底不动，避免重复。
    #[test]
    fn ensure_parent_entities_noop_when_entity_present() {
        let parent = "毛弟是否被人辱骂？";
        let rewritten = "毛弟 辱骂清单";
        assert_eq!(ensure_parent_entities(rewritten, parent), rewritten);
    }

    /// 父消息为空 / 与当前问句相同 → 兜底跳过。
    #[test]
    fn ensure_parent_entities_skips_empty_or_same_parent() {
        assert_eq!(ensure_parent_entities("abc", ""), "abc");
        assert_eq!(ensure_parent_entities("问句", "问句"), "问句");
    }

    /// 缺陷 3：追问泛词（未点名文件名）时，上一轮证据仍应被保留为保底依据。
    /// 追问"列出清单，把时间、辱骂人以及完整的辱骂内容列出"不含"聊天记录"等
    /// 文件名，但只要本轮检索 0 命中，历史证据必须带入，避免跨轮断层。
    #[test]
    fn carry_forward_source_keeps_history_when_unamed_or_no_hits() {
        // 追问未点名文件 + 本轮有其它命中 → 不强制保留（避免引入无关历史文件）
        assert!(!carry_forward_source(false, false));
        // 追问未点名文件 + 本轮 0 命中 → 必须保留历史证据
        assert!(carry_forward_source(false, true));
        // 追问点名了文件 → 无条件保留
        assert!(carry_forward_source(true, false));
        assert!(carry_forward_source(true, true));
    }

    /// 端到端回归：复现导出的"毛弟被辱骂"会话（首问"毛弟是否被人辱骂？" →
    /// 追问"列出清单，把时间、辱骂人以及完整的辱骂内容列出"），走真实
    /// `prepare_conversation_prompt` 管线，验证三处缺陷均已修复：
    /// 1) 改写后的 search_q 保留父问句实体"毛弟"（缺陷 1）
    /// 2) 逻辑范围"案件/和嘉案/聊天记录"解析为真实前缀"案件/HJ 和嘉 名誉权案/…"（缺陷 2）
    /// 3) 上轮引用证据随追问带入本轮 evidence（缺陷 3）
    #[tokio::test]
    async fn e2e_hejia_followup_retains_entity_scope_and_history() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        // 临时数据目录 + 基于文件的 SQLite DB
        let dir = std::env::temp_dir().join(format!("ls_ai_e2e_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("data.db");
        let db_str = db_path.to_str().unwrap().to_string();
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            crate::db::init_db(&conn).unwrap();
            drop(conn);
        }
        let pool = crate::db::get_pool(&db_str).unwrap();

        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(crate::indexer::IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool, im, indexer, scanner,
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            dir.clone(), index_dir, db_path, dummy_tx, None,
        );

        // 真实目录布局：scope 指向的 "案件/和嘉案/聊天记录" 是真实存在的目录
        // （125 个聊天记录截图/PDF）。范围外另有一个目录 "案件/HJ 和嘉 名誉权案"
        // 与 scope 目录并存（326 个文件，含 "和嘉" 子串与同名 "聊天记录" 文件），
        // 是字符级模糊解析误收泄漏的历史现场，必须被锚定前缀过滤拦在范围外。
        let c = state.db.get().unwrap();
        // scope 目录内文件（上一轮引用的证据）。文件名含检索实体词"毛弟"，
        // 确保经 path 通道确定性进入候选集后被 scoped 注入。
        let f1 = crate::db::tracker::upsert_file(&c, "案件/和嘉案/聊天记录/毛弟 辱骂 04聊天记录.pdf", "d1", 1700, 1000, Some("md5-04")).unwrap();
        let f2 = crate::db::tracker::upsert_file(&c, "案件/和嘉案/聊天记录/毛弟 06辱骂摘要.pdf", "d1", 1700, 1000, Some("md5-06")).unwrap();
        // 范围外干扰 1：HJ 和嘉 名誉权案目录与 scope 目录并存，路径同样含
        // "毛弟/辱骂"片段（会被 path 通道命中进入候选），文件名与内容同 scope
        // 内文件几乎一致——专门验证锚定前缀拦截而非内容相关度。
        let hj = crate::db::tracker::upsert_file(&c, "案件/HJ 和嘉 名誉权案/起诉文件/黄 诉 孙唐 辱骂/毛弟 04聊天记录.pdf", "d1", 1700, 1000, Some("md5-hj")).unwrap();
        // 范围外干扰 2：无关案件
        crate::db::tracker::upsert_file(&c, "案件/WC 万城/诉讼案件/股东资格/卷宗/001.pdf", "d1", 1700, 1000, Some("md5-wc")).unwrap();
        // 写入内容供注入（store_content 按 md5 存取，两处干扰项内容与 scope 内文件高度同词）
        crate::db::tracker::store_content(&c, "md5-04", "毛弟 唐晨骞 辱骂 缩货 老无赖 侬则缩货", false, None).unwrap();
        crate::db::tracker::store_content(&c, "md5-06", "孙孟燕 毛弟 辱骂 人模狗样 老逼样子 不要脸", false, None).unwrap();
        crate::db::tracker::store_content(&c, "md5-hj", "毛弟 唐晨骞 辱骂 缩货 老无赖 起诉状 名誉权", false, None).unwrap();
        crate::db::tracker::store_content(&c, "md5-wc", "万城 股东资格 庄建军 股权转让", false, None).unwrap();
        drop(c);

        // 会话历史：首问+答+追问（与导出完全一致）
        let messages = vec![
            ChatMessage { role: "user".into(), content: "毛弟是否被人辱骂？".into() },
            ChatMessage { role: "assistant".into(), content: "根据材料，毛弟（即原告黄树荪）确实被人辱骂。唐晨骞在微信群中多次辱骂毛弟。".into() },
            ChatMessage { role: "user".into(), content: "列出清单，把时间、辱骂人以及完整的辱骂内容列出".into() },
        ];
        // 上一轮引用的 2 份证据
        let source_ids = vec![f1.clone(), f2.clone()];
        let scope = TurnScope { mention_files: vec![], mention_dirs: vec![], inherit_from: vec![], conditions: vec![] };
        let session_scope = vec!["案件/和嘉案/聊天记录".to_string()];
        let strict = false;

        let prep = prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_scope, strict, false, true, None, "", None).await.unwrap();

        // 缺陷 1：改写 query 必须保留父问句实体"毛弟"
        assert!(prep.search_query.contains("毛弟"), "改写 query 必须含毛弟: {:?}", prep.search_query);
        // 缺陷 3：上一轮证据文件已注入为本轮材料（缺陷 1 补全的实体词使它们可被命中，
        // 或在本轮检索 0 命中时经 Layer 0.5 历史注入）。
        assert!(prep.evidence.iter().any(|e| e.file_id == f1), "上轮聊天记录证据必须带入: {:?}", prep.evidence.iter().map(|e| &e.path).collect::<Vec<_>>());
        assert!(prep.evidence.iter().any(|e| e.file_id == f2), "上轮辱骂摘要证据必须带入: {:?}", prep.evidence.iter().map(|e| &e.path).collect::<Vec<_>>());
        // 目录纪律：范围外目录（HJ 和嘉 名誉权案 / WC 万城）不得泄漏进 evidence——
        // 即使干扰项路径含检索词片段、内容与 scope 内文件高度同词。
        assert!(!prep.evidence.iter().any(|e| e.path.contains("HJ 和嘉")), "范围外 HJ 和嘉 名誉权案不得注入: {:?}", prep.evidence.iter().map(|e| &e.path).collect::<Vec<_>>());
        assert!(!prep.evidence.iter().any(|e| e.path.contains("万城")), "范围外万城文件不得注入 evidence: {:?}", prep.evidence.iter().map(|e| &e.path).collect::<Vec<_>>());
        assert!(
            prep.evidence.iter().all(|e| e.path.starts_with("案件/和嘉案/聊天记录")),
            "evidence 必须全部位于 scope 目录内: {:?}",
            prep.evidence.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        let _ = &hj; // hj 仅作为干扰项存在，evidence 必须不含它

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Layer 0.5 历史注入：当本轮检索 0 命中（追问与历史文件内容无词重叠）时，
    /// 上一轮引用的证据必须经历史通道注入，并带 from_history 标记。
    #[tokio::test]
    async fn e2e_history_injects_when_bm25_misses() {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex, RwLock};
        use crate::scanner::Scanner;
        use crate::search::IndexManager;
        use crate::state::{AppState, ScanDelta};

        let dir = std::env::temp_dir().join(format!("ls_ai_e2e_hist_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("data.db");
        let db_str = db_path.to_str().unwrap().to_string();
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            crate::db::init_db(&conn).unwrap();
            drop(conn);
        }
        let pool = crate::db::get_pool(&db_str).unwrap();
        let index_dir = dir.join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(crate::indexer::IndexerService::new(pool.clone(), im.clone()));
        let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
        let (dummy_tx, _) = std::sync::mpsc::channel();
        let state = AppState::new(
            pool, im, indexer, scanner,
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(ScanDelta::default())),
            dir.clone(), index_dir, db_path, dummy_tx, None,
        );

        // 历史证据文件：内容与追问（"整理一下摘要"）零词重叠 → BM25 0 命中
        let c = state.db.get().unwrap();
        let f1 = crate::db::tracker::upsert_file(&c, "案卷/和嘉/起诉状.pdf", "d1", 1700, 1000, Some("mh1")).unwrap();
        crate::db::tracker::store_content(&c, "mh1", "原告黄树荪 被告孙孟燕 被告唐晨骞 名誉权纠纷", false, None).unwrap();
        drop(c);

        let messages = vec![
            ChatMessage { role: "user".into(), content: "这个案子是谁起诉的？".into() },
            ChatMessage { role: "assistant".into(), content: "黄树荪起诉孙孟燕、唐晨骞，案由名誉权纠纷。".into() },
            ChatMessage { role: "user".into(), content: "帮我整理一下摘要".into() },
        ];
        let source_ids = vec![f1.clone()];
        let scope = TurnScope { mention_files: vec![], mention_dirs: vec![], inherit_from: vec![], conditions: vec![] };
        let session_scope = vec![];
        let strict = false;

        // skip_llm_rewrite=true 走规则改写（无 LLM 网关）→ 追问"帮我整理一下摘要"检索关键词为空，
        // 父实体经 rewrite 补入 → 但父文件内容无词重叠 → BM25 0 命中 → 历史注入兜底。
        let prep = prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_scope, strict, false, true, None, "", None).await.unwrap();
        assert!(
            prep.evidence.iter().any(|e| e.from_history && e.file_id == f1),
            "BM25 0 命中时历史证据必须经 Layer 0.5 注入且带 from_history: {:?}",
            prep.evidence.iter().map(|e| (&e.path, e.from_history)).collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod rrf_add_tests {
    use super::*;

    #[test]
    fn rrf_add_accumulates_across_channels() {
        let mut two: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        rrf_add(&mut two, "a", 0, 1.0);
        rrf_add(&mut two, "a", 0, 1.0);
        let mut one: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        rrf_add(&mut one, "b", 0, 1.0);
        let score_a = *two.get("a").unwrap();
        let score_b = *one.get("b").unwrap();
        assert!(
            (score_a - 2.0 * score_b).abs() < 1e-12,
            "two-channel rank-0 doc must be ~2x one-channel: a={score_a} b={score_b}"
        );
    }

    #[test]
    fn rrf_add_monotonically_decreasing_in_rank() {
        let mut acc: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        rrf_add(&mut acc, "r0", 0, 1.0);
        rrf_add(&mut acc, "r1", 1, 1.0);
        rrf_add(&mut acc, "r2", 2, 1.0);
        let s0 = *acc.get("r0").unwrap();
        let s1 = *acc.get("r1").unwrap();
        let s2 = *acc.get("r2").unwrap();
        assert!(s0 > s1, "rank 0 > rank 1: {s0} vs {s1}");
        assert!(s1 > s2, "rank 1 > rank 2: {s1} vs {s2}");
    }

    #[test]
    fn rrf_acc_missing_key_is_none_no_panic() {
        let acc: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        assert!(acc.get("nope").is_none());
        assert_eq!(acc.get("nope").copied().unwrap_or(0.0), 0.0);
    }
}

#[cfg(test)]
mod rerank_order_tests {
    use super::*;

    fn hit(id: &str) -> ScoredHit {
        ScoredHit {
            file_id: id.into(),
            path: String::new(),
            bm25_score: None,
            semantic_score: None,
            rrf_score: None,
            from_history: false,
            from_chunk: false,
            hit_chunks: Vec::new(),
        }
    }

    #[test]
    fn rerank_orders_prefix_by_score_desc_preserves_tail() {
        let original = vec![hit("A"), hit("B"), hit("C"), hit("D"), hit("E")];
        let rerank_idx = vec![0, 1, 2];
        let scores = vec![0.1, 0.9, 0.5];
        let perm = apply_rerank_order(&original, &rerank_idx, &scores, 1.0);
        assert_eq!(perm, vec![1, 2, 0, 3, 4]);
    }

    #[test]
    fn excluded_candidate_keeps_original_relative_position() {
        let original = vec![hit("A"), hit("B"), hit("C"), hit("D"), hit("E")];
        let rerank_idx = vec![0, 2];
        let scores = vec![0.9, 0.1];
        let perm = apply_rerank_order(&original, &rerank_idx, &scores, 1.0);
        assert_eq!(perm, vec![0, 2, 1, 3, 4]);
    }

    #[test]
    fn empty_inputs_yield_identity_permutation() {
        let original = vec![hit("A"), hit("B"), hit("C")];
        let perm = apply_rerank_order(&original, &[], &[], 1.0);
        assert_eq!(perm, vec![0, 1, 2]);
    }

    #[test]
    fn fusion_weight_protects_high_original_rank() {
        // 夹具对应"原本排第 1 的文档被重排打到最低分"（实测中的长文档误压）
        let original = vec![hit("A"), hit("B"), hit("C"), hit("D")];
        let rerank_idx = vec![0, 1, 2, 3];
        let scores = vec![0.1, 0.9, 0.8, 0.7];
        assert_eq!(
            apply_rerank_order(&original, &rerank_idx, &scores, 1.0),
            vec![1, 2, 3, 0],
            "w=1.0 纯重排，A 掉到末尾"
        );
        assert_eq!(
            apply_rerank_order(&original, &rerank_idx, &scores, 0.0),
            vec![0, 1, 2, 3],
            "w=0.0 纯原序，完全不动"
        );
        let fused = apply_rerank_order(&original, &rerank_idx, &scores, 0.5);
        assert_eq!(fused[0], 1, "got {fused:?}");
        assert_eq!(fused[1], 0, "w=0.5 融合后 A 应仅降一位，got {fused:?}");
    }
}
