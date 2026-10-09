//! Preprocessing for the PaddleOCR (DBNet) path.
//!
//! Only **one** transform survived empirical review: upscaling small images so
//! DBNet can detect small glyphs. Grayscale conversion, histogram equalization
//! and projection-profile deskew were removed because they are net-negative for
//! PP-OCRv5 on real documents — measured on a scanned complaint, running the old
//! preprocessing cut recognized text from 270 to 43 chars on a page quadrant and
//! from 416 to 385 on a full page. The contrast gate also misfired: on a mostly
//! white text page the p5/p95 spread easily lands under the 100 threshold, so
//! equalization fired on ordinary clean pages and degraded them.

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

/// Returns `Some(temp_png_path)` when the image was upscaled, `None` when the
/// original is already large enough. Caller MUST delete the returned temp file.
pub fn preprocess_for_ocr(input_path: &Path) -> Result<Option<PathBuf>> {
    let img = image::open(input_path).context("failed to load image for preprocessing")?;

    let (w, h) = img.dimensions();
    let longest = w.max(h);
    if longest >= MIN_LONGEST_SIDE {
        return Ok(None);
    }

    let scale = TARGET_LONGEST_SIDE as f64 / longest as f64;
    let new_w = (w as f64 * scale).round().max(1.0) as u32;
    let new_h = (h as f64 * scale).round().max(1.0) as u32;
    let resized = img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);

    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = std::env::temp_dir().join(format!(
        "ls_preprocess_{}_{}.png",
        std::process::id(),
        seq
    ));

    resized
        .save(&temp_path)
        .context("failed to save preprocessed image")?;

    Ok(Some(temp_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Luma, Rgb};

    fn save_img_tmp(img: &DynamicImage) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ls_preprocess_test_{}_{}.png",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        img.save(&path).expect("save test image");
        path
    }

    /// Create a synthetic low-contrast gradient image (100×100).
    fn make_low_contrast_gradient() -> ImageBuffer<Luma<u8>, Vec<u8>> {
        let (w, h) = (100u32, 100u32);
        let pixels: Vec<u8> = (0..(w * h))
            .map(|i| 110u8 + ((i % 30) as u8))
            .collect();
        ImageBuffer::from_raw(w, h, pixels).expect("valid pixel buffer")
    }

    /// Create a clean, high-contrast image (1200×900).
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

    /// Count distinct gray-level values in an image.
    fn distinct_gray_levels(img: &ImageBuffer<Luma<u8>, Vec<u8>>) -> usize {
        let mut seen = [false; 256];
        for &Luma([v]) in img.pixels() {
            seen[v as usize] = true;
        }
        seen.iter().filter(|&&b| b).count()
    }

    /// Clean, high-contrast, already-large → returns Ok(None): no transform.
    #[test]
    fn no_op_for_large_clean_image() {
        let path = save_img_tmp(&DynamicImage::ImageRgb8(make_high_contrast_image()));
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        assert!(
            result.expect("ok").is_none(),
            "already-large image must not be preprocessed"
        );
    }

    /// Any large image — including a low-contrast one — must be left untouched,
    /// because equalization/deskew were removed as net-negative.
    #[test]
    fn no_op_for_large_low_contrast_image() {
        let gray = image::imageops::resize(
            &make_low_contrast_gradient(),
            1200,
            900,
            image::imageops::FilterType::Nearest,
        );
        let path = save_img_tmp(&DynamicImage::ImageLuma8(gray));
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        assert!(
            result.expect("ok").is_none(),
            "large image must never be equalized/deskewed"
        );
    }

    /// Small image (<1000px) → returns Some, longest side = 1000.
    #[test]
    fn upscale_small_image() {
        let small = ImageBuffer::<Luma<u8>, _>::from_fn(200, 150, |x, _| {
            Luma([100 + (x % 30) as u8])
        });
        let path = save_img_tmp(&DynamicImage::ImageLuma8(small));
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some for small image").expect("ok");
        let out_img = image::open(&out_path).expect("decodable");
        let _ = std::fs::remove_file(&out_path);

        let (w, h) = out_img.dimensions();
        let longest = w.max(h);
        assert_eq!(longest, 1000, "longest side should be 1000, got {longest}");
    }

    /// Upscaling preserves gradient information (output is not binarized).
    #[test]
    fn upscale_preserves_gray_levels() {
        let gray = make_low_contrast_gradient();
        let path = save_img_tmp(&DynamicImage::ImageLuma8(gray));
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some for small image").expect("ok");
        let out_img = image::open(&out_path).expect("decodable").to_luma8();
        let _ = std::fs::remove_file(&out_path);

        let levels = distinct_gray_levels(&out_img);
        assert!(
            levels >= 16,
            "expected ≥16 distinct gray levels, got {levels}"
        );
    }

    /// Temp file is a valid decodable PNG.
    #[test]
    fn output_is_valid_png() {
        let path = save_img_tmp(&DynamicImage::ImageLuma8(make_low_contrast_gradient()));
        let result = preprocess_for_ocr(&path);
        let _ = std::fs::remove_file(&path);

        let out_path = result.expect("should return Some").expect("ok");
        assert!(
            image::open(&out_path).is_ok(),
            "output should be a valid decodable image"
        );
        let _ = std::fs::remove_file(&out_path);
    }
}
