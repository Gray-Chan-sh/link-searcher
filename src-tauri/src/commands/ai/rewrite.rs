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
        || is_ellipsis_followup(q)
}

/// 省略式追问：以「呢 / 吧 / 吗 / 么」收尾，且**整句没有任何专有名词**。
///
/// 2026-11-02 实测：`标的最大的呢？` 在「陈骥代理的案子」之后被问出来，老判据三条
/// 都不成立（有 6 个字、不以指代词开头、有检索词）→ 判成自足问题 → 只拿
/// `标的最大的` 去检索，候选池从 5 040 掉到 1 710，等于丢掉了上下文主体。
///
/// 判据里"无专有名词"是关键：`汪均益案的最终结果呢？` 已经点名了对象，不该继承；
/// `标的最大的呢？` 没有任何可锚定的专名，只能回上一轮找。
fn is_ellipsis_followup(q: &str) -> bool {
    let t = q
        .trim()
        .trim_end_matches(['？', '?', '。', '！', '!', '，', ',', ' ', '～', '~']);
    if t.is_empty() {
        return false;
    }
    let ends_with_particle =
        t.ends_with('呢') || t.ends_with('吧') || t.ends_with('吗') || t.ends_with('么');
    ends_with_particle && !has_proper_noun(t)
}

/// 问句里是否出现人名 / 地名 / 机构名 / 专有名词（jieba `nr|ns|nt|nz`，长度 ≥2）。
fn has_proper_noun(text: &str) -> bool {
    crate::search::schema::JIEBA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tag(text, true)
        .iter()
        .any(|t| {
            let tag = t.tag.to_string();
            t.word.chars().count() >= 2
                && (tag.starts_with("nr")
                    || tag.starts_with("ns")
                    || tag.starts_with("nt")
                    || tag.starts_with("nz"))
        })
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
#[cfg(test)]
mod pronoun_followup_tests {
    use super::*;

    #[test]
    fn keeps_person_name_and_drops_topic_words() {
        let entities = parent_person_entities("陈正先的身份证号是多少？");
        assert!(entities.iter().any(|e| e == "陈正先"), "{entities:?}");
        assert!(!entities.iter().any(|e| e == "身份证号"), "{entities:?}");
    }

    /// 规则兜底：父问句的关键词必须是**专名**，不能是主题词。
    /// 2026-11-02 实测：取前 3 个词会把 `身份证号` 带进「那他涉及哪些案件」。
    #[test]
    fn fallback_parent_keywords_prefer_proper_nouns() {
        let kws = parent_keywords("陈正先的身份证号是多少？", 3);
        assert_eq!(kws, vec!["陈正先".to_string()], "{kws:?}");

        let kws = parent_keywords("万城公司人员列表", 3);
        assert_eq!(kws, vec!["万城公司".to_string()], "{kws:?}");
    }

    /// 回归（2026-11-02 实测）：`标的最大的呢？` 被判成自足问题，只拿"标的最大的"
    /// 去检索，候选池 5 040 → 1 710，丢了上一轮的主体。
    #[test]
    fn trailing_particle_followup_needs_history() {
        assert!(needs_history_rewrite("标的最大的呢？"));
        assert!(needs_history_rewrite("那个案子的结果吧？") || needs_history_rewrite("结果吧？"));
        // 已点名具体对象 → 自足，不该继承上一轮。
        assert!(!needs_history_rewrite("汪均益案的最终结果呢？"));
    }
}

#[cfg(test)]
mod history_backed_entity_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: content.into() }
    }

    /// 核心回归（2026-11-02 实测）：模型把「今天是2026年11月12日，各人的年龄是多少」
    /// 正确改写成 `陈秀芳 家庭成员 年龄`，`陈秀芳` 来自上一轮对话 → **必须放行**。
    #[test]
    fn entity_from_history_is_allowed() {
        let msgs = vec![
            msg("user", "陈秀芳的家庭成员有哪些？"),
            msg("assistant", "陈秀芳的父亲是陈贤文，哥哥是陈正先……"),
            msg("user", "今天是2026年11月12日，各人的年龄是多少"),
        ];
        let hist = recent_history_text(&msgs, "今天是2026年11月12日，各人的年龄是多少");
        assert!(
            entities_backed_by_history("陈秀芳 家庭成员 年龄", "今天是2026年11月12日，各人的年龄是多少", &hist),
            "{hist}"
        );
    }

    /// 历史里根本没有的实体（模型编的）仍然要拦。
    #[test]
    fn entity_absent_from_history_is_rejected() {
        let msgs = vec![
            msg("user", "陈秀芳的家庭成员有哪些？"),
            msg("user", "今天是2026年11月12日，各人的年龄是多少"),
        ];
        let hist = recent_history_text(&msgs, "今天是2026年11月12日，各人的年龄是多少");
        assert!(!entities_backed_by_history(
            "张三 家庭成员 年龄",
            "今天是2026年11月12日，各人的年龄是多少",
            &hist
        ));
    }

    /// 本轮问句自带的实体不需要历史出处。
    #[test]
    fn entity_typed_in_question_needs_no_history() {
        assert!(entities_backed_by_history(
            "汪均益案 最终结果 判决",
            "汪均益案的最终结果是什么？",
            ""
        ));
    }

    /// 只校验"实体"，不拦普通同义改写。
    /// 2026-11-02 实测：`出生日期` 是把「今年多少岁」换了个说法，却被当成编造实体，
    /// 导致整条正确的改写（含 9 个人名）被弃用。
    #[test]
    fn synonyms_are_not_treated_as_entities() {
        let msgs = vec![
            msg("user", "陈秀芳的家庭成员有哪些？"),
            msg("assistant", "陈秀芳的父亲是陈贤文，哥哥是陈正先"),
            msg("user", "今天是2026年11月12日，各人的年龄是多少"),
        ];
        let q = "今天是2026年11月12日，各人的年龄是多少";
        let hist = recent_history_text(&msgs, q);
        assert!(entities_backed_by_history(
            "陈秀芳 陈贤文 陈正先 出生日期",
            q,
            &hist
        ));
    }

    /// 改写历史要包含助手回答（仅用于解析指代），且标注不得当事实。
    #[test]
    fn rewrite_history_includes_reference_only_answers() {
        let msgs = vec![
            msg("user", "陈秀芳今年多少岁"),
            msg("assistant", "她 1956 年出生"),
            msg("user", "她的哥哥今年多少岁了"),
        ];
        let h = rewrite_history(&msgs, &[], "她的哥哥今年多少岁了");
        assert!(h.contains("1956"), "上一轮回答应进改写历史: {h}");
        assert!(h.contains("不得作为事实依据"), "必须标注仅供参考: {h}");
    }
}

