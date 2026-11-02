//! Embedding-vector health: 判断库里存的向量是否**仍属于当前模型的空间**，
//! 并提供"不重新提取内容、只重算向量"的整库重建。
//!
//! ## 两个独立的判据
//!
//! 1. **维度**（旧判据，v14 前唯一）：`ai::cosine` 在长度不等时返回 0.0，
//!    所以换到不同维度的模型后语义检索**静默返回空**。这条只能发现"512 → 1024"
//!    这种换法。
//! 2. **模型指纹**（v14 起）：`doc_embeddings.model_fp` / `chunk_embeddings.model_fp`
//!    记录生成该向量的模型身份（见 `ai/identity.rs`）。同维度的两个模型
//!    （`bge-m3` 在两个网关上 —— 2026-09-29 的实际事故形态）维度判据完全无感，
//!    指纹判据能直接看出来。
//!
//! 两者都要报：维度不一致说明**连余弦都没法算**；指纹不一致说明**余弦能算但
//! 算出来的数没有意义**。后者更隐蔽，所以是本模块的主判据。

use serde::Serialize;
use tauri::State;

use crate::config;
use crate::state::AppState;

/// 语料的向量空间状态。
///
/// 五态是**从库里的事实直接推出来的**，不依赖"用户当时切了什么"：
///
/// | 状态 | 事实 | 语义检索 |
/// |------|------|----------|
/// | `Empty` | 库中没有任何向量 | 空（不需要重建） |
/// | `Current` | 全部向量都属于当前模型指纹 | 正常 |
/// | `Stale` | 全部属于**另一个**单一指纹 | **拒绝**（换过模型没重建） |
/// | `Mixed` | 存在 ≥2 个非空指纹 | **拒绝**（两个空间混在一起） |
/// | `Legacy` | 存在 `model_fp=''` 的行（v14 之前写入） | 允许 + 告警 |
///
/// `Legacy` 的取舍：`''` 的含义是**未知**，不是"旧模型"。v14 升级后的第一
/// 次启动必然是这个状态（16421 份内容全是 10 月写的），把它当成"不一致"会让
/// 所有升级用户一启动就被要求重建整库。所以它不阻断，只告警，并提供一个人工
/// 认领（"我就是用当前模型建的"）—— 认领是**断言**，不是核验。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CorpusState {
    Empty,
    Current,
    Stale,
    Mixed,
    Legacy,
}

impl CorpusState {
    /// 该状态下是否必须**拒绝**语义检索。
    ///
    /// `Stale` / `Mixed` 下任何余弦都是在两个不同空间的向量之间算的 —— 数值
    /// 有大小、但排序没有意义，比"返回空"更有害（用户会相信那个排序）。
    pub fn blocks_semantic(self) -> bool {
        matches!(self, CorpusState::Stale | CorpusState::Mixed)
    }
}

/// 一个指纹在库里的行数。
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ModelFpCount {
    /// 16 位十六进制；`""` = legacy（来源未知）。
    pub model_fp: String,
    pub doc_rows: u64,
    pub chunk_rows: u64,
}

#[derive(Serialize)]
pub struct EmbeddingConsistency {
    /// `provider:model` id currently configured for embeddings.
    pub active_model: String,
    /// Expected vector dimension when the active model is a known built-in
    /// local model; `None` for remote gateways (dimension unknown locally).
    pub expected_dim: Option<u32>,
    /// Distinct vector dimensions actually present in the corpus.
    pub stored_dims: Vec<u32>,
    /// 当前模型的身份指纹（`None` = 配置解析不出模型，语义检索本就关闭）。
    pub active_fp: Option<String>,
    /// 五态结论，见 [`CorpusState`]。
    pub corpus_state: CorpusState,
    /// 按指纹分组的行数 —— "混了哪几个空间"的直接证据。
    pub fp_counts: Vec<ModelFpCount>,
    /// `model_fp=''` 的行数（doc + chunk）。
    pub legacy_rows: u64,
    /// 当前指纹下仍缺向量的内容数（待回填规模）。
    pub pending_docs: u64,
    /// 当前指纹下仍缺向量的块数。
    pub pending_chunks: u64,
    /// True when every stored vector matches `expected_dim`（维度层面）**且**
    /// 语料没有混空间。保留这个名字是为了兼容旧前端。
    pub consistent: bool,
    /// 必须重建才能恢复语义检索（`Stale` / `Mixed`）。
    pub rebuild_required: bool,
}

