//! Prompt assembly for smart search and multi-turn conversation (RAG):
//! retrieval, evidence selection, budget packing and clarify handling.

use super::*;
/// BM25 retrieval + content assembly shared by one-shot and streaming
/// smart_search. Returns the prompt pair plus the source file lists.
pub(super) struct PreparedSmart {
    pub(super) system: String,
    pub(super) user_msg: String,
    pub(super) source_ids: Vec<String>,
    pub(super) source_files: Vec<String>,
    pub(super) evidence: Vec<EvidenceItem>,
}

pub(super) fn prepare_smart_prompt(
    state: &AppState,
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
        // 批量取文件 + 内容，避免每 hit 一次 DB round trip（N+1）
        let ids: Vec<String> = hits.iter().map(|h| h.file_id.clone()).collect();
        let recs = crate::db::tracker::get_files_by_ids(&conn, &ids).unwrap_or_default();
        let mut md5_to_text: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        {
            let md5s: Vec<String> = recs.iter().flatten().filter_map(|r| r.md5.clone()).collect();
            let contents = crate::db::tracker::get_contents(&conn, &md5s).unwrap_or_default();
            for (m, c) in md5s.iter().zip(contents.iter()) {
                if let Some(text) = c {
                    md5_to_text.insert(m.clone(), text.clone());
                }
            }
        }
        for (hit, rec) in hits.iter().zip(recs.iter()) {
            let Some(rec) = rec else { continue };
            let Some(md5) = &rec.md5 else { continue };
            let Some(text) = md5_to_text.get(md5) else { continue };
            if text.trim().is_empty() { continue; }
            docs.push(format!("【{}】\n{}", rec.path, truncate_text(text, 2000)));
            sids.push(hit.file_id.clone());
            sf.push(rec.path.clone());
            ev.push(EvidenceItem {
                file_id: hit.file_id.clone(),
                path: rec.path.clone(),
                snippet: truncate_text(text, 200),
                bm25_score: hit.bm25_score,
                semantic_score: hit.semantic_score,
                rrf_score: hit.rrf_score,
                rewritten: false,
                rewritten_query: None,
                from_history: false,
                material_no: 0,
                injected_spans: Vec::new(),
            });
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
/// Assembled conversation prompt + the (possibly updated) source file list
/// backing it. Follow-up questions re-retrieve relevant documents so the
/// answer (and the frontend source list) reflects the newest question.
pub(crate) struct PreparedConversation {
    pub(crate) system: String,
    pub(crate) user_msg: String,
    pub(crate) source_ids: Vec<String>,
    pub(crate) source_files: Vec<String>,
    pub(crate) evidence: Vec<EvidenceItem>,
    /// Final retrieval query (after rewrite) — recorded per turn for traceability.
    pub(crate) search_query: String,
    /// jieba-tokenized query terms — the frontend highlights these in the
    /// cited passage shown by the hover preview.
    pub(crate) search_terms: Vec<String>,
    /// Disclosure note prepended to the answer when the question refers to an
    /// unnamed entity and the materials split across several distinct ones.
    pub(crate) clarify_note: Option<String>,
    /// Candidate entities backing `clarify_note` — rendered as one-click chips
    /// that re-ask the question narrowed to the picked entity.
    pub(crate) clarify_candidates: Vec<String>,
    /// Same asks as `clarify_candidates`, but structured (carries `stype`) so the
    /// frontend can render a per-slot fill-in and send back a `ClarifyBinding`.
    pub(crate) clarify_slots: Vec<ClarifySlot>,
    /// 指代未绑定 **且** 提问里没有任何具名实体 → 无锚点可依。此时硬答会用错误
    /// 的案件作答（RAG_PIPELINE.md 所称"自信地答错"），故改为只问不答、不调用 LLM。
    pub(crate) clarify_blocking: bool,
    /// Number of BM25 hits before merge with @mention files.
    pub(crate) hits: usize,
    pub(crate) total_match_count: usize,
    /// Whether real material was injected (context non-empty after trim).
    /// When false under non-strict mode, callers should refuse to answer
    /// rather than let the LLM answer with no evidence.
    pub(crate) has_evidence: bool,
    /// Material numbers actually visible in `context` after the total length
    /// cap. Citations must validate against this set, **not** `evidence.len()`:
    /// materials past the cap were retrieved but never entered the prompt.
    pub(crate) visible_nums: std::collections::BTreeSet<usize>,
    /// Accumulated AI events for this turn's pipeline execution.
    /// Caller should batch-insert into ai_events table.
    pub(crate) events: Vec<(String, serde_json::Value)>,
}

const CONTEXT_BUDGET: usize = 150_000;
const SYSTEM_OVERHEAD: usize = 2_000;
const ANSWER_RESERVE: usize = 8_000;
/// 单份材料的预算 = 剩余预算的该百分比（递减分配）。排名靠前的材料拿更多，
/// 长文档才有足够篇幅覆盖位于文末的答案段。
///
/// [`MIN_PER_FILE`] 是**可用性下限**：实测答案块在一份文档里常排到第 3 名，
/// 装下它需要约 4600 字（3 块）。下限过低（如 1500）会让排名中后的文档只拿到
/// 1 块，出现"进了注入却看不到答案"。
const PER_FILE_PCT: usize = 14;
const MIN_PER_FILE: usize = 4_600;
/// 头块（标题/当事人/案号）仅在预算够放 4 块时才带上；独立于 [`MIN_PER_FILE`]，
/// 否则抬高后者会顺带禁掉头块。
const HEAD_MIN_BUDGET: usize = 6_000;
/// 全库文件级向量相似度阈值。实测标定（bge-small-zh-v1.5，11,679 条向量）：
/// 真实查询的最高余弦落在 0.61~0.77，中位数 0.26~0.45。原值 0.65 高于部分
/// 查询的 max（"联嵘"查询 max=0.6111），导致向量通道对该类查询完全失效；
/// 0.55 仍远高于中位数、保持区分度，且各查询可召回 40+ 候选。
const VECTOR_THRESHOLD: f32 = 0.55;
/// Chunk vectors are shorter/more focused than whole-file vectors, so the
/// similarity distribution sits lower; initial estimate, tunable via
/// `app_settings['chunk_vector_threshold']` in future.
const CHUNK_VECTOR_THRESHOLD: f32 = 0.55;
const CHUNK_VECTOR_TOP_K: usize = 500;

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

pub(crate) async fn prepare_conversation_prompt(
    state: &AppState,
    messages: &[ChatMessage],
    source_ids: &[String],
    scope: &TurnScope,
    session_retrieval_scope: &[String],
    strict_docs: bool,
    full_recall: bool,
    skip_llm_rewrite: bool,
    app: Option<&tauri::AppHandle>,
    session_id: &str,
    clarify_reply: Option<&ClarifyReply>,
) -> Result<PreparedConversation, String> {
    let emit_progress = |phase: &str, message: &str, current: usize, total: usize| {
        if let Some(a) = app {
            let _ = a.emit("ai-progress", AiProgress {
                session_id: session_id.to_string(),
                phase: phase.to_string(),
                message: message.to_string(),
                current,
                total,
            });
        }
    };
    macro_rules! check_cancel {
        () => {
            if crate::ai::ai_cancelled() {
                return Err("请求已取消".into());
            }
        };
    }
    let last_q = messages.last().map(|m| m.content.clone()).unwrap_or_default();
    // 澄清回填：用户回答追问时，前端只发答案本身（如"汪均益"），原问题随 binding
    // 回传。此处用「原问题 + 答案」作为本轮问题，答案即强锚点，检索才不会退化成
    // 对"汪均益"的单词查询。
    let last_q = match clarify_reply {
        Some(r) if !r.base_question.trim().is_empty() => {
            let base = r.base_question.trim();
            let anchors: Vec<&str> =
                r.bindings.iter().map(|b| b.value.trim()).filter(|v| !v.is_empty()).collect();
            if anchors.is_empty() { base.to_string() } else { format!("{base} {}", anchors.join(" ")) }
        }
        _ => last_q,
    };
    log::info!("[AI] ▶ prepare q=\"{}\" scope={} files strict={}",
        truncate_text(&last_q, 40),
        session_retrieval_scope.len(),
        strict_docs,
    );
    let mut events: Vec<(String, serde_json::Value)> = Vec::new();
    let mut material_order = prior_material_order(state, session_id);
    emit_progress("query_rewrite", "查询改写中...", 0, 0);
    let rule = rewrite_query(&last_q, messages);
    let original_rule_query = rule.query.clone();
    let search_q = if skip_llm_rewrite {
        rule.query.clone()
    } else {
        let context_paths = prior_evidence_paths(state, session_id);
        match llm_rewrite_query(&last_q, messages, &context_paths).await {
            Some(llm) if llm != rule.query => llm,
            _ => rule.query.clone(),
        }
    };
    // 硬性兜底：LLM 重写可能丢失上一轮问句的核心实体（人名/案名/项目名）。
    // 把父问句缺失的实体词补进检索 query，保证三通道能命中正确的实体文档。
    let search_q = {
        let parent_q = messages
            .iter()
            .rev()
            .filter(|m| m.role == "user")
            .map(|m| m.content.trim())
            .find(|c| !c.is_empty() && *c != last_q.trim())
            .unwrap_or("");
        ensure_parent_entities(&search_q, parent_q)
    };
    let rewritten = search_q != last_q.trim();
    let search_terms = query_terms(&search_q);
    log::info!("[AI]   rewrite: \"{}\" → \"{}\" (rewritten={})", truncate_text(&last_q, 30), truncate_text(&search_q, 30), rewritten);
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
            if p.is_empty() { continue; }
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
            // 文件扩展名检测：如果是文件路径（有扩展名），尝试精确匹配
            if let Some(ext) = std::path::Path::new(p).extension()
                && !ext.to_string_lossy().is_empty()
                    && let Ok(Some(rec)) = crate::db::tracker::get_file_by_path(&conn, p) {
                        scope_file_resolved.push((rec.id, rec.path));
                        continue;
                    }
            // 否则按相对路径前缀过滤（子目录/文件夹）。scope 是文件树里的
            // 真实目录，直接锚定原始路径前缀过滤；不做任何模糊/片段解析——
            // 字符级模糊会把仅含相同字符子序列的范围外目录（如真实库中并存的
            // "案件/HJ 和嘉 名誉权案"）误收进允许前缀，造成跨目录证据泄漏。
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
            // 否则按相对路径前缀过滤（子目录/文件夹）：直接锚定原始路径前缀
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
        "[AI]   scope: dirs={} prefixes={} files={} scope_files={} ext={:?} date={:?}~{:?}",
        dir_ids_opt.as_ref().map_or(0, |v| v.len()),
        path_prefixes_opt.as_ref().map_or(0, |v| v.len()),
        mention_file_ids.as_ref().map_or(0, |v| v.len()),
        scope_file_resolved.len(),
        ext_filter,
        date_from,
        date_to,
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
    // 评测用开关：允许环境变量覆盖检索参数，便于免重建做参数扫描（A/B）。
    let env_f32 = |key: &str, default: f32| -> f32 {
        std::env::var(key).ok().and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(default)
    };
    let env_usize = |key: &str, default: usize| -> usize {
        std::env::var(key).ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(default)
    };
    let vector_threshold = env_f32("LINK_SEARCHER_VECTOR_THRESHOLD", VECTOR_THRESHOLD);
    let chunk_threshold = env_f32("LINK_SEARCHER_CHUNK_VECTOR_THRESHOLD", CHUNK_VECTOR_THRESHOLD);
    let chunk_top_k = env_usize("LINK_SEARCHER_CHUNK_TOP_K", CHUNK_VECTOR_TOP_K);
    let chunk_rrf_weight = env_f32("LINK_SEARCHER_RRF_CHUNK_WEIGHT", 1.0) as f64;
    let bm25_rrf_weight = env_f32("LINK_SEARCHER_RRF_BM25_WEIGHT", 1.0) as f64;
    // BM25 头部"精英加成"：BM25 排名越靠前，字面相关性的精度越高；中后段
    // 则是任关键词命中的长尾（噪声）。实测全局提高 BM25 权重会同时抬高
    // rank 100~500 的长尾，灌满融合前 30（总分 80%→68%）；而只抬高前 K 名
    // 能精准保住"高字面相关 + 语义中等"的文档（旗舰案例 BM25 #7 被
    // 多通道中等文档挤出前 30 就是此因）。K=0 关闭（默认，行为不变）。
    let bm25_elite_k = env_usize("LINK_SEARCHER_BM25_ELITE_K", 0);
    let bm25_elite_weight = env_f32("LINK_SEARCHER_BM25_ELITE_WEIGHT", 4.0) as f64;
    // BM25 通道内部的语义重排（semantic_fuse，用设置页 semantic_weight 做
    // 分数混合）。外层管线已用 RRF 融合 BM25/向量/chunk/路径四个通道，内层
    // 再一次分数混合属重复融合：它会把"字面强、语义中等"的文档在通道内
    // 先压下去，外层再也救不回来。设为 off 时 BM25 通道保持纯 BM25 名次。
    let inner_semantic_fuse = std::env::var("LINK_SEARCHER_INNER_SEMANTIC_FUSE")
        .map(|v| !v.eq_ignore_ascii_case("off"))
        .unwrap_or(true)
        && crate::ai::embedding_enabled();
    let use_rrf = std::env::var("LINK_SEARCHER_FUSION")
        .map(|v| !v.eq_ignore_ascii_case("mix"))
        .unwrap_or(true);
    let mut rrf_acc: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let trace_ids_v = trace_target_ids(state, &std::env::var("LINK_SEARCHER_TRACE_TARGET").unwrap_or_default());
    // 从问句提炼核心实体词（如"常宏"），三通道共用：
    // 完整问句含大量泛词（民事/案件/多少），会稀释 BM25/向量信号并把
    // 精准文件挤出注入前 30；实体词让"常宏"这类专有名词直接命中。
    let retrieval_kws = extract_retrieval_keywords(&search_q);
    let vocab_rewrite_enabled = std::env::var("LINK_SEARCHER_VOCAB_REWRITE")
        .map(|v| !v.eq_ignore_ascii_case("off"))
        .unwrap_or(false);
    let final_kws = if vocab_rewrite_enabled {
        match state.db.get() {
            Ok(c) => match crate::ai::vocabulary_rewrite(&search_q, &c) {
                Some(llm_kws) => {
                    let mut merged = retrieval_kws.clone();
                    for k in llm_kws {
                        if !merged.contains(&k) {
                            merged.push(k);
                        }
                    }
                    log::info!("[AI]   vocab_rewrite merged kws: {:?}", merged);
                    merged
                }
                None => retrieval_kws,
            },
            Err(e) => {
                log::warn!("[AI] vocab_rewrite: db error: {e}");
                retrieval_kws
            }
        }
    } else {
        retrieval_kws
    };
    let bm25_query = if final_kws.is_empty() {
        search_q.clone()
    } else {
        final_kws.join(" OR ")
    };
    log::info!("[AI]   final_kws={:?} bm25_query=\"{}\"", final_kws, truncate_text(&bm25_query, 60));
    if !last_q.trim().is_empty() {
        check_cancel!();
        emit_progress("bm25", "BM25 检索中...", 0, 0);
        let total_files = {
            let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
            crate::db::tracker::count_active_files(&c).map_err(|e| e.to_string())?
        };
        let bm25_hits = bm25_relevant_hits(
            state, &bm25_query, (total_files as usize).max(500), inner_semantic_fuse,
            dir_ids_opt.clone(), ext_filter.clone(), date_from, date_to, None, mention_file_ids.clone(),
        ).unwrap_or_default();
        let bm25_count = bm25_hits.len();
        trace_rank("1. BM25 通道原始输出", bm25_hits.iter().map(|h| h.file_id.as_str()), &trace_ids_v);
        // 精英加成按"纯 BM25 名次"（bm25_score 降序）判定，而非通道内迭代名次：
        // 内层 semantic_fuse 会按分数混合重排该通道，若按重排名次加成，"字面强
        // 但语义中等"的文档在通道内已被压到几十名开外，外层再无从救起。
        let mut pure_order: Vec<usize> = (0..bm25_hits.len()).collect();
        pure_order.sort_by(|&a, &b| {
            bm25_hits[b].bm25_score.unwrap_or(0.0)
                .partial_cmp(&bm25_hits[a].bm25_score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut pure_rank: Vec<usize> = vec![0; bm25_hits.len()];
        for (r, &i) in pure_order.iter().enumerate() {
            pure_rank[i] = r;
        }
        for (rank, hit) in bm25_hits.into_iter().enumerate() {
            let channel_w = if bm25_elite_k > 0 && pure_rank[rank] < bm25_elite_k {
                bm25_elite_weight
            } else {
                bm25_rrf_weight
            };
            rrf_add(&mut rrf_acc, &hit.file_id, rank, channel_w);
            if all_seen.insert(hit.file_id.clone()) { all_hits.push(hit); }
        }
        trace_rank("2. 仅 BM25 入池", all_hits.iter().map(|h| h.file_id.as_str()), &trace_ids_v);
        emit_progress("bm25", &format!("BM25 完成，命中 {} 份", bm25_count), bm25_count, bm25_count);
        let has_locked_scope = mention_file_ids.is_some() && mention_file_ids.as_ref().is_some_and(|ids| !ids.is_empty());
        if crate::ai::embedding_enabled() && !has_locked_scope {
            emit_progress("vector", "语义扫描中...", 0, 0);
            log::info!("[AI]   about to call vector_full_scan");
            let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
            let vec_query = if final_kws.is_empty() { search_q.clone() } else { final_kws.join(" ") };
            // 只嵌入一次，文件级与 chunk 级两个向量通道共享同一查询向量
            // （debug 下 bge-large 单次推理 85s，重复嵌入翻倍浪费）。
            // cached_embed：同一/近似查询追问直接命中，跳过本地 BGE 推理。
            let query_emb = crate::ai::cached_embed(&vec_query);
            if let Some(qe) = &query_emb {
                if let Ok(vec_hits) = crate::ai::vector_scan_with_query_emb(&c, qe, vector_threshold) {
                    log::info!("[AI]   vector_full_scan returned {} hits", vec_hits.len());
                    for (rank, (fid, sim)) in vec_hits.into_iter().enumerate() {
                        rrf_add(&mut rrf_acc, &fid, rank, 1.0);
                        if all_seen.insert(fid.clone()) {
                            all_hits.push(ScoredHit {
                                file_id: fid, path: String::new(), bm25_score: None,
                                semantic_score: Some(sim as f64), rrf_score: None, from_history: false,
                                from_chunk: false, hit_chunks: Vec::new(),
                            });
                        }
                        if !full_recall && rank >= 500 { break; }
                    }
                }
                // chunk 级向量通道：只对"文档级粗筛已命中的 md5 集"做块级精检
                // （两级漏斗，避免全库 12.5 万+ chunk 暴力余弦）。粗筛集 =
                // BM25 命中 + 文件级向量命中（上面已并入 all_hits）。
                // 注：曾试过把全库 chunk 扫描作为独立召回通道，实测**无效果**
                // （候选仅 +5/~3244，top-20 完全不变）——因为 BM25 已命中数千份、
                // 文档本就在候选池内，弱 chunk 信号无法改变排名，且 +6.2s/查询。
                if let Ok(chunk_hits) = {
                    // 收集粗筛命中文件 → md5 候选集
                    let hit_ids: Vec<String> = all_hits.iter().map(|h| h.file_id.clone()).collect();
                    let recs = if hit_ids.is_empty() {
                        Vec::new()
                    } else {
                        crate::db::tracker::get_files_by_ids(&c, &hit_ids).unwrap_or_default()
                    };
                    let mut md5s: Vec<String> = recs
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r.md5)
                        .collect();
                    md5s.sort();
                    md5s.dedup();
                    if md5s.is_empty() {
                        // 粗筛 0 命中时回退全库 chunk 扫描（保底：不因漏斗丢失
                        // "仅块级可命中"的极端场景；正常粗筛命中数千份时走漏斗）。
                        crate::ai::chunk_vector_scan_with_query_emb(&c, qe, chunk_threshold, chunk_top_k)
                    } else {
                        crate::ai::chunk_vector_scan_for_md5s(&c, qe, &md5s, chunk_threshold, chunk_top_k)
                    }
                } {
                    log::info!("[AI]   chunk_vector_scan returned {} hits", chunk_hits.len());
                    let mut by_md5: std::collections::HashMap<String, Vec<(usize, f32)>> = std::collections::HashMap::new();
                    let mut md5_first_rank: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                    for (rank, (md5, idx, sim)) in chunk_hits.into_iter().enumerate() {
                        md5_first_rank.entry(md5.clone()).or_insert(rank);
                        by_md5.entry(md5).or_default().push((idx, sim));
                    }
                    for (md5, chunks) in by_md5 {
                        let Ok(file_ids) = crate::db::tracker::get_files_by_md5(&c, &md5) else { continue };
                        let first_rank = md5_first_rank.get(&md5).copied().unwrap_or(0);
                        for fid in file_ids {
                            rrf_add(&mut rrf_acc, &fid, first_rank, chunk_rrf_weight);
                            if all_seen.insert(fid.clone()) {
                                all_hits.push(ScoredHit {
                                    file_id: fid, path: String::new(), bm25_score: None,
                                    semantic_score: chunks.first().map(|(_, s)| *s as f64),
                                    rrf_score: None, from_history: false,
                                    from_chunk: true, hit_chunks: chunks.clone(),
                                });
                            } else if let Some(h) = all_hits.iter_mut().find(|h| h.file_id == fid) {
                                // 文件已从 BM25/向量通道进池 → 命中块仍须并入，
                                // 否则注入层退回阅读顺序、丢掉文末的答案段。
                                h.hit_chunks.extend(chunks.iter().copied());
                            }
                        }
                    }
                }
            }
            // QA pair vector channel: pre-generated questions match user query
            // by semantic similarity, bridging the vocabulary gap between user's
            // everyday language and document's formal terminology.
            let qa_scan_enabled = std::env::var("LINK_SEARCHER_QA_SCAN")
                .map(|v| !v.eq_ignore_ascii_case("off"))
                .unwrap_or(false);
            if qa_scan_enabled {
                if let Some(qe) = &query_emb {
                    if let Ok(qa_hits) = crate::ai::qa_vector_scan_with_query_emb(&c, qe, vector_threshold, 50) {
                        log::info!("[AI]   qa_vector_scan returned {} hits", qa_hits.len());
                        for (rank, (fid, sim)) in qa_hits.into_iter().enumerate() {
                            rrf_add(&mut rrf_acc, &fid, rank, 1.0);
                            if all_seen.insert(fid.clone()) {
                                all_hits.push(ScoredHit {
                                    file_id: fid, path: String::new(), bm25_score: None,
                                    semantic_score: Some(sim as f64), rrf_score: None,
                                    from_history: false, from_chunk: false, hit_chunks: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
            emit_progress("vector", &format!("语义扫描完成，累计 {} 份", all_hits.len()), all_hits.len(), all_hits.len());
        }
        let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let path_kws: Vec<String> = if final_kws.is_empty() {
            // 无实体词时退化为完整问句 OR 分词片段，尽量不丢召回
            crate::search::schema::split_query_terms(&search_q)
                .split_whitespace()
                .map(|s| s.trim_matches(|c: char| c == '"' || c == '\'' || c == '(' || c == ')').to_string())
                .filter(|s| !s.is_empty() && s != "OR")
                .collect()
        } else {
            final_kws.clone()
        };
        if let Ok(path_hits) = crate::db::tracker::path_match_files(&c, &path_kws) {
            log::info!("[AI]   path_match_files returned {} hits (kws={:?})", path_hits.len(), path_kws);
            for (rank, (fid, _path)) in path_hits.into_iter().enumerate() {
                rrf_add(&mut rrf_acc, &fid, rank, 1.0);
                if all_seen.insert(fid.clone()) {
                    all_hits.push(ScoredHit {
                        file_id: fid, path: String::new(), bm25_score: None,
                        semantic_score: None, rrf_score: None, from_history: false,
                        from_chunk: false, hit_chunks: Vec::new(),
                    });
                }
            }
        }
        {
            let c = state.db.get().map_err(|e| format!("db error: {e}"))?;
            // 批量补全 path，避免每 hit 一次 DB round trip（N+1）
            let missing_ids: Vec<String> = all_hits.iter()
                .filter(|h| h.path.is_empty())
                .map(|h| h.file_id.clone())
                .collect();
            let recs = crate::db::tracker::get_files_by_ids(&c, &missing_ids).unwrap_or_default();
            let mut i = 0usize;
            for hit in &mut all_hits {
                if hit.path.is_empty() {
                    if let Some(Some(rec)) = recs.get(i) {
                        hit.path = rec.path.clone();
                    }
                    i += 1;
                }
            }
        }
        // 硬性范围拦截：向量/chunk/path 三通道都是全库扫描，会把范围外文件
        // 混进 all_hits。合并完成后立即统一过滤：目录前缀规则 ∪ scope 文件
        // 白名单；范围只点名文件时（无前缀）为纯白名单——三个通道全拦，
        // 杜绝"范围=文件，证据却混入同名泛词文件"的泄漏。
        all_hits.retain(|h| {
            hit_in_scope(
                &h.file_id,
                &h.path,
                path_prefixes_opt.as_deref(),
                mention_file_ids.as_deref(),
            )
        });
        // 统一排序：三通道命中按混合分（weighted_mix）降序排列，而非按通道
        // 添加顺序。BM25 分与语义分归一化后加权（w=semantic_weight），
        // 路径命中（无分）排最后。保证"最相关的文件先进注入前 30"。
        trace_rank("3. 三通道合并+范围过滤后", all_hits.iter().map(|h| h.file_id.as_str()), &trace_ids_v);
        if all_hits.len() > 1 {
            if use_rrf {
                for h in &mut all_hits {
                    h.rrf_score = rrf_acc.get(&h.file_id).copied();
                }
                all_hits.sort_by(|a, b| {
                    let ra = a.rrf_score.unwrap_or(0.0);
                    let rb = b.rrf_score.unwrap_or(0.0);
                    rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
                });
            } else {
                let weight = crate::config::load_config().semantic_weight.clamp(0.0, 1.0);
                let max_bm25 = all_hits.iter()
                    .filter_map(|h| h.bm25_score)
                    .fold(0.0_f64, f64::max);
                let max_sem = all_hits.iter()
                    .filter_map(|h| h.semantic_score)
                    .fold(0.0_f64, f64::max);
                all_hits.sort_by(|a, b| {
                    let score = |h: &ScoredHit| -> f64 {
                        let b = h.bm25_score.map(|s| if max_bm25 > 0.0 { s / max_bm25 } else { 0.0 }).unwrap_or(0.0);
                        let s = h.semantic_score.map(|x| if max_sem > 0.0 { x / max_sem } else { 0.0 }).unwrap_or(0.0);
                        if b > 0.0 || s > 0.0 { weight * s + (1.0 - weight) * b } else { 0.0 }
                    };
                    score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }
        // --- Rerank stage (after fusion, before top-30 injection) ---
        let rerank_enabled = std::env::var("LINK_SEARCHER_RERANK")
            .map(|v| !v.eq_ignore_ascii_case("off"))
            .unwrap_or(true);
        let rerank_top_n = std::env::var("LINK_SEARCHER_RERANK_TOP_N")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(50);
        if rerank_enabled && all_hits.len() > 10 {
            let top_n = rerank_top_n.min(all_hits.len());
            let rc = state.db.get().map_err(|e| format!("db error: {e}"))?;
            let cand_ids: Vec<String> = all_hits[..top_n].iter().map(|h| h.file_id.clone()).collect();
            let cand_recs = crate::db::tracker::get_files_by_ids(&rc, &cand_ids).unwrap_or_default();
            let mut md5s: Vec<String> = cand_recs.iter()
                .filter_map(|r| r.as_ref().and_then(|r| r.md5.clone()))
                .collect();
            md5s.sort();
            md5s.dedup();
            let chunk_embs = crate::db::tracker::get_chunk_embeddings_by_md5s(&rc, &md5s).unwrap_or_default();
            let mut chunks_by_md5: std::collections::HashMap<String, Vec<(usize, Vec<f32>)>> = std::collections::HashMap::new();
            for (md5, idx, vec) in chunk_embs {
                chunks_by_md5.entry(md5).or_default().push((idx, vec));
            }
            let query_emb = crate::ai::cached_embed(&search_q);
            let mut passages: Vec<String> = Vec::new();
            let mut rerank_idx: Vec<usize> = Vec::new();
            for (i, rec) in cand_recs.iter().enumerate() {
                let Some(rec) = rec else { continue };
                let Some(md5) = &rec.md5 else { continue };
                let passage = if let Some(qe) = &query_emb {
                    if let Some(chunks) = chunks_by_md5.get(md5) {
                        let best = chunks.iter().max_by(|(_, a), (_, b)| {
                            crate::ai::cosine(qe, a).partial_cmp(&crate::ai::cosine(qe, b))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        if let Some((chunk_idx, _)) = best {
                            let db_chunks = crate::db::chunks::get_chunks(&rc, md5).unwrap_or_default();
                            db_chunks.iter()
                                .find(|c| c.chunk_index as usize == *chunk_idx)
                                .map(|c| crate::ai::truncate_for_embed(&c.text))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let passage = passage.or_else(|| {
                    crate::db::tracker::get_content(&rc, md5).ok().flatten()
                        .map(|t| crate::ai::truncate_for_embed(&t))
                });
                if let Some(p) = passage {
                    passages.push(p);
                    rerank_idx.push(i);
                }
            }
            drop(rc);
            if !passages.is_empty() {
                match crate::ai::rerank(&search_q, &passages) {
                    Some(scores) if scores.len() == rerank_idx.len() => {
                        // 默认 0.3：实测 75 题下 w=0.3 的每一类别都不低于启用重排前，
                        // 而 w=1.0（纯重排）会把 long_doc 从 100% 压到 60%。
                        let fusion_w: f64 = std::env::var("LINK_SEARCHER_RERANK_FUSION")
                            .ok()
                            .and_then(|v| v.trim().parse::<f64>().ok())
                            .unwrap_or(0.3);
                        let perm = apply_rerank_order(&all_hits, &rerank_idx, &scores, fusion_w);
                        let new_hits: Vec<ScoredHit> = perm.iter().map(|&i| all_hits[i].clone()).collect();
                        all_hits = new_hits;
                        trace_rank("4. rerank 后", all_hits.iter().map(|h| h.file_id.as_str()), &trace_ids_v);
                log::info!("[AI]   rerank: reordered {} candidates", rerank_idx.len());
                    }
                    Some(scores) => {
                        log::warn!("[AI]   rerank: score count mismatch ({} vs {}), keeping original order", scores.len(), rerank_idx.len());
                    }
                    None => {
                        log::warn!("[AI]   rerank: unavailable, keeping original order");
                    }
                }
            }
        }
        emit_progress("retrieval", &format!("三路合并完成，共 {} 份文件", all_hits.len()), all_hits.len(), all_hits.len());
        log::info!("[AI]   scan: q=\"{}\" bm25={} extra={} total={}", truncate_text(&search_q, 30), bm25_count, all_hits.len().saturating_sub(bm25_count), all_hits.len());
    }

    const MAX_CONTENT_INJECT: usize = 30;
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    // 预取本轮需要的所有文件记录（旧来源 + mention + 注入候选），
    // 后续循环一律查内存 Map，避免每文件一次 DB round trip（N+1）。
    let mut rec_by_id: std::collections::HashMap<String, crate::db::tracker::FileRecord> = std::collections::HashMap::new();
    let mut md5_to_text: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    {
        let mut need_ids: Vec<String> = Vec::new();
        need_ids.extend(source_ids.iter().cloned());
        need_ids.extend(mention_resolved.iter().map(|(fid, _)| fid.clone()));
        need_ids.extend(all_hits.iter().map(|h| h.file_id.clone()));
        // 去重
        let mut seen = std::collections::HashSet::new();
        need_ids.retain(|id| seen.insert(id.clone()));
        let recs = crate::db::tracker::get_files_by_ids(&conn, &need_ids).unwrap_or_default();
        let mut md5s: Vec<String> = Vec::new();
        for (id, rec) in need_ids.iter().zip(recs.iter()) {
            if let Some(rec) = rec {
                rec_by_id.insert(id.clone(), rec.clone());
                if let Some(md5) = &rec.md5 {
                    md5s.push(md5.clone());
                }
            }
        }
        md5s.sort();
        md5s.dedup();
        let contents = crate::db::tracker::get_contents(&conn, &md5s).unwrap_or_default();
        for (m, c) in md5s.iter().zip(contents.iter()) {
            if let Some(text) = c {
                md5_to_text.insert(m.clone(), text.clone());
            }
        }
    }

    // 旧来源保留：同一会话的追问（messages.len() > 1 说明有历史轮次）延续讨论
    // 上一轮引用的证据。即使追问未显式点名文件名（如"列出清单/把时间列出来"），
    // 上一轮已注入引用的文件也应作为保底候选带入本轮，避免历史证据断层。
    // 仅在 strict 模式（要求严格依据当前范围）下不自动保留，避免越界引用。
    let is_follow_up = messages.len() > 1;
    let mut history_resolved: Vec<(String, String)> = Vec::new(); // (file_id, path) 待随材料注入
    if !strict_docs && is_follow_up && all_hits.len() < MAX_CONTENT_INJECT {
        let message_text: String = messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ");
        for fid in source_ids.iter().rev() {
            if all_hits.len() >= MAX_CONTENT_INJECT { break; }
            if all_seen.contains(fid) { continue; }
            let Some(rec) = rec_by_id.get(fid) else { continue };
            if rec.status != "active" || rec.md5.is_none() { continue; }
            // 追问未点名文件时也保留（保底证据），而非要求文件名出现在问句中
            let named = {
                let stem = std::path::Path::new(&rec.path).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                !stem.is_empty() && message_text.contains(stem.as_str())
            };
            if carry_forward_source(named, all_hits.is_empty()) {
                all_seen.insert(fid.clone());
                // 记入 history_resolved，稍后随 Layer 0.5 注入为实际材料
                if !history_resolved.iter().any(|(id, _)| id == fid) {
                    history_resolved.push((fid.clone(), rec.path.clone()));
                }
            }
        }
    }

    let from_history_count = history_resolved.len();
    events.push(("retrieval".into(), serde_json::json!({
        "search_query": search_q,
        "total_matches": all_hits.len(),
        "from_history_count": from_history_count,
    })));

    let mut docs: Vec<String> = Vec::new();
    let mut evidence: Vec<EvidenceItem> = Vec::new();
    let mut mention_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut mention_has_content = false;

    // Layer 0: Always load explicitly scoped files into context (independent of BM25).
    // 均摊预算：所有引用文件共享 CONTEXT_BUDGET/3，每个文件按份数分配，
    // 避免"前面的文件占满预算、后面的引用文件被最终截断静默丢弃"。
    let mention_total_budget = CONTEXT_BUDGET / 3;
    let mention_files_with_content: Vec<(String, String, String, String)> = mention_resolved
        .iter()
        .filter_map(|(fid, resolved_path)| {
            let rec = rec_by_id.get(fid)?;
            let md5 = rec.md5.as_ref()?;
            let text = md5_to_text.get(md5)?;
            if text.trim().is_empty() { return None; }
            Some((fid.clone(), resolved_path.clone(), md5.clone(), text.clone()))
        })
        .collect();
    let per_mention_file = mention_total_budget / mention_files_with_content.len().max(1);
    let mut mention_budget_used = 0usize;
    for (fid, resolved_path, md5, text) in mention_files_with_content.iter() {
        let n = assign_material_no(&mut material_order, fid);
        mention_has_content = true;
        let (injected, injected_spans) = chunked_or_truncated_with_budget(&conn, md5, text, &search_q, per_mention_file, &[]);
        if !injected.trim().is_empty() {
            docs.push(format!("[{n}]（{resolved_path}）\n{injected}"));
            mention_budget_used = mention_budget_used.saturating_add(injected.chars().count());
            mention_index.insert(resolved_path.clone(), n);
            evidence.push(EvidenceItem {
                file_id: fid.clone(),
                path: resolved_path.clone(),
                snippet: cited_excerpt(text, &search_terms, 300),
                bm25_score: None,
                semantic_score: None,
                rrf_score: None,
                rewritten,
                rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                from_history: false,
                material_no: n,
                injected_spans,
            });
        }
    }
    if !mention_files_with_content.is_empty() {
        log::info!("[AI]   Layer0 mentions: {} files, {:.1}k/{}k budget used", mention_files_with_content.len(), mention_budget_used as f64 / 1000.0, mention_total_budget as f64 / 1000.0);
    }

    // Layer 0.5: 上一轮引用的证据随追问带入本轮为保底材料（缺陷 3 修复）。
    // 与 mention 共摊 Layer0 预算，按文件份数均分；证据条目标记 from_history=true。
    let history_total_budget = CONTEXT_BUDGET / 3;
    let history_files_with_content: Vec<(String, String, String, String)> = history_resolved
        .iter()
        .filter_map(|(fid, path)| {
            let rec = rec_by_id.get(fid)?;
            let md5 = rec.md5.as_ref()?;
            let text = md5_to_text.get(md5)?;
            if text.trim().is_empty() { return None; }
            Some((fid.clone(), path.clone(), md5.clone(), text.clone()))
        })
        .collect();
    let per_history_file = history_total_budget / history_files_with_content.len().max(1);
    let mut history_budget_used = 0usize;
    for (fid, resolved_path, md5, text) in history_files_with_content.iter() {
        let n = assign_material_no(&mut material_order, fid);
        let (injected, injected_spans) = chunked_or_truncated_with_budget(&conn, md5, text, &search_q, per_history_file, &[]);
        if !injected.trim().is_empty() {
            docs.push(format!("[{n}]（{resolved_path}）\n{injected}"));
            history_budget_used = history_budget_used.saturating_add(injected.chars().count());
            mention_index.insert(resolved_path.clone(), n);
            evidence.push(EvidenceItem {
                file_id: fid.clone(),
                path: resolved_path.clone(),
                snippet: cited_excerpt(text, &search_terms, 300),
                bm25_score: None,
                semantic_score: None,
                rrf_score: None,
                rewritten,
                rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                from_history: true,
                material_no: n,
                injected_spans,
            });
        }
    }
    if !history_files_with_content.is_empty() {
        log::info!("[AI]   Layer0.5 history: {} files carried forward, {:.1}k/{}k budget used", history_files_with_content.len(), history_budget_used as f64 / 1000.0, history_total_budget as f64 / 1000.0);
    }

    // Layer 1: Additional retrieval hits (not already in scope).
    // 范围过滤：向量/chunk 通道是全库扫描，会把范围外无关文件混进 all_hits。
    // 注入前按 path_prefixes 前缀过滤（与 BM25 安全网一致），mention 文件
    // （Layer 0 已注入，且可能不在前缀内）除外。全库范围（无前缀）不过滤。
    // strict_docs 不再硬性跳过注入——改为始终用 scoped 过滤，确保严格模式
    // 下目录范围内的命中也能注入（否则目录引用 + strict = 零材料 = 无法回答）。
    let inject_limit = if full_recall { all_hits.len().max(1) } else { MAX_CONTENT_INJECT };
    let scoped = |h: &ScoredHit| -> bool {
        hit_in_scope(&h.file_id, &h.path, path_prefixes_opt.as_deref(), mention_file_ids.as_deref())
    };
    let content_hits: Vec<&ScoredHit> = all_hits.iter()
        .filter(|h| !h.from_history && !mention_index.contains_key(&h.path) && scoped(h))
        .take(inject_limit)
        .collect();
    trace_rank("5. content_hits(注入候选)", content_hits.iter().map(|h| h.file_id.as_str()), &trace_ids_v);
    let mut content_budget = CONTEXT_BUDGET
        .saturating_sub(SYSTEM_OVERHEAD)
        .saturating_sub(ANSWER_RESERVE);
    emit_progress("injection", "注入文件内容中...", 0, content_hits.len());
    log::info!("[AI]   injection: content_hits={} mention_has_content={}", content_hits.len(), mention_has_content);
    let mut inject_idx = 0usize;
    for hit in &content_hits {
        if inject_idx.is_multiple_of(10) { check_cancel!(); }
        if let Some(rec) = rec_by_id.get(&hit.file_id)
            && let Some(md5) = &rec.md5
                && let Some(text) = md5_to_text.get(md5)
                    && !text.trim().is_empty() {
                        let per_file = if full_recall {
                            content_budget
                        } else {
                            (content_budget * PER_FILE_PCT / 100).max(MIN_PER_FILE).min(content_budget)
                        };
                        let (injected, injected_spans) = chunked_or_truncated_with_budget(&conn, md5, text, &search_q, per_file, &hit.hit_chunks);
                        if !injected.trim().is_empty() {
                            let n = assign_material_no(&mut material_order, &hit.file_id);
                            docs.push(format!("[{n}]（{}）\n{}", rec.path, injected));
                            evidence.push(EvidenceItem {
                                file_id: hit.file_id.clone(),
                                path: rec.path.clone(),
                                snippet: cited_excerpt(text, &search_terms, 300),
                                bm25_score: hit.bm25_score,
                                semantic_score: hit.semantic_score,
                                rrf_score: hit.rrf_score,
                                rewritten,
                                rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                                from_history: false,
                                material_no: n,
                                injected_spans,
                            });
                            content_budget = content_budget.saturating_sub(injected.chars().count());
                        }
                        if content_budget < MIN_PER_FILE { break; }
                    }
        inject_idx += 1;
        if inject_idx.is_multiple_of(10) || inject_idx == content_hits.len() {
            emit_progress("injection", &format!("注入中 {}/{}", inject_idx, content_hits.len()), inject_idx, content_hits.len());
        }
    }

    // ── strict + 目录引用兜底 ──────────────────────────────────────────
    // 目录引用的语义是"以该目录下的文件为资料"。泛问句（如"这是什么文档"）
    // 没有实体词时三通道可能全空（BM25 泛词命中被范围拦掉、向量低于阈值、
    // 文件名不含词），但范围内明明有已索引文件——此时按路径顺序回退注入
    // 这些文件（走内容缓存），而不是直接拒答。
    if strict_docs
        && docs.is_empty()
        && evidence.is_empty()
        && let Some(prefixes) = &path_prefixes_opt
        && !prefixes.is_empty()
    {
        let mut scope_files: Vec<(String, String, String, String)> = Vec::new(); // (id, path, md5, text)
        let mut seen_ids = std::collections::HashSet::new();
        for p in prefixes {
            let pat = format!("{}%", p.trim_end_matches('/'));
            let Ok(mut stmt) = conn.prepare(
                "SELECT id, path, md5, \
                        (SELECT text_content FROM content_index WHERE md5 = file_tracking.md5) \
                 FROM file_tracking \
                 WHERE status='active' AND indexed=1 AND path LIKE ?1 \
                 ORDER BY path LIMIT 50",
            ) else { continue };
            let Ok(rows) = stmt.query_map(rusqlite::params![&pat], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            }) else { continue };
            for row in rows.flatten() {
                let (id, path, md5, text) = row;
                let (Some(md5), Some(text)) = (md5, text) else { continue };
                if text.trim().is_empty() || !seen_ids.insert(id.clone()) { continue }
                scope_files.push((id, path, md5, text));
            }
        }
        if !scope_files.is_empty() {
            let budget = if content_budget > 0 { content_budget } else { CONTEXT_BUDGET / 3 };
            let per_file = (budget / scope_files.len()).max(500);
            let mut injected_n = 0usize;
            for (fid, path, md5, text) in &scope_files {
                let n = assign_material_no(&mut material_order, fid);
                let (injected, injected_spans) = chunked_or_truncated_with_budget(&conn, md5, text, &search_q, per_file, &[]);
                if injected.trim().is_empty() { continue }
                docs.push(format!("[{n}]（{path}）\n{injected}"));
                evidence.push(EvidenceItem {
                    file_id: fid.clone(),
                    path: path.clone(),
                    snippet: cited_excerpt(text, &search_terms, 300),
                    bm25_score: None,
                    semantic_score: None,
                    rrf_score: None,
                    rewritten,
                    rewritten_query: if rewritten { Some(search_q.clone()) } else { None },
                    from_history: false,
                    material_no: n,
                    injected_spans,
                });
                injected_n += 1;
            }
            log::info!("[AI] scope fallback: strict+目录引用零检索，回退注入范围内 {injected_n}/{} 份文件", scope_files.len());
        }
    }

    drop(conn);

    // source_ids/source_files 必须与 evidence 严格同序同长（只含实际注入的
    // 材料），前端 [N] 引用按 evidence 序号跳转原文，错位会跳到错误文件。
    let source_ids_final = evidence.iter().map(|e| e.file_id.clone()).collect();
    let source_files_final = evidence.iter().map(|e| e.path.clone()).collect();

    let max_context_chars = CONTEXT_BUDGET - SYSTEM_OVERHEAD - ANSWER_RESERVE;
    let context = truncate_text(&docs.join("\n\n---\n\n"), max_context_chars);

    // Layer 2: Chat history recall — keyword-match previous AI responses.
    // 不走 Tantivy（聊天消息未索引），在内存中对历史回答做关键词匹配，
    // 命中的回答注入 2000 字摘录到"对话回忆"段，让 LLM 能回忆之前讨论过的内容。
    const CHAT_RECALL_BUDGET: usize = 20_000;
    const CHAT_RECALL_PER_MSG: usize = 2_000;
    let mut chat_recall: Vec<String> = Vec::new();
    let mut recall_budget = CHAT_RECALL_BUDGET;
    if messages.len() > 1 && !final_kws.is_empty() {
        for (i, m) in messages.iter().enumerate().take(messages.len().saturating_sub(1)) {
            if m.role != "assistant" || m.content.trim().is_empty() { continue; }
            let content_lower = m.content.to_lowercase();
            let match_count = final_kws.iter()
                .filter(|kw| content_lower.contains(&kw.to_lowercase()))
                .count();
            if match_count == 0 { continue; }
            let turn = (i + 1) / 2;
            let limit = recall_budget.min(CHAT_RECALL_PER_MSG);
            let excerpt = truncate_text(&m.content, limit);
            recall_budget = recall_budget.saturating_sub(excerpt.chars().count());
            chat_recall.push(format!("【第{turn}轮回答片段】\n{excerpt}"));
            if recall_budget == 0 { break; }
        }
    }
    let recall_section = if chat_recall.is_empty() {
        String::new()
    } else {
        format!("\n\n--- 对话回忆（来自历史回答，按关键词匹配） ---\n{}", chat_recall.join("\n\n"))
    };

    log::info!("[AI]   context assembled: strict={} docs_len={} context_chars={} recall={} mention_resolved={} mention_has_content={}", strict_docs, docs.len(), context.chars().count(), chat_recall.len(), mention_resolved.len(), mention_has_content);
    // 严格模式（仅依据文档）：范围内无命中时明确拒绝，而非让 LLM 自由发挥。
    if strict_docs {
        if !missing_mentions.is_empty() {
            log::warn!("[AI] strict mode rejected: missing mentions={:?}", missing_mentions);
            return Err(format!("找不到引用文件: {}", missing_mentions.join(", ")));
        }
        if !mention_resolved.is_empty() && !mention_has_content {
            log::warn!("[AI] strict mode rejected: mention_resolved has no content");
            return Err("引用文件无可用内容".into());
        }
        if context.trim().is_empty() {
            log::warn!("[AI] strict mode rejected: empty context (all_hits={} docs={})", all_hits.len(), docs.len());
            return Err("未在与当前范围匹配的文档中找到依据".into());
        }
    }
    // 材料编号每轮从 [1] 重新计数（随命中变化），模型若引用对话历史里的
    // 旧编号会错位。明确告知本轮范围 + 历史编号已失效，杜绝跨轮引用。
    // 上限按"截断后实际可见"的材料编号计算——context 可能被 50k 截断，
    // 尾部材料并未进入 prompt，声明范围过大同样会造成悬空引用。
    let visible_nums = visible_material_numbers(&context);
    let material_note = if visible_nums.is_empty() {
        String::new()
    } else {
        let list = visible_nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
        format!("本轮提供材料编号：{}。", list)
    };
    let system = format!(
        "你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。如果材料不足以回答，请明确说明。\n{material_note}引用材料时在对应内容后标注 [N]（N 为材料编号）。只允许引用本轮列出的编号，未列出的编号一律不得引用。\n\n材料：\n{}{}",
        context, recall_section
    );
    let last_n = messages.len().saturating_sub(1);
    let mut user_msg = if messages.len() > 1 {
        let mut history_str = String::from("对话历史：\n");
        for m in messages.iter().take(last_n) {
            history_str.push_str(&format!("[{}] {}\n",
                if m.role == "user" { "用户" } else { "助手" },
                truncate_text(&m.content, 2000)));
        }
        format!("{}\n当前问题：{}", history_str, last_q)
    } else {
        last_q.clone()
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
        "truncated_to": max_context_chars,
    })));
    // 指代消解基于改写后的 search_q：代词已在改写中展开，避免规则管道二次误判歧义。
    let (clarify_note, clarify_candidates, clarify_slots, clarify_blocking) = {
        use crate::commands::clarify;
        let session_paths = prior_evidence_paths(state, session_id);
        let current: Vec<(String, String)> =
            evidence.iter().map(|e| (e.path.clone(), e.snippet.clone())).collect();
        let conditions: Vec<(String, String)> =
            scope.conditions.iter().map(|c| (c.kind.clone(), c.value.clone())).collect();
        let mut ir = clarify::propose_ir(&search_q);
        if let Some(r) = clarify_reply {
            for b in &r.bindings {
                if !clarify::apply_binding(&mut ir, &b.surface, &b.value) {
                    log::warn!("[AI]   clarify binding missed: surface={} value={}", b.surface, b.value);
                }
            }
        }
        ir.constraints =
            clarify::Constraints::from_scope(&scope.mention_files, &scope.mention_dirs, &conditions);
        let explicit_scope = ir.constraints.scope_items();
        let mut session_state = clarify::build_state(&session_paths, &current, &explicit_scope);
        for (value, stype) in clarify::question_entities(&search_q) {
            session_state.add(&value, &stype, 3);
        }
        let grounding = clarify::build_grounding(&session_paths, &current, &explicit_scope);
        let resolution = clarify::resolve(&ir, &session_state, &grounding);
        log::info!("[AI]   clarify ir={:?} verdict={:?}", ir, resolution.verdict);
        let blocking = clarify_is_blocking(&search_q, &resolution);
        let slots: Vec<ClarifySlot> = clarify::asks_from_resolution(&resolution)
            .into_iter()
            .map(|a| ClarifySlot { surface: a.surface, stype: a.stype, options: a.options })
            .collect();
        // 提示文案按模式分叉：阻塞轮在等用户填槽，非阻塞轮已经把答案给出去了、
        // 只需说明"按谁答的"。候选列表不进文案（含 jieba 粘连碎片，会误导）。
        match clarify::clarify_from_resolution(&resolution) {
            Some((surfaces, c)) => {
                let note = if blocking {
                    format!("（提示：请明确{surfaces}的指代。）请在下方填写具体指代。")
                } else {
                    format!(
                        "（提示：{surfaces}的指代不明确，以下回答按材料中最相关的对象作答。）如需精确，请用 @ 指定文件或目录。"
                    )
                };
                (Some(note), c, slots, blocking)
            }
            None => (None, Vec::new(), Vec::new(), false),
        }
    };
    if let Some(n) = &clarify_note {
        log::info!("[AI]   clarify note: {}", truncate_text(n, 60));
    }
    Ok(PreparedConversation { system, user_msg, source_ids: source_ids_final, source_files: source_files_final, evidence, search_query: search_q, search_terms, clarify_note, clarify_candidates, clarify_slots, clarify_blocking, hits, events, total_match_count: all_hits.len(), has_evidence: !context.trim().is_empty(), visible_nums })
}
/// 是否应「只问不答」：指代未绑定 **且** 提问里没有任何具名实体 —— 无锚点可依时
/// 硬答会用错误的案件作答（RAG_PIPELINE.md 所称"自信地答错"），不如先问清楚。
pub(super) fn clarify_is_blocking(q: &str, res: &crate::commands::clarify::Resolution) -> bool {
    use crate::commands::clarify::{SlotOutcome, Verdict};
    res.verdict == Verdict::Ambiguous
        && crate::commands::clarify::question_entities(q).is_empty()
        && res.outcomes.iter().any(|o| matches!(
            o,
            SlotOutcome::Ask { stype, .. } if stype == "case" || stype == "person"
        ))
}

/// Compose a turn's final text. A blocked clarification turn never ran the LLM, so
/// `raw_text` already IS the note: return it once and skip `cite` — running citation
/// matching over the note attaches bogus `[N]` from entity names it lists.
pub(super) fn compose_answer_text(
    clarify_blocking: bool,
    raw_text: String,
    clarify_note: Option<String>,
    cite: impl FnOnce(&str) -> String,
) -> String {
    if clarify_blocking {
        raw_text
    } else {
        let cited = cite(&raw_text);
        match clarify_note {
            Some(n) => format!("{n}\n\n{cited}"),
            None => cited,
        }
    }
}

pub fn truncate_text(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars].iter().collect()
    }
}

/// jieba-tokenize a retrieval query into matchable terms (len >= 2). Rewrites
/// emit compound phrases (e.g. "不动产资料查询") that never appear verbatim.
pub(super) fn query_terms(query: &str) -> Vec<String> {
    let jieba = crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner());
    jieba.cut(query, true).iter().map(|w| w.word.to_string()).filter(|w| w.chars().count() >= 2).collect()
}

/// Document/procedure type nouns that jieba mis-tags as proper nouns (「申请书」
/// 「通知书」→ `nr`「人名」). A category is never a candidate subject
/// (docs/RAG_PIPELINE.md §⑤). Explicit list, not a suffix rule: …书/…状/…证
/// would also reject real names (严慧书、严颖书、姜绍书 …).
///
/// 这是**sound-but-incomplete** 的前置排除：表内项确非主体，但表不可能全 →
/// 漏掉的由 clarify（State/Grounding 计数）兜底。
pub(crate) const DOC_TYPE_NOUNS: &[&str] = &[
    "申请书", "通知书", "委托书", "判决书", "裁定书", "决定书", "起诉状", "上诉状",
    "答辩状", "律师函", "情况说明", "公告", "传票", "证据", "合同", "协议",
    "证明", "证书", "笔录", "清单", "目录", "模板", "样式", "回执",
];

/// Pick the citation preview from the passage that best matches the query
/// terms, so the hover card / Markdown export show the referenced text rather
/// than the file head. Falls back to the head when no term matches.
pub(super) fn cited_excerpt(text: &str, terms: &[String], max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if terms.is_empty() || chars.len() <= max_chars {
        return truncate_text(text, max_chars);
    }
    let step = (max_chars / 2).max(1);
    let mut best_start = 0usize;
    let mut best_score = 0usize;
    let mut start = 0usize;
    while start < chars.len() {
        let end = (start + max_chars).min(chars.len());
        let window: String = chars[start..end].iter().collect();
        let score: usize = terms.iter().map(|t| window.matches(t.as_str()).count()).sum();
        if score > best_score {
            best_score = score;
            best_start = start;
        }
        if end == chars.len() { break; }
        start += step;
    }
    if best_score == 0 { return truncate_text(text, max_chars); }
    let win_end = (best_start + max_chars).min(chars.len());
    let win: String = chars[best_start..win_end].iter().collect();
    let byte_pos = terms.iter().filter_map(|t| win.find(t.as_str())).min().unwrap_or(0);
    let char_off = win[..byte_pos].chars().count();
    let s = (best_start + char_off).saturating_sub(20);
    let e = (s + max_chars).min(chars.len());
    chars[s..e].iter().collect()
}

/// Inject chunk text honoring a char budget. When the hit came from the
/// chunk-vector channel (`hit_chunks` non-empty), those exact chunks are
/// injected first (semantic evidence), then remaining budget is filled with
/// lexically-relevant chunks. Falls back to truncation without chunks.
///
/// Returns the injected text plus the **source-document char ranges** it
/// covers, so callers can tell whether a given passage reached the prompt.
pub(super) fn chunked_or_truncated_with_budget(
    conn: &rusqlite::Connection,
    md5: &str,
    text: &str,
    query: &str,
    char_budget: usize,
    hit_chunks: &[(usize, f32)],
) -> (String, Vec<(usize, usize)>) {
    if char_budget == 0 { return (String::new(), Vec::new()); }
    let text_full_chars = text.chars().count();
    // 原文超出预算 → 必然无法完整注入。给 LLM 显式标记"材料被截断"，
    // 避免它把注入的部分当作全文下结论（静默截断 → 可感知截断）。
    let truncated_note = if text_full_chars > char_budget {
        format!("\n\n〔注：该材料过长，仅注入部分内容（共 {text_full_chars} 字，未全部展示），据此回答可能不完整。〕")
    } else {
        String::new()
    };
    let head_span = (0usize, text_full_chars.min(char_budget));
    if text_full_chars <= char_budget {
        return (format!("{}{}", truncate_text(text, char_budget), truncated_note), vec![head_span]);
    }
    let chunks = crate::db::chunks::get_chunks(conn, md5).unwrap_or_default();
    if chunks.is_empty() {
        return (format!("{}{}", truncate_text(text, char_budget), truncated_note), vec![head_span]);
    }
    let mut packed: Vec<(String, (usize, usize))> = Vec::new();
    let mut used = 0usize;
    let push = |c: &crate::db::chunks::DocChunk,
                used: &mut usize,
                packed: &mut Vec<(String, (usize, usize))>| {
        let chunk_chars = c.text.chars().count() + 20;
        if *used + chunk_chars > char_budget {
            if packed.is_empty() {
                packed.push((
                    truncate_text(&c.text, char_budget),
                    (c.start_char as usize, c.start_char as usize + char_budget),
                ));
            }
            return false;
        }
        packed.push((
            format!("（第{}-{}字）\n{}", c.start_char, c.end_char, c.text),
            (c.start_char as usize, c.end_char as usize),
        ));
        *used += chunk_chars;
        true
    };
    let finish = |packed: Vec<(String, (usize, usize))>| {
        if packed.is_empty() {
            return (String::new(), Vec::new());
        }
        let spans: Vec<(usize, usize)> = packed.iter().map(|(_, s)| *s).collect();
        let body = packed.into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n···\n");
        (format!("{body}\n···\n{truncated_note}"), spans)
    };
    // 统一排序后打包。命中块不能无条件优先：查询含通用词时多数块都"命中"，
    // 按阅读顺序优先会把预算全花在文档开头，位于文末的答案段反而进不来。
    let (by_relevance, has_lexical_signal) =
        crate::db::chunks::chunks_for_packing(&chunks, query, hit_chunks);
    let order: Vec<&crate::db::chunks::DocChunk> = if has_lexical_signal {
        by_relevance
    } else {
        let mut sims = hit_chunks.to_vec();
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sims.iter()
            .filter_map(|(idx, _)| chunks.iter().find(|c| c.chunk_index == *idx as i64))
            .collect()
    };
    // 文档头块先入包：承载标题/当事人/案号等身份信息，且"这是谁/编号是多少"
    // 一类浅层问题答案常在此。仅在预算够放 4 块时才带——否则它会挤掉一份
    // 小预算文档里唯一的答案块（排名靠后的文档预算本就接近下限）。
    if char_budget >= HEAD_MIN_BUDGET
        && let Some(head) = chunks.first()
        && !push(head, &mut used, &mut packed) {
            return finish(packed);
        }
    for chunk in order {
        if chunk.chunk_index == 0 { continue; }
        if !push(chunk, &mut used, &mut packed) { break; }
    }
    finish(packed)
}
