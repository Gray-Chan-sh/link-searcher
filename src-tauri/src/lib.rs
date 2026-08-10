pub mod ai;
pub mod cli;
pub mod commands;
pub mod config;
pub mod db;
pub mod extractor;
pub mod indexer;
pub mod logs;
pub mod scanner;
pub mod search;
pub mod state;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use crate::commands::backup::{get_backup_status, restore_backup, trigger_backup};
use crate::commands::ai::{ai_capabilities, ask_documents, cancel_ai_request, conversation_ask, conversation_ask_stream, create_chat_session, delete_chat_session, export_chat_session, list_chat_sessions, load_chat_session, save_chat_session, smart_search, smart_search_stream, summarize_file, test_ai_gateway};
use crate::commands::config::{add_provider, delete_provider, get_config, migrate_data, refresh_provider_models, restart_app, set_active_model, test_provider, update_config, update_provider};
use crate::commands::dirs::{add_dir, get_dir_tree, list_dirs, remove_dir, update_dir};
use crate::commands::files::{download_files, get_duplicates, get_file, get_file_preview, list_dir_entries, list_files, list_files_db, open_file, preview_file, preview_file_by_path, reveal_in_folder};
use crate::commands::index::{backfill_embeddings, cancel_scan, check_index_health, check_index_integrity, get_index_errors, get_index_status, rebuild_index, reextract_missing_content, reindex_file, trigger_scan};
use crate::commands::search::{clear_search_history, export_search_results, get_browse_file_types, get_file_type_stats, get_search_history, search, suggest};
use crate::commands::settings::{get_settings, get_version, update_settings};
use crate::commands::logs::{clear_logs, get_logs, list_session_logs};
use crate::commands::funasr::install_funasr;
use crate::commands::tesseract::{check_dependencies, check_tesseract, get_file_type_support, get_unsupported_ext_stats, list_ocr_engines, test_ocr_engine};
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

    let result = tauri::Builder::default()
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
            summarize_file,
            ask_documents,
            smart_search,
            smart_search_stream,
            conversation_ask_stream,
            conversation_ask,
            list_chat_sessions,
            create_chat_session,
            delete_chat_session,
            load_chat_session,
            save_chat_session,
            export_chat_session,
            test_ai_gateway,
            ai_capabilities,
            cancel_ai_request,
            get_search_history,
            clear_search_history,
            export_search_results,
            get_file_type_stats,
            get_browse_file_types,
            get_index_status,
            trigger_scan,
            rebuild_index,
            reindex_file,
            reextract_missing_content,
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
            get_unsupported_ext_stats,
            check_index_health,
            check_index_integrity,
            backfill_embeddings,
            get_logs,
            list_session_logs,
            clear_logs,
            get_version,
            install_funasr,
            get_config,
            update_config,
            add_provider,
            update_provider,
            delete_provider,
            refresh_provider_models,
            set_active_model,
            test_provider,
            migrate_data,
            restart_app,
        ])
        .setup(|app| {
            log::info!("data directory: {:?}", data_dir);
            if let Err(e) = std::fs::create_dir_all(&data_dir) {
                eprintln!("[FATAL] failed to create data directory {:?}: {}", data_dir, e);
                return Err(Box::new(e));
            }

            // Initialize file logger (append across restarts, rotate at 100 MB)
            let log_path = data_dir.join("app.log");
            if let Ok(meta) = std::fs::metadata(&log_path) {
                if meta.len() > 100 * 1024 * 1024 {
                    let rotated = data_dir.join("app.log.1");
                    let _ = std::fs::rename(&log_path, &rotated);
                }
            }
            let log_file: Box<dyn std::io::Write + Send> =
                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    Ok(f) => Box::new(f),
                    Err(e) => {
                        eprintln!(
                            "[WARN] failed to open log file {:?}: {}, falling back to stderr",
                            log_path, e
                        );
                        Box::new(std::io::stderr())
                    }
                };
            env_logger::Builder::from_env(
                env_logger::Env::default().default_filter_or("info"),
            )
            .target(env_logger::Target::Pipe(log_file))
            .format(|buf, record| {
                use std::io::Write;
                let ts = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
                writeln!(
                    buf,
                    "[{} {:<5} {}] {}",
                    ts,
                    record.level(),
                    record.target(),
                    record.args()
                )
            })
            .init();
            let version = env!("GIT_VERSION");
            let commit_time = env!("GIT_COMMIT_TIME");
            log::info!("application started (git: {version}, {commit_time})");

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

            // Apply LO batch size to the global batcher.
            if let Ok(conn) = db_pool.get() {
                match conn.query_row(
                    "SELECT value FROM app_settings WHERE key = 'lo_batch_size'",
                    [],
                    |row| row.get::<_, String>(0),
                ) {
                    Ok(v) => match v.parse::<usize>() {
                        Ok(n) => {
                            crate::extractor::office::LO_BATCH_SIZE
                                .store(n.max(1), std::sync::atomic::Ordering::Relaxed);
                            log::info!("[STARTUP] LO batch size set to {}", n.max(1));
                        }
                        Err(e) => log::warn!(
                            "[STARTUP] Failed to parse lo_batch_size value '{}': {}",
                            v, e
                        ),
                    },
                    Err(e) => log::warn!("[STARTUP] Failed to read lo_batch_size setting: {}", e),
                }
            }

            if let Ok(conn) = db_pool.get() {
                let engine_name = conn
                    .query_row(
                        "SELECT value FROM app_settings WHERE key = 'ocr_engine'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap_or_else(|_| "PaddleOCR".to_string());
                log::info!("[STARTUP] OCR engine: {}", engine_name);
            }

            if let Err(e) = crate::extractor::paddleocr::health_check() {
                log::error!("OCR 引擎自检失败: {e}");
                eprintln!("[OCR] 引擎自检失败: {e}");
            } else {
                log::info!("OCR 引擎自检通过");
            }

            // Warm up Apple Vision OCR (preloads CoreML/ANE models on a background
            // thread, eliminating the 1–3 s first-call latency).
            #[cfg(target_os = "macos")]
            {
                let warmup_path = data_dir.join(".vision_warmup.png");
                {
                    let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([255u8; 4]));
                    if let Err(e) = img.save(&warmup_path) {
                        log::warn!("[STARTUP] Vision warmup image failed: {}", e);
                    }
                }
                std::thread::spawn(move || {
                    match crate::extractor::apple_vision::recognize_from_path(
                        &warmup_path, "eng",
                    ) {
                        Ok(_) => log::info!("Apple Vision OCR warmed up"),
                        Err(e) => log::warn!("Apple Vision OCR warmup failed: {e}"),
                    }
                    let _ = std::fs::remove_file(&warmup_path);
                });
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
                    log::debug!("[WATCHER] file {:?}: {:?}", event.kind, event.path);
                    let _ = scanner_for_watcher.handle_event(event);
                }
            });

            let logs_dir = data_dir.join("logs");
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

                // Per-scan session log (optional — scan proceeds without it).
                let mut slog = crate::logs::session::SessionLog::open(&logs_dir, "scan")
                    .map_err(|e| log::warn!("[STARTUP] 无法创建会话日志: {e}"))
                    .ok();
                let mut sess = |line: String| {
                    if let Some(ref mut f) = slog {
                        let _ = crate::logs::session::SessionLog::write(f, &line);
                    }
                };

                log::info!("[STARTUP] 检查系统依赖...");

                let ocr_ok = crate::extractor::paddleocr::health_check().is_ok();
                let lo_ok = office::is_libreoffice_available();

                let pdf_ok = pdf::is_pdftoppm_available();

                let funasr_ok = crate::extractor::audio::funasr_model_ready();
                let ffmpeg_ok = crate::extractor::audio::ffmpeg_available();

                log::info!(
                    "[STARTUP] PaddleOCR={} LibreOffice={} pdftoppm={} FunASR={} ffmpeg={}",
                    if ocr_ok { "OK" } else { "FAIL" },
                    if lo_ok { "OK" } else { "N/A" },
                    if pdf_ok { "OK" } else { "N/A" },
                    if funasr_ok { "OK" } else { "MISSING" },
                    if ffmpeg_ok { "OK" } else { "MISSING" },
                );

                if !ffmpeg_ok {
                    log::info!("[STARTUP] ffmpeg 未找到，音频解码暂不可用（brew install ffmpeg）");
                }

                if !funasr_ok {
                    log::info!("[STARTUP] FunASR 模型未下载，音频转写暂不可用（设置页可下载）");
                }

                if !ocr_ok {
                    log::error!("[STARTUP] PaddleOCR 引擎不可用，图片 OCR 将无法工作");
                }

                log::info!("[STARTUP] 开始扫描 {} 个目录", dirs.len());
                sess(format!("[STARTUP] 开始扫描 {} 个目录", dirs.len()));
                for dir in &dirs {
                    let result = scanner_ref.startup_scan(&dir.id, |prog| {
                        let _ = app_handle.emit("scan-progress", crate::commands::index::ScanEventPayload {
                            phase: prog.phase.into(),
                            current: prog.processed,
                            total: prog.total,
                            current_file: prog.current_file,
                            dir_id: dir.id.clone(),
                        });
                    });
                    match result {
                        Ok(r) => {
                            let line = format!(
                                "[STARTUP] {}: {} files, {} indexed, {} errors",
                                dir.path, r.total_files, r.indexed, r.errors
                            );
                            log::info!("{line}");
                            sess(line);
                        }
                        Err(e) => {
                            let line = format!("[STARTUP] {} 扫描失败: {e}", dir.path);
                            log::error!("{line}");
                            sess(line);
                        }
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

                // Close session log before signalling completion.
                sess("[STARTUP] 启动扫描完成".to_string());
                drop(sess);
                if let Some(f) = slog {
                    crate::logs::session::SessionLog::close(f);
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
        .run(tauri::generate_context!());
    match result {
        Ok(()) => {}
        Err(e) => {
            log::error!("Tauri runtime error: {e}");
            std::process::exit(1);
        }
    }
}
