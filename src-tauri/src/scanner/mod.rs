//! Directory scanner — full and incremental scans with progress reporting.
//!
//! Also provides [`handle_event`] for the real-time file watcher to consume
//! [`FileChangeEvent`]s.

pub mod watcher;
pub mod helpers;

use self::helpers::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use md5::Digest;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::collections::HashMap;

use crate::db::dir_config;
use crate::db::tracker::{self, FileRecord};
use crate::indexer::{BatchJob, IndexerService};

pub use watcher::{ChangeKind, FileChangeEvent, FileWatcher, WatcherCommand};

/// Progress snapshot emitted during a scan.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub total: u64,
    pub processed: u64,
    pub errors: u64,
    pub current_file: String,
    pub phase: &'static str, // "scan" | "index"
}

/// Summary returned after a scan completes.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub total_files: u64,
    pub indexed: u64,
    pub added: u64,
    pub deleted: u64,
    pub modified: u64,
    pub errors: u64,
    pub duration_ms: u64,
}

/// Skip when already indexed and unchanged — spurious Modify events (e.g.
/// Finder QuickLook atime bump) must not trigger re-indexing.
fn should_skip_watcher_event(rec: &FileRecord, mtime: i64, size: u64) -> bool {
    rec.indexed == 1 && rec.mtime == mtime && rec.size == size
}

/// Walks a directory tree, discovers files, and indexes them via
/// [`IndexerService`].
pub struct Scanner {
    db: Pool<SqliteConnectionManager>,
    indexer: Arc<IndexerService>,
    cancel_scan: Arc<AtomicBool>,
}

impl Scanner {
    pub fn new(db: Pool<SqliteConnectionManager>, indexer: Arc<IndexerService>) -> Self {
        Self::with_cancel(db, indexer, Arc::new(AtomicBool::new(false)))
    }

