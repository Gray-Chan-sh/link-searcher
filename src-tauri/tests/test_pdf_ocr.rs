#[test]
fn test_ocr_20111201() {
    use std::path::Path;
    use link_searcher_lib::extractor::pdf::PdfExtractor;
    let pdf = Path::new("/Users/gray/Documents/CH 常宏案/05 工商内档/天缘富达/20111201.pdf");
    if !pdf.exists() {
        eprintln!("PDF not found, skipping");
        return;
    }
    let extractor = PdfExtractor::new();
    eprintln!("=== Testing PDF OCR with chi_sim ===");
    match extractor.extract_with_lang(pdf, "chi_sim", None) {
        Ok(text) => {
            eprintln!("SUCCESS: {} chars", text.len());
            let preview = if text.len() > 500 { &text[..500] } else { &text };
            eprintln!("PREVIEW:\n{}", preview);
        }
        Err(e) => eprintln!("FAILED: {e}"),
    }
}

#[test]
fn test_ocr_bench_single_page() {
    use std::path::Path;
    use std::process::Command;
    use link_searcher_lib::extractor::paddleocr::recognize_with_metrics_from_path;
    let pdf = Path::new("/Users/gray/Documents/CH 常宏案/05 工商内档/天缘富达/20111201.pdf");
    if !pdf.exists() {
        eprintln!("PDF not found, skipping");
        return;
    }
    let tmp = std::env::temp_dir().join("ls_ocr_bench");
    std::fs::create_dir_all(&tmp).unwrap();
    let page = tmp.join("bench-page-1.png");
    // Render only page 1 at 200 DPI
    let status = Command::new("pdftoppm")
        .args(["-png", "-r", "200", "-f", "1", "-l", "1"])
        .arg(pdf)
        .arg(tmp.join("bench-page"))
        .status()
        .expect("pdftoppm not available");
    assert!(status.success(), "pdftoppm failed");
    eprintln!("=== Per-stage timings (200 DPI, page 1) ===");
    match recognize_with_metrics_from_path(&page) {
        Ok(timings) => eprintln!("{}", timings),
        Err(e) => eprintln!("FAILED: {e}"),
    }
}
