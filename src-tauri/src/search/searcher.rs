use std::sync::OnceLock;
use std::time::Instant;

use regex::Regex;

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, QueryParser, QueryClone, RangeQuery, RegexQuery, TermQuery};
use tantivy::schema::*;
use tantivy::snippet::SnippetGenerator;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, Order, Searcher, TantivyDocument, TantivyError,
};

use crate::search::schema::build_schema;

/// Field to sort search results by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    Score,
    Date,
    Size,
    Name,
}

impl Default for SortField {
    fn default() -> Self {
        Self::Score
    }
}

/// Cap for the number of results loaded into memory for name sorting.
/// Beyond this cap, only the first 10_000 matching results are sorted by
/// file_name; users can narrow the search scope to see more.
const SORT_NAME_CAP: usize = 10_000;

/// Parameters for a search query.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchParams {
    /// The full-text query string.
    pub query: String,
    /// Optional list of directory IDs to scope the search (OR logic).
    pub dir_ids: Option<Vec<String>>,
    /// Optional list of file IDs to scope the search (OR logic via path prefix).
    pub file_ids: Option<Vec<String>>,
    /// Optional list of file extensions to filter by (OR logic, lowercase).
    pub ext_filter: Option<Vec<String>>,
    /// Optional start of date range (unix micros).
    pub date_from: Option<i64>,
    /// Optional end of date range (unix micros).
    pub date_to: Option<i64>,
    /// Enable fuzzy search with edit distance 1 (single-word queries only).
    #[serde(default)]
    pub fuzzy: bool,
    /// Sort field.
    pub sort: SortField,
    /// Sort order: "asc" or "desc".
    #[serde(default = "default_sort_order")]
    pub sort_order: String,
    /// Page number (1-based).
    #[serde(default = "default_page")]
    pub page: usize,
    /// Results per page.
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    /// When true and the AI gateway is configured, rerank hits by semantic
    /// similarity (RRF fusion of BM25 + embedding cosine).
    #[serde(default)]
    pub semantic: bool,
}

fn default_sort_order() -> String {
    "desc".to_string()
}
fn default_page() -> usize {
    1
}
fn default_page_size() -> usize {
    20
}

/// A single search hit returned to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub file_id: String,
    pub file_name: String,
    pub file_ext: String,
    pub path: String,
    pub snippet: String,
    pub score: f64,
    pub mtime: i64,
    pub file_size: u64,
}

/// Full search response including pagination metadata.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub total: u64,
    pub page: usize,
    pub page_size: usize,
    pub took_ms: u64,
    pub hits: Vec<SearchHit>,
}

/// High-level search operations against a tantivy index.
pub struct SearcherWrap {
    reader: IndexReader,
    index: Index,
}

impl SearcherWrap {
    /// Create a new searcher from an [`IndexReader`] and its parent [`Index`].
    pub fn new(reader: IndexReader, index: Index) -> Self {
        Self { reader, index }
    }

