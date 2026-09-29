//! Embedding backfill: document-level and chunk-level vector generation,
//! plus the debounced post-index scheduler.

use std::collections::{hash_map::Entry, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use anyhow::Context;
use serde::Serialize;

use crate::db::tracker;

/// Serializes doc- and chunk-level embedding backfills. Both use the same local
/// model pool, so running them at once just makes each other slower and makes
/// progress hard to read. Whichever starts first runs to completion; the other
/// waits. (Chunk *building* in `db::chunks` isn't gated — it does no inference.)
static BACKFILL_LOCK: Mutex<()> = Mutex::new(());

fn backfill_guard() -> MutexGuard<'static, ()> {
    BACKFILL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn run_backfill_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
    let _serial = backfill_guard();
    let _guard = crate::state::TaskGuard::new("backfill");
    const BATCH: usize = 64;
    // Unique texts embedded per progress step. A multiple of BATCH so the
    // engine can parallelize across batches.
    const SUPER: usize = 256;

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
    for group in unique.chunks(SUPER) {
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
        report_progress("backfill", processed, total, failed, started);
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

/// Log a progress line (with rate + ETA) and publish it to the UI registry.
fn report_progress(task: &str, processed: usize, total: usize, failed: usize, started: std::time::Instant) {
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let rate = processed as f64 / elapsed;
    let eta_min = ((total.saturating_sub(processed)) as f64 / rate.max(0.001) / 60.0) as u64;
    log::info!(
        "[AI] 回填进度: {processed}/{total} (失败 {failed}, {rate:.1}/s, ETA {eta_min}m){}",
        crate::ai::embed_plan_summary()
            .map(|p| format!(" · {p}"))
            .unwrap_or_default()
    );
    crate::state::set_task_progress(
        task,
        processed as u64,
        total as u64,
        format!("{processed}/{total} · {rate:.1}/s · ETA {eta_min}m"),
    );
}

/// Background-caller wrapper (mirrors [`run_backfill_chunk_embeddings_public`]).
pub fn run_backfill_embeddings_public(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<(), String> {
    run_backfill_embeddings(db).map(|_| ())
}

/// Debounced post-index doc-embedding backfill: `SCHEDULED` admits a single
/// pending run and the 3s delay coalesces watcher/batch bursts.
pub fn schedule_backfill_embeddings(db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>) {
    static SCHEDULED: AtomicBool = AtomicBool::new(false);

    if !crate::ai::embedding_enabled() || SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    let pool = db.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        SCHEDULED.store(false, Ordering::SeqCst);
        // Re-checked after the delay so concurrent schedules don't stack runs.
        if crate::state::running_task_ids().iter().any(|t| t == "backfill") {
            return;
        }
        if let Err(e) = run_backfill_embeddings(&pool) {
            log::warn!("[AI] 调度回填失败: {e}");
        }
    });
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
pub(super) fn run_backfill_chunk_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
    let _serial = backfill_guard();
    let _guard = crate::state::TaskGuard::new("backfill_chunks");
    /// Chunks selected per inner pass. Bounds memory per selection and keeps
    /// progress readable; the outer loop continues until the backlog is gone.
    const MAX_CHUNKS_PER_PASS: usize = 4096;
    /// Long documents examined per pass.
    const MAX_DOCS_PER_PASS: usize = 500;
    const BATCH: usize = 64;
    const SUPER: usize = 256;

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
    loop {
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
        let mut pass_ok = 0usize;
        for group in tasks.chunks(SUPER) {
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
                            }
                        }
                    }
                    None => failed += 1,
                }
            }
            processed += group.len();
            report_progress("backfill_chunks", processed, total_missing, failed, started);
        }
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
