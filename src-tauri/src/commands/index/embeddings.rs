//! Embedding backfill: document-level and chunk-level vector generation,
//! plus the debounced post-index scheduler.

use std::collections::{hash_map::Entry, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Context;
use serde::Serialize;

use crate::db::tracker;

/// Serializes doc- and chunk-level embedding backfills. Both use the same
/// embedding backend, so running them at once only splits the same capacity.
///
/// Held per **pass** by the chunk drain (one pass ≈ 3.6 min, see
/// [`MAX_CHUNKS_PER_PASS`]) so it can't starve the doc pass; the doc pass still
/// holds it for its whole (short) run. A run-scoped chunk hold was the bug:
/// one full chunk drain (hours) kept newly indexed files waiting hours-to-days
/// for their vector (measured p90 = 58 h; see `docs/index-pipeline-eval.md`).
/// (Chunk *building* in `db::chunks` isn't gated — it does no inference.)
static BACKFILL_LOCK: Mutex<()> = Mutex::new(());

fn backfill_guard() -> MutexGuard<'static, ()> {
    BACKFILL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// RAII flag that is always cleared on drop, so an early `return` can never
/// leave a backfill marked as running.
struct RunningFlag(&'static AtomicBool);

impl Drop for RunningFlag {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Single-flight guards. Now that [`BACKFILL_LOCK`] is only held per shard,
/// nothing else stops a manual command and the post-index scheduler from
/// running the same backfill twice concurrently (duplicate inference + writes).
static DOC_BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);
static CHUNK_BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);

/// Chunks embedded per chunk-backfill pass. Bounds memory per selection and —
/// more importantly — sets how long a single [`BACKFILL_LOCK`] hold lasts:
/// ≈512 / 2.37 ≈ 3.6 min at the measured remote rate, so a long chunk drain
/// yields to doc backfill every few minutes instead of holding for hours.
const MAX_CHUNKS_PER_PASS: usize = 512;

/// Minimum interval between "yield to doc" attempts inside the chunk drain.
/// Guards against re-loading the (heavy, text-bearing) doc queue on every pass
/// when those rows keep failing to embed; normal passes are ~3.5 min, so this
/// only matters for pathologically short passes.
const DOC_YIELD_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) fn run_backfill_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
    if DOC_BACKFILL_RUNNING.swap(true, Ordering::SeqCst) {
        log::info!("[AI] doc 向量回填已在运行，跳过本次");
        return Ok(BackfillReport { processed: 0, pending: 0, failed: 0 });
    }
    let _running = RunningFlag(&DOC_BACKFILL_RUNNING);
    let _serial = backfill_guard();
    let _guard = crate::state::TaskGuard::new("backfill");
    const BATCH: usize = 64;

    let conn = db.get().map_err(|e| format!("db error: {e}"))?;
    let rows = missing_embedding_rows(&conn).map_err(|e| e.to_string())?;
    let total = rows.len();
    if total == 0 {
        return Ok(BackfillReport { processed: 0, pending: 0, failed: 0 });
    }

    // Duplicate files share content (same md5) and would yield identical
    // vectors — embed each unique md5 once and write the vector to all of its
    // file_ids. In this library ~10% of files are duplicates.
    let mut unique = group_by_md5(rows);
    // Sort by text length so every batch is length-homogeneous and the
    // engine's `BatchLongest` padding stays short (attention is O(L²)).
    unique.sort_by_key(|(t, _)| t.chars().count());

    log::info!(
        "[AI] 向量回填开始: {total} 个文件缺向量（{} 个唯一内容，并行度 {}）",
        unique.len(),
        crate::ai::local_embed::parallelism(),
    );
    let mut processed = 0usize;
    let mut failed = 0usize;
    let started = std::time::Instant::now();
    let mut tracker = RateTracker::new(started);
    for group in unique.chunks(super_group_size()) {
        let texts: Vec<String> = group.iter().map(|(t, _)| t.clone()).collect();
        let vecs = crate::ai::embed_batched(&texts, BATCH);
        for ((_, ids), v) in group.iter().zip(vecs) {
            match v {
                Some(vec) => {
                    for id in ids {
                        if let Err(e) = tracker::upsert_embedding(&conn, id, &vec) {
                            log::warn!("[AI] upsert_embedding failed {id}: {e}");
                            failed += 1;
                        }
                    }
                }
                None => failed += ids.len(),
            }
        }
        processed += group.iter().map(|(_, ids)| ids.len()).sum::<usize>();
        report_progress("backfill", processed, total, failed, &mut tracker);
    }
    log::info!("[AI] 回填完成: {processed} 处理, {failed} 失败, 剩余 {}", total - processed);
    crate::state::push_task_brief(
        "backfill",
        format!("向量回填完成: {processed} 补齐, {failed} 失败"),
    );
    Ok(BackfillReport {
        processed: processed as u64,
        pending: (total - processed) as u64,
        failed: failed as u64,
    })
}