/// Report whether stored vectors still belong to the active embedding model's
/// vector space. Dimension mismatch means cosine can't even be computed;
/// fingerprint mismatch means it can, but the numbers are meaningless.
#[tauri::command]
pub fn check_embedding_consistency(
    state: State<'_, AppState>,
) -> Result<EmbeddingConsistency, String> {
    let cfg = config::load_config();
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    consistency_report(&conn, &cfg).map_err(|e| e.to_string())
}

/// 供[`super::super::config::set_active_model`]的切换门禁使用：
/// 返回 `(语料状态, 待回填 doc 数, 待回填 chunk 数)`。
///
/// 与 [`consistency_report`] 共用同一套事实与同一套五态规则 —— 门禁和体检
/// 不能各自算一套，否则会出现"切换时说不一致、体检里说一致"（用户只会信其中一个）。
pub fn corpus_snapshot(
    conn: &rusqlite::Connection,
    cfg: &config::AppConfig,
) -> anyhow::Result<(CorpusState, u64, u64)> {
    let facts = corpus_facts(conn, cfg)?;
    Ok((facts.state, facts.pending_docs, facts.pending_chunks))
}

/// 从库里读出的、与"向量空间身份"有关的全部事实。
struct CorpusFacts {
    state: CorpusState,
    fp_counts: Vec<ModelFpCount>,
    legacy_rows: u64,
    pending_docs: u64,
    pending_chunks: u64,
}

fn corpus_facts(
    conn: &rusqlite::Connection,
    cfg: &config::AppConfig,
) -> anyhow::Result<CorpusFacts> {
    let active_fp = crate::ai::identity::model_fp(cfg, &cfg.active_embedding_model_id);
    let fp_counts: Vec<ModelFpCount> = crate::db::tracker::count_by_model_fp(conn)?
        .into_iter()
        .map(|(model_fp, doc_rows, chunk_rows)| ModelFpCount { model_fp, doc_rows, chunk_rows })
        .filter(|c| c.doc_rows + c.chunk_rows > 0)
        .collect();

    let state = classify_corpus(&fp_counts, active_fp.as_deref());
    let legacy_rows = fp_counts
        .iter()
        .filter(|c| c.model_fp.is_empty())
        .map(|c| c.doc_rows + c.chunk_rows)
        .sum();

    let (pending_docs, pending_chunks) = match active_fp.as_deref() {
        Some(fp) => (count_pending_docs(conn, fp)?, count_pending_chunks(conn, fp)?),
        // 没有可用的 embedding 模型：语义检索本就关闭，回填规模没有意义。
        None => (0, 0),
    };

    Ok(CorpusFacts {
        state,
        fp_counts,
        legacy_rows,
        pending_docs,
        pending_chunks,
    })
}

/// [`check_embedding_consistency`] 的本体（不依赖 Tauri `State`，便于单测）。
pub(super) fn consistency_report(
    conn: &rusqlite::Connection,
    cfg: &config::AppConfig,
) -> anyhow::Result<EmbeddingConsistency> {
    let active_model = cfg.active_embedding_model_id.clone();
    let expected_dim = if config::is_local_embedding_model(&active_model) {
        crate::ai::local_embed::local_model_dir_name(&active_model)
            .and_then(crate::commands::bge::local_model_dim)
    } else {
        None
    };
    let active_fp = crate::ai::identity::model_fp(cfg, &active_model);

    let stored_dims = collect_stored_dims(conn)?;
    let facts = corpus_facts(conn, cfg)?;

    let dims_ok = match expected_dim {
        // Known expected dim: every stored vector must match (empty corpus is
        // trivially consistent).
        Some(dim) => stored_dims.iter().all(|&d| d == dim),
        // Remote / no local model: can't judge from dimensions alone.
        None => true,
    };
    let rebuild_required = facts.state.blocks_semantic() || !dims_ok;

    Ok(EmbeddingConsistency {
        active_model,
        expected_dim,
        stored_dims,
        active_fp,
        corpus_state: facts.state,
        fp_counts: facts.fp_counts,
        legacy_rows: facts.legacy_rows,
        pending_docs: facts.pending_docs,
        pending_chunks: facts.pending_chunks,
        consistent: dims_ok && !facts.state.blocks_semantic(),
        rebuild_required,
    })
}

