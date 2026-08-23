use std::sync::atomic::Ordering;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tauri::Manager;

use crate::db::tracker;
use crate::search::searcher::{SearchParams, SortField, SearcherWrap};
use crate::state::AppState;
use crate::webapi::auth;
use crate::webapi::state::ApiState;

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    pub dir_ids: Option<Vec<String>>,
    pub ext_filter: Option<Vec<String>>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    pub fuzzy: Option<bool>,
    pub semantic: Option<bool>,
}

#[derive(Deserialize)]
pub struct FilesQuery {
    pub filter: Option<String>,
    pub ext: Option<String>,
    pub search: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

#[derive(Serialize)]
pub struct ApiError {
    pub error: String,
}

fn default_page() -> usize { 1 }
fn default_page_size() -> usize { 20 }

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, Json(self)).into_response()
    }
}

async fn search_handler(
    State(state): State<ApiState>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let mgr = app_state.index_manager.read().map_err(|e| ApiError { error: e.to_string() })?;
    let reader = mgr.reader().map_err(|e| ApiError { error: e.to_string() })?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);

    let search_params = SearchParams {
        query: crate::search::schema::split_query_terms(&params.q.to_lowercase()),
        dir_ids: params.dir_ids,
        file_ids: None,
        ext_filter: params.ext_filter,
        path_prefixes: None,
        date_from: params.date_from,
        date_to: params.date_to,
        sort: SortField::Score,
        sort_order: "desc".to_string(),
        page: params.page,
        page_size: params.page_size,
        fuzzy: params.fuzzy.unwrap_or(false),
        semantic: params.semantic.unwrap_or(false),
    };

    let result = searcher.search(&search_params).map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({
        "hits": result.hits,
        "total": result.total,
        "page": result.page,
        "page_size": result.page_size,
        "took_ms": result.took_ms,
    })))
}

async fn suggest_handler(
    State(state): State<ApiState>,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<Vec<String>>, ApiError> {
    let prefix = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
    let app_state = state.app_handle.state::<AppState>();
    let mgr = app_state.index_manager.read().map_err(|e| ApiError { error: e.to_string() })?;
    let reader = mgr.reader().map_err(|e| ApiError { error: e.to_string() })?;
    let searcher = SearcherWrap::new(reader.clone(), mgr.index().as_ref().clone());
    drop(mgr);
    let suggestions = searcher.suggest(prefix, 10).map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(suggestions))
}

async fn files_handler(
    State(state): State<ApiState>,
    Query(params): Query<FilesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;

    let ps = params.page_size.unwrap_or(50).max(1).min(1000);
    let p = params.page.unwrap_or(1).max(1);
    let offset = (p - 1) * ps;

    let mut wheres: Vec<&str> = vec!["status = 'active'"];
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql + Send>> = Vec::new();

    match params.filter.as_deref() {
        Some("indexed") => { wheres.push("indexed = 1"); }
        Some("pending") => { wheres.push("indexed IN (0, 3)"); }
        Some("failed") => { wheres.push("indexed = 2"); }
        _ => {}
    }
    if let Some(e) = &params.ext {
        wheres.push("path LIKE ?");
        sql_params.push(Box::new(format!("%.{e}")));
    }
    if let Some(s) = &params.search {
        wheres.push("path LIKE ?");
        sql_params.push(Box::new(format!("%{s}%")));
    }

    let where_clause = wheres.join(" AND ");
    let total: u64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM file_tracking WHERE {where_clause}"),
            rusqlite::params_from_iter(sql_params.iter().map(|p| p as &dyn rusqlite::ToSql)),
            |row| row.get(0),
        )
        .map_err(|e| ApiError { error: e.to_string() })?;

    let sort_col = match params.sort.as_deref().unwrap_or("path") {
        "ext" => "file_ext",
        "size" => "size",
        "mtime" => "mtime",
        _ => "path",
    };
    let order_dir = if params.order.as_deref() == Some("desc") { "DESC" } else { "ASC" };

    let data_sql = format!(
        "SELECT id, path, size, mtime, indexed FROM file_tracking WHERE {where_clause} ORDER BY {sort_col} {order_dir} LIMIT ?{} OFFSET ?{}",
        sql_params.len() + 1,
        sql_params.len() + 2,
    );
    let mut data_params: Vec<Box<dyn rusqlite::ToSql + Send>> = sql_params;
    data_params.push(Box::new(ps as i64));
    data_params.push(Box::new(offset as i64));

    let mut stmt = conn.prepare(&data_sql).map_err(|e| ApiError { error: e.to_string() })?;
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(data_params.iter().map(|p| p as &dyn rusqlite::ToSql)),
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "path": row.get::<_, String>(1)?,
                    "size": row.get::<_, u64>(2)?,
                    "mtime": row.get::<_, i64>(3)?,
                    "indexed": row.get::<_, i64>(4)?,
                }))
            },
        )
        .map_err(|e| ApiError { error: e.to_string() })?;
    let files: Vec<serde_json::Value> = rows.filter_map(|r| r.ok()).collect();

    Ok(Json(serde_json::json!({
        "files": files,
        "total": total,
        "page": p,
        "page_size": ps,
    })))
}

