use tauri::State;
use serde::Serialize;
use std::sync::atomic::Ordering;

use crate::db;
use crate::scanner::helpers::TempDir;
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
    if state.is_rebuilding.load(Ordering::SeqCst) {
        return Err("索引重建中，请稍后再试".to_string());
    }
    // Resolve dir_paths to file_ids via SQLite path prefix matching.
    let file_ids = if let Some(paths) = &dir_paths {
        if paths.is_empty() {
            None
        } else {
            let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
            let likes: Vec<String> = paths.iter().map(|p| {
                let escaped = p.replace('%', "\\%").replace('_', "\\_");
                format!("{}%", escaped)
            }).collect();
            let sql = format!(
                "SELECT id FROM file_tracking WHERE status = 'active' AND ({})",
                std::iter::repeat("path LIKE ? ESCAPE '\\'")
                    .take(likes.len())
                    .collect::<Vec<_>>()
                    .join(" OR ")
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| format!("db prepare error: {e}"))?;
            let params: Vec<&dyn rusqlite::types::ToSql> = likes.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))
                .map_err(|e| format!("db query error: {e}"))?;
            let ids: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            if ids.is_empty() {
                return Ok(SearchResponse {
                    total: 0,
                    page: page.unwrap_or(1),
                    page_size: page_size.unwrap_or(20),
                    took_ms: 0,
                    hits: Vec::new(),
                });
            }
            Some(ids)
        }
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
        sort: sort.unwrap_or_default(),
        sort_order: sort_order.unwrap_or_else(|| "desc".to_string()),
        page: page.unwrap_or(1),
        page_size: page_size.unwrap_or(20).min(max_results).min(1000),
        fuzzy: fuzzy.unwrap_or(false),
        semantic: semantic.unwrap_or(false),
    };

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

    let response = searcher
        .search(&params)
        .map_err(|e| format!("search failed: {e}"))?;

    // Semantic rerank (optional): when enabled and the AI gateway is
    // configured, fuse BM25 hits with embedding-cosine top-N via RRF so
    // meaning-matches surface alongside keyword matches. Runs on a
    // blocking thread with a short timeout so a hung embedding gateway
    // degrades to BM25-only rather than freezing the search.
    let mut response = response;
    if params.semantic && crate::ai::embedding_enabled() && !params.query.is_empty() {
        let db = state.db.clone();
        match semantic_rerank_worker(&db, &params.query, &response, params.page, params.page_size) {
            Ok(merged) => response = merged,
            Err(e) => log::warn!("[AI] semantic rerank skipped: {e}"),
        }
    }

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

