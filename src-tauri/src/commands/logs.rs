use std::fs::OpenOptions;
use tauri::State;

use crate::state::AppState;

#[tauri::command]
pub fn get_logs(
    state: State<'_, AppState>,
    lines: Option<usize>,
    file_id: Option<String>,
) -> Result<Vec<String>, String> {
    let log_path = state.data_dir.join("app.log");
    let content = std::fs::read_to_string(&log_path).map_err(|e| format!("{e}"))?;
    let all: Vec<&str> = content.lines().collect();

    // When a file_id is given, return the most recent complete indexing
    // session for that file: from the last "开始:" line mentioning the id
    // (both the start and end of the indexing are tagged [INDEX] [id]).
    if let Some(fid) = file_id {
        let id_tag = format!("[{fid}]");
        // Find the position of the LAST "开始:" line for this file.
        let mut start_idx = None;
        for (i, line) in all.iter().enumerate() {
            if line.contains(&id_tag) && line.contains("开始:") {
                start_idx = Some(i);
            }
        }
        return match start_idx {
            Some(s) => Ok(all[s..].iter().map(|s| s.to_string()).collect()),
            None => Ok(Vec::new()),
        };
    }

    let n = lines.unwrap_or(500).min(all.len());
    let start = all.len().saturating_sub(n);
    Ok(all[start..].iter().map(|s| s.to_string()).collect())
}

#[tauri::command]
pub fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    let log_path = state.data_dir.join("app.log");
    // Truncate the log file so subsequent reads see an empty log.
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| format!("{e}"))?;
    Ok(())
}