    /// Run a full-text search with optional filters, sorting, and pagination.
    ///
    /// Returns matching results along with total count and timing metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the query cannot be parsed or a field is missing.
    pub fn search(&self, params: &SearchParams) -> Result<SearchResponse, TantivyError> {
        let start = Instant::now();
        let searcher: Searcher = self.reader.searcher();
        let schema = build_schema();

        let query = self.build_query(&schema, params)?;

        // Compute total hits before pagination.
        let total = {
            let count_collector = tantivy::collector::Count;
            searcher.search(&*query, &count_collector)?
        };

        // Determine limit for fetching: need enough to cover the requested page.
        let limit = params.page * params.page_size;
        if limit == 0 {
            return Ok(SearchResponse {
                total: total as u64,
                page: params.page,
                page_size: params.page_size,
                took_ms: 0,
                hits: Vec::new(),
            });
        }

        // Determine sort field and order.
        let sort_order = if params.sort_order == "asc" {
            Order::Asc
        } else {
            Order::Desc
        };

        // TopDocs returns different types depending on the sort method.
        let doc_addrs_and_scores: Vec<(DocAddress, f64)> = match params.sort {
            SortField::Date => {
                let top = searcher.search(
                    &*query,
                    &TopDocs::with_limit(limit)
                        .order_by_fast_field::<DateTime>("mtime", sort_order),
                )?;
                let len = top.len();
                top.into_iter()
                    .enumerate()
                    .map(|(i, (_val, addr))| {
                        let rel_score = 1.0 - (i as f64 / len.max(1) as f64);
                        (addr, rel_score)
                    })
                    .collect()
            }
            SortField::Size => {
                let top = searcher.search(
                    &*query,
                    &TopDocs::with_limit(limit).order_by_u64_field("file_size", sort_order),
                )?;
                let len = top.len();
                top.into_iter()
                    .enumerate()
                    .map(|(i, (_val, addr))| {
                        let rel_score = 1.0 - (i as f64 / len.max(1) as f64);
                        (addr, rel_score)
                    })
                    .collect()
            }
            SortField::Name => {
                // Fetch ALL results, sort by file_name in Rust
                // (TEXT fields do not support fast-field sorting in Tantivy).
                let top = searcher.search(
                    &*query,
                    &TopDocs::with_limit(total.min(SORT_NAME_CAP) as usize),
                )?;
                let file_name_field = schema.get_field("file_name")?;
                let mut results: Vec<(DocAddress, f64, String)> = top
                    .into_iter()
                    .map(|(score, addr)| {
                        let file_name = searcher
                            .doc::<TantivyDocument>(addr)
                            .ok()
                            .and_then(|d| {
                                d.get_first(file_name_field)
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_lowercase())
                            })
                            .unwrap_or_default();
                        (addr, f64::from(score), file_name)
                    })
                    .collect();

                let desc = params.sort_order == "desc";
                results.sort_by(|a, b| {
                    if desc {
                        b.2.cmp(&a.2)
                    } else {
                        a.2.cmp(&b.2)
                    }
                });

                results
                    .into_iter()
                    .map(|(addr, score, _)| (addr, score))
                    .collect()
            }
            SortField::Score => {
                // Default: sort by score (BM25 relevance).
                let top = searcher.search(&*query, &TopDocs::with_limit(limit))?;
                top.into_iter().map(|(score, addr)| (addr, f64::from(score))).collect()
            }
        };

        // Paginate: skip to the requested page.
        let offset = (params.page.saturating_sub(1)) * params.page_size;
        let page_addrs: Vec<&(DocAddress, f64)> =
            doc_addrs_and_scores.iter().skip(offset).take(params.page_size).collect();

        // Resolve documents and build search hits.
        let file_id_field = schema.get_field("file_id")?;
        let file_name_field = schema.get_field("file_name")?;
        let file_ext_field = schema.get_field("file_ext")?;
        let path_field = schema.get_field("path")?;
        let content_field = schema.get_field("content")?;
        let mtime_field = schema.get_field("mtime")?;
        let file_size_field = schema.get_field("file_size")?;

        let snippet_generator = if !params.query.is_empty() {
            let fields = self.searchable_fields(&schema);
            let parser = QueryParser::for_index(&self.index, fields);
            if let Ok(q) = parser.parse_query(&params.query) {
                SnippetGenerator::create(&searcher, &*q, content_field).ok()
            } else {
                None
            }
        } else {
            None
        };

