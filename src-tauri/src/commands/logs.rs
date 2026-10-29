use std::fs::OpenOptions;
use tauri::State;

use crate::state::AppState;

/// 前端上报「这次 IPC 慢于阈值」（`client.invoke` 里统一计时，默认 >2s 才报）。
///
/// 存在理由：2026-10-29 那次界面卡死排查**完全没有线索** —— `app.log` 里没有任何
/// 命令级耗时，只能靠外部工具猜是哪个同步命令把 UI 线程堵住了。现在任何慢调用
/// 都会在这里落一行 `[SLOW-IPC]`，事后 grep 即可定位（同步命令慢 = UI 线程被占）。
/// 前端在 Web 模式下不发（没有 IPC），故用不到 `async`。
#[tauri::command]
pub fn log_slow_invoke(cmd: String, ms: u64) {
    log::warn!("[SLOW-IPC] {cmd} took {ms}ms");
}

#[tauri::command]
pub fn get_logs(
    state: State<'_, AppState>,
    lines: Option<usize>,
    file_id: Option<String>,
    session_id: Option<String>,
) -> Result<Vec<String>, String> {
    if let Some(sid) = session_id {
        if !sid.starts_with("scan-") || sid.contains("..") || sid.contains('/') || sid.contains('\\') {
            return Err("invalid session_id".to_string());
        }
        let filename = if sid.ends_with(".log") {
            sid
        } else {
            format!("{sid}.log")
        };
        let log_path = state.data_dir.join("logs").join(filename);
        if !log_path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&log_path).map_err(|e| format!("{e}"))?;
        let all: Vec<&str> = content.lines().collect();
        let n = lines.unwrap_or(500).min(all.len());
        let start = all.len().saturating_sub(n);
        return Ok(all[start..].iter().map(|s| s.to_string()).collect());
    }

    let log_path = state.data_dir.join("app.log");
    let content = std::fs::read_to_string(&log_path).map_err(|e| format!("{e}"))?;
    let all: Vec<&str> = content.lines().collect();

    // Return the most recent indexing session for this file: parallel batches
    // interleave in the log, so trim to the last 开始: and keep only [id] lines.
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
            Some(s) => Ok(all[s..]
                .iter()
                .filter(|line| line.contains(&id_tag))
                .map(|s| s.to_string())
                .collect()),
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

#[tauri::command]
pub fn list_session_logs(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let logs_dir = state.data_dir.join("logs");
    let mut entries: Vec<_> = match std::fs::read_dir(&logs_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().map(|t| t.is_file()).unwrap_or(false)
                    && e.file_name().to_string_lossy().starts_with("scan-")
                    && e.file_name().to_string_lossy().ends_with(".log")
            })
            .collect(),
        Err(_) => return Ok(Vec::new()),
    };
    entries.sort_by(|a, b| {
        let ma = a.metadata().and_then(|m| m.modified()).ok();
        let mb = b.metadata().and_then(|m| m.modified()).ok();
        mb.cmp(&ma)
    });
    entries.truncate(50);
    Ok(entries
        .into_iter()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect())
}