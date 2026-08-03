use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use super::Extractor;
use crate::scanner::helpers::TempDir;

pub struct PdfExtractor;

impl PdfExtractor {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_with_lang(&self, path: &Path, lang: &str) -> Result<String> {
        log::info!("[PDF] extracting {:?}", path.file_name());
        let doc = lopdf::Document::load(path).context("failed to load PDF")?;
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
        let is_wm = is_watermark_text(&page_texts);
        let is_garbled = is_garbled_text(&merged);
        let is_rep = is_repetitive(&merged);
        if merged.len() > 100 && !is_garbled && !is_wm && !is_rep {
            log::info!("[PDF] {:?}: clean text, skipping OCR", path.file_name());
            return Ok(merged);
        }
        log::info!("[PDF] {:?}: wm={} garbled={} rep={} → falling to OCR ({lang})",
            path.file_name(), is_wm, is_garbled, is_rep);
        if is_pdftoppm_available() {
            match ocr_pdf_via_pdftoppm(path, lang) {
                Ok(ocr_text) if !ocr_text.is_empty() => {
                    log::info!(
                        "[PDF] OCR fallback for {:?} (extracted {} chars, OCR'd {} chars)",
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                        merged.len(),
                        ocr_text.len(),
                    );
                    return Ok(ocr_text);
                }
                Ok(_) => log::warn!("[PDF] OCR returned empty text for {:?}", path.file_name()),
                Err(e) => log::warn!("[PDF] OCR failed for {:?}: {e}", path.file_name()),
            }
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
    let suspicious = text
        .chars()
        .filter(|c| {
            *c == '\u{FFFD}' // replacement character
                || (c.is_control() && *c != '\n' && *c != '\r' && *c != '\t')
        })
        .count() as f64;
    suspicious / total > 0.3
}

/// Detect if text across pages looks like a repeated watermark.
/// Compares consecutive non-empty pages using Jaccard similarity on character sets.
/// Returns true if >80% of consecutive page pairs have similarity >0.8.
pub fn is_watermark_text(pages: &[String]) -> bool {
    if pages.len() < 2 {
        return false;
    }
    let non_empty: Vec<&str> = pages
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if non_empty.len() < 2 {
        return false;
    }
    let mut similar = 0;
    let total = non_empty.len() - 1;
    for i in 1..non_empty.len() {
        let prev: HashSet<char> = non_empty[i - 1].chars().collect();
        let curr: HashSet<char> = non_empty[i].chars().collect();
        let intersection = prev.intersection(&curr).count();
        let union = prev.union(&curr).count();
        let sim = if union > 0 {
            intersection as f64 / union as f64
        } else {
            1.0
        };
        if sim > 0.8 {
            similar += 1;
        }
    }
    total > 0 && (similar as f64 / total as f64) > 0.8
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
pub fn ocr_pdf_via_pdftoppm(path: &Path, lang: &str) -> Result<String> {
    let tmp_dir = TempDir::new("ls_pdf_ocr")?;
    log::info!("[PDF] pdftoppm: rendering {:?}", path.file_name());

    let output_prefix = tmp_dir.path().join("page");
    let mut cmd = Command::new("pdftoppm");
    cmd.args(["-png", "-r", "200"]).arg(path).arg(&output_prefix);
    let mut child = cmd.spawn()
        .map_err(|e| anyhow::anyhow!("pdftoppm not available: {e}. Install poppler-utils."))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(child.wait()); });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(_)) => return Err(anyhow::anyhow!("pdftoppm failed to render PDF")),
        Ok(Err(e)) => return Err(anyhow::anyhow!("pdftoppm error: {e}")),
        Err(_) => {
            let _ = Command::new("pkill").arg("-f").arg("pdftoppm").status();
            return Err(anyhow::anyhow!("pdftoppm timed out after 120s"));
        }
    }

    let page_files: Vec<_> = (1..).map(|n| tmp_dir.path().join(format!("page-{n}.png")))
        .take_while(|p| p.exists()).collect();
    log::info!("[PDF] {:?}: {} page images, starting OCR ({})", path.file_name(), page_files.len(), lang);

    use rayon::prelude::*;
    let page_texts: Vec<Option<String>> = page_files
        .par_iter()
        .map(|page_path| {
            crate::extractor::ocr::ocr_image(page_path, lang)
                .ok()
                .map(|text| text.trim().to_owned())
                .filter(|t| !t.is_empty())
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
    Command::new("pdftoppm")
        .arg("--version")
        .output()
        .is_ok()
}

impl Extractor for PdfExtractor {
    fn extract(&self, path: &Path) -> Result<String> {
        self.extract_with_lang(path, "eng")
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
