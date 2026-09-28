//! Local BGE embedding engine (tract-onnx + tokenizers).
//!
//! Loads a local BGE model (small 512-dim or large 1024-dim) for offline,
//! privacy-first text embeddings without any remote API dependency.
//!
//! Inference is CPU-bound and (with tract) mostly single-threaded, so a **pool
//! of model replicas** is kept: concurrent callers each borrow a replica and
//! run in parallel, using more cores. Replicas are built lazily up to
//! [`parallelism`]; at rest the pool holds whatever has been built.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};

use tract_onnx::prelude::*;

const MAX_SEQ_LEN: usize = 512;
const QUERY_PREFIX: &str = "为这个句子生成表示以用于检索相关文章：";

struct LocalEmbedder {
    model: Mutex<TypedRunnableModel<TypedModel>>,
    tokenizer: Mutex<tokenizers::Tokenizer>,
}

struct PoolInner {
    idle: Vec<LocalEmbedder>,
    created: usize,
}

/// Pool of BGE model replicas. `acquire` hands out a replica (building a new
/// one if the pool can still grow), `release` returns it.
struct LocalEmbedPool {
    inner: Mutex<PoolInner>,
    cv: Condvar,
    max: usize,
    data_dir: PathBuf,
    model_name: String,
}

impl LocalEmbedPool {
    fn new(data_dir: PathBuf, model_name: String, max: usize) -> Self {
        Self {
            inner: Mutex::new(PoolInner { idle: Vec::new(), created: 0 }),
            cv: Condvar::new(),
            max: max.max(1),
            data_dir,
            model_name,
        }
    }

    fn acquire(&self) -> Result<LocalEmbedder, String> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(e) = inner.idle.pop() {
                return Ok(e);
            }
            if inner.created < self.max {
                // Build outside the lock so other threads can build too.
                inner.created += 1;
                drop(inner);
                match build(&self.data_dir, &self.model_name) {
                    Ok(e) => return Ok(e),
                    Err(err) => {
                        inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                        inner.created = inner.created.saturating_sub(1);
                        self.cv.notify_one();
                        return Err(err);
                    }
                }
            }
            // Pool is at capacity with everything in use — wait for a release.
            inner = self.cv.wait(inner).unwrap_or_else(|p| p.into_inner());
        }
    }

    fn release(&self, e: LocalEmbedder) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.idle.push(e);
        self.cv.notify_one();
    }
}

static INSTANCE: OnceLock<RwLock<Option<Arc<LocalEmbedPool>>>> = OnceLock::new();

fn instance() -> &'static RwLock<Option<Arc<LocalEmbedPool>>> {
    INSTANCE.get_or_init(|| RwLock::new(None))
}

/// Extract the local model directory name from `active_embedding_model_id`.
/// e.g. `"local:bge-small-zh-v1.5"` → `"bge-small-zh-v1.5"`.
pub fn local_model_dir_name(active_id: &str) -> Option<&str> {
    active_id.strip_prefix("local:")
}

/// Check if BGE model files exist on disk (does NOT load them).
pub fn bge_model_ready(data_dir: &Path, model_name: &str) -> bool {
    let dir = data_dir.join("models").join(model_name);
    dir.join("model.onnx").is_file() && dir.join("tokenizer.json").is_file()
}

/// Number of model replicas to use during batch embedding.
///
/// Precedence: env `LINK_SEARCHER_EMBED_PARALLELISM` > setting
/// `embed_parallelism` > auto (`cores / 2`, clamped to `[1, 4]`). A setting of
/// `0` means auto. Clamped to `[1, 16]`; each replica costs ~1 GB RAM, so keep
/// it modest on small-memory machines.
fn embed_parallelism() -> usize {
    if let Ok(raw) = std::env::var("LINK_SEARCHER_EMBED_PARALLELISM") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            return n.clamp(1, 16);
        }
    }
    let configured = {
        let data_dir = crate::config::load_config().data_dir;
        let db_path = data_dir.join("data.db");
        crate::db::get_pool(&db_path.to_string_lossy())
            .ok()
            .and_then(|pool| {
                pool.get().ok().and_then(|conn| {
                    conn.query_row(
                        "SELECT value FROM app_settings WHERE key = 'embed_parallelism'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .ok()
                })
            })
            .and_then(|v| v.trim().parse::<usize>().ok())
    };
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    match configured {
        Some(0) | None => (cores / 2).clamp(1, 4),
        Some(n) => n.clamp(1, 16),
    }
}

