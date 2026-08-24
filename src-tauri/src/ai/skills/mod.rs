//! RAG Skill 管道架构：将对话检索流程拆解为独立技能模块。
//!
//! 每个 Skill 拥有：
//! - 独立的输入/输出类型
//! - 独立的状态（如有）
//! - 独立的测试用例
//!
//! 管道编排顺序：
//! QueryRewrite → ScopeResolver → Retrieval → OldSourceManager → ContextAssembly

pub mod query_rewrite;
pub mod scope_resolver;
pub mod retrieval;
pub mod context_assembly;
pub mod pipeline;

use std::fmt;

/// Skill trait：所有技能模块实现此接口。
pub trait Skill {
    fn name(&self) -> &str;
}

/// Skill 执行错误。
#[derive(Debug)]
pub struct SkillError {
    pub message: String,
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SkillError {}

impl From<String> for SkillError {
    fn from(s: String) -> Self {
        SkillError { message: s }
    }
}
