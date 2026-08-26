use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::Extractor;
use crate::scanner::helpers::TempDir;

/// Locate a poppler binary (`pdftoppm` or `pdfimages`).
/// Searches PATH first, then common Homebrew installation prefixes
/// so the Tauri app (which may not inherit the terminal PATH) can
/// find them.
fn find_poppler_binary(name: &str) -> Option<PathBuf> {
    if Command::new(name).arg("--version").output().is_ok() {
        return Some(PathBuf::from(name));
    }
    // Dev mode: look relative to project root
    let dev_path = PathBuf::from("poppler-bin").join(name);
    if dev_path.exists() && Command::new(&dev_path).arg("--version").output().is_ok() {
        return Some(dev_path);
    }
    // Release mode: look next to the executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent() {
            let bundle_path = dir.join(name);
            if bundle_path.exists() && Command::new(&bundle_path).arg("--version").output().is_ok() {
                return Some(bundle_path);
            }
        }
    for prefix in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let candidate = PathBuf::from(prefix).join(name);
        if candidate.exists() && Command::new(&candidate).arg("--version").output().is_ok() {
            return Some(candidate);
        }
    }
    None
}

static PDFTOPPM_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDFIMAGES_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn pdftoppm_path() -> Option<&'static Path> {
    PDFTOPPM_PATH.get_or_init(|| find_poppler_binary("pdftoppm")).as_deref()
}

fn pdfimages_path() -> Option<&'static Path> {
    PDFIMAGES_PATH.get_or_init(|| find_poppler_binary("pdfimages")).as_deref()
}

static PDFTOTEXT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn pdftotext_path() -> Option<&'static Path> {
    PDFTOTEXT_PATH.get_or_init(|| find_poppler_binary("pdftotext")).as_deref()
}

static PDFINFO_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn pdfinfo_path() -> Option<&'static Path> {
    PDFINFO_PATH.get_or_init(|| find_poppler_binary("pdfinfo")).as_deref()
}

/// Get the number of pages in a PDF. Uses pdfinfo first (tolerant of
/// malformed PDFs that lopdf rejects), falling back to lopdf.
fn get_pdf_page_count(path: &Path) -> Result<u32> {
    // Try pdfinfo first — handles broken streams that lopdf rejects
    if let Some(bin) = pdfinfo_path() {
        let mut cmd = Command::new(bin);
        cmd.arg(path);
        let (status, stdout) = run_with_timeout(cmd, Duration::from_secs(60))
            .unwrap_or((None, Vec::new()));
        if status.map(|s| s.success()).unwrap_or(false) {
            let stdout = String::from_utf8_lossy(&stdout);
            for line in stdout.lines() {
                if let Some(val) = line.strip_prefix("Pages:") {
                    return val.trim().parse::<u32>().context("invalid pdfinfo Pages output");
                }
            }
        }
    }
    // Fall back to lopdf
    let doc = lopdf::Document::load(path).context("failed to load PDF")?;
    Ok(doc.get_pages().len() as u32)
}

/// Run a command with a timeout, capturing stdout. A broken/crafted PDF can
/// hang poppler forever; the timeout lets the scan worker move on. A timeout
/// yields `Ok((None, vec![]))` so callers can fall back.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<(Option<std::process::ExitStatus>, Vec<u8>)> {
    cmd.stdout(Stdio::piped());
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok((None, Vec::new()));
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    let mut out = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        std::io::Read::read_to_end(&mut so, &mut out)?;
    }
    Ok((Some(status), out))
}

/// Extract text via pdftotext (poppler) and check for watermarks/repetition.
/// Used as a fallback when lopdf cannot parse the PDF but the text layer is
/// still valid (common for digitally generated PDFs with stream errors).
fn try_pdftotext_extract(path: &Path) -> Option<String> {
    let bin = pdftotext_path()?;
    let mut cmd = Command::new(bin);
    cmd.arg(path).arg("-");
    let (status, stdout) = run_with_timeout(cmd, Duration::from_secs(120)).ok()?;
    let status = status?;
    if !status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&stdout).to_string();
    if text.len() < 100 {
        return None;
    }
    // Split by form-feed for per-page watermark detection
    let pages: Vec<String> = text.split('\x0c')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if pages.len() >= 2 && is_watermark_text(&pages) {
        return None;
    }
    if is_repetitive(&text) {
        return None;
    }
    Some(text)
}

pub struct PdfExtractor;

fn default_engine() -> super::ocr::OcrEngineType {
    super::ocr::OcrEngineType::PaddleOCR
}

