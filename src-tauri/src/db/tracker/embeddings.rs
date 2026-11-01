//! Embedding storage: document vectors and chunk vectors (little-endian
//! f32 blobs).

use anyhow::{Context, Result};
use rusqlite::Connection;
/// Store (or replace) a **content's** document embedding vector as a
/// little-endian f32 blob. Keyed by `md5`: duplicate files (same content) share
/// one row, exactly like `chunk_embeddings`.
///
/// `model_fp` 是生成该向量的模型指纹（`ai::identity`）。写入时盖章，读取时按它
/// 过滤 —— 这是"切换模型后不拿旧空间向量去比新空间查询"的唯一依据。
pub fn upsert_embedding(
    conn: &Connection,
    md5: &str,
    vector: &[f32],
    model_fp: &str,
) -> Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(vector.len() * 4);
    for x in vector {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    conn.execute(
        "INSERT INTO doc_embeddings (md5, dim, vector, updated_at, model_fp) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(md5) DO UPDATE SET dim=excluded.dim, vector=excluded.vector, updated_at=excluded.updated_at, model_fp=excluded.model_fp",
        rusqlite::params![md5, vector.len() as i64, bytes, chrono::Utc::now().timestamp(), model_fp],
    )?;
    Ok(())
}

/// Load every stored embedding as `(file_id, Vec<f32>)`. Used by the
/// semantic-search path to brute-force cosine over the whole corpus.
///
/// 表按 **md5** 存（同内容一份向量），这里 `JOIN file_tracking` 展开成
/// 「每个活跃文件一行」—— 调用方（`search.rs` / `retrieval.rs` / `ai/mod.rs`）
/// 一直按 file_id 做 BM25 名单与候选的对应，保持这个契约就不必改检索语义。
/// 顺带一个好处：已删除文件的向量结构性地不再出现在候选里（此前要靠孤儿清理）。
///
/// **每次调用都把整表读进内存，且没有缓存** —— 这是 2026-10-18 审计里**故意保留**
/// 的取舍：单条**查询**向量有 LRU 缓存（`ai::QUERY_EMBED_CACHE`），但"全库向量快照"
/// 的失效点散落在 upsert / delete / 目录清除 / 重建 / 迁移 / 孤儿清理里，漏掉任一处
/// 都会**静默返回按过期向量排的序**（错误结果比慢结果糟得多）。要加缓存必须先定死
/// 失效契约（或换 ANN 索引）。
///
/// 这里只做**度量**（行数 / 字节 / 耗时，debug 级），好判断"多大规模开始痛"。
///
/// `model_fp`：`Some(fp)` 只取该指纹的行（检索路径必须用这个）；`None` 不过滤
/// （仅诊断 / 统计用）。切换模型后旧指纹的行**结构性地**不参与检索 —— 不再靠
/// "维度恰好不同"来碰运气。
pub fn get_all_embeddings(
    conn: &Connection,
    model_fp: Option<&str>,
) -> Result<Vec<(String, Vec<f32>)>> {
    let started = std::time::Instant::now();
    let mut s = conn.prepare(
        "SELECT ft.id, e.dim, e.vector
         FROM doc_embeddings e
         JOIN file_tracking ft ON ft.md5 = e.md5
         WHERE ft.status = 'active'
           AND (?1 IS NULL OR e.model_fp = ?1)",
    )?;
    let mut bytes: usize = 0;
    let rows = s.query_map(rusqlite::params![model_fp], |row| {
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
        let (file_id, v) = row?;
        bytes += v.len() * 4;
        result.push((file_id, v));
    }
    log::debug!(
        "[EMB] 全库文档向量载入（无缓存，整表反序列化）: {} 条, {:.2} MB, {} ms",
        result.len(),
        bytes as f64 / 1_048_576.0,
        started.elapsed().as_millis()
    );
    Ok(result)
}

/// Remove a content's document embedding (md5 key), **unless another active
/// file still references that md5**.
///
/// 表改为 md5 键后，"某个文件没了/内容变了"不再等于"这份内容的向量该删"：
/// 内容按 md5 在目录间共享，无条件删会连带清掉别的目录里相同内容文件的向量
/// （`chunk_embeddings` 早先就踩过这个坑，见 `IndexerService::delete_file`）。
///
/// `exclude_file_id` 用于"这个文件正在删/正在改内容"的场景 —— 把它自己排除后
/// 再判断；传 `""` 表示不排除任何行（文件已被标记为非活跃时用）。
///
/// 返回实际删除的行数（0 = 因为仍被引用而保留）。
pub fn delete_embedding_unreferenced(
    conn: &Connection,
    md5: &str,
    exclude_file_id: &str,
) -> Result<u64> {
    let others = super::count_other_active_files_with_md5(conn, md5, exclude_file_id)?;
    if others > 0 {
        return Ok(0);
    }
    let n = conn.execute(
        "DELETE FROM doc_embeddings WHERE md5 = ?1",
        rusqlite::params![md5],
    )?;
    Ok(n as u64)
}

/// Store (or replace) a chunk embedding as a little-endian f32 blob, keyed by
/// (md5, chunk_index). Long documents get one vector per chunk so semantic
/// retrieval can hit detail-level content that a whole-file vector dilutes.
pub fn upsert_chunk_embedding(
    conn: &Connection,
    md5: &str,
    chunk_index: i64,
    vector: &[f32],
    model_fp: &str,
) -> Result<()> {
    let mut bytes: Vec<u8> = Vec::with_capacity(vector.len() * 4);
    for x in vector {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    conn.execute(
        "INSERT INTO chunk_embeddings (md5, chunk_index, dim, vector, updated_at, model_fp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(md5, chunk_index) DO UPDATE SET
             dim=excluded.dim, vector=excluded.vector, updated_at=excluded.updated_at,
             model_fp=excluded.model_fp",
        rusqlite::params![md5, chunk_index, vector.len() as i64, bytes, chrono::Utc::now().timestamp(), model_fp],
    )?;
    Ok(())
}

/// Load every chunk embedding as `(md5, chunk_index, Vec<f32>)`. Used by the
/// chunk-vector scan to brute-force cosine over all chunk vectors.
///
/// 同样**无缓存、整表载入**（原因见 [`get_all_embeddings`]）—— 这份比文档级更贵
/// （长文档每篇多个 chunk），所以别在热路径上反复调用；度量见下面的 debug 日志。
pub fn get_all_chunk_embeddings(
    conn: &Connection,
    model_fp: Option<&str>,
) -> Result<Vec<(String, usize, Vec<f32>)>> {
    let started = std::time::Instant::now();
    let mut s = conn.prepare(
        "SELECT md5, chunk_index, dim, vector FROM chunk_embeddings
         WHERE (?1 IS NULL OR model_fp = ?1)",
    )?;
    let mut bytes: usize = 0;
    let rows = s.query_map(rusqlite::params![model_fp], |row| {
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
        let (md5, chunk_index, v) = row?;
        bytes += v.len() * 4;
        result.push((md5, chunk_index, v));
    }
    log::debug!(
        "[EMB] 全库 chunk 向量载入（无缓存，整表反序列化）: {} 条, {:.2} MB, {} ms",
        result.len(),
        bytes as f64 / 1_048_576.0,
        started.elapsed().as_millis()
    );
    Ok(result)
}

/// Load chunk embeddings restricted to a set of md5s as
/// `(md5, chunk_index, Vec<f32>)`. Used by the two-level retrieval funnel:
/// document-level coarse filter picks top documents first, then only those
/// documents' chunk vectors are cosine-scanned (not the whole corpus).
pub fn get_chunk_embeddings_by_md5s(
    conn: &Connection,
    md5s: &[String],
    model_fp: &str,
) -> Result<Vec<(String, usize, Vec<f32>)>> {
    if md5s.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", md5s.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT md5, chunk_index, dim, vector FROM chunk_embeddings
         WHERE md5 IN ({placeholders}) AND model_fp = ?"
    );
    let mut s = conn.prepare(&sql)?;
    let mut param_refs: Vec<&dyn rusqlite::types::ToSql> =
        md5s.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
    param_refs.push(&model_fp);
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

/// 按模型指纹统计向量行数：`(model_fp, doc 行数, chunk 行数)`。
///
/// 这是"语料是否混了多个向量空间"的**直接事实**（v14 之前只能靠日志推断）。
/// 空串代表 legacy（v14 之前写入、来源未知）。按指纹升序返回，便于稳定展示。
pub fn count_by_model_fp(conn: &Connection) -> Result<Vec<(String, u64, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT fp,
                SUM(doc_n)   AS doc_n,
                SUM(chunk_n) AS chunk_n
         FROM (
             SELECT model_fp AS fp, COUNT(*) AS doc_n, 0 AS chunk_n FROM doc_embeddings GROUP BY model_fp
             UNION ALL
             SELECT model_fp AS fp, 0 AS doc_n, COUNT(*) AS chunk_n FROM chunk_embeddings GROUP BY model_fp
         )
         GROUP BY fp
         ORDER BY fp",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)? as u64,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