/// 快速判定：库里是否存在**属于别的模型空间**的向量。
///
/// 与 [`corpus_snapshot`] 的区别是代价：这里只要"有还是没有"，用 `LIMIT 1`
/// 的 `EXISTS`；完整聚合要扫全表（98k 行）。语义检索每次搜索都会问一次，
/// 所以必须便宜。
///
/// `''`（未知来源）**不算**外来：`Legacy` 不阻断，见 [`CorpusState`]。
pub fn has_foreign_vectors(
    conn: &rusqlite::Connection,
    active_fp: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(fp) = active_fp else {
        // 没有可用模型 = 没有可比的空间。
        return Ok(true);
    };
    for table in ["doc_embeddings", "chunk_embeddings"] {
        let exists: i64 = conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {table} WHERE model_fp <> '' AND model_fp <> ?1)"
            ),
            rusqlite::params![fp],
            |r| r.get(0),
        )?;
        if exists != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 由指纹分布推出五态。纯函数，规则见 [`CorpusState`]。
fn classify_corpus(counts: &[ModelFpCount], active_fp: Option<&str>) -> CorpusState {
    if counts.is_empty() {
        return CorpusState::Empty;
    }
    // 有未知来源的行就先报未知 —— "未知 ≠ 不一致"，但也不能假装一致。
    if counts.iter().any(|c| c.model_fp.is_empty()) {
        return CorpusState::Legacy;
    }
    let distinct: std::collections::BTreeSet<&str> =
        counts.iter().map(|c| c.model_fp.as_str()).collect();
    match (active_fp, distinct.len()) {
        (Some(fp), 1) if distinct.contains(fp) => CorpusState::Current,
        // 全部属于另一个单一空间 = 换过模型没重建。
        (_, 1) => CorpusState::Stale,
        // 两个以上空间 = 混用。
        _ => CorpusState::Mixed,
    }
}

/// 缺少**当前指纹**向量的内容数 —— 它衡量的是**重建代价**，不是回填队列。
///
/// 自动回填（`run_backfill_embeddings`）只补"没有行"的内容；这里把"属于旧空间的
/// 行"也算进来，是因为对话框/横幅要告诉用户"重建要重嵌多少"。两者口径不同是
/// 刻意的：一个是"要不要干活"，一个是"干活有多大"。
fn count_pending_docs(conn: &rusqlite::Connection, model_fp: &str) -> anyhow::Result<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM file_tracking ft
         JOIN content_index ci ON ft.md5 = ci.md5
         LEFT JOIN doc_embeddings e ON e.md5 = ft.md5
         WHERE ft.indexed = 1 AND ft.status = 'active'
           AND (e.md5 IS NULL
                OR e.model_fp <> ?1
                OR e.updated_at < ci.indexed_at)",
        rusqlite::params![model_fp],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as u64)
    .map_err(|e| anyhow::anyhow!("count pending doc embeddings: {e}"))
}

/// 同上（chunk 侧）：不属于当前指纹的块都算进"重建代价"。
fn count_pending_chunks(conn: &rusqlite::Connection, model_fp: &str) -> anyhow::Result<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM doc_chunks dc
         LEFT JOIN chunk_embeddings ce
           ON ce.md5 = dc.md5 AND ce.chunk_index = dc.chunk_index
         WHERE ce.md5 IS NULL OR ce.model_fp <> ?1",
        rusqlite::params![model_fp],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as u64)
    .map_err(|e| anyhow::anyhow!("count pending chunk embeddings: {e}"))
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

