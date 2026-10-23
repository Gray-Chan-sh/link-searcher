pub mod ai;
pub mod boot;
pub mod cli;
pub mod commands;
pub mod config;
pub mod config_watch;
pub mod db;
pub mod deps;
pub mod extractor;
pub mod indexer;
pub mod lark_bot;
pub mod lark_setup;
pub mod load;
pub mod logs;
pub mod maintenance;
pub mod process;
pub mod scanner;
pub mod search;
pub mod state;
pub mod transcript;
pub mod webapi;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::commands::backup::{apply_restore, delete_backup, export_backup, get_backup_status, get_dead_dirs, list_backups, plan_restore, preview_remap, remap_dir, remove_dir_with_files, restore_backup, restore_from_zip, trigger_backup, verify_backup};
use crate::commands::ai::{ai_capabilities, ai_topic_clusters, ask_documents, cancel_ai_request, conversation_ask, conversation_ask_stream, create_chat_session, delete_chat_session, export_chat_session, export_chat_session_json, get_ai_events, get_turn_ai_events, list_chat_sessions, load_chat_session, rename_chat_session, save_chat_session, smart_search, smart_search_stream, summarize_file, test_ai_gateway};
use crate::commands::config::{add_provider, delete_provider, get_config, migrate_data, refresh_provider_models, restart_app, set_active_model, test_provider, update_config, update_provider};
use crate::commands::dirs::{add_dir, get_dir_children, get_dir_tree, list_dirs, remove_dir, update_dir};
use crate::commands::files::{download_files, export_file_text, get_duplicates, get_file, get_file_preview, list_dir_entries, list_files, list_files_db, open_file, preview_file, preview_file_by_path, reveal_in_folder};
use crate::commands::index::{backfill_audio_timeline, backfill_chunk_embeddings, backfill_embeddings, backfill_quality, cancel_scan, check_embedding_consistency, check_index_health, check_index_integrity, get_index_errors, get_index_status, get_quality_summary, heal_index_integrity, quality_audit, re_extract_file, re_extract_low_quality, rebuild_embeddings, rebuild_index, reextract_missing_content, reindex_file, reindex_files, restore_files, retry_failed_files, trigger_scan, verify_index_content};
use crate::commands::lark::{get_lark_bindings, get_lark_config, get_lark_status, lark_bind_cancel, lark_bind_dismiss, lark_bind_start, lark_bind_status, lark_pairing_cancel, lark_pairing_start, lark_restart, lark_self_check, set_lark_config};
use crate::commands::search::{clear_search_history, export_search_results, get_browse_file_types, get_file_type_stats, get_search_history, refine_search, search, search_file_ids_only, search_file_paths, search_tree_prune, suggest};
use crate::commands::settings::{
    autostart_state, get_settings, get_version, set_autostart, update_settings,
};
use crate::commands::webapi::{web_session_logout, web_session_status};
use crate::commands::performance::{detect_hardware, auto_optimize, get_performance_profile, set_performance_tier};
use crate::commands::logs::{clear_logs, get_logs, list_session_logs};
use crate::commands::bge::{check_bge_installed, install_bge};
use crate::commands::rerank::check_rerank_installed;
use crate::commands::tesseract::{check_dependencies, check_tesseract, get_file_type_support, get_unsupported_ext_stats, list_ocr_engines, test_ocr_engine};
use crate::deps::commands::{cancel_dep_install, dep_install_status, get_setup_status, install_dep};
use crate::scanner::watcher::FileWatcher;
use crate::state::AppState;
use crate::state::ScanDelta;

use tauri::Emitter;
use tauri::Manager;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri_plugin_autostart::MacosLauncher;

pub fn run() {
    run_with_config(config::load_config());
}

// 切回 Regular activation policy 会重置 Dock 图标，需重新应用
#[cfg(target_os = "macos")]
fn restore_dock_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(include_bytes!("../icons/icon.png"));
    if let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe {
            NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon));
        }
    }
}
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)] // 唯一调用点在 `#[cfg(target_os = "macos")]` 门后；这个空实现只为让非 macOS 也能编译
fn restore_dock_icon() {}

