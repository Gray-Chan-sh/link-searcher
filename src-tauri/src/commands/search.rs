use tauri::State;
use serde::Serialize;
use std::sync::atomic::Ordering;

use crate::db;
use crate::search::searcher::{SearchHit, SearchParams, SearchResponse, SortField, SearcherWrap};
use crate::state::AppState;

#[derive(Serialize)]
pub struct HistoryEntry {
    pub id: String,
    pub query: String,
    pub dir_ids: Option<String>,
    pub filters: Option<String>,
    pub result_count: u64,
    pub pinned: bool,
    pub created_at: i64,
}

/// Pair of (file_id, absolute path) for a single tracked file.
/// Returned by `search_file_ids_only` so the frontend can resolve
/// every selected id to its path in one round-trip (instead of
/// falling back to a 20-item page slice that silently dropped extras).
#[derive(Serialize, Clone)]
pub struct IdWithPath {
    pub file_id: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct FileTypeStat {
    pub extension: String,
    pub name: String,
    pub count: u64,
    pub indexed: u64,
    pub pending: u64,
    pub failed: u64,
}

fn file_type_name(ext: &str) -> String {
    match ext.to_lowercase().as_str() {
        "pdf" => "PDF".to_string(),
        "doc" => "Word".to_string(),
        "docx" => "Word".to_string(),
        "xls" => "Excel".to_string(),
        "xlsx" => "Excel".to_string(),
        "ppt" => "PowerPoint".to_string(),
        "pptx" => "PowerPoint".to_string(),
        "txt" => "Text".to_string(),
        "md" => "Markdown".to_string(),
        "rtf" => "RTF".to_string(),
        "odt" => "OpenDocument Text".to_string(),
        "ods" => "OpenDocument Spreadsheet".to_string(),
        "odp" => "OpenDocument Presentation".to_string(),
        _ => ext.to_uppercase(),
    }
}

/// Resolve absolute directory-tree paths (frontend filter) to `file_ids`.
///
/// DB stores paths RELATIVE to each dir root, so a raw absolute prefix
/// never matches. Map each path to its owning dir plus a relative prefix,
/// matching `(dir_id, rel)` or `(dir_id, rel/…) — this also excludes
/// sibling dirs that merely share a prefix (`docs` vs `docs2`).
/// Also supports legacy absolute paths stored before migration.
pub fn resolve_dir_paths(
    conn: &rusqlite::Connection,
    paths: &[String],
) -> Result<Option<Vec<String>>, String> {
    if paths.is_empty() {
        return Ok(None);
    }
    let dirs = crate::db::dir_config::list_dirs(conn).map_err(|e| format!("db error: {e}"))?;
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();
    let mut matched = false;
    for p in paths {
        let p_path = std::path::Path::new(p);
        for d in &dirs {
            let Ok(rel) = crate::scanner::helpers::to_relative(&d.path, p_path) else {
                continue;
            };
            matched = true;
            if rel.is_empty() {
                clauses.push("dir_id = ?".to_string());
                params.push(d.id.clone());
            } else {
                let escaped = rel.replace('%', "\\%").replace('_', "\\_");
                // Match both relative path form ("新建文件夹/doc.pdf") and
                // legacy absolute path form ("/Users/.../新建文件夹/doc.pdf").
                let abs_prefix = format!("{}/{}", d.path.trim_end_matches('/'), escaped);
                clauses.push(format!(
                    "(dir_id = ? AND (path = ? OR path LIKE ? ESCAPE '\\')) \
                     OR (dir_id = ? AND path LIKE ? ESCAPE '\\')"
                ));
                params.push(d.id.clone());
                params.push(escaped.clone());       // rel path
                params.push(format!("{escaped}/%")); // rel prefix
                params.push(d.id.clone());          // abs dir_id
                params.push(format!("{abs_prefix}/%")); // abs prefix
            }
            break;
        }
    }
    if !matched {
        // Selected paths aren't under any monitored root — nothing to search.
        return Ok(Some(Vec::new()));
    }
    let sql = format!(
        "SELECT id FROM file_tracking WHERE status = 'active' AND ({})",
        clauses.join(" OR ")
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("db prepare error: {e}"))?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|e| format!("db query error: {e}"))?;
    Ok(Some(rows.filter_map(|r| r.ok()).collect()))
}

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query: String,
    page: Option<usize>,
    page_size: Option<usize>,
    dir_ids: Option<Vec<String>>,
    dir_paths: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    sort: Option<SortField>,
    sort_order: Option<String>,
    date_from: Option<i64>,
    date_to: Option<i64>,
    fuzzy: Option<bool>,
    semantic: Option<bool>,
) -> Result<SearchResponse, String> {
    log::info!("[CMD] search called: query={:?} page={:?} page_size={:?} sort={:?} sort_order={:?}",
        query, page, page_size, sort, sort_order);
    if state.is_rebuilding.load(Ordering::SeqCst) {
        return Err("索引重建中，请稍后再试".to_string());
    }
    // Resolve dir_paths to file_ids via SQLite path prefix matching.
    let file_ids = if let Some(paths) = &dir_paths {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        let ids = resolve_dir_paths(&conn, paths)?;
        if ids.as_ref().is_some_and(|v| v.is_empty()) {
            return Ok(SearchResponse {
                total: 0,
page: page.unwrap_or(1).clamp(1, 10_000), // 上限防 TopDocs::with_limit(page*page_size) OOM
                page_size: page_size.unwrap_or(20),
                took_ms: 0,
                hits: Vec::new(),
                semantic_applied: false,
            });
        }
        ids
    } else {
        None
    };

    // Read max results setting
    let max_results: usize = {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
        conn.query_row(
            "SELECT value FROM app_settings WHERE key = 'max_results'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000)
    };

    let params = SearchParams {
        query: query.to_lowercase(),
        dir_ids,
        file_ids,
        ext_filter,
        date_from,
        date_to,
        path_prefixes: None,
        sort: sort.unwrap_or_default(),
        sort_order: sort_order.unwrap_or_else(|| "desc".to_string()),
        page: page.unwrap_or(1),
        page_size: page_size.unwrap_or(20).min(max_results).min(1000),
        fuzzy: fuzzy.unwrap_or(false),
        semantic: semantic.unwrap_or(false),
        dedupe: true,
    };

    let response = search_with_semantic(&state, params.clone()).map_err(|e| {
        log::error!(
            "[SEARCH] search failed: query={:?} dedupe={} error={e}",
            params.query,
            params.dedupe
        );
        e
    })?;

    let conn = state
        .db
        .get()
        .map_err(|e| format!("db connection failed: {e}"))?;
    let filters_json = if params.ext_filter.is_some() || params.date_from.is_some() || params.date_to.is_some() {
        let mut parts = Vec::new();
        if let Some(ref exts) = params.ext_filter {
            parts.push(format!("ext:{}", exts.join(",")));
        }
        if let Some(ref from) = params.date_from {
            parts.push(format!("from:{from}"));
        }
        if let Some(ref to) = params.date_to {
            parts.push(format!("to:{to}"));
        }
        Some(parts.join(";"))
    } else {
        None
    };
    let dir_ids_json = params.dir_ids.as_ref().map(|ids| ids.join(","));
    let _ = db::search_history::add_entry(
        &conn,
        &params.query,
        dir_ids_json.as_deref(),
        filters_json.as_deref(),
        response.total,
    );

    Ok(response)
}

/// 执行一次搜索，并在 `semantic` 打开且 Embedding 可用时做 RRF 融合。
///
/// ⚠️ **桌面命令与 webapi `/api/search` 必须共用这一份实现**。此前融合只写在 Tauri
/// 命令里，`/api/search` 虽然接收 `semantic` 参数、也塞进了 [`SearchParams`]，但
/// `SearcherWrap::search` **从不读这个字段** —— 后果是"桌面端语义开关有效、
/// 浏览器端静默无效"（2026-10-17 查出的缺陷）。把两条入口收口到同一个函数后，
/// 这类"两个入口两套行为"不会再发生。
///
/// 融合前会**释放索引管理锁**（`SearcherWrap` 自带 reader/index 克隆），
/// 否则一次最长 5s 的 embedding 调用会把索引写入挡在门外。
pub fn search_with_semantic(
    state: &AppState,
    params: SearchParams,
) -> Result<SearchResponse, String> {
    let searcher = {
        let mgr = state
            .index_manager
            .read()
            .map_err(|e| format!("index manager lock error: {e}"))?;
        let reader = mgr
            .reader()
            .map_err(|e| format!("failed to get reader: {e}"))?;
        let wrap = crate::search::searcher::SearcherWrap::new(
            reader.clone(),
            mgr.index().as_ref().clone(),
        );
        drop(mgr);
        wrap
    };

    let mut response = searcher
        .search(&params)
        .map_err(|e| format!("search failed: {e}"))?;

    // 语义融合：把 BM25 命中与 embedding-cosine top-N 按 RRF 融合，让"意思相近"
    // 的结果与关键词命中一起浮现。embedding 调用有 5s 超时，挂掉的网关只会降级为
    // 纯关键词，不会把搜索冻住。
    //
    // **翻页复用**（2026-10-18）：查询 + 过滤条件相同的融合顺序是确定的，而"查询向量 +
    // 全库余弦 + RRF"是整条路径最贵的一段（日志实测 ~4s）→ 缓存 5 分钟，
    // 语义下翻页第 2 页起几乎即时。
    if should_semantic_rerank(&params, crate::ai::embedding_enabled()) {
        // 全库 top-N BM25（同过滤、不分页）作 RRF 的 BM25 侧融合源：
        // 页内 20 条的 page-local rank 会让第 2+ 页命中完全不参与融合。
        let mut top_params = params.clone();
        top_params.page = 1;
        top_params.page_size = BM25_FUSION_TOP_N;
        match searcher.search(&top_params) {
            Ok(top) => {
                let key = semantic_cache_key(&params);
                let fusion = match cache_get(&key) {
                    Some(f) => f,
                    None => match compute_semantic_fusion(&state.db, &params.query, &top.hits) {
                        Ok(f) => {
                            cache_put(&key, &f);
                            f
                        }
                        Err(e) => {
                            log::warn!("[AI] semantic rerank skipped: {e}");
                            SemanticFusion::default()
                        }
                    },
                };
                if !fusion.order.is_empty() {
                    response.hits = materialize_fused_page(
                        &state.db,
                        &params.query,
                        &response.hits,
                        &top.hits,
                        &fusion,
                        params.page,
                        params.page_size,
                    );
                    response.semantic_applied = true;
                }
            }
            Err(e) => log::warn!("[AI] semantic rerank skipped: {e}"),
        }
    }

    // 关键词命中的片段不在索引里（tantivy 的 content 字段未 stored）→ 按需从 DB 补。
    // 放在语义融合之后：融合出来的语义命中已经自带片段，这里只填空的。
    match state.db.get() {
        Ok(conn) => enrich_missing_snippets(&conn, &params.query, &mut response.hits),
        Err(e) => log::warn!("[SEARCH] snippet enrich skipped: {e}"),
    }

    Ok(response)
}

/// 关键词命中的摘要片段补齐。
///
/// tantivy 索引里的 `content` 字段**不 stored**（省索引体积），而 `SnippetGenerator`
/// 只能从 stored 字段取文本 → BM25 命中的 `snippet` 一直是空的（结果列表那行空白）。
/// 这里按 `file_id` 从 DB 取正文，用与语义命中相同的 `semantic_snippet` 生成片段
/// （含 `<em>` 高亮，且已剥掉 Markdown 展示标记），**不需要重建索引**。
///
/// 成本控制：只补空片段；单页最多读 [`SNIPPET_TEXT_BUDGET`] 字节正文，
/// 超大文档（长音频转写等）宁可保持空片段，也不为摘要把整页全文读进内存。
fn enrich_missing_snippets(conn: &rusqlite::Connection, query: &str, hits: &mut [SearchHit]) {
    const SNIPPET_TEXT_BUDGET: usize = 4 * 1024 * 1024;
    if query.trim().is_empty() {
        return;
    }
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT ci.text_content FROM file_tracking ft \
         JOIN content_index ci ON ci.md5 = ft.md5 WHERE ft.id = ?1",
    ) else {
        log::warn!("[SEARCH] snippet enrich: prepare failed");
        return;
    };

    let mut budget = SNIPPET_TEXT_BUDGET;
    for hit in hits.iter_mut() {
        if !hit.snippet.is_empty() {
            continue;
        }
        if budget == 0 {
            break;
        }
        match stmt.query_row(rusqlite::params![hit.file_id], |r| r.get::<_, String>(0)) {
            Ok(text) => {
                if text.len() > budget {
                    budget = 0;
                    continue;
                }
                budget = budget.saturating_sub(text.len());
                hit.snippet = semantic_snippet(&text, query);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => log::warn!("[SEARCH] snippet enrich failed for {}: {e}", hit.file_id),
        }
    }
}

/// 语义融合的准入（纯函数，便于单测）：三个条件缺一不可 ——
/// 请求里明确要、Embedding 网关可用、查询非空。
fn should_semantic_rerank(params: &SearchParams, embedding_enabled: bool) -> bool {
    params.semantic && embedding_enabled && !params.query.is_empty()
}

/// How many corpus-wide BM25 hits feed the semantic RRF fusion. Fusing
/// page-local ranks would let page-2+ hits never participate.
const BM25_FUSION_TOP_N: usize = 100;

/// 语义融合的结果（供分页缓存复用）：融合后的前 [`SEMANTIC_ORDER_TOP`] 个 id + 余弦。
#[derive(Clone, Default)]
struct SemanticFusion {
    /// RRF 融合后的顺序（`file_id` + 融合分）。
    order: Vec<(String, f64)>,
    /// `file_id` → 余弦相似度（仅 `order` 里的 id；用于"语义相似度 %"前缀）。
    cos: std::collections::HashMap<String, f32>,
}

/// 语义融合缓存条目。
struct SemanticCacheEntry {
    key: String,
    stored: std::time::Instant,
    fusion: SemanticFusion,
}

/// 语义融合缓存：同一查询的**翻页**直接命中，免掉"查询向量 + 全库余弦 + RRF"（日志实测 ~4s）。
///
/// 语义下翻页的查询与过滤条件不变，融合顺序是确定的；索引变化时最多陈旧
/// [`SEMANTIC_CACHE_TTL`]（翻页场景可接受）。内存量级：每条"前 300 个 id + 余弦"≈ 几十 KB。
static SEMANTIC_CACHE: std::sync::Mutex<Vec<SemanticCacheEntry>> =
    std::sync::Mutex::new(Vec::new());
/// 缓存有效期。
const SEMANTIC_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
/// 缓存最多保留几条（超了淘汰最旧）。
const SEMANTIC_CACHE_MAX: usize = 8;
/// 融合/缓存保留的前 N 名（覆盖 60 页 × 5 条；更深的页本就不在语义路径的稳定范围内）。
const SEMANTIC_ORDER_TOP: usize = 300;

/// 缓存键：查询 + 所有影响检索/排序的参数（**不含** page/page_size —— 那正是要复用的维度）。
fn semantic_cache_key(params: &SearchParams) -> String {
    format!(
        "{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}",
        params.query,
        params.dir_ids,
        params.file_ids,
        params.ext_filter,
        params.path_prefixes,
        params.date_from,
        params.date_to,
        params.fuzzy,
        params.sort,
        params.sort_order,
    )
}

/// 在缓存表里查（纯函数，`now` 注入便于单测）。
fn cache_lookup<'a>(
    entries: &'a [SemanticCacheEntry],
    key: &str,
    now: std::time::Instant,
    ttl: std::time::Duration,
) -> Option<&'a SemanticFusion> {
    entries
        .iter()
        .find(|e| e.key == key && now.duration_since(e.stored) < ttl)
        .map(|e| &e.fusion)
}

