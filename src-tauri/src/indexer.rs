//! Orchestrates Tantivy indexing with SQLite tracking and content deduplication.
//!
//! [`IndexerService`] wraps an [`IndexManager`] and DB pool to provide a
//! high-level API for indexing and deleting files.  The tantivy writer is
//! lazily created on first use and shared via a mutex so multiple callers
//! can batch documents before a single [`commit`](IndexerService::commit).

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use md5::Digest;
use r2d2::Pool;
use r2d2_sqlite::{rusqlite::Connection, SqliteConnectionManager};
use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tantivy::IndexWriter;

use crate::search::indexer::Indexer;
use crate::search::IndexManager;

/// High-level indexer that coordinates Tantivy writes, content dedup, and
/// file-tracking status.
pub struct IndexerService {
    db: Pool<SqliteConnectionManager>,
    pub(crate) index_manager: Arc<RwLock<IndexManager>>,
    writer: Mutex<Option<IndexWriter>>,
    commit_counter: AtomicU64,
    commit_interval: AtomicU64,
    batch_io_concurrency: AtomicUsize,
    cancel_scan: Arc<AtomicBool>,
}

// SAFETY: IndexWriter is `Send` (it holds owned channels + Arc<Mutex<…>>).
// Mutex<Option<IndexWriter>> adds Sync, so IndexerService itself is Sync.

/// A single file to be processed in a batch index operation.
pub struct BatchJob {
    pub file_id: String,
    pub file_path: PathBuf,
    pub rel_path: String,  // Relative path for storing in Tantivy index
    pub dir_id: String,
}

/// Outcome of indexing a single file within a batch.
pub struct BatchResult {
    pub file_id: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Default cap on Phase-1 concurrent file reads in [`batch_index`]
/// (read + MD5 + extraction + per-file SQLite `mark_extracted` writes).
const DEFAULT_BATCH_IO_CONCURRENCY: usize = 8;

/// Process-wide cache of dedicated Rayon pools for Phase-1 batch reads,
/// keyed by concurrency cap. The global Rayon pool is unbounded
/// (num_cpus threads), which is exactly what must be avoided here.
static BATCH_IO_POOLS: OnceLock<Mutex<HashMap<usize, Arc<ThreadPool>>>> = OnceLock::new();

/// Return (creating on first use) a dedicated Rayon pool with at most `cap`
/// worker threads for Phase-1 batch file reads.
fn batch_io_pool(cap: usize) -> Result<Arc<ThreadPool>> {
    let cap = cap.max(1);
    let pools = BATCH_IO_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut pools = pools
        .lock()
        .map_err(|_| anyhow::anyhow!("batch io pool cache lock poisoned"))?;
    Ok(match pools.entry(cap) {
        Entry::Occupied(e) => e.get().clone(),
        Entry::Vacant(e) => {
            let pool = Arc::new(
                ThreadPoolBuilder::new()
                    .num_threads(cap)
                    .build()
                    .map_err(|e| anyhow::anyhow!("failed to build batch io thread pool: {e}"))?,
            );
            e.insert(pool.clone());
            pool
        }
    })
}

struct ExtractedData {
    file_id: String,
    file_name: String,
    file_ext: String,
    dir_id: String,
    file_path_str: String,
    text: String,
    mtime: i64,
    file_size: u64,
    hash: String,
}

impl IndexerService {
    /// Extract text from a single file: read content, compute MD5, dedup
    /// against `content_index`, and run the extraction fallback chain.
    ///
    /// Shared by [`batch_index`](Self::batch_index) Phase 1 and
    /// [`index_file`](Self::index_file).
    fn extract_and_index_single(
        job: &BatchJob,
        conn: &Connection,
    ) -> Result<ExtractedData, (String, String)> {
        let file_name = job
            .file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let file_ext = job
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        const MAX_FILE_SIZE: u64 = 1024 * 1024 * 100;
        const CHUNK_SIZE: u64 = 1024 * 1024;

        let meta = match std::fs::metadata(&job.file_path) {
            Ok(m) => m,
            Err(e) => return Err((job.file_id.clone(), format!("stat: {e}"))),
        };
        let file_size = meta.len();

        let file = match std::fs::File::open(&job.file_path) {
            Ok(f) => f,
            Err(e) => return Err((job.file_id.clone(), format!("open: {e}"))),
        };
        let mut reader = BufReader::new(file);

        let mut raw: Vec<u8>;
        let hash: String;

        if file_size > MAX_FILE_SIZE {
            let mut head = vec![0u8; CHUNK_SIZE as usize];
            if let Err(e) = reader.read_exact(&mut head) {
                return Err((job.file_id.clone(), format!("read head: {e}")));
            }
            if let Err(e) = reader.seek(SeekFrom::End(-(CHUNK_SIZE as i64))) {
                return Err((job.file_id.clone(), format!("seek: {e}")));
            }
            let mut tail = vec![0u8; CHUNK_SIZE as usize];
            if let Err(e) = reader.read_exact(&mut tail) {
                return Err((job.file_id.clone(), format!("read tail: {e}")));
            }
            let mut hasher = md5::Md5::new();
            hasher.update(&head);
            hasher.update(&tail);
            hash = format!("{:x}", hasher.finalize());
            head.extend_from_slice(&tail);
            raw = head;
        } else {
            let mut buf = [0u8; 65536];
            let mut hasher = md5::Md5::new();
            raw = Vec::new();
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => return Err((job.file_id.clone(), format!("read: {e}"))),
                };
                hasher.update(&buf[..n]);
                raw.extend_from_slice(&buf[..n]);
            }
            hash = format!("{:x}", hasher.finalize());
        }

