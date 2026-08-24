//! RAG Pipeline：编排所有 Skill 的执行顺序。
//!
//! 执行顺序：
//! QueryRewrite → ScopeResolver → Retrieval → OldSourceManager → ContextAssembly

use std::sync::Arc;

use crate::ai::skills::{Skill, SkillError};
use crate::ai::skills::query_rewrite::{QueryRewriteSkill, QueryRewriteInput};
use crate::ai::skills::scope_resolver::{ScopeResolverSkill, ScopeResolverInput};
use crate::ai::skills::retrieval::{RetrievalSkill, RetrievalInput, OldSourceManager};
use crate::ai::skills::context_assembly::{ContextAssemblySkill, ContextAssemblyInput, ContextAssemblyOutput};

pub struct RAGPipeline {
    pub query_rewrite: QueryRewriteSkill,
    pub scope_resolver: ScopeResolverSkill,
    pub retrieval: RetrievalSkill,
    pub context_assembly: ContextAssemblySkill,
}

pub struct RAGContext {
    pub last_q: String,
    pub messages: Vec<crate::commands::ai::ChatMessage>,
    pub scope: crate::commands::ai::TurnScope,
    pub session_retrieval_scope: Vec<String>,
    pub strict_docs: bool,
    pub source_ids: Vec<String>,
    pub db: Arc<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>,
    pub state: Option<&'static tauri::State<'static, crate::state::AppState>>,
}

impl RAGPipeline {
    pub fn new() -> Self {
        RAGPipeline {
            query_rewrite: QueryRewriteSkill,
            scope_resolver: ScopeResolverSkill,
            retrieval: RetrievalSkill,
            context_assembly: ContextAssemblySkill,
        }
    }

    /// 执行完整的 RAG 管道。
    pub async fn execute(&self, ctx: &RAGContext) -> Result<ContextAssemblyOutput, SkillError> {
        // 1. QueryRewrite
        let qr = self.query_rewrite.execute(&QueryRewriteInput {
            last_q: ctx.last_q.clone(),
            messages: ctx.messages.clone(),
        }).await?;

        // 2. ScopeResolver
        let sr = self.scope_resolver.execute(&ScopeResolverInput {
            scope: ctx.scope.clone(),
            session_retrieval_scope: ctx.session_retrieval_scope.clone(),
            db: Box::leak(Box::new(ctx.db.clone())),
        })?;

        // 3. Retrieval
        let semantic = crate::ai::embedding_enabled();
        let ret = if let Some(state) = ctx.state {
            self.retrieval.execute(&RetrievalInput {
                search_q: qr.search_q.clone(),
                dir_ids: sr.dir_ids.clone(),
                path_prefixes: sr.path_prefixes.clone(),
                ext_filter: sr.ext_filter.clone(),
                date_from: sr.date_from,
                date_to: sr.date_to,
                file_ids: sr.mention_file_ids.clone(),
                semantic,
                state,
            })?
        } else {
            return Err(SkillError { message: "AppState not available".into() });
        };

        // 4. OldSourceManager
        let merged = OldSourceManager::merge(
            ret.hits,
            &ctx.source_ids,
            &ctx.messages,
            &sr.mention_resolved,
            ctx.strict_docs,
            15,
            &ctx.db,
        )?;

        // 5. ContextAssembly
        let output = self.context_assembly.execute(&ContextAssemblyInput {
            mention_resolved: sr.mention_resolved,
            merged_hits: merged,
            rewritten: qr.rewritten,
            search_q: qr.search_q,
            db: ctx.db.clone(),
            last_q: ctx.last_q.clone(),
            messages: ctx.messages.clone(),
        })?;

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_creation() {
        let pipeline = RAGPipeline::new();
        assert_eq!(pipeline.query_rewrite.name(), "QueryRewrite");
        assert_eq!(pipeline.scope_resolver.name(), "ScopeResolver");
        assert_eq!(pipeline.retrieval.name(), "Retrieval");
        assert_eq!(pipeline.context_assembly.name(), "ContextAssembly");
    }
}
