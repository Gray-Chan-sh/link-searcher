//! Follow-up query rewriting: deictic expansion, LLM rewrite, stopwords and
//! retrieval keyword extraction.

use super::{ChatMessage, truncate_text};
/// Outcome of a follow-up query rewrite: the query to actually retrieve
/// with (may equal the original when no rewrite applied).
pub struct RewriteOutcome {
    pub query: String,
}

/// 指代词：以它们开头的追问在检索上基本无从落地，必须借历史补全。
const DEICTIC: &[&str] =
    &["它", "这个", "那个", "上述", "上文", "该", "那", "此", "刚才", "上面", "之前", "前面"];

/// 本轮问题 = **最后一条 user 消息**（不是"最后一条消息"）。
///
/// 曾经用 `messages.last()`：卡片「换一批证据」重跑时，会话里最后一条是上一轮的
/// **回答**，于是"重跑"变成了"对上一条回答再问一遍"（2026-10-16 实测事件里，
/// 重跑那轮的 `query_rewrite.original` 正是上一轮回答正文）。
pub(super) fn question_from_messages(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.trim().to_string())
        .unwrap_or_default()
}

/// 本轮问句是否**需要借历史才能检索**：过短、以指代词开头、或过滤后一个检索词都不剩。
///
/// 两处共用：`rewrite_query` 用它决定是否前置父问句关键词；
/// [`may_inherit_parent_entities`] 用它判定"必须借历史"的场合。
pub(super) fn needs_history_rewrite(q: &str) -> bool {
    let q = q.trim();
    q.chars().count() < 4
        || DEICTIC.iter().any(|p| q.starts_with(p))
        || extract_retrieval_keywords(q).is_empty()
}

/// Query rewrite for follow-up questions: when the current question needs history
/// (see [`needs_history_rewrite`]) prepend keywords from the most recent *previous*
/// user message so BM25 sees the referents the pronoun points back to.
/// The LLM branch (see [`llm_rewrite_query`]) replaces pronouns contextually when
/// the gateway is available.
pub fn rewrite_query(last_q: &str, messages: &[ChatMessage]) -> RewriteOutcome {
    let q = last_q.trim();
    if !needs_history_rewrite(q) {
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
    let system = "你是检索查询改写助手。用户在与本地文档对话，你的任务是把他的追问改写成一条可独立检索的中文查询：补全指代（它/这/那/刚才/上面等）与省略，当问句缺乏区分性实体词（人名/公司名/案名/主题名）时从对话历史中继承主题实体。**继承的前提是当前问题确实需要借历史才能读懂**（有指代词、过短、或没有任何可检索的实体词）；若当前问题本身已经是一个完整、独立的新问题，就只围绕它改写，**绝不**引入历史里的人物 / 机构 / 案件 / 主题。要求：输出最小必要关键词短语，保留主题实体（具体人名/报告名称/年份/主题词），去掉“报告/文件/呢/吗/的/了”等无区分词。只输出改写后的查询本身，不要解释、不要加引号、不要写“改写为”。如果问题本身就完整无需改写，原样输出。";
    let user = format!("{history_str}\n当前问题：{last_q}\n改写后的查询：");
    let sys = system.to_string();
    let fut = tokio::task::spawn_blocking(move || crate::ai::chat(&sys, &user));
    match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(Some(s))) => valid_rewrite_output(&s, last_q),
        _ => None,
    }
}

/// 本轮问句是否**可以**继承上一轮的实体（人名/案名/机构名）。
///
/// 两条准入（任一成立即可）：
/// 1. 本轮问句**本身就需要借历史**（[`needs_history_rewrite`]：指代词开头/过短/无检索词）
///    —— 这正是"实体继承"的原始用途（"它的风险有哪些"要靠上轮的主体）；
/// 2. 否则必须是**与父问句共享检索词**的追问（= 同一话题，如父问"毛弟是否被人辱骂"、
///    本轮"列出清单，把时间、辱骂人列出"共享"辱骂"）。
///
/// 反过来就是 P2 那条规则：**自足问句 + 与父问句关键词零交集 = 新话题** ⇒ 不继承。
/// 2026-10-16 实测踩到：`汪均益案的最终结果是什么？`（自带实体）被硬塞了上一轮的人名，
/// 检索被带到旧材料上，模型只能答"材料不足"。
pub(super) fn may_inherit_parent_entities(current_q: &str, parent_q: &str) -> bool {
    if needs_history_rewrite(current_q) {
        return true;
    }
    let cur = extract_retrieval_keywords(current_q);
    if cur.is_empty() {
        return true;
    }
    let par = extract_retrieval_keywords(parent_q);
    cur.iter().any(|a| par.iter().any(|b| a == b))
}

/// 自足问句的改写结果里**不得出现本轮问句没有的实体**。
///
/// 与 [`may_inherit_parent_entities`] 成对：不许继承的场合（新话题），改写只能围绕本轮
/// 问句做取舍（去泛词、换同义词），**不能引入**上一轮的人名/案名 —— 那会把检索引到
/// 无关材料上，"材料里没有"的假象正是这么来的（2026-10-16 实测：新话题被 LLM 加上
/// 上一轮的人名，三通道都被带偏）。
///
/// 返回 `true` 表示这份改写应当**弃用**（回退规则链）。
///
/// 注意判据是"实体词"（`extract_retrieval_keywords`，已滤掉"判决/裁定/材料"这类泛词），
/// 所以重跑扩查刻意补的结论性词**不会**被误判违规。
pub(super) fn rewrite_adds_foreign_entity(rewritten: &str, current_q: &str, parent_q: &str) -> bool {
    let cur = current_q.trim();
    if cur.is_empty() || may_inherit_parent_entities(cur, parent_q) {
        return false;
    }
    let lower = cur.to_lowercase();
    extract_retrieval_keywords(rewritten)
        .iter()
        .any(|kw| !lower.contains(&kw.to_lowercase()))
}

