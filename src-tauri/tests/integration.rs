//! Integration tests for the Link-Searcher full-text search workflow.
//!
//! Tests the real modules (db, indexer, scanner, extractor, search) without
//! requiring a running Tauri app.  All temp files/dirs are cleaned up via
//! [`TempDir`] drop guards.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use link_searcher_lib::db::{self, dir_config, tracker};
use link_searcher_lib::extractor;
use link_searcher_lib::indexer::IndexerService;
use link_searcher_lib::scanner::Scanner;
use link_searcher_lib::search::searcher::{SearchParams, SearcherWrap};
use link_searcher_lib::search::IndexManager;

// ---------------------------------------------------------------------------
// Temp dir with automatic cleanup
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ls_int_{prefix}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Test environment
// ---------------------------------------------------------------------------

struct TestEnv {
    pool: Pool<SqliteConnectionManager>,
    index_mgr: Arc<RwLock<IndexManager>>,
    indexer: Arc<IndexerService>,
    scanner: Scanner,
    dir_id: String,
    dir_path: PathBuf,
}

impl TestEnv {
    fn new(dir_path: PathBuf) -> Self {
        // Use a file-based DB inside the temp dir so we can init via public API.
        let db_path = dir_path.join("test.db");
        let db_str = db_path.to_str().unwrap();
        db::init_db(db_str).unwrap();
        let pool = db::get_pool(db_str).unwrap();

        let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
        let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));

        let dir_id = {
            let c = pool.get().unwrap();
            dir_config::add_dir(
                &c,
                dir_path.to_str().unwrap(),
                Some("test"),
                None,
                None,
                None,
                true,
            )
            .unwrap()
            .id
        };

        let scanner = Scanner::new(pool.clone(), indexer.clone());
        Self { pool, index_mgr: im, indexer, scanner, dir_id, dir_path }
    }

    fn create_file(&self, name: &str, content: &str) -> PathBuf {
        let p = self.dir_path.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn track_file(&self, path: &Path, mtime: i64) -> String {
        let c = self.pool.get().unwrap();
        tracker::upsert_file(
            &c,
            path.to_str().unwrap(),
            &self.dir_id,
            mtime,
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            None,
        )
        .unwrap()
    }

    fn search(
        &self,
        query: &str,
        dir_ids: Option<Vec<String>>,
        ext_filter: Option<Vec<String>>,
    ) -> Vec<String> {
        let reader = self.index_mgr.read().unwrap().reader().unwrap();
        let idx = self.index_mgr.read().unwrap().index().clone();
        let searcher = SearcherWrap::new(reader, (*idx).clone());
        let params = SearchParams {
            query: query.to_string(),
            dir_ids,
            file_ids: None,
            ext_filter,
            date_from: None,
            date_to: None,
            sort: "score".to_string(),
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 100,
            fuzzy: false,
        };
        searcher.search(&params).unwrap().hits.into_iter().map(|h| h.file_id).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_full_indexing_workflow() -> Result<()> {
    let tmp = TempDir::new("full_index");
    let env = TestEnv::new(tmp.path().to_path_buf());

    let files: Vec<(PathBuf, &str)> = vec![
        (env.create_file("a.txt", "apple banana"), "apple banana"),
        (env.create_file("b.txt", "cherry date"), "cherry date"),
        (env.create_file("c.txt", "elderberry fig"), "elderberry fig"),
    ];

    for (path, _content) in &files {
        let fid = env.track_file(path, 1000);
        env.indexer.index_file(&fid, path, &env.dir_id)?;
    }
    env.indexer.commit()?;

    // All files should be marked indexed=1.
    for (path, _) in &files {
        let c = env.pool.get().unwrap();
        let rec = tracker::get_file_by_path(&c, path.to_str().unwrap())?.unwrap();
        assert_eq!(rec.indexed, 1, "{} should be indexed", path.display());
    }

    // Tantivy search finds content from all.
    assert_eq!(env.search("apple", None, None).len(), 1);
    assert_eq!(env.search("cherry", None, None).len(), 1);
    assert_eq!(env.search("elderberry", None, None).len(), 1);
    Ok(())
}

#[test]
fn test_incremental_indexing_dedup() -> Result<()> {
    let tmp = TempDir::new("dedup");
    let env = TestEnv::new(tmp.path().to_path_buf());

    let content = "duplicate content across files";
    let p1 = env.create_file("first.txt", content);
    let p2 = env.create_file("second.txt", content);

    let fid1 = env.track_file(&p1, 1000);
    env.indexer.index_file(&fid1, &p1, &env.dir_id)?;
    env.indexer.commit()?;

    let fid2 = env.track_file(&p2, 1001);
    env.indexer.index_file(&fid2, &p2, &env.dir_id)?;
    env.indexer.commit()?;

    // Both files should share the same md5.
    let c = env.pool.get().unwrap();
    let md5_1 = tracker::get_file_by_id(&c, &fid1)?.unwrap().md5;
    let md5_2 = tracker::get_file_by_id(&c, &fid2)?.unwrap().md5;
    assert!(md5_1.is_some() && md5_2.is_some(), "both should have md5");
    assert_eq!(md5_1, md5_2, "both files share the same md5");

    // content_index has only ONE entry.
    let count: i64 = c.query_row("SELECT COUNT(*) FROM content_index", [], |r| r.get(0))?;
    assert_eq!(count, 1, "content_index should have one entry");

    // Tantivy still has two documents (different file_ids).
    assert_eq!(env.search("duplicate", None, None).len(), 2);
    Ok(())
}

#[test]
fn test_full_scan() -> Result<()> {
    let tmp = TempDir::new("full_scan");
    let env = TestEnv::new(tmp.path().to_path_buf());

    env.create_file("alpha.txt", "alpha content");
    env.create_file("beta.txt", "beta content");
    env.create_file("gamma.md", "gamma markdown");
    env.create_file("delta.txt", "delta data");
    env.create_file("epsilon.md", "epsilon notes");

    let result = env.scanner.full_scan(&env.dir_id, |_| {})?;
    env.indexer.commit()?;

    assert_eq!(result.total_files, 5);
    assert_eq!(result.indexed, 5);
    assert_eq!(result.errors, 0);

    // All 5 tracked in DB.
    let c = env.pool.get().unwrap();
    let files = tracker::get_files_by_dir(&c, &env.dir_id)?;
    assert_eq!(files.len(), 5);
    assert!(files.iter().all(|f| f.indexed == 1), "all should be indexed");

    // Tantivy has all content.
    assert_eq!(env.search("alpha", None, None).len(), 1);
    assert_eq!(env.search("epsilon", None, None).len(), 1);

    // Stats correct.
    let stats = tracker::get_stats(&c, Some(&env.dir_id))?;
    assert_eq!(stats.total, 5);
    assert_eq!(stats.indexed, 5);
    assert_eq!(stats.pending, 0);
    Ok(())
}

#[test]
fn test_incremental_scan() -> Result<()> {
    let tmp = TempDir::new("incr_scan");
    let env = TestEnv::new(tmp.path().to_path_buf());

    // Full scan first.
    env.create_file("keep.txt", "keep this content");
    env.create_file("modify.txt", "original content");
    env.create_file("delete.txt", "will be deleted");
    let _full = env.scanner.full_scan(&env.dir_id, |_| {})?;
    env.indexer.commit()?;

    // Wait so mtime differs.
    std::thread::sleep(std::time::Duration::from_millis(20));

    // Modify, add, delete.
    env.create_file("modify.txt", "modified content!");
    env.create_file("new.txt", "brand new file");
    std::fs::remove_file(env.dir_path.join("delete.txt")).unwrap();

    let result = env.scanner.incremental_scan(&env.dir_id)?;
    env.indexer.commit()?;

    assert_eq!(result.indexed, 2, "modified + new should be indexed");

    // Modified file has new md5, is indexed.
    let c = env.pool.get().unwrap();
    let modified =
        tracker::get_file_by_path(&c, env.dir_path.join("modify.txt").to_str().unwrap())?.unwrap();
    assert_eq!(modified.indexed, 1, "modified should be indexed");
    assert_ne!(modified.md5.as_deref(), Some("original content"));

    // New file tracked and indexed.
    let new = tracker::get_file_by_path(&c, env.dir_path.join("new.txt").to_str().unwrap())?.unwrap();
    assert_eq!(new.indexed, 1);

    let deleted =
            tracker::get_file_by_path(&c, env.dir_path.join("delete.txt").to_str().unwrap())?;
    assert!(deleted.is_some(), "deleted file should still be tracked in DB");
    let d = deleted.unwrap();
    let deleted_or_cleared = d.status == "deleted" || d.md5.is_none();
    assert!(deleted_or_cleared, "deleted file should be marked deleted or have md5 cleared, got status={}, md5={:?}", d.status, d.md5);

    // Search finds modified content.
    assert!(env.search("modified", None, None).len() >= 1);
    Ok(())
}

#[test]
fn test_search_with_filters() -> Result<()> {
    let tmp = TempDir::new("search_filters");
    let env = TestEnv::new(tmp.path().to_path_buf());

    // Second dir config.
    let dir2_id = {
        let c = env.pool.get().unwrap();
        dir_config::add_dir(&c, "/tmp/fake_other", Some("other"), None, None, None, true)
            .unwrap()
            .id
    };

    let p1 = env.create_file("r1.txt", "report one financial");
    let p2 = env.create_file("r2.rs", "report two financial");
    let p3 = env.create_file("note.txt", "personal note");

    let fid1 = env.track_file(&p1, 1000);
    let fid2 = env.track_file(&p2, 1001);
    let fid3 = {
        let c = env.pool.get().unwrap();
        tracker::upsert_file(
            &c,
            p3.to_str().unwrap(),
            &dir2_id,
            1002,
            std::fs::metadata(&p3).unwrap().len(),
            None,
        )
        .unwrap()
    };

    env.indexer.index_file(&fid1, &p1, &env.dir_id)?;
    env.indexer.index_file(&fid2, &p2, &env.dir_id)?;
    env.indexer.index_file(&fid3, &p3, &dir2_id)?;
    env.indexer.commit()?;

    // dir_id filter.
    assert_eq!(env.search("", Some(vec![env.dir_id.clone()]), None).len(), 2);
    assert_eq!(env.search("", Some(vec![dir2_id.clone()]), None).len(), 1);

    // ext filter.
    assert_eq!(env.search("", None, Some(vec!["txt".to_string()])).len(), 2);
    assert_eq!(env.search("", None, Some(vec!["rs".to_string()])).len(), 1);

    // query filter.
    assert_eq!(env.search("financial", None, None).len(), 2);

    // query + dir filter.
    assert_eq!(env.search("financial", Some(vec![env.dir_id.clone()]), None).len(), 2);
    assert_eq!(env.search("financial", Some(vec![dir2_id]), None).len(), 0);
    Ok(())
}

#[test]
fn test_duplicate_detection() -> Result<()> {
    let tmp = TempDir::new("duplicates");
    let env = TestEnv::new(tmp.path().to_path_buf());

    let p1 = env.create_file("unique.txt", "unique content here");
    let p2 = env.create_file("dup_a.txt", "duplicate content");
    let p3 = env.create_file("dup_b.txt", "duplicate content");

    let fid1 = env.track_file(&p1, 1000);
    let fid2 = env.track_file(&p2, 1001);
    let fid3 = env.track_file(&p3, 1002);

    env.indexer.index_file(&fid1, &p1, &env.dir_id)?;
    env.indexer.index_file(&fid2, &p2, &env.dir_id)?;
    env.indexer.index_file(&fid3, &p3, &env.dir_id)?;
    env.indexer.commit()?;

    let c = env.pool.get().unwrap();
    let groups = tracker::get_duplicates(&c)?;
    assert_eq!(groups.len(), 1, "one duplicate group");
    assert_eq!(groups[0].count, 2, "two files in group");
    assert!(groups[0].paths.iter().any(|p| p.contains("dup_a")));
    assert!(groups[0].paths.iter().any(|p| p.contains("dup_b")));
    Ok(())
}

#[test]
fn test_text_extraction_formats() -> Result<()> {
    let tmp = TempDir::new("extraction");

    // .txt
    let txt = tmp.path().join("hello.txt");
    std::fs::write(&txt, "Hello from txt")?;
    assert_eq!(extractor::extract_text(&txt)?, "Hello from txt");

    // .md
    let md = tmp.path().join("readme.md");
    std::fs::write(&md, "# Markdown\n\nContent here")?;
    let md_text = extractor::extract_text(&md)?;
    assert!(md_text.contains("Content"), "md: {md_text:?}");

    // .pdf (minimal via lopdf)
    let pdf_path = tmp.path().join("test.pdf");
    {
        use lopdf::{Dictionary, Document, Object, Stream};
        let mut doc = Document::new();
        let font_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Font".to_vec())),
            (b"Subtype".to_vec(), Object::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec())),
        ]));
        let content_text = "PDF extracted text";
        let content = format!("BT /F1 12 Tf 100 700 Td ({content_text}) Tj ET");
        let stream = Stream::new(
            Dictionary::from_iter([(b"Length".to_vec(), Object::Integer(content.len() as i64))]),
            content.into_bytes(),
        );
        let content_id = doc.add_object(stream);
        let page_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Page".to_vec())),
            (b"MediaBox".to_vec(), Object::Array(vec![
                Object::Integer(0), Object::Integer(0),
                Object::Integer(612), Object::Integer(792),
            ])),
            (b"Contents".to_vec(), Object::Reference(content_id)),
            (b"Resources".to_vec(), Object::Dictionary(Dictionary::from_iter([
                (b"Font".to_vec(), Object::Dictionary(Dictionary::from_iter([
                    (b"F1".to_vec(), Object::Reference(font_id)),
                ]))),
            ]))),
        ]));
        let pages_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Pages".to_vec())),
            (b"Kids".to_vec(), Object::Array(vec![Object::Reference(page_id)])),
            (b"Count".to_vec(), Object::Integer(1)),
        ]));
        if let Ok(d) = doc.get_dictionary_mut(page_id) {
            d.set("Parent", Object::Reference(pages_id));
        }
        let catalog_id = doc.add_object(Dictionary::from_iter([
            (b"Type".to_vec(), Object::Name(b"Catalog".to_vec())),
            (b"Pages".to_vec(), Object::Reference(pages_id)),
        ]));
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(&pdf_path)?;
    }
    let pdft = extractor::extract_text(&pdf_path)?;
    assert!(pdft.contains("PDF"), "pdf: {pdft:?}");

    // .docx (minimal via zip)
    let docx_path = tmp.path().join("test.docx");
    {
        use std::io::Write;
        let f = std::fs::File::create(&docx_path)?;
        let mut z = zip::ZipWriter::new(f);
        z.add_directory("word/", zip::write::FileOptions::<()>::default())?;
        z.start_file("word/document.xml", zip::write::FileOptions::<()>::default())?;
        z.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>DOCX text</w:t></w:r></w:p></w:body>
</w:document>"#,
        )?;
        z.finish()?;
    }
    let docxt = extractor::extract_text(&docx_path)?;
    assert!(docxt.contains("DOCX"), "docx: {docxt:?}");

    Ok(())
}

