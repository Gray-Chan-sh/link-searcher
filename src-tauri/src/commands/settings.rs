use std::collections::HashMap;

use tauri::State;

use crate::state::AppState;

/// Whitelisted keys that may be modified through [`update_settings`].
const ALLOWED_KEYS: &[&str] = &[
    "ocr_engine",
    "ocr_lang",
    "ocr_concurrent",
    "lo_batch_size",
    "max_results",
    "exclude_patterns",
    "scan_time",
    "auto_backup",
    "backup_interval",
    "auto_start",
];

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM app_settings")
        .map_err(|e| format!("query error: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("query error: {e}"))?;
    let mut map = HashMap::new();
    for row in rows {
        let (k, v) = row.map_err(|e| format!("read error: {e}"))?;
        map.insert(k, v);
    }
    Ok(map)
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    settings: HashMap<String, String>,
) -> Result<(), String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    for (key, value) in &settings {
        if !ALLOWED_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown setting key: {key}"));
        }
        conn.execute(
            "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )
        .map_err(|e| format!("failed to update setting '{key}': {e}"))?;
    }
    // Apply batch size change immediately (no restart needed).
    if let Some(val) = settings.get("lo_batch_size") {
        if let Ok(n) = val.parse::<usize>() {
            crate::extractor::office::LO_BATCH_SIZE
                .store(n.max(1), std::sync::atomic::Ordering::Relaxed);
        }
    }
    Ok(())
}

#[tauri::command]
pub fn get_version() -> serde_json::Value {
    serde_json::json!({
        "hash": env!("GIT_VERSION"),
        "time": env!("GIT_COMMIT_TIME"),
    })
}