        log::info!("[INDEX] [{}] 开始: {file_name} ({file_ext}, {file_size} B)", job.file_id);

        // Dedup – reuse previously extracted content for the same hash.
        let text = 'dedup: {
            match crate::db::tracker::get_content(conn, &hash) {
                Ok(Some(t)) => {
                    log::info!(
                        "[INDEX] 去重: {file_name} 复用 md5={}& 的已有内容",
                        &hash[..8.min(hash.len())]
                    );
                    break 'dedup t;
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("[INDEX] dedup query failed, falling back to extraction: {e}");
                }
            }
            let mut ocr_used = false;
            let ocr_lang = crate::db::dir_config::get_dir(&conn, &job.dir_id)
                .ok()
                .flatten()
                .and_then(|c| {
                    if c.ocr_lang.is_empty() || c.ocr_lang == "eng" {
                        None // fall through to global default
                    } else {
                        Some(c.ocr_lang)
                    }
                })
                .or_else(|| {
                    conn.query_row(
                        "SELECT value FROM app_settings WHERE key = 'ocr_lang'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
                })
                .unwrap_or_else(|| "eng".to_string());
            let ocr_engine =
                conn.query_row(
                    "SELECT value FROM app_settings WHERE key = 'ocr_engine'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .map(|v| crate::extractor::ocr::map_engine(&v));
            let extracted = match crate::extractor::extract_text(&job.file_path, &ocr_lang, ocr_engine.clone()) {
                Ok(t) if t.len() > 10 => t,
                Ok(t) => {
                    if file_ext.eq_ignore_ascii_case("pdf") {
                        // PDF extraction already runs its own OCR fallback
                        // internally; short text means the text layer is
                        // genuinely minimal — OCR-ing the PDF as an image
                        // would just fail.
                        log::info!("[INDEX] PDF text short ({}), using as-is", t.len());
                        t
                    } else {
                        log::info!("[INDEX] 提取内容过短 ({}), 尝试 OCR 回退", t.len());
                        match crate::extractor::ocr::ocr_image(&job.file_path, "eng", ocr_engine.clone()) {
                            Ok(ocr) if !ocr.is_empty() => {
                                ocr_used = true;
                                ocr
                            }
                            _ => {
                                log::warn!("[INDEX] OCR 回退也失败, 使用原始内容");
                                t
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("[INDEX] 提取失败: {e}, 尝试纯文本回退");
                    let fallback = if file_size <= MAX_FILE_SIZE {
                        std::str::from_utf8(&raw).ok().map(|s| s.to_owned())
                    } else {
                        None
                    };
                    match fallback {
                        Some(t) => {
                            ocr_used = true;
                            t
                        }
                        None => {
                            return Err((
                                job.file_id.clone(),
                                format!("{}: 所有提取方式均失败: {e}", job.file_path.display()),
                            ))
                        }
                    }
                }
            };
            let char_count = extracted.chars().count();
            log::info!("[INDEX] [{}] 提取文字: {file_name} ({char_count} 字符)", job.file_id);
            if let Err(e) =
                crate::db::tracker::store_content(conn, &hash, &extracted, ocr_used, None)
            {
                log::warn!("[INDEX] 存储提取内容失败: {e}");
            }
            // Incrementally count hotwords for ASR/Wu dialect recognition
            crate::db::tracker::update_hotwords(conn, &extracted);
            extracted
        };

        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);

        // CRITICAL: mark_extracted must succeed for Phase 1 progress visibility.
        // If SQLite is busy (parallel writes), retry once.
        if let Err(e) = crate::db::tracker::mark_extracted(conn, &job.file_id, Some(&hash)) {
            log::warn!("[INDEX] mark_extracted retry for {}: {e}", job.file_id);
            if let Err(e2) = crate::db::tracker::mark_extracted(conn, &job.file_id, Some(&hash)) {
                log::warn!("[INDEX] mark_extracted retry FAILED for {}: {e2} (Phase 1 进度将不可见，下次扫描会重试)", job.file_id);
            }
        }

        Ok(ExtractedData {
            file_id: job.file_id.clone(),
            file_name,
            file_ext,
            dir_id: job.dir_id.clone(),
            file_path_str: job.rel_path.clone(),
            text,
            mtime,
            file_size,
            hash,
        })
    }

    /// Create a new indexer service backed by a DB pool and a tantivy index.
    pub fn new(
        db: Pool<SqliteConnectionManager>,
        index_manager: Arc<RwLock<IndexManager>>,
    ) -> Self {
        Self::with_cancel(db, index_manager, Arc::new(AtomicBool::new(false)))
    }

    /// Same as [`new`], but shares the app-wide cancel flag so a cancelled
    /// scan can interrupt an in-flight [`batch_index`](Self::batch_index).
    pub fn with_cancel(
        db: Pool<SqliteConnectionManager>,
        index_manager: Arc<RwLock<IndexManager>>,
        cancel_scan: Arc<AtomicBool>,
    ) -> Self {
        Self {
            db,
            index_manager,
            writer: Mutex::new(None),
            commit_counter: AtomicU64::new(0),
            commit_interval: AtomicU64::new(100),
            batch_io_concurrency: AtomicUsize::new(DEFAULT_BATCH_IO_CONCURRENCY),
            cancel_scan,
        }
    }

    /// Batch-index multiple files.
    ///
    /// Phase 1 (Rayon `par_iter`): read file content, compute MD5, extract text
    /// (CPU/IO-bound, fully parallel).
    /// Phase 2 (serial): lock the Tantivy writer once, add every document, and
    /// update DB tracking.
    pub fn batch_index(
        &self,
        jobs: Vec<BatchJob>,
        progress: impl Fn(u64, u64),
    ) -> Result<Vec<BatchResult>> {
        // Chunked pipeline: instead of extracting ALL files then writing the
        // index once, process in chunks so committed chunks become searchable
        // while later chunks are still extracting (Phase-1 search visibility).
        const CHUNK: usize = 250;
        let db = self.db.clone();
        let total = jobs.len() as u64;
        let started = Instant::now();
        let mut results: Vec<BatchResult> = Vec::with_capacity(total as usize);
        let mut success_count = 0u64;
        let mut error_count = 0u64;
        let mut total_bytes: u64 = 0;
        let mut done = 0u64;

        for chunk in jobs.chunks(CHUNK) {
            if self.cancel_scan.load(Ordering::Acquire) {
                log::info!("[INDEX] 批处理已取消, 跳过剩余 {} 项", total - done);
                break;
            }
            let chunk_total = chunk.len() as u64;
            log::info!(
                "[INDEX] 处理块 {}-{} / {} (并行提取中)",
                done + 1, done + chunk_total, total
            );

            // ── Chunk Phase 1: parallel extraction ─────────────────────
            let chunk_start = Instant::now();
            let pool = batch_io_pool(self.batch_io_concurrency.load(Ordering::Relaxed))?;
            let extracted: Vec<Result<ExtractedData, (String, String)>> = pool.install(|| {
                chunk
                    .par_iter()
                    .map(|job| {
                        if self.cancel_scan.load(Ordering::Acquire) {
                            return Err((job.file_id.clone(), "scan cancelled".to_string()));
                        }
                        let conn = match db.get() {
                            Ok(c) => c,
                            Err(e) => return Err((job.file_id.clone(), format!("DB conn: {e}"))),
                        };
                        Self::extract_and_index_single(job, &conn)
                    })
                    .collect()
            });

            // ── Chunk Phase 2: serial Tantivy write + DB tracking ──────
            let mut guard = self.lock_writer()?;
            let writer = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;
            let conn = self.db.get().context("failed to get DB connection")?;

            for extraction in extracted {
                if self.cancel_scan.load(Ordering::Acquire) {
                    log::info!("[INDEX] 批处理已取消, 跳过剩余项");
                    break;
                }
                done += 1;
                progress(done, total);
                match extraction {
                    Ok(data) => {
                        let file_id = data.file_id;
                        if let Err(e) = Indexer::add_document(
                            writer,
                            &file_id,
                            &data.file_name,
                            &data.file_ext,
                            &data.dir_id,
                            &data.file_path_str,
                            &data.text,
                            data.mtime,
                            data.file_size,
                        ) {
                            let err = format!("add_document: {e}");
                            log::error!("[INDEX] 写入失败: {}: {}", data.file_name, err);
                            let _ = crate::db::tracker::log_index_error(
                                &conn,
                                &file_id,
                                &data.file_path_str,
                                classify_error_str(&err, &data.file_ext),
                                &err,
                            );
                            let _ = crate::db::tracker::mark_failed(&conn, &file_id, &err);
                            results.push(BatchResult {
                                file_id,
                                success: false,
                                error: Some(err),
                            });
                            continue;
                        }

if let Err(e) =
                            crate::db::tracker::update_indexed(&conn, &file_id, Some(&data.hash))
                        {
                            let err = format!("update_indexed: {e}");
                            log::error!("[INDEX] 更新 tracking 失败: {}: {}", data.file_name, err);
                            // 回滚已写入的 Tantivy 文档：记录保持 pending，下轮重试时
                            // 重新 add_document，不回滚则会产生重复文档（每次扫描 +1）。
                            if let Err(rollback) = Indexer::delete_document(writer, &file_id) {
                                log::error!("[INDEX] 回滚文档失败 {}: {rollback}", data.file_name);
                            }
                            results.push(BatchResult {
                                file_id,
                                success: false,
                                error: Some(err),
                            });
                            continue;
                        }

                        log::info!("[INDEX] [{}] 完成: {}", file_id, data.file_name);
                        success_count += 1;
                        total_bytes += data.file_size;
                        results.push(BatchResult {
                            file_id,
                            success: true,
                            error: None,
                        });
                    }
                    Err((file_id, err)) => {
                        error_count += 1;
                        log::error!("[INDEX] 提取失败: {}", err);
                        if err == "scan cancelled" {
                            // User-cancelled jobs are not real failures: leave
                            // the record pending so the next scan retries it.
                            continue;
                        }
                        let etype = classify_error_str(&err, "");
                        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &file_id) {
                            let _ = crate::db::tracker::log_index_error(
                                &conn, &file_id, &rec.path, etype, &err,
                            );
                        }
                        let _ = crate::db::tracker::mark_failed(&conn, &file_id, &err);
                        results.push(BatchResult {
                            file_id,
                            success: false,
                            error: Some(err),
                        });
                    }
                }
            }

            // ── Chunk commit: make this chunk's docs searchable NOW ────
            if success_count > 0 {
                let _ = Indexer::commit(writer);
            }
            drop(conn);
            drop(guard);

            let chunk_elapsed = chunk_start.elapsed().as_secs_f64();
            log::info!(
                "[PERF] 块完成: {}-{} ({} ok, {} err), {:.1}MB, {:.1}s, {:.1} 文件/s — 本块立即可搜",
                done - chunk_total + 1, done,
                chunk_total, error_count,
                total_bytes as f64 / 1_048_576.0,
                chunk_elapsed,
                chunk_total as f64 / chunk_elapsed.max(0.001),
            );
        }

        let total_elapsed = started.elapsed();
        log::info!(
            "[PERF] 批索引总计: {total} 文件, {success_count} 成功 {error_count} 失败, 耗时 {:.1}s ({:.1} 文件/s)",
            total_elapsed.as_secs_f64(),
            total as f64 / total_elapsed.as_secs_f64().max(0.001),
        );

        Ok(results)
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Index a single file: extract content (or reuse deduplicated content),
    /// add to Tantivy, and mark the DB record as indexed.
    ///
    /// * `file_id`   – primary key in `file_tracking`
    /// * `file_path` – absolute path to the file on disk
    /// * `dir_id`    – owning directory config id
    pub fn index_file(
        &self,
        file_id: &str,
        file_path: &Path,
        dir_id: &str,
    ) -> Result<()> {
        if self.cancel_scan.load(Ordering::Acquire) {
            log::info!("[INDEX] 跳过 {file_id}: 扫描已取消");
            return Ok(());
        }
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let file_ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let conn = self.db.get().context("failed to get DB connection")?;

        // Compute relative path for index storage.
        let file_path_str = match crate::db::dir_config::get_dir(&conn, dir_id) {
            Ok(Some(cfg)) => crate::scanner::helpers::to_relative(&cfg.path, file_path)
                .unwrap_or_else(|_| file_path.to_string_lossy().to_string()),
            _ => file_path.to_string_lossy().to_string(),
        };

        let job = BatchJob {
            file_id: file_id.to_string(),
            file_path: file_path.to_path_buf(),
            rel_path: file_path_str.clone(),
            dir_id: dir_id.to_string(),
        };

        let result = (|| -> Result<()> {
            let data = Self::extract_and_index_single(&job, &conn)
                .map_err(|(_, e)| anyhow::anyhow!(e))?;

            let mut guard = self.lock_writer()?;
            let w = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;

            Indexer::add_document(
                w, &data.file_id, &data.file_name, &data.file_ext,
                &data.dir_id, &data.file_path_str, &data.text,
                data.mtime, data.file_size,
            )
            .map_err(|e| anyhow::anyhow!("failed to add document to index: {e}"))?;

            crate::db::tracker::update_indexed(&conn, &data.file_id, Some(&data.hash))
                .context("failed to update indexed status")?;

            log::info!("[INDEX] [{}] 完成: {}", file_id, data.file_name);

            // Periodic auto-commit every N successful files (reuse held writer).
            let count = self.commit_counter.fetch_add(1, Ordering::Relaxed) + 1;
            let interval = self.commit_interval.load(Ordering::Relaxed);
            if interval > 0 && count % interval == 0 {
                if let Err(e) = Indexer::commit(w) {
                    log::error!("[INDEX] 定期提交失败: {e}");
                }
            }

            Ok(())
        })();

        if let Err(ref e) = result {
            let error_type = classify_error(e, &file_ext);
            let _ = crate::db::tracker::log_index_error(&conn, file_id, &file_path.to_string_lossy(), error_type, &e.to_string());
            log::warn!("[INDEX] 失败: {file_name}: {e}");
        }

        result
    }

/// Remove a file from the Tantivy index and mark it as deleted in the DB.
    pub fn delete_file(&self, file_id: &str) -> Result<()> {
        let conn = self.db.get().context("failed to get DB connection")?;

        let mut guard = self.lock_writer()?;
        let w = guard.as_mut().ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;

        Indexer::delete_document(w, file_id)
            .map_err(|e| anyhow::anyhow!("failed to delete document from index: {e}"))?;

        // 文件已从 Tantivy 索引移除，标记为 deleted 使统计准确。
        if let Err(e) = crate::db::tracker::mark_deleted(&conn, file_id) {
            log::warn!("[INDEX] mark_deleted failed {}: {e}", file_id);
        }
        // 同步清理该文件的语义嵌入，避免删除后残留幽灵参与排序。
        if let Err(e) = crate::db::tracker::delete_embedding(&conn, file_id) {
            log::warn!("[INDEX] delete_embedding failed {}: {e}", file_id);
        }

        Ok(())
    }

    /// Remove a Tantivy document without touching file_tracking (used when a
    /// moved file's freshly-walked record is superseded by the old one).
    pub fn delete_document_only(&self, file_id: &str) -> Result<()> {
        let mut guard = self.lock_writer()?;
        let w = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;
        Indexer::delete_document(w, file_id)
            .map_err(|e| anyhow::anyhow!("failed to delete document from index: {e}"))
    }

    /// Remove all Tantivy documents belonging to `dir_id` (used when a
    /// sub-directory is absorbed into its parent). Does NOT touch
    /// file_tracking — those rows were already migrated to the parent.
    pub fn delete_dir(&self, dir_id: &str) -> Result<()> {
        let mut guard = self.lock_writer()?;
        let w = guard.as_mut().ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;
        Indexer::delete_by_dir(w, dir_id)
            .map_err(|e| anyhow::anyhow!("failed to delete dir documents: {e}"))?;
        Ok(())
    }

    /// Commit all pending Tantivy index changes, making them visible to search.
    pub fn commit(&self) -> Result<()> {
        let mut guard = self.lock_writer()?;
        if let Some(w) = guard.as_mut() {
            Indexer::commit(w).map_err(|e| anyhow::anyhow!("failed to commit index writer: {e}"))?;
        }
        Ok(())
    }

    /// Set the number of files indexed between automatic commits.
    /// Pass 0 to disable periodic commits (not recommended).
    pub fn set_commit_interval(&self, n: usize) {
        self.commit_interval.store(n as u64, Ordering::Relaxed);
    }

    /// Set the concurrency cap for Phase-1 batch file reads (read + MD5 +
    /// extraction). Lower caps reduce IO contention and parallel SQLite
    /// writes; the default is [`DEFAULT_BATCH_IO_CONCURRENCY`].
    pub fn set_batch_io_concurrency(&self, n: usize) {
        self.batch_io_concurrency.store(n, Ordering::Relaxed);
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Lock the writer mutex and return the guard.  Creates the writer on
    /// first access.
    fn lock_writer(&self) -> Result<MutexGuard<'_, Option<IndexWriter>>> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("index writer lock poisoned: {e}"))?;

        if guard.is_none() {
            // Hold the lock across creation; releasing it would let two threads each create a writer.
            let mgr = self
                .index_manager
                .read()
                .map_err(|e| anyhow::anyhow!("index manager lock poisoned: {e}"))?;
            let new_w = mgr
                .writer(50_000_000)
                .map_err(|e| anyhow::anyhow!("failed to create index writer: {e}"))?;
            *guard = Some(new_w);
        }
        Ok(guard)
    }

    /// Drop the current writer and release resources. The next write operation
    /// will lazily create a new writer. Used after rebuilding the index.
    pub fn reset_writer(&self) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(mut w) = guard.take() {
                let _ = Indexer::commit(&mut w);
            }
        }
    }
}

