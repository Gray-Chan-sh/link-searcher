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
use crate::commands::files::{download_files, get_duplicates, get_file, get_file_preview, list_dir_entries, list_files, open_file, preview_file, preview_file_by_path, reveal_in_folder};
use crate::commands::index::{cancel_scan, check_index_health, get_index_errors, get_index_status, rebuild_index, trigger_scan};
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
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

pub fn run() {
    let app_config = config::load_config();
    let data_dir = app_config.data_dir.clone();

    tauri::Builder::default()
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
            cancel_scan,
            get_index_errors,
            add_dir,
            get_dir_tree,
            remove_dir,
            list_dirs,
            update_dir,
            get_file,
            list_files,
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

            if let Err(e) = crate::extractor::paddleocr::health_check() {
                log::error!("OCR 引擎自检失败: {e}");
                eprintln!("[OCR] 引擎自检失败: {e}");
            } else {
                log::info!("OCR 引擎自检通过");
            }

            let index_dir = data_dir.join("index");
            let db_path = data_dir.join("data.db");

            std::fs::create_dir_all(&index_dir).ok();

            let db_pool = db::get_pool(db_path.to_string_lossy().as_ref()).expect("failed to create DB pool");
            db::init_db(db_path.to_string_lossy().as_ref()).expect("failed to init DB");

            let index_manager = Arc::new(RwLock::new(
                IndexManager::open_or_create(&index_dir).expect("failed to open index"),
            ));

            // Arc is shared between IndexerService and AppState so rebuild_index can swap the manager
            let indexer = Arc::new(IndexerService::new(
                db_pool.clone(),
                index_manager.clone(),
            ));

            let scanner = Arc::new(Scanner::new(db_pool.clone(), indexer.clone()));
            let is_scanning = Arc::new(AtomicBool::new(false));
            let cancel_scan = Arc::new(AtomicBool::new(false));

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
                cancel_scan,
                Arc::new(Mutex::new(ScanDelta::default())),
                data_dir,
                index_dir,
                db_path,
                watcher_tx,
                Some(watcher),
            );

            app.manage(app_state);

            let scanner_ref = scanner.clone();
            let db_ref = db_pool.clone();
            let watch_tx = watcher_tx_for_startup;
            std::thread::spawn(move || {
                use crate::extractor::{office, pdf};

                log::info!("[STARTUP] 检查系统依赖...");

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

                let conn = match db_ref.get() {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("[STARTUP] 无法获取数据库连接: {e}");
                        return;
                    }
                };
                let dirs = match crate::db::dir_config::list_dirs(&conn) {
                    Ok(d) => d,
                    Err(e) => {
                        log::error!("[STARTUP] 无法读取目录列表: {e}");
                        return;
                    }
                };
                drop(conn);

                if dirs.is_empty() {
                    log::info!("[STARTUP] 无已配置目录，跳过启动扫描");
                    return;
                }

                // One-time migration: convert absolute paths to relative
                if let Ok(conn) = db_ref.get() {
                    let _ = crate::db::tracker::migrate_paths_to_relative(&conn);
                }

                log::info!("[STARTUP] 开始扫描 {} 个目录", dirs.len());
                for dir in &dirs {
                    match scanner_ref.startup_scan(&dir.id) {
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
                    if let Err(e) = crate::db::vacuum(&conn) {
                        log::error!("[STARTUP] VACUUM failed: {e}");
                    }
                    drop(conn);
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
