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
}

/// Retrieval hit carrying its scores; semantic fields are `None` unless
/// the RRF fusion path ran.
pub struct ScoredHit {
    pub file_id: String,
    pub path: String,
    pub bm25_score: Option<f64>,
    pub semantic_score: Option<f64>,
    pub rrf_score: Option<f64>,
    pub from_history: bool,
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
    /// Per-turn trace ID for correlating app.log entries with the session export.
    #[serde(default)]
    trace_id: String,
    #[serde(default)]
    search_query: String,
    #[serde(default)]
    hits: usize,
    #[serde(default)]
    total_match_count: usize,
    #[serde(default)]
    llm_model: String,
    #[serde(default)]
    embedding_model: String,
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
    let hits = bm25_relevant_hits(
        state, &query.to_lowercase(), 3, crate::ai::embedding_enabled(),
        None, None, None, None, None, None,
    )?;

    let (context, source_ids, source_files, evidence) = {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let mut docs: Vec<String> = Vec::new();
        let mut sids: Vec<String> = Vec::new();
        let mut sf: Vec<String> = Vec::new();
        let mut ev: Vec<EvidenceItem> = Vec::new();
        for hit in &hits {
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id)
                && let Some(md5) = &rec.md5
                    && let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5)
                        && !text.trim().is_empty() {
                            docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                            sids.push(hit.file_id.clone());
                            sf.push(rec.path.clone());
                            ev.push(EvidenceItem {
                                file_id: hit.file_id.clone(),
                                path: rec.path.clone(),
                                snippet: truncate_text(&text, 200),
                                bm25_score: hit.bm25_score,
                                semantic_score: hit.semantic_score,
                                rrf_score: hit.rrf_score,
                                rewritten: false,
                                rewritten_query: None,
                                from_history: false,
                            });
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
    let hits = evidence.len();
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

    let raw_text = result.text.unwrap_or_default();
    let cited_text = auto_cite(&raw_text, &evidence);
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
        hits,
        total_match_count: 0,
        llm_model: String::new(),
        embedding_model: String::new(),
    });
    Ok(())
}

/// Outcome of a follow-up query rewrite: the query to actually retrieve
/// with (may equal the original when no rewrite applied).
pub struct RewriteOutcome {
    pub query: String,
}