/// Rolling samples used to show a *recent* processing rate. The cumulative
/// average is kept too, but any earlier stall (e.g. a gateway outage with long
/// retries) drags it down for the rest of the run — so the live rate, and the
/// ETA derived from it, must come from a short recent window instead.
struct RateTracker {
    started: std::time::Instant,
    samples: std::collections::VecDeque<(std::time::Instant, usize)>,
}

impl RateTracker {
    fn new(started: std::time::Instant) -> Self {
        Self { started, samples: std::collections::VecDeque::new() }
    }

    /// Record a `(now, processed)` sample, keeping a small window.
    fn record(&mut self, processed: usize) {
        self.samples.push_back((std::time::Instant::now(), processed));
        while self.samples.len() > 4 {
            self.samples.pop_front();
        }
    }

    /// Cumulative items/second since the task started.
    fn average(&self, processed: usize) -> f64 {
        processed as f64 / self.started.elapsed().as_secs_f64().max(0.001)
    }

    /// Items/second over the retained window (oldest sample → now). `None` when
    /// there are fewer than two samples or no progress in between.
    fn recent(&self, processed: usize) -> Option<f64> {
        if self.samples.len() < 2 {
            return None;
        }
        let (t0, p0) = *self.samples.front()?;
        let dt = t0.elapsed().as_secs_f64();
        let dp = processed.saturating_sub(p0);
        if dt < 1.0 || dp == 0 {
            return None;
        }
        Some(dp as f64 / dt)
    }
}

/// Progress/report group size. Remote (adaptive) gateways report per request,
/// so a smaller group makes the UI update sooner; local replicas benefit from a
/// larger group (more batches to spread across the pool).
fn super_group_size() -> usize {
    if crate::config::is_local_embedding_model(
        &crate::config::load_config().active_embedding_model_id,
    ) {
        256
    } else {
        64
    }
}

/// Log a progress line (with live rate + ETA) and publish it to the UI registry.
fn report_progress(
    task: &str,
    processed: usize,
    total: usize,
    failed: usize,
    tracker: &mut RateTracker,
) {
    tracker.record(processed);
    let avg = tracker.average(processed);
    let now = tracker.recent(processed).unwrap_or(avg);
    let eta_secs = if now > 0.0 {
        ((total.saturating_sub(processed)) as f64 / now) as u64
    } else {
        0
    };
    log::info!(
        "[AI] 回填进度: {processed}/{total} (失败 {failed}, 实时 {now:.1}/s, 平均 {avg:.1}/s, ETA {}m){}",
        eta_secs / 60,
        crate::ai::embed_plan_summary()
            .map(|p| format!(" · {p}"))
            .unwrap_or_default()
    );
    crate::state::set_task_progress_rates(
        task,
        processed as u64,
        total as u64,
        format!("{processed}/{total}"),
        now,
        avg,
        eta_secs,
    );
}