#[cfg(test)]
mod inherited_term_tests {
    use super::*;

    #[test]
    fn drops_parent_verbs_and_quantifiers() {
        let llm = "申洲 大通 会计师 事务所 做过 多份 验资 报告 所有 这些文号里有没有重复的？";
        let out = strip_inherited_generic_terms(llm, "这些文号里有没有重复的？");
        assert!(!out.contains("做过"), "{out}");
        assert!(!out.contains("多份"), "{out}");
        assert!(!out.contains("所有"), "{out}");
        assert!(out.contains("验资"), "{out}");
        assert!(out.contains("这些文号"), "{out}");
    }

    #[test]
    fn keeps_terms_the_user_actually_typed() {
        // "所有" is part of the current question here → must survive.
        let out = strip_inherited_generic_terms("万城公司 所有 工商变更", "万城公司所有工商变更");
        assert!(out.contains("所有"), "{out}");
    }
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

/// 构建检索改写用的对话历史。
///
/// 2026-11-02 起**包含助手回答**（最近 2 轮、截断 800 字，并显式标注"仅供参考"）：
/// 「她的哥哥今年多少岁了」这类追问必须靠上一轮答案里的家庭成员表才能解析指代。
/// 旧实现只喂 user 消息，怕"没有找到"这类否定结论污染检索 —— 折中办法是标注
/// 用途（仅解析指代，不得当事实），而不是把它们完全排除。
pub(super) fn rewrite_history(
    messages: &[ChatMessage],
    context_paths: &[String],
    last_q: &str,
) -> String {
    let prior: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| !m.content.trim().is_empty() && m.content.trim() != last_q.trim())
        .collect();
    // 最近 4 条（≈2 轮）足够解析指代，再往前只会稀释注意力。
    let start = prior.len().saturating_sub(4);
    let mut history_str = String::from("对话历史：\n");
    for m in &prior[start..] {
        if m.role == "assistant" {
            history_str.push_str(&format!(
                "助手回答（仅供参考、仅用于解析指代，不得作为事实依据）：{}\n",
                truncate_text(&m.content, 800)
            ));
        } else {
            history_str.push_str(&format!("用户：{}\n", truncate_text(&m.content, 300)));
        }
    }
    if !context_paths.is_empty() {
        history_str.push_str("会话已涉及的文件（可用于绑定指代）：\n");
        for p in context_paths.iter().take(12) {
            history_str.push_str(&format!("- {}\n", p.rsplit('/').next().unwrap_or(p.as_str())));
        }
    }
    history_str
}