async fn file_preview_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let rec = tracker::get_file_by_id(&conn, &id)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "file not found".into() })?;
    let text = rec.md5.as_ref()
        .and_then(|md5| tracker::get_content(&conn, md5).ok()?)
        .unwrap_or_default();
    Ok(Json(serde_json::json!({
        "id": rec.id,
        "path": rec.path,
        "size": rec.size,
        "mtime": rec.mtime,
        "content": text.chars().take(50000).collect::<String>(),
    })))
}

async fn index_status_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let total: u64 = conn.query_row("SELECT COUNT(*) FROM file_tracking WHERE status = 'active'", [], |r| r.get(0)).map_err(|e| ApiError { error: e.to_string() })?;
    let indexed: u64 = conn.query_row("SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND indexed = 1", [], |r| r.get(0)).map_err(|e| ApiError { error: e.to_string() })?;
    let pending: u64 = conn.query_row("SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND indexed IN (0, 3)", [], |r| r.get(0)).map_err(|e| ApiError { error: e.to_string() })?;
    let failed: u64 = conn.query_row("SELECT COUNT(*) FROM file_tracking WHERE status = 'active' AND indexed = 2", [], |r| r.get(0)).map_err(|e| ApiError { error: e.to_string() })?;

    let mut stmt = conn.prepare("SELECT id, path, error_msg FROM file_tracking WHERE status = 'active' AND indexed = 2 ORDER BY updated_at DESC LIMIT 10").map_err(|e| ApiError { error: e.to_string() })?;
    let errors: Vec<serde_json::Value> = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "path": row.get::<_, String>(1)?,
            "error": row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        }))
    }).map_err(|e| ApiError { error: e.to_string() })?.filter_map(|r| r.ok()).collect();

    let is_scanning = app_state.is_scanning.load(Ordering::Relaxed);
    let is_rebuilding = app_state.is_rebuilding.load(Ordering::Relaxed);

    Ok(Json(serde_json::json!({
        "total": total,
        "indexed": indexed,
        "pending": pending,
        "failed": failed,
        "is_scanning": is_scanning,
        "is_rebuilding": is_rebuilding,
        "recent_errors": errors,
    })))
}

async fn index_health_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let db_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_tracking WHERE status = 'active'", [], |r| r.get(0))
        .map_err(|e| ApiError { error: e.to_string() })?;
    let mgr = app_state.index_manager.read().map_err(|e| ApiError { error: e.to_string() })?;
    let reader = mgr.reader().map_err(|e| ApiError { error: e.to_string() })?;
    let index_count = reader.searcher().num_docs() as i64;
    Ok(Json(serde_json::json!({
        "db_doc_count": db_count,
        "index_doc_count": index_count,
        "healthy": (db_count - index_count).abs() < 100,
    })))
}