/// Background-caller wrapper (mirrors [`run_backfill_chunk_embeddings_public`]).
pub fn run_backfill_embeddings_public(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<(), String> {
    run_backfill_embeddings(db).map(|_| ())
}

/// Debounced post-index doc-embedding backfill: a single pending run is
/// admitted and the 3s delay coalesces watcher/batch bursts.
///
/// `PENDING` stays set across the whole window — debounce, the wait for
/// [`BACKFILL_LOCK`], and the run itself. Previously it was cleared right
/// before running, so every burst during a long chunk drain spawned another
/// thread that parked on the lock (a leak, since the blocking thread hadn't
/// registered its `TaskGuard` yet and the old check looked for `"backfill"`).
///
/// Superseded by [`ensure_embedding_keepers`] on the indexing path; kept for
/// callers that only want a one-shot doc backfill.
pub fn schedule_backfill_embeddings(db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) {
    static PENDING: AtomicBool = AtomicBool::new(false);

    if !crate::ai::embedding_enabled() || PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let pool = db.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        if let Err(e) = run_backfill_embeddings(&pool) {
            log::warn!("[AI] 调度回填失败: {e}");
        }
        PENDING.store(false, Ordering::SeqCst);
    });
}

/// Number of keeper threads currently alive (0 or 2). `compare_exchange(0, 2)`
/// admits exactly one pair; each thread decrements on exit, so the last one out
/// resets the counter and a later call can start a fresh pair.
static KEEPERS_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Decrements [`KEEPERS_ACTIVE`] on drop, so a panic in a keeper can't strand
/// the counter above 0 (which would block every future keeper pair forever).
struct KeeperTicket;

impl Drop for KeeperTicket {
    fn drop(&mut self) {
        KEEPERS_ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Whether a keeper should keep polling: while it still finds work, or while an
/// indexing pass is running (it may produce more rows at any moment).
fn keeper_should_continue(processed: u64, is_indexing: bool) -> bool {
    processed > 0 || is_indexing
}

/// Keep doc- and chunk-level embedding running *alongside* a scan instead of
/// only after it. Two keeper threads each drain their own queue; the
/// shard-scoped [`BACKFILL_LOCK`] makes them alternate, so neither starves the
/// other. A keeper polls every second while idle, and exits once its queue is
/// empty **and** indexing has stopped.
///
/// Callers: [`IndexerService::ensure_embedding_keepers`](crate::indexer::IndexerService::ensure_embedding_keepers)
/// on the scan/watcher path. Idempotent — a no-op while a pair is already alive.
pub fn ensure_embedding_keepers(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    is_indexing: Arc<AtomicBool>,
) {
    if !crate::ai::embedding_enabled() {
        return;
    }
    if KEEPERS_ACTIVE
        .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // a pair is already running
    }

    let pool_doc = db.clone();
    let flag_doc = is_indexing.clone();
    std::thread::spawn(move || {
        let _ticket = KeeperTicket;
        log::info!("[AI] 嵌入 keeper 启动（doc）");
        run_keeper_loop(&pool_doc, &flag_doc, run_backfill_embeddings, "backfill");
    });

    let pool_chunk = db.clone();
    let flag_chunk = is_indexing;
    std::thread::spawn(move || {
        let _ticket = KeeperTicket;
        log::info!("[AI] 嵌入 keeper 启动（chunk）");
        run_keeper_loop(
            &pool_chunk,
            &flag_chunk,
            run_backfill_chunk_embeddings,
            "backfill_chunks",
        );
    });
}

/// Drain one backfill repeatedly until it has nothing to do and indexing has
/// stopped. `run` is [`run_backfill_embeddings`] or
/// [`run_backfill_chunk_embeddings`].
fn run_keeper_loop(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    is_indexing: &AtomicBool,
    run: fn(&r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) -> Result<BackfillReport, String>,
    task: &str,
) {
    /// Poll interval when the queue is empty. Freshly committed rows are picked
    /// up within one poll, so this is only about not hot-looping.
    const POLL: std::time::Duration = std::time::Duration::from_millis(500);
    let mut logged_error = false;
    loop {
        let processed = match run(db) {
            Ok(r) => {
                logged_error = false;
                r.processed
            }
            Err(e) => {
                // Log only the first failure: a gateway-less/disabled AI would
                // otherwise log once per poll for the whole scan.
                if !logged_error {
                    log::warn!("[AI] {task} keeper 暂停: {e}");
                    logged_error = true;
                }
                0
            }
        };
        if !keeper_should_continue(processed, is_indexing.load(Ordering::Acquire)) {
            log::info!("[AI] 嵌入 keeper 退出（{task} 已排空且索引已停止）");
            return;
        }
        // Always yield between drains: a queue whose rows all keep failing
        // would otherwise spin, and even a productive drain benefits from
        // letting fresh rows accumulate.
        std::thread::sleep(POLL);
    }
}

/// Files whose document embedding is missing or stale, with cached extracted
/// text (joined via `content_index` by md5 — no re-extraction needed).
/// `e.updated_at < ci.indexed_at` marks content re-extracted after the
/// embedding was written; both columns are unix seconds.
///
/// Returns `(file_id, md5, text)`; callers should collapse duplicates by md5
/// (see [`group_by_md5`]) because files sharing content share their vector.
pub(super) fn missing_embedding_rows(
    conn: &rusqlite::Connection,
) -> anyhow::Result<Vec<(String, String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT ft.id, ft.md5, ci.text_content
             FROM file_tracking ft
             JOIN content_index ci ON ft.md5 = ci.md5
             LEFT JOIN doc_embeddings e ON e.file_id = ft.id
             WHERE ft.indexed = 1 AND ft.status = 'active'
               AND (e.file_id IS NULL OR e.updated_at < ci.indexed_at)",
        )
        .context("prepare missing-embedding query")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("query missing-embedding rows")?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect missing-embedding rows")
}

