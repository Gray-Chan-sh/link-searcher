//! File-tracking and content-index CRUD.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use uuid::Uuid;

/// Indexed state stored in `file_tracking.indexed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexedState {
    Pending = 0,
    Indexed = 1,
    Failed = 2,
    /// Phase-1 extraction complete; Tantivy write pending (mark_extracted).
    Extracted = 3,
}

impl std::fmt::Display for IndexedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Indexed => write!(f, "indexed"),
            Self::Failed => write!(f, "failed"),
            Self::Extracted => write!(f, "extracted"),
        }
    }
}

/// File status stored in `file_tracking.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Active,
    Deleted,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: String,
    pub path: String,
    pub dir_id: String,
    pub mtime: i64,
    pub size: u64,
    pub md5: Option<String>,
    pub status: String,
    pub indexed: i64,
    pub error_msg: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub dead_content: i64,
}

#[derive(Debug, Clone)]
pub struct IndexStats {
    pub total: u64,
    pub indexed: u64,
    pub pending: u64,
    pub errors: u64,
}

#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub md5: String,
    pub count: u64,
    pub paths: Vec<String>,
    pub file_ids: Vec<String>,
}

// -- file_tracking --

pub fn upsert_file(
    conn: &Connection,
    path: &str,
    dir_id: &str,
    mtime: i64,
    size: u64,
    md5: Option<&str>,
) -> Result<String> {
    let now = chrono::Utc::now().timestamp();
    let id = Uuid::new_v4().to_string();
    let file_ext = extension_of(path);
    conn.query_row(
         "INSERT INTO file_tracking (id,path,file_ext,dir_id,mtime,size,md5,status,indexed,error_msg,created_at,updated_at)
          VALUES (?1,?2,?3,?4,?5,?6,?7,'active',0,NULL,?8,?8)
          ON CONFLICT(path) DO UPDATE SET
              file_ext=excluded.file_ext,
              mtime=excluded.mtime, size=excluded.size, md5=excluded.md5,
              status='active',
              indexed=CASE WHEN file_tracking.mtime!=excluded.mtime OR file_tracking.size!=excluded.size THEN 0 ELSE file_tracking.indexed END,
              error_msg=NULL, updated_at=excluded.updated_at
          RETURNING id",
        rusqlite::params![id, path, file_ext, dir_id, mtime, size as i64, md5, now],
        |row| row.get::<_, String>(0),
    )
    .context("upsert_file failed")
}

pub fn mark_deleted(conn: &Connection, file_id: &str) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET status='deleted', updated_at=?1 WHERE id=?2",
            rusqlite::params![chrono::Utc::now().timestamp(), file_id],
        )
        .context("mark_deleted failed")?;
    if n == 0 {
        anyhow::bail!("file not found: {file_id}");
    }
    Ok(())
}

/// Permanently remove a tracked row (releases its UNIQUE path). Used when a
/// freshly-walked record is superseded by a moved file's original record.
pub fn hard_delete_file(conn: &Connection, file_id: &str) -> Result<()> {
    let n = conn
        .execute("DELETE FROM file_tracking WHERE id=?1", rusqlite::params![file_id])
        .context("hard_delete_file failed")?;
    if n == 0 {
        anyhow::bail!("file not found: {file_id}");
    }
    Ok(())
}

pub fn update_file_path(conn: &Connection, file_id: &str, new_path: &str, new_dir_id: &str) -> Result<()> {
    let file_ext = extension_of(new_path);
    let n = conn
        .execute(
            "UPDATE file_tracking SET path=?1, dir_id=?2, file_ext=?3, updated_at=?4 WHERE id=?5",
            rusqlite::params![new_path, new_dir_id, file_ext, chrono::Utc::now().timestamp(), file_id],
        )
        .context("update_file_path failed")?;
    if n == 0 {
        anyhow::bail!("file not found: {file_id}");
    }
    Ok(())
}

