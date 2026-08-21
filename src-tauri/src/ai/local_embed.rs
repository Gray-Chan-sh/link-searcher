//! Local BGE embedding engine (tract-onnx + tokenizers).
//!
//! Loads `bge-small-zh-v1.5` for offline, privacy-first text embeddings
//! without any remote API dependency.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use tract_onnx::prelude::*;

const MAX_SEQ_LEN: usize = 512;
const QUERY_PREFIX: &str = "为这个句子生成表示以用于检索相关文章：";

struct LocalEmbedder {
    model: Mutex<TypedRunnableModel<TypedModel>>,
    tokenizer: Mutex<tokenizers::Tokenizer>,
}

static INSTANCE: OnceLock<LocalEmbedder> = OnceLock::new();

/// Check if BGE model files exist on disk (does NOT load them).
pub fn bge_model_ready(data_dir: &Path) -> bool {
    let dir = data_dir.join("models").join("bge-small-zh-v1.5");
    dir.join("model.onnx").is_file() && dir.join("tokenizer.json").is_file()
}

/// Load the BGE model and tokenizer into the global singleton.
/// Idempotent — subsequent calls are no-ops.
pub fn init_local_embedder(data_dir: &Path) -> Result<(), String> {
    if INSTANCE.get().is_some() {
        return Ok(());
    }
    let embedder = build(data_dir)?;
    let _ = INSTANCE.set(embedder);
    Ok(())
}

fn build(data_dir: &Path) -> Result<LocalEmbedder, String> {
    let dir = data_dir.join("models").join("bge-small-zh-v1.5");
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
        strategy: tokenizers::PaddingStrategy::Fixed(MAX_SEQ_LEN),
        ..Default::default()
    }));
    let _ = tokenizer.with_truncation(Some(tokenizers::TruncationParams {
        max_length: MAX_SEQ_LEN,
        ..Default::default()
    }));

    log::info!("[BGE] 本地嵌入引擎就绪: {}", dir.display());
    Ok(LocalEmbedder {
        model: Mutex::new(model),
        tokenizer: Mutex::new(tokenizer),
    })
}

/// Embed a batch of texts in passage mode (no instruction prefix).
/// Returns `None` per text when the model is not loaded or inference fails.
pub fn embed_batch_local(texts: &[String]) -> Vec<Option<Vec<f32>>> {
    let Some(e) = INSTANCE.get() else {
        return vec![None; texts.len()];
    };
    if texts.is_empty() {
        return vec![];
    }

    // Tokenize — lock tokenizer, release before model inference.
    let token_data = {
        let tok = e.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        let batch = match tok.encode_batch(texts.to_vec(), true) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[BGE] tokenize batch failed: {e}");
                return vec![None; texts.len()];
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

    // Build flat i64 buffers → [batch, MAX_SEQ_LEN] tensors.
    let n = texts.len();
    let mut ids_buf = vec![0i64; n * MAX_SEQ_LEN];
    let mut mask_buf = vec![0i64; n * MAX_SEQ_LEN];
    let mut type_buf = vec![0i64; n * MAX_SEQ_LEN];

    for (i, (ids, mask, types)) in token_data.iter().enumerate() {
        let off = i * MAX_SEQ_LEN;
        let len = ids.len().min(MAX_SEQ_LEN);
        for j in 0..len {
            ids_buf[off + j] = ids[j] as i64;
            mask_buf[off + j] = mask[j] as i64;
            type_buf[off + j] = types[j] as i64;
        }
    }

    let ids_t = match Tensor::from_shape(&[n, MAX_SEQ_LEN], &ids_buf) {
        Ok(t) => t,
        Err(_) => return vec![None; texts.len()],
    };
    let mask_t = match Tensor::from_shape(&[n, MAX_SEQ_LEN], &mask_buf) {
        Ok(t) => t,
        Err(_) => return vec![None; texts.len()],
    };
    let type_t = match Tensor::from_shape(&[n, MAX_SEQ_LEN], &type_buf) {
        Ok(t) => t,
        Err(_) => return vec![None; texts.len()],
    };

    // Inference.
    let output = {
        let model = e.model.lock().unwrap_or_else(|p| p.into_inner());
        match model.run(tvec![ids_t.into(), mask_t.into(), type_t.into()]) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("[BGE] inference failed: {e}");
                return vec![None; texts.len()];
            }
        }
    };

    // Extract CLS token (position 0) and L2-normalize.
    let arr = match output[0].to_array_view::<f32>() {
        Ok(a) => a,
        Err(_) => return vec![None; texts.len()],
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
