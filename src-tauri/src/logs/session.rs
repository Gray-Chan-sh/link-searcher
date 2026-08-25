//! Session-scoped log files — one file per scan session under `data_dir/logs/`.
//!
//! Contract: [`SessionLog::open`] creates `{name}-{YYYYMMDD-HHmmss}.log` and
//! returns the [`File`]; the owning thread calls [`SessionLog::write`] to
//! append lines and [`SessionLog::close`] to flush + sync.
//!
//! Prefer [`SessionLogGuard`] — an RAII wrapper that auto-closes on drop and
//! exposes a [`write_line`] method for ergonomic one-liner calls.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Stateless handle to the session-log contract. See module docs.
pub struct SessionLog;

impl SessionLog {
    /// Create (or append to) `dir/{name}-{YYYYMMDD-HHmmss}.log`.
    ///
    /// `dir` is created if missing. Collisions within the same second fail
    /// with `AlreadyExists` — the caller treats the log as optional.
    pub fn open(dir: &Path, name: &str) -> io::Result<File> {
        std::fs::create_dir_all(dir)?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let path = dir.join(format!("{name}-{stamp}.log"));
        OpenOptions::new().create_new(true).append(true).open(path)
    }

    /// Append one line and flush so a crash loses at most the last line.
    pub fn write(file: &mut File, line: &str) -> io::Result<()> {
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    }

    /// Flush + sync to disk; the file closes when the [`File`] drops.
    pub fn close(file: File) {
        let mut f = file;
        let _ = f.flush();
        let _ = f.sync_all();
    }
}

/// RAII guard for a scan session log file.
///
/// Opens the file on construction (log proceeds without it on failure) and
/// flushes + syncs on drop. Exposes [`write_line`] for ergonomic appending.
pub struct SessionLogGuard {
    file: Option<File>,
}

impl SessionLogGuard {
    /// Open a new session log file at `{dir}/{name}-{timestamp}.log`.
    /// If the file cannot be created, the guard is inert (writes are no-ops).
    pub fn open(dir: &Path, name: &str) -> Self {
        let file = SessionLog::open(dir, name)
            .map_err(|e| log::warn!("[SCAN] 无法创建会话日志: {e}"))
            .ok();
        Self { file }
    }

    /// Append a line to the session log (no-op if file could not be opened).
    pub fn write_line(&mut self, line: &str) {
        if let Some(ref mut f) = self.file {
            let _ = SessionLog::write(f, line);
        }
    }
}

impl Drop for SessionLogGuard {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            SessionLog::close(f);
        }
    }
}