fn extension_of(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

pub(crate) fn backfill_file_ext(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("SELECT id, path FROM file_tracking WHERE file_ext IS NULL OR file_ext = ''")
        .context("prepare ext backfill")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .context("query ext backfill rows")?;
    for row in rows {
        let (id, path) = row.context("read ext backfill row")?;
        let file_ext = extension_of(&path);
        conn.execute(
            "UPDATE file_tracking SET file_ext=?1 WHERE id=?2",
            rusqlite::params![file_ext, id],
        )
        .context("update ext backfill")?;
    }
    Ok(())
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        id: row.get("id")?,
        path: row.get("path")?,
        dir_id: row.get("dir_id")?,
        mtime: row.get("mtime")?,
        size: row.get::<_, i64>("size")? as u64,
        md5: row.get("md5")?,
        status: row.get("status")?,
        indexed: row.get("indexed")?,
        error_msg: row.get("error_msg")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        dead_content: row.get("dead_content")?,
    })
}

const SEL: &str =
    "SELECT id,path,dir_id,mtime,size,md5,status,indexed,error_msg,created_at,updated_at,dead_content FROM file_tracking";

pub fn get_file_by_id(conn: &Connection, file_id: &str) -> Result<Option<FileRecord>> {
    let mut s = conn.prepare(&format!("{SEL} WHERE id=?1")).context("prepare get_file_by_id")?;
    let mut rows = s.query_map(rusqlite::params![file_id], row_to_record)?;
    Ok(rows.next().transpose()?)
}

pub fn get_file_by_path(conn: &Connection, path: &str) -> Result<Option<FileRecord>> {
    let mut s = conn.prepare(&format!("{SEL} WHERE path=?1")).context("prepare get_file_by_path")?;
    let mut rows = s.query_map(rusqlite::params![path], row_to_record)?;
    Ok(rows.next().transpose()?)
}

/// Search files by path fragment (LIKE %path%). Returns at most `limit` file IDs.
pub fn search_file_ids_by_path_fragment(conn: &Connection, fragment: &str, limit: usize) -> Result<Vec<String>> {
    let like = format!("%{}%", fragment);
    let mut s = conn.prepare(
        "SELECT id FROM file_tracking WHERE lower(path) LIKE ?1 AND status='active' ORDER BY path LIMIT ?2"
    ).context("prepare search_file_ids_by_path_fragment")?;
    let rows = s.query_map(rusqlite::params![like, limit as i64], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect search_file_ids_by_path_fragment")
}

pub fn get_files_by_dir(conn: &Connection, dir_id: &str) -> Result<Vec<FileRecord>> {
    let mut s = conn
        .prepare(&format!("{SEL} WHERE dir_id=?1 ORDER BY path"))
        .context("prepare get_files_by_dir")?;
    let rows = s.query_map(rusqlite::params![dir_id], row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect files_by_dir")
}

pub fn count_files_by_dir(conn: &Connection, dir_id: &str) -> Result<u64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_tracking WHERE dir_id=?1",
        rusqlite::params![dir_id],
        |r| r.get(0),
    )
    .context("count_files_by_dir failed")?;
    Ok(count as u64)
}

pub fn delete_files_by_dir(conn: &Connection, dir_id: &str) -> Result<u64> {
    let n = conn
        .execute("DELETE FROM file_tracking WHERE dir_id=?1", rusqlite::params![dir_id])
        .context("delete_files_by_dir failed")?;
    Ok(n as u64)
}

