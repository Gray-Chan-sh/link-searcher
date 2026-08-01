//! Shared helper functions for directory scanning.
//!
//! Path conversion utilities for storing/retrieving files using relative paths
//! within a monitored directory root.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow};
use crate::db::tracker::FileRecord;

const EXCLUDED_NAMES: &[&str] = &[".DS_Store", "Thumbs.db", ".git", ".svn", "__pycache__"];

const EXCLUDED_PREFIXES: &[char] = &['#', '$', '.', '~'];

const EXCLUDED_SUFFIXES: &[&str] = &[".tmp", ".temp", ".bak", ".swp", ".swo", "~"];

pub fn is_excluded(path: &Path, exclude_patterns: &[glob::Pattern]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if EXCLUDED_NAMES.contains(&name) {
        return true;
    }
    if let Some(c) = name.chars().next() {
        if EXCLUDED_PREFIXES.contains(&c) {
            return true;
        }
    }
    if EXCLUDED_SUFFIXES.iter().any(|s| name.ends_with(s)) {
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
    let now = chrono::Utc::now().timestamp_micros().to_string();
    conn.execute(
        "INSERT OR REPLACE INTO app_settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![format!("last_scan_{dir_id}"), now],
    )?;
    Ok(())
}


/// Convert an absolute path to a relative path within the given directory root.
/// Returns the relative path string, or an error if the file is not under dir_root.
pub fn to_relative(dir_root: &str, file_path: &Path) -> Result<String> {
    let root = PathBuf::from(dir_root);
    let rel = file_path
        .strip_prefix(&root)
        .map_err(|_| anyhow!("file {} is not under dir root {}", file_path.display(), dir_root))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Convert a stored relative path back to an absolute path by joining with dir_root.
pub fn to_absolute(dir_root: &str, rel_path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(dir_root).join(rel_path)
}

pub fn needs_reindex(existing: &Option<FileRecord>, mtime: i64) -> bool {
    match existing {
        Some(r) => r.mtime != mtime || r.indexed == 0 || r.indexed == 2,
        None => true,
    }
}

/// RAII temporary directory. Unique per instance (pid + uuid), removed
/// including contents when dropped. Solves concurrent runs sharing the same
/// temp dir and leaking leftovers on early returns.
pub struct TempDir {
    path: std::path::PathBuf,
}

impl TempDir {
    pub fn new(prefix: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_removed_on_drop() {
        let path = {
            let td = TempDir::new("test_tmp").unwrap();
            assert!(td.path().exists());
            td.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn temp_dir_names_are_unique() {
        let a = TempDir::new("test_tmp").unwrap().path().to_path_buf();
        let b = TempDir::new("test_tmp").unwrap().path().to_path_buf();
        assert_ne!(a, b);
    }

    #[test]
    fn to_relative_respects_component_boundary() {
        let root = "/tmp/foo";
        assert_eq!(to_relative(root, Path::new("/tmp/foo/bar.txt")).unwrap(), "bar.txt");
        assert_eq!(to_relative(root, Path::new("/tmp/foo/sub/deep.txt")).unwrap(), "sub/deep.txt");
        // sibling with shared prefix must NOT match
        assert!(to_relative(root, Path::new("/tmp/foobar/x.txt")).is_err());
    }
}