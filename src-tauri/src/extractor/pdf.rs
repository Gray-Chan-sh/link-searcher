use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;

use super::Extractor;

pub const LARGE_SCAN_PAGE_THRESHOLD: usize = 20;
pub const LARGE_SCAN_OCR_BUDGET: Duration = Duration::from_secs(600);

pub fn should_ocr_pages_individually(page_count: usize, whole_doc_failed: bool) -> bool {
    whole_doc_failed || page_count > LARGE_SCAN_PAGE_THRESHOLD
}

mod poppler;
pub(crate) use poppler::get_pdf_page_count;
pub use poppler::{is_pdfimages_available, is_pdftoppm_available, poppler_available};
use poppler::{pdftotext_path, run_with_timeout};

mod scan;
use scan::is_image_based_scan;
#[cfg(test)]
use scan::{full_page_image_pages, page_media_size};

/// Extract text via pdftotext (poppler) and check for watermarks/repetition.
/// Used as a fallback when lopdf cannot parse the PDF but the text layer is
/// still valid (common for digitally generated PDFs with stream errors).
fn try_pdftotext_extract(path: &Path) -> Option<String> {
    let bin = pdftotext_path()?;
    let mut cmd = crate::process::new(bin);
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

mod ocr;
pub use ocr::{ocr_pdf_via_pdfimages, ocr_pdf_via_pdftoppm};
use ocr::{merge_page_texts, ocr_single_pdf_page, page_needs_ocr, run_pdf_ocr_pipeline};

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
    ) -> Result<(String, bool)> {
        log::info!("[PDF] extracting {:?}", path.file_name());
        let engine = super::ocr::preferred_engine(engine);
        let doc = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lopdf::Document::load(path)
        })) {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => {
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
                    return Ok((text, false));
                }
                // anydoc handles Quartz/CFF PDFs that lopdf and pdftotext both fail on
                match anydoc::to_markdown(path) {
                    Ok(md) if md.len() > 100 => {
                        log::info!("[PDF] {:?}: anydoc fallback {} chars", path.file_name(), md.len());
                        return Ok((md, false));
                    }
                    Ok(_) => log::info!("[PDF] {:?}: anydoc returned empty, falling to image OCR", path.file_name()),
                    Err(anydoc_err) => log::info!("[PDF] {:?}: anydoc {anydoc_err}, falling to image OCR", path.file_name()),
                }
                log::info!(
                    "[PDF] {:?}: pdftotext unavailable/watermarked, falling to image OCR",
                    path.file_name()
                );
                return if let Some(text) = run_pdf_ocr_pipeline(path, 0, &[], lang, &engine) {
                    Ok((text, true))
                } else {
                    Err(anyhow::anyhow!(
                        "failed to load PDF and no OCR fallback available: {e}"
                    ))
                };
            }
            Err(_) => {
                log::warn!(
                    "[PDF] {:?}: lopdf panicked during load, trying pdftotext/anydoc fallback",
                    path.file_name()
                );
                if let Some(text) = try_pdftotext_extract(path) {
                    return Ok((text, false));
                }
                if let Some(text) = run_pdf_ocr_pipeline(path, 0, &[], lang, &engine) {
                    return Ok((text, true));
                }
                return Err(anyhow::anyhow!(
                    "lopdf panicked and no fallback available for {:?}",
                    path.file_name()
                ));
            }
        };
        let pages: Vec<u32> = doc.get_pages().into_keys().collect();
        if pages.is_empty() {
            return Ok((String::new(), false));
        }

        // Scans carry a synthetic, mis-ordered text layer that neither lopdf nor
        // poppler can reconstruct; OCR the page images instead. Some digital
        // PDFs (e.g. PowerPoint exports) also carry full-page background images
        // yet have a clean text layer — those must not pay for OCR.
        if is_image_based_scan(path, &doc) {
            if let Some(text) = try_pdftotext_extract(path)
                && prefer_recovered_text("", &text, pages.len())
            {
                log::info!(
                    "[PDF] {:?}: image-based scan but pdftotext layer is clean — using text layer ({} chars)",
                    path.file_name(),
                    text.chars().count()
                );
                return Ok((text, false));
            }
            if let Some(ocr_text) = run_pdf_ocr_pipeline(path, pages.len(), &[], lang, &engine) {
                log::info!(
                    "[PDF] {:?}: image-based scan — OCR bypassed text layer ({} chars)",
                    path.file_name(),
                    ocr_text.chars().count()
                );
                return Ok((ocr_text, true));
            }
        }

        // lopdf silently drops characters on fonts lacking a Name-valued
        // /Encoding; when poppler can recover a trustworthy text layer, use it
        // and skip the lossy lopdf pass entirely.
        if has_unparseable_font_encoding(&doc)
            && let Some(text) = try_pdftotext_extract(path)
            && !is_garbled_text(&text)
            && !is_implausible_text_layer(&text, pages.len())
            && !is_sparse_text_layer(&text, pages.len())
        {
            log::info!(
                "[PDF] {:?}: unparseable font /Encoding — pdftotext recovered {} chars",
                path.file_name(),
                text.len()
            );
            return Ok((text, false));
        }

        log::info!("[PDF] {:?}: {} pages, extracting text", path.file_name(), pages.len());
        let mut page_texts: Vec<String> = Vec::new();
        for page_num in &pages {
            let page_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                doc.extract_text(&[*page_num])
            }));
            match page_result {
                Ok(Ok(text)) => page_texts.push(text.trim_end_matches('\n').to_owned()),
                Ok(Err(e)) => {
                    log::warn!("[PDF] page {} extraction failed: {e}", page_num);
                    page_texts.push(String::new());
                }
                Err(_) => {
                    log::warn!("[PDF] page {} extraction panicked", page_num);
                    page_texts.push(String::new());
                }
            }
        }
        let merged = page_texts.join("\n");
        log::info!("[PDF] {:?}: extracted {} chars", path.file_name(), merged.len());
        let is_sparse = is_sparse_text_layer(&merged, pages.len());
        let is_implausible = is_implausible_text_layer(&merged, pages.len());

        // Capture pdf_inspector page-level info for per-page OCR decisions.
        // pages_needing_ocr is 0-indexed.
        let inspector_ocr_pages: Vec<u32> = if merged.len() > 100
            && let Ok(bytes) = std::fs::read(path) {
                match pdf_inspector::classify_pdf_mem(&bytes) {
                    Ok(class) => {
                        // Preserve existing whole-doc bypass for Scanned/ImageBased
                        let all_need_ocr = !class.pages_needing_ocr.is_empty()
                            && (class.pages_needing_ocr.len() * 2 > class.page_count as usize
                                || is_sparse);
                        if matches!(class.pdf_type, pdf_inspector::PdfType::Scanned | pdf_inspector::PdfType::ImageBased) || all_need_ocr {
                            log::info!(
                                "[PDF] {:?}: pdf-inspector={:?} (conf={:.0}%, {} ocr pages), bypassing text layer",
                                path.file_name(), class.pdf_type, class.confidence * 100., class.pages_needing_ocr.len()
                            );
                            if let Some(ocr_text) = run_pdf_ocr_pipeline(path, pages.len(), &page_texts, lang, &engine) {
                                return Ok((ocr_text, true));
                            }
                        }
                        class.pages_needing_ocr
                    }
                    Err(e) => {
                        log::warn!("[PDF] {:?}: pdf-inspector classify: {e}", path.file_name());
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

        let is_wm = is_watermark_text(&page_texts);
        let is_garbled = is_garbled_text(&merged);
        let is_rep = is_repetitive(&merged);
        let doc_clean = merged.len() > 100 && !is_garbled && !is_wm && !is_rep && !is_sparse && !is_implausible;

        // lopdf can produce watermark-only, garbled, or sparse text on PDFs
        // where pdftotext has the correct text layer — try pdftotext recovery.
        if !doc_clean {
            if let Some(text) = try_pdftotext_extract(path) {
                if prefer_recovered_text(&merged, &text, pages.len()) {
                    log::info!(
                        "[PDF] {:?}: pdftotext recovered {} chars (vs lopdf {})",
                        path.file_name(),
                        text.len(),
                        merged.len()
                    );
                    return Ok((text, false));
                }
            }
        }

        // Per-page refinement runs ONLY when the document-level text layer looks
        // healthy but individual pages may still be scanned/empty. When the
        // document-level layer is unusable (watermark/garbled/repetitive/sparse)
        // we must fall through to whole-document OCR, as before.
        if doc_clean {
            // Per-page OCR decisions: use pdf_inspector's per-page info when
            // available; fall back to conservative heuristic (assume images only
            // when the page text is sparse — the safest false-negative).
            let inspector_set: HashSet<usize> = inspector_ocr_pages.iter().map(|&p| p as usize).collect();
            let has_inspector_info = !inspector_set.is_empty();
            let needs_ocr: Vec<bool> = page_texts
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    let has_images = if has_inspector_info {
                        inspector_set.contains(&i)
                    } else {
                        is_sparse_text_layer(text, 1)
                    };
                    page_needs_ocr(text, has_images)
                })
                .collect();
            let ocr_count = needs_ocr.iter().filter(|&&b| b).count();
            let page_count = pages.len();

            if ocr_count == 0 {
                log::info!("[PDF] {:?}: clean text, skipping OCR", path.file_name());
                return Ok((merged, false));
            }
            if ocr_count < page_count {
                // MIXED: splice per-page OCR results into the text layer
                log::info!(
                    "[PDF] {:?}: mixed PDF — {}/{} pages need OCR, using per-page splice",
                    path.file_name(), ocr_count, page_count
                );
                let dpi = global_pdf_dpi();
                let mut ocr_pages = HashMap::new();
                for (i, &needs) in needs_ocr.iter().enumerate() {
                    if needs {
                        if let Some(text) = ocr_single_pdf_page(path, pages[i], dpi, lang, &engine) {
                            ocr_pages.insert(i, text);
                        }
                    }
                }
                if ocr_pages.len() == ocr_count {
                    let merged_text = merge_page_texts(&page_texts, &ocr_pages);
                    log::info!("[PDF] {:?}: per-page OCR complete, {} chars", path.file_name(), merged_text.len());
                    return Ok((merged_text, true));
                }
                log::warn!(
                    "[PDF] {:?}: per-page OCR partially failed ({}/{}), falling back to whole-document OCR",
                    path.file_name(), ocr_pages.len(), ocr_count
                );
            }
        }

        log::info!("[PDF] {:?}: wm={} garbled={} rep={} sparse={} implausible={} → falling to image-layer OCR ({lang})",
            path.file_name(), is_wm, is_garbled, is_rep, is_sparse, is_implausible);

        if let Some(ocr_text) = run_pdf_ocr_pipeline(path, pages.len(), &page_texts, lang, &engine) {
            return Ok((ocr_text, true));
        }

        Ok((merged, false))
    }

    pub fn extract_with_meta(
        &self,
        path: &Path,
        lang: &str,
        engine: Option<super::ocr::OcrEngineType>,
    ) -> Result<(String, super::quality::ExtractMeta)> {
        let (text, ocr_used) = self.extract_with_lang(path, lang, engine)?;
        let page_count = get_pdf_page_count(path).ok();
        let meta = super::quality::ExtractMeta {
            ocr_used,
            mean_confidence: None,
            page_count,
            image_dims: None,
            pre_sanitize_fffd_ratio: None,
        };
        Ok((text, meta))
    }
}