async fn dirs_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let dirs = crate::db::dir_config::list_dirs(&conn)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({ "dirs": dirs })))
}

async fn ai_capabilities_handler(
    State(_state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let caps = crate::ai::AiCapabilities::from_gateways(crate::ai::capabilities());
    Ok(Json(serde_json::json!({
        "embedding": caps.embedding,
        "llm": caps.llm,
    })))
}

async fn version_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "hash": env!("GIT_VERSION"),
        "time": env!("GIT_COMMIT_TIME"),
    }))
}

async fn settings_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let mut stmt = conn.prepare("SELECT key, value FROM app_settings")
        .map_err(|e| ApiError { error: e.to_string() })?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).map_err(|e| ApiError { error: e.to_string() })?;
    let mut map = serde_json::Map::new();
    for row in rows {
        let (k, v) = row.map_err(|e| ApiError { error: e.to_string() })?;
        map.insert(k, serde_json::Value::String(v));
    }
    Ok(Json(serde_json::Value::Object(map)))
}

async fn trigger_scan_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let dirs = crate::db::dir_config::list_dirs(&conn)
        .map_err(|e| ApiError { error: e.to_string() })?;
    drop(conn);
    if dirs.is_empty() {
        return Err(ApiError { error: "no directories configured".into() });
    }
    let app_handle = state.app_handle.clone();
    let scanner = app_state.scanner.clone();
    tokio::task::spawn_blocking(move || {
        for dir in &dirs {
            let _ = scanner.full_scan(&dir.id, |_| {});
        }
        let _ = app_handle.emit("scan-completed", serde_json::json!({}));
    });
    Ok(Json(serde_json::json!({ "status": "started" })))
}

async fn cancel_scan_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    app_state.cancel_scan.store(true, Ordering::Relaxed);
    crate::ai::cancel_ai();
    Ok(Json(serde_json::json!({ "status": "cancelled" })))
}

async fn reindex_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let file_id = body.get("file_id").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError { error: "missing file_id".into() })?;
    let app_state = state.app_handle.state::<AppState>();
    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let rec = tracker::get_file_by_id(&conn, file_id)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "file not found".into() })?;
    let dir = crate::db::dir_config::get_dir(&conn, &rec.dir_id)
        .map_err(|e| ApiError { error: e.to_string() })?
        .ok_or_else(|| ApiError { error: "dir not found".into() })?;
    if let Some(ref md5) = rec.md5 {
        let _ = tracker::delete_content(&conn, md5);
    }
    let full_path = std::path::Path::new(&dir.path).join(&rec.path);
    drop(conn);
    app_state.indexer.index_file(file_id, &full_path, &rec.dir_id)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({ "status": "reindexed" })))
}

async fn chat_sessions_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let h = crate::commands::ai::read_history(&app_state.data_dir);
    let mut sessions: Vec<_> = h.sessions;
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let metas: Vec<serde_json::Value> = sessions.iter().map(|s| serde_json::json!({
        "id": s.id,
        "title": s.title,
        "updated_at": s.updated_at,
    })).collect();
    Ok(Json(serde_json::json!({ "sessions": metas })))
}

async fn chat_session_create_handler(
    State(state): State<ApiState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let id = crate::commands::ai::create_chat_session_impl(&app_state.data_dir)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({ "id": id })))
}

async fn chat_session_load_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let h = crate::commands::ai::read_history(&app_state.data_dir);
    let session = h.sessions.into_iter().find(|s| s.id == id);
    match session {
        Some(s) => Ok(Json(serde_json::to_value(&s).unwrap_or_default())),
        None => Err(ApiError { error: "session not found".into() }),
    }
}

async fn chat_session_delete_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let mut h = crate::commands::ai::read_history(&app_state.data_dir);
    h.sessions.retain(|s| s.id != id);
    crate::commands::ai::write_history(&app_state.data_dir, &h)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

