//! 追问改写：LLM 结合对话把提问改写成可独立检索的查询，并给出专名实体。
//!
//! 2026-11-06 起**没有"规则改写"这条分支**：哪些词有区分度、哪个词是实体，一律由
//! 模型裁决。此前用 jieba + 停用词表在 LLM 之后再"审"一遍，导致同一问题的答案
//! 取决于用户挑哪个同义词（「情况」在词表里 → 答对；「始末」不在 → 答不出）。

use super::{ChatMessage, truncate_text};

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

/// 继承父问句实体时用的角色/亲属名词：jieba 常把「哥哥/父亲」标成 `nr`，但它们
/// **不是具体对象**，不能当作可继承的锚点实体。
const ROLE_NOUNS: &[&str] = &[
    "哥哥", "姐姐", "弟弟", "妹妹", "兄弟", "姐妹", "父亲", "母亲", "爸爸", "妈妈", "爷爷",
    "奶奶", "外公", "外婆", "儿子", "女儿", "妻子", "丈夫", "前妻", "前夫", "配偶", "家属",
    "家人", "亲戚", "本人", "对方", "当事人",
];

fn is_role_noun(word: &str) -> bool {
    ROLE_NOUNS.contains(&word)
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
}

#[cfg(test)]
mod history_backed_entity_tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage { role: role.into(), content: content.into() }
    }

    /// 改写输出解析：JSON（query / entities / needs / anchor / self_contained / basis）是主格式。
    #[test]
    fn parses_json_rewrite_reply() {
        let raw = r#"{"query":"王会芹 一审 二审 判决","entities":["王会芹"],"needs":[],"anchor":[],"self_contained":"yes","basis":"docs"}"#;
        let r = parse_rewrite_reply(raw);
        assert_eq!(r.query, "王会芹 一审 二审 判决");
        assert_eq!(r.entities, vec!["王会芹".to_string()]);
        assert_eq!(r.self_contained, Some(true));
        assert_eq!(r.basis, AnswerBasis::Docs);
        assert!(r.needs.is_empty() && r.anchor.is_empty());
    }

    /// `both` 的聚焦字段：缺什么事实、该去谁的材料里找。
    #[test]
    fn parses_focus_fields_for_both() {
        let raw = r#"{"query":"常宏 陈秀芳 出生日期","entities":["常宏","陈秀芳"],"needs":["出生日期"],"anchor":["陈秀芳"],"self_contained":"no","basis":"both"}"#;
        let r = parse_rewrite_reply(raw);
        assert_eq!(r.needs, vec!["出生日期".to_string()]);
        assert_eq!(r.anchor, vec!["陈秀芳".to_string()]);
        assert_eq!(r.basis, AnswerBasis::Both);
    }

    /// 「这些人谁年龄最大」这类纯追问：模型应判 history。
    #[test]
    fn parses_history_basis() {
        let raw = r#"{"query":"刘兴芳 周传华 金纯熙 年龄","entities":[],"self_contained":"no","basis":"history"}"#;
        let r = parse_rewrite_reply(raw);
        assert_eq!(r.self_contained, Some(false));
        assert_eq!(r.basis, AnswerBasis::History);
    }

    /// 模型漏给 basis ⇒ 保守退回 docs（宁可多检索，不少检索）；漏给聚焦字段 ⇒ 空表。
    #[test]
    fn missing_basis_defaults_to_docs() {
        let raw = r#"{"query":"陈秀芳 家庭成员","entities":["陈秀芳"]}"#;
        let r = parse_rewrite_reply(raw);
        assert_eq!(r.basis, AnswerBasis::Docs);
        assert!(r.needs.is_empty() && r.anchor.is_empty());
    }

    /// 模型常把 JSON 包在 ```json 代码块里，或前后带一句解释。
    #[test]
    fn parses_json_inside_fences() {
        let raw = "```json\n{\"query\":\"陈秀芳 兄长 出生日期\",\"entities\":[\"陈秀芳\"],\"self_contained\":\"no\",\"basis\":\"both\"}\n```";
        let r = parse_rewrite_reply(raw);
        assert_eq!(r.query, "陈秀芳 兄长 出生日期");
        assert_eq!(r.entities, vec!["陈秀芳".to_string()]);
        assert_eq!(r.self_contained, Some(false));
        assert_eq!(r.basis, AnswerBasis::Both);
    }

    /// 旧格式（两行 QUERY / SELF_CONTAINED）仍要能解析：网关/model 换版时不能让整轮
    /// 改写直接失效。entities 缺失 ⇒ 空表，basis 缺失 ⇒ docs。
    #[test]
    fn parses_legacy_two_line_reply() {
        let r = parse_rewrite_reply("QUERY: 陈秀芳 兄长 出生日期\nSELF_CONTAINED: no");
        assert_eq!(r.query, "陈秀芳 兄长 出生日期");
        assert!(r.entities.is_empty(), "{:?}", r.entities);
        assert!(r.needs.is_empty() && r.anchor.is_empty());
        assert_eq!(r.self_contained, Some(false));
        assert_eq!(r.basis, AnswerBasis::Docs);
    }

    /// `both` 聚焦：上一轮已答过的对象不再占检索词。
    #[test]
    fn refocus_drops_already_answered_entity() {
        let got = refocus_query(
            AnswerBasis::Both,
            "常宏 陈秀芳 出生日期",
            &["出生日期".to_string()],
            &["陈秀芳".to_string()],
        )
        .expect("both + anchor 应当聚焦");
        assert_eq!(got.0, "陈秀芳 出生日期");
        assert_eq!(got.1, vec!["陈秀芳".to_string()]);
    }

    /// 锚点必须保留：要查的事实属于旧对象时（爱人的名字只能从常宏的材料里找），
    /// 不能把常宏从检索词里删掉。
    #[test]
    fn refocus_keeps_anchor_needed_to_locate_new_person() {
        let got = refocus_query(
            AnswerBasis::Both,
            "常宏 陈秀芳 爱人 出生日期",
            &["配偶".to_string(), "出生日期".to_string()],
            &["常宏".to_string(), "陈秀芳".to_string()],
        )
        .expect("both + anchor 应当聚焦");
        assert_eq!(got.0, "常宏 陈秀芳 配偶 出生日期");
        assert_eq!(got.1, vec!["常宏".to_string(), "陈秀芳".to_string()]);
    }

    /// 只在 both 生效：docs / history 的检索词一个字都不动。
    #[test]
    fn refocus_is_off_for_docs_and_history() {
        assert!(refocus_query(AnswerBasis::Docs, "q", &["n".into()], &["a".into()]).is_none());
        assert!(refocus_query(AnswerBasis::History, "q", &["n".into()], &["a".into()]).is_none());
    }

    /// 模型没给 anchor ⇒ 不聚焦（与旧行为一致）。
    #[test]
    fn refocus_is_noop_without_anchor() {
        assert!(refocus_query(AnswerBasis::Both, "q", &["n".into()], &[]).is_none());
    }

    /// 用**语料事实**过滤改写词：语料里不存在的词被丢弃，同义词只要存在就保留。
    /// 这是替代"词性/词表判断实体"的关键：不需要知道「兄长」是不是人名。
    #[test]
    fn strips_terms_absent_from_corpus() {
        // df: 陈秀芳/兄长/出生日期 存在；张三不存在
        let df = |w: &str| match w {
            "陈秀芳" => 83,
            "兄长" => 12,
            "出生日期" => 500,
            "张三" => 0,
            _ => 0,
        };
        let out = strip_nonexistent_terms("陈秀芳 张三 兄长 出生日期", "各人的年龄", df)
            .expect("仍有可用词");
        assert!(out.contains("陈秀芳"), "{out}");
        assert!(out.contains("兄长"), "语料里存在的同义词必须保留: {out}");
        assert!(!out.contains("张三"), "语料里没有的词要丢弃: {out}");
    }

    /// 用户自己写的词不查语料（可能是生僻写法），全部被过滤光时返回 None。
    #[test]
    fn keeps_user_typed_terms_and_reports_empty() {
        let none = |_: &str| 0u64;
        // 问句自带的词一律保留
        assert_eq!(
            strip_nonexistent_terms("某个生僻词", "某个生僻词", none).as_deref(),
            Some("某个生僻词")
        );
        // 既不在问句、语料里也没有 ⇒ 丢弃 ⇒ 空 ⇒ None
        assert!(strip_nonexistent_terms("张三 李四", "各人的年龄", none).is_none());
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

    /// 回归（2026-11-03）：旧实现的沿用闸门要求"本轮命中 < 30"，而正常检索动辄数千命中
    /// ⇒ 追问轮从不沿用上一轮证据（实测 `from_history=0`），模型据此编出错误的出生年。
    #[test]
    fn history_carry_is_not_gated_by_hit_count() {
        // 大量命中 + 依赖上文的追问 ⇒ 仍必须沿用
        assert!(should_carry_history(true, true, false, false));
        // 零命中 ⇒ 沿用
        assert!(should_carry_history(true, false, false, true));
        // 点名了文件 ⇒ 沿用
        assert!(should_carry_history(true, false, true, false));
        // 首轮 / 自足的新问题 ⇒ 不沿用
        assert!(!should_carry_history(false, true, false, false));
        assert!(!should_carry_history(true, false, false, false));
    }
}

