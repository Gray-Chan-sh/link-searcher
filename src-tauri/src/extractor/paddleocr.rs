use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use pure_onnx_ocr::{OcrEngine, OcrEngineBuilder};

static DET_MODEL: &[u8] = include_bytes!("../../models/ppocrv5/det.onnx");
static REC_MODEL: &[u8] = include_bytes!("../../models/ppocrv5/rec.onnx");
static DICT_DATA: &[u8] = include_bytes!("../../models/ppocrv5/ppocrv5_dict.txt");

struct SendEngine(OcrEngine);
unsafe impl Send for SendEngine {}
unsafe impl Sync for SendEngine {}

static ENGINE: OnceLock<Result<SendEngine, String>> = OnceLock::new();

fn get_or_init_engine() -> &'static Result<SendEngine, String> {
    ENGINE.get_or_init(|| {
        let dir = match std::env::temp_dir().join("link_searcher_models").join("ppocrv5") {
            d if d.exists() => d,
            d => {
                if let Err(e) = std::fs::create_dir_all(&d) {
                    return Err(format!("无法创建模型缓存目录: {e}"));
                }
                d
            }
        };

        let det = dir.join("det.onnx");
        let rec = dir.join("rec.onnx");
        let dict = dir.join("ppocrv5_dict.txt");

        if let Err(e) = write_if_missing(&det, DET_MODEL) {
            return Err(format!("无法写入检测模型: {e}"));
        }
        if let Err(e) = write_if_missing(&rec, REC_MODEL) {
            return Err(format!("无法写入识别模型: {e}"));
        }
        if let Err(e) = write_if_missing(&dict, DICT_DATA) {
            return Err(format!("无法写入字典文件: {e}"));
        }

        match OcrEngineBuilder::new()
            .det_model_path(&det)
            .rec_model_path(&rec)
            .dictionary_path(&dict)
            .det_limit_side_len(960)
            .build()
        {
            Ok(engine) => Ok(SendEngine(engine)),
            Err(e) => Err(format!("PaddleOCR 引擎初始化失败: {e}")),
        }
    })
}

fn write_if_missing(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if !path.exists() {
        std::fs::write(path, data)?;
    }
    Ok(())
}

fn engine() -> Result<&'static OcrEngine, String> {
    match get_or_init_engine() {
        Ok(send) => Ok(&send.0),
        Err(e) => Err(e.clone()),
    }
}

pub fn health_check() -> Result<(), String> {
    let eng = engine()?;
    let img = image::DynamicImage::new_rgb8(64, 32);
    eng.run_from_image(&img)
        .map_err(|e| format!("PaddleOCR 引擎自检失败: {e}"))?;
    Ok(())
}

pub fn recognize_from_path(path: &Path) -> Result<String> {
    let eng = engine().map_err(|e| anyhow::anyhow!("{e}"))?;
    let results = eng
        .run_from_path(path)
        .with_context(|| format!("无法识别图片 {}", path.display()))?;
    Ok(results.into_iter().map(|r| r.text).collect::<Vec<_>>().join(" "))
}

pub fn recognize_from_image(image: &image::DynamicImage) -> Result<String> {
    let eng = engine().map_err(|e| anyhow::anyhow!("{e}"))?;
    let results = eng
        .run_from_image(image)
        .context("OCR 引擎无法处理此图片（模型可能不支持该输入格式）")?;
    Ok(results.into_iter().map(|r| r.text).collect::<Vec<_>>().join(" "))
}
