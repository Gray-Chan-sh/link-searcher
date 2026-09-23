//! Scanned-PDF OCR pipeline: whole-doc fast path (pdfimages / pdftoppm) and
//! per-page OCR with a time budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::scanner::helpers::TempDir;

use super::poppler::{pdfimages_path, pdftoppm_path};
use super::quality::{is_garbled_text, is_repetitive, is_sparse_text_layer};
use super::{
    get_pdf_page_count, global_pdf_dpi, should_ocr_pages_individually, LARGE_SCAN_OCR_BUDGET,
    LARGE_SCAN_PAGE_THRESHOLD,
};
pub(super) fn run_pdf_ocr_pipeline(
    path: &Path,
    page_count: usize,
    page_texts: &[String],
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Option<String> {
    let page_count = if page_count == 0 {
        get_pdf_page_count(path).unwrap_or(0) as usize
    } else {
        page_count
    };

    if !should_ocr_pages_individually(page_count, false) {
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine) {
            return Some(ocr_text);
        }
    } else {
        log::info!(
            "[PDF] {:?}: large scanned PDF ({} pages > {}), attempting fast-path whole-doc OCR first",
            path.file_name(), page_count, LARGE_SCAN_PAGE_THRESHOLD
        );
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine) {
            return Some(ocr_text);
        }
    }

    if should_ocr_pages_individually(page_count, true) {
        log::info!(
            "[PDF] {:?}: running per-page OCR loop for {} pages with budget {:?}",
            path.file_name(), page_count, LARGE_SCAN_OCR_BUDGET
        );
        let dpi = global_pdf_dpi();
        let mut ocr_pages = HashMap::new();
        let start_time = Instant::now();
        let mut pages_attempted = 0usize;
        let mut budget_hit = false;

        for page_num in 1..=page_count {
            if start_time.elapsed() >= LARGE_SCAN_OCR_BUDGET {
                budget_hit = true;
                log::warn!(
                    "[PDF] {:?}: per-page OCR reached time budget {:?}, stopping early at page {}/{}",
                    path.file_name(), LARGE_SCAN_OCR_BUDGET, page_num, page_count
                );
                break;
            }
            pages_attempted += 1;
            let page_idx = page_num - 1;
            if let Some(p_text) = ocr_single_pdf_page(path, page_num as u32, dpi, lang, engine) {
                ocr_pages.insert(page_idx, p_text);
            }
        }

        log::info!(
            "[PDF] {:?}: per-page OCR completed: attempted {}/{} pages, got text for {} pages, budget_hit={}",
            path.file_name(), pages_attempted, page_count, ocr_pages.len(), budget_hit
        );

        if !ocr_pages.is_empty() {
            let merged = merge_page_texts(page_texts, &ocr_pages);
            if !merged.trim().is_empty() {
                return Some(merged);
            }
        }
    }

    None
}

/// Try OCR via pdfimages → pdftoppm, returning the first non-empty result.
fn try_ocr_fallback(path: &Path, lang: &str, engine: &crate::extractor::ocr::OcrEngineType) -> Option<String> {
    if pdfimages_path().is_some() {
        match ocr_pdf_via_pdfimages(path, lang, engine) {
            Ok(ocr_text) if ocr_text.len() > 100 => {
                log::info!(
                    "[PDF] pdfimages OCR for {:?} (OCR'd {} chars)",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    ocr_text.len(),
                );
                return Some(ocr_text);
            }
            Ok(_) => log::warn!("[PDF] pdfimages OCR returned empty text for {:?}", path.file_name()),
            Err(e) => log::warn!("[PDF] pdfimages OCR failed for {:?}: {e}", path.file_name()),
        }
    }
    if pdftoppm_path().is_some() {
        match ocr_pdf_via_pdftoppm(path, lang, engine) {
            Ok(ocr_text) if !ocr_text.is_empty() => {
                log::info!(
                    "[PDF] pdftoppm OCR for {:?} (OCR'd {} chars)",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    ocr_text.len(),
                );
                return Some(ocr_text);
            }
            Ok(_) => log::warn!("[PDF] pdftoppm OCR returned empty text for {:?}", path.file_name()),
            Err(e) => log::warn!("[PDF] pdftoppm OCR failed for {:?}: {e}", path.file_name()),
        }
    }
    None
}
pub(super) fn page_needs_ocr(page_text: &str, page_has_images: bool) -> bool {
    if is_sparse_text_layer(page_text, 1) {
        return true;
    }
    if is_garbled_text(page_text) {
        return true;
    }
    if page_has_images && is_repetitive(page_text) {
        return true;
    }
    false
}

