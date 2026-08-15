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
}

/// Retrieval hit carrying its scores; semantic fields are `None` unless
/// the RRF fusion path ran.
struct ScoredHit {
    file_id: String,
    path: String,
    bm25_score: Option<f64>,
    semantic_score: Option<f64>,
    rrf_score: Option<f64>,
    /// Kept from earlier turns, not retrieved by the current query.
    from_history: bool,
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
}

/// BM25 retrieval + content assembly shared by one-shot and streaming
/// smart_search. Returns the prompt pair plus the source file lists.
struct PreparedSmart {
    system: String,
    user_msg: String,
    source_ids: Vec<String>,
    source_files: Vec<String>,
    evidence: Vec<EvidenceItem>,
}

fn prepare_smart_prompt(
    state: &tauri::State<'_, AppState>,
    query: &str,
) -> Result<PreparedSmart, String> {
    use crate::search::searcher::{SearchParams, SortField, SearcherWrap};

    let (context, source_ids, source_files, evidence) = {
        let mgr = state.index_manager.read().map_err(|e| format!("{e}"))?;
        let reader = mgr.reader().map_err(|e| format!("{e}"))?;
        let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
        drop(mgr);

        // P6 私密目录过滤：全库检索时排除 private 目录的文件。
        let public_dir_ids = {
            let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
            if crate::db::dir_config::has_private_dirs(&conn).unwrap_or(false) {
                Some(crate::db::dir_config::list_public_dir_ids(&conn).map_err(|e| format!("{e}"))?)
            } else {
                None
            }
        };

        let params = SearchParams {
            // NL questions become an exact PhraseQuery if parsed verbatim
            // (Tantivy default) — re-tokenise as explicit OR so any term hits.
            query: crate::search::schema::split_query_terms(&query.to_lowercase()),
            dir_ids: public_dir_ids, file_ids: None, ext_filter: None,
            date_from: None, date_to: None, path_prefixes: None,
            sort: SortField::Score, sort_order: "desc".to_string(),
            page: 1, page_size: 15, fuzzy: false, semantic: false,
        };
        let result = searcher.search(&params).map_err(|e| format!("{e}"))?;

        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let mut docs: Vec<String> = Vec::new();
        let mut sids: Vec<String> = Vec::new();
        let mut sf: Vec<String> = Vec::new();
        let mut ev: Vec<EvidenceItem> = Vec::new();
        // 首轮来源收敛为 top3：作为会话起点守卫，避免弱相关命中
        // （如“年度/报告”类泛词全库命中）从第一轮就把来源带偏。
        for hit in result.hits.iter().take(3) {
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id) {
                if let Some(md5) = &rec.md5 {
                    if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                        if !text.trim().is_empty() {
docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                sids.push(hit.file_id.clone());
                sf.push(rec.path.clone());
                ev.push(EvidenceItem {
                    file_id: hit.file_id.clone(),
                    path: rec.path.clone(),
                    snippet: truncate_text(&text, 200),
                    bm25_score: Some(hit.score),
                    semantic_score: None,
                    rrf_score: None,
                    rewritten: false,
                    rewritten_query: None,
                    from_history: false,
                });
                        }
                    }
                }
            }
        }
        drop(conn);
        (docs.join("\n\n---\n\n"), sids, sf, ev)
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
        evidence,
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
        evidence,
    });
    Ok(())
}

/// Outcome of a follow-up query rewrite: the query to actually retrieve
/// with (may equal the original when no rewrite applied).
struct RewriteOutcome {
    query: String,
}