/// 本轮回答应以什么为依据（由模型在改写时判定，2026-11-06）。
///
/// - `History`：只靠前几轮已给出的信息就能回答（排序/求年龄/算平均/比较/汇总）
///   → **跳过整条检索**，把上文升格为材料；
/// - `Both`：需要前几轮的对象 + 少量新事实 → 检索但收窄注入量；
/// - `Docs`：新问题 → 完整 RAG（原有行为）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AnswerBasis {
    History,
    Both,
    #[default]
    Docs,
}

impl AnswerBasis {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "history" => Some(Self::History),
            "both" => Some(Self::Both),
            "docs" | "documents" => Some(Self::Docs),
            _ => None,
        }
    }
}

/// 解析改写输出。
///
/// 主格式是**一行 JSON**（模型据此同时给出查询串、实体表与"是否依赖上文"）：
///
/// ```json
/// {"query":"王会芹 一审 二审 判决","entities":["王会芹"],"self_contained":"yes"}
/// ```
///
/// 兼容旧格式（`QUERY:` / `SELF_CONTAINED:` 两行、或只回一行查询串）：此时 `entities`
/// 为空表，检索只用 `query` 本身。
/// 改写输出的解析结果。
///
/// `needs` / `anchor` 是 2026-11-07 新增的**聚焦字段**（只在 `basis=both` 时起作用）：
/// - `needs`：本轮还缺的事实词（出生日期 / 身份证号 / 金额 / 签订日期…）；
/// - `anchor`：必须去哪些对象的材料里找这些事实（只填专名）。
///
/// 没有这两个字段是合法的（老格式/模型没给）→ 空表，行为与之前完全一致。
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct RewriteParse {
    pub query: String,
    pub entities: Vec<String>,
    pub needs: Vec<String>,
    pub anchor: Vec<String>,
    pub self_contained: Option<bool>,
    pub basis: AnswerBasis,
}