fn classify_error_str(msg: &str, _ext: &str) -> &'static str {
    if msg.contains("Permission denied") || msg.contains("Access denied") {
        "access_denied"
    } else if msg.contains("OCR") || msg.contains("tesseract") {
        "ocr_failed"
    } else if msg.contains("timeout") {
        "timeout"
    } else if msg.contains("损坏")
        || msg.contains("加密")
        || msg.contains("无法读取")
        || msg.contains("encrypted")
        || msg.contains("corrupted")
        || msg.contains("password")
    {
        "corrupted_or_protected"
    } else if msg.contains("parse") || msg.contains("invalid") || msg.contains("failed to") {
        "parse_error"
    } else {
        "unknown"
    }
}

fn classify_error(err: &anyhow::Error, ext: &str) -> &'static str {
    classify_error_str(&format!("{err}"), ext)
}

impl Drop for IndexerService {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(w) = guard.as_mut() {
                let _ = Indexer::commit(w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::search::schema::build_schema;
    use tantivy::Index;

    /// Create a clean in-memory DB + tantivy pair and insert a tracking record.
    fn setup() -> (IndexerService, String) {
        let pool: Pool<SqliteConnectionManager> = r2d2::Pool::builder()
            .max_size(2)
            .build(SqliteConnectionManager::memory())
            .unwrap();
        {
            let c = pool.get().unwrap();
            db::run_migrations(&c).unwrap();
            // Seed a dir_config so FK constraints are satisfied.
            c.execute(
                "INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d1','/tmp',1,0,0)",
                [],
            )
            .unwrap();
        }

        let schema = build_schema();
        let index = Index::create_in_ram(schema);
        crate::search::schema::register_tokenizers(&index);
        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));

