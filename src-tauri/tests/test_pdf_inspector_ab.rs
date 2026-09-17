//! A/B comparison: lopdf extract_text vs pdf-inspector process_pdf_mem.
//!
//! Run with: cargo test --test test_pdf_inspector_ab -- --nocapture --ignored
//!
//! Tests two pipelines on the same PDFs:
//!   A (current): lopdf::Document::load → extract_text per page → join
//!   B (candidate): pdf_inspector::process_pdf_mem → markdown
//!
//! Metrics: char count, expected-text hit, garbled rate, elapsed time.

use std::path::{Path, PathBuf};
use std::time::Instant;

use lopdf::{Dictionary, Document, Object, Stream};

/// Create a multi-page text PDF with known content.
fn make_text_pdf(path: &Path, pages: &[&str]) {
    let mut doc = Document::new();
    let font_id = doc.add_object(Dictionary::from_iter([
        (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
        (b"Subtype".to_vec(), Object::Name(b"Type1".to_vec())),
        (b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec())),
    ]));

    let mut page_ids: Vec<Object> = Vec::with_capacity(pages.len());
    let mut page_obj_ids: Vec<lopdf::ObjectId> = Vec::with_capacity(pages.len());
    for body in pages {
        let content = format!("BT /F1 12 Tf 72 700 Td ({}) Tj ET", body);
        let stream = Stream::new(
            Dictionary::from_iter([(
                b"Length".to_vec(),
                Object::Integer(content.len() as i64),
            )]),
            content.into_bytes(),
        );
        let content_id = doc.add_object(stream);
        let page_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            (
                b"MediaBox".to_vec(),
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(612),
                    Object::Integer(792),
                ]),
            ),
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
        page_ids.push(Object::Reference(page_id));
        page_obj_ids.push(page_id);
    }

    let pages_id = doc.add_object(Dictionary::from_iter([
        (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
        (b"Kids".to_vec(), Object::Array(page_ids)),
        (b"Count".to_vec(), Object::Integer(pages.len() as i64)),
    ]));

    // Set Parent on each page
    for pid in &page_obj_ids {
        if let Ok(page_dict) = doc.get_dictionary_mut(*pid) {
            page_dict.set("Parent", Object::Reference(pages_id));
        }
    }

    let catalog_id = doc.add_object(Dictionary::from_iter([
        (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
        (b"Pages".to_vec(), Object::Reference(pages_id)),
    ]));
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc.save(path).unwrap();
}

/// Pipeline A: lopdf extract_text (current approach, minus OCR fallback).
fn pipeline_lopdf(path: &Path) -> (String, u128) {
    let start = Instant::now();
    let doc = match Document::load(path) {
        Ok(d) => d,
        Err(e) => return (format!("ERROR: {e}"), start.elapsed().as_millis()),
    };
    let page_nums: Vec<u32> = doc.get_pages().into_keys().collect();
    let mut texts: Vec<String> = Vec::with_capacity(page_nums.len());
    for pn in &page_nums {
        match doc.extract_text(&[*pn]) {
            Ok(t) => texts.push(t.trim_end_matches('\n').to_owned()),
            Err(_) => texts.push(String::new()),
        }
    }
    (texts.join("\n"), start.elapsed().as_millis())
}

/// Pipeline B: pdf-inspector process_pdf_mem (full detect + extract + markdown).
fn pipeline_pdf_inspector(path: &Path) -> (String, String, u128) {
    let start = Instant::now();
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return (format!("ERROR: {e}"), String::new(), start.elapsed().as_millis()),
    };
    match pdf_inspector::process_pdf_mem(&bytes) {
        Ok(result) => {
            let md = result.markdown.unwrap_or_default();
            let pdf_type = format!("{:?}", result.pdf_type);
            (md, pdf_type, start.elapsed().as_millis())
        }
        Err(e) => (format!("ERROR: {e}"), String::new(), start.elapsed().as_millis()),
    }
}

