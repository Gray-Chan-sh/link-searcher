//! 历史向量来源核验（**只读**，不写库）。
//!
//! schema v14 之前写入的向量 `model_fp` 为空（来源未知），语义通道按指纹过滤后
//! 几乎看不到它们。2026-11-02 实测：doc 16418 条 / chunk 71110 条是空指纹，
//! 于是 `semantic_fuse` 拿"有向量的极少数"去替换 BM25 通道，检索行为随"交集是否
//! 为空"剧烈抖动（缺陷 A 已修：融合只重排不删；这里是缺陷 B 的核验手段）。
//!
//! 为什么不直接认领：v14 的全部意义就是"不拿旧空间向量去比新空间查询"。所以先
//! **抽样重算**：用当前模型把同一段正文重新嵌入，与库里的历史向量比余弦。
//! - 同模型同网关 → 余弦接近 1（默认阈值 0.95）
//! - 换了模型（即便同维度）→ 余弦明显更低
//!
//! 只有样本全部过阈值才建议认领；真正写库仍走 `claim_legacy_embeddings`（人工断言）。

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::Serialize;

/// 抽样条数：取最新写入的若干条（最可能来自当前管线）。
const SAMPLE_SIZE: usize = 6;
/// 太短的正文余弦不稳定，不适合做核验。
const MIN_TEXT_CHARS: usize = 200;
/// 默认判定阈值。同模型重算通常 > 0.99；换模型通常 < 0.9。
const DEFAULT_MIN_COS: f32 = 0.95;

#[derive(Debug, Serialize)]
pub struct LegacyVectorReport {
    pub active_fp: Option<String>,
    pub legacy_doc_rows: u64,
    pub legacy_chunk_rows: u64,
    pub sampled: usize,
    pub cosines: Vec<f32>,
    pub min_cosine: Option<f32>,
    pub mean_cosine: Option<f32>,
    pub threshold: f32,
    /// `claim` = 证据支持认领；`reject` = 来源不符或样本不足；`nothing_to_do` = 无历史向量。
    pub decision: String,
    pub notes: Vec<String>,
}

fn decode_vector(blob: &[u8], dim: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity(dim);
    for chunk in blob.chunks_exact(4) {
        v.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    v
}

fn threshold() -> f32 {
    std::env::var("LINK_SEARCHER_LEGACY_FP_MIN_COS")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| *v > 0.0 && *v <= 1.0)
        .unwrap_or(DEFAULT_MIN_COS)
}

fn count_legacy(conn: &rusqlite::Connection) -> Result<(u64, u64)> {
    let docs: u64 = conn
        .query_row("SELECT COUNT(*) FROM doc_embeddings WHERE model_fp = ''", [], |r| r.get(0))
        .context("count legacy doc_embeddings")?;
    let chunks: u64 = conn
        .query_row("SELECT COUNT(*) FROM chunk_embeddings WHERE model_fp = ''", [], |r| {
            r.get(0)
        })
        .context("count legacy chunk_embeddings")?;
    Ok((docs, chunks))
}

/// 抽样重算并给出结论。**不写库**。
pub fn sample(db: &Pool<SqliteConnectionManager>) -> Result<LegacyVectorReport, String> {
    let run = || -> Result<LegacyVectorReport> {
        let active_fp = crate::ai::identity::active_embedding_fp();
        let conn = db.get().context("db error")?;
        let (legacy_docs, legacy_chunks) = count_legacy(&conn)?;
        let mut report = LegacyVectorReport {
            active_fp: active_fp.clone(),
            legacy_doc_rows: legacy_docs,
            legacy_chunk_rows: legacy_chunks,
            sampled: 0,
            cosines: Vec::new(),
            min_cosine: None,
            mean_cosine: None,
            threshold: threshold(),
            decision: "nothing_to_do".into(),
            notes: Vec::new(),
        };
        if legacy_docs == 0 && legacy_chunks == 0 {
            report.notes.push("没有 model_fp 为空的历史向量".into());
            return Ok(report);
        }
        if active_fp.is_none() {
            report.decision = "reject".into();
            report.notes.push("当前没有激活的 embedding 模型，无法核验".into());
            return Ok(report);
        }

        let samples: Vec<(String, usize, Vec<u8>)> = {
            let mut stmt = conn.prepare(
                "SELECT e.md5, e.dim, e.vector
                 FROM doc_embeddings e
                 JOIN content_index c ON c.md5 = e.md5
                 WHERE e.model_fp = '' AND length(c.text_content) >= ?1
                 ORDER BY e.updated_at DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![MIN_TEXT_CHARS as i64, SAMPLE_SIZE as i64],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize, r.get::<_, Vec<u8>>(2)?)),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if samples.is_empty() {
            report.decision = "reject".into();
            report.notes.push("没有可用于核验的样本（正文太短或缺失）".into());
            return Ok(report);
        }

        let mut cosines: Vec<f32> = Vec::new();
        for (md5, dim, blob) in &samples {
            let text: Option<String> = conn
                .query_row(
                    "SELECT text_content FROM content_index WHERE md5 = ?1",
                    rusqlite::params![md5],
                    |r| r.get(0),
                )
                .ok();
            let Some(text) = text else {
                report.notes.push(format!("{md5}: 取不到正文，跳过"));
                continue;
            };
            let stored = decode_vector(blob, *dim);
            let Some(fresh) = crate::ai::cached_embed(&crate::ai::truncate_for_embed(&text)) else {
                report.notes.push(format!("{md5}: 重新嵌入失败（网关不可用？），跳过"));
                continue;
            };
            if fresh.len() != stored.len() {
                report.notes.push(format!(
                    "{md5}: 维度不一致（库内 {} vs 当前 {}）→ 来源不同",
                    stored.len(),
                    fresh.len()
                ));
                report.decision = "reject".into();
                report.sampled = cosines.len();
                report.cosines = cosines;
                return Ok(report);
            }
            cosines.push(crate::ai::cosine(&fresh, &stored));
        }

        report.sampled = cosines.len();
        if cosines.is_empty() {
            report.decision = "reject".into();
            report.notes.push("没有取到可核验的样本（网关/正文不可用）".into());
            return Ok(report);
        }
        let min = cosines.iter().copied().fold(f32::INFINITY, f32::min);
        let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
        report.min_cosine = Some(min);
        report.mean_cosine = Some(mean);
        report.cosines = cosines;

        if report.sampled < 3 || min < report.threshold {
            report.decision = "reject".into();
            report.notes.push(format!(
                "样本不足以确认同源（{} 条，min={:.4} < {}）→ 不建议认领",
                report.sampled, min, report.threshold
            ));
        } else {
            report.decision = "claim".into();
            report.notes.push(format!(
                "样本同源（{} 条，min={:.4} ≥ {}）→ 可以认领",
                report.sampled, min, report.threshold
            ));
        }
        Ok(report)
    };
    run().map_err(|e| format!("{e:#}"))
}
