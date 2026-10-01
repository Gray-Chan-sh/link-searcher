//! Scanned-PDF OCR pipeline: whole-doc fast path (pdfimages / pdftoppm) and
//! per-page OCR guarded by a progress-based stall watchdog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::scanner::helpers::TempDir;

use super::poppler::{pdf_longest_side_pt, pdfimages_path, pdftoppm_path};
use super::quality::{is_garbled_text, is_repetitive, is_sparse_text_layer};
use super::{
    get_pdf_page_count, global_ocr_stall_timeout, global_pdf_dpi, should_ocr_pages_individually,
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
    skip_pdfimages: bool,
) -> Option<String> {
    let page_count = if page_count == 0 {
        get_pdf_page_count(path).unwrap_or(0) as usize
    } else {
        page_count
    };

    if !should_ocr_pages_individually(page_count, false) {
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine, true, skip_pdfimages) {
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
        if let Some(ocr_text) = try_ocr_fallback(path, lang, engine, false, skip_pdfimages) {
            return Some(ocr_text);
        }
    }

    if should_ocr_pages_individually(page_count, true) {
        log::info!(
            "[PDF] {:?}: running per-page OCR loop for {} pages (stall watchdog {:?})",
            path.file_name(), page_count, global_ocr_stall_timeout()
        );
        let dpi = global_pdf_dpi();
        let mut ocr_pages = HashMap::new();
        let stall = global_ocr_stall_timeout();
        let mut pages_attempted = 0usize;
        let mut stalled = false;

        for page_num in 1..=page_count {
            pages_attempted += 1;
            let page_idx = page_num - 1;
            let page_started = Instant::now();
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
            // Progress watchdog: every page that returns advances the loop, so a
            // slow-but-working machine is fine. Only a single page that blows the
            // stall budget is treated as stuck. `None` disables the guard.
            if let Some(stall) = stall {
                if page_started.elapsed() >= stall {
                    stalled = true;
                    log::warn!(
                        "[PDF] {:?}: per-page OCR stalled — page {page_num} took {:?} (>= {:?}), stopping at {}/{}",
                        path.file_name(), page_started.elapsed(), stall, page_num, page_count
                    );
                    break;
                }
            }
        }

        log::info!(
            "[PDF] {:?}: per-page OCR completed: attempted {}/{} pages, got text for {} pages, stalled={}",
            path.file_name(), pages_attempted, page_count, ocr_pages.len(), stalled
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
/// `skip_pdfimages` is set for pages carrying `/Rotate` 90°/270°: `pdfimages`
/// copies the raw embedded image without applying the rotation, so it only
/// yields garbage — go straight to `pdftoppm` (which bakes it in).
fn try_ocr_fallback(
    path: &Path,
    lang: &str,
    engine: &crate::extractor::ocr::OcrEngineType,
    allow_whole_doc_pdftoppm: bool,
    skip_pdfimages: bool,
) -> Option<String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    // pdfimages 结果被判乱码时先留着：万一 pdftoppm 也失败，宁可回退到它，
    // 也不要因为一个误判把整篇内容丢掉。
    let mut rejected: Option<String> = None;
    if pdfimages_path().is_some() && !skip_pdfimages {
        match ocr_pdf_via_pdfimages(path, lang, engine) {
            Ok(ocr_text) if ocr_text.len() > 100 => {
                // `len > 100` alone let rotated-page garbage through (~all real
                // hits were one char per line). Reject it and retry with the
                // rotation-aware renderer below.
                if ocr_text_is_unusable(&ocr_text) {
                    log::warn!(
                        "[PDF] pdfimages OCR for {name:?} looks garbled ({} chars) — retrying via pdftoppm (applies page /Rotate)",
                        ocr_text.len(),
                    );
                    rejected = Some(ocr_text);
                } else {
                    log::info!("[PDF] pdfimages OCR for {name:?} (OCR'd {} chars)", ocr_text.len());
                    return Some(ocr_text);
                }
            }
            Ok(_) => log::warn!("[PDF] pdfimages OCR returned empty text for {name:?}"),
            Err(e) => log::warn!("[PDF] pdfimages OCR failed for {name:?}: {e}"),
        }
    } else if skip_pdfimages {
        log::info!("[PDF] {name:?}: pages carry /Rotate — skipping pdfimages (it ignores page rotation)");
    }
    if allow_whole_doc_pdftoppm && pdftoppm_path().is_some() {
        match ocr_pdf_via_pdftoppm(path, lang, engine) {
            Ok(ocr_text) if !ocr_text.is_empty() => {
                log::info!("[PDF] pdftoppm OCR for {name:?} (OCR'd {} chars)", ocr_text.len());
                return Some(ocr_text);
            }
            Ok(_) => log::warn!("[PDF] pdftoppm OCR returned empty text for {name:?}"),
            Err(e) => log::warn!("[PDF] pdftoppm OCR failed for {name:?}: {e}"),
        }
    }
    if let Some(text) = rejected {
        log::warn!("[PDF] {name:?}: pdftoppm yielded nothing — keeping the (suspect) pdfimages text");
        return Some(text);
    }
    None
}

/// Whether pdfimages-OCR output is unusable and must be retried with pdftoppm.
///
/// The old gate was `len > 100`, which rotated-page garbage trivially passes:
/// OCR of a sideways page emits roughly one character per line (`"0\n冈\n半\n…"`),
/// so the text looks long but carries no words. Reuse the same quality metrics
/// the indexer uses, plus an explicit "newlines outnumber text" check.
fn ocr_text_is_unusable(text: &str) -> bool {
    use crate::extractor::quality::{ExtractMeta, QualityFlag, compute_quality};
    if is_garbled_text(text) {
        return true;
    }
    let newlines = text.chars().filter(|c| *c == '\n').count();
    let non_ws = text.chars().filter(|c| !c.is_whitespace()).count();
    // 换行比正文还密 ⇒ 每个字被拆成一行（旋转/误读的典型形态）
    if non_ws > 0 && newlines * 2 >= non_ws {
        return true;
    }
    let q = compute_quality(
        text,
        &ExtractMeta { ocr_used: true, ..Default::default() },
        "pdf",
    );
    q.flags.iter().any(|f| {
        matches!(
            f,
            QualityFlag::LowPrintable | QualityFlag::LowLexicon | QualityFlag::HighFffd
        )
    })
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

/// Splice per-page OCR results into the text layer, one line per page.
///
/// The page count is `max(text_layer.len(), highest OCR page + 1)` — the caller
/// may pass an **empty** text layer (the "no text layer at all" path forces OCR
/// with `&[]`), in which case iterating `text_layer` alone would drop every
/// OCR'd page and return an empty string. That regression silently discarded
/// 142 of 145 pages on a real scanned volume.
pub(super) fn merge_page_texts(text_layer: &[String], ocr_pages: &HashMap<usize, String>) -> String {
    let ocr_len = ocr_pages.keys().max().map(|m| m + 1).unwrap_or(0);
    let page_count = text_layer.len().max(ocr_len);
    let mut parts: Vec<&str> = Vec::with_capacity(page_count);
    for i in 0..page_count {
        if let Some(ocr) = ocr_pages.get(&i) {
            parts.push(ocr.as_str());
        } else if let Some(tl) = text_layer.get(i) {
            parts.push(tl.as_str());
        }
    }
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
    let stall = global_ocr_stall_timeout();
    run_pdfimages_doc(path, primary, &prefix, stall)?;
    let mut per_page = group_images_by_page(tmp.path(), page_count)?;
    if per_page.iter().all(Option::is_none) && primary == "-j" {
        // `-j` skips non-JPEG encodings (CCITT/JBIG2/…): retry with PNG. Clear
        // any partial output first so the retry's progress check and page
        // grouping only see the PNG run.
        clear_dir(tmp.path());
        run_pdfimages_doc(path, "-png", &prefix, stall)?;
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

/// Snapshot of `pdfimages` output used by the stall watchdog: (file count, total
/// bytes) for entries whose stem starts with `stem`. Growth means progress.
fn output_progress(dir: &Path, stem: &str) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            let matches = p
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|n| n.starts_with(stem))
                .unwrap_or(false);
            if matches {
                count += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (count, bytes)
}

/// Remove every regular file directly under `dir` (best-effort).
fn clear_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Run one whole-document `pdfimages <fmt> -p` pass into `prefix`. `-p` embeds
/// the 1-based page number in each output filename (`prefix-<page>-<n>.<ext>`),
/// so every page's images can be grouped and OCR'd without re-parsing the PDF.
///
/// `stall` is a progress watchdog, not a wall-clock budget: the process may run
/// as long as it needs, and is only killed after `stall` elapses with **no new
/// output** (a wedged/corrupt file). `None` disables the watchdog.
fn run_pdfimages_doc(
    pdf_path: &Path,
    fmt: &str,
    prefix: &Path,
    stall: Option<Duration>,
) -> Result<()> {
    let bin = pdfimages_path()
        .ok_or_else(|| anyhow::anyhow!("pdfimages not available. Install poppler-utils."))?;
    let mut cmd = crate::process::new(bin);
    cmd.args([fmt, "-p"]).arg(pdf_path).arg(prefix);
    cmd.stderr(Stdio::null());

    let dir = prefix
        .parent()
        .ok_or_else(|| anyhow::anyhow!("bad pdfimages output prefix"))?;
    let stem = prefix.file_name().and_then(|s| s.to_str()).unwrap_or("img");

    let mut child = cmd.spawn().context("failed to spawn pdfimages")?;
    let mut last_progress = Instant::now();
    let mut last = output_progress(dir, stem);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                let now = output_progress(dir, stem);
                if now != last {
                    last = now;
                    last_progress = Instant::now();
                } else if let Some(stall) = stall {
                    if last_progress.elapsed() >= stall {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(anyhow::anyhow!(
                            "pdfimages stalled: no output progress for {stall:?}"
                        ));
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
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
    fn output_progress_counts_and_sums_matching_files() {
        let tmp = TempDir::new("ls_progress_test").unwrap();
        assert_eq!(output_progress(tmp.path(), "img"), (0, 0));
        std::fs::write(tmp.path().join("img-001-000.png"), b"abcd").unwrap();
        std::fs::write(tmp.path().join("img-002-000.png"), b"abcdefgh").unwrap();
        // Non-matching entries are ignored.
        std::fs::write(tmp.path().join("other.txt"), b"xxxxxxxxxx").unwrap();
        assert_eq!(output_progress(tmp.path(), "img"), (2, 12));
    }

    #[test]
    fn clear_dir_removes_files_only() {
        let tmp = TempDir::new("ls_clear_test").unwrap();
        std::fs::write(tmp.path().join("img-001-000.png"), b"x").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        clear_dir(tmp.path());
        assert_eq!(output_progress(tmp.path(), "img"), (0, 0));
        assert!(tmp.path().join("sub").is_dir());
    }

    #[test]
    fn merge_page_texts_splices_ocr_into_text_layer() {
        let tl = vec!["页1原生".to_string(), "页2原生".to_string()];
        let mut ocr = HashMap::new();
        ocr.insert(1usize, "页2 OCR".to_string());
        assert_eq!(merge_page_texts(&tl, &ocr), "页1原生\n页2 OCR");
    }

    /// 回归：强制 OCR 路径传的是**空** text_layer。旧实现只遍历 text_layer，
    /// 会把所有 OCR 结果丢掉、返回空串（实测在 145 页卷宗上丢了 142 页文字）。
    #[test]
    fn merge_page_texts_handles_empty_text_layer() {
        let mut ocr = HashMap::new();
        for i in 0..142usize {
            ocr.insert(i, format!("第{i}页OCR"));
        }
        let merged = merge_page_texts(&[], &ocr);
        assert!(merged.starts_with("第0页OCR"), "空 text_layer 也必须保留 OCR 结果");
        assert!(merged.contains("第141页OCR"), "最后一页不能丢");
        assert_eq!(merged.lines().count(), 142, "页数应等于 OCR 命中页数");
    }

    #[test]
    fn merge_page_texts_prefers_ocr_and_keeps_unmatched_pages() {
        // OCR 只覆盖部分页时，其余页保留原生文本；页数以两者最大值为准
        let tl = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut ocr = HashMap::new();
        ocr.insert(1usize, "B".to_string());
        ocr.insert(3usize, "D".to_string()); // 超出 text_layer 页数
        let merged = merge_page_texts(&tl, &ocr);
        assert_eq!(merged, "a\nB\nc\nD");
    }

    #[test]
    fn merge_page_texts_empty_inputs() {
        assert_eq!(merge_page_texts(&[], &HashMap::new()), "");
    }

    #[test]
    fn ocr_gate_rejects_rotated_page_garbage() {
        // 旋转扫描件的 pdfimages OCR 输出长这样：每个字一行、没有词。
        // 旧门槛只看 len>100，会被它骗过 → 必须判为不可用。
        let garbage = "0\n冈\n半\n叫\n国\n悔\n溲\n叫\n世\n瞓\n绊 7\n屉\n专\n奬\n瞓\n甲\n0 、\n0\n冈\n叫\n跹\n叵\n0\n0\n叫\n逭\n《 0\n典\n叵\n叫\n岬 鋈\n回 .\n";
        assert!(ocr_text_is_unusable(garbage));
        // 正常的 OCR 判决书正文（多字成行）必须放行。
        let normal = "上海市徐汇区人民法院\n民事判决书\n(2019)沪0104民初16644号\n原告:上海尊信科技服务(集团)有限责任公司,住所地上海市闵行区七莘路1855号第1幢208室。\n法定代表人:卢前荣,该公司董事长。\n被告:上海典欧实业有限公司。\n本院认为,涉案协议系双方就原告代理被告申请注册商标事宜达成的合意,合法有效,对原被告均有约束力。\n";
        assert!(!ocr_text_is_unusable(normal));
    }

    #[test]
    fn ocr_gate_rejects_empty_and_short() {
        assert!(ocr_text_is_unusable(""));
        assert!(ocr_text_is_unusable("\n\n   \n"));
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

