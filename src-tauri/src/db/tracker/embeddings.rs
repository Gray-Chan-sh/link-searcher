//! Embedding storage: document vectors, chunk vectors and QA-pair vectors
//! (little-endian f32 blobs).

use anyhow::{Context, Result};
use rusqlite::Connection;
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

// -- doc_qa_pairs --

/// Store a QA pair (question + its embedding vector) for a document.
/// Each question is a new row — no upsert, since one document can have
/// multiple distinct questions.
pub fn upsert_qa_pair(
    conn: &Connection,
    file_id: &str,
    question: &str,
    dim: usize,
    vector: &[u8],
) -> Result<()> {
    conn.execute(
        "INSERT INTO doc_qa_pairs (file_id, question, dim, vector, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![file_id, question, dim as i64, vector, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

/// Load every stored QA pair as `(file_id, Vec<f32>)`. Used by the
/// QA semantic-retrieval channel to brute-force cosine over all questions.
pub fn get_all_qa_vectors(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let mut s = conn.prepare(
        "SELECT file_id, dim, vector FROM doc_qa_pairs",
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

/// Remove all QA pairs for a document (e.g. when the file is deleted or re-indexed).
pub fn delete_qa_for_file(conn: &Connection, file_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM doc_qa_pairs WHERE file_id = ?1",
        rusqlite::params![file_id],
    )?;
    Ok(())
}

/// Count total QA pairs stored.
pub fn count_qa_pairs(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM doc_qa_pairs",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Store (or replace) a chunk embedding as a little-endian f32 blob, keyed by
/// (md5, chunk_index). Long documents get one vector per chunk so semantic
/// retrieval can hit detail-level content that a whole-file vector dilutes.
pub fn upsert_chunk_embedding(
    conn: &Connection,
    md5: &str,
    chunk_index: i64,
    vector: &[f32],
) -> Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(vector.len() * 4);
    for x in vector {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    conn.execute(
        "INSERT INTO chunk_embeddings (md5, chunk_index, dim, vector, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(md5, chunk_index) DO UPDATE SET
             dim=excluded.dim, vector=excluded.vector, updated_at=excluded.updated_at",
        rusqlite::params![md5, chunk_index, vector.len() as i64, bytes, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

/// Load every chunk embedding as `(md5, chunk_index, Vec<f32>)`. Used by the
/// chunk-vector scan to brute-force cosine over all chunk vectors.
pub fn get_all_chunk_embeddings(
    conn: &Connection,
) -> Result<Vec<(String, usize, Vec<f32>)>> {
    let mut s = conn.prepare(
        "SELECT md5, chunk_index, dim, vector FROM chunk_embeddings",
    )?;
    let rows = s.query_map([], |row| {
        let md5: String = row.get(0)?;
        let chunk_index: usize = row.get(1)?;
        let dim: usize = row.get(2)?;
        let blob: Vec<u8> = row.get(3)?;
        let mut v = Vec::with_capacity(dim);
        for chunk in blob.chunks_exact(4) {
            v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok((md5, chunk_index, v))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Load chunk embeddings restricted to a set of md5s as
/// `(md5, chunk_index, Vec<f32>)`. Used by the two-level retrieval funnel:
/// document-level coarse filter picks top documents first, then only those
/// documents' chunk vectors are cosine-scanned (not the whole corpus).
pub fn get_chunk_embeddings_by_md5s(
    conn: &Connection,
    md5s: &[String],
) -> Result<Vec<(String, usize, Vec<f32>)>> {
    if md5s.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", md5s.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT md5, chunk_index, dim, vector FROM chunk_embeddings WHERE md5 IN ({placeholders})"
    );
    let mut s = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        md5s.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
    let rows = s.query_map(param_refs.as_slice(), |row| {
        let md5: String = row.get(0)?;
        let chunk_index: usize = row.get(1)?;
        let dim: usize = row.get(2)?;
        let blob: Vec<u8> = row.get(3)?;
        let mut v = Vec::with_capacity(dim);
        for chunk in blob.chunks_exact(4) {
            v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        Ok((md5, chunk_index, v))
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

/// Remove all chunk embeddings for an md5 (e.g. when content is re-indexed).
pub fn delete_chunk_embeddings(conn: &Connection, md5: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM chunk_embeddings WHERE md5 = ?1",
        rusqlite::params![md5],
    )?;
    Ok(())
}

/// Number of stored chunk embeddings (for progress reporting).
pub fn count_chunk_embeddings(conn: &Connection) -> Result<u64> {
    conn.query_row("SELECT COUNT(*) FROM chunk_embeddings", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
        .context("count chunk embeddings")
}