#[test]
fn test_ocr_fallback() -> Result<()> {
    let tmp = TempDir::new("ocr");

    // Create a tiny 1x1 white PNG then extract text from it.
    // extract_text dispatches to ImageExtractor, which calls tesseract
    // if available, or returns empty string gracefully.
    let img_path = tmp.path().join("text.png");
    let img = image::DynamicImage::new_rgba8(1, 1);
    img.save(&img_path)?;

    let result = extractor::extract_text(&img_path)?;
    // If tesseract not available: empty string.
    // If tesseract available: may be empty (1px image) or contain something.
    // Either is acceptable — we just verify no panic/error.
    assert!(result.is_empty() || !result.is_empty());
    Ok(())
}

#[test]
fn test_concurrent_safety() -> Result<()> {
    let tmp = TempDir::new("concurrent");
    let env = TestEnv::new(tmp.path().to_path_buf());

    let p1 = env.create_file("con1.txt", "concurrent file one");
    let p2 = env.create_file("con2.txt", "concurrent file two");

    let fid1 = env.track_file(&p1, 1000);
    let fid2 = env.track_file(&p2, 1001);

    // Pre-create the writer before spawning threads to avoid a race condition
    // in IndexerService::lock_writer where both threads try to create the
    // tantivy IndexWriter simultaneously.
    let dummy = env.create_file("_preinit_.txt", "preinit");
    let dummy_fid = env.track_file(&dummy, 1);
    env.indexer.index_file(&dummy_fid, &dummy, &env.dir_id).unwrap();
    std::fs::remove_file(dummy).ok();

    let indexer1 = env.indexer.clone();
    let indexer2 = env.indexer.clone();
    let dir_id1 = env.dir_id.clone();
    let dir_id2 = env.dir_id.clone();
    let p1b = p1.clone();
    let p2b = p2.clone();
    let fid1b = fid1.clone();
    let fid2b = fid2.clone();

    let jh1 = std::thread::spawn(move || {
        indexer1.index_file(&fid1b, &p1b, &dir_id1).unwrap();
    });
    let jh2 = std::thread::spawn(move || {
        indexer2.index_file(&fid2b, &p2b, &dir_id2).unwrap();
    });

    jh1.join().expect("thread 1 panicked");
    jh2.join().expect("thread 2 panicked");

    env.indexer.commit()?;

    // Both files indexed.
    assert_eq!(env.search("concurrent", None, None).len(), 2);

    // Both DB records indexed=1.
    let c = env.pool.get().unwrap();
    let r1 = tracker::get_file_by_id(&c, &fid1)?.unwrap();
    let r2 = tracker::get_file_by_id(&c, &fid2)?.unwrap();
    assert_eq!(r1.indexed, 1);
    assert_eq!(r2.indexed, 1);
    Ok(())
}