/// Try OCR via pdfimages → pdftoppm, returning the first non-empty result.
fn try_ocr_fallback(path: &Path, lang: &str, engine: &super::ocr::OcrEngineType) -> Option<String> {
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

/// Check whether most pages contain large embedded images (area ≥100K px²).
/// Returns true for scanned PDFs with an image per page, false for text-based
/// PDFs where the text layer is the actual content.
impl Default for PdfExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfExtractor {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_with_lang(
        &self,
        path: &Path,
        lang: &str,
        engine: Option<super::ocr::OcrEngineType>,
    ) -> Result<String> {
        log::info!("[PDF] extracting {:?}", path.file_name());
        let doc = match lopdf::Document::load(path) {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "[PDF] {:?}: lopdf failed to parse ({e}), trying pdftotext/anydoc fallback",
                    path.file_name()
                );
                // Digital PDFs often have clean text accessible via pdftotext
                if let Some(text) = try_pdftotext_extract(path) {
                    log::info!(
                        "[PDF] {:?}: pdftotext fallback ({}) chars",
                        path.file_name(),
                        text.len()
                    );
                    return Ok(text);
                }
                // anydoc handles Quartz/CFF PDFs that lopdf and pdftotext both fail on
                match anydoc::to_markdown(path) {
                    Ok(md) if md.len() > 100 => {
                        log::info!("[PDF] {:?}: anydoc fallback {} chars", path.file_name(), md.len());
                        return Ok(md);
                    }
                    Ok(_) => log::info!("[PDF] {:?}: anydoc returned empty, falling to image OCR", path.file_name()),
                    Err(anydoc_err) => log::info!("[PDF] {:?}: anydoc {anydoc_err}, falling to image OCR", path.file_name()),
                }
                log::info!(
                    "[PDF] {:?}: pdftotext unavailable/watermarked, falling to image OCR",
                    path.file_name()
                );
                let engine = engine.unwrap_or_else(default_engine);
                return if let Some(text) = try_ocr_fallback(path, lang, &engine) {
                    Ok(text)
                } else {
                    Err(anyhow::anyhow!(
                        "failed to load PDF and no OCR fallback available: {e}"
                    ))
                };
            }
        };
        let pages: Vec<u32> = doc.get_pages().into_keys().collect();
        if pages.is_empty() {
            return Ok(String::new());
        }
        log::info!("[PDF] {:?}: {} pages, extracting text", path.file_name(), pages.len());
        let mut page_texts: Vec<String> = Vec::new();
        for page_num in &pages {
            match doc.extract_text(&[*page_num]) {
                Ok(text) => page_texts.push(text.trim_end_matches('\n').to_owned()),
                Err(e) => {
                    log::warn!("[PDF] page {} extraction failed: {e}", page_num);
                    page_texts.push(String::new());
                }
            }
        }
        let merged = page_texts.join("\n");
        log::info!("[PDF] {:?}: extracted {} chars", path.file_name(), merged.len());
        let engine = engine.unwrap_or_else(default_engine);

        // Use pdf-inspector for accurate PDF classification
        if merged.len() > 100
            && let Ok(bytes) = std::fs::read(path) {
                match pdf_inspector::classify_pdf_mem(&bytes) {
                    Ok(class) => {
                        let need_ocr = !class.pages_needing_ocr.is_empty()
                            && class.pages_needing_ocr.len() * 2 > class.page_count as usize;
                        if matches!(class.pdf_type, pdf_inspector::PdfType::Scanned | pdf_inspector::PdfType::ImageBased) || need_ocr {
                            log::info!(
                                "[PDF] {:?}: pdf-inspector={:?} (conf={:.0}%, {} ocr pages), bypassing text layer",
                                path.file_name(), class.pdf_type, class.confidence * 100., class.pages_needing_ocr.len()
                            );
                            if let Some(ocr_text) = try_ocr_fallback(path, lang, &engine) {
                                return Ok(ocr_text);
                            }
                        }
                    }
                    Err(e) => log::warn!("[PDF] {:?}: pdf-inspector classify: {e}", path.file_name()),
                }
            }

        let is_wm = is_watermark_text(&page_texts);
        let is_garbled = is_garbled_text(&merged);
        let is_rep = is_repetitive(&merged);
        if merged.len() > 100 && !is_garbled && !is_wm && !is_rep {
            log::info!("[PDF] {:?}: clean text, skipping OCR", path.file_name());
            return Ok(merged);
        }
        // lopdf can produce whitespace-only text on Quartz/CFF PDFs —
        // pdftotext handles these correctly
        if is_garbled
            && let Some(text) = try_pdftotext_extract(path) {
                log::info!("[PDF] {:?}: pdftotext recovered {} chars from garbled text", path.file_name(), text.len());
                return Ok(text);
            }
        log::info!("[PDF] {:?}: wm={} garbled={} rep={} → falling to image-layer OCR ({lang})",
            path.file_name(), is_wm, is_garbled, is_rep);
        if let Some(ocr_text) = try_ocr_fallback(path, lang, &engine) {
            return Ok(ocr_text);
        }
        Ok(merged)
    }
}