async fn chat_session_export_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app_state = state.app_handle.state::<AppState>();
    let md = crate::commands::ai::export_chat_session_impl(&app_state.data_dir, &id)
        .map_err(|e| ApiError { error: e.to_string() })?;
    let json = crate::commands::ai::export_chat_session_json_impl(&app_state.data_dir, &id)
        .map_err(|e| ApiError { error: e.to_string() })?;
    Ok(Json(serde_json::json!({ "markdown": md, "json": json })))
}

async fn chat_ask_handler(
    State(state): State<ApiState>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let question = body.get("question").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError { error: "missing question".into() })?
        .to_string();
    if !crate::ai::llm_enabled() {
        return Err(ApiError { error: "AI service not configured".into() });
    }
    let app_state = state.app_handle.state::<AppState>();
    let hits = crate::commands::ai::bm25_relevant_hits(
        &app_state, &question.to_lowercase(), 3,
        crate::ai::embedding_enabled(),
        None, None, None, None, None, None,
    ).map_err(|e| ApiError { error: e.to_string() })?;

    let conn = app_state.db.get().map_err(|e| ApiError { error: e.to_string() })?;
    let mut docs: Vec<String> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    for hit in &hits {
        if let Ok(Some(rec)) = tracker::get_file_by_id(&conn, &hit.file_id) {
            if let Some(md5) = &rec.md5 {
                if let Ok(Some(text)) = tracker::get_content(&conn, md5) {
                    if !text.trim().is_empty() {
                        docs.push(format!("【{}】\n{}", rec.path, crate::commands::ai::truncate_text(&text, 2000)));
                        sources.push(rec.path.clone());
                    }
                }
            }
        }
    }
    drop(conn);

    if docs.is_empty() {
        return Err(ApiError { error: "no relevant documents found".into() });
    }

    let context = crate::commands::ai::truncate_text(&docs.join("\n\n---\n\n"), 50000);
    let system = format!(
        "你是严谨的文档分析助手。仅基于以下材料回答，不臆造事实。\n\n材料：\n{}",
        context
    );
    let answer = tokio::task::spawn_blocking(move || crate::ai::chat(&system, &question))
        .await
        .map_err(|e| ApiError { error: format!("task failed: {e}") })?
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "answer": answer,
        "sources": sources,
    })))
}