/// 写入缓存（纯函数）：同 key 覆盖；超过 `max` 条时淘汰**最旧**的一条。
fn cache_store(entries: &mut Vec<SemanticCacheEntry>, entry: SemanticCacheEntry, max: usize) {
    entries.retain(|e| e.key != entry.key);
    entries.push(entry);
    while entries.len() > max {
        let Some((idx, _)) = entries.iter().enumerate().min_by_key(|(_, e)| e.stored) else {
            break;
        };
        entries.remove(idx);
    }
}

fn cache_get(key: &str) -> Option<SemanticFusion> {
    let now = std::time::Instant::now();
    let guard = SEMANTIC_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache_lookup(&guard, key, now, SEMANTIC_CACHE_TTL).cloned()
}

fn cache_put(key: &str, fusion: &SemanticFusion) {
    let mut guard = SEMANTIC_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache_store(
        &mut guard,
        SemanticCacheEntry {
            key: key.to_string(),
            stored: std::time::Instant::now(),
            fusion: fusion.clone(),
        },
        SEMANTIC_CACHE_MAX,
    );
}

/// 语义融合的**昂贵前半段**：查询向量（5s 超时）→ 全库余弦 → RRF 融合，截前 N 名。
///
/// `bm25_top`：全库 top-N BM25 命中（同过滤、不分页）作 RRF 的 BM25 侧来源。
/// 返回空 `order` = 没有任何向量（调用方退回纯关键词，且**不标记** `semantic_applied`）。
fn compute_semantic_fusion(
    pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    query: &str,
    bm25_top: &[SearchHit],
) -> Result<SemanticFusion, String> {
    use std::collections::HashMap;

    let q_vec = {
        let (tx, rx) = std::sync::mpsc::channel();
        let q = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(crate::ai::cached_embed(&q));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .ok()
            .and_then(|r| r)
    }
    .ok_or_else(|| "query embedding timeout (5s)".to_string())?;

    let conn = pool.get().map_err(|e| format!("db error: {e}"))?;
    let rows = crate::db::tracker::get_all_embeddings(&conn).map_err(|e| e.to_string())?;
    drop(conn);

    if rows.is_empty() {
        return Ok(SemanticFusion::default());
    }

    // Semantic cosine scores: file_id -> score.
    let mut scored: Vec<(String, f32)> = rows
        .iter()
        .map(|(fid, vec)| (fid.clone(), crate::ai::cosine(&q_vec, vec)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // RRF fusion: score = 1/(60 + rank) summed over BM25 and semantic lists.
    const K: f64 = 60.0;
    let mut fusion: HashMap<String, f64> = HashMap::new();
    for (i, hit) in bm25_top.iter().enumerate() {
        *fusion.entry(hit.file_id.clone()).or_insert(0.0) += 1.0 / (K + i as f64);
    }
    for (rank, (fid, _)) in scored.iter().enumerate() {
        *fusion.entry(fid.clone()).or_insert(0.0) += 1.0 / (K + rank as f64);
    }

    let mut order: Vec<(String, f64)> = fusion.into_iter().collect();
    order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    order.truncate(SEMANTIC_ORDER_TOP);

    // 余弦只保留 order 里的 id（缓存体积与 order 同量级）。
    let cos_all: HashMap<String, f32> = scored.into_iter().collect();
    let cos: HashMap<String, f32> = order
        .iter()
        .filter_map(|(id, _)| cos_all.get(id).map(|c| (id.clone(), *c)))
        .collect();

    Ok(SemanticFusion { order, cos })
}

/// 按融合顺序取出**当前页**的命中。
///
/// - 当前页的 BM25 命中**直接复用**（保留 tantivy 摘要）；
/// - 语义召回（BM25 未覆盖）的从 DB 物化（元数据 + 围绕查询词的高亮摘要）；
/// - 只保留"落在 BM25 作用域内"的融合结果（与旧实现一致：语义侧召回必须同过滤范围）；
/// - 页内不足时用剩余的 BM25 命中补齐（深页/过滤导致的空位）。
fn materialize_fused_page(
    pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    query: &str,
    bm25_page: &[SearchHit],
    bm25_top: &[SearchHit],
    fusion: &SemanticFusion,
    page: usize,
    page_size: usize,
) -> Vec<SearchHit> {
    use std::collections::{HashMap, HashSet};

    let mut by_id: HashMap<String, SearchHit> = bm25_page
        .iter()
        .cloned()
        .map(|h| (h.file_id.clone(), h))
        .collect();
    let allowed: HashSet<&str> = bm25_top.iter().map(|h| h.file_id.as_str()).collect();
    let start = page.saturating_sub(1).saturating_mul(page_size);

    let Ok(conn) = pool.get() else {
        return bm25_page.to_vec(); // 拿不到连接就退回纯 BM25 页，不报错
    };
    let mut hits: Vec<SearchHit> = Vec::with_capacity(page_size);
    for (fid, _) in fusion.order.iter().skip(start).take(page_size) {
        if let Some(h) = by_id.remove(fid) {
            hits.push(h);
            continue;
        }
        if !allowed.contains(fid.as_str()) {
            continue;
        }
        let cos = fusion.cos.get(fid).copied().unwrap_or(0.0);
        if let Some(h) = build_semantic_hit(&conn, query, fid, cos) {
            hits.push(h);
        }
    }
    // 页内不足：把本页 BM25 命中里没被融合顺序用到的补在末尾（不丢结果）。
    if hits.len() < page_size {
        let mut rest: Vec<SearchHit> = by_id.into_values().collect();
        rest.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.extend(rest.into_iter().take(page_size - hits.len()));
    }
    hits
}

/// 物化一条"语义召回"的命中：元数据从 DB 取，摘要围绕查询词高亮；
/// 纯语义命中（词不字面出现）前缀"语义相似度 %"，让用户知道**为什么**命中。
fn build_semantic_hit(
    conn: &rusqlite::Connection,
    query: &str,
    fid: &str,
    cos: f32,
) -> Option<SearchHit> {
    let rec = crate::db::tracker::get_file_by_id(conn, fid).ok().flatten()?;
    let file_name = std::path::Path::new(&rec.path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rec.path.clone());
    let raw_snippet = match rec.md5.as_deref() {
        Some(md5) => crate::db::tracker::get_content(conn, md5)
            .ok()
            .flatten()
            .map(|text| semantic_snippet(&text, query))
            .unwrap_or_default(),
        None => String::new(),
    };
    let snippet = if raw_snippet.contains("<em>") {
        raw_snippet
    } else if cos > 0.0 {
        format!("[语义相似度 {}%] {}", (cos * 100.0) as u32, raw_snippet)
    } else {
        raw_snippet
    };
    Some(SearchHit {
        file_id: fid.to_string(),
        file_name,
        file_ext: std::path::Path::new(&rec.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string(),
        path: rec.path.clone(),
        snippet,
        score: (cos * 100.0) as f64,
        mtime: rec.mtime,
        file_size: rec.size,
        duplicate_count: 0,
        duplicate_paths: vec![],
    })
}

/// 去掉 Markdown 的**展示层**标记：用于搜索结果片段 / AI 引用片段这类短文本，
/// **不改库里的正文**（正文里的 Markdown 是 anydoc 的结构产物，RAG 还需要它）。
///
/// 只处理确定是语法的形态，避免误伤正文：
/// - 成对的 `**` / `__`（加粗）、行内反引号
/// - 行首 `#{1,6} `（标题）、`>`（引用）、`- ` / `* ` / `+ `（列表）
/// - `[文字](url)` → `文字`，`![alt](url)` → `alt`
/// - 表格的 `|` 与 `|---|---|` 分隔行
///
/// `<em>` / `</em>`（片段高亮）原样保留。
pub(crate) fn strip_md_inline(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for raw_line in input.split('\n') {
        let mut body = raw_line.trim_start();
        if body.starts_with('#') {
            body = body.trim_start_matches('#').trim_start();
        } else if let Some(rest) = body.strip_prefix('>') {
            body = rest.trim_start();
        } else if let Some(rest) = body
            .strip_prefix("- ")
            .or_else(|| body.strip_prefix("* "))
            .or_else(|| body.strip_prefix("+ "))
        {
            body = rest;
        }
        if is_md_table_rule(body) {
            continue;
        }
        let no_links = strip_md_links(body);
        let no_marks = no_links
            .replace("**", "")
            .replace("__", "")
            .replace('`', "")
            .replace('|', " ");
        let squeezed = squeeze_ascii_spaces(&no_marks);
        let trimmed = squeezed.trim();
        if !trimmed.is_empty() {
            lines.push(trimmed.to_string());
        }
    }
    lines.join("\n")
}

/// 表格分隔行（`|---|---|`、`:--:` 等）——整行只有竖线/减号/冒号/空白。
fn is_md_table_rule(line: &str) -> bool {
    let mut has_dash = false;
    for c in line.chars() {
        match c {
            '-' => has_dash = true,
            '|' | ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    has_dash
}

/// `[文字](url)` → `文字`；`![alt](url)` → `alt`。括号不配对时原样保留。
fn strip_md_links(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(close_rel) = chars[i + 1..].iter().position(|&c| c == ']') {
                let close = i + 1 + close_rel;
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(paren_rel) = chars[close + 2..].iter().position(|&c| c == ')') {
                        let paren = close + 2 + paren_rel;
                        // 图片语法 `![alt](url)`：去掉前面的 `!`
                        if out.ends_with('!') {
                            out.pop();
                        }
                        out.extend(chars[i + 1..close].iter());
                        i = paren + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 连续 ASCII 空格/制表符压成一个空格（不动换行与全角空格）。
fn squeeze_ascii_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' || c == '\t' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

/// Build a human-readable snippet around the region where the document
/// best overlaps the query, so a semantic-only hit shows *why* it matched.
/// Uses character-level prefix matching (fast, no document tokenisation)
/// and limits the captured word to avoid whole-paragraph highlighting.
/// Falls back to the document opening when nothing overlaps.
///
/// 输出前统一剥掉 Markdown 展示标记：Office 文档的正文是 anydoc 产出的
/// Markdown（`**加粗**`、`|表格|`），直接展示会让用户在列表里看到 `**`。
fn semantic_snippet(content: &str, query: &str) -> String {
    strip_md_inline(&semantic_snippet_raw(content, query))
}

fn semantic_snippet_raw(content: &str, query: &str) -> String {
    let jieba = crate::search::schema::JIEBA.lock().unwrap_or_else(|e| e.into_inner());
    let terms: Vec<String> = jieba
        .cut(query, false)
        .into_iter()
        .map(|t| t.word.to_string())
        .filter(|t| t.chars().any(|c| c.is_ascii_alphanumeric() || !c.is_ascii()))
        .collect();
    if terms.is_empty() {
        return head_snippet(content);
    }

    // Build short prefixes (first 2 chars of each token) for fast matching.
    let mut prefixes: Vec<String> = Vec::new();
    for t in &terms {
        let p: String = t.chars().take(2).collect();
        if p.chars().count() == 2 && !prefixes.contains(&p) {
            prefixes.push(p);
        }
    }
    if prefixes.is_empty() {
        return head_snippet(content);
    }

    // Character-level search: find the first match of any prefix, capture
    // up to ~12 contiguous CJK/alnum characters around it (a single compound
    // word, not a whole sentence), and highlight the captured run.
    let lower = content.to_lowercase();
    const MAX_WORD: usize = 12;
    let mut best: Option<(usize, String)> = None;
    for p in &prefixes {
        let pl = p.to_lowercase();
        if let Some(pos) = lower.find(&pl) {
            let start = content.char_indices().rfind(|&(i,_)| i <= pos)
                .map(|(i,_)| i)
                .unwrap_or(pos);
            // Expand forward up to MAX_WORD chars but re-align to char boundary.
            let end = content[start..]
                .chars()
                .take(MAX_WORD)
                .fold(start, |acc, c| acc + c.len_utf8());
            let end = end.min(content.len());
            let captured = content[start..end].to_string();
            if best.as_ref().is_none_or(|(_, prev)| captured.len() > prev.len()) {
                best = Some((pos, captured));
            }
        }
    }
    let Some((_pos, matched)) = best else {
        return head_snippet(content);
    };
    snippet_around(content, &matched, |c| format!("<em>{c}</em>"))
}

/// Wrap `t` with highlight; used via closure to keep ownership simple.
fn snippet_around(content: &str, needle: &str, wrap: impl Fn(&str) -> String) -> String {
    let pos = content.to_lowercase().find(&needle.to_lowercase());
    let Some(pos) = pos else { return head_snippet(content).replace(needle, &wrap(needle)) };
    const WINDOW: usize = 60;

    let start = content.floor_char_boundary(pos.saturating_sub(WINDOW / 2));
    let end = content.ceil_char_boundary(
        (pos + needle.len() + WINDOW / 2).min(content.len()),
    );
    let (start, end) = if start >= end { (pos, pos.saturating_add(needle.len())) } else { (start, end) };

    let mut snippet = content[start..end].to_string();
    if start > 0 { snippet = format!("…{}", snippet); }
    if end < content.len() { snippet.push('…'); }
    snippet.replace(needle, &wrap(needle))
}

/// First up-to-~100 chars of content, char-boundary safe, with ellipsis.
/// Used as a no-highlight fallback for purely-semantic hits.
fn head_snippet(content: &str) -> String {
    const LIMIT: usize = 100;
    if content.chars().count() <= LIMIT {
        return content.to_string();
    }
    let cut = content.char_indices().nth(LIMIT).map(|(i, _)| i).unwrap_or(content.len());
    format!("{}…", &content[..cut])
}

#[tauri::command]
pub async fn suggest(state: State<'_, AppState>, prefix: String) -> Result<Vec<String>, String> {
    let mgr = state
        .index_manager
        .read()
        .map_err(|e| format!("index manager lock error: {e}"))?;
    let reader = mgr
        .reader()
        .map_err(|e| format!("failed to get reader: {e}"))?;
    let searcher = crate::search::searcher::SearcherWrap::new(
        reader.clone(),
        mgr.index().as_ref().clone(),
    );
    drop(mgr);
    searcher
        .suggest(&prefix, 10)
        .map_err(|e| format!("suggest failed: {e}"))
}

/// 文件树剪枝搜索：从根遍历，每个分支只返回最浅的命中节点（目录或文件），命中即停、不向下展开。
/// SQL 用 LIKE 预过滤（含命中段的路径必含搜索词，安全），Rust 端全量剪枝后返回所有结果（无 LIMIT）。
#[tauri::command]
pub async fn search_tree_prune(
    state: State<'_, AppState>,
    term: String,
) -> Result<Vec<String>, String> {
    search_tree_prune_impl(&state, term).await
}

pub async fn search_tree_prune_impl(
    app_state: &AppState,
    term: String,
) -> Result<Vec<String>, String> {
    let conn = app_state
        .db
        .get()
        .map_err(|e| format!("db connection failed: {e}"))?;
    // ponytail: LIKE 预过滤是安全的——一个会命中的目录段必然对应至少一条已索引文件路径，必含 term。
    // 若未来要支持"无已索引文件的空目录也搜得到"，需改为走文件系统遍历（get_dir_tree）。
    let like = format!("%{}%", term.to_lowercase());
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT path FROM file_tracking WHERE lower(path) LIKE ?1 AND status='active' ORDER BY path",
        )
        .map_err(|e| format!("prepare failed: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![like], |r| r.get::<_, String>(0))
        .map_err(|e| format!("query failed: {e}"))?;
    let paths: Vec<String> = rows
        .map(|r| r.map_err(|e| format!("row failed: {e}")))
        .collect::<Result<_, _>>()?;

    let term = term.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut pruned: Vec<String> = Vec::new();

    for path in paths {
        if pruned.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        let segs: Vec<&str> = path.split('/').collect();
        let mut prefix = String::new();
        for (i, seg) in segs.iter().enumerate() {
            prefix = if prefix.is_empty() {
                seg.to_string()
            } else {
                format!("{prefix}/{seg}")
            };
            if seg.to_lowercase().contains(&term) {
                out.push(prefix.clone());
                if i < segs.len() - 1 {
                    pruned.push(format!("{prefix}/"));
                }
                break;
            }
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn search_file_paths(
    state: State<'_, AppState>,
    prefix: String,
    limit: usize,
) -> Result<Vec<String>, String> {
    search_file_paths_impl(&state, prefix, limit).await
}

pub async fn search_file_paths_impl(
    app_state: &AppState,
    prefix: String,
    limit: usize,
) -> Result<Vec<String>, String> {
    let conn = app_state
        .db
        .get()
        .map_err(|e| format!("db connection failed: {e}"))?;
    let like = format!("%{}%", prefix.to_lowercase());
    let mut stmt = conn
        .prepare("SELECT DISTINCT path FROM file_tracking WHERE lower(path) LIKE ?1 AND status='active' ORDER BY path LIMIT ?2")
        .map_err(|e| format!("prepare failed: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![like, limit as i64], |r| r.get::<_, String>(0))
        .map_err(|e| format!("query failed: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("row failed: {e}"))?);
    }
    Ok(out)
}

#[tauri::command]
pub async fn get_search_history(state: State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> {
    get_search_history_impl(&state).await
}

pub async fn get_search_history_impl(app_state: &AppState) -> Result<Vec<HistoryEntry>, String> {
    let conn = app_state
        .db
        .get()
        .map_err(|e| format!("db connection failed: {e}"))?;
    let entries = db::search_history::list_recent(&conn, 10)
        .map_err(|e| format!("failed to list history: {e}"))?;
    Ok(entries
        .into_iter()
        .map(|e| HistoryEntry {
            id: e.id,
            query: e.query,
            dir_ids: e.dir_ids,
            filters: e.filters,
            result_count: e.result_count,
            pinned: e.pinned,
created_at: e.created_at,
        })
        .collect())
}

/// Clear the entire search history (all entries, pinned included).
#[tauri::command]
pub async fn clear_search_history(state: State<'_, AppState>) -> Result<(), String> {
    clear_search_history_impl(&state).await
}

pub async fn clear_search_history_impl(app_state: &AppState) -> Result<(), String> {
    let conn = app_state
        .db
        .get()
        .map_err(|e| format!("db connection failed: {e}"))?;
    db::search_history::clear_history(&conn).map_err(|e| format!("failed to clear history: {e}"))
}

#[tauri::command]
pub async fn export_search_results(
    state: State<'_, AppState>,
    query: String,
    dir_ids: Option<Vec<String>>,
    dir_paths: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    format: String,
) -> Result<String, String> {
    export_search_results_impl(&state, query, dir_ids, dir_paths, ext_filter, format).await
}

pub async fn export_search_results_impl(
    app_state: &AppState,
    query: String,
    dir_ids: Option<Vec<String>>,
    dir_paths: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    format: String,
) -> Result<String, String> {
    // Resolve dir_paths to file_ids via SQLite path prefix matching.
    let file_ids = if let Some(paths) = &dir_paths {
        let conn = app_state.db.get().map_err(|e| format!("db error: {e}"))?;
        let ids = resolve_dir_paths(&conn, paths)?;
        if ids.as_ref().is_some_and(|v| v.is_empty()) {
            return Ok(String::new());
        }
        ids
    } else {
        None
    };

    let dir_ids = dir_ids.unwrap_or_default();
    let ext_filter = ext_filter.unwrap_or_default();
    let dir_ids_opt = if dir_ids.is_empty() { None } else { Some(dir_ids) };
    let ext_filter_opt = if ext_filter.is_empty() { None } else { Some(ext_filter) };

    let export_page_size = {
        let conn = app_state.db.get().map_err(|e| format!("db error: {e}"))?;
        let max_results: usize = conn.query_row(
            "SELECT value FROM app_settings WHERE key = 'max_results'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
        max_results.min(5000)
    };

    let params = SearchParams {
        query: query.to_lowercase(),
        dir_ids: dir_ids_opt,
        file_ids,
        ext_filter: ext_filter_opt,
        date_from: None,
        date_to: None,
        path_prefixes: None,
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: 1,
        page_size: export_page_size,
        fuzzy: false,
        semantic: false,
        dedupe: false,
    };

    let mgr = app_state
        .index_manager
        .read()
        .map_err(|e| format!("index manager lock error: {e}"))?;
    let reader = mgr
        .reader()
        .map_err(|e| format!("failed to get reader: {e}"))?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());

    let response = searcher
        .search(&params)
        .map_err(|e| format!("search failed: {e}"))?;

    let mut content = String::new();
    match format.as_str() {
        "csv" => {
            content.push_str("file_name,file_ext,path,score,mtime,file_size\n");
            for hit in &response.hits {
                content.push_str(&format!(
                    "\"{}\",\"{}\",\"{}\",{},{},{}\n",
                    hit.file_name.replace('"', "\"\""),
                    hit.file_ext.replace('"', "\"\""),
                    hit.path.replace('"', "\"\""),
                    hit.score,
                    hit.mtime,
                    hit.file_size,
                ));
            }
        }
        _ => {
            for hit in &response.hits {
                content.push_str(&format!(
                    "{} ({}): {}\n",
                    hit.file_name, hit.file_ext, hit.snippet
                ));
            }
        }
    };
    drop(searcher);
    drop(mgr);
    Ok(content)
}
#[tauri::command]
pub async fn get_file_type_stats(state: State<'_, AppState>) -> Result<Vec<FileTypeStat>, String> {
    get_file_type_stats_impl(&state).await
}

pub async fn get_file_type_stats_impl(
    app_state: &AppState,
) -> Result<Vec<FileTypeStat>, String> {
    let conn = app_state.db.get().map_err(|e| format!("db error: {e}"))?;
    let sql = "SELECT file_ext, COUNT(*) as cnt, COALESCE(SUM(CASE WHEN indexed IN (1,3) THEN 1 ELSE 0 END),0) as idx, COALESCE(SUM(CASE WHEN indexed=0 THEN 1 ELSE 0 END),0) as pnd, COALESCE(SUM(CASE WHEN indexed=2 THEN 1 ELSE 0 END),0) as fld FROM file_tracking WHERE status='active' GROUP BY file_ext ORDER BY cnt DESC";
    let mut stmt = conn.prepare(sql).map_err(|e| format!("db prepare error: {e}"))?;
    let mut results = Vec::new();
    let rows = stmt.query_map([], |row| {
        let ext: String = row.get("file_ext")?;
        let cnt: i64 = row.get("cnt")?;
        let idx: i64 = row.get("idx")?;
        let pnd: i64 = row.get("pnd")?;
        let fld: i64 = row.get("fld")?;
        let name = file_type_name(&ext);
        Ok(FileTypeStat {
            extension: ext,
            name,
            count: cnt as u64,
            indexed: idx as u64,
            pending: pnd as u64,
            failed: fld as u64,
        })
    }).map_err(|e| format!("db query error: {e}"))?;
    for row in rows {
        let row = row.map_err(|e| format!("db query error: {e}"))?;
        results.push(row);
    }
    Ok(results)
}

/// Distinct file extensions present in tracked dirs, filtered to indexable
/// types only (used by the Browse page type dropdown).
#[tauri::command]
pub async fn get_browse_file_types(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    get_browse_file_types_impl(&state).await
}

pub async fn get_browse_file_types_impl(app_state: &AppState) -> Result<Vec<String>, String> {
    let supported = crate::extractor::get_supported_extensions();
    let conn = app_state.db.get().map_err(|e| format!("db error: {e}"))?;
    let sql = "SELECT file_ext, COUNT(*) as cnt FROM file_tracking WHERE status='active' GROUP BY file_ext ORDER BY cnt DESC";
    let mut stmt = conn.prepare(sql).map_err(|e| format!("db prepare error: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let ext: String = row.get("file_ext")?;
            let cnt: i64 = row.get("cnt")?;
            Ok((ext, cnt))
        })
        .map_err(|e| format!("db query error: {e}"))?;
    let mut types: Vec<String> = Vec::new();
    for row in rows {
        let (ext, _cnt) = row.map_err(|e| format!("db query error: {e}"))?;
        let lower = ext.to_lowercase();
        if !lower.is_empty() && supported.contains(&lower.as_str()) {
            types.push(lower);
        }
    }
    types.sort();
    Ok(types)
}

pub async fn search_file_ids_only_impl(
    app_state: &AppState,
    query: String,
    dir_ids: Option<Vec<String>>,
    dir_paths: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    semantic: Option<bool>,
) -> Result<Vec<IdWithPath>, String> {
    if app_state.is_rebuilding.load(Ordering::SeqCst) {
        return Err("索引重建中，请稍后再试".to_string());
    }
    let file_ids = if let Some(paths) = &dir_paths {
        let conn = app_state.db.get().map_err(|e| format!("db error: {e}"))?;
        let ids = resolve_dir_paths(&conn, paths)?;
        if ids.as_ref().is_some_and(|v| v.is_empty()) {
            return Ok(Vec::new());
        }
        ids
    } else {
        None
    };

    let params = SearchParams {
        query: query.to_lowercase(),
        dir_ids,
        file_ids,
        ext_filter,
        date_from: None,
        date_to: None,
        path_prefixes: None,
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: 1,
        page_size: 5000,
        fuzzy: false,
        semantic: semantic.unwrap_or(false),
        dedupe: false,
    };

    let response = search_with_semantic(app_state, params).map_err(|e| format!("{e}"))?;
    Ok(response
        .hits
        .into_iter()
        .map(|h| IdWithPath { file_id: h.file_id, path: h.path })
        .collect())
}

#[tauri::command]
pub async fn search_file_ids_only(
    state: State<'_, AppState>,
    query: String,
    dir_ids: Option<Vec<String>>,
    dir_paths: Option<Vec<String>>,
    ext_filter: Option<Vec<String>>,
    semantic: Option<bool>,
) -> Result<Vec<IdWithPath>, String> {
    search_file_ids_only_impl(&state, query, dir_ids, dir_paths, ext_filter, semantic).await
}

#[derive(Serialize)]
pub struct RefineSearchResponse {
    pub total: u64,
    pub hits: Vec<SearchHit>,
    pub took_ms: u64,
}

pub async fn refine_search_impl(
    app_state: &AppState,
    query: String,
    file_ids: Vec<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<RefineSearchResponse, String> {
    if app_state.is_rebuilding.load(Ordering::SeqCst) {
        return Err("索引重建中，请稍后再试".to_string());
    }
    if file_ids.is_empty() {
        return Ok(RefineSearchResponse { total: 0, hits: Vec::new(), took_ms: 0 });
    }

    let params = SearchParams {
        query: query.to_lowercase(),
        dir_ids: None,
        file_ids: Some(file_ids),
        ext_filter: None,
        date_from: None,
        date_to: None,
        path_prefixes: None,
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: page.unwrap_or(1),
        page_size: page_size.unwrap_or(200),
        fuzzy: false,
        semantic: false,
        dedupe: false,
    };

    let mgr = app_state.index_manager.read().map_err(|e| format!("{e}"))?;
    let reader = mgr.reader().map_err(|e| format!("{e}"))?;
    let searcher = crate::search::searcher::SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);

    let response = searcher.search(&params).map_err(|e| format!("{e}"))?;
    let total = response.total;
    let took_ms = response.took_ms;

    let conn = app_state.db.get().map_err(|e| format!("db error: {e}"))?;
    let mut hits: Vec<SearchHit> = Vec::new();
    for hit in response.hits {
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, &hit.file_id)
            && rec.status == "active" {
                hits.push(hit);
            }
    }
    drop(conn);

    Ok(RefineSearchResponse { total, hits, took_ms })
}

#[tauri::command]
pub async fn refine_search(
    state: State<'_, AppState>,
    query: String,
    file_ids: Vec<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<RefineSearchResponse, String> {
    refine_search_impl(&state, query, file_ids, page, page_size).await
}


#[cfg(test)]
mod snippet_tests {

    use super::{head_snippet, semantic_snippet, strip_md_inline};

    #[test]
    fn highlights_first_matching_term() {
        let s = semantic_snippet(
            "本案系物业服务合同纠纷，原告主张被告支付物业费及违约金。",
            "物业费 诉讼",
        );
        assert!(s.contains("<em>"), "expected highlight: {s}");
    }

    #[test]
    fn no_verbatim_term_falls_back_to_head() {
        // 纯语义命中（词不字面出现）：回退文档开头片段，不空白。
        let s = semantic_snippet("房屋租赁合同是当事人之间就房屋租赁权利义务所达成的协议", "物业费");
        assert!(s.contains("房屋租赁合同"), "got: {s}");
        assert!(!s.contains("<em>"), "no highlight for non-verbatim term: {s}");
    }

    #[test]
    fn head_snippet_truncates_cjk_safely() {
        let long: String = "物业费".repeat(200);
        let s = head_snippet(&long);
        assert!(s.ends_with('…'));
        assert!(s.len() <= 400, "got {} bytes", s.len());
    }

    #[test]
    fn multi_byte_char_boundary_no_panic() {
        // Window edges must not slice through a multi-byte char (CJK).
        let long_cn: String = "物业服务与".repeat(200); // pure CJK, > window
        let s = semantic_snippet(&long_cn, "物业");
        assert!(s.contains("<em>"), "expected highlight: {s}");
    }

    #[test]
    fn empty_query_falls_back_to_head() {
        // 空/无词查询：仍回退开头片段（预览可见），不空白。
        let s = semantic_snippet("任意文本内容", "  ");
        assert!(s.contains("任意文本"), "got: {s}");
    }

    #[test]
    fn composite_word_overlap_highlighted() {
        // 用户场景: 查询"物业费纠纷" 命中含"物业管理费合同案件"的文档。
        // snippet 应捕获并高亮更长复合词(而非必须精确等于查询词)。
        let s = semantic_snippet(
            "本案系物业管理费合同案件。原告主张被告支付拖欠的物业管理费。",
            "物业费纠纷",
        );
        assert!(s.contains("<em>"), "应高亮重叠词: {s}");
    }

    #[test]
    fn markdown_markers_are_stripped_from_snippets() {
        // Office 正文是 anydoc 产出的 Markdown：片段里不该露出 `**` / `|` / `#`。
        let s = semantic_snippet(
            "**坚持政府引导、规划先行。**建立健全政府引导、部门协同的工作机制。",
            "政府引导",
        );
        assert!(s.contains("<em>"), "应保留高亮: {s}");
        assert!(!s.contains("**"), "片段里不应出现加粗标记: {s}");
    }

    #[test]
    fn strip_md_inline_handles_common_markers() {
        assert_eq!(strip_md_inline("**加粗**文本"), "加粗文本");
        assert_eq!(strip_md_inline("## 二级标题"), "二级标题");
        assert_eq!(strip_md_inline("> 引用内容"), "引用内容");
        assert_eq!(strip_md_inline("- 列表项"), "列表项");
        assert_eq!(strip_md_inline("`inline code` 与 __下划线加粗__"), "inline code 与 下划线加粗");
    }

    #[test]
    fn strip_md_inline_handles_links_and_tables() {
        assert_eq!(
            strip_md_inline("详见[土地管理法](https://example.com/a?b=1)第五条"),
            "详见土地管理法第五条"
        );
        assert_eq!(strip_md_inline("![示意图](img.png) 图注"), "示意图 图注");
        assert_eq!(strip_md_inline("| 项目 | 金额 |"), "项目 金额");
        assert_eq!(strip_md_inline("|---|---|"), "");
    }

    #[test]
    fn strip_md_inline_keeps_plain_text_and_highlight_tags() {
        // 纯文本（含普通括号、下划线、星号作乘号）不应被改坏。
        let plain = "本案系物业服务合同纠纷（2023）皖0102民初123号，按3*4计算。";
        assert_eq!(strip_md_inline(plain), plain);
        // 片段高亮标签必须保留（前端按 <em> 渲染）。
        assert_eq!(strip_md_inline("<em>物业费</em>**纠纷**"), "<em>物业费</em>纠纷");
    }

    #[test]
    fn id_with_path_serializes_to_expected_json_shape() {
        // search_file_ids_only now returns Vec<IdWithPath> instead of Vec<String>
        // so the frontend can resolve all selected ids to paths in one round-trip
        // (fixes the "去聊天 only sends 20" bug). Lock the wire shape here.
        use super::IdWithPath;
        let row = IdWithPath {
            file_id: "abc-123".to_string(),
            path: "/Users/gray/Docs/合同.pdf".to_string(),
        };
        let json = serde_json::to_string(&row).unwrap();
        assert_eq!(json, r#"{"file_id":"abc-123","path":"/Users/gray/Docs/合同.pdf"}"#);
    }
}

/// 语义融合准入的测试：与 webapi `/api/search` 的修复直接相关 ——
/// "请求要 + 网关可用 + 查询非空"三条件缺一不可，避免"开关点了没变化"。
#[cfg(test)]
mod semantic_gate_tests {
    use super::{
        cache_lookup, cache_store, semantic_cache_key, should_semantic_rerank, SearchParams,
        SemanticCacheEntry, SemanticFusion, SortField,
    };

    fn params(query: &str, semantic: bool) -> SearchParams {
        SearchParams {
            query: query.to_string(),
            dir_ids: None,
            file_ids: None,
            ext_filter: None,
            path_prefixes: None,
            date_from: None,
            date_to: None,
            sort: SortField::Score,
            sort_order: "desc".to_string(),
            page: 1,
            page_size: 20,
            fuzzy: false,
            semantic,
            dedupe: true,
        }
    }

    #[test]
    fn semantic_rerank_requires_flag_gateway_and_query() {
        assert!(
            should_semantic_rerank(&params("合同", true), true),
            "三条件齐 → 融合"
        );
        assert!(
            !should_semantic_rerank(&params("合同", false), true),
            "没请求 → 不融合（默认纯关键词）"
        );
        assert!(
            !should_semantic_rerank(&params("合同", true), false),
            "Embedding 网关不可用 → 不融合（降级而不是报错）"
        );
        assert!(
            !should_semantic_rerank(&params("", true), true),
            "空查询 → 不融合（无词可嵌入）"
        );
    }

    /// 缓存键**不含** page/page_size（那正是要复用的维度）；查询/过滤变了才换键。
    #[test]
    fn semantic_cache_key_ignores_pagination_but_tracks_query() {
        let a = params("合同", true);
        let mut b = a.clone();
        b.page = 7;
        b.page_size = 50;
        assert_eq!(semantic_cache_key(&a), semantic_cache_key(&b), "翻页必须命中同一键");
        let mut c = a.clone();
        c.query = "物业".into();
        assert_ne!(semantic_cache_key(&a), semantic_cache_key(&c), "查询变了要换键");
        let mut d = a.clone();
        d.ext_filter = Some(vec!["pdf".into()]);
        assert_ne!(semantic_cache_key(&a), semantic_cache_key(&d), "过滤变了要换键");
    }

    /// 缓存读写：TTL 内命中、过期不命中、同 key 覆盖、超上限淘汰最旧。
    #[test]
    fn semantic_cache_respects_ttl_and_capacity() {
        let now = std::time::Instant::now();
        let ttl = std::time::Duration::from_secs(300);
        let mk = |key: &str, at: std::time::Instant| SemanticCacheEntry {
            key: key.to_string(),
            stored: at,
            fusion: SemanticFusion::default(),
        };
        let mut entries = vec![
            mk("a", now),
            mk("b", now - std::time::Duration::from_secs(301)),
        ];
        assert!(cache_lookup(&entries, "a", now, ttl).is_some(), "新鲜条目应命中");
        assert!(cache_lookup(&entries, "b", now, ttl).is_none(), "过期条目不命中");
        assert!(cache_lookup(&entries, "c", now, ttl).is_none(), "不存在的键不命中");

        // 同 key 覆盖 + 容量淘汰最旧
        cache_store(&mut entries, mk("a", now), 8);
        assert_eq!(
            entries.iter().filter(|e| e.key == "a").count(),
            1,
            "同 key 只保留一条"
        );
        for i in 0..12 {
            cache_store(&mut entries, mk(&format!("k{i}"), now), 8);
        }
        assert!(entries.len() <= 8, "容量上限：{}", entries.len());
        assert!(entries.iter().any(|e| e.key == "k11"), "新条目应在");
    }
}

#[cfg(test)]
mod snippet_enrich_tests {
    use super::{enrich_missing_snippets, SearchHit};
    use crate::db::{run_migrations, tracker};

    fn setup() -> (rusqlite::Connection, String) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        crate::db::dir_config::add_dir(&conn, "/docs", None, None, None, None, true).unwrap();
        let dir = crate::db::dir_config::list_dirs(&conn).unwrap().remove(0);
        (conn, dir.id)
    }

    fn fake_hit(file_id: &str, snippet: &str) -> SearchHit {
        SearchHit {
            file_id: file_id.to_string(),
            file_name: "a.docx".to_string(),
            file_ext: "docx".to_string(),
            path: "a.docx".to_string(),
            snippet: snippet.to_string(),
            score: 1.0,
            mtime: 0,
            file_size: 10,
            duplicate_count: 0,
            duplicate_paths: Vec::new(),
        }
    }

    #[test]
    fn fills_empty_snippets_and_strips_markdown() {
        let (conn, dir_id) = setup();
        let id = tracker::upsert_file(&conn, "a.docx", &dir_id, 1, 10, Some("md5a")).unwrap();
        tracker::store_content(&conn, "md5a", "**关于土地管理法**的实施意见正文", false, None).unwrap();

        let mut hits = vec![fake_hit(&id, "")];
        enrich_missing_snippets(&conn, "土地管理法", &mut hits);

        assert!(hits[0].snippet.contains("<em>"), "应高亮命中词: {}", hits[0].snippet);
        assert!(!hits[0].snippet.contains("**"), "片段不应带 Markdown 标记: {}", hits[0].snippet);
    }

    #[test]
    fn keeps_existing_snippets_untouched() {
        let (conn, dir_id) = setup();
        let id = tracker::upsert_file(&conn, "b.pdf", &dir_id, 1, 10, Some("md5b")).unwrap();
        tracker::store_content(&conn, "md5b", "物业费纠纷的判决书", false, None).unwrap();

        let mut hits = vec![fake_hit(&id, "已有片段")];
        enrich_missing_snippets(&conn, "物业费", &mut hits);
        assert_eq!(hits[0].snippet, "已有片段");
    }

    #[test]
    fn missing_content_or_file_leaves_snippet_empty() {
        let (conn, _dir_id) = setup();
        let mut hits = vec![fake_hit("no-such-file", "")];
        enrich_missing_snippets(&conn, "物业费", &mut hits);
        assert_eq!(hits[0].snippet, "");
    }
}

#[cfg(test)]
mod dir_path_resolve_tests {
    use super::resolve_dir_paths;

    fn setup_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn absolute_paths_map_to_relative_prefixes() {
        let conn = setup_conn();
        crate::db::dir_config::add_dir(&conn, "/home/docs", None, None, None, None, true)
            .unwrap();
        let d = crate::db::dir_config::list_dirs(&conn).unwrap().remove(0);
        crate::db::tracker::upsert_file(&conn, "report.pdf", &d.id, 1, 10, None).unwrap();
        crate::db::tracker::upsert_file(&conn, "sub/note.md", &d.id, 1, 10, None).unwrap();
        crate::db::tracker::upsert_file(&conn, "docs2/sibling.md", &d.id, 1, 10, None).unwrap();

        let ids = resolve_dir_paths(&conn, &["/home/docs".into()]).unwrap().unwrap();
        assert_eq!(ids.len(), 3);

        let ids = resolve_dir_paths(&conn, &["/home/docs/sub".into()]).unwrap().unwrap();
        assert_eq!(ids.len(), 1);
        let note = crate::db::tracker::get_file_by_path(&conn, "sub/note.md").unwrap().unwrap();
        assert!(ids.contains(&note.id));

        let ids = resolve_dir_paths(&conn, &["/elsewhere/x".into()]).unwrap().unwrap();
        assert!(ids.is_empty());

        assert!(resolve_dir_paths(&conn, &[]).unwrap().is_none());
    }
}
