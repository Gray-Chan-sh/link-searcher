pub mod cli;
pub mod commands;
pub mod config;
pub mod db;
pub mod extractor;
pub mod indexer;
pub mod scanner;
pub mod search;
pub mod state;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use crate::commands::backup::{get_backup_status, restore_backup, trigger_backup};
use crate::commands::config::{get_config, migrate_data, restart_app, update_config};
use crate::commands::dirs::{add_dir, get_dir_tree, list_dirs, remove_dir, update_dir};
use crate::commands::files::{download_files, get_duplicates, get_file, get_file_preview, list_dir_entries, list_files, list_files_db, open_file, preview_file, preview_file_by_path, reveal_in_folder};
use crate::commands::index::{cancel_scan, check_index_health, get_index_errors, get_index_status, rebuild_index, reindex_file, trigger_scan};
use crate::commands::search::{export_search_results, get_file_type_stats, get_search_history, search, suggest};
use crate::commands::settings::{get_settings, update_settings};
use crate::commands::logs::{clear_logs, get_logs};
use crate::commands::tesseract::{check_dependencies, check_tesseract, get_file_type_support, list_ocr_engines, test_ocr_engine};
use crate::indexer::IndexerService;
use crate::scanner::Scanner;
use crate::scanner::watcher::FileWatcher;
use crate::search::IndexManager;
use crate::state::AppState;
use crate::state::ScanDelta;
use env_logger;
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::menu::{Menu, MenuItem};
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

pub fn run() {
    run_with_config(config::load_config());
}

pub fn run_with_data_dir(data_dir: std::path::PathBuf) {
    let mut app_config = config::load_config();
    app_config.data_dir = data_dir;
    run_with_config(app_config);
}

