//! ONNX Runtime 后端（可选）：同样的模型/同样的前后处理，只把推理换成 ORT。
//!
//! 为什么加这个后端：实测（`D:\test\_probe\out\bench_matrix.md`，同形状同机）
//! `ort` 的 det 约 75ms、rec(8×48×320) 约 11ms，而 tract 0.20 是 1589ms / 667ms
//! —— 差 3~20 倍，主要差在 GEMM 内核。代价是每平台要带一个 ~13MB 的
//! `onnxruntime` 动态库。
//!
//! 设计要点：
//! - **动态加载**（`load-dynamic`）：编译期不链接 ORT，DLL 缺失时应用照常启动，
//!   自动回退到 tract 后端（见 `paddleocr::engine_backend`）。
//! - **复用** `pure-onnx-ocr` 的检测后处理（DB 后处理/unclip/scaler）、字典与 CTC 解码，
//!   只替换"跑模型"这一步 —— 前后处理与 tract 后端逐字节一致，质量不会因为换后端而变。
//! - DLL 查找顺序：`ORT_DYLIB_PATH` → `<data_dir>/runtime/onnxruntime/` → 可执行文件目录 → 系统 PATH。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::{DynamicImage, GenericImageView};
use ndarray::{Array2, Array3};
use pure_onnx_ocr::{
    DetPolygonScaler, DetPolygonScalerConfig, DetPolygonUnclipper,
    DetPolygonUnclipperConfig, DetPostProcessor, DetPostProcessorConfig, DetPreProcessor,
    DetPreProcessorConfig, RecDictionary, RecInferenceOutput, RecPostProcessor,
    RecPostProcessorConfig,
};

use super::pipeline::{
    bounds, bucket_for, natural_width, prepare_crop, Recognized, RunOutput, StageTimings,
    REC_BATCH, REC_HEIGHT, REC_WIDTH_BUCKETS,
};

/// ORT 绑定的 API 版本（对应 onnxruntime 1.31.x）。DLL 版本不符时加载会失败 → 回退 tract。
const ORT_API_HINT: &str = "1.31.x";

/// 动态库文件名（按平台）。
fn dylib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// 找一个可加载的 ORT 动态库。找不到 = 这个后端不可用（正常回退）。
pub fn locate_dylib(data_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(dd) = data_dir {
        for cand in [
            dd.join("runtime").join("onnxruntime").join(dylib_name()),
            dd.join("runtime").join("onnxruntime").join("lib").join(dylib_name()),
        ] {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let cand = dir.join(dylib_name());
        if cand.is_file() {
            return Some(cand);
        }
    }
    // 让 ORT 自己按系统搜索路径找一次
    None
}

/// 是否把 ORT 作为后端（`auto` 时看 DLL 在不在）。
pub fn available(data_dir: Option<&Path>) -> bool {
    locate_dylib(data_dir).is_some()
}

pub fn api_hint() -> &'static str {
    ORT_API_HINT
}

pub struct OrtPipeline {
    // 必须持有 Environment（Session 借用了它）
    _env: Arc<ort::environment::Environment>,
    det: Mutex<ort::session::Session>,
    rec: Mutex<ort::session::Session>,
    det_input: String,
    rec_input: String,
    det_pre: DetPreProcessor,
    det_post: DetPostProcessor,
    unclip: DetPolygonUnclipper,
    scaler: DetPolygonScaler,
    rec_post: RecPostProcessor,
}

