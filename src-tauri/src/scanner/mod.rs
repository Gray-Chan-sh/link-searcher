//! Directory scanner — full and incremental scans with progress reporting.
//!
//! Also provides [`process_event`] / [`process_event_batch`] for the
//! real-time file watcher to consume [`FileChangeEvent`]s.

pub mod watcher;
pub mod helpers;

use self::helpers::*;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use md5::Digest;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::db::dir_config;
use crate::db::tracker;
use crate::indexer::IndexerService;
use crate::search::indexer::Indexer;

pub use watcher::{ChangeKind, FileChangeEvent, FileWatcher, WatcherCommand};

/// Progress snapshot emitted during a scan.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub total: u64,
    pub processed: u64,
    pub errors: u64,
    pub current_file: String,
}

/// Summary returned after a scan completes.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub total_files: u64,
    pub indexed: u64,
    pub errors: u64,
    pub duration_ms: u64,
}

/// Walks a directory tree, discovers files, and indexes them via
/// [`IndexerService`].
pub struct Scanner {
    db: Pool<SqliteConnectionManager>,
    indexer: Arc<IndexerService>,
}

impl Scanner {
    pub fn new(db: Pool<SqliteConnectionManager>, indexer: Arc<IndexerService>) -> Self {
        Self { db, indexer }
    }

    /// Full scan of a single directory — walk every file, index
    /// new/changed files, and mark files absent from disk as deleted.
    pub fn full_scan(
        &self,
        dir_id: &str,
        progress: impl Fn(ScanProgress),
    ) -> Result<ScanResult> {
        let start = Instant::now();
        let conn = self.db.get().context("failed to get DB connection")?;

        let config = dir_config::get_dir(&conn, dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;
        log::info!("[SCAN] 开始扫描: {}", config.path);
        let exclude = parse_exclude_patterns(&config.exclude_patterns);
        let include_exts = parse_include_exts(&config.include_exts);

        // Phase 1: Quick counting pass — determine total before processing.
        let total: u64 = walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| extension_allowed(e.path(), &include_exts))
            .count() as u64;

        progress(ScanProgress { total, processed: 0, errors: 0, current_file: String::new() });

        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut processed = 0u64;
        let mut disk_paths: Vec<String> = Vec::new();

        for entry in walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude))
        {
            let entry = entry.context("walkdir error")?;
            if !entry.file_type().is_file() { continue; }
            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();
            if !extension_allowed(&path, &include_exts) { continue; }

            processed += 1;
            disk_paths.push(path_str.clone());

            let meta = entry.metadata().context("failed to read metadata")?;
            let mtime = mtime_micros(&meta).unwrap_or(0);
            let size = meta.len();
            let existing = tracker::get_file_by_path(&conn, &path_str)?;
            let needs_index = match &existing {
                Some(r) => r.mtime != mtime || r.indexed == 0 || r.indexed == 2,
                None => true,
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &path_str, dir_id, mtime, size, None)?;
                match self.indexer.index_file(&file_id, &path, dir_id) {
                    Ok(()) => indexed += 1,
                    Err(e) => {
                        let _ = tracker::mark_failed(&conn, &file_id, &e.to_string());
                        errors += 1;
                    }
                }
            }

            if processed % 100 == 0 {
                log::info!("[SCAN] 进度 [{processed}/{total}]");
            }
            progress(ScanProgress { total, processed, errors, current_file: path_str });
        }

        // Mark files in DB but absent from disk as deleted.
        let disk_set: std::collections::HashSet<String> = disk_paths.into_iter().collect();
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status == "active" && !disk_set.contains(&rec.path) {
                let _ = self.indexer.delete_file(&rec.id);
            }
        }

        record_last_scan(&conn, dir_id)?;
        drop(conn);

        self.indexer.commit().context("failed to commit index after full scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        log::info!("[SCAN] 扫描完成: {total} files, {indexed} indexed, {errors} errors in {duration_ms}ms");
        Ok(ScanResult { total_files: total, indexed, errors, duration_ms })
    }

    /// Incremental scan — only processes files whose mtime is newer than
    /// the last recorded scan time.  Falls back to a full scan when no
    /// previous scan time exists.  Also detects deleted files.
    pub fn incremental_scan(&self, dir_id: &str) -> Result<ScanResult> {
        let start = Instant::now();
        let conn = self.db.get().context("failed to get DB connection")?;

        let last_scan = get_last_scan_time(&conn, dir_id)?;
        if last_scan == 0 {
            return self.full_scan(dir_id, |_| {});
        }

        let config = dir_config::get_dir(&conn, dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;
        let exclude = parse_exclude_patterns(&config.exclude_patterns);
        let include_exts = parse_include_exts(&config.include_exts);

        let walker = walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude));

        let mut on_disk: Vec<String> = Vec::new();
        let mut indexed = 0u64;
        let mut errors = 0u64;

        for entry in walker {
            let entry = entry.context("walkdir error")?;
            if !entry.file_type().is_file() { continue; }
            if !extension_allowed(entry.path(), &include_exts) { continue; }
            let meta = entry.metadata().context("failed to read metadata")?;
            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();
            on_disk.push(path_str.clone());

            let mtime = mtime_micros(&meta).unwrap_or(0);
            if mtime <= last_scan { continue; }

            let existing = tracker::get_file_by_path(&conn, &path_str)?;
            let needs_index = match &existing {
                Some(r) => r.mtime != mtime || r.indexed == 0 || r.indexed == 2,
                None => true,
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &path_str, dir_id, mtime, meta.len(), None)?;
                match self.indexer.index_file(&file_id, &path, dir_id) {
                    Ok(()) => indexed += 1,
                    Err(e) => {
                        let _ = tracker::mark_failed(&conn, &file_id, &e.to_string());
                        errors += 1;
                    }
                }
            }
        }

        // Detect and remove deleted files.
        let disk_set: std::collections::HashSet<String> = on_disk.into_iter().collect();
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status == "active" && !disk_set.contains(&rec.path) {
                let _ = self.indexer.delete_file(&rec.id);
            }
        }

        let total_files = tracker::get_files_by_dir(&conn, dir_id)?.len() as u64;
        record_last_scan(&conn, dir_id)?;
        drop(conn);

        self.indexer.commit().context("failed to commit index after incremental scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ScanResult { total_files, indexed, errors, duration_ms })
    }
}

