use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use log;
use pure_onnx_ocr::{OcrEngine, OcrEngineBuilder};
use super::ocr::OcrStats;

const MIN_CONFIDENCE: f32 = 0.5;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// macOS QoS class values — see `pthread_set_qos_class_self_np(3)`.
/// We request USER_INTERACTIVE so the scheduler prefers performance cores
/// for the OCR inference threads.
#[cfg(target_os = "macos")]
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

/// Locate the PP-OCRv5 model dir.
///
/// Resolution order (release vs dev):
/// 1. `<data_dir>/models/ppocrv5` — where the setup wizard / dep installer
///    downloads the models for packaged builds.
/// 2. `<repo>/src-tauri/models/ppocrv5` — git-tracked dev models, so
///    `tauri dev` needs no download.
pub fn resolve_model_dir(data_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dd) = data_dir {
        let p = dd.join("models").join("ppocrv5");
        if p.join("det.onnx").is_file() && p.join("rec.onnx").is_file() {
            return Some(p);
        }
    }
    // Fall back to the git-tracked dev tree.
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models").join("ppocrv5");
    if dev.join("det.onnx").is_file() && dev.join("rec.onnx").is_file() {
        return Some(dev);
    }
    None
}

pub fn models_present() -> bool {
    resolve_model_dir(resolved_model_dir()).is_some()
}

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
        f(&guard)
    }
}

static POOL: OnceLock<Result<EnginePool, String>> = OnceLock::new();
static MODEL_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Point the OCR engine at a data dir (packaged builds). Called at startup
/// with the app data dir; dev builds may skip it (falls back to the repo
/// models dir). Must run before the first inference.
pub fn init(data_dir: &Path) {
    let _ = MODEL_DIR.set(data_dir.to_path_buf());
}

fn resolved_model_dir() -> Option<&'static Path> {
    MODEL_DIR.get().map(|p| p.as_path())
}

fn try_build_engine() -> Result<SendEngine, String> {
    let dir = resolve_model_dir(resolved_model_dir())
        .ok_or_else(|| {
            "PaddleOCR 模型未找到。请在首次启动引导或设置页「依赖中心」下载 OCR 模型。".to_string()
        })?;

    let det = dir.join("det.onnx");
    let rec = dir.join("rec.onnx");
    let dict = dict_with_space_token(&dir.join("ppocrv5_dict.txt"))?;

    let inner = OcrEngineBuilder::new()
        .det_model_path(&det)
        .rec_model_path(&rec)
        .dictionary_path(&dict)
        .det_limit_side_len(1280)
        .build()
        .map_err(|e| format!("PaddleOCR 引擎初始化失败: {e}"))?;

    Ok(SendEngine(Mutex::new(inner)))
}

/// Return a dictionary path whose vocabulary includes the model's space class.
///
/// PP-OCRv5's recognition model is trained with `use_space_char`, so it has one
/// more output class than the shipped `ppocrv5_dict.txt`: a trailing space
/// token. The crate's CTC decoder maps any class index past the dictionary to a
/// hard-coded `"[UNK]"`, which surfaced at **every word gap** (measured: 13
/// `[UNK]` in one complaint; 5 on a single page). Appending the missing space
/// line removes them (verified 5 → 0 on the same page).
///
/// The rewrite is done at runtime into a temp file so it works for both the
/// repo dev models and the models downloaded by the dependency installer.
fn dict_with_space_token(dict_path: &Path) -> Result<PathBuf, String> {
    let raw = std::fs::read_to_string(dict_path)
        .map_err(|e| format!("读取 OCR 字典失败: {e}"))?;
    if raw.lines().any(|line| line == " ") {
        return Ok(dict_path.to_path_buf());
    }

    let mut content = raw.trim_end_matches(['\n', '\r']).to_string();
    content.push('\n');
    content.push(' ');
    content.push('\n');

    let out = std::env::temp_dir().join(format!(
        "ls_ppocrv5_dict_space_{}.txt",
        std::process::id()
    ));
    std::fs::write(&out, content).map_err(|e| format!("写入 OCR 字典缓存失败: {e}"))?;
    Ok(out)
}

fn with_engine<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&OcrEngine) -> Result<T, String>,
{
    // 自适应并发闸门：一次调用 = 一张页图的整轮推理，正是内存/CPU 的大头。
    // 压力升高时这里排队（而不是失败），暂停档则等待恢复。
    let _permit = crate::load::acquire_ocr();
    engine_pool()?.with_engine(f)
}