        let mut hits = Vec::with_capacity(page_addrs.len());
        for (addr, score) in &page_addrs {
            let doc: TantivyDocument = searcher.doc::<TantivyDocument>(*addr)?;

            let snippet = snippet_generator
                .as_ref()
                .map(|sg| sg.snippet_from_doc(&doc).to_html())
                .unwrap_or_default();

            let file_id = doc
                .get_first(file_id_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let file_name = doc
                .get_first(file_name_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let file_ext = doc
                .get_first(file_ext_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let path = doc
                .get_first(path_field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let mtime = doc
                .get_first(mtime_field)
                .and_then(|v| v.as_datetime())
                .map(|d| d.into_timestamp_micros())
                .unwrap_or(0);
            let file_size = doc
                .get_first(file_size_field)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            hits.push(SearchHit {
                file_id,
                file_name,
                file_ext,
                path,
                snippet,
                score: *score,
                mtime,
                file_size,
            });
        }

        let took_ms = start.elapsed().as_millis() as u64;

        Ok(SearchResponse {
            total: total as u64,
            page: params.page,
            page_size: params.page_size,
            took_ms,
            hits,
        })
    }

    /// Generate autocomplete suggestions by searching the `content_suggest`
    /// field with a regex prefix query and returning matching file names.
    ///
    /// # Errors
    ///
    /// Returns an error if the query cannot be parsed or the index is closed.
    pub fn suggest(&self, prefix: &str, limit: usize) -> Result<Vec<String>, TantivyError> {
        let searcher: Searcher = self.reader.searcher();
        let schema = build_schema();
        let content_suggest_field = schema.get_field("content_suggest")?;
        let file_name_field = schema.get_field("file_name")?;

        if prefix.is_empty() {
            return Ok(Vec::new());
        }

        // Use a regex query to match terms starting with the prefix.
        let pattern = format!("(?i){}.*", regex::escape(prefix));
        let regex_query = RegexQuery::from_pattern(&pattern, content_suggest_field)?;
        let top_docs = searcher.search(&regex_query, &TopDocs::with_limit(limit))?;

        let mut suggestions = Vec::with_capacity(top_docs.len());
        let mut seen = std::collections::HashSet::new();
        for (_score, addr) in &top_docs {
            let doc: TantivyDocument = searcher.doc::<TantivyDocument>(*addr)?;
            if let Some(val) = doc.get_first(file_name_field) {
                if let Some(text) = val.as_str() {
                    if seen.insert(text.to_string()) {
                        suggestions.push(text.to_string());
                    }
                }
            }
        }

        Ok(suggestions)
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Build the combined query from user text + filters.
    fn build_query(
        &self,
        schema: &Schema,
        params: &SearchParams,
    ) -> Result<Box<dyn tantivy::query::Query>, TantivyError> {
        let mut subqueries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();

        // 1. Parse filename:xxx prefix and build full-text query.
        let (filename_value, remaining_query) = parse_filename_prefix(&params.query);
        if let Some(fname) = &filename_value {
            let file_name_field = schema.get_field("file_name")?;
            // Regex matches the filename token anywhere (README: 任意位置),
            // case-insensitive on the lowercased indexed tokens.
            let pattern = format!("(?i).*{}.*", regex::escape(&fname.to_lowercase()));
            let rq: Box<dyn tantivy::query::Query> =
                Box::new(RegexQuery::from_pattern(&pattern, file_name_field)?);
            subqueries.push((Occur::Must, rq));
        }

        if !remaining_query.is_empty() {
            if params.fuzzy && !remaining_query.contains(' ') {
                // Single word fuzzy search across all searchable fields.
                let fields = self.searchable_fields(schema);
                let mut sub = Vec::new();
                for field in fields {
                    let term = Term::from_field_text(field, &remaining_query.to_lowercase());
                    let fq = FuzzyTermQuery::new(term, 1, true);
                    sub.push((Occur::Should, Box::new(fq) as Box<dyn Query>));
                }
                subqueries.push((Occur::Must, Box::new(BooleanQuery::new(sub))));
            } else {
                let fields = self.searchable_fields(schema);
                let parser = QueryParser::for_index(&self.index, fields);
                let parsed = parser.parse_query(&remaining_query)?;
                subqueries.push((Occur::Must, parsed));
            }
        } else if filename_value.is_none() {
            // No query text → match everything (only if no filename filter either).
            subqueries.push((Occur::Must, Box::new(tantivy::query::AllQuery)));
        }

        // 2. dir_id filter (Must + Should: at least one dir_id must match).
        if let Some(dir_ids) = &params.dir_ids {
            if !dir_ids.is_empty() {
                let dir_id_field = schema.get_field("dir_id")?;
                let should_queries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = dir_ids
                    .iter()
                    .map(|id| {
                        let tq: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
                            Term::from_field_text(dir_id_field, id),
                            IndexRecordOption::Basic,
                        ));
                        (Occur::Should, tq)
                    })
                    .collect();
                let dir_filter = BooleanQuery::new(should_queries);
                subqueries.push((Occur::Must, Box::new(dir_filter)));
            }
        }

        // 3. file_ext filter (Must + Should: OR logic).
        if let Some(exts) = &params.ext_filter {
            if !exts.is_empty() {
                let ext_field = schema.get_field("file_ext")?;
                let should_queries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = exts
                    .iter()
                    .map(|ext| {
                        let tq: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
                            Term::from_field_text(ext_field, &ext.to_lowercase()),
                            IndexRecordOption::Basic,
                        ));
                        (Occur::Should, tq)
                    })
                    .collect();
                let ext_filter = BooleanQuery::new(should_queries);
                subqueries.push((Occur::Must, Box::new(ext_filter)));
            }
        }

        // 4. Date range filter (Must).
        if params.date_from.is_some() || params.date_to.is_some() {
            let date_from = params
                .date_from
                .map(|d| DateTime::from_timestamp_micros(d));
            let date_to = params.date_to.map(|d| DateTime::from_timestamp_micros(d));
            let range_query: Box<dyn tantivy::query::Query> = Box::new(RangeQuery::new_date_bounds(
                "mtime".to_string(),
                match date_from {
                    Some(d) => std::ops::Bound::Included(d),
                    None => std::ops::Bound::Unbounded,
                },
                match date_to {
                    Some(d) => std::ops::Bound::Included(d),
                    None => std::ops::Bound::Unbounded,
                },
            ));
            subqueries.push((Occur::Must, range_query));
        }

        // 5. file_id filter (Must + Should: OR logic for multiple file_ids).
        if let Some(file_ids) = &params.file_ids {
            if !file_ids.is_empty() {
                let file_id_field = schema.get_field("file_id")?;
                let should_queries: Vec<(Occur, Box<dyn tantivy::query::Query>)> = file_ids
                    .iter()
                    .map(|id| {
                        let tq: Box<dyn tantivy::query::Query> = Box::new(TermQuery::new(
                            Term::from_field_text(file_id_field, id),
                            IndexRecordOption::Basic,
                        ));
                        (Occur::Should, tq)
                    })
                    .collect();
                let file_id_filter = BooleanQuery::new(should_queries);
                subqueries.push((Occur::Must, Box::new(file_id_filter)));
            }
        }

        Ok(BooleanQuery::new(subqueries).box_clone())
    }