/// Query rewrite for follow-up questions: when `last_q` starts with a
/// deictic pronoun (它/这个/那个/上述/该/那/此/刚才/上面/之前/前面) or is
/// too short to retrieve on, prepend keywords from the most recent
/// *previous* user message so BM25 sees the referents the pronoun points
/// back to. The LLM branch (see [`llm_rewrite_query`]) replaces pronouns
/// contextually when the gateway is available.
fn rewrite_query(last_q: &str, messages: &[ChatMessage]) -> RewriteOutcome {
    const DEICTIC: &[&str] =
        &["它", "这个", "那个", "上述", "上文", "该", "那", "此", "刚才", "上面", "之前", "前面"];
    let q = last_q.trim();
    let needs_rewrite = q.chars().count() < 4 || DEICTIC.iter().any(|p| q.starts_with(p));
    if !needs_rewrite {
        return RewriteOutcome { query: q.to_string() };
    }
    let parent = messages
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .map(|m| m.content.trim())
        .find(|c| !c.is_empty() && *c != q);
    let Some(parent) = parent else { return RewriteOutcome { query: q.to_string() } };
    let kws = parent_keywords(parent, 3);
    if kws.is_empty() {
        return RewriteOutcome { query: q.to_string() };
    }
    RewriteOutcome { query: format!("{} {}", kws.join(" "), q) }
}

/// Validate an LLM rewrite response: non-empty, not longer than the input
/// query's practical retrieval ceiling, and not echoing the original.
fn valid_rewrite_output(s: &str, original: &str) -> Option<String> {
    let t = s.trim().trim_matches(['"', '\'', '“', '”']);
    if t.is_empty() || t == original.trim() || t.chars().count() > 80 {
        return None;
    }
    Some(t.to_string())
}

/// Try an LLM query rewrite within a strict time budget. Returns `None`
/// (and the caller falls back to the rule-based rewrite) on any failure:
/// gateway disabled, timeout, empty/garbage output.
async fn llm_rewrite_query(
    last_q: &str,
    messages: &[ChatMessage],
) -> Option<String> {
    if !crate::ai::llm_enabled() {
        return None;
    }
    let mut history_str = String::from("对话历史：\n");
    for m in messages.iter().rev().take(6).rev() {
        history_str.push_str(&format!(
            "{}：{}\n",
            if m.role == "user" { "用户" } else { "助手" },
            truncate_text(&m.content, 120),
        ));
    }
    let system = "你是检索查询改写助手。用户在与本地文档对话，你的任务是把他的追问改写成一条可独立检索的中文查询：补全指代（它/这/那/刚才/上面等）与省略。要求：输出最小必要关键词短语，保留主题实体（具体报告名称/年份/主题词），去掉“报告/文件/呢/吗/的/了”等无区分词。只输出改写后的查询本身，不要解释、不要加引号、不要写“改写为”。如果问题本身就完整无需改写，原样输出。";
    let user = format!("{history_str}\n当前问题：{last_q}\n改写后的查询：");
    let sys = system.to_string();
    let fut = tokio::task::spawn_blocking(move || crate::ai::chat(&sys, &user));
    match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(Some(s))) => valid_rewrite_output(&s, last_q),
        _ => None,
    }
}

fn is_rewrite_stopword(w: &str) -> bool {
    matches!(w,
        "它" | "这个" | "那个" | "上述" | "上文" | "该" | "那" | "此"
        | "的" | "了" | "吗" | "呢" | "啊" | "什么" | "如何" | "怎么" | "怎样"
        | "请" | "一下" | "我" | "你" | "是" | "在" | "有" | "和" | "与" | "及"
        | "对" | "于" | "一个" | "会" | "能" | "让")
}