// ---------------------------------------------------------------------------
// Event-processing helpers for the real-time watcher
// ---------------------------------------------------------------------------

/// Handle a single file change event from the watcher: index or delete.
impl Scanner {
    pub fn handle_event(&self, event: FileChangeEvent) -> Result<()> {
        let conn = self.db.get().context("failed to get DB connection")?;
        let file_path = &event.path;

        match event.kind {
            ChangeKind::Create | ChangeKind::Modify => {
                let path_str = file_path.to_string_lossy().to_string();
                let meta = std::fs::metadata(file_path)
                    .with_context(|| format!("failed to stat {path_str}"))?;
                let mtime = mtime_micros(&meta).unwrap_or(0);
                let size = meta.len();
                let file_id = tracker::upsert_file(&conn, &path_str, &event.dir_id, mtime, size, None)?;
                drop(conn);
                match self.indexer.index_file(&file_id, file_path, &event.dir_id) {
                    Ok(()) => log::info!("[WATCHER] indexed: {path_str}"),
                    Err(e) => log::error!("[WATCHER] failed to index {path_str}: {e}"),
                }
            }
            ChangeKind::Delete => {
                let path_str = file_path.to_string_lossy().to_string();
                if let Some(record) = tracker::get_file_by_path(&conn, &path_str)? {
                    drop(conn);
                    self.indexer.delete_file(&record.id)?;
                    log::info!("[WATCHER] deleted: {path_str}");
                }
            }
        }
        Ok(())
    }
}

/// Process a single [`FileChangeEvent`] using the provided [`IndexWriter`].
pub fn process_event(
    event: FileChangeEvent,
    conn: &rusqlite::Connection,
    writer: &mut tantivy::IndexWriter,
) -> Result<()> {
    match event.kind {
        ChangeKind::Create | ChangeKind::Modify => handle_create_modify(event, conn, writer),
        ChangeKind::Delete => handle_delete(event, conn, writer),
    }
}

/// Process a batch of events, committing the writer once at the end.
pub fn process_event_batch(
    events: Vec<FileChangeEvent>,
    conn: &rusqlite::Connection,
    writer: &mut tantivy::IndexWriter,
) -> Result<()> {
    for event in events {
        process_event(event, conn, writer)?;
    }
    Indexer::commit(writer)?;
    Ok(())
}

fn handle_create_modify(
    event: FileChangeEvent,
    conn: &rusqlite::Connection,
    writer: &mut tantivy::IndexWriter,
) -> Result<()> {
    let path_str = event.path.to_string_lossy().to_string();
    let meta = std::fs::metadata(&event.path)?;
    let mtime = mtime_micros(&meta).unwrap_or(0);
    let size = meta.len();
    let file_name = event.path
        .file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let file_ext = event.path
        .extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();

    let file_id = tracker::upsert_file(conn, &path_str, &event.dir_id, mtime, size, None)?;
    let text = crate::extractor::extract_text(&event.path).unwrap_or_default();
    Indexer::add_document(writer, &file_id, &file_name, &file_ext, &event.dir_id, &text, mtime, size)?;

    let hash = format!("{:x}", md5::Md5::digest(text.as_bytes()));
    tracker::update_indexed(conn, &file_id, Some(&hash))?;
    Ok(())
}