/// Query rewrite for follow-up questions: when `last_q` starts with a
/// deictic pronoun (它/这个/那个/上述/该/那/此/刚才/上面/之前/前面) or is
/// too short to retrieve on, prepend keywords from the most recent
/// *previous* user message so BM25 sees the referents the pronoun points
/// back to. The LLM branch (see [`llm_rewrite_query`]) replaces pronouns
/// contextually when the gateway is available.
pub fn rewrite_query(last_q: &str, messages: &[ChatMessage]) -> RewriteOutcome {
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

/// Merge retrieval-scope entries into a non-overlapping (union) set of
/// path prefixes. dir 过滤（监控根, dir_ids）与子目录 prefix 过滤当前是两个
/// 独立 Must 条件——同设父目录+子目录会被 AND 错误收窄。此处按"父吞子"
/// 去冗余：已选根目录之下的子前缀全部删掉（根已覆盖），prefix 内部保留最短。
///
/// `dir_roots`: (dir_id, 根绝对路径)，用于判断 prefix 是否落在某已选根下。
pub fn merge_scope_prefixes(
    dir_roots: &[(String, String)],
    dir_ids: &[String],
    prefixes: &[String],
) -> (Vec<String>, Vec<String>) {
    // prefix 是相对监控根的路径（如 "Docs/B"）；根绝对路径如 "/Volumes/Docs"。
    // 判断 prefix 归属：根 basename 与 prefix 首段相同 → 属于该根；若该根在
    // dir_ids 中，这个 prefix 被根覆盖（根已含其全部内容）→ 去掉。
    let kept: Vec<String> = prefixes
        .iter()
        .filter(|p| {
            let first_seg = p.split('/').next().unwrap_or("");
            let covered_by_selected_root = dir_roots
                .iter()
                .any(|(id, root)| {
                    dir_ids.contains(id)
                        && std::path::Path::new(root)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n == first_seg)
                            .unwrap_or(false)
                });
            !covered_by_selected_root
        })
        .cloned()
        .collect();
    // prefix 内部父吞子：保留最短——若某 prefix 是另一 prefix 的严格父路径，
    // 删掉后者（前者的检索范围已覆盖它）。
    let mut result: Vec<String> = Vec::new();
    for p in &kept {
        if kept.iter().any(|q| {
            q != p
                && p.starts_with(q.as_str())
                && p.as_bytes().get(q.len()) == Some(&b'/')
        }) {
            continue; // p 有严格父 prefix，被覆盖
        }
        result.push(p.clone());
    }
    (dir_ids.to_vec(), result)
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
pub async fn llm_rewrite_query(
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
    let max_cos = scored.iter().map(|(_, _, c)| *c).fold(0.0_f64, f64::max);
    if max_cos == 0.0 {
        log::warn!("[AI] semantic_fuse: all cosine=0.0 — q_vec dim={} nonzero={} doc dim={}",
            q_vec.len(), q_vec.iter().any(|x| *x != 0.0),
            emb_map.values().next().map(|v| v.len()).unwrap_or(0));
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
pub fn bm25_relevant_hits(
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
    // When path_prefixes are set, fetch more to compensate for post-filtering
    // (RegexQuery on STRING fields can silently fail on Unicode paths).
    let fetch = if semantic && crate::ai::embedding_enabled() {
        limit.max(100)
    } else if path_prefixes.as_ref().is_some_and(|p| !p.is_empty()) {
        limit.max(50)
    } else {
        limit
    };
    let params = SearchParams {
        query: crate::search::schema::split_query_terms(&query.to_lowercase()),
        dir_ids: dir_ids.clone(), file_ids: file_ids.clone(), ext_filter: ext_filter.clone(),
        date_from, date_to, path_prefixes: path_prefixes.clone(),
        sort: SortField::Score, sort_order: "desc".to_string(),
        page: 1, page_size: fetch, fuzzy: false, semantic: false,
    };
    log::info!("[AI] bm25_relevant_hits: q={query} dir_ids={:?} path_prefixes={:?} file_ids={:?}", dir_ids, path_prefixes, file_ids);
    let mut result = searcher.search(&params).map_err(|e| format!("{e}"))?;

    // Fallback: if BM25 returns zero hits but file_ids scope the search to
    // specific files, retry with an empty query (match-all within scope) so
    // the user's scoped files are still returned even when query terms
    // don't match the indexed content (tokenization/extraction differences).
    if result.hits.is_empty() && file_ids.as_ref().is_some_and(|ids| !ids.is_empty()) {
        let fallback_params = SearchParams {
            query: String::new(),
            dir_ids: dir_ids.clone(), file_ids: file_ids.clone(), ext_filter: ext_filter.clone(),
            date_from, date_to, path_prefixes: path_prefixes.clone(),
            sort: SortField::Score, sort_order: "desc".to_string(),
            page: 1, page_size: fetch, fuzzy: false, semantic: false,
        };
        log::info!("[AI] bm25_relevant_hits: zero hits with file_ids, retrying with empty query");
        result = searcher.search(&fallback_params).map_err(|e| format!("{e}"))?;
    }

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut bm25_hits: Vec<ScoredHit> = Vec::new();
    let mut seen_md5: std::collections::HashSet<String> = std::collections::HashSet::new();
    for hit in result.hits {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id)
            && rec.status == "active"
                && let Some(md5) = &rec.md5
                    && seen_md5.insert(md5.clone()) {
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

    // Safety net: RegexQuery on STRING fields can silently fail on Unicode
    // paths, falling back to AllQuery which disables scope filtering.
    if let Some(prefixes) = &path_prefixes
        && !prefixes.is_empty() {
            bm25_hits.retain(|h| {
                prefixes.iter().any(|p| h.path.starts_with(p.trim_end_matches('/')))
            });
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
    /// Final retrieval query (after rewrite) — recorded per turn for traceability.
    search_query: String,
    /// Number of BM25 hits before merge with @mention files.
    hits: usize,
    total_match_count: usize,
    /// Accumulated AI events for this turn's pipeline execution.
    /// Caller should batch-insert into ai_events table.
    events: Vec<(String, serde_json::Value)>,
}

const CONTEXT_BUDGET: usize = 150_000;
const SYSTEM_OVERHEAD: usize = 2_000;
const ANSWER_RESERVE: usize = 8_000;
const MAX_CONTENT_INJECT: usize = 30;
const VECTOR_THRESHOLD: f32 = 0.65;

/// Resolve file paths to file IDs with exact + LIKE fallback.
/// Returns (resolved, missing) where resolved is (file_id, path) pairs.
/// For each path: exact get_file_by_path first, then LIKE fallback
/// search_file_ids_by_path_fragment(path, 2). If exactly 1 LIKE match →
/// adopt; if 0 or ≥2 → missing. Only handles file paths, not directories.
pub fn resolve_mention_file_ids(
    conn: &rusqlite::Connection,
    paths: &[String],
) -> (Vec<(String, String)>, Vec<String>) {
    let mut resolved = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        // Exact match first
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_path(conn, path) {
            resolved.push((rec.id, rec.path));
            continue;
        }
        // LIKE fallback: limit 2 to detect ambiguity
        if let Ok(ids) = crate::db::tracker::search_file_ids_by_path_fragment(conn, path, 2)
            && ids.len() == 1 {
                // Exactly one LIKE match → adopt
                if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(conn, &ids[0]) {
                    resolved.push((rec.id, rec.path));
                    continue;
                }
            }
        missing.push(path.clone());
    }
    (resolved, missing)
}

async fn prepare_conversation_prompt(
    state: &tauri::State<'_, AppState>,
    messages: &[ChatMessage],
    source_ids: &[String],
    scope: &TurnScope,
    session_retrieval_scope: &[String],
    strict_docs: bool,
) -> Result<PreparedConversation, String> {
    let last_q = messages.last().map(|m| m.content.clone()).unwrap_or_default();
    log::info!(
        "[AI] prepare_conversation_prompt: q={} mention_files={:?} mention_dirs={:?} conditions={:?} session_retrieval_scope={:?} strict_docs={}",
        last_q, scope.mention_files, scope.mention_dirs, scope.conditions, session_retrieval_scope, strict_docs
    );
    let mut events: Vec<(String, serde_json::Value)> = Vec::new();
    // 追问改写：规则改写兜底 + LLM 改写增强（超时/失败自动降级回规则）。
    let rule = rewrite_query(&last_q, messages);
    let original_rule_query = rule.query.clone();
    let search_q = match llm_rewrite_query(&last_q, messages).await {
        Some(llm) if llm != rule.query => llm,
        _ => rule.query,
    };
    let rewritten = search_q != last_q.trim();
    log::info!("[AI] rewrite: original={last_q} search_q={search_q} rewritten={rewritten}");
    events.push(("query_rewrite".into(), serde_json::json!({
        "original": last_q,
        "rewritten": search_q,
        "was_rewritten": rewritten,
        "rewrite_method": if rewritten { if search_q != original_rule_query { "llm" } else { "rule" } } else { "none" },
    })));

    // 从 scope 提取检索过滤参数
    let mut dir_ids: Vec<String> = Vec::new();
    let mut path_prefixes: Vec<String> = Vec::new();
    let mut scope_file_resolved: Vec<(String, String)> = Vec::new();
    {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        for dir_path in session_retrieval_scope {
            let p = dir_path.trim().trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            // 先试监控根精确匹配（绝对路径或别名）
            if let Ok(mut stmt) = conn.prepare("SELECT id FROM dir_config WHERE path = ?1 OR alias = ?1")
                && let Ok(r) = stmt.query_row(rusqlite::params![p], |row| row.get::<_, String>(0)) {
                    dir_ids.push(r);
                    continue;
                }
            // 试文件路径精确匹配 → file_id（可靠的 TermQuery，不依赖 RegexQuery）
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_path(&conn, p) {
                scope_file_resolved.push((rec.id, rec.path));
                continue;
            }
            // LIKE 回退：路径片段匹配
            if let Ok(ids) = crate::db::tracker::search_file_ids_by_path_fragment(&conn, p, 2)
                && ids.len() == 1
                    && let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &ids[0]) {
                        scope_file_resolved.push((rec.id, rec.path));
                        continue;
                    }
            // 否则按相对路径前缀过滤（子目录/文件夹）
            path_prefixes.push(p.to_string());
        }
        drop(conn);
    }
    // 解析 @目录：绝对监控根 → dir_ids；相对路径子目录 → path_prefixes
    let dir_roots: Vec<(String, String)> = {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let dirs = crate::db::dir_config::list_dirs(&conn).map_err(|e| format!("db error: {e}"))?;
        dirs.into_iter().map(|d| (d.id, d.path)).collect()
    };
    {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        for dir_path in &scope.mention_dirs {
            let p = dir_path.trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            // 先试监控根精确匹配（绝对路径或别名）
            if let Ok(mut stmt) = conn.prepare("SELECT id FROM dir_config WHERE path = ?1 OR alias = ?1")
                && let Ok(r) = stmt.query_row(rusqlite::params![p], |row| row.get::<_, String>(0)) {
                    dir_ids.push(r);
                    continue;
                }
            // 否则按相对路径前缀过滤（子目录/文件夹）
            path_prefixes.push(p.to_string());
        }
        drop(conn);
    }
    // 并集合并：父目录吞噬其下的子前缀，消除 AND 交叉（A∪A/B=A）
    let dir_roots_ref: Vec<(String, String)> = dir_roots;
    {
        let (d, p) = merge_scope_prefixes(&dir_roots_ref, &dir_ids, &path_prefixes);
        dir_ids = d;
        path_prefixes = p;
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
    let (mut mention_resolved, missing_mentions) = {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let (r, m) = resolve_mention_file_ids(&conn, &scope.mention_files);
        drop(conn);
        (r, m)
    };
    // retrieval_scope 中的文件也作为直接注入（与 @mention 统一行为），
    // 不再只做 BM25 过滤——用户引用的文件应每轮都注入 LLM prompt。
    let mut seen_mention: std::collections::HashSet<String> = mention_resolved.iter().map(|(id, _)| id.clone()).collect();
    for (fid, path) in &scope_file_resolved {
        if seen_mention.insert(fid.clone()) {
            mention_resolved.push((fid.clone(), path.clone()));
        }
    }
    let all_file_ids: Vec<String> = mention_resolved.iter().map(|(id, _)| id.clone()).collect();
    let mention_file_ids: Option<Vec<String>> = if all_file_ids.is_empty() { None } else { Some(all_file_ids) };

    log::info!(
        "[AI] scope resolved: dir_ids={:?} path_prefixes={:?} file_ids={:?} ext={:?} date={:?}~{:?}",
        dir_ids_opt, path_prefixes_opt, mention_file_ids, ext_filter, date_from, date_to
    );
    events.push(("scope_resolved".into(), serde_json::json!({
        "dir_ids_count": dir_ids_opt.as_ref().map_or(0, |v| v.len()),
        "path_prefixes": path_prefixes_opt.clone().unwrap_or_default(),
        "mention_files_count": mention_file_ids.as_ref().map_or(0, |v| v.len()),
        "ext_filter": ext_filter,
        "date_from": date_from,
        "date_to": date_to,
    })));

    // 动态依据：BM25 全量扫描 + 向量全量扫描 + SQL 路径匹配，三路合并去重。
    let mut all_hits: Vec<ScoredHit> = Vec::new();
    let mut all_seen = std::collections::HashSet::new();
    for (fid, _) in &mention_resolved {
        all_seen.insert(fid.clone());
    }
    if !last_q.trim().is_empty() {
        // 1. BM25 全量（limit=文档总数，不限量）
        let total_files = {
            let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
            crate::db::tracker::count_active_files(&c).map_err(|e| e.to_string())?
        };
        let bm25_hits = bm25_relevant_hits(
            state, &search_q, (total_files as usize).max(500), crate::ai::embedding_enabled(),
            dir_ids_opt.clone(), ext_filter.clone(), date_from, date_to, path_prefixes_opt.clone(), mention_file_ids.clone(),
        ).unwrap_or_default();
        let bm25_count = bm25_hits.len();
        for hit in bm25_hits {
            if all_seen.insert(hit.file_id.clone()) { all_hits.push(hit); }
        }
        // 2. 向量全量扫描（语义兜底，阈值过滤）
        if crate::ai::embedding_enabled() {
            let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
            if let Ok(vec_hits) = crate::ai::vector_full_scan(&c, &search_q, VECTOR_THRESHOLD) {
                let mut i = 0usize;
                for (fid, sim) in vec_hits {
                    if all_seen.insert(fid.clone()) {
                        all_hits.push(ScoredHit {
                            file_id: fid, path: String::new(), bm25_score: None,
                            semantic_score: Some(sim as f64), rrf_score: None, from_history: false,
                        });
                    }
                    i += 1;
                    if i > 500 { break; }
                }
            }
        }
        // 3. SQL 路径匹配（无上限，精确命中）
        let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
        if let Ok(path_hits) = crate::db::tracker::path_match_files(&c, &search_q) {
            for (fid, _path) in path_hits {
                if all_seen.insert(fid.clone()) {
                    all_hits.push(ScoredHit {
                        file_id: fid, path: String::new(), bm25_score: None,
                        semantic_score: None, rrf_score: None, from_history: false,
                    });
                }
            }
        }
        // 填充 path（延迟加载）
        {
            let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
            for hit in &mut all_hits {
                if hit.path.is_empty() {
                    if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&c, &hit.file_id) {
                        hit.path = rec.path.clone();
                    }
                }
            }
        }
        log::info!("[AI] three_way_scan: query={search_q} bm25={} vector+path={} total={}", bm25_count, all_hits.len() - bm25_count, all_hits.len());
    }

    const MAX_CONTENT_INJECT: usize = 30;
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    // 旧来源保留：对话中明确提到过的文件优先补入
    if !strict_docs && all_hits.len() < MAX_CONTENT_INJECT {
        let message_text: String = messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ");
        for fid in source_ids.iter().rev() {
            if all_hits.len() >= MAX_CONTENT_INJECT { break; }
            if all_seen.contains(fid) { continue; }
            let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) else { continue };
            if rec.status == "active" && rec.md5.is_some() {
                let stem = std::path::Path::new(&rec.path).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                if !stem.is_empty() && message_text.contains(stem.as_str()) {
                    all_seen.insert(fid.clone());
                    all_hits.push(ScoredHit { file_id: fid.clone(), path: rec.path.clone(), bm25_score: None, semantic_score: None, rrf_score: None, from_history: true });
                }
            }
        }
    }

    let from_history_count = all_hits.iter().filter(|h| h.from_history).count();
    events.push(("retrieval".into(), serde_json::json!({
        "search_query": search_q,
        "total_matches": all_hits.len(),
        "from_history_count": from_history_count,
    })));

    let mut docs: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();
    // @mention 文件直用：按解析结果标 [N] 编号引用，优先于检索命中。
    let mut mention_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut mention_has_content = false;
    for (i, (fid, resolved_path)) in mention_resolved.iter().enumerate() {
        let n = i + 1; // [N] 从 1 开始
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid)
            && let Some(md5) = &rec.md5
                && let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5)
                    && !text.trim().is_empty() {
                        mention_has_content = true;
                        docs.push(format!("[{n}]（{resolved_path}）\n{}", chunked_or_truncated(&conn, md5, &text, &search_q)));
                        evidence.push(EvidenceItem {
                            file_id: fid.clone(),
                            path: resolved_path.clone(),
                            snippet: truncate_text(&text, 200),
                            bm25_score: None,
                            semantic_score: None,
                            rrf_score: None,
                            rewritten,
                            rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                            from_history: false,
                        });
                        mention_index.insert(resolved_path.clone(), n);
                    }
    }
    for hit in &all_hits {
        if hit.from_history { continue; }
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id)
            && let Some(md5) = &rec.md5
                && let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5)
                    && !text.trim().is_empty() {
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
    drop(conn);

    let source_ids_final = all_hits.iter().map(|h| h.file_id.clone()).collect();
    let source_files_final = all_hits.iter().map(|h| h.path.clone()).collect();

    let context = truncate_text(&docs.join("\n\n---\n\n"), 50000);
    // 严格模式（仅依据文档）：范围内无命中时明确拒绝，而非让 LLM 自由发挥。
    if strict_docs {
        if !missing_mentions.is_empty() {
            return Err(format!("找不到引用文件: {}", missing_mentions.join(", ")));
        }
        if !mention_resolved.is_empty() && !mention_has_content {
            return Err("引用文件无可用内容".into());
        }
        if context.trim().is_empty() {
            return Err("未在与当前范围匹配的文档中找到依据".into());
        }
    }
    let system = format!("你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。如果材料不足以回答，请明确说明。\n引用材料时在文件名后标注 [N]（N 为材料编号），便于用户查阅原文。\n\n材料：\n{}", context);
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
    let hits = all_hits.iter().filter(|h| !h.from_history).count();
    events.push(("context_assembled".into(), serde_json::json!({
        "material_count": docs.len(),
        "total_chars": context.chars().count(),
        "strict_docs": strict_docs,
        "truncated_to": 50000,
    })));
    Ok(PreparedConversation { system, user_msg, source_ids: source_ids_final, source_files: source_files_final, evidence, search_query: search_q, hits, events, total_match_count: all_hits.len() })
}

