//! Content index (dedup text cache) storage and quality-scoring persistence.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
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

pub fn store_content_with_quality(
    conn: &Connection,
    md5: &str,
    text: &str,
    ocr_used: bool,
    ocr_ms: Option<i64>,
    quality: Option<&crate::extractor::quality::QualityResult>,
) -> Result<()> {
    store_content_with_quality_and_reextract_count(conn, md5, text, ocr_used, ocr_ms, quality, 0)
}

pub fn store_content_with_quality_and_reextract_count(
    conn: &Connection,
    md5: &str,
    text: &str,
    ocr_used: bool,
    ocr_ms: Option<i64>,
    quality: Option<&crate::extractor::quality::QualityResult>,
    reextract_count: i64,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let (score, flags_json, confidence) = match quality {
        Some(q) => (
            Some(q.score as f64),
            crate::extractor::quality::flags_to_json(&q.flags),
            q.confidence,
        ),
        None => (None, "[]".to_string(), None),
    };
    conn.execute(
        "INSERT OR REPLACE INTO content_index \
         (md5,text_content,indexed_at,char_count,ocr_used,ocr_duration_ms,quality_score,quality_flags,reextract_count,mean_confidence) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![
            md5,
            text,
            now,
            text.chars().count() as i64,
            ocr_used as i64,
            ocr_ms,
            score,
            flags_json,
            reextract_count,
            confidence,
        ],
    )
    .context("store_content_with_quality failed")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct QualitySummary {
    pub green: i64,
    pub yellow: i64,
    pub red: i64,
    pub unevaluated: i64,
    pub total: i64,
}

pub fn get_quality_summary(conn: &Connection) -> Result<QualitySummary> {
    let row = conn.query_row(
        "SELECT \
            COALESCE(SUM(CASE WHEN quality_score > 0.75 THEN 1 ELSE 0 END), 0) AS green, \
            COALESCE(SUM(CASE WHEN quality_score >= 0.5 AND quality_score <= 0.75 THEN 1 ELSE 0 END), 0) AS yellow, \
            COALESCE(SUM(CASE WHEN quality_score < 0.5 THEN 1 ELSE 0 END), 0) AS red, \
            COALESCE(SUM(CASE WHEN quality_score IS NULL THEN 1 ELSE 0 END), 0) AS unevaluated, \
            COUNT(*) AS total \
         FROM content_index",
        [],
        |row| {
            Ok(QualitySummary {
                green: row.get(0)?,
                yellow: row.get(1)?,
                red: row.get(2)?,
                unevaluated: row.get(3)?,
                total: row.get(4)?,
            })
        },
    )
    .context("get_quality_summary failed")?;
    Ok(row)
}

#[derive(Debug, Clone, Serialize)]
pub struct LowQualityRow {
    pub md5: String,
    pub file_id: Option<String>,
    pub file_path: Option<String>,
    pub quality_score: Option<f64>,
    pub quality_flags: String,
    pub reextract_count: i64,
    pub char_count: i64,
    pub ocr_used: bool,
    pub preview: String,
}

pub fn get_low_quality_files(conn: &Connection, max_score: f64, limit: usize) -> Result<Vec<LowQualityRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT ci.md5, ft.id, ft.path, ci.quality_score, ci.quality_flags, \
                    ci.reextract_count, ci.char_count, ci.ocr_used, \
                    SUBSTR(ci.text_content, 1, 200) AS preview \
             FROM content_index ci \
             LEFT JOIN file_tracking ft ON ci.md5 = ft.md5 \
             WHERE ci.quality_score IS NOT NULL AND ci.quality_score < ?1 \
                   AND ci.reextract_count < 3 \
             ORDER BY ci.quality_score ASC \
             LIMIT ?2",
        )
        .context("prepare get_low_quality_files")?;
    let rows = stmt
        .query_map(rusqlite::params![max_score, limit as i64], |row| {
            Ok(LowQualityRow {
                md5: row.get(0)?,
                file_id: row.get(1)?,
                file_path: row.get(2)?,
                quality_score: row.get(3)?,
                quality_flags: row.get(4)?,
                reextract_count: row.get(5)?,
                char_count: row.get(6)?,
                ocr_used: row.get::<_, i64>(7)? != 0,
                preview: row.get(8)?,
            })
        })
        .context("query get_low_quality_files")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect get_low_quality_files")
}