        let svc = IndexerService::new(pool, im);

        let c = svc.db.get().unwrap();
        let fid = crate::db::tracker::upsert_file(&c, "/tmp/test.txt", "d1", 1000, 42, None).unwrap();
        (svc, fid)
    }

    fn tmp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("idx_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn test_batch_io_pool_limits_concurrency() {
        // Pre-fix, `par_iter` ran on Rayon's global pool (num_cpus threads),
        // blowing past the cap. Now Phase-1 runs on a dedicated pool with at
        // most `cap` worker threads (install joins the caller as ≤1 extra).
        let cap = 2usize;
        let pool = batch_io_pool(cap).unwrap();
        assert_eq!(pool.current_num_threads(), cap);

        let files: Vec<std::path::PathBuf> = (0..16)
            .map(|i| tmp_file(&format!("cap_{i}.txt"), "fixture content for batch io"))
            .collect();

        let active = std::sync::atomic::AtomicUsize::new(0);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        pool.install(|| {
            files.par_iter().for_each(|f| {
                let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                let _ = std::fs::read(f); // real IO like Phase-1 file reads
                std::thread::sleep(std::time::Duration::from_millis(5));
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });
        });

        let peak_val = peak.load(std::sync::atomic::Ordering::SeqCst);
        for f in &files {
            let _ = std::fs::remove_file(f);
        }
        assert!(
            peak_val <= cap + 1,
            "peak concurrent batch reads {peak_val} exceeds cap {cap} (+1 for the caller thread)"
        );
    }