/// Simple garbled check: ratio of replacement chars + control chars.
fn garbled_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 1.0;
    }
    let total = text.chars().count() as f64;
    let bad = text
        .chars()
        .filter(|c| {
            *c == '\u{FFFD}' || (c.is_control() && *c != '\n' && *c != '\r' && *c != '\t')
        })
        .count() as f64;
    bad / total
}

/// Count non-whitespace characters.
fn content_chars(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// Check if needle appears in haystack (case-insensitive, whitespace-stripped).
fn contains_ci(haystack: &str, needle: &str) -> bool {
    let norm = |s: &str| -> String {
        s.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect()
    };
    norm(haystack).contains(&norm(needle))
}

struct Comparison {
    label: String,
    a_chars: usize,
    b_chars: usize,
    a_garbled: f64,
    b_garbled: f64,
    a_ms: u128,
    b_ms: u128,
    b_type: String,
    hits: Vec<(String, bool, bool)>, // (needle, A_hit, B_hit)
}

impl Comparison {
    fn print(&self) {
        println!("\n============================================================");
        println!("PDF: {}", self.label);
        println!("------------------------------------------------------------");
        println!("  pdf-inspector type: {}", self.b_type);
        println!("  content chars:  A={:<6}  B={:<6}  delta={:+}",
            self.a_chars, self.b_chars, self.b_chars as i64 - self.a_chars as i64);
        println!("  garbled ratio:  A={:.1}%  B={:.1}%",
            self.a_garbled * 100.0, self.b_garbled * 100.0);
        println!("  elapsed:         A={:<4}ms  B={:<4}ms",
            self.a_ms, self.b_ms);
        if !self.hits.is_empty() {
            println!("  keyword hits:");
            for (needle, a, b) in &self.hits {
                println!("    {:<30}  A={}  B={}", needle, if *a { "YES" } else { "no" }, if *b { "YES" } else { "no" });
            }
        }
        // Verdict
        let b_better_quality = self.b_garbled < self.a_garbled
            || (self.b_chars > self.a_chars && self.b_garbled <= self.a_garbled + 0.05);
        let b_faster = self.b_ms < self.a_ms;
        println!("  verdict: {} {}",
            if b_better_quality { "B-better-quality" } else { "quality-comparable" },
            if b_faster { "+B-faster" } else { "" });
    }
}

/// Run A/B comparison on a single PDF.
fn compare_pdf(path: &Path, label: &str, keywords: &[&str]) -> Comparison {
    let (a_text, a_ms) = pipeline_lopdf(path);
    let (b_text, b_type, b_ms) = pipeline_pdf_inspector(path);

    let hits: Vec<(String, bool, bool)> = keywords
        .iter()
        .map(|kw| (kw.to_string(), contains_ci(&a_text, kw), contains_ci(&b_text, kw)))
        .collect();

    Comparison {
        label: label.to_string(),
        a_chars: content_chars(&a_text),
        b_chars: content_chars(&b_text),
        a_garbled: garbled_ratio(&a_text),
        b_garbled: garbled_ratio(&b_text),
        a_ms,
        b_ms,
        b_type,
        hits,
    }
}

/// Find real PDFs in a directory (max N), returning (path, label) pairs.
fn find_real_pdfs(dir: &Path, max: usize) -> Vec<(PathBuf, String)> {
    let mut results = Vec::new();
    if !dir.exists() {
        return results;
    }
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|e| e.to_str()) == Some("pdf")
        {
            let label = entry
                .file_name()
                .to_str()
                .unwrap_or("?")
                .to_string();
            results.push((entry.path().to_path_buf(), label));
            if results.len() >= max {
                break;
            }
        }
    }
    results
}

