//! Local reranker model status.
//!
//! Installation/dimension management lives in the dependency center
//! (`deps::catalog` + the `DepsTab` dropdown); this only exposes which built-in
//! rerankers are present so the AI settings dropdown can list them.

use serde::Serialize;
use tauri::State;

use crate::ai::local_rerank::{rerank_model_ready, RERANK_MODELS};
use crate::state::AppState;

#[derive(Serialize, Clone)]
pub struct RerankStatus {
    pub installed: bool,
    pub model_name: String,
    /// Directory name, i.e. the suffix of `local:<model_id>`.
    pub model_id: String,
}

#[tauri::command]
pub fn check_rerank_installed(state: State<'_, AppState>) -> Result<Vec<RerankStatus>, String> {
    Ok(RERANK_MODELS
        .iter()
        .map(|(dir, display)| RerankStatus {
            installed: rerank_model_ready(&state.data_dir, dir),
            model_name: display.to_string(),
            model_id: dir.to_string(),
        })
        .collect())
}