    /// Return the list of fields that can be searched by the query parser.
    fn searchable_fields(&self, schema: &Schema) -> Vec<Field> {
        let mut fields = Vec::new();
        for name in ["file_name", "content"] {
            if let Ok(f) = schema.get_field(name) {
                fields.push(f);
            }
        }
        fields
    }
}

/// Extract `filename:xxx` or `filename:"xxx yyy"` from a query string using regex.
///
/// Supports occurrences anywhere in the string (not only at the start).
/// If multiple `filename:` values are present, the last one wins.
/// Returns `(Some(value), remaining_query)` if found, or `(None, original_query)` otherwise.
fn parse_filename_prefix(query: &str) -> (Option<String>, String) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?i)(?:^|\s)filename:("([^"]+)"|(\S+))"#)
            .expect("static filename regex is valid")
    });
    let mut last_value: Option<String> = None;
    let mut segments: Vec<(usize, usize)> = Vec::new(); // (start, end) byte offsets of each match

    for cap in re.captures_iter(query) {
        let m = match cap.get(0) {
            Some(m) => m,
            None => continue,
        };
        let value = cap.get(2).or_else(|| cap.get(3)).map(|m| m.as_str()).unwrap_or("");
        last_value = Some(value.to_string());
        segments.push((m.start(), m.end()));
    }

    if segments.is_empty() {
        return (None, query.to_string());
    }

    // Rebuild query without filename: matches
    let mut remaining = String::new();
    let mut cursor = 0;
    for &(start, end) in &segments {
        remaining.push_str(&query[cursor..start]);
        cursor = end;
    }
    remaining.push_str(&query[cursor..]);

    // Normalize whitespace after removals
    let remaining = remaining.split_whitespace().collect::<Vec<_>>().join(" ");

    (last_value, remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::indexer::Indexer;
    use crate::search::schema::{build_schema, register_tokenizers};
    use tantivy::Index;

    fn setup_index_with_docs() -> (Index, IndexReader) {
        let schema = build_schema();
        let index = Index::create_in_ram(schema);
        register_tokenizers(&index);

        let mut writer = index.writer(50_000_000).unwrap();

        // Doc 1
        Indexer::add_document(
            &mut writer,
            "uuid-1",
            "report.pdf",
            "pdf",
            "dir-a",
            "/home/user/report.pdf",
            "annual financial report for 2024 showing revenue growth",
            1_700_000_000_000_000,
            204800,
        )
        .unwrap();

        // Doc 2
        Indexer::add_document(
            &mut writer,
            "uuid-2",
            "notes.txt",
            "txt",
            "dir-a",
            "/home/user/notes.txt",
            "meeting notes about project planning and resource allocation",
            1_700_000_000_100_000,
            4096,
        )
        .unwrap();

        // Doc 3 – different dir
        Indexer::add_document(
            &mut writer,
            "uuid-3",
            "code.rs",
            "rs",
            "dir-b",
            "/home/user/code.rs",
            "rust implementation of the search algorithm with efficient indexing",
            1_700_000_000_200_000,
            8192,
        )
        .unwrap();

        // Doc 4 – Chinese content
        Indexer::add_document(
            &mut writer,
            "uuid-4",
            "中文文档.txt",
            "txt",
            "dir-a",
            "/home/user/中文文档.txt",
            "这是一个关于搜索引擎的中文测试文档",
            1_700_000_000_300_000,
            1024,
        )
        .unwrap();

        Indexer::commit(&mut writer).unwrap();
        drop(writer);

        let reader = index.reader().unwrap();
        (index, reader)
    }

    #[test]
    fn test_search_text_query() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        let params = SearchParams {
            query: "financial report".to_string(),
            dir_ids: None,
            file_ids: None,
            ext_filter: None,
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic: false,
        };

        let resp = searcher.search(&params).expect("search");
        assert!(resp.total >= 1, "should find at least one document");
        assert!(
            resp.hits.iter().any(|h| h.file_id == "uuid-1"),
            "report.pdf should be in results"
        );
    }

    #[test]
    fn test_search_with_dir_filter() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        let params = SearchParams {
            query: String::new(),
            dir_ids: Some(vec!["dir-b".to_string()]),
            file_ids: None,
            ext_filter: None,
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic: false,
        };

        let resp = searcher.search(&params).expect("search");
        assert_eq!(resp.total, 1, "only one doc in dir-b");
        assert_eq!(resp.hits[0].file_id, "uuid-3");
    }

    #[test]
    fn test_search_with_ext_filter() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        let params = SearchParams {
            query: String::new(),
            dir_ids: None,
            file_ids: None,
            ext_filter: Some(vec!["pdf".to_string()]),
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic: false,
        };

        let resp = searcher.search(&params).expect("search");
        assert_eq!(resp.total, 1, "only one pdf");
        assert_eq!(resp.hits[0].file_id, "uuid-1");
    }

    #[test]
    fn test_pagination() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        // Page 1 with page_size=2.
        let params = SearchParams {
            query: String::new(),
            dir_ids: None,
            file_ids: None,
            ext_filter: None,
            date_from: None,
            date_to: None,
            sort: SortField::Date,
            sort_order: "asc".to_string(),
            page: 1,
            page_size: 2,
            fuzzy: false,
            semantic: false,
        };
        let resp = searcher.search(&params).expect("search");
        assert_eq!(resp.total, 4);
        assert_eq!(resp.hits.len(), 2);

        // Page 2.
        let params2 = SearchParams {
            page: 2,
            ..params
        };
        let resp2 = searcher.search(&params2).expect("search");
        assert_eq!(resp2.hits.len(), 2);
        // Ensure different results from page 1.
        let ids_page1: Vec<&str> = resp.hits.iter().map(|h| h.file_id.as_str()).collect();
        let ids_page2: Vec<&str> = resp2.hits.iter().map(|h| h.file_id.as_str()).collect();
        for id in &ids_page2 {
            assert!(!ids_page1.contains(id), "no overlap between pages");
        }
    }

    #[test]
    fn test_suggest() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        let suggestions = searcher.suggest("finan", 10).expect("suggest");
        // Should find at least one matching document (content_suggest matches "financial").
        assert!(
            !suggestions.is_empty(),
            "suggest should find at least one result, got: {suggestions:?}"
        );
    }

    #[test]
    fn test_search_chinese() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        let params = SearchParams {
            query: "搜索".to_string(),
            dir_ids: None,
            file_ids: None,
            ext_filter: None,
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic: false,
        };

        let resp = searcher.search(&params).expect("search");
        assert!(
            resp.total >= 1,
            "Chinese query should find the Chinese doc"
        );
        assert_eq!(resp.hits[0].file_id, "uuid-4");
    }

    #[test]
    fn filename_matches_any_position_case_insensitively() {
        let (index, reader) = setup_index_with_docs();
        let searcher = SearcherWrap::new(reader, index);

        let params = SearchParams {
            query: "filename:REPORT".to_string(), // uppercase query, lowercase index side
            dir_ids: None,
            file_ids: None,
            ext_filter: None,
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic: false,
        };

        let resp = searcher.search(&params).expect("search");
        assert!(
            resp.hits.iter().any(|h| h.file_id == "uuid-1"),
            "filename:REPORT should match report.pdf"
        );

        // Any-position match: a substring of the filename must hit too.
        let params = SearchParams {
            query: "filename:epor".to_string(), // "report" middle substring
            dir_ids: None,
            file_ids: None,
            ext_filter: None,
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic: false,
        };
        let resp = searcher.search(&params).expect("search");
        assert!(
            resp.hits.iter().any(|h| h.file_id == "uuid-1"),
            "filename:epor should match report.pdf (any position)"
        );
    }
}