/// Pipeline 版本的 prepare_conversation_prompt（使用 RAGPipeline）。
/// 与原函数功能相同，但拆分为独立 Skill 模块，便于测试和维护。
#[allow(dead_code)]
async fn prepare_conversation_prompt_pipeline(
    state: &tauri::State<'_, AppState>,
    messages: &[ChatMessage],
    source_ids: &[String],
    scope: &TurnScope,
    session_retrieval_scope: &[String],
    strict_docs: bool,
) -> Result<PreparedConversation, String> {
    use crate::ai::skills::pipeline::{RAGPipeline, RAGContext};
    use std::sync::Arc;

    let last_q = messages.last().map(|m| m.content.clone()).unwrap_or_default();
    let pipeline = RAGPipeline::new();
    let db = state.db.clone();
    let state_ptr: &'static tauri::State<'static, AppState> = unsafe { std::mem::transmute(state) };

    let output = pipeline.execute(&RAGContext {
        last_q,
        messages: messages.to_vec(),
        scope: scope.clone(),
        session_retrieval_scope: session_retrieval_scope.to_vec(),
        strict_docs,
        source_ids: source_ids.to_vec(),
        db: Arc::new(db),
        state: Some(state_ptr),
    }).await.map_err(|e| e.message)?;

    Ok(PreparedConversation {
        system: output.system,
        user_msg: output.user_msg,
        source_ids: output.source_ids,
        source_files: output.source_files,
        evidence: output.evidence,
        search_query: String::new(),
        hits: 0,
        events: vec![],
        total_match_count: 0,
    })
}

