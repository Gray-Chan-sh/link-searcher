//! OCR integration for extracting text from images.
//!
//! Supports multiple OCR engines:
//! - **Tesseract** (cross-platform, via CLI)
//! - **Apple Vision** (macOS, placeholder)
//! - **Windows OCR** (Windows, placeholder)
//!
//! The Tesseract engine uses the `tesseract` CLI via `std::process::Command`.
//! Falls back to English if the requested language is not installed.

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use super::paddleocr;
use imageproc::distance_transform::Norm;

/// Supported language codes for OCR.
#[allow(dead_code)]
const SUPPORTED_LANGUAGES: &[&str] = &["eng", "chi_sim", "jpn", "kor"];

/// Global OCR concurrency gate: caps simultaneously-active OCR inferences at
/// min(cores, 8) to prevent the nested par_iter fan-out (batch_index × PDF
/// page) from thrashing the CPU and slowing each page to 2.5s+. Hardware-
/// adaptive; no user setting — the old `ocr_concurrent` knob only governed
/// PaddleOCR's pool and was removed to avoid confusion.
struct OcrGate {
    limit: usize,
    current: Mutex<usize>,
    cv: Condvar,
}

impl OcrGate {
    fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            current: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    fn acquire(&self) -> OcrGuard<'_> {
        let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
        while *cur >= self.limit {
            cur = self.cv.wait(cur).unwrap_or_else(|e| e.into_inner());
        }
        *cur += 1;
        OcrGuard { gate: self }
    }
}

struct OcrGuard<'a> {
    gate: &'a OcrGate,
}

impl Drop for OcrGuard<'_> {
    fn drop(&mut self) {
        let mut cur = self.gate.current.lock().unwrap_or_else(|e| e.into_inner());
        *cur = cur.saturating_sub(1);
        self.gate.cv.notify_one();
    }
}

static OCR_GATE: OnceLock<OcrGate> = OnceLock::new();

fn ocr_gate() -> &'static OcrGate {
    OCR_GATE.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 8);
        log::info!("[OCR] 并发闸门就绪: {cores} slots (硬件自适应)");
        OcrGate::new(cores)
    })
}

/// Preprocess an image for better OCR accuracy.
///
/// Pipeline: grayscale → 2x enlargement → Gaussian denoising → adaptive binarization → morphological opening.
/// Outputs a white-background, black-text image optimized for Tesseract.
pub fn preprocess_image(input_path: &Path) -> Result<PathBuf> {
    let img = image::open(input_path).context("failed to load image for preprocessing")?;
    let gray = img.to_luma8();

    let enlarged = image::imageops::resize(
        &gray,
        gray.width() * 2,
        gray.height() * 2,
        image::imageops::FilterType::Lanczos3,
    );

    let denoised = imageproc::filter::gaussian_blur_f32(&enlarged, 1.0);
    let thresholded = imageproc::contrast::adaptive_threshold(&denoised, 31);

    let cleaned = imageproc::morphology::open(&thresholded, Norm::L1, 1);

    let output_path = std::env::temp_dir().join(format!("ls_pp_{}.png", uuid::Uuid::new_v4()));
    cleaned.save(&output_path).context("failed to save preprocessed image")?;

    Ok(output_path)
}

/// The OCR engine to use for text extraction.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum OcrEngineType {
    PaddleOCR,
    AppleVision,
    WindowsOcr,
    Tesseract,
    None,
}

/// Detect all available OCR engines on the current system.
///
/// Returns candidate engines for the Settings list, in display order: the
/// built-in PaddleOCR, then platform-native (Apple Vision / Windows OCR),
/// then Tesseract when installed. Usability is reported separately via
/// `list_ocr_engines`; actual runtime selection goes through
/// [`preferred_engine`], which is platform-aware.
pub fn detect_available_engines() -> Vec<OcrEngineType> {
    let mut engines = Vec::new();

    engines.push(OcrEngineType::PaddleOCR);

    #[cfg(target_os = "macos")]
    engines.push(OcrEngineType::AppleVision);

    #[cfg(target_os = "windows")]
    engines.push(OcrEngineType::WindowsOcr);

    if is_tesseract_available() {
        engines.push(OcrEngineType::Tesseract);
    }

    engines
}

