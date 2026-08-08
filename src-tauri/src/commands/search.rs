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
    // meaning-matches surface alongside keyword matches.
    let mut response = response;
    if params.semantic && crate::ai::embedding_enabled() && !params.query.is_empty() {
        match semantic_rerank(&state, &params, &response) {
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
fn semantic_rerank(state: &State<'_, AppState>, params: &SearchParams, bm25: &SearchResponse) -> Result<SearchResponse, String> {
    use std::collections::HashMap;

    let q_vec = crate::ai::embed(&params.query).ok_or_else(|| "query embedding failed".to_string())?;

    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
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

    // Rebuild hit list in fused order; BM25-only hits (no embedding) go last.
    let mut by_id: HashMap<String, SearchHit> = bm25
        .hits
        .iter()
        .cloned()
        .map(|h| (h.file_id.clone(), h))
        .collect();
    let mut merged: Vec<SearchHit> = ordered
        .iter()
        .filter_map(|(id, _)| by_id.remove(id))
        .collect();
    merged.extend(by_id.into_values());

    let start = params.page.saturating_sub(1) * params.page_size;
    let page_hits: Vec<SearchHit> = merged.into_iter().skip(start).take(params.page_size).collect();

    Ok(SearchResponse {
        total: bm25.total,
        page: params.page,
        page_size: params.page_size,
        took_ms: bm25.took_ms,
        hits: page_hits,
    })
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
    let entries = db::search_history::list_recent(&conn, 100)
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

