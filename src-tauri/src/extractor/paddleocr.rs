use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use log;
use md5::Digest;
mod pipeline;

use pipeline::OcrPipeline;
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
        // 1) 版本化目录：远端清单升级后的当前版本（`models/paddleocr/versions/<ver>`）
        if let Some(dir) = crate::deps::state::current_dir(dd, "paddleocr", 2) {
            return Some(dir);
        }
        // 2) 老的平铺安装（`models/ppocrv5`）：兼容旧安装，启动时会被迁移成版本目录
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

struct SendEngine(Mutex<OcrPipeline>);
// SAFETY: OcrPipeline 内部的 tract 会话带 RefCell 缓存（非 Send/Sync），
// 每个引擎由自己的 Mutex 串行化访问，因此跨线程共享是安全的。
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
        F: FnOnce(&OcrPipeline) -> Result<T, String>,
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

/// 引擎池。用 `RwLock<Option<_>>` 而不是 `OnceLock`：模型可以**在线升级**，
/// 升级/回退后必须能让下一轮推理用上新模型（`reload_engine_pool`）。
///
/// 单飞仍然成立：`POOL_INIT` 保证同一时刻只有一个线程在建池，
/// 与原先 `OnceLock::get_or_init` 的语义一致（避免 N 线程各建一份 20MB 模型）。
static POOL: std::sync::RwLock<Option<Arc<EnginePool>>> = std::sync::RwLock::new(None);
static POOL_INIT: Mutex<()> = Mutex::new(());
static MODEL_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Point the OCR engine at a data dir (packaged builds). Called at startup
/// with the app data dir; dev builds may skip it (falls back to the repo
/// models dir). Must run before the first inference.
pub fn init(data_dir: &Path) {
    let _ = MODEL_DIR.set(data_dir.to_path_buf());
    adopt_flat_install(data_dir);
}

/// 把老的平铺安装（`models/ppocrv5/`）登记成版本目录。
///
/// 启动时跑一次（老用户升级），**首次下载完成后也跑一次** —— 这样"当前版本"
/// 从一开始就是已知的，远端清单才能正确判断"要不要升级"。
pub fn adopt_flat_install(data_dir: &Path) {
    let def = crate::deps::catalog::all().into_iter().find(|d| d.id == "paddleocr");
    let Some(def) = def else { return };
    let legacy = data_dir.join("models").join("ppocrv5");
    let expected = crate::deps::catalog::builtin_digests(&def);
    if crate::deps::state::migrate_legacy_flat(
        data_dir,
        "paddleocr",
        &legacy,
        crate::deps::catalog::PADDLEOCR_BUILTIN_VERSION,
        &expected,
    ) {
        log::info!(
            "[OCR] 内置模型已迁移到版本目录（{}）",
            crate::deps::catalog::PADDLEOCR_BUILTIN_VERSION
        );
    }
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
    // 版本目录里新版模型用 `dict.txt`；老的平铺安装是 `ppocrv5_dict.txt`。
    let dict_name = if dir.join("dict.txt").is_file() {
        "dict.txt"
    } else {
        "ppocrv5_dict.txt"
    };
    let dict = dict_with_space_token(&dir.join(dict_name))?;

    let inner = OcrPipeline::load(&det, &rec, &dict, 1280)?;

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

    // 输出文件名必须**随输入字典而变**：此前只按 pid 命名，于是任何两个字典
    // （真实模型字典 / 测试字典，或将来多模型并存）都会写同一个文件、互相覆盖 ——
    // 实测会让并发的单测随机读到别人的内容而偶发失败。
    let mut hasher = md5::Md5::new();
    hasher.update(dict_path.to_string_lossy().as_bytes());
    let tag = format!("{:x}", hasher.finalize());
    let out = std::env::temp_dir().join(format!(
        "ls_ppocrv5_dict_space_{}_{}.txt",
        std::process::id(),
        &tag[..12.min(tag.len())]
    ));
    std::fs::write(&out, content).map_err(|e| format!("写入 OCR 字典缓存失败: {e}"))?;
    Ok(out)
}

