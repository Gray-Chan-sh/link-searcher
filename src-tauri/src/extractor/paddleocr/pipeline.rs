//! PaddleOCR 推理流水线（det → 裁剪 → **竖排旋转** → rec）。
//!
//! 为什么不用上游的 `OcrEngine`：
//! `pure-onnx-ocr` 0.1 的识别前处理直接按外接矩形裁剪，**没有 PP-OCR 的竖排旋转**
//! （官方流程：裁剪块 `高/宽 >= 1.5` 时逆时针旋转 90° 再送识别）。法务/工商材料里
//! 大量流程图、竖排标签、竖版扫描件因此整行读不出来：样例库 45 张图实测
//! `14458 → 16037` 字符（+11.6%），单页 `pdf_01-2` 从 89 字涨到 553 字。
//!
//! 这里复用 crate 暴露的各阶段（det 预处理/后处理/unclip/scaler、
//! rec 会话与 CTC 解码），只把"裁剪 + 旋转 + 组批"换成我们自己的实现。
//!
//! 其余行为与上游保持一致：识别高度 48、单块最大宽 320、批大小 8、
//! 归一化 `(v/255 - 0.5) / 0.5`、右填充为 0（归一化后即 -1）。

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::{DynamicImage, GenericImageView, RgbImage};
use pure_onnx_ocr::{
    DetInferenceSession, DetPolygonScaler, DetPolygonScalerConfig, DetPolygonUnclipper,
    DetPolygonUnclipperConfig, DetPostProcessor, DetPostProcessorConfig, DetPreProcessor,
    DetPreProcessorConfig, PreprocessedRecBatch, RecDictionary, RecInferenceSession,
    RecPostProcessor, RecPostProcessorConfig,
};

/// 识别输入高度（PP-OCR 固定 48）。
const REC_HEIGHT: u32 = 48;
/// 单个裁剪块的最大宽度（与上游默认一致；更宽的行会被压缩，属已知限制）。
const REC_MAX_WIDTH: u32 = 320;
/// 识别批大小（与上游默认一致）。
const REC_BATCH: usize = 8;
/// 竖排判定阈值：高/宽 >= 1.5 视为竖排，需要旋转 90°。
const VERTICAL_RATIO: f32 = 1.5;

/// 一条识别结果。
#[derive(Debug, Clone)]
pub struct Recognized {
    pub text: String,
    pub confidence: f32,
}

/// 单个阶段的耗时。
#[derive(Debug, Clone, Copy, Default)]
pub struct StageTimings {
    pub preprocess: Duration,
    pub inference: Duration,
    pub postprocess: Duration,
}

impl StageTimings {
    #[allow(dead_code)]
    pub fn total(&self) -> Duration {
        self.preprocess + self.inference + self.postprocess
    }
}

/// 一次整图推理的耗时拆解。
#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineTimings {
    pub image_decode: Duration,
    pub detection: StageTimings,
    pub recognition: StageTimings,
    pub total: Duration,
    pub rotated_regions: usize,
}

/// 一次推理的完整结果。
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub results: Vec<Recognized>,
    pub timings: PipelineTimings,
}

/// 裁剪块判定：是否属于竖排文本（需要逆时针旋转 90°）。
///
/// 与 PaddleOCR `get_rotate_crop_image` 的判据一致：`height / width >= 1.5`。
pub fn is_vertical(h: u32, w: u32) -> bool {
    if w == 0 {
        return false;
    }
    h as f32 / w as f32 >= VERTICAL_RATIO
}

pub struct OcrPipeline {
    det: DetInferenceSession,
    det_pre: DetPreProcessor,
    det_post: DetPostProcessor,
    unclip: DetPolygonUnclipper,
    scaler: DetPolygonScaler,
    rec: RecInferenceSession,
    rec_post: RecPostProcessor,
}

