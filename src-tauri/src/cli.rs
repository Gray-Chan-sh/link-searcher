use anyhow::{Context, Result};
use clap::Parser;

use crate::config;
use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
use crate::search::IndexManager;

#[derive(Parser)]
#[command(name = "link-searcher", about = "Cross-platform full-text search")]
pub enum Cli {
    /// Search the index from the command line
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
    /// Check index health
    Health,
}

pub fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    match cli {
        Cli::Search { query, limit } => {
            let data_dir = config::load_config().data_dir;
            let index_dir = data_dir.join(crate::config::INDEX_DIR_NAME);

            let index = IndexManager::open_or_create(&index_dir).context("failed to open index")?;
            let reader = index.reader().context("failed to create reader")?;
            let searcher = SearcherWrap::new(reader.clone(), index.index().as_ref().clone());

            let params = SearchParams {
                query,
                dir_ids: None,
                file_ids: None,
                ext_filter: None,
                date_from: None,
                date_to: None,
                sort: SortField::Score,
                sort_order: "desc".to_string(),
                page: 1,
                page_size: limit,
                fuzzy: false,
                semantic: false,
            };

            let result = searcher.search(&params).context("search failed")?;
            for hit in &result.hits {
                println!(
                    "{} ({}): {:.2}",
                    hit.file_name, hit.file_ext, hit.score
                );
            }
            println!(
                "--- {} results in {}ms ---",
                result.total, result.took_ms
            );
        }
        Cli::Health => {
            let data_dir = config::load_config().data_dir;
            let index_dir = data_dir.join(crate::config::INDEX_DIR_NAME);
            let db_path = data_dir.join("data.db");

            println!("Link-Searcher index health check");
            println!("  Data dir: {}", data_dir.display());
            println!();

            // Check index exists and is readable
            if index_dir.join("meta.json").exists() {
                match IndexManager::open_or_create(&index_dir) {
                    Ok(index) => {
                        match index.reader() {
                            Ok(reader) => {
                                let searcher = reader.searcher();
                                let num_segments = searcher.segment_readers().len();
                                let num_docs = searcher.num_docs();
                                println!("  Index: OK");
                                println!("    Segments: {num_segments}");
                                println!("    Documents: {num_docs}");
                            }
                            Err(e) => println!("  Index reader: FAILED ({e})"),
                        }
                    }
                    Err(e) => println!("  Index open: FAILED ({e})"),
                }
            } else {
                println!("  Index: NOT FOUND (no index at {})", index_dir.display());
            }
            println!();

            // Check DB is accessible
            if db_path.exists() {
                match rusqlite::Connection::open(&db_path) {
                    Ok(conn) => {
                        match conn.query_row("PRAGMA integrity_check", [], |row| {
                            row.get::<_, String>(0)
                        }) {
                            Ok(result) => {
                                println!("  Database: OK");
                                println!("    Integrity check: {result}");

                                // Get some stats
                                let total_files: u64 = conn
                                    .query_row(
                                        "SELECT COUNT(*) FROM file_tracking",
                                        [],
                                        |row| row.get(0),
                                    )
                                    .unwrap_or(0);
                                let indexed: u64 = conn
                                    .query_row(
                                        "SELECT COUNT(*) FROM content_index",
                                        [],
                                        |row| row.get(0),
                                    )
                                    .unwrap_or(0);
                                println!("    Tracked files: {total_files}");
                                println!("    Indexed entries: {indexed}");
                            }
                            Err(e) => println!("  Database: INTEGRITY FAILED ({e})"),
                        }
                    }
                    Err(e) => println!("  Database: FAILED ({e})"),
                }
            } else {
                println!(
                    "  Database: NOT FOUND (no db at {})",
                    db_path.display()
                );
            }
        }
    }
    Ok(())
}