use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::OnceLock;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::mpsc;
use serde::Serialize;

use crate::scanner::watcher::{FileWatcher, WatcherCommand};

use crate::indexer::IndexerService;
use crate::scanner::Scanner;
use crate::search::IndexManager;

/// A finished long-running task, surfaced to the UI as a brief.
#[derive(Debug, Clone, Serialize)]
pub struct TaskBrief {
    pub task: String,
    pub summary: String,
    pub completed_at: i64,
}

/// Global registry of long-running tasks. The frontend reads it via
/// `get_index_status` so buttons stay disabled across page switches
/// (this is the single source of truth for task liveness).
fn task_registry() -> &'static Mutex<Vec<String>> {
    static REG: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(Vec::new()))
}

/// Ring buffer of recently finished task briefs (newest first, cap 50).
fn task_briefs() -> &'static Mutex<std::collections::VecDeque<TaskBrief>> {
    static B: OnceLock<Mutex<std::collections::VecDeque<TaskBrief>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(std::collections::VecDeque::with_capacity(50)))
}

pub fn track_task(task: &str) {
    if let Ok(mut t) = task_registry().lock() {
        if !t.iter().any(|x| x == task) {
            t.push(task.to_string());
        }
    }
}

/// RAII guard: registers the task on creation, unregisters on drop — safe
/// across early returns / errors in the task body.
pub struct TaskGuard {
    task: String,
}

impl TaskGuard {
    pub fn new(task: &str) -> Self {
        track_task(task);
        Self { task: task.to_string() }
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        untrack_task(&self.task);
    }
}

pub fn untrack_task(task: &str) {
    if let Ok(mut t) = task_registry().lock() {
        t.retain(|x| x != task);
    }
}

pub fn running_task_ids() -> Vec<String> {
    task_registry().lock().map(|t| t.clone()).unwrap_or_default()
}

/// Push a finished-task brief (newest first, capped). Also writes a
/// `[TASK]`-prefixed log line so the LogViewer can locate it by grep.
pub fn push_task_brief(task: &str, summary: String) {
    log::info!("[TASK] {task}: {summary}");
    let brief = TaskBrief {
        task: task.to_string(),
        summary,
        completed_at: chrono::Utc::now().timestamp(),
    };
    if let Ok(mut b) = task_briefs().lock() {
        b.push_front(brief);
        while b.len() > 50 {
            b.pop_back();
        }
    }
}

pub fn task_brief_snapshot() -> Vec<TaskBrief> {
    task_briefs().lock().map(|b| b.iter().cloned().collect()).unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanDelta {
    pub added: u64,
    pub deleted: u64,
    pub modified: u64,
    pub errors: u64,
    pub duration_ms: u64,
}

impl Default for ScanDelta {
    fn default() -> Self {
        Self {
            added: 0,
            deleted: 0,
            modified: 0,
            errors: 0,
            duration_ms: 0,
        }
    }
}

pub struct AppState {
    pub db: Pool<SqliteConnectionManager>,
    pub index_manager: Arc<RwLock<IndexManager>>,
    pub indexer: Arc<IndexerService>,
    pub scanner: Arc<Scanner>,
    pub is_scanning: Arc<AtomicBool>,
    pub is_rebuilding: Arc<AtomicBool>,
    pub cancel_scan: Arc<AtomicBool>,
    pub is_restoring: Arc<AtomicBool>,
    pub scan_delta: Arc<Mutex<ScanDelta>>,
    pub data_dir: PathBuf,
    pub index_dir: PathBuf,
    pub db_path: PathBuf,
    pub watcher_tx: mpsc::Sender<WatcherCommand>,
    pub watcher: Option<FileWatcher>,
}

impl AppState {
    pub fn new(
        db: Pool<SqliteConnectionManager>,
        index_manager: Arc<RwLock<IndexManager>>,
        indexer: Arc<IndexerService>,
        scanner: Arc<Scanner>,
        is_scanning: Arc<AtomicBool>,
        is_rebuilding: Arc<AtomicBool>,
        cancel_scan: Arc<AtomicBool>,
        is_restoring: Arc<AtomicBool>,
        scan_delta: Arc<Mutex<ScanDelta>>,
        data_dir: PathBuf,
        index_dir: PathBuf,
        db_path: PathBuf,
        watcher_tx: mpsc::Sender<WatcherCommand>,
        watcher: Option<FileWatcher>,
    ) -> Self {
        Self {
            db,
            index_manager,
            indexer,
            scanner,
            is_scanning,
            is_rebuilding,
            cancel_scan,
            is_restoring,
            scan_delta,
            data_dir,
            index_dir,
            db_path,
            watcher_tx,
            watcher,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        if let Ok(mut t) = task_registry().lock() {
            t.clear();
        }
    }

    #[test]
    fn task_registry_tracks_and_untracks() {
        reset();
        track_task("verify");
        track_task("verify"); // idempotent
        track_task("backfill");
        let ids = running_task_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"verify".to_string()));
        assert!(ids.contains(&"backfill".to_string()));

        untrack_task("verify");
        assert_eq!(running_task_ids(), vec!["backfill".to_string()]);
    }

    #[test]
    fn task_guard_drops_on_scope_exit() {
        reset();
        {
            let _g = TaskGuard::new("reextract");
            assert!(running_task_ids().contains(&"reextract".to_string()));
        }
        assert!(!running_task_ids().contains(&"reextract".to_string()));
    }

    #[test]
    fn briefs_ring_buffer_newest_first_capped() {
        for i in 0..60 {
            push_task_brief("t", format!("summary {i}"));
        }
        let snap = task_brief_snapshot();
        assert_eq!(snap.len(), 50, "ring buffer must cap at 50");
        assert!(snap[0].summary == "summary 59", "newest first");
        assert!(snap[49].summary == "summary 10", "oldest retained");
    }
}