/// Initialize the local embedder pool. Idempotent. Replicas are **not** built
/// here — they are created lazily on first inference up to the parallelism
/// limit, so a single query only pays for one model load.
pub fn init_local_embedder(data_dir: &Path, model_name: &str) -> Result<(), String> {
    if instance().read().unwrap_or_else(|p| p.into_inner()).is_some() {
        return Ok(());
    }
    static INIT_LOCK: Mutex<()> = Mutex::new(());
    let _init = INIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if instance().read().unwrap_or_else(|p| p.into_inner()).is_some() {
        return Ok(());
    }
    if !bge_model_ready(data_dir, model_name) {
        return Err("BGE 模型文件不存在，请先下载模型".into());
    }
    let pool = Arc::new(LocalEmbedPool::new(
        data_dir.to_path_buf(),
        model_name.to_string(),
        embed_parallelism(),
    ));
    log::info!("[BGE] 本地嵌入池就绪: {} (并行度 {})", data_dir.join("models").join(model_name).display(), pool.max);
    let mut guard = instance().write().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(pool);
    }
    Ok(())
}

/// Drop the pool so the next [`init_local_embedder`] reloads the model. Called
/// when the active embedding model changes at runtime — otherwise the stale
/// pool would keep embedding with the previous model (wrong dimension).
/// In-flight replicas are returned to the old pool and dropped with it.
pub fn reset_local_embedder() {
    let mut guard = instance().write().unwrap_or_else(|p| p.into_inner());
    *guard = None;
}

/// Current configured parallelism (pool size), or the auto value if not loaded.
pub fn parallelism() -> usize {
    instance()
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|p| p.max)
        .unwrap_or_else(embed_parallelism)
}

fn build(data_dir: &Path, model_name: &str) -> Result<LocalEmbedder, String> {
    let dir = data_dir.join("models").join(model_name);
    let onnx = dir.join("model.onnx");
    let tok_path = dir.join("tokenizer.json");

    if !onnx.is_file() || !tok_path.is_file() {
        return Err("BGE 模型文件不存在，请先下载模型".into());
    }

    let model = tract_onnx::onnx()
        .model_for_path(&onnx)
        .map_err(|e| format!("加载 BGE ONNX 模型: {e}"))?
        .into_optimized()
        .map_err(|e| format!("优化 BGE 模型: {e}"))?
        .into_runnable()
        .map_err(|e| format!("构建 BGE 推理引擎: {e}"))?;

    let mut tokenizer = tokenizers::Tokenizer::from_file(&tok_path)
        .map_err(|e| format!("加载 BGE tokenizer: {e}"))?;
    let _ = tokenizer.with_padding(Some(tokenizers::PaddingParams {
        strategy: tokenizers::PaddingStrategy::BatchLongest,
        ..Default::default()
    }));
    let _ = tokenizer.with_truncation(Some(tokenizers::TruncationParams {
        max_length: MAX_SEQ_LEN,
        ..Default::default()
    }));

    log::info!("[BGE] 本地嵌入副本就绪: {}", dir.display());
    Ok(LocalEmbedder {
        model: Mutex::new(model),
        tokenizer: Mutex::new(tokenizer),
    })
}

/// Borrow a replica from the pool, run `f`, then return it.
fn with_replica<R>(
    pool: &LocalEmbedPool,
    f: impl FnOnce(&LocalEmbedder) -> R,
) -> Result<R, String> {
    let e = pool.acquire()?;
    let r = f(&e);
    pool.release(e);
    Ok(r)
}

