//! Shared core bootstrap — opens the data directory, initialises the SQLite
//! schema and the Tantivy index, and wires up the scanner/indexer graph once,
//! so both the Tauri GUI (`lib.rs`) and the CLI (`cli.rs`) reuse identical
//! assembly instead of duplicating it.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::db;
use crate::indexer::IndexerService;
use crate::scanner::Scanner;
use crate::search::IndexManager;

/// Core services shared by the GUI and the CLI.
pub struct Bootstrap {
    /// SQLite connection pool for `data.db`.
    pub pool: Pool<SqliteConnectionManager>,
    /// Tantivy index manager (swappable via `rebuild_index`).
    pub index_manager: Arc<RwLock<IndexManager>>,
    /// Batch indexer.
    pub indexer: Arc<IndexerService>,
    /// Directory scanner + real-time event handler.
    pub scanner: Arc<Scanner>,
    /// Shared cancel flag; the GUI also owns it in `AppState`.
    pub cancel_scan: Arc<AtomicBool>,
}

/// Open (or create) the data directory, initialise the SQLite schema and the
/// Tantivy index, and assemble the scanner/indexer graph.
///
/// Errors are returned rather than panicked on — CLI callers print them and
/// exit non-zero.
pub fn bootstrap_core(data_dir: &Path) -> Result<Bootstrap> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data directory {:?}", data_dir))?;
    let index_dir = data_dir.join(crate::config::INDEX_DIR_NAME);
    std::fs::create_dir_all(&index_dir)
        .with_context(|| format!("failed to create index directory {:?}", index_dir))?;

    let db_path = data_dir.join("data.db");
    let pool = db::get_pool(db_path.to_string_lossy().as_ref())?;
    let init_conn = pool.get().context("failed to get DB connection")?;
    db::init_db(&init_conn)?;
    drop(init_conn);

    let index_manager = Arc::new(RwLock::new(IndexManager::open_or_create(&index_dir)?));

    let cancel_scan = Arc::new(AtomicBool::new(false));
    let indexer = Arc::new(IndexerService::with_cancel(
        pool.clone(),
        index_manager.clone(),
        cancel_scan.clone(),
    ));
    let scanner = Arc::new(Scanner::with_cancel(
        pool.clone(),
        indexer.clone(),
        cancel_scan.clone(),
    ));

    Ok(Bootstrap { pool, index_manager, indexer, scanner, cancel_scan })
}