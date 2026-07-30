use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use super::Extractor;

pub struct PdfExtractor;

impl PdfExtractor {
    pub fn new() -> Self {
        Self
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

/// Render PDF pages to images using pdftoppm and run OCR.
/// Returns extracted text from all pages.
pub fn ocr_pdf_via_pdftoppm(path: &Path, lang: &str) -> Result<String> {
    let tmp_dir =
        std::env::temp_dir().join(format!("ls_pdf_ocr_{}", std::process::id()));
    std::fs::create_dir_all(&tmp_dir)?;

    let output_prefix = tmp_dir.join("page");
    let status = std::process::Command::new("pdftoppm")
        .args(["-png", "-r", "200"])
        .arg(path)
        .arg(&output_prefix)
        .status()
        .map_err(|e| anyhow::anyhow!("pdftoppm not available: {e}. Install poppler-utils."))?;

    if !status.success() {
        return Err(anyhow::anyhow!("pdftoppm failed to render PDF"));
    }

    let mut full_text = String::new();
    let mut page_num = 1;
    loop {
        let page_path = tmp_dir.join(format!("page-{page_num}.png"));
        if !page_path.exists() {
            break;
        }

        if let Ok(text) = crate::extractor::ocr::ocr_image(&page_path, lang) {
            if !text.trim().is_empty() {
                if !full_text.is_empty() {
                    full_text.push('\n');
                }
                full_text.push_str(text.trim());
            }
        }
        page_num += 1;
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
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
        let doc = lopdf::Document::load(path).context("failed to load PDF")?;
        let pages: Vec<u32> = doc.get_pages().into_keys().collect();
        if pages.is_empty() {
            return Ok(String::new());
        }

        // Extract text per page — continue on per-page error
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
        let is_wm = is_watermark_text(&page_texts);
        let is_garbled = is_garbled_text(&merged);

        // Clean text: >50 chars, not garbled, not watermark
        if merged.len() > 50 && !is_garbled && !is_wm {
            return Ok(merged);
        }

        // OCR fallback
        if is_pdftoppm_available() {
            match ocr_pdf_via_pdftoppm(path, "eng") {
                Ok(ocr_text) if !ocr_text.is_empty() => {
                    log::info!(
                        "[PDF] OCR fallback for {:?} (extracted {} chars, OCR'd {} chars)",
                        path.file_name().unwrap_or_default(),
                        merged.len(),
                        ocr_text.len()
                    );
                    return Ok(ocr_text);
                }
                _ => {}
            }
        }

        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Document, Object, Stream};

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
        assert!(result.contains("Page One"), "result: {:?}", result);
        assert!(result.contains("Page Two"), "result: {:?}", result);

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
}