fn with_engine<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&OcrPipeline) -> Result<T, String>,
{
    // 自适应并发闸门：一次调用 = 一张页图的整轮推理，正是内存/CPU 的大头。
    // 压力升高时这里排队（而不是失败），暂停档则等待恢复。
    let _permit = crate::load::acquire_ocr();
    let pool = engine_pool()?;
    pool.with_engine(f)
}

/// 取（必要时构建）引擎池。
///
/// 历史教训：早期是 `POOL.get()` 然后 `EnginePool::new()`，并发首调时每个线程
/// 各建一份池（N 线程 × N 份 20MB 模型），且任一份构建失败会让整页 OCR 静默
/// 丢弃。现在用 `POOL_INIT` 互斥 + 双重检查保证"只建一次"；
/// 与 `OnceLock` 的唯一区别是**可以被 `reload_engine_pool` 清掉**。
fn engine_pool() -> Result<Arc<EnginePool>, String> {
    if let Some(pool) = POOL
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    {
        return Ok(pool);
    }

    let _guard = POOL_INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pool) = POOL
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    {
        return Ok(pool);
    }

    let n = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(2)
        .clamp(1, 8);
    let built = EnginePool::new(n)?;
    let arc = Arc::new(built);
    *POOL.write().unwrap_or_else(|e| e.into_inner()) = Some(arc.clone());
    Ok(arc)
}

/// 丢弃当前引擎池 —— 模型升级/回退后调用，让下一轮推理重新加载新模型。
///
/// 不阻塞正在跑的推理：它们持有 `Arc<EnginePool>`，跑完自然释放；
/// 新调用会构建新的池（旧池在最后一个引用释放时销毁）。
pub fn reload_engine_pool() {
    let old = POOL.write().unwrap_or_else(|e| e.into_inner()).take();
    if old.is_some() {
        log::info!("[OCR] 已丢弃旧引擎池，下一轮推理将重新加载模型");
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

/// 校验**指定目录**里的模型能否真正跑起来（升级后的冒烟测试）。
///
/// 不复用全局引擎池（池是 `OnceLock`，缓存的是旧模型），而是新建一条流水线：
/// 只用于"装完先验一遍"，成功后才把 `state.json` 切到新版本。
///
/// 为什么不能用空白图：检测框数为 0 时同样"没报错"，但模型其实可能是坏的
/// （tract 0.20 上 v6 就是这样：能跑、结果全 blank）。这里画几条黑色横条当作
/// "文字行"，要求至少检出 1 个区域。
pub fn verify_model_dir(dir: &Path) -> Result<usize, String> {
    let det = dir.join("det.onnx");
    let rec = dir.join("rec.onnx");
    if !det.is_file() || !rec.is_file() {
        return Err("模型目录缺少 det.onnx / rec.onnx".to_string());
    }
    let dict_name = if dir.join("dict.txt").is_file() {
        "dict.txt"
    } else {
        "ppocrv5_dict.txt"
    };
    let dict = dict_with_space_token(&dir.join(dict_name))?;
    let pipeline = pipeline::OcrPipeline::load(&det, &rec, &dict, 1280)?;
    let run = pipeline.run_from_image(&synthetic_text_page())?;
    Ok(run.results.len())
}

/// 生成一张"像文字行"的合成图：白底 + 若干条长短不一的黑色横条。
fn synthetic_text_page() -> image::DynamicImage {
    let (w, h) = (800u32, 600u32);
    let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]));
    let ink = image::Rgb([20, 20, 20]);
    for row in 0..8u32 {
        let y0 = 40 + row * 65;
        let len = 300 + (row * 97) % 380;
        for y in y0..(y0 + 26).min(h) {
            for x in 40..(40 + len).min(w) {
                img.put_pixel(x, y, ink);
            }
        }
    }
    image::DynamicImage::ImageRgb8(img)
}

