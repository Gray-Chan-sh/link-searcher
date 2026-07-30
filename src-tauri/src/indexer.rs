//! Orchestrates Tantivy indexing with SQLite tracking and content deduplication.
//!
//! [`IndexerService`] wraps an [`IndexManager`] and DB pool to provide a
//! high-level API for indexing and deleting files.  The tantivy writer is
//! lazily created on first use and shared via a mutex so multiple callers
//! can batch documents before a single [`commit`](IndexerService::commit).

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use anyhow::{Context, Result};
use md5::Digest;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use tantivy::IndexWriter;

use crate::search::indexer::Indexer;
use crate::search::IndexManager;

/// High-level indexer that coordinates Tantivy writes, content dedup, and
/// file-tracking status.
pub struct IndexerService {
    db: Pool<SqliteConnectionManager>,
    pub(crate) index_manager: Arc<RwLock<IndexManager>>,
    writer: Mutex<Option<IndexWriter>>,
}

// SAFETY: IndexWriter is `Send` (it holds owned channels + Arc<Mutex<…>>).
// Mutex<Option<IndexWriter>> adds Sync, so IndexerService itself is Sync.

impl IndexerService {
    /// Create a new indexer service backed by a DB pool and a tantivy index.
    pub fn new(
        db: Pool<SqliteConnectionManager>,
        index_manager: Arc<RwLock<IndexManager>>,
    ) -> Self {
        Self {
            db,
            index_manager,
            writer: Mutex::new(None),
        }
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
        // Compute file metadata early (only depends on file_path, not fallible).
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

        // Wrap the fallible body so we can log errors before returning them.
        let conn = self.db.get().context("failed to get DB connection")?;

        let result = (|| -> Result<()> {

            // Read raw bytes and compute MD5 for dedup.
            let raw = std::fs::read(file_path)
                .with_context(|| format!("failed to read {file_path:?}"))?;
            let hash = format!("{:x}", md5::Md5::digest(&raw));

            // Stage 1: Start
            log::info!("[INDEX] 开始: {file_name} ({file_ext}, {} bytes)", raw.len());

            // Dedup: reuse previously extracted text if content already indexed.
            let text = match crate::db::tracker::get_content(&conn, &hash)
                .context("failed to query content_index")?
            {
                Some(t) => {
                    log::info!("[INDEX] 去重: {file_name} 复用 md5={hash:.8} 的已有内容");
                    t
                }
                None => {
                    // Fallback chain for text extraction; track whether OCR was actually used.
                    let mut ocr_used = false;
                    let extracted = match crate::extractor::extract_text(file_path) {
                        Ok(text) if text.len() > 10 => text,
                        Ok(text) => {
                            log::info!("[INDEX] 提取内容过短 ({})，尝试 OCR 回退", text.len());
                            match crate::extractor::ocr::ocr_image(file_path, "eng") {
                                Ok(ocr_text) if !ocr_text.is_empty() => {
                                    ocr_used = true;
                                    ocr_text
                                }
                                _ => {
                                    log::warn!("[INDEX] OCR 回退也失败，使用原始内容");
                                    text
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("[INDEX] 提取失败: {e}，尝试纯文本回退");
                            match std::fs::read_to_string(file_path) {
                                Ok(t) => {
                                    ocr_used = true;
                                    t
                                }
                                Err(_) => return Err(anyhow::anyhow!("所有提取方式均失败: {e}")),
                            }
                        }
                    };
                    let char_count = extracted.chars().count();
                    log::info!("[INDEX] 提取文字: {file_name} ({char_count} 字符)");
                    crate::db::tracker::store_content(&conn, &hash, &extracted, ocr_used, None)
                        .context("failed to store extracted content")?;
                    extracted
                }
            };

            // Gather metadata for the Tantivy document.
            let meta = std::fs::metadata(file_path)
                .with_context(|| format!("failed to stat {file_path:?}"))?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0);
            let file_size = raw.len() as u64;

            // Acquire the tantivy writer and add the document.
            let mut guard = self.lock_writer()?;
            let w = guard
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("writer poisoned"))?;

            Indexer::add_document(w, file_id, &file_name, &file_ext, dir_id, &text, mtime, file_size)
                .map_err(|e| anyhow::anyhow!("failed to add document to index: {e}"))?;

            // Update tracking row.
            crate::db::tracker::update_indexed(&conn, file_id, Some(&hash))
                .context("failed to update indexed status")?;

            log::info!("[INDEX] 完成: {file_name}");

            Ok(())
        })();

        if let Err(ref e) = result {
            let error_type = classify_error(e, &file_ext);
            let _ = crate::db::tracker::log_index_error(&conn, file_id, &file_path.to_string_lossy(), error_type, &e.to_string());
            log::error!("[INDEX] 失败: {file_name}: {e}");
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

        // Mark as indexed=0 (re-indexable) so a future scan can re-add it.
        // If the file still exists in file_tracking, it gets a second chance.
        crate::db::tracker::update_indexed(&conn, file_id, None)
            .or_else(|_| crate::db::tracker::mark_deleted(&conn, file_id))
            .context("failed to update file tracking")?;

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

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Lock the writer mutex and return the guard.  Creates the writer on
    /// first access.
    fn lock_writer(&self) -> Result<MutexGuard<'_, Option<IndexWriter>>> {
        let guard = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("index writer lock poisoned: {e}"))?;

        if guard.is_some() {
            return Ok(guard);
        }
        drop(guard); // release before creating

        let mgr = self
            .index_manager
            .read()
            .map_err(|e| anyhow::anyhow!("index manager lock poisoned: {e}"))?;
        let new_w = mgr
            .writer(50_000_000)
            .map_err(|e| anyhow::anyhow!("failed to create index writer: {e}"))?;
        drop(mgr);

        let mut guard = self
            .writer
            .lock()
            .map_err(|e| anyhow::anyhow!("index writer lock poisoned: {e}"))?;
        *guard = Some(new_w);
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

fn classify_error(err: &anyhow::Error, _ext: &str) -> &'static str {
    let msg = format!("{err}");
    if msg.contains("Permission denied") || msg.contains("Access denied") {
        "access_denied"
    } else if msg.contains("OCR") || msg.contains("tesseract") {
        "ocr_failed"
    } else if msg.contains("timeout") {
        "timeout"
    } else if msg.contains("parse") || msg.contains("invalid") || msg.contains("failed to") {
        "parse_error"
    } else {
        "unknown"
    }
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
    fn test_index_file_creates_document() {
        let (svc, fid) = setup();
        let path = tmp_file("test.txt", "hello world test content");

        svc.index_file(&fid, &path, "d1").unwrap();
        svc.commit().unwrap();

        // Verify Tantivy has the document.
        let mgr = svc.index_manager.read().unwrap();
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
        let reader = { svc.index_manager.read().unwrap().reader().unwrap() };
        let searcher = reader.searcher();
        let schema = build_schema();
        let content_f = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(svc.index_manager.read().unwrap().index(), vec![content_f]);
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
        let reader = { svc.index_manager.read().unwrap().reader().unwrap() };
        let searcher = reader.searcher();
        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let parser = tantivy::query::QueryParser::for_index(svc.index_manager.read().unwrap().index(), vec![content]);
        let query = parser.parse_query("delete").unwrap();
        let top = searcher
            .search(&query, &tantivy::collector::TopDocs::with_limit(10))
            .unwrap();
        assert_eq!(top.len(), 0, "deleted doc should not be found");

        let _ = std::fs::remove_file(&path);
    }
}
