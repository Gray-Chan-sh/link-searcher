use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::sync::RwLock;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::mpsc;
use serde::Serialize;

use crate::scanner::watcher::{FileWatcher, WatcherCommand};

use crate::indexer::IndexerService;
use crate::scanner::Scanner;
use crate::search::IndexManager;

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
    pub cancel_scan: Arc<AtomicBool>,
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
        cancel_scan: Arc<AtomicBool>,
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
            cancel_scan,
            scan_delta,
            data_dir,
            index_dir,
            db_path,
            watcher_tx,
            watcher,
        }
    }
}