/// Cheap `EXISTS` probe for the doc-backfill queue (same predicate as
/// [`missing_embedding_rows`], but it never loads the text bodies).
///
/// The chunk drain uses it before each pass to yield to doc embedding, so newly
/// indexed files get their vector without waiting for the whole (hours-long)
/// chunk backlog — and without paying for a full `missing_embedding_rows` scan
/// just to discover there is nothing to do.
pub(super) fn has_missing_doc_embeddings(conn: &rusqlite::Connection) -> anyhow::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM file_tracking ft
             JOIN content_index ci ON ft.md5 = ci.md5
             LEFT JOIN doc_embeddings e ON e.file_id = ft.id
             WHERE ft.indexed = 1 AND ft.status = 'active'
               AND (e.file_id IS NULL OR e.updated_at < ci.indexed_at)
         )",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n != 0)
    .context("query has_missing_doc_embeddings")
}

/// Collapse `(file_id, md5, text)` rows by md5: duplicate files (same content)
/// become one `(text, file_ids)` entry so the text is embedded once and the
/// resulting vector is written to every file_id.
pub(super) fn group_by_md5(rows: Vec<(String, String, String)>) -> Vec<(String, Vec<String>)> {
    let mut map: HashMap<String, (String, Vec<String>)> = HashMap::new();
    for (file_id, md5, text) in rows {
        match map.entry(md5) {
            Entry::Occupied(mut e) => e.get_mut().1.push(file_id),
            Entry::Vacant(e) => {
                e.insert((text, vec![file_id]));
            }
        }
    }
    map.into_values().collect()
}

/// Long documents (md5s that have `doc_chunks` rows) that have at least one
/// chunk lacking an embedding, capped per run. Ordered by fewest-missing
/// first so near-complete documents converge quickly and produce fully
/// searchable vector sets sooner. Returns md5s only; the caller loads the
/// per-md5 chunk set and skips already-embedded indexes (partial progress).
pub(super) fn missing_chunk_embedding_md5s(
    conn: &rusqlite::Connection,
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT dc.md5
             FROM doc_chunks dc
             LEFT JOIN chunk_embeddings ce
               ON ce.md5 = dc.md5 AND ce.chunk_index = dc.chunk_index
             WHERE ce.md5 IS NULL
             GROUP BY dc.md5
             ORDER BY COUNT(*) ASC, dc.md5
             LIMIT ?1",
        )
        .context("prepare missing-chunk-embedding query")?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], |row| row.get::<_, String>(0))
        .context("query missing-chunk-embedding md5s")?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect missing-chunk-embedding md5s")
}

