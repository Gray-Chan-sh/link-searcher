//! File-tracking and content-index CRUD.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use uuid::Uuid;

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
    conn.query_row(
         "INSERT INTO file_tracking (id,path,dir_id,mtime,size,md5,status,indexed,error_msg,created_at,updated_at)
          VALUES (?1,?2,?3,?4,?5,?6,'active',0,NULL,?7,?7)
          ON CONFLICT(path) DO UPDATE SET
              mtime=excluded.mtime, size=excluded.size, md5=excluded.md5,
              status='active',
              indexed=CASE WHEN file_tracking.mtime!=excluded.mtime OR file_tracking.size!=excluded.size THEN 0 ELSE file_tracking.indexed END,
              error_msg=NULL, updated_at=excluded.updated_at
          RETURNING id",
        rusqlite::params![id, path, dir_id, mtime, size as i64, md5, now],
        |row| row.get::<_, String>(0),
    )
    .context("upsert_file failed")
}

pub fn mark_deleted(conn: &Connection, path: &str) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET status='deleted', updated_at=?1 WHERE path=?2",
            rusqlite::params![chrono::Utc::now().timestamp(), path],
        )
        .context("mark_deleted failed")?;
    if n == 0 {
        anyhow::bail!("file not found: {path}");
    }
    Ok(())
}

pub fn update_file_path(conn: &Connection, file_id: &str, new_path: &str, new_dir_id: &str) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET path=?1, dir_id=?2, updated_at=?3 WHERE id=?4",
            rusqlite::params![new_path, new_dir_id, chrono::Utc::now().timestamp(), file_id],
        )
        .context("update_file_path failed")?;
    if n == 0 {
        anyhow::bail!("file not found: {file_id}");
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
    })
}

const SEL: &str =
    "SELECT id,path,dir_id,mtime,size,md5,status,indexed,error_msg,created_at,updated_at FROM file_tracking";

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

pub fn get_files_by_dir(conn: &Connection, dir_id: &str) -> Result<Vec<FileRecord>> {
    let mut s = conn
        .prepare(&format!("{SEL} WHERE dir_id=?1 ORDER BY path"))
        .context("prepare get_files_by_dir")?;
    let rows = s.query_map(rusqlite::params![dir_id], row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect files_by_dir")
}

pub fn get_files_needing_index(conn: &Connection, limit: usize) -> Result<Vec<FileRecord>> {
    let mut s = conn
        .prepare(&format!("{SEL} WHERE indexed IN (0,2) ORDER BY updated_at ASC LIMIT ?1"))
        .context("prepare get_files_needing_index")?;
    let rows = s.query_map(rusqlite::params![limit as i64], row_to_record)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().context("collect needing_index")
}

pub fn update_indexed(conn: &Connection, file_id: &str, md5: Option<&str>) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE file_tracking SET indexed=1, md5=?1, error_msg=NULL, updated_at=?2 WHERE id=?3",
            rusqlite::params![md5, chrono::Utc::now().timestamp(), file_id],
        )
        .context("update_indexed failed")?;
    if n == 0 { anyhow::bail!("file not found: {file_id}"); }
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

fn run_stats_query(conn: &Connection, clause: &str, param: Option<&str>) -> Result<IndexStats> {
    let sql = format!(
        "SELECT COUNT(*) t, \
                COALESCE(SUM(CASE WHEN indexed=1 THEN 1 ELSE 0 END),0) i, \
                COALESCE(SUM(CASE WHEN indexed IN (0,2) THEN 1 ELSE 0 END),0) p, \
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
        Some(d) => run_stats_query(conn, "WHERE dir_id=?1", Some(d)),
        None => run_stats_query(conn, "", None),
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
    let mut s = conn.prepare("SELECT text_content FROM content_index WHERE md5=?1").context("prepare get_content")?;
    let mut rows = s.query_map(rusqlite::params![md5], |row| row.get::<_, String>(0))?;
    Ok(rows.next().transpose()?)
}

pub fn get_content_ocr_used(conn: &Connection, md5: &str) -> Result<bool> {
    let result = conn.query_row(
        "SELECT ocr_used FROM content_index WHERE md5 = ?1",
        rusqlite::params![md5],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(result != 0)
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
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_tracking ft \
         JOIN content_index ci ON ft.md5 = ci.md5 \
         WHERE ft.status = 'active' AND ci.ocr_used = 1",
        [],
        |row| row.get(0),
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); crate::db::run_migrations(&c).unwrap(); c }

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
        mark_deleted(&conn, "/a.txt").unwrap();
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
    fn test_stats() {
        let conn = db();
        let a = upsert_file(&conn, "/a.txt", "d1", 1000, 1, None).unwrap();
        let b = upsert_file(&conn, "/b.txt", "d2", 1000, 2, None).unwrap();
        update_indexed(&conn, &a, Some("m_a")).unwrap();
        mark_failed(&conn, &b, "err").unwrap();
        let all = get_stats(&conn, None).unwrap();
        assert_eq!(all.total, 2); assert_eq!(all.indexed, 1); assert_eq!(all.pending, 1); assert_eq!(all.errors, 1);
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
    fn test_content_roundtrip() {
        let conn = db();
        store_content(&conn, "m1", "hello", false, None).unwrap();
        assert_eq!(get_content(&conn, "m1").unwrap().unwrap(), "hello");
        assert!(get_content(&conn, "mX").unwrap().is_none());
    }
}