/// Self-contained search page served at `/` — no external JS, no build step.
const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Link-Searcher</title>
<style>
*{margin:0;padding:0;box-sizing:border-box}
body{font-family:-apple-system,"Segoe UI",Roboto,sans-serif;background:#f5f5f5;color:#1a1a1a;padding:2rem}
.container{max-width:720px;margin:0 auto}
h1{font-size:1.4rem;margin-bottom:1rem;color:#2563eb}
.token-row{display:flex;gap:.5rem;margin-bottom:1rem}
input{padding:.6rem .8rem;border:1px solid #d0d0d0;border-radius:.4rem;font-size:.95rem;flex:1}
input:focus{outline:none;border-color:#2563eb}
.search-row{display:flex;gap:.5rem;margin-bottom:1.2rem}
button{padding:.6rem 1.2rem;border:none;border-radius:.4rem;background:#2563eb;color:#fff;cursor:pointer;font-size:.95rem}
button:hover{background:#1d4ed8}
.status{font-size:.85rem;color:#666;margin-bottom:1rem}
.results{display:flex;flex-direction:column;gap:.8rem}
.hit{background:#fff;padding:1rem;border-radius:.5rem;box-shadow:0 1px 3px rgba(0,0,0,.08)}
.hit .path{color:#2563eb;font-size:.9rem;word-break:break-all}
.hit .name{font-weight:600;font-size:1rem}
.hit .meta{color:#888;font-size:.8rem;margin-top:.3rem}
.error{background:#fee2e2;color:#991b1b;padding:.8rem;border-radius:.4rem;font-size:.9rem}
</style>
</head>
<body>
<div class="container">
<h1>&#128269; Link-Searcher</h1>
<div class="token-row"><input id="token" placeholder="Bearer Token" type="password"><button onclick="saveToken()">&#128190;</button></div>
<div class="search-row"><input id="q" placeholder="搜索文档..." onkeydown="if(event.key==='Enter')doSearch()"><button onclick="doSearch()">搜索</button></div>
<div class="status" id="status"></div>
<div class="results" id="results"></div>
</div>
<script>
let token=localStorage.getItem('ls_token')||'';
document.getElementById('token').value=token;
function saveToken(){token=document.getElementById('token').value.trim();localStorage.setItem('ls_token',token);loadStatus()}
async function api(path){
  const r=await fetch(path,{headers:{Authorization:'Bearer '+token}});
  if(!r.ok){throw new Error('HTTP '+r.status+(r.status===401?' Token 无效':''))}
  return r.json();
}
async function doSearch(){
  const q=document.getElementById('q').value.trim();if(!q)return;
  const el=document.getElementById('results');el.innerHTML='<p class="status">搜索中...</p>';
  try{const d=await api('/api/search?q='+encodeURIComponent(q)+'&page_size=20');
    document.getElementById('status').textContent=d.total+' 条结果，耗时 '+d.took_ms+'ms';
    if(!d.hits.length){el.innerHTML='<p class="status">未找到结果</p>';return}
    el.innerHTML=d.hits.map(h=>'<div class="hit"><span class="path">'+esc(h.path)+'</span><br><span class="name">'+esc(h.file_name||'')+'</span><div class="meta">'+fmtSize(h.file_size)+' · '+new Date(h.mtime/1000).toLocaleDateString()+' · 相关度 '+h.score.toFixed(1)+'</div></div>').join('');
  }catch(e){el.innerHTML='';document.getElementById('status').innerHTML='<span class="error">'+esc(e.message)+'</span>'}
}
async function loadStatus(){
  try{const s=await api('/api/index/status');
    document.getElementById('status').textContent='已索引 '+s.indexed+'/'+s.total+' · 失败 '+s.failed;
  }catch(e){document.getElementById('status').innerHTML='<span class="error">'+esc(e.message)+'</span>'}
}
function esc(s){return (s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;')}
function fmtSize(n){return n>=1048576?(n/1048576).toFixed(1)+' MB':(n/1024).toFixed(0)+' KB'}
loadStatus();
</script>
</body></html>"#;

async fn index_handler() -> impl IntoResponse {
    axum::response::Html(INDEX_HTML)
}

pub fn build_router(state: ApiState) -> Router {
    let index_page = Router::new()
        .route("/", get(index_handler))
        .route("/index.html", get(index_handler));

    let read_routes = Router::new()
        .route("/api/search", get(search_handler))
        .route("/api/suggest", get(suggest_handler))
        .route("/api/files", get(files_handler))
        .route("/api/files/{id}/preview", get(file_preview_handler))
        .route("/api/index/status", get(index_status_handler))
        .route("/api/index/health", get(index_health_handler))
        .route("/api/dirs", get(dirs_handler))
        .route("/api/ai/capabilities", get(ai_capabilities_handler))
        .route("/api/version", get(version_handler))
        .route("/api/settings", get(settings_handler));

    let write_routes = Router::new()
        .route("/api/scan/trigger", post(trigger_scan_handler))
        .route("/api/scan/cancel", post(cancel_scan_handler))
        .route("/api/reindex", post(reindex_handler))
        .route("/api/chat/ask", post(chat_ask_handler));

    let chat_routes = Router::new()
        .route("/api/chat/sessions", get(chat_sessions_handler))
        .route("/api/chat/sessions", post(chat_session_create_handler))
        .route("/api/chat/sessions/{id}", get(chat_session_load_handler))
        .route("/api/chat/sessions/{id}", delete(chat_session_delete_handler))
        .route("/api/chat/sessions/{id}/export", post(chat_session_export_handler));

    index_page
        .merge(
            read_routes
                .merge(write_routes)
                .merge(chat_routes)
                .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth::bearer_auth))
        )
        .with_state(state)
}