/// Try an LLM query rewrite within a generous time budget.
///
/// 超时默认 **60 秒**（`LINK_SEARCHER_REWRITE_TIMEOUT_SECS` 可调）：这个网关的改写
/// 请求常态 4–6 秒，旧的 5 秒上限会把关键的指代消解直接打死（2026-11-02 实测）。
pub async fn llm_rewrite_query(
    last_q: &str,
    messages: &[ChatMessage],
    context_paths: &[String],
) -> RewriteAttempt {
    if !crate::ai::llm_enabled() {
        return RewriteAttempt::Unavailable;
    }
    let history_str = rewrite_history(messages, context_paths, last_q);
    let system = "你是检索查询改写助手。用户在与本地文档对话，你的任务是把他的提问改写成一条**可独立检索**的中文查询。\
规则：\
1) 补全指代与省略：代词（他/她/它/他们/这个/那个/该/上述/前者/后者）和**省略主语**的追问（例如上一轮问「万城公司人员列表」，本轮只问「办公室」「公司办公室」「各人的年龄」「标的最大的呢」），必须从上文找出它真正指向的人名/机构名/案件名补进查询。\
2) 继承的主题实体必须**来自对话历史**（用户的提问或助手的回答），不得凭空编造。\
3) 若当前问题本身就是一个完整、独立的新问题（自带明确的人名/机构名/案件名/文件号），就只围绕它改写，**不要**引入历史里的对象。\
4) 助手的历史回答只用于判断「这句话在说什么」，**不得**把它当作事实写进查询。\
5) 输出最小必要关键词短语（6～12 个词，空格分隔），去掉“的/了/吗/呢/报告/文件”这类无区分词。\
只输出改写后的查询本身，不要解释、不要加引号、不要写“改写为”，也不要照抄整句问题。";
    let user = format!("{history_str}\n当前问题：{last_q}\n改写后的查询：");
    let sys = system.to_string();
    let fut = tokio::task::spawn_blocking(move || crate::ai::chat(&sys, &user));
    let budget = rewrite_timeout_secs();
    let started = std::time::Instant::now();
    match tokio::time::timeout(std::time::Duration::from_secs(budget), fut).await {
        Ok(Ok(Some(s))) => match valid_rewrite_output(&s, last_q) {
            Some(v) => RewriteAttempt::Ok(strip_inherited_generic_terms(&v, last_q)),
            None => RewriteAttempt::Invalid(truncate_text(s.trim(), 60)),
        },
        Ok(Ok(None)) => RewriteAttempt::Invalid("gateway returned no content".into()),
        Ok(Err(e)) => RewriteAttempt::Invalid(format!("task failed: {e}")),
        Err(_) => {
            log::warn!(
                "[AI]   llm rewrite timeout after {}s（q=\"{}\"）",
                started.elapsed().as_secs(),
                truncate_text(last_q, 30)
            );
            RewriteAttempt::Timeout(budget)
        }
    }
}

/// Outcome of one LLM rewrite attempt. The caller must be able to tell
/// "模型没答" 与 "模型答了但不可用" 与 "超时" —— 三者以前都塌成 `None`，
/// 日志里只剩 `method=rule`，查不出是网关慢还是守卫拦的（2026-11-02 实测：
/// 一次 5.0s 超时与一次被守卫弃用，现象完全相同）。
pub enum RewriteAttempt {
    /// 可用（已通过 `valid_rewrite_output` + 泛词清洗）。
    Ok(String),
    /// 模型返回了，但内容不可用（空/超长/无检索词/原样回显）。
    Invalid(String),
    /// 超时。
    Timeout(u64),
    /// 未启用（无 LLM 配置或显式跳过）。
    Unavailable,
}

