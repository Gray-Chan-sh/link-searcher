//! Shared helper functions for directory scanning.

use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};

const EXCLUDED_NAMES: &[&str] = &[".DS_Store", "Thumbs.db", ".git", ".svn", "__pycache__"];

/// Check whether a path should be excluded from scanning based on file name
/// or user-supplied glob patterns.
pub fn is_excluded(path: &Path, exclude_patterns: &[glob::Pattern]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if EXCLUDED_NAMES.contains(&name) {
        return true;
    }
    for pattern in exclude_patterns {
        if pattern.matches_path(path) {
            return true;
        }
    }
    false
}

/// Check whether a file's extension is in the allowed set, or whether the
/// extractor supports it when no explicit extension list is configured.
pub fn extension_allowed(path: &Path, include_exts: &Option<Vec<String>>) -> bool {
    match include_exts {
        Some(exts) => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            !ext.is_empty() && exts.contains(&ext)
        }
        None => crate::extractor::is_supported(path),
    }
}

/// Extract modification time as microseconds since UNIX_EPOCH.
pub fn mtime_micros(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_micros() as i64)
}

/// Parse a comma-separated list of glob patterns from a config value.
pub fn parse_exclude_patterns(raw: &Option<String>) -> Vec<glob::Pattern> {
    raw.as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| glob::Pattern::new(s).ok())
        .collect()
}

/// Parse a comma-separated list of file extensions from a config value.
pub fn parse_include_exts(raw: &Option<String>) -> Option<Vec<String>> {
    let s = raw.as_deref()?;
    let exts: Vec<String> = s.split(',').map(|e| e.trim().to_lowercase()).collect();
    if exts.is_empty() {
        None
    } else {
        Some(exts)
    }
}

/// Retrieve the last-scan timestamp for a directory from `app_settings`.
pub fn get_last_scan_time(conn: &rusqlite::Connection, dir_id: &str) -> Result<i64> {
    let key = format!("last_scan_{dir_id}");
    let mut stmt = conn
        .prepare("SELECT value FROM app_settings WHERE key = ?1")
        .context("prepare last_scan")?;
    let mut rows = stmt
        .query_map(rusqlite::params![key], |row| row.get::<_, String>(0))
        .context("query last_scan")?;
    match rows.next().transpose()? {
        Some(v) => v.parse::<i64>().or(Ok(0)),
        None => Ok(0),
    }
}

/// Record the current timestamp as the last-scan time for a directory.
pub fn record_last_scan(conn: &rusqlite::Connection, dir_id: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp().to_string();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![format!("last_scan_{dir_id}"), now],
    )?;
    Ok(())
}