#[derive(Serialize)]
pub struct ClaimLegacyReport {
    /// 被认领到的指纹（当前模型）。
    pub model_fp: String,
    pub claimed_docs: u64,
    pub claimed_chunks: u64,
}

/// 把 `model_fp=''` 的行（v14 之前写入，来源未知）**认领**为当前模型。
///
/// ## 认领前先核验（2026-11-02）
///
/// 在此之前这是纯人工断言：用户说"这些就是我刚才用这个模型建的"，系统照写。
/// 现在改为**先抽样重算**（[`super::legacy_vectors::sample`]）：用当前模型把同一段
/// 正文重新嵌入，与库内历史向量比余弦。样本不同源时**拒绝认领**并返回证据 ——
/// 除非调用方显式传 `force=true`（仍然只动 `''` 的行）。
///
/// - 只动 `''` 的行，绝不覆盖已有指纹（否则"认领"就成了"把所有空间都改成当前
///   空间"的静默修复，把 `Mixed` 洗成 `Current`）。
/// - 返回并展示真实行数，不显示"已核验"这类措辞。
/// - 之后若语义检索结果明显不对，唯一的办法仍是重建。
#[tauri::command]
pub async fn claim_legacy_embeddings(
    state: State<'_, AppState>,
    force: Option<bool>,
) -> Result<ClaimLegacyReport, String> {
    let db_pool = state.db.clone();
    let force = force.unwrap_or(false);
    tokio::task::spawn_blocking(move || claim_legacy_core(&db_pool, force))
        .await
        .map_err(|e| format!("claim legacy task panicked: {e}"))?
}

fn claim_legacy_core(
    db_pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    force: bool,
) -> Result<ClaimLegacyReport, String> {
    let cfg = config::load_config();
    let model_fp = crate::ai::identity::model_fp(&cfg, &cfg.active_embedding_model_id)
        .ok_or_else(|| "当前没有可用的 embedding 模型，无法认领".to_string())?;

    // 先核验：抽样重算必须与当前模型同源。
    let evidence = super::legacy_vectors::sample(db_pool)?;
    if evidence.decision != "claim" && !force {
        return Err(format!(
            "拒绝认领：{}（样本 {} 条，min_cosine={}，阈值 {}）。如确认要认领，请传 force=true",
            evidence.notes.join("；"),
            evidence.sampled,
            evidence.min_cosine.map(|v| format!("{v:.4}")).unwrap_or_else(|| "-".into()),
            evidence.threshold
        ));
    }
    if evidence.decision != "claim" && force {
        log::warn!("[AI] 认领历史向量：核验未通过（{}），但调用方强制认领", evidence.notes.join("；"));
    }

    let conn = db_pool.get().map_err(|e| format!("db error: {e}"))?;
    let (claimed_docs, claimed_chunks) =
        claim_legacy_rows(&conn, &model_fp).map_err(|e| e.to_string())?;

    log::info!(
        "[AI] 已把 {claimed_docs} 条 doc / {claimed_chunks} 条 chunk 的未知来源向量认领为 {model_fp}（核验 min_cosine={}）",
        evidence.min_cosine.map(|v| format!("{v:.4}")).unwrap_or_else(|| "-".into())
    );
    Ok(ClaimLegacyReport {
        model_fp,
        claimed_docs,
        claimed_chunks,
    })
}

fn claim_legacy_rows(
    conn: &rusqlite::Connection,
    model_fp: &str,
) -> anyhow::Result<(u64, u64)> {
    let tx = conn.unchecked_transaction()?;
    let docs = tx.execute(
        "UPDATE doc_embeddings SET model_fp = ?1 WHERE model_fp = ''",
        rusqlite::params![model_fp],
    )? as u64;
    let chunks = tx.execute(
        "UPDATE chunk_embeddings SET model_fp = ?1 WHERE model_fp = ''",
        rusqlite::params![model_fp],
    )? as u64;
    tx.commit()?;
    Ok((docs, chunks))
}

