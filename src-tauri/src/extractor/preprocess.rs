//! Gradient-preserving preprocessing for deep-learning OCR (PaddleOCR).
//!
//! Unlike the Tesseract-oriented pipeline in [`super::ocr::preprocess_image`]
//! which binarizes, this module preserves gradient information needed by
//! DBNet.  It conditionally upscales small images and applies contrast
//! enhancement only when the input is low-contrast.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use image::{GenericImageView, GrayImage};

/// Monotonically increasing counter for unique temp filenames.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Minimum longest-side length in pixels for DBNet to detect small text.
const MIN_LONGEST_SIDE: u32 = 1000;

/// Target longest-side length when upscaling small images.
const TARGET_LONGEST_SIDE: u32 = 1000;

/// Below this spread (p95 − p5 on 0–255) the image is considered low-contrast.
const LOW_CONTRAST_THRESHOLD: u8 = 100;

/// Longest side used for detection (angle is scale-invariant; smaller = faster).
const DESKEW_DETECT_MAX_SIDE: u32 = 400;

/// Skew below this (degrees) is left untouched to avoid harming clean docs.
const DESKEW_MIN_ANGLE_DEG: f32 = 2.0;

/// Skew above this (degrees) is too severe to auto-correct safely.
const DESKEW_MAX_ANGLE_DEG: f32 = 15.0;

const DESKEW_SEARCH_STEP_DEG: f32 = 0.5;
const DESKEW_SEARCH_RANGE_DEG: f32 = 10.0;

/// Projection variance below this means no text-line structure to deskew.
const DESKEW_MIN_PROFILE_VARIANCE: f32 = 1.0;

/// Minimum fraction of rows with zero ink at the detected angle, confirming
/// horizontal text lines exist (rejects edge/uniform images without structure).
const DESKEW_MIN_EMPTY_ROW_FRACTION: f32 = 0.02;

/// Detect the rotation (degrees) to apply to deskew a grayscale page.
///
/// Coarse projection-profile search: binarize a throwaway copy (Otsu),
/// rotate it through ±10° in 0.5° steps, and pick the angle whose
/// horizontal projection profile (per-row ink-count) has the maximum
/// variance — i.e. text lines collapse into the fewest, most distinct rows.
/// The OUTPUT image is never binarized; only this detection copy is.
fn detect_deskew_angle(gray: &GrayImage) -> f32 {
    use imageproc::contrast::{otsu_level, threshold, ThresholdType};
    use imageproc::geometric_transformations::{
        rotate_about_center, Interpolation,
    };

    let (w, h) = gray.dimensions();
    let longest = w.max(h);
    let detect = if longest > DESKEW_DETECT_MAX_SIDE {
        let scale = DESKEW_DETECT_MAX_SIDE as f64 / longest as f64;
        let new_w = (w as f64 * scale).round().max(1.0) as u32;
        let new_h = (h as f64 * scale).round().max(1.0) as u32;
        image::imageops::resize(gray, new_w, new_h, image::imageops::FilterType::Triangle)
    } else {
        gray.clone()
    };

    // ink = 0, background = 255 (detection only; output stays grayscale).
    let level = otsu_level(&detect);
    let binary = threshold(&detect, level, ThresholdType::Binary);

    let (bw, bh) = binary.dimensions();
    let mut best_angle = 0.0f32;
    let mut best_score = f32::NEG_INFINITY;
    let mut best_empty_rows = 0usize;

    let mut angle = -DESKEW_SEARCH_RANGE_DEG;
    while angle <= DESKEW_SEARCH_RANGE_DEG {
        let rotated = rotate_about_center(
            &binary,
            angle.to_radians(),
            Interpolation::Nearest,
            image::Luma([255u8]),
        );

        let mut row_ink = vec![0u32; bh as usize];
        for y in 0..bh {
            let mut count = 0u32;
            for x in 0..bw {
                if rotated.get_pixel(x, y)[0] == 0 {
                    count += 1;
                }
            }
            row_ink[y as usize] = count;
        }

        let mean = row_ink.iter().sum::<u32>() as f32 / bh as f32;
        let variance = row_ink
            .iter()
            .map(|&c| {
                let d = c as f32 - mean;
                d * d
            })
            .sum::<f32>()
            / bh as f32;

        if variance > best_score {
            best_score = variance;
            best_angle = angle;
            best_empty_rows = row_ink.iter().filter(|&&c| c == 0).count();
        }
        angle += DESKEW_SEARCH_STEP_DEG;
    }

    if best_score < DESKEW_MIN_PROFILE_VARIANCE {
        return 0.0;
    }
    let empty_fraction = best_empty_rows as f32 / bh as f32;
    if empty_fraction < DESKEW_MIN_EMPTY_ROW_FRACTION {
        return 0.0;
    }
    best_angle
}