    #[test]
    fn test_index_file_creates_document() {
        let (svc, fid) = setup();
        let path = tmp_file("test_create.txt", "hello world test content");

        svc.index_file(&fid, &path, "d1").unwrap();
        svc.commit().unwrap();

        // Verify Tantivy has the document.
        let mgr = svc.index_manager.read().unwrap(); // nosemgrep: rust-rwlock-read-unwrap
        let reader = mgr.reader().unwrap();
        let searcher = reader.searcher();
        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(mgr.index(), vec![content]);
        let query = parser.parse_query("hello").unwrap();
        let top = searcher
            .search(&query, &tantivy::collector::TopDocs::with_limit(10))
            .unwrap();
        assert_eq!(top.len(), 1, "should find one document");

        // Verify DB tracking was updated.
        let c = svc.db.get().unwrap();
        let rec = crate::db::tracker::get_file_by_id(&c, &fid).unwrap().unwrap();
        assert_eq!(rec.indexed, 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_index_file_dedup_content() {
        let (svc, fid1) = setup();
        let fid2 = {
            let c = svc.db.get().unwrap();
            crate::db::tracker::upsert_file(&c, "/tmp/test2.txt", "d1", 1000, 42, None).unwrap()
        };

        let content = "deduplicated content string";
        let path1 = tmp_file("a.txt", content);
        let path2 = tmp_file("b.txt", content);

        svc.index_file(&fid1, &path1, "d1").unwrap();
        svc.index_file(&fid2, &path2, "d1").unwrap();
        svc.commit().unwrap();

        // Both should be findable.
        let mgr = svc.index_manager.read().unwrap(); // nosemgrep: rust-rwlock-read-unwrap
        let reader = mgr.reader().unwrap();
        let searcher = reader.searcher();
        let schema = build_schema();
        let content_f = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(mgr.index(), vec![content_f]);
        let query = parser.parse_query("deduplicated").unwrap();
        let top = searcher
            .search(&query, &tantivy::collector::TopDocs::with_limit(10))
            .unwrap();
        assert_eq!(top.len(), 2, "both files should match");

        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn test_index_file_handles_missing_file_gracefully() {
        let (svc, fid) = setup();
        let missing = std::path::Path::new("/tmp/nonexistent_file_xyz.txt");

        let result = svc.index_file(&fid, missing, "d1");
        assert!(result.is_err(), "should fail on missing file");

        // DB tracking should remain at indexed=0 (inserted but never updated).
        let c = svc.db.get().unwrap();
        let rec = crate::db::tracker::get_file_by_id(&c, &fid).unwrap().unwrap();
        assert_eq!(rec.indexed, 0, "should not be marked as indexed");
    }

    #[test]
    fn test_delete_file() {
        let (svc, fid) = setup();
        let path = tmp_file("test.txt", "delete test content");

        svc.index_file(&fid, &path, "d1").unwrap();
        svc.commit().unwrap();

        svc.delete_file(&fid).unwrap();
        svc.commit().unwrap();

        // Verify it's gone from Tantivy.
        let mgr = svc.index_manager.read().unwrap(); // nosemgrep: rust-rwlock-read-unwrap
        let reader = mgr.reader().unwrap();
        let searcher = reader.searcher();
        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(mgr.index(), vec![content]);
        let query = parser.parse_query("delete").unwrap();
        let top = searcher
            .search(&query, &tantivy::collector::TopDocs::with_limit(10))
            .unwrap();
        assert_eq!(top.len(), 0, "deleted doc should not be found");

        let _ = std::fs::remove_file(&path);
    }
}
