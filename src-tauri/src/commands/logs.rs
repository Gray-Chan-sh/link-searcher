use std::fs;
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub fn get_logs(state: State<'_, AppState>, lines: Option<usize>) -> Result<Vec<String>, String> {
    let log_path = state.data_dir.join("app.log");
    let content = fs::read_to_string(&log_path).map_err(|e| format!("{e}"))?;
    let all: Vec<&str> = content.lines().collect();
    let n = lines.unwrap_or(100).min(all.len());
    Ok(all[all.len() - n..].iter().map(|s| s.to_string()).collect())
}

#[tauri::command]
pub fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    let log_path = state.data_dir.join("app.log");
    fs::write(&log_path, "").map_err(|e| format!("{e}"))
}