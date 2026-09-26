//! Embedding backfill: document-level and chunk-level vector generation,
//! plus the debounced post-index scheduler.

use std::collections::{hash_map::Entry, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use serde::Serialize;

use crate::db::tracker;
pub(super) fn run_backfill_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
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
    // Sort by text length so every 64-batch is length-homogeneous and the
    // engine's `BatchLongest` padding stays short (attention is O(L²)).
    unique.sort_by_key(|(t, _)| t.chars().count());

    log::info!(
        "[AI] 向量回填开始: {total} 个文件缺向量（{} 个唯一内容）",
        unique.len()
    );
    let mut processed = 0usize;
    let mut failed = 0usize;
    for chunk in unique.chunks(BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(t, _)| t.clone()).collect();
        let vecs = crate::ai::embed_batched(&texts, BATCH);
        for ((_, ids), v) in chunk.iter().zip(vecs) {
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
        processed += chunk.iter().map(|(_, ids)| ids.len()).sum::<usize>();
        log::info!("[AI] 回填进度: {processed}/{total} (失败 {failed})");
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
/// detail-level content that a whole-file vector dilutes. Idempotent —
/// repeated runs converge (only md5s with zero chunk embeddings are picked).
pub(super) fn run_backfill_chunk_embeddings(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<BackfillReport, String> {
    if !crate::ai::embedding_enabled() {
        return Err("AI 未配置（embedding_api_base 为空），无法生成语义向量".into());
    }
    let _guard = crate::state::TaskGuard::new("backfill_chunks");
    const MAX_PER_RUN: usize = 500;
    const BATCH: usize = 64;

    let conn = db.get().map_err(|e| format!("db error: {e}"))?;
    let md5s = missing_chunk_embedding_md5s(&conn, MAX_PER_RUN).map_err(|e| e.to_string())?;
    let total = md5s.len();
    if total == 0 {
        return Ok(BackfillReport { processed: 0, pending: 0, failed: 0 });
    }

    log::info!("[AI] chunk 向量回填开始: {total} 个长文档缺块向量");
    let mut processed = 0usize;
    let mut failed = 0usize;
    for md5 in &md5s {
        let chunks = match crate::db::chunks::get_chunks(&conn, md5) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[AI] get_chunks failed for {md5}: {e}");
                failed += 1;
                continue;
            }
        };
        if chunks.is_empty() {
            continue;
        }
        // 只嵌缺失的块：中断后恢复不重复嵌已完成的块（P1-B 增量收敛）。
        let existing = match existing_chunk_indexes(&conn, md5) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[AI] existing_chunk_indexes failed for {md5}: {e}");
                failed += 1;
                continue;
            }
        };
        let mut missing: Vec<&crate::db::chunks::DocChunk> = chunks
            .iter()
            .filter(|c| !existing.contains(&c.chunk_index))
            .collect();
        if missing.is_empty() {
            continue;
        }
        // Length-sort each batch so `BatchLongest` padding stays short.
        missing.sort_by_key(|c| c.text.chars().count());
        for batch in missing.chunks(BATCH) {
            let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
            let vecs = crate::ai::embed_batched(&texts, BATCH);
            for (chunk, v) in batch.iter().zip(vecs) {
                match v {
                    Some(vec) => {
                        if let Err(e) = crate::db::tracker::upsert_chunk_embedding(
                            &conn, md5, chunk.chunk_index, &vec,
                        ) {
                            log::warn!("[AI] upsert_chunk_embedding failed {md5}#{}: {e}", chunk.chunk_index);
                            failed += 1;
                        }
                    }
                    None => failed += 1,
                }
            }
        }
        processed += 1;
    }
    log::info!("[AI] chunk 向量回填完成: {processed} 文档, {failed} 失败");
    crate::state::push_task_brief(
        "backfill_chunks",
        format!("chunk 向量回填完成: {processed} 补齐, {failed} 失败"),
    );
    Ok(BackfillReport {
        processed: processed as u64,
        pending: (total - processed) as u64,
        failed: failed as u64,
    })
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
    use super::group_by_md5;

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
}