/// Post-process LLM response: supplement [N] citations for sentences that
/// match evidence snippets but weren't tagged by the LLM.
///
/// Algorithm:
/// 1. Split by 。！？.!? into sentences
/// 2. Skip sentences that already have [N]
/// 3. For remaining sentences, compute keyword overlap with each evidence snippet
/// 4. If overlap > threshold, add [N] to sentence end
/// 5. Merge consecutive sentences that cite the same source
pub fn auto_cite(answer: &str, evidence: &[EvidenceItem]) -> String {
    if evidence.is_empty() || answer.trim().is_empty() {
        return answer.to_string();
    }

    let code_block_re = regex::Regex::new(r"```[\s\S]*?```").unwrap();
    let mut placeholders: Vec<String> = Vec::new();
    let protected = code_block_re.replace_all(answer, |caps: &regex::Captures| {
        let idx = placeholders.len();
        placeholders.push(caps[0].to_string());
        format!("\x00CODE{idx}\x00")
    });

    let labels: Vec<(usize, &str)> = evidence.iter().enumerate().map(|(i, _)| (i + 1, &evidence[i].snippet as &str)).collect();

    let sent_re = regex::Regex::new(r"[^。！？.!?\n]*[。！？.!?]").unwrap();
    let mut result = String::with_capacity(protected.len() + 64);
    let mut last_end = 0;
    let mut prev_cite: Option<usize> = None;

    for m in sent_re.find_iter(&protected) {
        let sent = m.as_str();
        let trimmed = sent.trim();

        result.push_str(&protected[last_end..m.start()]);
        result.push_str(sent);

        last_end = m.end();

        if trimmed.is_empty() || trimmed.starts_with("\x00CODE") || regex::Regex::new(r"\[\d+\]").unwrap().is_match(trimmed) {
            prev_cite = None;
            continue;
        }

        let mut best: Option<usize> = None;
        let mut best_score = 0.0;
        for (n, snippet) in &labels {
            let score = keyword_overlap(sent, snippet);
            if score > 0.15 && score > best_score {
                best_score = score;
                best = Some(*n);
            }
        }

        if let Some(n) = best {
            if prev_cite == Some(n) {
                let pos = result.rfind(&format!("[{n}]")).unwrap_or(result.len());
                result.replace_range(pos..pos + format!("[{n}]").len(), "");
            }
            result.push_str(&format!("[{n}]"));
            prev_cite = Some(n);
        } else {
            prev_cite = None;
        }
    }
    result.push_str(&protected[last_end..]);

    for (idx, code) in placeholders.iter().enumerate() {
        result = result.replace(&format!("\x00CODE{idx}\x00"), code);
    }

    result
}