/// 改写超时（秒）。默认 60 —— 这个网关的改写请求常态 4–6 秒，旧值 5 秒会
/// 直接把「她的哥哥今年多少岁了」这类关键改写打死（2026-11-02 实测）。
fn rewrite_timeout_secs() -> u64 {
    std::env::var("LINK_SEARCHER_REWRITE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v >= 5)
        .unwrap_or(60)
}

/// 最近若干条消息的**纯文本**（含助手回答），供"改写引入的实体是否被历史支持"判定。
///
/// 与 [`rewrite_history`] 的区别：这里不带格式、不做角色筛选，只用来做**存在性**
/// 检查 —— 改写里出现的人名/机构名只要在历史里出现过就允许继承。
pub(super) fn recent_history_text(messages: &[ChatMessage], last_q: &str) -> String {
    let mut out = String::new();
    for m in messages {
        let c = m.content.trim();
        if c.is_empty() || c == last_q.trim() {
            continue;
        }
        out.push_str(c);
        out.push('\n');
    }
    out
}

/// 改写结果里**本轮问句没出现**的实体，是否都能在对话历史里找到出处。
///
/// 只针对**实体**（专名 / 含数字的编号）判定 —— 普通词汇（同义词、上位词，例如把
/// 「今年多少岁」改成「出生日期」）不受此限：那不是"指代消解"，只是换词，不该拦。
/// 2026-11-02 实测踩到：模型把「今天是2026年11月12日，各人的年龄是多少」改写成
/// `陈秀芳 陈贤文 … 李耀华 出生日期`（名字全部来自上一轮回答，完全正确），
/// 却因为 `出生日期` 不在历史里被整条弃用 —— 守卫把"换词"当成了"编造实体"。
pub(super) fn entities_backed_by_history(
    rewritten: &str,
    current_q: &str,
    history_text: &str,
) -> bool {
    let cur = current_q.to_lowercase();
    let hist = history_text.to_lowercase();
    for kw in extract_retrieval_keywords(rewritten) {
        let k = kw.to_lowercase();
        if k.chars().count() < 2 {
            continue;
        }
        // 本轮问句已有 → 无需历史
        if cur.contains(&k) {
            continue;
        }
        if !is_entity_like(&k) {
            // 普通词汇/同义改写：不参与实体校验
            continue;
        }
        if hist.contains(&k) {
            continue;
        }
        log::info!("[AI]   llm rewrite 引入了历史里不存在的实体：{kw}");
        return false;
    }
    true
}

/// 这个词像不像"具体对象"（人名/地名/机构名/专名，或含数字的编号/案号）。
/// 决定它是否必须能在历史里找到出处。
fn is_entity_like(word: &str) -> bool {
    if word.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    crate::search::schema::JIEBA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tag(word, true)
        .iter()
        .any(|t| {
            let tag = t.tag.to_string();
            t.word.chars().count() >= 2
                && (tag.starts_with("nr")
                    || tag.starts_with("ns")
                    || tag.starts_with("nt")
                    || tag.starts_with("nz"))
        })
}
///
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

/// 重跑扩查的「真换词」守卫：扩查结果必须引入**本轮问句之外的新检索词**。
///
/// 与 [`retry_expansion_keeps_question_entity`] 互补：那条保"不丢锚点"，这条防"没换词"。
/// 只挡"与原句相同"不够 —— LLM 常把原句删几个虚词后回显，词集与首轮完全相同，
/// 用户点了「换一批」却还是同一批材料（2026-10-16 实测：
/// `汪均益案的最终结果是什么？` → `汪均益案 最终结果`，两轮的 `final_kws` 一模一样）。
///
/// 判据：扩查结果里存在至少一个检索词，与问句的每个检索词**都无包含关系**
/// （"汪均益案" 与 "汪均益" 互为包含 → 视为同词，不算新词）。词集无新增即弃用
/// （调用方回退常规链）。
pub(super) fn retry_expansion_adds_new_terms(expanded: &str, question: &str) -> bool {
    let q_kws = extract_retrieval_keywords(question);
    if q_kws.is_empty() {
        // 问句本身没有检索词可对比 → 无从判定；空输出/泛词由 `valid_rewrite_output` 挡住。
        return true;
    }
    let q_lower: Vec<String> = q_kws.iter().map(|k| k.to_lowercase()).collect();
    extract_retrieval_keywords(expanded).iter().any(|k| {
        let kl = k.to_lowercase();
        !q_lower
            .iter()
            .any(|q| q == &kl || q.contains(kl.as_str()) || kl.contains(q.as_str()))
    })
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
    // 「X案」补回实体 X：jieba 会把类别后缀"案"粘在实体后（"汪均益案的最终结果"
    // → "汪均益案"），而语料里通常没有"X案"这种写法（正文写"汪均益"、"汪均益诉…"），
    // 该词就是**零命中词**；只保留它的话，实体将从 BM25/路径/向量三通道全部消失
    // （2026-10-16 实测："汪均益案的最终结果是什么？"首轮 30 份材料全噪声、答"材料不足"；
    // 同一条管线只要带上"汪均益"就能注入一审判决书/48号裁定并答对）。
    // 只对去尾后 ≥2 字、非泛词、非动作短语（"已立案"→"已立"）的词生效。
    for w in out.clone() {
        let prefix: Option<String> = if let Some(p) = w.strip_suffix("案件") {
            Some(p.to_string())
        } else {
            w.strip_suffix('案').map(|p| p.to_string())
        };
        let Some(prefix) = prefix else { continue };
        if prefix.chars().count() < 2 || is_retrieval_stopword(&prefix) {
            continue;
        }
        // 动作短语（立案/结案/撤案/办案/受案…）不得产出"已立"这类碎片。
        if prefix
            .chars()
            .last()
            .is_some_and(|c| matches!(c, '立' | '诉' | '审' | '结' | '撤' | '办' | '受' | '查' | '处'))
        {
            continue;
        }
        if !out.contains(&prefix) {
            out.push(prefix);
        }
    }
    // 丢弃被更长候选包含的子词片段（"不动"/"动产" ⊆ "不动产"）；例外：**"X案"/"X案件"
    // 不覆盖实体"X"** —— 前者可能根本不在语料里，让"X"活下来才有召回锚点（见上）。
    out
        .iter()
        .filter(|a| {
            !out.iter().any(|b| {
                b.as_str() != a.as_str()
                    && b.chars().count() > a.chars().count()
                    && b.contains(a.as_str())
                    && !(b.ends_with('案')
                        && (b.trim_end_matches('案') == a.as_str()
                            || b.trim_end_matches("案件") == a.as_str()))
            })
        })
        .cloned()
        .collect()
}

/// The pronoun surface itself (longest match first, so "他们" wins over "他").
/// Needed to feed a binding into the clarify engine: we already resolved "他"
/// to a concrete person, so the engine must not report it as ambiguous.
///
/// 注：这里只服务 clarify，不再决定检索词 —— 检索词由 LLM 结合对话改写
/// （2026-11-02 起，规则链只在 LLM 不可用时兜底）。
pub(super) fn person_pronoun_surface(text: &str) -> Option<&'static str> {
    ["他们", "她们", "他", "她", "该人", "此人", "当事人"]
        .iter()
        .find(|p| text.contains(*p))
        .copied()
}