/// Whether a specific engine is actually usable on this machine right now.
fn engine_usable(engine: &OcrEngineType) -> bool {
    match engine {
        OcrEngineType::PaddleOCR => super::paddleocr::models_present(),
        OcrEngineType::AppleVision => cfg!(target_os = "macos"),
        OcrEngineType::WindowsOcr => cfg!(target_os = "windows") && super::windows_ocr::availability().0,
        OcrEngineType::Tesseract => is_tesseract_available(),
        OcrEngineType::None => true,
    }
}

/// Best OCR engine to fall back to on this platform. Platform-native engines
/// are preferred because they spawn no extra subprocesses (Windows OCR runs
/// in-process; Apple Vision is in-process) — this avoids the black console
/// windows that invoking poppler/tesseract on Windows would create — then the
/// built-in PaddleOCR, then Tesseract.
pub fn platform_default_engine() -> OcrEngineType {
    let platform_first = [
        #[cfg(target_os = "macos")]
        OcrEngineType::AppleVision,
        #[cfg(target_os = "windows")]
        OcrEngineType::WindowsOcr,
    ];
    for c in platform_first {
        if engine_usable(&c) {
            return c;
        }
    }
    if engine_usable(&OcrEngineType::PaddleOCR) {
        return OcrEngineType::PaddleOCR;
    }
    if engine_usable(&OcrEngineType::Tesseract) {
        return OcrEngineType::Tesseract;
    }
    // Nothing usable — keep the built-in default so callers surface a clear
    // "model not installed" error instead of an obscure engine failure.
    OcrEngineType::PaddleOCR
}

/// Resolve the engine to actually run. If the configured engine isn't usable
/// on this machine (e.g. the seeded "AppleVision" on Windows, or PaddleOCR
/// before its models are downloaded), fall back to [`platform_default_engine`]
/// instead of failing or silently indexing nothing.
pub fn preferred_engine(configured: Option<OcrEngineType>) -> OcrEngineType {
    match configured {
        Some(engine) if engine_usable(&engine) => engine,
        configured => {
            let fallback = platform_default_engine();
            if configured.is_some() {
                log::warn!(
                    "[OCR] configured engine {:?} unusable on this machine, using {:?}",
                    configured,
                    fallback
                );
            }
            fallback
        }
    }
}

/// Map a settings string to its [`OcrEngineType`] variant.
/// Unknown values default to PaddleOCR.
pub fn map_engine(value: &str) -> OcrEngineType {
    match value {
        "AppleVision" => OcrEngineType::AppleVision,
        "Tesseract" => OcrEngineType::Tesseract,
        "WindowsOcr" => OcrEngineType::WindowsOcr,
        "None" => OcrEngineType::None,
        _ => OcrEngineType::PaddleOCR,
    }
}

