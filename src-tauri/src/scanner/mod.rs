//! Directory scanner — full and incremental scans with progress reporting.
//!
//! Also provides [`handle_event`] for the real-time file watcher to consume
//! [`FileChangeEvent`]s.

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
use crate::indexer::{BatchJob, IndexerService};

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
    pub deleted: u64,
    pub modified: u64,
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

        // Record last scan time BEFORE walking — prevents missing files
        // that are modified during the scan.
        record_last_scan(&conn, dir_id)?;

        // Phase 1: Count files for the progress bar.
        //
        // Two passes are intentional — the first gives the frontend a `total`
        // so the progress bar renders determinate.  The second does the real
        // indexing.
        //
        // walkdir calls `stat` internally on every entry (it needs the full
        // `stat` struct for descent decisions), so we cannot avoid the syscall
        // entirely.  This first pass is still cheaper because it **only** does
        // the count — no DB queries, no mtime parsing, no indexing.  For very
        // large directories the count itself can take seconds, so we time-box
        // it at 3 s.  When it times out we report total=0 (unknown) and let
        // the frontend show indeterminate progress.
        let total: u64 = {
            let (tx, rx) = std::sync::mpsc::channel();
            let path = config.path.clone();
            let excl = exclude.clone();
            let exts = include_exts.clone();
            std::thread::spawn(move || {
                let n = walkdir::WalkDir::new(&path)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|e| !is_excluded(e.path(), &excl))
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                    .filter(|e| extension_allowed(e.path(), &exts))
                    .count() as u64;
                let _ = tx.send(n);
            });
            match rx.recv_timeout(std::time::Duration::from_secs(3)) {
                Ok(n) => n,
                Err(_) => {
                    log::warn!("[SCAN] count phase timed out (>3 s), showing indeterminate progress");
                    0
                }
            }
        };

        progress(ScanProgress { total, processed: 0, errors: 0, current_file: String::new() });

        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut processed = 0u64;
        let mut disk_paths: Vec<String> = Vec::new();
        let mut jobs: Vec<BatchJob> = Vec::new();
        let mut added = 0u64;
        let mut modified = 0u64;
        let mut deleted = 0u64;

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
                Some(r) => {
                    if r.mtime != mtime {
                        modified += 1;
                    }
                    r.mtime != mtime || r.indexed == 0 || r.indexed == 2
                },
                None => {
                    added += 1;
                    true
                },
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &path_str, dir_id, mtime, size, None)?;
                jobs.push(BatchJob { file_id, file_path: path, dir_id: dir_id.to_string() });
            }

            if processed % 100 == 0 {
                log::info!("[SCAN] 进度 [{processed}/{total}]");
            }
            progress(ScanProgress { total, processed, errors, current_file: path_str });
        }

        if !jobs.is_empty() {
            for r in self.indexer.batch_index(jobs)? {
                if r.success {
                    indexed += 1;
                } else {
                    errors += 1;
                }
            }
        }

        // Mark files in DB but absent from disk as deleted.
        let disk_set: std::collections::HashSet<String> = disk_paths.into_iter().collect();
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status == "active" && !disk_set.contains(&rec.path) {
                let _ = self.indexer.delete_file(&rec.id);
                deleted += 1;
            }
        }

        drop(conn);

        self.indexer.commit().context("failed to commit index after full scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        log::info!("[SCAN] 扫描完成: {total} files, {indexed} indexed, {errors} errors in {duration_ms}ms");
        Ok(ScanResult { total_files: total, indexed, deleted, modified, errors, duration_ms })
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

        // Record last scan time BEFORE walking the main loop.
        record_last_scan(&conn, dir_id)?;

        let mut on_disk: Vec<String> = Vec::new();
        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut jobs: Vec<BatchJob> = Vec::new();
        let mut added = 0u64;
        let mut modified = 0u64;

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
                Some(r) => {
                    if r.mtime != mtime {
                        modified += 1;
                    }
                    r.mtime != mtime || r.indexed == 0 || r.indexed == 2
                },
                None => {
                    added += 1;
                    true
                },
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &path_str, dir_id, mtime, meta.len(), None)?;
                jobs.push(BatchJob { file_id, file_path: path, dir_id: dir_id.to_string() });
            }
        }

        if !jobs.is_empty() {
            for r in self.indexer.batch_index(jobs)? {
                if r.success {
                    indexed += 1;
                } else {
                    errors += 1;
                }
            }
        }

        // Detect and remove deleted files.
        let disk_set: std::collections::HashSet<String> = on_disk.into_iter().collect();
        let mut deleted = 0u64;
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status == "active" && !disk_set.contains(&rec.path) {
                let _ = self.indexer.delete_file(&rec.id);
                deleted += 1;
            }
        }

        let total_files = tracker::get_files_by_dir(&conn, dir_id)?.len() as u64;
        drop(conn);

        self.indexer.commit().context("failed to commit index after incremental scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ScanResult { total_files, indexed, deleted, modified, errors, duration_ms })
    }

    pub fn startup_scan(&self, dir_id: &str) -> Result<ScanResult> {
        let start = Instant::now();
        let conn = self.db.get().context("failed to get DB connection")?;

        let config = dir_config::get_dir(&conn, dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;
        log::info!("[STARTUP] 启动扫描: {}", config.path);
        let exclude = parse_exclude_patterns(&config.exclude_patterns);
        let include_exts = parse_include_exts(&config.include_exts);

        let walker = walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude));

        // Record last scan time BEFORE walking the main loop.
        record_last_scan(&conn, dir_id)?;

        struct DiskEntry {
            path: String,
            size: u64,
            name: String,
        }

        let mut on_disk: Vec<DiskEntry> = Vec::new();
        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut moved = 0u64;
        let mut added = 0u64;
        let mut modified = 0u64;
        let mut deleted = 0u64;
        let mut jobs: Vec<BatchJob> = Vec::new();

        for entry in walker {
            let entry = entry.context("walkdir error")?;
            if !entry.file_type().is_file() { continue; }
            if !extension_allowed(entry.path(), &include_exts) { continue; }
            let meta = entry.metadata().context("failed to read metadata")?;
            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();
            let name = entry.file_name().to_string_lossy().to_string();

            let mtime = mtime_micros(&meta).unwrap_or(0);
            let size = meta.len();

            let existing = tracker::get_file_by_path(&conn, &path_str)?;
            let needs_index = match &existing {
                Some(r) => {
                    if r.mtime != mtime {
                        modified += 1;
                    }
                    r.mtime != mtime || r.indexed == 0 || r.indexed == 2
                },
                None => {
                    added += 1;
                    true
                },
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &path_str, dir_id, mtime, size, None)?;
                jobs.push(BatchJob { file_id, file_path: path, dir_id: dir_id.to_string() });
            }

            on_disk.push(DiskEntry { path: path_str, size, name });
        }

        if !jobs.is_empty() {
            for r in self.indexer.batch_index(jobs)? {
                if r.success {
                    indexed += 1;
                } else {
                    errors += 1;
                }
            }
        }

        let disk_set: std::collections::HashSet<String> =
            on_disk.iter().map(|e| e.path.clone()).collect();

        let mut cleaned = 0u64;
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status != "active" {
                continue;
            }
            if is_excluded(std::path::Path::new(&rec.path), &exclude) {
                self.indexer.delete_file(&rec.id)?;
                cleaned += 1;
                deleted += 1;
            }
        }
        if cleaned > 0 {
            log::info!("[STARTUP] 清理排除文件: {} 个", cleaned);
        }

        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status != "active" || disk_set.contains(&rec.path) {
                continue;
            }

            let old_name = std::path::Path::new(&rec.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let candidate = on_disk.iter().find(|e| {
                e.name == old_name
                    && e.size == rec.size
                    && e.path != rec.path
                    && tracker::get_file_by_path(&conn, &e.path)
                        .map(|r| r.is_none())
                        .unwrap_or(true)
            });

            if let Some(found) = candidate {
                if let Some(ref expected_md5) = rec.md5 {
                    if let Ok(raw) = std::fs::read(&found.path) {
                        let actual_md5 = format!("{:x}", md5::Md5::digest(&raw));
                        if actual_md5 == *expected_md5 {
                            tracker::update_file_path(&conn, &rec.id, &found.path, dir_id)?;
                            log::info!("[STARTUP] 移位(MD5匹配): {} -> {}", rec.path, found.path);
                            moved += 1;
                            continue;
                        }
                    }
                }
            }

            self.indexer.delete_file(&rec.id)?;
            log::info!("[STARTUP] 删除: {}", rec.path);
            deleted += 1;
        }

        let total_files = tracker::get_files_by_dir(&conn, dir_id)?.len() as u64;
        drop(conn);

        self.indexer.commit().context("failed to commit index after startup scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        log::info!(
            "[STARTUP] {} 完成: {} files, {} indexed, {} moved, {} errors in {}ms",
            config.path, total_files, indexed, moved, errors, duration_ms
        );
        Ok(ScanResult { total_files, indexed, deleted, modified, errors, duration_ms })
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