pub fn migrate_paths_to_relative(conn: &Connection) -> Result<u64> {
    let dirs = crate::db::dir_config::list_dirs(conn)
        .context("failed to list dirs for migration")?;
    let mut count = 0u64;
    for dir in &dirs {
        let prefix = format!("{}/", dir.path.trim_end_matches('/'));
        let mut stmt = conn
            .prepare("SELECT id, path FROM file_tracking WHERE dir_id=?1 AND path LIKE ?2")
            .context("prepare migration select")?;
        let rows = stmt
            .query_map(
                rusqlite::params![dir.id, format!("{prefix}%")],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .context("query migration rows")?;
        for row in rows {
            let (id, path) = row.context("read migration row")?;
            if let Some(rel) = path.strip_prefix(&prefix) {
                let file_ext = extension_of(rel);
                conn.execute(
                    "UPDATE file_tracking SET path=?1, file_ext=?2 WHERE id=?3",
                    rusqlite::params![rel, file_ext, id],
                )
                .context("update migrated path")?;
                count += 1;
            }
        }
    }
    log::info!("[DB] 路径迁移: {} 条记录", count);
    Ok(count)
}

/// Absorb a sub-directory's file records into its parent directory:
/// re-roots `path` under the parent and reassigns `dir_id`. Runs in one
/// transaction; `sub_rel` is the sub-directory's path relative to the parent
/// (e.g. `B` for parent `A`, yielding new paths like `B/foo.txt`).
///
/// Also resets the `indexed` flag so a re-scan of the parent rebuilds the
/// Tantivy documents with the new paths (extraction is skipped via the
/// MD5/content dedup, so this is cheap).
pub fn absorb_subdir(
    conn: &Connection,
    sub_dir_id: &str,
    parent_dir_id: &str,
    sub_rel: &str,
) -> Result<u64> {
    let prefix = format!("{sub_rel}/");
    let tx = conn.unchecked_transaction().context("begin absorb txn")?;

    let rows = tx
        .prepare("SELECT id, path, md5 FROM file_tracking WHERE dir_id=?1")
        .context("prepare absorb select")?
        .query_map(rusqlite::params![sub_dir_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("collect absorb rows")?;

    let count = rows.len();
    for (id, path, md5) in rows {
        let new_path = format!("{prefix}{path}");
        let file_ext = extension_of(&new_path);
        tx.execute(
            "UPDATE file_tracking SET path=?1, file_ext=?2, dir_id=?3, indexed=0, md5=?4, error_msg=NULL WHERE id=?5",
            rusqlite::params![new_path, file_ext, parent_dir_id, md5, id],
        )
        .context("absorb update row")?;
    }

    tx.commit().context("commit absorb txn")?;
    Ok(count as u64)
}

pub fn get_files_needing_index(conn: &Connection, limit: usize) -> Result<Vec<FileRecord>> {
    let mut s = conn
        .prepare(&format!("{SEL} WHERE indexed IN (0,2,3) ORDER BY updated_at ASC LIMIT ?1"))
        .context("prepare get_files_needing_index")?;
    let rows = s.query_map(rusqlite::params![limit as i64], row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect needing_index")
}

/// Files marked indexed but whose extracted content is empty (trimmed length
/// 0) — candidates for validity re-check. `dead_content=0` keeps previously
/// confirmed-empty files out of automatic verification.
pub fn find_empty_content_files(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut s = conn
        .prepare(&format!(
            "{SEL} WHERE indexed=1 AND dead_content=0 AND status='active' \
             AND COALESCE(length(trim((SELECT text_content FROM content_index WHERE md5=file_tracking.md5))),0)=0"
        ))
        .context("prepare find_empty_content_files")?;
    let rows = s.query_map([], row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect find_empty_content_files")
}

/// Files confirmed empty-content (dead_content=1) — only reachable via
/// manual force re-verify.
pub fn find_dead_files(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut s = conn
        .prepare(&format!("{SEL} WHERE dead_content=1 AND status='active'"))
        .context("prepare find_dead_files")?;
    let rows = s.query_map([], row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect find_dead_files")
}

pub fn update_indexed(conn: &Connection, file_id: &str, md5: Option<&str>) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET indexed=1, md5=?1, error_msg=NULL, dead_content=0, updated_at=?2 WHERE id=?3",
            rusqlite::params![md5, chrono::Utc::now().timestamp(), file_id],
        )
        .context("update_indexed failed")?;
    if n == 0 { anyhow::bail!("file not found: {file_id}"); }
    Ok(())
}

/// Mark a file as having content that verified empty even after a retry.
/// Such files are skipped by future automatic verification (manual re-verify
/// can still force them); any successful re-index clears the flag.
pub fn mark_dead_content(conn: &Connection, file_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE file_tracking SET dead_content=1, updated_at=?1 WHERE id=?2",
        rusqlite::params![chrono::Utc::now().timestamp(), file_id],
    )
    .context("mark_dead_content failed")?;
    Ok(())
}

/// Clear the dead-content flag after any successful extraction.
pub fn clear_dead_content(conn: &Connection, file_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE file_tracking SET dead_content=0, updated_at=?1 WHERE id=?2",
        rusqlite::params![chrono::Utc::now().timestamp(), file_id],
    )
    .context("clear_dead_content failed")?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, file_id: &str, error: &str) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET indexed=2, error_msg=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![error, chrono::Utc::now().timestamp(), file_id],
        )
        .context("mark_failed failed")?;
    if n == 0 { anyhow::bail!("file not found: {file_id}"); }
    Ok(())
}