pub(super) fn merge_page_texts(text_layer: &[String], ocr_pages: &HashMap<usize, String>) -> String {
    let parts: Vec<&str> = text_layer
        .iter()
        .enumerate()
        .map(|(i, tl)| match ocr_pages.get(&i) {
            Some(ocr) => ocr.as_str(),
            None => tl.as_str(),
        })
        .collect();
    parts.join("\n")
}

pub(super) fn ocr_single_pdf_page(
    path: &Path,
    page_number: u32,
    dpi: u32,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Option<String> {
    let bin = pdftoppm_path()?;
    let tmp_dir = TempDir::new("ls_pdf_page_ocr").ok()?;
    let output_prefix = tmp_dir.path().join("page");

    let mut cmd = crate::process::new(bin);
    cmd.args([
        "-png",
        "-r",
        &dpi.to_string(),
        "-f",
        &page_number.to_string(),
        "-l",
        &page_number.to_string(),
    ])
    .arg(path)
    .arg(&output_prefix);
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }

    let page_file = std::fs::read_dir(tmp_dir.path())
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("png"))?;

    let text = crate::extractor::ocr::ocr_image_with_engine(&page_file, engine, lang).ok()?;
    let trimmed = text.trim().to_owned();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Render PDF pages to images using pdftoppm and run OCR.
/// Returns extracted text from all pages.
pub fn ocr_pdf_via_pdftoppm(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Result<String> {
    let tmp_dir = TempDir::new("ls_pdf_ocr")?;
    log::info!("[PDF] pdftoppm: rendering {:?}", path.file_name());

    let output_prefix = tmp_dir.path().join("page");
    let bin = pdftoppm_path()
        .ok_or_else(|| anyhow::anyhow!("pdftoppm not available. Install poppler-utils."))?;
    let mut cmd = crate::process::new(bin);
    let dpi = global_pdf_dpi();
    cmd.args(["-png", "-r", &dpi.to_string()]).arg(path).arg(&output_prefix);
    cmd.stderr(Stdio::null());
    let mut child = cmd.spawn()
        .map_err(|e| anyhow::anyhow!("pdftoppm not available: {e}. Install poppler-utils."))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow::anyhow!("pdftoppm timed out after 120s"));
            }
            Err(e) => return Err(anyhow::anyhow!("pdftoppm error: {e}")),
        }
    };
    if !status.success() {
        return Err(anyhow::anyhow!("pdftoppm failed to render PDF"));
    }

    let page_files: Vec<_> = (1..).map(|n| tmp_dir.path().join(format!("page-{n}.png")))
        .take_while(|p| p.exists()).collect();
    log::info!(
        "[PDF] {:?}: {} page images, starting OCR ({}) [engine={:?}]",
        path.file_name(),
        page_files.len(),
        lang,
        engine,
    );

    use rayon::prelude::*;
    let page_texts: Vec<Option<String>> = page_files
        .par_iter()
        .map(|page_path| {
            let page_no = page_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_owned();
            let started = std::time::Instant::now();
            let result = crate::extractor::ocr::ocr_image_with_regions(page_path, engine, lang);
            match result {
                Ok((text, regions)) => {
                    log::info!(
                        "[PDF] page {}: {} chars from {} regions in {:.1}s",
                        page_no,
                        text.len(),
                        regions,
                        started.elapsed().as_secs_f64(),
                    );
                    Some(text)
                        .map(|t| t.trim().to_owned())
                        .filter(|t| !t.is_empty())
                }
                Err(e) => {
                    log::warn!("[PDF] page {} OCR failed: {e}", page_no);
                    None
                }
            }
        })
        .collect();

    let mut full_text = String::new();
    for text in page_texts.into_iter().flatten() {
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&text);
    }

    Ok(full_text)
}