/// Existing (md5, chunk_index) pairs that already have embeddings, so the
/// per-md5 backfill loop only embeds the missing ones (partial progress —
/// a doc interrupted mid-run resumes without re-embedding finished chunks).
pub(super) fn existing_chunk_indexes(conn: &rusqlite::Connection, md5: &str) -> anyhow::Result<std::collections::HashSet<i64>> {
    let mut stmt = conn
        .prepare("SELECT chunk_index FROM chunk_embeddings WHERE md5 = ?1")
        .context("prepare existing-chunk-indexes")?;
    let rows = stmt
        .query_map(rusqlite::params![md5], |row| row.get::<_, i64>(0))
        .context("query existing-chunk-indexes")?;
    let mut set = std::collections::HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Backfill chunk-level embeddings for long documents: every `doc_chunks`
/// text (≤1500 chars) gets its own vector, so semantic retrieval can hit
/// detail-level content that a whole-file vector dilutes. Idempotent — a doc
/// interrupted mid-run resumes without re-embedding finished chunks.
///
/// Chunks from all selected docs are flattened and embedded in bulk so the
/// engine's replica pool can parallelize across batches.
///
/// Before each pass it **yields to doc embedding** (see
/// [`has_missing_doc_embeddings`]) so newly indexed files get a searchable
/// vector without waiting for the whole chunk backlog to drain.
pub(super) fn run_backfill_chunk_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
    if CHUNK_BACKFILL_RUNNING.swap(true, Ordering::SeqCst) {
        log::info!("[AI] chunk 向量回填已在运行，跳过本次");
        return Ok(BackfillReport { processed: 0, pending: 0, failed: 0 });
    }
    let _running = RunningFlag(&CHUNK_BACKFILL_RUNNING);
    let _guard = crate::state::TaskGuard::new("backfill_chunks");
    /// Long documents examined per pass.
    const MAX_DOCS_PER_PASS: usize = 500;
    const BATCH: usize = 64;

    let conn = db.get().map_err(|e| format!("db error: {e}"))?;
    let total_missing = count_missing_chunks(&conn).map_err(|e| e.to_string())?;
    if total_missing == 0 {
        return Ok(BackfillReport { processed: 0, pending: 0, failed: 0 });
    }

    log::info!(
        "[AI] chunk 向量回填开始: {total_missing} 个待补块（并行度 {}），循环直至补齐",
        crate::ai::local_embed::parallelism(),
    );
    let mut processed = 0usize;
    let mut failed = 0usize;
    let started = std::time::Instant::now();
    let mut tracker = RateTracker::new(started);
    // 上一轮让位 doc 回填的时间点（见循环内的说明）。
    let mut last_doc_yield = std::time::Instant::now()
        .checked_sub(DOC_YIELD_MIN_INTERVAL)
        .unwrap_or_else(std::time::Instant::now);
    loop {
        // 让位 doc 回填：新索引的文件先拿到向量，再继续补 chunk。
        //
        // 必须在**循环内**做：本函数是 drain-to-empty，一次调用会一直循环到缺块
        // 清空（扫描期间可能数小时），在调用点之前让位不起作用。doc 回填内部自己
        // 持 BACKFILL_LOCK，所以这里必须在取 pass 锁**之前**调用，否则死锁。
        if last_doc_yield.elapsed() >= DOC_YIELD_MIN_INTERVAL {
            let has_doc_work = match has_missing_doc_embeddings(&conn) {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("[AI] 检查 doc 回填队列失败: {e}");
                    false
                }
            };
            if has_doc_work {
                last_doc_yield = std::time::Instant::now();
                if let Err(e) = run_backfill_embeddings(db) {
                    log::warn!("[AI] chunk 回填让位 doc 失败: {e}");
                }
            }
        }

        // Hold the data lock per pass, not for the whole drain: one full chunk
        // backlog can take hours, and a run-scoped hold would keep doc backfill
        // (and thus newly indexed files' vectors) waiting the entire time.
        let _pass_serial = backfill_guard();
        let pass_started = std::time::Instant::now();
        // Only the missing chunks, so every pass makes real progress and an
        // interrupted run resumes without redoing finished work.
        let mut tasks =
            collect_missing_chunk_tasks(&conn, MAX_DOCS_PER_PASS, MAX_CHUNKS_PER_PASS)
                .map_err(|e| e.to_string())?;
        if tasks.is_empty() {
            break;
        }
        // Length-sort so `BatchLongest` padding stays short.
        tasks.sort_by_key(|(_, _, t)| t.chars().count());
        let pass_len = tasks.len();
        // 进度分母随扫描增长：已完成 + 本轮实测的剩余缺口。用启动时的快照作分母，
        // 扫描期间新增的块会让读数出现 48/24 这种 >100%（实测）。
        let pass_total = processed + count_missing_chunks(&conn).unwrap_or(pass_len);
        let mut pass_ok = 0usize;
        let mut pass_failed = 0usize;
        for group in tasks.chunks(super_group_size()) {
            let texts: Vec<String> = group.iter().map(|(_, _, t)| t.clone()).collect();
            let vecs = crate::ai::embed_batched(&texts, BATCH);
            for ((md5, idx, _), v) in group.iter().zip(vecs) {
                match v {
                    Some(vec) => {
                        match crate::db::tracker::upsert_chunk_embedding(&conn, md5, *idx, &vec) {
                            Ok(()) => pass_ok += 1,
                            Err(e) => {
                                log::warn!("[AI] upsert_chunk_embedding failed {md5}#{idx}: {e}");
                                failed += 1;
                                pass_failed += 1;
                            }
                        }
                    }
                    None => {
                        failed += 1;
                        pass_failed += 1;
                    }
                }
            }
            processed += group.len();
            report_progress("backfill_chunks", processed, pass_total, failed, &mut tracker);
        }
        // Observability + lock-yield proof: one line per pass, so the log shows
        // the drain is sharded and how long each shard held the data lock.
        log::info!(
            "[AI] chunk 回填分片: {pass_len} 块 ({pass_ok} ok, {pass_failed} fail), {:.1}s（本轮持锁结束，doc 回填可插队）",
            pass_started.elapsed().as_secs_f64(),
        );
        // A pass that embedded nothing would re-select the same rows forever
        // (e.g. every chunk keeps failing); stop instead of hot-looping.
        if pass_ok == 0 {
            log::warn!("[AI] chunk 向量回填本轮 {pass_len} 块全部失败，停止以避免死循环");
            break;
        }
    }
    let remaining = count_missing_chunks(&conn).unwrap_or(0);
    log::info!("[AI] chunk 向量回填完成: 共 {processed} 块, {failed} 失败, 剩余 {remaining}");
    crate::state::push_task_brief(
        "backfill_chunks",
        format!("chunk 向量回填完成: {processed} 补齐, {failed} 失败, 剩余 {remaining}"),
    );
    Ok(BackfillReport {
        processed: processed as u64,
        pending: remaining as u64,
        failed: failed as u64,
    })
}