pub fn run_with_data_dir(data_dir: std::path::PathBuf) {
    let mut app_config = config::load_config();
    app_config.data_dir = data_dir;
    run_with_config(app_config);
}

fn run_with_config(app_config: config::AppConfig) {
    let data_dir = app_config.data_dir.clone();

    let builder = tauri::Builder::default()
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
        .manage(app_config);

    // MCP plugin — debug-only AI agent integration.
    // Shadow the binding instead of `let mut builder` + reassignment: release builds would
    // otherwise carry an `unused_mut` warning (the only mutation is this debug-only branch).
    #[cfg(debug_assertions)]
    let builder = builder.plugin(tauri_plugin_mcp::init());

    let result = builder.invoke_handler(tauri::generate_handler![
            search,
            search_file_paths,
            search_tree_prune,
            suggest,
            search_file_ids_only,
            ai_topic_clusters,
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
            rename_chat_session,
            export_chat_session,
            export_chat_session_json,
            get_ai_events,
            get_turn_ai_events,
            test_ai_gateway,
            ai_capabilities,
            cancel_ai_request,
            get_search_history,
            clear_search_history,
            export_search_results,
            refine_search,
            get_file_type_stats,
            get_browse_file_types,
            get_index_status,
            trigger_scan,
            rebuild_index,
            reindex_file,
            reindex_files,
            retry_failed_files,
            reextract_missing_content,
            verify_index_content,
            get_quality_summary,
            quality_audit,
            re_extract_file,
            re_extract_low_quality,
            cancel_scan,
            get_index_errors,
            add_dir,
get_dir_children,
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
            export_file_text,
            list_dir_entries,
            preview_file_by_path,
            reveal_in_folder,
            trigger_backup,
            get_backup_status,
            list_backups,
            export_backup,
            restore_from_zip,
            restore_backup,
            plan_restore,
            apply_restore,
            preview_remap,
            verify_backup,
            delete_backup,
            get_dead_dirs,
            remap_dir,
            remove_dir_with_files,
            get_settings,
            update_settings,
            autostart_state,
            set_autostart,
            web_session_status,
            web_session_logout,
            check_tesseract,
            list_ocr_engines,
            test_ocr_engine,
            check_dependencies,
            get_file_type_support,
            get_unsupported_ext_stats,
            check_index_health,
            check_index_integrity,
            heal_index_integrity,
            restore_files,
            backfill_embeddings,
            backfill_chunk_embeddings,
            backfill_audio_timeline,
            check_embedding_consistency,
            rebuild_embeddings,
            backfill_quality,
            get_logs,
            list_session_logs,
            clear_logs,
            get_version,
            detect_hardware,
            auto_optimize,
            get_performance_profile,
            set_performance_tier,
            install_bge,
            check_bge_installed,
            check_rerank_installed,
            get_setup_status,
            install_dep,
            cancel_dep_install,
            dep_install_status,
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
            get_lark_config,
            set_lark_config,
            get_lark_status,
            lark_restart,
            lark_pairing_start,
            lark_pairing_cancel,
            get_lark_bindings,
            lark_self_check,
            lark_bind_start,
            lark_bind_status,
            lark_bind_cancel,
            lark_bind_dismiss,
        ])
        .setup(|app| {
            // Pick up tools installed (e.g. by winget) since this process
            // started: a running process keeps its launch-time PATH.
            crate::process::refresh_path();
            // A previous migration could not delete the old data dir because
            // its handles were still open; retry now that they are released.
            crate::config::run_pending_cleanup(&data_dir);
            log::info!("data directory: {:?}", data_dir);
            if let Err(e) = std::fs::create_dir_all(&data_dir) {
                eprintln!("[FATAL] failed to create data directory {:?}: {}", data_dir, e);
                return Err(Box::new(e));
            }

            // Point model-based extractors at the data dir (packaged builds).
            crate::extractor::paddleocr::init(&data_dir);

            // Initialize file logger (append across restarts, rotate at 100 MB)
            let log_path = data_dir.join("app.log");
            if let Ok(meta) = std::fs::metadata(&log_path)
                && meta.len() > 100 * 1024 * 1024 {
                    let rotated = data_dir.join("app.log.1");
                    let _ = std::fs::rename(&log_path, &rotated);
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
            .filter_level(log::LevelFilter::Info)
            .filter_module("tantivy", log::LevelFilter::Warn)
            .filter_module("tract_linalg", log::LevelFilter::Warn)
            .filter_module("tract_core", log::LevelFilter::Warn)
            .target(env_logger::Target::Pipe(log_file))
            .format(|buf, record| {
                use std::io::Write;
                let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                let module = record.module_path().unwrap_or("?");
                let short = module.rsplit("::").next().unwrap_or(module);
                writeln!(buf, "{} {:<5} [{}] {}", ts, record.level(), short, record.args())
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

            // Clean up leftovers from an interrupted or failed index rebuild:
            // `index.tmp-*` build dirs and the `index.old` pre-swap backup. No
            // rebuild can be running at startup, so these are always orphaned.
            if let Ok(entries) = std::fs::read_dir(&data_dir) {
                for e in entries.flatten() {
                    let name = e.file_name();
                    let name = name.to_string_lossy();
                    if (name == "index.old" || name.starts_with("index.tmp-"))
                        && e.path().is_dir()
                    {
                        let _ = std::fs::remove_dir_all(e.path());
                        log::info!("[STARTUP] 清理残留索引目录: {name}");
                    }
                }
            }

            // Shared core: DB pool + index manager + indexer + scanner
            // (also used by the CLI via `boot::bootstrap_core`).
            let boot::Bootstrap {
                pool: db_pool,
                index_manager,
                indexer,
                scanner,
                cancel_scan,
            } = boot::bootstrap_core(&data_dir)?;
            let index_dir = data_dir.join(config::INDEX_DIR_NAME);
            let db_path = data_dir.join("data.db");

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

            if crate::extractor::paddleocr::models_present() {
                match crate::extractor::paddleocr::health_check() {
                    Ok(()) => log::info!("OCR 引擎自检通过"),
                    Err(e) => log::error!("OCR 引擎自检失败: {e}"),
                }
            } else {
                log::info!("[STARTUP] PaddleOCR 模型未安装，跳过自检（可在依赖中心安装）");
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
            // Clone before AppState::new moves `indexer` in; used later by the
            // startup-scan thread for the post-scan integrity heal.
            let indexer_for_startup = indexer.clone();
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
                data_dir.clone(),
                index_dir,
                db_path.clone(),
                watcher_tx,
                Some(watcher),
            );

            app.manage(app_state);

            // Web API server — 默认开启（`web_api_enabled == "false"` 才关闭），
            // 启动即监听，远程机器可直接访问（token 见 config.json / 设置页）。
            {
                let app_handle_for_api = app.handle().clone();
                let app_state_ref = app_handle_for_api.state::<AppState>();
                let web_api_enabled = if let Ok(conn) = app_state_ref.db.get() {
                    conn.query_row::<String, _, _>(
                        "SELECT value FROM app_settings WHERE key = 'web_api_enabled'",
                        [],
                        |r| r.get(0),
                    ).unwrap_or_default() != "false"
                } else {
                    true
                };
                if web_api_enabled {
                    log::info!("[STARTUP] Web API: 自动开启（web_api_enabled != false）");
                    webapi::spawn_server(app.handle().clone());
                } else {
                    log::info!("[STARTUP] Web API: 已被设置关闭（web_api_enabled = false）");
                }
            }

            // 一次性把 DB 里遗留的飞书策略搬进 config.json（幂等；见 settings.rs）。
            // 必须在拉 bot **之前**跑：bot 读的是 config.json，晚一步就会用旧默认值启动。
            if let Ok(conn) = app.state::<AppState>().db.get() {
                crate::commands::settings::migrate_legacy_lark_settings(&conn);
            }

            // 飞书 bot sidecar（§5.4 的 C3）：webapi 之后再拉，因为它要经 webapi 读设置。
            // bot 侧对"webapi 还没监听上"有重试（见 bin/lark-bot.rs），故这里不必等。
            lark_bot::spawn_if_enabled();
            // 看门狗：机器人**意外退出**（消息长连接断、崩溃…）时自动拉起，避免
            // "离线到用户重启应用"（2026-10-18 通讯审计发现）。
            lark_bot::spawn_watchdog();

            // `config.json` 热重载（§5.6 B 档）：手改配置文件后自动生效。
            // 放在 bot 之后 —— 它可能在"飞书策略变了"时重启 bot。
            config_watch::spawn(app.handle().clone());

            // 后台维护：定时扫描 / 自动备份 / watcher 溢出补扫。设置页里这三个开关
            // 过去只是"写进 app_settings 的字符串"（`auto_backup` 甚至显示为关却存着
            // "1"），没有任何代码读 —— 用户以为有备份可恢复。见 `maintenance` 模块头。
            //
            // `startup_scan_done`：启动扫描没有抢 `is_scanning`（历史如此），若维护线程
            // 在它跑着时也去扫一遍，两个 walker 会同时走同一批目录。故维护线程等这个
            // 标志（启动扫描 + 启动期 VACUUM 结束后置位）再动手。
            let startup_scan_done = Arc::new(AtomicBool::new(false));
            maintenance::start(app.handle().clone(), startup_scan_done.clone());

            let app_handle = app.handle().clone();
            let scanner_ref = scanner.clone();
            let db_ref = db_pool.clone();
            let indexer_ref = indexer_for_startup;
            let watch_tx = watcher_tx_for_startup;
            // （`startup_scan_done` 在上面创建、已克隆给维护线程。）
            let startup_scan_done_for_thread = startup_scan_done.clone();

            // R3-11: StartWatch 必须先于扫描线程，否则扫描期间的变更会丢失
            let dirs = {
                let conn = match db_ref.get() {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!("[STARTUP] 无法获取数据库连接: {e}");
                        return Ok(());
                    }
                };
                let dirs = match crate::db::dir_config::list_enabled_dirs(&conn) {
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
                // Do NOT return early: the window icon, system tray and
                // close-to-tray handler further below must still be installed
                // on a fresh install with no monitored directories.
                log::info!("[STARTUP] 无已配置目录，跳过启动扫描（仍初始化托盘/窗口）");
            }

            // One-time migration: normalize legacy separator variants, then
            // convert absolute paths to relative (order matters — the relative
            // migration derives its strip prefix from dir_config roots).
            if let Ok(conn) = db_ref.get() {
                let _ = crate::db::tracker::migrate_dir_paths_to_forward_slash(&conn);
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
                use crate::extractor::pdf;

                let mut slog = crate::logs::session::SessionLogGuard::open(&logs_dir, "scan");

                log::info!("[STARTUP] 检查依赖...");

                let ocr_ok = crate::extractor::paddleocr::health_check().is_ok();

                let pdf_ok = pdf::is_pdftoppm_available();

                let asr_ok = crate::extractor::audio::sensevoice_model_ready(&data_dir);
                let ffmpeg_ok = crate::extractor::audio::ffmpeg_available();

                log::info!(
                    "[STARTUP] deps: OCR={} pdf={} ASR={} ffmpeg={}",
                    if ocr_ok { "ok" } else { "fail" },
                    if pdf_ok { "ok" } else { "n/a" },
                    if asr_ok { "ok" } else { "no" },
                    if ffmpeg_ok { "ok" } else { "no" },
                );

                    if !ffmpeg_ok {
                        log::info!("[STARTUP] ffmpeg not found");
                    }

                    if !asr_ok {
                        log::info!("[STARTUP] ASR model not downloaded (SenseVoice)");
                    }

                if !ocr_ok {
                    log::error!("[STARTUP] PaddleOCR 引擎不可用，图片 OCR 将无法工作");
                }

                // 自适应并发（内存压力优雅降级/升级）：独立线程采样本进程 WS 与系统
                // 已用内存，压力升高时收紧页窗口 / OCR 许可 / 在飞文档数，平静后逐级
                // 升回；到暂停档则停止派发新页（在飞页跑完），避免 Windows 侧
                // 分配失败直接 abort 或换页拖垮整机。
                crate::load::start_supervisor(db_pool.clone());

                log::info!("[STARTUP] 开始扫描 {} 个目录", dirs.len());
                slog.write_line(&format!("[STARTUP] 开始扫描 {} 个目录", dirs.len()));
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
                            slog.write_line(&line);
                        }
                        Err(e) => {
                            let line = format!("[STARTUP] {} 扫描失败: {e}", dir.path);
                            log::error!("{line}");
                            slog.write_line(&line);
                        }
                    }
                }

                // Post-scan maintenance
                if let Ok(conn) = db_ref.get() {
                    if let Err(e) = crate::db::cleanup_orphan_content(&conn) {
                        log::error!("[STARTUP] orphan cleanup failed: {e}");
                    }
                    if let Err(e) = crate::db::cleanup_orphan_chunk_embeddings(&conn) {
                        log::error!("[STARTUP] orphan chunk-embedding cleanup failed: {e}");
                    }
                    if let Err(e) = crate::db::cleanup_orphan_file_rows(&conn) {
                        log::error!("[STARTUP] orphan doc vector/summary cleanup failed: {e}");
                    }
                    drop(conn);
                }

                // VACUUM **只在确实有碎片**时做：以前只按文件大小（>100 MiB）判断，
                // 于是一个几 GB 的库每次启动都要整库重写一遍（分钟级 IO，还会和紧接着
                // 启动的向量回填抢写锁）。空闲页占比低于阈值时跳过，代价近乎为零。
                const VACUUM_MIN_BYTES: u64 = 100 * 1024 * 1024;
                const VACUUM_MIN_FREELIST_RATIO: f64 = 0.10;
                let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
                if db_size > VACUUM_MIN_BYTES {
                    if let Ok(conn) = db_ref.get() {
                        let ratio = crate::db::fragmentation_ratio(&conn);
                        if ratio.is_some_and(|r| r >= VACUUM_MIN_FREELIST_RATIO) {
                            if let Err(e) = crate::db::vacuum(&conn) {
                                log::error!("[STARTUP] VACUUM failed: {e}");
                            }
                        } else {
                            log::info!(
                                "[STARTUP] VACUUM skipped (fragmentation={ratio:?} < {VACUUM_MIN_FREELIST_RATIO}, db_size={db_size} B)"
                            );
                        }
                        drop(conn);
                    }
                } else {
                    log::info!("[STARTUP] VACUUM skipped (db_size={db_size} B, threshold={VACUUM_MIN_BYTES})");
                }

                // 启动扫描 + 启动期 VACUUM 都做完了 → 放行后台维护线程
                // （定时扫描 / 自动备份 / watcher 溢出补扫）。
                startup_scan_done_for_thread.store(true, std::sync::atomic::Ordering::Release);

                // Chunk building + chunk/doc embedding backfills run
                // sequentially on one background thread: they share the local
                // model pool, so stacking them only makes each other slower.
                // Order: build chunks → embed chunks → embed docs.
                {
                    let pool_bg = db_ref.clone();
                    std::thread::spawn(move || {
                        let _ = crate::db::chunks::run_backfill_chunks(&pool_bg);
                        let _ = crate::commands::index::run_backfill_chunk_embeddings_public(&pool_bg);
                        let _ = crate::commands::index::run_backfill_embeddings_public(&pool_bg);
                    });
                }

                log::info!("[STARTUP] scan: {} dirs done", dirs.len());
                drop(slog);

                app_handle.emit("scan-completed", serde_json::json!({}))
                    .unwrap_or_else(|e| log::error!("[STARTUP] failed to emit scan-completed: {e}"));
                log::info!("[STARTUP] 启动扫描完成");

                // 索引完整性自愈：schema 重建会清空 Tantivy 而不重灌，DB 的
                // indexed=1 标记与索引脱节后增量扫描永远不会补写缺失文件。
                // 扫描完成后比对 DB↔Tantivy，把缺失文件重灌（复用 md5 缓存）。
                {
                    let pool_h = db_ref.clone();
                    let indexer_h = indexer_ref.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = crate::commands::index::run_index_integrity_heal(&pool_h, &indexer_h) {
                            log::error!("[STARTUP] 索引完整性自愈失败: {e}");
                        }
                    });
                }
            });

            // Set window icon from embedded PNG bytes
            if let Some(window) = app.get_webview_window("main") {
                let icon_bytes = include_bytes!("../icons/32x32.png");
                let img = image::load_from_memory(icon_bytes).map(|i| i.into_rgba8());
                if let Ok(img) = img {
                    let (w, h) = img.dimensions();
                    let rgba = img.into_raw();
                    let _ = window.set_icon(tauri::image::Image::new(&rgba, w, h));
                }
            }

            // System tray: "显示/隐藏" + "退出"，左键点击切换窗口
            let show_hide = MenuItemBuilder::with_id("toggle", "显示/隐藏").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let menu = MenuBuilder::new(app)
                .item(&show_hide)
                .separator()
                .item(&quit)
                .build()?;

            let tray_icon = image::load_from_memory(include_bytes!("../icons/32x32.png"))
                .ok()
                .map(|i| {
                    let rgba = i.into_rgba8();
                    let (w, h) = rgba.dimensions();
                    tauri::image::Image::new_owned(rgba.into_raw(), w, h)
                });

            let mut tray_builder = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("Link-Searcher")
                .show_menu_on_left_click(false);
            if let Some(icon) = tray_icon {
                tray_builder = tray_builder.icon(icon);
            }

            let _tray = tray_builder
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "toggle" => {
                        if let Some(window) = app.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                                #[cfg(target_os = "macos")]
                                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                                log::info!("[TRAY] 隐藏窗口");
                            } else {
                                #[cfg(target_os = "macos")]
                                {
                                    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
                                    restore_dock_icon();
                                }
                                let _ = window.show();
                                let _ = window.set_focus();
                                log::info!("[TRAY] 显示窗口");
                            }
                        }
                    }
                    "quit" => {
                        log::info!("[TRAY] 用户退出");
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            if window.is_visible().unwrap_or(false) {
                                let _ = window.hide();
                                #[cfg(target_os = "macos")]
                                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                                log::info!("[TRAY] 左键隐藏窗口");
                            } else {
                                #[cfg(target_os = "macos")]
                                {
                                    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
                                    restore_dock_icon();
                                }
                                let _ = window.show();
                                let _ = window.set_focus();
                                log::info!("[TRAY] 左键显示窗口");
                            }
                        }
                    }
                })
                .build(app)?;

            // 窗口关闭 → 隐藏到托盘而非退出
            if let Some(window) = app.get_webview_window("main") {
                let window_clone = window.clone();
                #[cfg(target_os = "macos")]
                let app_handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_clone.hide();
                        #[cfg(target_os = "macos")]
                        let _ = app_handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
                        log::info!("[WINDOW] 关闭请求 → 隐藏到托盘");
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!());

    match result {
        Ok(app) => {
            app.run(|app_handle, event| {
                if let tauri::RunEvent::Exit = event {
                    // 先停看门狗（否则它可能在退出途中把 bot 又拉起来），再收 bot：
                    // bot 一停，其 lark-cli 子进程会因 stdin EOF 优雅退出
                    //（bus daemon 按决策**留着复用**，不在这里动它）。
                    lark_bot::stop_watchdog();
                    lark_bot::shutdown();
                    if let Some(token) = app_handle.try_state::<tokio_util::sync::CancellationToken>() {
                        token.cancel();
                        log::info!("[SHUTDOWN] Web API server shutdown signal sent");
                    }
                }
            });
        }
        Err(e) => {
            log::error!("Tauri runtime error: {e}");
            std::process::exit(1);
        }
    }
}