/// 重建进行中的标记（单飞）。
///
/// 并发重建的后果不是"多跑一遍"：两条流都会 `DELETE FROM *_embeddings` 再各自
/// 回填，交错之下最终库里可能只剩一半向量，而两边的报告都显示"成功"。
static REBUILD_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 重建期间置位、结束时（含失败/early return）自动复位。
struct RebuildFlag;

impl Drop for RebuildFlag {
    fn drop(&mut self) {
        REBUILD_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
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
    // 已经在重建就**拒绝**，而不是排队再来一遍：先一次会把所有向量删掉并
    // 开始重嵌，后一次也会删掉前一次刚写进去的向量（然后重嵌一遍），
    // 两边的进度与报告都失去意义。
    if REBUILD_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("语义向量重建已在进行中，请等待完成（索引状态页可看进度）".into());
    }
    let _running = RebuildFlag;

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
            "INSERT INTO doc_embeddings (md5, dim, vector, updated_at) VALUES ('f1', 512, x'00', 0)",
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

    fn fp_count(fp: &str) -> ModelFpCount {
        ModelFpCount {
            model_fp: fp.to_string(),
            doc_rows: 1,
            chunk_rows: 1,
        }
    }

    #[test]
    fn classify_covers_the_five_states() {
        // 无向量
        assert_eq!(classify_corpus(&[], Some("cur")), CorpusState::Empty);
        // 全部属于当前模型
        assert_eq!(classify_corpus(&[fp_count("cur")], Some("cur")), CorpusState::Current);
        // 全部属于另一个单一空间
        assert_eq!(classify_corpus(&[fp_count("old")], Some("cur")), CorpusState::Stale);
        // 两个空间混在一起
        assert_eq!(
            classify_corpus(&[fp_count("cur"), fp_count("old")], Some("cur")),
            CorpusState::Mixed
        );
        // 未知来源（v14 之前写入）优先报未知，而不是"不一致"
        assert_eq!(classify_corpus(&[fp_count("")], Some("cur")), CorpusState::Legacy);
        assert_eq!(
            classify_corpus(&[fp_count(""), fp_count("cur")], Some("cur")),
            CorpusState::Legacy
        );
        // 没有可用模型：有数据但认不出空间
        assert_eq!(classify_corpus(&[fp_count("old")], None), CorpusState::Stale);
        assert_eq!(
            classify_corpus(&[fp_count("a"), fp_count("b")], None),
            CorpusState::Mixed
        );
    }

    /// `Stale` / `Mixed` 必须阻断语义检索：这两种状态下任何余弦都是在两个不同
    /// 空间的向量之间算的，数值有大小但排序没有意义。
    #[test]
    fn only_stale_and_mixed_block_semantic() {
        assert!(CorpusState::Stale.blocks_semantic());
        assert!(CorpusState::Mixed.blocks_semantic());
        for s in [CorpusState::Empty, CorpusState::Current, CorpusState::Legacy] {
            assert!(!s.blocks_semantic(), "{s:?} 不该阻断");
        }
    }

    fn cfg_with_endpoint(base_url: &str) -> config::AppConfig {
        use crate::config::{AppConfig, ProviderConfig};
        let mut cfg = AppConfig::default();
        cfg.providers = vec![ProviderConfig {
            id: "p1".to_string(),
            name: "p1".to_string(),
            base_url: base_url.to_string(),
            api_key: String::new(),
            models: Vec::new(),
        }];
        cfg.active_embedding_model_id = "p1:bge-m3".to_string();
        cfg
    }

    fn insert_doc_vec(conn: &rusqlite::Connection, md5: &str, model_fp: &str) {
        conn.execute(
            "INSERT INTO doc_embeddings (md5, dim, vector, updated_at, model_fp) VALUES (?1, 4, x'00000000', 0, ?2)",
            rusqlite::params![md5, model_fp],
        )
        .unwrap();
    }