/// 父问句里适合继承的实体：人名 / 地名 / 机构名 / 专名（jieba `nr|ns|nt|nz`）。
///
/// 机构后缀会与前面的专名**拼接**：`万城`(ns) + `公司`(n) → `万城公司`。
/// 不拼的话「万城公司人员列表」只能取出 `万城`，而"办公室"这类省略式追问真正
/// 需要的是整个机构名（2026-11-02 实测）。
pub(super) fn parent_person_entities(text: &str) -> Vec<String> {
    const ORG_SUFFIX: &[&str] = &[
        "公司", "事务所", "集团", "银行", "法院", "委员会", "中心", "医院", "学校", "大学", "局",
        "厂", "店",
    ];
    let tokens: Vec<(String, String)> = crate::search::schema::JIEBA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tag(text, true)
        .iter()
        .map(|t| (t.word.to_string(), t.tag.to_string()))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for (i, (w, tag)) in tokens.iter().enumerate() {
        if w.chars().count() < 2 {
            continue;
        }
        let next_org_suffix = tokens.get(i + 1).and_then(|(next, ntag)| {
            (ntag.starts_with('n') && ORG_SUFFIX.contains(&next.as_str())).then_some(next.as_str())
        });
        let is_proper = tag.starts_with("nr")
            || tag.starts_with("ns")
            || tag.starts_with("nt")
            || tag.starts_with("nz");
        // 专名直接收；普通名词**只有**在后接机构后缀时才收（"万城"+"公司"）。
        // 后者是因为 jieba 对"万城"这类未登录专名可能只给 `n`，但它显然是机构名的一部分。
        let accepted = is_proper || (next_org_suffix.is_some() && tag.starts_with('n'));
        if !accepted {
            continue;
        }
        let mut entity = w.clone();
        if let Some(suffix) = next_org_suffix {
            entity.push_str(suffix);
        }
        if !out.contains(&entity) {
            out.push(entity);
        }
    }
    out
}