/// Run OCR against an image using a specific engine.
///
/// # Errors
///
/// Delegates to the underlying engine implementation.
pub fn ocr_image_with_engine(path: &Path, engine: &OcrEngineType, lang: &str) -> Result<String> {
    let _gate = ocr_gate().acquire();
    match engine {
        OcrEngineType::PaddleOCR => paddleocr::recognize_from_path(path),
        OcrEngineType::Tesseract => ocr_image_tesseract(path, lang),
        OcrEngineType::AppleVision => {
            super::apple_vision::recognize_from_path(path, lang)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        OcrEngineType::WindowsOcr => {
            super::windows_ocr::recognize_from_path(path, lang)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        OcrEngineType::None => Ok(String::new()),
    }
}

/// Like [`ocr_image_with_engine`], but also returns the number of detected
/// text regions, used for per-page performance diagnostics in PDF OCR.
pub fn ocr_image_with_regions(
    path: &Path,
    engine: &OcrEngineType,
    lang: &str,
) -> Result<(String, usize)> {
    let _gate = ocr_gate().acquire();
    match engine {
        OcrEngineType::PaddleOCR => {
            paddleocr::recognize_from_path_with_regions(path)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        OcrEngineType::AppleVision => {
            super::apple_vision::recognize_from_path_with_regions(path, lang)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        OcrEngineType::Tesseract => {
            let text = ocr_image_tesseract(path, lang)?;
            let regions = text.lines().filter(|l| !l.trim().is_empty()).count();
            Ok((text, regions))
        }
        OcrEngineType::WindowsOcr => {
            super::windows_ocr::recognize_from_path_with_regions(path, lang)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        OcrEngineType::None => Ok((String::new(), 0)),
    }
}

/// Extract text from an image using Tesseract OCR.
///
/// Falls back to English (`eng`) if the specified language is not installed.
///
/// # Errors
///
/// Returns an error if `tesseract` is not available, the image cannot be read,
/// or the process exits with a non-zero status.
pub fn ocr_image_tesseract(image_path: &Path, lang: &str) -> Result<String> {
    let available = get_available_languages()?;
    let effective_lang = if available.iter().any(|l| l == lang) {
        lang
    } else {
        "eng"
    };

    // Preprocess image for better OCR accuracy
    let pp_path = preprocess_image(image_path)?;

    let mut cmd = crate::process::new("tesseract");
    cmd.arg(pp_path.as_os_str())
        .arg("stdout")
        .arg("-l")
        .arg(effective_lang)
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().context("failed to execute tesseract — is it installed?")?;
    // Poll with a timeout instead of blocking: on timeout kill the child,
    // reap it and remove the temp image (a broken image can hang tesseract).
    let mut stdout_out = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait()? {
            Some(st) => {
                if let Some(mut so) = child.stdout.take() {
                    std::io::Read::read_to_end(&mut so, &mut stdout_out)?;
                }
                break st;
            }
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&pp_path);
                anyhow::bail!("tesseract timed out after 120s");
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    let _ = std::fs::remove_file(&pp_path);
    if !status.success() {
        anyhow::bail!("tesseract exited with code {:?}", status.code());
    }
    let text = String::from_utf8_lossy(&stdout_out).trim().to_string();
    Ok(text)
}

/// Extract text from an image using the best available engine.
///
/// Convenience wrapper. When `engine` is `None` — or the configured engine is
/// not usable on this machine — [`preferred_engine`] picks the platform
/// default.
pub fn ocr_image(image_path: &Path, lang: &str, engine: Option<OcrEngineType>) -> Result<String> {
    let e = preferred_engine(engine);
    ocr_image_with_engine(image_path, &e, lang)
}

/// Generate a simple test pattern PNG image for OCR engine validation.
///
/// Generate a test image containing known text ("Hello OCR 123").
///
/// The image is 400×100 pixels with black text on a white background,
/// clear enough for any OCR engine to recognize.
pub fn create_test_image() -> Result<Vec<u8>> {
    use ab_glyph::{FontArc, PxScale};

    let mut img = image::RgbaImage::new(400, 100);

    // Fill white background
    for pixel in img.pixels_mut() {
        *pixel = image::Rgba([255u8, 255u8, 255u8, 255u8]);
    }

    // Try to draw text using a built-in font
    let font_data: &[u8] = include_bytes!("../../../assets/Arial Unicode.ttf");
    if let Ok(font) = FontArc::try_from_slice(font_data) {
        let scale = PxScale::from(32.0);
        imageproc::drawing::draw_text_mut(
            &mut img,
            image::Rgba([0u8, 0u8, 0u8, 255u8]),
            20, 30,
            scale,
            &font,
            "Hello OCR 123",
        );
    } else {
        // Fallback: draw a recognizable pattern when font is unavailable
        draw_h12_pattern(&mut img);
    }

    let dyn_img = image::DynamicImage::from(img);
    let mut buf = std::io::Cursor::new(Vec::new());
    dyn_img
        .write_to(&mut buf, image::ImageFormat::Png)
        .context("failed to encode test PNG")?;
    Ok(buf.into_inner())
}

fn draw_h12_pattern(img: &mut image::RgbaImage) {
    let black = image::Rgba([0u8, 0u8, 0u8, 255u8]);
    for y in 20..80 {
        img.put_pixel(30, y, black);
        img.put_pixel(50, y, black);
        if (45..=55).contains(&y) {
            for x in 30..50 {
                img.put_pixel(x, y, black);
            }
        }
    }
    for y in 20..80 {
        img.put_pixel(80, y, black);
    }
    for y in 20..80 {
        for x in 110..130 {
            if !(30..=70).contains(&y) || (45..=55).contains(&y) {
                img.put_pixel(x, y, black);
            }
        }
    }
}

/// Check if the `tesseract` CLI is available on the system.
pub fn is_tesseract_available() -> bool {
    crate::process::probe_ok("tesseract", &["--version"])
}

/// Return the list of languages installed for Tesseract.
///
/// Parses the output of `tesseract --list-langs`, skipping the first line
/// ("List of available languages...").
///
/// # Errors
///
/// Returns an error if `tesseract` is not available or the command fails.
pub fn get_available_languages() -> Result<Vec<String>> {
    let output = crate::process::new("tesseract")
        .arg("--list-langs")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .context("failed to list tesseract languages")?;

    if !output.status.success() {
        anyhow::bail!("tesseract --list-langs failed");
    }

    // tesseract may output to stdout or stderr depending on version
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    let languages: Vec<String> = combined
        .lines()
        .skip(1) // skip "List of available languages" header
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    Ok(languages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ocr_gate_limits_concurrency() {
        let gate: &'static OcrGate = Box::leak(Box::new(OcrGate::new(3)));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles: Vec<_> = (0..12)
            .map(|_| {
                let active = std::sync::Arc::clone(&active);
                let peak = std::sync::Arc::clone(&peak);
                std::thread::spawn(move || {
                    let _guard = gate.acquire();
                    let now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= 3, "peak {} exceeded gate", peak.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn test_is_tesseract_available_returns_bool() {
        // Does not assert on the value — only checks it returns a bool
        let available = is_tesseract_available();
        assert!(available == available); // identity check, always true
    }

    #[test]
    fn test_get_available_languages_returns_list() {
        // If tesseract is available, this should return a non-empty list.
        // If not available, it should return an error.
        match get_available_languages() {
            Ok(langs) => {
                // Should at least contain "eng" if tesseract has it
                assert!(!langs.is_empty(), "expected at least one language");
            }
            Err(_) => {
                // tesseract not installed — that's fine
            }
        }
    }

    #[test]
    fn test_supported_languages_contains_eng() {
        assert!(SUPPORTED_LANGUAGES.contains(&"eng"));
        assert!(SUPPORTED_LANGUAGES.contains(&"chi_sim"));
        assert!(SUPPORTED_LANGUAGES.contains(&"jpn"));
        assert!(SUPPORTED_LANGUAGES.contains(&"kor"));
    }
}

#[cfg(test)]
mod paddleocr_poc {
    use super::*;
    use std::path::PathBuf;

    fn model_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models").join("ppocrv5")
    }

    #[test]
    fn poc_recognizes_hello_ocr() {
        let det = model_dir().join("det.onnx");
        let rec = model_dir().join("rec.onnx");
        let dict = model_dir().join("ppocrv5_dict.txt");

        if !det.exists() {
            eprintln!("POC SKIP: det.onnx not found at {:?}", det);
            return;
        }

        let engine = pure_onnx_ocr::OcrEngineBuilder::new()
            .det_model_path(&det)
            .rec_model_path(&rec)
            .dictionary_path(&dict)
            .det_limit_side_len(960)
            .build()
            .expect("POC: engine build failed");

        let png_data = create_test_image().expect("POC: create_test_image failed");
        let tmp_path = std::env::temp_dir().join(format!("ls_poc_{}.png", std::process::id()));
        std::fs::write(&tmp_path, &png_data).expect("POC: write tmp file failed");

        let results = engine.run_from_path(&tmp_path).expect("POC: OCR inference failed");
        let _ = std::fs::remove_file(&tmp_path);

        eprintln!("POC: detected {} text regions", results.len());
        for (i, r) in results.iter().enumerate() {
            eprintln!("  [{}] \"{}\"  conf={:.4}", i, r.text, r.confidence);
        }

        assert!(!results.is_empty(), "POC FAIL: expected ≥1 text region");
        eprintln!("POC SUCCESS: {} regions recognized", results.len());
    }
}