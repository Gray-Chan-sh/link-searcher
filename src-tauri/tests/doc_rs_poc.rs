use std::path::Path;

use link_searcher_lib::extractor::office::{lo_binary, OfficeExtractor};
use link_searcher_lib::extractor::Extractor;

fn lo_baseline_chars(path: &Path) -> Option<usize> {
    let stem = path.file_stem()?.to_str()?;
    let txt = Path::new("/tmp/ls-rwml-poc/lo_out").join(format!("{stem}.txt"));
    std::fs::read_to_string(txt).ok().map(|s| s.len())
}

/// POC evidence: rwml raw extraction vs LO baseline vs the composite
/// extract() path (rwml -> LO fallback). Skip when fixtures absent.
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
        let lo_chars = lo_baseline_chars(&path);
        let ext_res = OfficeExtractor::new().extract(&path).map(|s| s.len());
        match &rwml_res {
            Ok(t) => println!(
                "[doc_rs_poc] {:?} rwml=OK chars={} lo={:?} extract()={:?} preview={:?}",
                path.file_name().unwrap(),
                t.len(),
                lo_chars,
                ext_res,
                t.chars().take(30).collect::<String>(),
            ),
            Err(e) => println!(
                "[doc_rs_poc] {:?} rwml=ERR {:?} lo={:?} extract()={:?}",
                path.file_name().unwrap(),
                e,
                lo_chars,
                ext_res,
            ),
        }
        if lo_binary().is_some() {
            assert!(
                ext_res.is_ok(),
                "extract() should fall back to LO, got: {:?}",
                ext_res
            );
        }
    }
    Ok(())
}