/// 重跑（卡片「换一批证据」）专用的**查询扩展**：上一轮用问题原词没能找到能直接回答
/// 的材料，这次换一组**不一样**但更可能命中的检索词。
///
/// 与 [`llm_rewrite_query`] 的分工：
/// - 输入是**本轮原始问句**（不是上一条回答 —— 那正是被修掉的 bug）外加上一轮回答；
/// - 任务不是"补全指代"，而是"换词"：首轮没答上多半是检索词过于具体、或用了语料里
///   没有的说法，所以要求保留区分性实体（人名/案号/年份），补上同义词、上位词与
///   结论性词（判决/裁定/调解/结案…看问题而定）；
/// - 仍走 [`valid_rewrite_output`]：泛词化、空输出、原样回显一律视为无效。
///
/// 返回 `None` ⇒ 调用方回退规则链：**"换一批"绝不能退化成"同一个查询再搜一次"
/// （那就是白点一次），但更不能因为扩展失败就不回答。**
pub async fn llm_expand_retry_query(question: &str, prior_answer: &str) -> Option<String> {
    let q = question.trim();
    if q.is_empty() || !crate::ai::llm_enabled() {
        return None;
    }
    let system = "你在为本地文档检索**换一批检索词**。上一轮用问题原词检索失败（材料里找不到能回答的内容），现在必须换一组**不同的**词再搜。硬性要求：1) 只围绕同一个问题，不换话题，不引入问题之外的人名/机构名/案名；2) **必须保留**问题里的区分性实体（人名/机构/案号/年份/项目名），一个字都不能改；3) **必须换词**：不许只把原句删掉“的/是/什么”变短一点就交差，要补上同义表述、上位词，以及这类问题在文档里通常出现的**结论性词** —— 例如问结果的补：判决/裁定/调解/结案/结论/处理结果；问时间的补：签订/履行/起止；问金额的补：金额/结算/付款（按问题类型自己判断）；4) 输出 6～12 个检索词，用空格分隔；不要解释、不要引号、不要照抄上一轮回答。";
    let user = format!(
        "问题：{q}\n上一轮回答（供参考，不要照抄）：{}\n新的检索词：",
        truncate_text(prior_answer.trim(), 300)
    );
    let fut = tokio::task::spawn_blocking(move || crate::ai::chat(&system.to_string(), &user));
    match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(Some(s))) => valid_rewrite_output(&s, q),
        _ => None,
    }
}

/// 重跑扩查的守卫：**本轮问句的锚点实体必须还在**。
///
/// 与 [`rewrite_adds_foreign_entity`] 防的是**相反**的两件事，判据也相反：
/// - 新话题守卫防"**多**出上一轮的人名/案名"；
/// - 这里防"**丢**掉本轮问句的人名/案号"。
///
/// 扩查**本来就该补词**（判决书 / 裁定书 / 结案 / 调解…这些正是首轮没命中的原因），
/// 若把"补词"当违规拦下来，重跑就退化成"原词再搜一遍" —— 用户点了等于白发
/// （2026-10-16 实测踩到：`汪均益案 最终结果` 被这条判据之外的守卫拦掉，`retry` 标记
/// 变成 true 但 `rewritten` 与原问题一模一样）。故只要求"至少保住一个锚点实体"。
pub(super) fn retry_expansion_keeps_question_entity(expanded: &str, question: &str) -> bool {
    let anchors = extract_retrieval_keywords(question);
    if anchors.is_empty() {
        // 问句本身没有区分性实体（如"把时间列出来"）→ 无从判定；空输出/泛词由
        // `valid_rewrite_output` 挡住。
        return true;
    }
    let lower_expanded = expanded.to_lowercase();
    if anchors.iter().any(|a| lower_expanded.contains(&a.to_lowercase())) {
        return true;
    }
    // 反向包含也要认：锚点常是**复合词**（"汪均益案"），而扩查可能只用其中的人名
    // （"汪均益"）—— 那同样是锚定在本轮问句的实体上，不能判成"丢了锚点"。
    let lower_anchors: Vec<String> = anchors.iter().map(|a| a.to_lowercase()).collect();
    extract_retrieval_keywords(expanded)
        .iter()
        .any(|t| t.chars().count() >= 2 && lower_anchors.iter().any(|a| a.contains(&t.to_lowercase())))
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
pub(super) fn ensure_parent_entities(search_q: &str, parent_q: &str, current_q: &str) -> String {
    // 只能是"可以继承"的场合：新话题（自足问句 + 与父问句零交集）**不**硬塞父问句实体，
    // 否则这条兜底会把 LLM 那侧刚修掉的"继承上一轮实体"重新加回去（P2）。
    if !may_inherit_parent_entities(current_q, parent_q) {
        return search_q.to_string();
    }
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