fn parent_keywords(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for t in crate::search::schema::JIEBA.cut(text, false) {
        let w = t.word.trim();
        if w.chars().count() < 2 || is_rewrite_stopword(w) {
            continue;
        }
        if !out.iter().any(|k: &String| k == w) {
            out.push(w.to_string());
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Reciprocal Rank Fusion: score = Σ 1/(k + rank) summed over two
/// pre-sorted lists (best-first). Lists must be ordered by score descending;
/// fusion uses position as rank (0‑based → 1/(k+0), 1/(k+1), …).
/// 生产路径已被 weighted mixing 替代；保留供测试验证 RRF 行为。
#[cfg(test)]
fn rrf_fuse(
    bm25_ranked: &[(String, f64)],
    semantic_ranked: &[(String, f32)],
    k: f64,
) -> Vec<(String, f64)> {
    let mut fusion = std::collections::HashMap::new();
    for (i, (fid, _)) in bm25_ranked.iter().enumerate() {
        *fusion.entry(fid.clone()).or_insert(0.0) += 1.0 / (k + i as f64);
    }
    for (i, (fid, _)) in semantic_ranked.iter().enumerate() {
        *fusion.entry(fid.clone()).or_insert(0.0) += 1.0 / (k + i as f64);
    }
    let mut ordered: Vec<(String, f64)> = fusion.into_iter().collect();
    ordered.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ordered
}

/// 加权混合排序：score = w×cosine + (1-w)×(bm25/max_bm25)。
/// BM25 归一化到 0~1 与 cosine 同尺度；返回按混合分降序的 (文件, 归一bm25, cosine, mix)。
pub fn weighted_mix(
    hits: Vec<(String, f64, f64)>,
    w: f64,
) -> Vec<(String, f64, f64, f64)> {
    let max_bm25 = hits.iter().map(|(_, b, _)| *b).fold(0.0_f64, f64::max);
    let norm = |raw: f64| if max_bm25 > 0.0 { raw / max_bm25 } else { 0.0 };
    let mut fused: Vec<(String, f64, f64, f64)> = hits
        .into_iter()
        .map(|(fid, b, c)| (fid.clone(), norm(b), c, w * c + (1.0 - w) * norm(b)))
        .collect();
    fused.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    fused
}

/// Semantic rerank of BM25 hits: embed the query, score the stored
/// embeddings of the BM25 candidates by cosine, fuse both orders via
/// weighted mixing (score = w×cosine + (1-w)×bm25_norm), and return the
/// fused hits with their scores. `None` when any step fails (embedding
/// gateway down, no stored vectors) — caller falls back to BM25.
fn semantic_fuse(
    conn: &rusqlite::Connection,
    query: &str,
    bm25_hits: &[ScoredHit],
    semantic_weight: f64,
) -> Option<Vec<ScoredHit>> {
    let q_vec = {
        let (tx, rx) = std::sync::mpsc::channel();
        let q = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(crate::ai::embed(&q));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .and_then(|r| r)
    }?;
    let rows = crate::db::tracker::get_all_embeddings(conn).ok()?;
    let bm25_ids: std::collections::HashSet<&str> =
        bm25_hits.iter().map(|h| h.file_id.as_str()).collect();
    let emb_map: std::collections::HashMap<&str, &Vec<f32>> = rows
        .iter()
        .filter(|(fid, _)| bm25_ids.contains(fid.as_str()))
        .map(|(fid, v)| (fid.as_str(), v))
        .collect();
    if emb_map.is_empty() {
        return None;
    }
    // 每份候选：原始 BM25 分 + cosine 相似度。
    let scored: Vec<(&ScoredHit, f64, f64)> = bm25_hits
        .iter()
        .filter_map(|h| {
            emb_map
                .get(h.file_id.as_str())
                .map(|v| (h, h.bm25_score.unwrap_or(0.0), crate::ai::cosine(&q_vec, v) as f64))
        })
        .collect();
    if scored.is_empty() {
        return None;
    }
    // BM25 分数归一化到 0~1（max 归一到 1）——与 cosine 同尺度才能加权。
    let pairs: Vec<(String, f64, f64)> = scored
        .iter()
        .map(|(h, b, c)| (h.file_id.clone(), *b, *c))
        .collect();
    let fused = weighted_mix(pairs, semantic_weight.clamp(0.0, 1.0));
    Some(
        fused
            .iter()
            .filter_map(|(fid, _, c, mix)| {
                scored
                    .iter()
                    .find(|(h, _, _)| h.file_id == *fid)
                    .map(|(h, b, _)| ScoredHit {
                        file_id: h.file_id.clone(),
                        path: h.path.clone(),
                        bm25_score: Some(*b),
                        semantic_score: Some(*c),
                        rrf_score: Some(*mix),
                        from_history: false,
                    })
            })
            .collect(),
    )
}

/// BM25 retrieval returning top relevant hits — tokenised as explicit OR
/// (a raw question would parse as an exact phrase and miss). When
/// `semantic` is true and the embedding gateway is configured, reranks the
/// BM25 candidates via RRF fusion with embedding cosine scores (gracefully
/// falls back to BM25-only on any failure).
fn bm25_relevant_hits(
    state: &tauri::State<'_, AppState>,
    query: &str,
    limit: usize,
    semantic: bool,
    dir_ids: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    date_from: Option<i64>,
    date_to: Option<i64>,
    path_prefixes: Option<Vec<String>>,
    file_ids: Option<Vec<String>>,
) -> Result<Vec<ScoredHit>, String> {
    use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
    let mgr = state.index_manager.read().map_err(|e| format!("{e}"))?;
    let reader = mgr.reader().map_err(|e| format!("{e}"))?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);

    // When semantic fusion is active we fetch more candidates for the RRF pool.
    let fetch = if semantic && crate::ai::embedding_enabled() {
        limit.max(100)
    } else {
        limit
    };
    // P6 私密目录过滤：全库检索（dir_ids 为空）时排除 private 目录的文件。
    let dir_ids = if dir_ids.as_ref().is_none_or(|v| v.is_empty()) {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        if crate::db::dir_config::has_private_dirs(&conn).unwrap_or(false) {
            Some(crate::db::dir_config::list_public_dir_ids(&conn).map_err(|e| format!("{e}"))?)
        } else {
            None
        }
    } else {
        dir_ids
    };
    let params = SearchParams {
        query: crate::search::schema::split_query_terms(&query.to_lowercase()),
        dir_ids: dir_ids.clone(), file_ids: file_ids.clone(), ext_filter,
        date_from, date_to, path_prefixes: path_prefixes.clone(),
        sort: SortField::Score, sort_order: "desc".to_string(),
        page: 1, page_size: fetch, fuzzy: false, semantic: false,
    };
    log::info!("[AI] bm25_relevant_hits: q={query} dir_ids={:?} path_prefixes={:?} file_ids={:?}", dir_ids, path_prefixes, file_ids);
    let result = searcher.search(&params).map_err(|e| format!("{e}"))?;

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut bm25_hits: Vec<ScoredHit> = Vec::new();
    for hit in result.hits {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id) {
            if rec.status == "active" && rec.md5.is_some() {
                bm25_hits.push(ScoredHit {
                    file_id: hit.file_id,
                    path: rec.path,
                    bm25_score: Some(hit.score),
                    semantic_score: None,
                    rrf_score: None,
                    from_history: false,
                });
            }
        }
    }

    if semantic && crate::ai::embedding_enabled() && !bm25_hits.is_empty() {
        let weight = crate::config::load_config().semantic_weight.clamp(0.0, 1.0);
        if let Some(fused) = semantic_fuse(&conn, query, &bm25_hits, weight) {
            drop(conn);
            return Ok(fused.into_iter().take(limit).collect());
        }
    }
    drop(conn);
    Ok(bm25_hits.into_iter().take(limit).collect())
}

/// Assembled conversation prompt + the (possibly updated) source file list
/// backing it. Follow-up questions re-retrieve relevant documents so the
/// answer (and the frontend source list) reflects the newest question.
struct PreparedConversation {
    system: String,
    user_msg: String,
    source_ids: Vec<String>,
    source_files: Vec<String>,
    evidence: Vec<EvidenceItem>,
}

async fn prepare_conversation_prompt(
    state: &tauri::State<'_, AppState>,
    messages: &[ChatMessage],
    source_ids: &[String],
    scope: &TurnScope,
    session_scope_dir_ids: &[String],
    strict_docs: bool,
) -> Result<PreparedConversation, String> {
    let last_q = messages.last().map(|m| m.content.clone()).unwrap_or_default();
    log::info!(
        "[AI] prepare_conversation_prompt: q={} mention_files={:?} mention_dirs={:?} conditions={:?} session_scope_dir_ids={:?} strict_docs={}",
        last_q, scope.mention_files, scope.mention_dirs, scope.conditions, session_scope_dir_ids, strict_docs
    );
    // 追问改写：规则改写兜底 + LLM 改写增强（超时/失败自动降级回规则）。
    let rule = rewrite_query(&last_q, messages);
    let search_q = match llm_rewrite_query(&last_q, messages).await {
        Some(llm) if llm != rule.query => llm,
        _ => rule.query,
    };
    let rewritten = search_q != last_q.trim();
    log::info!("[AI] rewrite: original={last_q} search_q={search_q} rewritten={rewritten}");

    // 从 scope 提取检索过滤参数
    let mut dir_ids: Vec<String> = Vec::new();
    dir_ids.extend(session_scope_dir_ids.iter().cloned());
    // 解析 @目录：绝对监控根 → dir_ids；相对路径子目录 → path_prefixes
    let mut path_prefixes: Vec<String> = Vec::new();
    {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        for dir_path in &scope.mention_dirs {
            let p = dir_path.trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            // 先试监控根精确匹配（绝对路径或别名）
            if let Ok(mut stmt) = conn.prepare("SELECT id FROM dir_config WHERE path = ?1 OR alias = ?1") {
                if let Ok(r) = stmt.query_row(rusqlite::params![p], |row| row.get::<_, String>(0)) {
                    dir_ids.push(r);
                    continue;
                }
            }
            // 否则按相对路径前缀过滤（子目录/文件夹）
            path_prefixes.push(p.to_string());
        }
        drop(conn);
    }

    // 提取条件过滤
    let mut ext_filter: Option<Vec<String>> = None;
    let mut date_from: Option<i64> = None;
    let mut date_to: Option<i64> = None;
    for c in &scope.conditions {
        match c.kind.as_str() {
            "ext" => ext_filter = Some(c.value.split(',').map(|s| s.trim().to_lowercase()).collect()),
            "date" => {
                let parts: Vec<&str> = c.value.splitn(2, '~').collect();
                if parts.len() == 2 {
                    let _ = chrono::NaiveDate::parse_from_str(parts[0], "%Y-%m-%d").ok()
                        .map(|d| date_from = Some(d.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp_micros()).unwrap_or(0)));
                    let _ = chrono::NaiveDate::parse_from_str(parts[1], "%Y-%m-%d").ok()
                        .map(|d| date_to = Some(d.and_hms_opt(23, 59, 59).map(|dt| dt.and_utc().timestamp_micros()).unwrap_or(0)));
                }
            }
            _ => {}
        }
    }
    let dir_ids_opt = if dir_ids.is_empty() { None } else { Some(dir_ids) };
    let path_prefixes_opt = if path_prefixes.is_empty() { None } else { Some(path_prefixes) };

    // 解析 @mention 文件路径 → file_ids，传给搜索限定范围
    let mention_file_ids: Option<Vec<String>> = if scope.mention_files.is_empty() {
        None
    } else {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let ids: Vec<String> = scope.mention_files.iter().filter_map(|path| {
            // 先精确匹配
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_path(&conn, path) {
                return Some(rec.id);
            }
            // 回退：LIKE 匹配（用户可能只输入了文件名，不含目录前缀）
            if let Ok(mut ids) = crate::db::tracker::search_file_ids_by_path_fragment(&conn, path, 1) {
                return ids.pop();
            }
            None
        }).collect();
        drop(conn);
        if ids.is_empty() { None } else { Some(ids) }
    };

    log::info!(
        "[AI] scope resolved: dir_ids={:?} path_prefixes={:?} file_ids={:?} ext={:?} date={:?}~{:?}",
        dir_ids_opt, path_prefixes_opt, mention_file_ids, ext_filter, date_from, date_to
    );

    // 动态依据：保留仍有效的旧来源，并按追问问题检索命中补齐（去重, ≤15）。
    // 语义开启时对追问做 BM25+embedding RRF 融合重排。
    let new_hits = if last_q.trim().is_empty() {
        Vec::new()
    } else {
        bm25_relevant_hits(
            state, &search_q, 10, crate::ai::embedding_enabled(),
            dir_ids_opt, ext_filter, date_from, date_to, path_prefixes_opt, mention_file_ids,
        )?
    };
    log::info!("[AI] retrieved hits: {} new_hits (from limited scope)", new_hits.len());

    const MAX_SOURCES: usize = 15;
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut merged: Vec<ScoredHit> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 新检索优先：追问要主导依据更新（否则旧来源累积顶满上限,
    // 新文档永远挤不进来, 退化为"只围绕第一轮文档回答"）。
    for hit in new_hits {
        if seen.insert(hit.file_id.clone()) {
            merged.push(hit);
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
    // 旧来源只保留*对话中明确提到过的*（用户/助手引用过 = 实际在使用），
    // 硬上限 3 份——防止陈旧来源把 15 槽位挤满、稀释本轮检索命中。
    const MAX_OLD_SOURCES: usize = 3;
    let mut old_count = 0usize;
    for fid in source_ids.iter().rev() {
        if merged.len() >= MAX_SOURCES || old_count >= MAX_OLD_SOURCES {
            break;
        }
        if seen.contains(fid) {
            continue;
        }
        let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) else { continue };
        if rec.status != "active" || rec.md5.is_none() {
            continue;
        }
        if !mentioned(&rec.path) {
            continue;
        }
        seen.insert(fid.clone());
        merged.push(ScoredHit {
            file_id: fid.clone(),
            path: rec.path.clone(),
            bm25_score: None,
            semantic_score: None,
            rrf_score: None,
            from_history: true,
        });
        old_count += 1;
    }

    let mut docs: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();
    // @mention 文件直用：按路径解析为 [N] 编号引用，优先于检索命中。
    let mut mention_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, path) in scope.mention_files.iter().enumerate() {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_path(&conn, path) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!("[{}]（{path}）\n{}", i + 1, truncate_text(&text, 2000)));
                        evidence.push(EvidenceItem {
                            file_id: rec.id.clone(),
                            path: path.clone(),
                            snippet: truncate_text(&text, 200),
                            bm25_score: None,
                            semantic_score: None,
                            rrf_score: None,
                            rewritten,
                            rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                            from_history: false,
                        });
                        mention_index.insert(path.clone(), i + 1);
                    }
                }
            }
        }
    }
    for hit in &merged {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                        evidence.push(EvidenceItem {
                            file_id: hit.file_id.clone(),
                            path: rec.path.clone(),
                            snippet: truncate_text(&text, 200),
                            bm25_score: hit.bm25_score,
                            semantic_score: hit.semantic_score,
                            rrf_score: hit.rrf_score,
                            rewritten,
                            rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                            from_history: hit.from_history,
                        });
                    }
                }
            }
        }
    }
    drop(conn);

    let source_ids_final = merged.iter().map(|h| h.file_id.clone()).collect();
    let source_files_final = merged.iter().map(|h| h.path.clone()).collect();

    let context = truncate_text(&docs.join("\n\n---\n\n"), 24000);
    // 严格模式（仅依据文档）：范围内无命中时明确拒绝，而非让 LLM 自由发挥。
    if strict_docs && context.trim().is_empty() {
        return Err("未在与当前范围匹配的文档中找到依据".into());
    }
    let system = format!("你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。如果材料不足以回答，请明确说明。\n\n材料：\n{}", context);
    let last_n = messages.len().saturating_sub(1);
    let mut user_msg = if messages.len() > 1 {
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
    // 将 @mention 替换为 [N] 编号引用（路径字符串不进 LLM）。
    for (path, idx) in &mention_index {
        user_msg = user_msg.replace(&format!("@{path}"), &format!("[{}]", idx));
    }
    Ok(PreparedConversation { system, user_msg, source_ids: source_ids_final, source_files: source_files_final, evidence })
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
    session_scope_dir_ids: Vec<String>,
    strict_docs: bool,
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

    let PreparedConversation { system, user_msg, .. } =
        prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_scope_dir_ids, strict_docs).await?;
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
    scope: TurnScope,
    session_scope_dir_ids: Vec<String>,
    strict_docs: bool,
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
        "[AI] conversation_ask_stream: messages={} source_ids={}",
        messages.len(),
        source_ids.len()
    );
    crate::ai::reset_ai_cancel();

    log::info!("[AI] conversation_ask_stream: scope={:?} session_scope_dir_ids={:?}", scope, session_scope_dir_ids);

    let PreparedConversation { system, user_msg, source_ids, source_files, evidence, .. } =
        prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_scope_dir_ids, strict_docs).await?;
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
        evidence,
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