/// Embed a batch of texts in passage mode (no instruction prefix).
/// Returns `None` per text when the model is not loaded or inference fails.
pub fn embed_batch_local(texts: &[String]) -> Vec<Option<Vec<f32>>> {
    let n = texts.len();
    if n == 0 {
        return vec![];
    }
    let pool = match instance().read().unwrap_or_else(|p| p.into_inner()).clone() {
        Some(p) => p,
        None => return vec![None; n],
    };
    match with_replica(&pool, |e| embed_batch_with(e, texts)) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[BGE] {e}");
            vec![None; n]
        }
    }
}

/// Pause background batch embedding while an interactive chat turn is in
/// flight, freeing CPU (and the reserved replica) for the user's retrieval.
///
/// Bounded so a chatty user can't starve indexing forever. Granularity is one
/// batch: inference already running can't be interrupted, so the *first* turn
/// that starts mid-batch may still overlap it; once that batch finishes, the
/// backfill stays parked for as long as turns keep arriving.
fn yield_to_chat() {
    use std::time::{Duration, Instant};
    const MAX_YIELD: Duration = Duration::from_secs(120);
    if !crate::ai::chat_in_flight() {
        return;
    }
    let start = Instant::now();
    while crate::ai::chat_in_flight() && start.elapsed() < MAX_YIELD {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Embed many texts by slicing into `batch_size` batches and running up to
/// [`parallelism`] batches concurrently (one replica each). Output order matches
/// the input.
///
/// One replica is **reserved for interactive queries**: batch work runs on at
/// most `max - 1` replicas, so a chat turn always finds a warm replica even
/// while a long backfill is running (interactive embedding uses `acquire`,
/// which may take the reserved one).
pub fn embed_batched_local(texts: &[String], batch_size: usize) -> Vec<Option<Vec<f32>>> {
    let n = texts.len();
    if n == 0 {
        return vec![];
    }
    let pool = match instance().read().unwrap_or_else(|p| p.into_inner()).clone() {
        Some(p) => p,
        None => return vec![None; n],
    };
    let batch_size = batch_size.max(1);
    // Truncate up front so every batch sees the same text the remote path would.
    let texts: Vec<String> = texts.iter().map(|t| crate::ai::truncate_for_embed(t)).collect();
    let chunk_count = texts.len().div_ceil(batch_size);
    let out: Mutex<Vec<Option<Vec<f32>>>> = Mutex::new(vec![None; n]);
    let next = AtomicUsize::new(0);
    const RESERVED_FOR_QUERIES: usize = 1;
    let workers = pool
        .max
        .saturating_sub(RESERVED_FOR_QUERIES)
        .max(1)
        .min(chunk_count);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= chunk_count {
                    break;
                }
                // Yield to an in-flight chat turn before starting this batch.
                yield_to_chat();
                let base = i * batch_size;
                let end = (base + batch_size).min(texts.len());
                let chunk = &texts[base..end];
                match with_replica(&pool, |e| embed_batch_with(e, chunk)) {
                    Ok(vals) => {
                        let mut guard = out.lock().unwrap_or_else(|p| p.into_inner());
                        for (j, v) in vals.into_iter().enumerate() {
                            guard[base + j] = v;
                        }
                    }
                    Err(e) => log::warn!("[BGE] batch {i} failed: {e}"),
                }
            });
        }
    });

    out.into_inner().unwrap_or_else(|p| p.into_inner())
}

