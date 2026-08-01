//! IPC integration tests — verifies that frontend IPC command invocations
//! reach the correct Rust handler and return the expected response shape.
//!
//! These tests use a mock Tauri runtime with temp-file storage so they don't
//! need a real window or full app config.

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
        let dir = std::env::temp_dir().join(format!("ls_ipc_{prefix}_{}", std::process::id()));
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

/// Build a mock Tauri app with managed [`AppState`] and the registered IPC
/// command handlers, then create a webview window for IPC testing.
///
/// The mock context from `mock_context(noop_assets())` has an empty ACL with
/// no allowed commands, so we inject `__allow_command` for each tested command
/// directly into the runtime authority.
fn setup_app(
    tmp: &TempDir,
) -> (tauri::App<tauri::test::MockRuntime>, tauri::WebviewWindow<tauri::test::MockRuntime>) {
    let db_path = tmp.path().join("test.db");
    let db_str = db_path.to_str().unwrap();
    db::init_db(db_str).unwrap();
    let pool = db::get_pool(db_str).unwrap();
    let index_dir = tmp.path().join("index");

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
        Arc::new(AtomicBool::new(false)),           // is_rebuilding
        Arc::new(AtomicBool::new(false)),           // cancel_scan
        Arc::new(AtomicBool::new(false)),           // is_restoring
        Arc::new(Mutex::new(ScanDelta::default())), // scan_delta
        tmp.path().to_path_buf(),                   // data_dir
        index_dir,
        db_path,
        dummy_tx,
        None,
    );

    let mut ctx = mock_context(noop_assets());
    for cmd in &["list_dirs", "add_dir", "get_index_status", "search", "suggest", "get_settings"] {
        ctx.runtime_authority_mut()
            .__allow_command(cmd.to_string(), tauri::utils::acl::ExecutionContext::Local);
    }

    let app = mock_builder()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            link_searcher_lib::commands::dirs::list_dirs,
            link_searcher_lib::commands::dirs::add_dir,
            link_searcher_lib::commands::index::get_index_status,
            link_searcher_lib::commands::search::search,
            link_searcher_lib::commands::search::suggest,
            link_searcher_lib::commands::settings::get_settings,
        ])
        .build(ctx)
        .expect("failed to build mock app");

    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to create webview window");

    (app, webview)
}

/// Invoke a Tauri IPC command and return the deserialized response body.
fn invoke_cmd(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    args: Value,
) -> Value {
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
    .unwrap_or_else(|e| panic!("IPC call '{cmd}' returned error: {e}"));

    response
        .deserialize::<Value>()
        .unwrap_or_else(|e| panic!("failed to deserialize IPC response for '{cmd}': {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_ipc_list_dirs() {
    let tmp = TempDir::new("list_dirs");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "list_dirs", json!({}));
    assert_eq!(result, json!([]), "expected empty dir list");
}

#[test]
fn test_ipc_add_dir() {
    let tmp = TempDir::new("add_dir");
    let (_app, webview) = setup_app(&tmp);

    let dir_path = tmp.path().join("mydir");
    std::fs::create_dir_all(&dir_path).unwrap();

    let result = invoke_cmd(
        &webview,
        "add_dir",
        json!({
            "path": dir_path.to_str().unwrap(),
            "alias": "test",
            "recursive": true,
        }),
    );

    assert_eq!(
        result["path"], json!(dir_path.to_str().unwrap()),
        "path should match"
    );
    assert_eq!(result["alias"], json!("test"), "alias should match");
    assert_eq!(result["recursive"], json!(true), "recursive should match");
}

#[test]
fn test_ipc_get_index_status() {
    let tmp = TempDir::new("index_status");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "get_index_status", json!({}));

    assert_eq!(
        result["total_files"], json!(0),
        "expected total_files=0"
    );
    assert_eq!(result["indexed"], json!(0), "expected indexed=0");
}

#[test]
fn test_ipc_search() {
    let tmp = TempDir::new("search");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(
        &webview,
        "search",
        json!({
            "query": "",
            "page": 1,
            "page_size": 20,
        }),
    );

    assert_eq!(result["total"], json!(0), "expected total=0");
    assert_eq!(result["hits"], json!([]), "expected empty hits");
}

#[test]
fn test_ipc_suggest() {
    let tmp = TempDir::new("suggest");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "suggest", json!({"prefix": ""}));

    assert_eq!(result, json!([]), "expected empty suggestions");
}

#[test]
fn test_ipc_get_settings() {
    let tmp = TempDir::new("settings");
    let (_app, webview) = setup_app(&tmp);

    let result = invoke_cmd(&webview, "get_settings", json!({}));

    assert!(
        result.as_object().unwrap().contains_key("ocr_lang"),
        "settings should contain 'ocr_lang' key"
    );
    assert_eq!(result["ocr_lang"], json!("eng"), "expected ocr_lang=eng");
}