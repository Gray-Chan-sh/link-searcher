//! Follow-up query rewriting: deictic expansion, LLM rewrite, stopwords and
//! retrieval keyword extraction.

use super::{ChatMessage, truncate_text};
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
    // 触发改写：
    // 1) 问句过短（<4 字）
    // 2) 以指代词开头（它/这个/那个…）
    // 3) 检索关键词为空（extract_retrieval_keywords 已过滤停用词，返回空
    //    ⇒ 问句不含任何可检索实体词，如"把时间列出来"这类依赖上文的追问，
    //    必须借助历史补全实体词。）
    let keywords_empty = extract_retrieval_keywords(&q).is_empty();
    let needs_rewrite = q.chars().count() < 4
        || DEICTIC.iter().any(|p| q.starts_with(p))
        || keywords_empty;
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
/// query's practical retrieval ceiling, not echoing the original, and — the
/// key point — carrying at least one *retrievable* term.
///
/// 「先改写再做停用词过滤」的前置保证就在这里：若改写结果经停用词过滤后
/// 一个检索词都不剩（例如 LLM 只把问题换成"文档 内容"这类泛词），它并不比
/// 原句更有用，视为无效 → 回退规则链（再由目录/范围兜底接手），避免把一次
/// 无效的 LLM 往返当成有效改写继续往下走。
pub(super) fn valid_rewrite_output(s: &str, original: &str) -> Option<String> {
    let t = s.trim().trim_matches(['"', '\'', '“', '”']);
    if t.is_empty() || t == original.trim() || t.chars().count() > 80 {
        return None;
    }
    if extract_retrieval_keywords(t).is_empty() {
        log::info!("[AI]   llm rewrite rejected: no retrievable term in {:?}", truncate_text(t, 40));
        return None;
    }
    Some(t.to_string())
}

/// 构建检索改写用对话历史字符串。只包含用户消息——助手回答
/// （尤其是否定结论）回灌会导致自相矛盾的检索查询。
pub(super) fn rewrite_history(messages: &[ChatMessage], context_paths: &[String]) -> String {
    let users: Vec<&ChatMessage> = messages.iter().filter(|m| m.role == "user").collect();
    let mut history_str = String::from("对话历史：\n");
    let start = users.len().saturating_sub(8);
    for m in &users[start..] {
        history_str.push_str(&format!("用户：{}\n", truncate_text(&m.content, 300)));
    }
    if !context_paths.is_empty() {
        history_str.push_str("会话已涉及的文件（可用于绑定指代）：\n");
        for p in context_paths.iter().take(12) {
            history_str.push_str(&format!("- {}\n", p.rsplit('/').next().unwrap_or(p.as_str())));
        }
    }
    history_str
}

