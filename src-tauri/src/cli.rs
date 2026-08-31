use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

use crate::boot;
use crate::config;
use crate::db;
use crate::scanner::{FileWatcher, WatcherCommand};
use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
use crate::search::IndexManager;

#[derive(Parser)]
#[command(name = "link-searcher", about = "Cross-platform full-text search")]
pub enum Cli {
    /// Search the index from the command line
    #[command(visible_alias = "search")]
    Index {
        /// Search query
        query: String,
        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
    /// Scan a directory (or all configured dirs) and exit
    Scan {
        /// Directory to scan; defaults to all configured directories
        dir: Option<String>,
    },
    /// Watch a directory for file changes in real time
    Watch {
        /// Directory to watch
        dir: String,
    },
    /// Check index health
    Health,
    /// Ask the AI a question over the indexed documents (full-recall chat)
    Chat {
        /// The question to ask
        query: String,
        /// Optional retrieval scope (comma-separated file paths or dirs)
        #[arg(short, long, default_value = "")]
        scope: String,
        /// Enable full recall (retrieve and consider all matching files)
        #[arg(long)]
        full_recall: bool,
        /// Only run retrieval, skip the LLM call (prints retrieval results only)
        #[arg(long)]
        no_llm: bool,
        /// Dry run: 3-way scan + injection summary, skip LLM call
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    match cli {
        Cli::Index { query, limit } => {
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
                path_prefixes: None,
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
        Cli::Scan { dir } => {
            let data_dir = config::load_config().data_dir;
            let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;
            let dirs = match dir {
                Some(d) => {
                    let path = std::fs::canonicalize(&d)
                        .with_context(|| format!("cannot access directory: {d}"))?;
                    vec![ensure_dir_config(&bootstrap, &path)?]
                }
                None => {
                    let conn = bootstrap.pool.get().context("failed to get DB connection")?;
                    let dirs = db::dir_config::list_dirs(&conn).context("failed to list dirs")?;
                    drop(conn);
                    if dirs.is_empty() {
                        println!(
                            "No configured directories. Pass one: link-searcher scan /path"
                        );
                    }
                    dirs.into_iter()
                        .map(|d| (d.id, PathBuf::from(d.path)))
                        .collect()
                }
            };
            for (dir_id, dir_path) in &dirs {
                eprintln!("\n[scan] scanning {} ...", dir_path.display());
                let mut slog = crate::logs::session::SessionLogGuard::open(&data_dir.join("logs"), "scan");
                slog.write_line(&format!("[CLI] 开始扫描 {}", dir_path.display()));
                let result = bootstrap
                    .scanner
                    .full_scan(dir_id, |prog| {
                        eprint!(
                            "\r[scan] {} {}/{} {}",
                            prog.phase, prog.processed, prog.total, prog.current_file
                        );
                    })
                    .with_context(|| format!("scan failed for {}", dir_path.display()))?;
                let line = format!(
                    "[CLI] {}: {} files, {} indexed (added {}, modified {}, deleted {}, errors {}) in {} ms",
                    dir_path.display(),
                    result.total_files,
                    result.indexed,
                    result.added,
                    result.modified,
                    result.deleted,
                    result.errors,
                    result.duration_ms,
                );
                slog.write_line(&line);
                drop(slog);
                eprintln!();
                println!("{line}");
            }
        }
        Cli::Watch { dir } => {
            let data_dir = config::load_config().data_dir;
            let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;
            let path = std::fs::canonicalize(&dir)
                .with_context(|| format!("cannot access directory: {dir}"))?;
            let (dir_id, dir_path) = ensure_dir_config(&bootstrap, &path)?;

            let (watcher, event_rx) = FileWatcher::new();
            let watch_tx = watcher.tx().clone();
            watch_tx
                .send(WatcherCommand::StartWatch {
                    dir_id: dir_id.clone(),
                    path: dir_path.clone(),
                })
                .context("failed to start watcher")?;

            // Baseline scan first so existing files are tracked — mirrors the
            // GUI startup ordering (R3-11): StartWatch precedes the scan.
            let mut slog = crate::logs::session::SessionLogGuard::open(&data_dir.join("logs"), "scan");
            slog.write_line(&format!("[CLI] watch 基线扫描 {}", dir_path.display()));
            let result = bootstrap
                .scanner
                .startup_scan(&dir_id, |prog| {
                    eprint!(
                        "\r[scan] {} {}/{} {}",
                        prog.phase, prog.processed, prog.total, prog.current_file
                    );
                })
                .with_context(|| format!("baseline scan failed for {}", dir_path.display()))?;
            slog.write_line(&format!(
                "[CLI] watch 基线完成: {} files, {} indexed",
                result.total_files, result.indexed
            ));
            drop(slog);
            eprintln!();
            println!(
                "[watch] watching {} ({} files, {} indexed) — press Ctrl-C to stop",
                dir_path.display(),
                result.total_files,
                result.indexed
            );

            let scanner = bootstrap.scanner.clone();
            for event in event_rx {
                if let Err(e) = scanner.handle_event(event) {
                    eprintln!("[watch] error: {e}");
                }
            }
        }
        Cli::Chat { query, scope, full_recall, no_llm: _, dry_run } => {
            let data_dir = config::load_config().data_dir;
            let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;

            let (watcher_tx, _) = std::sync::mpsc::channel();
            let state = crate::state::AppState::new(
                bootstrap.pool.clone(),
                bootstrap.index_manager.clone(),
                bootstrap.indexer.clone(),
                bootstrap.scanner.clone(),
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                bootstrap.cancel_scan.clone(),
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                std::sync::Arc::new(std::sync::Mutex::new(crate::state::ScanDelta::default())),
                data_dir.clone(),
                data_dir.join(crate::config::INDEX_DIR_NAME),
                data_dir.join("data.db"),
                watcher_tx,
                None,
            );

            let mention_files: Vec<String> = scope
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let turn_scope = crate::commands::ai::TurnScope {
                mention_files,
                mention_dirs: vec![],
                inherit_from: vec![],
                conditions: vec![],
            };
            let messages = vec![crate::commands::ai::ChatMessage {
                role: "user".into(),
                content: query.clone(),
            }];

            if dry_run {
                let prepared = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("failed to create tokio runtime")?
                .block_on(crate::commands::ai::prepare_conversation_prompt(
                    &state, &messages, &[], &turn_scope, &[], false, full_recall, true, None, "",
                ))
                    .map_err(|e| anyhow::anyhow!("检索失败: {e}"))?;
                drop(prepared.events);

                println!(
                    "[三路检索] 命中 {} 份文件，注入 {} 条证据（full_recall={}）",
                    prepared.total_match_count, prepared.evidence.len(), full_recall
                );
                for (i, ev) in prepared.evidence.iter().enumerate().take(20) {
                    println!(
                        "  [{:>3}] bm25={} sem={} path={}",
                        i + 1,
                        ev.bm25_score.map(|s| format!("{s:.2}")).unwrap_or("-".into()),
                        ev.semantic_score.map(|s| format!("{s:.3}")).unwrap_or("-".into()),
                        ev.path
                    );
                }
                if prepared.evidence.len() > 20 {
                    println!("  ... 共 {} 条证据，仅显示前 20 条", prepared.evidence.len());
                }
                println!(
                    "[覆盖] 注入 {} 份全文 + {} 份摘要兜底 = {} 总覆盖（不遗漏）",
                    prepared.evidence.len(),
                    prepared.total_match_count.saturating_sub(prepared.evidence.len()),
                    prepared.total_match_count
                );
                return Ok(());
            }

            let total_files = {
                let c = bootstrap.pool.get().context("failed to get DB connection")?;
                crate::db::tracker::count_active_files(&c).context("count_active_files failed")?
            };
            let limit = if full_recall { (total_files as usize).max(500) } else { 100 };
            let bm25_hits = crate::commands::ai::bm25_relevant_hits(
                &state,
                &query.to_lowercase(),
                limit,
                false,
                None, None, None, None, None, None,
            )
            .map_err(|e| anyhow::anyhow!("bm25_relevant_hits failed: {e}"))?;

            println!(
                "[BM25] 共 {} 份文件，命中 {} 条（full_recall={} limit={}）",
                total_files,
                bm25_hits.len(),
                full_recall,
                limit
            );
            for (i, hit) in bm25_hits.iter().enumerate().take(20) {
                println!(
                    "  [{:>3}] score={:.2} path={}",
                    i + 1,
                    hit.bm25_score.unwrap_or(0.0),
                    hit.path
                );
            }
            if bm25_hits.len() > 20 {
                println!("  ... 仅显示前 20 条，共 {} 条命中", bm25_hits.len());
            }
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

/// Register `path` in `dir_config` (reusing an existing entry) and return its
/// `(dir_id, canonical path)`.
fn ensure_dir_config(bootstrap: &boot::Bootstrap, path: &Path) -> Result<(String, PathBuf)> {
    let path_str = path.to_string_lossy().to_string();
    let conn = bootstrap.pool.get().context("failed to get DB connection")?;
    let existing = db::dir_config::list_dirs(&conn)
        .context("failed to list dirs")?
        .into_iter()
        .find(|d| Path::new(&d.path) == path);
    if let Some(d) = existing {
        drop(conn);
        return Ok((d.id, path.to_path_buf()));
    }
    let created =
        db::dir_config::add_dir(&conn, &path_str, None, None, None, None, true)
            .context("failed to register directory")?;
    drop(conn);
    Ok((created.id, path.to_path_buf()))
}