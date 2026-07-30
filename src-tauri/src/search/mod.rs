pub mod indexer;
pub mod schema;
pub mod searcher;

use std::path::Path;
use std::sync::Arc;

use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy};

use crate::search::schema::{build_schema, register_tokenizers};

/// Thread-safe manager around a tantivy [`Index`].
///
/// Provides controlled access to the index reader and writer, ensuring
/// that custom tokenizers (jieba, suggest) are registered immediately
/// after index creation.
pub struct IndexManager {
    index: Arc<Index>,
}

impl IndexManager {
    /// Open an existing tantivy index at `path`, or create a new one if
    /// none exists.
    ///
    /// # Errors
    ///
    /// Returns `tantivy::TantivyError` if the directory cannot be read or
    /// the schema is incompatible.
    pub fn open_or_create(path: &Path) -> Result<Self, tantivy::TantivyError> {
        let schema = build_schema();
        let index = if path.join("meta.json").exists() {
            Index::open_in_dir(path)?
        } else {
            std::fs::create_dir_all(path).ok();
            Index::create_in_dir(path, schema)?
        };
        register_tokenizers(&index);
        Ok(Self {
            index: Arc::new(index),
        })
    }

    /// Create an in-memory index for testing.
    pub fn create_in_ram() -> Self {
        let schema = build_schema();
        let index = Index::create_in_ram(schema);
        register_tokenizers(&index);
        Self {
            index: Arc::new(index),
        }
    }

    /// Return a new [`IndexReader`] with the default reload policy
    /// (reload on commit with a 1-second delay).
    pub fn reader(&self) -> Result<IndexReader, tantivy::TantivyError> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(reader)
    }

    /// Return a new [`IndexWriter`] with the given memory budget (in bytes).
    ///
    /// A budget of 50–200 MB is typical for desktop usage.
    ///
    /// # Errors
    ///
    /// Returns an error if another writer is already open on this index.
    pub fn writer(&self, memory_budget: usize) -> Result<IndexWriter, tantivy::TantivyError> {
        self.index.writer(memory_budget)
    }

    /// Access the underlying [`Index`].
    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("test_index_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn test_open_or_create_creates_new_index() {
        let dir = temp_dir("new");
        let manager = IndexManager::open_or_create(&dir).expect("open_or_create should succeed");
        let reader = manager.reader().expect("reader should be created");
        let searcher = reader.searcher();
        // Fresh index has zero segments.
        assert_eq!(searcher.segment_readers().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_open_or_create_reopens_existing_index() {
        let dir = temp_dir("reopen");
        // Create with a document.
        let m1 = IndexManager::open_or_create(&dir).expect("first open");
        let mut w = m1.writer(50_000_000).expect("writer");
        let schema = crate::search::schema::build_schema();
        let file_id = schema.get_field("file_id").unwrap();
        let doc = tantivy::doc!(file_id => "test-doc");
        w.add_document(doc).expect("add doc");
        w.commit().expect("commit");
        drop(w);
        drop(m1);

        // Re-open.
        let m2 = IndexManager::open_or_create(&dir).expect("second open");
        let reader = m2.reader().expect("reader");
        // Verify the re-opened index is usable by searching.
        let searcher = reader.searcher();
        assert!(searcher.num_docs() > 0,
            "expected >0 docs, got {}", searcher.num_docs());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_in_ram_works() {
        let manager = IndexManager::create_in_ram();
        let reader = manager.reader().expect("reader");
        let searcher = reader.searcher();
        assert_eq!(searcher.segment_readers().len(), 0);
    }

    #[test]
    fn test_writer_accepts_memory_budget() {
        let manager = IndexManager::create_in_ram();
        let writer = manager.writer(100_000_000);
        assert!(writer.is_ok());
    }
}