/// 清洗模型给的词表：去空白、去空串、封顶 6 个（防止模型把整句塞进来）。
fn clean_terms(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in v {
        let t = t.trim().to_string();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
        if out.len() >= 6 {
            break;
        }
    }
    out
}

fn parse_rewrite_reply(raw: &str) -> RewriteParse {
    if let Some(r) = parse_rewrite_json(raw) {
        return r;
    }
    let mut query: Option<String> = None;
    let mut self_contained: Option<bool> = None;
    for line in raw.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        if let Some(rest) = lower.strip_prefix("query:") {
            // 用原行切片取回大小写无关的正文
            let start = t.len() - rest.len();
            query = Some(t[start..].trim().to_string());
        } else if lower.starts_with("self_contained:") {
            let v = lower.split(':').nth(1).unwrap_or("").trim().to_string();
            self_contained = match v.as_str() {
                "no" | "false" | "0" | "否" => Some(false),
                "yes" | "true" | "1" | "是" => Some(true),
                _ => None,
            };
        }
    }
    let q = match query {
        Some(q) if !q.is_empty() => q,
        // 没有 QUERY: 前缀 → 视为旧格式，整段就是查询
        _ => raw.trim().to_string(),
    };
    // 旧格式没有 basis 字段 ⇒ 退回完整 RAG（保守，不会少检索）。
    RewriteParse {
        query: q,
        entities: Vec::new(),
        needs: Vec::new(),
        anchor: Vec::new(),
        self_contained,
        basis: AnswerBasis::Docs,
    }
}

