use tauri::State;
use serde::Serialize;
use std::sync::atomic::Ordering;

use crate::db;
use crate::scanner::helpers::TempDir;
use crate::search::searcher::{SearchParams, SearchResponse, SortField, SearcherWrap};
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
        query,
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

    let params = SearchParams {
        query,
        dir_ids: dir_ids_opt,
        file_ids,
        ext_filter: ext_filter_opt,
        date_from: None,
        date_to: None,
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: 1,
        page_size: 10000,
        fuzzy: false,
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

    let output = match format.as_str() {
        "csv" => {
            let mut csv = String::from("file_name,file_ext,path,score,mtime,file_size\n");
            for hit in &response.hits {
                csv.push_str(&format!(
                    "\"{}\",\"{}\",\"{}\",{},{},{}\n",
                    hit.file_name.replace('"', "\"\""),
                    hit.file_ext.replace('"', "\"\""),
                    hit.path.replace('"', "\"\""),
                    hit.score,
                    hit.mtime,
                    hit.file_size,
                ));
            }
            csv
        }
        _ => {
            let mut txt = String::new();
            for hit in &response.hits {
                txt.push_str(&format!("{} ({}): {}\n", hit.file_name, hit.file_ext, hit.snippet));
            }
            txt
        }
    };

    drop(searcher);
    drop(mgr);

    let tmp_dir = TempDir::new("ls_export").map_err(|e| format!("failed to create temp dir: {e}"))?;
    let tmp_path = tmp_dir.path().join(format!("export.{}", format));
    std::fs::write(&tmp_path, &output).map_err(|e| format!("failed to write export: {e}"))?;
    Ok(tmp_path.to_string_lossy().to_string())
}
#[tauri::command]
pub async fn get_file_type_stats(state: State<'_, AppState>) -> Result<Vec<FileTypeStat>, String> {
    let conn = state.db.get().map_err(|e| format!("db error: {e}"))?;
    let sql = "SELECT file_ext, COUNT(*) as cnt FROM file_tracking WHERE status='active' GROUP BY file_ext ORDER BY cnt DESC";
    let mut stmt = conn.prepare(sql).map_err(|e| format!("db prepare error: {e}"))?;
    let mut results = Vec::new();
    let rows = stmt.query_map([], |row| {
        let ext: String = row.get("file_ext")?;
        let cnt: i64 = row.get("cnt")?;
        let name = file_type_name(&ext);
        Ok(FileTypeStat {
            extension: ext,
            name,
            count: cnt as u64,
        })
    }).map_err(|e| format!("db query error: {e}"))?;
    for row in rows {
        let row = row.map_err(|e| format!("db query error: {e}"))?;
        results.push(row);
    }
    Ok(results)
}