/// Single-batch inference against a specific replica.
fn embed_batch_with(e: &LocalEmbedder, texts: &[String]) -> Vec<Option<Vec<f32>>> {
    let n = texts.len();
    if n == 0 {
        return vec![];
    }

    let token_data = {
        let tok = e.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        let batch = match tok.encode_batch(texts.to_vec(), true) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[BGE] tokenize batch failed: {e}");
                return vec![None; n];
            }
        };
        batch
            .into_iter()
            .map(|enc| {
                (
                    enc.get_ids().to_vec(),
                    enc.get_attention_mask().to_vec(),
                    enc.get_type_ids().to_vec(),
                )
            })
            .collect::<Vec<_>>()
    };

    // Dynamic padding: `BatchLongest` pads every sequence to the *batch's*
    // longest (≤ MAX_SEQ_LEN), so a batch of short docs costs far less than a
    // fixed 512-token forward. Attention is O(L²), so this is the main lever.
    let lens: Vec<usize> = token_data.iter().map(|(ids, _, _)| ids.len()).collect();
    let seq_len = batch_seq_len(&lens);
    let mut ids_buf = vec![0i64; n * seq_len];
    let mut mask_buf = vec![0i64; n * seq_len];
    let mut type_buf = vec![0i64; n * seq_len];

    for (i, (ids, mask, types)) in token_data.iter().enumerate() {
        let off = i * seq_len;
        let len = ids.len().min(seq_len);
        for j in 0..len {
            ids_buf[off + j] = ids[j] as i64;
            mask_buf[off + j] = mask[j] as i64;
            type_buf[off + j] = types[j] as i64;
        }
    }

    let ids_t = match Tensor::from_shape(&[n, seq_len], &ids_buf) {
        Ok(t) => t,
        Err(_) => return vec![None; n],
    };
    let mask_t = match Tensor::from_shape(&[n, seq_len], &mask_buf) {
        Ok(t) => t,
        Err(_) => return vec![None; n],
    };
    let type_t = match Tensor::from_shape(&[n, seq_len], &type_buf) {
        Ok(t) => t,
        Err(_) => return vec![None; n],
    };

    let output = {
        let model = e.model.lock().unwrap_or_else(|p| p.into_inner());
        match model.run(tvec![ids_t.into(), mask_t.into(), type_t.into()]) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("[BGE] inference failed: {e}");
                return vec![None; n];
            }
        }
    };

    let arr = match output[0].to_array_view::<f32>() {
        Ok(a) => a,
        Err(_) => return vec![None; n],
    };
    let hidden_dim = arr.shape()[2];
    let mut result = Vec::with_capacity(n);
    for i in 0..n {
        let mut vec: Vec<f32> = (0..hidden_dim).map(|d| arr[[i, 0, d]]).collect();
        l2_normalize(&mut vec);
        result.push(Some(vec));
    }
    result
}

/// Embed a single query in query mode (prepends BGE instruction prefix).
pub fn embed_query_local(query: &str) -> Option<Vec<f32>> {
    let full = format!("{QUERY_PREFIX}{query}");
    let batch = embed_batch_local(&[full]);
    batch.into_iter().next().flatten()
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        let inv = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv;
        }
    }
}

/// Batch tensor sequence length: the longest encoded length, clamped to
/// `[1, MAX_SEQ_LEN]`. Drives dynamic padding (see [`embed_batch_with`]).
fn batch_seq_len(lens: &[usize]) -> usize {
    lens.iter().copied().max().unwrap_or(1).clamp(1, MAX_SEQ_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_seq_len_uses_longest_and_clamps() {
        assert_eq!(batch_seq_len(&[]), 1);
        assert_eq!(batch_seq_len(&[3, 7, 5]), 7);
        assert_eq!(batch_seq_len(&[10_000]), MAX_SEQ_LEN);
        assert_eq!(batch_seq_len(&[0]), 1);
    }

    #[test]
    fn init_missing_model_errors_without_loading() {
        reset_local_embedder();
        let dir = std::env::temp_dir().join("ls_embed_missing_model_test");
        assert!(init_local_embedder(&dir, "no-such-model").is_err());
        // Failed init must leave the singleton unloaded (not a poisoned half-state).
        assert!(instance().read().unwrap_or_else(|p| p.into_inner()).is_none());
    }
}
