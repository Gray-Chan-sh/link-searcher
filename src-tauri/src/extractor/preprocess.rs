//! Gradient-preserving preprocessing for deep-learning OCR (PaddleOCR).
//!
//! Unlike the Tesseract-oriented pipeline in [`super::ocr::preprocess_image`]
//! which binarizes, this module preserves gradient information needed by
//! DBNet.  It conditionally upscales small images and applies contrast
//! enhancement only when the input is low-contrast.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use image::GenericImageView;

/// Monotonically increasing counter for unique temp filenames.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Minimum longest-side length in pixels for DBNet to detect small text.
const MIN_LONGEST_SIDE: u32 = 1000;

/// Target longest-side length when upscaling small images.
const TARGET_LONGEST_SIDE: u32 = 1000;

/// Below this spread (p95 − p5 on 0–255) the image is considered low-contrast.
const LOW_CONTRAST_THRESHOLD: u8 = 100;

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

    if !modified {
        return Ok(None);
    }

    // --- Step 4: Write temp PNG (3-channel RGB so every decoder accepts it) ---
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
}