/// Build the engine pool exactly once.
///
/// The previous `POOL.get()`-then-`EnginePool::new()` sequence was a race: when
/// several OCR threads started simultaneously (parallel per-page OCR on a fresh
/// process), each built its own pool. That means N threads × N engines of
/// duplicate 21 MB models at once, and a build failure in any of them made the
/// whole page's OCR return `None` and get silently dropped — the cause of the
/// "only part of the document was extracted" symptom. `OnceLock::get_or_init`
/// runs the initializer once and makes the others wait for it.
fn engine_pool() -> Result<&'static EnginePool, String> {
    match POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(2)
            .clamp(1, 8);
        EnginePool::new(n)
    }) {
        Ok(pool) => Ok(pool),
        Err(e) => Err(e.clone()),
    }
}

pub fn health_check() -> Result<(), String> {
    with_engine(|eng| {
        let img = image::DynamicImage::new_rgb8(64, 32);
        eng.run_from_image(&img)
            .map_err(|e| format!("PaddleOCR 引擎自检失败: {e}"))?;
        Ok(())
    })
}

pub fn recognize_from_path_enriched(
    path: &Path,
) -> Result<(String, OcrStats), String> {
    with_engine(|eng| {
        eng.run_from_path(path)
            .map(|results| {
                let filtered: Vec<_> = results
                    .into_iter()
                    .filter(|r| r.confidence >= MIN_CONFIDENCE)
                    .collect();
                let text = filtered.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ");
                let mean_confidence = if filtered.is_empty() {
                    None
                } else {
                    let sum: f32 = filtered.iter().map(|r| r.confidence).sum();
                    Some(sum / filtered.len() as f32)
                };
                let stats = OcrStats { region_count: filtered.len(), mean_confidence };
                (text, stats)
            })
            .map_err(|e| format!("无法识别图片 {}: {}", path.display(), e))
    })
}

pub fn recognize_from_path(path: &Path) -> Result<String> {
    recognize_from_path_enriched(path)
        .map(|(text, _stats)| text)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_confidence_is_expected_value() {
        assert!((MIN_CONFIDENCE - 0.5).abs() < f32::EPSILON);
    }

    /// The dictionary must gain exactly one trailing space token so the model's
    /// `use_space_char` class decodes as a space instead of `[UNK]`.
    #[test]
    fn dict_with_space_token_appends_missing_space() {
        let dir = std::env::temp_dir().join(format!("ls_dict_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ppocrv5_dict.txt");
        std::fs::write(&src, "a\nb\n\u{3000}\n").unwrap();

        let out = dict_with_space_token(&src).expect("ok");
        let content = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = content.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.last(), Some(&" "), "space token must be appended");
        assert_eq!(lines.len(), 4, "content lines plus appended space: {lines:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dictionary that already carries a space line is used unchanged.
    #[test]
    fn dict_with_space_token_keeps_existing_space() {
        let dir = std::env::temp_dir().join(format!("ls_dict_test2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ppocrv5_dict.txt");
        std::fs::write(&src, "a\nb\n \n").unwrap();

        let out = dict_with_space_token(&src).expect("ok");
        assert_eq!(out, src, "existing space token must short-circuit");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Number of engines currently built in the pool (0 if not yet built).
/// Used for per-page OCR performance diagnosis in `pdf.rs`.
pub fn active_pool_size() -> usize {
    POOL.get()
        .and_then(|r| r.as_ref().ok())
        .map(|p| p.engines.len())
        .unwrap_or(0)
}

/// Like `recognize_from_path`, but also returns the number of detected text
/// regions, so callers can log per-page workload (regions, time) for diagnosis.
pub fn recognize_from_path_with_regions(path: &Path) -> Result<(String, usize), String> {
    with_engine(|eng| {
        let run = eng
            .run_with_metrics_from_path(path)
            .map_err(|e| format!("无法识别图片 {}: {}", path.display(), e))?;
        let filtered: Vec<_> = run
            .results
            .into_iter()
            .filter(|r| r.confidence >= MIN_CONFIDENCE)
            .collect();
        let text = filtered
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join(" ");
        let count = filtered.len();
        Ok((text, count))
    })
    .map_err(|e| e.to_string())
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
    .map_err(|e| e.to_string())
}

pub fn recognize_from_image(image: &image::DynamicImage) -> Result<String> {
    // Clone image so the closure owns it (needed for thread send + 'static)
    let image = image.clone();
    with_engine_timed(move |eng| {
        eng.run_from_image(&image)
            .map(|results| {
                results
                    .into_iter()
                    .filter(|r| r.confidence >= MIN_CONFIDENCE)
                    .map(|r| r.text)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map_err(|e| format!("OCR 引擎无法处理此图片（模型可能不支持该输入格式）: {}", e))
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
}