/// 从模型回复里抠出第一个 JSON 对象（允许 ```json 围栏、前后解释文字）。
fn parse_rewrite_json(raw: &str) -> Option<RewriteParse> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    #[derive(serde::Deserialize)]
    struct Reply {
        #[serde(default)]
        query: String,
        #[serde(default)]
        entities: Vec<String>,
        #[serde(default)]
        needs: Vec<String>,
        #[serde(default)]
        anchor: Vec<String>,
        #[serde(default)]
        self_contained: Option<serde_json::Value>,
        #[serde(default)]
        basis: Option<String>,
    }
    let reply: Reply = serde_json::from_str(&raw[start..=end]).ok()?;
    let query = reply.query.trim().to_string();
    if query.is_empty() {
        return None;
    }
    let self_contained = match reply.self_contained {
        Some(serde_json::Value::Bool(b)) => Some(b),
        Some(serde_json::Value::String(s)) => match s.trim().to_lowercase().as_str() {
            "yes" | "true" | "1" | "是" => Some(true),
            "no" | "false" | "0" | "否" => Some(false),
            _ => None,
        },
        _ => None,
    };
    let basis = reply
        .basis
        .as_deref()
        .and_then(AnswerBasis::parse)
        .unwrap_or_default();
    Some(RewriteParse {
        query,
        entities: clean_terms(reply.entities),
        needs: clean_terms(reply.needs),
        anchor: clean_terms(reply.anchor),
        self_contained,
        basis,
    })
}

/// 用**语料事实**过滤改写词：正文索引里一次都没出现过的词直接丢弃。
///
/// 这替代了"像不像实体"的词性/词表判断：编造的人名（语料里没有）自动出局，
/// 而「兄长 / 家兄」这类同义词只要语料里出现过就保留 —— 不需要任何封闭词表。
///
/// 本轮问句里用户自己写的词**不校验**（用户可能故意用生僻写法）。
/// 全部被丢光 ⇒ 返回 `None`，调用方退回原问句（不使用任何词典）。
pub(super) fn strip_nonexistent_terms(
    rewritten: &str,
    current_q: &str,
    df: impl Fn(&str) -> u64,
) -> Option<String> {
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped: Vec<&str> = Vec::new();
    for token in rewritten.split_whitespace() {
        if token.is_empty() {
            continue;
        }
        if current_q.contains(token) || df(token) > 0 {
            kept.push(token);
        } else {
            dropped.push(token);
        }
    }
    if !dropped.is_empty() {
        log::info!("[AI]   rewrite 丢弃语料中不存在的词：{dropped:?}");
    }
    if kept.is_empty() {
        None
    } else {
        Some(kept.join(" "))
    }
}