fn handle_delete(
    event: FileChangeEvent,
    conn: &rusqlite::Connection,
    writer: &mut tantivy::IndexWriter,
) -> Result<()> {
    let path_str = event.path.to_string_lossy().to_string();
    if let Some(record) = tracker::get_file_by_path(conn, &path_str)? {
        Indexer::delete_document(writer, &record.id)?;
        tracker::mark_deleted(conn, &path_str)?;
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::search::IndexManager;
    use tantivy::Index;
    use std::sync::Arc;

    fn setup_env() -> (Pool<SqliteConnectionManager>, Arc<IndexerService>, String) {
        let pool = r2d2::Pool::builder().max_size(2)
            .build(SqliteConnectionManager::memory()).unwrap();
        {
            let c = pool.get().unwrap();
            db::run_migrations(&c).unwrap();
        }
        let schema = crate::search::schema::build_schema();
        let _index = Index::create_in_ram(schema);
        crate::search::schema::register_tokenizers(&_index);
        let im = Arc::new(std::sync::RwLock::new(IndexManager::create_in_ram()));
        let svc = Arc::new(IndexerService::new(pool.clone(), im));
        let dir_id = {
            let c = pool.get().unwrap();
            dir_config::add_dir(&c, std::env::temp_dir().to_str().unwrap(),
                Some("test"), None, None, None, true).unwrap().id
        };
        (pool, svc, dir_id)
    }

    #[test]
    fn test_full_scan_empty_dir() {
        let dir = std::env::temp_dir().join(format!("scan_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (pool, svc, dir_id) = setup_env();
        {
            let c = pool.get().unwrap();
            c.execute("UPDATE dir_config SET path=?1 WHERE id=?2",
                rusqlite::params![dir.to_str().unwrap(), dir_id]).unwrap();
        }
        let result = Scanner::new(pool, svc).full_scan(&dir_id, |_| {}).unwrap();
        assert_eq!(result.total_files, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_full_scan_indexes_files() {
        let dir = std::env::temp_dir().join(format!("scan_files_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        std::fs::write(dir.join("b.txt"), "world").unwrap();

        let (pool, svc, dir_id) = setup_env();
        {
            let c = pool.get().unwrap();
            c.execute("UPDATE dir_config SET path=?1 WHERE id=?2",
                rusqlite::params![dir.to_str().unwrap(), dir_id]).unwrap();
        }
        let result = Scanner::new(pool, svc.clone()).full_scan(&dir_id, |_| {}).unwrap();
        assert_eq!(result.total_files, 2);
        assert_eq!(result.indexed, 2);
        assert_eq!(result.errors, 0);

        svc.commit().unwrap();
        let mgr = svc.index_manager.read().unwrap();
        let reader = mgr.reader().unwrap();
        let searcher = reader.searcher();
        let schema = crate::search::schema::build_schema();
        let cf = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(mgr.index(), vec![cf]);
        let top = searcher.search(&parser.parse_query("hello").unwrap(),
            &tantivy::collector::TopDocs::with_limit(10)).unwrap();
        assert_eq!(top.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_incremental_scan_detects_new_file() {
        let dir = std::env::temp_dir().join(format!("scan_incr_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("existing.txt"), "existing content").unwrap();

        let (pool, svc, dir_id) = setup_env();
        {
            let c = pool.get().unwrap();
            c.execute("UPDATE dir_config SET path=?1 WHERE id=?2",
                rusqlite::params![dir.to_str().unwrap(), dir_id]).unwrap();
        }

        let scanner = Scanner::new(pool, svc.clone());
        scanner.full_scan(&dir_id, |_| {}).unwrap();
        svc.commit().unwrap();

        // Add a new file after the full scan.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(dir.join("new.txt"), "new file content").unwrap();

        let r2 = scanner.incremental_scan(&dir_id).unwrap();
        assert_eq!(r2.indexed, 1);
        svc.commit().unwrap();

        let mgr = svc.index_manager.read().unwrap();
        let reader = mgr.reader().unwrap();
        let searcher = reader.searcher();
        let schema = crate::search::schema::build_schema();
        let cf = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(mgr.index(), vec![cf]);
        let top = searcher.search(&parser.parse_query("existing").unwrap(),
            &tantivy::collector::TopDocs::with_limit(10)).unwrap();
        assert_eq!(top.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}