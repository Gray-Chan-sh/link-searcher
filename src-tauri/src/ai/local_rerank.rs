//! Local cross-encoder reranker engine (tract-onnx + tokenizers).
//!
//! Scores `(query, passage)` pairs in one forward pass per pair — the same
//! pure-Rust ONNX stack as `local_embed`. Loads a BERT/XLM-R cross-encoder
//! (e.g. `bge-reranker-base`, `ms-marco-MiniLM-L-6-v2`) and applies sigmoid to
//! the single relevance logit.
//!
//! Reranking is opt-in and fail-open: any failure returns `None` and the caller
//! keeps the original ordering (`ai::rerank`).

use std::path::Path;
use std::sync::{Mutex, OnceLock, RwLock};

use tokenizers::EncodeInput;
use tract_onnx::prelude::*;

const MAX_SEQ_LEN: usize = 512;

/// Built-in local reranker models: `(dir_name, display_name)`. The directory
/// name is both the HuggingFace repo suffix (`Xenova/<dir_name>`) and the
/// suffix of the `local:<dir_name>` active-model id.
pub const RERANK_MODELS: &[(&str, &str)] = &[
    ("bge-reranker-base", "BGE-Reranker-Base (中英)"),
    ("ms-marco-MiniLM-L-6-v2", "MS-Marco MiniLM-L6 (英文/快)"),
];

struct LocalReranker {
    model: Mutex<TypedRunnableModel<TypedModel>>,
    tokenizer: Mutex<tokenizers::Tokenizer>,
    /// Number of ONNX inputs: 2 for XLM-R (no token_type_ids), 3 for BERT.
    inputs: usize,
}

static INSTANCE: OnceLock<RwLock<Option<LocalReranker>>> = OnceLock::new();

fn instance() -> &'static RwLock<Option<LocalReranker>> {
    INSTANCE.get_or_init(|| RwLock::new(None))
}

/// Check if the reranker files exist on disk (does NOT load them).
pub fn rerank_model_ready(data_dir: &Path, model_name: &str) -> bool {
    let dir = data_dir.join("models").join(model_name);
    dir.join("model.onnx").is_file() && dir.join("tokenizer.json").is_file()
}

/// Load the cross-encoder into the global singleton. Idempotent until
/// [`reset_local_reranker`] is called.
pub fn init_local_reranker(data_dir: &Path, model_name: &str) -> Result<(), String> {
    {
        let guard = instance().read().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            return Ok(());
        }
    }
    let reranker = build(data_dir, model_name)?;
    let mut guard = instance().write().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        *guard = Some(reranker);
    }
    Ok(())
}

/// Drop the loaded model so the next init reloads it (active model changed).
pub fn reset_local_reranker() {
    let mut guard = instance().write().unwrap_or_else(|p| p.into_inner());
    *guard = None;
}

/// Remove every built-in reranker directory except `keep` (best-effort).
pub fn prune_local_rerank_models_except(data_dir: &Path, keep: &str) {
    for &(name, _) in RERANK_MODELS {
        if name == keep {
            continue;
        }
        let dir = data_dir.join("models").join(name);
        if !dir.exists() {
            continue;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => log::info!("[RERANK] 已删除其它重排模型: {}", dir.display()),
            Err(e) => log::warn!("[RERANK] 删除重排模型目录失败 {}: {e}", dir.display()),
        }
    }
}