/// Returns `Some(temp_png_path)` when the image was transformed, `None` when
/// the original is already suitable. Caller MUST delete the returned temp file.
pub fn preprocess_for_ocr(input_path: &Path) -> Result<Option<PathBuf>> {
    let img = image::open(input_path).context("failed to load image for preprocessing")?;
    let mut modified = false;

    // --- Step 1: Upscale small images (longest side < 1000 → 1000) ---
    let img = {
        let (w, h) = img.dimensions();
        let longest = w.max(h);
        if longest < MIN_LONGEST_SIDE {
            modified = true;
            let scale = TARGET_LONGEST_SIDE as f64 / longest as f64;
            let new_w = (w as f64 * scale).round() as u32;
            let new_h = (h as f64 * scale).round() as u32;
            img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
        } else {
            img
        }
    };

    // --- Step 2: Convert to grayscale (Luma8) ---
    let gray = img.to_luma8();

    // --- Step 3: Measure contrast and enhance if low ---
    let mut enhanced = gray.clone();
    {
        let mut histogram = [0u32; 256];
        for pixel in gray.iter() {
            histogram[*pixel as usize] += 1;
        }
        let total = gray.len() as u64;
        let mut cumulative = 0u64;
        let mut p5_val: u8 = 0;
        let mut p95_val: u8 = 255;
        let p5_target = total / 20; // 5%
        let p95_target = total * 19 / 20; // 95%

        for (val, &count) in histogram.iter().enumerate() {
            cumulative += count as u64;
            if p5_val == 0 && cumulative >= p5_target {
                p5_val = val as u8;
            }
            if cumulative >= p95_target {
                p95_val = val as u8;
                break;
            }
        }

        let spread = p95_val.saturating_sub(p5_val);
        if spread < LOW_CONTRAST_THRESHOLD {
            enhanced = imageproc::contrast::equalize_histogram(&gray);
            modified = true;
        }
    }

    // --- Step 4: Deskew (only for clearly-tilted pages) ---
    let deskew_angle = detect_deskew_angle(&enhanced);
    if deskew_angle.abs() > DESKEW_MIN_ANGLE_DEG
        && deskew_angle.abs() <= DESKEW_MAX_ANGLE_DEG
    {
        enhanced = imageproc::geometric_transformations::rotate_about_center(
            &enhanced,
            deskew_angle.to_radians(),
            imageproc::geometric_transformations::Interpolation::Bilinear,
            image::Luma([255u8]),
        );
        modified = true;
    }

    if !modified {
        return Ok(None);
    }

    // --- Step 5: Write temp PNG (3-channel RGB so every decoder accepts it) ---
    let rgb = image::DynamicImage::ImageLuma8(enhanced).to_rgb8();
    let dyn_rgb = image::DynamicImage::ImageRgb8(rgb);

    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = std::env::temp_dir().join(format!(
        "ls_preprocess_{}_{}.png",
        std::process::id(),
        seq
    ));

    dyn_rgb
        .save(&temp_path)
        .context("failed to save preprocessed image")?;

    Ok(Some(temp_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, GenericImageView, ImageBuffer, Luma, Rgb};

    /// Create a synthetic low-contrast gradient image (100×100).
    /// Pixels range from 110 to 140 — a spread of 30, well below threshold.
    fn make_low_contrast_gradient() -> ImageBuffer<Luma<u8>, Vec<u8>> {
        let (w, h) = (100u32, 100u32);
        let pixels: Vec<u8> = (0..(w * h))
            .map(|i| 110u8 + ((i % 30) as u8))
            .collect();
        ImageBuffer::from_raw(w, h, pixels).expect("valid pixel buffer")
    }

    /// Create a clean, high-contrast image (800×600) that needs no processing.
    fn make_high_contrast_image() -> ImageBuffer<Rgb<u8>, Vec<u8>> {
        let (w, h) = (1200u32, 900u32);
        let mut pixels = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 3) as usize;
                let val = if x < w / 2 { 10u8 } else { 240u8 };
                pixels[idx] = val;
                pixels[idx + 1] = val;
                pixels[idx + 2] = val;
            }
        }
        ImageBuffer::from_raw(w, h, pixels).expect("valid pixel buffer")
    }

    fn save_gray_tmp(img: &ImageBuffer<Luma<u8>, Vec<u8>>) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ls_preprocess_test_{}_{}.png",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        DynamicImage::ImageLuma8(img.clone())
            .save(&path)
            .expect("save test image");
        path
    }

    fn save_rgb_tmp(img: &ImageBuffer<Rgb<u8>, Vec<u8>>) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ls_preprocess_test_{}_{}.png",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        DynamicImage::ImageRgb8(img.clone())
            .save(&path)
            .expect("save test image");
        path
    }

    /// Count distinct gray-level values in an image.
    fn distinct_gray_levels(img: &ImageBuffer<Luma<u8>, Vec<u8>>) -> usize {
        let mut seen = [false; 256];
        for &Luma([v]) in img.pixels() {
            seen[v as usize] = true;
        }
        seen.iter().filter(|&&b| b).count()
    }

    /// Create a synthetic page of horizontal text lines (dark "ink" on white).
    /// Structured enough for projection-profile skew detection.
    fn make_text_page(w: u32, h: u32) -> ImageBuffer<Luma<u8>, Vec<u8>> {
        let mut img = ImageBuffer::from_pixel(w, h, Luma([255u8]));
        let line_height = 20u32;
        let line_gap = 16u32;
        let char_w = 12u32;
        let char_gap = 6u32;

        let mut y = 50u32;
        while y + line_height < h - 50 {
            let mut x = 50u32;
            while x + char_w < w - 50 {
                for cy in 0..line_height {
                    for cx in 0..char_w {
                        let px = x + cx;
                        let py = y + cy;
                        // Leave internal white gaps to mimic letterforms.
                        if !((cx + cy * 3) % 5 == 0 || (cx * 2 + cy) % 7 == 0) {
                            img.put_pixel(px, py, Luma([20u8]));
                        }
                    }
                }
                x += char_w + char_gap;
            }
            y += line_height + line_gap;
        }
        img
    }

    /// Rotate a straight text page by +4° to simulate a small scan skew.
    fn make_skewed_text_page() -> ImageBuffer<Luma<u8>, Vec<u8>> {
        let straight = make_text_page(1200, 900);
        imageproc::geometric_transformations::rotate_about_center(
            &straight,
            4.0_f32.to_radians(),
            imageproc::geometric_transformations::Interpolation::Bilinear,
            image::Luma([255u8]),
        )
    }

    // --- Test 1: Output is NOT binarized (many gray levels preserved) ---
    #[test]
    fn test_not_binarized_preserves_gray_levels() {
        let gray = make_low_contrast_gradient();
        let path = save_gray_tmp(&gray);
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some for low-contrast").expect("ok");
        let out_img = image::open(&out_path)
            .expect("decodable")
            .to_luma8();
        let _ = std::fs::remove_file(&out_path);

        let levels = distinct_gray_levels(&out_img);
        assert!(
            levels >= 16,
            "expected ≥16 distinct gray levels (not binarized), got {levels}"
        );
    }

    // --- Test 2: Clean, high-contrast, already-large → returns Ok(None) ---
    #[test]
    fn test_no_op_for_high_contrast_large() {
        let rgb = make_high_contrast_image();
        let path = save_rgb_tmp(&rgb);
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        assert!(
            result.expect("ok").is_none(),
            "high-contrast large image should not be preprocessed"
        );
    }

    // --- Test 3: Small image (<1000px) → returns Some, longest side = 1000 ---
    #[test]
    fn test_upscale_small_image() {
        let small = ImageBuffer::<Luma<u8>, _>::from_fn(200, 150, |x, _| {
            Luma([100 + (x % 30) as u8])
        });
        let path = save_gray_tmp(&small);
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some for small image").expect("ok");
        let out_img = image::open(&out_path).expect("decodable");
        let _ = std::fs::remove_file(&out_path);

        let (w, h) = out_img.dimensions();
        let longest = w.max(h);
        assert_eq!(longest, 1000, "longest side should be 1000, got {longest}");
    }

    // --- Test 4: Low-contrast → contrast spread increases ---
    #[test]
    fn test_low_contrast_enhanced() {
        let gray = make_low_contrast_gradient();
        let path = save_gray_tmp(&gray);

        // Measure input spread
        let input_spread = {
            let mut hist = [0u32; 256];
            for &Luma([v]) in gray.pixels() {
                hist[v as usize] += 1;
            }
            let total = gray.len() as u64;
            let mut cum = 0u64;
            let (mut p5, mut p95) = (0u8, 255u8);
            for (v, &c) in hist.iter().enumerate() {
                cum += c as u64;
                if p5 == 0 && cum >= total / 20 {
                    p5 = v as u8;
                }
                if cum >= total * 19 / 20 {
                    p95 = v as u8;
                    break;
                }
            }
            p95.saturating_sub(p5)
        };

        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some for low-contrast").expect("ok");
        let out_img = image::open(&out_path).expect("decodable").to_luma8();
        let _ = std::fs::remove_file(&out_path);

        let output_spread = {
            let mut hist = [0u32; 256];
            for &Luma([v]) in out_img.pixels() {
                hist[v as usize] += 1;
            }
            let total = out_img.len() as u64;
            let mut cum = 0u64;
            let (mut p5, mut p95) = (0u8, 255u8);
            for (v, &c) in hist.iter().enumerate() {
                cum += c as u64;
                if p5 == 0 && cum >= total / 20 {
                    p5 = v as u8;
                }
                if cum >= total * 19 / 20 {
                    p95 = v as u8;
                    break;
                }
            }
            p95.saturating_sub(p5)
        };

        assert!(
            output_spread > input_spread,
            "output spread ({output_spread}) should exceed input spread ({input_spread})"
        );
    }

    // --- Test 5: Temp file is a valid decodable PNG ---
    #[test]
    fn test_output_is_valid_png() {
        let gray = make_low_contrast_gradient();
        let path = save_gray_tmp(&gray);
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some").expect("ok");
        // If open succeeds, it's a valid image (PNG decoding verified)
        let img = image::open(&out_path);
        assert!(img.is_ok(), "output should be a valid decodable image");
        let _ = std::fs::remove_file(&out_path);
    }

    // --- Test 6: Straight high-contrast large text → not preprocessed (no deskew) ---
    #[test]
    fn test_deskew_straight_text_no_rotation() {
        let text = make_text_page(1200, 900);
        let path = save_gray_tmp(&text);
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        assert!(
            result.expect("ok").is_none(),
            "straight high-contrast large text should not be preprocessed"
        );
    }

    // --- Test 7: Skewed text → deskew measurably reduces the skew angle ---
    #[test]
    fn test_deskew_reduces_skew() {
        let skewed = make_skewed_text_page();
        let path = save_gray_tmp(&skewed);

        let input_angle = super::detect_deskew_angle(&skewed);

        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result
            .expect("should return Some for skewed text")
            .expect("ok");
        let out_img = image::open(&out_path).expect("decodable").to_luma8();
        let _ = std::fs::remove_file(&out_path);

        let output_angle = super::detect_deskew_angle(&out_img);

        assert!(
            input_angle.abs() > 2.0,
            "input should exhibit skew > 2°, got {input_angle}"
        );
        assert!(
            output_angle.abs() < input_angle.abs(),
            "output skew ({output_angle}) should be less than input ({input_angle})"
        );
        assert!(
            output_angle.abs() <= 1.0,
            "output skew should settle within ±1° of horizontal, got {output_angle}"
        );
    }
}