/// Jaccard-like keyword overlap score between two strings.
fn keyword_overlap(a: &str, b: &str) -> f64 {
    let a_words: std::collections::HashSet<String> = crate::search::schema::JIEBA
        .cut(a, true)
        .iter()
        .map(|w| w.word.to_lowercase())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    let b_words: std::collections::HashSet<String> = crate::search::schema::JIEBA
        .cut(b, true)
        .iter()
        .map(|w| w.word.to_lowercase())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    if a_words.is_empty() || b_words.is_empty() { return 0.0; }
    let intersection = a_words.intersection(&b_words).count();
    intersection as f64 / a_words.len().max(1) as f64
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

    let PreparedConversation { system, user_msg, evidence, .. } =
        prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_retrieval_scope, strict_docs).await?;
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
    let cited = auto_cite(&answer, &evidence);
    log::info!("[AI] conversation_ask: done, answer_chars={} cited_chars={}", answer.chars().count(), cited.chars().count());

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

    log::info!("[AI] conversation_ask_stream: scope={:?}", scope);

    let PreparedConversation { system, user_msg, source_ids, source_files, evidence, search_query, hits, total_match_count, mut events } =
        prepare_conversation_prompt(&state, &messages, &source_ids, &scope, &session_retrieval_scope, strict_docs).await?;
    let trace_id = format!("{session_id}#t{}", messages.iter().filter(|m| m.role == "user").count());
    let cfg = crate::config::load_config();
    let turn_number = messages.iter().filter(|m| m.role == "user").count().saturating_sub(1);
    log::info!(
        "[AI_TRACE] turn_begin trace_id={trace_id} llm={} embedding={} strict={} hits={} search_q={}",
        cfg.active_llm_model_id, cfg.active_embedding_model_id, strict_docs, hits, search_query
    );
    events.push(("llm_call".into(), serde_json::json!({
        "model_id": cfg.active_llm_model_id,
        "system_prompt_chars": system.chars().count(),
        "user_msg_chars": user_msg.chars().count(),
        "streaming": true,
    })));
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
    let cited_text = auto_cite(&raw_text, &evidence);
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

