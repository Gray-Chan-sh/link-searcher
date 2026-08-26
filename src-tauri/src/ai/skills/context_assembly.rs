//! ContextAssembly Skill：将检索结果和 @mention 文件组装为 LLM prompt 和 evidence 列表。

use crate::commands::ai::{EvidenceItem, ScoredHit, truncate_text};
use crate::ai::skills::{Skill, SkillError};

pub struct ContextAssemblyInput {
    pub mention_resolved: Vec<(String, String)>,
    pub merged_hits: Vec<ScoredHit>,
    pub rewritten: bool,
    pub search_q: String,
    pub db: std::sync::Arc<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>,
    pub last_q: String,
    pub messages: Vec<crate::commands::ai::ChatMessage>,
}

pub struct ContextAssemblyOutput {
    pub system: String,
    pub user_msg: String,
    pub source_ids: Vec<String>,
    pub source_files: Vec<String>,
    pub evidence: Vec<EvidenceItem>,
}

pub struct ContextAssemblySkill;

impl Skill for ContextAssemblySkill {
    fn name(&self) -> &str { "ContextAssembly" }
}

impl ContextAssemblySkill {
    pub fn execute(&self, input: &ContextAssemblyInput) -> Result<ContextAssemblyOutput, SkillError> {
        let conn = input.db.get().map_err(|e| SkillError { message: format!("db: {e}") })?;
        let mut docs: Vec<String> = Vec::new();
        let mut evidence: Vec<EvidenceItem> = Vec::new();
        let mut source_ids: Vec<String> = Vec::new();
        let mut source_files: Vec<String> = Vec::new();
        let mut mention_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        // @mention 文件直用
        for (i, (fid, resolved_path)) in input.mention_resolved.iter().enumerate() {
            let n = i + 1;
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid)
                && let Some(md5) = &rec.md5
                    && let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5)
                        && !text.trim().is_empty() {
                            docs.push(format!("[{n}]（{resolved_path}）\n{}", truncate_text(&text, 50000)));
                            evidence.push(EvidenceItem {
                                file_id: fid.clone(),
                                path: resolved_path.clone(),
                                snippet: truncate_text(&text, 200),
                                bm25_score: None,
                                semantic_score: None,
                                rrf_score: None,
                                rewritten: input.rewritten,
                                rewritten_query: if input.rewritten { Some(input.search_q.clone()) } else { None },
                                from_history: false,
                            });
                            mention_index.insert(resolved_path.clone(), n);
                            source_ids.push(fid.clone());
                            source_files.push(resolved_path.clone());
                        }
        }

        // 检索命中
        for hit in &input.merged_hits {
            if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id)
                && let Some(md5) = &rec.md5
                    && let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5)
                        && !text.trim().is_empty() {
                            docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                            evidence.push(EvidenceItem {
                                file_id: hit.file_id.clone(),
                                path: rec.path.clone(),
                                snippet: truncate_text(&text, 200),
                                bm25_score: hit.bm25_score,
                                semantic_score: hit.semantic_score,
                                rrf_score: hit.rrf_score,
                                rewritten: input.rewritten,
                                rewritten_query: if input.rewritten { Some(input.search_q.clone()) } else { None },
                                from_history: hit.from_history,
                            });
                            source_ids.push(hit.file_id.clone());
                            source_files.push(rec.path.clone());
                        }
        }
        drop(conn);

        let context = truncate_text(&docs.join("\n\n---\n\n"), 50000);
        let system = format!("你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。如果材料不足以回答，请明确说明。\n引用材料时在文件名后标注 [N]（N 为材料编号），便于用户查阅原文。\n\n材料：\n{}", context);

        let last_n = input.messages.len().saturating_sub(1);
        let mut user_msg = if input.messages.len() > 1 {
            let mut history_str = String::from("对话历史：\n");
            for m in input.messages.iter().take(last_n) {
                history_str.push_str(&format!("[{}] {}\n",
                    if m.role == "user" { "用户" } else { "助手" },
                    truncate_text(&m.content, 500)));
            }
            format!("{}\n当前问题：{}", history_str, input.last_q)
        } else {
            input.last_q.clone()
        };

        for (path, idx) in &mention_index {
            user_msg = user_msg.replace(&format!("@{path}"), &format!("[{}]", idx));
        }

        Ok(ContextAssemblyOutput { system, user_msg, source_ids, source_files, evidence })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_context() {
        let db = std::sync::Arc::new(r2d2::Pool::new(r2d2_sqlite::SqliteConnectionManager::memory()).unwrap());
        let skill = ContextAssemblySkill;
        let result = skill.execute(&ContextAssemblyInput {
            mention_resolved: vec![],
            merged_hits: vec![],
            rewritten: false,
            search_q: String::new(),
            db,
            last_q: "test".into(),
            messages: vec![],
        }).unwrap();
        assert!(result.system.contains("材料："));
        assert_eq!(result.user_msg, "test");
    }
}