/// 检索级规则兜底：父问句的实体 + 本轮问句（无实体时用父问句关键词）。
///
/// 注：代词不再单独短路检索 —— 那会跳过"结合对话改写"这一步，把上一轮的
/// 主题词（身份证号/多少）带进来（2026-11-02 实测）。这里只作为 LLM 不可用时的兜底。

/// Scrubs verbs/quantifiers that an LLM rewrite copied over from the parent
/// turn but that the current question never asked for.
///
/// 2026-11-02 实测：追问 `这些文号里有没有重复的？` 的 LLM 改写是
/// `申洲 大通 会计师 事务所 做过 多份 验资 报告 所有 这些文号里有没有重复的？`
/// —— `做过/多份/所有` 是上一轮的动词量词，只会稀释 BM25（hits 从 7239 涨到 9873）。
///
/// 只删**本轮问句里没有的**词：用户自己写的"所有"必须保留。
pub(super) fn strip_inherited_generic_terms(rewritten: &str, current_q: &str) -> String {
    let kept: Vec<&str> = rewritten
        .split_whitespace()
        .filter(|t| {
            let t = t.trim();
            if t.is_empty() || current_q.contains(t) {
                return true;
            }
            !super::sufficiency::is_non_entity_word(t)
        })
        .collect();
    if kept.is_empty() {
        rewritten.to_string()
    } else {
        kept.join(" ")
    }
}

/// 规则兜底用的"父问句关键词"。
///
/// **优先专有名词**（人名/地名/机构名/专名 `nr|ns|nt|nz`）：追问真正需要继承的是
/// "在说谁/说哪家公司"，而不是父问句的整串主题词。2026-11-02 实测：父问句
/// `陈正先的身份证号是多少？` 取前 3 个关键词会把 `身份证号` 带进「那他涉及哪些案件」，
/// 而 `万城公司人员列表` → 只取 `万城公司`，正好是「办公室」这类省略式追问缺的那一半。
fn parent_keywords(text: &str, max: usize) -> Vec<String> {
    let entities = parent_person_entities(text);
    if !entities.is_empty() {
        return entities.into_iter().take(max).collect();
    }
    let mut out = Vec::new();
    for t in crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner()).cut(text, false) {
        let w = t.word.trim();
        if w.chars().count() < 2 || is_rewrite_stopword(w) {
            continue;
        }
        // 继承父问句关键词时也要滤掉动词/量词（"做过/多份/所有"），
        // 否则规则改写路径会把上一轮的问法带进本轮检索。
        if super::sufficiency::is_non_entity_word(w) {
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

/// 规则兜底的继承准入：**只**在 LLM 改写不可用/无效时使用。
///
/// 两条准入（任一成立）：① 本轮问句本身要借历史（[`needs_history_rewrite`]）；
/// ② 与父问句共享检索词（同一话题）。反过来就是 P2：自足 + 零交集 = 新话题 ⇒ 不继承。
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

/// 规则兜底：LLM 不可用时，把父问句的核心实体补进检索词。
///
/// 只在 `rewrite_method == "rule"` 时调用 —— LLM 改写成功的场合不要再补，
/// 否则会把上一轮的主题词（"身份证号"）又加回去（2026-11-02 实测）。
pub(super) fn ensure_parent_entities(search_q: &str, parent_q: &str, current_q: &str) -> String {
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
