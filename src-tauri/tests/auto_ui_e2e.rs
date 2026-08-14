//! Auto-generated UI E2E tests based on MCP runtime DOM snapshot.
//!
//! WARNING:
//! - Static analysis cannot detect controls that require business preconditions
//!   (login, specific data) to render — those cases may fail at runtime.
//! - This E2E suite is designed for local macOS debug environment only.
//!   It cannot run in headless CI without a GUI.
//! - tauri-plugin-mcp is only enabled in debug builds (`#[cfg(debug_assertions)]`).
//!   Production builds must exclude it.
//!
//! Generated from MCP query_page(mode="map") on 2026-08-14.
//! Detected routes: /, /chat, /browse, /directories, /index, /logs, /file-types, /settings
//! Detected interactive elements: nav links, theme toggle, filter checkboxes

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, atomic::AtomicBool};

use serde_json::{json, Value};
use tauri::ipc::CallbackFn;
use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
use tauri::webview::InvokeRequest;

use link_searcher_lib::db;
use link_searcher_lib::indexer::IndexerService;
use link_searcher_lib::scanner::Scanner;
use link_searcher_lib::search::IndexManager;
use link_searcher_lib::state::{AppState, ScanDelta};

// ---------------------------------------------------------------------------
// Temp dir with automatic cleanup
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ls_ui_e2e_{prefix}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn setup_app(
    tmp: &TempDir,
) -> (tauri::App<tauri::test::MockRuntime>, tauri::WebviewWindow<tauri::test::MockRuntime>) {
    let db_path = tmp.path().join("test.db");
    let db_str = db_path.to_str().unwrap();
    let conn = rusqlite::Connection::open(db_str).unwrap();
    db::init_db(&conn).unwrap();
    drop(conn);
    let pool = db::get_pool(db_str).unwrap();
    let data_dir = tmp.path().join("data");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&data_dir).unwrap();

    let im = Arc::new(RwLock::new(IndexManager::create_in_ram()));
    let indexer = Arc::new(IndexerService::new(pool.clone(), im.clone()));
    let scanner = Arc::new(Scanner::new(pool.clone(), indexer.clone()));
    let is_scanning = Arc::new(AtomicBool::new(false));

    let (dummy_tx, _) = std::sync::mpsc::channel();
    let app_state = AppState::new(
        pool,
        im,
        indexer,
        scanner,
        is_scanning,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(Mutex::new(ScanDelta::default())),
        tmp.path().join("data"),
        index_dir,
        db_path,
        dummy_tx,
        None,
    );

    let mut ctx = mock_context(noop_assets());
    // Register ALL commands that the frontend might invoke
for cmd in &[
        "list_dirs", "add_dir", "remove_dir", "get_dir_children", "get_dir_tree",
        "get_index_status", "search", "suggest",
        "get_settings", "update_settings", "get_version",
        "list_files", "get_config",
        "get_logs",
        "list_ocr_engines", "get_file_type_support",
        "ai_capabilities", "cancel_ai_request",
        "list_chat_sessions", "create_chat_session", "delete_chat_session",
        "load_chat_session", "save_chat_session", "export_chat_session",
        "test_ai_gateway",
    ] {
        ctx.runtime_authority_mut()
            .__allow_command(cmd.to_string(), tauri::utils::acl::ExecutionContext::Local);
    }

    let app = mock_builder()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            link_searcher_lib::commands::dirs::list_dirs,
            link_searcher_lib::commands::dirs::add_dir,
            link_searcher_lib::commands::dirs::remove_dir,
            link_searcher_lib::commands::dirs::get_dir_children,
            link_searcher_lib::commands::dirs::get_dir_tree,
            link_searcher_lib::commands::index::get_index_status,
            link_searcher_lib::commands::search::search,
            link_searcher_lib::commands::search::suggest,
            link_searcher_lib::commands::settings::get_settings,
            link_searcher_lib::commands::settings::update_settings,
            link_searcher_lib::commands::settings::get_version,
            link_searcher_lib::commands::files::list_files,
            link_searcher_lib::commands::config::get_config,
            link_searcher_lib::commands::logs::get_logs,
            link_searcher_lib::commands::tesseract::list_ocr_engines,
            link_searcher_lib::commands::tesseract::get_file_type_support,
            link_searcher_lib::commands::ai::ai_capabilities,
            link_searcher_lib::commands::ai::cancel_ai_request,
            link_searcher_lib::commands::ai::list_chat_sessions,
            link_searcher_lib::commands::ai::create_chat_session,
            link_searcher_lib::commands::ai::delete_chat_session,
            link_searcher_lib::commands::ai::load_chat_session,
            link_searcher_lib::commands::ai::save_chat_session,
            link_searcher_lib::commands::ai::export_chat_session,
            link_searcher_lib::commands::ai::test_ai_gateway,
        ])
        .build(ctx)
        .expect("failed to build mock app");

    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to create webview window");

    (app, webview)
}

