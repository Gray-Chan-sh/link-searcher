//! Prompt assembly for smart search and multi-turn conversation (RAG):
//! retrieval, evidence selection, budget packing and clarify handling.

use super::*;
use super::rewrite::{parent_person_entities, person_pronoun_surface};
use super::sufficiency::{
    check_sufficiency, expand_neighbor_chunks, expansion_queries, missing_slots,
    SufficiencySignals,
};

/// 查询向量（`cached_embed`）的等待上限：本地嵌入池可能被后台回填占满（bge-large
/// 单次推理 85s 量级），绝不能把整轮聊天卡在"语义扫描中"。超时后各阶段自行退回
/// BM25（见 `ai::cached_embed_with_timeout`）。
///
/// ⚠️ 所有调用 `cached_embed*` 的地方都必须走带超时的版本 —— 之前重排阶段用的是
/// 裸 `cached_embed`，于是"向量阶段已超时退回 BM25、重排阶段却永久卡住"。
pub(super) const QUERY_EMBED_TIMEOUT_SECS: u64 = 20;

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
        &RetrievalCtx::from_state(state), &query.to_lowercase(), 3, crate::ai::embedding_enabled(),
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
                snippet: crate::commands::search::strip_md_inline(&truncate_text(text, 200)),
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
    /// 「这轮凭什么答的」：`history` / `both` / `docs`（空串 = 旧数据/未判定）。
    /// 前端据此在回答上方显示「依据：第 N 轮结论 / N 份材料」。
    pub(crate) basis: String,
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
/// 提示词留痕：system 段只留头部（指令在最前面，材料在后）。
const PROMPT_SNAPSHOT_HEAD: usize = 2_000;
/// 提示词留痕：user 段首尾各留多少字（头部是对话历史，尾部是当前问题）。
const PROMPT_SNAPSHOT_TAIL: usize = 1_000;

/// 回答提示词的**分段模板**（不含材料本体）：`[指令, 材料编号说明与沿用规则 + 材料头]`。
///
/// 用分段常量而不是 `format!` 字面量，是为了让设置页展示的模板与真正拼出来的提示词
/// **同源** —— 另抄一份到文档里，迟早会漂移。拼接顺序：
/// `PARTS[0] + 材料编号说明 + PARTS[1] + 材料正文 + 对话回忆`。
pub(crate) const ANSWER_SYSTEM_PARTS: [&str; 2] = [
    "你是严谨的文档分析助手。优先依据以下材料回答，不臆造事实。\n",
    "引用材料时在对应内容后标注 [N]（N 为本轮材料编号）；只引用本轮列出的编号。\n\
前几轮已经给出的事实（名单/金额/日期/结论）可以直接沿用：需要据此排序、换算、汇总时，\
说明依据哪一轮（如「基于第 2 轮列出的出生日期」）即可，不必重新从材料推导；\
沿用旧结论时不要为它编造 [N]。材料与上文都不足以回答时，明确说明缺什么。\n\
比较类问题（谁大 / 谁更多 / 谁更早 / 谁的钱多）必须**先分别列出双方的事实依据，最后只给一个结论**；\
禁止在开头先下结论、后文再纠正。\"谁大\"按年龄年长作答；若问的其实是出生先后或体量大小，用一句话点明口径。\n\n材料：\n",
];

fn answer_system_prompt(material_note: &str, context: &str, recall: &str) -> String {
    let mut s = String::with_capacity(
        ANSWER_SYSTEM_PARTS[0].len() + material_note.len()
            + ANSWER_SYSTEM_PARTS[1].len() + context.len() + recall.len(),
    );
    s.push_str(ANSWER_SYSTEM_PARTS[0]);
    s.push_str(material_note);
    s.push_str(ANSWER_SYSTEM_PARTS[1]);
    s.push_str(context);
    s.push_str(recall);
    s
}

/// 给设置页用的**人类可读**模板：变量部分标成 `{...}`。
pub(crate) fn answer_system_template_display() -> String {
    let mut s = String::new();
    s.push_str(ANSWER_SYSTEM_PARTS[0]);
    s.push_str("{本轮材料编号说明}");
    s.push_str(ANSWER_SYSTEM_PARTS[1]);
    s.push_str("{材料正文}");
    s.push_str("{对话回忆（可选）}");
    s
}
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

/// 阈值只判「强相关」：短实体查询（2~4 字，如 "深蓝"）与全库任意文档的余弦
/// 普遍落在 0.45~0.52，硬阈值 0.55 会让语义通道整条归零。**强命中为 0 时**
/// 保底放行相似度最高的这么多份弱候选（见
/// [`crate::ai::vector_scan_with_query_emb`]），以低权重进池、由重排决定去留；
/// 已有强命中则不再追加，避免给健康查询制造噪声。
const VECTOR_RELEASE_TOP_K: usize = 50;
/// chunk 向量通道同上的保底放行条数。
const CHUNK_VECTOR_RELEASE_K: usize = 50;
/// 保底放行的弱向量候选（低于阈值）在 RRF 里的权重——低于强命中的 1.0，
/// 避免弱信号与强命中同权把噪声抬进注入。
const WEAK_VECTOR_RRF_WEIGHT: f64 = 0.4;
/// 字符集兜底触发线：BM25 命中少于此值且语义无强命中时启用。
/// 词序颠倒/错别字场景（"深蓝"↔"蓝深"）BM25 通常只命中 0~3 份噪音。
const CHAR_FALLBACK_BM25_MAX: usize = 5;
/// 字符集兜底的实体词最长字数（超出则区分度不足，放弃扫描）。
const CHAR_FALLBACK_MAX_CHARS: usize = 4;
/// 单个实体词的字符共现候选上限；超过即视为泛词（如"上海"），整词放弃。
const CHAR_FALLBACK_MAX_CANDIDATES: usize = 300;
/// 字符集兜底候选在 RRF 里的权重（字面证据强度介于 BM25 与路径匹配之间）。
const CHAR_FALLBACK_RRF_WEIGHT: f64 = 0.5;

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

/// Retrieval + prompt assembly without cancellation — the original signature,
/// kept for the CLI and tests (`docs/LARK_BOT.md` §5.5 stage 1).
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
    // 重跑（卡片「换一批证据」）：上一轮的回答正文。`Some` ⇒ 走"查询扩展 + 重新检索"。
    retry_prior_answer: Option<&str>,
) -> Result<PreparedConversation, String> {
    prepare_conversation_prompt_with_cancel(
        state, messages, source_ids, scope, session_retrieval_scope, strict_docs,
        full_recall, skip_llm_rewrite, app, session_id, clarify_reply, retry_prior_answer, None,
    )
    .await
}

/// `spawn_blocking` 阶段需要独占持有的全部输入（查询改写结果 + 本轮参数）。
/// `spawn_blocking` 要求 `'static`，所以这里都是 owned 值（`messages` 等为
/// 小对象拷贝，几 KB 量级）。
struct AssembleJob {
    messages: Vec<ChatMessage>,
    source_ids: Vec<String>,
    scope: TurnScope,
    session_retrieval_scope: Vec<String>,
    strict_docs: bool,
    full_recall: bool,
    session_id: String,
    clarify_reply: Option<ClarifyReply>,
    last_q: String,
    search_q: String,
    /// 模型判定的专名实体（检索关键词与补查判据的唯一来源）。
    entities: Vec<String>,
    /// 本轮依据：History ⇒ 跳过检索；Both ⇒ 检索但收窄；Docs ⇒ 完整 RAG。
    basis: super::rewrite::AnswerBasis,
    search_terms: Vec<String>,
    rewritten: bool,
    /// 本轮是否依赖上文（模型自评；模型没给时按"有历史"处理）。决定是否沿用上一轮材料。
    context_dependent: bool,
    /// Pronoun follow-up already bound to a parent person entity
    /// (surface, entity). Handed to the clarify engine so it does not report
    /// the same pronoun as ambiguous.
    pronoun_binding: Option<(String, String)>,
    events: Vec<(String, serde_json::Value)>,
    material_order: Vec<String>,
    app: Option<tauri::AppHandle>,
    cancel: Option<std::sync::Arc<crate::ai::CancelToken>>,
}

/// 作答前充分性自检提示词（静态、无插值）。放在模块级是为了让设置页展示
/// **实际生效**的那一份，而不是另抄一份。
pub(crate) const GAP_CHECK_SYSTEM_PROMPT: &str = "你是回答前的充分性审查员。你**只判断现有信息够不够**，不要回答问题，也不要解释。\
给定【问题】【对话历史摘要】【已注入材料摘要】：\
- 若仅凭这些信息就能完整、准确地回答，输出 {\"answerable\":true,\"missing\":[]}；\
- 否则输出 {\"answerable\":false,\"missing\":[\"<还缺的事实>\"]}，missing 用 1~3 个**可在文档库里检索到的名词**表示（如 \"汪均晋 死亡证明\"、\"身份证号\"），不要写\"更多材料\"\"相关文件\"这类空话。\
只输出一行 JSON，不要代码块。";

/// 「先按上文作答、作答前自检」的第二段：判断现有材料/上文是否**足够**回答本轮问题。
///
/// 只做判断，不回答问题；输出一行 JSON：`{"answerable":bool,"missing":[检索词]}`。
/// `missing` 要求是能在卷宗里检索到的名词（人名/文件类型/事项），不是"更多材料"这类空话。
pub(super) struct GapCheck {
    pub answerable: bool,
    pub missing: Vec<String>,
}

