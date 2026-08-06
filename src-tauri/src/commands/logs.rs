use std::fs;
use std::io::Write;
use tauri::State;

use crate::state::AppState;

/// Extract the ISO-8601 timestamp from a log line.
/// Log format: `[2026-08-05T23:25:04Z ...]`
fn log_timestamp(line: &str) -> Option<&str> {
    let start = line.find('[')? + 1;
    let end = line[start..].find(']')?;
    Some(&line[start..start + end])
}

fn marker_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("log_marker")
}

/// Format current UTC time as ISO 8601 (same format as log lines, minus the Z).
fn now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let time = secs % 86400;
    // Approximate year/month/day from days since epoch
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if remaining < year_days { break; }
        remaining -= year_days;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let mdays = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && remaining >= mdays[m] as i64 {
        remaining -= mdays[m] as i64;
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m + 1, remaining + 1,
        time / 3600, (time % 3600) / 60, time % 60
    )
}

#[tauri::command]
pub fn get_logs(state: State<'_, AppState>, lines: Option<usize>) -> Result<Vec<String>, String> {
    let log_path = state.data_dir.join("app.log");
    let content = fs::read_to_string(&log_path).map_err(|e| format!("{e}"))?;
    let all: Vec<&str> = content.lines().collect();

    let marker = fs::read_to_string(marker_path(&state.data_dir))
        .ok()
        .map(|s| s.trim().to_string());

    if let Some(ref marker_ts) = marker {
        let filtered: Vec<&str> = all
            .iter()
            .filter(|line| {
                log_timestamp(line).map_or(false, |ts| ts >= marker_ts.as_str())
            })
            .copied()
            .collect();
        let n = lines.unwrap_or(500).min(filtered.len());
        let start = filtered.len().saturating_sub(n);
        return Ok(filtered[start..].iter().map(|s| s.to_string()).collect());
    }

    let n = lines.unwrap_or(100).min(all.len());
    Ok(all[all.len() - n..].iter().map(|s| s.to_string()).collect())
}

#[tauri::command]
pub fn clear_logs(state: State<'_, AppState>) -> Result<(), String> {
    let ts = now_iso();
    fs::File::create(marker_path(&state.data_dir))
        .map_err(|e| format!("{e}"))?
        .write_all(ts.as_bytes())
        .map_err(|e| format!("{e}"))
}