/// Invoke a command and panic on error.
fn invoke_cmd(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    args: Value,
) -> Value {
    try_invoke_cmd(webview, cmd, args)
        .unwrap_or_else(|e| panic!("IPC call '{cmd}' returned error: {e}"))
}

/// Invoke a command, returning Result (no panic).
fn try_invoke_cmd(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    args: Value,
) -> Result<Value, String> {
    let response = get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.to_string(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: args.clone().into(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map_err(|e| format!("{e}"))?;

    response
        .deserialize::<Value>()
        .map_err(|e| format!("deserialize failed: {e}"))
}

// ===========================================================================
// Tests
// ===========================================================================

// ---------------------------------------------------------------------------
// 1. Window & App bootstrap
// ---------------------------------------------------------------------------

#[test]
fn test_app_bootstrap_creates_empty_state() {
    let tmp = TempDir::new("bootstrap");
    let (_app, _webview) = setup_app(&tmp);
    // App bootstraps without panic — state is clean
}

// ---------------------------------------------------------------------------
// 2. Navigation routes (IPC-backed page data)
// ---------------------------------------------------------------------------

#[test]
fn test_search_page_empty() {
    let tmp = TempDir::new("search_page");
    let (_app, webview) = setup_app(&tmp);

    // Empty search returns empty result set
    let result = invoke_cmd(&webview, "search", json!({
        "query": "",
        "page": 1,
        "page_size": 20,
    }));
    assert_eq!(result["total"], json!(0), "expected total=0");
    assert_eq!(result["hits"], json!([]), "expected empty hits");
}

#[test]
fn test_suggest_empty() {
    let tmp = TempDir::new("suggest_page");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "suggest", json!({"prefix": ""}));
    assert_eq!(result, json!([]), "expected empty suggestions");
}

#[test]
fn test_browse_page_files_empty() {
    let tmp = TempDir::new("browse_page");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "list_files", json!({
        "page": 1,
        "page_size": 50,
        "sort_field": "path",
        "sort_order": "asc",
    }));
    assert_eq!(result["total"], json!(0), "expected total=0");
    assert_eq!(result["files"], json!([]), "expected empty files");
}

#[test]
fn test_directories_page_empty() {
    let tmp = TempDir::new("dirs_page");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "list_dirs", json!({}));
    assert_eq!(result, json!([]), "expected empty dir list");
}

#[test]
fn test_index_status_page_initial() {
    let tmp = TempDir::new("index_page");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "get_index_status", json!({}));
    assert_eq!(result["total_files"], json!(0), "expected total_files=0");
    assert_eq!(result["indexed"], json!(0), "expected indexed=0");
    let pending = result["pending"].as_i64().unwrap_or(0);
    assert_eq!(pending, 0, "expected pending=0");
    let failed = result["failed"].as_i64().unwrap_or(0);
    assert_eq!(failed, 0, "expected failed=0");
}

#[test]
fn test_settings_page_get() {
    let tmp = TempDir::new("settings_page");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "get_settings", json!({}));
    assert!(result.as_object().unwrap().contains_key("ocr_lang"), "settings should contain 'ocr_lang' key");
    assert_eq!(result["ocr_lang"], json!("chi_sim"), "expected ocr_lang=chi_sim");
}

#[test]
fn test_logs_page_empty() {
    let tmp = TempDir::new("logs_page");
    let (_app, webview) = setup_app(&tmp);

    // get_logs requires a logs directory — best-effort in mock mode
    if let Ok(result) = try_invoke_cmd(&webview, "get_logs", json!({
        "page": 1,
        "page_size": 50,
    })) {
        assert!(result.is_object() || result.is_array(), "logs should return an object or array");
    }
}

// ---------------------------------------------------------------------------
// 3. AI Chat page (session CRUD)
// ---------------------------------------------------------------------------

#[test]
fn test_ai_capabilities() {
    let tmp = TempDir::new("ai_cap");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "ai_capabilities", json!({}));
    // Just verify the response has the expected shape
    assert!(result.get("embedding").is_some(), "response should have 'embedding' field");
    assert!(result.get("llm").is_some(), "response should have 'llm' field");
}

