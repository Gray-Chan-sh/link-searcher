//! Index-content verification: re-extract files marked indexed but with empty
//! content, and flag persistently-empty files as dead.

use serde::Serialize;

use crate::db;
use crate::db::tracker;
use crate::extractor::ocr;

use super::ReextractOutcome;
#[derive(Serialize, Debug)]
pub struct VerifyReport {
    pub checked: u64,
    pub recovered: u64,
    pub dead: u64,
    pub failed: u64,
}
pub(super) fn run_verify_core(
    db_pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    indexer: &std::sync::Arc<crate::indexer::IndexerService>,
    force_dead: bool,
) -> Result<VerifyReport, String> {
    let _guard = crate::state::TaskGuard::new("verify");
    let conn = match db_pool.get() {
        Ok(c) => c,
        Err(e) => return Err(format!("db error: {e}")),
    };
        let mut candidates = match tracker::find_empty_content_files(&conn) {
            Ok(v) => v,
            Err(e) => return Err(format!("{e}")),
        };
        if force_dead
            && let Ok(dead) = tracker::find_dead_files(&conn) {
                candidates.extend(dead);
            }
        let mut checked = 0u64;
        let mut recovered = 0u64;
        let mut dead = 0u64;
        let mut failed = 0u64;

        for rec in candidates {
            let dir = match db::dir_config::get_dir(&conn, &rec.dir_id)
                .and_then(|d| d.ok_or_else(|| anyhow::anyhow!("dir config not found")))
            {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("[VERIFY] {}: {e}", rec.path);
                    failed += 1;
                    continue;
                }
            };
            if let Some(ref md5) = rec.md5 {
                let _ = tracker::delete_content(&conn, md5);
            }
            let full_path = std::path::Path::new(&dir.path).join(&rec.path);
            let file_id = rec.id.clone();
            if let Err(e) = indexer.delete_document_only(&file_id) {
                log::warn!("[VERIFY] delete stale doc failed {file_id}: {e}");
            }
            match indexer.index_file(&file_id, &full_path, &rec.dir_id, None) {
                Ok(()) => {
                    checked += 1;
                    // Re-check the stored content: recovery only counts when
                    // non-empty text actually landed.
                    let ok = conn
                        .query_row(
                            "SELECT length(trim(text_content)) FROM content_index \
                             JOIN file_tracking ON file_tracking.md5 = content_index.md5 \
                             WHERE file_tracking.id = ?1",
                            rusqlite::params![file_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .unwrap_or(0);
                    if ok > 0 {
                        recovered += 1;
                        log::info!("[VERIFY] 恢复: {} ({} chars)", rec.path, ok);
                    } else {
                        let _ = tracker::mark_dead_content(&conn, &file_id);
                        dead += 1;
                        log::warn!("[VERIFY] 仍为空, 标记 dead: {}", rec.path);
                    }
                }
                Err(e) => {
                    log::warn!("[VERIFY] 重试失败: {}: {e}", rec.path);
                    failed += 1;
                }
            }
        }
        crate::state::push_task_brief(
            "verify",
            format!("索引有效性验证完成: 检查 {checked}，恢复 {recovered}，空 {dead}，失败 {failed}"),
        );
        Ok(VerifyReport { checked, recovered, dead, failed })
}

/// Returns (new_reextract_count, is_exhausted).
pub(crate) fn next_reextract_state(
    old_count: i64,
    old_score: Option<f64>,
    new_score: Option<f64>,
) -> (i64, bool) {
    if let (Some(old), Some(new)) = (old_score, new_score) {
        if (new - old) < 0.1 {
            return (3, true);
        }
    }
    let new_count = std::cmp::min(old_count + 1, 3);
    (new_count, false)
}

/// Shared single-file re-extraction core used by the CLI `quality reextract`
/// and the Tauri commands [`re_extract_file`] / [`re_extract_low_quality`].
/// Clears the cached content so `index_file` re-extracts, deletes the stale
/// Tantivy doc, re-indexes, and applies the `reextract_count` guard via
/// [`next_reextract_state`]. `old_score`/`old_count` are supplied by the caller
/// (DB lookup for single-file mode, the low-quality row for batch mode).
pub(crate) fn reextract_one(
    db_pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    indexer: &crate::indexer::IndexerService,
    file_id: &str,
    old_score: Option<f64>,
    old_count: i64,
    engine_override: Option<ocr::OcrEngineType>,
) -> anyhow::Result<ReextractOutcome> {
    let conn = db_pool
        .get()
        .map_err(|e| anyhow::anyhow!("db error: {e}"))?;
    let rec = tracker::get_file_by_id(&conn, file_id)?
        .ok_or_else(|| anyhow::anyhow!("file not found"))?;
    let dir = db::dir_config::get_dir(&conn, &rec.dir_id)?
        .ok_or_else(|| anyhow::anyhow!("dir config not found"))?;
    let md5 = rec
        .md5
        .clone()
        .ok_or_else(|| anyhow::anyhow!("file has no md5"))?;

    if old_count >= 3 {
        return Ok(ReextractOutcome {
            reextracted: false,
            old_score,
            new_score: old_score,
            reason: Some("max_reextract".into()),
        });
    }

    // ponytail: delete_content clears md5 cache so index_file re-extracts
    let _ = tracker::delete_content(&conn, &md5);
    let full_path = std::path::Path::new(&dir.path).join(&rec.path);
    drop(conn);

    // Must delete stale Tantivy doc — re-adding without it leaves duplicates.
    let _ = indexer.delete_document_only(file_id);
    indexer.index_file(file_id, &full_path, &rec.dir_id, engine_override)?;

    let conn = db_pool
        .get()
        .map_err(|e| anyhow::anyhow!("db error: {e}"))?;
    let (new_score, _new_count, _new_confidence) = match tracker::get_content_quality(&conn, &md5)? {
        Some(v) => v,
        None => (None, 0, None),
    };

    let (next_count, exhausted) = next_reextract_state(old_count, old_score, new_score);

    if exhausted {
        let mut flags: Vec<String> = {
            let flags_str: String = conn
                .query_row(
                    "SELECT quality_flags FROM content_index WHERE md5 = ?1",
                    rusqlite::params![md5],
                    |row| row.get(0),
                )
                .unwrap_or_else(|_| "[]".into());
            serde_json::from_str::<Vec<String>>(&flags_str).unwrap_or_default()
        };
        if !flags.contains(&"exhausted".to_string()) {
            flags.push("exhausted".into());
        }
        let flags_json =
            serde_json::to_string(&flags).unwrap_or_else(|_| "[\"exhausted\"]".into());
        let _ = tracker::update_reextract_state(&conn, &md5, 3, &flags_json);
        return Ok(ReextractOutcome {
            reextracted: true,
            old_score,
            new_score,
            reason: Some("exhausted".into()),
        });
    }

    let flags_str: String = conn
        .query_row(
            "SELECT quality_flags FROM content_index WHERE md5 = ?1",
            rusqlite::params![md5],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "[]".into());
    let _ = tracker::update_reextract_state(&conn, &md5, next_count, &flags_str);

    Ok(ReextractOutcome {
        reextracted: true,
        old_score,
        new_score,
        reason: None,
    })
}

