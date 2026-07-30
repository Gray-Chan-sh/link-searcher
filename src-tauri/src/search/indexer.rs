use tantivy::schema::*;
use tantivy::{doc, DateTime, IndexWriter};
use tantivy::TantivyError;

use crate::search::schema::build_schema;

/// High-level operations for writing documents into the tantivy index.
pub struct Indexer;

impl Indexer {
    /// Add a new document to the index.
    ///
    /// The document is buffered in the writer until [`commit`](IndexWriter::commit)
    /// is called, at which point it becomes visible to searchers.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer has been closed or a field value is
    /// incompatible with the schema.
    pub fn add_document(
        writer: &mut IndexWriter,
        file_id: &str,
        file_name: &str,
        file_ext: &str,
        dir_id: &str,
        content: &str,
        mtime: i64,
        file_size: u64,
    ) -> Result<(), TantivyError> {
        let schema = build_schema();

        let file_id_field = schema.get_field("file_id").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let file_name_field = schema.get_field("file_name").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let file_ext_field = schema.get_field("file_ext").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let dir_id_field = schema.get_field("dir_id").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let content_field = schema.get_field("content").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let content_suggest_field = schema.get_field("content_suggest").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let mtime_field = schema.get_field("mtime").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;
        let file_size_field = schema.get_field("file_size").map_err(|e| TantivyError::InvalidArgument(format!("{e:?}")))?;

        let document = doc!(
            file_id_field => file_id,
            file_name_field => file_name,
            file_ext_field => file_ext.to_lowercase(),
            dir_id_field => dir_id,
            content_field => content,
            content_suggest_field => content,
            mtime_field => DateTime::from_timestamp_micros(mtime),
            file_size_field => file_size,
        );

        writer.add_document(document)?;
        Ok(())
    }

    /// Delete all documents matching the given `file_id`.
    ///
    /// The deletion is buffered until the next commit.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer is closed.
    pub fn delete_document(
        writer: &mut IndexWriter,
        file_id: &str,
    ) -> Result<(), TantivyError> {
        let schema = build_schema();
        let file_id_field = schema.get_field("file_id")?;
        writer.delete_term(Term::from_field_text(file_id_field, file_id));
        Ok(())
    }

    /// Commit pending additions and deletions, making them visible to
    /// subsequent searches.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer is closed or a disk write fails.
    pub fn commit(writer: &mut IndexWriter) -> Result<(), TantivyError> {
        writer.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::schema::build_schema;
    use crate::search::schema::register_tokenizers;
    use tantivy::Index;

    fn setup_index() -> Index {
        let schema = build_schema();
        let index = Index::create_in_ram(schema);
        register_tokenizers(&index);
        index
    }

    #[test]
    fn test_add_and_search_document() {
        let index = setup_index();
        let mut writer = index.writer(50_000_000).expect("writer");

        Indexer::add_document(
            &mut writer,
            "uuid-1",
            "test.txt",
            "txt",
            "dir-1",
            "hello world this is a test document",
            1_700_000_000_000_000,
            1024,
        )
        .expect("add_document");

        Indexer::commit(&mut writer).expect("commit");

        let reader = index.reader().expect("reader");
        let searcher = reader.searcher();

        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let query_parser =
            tantivy::query::QueryParser::for_index(&index, vec![content]);
        let query = query_parser.parse_query("hello").expect("parse query");
        let top_docs = searcher
            .search(&query, &tantivy::collector::TopDocs::with_limit(10))
            .expect("search");

        assert_eq!(top_docs.len(), 1, "should find exactly one document");
    }

    #[test]
    fn test_delete_document() {
        let index = setup_index();
        let mut writer = index.writer(50_000_000).expect("writer");

        Indexer::add_document(
            &mut writer,
            "uuid-to-delete",
            "delete-me.txt",
            "txt",
            "dir-1",
            "content to delete",
            1_700_000_000_000_000,
            512,
        )
        .expect("add_document");

        Indexer::commit(&mut writer).expect("commit");

        // Delete and re-commit.
        Indexer::delete_document(&mut writer, "uuid-to-delete")
            .expect("delete_document");
        Indexer::commit(&mut writer).expect("commit");

        let reader = index.reader().expect("reader");
        let searcher = reader.searcher();

        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let query_parser =
            tantivy::query::QueryParser::for_index(&index, vec![content]);
        let query = query_parser.parse_query("delete").expect("parse query");
        let top_docs = searcher
            .search(&query, &tantivy::collector::TopDocs::with_limit(10))
            .expect("search");

        assert_eq!(top_docs.len(), 0, "deleted document should not be found");
    }

    #[test]
    fn test_commit_after_delete_makes_it_permanent() {
        let index = setup_index();
        let mut writer = index.writer(50_000_000).expect("writer");

        Indexer::add_document(
            &mut writer,
            "perm-uuid",
            "permanent.txt",
            "txt",
            "dir-1",
            "permanent content here",
            1_700_000_000_000_000,
        256,
        )
        .expect("add_document");
        Indexer::commit(&mut writer).expect("commit");

        // Confirm it's there.
        let reader = index.reader().expect("reader");
        let schema = build_schema();
        let content = schema.get_field("content").unwrap();
        let searcher = reader.searcher();
        let query_parser =
            tantivy::query::QueryParser::for_index(&index, vec![content]);
        let query = query_parser.parse_query("permanent").unwrap();
        let top_docs = searcher.search(&query, &tantivy::collector::TopDocs::with_limit(10)).unwrap();
        assert_eq!(top_docs.len(), 1);
    }
}
