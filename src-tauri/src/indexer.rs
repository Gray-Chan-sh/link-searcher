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
    writer_buffer_mb: AtomicUsize,
    cancel_scan: Arc<AtomicBool>,
    /// True while a scan/indexing pass is running. Owned here (not just in
    /// `AppState`) so the [`Scanner`](crate::scanner::Scanner) can flip it
    /// around each scan and the embedding keepers can stay alive across long
    /// extraction batches instead of exiting between them.
    is_indexing: Arc<AtomicBool>,
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

/// Default Tantivy writer memory budget (MB) when no setting is present.
const DEFAULT_WRITER_BUFFER_MB: usize = 150;

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
    /// Takes the **pool**, not a borrowed connection: the extraction itself
    /// (which may spend minutes in OCR) must not hold a pooled connection,
    /// otherwise the `batch_io_concurrency` extractor workers starve every
    /// other DB user (UI IPC, Web API, embedding keepers) for the whole batch.
    /// A connection is borrowed only for the short read before extraction and
    /// the short write after it, and returned immediately.
    ///
    /// Shared by [`batch_index`](Self::batch_index) Phase 1 and
    /// [`index_file`](Self::index_file).
    fn extract_and_index_single(
        job: &BatchJob,
        db: &Pool<SqliteConnectionManager>,
        engine_override: Option<crate::extractor::ocr::OcrEngineType>,
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

        // 记录"页派发被内存压力打断"的世代号：提取结束时若变了，说明这份内容只拿到
        // 一部分页（见下方 truncated_since 的判定）。
        let epoch0 = crate::load::pause_epoch();

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

        // ── 阶段 A：提取前的 DB 读取（借用连接，读完立刻归还）────────────
        //
        // 「去重命中」+「OCR 语言/引擎」是提取所需的全部 DB 输入，一次取完即
        // 可释放连接。这样下面的提取（可能数分钟 OCR）**不持有任何连接**，
        // 提取 worker 不再长期占用连接池。
        let (dedup_hit, ocr_lang, ocr_engine) = {
            let conn = db
                .get()
                .map_err(|e| (job.file_id.clone(), format!("DB conn: {e}")))?;

            // Dedup – reuse previously extracted content for the same hash.
            let dedup_hit = match crate::db::tracker::get_content(&conn, &hash) {
                Ok(Some(t)) => Some(t),
                Ok(None) => None,
                Err(e) => {
                    log::warn!("[INDEX] dedup query failed, falling back to extraction: {e}");
                    None
                }
            };

            // 去重命中时不会走提取，语言/引擎用不上（回传占位值）。
            let ocr_lang = if dedup_hit.is_some() {
                String::new()
            } else {
                crate::db::dir_config::get_dir(&conn, &job.dir_id)
                    .ok()
                    .flatten()
                    .and_then(|c| {
                        if c.ocr_lang.is_empty() || c.ocr_lang == "eng" {
                            None
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
                    .unwrap_or_else(|| "eng".to_string())
            };

            let ocr_engine = engine_override.clone().or_else(|| {
                conn.query_row(
                    "SELECT value FROM app_settings WHERE key = 'ocr_engine'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .ok()
                .map(|v| crate::extractor::ocr::map_engine(&v))
            });

            (dedup_hit, ocr_lang, ocr_engine)
        }; // ← 连接在此归还；下面整段提取不再持有连接

        // ── 阶段 B：提取（零 DB 连接）────────────────────────────────
        let text = if let Some(t) = dedup_hit {
            log::info!(
                "[INDEX] 去重: {file_name} 复用 md5={}& 的已有内容",
                &hash[..8.min(hash.len())]
            );
            t
        } else {
            let mut ocr_used = false;
            let extracted = match crate::extractor::extract_text_with_meta(
                &job.file_path,
                &ocr_lang,
                ocr_engine.clone(),
            ) {
                Ok((t, meta)) if t.len() > 10 => (t, meta),
                Ok((t, meta)) => {
                    if file_ext.eq_ignore_ascii_case("pdf") {
                        // A PDF that yields almost no text is not a normal state —
                        // surface it (quality flags will mark it LowPrintable/LowDensity)
                        // instead of silently accepting an empty body.
                        log::warn!(
                            "[INDEX] PDF text short ({} chars) — 可能未成功恢复正文 (file: {file_name})",
                            t.len()
                        );
                        (t, meta)
                    } else {
                        log::info!("[INDEX] 提取内容过短 ({}), 尝试 OCR 回退", t.len());
                        match crate::extractor::ocr::ocr_image(&job.file_path, &ocr_lang, ocr_engine.clone()) {
                            Ok(ocr) if !ocr.is_empty() => {
                                ocr_used = true;
                                let mut m = meta;
                                m.ocr_used = true;
                                (ocr, m)
                            }
                            _ => {
                                log::warn!("[INDEX] OCR 回退也失败, 使用原始内容");
                                (t, meta)
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("[INDEX] 提取失败: {e}, 尝试纯文本回退");
                    let fallback = if file_size <= MAX_FILE_SIZE {
                        std::str::from_utf8(&raw).ok().map(crate::extractor::sanitize_text)
                    } else {
                        None
                    };
                    match fallback {
                        Some(t) if is_plausible_text_fallback(&t, &file_ext) => {
                            (t, crate::extractor::quality::ExtractMeta::default())
                        }
                        Some(_) => {
                            log::warn!(
                                "[INDEX] 纯文本回退不可信 (文件: {file_name}), 标记为失败"
                            );
                            return Err((
                                job.file_id.clone(),
                                format!("{}: 所有提取方式均失败: {e}", job.file_path.display()),
                            ));
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
            // 内存压力把页派发打断时，这份内容只是"一部分页"：**不能**当成成功落库
            // （那就是静默丢正文），直接判失败让扫描稍后重试。
            if crate::load::truncated_since(epoch0) {
                log::warn!("[LOAD] {file_name}: 内存压力暂停，本次提取只完成部分页 → 判失败稍后重试");
                return Err((
                    job.file_id.clone(),
                    format!(
                        "内存压力暂停（自适应降级）: {file_name} 仅完成部分页，稍后重试"
                    ),
                ));
            }
            let char_count = extracted.0.chars().count();
            log::info!("[INDEX] [{}] 提取文字: {file_name} ({char_count} 字符)", job.file_id);
            let mut qmeta = extracted.1.clone();
            qmeta.file_size = Some(file_size);
            let quality =
                crate::extractor::quality::compute_quality(&extracted.0, &qmeta, &file_ext);
            // 落库的 ocr_used 必须取提取器返回的 meta（PDF 扫描件的 OCR 是在
            // pdf 模块内部完成的，本地变量 ocr_used 只覆盖"图片短文本回退"那
            // 一条路），否则 content_index.ocr_used 对 PDF 永远是 0，质量审计
            // 与"哪些文件是 OCR 出来的"统计全部失真。
            let ocr_used = extracted.1.ocr_used || ocr_used;
            // ── 阶段 C（写）：取一条短连接落库，写完立刻归还 ──────────────
            match db.get() {
                Ok(conn) => {
                    if retry_busy("store_content", &job.file_id, || {
                        crate::db::tracker::store_content_with_quality(
                            &conn,
                            &hash,
                            &extracted.0,
                            ocr_used,
                            None,
                            Some(&quality),
                        )
                    })
                    .is_none()
                    {
                        log::warn!(
                            "[INDEX] {}: 内容未落库（quality 将丢失，backfill 可补回）",
                            job.file_id
                        );
                    }
                }
                Err(e) => {
                    // 内容暂未落库：本次判失败，下次扫描命中去重后补写。
                    log::warn!("[INDEX] {}: 取连接失败，内容未落库: {e}", job.file_id);
                }
            }
            extracted.0
        };

        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);

        // ── 阶段 C（写）：chunk 存储 + mark_extracted（短连接）────────────
        let conn = db
            .get()
            .map_err(|e| (job.file_id.clone(), format!("DB conn: {e}")))?;

        // Long-doc chunk storage for RAG retrieval (content-keyed by md5);
        // dedup reuse keeps existing chunks, below-threshold clears stale ones.
        if text.chars().count() > crate::db::chunks::CHUNK_THRESHOLD {
            match crate::db::chunks::get_chunks(&conn, &hash) {
                Ok(existing) if existing.is_empty() => {
                    let chunks = crate::db::chunks::chunk_text(&text);
                    if retry_busy("replace_chunks", &job.file_id, || {
                        crate::db::chunks::replace_chunks(&conn, &hash, &chunks)
                    })
                    .is_none()
                    {
                        log::warn!("[INDEX] {}: doc_chunks 未写入", job.file_id);
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("[INDEX] doc_chunks read failed {}: {e}", job.file_id),
            }
        } else if let Err(e) = crate::db::chunks::delete_chunks(&conn, &hash) {
            log::warn!("[INDEX] doc_chunks cleanup failed {}: {e}", job.file_id);
        }

        // CRITICAL: mark_extracted must succeed for Phase 1 progress visibility.
        if retry_busy("mark_extracted", &job.file_id, || {
            crate::db::tracker::mark_extracted(&conn, &job.file_id, Some(&hash))
        })
        .is_none()
        {
            log::warn!(
                "[INDEX] {}: Phase 1 进度将不可见（下次扫描会重试）",
                job.file_id
            );
        }
        drop(conn);

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
            writer_buffer_mb: AtomicUsize::new(DEFAULT_WRITER_BUFFER_MB),
            cancel_scan,
            is_indexing: Arc::new(AtomicBool::new(false)),
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
        const CHUNK: usize = 500;
        let db = self.db.clone();
        let total = jobs.len() as u64;
        let started = Instant::now();
        let mut results: Vec<BatchResult> = Vec::with_capacity(total as usize);
        let mut success_count = 0u64;
        let mut error_count = 0u64;
        let total_bytes: u64 = 0;
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
            let ws_before_chunk = crate::load::sample().process_ws_mb;
            let pool = batch_io_pool(self.batch_io_concurrency.load(Ordering::Relaxed))?;
            let extracted: Vec<Result<ExtractedData, (String, String)>> = pool.install(|| {
                chunk
                    .par_iter()
                    .map(|job| {
                        if self.cancel_scan.load(Ordering::Acquire) {
                            return Err((job.file_id.clone(), "scan cancelled".to_string()));
                        }
                        // 自适应并发闸门：限制同时展开的文档数（每篇 PDF 在内存里持有
                        // 对象图 + 页位图）。必须在任何取连接**之前**——限流/暂停时若
                        // 已持有连接去等待，会把连接池占满（实测过 55 次
                        // "DB conn: timed out waiting for connection" 失败）。
                        let _doc_permit = crate::load::acquire_doc();
                        // 连接由 extract_and_index_single 内部按需短持有：这里不再
                        // 跨整个提取期占着池连接。
                        Self::extract_and_index_single(job, &db, None)
                    })
                    .collect()
            });

            // ── 批屏障观测 ────────────────────────────────────────────
            // 为"Phase-1 结果驻留到 Phase 2 才释放 ≈ 18GB"这一**未经证实**的
            // 归因取证：同时打出「本块驻留的提取文本量」与「进程 WS」。
            // 若文本量远小于 WS，则峰值来自提取期页位图（由 load.rs 的并发闸门
            // 管辖），而不是本屏障 —— 据此再决定要不要改分批落库。
            let extracted_len = extracted.len();
            let retained_text_mb: f64 = extracted
                .iter()
                .filter_map(|r| r.as_ref().ok())
                .map(|d| d.text.len() as f64)
                .sum::<f64>()
                / 1_048_576.0;
            let ws_at_barrier = crate::load::sample().process_ws_mb;

            // ── Chunk Phase 2: serial Tantivy write + DB tracking ──────
            let mut guard = self.lock_writer()?;
            let writer = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;
            let conn = self.db.get().context("failed to get DB connection")?;
            let mut pending_idx: Vec<(String, String)> = Vec::with_capacity(chunk.len());

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
                            &data.hash,
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
                        pending_idx.push((file_id, data.hash));
                    }
                    Err((file_id, err)) => {
                        error_count += 1;
                        log::error!("[INDEX] 提取失败: {}", err);
                        if err == "scan cancelled" {
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

            // Batch all tracking updates in one transaction.
            if !pending_idx.is_empty() {
                match crate::db::tracker::update_indexed_batch(&conn, &pending_idx) {
                    Ok(()) => {
                        for (file_id, _) in &pending_idx {
                            results.push(BatchResult { file_id: file_id.clone(), success: true, error: None });
                        }
                        success_count += pending_idx.len() as u64;
                    }
                    Err(e) => {
                        let err = format!("update_indexed_batch: {e}");
                        log::error!("[INDEX] 批量更新 tracking 失败: {err}");
                        for (file_id, _) in &pending_idx {
                            if let Err(rollback) = Indexer::delete_document(writer, file_id) {
                                log::error!("[INDEX] 回滚文档失败 {}: {rollback}", file_id);
                            }
                            results.push(BatchResult { file_id: file_id.clone(), success: false, error: Some(err.clone()) });
                        }
                    }
                }
            }

            // ── Chunk commit: make this chunk's docs searchable NOW ────
            if success_count > 0 {
                let _ = Indexer::commit(writer);
            }
            drop(conn);
            drop(guard);

            // 批屏障观测：Phase 2 落库后的 WS（对比 Phase-1 结束时的驻留文本量）。
            let ws_after_flush = crate::load::sample().process_ws_mb;
            log::info!(
                "[PERF] 批屏障: 本块 {extracted_len} 份提取结果驻留 {retained_text_mb:.1}MB 文本; WS {ws_before_chunk}MB(块前) → {ws_at_barrier}MB(提取完) → {ws_after_flush}MB(落库后)"
            );

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

        crate::commands::index::ensure_embedding_keepers(&self.db, self.is_indexing.clone());

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
    /// 内容变更时清理该文件的旧语义向量。
    ///
    /// 向量回填是"只补缺失"语义（`missing_embedding_rows` 只选没有行的文件），
    /// 而 doc 向量按 `file_id` 键、其不随内容变化 —— 不清旧向量就会永远按
    /// 旧内容参与语义排序（搜新话题搜不到、搜旧话题却命中）。
    /// 清掉后由扫描结束时的 `run_backfill_embeddings` 重建。
    fn clear_stale_embeddings(conn: &Connection, file_id: &str, new_hash: &str) {
        let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(conn, file_id) else {
            return;
        };
        let Some(old_md5) = rec.md5.as_deref() else {
            return;
        };
        if old_md5 == new_hash {
            return;
        }
        if let Err(e) = crate::db::tracker::delete_embedding(conn, file_id) {
            log::warn!("[INDEX] 内容变更清理旧 doc 向量失败 {file_id}: {e}");
        }
        if let Err(e) = crate::db::tracker::delete_chunk_embeddings(conn, old_md5) {
            log::warn!("[INDEX] 内容变更清理旧 chunk 向量失败 {old_md5}: {e}");
        }
    }

    pub fn index_file(
        &self,
        file_id: &str,
        file_path: &Path,
        dir_id: &str,
        engine_override: Option<crate::extractor::ocr::OcrEngineType>,
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

        // 自适应并发闸门（与批处理同一把）：先拿许可，再取 DB 连接。
        let _doc_permit = crate::load::acquire_doc();

        // Compute relative path for index storage（短连接读取，读完立刻归还）。
        let file_path_str = {
            let conn = self.db.get().context("failed to get DB connection")?;
            match crate::db::dir_config::get_dir(&conn, dir_id) {
                Ok(Some(cfg)) => crate::scanner::helpers::to_relative(&cfg.path, file_path)
                    .unwrap_or_else(|_| {
                        crate::scanner::helpers::normalize_os_path(&file_path.to_string_lossy())
                    }),
                _ => crate::scanner::helpers::normalize_os_path(&file_path.to_string_lossy()),
            }
        };

        let job = BatchJob {
            file_id: file_id.to_string(),
            file_path: file_path.to_path_buf(),
            rel_path: file_path_str.clone(),
            dir_id: dir_id.to_string(),
        };

        let result = (|| -> Result<()> {
            // 提取期不持有连接（见 `extract_and_index_single` 的文档）。
            let data = Self::extract_and_index_single(&job, &self.db, engine_override)
                .map_err(|(_, e)| anyhow::anyhow!(e))?;

            {
                let conn = self.db.get().context("failed to get DB connection")?;
                Self::clear_stale_embeddings(&conn, file_id, &data.hash);
            }

            let mut guard = self.lock_writer()?;
            let w = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;

            Indexer::add_document(
                w, &data.file_id, &data.file_name, &data.file_ext,
                &data.dir_id, &data.file_path_str, &data.text,
                &data.hash, data.mtime, data.file_size,
            )
            .map_err(|e| anyhow::anyhow!("failed to add document to index: {e}"))?;

            {
                let conn = self.db.get().context("failed to get DB connection")?;
                crate::db::tracker::update_indexed(&conn, &data.file_id, Some(&data.hash))
                    .context("failed to update indexed status")?;
            }

            log::info!("[INDEX] [{}] 完成: {}", file_id, data.file_name);

            self.ensure_embedding_keepers();

            // Periodic auto-commit every N successful files (reuse held writer).
            let count = self.commit_counter.fetch_add(1, Ordering::Relaxed) + 1;
            let interval = self.commit_interval.load(Ordering::Relaxed);
            if interval > 0 && count.is_multiple_of(interval)
                && let Err(e) = Indexer::commit(w) {
                    log::error!("[INDEX] 定期提交失败: {e}");
                }

            Ok(())
        })();

        if let Err(ref e) = result {
            let error_type = classify_error(e, &file_ext);
            if let Ok(conn) = self.db.get() {
                let _ = crate::db::tracker::log_index_error(&conn, file_id, &file_path.to_string_lossy(), error_type, &e.to_string());
            }
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
        // 同步清理该文件的 chunk 级嵌入。块向量按 md5 键、内容在**目录间共享**：
        // 只有当没有其他活跃文件仍引用该 md5 时才删，否则会连带清掉别的目录里
        // 相同内容文件的块向量（曾按 md5 无条件删，属缺陷）。
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, file_id)
            && let Some(md5) = &rec.md5
        {
            match crate::db::tracker::count_other_active_files_with_md5(&conn, md5, file_id) {
                Ok(0) => {
                    if let Err(e) = crate::db::tracker::delete_chunk_embeddings(&conn, md5) {
                        log::warn!("[INDEX] delete_chunk_embeddings failed {md5}: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("[INDEX] md5 reference check failed {md5}: {e}"),
            }
        }

        Ok(())
    }

    /// Purge **all data** belonging to `dir_id`: Tantivy documents, `file_tracking`
    /// rows, and the per-file vectors / summaries / index errors.
    ///
    /// Content-keyed rows (`content_index`, `doc_chunks`, `chunk_embeddings`) are
    /// removed only when their md5 is no longer referenced by any tracked file, so
    /// content shared with another directory survives (content-based dedup).
    ///
    /// `deep` additionally clears the directory's unsupported-extension stats.
    /// `dir_config` is left untouched — the caller owns that.
    ///
    /// Both directory-removal paths go through here so a re-added directory is
    /// treated as brand-new data (no stale rows, no ghost search hits).
    pub fn purge_dir_data(&self, dir_id: &str, deep: bool) -> Result<()> {
        let conn = self.db.get().context("failed to get DB connection")?;

        // Tantivy: one term delete for the whole directory (committed below).
        self.delete_dir(dir_id)?;

        // Per-`file_id` rows must be cleared *before* file_tracking disappears.
        for sql in [
            "DELETE FROM doc_embeddings WHERE file_id IN (SELECT id FROM file_tracking WHERE dir_id=?1)",
            "DELETE FROM doc_summaries  WHERE file_id IN (SELECT id FROM file_tracking WHERE dir_id=?1)",
            "DELETE FROM index_errors   WHERE file_id IN (SELECT id FROM file_tracking WHERE dir_id=?1)",
        ] {
            if let Err(e) = conn.execute(sql, [dir_id]) {
                log::warn!("[INDEX] purge_dir_data: {e}");
            }
        }
        conn.execute("DELETE FROM file_tracking WHERE dir_id=?1", [dir_id])
            .context("purge_dir_data: delete file_tracking failed")?;
        if deep
            && let Err(e) = conn.execute("DELETE FROM unsupported_ext_stats WHERE dir_id=?1", [dir_id])
        {
            log::warn!("[INDEX] purge_dir_data: unsupported_ext_stats: {e}");
        }

        // Content-keyed cleanup: only rows whose md5 no longer has any owner.
        if let Err(e) = crate::db::cleanup_orphan_content(&conn) {
            log::warn!("[INDEX] purge_dir_data: cleanup_orphan_content: {e}");
        }
        if let Err(e) = crate::db::cleanup_orphan_chunk_embeddings(&conn) {
            log::warn!("[INDEX] purge_dir_data: cleanup_orphan_chunk_embeddings: {e}");
        }

        drop(conn);
        self.commit().context("failed to commit after dir purge")?;
        Ok(())
    }

    /// Remove data left behind by directories that are no longer configured.
    ///
    /// A scan's cancellation is cooperative, so while a directory is being
    /// removed the in-flight scan can still `upsert` its rows and write Tantivy
    /// documents for a `dir_id` whose `dir_config` row is already gone. This
    /// sweeps every such orphan `dir_id` (plus per-`file_id` vectors and
    /// content-keyed rows left without an owner) so a removed directory leaves
    /// no ghost search hits.
    ///
    /// Returns the number of orphan `dir_id`s swept.
    pub fn sweep_orphan_dirs(&self) -> Result<u64> {
        let conn = self.db.get().context("failed to get DB connection")?;

        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT ft.dir_id FROM file_tracking ft \
                     LEFT JOIN dir_config dc ON dc.id = ft.dir_id \
                     WHERE dc.id IS NULL",
                )
                .context("prepare orphan dir ids")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .context("query orphan dir ids")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("collect orphan dir ids")?
        };

        for dir_id in &ids {
            if let Err(e) = self.delete_dir(dir_id) {
                log::warn!("[INDEX] sweep: 删除 {dir_id} 的 Tantivy 文档失败: {e}");
            }
            for sql in [
                "DELETE FROM file_tracking WHERE dir_id=?1",
                "DELETE FROM unsupported_ext_stats WHERE dir_id=?1",
            ] {
                if let Err(e) = conn.execute(sql, [dir_id.as_str()]) {
                    log::warn!("[INDEX] sweep: {dir_id}: {e}");
                }
            }
        }

        // Per-`file_id` rows whose owner row is gone (the dir deletes above
        // removed them), then the md5-keyed content that lost its last owner.
        if let Err(e) = crate::db::cleanup_orphan_file_rows(&conn) {
            log::warn!("[INDEX] sweep: cleanup_orphan_file_rows: {e}");
        }
        if let Err(e) = crate::db::cleanup_orphan_content(&conn) {
            log::warn!("[INDEX] sweep: cleanup_orphan_content: {e}");
        }
        if let Err(e) = crate::db::cleanup_orphan_chunk_embeddings(&conn) {
            log::warn!("[INDEX] sweep: cleanup_orphan_chunk_embeddings: {e}");
        }

        drop(conn);
        self.commit().context("failed to commit after orphan sweep")?;
        if !ids.is_empty() {
            log::info!("[INDEX] sweep: 清理了 {} 个已移除目录的残留数据", ids.len());
        }
        Ok(ids.len() as u64)
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

    /// Force an immediate commit of pending writes. Used by the integrity heal
    /// so the final batch of re-indexed files becomes searchable right away
    /// instead of waiting for the periodic auto-commit.
    pub fn commit_now(&self) -> Result<()> {
        let mut guard = self.lock_writer()?;
        let w = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;
        Indexer::commit(w).map_err(|e| anyhow::anyhow!("commit failed: {e}"))
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

    /// Current Phase-1 concurrency cap (the value actually in effect).
    pub fn batch_io_concurrency(&self) -> usize {
        self.batch_io_concurrency.load(Ordering::Relaxed)
    }

    /// Current commit interval (files between automatic Tantivy commits).
    pub fn commit_interval(&self) -> usize {
        self.commit_interval.load(Ordering::Relaxed) as usize
    }

    /// Set the Tantivy writer memory budget (MB). Takes effect on the next
    /// writer (re)creation — call [`Self::reset_writer`] to force it now.
    pub fn set_writer_buffer_mb(&self, mb: usize) {
        self.writer_buffer_mb.store(mb, Ordering::Relaxed);
    }

    /// Current Tantivy writer memory budget (MB).
    pub fn writer_buffer_mb(&self) -> usize {
        self.writer_buffer_mb.load(Ordering::Relaxed)
    }

    /// Whether a scan/indexing pass is currently running (see [`Self::is_indexing`]).
    pub fn is_indexing(&self) -> bool {
        self.is_indexing.load(Ordering::Acquire)
    }

    /// Set the indexing-in-progress flag. Used by the scanner (via
    /// `IndexingScope`) around each scan.
    pub fn set_indexing(&self, value: bool) {
        self.is_indexing.store(value, Ordering::Release);
    }

    /// Shared handle to the indexing flag, for the embedding keepers.
    pub fn is_indexing_flag(&self) -> Arc<AtomicBool> {
        self.is_indexing.clone()
    }

    /// Kick the doc/chunk embedding keepers so extraction and embedding overlap
    /// instead of running back-to-back. Idempotent — a no-op while keepers are
    /// already alive.
    pub fn ensure_embedding_keepers(&self) {
        crate::commands::index::ensure_embedding_keepers(&self.db, self.is_indexing.clone());
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
                .writer(self.writer_buffer_mb.load(Ordering::Relaxed).max(1) * 1_048_576)
                .map_err(|e| anyhow::anyhow!("failed to create index writer: {e}"))?;
            *guard = Some(new_w);
        }
        Ok(guard)
    }

    /// Drop the current writer and release resources. The next write operation
    /// will lazily create a new writer. Used after rebuilding the index.
    pub fn reset_writer(&self) {
        if let Ok(mut guard) = self.writer.lock()
            && let Some(mut w) = guard.take() {
                let _ = Indexer::commit(&mut w);
            }
    }
}

/// Retry a DB write that races with the other parallel writers (the batch
/// extractor's 8–14 threads plus the embedding keepers).
///
/// A single *immediate* retry just hits the same `SQLITE_BUSY`: measured 7/7
/// `mark_extracted` retries and 15/22 `store_content` retries failing while a
/// scan overlapped the chunk keeper. Back off briefly between attempts instead.
///
/// Returns `None` when every attempt failed; the caller logs the consequence.
fn retry_busy<T>(what: &str, file_id: &str, mut op: impl FnMut() -> Result<T>) -> Option<T> {
    const ATTEMPTS: u32 = 3;
    let mut delay = std::time::Duration::from_millis(50);
    for attempt in 1..=ATTEMPTS {
        match op() {
            Ok(v) => return Some(v),
            Err(e) if attempt == ATTEMPTS => {
                log::warn!("[INDEX] {what} 连续 {ATTEMPTS} 次失败 {file_id}: {e}");
            }
            Err(e) => {
                log::warn!("[INDEX] {what} 重试 {attempt}/{ATTEMPTS} {file_id}: {e}");
                std::thread::sleep(delay);
                delay *= 4;
            }
        }
    }
    None
}

fn is_plausible_text_fallback(text: &str, file_ext: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    if text.contains('\0') {
        return false;
    }
    let quality = crate::extractor::quality::compute_quality(
        text,
        &crate::extractor::quality::ExtractMeta::default(),
        file_ext,
    );
    !quality.flags.contains(&crate::extractor::quality::QualityFlag::LowPrintable)
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
        if let Ok(mut guard) = self.writer.lock()
            && let Some(w) = guard.as_mut() {
                let _ = Indexer::commit(w);
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::search::schema::build_schema;
    use tantivy::Index;

    /// Create a clean DB + tantivy pair and insert a tracking record.
    ///
    /// ⚠️ 用**临时文件库**而不是 `SqliteConnectionManager::memory()`：
    /// 内存 SQLite 是**每个连接一个独立库**，而这里用的是连接池 —— 池一旦发出第二个
    /// 连接（并行跑测试时就会），那个连接看到的库里**没有表**，于是随执行时序偶发
    /// `upsert_file failed` / `delete file_tracking failed`（2026-10-15 实测连跑 4 轮红 2 轮）。
    fn setup() -> (IndexerService, String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("idx_pool_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool: Pool<SqliteConnectionManager> = r2d2::Pool::builder()
            .max_size(4)
            .build(SqliteConnectionManager::file(dir.join("data.db")))
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
        (svc, fid, dir)
    }

    #[test]
    fn perf_params_set_and_get_roundtrip() {
        let (svc, _, _dir) = setup();
        assert_eq!(svc.batch_io_concurrency(), DEFAULT_BATCH_IO_CONCURRENCY);
        assert_eq!(svc.writer_buffer_mb(), DEFAULT_WRITER_BUFFER_MB);
        assert_eq!(svc.commit_interval(), 100);

        svc.set_batch_io_concurrency(14);
        svc.set_commit_interval(2000);
        svc.set_writer_buffer_mb(300);

        // Getters must reflect what a tier change would actually apply.
        assert_eq!(svc.batch_io_concurrency(), 14);
        assert_eq!(svc.commit_interval(), 2000);
        assert_eq!(svc.writer_buffer_mb(), 300);
    }

    fn tmp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("idx_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn test_content_change_clears_stale_embeddings() {
        let (svc, fid, _dir) = setup();
        let c = svc.db.get().unwrap();

        crate::db::tracker::update_indexed(&c, &fid, Some("oldhash")).unwrap();
        crate::db::tracker::upsert_embedding(&c, &fid, &[1.0, 2.0, 3.0]).unwrap();
        crate::db::tracker::upsert_chunk_embedding(&c, "oldhash", 0, &[1.0, 2.0]).unwrap();

        IndexerService::clear_stale_embeddings(&c, &fid, "oldhash");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM doc_embeddings WHERE file_id=?1", [&fid], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "内容未变时不应清理向量");

        IndexerService::clear_stale_embeddings(&c, &fid, "newhash");
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM doc_embeddings WHERE file_id=?1", [&fid], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "内容变更后应清理 doc 向量（否则永远按旧内容参与排序）");
        let m: i64 = c
            .query_row("SELECT COUNT(*) FROM chunk_embeddings WHERE md5='oldhash'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(m, 0, "内容变更后应清理旧 md5 的 chunk 向量");
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
        let (svc, fid, _dir) = setup();
        let path = tmp_file("test_create.txt", "hello world test content");

        svc.index_file(&fid, &path, "d1", None).unwrap();
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
        let (svc, fid1, _dir) = setup();
        let fid2 = {
            let c = svc.db.get().unwrap();
            crate::db::tracker::upsert_file(&c, "/tmp/test2.txt", "d1", 1000, 42, None).unwrap()
        };

        let content = "deduplicated content string";
        let path1 = tmp_file("a.txt", content);
        let path2 = tmp_file("b.txt", content);

        svc.index_file(&fid1, &path1, "d1", None).unwrap();
        svc.index_file(&fid2, &path2, "d1", None).unwrap();
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
        let (svc, fid, _dir) = setup();
        let missing = std::path::Path::new("/tmp/nonexistent_file_xyz.txt");

        let result = svc.index_file(&fid, missing, "d1", None);
        assert!(result.is_err(), "should fail on missing file");

        // DB tracking should remain at indexed=0 (inserted but never updated).
        let c = svc.db.get().unwrap();
        let rec = crate::db::tracker::get_file_by_id(&c, &fid).unwrap().unwrap();
        assert_eq!(rec.indexed, 0, "should not be marked as indexed");
    }

    /// 删除目录要能"彻底清空该目录的数据"，同时**保留与其他目录共享的内容**：
    /// Tantivy 文档、file_tracking、doc 向量/摘要/错误按 file_id 清；md5 键的
    /// content/chunks/块向量只在无人引用时才清。重加该目录即全新处理。
    #[test]
    fn purge_dir_data_clears_dir_but_keeps_shared_content() {
        let (svc, fid, _dir) = setup(); // dir 'd1' + fid(/tmp/test.txt)
        let path = tmp_file("purge_a.txt", "purge dir data shared content marker");
        svc.index_file(&fid, &path, "d1", None).unwrap();
        svc.commit().unwrap();

        let shared_md5 = {
            let conn = svc.db.get().unwrap();
            crate::db::tracker::get_file_by_id(&conn, &fid).unwrap().unwrap().md5.unwrap()
        };

        {
            let conn = svc.db.get().unwrap();
            conn.execute(
                "INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d2','/tmp2',1,0,0)",
                [],
            )
            .unwrap();
            // d2 里放一份**相同内容**（共享 md5），另在 d1 放一份独占内容。
            let fid_shared = crate::db::tracker::upsert_file(&conn, "/tmp2/dup.txt", "d2", 1000, 42, Some(&shared_md5)).unwrap();
            let fid_only = crate::db::tracker::upsert_file(&conn, "/tmp/only.txt", "d1", 1000, 42, Some("only1")).unwrap();
            for f in [&fid, &fid_shared, &fid_only] {
                crate::db::tracker::upsert_embedding(&conn, f, &[1.0, 2.0]).unwrap();
            }
            let md5s = [shared_md5.clone(), "only1".to_string()];
            for md5 in &md5s {
                crate::db::tracker::upsert_chunk_embedding(&conn, md5, 0, &[1.0]).unwrap();
            }
            crate::db::tracker::store_content(&conn, &shared_md5, "shared body", false, None).unwrap();
            crate::db::tracker::store_content(&conn, "only1", "only body", false, None).unwrap();
            conn.execute(
                "INSERT INTO doc_summaries (file_id,summary,updated_at) VALUES (?1,'s',0)",
                [&fid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO unsupported_ext_stats (ext,count,dir_id,updated_at) VALUES ('xyz',1,'d1',0)",
                [],
            )
            .unwrap();
        }

        svc.purge_dir_data("d1", true).unwrap();

        let conn = svc.db.get().unwrap();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM file_tracking WHERE dir_id='d1'"), 0, "d1 的追踪行应清空");
        assert_eq!(count("SELECT COUNT(*) FROM file_tracking WHERE dir_id='d2'"), 1, "d2 不受影响");
        assert_eq!(count(&format!("SELECT COUNT(*) FROM doc_embeddings WHERE file_id='{fid}'")), 0, "d1 的 doc 向量应清空");
        assert_eq!(count("SELECT COUNT(*) FROM doc_summaries"), 0, "摘要应按 file_id 清空");
        assert_eq!(count("SELECT COUNT(*) FROM content_index WHERE md5='only1'"), 0, "独占内容应删除");
        assert_eq!(count(&format!("SELECT COUNT(*) FROM content_index WHERE md5='{shared_md5}'")), 1, "共享内容必须保留");
        assert_eq!(count("SELECT COUNT(*) FROM chunk_embeddings WHERE md5='only1'"), 0, "独占内容的块向量应删除");
        assert_eq!(count(&format!("SELECT COUNT(*) FROM chunk_embeddings WHERE md5='{shared_md5}'")), 1, "共享内容的块向量必须保留");
        assert_eq!(count("SELECT COUNT(*) FROM unsupported_ext_stats WHERE dir_id='d1'"), 0, "purge 应清扩展名统计");

        // Tantivy：该目录的文档应不再可搜。
        svc.commit().unwrap();
        let mgr = svc.index_manager.read().unwrap(); // nosemgrep: rust-rwlock-read-unwrap
        let reader = mgr.reader().unwrap();
        let searcher = reader.searcher();
        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(mgr.index(), vec![content]);
        let top = searcher
            .search(&parser.parse_query("marker").unwrap(), &tantivy::collector::TopDocs::with_limit(10))
            .unwrap();
        assert_eq!(top.len(), 0, "purge 后该目录文档不应再被搜到（否则是幽灵命中）");

        let _ = std::fs::remove_file(&path);
    }

    /// 删除目录时扫描可能还在写（取消是合作式的），取消生效前写进来的行会留下
    /// "孤儿 dir_id"（dir_config 里已不存在）。sweep_orphan_dirs 必须把它们连同
    /// 行/文档/向量一起清掉，且**不动**仍在配置里的目录。
    #[test]
    fn sweep_orphan_dirs_removes_leftovers_but_keeps_configured_dirs() {
        let (svc, fid, _dir) = setup(); // 'd1' + fid
        {
            let conn = svc.db.get().unwrap();
            // 模拟"扫描仍在跑、目录已被移除"
            conn.execute(
                "INSERT INTO file_tracking (id,path,dir_id,mtime,size,md5,status,indexed,created_at,updated_at) \
                 VALUES ('ghost','ghost.txt','gone-dir',0,0,'ghostmd5','active',1,0,0)",
                [],
            )
            .unwrap();
            crate::db::tracker::upsert_embedding(&conn, "ghost", &[1.0]).unwrap();
            crate::db::tracker::upsert_chunk_embedding(&conn, "ghostmd5", 0, &[1.0]).unwrap();
            crate::db::tracker::store_content(&conn, "ghostmd5", "ghost body", false, None).unwrap();
            conn.execute(
                "INSERT INTO doc_chunks (md5,chunk_index,start_char,end_char,text) VALUES ('ghostmd5',0,0,10,'ghost')",
                [],
            )
            .unwrap();
            // 仍在配置里的目录（d1）的数据必须留下
            crate::db::tracker::update_indexed(&conn, &fid, Some("keepmd5")).unwrap();
            crate::db::tracker::store_content(&conn, "keepmd5", "keep body", false, None).unwrap();
        }

        let swept = svc.sweep_orphan_dirs().unwrap();
        assert_eq!(swept, 1, "应清扫 1 个孤儿 dir_id");

        let conn = svc.db.get().unwrap();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM file_tracking WHERE dir_id='gone-dir'"), 0, "孤儿行应删除");
        assert_eq!(count("SELECT COUNT(*) FROM doc_embeddings WHERE file_id='ghost'"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM doc_chunks WHERE md5='ghostmd5'"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM chunk_embeddings WHERE md5='ghostmd5'"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM content_index WHERE md5='ghostmd5'"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM file_tracking"), 1, "只剩仍在配置里的目录");
        assert_eq!(count(&format!("SELECT COUNT(*) FROM file_tracking WHERE id='{fid}'")), 1);
        assert_eq!(count("SELECT COUNT(*) FROM content_index WHERE md5='keepmd5'"), 1);
    }

    #[test]
    fn test_delete_file() {
        let (svc, fid, _dir) = setup();
        let path = tmp_file("test.txt", "delete test content");

        svc.index_file(&fid, &path, "d1", None).unwrap();
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

    /// 并发写竞争下的重试要有**退避**：立即重试会撞同一把锁（实测 7/7、15/22 失败）。
    #[test]
    fn retry_busy_backs_off_then_gives_up() {
        let mut n = 0;
        let ok = retry_busy("unit", "f1", || -> Result<u8> {
            n += 1;
            if n < 3 {
                anyhow::bail!("database is locked");
            }
            Ok(7)
        });
        assert_eq!(ok, Some(7), "前两次失败后第三次应成功");
        assert_eq!(n, 3);

        let mut k = 0;
        let failed: Option<u8> = retry_busy("unit", "f2", || -> Result<u8> {
            k += 1;
            anyhow::bail!("database is locked")
        });
        assert!(failed.is_none(), "一直失败要返回 None");
        assert_eq!(k, 3, "最多尝试 3 次");
    }

    #[test]
    fn test_plausible_fallback_normal_text() {
        let text = "Hello World 这是一个测试文本 合同编号 1234567890 包含各种常用字符内容";
        assert!(is_plausible_text_fallback(text, "txt"));
    }

    #[test]
    fn test_plausible_fallback_null_bytes_rejected() {
        let text = "hello world\0\0\0\0\0more text here";
        assert!(!is_plausible_text_fallback(text, "txt"));
    }

    #[test]
    fn test_plausible_fallback_binary_junk_rejected() {
        // Binary content: mostly non-printable bytes, printable_ratio << 0.7
        let text: String = (0..200)
            .map(|i| char::from_u32(i % 32).unwrap_or('\x01'))
            .collect();
        assert!(!is_plausible_text_fallback(&text, "pdf"));
    }

    #[test]
    fn test_plausible_fallback_empty_rejected() {
        assert!(!is_plausible_text_fallback("", "txt"));
    }

    #[test]
    fn test_plausible_fallback_legit_plaintext() {
        let text = "这是一份正式的合同文本，甲方为某某公司，乙方为某某个人。签订日期：2025年1月1日。双方经协商一致，达成以下协议条款。";
        assert!(is_plausible_text_fallback(text, "txt"));
    }

    #[test]
    fn test_quality_score_persisted_after_extraction() {
        let (svc, fid, _dir) = setup();
        let path = tmp_file(
            "quality_test.txt",
            "This is a test document with sufficient content for quality scoring to produce a non-null score",
        );

        svc.index_file(&fid, &path, "d1", None).unwrap();

        let conn = svc.db.get().unwrap();
        let rec = crate::db::tracker::get_file_by_id(&conn, &fid)
            .unwrap()
            .unwrap();
        let md5 = rec.md5.expect("md5 should be set after indexing");
        let (score, _, _) = crate::db::tracker::get_content_quality(&conn, &md5)
            .unwrap()
            .expect("quality row should exist");
        assert!(
            score.is_some(),
            "quality_score must be non-null after extraction"
        );

        let _ = std::fs::remove_file(&path);
    }
}