/// Try an LLM query rewrite within a strict time budget. Returns `None`
/// (and the caller falls back to the rule-based rewrite) on any failure:
/// gateway disabled, timeout, empty/garbage output.
pub async fn llm_rewrite_query(
    last_q: &str,
    messages: &[ChatMessage],
    context_paths: &[String],
) -> Option<String> {
    if !crate::ai::llm_enabled() {
        return None;
    }
    let history_str = rewrite_history(messages, context_paths);
    let system = "你是检索查询改写助手。用户在与本地文档对话，你的任务是把他的追问改写成一条可独立检索的中文查询：补全指代（它/这/那/刚才/上面等）与省略，当问句缺乏区分性实体词（人名/公司名/案名/主题名）时从对话历史中继承主题实体。要求：输出最小必要关键词短语，保留主题实体（具体人名/报告名称/年份/主题词），去掉“报告/文件/呢/吗/的/了”等无区分词。只输出改写后的查询本身，不要解释、不要加引号、不要写“改写为”。如果问题本身就完整无需改写，原样输出。";
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

/// 检索级停用词：问句中无区分度的泛词。它们会稀释 BM25/向量信号，
/// 把"常宏"这类核心实体挤到检索排序后面。
fn is_retrieval_stopword(w: &str) -> bool {
    is_rewrite_stopword(w)
        || matches!(w,
            // 疑问/数量泛词
            "多少" | "几个" | "哪些" | "什么" | "是否" | "有无" | "怎么" | "如何"
            | "一共" | "总共" | "合计" | "数量" | "数目" | "份" | "个" | "几"
            // 动作/主题泛词
            | "涉及" | "相关" | "有关" | "关于" | "涉及到的" | "需要" | "知道" | "看看"
            | "告诉" | "查询" | "搜索" | "查找" | "列出" | "列表" | "列举" | "汇总" | "整理"
            // 连接/引导泛词（问句开头的高频虚词，无检索区分度）
            | "根据" | "依据" | "按照" | "依照" | "说明" | "表明" | "请问" | "指明" | "指出"
            // 领域泛词（检索全库时无区分度）
            | "案件" | "案子" | "民事" | "民事案件" | "刑事案件" | "刑事" | "行政" | "行政诉讼"
            | "诉讼" | "起诉" | "判决" | "裁定" | "案由"
            | "文件" | "文档" | "资料" | "材料" | "内容" | "信息" | "情况" | "问题"
            | "公司" | "单位" | "部门" | "人员" | "时间" | "日期" | "地点" | "会议纪要")
}

/// 从检索问句中提炼核心实体词（专有名词优先，如人名/地名/机构名），
/// 用于 BM25/路径/向量三通道的精准检索。
///
/// 用 `TokenizeMode::Search` 而非词性标注：jieba 默认词典不收录人名
/// （"常宏"会被 tag 拆成 常+宏 两个单字），而 Search 模式能保留
/// "常宏"/"万城" 这类专有名词为整体词。
///
/// Search 模式会先输出子词片段（如"不动产"→"不动"/"动产"/"不动产"），
/// 这里在收集后丢弃被更长候选包含的子词片段，只保留完整词。
///
/// 例："涉及常宏的民事案件一共有多少，请列表" → ["常宏"]
///      "万城的股东资格确认纠纷" → ["万城", "股东资格"]
///      "上周会议纪要" → []（无实体，调用方回退完整问句）
pub(super) fn extract_retrieval_keywords(query: &str) -> Vec<String> {
    let q = query.trim();
    if q.chars().count() < 2 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for t in crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner()).tokenize(q, jieba_rs::TokenizeMode::Search, true) {
        // jieba-rs 的 Token 直接带 word（&str），无需手动切片
        let w = t.word.trim();
        if w.is_empty() || w.chars().count() < 2 || is_retrieval_stopword(w) {
            continue;
        }
        // 4 位以上的纯数字保留：案号/编号（如 "22963"）是文件名里的强锚点，
        // 丢弃它会让"22963 号判决…"这类提问匹配不到目标文件。
        let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
        if digits < 4
            && w.chars().all(|c| c.is_ascii_digit() || c.is_ascii_punctuation() || c.is_whitespace())
        {
            continue;
        }
        if out.iter().any(|k: &String| k == w) {
            continue;
        }
        out.push(w.to_string());
    }
    // 丢弃被更长候选包含的子词片段（"不动"/"动产" ⊆ "不动产"）
    out
        .iter()
        .filter(|a| {
            !out.iter().any(|b| {
                b.as_str() != a.as_str() && b.chars().count() > a.chars().count() && b.contains(a.as_str())
            })
        })
        .cloned()
        .collect()
}

fn parent_keywords(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for t in crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner()).cut(text, false) {
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

/// Hard fallback for entity inheritance: after LLM/rule-based query rewrite,
/// ensure that core entity keywords from the parent question are present in
/// the rewritten query. When the LLM rewrite drops a distinguishing entity
/// (e.g. a person name or case name), prepend the missing entities so BM25,
/// vector, and path channels can still hit the correct documents.
///
/// Uses `extract_retrieval_keywords` (Search tokenization mode) on the
/// parent to capture proper nouns like "毛弟" / "常宏" / "万城" that jieba
/// cut mode may split.
pub(super) fn ensure_parent_entities(search_q: &str, parent_q: &str) -> String {
    if parent_q.trim().is_empty() || parent_q.trim() == search_q.trim() {
        return search_q.to_string();
    }
    let parent_entities = extract_retrieval_keywords(parent_q);
    if parent_entities.is_empty() {
        return search_q.to_string();
    }
    let lower = search_q.to_lowercase();
    let missing: Vec<&str> = parent_entities
        .iter()
        .filter(|kw| !lower.contains(&kw.to_lowercase()))
        .map(|s| s.as_str())
        .collect();
    if missing.is_empty() {
        return search_q.to_string();
    }
    format!("{} {}", missing.join(" "), search_q)
}

/// Whether a prior turn's source file should be carried forward as evidence
/// for the current follow-up turn. A file is kept when the follow-up question
/// names it OR when the current turn found no hits at all (so history is the
/// only available evidence). Prevents the cross-turn evidence gap where a
/// generic follow-up ("列出清单/把时间列出来") drops the prior turn's cited
/// documents because the question never names the file.
pub(super) fn carry_forward_source(is_named: bool, all_hits_empty: bool) -> bool {
    is_named || all_hits_empty
}