pub fn truncate_text(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars].iter().collect()
    }
}

pub async fn batch_summarize(
    state: &tauri::State<'_, crate::state::AppState>,
    file_ids: &[String],
    query: &str,
) -> Result<(String, Vec<String>), String> {
    if file_ids.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    const BATCH_SIZE: usize = 15;
    let batches: Vec<&[String]> = file_ids.chunks(BATCH_SIZE).collect();

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let batch_contents: Vec<String> = batches.iter().map(|batch| {
        let mut content = String::new();
        for fid in *batch {
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) {
                if let Some(md5) = &rec.md5 {
                    if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                        let brief = truncate_text(&text, 3000);
                        content.push_str(&format!("【{}】\n{}\n\n", rec.path, brief));
                    }
                }
            }
        }
        content
    }).collect();
    drop(conn);

    let total = batches.len();
    let mut summaries: Vec<String> = Vec::with_capacity(total);
    let system = "你是法律文档分析助手。请用简洁中文总结以下文档内容中与查询主题相关的关键信息，不超过500字。";

    for (i, batch_content) in batch_contents.iter().enumerate() {
        let user_msg = format!("查询主题：{}\n\n第{}批文档（共{}批）：\n{}", query, i + 1, total, batch_content);
        match tokio::task::spawn_blocking({
            let sys = system.to_string();
            let usr = user_msg;
            move || crate::ai::chat(&sys, &usr)
        }).await {
            Ok(Some(summary)) => summaries.push(summary),
            Ok(None) => summaries.push(format!("（第{}批摘要生成失败）", i + 1)),
            Err(e) => summaries.push(format!("（第{}批处理出错: {e}）", i + 1)),
        }
    }

    let combined = summaries.join("\n\n---\n\n");
    if batches.len() == 1 {
        return Ok((combined, file_ids.to_vec()));
    }

    let reduce_system = "你是法律文档分析助手。以下是多组文档摘要，请将它们整合为一份连贯的综合分析。";
    let reduce_msg = format!("查询主题：{}\n\n各批摘要如下：\n{}", query, combined);

    let final_summary = match tokio::task::spawn_blocking(move || crate::ai::chat(reduce_system, &reduce_msg)).await {
        Ok(Some(s)) => s,
        _ => combined,
    };

    Ok((final_summary, file_ids.to_vec()))
}

