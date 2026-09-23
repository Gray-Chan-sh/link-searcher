//! DB↔Tantivy integrity healing: re-index records marked indexed but absent
//! from the search index, and collapse duplicate documents.

use serde::Serialize;
#[derive(Serialize, Debug, Default)]
pub struct IndexHealReport {
    /// DB records marked indexed=1 (expected searchable) at the start.
    pub db_indexed: u64,
    /// Documents actually present in Tantivy at the start.
    pub tantivy_docs: u64,
    /// Records re-indexed because they were absent from Tantivy.
    pub healed: u64,
    /// Duplicate documents removed (a file whose id appeared >1× in Tantivy).
    pub deduped: u64,
    /// Records that could not be re-indexed (e.g. file gone from disk;
    /// the scanner will mark them deleted on the next scan).
    pub failed: u64,
}

/// Heal DB↔Tantivy drift: DB records marked indexed=1 but absent from the
/// Tantivy index are re-indexed in place, and duplicate documents (same
/// file_id multiple times) are collapsed to one. A schema-mismatch rebuild
/// clears the Tantivy directory and creates an empty index without
/// re-populating it, and `needs_reindex` skips unchanged indexed=1 records —
/// so files orphaned this way would never be re-written by a scan. Duplicates
/// arise when a heal races an in-flight batch index writing the same file.
/// Unchanged files reuse their cached extraction by md5 (no re-OCR).
/// Idempotent: a no-op when DB and Tantivy are in sync.
pub(crate) fn run_index_integrity_heal(
    db_pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    indexer: &std::sync::Arc<crate::indexer::IndexerService>,
) -> Result<IndexHealReport, String> {
    use std::collections::{HashMap, HashSet};
    use tantivy::collector::TopDocs;
    use tantivy::query::AllQuery;
    use tantivy::schema::Value;

    // Serialize heal runs process-wide: automatic hooks (startup / scan
    // completion) and a manual trigger must not re-index concurrently.
    static HEAL_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _lock = HEAL_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| "heal lock poisoned".to_string())?;
    let _guard = crate::state::TaskGuard::new("heal-index");

    let db_indexed: i64 = {
        let conn = db_pool.get().map_err(|e| format!("db error: {e}"))?;
        conn.query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status='active' AND indexed=1",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("{e}"))?
    };
    if db_indexed == 0 {
        return Ok(IndexHealReport::default());
    }

    // Enumerate file_ids actually present in Tantivy. Force a fresh reload:
    // the throttled reader may lag recent commits, which would make this diff
    // re-report just-healed files as missing on a repeated run.
    let im = indexer
        .index_manager
        .read()
        .map_err(|e| format!("index lock: {e}"))?;
    let reader = im.reader_fresh().map_err(|e| format!("reader: {e}"))?;
    let searcher = reader.searcher();
    let schema = crate::search::schema::build_schema();
    let file_id_field = schema.get_field("file_id").map_err(|e| format!("{e}"))?;
    // Limit by actual doc count: duplicates make docs exceed db_indexed.
    let doc_count = searcher.num_docs();
    let top = searcher
        .search(&AllQuery, &TopDocs::with_limit(doc_count as usize + 1))
        .map_err(|e| format!("tantivy search: {e}"))?;
    let mut tantivy_ids: HashSet<String> = HashSet::with_capacity(doc_count as usize);
    let mut dup_ids: Vec<String> = Vec::new();
    for (_score, addr) in top {
        if let Ok(doc) = searcher.doc::<tantivy::TantivyDocument>(addr)
            && let Some(v) = doc.get_first(file_id_field).and_then(|v| v.as_str())
        {
            if !tantivy_ids.insert(v.to_string()) {
                dup_ids.push(v.to_string());
            }
        }
    }
    drop(im);

    // DB records marked indexed=1 → (dir_id, rel_path) map + missing set.
    let mut by_id: HashMap<String, (String, String)> = HashMap::new();
    let mut missing: Vec<String> = Vec::new(); // ids absent from Tantivy
    {
        let conn = db_pool.get().map_err(|e| format!("db error: {e}"))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, dir_id, path FROM file_tracking \
                 WHERE status='active' AND indexed=1",
            )
            .map_err(|e| format!("{e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| format!("{e}"))?;
        for row in rows {
            let (id, dir_id, path) = row.map_err(|e| format!("{e}"))?;
            by_id.insert(id.clone(), (dir_id, path));
            if !tantivy_ids.contains(&id) {
                missing.push(id);
            }
        }
    }
    // Only collapse duplicates for records that are still active & indexed=1;
    // a dup id already deleted from the DB should not be resurrected.
    dup_ids.retain(|id| by_id.contains_key(id));

    if missing.is_empty() && dup_ids.is_empty() {
        log::info!(
            "[HEAL] DB({db_indexed}) 与 Tantivy({}) 一致，无需修复",
            tantivy_ids.len()
        );
        return Ok(IndexHealReport {
            db_indexed: db_indexed as u64,
            tantivy_docs: tantivy_ids.len() as u64,
            healed: 0,
            deduped: 0,
            failed: 0,
        });
    }

    // dir_id → monitor root, resolved once.
    let dir_roots: std::collections::HashMap<String, String> = {
        let conn = db_pool.get().map_err(|e| format!("db error: {e}"))?;
        crate::db::dir_config::list_dirs(&conn)
            .map_err(|e| format!("{e}"))?
            .into_iter()
            .map(|d| (d.id, d.path))
            .collect()
    };

    if !missing.is_empty() {
        log::warn!(
            "[HEAL] DB 标 indexed=1 但 Tantivy 缺失 {} 份，开始重灌（复用 md5 缓存，不重新 OCR）",
            missing.len()
        );
    }

    let mut healed = 0u64;
    let mut failed = 0u64;
    let missing_total = missing.len() as u64;
    for (i, file_id) in missing.iter().enumerate() {
        let done = i as u64 + 1;
        if done % 200 == 0 || done == missing_total {
            log::info!("[HEAL] 进度 {done}/{missing_total} (成功 {healed}, 失败 {failed})");
        }
        let (dir_id, rel_path) = &by_id[file_id];
        let Some(root) = dir_roots.get(dir_id) else {
            log::warn!("[HEAL] {rel_path}: dir_config {dir_id} 不存在，跳过");
            failed += 1;
            continue;
        };
        let full_path = std::path::Path::new(root).join(rel_path);
        if !full_path.exists() {
            // File gone from disk — leave it for the scanner to mark deleted.
            log::debug!("[HEAL] {rel_path} 已不在磁盘，跳过（等待扫描标删）");
            failed += 1;
            continue;
        }
        // No delete-before-add here: the fresh-reload snapshot above makes
        // `missing` genuinely absent from the committed index, and a
        // same-batch delete+add of one file_id would cancel itself out.
        match indexer.index_file(file_id, &full_path, dir_id, None) {
            Ok(()) => healed += 1,
            Err(e) => {
                log::warn!("[HEAL] 重灌失败 {rel_path}: {e}");
                failed += 1;
            }
        }
    }

    // ── Duplicate cleanup ─────────────────────────────────────────────
    // Delete every copy of a duplicated file_id (by term), commit, then
    // re-add one clean copy in a separate commit so the add survives.
    let mut deduped = 0u64;
    if !dup_ids.is_empty() {
        log::warn!("[HEAL] 清理 {} 个重复文件（先删全部副本，再补一份）", dup_ids.len());
        let mut deleted_any = false;
        for id in &dup_ids {
            match indexer.delete_document_only(id) {
                Ok(()) => deleted_any = true,
                Err(e) => log::warn!("[HEAL] 删除重复文档失败 {id}: {e}"),
            }
        }
        if deleted_any && let Err(e) = indexer.commit_now() {
            log::error!("[HEAL] 重复删除提交失败: {e}");
        }
        for id in &dup_ids {
            let (dir_id, rel_path) = &by_id[id];
            let Some(root) = dir_roots.get(dir_id) else {
                failed += 1;
                continue;
            };
            let full_path = std::path::Path::new(root).join(rel_path);
            if !full_path.exists() {
                log::debug!("[HEAL] {rel_path} 已不在磁盘，跳过重复补写");
                failed += 1;
                continue;
            }
            match indexer.index_file(id, &full_path, dir_id, None) {
                Ok(()) => {
                    deduped += 1;
                    log::info!("[HEAL] 重复文件已归一: {rel_path}");
                }
                Err(e) => {
                    log::warn!("[HEAL] 重复文件补写失败 {rel_path}: {e}");
                    failed += 1;
                }
            }
        }
    }

    if (healed > 0 || deduped > 0) && let Err(e) = indexer.commit_now() {
        log::error!("[HEAL] 最终提交失败: {e}");
    }

    crate::state::push_task_brief(
        "heal-index",
        format!("索引完整性修复: DB {db_indexed} / Tantivy {}，缺失 {missing_total} 成功 {healed}，去重 {deduped}，失败 {failed}",
            tantivy_ids.len()),
    );
    log::info!("[HEAL] 完成: 缺失 {missing_total}, 成功 {healed}, 去重 {deduped}, 失败 {failed}");
    Ok(IndexHealReport {
        db_indexed: db_indexed as u64,
        tantivy_docs: tantivy_ids.len() as u64,
        healed,
        deduped,
        failed,
    })
}