fn run_with_config(app_config: config::AppConfig) {
    let data_dir = app_config.data_dir.clone();

    tauri::Builder::default()
        // Must precede other plugins so the second instance exits before they initialize.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .manage(app_config)
        .invoke_handler(tauri::generate_handler![
            search,
            suggest,
            get_search_history,
            export_search_results,
            get_file_type_stats,
            get_index_status,
            trigger_scan,
            rebuild_index,
            reindex_file,
            cancel_scan,
            get_index_errors,
            add_dir,
            get_dir_tree,
            remove_dir,
            list_dirs,
            update_dir,
            get_file,
            list_files,
            list_files_db,
            get_duplicates,
            preview_file,
            open_file,
            download_files,
            get_file_preview,
            list_dir_entries,
            preview_file_by_path,
            reveal_in_folder,
            trigger_backup,
            get_backup_status,
            restore_backup,
            get_settings,
            update_settings,
            check_tesseract,
            list_ocr_engines,
            test_ocr_engine,
            check_dependencies,
            get_file_type_support,
            check_index_health,
            get_logs,
            clear_logs,
            get_config,
            update_config,
            migrate_data,
            restart_app,
        ])
        .setup(|app| {
            log::info!("data directory: {:?}", data_dir);
            if let Err(e) = std::fs::create_dir_all(&data_dir) {
                eprintln!("[FATAL] failed to create data directory {:?}: {}", data_dir, e);
                return Err(Box::new(e));
            }

            // Initialize file logger
            let log_path = data_dir.join("app.log");
            let log_file: Box<dyn std::io::Write + Send> =
                match std::fs::File::create(&log_path) {
                    Ok(f) => Box::new(f),
                    Err(e) => {
                        eprintln!(
                            "[WARN] failed to create log file {:?}: {}, falling back to stderr",
                            log_path, e
                        );
                        Box::new(std::io::stderr())
                    }
                };
            env_logger::Builder::from_env(
                env_logger::Env::default().default_filter_or("info"),
            )
            .target(env_logger::Target::Pipe(log_file))
            .format_timestamp_secs()
            .init();
            log::info!("application started");

            // One-time migration: rename legacy `index` dir to `.ls-index`
            let legacy_index = data_dir.join("index");
            let new_index = data_dir.join(config::INDEX_DIR_NAME);
            if !new_index.exists() && legacy_index.exists() && legacy_index.is_dir() {
                std::fs::rename(&legacy_index, &new_index).ok();
                log::info!("[STARTUP] 索引目录迁移: index -> {}", config::INDEX_DIR_NAME);
            }

            let index_dir = new_index;
            let db_path = data_dir.join("data.db");

            std::fs::create_dir_all(&index_dir).ok();

            let db_pool = db::get_pool(db_path.to_string_lossy().as_ref())?;
            let init_conn = db_pool.get()?;
            db::init_db(&init_conn)?;
            drop(init_conn);

            // Apply OCR concurrency setting before the engine pool is built
            // (health_check below lazily constructs the pool).
            match db_pool.get() {
                Ok(conn) => {
                    match conn.query_row(
                        "SELECT value FROM app_settings WHERE key = 'ocr_concurrent'",
                        [],
                        |row| row.get::<_, String>(0),
                    ) {
                        Ok(v) => {
                            match v.parse::<usize>() {
                                Ok(n) => {
                                    crate::extractor::paddleocr::set_pool_size(n);
                                    log::info!("[STARTUP] OCR concurrency set to {} engine(s)", n);
                                }
                                Err(e) => log::warn!("[STARTUP] Failed to parse ocr_concurrent value '{}': {}", v, e),
                            }
                        }
                        Err(e) => log::warn!("[STARTUP] Failed to read ocr_concurrent setting: {}", e),
                    }
                }
                Err(e) => log::warn!("[STARTUP] Failed to get DB connection for OCR concurrency: {}", e),
            }

            if let Err(e) = crate::extractor::paddleocr::health_check() {
                log::error!("OCR 引擎自检失败: {e}");
                eprintln!("[OCR] 引擎自检失败: {e}");
            } else {
                log::info!("OCR 引擎自检通过");
            }

            // Warn (don't block) if any monitored dir already overlaps the data dir.
            if let Ok(conn) = db_pool.get() {
                if let Ok(dirs) = crate::db::dir_config::list_dirs(&conn) {
                    for dir in &dirs {
                        if let Err(msg) = crate::commands::helpers::check_data_dir_overlap(
                            &data_dir,
                            std::path::Path::new(&dir.path),
                        ) {
                            log::warn!(
                                "检测到数据目录 {} 与监控目录 {} 存在交叠，请尽快修正 ({msg})",
                                data_dir.display(),
                                dir.path
                            );
                        }
                    }
                }
                drop(conn);
            }

            let index_manager = Arc::new(RwLock::new(
                IndexManager::open_or_create(&index_dir)?,
            ));

            // Arc is shared between IndexerService and AppState so rebuild_index can swap the manager
            let cancel_scan = Arc::new(AtomicBool::new(false));
            let indexer = Arc::new(IndexerService::with_cancel(
                db_pool.clone(),
                index_manager.clone(),
                cancel_scan.clone(),
            ));

            let scanner = Arc::new(Scanner::with_cancel(
                db_pool.clone(),
                indexer.clone(),
                cancel_scan.clone(),
            ));
            let is_scanning = Arc::new(AtomicBool::new(false));

            let (watcher, event_rx) = FileWatcher::new();
            let watcher_tx = watcher.tx().clone();
            let watcher_tx_for_startup = watcher_tx.clone();

            let scanner_for_watcher = scanner.clone();
            std::thread::spawn(move || {
                while let Ok(event) = event_rx.recv() {
                    log::info!("[WATCHER] file {:?}: {:?}", event.kind, event.path);
                    let _ = scanner_for_watcher.handle_event(event);
                }
            });

            let app_state = AppState::new(
                db_pool.clone(),
                index_manager,
                indexer,
                scanner.clone(),
                is_scanning,
                Arc::new(AtomicBool::new(false)),
                cancel_scan,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(ScanDelta::default())),
                data_dir,
                index_dir,
                db_path.clone(),
                watcher_tx,
                Some(watcher),
            );

            app.manage(app_state);

            let app_handle = app.handle().clone();
            let scanner_ref = scanner.clone();
            let db_ref = db_pool.clone();
            let watch_tx = watcher_tx_for_startup;

            // R3-11: StartWatch 必须先于扫描线程，否则扫描期间的变更会丢失
            let dirs = {
                let conn = match db_ref.get() {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("[STARTUP] 无法获取数据库连接: {e}");
                        return Ok(());
                    }
                };
                let dirs = match crate::db::dir_config::list_dirs(&conn) {
                    Ok(d) => d,
                    Err(e) => {
                        log::error!("[STARTUP] 无法读取目录列表: {e}");
                        return Ok(());
                    }
                };
                drop(conn);
                dirs
            };

            if dirs.is_empty() {
                log::info!("[STARTUP] 无已配置目录，跳过启动扫描");
                return Ok(());
            }

            // One-time migration: convert absolute paths to relative
            if let Ok(conn) = db_ref.get() {
                let _ = crate::db::tracker::migrate_paths_to_relative(&conn);
            }

            for dir in &dirs {
                let path = std::path::PathBuf::from(&dir.path);
                if path.exists() {
                    let _ = watch_tx.send(
                        crate::scanner::watcher::WatcherCommand::StartWatch {
                            dir_id: dir.id.clone(),
                            path,
                        },
                    );
                    log::info!("[STARTUP] 启动文件监控: {}", dir.path);
                }
            }

            std::thread::spawn(move || {
                use crate::extractor::{office, pdf};

                log::info!("[STARTUP] 检查系统依赖...");

                // ────── LibreOffice Dock icon suppression ──────
                // Clear any LSUIElement left over from a crashed session, then
                // suppress the Dock icon only for the duration of this startup
                // scan (restored automatically when the guard drops).
                crate::extractor::office::ensure_lo_background_mode();
                let _lo_guard = crate::extractor::office::LoBackgroundGuard::enter();

                let ocr_ok = crate::extractor::paddleocr::health_check().is_ok();
                let lo_ok = office::is_libreoffice_available();

                let pdf_ok = pdf::is_pdftoppm_available();

                log::info!(
                    "[STARTUP] PaddleOCR={} LibreOffice={} pdftoppm={}",
                    if ocr_ok { "OK" } else { "FAIL" },
                    if lo_ok { "OK" } else { "N/A" },
                    if pdf_ok { "OK" } else { "N/A" },
                );

                if !ocr_ok {
                    log::error!("[STARTUP] PaddleOCR 引擎不可用，图片 OCR 将无法工作");
                }

                log::info!("[STARTUP] 开始扫描 {} 个目录", dirs.len());
                for dir in &dirs {
                    match scanner_ref.startup_scan(&dir.id, |_| {}) {
                        Ok(r) => log::info!(
                            "[STARTUP] {}: {} files, {} indexed, {} errors",
                            dir.path, r.total_files, r.indexed, r.errors
                        ),
                        Err(e) => log::error!("[STARTUP] {} 扫描失败: {e}", dir.path),
                    }
                }

                // Post-scan maintenance
                if let Ok(conn) = db_ref.get() {
                    if let Err(e) = crate::db::cleanup_orphan_content(&conn) {
                        log::error!("[STARTUP] orphan cleanup failed: {e}");
                    }
                    drop(conn);
                }

                // VACUUM after watchers started — only if DB > 100 MiB
                let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
                if db_size > 100 * 1024 * 1024 {
                    if let Ok(conn) = db_ref.get() {
                        if let Err(e) = crate::db::vacuum(&conn) {
                            log::error!("[STARTUP] VACUUM failed: {e}");
                        }
                        drop(conn);
                    }
                } else {
                    log::info!("[STARTUP] VACUUM skipped (db_size={db_size} B, threshold=100 MiB)");
                }

                app_handle.emit("scan-completed", serde_json::json!({}))
                    .unwrap_or_else(|e| log::error!("[STARTUP] failed to emit scan-completed: {e}"));
                log::info!("[STARTUP] 启动扫描完成");
            });

            // Set window icon from embedded PNG bytes
            if let Some(window) = app.get_webview_window("main") {
                let icon_bytes = include_bytes!("../icons/32x32.png");
                let img = image::load_from_memory(icon_bytes)
                    .and_then(|i| Ok(i.into_rgba8()));
                if let Ok(img) = img {
                    let (w, h) = img.dimensions();
                    let rgba = img.into_raw();
                    let _ = window.set_icon(tauri::image::Image::new(&rgba, w, h));
                }
            }

            // System tray
            let show_item = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
                        "show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::DoubleClick { .. } = event {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            // Close button minimizes to tray
            if let Some(window) = app.get_webview_window("main") {
                let window_ = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