pub fn mark_extracted(conn: &Connection, file_id: &str, md5: Option<&str>) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET indexed=3, md5=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![md5, chrono::Utc::now().timestamp(), file_id],
        )
        .context("mark_extracted failed")?;
    if n == 0 { anyhow::bail!("file not found: {file_id}"); }
    Ok(())
}

fn run_stats_query(conn: &Connection, clause: &str, param: Option<&str>) -> Result<IndexStats> {
    let sql = format!(
        "SELECT COUNT(*) t, \
                COALESCE(SUM(CASE WHEN indexed=1 THEN 1 ELSE 0 END),0) i, \
                COALESCE(SUM(CASE WHEN indexed IN (0,3) THEN 1 ELSE 0 END),0) p, \
                COALESCE(SUM(CASE WHEN indexed=2 THEN 1 ELSE 0 END),0) e \
         FROM file_tracking {clause}"
    );
    let mut s = conn.prepare(&sql).context("prepare stats")?;
    match param {
        Some(p) => s.query_row(rusqlite::params![p], |row| {
            Ok(IndexStats { total: row.get::<_,i64>(0)? as u64, indexed: row.get::<_,i64>(1)? as u64, pending: row.get::<_,i64>(2)? as u64, errors: row.get::<_,i64>(3)? as u64 })
        }),
        None => s.query_row([], |row| {
            Ok(IndexStats { total: row.get::<_,i64>(0)? as u64, indexed: row.get::<_,i64>(1)? as u64, pending: row.get::<_,i64>(2)? as u64, errors: row.get::<_,i64>(3)? as u64 })
        }),
    }.context("stats query failed")
}

pub fn get_stats(conn: &Connection, dir_id: Option<&str>) -> Result<IndexStats> {
    match dir_id {
        Some(d) => run_stats_query(conn, "WHERE dir_id=?1 AND status='active'", Some(d)),
        None => run_stats_query(conn, "WHERE status='active'", None),
    }
}

pub fn get_duplicates(conn: &Connection) -> Result<Vec<DuplicateGroup>> {
    let mut s = conn
        .prepare(
            "SELECT md5, COUNT(*) cnt, GROUP_CONCAT(path) paths, GROUP_CONCAT(id) ids \
             FROM file_tracking WHERE md5 IS NOT NULL AND status='active' \
             GROUP BY md5 HAVING COUNT(*) > 1",
        )
        .context("prepare duplicates")?;
    let groups = s.query_map([], |row| {
        Ok(DuplicateGroup {
            md5: row.get("md5")?,
            count: row.get::<_, i64>("cnt")? as u64,
            paths: row.get::<_, String>("paths")?.split(',').map(String::from).collect(),
            file_ids: row.get::<_, String>("ids")?.split(',').map(String::from).collect(),
        })
    })?;
    groups.collect::<rusqlite::Result<Vec<_>>>().context("collect duplicates")
}

// -- content_index --

pub fn store_content(conn: &Connection, md5: &str, text: &str, ocr_used: bool, ocr_ms: Option<i64>) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT OR REPLACE INTO content_index (md5,text_content,indexed_at,char_count,ocr_used,ocr_duration_ms) \
         VALUES (?1,?2,?3,?4,?5,?6)",
        rusqlite::params![md5, text, now, text.chars().count() as i64, ocr_used as i64, ocr_ms],
    )
    .context("store_content failed")?;
    Ok(())
}

pub fn get_content(conn: &Connection, md5: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT text_content FROM content_index WHERE md5=?1")?;
    let mut rows = stmt.query_map(rusqlite::params![md5], |row| row.get::<_, String>(0))?;
    Ok(rows.next().transpose()?)
}

/// Delete content from the dedup cache so the next extraction for this hash
/// will re-run OCR/extraction rather than reusing stale text.
pub fn delete_content(conn: &Connection, md5: &str) -> Result<()> {
    conn.execute("DELETE FROM content_index WHERE md5=?1", rusqlite::params![md5])
        .context("delete_content failed")?;
    Ok(())
}

