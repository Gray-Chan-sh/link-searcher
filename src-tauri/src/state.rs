use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::RwLock;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::mpsc;

use crate::scanner::watcher::{FileWatcher, WatcherCommand};

use crate::indexer::IndexerService;
use crate::scanner::Scanner;
use crate::search::IndexManager;

pub struct AppState {
    pub db: Pool<SqliteConnectionManager>,
    pub index_manager: Arc<RwLock<IndexManager>>,
    pub indexer: Arc<IndexerService>,
    pub scanner: Arc<Scanner>,
    pub is_scanning: Arc<AtomicBool>,
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
            data_dir,
            index_dir,
            db_path,
            watcher_tx,
            watcher,
        }
    }
}