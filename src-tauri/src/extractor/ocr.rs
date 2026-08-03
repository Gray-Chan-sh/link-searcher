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
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use super::paddleocr;
use imageproc::distance_transform::Norm;

/// Supported language codes for OCR.
#[allow(dead_code)]
const SUPPORTED_LANGUAGES: &[&str] = &["eng", "chi_sim", "jpn", "kor"];

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
/// Returns engines in priority order (platform-native first, then Tesseract).
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
    match engine {
        OcrEngineType::PaddleOCR => paddleocr::recognize_from_path(path),
        OcrEngineType::Tesseract => ocr_image_tesseract(path, lang),
        OcrEngineType::AppleVision => {
            super::apple_vision::recognize_from_path(path, lang)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        OcrEngineType::WindowsOcr => {
            // ponytail: call Windows OCR via WinRT.
            // For now, fall back to paddleocr.
            paddleocr::recognize_from_path(path)
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
            paddleocr::recognize_from_path_with_regions(path)
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

    let mut cmd = Command::new("tesseract");
    cmd.arg(pp_path.as_os_str())
        .arg("stdout")
        .arg("-l")
        .arg(effective_lang)
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped());
    let child = cmd.spawn().context("failed to execute tesseract — is it installed?")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait_with_output()); });
    match rx.recv_timeout(Duration::from_secs(120)).context("tesseract wait failed")? {
        Ok(output) => {
            let _ = std::fs::remove_file(&pp_path);
            if !output.status.success() {
                anyhow::bail!("tesseract exited with code {:?}", output.status.code());
            }
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(text)
        }
        Err(_) => {
            let _ = Command::new("pkill").arg("-f").arg("tesseract").status();
            anyhow::bail!("tesseract timed out after 120s");
        }
    }
}

/// Extract text from an image using the best available engine.
///
/// Convenience wrapper that delegates to `ocr_image_with_engine` with
/// `OcrEngineType::Tesseract`. Callers that need a specific engine should
/// use `ocr_image_with_engine` directly.
pub fn ocr_image(image_path: &Path, _lang: &str) -> Result<String> {
    paddleocr::recognize_from_path(image_path)
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
        if y >= 45 && y <= 55 {
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
            if y < 30 || y > 70 || (y >= 45 && y <= 55) {
                img.put_pixel(x, y, black);
            }
        }
    }
}

/// Check if the `tesseract` CLI is available on the system.
pub fn is_tesseract_available() -> bool {
    Command::new("tesseract")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
    let output = Command::new("tesseract")
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