/// Detect if extracted PDF text is garbled / corrupted.
/// Returns true if >30% of characters are suspicious.
pub fn is_garbled_text(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let total = text.chars().count() as f64;
    // >30% suspicious chars (replacement char, stray control chars)
    let suspicious = text
        .chars()
        .filter(|c| {
            *c == '\u{FFFD}'
                || (c.is_control() && *c != '\n' && *c != '\r' && *c != '\t')
        })
        .count() as f64;
    if suspicious / total > 0.3 {
        return true;
    }
    // <5% non-whitespace → lopdf parsed only spaces (e.g. Quartz PDFs)
    let non_blank = text.chars().filter(|c| !c.is_whitespace()).count() as f64;
    non_blank / total < 0.05
}

/// Normalize page text for watermark comparison: strip variable parts
/// (hex codes ≥30 chars, dates, URLs, whitespace) leaving only stable text.
fn normalize_for_watermark(text: &str) -> String {
    let mut out = String::with_capacity(300);
    let chars: Vec<char> = text.chars().take(300).collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Skip URLs
        if c == 'h' && i + 4 <= len && chars[i..i + 4] == ['h', 't', 't', 'p'] {
            while i < len && !chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        // Skip hex blobs (≥30 consecutive hex chars → verification codes, UUIDs)
        if c.is_ascii_hexdigit() {
            let start = i;
            while i < len && chars[i].is_ascii_hexdigit() {
                i += 1;
            }
            if i - start >= 30 {
                continue;
            }
            i = start;
        }
        // Skip date/time patterns: YYYY-MM-DD HH:MM:SS or YYYY.MM.DD
        if c.is_ascii_digit() {
            let start = i;
            while i < len
                && (chars[i].is_ascii_digit()
                    || chars[i] == '-'
                    || chars[i] == '.'
                    || chars[i] == ':')
            {
                i += 1;
            }
            let slice: String = chars[start..i].iter().collect();
            if slice.contains('-') || slice.contains(':')
                || (slice.contains('.') && slice.len() >= 8)
            {
                continue;
            }
            // Short pure-digit sequences (<5 chars) are PDF coordinate
            // garbage, not meaningful content. Skip them.
            if slice.chars().all(|c| c.is_ascii_digit()) && slice.len() <= 4 {
                continue;
            }
            i = start;
        }
        out.push(c);
        i += 1;
    }
    // Strip trailing page numbers like "1.", "12."
    while out.ends_with('.') {
        out.pop();
        while out.chars().next_back().is_some_and(|c| c.is_ascii_digit()) {
            out.pop();
        }
    }
    out
}

/// Detect if text across pages looks like a repeated watermark.
/// Normalizes each page's prefix (strips hex codes, dates, URLs, whitespace)
/// then checks whether adjacent normalized prefixes are identical. Returns
/// true if >80% of consecutive page pairs match.
pub fn is_watermark_text(pages: &[String]) -> bool {
    if pages.len() < 2 {
        return false;
    }
    let normalized: Vec<String> = pages
        .iter()
        .map(|p| normalize_for_watermark(p))
        .filter(|n| n.chars().count() > 2)
        .collect();
    if normalized.len() < 2 {
        return false;
    }
    let mut same = 0usize;
    for i in 1..normalized.len() {
        if normalized[i - 1] == normalized[i] {
            same += 1;
        }
    }
    let total = normalized.len() - 1;
    total > 0 && (same as f64 / total as f64) > 0.8
}

