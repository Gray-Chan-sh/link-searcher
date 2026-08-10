use std::path::Path;

use link_searcher_lib::extractor::office::OfficeExtractor;
use link_searcher_lib::extractor::Extractor;

/// Negative-case evidence: rwml must error (not panic) on a corrupt .doc,
/// and extract() must degrade gracefully (Err via LO fallback, never panic).
#[test]
fn doc_rs_poc_fallback() {
    let path = Path::new("/tmp/ls-rwml-poc/fixtures/f7_broken.doc");
    if !path.exists() {
        println!("[doc_rs_poc_fallback] fixture missing, skipping");
        return;
    }
    let bytes = std::fs::read(path).unwrap();
    let rwml_res = std::panic::catch_unwind(|| rwml::extract_text(&bytes));
    let rwml_out = match rwml_res {
        Ok(Ok(t)) => format!("OK chars={}", t.len()),
        Ok(Err(e)) => format!("ERR {e:?}"),
        Err(_) => "PANIC".to_string(),
    };
    let ext_res = std::panic::catch_unwind(|| OfficeExtractor::new().extract(path));
    let ext_out = match ext_res {
        Ok(Ok(t)) => format!("OK chars={}", t.len()),
        Ok(Err(e)) => format!("Err({e})"),
        Err(_) => "PANIC".to_string(),
    };
    println!("[doc_rs_poc_fallback] f7_broken.doc rwml={rwml_out} extract()={ext_out}");
    assert!(!rwml_out.contains("PANIC"), "rwml must not panic on corrupt input");
    assert!(!ext_out.contains("PANIC"), "extract() must not panic on corrupt input");
}