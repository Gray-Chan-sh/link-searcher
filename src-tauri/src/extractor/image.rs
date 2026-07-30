use std::path::Path;

use anyhow::{Context, Result};
use image::GenericImageView;

use super::ocr;
use super::Extractor;

const MAX_DIMENSION: u32 = 4000;

pub struct ImageExtractor;

impl ImageExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for ImageExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        let img = image::open(path).context("failed to load image")?;

        if !ocr::is_tesseract_available() {
            return Ok(String::new());
        }

        let processed = preprocess_image(&img);
        let tmp_dir = std::env::temp_dir().join("link_searcher_ocr");
        std::fs::create_dir_all(&tmp_dir)?;

        let tmp_path = tmp_dir.join(format!("ocr_{}.png", uuid::Uuid::new_v4()));
        let result = processed.save(&tmp_path);
        if result.is_err() {
            return Ok(String::new());
        }

        let text = ocr::ocr_image(&tmp_path, "eng").unwrap_or_default();
        let _ = std::fs::remove_file(&tmp_path);
        Ok(text)
    }
}

/// Resize image if it exceeds MAX_DIMENSION on either axis, then convert to grayscale.
fn preprocess_image(img: &image::DynamicImage) -> image::DynamicImage {
    let (w, h) = img.dimensions();
    let resized = if w > MAX_DIMENSION || h > MAX_DIMENSION {
        img.resize(MAX_DIMENSION, MAX_DIMENSION, image::imageops::FilterType::Lanczos3)
    } else {
        img.clone()
    };
    resized.grayscale()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_image_extract_png() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_image_ocr_png");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("test.png");

        let img = image::DynamicImage::new_rgba8(1, 1);
        img.save(&path)?;

        let extractor = ImageExtractor::new();
        let result = extractor.extract(&path);
        assert!(result.is_ok(), "expected ok, got {:?}", result.err());
        // Result content depends on tesseract availability — just verify no error

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_image_extract_jpg() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_image_ocr_jpg");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("test.jpg");

        let img = image::DynamicImage::new_rgba8(1, 1);
        img.save(&path)?;

        let extractor = ImageExtractor::new();
        let result = extractor.extract(&path);
        assert!(result.is_ok(), "expected ok, got {:?}", result.err());

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_image_extract_invalid_returns_error() {
        let dir = std::env::temp_dir().join("extractor_test_image_ocr_invalid");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("invalid.png");

        std::fs::write(&path, b"not an image").unwrap();

        let extractor = ImageExtractor::new();
        let result = extractor.extract(&path);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_preprocess_image_resizes_large_image() {
        let img = image::DynamicImage::new_rgba8(5000, 3000);
        let processed = preprocess_image(&img);
        let (w, h) = processed.dimensions();
        assert!(w <= MAX_DIMENSION, "width {w} exceeds max");
        assert!(h <= MAX_DIMENSION, "height {h} exceeds max");
        assert_eq!(processed.color(), image::ColorType::La8);
    }

    #[test]
    fn test_preprocess_image_keeps_small_image() {
        let img = image::DynamicImage::new_rgba8(100, 100);
        let processed = preprocess_image(&img);
        let (w, h) = processed.dimensions();
        assert_eq!(w, 100);
        assert_eq!(h, 100);
        assert_eq!(processed.color(), image::ColorType::La8);
    }
}