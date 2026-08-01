use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use pure_onnx_ocr::{OcrEngine, OcrEngineBuilder};

static DET_MODEL: &[u8] = include_bytes!("../../models/ppocrv5/det.onnx");
static REC_MODEL: &[u8] = include_bytes!("../../models/ppocrv5/rec.onnx");
static DICT_DATA: &[u8] = include_bytes!("../../models/ppocrv5/ppocrv5_dict.txt");

struct SendEngine(Mutex<OcrEngine>);
// SAFETY: Mutex serializes all access to the inner OcrEngine, protecting its RefCell interior.
unsafe impl Send for SendEngine {}
unsafe impl Sync for SendEngine {}

static ENGINE: OnceLock<SendEngine> = OnceLock::new();

fn write_if_missing(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::write(path, data)?;
    }
    Ok(())
}

fn try_build_engine() -> Result<SendEngine, String> {
    let dir = match std::env::temp_dir().join("link_searcher_models").join("ppocrv5") {
        d if d.exists() => d,
        d => {
            std::fs::create_dir_all(&d).map_err(|e| format!("无法创建模型缓存目录: {e}"))?;
            d
        }
    };

    let det = dir.join("det.onnx");
    let rec = dir.join("rec.onnx");
    let dict = dir.join("ppocrv5_dict.txt");

    write_if_missing(&det, DET_MODEL).map_err(|e| format!("无法写入检测模型: {e}"))?;
    write_if_missing(&rec, REC_MODEL).map_err(|e| format!("无法写入识别模型: {e}"))?;
    write_if_missing(&dict, DICT_DATA).map_err(|e| format!("无法写入字典文件: {e}"))?;

    let inner = OcrEngineBuilder::new()
        .det_model_path(&det)
        .rec_model_path(&rec)
        .dictionary_path(&dict)
        .det_limit_side_len(960)
        .build()
        .map_err(|e| format!("PaddleOCR 引擎初始化失败: {e}"))?;

    Ok(SendEngine(Mutex::new(inner)))
}

fn with_engine<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&OcrEngine) -> Result<T, String>,
{
    let engine = match ENGINE.get() {
        Some(engine) => engine,
        None => {
            let engine = try_build_engine()?;
            let _ = ENGINE.set(engine); // race loser's engine is dropped
            ENGINE.get().expect("just set")
        }
    };

    let guard = engine.0.lock().unwrap_or_else(|e| e.into_inner());
    f(&*guard)
}

pub fn health_check() -> Result<(), String> {
    with_engine(|eng| {
        let img = image::DynamicImage::new_rgb8(64, 32);
        eng.run_from_image(&img)
            .map_err(|e| format!("PaddleOCR 引擎自检失败: {e}"))?;
        Ok(())
    })
}

pub fn recognize_from_path(path: &Path) -> Result<String> {
    with_engine(|eng| {
        eng.run_from_path(path)
            .map(|results| results.into_iter().map(|r| r.text).collect::<Vec<_>>().join(" "))
            .map_err(|e| format!("无法识别图片 {}: {}", path.display(), e))
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn recognize_from_image(image: &image::DynamicImage) -> Result<String> {
    with_engine(|eng| {
        eng.run_from_image(image)
            .map(|results| results.into_iter().map(|r| r.text).collect::<Vec<_>>().join(" "))
            .map_err(|e| format!("OCR 引擎无法处理此图片（模型可能不支持该输入格式）: {}", e))
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
}
