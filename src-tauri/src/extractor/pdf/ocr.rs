//! Scanned-PDF OCR pipeline: whole-doc fast path (pdfimages / pdftoppm) and
//! per-page OCR with a time budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::scanner::helpers::TempDir;

use super::poppler::{pdf_longest_side_pt, pdfimages_path, pdftoppm_path};
use super::quality::{is_garbled_text, is_repetitive, is_sparse_text_layer};
use super::{
    get_pdf_page_count, global_pdf_dpi, should_ocr_pages_individually, LARGE_SCAN_OCR_BUDGET,
    LARGE_SCAN_PAGE_THRESHOLD,
};

/// Upper bound on the longest side (px) of a rendered page. Oversized pages
/// (e.g. a 3000×4000pt scan) would otherwise rasterize to >200 Mpx — slow and
/// above the OCR engine's image-decode limit. 2500px ≈ 214 DPI for A4, and the
/// OCR preprocessor normalizes the longest side to ~1000px anyway.
const MAX_RENDER_LONGEST_SIDE: u32 = 2500;
const MIN_RENDER_LONGEST_SIDE: u32 = 1000;

/// The largest image on a page smaller than this is an icon/logo, not a scan.
const MIN_PAGE_IMAGE_AREA: u64 = 100_000;

/// Timeout for the single whole-document `pdfimages` extraction. One pass over a
/// 40-page 80MB scan is ~0.1–2s; the timeout only guards pathological files.
const PDFIMAGES_DOC_TIMEOUT: Duration = Duration::from_secs(120);

