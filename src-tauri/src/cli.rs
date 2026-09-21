use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

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
        /// With --dry-run: also print the source-document char ranges injected
        #[arg(long)]
        dump_injected: bool,
        /// Prior turns as JSON: [{"role":"user","content":"…"}, …]
        #[arg(long)]
        history: Option<PathBuf>,
    },
    /// OCR-quality health check (headless)
    Quality {
        #[command(subcommand)]
        cmd: QualityCmd,
    },
    /// Generate QA pairs for documents (offline indexing for semantic retrieval)
    IndexQa {
        /// Max documents to process (0 = all)
        #[arg(short, long, default_value = "0")]
        limit: usize,
        /// Min char count (skip tiny docs)
        #[arg(long, default_value = "500")]
        min_chars: usize,
        /// Max char count (skip huge docs, use chunks instead)
        #[arg(long, default_value = "10000")]
        max_chars: usize,
        /// Questions per document
        #[arg(short, long, default_value = "5")]
        count: usize,
    },
}

#[derive(Subcommand)]
pub enum QualityCmd {
    /// Backfill quality scores for content rows that are missing them
    Backfill {
        /// Max rows to score (default 20000)
        #[arg(short, long)]
        limit: Option<usize>,
    },
    /// List low-quality files and the quality-score histogram
    Audit {
        /// Max rows to list (default 50)
        #[arg(short, long)]
        limit: Option<usize>,
        /// Maximum quality score to include (default 0.5)
        #[arg(long, default_value = "0.5")]
        max_score: f64,
    },
    /// Re-extract low-quality files (batch) or one file by id
    Reextract {
        /// Re-extract a single file by id
        #[arg(long)]
        file_id: Option<String>,
        /// Max files to re-extract in batch mode (default 100)
        #[arg(short, long)]
        limit: Option<usize>,
        /// Maximum quality score to include in batch mode (default 0.5)
        #[arg(long, default_value = "0.5")]
        max_score: f64,
        /// Confirm batch re-extraction (required without --file-id)
        #[arg(long)]
        yes: bool,
    },
}

fn fmt_score(score: Option<f64>) -> String {
    score.map(|s| format!("{s:.3}")).unwrap_or_else(|| "-".into())
}