    /// 端到端：同样的维度、只是换了网关 —— 旧实现会判"一致"（维度相同），
    /// 新实现必须认出是两个空间。
    #[test]
    fn report_distinguishes_same_dim_other_endpoint() {
        let conn = mem_conn();
        let cfg_a = cfg_with_endpoint("http://a:8000/v1");
        let cfg_b = cfg_with_endpoint("http://b:8000/v1");
        let fp_a = crate::ai::identity::model_fp(&cfg_a, "p1:bge-m3").unwrap();
        let fp_b = crate::ai::identity::model_fp(&cfg_b, "p1:bge-m3").unwrap();
        assert_ne!(fp_a, fp_b);

        insert_doc_vec(&conn, "m1", &fp_a);

        let own = consistency_report(&conn, &cfg_a).unwrap();
        assert_eq!(own.corpus_state, CorpusState::Current);
        assert!(own.consistent);
        assert!(!own.rebuild_required);

        let other = consistency_report(&conn, &cfg_b).unwrap();
        assert_eq!(other.corpus_state, CorpusState::Stale);
        assert!(!other.consistent);
        assert!(other.rebuild_required, "同维不同网关必须要求重建");
        assert_eq!(other.stored_dims, vec![4]);
        assert_eq!(other.fp_counts.len(), 1);
    }

    #[test]
    fn report_treats_legacy_rows_as_unknown_not_inconsistent() {
        let conn = mem_conn();
        let cfg = cfg_with_endpoint("http://a:8000/v1");
        insert_doc_vec(&conn, "m1", "");

        let r = consistency_report(&conn, &cfg).unwrap();
        assert_eq!(r.corpus_state, CorpusState::Legacy);
        assert_eq!(r.legacy_rows, 1);
        assert!(
            !r.rebuild_required,
            "未知来源不阻断（否则每个升级用户一启动就被要求重建整库）"
        );
    }

    #[test]
    fn claim_only_touches_legacy_rows() {
        let conn = mem_conn();
        insert_doc_vec(&conn, "m1", "");
        insert_doc_vec(&conn, "m2", "keep-me");

        let (docs, chunks) = claim_legacy_rows(&conn, "cur").unwrap();
        assert_eq!((docs, chunks), (1, 0), "只动 '' 的行");

        let counts = crate::db::tracker::count_by_model_fp(&conn).unwrap();
        assert_eq!(
            counts,
            vec![("cur".to_string(), 1, 0), ("keep-me".to_string(), 1, 0)],
            "已有指纹绝不能被覆盖（否则认领就成了把 Mixed 洗成 Current）"
        );
    }

    #[test]
    fn report_counts_pending_docs_for_active_fp_only() {
        let conn = mem_conn();
        let cfg = cfg_with_endpoint("http://a:8000/v1");
        let fp = crate::ai::identity::model_fp(&cfg, "p1:bge-m3").unwrap();

        conn.execute(
            "INSERT INTO dir_config (id,path,recursive,created_at,updated_at) VALUES ('d1','/a',1,0,0)",
            [],
        )
        .unwrap();
        for (id, md5) in [("f1", "m1"), ("f2", "m2")] {
            conn.execute(
                "INSERT INTO file_tracking (id,path,dir_id,mtime,size,md5,status,indexed,created_at,updated_at) \
                 VALUES (?1,?1,'d1',0,0,?2,'active',1,0,0)",
                rusqlite::params![id, md5],
            )
            .unwrap();
            crate::db::tracker::store_content(&conn, md5, "body", false, None).unwrap();
        }
        insert_doc_vec(&conn, "m1", "");

        let r = consistency_report(&conn, &cfg).unwrap();
        // m1 是 legacy（不算当前指纹）、m2 完全没有向量 → 两份都待回填。
        assert_eq!(r.pending_docs, 2);
        let _ = fp;
    }
}