/// Semantically rerank search hits: embed the query, score every stored
/// embedding by cosine, then fuse with the BM25 ranks via Reciprocal Rank
/// Fusion. Returns a merged `SearchResponse` with reordered hits.
fn semantic_rerank_worker(
    pool: &r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>,
    query: &str,
    bm25: &SearchResponse,
    page: usize,
    page_size: usize,
) -> Result<SearchResponse, String> {
    use std::collections::HashMap;

    let q_vec = {
        let (tx, rx) = std::sync::mpsc::channel();
        let q = query.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(crate::ai::embed(&q));
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
        return Ok(bm25.clone());
    }

    // Semantic cosine scores: file_id -> score.
    let mut scored: Vec<(String, f32)> = rows
        .iter()
        .map(|(fid, vec)| (fid.clone(), crate::ai::cosine(&q_vec, vec)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cos_map: std::collections::HashMap<String, f32> = scored.iter().cloned().collect();

    // RRF fusion: score = 1/(60 + rank) summed over BM25 and semantic lists.
    const K: f64 = 60.0;
    let mut fusion: HashMap<String, f64> = HashMap::new();
    for (i, hit) in bm25.hits.iter().enumerate() {
        *fusion.entry(hit.file_id.clone()).or_insert(0.0) += 1.0 / (K + i as f64);
    }
    for (rank, (fid, _)) in scored.iter().enumerate() {
        *fusion.entry(fid.clone()).or_insert(0.0) += 1.0 / (K + rank as f64);
    }

    let mut ordered: Vec<(String, f64)> = fusion.into_iter().collect();
    ordered.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Rebuild hit list in fused order. The semantic side may surface docs
    // that BM25 never matched — those must be materialised from the DB
    // (file_name/path/mtime/size) instead of being silently dropped, or a
    // pure-semantic query (different wording, same meaning) returns nothing.
    let mut by_id: HashMap<String, SearchHit> = bm25
        .hits
        .iter()
        .cloned()
        .map(|h| (h.file_id.clone(), h))
        .collect();

    // Fetch metadata for semantic-only ids (top fused order, bounded).
    let conn = pool.get().map_err(|e| format!("db error: {e}"))?;
    for (fid, _fusion) in ordered.iter().take(100) {
        if by_id.contains_key(fid) {
            continue;
        }
        if let Ok(Some(rec)) = crate::db::tracker::get_file_by_id(&conn, fid) {
            let file_name = std::path::Path::new(&rec.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| rec.path.clone());
            // Semantic score (cosine similarity) — visible so the user can
            // tell how close the match is; 0 means "matched via keywords".
            let cos = cos_map.get(fid).copied().unwrap_or(0.0);
            // Build a snippet around the query's first matching word so the
            // user sees *why* this document matched (highlighted). When no
            // word overlap exists (pure semantic hit), prefix the similarity
            // percentage so the user can judge relevance.
            let raw_snippet = match rec.md5.as_deref() {
                Some(md5) => crate::db::tracker::get_content(&conn, md5)
                    .ok()
                    .flatten()
                    .map(|text| semantic_snippet(&text, query))
                    .unwrap_or_default(),
                None => String::new(),
            };
            let snippet = if raw_snippet.contains("<em>") {
                raw_snippet
            } else if cos > 0.0 {
                format!(
                    "[语义相似度 {}%] {}",
                    (cos * 100.0) as u32,
                    raw_snippet,
                )
            } else {
                raw_snippet
            };
            by_id.insert(
                fid.clone(),
                SearchHit {
                    file_id: fid.clone(),
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
                },
            );
        }
    }
    drop(conn);

    let mut merged: Vec<SearchHit> = ordered
        .iter()
        .filter_map(|(id, _)| by_id.remove(id))
        .collect();
    merged.extend(by_id.into_values());

    let start = page.saturating_sub(1) * page_size;
    let page_hits: Vec<SearchHit> = merged.into_iter().skip(start).take(page_size).collect();

    Ok(SearchResponse {
        total: bm25.total,
        page: page,
        page_size: page_size,
        took_ms: bm25.took_ms,
        hits: page_hits,
    })
}

/// Build a human-readable snippet around the region where the document
/// best overlaps the query, so a semantic-only hit shows *why* it matched.
/// Uses character-level prefix matching (fast, no document tokenisation)
/// and limits the captured word to avoid whole-paragraph highlighting.
/// Falls back to the document opening when nothing overlaps.
fn semantic_snippet(content: &str, query: &str) -> String {
    let jieba = &crate::search::schema::JIEBA;
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
            let start = content.char_indices()
                .filter(|&(i,_)| i <= pos)
                .last()
                .map(|(i,_)| i)
                .unwrap_or(pos);
            // Expand forward up to MAX_WORD chars but re-align to char boundary.
            let end = content[start..]
                .chars()
                .take(MAX_WORD)
                .fold(start, |acc, c| acc + c.len_utf8());
            let end = end.min(content.len());
            let captured = content[start..end].to_string();
            if best.as_ref().map_or(true, |(_, prev)| captured.len() > prev.len()) {
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
fn snippet_around<'a>(content: &'a str, needle: &str, wrap: impl Fn(&str) -> String) -> String {
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

#[tauri::command]
pub async fn get_search_history(state: State<'_, AppState>) -> Result<Vec<HistoryEntry>, String> {
    let conn = state
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
    let conn = state
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
    // Resolve dir_paths to file_ids via SQLite path prefix matching.
    let file_ids = if let Some(paths) = &dir_paths {
        if paths.is_empty() {
            None
        } else {
            let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
            let likes: Vec<String> = paths.iter().map(|p| {
                let escaped = p.replace('%', "\\%").replace('_', "\\_");
                format!("{}%", escaped)
            }).collect();
            let sql = format!(
                "SELECT id FROM file_tracking WHERE status = 'active' AND ({})",
                std::iter::repeat("path LIKE ? ESCAPE '\\'")
                    .take(likes.len())
                    .collect::<Vec<_>>()
                    .join(" OR ")
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| format!("db prepare error: {e}"))?;
            let params: Vec<&dyn rusqlite::types::ToSql> = likes.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))
                .map_err(|e| format!("db query error: {e}"))?;
            let ids: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            if ids.is_empty() && !paths.is_empty() {
                return Ok(String::new());
            }
            Some(ids)
        }
    } else {
        None
    };

    let dir_ids = dir_ids.unwrap_or_default();
    let ext_filter = ext_filter.unwrap_or_default();
    let dir_ids_opt = if dir_ids.is_empty() { None } else { Some(dir_ids) };
    let ext_filter_opt = if ext_filter.is_empty() { None } else { Some(ext_filter) };

    let export_page_size = {
        let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
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
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: 1,
        page_size: export_page_size,
        fuzzy: false,
        semantic: false,
    };

    let mgr = state
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

    let tmp_dir = TempDir::new("ls_export").map_err(|e| format!("failed to create temp dir: {e}"))?;
    let tmp_path = tmp_dir.path().join(format!("export.{}", format));
    let mut file = std::fs::File::create(&tmp_path).map_err(|e| format!("failed to create export file: {e}"))?;
    use std::io::Write;

    match format.as_str() {
        "csv" => {
            writeln!(file, "file_name,file_ext,path,score,mtime,file_size").map_err(|e| format!("write error: {e}"))?;
            for hit in &response.hits {
                writeln!(file, "\"{}\",\"{}\",\"{}\",{},{},{}",
                    hit.file_name.replace('"', "\"\""),
                    hit.file_ext.replace('"', "\"\""),
                    hit.path.replace('"', "\"\""),
                    hit.score,
                    hit.mtime,
                    hit.file_size,
                ).map_err(|e| format!("write error: {e}"))?;
            }
        }
        _ => {
            for hit in &response.hits {
                writeln!(file, "{} ({}): {}", hit.file_name, hit.file_ext, hit.snippet).map_err(|e| format!("write error: {e}"))?;
            }
        }
    };
    drop(searcher);
    drop(mgr);
    Ok(tmp_path.to_string_lossy().to_string())
}
#[tauri::command]
pub async fn get_file_type_stats(state: State<'_, AppState>) -> Result<Vec<FileTypeStat>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
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
    let supported = crate::extractor::get_supported_extensions();
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
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


#[cfg(test)]
mod snippet_tests {

    use super::{head_snippet, semantic_snippet};

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
}