/// One turn's source file references, recorded when a conversation turn
/// completes so the session history can show which documents backed each
/// user/assistant exchange. `items` carries the traceable evidence
/// (scores, rewrite info) for that turn.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PerTurnEvidence {
    pub turn_index: usize,
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub items: Vec<EvidenceItem>,
}

/// 每轮最终的 @mention 生效集合（含继承解析后），持久化供 `@第N轮` 引用。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PerTurnScope {
    pub turn_index: usize,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub dirs: Vec<String>,
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
    /// 每轮 @mention 生效集合，供 `@第N轮` 继承解析。
    #[serde(default)]
    pub per_turn_scopes: Vec<PerTurnScope>,
    /// 会话级目录范围（从树状控件/右键加入，持续到替换/清空）。
    #[serde(default)]
    pub scope_dir_ids: Vec<String>,
    /// 会话级结构化条件（ext/date/模糊）。
    #[serde(default)]
    pub scope_conditions: Vec<ScopeCondition>,
    /// P2 严格模式：范围内无命中时拒绝回答（会话级，可切换）。
    #[serde(default)]
    pub strict_docs: bool,
    /// P3 专注模式：仅分析此文件（会话级，临时屏蔽其他范围）。
    #[serde(default)]
    pub focus_file: Option<String>,
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
                    per_turn_evidence: vec![],
                    per_turn_scopes: vec![],
                    scope_dir_ids: vec![],
                    scope_conditions: vec![],
                    strict_docs: false,
                    focus_file: None,
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
    create_chat_session_impl(&state.data_dir)
}