pub fn get_content_ocr_used(conn: &Connection, md5: &str) -> Result<bool> {
    match conn.query_row(
        "SELECT ocr_used FROM content_index WHERE md5 = ?1",
        rusqlite::params![md5],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(v) => Ok(v != 0),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub fn get_total_image_files(conn: &Connection) -> Result<u64> {
    let exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff"];
    let mut count: i64 = 0;
    for ext in &exts {
        count += conn.query_row(
            "SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND path LIKE ?1",
            rusqlite::params![format!("%.{}", ext)],
            |row| row.get(0),
        ).unwrap_or(0);
    }
    Ok(count as u64)
}

pub fn get_ocred_count(conn: &Connection) -> Result<u64> {
    let exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tiff"];
    let mut count: i64 = 0;
    for ext in &exts {
        count += conn
            .query_row(
                "SELECT COUNT(DISTINCT ft.md5) FROM file_tracking ft \
                 JOIN content_index ci ON ft.md5 = ci.md5 \
                 WHERE ft.status = 'active' AND ft.path LIKE ?1 \
                 AND length(ci.text_content) > 10",
                rusqlite::params![format!("%.{}", ext)],
                |row| row.get(0),
            )
            .unwrap_or(0);
    }
    Ok(count as u64)
}

// -- index_errors --

#[derive(Debug, Serialize)]
pub struct IndexError {
    pub id: i64,
    pub file_id: String,
    pub file_path: String,
    pub error_type: String,
    pub error_msg: String,
    pub created_at: i64,
}

pub fn log_index_error(conn: &Connection, file_id: &str, file_path: &str, error_type: &str, error_msg: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO index_errors (file_id, file_path, error_type, error_msg, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![file_id, file_path, error_type, error_msg, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

pub fn get_index_errors(conn: &Connection, limit: usize) -> Result<Vec<IndexError>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_id, file_path, error_type, error_msg, created_at FROM index_errors ORDER BY created_at DESC LIMIT ?1"
    )?;
    let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
        Ok(IndexError {
            id: row.get(0)?,
            file_id: row.get(1)?,
            file_path: row.get(2)?,
            error_type: row.get(3)?,
            error_msg: row.get(4)?,
            created_at: row.get(5)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

pub fn update_hotwords(conn: &Connection, text: &str) {
    let jieba = &crate::search::schema::JIEBA;
    for token in jieba.tokenize(text, jieba_rs::TokenizeMode::Search, true) {
        let word = token.word;
        if word.len() >= 2 && !word.chars().all(|c| c.is_ascii_digit()) {
            let _ = conn.execute(
                "INSERT INTO hotword_counts (word, count) VALUES (?1, 1)
                 ON CONFLICT(word) DO UPDATE SET count = count + 1",
                rusqlite::params![word],
            );
        }
    }
}

pub fn get_hotwords(conn: &Connection, limit: usize) -> Result<Vec<String>> {
    let mut s = conn.prepare("SELECT word FROM hotword_counts ORDER BY count DESC LIMIT ?1")?;
    let rows = s.query_map(rusqlite::params![limit as i64], |row| row.get::<_, String>(0))?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Increment the seen-count of a file extension that exists on disk but is
/// not in the extractor whitelist. `dir_id` is the scan root that saw it.
pub fn bump_unsupported_ext(conn: &Connection, ext: &str, dir_id: &str) {
    if ext.is_empty() {
        return;
    }
    let _ = conn.execute(
        "INSERT INTO unsupported_ext_stats (ext, count, dir_id, updated_at)
         VALUES (?1, 1, ?2, ?3)
         ON CONFLICT(ext) DO UPDATE SET count = count + 1, updated_at = ?3",
        rusqlite::params![ext, dir_id, chrono::Utc::now().timestamp()],
    );
}

/// Clear stats for one directory before a scan walk begins, so the table
/// reflects the current disk state after the run rather than cumulative totals.
pub fn reset_unsupported_ext(conn: &Connection, dir_id: &str) {
    let _ = conn.execute(
        "DELETE FROM unsupported_ext_stats WHERE dir_id = ?1",
        rusqlite::params![dir_id],
    );
}

pub struct UnsupportedExtStat {
    pub ext: String,
    pub count: u64,
    pub dir_id: String,
    pub updated_at: i64,
}

pub fn get_unsupported_ext_stats(conn: &Connection) -> Result<Vec<UnsupportedExtStat>> {
    let mut s = conn.prepare(
        "SELECT ext, count, dir_id, updated_at FROM unsupported_ext_stats ORDER BY count DESC",
    )?;
    let rows = s.query_map([], |row| {
        Ok(UnsupportedExtStat {
            ext: row.get(0)?,
            count: row.get(1)?,
            dir_id: row.get(2)?,
            updated_at: row.get(3)?,
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Store (or replace) a document's embedding vector as a little-endian f32 blob.
pub fn upsert_embedding(
    conn: &Connection,
    file_id: &str,
    vector: &[f32],
) -> Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(vector.len() * 4);
    for x in vector {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    conn.execute(
        "INSERT INTO doc_embeddings (file_id, dim, vector, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(file_id) DO UPDATE SET dim=excluded.dim, vector=excluded.vector, updated_at=excluded.updated_at",
        rusqlite::params![file_id, vector.len() as i64, bytes, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

/// Load every stored embedding as `(file_id, Vec<f32>)`. Used by the
/// semantic-search path to brute-force cosine over the whole corpus.
pub fn get_all_embeddings(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let mut s = conn.prepare(
        "SELECT file_id, dim, vector FROM doc_embeddings",
    )?;
    let rows = s.query_map([], |row| {
        let file_id: String = row.get(0)?;
        let dim: usize = row.get(1)?;
        let blob: Vec<u8> = row.get(2)?;
        let mut v = Vec::with_capacity(dim);
        for chunk in blob.chunks_exact(4) {
            v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok((file_id, v))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Remove a document's embedding (e.g. when the file is deleted or re-indexed).
pub fn delete_embedding(conn: &Connection, file_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM doc_embeddings WHERE file_id = ?1",
        rusqlite::params![file_id],
    )?;
    Ok(())
}

/// Number of stored embeddings (for the AI status panel).
pub fn count_embeddings(conn: &Connection) -> Result<u64> {
    conn.query_row("SELECT COUNT(*) FROM doc_embeddings", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
        .context("count embeddings")
}

/// Store (or replace) a document's LLM-generated summary.
pub fn upsert_summary(conn: &Connection, file_id: &str, summary: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO doc_summaries (file_id, summary, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(file_id) DO UPDATE SET summary=excluded.summary, updated_at=excluded.updated_at",
        rusqlite::params![file_id, summary, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

/// Load a stored summary by file id, if one exists.
pub fn get_summary(conn: &Connection, file_id: &str) -> Result<Option<String>> {
    let mut s = conn
        .prepare("SELECT summary FROM doc_summaries WHERE file_id = ?1")
        .context("prepare get_summary")?;
    let mut rows = s
        .query_map(rusqlite::params![file_id], |row| row.get::<_, String>(0))
        .context("query get_summary")?;
    match rows.next().transpose()? {
        Some(v) => Ok(Some(v)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); crate::db::run_migrations(&c).unwrap(); c }

    #[test]
    fn test_absorb_subdir_reroots_paths_and_dir_id() {
        let conn = db();
        // file_tracking.path is stored relative to the dir root (see
        // scanner to_relative), so sub-dir rows look like "foo.txt".
        let foo_id = upsert_file(&conn, "foo.txt", "sub", 1000, 10, Some("md5foo")).unwrap();
        upsert_file(&conn, "bar.pdf", "sub", 1000, 20, Some("md5bar")).unwrap();

        let n = absorb_subdir(&conn, "sub", "parent", "B").unwrap();
        assert_eq!(n, 2);

        let foo = get_file_by_id(&conn, &foo_id).unwrap().unwrap();
        assert_eq!(foo.path, "B/foo.txt");
        assert_eq!(foo.dir_id, "parent");
        assert_eq!(foo.md5.as_deref(), Some("md5foo"));
        assert_eq!(foo.indexed, 0);

        // old dir no longer holds the rows
        assert_eq!(get_files_by_dir(&conn, "sub").unwrap().len(), 0);
        assert_eq!(get_files_by_dir(&conn, "parent").unwrap().len(), 2);
    }

    #[test]
    fn test_upsert_and_retrieve() {
        let conn = db();
        let id = upsert_file(&conn, "/a.txt", "d1", 1000, 42, None).unwrap();
        let r = get_file_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(r.path, "/a.txt"); assert_eq!(r.size, 42); assert_eq!(r.status, "active");
    }

    #[test]
    fn test_upsert_updates_existing() {
        let conn = db();
        let id1 = upsert_file(&conn, "/a.txt", "d1", 1000, 42, None).unwrap();
        let id2 = upsert_file(&conn, "/a.txt", "d1", 2000, 99, Some("abc")).unwrap();
        assert_eq!(id1, id2);
        let r = get_file_by_path(&conn, "/a.txt").unwrap().unwrap();
        assert_eq!(r.mtime, 2000); assert_eq!(r.md5, Some("abc".into())); assert_eq!(r.indexed, 0);
    }

    #[test]
    fn test_mark_deleted() {
        let conn = db();
        let id = upsert_file(&conn, "/a.txt", "d1", 1000, 1, None).unwrap();
        mark_deleted(&conn, &id).unwrap();
        assert_eq!(get_file_by_id(&conn, &id).unwrap().unwrap().status, "deleted");
        assert!(mark_deleted(&conn, "/nope.txt").is_err());
    }

    #[test]
    fn test_files_by_dir() {
        let conn = db();
        upsert_file(&conn, "/d1/a.txt", "d1", 1000, 1, None).unwrap();
        upsert_file(&conn, "/d1/b.txt", "d1", 1000, 2, None).unwrap();
        upsert_file(&conn, "/d2/c.txt", "d2", 1000, 3, None).unwrap();
        assert_eq!(get_files_by_dir(&conn, "d1").unwrap().len(), 2);
        assert_eq!(get_files_by_dir(&conn, "d2").unwrap().len(), 1);
    }

    #[test]
    fn test_indexing_workflow() {
        let conn = db();
        upsert_file(&conn, "/a.txt", "d1", 1000, 1, None).unwrap();
        let id = upsert_file(&conn, "/b.txt", "d1", 1000, 2, None).unwrap();
        update_indexed(&conn, &id, Some("md5b")).unwrap();
        assert_eq!(get_files_needing_index(&conn, 10).unwrap().len(), 1);

        mark_failed(&conn, &id, "timeout").unwrap();
        let r = get_file_by_id(&conn, &id).unwrap().unwrap();
        assert_eq!(r.indexed, 2);
        assert_eq!(r.error_msg, Some("timeout".into()));
        assert_eq!(get_files_needing_index(&conn, 10).unwrap().len(), 2);
    }

    #[test]
    fn test_dead_content_lifecycle() {
        let conn = db();
        let id = upsert_file(&conn, "/d.doc", "d1", 1000, 1, None).unwrap();
        update_indexed(&conn, &id, Some("m1")).unwrap();
        assert_eq!(get_file_by_id(&conn, &id).unwrap().unwrap().dead_content, 0);

        // Mark dead -> flag set, excluded from auto-verify, in dead list.
        mark_dead_content(&conn, &id).unwrap();
        assert_eq!(get_file_by_id(&conn, &id).unwrap().unwrap().dead_content, 1);
        assert!(find_empty_content_files(&conn).unwrap().is_empty(), "dead file must be excluded from auto candidates");
        assert_eq!(find_dead_files(&conn).unwrap().len(), 1);

        // Successful re-index -> flag cleared (Q6-A).
        update_indexed(&conn, &id, Some("m2")).unwrap();
        assert_eq!(get_file_by_id(&conn, &id).unwrap().unwrap().dead_content, 0);
        assert!(find_dead_files(&conn).unwrap().is_empty());
    }

    #[test]
    fn test_find_empty_content_files_picks_truthy_empty() {
        let conn = db();
        // a: indexed with real text -> NOT a candidate
        let a = upsert_file(&conn, "/a.txt", "d1", 1000, 1, None).unwrap();
        update_indexed(&conn, &a, Some("m-a")).unwrap();
        store_content(&conn, "m-a", "real text", false, None).unwrap();

        // b: indexed with empty text (LO fake-success) -> candidate
        let b = upsert_file(&conn, "/b.txt", "d1", 1000, 1, None).unwrap();
        update_indexed(&conn, &b, Some("m-b")).unwrap();
        store_content(&conn, "m-b", "   ", false, None).unwrap();

        // c: indexed but NO content row -> candidate (missing content)
        let c = upsert_file(&conn, "/c.txt", "d1", 1000, 1, None).unwrap();
        update_indexed(&conn, &c, Some("m-c")).unwrap();

        let candidates = find_empty_content_files(&conn).unwrap();
        let ids: Vec<String> = candidates.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&b), "empty-text file should be candidate: {ids:?}");
        assert!(ids.contains(&c), "missing-content file should be candidate: {ids:?}");
        assert!(!ids.contains(&a), "real-text file must NOT be candidate: {ids:?}");
    }

    #[test]
    fn test_stats() {
        let conn = db();
        let a = upsert_file(&conn, "/a.txt", "d1", 1000, 1, None).unwrap();
        let b = upsert_file(&conn, "/b.txt", "d2", 1000, 2, None).unwrap();
        update_indexed(&conn, &a, Some("m_a")).unwrap();
        mark_failed(&conn, &b, "err").unwrap();
        let all = get_stats(&conn, None).unwrap();
        assert_eq!(all.total, 2);
        assert_eq!(all.indexed, 1);
        assert_eq!(all.pending, 0);
        assert_eq!(all.errors, 1);
        assert_eq!(get_stats(&conn, Some("d1")).unwrap().indexed, 1);
    }

    #[test]
    fn test_duplicates() {
        let conn = db();
        upsert_file(&conn, "/a.txt", "d1", 1000, 1, Some("dup")).unwrap();
        upsert_file(&conn, "/b.txt", "d1", 1000, 2, Some("dup")).unwrap();
        upsert_file(&conn, "/c.txt", "d1", 1000, 3, Some("uniq")).unwrap();
        let dups = get_duplicates(&conn).unwrap();
        assert_eq!(dups.len(), 1); assert_eq!(dups[0].count, 2);
    }

    #[test]
    fn test_unsupported_ext_stats_accumulate() {
        let conn = db();
        bump_unsupported_ext(&conn, "wps", "d1");
        bump_unsupported_ext(&conn, "wps", "d1");
        bump_unsupported_ext(&conn, "xyz", "d2");
        bump_unsupported_ext(&conn, "", "d1");
        let stats = get_unsupported_ext_stats(&conn).unwrap();
        assert_eq!(stats.len(), 2);
        let wps = stats.iter().find(|s| s.ext == "wps").unwrap();
        assert_eq!(wps.count, 2);
        assert_eq!(wps.dir_id, "d1");
        let xyz = stats.iter().find(|s| s.ext == "xyz").unwrap();
        assert_eq!(xyz.count, 1);
    }

    #[test]
    fn test_unsupported_ext_reset_per_dir() {
        let conn = db();
        bump_unsupported_ext(&conn, "wps", "d1");
        bump_unsupported_ext(&conn, "xyz", "d2");
        reset_unsupported_ext(&conn, "d1");
        let stats = get_unsupported_ext_stats(&conn).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].ext, "xyz");
        assert_eq!(stats[0].dir_id, "d2");
    }

    #[test]
    fn test_content_roundtrip() {
        let conn = db();
        store_content(&conn, "m1", "hello", false, None).unwrap();
        assert_eq!(get_content(&conn, "m1").unwrap().unwrap(), "hello");
        assert!(get_content(&conn, "mX").unwrap().is_none());
    }

    #[test]
    fn test_embedding_roundtrip() {
        let conn = db();
        upsert_embedding(&conn, "f1", &[0.5, -1.0, 2.0]).unwrap();
        upsert_embedding(&conn, "f2", &[1.0, 2.0]).unwrap();
        let all = get_all_embeddings(&conn).unwrap();
        assert_eq!(all.len(), 2);
        let v = all.iter().find(|(id, _)| id == "f1").unwrap();
        assert_eq!(v.1, vec![0.5, -1.0, 2.0]);

        upsert_embedding(&conn, "f1", &[9.0]).unwrap();
        let all = get_all_embeddings(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().find(|(id, _)| id == "f1").unwrap().1, vec![9.0]);

        delete_embedding(&conn, "f1").unwrap();
        assert_eq!(count_embeddings(&conn).unwrap(), 1);
    }

    #[test]
    fn test_hard_delete_releases_unique_path() {
        let conn = db();
        let dir_id = "d1";
        let id_a = upsert_file(&conn, "a.txt", dir_id, 1, 10, None).unwrap();
        let id_b = upsert_file(&conn, "b.txt", dir_id, 1, 10, None).unwrap();

        // Hard-deleting b frees its path so a's path can be moved onto it.
        hard_delete_file(&conn, &id_b).unwrap();
        assert!(get_file_by_path(&conn, "b.txt").unwrap().is_none());
        update_file_path(&conn, &id_a, "b.txt", dir_id).unwrap();

        let moved = get_file_by_path(&conn, "b.txt").unwrap().expect("a now owns b.txt");
        assert_eq!(moved.id, id_a);
    }
}