pub fn recognize_from_path_enriched(
    path: &Path,
) -> Result<(String, OcrStats), String> {
    with_engine(|eng| {
        eng.run_from_path(path)
            .map(|run| {
                let filtered: Vec<_> = run
                    .results
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

    /// 缓存文件必须**按输入字典分开**：此前只按 pid 命名，两个字典会互相覆盖 ——
    /// 这正是 `dict_with_space_token_appends_missing_space` 在全量并行跑时偶发失败
    /// 的原因（它与真实引擎初始化抢同一个 `%TEMP%` 文件）。
    #[test]
    fn dict_with_space_token_separates_dictionaries() {
        let base = std::env::temp_dir().join(format!("ls_dict_test3_{}", std::process::id()));
        let (a_dir, b_dir) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        let a = a_dir.join("ppocrv5_dict.txt");
        let b = b_dir.join("ppocrv5_dict.txt");
        std::fs::write(&a, "a\nb\n\u{3000}\n").unwrap();
        std::fs::write(&b, "x\ny\nz\n").unwrap();

        let out_a = dict_with_space_token(&a).expect("ok");
        let out_b = dict_with_space_token(&b).expect("ok");
        assert_ne!(out_a, out_b, "两个字典不能共用同一个缓存文件");
        assert!(std::fs::read_to_string(&out_a).unwrap().ends_with("\n \n"));
        assert!(std::fs::read_to_string(&out_b).unwrap().ends_with("\n \n"));
        assert!(std::fs::read_to_string(&out_b).unwrap().starts_with("x\n"));

        // 交错重取：B 的写入不能覆盖 A 的缓存。
        let _ = dict_with_space_token(&b).expect("ok");
        let a_now = std::fs::read_to_string(&out_a).unwrap();
        assert!(a_now.starts_with("a\n"), "重取 B 后 A 的缓存被覆盖了: {a_now:?}");

        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Number of engines currently built in the pool (0 if not yet built).
/// Used for per-page OCR performance diagnosis in `pdf.rs`.
pub fn active_pool_size() -> usize {
    POOL.read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|p| p.engines.len())
        .unwrap_or(0)
}

/// Like `recognize_from_path`, but also returns the number of detected text
/// regions, so callers can log per-page workload (regions, time) for diagnosis.
pub fn recognize_from_path_with_regions(path: &Path) -> Result<(String, usize), String> {
    with_engine(|eng| {
        let run = eng
            .run_from_path(path)
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
    F: FnOnce(&OcrPipeline) -> Result<T, String> + Send + 'static,
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
            // ⚠️ 超时必须**直接返回**，不要 `join()`：卡死的推理线程会让 join 永久
            // 阻塞，超时形同虚设，还会把调用者（rayon worker）连同 OcrGate 许可一起
            // 拖死 —— 整批提取停摆而日志里看不到任何东西（旧实现就是这样）。
            // 这里放弃该线程：它持有引擎锁与 load 许可（有界泄漏，每个卡住的调用一次），
            // 后续调用由 OcrGate 的「零进展」判定救出（见 `extractor/ocr.rs`）。
            log::error!("[OCR] PaddleOCR 单图推理超过 120s → 放弃等待该线程（不 join）");
            drop(handle); // 显式说明：故意不 join，线程自行消亡
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
            .run_from_path(path)
            .map_err(|e| format!("无法识别图片 {}: {}", path.display(), e))?;
        let t = &run.timings;
        Ok(format!(
            "decode={:.2}s det(pre={:.2}s inf={:.2}s post={:.2}s) rec(pre={:.2}s inf={:.2}s post={:.2}s) total={:.2}s regions={} rotated={}",
            t.image_decode.as_secs_f64(),
            t.detection.preprocess.as_secs_f64(),
            t.detection.inference.as_secs_f64(),
            t.detection.postprocess.as_secs_f64(),
            t.recognition.preprocess.as_secs_f64(),
            t.recognition.inference.as_secs_f64(),
            t.recognition.postprocess.as_secs_f64(),
            t.total.as_secs_f64(),
            run.results.len(),
            t.rotated_regions,
        ))
    })
    .map_err(|e| e.to_string())
}

pub fn recognize_from_image(image: &image::DynamicImage) -> Result<String> {
    // Clone image so the closure owns it (needed for thread send + 'static)
    let image = image.clone();
    with_engine_timed(move |eng| {
        eng.run_from_image(&image)
            .map(|run| {
                run.results
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
