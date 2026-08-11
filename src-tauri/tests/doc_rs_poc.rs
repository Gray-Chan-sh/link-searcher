use std::path::Path;

use link_searcher_lib::extractor::office::OfficeExtractor;
use link_searcher_lib::extractor::Extractor;

/// POC evidence: rwml raw extraction vs the composite extract() path.
/// Skip when fixtures absent.
#[test]
fn doc_rs_poc() -> anyhow::Result<()> {
    let dir = Path::new("/tmp/ls-rwml-poc/fixtures");
    if !dir.exists() {
        println!("[doc_rs_poc] fixtures dir {:?} missing, skipping", dir);
        return Ok(());
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "doc"))
        .collect();
    files.sort_by_key(|e| e.file_name());
    for entry in files {
        let path = entry.path();
        let bytes = std::fs::read(&path)?;
        let rwml_res = rwml::extract_text(&bytes);
        let ext_res = OfficeExtractor::new().extract(&path).map(|s| s.len());
        match &rwml_res {
            Ok(t) => println!(
                "[doc_rs_poc] {:?} rwml=OK chars={} extract()={:?} preview={:?}",
                path.file_name().unwrap(),
                t.len(),
                ext_res,
                t.chars().take(30).collect::<String>(),
            ),
            Err(e) => println!(
                "[doc_rs_poc] {:?} rwml=ERR {:?} extract()={:?}",
                path.file_name().unwrap(),
                e,
                ext_res,
            ),
        }
        if let Err(e) = &ext_res {
            println!("[doc_rs_poc] NOTE: extract() failed (no LO fallback): {e}");
        }
    }
    Ok(())
}