/// 同步版（在 `assemble_conversation_prompt` 的 blocking 段里调用）。
///
/// 超时 15 秒、任何失败都**放行**（返回 `None`），绝不能因为一次自检卡住整轮回答。
/// 第二个返回值说明失败原因（`ok` / `llm_disabled` / `gateway_or_timeout` / `parse_failed`），
/// 写进 `gap_check` 事件 —— 此前失败与"判定为够用"在事件里长得一样，无法定位。
pub(super) fn check_context_gap(
    question: &str,
    history_text: &str,
    snippets: &[String],
) -> (Option<GapCheck>, &'static str) {
    if !crate::ai::llm_enabled() {
        return (None, "llm_disabled");
    }
    let mut materials = String::new();
    for (i, s) in snippets.iter().take(12).enumerate() {
        materials.push_str(&format!("[{}] {}\n", i + 1, truncate_text(s, 240)));
    }
    let system = GAP_CHECK_SYSTEM_PROMPT;
    let user = format!(
        "【问题】{question}\n【对话历史摘要】{}\n【已注入材料摘要】\n{materials}\n只输出一行 JSON：",
        truncate_text(history_text, 4000)
    );
    let mut raw: Option<String> = None;
    for attempt in 0..2u8 {
        // 第一次用正常超时；失败后再给一次机会（网关限流/瞬时断连很常见，
        // 一枚 429 就放弃自检等于这道保险形同虚设）。重试用更短超时，避免最坏 30s。
        let budget = if attempt == 0 { GAP_CHECK_TIMEOUT_SECS } else { 6 };
        if let Some(r) = crate::ai::chat_with_timeout(&system.to_string(), &user, budget) {
            raw = Some(r);
            break;
        }
        if attempt == 0 {
            log::warn!("[AI]   gap_check 第一次失败（限流/断连/超时），重试一次");
            std::thread::sleep(std::time::Duration::from_millis(1200));
        }
    }
    let Some(raw) = raw else {
        return (None, "gateway_or_timeout");
    };
    match parse_gap_reply(&raw) {
        Some(g) => (Some(g), "ok"),
        None => (None, "parse_failed"),
    }
}

/// 自检超时（秒）。网关常态 4~15s，超过就放弃补查、按上文作答。
const GAP_CHECK_TIMEOUT_SECS: u64 = 15;

fn parse_gap_reply(raw: &str) -> Option<GapCheck> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Reply {
        #[serde(default)]
        answerable: bool,
        #[serde(default)]
        missing: Vec<String>,
    }
    let reply: Reply = serde_json::from_str(&raw[start..=end]).ok()?;
    let missing = reply
        .missing
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(3)
        .collect();
    Some(GapCheck { answerable: reply.answerable, missing })
}

/// 补查查询串：缺失事实优先，实体做锚点；都没有就用原查询。
fn gap_query(missing: &[String], entities: &[String], fallback: &str) -> String {
    if missing.is_empty() {
        fallback.to_string()
    } else {
        format!("{} {}", entities.join(" "), missing.join(" "))
            .trim()
            .to_string()
    }
}

#[cfg(test)]
mod gap_check_tests {
    use super::*;

    #[test]
    fn parses_answerable() {
        let g = parse_gap_reply(r#"{"answerable":true,"missing":[]}"#).expect("json");
        assert!(g.answerable);
        assert!(g.missing.is_empty());
    }

    #[test]
    fn parses_missing_terms_and_caps_three() {
        let g = parse_gap_reply(
            "```json\n{\"answerable\":false,\"missing\":[\"汪均晋 死亡证明\",\" \",\"户口注销\",\"身份证号\",\"多余\"]}\n```",
        )
        .expect("json");
        assert!(!g.answerable);
        assert_eq!(g.missing.len(), 3);
        assert_eq!(g.missing[0], "汪均晋 死亡证明");
    }

    #[test]
    fn invalid_reply_is_none() {
        // 没有 JSON 对象 → 判不了 → 调用方按"放行"处理
        assert!(parse_gap_reply("答案够用").is_none());
        // 空对象是合法 JSON：answerable=false、missing 为空 → 会用原查询补查
        assert!(parse_gap_reply("{}").is_some());
    }

    #[test]
    fn gap_query_uses_missing_terms_with_entities_as_anchor() {
        let q = gap_query(
            &["死亡证明".to_string()],
            &["汪均晋".to_string()],
            "无关的原查询",
        );
        assert_eq!(q, "汪均晋 死亡证明");
    }

    #[test]
    fn gap_query_falls_back_when_nothing_missing() {
        assert_eq!(gap_query(&[], &["汪均晋".to_string()], "原查询"), "原查询");
    }
}

#[cfg(test)]
mod prompt_snapshot_tests {
    use super::*;

    #[test]
    fn tail_text_keeps_the_question_at_the_end() {
        let s = format!("{}\n当前问题：他和陈秀芳谁大？", "历史".repeat(500));
        let t = tail_text(&s, 100);
        assert!(t.ends_with("当前问题：他和陈秀芳谁大？"));
        assert_eq!(t.chars().count(), 100);
    }

    #[test]
    fn tail_text_returns_short_string_unchanged() {
        assert_eq!(tail_text("短", 100), "短");
    }

    #[test]
    fn digest_is_stable_and_differs_on_change() {
        assert_eq!(prompt_digest("abc"), prompt_digest("abc"));
        assert_ne!(prompt_digest("abc"), prompt_digest("abd"));
        assert_eq!(prompt_digest("abc").chars().count(), 8);
        assert_eq!(prompt_digest(""), "cbf29ce4");
    }

    #[test]
    fn answer_template_keeps_every_variable_part() {
        let s = answer_system_prompt("本轮提供材料编号：1。", "【材料】正文", "\n\n回忆");
        assert!(s.contains("本轮提供材料编号：1。"));
        assert!(s.contains("【材料】正文"));
        assert!(s.ends_with("\n\n回忆"));
        // 展示用的模板必须把变量位置标出来，否则设置页看到的是"少了东西"的提示词
        let disp = answer_system_template_display();
        assert!(disp.contains("{本轮材料编号说明}"));
        assert!(disp.contains("{材料正文}"));
        assert!(disp.contains("{对话回忆"));
    }
}

/// 合并"补查新增命中"与已有排序，返回 `(前置份数, 合并后的列表)`。
///
/// 补查是**补召回**，不是重排：只有路径里真的含缺失实体的命中可以前置（封顶
/// `front_max`），其余一律追加到队尾 —— 已有的排序（BM25 名次 + 重排结果）优先。
///
/// 2026-11-06 实测：原先无条件 `new_hits.extend(all_hits.drain(..))`，158 个只含
/// 泛词「案件」的文书被整体前置，注入上限 30 全被它们占满，而 BM25 第 1~28 名的
/// 王会芹卷宗被挤到 158 名之后，模型只能答"材料未提供"。
pub(super) fn merge_supplementary_hits(
    existing: Vec<ScoredHit>,
    new_hits: Vec<ScoredHit>,
    entities: &[String],
    front_max: usize,
) -> (usize, Vec<ScoredHit>) {
    let mut front: Vec<ScoredHit> = Vec::new();
    let mut tail: Vec<ScoredHit> = Vec::new();
    for hit in new_hits {
        let path_hit =
            !entities.is_empty() && entities.iter().any(|e| hit.path.contains(e.as_str()));
        if path_hit && front.len() < front_max {
            front.push(hit);
        } else {
            tail.push(hit);
        }
    }
    let promoted = front.len();
    front.extend(existing);
    front.extend(tail);
    (promoted, front)
}

#[cfg(test)]
mod supplementary_merge_tests {
    use super::*;

    fn hit(id: &str, path: &str) -> ScoredHit {
        ScoredHit {
            file_id: id.to_string(),
            path: path.to_string(),
            bm25_score: None,
            semantic_score: None,
            rrf_score: None,
            from_history: false,
            from_chunk: false,
            hit_chunks: Vec::new(),
        }
    }

    /// 回归（2026-11-06）：补查命中里没有实体命中时，已有排序必须原封不动。
    #[test]
    fn supplementary_hits_never_displace_existing_order() {
        let existing = vec![hit("a", "案件/WHQ/判决书.pdf"), hit("b", "案件/WHQ/上诉状.pdf")];
        let new = vec![hit("x", "案件/未结案清单.txt"), hit("y", "其他案件/受理通知书.pdf")];
        let (promoted, merged) =
            merge_supplementary_hits(existing, new, &["王会芹".to_string()], 10);
        assert_eq!(promoted, 0);
        assert_eq!(
            merged.iter().map(|h| h.file_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "x", "y"]
        );
    }

    /// 路径真的含实体的补查命中可以前置，但封顶，且不能吃掉已有排序。
    #[test]
    fn entity_path_hits_promoted_but_capped() {
        let existing = vec![hit("a", "案件/WHQ/判决书.pdf")];
        let new: Vec<_> = (0..15)
            .map(|i| hit(&format!("p{i}"), "案件/WHQ 王会芹/材料.pdf"))
            .collect();
        let (promoted, merged) =
            merge_supplementary_hits(existing, new, &["王会芹".to_string()], 10);
        assert_eq!(promoted, 10);
        assert_eq!(merged.len(), 16);
        assert_eq!(merged[10].file_id, "a", "已有结果必须排在前置命中之后、其余补查之前");
    }