mod quality;
pub use quality::{
    is_garbled_text, is_implausible_text_layer, is_sparse_text_layer, is_watermark_text,
    prefer_recovered_text,
};
use quality::{has_unparseable_font_encoding, is_repetitive};

impl Extractor for PdfExtractor {
    /// Prefer [`extract_with_lang`] for language-aware extraction.
    fn extract(&self, path: &Path) -> Result<String> {
        let lang = global_ocr_lang();
        self.extract_with_lang(path, &lang, None).map(|(text, _ocr)| text)
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

/// Parse an optional DPI string into a clamped u32 in [100..=600], defaulting
/// to 300 on None, empty string, non-numeric, or out-of-range input.
pub fn normalize_pdf_dpi(raw: Option<&str>) -> u32 {
    let s = match raw {
        Some(v) if !v.is_empty() => v,
        _ => return 300,
    };
    let n = match s.parse::<u32>() {
        Ok(v) => v,
        Err(_) => return 300,
    };
    n.clamp(100, 600)
}

/// Read the `ocr_pdf_dpi` setting from SQLite, clamped to [100..=600].
pub fn global_pdf_dpi() -> u32 {
    static POOL: OnceLock<Option<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>> =
        OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let data_dir = crate::config::load_config().data_dir;
        let db_path = data_dir.join("data.db");
        crate::db::get_pool(&db_path.to_string_lossy()).ok()
    });
    let raw = match pool {
        Some(pool) => pool.get().ok().and_then(|conn| {
            conn.query_row(
                "SELECT value FROM app_settings WHERE key = 'ocr_pdf_dpi'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
        }),
        None => None,
    };
    normalize_pdf_dpi(raw.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual end-to-end extraction benchmark on a real PDF
    /// (`LS_TEST_PDF=<path> cargo test --lib tmp_e2e_extract_pdf -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn tmp_e2e_extract_pdf() {
        let Ok(p) = std::env::var("LS_TEST_PDF") else {
            return;
        };
        let engine = crate::extractor::ocr::OcrEngineType::PaddleOCR;
        let t = std::time::Instant::now();
        let extractor = PdfExtractor::new();
        match extractor.extract_with_lang(std::path::Path::new(&p), "chi_sim", Some(engine)) {
            Ok((text, ocr_used)) => eprintln!(
                "E2E: {} chars, ocr_used={}, {:.1}s",
                text.chars().count(),
                ocr_used,
                t.elapsed().as_secs_f64()
            ),
            Err(e) => eprintln!("E2E ERR: {e} after {:.1}s", t.elapsed().as_secs_f64()),
        }
    }
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

    fn create_pdf_with_font_encoding(
        path: &Path,
        encoding: Option<&[u8]>,
        with_to_unicode: bool,
    ) -> Result<()> {
        let mut doc = Document::new();
        let mut font_entries: Vec<(Vec<u8>, Object)> = vec![
            (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
            (b"Subtype".to_vec(), Object::Name(b"TrueType".to_vec())),
            (b"BaseFont".to_vec(), Object::Name(b"STSongti-SC-Regular".to_vec())),
        ];
        if let Some(enc) = encoding {
            font_entries.push((b"Encoding".to_vec(), Object::Name(enc.to_vec())));
        }
        if with_to_unicode {
            let cmap_id = doc.add_object(Stream::new(
                Dictionary::new(),
                b"begincmap endcmap".to_vec(),
            ));
            font_entries.push((b"ToUnicode".to_vec(), Object::Reference(cmap_id)));
        }
        let font_id = doc.add_object(Dictionary::from_iter(font_entries));

        let content_bytes = "BT /F1 12 Tf 100 700 Td (x) Tj ET".to_string();
        let content_id = doc.add_object(Stream::new(
            Dictionary::from_iter([(b"Length".to_vec(), Object::Integer(content_bytes.len() as i64))]),
            content_bytes.into_bytes(),
        ));
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
                Object::Dictionary(Dictionary::from_iter([(
                    b"Font".to_vec(),
                    Object::Dictionary(Dictionary::from_iter([(
                        b"F1".to_vec(),
                        Object::Reference(font_id),
                    )])),
                )])),
            ),
        ]));
        let pages_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
            (b"Kids".to_vec(), Object::Array(vec![Object::Reference(page_id)])),
            (b"Count".to_vec(), Object::Integer(1)),
        ]));
        if let Ok(page_dict) = doc.get_dictionary_mut(page_id) {
            page_dict.set("Parent", Object::Reference(pages_id));
        }
        let catalog_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
            (b"Pages".to_vec(), Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(path)?;
        Ok(())
    }

    #[test]
    fn test_unparseable_font_encoding_detected_without_encoding_but_with_tounicode() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_font_enc_risky");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("risky.pdf");
        create_pdf_with_font_encoding(&path, None, true)?;

        let doc = Document::load(&path)?;
        assert!(has_unparseable_font_encoding(&doc));

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_unparseable_font_encoding_ignored_when_encoding_named() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_font_enc_named");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("named.pdf");
        create_pdf_with_font_encoding(&path, Some(b"WinAnsiEncoding"), true)?;

        let doc = Document::load(&path)?;
        assert!(!has_unparseable_font_encoding(&doc));

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_unparseable_font_encoding_ignored_without_tounicode() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_font_enc_no_cmap");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("no_cmap.pdf");
        create_pdf_with_font_encoding(&path, None, false)?;

        let doc = Document::load(&path)?;
        assert!(!has_unparseable_font_encoding(&doc));

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_page_media_size_reads_direct_and_inherited() -> Result<()> {
        let dir = std::env::temp_dir().join("extractor_test_mediabox");
        std::fs::create_dir_all(&dir)?;

        for (name, on_page) in [("direct.pdf", true), ("inherited.pdf", false)] {
            let path = dir.join(name);
            let mut doc = Document::new();
            let page_id = doc.add_object(Dictionary::from_iter([
                (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            ]));
            let mediabox = Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]);
            let mut pages_entries = vec![
                (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
                (b"Kids".to_vec(), Object::Array(vec![Object::Reference(page_id)])),
                (b"Count".to_vec(), Object::Integer(1)),
            ];
            if !on_page {
                pages_entries.push((b"MediaBox".to_vec(), mediabox.clone()));
            }
            let pages_id = doc.add_object(Dictionary::from_iter(pages_entries));
            if let Ok(page_dict) = doc.get_dictionary_mut(page_id) {
                page_dict.set("Parent", Object::Reference(pages_id));
                if on_page {
                    page_dict.set("MediaBox", mediabox);
                }
            }
            let catalog_id = doc.add_object(Dictionary::from_iter([
                (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
                (b"Pages".to_vec(), Object::Reference(pages_id)),
            ]));
            doc.trailer.set("Root", Object::Reference(catalog_id));
            doc.save(&path)?;

            let doc = Document::load(&path)?;
            let pid = *doc.get_pages().values().next().unwrap();
            assert_eq!(page_media_size(&doc, pid), Some((612.0, 792.0)), "case {name}");
        }

        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn test_full_page_image_pages_detects_scan_coverage() {
        let listing = "\
page   num  type   width height color comp bpc  enc interp  object ID x-ppi y-ppi size ratio
--------------------------------------------------------------------------------------------
   1     0 image    1240  1754  rgb     3   8  jpeg   no       551  0   150   150 38.6K 0.6%
   1     1 stencil    24    16  -       1   1  ccitt  no         7  0   300   300 19B   40%
   2     0 image     200   150  rgb     3   8  jpeg   no       552  0   150   150 1.0K  0.6%
   3     0 image    2480  3508  rgb     3   8  jpeg   no       553  0   300   300 90K   0.6%";
        let a4 = (595.2_f32, 841.68_f32);
        let mut pages: Vec<u32> = full_page_image_pages(listing, |_| Some(a4)).into_iter().collect();
        pages.sort_unstable();
        // p1 (1240x1754@150) and p3 (2480x3508@300) both render to full A4.
        assert_eq!(pages, vec![1, 3]);
    }

    #[test]
    fn test_is_repetitive() {
        assert!(!is_repetitive("short"));
        let wm = "This document is confidential and for internal use only\n".repeat(3);
        assert!(is_repetitive(&wm), "repeated watermark line should be detected");
        let varied = (0..20).map(|i| format!("Line {i} of real content")).collect::<Vec<_>>().join("\n");
        assert!(!is_repetitive(&varied), "distinct lines should not be flagged");
    }

    #[test]
    fn test_normalize_pdf_dpi_none_defaults_300() {
        assert_eq!(normalize_pdf_dpi(None), 300);
    }

    #[test]
    fn test_normalize_pdf_dpi_zero_clamped_to_100() {
        assert_eq!(normalize_pdf_dpi(Some("0")), 100);
    }

    #[test]
    fn test_normalize_pdf_dpi_too_large_clamped_to_600() {
        assert_eq!(normalize_pdf_dpi(Some("9999")), 600);
    }

    #[test]
    fn test_normalize_pdf_dpi_valid_passthrough() {
        assert_eq!(normalize_pdf_dpi(Some("300")), 300);
    }

    #[test]
    fn test_normalize_pdf_dpi_invalid_falls_back_300() {
        assert_eq!(normalize_pdf_dpi(Some("abc")), 300);
    }

    #[test]
    fn test_normalize_pdf_dpi_negative_falls_back_300() {
        assert_eq!(normalize_pdf_dpi(Some("-5")), 300);
    }

    #[test]
    fn test_is_sparse_empty_text_is_sparse() {
        assert!(is_sparse_text_layer("", 5));
    }

    #[test]
    fn test_is_sparse_page_count_zero_returns_false() {
        assert!(!is_sparse_text_layer("some text", 0));
    }

    #[test]
    fn test_is_sparse_one_page_20_chars_is_sparse() {
        // 20 < 50 * 1 → true
        assert!(is_sparse_text_layer("abcdefghij1234567890", 1));
    }

    #[test]
    fn test_is_sparse_one_page_500_chars_not_sparse() {
        let text = "a".repeat(500);
        assert!(!is_sparse_text_layer(&text, 1));
    }

    #[test]
    fn test_is_sparse_ten_pages_400_chars_is_sparse() {
        // 400 < 50 * 10 = 500 → true
        assert!(is_sparse_text_layer(&"a".repeat(400), 10));
    }

    #[test]
    fn test_is_sparse_watermark_only_many_pages() {
        // 5 pages × 15 chars = 75 < 50 * 5 = 250 → true
        let wm = "confidential\n".repeat(5);
        assert!(is_sparse_text_layer(&wm, 5));
    }

    #[test]
    fn test_poppler_available_is_consistent_with_resolved_paths() {
        // Environment-dependent value — do NOT assert true/false, only that
        // the predicate is callable without panicking and equals the
        // conjunction of the two resolved-path predicates it derives from.
        let expected = is_pdftoppm_available() && is_pdfimages_available();
        assert_eq!(poppler_available(), expected);
    }

    #[test]
    fn test_page_needs_ocr_healthy_text_returns_false() {
        let text = "This is a well-formed page with substantial body text. \
            It contains far more than fifty non-whitespace characters and \
            is not garbled or repetitive.";
        assert!(!page_needs_ocr(text, false), "healthy text page should not need OCR");
        assert!(!page_needs_ocr(text, true), "healthy text page should not need OCR even with images");
    }

    #[test]
    fn test_page_needs_ocr_sparse_returns_true() {
        assert!(page_needs_ocr("short", false), "sparse page should need OCR");
    }

    #[test]
    fn test_page_needs_ocr_garbled_returns_true() {
        let garbled = "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}abc";
        assert!(page_needs_ocr(garbled, false), "garbled page should need OCR");
    }

    #[test]
    fn test_page_needs_ocr_empty_with_images_returns_true() {
        assert!(page_needs_ocr("", true), "empty page with images should need OCR");
    }

    #[test]
    fn test_page_needs_ocr_repetitive_with_images_returns_true() {
        let rep = "CONFIDENTIAL - INTERNAL USE ONLY\n".repeat(5);
        assert!(page_needs_ocr(&rep, true), "repetitive text with images should need OCR");
    }

    #[test]
    fn test_page_needs_ocr_repetitive_without_images_returns_false() {
        let rep = "CONFIDENTIAL - INTERNAL USE ONLY\n".repeat(5);
        assert!(!page_needs_ocr(&rep, false), "repetitive text without images should not need OCR");
    }

    #[test]
    fn test_should_ocr_pages_individually() {
        // Small doc, whole doc succeeded -> false
        assert!(!should_ocr_pages_individually(10, false));
        // Small doc, boundary 20, whole doc succeeded -> false
        assert!(!should_ocr_pages_individually(20, false));
        // Small doc, whole doc failed -> true
        assert!(should_ocr_pages_individually(10, true));
        // Large doc (> 20), whole doc succeeded -> true
        assert!(should_ocr_pages_individually(21, false));
        // Large doc (> 20), whole doc failed -> true
        assert!(should_ocr_pages_individually(21, true));
    }

    #[test]
    fn test_merge_page_texts_empty_ocr_map_equals_join() {
        let text_layer = vec![
            "Page one content".to_string(),
            "Page two content".to_string(),
        ];
        let ocr_pages = std::collections::HashMap::new();
        assert_eq!(merge_page_texts(&text_layer, &ocr_pages), "Page one content\nPage two content");
    }

    #[test]
    fn test_merge_page_texts_mixed_splices_ocr_in_order() {
        let text_layer = vec![
            "Page one content".to_string(),
            "Page two scan text".to_string(),
            "Page three content".to_string(),
        ];
        let mut ocr_pages = std::collections::HashMap::new();
        ocr_pages.insert(1, "OCR'd page two".to_string());
        let result = merge_page_texts(&text_layer, &ocr_pages);
        assert_eq!(result, "Page one content\nOCR'd page two\nPage three content");
    }

    #[test]
    fn test_merge_page_texts_ocr_overrides_text_layer() {
        let text_layer = vec![
            "Original text".to_string(),
            "Original page two".to_string(),
        ];
        let mut ocr_pages = std::collections::HashMap::new();
        ocr_pages.insert(0, "Replacement OCR".to_string());
        let result = merge_page_texts(&text_layer, &ocr_pages);
        assert_eq!(result, "Replacement OCR\nOriginal page two");
    }

    // ── is_implausible_text_layer tests ──────────────────────────────

    #[test]
    fn test_implausible_normal_legal_text_over_pages_returns_false() {
        // Simulates a 10-page contract: ~2000 chars/page → 20k total.
        // That's dense but plausible for a real legal document.
        let page = "合同编号 2024-LX-001 第一条 甲方与乙方经友好协商，就以下事宜达成一致条款如下。\
            本合同自签署之日起生效。本文包含中英文混合内容 Article 1 Section 2 \
            双方同意在本合同签署后三十日内完成交付。This is a test sentence for density. \
            The quick brown fox jumps over the lazy dog. 合同条款内容。";
        let text = page.repeat(10); // ~10 pages worth
        assert!(!is_implausible_text_layer(&text, 10));
    }

    #[test]
    fn test_implausible_binary_junk_low_printable_returns_true() {
        // Private Use Area + replacement chars are non-printable → low printable ratio.
        let mut junk = String::new();
        for _ in 0..200 {
            junk.push('\u{E000}');
            junk.push('\u{FFFD}');
        }
        assert!(is_implausible_text_layer(&junk, 5));
    }

    #[test]
    fn test_implausible_modest_text_one_page_returns_false() {
        let text = "This is a short document with just a few sentences on a single page.";
        assert!(!is_implausible_text_layer(&text, 1));
    }

    #[test]
    fn test_implausible_absurd_density_one_page_returns_true() {
        // 25,000 chars on 1 page → absurd density (>20k/page)
        let text = "A".repeat(25_000);
        assert!(is_implausible_text_layer(&text, 1));
    }

    #[test]
    fn test_implausible_empty_text_returns_false() {
        assert!(!is_implausible_text_layer("", 5));
    }

    // ── prefer_recovered_text tests ──────────────────────────────────

    #[test]
    fn test_prefer_recovered_text_bug_scenario_watermark_vs_clean() {
        // Simulates the confirmed bug: lopdf returns only watermark text,
        // pdftotext returns full document text.
        let watermark = "国家企业信用信息公示系统\n".repeat(20);
        let recovered = "企业信用信息公示报告 上海岩锦物业管理有限公司 法定代表人 注册资本 统一社会信用代码 ".repeat(80);
        assert!(prefer_recovered_text(&watermark, &recovered, 46));
    }

    #[test]
    fn test_prefer_recovered_text_recovered_shorter_rejected() {
        let lopdf = "Substantial document content here. ".repeat(100);
        let recovered = "short";
        assert!(!prefer_recovered_text(&lopdf, &recovered, 10));
    }

    #[test]
    fn test_prefer_recovered_text_recovered_garbled_rejected() {
        let lopdf = "Readable content here for comparison. ".repeat(10);
        let garbled = "\u{FFFD}\u{FFFD}\u{FFFD}".repeat(200);
        assert!(!prefer_recovered_text(&lopdf, &garbled, 5));
    }

    #[test]
    fn test_prefer_recovered_text_recovered_sparse_rejected() {
        let lopdf = "hi";
        let recovered = "short"; // 5 non-ws < 50*1 → sparse
        assert!(!prefer_recovered_text(&lopdf, &recovered, 1));
    }

    #[test]
    fn test_prefer_recovered_text_equal_length_rejected() {
        let text = "Same content for both. ".repeat(50);
        assert!(!prefer_recovered_text(&text, &text, 5));
    }
}