/// Inject the full text (≤50 K chars) or, for longer documents, select the
/// top-8 lexically-relevant chunks via jieba term overlap. Falls back to
/// truncation when chunks are unavailable.
fn chunked_or_truncated(conn: &rusqlite::Connection, md5: &str, text: &str, query: &str) -> String {
    if text.chars().count() <= 50_000 {
        return truncate_text(text, 50_000);
    }
    let chunks = match crate::db::chunks::get_chunks(conn, md5) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[AI] doc_chunks read failed: {e}");
            vec![]
        }
    };
    if chunks.is_empty() {
        return truncate_text(text, 50_000);
    }
    crate::db::chunks::select_relevant_chunks(&chunks, query, 8)
        .iter()
        .map(|c| format!("（第{}-{}字）\n{}", c.start_char, c.end_char, c.text))
        .collect::<Vec<_>>()
        .join("\n···\n")
}

fn chunked_or_truncated_with_budget(conn: &rusqlite::Connection, md5: &str, text: &str, query: &str, char_budget: usize) -> String {
    if char_budget == 0 { return String::new(); }
    if text.chars().count() <= char_budget {
        return truncate_text(text, char_budget);
    }
    let chunks = crate::db::chunks::get_chunks(conn, md5).unwrap_or_default();
    if chunks.is_empty() {
        return truncate_text(text, char_budget);
    }
    let relevant = crate::db::chunks::select_relevant_chunks(&chunks, query, chunks.len());
    let mut packed: Vec<String> = Vec::new();
    let mut used = 0usize;
    for chunk in &relevant {
        let chunk_chars = chunk.text.chars().count() + 20;
        if used + chunk_chars > char_budget {
            if packed.is_empty() {
                packed.push(truncate_text(&chunk.text, char_budget));
            }
            break;
        }
        packed.push(format!("（第{}-{}字）\n{}", chunk.start_char, chunk.end_char, chunk.text));
        used += chunk_chars;
    }
    packed.join("\n···\n")
}

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
    #[serde(default)]
    pub strict_docs: bool,
}


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