    /// Same as [`new`], but shares the app-wide cancel flag so the
    /// `cancel_scan` command interrupts the walk and batch index.
    pub fn with_cancel(
        db: Pool<SqliteConnectionManager>,
        indexer: Arc<IndexerService>,
        cancel_scan: Arc<AtomicBool>,
    ) -> Self {
        Self { db, indexer, cancel_scan }
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
        if config.private {
            log::info!("[SCAN] 私密目录跳过: {}", config.path);
            return Ok(ScanResult { total_files: 0, indexed: 0, added: 0, deleted: 0, modified: 0, errors: 0, duration_ms: 0 });
        }
        let dir_root = &config.path;
        log::info!("[SCAN] 开始扫描: {}", config.path);
        let exclude = parse_exclude_patterns(&config.exclude_patterns);
        let include_exts = parse_include_exts(&config.include_exts);

        // Record last scan time BEFORE walking — prevents missing files
        // that are modified during the scan.
        record_last_scan(&conn, dir_id)?;
        tracker::reset_unsupported_ext(&conn, dir_id);

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

        progress(ScanProgress { total, processed: 0, errors: 0, current_file: String::new(), phase: "scan" });

        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut processed = 0u64;
        let mut on_disk: Vec<DiskEntry> = Vec::new();
        let mut jobs: Vec<BatchJob> = Vec::new();
        let mut added = 0u64;
        let mut modified = 0u64;
        let mut deleted = 0u64;

        // Bulk-load tracked records once instead of a per-file indexed SELECT.
        let records: HashMap<String, FileRecord> = tracker::get_files_by_dir(&conn, dir_id)?
            .into_iter()
            .map(|r| (r.path.clone(), r))
            .collect();

        for entry in walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude))
        {
            let Ok(entry) = entry else {
                errors += 1;
                continue;
            };
            if !entry.file_type().is_file() { continue; }
            if self.cancel_scan.load(Ordering::Acquire) {
                log::info!("[SCAN] 扫描已取消, 停止遍历");
                break;
            }
            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();
            let rel_path = to_relative(dir_root, &path)?;
            if !extension_allowed(&path, &include_exts) {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    tracker::bump_unsupported_ext(&conn, &ext.to_lowercase(), dir_id);
                }
                continue;
            }

            processed += 1;
            let name = entry.file_name().to_string_lossy().to_string();

            let meta = match crate::scanner::helpers::metadata_timeout(entry.path(), std::time::Duration::from_secs(15)) { Ok(m) => m, Err(e) => { log::warn!("[SCAN] metadata error: {e}"); errors += 1; continue; } };
            let mtime = mtime_micros(&meta).unwrap_or(0);
            let size = meta.len();
            let existing = records.get(&rel_path).cloned();
            let needs_index = needs_reindex(&existing, mtime);
            if let Some(r) = &existing {
                if r.mtime != mtime { modified += 1; }
            } else {
                added += 1;
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &rel_path, dir_id, mtime, size, None)?;
                jobs.push(BatchJob { file_id, file_path: path, rel_path: rel_path.clone(), dir_id: dir_id.to_string() });
            }

            on_disk.push(DiskEntry { abs_path: path_str, rel_path, size, name });

            if processed % 100 == 0 {
                let elapsed = start.elapsed().as_secs_f64();
                let rate = processed as f64 / elapsed.max(0.001);
                let eta = (total - processed) as f64 / rate.max(0.001);
                log::info!("[SCAN] 进度 [{processed}/{total}] {:.1}s, {:.0} f/s, ETA {:.0}s", elapsed, rate, eta);
            }
            progress(ScanProgress { total, processed, errors, current_file: String::new(), phase: "scan" });
        }

        if !jobs.is_empty() && !self.cancel_scan.load(Ordering::Acquire) {
            for r in self.indexer.batch_index(jobs, |done, total| {
                let _ = progress(ScanProgress {
                    total,
                    processed: done,
                    errors: 0,
                    current_file: String::new(),
                    phase: "index",
                });
            })? {
                if r.success {
                    indexed += 1;
                } else {
                    errors += 1;
                }
            }
        }

        // Cancelled — skip the delete-detection pass, commit partial results.
        if self.cancel_scan.load(Ordering::Acquire) {
            log::info!("[SCAN] 已取消, 提交部分结果并返回");
            drop(conn);
            self.indexer.commit().context("failed to commit index after cancelled scan")?;
            let duration_ms = start.elapsed().as_millis() as u64;
            return Ok(ScanResult { total_files: total, indexed, added, deleted, modified, errors, duration_ms });
        }

        // Mark files in DB but absent from disk as deleted.
        let disk_set: std::collections::HashSet<&str> = on_disk.iter().map(|e| e.rel_path.as_str()).collect();
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status == "active" && !disk_set.contains(rec.path.as_str()) {
                // ext 被 include_exts 过滤但仍在磁盘：保留记录，非删除。
                if include_exts.is_some()
                    && !extension_allowed(std::path::Path::new(&rec.path), &include_exts)
                    && std::path::Path::new(dir_root).join(&rec.path).exists()
                {
                    continue;
                }
                let _ = self.indexer.delete_file(&rec.id);
                deleted += 1;
            }
        }

        drop(conn);

        self.indexer.commit().context("failed to commit index after full scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        log::info!("[SCAN] 扫描完成: {total} files, {indexed} indexed, {errors} errors in {duration_ms}ms");
        Ok(ScanResult { total_files: total, indexed, added, deleted, modified, errors, duration_ms })
    }

    /// Incremental scan — only processes files whose mtime is newer than
    /// the last recorded scan time.  Falls back to a full scan when no
    /// previous scan time exists.  Also detects deleted files.
    pub fn incremental_scan(
        &self,
        dir_id: &str,
        progress: impl Fn(ScanProgress),
    ) -> Result<ScanResult> {
        let start = Instant::now();
        let conn = self.db.get().context("failed to get DB connection")?;

        let last_scan = get_last_scan_time(&conn, dir_id)?;
        if last_scan == 0 {
            return self.full_scan(dir_id, |_| {});
        }

        let config = dir_config::get_dir(&conn, dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;
        if config.private {
            log::info!("[SCAN] 私密目录跳过(增量): {}", config.path);
            return Ok(ScanResult { total_files: 0, indexed: 0, added: 0, deleted: 0, modified: 0, errors: 0, duration_ms: 0 });
        }
        let dir_root = &config.path;
        let exclude = parse_exclude_patterns(&config.exclude_patterns);
        let include_exts = parse_include_exts(&config.include_exts);

        let walker = walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude));

        // Record last scan time BEFORE walking the main loop.
        record_last_scan(&conn, dir_id)?;
        tracker::reset_unsupported_ext(&conn, dir_id);

        // Load all tracked records once. The old mtime gate skipped every
        // unchanged file's DB lookup; we now need the record for every file
        // to retry previously-failed ones, so a single bulk query + in-memory
        // lookup beats a per-file indexed SELECT.
        let records: HashMap<String, crate::db::tracker::FileRecord> =
            tracker::get_files_by_dir(&conn, dir_id)?
                .into_iter()
                .map(|r| (r.path.clone(), r))
                .collect();

        let mut on_disk: Vec<DiskEntry> = Vec::new();
        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut jobs: Vec<BatchJob> = Vec::new();
        let mut added = 0u64;
        let mut modified = 0u64;
        let mut processed = 0u64;

        for entry in walker {
            let Ok(entry) = entry else {
                errors += 1;
                continue;
            };
            if !entry.file_type().is_file() { continue; }
            if !extension_allowed(entry.path(), &include_exts) {
                if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                    tracker::bump_unsupported_ext(&conn, &ext.to_lowercase(), dir_id);
                }
                continue;
            }
            if self.cancel_scan.load(Ordering::Acquire) {
                log::info!("[SCAN] 扫描已取消, 停止遍历");
                break;
            }
            let meta = match crate::scanner::helpers::metadata_timeout(entry.path(), std::time::Duration::from_secs(15)) { Ok(m) => m, Err(e) => { log::warn!("[SCAN] metadata error: {e}"); errors += 1; continue; } };
            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();
            let rel_path = to_relative(dir_root, &path)?;
            let name = entry.file_name().to_string_lossy().to_string();
            processed += 1;
            progress(ScanProgress { total: 0, processed, errors: 0, current_file: path_str.clone(), phase: "scan" });
            on_disk.push(DiskEntry { abs_path: path_str, rel_path: rel_path.clone(), size: meta.len(), name });

            let mtime = mtime_micros(&meta).unwrap_or(0);
            let existing = records.get(&rel_path).cloned();
            let needs_index = needs_reindex(&existing, mtime);
            if let Some(r) = &existing {
                if r.mtime != mtime { modified += 1; }
            } else {
                added += 1;
            };
            // Skip only when mtime unchanged AND no reindex needed — this
            // excludes failed files (handled via manual re-index only).
            if mtime <= last_scan && !needs_index { continue; }

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &rel_path, dir_id, mtime, meta.len(), None)?;
                jobs.push(BatchJob { file_id, file_path: path, rel_path: rel_path.clone(), dir_id: dir_id.to_string() });
            }
        }

        if !jobs.is_empty() && !self.cancel_scan.load(Ordering::Acquire) {
            for r in self.indexer.batch_index(jobs, |done, total| {
                let _ = progress(ScanProgress {
                    total,
                    processed: done,
                    errors: 0,
                    current_file: String::new(),
                    phase: "index",
                });
            })? {
                if r.success {
                    indexed += 1;
                } else {
                    errors += 1;
                }
            }
        }

        // Cancelled — skip the delete-detection pass, commit partial results.
        if self.cancel_scan.load(Ordering::Acquire) {
            log::info!("[SCAN] 已取消, 提交部分结果并返回");
            let total_files = tracker::get_files_by_dir(&conn, dir_id)?.len() as u64;
            drop(conn);
            self.indexer.commit().context("failed to commit index after cancelled scan")?;
            let duration_ms = start.elapsed().as_millis() as u64;
            return Ok(ScanResult { total_files, indexed, added, deleted: 0, modified, errors, duration_ms });
        }

        // Detect and remove deleted files.
        let disk_set: std::collections::HashSet<&str> = on_disk.iter().map(|e| e.rel_path.as_str()).collect();
        let mut deleted = 0u64;
        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status == "active" && !disk_set.contains(rec.path.as_str()) {
                // ext 被 include_exts 过滤但仍在磁盘：保留记录，非删除。
                if include_exts.is_some()
                    && !extension_allowed(std::path::Path::new(&rec.path), &include_exts)
                    && std::path::Path::new(dir_root).join(&rec.path).exists()
                {
                    continue;
                }
                let _ = self.indexer.delete_file(&rec.id);
                deleted += 1;
            }
        }

        let total_files = tracker::get_files_by_dir(&conn, dir_id)?.len() as u64;
        drop(conn);

        self.indexer.commit().context("failed to commit index after incremental scan")?;
        let duration_ms = start.elapsed().as_millis() as u64;
        Ok(ScanResult { total_files, indexed, added, deleted, modified, errors, duration_ms })
    }

    pub fn startup_scan(
        &self,
        dir_id: &str,
        progress: impl Fn(ScanProgress),
    ) -> Result<ScanResult> {
        let start = Instant::now();
        let conn = self.db.get().context("failed to get DB connection")?;

        let config = dir_config::get_dir(&conn, dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;
        if config.private {
            log::info!("[STARTUP] 私密目录跳过: {}", config.path);
            return Ok(ScanResult { total_files: 0, indexed: 0, added: 0, deleted: 0, modified: 0, errors: 0, duration_ms: 0 });
        }
        let dir_root = &config.path;
        log::info!("[STARTUP] 启动扫描: {}", config.path);
        let exclude = parse_exclude_patterns(&config.exclude_patterns);
        let include_exts = parse_include_exts(&config.include_exts);

        let walker = walkdir::WalkDir::new(&config.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_excluded(e.path(), &exclude));

        // Record last scan time BEFORE walking the main loop.
        record_last_scan(&conn, dir_id)?;
        tracker::reset_unsupported_ext(&conn, dir_id);

        let mut on_disk: Vec<DiskEntry> = Vec::new();
        let mut indexed = 0u64;
        let mut errors = 0u64;
        let mut moved = 0u64;
        let mut added = 0u64;
        let mut modified = 0u64;
        let mut deleted = 0u64;
        let mut jobs: Vec<BatchJob> = Vec::new();
        let mut processed = 0u64;

        for entry in walker {
            let Ok(entry) = entry else {
                errors += 1;
                continue;
            };
            if !entry.file_type().is_file() { continue; }
            if !extension_allowed(entry.path(), &include_exts) {
                if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                    tracker::bump_unsupported_ext(&conn, &ext.to_lowercase(), dir_id);
                }
                continue;
            }
            if self.cancel_scan.load(Ordering::Acquire) {
                log::info!("[SCAN] 启动扫描已取消, 停止遍历");
                break;
            }
            let meta = match crate::scanner::helpers::metadata_timeout(entry.path(), std::time::Duration::from_secs(15)) { Ok(m) => m, Err(e) => { log::warn!("[SCAN] metadata error: {e}"); errors += 1; continue; } };
            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();
            let rel_path = to_relative(dir_root, &path)?;
            let name = entry.file_name().to_string_lossy().to_string();
            processed += 1;
            progress(ScanProgress { total: 0, processed, errors: 0, current_file: path_str.clone(), phase: "scan" });

            let mtime = mtime_micros(&meta).unwrap_or(0);
            let size = meta.len();

            let existing = tracker::get_file_by_path(&conn, &rel_path)?;
            let needs_index = needs_reindex(&existing, mtime);
            if let Some(r) = &existing {
                if r.mtime != mtime { modified += 1; }
            } else {
                added += 1;
            };

            if needs_index {
                let file_id = tracker::upsert_file(&conn, &rel_path, dir_id, mtime, size, None)?;
                jobs.push(BatchJob { file_id, file_path: path, rel_path: rel_path.clone(), dir_id: dir_id.to_string() });
            }

            on_disk.push(DiskEntry { abs_path: path_str, rel_path, size, name });
        }

        if !jobs.is_empty() && !self.cancel_scan.load(Ordering::Acquire) {
            for r in self.indexer.batch_index(jobs, |done, total| {
                let _ = progress(ScanProgress {
                    total,
                    processed: done,
                    errors: 0,
                    current_file: String::new(),
                    phase: "index",
                });
            })? {
                if r.success {
                    indexed += 1;
                } else {
                    errors += 1;
                }
            }
        }

        // Cancelled — skip cleanup/move detection, commit partial results.
        if self.cancel_scan.load(Ordering::Acquire) {
            log::info!("[SCAN] 启动扫描已取消, 提交部分结果并返回");
            let total_files = tracker::get_files_by_dir(&conn, dir_id)?.len() as u64;
            drop(conn);
            self.indexer.commit().context("failed to commit index after cancelled scan")?;
            let duration_ms = start.elapsed().as_millis() as u64;
            return Ok(ScanResult { total_files, indexed, added, deleted, modified, errors, duration_ms });
        }

        let disk_set: std::collections::HashSet<String> =
            on_disk.iter().map(|e| e.rel_path.clone()).collect();

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

        // Build HashMap index: (name, size) -> Vec<DiskEntry> for O(1) move detection.
        let mut by_name_size: HashMap<(String, u64), Vec<DiskEntry>> = HashMap::new();
        for entry in &on_disk {
            by_name_size.entry((entry.name.clone(), entry.size)).or_default().push(entry.clone());
        }

        for rec in &tracker::get_files_by_dir(&conn, dir_id)? {
            if rec.status != "active" || disk_set.contains(&rec.path) {
                continue;
            }

            let old_name = std::path::Path::new(&rec.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let candidate = by_name_size
                .get(&(old_name, rec.size))
                .and_then(|candidates| candidates.iter().find(|e| e.rel_path != rec.path));

            if let Some(found) = candidate {
                // MD5 校验限 ≤10MB（大文件避免整体读入内存 OOM）。大文件候选
                // 视为"已移动"处理：不比对内容，直接删旧记录——新路径的记录
                // B 已由 walk 建立并索引，旧记录不再保留（防幽灵残留）。
                let is_move = found.size <= 10 * 1024 * 1024
                    && rec.md5.as_deref().is_some_and(|expected_md5| {
                        std::fs::read(&found.abs_path)
                            .map(|raw| format!("{:x}", md5::Md5::digest(&raw)) == expected_md5)
                            .unwrap_or(false)
                    });
                if is_move {
                    // 移位：walk 已为新路径建了记录 B（含 Tantivy 文档）。
                    // 硬删 B，让原记录 A 接管新路径——A 保留 md5/内容，不重提取。
                    // file_tracking.path 是 STORED 非 indexed，Tantivy 无需重写。
                    if let Ok(Some(new_rec)) =
                        tracker::get_file_by_path(&conn, &found.rel_path)
                    {
                        tracker::hard_delete_file(&conn, &new_rec.id)?;
                        let _ = self.indexer.delete_document_only(&new_rec.id);
                        tracker::update_file_path(&conn, &rec.id, &found.rel_path, dir_id)?;
                        log::info!("[STARTUP] 移位(MD5匹配): {} -> {}", rec.path, found.abs_path);
                        moved += 1;
                        continue;
                    }
                }
            }

            // ext 被 include_exts 过滤但仍在磁盘：保留记录，非删除。
            if include_exts.is_some()
                && !extension_allowed(std::path::Path::new(&rec.path), &include_exts)
                && std::path::Path::new(&config.path).join(&rec.path).exists()
            {
                continue;
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
        Ok(ScanResult { total_files, indexed, added, deleted, modified, errors, duration_ms })
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

        // Get directory root for this dir_id
        let dir_config = dir_config::get_dir(&conn, &event.dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir config not found: {}", event.dir_id))?;
        if dir_config.private {
            return Ok(());
        }
        let dir_root = &dir_config.path;

        // Compute relative path
        let rel_path = to_relative(dir_root, file_path)?;
        let path_str = file_path.to_string_lossy().to_string();

        match event.kind {
            ChangeKind::Create | ChangeKind::Modify => {
                let exclude = parse_exclude_patterns(&dir_config.exclude_patterns);
                if is_excluded(file_path, &exclude) {
                    return Ok(());
                }
                let meta = crate::scanner::helpers::metadata_timeout(file_path, std::time::Duration::from_secs(10))
                    .with_context(|| format!("failed to stat {path_str}"))?;
                let mtime = mtime_micros(&meta).unwrap_or(0);
                let size = meta.len();

                // Skip files already marked as failed — soffice side-effects
                // (mtime bump, lock-file create/delete) would otherwise
                // trigger an infinite re-index loop on broken docs.
                let existing = tracker::get_file_by_path(&conn, &rel_path)?;
                if let Some(rec) = &existing {
                    if rec.indexed == 2 {
                        // Failed — skip to avoid infinite re-index loop
                        return Ok(());
                    }
                }

                let file_id = tracker::upsert_file(&conn, &rel_path, &event.dir_id, mtime, size, None)?;

                // Compare against the PRE-upsert record: upsert_file
                // overwrites mtime, so a post-upsert lookup always matches
                // and would swallow every Create/Modify event.
                let skip = existing
                    .as_ref()
                    .is_some_and(|rec| should_skip_watcher_event(rec, mtime, size));
                drop(conn);
                if skip {
                    return Ok(());
                }
                match self.indexer.index_file(&file_id, file_path, &event.dir_id) {
                    Ok(()) => log::info!("[WATCHER] indexed: {path_str}"),
                    Err(e) => log::error!("[WATCHER] failed to index {path_str}: {e}"),
                }
            }
            ChangeKind::Delete => {
                // Look up by relative path since that's how it's stored in DB
                if let Some(record) = tracker::get_file_by_path(&conn, &rel_path)? {
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
        let mgr = svc.index_manager.read().unwrap(); // nosemgrep: rust-rwlock-read-unwrap
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

        let r2 = scanner.incremental_scan(&dir_id, |_| {}).unwrap();
        assert_eq!(r2.indexed, 1);
        svc.commit().unwrap();

        let mgr = svc.index_manager.read().unwrap(); // nosemgrep: rust-rwlock-read-unwrap
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

#[cfg(test)]
mod watcher_skip_tests {
    use crate::db::tracker::FileRecord;

    fn rec(indexed: i64) -> FileRecord {
        FileRecord {
            id: "id".into(),
            path: "p".into(),
            dir_id: "d".into(),
            mtime: 100,
            size: 5,
            md5: None,
            status: "active".into(),
            indexed,
            error_msg: None,
            created_at: 0,
            updated_at: 0,
            dead_content: 0,
        }
    }

    #[test]
    fn skip_only_when_indexed_and_unchanged() {
        assert!(super::should_skip_watcher_event(&rec(1), 100, 5));
        assert!(!super::should_skip_watcher_event(&rec(1), 101, 5));
        assert!(!super::should_skip_watcher_event(&rec(1), 100, 6));
        assert!(!super::should_skip_watcher_event(&rec(0), 100, 5));
        assert!(!super::should_skip_watcher_event(&rec(3), 100, 5));
    }
}