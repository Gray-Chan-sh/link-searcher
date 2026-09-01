//! Retrieval Skill：执行 BM25 + 语义混合检索，以及旧来源保留与去重。

use std::collections::HashSet;

use crate::commands::ai::{bm25_relevant_hits, ScoredHit};
use crate::ai::skills::{Skill, SkillError};

pub struct RetrievalInput {
    pub search_q: String,
    pub dir_ids: Option<Vec<String>>,
    pub path_prefixes: Option<Vec<String>>,
    pub ext_filter: Option<Vec<String>>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    pub file_ids: Option<Vec<String>>,
    pub semantic: bool,
    pub state: &'static tauri::State<'static, crate::state::AppState>,
}

pub struct RetrievalOutput {
    pub hits: Vec<ScoredHit>,
    pub hit_count: usize,
}

pub struct RetrievalSkill;

impl Skill for RetrievalSkill {
    fn name(&self) -> &str { "Retrieval" }
}

impl RetrievalSkill {
    pub fn execute(&self, input: &RetrievalInput) -> Result<RetrievalOutput, SkillError> {
        let hits = bm25_relevant_hits(
            input.state,
            &input.search_q,
            10,
            input.semantic,
            input.dir_ids.clone(),
            input.ext_filter.clone(),
            input.date_from,
            input.date_to,
            input.path_prefixes.clone(),
            input.file_ids.clone(),
        ).map_err(|e| SkillError { message: e })?;
        let count = hits.len();
        Ok(RetrievalOutput { hits, hit_count: count })
    }
}

/// OldSourceManager：处理旧来源保留逻辑。
pub struct OldSourceManager;

impl OldSourceManager {
    /// 合并本轮检索命中与旧来源：
    /// - 新检索优先（追问更新依据）
    /// - 旧来源仅保留对话中提到的（≤3）
    /// - 去重（按 file_id）
    /// - strict_docs=true 时跳过旧来源
    pub fn merge(
        new_hits: Vec<ScoredHit>,
        source_ids: &[String],
        messages: &[crate::commands::ai::ChatMessage],
        mention_resolved: &[(String, String)],
        strict_docs: bool,
        max_sources: usize,
        db: &std::sync::Arc<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>,
    ) -> Result<Vec<ScoredHit>, SkillError> {
        let conn = db.get().map_err(|e| SkillError { message: format!("db: {e}") })?;
        let mut merged: Vec<ScoredHit> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for (fid, _) in mention_resolved {
            seen.insert(fid.clone());
        }

        for hit in new_hits {
            if seen.insert(hit.file_id.clone()) {
                merged.push(hit);
            }
        }

        if !strict_docs {
            let message_text: String = messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join(" ");
            let mentioned = |rec_path: &str| -> bool {
                let stem = std::path::Path::new(rec_path).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let name = std::path::Path::new(rec_path).file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
                (!stem.is_empty() && message_text.contains(stem.as_str())) || (!name.is_empty() && message_text.contains(name.as_str()))
            };

            const MAX_OLD: usize = 3;
            let mut old_count = 0usize;
            for fid in source_ids.iter().rev() {
                if merged.len() >= max_sources || old_count >= MAX_OLD { break; }
                if seen.contains(fid) { continue; }
                let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) else { continue };
                if rec.status != "active" || rec.md5.is_none() { continue; }
                if !mentioned(&rec.path) { continue; }
                seen.insert(fid.clone());
                merged.push(ScoredHit {
                    file_id: fid.clone(),
                    path: rec.path.clone(),
                    bm25_score: None,
                    semantic_score: None,
                    rrf_score: None,
                    from_history: true,
                    from_chunk: false,
                    hit_chunks: Vec::new(),
                });
                old_count += 1;
            }
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, path: &str) -> ScoredHit {
        ScoredHit {
            file_id: id.into(), path: path.into(),
            bm25_score: Some(1.0), semantic_score: None, rrf_score: None,
            from_history: false, from_chunk: false, hit_chunks: Vec::new(),
        }
    }

    #[test]
    fn empty_input_returns_empty() {
        let db = std::sync::Arc::new(r2d2::Pool::new(r2d2_sqlite::SqliteConnectionManager::memory()).unwrap());
        let result = OldSourceManager::merge(vec![], &[], &[], &[], false, 15, &db).unwrap_or_default();
        assert!(result.is_empty());
    }
}