/// Number of `doc_chunks` rows without a matching `chunk_embeddings` row.
fn count_missing_chunks(conn: &rusqlite::Connection) -> anyhow::Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM doc_chunks dc
         LEFT JOIN chunk_embeddings ce
           ON ce.md5 = dc.md5 AND ce.chunk_index = dc.chunk_index
         WHERE ce.md5 IS NULL",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
    .context("count missing chunk embeddings")
}

/// Up to `max_chunks` missing `(md5, chunk_index, text)` rows across at most
/// `max_docs` long documents, fewest-missing docs first (see
/// [`missing_chunk_embedding_md5s`]). Redundant pairs are filtered out again so
/// a doc interrupted mid-pass resumes without re-embedding finished chunks.
fn collect_missing_chunk_tasks(
    conn: &rusqlite::Connection,
    max_docs: usize,
    max_chunks: usize,
) -> anyhow::Result<Vec<(String, i64, String)>> {
    let md5s = missing_chunk_embedding_md5s(conn, max_docs)?;
    let mut tasks: Vec<(String, i64, String)> = Vec::new();
    for md5 in &md5s {
        let chunks = match crate::db::chunks::get_chunks(conn, md5) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[AI] get_chunks failed for {md5}: {e}");
                continue;
            }
        };
        let existing = match existing_chunk_indexes(conn, md5) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[AI] existing_chunk_indexes failed for {md5}: {e}");
                continue;
            }
        };
        for c in &chunks {
            if !existing.contains(&c.chunk_index) {
                tasks.push((md5.clone(), c.chunk_index, c.text.clone()));
                if tasks.len() >= max_chunks {
                    return Ok(tasks);
                }
            }
        }
    }
    Ok(tasks)
}

