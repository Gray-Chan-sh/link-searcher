//! Embedding-vector health: detect vectors whose dimension no longer matches
//! the active embedding model (e.g. after switching bge-small 512 → bge-large
//! 1024) and rebuild them without re-extracting content.
//!
//! A dimension mismatch is silent in the search path — `ai::cosine` returns
//! 0.0 when lengths differ (`ai/mod.rs`), so semantic search just returns
//! nothing. This module surfaces that state and offers a one-click re-embed.

use serde::Serialize;
use tauri::State;

use crate::config;
use crate::state::AppState;

#[derive(Serialize)]
pub struct EmbeddingConsistency {
    /// `provider:model` id currently configured for embeddings.
    pub active_model: String,
    /// Expected vector dimension when the active model is a known built-in
    /// local model; `None` for remote gateways (dimension unknown locally).
    pub expected_dim: Option<u32>,
    /// Distinct vector dimensions actually present in the corpus.
    pub stored_dims: Vec<u32>,
    /// True when every stored vector matches `expected_dim` (or when the active
    /// model's dimension is unknown, so there is nothing to compare).
    pub consistent: bool,
}

/// Report whether stored vectors match the active embedding model's
/// dimension. A mismatch means semantic search silently fails (cosine = 0).
#[tauri::command]
pub fn check_embedding_consistency(
    state: State<'_, AppState>,
) -> Result<EmbeddingConsistency, String> {
    let cfg = config::load_config();
    let active_model = cfg.active_embedding_model_id.clone();
    let expected_dim = if config::is_local_embedding_model(&active_model) {
        crate::ai::local_embed::local_model_dir_name(&active_model)
            .and_then(crate::commands::bge::local_model_dim)
    } else {
        None
    };

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let stored_dims = collect_stored_dims(&conn).map_err(|e| e.to_string())?;

    let consistent = match expected_dim {
        // Known expected dim: every stored vector must match (empty corpus is
        // trivially consistent).
        Some(dim) => stored_dims.iter().all(|&d| d == dim),
        // Remote / no local model: can't judge from dimensions alone.
        None => true,
    };

    Ok(EmbeddingConsistency {
        active_model,
        expected_dim,
        stored_dims,
        consistent,
    })
}

fn collect_stored_dims(conn: &rusqlite::Connection) -> anyhow::Result<Vec<u32>> {
    let mut dims = std::collections::BTreeSet::new();
    for table in ["doc_embeddings", "chunk_embeddings"] {
        let mut stmt = conn.prepare(&format!("SELECT DISTINCT dim FROM {table}"))?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        for d in rows {
            dims.insert(d? as u32);
        }
    }
    Ok(dims.into_iter().collect())
}

#[derive(Serialize)]
pub struct RebuildEmbeddingsReport {
    pub cleared_docs: u64,
    pub cleared_chunks: u64,
    pub embedded_docs: u64,
    pub embedded_chunks: u64,
    pub failed: u64,
}

/// Drop every stored vector and re-embed from cached `content_index` text
/// (no re-extraction / no OCR). Use after switching embedding models so the
/// whole corpus shares one vector space again.
#[tauri::command]
pub async fn rebuild_embeddings(
    state: State<'_, AppState>,
) -> Result<RebuildEmbeddingsReport, String> {
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || rebuild_embeddings_core(&db))
        .await
        .map_err(|e| format!("rebuild embeddings task panicked: {e}"))?
}

fn rebuild_embeddings_core(
    db: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
) -> Result<RebuildEmbeddingsReport, String> {
    let _guard = crate::state::TaskGuard::new("rebuild-embeddings");
    if !crate::ai::embedding_enabled() {
        return Err("嵌入模型不可用（未配置网关或本地模型未就绪），无法重建语义向量".into());
    }

    let conn = db.get().map_err(|e| format!("db error: {e}"))?;
    let cleared_docs = conn
        .execute("DELETE FROM doc_embeddings", [])
        .map_err(|e| e.to_string())? as u64;
    let cleared_chunks = conn
        .execute("DELETE FROM chunk_embeddings", [])
        .map_err(|e| e.to_string())? as u64;
    drop(conn);

    log::info!(
        "[AI] 语义向量重建: 清空 doc={cleared_docs} chunk={cleared_chunks}，开始重嵌"
    );

    let doc_report = super::embeddings::run_backfill_embeddings(db)?;
    let chunk_report = super::embeddings::run_backfill_chunk_embeddings(db)?;

    crate::state::push_task_brief(
        "rebuild-embeddings",
        format!(
            "语义向量重建完成: doc {} / chunk {}（失败 {}）",
            doc_report.processed,
            chunk_report.processed,
            doc_report.failed + chunk_report.failed
        ),
    );

    Ok(RebuildEmbeddingsReport {
        cleared_docs,
        cleared_chunks,
        embedded_docs: doc_report.processed,
        embedded_chunks: chunk_report.processed,
        failed: doc_report.failed + chunk_report.failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn collects_distinct_dims_across_vector_tables() {
        let conn = mem_conn();
        conn.execute(
            "INSERT INTO doc_embeddings (file_id, dim, vector, updated_at) VALUES ('f1', 512, x'00', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunk_embeddings (md5, chunk_index, dim, vector, updated_at) VALUES ('m1', 0, 1024, x'00', 0)",
            [],
        )
        .unwrap();

        assert_eq!(collect_stored_dims(&conn).unwrap(), vec![512, 1024]);
    }

    #[test]
    fn empty_corpus_has_no_dims() {
        let conn = mem_conn();
        assert!(collect_stored_dims(&conn).unwrap().is_empty());
    }
}
