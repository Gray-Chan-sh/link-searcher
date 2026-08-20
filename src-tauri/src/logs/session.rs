//! Session-scoped log files — one file per scan session under `data_dir/logs/`.
//!
//! Contract (shared with T7): `SessionLog::open` creates
//! `{name}-{YYYYMMDD-HHmmss}.log` and returns the [`File`]; the owning thread
//! calls [`SessionLog::write`] to append lines and [`SessionLog::close`] to
//! flush + sync before handing the handle back. Each scan session is owned by
//! exactly one thread, so no locking is needed.

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