/// Validate an LLM rewrite response: non-empty, not echoing the original, and
/// not absurdly long.
///
/// 这里**不再**用停用词表判断"是否还剩检索词"：哪些词有区分度由模型决定
/// （2026-11-06：词典裁决把「始末」当实体、把「情况」当泛词，同一问题的答案
/// 取决于用户挑哪个同义词）。空输出/原样回显这类**结构性**问题仍在此拦截。
pub(super) fn valid_rewrite_output(s: &str, original: &str) -> Option<String> {
    let t = s.trim().trim_matches(['"', '\'', '“', '”']);
    if t.is_empty() || t == original.trim() || t.chars().count() > 80 {
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
/// 改写提示词。**静态、无插值**，放在模块级是为了让设置页能展示"实际生效的那一份"，
/// 而不是让文档另抄一份（抄的那份迟早漂移）。
pub(crate) const REWRITE_SYSTEM_PROMPT: &str = "你是检索查询改写助手。用户在与本地文档对话，你的任务有三件：\
① 把提问改写成一条可独立检索的中文查询；② 判断这条查询依赖哪些专名实体；③ 判断这一轮**还需不需要看文档**。\
规则：\
1) 补全指代与省略：代词（他/她/它/他们/这个/那个/该/上述/前者/后者）和**省略主语**的追问（例如上一轮问「万城公司人员列表」，本轮只问「办公室」「公司办公室」「各人的年龄」「标的最大的呢」「兄长呢」），必须从上文找出它真正指向的人名/机构名/案件名补进检索词。\
2) 继承的实体必须**来自对话历史**（用户的提问或助手的回答），不得凭空编造。\
3) 若当前问题本身就是一个完整、独立的新问题（自带明确的人名/机构名/案件名/文件号），就只围绕它改写，**不要**引入历史里的对象。\
4) 助手的历史回答只用于判断「这句话在说什么」，**不得**把它当作事实写进检索词。\
5) query 用最小必要检索词（4～12 个，空格分隔），保留有区分度的词，去掉「的/了/吗/呢」这类虚词；\
不要堆同义词，也不要把「案件/文件/材料/情况/结果」这类在语料里到处都是的词当主检索词。\
query 里每个词都要能通过一个检验：**如果答案在那份文档里，那份文档会不会写出这个词？**\
会 → 保留（出生日期、身份证号、判决、签订日期）；不会 → 不要写（谁大、大小、哪个更多、先后、怎么样）。\
比较/排序/汇总/换算本身由回答阶段处理（用户的原话里已经说清了），检索不需要、也不应该重复表达它。\
6) entities 只填**能在语料里定位到具体对象的专名**：人名、机构名、案号、项目名、地点。\
「始末 / 情况 / 经过 / 来龙去脉 / 最早 / 结果」这类**描述性/过程性词不是实体，禁止写进 entities** —— 它们不会出现在卷宗正文里，写进去会把检索带偏。没有专名时给空数组。\
7) basis 判断这一轮该以什么为依据：\
   - \"history\"：**只靠前几轮已经给出的信息就能回答** —— 例如对上一轮列出的名单/金额/日期做排序、求年龄、算平均、比较、汇总、换一种说法重述。不需要任何新文档。\
     **凡是对前几轮已经列出的名单/数值做排序、求年龄、算平均/中位数、合计、比大小、换算单位的，一律填 \"history\"，不要填 \"docs\"** —— 这些只是对已知数值的计算，再看原文没有任何增益。\
   - \"both\"：需要前几轮的对象，但还缺少量事实（如某人的身份证号、某个金额），要再查一点文档。\
   - \"docs\"：新问题/换了对象/前几轮没有相关结论，需要重新检索文档。\
   拿不准时填 \"docs\"；**这是第一轮（没有历史）时必须填 \"docs\"**。\
8) needs / anchor 只在 basis=\"both\" 时起作用，用来指明\"这一轮到底还缺什么、该去谁的材料里找\"：\
   needs 填 1~3 个**还缺的事实词**（出生日期 / 身份证号 / 金额 / 签订日期…），仍按规则 5 的\"材料会不会写\"检验；\
   anchor 填**必须去查的对象**（只填专名）—— **已经答过、本轮不再需要的事实所属对象，不要写进 anchor。**\
   例1：上一轮已答过常宏的生日，本轮问「他和陈秀芳谁大」→ needs=[\"出生日期\"]、anchor=[\"陈秀芳\"]（常宏不用再查）。\
   例2：本轮问「他的爱人和陈秀芳谁大」（爱人姓名未知）→ needs=[\"配偶\",\"出生日期\"]、anchor=[\"常宏\",\"陈秀芳\"]（爱人只能从常宏的材料里找，必须保留常宏）。\
   basis 是 history / docs 时给空数组。\
输出**一行 JSON**（不要代码块、不要解释）：\
{\"query\":\"<检索词>\",\"entities\":[\"<专名>\", ...],\"needs\":[\"<还缺的事实>\"],\"anchor\":[\"<要查谁的材料>\"],\"self_contained\":\"yes|no\",\"basis\":\"history|docs|both\"}\
self_contained 表示**这句话离开上文还能不能读懂**：依赖指代/省略（他、它、各人、办公室、兄长呢…）填 no；自带完整对象（陈秀芳的生日是哪天）填 yes。";

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
    // 关键新增（2026-11-06）：让模型判断这一轮**要不要再看文档**。
    // 多轮会话里大量追问只是对上一轮结论做比较/换算/汇总（"这些人谁最大"
    // "各人年龄多少"），再跑一遍全库检索既慢又会让事实来源漂移。
    let has_history = messages.iter().filter(|m| m.role == "user").count() > 1;
    let system = REWRITE_SYSTEM_PROMPT;
    let user = format!("{history_str}\n当前问题：{last_q}");
    let sys = system.to_string();
    let budget = rewrite_timeout_secs();
    let started = std::time::Instant::now();
    let mut last_err = String::new();
    // 网关偶发断连（实测 `peer closed connection without sending TLS close_notify`，
    // 1 秒内就失败）会让整轮改写退化成原句 —— 对指代型追问等于直接答错。
    // 一次失败不足以判定网关不可用，重试一次。
    for attempt in 0..2u32 {
        let sys_c = sys.clone();
        let user_c = user.clone();
        let fut = tokio::task::spawn_blocking(move || crate::ai::chat(&sys_c, &user_c));
        match tokio::time::timeout(std::time::Duration::from_secs(budget), fut).await {
            Ok(Ok(Some(s))) => {
                let r = parse_rewrite_reply(&s);
                // 首轮（没有历史）不可能"只靠上文" —— 模型偶发误判时兜底为 docs。
                let basis = if has_history { r.basis } else { AnswerBasis::Docs };
                return match valid_rewrite_output(&r.query, last_q) {
                    Some(v) => RewriteAttempt::Ok(RewriteOk {
                        query: v,
                        entities: r.entities,
                        needs: r.needs,
                        anchor: r.anchor,
                        self_contained: r.self_contained,
                        basis,
                    }),
                    None => RewriteAttempt::Invalid(truncate_text(s.trim(), 60)),
                };
            }
            Ok(Ok(None)) => {
                last_err = "gateway returned no content".to_string();
                log::warn!("[AI]   llm rewrite attempt {} failed: {last_err}", attempt + 1);
            }
            Ok(Err(e)) => {
                last_err = format!("task failed: {e}");
                log::warn!("[AI]   llm rewrite attempt {} failed: {last_err}", attempt + 1);
            }
            Err(_) => {
                log::warn!(
                    "[AI]   llm rewrite timeout after {}s（q=\"{}\"）",
                    started.elapsed().as_secs(),
                    truncate_text(last_q, 30)
                );
                return RewriteAttempt::Timeout(budget);
            }
        }
        if attempt == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    RewriteAttempt::Invalid(last_err)
}

/// Outcome of one LLM rewrite attempt. The caller must be able to tell
/// "模型没答" 与 "模型答了但不可用" 与 "超时" —— 三者以前都塌成 `None`，
/// 日志里只剩 `method=rule`，查不出是网关慢还是守卫拦的（2026-11-02 实测：
/// 一次 5.0s 超时与一次被守卫弃用，现象完全相同）。
pub enum RewriteAttempt {
    /// 可用（已通过 `valid_rewrite_output` + 语料存在性过滤）。
    Ok(RewriteOk),
    /// 模型返回了，但内容不可用（空/超长/原样回显）。
    Invalid(String),
    /// 超时。
    Timeout(u64),
    /// 未启用（无 LLM 配置或显式跳过）。
    Unavailable,
}

/// 一次成功的改写。
pub struct RewriteOk {
    pub query: String,
    /// 模型判定的**专名实体**（人名/机构名/案号/项目名）。检索与"要不要补查"
    /// 都以它为准 —— 这是替换掉 jieba + 停用词表的唯一裁决点（2026-11-06）。
    ///
    /// 为空表是合法结果（问题里没有专名，比如"合同违约金上限是多少"）。
    pub entities: Vec<String>,
    /// 本轮**还缺的事实词**（只在 `basis=both` 时参与聚焦检索）：出生日期 / 身份证号 /
    /// 金额 / 签订日期这类"材料里会写出来"的词。
    pub needs: Vec<String>,
    /// 必须**去哪些对象的材料里**找 `needs`（只填专名）。`both` 时检索只查这些对象，
    /// 避免上一轮已答过的对象继续霸占注入名额（2026-11-07 实测 9/12 是旧对象）。
    pub anchor: Vec<String>,
    /// 模型自己判断「这句话离开上文读不懂」。
    ///
    /// 这是"要不要沿用上一轮材料"的**主判据**：模型看得到完整对话，
    /// `兄长 / 家兄 / 各人 / 办公室` 这类说法对它都是普通语义，不需要我们列词表。
    /// `None` 表示模型没给这个字段（老格式/解析失败）→ 调用方按"有历史就沿用"处理。
    pub self_contained: Option<bool>,
    /// 本轮该以什么为依据（history / both / docs），决定要不要跳过检索。
    pub basis: AnswerBasis,
}

/// `basis=both` 时的**聚焦检索词**：只查"还缺的事实 + 该去哪找"。
///
/// 起因（2026-11-07 实测）：上一轮答过常宏的生日，本轮问「他和陈秀芳谁大」，
/// 改写给出 `entities=[常宏,陈秀芳]`，而 BM25 用 `entities` 做 OR 查询 —— 常宏是
/// 超高频实体（1375 命中），12 个注入名额被它占掉 9 个。
///
/// `needs` / `anchor` 是模型对"缺什么、去哪找"的判断：
/// - `anchor=[陈秀芳]` + `needs=[出生日期]` → `陈秀芳 出生日期`（旧对象不再重查）；
/// - `anchor=[常宏,陈秀芳]` + `needs=[配偶, 出生日期]` → 保留常宏（爱人的名字
///   只能从他的材料里找到）。
///
/// 只有 `both` 且 `anchor` 非空才生效；其余情况返回 `None`（行为与之前完全一致）。
pub(super) fn refocus_query(
    basis: AnswerBasis,
    query: &str,
    needs: &[String],
    anchor: &[String],
) -> Option<(String, Vec<String>)> {
    if basis != AnswerBasis::Both || anchor.is_empty() {
        return None;
    }
    let mut terms: Vec<String> = Vec::new();
    for t in anchor.iter().chain(needs.iter()) {
        let t = t.trim();
        if t.is_empty() || terms.iter().any(|x| x == t) {
            continue;
        }
        terms.push(t.to_string());
    }
    if terms.is_empty() {
        return None;
    }
    let focused = terms.join(" ");
    if focused == query.trim() {
        return None;
    }
    let anchor_kws = anchor
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Some((focused, anchor_kws))
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

/// 「换一批证据」用的查询扩展提示词（静态、无插值），同 [`REWRITE_SYSTEM_PROMPT`] 的理由放在模块级。
pub(crate) const REWRITE_RETRY_SYSTEM_PROMPT: &str = "你在为本地文档检索**换一批检索词**。上一轮用问题原词检索失败（材料里找不到能回答的内容），现在必须换一组**不同的**词再搜。硬性要求：1) 只围绕同一个问题，不换话题，不引入问题之外的人名/机构名/案名；2) **必须保留**问题里的区分性实体（人名/机构/案号/年份/项目名），一个字都不能改；3) **必须换词**：不许只把原句删掉“的/是/什么”变短一点就交差，要补上同义表述、上位词，以及这类问题在文档里通常出现的**结论性词** —— 例如问结果的补：判决/裁定/调解/结案/结论/处理结果；问时间的补：签订/履行/起止；问金额的补：金额/结算/付款（按问题类型自己判断）；4) 输出 6～12 个检索词，用空格分隔；不要解释、不要引号、不要照抄上一轮回答。";

///
/// 重跑（卡片「换一批证据」）专用的**查询扩展**：上一轮用问题原词没能找到能直接回答
/// 的材料，这次换一组**不一样**但更可能命中的检索词。
///
/// 与 [`llm_rewrite_query`] 的分工：
/// - 输入是**本轮原始问句**（不是上一条回答 —— 那正是被修掉的 bug）外加上一轮回答；
/// - 任务不是"补全指代"，而是"换词"：首轮没答上多半是检索词过于具体、或用了语料里
///   没有的说法，所以要求保留区分性实体（人名/案号/年份），补上同义词、上位词与
///   结论性词（判决/裁定/调解/结案…看问题而定）；
/// - 仍走 [`valid_rewrite_output`]：空输出、原样回显一律视为无效。
///
/// 返回 `None` ⇒ 调用方用原问句检索：**"换一批"绝不能退化成"同一个查询再搜一次"**
/// （那就是白点一次），但更不能因为扩展失败就不回答。
pub async fn llm_expand_retry_query(question: &str, prior_answer: &str) -> Option<String> {
    let q = question.trim();
    if q.is_empty() || !crate::ai::llm_enabled() {
        return None;
    }
    let system = REWRITE_RETRY_SYSTEM_PROMPT;
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

/// The pronoun surface itself (longest match first, so "他们" wins over "他").
/// Needed to feed a binding into the clarify engine: we already resolved "他"
/// to a concrete person, so the engine must not report it as ambiguous.
///
/// 注：这里只服务 clarify，不决定检索词 —— 检索词由 LLM 结合对话改写。
pub(super) fn person_pronoun_surface(text: &str) -> Option<&'static str> {
    ["他们", "她们", "他", "她", "该人", "此人", "当事人"]
        .iter()
        .find(|p| text.contains(*p))
        .copied()
}

/// 父问句里适合继承的实体：人名 / 地名 / 机构名 / 专名（jieba `nr|ns|nt|nz`）。
///
/// 仅供 clarify 的代词绑定使用（把"他"绑定成上一轮的人名），**不再决定检索词**。
///
/// 机构后缀会与前面的专名**拼接**：`万城`(ns) + `公司`(n) → `万城公司`。
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
        if !accepted || is_role_noun(w) {
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

/// Whether a prior turn's source file should be carried forward as evidence
/// for the current follow-up turn. A file is kept when the follow-up question
/// names it OR when the current turn found no hits at all (so history is the
/// only available evidence). Prevents the cross-turn evidence gap where a
/// generic follow-up ("列出清单/把时间列出来") drops the prior turn's cited
/// documents because the question never names the file.
pub(super) fn carry_forward_source(is_named: bool, all_hits_empty: bool) -> bool {
    is_named || all_hits_empty
}

/// 追问轮是否沿用上一轮引用的材料。
///
/// 2026-11-03 回归：旧实现额外要求 `all_hits.len() < 30`，而正常检索动辄数千命中，
/// 于是这条"证据延续"从未生效过（实测 `from_history=0`，追问轮注入的全是无关材料，
/// 模型据此编出了错误的出生年）。判据只应与"是不是追问"和"问句是否依赖上文"有关，
/// 与"本轮命中多少"无关。
pub(super) fn should_carry_history(
    is_follow_up: bool,
    context_dependent: bool,
    is_named: bool,
    all_hits_empty: bool,
) -> bool {
    is_follow_up && (context_dependent || carry_forward_source(is_named, all_hits_empty))
}