/// Target longest-side pixels for rendering at `dpi`, clamped so a giant page
/// can't explode. Falls back to the cap when the page size is unknown.
fn render_longest_side(path: &Path, dpi: u32) -> u32 {
    let target = pdf_longest_side_pt(path)
        .map(|pt| (dpi as f64 / 72.0 * pt).round() as u32)
        .unwrap_or(MAX_RENDER_LONGEST_SIDE);
    target.clamp(MIN_RENDER_LONGEST_SIDE, MAX_RENDER_LONGEST_SIDE)
}
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
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine, true) {
            return Some(ocr_text);
        }
    } else {
        log::info!(
            "[PDF] {:?}: large scanned PDF ({} pages > {}), attempting fast-path whole-doc OCR first",
            path.file_name(), page_count, LARGE_SCAN_PAGE_THRESHOLD
        );
        // Skip whole-doc pdftoppm for large docs: rendering every page at once
        // takes minutes and reliably trips the 120s timeout, after which the
        // per-page loop below redoes the same work in parallel anyway.
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine, false) {
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
            match ocr_single_pdf_page(path, page_num as u32, dpi, lang, engine) {
                Some(p_text) => {
                    ocr_pages.insert(page_idx, p_text);
                }
                None => {
                    // Give up only if the first few pages all fail to render —
                    // avoids aborting a scan that has blank/failed pages before
                    // real content.
                    if ocr_pages.is_empty() && pages_attempted >= 5 {
                        log::warn!(
                            "[PDF] {:?}: aborting per-page OCR — first {pages_attempted} pages yielded no text",
                            path.file_name()
                        );
                        break;
                    }
                }
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

/// Try OCR via pdfimages → (optionally) pdftoppm, returning the first usable
/// result. `allow_whole_doc_pdftoppm` is false for large documents, where the
/// all-pages render is abandoned in favour of the budgeted per-page loop.
fn try_ocr_fallback(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
    allow_whole_doc_pdftoppm: bool,
) -> Option<String> {
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
    if allow_whole_doc_pdftoppm && pdftoppm_path().is_some() {
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

    let scale = render_longest_side(path, dpi).to_string();
    let mut cmd = crate::process::new(bin);
    cmd.args([
        "-png",
        "-scale-to",
        &scale,
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
    let scale = render_longest_side(path, dpi).to_string();
    cmd.args(["-png", "-scale-to", &scale]).arg(path).arg(&output_prefix);
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
///
/// A single whole-document `pdfimages` pass extracts every page image at once
/// (`-p` encodes the page number in each filename). The previous per-page
/// approach re-parsed the whole PDF once per page — on 40-page/80MB scans that
/// meant 40 process spawns racing a 30s timeout each (measured: whole-doc pass
/// ~0.1s, per-page frequently timed out under load).
pub fn ocr_pdf_via_pdfimages(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
) -> Result<String> {
    log::info!("[PDF] pdfimages: extracting {:?}", path.file_name());

    let page_count = get_pdf_page_count(path)? as usize;
    if page_count == 0 {
        return Ok(String::new());
    }

    // One `pdfimages -list` (0.1–0.2s) gives both the scan-coverage pre-check
    // and the per-page image encoding, so JPEGs can be extracted natively.
    let info = super::scan::image_list_info(path);

    // A digital/vector PDF with few (or zero) page images is NOT a scan — the
    // image pass would recover nothing. Only run it when most pages carry one.
    if let Some(ref i) = info {
        if i.pages_with_image == 0 || (i.total_pages > 2 && i.pages_with_image * 2 < i.total_pages) {
            return Err(anyhow::anyhow!(
                "pdfimages: only {}/{} pages have images — not a scanned PDF",
                i.pages_with_image, i.total_pages
            ));
        }
    }

    log::info!(
        "[PDF] {:?}: {} pages, extracting images via pdfimages",
        path.file_name(),
        page_count,
    );

    // JPEGs are copied out natively (`-j`, ~0.1s/page) instead of decoded and
    // re-encoded to PNG (~26s/page on a 12 MP scan). Use the native path only
    // when every listed image is JPEG; any other encoding needs `-png`.
    let all_jpeg = match &info {
        Some(i) => {
            !i.enc_by_page.is_empty()
                && i.enc_by_page
                    .values()
                    .all(|e| e.eq_ignore_ascii_case("jpeg"))
        }
        None => true,
    };

    let tmp = TempDir::new("ls_pdfimg")?;
    let prefix = tmp.path().join("img");
    let primary = if all_jpeg { "-j" } else { "-png" };
    run_pdfimages_doc(path, primary, &prefix)?;
    let mut per_page = group_images_by_page(tmp.path(), page_count)?;
    if per_page.iter().all(Option::is_none) && primary == "-j" {
        // `-j` skips non-JPEG encodings (CCITT/JBIG2/…): retry with PNG.
        run_pdfimages_doc(path, "-png", &prefix)?;
        per_page = group_images_by_page(tmp.path(), page_count)?;
    }

    // Count pages with an extracted image (NOT pages that OCR'd to text) — a
    // scan whose OCR yields little must not be misclassified as "not a scan".
    let pages_with_image = per_page.iter().filter(|p| p.is_some()).count();
    if page_count > 2 && pages_with_image * 2 < page_count {
        return Err(anyhow::anyhow!(
            "pdfimages: only {pages_with_image}/{page_count} pages had images — not a scanned PDF"
        ));
    }

    use rayon::prelude::*;
    let pages: Vec<u32> = (1..=page_count as u32).collect();
    let results: Vec<Option<String>> = pages
        .par_iter()
        .map(|&page_num| {
            let page_idx = (page_num - 1) as usize;
            let (best_path, best_area) = per_page.get(page_idx)?.as_ref()?;
            if *best_area < MIN_PAGE_IMAGE_AREA {
                return None;
            }
            let started = std::time::Instant::now();
            match crate::extractor::ocr::ocr_image_with_regions(best_path, engine, lang) {
                Ok((text, _regions)) => {
                    log::info!(
                        "[PDF] pdfimages page {page_num}: {} chars in {:.1}s",
                        text.len(),
                        started.elapsed().as_secs_f64(),
                    );
                    let trimmed = text.trim().to_owned();
                    if trimmed.is_empty() { None } else { Some(trimmed) }
                }
                Err(e) => {
                    log::warn!("[PDF] pdfimages page {page_num}: {e}");
                    None
                }
            }
        })
        .collect();

    let mut full_text = String::new();
    for text in results.into_iter().flatten() {
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&text);
    }

    Ok(full_text)
}

/// Run one whole-document `pdfimages <fmt> -p` pass into `prefix`. `-p` embeds
/// the 1-based page number in each output filename (`prefix-<page>-<n>.<ext>`),
/// so every page's images can be grouped and OCR'd without re-parsing the PDF.
fn run_pdfimages_doc(pdf_path: &Path, fmt: &str, prefix: &Path) -> Result<()> {
    let bin = pdfimages_path()
        .ok_or_else(|| anyhow::anyhow!("pdfimages not available. Install poppler-utils."))?;
    let mut cmd = crate::process::new(bin);
    cmd.args([fmt, "-p"]).arg(pdf_path).arg(prefix);
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().context("failed to spawn pdfimages")?;
    let deadline = Instant::now() + PDFIMAGES_DOC_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow::anyhow!("pdfimages timed out after {PDFIMAGES_DOC_TIMEOUT:?}"));
            }
            Err(e) => return Err(anyhow::anyhow!("pdfimages failed: {e}")),
        }
    };
    if !status.success() {
        return Err(anyhow::anyhow!("pdfimages failed"));
    }
    Ok(())
}

/// Image extensions `image::open` may be asked to decode across pdfimages'
/// output formats.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "ppm", "pgm", "pbm", "pam", "jp2", "j2k", "tif", "tiff", "bmp",
];

