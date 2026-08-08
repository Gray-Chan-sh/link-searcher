//! AI gateway commands: per-file summaries and cross-document Q&A (RAG).

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Serialize)]
pub struct SummaryResult {
    pub file_id: String,
    pub summary: String,
    pub cached: bool,
}

/// Generate (or fetch cached) an LLM summary for a file's extracted text.
#[tauri::command]
pub fn summarize_file(
    state: State<'_, AppState>,
    file_id: String,
) -> Result<SummaryResult, String> {
    if !crate::ai::ai_enabled() {
        return Err("AI 服务未配置，请在设置页填写 API Base URL".into());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    // Cached?
    if let Some(saved) = crate::db::tracker::get_summary(&conn, &file_id)
        .map_err(|e| format!("{e}"))?
    {
        return Ok(SummaryResult { file_id, summary: saved, cached: true });
    }

    // Resolve file record → content.
    let rec = crate::db::tracker::get_file_by_id(&conn, &file_id)
        .map_err(|e| format!("{e}"))?
        .ok_or_else(|| "file not found".to_string())?;
    let md5 = rec.md5.clone().ok_or_else(|| "no content hash".to_string())?;
    let text = crate::db::tracker::get_content(&conn, &md5)
        .map_err(|e| format!("{e}"))?
        .unwrap_or_default();
    drop(conn);

    if text.trim().is_empty() {
        return Err("该文件没有可摘要的文本内容".into());
    }
    let text = truncate_text(text.as_str(), 8000);

    let summary = crate::ai::chat(
        "你是文档摘要助手。用简洁的中文总结以下文档内容，突出主题、关键信息与结论，不超过150字。",
        &text,
    )
    .ok_or_else(|| "AI 请求失败（检查 API 配置或网络）".to_string())?;

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let _ = crate::db::tracker::upsert_summary(&conn, &file_id, &summary);
    Ok(SummaryResult { file_id, summary, cached: false })
}

/// Ask a question over one or more documents' extracted text (RAG).
#[tauri::command]
pub fn ask_documents(
    state: State<'_, AppState>,
    file_ids: Vec<String>,
    question: String,
) -> Result<String, String> {
    if !crate::ai::ai_enabled() {
        return Err("AI 服务未配置，请在设置页配置 API Base URL".into());
    }
    if question.trim().is_empty() {
        return Err("问题不能为空".into());
    }
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;

    let mut docs: Vec<String> = Vec::new();
    for fid in &file_ids {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = crate::db::tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!("【{}】\n{}", rec.path, truncate_text(&text, 2000)));
                    }
                }
            }
        }
    }
    drop(conn);

    if docs.is_empty() {
        return Err("所选文件没有可用的文本内容".into());
    }

    let context = docs.join("\n\n---\n\n");
    let user_msg = format!(
        "以下是从用户本地文档中提取的内容，请基于这些内容回答问题。若内容不足以回答，请明确说明。\n\n{context}\n\n问题：{question}",
        context = truncate_text(&context, 24000),
    );

    crate::ai::chat("你是严谨的文档分析助手。仅基于提供的材料回答，不臆造事实，回答简洁有条理。", &user_msg)
        .ok_or_else(|| "AI 请求失败（检查网关配置或网络）".to_string())
}

fn truncate_text(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[..max_chars].iter().collect()
    }
}