pub fn get_content_quality(conn: &Connection, md5: &str) -> Result<Option<(Option<f64>, i64, Option<f64>)>> {
    match conn.query_row(
        "SELECT quality_score, reextract_count, mean_confidence FROM content_index WHERE md5 = ?1",
        rusqlite::params![md5],
        |row| Ok((
            row.get::<_, Option<f64>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<f64>>(2)?,
        )),
    ) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn update_reextract_state(conn: &Connection, md5: &str, count: i64, flags_json: &str) -> Result<()> {
    conn.execute(
        "UPDATE content_index SET reextract_count = ?1, quality_flags = ?2 WHERE md5 = ?3",
        rusqlite::params![count, flags_json, md5],
    )
    .context("update_reextract_state failed")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingQualityRow {
    pub md5: String,
    pub text_content: String,
    pub ocr_used: bool,
    pub rel_path: Option<String>,
    pub dir_id: Option<String>,
}

pub fn get_content_missing_quality(conn: &Connection, limit: usize) -> Result<Vec<MissingQualityRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT ci.md5, ci.text_content, ci.ocr_used, MAX(ft.path), MAX(ft.dir_id) \
             FROM content_index ci \
             LEFT JOIN file_tracking ft ON ci.md5 = ft.md5 \
             WHERE ci.quality_score IS NULL \
             GROUP BY ci.md5 \
             ORDER BY ci.indexed_at DESC \
             LIMIT ?1",
        )
        .context("prepare get_content_missing_quality")?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], |row| {
            Ok(MissingQualityRow {
                md5: row.get(0)?,
                text_content: row.get(1)?,
                ocr_used: row.get::<_, i64>(2)? != 0,
                rel_path: row.get(3)?,
                dir_id: row.get(4)?,
            })
        })
        .context("query get_content_missing_quality")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect get_content_missing_quality")
}

pub fn update_content_quality(conn: &Connection, md5: &str, score: f64, flags_json: &str) -> Result<()> {
    conn.execute(
        "UPDATE content_index SET quality_score=?1, quality_flags=?2 WHERE md5=?3",
        rusqlite::params![score, flags_json, md5],
    )
    .context("update_content_quality failed")?;
    Ok(())
}

pub fn count_reextractable(conn: &Connection, max_score: f64, cap: i64) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM content_index \
         WHERE quality_score IS NOT NULL AND quality_score < ?1 \
         AND reextract_count < ?2",
        rusqlite::params![max_score, cap],
        |row| row.get::<_, i64>(0),
    )
    .context("count_reextractable failed")?;
    Ok(count)
}

pub fn get_content(conn: &Connection, md5: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT text_content FROM content_index WHERE md5=?1")?;
    let mut rows = stmt.query_map(rusqlite::params![md5], |row| row.get::<_, String>(0))?;
    Ok(rows.next().transpose()?)
}

/// Batch `get_content`: one query for a list of md5s, returning entries in
/// input order (None when a hash has no cached text).
pub fn get_contents(conn: &Connection, md5s: &[String]) -> Result<Vec<Option<String>>> {
    if md5s.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", md5s.len()).collect::<Vec<_>>().join(",");
    let sql = format!("SELECT md5, text_content FROM content_index WHERE md5 IN ({placeholders})");
    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        md5s.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("query get_contents")?;
    let mut by_md5: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (md5, text) in rows.flatten() {
        by_md5.insert(md5, text);
    }
    Ok(md5s.iter().map(|m| by_md5.get(m).cloned()).collect())
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