impl OrtPipeline {
    pub fn load(
        det_model: &Path,
        rec_model: &Path,
        dictionary: &Path,
        limit_side_len: u32,
        data_dir: Option<&Path>,
    ) -> Result<Self, String> {
        let dylib = locate_dylib(data_dir)
            .ok_or_else(|| format!("未找到 {}（可放到 <data_dir>/runtime/onnxruntime/）", dylib_name()))?;

        let env = Arc::new(
            ort::init_from(&dylib)
                .map_err(|e| format!("加载 {} 失败（需要 ORT {ORT_API_HINT}）: {e}", dylib.display()))?
                .with_name("link-searcher")
                .build()
                .map_err(|e| format!("初始化 ONNX Runtime 失败: {e}"))?,
        );

        let build = |p: &Path| -> Result<ort::session::Session, String> {
            ort::session::Session::builder(&env)
                .map_err(|e| format!("创建 ORT 会话失败: {e}"))?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
                .map_err(|e| format!("配置 ORT 会话失败: {e}"))?
                .with_intra_threads(1)
                .map_err(|e| format!("配置 ORT 线程失败: {e}"))?
                .commit_from_file(p)
                .map_err(|e| format!("加载模型 {} 失败: {e}", p.display()))
        };
        let det = build(det_model)?;
        let rec = build(rec_model)?;
        let det_input = det
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| "检测模型没有输入".to_string())?;
        let rec_input = rec
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| "识别模型没有输入".to_string())?;

        let dict = RecDictionary::from_path(dictionary)
            .map_err(|e| format!("加载识别字典失败: {e}"))?;

        Ok(Self {
            _env: env,
            det: Mutex::new(det),
            rec: Mutex::new(rec),
            det_input,
            rec_input,
            det_pre: DetPreProcessor::new(DetPreProcessorConfig { limit_side_len }),
            det_post: DetPostProcessor::new(DetPostProcessorConfig::default()),
            unclip: DetPolygonUnclipper::new(DetPolygonUnclipperConfig::default()),
            scaler: DetPolygonScaler::new(DetPolygonScalerConfig::default()),
            rec_post: RecPostProcessor::new(Arc::new(dict), RecPostProcessorConfig::default()),
        })
    }

    pub fn run_from_path(&self, path: &Path) -> Result<RunOutput, String> {
        let start = Instant::now();
        let image = image::open(path)
            .map_err(|e| format!("无法解码图片 {}: {}", path.display(), e))?;
        let decode = start.elapsed();
        let mut out = self.run_from_image(&image)?;
        out.timings.image_decode = decode;
        out.timings.total = start.elapsed();
        Ok(out)
    }

    pub fn run_from_image(&self, image: &DynamicImage) -> Result<RunOutput, String> {
        let start = Instant::now();
        let dims = image.dimensions();

        // ---- 检测 ----
        let t = Instant::now();
        let pre = self
            .det_pre
            .process(image)
            .map_err(|e| format!("检测预处理失败: {e}"))?;
        let shape: Vec<usize> = pre.tensor.shape().to_vec();
        let view = pre
            .tensor
            .to_array_view::<f32>()
            .map_err(|e| format!("检测输入张量读取失败: {e}"))?;
        let data: Vec<f32> = view
            .as_slice()
            .ok_or_else(|| "检测输入张量非连续".to_string())?
            .to_vec();
        let det_pre_t = t.elapsed();

        let t = Instant::now();
        let prob: Array2<f32> = {
            let input = ort::value::Tensor::from_array((shape.clone(), data))
                .map_err(|e| format!("构造检测输入失败: {e}"))?;
            let mut det = self.det.lock().unwrap_or_else(|e| e.into_inner());
            let out = det
                .run(ort::inputs![self.det_input.as_str() => input])
                .map_err(|e| format!("检测推理失败: {e}"))?;
            let (oshape, odata) = out[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("检测输出读取失败: {e}"))?;
            let dims: Vec<usize> = oshape.iter().map(|d| *d as usize).collect();
            let (h, w) = match dims.len() {
                4 => (dims[2], dims[3]),
                3 => (dims[1], dims[2]),
                _ => return Err(format!("检测输出维度异常: {dims:?}")),
            };
            Array2::from_shape_vec((h, w), odata.to_vec())
                .map_err(|e| format!("检测输出重塑失败: {e}"))?
        };
        let det_inf_t = t.elapsed();

        let t = Instant::now();
        let contours = self
            .det_post
            .process_probability_map(&prob)
            .map_err(|e| format!("检测后处理失败: {e}"))?;
        let polygons = self.unclip.unclip_contours(&contours);
        let scaled = self.scaler.scale_polygons(&polygons, pre.scale_ratio, dims);
        let det_post_t = t.elapsed();

        if scaled.is_empty() {
            return Ok(RunOutput {
                results: Vec::new(),
                timings: super::pipeline::PipelineTimings {
                    detection: StageTimings {
                        preprocess: det_pre_t,
                        inference: det_inf_t,
                        postprocess: det_post_t,
                    },
                    total: start.elapsed(),
                    ..Default::default()
                },
            });
        }

        // ---- 识别（与 tract 后端共用裁剪/旋转/分桶逻辑）----
        let t = Instant::now();
        let mut by_bucket: Vec<(u32, Vec<(usize, Vec<f32>, u32)>)> =
            REC_WIDTH_BUCKETS.iter().map(|b| (*b, Vec::new())).collect();
        let mut rotated = 0usize;
        for (idx, poly) in scaled.iter().enumerate() {
            let region = bounds(poly, dims);
            let bucket = bucket_for(natural_width(region));
            let (buf, width, did_rotate) = prepare_crop(image, region, bucket);
            if did_rotate {
                rotated += 1;
            }
            if let Some(slot) = by_bucket.iter_mut().find(|(b, _)| *b == bucket) {
                slot.1.push((idx, buf, width));
            }
        }
        let rec_pre_t = t.elapsed();

        let t = Instant::now();
        let mut slots: Vec<Option<(String, f32)>> = vec![None; scaled.len()];
        for (bucket, items) in by_bucket.iter() {
            if items.is_empty() {
                continue;
            }
            for (idx, text, confidence) in self.recognize_bucket(items, *bucket)? {
                if let Some(slot) = slots.get_mut(idx) {
                    *slot = Some((text, confidence));
                }
            }
        }
        let rec_inf_t = t.elapsed();

        let results: Vec<Recognized> = slots
            .into_iter()
            .flatten()
            .map(|(text, confidence)| Recognized { text, confidence })
            .collect();

        Ok(RunOutput {
            timings: super::pipeline::PipelineTimings {
                image_decode: Duration::ZERO,
                detection: StageTimings {
                    preprocess: det_pre_t,
                    inference: det_inf_t,
                    postprocess: det_post_t,
                },
                recognition: StageTimings {
                    preprocess: rec_pre_t,
                    inference: rec_inf_t,
                    postprocess: Duration::ZERO,
                },
                total: start.elapsed(),
                rotated_regions: rotated,
            },
            results,
        })
    }

    fn recognize_bucket(
        &self,
        items: &[(usize, Vec<f32>, u32)],
        bucket: u32,
    ) -> Result<Vec<(usize, String, f32)>, String> {
        let mut out = Vec::with_capacity(items.len());
        for chunk in items.chunks(REC_BATCH) {
            let mut flat: Vec<f32> =
                Vec::with_capacity(REC_BATCH * (3 * REC_HEIGHT * bucket) as usize);
            let mut widths: Vec<u32> = Vec::with_capacity(REC_BATCH);
            for (_, buf, w) in chunk {
                flat.extend_from_slice(buf);
                widths.push(*w);
            }
            if let Some((_, last_buf, last_w)) = chunk.last() {
                for _ in chunk.len()..REC_BATCH {
                    flat.extend_from_slice(last_buf);
                    widths.push(*last_w);
                }
            }

            let logits: Array3<f32> = {
                let input = ort::value::Tensor::from_array((
                    [REC_BATCH, 3, REC_HEIGHT as usize, bucket as usize],
                    flat,
                ))
                .map_err(|e| format!("构造识别输入失败: {e}"))?;
                let mut rec = self.rec.lock().unwrap_or_else(|e| e.into_inner());
                let res = rec
                    .run(ort::inputs![self.rec_input.as_str() => input])
                    .map_err(|e| format!("识别推理失败: {e}"))?;
                let (oshape, odata) = res[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| format!("识别输出读取失败: {e}"))?;
                let dims: Vec<usize> = oshape.iter().map(|d| *d as usize).collect();
                if dims.len() != 3 {
                    return Err(format!("识别输出维度异常: {dims:?}"));
                }
                Array3::from_shape_vec((dims[0], dims[1], dims[2]), odata.to_vec())
                    .map_err(|e| format!("识别输出重塑失败: {e}"))?
            };

            let steps = logits.shape()[1];
            let scale = steps as f32 / bucket as f32;
            let valid: Vec<usize> = widths
                .iter()
                .map(|w| ((scale * *w as f32).round() as isize).clamp(1, steps as isize) as usize)
                .collect();

            let decoded = self
                .rec_post
                .process(&RecInferenceOutput { logits, valid_timesteps: valid })
                .map_err(|e| format!("识别解码失败: {e}"))?;
            for ((idx, _, _), seq) in chunk.iter().zip(decoded.iter()) {
                out.push((*idx, seq.text.clone(), seq.confidence));
            }
        }
        Ok(out)
    }
}