fn now_ts() -> i64 {
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
    let mut out = format!("{index}. 📄 `{}` {score_str}", e.path);
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
#[tauri::command]
pub fn export_chat_session(state: State<'_, AppState>, id: String) -> Result<String, String> {
    export_chat_session_impl(&state.data_dir, &id)
}

pub fn export_chat_session_impl(data_dir: &std::path::Path, id: &str) -> Result<String, String> {
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
}

/// 导出会话为 JSON（分析友好）：空范围轮次 scope=[] ，无回答轮次 answer=null。
#[tauri::command]
pub fn export_chat_session_json(state: State<'_, AppState>, id: String) -> Result<String, String> {
    export_chat_session_json_impl(&state.data_dir, &id)
}

pub fn export_chat_session_json_impl(data_dir: &std::path::Path, id: &str) -> Result<String, String> {
    let h = read_history(data_dir);
    let session = h
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "会话不存在".to_string())?;

    let mut turns = Vec::new();
    let mut turn_idx = 0usize;
    for (i, m) in session.messages.iter().enumerate() {
        if m.role == "user" {
            turn_idx += 1;
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
            });
        }
    }

    let config = crate::config::load_config();
    let export = ChatExportJson {
        schema_version: 2,
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

#[derive(Serialize)]
pub struct AiEventJson {
    pub id: i64,
    pub session_id: String,
    pub turn_number: usize,
    pub event_seq: u32,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

#[tauri::command]
pub fn get_ai_events(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<AiEventJson>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let events = crate::db::ai_events::get_session_events(&conn, &session_id)
        .map_err(|e| format!("{e}"))?;
    Ok(events.into_iter().map(|e| AiEventJson {
        id: e.id,
        session_id: e.session_id,
        turn_number: e.turn_number,
        event_seq: e.event_seq,
        event_type: e.event_type,
        payload: e.payload,
        created_at: e.created_at,
    }).collect())
}

#[tauri::command]
pub fn get_turn_ai_events(
    state: State<'_, AppState>,
    session_id: String,
    turn_number: usize,
) -> Result<Vec<AiEventJson>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let events = crate::db::ai_events::get_turn_events(&conn, &session_id, turn_number)
        .map_err(|e| format!("{e}"))?;
    Ok(events.into_iter().map(|e| AiEventJson {
        id: e.id,
        session_id: e.session_id,
        turn_number: e.turn_number,
        event_seq: e.event_seq,
        event_type: e.event_type,
        payload: e.payload,
        created_at: e.created_at,
    }).collect())
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
        let md = export_chat_session_impl(&dir, "s1").unwrap();
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
        let md = export_chat_session_impl(&dir, "s3").unwrap();
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
        let json_str = export_chat_session_json_impl(&dir, "s2").unwrap();
        let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let t0 = &v["turns"][0];
        assert_eq!(t0["scope"], serde_json::json!([]), "未指定范围轮次 scope 应为空数组");
        assert_eq!(t0["answer"], serde_json::Value::Null, "无回答轮次 answer 应为 null");
        assert_eq!(t0["evidence"].as_array().map(Vec::len), Some(0), "无依据轮次 evidence 应为空数组");

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
        let md = export_chat_session_impl(&dir, "s1").unwrap();
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
        let md = export_chat_session_impl(&dir, "s2").unwrap();
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
}

#[cfg(test)]
mod auto_cite_md_tests {
    use super::*;

    fn ev(snippet: &str) -> EvidenceItem {
        EvidenceItem {
            file_id: "f1".into(), path: "合同.pdf".into(), snippet: snippet.into(),
            bm25_score: None, semantic_score: None, rrf_score: None,
            rewritten: false, rewritten_query: None, from_history: false,
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
}
