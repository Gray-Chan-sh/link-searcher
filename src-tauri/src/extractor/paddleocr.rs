use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use log;
use pure_onnx_ocr::{OcrEngine, OcrEngineBuilder};

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// macOS QoS class values — see `pthread_set_qos_class_self_np(3)`.
/// We request USER_INTERACTIVE so the scheduler prefers performance cores
/// for the OCR inference threads.
#[cfg(target_os = "macos")]
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

static DET_MODEL: &[u8] = include_bytes!("../../models/ppocrv5/det.onnx");
static REC_MODEL: &[u8] = include_bytes!("../../models/ppocrv5/rec.onnx");
static DICT_DATA: &[u8] = include_bytes!("../../models/ppocrv5/ppocrv5_dict.txt");

struct SendEngine(Mutex<OcrEngine>);
// SAFETY: OcrEngine contains non-Send interior mutability (RefCell-backed ONNX session).
// Each engine's Mutex serializes all access, so SendEngine is safe to share across threads.
unsafe impl Send for SendEngine {}
unsafe impl Sync for SendEngine {}

/// Pool of OCR engines, one Mutex per engine, round-robin access.
/// Each engine is single-threaded (tract has no intra-op parallelism), so a
/// pool lets concurrent OCR calls (multi-page PDFs, Rayon batch_index) run on
/// multiple cores instead of serializing on one global Mutex.
struct EnginePool {
    engines: Vec<SendEngine>,
    next: AtomicUsize,
}

impl EnginePool {
    fn new(count: usize) -> Result<Self, String> {
        log::info!("Building OCR engine pool ({} engine(s))…", count);
        let mut engines = Vec::with_capacity(count);
        for _ in 0..count {
            engines.push(try_build_engine()?);
        }
        log::info!("OCR engine pool ready with {} engine(s)", count);
        Ok(Self {
            engines,
            next: AtomicUsize::new(0),
        })
    }

    fn with_engine<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&OcrEngine) -> Result<T, String>,
    {
        #[cfg(target_os = "macos")]
        unsafe {
            pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
        }
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.engines.len();
        let guard = self.engines[idx].0.lock().unwrap_or_else(|e| e.into_inner());
        f(&*guard)
    }
}

static CONFIGURED_POOL_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Set the OCR engine pool size from the `ocr_concurrent` user setting.
/// Must be called before the first OCR use (pool is built lazily). 0 = auto.
pub fn set_pool_size(size: usize) {
    CONFIGURED_POOL_SIZE.store(size.clamp(1, 8), Ordering::Relaxed);
}

fn pool_size() -> usize {
    let configured = CONFIGURED_POOL_SIZE.load(Ordering::Relaxed);
    if configured > 0 {
        return configured;
    }
    let cores = thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    cores.clamp(1, 4)
}

static POOL: OnceLock<EnginePool> = OnceLock::new();

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
    let pool = match POOL.get() {
        Some(pool) => pool,
        None => {
            let pool = EnginePool::new(pool_size())?;
            let _ = POOL.set(pool); // race loser's engines are dropped
            POOL.get().expect("just set")
        }
    };
    pool.with_engine(f)
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

/// Number of engines currently built in the pool (0 if not yet built).
/// Used for per-page OCR performance diagnosis in `pdf.rs`.
pub fn active_pool_size() -> usize {
    POOL.get().map(|p| p.engines.len()).unwrap_or(0)
}

/// Like `recognize_from_path`, but also returns the number of detected text
/// regions, so callers can log per-page workload (regions, time) for diagnosis.
pub fn recognize_from_path_with_regions(path: &Path) -> Result<(String, usize), String> {
    with_engine(|eng| {
        let run = eng
            .run_with_metrics_from_path(path)
            .map_err(|e| format!("无法识别图片 {}: {}", path.display(), e))?;
        let text = run
            .results
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join(" ");
        Ok((text, run.results.len()))
    })
    .map_err(|e| format!("{e}"))
}

/// Run a closure that holds the OCR engine lock, with a 120s timeout.
/// Prevents the global Mutex from being held indefinitely if OCR hangs.
fn with_engine_timed<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&OcrEngine) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let result = with_engine(f);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(v) => v,
        Err(_) => {
            let _ = handle.join();
            Err("PaddleOCR timed out after 120s".to_string())
        }
    }
}

/// Run OCR on an image file, returning a human-readable timing breakdown
/// (image decode, detection preprocess/inference/postprocess, recognition
/// preprocess/inference/postprocess). Used for performance diagnosis.
pub fn recognize_with_metrics_from_path(path: &Path) -> Result<String, String> {
    with_engine(|eng| {
        let run = eng
            .run_with_metrics_from_path(path)
            .map_err(|e| format!("无法识别图片 {}: {}", path.display(), e))?;
        let t = &run.timings;
        Ok(format!(
            "decode={:.2}s det(pre={:.2}s inf={:.2}s post={:.2}s) rec(pre={:.2}s inf={:.2}s post={:.2}s) total={:.2}s regions={}",
            t.image_decode.as_secs_f64(),
            t.detection.preprocess.as_secs_f64(),
            t.detection.inference.as_secs_f64(),
            t.detection.postprocess.as_secs_f64(),
            t.recognition.preprocess.as_secs_f64(),
            t.recognition.inference.as_secs_f64(),
            t.recognition.postprocess.as_secs_f64(),
            t.total.as_secs_f64(),
            run.results.len(),
        ))
    })
    .map_err(|e| format!("{e}"))
}

pub fn recognize_from_image(image: &image::DynamicImage) -> Result<String> {
    // Clone image so the closure owns it (needed for thread send + 'static)
    let image = image.clone();
    with_engine_timed(move |eng| {
        eng.run_from_image(&image)
            .map(|results| results.into_iter().map(|r| r.text).collect::<Vec<_>>().join(" "))
            .map_err(|e| format!("OCR 引擎无法处理此图片（模型可能不支持该输入格式）: {}", e))
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
}