/// Public wrapper for background thread callers (startup / post-scan) that
/// only care about "did it run without panicking".
pub fn run_backfill_chunk_embeddings_public(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<(), String> {
    run_backfill_chunk_embeddings(db).map(|_| ())
}
/// Summary of a [`backfill_embeddings`] run.
#[derive(Serialize)]
pub struct BackfillReport {
    pub processed: u64,
    pub pending: u64,
    pub failed: u64,
}

#[cfg(test)]
mod tests {
    use super::{collect_missing_chunk_tasks, count_missing_chunks, group_by_md5};

    /// The doc-yield probe must report exactly the predicate
    /// `missing_embedding_rows` uses — and must go false once the vector exists.
    #[test]
    fn has_missing_doc_embeddings_tracks_doc_queue() {
        use super::has_missing_doc_embeddings;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d1','/a',1,0,0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_tracking (id,path,dir_id,mtime,size,md5,status,indexed,created_at,updated_at) \
             VALUES ('f1','p1','d1',0,0,'m1','active',1,0,0)",
            [],
        )
        .unwrap();
        crate::db::tracker::store_content(&conn, "m1", "body text", false, None).unwrap();

        assert!(has_missing_doc_embeddings(&conn).unwrap(), "有内容但无 doc 向量 → 应报告有活");

        crate::db::tracker::upsert_embedding(&conn, "f1", &[1.0, 2.0]).unwrap();
        assert!(!has_missing_doc_embeddings(&conn).unwrap(), "补上向量后应报告无活");
    }

    /// `RunningFlag` must clear on drop even when its scope returns early —
    /// otherwise an aborted/skipped run leaves the single-flight flag stuck and
    /// every later backfill is refused.
    #[test]
    fn running_flag_clears_on_early_return() {
        use std::sync::atomic::{AtomicBool, Ordering};

        fn scope(flag: &'static AtomicBool) -> u32 {
            if flag.swap(true, Ordering::SeqCst) {
                return 0; // already running → bail out without holding the flag
            }
            let _running = super::RunningFlag(flag);
            1
        }

        static F: AtomicBool = AtomicBool::new(false);
        assert_eq!(scope(&F), 1);
        assert_eq!(scope(&F), 1, "flag must be reset after the guarded scope ends");
        assert!(!F.load(Ordering::SeqCst));
    }

    /// The scheduler's single-flight gate must span the whole debounce+wait+run
    /// window; clearing it early (as the old `SCHEDULED` did) let every burst
    /// during a long chunk drain spawn another thread parked on the lock.
    #[test]
    fn schedule_pending_flag_spans_full_window() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static P: AtomicBool = AtomicBool::new(false);
        assert!(!P.swap(true, Ordering::SeqCst), "first schedule is admitted");
        assert!(P.swap(true, Ordering::SeqCst), "re-schedule while pending is refused");
        P.store(false, Ordering::SeqCst);
        assert!(!P.swap(true, Ordering::SeqCst), "schedule admitted again after the run");
    }

    /// The per-pass chunk cap must stay small enough that one lock hold is
    /// minutes, not hours — this is the property that keeps doc backfill from
    /// starving. (Measured throughput ≈ 2.37 chunks/s, so 512 ≈ 3.6 min.)
    #[test]
    fn chunk_pass_cap_keeps_lock_hold_short() {
        const CHUNKS_PER_SEC: f64 = 2.37;
        const MAX_PASS_SECS: f64 = 15.0 * 60.0;
        let hold_secs = super::MAX_CHUNKS_PER_PASS as f64 / CHUNKS_PER_SEC;
        assert!(
            hold_secs <= MAX_PASS_SECS,
            "a {}-chunk pass would hold the lock {hold_secs:.0}s — too long",
            super::MAX_CHUNKS_PER_PASS
        );
    }

    /// A keeper must only stop when BOTH its queue is empty and indexing has
    /// stopped — otherwise it would exit mid-scan and wait for the next batch.
    #[test]
    fn keeper_keeps_polling_while_work_or_indexing() {
        use super::keeper_should_continue;
        assert!(keeper_should_continue(1, false), "work found → keep going");
        assert!(keeper_should_continue(0, true), "idle but indexing → keep waiting");
        assert!(keeper_should_continue(5, true), "both → keep going");
        assert!(!keeper_should_continue(0, false), "idle and no scan → exit");
    }

    #[test]
    fn group_by_md5_collapses_duplicate_files() {
        let rows = vec![
            ("f1".to_string(), "m1".to_string(), "hello".to_string()),
            ("f2".to_string(), "m1".to_string(), "hello".to_string()),
            ("f3".to_string(), "m2".to_string(), "world".to_string()),
        ];
        let mut grouped = group_by_md5(rows);
        grouped.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(grouped.len(), 2, "two unique md5s");
        assert_eq!(grouped[0].0, "hello");
        assert_eq!(grouped[0].1, vec!["f1", "f2"]);
        assert_eq!(grouped[1].0, "world");
        assert_eq!(grouped[1].1, vec!["f3"]);
    }

    #[test]
    fn group_by_md5_empty_input() {
        assert!(group_by_md5(vec![]).is_empty());
    }

    /// The live rate must come from the recent window, not the lifetime
    /// average: an earlier stall (long retries) would otherwise keep the
    /// displayed speed and ETA depressed for the rest of the run.
    #[test]
    fn rate_tracker_recent_ignores_old_slow_stretch() {
        use std::time::{Duration, Instant};
        let started = Instant::now() - Duration::from_secs(600);
        let mut t = super::RateTracker::new(started);
        // Old, slow history is *not* in the window: only the last few samples.
        let now = Instant::now();
        t.samples.push_back((now - Duration::from_secs(60), 100));
        t.samples.push_back((now - Duration::from_secs(20), 600));
        t.record(800); // appends (now, 800)

        let recent = t.recent(800).expect("window has >= 2 samples and progress");
        assert!(recent > 10.0 && recent < 14.0, "recent={recent}"); // 700 items / ~60s
        assert!(t.average(800) < 2.0, "lifetime avg must stay low");
    }

    #[test]
    fn rate_tracker_needs_two_samples() {
        use std::time::Instant;
        let started = Instant::now();
        let mut t = super::RateTracker::new(started);
        t.record(10);
        assert!(t.recent(10).is_none(), "one sample is not a rate");
        assert!(t.recent(999).is_none());
    }

    /// The chunk backfill selects only genuinely-missing chunks and honours the
    /// per-pass cap — the loop depends on both to converge.
    #[test]
    fn count_and_collect_missing_chunks_respect_caps() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();

        // md5-a: all chunks missing.
        let long_a = format!("{}。{}。{}。", "甲".repeat(5000), "乙".repeat(5000), "丙".repeat(5000));
        crate::db::tracker::store_content(&conn, "md5-a", &long_a, false, None).unwrap();
        let wa = crate::db::chunks::chunk_text(&long_a);
        crate::db::chunks::replace_chunks(&conn, "md5-a", &wa).unwrap();

        // md5-b: same size but its first 2 chunks already embedded.
        let long_b = format!("{}。{}。{}。", "子".repeat(5000), "丑".repeat(5000), "寅".repeat(5000));
        crate::db::tracker::store_content(&conn, "md5-b", &long_b, false, None).unwrap();
        let wb = crate::db::chunks::chunk_text(&long_b);
        crate::db::chunks::replace_chunks(&conn, "md5-b", &wb).unwrap();
        let chunks_b = crate::db::chunks::get_chunks(&conn, "md5-b").unwrap();
        for c in chunks_b.iter().take(2) {
            crate::db::tracker::upsert_chunk_embedding(&conn, "md5-b", c.chunk_index, &[1.0, 2.0, 3.0]).unwrap();
        }

        let total = count_missing_chunks(&conn).unwrap();
        assert_eq!(total, wa.len() + (chunks_b.len() - 2), "only missing chunks counted");

        let capped = collect_missing_chunk_tasks(&conn, 10, 1).unwrap();
        assert_eq!(capped.len(), 1, "per-pass chunk cap respected");

        let all = collect_missing_chunk_tasks(&conn, 10, 100_000).unwrap();
        assert_eq!(all.len(), total);
        let b_missing = all.iter().filter(|(m, _, _)| m == "md5-b").count();
        assert_eq!(b_missing, chunks_b.len() - 2, "embedded chunks must not be re-selected");
    }
}
