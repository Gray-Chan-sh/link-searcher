pub mod indexer;
pub mod schema;
pub mod searcher;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyError};

use crate::search::schema::{build_schema, register_tokenizers};

/// Thread-safe manager around a tantivy [`Index`].
///
/// Provides controlled access to the index reader and writer, ensuring
/// that custom tokenizers (jieba, suggest) are registered immediately
/// after index creation.
///
/// The [`IndexReader`] is created once at construction and cached, then
/// reloaded to pick up recent index commits. Explicit reloads are
/// throttled to `RELOAD_INTERVAL`: the reader's
/// `OnCommitWithDelay` policy already auto-refreshes after commits, so
/// forcing a reload on every call would reopen all segment readers on
/// each search. Within the window we rely on the auto-refresh; at most
/// one explicit reload happens per window.
pub struct IndexManager {
    index: Arc<Index>,
    reader: IndexReader,
    /// When the previous explicit reload happened; guards the throttle.
    last_reload: Mutex<Instant>,
    /// Injectable time source (defaults to `Instant::now`); tests
    /// override it to advance time deterministically.
    now: Box<dyn Fn() -> Instant + Send + Sync>,
    /// Number of explicit reloads performed — lets tests assert the
    /// throttle window is honoured.
    reload_count: AtomicU64,
}

impl IndexManager {
    /// Minimum time between two explicit reloads.
    const RELOAD_INTERVAL: Duration = Duration::from_secs(1);

    /// Shared construction: registers tokenizers and builds the reader.
    fn from_index(
        index: Index,
        now: Box<dyn Fn() -> Instant + Send + Sync>,
    ) -> Result<Self, TantivyError> {
        register_tokenizers(&index);
        let index = Arc::new(index);
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        // Initialise to two windows in the past so the first `reader()`
        // call always reloads (matching the previous unconditional
        // behaviour); subsequent calls within a window are throttled.
        let last_reload = Instant::now()
            .checked_sub(Self::RELOAD_INTERVAL * 2)
            .unwrap_or_else(Instant::now);
        Ok(Self {
            index,
            reader,
            last_reload: Mutex::new(last_reload),
            now,
            reload_count: AtomicU64::new(0),
        })
    }

    /// Open an existing tantivy index at `path`, or create a new one if
    /// none exists.
    ///
    /// # Errors
    ///
    /// Returns `tantivy::TantivyError` if the directory cannot be read or
    /// the schema is incompatible.
    pub fn open_or_create(path: &Path) -> Result<Self, TantivyError> {
        let schema = build_schema();
        let index = if path.join("meta.json").exists() {
            Index::open_in_dir(path)?
        } else {
            std::fs::create_dir_all(path).ok();
            Index::create_in_dir(path, schema)?
        };
        Self::from_index(index, Box::new(Instant::now))
    }

    /// Create an in-memory index for testing.
    pub fn create_in_ram() -> Self {
        let schema = build_schema();
        Self::from_index(Index::create_in_ram(schema), Box::new(Instant::now))
            .expect("in-memory reader creation must succeed")
    }

    /// Create an in-memory index with an injectable time source.
    #[cfg(test)]
    fn create_in_ram_with_clock(now: Box<dyn Fn() -> Instant + Send + Sync>) -> Self {
        let schema = build_schema();
        Self::from_index(Index::create_in_ram(schema), now)
            .expect("in-memory reader creation must succeed")
    }

    /// Return a reference to the cached [`IndexReader`], reloading it to
    /// pick up recent index commits.
    ///
    /// Reloads are throttled to [`Self::RELOAD_INTERVAL`]: within the
    /// window, the reader's `OnCommitWithDelay` auto-refresh supplies
    /// freshness, so forcing a reload on every call would only reopen all
    /// segment readers needlessly.
    pub fn reader(&self) -> Result<&IndexReader, TantivyError> {
        let now = (self.now)();
        let mut last = self.last_reload.lock().unwrap_or_else(|p| p.into_inner()); // nosemgrep: rust-mutex-lock-unwrap
        if now.saturating_duration_since(*last) >= Self::RELOAD_INTERVAL {
            self.reader.reload()?;
            *last = now;
            self.reload_count.fetch_add(1, Ordering::Relaxed);
        }
        Ok(&self.reader)
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

    /// Build a manager with a test-controlled clock; returns the clock
    /// handle so tests can advance time.
    fn manager_with_clock() -> (IndexManager, Arc<Mutex<Instant>>) {
        let clock = Arc::new(Mutex::new(Instant::now()));
        let manager = IndexManager::create_in_ram_with_clock(Box::new({
            let clock = Arc::clone(&clock);
            move || *clock.lock().unwrap() // nosemgrep: rust-mutex-lock-unwrap
        }));
        (manager, clock)
    }

    #[test]
    fn reader_throttles_reloads_within_window() {
        // Given: a manager with a fixed test clock.
        let (manager, _clock) = manager_with_clock();
        let _ = manager.reader().expect("reader should reload");
        // When: a second reader() call happens at the same instant.
        let _ = manager.reader().expect("reader should serve cached searcher");
        // Then: only the first call performed an explicit reload.
        assert_eq!(
            manager.reload_count.load(Ordering::Relaxed),
            1,
            "reader() within the throttle window must not reload again"
        );
    }

    #[test]
    fn reader_reloads_after_window_elapses() {
        // Given: a manager whose clock can be advanced.
        let (manager, clock) = manager_with_clock();
        let _ = manager.reader().expect("reader should reload");
        assert_eq!(manager.reload_count.load(Ordering::Relaxed), 1);
        *clock.lock().unwrap() += Duration::from_secs(2); // nosemgrep: rust-mutex-lock-unwrap
        // When: reader() is called after the 1s window has elapsed.
        let _ = manager.reader().expect("reader should reload");
        // Then: a fresh explicit reload happens for freshness.
        assert_eq!(
            manager.reload_count.load(Ordering::Relaxed),
            2,
            "reader() must reload again once the throttle window has elapsed"
        );
    }
}