/// Group `pdfimages -p` output by page, keeping the largest decodable image on
/// each 1-based page as `(path, area_px)`. Pages without a usable image are
/// `None`.
fn group_images_by_page(dir: &Path, page_count: usize) -> Result<Vec<Option<(PathBuf, u64)>>> {
    let mut best: Vec<Option<(PathBuf, u64)>> = vec![None; page_count];
    for entry in std::fs::read_dir(dir)?.flatten() {
        let p = entry.path();
        let is_img = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if !is_img {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // `img-<page>-<n>` → rsplitn yields [n, page, head].
        let mut parts = stem.rsplitn(3, '-');
        let _n = parts.next();
        let Some(page) = parts.next().and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        if page == 0 || page > page_count {
            continue;
        }
        let Ok((w, h)) = image::image_dimensions(&p) else {
            continue;
        };
        let area = w as u64 * h as u64;
        let slot = &mut best[page - 1];
        if slot.as_ref().map(|(_, a)| area > *a).unwrap_or(true) {
            *slot = Some((p, area));
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32) {
        image::RgbImage::new(w, h).save(path).unwrap();
    }

    #[test]
    fn group_images_by_page_picks_largest_and_ignores_out_of_range() {
        let tmp = TempDir::new("ls_group_test").unwrap();
        // Page 1 carries two images — the larger must win.
        write_png(&tmp.path().join("img-001-000.png"), 50, 50);
        write_png(&tmp.path().join("img-001-001.png"), 400, 300);
        // Page 2 has no image (only a non-image file).
        std::fs::write(tmp.path().join("img-002-000.txt"), b"x").unwrap();
        // Page 3 has one image.
        write_png(&tmp.path().join("img-003-000.png"), 200, 200);
        // Out-of-range page must be ignored.
        write_png(&tmp.path().join("img-009-000.png"), 100, 100);

        let grouped = group_images_by_page(tmp.path(), 3).unwrap();
        assert_eq!(grouped.len(), 3);
        assert_eq!(grouped[0].as_ref().map(|(_, a)| *a), Some(400 * 300));
        assert!(grouped[1].is_none());
        assert_eq!(grouped[2].as_ref().map(|(_, a)| *a), Some(200 * 200));
    }
}