fn create_chat_session_impl(data_dir: &std::path::Path) -> Result<String, String> {
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
        scope_dir_ids: vec![],
        scope_conditions: vec![],
        strict_docs: false,
        focus_file: None,
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
    save_chat_session_impl(&state.data_dir, session)
}

fn save_chat_session_impl(data_dir: &std::path::Path, session: ChatSession) -> Result<(), String> {
    let mut h = read_history(data_dir);
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
    let mut out = format!("{index}. 📄 `{}` {score_str}", e.path);
    if e.rewritten {
        if let Some(q) = &e.rewritten_query {
            out.push_str(&format!("\n    ↳ 查询改写: `{q}`"));
        }
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
#[tauri::command]
pub fn export_chat_session(state: State<'_, AppState>, id: String) -> Result<String, String> {
    export_chat_session_impl(&state.data_dir, &id)
}

fn export_chat_session_impl(data_dir: &std::path::Path, id: &str) -> Result<String, String> {
    let h = read_history(data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "会话不存在".to_string())?;

    let mut md = String::new();
    let now = now_ts();
    md.push_str(&format!("# {}\n\n", session.title));
    md.push_str(&format!("> 会话 ID: `{}`\n", session.id));
    md.push_str(&format!("> 创建时间: {}\n", session.created_at));
    md.push_str(&format!("> 导出时间: {}\n\n", now));

    if session.strict_docs {
        md.push_str("> ⚙️ 严格模式（仅依据文档）：开启\n");
    }
    if let Some(f) = &session.focus_file {
        md.push_str(&format!("> 📌 专注模式: `{}`\n", f));
    }
    if !session.scope_dir_ids.is_empty() {
        md.push_str(&format!("> 📁 会话范围目录: `{:?}`\n", session.scope_dir_ids));
    }
    // 按轮索引分组：user 消息和它的 evidence/scope
    let mut turn_idx = 0usize;
    let mut user_msg_iter = session.messages.iter().filter(|m| m.role == "user");
    for (i, m) in session.messages.iter().enumerate() {
        if m.role == "user" {
            turn_idx += 1;
            md.push_str(&format!("---\n\n## 问 (第 {turn_idx} 轮)\n\n{}\n", m.content));
            // 本轮引用
            let scopes = session.per_turn_scopes.iter().find(|s| s.turn_index == turn_idx - 1);
            if let Some(sc) = scopes {
                if !sc.files.is_empty() || !sc.dirs.is_empty() {
                    md.push_str("\n**引用:**\n");
                    for f in &sc.files {
                        md.push_str(&format!("- 📄 `{}`\n", f));
                    }
                    for d in &sc.dirs {
                        md.push_str(&format!("- 📁 `{}`\n", d));
                    }
                }
            }
            // 找下一条 assistant 消息
            if let Some(assistant_msg) = session.messages.get(i + 1).filter(|m| m.role == "assistant") {
                md.push_str(&format!("\n## 答\n\n{}\n", assistant_msg.content));
                // 本轮检索依据
                let evidence = session.per_turn_evidence.iter().find(|e| e.turn_index == turn_idx - 1);
                if let Some(ev) = evidence {
                    if !ev.items.is_empty() {
                        md.push_str(&format!("\n**检索依据（{}）:**\n", ev.items.len()));
                        for (j, item) in ev.items.iter().enumerate() {
                            md.push_str(&format!("{}\n", fmt_evidence_item(item, j + 1)));
                        }
                    }
                }
            }
        }
    }
    md.push_str("\n---\n");
    md.push_str(&format!("\n_导出时间: {}\n", now));
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
            scope_dir_ids: vec![],
            scope_conditions: vec![],
            strict_docs: false,
            focus_file: None,
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
                }],
            }],
            per_turn_scopes: vec![PerTurnScope {
                turn_index: 0,
                files: vec!["a.pdf".into()],
                dirs: vec![],
            }],
            scope_dir_ids: vec!["dir1".into()],
            scope_conditions: vec![],
            strict_docs: true,
            focus_file: Some("a.pdf".into()),
        };
        save_chat_session_impl(&dir, session).unwrap();
        let md = export_chat_session_impl(&dir, "s1").unwrap();
        assert!(md.contains("# 克虏伯项目"));
        assert!(md.contains("## 问 (第 1 轮)"), "第一轮问题应导出: {md}");
        assert!(md.contains("项目背景"));
        assert!(md.contains("## 答"));
        assert!(md.contains("2015年启动"));
        assert!(md.contains("第 2 轮"), "第二轮问题应导出");
        assert!(md.contains("检索依据（1）"));
        assert!(md.contains("a.pdf"));
        assert!(md.contains("查询改写"));
        assert!(md.contains("严格模式"));
        assert!(md.contains("专注模式"));

        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: content.into() }
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
        assert!(valid_rewrite_output("x", "原问题").is_some());
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
            scope_dir_ids: vec![],
            scope_conditions: vec![],
            strict_docs: false,
            focus_file: None,
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
                }],
            }],
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: ChatSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back.per_turn_evidence[0].items.len(), 1);
        assert_eq!(back.per_turn_evidence[0].items[0].rewritten_query.as_deref(), Some("q"));
    }
}