/// Returns true if text is highly repetitive — e.g. a watermark repeated
/// verbatim across lines/pages, which character-set Jaccard misses when
/// pages vary or only one page exists.
fn is_repetitive(text: &str) -> bool {
    if text.len() < 100 {
        return false;
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let distinct: HashSet<&str> = lines.iter().copied().collect();
    (lines.len() - distinct.len()) as f64 / lines.len() as f64 > 0.6
}

/// Render PDF pages to images using pdftoppm and run OCR.
/// Returns extracted text from all pages.
pub fn ocr_pdf_via_pdftoppm(
    path: &Path,
    lang: &str,
    engine: &super::ocr::OcrEngineType,
) -> Result<String> {
    let tmp_dir = TempDir::new("ls_pdf_ocr")?;
    log::info!("[PDF] pdftoppm: rendering {:?}", path.file_name());

    let output_prefix = tmp_dir.path().join("page");
    let bin = pdftoppm_path()
        .ok_or_else(|| anyhow::anyhow!("pdftoppm not available. Install poppler-utils."))?;
    let mut cmd = Command::new(bin);
    cmd.args(["-png", "-r", "200"]).arg(path).arg(&output_prefix);
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

/// Check if pdftoppm is available on the system.
pub fn is_pdftoppm_available() -> bool {
    pdftoppm_path().is_some()
}

/// Check if pdfimages is available on the system.
pub fn is_pdfimages_available() -> bool {
    pdfimages_path().is_some()
}

/// Render scanned PDF pages via pdfimages (extracts only the image layer,
/// not overlays/annotations/watermarks). Returns the OCR'd text with far
/// less watermark contamination than pdftoppm-based rendering.
pub fn ocr_pdf_via_pdfimages(
    path: &Path,
    lang: &str,
    engine: &super::ocr::OcrEngineType,
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
    engine: &super::ocr::OcrEngineType,
) -> Result<String> {
    let tmp = TempDir::new("ls_pdfimg")?;
    let prefix = tmp.path().join("img");

    let bin = pdfimages_path()
        .ok_or_else(|| anyhow::anyhow!("pdfimages not available. Install poppler-utils."))?;
    let mut cmd = Command::new(bin);
    cmd.args([
        "-png",
        "-f",
        &page_num.to_string(),
        "-l",
        &page_num.to_string(),
    ])
    .arg(pdf_path)
    .arg(&prefix);

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

impl Extractor for PdfExtractor {
    /// Prefer [`extract_with_lang`] for language-aware extraction.
    fn extract(&self, path: &Path) -> Result<String> {
        // Read the global ocr_lang setting instead of hard-coding "eng",
        // so direct extract() calls honor the user's language preference.
        let lang = global_ocr_lang();
        self.extract_with_lang(path, &lang, None)
    }
}

/// Best-effort read of the app-level OCR language setting. Falls back to
/// "eng" when the setting or DB is unavailable (e.g. in unit tests).
/// The connection pool is created once and cached.
fn global_ocr_lang() -> String {
    static POOL: OnceLock<Option<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>> =
        OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let data_dir = crate::config::load_config().data_dir;
        let db_path = data_dir.join("data.db");
        crate::db::get_pool(&db_path.to_string_lossy()).ok()
    });
    match pool {
        Some(pool) => match pool.get() {
            Ok(conn) => conn
                .query_row(
                    "SELECT value FROM app_settings WHERE key = 'ocr_lang'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap_or_else(|_| "eng".to_string()),
            Err(_) => "eng".to_string(),
        },
        None => "eng".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Document, Object, Stream};

    /// 大小写与空白不敏感的子串匹配（OCR 识别可能存在大小写/空格误差）
    fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
        let norm = |s: &str| -> String {
            s.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect()
        };
        norm(haystack).contains(&norm(needle))
    }

    fn create_test_pdf(path: &Path, text: &str) -> Result<()> {
        let mut doc = Document::new();

        // Create a font entry
        let font_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
            (b"Subtype".to_vec(), Object::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec())),
        ]));

        // Create content stream
        let content_bytes = format!(
            "BT /F1 12 Tf 100 700 Td ({}) Tj ET",
            text
        );
        let stream = Stream::new(
            Dictionary::from_iter([
                (b"Length".to_vec(), Object::Integer(content_bytes.len() as i64)),
            ]),
            content_bytes.into_bytes(),
        );
        let content_id = doc.add_object(stream);

        // Create page dictionary
        let page_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            (b"MediaBox".to_vec(), Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            (b"Contents".to_vec(), Object::Reference(content_id)),
            (
                b"Resources".to_vec(),
                Object::Dictionary(Dictionary::from_iter([
                    (
                        b"Font".to_vec(),
                        Object::Dictionary(Dictionary::from_iter([
                            (b"F1".to_vec(), Object::Reference(font_id)),
                        ])),
                    ),
                ])),
            ),
        ]));

        // Create pages tree
        let pages_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
            (b"Kids".to_vec(), Object::Array(vec![Object::Reference(page_id)])),
            (b"Count".to_vec(), Object::Integer(1)),
        ]));

        // Update page with Parent reference
        if let Ok(page_dict) = doc.get_dictionary_mut(page_id) {
            page_dict.set("Parent", Object::Reference(pages_id));
        }

        // Create catalog
        let catalog_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
            (b"Pages".to_vec(), Object::Reference(pages_id)),
        ]));

        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(path)?;
        Ok(())
    }

    #[test]
    fn test_pdf_extract_simple() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_pdf_simple");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("test.pdf");

        create_test_pdf(&path, "Hello PDF! This is a sufficiently long text to pass the garbled-text threshold and avoid OCR fallback in tests.")?;

        let extractor = PdfExtractor::new();
        let result = extractor.extract(&path)?;
        assert!(result.contains("Hello PDF!"));

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_pdf_multiple_pages() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_pdf_multi");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("multi.pdf");

        // Multi-page PDF using lopdf — second page
        let mut doc = Document::new();

        let font_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
            (b"Subtype".to_vec(), Object::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec())),
        ]));

        // Page 1 content
        let content1 = "BT /F1 12 Tf 100 700 Td (Page One) Tj ET";
        let stream1 = Stream::new(
            Dictionary::from_iter([
                (b"Length".to_vec(), Object::Integer(content1.len() as i64)),
            ]),
            content1.as_bytes().to_vec(),
        );
        let content_id1 = doc.add_object(stream1);

        let page_id1 = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            (b"MediaBox".to_vec(), Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            (b"Contents".to_vec(), Object::Reference(content_id1)),
            (
                b"Resources".to_vec(),
                Object::Dictionary(Dictionary::from_iter([
                    (
                        b"Font".to_vec(),
                        Object::Dictionary(Dictionary::from_iter([
                            (b"F1".to_vec(), Object::Reference(font_id)),
                        ])),
                    ),
                ])),
            ),
        ]));

        // Page 2 content
        let content2 = "BT /F1 12 Tf 100 700 Td (Page Two) Tj ET";
        let stream2 = Stream::new(
            Dictionary::from_iter([
                (b"Length".to_vec(), Object::Integer(content2.len() as i64)),
            ]),
            content2.as_bytes().to_vec(),
        );
        let content_id2 = doc.add_object(stream2);

        let page_id2 = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            (b"MediaBox".to_vec(), Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ])),
            (b"Contents".to_vec(), Object::Reference(content_id2)),
            (
                b"Resources".to_vec(),
                Object::Dictionary(Dictionary::from_iter([
                    (
                        b"Font".to_vec(),
                        Object::Dictionary(Dictionary::from_iter([
                            (b"F1".to_vec(), Object::Reference(font_id)),
                        ])),
                    ),
                ])),
            ),
        ]));

        let pages_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
            (
                b"Kids".to_vec(),
                Object::Array(vec![
                    Object::Reference(page_id1),
                    Object::Reference(page_id2),
                ]),
            ),
            (b"Count".to_vec(), Object::Integer(2)),
        ]));

        for pid in [page_id1, page_id2] {
            if let Ok(page_dict) = doc.get_dictionary_mut(pid) {
                page_dict.set("Parent", Object::Reference(pages_id));
            }
        }

        let catalog_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
            (b"Pages".to_vec(), Object::Reference(pages_id)),
        ]));

        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(&path)?;

        let extractor = PdfExtractor::new();
        let result = extractor.extract(&path)?;
        assert!(contains_ignore_case(&result, "Page One"), "result: {:?}", result);
        assert!(contains_ignore_case(&result, "Page Two"), "result: {:?}", result);

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_pdf_empty_pages() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_pdf_empty");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("empty.pdf");

        // PDF with no pages (trailer only)
        let mut doc = Document::new();
        let pages_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
            (b"Kids".to_vec(), Object::Array(vec![])),
            (b"Count".to_vec(), Object::Integer(0)),
        ]));
        let catalog_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
            (b"Pages".to_vec(), Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(&path)?;

        let extractor = PdfExtractor::new();
        let result = extractor.extract(&path)?;
        assert_eq!(result, "");

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_is_repetitive() {
        assert!(!is_repetitive("short"));
        let wm = "This document is confidential and for internal use only\n".repeat(3);
        assert!(is_repetitive(&wm), "repeated watermark line should be detected");
        let varied = (0..20).map(|i| format!("Line {i} of real content")).collect::<Vec<_>>().join("\n");
        assert!(!is_repetitive(&varied), "distinct lines should not be flagged");
    }
}