/// Render scanned PDF pages via pdfimages (extracts only the image layer,
/// not overlays/annotations/watermarks). Returns the OCR'd text with far
/// less watermark contamination than pdftoppm-based rendering.
pub fn ocr_pdf_via_pdfimages(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Result<String> {
    log::info!("[PDF] pdfimages: extracting {:?}", path.file_name());

    let page_count = get_pdf_page_count(path)?;
    if page_count == 0 {
        return Ok(String::new());
    }
    let pages: Vec<u32> = (1..=page_count).collect();

    log::info!(
        "[PDF] {:?}: {} pages, extracting images via pdfimages",
        path.file_name(),
        page_count,
    );

    use rayon::prelude::*;
    let page_texts: Vec<Option<String>> = pages
        .par_iter()
        .map(|page_num| {
            let page_no = page_num.to_string();
            let started = std::time::Instant::now();
            match extract_and_ocr_page_via_pdfimages(path, *page_num, lang, engine) {
                Ok(text) if !text.trim().is_empty() => {
                    log::info!(
                        "[PDF] pdfimages page {page_no}: {} chars in {:.1}s",
                        text.len(),
                        started.elapsed().as_secs_f64(),
                    );
                    Some(text)
                }
                Ok(_) => {
                    log::warn!("[PDF] pdfimages page {page_no}: empty OCR result");
                    None
                }
                Err(e) => {
                    log::warn!("[PDF] pdfimages page {page_no}: {e}");
                    None
                }
            }
        })
        .collect();

    let pages_with_text = page_texts.iter().filter(|t| t.is_some()).count();
    if pages.len() > 2 && pages_with_text * 2 < pages.len() {
        return Err(anyhow::anyhow!(
            "pdfimages: only {pages_with_text}/{len} pages had images — not a scanned PDF",
            len = pages.len(),
        ));
    }

    let mut full_text = String::new();
    for text in page_texts.into_iter().flatten() {
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(text.trim());
    }

    Ok(full_text)
}

/// Extract images from a single PDF page using pdfimages, pick the largest
/// (the scanned page image), and OCR it.
fn extract_and_ocr_page_via_pdfimages(
    pdf_path: &Path,
    page_num: u32,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Result<String> {
    let tmp = TempDir::new("ls_pdfimg")?;
    let prefix = tmp.path().join("img");

    let bin = pdfimages_path()
        .ok_or_else(|| anyhow::anyhow!("pdfimages not available. Install poppler-utils."))?;
    let mut cmd = crate::process::new(bin);
    cmd.args([
        "-png",
        "-f",
        &page_num.to_string(),
        "-l",
        &page_num.to_string(),
    ])
    .arg(pdf_path)
    .arg(&prefix);
    cmd.stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .context("failed to spawn pdfimages")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow::anyhow!("pdfimages page {page_num} timed out after 30s"));
            }
            Err(e) => return Err(anyhow::anyhow!("pdfimages page {page_num}: {e}")),
        }
    };
    if !status.success() {
        return Err(anyhow::anyhow!("pdfimages page {page_num} failed"));
    }

    let mut best_path: Option<PathBuf> = None;
    let mut best_area: u64 = 0;
    for entry in std::fs::read_dir(tmp.path())
        .with_context(|| format!("failed to read pdfimages output dir for page {page_num}"))?
    {
        let entry = entry?;
        let img_path = entry.path();
        if img_path.extension().is_some_and(|e| e == "png") {
            match image::open(&img_path) {
                Ok(img) => {
                    let area = (img.width() as u64) * (img.height() as u64);
                    if area > best_area {
                        best_area = area;
                        best_path = Some(img_path);
                    }
                }
                Err(e) => {
                    log::warn!(
                        "[PDF] page {page_num}: failed to open image {:?}: {e}",
                        img_path.file_name()
                    );
                }
            }
        }
    }

    let best_path = best_path
        .ok_or_else(|| anyhow::anyhow!("pdfimages page {page_num}: no valid images found"))?;

    const MIN_PAGE_IMAGE_AREA: u64 = 100_000;
    if best_area < MIN_PAGE_IMAGE_AREA {
        return Err(anyhow::anyhow!(
            "pdfimages page {page_num}: largest image too small ({best_area} px²) — not a scanned page"
        ));
    }

    let (text, _regions) =
        crate::extractor::ocr::ocr_image_with_regions(&best_path, engine, lang)?;
    Ok(text)
}