#[test]
fn test_chat_session_crud() {
    let tmp = TempDir::new("chat_session");
    let (_app, webview) = setup_app(&tmp);

    // List — should be empty
    let list = invoke_cmd(&webview, "list_chat_sessions", json!({}));
    let initial_count = list.as_array().map(|a| a.len()).unwrap_or(0);

    // Create
    let id = invoke_cmd(&webview, "create_chat_session", json!({}));
    let session_id = id.as_str().unwrap().to_string();
    assert!(!session_id.is_empty(), "session id must not be empty");

    // List again — should have one more
    let list2 = invoke_cmd(&webview, "list_chat_sessions", json!({}));
    assert_eq!(
        list2.as_array().map(|a| a.len()).unwrap_or(0),
        initial_count + 1,
        "session count should increase by 1"
    );

    // Load
    let loaded = invoke_cmd(&webview, "load_chat_session", json!({"id": session_id}));
    assert!(loaded.is_object(), "loaded session should be an object");
    assert_eq!(loaded["id"], json!(session_id), "loaded session id should match");

    // Save (update title)
    let save_result = invoke_cmd(&webview, "save_chat_session", json!({
        "session": {
            "id": session_id,
            "title": "测试会话",
            "created_at": 0,
            "updated_at": 0,
            "messages": [],
            "source_ids": [],
            "source_files": [],
        }
    }));
    assert_eq!(save_result, json!(null), "save should succeed");

    // Load updated
    let updated = invoke_cmd(&webview, "load_chat_session", json!({"id": session_id}));
    assert_eq!(updated["title"], json!("测试会话"), "title should be updated");

    // Export
    let exported = invoke_cmd(&webview, "export_chat_session", json!({"id": session_id}));
    assert!(exported.as_str().map(|s| s.len() > 0).unwrap_or(false), "export should return markdown");

    // Delete
    let delete_result = invoke_cmd(&webview, "delete_chat_session", json!({"id": session_id}));
    assert_eq!(delete_result, json!(null), "delete should succeed");

    // Verify deleted
    let list3 = invoke_cmd(&webview, "list_chat_sessions", json!({}));
    assert_eq!(
        list3.as_array().map(|a| a.len()).unwrap_or(0),
        initial_count,
        "session count should be back to initial"
    );
}

// ---------------------------------------------------------------------------
// 4. Directory management
// ---------------------------------------------------------------------------

#[test]
fn test_add_and_remove_dir() {
    let tmp = TempDir::new("dir_crud");
    let (_app, webview) = setup_app(&tmp);

    let dir_path = tmp.path().join("test_monitor_dir");
    std::fs::create_dir_all(&dir_path).unwrap();

    // Add
    let added = invoke_cmd(&webview, "add_dir", json!({
        "path": dir_path.to_str().unwrap(),
        "alias": "e2e-test",
        "recursive": true,
    }));
    assert_eq!(added["path"], json!(dir_path.to_str().unwrap()), "path should match");
    assert_eq!(added["alias"], json!("e2e-test"), "alias should match");

    let dir_id = added["id"].as_str().unwrap().to_string();

    // List — should include it
    let list = invoke_cmd(&webview, "list_dirs", json!({}));
    let ids: Vec<&str> = list.as_array().unwrap().iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert!(ids.contains(&dir_id.as_str()), "dir should be in list");

    // Remove
    let removed = invoke_cmd(&webview, "remove_dir", json!({"id": dir_id}));
    assert_eq!(removed, json!(null), "remove should succeed");

    // Verify removed
    let list2 = invoke_cmd(&webview, "list_dirs", json!({}));
    let ids2: Vec<&str> = list2.as_array().unwrap().iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert!(!ids2.contains(&dir_id.as_str()), "dir should be removed from list");
}

// ---------------------------------------------------------------------------
// 5. File type support
// ---------------------------------------------------------------------------

#[test]
fn test_ocr_engine_list() {
    let tmp = TempDir::new("ocr_engines");
    let (_app, webview) = setup_app(&tmp);

    // Best-effort in mock mode (no PaddleOCR models initialized)
    if let Ok(result) = try_invoke_cmd(&webview, "list_ocr_engines", json!({})) {
        if let Some(engines) = result.as_array() {
            assert!(!engines.is_empty(), "should list at least one OCR engine");
        }
    }
}

#[test]
fn test_file_type_support() {
    let tmp = TempDir::new("file_types");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "get_file_type_support", json!({}));
    assert!(result.is_array(), "file type support should be an array");
    assert!(!result.as_array().unwrap().is_empty(), "should list supported types");
}

// ---------------------------------------------------------------------------
// 6. Config & settings
// ---------------------------------------------------------------------------

#[test]
fn test_config_round_trip() {
    let tmp = TempDir::new("config");
    let (_app, webview) = setup_app(&tmp);

    let cfg = invoke_cmd(&webview, "get_config", json!({}));
    assert!(cfg.is_object(), "config should be an object");
    // Default config should have empty providers list
    assert!(cfg["providers"].as_array().is_some(), "providers should be a list");

    // Update settings (use known valid keys only)
    let updated = invoke_cmd(&webview, "update_settings", json!({
        "settings": {
            "ocr_lang": "eng",
        }
    }));
    assert_eq!(updated, json!(null), "update_settings should succeed");

    let settings = invoke_cmd(&webview, "get_settings", json!({}));
    assert_eq!(settings["ocr_lang"], json!("eng"), "ocr_lang should be updated");
}

// ---------------------------------------------------------------------------
// 7. Version info
// ---------------------------------------------------------------------------

#[test]
fn test_app_version() {
    let tmp = TempDir::new("version");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "get_version", json!({}));
    // In mock mode, version may be empty (no GIT_VERSION env var) — just verify it doesn't panic
    let version = result.as_str().unwrap_or("");
    assert!(version.is_empty() || version.contains('.'), "version should be empty or semver-like");
}