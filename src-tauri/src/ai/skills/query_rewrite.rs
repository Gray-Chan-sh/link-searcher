//! QueryRewrite Skill：将用户的追问改写为独立可检索的查询。

use crate::commands::ai::{ChatMessage, rewrite_query, llm_rewrite_query};
use crate::ai::skills::{Skill, SkillError};

pub struct QueryRewriteInput {
    pub last_q: String,
    pub messages: Vec<ChatMessage>,
}

pub struct QueryRewriteOutput {
    pub search_q: String,
    pub rewritten: bool,
    pub original_q: String,
}

pub struct QueryRewriteSkill;

impl Skill for QueryRewriteSkill {
    fn name(&self) -> &str { "QueryRewrite" }
}

impl QueryRewriteSkill {
    pub async fn execute(&self, input: &QueryRewriteInput) -> Result<QueryRewriteOutput, SkillError> {
        let last_q = input.last_q.trim();
        if last_q.is_empty() {
            return Ok(QueryRewriteOutput { search_q: String::new(), rewritten: false, original_q: input.last_q.clone() });
        }
        let rule = rewrite_query(last_q, &input.messages);
        let search_q = match llm_rewrite_query(last_q, &input.messages).await {
            Some(llm) if llm != rule.query => llm,
            _ => rule.query,
        };
        let rewritten = search_q != last_q;
        log::info!("[QueryRewrite] original={} rewritten={} search_q={}", last_q, rewritten, search_q);
        Ok(QueryRewriteOutput { search_q, rewritten, original_q: input.last_q.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn msg(r: &str, c: &str) -> ChatMessage { ChatMessage { role: r.into(), content: c.into() } }

    #[tokio::test]
    async fn empty_input_returns_empty() {
        let r = QueryRewriteSkill.execute(&QueryRewriteInput { last_q: "".into(), messages: vec![] }).await.unwrap();
        assert!(r.search_q.is_empty()); assert!(!r.rewritten);
    }

    #[tokio::test]
    async fn complete_question_not_rewritten() {
        let r = QueryRewriteSkill.execute(&QueryRewriteInput { last_q: "这份合同的法律风险是什么？".into(), messages: vec![] }).await.unwrap();
        assert_eq!(r.search_q, "这份合同的法律风险是什么？");
        assert!(!r.rewritten);
    }

    #[tokio::test]
    async fn deictic_pronoun_triggers_rewrite() {
        let r = QueryRewriteSkill.execute(&QueryRewriteInput {
            last_q: "它呢".into(),
            messages: vec![msg("user", "请总结这份年度财务报告"), msg("assistant", "报告显示营收增长 20%。"), msg("user", "它呢")],
        }).await.unwrap();
        assert!(r.rewritten);
        assert!(r.search_q.contains("年度"), "rewritten: {}", r.search_q);
    }
}