impl OcrPipeline {
    pub fn load(
        det_model: &Path,
        rec_model: &Path,
        dictionary: &Path,
        limit_side_len: u32,
    ) -> Result<Self, String> {
        let det = DetInferenceSession::load(det_model)
            .map_err(|e| format!("加载检测模型失败: {e}"))?;
        let rec = RecInferenceSession::load(rec_model)
            .map_err(|e| format!("加载识别模型失败: {e}"))?;
        let dict = RecDictionary::from_path(dictionary)
            .map_err(|e| format!("加载识别字典失败: {e}"))?;
        Ok(Self {
            det,
            det_pre: DetPreProcessor::new(DetPreProcessorConfig { limit_side_len }),
            det_post: DetPostProcessor::new(DetPostProcessorConfig::default()),
            unclip: DetPolygonUnclipper::new(DetPolygonUnclipperConfig::default()),
            scaler: DetPolygonScaler::new(DetPolygonScalerConfig::default()),
            rec,
            rec_post: RecPostProcessor::new(Arc::new(dict), RecPostProcessorConfig::default()),
        })
    }

    /// 从文件路径识别：解码 + 整条流水线。
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

    /// 对已解码图片跑完整流水线。
    pub fn run_from_image(&self, image: &DynamicImage) -> Result<RunOutput, String> {
        let start = Instant::now();
        let dims = image.dimensions();

        // ---- 检测 ----
        let t = Instant::now();
        let pre = self
            .det_pre
            .process(image)
            .map_err(|e| format!("检测预处理失败: {e}"))?;
        let det_pre_t = t.elapsed();

        let t = Instant::now();
        let det_out = self
            .det
            .run(&pre)
            .map_err(|e| format!("检测推理失败: {e}"))?;
        let det_inf_t = t.elapsed();

        let t = Instant::now();
        let contours = self
            .det_post
            .process(&det_out)
            .map_err(|e| format!("检测后处理失败: {e}"))?;
        let polygons = self.unclip.unclip_contours(&contours);
        let scaled = self.scaler.scale_polygons(&polygons, pre.scale_ratio, dims);
        let det_post_t = t.elapsed();

        if scaled.is_empty() {
            return Ok(RunOutput {
                results: Vec::new(),
                timings: PipelineTimings {
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

        // ---- 识别（裁剪 + 竖排旋转 + 组批）----
        let t = Instant::now();
        let mut crops = Vec::with_capacity(scaled.len());
        let mut rotated = 0usize;
        for poly in &scaled {
            let region = bounds(poly, dims);
            let (buf, width, did_rotate) = prepare_crop(image, region);
            if did_rotate {
                rotated += 1;
            }
            crops.push((buf, width));
        }
        let rec_pre_t = t.elapsed();

        let t = Instant::now();
        let mut texts = Vec::with_capacity(crops.len());
        for chunk in crops.chunks(REC_BATCH) {
            texts.extend(self.recognize_batch(chunk)?);
        }
        let rec_inf_t = t.elapsed();

        let results: Vec<Recognized> = texts
            .into_iter()
            .map(|(text, confidence)| Recognized { text, confidence })
            .collect();

        Ok(RunOutput {
            timings: PipelineTimings {
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

    /// 对一批裁剪块（已归一化）做识别；批未满时复制最后一块补齐，
    /// 让 tract 只需要缓存 `(REC_BATCH, REC_MAX_WIDTH)` 一种形状，
    /// 否则每换一个批大小都要重新 optimize（实测每新增一种形状 2~3 秒）。
    fn recognize_batch(&self, chunk: &[(Vec<f32>, u32)]) -> Result<Vec<(String, f32)>, String> {
        let mut flat: Vec<f32> =
            Vec::with_capacity(REC_BATCH * (3 * REC_HEIGHT * REC_MAX_WIDTH) as usize);
        let mut widths: Vec<u32> = Vec::with_capacity(REC_BATCH);

        for (buf, width) in chunk {
            flat.extend_from_slice(buf);
            widths.push(*width);
        }
        if let Some((last_buf, last_width)) = chunk.last() {
            for _ in chunk.len()..REC_BATCH {
                flat.extend_from_slice(last_buf);
                widths.push(*last_width);
            }
        }

        let tensor: tract_onnx::prelude::Tensor = tract_onnx::prelude::tract_ndarray::Array4::<f32>::from_shape_vec(
            (
                REC_BATCH,
                3,
                REC_HEIGHT as usize,
                REC_MAX_WIDTH as usize,
            ),
            flat,
        )
        .map_err(|e| format!("识别输入张量构造失败: {e}"))?
        .into_dyn()
        .into();

        let batch = PreprocessedRecBatch {
            tensor,
            valid_widths: widths,
            max_width: REC_MAX_WIDTH,
        };
        let out = self
            .rec
            .run(&batch)
            .map_err(|e| format!("识别推理失败: {e}"))?;
        let mut decoded = self
            .rec_post
            .process(&out)
            .map_err(|e| format!("识别解码失败: {e}"))?;
        decoded.truncate(chunk.len());
        Ok(decoded
            .into_iter()
            .map(|seq| (seq.text, seq.confidence))
            .collect())
    }
}

/// 多边形 → 外接矩形（x, y, w, h），并夹到图片范围内。
fn bounds(poly: &pure_onnx_ocr::Polygon<f64>, dims: (u32, u32)) -> (u32, u32, u32, u32) {
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in &poly.exterior().0 {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    let (img_w, img_h) = dims;
    let x = x0.max(0.0).floor() as u32;
    let y = y0.max(0.0).floor() as u32;
    let x = x.min(img_w.saturating_sub(1));
    let y = y.min(img_h.saturating_sub(1));
    let w = ((x1 - x0).round() as u32).max(1).min(img_w - x);
    let h = ((y1 - y0).round() as u32).max(1).min(img_h - y);
    (x, y, w, h)
}

/// 裁剪 → 竖排旋转 → 缩放 → 归一化。返回 (CHW 缓冲, 有效宽度, 是否旋转)。
///
/// 缓冲区固定为 `3 * 48 * 320`，右侧未使用部分填 `-1.0`（即原图 0 黑，
/// 与上游 `pad_value = [0,0,0]` 归一化后的值一致）。
fn prepare_crop(image: &DynamicImage, region: (u32, u32, u32, u32)) -> (Vec<f32>, u32, bool) {
    let (x, y, w, h) = region;
    let mut crop: DynamicImage = image.crop_imm(x, y, w, h);

    let mut did_rotate = false;
    if is_vertical(h, w) {
        // PaddleOCR: cv2.ROTATE_90_COUNTERCLOCKWISE（image crate 的 rotate270）
        crop = crop.rotate270();
        did_rotate = true;
    }

    let (cw, ch) = crop.dimensions();
    let target_w = ((REC_HEIGHT as f32) * (cw as f32) / (ch.max(1) as f32))
        .round()
        .max(1.0) as u32;
    let target_w = target_w.min(REC_MAX_WIDTH);
    let resized = crop.resize_exact(target_w, REC_HEIGHT, image::imageops::FilterType::Triangle);
    let rgb: RgbImage = resized.to_rgb8();

    let plane = (REC_HEIGHT * REC_MAX_WIDTH) as usize;
    let mut buf = vec![-1.0f32; 3 * plane];
    // 用裸字节切片逐行写，避免 `get_pixel` 的逐像素边界检查
    // （dev 构建下我们的 crate 是 O0，这个循环是整条流水线最热的地方）。
    let raw = rgb.as_raw();
    let row_bytes = (target_w * 3) as usize;
    for yy in 0..REC_HEIGHT as usize {
        let src_row = &raw[yy * row_bytes..yy * row_bytes + row_bytes];
        let dst_row = yy * REC_MAX_WIDTH as usize;
        for xx in 0..target_w as usize {
            let s = xx * 3;
            let d = dst_row + xx;
            buf[d] = (src_row[s] as f32 / 255.0 - 0.5) / 0.5;
            buf[plane + d] = (src_row[s + 1] as f32 / 255.0 - 0.5) / 0.5;
            buf[2 * plane + d] = (src_row[s + 2] as f32 / 255.0 - 0.5) / 0.5;
        }
    }
    (buf, target_w, did_rotate)
}

#[cfg(test)]
mod tests {
    use super::is_vertical;

    #[test]
    fn vertical_ratio_uses_h_over_w() {
        assert!(is_vertical(69, 19), "19x69 竖排块必须旋转");
        assert!(is_vertical(300, 200), "1.5 倍整好达到阈值");
        assert!(!is_vertical(20, 60));
        assert!(!is_vertical(48, 0), "宽度为 0 时不做旋转");
    }
}