fn build(data_dir: &Path, model_name: &str) -> Result<LocalReranker, String> {
    let dir = data_dir.join("models").join(model_name);
    let onnx = dir.join("model.onnx");
    let tok_path = dir.join("tokenizer.json");

    if !onnx.is_file() || !tok_path.is_file() {
        return Err("重排模型文件不存在，请先下载模型".into());
    }

    let model = tract_onnx::onnx()
        .model_for_path(&onnx)
        .map_err(|e| format!("加载重排 ONNX 模型: {e}"))?
        .into_optimized()
        .map_err(|e| format!("优化重排模型: {e}"))?;
    let inputs = model.inputs.len();
    let runnable = model
        .into_runnable()
        .map_err(|e| format!("构建重排推理引擎: {e}"))?;

    let mut tokenizer = tokenizers::Tokenizer::from_file(&tok_path)
        .map_err(|e| format!("加载重排 tokenizer: {e}"))?;
    let _ = tokenizer.with_truncation(Some(tokenizers::TruncationParams {
        max_length: MAX_SEQ_LEN,
        ..Default::default()
    }));

    log::info!(
        "[RERANK] 本地重排引擎就绪: {} ({} inputs)",
        dir.display(),
        inputs
    );
    Ok(LocalReranker {
        model: Mutex::new(runnable),
        tokenizer: Mutex::new(tokenizer),
        inputs,
    })
}

/// Score each passage against the query (sigmoid of the cross-encoder logit).
/// Order matches `passages`. Returns `None` when the model is not loaded or
/// inference/tokenization fails.
pub fn rerank_local(query: &str, passages: &[String]) -> Option<Vec<f32>> {
    if passages.is_empty() {
        return Some(Vec::new());
    }
    let guard = instance().read().unwrap_or_else(|p| p.into_inner());
    let e = guard.as_ref()?;

    let n = passages.len();
    let encoded: Vec<(Vec<u32>, Vec<u32>, Vec<u32>)> = {
        let tok = e.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::with_capacity(n);
        for p in passages {
            let enc = tok
                .encode(EncodeInput::Dual(query.into(), p.as_str().into()), true)
                .map_err(|err| {
                    log::warn!("[RERANK] tokenize pair failed: {err}");
                })
                .ok()?;
            out.push((
                enc.get_ids().to_vec(),
                enc.get_attention_mask().to_vec(),
                enc.get_type_ids().to_vec(),
            ));
        }
        out
    };

    let max_len = encoded
        .iter()
        .map(|(ids, _, _)| ids.len())
        .max()
        .unwrap_or(1)
        .clamp(1, MAX_SEQ_LEN);
    let mut ids_buf = vec![0i64; n * max_len];
    let mut mask_buf = vec![0i64; n * max_len];
    let mut type_buf = vec![0i64; n * max_len];
    for (i, (ids, mask, types)) in encoded.iter().enumerate() {
        let off = i * max_len;
        let len = ids.len().min(max_len);
        for j in 0..len {
            ids_buf[off + j] = ids[j] as i64;
            mask_buf[off + j] = mask[j] as i64;
            type_buf[off + j] = types[j] as i64;
        }
    }

    let ids_t = Tensor::from_shape(&[n, max_len], &ids_buf).ok()?;
    let mask_t = Tensor::from_shape(&[n, max_len], &mask_buf).ok()?;
    let type_t = Tensor::from_shape(&[n, max_len], &type_buf).ok()?;

    let output = {
        let model = e.model.lock().unwrap_or_else(|p| p.into_inner());
        let result = if e.inputs == 2 {
            model.run(tvec![ids_t.into(), mask_t.into()])
        } else {
            model.run(tvec![ids_t.into(), mask_t.into(), type_t.into()])
        };
        match result {
            Ok(o) => o,
            Err(err) => {
                log::warn!("[RERANK] inference failed: {err}");
                return None;
            }
        }
    };

    let arr = output[0].to_array_view::<f32>().ok()?;
    let rank = arr.shape().len();
    let last = *arr.shape().last().unwrap_or(&1);
    let mut scores = Vec::with_capacity(n);
    for i in 0..n {
        let logit = if rank == 1 {
            arr[[i]]
        } else if last == 1 {
            arr[[i, 0]]
        } else {
            // Multi-class head: the last column is the relevance logit.
            arr[[i, last - 1]]
        };
        scores.push(sigmoid(logit));
    }
    Some(scores)
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