fn build_messages(
    query: &str,
    history: Option<&PathBuf>,
) -> Result<Vec<crate::commands::ai::ChatMessage>> {
    let mut messages: Vec<crate::commands::ai::ChatMessage> = Vec::new();
    if let Some(path) = history {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read history file: {}", path.display()))?;
        let prior: Vec<serde_json::Value> =
            serde_json::from_str(&raw).context("history must be a JSON array of {role, content}")?;
        for m in prior {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string();
            let content = m.get("content").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            if !content.is_empty() {
                messages.push(crate::commands::ai::ChatMessage { role, content });
            }
        }
    }
    messages.push(crate::commands::ai::ChatMessage {
        role: "user".into(),
        content: query.to_string(),
    });
    Ok(messages)
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
                dedupe: true,
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
        Cli::Chat { query, scope, full_recall, no_llm, dry_run, dump_injected, history } => {
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
            let messages = build_messages(&query, history.as_ref())?;

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
                if let Some(n) = &prepared.clarify_note {
                    let mode = if prepared.clarify_blocking { "只问不答" } else { "提示+作答" };
                    println!("[澄清/{mode}] {n}");
                }
                for (i, ev) in prepared.evidence.iter().enumerate().take(30) {
                    println!(
                        "  [{:>3}] bm25={} sem={} path={}",
                        i + 1,
                        ev.bm25_score.map(|s| format!("{s:.2}")).unwrap_or("-".into()),
                        ev.semantic_score.map(|s| format!("{s:.3}")).unwrap_or("-".into()),
                        ev.path
                    );
                    if dump_injected {
                        let spans = ev
                            .injected_spans
                            .iter()
                            .map(|(s, e)| format!("{s}-{e}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("        spans=[{spans}]");
                    }
                }
                if prepared.evidence.len() > 30 {
                    println!("  ... 共 {} 条证据，仅显示前 30 条", prepared.evidence.len());
                }
                println!(
                    "[覆盖] 注入 {} 份全文 + {} 份摘要兜底 = {} 总覆盖（不遗漏）",
                    prepared.evidence.len(),
                    prepared.total_match_count.saturating_sub(prepared.evidence.len()),
                    prepared.total_match_count
                );
                return Ok(());
            }

            if no_llm {
                // BM25-only 检索展示（原行为）
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
                return Ok(());
            }

            // 完整 RAG 问答：三路检索 → 注入 → LLM → 引用标注
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
            let messages = build_messages(&query, history.as_ref())?;
            let prepared = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to create tokio runtime")?
                .block_on(crate::commands::ai::prepare_conversation_prompt(
                    &state, &messages, &[], &turn_scope, &[], false, full_recall, false, None, "",
                ))
                .map_err(|e| anyhow::anyhow!("检索失败: {e}"))?;
            if !prepared.has_evidence {
                println!("未在与当前范围匹配的文档中找到依据。建议换关键词或 @ 引用文件。");
                return Ok(());
            }
            eprintln!(
                "[RAG] 命中 {} 份，注入 {} 条证据，调用 LLM 中…",
                prepared.total_match_count,
                prepared.evidence.len()
            );
            let raw = crate::ai::chat(&prepared.system, &prepared.user_msg)
                .ok_or_else(|| anyhow::anyhow!("LLM 调用失败（检查网关配置或网络）"))?;
            let cited = crate::commands::ai::auto_cite(
                &crate::commands::ai::sanitize_citations(&raw, prepared.evidence.len()),
                &prepared.evidence,
            );
            println!("{cited}");
        }
        Cli::Quality { cmd } => match cmd {
            QualityCmd::Backfill { limit } => {
                let data_dir = config::load_config().data_dir;
                let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;
                let limit = limit.unwrap_or(20000).min(20000);
                let conn = bootstrap.pool.get().context("failed to get DB connection")?;
                let (processed, scored) =
                    crate::commands::index::run_quality_backfill(&conn, limit)
                        .context("quality backfill failed")?;
                drop(conn);
                println!("processed {processed} / scored {scored}");
            }
            QualityCmd::Audit { limit, max_score } => {
                let data_dir = config::load_config().data_dir;
                let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;
                let conn = bootstrap.pool.get().context("failed to get DB connection")?;
                let summary = db::tracker::get_quality_summary(&conn)
                    .context("failed to get quality summary")?;
                let rows = db::tracker::get_low_quality_files(&conn, max_score, limit.unwrap_or(50))
                    .context("failed to list low-quality files")?;
                drop(conn);

                println!("quality summary:");
                println!("  green        {}", summary.green);
                println!("  yellow       {}", summary.yellow);
                println!("  red          {}", summary.red);
                println!("  unevaluated  {}", summary.unevaluated);
                println!();
                println!("low-quality files (score < {max_score}):");
                if rows.is_empty() {
                    println!("  (none)");
                }
                for row in &rows {
                    println!(
                        "  {:>6.3}  chars={:<8}  flags={:<28}  {}",
                        row.quality_score.unwrap_or(0.0),
                        row.char_count,
                        row.quality_flags,
                        row.file_path.as_deref().unwrap_or(&row.md5),
                    );
                }
            }
            QualityCmd::Reextract { file_id, limit, max_score, yes } => {
                let data_dir = config::load_config().data_dir;
                let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;

                if let Some(id) = file_id {
                    let conn = bootstrap.pool.get().context("failed to get DB connection")?;
                    let rec = db::tracker::get_file_by_id(&conn, &id)
                        .context("failed to look up file")?
                        .ok_or_else(|| anyhow::anyhow!("file not found"))?;
                    let md5 = rec
                        .md5
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("file has no md5"))?;
                    let (old_score, old_count) =
                        match db::tracker::get_content_quality(&conn, &md5)
                            .context("failed to read quality state")?
                        {
                            Some(v) => v,
                            None => (None, 0),
                        };
                    drop(conn);

                    let outcome = crate::commands::index::reextract_one(
                        &bootstrap.pool,
                        &bootstrap.indexer,
                        &id,
                        old_score,
                        old_count,
                        None,
                    )
                    .context("re-extract failed")?;
                    println!(
                        "re-extracted={} old_score={} new_score={} reason={}",
                        outcome.reextracted,
                        fmt_score(outcome.old_score),
                        fmt_score(outcome.new_score),
                        outcome.reason.as_deref().unwrap_or("-"),
                    );
                } else {
                    let limit = limit.unwrap_or(100);
                    let conn = bootstrap.pool.get().context("failed to get DB connection")?;
                    let rows = db::tracker::get_low_quality_files(&conn, max_score, limit)
                        .context("failed to list low-quality files")?;
                    drop(conn);

                    let would = rows
                        .iter()
                        .filter(|r| r.file_id.is_some() && r.reextract_count < 3)
                        .count();
                    if !yes {
                        println!(
                            "{would} file(s) would be re-extracted ({} low-quality below {max_score}); pass --yes to proceed",
                            rows.len(),
                        );
                        return Err(anyhow::anyhow!(
                            "aborted: --yes required for batch re-extraction"
                        ));
                    }

                    let mut processed = 0usize;
                    let mut ok = 0usize;
                    let mut failed = 0usize;
                    let mut exhausted = 0usize;
                    for row in &rows {
                        let Some(ref id) = row.file_id else {
                            continue;
                        };
                        if row.reextract_count >= 3 {
                            exhausted += 1;
                            continue;
                        }
                        processed += 1;
                        match crate::commands::index::reextract_one(
                            &bootstrap.pool,
                            &bootstrap.indexer,
                            id,
                            row.quality_score,
                            row.reextract_count,
                            None,
                        ) {
                            Ok(outcome) if outcome.reextracted => {
                                if outcome.reason.as_deref() == Some("exhausted") {
                                    exhausted += 1;
                                } else {
                                    ok += 1;
                                }
                            }
                            Ok(_) => exhausted += 1,
                            Err(e) => {
                                eprintln!("[reextract] {id}: {e}");
                                failed += 1;
                            }
                        }
                    }
                    println!("processed {processed} / ok {ok} / failed {failed} / exhausted {exhausted}");
                }
            }
        },
        Cli::IndexQa { limit, min_chars, max_chars, count } => {
            let data_dir = config::load_config().data_dir;
            let bootstrap = boot::bootstrap_core(&data_dir).context("failed to bootstrap core")?;
            let conn = bootstrap.pool.get().context("failed to get DB connection")?;

            let limit_clause = if limit == 0 { String::new() } else { format!("LIMIT {limit}") };
            let sql = format!(
                "SELECT ft.id, ci.md5, ci.text_content, ci.char_count
                 FROM content_index ci
                 JOIN file_tracking ft ON ft.md5 = ci.md5
                 WHERE ci.char_count BETWEEN ?1 AND ?2
                   AND ft.status = 'active'
                 ORDER BY ci.char_count ASC
                 {limit_clause}"
            );
            let mut stmt = conn.prepare(&sql).context("failed to prepare query")?;
            let rows = stmt
                .query_map(rusqlite::params![min_chars as i64, max_chars as i64], |row| {
                    let file_id: String = row.get(0)?;
                    let md5: String = row.get(1)?;
                    let text: String = row.get(2)?;
                    let char_count: i64 = row.get(3)?;
                    Ok((file_id, md5, text, char_count))
                })
                .context("failed to query docs")?;
            let mut docs: Vec<(String, String, String, i64)> = Vec::new();
            for row in rows {
                docs.push(row.context("failed to read row")?);
            }
            drop(stmt);

            let total = docs.len();
            if total == 0 {
                println!("No documents matching char_count {min_chars}..{max_chars} found.");
                return Ok(());
            }
            println!("[QA] {total} documents to process (count={count}, max_chars={max_chars})");

            let mut total_qa = 0usize;
            for (i, (file_id, _md5, text, char_count)) in docs.iter().enumerate() {
                let truncated = if text.chars().count() > max_chars {
                    text.chars().take(max_chars).collect::<String>()
                } else {
                    text.clone()
                };
                let system = format!(
                    "你是一个法律文档分析专家。阅读以下文档内容，生成{count}个该文档能回答的具体问题。要求：1）用自然口语提问，不要用正式法律术语；2）每个问题只问一个事实；3）每行一个问题，不要编号。只输出问题，不要解释。"
                );
                // Retry with backoff: remote API may rate-limit rapid calls.
                let mut reply_opt = None;
                for attempt in 1..=3 {
                    if attempt > 1 {
                        std::thread::sleep(std::time::Duration::from_secs(3 * attempt as u64));
                    }
                    if let Some(r) = crate::ai::chat(&system, &truncated) {
                        reply_opt = Some(r);
                        break;
                    }
                    eprintln!("[QA] {}/{total} {file_id} attempt {attempt}/3 failed", i + 1);
                }
                let Some(reply) = reply_opt else {
                    eprintln!("[QA] {}/{total} {file_id} → LLM call failed after 3 retries, skipping", i + 1);
                    continue;
                };
                // Pace: avoid overwhelming the remote API.
                std::thread::sleep(std::time::Duration::from_millis(500));
                let questions: Vec<&str> = reply
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .take(count)
                    .collect();
                let mut n = 0usize;
                for q in &questions {
                    if let Some(emb) = crate::ai::cached_embed(q) {
                        let bytes: Vec<u8> = emb
                            .iter()
                            .flat_map(|f| f.to_le_bytes())
                            .collect();
                        if let Err(e) = db::tracker::upsert_qa_pair(
                            &conn, file_id, q, emb.len(), &bytes,
                        ) {
                            eprintln!("[QA] upsert_qa_pair failed for {file_id}: {e}");
                            continue;
                        }
                        n += 1;
                    }
                }
                total_qa += n;
                println!("[QA] {}/{total} {file_id} (chars={char_count}) → {n} questions", i + 1);
            }
            println!("[QA] done: {total} documents, {total_qa} QA pairs stored");
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