#[test]
fn test_ab_synthetic_text_pdf() {
    let dir = std::env::temp_dir().join("ls_pdf_ab_test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("synthetic_text.pdf");

    let pages = [
        "This is page one with important keywords like invoice and contract.",
        "Page two contains financial data with numbers 12345 and references.",
        "Third page has legal terms confidentiality agreement clause seven.",
    ];
    make_text_pdf(&path, &pages);

    let cmp = compare_pdf(&path, "synthetic_text.pdf", &["invoice", "contract", "financial", "12345", "confidentiality"]);
    cmp.print();

    // Both pipelines should extract the text from a clean digital PDF
    let a = pipeline_lopdf(&path);
    assert!(a.0.contains("Page one") || a.0.contains("page one"), "lopdf should extract page 1");
    assert!(content_chars(&a.0) > 50, "lopdf should extract meaningful text");

    let b = pipeline_pdf_inspector(&path);
    assert!(!b.1.is_empty(), "pdf-inspector should classify the PDF");
    // pdf-inspector should produce markdown with the text content
    assert!(b.0.contains("invoice") || b.0.contains("Page one") || b.0.contains("page one"),
        "pdf-inspector should extract text, got: {}", &b.0[..b.0.len().min(200)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_ab_synthetic_multipage() {
    let dir = std::env::temp_dir().join("ls_pdf_ab_multi");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("multi_page.pdf");

    let pages: Vec<String> = (0..10)
        .map(|i| format!("Page number {i} with content paragraph describing topic {i} in detail."))
        .collect::<Vec<_>>();
    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    make_text_pdf(&path, &page_refs);

    let cmp = compare_pdf(&path, "multi_page.pdf (10 pages)", &["Page number 0", "Page number 5", "Page number 9"]);
    cmp.print();

    // Both should handle multi-page
    assert!(cmp.a_chars > 100, "lopdf should extract multi-page text");
    assert!(cmp.b_chars > 100, "pdf-inspector should extract multi-page text");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "scans real PDFs in ~/Documents — run with --ignored"]
fn test_ab_real_pdfs() {
    // Scan common directories for real PDFs
    let scan_dirs = [
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Documents"),
    ];

    let mut all_results = Vec::new();

    for dir in &scan_dirs {
        let pdfs = find_real_pdfs(dir, 20);
        println!("Found {} PDFs in {}", pdfs.len(), dir.display());

        for (path, label) in pdfs {
            // Skip very large files (>50MB) to keep the test reasonable
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() > 50 * 1024 * 1024 {
                    println!("  SKIP {} ({}MB)", label, meta.len() / 1024 / 1024);
                    continue;
                }
            }
            let cmp = compare_pdf(&path, &label, &[]);
            cmp.print();
            all_results.push(cmp);
        }
    }

    // Summary
    if all_results.is_empty() {
        println!("\nNo real PDFs found — synthetic tests cover the basics.");
        return;
    }

    println!("\n============================================================");
    println!("SUMMARY ({} PDFs)", all_results.len());
    println!("------------------------------------------------------------");

    let b_better = all_results
        .iter()
        .filter(|c| c.b_garbled < c.a_garbled || (c.b_chars > c.a_chars && c.b_garbled <= c.a_garbled + 0.05))
        .count();
    let b_faster = all_results.iter().filter(|c| c.b_ms < c.a_ms).count();
    let total_a_chars: usize = all_results.iter().map(|c| c.a_chars).sum();
    let total_b_chars: usize = all_results.iter().map(|c| c.b_chars).sum();
    let avg_a_garbled: f64 = all_results.iter().map(|c| c.a_garbled).sum::<f64>() / all_results.len() as f64;
    let avg_b_garbled: f64 = all_results.iter().map(|c| c.b_garbled).sum::<f64>() / all_results.len() as f64;
    let avg_a_ms: f64 = all_results.iter().map(|c| c.a_ms as f64).sum::<f64>() / all_results.len() as f64;
    let avg_b_ms: f64 = all_results.iter().map(|c| c.b_ms as f64).sum::<f64>() / all_results.len() as f64;

    println!("  B better quality: {}/{}", b_better, all_results.len());
    println!("  B faster:         {}/{}", b_faster, all_results.len());
    println!("  Total content chars:  A={}  B={}  delta={:+}",
        total_a_chars, total_b_chars, total_b_chars as i64 - total_a_chars as i64);
    println!("  Avg garbled ratio:    A={:.1}%  B={:.1}%", avg_a_garbled * 100.0, avg_b_garbled * 100.0);
    println!("  Avg elapsed:           A={:.0}ms  B={:.0}ms", avg_a_ms, avg_b_ms);
}