    /// 模型没给实体时，任何补查命中都不允许前置。
    #[test]
    fn no_entities_promotes_nothing() {
        let existing = vec![hit("a", "p/a")];
        let new = vec![hit("x", "王会芹/判决.pdf")];
        let (promoted, merged) = merge_supplementary_hits(existing, new, &[], 10);
        assert_eq!(promoted, 0);
        assert_eq!(merged[0].file_id, "a");
    }
}

/// [`prepare_conversation_prompt`] plus a per-generation [`CancelToken`], so a
/// cancel that arrives during retrieval aborts the pipeline instead of letting
/// it hand work to the LLM afterwards.
///
/// ⚠️ 本函数分两段执行：**查询改写**（含两次 LLM 调用）留在 async 上下文；
/// **检索 + 组装**整体交给 `spawn_blocking`（`assemble_conversation_prompt`）。
/// 后者是纯同步重活（BM25 + 全库向量扫描 + chunk 精检 + 远程 rerank），debug
/// 构建实测 30~60s —— 以前它直接跑在 async worker 上，会把 tokio worker 钉死
/// 数十秒，表现为「发问后界面长时间零输出」。
pub(crate) async fn prepare_conversation_prompt_with_cancel(
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
    // 重跑（卡片「换一批证据」）：上一轮的回答正文。`Some` ⇒ 走"查询扩展 + 重新检索"。
    retry_prior_answer: Option<&str>,
    cancel: Option<std::sync::Arc<crate::ai::CancelToken>>,
) -> Result<PreparedConversation, String> {
    let ctx = RetrievalCtx::from_state(state);
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
    let last_q = question_from_messages(messages);
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
    let material_order = prior_material_order(&ctx, session_id);
    // 聊天前连通性预检：embedding / rerank 各发一次最小请求（结果缓存 2 分钟）。
    // 失败不阻断回答，但**必须说出来**并让本轮跳过该通道 ——
    // 否则网关 404 会表现成"答案总是缺东西"，而日志里只有每几秒一条的 404。
    if !skip_llm_rewrite
        && crate::ai::llm_enabled()
        && let Ok(health) = tokio::task::spawn_blocking(crate::ai::preflight_channels).await
    {
        for (name, st) in [("embedding", &health.embedding), ("rerank", &health.rerank)] {
            if !st.ok {
                emit_progress(
                    "capability",
                    &format!("{name} 通道不可用，本轮跳过（{}）", st.detail),
                    0,
                    0,
                );
            }
        }
        events.push(("capability".into(), serde_json::json!({
            "llm": { "ok": health.llm.ok, "detail": health.llm.detail },
            "embedding": { "ok": health.embedding.ok, "detail": health.embedding.detail },
            "rerank": { "ok": health.rerank.ok, "detail": health.rerank.detail },
        })));
    }
    emit_progress("query_rewrite", "查询改写中...", 0, 0);
    // 上一条 user 消息（父问句）：只用于把代词绑定给 clarify 引擎。
    let parent_q = messages
        .iter()
        .rev()
        .filter(|m| m.role == "user")
        .map(|m| m.content.trim())
        .find(|c| !c.is_empty() && *c != last_q.trim())
        .unwrap_or("");
    let is_retry = retry_prior_answer.is_some_and(|s| !s.trim().is_empty());
    // 重跑（卡片「换一批证据」）：问题仍是**原问题**（`question_from_messages` 保证），
    // 但检索词要换一批 —— 首轮用原词没找到能回答的材料，重跑的意义就在"换组词再搜"。
    // 扩展失败一律用原问句检索（宁可答得少，也不能不答；更不能原词再搜一遍假装重跑）。
    let retry_q = if is_retry {
        let prior = retry_prior_answer.unwrap_or("").trim().to_string();
        llm_expand_retry_query(&last_q, &prior).await
    } else {
        None
    };
    if is_retry {
        match &retry_q {
            Some(q) => log::info!("[AI]   retry 扩查：\"{}\" → \"{}\"", truncate_text(&last_q, 30), truncate_text(q, 40)),
            None => log::info!("[AI]   retry 扩查未产出（用原问句检索）：\"{}\"", truncate_text(&last_q, 30)),
        }
    }
    // 代词绑定只用于**给 clarify 引擎提供绑定**（避免回答前多一句"指代不明确"提示），
    // 不短路检索：检索词一律交给 LLM 结合"之前的提问和回答"改写。
    let pronoun_binding: Option<(String, String)> = person_pronoun_surface(&last_q)
        .zip(parent_person_entities(parent_q).into_iter().next())
        .map(|(surface, entity)| (surface.to_string(), entity));
    let mut rewrite_fallback: Option<String> = None;
    // 模型自评「这句话离开上文读不懂」；None = 模型没给（旧格式/没走改写）。
    let mut self_contained: Option<bool> = None;
    // 模型判定的专名实体：检索关键词与"要不要补查"都以它为准，不再用任何词表。
    let mut llm_entities: Vec<String> = Vec::new();
    // `both` 聚焦字段：本轮还缺的事实 / 该去谁的材料里找（模型判定）。
    let mut llm_needs: Vec<String> = Vec::new();
    let mut llm_anchor: Vec<String> = Vec::new();
    let mut refocused = false;
    // 本轮依据：由模型在改写时判定；任何回退路径都退回 Docs（完整检索，宁可多查）。
    let mut basis = super::rewrite::AnswerBasis::Docs;
    let (search_q, rewrite_method) = if let Some(q) = retry_q {
        (q, "retry")
    } else if skip_llm_rewrite || !crate::ai::llm_enabled() {
        rewrite_fallback = Some(if skip_llm_rewrite {
            "llm rewrite skipped".into()
        } else {
            "llm unavailable".into()
        });
        // 不改写就用用户原话检索：这是"用户怎么说就怎么搜"，不是用词典替他换词。
        (last_q.clone(), "none")
    } else {
        let context_paths = prior_evidence_paths(&ctx, session_id);
        match llm_rewrite_query(&last_q, messages, &context_paths).await {
            super::rewrite::RewriteAttempt::Ok(ok) => {
                self_contained = ok.self_contained;
                llm_entities = ok.entities;
                basis = ok.basis;
                llm_needs = ok.needs;
                llm_anchor = ok.anchor;
                // ── both 聚焦（2026-11-07） ──────────────────────────────
                // 「他和陈秀芳谁大」这类追问，改写会把两个名字都放进 entities，
                // 而 BM25 用 entities 做 OR 查询 —— 旧对象是超高频实体，实测 12 个
                // 注入名额被它占掉 9 个。模型给的 anchor（该去谁的材料里找）+ needs
                // （还缺什么）把检索收窄到"真正缺的那点事实"；anchor 为空则不动。
                let mut q_for_search = ok.query.clone();
                if let Some((focused, anchor_kws)) = super::rewrite::refocus_query(
                    basis, &q_for_search, &llm_needs, &llm_anchor)
                {
                    log::info!(
                        "[AI]   refocus(both): \"{}\" → \"{}\" (anchor={:?}, needs={:?})",
                        truncate_text(&q_for_search, 40), truncate_text(&focused, 40),
                        llm_anchor, llm_needs
                    );
                    q_for_search = focused;
                    llm_entities = anchor_kws;
                    refocused = true;
                }
                // 用**语料事实**过滤改写词：正文里一次都没出现过的词直接丢弃
                // （编造的人名自动出局；「兄长/家兄」等同义词只要语料有就保留）。
                // 纯追问（basis=history）不需要可检索词，跳过这道闸。
                let filtered = if basis == super::rewrite::AnswerBasis::History {
                    Some(q_for_search.clone())
                } else {
                    super::rewrite::strip_nonexistent_terms(&q_for_search, &last_q, |t| {
                        super::retrieval::term_doc_freq(&ctx, t)
                    })
                };
                match filtered {
                    Some(q) => (q, "llm"),
                    None => {
                        log::warn!("[AI]   llm rewrite 的词在语料里全不存在，改用原问句检索");
                        rewrite_fallback = Some("no term exists in corpus".into());
                        (last_q.clone(), "none")
                    }
                }
            }
            super::rewrite::RewriteAttempt::Invalid(why) => {
                log::info!("[AI]   llm rewrite 无效（改用原问句检索）：{why}");
                rewrite_fallback = Some(format!("invalid: {why}"));
                (last_q.clone(), "none")
            }
            super::rewrite::RewriteAttempt::Timeout(secs) => {
                log::warn!("[AI]   llm rewrite 超时 {secs}s（改用原问句检索）");
                rewrite_fallback = Some(format!("timeout {secs}s"));
                (last_q.clone(), "none")
            }
            super::rewrite::RewriteAttempt::Unavailable => {
                rewrite_fallback = Some("llm unavailable".into());
                (last_q.clone(), "none")
            }
        }
    };
    // 是否需要沿用上一轮材料：**以模型自评为准**（它看得到完整对话，
    // 兄长/家兄/各人/办公室 对它都是普通语义）；模型没给时按结构判断 ——
    // 有历史就沿用（不涉及任何词表）。
    let context_dependent = match self_contained {
        Some(v) => !v,
        None => messages.iter().filter(|m| m.role == "user").count() > 1,
    };
    let rewritten = search_q != last_q.trim();
    let search_terms = query_terms(&search_q);
    log::info!("[AI]   rewrite: \"{}\" → \"{}\" (rewritten={}, method={}, basis={:?}, entities={:?})", truncate_text(&last_q, 30), truncate_text(&search_q, 30), rewritten, rewrite_method, basis, llm_entities);
    events.push(("query_rewrite".into(), serde_json::json!({
        "original": last_q,
        "rewritten": search_q,
        "was_rewritten": rewritten,
        "rewrite_method": if rewritten { rewrite_method } else { "none" },
        "basis": format!("{basis:?}").to_lowercase(),
        "retry": is_retry,
        // both 聚焦：needs=还缺什么，anchor=该去谁的材料里找；refocused=true 表示
        // 检索词与 BM25 实体已被收窄（旧对象不再重查）。
        "needs": llm_needs,
        "anchor": llm_anchor,
        "refocused": refocused,
        // 为什么没走 LLM 改写：以前这里静默成 method=rule，网关断连与守卫弃用看起来一样。
        "fallback_reason": rewrite_fallback,
    })));

    // ── 检索 + 组装：同步重活，整体挪出 async worker ─────────────────────
    // 复用 `ai.rs` 里 LLM 调用同款的 `spawn_blocking` 范式。此前这段直接
    // `.await` 在命令体内，debug 构建下要把一个 tokio worker 钉死 30~60s。
    let job = AssembleJob {
        messages: messages.to_vec(),
        source_ids: source_ids.to_vec(),
        scope: scope.clone(),
        session_retrieval_scope: session_retrieval_scope.to_vec(),
        strict_docs,
        full_recall,
        session_id: session_id.to_string(),
        clarify_reply: clarify_reply.cloned(),
        last_q,
        search_q,
        entities: llm_entities,
        basis,
        search_terms,
        rewritten,
        context_dependent,
        pronoun_binding,
        events,
        material_order,
        app: app.cloned(),
        cancel,
    };
    tokio::task::spawn_blocking(move || assemble_conversation_prompt(&ctx, job))
        .await
        .map_err(|e| format!("检索任务异常退出: {e}"))?
}

/// [`prepare_conversation_prompt_with_cancel`] 的阻塞段：纯同步检索 + prompt 组装。
fn assemble_conversation_prompt(
    ctx: &RetrievalCtx,
    job: AssembleJob,
) -> Result<PreparedConversation, String> {
    let AssembleJob {
        messages, source_ids, scope, session_retrieval_scope, strict_docs, full_recall,
        session_id, clarify_reply, last_q, search_q, entities: llm_entities, basis,
        search_terms, rewritten, context_dependent, pronoun_binding, mut events,
        mut material_order, app, cancel,
    } = job;
    let emit_progress = |phase: &str, message: &str, current: usize, total: usize| {
        if let Some(a) = &app {
            let _ = a.emit("ai-progress", AiProgress {
                session_id: session_id.clone(),
                phase: phase.to_string(),
                message: message.to_string(),
                current,
                total,
            });
        }
    };
    macro_rules! check_cancel {
        () => {
            if cancel.as_deref().is_some_and(crate::ai::CancelToken::is_cancelled) {
                return Err("请求已取消".into());
            }
        };
    }

    // 从 scope 提取检索过滤参数
    let mut dir_ids: Vec<String> = Vec::new();
    let mut path_prefixes: Vec<String> = Vec::new();
    let mut scope_file_resolved: Vec<(String, String)> = Vec::new();
    {
        let conn = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
        for dir_path in &session_retrieval_scope {
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
        let conn = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
        let dirs = crate::db::dir_config::list_dirs(&conn).map_err(|e| format!("db error: {e}"))?;
        dirs.into_iter().map(|d| (d.id, d.path)).collect()
    };
    {
        let conn = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
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
        let conn = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
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
    // 通道台账：写进 `retrieval_fingerprint` 事件，让"两次回答为什么不同"可二分
    // （指纹相同 → LLM 采样；指纹不同 → 检索层）。
    let mut bm25_returned: usize = 0;
    let mut vector_returned: usize = 0;
    let mut chunk_returned: usize = 0;
    let mut path_returned: usize = 0;
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
    // 阈值之外保底放行的弱候选条数 / 权重（见 VECTOR_RELEASE_TOP_K）。
    let vector_release_k = env_usize("LINK_SEARCHER_VECTOR_RELEASE_K", VECTOR_RELEASE_TOP_K);
    let chunk_vector_release_k = env_usize("LINK_SEARCHER_CHUNK_VECTOR_RELEASE_K", CHUNK_VECTOR_RELEASE_K);
    let weak_vector_weight =
        env_f32("LINK_SEARCHER_RRF_WEAK_VECTOR_WEIGHT", WEAK_VECTOR_RRF_WEIGHT as f32) as f64;
    // 字符集兜底触发线（0 = 关闭）。
    let char_fallback_max = env_usize("LINK_SEARCHER_CHAR_FALLBACK_MAX", CHAR_FALLBACK_BM25_MAX);
    let char_fallback_weight =
        env_f32("LINK_SEARCHER_RRF_CHAR_WEIGHT", CHAR_FALLBACK_RRF_WEIGHT as f32) as f64;
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
        && crate::ai::embedding_enabled()
        && crate::ai::embedding_reachable();
    let use_rrf = std::env::var("LINK_SEARCHER_FUSION")
        .map(|v| !v.eq_ignore_ascii_case("mix"))
        .unwrap_or(true);
    let mut rrf_acc: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let trace_ids_v = trace_target_ids(&ctx, &std::env::var("LINK_SEARCHER_TRACE_TARGET").unwrap_or_default());
    // 检索关键词 = **模型判定的专名实体**（2026-11-06 起）。
    //
    // 这里不再用 jieba + 停用词表二次加工：此前 LLM 写出「王会芹 案件 始末」，
    // 代码又把它切成 ["王会芹","始末"]，于是"用户能不能得到答案"取决于某个词
    // 恰好在不在停用词表里（「情况」在、「始末」不在）。谁有区分度由模型决定。
    let entity_kws = llm_entities.clone();
    let mut final_kws = entity_kws.clone();
    let vocab_rewrite_enabled = std::env::var("LINK_SEARCHER_VOCAB_REWRITE")
        .map(|v| !v.eq_ignore_ascii_case("off"))
        .unwrap_or(false);
    if vocab_rewrite_enabled
        && let Ok(c) = ctx.db.get()
            && let Some(llm_kws) = crate::ai::vocabulary_rewrite(&search_q, &c) {
                for k in llm_kws {
                    if !final_kws.contains(&k) {
                        final_kws.push(k);
                    }
                }
                log::info!("[AI]   vocab_rewrite merged kws: {:?}", final_kws);
            }
    let bm25_query = if final_kws.is_empty() {
        search_q.clone()
    } else {
        final_kws.join(" OR ")
    };
    log::info!("[AI]   final_kws={:?} (entities={:?}) bm25_query=\"{}\"", final_kws, entity_kws, truncate_text(&bm25_query, 60));
    // 纯追问（basis=history）：模型判定"只靠前几轮的信息就能答"，
    // 跳过整条检索（BM25/向量/chunk/路径/渐进补查/重排），把上文升格为材料。
    // 2026-11-06 实测：4 轮会话里 t2/t3 都是纯追问，却各跑了 1171/1256 份候选，
    // 单轮 14~38s，而 t2 的回答里已经列出全部出生日期。
    let mut skip_retrieval = basis == super::rewrite::AnswerBasis::History;
    if skip_retrieval {
        log::info!("[AI]   basis=history → skip retrieval (no BM25/vector/rerank this turn)");
        emit_progress("retrieval", "本轮复用上文结论，跳过检索", 0, 0);
    }
    // 语义通道里「强相关」（过阈值）的命中数：字符集兜底的触发条件之一。
    let mut strong_vector_hits = 0usize;
    if !skip_retrieval && !last_q.trim().is_empty() {
        check_cancel!();
        emit_progress("bm25", "BM25 检索中...", 0, 0);
        let total_files = {
            let c = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
            crate::db::tracker::count_active_files(&c).map_err(|e| e.to_string())?
        };
        let bm25_outcome = super::retrieval::bm25_relevant_hits_with_total(
            &ctx, &bm25_query, (total_files as usize).max(500), inner_semantic_fuse,
            dir_ids_opt.clone(), ext_filter.clone(), date_from, date_to, None, mention_file_ids.clone(),
        )
        .unwrap_or_else(|e| {
            // 通道失败必须留痕：以前 `unwrap_or_default()` 会让"检索炸了"看起来像"没命中"。
            log::warn!("[AI]   bm25 channel failed: {e}");
            super::retrieval::Bm25Outcome { hits: Vec::new(), matched_total: 0 }
        });
        let bm25_matched_total = bm25_outcome.matched_total;
        let bm25_hits = bm25_outcome.hits;
        let bm25_count = bm25_hits.len();
        bm25_returned = bm25_count;
        // 词法通道自检：无 scope 限定时，返回数远低于索引报告的字面命中数 = 降级。
        // 2026-11-02 实测过一次 7003 → 1（重启后 6 分钟，随后自愈），当时无迹可寻。
        let bm25_unscoped = dir_ids_opt.as_ref().is_none_or(|v| v.is_empty())
            && mention_file_ids.as_ref().is_none_or(|v| v.is_empty())
            && ext_filter.as_ref().is_none_or(|v| v.is_empty())
            && date_from.is_none()
            && date_to.is_none();
        if bm25_unscoped
            && bm25_matched_total >= 50
            && (bm25_count as u64) * 100 < bm25_matched_total
        {
            log::warn!(
                "[AI]   retrieval anomaly: bm25 returned {} of matched_total={} (q=\"{}\")",
                bm25_count, bm25_matched_total, truncate_text(&search_q, 40)
            );
            events.push(("retrieval_anomaly".into(), serde_json::json!({
                "channel": "bm25",
                "returned": bm25_count,
                "matched_total": bm25_matched_total,
                "search_query": search_q,
            })));
        }
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
        if crate::ai::embedding_enabled() && crate::ai::embedding_reachable() && !has_locked_scope {
            emit_progress("vector", "语义扫描中...", 0, 0);
            log::info!("[AI]   about to call vector_full_scan");
            let c = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
            // 语义查询用**改写后的完整问句**：把问句压成实体词会让查询向量
            // 退化成几个孤立汉字（"深蓝公司"→"深蓝"），余弦普遍低于阈值，
            // 语义通道等于被关掉。实体词本就在 search_q 内，无需再拼。
            let vec_query = if search_q.trim().is_empty() { final_kws.join(" ") } else { search_q.clone() };
            // 只嵌入一次，文件级与 chunk 级两个向量通道共享同一查询向量
            // （debug 下 bge-large 单次推理 85s，重复嵌入翻倍浪费）。
            // cached_embed：同一/近似查询追问直接命中，跳过本地 BGE 推理。
            // 超时兜底：本地嵌入池被后台回填占满时最多等 20s，之后退回 BM25，
            // 绝不把整轮聊天卡在"语义扫描中"（见 ai::cached_embed_with_timeout）。
            let t_embed = std::time::Instant::now();
            let query_emb =
                crate::ai::cached_embed_with_timeout(&vec_query, QUERY_EMBED_TIMEOUT_SECS);
            log::info!("[AI]   query embed timing: {}ms (hit={})", t_embed.elapsed().as_millis(), query_emb.is_some());
            if query_emb.is_none() {
                log::warn!(
                    "[AI]   query embedding timed out (>{QUERY_EMBED_TIMEOUT_SECS}s); skipping vector channels, falling back to BM25"
                );
                emit_progress("vector", "语义嵌入超时，已回退 BM25", 0, 0);
            }
            if let Some(qe) = &query_emb {
                if let Ok(vec_hits) = crate::ai::vector_scan_with_query_emb(&c, qe, vector_threshold, vector_release_k) {
                    log::info!("[AI]   vector_full_scan returned {} hits", vec_hits.len());
                    vector_returned = vec_hits.len();
                    for (rank, (fid, sim)) in vec_hits.into_iter().enumerate() {
                        // 过阈值=强命中（满权重）；保底放行的弱候选降权，避免
                        // 与强命中同权把噪声抬进注入。
                        let strong = sim >= vector_threshold;
                        if strong {
                            strong_vector_hits += 1;
                        }
                        rrf_add(&mut rrf_acc, &fid, rank, if strong { 1.0 } else { weak_vector_weight });
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
                // 这一步要遍历数万 chunk 的向量做余弦，是"语义扫描"之后的第二个
                // 静默长阶段（日志里只有事后的一行 chunk_vector_scan），补事件。
                emit_progress("vector", "块级语义精检中...", 0, 0);
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
                        crate::ai::chunk_vector_scan_with_query_emb(&c, qe, chunk_threshold, chunk_top_k, chunk_vector_release_k)
                    } else {
                        crate::ai::chunk_vector_scan_for_md5s(&c, qe, &md5s, chunk_threshold, chunk_top_k, chunk_vector_release_k)
                    }
                } {
                    log::info!("[AI]   chunk_vector_scan returned {} hits", chunk_hits.len());
                    chunk_returned = chunk_hits.len();
                    let mut by_md5: std::collections::HashMap<String, Vec<(usize, f32)>> = std::collections::HashMap::new();
                    let mut md5_first_rank: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                    for (rank, (md5, idx, sim)) in chunk_hits.into_iter().enumerate() {
                        md5_first_rank.entry(md5.clone()).or_insert(rank);
                        by_md5.entry(md5).or_default().push((idx, sim));
                    }
                    for (md5, chunks) in by_md5 {
                        let Ok(file_ids) = crate::db::tracker::get_files_by_md5(&c, &md5) else { continue };
                        let first_rank = md5_first_rank.get(&md5).copied().unwrap_or(0);
                        // 块相似度过阈=强命中；保底放行的弱块降权（与文件级一致）。
                        let best = chunks.iter().map(|(_, s)| *s).fold(0.0_f32, f32::max);
                        let chunk_w =
                            if best >= chunk_threshold { chunk_rrf_weight } else { weak_vector_weight };
                        for fid in file_ids {
                            rrf_add(&mut rrf_acc, &fid, first_rank, chunk_w);
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
            emit_progress("vector", &format!("语义扫描完成，累计 {} 份", all_hits.len()), all_hits.len(), all_hits.len());
        }
        let c = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
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
            path_returned = path_hits.len();
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
        // C. 字符集兜底（治词序颠倒 / 错别字）：BM25 命中极少且语义无强命中
        // → 问句里的实体词很可能在文档里换了词序/写法（"深蓝"↔"蓝深"）。
        // 字面与语义都救不回时，退化为最宽松的"字符共现"（正文含实体词的
        // 全部汉字，不要求相邻），把倒序写法捞回候选池。
        if char_fallback_max > 0
            && !final_kws.is_empty()
            && bm25_count < char_fallback_max
            && strong_vector_hits == 0
        {
            let c = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
            let cands = char_cooccur_candidates(
                &c, &final_kws, CHAR_FALLBACK_MAX_CHARS, CHAR_FALLBACK_MAX_CANDIDATES,
            );
            log::info!("[AI]   char_fallback: kws={:?} -> {} candidates", final_kws, cands.len());
            for (rank, (fid, path)) in cands.into_iter().enumerate() {
                rrf_add(&mut rrf_acc, &fid, rank, char_fallback_weight);
                if all_seen.insert(fid.clone()) {
                    all_hits.push(ScoredHit {
                        file_id: fid, path, bm25_score: None, semantic_score: None,
                        rrf_score: None, from_history: false, from_chunk: false,
                        hit_chunks: Vec::new(),
                    });
                }
            }
        }
        {
            let c = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
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

        // ── Progressive retrieval (P4.3) ────────────────────────────────
        // The first multi-channel pass is intentionally broad but can still be
        // sparse (short entity query, unusual wording, OCR variance). Give it
        // one bounded lexical second chance *before* rerank/injection. Scope is
        // never widened here: every supplementary hit goes through the same
        // `hit_in_scope` predicate below.
        let progressive_mode = std::env::var("LINK_SEARCHER_PROGRESSIVE")
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_else(|_| "on".into());
        let progressive_enabled = progressive_mode != "off";
        let progressive_shadow = progressive_mode == "shadow";
        let progressive_min_hits = std::env::var("LINK_SEARCHER_PROGRESSIVE_MIN_HITS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(5);
        let progressive_max_rounds = std::env::var("LINK_SEARCHER_PROGRESSIVE_MAX_ROUNDS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .min(2);
        let mut progressive_rounds = 0usize;
        let mut progressive_tried: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut suff_signals = SufficiencySignals {
            hit_count: all_hits.len(),
            min_hits: progressive_min_hits,
            top_rerank_score: None,
            min_rerank_score: 0.0,
            has_strong_lexical: all_hits
                .iter()
                .any(|h| h.bm25_score.is_some_and(|s| s > 0.0)),
            has_strong_vector: all_hits.iter().any(|h| h.semantic_score.is_some()),
            ..Default::default()
        };
        let mut suff_report = check_sufficiency(&suff_signals);
        events.push(("sufficiency_check".into(), serde_json::json!({
            "round": 0,
            "hit_count": suff_signals.hit_count,
            "sufficient": suff_report.sufficient,
            "reasons": &suff_report.reasons,
            "suggested": &suff_report.suggested,
        })));
        while progressive_enabled
            && !progressive_shadow
            && !suff_report.sufficient
            && progressive_rounds < progressive_max_rounds
        {
            check_cancel!();
            let variants: Vec<String> = expansion_queries(&last_q, &search_q, &final_kws)
                .into_iter()
                .filter(|q| progressive_tried.insert(q.clone()))
                .collect();
            if variants.is_empty() {
                break;
            }
            emit_progress(
                "progressive",
                &format!("补充检索... 第 {} 轮（{} 个查询变体）", progressive_rounds + 1, variants.len()),
                0,
                0,
            );
            let mut added = 0usize;
            for variant in &variants {
                check_cancel!();
                match bm25_relevant_hits(
                    &ctx,
                    variant,
                    500,
                    false,
                    dir_ids_opt.clone(),
                    ext_filter.clone(),
                    date_from,
                    date_to,
                    None,
                    mention_file_ids.clone(),
                ) {
                    Ok(hits) => {
                        for (rank, hit) in hits.into_iter().enumerate() {
                            rrf_add(&mut rrf_acc, &hit.file_id, rank, 0.5);
                            if all_seen.insert(hit.file_id.clone()) {
                                all_hits.push(hit);
                                added += 1;
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("[AI]   progressive: variant bm25 failed: {e}");
                    }
                }
            }
            // Supplementary BM25 still runs over the global index; re-apply the
            // same hard scope before anything can reach the prompt.
            all_hits.retain(|h| {
                hit_in_scope(
                    &h.file_id,
                    &h.path,
                    path_prefixes_opt.as_deref(),
                    mention_file_ids.as_deref(),
                )
            });
            progressive_rounds += 1;
            suff_signals.hit_count = all_hits.len();
            suff_signals.has_strong_lexical = all_hits
                .iter()
                .any(|h| h.bm25_score.is_some_and(|s| s > 0.0));
            suff_signals.has_strong_vector = all_hits.iter().any(|h| h.semantic_score.is_some());
            suff_report = check_sufficiency(&suff_signals);
            events.push(("progressive_round".into(), serde_json::json!({
                "round": progressive_rounds,
                "queries": variants,
                "added": added,
                "hit_count": all_hits.len(),
                "sufficient": suff_report.sufficient,
                "reasons": &suff_report.reasons,
                "suggested": &suff_report.suggested,
            })));
            if added == 0 {
                break;
            }
        }
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
        if rerank_enabled && crate::ai::rerank_reachable() && all_hits.len() > 10 {
            let top_n = rerank_top_n.min(all_hits.len());
            // 重排是最长的**静默**阶段：远程 /rerank 一次要传 50 份片段，实测
            // 17s（debug）且此前一行日志/事件都不发 —— 用户只看到"语义扫描完成"
            // 之后界面彻底不动。开始/备妥/完成各发一次。
            emit_progress("rerank", &format!("重排中... 候选 {top_n} 份（远程重排通常需数秒）"), 0, 0);
            let rc = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
            let cand_ids: Vec<String> = all_hits[..top_n].iter().map(|h| h.file_id.clone()).collect();
            let cand_recs = crate::db::tracker::get_files_by_ids(&rc, &cand_ids).unwrap_or_default();
            let mut md5s: Vec<String> = cand_recs.iter()
                .filter_map(|r| r.as_ref().and_then(|r| r.md5.clone()))
                .collect();
            md5s.sort();
            md5s.dedup();
            let chunk_embs = match crate::ai::identity::active_embedding_fp() {
                Some(fp) => crate::db::tracker::get_chunk_embeddings_by_md5s(&rc, &md5s, &fp)
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let mut chunks_by_md5: std::collections::HashMap<String, Vec<(usize, Vec<f32>)>> = std::collections::HashMap::new();
            for (md5, idx, vec) in chunk_embs {
                chunks_by_md5.entry(md5).or_default().push((idx, vec));
            }
            // ⚠️ 必须走**带超时**的版本：向量阶段可能已因超时退回 BM25（查询向量
            // 未入缓存），此处裸调用会再次真推理并**永久**卡住整轮聊天。
            let query_emb =
                crate::ai::cached_embed_with_timeout(&search_q, QUERY_EMBED_TIMEOUT_SECS);
            if query_emb.is_none() {
                log::warn!(
                    "[AI]   重排查询向量超时 (>{QUERY_EMBED_TIMEOUT_SECS}s)；改用正文片段做重排通道"
                );
            }
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
                                .map(|c| crate::ai::truncate_for_rerank(&c.text))
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
                        .map(|t| crate::ai::truncate_for_rerank(&t))
                });
                if let Some(p) = passage {
                    passages.push(p);
                    rerank_idx.push(i);
                }
            }
            drop(rc);
            if !passages.is_empty() {
                emit_progress("rerank", &format!("重排中... 已备妥 {} 份候选片段", passages.len()), 0, 0);
                // 重排失败**必须**如实说：以前无论成败都发"重排完成，N 份候选已重排"，
                // 于是模型没加载/网关 404 时用户看到的是"重排生效了"，实际是原始
                // 顺序 —— 静默降级被包装成了成功（与 embedding 那次事故同一类）。
                let mut rerank_note = "重排跳过（模型不可用，保持原顺序）";
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
                        rerank_note = "重排完成";
                    }
                    Some(scores) => {
                        log::warn!("[AI]   rerank: score count mismatch ({} vs {}), keeping original order", scores.len(), rerank_idx.len());
                        rerank_note = "重排跳过（返回条数与候选不符，保持原顺序）";
                    }
                    None => {
                        log::warn!("[AI]   rerank: unavailable, keeping original order");
                    }
                }
                emit_progress("rerank", &format!("{rerank_note}（{} 份候选）", rerank_idx.len()), rerank_idx.len(), rerank_idx.len());
            } else {
                emit_progress("rerank", "重排跳过（无可用候选片段）", 0, 0);
            }
        }

        // ── Post-rerank entity/slot sufficiency ─────────────────────────
        // The sparse-hit pass above cannot see *what* is missing: five
        // unrelated hits are enough to skip it. This pass reads the top
        // candidates' text and checks two cheap signals:
        //   1) a query entity is absent from all top passages;
        //   2) the question asks for an amount/date/number and no plausible
        //      token exists in the top passages.
        // If either is true, one bounded lexical retry is allowed. New hits are
        // prepended (they are entity-targeted) but still pass `hit_in_scope`.
        if progressive_enabled {
            let mut missing_entities: Vec<String> = Vec::new();
            let mut missing_slots_found: Vec<String> = Vec::new();
            if !all_hits.is_empty() {
                let rc = ctx.db.get().map_err(|e| format!("db error: {e}"))?;
                let top_ids: Vec<String> = all_hits
                    .iter()
                    .take(10)
                    .map(|h| h.file_id.clone())
                    .collect();
                let recs = crate::db::tracker::get_files_by_ids(&rc, &top_ids).unwrap_or_default();
                let mut md5s: Vec<String> = recs
                    .iter()
                    .filter_map(|r| r.as_ref().and_then(|r| r.md5.clone()))
                    .collect();
                md5s.sort();
                md5s.dedup();
                let contents = crate::db::tracker::get_contents(&rc, &md5s).unwrap_or_default();
                drop(rc);
                let mut evidence_text = String::new();
                for text in contents.iter().flatten() {
                    if evidence_text.chars().count() >= 50_000 {
                        break;
                    }
                    evidence_text.push_str(text);
                    evidence_text.push('\n');
                }
                // 只在**模型判定为专名**的词上做定向补查：哪个词值得补查由模型决定，
                // 不再用 GENERIC_NOUN_TERMS / jieba 词性判断（「始末」正是被那套
                // 词表当成实体，才触发了一轮把正确答案冲掉的补查，2026-11-06）。
                for kw in entity_kws.iter().take(3) {
                    let kw = kw.trim();
                    if kw.is_empty() {
                        continue;
                    }
                    let in_text = evidence_text.contains(kw);
                    let in_path = all_hits.iter().take(10).any(|h| h.path.contains(kw));
                    if !in_text && !in_path {
                        missing_entities.push(kw.to_string());
                    }
                }
                missing_slots_found = missing_slots(&last_q, &evidence_text);
            }

            if !missing_entities.is_empty() || !missing_slots_found.is_empty() {
                let post_report = check_sufficiency(&SufficiencySignals {
                    hit_count: all_hits.len(),
                    min_hits: 0,
                    missing_entities: missing_entities.clone(),
                    missing_slots: missing_slots_found.clone(),
                    ..Default::default()
                });
                events.push(("sufficiency_check".into(), serde_json::json!({
                    "round": progressive_rounds + 1,
                    "phase": "post_rerank",
                    "hit_count": all_hits.len(),
                    "missing_entities": &missing_entities,
                    "missing_slots": &missing_slots_found,
                    "sufficient": post_report.sufficient,
                    "reasons": &post_report.reasons,
                    "suggested": &post_report.suggested,
                })));
                if !progressive_shadow && progressive_rounds < progressive_max_rounds {
                    let mut variants: Vec<String> = Vec::new();
                    for q in missing_entities
                        .iter()
                        .cloned()
                        .chain(expansion_queries(&last_q, &search_q, &final_kws))
                    {
                        let q = q.trim().to_string();
                        if !q.is_empty() && progressive_tried.insert(q.clone()) {
                            variants.push(q);
                        }
                    }
                    variants.truncate(3);
                    if !variants.is_empty() {
                        emit_progress(
                            "progressive",
                            &format!("补充检索... 命中缺失（{} 个查询变体）", variants.len()),
                            0,
                            0,
                        );
                        let mut new_hits: Vec<ScoredHit> = Vec::new();
                        for variant in &variants {
                            check_cancel!();
                            match bm25_relevant_hits(
                                &ctx,
                                variant,
                                200,
                                false,
                                dir_ids_opt.clone(),
                                ext_filter.clone(),
                                date_from,
                                date_to,
                                None,
                                mention_file_ids.clone(),
                            ) {
                                Ok(hits) => {
                                    for (rank, hit) in hits.into_iter().enumerate() {
                                        rrf_add(&mut rrf_acc, &hit.file_id, rank, 0.5);
                                        if all_seen.insert(hit.file_id.clone()) {
                                            new_hits.push(hit);
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::warn!("[AI]   progressive: post-rerank bm25 failed: {e}");
                                }
                            }
                        }
                        if !new_hits.is_empty() {
                            let added = new_hits.len();
                            // 补查是**补召回**，不是重排：绝不把已经排好的正确结果顶下去。
                            //
                            // 2026-11-06 实测：这里原先无条件 `new_hits.extend(all_hits.drain(..))`
                            // 把 158 个只含泛词「案件」的文书插到最前，注入上限 30 全被它们占满，
                            // 而王会芹本人的 33 份卷宗（BM25 第 1~28 名）被挤到 158 名之后 ——
                            // 模型只能看到未结案清单里那一行，答"材料未提供"。
                            //
                            // 现在只有「路径里真的含有缺失实体」的命中可以前置，且封顶
                            // ENTITY_FRONT_MAX 份；其余一律追加到队尾，等预算有空位才用。
                            const ENTITY_FRONT_MAX: usize = 10;
                            let (promoted, merged) = merge_supplementary_hits(
                                std::mem::take(&mut all_hits),
                                new_hits,
                                &missing_entities,
                                ENTITY_FRONT_MAX,
                            );
                            all_hits = merged;
                            all_hits.retain(|h| {
                                hit_in_scope(
                                    &h.file_id,
                                    &h.path,
                                    path_prefixes_opt.as_deref(),
                                    mention_file_ids.as_deref(),
                                )
                            });
                            progressive_rounds += 1;
                            events.push(("progressive_round".into(), serde_json::json!({
                                "round": progressive_rounds,
                                "phase": "post_rerank",
                                "queries": variants,
                                "added": added,
                                "promoted": promoted,
                                "hit_count": all_hits.len(),
                                "missing_entities": &missing_entities,
                                "missing_slots": &missing_slots_found,
                            })));
                        } else {
                            // Keep the attempt visible even when the variant
                            // returned only already-seen files.
                            events.push(("progressive_round".into(), serde_json::json!({
                                "round": progressive_rounds + 1,
                                "phase": "post_rerank",
                                "queries": variants,
                                "added": 0,
                                "hit_count": all_hits.len(),
                                "missing_entities": &missing_entities,
                                "missing_slots": &missing_slots_found,
                            })));
                        }
                    }
                }
            }
        }
        emit_progress("retrieval", &format!("三路合并完成，共 {} 份文件", all_hits.len()), all_hits.len(), all_hits.len());
        log::info!("[AI]   scan: q=\"{}\" bm25={} extra={} total={}", truncate_text(&search_q, 30), bm25_count, all_hits.len().saturating_sub(bm25_count), all_hits.len());
    }

    const MAX_CONTENT_INJECT: usize = 30;
    let conn = ctx.db.get().map_err(|e| format!("db error: {e}"))?;

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
    //
    // ⚠️ 2026-11-03 修复：原条件 `all_hits.len() < MAX_CONTENT_INJECT`（30）等于**永不放行**
    // —— 正常检索动辄数千命中，于是"沿用上一轮证据"从来没生效过。实测：
    // `陈秀芳的家庭成员有哪些？` → `今天是…各人的年龄是多少` 时 `from_history=0`，
    // 本轮注入的全是无关案件材料，模型只能把"1958/1938"编出来（材料里的正确值是 1928）。
    //
    // 现在改为：追问轮 + **依赖上文**（问句没有专名）⇒ 沿用上一轮引用材料，
    // 数量上限随 basis 调整（纯追问要更多），且必须仍在本轮 scope 内。
    //
    // 2026-11-08（Step 2）：沿用**计入本轮份数上限**，不再额外叠加 —— 之前
    // docs 会变成 30 新 + 10 沿用 = 40 份 / 131k 字，both 变成 12 + 12 = 24 份。
    // 这里把沿用压到总上限的 1/4（docs 30→7、both 12→3），并只保留与本轮
    // 实体（entities）相交的旧材料；history 分支仍以沿用为唯一来源，保留 20。
    let is_follow_up = messages.len() > 1;
    let history_cap = match basis {
        super::rewrite::AnswerBasis::History => 20,
        super::rewrite::AnswerBasis::Both => 3,
        super::rewrite::AnswerBasis::Docs => 7,
    };
    let mut history_resolved: Vec<(String, String)> = Vec::new(); // (file_id, path) 待随材料注入
    if is_follow_up {
        let message_text: String = messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ");
        for fid in source_ids.iter().rev().take(history_cap * 2) {
            if history_resolved.len() >= history_cap { break; }
            if all_seen.contains(fid) { continue; }
            let Some(rec) = rec_by_id.get(fid) else { continue };
            if rec.status != "active" || rec.md5.is_none() { continue; }
            // 只沿用与本轮检索对象相关的旧材料：「他和陈秀芳谁大」本轮只查陈秀芳，
            // 就不再带上一轮的常宏材料（否则 12 个名额被旧对象占满）。
            // `History` 分支不筛：它没有新检索，沿用就是全部材料。
            if basis != super::rewrite::AnswerBasis::History
                && !entity_kws.is_empty()
                && !entity_kws.iter().any(|e| rec.path.contains(e.as_str()))
            {
                continue;
            }
            // 范围守卫：上一轮引用的文件也必须仍在本轮 scope 内（用户可能刚切换了范围）。
            if !hit_in_scope(
                fid,
                &rec.path,
                path_prefixes_opt.as_deref(),
                mention_file_ids.as_deref(),
            ) {
                continue;
            }
            // 追问未点名文件时也保留（保底证据），而非要求文件名出现在问句中
            let named = {
                let stem = std::path::Path::new(&rec.path).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                !stem.is_empty() && message_text.contains(stem.as_str())
            };
            // 模型已判定 history/both ⇒ 明确要沿用上文，不再让启发式否决。
            if matches!(basis, super::rewrite::AnswerBasis::History | super::rewrite::AnswerBasis::Both)
                || super::rewrite::should_carry_history(
                    is_follow_up,
                    context_dependent,
                    named,
                    all_hits.is_empty(),
                )
            {
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
    // 纯追问（history）时这是本轮**唯一**材料来源，预算翻倍。
    let history_total_budget = if basis == super::rewrite::AnswerBasis::History {
        CONTEXT_BUDGET / 2
    } else {
        CONTEXT_BUDGET / 3
    };
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

    // ── Layer 0.6: 「先按上文、作答前自检」的第二段（方案 3） ─────────────
    //
    // basis=history 是模型在**还没看过文档**时下的判断：它只能看到最近 2 轮、
    // 每条 800 字的历史摘要，很容易高估上文（"上一轮的所有人口 / 生日"结论段
    // 常被截掉）。只靠一次前置判断决定"不检索"，错了就是直接答错或答"不确定"。
    //
    // 这里在组装完上文之后、把材料交给回答模型之前，再做一次**带上下文的自检**：
    // 不够就按缺失项补一次定向检索。自检失败/超时一律放行（不阻塞回答）。
    let mut gap_supplement = false;
    if skip_retrieval {
        check_cancel!();
        let mut prior_answers = String::new();
        for m in messages.iter().rev().filter(|m| m.role == "assistant").take(2) {
            prior_answers.push_str(&truncate_text(&m.content, 2000));
            prior_answers.push('\n');
        }
        let snippets: Vec<String> = evidence.iter().map(|e| e.snippet.clone()).collect();
        let (gap, gap_error) = check_context_gap(&last_q, &prior_answers, &snippets);
        let mut gap_event = serde_json::json!({
            "checked": gap.is_some(),
            "error_kind": gap_error,
            "answerable": gap.as_ref().map(|g| g.answerable).unwrap_or(true),
            "missing": gap.as_ref().map(|g| g.missing.clone()).unwrap_or_default(),
            "supplemented": 0,
        });
        if let Some(g) = &gap
            && !g.answerable
        {
            let gap_q = gap_query(&g.missing, &entity_kws, &search_q);
            gap_event["query"] = serde_json::json!(gap_q);
            log::info!(
                "[AI]   gap_check: not answerable (missing={:?}) → supplementary retrieval q=\"{}\"",
                g.missing,
                truncate_text(&gap_q, 40)
            );
            let supplement = super::retrieval::bm25_relevant_hits(
                &ctx,
                &gap_q,
                200,
                false,
                dir_ids_opt.clone(),
                ext_filter.clone(),
                date_from,
                date_to,
                path_prefixes_opt.clone(),
                mention_file_ids.clone(),
            );
            match supplement {
                Ok(hits) => {
                    let before = all_hits.len();
                    for hit in hits {
                        if all_seen.insert(hit.file_id.clone()) {
                            all_hits.push(hit);
                        }
                    }
                    let added = all_hits.len() - before;
                    if added > 0 {
                        // ⚠️ 补查新增的命中**不在**上面那批预取里：`rec_by_id` /
                        // `md5_to_text` 是开跑前按 source/mention/all_hits 一次性取好的。
                        // 不补进来，注入循环会因取不到路径与正文而整份跳过 —— 表现为
                        // `supplemented=189` 但 `injected=0`、`material_count=0`，
                        // 模型只能回"材料未提及"（2026-11-07 实测，v1.5.11 的静默失效）。
                        let new_ids: Vec<String> = all_hits[before..]
                            .iter()
                            .map(|h| h.file_id.clone())
                            .filter(|id| !rec_by_id.contains_key(id))
                            .collect();
                        if !new_ids.is_empty() {
                            let recs = crate::db::tracker::get_files_by_ids(&conn, &new_ids)
                                .unwrap_or_default();
                            let mut md5s: Vec<String> = Vec::new();
                            for (id, rec) in new_ids.iter().zip(recs.iter()) {
                                if let Some(rec) = rec {
                                    rec_by_id.insert(id.clone(), rec.clone());
                                    if let Some(md5) = &rec.md5 {
                                        md5s.push(md5.clone());
                                    }
                                }
                            }
                            md5s.sort();
                            md5s.dedup();
                            md5s.retain(|m| !md5_to_text.contains_key(m));
                            let contents = crate::db::tracker::get_contents(&conn, &md5s)
                                .unwrap_or_default();
                            for (m, c) in md5s.iter().zip(contents.iter()) {
                                if let Some(text) = c {
                                    md5_to_text.insert(m.clone(), text.clone());
                                }
                            }
                        }
                        bm25_returned += added;
                        skip_retrieval = false;
                        gap_supplement = true;
                        emit_progress(
                            "retrieval",
                            &format!("上文不足，补查 {added} 份材料"),
                            added,
                            added,
                        );
                        gap_event["supplemented"] = serde_json::json!(added);
                        log::info!("[AI]   gap_check supplement: +{added} candidates");
                    }
                }
                Err(e) => log::warn!("[AI]   gap_check supplement failed: {e}"),
            }
        }
        events.push(("gap_check".into(), gap_event));
    }

    // Layer 1: Additional retrieval hits (not already in scope).
    // 范围过滤：向量/chunk 通道是全库扫描，会把范围外无关文件混进 all_hits。
    // 注入前按 path_prefixes 前缀过滤（与 BM25 安全网一致），mention 文件
    // （Layer 0 已注入，且可能不在前缀内）除外。全库范围（无前缀）不过滤。
    // strict_docs 不再硬性跳过注入——改为始终用 scoped 过滤，确保严格模式
    // 下目录范围内的命中也能注入（否则目录引用 + strict = 零材料 = 无法回答）。
    // basis=both（上文 + 少量新事实）：检索只用来补细节，注入量收窄，
    // 把上下文预算留给上几轮的结论，避免 30 份材料再次盖过历史。
    // gap_check 补查同理：只补"缺的那点事实"，不需要 30 份。
    const BOTH_CONTENT_INJECT: usize = 12;
    // 2026-11-08（Step 2）：沿用份数要从本轮上限里扣掉，否则"上限"名不副实
    // （实测 docs 40 份 / 131k 字）。历史分支不扣：它本来就没有新检索。
    let carried = history_resolved.len();
    let inject_limit = if full_recall {
        all_hits.len().max(1)
    } else if basis == super::rewrite::AnswerBasis::Both || gap_supplement {
        BOTH_CONTENT_INJECT.saturating_sub(carried).max(6)
    } else if skip_retrieval {
        0
    } else {
        MAX_CONTENT_INJECT.saturating_sub(carried)
    };
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
    let neighbor_radius = std::env::var("LINK_SEARCHER_NEIGHBOR_CHUNKS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .min(2);
    emit_progress("injection", "注入文件内容中...", 0, content_hits.len());
    log::info!("[AI]   injection: content_hits={} mention_has_content={}", content_hits.len(), mention_has_content);
    let mut inject_idx = 0usize;
    let mut inject_skipped = 0usize;
    let mut content_injected = 0usize;
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
                        let expanded_chunks = if neighbor_radius > 0 && !hit.hit_chunks.is_empty() {
                            expand_neighbor_chunks(&hit.hit_chunks, neighbor_radius)
                        } else {
                            Vec::new()
                        };
                        let hit_chunks: &[(usize, f32)] = if expanded_chunks.is_empty() {
                            &hit.hit_chunks
                        } else {
                            &expanded_chunks
                        };
                        let (injected, injected_spans) = chunked_or_truncated_with_budget(&conn, md5, text, &search_q, per_file, hit_chunks);
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
                            content_injected += 1;
                        }
                        if content_budget < MIN_PER_FILE { break; }
                    } else {
                    // 命中没有记录/正文缓存 → 整份跳过。**静默跳过 == 检索到了却等于没检索**：
                    // v1.5.11 的补查失效就是这样（189 命中、注入 0 份、无任何告警）。
                    inject_skipped += 1;
                    }
        inject_idx += 1;
        if inject_idx.is_multiple_of(10) || inject_idx == content_hits.len() {
            emit_progress("injection", &format!("注入中 {}/{}", inject_idx, content_hits.len()), inject_idx, content_hits.len());
        }
    }
    if inject_skipped > 0 {
        log::warn!(
            "[AI]   injection: {inject_skipped}/{} candidates skipped (no record or no cached text)",
            content_hits.len()
        );
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
    let docs_text = docs.join("\n\n---\n\n");
    let docs_chars = docs_text.chars().count();
    let context = truncate_text(&docs_text, max_context_chars);

    // Layer 2: Chat history recall — previous AI responses become usable context.
    //
    // 纯追问（basis=history）时它就是本轮的主要答案来源：**不再按关键词筛选**
    // （"这些人/各人/平均"这类词本来就不会出现在上一轮回答里），且每条放宽到 8k。
    // 其他情况仍按关键词命中筛选，避免把无关旧结论灌进上下文。
    let recall_all = basis == super::rewrite::AnswerBasis::History;
    const CHAT_RECALL_BUDGET: usize = 20_000;
    const CHAT_RECALL_PER_MSG: usize = 2_000;
    const CHAT_RECALL_PER_MSG_HISTORY: usize = 8_000;
    let per_msg_cap = if recall_all { CHAT_RECALL_PER_MSG_HISTORY } else { CHAT_RECALL_PER_MSG };
    let mut chat_recall: Vec<String> = Vec::new();
    let mut recall_budget = CHAT_RECALL_BUDGET;
    if messages.len() > 1 && (recall_all || !final_kws.is_empty()) {
        for (i, m) in messages.iter().enumerate().take(messages.len().saturating_sub(1)) {
            if m.role != "assistant" || m.content.trim().is_empty() { continue; }
            if !recall_all {
                let content_lower = m.content.to_lowercase();
                let match_count = final_kws.iter()
                    .filter(|kw| content_lower.contains(&kw.to_lowercase()))
                    .count();
                if match_count == 0 { continue; }
            }
            let turn = (i + 1) / 2;
            let limit = recall_budget.min(per_msg_cap);
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
    // 材料编号在整个会话内稳定（按首次出现顺序分配），引用可以跨轮沿用；
    // 本轮只声明"实际进入提示词"的编号，避免截断后出现悬空引用。
    let visible_nums = visible_material_numbers(&context);
    let material_note = if visible_nums.is_empty() {
        String::new()
    } else {
        let list = visible_nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
        format!("本轮提供材料编号：{}。", list)
    };
    let system = answer_system_prompt(&material_note, &context, &recall_section);
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
    // 检索指纹：同一问题的两次回答不同时，先比这个事件 ——
    // 指纹一致 ⇒ 差异来自 LLM 采样；指纹不同 ⇒ 差异来自检索层（哪条通道、哪个顺序）。
    let fingerprint_top: Vec<&str> = all_hits
        .iter()
        .take(20)
        .map(|h| h.file_id.as_str())
        .collect();
    events.push(("retrieval_fingerprint".into(), serde_json::json!({
        "search_query": search_q,
        "channels": {
            "bm25": bm25_returned,
            "vector": vector_returned,
            "chunk": chunk_returned,
            "path": path_returned,
        },
        "pool": all_hits.len(),
        "injected": docs.len(),
        "top20": fingerprint_top,
    })));
    events.push(("context_assembled".into(), serde_json::json!({
        "material_count": docs.len(),
        "total_chars": context.chars().count(),
        "strict_docs": strict_docs,
        "truncated_to": max_context_chars,
        // ── 组装诊断（2026-11-08，Step 0：只观测，不改行为）────────────────
        // 此前只有 total_chars，出问题无法区分"材料不够""沿用翻倍""触顶丢件"。
        "carried_count": history_files_with_content.len(),
        "carried_chars": history_budget_used,
        "injected": content_injected,
        "dropped": content_hits.len().saturating_sub(inject_idx),
        "skipped": inject_skipped,
        "truncated": docs_chars > max_context_chars,
    })));
    // ── 提示词留痕（只读审计，2026-11-07） ────────────────────────────────
    // 提示词全部硬编码在源码里，用户看不到；一旦"答得怪"，无法区分是指令让它
    // 理解错、还是材料没给对。这里把**指令段**（system 头部 2000 字，材料在后）
    // 与**问题段**（user 首尾各 1000 字：头部是对话历史，尾部是当前问题）
    // 连同长度/摘要写进 ai_events，由「推理过程」里的「提示词」一栏展开查看。
    // 只读、不改行为，也不参与任何判断。
    events.push(("prompt_snapshot".into(), serde_json::json!({
        "system_chars": system.chars().count(),
        "system_head": truncate_text(&system, PROMPT_SNAPSHOT_HEAD),
        "system_digest": prompt_digest(&system),
        "user_chars": user_msg.chars().count(),
        "user_head": truncate_text(&user_msg, PROMPT_SNAPSHOT_TAIL),
        "user_tail": tail_text(&user_msg, PROMPT_SNAPSHOT_TAIL),
        "user_digest": prompt_digest(&user_msg),
    })));
    // 指代消解基于改写后的 search_q：代词已在改写中展开，避免规则管道二次误判歧义。
    let (clarify_note, clarify_candidates, clarify_slots, clarify_blocking) = {
        use crate::commands::clarify;
        let session_paths = prior_evidence_paths(&ctx, &session_id);
        let current: Vec<(String, String)> =
            evidence.iter().map(|e| (e.path.clone(), e.snippet.clone())).collect();
        let conditions: Vec<(String, String)> =
            scope.conditions.iter().map(|c| (c.kind.clone(), c.value.clone())).collect();
        let mut ir = clarify::propose_ir(&search_q);
        // 我们已经把「他/她」确定绑定到上一轮的人物实体，就把这条绑定交给
        // clarify 引擎，否则它仍会把同一个代词判成"指代不明确"并追加提示
        // （2026-11-02 实测：检索词里明明有"陈正先"，回答前仍弹提示）。
        if let Some((surface, value)) = pronoun_binding.as_ref() {
            if clarify::apply_binding(&mut ir, surface, value) {
                log::info!("[AI]   clarify auto-bound: {surface} → {value}");
            } else {
                log::info!("[AI]   clarify auto-bind skipped (no slot for {surface})");
            }
        }
        if let Some(r) = clarify_reply.as_ref() {
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
    // 「有依据」= 有材料 **或** 有可用的历史结论。旧实现只看 docs，
    // 于是"纯追问 + 本轮检索空"会直接拒答，连 LLM 都不调（2026-11-06 修）。
    let has_evidence = !context.trim().is_empty() || !chat_recall.is_empty();
    let basis_tag = match basis {
        super::rewrite::AnswerBasis::History => "history",
        super::rewrite::AnswerBasis::Both => "both",
        super::rewrite::AnswerBasis::Docs => "docs",
    };
    Ok(PreparedConversation { system, user_msg, source_ids: source_ids_final, source_files: source_files_final, evidence, search_query: search_q, search_terms, basis: basis_tag.to_string(), clarify_note, clarify_candidates, clarify_slots, clarify_blocking, hits, events, total_match_count: all_hits.len(), has_evidence, visible_nums })
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

/// 取尾部 N 个字符 —— `user_msg` 的「当前问题」在最后，头截断会正好把它丢掉。
fn tail_text(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    s.chars().skip(n - max_chars).collect()
}

/// 8 位十六进制摘要（FNV-1a）。只用于"这一轮与上一轮的提示词是不是同一份"这类
/// 比对，**不是**安全哈希。
fn prompt_digest(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")[..8].to_string()
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
    // 证据片段是给用户看的（聊天引用卡片 / 飞书回答）：Office 正文是 anydoc 产出的
    // Markdown，这里统一剥掉展示标记，免得引用里出现 `**加粗**`、`|表格|`。
    crate::commands::search::strip_md_inline(&cited_excerpt_raw(text, terms, max_chars))
}

fn cited_excerpt_raw(text: &str, terms: &[String], max_chars: usize) -> String {
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
